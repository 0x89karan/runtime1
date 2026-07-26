use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};

use super::app::{App, MemoryAbsence, MemoryPane, SpawnFocus, View};
use super::approvals::ApprovalsMode;
use super::converse::{ConversePhase, TurnRole};
use super::memory::{
    filter_entries, filter_short_term, read_agent_memory, read_kb_segments, MAX_DISPLAY_ENTRIES,
    MAX_SEARCH_ENTRIES,
};
use super::reader;
use super::topology::render_tree;

/// Strip ASCII control characters (< 0x20, except tab) from a string before
/// rendering it in a TUI widget or plain-text output. Guards against ANSI
/// escape sequences embedded in OS error messages.
fn sanitize(s: &str) -> String {
    s.chars().filter(|&c| c >= ' ' || c == '\t').collect()
}

const MIN_TOPOLOGY_WIDTH: u16 = 60;
const MIN_MEMORY_WIDTH:   u16 = 50;

/// ux.1: below this total terminal width, the chat rail hides entirely and Dashboard
/// falls back to the table-only layout (Design Pass 6, corrected arithmetic: table's own
/// column floor is Min(20)+20+4+10+12+6=72 raw cols + ~8 border/padding ≈ 80, rail needs
/// ≥30 cols of prose width + ~4 border ≈ 34, + 1 divider column = 115).
const MIN_TOTAL_WIDTH_FOR_RAIL: u16 = 115;
/// ux.1: below this height, the rail hides too — 3 rows for the input box (border top,
/// text line, border bottom) + 5 minimally-useful transcript rows (Design Pass 6).
const MIN_RAIL_HEIGHT: u16 = 8;
/// ux.1: fixed width of the chat rail pane — a fixed `Length`, not a `Percentage`, so the
/// agent table always keeps its own `Min(72)` floor regardless of terminal width (Design
/// Pass 1 finding: a naive percentage split would crush the table below its own columns).
const CONVERSE_RAIL_WIDTH: u16 = 32;
/// ux.1: protects the existing 6-column table (Min(20)+20+4+10+12+6 = 72) from being
/// squeezed by the rail split.
const CONVERSE_TABLE_MIN_WIDTH: u16 = 72;

/// Pure width/height gate for whether the chat rail fits — extracted so the exact
/// floor arithmetic (Design Pass 6) is unit-testable without a full terminal render,
/// and so `handle_dashboard_key` (mod.rs) can check rail visibility before letting
/// `Tab` focus it (caught during /review's adversarial pass — Tab previously focused
/// the rail unconditionally, silently swallowing all keystrokes on narrow terminals
/// where the rail is actually hidden).
pub fn converse_rail_fits(width: u16, height: u16) -> bool {
    width >= MIN_TOTAL_WIDTH_FOR_RAIL && height >= MIN_RAIL_HEIGHT
}

/// Fixed chrome rows `render_dashboard` always reserves outside `content_area`: header,
/// attention summary, and footer, plus one more row when a spawn banner is showing.
/// Single source of truth for both `render_dashboard`'s own `Layout` constraints and
/// `handle_dashboard_key` (mod.rs)'s pre-render Tab-visibility estimate — found by
/// `/ship`'s Step 9 maintainability specialist as a duplicated literal that could
/// silently desync if the layout ever changes.
pub fn dashboard_chrome_rows(has_spawn_banner: bool) -> u16 {
    let header_and_summary = 2;
    let footer = 2;
    let banner = if has_spawn_banner { 1 } else { 0 };
    header_and_summary + footer + banner
}

pub fn render(f: &mut Frame, app: &App) {
    match app.view {
        View::Dashboard   => render_dashboard(f, app),
        View::AgentDetail => render_agent_detail(f, app),
        View::System      => render_system(f, app),
        View::Topology    => render_topology(f, app),
        View::Memory      => render_memory(f, app),
        View::Spawn       => render_spawn(f, app),
        View::Inspector   => render_inspector(f, app),
        View::Approvals   => render_approvals(f, app),
        View::Credentials => render_credentials(f, app),
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
        s if s.starts_with("running")           => Style::default().fg(Color::Green),
        s if s.starts_with("waiting")           => Style::default().fg(Color::Cyan),
        s if s.starts_with("deferred")          => Style::default().fg(Color::Yellow),
        s if s.starts_with("awaiting_child")    => Style::default().fg(Color::Cyan),
        s if s.starts_with("awaiting_approval") => Style::default().fg(Color::Magenta),
        s if s.starts_with("done")              => Style::default().fg(Color::Blue),
        s if s.starts_with("failed")            => Style::default().fg(Color::Red),
        _                                       => Style::default(),
    }
}

/// `AttentionSignal.since` is an absolute Unix-epoch second (see `surfaces::AttentionSignal`'s
/// doc comment), NOT a duration — every "{X}s ago" render site must subtract it from "now"
/// first. (Adversarial review finding: every call site originally formatted `since` directly,
/// producing a nonsensical multi-billion-second "ago" value on every real signal shown.)
fn secs_ago(since: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(since);
    now.saturating_sub(since)
}

/// `BudgetRisk` and `EvaluationUnavailable` are recomputed fresh on every scheduler tick /
/// every poll — their `since` is stamped "now" every time, not a tracked onset (ship-review
/// Red Team finding). Rendering that as "0s ago" implies a duration these signal types can't
/// actually measure, and would silently mislead an operator into thinking a long-standing
/// budget or config-drift issue just started. `ApprovalPending` always has a real onset
/// (`created_at`). `Degraded` usually has one too (`last_refresh_at`/`attention_since`), EXCEPT
/// for ApiKey-style providers whose env var was never set — that path never populates either
/// field, so `derive_attention` sends `since: 0` as a sentinel (a real Unix-epoch second is
/// never exactly 0) meaning "no onset was ever tracked" (adversarial review finding, Claude +
/// Codex both independently caught this for the missing-API-key case).
fn age_display(sig: &reader::AttentionSignal) -> String {
    match sig.reason {
        // Recomputed-onset signals stamp `since: now` every tick (no tracked onset) — render
        // "active", never a misleading "0s ago". `Error` (ux.2b) joins these.
        reader::AttentionReason::BudgetRisk
        | reader::AttentionReason::EvaluationUnavailable
        | reader::AttentionReason::Error => "active".to_string(),
        reader::AttentionReason::Degraded if sig.since == 0 => "active".to_string(),
        // `Idle` (ux.2b) carries a REAL onset (last_event_at), so its elapsed time is meaningful.
        reader::AttentionReason::ApprovalPending
        | reader::AttentionReason::Degraded
        | reader::AttentionReason::Idle => {
            format!("{}s ago", secs_ago(sig.since))
        }
    }
}

/// Three-way row classification shared by the TUI glyph, `--plain` marker, and the legend
/// text — a single source of truth so the three can never drift apart (Maintainability
/// review finding: the TUI and `--plain` paths previously reimplemented this independently
/// with slightly different boolean logic).
enum AttentionClass {
    /// Evaluated, zero signals active.
    Clean,
    /// Every active signal is `EvaluationUnavailable` — a read/parse failure, never rendered
    /// as Clean (Design Review Pass 2's CRITICAL finding).
    Unavailable,
    /// At least one real (non-`EvaluationUnavailable`) signal is active.
    Active,
}

fn classify_attention(signals: &[reader::AttentionSignal]) -> AttentionClass {
    if signals.is_empty() {
        return AttentionClass::Clean;
    }
    if signals.iter().all(|s| s.reason == reader::AttentionReason::EvaluationUnavailable) {
        return AttentionClass::Unavailable;
    }
    AttentionClass::Active
}

/// Named so the legend line (below) can never silently drift from what the glyph functions
/// actually render — both read from these same three constants.
const GLYPH_ACTIVE: &str = "⚠";
const GLYPH_CLEAN: &str = "·";
const GLYPH_UNAVAILABLE: &str = "?";

/// ux.2a: single-glyph attention indicator for a row. Three states, never a blank cell —
/// `Clean` and `EvaluationUnavailable` are visually distinct so a failed read is never
/// mistaken for "nothing wrong" (Design Review Pass 2's CRITICAL finding).
fn attention_glyph_and_style(signals: &[reader::AttentionSignal]) -> (&'static str, Style) {
    match classify_attention(signals) {
        AttentionClass::Clean       => (GLYPH_CLEAN, Style::default().fg(Color::DarkGray)),
        AttentionClass::Unavailable => (GLYPH_UNAVAILABLE, Style::default().fg(Color::Yellow)),
        AttentionClass::Active      => {
            let color = if signals.iter().any(|s| s.reason.is_critical()) { Color::Red } else { Color::Yellow };
            (GLYPH_ACTIVE, Style::default().fg(color))
        }
    }
}

/// Highest-priority active signal for the stacked reason line, by declaration order
/// (`AttentionReason`'s `Ord` impl) — NOT severity. `ApprovalPending` always wins even when
/// a `Degraded` signal is more severe, since an approval is the one signal type an operator
/// resolves directly (Design Fix 1).
pub(crate) fn top_attention_signal(signals: &[reader::AttentionSignal]) -> Option<&reader::AttentionSignal> {
    signals.iter().min_by(|a, b| a.reason.cmp(&b.reason))
}

/// Fleet-wide attention counts, shared by the TUI and `--plain` summary lines so the two
/// can never drift on what counts as "needs attention" vs. "unavailable" (Design Fix 3:
/// `EvaluationUnavailable`-only agents count toward `unavailable`, not `needing`).
fn attention_counts(agents: &[reader::AgentInfo]) -> (usize, usize) {
    let needing = agents.iter().filter(|a| {
        a.attention.iter().any(|s| s.reason != reader::AttentionReason::EvaluationUnavailable)
    }).count();
    let unavailable = agents.iter().filter(|a| {
        a.attention.iter().any(|s| s.reason == reader::AttentionReason::EvaluationUnavailable)
    }).count();
    (needing, unavailable)
}

/// Abbreviate a token count for the 12-col Budget cell: `512`, `47k`, `1.2M`, `100M`, `5.0G`.
/// Drops the decimal at ≥100M and switches to a G tier at ≥1B so an operator-set ceiling
/// (ux.11a SetBudget accepts any u64) never blows past the fixed column (Codex ship review:
/// `5000.0M/5000.0M` = 15 chars would clip). Extreme (>1e12) values still clip — never reached
/// by realistic token budgets.
fn abbrev_tokens(n: u64) -> String {
    // Width is bounded per side so `spent/limit` fits the 12-col cell for every u64
    // SetBudget accepts. One decimal ONLY where the integer part is a single digit
    // (1.2M, 5.0G); integer form above that (47M, 999M, 100G) — so nothing rounds up
    // into a 6-char `100.0M` (Codex ship review: `99_950_000` → `100.0M` overflowed).
    if n < 1_000 {
        format!("{n}")                                   // ≤ "999"
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)                        // ≤ "999k"
    } else if n < 10_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)        // "1.0M".."9.9M"
    } else if n < 1_000_000_000 {
        format!("{}M", n / 1_000_000)                    // "10M".."999M"
    } else if n < 10_000_000_000 {
        format!("{:.1}G", n as f64 / 1_000_000_000.0)    // "1.0G".."9.9G"
    } else {
        format!("{}G", n / 1_000_000_000)                // "10G".."999G" (u64::MAX maps to Unlimited upstream)
    }
}

/// Render the Budget cell (ux.11a): windowed spend against the ceiling.
/// Unlimited → `47k spent` (NEVER `47k/0`); bounded → `47k/100k`, `1.2M/2.0M`.
fn format_budget_cell(windowed_spent: u64, budget: &reader::BudgetKind) -> String {
    match budget {
        reader::BudgetKind::Unlimited     => format!("{} spent", abbrev_tokens(windowed_spent)),
        reader::BudgetKind::Tokens(limit) => {
            format!("{}/{}", abbrev_tokens(windowed_spent), abbrev_tokens(*limit))
        }
    }
}

fn render_dashboard(f: &mut Frame, app: &App) {
    let area = f.area();

    // When a spawn banner is active, carve out an extra line below the header. Row counts
    // here must match `dashboard_chrome_rows()` above — that function is the single source
    // of truth `handle_dashboard_key` (mod.rs) uses to estimate this same layout.
    let (header_area, summary_area, banner_area, content_area, footer_area) = if app.spawn_banner.is_some() {
        debug_assert_eq!(dashboard_chrome_rows(true), 5);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),  // header bar
                Constraint::Length(1),  // ux.2a: attention summary line
                Constraint::Length(1),  // spawn banner
                Constraint::Min(1),     // main content
                Constraint::Length(2),  // footer: key hints + ux.2a legend
            ])
            .split(area);
        (chunks[0], chunks[1], Some(chunks[2]), chunks[3], chunks[4])
    } else {
        debug_assert_eq!(dashboard_chrome_rows(false), 4);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),  // header bar
                Constraint::Length(1),  // ux.2a: attention summary line
                Constraint::Min(1),     // main content
                Constraint::Length(2),  // footer: key hints + ux.2a legend
            ])
            .split(area);
        (chunks[0], chunks[1], None, chunks[2], chunks[3])
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

    // ux.2a: attention summary line — always rendered, even at zero, so the layout never
    // reflows and "nothing needs attention" is a stated fact, not an absence (Design Fix 3
    // covers the fleet-wide-unavailable case; per-agent EvaluationUnavailable is excluded
    // from the "needs attention" count here, matching Reference table 1's semantics).
    let (needing, unavailable) = attention_counts(&app.agents);
    let summary_text = match (needing, unavailable) {
        (0, 0) => "0 need attention".to_string(),
        (n, 0) => format!("{n} need attention"),
        (n, m) => format!("{n} need attention · {m} unavailable"),
    };
    let summary_style = if needing > 0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    f.render_widget(Paragraph::new(summary_text).style(summary_style), summary_area);

    // Spawn banner (shown after live injection via /agents/control).
    if let (Some(msg), Some(banner_rect)) = (&app.spawn_banner, banner_area) {
        let text = format!(" ✓ {} ", sanitize(msg));
        f.render_widget(
            Paragraph::new(text).style(Style::default().bg(Color::Green).fg(Color::Black)),
            banner_rect,
        );
    }

    // ux.1: horizontal split for the chat rail — only when the terminal is wide/tall
    // enough (Design Pass 6's corrected floor); below it, the table gets the full
    // content_area exactly as before this increment (zero layout change at narrow widths).
    let rail_fits = converse_rail_fits(content_area.width, content_area.height);
    let (table_area, rail_area) = if rail_fits {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(CONVERSE_TABLE_MIN_WIDTH), Constraint::Length(CONVERSE_RAIL_WIDTH)])
            .split(content_area);
        (chunks[0], Some(chunks[1]))
    } else {
        (content_area, None)
    };

    if let Some(rail_rect) = rail_area {
        render_converse_rail(f, app, rail_rect);
    }

    // Agent table
    let selected_idx = app.selected_index();
    let header_row = Row::new(vec![
        Cell::from("Agent ID").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Status").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("ATTN").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Context").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Budget").style(Style::default().add_modifier(Modifier::BOLD)),
        Cell::from("Tools").style(Style::default().add_modifier(Modifier::BOLD)),
    ]).style(Style::default().bg(Color::DarkGray));

    let rows: Vec<Row> = app.agents.iter().enumerate().map(|(i, a)| {
        let is_sel = selected_idx == Some(i);
        let bg = if is_sel { Color::Blue } else { Color::Reset };
        let (glyph, glyph_style) = attention_glyph_and_style(&a.attention);
        // Stacked reason line, per the top-priority (most actionable) active signal — rendered
        // as line 2 of the Agent ID cell (ratatui `Table` cells don't span columns; this is the
        // widest column, `Constraint::Min(20)`, so the reason text has room to be readable).
        let id_text: ratatui::text::Text = if let Some(sig) = top_attention_signal(&a.attention) {
            let age = age_display(sig);
            let reason_line = match &sig.evidence {
                Some(ev) => format!("  {} {} ({}) · {age}", glyph, sig.reason.label(), sanitize(ev)),
                None     => format!("  {} {} · {age}", glyph, sig.reason.label()),
            };
            ratatui::text::Text::from(vec![
                Line::from(a.id.clone()),
                Line::from(Span::styled(reason_line, glyph_style)),
            ])
        } else {
            ratatui::text::Text::from(a.id.clone())
        };
        let height = if top_attention_signal(&a.attention).is_some() { 2 } else { 1 };
        Row::new(vec![
            Cell::from(id_text),
            Cell::from(a.status.clone()).style(status_style(&a.status)),
            Cell::from(glyph).style(glyph_style),
            Cell::from(format!("{}", a.context_tokens)),
            Cell::from(format_budget_cell(a.windowed_spent, &a.budget)),
            Cell::from(format!("{}", a.tools.len())),
        ]).style(Style::default().bg(bg)).height(height)
    }).collect();

    if app.agents.is_empty() {
        let msg = app.error.as_deref()
            .map(|e| format!("error: {}", sanitize(e)))
            .unwrap_or_else(|| "no agents running".to_string());
        f.render_widget(
            Paragraph::new(msg)
                .block(Block::default().borders(Borders::ALL).title(" agents ")),
            table_area,
        );
    } else {
        let table = Table::new(
            rows,
            [
                Constraint::Min(20),     // Agent ID
                Constraint::Length(20),  // Status
                Constraint::Length(4),   // ATTN (ux.2a) — leads, right after Status
                Constraint::Length(10),  // Context
                Constraint::Length(12),  // Budget
                Constraint::Length(6),   // Tools
            ],
        )
        .header(header_row)
        .block(Block::default().borders(Borders::ALL).title(" agents "));
        f.render_widget(table, table_area);
    }

    // Footer: key hints line + ux.2a legend line (a genuine 2-row layout change from the
    // single-line footer this Dashboard had before — Design/DX Review both flagged that
    // adding a legend without widening the footer wasn't actually free).
    let footer_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(footer_area);
    // ux.1: 3-state footer (DX Pass 1 — the single biggest discoverability gap found in
    // review). `r` is listed first and set apart from the `[x]` nav-key bracket style,
    // since it's a different key *category* (act on the selected row, not navigate away).
    let hints = if !rail_fits {
        " ↑/↓ select  Enter: view detail  [s]ystem  [t]opology  [m]emory  [n]ew  [a]pprove  [c]reds  [i]nspector  q quit  (resize to 115+ cols / 8+ rows for chat) ".to_string()
    } else if app.converse_view.rail_focused {
        " Esc/Tab: back to table  Enter: send  ↑/↓: scroll  End: follow  Ctrl-c: cancel ".to_string()
    } else {
        " ↑/↓ select  r retarget chat  Tab: chat  Enter: view detail  [s]ystem  [t]opology  [m]emory  [n]ew  [a]pprove  [c]reds  [i]nspector  q quit ".to_string()
    };
    f.render_widget(
        Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        footer_chunks[0],
    );
    let legend = format!(
        "  Legend: {GLYPH_ACTIVE} needs attention   {GLYPH_CLEAN} checked, clear   {GLYPH_UNAVAILABLE} couldn't check"
    );
    f.render_widget(
        Paragraph::new(legend).style(Style::default().fg(Color::DarkGray)),
        footer_chunks[1],
    );
}

/// ux.1: middle-ellipsis truncation for the border-title target selector, so a long
/// agent id can't overflow or corrupt the rail's fixed-width border (Design dual-voice
/// finding item 7). Budget chosen to comfortably fit `┤ → {name} ├` inside
/// CONVERSE_RAIL_WIDTH; short ids pass through unchanged.
fn truncate_target_label(id: &str, budget: usize) -> String {
    if id.chars().count() <= budget {
        return id.to_string();
    }
    let half = budget.saturating_sub(1) / 2;
    let chars: Vec<char> = id.chars().collect();
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len().saturating_sub(half)..].iter().collect();
    format!("{head}…{tail}")
}

/// Approximate the number of visual rows `Paragraph::wrap` will render a `Line` as, so
/// the chat rail's scroll offset (visual rows, not logical lines) can account for a long
/// turn/streamed reply that wraps across multiple rows in the narrow rail. Not
/// pixel-perfect (doesn't replicate ratatui's exact word-break algorithm), but far more
/// accurate than assuming one row per line — found by /ship's Step 11 adversarial pass.
fn wrapped_row_count(line: &Line, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let char_count: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    let rows = char_count.saturating_sub(1) / width + 1;
    rows as u16
}

/// ux.1: the Dashboard chat rail — a permanent pane beside the agent table (not a
/// separate `View`), honoring the project's locked "one unified screen" decision
/// (docs/ROADMAP.md:1089) rather than a 10th full-screen tab.
fn render_converse_rail(f: &mut Frame, app: &App, area: Rect) {
    let target = &app.converse_view.active_target;
    let label = truncate_target_label(target, 20);
    let border_style = if app.converse_view.rail_focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(area);
    let (transcript_area, input_area) = (chunks[0], chunks[1]);

    // Transcript: flushed history + the in-progress current_reply (green while streaming).
    let mut lines: Vec<Line> = Vec::new();
    let target_state = app.converse_view.targets.get(target);
    match target_state {
        None => {
            lines.push(Line::from(format!("No conversation yet with {label} — press Enter to start")));
        }
        Some(state) => {
            if state.history.is_empty() && state.current_reply.is_empty() {
                lines.push(Line::from(format!("No conversation yet with {label} — press Enter to start")));
            }
            for turn in &state.history {
                let (prefix, style) = match turn.role {
                    TurnRole::Operator  => ("you: ", Style::default()),
                    TurnRole::Assistant => ("agent: ", Style::default()),
                    TurnRole::System    => ("! ", Style::default().fg(Color::Yellow)),
                };
                lines.push(Line::from(Span::styled(format!("{prefix}{}", sanitize(&turn.text)), style)));
            }
            if !state.current_reply.is_empty() || state.phase == ConversePhase::Streaming {
                let prefix = if state.phase == ConversePhase::Dispatching { "... " } else { "agent: " };
                lines.push(Line::from(Span::styled(
                    format!("{prefix}{}", sanitize(&state.current_reply)),
                    Style::default().fg(Color::Green),
                )));
            } else if state.phase == ConversePhase::Dispatching {
                lines.push(Line::from(Span::styled("...", Style::default().fg(Color::Green))));
            }
        }
    }

    // ux.1 acceptance criterion 3: streaming never yanks the scroll when the operator
    // has scrolled up. `follow` (default) always shows the tail; a manual scroll-up
    // pins the view and shows a `▼ N new` indicator instead of jumping back down.
    let inner_height = transcript_area.height.saturating_sub(2); // borders top+bottom
    // `lines.len()` counts logical turns, not the visual rows `Paragraph::wrap` actually
    // renders — found by /ship's Step 11 adversarial pass (Codex structured review): a
    // long streamed current_reply that wraps across many rows in the narrow rail left
    // `bottom_offset` near 0, pinning follow-mode to the TOP of that reply instead of its
    // live tail, making ongoing output appear to vanish below the viewport. Sum
    // per-line wrapped-row estimates instead of counting lines 1:1.
    let inner_width = transcript_area.width.saturating_sub(2); // borders left+right
    let total_lines: u16 = lines.iter().map(|line| wrapped_row_count(line, inner_width)).sum();
    let bottom_offset = total_lines.saturating_sub(inner_height);
    let (scroll_offset, title) = match target_state {
        Some(state) if !state.follow => {
            let offset = bottom_offset.saturating_sub(state.scroll_up_lines.min(bottom_offset));
            let suffix = if state.new_since_scroll > 0 {
                format!(" ▼ {} new ", state.new_since_scroll)
            } else {
                String::new()
            };
            (offset, format!("┤ → {label} ├{suffix}"))
        }
        _ => (bottom_offset, format!("┤ → {label} ├")),
    };

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title).border_style(border_style))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((scroll_offset, 0)),
        transcript_area,
    );

    // Input box: fixed 3-row height (border top, text line, border bottom) — same
    // idiom as header_footer_layout's Constraint::Length usage elsewhere in this file.
    let input_display = if app.converse_view.rail_focused {
        format!("{}█", app.converse_view.input) // simple block cursor while typing
    } else {
        app.converse_view.input.clone()
    };
    f.render_widget(
        Paragraph::new(input_display)
            .block(Block::default().borders(Borders::ALL).border_style(border_style)),
        input_area,
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
    // ux.2a: persistent attention strip — renders iff ≥1 signal is active, ALWAYS the top
    // line(s) (before Status), one line per active signal, highest-priority first. Absent
    // entirely for a clean agent, not rendered blank (same "silence is a real state" rule as
    // the Dashboard).
    let mut sorted_signals: Vec<&reader::AttentionSignal> = agent.attention.iter().collect();
    sorted_signals.sort_by(|a, b| a.reason.cmp(&b.reason));
    let attention_lines: Vec<Line> = sorted_signals.iter().map(|sig| {
        let (glyph, style) = attention_glyph_and_style(std::slice::from_ref(sig));
        let age = age_display(sig);
        let text = match &sig.evidence {
            Some(ev) => format!("{} {} ({}) · {age}", glyph, sig.reason.label(), sanitize(ev)),
            None     => format!("{} {} · {age}", glyph, sig.reason.label()),
        };
        Line::from(Span::styled(text, style))
    }).collect();

    let mut lines: Vec<Line> = attention_lines;
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines.extend(vec![
        {
            let mut spans = vec![
                Span::styled("  Status:   ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(agent.status.clone(), status_style(&agent.status)),
            ];
            if let Some(detail) = &agent.status_detail {
                spans.push(Span::raw(format!(" [{detail}]")));
            }
            Line::from(spans)
        },
        {
            let ctx_str = if agent.tier == "universal" {
                "N/A".to_string()
            } else {
                format!("{} tokens", agent.context_tokens)
            };
            Line::from(format!("  Context:  {ctx_str}"))
        },
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
        Line::from(vec![
            Span::styled("  Egress:   ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "{} brokered  {} denied",
                agent.egress_brokered, agent.egress_denied
            )),
        ]),
        {
            let tier = &agent.tier;
            if tier == "universal" {
                let pid_str = if agent.pid > 0 { format!("{}", agent.pid) } else { "?".to_string() };
                let iso_str = if agent.isolation.is_empty() { "none" } else { &agent.isolation };
                Line::from(vec![
                    Span::styled("  Tier:     ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(
                        format!("TIER: universal | ISO: {iso_str} | PID: {pid_str}"),
                        Style::default().fg(ratatui::style::Color::Cyan),
                    ),
                ])
            } else {
                Line::from("")
            }
        },
    ]);
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" detail ")),
        content_area,
    );

    let hints = " Esc/q back to dashboard | [i] inspector ";
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
    // Isolation tier line (ma.4).
    let (iso_text, iso_style) = match app.isolation.as_ref() {
        None => (
            "unknown".to_string(),
            Style::default(),
        ),
        Some(iso) => {
            let tier_style = match iso.tier.as_str() {
                "full"       => Style::default().fg(Color::Green),
                "capability" => Style::default().fg(Color::Yellow),
                _            => Style::default().fg(Color::Red),
            };
            let runsc_str = sanitize(iso.runsc.as_deref().unwrap_or("none"));
            let text = format!(
                "{} (arch={} runsc={} landlock={} seccomp={})",
                sanitize(&iso.tier), sanitize(&iso.arch), runsc_str, iso.landlock, iso.seccomp
            );
            (text, tier_style)
        }
    };
    lines.push(Line::from(vec![
        Span::styled("  Isolation: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(iso_text, iso_style),
    ]));
    // Legend: capability = kernel-level sandbox only (Landlock and/or seccomp, no gVisor).
    lines.push(Line::from(vec![
        Span::styled(
            "             (full=gVisor+landlock+seccomp  capability=kernel-only  none=unsandboxed)",
            Style::default().fg(Color::DarkGray),
        ),
    ]));

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
            // Colour-code by event kind. Errors share ONE predicate with the Inspector
            // `Errors` filter (par.1-ar-01) so the highlight and the filter can't drift.
            let style = if super::inspector::is_error_event(s) {
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

fn render_approvals(f: &mut Frame, app: &App) {
    let (header_area, content_area, footer_area) = header_footer_layout(f.area());

    let pending_count = app.approvals_items.len();
    f.render_widget(
        Paragraph::new(format!(
            " agentctl watch › approvals ({pending_count} pending) "
        )).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    let av = &app.approvals_view;

    match av.mode {
        ApprovalsMode::List => {
            if app.approvals_items.is_empty() {
                let lines = vec![
                    Line::from(""),
                    Line::from("  No pending approvals."),
                ];
                f.render_widget(
                    Paragraph::new(lines)
                        .block(Block::default().borders(Borders::ALL).title(" pending approvals ")),
                    content_area,
                );
            } else {
                let rows: Vec<Row> = app.approvals_items.iter().enumerate().map(|(i, a)| {
                    let is_sel = av.selected_idx == i;
                    let bg = if is_sel { Color::Blue } else { Color::Reset };
                    let risk_style = match a.risk.as_str() {
                        "high"   => Style::default().fg(Color::Red),
                        "medium" => Style::default().fg(Color::Yellow),
                        _        => Style::default().fg(Color::Green),
                    };
                    Row::new(vec![
                        Cell::from(a.id.clone()),
                        Cell::from(a.agent_id.clone()),
                        Cell::from(a.kind.clone()),
                        Cell::from(a.risk.clone()).style(risk_style),
                        Cell::from(sanitize(&a.summary)),
                        Cell::from(format!("{}s", a.age_secs)),
                    ]).style(Style::default().bg(bg))
                }).collect();

                let header_row = Row::new(vec![
                    Cell::from("ID").style(Style::default().add_modifier(Modifier::BOLD)),
                    Cell::from("Agent").style(Style::default().add_modifier(Modifier::BOLD)),
                    Cell::from("Kind").style(Style::default().add_modifier(Modifier::BOLD)),
                    Cell::from("Risk").style(Style::default().add_modifier(Modifier::BOLD)),
                    Cell::from("Summary").style(Style::default().add_modifier(Modifier::BOLD)),
                    Cell::from("Age").style(Style::default().add_modifier(Modifier::BOLD)),
                ]).style(Style::default().bg(Color::DarkGray));

                let table = Table::new(
                    rows,
                    [
                        Constraint::Length(12),  // ID
                        Constraint::Length(14),  // Agent
                        Constraint::Length(14),  // Kind
                        Constraint::Length(7),   // Risk
                        Constraint::Min(20),     // Summary
                        Constraint::Length(6),   // Age
                    ],
                )
                .header(header_row)
                .block(Block::default().borders(Borders::ALL).title(" pending approvals "));
                f.render_widget(table, content_area);
            }

            // Result message banner (shown briefly after approve/reject).
            if let Some(msg) = &av.result_msg {
                let style = if msg.starts_with("Error") || msg.starts_with("error") {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::Green)
                };
                f.render_widget(
                    Paragraph::new(format!(" {msg} ")).style(style),
                    footer_area,
                );
                return;
            }

            let hints = " ↑/↓/j/k select  Enter resolve  Esc/q back to dashboard ";
            f.render_widget(
                Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
                footer_area,
            );
        }

        ApprovalsMode::Confirm => {
            let item = app.approvals_items.get(av.selected_idx);
            let (id, agent, kind, risk, summary) = item
                .map(|a| (a.id.as_str(), a.agent_id.as_str(), a.kind.as_str(), a.risk.as_str(), a.summary.as_str()))
                .unwrap_or(("?", "?", "?", "?", "?"));

            let risk_style = match risk {
                "high"   => Style::default().fg(Color::Red),
                "medium" => Style::default().fg(Color::Yellow),
                _        => Style::default().fg(Color::Green),
            };

            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("  ID:      ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(id),
                ]),
                Line::from(vec![
                    Span::styled("  Agent:   ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(agent),
                ]),
                Line::from(vec![
                    Span::styled("  Kind:    ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(kind),
                ]),
                Line::from(vec![
                    Span::styled("  Risk:    ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(risk, risk_style),
                ]),
                Line::from(vec![
                    Span::styled("  Summary: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(sanitize(summary)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  [a] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                    Span::raw("Approve"),
                    Span::raw("    "),
                    Span::styled("[d] ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::raw("Approve (don't ask again for this kind)"),
                    Span::raw("    "),
                    Span::styled("[r] ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                    Span::raw("Reject"),
                ]),
            ];

            f.render_widget(
                Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title(" confirm action ")),
                content_area,
            );

            let hints = " [a]pprove  [d] approve+don't ask  [r]eject  Esc cancel ";
            f.render_widget(
                Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
                footer_area,
            );
        }

        ApprovalsMode::RejectReason => {
            let lines = vec![
                Line::from(""),
                Line::from("  Enter rejection reason (optional, press Enter to submit):"),
                Line::from(""),
                Line::from(format!("  > {}_", av.reject_reason)),
            ];

            f.render_widget(
                Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title(" reject reason ")),
                content_area,
            );

            let hints = " Enter submit  Esc cancel ";
            f.render_widget(
                Paragraph::new(hints).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
                footer_area,
            );
        }
    }
}

fn render_credentials(f: &mut Frame, app: &App) {
    let (header_area, content_area, footer_area) = header_footer_layout(f.area());

    let title = Span::styled(" Credentials ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(Paragraph::new(Line::from(vec![title])), header_area);

    match app.credentials.as_ref() {
        None => {
            let msg = Paragraph::new("Credential gateway not configured (no [credential_gateway] in agent.toml).")
                .block(Block::default().borders(Borders::ALL).title("Status"));
            f.render_widget(msg, content_area);
        }
        Some(creds) if !creds.gateway_enabled => {
            let msg = Paragraph::new("Credential gateway disabled.")
                .block(Block::default().borders(Borders::ALL).title("Status"));
            f.render_widget(msg, content_area);
        }
        Some(creds) => {
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(vec![
                Span::styled("Gateway: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled("enabled", Style::default().fg(Color::Green)),
            ]));
            lines.push(Line::from(format!(
                "Configured providers: {}",
                if creds.configured_providers.is_empty() { "(none)".to_string() }
                else { creds.configured_providers.join(", ") }
            )));
            lines.push(Line::from(""));

            for ph in &creds.provider_health {
                let fresh_style = if ph.token_fresh {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                let fresh_label = if ph.token_fresh { "fresh" } else { "stale/missing" };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {} ", sanitize(&ph.name)), Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(format!("[{fresh_label}]"), fresh_style),
                ]));
                if let Some(exp) = ph.expires_at {
                    lines.push(Line::from(format!("    expires_at: {exp}")));
                }
                if let Some(last) = ph.last_refresh_at {
                    lines.push(Line::from(format!("    last_refresh: {last}")));
                }
                if let Some(ref err) = ph.last_error {
                    lines.push(Line::from(vec![
                        Span::raw("    last_error: "),
                        Span::styled(sanitize(err), Style::default().fg(Color::Red)),
                    ]));
                }
            }

            let para = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title("Credential Gateway Health"));
            f.render_widget(para, content_area);
        }
    }

    let footer = Line::from(Span::styled(
        " [Esc/q] back ",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(footer), footer_area);
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
    if let Some(iso) = app.isolation.as_ref() {
        out.push_str(&format!("isolation_tier: {}\n", sanitize(&iso.tier)));
        out.push_str(&format!("isolation_arch: {}\n", sanitize(&iso.arch)));
        if let Some(p) = &iso.runsc {
            out.push_str(&format!("isolation_runsc: {}\n", sanitize(p)));
        }
        out.push_str(&format!("isolation_landlock: {}\n", iso.landlock));
        out.push_str(&format!("isolation_seccomp: {}\n", iso.seccomp));
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
        // ux.2a: attention summary — same "N need attention · M unavailable" semantics as the
        // TUI's summary line, never silently omitting the caveat when M > 0.
        let (needing, unavailable) = attention_counts(&app.agents);
        out.push_str(&match (needing, unavailable) {
            (0, 0) => "attention: 0 need attention\n".to_string(),
            (n, 0) => format!("attention: {n} need attention\n"),
            (n, m) => format!("attention: {n} need attention, {m} unavailable\n"),
        });
        out.push_str(&format!("agents: {}\n", app.agents.len()));
        for a in &app.agents {
            let ctx_str = if a.tier == "universal" { "N/A".to_string() } else { a.context_tokens.to_string() };
            let tier_str = if a.tier == "universal" {
                format!(" tier=universal pid={}", a.pid)
            } else {
                String::new()
            };
            let attn_marker = match classify_attention(&a.attention) {
                AttentionClass::Clean       => "[OK]",
                AttentionClass::Unavailable => "[?]",
                AttentionClass::Active      => "[!]",
            };
            out.push_str(&format!(
                // Ship-review finding (Codex adversarial): the attention marker and status
                // bracket must stay as two separate tokens ("[OK] [failed]"), not concatenated
                // ("[OK][failed]") — anything parsing --plain output positionally (this mode is
                // explicitly for CI/non-TTY consumption) would otherwise see the second token
                // silently change shape.
                "  {} {attn} [{status}] ctx={ctx} budget={budget} tools={tools}{tier}\n",
                a.id,
                attn   = attn_marker,
                status = a.status,
                ctx    = ctx_str,
                budget = a.budget.display(),
                tools  = a.tools.len(),
                tier   = tier_str,
            ));
            // Marker + reason text, not a bare marker — collapsing Approval/Budget/Degraded
            // to an unlabeled "[!]" would make them indistinguishable in plain mode (DX Review
            // finding).
            let mut sorted: Vec<&reader::AttentionSignal> = a.attention.iter().collect();
            sorted.sort_by(|x, y| x.reason.cmp(&y.reason));
            for sig in sorted {
                let marker = if sig.reason == reader::AttentionReason::EvaluationUnavailable { "[?]" } else { "[!]" };
                let age = age_display(sig);
                match &sig.evidence {
                    Some(ev) => out.push_str(&format!(
                        "    {marker} {} ({}) · {age}\n", sig.reason.label(), sanitize(ev)
                    )),
                    None => out.push_str(&format!(
                        "    {marker} {} · {age}\n", sig.reason.label()
                    )),
                }
            }
        }
    }
    // Topology section
    out.push_str("topology:\n");
    for a in &app.agents {
        let parent = a.parent_id.as_deref().unwrap_or("none");
        out.push_str(&format!("  topology: {} parent={} status={}\n", a.id, parent, a.status));
    }
    // Credentials section.
    match app.credentials.as_ref() {
        None => out.push_str("credentials: gateway not configured\n"),
        Some(creds) if !creds.gateway_enabled => out.push_str("credentials: gateway disabled\n"),
        Some(creds) => {
            out.push_str("credentials:\n");
            out.push_str(&format!("  providers: {}\n",
                if creds.configured_providers.is_empty() { "(none)".to_string() }
                else { creds.configured_providers.join(", ") }
            ));
            for ph in &creds.provider_health {
                let freshness = if ph.token_fresh { "fresh" } else { "stale/missing" };
                out.push_str(&format!("  {} token_fresh={} ({freshness})", sanitize(&ph.name), ph.token_fresh));
                if let Some(exp) = ph.expires_at {
                    out.push_str(&format!(" expires_at={exp}"));
                }
                if let Some(last) = ph.last_refresh_at {
                    out.push_str(&format!(" last_refresh={last}"));
                }
                out.push('\n');
                if let Some(ref err) = ph.last_error {
                    out.push_str(&format!("    last_error: {}\n", sanitize(err)));
                }
            }
        }
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
    // Approvals section.
    if app.approvals_items.is_empty() {
        out.push_str("approvals: (none pending)\n");
    } else {
        out.push_str(&format!("approvals: {} pending\n", app.approvals_items.len()));
        for a in &app.approvals_items {
            out.push_str(&format!(
                "  {} agent={} kind={} risk={} age={}s — {}\n",
                a.id,
                a.agent_id,
                a.kind,
                a.risk,
                a.age_secs,
                sanitize(&a.summary),
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
            id:              id.to_string(),
            status:          status.to_string(),
            status_detail:   None,
            context_tokens:  ctx,
            budget:          BudgetKind::Unlimited,
            windowed_spent:  ctx,
            tools,
            parent_id:       None,
            sandbox:         None,
            egress_brokered: 0,
            egress_denied:   0,
            tier:            "native".to_string(),
            isolation:       String::new(),
            pid:             0,
            attention:       vec![],
        }
    }

    fn app_from_snap(snap: Snapshot) -> App {
        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(snap);
        app
    }

    // ── ux.1 chat rail: width/height floor + border-title truncation ─────────

    #[test]
    fn budget_cell_unlimited_shows_spent_never_slash_zero() {
        use crate::watch::reader::BudgetKind;
        assert_eq!(format_budget_cell(47_000, &BudgetKind::Unlimited), "47k spent");
        assert_eq!(format_budget_cell(512, &BudgetKind::Unlimited), "512 spent");
        // Never render "/0" for unlimited.
        assert!(!format_budget_cell(47_000, &BudgetKind::Unlimited).contains("/0"));
    }

    #[test]
    fn budget_cell_bounded_shows_spend_over_limit() {
        use crate::watch::reader::BudgetKind;
        assert_eq!(format_budget_cell(47_000, &BudgetKind::Tokens(100_000)), "47k/100k");
        assert_eq!(format_budget_cell(1_200_000, &BudgetKind::Tokens(2_000_000)), "1.2M/2.0M");
    }

    #[test]
    fn budget_cell_fits_twelve_col_column() {
        use crate::watch::reader::BudgetKind;
        let cases = [
            format_budget_cell(0, &BudgetKind::Unlimited),
            format_budget_cell(999_000, &BudgetKind::Unlimited),
            format_budget_cell(50_000_000, &BudgetKind::Tokens(50_000_000)),
            format_budget_cell(47_000, &BudgetKind::Tokens(100_000)),
            format_budget_cell(1_200_000, &BudgetKind::Tokens(2_000_000)),
            // Large operator-set ceilings (SetBudget accepts any u64) must still fit (Codex).
            format_budget_cell(100_000_000, &BudgetKind::Tokens(200_000_000)),
            format_budget_cell(5_000_000_000, &BudgetKind::Tokens(5_000_000_000)),
            format_budget_cell(1_000_000_000, &BudgetKind::Unlimited),
            // Codex ship-review boundaries: near-100M rounding, and huge G values.
            format_budget_cell(99_950_000, &BudgetKind::Tokens(99_950_000)),
            format_budget_cell(100_000_000_000, &BudgetKind::Tokens(100_000_000_000)),
            format_budget_cell(9_999_999, &BudgetKind::Tokens(9_999_999)),
        ];
        for c in cases {
            assert!(c.chars().count() <= 12, "budget cell '{c}' exceeds 12 cols");
        }
    }

    #[test]
    fn min_total_width_for_rail_115_hides_rail_below_floor() {
        assert!(!converse_rail_fits(114, 20), "114 cols must hide the rail");
        assert!(converse_rail_fits(115, 20), "115 cols must show the rail");
    }

    #[test]
    fn min_rail_height_8_hides_rail_below_floor() {
        assert!(!converse_rail_fits(200, 7), "7 rows must hide the rail");
        assert!(converse_rail_fits(200, 8), "8 rows must show the rail");
    }

    #[test]
    fn dashboard_chrome_rows_accounts_for_spawn_banner() {
        assert_eq!(dashboard_chrome_rows(false), 4, "header(1) + summary(1) + footer(2)");
        assert_eq!(dashboard_chrome_rows(true), 5, "+1 more when the spawn banner is showing");
    }

    #[test]
    fn wrapped_row_count_accounts_for_line_wrapping() {
        // Found by /ship's Step 11 adversarial pass (Codex structured review): counting
        // lines.len() (logical turns) instead of wrapped visual rows left the chat
        // rail's follow-mode scroll offset near 0 for a long current_reply, pinning the
        // view to the TOP of a wrapping reply instead of its live tail.
        let short = Line::from("hi");
        assert_eq!(wrapped_row_count(&short, 30), 1, "text shorter than the width is one row");

        let exact = Line::from("x".repeat(30));
        assert_eq!(wrapped_row_count(&exact, 30), 1, "text exactly filling the width is still one row");

        let one_over = Line::from("x".repeat(31));
        assert_eq!(wrapped_row_count(&one_over, 30), 2, "one char over the width needs a second row");

        let three_rows = Line::from("x".repeat(61));
        assert_eq!(wrapped_row_count(&three_rows, 30), 3);

        let empty = Line::from("");
        assert_eq!(wrapped_row_count(&empty, 30), 1, "even an empty line occupies one row");
    }

    #[test]
    fn border_title_truncation_with_unread_badge_fits_32_cols() {
        // Budget of 20 chars for the target label leaves room for "┤ → " (4) + " ├" (2)
        // = 26, comfortably inside CONVERSE_RAIL_WIDTH (32) even for a long agent id.
        let long_id = "scout-a1b2c3d4-e5f6-9f3d-worker-99";
        let truncated = truncate_target_label(long_id, 20);
        assert!(truncated.chars().count() <= 20, "truncated label must respect the budget: {truncated}");
        assert!(truncated.contains('…'), "long id must be middle-truncated: {truncated}");
        let full_title = format!("┤ → {truncated} ├");
        assert!(full_title.chars().count() <= CONVERSE_RAIL_WIDTH as usize, "title must fit in the rail width: {full_title}");
    }

    #[test]
    fn short_target_label_passes_through_unchanged() {
        assert_eq!(truncate_target_label("orch-default", 20), "orch-default");
    }

    // ── render_plain: empty state ─────────────────────────────────────────────

    #[test]
    fn render_plain_no_agents_outputs_none_line() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("agents: (none)"), "empty agent list must produce '(none)' line");
    }

    #[test]
    fn render_plain_no_provider_omits_provider_line() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(!out.contains("provider:"), "no provider → no provider line");
    }

    #[test]
    fn render_plain_no_budget_omits_tokens_line() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(!out.contains("tokens_spent:"), "no budget → no tokens_spent line");
    }

    // ── render_plain: error field ────────────────────────────────────────────

    #[test]
    fn render_plain_error_appears_in_output() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None,
            isolation: None, credentials: None, error: Some("permission denied".to_string()),
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
            isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("provider: claude-opus-4 (anthropic)"));
    }

    #[test]
    fn render_plain_includes_tokens_spent_when_budget_present() {
        let snap = Snapshot {
            agents: vec![],
            budget: Some(SysBudget { spent: 99_000, total: 0 }),
            queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("tokens_spent: 99000"));
    }

    #[test]
    fn render_plain_includes_queue_depth_when_queue_present() {
        let snap = Snapshot {
            agents: vec![], budget: None,
            queue: Some(SysQueue { depth: 7 }),
            sandbox: None, provider: None, isolation: None, credentials: None, error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
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
            sandbox: Some(sb), provider: None, isolation: None, credentials: None, error: None,
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
            sandbox: Some(sb), provider: None, isolation: None, credentials: None, error: None,
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
            sandbox: Some(sb), provider: None, isolation: None, credentials: None, error: None,
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
                budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
            };
            let out = render_plain(&app_from_snap(snap));
            assert!(out.contains(&format!("[{status}]")),
                "status '{status}' must appear in render_plain output");
        }
    }

    // ── Memory view: render_plain ────────────────────────────────────────────

    fn tmpdir() -> tempfile::TempDir { tempfile::tempdir().unwrap() }

    fn empty_snap() -> Snapshot {
        Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None }
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
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

    // ── render_plain: isolation tier (ma.4) ──────────────────────────────────

    #[test]
    fn render_plain_isolation_tier_none_when_not_present() {
        let out = render_plain(&app_from_snap(empty_snap()));
        // No isolation_caps → no isolation_tier line.
        assert!(!out.contains("isolation_tier:"), "no isolation field → no isolation_tier line");
    }

    #[test]
    fn render_plain_isolation_tier_full_appears_in_output() {
        use crate::watch::reader::SysIsolation;
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None,
            isolation: Some(SysIsolation {
                tier:     "full".to_string(),
                arch:     "x86_64".to_string(),
                runsc:    Some("/usr/bin/runsc".to_string()),
                landlock: true,
                seccomp:  true,
            }),
            credentials: None,
            error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("isolation_tier: full"), "full tier must appear in plain output");
        assert!(out.contains("isolation_arch: x86_64"), "arch must appear");
        assert!(out.contains("isolation_runsc: /usr/bin/runsc"), "runsc path must appear");
        assert!(out.contains("isolation_landlock: true"), "landlock=true must appear");
        assert!(out.contains("isolation_seccomp: true"), "seccomp=true must appear");
    }

    #[test]
    fn render_plain_isolation_runsc_absent_when_none() {
        use crate::watch::reader::SysIsolation;
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None,
            isolation: Some(SysIsolation {
                tier:     "capability".to_string(),
                arch:     "aarch64".to_string(),
                runsc:    None,
                landlock: true,
                seccomp:  false,
            }),
            credentials: None,
            error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("isolation_tier: capability"));
        // runsc must not be printed when None.
        assert!(!out.contains("isolation_runsc:"), "runsc line must be omitted when None");
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

    // ── render_plain: isolation tier ────────────────────────────────────────

    #[test]
    fn render_plain_isolation_none_omits_isolation_lines() {
        use crate::watch::reader::Snapshot;
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None,
            provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(!out.contains("isolation_tier:"),
            "absent isolation must omit isolation_tier line");
    }

    #[test]
    fn render_plain_isolation_capability_appears() {
        use crate::watch::reader::{Snapshot, SysIsolation};
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None,
            provider: None, credentials: None, error: None,
            isolation: Some(SysIsolation {
                tier:     "capability".to_string(),
                arch:     "aarch64".to_string(),
                runsc:    None,
                landlock: true,
                seccomp:  false,
            }),
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("isolation_tier: capability"),
            "capability tier must appear in plain output; got:\n{out}");
        assert!(out.contains("isolation_arch: aarch64"),
            "arch must appear in plain output; got:\n{out}");
    }

    #[test]
    fn render_plain_isolation_none_tier_appears() {
        use crate::watch::reader::{Snapshot, SysIsolation};
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None,
            provider: None, credentials: None, error: None,
            isolation: Some(SysIsolation {
                tier:     "none".to_string(),
                arch:     "x86_64".to_string(),
                runsc:    None,
                landlock: false,
                seccomp:  false,
            }),
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("isolation_tier: none"),
            "none tier must appear in plain output; got:\n{out}");
    }

    // ── render_plain: credentials section ─────────────────────────────────────

    #[test]
    fn render_plain_credentials_not_configured_shows_message() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None,
            provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("credentials: gateway not configured"),
            "None credentials must show not-configured; got:\n{out}");
    }

    #[test]
    fn render_plain_credentials_disabled_shows_disabled() {
        use crate::watch::reader::SysCredentials;
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None,
            provider: None, isolation: None, error: None,
            credentials: Some(SysCredentials {
                gateway_enabled:      false,
                configured_providers: vec![],
                provider_health:      vec![],
            }),
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("credentials: gateway disabled"),
            "Disabled credentials must show disabled; got:\n{out}");
    }

    #[test]
    fn render_plain_credentials_fresh_token_appears() {
        use crate::watch::reader::{SysCredentials, ProvHealthInfo};
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None,
            provider: None, isolation: None, error: None,
            credentials: Some(SysCredentials {
                gateway_enabled:      true,
                configured_providers: vec!["google".to_string()],
                provider_health:      vec![
                    ProvHealthInfo {
                        name:            "google".to_string(),
                        token_fresh:     true,
                        last_refresh_at: Some(1720000000),
                        expires_at:      Some(1720003600),
                        last_error:      None,
                    }
                ],
            }),
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("credentials:"), "must have credentials section");
        assert!(out.contains("google"), "must list provider name");
        assert!(out.contains("token_fresh=true"), "must show token_fresh");
        assert!(out.contains("fresh"), "must show freshness label");
        assert!(out.contains("expires_at="), "must show expiry");
        assert!(out.contains("last_refresh="), "must show last refresh");
    }

    #[test]
    fn render_plain_credentials_stale_token_shows_error() {
        use crate::watch::reader::{SysCredentials, ProvHealthInfo};
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None,
            provider: None, isolation: None, error: None,
            credentials: Some(SysCredentials {
                gateway_enabled:      true,
                configured_providers: vec!["google".to_string()],
                provider_health:      vec![
                    ProvHealthInfo {
                        name:            "google".to_string(),
                        token_fresh:     false,
                        last_refresh_at: None,
                        expires_at:      None,
                        last_error:      Some("token_expired".to_string()),
                    }
                ],
            }),
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("stale/missing"), "stale token must show stale label");
        assert!(out.contains("last_error: token_expired"), "must show last_error");
    }

    // ── ux.2a: attention glyph / priority / --plain ────────────────────────────

    /// `since` is an absolute Unix-epoch second, NOT a duration — constructed here as
    /// "90 seconds ago" from real wall-clock time, matching what production code actually
    /// produces (a tiny hand-picked constant like `since: 90` would silently mask the exact
    /// "since is an epoch, not a duration" bug an adversarial review caught in this feature).
    fn signal(reason: reader::AttentionReason, evidence: Option<&str>) -> reader::AttentionSignal {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        reader::AttentionSignal { reason, since: now - 90, evidence: evidence.map(str::to_string) }
    }

    #[test]
    fn secs_ago_computes_elapsed_from_epoch_not_the_epoch_itself() {
        // Regression test for the adversarial-review CRITICAL finding: `since` is an absolute
        // Unix-epoch second; rendering it directly as "{since}s ago" produces a nonsensical
        // multi-billion-second value. `secs_ago` must subtract from "now" first.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let ago = secs_ago(now - 42);
        assert!(ago < 1_000_000, "secs_ago({}) returned {ago} — looks like a raw epoch, not an elapsed duration", now - 42);
        assert!((40..=45).contains(&ago), "expected ~42s elapsed, got {ago}s (tolerance for test execution time)");
    }

    fn make_agent_with_attention(id: &str, attention: Vec<reader::AttentionSignal>) -> AgentInfo {
        let mut a = make_agent(id, "running", 100, vec![]);
        a.attention = attention;
        a
    }

    #[test]
    fn attention_glyph_clean_is_dim_dot() {
        let (glyph, style) = attention_glyph_and_style(&[]);
        assert_eq!(glyph, "·");
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn attention_glyph_evaluation_unavailable_only_is_question_mark() {
        let sigs = vec![signal(reader::AttentionReason::EvaluationUnavailable, Some("credential_gateway"))];
        let (glyph, style) = attention_glyph_and_style(&sigs);
        assert_eq!(glyph, "?");
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn attention_glyph_real_signal_is_warning_not_question_mark() {
        // A real signal must never be visually indistinguishable from "couldn't check" —
        // that ambiguity was the Design Review's CRITICAL finding.
        let sigs = vec![signal(reader::AttentionReason::ApprovalPending, None)];
        let (glyph, _) = attention_glyph_and_style(&sigs);
        assert_eq!(glyph, "⚠");
    }

    #[test]
    fn attention_glyph_mixed_real_and_unavailable_is_warning_not_question_mark() {
        // A real signal alongside an EvaluationUnavailable one (e.g. approval pending AND the
        // credential-health read failed) must still lead with the real, actionable signal —
        // the "all EvaluationUnavailable" branch must not fire when a real signal is present.
        let sigs = vec![
            signal(reader::AttentionReason::ApprovalPending, Some("act_1")),
            signal(reader::AttentionReason::EvaluationUnavailable, Some("credential_gateway")),
        ];
        let (glyph, _) = attention_glyph_and_style(&sigs);
        assert_eq!(glyph, "⚠", "a real signal must not be masked by a co-occurring EvaluationUnavailable");
    }

    #[test]
    fn attention_glyph_degraded_is_red_others_are_yellow() {
        let (_, degraded_style) = attention_glyph_and_style(&[signal(reader::AttentionReason::Degraded, None)]);
        assert_eq!(degraded_style.fg, Some(Color::Red));
        let (_, approval_style) = attention_glyph_and_style(&[signal(reader::AttentionReason::ApprovalPending, None)]);
        assert_eq!(approval_style.fg, Some(Color::Yellow));
    }

    #[test]
    fn top_attention_signal_approval_beats_degraded() {
        let sigs = vec![
            signal(reader::AttentionReason::Degraded, Some("google")),
            signal(reader::AttentionReason::ApprovalPending, Some("act_1")),
        ];
        let top = top_attention_signal(&sigs).expect("must have a top signal");
        assert_eq!(top.reason, reader::AttentionReason::ApprovalPending);
    }

    #[test]
    fn render_plain_clean_agent_shows_ok_marker() {
        let snap = Snapshot {
            agents: vec![make_agent_with_attention("scout-1", vec![])],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("[OK]"));
        assert!(out.contains("attention: 0 need attention"));
    }

    /// Ship-review finding (Codex adversarial): the attention marker and status bracket must
    /// be two separate tokens, not concatenated — --plain is explicitly for CI/non-TTY
    /// consumption, and a positional-field parser would see the status token silently change
    /// shape from "[status]" to "[OK][status]".
    #[test]
    fn render_plain_attn_marker_and_status_bracket_have_a_space_between_them() {
        let snap = Snapshot {
            agents: vec![make_agent_with_attention("scout-1", vec![])],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("[OK] ["), "attn marker and status bracket must be space-separated, not concatenated: {out}");
        assert!(!out.contains("[OK]["), "must never concatenate the two tokens: {out}");
    }

    #[test]
    fn render_plain_flagged_agent_shows_marker_and_reason_text() {
        let snap = Snapshot {
            agents: vec![make_agent_with_attention(
                "scout-1",
                vec![signal(reader::AttentionReason::ApprovalPending, Some("act_1"))],
            )],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("[!]"));
        assert!(out.contains("approval pending (act_1)"), "reason text must render, not a bare marker");
        assert!(out.contains("attention: 1 need attention"));
    }

    #[test]
    fn render_plain_evaluation_unavailable_shows_distinct_marker_and_is_not_counted_as_needing() {
        let snap = Snapshot {
            agents: vec![make_agent_with_attention(
                "scout-1",
                vec![signal(reader::AttentionReason::EvaluationUnavailable, Some("credential_gateway"))],
            )],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("[?]"));
        assert!(out.contains("attention: 0 need attention"), "eval-unavailable alone is not 'needs attention'");
    }

    #[test]
    fn render_plain_mixed_needing_and_unavailable_shows_both_counts() {
        // Coverage gap: `attention_counts`'s (n, m) match arm — both a genuinely-flagged agent
        // AND a separate couldn't-evaluate agent present in the same fleet snapshot — was never
        // exercised; every existing test hit either (0,0), (n,0), or (0,m) but not both counts
        // positive at once. A swapped format arg or wrong separator in that third arm would
        // have compiled and passed every other test here.
        let snap = Snapshot {
            agents: vec![
                make_agent_with_attention(
                    "scout-1",
                    vec![signal(reader::AttentionReason::ApprovalPending, Some("act_1"))],
                ),
                make_agent_with_attention(
                    "scout-2",
                    vec![signal(reader::AttentionReason::EvaluationUnavailable, Some("credential_gateway"))],
                ),
            ],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(
            out.contains("attention: 1 need attention, 1 unavailable"),
            "mixed needing+unavailable branch must report both counts together, not just one: {out}"
        );
    }

    #[test]
    fn render_plain_label_text_matches_each_attention_reason() {
        // Coverage gap: only `AttentionReason::ApprovalPending`'s label() string
        // ("approval pending") was ever asserted verbatim elsewhere in this file. A typo in
        // the Degraded/BudgetRisk/EvaluationUnavailable label() arms (reader.rs) would compile
        // and pass every existing test — the glyph/routing/count tests exercise those branches
        // structurally but never check the actual label text they render.
        let snap = Snapshot {
            agents: vec![make_agent_with_attention(
                "scout-1",
                vec![
                    signal(reader::AttentionReason::Degraded, Some("google")),
                    signal(reader::AttentionReason::BudgetRisk, Some("92%")),
                    signal(reader::AttentionReason::EvaluationUnavailable, Some("credential_gateway")),
                ],
            )],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("degraded (google)"), "Degraded label text must render verbatim: {out}");
        assert!(out.contains("budget risk (92%)"), "BudgetRisk label text must render verbatim: {out}");
        assert!(
            out.contains("evaluation unavailable (credential_gateway)"),
            "EvaluationUnavailable label text must render verbatim: {out}"
        );
    }

    /// Ship-review Red Team finding: `derive_attention` recomputes `since: now` on every
    /// scheduler tick for BudgetRisk and config-drift EvaluationUnavailable — there is no
    /// tracked onset, so displaying "0s ago" would misrepresent a potentially long-standing
    /// issue as having "just started." These two must show "active" instead; ApprovalPending
    /// and Degraded DO have a real onset and must keep showing true elapsed time.
    #[test]
    fn render_plain_budget_and_unavailable_show_active_not_elapsed_time() {
        let snap = Snapshot {
            agents: vec![make_agent_with_attention(
                "scout-1",
                vec![
                    signal(reader::AttentionReason::BudgetRisk, Some("92%")),
                    signal(reader::AttentionReason::EvaluationUnavailable, Some("credential_gateway")),
                ],
            )],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("budget risk (92%) · active"), "BudgetRisk must show 'active', not a fake elapsed time: {out}");
        assert!(
            out.contains("evaluation unavailable (credential_gateway) · active"),
            "EvaluationUnavailable must show 'active', not a fake elapsed time: {out}"
        );
        assert!(!out.contains("0s ago"), "must never render a misleading '0s ago' for these signal types: {out}");
    }

    #[test]
    fn render_plain_approval_and_degraded_still_show_real_elapsed_time() {
        let snap = Snapshot {
            agents: vec![make_agent_with_attention(
                "scout-1",
                vec![signal(reader::AttentionReason::ApprovalPending, Some("act_1"))],
            )],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("s ago"), "ApprovalPending must still show real elapsed time, not 'active': {out}");
        assert!(!out.contains("· active"), "ApprovalPending must not show 'active': {out}");
    }

    /// Ship-review finding (Claude + Codex adversarial): Degraded's `since: 0` sentinel (an
    /// ApiKey provider that was never configured, so no real onset was ever tracked) must
    /// render "active", not "0s ago" — indistinguishable from a credential that broke this
    /// exact instant, which is the misleading-freshness bug this session already fixed once
    /// for BudgetRisk/EvaluationUnavailable.
    #[test]
    fn render_plain_degraded_zero_since_sentinel_shows_active() {
        let snap = Snapshot {
            agents: vec![make_agent_with_attention(
                "scout-1",
                vec![reader::AttentionSignal {
                    reason: reader::AttentionReason::Degraded,
                    since: 0,
                    evidence: Some("brave_search".to_string()),
                }],
            )],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("degraded (brave_search) · active"), "since:0 sentinel must show 'active': {out}");
        assert!(!out.contains("0s ago"), "must never render the misleading '0s ago' for a never-tracked onset: {out}");
    }
}
