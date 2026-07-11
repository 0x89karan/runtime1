//! ux.0 — background event producers + the pushed-event type.
//!
//! Option B (not `tokio::select!`): agentctl stays 100% synchronous. Two detached
//! std::threads push `AppEvent`s into a **bounded** `sync_channel`; the render loop
//! (`mod.rs`) drains them with `try_recv` and never blocks on I/O.
//!
//! ```text
//!   snapshot thread ──┐  sync_channel(CAP)   ┌── render loop: poll(30ms) keys
//!   (every interval)  ├──► try_send ─────────┤     + drain try_recv → step()
//!   SSE thread (HTTP) ─┘  (drop-on-full)      └── coalesced redraw on dirty
//!                     ◄── wake_rx (reconcile) ─┘  Invalidated → poll snapshot NOW
//! ```
//!
//! Producers are **detached daemons** (never `join()`ed — a thread blocked in
//! `BufReader::read_line` cannot be cancelled; F7). Dropping the `Receiver` makes
//! the next `try_send` return `Disconnected`, which unwinds the thread; a thread
//! parked in a blocking read exits on its next read/timeout. Process exit reaps any
//! stragglers. Each producer body is wrapped in `catch_unwind` so a panic surfaces
//! an `AppEvent::ProducerDied` sentinel instead of silently killing the feed.

use std::io::BufRead as _;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::watch::reader::{PendingAction, Snapshot};
use crate::watch::source::DataSource;

/// Bounded channel depth. Small: producers `try_send` and drop on full (the next
/// snapshot is fresh anyway; SSE is lossy and reconciled by the snapshot poll).
pub const CHANNEL_CAP: usize = 256;

/// SSE liveness: reqwest::blocking exposes only a whole-request timeout (no idle
/// timeout), so this doubles as bounded half-open detection AND a periodic
/// reconnect. 90s ≈ 3 missed 30s server pings (F1/C2). A healthy stream is closed
/// by this total timeout and reconnects promptly (backoff resets after a healthy
/// connection — see `next_backoff`); the gap is reconciled by the snapshot poll.
const SSE_TOTAL_TIMEOUT: Duration = Duration::from_secs(90);
const SSE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SSE_BACKOFF_START: Duration = Duration::from_millis(500);
const SSE_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// An event pushed from a producer thread (or a key from the render loop) into the
/// single channel. `step()` in `mod.rs` folds each into `App`.
pub enum AppEvent {
    /// A key press. Constructed by the render loop (never sent by a producer thread).
    Key(crossterm::event::KeyEvent),
    /// Fresh scheduler snapshot (authoritative view state). Boxed — it's the largest
    /// variant by far, so boxing keeps `AppEvent` small in the channel.
    Snapshot(Box<Snapshot>),
    /// Fresh approval queue.
    Approvals(Vec<PendingAction>),
    /// A parsed SSE flight event (fills the bounded ring; ux.2 renders it).
    Flight(serde_json::Value),
    /// SSE reconnect / parse-fail / broadcast `{"lagged":n}` — the stream is lossy,
    /// so the snapshot poll is authoritative. Marks a ring gap AND asks the render
    /// loop to force an immediate snapshot reconcile (F3/C4).
    Invalidated,
    /// Channel overflow: `n` flight events were dropped since the last signal (C3).
    EventsDropped(usize),
    /// A producer thread panicked and exited. Surfaces the dead feed as a gap instead
    /// of a silent freeze (the last snapshot would otherwise render forever).
    ProducerDied(&'static str),
}

/// Handle owning the producer thread JoinHandles. Intentionally NEVER joined (F7):
/// dropping it drops nothing blocking; the threads are unwound by `Receiver` drop
/// (Disconnected on next send) or process exit.
pub struct Producers {
    _snapshot: JoinHandle<()>,
    _sse: Option<JoinHandle<()>>,
}

/// Spawn the snapshot producer (always) and the SSE producer (HTTP sources only —
/// `event_stream_url()` is `None` for FUSE, which stays snapshot-poll-only; F4).
/// Returns the event receiver, a `wake` sender (send `()` to force an immediate
/// snapshot poll — used for `Invalidated` reconcile), and the producer handles.
pub fn spawn_producers(
    source: Arc<dyn DataSource>,
    interval: Duration,
) -> (Receiver<AppEvent>, SyncSender<()>, Producers) {
    let (tx, rx) = sync_channel::<AppEvent>(CHANNEL_CAP);
    // Small wake channel: render loop → snapshot producer ("poll now"). Depth > 1 so a
    // burst of Invalidated events never blocks the render loop on `try_send`.
    let (wake_tx, wake_rx) = sync_channel::<()>(4);

    let snap_tx = tx.clone();
    let snap_src = Arc::clone(&source);
    let snapshot = thread::spawn(move || {
        let sentinel_tx = snap_tx.clone();
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
            snapshot_loop(snap_src, snap_tx, wake_rx, interval)
        }));
        if r.is_err() {
            let _ = sentinel_tx.try_send(AppEvent::ProducerDied("snapshot"));
        }
    });

    let sse = source.event_stream_url().map(|url| {
        let sse_tx = tx; // last clone; move into the SSE thread
        thread::spawn(move || {
            let sentinel_tx = sse_tx.clone();
            let r = std::panic::catch_unwind(AssertUnwindSafe(|| sse_loop(url, sse_tx)));
            if r.is_err() {
                let _ = sentinel_tx.try_send(AppEvent::ProducerDied("sse"));
            }
        })
    });

    (rx, wake_tx, Producers { _snapshot: snapshot, _sse: sse })
}

/// Poll `load_snapshot` + `load_approvals` every `interval` (F6: honors `--interval`,
/// not a hardcoded cadence — same refresh rate as the pre-refactor loop, but off the
/// render thread so a slow server never freezes the UI; F5). A `wake` on `wake_rx`
/// (sent by the render loop on `Invalidated`) forces an immediate re-poll so state
/// reconciles without waiting for the next interval tick (F3/C4).
fn snapshot_loop(
    source: Arc<dyn DataSource>,
    tx: SyncSender<AppEvent>,
    wake_rx: Receiver<()>,
    interval: Duration,
) {
    loop {
        if send_or_stop(&tx, AppEvent::Snapshot(Box::new(source.load_snapshot()))) {
            return;
        }
        if send_or_stop(&tx, AppEvent::Approvals(source.load_approvals())) {
            return;
        }
        match wake_rx.recv_timeout(interval) {
            // Woken for an immediate reconcile — coalesce any burst of wakes.
            Ok(()) => while wake_rx.try_recv().is_ok() {},
            Err(RecvTimeoutError::Timeout) => {}
            // wake_tx dropped (render loop exiting) → the next send_or_stop returns.
            Err(RecvTimeoutError::Disconnected) => {}
        }
    }
}

/// Reconnecting SSE reader. Reuses the blocking `BufReader` line loop from
/// `orchestrate.rs`; parse errors count as a dropped event (never panic). On any end
/// (EOF / timeout / connect fail) it emits `Invalidated` and reconnects — with backoff
/// that RESETS after a healthy connection so a healthy stream's 90s total-timeout close
/// reconnects promptly, and only GROWS on connect/handshake failure (fix: was monotonic).
fn sse_loop(events_url: String, tx: SyncSender<AppEvent>) {
    let mut backoff = SSE_BACKOFF_START;
    let mut dropped: usize = 0;
    loop {
        match stream_once(&events_url, &tx, &mut dropped) {
            StreamEnd::ReceiverGone => return,
            StreamEnd::Reconnect { connected } => {
                // The stream is lossy across a gap; force snapshot reconciliation.
                if send_or_stop(&tx, AppEvent::Invalidated) {
                    return;
                }
                // A healthy close reconnects at START; a connect failure sleeps the
                // current (growing) backoff. Grow only on connect failure.
                let sleep = if connected { SSE_BACKOFF_START } else { backoff };
                thread::sleep(sleep.saturating_add(jitter(sleep)));
                backoff = next_backoff(backoff, connected);
            }
        }
    }
}

/// Backoff transition: a healthy (connected) close resets to START; a connect/handshake
/// failure doubles up to MAX. Pure — unit-tested.
fn next_backoff(current: Duration, connected: bool) -> Duration {
    if connected {
        SSE_BACKOFF_START
    } else {
        (current * 2).min(SSE_BACKOFF_MAX)
    }
}

enum StreamEnd {
    /// Render loop dropped the Receiver — stop the producer.
    ReceiverGone,
    /// Stream ended/failed — reconnect. `connected` = the HTTP stream was successfully
    /// established (a healthy close, e.g. the 90s total timeout) vs. a connect failure.
    Reconnect { connected: bool },
}

fn stream_once(events_url: &str, tx: &SyncSender<AppEvent>, dropped: &mut usize) -> StreamEnd {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Some(SSE_TOTAL_TIMEOUT))
        .connect_timeout(SSE_CONNECT_TIMEOUT)
        .tcp_keepalive(Some(Duration::from_secs(30)))
        .build()
    {
        Ok(c) => c,
        Err(_) => return StreamEnd::Reconnect { connected: false },
    };
    let resp = match client.get(events_url).header("accept", "text/event-stream").send() {
        Ok(r) if r.status().is_success() => r,
        _ => return StreamEnd::Reconnect { connected: false },
    };
    // Past this point the stream was established: any end is a "healthy" close (EOF /
    // total-timeout), so the caller resets backoff and reconnects promptly.
    let mut reader = std::io::BufReader::new(resp);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return StreamEnd::Reconnect { connected: true }, // EOF
            Ok(_) => {}
            // ux.0-followup: a read error (incl. invalid UTF-8) tears down the whole
            // connection rather than skipping the one bad line. agentd emits UTF-8 JSON
            // so this is theoretical; ux.2's real SSE parser should skip-line instead.
            Err(_) => return StreamEnd::Reconnect { connected: true }, // read timeout / io error
        }
        // SSE: only `data:` lines carry payload; `: ping`, `event:`, blanks ignored.
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let Some(data) = trimmed.strip_prefix("data:") else { continue };
        let data = data.strip_prefix(' ').unwrap_or(data);
        if data.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
            // Parse failure = a lost event; count it (surfaced via EventsDropped) rather
            // than skipping silently, so the gap/drop counters stay honest (C4).
            *dropped += 1;
            continue;
        };
        // Broadcast lag marker (management.rs) → treat as a gap (C4).
        if value.get("lagged").is_some() {
            if send_or_stop(tx, AppEvent::Invalidated) {
                return StreamEnd::ReceiverGone;
            }
            continue;
        }
        // Flush any accumulated drop count before the next real event (C3).
        if *dropped > 0 {
            match tx.try_send(AppEvent::EventsDropped(*dropped)) {
                Ok(()) => *dropped = 0,
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return StreamEnd::ReceiverGone,
            }
        }
        match tx.try_send(AppEvent::Flight(value)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => *dropped += 1, // never block the producer
            Err(TrySendError::Disconnected(_)) => return StreamEnd::ReceiverGone,
        }
    }
}

/// `try_send` an event; returns `true` if the Receiver is gone (stop the thread).
/// Full → drop (a fresh snapshot follows; SSE is lossy by contract). NOTE: snapshots
/// and approvals also flow through here, but the render loop drains at ≥33 Hz and a
/// full-then-dropped snapshot is immediately followed by another at the next tick, so
/// authoritative state is never lost for more than one interval.
fn send_or_stop(tx: &SyncSender<AppEvent>, ev: AppEvent) -> bool {
    matches!(tx.try_send(ev), Err(TrySendError::Disconnected(_)))
}

/// Cheap deterministic-enough jitter (0..backoff/2) without pulling `rand`.
/// Uses the current nanos; collisions across producers are harmless.
fn jitter(backoff: Duration) -> Duration {
    let half = backoff.as_millis().max(1) as u64 / 2;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(nanos % half.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_resets_after_healthy_connection() {
        // A healthy close (connected=true) resets to START, even from the cap.
        assert_eq!(next_backoff(SSE_BACKOFF_MAX, true), SSE_BACKOFF_START);
        assert_eq!(next_backoff(SSE_BACKOFF_START, true), SSE_BACKOFF_START);
    }

    #[test]
    fn backoff_grows_only_on_connect_failure() {
        // Connect failure (connected=false) doubles, capped at MAX.
        assert_eq!(next_backoff(SSE_BACKOFF_START, false), SSE_BACKOFF_START * 2);
        assert_eq!(next_backoff(SSE_BACKOFF_MAX, false), SSE_BACKOFF_MAX);
    }
}
