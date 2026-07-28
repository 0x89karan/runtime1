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
//! a blocking read cannot be cancelled; F7). Dropping the `Receiver` makes
//! the next `try_send` return `Disconnected`, which unwinds the thread; a thread
//! parked in a blocking read exits on its next read/timeout. Process exit reaps any
//! stragglers. Each producer body is wrapped in `catch_unwind` so a panic surfaces
//! an `AppEvent::ProducerDied` sentinel instead of silently killing the feed.

use std::io::BufRead;
use std::panic::AssertUnwindSafe;
use std::process::{Child, ChildStdout};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::watch::logs::{parse_compose_line, LogLine};
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

/// ux.10-A: max log lines per `AppEvent::LogLines` batch. Compose replays its backfill in
/// one burst and a chatty project can outpace the 33 Hz render tick, so lines travel in
/// batches: `CHANNEL_CAP` slots × this batch size is the real buffer, instead of one
/// channel slot per line (which would drop ~everything during the backfill).
const LOG_BATCH_MAX: usize = 64;

/// Identifies the log reader in `AppEvent::ProducerDied`. A shared constant, not a bare
/// literal at each end: `step()` in mod.rs BRANCHES on this value (a dead log reader is
/// reported in the Logs view, not as an event-feed gap), so a typo on either side would
/// compile fine and silently take the wrong branch (/review's maintainability pass).
pub const LOGS_PRODUCER: &str = "logs";

/// Hard cap on one log record's bytes as it is READ (see `read_bounded_line`). Comfortably
/// above the render clip (`logs::MAX_LOG_LINE_CHARS`) so nothing visible is lost, and far
/// below anything that could pressure memory. Two buffers hold log bytes, and the channel —
/// not the ring — dominates: the ring is `LOG_RING_CAP × MAX_LOG_LINE_BYTES` ≈ 8 MB, while
/// in-flight events are `CHANNEL_CAP × LOG_BATCH_MAX` lines ≈ 67 MB if the render loop stalls
/// with the channel full. Lossy UTF-8 decoding can expand a record up to 3x (each invalid byte
/// becomes U+FFFD), so `pump_lines` truncates the decoded text to `logs::MAX_LOG_LINE_CHARS` —
/// everything past that is clipped at render anyway (/review's security + maintainability
/// passes both flagged the stated bound as understated).
const MAX_LOG_LINE_BYTES: usize = 4096;

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
    /// ux.10-A: a batch of tailed `docker compose logs` lines. Batched (not one event per
    /// line) so the compose backfill burst doesn't overflow the channel — see LOG_BATCH_MAX.
    LogLines(Vec<LogLine>),
    /// ux.10-A: `n` log lines dropped to channel backpressure since the last signal.
    /// Rendered in the Logs header — a silently truncated tail reads as "nothing happened".
    LogLinesDropped(usize),
}

/// Handle owning the producer thread JoinHandles. The threads are intentionally NEVER
/// joined (F7): dropping this drops nothing blocking; they are unwound by `Receiver` drop
/// (Disconnected on next send) or process exit.
///
/// The log tail is the exception that forced a `Drop` impl — see the field comment.
pub struct Producers {
    _snapshot: JoinHandle<()>,
    _sse: Option<JoinHandle<()>>,
    /// ux.10-A: the `docker compose logs --follow` child, owned HERE so `Drop` can kill it.
    /// This is load-bearing, not hygiene: `--follow` never EOFs, and the reader thread is
    /// never joined, so without an explicit kill the child would be ORPHANED at quit — a
    /// stray `docker compose logs` process streaming into a dead pipe for the rest of the
    /// terminal session, once per `agentctl watch` (A2).
    logs_child: Option<Child>,
    _logs: Option<JoinHandle<()>>,
}

impl Drop for Producers {
    fn drop(&mut self) {
        if let Some(child) = self.logs_child.as_mut() {
            // Kills the whole process group (the `docker` CLI *and* the `docker-compose`
            // plugin it forked) and reaps it. That also closes the write end of the pipe,
            // which is what unblocks the reader thread parked in `fill_buf`.
            crate::docker::kill_tail(child);
        }
    }
}

/// Spawn the snapshot producer (always), the SSE producer (HTTP sources only —
/// `event_stream_url()` is `None` for FUSE, which stays snapshot-poll-only; F4), and — when
/// `log_services` is `Some` (a Compose project was detected at startup) — the compose log
/// tail. The tail is spawned EAGERLY rather than on first entry to the Logs view, because
/// `spawn_producers` consumes the sender internally and `Producers::drop` is what tears the
/// child down; entering the view then just reveals a ring that is already filling (A3).
///
/// Returns the event receiver, a `wake` sender (send `()` to force an immediate
/// snapshot poll — used for `Invalidated` reconcile), and the producer handles.
pub fn spawn_producers(
    source: Arc<dyn DataSource>,
    interval: Duration,
    log_services: Option<Vec<String>>,
) -> (Receiver<AppEvent>, SyncSender<()>, Producers) {
    let (tx, rx) = sync_channel::<AppEvent>(CHANNEL_CAP);
    // Small wake channel: render loop → snapshot producer ("poll now"). Depth > 1 so a
    // burst of Invalidated events never blocks the render loop on `try_send`.
    let (wake_tx, wake_rx) = sync_channel::<()>(4);

    // Start the log tail first so `tx` can still be cloned freely (the SSE arm below moves
    // the last clone into its thread).
    let (logs_child, logs_thread) = match log_services {
        Some(services) => spawn_log_tail(tx.clone(), services),
        None => (None, None),
    };

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

    (
        rx,
        wake_tx,
        Producers { _snapshot: snapshot, _sse: sse, logs_child, _logs: logs_thread },
    )
}

/// Start `docker compose logs --follow` and the thread that reads it. A failure to spawn
/// (no docker binary, exec error) is surfaced as a visible notice line rather than a silent
/// empty view — detection said a project existed, so a missing tail is real news.
fn spawn_log_tail(
    tx: SyncSender<AppEvent>,
    services: Vec<String>,
) -> (Option<Child>, Option<JoinHandle<()>>) {
    let mut child = match crate::docker::spawn_compose_logs(services.len()) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.try_send(AppEvent::LogLines(vec![LogLine::notice(format!(
                "— could not start `docker compose logs`: {e} —"
            ))]));
            return (None, None);
        }
    };
    // `stdout` is piped by spawn_compose_logs, so `take()` is Some; be defensive anyway
    // rather than unwrapping in a TUI process.
    let Some(stdout) = child.stdout.take() else {
        let _ = tx.try_send(AppEvent::LogLines(vec![LogLine::notice(
            "— `docker compose logs` produced no stdout pipe —",
        )]));
        crate::docker::kill_tail(&mut child);
        return (None, None);
    };
    let handle = thread::spawn(move || {
        let sentinel_tx = tx.clone();
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| logs_loop(stdout, tx, services)));
        if r.is_err() {
            let _ = sentinel_tx.try_send(AppEvent::ProducerDied(LOGS_PRODUCER));
        }
    });
    (Some(child), Some(handle))
}

/// Read the compose tail line-by-line, batching into `AppEvent::LogLines`.
///
/// Batching without a timer or a second thread: after each read, `BufReader::buffer()`
/// reports what is ALREADY buffered without blocking. While that is non-empty we keep
/// accumulating; the moment it runs dry (the next read would block) we flush. So a burst
/// travels as full batches and a trickle travels as single-line batches — no latency added
/// to the quiet case.
fn logs_loop(stdout: ChildStdout, tx: SyncSender<AppEvent>, services: Vec<String>) {
    pump_lines(&mut std::io::BufReader::new(stdout), &tx, &services);
}

/// The reader loop itself, generic over the input so it can be driven by an in-memory reader
/// in tests (including one that stalls mid-record — the liveness case a real pipe can't easily
/// reproduce in a unit test).
fn pump_lines<R: std::io::Read>(
    reader: &mut std::io::BufReader<R>,
    tx: &SyncSender<AppEvent>,
    services: &[String],
) {
    let mut batch: Vec<LogLine> = Vec::with_capacity(LOG_BATCH_MAX);
    let mut dropped = 0usize;
    let mut raw     = Vec::new();
    // Persists across reads: set when a record was truncated at the byte cap, cleared when the
    // reader resynchronizes at that record's newline.
    let mut discarding = false;
    loop {
        let eof = match read_bounded_line(reader, &mut raw, &mut discarding) {
            Ok(0) => true,
            Ok(_) => false,
            // An I/O error on the pipe ends the tail. The child is killed by
            // `Producers::drop` either way, so there is nothing to clean up here beyond
            // reporting the end.
            Err(_) => true,
        };
        if !eof {
            // Lossy, not strict: container stdout is untrusted bytes, and a single invalid
            // UTF-8 byte must not be able to kill the whole tail for the session. Clamped to
            // the rendered width right here, because lossy decoding can expand a 4 KB record
            // 3x (every invalid byte becomes U+FFFD) and everything past the clip is thrown
            // away at render anyway — so the ring and the channel both stay honest to their
            // documented bounds.
            let decoded = String::from_utf8_lossy(&raw);
            let text    = decoded.trim_end_matches(['\r', '\n']);
            let clamped = crate::watch::logs::clip_payload(text);
            batch.push(parse_compose_line(&clamped, services));
        }
        // Flush when: EOF, the batch is full, nothing more is already buffered (the next read
        // would block), OR the record was just TRUNCATED. That last condition completes the
        // liveness fix (/review round 3): returning the truncated prefix early is pointless if
        // it then sits in `batch` because bytes of the discarded remainder happen to be
        // buffered — the next read can park for as long as the writer stays silent.
        if (eof || batch.len() >= LOG_BATCH_MAX || discarding || reader.buffer().is_empty())
            && !batch.is_empty()
            && flush_batch(tx, &mut batch, &mut dropped)
        {
            return; // receiver gone
        }
        if eof {
            // BLOCKING sends, unlike every other send on this thread. The two terminal
            // messages are the last thing this producer will ever say, so a `try_send` that
            // lands on a momentarily-full channel would lose them permanently (a truncated
            // tail would then read as complete). Blocking is safe here: the render loop drains
            // at ~33 Hz, and if it has exited the channel is disconnected and `send` returns
            // Err immediately rather than parking.
            if dropped > 0 {
                let _ = tx.send(AppEvent::LogLinesDropped(dropped));
            }
            // The child exited on its own (project stopped, `docker` died). Say so: an
            // unexplained frozen tail is indistinguishable from a quiet project.
            let _ = tx.send(AppEvent::LogLines(vec![LogLine::notice(
                "— docker compose logs stream ended —",
            )]));
            return;
        }
    }
}

/// Read one newline-terminated record into `buf`, capped at `MAX_LOG_LINE_BYTES`.
///
/// `BufRead::read_line`/`read_until` grow their buffer until the delimiter arrives, so a
/// container that writes a gigabyte with no `\n` (a serialized blob, a core dump on stdout)
/// would grow this allocation without bound and take the cockpit down with it.
///
/// Bounded in BOTH memory and time: on hitting the cap the truncated record is returned
/// IMMEDIATELY (with `discarding` set) rather than waiting for a newline that may never come.
/// A stalled 4 KB-and-counting record therefore still shows up in the view; the reader then
/// consumes and discards the rest of that record and resynchronizes at the next newline.
/// Returning only at the newline would bound memory but not liveness — a mid-record stall
/// would freeze the whole tail, including other services' lines (found by /review round 2).
///
/// `discarding` is caller-owned state so it survives across calls (that is what makes the
/// early return safe). Returns 0 ONLY at EOF; any non-zero value means a record was produced,
/// but is not a length — an empty log line reports 1 with an empty `buf`, so read the size from
/// `buf` itself. Bytes are raw: the caller decodes lossily, because container output is not
/// guaranteed UTF-8.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    discarding: &mut bool,
) -> std::io::Result<usize> {
    buf.clear();
    loop {
        let available = match reader.fill_buf() {
            Ok([]) => {
                // EOF: a final unterminated record still counts as a line.
                return Ok(buf.len());
            }
            Ok(b) => b,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let newline = available.iter().position(|&b| b == b'\n');
        // Tail of an over-long record: throw it away, resync at the next newline.
        if *discarding {
            match newline {
                Some(i) => {
                    reader.consume(i + 1);
                    *discarding = false;
                }
                None => {
                    let n = available.len();
                    reader.consume(n);
                }
            }
            continue;
        }
        let room = MAX_LOG_LINE_BYTES.saturating_sub(buf.len());
        match newline {
            Some(i) => {
                buf.extend_from_slice(&available[..i.min(room)]);
                reader.consume(i + 1);
                return Ok(buf.len().max(1)); // never report 0 (EOF) for an empty line
            }
            None => {
                let take = available.len().min(room);
                buf.extend_from_slice(&available[..take]);
                reader.consume(take);
                if buf.len() >= MAX_LOG_LINE_BYTES {
                    *discarding = true;
                    return Ok(buf.len());
                }
            }
        }
    }
}

/// How long a full channel is treated as BACKPRESSURE (wait) rather than loss (drop).
///
/// Sized against the consumer, not guessed: the render loop drains up to `CHANNEL_CAP` events
/// per ~30 ms tick, so a full channel clears within a tick or two. Waiting that long is
/// invisible to the operator; dropping instead of waiting is not.
///
/// Deliberately SHORT (≈2 ticks, not the 250 ms first tried): while this thread waits it keeps
/// taking freed slots, and every OTHER producer on this channel is drop-on-full — a snapshot
/// or approval list that loses its slot is simply gone, and a frozen agent table has no
/// counter to show it. A long window let a crash-looping container starve the authoritative
/// feed (/review's red-team pass called it a priority inversion). Two ticks is enough to
/// absorb a compose backfill burst — /qa's 5 000-line flood still lands zero drops — without
/// holding the channel against the panes that matter more.
const LOG_SEND_BACKPRESSURE: Duration = Duration::from_millis(60);
/// Poll interval while waiting out backpressure.
const LOG_SEND_RETRY: Duration = Duration::from_millis(2);

/// Send one batch, flushing any pending drop count first. Returns `true` if the Receiver is
/// gone (stop the thread).
///
/// A full channel is waited out (bounded by `LOG_SEND_BACKPRESSURE`), not dropped on sight.
/// Measured reason: `docker compose logs` writes line-by-line, so the pipe usually hands the
/// reader ONE line per read and the "flush when the buffer runs dry" heuristic degenerates to
/// one-line batches. /qa's 5 000-line burst then overflowed the 256-slot channel and lost
/// 4 479 lines (~90%) — honestly counted, but a log view that discards nine tenths of a burst
/// is not a log view. Waiting is also the correct backpressure: it propagates into the pipe,
/// which is what pipes are for. Only a wait that EXCEEDS the deadline counts as dropped, so
/// a genuinely wedged render loop still can't stall the reader indefinitely.
fn flush_batch(
    tx: &SyncSender<AppEvent>,
    batch: &mut Vec<LogLine>,
    dropped: &mut usize,
) -> bool {
    if *dropped > 0 {
        match tx.try_send(AppEvent::LogLinesDropped(*dropped)) {
            Ok(()) => *dropped = 0,
            Err(TrySendError::Full(_)) => {} // retry with the next batch
            Err(TrySendError::Disconnected(_)) => return true,
        }
    }
    let n = batch.len();
    // `mem::take` empties `batch` here; on Full the Vec comes back inside the error and is
    // handed to the next attempt, so nothing is duplicated or silently lost.
    let mut ev = AppEvent::LogLines(std::mem::take(batch));
    let deadline = Instant::now() + LOG_SEND_BACKPRESSURE;
    loop {
        match tx.try_send(ev) {
            Ok(()) => return false,
            Err(TrySendError::Full(returned)) => {
                if Instant::now() >= deadline {
                    *dropped += n;
                    // Report the loss NOW rather than waiting for a next batch that may never
                    // come: a finite burst followed by a quiet `--follow` stream would leave
                    // the count stranded in this local, so the header would show a complete
                    // tail that is silently missing lines (found by /review's structured
                    // Codex pass). Best-effort — if this send also finds the channel full,
                    // the count stays pending and the next batch or EOF reports it.
                    match tx.try_send(AppEvent::LogLinesDropped(*dropped)) {
                        Ok(()) => *dropped = 0,
                        Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Disconnected(_)) => return true,
                    }
                    return false;
                }
                ev = returned;
                thread::sleep(LOG_SEND_RETRY);
            }
            Err(TrySendError::Disconnected(_)) => return true,
        }
    }
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

    fn log_batch(n: usize) -> Vec<LogLine> {
        (0..n).map(|i| LogLine { text: format!("l{i}"), ..LogLine::default() }).collect()
    }

    #[test]
    fn flush_batch_sends_and_empties_the_batch() {
        let (tx, rx) = sync_channel::<AppEvent>(4);
        let mut batch = log_batch(3);
        let mut dropped = 0;
        assert!(!flush_batch(&tx, &mut batch, &mut dropped));
        assert!(batch.is_empty());
        assert_eq!(dropped, 0);
        match rx.try_recv().unwrap() {
            AppEvent::LogLines(lines) => assert_eq!(lines.len(), 3),
            _ => panic!("expected LogLines"),
        }
    }

    #[test]
    fn flush_batch_counts_the_batch_as_dropped_only_after_waiting_out_backpressure() {
        let (tx, _rx) = sync_channel::<AppEvent>(1);
        // Fill the single slot and never drain it: backpressure that never clears.
        tx.try_send(AppEvent::Invalidated).unwrap();
        let mut batch = log_batch(5);
        let mut dropped = 0;
        let started = Instant::now();
        assert!(!flush_batch(&tx, &mut batch, &mut dropped));
        let waited = started.elapsed();
        assert_eq!(dropped, 5, "counted as dropped only after the deadline");
        assert!(batch.is_empty(), "a dropped batch must not be re-sent later");
        assert!(
            waited >= LOG_SEND_BACKPRESSURE,
            "must wait out the backpressure window before declaring loss (waited {waited:?})"
        );
        assert!(waited < LOG_SEND_BACKPRESSURE * 4, "but must not wait unboundedly");
    }

    /// The point of the backpressure window: a channel that clears within it loses NOTHING.
    ///
    /// The handoff is causal, not timed — the drain happens on THIS thread once the flusher has
    /// signalled that it is retrying, so the test cannot flip to `dropped == 7` on a loaded CI
    /// runner the way a `sleep(40ms)` against a 60 ms deadline would (/review's testing pass).
    #[test]
    fn flush_batch_waits_for_a_slot_instead_of_dropping_when_the_drain_is_imminent() {
        let (tx, rx) = sync_channel::<AppEvent>(1);
        tx.try_send(AppEvent::Invalidated).unwrap(); // full
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
        let flusher = thread::spawn(move || {
            let mut batch = log_batch(7);
            let mut dropped = 0;
            started_tx.send(()).unwrap();
            assert!(!flush_batch(&tx, &mut batch, &mut dropped));
            done_tx.send(dropped).unwrap();
        });
        started_rx.recv().unwrap();
        // Free the slot; the flusher's retry loop must take it rather than time out.
        let first = rx.recv().unwrap();
        assert!(matches!(first, AppEvent::Invalidated));
        let dropped = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(dropped, 0, "a slot freed inside the window must not count as loss");
        match rx.recv_timeout(Duration::from_secs(1)).unwrap() {
            AppEvent::LogLines(lines) => assert_eq!(lines.len(), 7),
            _ => panic!("expected the batch to arrive intact"),
        }
        flusher.join().unwrap();
    }

    /// A pending drop count must never be silently zeroed when the channel is congested — that
    /// is the only time it is non-zero, so losing it there is losing it always.
    #[test]
    fn a_pending_drop_count_survives_a_full_channel_instead_of_being_zeroed() {
        let (tx, rx) = sync_channel::<AppEvent>(1);
        tx.try_send(AppEvent::Invalidated).unwrap(); // full: the pre-send must fail
        let mut batch = log_batch(4);
        let mut dropped = 9;
        assert!(!flush_batch(&tx, &mut batch, &mut dropped));
        assert_eq!(dropped, 13, "9 unreported + 4 newly dropped, never reset on Full");
        // Drain one slot, then flush again: the accumulated count is reported into it.
        let _ = rx.recv().unwrap();
        let mut batch2 = log_batch(1);
        assert!(!flush_batch(&tx, &mut batch2, &mut dropped));
        assert!(
            matches!(rx.try_recv().unwrap(), AppEvent::LogLinesDropped(13)),
            "the accumulated 13 must reach the UI once a slot frees"
        );
        // The single freed slot went to the count, so this one-line batch was itself lost —
        // and the counter now carries exactly that, nothing more.
        assert_eq!(dropped, 1);
    }

    /// EINTR is retried, not treated as a dead stream: `pump_lines` maps any read error to EOF
    /// and stops the tail for the session, so a benign signal must not end up there.
    #[test]
    fn read_bounded_line_retries_through_an_interrupted_read() {
        struct EintrOnce {
            data:  Vec<u8>,
            pos:   usize,
            fired: bool,
        }
        impl std::io::Read for EintrOnce {
            fn read(&mut self, _b: &mut [u8]) -> std::io::Result<usize> {
                unreachable!("BufRead path only")
            }
        }
        impl BufRead for EintrOnce {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                if !self.fired {
                    self.fired = true;
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                Ok(&self.data[self.pos..])
            }
            fn consume(&mut self, n: usize) {
                self.pos += n;
            }
        }
        let mut reader = EintrOnce { data: b"hello\n".to_vec(), pos: 0, fired: false };
        let (mut buf, mut discarding) = (Vec::new(), false);
        read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap();
        assert_eq!(buf, b"hello", "EINTR must be retried, not reported as a dead stream");
    }

    /// Boundary: a record of EXACTLY the cap whose newline lands in the same window must not
    /// enter discard mode, and must not swallow or split the record after it.
    #[test]
    fn read_bounded_line_at_exactly_the_cap_does_not_eat_the_next_record() {
        let mut data = vec![b'x'; MAX_LOG_LINE_BYTES];
        data.extend_from_slice(b"\nnext\n");
        let mut reader = std::io::BufReader::new(&data[..]);
        let (mut buf, mut discarding) = (Vec::new(), false);
        assert_eq!(
            read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap(),
            MAX_LOG_LINE_BYTES
        );
        read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap();
        assert_eq!(buf, b"next", "the following record survives intact");
        assert_eq!(read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap(), 0);
    }

    /// A tail that cannot start says so, instead of leaving a view that looks like a quiet
    /// project forever (detection already claimed a compose project exists).
    #[test]
    fn a_tail_that_cannot_start_announces_itself() {
        let _env = crate::ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let empty = tempfile::tempdir().unwrap();
        let prev  = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", empty.path().display().to_string()); // no `docker` anywhere
        let (tx, rx) = sync_channel::<AppEvent>(CHANNEL_CAP);
        let (child, handle) = spawn_log_tail(tx, vec!["cos".to_string()]);
        std::env::set_var("PATH", prev);
        assert!(child.is_none() && handle.is_none());
        match rx.try_recv().expect("a failed tail must be announced") {
            AppEvent::LogLines(lines) => {
                assert_eq!(lines.len(), 1);
                assert!(lines[0].notice);
                assert!(lines[0].text.contains("could not start"), "got: {}", lines[0].text);
            }
            _ => panic!("expected LogLines"),
        }
    }

    #[test]
    fn flush_batch_reports_the_pending_drop_count_before_the_next_batch() {
        let (tx, rx) = sync_channel::<AppEvent>(4);
        let mut batch = log_batch(2);
        let mut dropped = 9;
        assert!(!flush_batch(&tx, &mut batch, &mut dropped));
        assert_eq!(dropped, 0, "the drop count is cleared once reported");
        assert!(matches!(rx.try_recv().unwrap(), AppEvent::LogLinesDropped(9)));
        assert!(matches!(rx.try_recv().unwrap(), AppEvent::LogLines(_)));
    }

    /// A container that writes without a newline must not be able to grow the read buffer
    /// without bound (`read_line` would): the record is capped and the remainder discarded
    /// until the next newline, so the reader resynchronizes instead of inventing lines.
    #[test]
    fn read_bounded_line_caps_a_newline_less_record_and_resyncs() {
        let mut data = vec![b'x'; MAX_LOG_LINE_BYTES * 3];
        data.extend_from_slice(b"\nnext line\n");
        let mut reader = std::io::BufReader::new(&data[..]);
        let mut buf = Vec::new();
        let mut discarding = false;

        let n = read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap();
        assert_eq!(n, MAX_LOG_LINE_BYTES, "over-long record clipped at the cap");
        assert!(discarding, "the rest of the record is marked for discard");

        read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap();
        assert_eq!(buf, b"next line", "the rest of the giant record was discarded, not split");
        assert!(!discarding, "resynchronized at the newline");
        assert_eq!(read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap(), 0, "EOF");
    }

    /// The normal path, end to end: batches respect `LOG_BATCH_MAX`, every line arrives exactly
    /// once in order, and EOF is announced. Nothing pinned any of that before — the batch cap
    /// (the increment's central design claim), the no-loss-across-batch-boundaries property, and
    /// the blocking terminal sends could all be broken with every test still green.
    #[test]
    fn pump_lines_batches_every_line_in_order_and_announces_the_end_of_stream() {
        let mut data = String::new();
        for i in 0..200 {
            data.push_str(&format!("cos-1  | 2026-07-27T00:00:00Z line {i}\n"));
        }
        let (tx, rx) = sync_channel::<AppEvent>(CHANNEL_CAP);
        pump_lines(
            &mut std::io::BufReader::new(data.as_bytes()),
            &tx,
            &["cos".to_string()],
        );
        drop(tx);
        let (mut texts, mut notices) = (Vec::new(), 0);
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AppEvent::LogLines(lines) => {
                    assert!(lines.len() <= LOG_BATCH_MAX, "batch must respect LOG_BATCH_MAX");
                    for l in lines {
                        if l.notice {
                            notices += 1;
                        } else {
                            assert_eq!(l.service.as_deref(), Some("cos"));
                            texts.push(l.text);
                        }
                    }
                }
                AppEvent::LogLinesDropped(n) => panic!("nothing should be dropped: {n}"),
                _ => panic!("unexpected event"),
            }
        }
        assert_eq!(texts.len(), 200, "no line lost or duplicated across batches");
        assert_eq!(texts[0], "line 0");
        assert_eq!(texts[199], "line 199");
        assert_eq!(notices, 1, "end of stream announced exactly once");
    }

    /// Liveness (round-2 fix): a record that hits the cap and then STALLS mid-record — no
    /// newline ever arrives — must still be returned, or the whole tail (including other
    /// services' lines) would freeze behind it. Modelled with a reader that yields the
    /// over-long prefix and then blocks forever, represented here by "would-block".
    #[test]
    fn read_bounded_line_returns_a_truncated_record_without_waiting_for_a_newline() {
        struct StallAfterPrefix {
            data: Vec<u8>,
            pos:  usize,
        }
        impl std::io::Read for StallAfterPrefix {
            fn read(&mut self, _b: &mut [u8]) -> std::io::Result<usize> {
                unreachable!("BufRead path only")
            }
        }
        impl BufRead for StallAfterPrefix {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                if self.pos >= self.data.len() {
                    // The stall: a real pipe would park here forever.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "stalled mid-record",
                    ));
                }
                Ok(&self.data[self.pos..])
            }
            fn consume(&mut self, n: usize) {
                self.pos += n;
            }
        }
        let mut reader =
            StallAfterPrefix { data: vec![b'y'; MAX_LOG_LINE_BYTES + 10], pos: 0 };
        let mut buf = Vec::new();
        let mut discarding = false;
        // Must return the capped prefix rather than reaching the stalling fill_buf.
        let n = read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap();
        assert_eq!(n, MAX_LOG_LINE_BYTES);
        assert!(discarding);
    }

    /// End-to-end liveness (round-3 fix): a writer that emits >4 KB with no newline and then
    /// GOES SILENT must still get its truncated record delivered to the UI. The early return
    /// from `read_bounded_line` alone wasn't enough — the batch also has to be flushed while
    /// `discarding` is set, or the line sits in the batch behind a read that never returns.
    #[test]
    fn a_truncated_record_reaches_the_ui_while_the_writer_is_still_silent() {
        /// Yields `data` once, then BLOCKS on `gate` (a real stalled pipe). When the test drops
        /// its gate sender, the block ends as EOF so the thread exits cleanly.
        struct SilentAfterPrefix {
            data: Vec<u8>,
            pos:  usize,
            gate: Receiver<()>,
        }
        impl std::io::Read for SilentAfterPrefix {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.pos >= self.data.len() {
                    let _ = self.gate.recv(); // parks until the test finishes
                    return Ok(0);
                }
                let n = (self.data.len() - self.pos).min(out.len());
                out[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }

        let (tx, rx) = sync_channel::<AppEvent>(CHANNEL_CAP);
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        // MAX + 10 bytes: the 10 extra land in BufReader's buffer, which is exactly the state
        // that used to suppress the flush.
        let reader = SilentAfterPrefix {
            data: vec![b'z'; MAX_LOG_LINE_BYTES + 10],
            pos:  0,
            gate: gate_rx,
        };
        thread::spawn(move || {
            pump_lines(&mut std::io::BufReader::new(reader), &tx, &[]);
        });

        let ev = rx
            .recv_timeout(Duration::from_secs(3))
            .expect("the truncated record must arrive while the writer is still silent");
        match ev {
            AppEvent::LogLines(lines) => {
                assert_eq!(lines.len(), 1);
                // The record was read up to the 4 KB byte cap and then clamped to the rendered
                // width at decode, so what reaches the view is the clip plus its marker.
                assert_eq!(
                    lines[0].text.chars().count(),
                    crate::watch::logs::MAX_LOG_LINE_CHARS + 1
                );
                assert!(lines[0].text.ends_with('…'));
            }
            _ => panic!("expected LogLines"),
        }
        drop(gate_tx); // release the reader thread
    }

    /// Invalid UTF-8 from a container is decoded lossily by the caller — it must NOT be a read
    /// error, which the loop treats as EOF and would permanently kill the tail.
    #[test]
    fn read_bounded_line_returns_invalid_utf8_bytes_instead_of_erroring() {
        let data: &[u8] = b"cos-1  | ok \xff\xfe bad\nsecond\n";
        let mut reader = std::io::BufReader::new(data);
        let mut buf = Vec::new();
        let mut discarding = false;
        read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("ok"), "text before the invalid bytes survives: {text}");
        assert!(text.contains("bad"), "text AFTER the invalid bytes survives: {text}");
        read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap();
        assert_eq!(buf, b"second", "the stream continues past invalid UTF-8");
    }

    #[test]
    fn read_bounded_line_handles_empty_lines_and_unterminated_tails() {
        let mut reader = std::io::BufReader::new(&b"\na\nnoeol"[..]);
        let mut buf = Vec::new();
        let mut discarding = false;
        // Empty line: non-zero return (0 means EOF) with an empty payload.
        assert_eq!(read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap(), 1);
        assert!(buf.is_empty());
        read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap();
        assert_eq!(buf, b"a");
        read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap();
        assert_eq!(buf, b"noeol", "a final unterminated record is still a line");
        assert_eq!(read_bounded_line(&mut reader, &mut buf, &mut discarding).unwrap(), 0);
    }

    #[test]
    fn flush_batch_stops_the_reader_when_the_receiver_is_gone() {
        let (tx, rx) = sync_channel::<AppEvent>(4);
        drop(rx);
        let mut batch = log_batch(1);
        let mut dropped = 0;
        assert!(flush_batch(&tx, &mut batch, &mut dropped));
    }
}
