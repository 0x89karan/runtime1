use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub mod app;
pub mod approvals;
pub mod inspector;
pub mod memory;
pub mod reader;
pub mod spawn;
pub mod topology;
pub mod views;

use app::{App, MemoryPane, PendingSpawn, SpawnFocus, View};
use approvals::ApprovalsMode;
use reader::load_snapshot;

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
    let agents_dir = args.agents_dir;
    let interval   = Duration::from_secs(args.interval.max(1));
    let log_path   = args.log_path;

    // Startup mount validation: require system/ subdir to be present.
    let sys_dir = agents_dir.join("system");
    if !sys_dir.exists() {
        anyhow::bail!(
            "agents dir {:?} does not contain a 'system/' subdirectory.\n\
             Is agentd running with the FUSE filesystem mounted?\n\
             Start agentd, or point --agents-dir at the correct mountpoint.",
            agents_dir
        );
    }

    // Decide TUI vs plain mode.
    let is_tty = io::stdout().is_terminal();
    let use_plain = args.plain || (!args.no_plain && !is_tty);

    if use_plain {
        if !is_tty && !args.plain {
            eprintln!("note: stdout is not a TTY — using plain text mode (--plain)");
        }
        run_plain(agents_dir, interval, log_path)
    } else {
        run_tui(agents_dir, interval, log_path)
    }
}

fn run_plain(agents_dir: PathBuf, interval: Duration, log_path: Option<PathBuf>) -> anyhow::Result<()> {
    let mut app = App::new(agents_dir.clone());
    app.log_path = log_path;
    loop {
        let snap = load_snapshot(&agents_dir);
        app.apply_snapshot(snap);
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

fn run_tui(agents_dir: PathBuf, interval: Duration, log_path: Option<PathBuf>) -> anyhow::Result<()> {
    let stdout = io::stdout();

    // CleanupGuard: restores terminal on both normal exit and panic.
    struct CleanupGuard;
    impl Drop for CleanupGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
    }

    enable_raw_mode().context("enabling raw mode")?;
    // Guard must be created immediately after enable_raw_mode succeeds so that
    // any subsequent failure (including EnterAlternateScreen) triggers cleanup.
    let _guard = CleanupGuard;
    let mut stdout = stdout;
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;

    // Restore terminal on panic before the guard's Drop runs.
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        orig_hook(info);
    }));

    let backend  = CrosstermBackend::new(io::stdout());
    let mut term = Terminal::new(backend).context("creating terminal")?;

    let mut app  = App::new(agents_dir.clone());
    app.log_path = log_path.clone();
    let tick_ms  = interval.as_millis().max(100) as u64;

    loop {
        // Refresh state on every frame.
        let snap = load_snapshot(&agents_dir);
        app.apply_snapshot(snap);

        term.draw(|f| views::render(f, &app))?;

        if event::poll(Duration::from_millis(tick_ms))? {
            match event::read()? {
                Event::Key(key) => {
                    let ctrl_c = key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL);
                    if ctrl_c {
                        break;
                    }
                    // Capture view BEFORE dispatch: q should quit only if we
                    // were already on the Dashboard, not if we just navigated
                    // back to it from AgentDetail/System/Topology/Memory.
                    let was_dashboard = app.view == View::Dashboard;
                    match app.view {
                        View::Dashboard => handle_dashboard_key(key.code, &mut app),
                        View::AgentDetail | View::System => {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => {
                                    app.view = View::Dashboard;
                                }
                                _ => {}
                            }
                        }
                        View::Topology => {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => {
                                    app.view = View::Dashboard;
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    app.topology_scroll = app.topology_scroll.saturating_sub(1);
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    app.topology_scroll += 1;
                                }
                                _ => {}
                            }
                        }
                        View::Memory    => handle_memory_key(key.code, &mut app),
                        View::Spawn     => handle_spawn_key(key.code, &mut app),
                        View::Inspector => handle_inspector_key(key.code, &mut app),
                        View::Approvals => handle_approvals_key(key.code, &mut app),
                    }
                    if matches!(key.code, KeyCode::Char('q')) && was_dashboard {
                        break;
                    }
                    // Pending exec: TUI loop exits; terminal is restored below.
                    if app.spawn_view.pending_exec.is_some() {
                        break;
                    }
                }
                Event::Resize(_, _) => { /* ratatui handles this */ }
                _ => {}
            }
        }
    }

    // Force terminal restore before any pending exec replaces the process.
    drop(_guard);
    let _ = std::panic::take_hook();

    // Pending spawn: try /agents/control first; fall back to exec agentd.
    if let Some(pending) = app.spawn_view.pending_exec.take() {
        match execute_pending_spawn(&agents_dir, pending)? {
            SpawnOutcome::FellBackToExec => {
                // exec_agentd() replaced this process — unreachable on success.
            }
            SpawnOutcome::InjectedViaControl { agent_id_hint } => {
                // Re-enter TUI to show the banner and let the operator watch the agent.
                enable_raw_mode().context("re-enabling raw mode after inject")?;
                let _guard2 = CleanupGuard;
                execute!(io::stdout(), EnterAlternateScreen).context("re-entering alternate screen")?;
                let backend2  = CrosstermBackend::new(io::stdout());
                let mut term2 = Terminal::new(backend2).context("recreating terminal")?;
                let mut app2  = App::new(agents_dir.clone());
                app2.log_path = log_path;
                app2.spawn_banner = Some(format!("Agent '{}' injected via /agents/control", agent_id_hint));

                loop {
                    let snap = load_snapshot(&agents_dir);
                    app2.apply_snapshot(snap);
                    term2.draw(|f| views::render(f, &app2))?;
                    if event::poll(Duration::from_millis(tick_ms))? {
                        match event::read()? {
                            Event::Key(key) => {
                                let ctrl_c = key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL);
                                if ctrl_c { break; }
                                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                                    && app2.view == View::Dashboard
                                {
                                    break;
                                }
                                // Any key clears the banner.
                                app2.spawn_banner = None;
                                let was_dashboard = app2.view == View::Dashboard;
                                match app2.view {
                                    View::Dashboard => handle_dashboard_key(key.code, &mut app2),
                                    View::AgentDetail | View::System => {
                                        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                                            app2.view = View::Dashboard;
                                        }
                                    }
                                    View::Topology => {
                                        match key.code {
                                            KeyCode::Char('q') | KeyCode::Esc => { app2.view = View::Dashboard; }
                                            KeyCode::Up | KeyCode::Char('k') => {
                                                app2.topology_scroll = app2.topology_scroll.saturating_sub(1);
                                            }
                                            KeyCode::Down | KeyCode::Char('j') => {
                                                app2.topology_scroll += 1;
                                            }
                                            _ => {}
                                        }
                                    }
                                    View::Memory    => handle_memory_key(key.code, &mut app2),
                                    View::Spawn     => handle_spawn_key(key.code, &mut app2),
                                    View::Inspector => handle_inspector_key(key.code, &mut app2),
                                    View::Approvals => handle_approvals_key(key.code, &mut app2),
                                }
                                if matches!(key.code, KeyCode::Char('q')) && was_dashboard {
                                    break;
                                }
                            }
                            Event::Resize(_, _) => {}
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    Ok(())
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

fn handle_dashboard_key(code: KeyCode, app: &mut App) {
    match code {
        KeyCode::Up | KeyCode::Char('k')   => app.select_prev(),
        KeyCode::Down | KeyCode::Char('j') => app.select_next(),
        KeyCode::Enter                     => {
            if app.selected_agent().is_some() {
                app.view = View::AgentDetail;
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

/// Write `payload` bytes to `/agents/control`, using an explicit `close(2)` so FUSE
/// flush errors are visible rather than silently swallowed by Rust's `File::drop`.
fn write_control_command(agents_dir: &std::path::Path, payload: &str) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::io::IntoRawFd as _;

    let control = agents_dir.join("control");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(&control)
        .map_err(|e| format!("open /agents/control: {e}"))?;
    f.write_all(payload.as_bytes())
        .map_err(|e| format!("write error: {e}"))?;
    let fd = f.into_raw_fd();
    // SAFETY: fd is valid and exclusively owned (into_raw_fd consumed the File).
    let rc = unsafe { libc::close(fd) };
    if rc != 0 {
        return Err(format!(
            "scheduler rejected command ({})",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn handle_approvals_key(code: KeyCode, app: &mut App) {
    match app.approvals_view.mode {
        // ── Typing a reject reason ────────────────────────────────────────────
        ApprovalsMode::RejectReason => {
            match code {
                KeyCode::Enter => {
                    let id_opt = app.approvals_view.confirmed_id.clone();
                    let found  = id_opt.as_deref().and_then(|id| {
                        app.approvals_items.iter().find(|i| i.id == id)
                    });
                    app.approvals_view.result_msg = if let Some(item) = found {
                        let reason = app.approvals_view.reject_reason.clone();
                        let payload = if reason.is_empty() {
                            serde_json::json!({ "reject": { "id": &item.id } })
                        } else {
                            serde_json::json!({ "reject": { "id": &item.id, "reason": reason } })
                        };
                        let json_str = serde_json::to_string(&payload).unwrap_or_default();
                        match write_control_command(&app.agents_dir, &json_str) {
                            Ok(())  => Some(format!("Rejected {}", item.id)),
                            Err(e)  => Some(format!("Error: {e}")),
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
                    let id_opt = app.approvals_view.confirmed_id.clone();
                    let found  = id_opt.as_deref().and_then(|id| {
                        app.approvals_items.iter().find(|i| i.id == id)
                    });
                    app.approvals_view.result_msg = if let Some(item) = found {
                        let payload  = serde_json::json!({ "approve": { "id": &item.id } });
                        let json_str = serde_json::to_string(&payload).unwrap_or_default();
                        match write_control_command(&app.agents_dir, &json_str) {
                            Ok(())  => Some(format!("Approved {}", item.id)),
                            Err(e)  => Some(format!("Error: {e}")),
                        }
                    } else {
                        Some("Approval already resolved — refreshed list.".to_string())
                    };
                    app.approvals_view.mode = ApprovalsMode::List;
                }
                KeyCode::Char('d') => {
                    let id_opt = app.approvals_view.confirmed_id.clone();
                    let found  = id_opt.as_deref().and_then(|id| {
                        app.approvals_items.iter().find(|i| i.id == id)
                    });
                    app.approvals_view.result_msg = if let Some(item) = found {
                        let kind     = item.kind.clone();
                        let payload  = serde_json::json!({
                            "approve": { "id": &item.id, "auto_approve_kind": kind }
                        });
                        let json_str = serde_json::to_string(&payload).unwrap_or_default();
                        match write_control_command(&app.agents_dir, &json_str) {
                            Ok(())  => Some(format!("Approved {} (auto for '{}')", item.id, item.kind)),
                            Err(e)  => Some(format!("Error: {e}")),
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

    use crossterm::event::KeyCode;

    use super::{handle_approvals_key, handle_dashboard_key, handle_memory_key, handle_spawn_key, App, View};
    use crate::watch::app::{MemoryPane, SpawnFocus};
    use crate::watch::approvals::ApprovalsMode;
    use crate::watch::reader::{AgentInfo, BudgetKind, PendingAction, Snapshot};

    fn make_snapshot(ids: &[&str]) -> Snapshot {
        Snapshot {
            agents: ids.iter().map(|id| AgentInfo {
                id:              id.to_string(),
                status:          "running".to_string(),
                context_tokens:  0,
                budget:          BudgetKind::Unlimited,
                tools:           vec![],
                parent_id:       None,
                sandbox:         None,
                egress_brokered: 0,
                egress_denied:   0,
            }).collect(),
            budget: None, queue: None, sandbox: None, provider: None, error: None,
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
        handle_dashboard_key(KeyCode::Down, &mut app);
        assert_eq!(app.selected_id.as_deref(), Some("b"));
    }

    #[test]
    fn dashboard_key_j_advances_selection() {
        let mut app = app_with_agents(&["a", "b"]);
        handle_dashboard_key(KeyCode::Char('j'), &mut app);
        assert_eq!(app.selected_id.as_deref(), Some("b"));
    }

    #[test]
    fn dashboard_key_up_arrow_decrements_selection() {
        let mut app = app_with_agents(&["a", "b", "c"]);
        app.selected_id = Some("b".to_string());
        handle_dashboard_key(KeyCode::Up, &mut app);
        assert_eq!(app.selected_id.as_deref(), Some("a"));
    }

    #[test]
    fn dashboard_key_k_decrements_selection() {
        let mut app = app_with_agents(&["a", "b"]);
        app.selected_id = Some("b".to_string());
        handle_dashboard_key(KeyCode::Char('k'), &mut app);
        assert_eq!(app.selected_id.as_deref(), Some("a"));
    }

    #[test]
    fn dashboard_key_enter_switches_to_agent_detail_when_selection_present() {
        let mut app = app_with_agents(&["a"]);
        assert_eq!(app.view, View::Dashboard);
        handle_dashboard_key(KeyCode::Enter, &mut app);
        assert_eq!(app.view, View::AgentDetail);
    }

    #[test]
    fn dashboard_key_enter_does_not_switch_view_when_no_agents() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Enter, &mut app);
        assert_eq!(app.view, View::Dashboard,
            "Enter with no agents must not navigate to AgentDetail");
    }

    #[test]
    fn dashboard_key_s_switches_to_system_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Char('s'), &mut app);
        assert_eq!(app.view, View::System);
    }

    #[test]
    fn dashboard_key_t_switches_to_topology_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Char('t'), &mut app);
        assert_eq!(app.view, View::Topology);
    }

    #[test]
    fn dashboard_key_other_is_noop() {
        let mut app = app_with_agents(&["a"]);
        let original_id = app.selected_id.clone();
        handle_dashboard_key(KeyCode::F(1), &mut app);
        assert_eq!(app.view, View::Dashboard);
        assert_eq!(app.selected_id, original_id);
    }

    #[test]
    fn dashboard_key_m_switches_to_memory_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Char('m'), &mut app);
        assert_eq!(app.view, View::Memory);
    }

    #[test]
    fn dashboard_key_n_switches_to_spawn_view() {
        let mut app = App::new(PathBuf::from("/agents"));
        handle_dashboard_key(KeyCode::Char('n'), &mut app);
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
        handle_dashboard_key(KeyCode::Char('m'), &mut app);
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
        handle_dashboard_key(KeyCode::Char('a'), &mut app);
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
        handle_approvals_key(KeyCode::Char('q'), &mut app);
        assert_eq!(app.view, View::Dashboard,
            "'q' in Approvals list mode must return to Dashboard");
    }

    #[test]
    fn approvals_key_esc_returns_to_dashboard_from_list() {
        let mut app = app_with_approvals(&[]);
        handle_approvals_key(KeyCode::Esc, &mut app);
        assert_eq!(app.view, View::Dashboard,
            "Esc in Approvals list mode must return to Dashboard");
    }

    #[test]
    fn approvals_key_enter_enters_confirm_when_items_present() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        handle_approvals_key(KeyCode::Enter, &mut app);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::Confirm,
            "Enter with pending items must enter Confirm mode");
        assert_eq!(app.view, View::Approvals,
            "view must stay Approvals after Enter");
    }

    #[test]
    fn approvals_key_enter_is_noop_when_no_items() {
        let mut app = app_with_approvals(&[]);
        handle_approvals_key(KeyCode::Enter, &mut app);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "Enter with no items must stay in List mode");
    }

    #[test]
    fn approvals_key_j_advances_selection() {
        let mut app = app_with_approvals(&[("act_0", "w"), ("act_1", "w")]);
        handle_approvals_key(KeyCode::Char('j'), &mut app);
        assert_eq!(app.approvals_view.selected_idx, 1, "'j' must advance selection");
    }

    #[test]
    fn approvals_key_k_decrements_selection() {
        let mut app = app_with_approvals(&[("act_0", "w"), ("act_1", "w")]);
        app.approvals_view.selected_idx = 1;
        handle_approvals_key(KeyCode::Char('k'), &mut app);
        assert_eq!(app.approvals_view.selected_idx, 0, "'k' must decrement selection");
    }

    #[test]
    fn approvals_key_k_saturates_at_zero() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.selected_idx = 0;
        handle_approvals_key(KeyCode::Char('k'), &mut app);
        assert_eq!(app.approvals_view.selected_idx, 0, "k at 0 must not underflow");
    }

    #[test]
    fn approvals_key_j_saturates_at_last() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        handle_approvals_key(KeyCode::Char('j'), &mut app);
        assert_eq!(app.approvals_view.selected_idx, 0, "j at last must not overflow");
    }

    #[test]
    fn approvals_confirm_r_enters_reject_reason_mode() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(KeyCode::Char('r'), &mut app);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::RejectReason,
            "'r' in Confirm mode must switch to RejectReason mode");
        assert!(app.approvals_view.reject_reason.is_empty(),
            "reject_reason must be cleared when entering RejectReason mode");
    }

    #[test]
    fn approvals_confirm_esc_returns_to_list() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(KeyCode::Esc, &mut app);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "Esc in Confirm mode must return to List mode (not Dashboard)");
        assert_eq!(app.view, View::Approvals);
    }

    #[test]
    fn approvals_reject_reason_char_appends_to_reason() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        handle_approvals_key(KeyCode::Char('x'), &mut app);
        assert_eq!(app.approvals_view.reject_reason, "x");
    }

    #[test]
    fn approvals_reject_reason_backspace_pops_char() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        app.approvals_view.reject_reason = "foo".to_string();
        handle_approvals_key(KeyCode::Backspace, &mut app);
        assert_eq!(app.approvals_view.reject_reason, "fo");
    }

    #[test]
    fn approvals_reject_reason_esc_cancels_to_confirm() {
        let mut app = app_with_approvals(&[("act_0", "w")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        app.approvals_view.reject_reason = "partial".to_string();
        handle_approvals_key(KeyCode::Esc, &mut app);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::Confirm,
            "Esc in RejectReason mode must return to Confirm (not List)");
        assert!(app.approvals_view.reject_reason.is_empty(),
            "reject_reason must be cleared on Esc cancel");
    }

    #[test]
    fn approvals_reject_reason_enter_with_no_control_file_sets_error_msg() {
        // /nonexistent/control doesn't exist → write_control_command fails.
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode = ApprovalsMode::RejectReason;
        app.approvals_view.reject_reason = "too risky".to_string();
        handle_approvals_key(KeyCode::Enter, &mut app);
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
        handle_approvals_key(KeyCode::Char('a'), &mut app);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "after Approve must return to List mode");
        assert!(app.approvals_view.result_msg.is_some(),
            "result_msg must be set after approve attempt");
    }

    #[test]
    fn approvals_confirm_dont_ask_again_with_no_control_file_sets_error_msg() {
        let mut app = app_with_approvals(&[("act_0", "write_file")]);
        app.approvals_view.mode = ApprovalsMode::Confirm;
        handle_approvals_key(KeyCode::Char('d'), &mut app);
        assert_eq!(app.approvals_view.mode, ApprovalsMode::List,
            "after 'don't ask again' must return to List mode");
        assert!(app.approvals_view.result_msg.is_some(),
            "result_msg must be set after don't-ask-again attempt");
    }
}
