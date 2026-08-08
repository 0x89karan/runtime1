use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, SyncSender},
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
// ux.10: `tui_input` widgets consume a crossterm `Event`; the `EventHandler` trait provides
// `Input::handle_event`. Bracketed-paste text is spliced directly (see insert_paste_into_input).
use tui_input::backend::crossterm::EventHandler;

pub mod app;
pub mod approvals;
pub mod converse;
pub mod inspector;
pub mod logs;
pub mod memory;
pub mod overlay;
pub mod pump;
pub mod reader;
pub mod source;
pub mod spawn;
pub mod topology;
pub mod views;

use app::{App, JobOverlayMode, JobsOverlay, MemoryPane, PendingSpawn, SpawnFocus, View};
use overlay::{budget_prefill, budget_needs_second_gate, menu_items, DashboardOverlay, MenuAction, OverlayMode, PendingVerb};
use approvals::ApprovalsMode;
use pump::{spawn_producers, AppEvent};
use source::{detect_source, DataSource};

/// Outcome of `execute_pending_spawn`. Determines whether the TUI stays alive
/// (InjectedViaControl) or the process is replaced by agentd (FellBackToExec).
enum SpawnOutcome {
    /// Written successfully to /agents/control; TUI stays in Dashboard with banner.
    InjectedViaControl { agent_id_hint: String },
    /// Control file absent or write failed; the fallback exec path was taken.
    FellBackToExec,
}

#[derive(clap::Args)]
pub struct Args {
    /// Path to the agentd FUSE mountpoint
    #[arg(long, default_value = "/agents")]
    pub agents_dir: PathBuf,

    /// Management API URL (e.g. http://127.0.0.1:7999). Overrides FUSE.
    /// Falls back to AGENTCTL_URL env var when not set.
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,

    /// Refresh interval in seconds
    #[arg(long, default_value = "1")]
    pub interval: u64,

    /// Force plain-text output (no TUI / no ANSI escapes)
    #[arg(long)]
    pub plain: bool,

    /// Force TUI mode even when stdout is not a TTY (overrides auto-detection)
    #[arg(long, conflicts_with = "plain")]
    pub no_plain: bool,

    /// Path to flight.jsonl for message edge data in the Topology view
    #[arg(long)]
    pub log_path: Option<PathBuf>,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let agents_dir = args.agents_dir.clone();
    let interval   = Duration::from_secs(args.interval.max(1));
    let log_path   = args.log_path;

    // Detect data source: --url > AGENTCTL_URL env > FUSE > HTTP default port.
    // When --url is given, skip the FUSE mount validation.
    let data_source = detect_source(args.url.as_deref(), &agents_dir)?;

    // Decide TUI vs plain mode.
    let is_tty = io::stdout().is_terminal();
    let use_plain = args.plain || (!args.no_plain && !is_tty);

    if use_plain {
        if !is_tty && !args.plain {
            eprintln!("note: stdout is not a TTY — using plain text mode (--plain)");
        }
        run_plain(agents_dir, interval, log_path, data_source)
    } else {
        // Share the source across the render loop + producer threads (Option B).
        let source: Arc<dyn DataSource> = Arc::from(data_source);
        run_tui(agents_dir, interval, log_path, source)
    }
}

fn run_plain(agents_dir: PathBuf, interval: Duration, log_path: Option<PathBuf>, source: Box<dyn DataSource>) -> anyhow::Result<()> {
    let mut app = App::new(agents_dir.clone());
    app.log_path = log_path;
    loop {
        let snap = source.load_snapshot();
        app.apply_snapshot(snap);
        let approvals = source.load_approvals();
        app.update_approvals(approvals);
        let text = views::render_plain(&app);
        print!("{text}");
        println!("---");
        // Flush ensures the snapshot block reaches the reader even when piped.
        // SIGINT terminates the process via the OS default handler; no raw-mode
        // terminal state is active so no explicit cleanup hook is needed.
        io::stdout().flush().ok();
        std::thread::sleep(interval);
    }
}

/// ux.0: outcome of one `step()` — mark the frame dirty, force a snapshot reconcile,
/// or quit the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    Redraw,
    /// Ask the snapshot producer to re-poll NOW (Invalidated → immediate reconcile; F3/C4).
    Reconcile,
    Quit,
}

/// Max events drained from the channel per render tick before yielding to key poll +
/// draw. Bounds per-tick work so a high-rate SSE burst can't starve input/render
/// (anti-livelock). = CHANNEL_CAP, so a full channel still drains in one tick.
const MAX_DRAIN_PER_TICK: usize = pump::CHANNEL_CAP;

static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn handle_shutdown_signal(_sig: libc::c_int) {
    // Signal-safe: an atomic store, no allocation, no locking.
    SHUTDOWN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Installs a minimal SIGTERM/SIGINT handler that only sets an atomic flag.
/// `run_tui_loop`'s 30ms key-poll tick checks it every iteration and returns
/// normally, letting `TermGuard::drop()` restore the terminal before the process
/// exits. Without this, an externally-delivered `kill(2)` (e.g. `docker stop`, or
/// an entrypoint script forwarding SIGTERM) uses the default signal disposition —
/// immediate termination with no unwind — so neither the panic hook nor `Drop`
/// ever runs, leaving the operator's terminal stuck in raw mode + the alternate
/// screen. Ctrl-C-as-a-key (crossterm `Event::Key` with `KeyCode::Char('c')` +
/// `CONTROL`) is unaffected: raw mode already suppresses the tty driver's own
/// SIGINT generation, so this handler only ever fires for out-of-band signals.
fn install_shutdown_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGTERM, handle_shutdown_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, handle_shutdown_signal as *const () as libc::sighandler_t);
    }
}

/// Terminal session guard (C2): owns raw mode + the alternate screen + a panic hook
/// that restores the terminal. `Drop` restores everything (panic-safe), replacing the
/// prior ad-hoc CleanupGuard + separate panic-hook dance in a single owner.
struct TermGuard;
impl TermGuard {
    fn enter() -> anyhow::Result<Self> {
        enable_raw_mode().context("enabling raw mode")?;
        // ux.10: EnableBracketedPaste so a terminal paste arrives as one `Event::Paste`
        // (routed to the focused input widget) instead of a burst of synthetic keystrokes.
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste) {
            let _ = disable_raw_mode();
            return Err(anyhow::Error::from(e)).context("entering alternate screen");
        }
        // Chain a terminal-restoring hook ahead of the previous one so a panic still
        // prints its message after the screen is restored. Gate the terminal restore on
        // the MAIN (render) thread: a detached producer thread panicking must NOT touch
        // raw-mode / the alt-screen while the render loop is still drawing (fix 5).
        let main_id = std::thread::current().id();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == main_id {
                let _ = disable_raw_mode();
                let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste);
            }
            prev(info);
        }));
        Ok(TermGuard)
    }
}
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste);
        // Drop our hook (reinstalls the default) — matches the prior post-loop take_hook().
        let _ = std::panic::take_hook();
    }
}

fn run_tui(
    agents_dir: PathBuf,
    interval: Duration,
    log_path: Option<PathBuf>,
    source: Arc<dyn DataSource>,
) -> anyhow::Result<()> {
    // ux.10-A (D1): probe for a Docker Compose project ONCE, deliberately BEFORE both
    // TermGuard::enter() AND the signal handlers. Before TermGuard because the docker CLI can
    // take a few hundred ms (or fail noisily) and must do so on the normal screen, never
    // inside the alternate screen. Before the handlers because they downgrade SIGINT to "set
    // an atomic the render loop polls" — install them first and a Ctrl-C during a wedged
    // probe would do nothing at all, since the loop isn't running yet (found by /review's
    // Codex pass; the probe also has its own 3 s deadline). The result gates the `[l]` Logs
    // view and its legend entry for the whole session.
    let docker = crate::docker::detect_docker_context();
    install_shutdown_signal_handlers();
    let guard = TermGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend).context("creating terminal")?;
    let mut app = App::new(agents_dir.clone());
    app.log_path = log_path.clone();
    if let Some(ctx) = &docker {
        app.logs_view.enable(ctx.services.clone(), ctx.project.clone());
    }

    run_tui_loop(&mut term, &mut app, &source, interval, docker.as_ref())?;

    // Restore the terminal before any pending exec replaces the process.
    drop(guard);

    // Handle a pending spawn after EACH loop exit — including one queued from a
    // post-inject re-enter (fix 3: the re-enter's pending_exec was previously ignored,
    // so a spawn from the injected view silently quit without launching).
    let mut pending = app.spawn_view.pending_exec.take();
    while let Some(p) = pending {
        // Try /agents/control first; fall back to exec agentd.
        match execute_pending_spawn(&agents_dir, p)? {
            SpawnOutcome::FellBackToExec => {
                // exec_agentd() replaced this process — unreachable on success.
                break;
            }
            SpawnOutcome::InjectedViaControl { agent_id_hint } => {
                // Re-enter to show the banner and let the operator watch the agent.
                let guard2 = TermGuard::enter()?;
                let backend2 = CrosstermBackend::new(io::stdout());
                let mut term2 = Terminal::new(backend2).context("recreating terminal")?;
                let mut app2 = App::new(agents_dir.clone());
                app2.log_path = log_path.clone();
                if let Some(ctx) = &docker {
                    app2.logs_view.enable(ctx.services.clone(), ctx.project.clone());
                }
                app2.spawn_banner =
                    Some(format!("Agent '{}' injected via /agents/control", agent_id_hint));
                run_tui_loop(&mut term2, &mut app2, &source, interval, docker.as_ref())?;
                drop(guard2);
                pending = app2.spawn_view.pending_exec.take();
            }
        }
    }
    Ok(())
}

/// Clear chat-rail focus if a resize just made the rail no longer fit. Found by /ship's
/// Step 11 adversarial pass (Codex structured review + adversarial exec, independently
/// confirmed): without this, `render_dashboard` hides the rail but `rail_focused` stays
/// true, so `handle_dashboard_key` (which checks `rail_focused` before checking
/// visibility) keeps capturing every keystroke into an invisible input box until the
/// operator guesses Esc/Tab. Extracted as a pure `App`-mutating function (rather than
/// inlined in the crossterm event loop) so this logic is unit-testable without a real
/// `Terminal`.
fn on_resize(app: &mut App, w: u16, h: u16) {
    let chrome_rows = views::dashboard_chrome_rows(app.spawn_banner.is_some());
    if !views::converse_rail_fits(w, h.saturating_sub(chrome_rows)) {
        app.converse_view.rail_focused = false;
    }
}

/// Upper bound on a single bracketed paste. Generous for a chat message or a search term,
/// small enough that splicing it is microseconds of work on the render thread.
const MAX_PASTE_CHARS: usize = 8192;

/// ux.10: splice bracketed-paste text into a `tui_input::Input` at the cursor.
///
/// `tui_input`'s crossterm backend does not translate `Event::Paste`, so the text has to be
/// inserted by hand. It is spliced in ONE rebuild rather than as a run of `InsertChar`
/// requests: `InsertChar` walks the value to compare against the cursor (and rebuilds the
/// String when the cursor is interior), so per-char insertion is O(n²) in the paste length —
/// and `route_paste` runs on the MAIN render thread, so a large paste froze the whole cockpit
/// with Ctrl-C inert (raw mode suppresses the tty's SIGINT and the loop is not polling).
/// Found by /review's red-team pass; the bound and the splice together make the cost linear
/// and capped. Control characters are dropped — none of these fields is multi-line, and a
/// pasted ESC has no business reaching a widget.
fn insert_paste_into_input(input: &mut tui_input::Input, text: &str) {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_PASTE_CHARS)
        .collect();
    if clean.is_empty() {
        return;
    }
    let value    = input.value();
    let cursor   = input.cursor().min(value.chars().count());
    let byte_idx = value.char_indices().nth(cursor).map_or(value.len(), |(i, _)| i);
    let mut next = String::with_capacity(value.len() + clean.len());
    next.push_str(&value[..byte_idx]);
    next.push_str(&clean);
    next.push_str(&value[byte_idx..]);
    let next_cursor = cursor + clean.chars().count();
    *input = tui_input::Input::new(next).with_cursor(next_cursor);
}

/// ux.10: route bracketed-paste text to whichever input widget currently has focus.
/// A no-op for any view/focus that has no active text field (the paste is dropped),
/// mirroring how a stray keystroke is ignored outside an input.
fn route_paste(app: &mut App, text: &str) {
    match app.view {
        // ux.13-TUI: the overlay's budget field, and it is FIRST — the rail arm below matches on
        // `View::Dashboard` too, and an overlay open over a focused rail would otherwise send the
        // paste to the rail underneath it (design finding M6: without this arm a pasted token count
        // silently vanishes).
        View::Dashboard if matches!(
            app.dashboard_overlay.as_ref().map(|o| &o.mode),
            Some(OverlayMode::Budget { .. })
        ) => {
            if let Some(ov) = app.dashboard_overlay.as_mut() {
                if let OverlayMode::Budget { input, error } = &mut ov.mode {
                    insert_paste_into_input(input, text);
                    *error = None;
                }
            }
        }
        View::Dashboard if app.converse_view.rail_focused => {
            insert_paste_into_input(&mut app.converse_view.input, text);
        }
        View::Spawn if app.spawn_view.focus == SpawnFocus::TaskField => {
            app.spawn_view.task_input.insert_str(text);
        }
        View::Memory if app.memory_view.search_active => {
            insert_paste_into_input(&mut app.memory_view.search_query, text);
        }
        View::Inspector if app.inspector_view.search_active => {
            insert_paste_into_input(&mut app.inspector_view.search_query, text);
            app.inspector_view.rebuild_view();
        }
        View::Approvals if app.approvals_view.mode == ApprovalsMode::RejectReason => {
            insert_paste_into_input(&mut app.approvals_view.reject_reason, text);
        }
        // ux.10-A: the Logs search field is a tui_input like the others, so a pasted
        // container id / error string can be searched for without retyping it.
        View::Logs if app.logs_view.search_active => {
            insert_paste_into_input(&mut app.logs_view.search_query, text);
            app.logs_view.match_cursor = None;
        }
        _ => {}
    }
}

/// The single event-pushed render loop (Option B). One bounded channel, two detached
/// producer threads (snapshot + optional SSE), a 30 ms crossterm key poll, and a
/// coalesced redraw. Replaces the two prior sync poll-render loops (F9). The render
/// path never blocks on I/O: snapshots/approvals/SSE arrive from producer threads;
/// keys are polled non-blocking (F5).
fn run_tui_loop(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    source: &Arc<dyn DataSource>,
    interval: Duration,
    docker: Option<&crate::docker::DockerContext>,
) -> anyhow::Result<()> {
    // Producers stop when `_producers`/`rx`/`wake_tx` drop at loop exit (detached; never
    // joined — F7). ux.10-A: `_producers` also OWNS the `docker compose logs --follow`
    // child and kills it in `Drop`, so it must stay bound for the whole loop (a `_`
    // binding would drop it immediately and kill the tail before the first frame).
    let (rx, wake_tx, _producers) = spawn_producers(
        Arc::clone(source),
        interval,
        docker.map(|d| d.services.clone()),
    );
    // ux.1: seed the real terminal size once at startup (crossterm::event::Resize keeps
    // it current after this — see the Event::Resize arm below); App::term_size otherwise
    // defaults to DEFAULT_TERM_SIZE, which is only a placeholder for tests/pre-first-frame.
    if let Ok(size) = crossterm::terminal::size() {
        app.term_size = size;
    }
    // ux.13-TUI: seeded HERE, next to `term_size`, and from the SAME source object this loop uses — so
    // the overlay's printed `Equivalent: agentctl …` line names this daemon rather than whatever the CLI
    // would re-resolve. In `run_tui` it was missed by the post-inject re-enter path, which builds a
    // SECOND `App` and copied only `log_path`/`logs_view` (the fix-review pass). Every App that reaches
    // the loop passes through here.
    app.cli_conn = source.cli_connection_flags();
    loop {
        // Checked every tick (~30ms) so an out-of-band SIGTERM/SIGINT unwinds
        // normally instead of hitting the default signal disposition — see
        // install_shutdown_signal_handlers().
        if SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(());
        }
        // ux.13-TUI: perform a confirmed row verb here — AFTER the shutdown check and BEFORE
        // `event::poll` (eng finding E8). The poll ordering is the load-bearing half: after it, a
        // keystroke queued during the blocking call is dispatched FIRST, so a second Enter arms a second
        // cancel and a `q` returns `Effect::Quit` — the loop exits, the verb never sends, and the last
        // frame still claims it is in flight. The `InFlight` frame was already flushed by the previous
        // iteration's `term.draw`, which is the point of the two-phase split.
        //
        // What this placement does NOT do (corrected at /review — the earlier comment overclaimed): the
        // call runs on the loop thread, so a shutdown requested DURING it is not noticed until the next
        // iteration. Checking first means the delay is at most one verb rather than one verb plus a
        // tick, not that SIGTERM/Ctrl-C is unaffected. Removing that delay needs the verb off this
        // thread — see TODOS.md's P3 entry.
        if apply_effects(drain_pending_verb(app, source.as_ref()), app, &wake_tx) {
            return Ok(());
        }
        // Drain up to MAX_DRAIN_PER_TICK events, then always yield to key poll + draw
        // so a high-rate SSE burst can't starve input/render (fix 2 anti-livelock).
        if drain_events(&rx, app, source.as_ref(), &wake_tx, MAX_DRAIN_PER_TICK) {
            return Ok(()); // a step returned Quit
        }
        // ux.1: check for client-side dispatch timeouts once per tick (cheap — small
        // HashMap) before the key poll, so a hung target surfaces its resume hint even
        // if the operator isn't actively pressing keys.
        if app.converse_view.check_dispatch_timeouts() {
            app.dirty = true;
        }
        // Poll for a key, capped at 30 ms so the loop stays responsive.
        if event::poll(Duration::from_millis(30))? {
            match event::read()? {
                Event::Key(key) => {
                    if apply_effects(step(app, AppEvent::Key(key), source.as_ref()), app, &wake_tx) {
                        return Ok(());
                    }
                }
                Event::Resize(w, h) => {
                    app.term_size = (w, h);
                    on_resize(app, w, h);
                    app.dirty = true;
                }
                // ux.10: bracketed paste — route the pasted text to whichever input widget
                // currently has focus, then redraw. Non-focused views ignore it.
                Event::Paste(text) => {
                    route_paste(app, &text);
                    app.dirty = true;
                }
                _ => {}
            }
        }
        // Coalesced redraw: draw at most once per tick, only when something changed.
        if app.dirty {
            term.draw(|f| views::render(f, app))?;
            app.dirty = false;
        }
    }
}

/// Apply a step's effects to `app`; returns `true` if the loop should quit.
/// `Reconcile` pokes the snapshot producer to re-poll immediately (via `wake_tx`).
fn apply_effects(effects: Vec<Effect>, app: &mut App, wake_tx: &SyncSender<()>) -> bool {
    for eff in effects {
        match eff {
            Effect::Redraw => app.dirty = true,
            Effect::Reconcile => {
                let _ = wake_tx.try_send(()); // full → a wake is already pending
            }
            Effect::Quit => return true,
        }
    }
    false
}

/// Drain at most `max` events from `rx`, folding each via `step`. Returns `true` if a
/// step returned Quit. Bounding the drain (vs. draining until empty) is the
/// anti-livelock fix: the loop always falls through to key poll + draw within one tick.
fn drain_events(
    rx: &Receiver<AppEvent>,
    app: &mut App,
    source: &dyn DataSource,
    wake_tx: &SyncSender<()>,
    max: usize,
) -> bool {
    for _ in 0..max {
        match rx.try_recv() {
            Ok(ev) => {
                if apply_effects(step(app, ev, source), app, wake_tx) {
                    return true;
                }
            }
            Err(_) => break, // channel empty (or disconnected) — yield to poll/draw
        }
    }
    false
}

/// ux.10-A: a log-state change is only worth a frame when the Logs view is the one on screen.
/// Off-view the ring, the service registry, and the drop counter still accumulate (that is the
/// point of the eager tail) — only the redraw is skipped. Entering the view repaints anyway,
/// via the `[l]` keypress's own `Effect::Redraw`.
fn logs_redraw(app: &App) -> Vec<Effect> {
    if app.view == View::Logs {
        vec![Effect::Redraw]
    } else {
        vec![]
    }
}

/// Fold one `AppEvent` into `App`, returning render/quit effects (F2). Pure and
/// terminal-free except that the Approvals view calls `source.approve/deny` — which
/// runs on the MAIN thread here, NOT inside a tokio runtime, so `reqwest::blocking`
/// never panics (the reason Option B was chosen over `tokio::select!`).
fn step(app: &mut App, ev: AppEvent, source: &dyn DataSource) -> Vec<Effect> {
    match ev {
        AppEvent::Snapshot(snap) => {
            app.apply_snapshot(*snap);
            vec![Effect::Redraw]
        }
        AppEvent::Approvals(items) => {
            app.update_approvals(items);
            vec![Effect::Redraw]
        }
        AppEvent::Flight(value) => {
            // ux.1: fold into per-target chat state unconditionally, regardless of the
            // active view — a backgrounded target's reply keeps accumulating even while
            // the operator is on Topology/Memory/etc. (Eng Phase 1 "dashboard behind
            // stays live" requirement). No-op for any agent_id not already tracked.
            app.converse_view.on_flight_event(&value);
            // ux.13-TUI: the scheduler's own confirmation that a cancel landed. Clearing the
            // "cancelling…" marker here (as well as on row disappearance) is what keeps the marker from
            // outliving the fact — an `agent_cancelled` event can arrive a whole poll interval before
            // the row leaves the snapshot.
            app.note_cancel_confirmation(&value);
            app.push_event(value);
            vec![Effect::Redraw]
        }
        AppEvent::Invalidated => {
            // Stream gap: mark it AND force an immediate snapshot reconcile rather than
            // waiting for the next interval tick (F3/C4 — snapshot is authoritative).
            app.mark_gap();
            vec![Effect::Redraw, Effect::Reconcile]
        }
        AppEvent::EventsDropped(n) => {
            app.note_dropped(n);
            vec![Effect::Redraw]
        }
        AppEvent::LogLines(batch) => {
            // ux.10-A: folded unconditionally, regardless of the active view — the tail
            // keeps filling its ring while the operator is elsewhere, so `[l]` opens on
            // real scrollback instead of an empty pane. The REDRAW, however, is gated on the
            // Logs view actually being on screen: the tail is spawned eagerly, so an
            // unconditional Redraw made a chatty compose project rebuild the Dashboard (table
            // + rail + attention signals) at the 33 Hz tick ceiling instead of once per
            // snapshot — burning CPU on frames nobody is looking at AND slowing the drain that
            // decides whether log batches get dropped (found by /review's red-team pass).
            app.logs_view.push_lines(batch);
            logs_redraw(app)
        }
        AppEvent::LogLinesDropped(n) => {
            app.logs_view.note_dropped(n);
            logs_redraw(app)
        }
        AppEvent::ProducerDied(which) => {
            // A producer thread panicked (fix 5): surface it as a gap so the feed
            // reads as stalled instead of silently rendering the last state forever.
            // ux.10-A: a dead LOG reader is not a snapshot/SSE gap — reporting it as one
            // would slander the authoritative feed, so it is reported in the Logs view.
            if which == pump::LOGS_PRODUCER {
                app.logs_view
                    .push_lines(vec![logs::LogLine::notice("— log reader stopped —")]);
                return logs_redraw(app);
            }
            app.mark_gap();
            vec![Effect::Redraw]
        }
        AppEvent::Key(key) => step_key(app, key, source),
    }
}

/// Key dispatch, extracted verbatim from the old inline loop (behavior identical for
/// the main path). Returns `Quit` for ctrl-c, `q` on the Dashboard, or when a pending
/// exec was queued (the caller runs it after the loop). Note: the post-inject re-enter
/// now shares this dispatch, so `Esc` on its Dashboard no longer exits (only `q` does),
/// matching the main loop — a deliberate unification of the two formerly-divergent loops.
fn step_key(app: &mut App, key: KeyEvent, source: &dyn DataSource) -> Vec<Effect> {
    let ctrl_c =
        key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl_c {
        return vec![Effect::Quit];
    }
    // Any key clears the post-inject banner (no-op in the main loop where it's None).
    app.spawn_banner = None;
    // Capture BEFORE dispatch: q quits only if we were already on the Dashboard,
    // not if we just navigated back to it. ux.1 bug caught by /qa's interactive pass:
    // this check didn't know about the chat rail's text-capture focus, so typing an
    // ordinary word containing 'q' (e.g. "qa", "quick") into the rail queued
    // Effect::Quit and killed the whole TUI mid-keystroke — 'q' WAS correctly
    // captured as literal rail input by handle_dashboard_key, but this outer,
    // rail-focus-unaware check fired anyway right after. Must also gate on the
    // rail's focus state as it was BEFORE dispatch (same "before, not after"
    // capture discipline as `was_dashboard` itself, for the same reason).
    let was_dashboard = app.view == View::Dashboard;
    let rail_was_focused = app.converse_view.rail_focused;
    // ux.13-TUI: same "capture BEFORE dispatch" discipline, for the same reason. The app TEACHES `q`
    // as dismiss (Approvals' Confirm mode binds `Esc | Char('q')`), so an operator who learned that
    // would press `q` in a Cancel overlay and lose the whole cockpit mid-incident — the ux.1 chat-rail
    // bug class exactly. Captured before dispatch because the overlay may CLOSE during dispatch, and
    // the keypress that closed it must not also quit.
    let overlay_was_open = app.dashboard_overlay.is_some();
    match app.view {
        View::Dashboard => handle_dashboard_key(key, app, source),
        View::AgentDetail | View::System => {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                app.view = View::Dashboard;
            }
        }
        View::Topology => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => app.view = View::Dashboard,
            KeyCode::Up | KeyCode::Char('k') => {
                app.topology_scroll = app.topology_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => app.topology_scroll += 1,
            _ => {}
        },
        View::Memory => handle_memory_key(key, app),
        View::Spawn => handle_spawn_key(key, app, source),
        View::Inspector => handle_inspector_key(key, app),
        View::Approvals => handle_approvals_key(key, app, source),
        View::Credentials => {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                app.view = View::Dashboard;
            }
        }
        View::Logs => handle_logs_key(key, app),
        View::Jobs => handle_jobs_key(key, app),
    }
    let mut effects = vec![Effect::Redraw];
    if matches!(key.code, KeyCode::Char('q'))
        && was_dashboard
        && !rail_was_focused
        && !overlay_was_open
    {
        effects.push(Effect::Quit);
    }
    if app.spawn_view.pending_exec.is_some() {
        effects.push(Effect::Quit);
    }
    effects
}

fn handle_memory_key(key: KeyEvent, app: &mut App) {
    let code = key.code;
    match code {
        // Search mode: [/] enters, Esc exits + clears query.
        KeyCode::Char('/') if !app.memory_view.search_active => {
            app.memory_view.search_active = true;
        }
        KeyCode::Esc if app.memory_view.search_active => {
            app.memory_view.search_active = false;
            app.memory_view.search_query.reset();
        }
        // ux.10: while searching, every other key is edited into the tui_input widget
        // (chars, Backspace, cursor movement, Ctrl-W/U, …). The apply_snapshot tick reads
        // `search_query.value()` to refilter, so no explicit rebuild is needed here.
        _ if app.memory_view.search_active => {
            app.memory_view.search_query.handle_event(&Event::Key(key));
        }
        // Pane cycling (true-tab model — each pane keeps its own scroll).
        KeyCode::Tab if !app.memory_view.search_active => {
            app.memory_view.pane = match app.memory_view.pane {
                MemoryPane::ShortTerm => MemoryPane::LongTerm,
                MemoryPane::LongTerm  => MemoryPane::Kb,
                MemoryPane::Kb        => MemoryPane::ShortTerm,
            };
        }
        // Per-pane scroll.
        KeyCode::Up | KeyCode::Char('k') if !app.memory_view.search_active => {
            let s = app.memory_view.active_scroll_mut();
            *s = s.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') if !app.memory_view.search_active => {
            *app.memory_view.active_scroll_mut() += 1;
        }
        // Back to dashboard.
        KeyCode::Esc | KeyCode::Char('q') if !app.memory_view.search_active => {
            app.view = View::Dashboard;
        }
        _ => {}
    }
}

/// ux.1: reuses Spawn's exact tuple-match idiom (`handle_spawn_key`'s `let focus =
/// app.spawn_view.focus.clone(); match (&focus, code)`) — focus is read from `App` state
/// internally, not threaded in by the caller, so `step_key`'s Dashboard call site and
/// every existing test call site are unaffected by this retrofit's signature.
fn handle_dashboard_key(key: KeyEvent, app: &mut App, source: &dyn DataSource) {
    let code = key.code;

    // ux.13-TUI: while a row-action overlay is open it owns the ENTIRE keyboard, and unmapped keys
    // are no-ops rather than falling through. Intercepting only Enter/Esc/Tab/q would leave
    // `s`/`t`/`m`/`n`/`a`/`c`/`i`/`l` changing `app.view` with the overlay still `Some` — and since
    // `step_key` dispatches on `app.view`, the next key would land in a different view's handler
    // underneath a modal. `j`/`k` would also desync the highlight from the pinned target. Same
    // unconditional-early-return idiom as the Logs search field and Spawn's TaskField below.
    if app.dashboard_overlay.is_some() {
        handle_overlay_key(key, app);
        return;
    }

    let rail_focused = app.converse_view.rail_focused;

    // Rail-focused: captures all input (Spawn's TaskField Esc-capture idiom, mod.rs:518-526)
    // until Esc or Tab returns focus to the table. Enter sends the current input as a
    // chat message via the shared converse::dispatch helper instead of table routing.
    if rail_focused {
        match code {
            KeyCode::Esc | KeyCode::Tab => {
                app.converse_view.rail_focused = false;
            }
            KeyCode::Enter => {
                let target = app.converse_view.active_target.clone();
                // Double-submit guard (CEO Section 1 state machine, caught missing during
                // /review): Enter is a no-op while the active target is DISPATCHING/
                // STREAMING — otherwise a second Enter mid-turn would call `dispatch()`
                // again, and since the agent is no longer "waiting" from the server's
                // point of view, it would attempt to re-spawn the SAME agent_id instead
                // of injecting, corrupting turn order. Input is preserved, not cleared,
                // so the operator can just wait and press Enter again once idle.
                let is_busy = app.converse_view.targets.get(&target)
                    .is_some_and(|s| s.phase != converse::ConversePhase::Idle);
                if is_busy {
                    return;
                }
                // ux.10: read the tui_input value, then reset() the widget (replacing the
                // old std::mem::take of a String). Reset happens inside the non-empty
                // block so an empty Enter is a no-op and the widget state is untouched.
                let text = app.converse_view.input.value().to_string();
                if !text.is_empty() {
                    app.converse_view.input.reset();
                    let state = app.converse_view.targets.entry(target.clone()).or_default();
                    // Optimistic echo: the sent message appears instantly, before the
                    // network round-trip completes (Hour 1 narrative, CEO Step 0E).
                    state.push_history(converse::TurnRole::Operator, text.clone());
                    // FUSE-mode DataSources don't support spawn() or event_stream_url()
                    // (found by /ship's Step 9 red team pass): a spawn always fails, and
                    // even a successful inject can never surface a reply, since
                    // ConverseView is driven entirely by SSE events. Without this gate the
                    // rail would sit at "Dispatching..." until the 30s timeout on every
                    // single message, forever — fail fast instead, mirroring
                    // orchestrate.rs's existing `event_stream_url().ok_or_else(...)` gate.
                    if source.event_stream_url().is_none() {
                        state.push_history(
                            converse::TurnRole::System,
                            "Chat requires the management API — restart agentctl with \
                             --url http://HOST:PORT (agentd needs [management] enabled=true) \
                             to use the chat rail."
                                .to_string(),
                        );
                        return;
                    }
                    // ux.13-TUI: park the turn for the LOOP instead of calling `dispatch` here.
                    // `dispatch` does a `load_snapshot` (5 s client) plus a spawn (3 s), so this arm
                    // used to freeze the whole cockpit — including Ctrl-C — for up to ~8 s on the
                    // most frequent interaction in the app (TODOS.md's ranked P2). Same slot and
                    // same discipline as the row verbs; the difference is that the rail's own
                    // `Dispatching…` phase is the in-flight frame, so no overlay is involved.
                    //
                    // The phase is set BEFORE arming, which is also what keeps the double-submit
                    // guard above correct across the gap: a second Enter while the call is in
                    // flight sees a non-Idle phase and is a no-op. `drain_pending_verb` resets it
                    // to Idle on failure, or Enter would never work again.
                    let state = app.converse_view.targets.entry(target.clone()).or_default();
                    state.phase = converse::ConversePhase::Dispatching;
                    state.last_event_at = Some(std::time::Instant::now());
                    app.pending_verb = Some(PendingVerb::Chat { target, text });
                }
            }
            // ux.1: while the rail is focused it captures all printable characters as
            // chat text (Spawn's TaskField idiom) — scroll/follow MUST use non-printable
            // keys only (arrows, End), never 'j'/'k'/'G' letter aliases, or typing an
            // ordinary word containing those letters would hijack the transcript instead
            // of being entered as text. This is deliberately different from the
            // TABLE-focused idiom below, which has no text-capture concern.
            KeyCode::Up => {
                if let Some(state) = app.converse_view.targets.get_mut(&app.converse_view.active_target) {
                    state.scroll_up();
                }
            }
            KeyCode::Down => {
                if let Some(state) = app.converse_view.targets.get_mut(&app.converse_view.active_target) {
                    state.scroll_down();
                }
            }
            KeyCode::End => {
                if let Some(state) = app.converse_view.targets.get_mut(&app.converse_view.active_target) {
                    state.re_follow();
                }
            }
            // ux.10: every other key (printable chars, Backspace, Left/Right/Home cursor
            // movement, Ctrl-W/U word/line edits) is edited into the tui_input widget.
            // Enter/Esc/Tab/Up/Down/End above are intercepted first so rail semantics
            // (send / defocus / transcript scroll) survive — see the ux.1 note that
            // scroll/follow must use non-printable keys only while the rail captures text.
            _ => {
                app.converse_view.input.handle_event(&Event::Key(key));
            }
        }
        return;
    }

    match code {
        // ux.1: Tab focuses the chat rail; r retargets it to the selected row's agent —
        // both only active while the table (not the rail) has focus. Gated on the rail
        // actually being visible (caught during /review's adversarial Codex pass) — Tab
        // must be a no-op on a terminal too small for the rail, or it would silently
        // swallow every subsequent keystroke into an input box the operator can't see.
        KeyCode::Tab => {
            // Fixed chrome rows render_dashboard always reserves before content_area —
            // derived from views::dashboard_chrome_rows(), the single source of truth
            // (found duplicated as an independent literal here by /ship's Step 9
            // maintainability specialist). A slightly-too-generous estimate here is
            // harmless — worst case Tab focuses the rail one tick before render's own
            // (authoritative) check hides it again; a silent freeze is the bug this guards
            // against.
            let chrome_rows = views::dashboard_chrome_rows(app.spawn_banner.is_some());
            let (w, h) = app.term_size;
            if views::converse_rail_fits(w, h.saturating_sub(chrome_rows)) {
                app.converse_view.rail_focused = true;
            }
        }
        KeyCode::Char('r') => {
            if let Some(agent) = app.selected_agent() {
                app.converse_view.retarget(&agent.id.clone());
            }
        }
        KeyCode::Up | KeyCode::Char('k')   => app.select_prev(),
        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
        KeyCode::Enter                     => {
            if let Some(agent) = app.selected_agent() {
                // ux.2a: route by the highest-priority active signal (actionability, NOT
                // severity — Design Fix 1: an ApprovalPending signal always wins Enter-routing
                // even when a Degraded signal is more severe, since it's the one signal type
                // an operator resolves directly). No active signal keeps the unchanged default.
                // Reuses views::top_attention_signal — the Dashboard's stacked-reason-line
                // picker — so routing and display can never disagree on which signal leads.
                let top = views::top_attention_signal(&agent.attention);
                match top.map(|s| &s.reason) {
                    Some(reader::AttentionReason::ApprovalPending) => {
                        app.view = View::Approvals;
                        app.approvals_view.mode         = ApprovalsMode::List;
                        app.approvals_view.selected_idx = 0;
                        app.approvals_view.result_msg   = None;
                    }
                    Some(reader::AttentionReason::Degraded) => {
                        app.view = View::Credentials;
                    }
                    _ => {
                        // BudgetRisk, EvaluationUnavailable, or no signal at all — AgentDetail
                        // is already agent-scoped, the safest and most informative default.
                        app.view = View::AgentDetail;
                    }
                }
            }
        }
        KeyCode::Char('s') => app.view = View::System,
        KeyCode::Char('t') => { app.view = View::Topology; app.topology_scroll = 0; }
        KeyCode::Char('m') => {
            app.view = View::Memory;
            // Reset memory navigation state on every entry.
            app.memory_view.short_term_scroll = 0;
            app.memory_view.long_term_scroll  = 0;
            app.memory_view.kb_scroll         = 0;
            app.memory_view.pane              = MemoryPane::ShortTerm;
            app.memory_view.search_query.reset();
            app.memory_view.search_active     = false;
        }
        KeyCode::Char('n') => {
            app.view = View::Spawn;
            // Lazy-load templates on first entry.
            app.spawn_view.load();
        }
        KeyCode::Char('i') => {
            app.view = View::Inspector;
            // Load-once: first entry triggers load via apply_snapshot; [r] reloads.
        }
        KeyCode::Char('a') => {
            app.view = View::Approvals;
            app.approvals_view.mode        = ApprovalsMode::List;
            app.approvals_view.selected_idx = 0;
            app.approvals_view.result_msg  = None;
        }
        KeyCode::Char('c') => {
            app.view = View::Credentials;
        }
        // ux.13-TUI: `x` opens the row-action overlay for the SELECTED agent, pinning its id at open
        // time. One key -> graded menu -> confirm, which is what lazygit/k9s/htop actually do (`x`
        // there opens a menu; k9s uses Ctrl-D/Ctrl-K; htop's `k` is a signal picker) — it keeps the
        // irreversible action the one you travel to, and costs one footer hint instead of three.
        KeyCode::Char('x') => {
            if let Some(agent) = app.selected_agent() {
                let id = agent.id.clone();
                app.dashboard_overlay = Some(DashboardOverlay::menu(id));
            }
        }
        // ux.13-TUI: `?` opens the key map. It is bound HERE and not globally because the key table it
        // renders is the Dashboard's; other views own their own footers.
        KeyCode::Char('?') => {
            app.dashboard_overlay = Some(DashboardOverlay::help());
        }
        // ux.10-A: [l] opens the compose log tail. `[l]`, not `[g]` — `[g]` is Spawn's
        // "generate preview" and, although key dispatch is per-view, reusing the letter
        // across views is a footgun the /autoplan eng consensus explicitly rejected.
        // Gated on the startup detection: on bare agentd / QEMU there is no compose project,
        // so the key is inert AND absent from the legend (views.rs reads the same flag).
        KeyCode::Char('l') if app.logs_view.available => {
            app.view = View::Logs;
        }
        // attn.2-R5: `[J]` (capital — lowercase `j` is vim-down navigation above) opens the
        // Jobs view: job schedule rows with a per-row manual "fire now" verb.
        KeyCode::Char('J') => {
            app.view = View::Jobs;
            app.jobs_selected = 0;
            app.jobs_overlay = None;
        }
        _ => {}
    }
}


/// ux.13-TUI: overlay key handling. The overlay owns every key while it is open — see the
/// early-return comment in `handle_dashboard_key`. Unmapped keys are deliberate no-ops.
///
/// **No `source` parameter, on purpose.** A verb's blocking call must never happen during key
/// dispatch (eng finding H1: `HttpSource`'s confirm client blocks up to 3 s, which would freeze the
/// cockpit with no frame drawn and no spinner possible). Confirming a verb writes
/// `app.pending_verb` + an `InFlight` frame and returns; `drain_pending_verb` performs the call from
/// the loop on the next iteration. Keeping the source out of this signature makes that a compile-time
/// property rather than a convention the next contributor has to notice.
fn handle_overlay_key(key: KeyEvent, app: &mut App) {
    let Some(ov) = app.dashboard_overlay.as_ref() else { return };

    // /review (maintainability specialist, CRITICAL): the RENDER path degrades below the box floor to a
    // single line that ignores `ov.mode` entirely, but this handler used to keep running the whole state
    // machine underneath it. On a terminal narrower than 34 cols or shorter than 7 rows the operator saw
    // only " action: <id> — Esc/q dismiss " while Enter still armed Park (a CHECKPOINTED set_budget) and
    // Enter→Enter still armed Cancel — a destructive mutation with no menu, no confirm text, and no
    // visible result, since InFlight/Result collapse into that same line. Fail closed: when the box does
    // not fit, the only live keys are the ones that get you out.
    if !views::overlay_fits_dashboard(app.term_size) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            app.dashboard_overlay = None;
        }
        return;
    }
    // Resolve-at-use against the PINNED id: `apply_snapshot` retargets `selected_id` from a producer
    // thread, so anything reading the live selection here could act on a different agent than the one
    // the operator opened this overlay against.
    //
    // This is a FLOOR, not a guarantee, and deliberately so: a snapshot can land in the channel after
    // `drain_events` and before this keypress is read, so `app.agents` is up to one poll interval stale
    // and the vanished-target guards below can only catch a row that has already been folded away
    // (Codex's adversarial pass). The remaining window is closed server-side — a cancel for an unknown
    // agent is a 404, which `explain_verb_error` renders as "may have already finished" — not here.
    let target    = ov.target(&app.agents).cloned();
    let target_id = ov.target_id.clone();
    let cursor    = ov.cursor;

    match &ov.mode {
        // ── the graded menu ───────────────────────────────────────────────────────────
        OverlayMode::Menu => {
            // Built HERE, not above the match: only this arm reads it, and every keystroke in the budget
            // field was paying for a menu nobody rendered (/review's performance specialist). The
            // renderer builds the same list from the same accessor — `menu_items`' contract is that the
            // two agree, or Enter arms an item the operator was shown as something else.
            let items = target.as_ref()
                .map(|t| menu_items(t, app.budget_resettable()))
                .unwrap_or_default();
            match key.code {
            // `q` dismisses here and does NOT quit — the outer guard in `step_key` is gated on
            // `overlay_was_open` so this cannot fall through to Effect::Quit.
            KeyCode::Esc | KeyCode::Char('q') => app.dashboard_overlay = None,
            KeyCode::Up | KeyCode::Char('k') => set_cursor(app, cursor.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => {
                set_cursor(app, (cursor + 1).min(items.len().saturating_sub(1)));
            }
            KeyCode::Enter => {
                // A blocked item's reason is rendered under the highlighted row, so Enter on it is a
                // no-op rather than an error state the operator then has to dismiss.
                // `items` is empty when the pinned target is gone, so this early return is also the
                // vanished-target gate for the menu's own verbs.
                let Some(item) = items.get(cursor).filter(|i| i.enabled()) else { return };
                match item.action {
                    // Reversible and a single call, so it arms straight off the menu. `limit` came
                    // from `park_limit`, which is what keeps a zero-spend Park from writing the
                    // checkpointed "0 = unlimited" and un-capping the runaway for good (E1).
                    MenuAction::Park { limit } => arm_verb(
                        app,
                        PendingVerb::SetBudget { agent_id: target_id, limit, park: true },
                    ),
                    MenuAction::SetBudget => {
                        let prefill = target.as_ref()
                            .map(|t| budget_prefill(&t.budget))
                            .unwrap_or_default();
                        set_mode(app, OverlayMode::Budget {
                            input: tui_input::Input::new(prefill),
                            error: None,
                        });
                    }
                    MenuAction::Cancel => set_mode(app, OverlayMode::ConfirmCancel),
                }
            }
            _ => {}
            }
        }

        // ── the irreversible verb's own gate ──────────────────────────────────────────
        OverlayMode::ConfirmCancel => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => set_mode(app, OverlayMode::Menu),
            // /review (security specialist): fail closed when the pin has vanished. The box already
            // renders "No action sent: <id> is no longer in the snapshot" in that state, but this arm
            // used to send the cancel anyway — and agent ids are REUSED here (CoS agents have fixed
            // config ids and cron respawns them), so a confirm keypress landing after the pinned
            // instance finished could cancel a fresh agent of the same name while the frame said
            // nothing was sent. The Menu arm was already safe (no target ⇒ no items ⇒ early return).
            KeyCode::Enter | KeyCode::Char('y') if target.is_none() => {
                set_mode(app, OverlayMode::Result { text: target_gone_text(&target_id), ok: false });
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                arm_verb(app, PendingVerb::Cancel { agent_id: target_id });
            }
            _ => {}
        },

        // ── the numeric field ─────────────────────────────────────────────────────────
        OverlayMode::Budget { input, .. } => match key.code {
            KeyCode::Esc => set_mode(app, OverlayMode::Menu),
            KeyCode::Enter => {
                let raw = input.value().trim().to_string();
                match parse_budget(&raw) {
                    Err(e) => set_mode(app, OverlayMode::Budget {
                        input: input.clone(),
                        error: Some(e),
                    }),
                    Ok(limit) => {
                        let needs_gate = target.as_ref()
                            .map(|t| budget_needs_second_gate(limit, &t.budget))
                            // No target to compare against: gate it. The unknown case is the one
                            // where a silent un-cap is likeliest to slip through.
                            .unwrap_or(true);
                        if needs_gate {
                            set_mode(app, OverlayMode::ConfirmBudget { limit });
                        } else {
                            arm_verb(app, PendingVerb::SetBudget {
                                agent_id: target_id, limit, park: false,
                            });
                        }
                    }
                }
            }
            // Everything else edits the widget in place (cursor movement, word-delete, Ctrl-U, …).
            // Paste is routed here too — see `route_paste`. In place rather than clone-then-writeback:
            // the field accepts pasted text up to MAX_PASTE_CHARS, and copying it per keystroke is the
            // shape of the O(n²) paste path a prior increment already had to fix.
            _ => {
                if let Some(ov) = app.dashboard_overlay.as_mut() {
                    if let OverlayMode::Budget { input, error } = &mut ov.mode {
                        input.handle_event(&Event::Key(key));
                        *error = None;
                    }
                }
            }
        },

        // ── the second gate for a removal or a raise (M2) ─────────────────────────────
        OverlayMode::ConfirmBudget { limit } => {
            let limit = *limit;
            match key.code {
                // Back to the FIELD, not the menu: the operator who declines the gate almost always
                // wants to correct the number they just typed.
                KeyCode::Esc | KeyCode::Char('q') => set_mode(app, OverlayMode::Budget {
                    input: tui_input::Input::new(limit.to_string()),
                    error: None,
                }),
                // Same fail-closed rule as ConfirmCancel: a budget written against a reused id lands on
                // a different instance than the dialog was opened on.
                KeyCode::Enter | KeyCode::Char('y') if target.is_none() => {
                    set_mode(app, OverlayMode::Result { text: target_gone_text(&target_id), ok: false });
                }
                KeyCode::Enter | KeyCode::Char('y') => arm_verb(app, PendingVerb::SetBudget {
                    agent_id: target_id, limit, park: false,
                }),
                _ => {}
            }
        }

        // The verb is armed and the loop is about to perform it; there is nothing a key could mean
        // here that would be true. Every key is a no-op, including `q` — see the `overlay_was_open`
        // guard in `step_key`.
        OverlayMode::InFlight { .. } => {}

        // Explicit dismissal only. Any-key-dismisses is the `spawn_banner` behaviour design finding M3
        // rejected: the operator taps a key while reading and loses the only report of what happened.
        OverlayMode::Result { .. } => {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
                app.dashboard_overlay = None;
            }
        }

        // Help closes on its own key too — pressing `?` twice should not leave the operator holding a
        // modal they have to guess their way out of.
        OverlayMode::Help => {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Char('?')) {
                app.dashboard_overlay = None;
            }
        }
    }
}

/// The DX phase's replacement copy for a target that resolved away, shared by every arm that refuses
/// to write because of it.
fn target_gone_text(target_id: &str) -> String {
    format!(
        "No action sent: {target_id} is no longer in the snapshot. It may have finished or been \
         removed; dismiss and select another running agent."
    )
}

fn set_mode(app: &mut App, mode: OverlayMode) {
    if let Some(ov) = app.dashboard_overlay.as_mut() {
        ov.mode = mode;
    }
}

fn set_cursor(app: &mut App, cursor: usize) {
    if let Some(ov) = app.dashboard_overlay.as_mut() {
        ov.cursor = cursor;
    }
}

/// Park the verb for the loop and show the in-flight frame. The two must happen together: an armed
/// verb with no `InFlight` frame is a silent 3 s freeze, and an `InFlight` frame with no armed verb is
/// a spinner that never resolves.
fn arm_verb(app: &mut App, verb: PendingVerb) {
    set_mode(app, OverlayMode::InFlight { label: verb.in_flight_label() });
    app.pending_verb = Some(verb);
}

/// Parse a typed budget. Rejects anything non-numeric rather than saturating: `set_budget` is a
/// security-adjacent number, and a typo silently becoming `0` (≡ unlimited) is the M2 footgun.
fn parse_budget(raw: &str) -> Result<u64, String> {
    if raw.is_empty() {
        return Err("Enter a number of tokens (0 = unlimited).".to_string());
    }
    // `_` and `,` are how humans write 200_000 / 200,000; accept both, reject everything else.
    let cleaned: String = raw.chars().filter(|c| *c != '_' && *c != ',').collect();
    cleaned.parse::<u64>().map_err(|_| {
        format!("'{raw}' is not a token count. Enter digits only (0 = unlimited).")
    })
}

/// Cap on the buffered events inspected after a blocking verb. Generous (a human cannot out-type this in
/// 3 s) but finite, so a wedged stdin can never spin the loop here.
const MAX_DISCARDED_KEYS_PER_VERB: usize = 256;

/// Perform the armed verb from the LOOP, after the in-flight frame has been flushed.
///
/// Called once per tick from `run_tui_loop`, after the shutdown check and BEFORE `event::poll`.
/// Placement is load-bearing (eng finding E8): after the poll, a keystroke queued during the blocking
/// call is dispatched FIRST, so a second Enter arms a second cancel and a `q` returns `Effect::Quit` —
/// the loop exits, the verb never sends, and the last frame the operator saw claimed it was in flight.
///
/// Extracted rather than inlined because `run_tui_loop` needs a real `Terminal` and therefore has zero
/// test coverage (the same reason `on_resize` was extracted).
fn drain_pending_verb(app: &mut App, source: &dyn DataSource) -> Vec<Effect> {
    // `.take()` FIRST, before the call. Any early return or panic path that left the slot filled
    // would re-arm the same verb on the next 30 ms tick — a cancel storm against the scheduler.
    let Some(verb) = app.pending_verb.take() else { return vec![] };

    // `count` is the SERVER's answer for Cancel (native subtree + universal agents parented into it),
    // which the client cannot compute; 0 means "this source cannot know", as over FUSE.
    let outcome = match &verb {
        PendingVerb::Cancel { agent_id } => source.cancel(agent_id),
        PendingVerb::SetBudget { agent_id, limit, .. } => source.set_budget(agent_id, *limit).map(|()| 0),
        // The chat turn has no overlay and no count, so it returns straight out of the match rather
        // than being routed by an earlier statement — an `unreachable!()` guarded by statement order is
        // a panic waiting for the next edit (/review's maintainability specialist).
        PendingVerb::Chat { target, text } => return drain_chat_turn(app, source, target, text),
        // Different target entity (jobs_overlay, not dashboard_overlay) and a different
        // outcome shape (a child id, not a count) — same early-return shape as Chat above,
        // for the same reason: this needs its own handling, not a fourth special case
        // wedged into the count-shaped match below.
        PendingVerb::RunJob { job_id } => return drain_run_job_fire(app, source, job_id),
    };

    // Mark the row BEFORE reading the outcome text: on the confirming source the agent may already be
    // gone from the next snapshot, and on the queued source this marker is the only feedback there is.
    //
    // Deliberately NOT marked on an Err — including the timeout, whose copy says the write "may still
    // have been applied". A row reading "cancelling…" is a claim that a cancel is on its way; on an
    // error the honest surface is the result frame, which stays on screen and says exactly how much is
    // unknown. The marker's own escalation path is for cancels that WERE accepted and then went quiet.
    //
    // On a NON-confirming source (FUSE) `Ok` means only "queued", so the marker outruns what the source
    // can prove (Codex's adversarial pass). It is still the honest signal: the operator DID ask, and the
    // marker self-corrects either way — if the scheduler rejected because the agent is gone the row
    // disappears and the marker with it, and if it rejected for any other reason the row survives and
    // the 60 s escalation turns it into "cancel not confirmed". The result frame carries the
    // "cannot confirm" caveat in the same frame.
    if let (PendingVerb::Cancel { agent_id }, Ok(_)) = (&verb, &outcome) {
        app.mark_cancel_requested(agent_id, source.confirms_mutations());
    }
    // Discard whatever the operator TYPED while the call blocked. The tty buffers those keys; the loop
    // replays them one per iteration against whatever mode is current when it gets there — so two
    // impatient presses during a 3 s cancel would dismiss the Result frame (losing the only report of
    // what a destructive verb did, the exact `spawn_banner` behaviour design finding M3 rejected) and
    // then, with `overlay_was_open` now false, QUIT the cockpit mid-incident (/review's red team). The
    // InFlight hint promises keys are ignored; this is what makes that true.
    //
    // KEYS only. The first version of this drained the whole queue, which ate `Event::Resize` — and
    // since the box-vs-degraded-line decision now reads `app.term_size`, a swallowed resize would leave
    // BOTH the renderer and the key gate deciding against stale dimensions (Codex, reviewing the review
    // fixes: two of them interacting). Resize is applied, not dropped; Ctrl-C is honoured, because "I
    // pressed Ctrl-C during the freeze and it did nothing" is the failure this whole two-phase split
    // exists to avoid; paste is dropped like any other input typed at a frame that no longer exists.
    let mut quit = false;
    for _ in 0..MAX_DISCARDED_KEYS_PER_VERB {
        match event::poll(Duration::ZERO) {
            Ok(true) => match event::read() {
                Ok(Event::Resize(w, h)) => {
                    app.term_size = (w, h);
                    on_resize(app, w, h);
                    app.dirty = true;
                }
                Ok(Event::Key(k))
                    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    quit = true;
                }
                Ok(_) => {}   // a key or paste aimed at the in-flight frame: deliberately dropped
                Err(_) => break,
            },
            _ => break,
        }
    }

    // Self-heal, in case an event was lost some other way: the gate that decides whether a verb can be
    // armed now depends on `term_size`, and it is otherwise only ever updated by `Event::Resize`. One
    // query per verb, not per tick — the repo deliberately avoids ad-hoc size queries on the hot path
    // (they made a test fail under `cargo test`'s no-TTY environment), and this path already did I/O.
    if let Ok(size) = crossterm::terminal::size() {
        if size != app.term_size {
            app.term_size = size;
            on_resize(app, size.0, size.1);
            app.dirty = true;
        }
    }

    let resettable = app.budget_resettable();
    let (text, ok) = match outcome {
        // Requested, not done — even on the confirming source, since the scheduler acts at the next
        // step boundary. `agentctl cancel` prints the same tense: the CLI and the TUI must not
        // describe one write two ways (DX finding).
        Ok(count) if source.confirms_mutations() => (
            format!("{} — {}", verb_requested_text(&verb, count, resettable), verb.equivalent_cli(&app.cli_conn)),
            true,
        ),
        // C5: over FUSE the ONLY signal is that `close(2)` succeeded, i.e. the command was queued.
        // "agent not found", "SetCaps is narrow-only" and "capability is inert" all arrive here as
        // `Ok(())`, so claiming the verb took effect would be a lie the operator cannot check. Say
        // what actually happened, and where the real verdict shows up.
        Ok(_) => (
            format!(
                "Request queued over FUSE; this path cannot confirm the scheduler accepted it. \
                 Watch {}, or check Inspector for fuse_control_error. — {}",
                verb_target(&verb),
                verb.equivalent_cli(&app.cli_conn),
            ),
            true,
        ),
        Err(e) => (explain_verb_error(&e), false),
    };
    set_mode(app, OverlayMode::Result { text, ok });
    // Reconcile: poke the snapshot producer so the row's new state arrives on the next frame rather
    // than up to a full interval later. A Ctrl-C seen during the freeze still quits — after the result
    // is recorded, so the flight log and the frame agree on what happened before the exit.
    if quit {
        return vec![Effect::Redraw, Effect::Quit];
    }
    vec![Effect::Redraw, Effect::Reconcile]
}

/// Perform one chat-rail turn from the LOOP (ux.13-TUI; closes TODOS.md's ~8 s-freeze P2).
///
/// Everything here was previously inline in `handle_dashboard_key`'s rail `Enter` arm and is moved
/// unchanged, including the resolved-id reconciliation, which is the subtle part.
fn drain_chat_turn(
    app:    &mut App,
    source: &dyn DataSource,
    target: &str,
    text:   &str,
) -> Vec<Effect> {
    match converse::dispatch(source, target, text, converse::DEFAULT_MAX_TURNS) {
        Ok(resolved_id) => {
            // If the server resolved a different id than requested (e.g. HttpSource::spawn's
            // "operator-agent" fallback when the response omits `agent_id`), the already-pushed echo
            // lives under the stale `target` key, which will never receive the real agent's events
            // (those are tagged with `resolved_id`). Move it across rather than orphaning it (found by
            // /ship's Step 9 testing + maintainability specialists — two views of the same bug).
            if resolved_id != target {
                if let Some(abandoned) = app.converse_view.targets.remove(target) {
                    let dest = app.converse_view.targets.entry(resolved_id.clone()).or_default();
                    for turn in abandoned.history {
                        dest.push_history(turn.role, turn.text);
                    }
                }
            }
            app.converse_view.active_target = resolved_id.clone();
            // entry().or_default(), not get_mut(): resolved_id's entry may not exist yet even after the
            // move above (e.g. `target` had no prior state to move). get_mut on a fresh resolved_id
            // would silently no-op, dropping this state update and, since every subsequent event for
            // the real agent is looked up by this same key, wedging the conversation forever.
            let state = app.converse_view.targets.entry(resolved_id).or_default();
            state.phase = converse::ConversePhase::Dispatching;
            state.last_event_at = Some(std::time::Instant::now());
            // Reconcile: a spawn just created an agent, so poke the snapshot producer rather than
            // waiting up to a full interval for the row to appear.
            vec![Effect::Redraw, Effect::Reconcile]
        }
        Err(e) => {
            if let Some(state) = app.converse_view.targets.get_mut(target) {
                state.push_history(
                    converse::TurnRole::System,
                    format!("Spawn rejected: {e} — press Enter to retry"),
                );
                // Back to Idle, or the double-submit guard blocks the retry the message just invited.
                // The phase was set optimistically at arm time so the rail could draw `Dispatching…`
                // before the blocking call — this is the other half of that trade.
                state.phase = converse::ConversePhase::Idle;
                state.last_event_at = None;
            }
            vec![Effect::Redraw]
        }
    }
}

/// Perform a manual job fire from the LOOP (attn.2-R5). Mirrors `drain_chat_turn`'s early-
/// return shape: this targets `app.jobs_overlay`, not `dashboard_overlay`, and its outcome is
/// a child id, not a count, so it does not fit the shared match in `drain_pending_verb`.
///
/// Deliberately does NOT replicate that function's discard-buffered-keys-during-the-call
/// block (same accepted gap as `drain_chat_turn` — a Ctrl-C or resize during the ~2 s HTTP
/// round-trip is dropped rather than honoured immediately). Lower risk here than for Chat
/// (whose blocking window can reach ~8 s): filed as a residual, not fixed, given the
/// smaller window and the size of what already shipped this increment.
fn drain_run_job_fire(app: &mut App, source: &dyn DataSource, job_id: &str) -> Vec<Effect> {
    let outcome = source.run_job(job_id);
    let (text, ok) = match &outcome {
        Ok(child_id) => {
            // attn.2-R5 fix: record the fire session-locally, independent of the occurrence
            // ledger the Jobs table's own `last_outcome` column is sourced from (which a
            // manual fire deliberately never touches) — see jobs_last_manual_fire's doc.
            app.jobs_last_manual_fire.insert(
                job_id.to_string(),
                (child_id.clone(), std::time::Instant::now()),
            );
            (format!("Fired '{job_id}' — child '{child_id}'"), true)
        }
        Err(e) => (format!("Fire failed: {}", explain_verb_error(e)), false),
    };
    // Only update the overlay if it's still pinned to the SAME job — resolve-at-use, same
    // discipline `DashboardOverlay::target` uses, in case the operator somehow left the view
    // and reopened a different job's overlay while this call was in flight.
    if let Some(ov) = &mut app.jobs_overlay {
        if ov.target_job_id == job_id {
            ov.mode = JobOverlayMode::Result { text, ok };
        }
    }
    vec![Effect::Redraw, Effect::Reconcile]
}

/// Rewrite a raw transport/HTTP error into WHAT happened, WHY, and WHAT TO DO.
///
/// Shared with `verbs.rs` so `agentctl cancel` and the cockpit's `[x]` explain a failure the same way
/// (the DX phase's one-vocabulary finding; /qa found the error paths still diverged). Keep the wording
/// surface-NEUTRAL for that reason: no "dismiss", no "press", nothing that assumes an overlay.
///
/// Pure and string-based on purpose. E10 killed the alternative (a startup auth probe): there is no
/// unauthenticated way to learn whether agentd has an approval secret — the gate list is exactly the
/// mutating routes, and `/healthz`/`/snapshot` reveal nothing. So the response IS the discovery
/// mechanism, and classifying it after the fact is the only option.
///
/// Unrecognised errors pass through verbatim: inventing an explanation for an error nobody has seen is
/// how a cockpit teaches the wrong fix.
pub fn explain_verb_error(raw: &str) -> String {
    if raw.contains("HTTP 401") || raw.contains("HTTP 403") {
        return "Action refused: approval token missing or wrong (HTTP 401/403). Export the same \
                AGENTOS_APPROVAL_SECRET used by agentd, then run this again — and restart \
                `agentctl watch` if it is open, since it reads the secret once at startup."
            .to_string();
    }
    if raw.contains("HTTP 503") {
        // attn.2-R5 fix (/autoplan retroactive review — HIGH, DX phase): the generic "Action
        // not sent" framing is FALSE for run_job's timeout specifically. The server's own
        // route already `try_send`s the command successfully before the 2 s confirm-wait —
        // a timeout means the confirmation didn't arrive in time, not that nothing happened.
        // Telling the operator to "retry" here can cause exactly the double real-Gmail-fire
        // the concurrent-fire guard exists to prevent. Every other 503 case here (budget/
        // cancel/set-caps/spawn) genuinely has NOT been sent yet — only run_job's server-side
        // string ("timed out waiting for run_job", `management.rs`) distinguishes this one.
        if raw.contains("timed out waiting for run_job") {
            return format!(
                "Uncertain: agentd may have already started this fire even though the \
                 confirmation didn't arrive in time ({}). Check the Jobs view or the flight \
                 log before firing again — retrying blind risks a second, concurrent run of \
                 the same job.",
                raw.trim(),
            );
        }
        return format!(
            "Action not sent: agentd's control channel is unavailable or busy ({}). Wait a second and \
             retry; if it persists, restart agentd.",
            raw.trim(),
        );
    }
    if raw.contains("HTTP 409") {
        // attn.2-R5 fix: the ONE failure mode genuinely new to run_job (a job already has a
        // live run — the concurrent-fire guard's own rejection) was falling through to raw,
        // unexplained JSON. Nanosecond-id collisions (the OTHER thing that could produce a
        // 409 here) are vanishingly rare and self-resolve on retry either way, so one message
        // covering both is accurate without needing to distinguish them.
        return format!(
            "Action not sent: this job already has a live run in progress, or a rare id \
             collision occurred ({}). If a run is genuinely in progress, wait for it to finish \
             (check the Jobs view or the flight log) — firing again now would not be a retry, \
             it would be a second concurrent run.",
            raw.trim(),
        );
    }
    if raw.contains("HTTP 404") {
        return format!(
            "Action not sent: agentd does not know this agent, job, or route ({}). An agent may \
             have already finished — check the agent list; a job id must match [[jobs]] in the \
             connected agentd's config.",
            raw.trim(),
        );
    }
    raw.to_string()
}

fn verb_target(verb: &PendingVerb) -> &str {
    match verb {
        PendingVerb::Cancel { agent_id } | PendingVerb::SetBudget { agent_id, .. } => agent_id,
        PendingVerb::Chat { target, .. } => target,
        // Never rendered, same as Chat above: RunJob returns early from drain_pending_verb
        // (drain_run_job_fire) and never reaches the code that calls this. Present only to
        // keep this match exhaustive over PendingVerb's full type.
        PendingVerb::RunJob { job_id } => job_id,
    }
}

/// `count` is the server's cascade size for Cancel; `0` means the source could not report one, in
/// which case the copy says nothing about how many agents were affected rather than guessing.
fn verb_requested_text(verb: &PendingVerb, count: u64, resettable: bool) -> String {
    match verb {
        PendingVerb::Cancel { agent_id } if count > 0 => format!(
            "Cancel requested for {agent_id} — {count} agent{} flagged, taking effect at the next \
             step boundary",
            if count == 1 { "" } else { "s" },
        ),
        PendingVerb::Cancel { agent_id } =>
            format!("Cancel requested for {agent_id} — takes effect at its next step boundary"),
        // The result frame must not contradict the menu the operator just used. `budget_resettable`
        // decides which is true, and BOTH readings are worse than "reversible": with a window the park
        // expires by itself at the next rollover; without one, exhaustion terminates the agent.
        PendingVerb::SetBudget { agent_id, limit, park: true } if resettable =>
            format!("Park requested for {agent_id} at {limit} tokens — it resumes by itself at the \
                     next budget-window rollover; raise the limit to revive it sooner"),
        PendingVerb::SetBudget { agent_id, limit, park: true } =>
            format!("Park requested for {agent_id} at {limit} tokens — there is no reset window, so \
                     this ENDS the agent at its next admission check"),
        PendingVerb::SetBudget { agent_id, limit, park: false } =>
            format!("Budget for {agent_id} set to {limit} tokens"),
        // Never rendered: the chat turn reports through the rail transcript, not an overlay result.
        PendingVerb::Chat { target, .. } => format!("Sent to {target}"),
        // Never rendered — see verb_target's comment above.
        PendingVerb::RunJob { job_id } => format!("Fired {job_id}"),
    }
}

/// ux.10-A: Logs view keys.
///
/// The `rows` page size comes from `logs::logs_viewport_rows(app.term_size)` — the same
/// helper `views::render_logs` uses — so "scroll one page" and "am I at the bottom" agree
/// with what is actually on screen.
fn handle_logs_key(key: KeyEvent, app: &mut App) {
    let rows = logs::logs_viewport_rows(app.term_size.1);
    // Search field owns the keyboard while active: Enter COMMITS the query (the field
    // closes, the query stays for highlighting + n/N), Esc cancels and clears. Everything
    // else edits the tui_input widget — so typing a word containing 'q'/'j'/'n' searches
    // for it instead of quitting, scrolling, or jumping (the ux.1 rail-focus lesson).
    if app.logs_view.search_active {
        match key.code {
            KeyCode::Enter => app.logs_view.search_active = false,
            KeyCode::Esc => {
                app.logs_view.search_active = false;
                app.logs_view.search_query.reset();
                app.logs_view.match_cursor = None;
            }
            _ => {
                app.logs_view.search_query.handle_event(&Event::Key(key));
                // Editing the query changes the match set, so the n/N cursor is stale.
                app.logs_view.match_cursor = None;
            }
        }
        return;
    }
    // Taken before the mutable borrow below, which would otherwise conflict with `app.view`.
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        app.view = View::Dashboard;
        return;
    }
    let lv = &mut app.logs_view;
    match key.code {
        KeyCode::Char('/')                 => lv.search_active = true,
        KeyCode::Tab                       => lv.cycle_filter(),
        KeyCode::Char('t')                 => lv.absolute_ts = !lv.absolute_ts,
        KeyCode::Char('n')                 => lv.next_match(rows),
        KeyCode::Char('N')                 => lv.prev_match(rows),
        KeyCode::Up | KeyCode::Char('k')   => lv.scroll_by(rows, -1),
        KeyCode::Down | KeyCode::Char('j') => lv.scroll_by(rows, 1),
        KeyCode::PageUp                    => lv.scroll_by(rows, -(rows as isize)),
        KeyCode::PageDown                  => lv.scroll_by(rows, rows as isize),
        // g/G keep their in-view top/bottom meaning (the plan's D7) — they are NOT the
        // view-opening key, which is why the Logs key is `[l]`.
        KeyCode::Char('g') | KeyCode::Home => lv.scroll_to_top(),
        KeyCode::Char('G') | KeyCode::End   => lv.scroll_to_bottom(),
        _ => {}
    }
}

/// attn.2-R5: Jobs view keys. While `jobs_overlay` is open it owns the ENTIRE keyboard —
/// same unconditional-early-return idiom `handle_dashboard_key` uses for
/// `dashboard_overlay`, for the same reason (an unmapped key falling through to row
/// navigation underneath a confirm/in-flight/result frame would desync the highlight or,
/// worse, let a key meant to dismiss a Result frame instead move the selection).
fn handle_jobs_key(key: KeyEvent, app: &mut App) {
    let code = key.code;

    if let Some(ov) = &app.jobs_overlay {
        match &ov.mode {
            JobOverlayMode::ConfirmFire => match code {
                // `y` (not a bare Enter) — a manual fire calls live capabilities right now
                // and can overwrite today's real data if the job already ran today
                // (attn.2-R5 residual); the confirm gate should cost a deliberate keypress,
                // not the same Enter that opened it.
                KeyCode::Char('y') => {
                    let job_id = ov.target_job_id.clone();
                    app.jobs_overlay = Some(JobsOverlay {
                        target_job_id: job_id.clone(),
                        mode: JobOverlayMode::InFlight,
                    });
                    // The key handler never calls the DataSource directly for a verb whose
                    // HTTP round-trip can take ~2 s (same H1 finding PendingVerb exists
                    // for) — parked here, performed by drain_pending_verb/
                    // drain_run_job_fire on the next loop iteration.
                    app.pending_verb = Some(PendingVerb::RunJob { job_id });
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => {
                    app.jobs_overlay = None;
                }
                _ => {}
            },
            // Keys are ignored while in flight — the InFlight frame's own hint promises this.
            JobOverlayMode::InFlight => {}
            JobOverlayMode::Result { .. } => {
                // Any key dismisses. Deliberately not auto-cleared otherwise (same M3 finding
                // as the Dashboard's Result frame): an operator who taps a key while reading
                // must not lose the only report of what the fire did.
                app.jobs_overlay = None;
            }
        }
        return;
    }

    match code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.jobs_selected = app.jobs_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !app.jobs.is_empty() {
                app.jobs_selected = (app.jobs_selected + 1).min(app.jobs.len() - 1);
            }
        }
        KeyCode::Char('f') | KeyCode::Enter => {
            if let Some(job) = app.jobs.get(app.jobs_selected) {
                app.jobs_overlay = Some(JobsOverlay {
                    target_job_id: job.job_id.clone(),
                    mode: JobOverlayMode::ConfirmFire,
                });
            }
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            app.view = View::Dashboard;
        }
        _ => {}
    }
}

fn handle_spawn_key(key: KeyEvent, app: &mut App, source: &dyn DataSource) {
    let focus = app.spawn_view.focus.clone();
    let code = key.code;
    // ux.10: the task field is a multi-line tui_textarea. Tab (focus-cycle) and Esc
    // (defocus) MUST be intercepted BEFORE delegating, or the textarea would swallow
    // them; every other key — including Enter (=newline) and printable chars like 'r'/'g'
    // that are spawn/generate shortcuts elsewhere — is edited into the field.
    if focus == SpawnFocus::TaskField {
        match code {
            KeyCode::Tab => app.spawn_view.focus_next(),
            KeyCode::Esc => app.spawn_view.focus = SpawnFocus::TemplatePicker,
            _ => {
                app.spawn_view.task_input.input(Event::Key(key));
            }
        }
        return;
    }
    match (&focus, code) {
        // Global: Tab cycles focus.
        (_, KeyCode::Tab) => {
            app.spawn_view.focus_next();
        }
        // Global: Esc/q back to Dashboard (only when TaskField is not focused —
        // TaskField Esc is handled above).
        (_, KeyCode::Esc) | (_, KeyCode::Char('q')) => {
            app.view = View::Dashboard;
        }
        // Template picker navigation.
        (SpawnFocus::TemplatePicker, KeyCode::Up | KeyCode::Char('k')) => {
            app.spawn_view.select_template_prev();
        }
        (SpawnFocus::TemplatePicker, KeyCode::Down | KeyCode::Char('j')) => {
            app.spawn_view.select_template_next();
        }
        // Cap toggle navigation and toggle.
        (SpawnFocus::CapToggles, KeyCode::Up | KeyCode::Char('k')) => {
            app.spawn_view.cap_prev();
        }
        (SpawnFocus::CapToggles, KeyCode::Down | KeyCode::Char('j')) => {
            app.spawn_view.cap_next();
        }
        (SpawnFocus::CapToggles, KeyCode::Char(' ') | KeyCode::Enter) => {
            let idx = app.spawn_view.cap_idx;
            app.spawn_view.toggle_cap_at(idx);
        }
        // Generate action: [g] shortcut (outside TaskField) or Enter on button.
        // ux.3 M2: the preview branch (JSON vs TOML) is chosen by the active source, so the
        // operator previews the exact SpawnRequest (same fields/values) a spawn will send.
        (_, KeyCode::Char('g')) => {
            app.spawn_view.do_generate(source);
        }
        (SpawnFocus::ActionGenerate, KeyCode::Enter | KeyCode::Char(' ')) => {
            app.spawn_view.do_generate(source);
        }
        // Spawn action: [r] shortcut (outside TaskField) or Enter on button.
        (_, KeyCode::Char('r')) => {
            do_spawn_action(app, source);
        }
        (SpawnFocus::ActionSpawn, KeyCode::Enter | KeyCode::Char(' ')) => {
            do_spawn_action(app, source);
        }
        _ => {}
    }
}

/// Route the confirmed Spawn action (ux.3 M3). When the active source is HTTP
/// (`event_stream_url().is_some()` → `HttpSource`), resolve the form into a typed
/// `SpawnRequest` and POST it to `/api/v1/spawn` INLINE on the main thread (matching the
/// approve/deny/converse `reqwest::blocking` precedent) — the TUI stays alive, no second
/// agentd is exec'd, and the FUSE `spawn()` stub (which always errors) is unreachable from
/// this view. On success: set the banner + `pending_focus` (M5 auto-drop) and drop into the
/// Dashboard. On failure: surface the server's reason verbatim in the Spawn view's result
/// line (M6) and stay put. When the active source is FUSE, fall through to the unchanged
/// `do_spawn()` → `pending_exec` → `execute_pending_spawn` (`/agents/control` write) path,
/// which keeps the local `ANTHROPIC_API_KEY` gate (M4).
fn do_spawn_action(app: &mut App, source: &dyn DataSource) {
    if source.event_stream_url().is_some() {
        let req = match app.spawn_view.build_spawn_request() {
            Ok(r)  => r,
            Err(e) => { app.spawn_view.result_msg = Some(e); return; }
        };
        match source.spawn(&req) {
            Ok(agent_id) => {
                app.spawn_view.result_msg =
                    Some(format!("Spawned '{agent_id}' via management API."));
                app.spawn_banner =
                    Some(format!("Agent '{agent_id}' spawned via management API"));
                // M5: sticky auto-focus + selection; apply_snapshot binds it once the
                // agent shows up and refuses to wipe it before then.
                app.selected_id   = Some(agent_id.clone());
                app.pending_focus = Some(agent_id);
                // Auto-drop: return to the Dashboard to watch the new agent.
                app.view = View::Dashboard;
            }
            Err(e) => {
                // M6: the server body (e.g. cap.4's 400 "spawn refused … privileged")
                // arrives verbatim from HttpSource::spawn — surface it, stay in the view,
                // set no banner/focus.
                app.spawn_view.result_msg = Some(format!("Spawn failed: {e}"));
            }
        }
    } else {
        // FUSE/exec path unchanged (keeps the local API-key gate — M4).
        app.spawn_view.do_spawn();
    }
}

fn handle_inspector_key(key: KeyEvent, app: &mut App) {
    let code = key.code;
    match code {
        // Search mode: [/] enters, Esc exits + clears query.
        KeyCode::Char('/') if !app.inspector_view.search_active => {
            app.inspector_view.search_active = true;
        }
        KeyCode::Esc if app.inspector_view.search_active => {
            app.inspector_view.search_active = false;
            app.inspector_view.search_query.reset();
            app.inspector_view.rebuild_view();
        }
        // ux.10: delegate all other search-mode keys to the tui_input widget, then
        // rebuild the filtered line list (the Inspector filters eagerly, not per-tick).
        _ if app.inspector_view.search_active => {
            app.inspector_view.search_query.handle_event(&Event::Key(key));
            app.inspector_view.rebuild_view();
        }
        // [Tab] cycles the filter.
        KeyCode::Tab if !app.inspector_view.search_active => {
            app.inspector_view.filter = app.inspector_view.filter.next();
            app.inspector_view.rebuild_view();
        }
        // [r] reloads the flight log.
        KeyCode::Char('r') if !app.inspector_view.search_active => {
            let log = app.log_path.clone();
            app.inspector_view.loaded = false;
            app.inspector_view.load(log.as_deref());
        }
        // Scroll.
        KeyCode::Up | KeyCode::Char('k') if !app.inspector_view.search_active => {
            app.inspector_view.scroll = app.inspector_view.scroll.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') if !app.inspector_view.search_active => {
            let max = app.inspector_view.lines.len().saturating_sub(1);
            app.inspector_view.scroll = (app.inspector_view.scroll + 1).min(max);
        }
        KeyCode::PageUp if !app.inspector_view.search_active => {
            app.inspector_view.scroll = app.inspector_view.scroll.saturating_sub(10);
        }
        KeyCode::PageDown if !app.inspector_view.search_active => {
            let max = app.inspector_view.lines.len().saturating_sub(1);
            app.inspector_view.scroll = (app.inspector_view.scroll + 10).min(max);
        }
        KeyCode::Home if !app.inspector_view.search_active => {
            app.inspector_view.scroll = 0;
        }
        KeyCode::End if !app.inspector_view.search_active => {
            app.inspector_view.scroll = app.inspector_view.lines.len().saturating_sub(1);
        }
        // Back to dashboard.
        KeyCode::Esc | KeyCode::Char('q') if !app.inspector_view.search_active => {
            app.view = View::Dashboard;
        }
        _ => {}
    }
}

fn handle_approvals_key(key: KeyEvent, app: &mut App, source: &dyn DataSource) {
    let code = key.code;
    match app.approvals_view.mode {
        // ── Typing a reject reason ────────────────────────────────────────────
        ApprovalsMode::RejectReason => {
            match code {
                KeyCode::Enter => {
                    let found_id = app.confirm_item().map(|i| i.id.clone());
                    app.approvals_view.result_msg = if let Some(found_id) = found_id {
                        let reason = app.approvals_view.reject_reason.value().to_string();
                        let reason_opt = if reason.is_empty() { None } else { Some(reason.as_str()) };
                        match source.deny(&found_id, reason_opt) {
                            Ok(()) => {
                                app.approvals_items.retain(|i| i.id != found_id);
                                Some(format!("Rejected {found_id}"))
                            }
                            Err(e) => Some(format!("Error: {e}")),
                        }
                    } else {
                        Some("Approval already resolved — refreshed list.".to_string())
                    };
                    app.approvals_view.mode         = ApprovalsMode::List;
                    app.approvals_view.confirmed_id = None;
                    app.approvals_view.reject_reason.reset();
                }
                KeyCode::Esc => {
                    app.approvals_view.mode = ApprovalsMode::Confirm;
                    app.approvals_view.reject_reason.reset();
                }
                // ux.10: all other keys edit the tui_input reject-reason field.
                _ => {
                    app.approvals_view.reject_reason.handle_event(&Event::Key(key));
                }
            }
        }

        // ── 3-option confirm dialog ───────────────────────────────────────────
        ApprovalsMode::Confirm => {
            match code {
                KeyCode::Char('a') => {
                    // Resolve through the same pinned-id helper the renderer uses, so what the dialog
                    // showed is exactly what this key acts on (C2/E12).
                    let found_id = app.confirm_item().map(|i| i.id.clone());
                    app.approvals_view.result_msg = if let Some(found_id) = found_id {
                        match source.approve(&found_id) {
                            Ok(()) => {
                                app.approvals_items.retain(|i| i.id != found_id);
                                Some(format!("Approved {found_id}"))
                            }
                            Err(e) => Some(format!("Error: {e}")),
                        }
                    } else {
                        Some("Approval already resolved — refreshed list.".to_string())
                    };
                    app.approvals_view.mode = ApprovalsMode::List;
                }
                KeyCode::Char('d') => {
                    // "Don't ask again for this kind" — sends auto_approve_kind via FUSE;
                    // HTTP path inherits the default which falls back to plain approve.
                    let found = app.confirm_item().map(|i| (i.id.clone(), i.kind.clone()));
                    app.approvals_view.result_msg = if let Some((found_id, found_kind)) = found {
                        match source.approve_with_kind(&found_id, &found_kind) {
                            Ok(()) => {
                                app.approvals_items.retain(|i| i.id != found_id);
                                // Report the effect the SOURCE actually had. HTTP has no route for the
                                // standing rule and silently degrades to a plain approve, so claiming
                                // "auto for '<kind>'" there told the operator a policy existed on the one
                                // surface that IS an authority boundary (/review's security specialist).
                                if source.supports_auto_approve_kind() {
                                    Some(format!("Approved {found_id} (auto for '{found_kind}')"))
                                } else {
                                    Some(format!(
                                        "Approved {found_id} — but 'don't ask again' is FUSE-only, so \
                                         no auto-approve rule for '{found_kind}' was registered."
                                    ))
                                }
                            }
                            Err(e) => Some(format!("Error: {e}")),
                        }
                    } else {
                        Some("Approval already resolved — refreshed list.".to_string())
                    };
                    app.approvals_view.mode = ApprovalsMode::List;
                }
                KeyCode::Char('r') => {
                    app.approvals_view.mode = ApprovalsMode::RejectReason;
                    app.approvals_view.reject_reason.reset();
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.approvals_view.mode         = ApprovalsMode::List;
                    app.approvals_view.confirmed_id = None;
                }
                _ => {}
            }
        }

        // ── Browse the list ───────────────────────────────────────────────────
        ApprovalsMode::List => {
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    app.approvals_view.selected_idx =
                        app.approvals_view.selected_idx.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let max = app.approvals_items.len().saturating_sub(1);
                    if app.approvals_view.selected_idx < max {
                        app.approvals_view.selected_idx += 1;
                    }
                }
                KeyCode::Enter => {
                    if let Some(item) = app.approvals_items.get(app.approvals_view.selected_idx) {
                        app.approvals_view.confirmed_id = Some(item.id.clone());
                        app.approvals_view.mode         = ApprovalsMode::Confirm;
                        app.approvals_view.result_msg   = None;
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.view = View::Dashboard;
                }
                _ => {}
            }
        }
    }
}

/// Resolve the template, build the config, then:
///   1. Try writing JSON to `/agents/control` (live injection if agentd is running).
///   2. Fall back to writing a temp TOML and exec'ing agentd.
fn execute_pending_spawn(agents_dir: &Path, pending: PendingSpawn) -> anyhow::Result<SpawnOutcome> {
    use std::io::Write as _;

    let resolver = crate::build_resolver(None, None);
    let (cfg, _) = resolver.resolve(&pending.template_name)?;
    if let Some(requires) = &cfg.template.gated_requires {
        crate::spawn::warn_gated_requires(requires);
    }
    let task_str = if pending.task.is_empty() { None } else { Some(pending.task.as_str()) };
    let mut config = cfg.to_agent_config(task_str, pending.extra_caps.clone())?;
    // Strip caps the user explicitly disabled so unchecking a baseline cap revokes it.
    if let Some(agent) = config.agent.as_mut() {
        if let Some(caps) = agent.capabilities.as_mut() {
            caps.retain(|c| !pending.disabled_caps.contains(c));
        }
    }

    // Try live injection via /agents/control if agentd is running.
    let control_path = agents_dir.join("control");
    if control_path.exists() {
        let agent_id = config.agent.as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_else(|| "operator".to_string());
        let capabilities = config.agent.as_ref()
            .and_then(|a| a.capabilities.clone());
        let payload = serde_json::json!({
            "task":         pending.task,
            "id":           agent_id,
            "capabilities": capabilities,
        });
        let json_bytes = serde_json::to_vec(&payload)
            .map_err(|e| anyhow::anyhow!("JSON serialization failed: {e}"))?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&control_path)
            .context("opening /agents/control")?;
        f.write_all(&json_bytes).context("writing to /agents/control")?;
        // Explicitly close to propagate FUSE flush() errors (e.g. EBUSY when
        // the scheduler channel is full). Rust File::drop silently discards close errors.
        let fd = {
            use std::os::unix::io::IntoRawFd as _;
            f.into_raw_fd()
        };
        // SAFETY: fd is valid and exclusively owned — into_raw_fd() consumed the File.
        let rc = unsafe { libc::close(fd) };
        if rc != 0 {
            return Err(anyhow::anyhow!(
                "scheduler rejected the command ({}): try again shortly",
                std::io::Error::last_os_error()
            ));
        }
        let agent_id_hint = payload["id"].as_str().unwrap_or("operator").to_string();
        return Ok(SpawnOutcome::InjectedViaControl { agent_id_hint });
    }

    // Fall back: write TOML config to a temp file and exec agentd.
    let toml_str = toml::to_string_pretty(&config)
        .map_err(|e| anyhow::anyhow!("config serialization failed: {e}"))?;
    let mut tmpfile = tempfile::NamedTempFile::new()
        .context("creating temp config file")?;
    tmpfile.write_all(toml_str.as_bytes())
        .context("writing temp config")?;
    tmpfile.flush()
        .context("flushing temp config")?;
    let (_, path) = tmpfile.keep().context("keeping temp file")?;
    let agentd = crate::spawn::resolve_agentd(&None)?;
    crate::spawn::exec_agentd(&agentd, &path)?;
    Ok(SpawnOutcome::FellBackToExec)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::{
        do_spawn_action, drain_events, drain_pending_verb, explain_verb_error, handle_approvals_key,
        handle_dashboard_key, handle_inspector_key, handle_jobs_key, handle_logs_key, handle_memory_key,
        handle_spawn_key, logs, on_resize, overlay, parse_budget, route_paste, set_mode, step,
        step_key, App, Effect, View, MAX_PASTE_CHARS, PendingVerb,
    };
    use crate::watch::app::{CancelMarker, JobOverlayMode, JobsOverlay, MemoryPane, SpawnFocus};
    use crate::watch::approvals::ApprovalsMode;
    use crate::watch::pump::AppEvent;
    use crate::watch::reader::{self, AgentInfo, BudgetKind, PendingAction, Snapshot};
    use crate::watch::source::{DataSource, HttpSource, SpawnRequest};

    struct TestSource;
    impl DataSource for TestSource {
        fn load_snapshot(&self) -> Snapshot {
            Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
        }
        fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
        fn approve(&self, _id: &str) -> Result<(), String> { Err("mock: no control".into()) }
        fn deny(&self, _id: &str, _reason: Option<&str>) -> Result<(), String> { Err("mock: no control".into()) }
    }

    // ── shutdown signal handling (terminal-corruption fix) ──────────────────

    // Both signals are asserted in ONE test (not two parallel tests) because they
    // share the single process-wide SHUTDOWN_REQUESTED static — separate #[test]
    // fns would race under cargo test's default parallel execution (one test's
    // reset could clobber the flag between another's raise() and assert()).
    #[test]
    fn shutdown_signal_handler_sets_flag_on_sigterm_and_sigint() {
        use super::{install_shutdown_signal_handlers, SHUTDOWN_REQUESTED};
        use std::sync::atomic::Ordering;
        install_shutdown_signal_handlers();

        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        // SAFETY: raise() sends the signal to the current process; the handler
        // only does an atomic store (signal-safe), so this cannot corrupt state.
        unsafe { libc::raise(libc::SIGTERM) };
        assert!(
            SHUTDOWN_REQUESTED.load(Ordering::SeqCst),
            "handler must set the flag synchronously before raise() returns (SIGTERM)"
        );

        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        unsafe { libc::raise(libc::SIGINT) };
        assert!(
            SHUTDOWN_REQUESTED.load(Ordering::SeqCst),
            "handler must set the flag synchronously before raise() returns (SIGINT)"
        );

        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        // Restore the OS default so a later Ctrl-C during the same `cargo test`
        // process (all tests share one binary/process) still aborts normally,
        // instead of being silently absorbed by our handler for the rest of the run.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
    }

    fn make_snapshot(ids: &[&str]) -> Snapshot {
        Snapshot {
            agents: ids.iter().map(|id| AgentInfo {
                id:              id.to_string(),
                status:          "running".to_string(),
                status_detail:   None,
                context_tokens:  0,
                budget:          BudgetKind::Unlimited,
                windowed_spent:  0,
                tools:           vec![],
                parent_id:       None,
                sandbox:         None,
                egress_brokered: 0,
                egress_denied:   0,
                tier:            "native".to_string(),
                isolation:       String::new(),
                pid:             0,
                attention:       vec![],
            }).collect(),
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        }
    }

    fn app_with_agents(ids: &[&str]) -> App {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(make_snapshot(ids));
        app
    }

    // ── handle_dashboard_key ─────────────────────────────────────────────────

    #[test]
    fn dashboard_key_down_arrow_advances_selection() {
        let mut app = app_with_agents(&["a", "b", "c"]);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &TestSource);

        assert_eq!(app.selected_id.as_deref(), Some("b"));
    }

    #[test]
    fn dashboard_key_j_advances_selection() {
        let mut app = app_with_agents(&["a", "b"]);
        handle_dashboard_key(kev(KeyCode::Char('j')), &mut app, &TestSource);
        assert_eq!(app.selected_id.as_deref(), Some("b"));
    }

    #[test]
    fn dashboard_key_up_arrow_decrements_selection() {
        let mut app = app_with_agents(&["a", "b", "c"]);
        app.selected_id = Some("b".to_string());
        handle_dashboard_key(kev(KeyCode::Up), &mut app, &TestSource);

        assert_eq!(app.selected_id.as_deref(), Some("a"));
    }

    #[test]
    fn dashboard_key_k_decrements_selection() {
        let mut app = app_with_agents(&["a", "b"]);
        app.selected_id = Some("b".to_string());
        handle_dashboard_key(kev(KeyCode::Char('k')), &mut app, &TestSource);
        assert_eq!(app.selected_id.as_deref(), Some("a"));
    }

    #[test]
    fn dashboard_key_enter_switches_to_agent_detail_when_selection_present() {
        let mut app = app_with_agents(&["a"]);
        assert_eq!(app.view, View::Dashboard);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);

        assert_eq!(app.view, View::AgentDetail);
    }

    // ── ux.2a: Enter-routing by actionability (Design Fix 1) ───────────────────

    #[test]
    fn dashboard_key_enter_routes_approval_pending_to_approvals() {
        let mut app = app_with_agents(&["a"]);
        app.agents[0].attention.push(reader::AttentionSignal {
            reason: reader::AttentionReason::ApprovalPending,
            since: 10,
            evidence: Some("act_1".to_string()),
        });
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);

        assert_eq!(app.view, View::Approvals);
    }

    #[test]
    fn dashboard_key_enter_routes_degraded_to_credentials() {
        let mut app = app_with_agents(&["a"]);
        app.agents[0].attention.push(reader::AttentionSignal {
            reason: reader::AttentionReason::Degraded,
            since: 10,
            evidence: Some("google".to_string()),
        });
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);

        assert_eq!(app.view, View::Credentials);
    }

    #[test]
    fn dashboard_key_enter_routes_budget_risk_to_agent_detail() {
        let mut app = app_with_agents(&["a"]);
        app.agents[0].attention.push(reader::AttentionSignal {
            reason: reader::AttentionReason::BudgetRisk,
            since: 10,
            evidence: Some("92%".to_string()),
        });
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);

        assert_eq!(app.view, View::AgentDetail);
    }

    #[test]
    fn dashboard_key_enter_approval_wins_over_more_severe_degraded() {
        // Design Fix 1: routing is actionability-driven, not severity-driven — ApprovalPending
        // always wins Enter-routing even though Degraded (Critical) is more severe (Info).
        let mut app = app_with_agents(&["a"]);
        app.agents[0].attention.push(reader::AttentionSignal {
            reason: reader::AttentionReason::Degraded,
            since: 10,
            evidence: Some("google".to_string()),
        });
        app.agents[0].attention.push(reader::AttentionSignal {
            reason: reader::AttentionReason::ApprovalPending,
            since: 10,
            evidence: Some("act_1".to_string()),
        });
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);

        assert_eq!(app.view, View::Approvals);
    }

    #[test]
    fn dashboard_key_enter_does_not_switch_view_when_no_agents() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);

        assert_eq!(app.view, View::Dashboard,
            "Enter with no agents must not navigate to AgentDetail");
    }

    #[test]
    fn dashboard_key_s_switches_to_system_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(kev(KeyCode::Char('s')), &mut app, &TestSource);
        assert_eq!(app.view, View::System);
    }

    #[test]
    fn dashboard_key_t_switches_to_topology_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(kev(KeyCode::Char('t')), &mut app, &TestSource);
        assert_eq!(app.view, View::Topology);
    }

    #[test]
    fn dashboard_key_other_is_noop() {
        let mut app = app_with_agents(&["a"]);
        let original_id = app.selected_id.clone();
        handle_dashboard_key(kev(KeyCode::F(1)), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard);
        assert_eq!(app.selected_id, original_id);
    }

    // ── ux.1 Dashboard focus retrofit ───────────────────────────────────────────

    #[test]
    fn tab_toggles_rail_focus_both_directions() {
        let mut app = app_with_agents(&["a"]);
        assert!(!app.converse_view.rail_focused);
        handle_dashboard_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert!(app.converse_view.rail_focused, "Tab should focus the rail from the table");
        handle_dashboard_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert!(!app.converse_view.rail_focused, "Tab should return focus to the table from the rail");
    }

    #[test]
    fn tab_is_a_noop_when_terminal_too_narrow_for_rail() {
        // Caught during /review's adversarial Codex pass: Tab previously focused the
        // rail unconditionally, silently swallowing every subsequent keystroke into an
        // input box the operator can't even see on a narrow/short terminal.
        let mut app = app_with_agents(&["a"]);
        app.term_size = (80, 24); // below MIN_TOTAL_WIDTH_FOR_RAIL (115)
        handle_dashboard_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert!(!app.converse_view.rail_focused, "Tab must not focus a rail that isn't visible");
    }

    #[test]
    fn resize_below_rail_floor_clears_focus() {
        // Found by /ship's Step 11 adversarial pass (Codex structured review +
        // adversarial exec, independently confirmed): shrinking the terminal while the
        // rail was focused used to leave rail_focused true even though render_dashboard
        // now hides the rail — every keystroke vanished into an invisible input box.
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        on_resize(&mut app, 80, 24); // below MIN_TOTAL_WIDTH_FOR_RAIL (115)
        assert!(!app.converse_view.rail_focused, "focus must clear when the rail no longer fits");
    }

    #[test]
    fn resize_that_keeps_rail_visible_leaves_focus_untouched() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        on_resize(&mut app, 200, 50); // comfortably above the floor
        assert!(app.converse_view.rail_focused, "focus must be preserved when the rail still fits");
    }

    #[test]
    fn esc_returns_focus_to_table_from_rail() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        handle_dashboard_key(kev(KeyCode::Esc), &mut app, &TestSource);
        assert!(!app.converse_view.rail_focused);
    }

    #[test]
    fn rail_focused_char_input_does_not_trigger_view_shortcut() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        handle_dashboard_key(kev(KeyCode::Char('s')), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard, "'s' must be captured as chat input, not a view switch, while the rail has focus");
        assert_eq!(app.converse_view.input.value(), "s");
    }

    #[test]
    fn r_retargets_only_when_table_focused() {
        let mut app = app_with_agents(&["a", "b"]);
        app.select_next(); // select "b"
        handle_dashboard_key(kev(KeyCode::Char('r')), &mut app, &TestSource);
        assert_eq!(app.converse_view.active_target, "b", "r should retarget to the selected row while the table has focus");

        // While rail-focused, 'r' is literal input, not a retarget.
        app.converse_view.rail_focused = true;
        app.converse_view.active_target = "orch-default".to_string();
        handle_dashboard_key(kev(KeyCode::Char('r')), &mut app, &TestSource);
        assert_eq!(app.converse_view.active_target, "orch-default", "r must not retarget while the rail has focus");
        assert_eq!(app.converse_view.input.value(), "r");
    }

    #[test]
    fn enter_routes_to_chat_send_when_rail_focused_vs_existing_routing_when_table_focused() {
        // Table-focused: Enter keeps its existing AgentDetail/Approvals/Credentials routing.
        let mut app = app_with_agents(&["a"]);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);
        assert_eq!(app.view, View::AgentDetail, "existing Enter routing must be unchanged when the table has focus");

        // Rail-focused: Enter sends the typed input as a chat message instead.
        // TestSource doesn't implement event_stream_url (default trait impl → None), so
        // the optimistic echo is followed by the FUSE-capability-gate system line, not a
        // dispatch attempt — this test asserts the echo happened first, which is the
        // behavior under test here. See the dedicated gate test below for the full
        // no-SSE-support behavior.
        let mut app2 = app_with_agents(&["a"]);
        app2.converse_view.rail_focused = true;
        app2.converse_view.input = tui_input::Input::new("hello".to_string());
        handle_dashboard_key(kev(KeyCode::Enter), &mut app2, &TestSource);
        assert_eq!(app2.view, View::Dashboard, "sending a chat message must not change the view");
        assert!(app2.converse_view.input.value().is_empty(), "input box clears after send");
        let state = app2.converse_view.targets.get(&app2.converse_view.active_target).unwrap();
        assert_eq!(state.history.front().unwrap().text, "hello", "optimistic echo must show the sent message immediately");
        assert_eq!(state.history.front().unwrap().role, super::converse::TurnRole::Operator);
    }

    #[test]
    fn enter_fails_fast_with_clear_message_when_source_has_no_event_stream() {
        // Found by /ship's Step 9 red team pass: FuseSource supports neither spawn() nor
        // event_stream_url() (falls to the DataSource trait's defaults), so without this
        // gate the rail would sit at "Dispatching..." until the 30s timeout on every
        // single message over the default local FUSE mode, forever. TestSource also
        // doesn't override event_stream_url() (defaults to None), so it doubles as the
        // "no SSE support" case here.
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        app.converse_view.input = tui_input::Input::new("hello".to_string());
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);

        assert!(app.converse_view.input.value().is_empty(), "input box clears after send");
        let state = app.converse_view.targets.get(&app.converse_view.active_target).unwrap();
        assert_eq!(state.history.len(), 2, "the echo plus the gate's explanation, nothing more");
        assert_eq!(state.history[0].role, super::converse::TurnRole::Operator, "the echo must still show what was typed");
        assert_eq!(state.history[1].role, super::converse::TurnRole::System);
        assert!(
            state.history[1].text.contains("management API"),
            "the operator must be told WHY nothing happens, not left staring at a silent hang"
        );
        assert_eq!(state.phase, super::converse::ConversePhase::Idle, "must not enter Dispatching -- nothing was actually dispatched");
    }

    #[test]
    fn enter_is_a_noop_while_target_is_busy_double_submit_guard() {
        // Caught during /review: without this guard, a second Enter mid-turn would call
        // dispatch() again — since the agent is no longer "waiting" server-side, it would
        // attempt to re-spawn the SAME agent_id instead of injecting, corrupting turn order.
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        app.converse_view.retarget("orch-default");
        app.converse_view.targets.get_mut("orch-default").unwrap().phase = super::converse::ConversePhase::Dispatching;
        app.converse_view.input = tui_input::Input::new("second message".to_string());

        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);

        assert_eq!(app.converse_view.input.value(), "second message", "input must be preserved, not cleared, while busy");
        let state = app.converse_view.targets.get("orch-default").unwrap();
        assert!(state.history.is_empty(), "no dispatch (and no optimistic echo) must happen while busy");
    }

    // Server can resolve a different agent id than requested (HttpSource::spawn falls
    // back to the literal "operator-agent" when the response omits `agent_id`). Found by
    // /ship's Step 9 testing + maintainability specialists as two views of the same bug:
    // get_mut(&resolved_id) silently dropped the state update (testing), and the
    // optimistic echo pushed under the pre-dispatch `target` key was orphaned, never
    // reachable again since future events are tagged with `resolved_id` (maintainability).
    struct ResolvesDifferentIdSource;
    impl DataSource for ResolvesDifferentIdSource {
        fn load_snapshot(&self) -> Snapshot {
            Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
        }
        fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
        fn approve(&self, _id: &str) -> Result<(), String> { Err("n/a".into()) }
        fn deny(&self, _id: &str, _reason: Option<&str>) -> Result<(), String> { Err("n/a".into()) }
        fn spawn(&self, _req: &crate::watch::source::SpawnRequest) -> Result<String, String> {
            Ok("operator-agent".to_string())
        }
        fn event_stream_url(&self) -> Option<String> {
            Some("http://test/api/v1/events".to_string())
        }
        /// This double impersonates the HTTP source (it answers `event_stream_url`), so it must
        /// impersonate the confirm behaviour too — inheriting the `false` default would silently make
        /// it a FUSE-like source and mask the tense split (eng finding E9's trap).
        fn confirms_mutations(&self) -> bool { true }
    }

    #[test]
    fn enter_moves_echo_and_creates_state_when_server_resolves_a_different_id() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        app.converse_view.retarget("requested-id");
        app.converse_view.input = tui_input::Input::new("hello".to_string());

        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &ResolvesDifferentIdSource);

        // ux.13-TUI: the keypress ARMS the turn; the loop performs it. `dispatch` does a
        // `load_snapshot` (5 s client) plus a spawn (3 s), so doing it here froze the cockpit — Ctrl-C
        // included — for up to ~8 s on the app's most frequent interaction (TODOS.md's ranked P2).
        assert_eq!(
            app.pending_verb,
            Some(overlay::PendingVerb::Chat {
                target: "requested-id".to_string(), text: "hello".to_string(),
            }),
            "Enter must park the turn for the loop, not send it from the key handler"
        );
        assert_eq!(app.converse_view.active_target, "requested-id",
            "nothing is resolved yet — the server has not been asked");
        assert_eq!(
            app.converse_view.targets.get("requested-id").map(|s| s.phase.clone()),
            Some(super::converse::ConversePhase::Dispatching),
            "the rail must already show Dispatching…, which is this verb's in-flight frame"
        );

        let effects = drain_pending_verb(&mut app, &ResolvesDifferentIdSource);
        assert!(effects.contains(&Effect::Reconcile), "a spawn should poke the snapshot producer");

        assert_eq!(app.converse_view.active_target, "operator-agent", "rail must follow the server-resolved id");
        assert!(
            !app.converse_view.targets.contains_key("requested-id"),
            "the stale pre-dispatch key must not linger, orphaning the echo behind it"
        );
        let state = app.converse_view.targets.get("operator-agent")
            .expect("a state entry must exist for the server-resolved id, not silently dropped");
        assert_eq!(state.phase, super::converse::ConversePhase::Dispatching);
        assert_eq!(
            state.history.back().map(|t| t.text.as_str()),
            Some("hello"),
            "the optimistic echo must be moved across, not lost"
        );

        // A subsequent delta tagged with the resolved id must be accepted, not dropped.
        let delta = serde_json::json!({
            "kind": "inference_stream_delta",
            "data": { "agent_id": "operator-agent", "turn_seq": 0, "chunk_seq": 0, "text": "hi" }
        });
        app.converse_view.on_flight_event(&delta);
        assert_eq!(
            app.converse_view.targets.get("operator-agent").unwrap().current_reply,
            "hi",
            "delta for the server-resolved id must not be silently discarded"
        );
    }

    /// The failure half of the same migration: the phase is set optimistically at ARM time so the rail
    /// can draw `Dispatching…` before the blocking call, so a rejected dispatch MUST put it back to
    /// Idle — otherwise the double-submit guard blocks the retry the error message itself invites, and
    /// the rail is wedged for the rest of the session.
    #[test]
    fn a_rejected_chat_turn_returns_the_rail_to_idle_so_enter_works_again() {
        /// Looks like HTTP (so the rail's fail-fast gate passes) but rejects the spawn.
        struct ChatRejectSource;
        impl DataSource for ChatRejectSource {
            fn load_snapshot(&self) -> Snapshot {
                Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
            }
            fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
            fn approve(&self, _id: &str) -> Result<(), String> { Err("n/a".into()) }
            fn deny(&self, _id: &str, _r: Option<&str>) -> Result<(), String> { Err("n/a".into()) }
            fn spawn(&self, _req: &crate::watch::source::SpawnRequest) -> Result<String, String> {
                Err("HTTP 429: too many agents".to_string())
            }
            fn event_stream_url(&self) -> Option<String> { Some("http://test/api/v1/events".into()) }
            fn confirms_mutations(&self) -> bool { true }
        }
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        app.converse_view.retarget("orch-default");
        app.converse_view.input = tui_input::Input::new("hello".to_string());

        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &ChatRejectSource);
        assert!(app.pending_verb.is_some());
        drain_pending_verb(&mut app, &ChatRejectSource);

        let state = app.converse_view.targets.get("orch-default").expect("state");
        assert_eq!(state.phase, super::converse::ConversePhase::Idle,
            "a rejected turn must not leave the rail permanently busy");
        assert!(state.history.back().is_some_and(|t| t.text.contains("press Enter to retry")),
            "and must say so: {:?}", state.history.back());

        // Proof it is actually retryable: a second Enter arms again.
        app.converse_view.input = tui_input::Input::new("again".to_string());
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &ChatRejectSource);
        assert!(app.pending_verb.is_some(), "Enter must work after a rejection");
    }

    /// The double-submit guard has to survive the new gap between arming and sending: while the call is
    /// in flight the phase is non-Idle, so a second Enter is a no-op instead of arming a second spawn of
    /// the SAME agent id (which the server would treat as a fresh spawn, corrupting turn order).
    #[test]
    fn a_second_enter_while_a_chat_turn_is_in_flight_is_a_no_op() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        app.converse_view.retarget("orch-default");
        app.converse_view.input = tui_input::Input::new("first".to_string());
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &ResolvesDifferentIdSource);
        let armed = app.pending_verb.clone();

        app.converse_view.input = tui_input::Input::new("second".to_string());
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &ResolvesDifferentIdSource);
        assert_eq!(app.pending_verb, armed, "the armed turn must not be replaced mid-flight");
        assert_eq!(app.converse_view.input.value(), "second",
            "and the operator's text must be preserved, not eaten");
    }

    #[test]
    fn rail_focused_up_down_scroll_transcript_not_typed_as_text() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        app.converse_view.retarget("orch-default");
        handle_dashboard_key(kev(KeyCode::Up), &mut app, &TestSource);
        let state = app.converse_view.targets.get("orch-default").unwrap();
        assert_eq!(state.scroll_up_lines, 1, "Up must scroll, not type, while the rail has focus");
        assert!(app.converse_view.input.value().is_empty(), "Up must not be captured as text input");
    }

    #[test]
    fn rail_focused_letter_j_k_g_are_captured_as_text_not_scroll_shortcuts() {
        // Critical: 'j'/'k'/'G' must NOT be scroll shortcuts while the rail has text
        // focus, or typing an ordinary word containing those letters would hijack the
        // transcript instead of being entered (this was caught and fixed during /review).
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        for c in ['j', 'k', 'G'] {
            handle_dashboard_key(kev(KeyCode::Char(c)), &mut app, &TestSource);
        }
        assert_eq!(app.converse_view.input.value(), "jkG", "j/k/G must be captured as literal chat text while the rail has focus");
    }

    #[test]
    fn rail_focused_end_re_arms_follow() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        app.converse_view.retarget("orch-default");
        {
            let state = app.converse_view.targets.get_mut("orch-default").unwrap();
            state.scroll_up();
            state.push_history(super::converse::TurnRole::Assistant, "msg".to_string());
        }
        handle_dashboard_key(kev(KeyCode::End), &mut app, &TestSource);
        let state = app.converse_view.targets.get("orch-default").unwrap();
        assert!(state.follow, "End must re-arm follow");
        assert_eq!(state.new_since_scroll, 0, "End must clear the unread counter");
    }

    #[test]
    fn dashboard_key_m_switches_to_memory_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(kev(KeyCode::Char('m')), &mut app, &TestSource);
        assert_eq!(app.view, View::Memory);
    }

    #[test]
    fn dashboard_key_n_switches_to_spawn_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(kev(KeyCode::Char('n')), &mut app, &TestSource);
        assert_eq!(app.view, View::Spawn,
            "[n] must switch to Spawn view");
        assert!(app.spawn_view.loaded,
            "spawn_view.load() must have been called on entry");
    }

    // ── handle_memory_key ────────────────────────────────────────────────────

    #[test]
    fn memory_key_slash_enters_search_mode() {
        let mut app = App::new(PathBuf::from("/agents"));
        assert!(!app.memory_view.search_active);
        handle_memory_key(kev(KeyCode::Char('/')), &mut app);
        assert!(app.memory_view.search_active, "/ must activate search mode");
    }

    #[test]
    fn memory_key_esc_exits_search_mode_and_clears_query() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.search_active = true;
        app.memory_view.search_query  = tui_input::Input::new("foo".to_string());
        handle_memory_key(kev(KeyCode::Esc), &mut app);
        assert!(!app.memory_view.search_active, "Esc must exit search mode");
        assert!(app.memory_view.search_query.value().is_empty(), "Esc must clear query");
    }

    #[test]
    fn memory_key_char_appends_to_query_when_searching() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.search_active = true;
        app.memory_view.search_query  = tui_input::Input::new("fo".to_string());
        handle_memory_key(kev(KeyCode::Char('o')), &mut app);
        assert_eq!(app.memory_view.search_query.value(), "foo");
    }

    #[test]
    fn memory_key_backspace_pops_query_char() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.search_active = true;
        app.memory_view.search_query  = tui_input::Input::new("foo".to_string());
        handle_memory_key(kev(KeyCode::Backspace), &mut app);
        assert_eq!(app.memory_view.search_query.value(), "fo");
    }

    #[test]
    fn memory_key_tab_cycles_shortterm_longterm_kb() {
        let mut app = App::new(PathBuf::from("/agents"));
        assert_eq!(app.memory_view.pane, MemoryPane::ShortTerm);
        handle_memory_key(kev(KeyCode::Tab), &mut app);
        assert_eq!(app.memory_view.pane, MemoryPane::LongTerm);
        handle_memory_key(kev(KeyCode::Tab), &mut app);
        assert_eq!(app.memory_view.pane, MemoryPane::Kb);
        handle_memory_key(kev(KeyCode::Tab), &mut app);
        assert_eq!(app.memory_view.pane, MemoryPane::ShortTerm, "tab must wrap around");
    }

    #[test]
    fn memory_key_up_decrements_active_pane_scroll() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.short_term_scroll = 3;
        handle_memory_key(kev(KeyCode::Up), &mut app);
        assert_eq!(app.memory_view.short_term_scroll, 2);
    }

    #[test]
    fn memory_key_scroll_saturates_at_zero() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.short_term_scroll = 0;
        handle_memory_key(kev(KeyCode::Char('k')), &mut app);
        assert_eq!(app.memory_view.short_term_scroll, 0, "scroll must not underflow");
    }

    #[test]
    fn memory_key_j_increments_active_pane_scroll() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_memory_key(kev(KeyCode::Char('j')), &mut app);
        assert_eq!(app.memory_view.short_term_scroll, 1);
    }

    #[test]
    fn memory_key_q_returns_to_dashboard() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Memory;
        handle_memory_key(kev(KeyCode::Char('q')), &mut app);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn memory_key_esc_returns_to_dashboard_when_not_searching() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Memory;
        handle_memory_key(kev(KeyCode::Esc), &mut app);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn app_memory_state_resets_on_m_key() {
        let mut app = App::new(PathBuf::from("/agents"));
        // Pre-set stale state.
        app.memory_view.short_term_scroll = 10;
        app.memory_view.long_term_scroll  = 5;
        app.memory_view.kb_scroll         = 3;
        app.memory_view.search_query      = tui_input::Input::new("old query".to_string());
        app.memory_view.search_active     = true;
        handle_dashboard_key(kev(KeyCode::Char('m')), &mut app, &TestSource);
        assert_eq!(app.memory_view.short_term_scroll, 0, "short_term_scroll must reset");
        assert_eq!(app.memory_view.long_term_scroll,  0, "long_term_scroll must reset");
        assert_eq!(app.memory_view.kb_scroll,         0, "kb_scroll must reset");
        assert!(app.memory_view.search_query.value().is_empty(), "search_query must clear");
        assert!(!app.memory_view.search_active,           "search_active must clear");
        assert_eq!(app.memory_view.pane, crate::watch::app::MemoryPane::ShortTerm,
            "pane must reset to ShortTerm");
    }

    // ── handle_spawn_key ────────────────────────────────────────────────────

    #[test]
    fn spawn_key_esc_returns_to_dashboard_when_not_in_task_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.focus = SpawnFocus::TemplatePicker;
        handle_spawn_key(kev(KeyCode::Esc), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn spawn_key_q_returns_to_dashboard_when_not_in_task_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.focus = SpawnFocus::ActionGenerate;
        handle_spawn_key(kev(KeyCode::Char('q')), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn spawn_key_q_appends_to_task_when_task_field_focused() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.focus = SpawnFocus::TaskField;
        handle_spawn_key(kev(KeyCode::Char('q')), &mut app, &TestSource);
        assert_eq!(app.view, View::Spawn, "view must stay Spawn while in task field");
        assert_eq!(app.spawn_view.task_input.lines().join("\n"), "q", "char must append to task input");
    }

    #[test]
    fn spawn_key_esc_defocuses_task_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.focus = SpawnFocus::TaskField;
        handle_spawn_key(kev(KeyCode::Esc), &mut app, &TestSource);
        assert_eq!(app.view, View::Spawn, "Esc in task field must not exit spawn view");
        assert_eq!(app.spawn_view.focus, SpawnFocus::TemplatePicker,
            "Esc in task field must defocus to TemplatePicker");
    }

    #[test]
    fn spawn_key_tab_cycles_focus() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        // inject a cap so CapToggles is reachable
        app.spawn_view.cap_toggles = vec![(
            agentd::capability::Capability::Spawn,
            "Spawn".to_string(),
            true,
        )];
        assert_eq!(app.spawn_view.focus, SpawnFocus::TemplatePicker);
        handle_spawn_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert_eq!(app.spawn_view.focus, SpawnFocus::TaskField);
        handle_spawn_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert_eq!(app.spawn_view.focus, SpawnFocus::CapToggles);
        handle_spawn_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert_eq!(app.spawn_view.focus, SpawnFocus::ActionGenerate);
        handle_spawn_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert_eq!(app.spawn_view.focus, SpawnFocus::ActionSpawn);
        handle_spawn_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert_eq!(app.spawn_view.focus, SpawnFocus::TemplatePicker, "must wrap");
    }

    #[test]
    fn spawn_key_backspace_removes_last_char_from_task() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TaskField;
        // Type into the textarea so the cursor sits at the end (a freshly-constructed
        // TextArea parks the cursor at the start, where Backspace is a no-op).
        for c in ['h', 'e', 'l', 'l', 'o'] {
            handle_spawn_key(kev(KeyCode::Char(c)), &mut app, &TestSource);
        }
        handle_spawn_key(kev(KeyCode::Backspace), &mut app, &TestSource);
        assert_eq!(app.spawn_view.task_input.lines().join("\n"), "hell");
    }

    #[test]
    fn spawn_key_space_toggles_cap_when_cap_toggles_focused() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::CapToggles;
        app.spawn_view.cap_toggles = vec![(
            agentd::capability::Capability::Spawn,
            "Spawn".to_string(),
            true,
        )];
        app.spawn_view.cap_idx = 0;
        handle_spawn_key(kev(KeyCode::Char(' ')), &mut app, &TestSource);
        assert!(!app.spawn_view.cap_toggles[0].2, "space must toggle cap off");
    }

    #[test]
    fn spawn_key_enter_in_task_field_inserts_newline_not_focus_advance() {
        // ux.10: with the multi-line tui_textarea, Enter inserts a newline IN-FIELD and
        // must NOT advance focus (Tab cycles focus instead; submit is [r]/ActionSpawn).
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TaskField;
        for c in ['a', 'b'] {
            handle_spawn_key(kev(KeyCode::Char(c)), &mut app, &TestSource);
        }
        handle_spawn_key(kev(KeyCode::Enter), &mut app, &TestSource);
        handle_spawn_key(kev(KeyCode::Char('c')), &mut app, &TestSource);
        assert_eq!(app.spawn_view.focus, SpawnFocus::TaskField,
            "Enter in TaskField must NOT advance focus (it inserts a newline)");
        assert_eq!(app.spawn_view.task_input.lines(), &["ab".to_string(), "c".to_string()],
            "Enter must split the task into two lines");
    }

    #[test]
    fn spawn_key_up_in_template_picker_calls_select_template_prev() {
        use crate::watch::spawn::SpawnTemplate;
        use agentd::template::TemplateSource;
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TemplatePicker;
        app.spawn_view.templates = vec![
            SpawnTemplate { name: "a".into(), source: TemplateSource::Repo,
                            description: String::new(), showcases: String::new(), suggested_caps: vec![], sample_tasks: vec![] },
            SpawnTemplate { name: "b".into(), source: TemplateSource::Repo,
                            description: String::new(), showcases: String::new(), suggested_caps: vec![], sample_tasks: vec![] },
        ];
        app.spawn_view.template_idx = 1;
        handle_spawn_key(kev(KeyCode::Up), &mut app, &TestSource);
        assert_eq!(app.spawn_view.template_idx, 0, "Up in TemplatePicker must decrement index");
    }

    #[test]
    fn spawn_key_down_in_template_picker_calls_select_template_next() {
        use crate::watch::spawn::SpawnTemplate;
        use agentd::template::TemplateSource;
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TemplatePicker;
        app.spawn_view.templates = vec![
            SpawnTemplate { name: "a".into(), source: TemplateSource::Repo,
                            description: String::new(), showcases: String::new(), suggested_caps: vec![], sample_tasks: vec![] },
            SpawnTemplate { name: "b".into(), source: TemplateSource::Repo,
                            description: String::new(), showcases: String::new(), suggested_caps: vec![], sample_tasks: vec![] },
        ];
        app.spawn_view.template_idx = 0;
        handle_spawn_key(kev(KeyCode::Down), &mut app, &TestSource);
        assert_eq!(app.spawn_view.template_idx, 1, "Down in TemplatePicker must increment index");
    }

    #[test]
    fn spawn_key_k_in_cap_toggles_calls_cap_prev() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::CapToggles;
        app.spawn_view.cap_toggles = vec![
            (agentd::capability::Capability::Spawn, "Spawn".to_string(), true),
            (agentd::capability::Capability::Spawn, "Spawn2".to_string(), true),
        ];
        app.spawn_view.cap_idx = 1;
        handle_spawn_key(kev(KeyCode::Char('k')), &mut app, &TestSource);
        assert_eq!(app.spawn_view.cap_idx, 0, "'k' in CapToggles must call cap_prev");
    }

    #[test]
    fn spawn_key_j_in_cap_toggles_calls_cap_next() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::CapToggles;
        app.spawn_view.cap_toggles = vec![
            (agentd::capability::Capability::Spawn, "Spawn".to_string(), true),
            (agentd::capability::Capability::Spawn, "Spawn2".to_string(), true),
        ];
        app.spawn_view.cap_idx = 0;
        handle_spawn_key(kev(KeyCode::Char('j')), &mut app, &TestSource);
        assert_eq!(app.spawn_view.cap_idx, 1, "'j' in CapToggles must call cap_next");
    }

    #[test]
    fn spawn_key_g_calls_do_generate_when_not_in_task_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TemplatePicker;
        // No templates loaded — do_generate sets an error result_msg.
        handle_spawn_key(kev(KeyCode::Char('g')), &mut app, &TestSource);
        assert!(app.spawn_view.result_msg.is_some(),
            "'g' outside TaskField must invoke do_generate (result_msg set)");
    }

    #[test]
    fn spawn_key_r_calls_do_spawn_when_not_in_task_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TemplatePicker;
        // No templates loaded — do_spawn sets an error result_msg.
        handle_spawn_key(kev(KeyCode::Char('r')), &mut app, &TestSource);
        assert!(app.spawn_view.result_msg.is_some(),
            "'r' outside TaskField must invoke do_spawn (result_msg set)");
    }

    #[test]
    fn spawn_key_r_appends_to_task_when_task_field_focused() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TaskField;
        handle_spawn_key(kev(KeyCode::Char('r')), &mut app, &TestSource);
        assert_eq!(app.spawn_view.task_input.lines().join("\n"), "r",
            "Char('r') in TaskField must append to task input, not trigger do_spawn");
        assert!(app.spawn_view.pending_exec.is_none(),
            "Char('r') in TaskField must not set pending_exec");
        assert!(app.spawn_view.result_msg.is_none(),
            "Char('r') in TaskField must not set result_msg");
    }

    #[test]
    fn spawn_key_g_appends_to_task_when_task_field_focused() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TaskField;
        handle_spawn_key(kev(KeyCode::Char('g')), &mut app, &TestSource);
        assert_eq!(app.spawn_view.task_input.lines().join("\n"), "g",
            "Char('g') in TaskField must append to task input, not trigger do_generate");
        assert!(app.spawn_view.preview.is_none(),
            "Char('g') in TaskField must not trigger do_generate");
        assert!(app.spawn_view.result_msg.is_none(),
            "Char('g') in TaskField must not set result_msg");
    }

    // ── ux.3: do_spawn_action routing (M3/M5/M6) ──────────────────────────────

    /// FUSE-mode source whose `spawn()` panics — proves the Spawn view never reaches the
    /// FUSE `spawn()` stub (M3 gates strictly on `event_stream_url().is_some()`).
    struct FuseSpawnGuard;
    impl DataSource for FuseSpawnGuard {
        fn load_snapshot(&self) -> Snapshot {
            Snapshot { agents: vec![], budget: None, queue: None, sandbox: None,
                       provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
        }
        fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
        fn approve(&self, _id: &str) -> Result<(), String> { Err("n/a".into()) }
        fn deny(&self, _id: &str, _r: Option<&str>) -> Result<(), String> { Err("n/a".into()) }
        fn spawn(&self, _req: &SpawnRequest) -> Result<String, String> {
            panic!("FUSE-mode Spawn view must never call source.spawn() (M3 gate)");
        }
        // event_stream_url() defaults to None → FUSE mode.
    }

    /// Load the repo template catalogue and select `scout` (read-only, non-privileged
    /// caps) into a Spawn-view App with a task filled in. Shared by the HTTP-mode tests.
    ///
    /// Returns `None` when the catalogue is EMPTY — which happens only where the repo's
    /// `templates/` dir isn't present in the filesystem at all, e.g. the aarch64 QEMU-cross
    /// CI rootfs (the cross harness doesn't copy `templates/` into the emulated image, and
    /// `default_repo_dir`'s exe-walk can't find it). Callers skip-with-notice in that case;
    /// the routing/body logic under test is arch-independent and fully covered on x86_64 +
    /// macOS (native) + the live runtime /qa. A NON-empty catalogue missing `scout` is a real
    /// regression and still panics.
    fn spawn_app_on_scout() -> Option<App> {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.load();
        if app.spawn_view.templates.is_empty() {
            return None; // catalogue fixture absent (e.g. QEMU-cross) — caller skips
        }
        let idx = app.spawn_view.templates.iter().position(|t| t.name == "scout")
            .expect("scout must be present in a non-empty repo catalogue");
        app.spawn_view.template_idx = idx;
        app.spawn_view.rebuild_cap_toggles();
        app.spawn_view.task_input = tui_textarea::TextArea::new(vec!["list /workspace".to_string()]);
        Some(app)
    }

    #[test]
    fn http_mode_spawn_routes_to_management_api_with_caps_and_priority() {
        let server = httpmock::MockServer::start();
        // The route matcher asserts the POST body carries `priority` AND a real toggled
        // cap (`FsRead`, from scout's fs_read = ["/workspace"]) — the load-bearing M1 fix.
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/v1/spawn")
                .body_contains("\"priority\"")
                .body_contains("FsRead");
            then.status(201).json_body(serde_json::json!({"agent_id": "scout-1"}));
        });
        let source = HttpSource::new(server.base_url());

        let Some(mut app) = spawn_app_on_scout() else {
            eprintln!("SKIP http_mode_spawn_routes_to_management_api_with_caps_and_priority: \
                       repo template catalogue not present (e.g. QEMU-cross rootfs); \
                       arch-independent routing is covered on x86_64/macOS");
            return;
        };
        do_spawn_action(&mut app, &source);

        mock.assert(); // exactly one matching POST — no 2nd agentd exec'd
        assert_eq!(app.pending_focus.as_deref(), Some("scout-1"),
            "success must set sticky auto-focus (M5)");
        assert_eq!(app.selected_id.as_deref(), Some("scout-1"),
            "success must set the selection to the new agent (M5)");
        assert!(app.spawn_banner.is_some(), "success must set the confirmed banner");
        assert_eq!(app.view, View::Dashboard, "auto-drop into the Dashboard on success");
        assert!(app.spawn_view.pending_exec.is_none(),
            "HTTP path must NOT queue a local agentd exec");
    }

    #[test]
    fn http_mode_spawn_privileged_refusal_surfaces_reason_no_focus() {
        let server = httpmock::MockServer::start();
        // cap.4's deny-by-default gate returns 400 (NOT 403 — M6) with the reason + remedy.
        server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/v1/spawn");
            then.status(400).body(
                "spawn refused: unrestricted (all capabilities) is privileged; \
                 set AGENTOS_ALLOW_PRIVILEGED_SPAWN=1 to allow operator-driven privileged spawns");
        });
        let source = HttpSource::new(server.base_url());

        let Some(mut app) = spawn_app_on_scout() else {
            eprintln!("SKIP http_mode_spawn_privileged_refusal_surfaces_reason_no_focus: \
                       repo template catalogue not present (e.g. QEMU-cross rootfs); \
                       arch-independent routing is covered on x86_64/macOS");
            return;
        };
        do_spawn_action(&mut app, &source);

        let msg = app.spawn_view.result_msg.as_deref().unwrap_or("");
        assert!(msg.contains("privileged"),
            "the server's refusal reason must land in the Spawn view's result line (M6): {msg}");
        assert!(app.pending_focus.is_none(), "a refused spawn must NOT set auto-focus");
        assert!(app.spawn_banner.is_none(), "a refused spawn must NOT set a banner");
        assert_eq!(app.view, View::Spawn, "must stay in the Spawn view on failure (no teardown)");
    }

    #[test]
    fn fuse_mode_spawn_never_calls_source_spawn_stub() {
        // FUSE mode (event_stream_url None) must fall through to do_spawn()/pending_exec —
        // NOT source.spawn() (whose FUSE stub always errors). FuseSpawnGuard panics if the
        // stub is reached. No templates loaded → do_spawn short-circuits before any exec.
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.focus = SpawnFocus::ActionSpawn;
        do_spawn_action(&mut app, &FuseSpawnGuard);
        assert!(app.pending_focus.is_none(), "FUSE path must not set HTTP auto-focus");
        assert!(app.spawn_banner.is_none(), "FUSE path must not set the HTTP banner");
        assert_eq!(app.spawn_view.result_msg.as_deref(), Some("No template selected."),
            "FUSE path routes to do_spawn(), not the HTTP spawn");
    }

    // ── handle_dashboard_key: [a] opens Approvals view ───────────────────────

    #[test]
    fn dashboard_key_a_switches_to_approvals_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(kev(KeyCode::Char('a')), &mut app, &TestSource);
        assert_eq!(app.view, View::Approvals, "[a] must switch to Approvals view");
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "entering Approvals view must reset mode to List");
        assert_eq!(app.approvals_view.selected_idx, 0,
            "entering Approvals view must reset selection to 0");
        assert!(app.approvals_view.result_msg.is_none(),
            "entering Approvals view must clear result_msg");
    }

    // ── handle_approvals_key ─────────────────────────────────────────────────

    fn make_pending_action(id: &str, kind: &str) -> PendingAction {
        PendingAction {
            id:       id.to_string(),
            agent_id: "scout".to_string(),
            kind:     kind.to_string(),
            risk:     "medium".to_string(),
            summary:  "test action".to_string(),
            args:     serde_json::Value::Null,
            age_secs: 0,
        }
    }

    fn app_with_approvals(ids: &[(&str, &str)]) -> App {
        let mut app = App::new(PathBuf::from("/nonexistent"));
        app.approvals_items = ids.iter().map(|(id, kind)| make_pending_action(id, kind)).collect();
        app.view = View::Approvals;
        app
    }

    #[test]
    fn approvals_key_q_returns_to_dashboard_from_list() {
        let mut app = app_with_approvals(&[]);
        handle_approvals_key(kev(KeyCode::Char('q')), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard,
            "'q' in Approvals list mode must return to Dashboard");
    }

    #[test]
    fn approvals_key_esc_returns_to_dashboard_from_list() {
        let mut app = app_with_approvals(&[]);
        handle_approvals_key(kev(KeyCode::Esc), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard,
            "Esc in Approvals list mode must return to Dashboard");
    }

    #[test]
    fn approvals_key_enter_enters_confirm_when_items_present() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        handle_approvals_key(kev(KeyCode::Enter), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::Confirm,
            "Enter with pending items must enter Confirm mode");
        assert_eq!(app.view, View::Approvals,
            "view must stay Approvals after Enter");
    }

    #[test]
    fn approvals_key_enter_is_noop_when_no_items() {
        let mut app = app_with_approvals(&[]);
        handle_approvals_key(kev(KeyCode::Enter), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "Enter with no items must stay in List mode");
    }

    #[test]
    fn approvals_key_j_advances_selection() {
        let mut app = app_with_approvals(&[("act_0", "w"), ("act_1", "w")]);
        handle_approvals_key(kev(KeyCode::Char('j')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.selected_idx, 1, "'j' must advance selection");
    }

    #[test]
    fn approvals_key_k_decrements_selection() {
        let mut app = app_with_approvals(&[("act_0", "w"), ("act_1", "w")]);
        app.approvals_view.selected_idx = 1;
        handle_approvals_key(kev(KeyCode::Char('k')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.selected_idx, 0, "'k' must decrement selection");
    }

    #[test]
    fn approvals_key_k_saturates_at_zero() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.selected_idx = 0;
        handle_approvals_key(kev(KeyCode::Char('k')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.selected_idx, 0, "k at 0 must not underflow");
    }

    #[test]
    fn approvals_key_j_saturates_at_last() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        handle_approvals_key(kev(KeyCode::Char('j')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.selected_idx, 0, "j at last must not overflow");
    }

    #[test]
    fn approvals_confirm_r_enters_reject_reason_mode() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(kev(KeyCode::Char('r')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::RejectReason,
            "'r' in Confirm mode must switch to RejectReason mode");
        assert!(app.approvals_view.reject_reason.value().is_empty(),
            "reject_reason must be cleared when entering RejectReason mode");
    }

    #[test]
    fn approvals_confirm_esc_returns_to_list() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(kev(KeyCode::Esc), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "Esc in Confirm mode must return to List mode (not Dashboard)");
        assert_eq!(app.view, View::Approvals);
    }

    #[test]
    fn approvals_reject_reason_char_appends_to_reason() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        handle_approvals_key(kev(KeyCode::Char('x')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.reject_reason.value(), "x");
    }

    #[test]
    fn approvals_reject_reason_backspace_pops_char() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        app.approvals_view.reject_reason = tui_input::Input::new("foo".to_string());
        handle_approvals_key(kev(KeyCode::Backspace), &mut app, &TestSource);
        assert_eq!(app.approvals_view.reject_reason.value(), "fo");
    }

    #[test]
    fn approvals_reject_reason_esc_cancels_to_confirm() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        app.approvals_view.reject_reason = tui_input::Input::new("partial".to_string());
        handle_approvals_key(kev(KeyCode::Esc), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::Confirm,
            "Esc in RejectReason mode must return to Confirm (not List)");
        assert!(app.approvals_view.reject_reason.value().is_empty(),
            "reject_reason must be cleared on Esc cancel");
    }

    #[test]
    fn approvals_reject_reason_enter_with_no_control_file_sets_error_msg() {
        // TestSource.deny() returns Err("mock: no control") → result_msg is set.
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        app.approvals_view.reject_reason = tui_input::Input::new("too risky".to_string());
        handle_approvals_key(kev(KeyCode::Enter), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "after Enter in RejectReason mode must return to List");
        // result_msg is set (either success or error — we're testing the error path here).
        assert!(app.approvals_view.result_msg.is_some(),
            "result_msg must be set after submit attempt");
    }

    #[test]
    fn approvals_confirm_approve_with_no_control_file_sets_error_msg() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode         = ApprovalsMode::Confirm;
        // Pin the id the way `Enter` does. Without it this test silently exercised the
        // "already resolved" branch instead of the approve call it names (ux.13-TUI step 4).
        app.approvals_view.confirmed_id = Some("act_0".to_string());
        handle_approvals_key(kev(KeyCode::Char('a')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "after Approve must return to List mode");
        assert_eq!(app.approvals_view.result_msg.as_deref(), Some("Error: mock: no control"),
            "the source's failure must surface, not a fabricated success");
    }

    #[test]
    fn approvals_confirm_dont_ask_again_with_no_control_file_sets_error_msg() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode         = ApprovalsMode::Confirm;
        app.approvals_view.confirmed_id = Some("act_0".to_string());
        handle_approvals_key(kev(KeyCode::Char('d')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "after 'don't ask again' must return to List mode");
        assert!(app.approvals_view.result_msg.is_some(),
            "result_msg must be set after don't-ask-again attempt");
    }

    // ── ux.13-TUI step 4: what the dialog acts on is what the dialog showed (C2/E12) ──────
    //
    // The approval gate is the one real authority boundary in the cockpit, so both directions of
    // the pinned-id resolution get a test: a pin that is still pending must reach the source with
    // the PINNED id, and a pin that resolved out-of-band must reach the source not at all.

    /// Records every id the view sends, so "did not call the source" is an assertion rather than
    /// an inference from a message string.
    #[derive(Default)]
    struct RecordingApprovalSource {
        approved: std::sync::Mutex<Vec<String>>,
        denied:   std::sync::Mutex<Vec<String>>,
    }
    impl DataSource for RecordingApprovalSource {
        fn load_snapshot(&self) -> Snapshot {
            Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
        }
        fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
        fn approve(&self, id: &str) -> Result<(), String> {
            self.approved.lock().unwrap().push(id.to_string());
            Ok(())
        }
        fn deny(&self, id: &str, _reason: Option<&str>) -> Result<(), String> {
            self.denied.lock().unwrap().push(id.to_string());
            Ok(())
        }
    }

    #[test]
    fn approvals_approve_sends_the_pinned_id_even_when_the_list_reordered() {
        let src = RecordingApprovalSource::default();
        let mut app = app_with_approvals(&[("act_9", "kb_write"), ("act_1", "shell_exec")]);
        app.approvals_view.mode         = ApprovalsMode::Confirm;
        app.approvals_view.confirmed_id = Some("act_1".to_string());
        app.approvals_view.selected_idx = 0; // highlight drifted to the other item

        handle_approvals_key(kev(KeyCode::Char('a')), &mut app, &src);

        assert_eq!(src.approved.lock().unwrap().as_slice(), ["act_1"],
            "must approve the PINNED id, never the highlighted row");
        assert_eq!(app.approvals_view.result_msg.as_deref(), Some("Approved act_1"));
        assert!(!app.approvals_items.iter().any(|i| i.id == "act_1"),
            "the resolved item must leave the list");
        assert!(app.approvals_items.iter().any(|i| i.id == "act_9"),
            "and the untouched item must remain");
    }

    #[test]
    fn approvals_approve_on_an_already_resolved_pin_calls_nothing_and_says_so() {
        let src = RecordingApprovalSource::default();
        // The pinned approval was resolved out-of-band (Telegram / `agentctl approve` / expiry) and
        // the next `update_approvals` replaced the list. Only `act_9` survives.
        let mut app = app_with_approvals(&[("act_9", "kb_write")]);
        app.approvals_view.mode         = ApprovalsMode::Confirm;
        app.approvals_view.confirmed_id = Some("act_1".to_string());

        handle_approvals_key(kev(KeyCode::Char('a')), &mut app, &src);

        assert!(src.approved.lock().unwrap().is_empty(),
            "a vanished pin must never fall through to the surviving item: {:?}", src.approved.lock().unwrap());
        assert_eq!(app.approvals_view.result_msg.as_deref(),
            Some("Approval already resolved — refreshed list."));
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List);
        assert!(app.approvals_items.iter().any(|i| i.id == "act_9"),
            "and nothing may be removed from the list on the no-op path");
    }

    #[test]
    fn approvals_reject_sends_the_pinned_id_and_no_op_when_it_vanished() {
        // Direction 1: still pending → the pinned id is denied.
        let src = RecordingApprovalSource::default();
        let mut app = app_with_approvals(&[("act_9", "kb_write"), ("act_1", "shell_exec")]);
        app.approvals_view.mode         = ApprovalsMode::RejectReason;
        app.approvals_view.confirmed_id = Some("act_1".to_string());
        app.approvals_view.selected_idx = 0;
        app.approvals_view.reject_reason = tui_input::Input::new("too risky".to_string());
        handle_approvals_key(kev(KeyCode::Enter), &mut app, &src);
        assert_eq!(src.denied.lock().unwrap().as_slice(), ["act_1"], "reject must use the pinned id");
        assert_eq!(app.approvals_view.result_msg.as_deref(), Some("Rejected act_1"));
        assert!(app.approvals_view.confirmed_id.is_none(), "the pin is released after resolving");

        // Direction 2: resolved out-of-band → nothing is sent.
        let src2 = RecordingApprovalSource::default();
        let mut app2 = app_with_approvals(&[("act_9", "kb_write")]);
        app2.approvals_view.mode         = ApprovalsMode::RejectReason;
        app2.approvals_view.confirmed_id = Some("act_1".to_string());
        handle_approvals_key(kev(KeyCode::Enter), &mut app2, &src2);
        assert!(src2.denied.lock().unwrap().is_empty(), "a vanished pin must not deny a different approval");
        assert_eq!(app2.approvals_view.result_msg.as_deref(),
            Some("Approval already resolved — refreshed list."));
    }

    // ── ux.0: step() / event pump ────────────────────────────────────────────
    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    // ux.10: the focused-input handlers now take a full `KeyEvent` (so tui-input /
    // tui-textarea widgets receive real crossterm events). This wraps a bare `KeyCode`
    // for the many existing key-dispatch tests.
    fn kev(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn step_snapshot_applies_and_redraws() {
        let mut app = App::new(PathBuf::from("/agents"));
        let effects = step(
            &mut app,
            AppEvent::Snapshot(Box::new(make_snapshot(&["a1", "a2"]))),
            &TestSource,
        );
        assert_eq!(app.agents.len(), 2);
        assert_eq!(effects, vec![Effect::Redraw]);
    }

    #[test]
    fn step_key_ctrl_c_quits() {
        let mut app = App::new(PathBuf::from("/agents"));
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(step_key(&mut app, ev, &TestSource), vec![Effect::Quit]);
    }

    #[test]
    fn step_key_q_on_dashboard_quits() {
        let mut app = App::new(PathBuf::from("/agents"));
        assert_eq!(app.view, View::Dashboard);
        assert!(step_key(&mut app, key('q'), &TestSource).contains(&Effect::Quit));
    }

    #[test]
    fn step_key_q_off_dashboard_does_not_quit() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Memory;
        let effects = step_key(&mut app, key('q'), &TestSource);
        assert!(!effects.contains(&Effect::Quit));
        assert_eq!(app.view, View::Dashboard, "memory 'q' navigates back, not quit");
    }

    #[test]
    fn step_key_q_while_rail_focused_types_not_quits() {
        // Real crash caught by /qa's interactive pass against the compiled binary:
        // typing an ordinary word containing 'q' (e.g. "qa") into the chat rail
        // triggered an unwanted quit mid-keystroke. handle_dashboard_key correctly
        // captured 'q' as literal rail input, but step_key's outer "q quits the
        // Dashboard" convenience check didn't know about rail focus and fired
        // anyway, since app.view is still Dashboard while the rail has focus (the
        // rail is part of Dashboard, not a separate View). This test exercises the
        // FULL step_key pipeline, not handle_dashboard_key in isolation — the bug
        // lived specifically in the interaction between the two.
        let mut app = App::new(PathBuf::from("/agents"));
        app.converse_view.rail_focused = true;
        let effects = step_key(&mut app, key('q'), &TestSource);
        assert!(!effects.contains(&Effect::Quit), "'q' must not quit while the rail has text focus");
        assert_eq!(app.converse_view.input.value(), "q", "'q' must be captured as literal chat input");
        assert_eq!(app.view, View::Dashboard);
    }

    // ── ux.10-A: Logs view ───────────────────────────────────────────────────
    //
    // These exercise the DISPATCH GATE and the key/state machine, never the shell-out:
    // `docker compose ps` / `logs` are only reached from `run_tui`/`spawn_producers`, which
    // no test enters. `/qa` covers the real subprocess + the orphan-kill at runtime.

    fn logs_app(available: bool) -> App {
        let mut app = App::new(PathBuf::from("/agents"));
        if available {
            app.logs_view.enable(vec!["cos".to_string(), "agent".to_string()], "test-project".to_string());
        }
        app
    }

    fn push_log_lines(app: &mut App, n: usize) {
        let batch = (0..n)
            .map(|i| logs::LogLine {
                service: Some("cos".to_string()),
                ts:      None,
                text:    format!("line {i}"),
                notice:  false,
            })
            .collect();
        step(app, AppEvent::LogLines(batch), &TestSource);
    }

    #[test]
    fn dashboard_key_l_opens_logs_only_when_docker_was_detected() {
        // Gate ON: [l] opens the view.
        let mut app = logs_app(true);
        handle_dashboard_key(key('l'), &mut app, &TestSource);
        assert_eq!(app.view, View::Logs);

        // Gate OFF (bare agentd / QEMU / no compose project): the key is inert. Not merely
        // "renders an empty view" — it must not change the view at all, matching the legend
        // which omits it entirely.
        let mut app = logs_app(false);
        handle_dashboard_key(key('l'), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn step_log_lines_fills_the_ring_regardless_of_active_view() {
        // The tail keeps filling while the operator is elsewhere, so [l] opens on real
        // scrollback rather than an empty pane.
        let mut app = logs_app(true);
        app.view = View::Memory;
        push_log_lines(&mut app, 3);
        assert_eq!(app.logs_view.lines.len(), 3);
    }

    #[test]
    fn step_log_lines_dropped_accumulates_visible_drop_count() {
        let mut app = logs_app(true);
        app.view = View::Logs;
        let effects = step(&mut app, AppEvent::LogLinesDropped(12), &TestSource);
        assert_eq!(app.logs_view.dropped, 12);
        assert_eq!(effects, vec![Effect::Redraw]);
    }

    /// Log traffic must not repaint the whole TUI while the operator is looking at another
    /// view: the tail is spawned eagerly, so an unconditional Redraw turned a chatty compose
    /// project into a 33 Hz dashboard rebuild (and the slower drain then manufactured the very
    /// drops the header reports). Found by /review's red-team pass.
    #[test]
    fn log_events_do_not_repaint_while_another_view_is_on_screen() {
        let mut app = logs_app(true);
        assert_eq!(app.view, View::Dashboard);
        let batch = vec![logs::LogLine { text: "tick".into(), ..logs::LogLine::default() }];
        assert!(
            step(&mut app, AppEvent::LogLines(batch.clone()), &TestSource).is_empty(),
            "off-view log lines must not request a frame"
        );
        assert_eq!(app.logs_view.lines.len(), 1, "but the ring still accumulates");
        assert!(step(&mut app, AppEvent::LogLinesDropped(3), &TestSource).is_empty());
        assert_eq!(app.logs_view.dropped, 3, "and so does the drop counter");
        assert!(
            step(&mut app, AppEvent::ProducerDied(crate::watch::pump::LOGS_PRODUCER), &TestSource).is_empty()
        );
        assert_eq!(app.event_gaps, 0, "a dead log reader is not an event-feed gap");

        // On the Logs view the same events DO redraw.
        app.view = View::Logs;
        assert_eq!(
            step(&mut app, AppEvent::LogLines(batch), &TestSource),
            vec![Effect::Redraw]
        );
    }

    #[test]
    fn step_producer_died_for_logs_reports_in_the_logs_view_not_as_a_feed_gap() {
        // A dead log reader must not slander the authoritative snapshot/SSE feed.
        let mut app = logs_app(true);
        step(&mut app, AppEvent::ProducerDied("logs"), &TestSource);
        assert_eq!(app.event_gaps, 0, "the log tail is not the event feed");
        assert!(app.logs_view.lines.back().unwrap().notice);
    }

    #[test]
    fn logs_key_q_returns_to_dashboard_and_esc_does_too() {
        let mut app = logs_app(true);
        app.view = View::Logs;
        handle_logs_key(key('q'), &mut app);
        assert_eq!(app.view, View::Dashboard);

        app.view = View::Logs;
        handle_logs_key(kev(KeyCode::Esc), &mut app);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn logs_key_q_while_searching_types_instead_of_leaving_the_view() {
        // Same class of bug as the ux.1 chat-rail 'q' quit: a text field must swallow
        // letters that are shortcuts elsewhere.
        let mut app = logs_app(true);
        app.view = View::Logs;
        handle_logs_key(kev(KeyCode::Char('/')), &mut app);
        assert!(app.logs_view.search_active);
        handle_logs_key(key('q'), &mut app);
        assert_eq!(app.view, View::Logs);
        assert_eq!(app.logs_view.search_query.value(), "q");
    }

    #[test]
    fn logs_search_enter_commits_the_query_and_esc_clears_it() {
        let mut app = logs_app(true);
        app.view = View::Logs;
        app.logs_view.search_active = true;
        for c in "boom".chars() {
            handle_logs_key(key(c), &mut app);
        }
        handle_logs_key(kev(KeyCode::Enter), &mut app);
        assert!(!app.logs_view.search_active, "Enter closes the field");
        assert_eq!(app.logs_view.search_query.value(), "boom", "…but keeps the query");

        app.logs_view.search_active = true;
        handle_logs_key(kev(KeyCode::Esc), &mut app);
        assert!(!app.logs_view.search_active);
        assert_eq!(app.logs_view.search_query.value(), "");
        assert_eq!(app.view, View::Logs, "Esc cancels the search, it does not leave");
    }

    #[test]
    fn logs_key_tab_cycles_the_service_filter() {
        let mut app = logs_app(true);
        app.view = View::Logs;
        assert_eq!(app.logs_view.active_filter(), None);
        handle_logs_key(kev(KeyCode::Tab), &mut app);
        assert_eq!(app.logs_view.active_filter(), Some("cos"));
        handle_logs_key(kev(KeyCode::Tab), &mut app);
        assert_eq!(app.logs_view.active_filter(), Some("agent"));
        handle_logs_key(kev(KeyCode::Tab), &mut app);
        assert_eq!(app.logs_view.active_filter(), None);
    }

    #[test]
    fn logs_key_scroll_pauses_follow_and_g_shift_g_jump_to_the_ends() {
        let mut app = logs_app(true);
        app.view = View::Logs;
        app.term_size = (120, 24); // → logs_viewport_rows == 20
        push_log_lines(&mut app, 100);
        let rows = logs::logs_viewport_rows(app.term_size.1);
        assert!(app.logs_view.follow);

        handle_logs_key(kev(KeyCode::Up), &mut app);
        assert!(!app.logs_view.follow, "any upward scroll pauses follow");
        assert_eq!(app.logs_view.effective_scroll(rows), 79);

        handle_logs_key(key('g'), &mut app);
        assert_eq!(app.logs_view.effective_scroll(rows), 0);
        handle_logs_key(key('G'), &mut app);
        assert!(app.logs_view.follow, "[G] re-arms follow");
        assert_eq!(app.logs_view.effective_scroll(rows), 80);

        // PageUp moves a full viewport.
        handle_logs_key(kev(KeyCode::PageUp), &mut app);
        assert_eq!(app.logs_view.effective_scroll(rows), 60);
    }

    #[test]
    fn logs_key_t_toggles_the_timestamp_mode() {
        let mut app = logs_app(true);
        app.view = View::Logs;
        assert!(!app.logs_view.absolute_ts);
        handle_logs_key(key('t'), &mut app);
        assert!(app.logs_view.absolute_ts);
        handle_logs_key(key('t'), &mut app);
        assert!(!app.logs_view.absolute_ts);
    }

    /// A large paste must not be able to freeze the render loop. Per-char `InsertChar` was
    /// O(n²) and ran on the main thread, so Ctrl-C, `q` and even an external SIGTERM were all
    /// inert during it (raw mode suppresses the tty's SIGINT and the loop isn't polling).
    /// Found by /review's red-team pass.
    #[test]
    fn a_huge_paste_is_bounded_spliced_at_the_cursor_and_stripped_of_control_chars() {
        let mut app = logs_app(true);
        app.view = View::Logs;
        app.logs_view.search_active = true;
        let huge = "x".repeat(MAX_PASTE_CHARS * 4);
        let started = std::time::Instant::now();
        route_paste(&mut app, &huge);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "a big paste must stay linear and fast (took {:?})",
            started.elapsed()
        );
        assert_eq!(
            app.logs_view.search_query.value().chars().count(),
            MAX_PASTE_CHARS,
            "capped at ingest, not just clipped at render"
        );

        // Spliced AT THE CURSOR, and control characters never make it into a widget.
        let mut app = logs_app(true);
        app.view = View::Logs;
        app.logs_view.search_active = true;
        app.logs_view.search_query = tui_input::Input::new("ac".to_string()).with_cursor(1);
        route_paste(&mut app, "b\x1b\n");
        assert_eq!(app.logs_view.search_query.value(), "abc");
        assert_eq!(app.logs_view.search_query.cursor(), 2, "cursor follows the inserted text");
    }

    #[test]
    fn logs_paste_routes_into_the_search_field_only_while_searching() {
        let mut app = logs_app(true);
        app.view = View::Logs;
        route_paste(&mut app, "needle");
        assert_eq!(app.logs_view.search_query.value(), "", "no focused field → dropped");
        app.logs_view.search_active = true;
        route_paste(&mut app, "needle");
        assert_eq!(app.logs_view.search_query.value(), "needle");
    }


    // ── ux.13-TUI: row-action overlay (safety fixes before any verb is wired) ──

    #[test]
    fn dashboard_key_x_opens_the_overlay_pinned_to_the_selected_agent() {
        let mut app = app_with_agents(&["a", "b", "c"]);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &TestSource); // select "b"
        handle_dashboard_key(key('x'), &mut app, &TestSource);
        let ov = app.dashboard_overlay.as_ref().expect("overlay opened");
        assert_eq!(ov.target_id, "b");
        assert_eq!(ov.mode.kind(), "menu");
    }

    #[test]
    fn dashboard_key_x_is_a_noop_with_no_selection() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(key('x'), &mut app, &TestSource);
        assert!(app.dashboard_overlay.is_none(), "no row selected -> nothing to act on");
    }

    /// C1/E5: the pinned target must survive a snapshot that retargets the SELECTION. `apply_snapshot`
    /// clears a vanished selection and auto-selects row 0, from a producer thread — so without the pin
    /// the operator's confirm would land on whatever row 0 happens to be, and Cancel cascades.
    #[test]
    fn overlay_target_survives_a_snapshot_that_retargets_the_selection() {
        let mut app = app_with_agents(&["cos-coordinator", "scout-2"]);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &TestSource); // select scout-2
        handle_dashboard_key(key('x'), &mut app, &TestSource);
        assert_eq!(app.dashboard_overlay.as_ref().unwrap().target_id, "scout-2");

        // scout-2 finishes; the snapshot drops it and apply_snapshot auto-selects row 0.
        app.apply_snapshot(make_snapshot(&["cos-coordinator"]));
        assert_eq!(app.selected_id.as_deref(), Some("cos-coordinator"), "selection moved (expected)");
        let ov = app.dashboard_overlay.as_ref().unwrap();
        assert_eq!(ov.target_id, "scout-2", "but the overlay target did NOT move");
        assert!(
            ov.target(&app.agents).is_none(),
            "and it resolves to None rather than falling back to row 0"
        );
    }

    /// C3: `q` inside the overlay must dismiss, NOT quit. Driven through `step_key` on purpose — a
    /// `handle_dashboard_key`-level test cannot see the `Effect::Quit` push and would pass with the
    /// gate reverted.
    #[test]
    fn step_key_q_inside_the_overlay_dismisses_and_does_not_quit() {
        let mut app = app_with_agents(&["a"]);
        handle_dashboard_key(key('x'), &mut app, &TestSource);
        assert!(app.dashboard_overlay.is_some());

        let effects = step_key(&mut app, key('q'), &TestSource);
        assert!(
            !effects.contains(&Effect::Quit),
            "q must dismiss the overlay, not kill the cockpit mid-incident"
        );
        assert!(app.dashboard_overlay.is_none(), "and the overlay closed");
        assert_eq!(app.view, View::Dashboard);

        // The NEXT q, with no overlay, quits as before — the gate must not be sticky.
        assert!(step_key(&mut app, key('q'), &TestSource).contains(&Effect::Quit));
    }

    #[test]
    fn step_key_ctrl_c_still_quits_from_inside_the_overlay() {
        let mut app = app_with_agents(&["a"]);
        handle_dashboard_key(key('x'), &mut app, &TestSource);
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(step_key(&mut app, ev, &TestSource), vec![Effect::Quit]);
    }

    /// E4: the overlay owns the WHOLE keyboard. Intercepting only Enter/Esc/Tab/q would let these
    /// change `app.view` with the overlay still open, so the next key would land in another view's
    /// handler underneath a modal. Asserts BOTH `view` and `selected_id` — asserting only `view`
    /// passes while `j`/`k` still desyncs the highlight from the pinned target.
    #[test]
    fn overlay_swallows_view_switches_and_row_navigation() {
        let mut app = app_with_agents(&["a", "b", "c"]);
        // /review (testing specialist): `[l]` is inert in the base handler unless a compose project was
        // detected, so without this the 'l' row proved nothing about the overlay. And 'r' retargets the
        // CHAT RAIL, which none of the originally-asserted fields would have caught — a fall-through was
        // invisible. Both are now real rows.
        app.logs_view.available = true;
        let rail_target = app.converse_view.active_target.clone();
        handle_dashboard_key(key('x'), &mut app, &TestSource);
        let pinned = app.dashboard_overlay.as_ref().unwrap().target_id.clone();

        for k in ['s', 't', 'm', 'n', 'a', 'c', 'i', 'l', 'r', 'j', 'k'] {
            handle_dashboard_key(key(k), &mut app, &TestSource);
            assert_eq!(app.view, View::Dashboard, "'{k}' must not switch views from inside the overlay");
            assert_eq!(app.selected_id.as_deref(), Some("a"), "'{k}' must not move the selection");
            assert_eq!(
                app.dashboard_overlay.as_ref().map(|o| o.target_id.clone()),
                Some(pinned.clone()),
                "'{k}' must not change the pinned target"
            );
            assert_eq!(app.converse_view.active_target, rail_target,
                "'{k}' must not retarget the chat rail underneath the modal");
        }
        // Tab must not focus the chat rail underneath the modal either.
        handle_dashboard_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert!(!app.converse_view.rail_focused, "Tab must not reach the rail under a modal");
    }

    /// `x` while the rail has text focus must TYPE an x, not open an overlay — the rail captures
    /// printable keys, and this test fails if the overlay gate is ever placed before the rail's
    /// early return.
    #[test]
    fn x_while_rail_focused_types_instead_of_opening_the_overlay() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        handle_dashboard_key(key('x'), &mut app, &TestSource);
        assert!(app.dashboard_overlay.is_none(), "rail focus wins over the verb key");
        assert_eq!(app.converse_view.input.value(), "x");
    }

    #[test]
    fn overlay_menu_cursor_moves_and_saturates_at_both_ends() {
        let mut app = app_with_agents(&["a"]);
        handle_dashboard_key(key('x'), &mut app, &TestSource);
        let cursor = |app: &App| app.dashboard_overlay.as_ref().unwrap().cursor;
        assert_eq!(cursor(&app), 0);
        handle_dashboard_key(kev(KeyCode::Up), &mut app, &TestSource);
        assert_eq!(cursor(&app), 0, "saturates at the top");
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &TestSource);
        assert_eq!(cursor(&app), 1);
        // Past the last item: the cursor must clamp to the item COUNT, or Enter indexes past the end
        // of `menu_items` and the menu silently does nothing on its own bottom row.
        for _ in 0..10 {
            handle_dashboard_key(kev(KeyCode::Down), &mut app, &TestSource);
        }
        let last = overlay::menu_items(app.selected_agent().unwrap(), false).len() - 1;
        assert_eq!(cursor(&app), last, "must clamp to the last item, not run off the list");
    }

    // ── ux.13-TUI step 5: arming a verb, and the loop that performs it ────────────────

    /// Records what actually reached the `DataSource`, which is the only way to assert both halves of
    /// the two-phase split: nothing during key dispatch, exactly once from the loop.
    #[derive(Default)]
    struct VerbSource {
        cancels: std::sync::Mutex<Vec<String>>,
        budgets: std::sync::Mutex<Vec<(String, u64)>>,
        fail:    Option<String>,
    }
    impl VerbSource {
        fn failing(msg: &str) -> Self {
            Self { fail: Some(msg.to_string()), ..Default::default() }
        }
        fn calls(&self) -> usize {
            self.cancels.lock().unwrap().len() + self.budgets.lock().unwrap().len()
        }
    }
    impl DataSource for VerbSource {
        fn load_snapshot(&self) -> Snapshot {
            Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
        }
        fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
        fn approve(&self, _id: &str) -> Result<(), String> { Err("n/a".into()) }
        fn deny(&self, _id: &str, _r: Option<&str>) -> Result<(), String> { Err("n/a".into()) }
        fn cancel(&self, agent_id: &str) -> Result<u64, String> {
            self.cancels.lock().unwrap().push(agent_id.to_string());
            // 3 = the server's cascade count (target + 2 children), the number the client cannot know.
            match &self.fail { Some(e) => Err(e.clone()), None => Ok(3) }
        }
        /// Confirming, like HTTP: the past-tense/queued split is asserted separately, by a double that
        /// deliberately does NOT confirm.
        fn confirms_mutations(&self) -> bool { true }
        fn set_budget(&self, agent_id: &str, limit: u64) -> Result<(), String> {
            self.budgets.lock().unwrap().push((agent_id.to_string(), limit));
            match &self.fail { Some(e) => Err(e.clone()), None => Ok(()) }
        }
    }

    /// An agent list with real spend, so Park is available (`menu_items` disables it at zero).
    fn app_with_spend(ids: &[&str], spent: u64, budget: BudgetKind) -> App {
        let mut snap = make_snapshot(ids);
        for a in &mut snap.agents {
            a.windowed_spent = spent;
            a.budget = budget.clone();
        }
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(snap);
        app
    }

    fn mode_kind(app: &App) -> &'static str {
        app.dashboard_overlay.as_ref().expect("overlay open").mode.kind()
    }

    /// H1: the confirm keypress must NOT perform the call. `HttpSource`'s confirm client blocks up to
    /// 3 s, so a call made here freezes the cockpit with no frame drawn — the operator sees a dead
    /// terminal at the exact moment they are stopping a runaway.
    #[test]
    fn park_arms_the_verb_and_the_keypress_calls_nothing() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        handle_dashboard_key(key('x'), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src); // cursor 0 = Park

        assert_eq!(src.calls(), 0, "the key handler must never touch the DataSource");
        assert_eq!(mode_kind(&app), "in_flight", "the operator must see a frame saying so");
        assert_eq!(
            app.pending_verb,
            Some(overlay::PendingVerb::SetBudget {
                agent_id: "scout-2".to_string(), limit: 47_000, park: true,
            }),
            "Park must arm set_budget at the RECORDED spend, never 0"
        );
    }

    /// E1 again, at the level the operator meets it: with no recorded spend the item is inert.
    /// `set_budget(0)` would mean UNLIMITED and would be checkpointed — a permanent un-cap.
    #[test]
    fn park_is_inert_at_zero_spend() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 0, BudgetKind::Unlimited);
        handle_dashboard_key(key('x'), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        assert!(app.pending_verb.is_none(), "a blocked item must arm nothing");
        assert_eq!(mode_kind(&app), "menu", "and must not leave the menu");
        assert_eq!(src.calls(), 0);
    }

    /// The irreversible verb is two gates deep, and the first gate arms nothing.
    #[test]
    fn cancel_takes_two_confirmations_and_the_first_arms_nothing() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        handle_dashboard_key(key('x'), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &src); // → Cancel
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        assert_eq!(mode_kind(&app), "confirm_cancel");
        assert!(app.pending_verb.is_none(), "the menu must not arm the irreversible verb directly");

        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        assert_eq!(
            app.pending_verb,
            Some(overlay::PendingVerb::Cancel { agent_id: "scout-2".to_string() })
        );
        assert_eq!(src.calls(), 0, "still not from the key handler");
    }

    /// Esc backs out of the confirm to the menu, on the row it came from.
    #[test]
    fn esc_backs_out_of_each_gate_preserving_the_menu_row() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Tokens(200_000));
        handle_dashboard_key(key('x'), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &src); // → Set budget
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        assert_eq!(mode_kind(&app), "budget");
        handle_dashboard_key(kev(KeyCode::Esc), &mut app, &src);
        assert_eq!(mode_kind(&app), "menu");
        assert_eq!(app.dashboard_overlay.as_ref().unwrap().cursor, 1,
            "Esc must return to the row the operator opened, not to the top");
        assert!(app.pending_verb.is_none());
    }

    /// E8, the drain-once property. A single-iteration test passes with a re-arming bug: the slot is
    /// `.take()`n BEFORE the call precisely so an early return cannot re-fire the same verb every
    /// 30 ms — a cancel storm against the scheduler.
    #[test]
    fn drain_performs_the_verb_exactly_once_across_repeated_ticks() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        handle_dashboard_key(key('x'), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src); // armed

        let first = drain_pending_verb(&mut app, &src);
        assert_eq!(first, vec![Effect::Redraw, Effect::Reconcile]);
        for _ in 0..3 {
            assert!(drain_pending_verb(&mut app, &src).is_empty(), "an empty slot must be a no-op");
        }
        assert_eq!(src.cancels.lock().unwrap().as_slice(), ["scout-2"],
            "exactly one cancel must reach the scheduler");
        assert_eq!(mode_kind(&app), "result");
    }

    /// The success copy is the CLI's tense, not "cancelled": the scheduler acts at the next step
    /// boundary, and `agentctl cancel` already says so. Two vocabularies for one write is how the
    /// operator ends up trusting neither (DX finding).
    #[test]
    fn drain_reports_requested_not_done_and_names_the_cli_equivalent() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        app.dashboard_overlay = Some(overlay::DashboardOverlay::menu("scout-2"));
        app.pending_verb = Some(overlay::PendingVerb::Cancel { agent_id: "scout-2".into() });
        drain_pending_verb(&mut app, &src);
        let overlay::OverlayMode::Result { text, ok } = &app.dashboard_overlay.as_ref().unwrap().mode
        else { panic!("expected a Result frame, got {}", mode_kind(&app)) };
        assert!(*ok);
        assert!(text.contains("Cancel requested"), "must not claim the agent is already stopped: {text}");
        assert!(text.contains("next step boundary"), "must say when it takes effect: {text}");
        // E6: the SERVER's count (3 here), which the client cannot compute — the route has always
        // returned it and `HttpSource::cancel` used to throw the body away.
        assert!(text.contains("3 agents flagged"), "must report the server's cascade count: {text}");
        assert!(text.contains("agentctl cancel scout-2"), "must name the CLI equivalent: {text}");
    }

    /// A CONFIRMING source whose reply carries no `count` (an older agentd, or a body that fails to
    /// parse) must make the copy say nothing about how many agents were hit, rather than printing a
    /// guessed "0 agents".
    #[test]
    fn drain_omits_the_cascade_count_when_the_source_cannot_report_one() {
        struct NoCountSource;
        impl DataSource for NoCountSource {
            fn confirms_mutations(&self) -> bool { true }
            fn load_snapshot(&self) -> Snapshot {
                Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
            }
            fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
            fn approve(&self, _id: &str) -> Result<(), String> { Err("n/a".into()) }
            fn deny(&self, _id: &str, _r: Option<&str>) -> Result<(), String> { Err("n/a".into()) }
            fn cancel(&self, _id: &str) -> Result<u64, String> { Ok(0) }
        }
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        app.dashboard_overlay = Some(overlay::DashboardOverlay::menu("scout-2"));
        app.pending_verb = Some(overlay::PendingVerb::Cancel { agent_id: "scout-2".into() });
        drain_pending_verb(&mut app, &NoCountSource);
        let overlay::OverlayMode::Result { text, ok } = &app.dashboard_overlay.as_ref().unwrap().mode
        else { panic!("expected a Result frame") };
        assert!(*ok);
        assert!(!text.contains("0 agent"), "must not report a count it does not have: {text}");
        assert!(text.contains("Cancel requested"), "{text}");
    }

    /// C5: over FUSE, `Ok(())` means "the command was queued", nothing more — "agent not found" and
    /// "SetCaps is narrow-only" arrive as `Ok(())` too. So the result copy must not use past tense, and
    /// must point at where the scheduler's real verdict shows up. Asserted on a double that inherits
    /// the `false` default, which is exactly what a FUSE source does.
    #[test]
    fn drain_says_queued_not_accepted_on_a_source_that_cannot_confirm() {
        struct FuseLikeVerbSource;
        impl DataSource for FuseLikeVerbSource {
            fn load_snapshot(&self) -> Snapshot {
                Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
            }
            fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
            fn approve(&self, _id: &str) -> Result<(), String> { Err("n/a".into()) }
            fn deny(&self, _id: &str, _r: Option<&str>) -> Result<(), String> { Err("n/a".into()) }
            fn cancel(&self, _id: &str) -> Result<u64, String> { Ok(0) }
        }
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        app.dashboard_overlay = Some(overlay::DashboardOverlay::menu("scout-2"));
        app.pending_verb = Some(overlay::PendingVerb::Cancel { agent_id: "scout-2".into() });
        drain_pending_verb(&mut app, &FuseLikeVerbSource);
        let overlay::OverlayMode::Result { text, .. } = &app.dashboard_overlay.as_ref().unwrap().mode
        else { panic!("expected a Result frame") };
        assert!(text.contains("queued over FUSE"), "must say what actually happened: {text}");
        assert!(text.contains("cannot confirm"), "{text}");
        assert!(text.contains("fuse_control_error"), "must point at the real verdict: {text}");
        assert!(!text.contains("Cancel requested for"),
            "must not borrow the confirming source's copy: {text}");
    }

    /// `?` was bound by NO key anywhere before this increment — which was the CEO phase's own argument
    /// for striking the `:` palette ("the keys are on screen") coming due.
    #[test]
    fn question_mark_opens_the_help_overlay_and_closes_on_itself() {
        let mut app = app_with_agents(&["a"]);
        handle_dashboard_key(key('?'), &mut app, &TestSource);
        assert_eq!(mode_kind(&app), "help");
        // Pressing it again closes: a modal you open with `?` and cannot close with `?` is a trap.
        handle_dashboard_key(key('?'), &mut app, &TestSource);
        assert!(app.dashboard_overlay.is_none());
    }

    /// Help owns the keyboard like every other overlay mode — E4's rule is not per-mode.
    #[test]
    fn the_help_overlay_swallows_view_switches() {
        let mut app = app_with_agents(&["a", "b"]);
        app.logs_view.available = true; // else the 'l' row is vacuous (/review)
        let rail_target = app.converse_view.active_target.clone();
        handle_dashboard_key(key('?'), &mut app, &TestSource);
        for k in ['s', 't', 'm', 'n', 'a', 'c', 'i', 'l', 'j', 'k', 'r', 'x'] {
            handle_dashboard_key(key(k), &mut app, &TestSource);
            assert_eq!(app.view, View::Dashboard, "'{k}' must not switch views under the help modal");
            assert_eq!(app.selected_id.as_deref(), Some("a"), "'{k}' must not move the selection");
            assert_eq!(app.converse_view.active_target, rail_target, "'{k}' must not retarget the rail");
            assert_eq!(mode_kind(&app), "help", "'{k}' must not replace the help modal (incl. 'x')");
        }
        handle_dashboard_key(kev(KeyCode::Tab), &mut app, &TestSource);
        assert!(!app.converse_view.rail_focused, "Tab must not reach the rail under the help modal");
    }

    /// `?` while the chat rail has text focus must TYPE it, not open help — the rail-capture rule
    /// (ux.1's bug class) applies to every new printable key.
    #[test]
    fn question_mark_while_the_rail_is_focused_types_instead_of_opening_help() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        handle_dashboard_key(key('?'), &mut app, &TestSource);
        assert!(app.dashboard_overlay.is_none());
        assert_eq!(app.converse_view.input.value(), "?");
    }

    /// `q` must dismiss help, not quit the cockpit. Through `step_key`, because
    /// `handle_dashboard_key` never pushes `Effect::Quit`.
    #[test]
    fn step_key_q_inside_help_dismisses_without_quitting() {
        let mut app = app_with_agents(&["a"]);
        handle_dashboard_key(key('?'), &mut app, &TestSource);
        let effects = step_key(&mut app, key('q'), &TestSource);
        assert!(!effects.contains(&Effect::Quit), "q inside help must not kill the cockpit");
        assert!(app.dashboard_overlay.is_none(), "but it must dismiss");
    }

    // ── M8: the row has to SAY a cancel is in flight ──────────────────────────────────

    /// There is no `AgentStatus::Cancelling` and `cancel_requested` is scheduler-private, so without a
    /// client-side marker a successfully-cancelled row reads `running` for a whole turn and then
    /// vanishes — indistinguishable from a keypress that did nothing.
    #[test]
    fn a_confirmed_cancel_marks_the_row_and_its_subtree() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["cos-coordinator", "scout-2"], 47_000, BudgetKind::Unlimited);
        // scout-2 is a child, so the cascade covers it too.
        app.agents[1].parent_id = Some("cos-coordinator".to_string());
        app.topology = crate::watch::topology::build_graph(&app.agents, None);
        app.dashboard_overlay = Some(overlay::DashboardOverlay::menu("cos-coordinator"));
        app.pending_verb = Some(overlay::PendingVerb::Cancel { agent_id: "cos-coordinator".into() });

        drain_pending_verb(&mut app, &src);

        assert_eq!(app.cancel_marker("cos-coordinator"), Some(CancelMarker::InFlight));
        assert_eq!(app.cancel_marker("scout-2"), Some(CancelMarker::InFlight),
            "Cancel cascades, so the child row must not keep reading 'running' either");
    }

    #[test]
    fn a_failed_cancel_marks_nothing() {
        let src = VerbSource::failing("HTTP 503: busy");
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        app.dashboard_overlay = Some(overlay::DashboardOverlay::menu("scout-2"));
        app.pending_verb = Some(overlay::PendingVerb::Cancel { agent_id: "scout-2".into() });
        drain_pending_verb(&mut app, &src);
        assert_eq!(app.cancel_marker("scout-2"), None,
            "a cancel that never reached the scheduler must not show as in flight");
    }

    /// The scheduler's own confirmation, which can arrive a whole poll interval before the row leaves
    /// the snapshot.
    #[test]
    fn the_agent_cancelled_event_marks_the_row_as_cancelled_by_the_operator() {
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        app.mark_cancel_requested("scout-2", true);
        assert!(app.cancel_marker("scout-2").is_some());

        // An unrelated event must not clear it.
        step(&mut app, AppEvent::Flight(serde_json::json!({
            "agent": "scout-2", "kind": "agent_step", "data": {}
        })), &TestSource);
        assert!(app.cancel_marker("scout-2").is_some(), "only agent_cancelled clears the marker");

        // …and one for a DIFFERENT agent must not clear this one.
        step(&mut app, AppEvent::Flight(serde_json::json!({
            "agent": "other", "kind": "agent_cancelled", "data": {}
        })), &TestSource);
        assert!(app.cancel_marker("scout-2").is_some(), "must match on the agent id");

        step(&mut app, AppEvent::Flight(serde_json::json!({
            "agent": "scout-2", "kind": "agent_cancelled", "data": {}
        })), &TestSource);
        // /qa (real agentd): NOT cleared — a cancelled agent's row reads `failed`, so dropping the marker
        // here left the operator staring at a red failure with nothing saying it was their own stop.
        assert_eq!(app.cancel_marker("scout-2"), Some(CancelMarker::Landed),
            "the confirmation must ATTRIBUTE the failure, not vanish");
    }

    // ── DX: the five error strings ────────────────────────────────────────────────────

    /// Every one of these replaced a string that failed WHAT / WHY / WHAT-TO-DO. The 401/403 case is the
    /// one that cannot be discovered any other way: there is no unauthenticated route that reveals
    /// whether agentd has an approval secret (E10 killed the startup probe), so the response is the only
    /// teacher the operator gets.
    #[test]
    fn verb_errors_are_rewritten_into_what_why_and_what_to_do() {
        let auth = explain_verb_error("HTTP 401");
        assert!(auth.contains("approval token"), "{auth}");
        assert!(auth.contains("AGENTOS_APPROVAL_SECRET"), "must name the env var: {auth}");
        assert!(auth.contains("run this again"), "the CLI's own next step must be there: {auth}");
        assert!(auth.contains("restart `agentctl watch`"),
            "and the TUI's, which differs because it reads the secret once at startup: {auth}");
        // Surface-neutral: this copy is now shared with the CLI, so it must not assume an overlay
        // (/qa found "dismiss and check the table" printed by `agentctl cancel`).
        for msg in [&auth, &explain_verb_error("HTTP 404"), &explain_verb_error("HTTP 503: busy")] {
            assert!(!msg.contains("dismiss"), "TUI-only vocabulary leaked into shared copy: {msg}");
        }
        assert_eq!(explain_verb_error("HTTP 403: forbidden"), auth, "403 is the same problem");

        let busy = explain_verb_error("HTTP 503: control channel full");
        assert!(busy.contains("Wait a second and retry"), "503 is retryable and must say so: {busy}");
        assert!(busy.contains("control channel full"), "must keep the server's own detail: {busy}");

        let gone = explain_verb_error("HTTP 404: no such agent");
        assert!(gone.contains("may have already finished"), "{gone}");
        assert!(gone.contains("check the agent list"), "{gone}");

        // Anything unrecognised passes through verbatim: inventing an explanation for an error nobody
        // has seen is how a cockpit teaches the wrong fix.
        assert_eq!(explain_verb_error("kernel exploded"), "kernel exploded");
    }

    /// The failure the pty drive exposed: `reqwest::Error`'s Display drops the cause chain, so the
    /// confirm client's 3 s timeout reached the operator as "HTTP error: error sending request for url
    /// (…)" — no mention of a timeout, and no hint that the mutation may STILL land.
    #[test]
    fn a_transport_timeout_says_it_timed_out_and_that_the_write_may_still_apply() {
        use crate::watch::source::describe_send_error;
        // A real reqwest timeout, produced rather than mocked, because `is_timeout()` is the thing under
        // test. The holder thread is JOINED (it used to outlive the test by 3 s) and the connect half no
        // longer depends on an ephemeral port staying free — /review flagged both as flake sources.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().take(1).filter_map(Result::ok).collect();
            let _ = release_rx.recv(); // hold the connection open, replying nothing, until released
            drop(held);
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(150))
            .build()
            .unwrap();
        let err = client.post(format!("http://{addr}/api/v1/agents/a/cancel")).send()
            .expect_err("must time out");
        let msg = describe_send_error(&err);
        let _ = release_tx.send(());
        server.join().expect("holder thread");
        assert!(msg.contains("Timed out"), "{msg}");
        assert!(msg.contains("may still have been applied"),
            "a delivered-but-unanswered mutation is NOT known to have failed: {msg}");

        // The opposite case — nothing was sent — must not say "may still". Port 1 is privileged and
        // never listening, so this needs no port bookkeeping.
        let err = client.post("http://127.0.0.1:1/api/v1/agents/a/cancel").send()
            .expect_err("must fail to connect");
        let msg = describe_send_error(&err);
        assert!(msg.contains("nothing was sent"), "{msg}");
        assert!(msg.contains("--url"), "must point at the likely misconfiguration: {msg}");
    }


    // ── /review findings: the budget verb's own drain, and the two fail-closed gates ───

    /// E1's LAST MILE, and the gap /review's testing specialist caught: every drain test used Cancel, so
    /// replacing `source.set_budget(agent_id, *limit)` with `set_budget(agent_id, 0)` — the exact un-cap
    /// footgun this increment exists to prevent, since 0 means UNLIMITED and is CHECKPOINTED — passed the
    /// entire suite. This asserts the number that actually reaches the wire.
    #[test]
    fn drain_sends_the_parked_limit_not_zero_and_marks_no_cancel() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        handle_dashboard_key(key('x'), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src); // Park
        drain_pending_verb(&mut app, &src);

        assert_eq!(src.budgets.lock().unwrap().as_slice(), [("scout-2".to_string(), 47_000)],
            "0 here would mean UNLIMITED and would be written to the checkpoint");
        assert!(src.cancels.lock().unwrap().is_empty(), "Park is a set_budget, not a cancel");
        assert_eq!(app.cancel_marker("scout-2"), None,
            "and a budget verb must not borrow the cancel marker");
    }

    /// The typed-budget path to the wire, same reason.
    #[test]
    fn drain_sends_the_typed_budget_limit() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Tokens(200_000));
        open_budget_field(&mut app, &src);
        set_mode(&mut app, overlay::OverlayMode::Budget {
            input: tui_input::Input::new("100000".to_string()), error: None,
        });
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        drain_pending_verb(&mut app, &src);
        assert_eq!(src.budgets.lock().unwrap().as_slice(), [("scout-2".to_string(), 100_000)]);
    }

    /// The Park RESULT copy must not contradict the menu the operator just used. Before /review it
    /// hardcoded "raise the limit to revive it" in BOTH deployments — so on the config default, where the
    /// menu correctly says the park ENDS the agent, the very next frame promised a revival.
    #[test]
    fn the_park_result_copy_matches_the_deployment_it_ran_against() {
        for (resettable, must, must_not) in [
            (true,  "resumes by itself", "ENDS the agent"),
            (false, "ENDS the agent",    "resumes by itself"),
        ] {
            let src = VerbSource::default();
            let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
            app.budget = Some(reader::SysBudget { spent: 47_000, total: 0, resettable });
            handle_dashboard_key(key('x'), &mut app, &src);
            handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
            drain_pending_verb(&mut app, &src);
            let overlay::OverlayMode::Result { text, .. } =
                &app.dashboard_overlay.as_ref().unwrap().mode else { panic!("expected Result") };
            assert!(text.contains(must), "resettable={resettable}: {text}");
            assert!(!text.contains(must_not), "resettable={resettable}: {text}");
        }
    }

    /// /review (security specialist): the confirm gates used to write against a pin the snapshot had
    /// already dropped, while the SAME box rendered "No action sent … no longer in the snapshot". Agent
    /// ids are reused here — CoS agents have fixed config ids and cron respawns them — so that write
    /// could land on a fresh agent of the same name. Both gates now fail closed.
    #[test]
    fn a_confirm_gate_sends_nothing_once_the_pinned_target_is_gone() {
        for gate in ["cancel", "budget"] {
            let src = VerbSource::default();
            let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Tokens(200_000));
            handle_dashboard_key(key('x'), &mut app, &src);
            // Reach the gate while the target still exists…
            if gate == "cancel" {
                handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
                handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
                handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
                assert_eq!(mode_kind(&app), "confirm_cancel");
            } else {
                handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
                handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
                set_mode(&mut app, overlay::OverlayMode::Budget {
                    input: tui_input::Input::new("0".to_string()), error: None,
                });
                handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
                assert_eq!(mode_kind(&app), "confirm_budget");
            }
            // …then it finishes, through the real snapshot fold, and a DIFFERENT agent takes row 0.
            app.apply_snapshot(make_snapshot(&["cos-coordinator"]));

            handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
            assert!(app.pending_verb.is_none(), "{gate}: nothing may be armed against a vanished pin");
            drain_pending_verb(&mut app, &src);
            assert_eq!(src.calls(), 0, "{gate}: and nothing may reach the scheduler");
            let overlay::OverlayMode::Result { text, ok } =
                &app.dashboard_overlay.as_ref().unwrap().mode
            else { panic!("{gate}: expected the refusal to be REPORTED, not silent") };
            assert!(!*ok);
            assert!(text.contains("No action sent"), "{gate}: {text}");
        }
    }

    /// Below the box floor the render collapses to one line that cannot show a menu, a confirm, or a
    /// result — so no verb may be armed there. /review (maintainability) found the handler running the
    /// full state machine under that line: Enter armed Park (a checkpointed `set_budget`) invisibly.
    #[test]
    fn no_verb_can_be_armed_on_a_terminal_too_small_to_show_the_overlay() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Tokens(200_000));
        app.term_size = (40, 8); // content area is far below the 34x7 box floor
        assert!(!crate::watch::views::overlay_fits_dashboard(app.term_size), "precondition");
        handle_dashboard_key(key('x'), &mut app, &src);
        assert!(app.dashboard_overlay.is_some(), "the overlay still OPENS — it degrades, it is not absent");

        for k in [KeyCode::Enter, KeyCode::Down, KeyCode::Enter, KeyCode::Char('y'), KeyCode::Enter] {
            handle_dashboard_key(kev(k), &mut app, &src);
            assert!(app.pending_verb.is_none(), "{k:?} must not arm a verb the operator cannot see");
        }
        assert_eq!(src.calls(), 0);
        assert_eq!(mode_kind(&app), "menu", "and the mode must not advance under the degraded line");

        // Esc still works — a modal with no visible exit is the other half of the bug.
        handle_dashboard_key(kev(KeyCode::Esc), &mut app, &src);
        assert!(app.dashboard_overlay.is_none());
    }

    /// Both destructive aliases, plus the negative control that `y` is inert in the LANDING state — the
    /// assertion that catches a future reorder putting the irreversible verb one keypress from the menu.
    #[test]
    fn y_is_inert_in_the_menu_and_only_confirms_from_a_gate() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        handle_dashboard_key(key('x'), &mut app, &src);
        handle_dashboard_key(key('y'), &mut app, &src);
        assert_eq!(mode_kind(&app), "menu", "y must not be a shortcut from the landing state");
        assert!(app.pending_verb.is_none());

        handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        handle_dashboard_key(key('q'), &mut app, &src);
        assert_eq!(mode_kind(&app), "menu", "q backs out of the gate rather than dismissing");
        assert_eq!(app.dashboard_overlay.as_ref().unwrap().cursor, 2, "on the row it came from");

        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        handle_dashboard_key(key('y'), &mut app, &src);
        assert_eq!(app.pending_verb,
            Some(overlay::PendingVerb::Cancel { agent_id: "scout-2".to_string() }),
            "y confirms from the gate — the alias every other test reached via Enter");
    }

    /// The budget field driven through REAL keystrokes. Every other budget test injects the value with
    /// `set_mode`, so the clone-edit-writeback arm — the only way an operator gets a number in — had no
    /// coverage at all, including whether the prefill can be cleared.
    #[test]
    fn typing_edits_the_budget_field_through_the_real_key_path() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Tokens(200_000));
        open_budget_field(&mut app, &src);
        assert_eq!(budget_input(&app), "200000");

        // The prefill MUST be clearable, or prefilling is a trap: a naive operator typing 5000 would
        // submit 2000005000.
        handle_dashboard_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), &mut app, &src);
        assert_eq!(budget_input(&app), "", "Ctrl-U must clear the prefilled limit");
        for c in ['5', '0', '0', '0'] {
            handle_dashboard_key(key(c), &mut app, &src);
        }
        assert_eq!(budget_input(&app), "5000", "digits must reach the widget");
        handle_dashboard_key(kev(KeyCode::Backspace), &mut app, &src);
        assert_eq!(budget_input(&app), "500");

        // A bad submit leaves an error; the next keystroke clears it rather than leaving stale red text.
        set_mode(&mut app, overlay::OverlayMode::Budget {
            input: tui_input::Input::new("x".to_string()), error: None,
        });
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        match &app.dashboard_overlay.as_ref().unwrap().mode {
            overlay::OverlayMode::Budget { error, .. } => assert!(error.is_some()),
            other => panic!("expected the field, got {}", other.kind()),
        }
        handle_dashboard_key(kev(KeyCode::Backspace), &mut app, &src);
        match &app.dashboard_overlay.as_ref().unwrap().mode {
            overlay::OverlayMode::Budget { error, input } => {
                assert!(error.is_none(), "editing must clear the stale error");
                assert_eq!(input.value(), "");
            }
            other => panic!("expected the field, got {}", other.kind()),
        }
    }

    /// /review (security): the `[d]` message must describe what the SOURCE did. HTTP has no
    /// auto-approve-kind route and inherits a plain approve, so the old copy told the operator a
    /// standing policy existed when nothing was registered — on the approval gate, the one real
    /// authority boundary here. Both real impls, and both messages.
    #[test]
    fn dont_ask_again_only_claims_a_standing_rule_where_one_is_registered() {
        use crate::watch::source::FuseSource;
        assert!(FuseSource { agents_dir: PathBuf::from("/agents") }.supports_auto_approve_kind(),
            "the FUSE control command carries auto_approve_kind");
        assert!(!HttpSource::new("http://127.0.0.1:7999".to_string()).supports_auto_approve_kind(),
            "there is no HTTP route for it — it degrades to a plain approve");

        // A double that approves successfully but does NOT support the standing rule (i.e. HTTP-shaped).
        #[derive(Default)]
        struct PlainApproveSource(std::sync::Mutex<Vec<String>>);
        impl DataSource for PlainApproveSource {
            fn load_snapshot(&self) -> Snapshot {
                Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
            }
            fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
            fn approve(&self, id: &str) -> Result<(), String> {
                self.0.lock().unwrap().push(id.to_string());
                Ok(())
            }
            fn deny(&self, _id: &str, _r: Option<&str>) -> Result<(), String> { Err("n/a".into()) }
        }
        let src = PlainApproveSource::default();
        let mut app = app_with_approvals(&[("act_1", "shell_exec")]);
        app.approvals_view.mode         = ApprovalsMode::Confirm;
        app.approvals_view.confirmed_id = Some("act_1".to_string());
        handle_approvals_key(kev(KeyCode::Char('d')), &mut app, &src);

        let msg = app.approvals_view.result_msg.as_deref().unwrap();
        assert!(msg.contains("Approved act_1"), "the approval itself DID happen: {msg}");
        assert!(msg.contains("FUSE-only"), "and the operator must be told the rule was not registered: {msg}");
        assert!(!msg.contains("(auto for"), "must not claim a standing policy: {msg}");
    }

    /// Both REAL implementations, not one double: a single double inherits the `false` default and
    /// passes with the whole gate reverted (eng finding E9).
    #[test]
    fn only_the_http_source_claims_to_confirm_mutations() {
        use crate::watch::source::FuseSource;
        assert!(HttpSource::new("http://127.0.0.1:7999".to_string()).confirms_mutations(),
            "HTTP holds the connection until the scheduler answers");
        assert!(!FuseSource { agents_dir: PathBuf::from("/agents") }.confirms_mutations(),
            "FUSE learns only that close(2) succeeded — the scheduler's verdict goes to the flight log");
    }

    #[test]
    fn drain_surfaces_a_failure_verbatim_and_holds_the_overlay_open() {
        let src = VerbSource::failing("HTTP 503: control channel busy");
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        app.dashboard_overlay = Some(overlay::DashboardOverlay::menu("scout-2"));
        app.pending_verb = Some(overlay::PendingVerb::Cancel { agent_id: "scout-2".into() });
        drain_pending_verb(&mut app, &src);
        let overlay::OverlayMode::Result { text, ok } = &app.dashboard_overlay.as_ref().unwrap().mode
        else { panic!("expected a Result frame") };
        assert!(!*ok, "a failed verb must not render as success");
        assert!(text.contains("503"), "the operator needs the real error: {text}");
        assert!(app.dashboard_overlay.is_some(), "the failure must stay on screen until dismissed");
    }

    /// C1, the retarget hazard, driven through a REAL snapshot fold: `apply_snapshot` runs from a
    /// producer thread every ~30 ms and moves `selected_id`. A test that never folds a snapshot passes
    /// with the pinning removed, because the selection never changes.
    #[test]
    fn a_snapshot_that_moves_the_selection_cannot_move_the_verb_target() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["cos-coordinator", "scout-2"], 47_000, BudgetKind::Unlimited);
        app.selected_id = Some("scout-2".to_string());
        handle_dashboard_key(key('x'), &mut app, &src);

        // A snapshot lands in which scout-2 is gone: `apply_snapshot` clears the dead selection and
        // auto-selects row 0 — the coordinator, whose cancel would cascade to its whole subtree.
        let mut snap = make_snapshot(&["cos-coordinator"]);
        snap.agents[0].windowed_spent = 47_000;
        // A real budget on the survivor, deliberately: it makes every menu item on the COORDINATOR
        // armable, so if this handler ever read the live selection instead of the pin, the keypresses
        // below would succeed in acting on it. With `Unlimited` here the mutation is masked by a
        // prefill parse error — verified by running that negative control.
        snap.agents[0].budget = BudgetKind::Tokens(200_000);
        app.apply_snapshot(snap);
        assert_eq!(app.selected_id.as_deref(), Some("cos-coordinator"), "precondition: it retargeted");
        assert_eq!(app.dashboard_overlay.as_ref().unwrap().target_id, "scout-2",
            "the pin must not follow the selection");

        // Everything the operator could press next must refuse to act on the coordinator.
        for _ in 0..4 {
            handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
            handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        }
        assert!(app.pending_verb.is_none(), "a vanished target must arm nothing");
        drain_pending_verb(&mut app, &src);
        assert_eq!(src.calls(), 0, "and must never reach the coordinator");
    }

    /// The same hazard with the target still ALIVE: the selection moved, the verb must not.
    #[test]
    fn the_armed_verb_carries_the_pinned_id_not_the_current_selection() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["cos-coordinator", "scout-2"], 47_000, BudgetKind::Unlimited);
        app.selected_id = Some("scout-2".to_string());
        handle_dashboard_key(key('x'), &mut app, &src);
        // The selection drifts while the overlay is up (what apply_snapshot's row-0 auto-select does).
        app.selected_id = Some("cos-coordinator".to_string());
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Down), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        assert_eq!(
            app.pending_verb,
            Some(overlay::PendingVerb::Cancel { agent_id: "scout-2".to_string() }),
            "the confirm must act on the pinned agent, not whatever row is highlighted now"
        );
    }

    // ── the budget field (M2) ─────────────────────────────────────────────────────────

    fn open_budget_field(app: &mut App, src: &VerbSource) {
        handle_dashboard_key(key('x'), app, src);
        handle_dashboard_key(kev(KeyCode::Down), app, src); // → Set budget
        handle_dashboard_key(kev(KeyCode::Enter), app, src);
    }

    fn budget_input(app: &App) -> String {
        match &app.dashboard_overlay.as_ref().unwrap().mode {
            overlay::OverlayMode::Budget { input, .. } => input.value().to_string(),
            other => panic!("expected the budget field, got {}", other.kind()),
        }
    }

    #[test]
    fn budget_field_opens_on_the_current_limit() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Tokens(200_000));
        open_budget_field(&mut app, &src);
        assert_eq!(budget_input(&app), "200000",
            "an empty field plus Enter would submit 'unlimited' — the inverse of the intent (M2)");
    }

    #[test]
    fn tightening_the_budget_arms_without_a_second_gate() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Tokens(200_000));
        open_budget_field(&mut app, &src);
        set_mode(&mut app, overlay::OverlayMode::Budget {
            input: tui_input::Input::new("100000".to_string()), error: None,
        });
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        assert_eq!(mode_kind(&app), "in_flight", "a tightening needs no extra ceremony");
        assert_eq!(
            app.pending_verb,
            Some(overlay::PendingVerb::SetBudget {
                agent_id: "scout-2".into(), limit: 100_000, park: false,
            })
        );
    }

    /// M2: `0` REMOVES the cap. It must never be one keypress from a field that opens prefilled.
    #[test]
    fn zero_and_raises_go_through_a_second_gate() {
        for (typed, current) in [("0", BudgetKind::Tokens(200_000)), ("300000", BudgetKind::Tokens(200_000))] {
            let src = VerbSource::default();
            let mut app = app_with_spend(&["scout-2"], 47_000, current);
            open_budget_field(&mut app, &src);
            set_mode(&mut app, overlay::OverlayMode::Budget {
                input: tui_input::Input::new(typed.to_string()), error: None,
            });
            handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
            assert_eq!(mode_kind(&app), "confirm_budget", "'{typed}' must be gated");
            assert!(app.pending_verb.is_none(), "'{typed}' must not arm on the first Enter");

            // Declining returns to the FIELD with the number intact, so it can be corrected.
            handle_dashboard_key(kev(KeyCode::Esc), &mut app, &src);
            assert_eq!(budget_input(&app), typed);

            handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src); // gate again
            handle_dashboard_key(kev(KeyCode::Char('y')), &mut app, &src);
            assert_eq!(mode_kind(&app), "in_flight");
            assert_eq!(app.pending_verb.as_ref().map(|v| matches!(
                v, overlay::PendingVerb::SetBudget { limit, park: false, .. } if *limit == typed.parse::<u64>().unwrap()
            )), Some(true));
        }
    }

    #[test]
    fn a_non_numeric_budget_is_rejected_in_place_not_silently_zeroed() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Tokens(200_000));
        open_budget_field(&mut app, &src);
        set_mode(&mut app, overlay::OverlayMode::Budget {
            input: tui_input::Input::new("20o000".to_string()), error: None,
        });
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src);
        match &app.dashboard_overlay.as_ref().unwrap().mode {
            overlay::OverlayMode::Budget { error, input } => {
                assert!(error.is_some(), "a typo must be reported, never parsed as 0 (= unlimited)");
                assert_eq!(input.value(), "20o000", "and the operator's text must survive to be fixed");
            }
            other => panic!("must stay in the field, got {}", other.kind()),
        }
        assert!(app.pending_verb.is_none());
    }

    #[test]
    fn budget_accepts_human_digit_grouping() {
        assert_eq!(parse_budget("200_000"), Ok(200_000));
        assert_eq!(parse_budget("200,000"), Ok(200_000));
        assert_eq!(parse_budget("0"), Ok(0));
        assert!(parse_budget("").is_err(), "an empty field must not submit 'unlimited'");
        assert!(parse_budget("-5").is_err());
        assert!(parse_budget("1e6").is_err());
    }

    /// M6: without an overlay arm in `route_paste`, a pasted token count goes to the chat rail
    /// underneath the modal — or nowhere — and the operator retypes it under time pressure.
    #[test]
    fn a_pasted_budget_reaches_the_field_not_the_rail_underneath() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Tokens(200_000));
        open_budget_field(&mut app, &src);
        set_mode(&mut app, overlay::OverlayMode::Budget {
            input: tui_input::Input::default(), error: None,
        });
        // The rail arm also matches `View::Dashboard`, so ordering inside `route_paste` decides this:
        // with the overlay arm second, the paste would land in the rail under the modal.
        app.converse_view.rail_focused = true;
        route_paste(&mut app, "150000");
        assert_eq!(budget_input(&app), "150000");
        assert!(app.converse_view.input.value().is_empty(), "must not leak into the chat rail");
    }

    // ── the in-flight and result frames ───────────────────────────────────────────────

    /// While a verb is in flight every key is a no-op — including `q`, which must not quit. Driven
    /// through `step_key`, because `handle_dashboard_key` never pushes `Effect::Quit`, so a
    /// handler-level test passes with the `overlay_was_open` guard reverted.
    #[test]
    fn step_key_q_during_a_verb_does_not_quit() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        handle_dashboard_key(key('x'), &mut app, &src);
        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &src); // Park → in flight
        assert_eq!(mode_kind(&app), "in_flight");

        let effects = step_key(&mut app, key('q'), &src);
        assert!(!effects.contains(&Effect::Quit), "q must not kill the cockpit mid-verb");
        assert_eq!(mode_kind(&app), "in_flight", "and must not dismiss the frame either");
        assert!(app.pending_verb.is_some(), "the armed verb must survive the keypress");
    }

    /// M3: the result is not `spawn_banner`. Any-key-dismisses loses the only report of what a
    /// destructive verb did, to a keystroke the operator did not mean as an acknowledgement.
    #[test]
    fn the_result_frame_dismisses_only_on_an_explicit_key() {
        let src = VerbSource::default();
        let mut app = app_with_spend(&["scout-2"], 47_000, BudgetKind::Unlimited);
        app.dashboard_overlay = Some(overlay::DashboardOverlay::menu("scout-2"));
        app.pending_verb = Some(overlay::PendingVerb::Cancel { agent_id: "scout-2".into() });
        drain_pending_verb(&mut app, &src);
        assert_eq!(mode_kind(&app), "result");

        for k in ['z', 'j', 'x', 'a'] {
            handle_dashboard_key(key(k), &mut app, &src);
            assert!(app.dashboard_overlay.is_some(), "'{k}' must not dismiss the result");
        }
        handle_dashboard_key(kev(KeyCode::Esc), &mut app, &src);
        assert!(app.dashboard_overlay.is_none(), "Esc dismisses");
    }

    #[test]
    fn step_flight_pushes_to_ring() {
        let mut app = App::new(PathBuf::from("/agents"));
        let effects = step(&mut app, AppEvent::Flight(serde_json::json!({"k": "v"})), &TestSource);
        assert_eq!(app.events.len(), 1);
        assert_eq!(effects, vec![Effect::Redraw]);
    }

    #[test]
    fn step_invalidated_marks_gap() {
        let mut app = App::new(PathBuf::from("/agents"));
        step(&mut app, AppEvent::Invalidated, &TestSource);
        assert_eq!(app.event_gaps, 1);
    }

    #[test]
    fn step_events_dropped_accumulates() {
        let mut app = App::new(PathBuf::from("/agents"));
        step(&mut app, AppEvent::EventsDropped(5), &TestSource);
        step(&mut app, AppEvent::EventsDropped(3), &TestSource);
        assert_eq!(app.dropped_events, 8);
    }

    #[test]
    fn event_ring_is_bounded() {
        let mut app = App::new(PathBuf::from("/agents"));
        let cap = crate::watch::app::EVENT_RING_CAP;
        for i in 0..(cap + 500) {
            app.push_event(serde_json::json!({ "i": i }));
        }
        assert_eq!(app.events.len(), cap, "ring capped at EVENT_RING_CAP");
        assert_eq!(app.events.front().unwrap()["i"], 500, "oldest 500 tail-dropped");
    }

    #[test]
    fn snapshot_producer_emits_without_sse_for_fuse_like_source() {
        // TestSource.event_stream_url() is None → snapshot-only producer (F4);
        // proves the pushed loop gets state without an SSE feed.
        use std::sync::Arc;
        use std::time::Duration;
        let src: Arc<dyn DataSource> = Arc::new(TestSource);
        // `None` log_services: no compose tail is spawned, so this test never shells out
        // to docker (ux.10-A — the log producer is opt-in on startup detection).
        let (rx, _wake, _producers) =
            crate::watch::pump::spawn_producers(src, Duration::from_millis(5), None);
        let ev = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("snapshot producer should emit");
        assert!(matches!(ev, AppEvent::Snapshot(_)));
        // Dropping rx/_producers stops the detached producer (F7: never joined).
    }

    #[test]
    fn step_invalidated_requests_reconcile() {
        // fix 4: Invalidated must trigger an immediate snapshot reconcile, not just a gap.
        let mut app = App::new(PathBuf::from("/agents"));
        let effects = step(&mut app, AppEvent::Invalidated, &TestSource);
        assert!(effects.contains(&Effect::Reconcile), "Invalidated must Reconcile");
        assert_eq!(app.event_gaps, 1);
    }

    #[test]
    fn step_producer_died_marks_gap() {
        // fix 5: a producer-thread panic surfaces as a gap, not a silent frozen feed.
        let mut app = App::new(PathBuf::from("/agents"));
        step(&mut app, AppEvent::ProducerDied("snapshot"), &TestSource);
        assert_eq!(app.event_gaps, 1);
    }

    #[test]
    fn drain_events_caps_per_tick() {
        // fix 2 (anti-livelock): a saturated channel drains at most `max` per call, then
        // yields — the loop still reaches key poll + draw instead of spinning forever.
        use std::sync::mpsc::sync_channel;
        let (tx, rx) = sync_channel::<AppEvent>(64);
        for i in 0..20 {
            tx.try_send(AppEvent::Flight(serde_json::json!({ "i": i }))).unwrap();
        }
        let (wake_tx, _wake_rx) = sync_channel::<()>(4);
        let mut app = App::new(PathBuf::from("/agents"));
        let quit = drain_events(&rx, &mut app, &TestSource, &wake_tx, 4);
        assert!(!quit, "no Quit from Flight events");
        assert_eq!(app.events.len(), 4, "drained exactly the cap");
        // 16 events remain in the channel for the next tick.
        let remaining = std::iter::from_fn(|| rx.try_recv().ok()).count();
        assert_eq!(remaining, 16, "cap left the rest for the next tick");
    }

    // ── ux.10: input-widget integration (behavior preservation) ──────────────

    #[test]
    fn ux10_tui_input_reflects_char_event_in_value() {
        // Smoke test: a tui_input::Input fed a crossterm char event reflects it in .value().
        use tui_input::backend::crossterm::EventHandler;
        let mut input = tui_input::Input::default();
        input.handle_event(&Event::Key(kev(KeyCode::Char('z'))));
        assert_eq!(input.value(), "z");
    }

    #[test]
    fn ux10_converse_rail_typing_then_enter_sends_and_resets() {
        // The rail's message input is a tui_input widget: typed chars route through
        // handle_event into .value(); Enter dispatches a send (echo pushed) and reset()s
        // the widget — Enter is a send, never a literal newline in the field.
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        for c in ['h', 'i'] {
            handle_dashboard_key(kev(KeyCode::Char(c)), &mut app, &TestSource);
        }
        assert_eq!(app.converse_view.input.value(), "hi", "chars must edit the tui_input value");

        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);
        assert!(app.converse_view.input.value().is_empty(),
            "Enter sends and resets the input (never inserts a newline)");
        let state = app.converse_view.targets.get(&app.converse_view.active_target).unwrap();
        assert_eq!(state.history.front().unwrap().text, "hi",
            "the optimistic echo proves Enter dispatched a send");
    }

    #[test]
    fn ux10_converse_rail_busy_guard_preserves_input_and_blocks_second_send() {
        // A second Enter while the target is mid-turn must be a no-op: input preserved
        // (not reset), no new echo — the double-submit guard survives the widget swap.
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        app.converse_view.retarget("orch-default");
        app.converse_view.targets.get_mut("orch-default").unwrap().phase =
            super::converse::ConversePhase::Dispatching;
        app.converse_view.input = tui_input::Input::new("queued".to_string());

        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &TestSource);
        assert_eq!(app.converse_view.input.value(), "queued",
            "input must be preserved (not reset) while the target is busy");
        assert!(app.converse_view.targets.get("orch-default").unwrap().history.is_empty(),
            "no echo/dispatch may happen while busy");
    }

    #[test]
    fn ux10_inspector_search_typing_updates_value_and_esc_resets() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Inspector;
        app.inspector_view.search_active = true;
        handle_inspector_key(kev(KeyCode::Char('e')), &mut app);
        handle_inspector_key(kev(KeyCode::Char('r')), &mut app);
        assert_eq!(app.inspector_view.search_query.value(), "er",
            "typing must edit the inspector search value (which rebuild_view consumes)");

        handle_inspector_key(kev(KeyCode::Esc), &mut app);
        assert!(!app.inspector_view.search_active, "Esc exits search mode");
        assert!(app.inspector_view.search_query.value().is_empty(),
            "Esc must reset() the search value");
    }

    #[test]
    fn ux10_memory_search_typing_updates_value_via_widget() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Memory;
        app.memory_view.search_active = true;
        handle_memory_key(kev(KeyCode::Char('k')), &mut app);
        handle_memory_key(kev(KeyCode::Char('b')), &mut app);
        assert_eq!(app.memory_view.search_query.value(), "kb",
            "memory search typing must land in the tui_input value (read by apply_snapshot)");
    }

    #[test]
    fn ux10_route_paste_inserts_into_focused_spawn_task_field() {
        // Bracketed paste routes to the focused textarea via route_paste (insert_str).
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.focus = SpawnFocus::TaskField;
        route_paste(&mut app, "pasted task");
        assert_eq!(app.spawn_view.task_input.lines().join("\n"), "pasted task",
            "paste must be inserted into the focused spawn task textarea");
    }

    #[test]
    fn ux10_route_paste_into_focused_converse_rail() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        route_paste(&mut app, "clip");
        assert_eq!(app.converse_view.input.value(), "clip",
            "paste must be inserted into the focused converse rail input");
    }

    // ── attn.2-R5: Jobs view + manual fire ──────────────────────────────────────

    fn job_row(job_id: &str) -> reader::SysJob {
        reader::SysJob {
            job_id: job_id.to_string(),
            schedule_described: "0 8 * * * (UTC)".to_string(),
            next_fire_ts: 1_800_000_000,
            last_outcome: String::new(),
            last_skip_reason: None,
            shadow_mode: true,
        }
    }

    fn app_with_jobs(ids: &[&str]) -> App {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None,
            isolation: None, credentials: None, error: None,
            jobs: ids.iter().map(|id| job_row(id)).collect(),
        });
        app
    }

    /// Records exactly what reached the `DataSource`, mirroring `VerbSource` above — the
    /// same two-phase-split proof (nothing during key dispatch, exactly once from the loop).
    #[derive(Default)]
    struct RunJobSource {
        calls: std::sync::Mutex<Vec<String>>,
        fail:  Option<String>,
    }
    impl RunJobSource {
        fn failing(msg: &str) -> Self {
            Self { fail: Some(msg.to_string()), ..Default::default() }
        }
    }
    impl DataSource for RunJobSource {
        fn load_snapshot(&self) -> Snapshot {
            Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
        }
        fn load_approvals(&self) -> Vec<PendingAction> { vec![] }
        fn approve(&self, _id: &str) -> Result<(), String> { Err("n/a".into()) }
        fn deny(&self, _id: &str, _r: Option<&str>) -> Result<(), String> { Err("n/a".into()) }
        fn run_job(&self, job_id: &str) -> Result<String, String> {
            self.calls.lock().unwrap().push(job_id.to_string());
            match &self.fail {
                Some(e) => Err(e.clone()),
                None => Ok(format!("{job_id}-manual-1")),
            }
        }
    }

    #[test]
    fn jobs_key_up_down_clamp_to_the_list_bounds() {
        let mut app = app_with_jobs(&["cos-inbox", "cos-curator"]);
        handle_jobs_key(kev(KeyCode::Up), &mut app);
        assert_eq!(app.jobs_selected, 0, "cannot go above row 0");
        handle_jobs_key(kev(KeyCode::Down), &mut app);
        handle_jobs_key(kev(KeyCode::Down), &mut app);
        handle_jobs_key(kev(KeyCode::Down), &mut app);
        assert_eq!(app.jobs_selected, 1, "cannot go past the last row");
    }

    #[test]
    fn jobs_key_enter_opens_confirm_pinned_to_the_selected_row() {
        let mut app = app_with_jobs(&["cos-inbox", "cos-curator"]);
        app.jobs_selected = 1;
        handle_jobs_key(kev(KeyCode::Enter), &mut app);
        let ov = app.jobs_overlay.as_ref().expect("overlay must open");
        assert_eq!(ov.target_job_id, "cos-curator");
        assert_eq!(ov.mode, JobOverlayMode::ConfirmFire);
    }

    #[test]
    fn jobs_key_y_on_confirm_arms_pending_verb_and_moves_to_in_flight() {
        let mut app = app_with_jobs(&["cos-inbox"]);
        handle_jobs_key(kev(KeyCode::Enter), &mut app);
        handle_jobs_key(kev(KeyCode::Char('y')), &mut app);
        assert_eq!(
            app.pending_verb,
            Some(PendingVerb::RunJob { job_id: "cos-inbox".to_string() }),
            "the key handler must never call the DataSource directly — it parks the verb"
        );
        assert_eq!(app.jobs_overlay.as_ref().unwrap().mode, JobOverlayMode::InFlight);
    }

    #[test]
    fn jobs_key_n_esc_q_on_confirm_closes_without_arming_anything() {
        for key in [KeyCode::Char('n'), KeyCode::Esc, KeyCode::Char('q')] {
            let mut app = app_with_jobs(&["cos-inbox"]);
            handle_jobs_key(kev(KeyCode::Enter), &mut app);
            handle_jobs_key(kev(key), &mut app);
            assert!(app.jobs_overlay.is_none(), "{key:?} must close the confirm overlay");
            assert!(app.pending_verb.is_none(), "{key:?} must not arm a fire");
        }
    }

    #[test]
    fn jobs_key_any_key_dismisses_the_result_frame() {
        let mut app = app_with_jobs(&["cos-inbox"]);
        app.jobs_overlay = Some(JobsOverlay {
            target_job_id: "cos-inbox".to_string(),
            mode: JobOverlayMode::Result { text: "Fired".to_string(), ok: true },
        });
        handle_jobs_key(kev(KeyCode::Char('z')), &mut app);
        assert!(app.jobs_overlay.is_none());
    }

    #[test]
    fn jobs_key_in_flight_ignores_every_key() {
        let mut app = app_with_jobs(&["cos-inbox"]);
        app.jobs_overlay = Some(JobsOverlay {
            target_job_id: "cos-inbox".to_string(),
            mode: JobOverlayMode::InFlight,
        });
        for key in [KeyCode::Char('y'), KeyCode::Esc, KeyCode::Enter] {
            handle_jobs_key(kev(key), &mut app);
            assert_eq!(app.jobs_overlay.as_ref().unwrap().mode, JobOverlayMode::InFlight,
                "{key:?} must be a no-op while a call is in flight");
        }
    }

    #[test]
    fn jobs_key_esc_q_with_no_overlay_returns_to_dashboard() {
        let mut app = app_with_jobs(&["cos-inbox"]);
        app.view = View::Jobs;
        handle_jobs_key(kev(KeyCode::Esc), &mut app);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn explain_verb_error_409_names_the_concurrent_run_not_raw_json() {
        let raw = "HTTP 409: {\"error\":\"job 'cos-inbox' already has a live run in progress (1 child(ren)); concurrent fire refused\"}";
        let out = explain_verb_error(raw);
        assert!(out.contains("live run in progress") || out.contains("second concurrent run"),
            "409 must be humanized, not passed through as raw JSON: {out}");
        assert!(!out.starts_with("HTTP 409"), "must not just echo the raw error: {out}");
    }

    #[test]
    fn explain_verb_error_run_job_timeout_does_not_claim_action_not_sent() {
        // /autoplan retroactive review: for run_job specifically, "Action not sent" can be
        // FALSE (the command may already be queued) — telling the operator to retry blind
        // risks the exact concurrent-fire race the guard exists to prevent.
        let raw = "HTTP 503: {\"error\":\"timed out waiting for run_job\"}";
        let out = explain_verb_error(raw);
        assert!(!out.contains("Action not sent"),
            "run_job's timeout must not claim nothing happened: {out}");
        assert!(out.contains("Uncertain") || out.contains("may have already"), "{out}");
    }

    #[test]
    fn explain_verb_error_other_503s_still_say_action_not_sent() {
        // Every OTHER 503 (cancel/set-budget/set-caps/spawn) genuinely has not been sent yet
        // — the run_job-specific wording must not leak into these.
        let raw = "HTTP 503: {\"error\":\"timed out waiting for cancel\"}";
        let out = explain_verb_error(raw);
        assert!(out.contains("Action not sent"), "{out}");
    }

    #[test]
    fn drain_run_job_fire_success_writes_the_result_into_jobs_overlay() {
        let mut app = app_with_jobs(&["cos-inbox"]);
        app.jobs_overlay = Some(JobsOverlay {
            target_job_id: "cos-inbox".to_string(),
            mode: JobOverlayMode::InFlight,
        });
        app.pending_verb = Some(PendingVerb::RunJob { job_id: "cos-inbox".to_string() });
        let src = RunJobSource::default();
        drain_pending_verb(&mut app, &src);
        assert_eq!(src.calls.lock().unwrap().as_slice(), ["cos-inbox"]);
        let JobOverlayMode::Result { ok, .. } = &app.jobs_overlay.as_ref().unwrap().mode else {
            panic!("expected Result mode");
        };
        assert!(*ok);
        assert!(app.pending_verb.is_none(), "the slot must be drained, not re-armed");
    }

    #[test]
    fn drain_run_job_fire_error_writes_a_non_ok_result() {
        let mut app = app_with_jobs(&["cos-inbox"]);
        app.jobs_overlay = Some(JobsOverlay {
            target_job_id: "cos-inbox".to_string(),
            mode: JobOverlayMode::InFlight,
        });
        app.pending_verb = Some(PendingVerb::RunJob { job_id: "cos-inbox".to_string() });
        let src = RunJobSource::failing("unknown job id");
        drain_pending_verb(&mut app, &src);
        let JobOverlayMode::Result { ok, text } = &app.jobs_overlay.as_ref().unwrap().mode else {
            panic!("expected Result mode");
        };
        assert!(!ok);
        assert!(text.contains("unknown job id") || !text.is_empty());
    }

    #[test]
    fn drain_run_job_fire_does_not_clobber_a_different_jobs_overlay() {
        // Resolve-at-use (same discipline DashboardOverlay::target uses): if the operator
        // somehow reopened the overlay against a DIFFERENT job while the first call was in
        // flight, the stale call's result must not overwrite the new target's frame.
        let mut app = app_with_jobs(&["cos-inbox", "cos-curator"]);
        app.jobs_overlay = Some(JobsOverlay {
            target_job_id: "cos-curator".to_string(),
            mode: JobOverlayMode::ConfirmFire,
        });
        app.pending_verb = Some(PendingVerb::RunJob { job_id: "cos-inbox".to_string() });
        let src = RunJobSource::default();
        drain_pending_verb(&mut app, &src);
        assert_eq!(
            app.jobs_overlay.as_ref().unwrap().mode,
            JobOverlayMode::ConfirmFire,
            "a stale in-flight result for a different job must not touch the current overlay"
        );
    }
}
