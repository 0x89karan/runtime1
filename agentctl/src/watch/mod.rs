use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, SyncSender},
    sync::Arc,
    time::Duration,
};

use anyhow::Context;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub mod app;
pub mod approvals;
pub mod converse;
pub mod inspector;
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
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen) {
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
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
            }
            prev(info);
        }));
        Ok(TermGuard)
    }
}
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
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
    install_shutdown_signal_handlers();
    let guard = TermGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend).context("creating terminal")?;
    let mut app = App::new(agents_dir.clone());
    app.log_path = log_path.clone();

    run_tui_loop(&mut term, &mut app, &source, interval)?;

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
                app2.spawn_banner =
                    Some(format!("Agent '{}' injected via /agents/control", agent_id_hint));
                run_tui_loop(&mut term2, &mut app2, &source, interval)?;
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
) -> anyhow::Result<()> {
    // Producers stop when `_producers`/`rx`/`wake_tx` drop at loop exit (detached; never joined — F7).
    let (rx, wake_tx, _producers) = spawn_producers(Arc::clone(source), interval);
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
        AppEvent::ProducerDied(_which) => {
            // A producer thread panicked (fix 5): surface it as a gap so the feed
            // reads as stalled instead of silently rendering the last state forever.
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
        View::Dashboard => handle_dashboard_key(key.code, app, source),
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
        View::Memory => handle_memory_key(key.code, app),
        View::Spawn => handle_spawn_key(key.code, app),
        View::Inspector => handle_inspector_key(key.code, app),
        View::Approvals => handle_approvals_key(key.code, app, source),
        View::Credentials => {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                app.view = View::Dashboard;
            }
        }
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

fn handle_memory_key(code: KeyCode, app: &mut App) {
    match code {
        // Search mode: [/] enters, Esc exits + clears query.
        KeyCode::Char('/') if !app.memory_view.search_active => {
            app.memory_view.search_active = true;
        }
        KeyCode::Esc if app.memory_view.search_active => {
            app.memory_view.search_active = false;
            app.memory_view.search_query.clear();
        }
        // Typed characters → query while in search mode.
        KeyCode::Char(c) if app.memory_view.search_active => {
            app.memory_view.search_query.push(c);
        }
        KeyCode::Backspace if app.memory_view.search_active => {
            app.memory_view.search_query.pop();
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
fn handle_dashboard_key(code: KeyCode, app: &mut App, source: &dyn DataSource) {
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
                let text = std::mem::take(&mut app.converse_view.input);
                if !text.is_empty() {
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
            KeyCode::Backspace => {
                app.converse_view.input.pop();
            }
            KeyCode::Char(c) => {
                app.converse_view.input.push(c);
            }
            _ => {}
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
            app.memory_view.search_query.clear();
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
        _ => {}
    }
}

fn handle_spawn_key(code: KeyCode, app: &mut App) {
    let focus = app.spawn_view.focus.clone();
    match (&focus, code) {
        // TaskField captures all char input; Esc defocuses, Enter tabs forward.
        (SpawnFocus::TaskField, KeyCode::Char(c)) => {
            app.spawn_view.task_input.push(c);
        }
        (SpawnFocus::TaskField, KeyCode::Backspace) => {
            app.spawn_view.task_input.pop();
        }
        (SpawnFocus::TaskField, KeyCode::Esc) => {
            app.spawn_view.focus = SpawnFocus::TemplatePicker;
        }
        (SpawnFocus::TaskField, KeyCode::Enter) => {
            app.spawn_view.focus_next();
        }
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
        (_, KeyCode::Char('g')) => {
            let dir = app.agents_dir.clone();
            app.spawn_view.do_generate(Some(&dir));
        }
        (SpawnFocus::ActionGenerate, KeyCode::Enter | KeyCode::Char(' ')) => {
            let dir = app.agents_dir.clone();
            app.spawn_view.do_generate(Some(&dir));
        }
        // Spawn action: [r] shortcut (outside TaskField) or Enter on button.
        (_, KeyCode::Char('r')) => {
            app.spawn_view.do_spawn();
        }
        (SpawnFocus::ActionSpawn, KeyCode::Enter | KeyCode::Char(' ')) => {
            app.spawn_view.do_spawn();
        }
        _ => {}
    }
}

fn handle_inspector_key(code: KeyCode, app: &mut App) {
    match code {
        // Search mode: [/] enters, Esc exits + clears query.
        KeyCode::Char('/') if !app.inspector_view.search_active => {
            app.inspector_view.search_active = true;
        }
        KeyCode::Esc if app.inspector_view.search_active => {
            app.inspector_view.search_active = false;
            app.inspector_view.search_query.clear();
            app.inspector_view.rebuild_view();
        }
        KeyCode::Char(c) if app.inspector_view.search_active => {
            app.inspector_view.search_query.push(c);
            app.inspector_view.rebuild_view();
        }
        KeyCode::Backspace if app.inspector_view.search_active => {
            app.inspector_view.search_query.pop();
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

fn handle_approvals_key(code: KeyCode, app: &mut App, source: &dyn DataSource) {
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
                        let reason = app.approvals_view.reject_reason.clone();
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
                    app.approvals_view.reject_reason.clear();
                }
                KeyCode::Esc => {
                    app.approvals_view.mode = ApprovalsMode::Confirm;
                    app.approvals_view.reject_reason.clear();
                }
                KeyCode::Char(c) => {
                    app.approvals_view.reject_reason.push(c);
                }
                KeyCode::Backspace => {
                    app.approvals_view.reject_reason.pop();
                }
                _ => {}
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
                    app.approvals_view.reject_reason.clear();
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

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{
        drain_events, handle_approvals_key, handle_dashboard_key, handle_memory_key,
        handle_spawn_key, on_resize, step, step_key, App, Effect, View,
    };
    use crate::watch::app::{MemoryPane, SpawnFocus};
    use crate::watch::approvals::ApprovalsMode;
    use crate::watch::pump::AppEvent;
    use crate::watch::reader::{self, AgentInfo, BudgetKind, PendingAction, Snapshot};
    use crate::watch::source::DataSource;

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
        handle_dashboard_key(KeyCode::Down, &mut app, &TestSource);

        assert_eq!(app.selected_id.as_deref(), Some("b"));
    }

    #[test]
    fn dashboard_key_j_advances_selection() {
        let mut app = app_with_agents(&["a", "b"]);
        handle_dashboard_key(KeyCode::Char('j'), &mut app, &TestSource);
        assert_eq!(app.selected_id.as_deref(), Some("b"));
    }

    #[test]
    fn dashboard_key_up_arrow_decrements_selection() {
        let mut app = app_with_agents(&["a", "b", "c"]);
        app.selected_id = Some("b".to_string());
        handle_dashboard_key(KeyCode::Up, &mut app, &TestSource);

        assert_eq!(app.selected_id.as_deref(), Some("a"));
    }

    #[test]
    fn dashboard_key_k_decrements_selection() {
        let mut app = app_with_agents(&["a", "b"]);
        app.selected_id = Some("b".to_string());
        handle_dashboard_key(KeyCode::Char('k'), &mut app, &TestSource);
        assert_eq!(app.selected_id.as_deref(), Some("a"));
    }

    #[test]
    fn dashboard_key_enter_switches_to_agent_detail_when_selection_present() {
        let mut app = app_with_agents(&["a"]);
        assert_eq!(app.view, View::Dashboard);
        handle_dashboard_key(KeyCode::Enter, &mut app, &TestSource);

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
        handle_dashboard_key(KeyCode::Enter, &mut app, &TestSource);

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
        handle_dashboard_key(KeyCode::Enter, &mut app, &TestSource);

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
        handle_dashboard_key(KeyCode::Enter, &mut app, &TestSource);

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
        handle_dashboard_key(KeyCode::Enter, &mut app, &TestSource);

        assert_eq!(app.view, View::Approvals);
    }

    #[test]
    fn dashboard_key_enter_does_not_switch_view_when_no_agents() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Enter, &mut app, &TestSource);

        assert_eq!(app.view, View::Dashboard,
            "Enter with no agents must not navigate to AgentDetail");
    }

    #[test]
    fn dashboard_key_s_switches_to_system_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Char('s'), &mut app, &TestSource);
        assert_eq!(app.view, View::System);
    }

    #[test]
    fn dashboard_key_t_switches_to_topology_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Char('t'), &mut app, &TestSource);
        assert_eq!(app.view, View::Topology);
    }

    #[test]
    fn dashboard_key_other_is_noop() {
        let mut app = app_with_agents(&["a"]);
        let original_id = app.selected_id.clone();
        handle_dashboard_key(KeyCode::F(1), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard);
        assert_eq!(app.selected_id, original_id);
    }

    // ── ux.1 Dashboard focus retrofit ───────────────────────────────────────────

    #[test]
    fn tab_toggles_rail_focus_both_directions() {
        let mut app = app_with_agents(&["a"]);
        assert!(!app.converse_view.rail_focused);
        handle_dashboard_key(KeyCode::Tab, &mut app, &TestSource);
        assert!(app.converse_view.rail_focused, "Tab should focus the rail from the table");
        handle_dashboard_key(KeyCode::Tab, &mut app, &TestSource);
        assert!(!app.converse_view.rail_focused, "Tab should return focus to the table from the rail");
    }

    #[test]
    fn tab_is_a_noop_when_terminal_too_narrow_for_rail() {
        // Caught during /review's adversarial Codex pass: Tab previously focused the
        // rail unconditionally, silently swallowing every subsequent keystroke into an
        // input box the operator can't even see on a narrow/short terminal.
        let mut app = app_with_agents(&["a"]);
        app.term_size = (80, 24); // below MIN_TOTAL_WIDTH_FOR_RAIL (115)
        handle_dashboard_key(KeyCode::Tab, &mut app, &TestSource);
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
        handle_dashboard_key(KeyCode::Esc, &mut app, &TestSource);
        assert!(!app.converse_view.rail_focused);
    }

    #[test]
    fn rail_focused_char_input_does_not_trigger_view_shortcut() {
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        handle_dashboard_key(KeyCode::Char('s'), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard, "'s' must be captured as chat input, not a view switch, while the rail has focus");
        assert_eq!(app.converse_view.input, "s");
    }

    #[test]
    fn r_retargets_only_when_table_focused() {
        let mut app = app_with_agents(&["a", "b"]);
        app.select_next(); // select "b"
        handle_dashboard_key(KeyCode::Char('r'), &mut app, &TestSource);
        assert_eq!(app.converse_view.active_target, "b", "r should retarget to the selected row while the table has focus");

        // While rail-focused, 'r' is literal input, not a retarget.
        app.converse_view.rail_focused = true;
        app.converse_view.active_target = "orch-default".to_string();
        handle_dashboard_key(KeyCode::Char('r'), &mut app, &TestSource);
        assert_eq!(app.converse_view.active_target, "orch-default", "r must not retarget while the rail has focus");
        assert_eq!(app.converse_view.input, "r");
    }

    #[test]
    fn enter_routes_to_chat_send_when_rail_focused_vs_existing_routing_when_table_focused() {
        // Table-focused: Enter keeps its existing AgentDetail/Approvals/Credentials routing.
        let mut app = app_with_agents(&["a"]);
        handle_dashboard_key(KeyCode::Enter, &mut app, &TestSource);
        assert_eq!(app.view, View::AgentDetail, "existing Enter routing must be unchanged when the table has focus");

        // Rail-focused: Enter sends the typed input as a chat message instead.
        // TestSource doesn't implement event_stream_url (default trait impl → None), so
        // the optimistic echo is followed by the FUSE-capability-gate system line, not a
        // dispatch attempt — this test asserts the echo happened first, which is the
        // behavior under test here. See the dedicated gate test below for the full
        // no-SSE-support behavior.
        let mut app2 = app_with_agents(&["a"]);
        app2.converse_view.rail_focused = true;
        app2.converse_view.input = "hello".to_string();
        handle_dashboard_key(KeyCode::Enter, &mut app2, &TestSource);
        assert_eq!(app2.view, View::Dashboard, "sending a chat message must not change the view");
        assert!(app2.converse_view.input.is_empty(), "input box clears after send");
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
        app.converse_view.input = "hello".to_string();
        handle_dashboard_key(KeyCode::Enter, &mut app, &TestSource);

        assert!(app.converse_view.input.is_empty(), "input box clears after send");
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
        app.converse_view.input = "second message".to_string();

        handle_dashboard_key(KeyCode::Enter, &mut app, &TestSource);

        assert_eq!(app.converse_view.input, "second message", "input must be preserved, not cleared, while busy");
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
        app.converse_view.input = "hello".to_string();

        handle_dashboard_key(KeyCode::Enter, &mut app, &ResolvesDifferentIdSource);

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
        handle_dashboard_key(KeyCode::Up, &mut app, &TestSource);
        let state = app.converse_view.targets.get("orch-default").unwrap();
        assert_eq!(state.scroll_up_lines, 1, "Up must scroll, not type, while the rail has focus");
        assert!(app.converse_view.input.is_empty(), "Up must not be captured as text input");
    }

    #[test]
    fn rail_focused_letter_j_k_g_are_captured_as_text_not_scroll_shortcuts() {
        // Critical: 'j'/'k'/'G' must NOT be scroll shortcuts while the rail has text
        // focus, or typing an ordinary word containing those letters would hijack the
        // transcript instead of being entered (this was caught and fixed during /review).
        let mut app = app_with_agents(&["a"]);
        app.converse_view.rail_focused = true;
        for c in ['j', 'k', 'G'] {
            handle_dashboard_key(KeyCode::Char(c), &mut app, &TestSource);
        }
        assert_eq!(app.converse_view.input, "jkG", "j/k/G must be captured as literal chat text while the rail has focus");
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
        handle_dashboard_key(KeyCode::End, &mut app, &TestSource);
        let state = app.converse_view.targets.get("orch-default").unwrap();
        assert!(state.follow, "End must re-arm follow");
        assert_eq!(state.new_since_scroll, 0, "End must clear the unread counter");
    }

    #[test]
    fn dashboard_key_m_switches_to_memory_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Char('m'), &mut app, &TestSource);
        assert_eq!(app.view, View::Memory);
    }

    #[test]
    fn dashboard_key_n_switches_to_spawn_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Char('n'), &mut app, &TestSource);
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
        handle_memory_key(KeyCode::Char('/'), &mut app);
        assert!(app.memory_view.search_active, "/ must activate search mode");
    }

    #[test]
    fn memory_key_esc_exits_search_mode_and_clears_query() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.search_active = true;
        app.memory_view.search_query  = "foo".to_string();
        handle_memory_key(KeyCode::Esc, &mut app);
        assert!(!app.memory_view.search_active, "Esc must exit search mode");
        assert!(app.memory_view.search_query.is_empty(), "Esc must clear query");
    }

    #[test]
    fn memory_key_char_appends_to_query_when_searching() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.search_active = true;
        app.memory_view.search_query  = "fo".to_string();
        handle_memory_key(KeyCode::Char('o'), &mut app);
        assert_eq!(app.memory_view.search_query, "foo");
    }

    #[test]
    fn memory_key_backspace_pops_query_char() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.search_active = true;
        app.memory_view.search_query  = "foo".to_string();
        handle_memory_key(KeyCode::Backspace, &mut app);
        assert_eq!(app.memory_view.search_query, "fo");
    }

    #[test]
    fn memory_key_tab_cycles_shortterm_longterm_kb() {
        let mut app = App::new(PathBuf::from("/agents"));
        assert_eq!(app.memory_view.pane, MemoryPane::ShortTerm);
        handle_memory_key(KeyCode::Tab, &mut app);
        assert_eq!(app.memory_view.pane, MemoryPane::LongTerm);
        handle_memory_key(KeyCode::Tab, &mut app);
        assert_eq!(app.memory_view.pane, MemoryPane::Kb);
        handle_memory_key(KeyCode::Tab, &mut app);
        assert_eq!(app.memory_view.pane, MemoryPane::ShortTerm, "tab must wrap around");
    }

    #[test]
    fn memory_key_up_decrements_active_pane_scroll() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.short_term_scroll = 3;
        handle_memory_key(KeyCode::Up, &mut app);
        assert_eq!(app.memory_view.short_term_scroll, 2);
    }

    #[test]
    fn memory_key_scroll_saturates_at_zero() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.memory_view.short_term_scroll = 0;
        handle_memory_key(KeyCode::Char('k'), &mut app);
        assert_eq!(app.memory_view.short_term_scroll, 0, "scroll must not underflow");
    }

    #[test]
    fn memory_key_j_increments_active_pane_scroll() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_memory_key(KeyCode::Char('j'), &mut app);
        assert_eq!(app.memory_view.short_term_scroll, 1);
    }

    #[test]
    fn memory_key_q_returns_to_dashboard() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Memory;
        handle_memory_key(KeyCode::Char('q'), &mut app);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn memory_key_esc_returns_to_dashboard_when_not_searching() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Memory;
        handle_memory_key(KeyCode::Esc, &mut app);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn app_memory_state_resets_on_m_key() {
        let mut app = App::new(PathBuf::from("/agents"));
        // Pre-set stale state.
        app.memory_view.short_term_scroll = 10;
        app.memory_view.long_term_scroll  = 5;
        app.memory_view.kb_scroll         = 3;
        app.memory_view.search_query      = "old query".to_string();
        app.memory_view.search_active     = true;
        handle_dashboard_key(KeyCode::Char('m'), &mut app, &TestSource);
        assert_eq!(app.memory_view.short_term_scroll, 0, "short_term_scroll must reset");
        assert_eq!(app.memory_view.long_term_scroll,  0, "long_term_scroll must reset");
        assert_eq!(app.memory_view.kb_scroll,         0, "kb_scroll must reset");
        assert!(app.memory_view.search_query.is_empty(), "search_query must clear");
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
        handle_spawn_key(KeyCode::Esc, &mut app);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn spawn_key_q_returns_to_dashboard_when_not_in_task_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.focus = SpawnFocus::ActionGenerate;
        handle_spawn_key(KeyCode::Char('q'), &mut app);
        assert_eq!(app.view, View::Dashboard);
    }

    #[test]
    fn spawn_key_q_appends_to_task_when_task_field_focused() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.focus = SpawnFocus::TaskField;
        handle_spawn_key(KeyCode::Char('q'), &mut app);
        assert_eq!(app.view, View::Spawn, "view must stay Spawn while in task field");
        assert_eq!(app.spawn_view.task_input, "q", "char must append to task input");
    }

    #[test]
    fn spawn_key_esc_defocuses_task_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.view = View::Spawn;
        app.spawn_view.focus = SpawnFocus::TaskField;
        handle_spawn_key(KeyCode::Esc, &mut app);
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
        handle_spawn_key(KeyCode::Tab, &mut app);
        assert_eq!(app.spawn_view.focus, SpawnFocus::TaskField);
        handle_spawn_key(KeyCode::Tab, &mut app);
        assert_eq!(app.spawn_view.focus, SpawnFocus::CapToggles);
        handle_spawn_key(KeyCode::Tab, &mut app);
        assert_eq!(app.spawn_view.focus, SpawnFocus::ActionGenerate);
        handle_spawn_key(KeyCode::Tab, &mut app);
        assert_eq!(app.spawn_view.focus, SpawnFocus::ActionSpawn);
        handle_spawn_key(KeyCode::Tab, &mut app);
        assert_eq!(app.spawn_view.focus, SpawnFocus::TemplatePicker, "must wrap");
    }

    #[test]
    fn spawn_key_backspace_removes_last_char_from_task() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TaskField;
        app.spawn_view.task_input = "hello".to_string();
        handle_spawn_key(KeyCode::Backspace, &mut app);
        assert_eq!(app.spawn_view.task_input, "hell");
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
        handle_spawn_key(KeyCode::Char(' '), &mut app);
        assert!(!app.spawn_view.cap_toggles[0].2, "space must toggle cap off");
    }

    #[test]
    fn spawn_key_enter_in_task_field_cycles_focus_forward() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TaskField;
        // No cap_toggles — TaskField Enter skips to ActionGenerate.
        handle_spawn_key(KeyCode::Enter, &mut app);
        assert_eq!(app.spawn_view.focus, SpawnFocus::ActionGenerate,
            "Enter in TaskField must advance focus (not exit view)");
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
        handle_spawn_key(KeyCode::Up, &mut app);
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
        handle_spawn_key(KeyCode::Down, &mut app);
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
        handle_spawn_key(KeyCode::Char('k'), &mut app);
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
        handle_spawn_key(KeyCode::Char('j'), &mut app);
        assert_eq!(app.spawn_view.cap_idx, 1, "'j' in CapToggles must call cap_next");
    }

    #[test]
    fn spawn_key_g_calls_do_generate_when_not_in_task_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TemplatePicker;
        // No templates loaded — do_generate sets an error result_msg.
        handle_spawn_key(KeyCode::Char('g'), &mut app);
        assert!(app.spawn_view.result_msg.is_some(),
            "'g' outside TaskField must invoke do_generate (result_msg set)");
    }

    #[test]
    fn spawn_key_r_calls_do_spawn_when_not_in_task_field() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TemplatePicker;
        // No templates loaded — do_spawn sets an error result_msg.
        handle_spawn_key(KeyCode::Char('r'), &mut app);
        assert!(app.spawn_view.result_msg.is_some(),
            "'r' outside TaskField must invoke do_spawn (result_msg set)");
    }

    #[test]
    fn spawn_key_r_appends_to_task_when_task_field_focused() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.spawn_view.focus = SpawnFocus::TaskField;
        handle_spawn_key(KeyCode::Char('r'), &mut app);
        assert_eq!(app.spawn_view.task_input, "r",
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
        handle_spawn_key(KeyCode::Char('g'), &mut app);
        assert_eq!(app.spawn_view.task_input, "g",
            "Char('g') in TaskField must append to task input, not trigger do_generate");
        assert!(app.spawn_view.preview.is_none(),
            "Char('g') in TaskField must not trigger do_generate");
        assert!(app.spawn_view.result_msg.is_none(),
            "Char('g') in TaskField must not set result_msg");
    }

    // ── handle_dashboard_key: [a] opens Approvals view ───────────────────────

    #[test]
    fn dashboard_key_a_switches_to_approvals_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Char('a'), &mut app, &TestSource);
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
        handle_approvals_key(KeyCode::Char('q'), &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard,
            "'q' in Approvals list mode must return to Dashboard");
    }

    #[test]
    fn approvals_key_esc_returns_to_dashboard_from_list() {
        let mut app = app_with_approvals(&[]);
        handle_approvals_key(KeyCode::Esc, &mut app, &TestSource);
        assert_eq!(app.view, View::Dashboard,
            "Esc in Approvals list mode must return to Dashboard");
    }

    #[test]
    fn approvals_key_enter_enters_confirm_when_items_present() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        handle_approvals_key(KeyCode::Enter, &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::Confirm,
            "Enter with pending items must enter Confirm mode");
        assert_eq!(app.view, View::Approvals,
            "view must stay Approvals after Enter");
    }

    #[test]
    fn approvals_key_enter_is_noop_when_no_items() {
        let mut app = app_with_approvals(&[]);
        handle_approvals_key(KeyCode::Enter, &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "Enter with no items must stay in List mode");
    }

    #[test]
    fn approvals_key_j_advances_selection() {
        let mut app = app_with_approvals(&[("act_0", "w"), ("act_1", "w")]);
        handle_approvals_key(KeyCode::Char('j'), &mut app, &TestSource);
        assert_eq!(app.approvals_view.selected_idx, 1, "'j' must advance selection");
    }

    #[test]
    fn approvals_key_k_decrements_selection() {
        let mut app = app_with_approvals(&[("act_0", "w"), ("act_1", "w")]);
        app.approvals_view.selected_idx = 1;
        handle_approvals_key(KeyCode::Char('k'), &mut app, &TestSource);
        assert_eq!(app.approvals_view.selected_idx, 0, "'k' must decrement selection");
    }

    #[test]
    fn approvals_key_k_saturates_at_zero() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.selected_idx = 0;
        handle_approvals_key(KeyCode::Char('k'), &mut app, &TestSource);
        assert_eq!(app.approvals_view.selected_idx, 0, "k at 0 must not underflow");
    }

    #[test]
    fn approvals_key_j_saturates_at_last() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        handle_approvals_key(KeyCode::Char('j'), &mut app, &TestSource);
        assert_eq!(app.approvals_view.selected_idx, 0, "j at last must not overflow");
    }

    #[test]
    fn approvals_confirm_r_enters_reject_reason_mode() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(KeyCode::Char('r'), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::RejectReason,
            "'r' in Confirm mode must switch to RejectReason mode");
        assert!(app.approvals_view.reject_reason.is_empty(),
            "reject_reason must be cleared when entering RejectReason mode");
    }

    #[test]
    fn approvals_confirm_esc_returns_to_list() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(KeyCode::Esc, &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "Esc in Confirm mode must return to List mode (not Dashboard)");
        assert_eq!(app.view, View::Approvals);
    }

    #[test]
    fn approvals_reject_reason_char_appends_to_reason() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        handle_approvals_key(KeyCode::Char('x'), &mut app, &TestSource);
        assert_eq!(app.approvals_view.reject_reason, "x");
    }

    #[test]
    fn approvals_reject_reason_backspace_pops_char() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        app.approvals_view.reject_reason = "foo".to_string();
        handle_approvals_key(KeyCode::Backspace, &mut app, &TestSource);
        assert_eq!(app.approvals_view.reject_reason, "fo");
    }

    #[test]
    fn approvals_reject_reason_esc_cancels_to_confirm() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        app.approvals_view.reject_reason = "partial".to_string();
        handle_approvals_key(KeyCode::Esc, &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::Confirm,
            "Esc in RejectReason mode must return to Confirm (not List)");
        assert!(app.approvals_view.reject_reason.is_empty(),
            "reject_reason must be cleared on Esc cancel");
    }

    #[test]
    fn approvals_reject_reason_enter_with_no_control_file_sets_error_msg() {
        // TestSource.deny() returns Err("mock: no control") → result_msg is set.
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        app.approvals_view.reject_reason = "too risky".to_string();
        handle_approvals_key(KeyCode::Enter, &mut app, &TestSource);
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
        handle_approvals_key(KeyCode::Char('a'), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "after Approve must return to List mode");
        assert!(app.approvals_view.result_msg.is_some(),
            "result_msg must be set after approve attempt");
    }

    #[test]
    fn approvals_confirm_dont_ask_again_with_no_control_file_sets_error_msg() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(KeyCode::Char('d'), &mut app, &TestSource);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "after 'don't ask again' must return to List mode");
        assert!(app.approvals_view.result_msg.is_some(),
            "result_msg must be set after don't-ask-again attempt");
    }

    // ── ux.0: step() / event pump ────────────────────────────────────────────
    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
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
        assert_eq!(app.converse_view.input, "q", "'q' must be captured as literal chat input");
        assert_eq!(app.view, View::Dashboard);
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
        let (rx, _wake, _producers) =
            crate::watch::pump::spawn_producers(src, Duration::from_millis(5));
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
}
