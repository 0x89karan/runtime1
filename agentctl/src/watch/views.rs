use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

use super::app::{App, View};

/// Strip ASCII control characters (< 0x20, except tab) from a string before
/// rendering it in a TUI widget or plain-text output. Guards against ANSI
/// escape sequences embedded in OS error messages.
fn sanitize(s: &str) -> String {
    s.chars().filter(|&c| c >= ' ' || c == '\t').collect()
}

pub fn render(f: &mut Frame, app: &App) {
    match app.view {
        View::Dashboard   => render_dashboard(f, app),
        View::AgentDetail => render_agent_detail(f, app),
        View::System      => render_system(f, app),
    }
}

fn header_footer_layout(area: Rect) -> (Rect, Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // header bar
            Constraint::Min(1),     // main content
            Constraint::Length(1),  // footer / key hints
        ])
        .split(area);
    (chunks[0], chunks[1], chunks[2])
}

fn status_style(status: &str) -> Style {
    match status {
        s if s.starts_with("running")         => Style::default().fg(Color::Green),
        s if s.starts_with("deferred")        => Style::default().fg(Color::Yellow),
        s if s.starts_with("awaiting_child")  => Style::default().fg(Color::Cyan),
        s if s.starts_with("done")            => Style::default().fg(Color::Blue),
        s if s.starts_with("failed")          => Style::default().fg(Color::Red),
        _                                     => Style::default(),
    }
}

fn render_dashboard(f: &mut Frame, app: &App) {
    let (header_area, content_area, footer_area) = header_footer_layout(f.area());

    // Header
    let title = match app.provider.as_ref().map(|p| p.model.as_str()) {
        Some(m) if !m.is_empty() => format!(" agentctl watch  │  model: {m} "),
        _                        => " agentctl watch ".to_string(),
    };
    f.render_widget(
        Paragraph::new(title).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    // Agent table
    let selected_idx = app.selected_index();
    let header_row = Row::new(vec![
        Cell::from("Agent ID").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Status").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Context").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Budget").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Tools").style(Style::default().add_modifier(Modifier::BOLD)),
    ]).style(Style::default().bg(Color::DarkGray));

    let rows: Vec<Row> = app.agents.iter().enumerate().map(|(i, a)| {
        let is_sel = selected_idx == Some(i);
        let bg = if is_sel { Color::Blue } else { Color::Reset };
        Row::new(vec![
            Cell::from(a.id.clone()),
            Cell::from(a.status.clone()).style(status_style(&a.status)),
            Cell::from(format!("{}", a.context_tokens)),
            Cell::from(a.budget.display()),
            Cell::from(format!("{}", a.tools.len())),
        ]).style(Style::default().bg(bg))
    }).collect();

    if app.agents.is_empty() {
        let msg = app.error.as_deref()
            .map(|e| format!("error: {}", sanitize(e)))
            .unwrap_or_else(|| "no agents running".to_string());
        f.render_widget(
            Paragraph::new(msg)
                .block(Block::default().borders(Borders::ALL).title(" agents ")),
            content_area,
        );
    } else {
        let table = Table::new(
            rows,
            [
                Constraint::Min(20),     // Agent ID
                Constraint::Length(20),  // Status
                Constraint::Length(10),  // Context
                Constraint::Length(12),  // Budget
                Constraint::Length(6),   // Tools
            ],
        )
        .header(header_row)
        .block(Block::default().borders(Borders::ALL).title(" agents "));
        f.render_widget(table, content_area);
    }

    // Footer
    let hints = " ↑/↓ select  Enter detail  s system  q quit ";
    f.render_widget(
        Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        footer_area,
    );
}

fn render_agent_detail(f: &mut Frame, app: &App) {
    let (header_area, content_area, footer_area) = header_footer_layout(f.area());

    let agent = match app.selected_agent() {
        Some(a) => a,
        None    => { render_dashboard(f, app); return; }
    };

    let title = format!(" agent: {} ", agent.id);
    f.render_widget(
        Paragraph::new(title).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    let tools_str = if agent.tools.is_empty() {
        "(none)".to_string()
    } else {
        agent.tools.join(", ")
    };
    let lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("  Status:   ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(agent.status.clone(), status_style(&agent.status)),
        ]),
        Line::from(format!("  Context:  {} tokens", agent.context_tokens)),
        Line::from(format!("  Budget:   {}", agent.budget.display())),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Tools:    ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(tools_str),
        ]),
    ];

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" detail ")),
        content_area,
    );

    let hints = " Esc/q back to dashboard ";
    f.render_widget(
        Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        footer_area,
    );
}

fn render_system(f: &mut Frame, app: &App) {
    let (header_area, content_area, footer_area) = header_footer_layout(f.area());

    f.render_widget(
        Paragraph::new(" agentctl watch › system ")
            .style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    let spent   = app.budget.as_ref().map(|b| b.spent).unwrap_or(0);
    let depth   = app.queue.as_ref().map(|q| q.depth).unwrap_or(0);
    let sandbox = app.sandbox.as_ref().map(|s| s.applied).unwrap_or(false);
    let model   = app.provider.as_ref().map(|p| p.model.as_str()).unwrap_or("unknown");
    let backend = app.provider.as_ref().map(|p| p.backend.as_str()).unwrap_or("unknown");

    let lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Provider:  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{model} ({backend})")),
        ]),
        Line::from(vec![
            Span::styled("  Tokens:    ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{spent} spent")),
        ]),
        Line::from(vec![
            Span::styled("  Queue:     ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("{depth} deferred")),
        ]),
        Line::from(vec![
            Span::styled("  Sandbox:   ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(if sandbox { "applied" } else { "none" }),
        ]),
    ];

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" system ")),
        content_area,
    );

    let hints = " Esc/q back to dashboard ";
    f.render_widget(
        Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        footer_area,
    );
}

/// Render a plain-text snapshot to a string (for --plain mode, no ANSI).
pub fn render_plain(app: &App) -> String {
    let mut out = String::new();
    if let Some(ref e) = app.error {
        out.push_str(&format!("error: {}\n", sanitize(e)));
    }
    if let Some(p) = app.provider.as_ref() {
        out.push_str(&format!("provider: {} ({})\n", p.model, p.backend));
    }
    if let Some(b) = app.budget.as_ref() {
        out.push_str(&format!("tokens_spent: {}\n", b.spent));
    }
    if let Some(q) = app.queue.as_ref() {
        out.push_str(&format!("queue_depth: {}\n", q.depth));
    }
    if app.agents.is_empty() {
        out.push_str("agents: (none)\n");
    } else {
        out.push_str(&format!("agents: {}\n", app.agents.len()));
        for a in &app.agents {
            out.push_str(&format!(
                "  {} [{status}] ctx={ctx} budget={budget} tools={tools}\n",
                a.id,
                status = a.status,
                ctx    = a.context_tokens,
                budget = a.budget.display(),
                tools  = a.tools.len(),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::watch::app::App;
    use crate::watch::reader::{AgentInfo, BudgetKind, Snapshot, SysBudget, SysProvider, SysQueue};

    fn make_agent(id: &str, status: &str, ctx: u64, tools: Vec<String>) -> AgentInfo {
        AgentInfo {
            id:             id.to_string(),
            status:         status.to_string(),
            context_tokens: ctx,
            budget:         BudgetKind::Unlimited,
            tools,
        }
    }

    fn app_from_snap(snap: Snapshot) -> App {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(snap);
        app
    }

    // ── render_plain: empty state ─────────────────────────────────────────────

    #[test]
    fn render_plain_no_agents_outputs_none_line() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("agents: (none)"), "empty agent list must produce '(none)' line");
    }

    #[test]
    fn render_plain_no_provider_omits_provider_line() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(!out.contains("provider:"), "no provider → no provider line");
    }

    #[test]
    fn render_plain_no_budget_omits_tokens_line() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(!out.contains("tokens_spent:"), "no budget → no tokens_spent line");
    }

    // ── render_plain: error field ────────────────────────────────────────────

    #[test]
    fn render_plain_error_appears_in_output() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None,
            error: Some("permission denied".to_string()),
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("error: permission denied"));
    }

    // ── render_plain: provider / budget / queue fields ───────────────────────

    #[test]
    fn render_plain_includes_provider_when_present() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None,
            provider: Some(SysProvider { model: "claude-opus-4".to_string(), backend: "anthropic".to_string() }),
            error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("provider: claude-opus-4 (anthropic)"));
    }

    #[test]
    fn render_plain_includes_tokens_spent_when_budget_present() {
        let snap = Snapshot {
            agents: vec![],
            budget: Some(SysBudget { spent: 99_000, total: 0 }),
            queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("tokens_spent: 99000"));
    }

    #[test]
    fn render_plain_includes_queue_depth_when_queue_present() {
        let snap = Snapshot {
            agents: vec![], budget: None,
            queue: Some(SysQueue { depth: 7 }),
            sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("queue_depth: 7"));
    }

    // ── render_plain: agents list ────────────────────────────────────────────

    #[test]
    fn render_plain_lists_each_agent_with_all_fields() {
        let snap = Snapshot {
            agents: vec![
                make_agent("scout-1", "running", 1500, vec!["read_file".to_string(), "write_file".to_string()]),
            ],
            budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("agents: 1"));
        assert!(out.contains("scout-1"));
        assert!(out.contains("[running]"));
        assert!(out.contains("ctx=1500"));
        assert!(out.contains("budget=unlimited"));
        assert!(out.contains("tools=2"), "tools count must be 2");
    }

    #[test]
    fn render_plain_lists_multiple_agents() {
        let snap = Snapshot {
            agents: vec![
                make_agent("a", "done", 0, vec![]),
                make_agent("b", "failed", 500, vec![]),
            ],
            budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("agents: 2"));
        assert!(out.contains("  a "));
        assert!(out.contains("  b "));
    }

    #[test]
    fn render_plain_agent_with_no_tools_shows_zero() {
        let snap = Snapshot {
            agents: vec![make_agent("agent-x", "running", 0, vec![])],
            budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("tools=0"));
    }

    // ── status_style: coverage via plain-text content (not TUI) ─────────────
    // status_style is a private render helper — we validate the status strings
    // are plumbed through render_plain correctly as a proxy.

    #[test]
    fn render_plain_preserves_agent_status_string() {
        for status in &["running", "deferred", "awaiting_child", "done", "failed", "unknown-xyz"] {
            let snap = Snapshot {
                agents: vec![make_agent("a", status, 0, vec![])],
                budget: None, queue: None, sandbox: None, provider: None, error: None,
            };
            let out = render_plain(&app_from_snap(snap));
            assert!(out.contains(&format!("[{status}]")),
                "status '{status}' must appear in render_plain output");
        }
    }
}
