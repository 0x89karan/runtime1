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
pub mod pump;
pub mod reader;
pub mod source;
pub mod spawn;
pub mod topology;
pub mod views;

use app::{App, MemoryPane, PendingSpawn, SpawnFocus, View};
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
    loop {
        // Checked every tick (~30ms) so an out-of-band SIGTERM/SIGINT unwinds
        // normally instead of hitting the default signal disposition — see
        // install_shutdown_signal_handlers().
        if SHUTDOWN_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
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
    }
    let mut effects = vec![Effect::Redraw];
    if matches!(key.code, KeyCode::Char('q')) && was_dashboard && !rail_was_focused {
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
                    match converse::dispatch(source, &target, &text, converse::DEFAULT_MAX_TURNS) {
                        Ok(resolved_id) => {
                            // If the server resolved a different id than requested (e.g.
                            // HttpSource::spawn's "operator-agent" fallback when the response
                            // omits `agent_id`), the just-pushed echo lives under the stale
                            // `target` key, which will never receive the real agent's events
                            // (those are tagged with `resolved_id`). Move it across rather
                            // than orphaning it (found by /ship's Step 9 testing +
                            // maintainability specialists — two views of the same bug).
                            if resolved_id != target {
                                if let Some(abandoned) = app.converse_view.targets.remove(&target) {
                                    let dest =
                                        app.converse_view.targets.entry(resolved_id.clone()).or_default();
                                    for turn in abandoned.history {
                                        dest.push_history(turn.role, turn.text);
                                    }
                                }
                            }
                            app.converse_view.active_target = resolved_id.clone();
                            // entry().or_default(), not get_mut(): resolved_id's entry may not
                            // exist yet even after the move above (e.g. `target` had no prior
                            // state to move). get_mut on a fresh resolved_id would silently
                            // no-op, dropping this state update and, since every subsequent
                            // event for the real agent is looked up by this same key, wedging
                            // the conversation forever.
                            let state = app.converse_view.targets.entry(resolved_id).or_default();
                            state.phase = converse::ConversePhase::Dispatching;
                            state.last_event_at = Some(std::time::Instant::now());
                        }
                        Err(e) => {
                            if let Some(state) = app.converse_view.targets.get_mut(&target) {
                                state.push_history(
                                    converse::TurnRole::System,
                                    format!("Spawn rejected: {e} — press Enter to retry"),
                                );
                            }
                        }
                    }
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
        // ux.10-A: [l] opens the compose log tail. `[l]`, not `[g]` — `[g]` is Spawn's
        // "generate preview" and, although key dispatch is per-view, reusing the letter
        // across views is a footgun the /autoplan eng consensus explicitly rejected.
        // Gated on the startup detection: on bare agentd / QEMU there is no compose project,
        // so the key is inert AND absent from the legend (views.rs reads the same flag).
        KeyCode::Char('l') if app.logs_view.available => {
            app.view = View::Logs;
        }
        _ => {}
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
                    let id_opt   = app.approvals_view.confirmed_id.clone();
                    let found_id = id_opt.as_deref().and_then(|id| {
                        app.approvals_items.iter().find(|i| i.id == id).map(|i| i.id.clone())
                    });
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
                    let id_opt   = app.approvals_view.confirmed_id.clone();
                    let found_id = id_opt.as_deref().and_then(|id| {
                        app.approvals_items.iter().find(|i| i.id == id).map(|i| i.id.clone())
                    });
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
                    let id_opt  = app.approvals_view.confirmed_id.clone();
                    let found   = id_opt.as_deref().and_then(|id| {
                        app.approvals_items.iter().find(|i| i.id == id)
                            .map(|i| (i.id.clone(), i.kind.clone()))
                    });
                    app.approvals_view.result_msg = if let Some((found_id, found_kind)) = found {
                        match source.approve_with_kind(&found_id, &found_kind) {
                            Ok(()) => {
                                app.approvals_items.retain(|i| i.id != found_id);
                                Some(format!("Approved {found_id} (auto for '{found_kind}')"))
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
        do_spawn_action, drain_events, handle_approvals_key, handle_dashboard_key,
        handle_inspector_key, handle_logs_key, handle_memory_key, handle_spawn_key, logs,
        on_resize, route_paste, step, step_key, App, Effect, View, MAX_PASTE_CHARS,
    };
    use crate::watch::app::{MemoryPane, SpawnFocus};
    use crate::watch::approvals::ApprovalsMode;
    use crate::watch::pump::AppEvent;
    use crate::watch::reader::{self, AgentInfo, BudgetKind, PendingAction, Snapshot};
    use crate::watch::source::{DataSource, HttpSource, SpawnRequest};

    struct TestSource;
    impl DataSource for TestSource {
        fn load_snapshot(&self) -> Snapshot {
            Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None }
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
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
            Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None }
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
    }

    #[test]
    fn enter_moves_echo_and_creates_state_when_server_resolves_a_different_id() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        app.converse_view.retarget("requested-id");
        app.converse_view.input = tui_input::Input::new("hello".to_string());

        handle_dashboard_key(kev(KeyCode::Enter), &mut app, &ResolvesDifferentIdSource);

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
                       provider: None, isolation: None, credentials: None, error: None }
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
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(kev(KeyCode::Char('a')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "after Approve must return to List mode");
        assert!(app.approvals_view.result_msg.is_some(),
            "result_msg must be set after approve attempt");
    }

    #[test]
    fn approvals_confirm_dont_ask_again_with_no_control_file_sets_error_msg() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(kev(KeyCode::Char('d')), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "after 'don't ask again' must return to List mode");
        assert!(app.approvals_view.result_msg.is_some(),
            "result_msg must be set after don't-ask-again attempt");
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
}
