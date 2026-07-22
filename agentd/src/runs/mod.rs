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

/// One attention line in a morning brief (ux.11c). Named by `run_id` (= segment_id)
/// and `agent_id` so the operator can act on it today and ux.13 can attach verbs later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefItem {
    /// The run segment id (`"{agent_id}:{seq}"`).
    pub run_id:      String,
    pub agent_id:    String,
    /// "running" | "failed" | "interrupted" | "done" (attention items are the non-`done` ones).
    pub status:      String,
    #[serde(default)]
    pub spend:       Option<u64>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub last_error:  Option<String>,
}

/// A persisted morning brief (ux.11c) — the durable, pull-readable delivery record.
/// agentd authors the factual spine deterministically from `runs.redb` (CEO F2); the
/// model contributes only `narrative`. The "N need approval" headline is NOT stored
/// here — it is live scheduler state, overlaid at `GET /api/v1/brief` time (Eng G1).
/// All fields added after v1 must be `#[serde(default)]` (shares runs.redb's E5 rule).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BriefRecord {
    /// `"brief:{seq}"`.
    pub brief_id:       String,
    pub created_at:     u64,
    /// Window covered: `[window_from, window_to)`. First-ever brief floors at now−24h.
    pub window_from:    u64,
    pub window_to:      u64,
    /// Total runs in the window (over the FULL window, not the clamped `items`).
    pub run_count:      u64,
    pub failed_count:   u64,
    /// Sum of per-run `spend` (tokens) across the window; universal-tier (None spend) omitted.
    pub spend_total:    u64,
    /// Attention items (non-`done`), newest-first, capped at `MAX_BRIEF_ITEMS`.
    pub items:          Vec<BriefItem>,
    /// Count of genuinely-ok (`done`) runs not itemized — rendered as "✓ N ok".
    /// (Renamed meaning post-review: it must NOT include truncated failures.)
    pub overflow_count: u64,
    /// Attention items beyond `MAX_BRIEF_ITEMS` that could not be shown (failures/blocked/
    /// running). Rendered separately as "⚠ N more need attention" so truncated failures are
    /// never mislabeled as "ok". Full detail remains in `runs_query`.
    #[serde(default)]
    pub attention_overflow: u64,
    /// Optional model-authored narrative color (facts above cannot be faked by it).
    #[serde(default)]
    pub narrative:      Option<String>,
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

/// Messages on the single writer channel. Lifecycle events AND brief composition go
/// through the ONE channel so FIFO ordering holds: any `Close` enqueued before a
/// `PublishBrief` is applied to `runs.redb` first, so the brief can never miss a run
/// whose close was still in flight (review: Codex critical — the own-txn design raced the
/// channel drain and could drop a just-completed failure from every future window).
pub enum WriterMsg {
    Event(RunEvent),
    /// Compose + persist a brief on the writer thread; reply with the persisted record.
    PublishBrief {
        narrative: Option<String>,
        now:       u64,
        reply:     tokio::sync::oneshot::Sender<anyhow::Result<BriefRecord>>,
    },
}

/// Cheap, clonable handle the scheduler holds. Best-effort: a full/closed channel
/// drops the event with a warn — recording a run must never stall or crash an agent.
#[derive(Clone)]
pub struct RunTracker {
    tx: Option<tokio::sync::mpsc::UnboundedSender<WriterMsg>>,
}

impl RunTracker {
    /// A live tracker feeding `run_writer`.
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<WriterMsg>) -> Self {
        Self { tx: Some(tx) }
    }

    /// A no-op tracker (no run store configured, or tests).
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    /// A [`BriefPublisher`] on the SAME channel (ux.11c) — brief writes share the writer
    /// lane with lifecycle events, guaranteeing ordering. Disabled when the tracker is.
    pub fn brief_publisher(&self) -> BriefPublisher {
        BriefPublisher { tx: self.tx.clone() }
    }

    fn send(&self, ev: RunEvent) {
        if let Some(tx) = &self.tx {
            if tx.send(WriterMsg::Event(ev)).is_err() {
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

/// Handle the `publish_brief` tool holds (ux.11c). Sends a `PublishBrief` command down
/// the same channel the scheduler's lifecycle events use, so composition runs on the
/// single writer AFTER every previously-enqueued event is applied, then awaits the reply.
#[derive(Clone)]
pub struct BriefPublisher {
    tx: Option<tokio::sync::mpsc::UnboundedSender<WriterMsg>>,
}

impl BriefPublisher {
    /// A no-op publisher (no run store configured) — `publish` returns an error.
    pub fn disabled() -> Self {
        Self { tx: None }
    }

    /// Compose + persist a brief on the writer thread and return the persisted record.
    /// Errors (no store, channel closed, writer dropped, or a persist failure) propagate
    /// so the tool returns `Err` and NO `BriefWritten` event fires (advance-on-success).
    pub async fn publish(&self, narrative: Option<String>, now: u64) -> anyhow::Result<BriefRecord> {
        let tx = self
            .tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("run history not configured; cannot publish brief"))?;
        let (reply, reply_rx) = tokio::sync::oneshot::channel();
        tx.send(WriterMsg::PublishBrief { narrative, now, reply })
            .map_err(|_| anyhow::anyhow!("run-history writer channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("run-history writer dropped the brief reply"))?
    }
}

/// Dedicated writer task: drains `rx` and applies each event to the single
/// `RunsStore` writer. Runs off the scheduler loop (G4). A write failure is
/// logged, never propagated — run history is best-effort like flight logging.
pub async fn run_writer(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<WriterMsg>,
    store: std::sync::Arc<RunsStore>,
) {
    while let Some(msg) = rx.recv().await {
        match msg {
            WriterMsg::Event(ev) => {
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
            WriterMsg::PublishBrief { narrative, now, reply } => {
                // Composition runs HERE, after all earlier events applied (FIFO). The full
                // RUNS scan happening on this off-loop writer is fine — later events just
                // queue behind it, which never stalls the scheduler (best-effort logging).
                let store = std::sync::Arc::clone(&store);
                let res = tokio::task::spawn_blocking(move || store.publish_brief(narrative, now)).await;
                let flattened = match res {
                    Ok(inner) => inner,
                    Err(e)    => Err(anyhow::anyhow!("brief writer join failed: {e}")),
                };
                // Receiver may have gone away (tool cancelled) — dropping the reply is fine.
                let _ = reply.send(flattened);
            }
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

    #[tokio::test]
    async fn disabled_brief_publisher_errors() {
        let bp = BriefPublisher::disabled();
        assert!(bp.publish(None, 100_000).await.is_err(), "no store → error, not a false brief");
    }

    #[tokio::test]
    async fn brief_sees_close_enqueued_before_it_fifo() {
        // Review C1 (Codex critical): a Close enqueued BEFORE PublishBrief must be applied
        // first (single writer, FIFO), so the brief never drops a just-completed failure.
        let dir = tempfile::tempdir().unwrap();
        let (store, _q) = RunsStore::open(&dir.path().join("runs.redb")).unwrap();
        let store = Arc::new(store);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let writer = tokio::spawn(run_writer(rx, Arc::clone(&store)));

        let tracker = RunTracker::new(tx);
        let publisher = tracker.brief_publisher();
        // Enqueue open + a FAILED close, then immediately request a brief — all on one lane.
        tracker.open("scout", Some("cos".into()), "child_spawn", Some(0), "native");
        tracker.close("scout", "failed", Some("err".into()), Some("boom".into()), Some(9));
        // `tracker.close` stamps end_ts with real wall-clock time, so the brief window
        // must be anchored to real time too (a few seconds ahead covers the close).
        let brief = publisher.publish(None, unix_now_secs() + 5).await.unwrap();
        assert_eq!(brief.run_count, 1, "the close ahead in the FIFO queue was applied first");
        assert_eq!(brief.failed_count, 1);
        assert_eq!(brief.items[0].run_id, "scout:0");
        assert_eq!(brief.items[0].last_error.as_deref(), Some("boom"));

        drop(tracker);
        drop(publisher);
        writer.await.unwrap();
    }
}
