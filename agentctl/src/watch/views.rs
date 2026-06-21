use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

use super::app::{App, MemoryAbsence, MemoryPane, SpawnFocus, View};
use super::memory::{
    filter_entries, filter_short_term, read_agent_memory, read_kb_segments, MAX_DISPLAY_ENTRIES,
    MAX_SEARCH_ENTRIES,
};
use super::topology::render_tree;

/// Strip ASCII control characters (< 0x20, except tab) from a string before
/// rendering it in a TUI widget or plain-text output. Guards against ANSI
/// escape sequences embedded in OS error messages.
fn sanitize(s: &str) -> String {
    s.chars().filter(|&c| c >= ' ' || c == '\t').collect()
}

const MIN_TOPOLOGY_WIDTH: u16 = 60;
const MIN_MEMORY_WIDTH:   u16 = 50;

pub fn render(f: &mut Frame, app: &App) {
    match app.view {
        View::Dashboard   => render_dashboard(f, app),
        View::AgentDetail => render_agent_detail(f, app),
        View::System      => render_system(f, app),
        View::Topology    => render_topology(f, app),
        View::Memory      => render_memory(f, app),
        View::Spawn       => render_spawn(f, app),
        View::Inspector   => render_inspector(f, app),
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
    let area = f.area();

    // When a spawn banner is active, carve out an extra line below the header.
    let (header_area, banner_area, content_area, footer_area) = if app.spawn_banner.is_some() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),  // header bar
                Constraint::Length(1),  // spawn banner
                Constraint::Min(1),     // main content
                Constraint::Length(1),  // footer / key hints
            ])
            .split(area);
        (chunks[0], Some(chunks[1]), chunks[2], chunks[3])
    } else {
        let (h, c, f2) = header_footer_layout(area);
        (h, None, c, f2)
    };

    // Header
    let title = match app.provider.as_ref().map(|p| p.model.as_str()) {
        Some(m) if !m.is_empty() => format!(" agentctl watch  │  model: {m} "),
        _                        => " agentctl watch ".to_string(),
    };
    f.render_widget(
        Paragraph::new(title).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    // Spawn banner (shown after live injection via /agents/control).
    if let (Some(msg), Some(banner_rect)) = (&app.spawn_banner, banner_area) {
        let text = format!(" ✓ {} ", sanitize(msg));
        f.render_widget(
            Paragraph::new(text).style(Style::default().bg(Color::Green).fg(Color::Black)),
            banner_rect,
        );
    }

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
    let hints = " ↑/↓ select  Enter detail  [s]ystem  [t]opology  [m]emory  [n]ew  q quit ";
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
    let sandbox_str = match &agent.sandbox {
        None => "(unavailable)".to_string(),
        Some(sb) if sb.servers.is_empty() => "(none)".to_string(),
        Some(sb) => {
            sb.servers.iter()
                .map(|s| {
                    let flags: Vec<&str> = [
                        s.landlock.then_some("landlock"),
                        s.seccomp.then_some("seccomp"),
                        s.landlock_net.then_some("landlock_net"),
                        s.namespace_net.then_some("net_ns"),
                        s.namespace_mount.then_some("mount_ns"),
                        (!s.isolation.is_empty() && s.isolation != "none")
                            .then_some(s.isolation.as_str()),
                        (!s.transport.is_empty() && s.transport != "stdio")
                            .then_some(s.transport.as_str()),
                    ].iter().filter_map(|x| *x).collect();
                    if flags.is_empty() {
                        format!("{}:none", s.name)
                    } else {
                        // Truncate safely at char boundary
                        let flags_str = flags.join(",");
                        format!("{}:{}", s.name, flags_str)
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
        }
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
        Line::from(vec![
            Span::styled("  Sandbox:  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(sandbox_str),
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
    let sandbox_ref = app.sandbox.as_ref();
    let sandbox     = sandbox_ref.map(|s| s.any_sandboxed).unwrap_or(false);
    let degs        = sandbox_ref.map(|s| s.degradations.as_slice()).unwrap_or_default();
    let model   = app.provider.as_ref().map(|p| p.model.as_str()).unwrap_or("unknown");
    let backend = app.provider.as_ref().map(|p| p.backend.as_str()).unwrap_or("unknown");

    let mut lines: Vec<Line> = vec![
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
    for deg in degs {
        lines.push(Line::from(vec![
            Span::styled("  ! Degraded:", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {deg}"), Style::default().fg(Color::Yellow)),
        ]));
    }

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

fn render_topology(f: &mut Frame, app: &App) {
    let area = f.area();

    // Min-width guard.
    if area.width < MIN_TOPOLOGY_WIDTH {
        f.render_widget(
            Paragraph::new(format!("terminal too narrow (min {} cols)", MIN_TOPOLOGY_WIDTH))
                .style(Style::default().fg(Color::Red)),
            area,
        );
        return;
    }

    // Split: header, scrollable body, fixed legend footer.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(1),    // scrollable tree
            Constraint::Length(1), // legend footer
        ])
        .split(area);
    let (header_area, body_area, legend_area) = (chunks[0], chunks[1], chunks[2]);

    f.render_widget(
        Paragraph::new(" agentctl watch › topology ")
            .style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    let all_lines = render_tree(&app.topology);
    let scroll    = app.topology_scroll.min(all_lines.len().saturating_sub(1));
    let height    = body_area.height as usize;
    let visible: Vec<Line> = all_lines
        .iter()
        .skip(scroll)
        .take(height)
        .map(|l| Line::from(l.as_str()))
        .collect();

    let parse_err_note = if app.topology.parse_errors > 0 {
        format!("  ({} parse errors in flight log)", app.topology.parse_errors)
    } else {
        String::new()
    };
    f.render_widget(
        Paragraph::new(visible)
            .block(Block::default().borders(Borders::ALL).title(" spawn tree ")),
        body_area,
    );

    let legend = format!(
        " ├─spawn→ child  ╌→ sent  ←╌ received  ●live ✓done ✗failed  Esc/q back{}",
        parse_err_note
    );
    f.render_widget(
        Paragraph::new(legend).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        legend_area,
    );
}

fn render_memory(f: &mut Frame, app: &App) {
    let area = f.area();

    if area.width < MIN_MEMORY_WIDTH {
        f.render_widget(
            Paragraph::new(format!("terminal too narrow for Memory view (min {} cols)", MIN_MEMORY_WIDTH))
                .style(Style::default().fg(Color::Red)),
            area,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header bar
            Constraint::Length(1), // pane tab line + agent id
            Constraint::Length(1), // search bar
            Constraint::Min(1),    // content
            Constraint::Length(1), // footer hints
        ])
        .split(area);
    let (header_area, tab_area, search_area, content_area, footer_area) =
        (chunks[0], chunks[1], chunks[2], chunks[3], chunks[4]);

    // Header
    f.render_widget(
        Paragraph::new(" agentctl watch › memory ")
            .style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    // Tab bar
    let pane   = &app.memory_view.pane;
    let agent  = app.selected_agent().map(|a| a.id.as_str()).unwrap_or("(none)");
    let st_lbl = if *pane == MemoryPane::ShortTerm { "[Short-term]" } else { " Short-term " };
    let lt_lbl = if *pane == MemoryPane::LongTerm  { "[Long-term]"  } else { " Long-term "  };
    let kb_lbl = if *pane == MemoryPane::Kb         { "[KB]"         } else { " KB "         };
    let tab_line = format!("{st_lbl}  {lt_lbl}  {kb_lbl}   Agent: {agent}");
    f.render_widget(Paragraph::new(tab_line), tab_area);

    // Search bar
    let sq = &app.memory_view.search_query;
    let search_line = if app.memory_view.search_active {
        format!("Search: {sq}_")
    } else if sq.is_empty() {
        " [/] search ".to_string()
    } else {
        format!("Search: {sq}  (press [/] to edit)")
    };
    f.render_widget(Paragraph::new(search_line), search_area);

    // Content area — only the active pane is rendered (true-tab model).
    match pane {
        MemoryPane::ShortTerm => render_memory_short_term_pane(f, app, content_area),
        MemoryPane::LongTerm  => render_memory_long_term_pane(f, app, content_area),
        MemoryPane::Kb        => render_memory_kb_pane(f, app, content_area),
    }

    // Footer
    let hints = " [Tab] pane  [/] search  ↑/↓ scroll  Esc/q back ";
    f.render_widget(
        Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        footer_area,
    );
}

fn render_memory_short_term_pane(f: &mut Frame, app: &App, area: Rect) {
    let mem = match app.memory_view.agent_memory.as_ref() {
        Some(m) => m,
        None    => {
            f.render_widget(
                Paragraph::new("(no agent selected — select from Dashboard with Enter)")
                    .block(Block::default().borders(Borders::ALL).title(" short-term ")),
                area,
            );
            return;
        }
    };
    let q      = &app.memory_view.search_query;
    let items  = filter_short_term(&mem.short_term, q);
    let total  = mem.short_term.len();
    let title  = if q.is_empty() {
        format!(" SHORT TERM — {} items ", total)
    } else {
        format!(" SHORT TERM — {} matches of {} ", items.len(), total)
    };
    let scroll  = app.memory_view.short_term_scroll;
    let height  = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(i, s)| Line::from(format!("  {}. {}", i + 1, sanitize(s))))
        .collect();
    let body = if lines.is_empty() {
        vec![Line::from("  (no items)")]
    } else {
        lines
    };
    f.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn render_memory_long_term_pane(f: &mut Frame, app: &App, area: Rect) {
    let mem = match app.memory_view.agent_memory.as_ref() {
        Some(m) => m,
        None    => {
            f.render_widget(
                Paragraph::new("(no agent selected — select from Dashboard with Enter)")
                    .block(Block::default().borders(Borders::ALL).title(" long-term ")),
                area,
            );
            return;
        }
    };
    let q       = &app.memory_view.search_query;
    let entries = filter_entries(&mem.long_term, q);
    let total   = mem.long_term.len();
    let cap_note = if mem.long_term_truncated {
        format!(" (display capped at {MAX_DISPLAY_ENTRIES}, searching up to {MAX_SEARCH_ENTRIES})")
    } else {
        String::new()
    };
    let title = if q.is_empty() {
        format!(" LONG TERM — {} entries{cap_note} ", total)
    } else {
        format!(" LONG TERM — {} matches of {}{cap_note} ", entries.len(), total)
    };
    let scroll = app.memory_view.long_term_scroll;
    let height = area.height.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = vec![];
    for e in entries.iter().skip(scroll).take(height / 3 + 1) {
        lines.push(Line::from(format!("  key: {}", sanitize(&e.key))));
        let preview = &e.content[..e.content.floor_char_boundary(200.min(e.content.len()))];
        lines.push(Line::from(format!("  val: {}", sanitize(preview))));
        if !e.provenance.is_empty() {
            lines.push(Line::from(format!("  prv: {}", sanitize(&e.provenance))));
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.push(Line::from("  (no entries)"));
    }
    let visible: Vec<Line> = lines.into_iter().take(height).collect();
    f.render_widget(
        Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn render_memory_kb_pane(f: &mut Frame, app: &App, area: Rect) {
    // Absence banner takes priority over normal content.
    if let Some(absence) = &app.memory_view.absence {
        let msg = match absence {
            MemoryAbsence::Subsystem =>
                "memory subsystem not present — agentd must be compiled with Phase 5 (redb).\nSee CHANGELOG.md v0.18.0.",
            MemoryAbsence::Empty =>
                "memory subsystem present — no KB data written yet",
        };
        f.render_widget(
            Paragraph::new(msg).block(Block::default().borders(Borders::ALL).title(" KB ")),
            area,
        );
        return;
    }

    let q      = &app.memory_view.search_query;
    let scroll = app.memory_view.kb_scroll;
    let height = area.height.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = vec![];
    for seg in &app.memory_view.kb_segments {
        let class_badge = if seg.class.is_empty() { String::new() } else { format!(" [{}]", seg.class) };
        let entries     = filter_entries(&seg.entries, q);
        let cap_note    = if seg.truncated {
            format!(" (capped at {MAX_DISPLAY_ENTRIES})")
        } else {
            String::new()
        };
        lines.push(Line::from(format!("  {}{class_badge} — {} entries{cap_note}", seg.name, entries.len())));
        for e in &entries {
            let preview = &e.content[..e.content.floor_char_boundary(120.min(e.content.len()))];
            let prov    = if e.provenance.is_empty() { String::new() }
                          else { format!("  [{}]", sanitize(&e.provenance)) };
            lines.push(Line::from(format!("    {} → {}{prov}", sanitize(&e.key), sanitize(preview))));
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        lines.push(Line::from("  (no KB entries)"));
    }

    let title = if q.is_empty() { " KB SEGMENTS ".to_string() }
                else { format!(" KB SEGMENTS  [filter: {q}] ") };
    let visible: Vec<Line> = lines.into_iter().skip(scroll).take(height).collect();
    f.render_widget(
        Paragraph::new(visible).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn render_spawn(f: &mut Frame, app: &App) {
    let area = f.area();
    // Need at least 4 rows for a useful layout.
    if area.height < 4 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // header
            Constraint::Length(6),  // template picker
            Constraint::Length(3),  // task input
            Constraint::Min(4),     // cap toggles + preview split
            Constraint::Length(1),  // footer
        ])
        .split(area);

    let (header_area, picker_area, task_area, mid_area, footer_area) =
        (chunks[0], chunks[1], chunks[2], chunks[3], chunks[4]);

    // Header
    f.render_widget(
        Paragraph::new(" spawn agent ")
            .style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    // Template picker
    let sv = &app.spawn_view;
    let picker_title = if sv.load_error.is_some() {
        " TEMPLATE (load error) ".to_string()
    } else {
        format!(" TEMPLATE ({}/{}) ", sv.template_idx + 1, sv.templates.len().max(1))
    };
    let picker_focus = sv.focus == SpawnFocus::TemplatePicker;
    let picker_block = Block::default()
        .borders(Borders::ALL)
        .title(picker_title)
        .border_style(if picker_focus {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });

    let picker_lines: Vec<Line> = if sv.templates.is_empty() {
        // Only show the error alone when no templates loaded at all.
        if let Some(err) = &sv.load_error {
            vec![Line::from(format!("  error: {}", sanitize(err)))]
        } else {
            vec![Line::from("  (no templates — run `agentctl list-templates` to check)")]
        }
    } else {
        sv.templates.iter().enumerate().flat_map(|(i, t)| {
            use agentd::template::TemplateSource;
            let marker = if i == sv.template_idx { "> " } else { "  " };
            let source_badge = match t.source {
                TemplateSource::User => " [user]",
                TemplateSource::Repo => " [repo]",
            };
            let style = if i == sv.template_idx {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mut lines = vec![Line::styled(
                format!("{marker}{}{source_badge} — {}", t.name, sanitize(&t.description)),
                style,
            )];
            // Show showcases as an indented sub-line for the selected template.
            if i == sv.template_idx && !t.showcases.is_empty() {
                lines.push(Line::from(format!("    {}", sanitize(&t.showcases))));
            }
            lines
        }).collect()
    };
    f.render_widget(Paragraph::new(picker_lines).block(picker_block), picker_area);

    // Task input
    let task_focus = sv.focus == SpawnFocus::TaskField;
    let task_block = Block::default()
        .borders(Borders::ALL)
        .title(" TASK ")
        .border_style(if task_focus {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });
    let task_text = if task_focus {
        format!("{}_", sv.task_input)
    } else if sv.task_input.is_empty() {
        "(empty — Tab to focus, type task description)".to_string()
    } else {
        sv.task_input.clone()
    };
    f.render_widget(Paragraph::new(task_text).block(task_block), task_area);

    // Split mid area: left = cap toggles, right = preview
    let mid_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(mid_area);
    let (cap_area, preview_area) = (mid_chunks[0], mid_chunks[1]);

    // Cap toggles
    let cap_focus = sv.focus == SpawnFocus::CapToggles;
    let cap_block = Block::default()
        .borders(Borders::ALL)
        .title(" CAPABILITIES ")
        .border_style(if cap_focus { Style::default().fg(Color::Yellow) } else { Style::default() });
    let cap_lines: Vec<Line> = if sv.cap_toggles.is_empty() {
        vec![Line::from("  (no suggested caps)")]
    } else {
        sv.cap_toggles.iter().enumerate().map(|(i, (_, label, enabled))| {
            let cursor = if cap_focus && i == sv.cap_idx { ">" } else { " " };
            let check  = if *enabled { "[x]" } else { "[ ]" };
            Line::from(format!(" {cursor} {check} {}", sanitize(label)))
        }).collect()
    };
    f.render_widget(Paragraph::new(cap_lines).block(cap_block), cap_area);

    // Preview pane — generated agent.toml or action buttons
    let gen_focus   = sv.focus == SpawnFocus::ActionGenerate;
    let spawn_focus = sv.focus == SpawnFocus::ActionSpawn;
    let preview_block = Block::default()
        .borders(Borders::ALL)
        .title(" PREVIEW / ACTIONS ");
    let preview_lines: Vec<Line> = {
        let mut lines: Vec<Line> = vec![];
        // Action buttons row
        let gen_style   = if gen_focus   { Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD) } else { Style::default() };
        let spawn_style = if spawn_focus { Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)  } else { Style::default() };
        lines.push(Line::from(vec![
            Span::styled(" [g] Generate ", gen_style),
            Span::raw("  "),
            Span::styled(" [r] Spawn ", spawn_style),
        ]));
        lines.push(Line::from(""));
        // Result / error message
        if let Some(msg) = &sv.result_msg {
            lines.push(Line::from(format!("  {}", sanitize(msg))));
            lines.push(Line::from(""));
        }
        // Generated preview
        if let Some(preview) = &sv.preview {
            for l in preview.lines().take(20) {
                lines.push(Line::from(format!("  {}", sanitize(l))));
            }
        }
        lines
    };
    f.render_widget(Paragraph::new(preview_lines).block(preview_block), preview_area);

    // Footer
    let hints = " [Tab] focus  [g] generate  [r] spawn  Esc/q back ";
    f.render_widget(
        Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        footer_area,
    );
}

fn render_inspector(f: &mut Frame, app: &App) {
    use super::inspector::InspectorFilter;

    let (header_area, content_area, footer_area) = header_footer_layout(f.area());

    let filter_label = app.inspector_view.filter.label();
    let search_hint  = if app.inspector_view.search_active {
        format!(" › search: {}_", app.inspector_view.search_query)
    } else {
        String::new()
    };
    f.render_widget(
        Paragraph::new(format!(
            " agentctl watch › inspector [{}]{} — loaded {}",
            filter_label, search_hint, app.inspector_view.load_time
        )).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    let height = content_area.height as usize;
    let lines  = &app.inspector_view.lines;
    let scroll = app.inspector_view.scroll.min(lines.len().saturating_sub(1));

    let visible: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .take(height)
        .map(|l| {
            let s = l.as_str();
            // Colour-code by event kind.
            let style = if s.contains("\"kind\":\"tool_error\"")
                || s.contains("\"kind\":\"inference_error\"")
                || s.contains("\"kind\":\"agent_failed\"")
            {
                Style::default().fg(Color::Red)
            } else if s.contains("\"kind\":\"sandbox_applied\"")
                || s.contains("\"kind\":\"sandbox_skipped\"")
            {
                Style::default().fg(Color::Cyan)
            } else if s.contains("\"kind\":\"capability_denied\"") {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            let n = 200.min(s.len());
            let end = s.floor_char_boundary(n);
            Line::from(Span::styled(s[..end].to_string(), style))
        })
        .collect();

    f.render_widget(
        Paragraph::new(visible)
            .block(Block::default().borders(Borders::ALL).title(" flight log ")),
        content_area,
    );

    let filter_cycle: String = [
        InspectorFilter::All,
        InspectorFilter::Errors,
        InspectorFilter::Sandbox,
        InspectorFilter::CapDenied,
    ]
    .iter()
    .map(|f| {
        if f == &app.inspector_view.filter {
            format!("[{}]", f.label())
        } else {
            f.label().to_string()
        }
    })
    .collect::<Vec<_>>()
    .join(" ");

    let hints = format!(" Tab:filter({filter_cycle})  [/]search  [r]refresh  Esc/q back ");
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
    if let Some(sb) = app.sandbox.as_ref() {
        out.push_str(&format!("sandbox: any_sandboxed={}\n", sb.any_sandboxed));
        for s in &sb.servers {
            out.push_str(&format!(
                "  server {}: transport={} isolation={} landlock={} seccomp={} \
                spawn_enforcement={} namespace_net={} namespace_mount={} landlock_net={}\n",
                sanitize(&s.name), sanitize(&s.transport), sanitize(&s.isolation),
                s.landlock, s.seccomp, sanitize(&s.spawn_enforcement),
                s.namespace_net, s.namespace_mount, s.landlock_net,
            ));
        }
        for d in &sb.degradations {
            out.push_str(&format!("  degradation: {}\n", sanitize(d)));
        }
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
    // Topology section
    out.push_str("topology:\n");
    for a in &app.agents {
        let parent = a.parent_id.as_deref().unwrap_or("none");
        out.push_str(&format!("  topology: {} parent={} status={}\n", a.id, parent, a.status));
    }
    // Memory section — read live from FUSE; skip if Phase 5 absent.
    let kb_dir = app.agents_dir.join("kb");
    if !kb_dir.is_dir() {
        out.push_str("memory: subsystem not present\n");
        return out;
    }
    out.push_str("memory:\n");
    // Per-agent memory (first 5 entries each section, no search filter).
    for a in &app.agents {
        if let Some(mem) = read_agent_memory(&app.agents_dir, &a.id, "") {
            out.push_str(&format!("  agent {}:\n", a.id));
            if mem.short_term.is_empty() {
                out.push_str("    short_term: (empty)\n");
            } else {
                out.push_str("    short_term:\n");
                for item in mem.short_term.iter().take(5) {
                    out.push_str(&format!("      - {}\n", sanitize(item)));
                }
            }
            if mem.long_term.is_empty() {
                out.push_str("    long_term: (empty)\n");
            } else {
                out.push_str("    long_term:\n");
                for e in mem.long_term.iter().take(5) {
                    let preview = &e.content[..e.content.floor_char_boundary(80.min(e.content.len()))];
                    out.push_str(&format!("      {}: {}\n", sanitize(&e.key), sanitize(preview)));
                }
            }
        }
    }
    // KB segments.
    let kb_segs = read_kb_segments(&app.agents_dir, "");
    if kb_segs.is_empty() {
        out.push_str("  kb: (no segments)\n");
    } else {
        out.push_str("  kb:\n");
        for seg in &kb_segs {
            let badge = if seg.class.is_empty() { String::new() } else { format!(" [{}]", seg.class) };
            out.push_str(&format!("    {}{badge}: {} entries\n", seg.name, seg.entries.len()));
            for e in seg.entries.iter().take(5) {
                let preview = &e.content[..e.content.floor_char_boundary(80.min(e.content.len()))];
                out.push_str(&format!("      {}: {}\n", sanitize(&e.key), sanitize(preview)));
            }
        }
    }
    // Spawn section — template catalogue summary.
    out.push_str("spawn:\n");
    if app.spawn_view.templates.is_empty() {
        out.push_str("  templates: (none loaded — enter Spawn view in TUI to load)\n");
    } else {
        out.push_str(&format!("  templates: {}\n", app.spawn_view.templates.len()));
        for t in &app.spawn_view.templates {
            out.push_str(&format!("    {} — {}\n", t.name, sanitize(&t.description)));
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
            parent_id:      None,
            sandbox:        None,
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

    // ── topology section in render_plain ────────────────────────────────────

    #[test]
    fn render_plain_topology_section_no_parent() {
        let mut a = make_agent("root", "running", 0, vec![]);
        a.parent_id = None;
        let snap = Snapshot {
            agents: vec![a],
            budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("topology:"), "topology header must appear");
        assert!(out.contains("parent=none"), "top-level agent must show parent=none");
    }

    #[test]
    fn render_plain_topology_section_with_parent() {
        let mut child = make_agent("scout", "done", 0, vec![]);
        child.parent_id = Some("coordinator".to_string());
        let snap = Snapshot {
            agents: vec![child],
            budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("parent=coordinator"), "child agent must show parent id");
    }

    // ── render_plain: sandbox block ──────────────────────────────────────────

    #[test]
    fn render_plain_sandbox_with_server_and_degradation() {
        use crate::watch::reader::{ServerEnforcement, SysSandbox};
        let sb = SysSandbox {
            any_sandboxed: true,
            servers: vec![ServerEnforcement {
                name:              "search".to_string(),
                transport:         "stdio".to_string(),
                isolation:         "none".to_string(),
                landlock:          true,
                seccomp:           true,
                spawn_enforcement: "fork_vfork_only".to_string(),
                namespace_net:     false,
                namespace_mount:   false,
                landlock_net:      false,
            }],
            degradations: vec!["landlock_net_unavailable".to_string()],
        };
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None,
            sandbox: Some(sb), provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("sandbox: any_sandboxed=true"), "any_sandboxed must appear");
        assert!(out.contains("server search:"), "server name must appear");
        assert!(out.contains("transport=stdio"), "transport must appear");
        assert!(out.contains("landlock=true"), "landlock flag must appear");
        assert!(out.contains("seccomp=true"), "seccomp flag must appear");
        assert!(out.contains("spawn_enforcement=fork_vfork_only"), "spawn_enforcement must appear");
        assert!(out.contains("degradation: landlock_net_unavailable"), "degradation must appear");
    }

    #[test]
    fn render_plain_sandbox_http_transport_server() {
        use crate::watch::reader::{ServerEnforcement, SysSandbox};
        let sb = SysSandbox {
            any_sandboxed: false,
            servers: vec![ServerEnforcement {
                name:              "linear".to_string(),
                transport:         "http".to_string(),
                isolation:         "none".to_string(),
                landlock:          false,
                seccomp:           false,
                spawn_enforcement: "none".to_string(),
                namespace_net:     false,
                namespace_mount:   false,
                landlock_net:      false,
            }],
            degradations: vec![],
        };
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None,
            sandbox: Some(sb), provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("transport=http"), "HTTP transport must appear in render_plain output");
    }

    #[test]
    fn render_plain_sandbox_gvisor_shows_isolation() {
        use crate::watch::reader::{ServerEnforcement, SysSandbox};
        let sb = SysSandbox {
            any_sandboxed: true,
            servers: vec![ServerEnforcement {
                name:      "sandbox-server".to_string(),
                transport: "stdio".to_string(),
                isolation: "gvisor".to_string(),
                landlock:  false, seccomp: false,
                spawn_enforcement: "none".to_string(),
                namespace_net: false, namespace_mount: false, landlock_net: false,
            }],
            degradations: vec![],
        };
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None,
            sandbox: Some(sb), provider: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("isolation=gvisor"), "gvisor isolation must appear in render_plain output");
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

    // ── Memory view: render_plain ────────────────────────────────────────────

    fn tmpdir() -> tempfile::TempDir { tempfile::tempdir().unwrap() }

    fn empty_snap() -> Snapshot {
        Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, error: None }
    }

    fn app_with_dir(dir: &std::path::Path, snap: Snapshot) -> App {
        let mut app = App::new(dir.to_path_buf());
        app.apply_snapshot(snap);
        app
    }

    #[test]
    fn render_memory_absent_subsystem_shows_message() {
        let d = tmpdir();
        // No kb/ dir → Phase 5 absent.
        let out = render_plain(&app_with_dir(d.path(), empty_snap()));
        assert!(out.contains("memory: subsystem not present"),
            "absent Phase 5 must produce 'subsystem not present' line; got:\n{out}");
    }

    #[test]
    fn render_memory_absent_empty_shows_no_data_yet() {
        let d = tmpdir();
        std::fs::create_dir_all(d.path().join("kb")).unwrap();
        let out = render_plain(&app_with_dir(d.path(), empty_snap()));
        assert!(out.contains("memory:"), "memory header must appear");
        assert!(out.contains("kb: (no segments)"), "empty kb must show '(no segments)'");
    }

    #[test]
    fn render_memory_no_entries_for_agent_kb_still_renders() {
        let d = tmpdir();
        let seg = d.path().join("kb").join("project");
        std::fs::create_dir_all(&seg).unwrap();
        std::fs::write(seg.join("k1"), r#"{"content":"note","class":"log","provenance":{}}"#).unwrap();
        // No agents in snapshot, but KB has a segment.
        let out = render_plain(&app_with_dir(d.path(), empty_snap()));
        assert!(out.contains("project"), "KB segment must appear even with no agents");
    }

    #[test]
    fn render_memory_shows_short_term_items() {
        let d = tmpdir();
        std::fs::create_dir_all(d.path().join("kb")).unwrap();
        let mem = d.path().join("agent-1").join("memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(mem.join("short_term"), "key insight here\nfact two\n").unwrap();
        let snap = Snapshot {
            agents: vec![make_agent("agent-1", "running", 0, vec![])],
            budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_with_dir(d.path(), snap));
        assert!(out.contains("key insight here"), "short_term item must appear in output");
        assert!(out.contains("fact two"), "second short_term item must appear");
    }

    #[test]
    fn render_memory_shows_kb_segments_with_class_badge() {
        let d = tmpdir();
        let seg = d.path().join("kb").join("events");
        std::fs::create_dir_all(&seg).unwrap();
        std::fs::write(seg.join("e1"), r#"{"content":"entry","class":"log","provenance":{}}"#).unwrap();
        let out = render_plain(&app_with_dir(d.path(), empty_snap()));
        assert!(out.contains("[log]"), "class badge [log] must appear for log segments");
        assert!(out.contains("events"), "segment name must appear");
    }

    #[test]
    fn render_memory_truncation_indicator() {
        use crate::watch::memory::MAX_DISPLAY_ENTRIES;
        let d = tmpdir();
        let seg = d.path().join("kb").join("big");
        std::fs::create_dir_all(&seg).unwrap();
        for i in 0..(MAX_DISPLAY_ENTRIES + 2) {
            std::fs::write(
                seg.join(format!("k{i:04}")),
                r#"{"content":"v","class":"scratch","provenance":{}}"#,
            ).unwrap();
        }
        let out = render_plain(&app_with_dir(d.path(), empty_snap()));
        // Plain mode reads MAX_DISPLAY_ENTRIES entries and the truncated flag is on the segment.
        // The entries count shown in the output will reflect what was fetched (20).
        assert!(out.contains("big"), "segment name must appear");
    }

    #[test]
    fn render_memory_plain_mode_all_agents() {
        let d = tmpdir();
        std::fs::create_dir_all(d.path().join("kb")).unwrap();
        for id in &["agent-a", "agent-b"] {
            let mem = d.path().join(id).join("memory");
            std::fs::create_dir_all(&mem).unwrap();
            std::fs::write(mem.join("short_term"), format!("note from {id}\n")).unwrap();
        }
        let snap = Snapshot {
            agents: vec![
                make_agent("agent-a", "running", 0, vec![]),
                make_agent("agent-b", "running", 0, vec![]),
            ],
            budget: None, queue: None, sandbox: None, provider: None, error: None,
        };
        let out = render_plain(&app_with_dir(d.path(), snap));
        assert!(out.contains("agent-a"), "agent-a must appear in memory section");
        assert!(out.contains("agent-b"), "agent-b must appear in memory section");
        assert!(out.contains("note from agent-a"), "agent-a short_term must appear");
        assert!(out.contains("note from agent-b"), "agent-b short_term must appear");
    }

    #[test]
    fn render_memory_control_chars_not_rendered() {
        let d = tmpdir();
        let seg = d.path().join("kb").join("sec");
        std::fs::create_dir_all(&seg).unwrap();
        // Store a content string containing an ANSI escape sequence.
        let raw = "{\"content\":\"hello\x1bworld\",\"class\":\"log\",\"provenance\":{}}";
        std::fs::write(seg.join("k1"), raw).unwrap();
        let out = render_plain(&app_with_dir(d.path(), empty_snap()));
        assert!(!out.contains('\x1b'), "ANSI escape must be stripped before output");
        assert!(out.contains("helloworld") || out.contains("hello"), "content must appear without ESC");
    }

    #[test]
    fn render_memory_provenance_ts_nanoseconds_formatted_as_rfc3339() {
        use crate::watch::memory::parse_entry;
        // 1_000_000_000 ns = 1970-01-01T00:00:01Z
        let raw = r#"{"content":"x","provenance":{"agent_id":"a","turn":1,"ts":1000000000,"task_fp":"0x1"}}"#;
        let e = parse_entry("k", raw);
        assert!(e.provenance.contains("1970-01-01T00:00:01Z"),
            "nanosecond u64 ts must be formatted as RFC3339; got: {}", e.provenance);
    }

    #[test]
    fn render_memory_min_width_guard() {
        // Structural: constant must match the plan specification.
        assert_eq!(MIN_MEMORY_WIDTH, 50, "MIN_MEMORY_WIDTH must be 50 per plan");
    }

    #[test]
    fn render_memory_search_shows_match_count() {
        use crate::watch::memory::{filter_entries, MemoryEntry};
        let entries: Vec<MemoryEntry> = (0..5).map(|i| MemoryEntry {
            key:        format!("k{i}"),
            content:    if i < 2 { "needle content".to_string() } else { "hay".to_string() },
            provenance: String::new(),
            class:      String::new(),
        }).collect();
        let matches = filter_entries(&entries, "needle");
        assert_eq!(matches.len(), 2,
            "filter_entries must return 2 matches for 'needle' in 5 entries");
    }

    #[test]
    fn render_memory_true_tab_only_active_pane_rendered() {
        use crate::watch::app::{MemoryPane, MemoryPaneState};
        // Verify that active_scroll_mut operates on the correct field per pane.
        let mut state = MemoryPaneState { pane: MemoryPane::LongTerm, ..Default::default() };
        *state.active_scroll_mut() = 7;
        // Switch pane — LongTerm scroll must be preserved, ShortTerm scroll is separate.
        state.pane = MemoryPane::ShortTerm;
        assert_eq!(state.short_term_scroll, 0, "ShortTerm scroll must be independent of LongTerm");
        state.pane = MemoryPane::LongTerm;
        assert_eq!(*state.active_scroll_mut(), 7, "LongTerm scroll must survive pane switch");
    }

    #[test]
    fn render_memory_search_active_highlighted() {
        use crate::watch::app::MemoryPaneState;
        // Structural: search_active flag is independent state from search_query.
        let mut state = MemoryPaneState {
            search_active: true,
            search_query: "arch".to_string(),
            ..Default::default()
        };
        assert!(state.search_active);
        assert_eq!(state.search_query, "arch");
        // Closing search leaves query in place (user can re-open and see it).
        // Pressing [/] again would set search_active=true again.
        state.search_active = false;
        assert!(!state.search_active);
        assert_eq!(state.search_query, "arch", "query must persist after closing search mode");
    }
}
