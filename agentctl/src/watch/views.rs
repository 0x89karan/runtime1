use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
};

use super::app::{App, CancelMarker, JobOverlayMode, JobsOverlay, MemoryAbsence, MemoryPane, SpawnFocus, View};
use super::approvals::ApprovalsMode;
use super::converse::{ConversePhase, TurnRole};
use super::logs::{clip_payload, format_ts, logs_viewport_rows};
use super::overlay::{menu_items, overlay_fits, overlay_rect, overlay_text_width, DashboardOverlay, OverlayMode, PendingVerb};
use super::memory::{
    filter_entries, filter_short_term, read_agent_memory, read_kb_segments, MAX_DISPLAY_ENTRIES,
    MAX_SEARCH_ENTRIES,
};
use super::reader;
use super::topology::{descendants, render_tree, TopologyGraph};

/// Strip control characters from a string before rendering it in a TUI widget or plain-text
/// output. Guards against ANSI escape sequences embedded in OS error messages — and, since
/// ux.10-A, against raw container output, which is fully untrusted bytes.
///
/// Three classes go: C0 (< 0x20, except tab), DEL (0x7f), and C1 (U+0080–U+009F — many
/// terminals still interpret these as control codes, `ESC`-equivalents among them, when they
/// arrive as UTF-8).
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            c == '\t' || (c >= ' ' && c != '\u{7f}' && !('\u{80}'..='\u{9f}').contains(&c))
        })
        .collect()
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
        View::Logs        => render_logs(f, app),
        View::Jobs        => render_jobs(f, app),
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
        // ux.13-TUI (M8): a requested cancel has nothing to read in the snapshot — no
        // `AgentStatus::Cancelling` exists — so the row would keep saying "running" for a whole turn and
        // then vanish, which is indistinguishable from "my keypress did nothing". The marker gets its OWN
        // line in this column, not the Status cell.
        //
        // Two review rounds converged here. Substituting it into Status hid the row's real state for as
        // long as the marker lived (red team). Appending it there — "running · cancelling…" — then
        // regressed at exactly the widths this branch newly claims to support: measured on real frames,
        // ratatui gives Status 24 cols at the 115-col rail floor and 21 at 80, so
        // "awaiting_approval · NOT CANCELLED" rendered with no cancel signal at all (the fix-review
        // pass). This column is `Constraint::Min(20)`, the widest, and already carries a second line for
        // attention reasons — so the marker goes beside the idiom it matches, and Status is untouched at
        // every width.
        let mut id_lines = vec![Line::from(a.id.clone())];
        if let Some(sig) = top_attention_signal(&a.attention) {
            let age = age_display(sig);
            let reason_line = match &sig.evidence {
                Some(ev) => format!("  {} {} ({}) · {age}", glyph, sig.reason.label(), sanitize(ev)),
                None     => format!("  {} {} · {age}", glyph, sig.reason.label()),
            };
            id_lines.push(Line::from(Span::styled(reason_line, glyph_style)));
        }
        if let Some(marker) = app.cancel_marker(&a.id) {
            let style = match marker {
                // Escalation, not decoration: this is a cancel that never took.
                CancelMarker::Unconfirmed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                CancelMarker::InFlight    => Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                // Green against the row's red `failed`: the stop worked, and that is the point.
                CancelMarker::Landed      => Style::default().fg(Color::Green),
            };
            // Both lines when both apply: an attention signal is advisory, a requested cancel is
            // something the operator DID, and neither may hide the other.
            id_lines.push(Line::from(Span::styled(format!("  ⨯ {}", marker.label()), style)));
        }
        let height = id_lines.len() as u16;
        Row::new(vec![
            Cell::from(ratatui::text::Text::from(id_lines)),
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
                Constraint::Length(20),  // Status (unchanged: the cancel marker has its own line)
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
    // ux.10-A: `[l]ogs` appears ONLY when a compose project was detected at startup — the
    // same `logs_view.available` flag that gates the key itself (mod.rs), so the legend can
    // never advertise a key that does nothing.
    let hints = dashboard_hints(
        if app.converse_view.rail_focused { FooterState::RailFocused }
        else if rail_fits { FooterState::Table }
        else { FooterState::NoRail },
        app.logs_view.available,
    );
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

    // ux.13-TUI: drawn LAST so it sits over the table + rail. Anything openable must be visible —
    // an overlay that owns the keyboard without rendering would trap the operator in an invisible mode.
    if let Some(ov) = &app.dashboard_overlay {
        render_dashboard_overlay(f, app, ov, content_area);
    }
}


/// Will the row-action overlay render as a BOX (rather than degrade to a single line) on a terminal of
/// `term_size`?
///
/// The key handler needs the same answer the renderer will reach, and it has only `App.term_size` —
/// the renderer works from `content_area`, which is the frame minus the Dashboard's fixed chrome and,
/// when it fits, minus the chat rail. Uses the widest chrome (banner present) and subtracts the rail,
/// so the predicate is CONSERVATIVE: it can say "no box" one row before the renderer would, and a
/// too-small answer only ever disables verbs (fail closed), never enables one that cannot be seen.
pub fn overlay_fits_dashboard(term_size: (u16, u16)) -> bool {
    let (w, h) = term_size;
    let content_h = h.saturating_sub(dashboard_chrome_rows(true));
    let content_w = if converse_rail_fits(w, content_h) {
        w.saturating_sub(CONVERSE_RAIL_WIDTH)
    } else {
        w
    };
    overlay_fits(content_w, content_h)
}

/// The widest a footer line may be **in a state that only renders when the chat rail fits** — so the
/// terminal is at least `MIN_TOTAL_WIDTH_FOR_RAIL` columns wide. Derived, not a literal: the rail floor
/// and the footer budget are the same number and must not drift apart.
///
/// Measured, not chosen: the pre-ux.13-TUI narrow footer ran to **162 columns** with `[l]ogs`
/// present, so `q quit` began at column 114 and its own `(resize to 115+ cols…)` hint started at 122 —
/// on the only widths where that branch renders (width < 115) the hint about being too narrow was
/// itself off-screen. Acceptance is a WIDTH, deliberately: `contains("q quit")` passes with the clip
/// bug fully intact (design finding V3).
pub const MAX_FOOTER_COLS: usize = MIN_TOTAL_WIDTH_FOR_RAIL as usize - 1;

/// The widest the **narrow** footer may be. A separate, much smaller bound, because
/// `FooterState::NoRail` renders ONLY below the rail floor — bounding it by `MAX_FOOTER_COLS` was
/// vacuous, and /review's testing specialist caught that the shipped narrow line was 113 cols with
/// `q quit` at column 87: on an 80-column terminal, the exact defect V3 exists to fix was still there,
/// with the test passing. 80 columns is the canonical narrow terminal; the view keys come off the line
/// and live behind `?`, which is what `?` is for.
pub const MAX_NARROW_FOOTER_COLS: usize = 80;

/// Which footer the Dashboard is showing. The three states predate this increment (ux.1's DX pass);
/// only their content changed.
#[derive(Clone, Copy, PartialEq)]
pub enum FooterState {
    /// Table focused, chat rail visible.
    Table,
    /// Chat rail has text focus — its keys replace the table's entirely.
    RailFocused,
    /// Terminal too narrow/short for the rail.
    NoRail,
}

/// One row of the Dashboard key map.
///
/// **This table is the single source of truth for both the footer and the `?` overlay.** The Eng phase
/// called `?` "not nearly free" for exactly this reason: a hand-written help screen is a second copy of
/// the key list, and the copy that drifts is always the one the operator reads when they are lost.
pub struct KeyHint {
    /// Footer form — terse, because the footer has 114 columns for everything.
    pub short:  &'static str,
    /// Help form: the key, then what it does in words.
    pub key:    &'static str,
    pub what:   &'static str,
    /// Rendered in the footer's table state (all rows appear in `?`).
    pub footer: bool,
    /// Only present when a docker-compose project was detected at startup.
    pub docker: bool,
}

const DASHBOARD_KEYS: &[KeyHint] = &[
    KeyHint { short: "x act",      key: "x",       what: "row actions on the selected agent (park, budget, cancel)", footer: true,  docker: false },
    KeyHint { short: "? keys",     key: "?",       what: "this help", footer: true, docker: false },
    KeyHint { short: "↑↓ sel",     key: "↑/↓, j/k", what: "select a row", footer: true, docker: false },
    KeyHint { short: "r target",   key: "r",       what: "retarget the chat rail at the selected agent", footer: true, docker: false },
    KeyHint { short: "Tab chat",   key: "Tab",     what: "focus the chat rail (Esc/Tab returns)", footer: true, docker: false },
    KeyHint { short: "Enter open", key: "Enter",   what: "open the selected agent's detail view", footer: true, docker: false },
    KeyHint { short: "[s]ys",      key: "s",       what: "system view — queue, budget, provider, sandbox, credentials", footer: true, docker: false },
    KeyHint { short: "[t]op",      key: "t",       what: "topology — the spawn tree and message edges", footer: true, docker: false },
    KeyHint { short: "[m]em",      key: "m",       what: "memory — per-agent short-term and the shared KB", footer: true, docker: false },
    KeyHint { short: "[n]ew",      key: "n",       what: "spawn a new agent from a template", footer: true, docker: false },
    KeyHint { short: "[a]pp",      key: "a",       what: "approvals — resolve pending operator gates", footer: true, docker: false },
    KeyHint { short: "[c]red",     key: "c",       what: "credentials — provider health and token freshness", footer: true, docker: false },
    KeyHint { short: "[i]nsp",     key: "i",       what: "inspector — the flight-recorder log", footer: true, docker: false },
    KeyHint { short: "[l]og",      key: "l",       what: "logs — tail the docker compose project", footer: true, docker: true },
    KeyHint { short: "q quit",     key: "q",       what: "quit (inside an overlay it dismisses instead)", footer: true, docker: false },
    // Not in the footer — real keys with no room, which is precisely what `?` is for.
    // /autoplan retroactive review (2026-08-07): `[J]` opened the Jobs view (mod.rs's
    // handle_dashboard_key) but was never added here, so it was undiscoverable via `?` too —
    // the exact copy-drift this table's own doc comment above exists to prevent. The footer
    // itself is already at MAX_FOOTER_COLS with no room (footer: false is correct, not a
    // compromise).
    KeyHint { short: "", key: "J", what: "jobs — scheduled [[jobs]] entries, with a manual fire-now verb", footer: false, docker: false },
    KeyHint { short: "", key: "Ctrl-c", what: "quit from anywhere, including mid-verb", footer: false, docker: false },
    KeyHint { short: "", key: "Esc",    what: "leave a view, dismiss an overlay, or unfocus the chat rail", footer: false, docker: false },
];

/// The footer line for `state`. Kept pure and public so its WIDTH can be asserted (see
/// [`MAX_FOOTER_COLS`]) — the clip bug it replaces was invisible to every content-based assertion.
pub fn dashboard_hints(state: FooterState, logs_available: bool) -> String {
    if state == FooterState::RailFocused {
        return " Esc/Tab back to table  Enter send  ↑↓ scroll  End follow  Ctrl-c cancel ".to_string();
    }
    // Below the rail floor the line has ~80 columns for everything, so it keeps only the keys that
    // cannot be discovered another way: the rail keys are unreachable at this width, and the per-view
    // letters are one `?` away. A hint that clips is worse than a hint that is absent.
    let skip_rail = state == FooterState::NoRail;
    let parts: Vec<&str> = DASHBOARD_KEYS
        .iter()
        .filter(|k| k.footer)
        .filter(|k| !k.docker || logs_available)
        .filter(|k| !(skip_rail && matches!(k.key, "r" | "Tab")))
        .filter(|k| !(skip_rail && k.short.starts_with('[')))
        .map(|k| k.short)
        .collect();
    // The view cluster is single-spaced so the whole line fits; the other groups are double-spaced.
    let mut line = String::from(" ");
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            let tight = part.starts_with('[') && parts[i - 1].starts_with('[');
            line.push_str(if tight { " " } else { "  " });
        }
        line.push_str(part);
    }
    if skip_rail {
        // Derived from the same constant the rail's own fits check uses, so the advertised width cannot
        // drift from the width that actually brings the rail back.
        line.push_str(&format!("  ({MIN_TOTAL_WIDTH_FOR_RAIL}+ cols: chat)"));
    }
    line.push(' ');
    // Load-bearing in debug builds as well as in the test: the footer is assembled from a table that
    // will grow, and the next key someone adds must fail loudly rather than silently push `q quit` off
    // the right edge again. Bound per state — the narrow line is drawn on narrow terminals.
    let budget = if skip_rail { MAX_NARROW_FOOTER_COLS } else { MAX_FOOTER_COLS };
    debug_assert!(
        line.chars().count() <= budget,
        "footer is {} cols (max {budget}): {line}",
        line.chars().count(),
    );
    line
}

/// The `?` overlay's body: every key, including the ones the footer has no room for.
fn help_lines(logs_available: bool) -> Vec<(&'static str, &'static str)> {
    DASHBOARD_KEYS
        .iter()
        .filter(|k| !k.docker || logs_available)
        .map(|k| (k.key, k.what))
        .collect()
}

/// ux.13-TUI: the Dashboard row-action overlay, drawn OVER the live dashboard.
///
/// `Clear` is the only widget in this tree that panics on an out-of-frame `Rect` (it indexes the
/// buffer without intersecting, unlike `Block`), so the rect comes from `overlay_rect`, which clamps,
/// and an empty result degrades to a single-line prompt instead of reaching `Clear` at all.
///
/// The target's live row is rendered INSIDE the box: a centred overlay usually covers the very row it
/// is about, so "the dashboard is visible behind it" cannot be the way the operator confirms they are
/// acting on the right agent.
fn render_dashboard_overlay(f: &mut Frame, app: &App, ov: &DashboardOverlay, area: Rect) {
    let target = ov.target(&app.agents);

    // Degraded path for a terminal too small for a box — never a modal with no visible exit.
    //
    // From `term_size`, the SAME input `handle_overlay_key`'s fail-closed gate uses. Deriving it from
    // `area` here instead left a window (height 11: chrome differs by one row) where the renderer drew a
    // full menu that the handler refused to act on — a live-looking, completely dead dialog with no
    // explanation on screen (/review's red team). One predicate, one answer.
    if !overlay_fits_dashboard(app.term_size) {
        // "dismiss", never "cancel": Cancel is a VERB in this overlay, and this is the one hint that
        // used to teach the opposite reading of the same key. It also states that the actions are
        // unavailable here, because `handle_overlay_key` refuses to arm one at this size.
        // Help carries no target (`target_id` is empty), so it must not fall through to the row wording —
        // pressing `?` on a small terminal used to answer " is no longer present" about no agent at all
        // (/review's red team). The box path already guarded this; the degraded path did not.
        let line = match (&ov.mode, target) {
            (OverlayMode::Help, _) => " key map needs a bigger terminal; Esc/q dismisses ".to_string(),
            (_, Some(_)) => format!(
                " {} — row actions need a bigger terminal; Esc/q dismisses ",
                sanitize(&ov.target_id),
            ),
            (_, None) => format!(" {} is no longer present — Esc/q dismisses ", sanitize(&ov.target_id)),
        };
        let row = Rect { x: area.x, y: area.y + area.height.saturating_sub(1), width: area.width, height: 1 };
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(Color::Yellow).fg(Color::Black)),
            row.intersection(area),
        );
        return;
    }

    let mut body: Vec<Line> = Vec::new();
    // Help is not about a row, so it gets no agent header and no "no longer present" notice.
    let show_target = !matches!(ov.mode, OverlayMode::Help);
    match target.filter(|_| show_target) {
        Some(a) => {
            body.push(Line::from(vec![
                Span::styled("agent  ", Style::default().fg(Color::DarkGray)),
                Span::styled(sanitize(&a.id), Style::default().add_modifier(Modifier::BOLD)),
            ]));
            body.push(Line::from(vec![
                Span::styled("status ", Style::default().fg(Color::DarkGray)),
                Span::styled(sanitize(&a.status), status_style(&a.status)),
            ]));
            body.push(Line::from(vec![
                Span::styled("spend  ", Style::default().fg(Color::DarkGray)),
                Span::raw(format_budget_cell(a.windowed_spent, &a.budget)),
            ]));
        }
        None if !show_target => {}
        None => {
            // Resolve-at-use: the pinned agent is gone, so say so and offer no action. Mirrors the
            // Approvals view's "already resolved" branch rather than acting on a stale target.
            // Copy is the DX phase's replacement, verbatim: what happened, why, what to do — and it
            // opens with "No action sent", because the first thing the operator needs to know is that
            // nothing was written.
            for line in wrap_plain(
                &format!(
                    "No action sent: {} is no longer in the snapshot. It may have finished or been \
                     removed; dismiss and select another running agent.",
                    sanitize(&ov.target_id),
                ),
                overlay_text_width(area),
            ) {
                body.push(Line::from(Span::styled(line, Style::default().fg(Color::Yellow))));
            }
        }
    }
    body.push(Line::from(""));
    // The mode body renders even when the target has vanished: an `InFlight`/`Result` frame is ABOUT a
    // write already sent against the pinned id, and an agent that disappeared because the cancel
    // landed is the most likely case of all.
    body.extend(overlay_mode_body(
        ov,
        target,
        &app.topology,
        OverlayCtx {
            budget_resettable: app.budget_resettable(),
            logs_available:    app.logs_view.available,
            cli_conn:          &app.cli_conn,
        },
        overlay_text_width(area),
    ));
    body.push(Line::from(""));

    let hints = Line::from(Span::styled(
        overlay_hints(&ov.mode),
        Style::default().bg(Color::DarkGray).fg(Color::White),
    ));

    let rect = overlay_rect(area, body.len() as u16 + 3);
    if rect.is_empty() {
        return; // clamped to nothing — the fits check above normally prevents this
    }
    // The hints line is the LAST thing dropped, never the first. `overlay_rect` caps height to the
    // frame, so on a short terminal the body is taller than the box and `Paragraph` clips from the
    // bottom — which is exactly where the dismissal key lives. A modal whose exit key has been clipped
    // off is a trapped operator, so the body yields rows to it instead.
    let inner_rows = rect.height.saturating_sub(2) as usize;
    if inner_rows > 0 {
        body.truncate(inner_rows - 1);
        body.push(hints);
    } else {
        body.clear();
    }
    let title = match ov.mode {
        OverlayMode::Menu                 => " row actions ",
        OverlayMode::ConfirmCancel        => " confirm cancel ",
        OverlayMode::Budget { .. }        => " set budget ",
        OverlayMode::ConfirmBudget { .. } => " confirm budget change ",
        OverlayMode::InFlight { .. }      => " working ",
        OverlayMode::Result { .. }        => " result ",
        OverlayMode::Help                 => " keys ",
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(title),
        ),
        rect,
    );
}

/// The per-mode content of the row-action overlay.
///
/// Split out from `render_dashboard_overlay` so the geometry/`Clear` path stays one small function
/// with one job, and so this can grow per mode without the clamp logic drifting.
/// The session facts the overlay body needs. A struct, not four positional parameters: two of them were
/// adjacent same-typed bools derived from the same `App`, so transposing them compiled and produced a
/// plausible-looking frame — including in this file's own test, which passed them as `false, true`
/// (/review's maintainability specialist).
#[derive(Clone, Copy)]
struct OverlayCtx<'a> {
    /// Does the connected agentd have a budget-reset window? Decides what Park MEANS.
    budget_resettable: bool,
    /// Was a docker-compose project detected? Gates the `[l]` row in the `?` key map.
    logs_available: bool,
    /// Flags that make a printed `agentctl …` command reach THIS daemon.
    cli_conn: &'a str,
}

/// `text_width` is the REAL inner width of the box for this frame (`overlay_text_width`), not a
/// constant: prose is wrapped to it here, because the box height is derived from the resulting line
/// count. Hardcoding it is what made the first real pty frame read "…stop at the n".
fn overlay_mode_body<'a>(
    ov: &'a DashboardOverlay,
    target: Option<&'a reader::AgentInfo>,
    topology: &TopologyGraph,
    ctx: OverlayCtx<'_>,
    text_width: usize,
) -> Vec<Line<'a>> {
    let OverlayCtx { budget_resettable, logs_available, cli_conn } = ctx;
    let mut out: Vec<Line> = Vec::new();
    // Wrap a paragraph of prose into styled lines at the box width.
    let para = |out: &mut Vec<Line>, text: &str, indent: usize, style: Style| {
        for line in wrap_plain(text, text_width.saturating_sub(indent).max(8)) {
            out.push(Line::from(Span::styled(format!("{}{line}", " ".repeat(indent)), style)));
        }
    };
    match &ov.mode {
        OverlayMode::Menu => {
            let Some(a) = target else { return out };
            let items = menu_items(a, budget_resettable);
            for (i, item) in items.iter().enumerate() {
                let selected = i == ov.cursor;
                let marker = if selected { "▸ " } else { "  " };
                let label_style = match (selected, item.enabled()) {
                    (_, false)    => Style::default().fg(Color::DarkGray),
                    (true, true)  => Style::default().fg(Color::White).bg(Color::Blue).add_modifier(Modifier::BOLD),
                    (false, true) => Style::default().add_modifier(Modifier::BOLD),
                };
                // The detail is clipped rather than wrapped: a row that grows to two lines would
                // shift every row below it as the cursor moves.
                let detail = clip_to(&item.detail,
                    text_width.saturating_sub(marker.len() + item.label.chars().count() + 2));
                out.push(Line::from(vec![
                    Span::raw(marker),
                    Span::styled(item.label.clone(), label_style),
                    Span::raw("  "),
                    Span::styled(detail, Style::default().fg(Color::DarkGray)),
                ]));
                // The blocked reason renders under the row it belongs to, which is what lets Enter on a
                // disabled item be a plain no-op instead of an error the operator must dismiss.
                if selected {
                    if let Some(reason) = &item.blocked {
                        // Wrapped: this copy carries the "what to do instead" clause at the END, and a
                        // clipped line would keep the refusal while losing the remedy.
                        para(&mut out, reason, 4, Style::default().fg(Color::Yellow));
                    }
                }
            }
        }

        OverlayMode::ConfirmCancel => {
            para(&mut out, &format!("Cancel {}?", sanitize(&ov.target_id)), 0,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));
            para(&mut out,
                "This cannot be undone. The agent and its spawned subtree stop at the next step \
                 boundary.",
                0, Style::default().fg(Color::DarkGray));
            // C4: Cancel CASCADES. On this repo's own coordinator fixture that is three agents, and a
            // confirm dialog naming one id would have understated the blast radius by 2. "at least"
            // because the client walk is a floor — the snapshot is up to a poll stale and has no
            // universal-tier parentage, so the server's count can legitimately be higher (E6).
            let kids = descendants(topology, &ov.target_id);
            if !kids.is_empty() {
                para(&mut out,
                    &format!("Also stops at least {} spawned agent{}: {}",
                        kids.len(),
                        if kids.len() == 1 { "" } else { "s" },
                        kids.iter().map(|k| sanitize(k)).collect::<Vec<_>>().join(", ")),
                    0, Style::default().fg(Color::Yellow));
            }
            out.push(Line::from(""));
            out.extend(equivalent_cli_lines(
                &PendingVerb::Cancel { agent_id: ov.target_id.clone() },
                cli_conn, text_width,
            ));
        }

        OverlayMode::Budget { input, error } => {
            let current = target.map(|a| a.budget.display()).unwrap_or_else(|| "?".to_string());
            para(&mut out,
                &format!("Token budget for {} (current: {current})", sanitize(&ov.target_id)),
                0, Style::default().add_modifier(Modifier::BOLD));
            out.push(Line::from(""));
            out.push(Line::from(clip_to(
                &format!("  > {}", input_with_cursor_glyph(input, '_')),
                text_width,
            )));
            // Said on the field itself, because this is the inversion design finding M2 is about: an
            // empty field plus Enter must not read as "no cap".
            para(&mut out, "0 = unlimited (removes the cap)", 2, Style::default().fg(Color::DarkGray));
            if let Some(e) = error {
                para(&mut out, e, 2, Style::default().fg(Color::Red));
            }
        }

        OverlayMode::ConfirmBudget { limit } => {
            let headline = if *limit == 0 {
                format!("Remove the budget cap on {}?", sanitize(&ov.target_id))
            } else {
                format!("Raise {}'s budget to {limit} tokens?", sanitize(&ov.target_id))
            };
            para(&mut out, &headline, 0,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD));
            para(&mut out,
                if *limit == 0 {
                    "0 means UNLIMITED — the agent can then spend without bound, and the change \
                     survives a restart."
                } else {
                    "This widens the cap rather than tightening it, and the change survives a \
                     restart."
                },
                0, Style::default().fg(Color::Yellow));
            out.push(Line::from(""));
            out.extend(equivalent_cli_lines(
                &PendingVerb::SetBudget { agent_id: ov.target_id.clone(), limit: *limit, park: false },
                cli_conn, text_width,
            ));
        }

        OverlayMode::InFlight { label } => {
            para(&mut out, &sanitize(label), 0,
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
            para(&mut out, "waiting for agentd to confirm…", 0,
                Style::default().fg(Color::DarkGray));
        }

        OverlayMode::Help => {
            // Rendered from the SAME table as the footer, so the two cannot drift.
            for (key, what) in help_lines(logs_available) {
                let key_col = format!("{key:<9}");
                out.push(Line::from(vec![
                    Span::styled(key_col, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::raw(clip_to(what, text_width.saturating_sub(9))),
                ]));
            }
        }

        OverlayMode::Result { text, ok } => {
            let style = if *ok {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            };
            // Wrapped here rather than by `Paragraph::wrap`, which is not enabled: the box height is
            // computed from `body.len()`, so wrapping has to happen where it can be counted.
            para(&mut out, &sanitize(text), 0, style);
        }
    }
    out
}

/// The DX phase's highest-value line: every overlay states the `agentctl` command that does the same
/// thing, including the flags that make it reach THIS daemon. It teaches the fallback path for when the TUI is the broken thing, and makes an incident
/// note copy-pasteable.
fn equivalent_cli_lines(verb: &PendingVerb, conn: &str, width: usize) -> Vec<Line<'static>> {
    const LABEL: &str = "Equivalent: ";
    // Built from `PendingVerb`, never hand-formatted: the two `format!("agentctl …")` copies this
    // replaces were invisible to the clap drift guard AND skipped `sanitize`, so an agent id carrying
    // ESC/CSI bytes reached the terminal from the two frames an operator reads before granting a
    // destructive verb (/review: maintainability + security, same two lines).
    let cmd = &sanitize(&verb.equivalent_cli(conn));
    if LABEL.len() + cmd.chars().count() <= width {
        return vec![Line::from(vec![
            Span::styled(LABEL, Style::default().fg(Color::DarkGray)),
            Span::styled(cmd.to_string(), Style::default().fg(Color::Cyan)),
        ])];
    }
    // Narrow box: label on its own row, command wrapped under it. The command stays readable in full
    // rather than being clipped — it is meant to be typed.
    let mut out = vec![Line::from(Span::styled("Equivalent:", Style::default().fg(Color::DarkGray)))];
    for line in wrap_plain(cmd, width.saturating_sub(2).max(8)) {
        out.push(Line::from(Span::styled(
            format!("  {line}"),
            Style::default().fg(Color::Cyan),
        )));
    }
    out
}

/// Clip a string to `width`, marking the cut. Used where a line must stay ONE row tall (menu rows),
/// as opposed to prose, which wraps.
fn clip_to(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let head: String = s.chars().take(width - 1).collect();
    format!("{head}…")
}

/// Greedy word wrap. `Paragraph::wrap` cannot be used for the overlay body because the box height is
/// derived from the line count before rendering, so wrapping has to happen where it can be counted.
fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Per-mode key hints. Every mode states its own exit — a modal whose dismissal key is only listed on
/// another screen is how an operator gets stuck in one.
fn overlay_hints(mode: &OverlayMode) -> &'static str {
    match mode {
        OverlayMode::Menu                 => " ↑/↓ select   Enter choose   Esc/q dismiss ",
        OverlayMode::ConfirmCancel        => " Enter/y CANCEL THE AGENT   Esc/q back ",
        OverlayMode::Budget { .. }        => " type a number   Enter submit   Esc back ",
        OverlayMode::ConfirmBudget { .. } => " Enter/y confirm   Esc back to the field ",
        OverlayMode::InFlight { .. }      => " working — keys are ignored until agentd answers ",
        OverlayMode::Result { .. }        => " Esc/q/Enter dismiss ",
        OverlayMode::Help                 => " Esc/q/Enter/? close ",
    }
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
/// ux.10: render a single-line `tui_input` value into a label string with the cursor
/// glyph drawn at the input's ACTUAL cursor position (not appended at the end), so
/// Left/Right/Home/End/Ctrl-A movement is reflected on screen. The split point is the
/// char-index cursor (`Input::cursor()`) — always a valid char boundary — so this is
/// width-/multibyte-safe and never panics (empty value → glyph at col 0; cursor at end →
/// glyph at the tail, matching the old look). Used for the inline label-style search /
/// reason fields; the converse rail uses a real terminal cursor via `set_cursor_position`.
fn input_with_cursor_glyph(input: &tui_input::Input, glyph: char) -> String {
    let value    = input.value();
    let byte_idx = value
        .char_indices()
        .nth(input.cursor())
        .map_or(value.len(), |(i, _)| i);
    let (head, tail) = value.split_at(byte_idx);
    format!("{head}{glyph}{tail}")
}

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
    // ux.10: draw the plain value (horizontally scrolled so a long line keeps the cursor
    // in view), then place the REAL terminal cursor at the input's actual column while the
    // rail is focused. Codex /review caught the prior `{value}█` glyph-append: once cursor
    // movement (Left/Home/Ctrl-A) was possible, the appended glyph lied about the edit
    // position. `visual_cursor`/`visual_scroll` are width-aware; an unfocused rail draws no
    // cursor.
    let inner_width = input_area.width.saturating_sub(2).max(1) as usize; // minus borders
    let scroll = app.converse_view.input.visual_scroll(inner_width);
    f.render_widget(
        Paragraph::new(app.converse_view.input.value())
            .scroll((0, scroll as u16))
            .block(Block::default().borders(Borders::ALL).border_style(border_style)),
        input_area,
    );
    if app.converse_view.rail_focused {
        let cursor_col = app.converse_view.input.visual_cursor().saturating_sub(scroll) as u16;
        f.set_cursor_position((input_area.x + 1 + cursor_col, input_area.y + 1));
    }
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
    let sq = app.memory_view.search_query.value();
    let search_line = if app.memory_view.search_active {
        format!("Search: {}", input_with_cursor_glyph(&app.memory_view.search_query, '_'))
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
    let q      = app.memory_view.search_query.value();
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
    let q       = app.memory_view.search_query.value();
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

    let q      = app.memory_view.search_query.value();
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
    // ux.10: render the multi-line `tui_textarea` inside the bordered block. The
    // textarea owns its own cursor + placeholder ("(empty — Tab to focus …)") and
    // scrolls internally, so the block just supplies the border + focus color.
    let task_inner = task_block.inner(task_area);
    f.render_widget(task_block, task_area);
    f.render_widget(&sv.task_input, task_inner);

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
        format!(" › search: {}", input_with_cursor_glyph(&app.inspector_view.search_query, '_'))
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

/// ux.10-A: stable per-service colour so the eye can follow one service through an
/// interleaved tail. A byte sum, not a real hash — enough to spread a handful of compose
/// services across the palette, and stable across restarts (a colour that moved between
/// sessions would be worse than no colour).
const SERVICE_COLORS: [Color; 6] = [
    Color::Cyan,
    Color::Green,
    Color::Magenta,
    Color::Yellow,
    Color::Blue,
    Color::LightRed,
];

fn service_color(name: &str) -> Color {
    let sum: usize = name.bytes().map(usize::from).sum();
    SERVICE_COLORS[sum % SERVICE_COLORS.len()]
}

/// Fixed gutter widths so payloads line up into a readable column regardless of service
/// name length (the whole point of keeping compose's prefix).
const LOG_SERVICE_COL: usize = 12;
const LOG_TS_COL: usize = 8;

/// Left-align into exactly `width` display cells, ellipsizing an over-long value.
fn pad_or_clip(s: &str, width: usize) -> String {
    if s.chars().count() > width {
        let keep = width.saturating_sub(1);
        return s.chars().take(keep).chain(std::iter::once('…')).collect();
    }
    format!("{s:<width$}")
}

/// Max characters of the search query shown in the Logs header.
const MAX_HEADER_QUERY_CHARS: usize = 80;

/// Render an over-long query as a window AROUND THE CURSOR, with `…` marking each cut side.
///
/// Not tail-keeping: with the tail kept, pressing Home on a 200-char query and typing put the
/// cursor glyph inside the discarded prefix, so the header froze and every keystroke was
/// invisible — the same "cursor isn't where the edit is" defect /review caught in sub-part B,
/// reintroduced through the clip (/review's red-team pass). Slicing to the window first also
/// makes the per-frame work O(window) instead of two full-length Strings, which is what the
/// clip was there for in the first place.
fn header_query_window(input: &tui_input::Input, glyph: char) -> String {
    let value: Vec<char> = input.value().chars().collect();
    if value.len() < MAX_HEADER_QUERY_CHARS {
        return input_with_cursor_glyph(input, glyph);
    }
    let cursor = input.cursor().min(value.len());
    // Room for the glyph plus a leading/trailing ellipsis.
    let span  = MAX_HEADER_QUERY_CHARS.saturating_sub(3);
    let half  = span / 2;
    let start = cursor.saturating_sub(half);
    let end   = (start + span).min(value.len());
    let start = end.saturating_sub(span);
    let mut out = String::with_capacity(MAX_HEADER_QUERY_CHARS + 2);
    if start > 0 {
        out.push('…');
    }
    out.extend(&value[start..cursor]);
    out.push(glyph);
    out.extend(&value[cursor..end]);
    if end < value.len() {
        out.push('…');
    }
    out
}

/// Head-keeping clip for a COMMITTED query (no cursor to track — what was searched for is the
/// useful part).
fn clip_header_query(q: &str) -> String {
    let mut chars = q.chars();
    let out: String = chars.by_ref().take(MAX_HEADER_QUERY_CHARS).collect();
    if chars.next().is_some() {
        return format!("{out}…");
    }
    out
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// ux.10-A: the `[l]` Logs view — a bounded tail of `docker compose logs`, service-filtered
/// with `Tab`, searched with `/` (highlight + `n`/`N`, NOT a filter — a matching log line is
/// only useful with its surrounding context).
/// Below this height the content block is all border and no payload rows, so the view would
/// render an empty box while its header claimed thousands of lines and every scroll key mutated
/// an invisible viewport. Guarded like `render_memory`'s `MIN_MEMORY_WIDTH` (/review's red-team
/// pass): header 1 + footer 1 + 2 border rows + at least 2 usable rows.
const MIN_LOGS_HEIGHT: u16 = 6;

/// Minimum frame the Jobs view's confirm/in-flight/result box can be drawn in — same
/// `overlay_fits` predicate the Dashboard's row-action overlay uses, against THIS view's own
/// (simpler, no chat-rail) chrome instead of `dashboard_chrome_rows`.
fn overlay_fits_jobs(term_size: (u16, u16)) -> bool {
    let (w, h) = term_size;
    // header (1) + table border (2) + footer (1) — conservative on purpose, same "fail closed,
    // never enable a box that can't be seen" direction as overlay_fits_dashboard.
    const JOBS_CHROME_ROWS: u16 = 4;
    overlay_fits(w, h.saturating_sub(JOBS_CHROME_ROWS))
}

/// UTC epoch seconds → an explicit-UTC human string. Never a bare "08:00": this project has no
/// local-timezone concept anywhere (attn.4 DX finding — the whole reason `agentctl jobs`
/// prints "(UTC)" too), and a bare timestamp here would launder that gap right back in.
fn format_fire_ts(ts: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| format!("{ts} (unparseable)"))
}

fn render_jobs(f: &mut Frame, app: &App) {
    let (header_area, content_area, footer_area) = header_footer_layout(f.area());

    let title = Span::styled(" Jobs ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(Paragraph::new(Line::from(vec![title])), header_area);

    if app.jobs.is_empty() {
        let msg = if app.error.is_some() {
            format!("error: {}", sanitize(app.error.as_deref().unwrap_or("")))
        } else {
            "no [[jobs]] with a schedule declared — or this source has no job data \
             (FUSE has no producer yet; connect with --url to see jobs)".to_string()
        };
        f.render_widget(
            Paragraph::new(msg).block(Block::default().borders(Borders::ALL).title(" jobs ")),
            content_area,
        );
    } else {
        let header_row = Row::new(vec![
            Cell::from("Job ID").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Schedule").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Next fire").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Last outcome").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Mode").style(Style::default().add_modifier(Modifier::BOLD)),
        ]).style(Style::default().bg(Color::DarkGray));

        let rows: Vec<Row> = app.jobs.iter().enumerate().map(|(i, j)| {
            let is_sel = i == app.jobs_selected;
            let bg = if is_sel { Color::Blue } else { Color::Reset };
            let outcome_style = match j.last_outcome.as_str() {
                "fired" | "caught_up" => Style::default().fg(Color::Green),
                "skipped" => Style::default().fg(Color::Red),
                "shadow_logged" => Style::default().fg(Color::Yellow),
                _ => Style::default().fg(Color::DarkGray),
            };
            // Second line under the skip reason, when there is one — same "why", not just
            // "what happened" idiom the Dashboard's attention-reason line uses.
            let mut outcome_lines = vec![Line::from(if j.last_outcome.is_empty() {
                "(never)".to_string()
            } else {
                j.last_outcome.clone()
            })];
            let mut outcome_height = if let Some(reason) = j.last_skip_reason.as_deref().filter(|r| !r.is_empty()) {
                outcome_lines.push(Line::from(Span::styled(
                    format!("  {}", sanitize(reason)),
                    Style::default().fg(Color::DarkGray),
                )));
                2
            } else {
                1
            };
            // attn.2-R5 fix (/autoplan retroactive review): last_outcome above is sourced
            // from the occurrence ledger, which a manual fire deliberately never touches —
            // so this column is structurally incapable of ever reflecting one on its own.
            // jobs_last_manual_fire is the session-local, client-side acknowledgement that
            // closes that gap without perturbing the ledger.
            if let Some((_child_id, fired_at)) = app.jobs_last_manual_fire.get(&j.job_id) {
                // Child id deliberately omitted here — the 28-col "Last outcome" column has
                // no room for a nanosecond-suffixed id (the Result overlay already showed
                // the full id when the fire happened); this row exists only to acknowledge
                // "yes, that just happened", which "manually fired Ns ago" does on its own.
                outcome_lines.push(Line::from(Span::styled(
                    format!("  manually fired {}s ago", fired_at.elapsed().as_secs()),
                    Style::default().fg(Color::Cyan),
                )));
                outcome_height += 1;
            }
            let mode_text = if j.shadow_mode { "shadow" } else { "live" };
            let mode_style = if j.shadow_mode {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            };
            Row::new(vec![
                Cell::from(sanitize(&j.job_id)),
                Cell::from(sanitize(&j.schedule_described)),
                Cell::from(format_fire_ts(j.next_fire_ts)),
                Cell::from(ratatui::text::Text::from(outcome_lines)).style(outcome_style),
                Cell::from(mode_text).style(mode_style),
            ]).style(Style::default().bg(bg)).height(outcome_height)
        }).collect();

        let table = Table::new(
            rows,
            [
                Constraint::Min(16),    // Job ID
                Constraint::Length(18), // Schedule
                Constraint::Length(24), // Next fire — format_fire_ts is 23 chars ("...UTC"); +1
                                        // margin (/autoplan retroactive review: 22 clipped
                                        // "UTC" to "UT" on every single row, deterministically)
                Constraint::Length(28), // Last outcome (+ skip reason on a second line)
                Constraint::Length(8),  // Mode
            ],
        )
        .header(header_row)
        .block(Block::default().borders(Borders::ALL).title(" jobs "));
        f.render_widget(table, content_area);
    }

    let footer = Line::from(Span::styled(
        " [↑/k ↓/j] select  [f/Enter] fire now  [Esc/q] back ",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(footer), footer_area);

    if let Some(ov) = &app.jobs_overlay {
        render_jobs_overlay(f, app, ov, content_area);
    }
}

fn render_jobs_overlay(f: &mut Frame, app: &App, ov: &JobsOverlay, area: Rect) {
    // Degraded path for a terminal too small for a box — mirrors render_dashboard_overlay's
    // fail-closed direction exactly, against this view's own (simpler) chrome predicate.
    if !overlay_fits_jobs(app.term_size) {
        let line = format!(" {} — fire confirmation needs a bigger terminal; Esc/q dismisses ", sanitize(&ov.target_job_id));
        let row = Rect { x: area.x, y: area.y + area.height.saturating_sub(1), width: area.width, height: 1 };
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(Color::Yellow).fg(Color::Black)),
            row.intersection(area),
        );
        return;
    }

    let mut body: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("job  ", Style::default().fg(Color::DarkGray)),
            Span::styled(sanitize(&ov.target_job_id), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
    ];

    match &ov.mode {
        JobOverlayMode::ConfirmFire => {
            // /autoplan retroactive review (2026-08-07): this text used to be identical
            // regardless of the row's own shadow/live mode, understating the risk on exactly
            // the jobs where it mattered most. Now scaled by it, and updated to reflect the
            // concurrent-fire guard's fix — that risk is refused server-side now, not raced.
            let shadow = app.jobs.iter().find(|j| j.job_id == ov.target_job_id).map(|j| j.shadow_mode).unwrap_or(false);
            let risk = if shadow {
                "This job is in SHADOW mode — the automatic scheduler never dispatches it, so \
                 there is no risk of racing a concurrent scheduled fire."
            } else {
                "This job is LIVE — if a run is already in progress (scheduled or manual), \
                 this fire will be REFUSED, not raced, so there is no risk of two concurrent runs."
            };
            for line in wrap_plain(
                &format!(
                    "Fire '{}' now? This calls its real capabilities immediately — live Gmail/KB \
                     writes, whatever this job is configured to do — ignoring shadow mode. {risk} \
                     Separately, if this job already fired today, firing it again still \
                     OVERWRITES today's real data with this run's (last-writer-wins on the \
                     job's date-keyed KB key) — this is NOT guarded, only warned about here.",
                    sanitize(&ov.target_job_id),
                ),
                overlay_text_width(area),
            ) {
                body.push(Line::from(Span::styled(line, Style::default().fg(Color::Yellow))));
            }
        }
        JobOverlayMode::InFlight => {
            body.push(Line::from(format!("firing {}…", sanitize(&ov.target_job_id))));
            body.push(Line::from(""));
            body.push(Line::from(Span::styled(
                "Keys are ignored until this completes.",
                Style::default().fg(Color::DarkGray),
            )));
        }
        JobOverlayMode::Result { text, ok } => {
            let style = if *ok { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Red) };
            for line in wrap_plain(text, overlay_text_width(area)) {
                body.push(Line::from(Span::styled(line, style)));
            }
        }
    }
    body.push(Line::from(""));

    let hints = Line::from(Span::styled(
        match &ov.mode {
            JobOverlayMode::ConfirmFire => " [y] fire now  [n/Esc] cancel ",
            JobOverlayMode::InFlight    => " working… ",
            JobOverlayMode::Result { .. } => " any key dismisses ",
        },
        Style::default().bg(Color::DarkGray).fg(Color::White),
    ));

    let rect = overlay_rect(area, body.len() as u16 + 3);
    if rect.is_empty() {
        return;
    }
    let inner_rows = rect.height.saturating_sub(2) as usize;
    if inner_rows > 0 {
        body.truncate(inner_rows - 1);
        body.push(hints);
    } else {
        body.clear();
    }
    let title = match &ov.mode {
        JobOverlayMode::ConfirmFire    => " confirm fire ",
        JobOverlayMode::InFlight       => " working ",
        JobOverlayMode::Result { .. }  => " result ",
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(title),
        ),
        rect,
    );
}

fn render_logs(f: &mut Frame, app: &App) {
    let (header_area, content_area, footer_area) = header_footer_layout(f.area());
    let lv = &app.logs_view;

    if f.area().height < MIN_LOGS_HEIGHT {
        f.render_widget(
            Paragraph::new(format!(
                " logs need at least {MIN_LOGS_HEIGHT} rows — resize, or Esc/q to go back "
            ))
            .style(Style::default().bg(Color::DarkGray).fg(Color::White)),
            f.area(),
        );
        return;
    }

    // The same helper the key handler uses (`logs::logs_viewport_rows`) — fed the frame height
    // here and `App::term_size` there. Those agree because the `Event::Resize` arm keeps
    // `term_size` equal to the frame size; only the arithmetic is genuinely shared, so this is
    // a maintained invariant, not a structural guarantee (/review's maintainability pass
    // corrected an earlier "can never disagree" claim here). The debug_assert below pins the
    // chrome arithmetic against the layout actually used.
    let rows = logs_viewport_rows(f.area().height);
    debug_assert_eq!(
        rows,
        content_area.height.saturating_sub(2).max(1) as usize,
        "logs_viewport_rows drifted from header_footer_layout + the content block's borders"
    );
    let indices = lv.visible_indices();
    // Reuses `indices.len()` instead of re-walking the ring inside `filtered_len()`.
    let scroll = lv.effective_scroll_for_len(indices.len(), rows);
    let now    = now_unix();
    // ONE match pass per frame, reusing `indices`. Previously the header count called
    // `match_positions()` (which re-derived `visible_indices()` and re-scanned every stored
    // line) and each visible row called `matches_search()` again — measured by /review's
    // performance pass at 1.2 ms/frame typical and 9.5 ms with 4 KB lines, against a 30 ms
    // tick. `matches` is indexed by position within `indices`, so the body reuses it directly.
    let matches: Vec<bool> = if lv.search_query.value().is_empty() {
        Vec::new()
    } else {
        indices
            .iter()
            .map(|&i| lv.lines.get(i).is_some_and(|l| lv.matches_search(l)))
            .collect()
    };
    let match_count = matches.iter().filter(|m| **m).count();

    // ── header: filter cycle, line counts, drop accounting, search state ──
    let filters = lv
        .filter_labels()
        .iter()
        .enumerate()
        .map(|(i, name)| {
            if i == lv.filter_idx {
                format!("[{}]", sanitize(name))
            } else {
                sanitize(name)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let counts = if lv.active_filter().is_some() {
        format!("{} of {} lines", indices.len(), lv.lines.len())
    } else {
        format!("{} lines", lv.lines.len())
    };
    // Never silent: lines lost to backpressure are stated, not quietly missing.
    let dropped = if lv.dropped > 0 {
        format!("  ⚠ {} dropped", lv.dropped)
    } else {
        String::new()
    };
    // The query is operator input (possibly a large paste), so it is clipped FIRST and only
    // then sanitized — clipping last still built two full-length Strings per frame, so the
    // stated "a 100 KB paste must not build a 100 KB line every frame" guard did not actually
    // hold (found by /review's performance pass).
    let search = if lv.search_active {
        // Sanitized on the EDITING path too, not just the committed one: bracketed paste can
        // put control bytes into the field, and an unsanitized live query would put them
        // straight into the header line (found by /review round 2).
        format!("   search: {}", sanitize(&header_query_window(&lv.search_query, '_')))
    } else if !lv.search_query.value().is_empty() {
        format!(
            "   /{} ({match_count} match{})",
            sanitize(&clip_header_query(lv.search_query.value())),
            if match_count == 1 { "" } else { "es" }
        )
    } else {
        String::new()
    };
    f.render_widget(
        Paragraph::new(format!(
            " agentctl watch › logs  {filters} — {counts}{dropped}{search} "
        ))
        .style(Style::default().bg(Color::DarkGray).fg(Color::White)),
        header_area,
    );

    // ── body: service | age | payload ──
    let mut body: Vec<Line> = indices
        .iter()
        .enumerate()
        .skip(scroll)
        .take(rows)
        .filter_map(|(pos, &i)| lv.lines.get(i).map(|l| (pos, l)))
        .map(|(pos, l)| {
            // agentctl's own status lines are never attributed to a service.
            if l.notice {
                return Line::from(Span::styled(
                    sanitize(&l.text),
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                ));
            }
            // Every untrusted-derived column is sanitized, not just the payload: the service
            // name comes from the compose prefix and the timestamp column can fall back to raw
            // stamp bytes, so both are container-controlled too (/review's security pass found
            // this asymmetry — ratatui filters control chars as a second layer, but the plain
            // renderer does not, so the guard belongs here).
            let svc = l.service.as_deref().unwrap_or("-");
            let ts  = format_ts(l.ts.as_deref(), lv.absolute_ts, now);
            let payload_style = if matches.get(pos).copied().unwrap_or(false) {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(
                    sanitize(&pad_or_clip(svc, LOG_SERVICE_COL)),
                    Style::default().fg(service_color(svc)),
                ),
                Span::styled(
                    format!(" {:>w$}  ", sanitize(&ts), w = LOG_TS_COL),
                    Style::default().fg(Color::DarkGray),
                ),
                // Clip BEFORE sanitize: sanitizing first walked and allocated the whole stored
                // record (up to 4 KB) only for ~87% of it to be thrown away.
                Span::styled(sanitize(&clip_payload(&l.text)), payload_style),
            ])
        })
        .collect();
    if body.is_empty() {
        // Distinguish "the tail has produced nothing" from "this filter matches nothing" — the
        // header simultaneously says e.g. "0 of 1500 lines", so a single message contradicted
        // it and read as a dead tail (/review's maintainability pass).
        let msg = match lv.active_filter() {
            Some(svc) if !lv.lines.is_empty() => {
                format!("  (no lines from '{}' yet — Tab to change filter)", sanitize(svc))
            }
            _ => "  (no output yet — tailing `docker compose logs`)".to_string(),
        };
        body.push(Line::from(Span::styled(msg, Style::default().fg(Color::DarkGray))));
    }
    // Name the project in the title. The compose project comes from agentctl's CWD while the
    // rest of the cockpit describes whatever `--url`/FUSE points at, so these can be different
    // machines — an unlabelled title let local container output read as the watched host's
    // (/review's red-team pass).
    let title = if lv.project.is_empty() {
        " docker compose logs ".to_string()
    } else {
        format!(" docker compose logs — {} ", sanitize(&lv.project))
    };
    f.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(title)),
        content_area,
    );

    // ── footer: keys + follow state (the one piece of state the body can't show) ──
    let follow = if lv.follow { "FOLLOW" } else { "PAUSED — [G] to follow" };
    let hints = format!(
        " Tab:service  [/]search  n/N:match  ↑/↓ scroll  [g]/[G] top/bottom  [t]ime:{}  Esc/q back — {follow} ",
        if lv.absolute_ts { "abs" } else { "rel" }
    );
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
            // ux.13-TUI step 4: render from the PINNED id (`App::confirm_item`), the same resolver the
            // key handler acts through. Reading `approvals_items[selected_idx]` here meant an item
            // resolving out-of-band could shift the list under a live dialog, showing one approval's
            // risk/summary while `[a]` approved another.
            let lines = match app.confirm_item() {
                Some(a) => {
                    let risk_style = match a.risk.as_str() {
                        "high"   => Style::default().fg(Color::Red),
                        "medium" => Style::default().fg(Color::Yellow),
                        _        => Style::default().fg(Color::Green),
                    };
                    vec![
                        Line::from(""),
                        Line::from(vec![
                            Span::styled("  ID:      ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(a.id.as_str()),
                        ]),
                        Line::from(vec![
                            Span::styled("  Agent:   ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(a.agent_id.as_str()),
                        ]),
                        Line::from(vec![
                            Span::styled("  Kind:    ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(a.kind.as_str()),
                        ]),
                        Line::from(vec![
                            Span::styled("  Risk:    ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::styled(a.risk.as_str(), risk_style),
                        ]),
                        Line::from(vec![
                            Span::styled("  Summary: ", Style::default().add_modifier(Modifier::BOLD)),
                            Span::raw(sanitize(&a.summary)),
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
                    ]
                }
                // The former `unwrap_or(("?", "?", "?", "?", "?"))` case, said out loud: the pinned
                // approval is no longer pending, so there is nothing here to approve or reject. The
                // keys are no-ops that return to the list — the body must not imply otherwise.
                None => {
                    // sanitize: approval ids are internally generated today, but this is the last
                    // unsanitized id rendered in a dialog and the next id source may not be.
                    let pinned = sanitize(av.confirmed_id.as_deref().unwrap_or("(none)"));
                    vec![
                        Line::from(""),
                        Line::from(vec![
                            Span::styled("  This approval was already resolved.",
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                        ]),
                        Line::from(""),
                        Line::from(format!("  {pinned} is no longer pending — it was approved or")),
                        Line::from("  rejected elsewhere (Telegram, agentctl, or it expired)."),
                        Line::from(""),
                        Line::from("  Press Esc to go back to the list."),
                    ]
                }
            };

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
                Line::from(format!("  > {}", input_with_cursor_glyph(&av.reject_reason, '_'))),
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

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::watch::app::App;
    use crate::watch::reader::{AgentInfo, BudgetKind, PendingAction, Snapshot, SysBudget, SysProvider, SysQueue};

    /// Render one view into a `TestBackend` and flatten the buffer to text, one line per row.
    ///
    /// The only way to test what the operator actually SEES. Introduced for ux.13-TUI step 4: the
    /// approvals Confirm dialog resolved its display by index while acting by pinned id, and no
    /// pure-function test can catch a divergence that lives in the renderer.
    fn render_to_text(app: &App, w: u16, h: u16, view: fn(&mut Frame, &App)) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).expect("test backend");
        term.draw(|f| view(f, app)).expect("draw");
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ── ux.10-A: Logs view formatters + the widened control-char guard ───────

    /// `sanitize` was widened in ux.10-A specifically because container stdout is untrusted
    /// bytes. Only the C0 class had any coverage, so reverting the widening would have passed
    /// every test while restoring the injection vector (/review's testing pass).
    #[test]
    fn sanitize_strips_c0_del_and_c1_but_keeps_tab_and_printables() {
        assert_eq!(sanitize("a\x1bb\x07c"), "abc", "C0 incl. ESC and BEL");
        assert_eq!(sanitize("a\u{7f}b"), "ab", "DEL");
        assert_eq!(sanitize("a\u{80}b\u{9b}c\u{9f}d"), "abcd", "C1 range incl. CSI + bounds");
        assert_eq!(sanitize("a\tb"), "a\tb", "tab is deliberately kept");
        assert_eq!(sanitize("a\u{a0}é🚀b"), "a\u{a0}é🚀b", "non-controls survive");
    }

    #[test]
    fn logs_gutter_formatters_respect_their_width_contracts() {
        assert_eq!(pad_or_clip("cos", LOG_SERVICE_COL).chars().count(), LOG_SERVICE_COL);
        assert_eq!(
            pad_or_clip(&"a".repeat(LOG_SERVICE_COL), LOG_SERVICE_COL).chars().count(),
            LOG_SERVICE_COL
        );
        let clipped = pad_or_clip(&"a".repeat(LOG_SERVICE_COL + 5), LOG_SERVICE_COL);
        assert_eq!(clipped.chars().count(), LOG_SERVICE_COL);
        assert!(clipped.ends_with('…'));
        // Multibyte names stay char-count-correct (display width is a known caveat).
        assert_eq!(pad_or_clip("日本語サービス名", LOG_SERVICE_COL).chars().count(), LOG_SERVICE_COL);
    }

    #[test]
    fn service_colour_is_stable_and_spreads_across_the_palette() {
        assert_eq!(service_color("cos"), service_color("cos"), "stable across calls");
        let distinct: std::collections::HashSet<_> =
            ["cos", "agent", "qdrant"].iter().map(|s| service_color(s)).collect();
        assert!(distinct.len() >= 2, "real service names should not all collide");
    }

    /// The header window must keep the CURSOR visible on a long query — tail-keeping put the
    /// glyph inside the discarded prefix, so editing at the start showed no feedback at all
    /// (/review's red-team pass).
    #[test]
    fn header_query_window_keeps_the_cursor_visible_wherever_it_is() {
        let long = "q".repeat(MAX_HEADER_QUERY_CHARS * 3);
        // Cursor at the very start (Home).
        let at_start = header_query_window(&tui_input::Input::new(long.clone()).with_cursor(0), '_');
        assert!(at_start.starts_with('_'), "cursor must be visible at the head: {at_start}");
        assert!(at_start.ends_with('…'), "and the cut tail marked");
        assert!(at_start.chars().count() <= MAX_HEADER_QUERY_CHARS + 2);
        // Cursor in the middle.
        let mid_pos = long.chars().count() / 2;
        let mid = header_query_window(
            &tui_input::Input::new(long.clone()).with_cursor(mid_pos),
            '_',
        );
        assert!(mid.contains('_'), "cursor visible mid-query");
        assert!(mid.starts_with('…') && mid.ends_with('…'), "both cuts marked: {mid}");
        // Short queries are untouched (identical to the plain glyph rendering).
        let short = tui_input::Input::new("abc".to_string());
        assert_eq!(header_query_window(&short, '_'), input_with_cursor_glyph(&short, '_'));
    }

    #[test]
    fn committed_header_query_keeps_the_head_and_marks_the_cut() {
        let exact = "q".repeat(MAX_HEADER_QUERY_CHARS);
        assert_eq!(clip_header_query(&exact), exact, "at the boundary nothing is clipped");
        let over = "q".repeat(MAX_HEADER_QUERY_CHARS + 10);
        let out  = clip_header_query(&over);
        assert_eq!(out.chars().count(), MAX_HEADER_QUERY_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    // ── ux.10: cursor glyph is drawn at the input's actual position ──────────
    // (Codex /review: appending the glyph at .value()'s end lied about the edit
    // position once Left/Home/Ctrl-A cursor movement became possible.)

    #[test]
    fn input_cursor_glyph_at_end_by_default() {
        let input = tui_input::Input::new("abc".to_string()); // cursor parks at end
        assert_eq!(input_with_cursor_glyph(&input, '_'), "abc_");
    }

    #[test]
    fn input_cursor_glyph_empty_value_is_glyph_only() {
        let input = tui_input::Input::default();
        assert_eq!(input_with_cursor_glyph(&input, '_'), "_", "no panic on empty; glyph at col 0");
    }

    #[test]
    fn input_cursor_glyph_follows_left_and_home_movement() {
        use tui_input::backend::crossterm::EventHandler;
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut input = tui_input::Input::new("abc".to_string());
        let before = input.visual_cursor();

        // One Left: cursor moves before 'c' → glyph is drawn between 'b' and 'c'.
        input.handle_event(&Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        assert_eq!(input.visual_cursor(), before - 1, "Left must move the cursor left");
        assert_eq!(input_with_cursor_glyph(&input, '_'), "ab_c",
            "glyph must render at the cursor position, not appended at the end");

        // Home: cursor to col 0 → glyph leads the value.
        input.handle_event(&Event::Key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)));
        assert_eq!(input.visual_cursor(), 0, "Home must move the cursor to column 0");
        assert_eq!(input_with_cursor_glyph(&input, '_'), "_abc");
    }

    #[test]
    fn input_cursor_glyph_multibyte_split_is_char_safe() {
        // Wide/multibyte chars: splitting by char index (not display column) must stay
        // on a valid char boundary and never panic.
        let mut input = tui_input::Input::new("héllo".to_string());
        // Move Left twice from the end: cursor sits before "lo" (after "hél").
        use tui_input::backend::crossterm::EventHandler;
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        input.handle_event(&Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        input.handle_event(&Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        assert_eq!(input_with_cursor_glyph(&input, '_'), "hél_lo");
    }

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
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("agents: (none)"), "empty agent list must produce '(none)' line");
    }

    #[test]
    fn render_plain_no_provider_omits_provider_line() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(!out.contains("provider:"), "no provider → no provider line");
    }

    #[test]
    fn render_plain_no_budget_omits_tokens_line() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(!out.contains("tokens_spent:"), "no budget → no tokens_spent line");
    }

    // ── render_plain: error field ────────────────────────────────────────────

    #[test]
    fn render_plain_error_appears_in_output() {
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None,
            isolation: None, credentials: None, jobs: vec![], error: Some("permission denied".to_string()),
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
            isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("provider: claude-opus-4 (anthropic)"));
    }

    #[test]
    fn render_plain_includes_tokens_spent_when_budget_present() {
        let snap = Snapshot {
            agents: vec![],
            budget: Some(SysBudget { spent: 99_000, total: 0, resettable: false }),
            queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("tokens_spent: 99000"));
    }

    #[test]
    fn render_plain_includes_queue_depth_when_queue_present() {
        let snap = Snapshot {
            agents: vec![], budget: None,
            queue: Some(SysQueue { depth: 7 }),
            sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            sandbox: Some(sb), provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            sandbox: Some(sb), provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            sandbox: Some(sb), provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
                budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
            };
            let out = render_plain(&app_from_snap(snap));
            assert!(out.contains(&format!("[{status}]")),
                "status '{status}' must appear in render_plain output");
        }
    }

    // ── Memory view: render_plain ────────────────────────────────────────────

    fn tmpdir() -> tempfile::TempDir { tempfile::tempdir().unwrap() }

    fn empty_snap() -> Snapshot {
        Snapshot { agents: vec![], budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None }
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            jobs: vec![],
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
            jobs: vec![],
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
            search_query: tui_input::Input::new("arch".to_string()),
            ..Default::default()
        };
        assert!(state.search_active);
        assert_eq!(state.search_query.value(), "arch");
        // Closing search leaves query in place (user can re-open and see it).
        // Pressing [/] again would set search_active=true again.
        state.search_active = false;
        assert!(!state.search_active);
        assert_eq!(state.search_query.value(), "arch", "query must persist after closing search mode");
    }

    // ── render_plain: isolation tier ────────────────────────────────────────

    #[test]
    fn render_plain_isolation_none_omits_isolation_lines() {
        use crate::watch::reader::Snapshot;
        let snap = Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None,
            provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            provider: None, credentials: None, jobs: vec![], error: None,
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
            provider: None, credentials: None, jobs: vec![], error: None,
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
            provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            provider: None, isolation: None, jobs: vec![], error: None,
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
            provider: None, isolation: None, jobs: vec![], error: None,
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
            provider: None, isolation: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
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
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let out = render_plain(&app_from_snap(snap));
        assert!(out.contains("degraded (brave_search) · active"), "since:0 sentinel must show 'active': {out}");
        assert!(!out.contains("0s ago"), "must never render the misleading '0s ago' for a never-tracked onset: {out}");
    }

    // ── ux.13-TUI step 4: the Approvals confirm dialog renders the PINNED item ───────

    fn approval(id: &str, kind: &str, risk: &str, summary: &str) -> PendingAction {
        PendingAction {
            id:       id.to_string(),
            agent_id: "cos-inbox".to_string(),
            kind:     kind.to_string(),
            risk:     risk.to_string(),
            summary:  summary.to_string(),
            args:     serde_json::Value::Null,
            age_secs: 12,
        }
    }

    /// Two approvals, the dialog pinned to the SECOND while the highlight still points at the first —
    /// exactly the state `update_approvals` produces when an item resolves out-of-band (Telegram,
    /// `agentctl approve`, expiry) while the dialog is up: the list is replaced, only `selected_idx`
    /// is clamped, and the pin is untouched.
    ///
    /// Negative control: reverting the renderer to `approvals_items.get(av.selected_idx)` makes this
    /// fail on every assertion — it would display the low-risk item while `[a]` approved the high-risk
    /// one. That mismatch is a security defect, not a cosmetic one: risk is the field the operator
    /// reads before granting.
    #[test]
    fn approvals_confirm_renders_the_pinned_item_not_the_highlighted_index() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.update_approvals(vec![
            approval("act_9", "kb_write", "low", "append to the changelog segment"),
            approval("act_1", "shell_exec", "high", "rm -rf /data/checkpoints"),
        ]);
        app.approvals_view.selected_idx = 0;                        // highlight = act_9
        app.approvals_view.confirmed_id = Some("act_1".to_string()); // pinned    = act_1
        app.approvals_view.mode = ApprovalsMode::Confirm;

        let out = render_to_text(&app, 100, 20, render_approvals);
        assert!(out.contains("act_1"), "must show the pinned id: {out}");
        assert!(out.contains("rm -rf /data/checkpoints"), "must show the pinned summary: {out}");
        assert!(out.contains("high"), "must show the pinned RISK — the field authority turns on: {out}");
        assert!(!out.contains("act_9"), "must not show the highlighted-but-unpinned item: {out}");
        assert!(!out.contains("append to the changelog"), "wrong summary shown: {out}");
        assert!(!out.contains("kb_write"), "wrong kind shown: {out}");
    }

    /// The other direction: the pinned approval is no longer pending. Previously this rendered
    /// `ID: ?  Agent: ?  Kind: ?  Risk: ?` — which reads as a live dialog with unknown fields, over
    /// keys that are silently no-ops. It must say what happened instead.
    #[test]
    fn approvals_confirm_says_already_resolved_when_the_pin_is_gone() {
        let mut app = App::new(PathBuf::from("/agents"));
        app.update_approvals(vec![approval("act_9", "kb_write", "low", "append to the changelog")]);
        app.approvals_view.confirmed_id = Some("act_1".to_string()); // resolved out-of-band
        app.approvals_view.mode = ApprovalsMode::Confirm;

        let out = render_to_text(&app, 100, 20, render_approvals);
        assert!(out.contains("already resolved"), "must state what happened: {out}");
        assert!(out.contains("act_1"), "must name the approval that went away: {out}");
        assert!(!out.contains("Risk:"), "must not render a live field set with '?' values: {out}");
        assert!(!out.contains("act_9"), "must never silently retarget to a surviving item: {out}");
    }

    /// E3, the render-thread panic guard, driven through a REAL frame rather than only the geometry
    /// helper. `Clear` indexes the buffer without intersecting it, so an out-of-frame rect aborts the
    /// render thread and drops the operator out of the cockpit with the runaway still running. Three
    /// sizes: below the box floor (degraded single line), just above it, and a normal terminal (the
    /// path that actually reaches `Clear`).
    ///
    /// **40×14 is the row that does the work**, and it is here because the negative control was run:
    /// with `overlay_rect` reverted to naive fixed-72-wide geometry, 10×3 / 34×8 / 80×24 / 200×50 all
    /// still PASS — the small frames never reach `Clear` (the degraded path catches them) and the wide
    /// ones fit a 72-col box. Only a frame wide enough to open the box yet narrower than the box
    /// panics. Do not drop that size.
    #[test]
    fn dashboard_overlay_renders_without_panicking_at_every_frame_size() {
        let snap = Snapshot {
            agents: vec![make_agent("scout-2", "running", 1_000, vec![])],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let mut app = app_from_snap(snap);
        app.dashboard_overlay = Some(crate::watch::overlay::DashboardOverlay::menu("scout-2"));

        for (w, h) in [(10, 3), (34, 8), (40, 14), (44, 11), (80, 24), (200, 50)] {
            let out = render_to_text(&app, w, h, render_dashboard);
            assert!(!out.is_empty(), "no frame drawn at {w}x{h}");
        }
    }

    // ── attn.2-R5: Jobs view + manual-fire overlay ────────────────────────────────────

    fn job_row(job_id: &str, last_outcome: &str, shadow: bool) -> reader::SysJob {
        reader::SysJob {
            job_id: job_id.to_string(),
            schedule_described: "0 8 * * * (UTC)".to_string(),
            next_fire_ts: 1_800_000_000,
            last_outcome: last_outcome.to_string(),
            last_skip_reason: None,
            shadow_mode: shadow,
        }
    }

    fn app_with_jobs(jobs: Vec<reader::SysJob>) -> App {
        app_from_snap(Snapshot {
            agents: vec![], budget: None, queue: None, sandbox: None, provider: None,
            isolation: None, credentials: None, jobs, error: None,
        })
    }

    #[test]
    fn render_jobs_empty_list_shows_a_message_not_a_panic() {
        let app = app_with_jobs(vec![]);
        let out = render_to_text(&app, 100, 30, render_jobs);
        assert!(out.contains("no [[jobs]]") || out.contains("no job data"));
    }

    #[test]
    fn render_jobs_lists_every_row_with_schedule_and_next_fire() {
        let app = app_with_jobs(vec![
            job_row("cos-inbox", "fired", true),
            job_row("cos-curator", "skipped", false),
        ]);
        let out = render_to_text(&app, 100, 30, render_jobs);
        assert!(out.contains("cos-inbox"));
        assert!(out.contains("cos-curator"));
        assert!(out.contains("2026") || out.contains("UTC"), "next fire must render as explicit UTC, not a bare timestamp:\n{out}");
        // /autoplan retroactive review: Constraint::Length(22) clipped "UTC" to "UT" on every
        // row, deterministically (format_fire_ts is 23 chars). Widened to 24 — this must
        // never regress to a bare "UT".
        assert!(out.contains("UTC"), "the column must not clip \"UTC\" down to \"UT\":\n{out}");
        assert!(!out.contains(" UT ") && !out.contains(" UT\n") && !out.contains(" UT│"),
            "must never render a clipped \"UT\" with no trailing C:\n{out}");
        assert!(out.contains("shadow"));
        assert!(out.contains("live"));
    }

    #[test]
    fn render_jobs_shows_a_manual_fire_even_though_last_outcome_never_will() {
        // /autoplan retroactive review (2026-08-07, CRITICAL): last_outcome is sourced from
        // the occurrence ledger, which a manual fire deliberately never touches — so this is
        // the fix, not a duplicate of render_jobs_lists_every_row_with_schedule_and_next_fire.
        let mut app = app_with_jobs(vec![job_row("cos-inbox", "", true)]);
        app.jobs_last_manual_fire.insert(
            "cos-inbox".to_string(),
            ("cos-inbox-manual-123".to_string(), std::time::Instant::now()),
        );
        let out = render_to_text(&app, 100, 30, render_jobs);
        assert!(out.contains("manually fired"), "the manual fire must be acknowledged in the row:\n{out}");
    }

    #[test]
    fn render_jobs_shows_the_skip_reason_when_present() {
        let mut job = job_row("cos-inbox", "skipped", true);
        job.last_skip_reason = Some("unknown job id".to_string());
        let app = app_with_jobs(vec![job]);
        let out = render_to_text(&app, 100, 30, render_jobs);
        assert!(out.contains("unknown job id"), "the skip reason must be visible, not just the bare outcome:\n{out}");
    }

    #[test]
    fn jobs_overlay_renders_without_panicking_at_every_frame_size() {
        let mut app = app_with_jobs(vec![job_row("cos-inbox", "", true)]);
        for mode in [
            JobOverlayMode::ConfirmFire,
            JobOverlayMode::InFlight,
            JobOverlayMode::Result { text: "Fired 'cos-inbox' — child 'cos-inbox-manual-1'".to_string(), ok: true },
        ] {
            app.jobs_overlay = Some(JobsOverlay { target_job_id: "cos-inbox".to_string(), mode });
            for (w, h) in [(10, 3), (34, 8), (40, 14), (44, 11), (80, 24), (200, 50)] {
                app.term_size = (w, h);
                let out = render_to_text(&app, w, h, render_jobs);
                assert!(!out.is_empty(), "no frame drawn at {w}x{h}");
            }
        }
    }

    #[test]
    fn jobs_overlay_confirm_frame_warns_about_the_same_day_overwrite_risk() {
        let mut app = app_with_jobs(vec![job_row("cos-inbox", "fired", true)]);
        app.term_size = (100, 30);
        app.jobs_overlay = Some(JobsOverlay {
            target_job_id: "cos-inbox".to_string(),
            mode: JobOverlayMode::ConfirmFire,
        });
        let out = render_to_text(&app, 100, 30, render_jobs);
        assert!(out.contains("OVERWRITES") || out.contains("overwrite"),
            "the confirm frame must state the attn.2-R5 same-day KB-overwrite risk, not just \"are you sure\":\n{out}");
        assert!(out.contains("shadow mode"), "must state that a manual fire ignores shadow mode:\n{out}");
        assert!(!out.contains("attn.2-R5 residual"), "internal ticket jargon must not leak into operator-facing copy:\n{out}");
    }

    #[test]
    fn jobs_overlay_confirm_frame_scales_by_shadow_vs_live_mode() {
        // /autoplan retroactive review: the warning used to be IDENTICAL regardless of the
        // row's own Mode, under-warning on exactly the live jobs where the concurrent-fire
        // race was real. Now scaled by it (and updated to reflect the guard's fix).
        let mut shadow_app = app_with_jobs(vec![job_row("cos-inbox", "", true)]);
        shadow_app.term_size = (100, 30);
        shadow_app.jobs_overlay = Some(JobsOverlay { target_job_id: "cos-inbox".to_string(), mode: JobOverlayMode::ConfirmFire });
        let shadow_out = render_to_text(&shadow_app, 100, 30, render_jobs);
        assert!(shadow_out.contains("SHADOW mode"), "{shadow_out}");

        let mut live_app = app_with_jobs(vec![job_row("cos-inbox", "", false)]);
        live_app.term_size = (100, 30);
        live_app.jobs_overlay = Some(JobsOverlay { target_job_id: "cos-inbox".to_string(), mode: JobOverlayMode::ConfirmFire });
        let live_out = render_to_text(&live_app, 100, 30, render_jobs);
        assert!(live_out.contains("LIVE"), "{live_out}");
        assert_ne!(shadow_out, live_out, "the two modes must render genuinely different copy, not the same boilerplate");
    }

    #[test]
    fn jobs_overlay_result_frame_shows_ok_and_error_distinctly() {
        for (ok, text) in [(true, "Fired 'cos-inbox' — child 'x'"), (false, "Fire failed: unknown job id")] {
            let mut app = app_with_jobs(vec![job_row("cos-inbox", "", true)]);
            app.term_size = (100, 30);
            app.jobs_overlay = Some(JobsOverlay {
                target_job_id: "cos-inbox".to_string(),
                mode: JobOverlayMode::Result { text: text.to_string(), ok },
            });
            let out = render_to_text(&app, 100, 30, render_jobs);
            assert!(out.contains(text) || out.contains(&text[..20]), "result text must render:\n{out}");
        }
    }

    #[test]
    fn jobs_overlay_too_small_degrades_to_a_single_line_not_a_panic() {
        let mut app = app_with_jobs(vec![job_row("cos-inbox", "", true)]);
        app.term_size = (20, 5);
        app.jobs_overlay = Some(JobsOverlay {
            target_job_id: "cos-inbox".to_string(),
            mode: JobOverlayMode::ConfirmFire,
        });
        let out = render_to_text(&app, 20, 5, render_jobs);
        // Truncated at 20 cols before "bigger terminal" — the degraded MESSAGE still starts
        // rendering (no panic, no silently-empty frame), which is what this guards.
        assert!(out.contains("cos-inbox") && out.contains("fire"), "degraded line must at least start rendering:\n{out}");
    }

    // ── ux.13-TUI step 5: every overlay mode has to be readable on a real frame ───────

    fn overlay_frame(mode: crate::watch::overlay::OverlayMode, spent: u64, w: u16, h: u16) -> String {
        let mut a = make_agent("scout-2", "running", 12_000, vec![]);
        a.windowed_spent = spent;
        a.budget = BudgetKind::Tokens(200_000);
        let snap = Snapshot {
            agents: vec![a],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let mut app = app_from_snap(snap);
        let mut ov = crate::watch::overlay::DashboardOverlay::menu("scout-2");
        ov.mode = mode;
        app.dashboard_overlay = Some(ov);
        render_to_text(&app, w, h, render_dashboard)
    }

    #[test]
    fn overlay_menu_frame_shows_all_three_verbs_and_the_live_row() {
        use crate::watch::overlay::OverlayMode;
        let out = overlay_frame(OverlayMode::Menu, 47_000, 120, 30);
        assert!(out.contains("scout-2"), "the pinned target's live row belongs inside the box: {out}");
        for verb in ["Park", "Set budget", "Cancel"] {
            assert!(out.contains(verb), "missing verb {verb}: {out}");
        }
        assert!(out.contains("Esc/q dismiss"), "every modal states its own exit: {out}");
    }

    /// E1's UI half: at zero spend Park is visibly unavailable AND says why, rather than silently
    /// doing something dangerous or silently doing nothing.
    #[test]
    fn overlay_menu_frame_explains_a_blocked_park() {
        use crate::watch::overlay::OverlayMode;
        let out = overlay_frame(OverlayMode::Menu, 0, 120, 30);
        assert!(out.contains("0 means unlimited"), "the reason must be on screen: {out}");
        assert!(out.contains("Use Cancel or set a positive budget"), "and the alternative: {out}");
    }

    #[test]
    fn overlay_confirm_cancel_frame_states_the_irreversibility_and_the_cli() {
        use crate::watch::overlay::OverlayMode;
        let out = overlay_frame(OverlayMode::ConfirmCancel, 47_000, 120, 30);
        assert!(out.contains("cannot be undone"), "{out}");
        assert!(out.contains("agentctl cancel scout-2"),
            "the CLI equivalent is what makes the TUI teachable and the incident note copy-pasteable: {out}");
    }

    /// C4/E6: the confirm must show the CASCADE, not just the id the operator selected. A dialog naming
    /// one agent while the scheduler flags three is the blast radius nobody was shown.
    #[test]
    fn overlay_confirm_cancel_frame_shows_the_subtree_it_will_also_stop() {
        use crate::watch::overlay::{DashboardOverlay, OverlayMode};
        let mut coordinator = make_agent("cos-coordinator", "running", 31_000, vec![]);
        coordinator.windowed_spent = 31_000;
        let mut s1 = make_agent("scout-1", "running", 1_000, vec![]);
        s1.parent_id = Some("cos-coordinator".to_string());
        let mut s2 = make_agent("scout-2", "running", 1_000, vec![]);
        s2.parent_id = Some("cos-coordinator".to_string());
        let snap = Snapshot {
            agents: vec![coordinator, s1, s2],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let mut app = app_from_snap(snap);
        let mut ov = DashboardOverlay::menu("cos-coordinator");
        ov.mode = OverlayMode::ConfirmCancel;
        app.dashboard_overlay = Some(ov);

        let out = render_to_text(&app, 120, 30, render_dashboard);
        assert!(out.contains("at least 2 spawned agents"),
            "the count must be labelled 'at least' — the client walk is a floor, the server's count \
             can be higher (no universal-tier parentage, up-to-a-poll-stale snapshot): {out}");
        assert!(out.contains("scout-1") && out.contains("scout-2"),
            "and the actual ids, so the operator can recognise what they are about to stop: {out}");
    }

    #[test]
    fn overlay_budget_frame_shows_the_field_and_the_zero_meaning() {
        use crate::watch::overlay::OverlayMode;
        let out = overlay_frame(
            OverlayMode::Budget { input: tui_input::Input::new("200000".into()), error: None },
            47_000, 120, 30,
        );
        assert!(out.contains("200000"), "the prefilled limit must be visible: {out}");
        assert!(out.contains("0 = unlimited"), "M2's inversion must be stated at the field: {out}");
    }

    #[test]
    fn overlay_in_flight_frame_says_what_is_happening() {
        use crate::watch::overlay::OverlayMode;
        let out = overlay_frame(
            OverlayMode::InFlight { label: "cancelling scout-2…".into() }, 47_000, 120, 30,
        );
        assert!(out.contains("cancelling scout-2"), "{out}");
        assert!(out.contains("waiting for agentd"), "{out}");
    }

    /// A long result string must WRAP, not vanish off the right edge — the failure copy carries the
    /// actionable half at the end ("…then restart agentctl watch").
    #[test]
    fn overlay_result_frame_wraps_a_long_failure() {
        use crate::watch::overlay::OverlayMode;
        let long = "Action refused: approval token missing or wrong (HTTP 401/403). Export the same \
                    AGENTOS_APPROVAL_SECRET used by agentd, then restart agentctl watch.";
        let out = overlay_frame(
            OverlayMode::Result { text: long.to_string(), ok: false }, 47_000, 120, 30,
        );
        assert!(out.contains("Action refused"), "{out}");
        // The tail is asserted word-wise, because WHERE the wrap falls is layout, not contract — what
        // matters is that the actionable end of the sentence is on screen at all.
        assert!(out.contains("AGENTOS_APPROVAL_SECRET") && out.contains("watch."),
            "the actionable tail must survive wrapping, not fall off the box: {out}");
    }

    /// M8 on the actual frame, at every width this branch claims to support and with the LONGEST real
    /// status. Parameterised because the single 120x24 + "running" case was exactly the one combination
    /// that fit: ratatui gives the Status cell 24 cols at the 115 rail floor and 21 at 80, so appending
    /// the marker there rendered `awaiting_approval · NOT CANCELLED` with no cancel signal at all
    /// (the fix-review pass). The marker now lives on its own line in the widest column.
    #[test]
    fn the_cancel_marker_is_visible_at_every_supported_width_and_status() {
        for (w, h) in [(80u16, 24u16), (100, 24), (115, 24), (120, 20), (140, 40)] {
            for status in ["running", "awaiting_approval"] {
                let snap = Snapshot {
                    agents: vec![make_agent("scout-2", status, 12_000, vec![])],
                    budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
                };
                let mut app = app_from_snap(snap);
                app.term_size = (w, h);

                let before = render_to_text(&app, w, h, render_dashboard);
                assert!(before.contains(&status[..7]), "precondition at {w}x{h}: {before}");

                app.mark_cancel_requested("scout-2", true);
                let during = render_to_text(&app, w, h, render_dashboard);
                assert!(during.contains("cancelling…"),
                    "the in-flight marker must be WHOLE at {w}x{h} with status {status}:\n{during}");
                // The row's real state stays visible — the marker annotates, it never substitutes
                // (/review's red team).
                assert!(during.contains(&status[..7]),
                    "the status must survive alongside the marker at {w}x{h}:\n{during}");

                // Aged out with no confirmation: escalation must be legible at the same widths.
                let stale = std::time::Instant::now()
                    .checked_sub(crate::watch::app::CANCEL_CONFIRM_GRACE + std::time::Duration::from_secs(1))
                    .expect("monotonic clock far enough from its origin");
                app.pending_cancel.insert("scout-2".to_string(),
                    crate::watch::app::CancelRequest { asked_at: stale, confirmed: false, landed: false });
                let after = render_to_text(&app, w, h, render_dashboard);
                assert!(after.contains("NOT CANCELLED"),
                    "a lost cancel must be visible at {w}x{h} with status {status}:\n{after}");
            }
        }
    }

    /// The clip discipline: on a short frame the body loses rows before the hints line does, because
    /// the hints line is where the dismissal key is written.
    #[test]
    fn overlay_keeps_its_exit_key_visible_on_a_short_terminal() {
        use crate::watch::overlay::OverlayMode;
        let out = overlay_frame(OverlayMode::Menu, 47_000, 100, 13);
        assert!(out.contains("Esc/q dismiss"),
            "a modal whose exit key got clipped off is a trapped operator: {out}");
    }

    /// The general guard for the defect the first real pty frame exposed: a hand-wrapped sentence that
    /// was 73 chars wide inside a 70-col box, so `confirm cancel` read "…stop at the n". Every mode's
    /// body is checked against every plausible box width — a per-mode assertion on one width would go
    /// stale the moment someone adds a sentence.
    #[test]
    fn no_overlay_mode_emits_a_line_wider_than_its_box() {
        use crate::watch::overlay::{DashboardOverlay, OverlayMode};
        let mut a = make_agent("scout-2", "running", 12_000, vec![]);
        a.windowed_spent = 0; // zero spend ⇒ the long blocked-Park reason is in play
        a.budget = BudgetKind::Tokens(200_000);

        let modes = || vec![
            OverlayMode::Menu,
            OverlayMode::ConfirmCancel,
            OverlayMode::Budget { input: tui_input::Input::new("200000".into()), error: None },
            OverlayMode::Budget {
                input: tui_input::Input::new("20o000".into()),
                error: Some("'20o000' is not a token count. Enter digits only (0 = unlimited).".into()),
            },
            OverlayMode::ConfirmBudget { limit: 0 },
            OverlayMode::ConfirmBudget { limit: 900_000 },
            OverlayMode::InFlight { label: "cancelling scout-2…".into() },
            OverlayMode::Result {
                text: "Cancel requested for scout-2 — takes effect at its next step boundary — \
                       agentctl cancel scout-2".into(),
                ok: true,
            },
        ];

        for width in [34u16, 40, 60, 80, 100, 120, 200] {
            let frame = Rect { x: 0, y: 0, width, height: 40 };
            let text_width = crate::watch::overlay::overlay_text_width(frame);
            for mode in modes() {
                let mut ov = DashboardOverlay::menu("scout-2");
                let kind = mode.kind();
                ov.mode = mode;
                let ctx = OverlayCtx {
                    budget_resettable: false, logs_available: true, cli_conn: " --url http://h:7999",
                };
                for line in overlay_mode_body(&ov, Some(&a), &TopologyGraph::default(), ctx, text_width) {
                    assert!(
                        line.width() <= text_width,
                        "{kind} at frame width {width}: line is {} cols in a {text_width}-col box: {:?}",
                        line.width(), line,
                    );
                }
            }
        }
    }

    // ── V3: the footer clip, and the `?` that replaces what the footer had to give up ──

    /// The acceptance criterion is a WIDTH. `contains("q quit")` passes with the clip bug fully intact:
    /// before this, the narrow footer ran to 162 columns, so `q quit` started at column 114 and the
    /// `(resize to 115+ cols…)` hint at 122 — and since that branch only renders BELOW 115 columns, the
    /// hint about being too narrow was itself always off-screen.
    /// Each state is bounded by the width it is actually DRAWN at — the fix /review's testing specialist
    /// forced. Bounding every state by 114 was vacuous for `NoRail`, which only renders BELOW the rail
    /// floor: the shipped narrow line was 113 cols with `q quit` at column 87, so on an 80-column
    /// terminal the very defect V3 exists to fix was still shipping, with this test green.
    #[test]
    fn every_footer_state_fits_the_terminal_it_is_drawn_in() {
        for logs in [false, true] {
            for state in [FooterState::Table, FooterState::RailFocused] {
                let line = dashboard_hints(state, logs);
                let cols = line.chars().count();
                assert!(cols <= MAX_FOOTER_COLS,
                    "footer is {cols} cols (max {MAX_FOOTER_COLS}) with logs={logs}: {line}");
            }
            // NoRail renders only below MIN_TOTAL_WIDTH_FOR_RAIL, so 114 was never its bound.
            let narrow = dashboard_hints(FooterState::NoRail, logs);
            let cols = narrow.chars().count();
            assert!(cols <= MAX_NARROW_FOOTER_COLS,
                "narrow footer is {cols} cols but only renders below {MIN_TOTAL_WIDTH_FOR_RAIL}: {narrow}");
        }
    }

    /// And the assertion no string length can fake: drive a real 80x24 frame and read the bottom rows.
    /// `contains("q quit")` on the STRING passes with the clip bug intact; `contains` on the rendered
    /// BUFFER cannot, because ratatui truncates at the terminal edge.
    #[test]
    fn the_narrow_footer_is_actually_on_screen_at_eighty_columns() {
        let snap = Snapshot {
            agents: vec![make_agent("scout-2", "running", 1_000, vec![])],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let out = render_to_text(&app_from_snap(snap), 80, 24, render_dashboard);
        assert!(out.contains("q quit"), "the quit key must be ON SCREEN at 80 cols: {out}");
        assert!(out.contains("115+ cols"),
            "so must the hint that explains the missing rail — the branch that renders it IS the \
             too-narrow case: {out}");
        assert!(out.contains("x act") && out.contains("? keys"),
            "and the two keys that reach the verbs and the key map: {out}");
    }

    /// The docker gate the ux.10-A build established: one flag gates the key AND its hint, so the footer
    /// can never advertise a key that does nothing.
    #[test]
    fn the_logs_hint_appears_only_when_a_compose_project_was_detected() {
        assert!(!dashboard_hints(FooterState::Table, false).contains("[l]"),
            "no compose project ⇒ no advertised key");
        assert!(dashboard_hints(FooterState::Table, true).contains("[l]"));
    }

    /// The rail keys are meaningless at a width where the rail cannot appear, and the narrow footer has
    /// to spend those columns telling the operator how to get it back.
    #[test]
    fn the_narrow_footer_drops_what_it_cannot_afford_and_states_the_width_it_needs() {
        let narrow = dashboard_hints(FooterState::NoRail, true);
        assert!(!narrow.contains("Tab chat"), "the rail is not reachable at this width: {narrow}");
        assert!(!narrow.contains("r target"), "{narrow}");
        assert!(!narrow.contains("[s]ys"), "the per-view letters live behind `?` at this width: {narrow}");
        assert!(!narrow.contains("[l]og"), "{narrow}");
        assert!(narrow.contains("? keys"), "…which means `?` itself must survive: {narrow}");
        assert!(narrow.contains("x act"), "and the verb key: {narrow}");
        assert!(narrow.contains("115+ cols"), "must say what it needs: {narrow}");
        let wide = dashboard_hints(FooterState::Table, true);
        assert!(wide.contains("Tab chat") && wide.contains("r target") && wide.contains("[s]ys"), "{wide}");
    }

    /// `?` and the footer must render from ONE table. A hand-written help screen is a second copy of the
    /// key list, and the copy that drifts is the one the operator reads when they are already lost.
    #[test]
    fn the_help_overlay_covers_every_footer_key_and_then_some() {
        use crate::watch::overlay::{DashboardOverlay, OverlayMode};
        let help = help_lines(true);
        for hint in DASHBOARD_KEYS.iter().filter(|k| k.footer) {
            assert!(help.iter().any(|(k, _)| *k == hint.key)
                    || help.iter().any(|(k, _)| k.starts_with(hint.key)),
                "footer key '{}' is missing from ?", hint.key);
        }
        // And it documents keys the footer has no room for — the reason `?` exists at all.
        assert!(help.iter().any(|(k, _)| *k == "Ctrl-c"));
        // /autoplan retroactive review: `[J]` (Jobs view) is real and shipped but was missing
        // from this table entirely, so it was undiscoverable via `?` on top of having no
        // footer room — the exact drift this table exists to prevent.
        assert!(help.iter().any(|(k, _)| *k == "J"), "the Jobs view key must be documented in ?: {help:?}");

        let mut app = App::new(PathBuf::from("/agents"));
        app.apply_snapshot(Snapshot {
            agents: vec![make_agent("scout-2", "running", 1, vec![])],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        });
        let mut ov = DashboardOverlay::help();
        ov.mode = OverlayMode::Help;
        app.dashboard_overlay = Some(ov);
        let out = render_to_text(&app, 120, 30, render_dashboard);
        assert!(out.contains("keys"), "the box must be titled: {out}");
        assert!(out.contains("row actions on the selected agent"), "{out}");
        assert!(out.contains("Esc/q/Enter/? close"), "and state how to leave: {out}");
        assert!(!out.contains("scout-2 is no longer present"),
            "help is not row-scoped — it must not render the vanished-target notice: {out}");
    }

    /// /review's red team: the degraded path predates the `?` mode and fell through to the row wording,
    /// so `?` on a small terminal answered " is no longer present" — about no agent at all, from the one
    /// mode whose entire purpose is teaching the key map.
    #[test]
    fn the_help_overlay_degrades_to_its_own_wording_not_a_vanished_agent_notice() {
        use crate::watch::overlay::DashboardOverlay;
        let snap = Snapshot {
            agents: vec![make_agent("scout-2", "running", 1_000, vec![])],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let mut app = app_from_snap(snap);
        app.term_size = (40, 8); // below the box floor
        app.dashboard_overlay = Some(DashboardOverlay::help());
        let out = render_to_text(&app, 40, 8, render_dashboard);
        assert!(out.contains("key map needs a bigger terminal"), "{out}");
        assert!(!out.contains("no longer present"),
            "help is not about a row, so it must never say a row vanished: {out}");
    }

    /// RT2: the renderer and the key handler must reach the SAME box-vs-line answer. Deriving one from
    /// `area` and the other from `term_size` left a height where the renderer drew a full menu that the
    /// handler refused to act on — a live-looking, completely dead dialog.
    #[test]
    fn the_renderer_and_the_key_handler_agree_on_box_versus_line() {
        use crate::watch::overlay::DashboardOverlay;
        for (w, h) in [(40u16, 8u16), (80, 11), (80, 12), (100, 13), (120, 30), (34, 24)] {
            let snap = Snapshot {
                agents: vec![make_agent("scout-2", "running", 1_000, vec![])],
                budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
            };
            let mut app = app_from_snap(snap);
            app.term_size = (w, h);
            app.dashboard_overlay = Some(DashboardOverlay::menu("scout-2"));
            let out = render_to_text(&app, w, h, render_dashboard);

            // The BOX is identified by its border title, not by the words "row actions" — the degraded
            // line contains those too ("row actions need a bigger terminal"), which made the first
            // version of this probe ambiguous.
            let drew_box = out.contains("┌ row actions");
            let handler_acts = overlay_fits_dashboard((w, h));
            assert_eq!(drew_box, handler_acts,
                "at {w}x{h} the renderer drew {} while the handler would {} — a dead menu with no \
                 explanation on screen is the failure this asserts against:\n{out}",
                if drew_box { "a box" } else { "the degraded line" },
                if handler_acts { "act" } else { "refuse" },
            );
        }
    }

    #[test]
    fn wrap_plain_breaks_on_words_and_never_loses_text() {
        let text = "one two three four five six seven";
        let lines = wrap_plain(text, 10);
        assert!(lines.iter().all(|l| l.chars().count() <= 10), "{lines:?}");
        assert_eq!(lines.join(" "), text, "wrapping must not drop or duplicate words");
        // A word longer than the width still gets emitted rather than dropped (it will be clipped by
        // the renderer, not silently swallowed here).
        assert_eq!(wrap_plain("aaaaaaaaaaaaaaa", 5), vec!["aaaaaaaaaaaaaaa"]);
        assert_eq!(wrap_plain("", 10), vec![""]);
    }

    /// And with a target that has vanished — the branch that must not fall back to row 0.
    #[test]
    fn dashboard_overlay_renders_the_vanished_target_branch() {
        let snap = Snapshot {
            agents: vec![make_agent("cos-coordinator", "running", 1_000, vec![])],
            budget: None, queue: None, sandbox: None, provider: None, isolation: None, credentials: None, jobs: vec![], error: None,
        };
        let mut app = app_from_snap(snap);
        app.dashboard_overlay = Some(crate::watch::overlay::DashboardOverlay::menu("scout-2"));

        let out = render_to_text(&app, 100, 24, render_dashboard);
        assert!(out.contains("No action sent"),
            "the first thing the operator needs is that nothing was written: {out}");
        assert!(out.contains("no longer in the snapshot"), "must say the pinned target is gone: {out}");
        assert!(out.contains("select another running agent"), "and what to do instead: {out}");
    }
}
