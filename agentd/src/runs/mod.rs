//! ux.11b-substrate — durable run history.
//!
//! A **run segment** is one authoritative slice of an agent's life: opened when
//! the agent enters the scheduler (config seed, operator/child/universal spawn,
//! or restore) and closed on terminal or park. Records are written from the
//! scheduler's in-process state-machine transitions (CEO C1) — NOT derived from
//! the best-effort flight log — so a dropped flight event never drops a run.
//!
//! Writes go through a [`RunTracker`] (an mpsc sender) so the scheduler loop
//! never touches redb inline (G4): the handlers `send()` a [`RunEvent`]
//! (non-blocking, best-effort — like the flight recorder, it must never stall
//! or crash an agent), and a dedicated [`run_writer`] task owns the single
//! [`store::RunsStore`] writer.

pub mod store;

pub use store::{RunFilter, RunsStore};

use serde::{Deserialize, Serialize};

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A durable per-segment run record. All fields added after v1 must be
/// `#[serde(default)]` so an old `runs.redb` still deserializes (E5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunRecord {
    /// `"{agent_id}:{segment_seq}"`.
    pub segment_id:   String,
    pub agent_id:     String,
    pub segment_seq:  u64,
    #[serde(default)]
    pub parent_id:    Option<String>,
    /// "config_seed" | "operator_spawn" | "child_spawn" | "universal_spawn" | "restore" | "resume".
    pub start_reason: String,
    pub start_ts:     u64,
    #[serde(default)]
    pub end_ts:       Option<u64>,
    /// "running" | "done" | "failed" | "interrupted" (clean-shutdown close of a still-open
    /// segment). A `parked` status is reserved for a future increment that closes segments on
    /// mid-run park/resume (ux.11b-ar-01) — v1 does not emit it.
    pub status:       String,
    #[serde(default)]
    pub stop_reason:  Option<String>,
    #[serde(default)]
    pub last_error:   Option<String>,
    #[serde(default)]
    pub approvals_count: u64,
    /// Lifetime `context_tokens()` at open. `None` for universal-tier (proxy-metered).
    #[serde(default)]
    pub start_context_tokens: Option<u64>,
    /// Δ(lifetime spend) across the segment. `None` for universal-tier or an unclosed segment.
    #[serde(default)]
    pub spend:        Option<u64>,
    /// "native" | "universal".
    #[serde(default)]
    pub tier:         String,
}

/// A lifecycle transition sent from the scheduler to the writer task.
#[derive(Debug, Clone)]
pub enum RunEvent {
    Open {
        agent_id:     String,
        parent_id:    Option<String>,
        start_reason: String,
        start_context_tokens: Option<u64>,
        tier:         String,
        ts:           u64,
    },
    /// Close the agent's open segment (terminal, or an "interrupted" clean-shutdown close).
    Close {
        agent_id:     String,
        status:       String,
        stop_reason:  Option<String>,
        last_error:   Option<String>,
        end_context_tokens: Option<u64>,
        ts:           u64,
    },
    /// Increment the open segment's approval counter (G6).
    IncrApproval { agent_id: String },
}

/// Cheap, clonable handle the scheduler holds. Best-effort: a full/closed channel
/// drops the event with a warn — recording a run must never stall or crash an agent.
#[derive(Clone)]
pub struct RunTracker {
    tx: Option<tokio::sync::mpsc::UnboundedSender<RunEvent>>,
}

impl RunTracker {
    /// A live tracker feeding `run_writer`.
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<RunEvent>) -> Self {
        Self { tx: Some(tx) }
    }

    /// A no-op tracker (no run store configured, or tests).
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    fn send(&self, ev: RunEvent) {
        if let Some(tx) = &self.tx {
            if tx.send(ev).is_err() {
                tracing::warn!("run-history writer channel closed; dropping run event (best-effort)");
            }
        }
    }

    /// Open a segment. Idempotent in the writer: a second open for an agent that
    /// already has an open segment is a no-op (restart continues the open segment, G3).
    pub fn open(
        &self,
        agent_id: &str,
        parent_id: Option<String>,
        start_reason: &str,
        start_context_tokens: Option<u64>,
        tier: &str,
    ) {
        self.send(RunEvent::Open {
            agent_id: agent_id.to_string(),
            parent_id,
            start_reason: start_reason.to_string(),
            start_context_tokens,
            tier: tier.to_string(),
            ts: unix_now_secs(),
        });
    }

    /// Close the agent's open segment on a terminal outcome.
    pub fn close(
        &self,
        agent_id: &str,
        status: &str,
        stop_reason: Option<String>,
        last_error: Option<String>,
        end_context_tokens: Option<u64>,
    ) {
        self.send(RunEvent::Close {
            agent_id: agent_id.to_string(),
            status: status.to_string(),
            stop_reason,
            last_error,
            end_context_tokens,
            ts: unix_now_secs(),
        });
    }

    /// Increment the open segment's approval counter (G6).
    pub fn incr_approval(&self, agent_id: &str) {
        self.send(RunEvent::IncrApproval { agent_id: agent_id.to_string() });
    }
}

/// Dedicated writer task: drains `rx` and applies each event to the single
/// `RunsStore` writer. Runs off the scheduler loop (G4). A write failure is
/// logged, never propagated — run history is best-effort like flight logging.
pub async fn run_writer(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<RunEvent>,
    store: std::sync::Arc<RunsStore>,
) {
    while let Some(ev) = rx.recv().await {
        let store = std::sync::Arc::clone(&store);
        // redb writes are synchronous; do them on a blocking thread so this task's
        // async worker isn't held on the commit fsync.
        let res = tokio::task::spawn_blocking(move || store.apply(ev)).await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "run-history write failed (best-effort)"),
            Err(e)     => tracing::warn!(error = %e, "run-history writer join failed"),
        }
    }
}

#[cfg(test)]
mod tracker_tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn tracker_to_writer_to_store_round_trip() {
        // Proves the off-loop path (G4): scheduler → RunTracker.send → run_writer → RunsStore.
        let dir = tempfile::tempdir().unwrap();
        let (store, _q) = RunsStore::open(&dir.path().join("runs.redb")).unwrap();
        let store = Arc::new(store);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let writer = tokio::spawn(run_writer(rx, Arc::clone(&store)));

        let tracker = RunTracker::new(tx);
        tracker.open("inbox", Some("cos".into()), "child_spawn", Some(100), "native");
        tracker.incr_approval("inbox");
        tracker.close("inbox", "done", Some("completed".into()), None, Some(140));
        drop(tracker); // close the channel so the writer task ends

        writer.await.unwrap();
        let rec = store.get("inbox:0").unwrap().unwrap();
        assert_eq!(rec.status, "done");
        assert_eq!(rec.parent_id.as_deref(), Some("cos"));
        assert_eq!(rec.spend, Some(40), "140 - 100");
        assert_eq!(rec.approvals_count, 1);
    }

    #[tokio::test]
    async fn disabled_tracker_is_a_noop() {
        // A no-store deployment must not panic when the scheduler calls the tracker.
        let t = RunTracker::disabled();
        t.open("a", None, "config_seed", Some(0), "native");
        t.incr_approval("a");
        t.close("a", "done", None, None, Some(1)); // no channel → silently dropped
    }
}
