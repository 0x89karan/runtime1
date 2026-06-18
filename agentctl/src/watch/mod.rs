use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
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
pub mod memory;
pub mod reader;
pub mod topology;
pub mod views;

use app::{App, MemoryPane, View};
use reader::load_snapshot;

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
    app.log_path = log_path;
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
                        View::Memory => handle_memory_key(key.code, &mut app),
                    }
                    if matches!(key.code, KeyCode::Char('q')) && was_dashboard {
                        break;
                    }
                }
                Event::Resize(_, _) => { /* ratatui handles this */ }
                _ => {}
            }
        }
    }

    // Restore the original panic hook — the terminal-restore hook is no longer
    // needed once the event loop exits; CleanupGuard::drop handles cleanup.
    let _ = std::panic::take_hook();

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
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crossterm::event::KeyCode;

    use super::{handle_dashboard_key, handle_memory_key, App, View};
    use crate::watch::app::MemoryPane;
    use crate::watch::reader::{AgentInfo, BudgetKind, Snapshot};

    fn make_snapshot(ids: &[&str]) -> Snapshot {
        Snapshot {
            agents: ids.iter().map(|id| AgentInfo {
                id:             id.to_string(),
                status:         "running".to_string(),
                context_tokens: 0,
                budget:         BudgetKind::Unlimited,
                tools:          vec![],
                parent_id:      None,
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
}
