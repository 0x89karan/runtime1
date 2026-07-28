//! ux.13-TUI — the Dashboard row-action overlay: state, geometry, target resolution, the menu model,
//! and the verb payloads the event loop performs.
//!
//! Four groups live here. The first three are each the answer to a specific /autoplan finding; the
//! fourth is the model the renderer and the key handler must share:
//!
//! 1. **`DashboardOverlay`** — Dashboard-OWNED state, not a global `App.overlay`. A global would
//!    force every key/paste/resize/render path to ask "is there an overlay?", which is exactly how
//!    the `q`-quits bug happened (the Dashboard had a sub-mode `step_key` did not model). Eng
//!    finding E-arch.
//! 2. **`overlay_rect`** — geometry that is CLAMPED to the frame. `Clear` is the only widget in the
//!    tree that panics on an out-of-frame `Rect`: ratatui's `clear.rs` indexes `buf[(x,y)]` with no
//!    `intersection(buf.area)`, while `Block::render_ref` does intersect — which is why every
//!    existing view survives sloppy geometry and this one would not. A render panic exits the
//!    cockpit mid-incident with the runaway still running. Eng finding E3.
//! 3. **`target`** — resolve-at-use against the PINNED id, never the live selection.
//!    `App::apply_snapshot` clears a vanished selection and then auto-selects row 0, from a producer
//!    thread, independent of keystrokes. So an overlay that read `selected_agent()` could retarget
//!    between the operator's keypress and their confirm — and since Cancel cascades, that means
//!    killing a coordinator's whole subtree by accident. Design finding C1; the resolve-at-use shape
//!    (rather than a `Vanished` state) is Eng finding E5, copying the Approvals `found_id` idiom.
//! 4. **The menu model and the verbs** — [`PendingVerb`] (including `Chat`, which is not an overlay
//!    verb but shares the loop's execution slot), [`MenuItem`]/[`MenuAction`]/[`menu_items`],
//!    [`park_limit`] + [`PARK_FLOOR_TOKENS`], and the budget-field helpers. `menu_items` is the single
//!    list the render, the cursor bounds, and Enter's dispatch all read: three copies of "which item is
//!    where" is how a menu acts on a different row than it highlights.

use ratatui::layout::Rect;

use super::reader::{AgentInfo, BudgetKind};

/// Which row action the operator is performing. `Menu` is the landing state: one key opens a graded
/// list, so the irreversible option is the one you travel to (the actual lazygit/k9s/htop idiom —
/// `x` there opens a MENU; k9s uses `Ctrl-D`/`Ctrl-K`; htop's `k` is a signal picker).
/// No `PartialEq`/`Copy`: `Budget` holds a `tui_input::Input`, which implements neither, so tests
/// match on [`OverlayMode::kind`] rather than comparing values (flagged at /autoplan's Eng phase
/// before it could become a pile of `assert_eq!`s that don't compile).
#[derive(Debug, Clone)]
pub enum OverlayMode {
    /// Row-action menu. The highlighted row lives on [`DashboardOverlay::cursor`], not here, so it
    /// survives a round trip through a sub-mode: `Esc` out of the budget field returns the operator to
    /// the item they opened, and there is exactly one source of truth for "which row".
    Menu,
    /// Second gate for the one irreversible verb. Cancel cascades to the whole spawned subtree, so
    /// it is never armed straight off the menu.
    ConfirmCancel,
    /// Numeric budget entry, prefilled with the agent's current limit.
    Budget { input: tui_input::Input, error: Option<String> },
    /// Second gate for a budget that REMOVES (`0` = unlimited) or RAISES the cap — design finding M2:
    /// a cleared field plus `0` is the inverse of the operator's intent, so it cannot be one keypress.
    ConfirmBudget { limit: u64 },
    /// The frame drawn BEFORE the blocking call. The verb runs on the loop thread from
    /// `drain_pending_verb`, never from the key handler, so this state is what makes "in flight"
    /// representable at all (eng finding H1): `HttpSource`'s confirm client blocks up to 3 s, and a
    /// call made inside the key handler would freeze the cockpit with no frame ever drawn.
    InFlight { label: String },
    /// Outcome, held until explicitly dismissed. Deliberately NOT `spawn_banner`, which any keypress
    /// clears (design finding M3) — an operator who taps a key while reading loses the only report of
    /// what a destructive verb did.
    Result { text: String, ok: bool },
    /// `?` — the key map. Not row-scoped (`target_id` is empty), but it lives in the same overlay so it
    /// inherits the clamped geometry AND the keyboard-ownership rule for free; a help screen that let
    /// `s` switch views underneath it would be the same bug class as the row menu's.
    ///
    /// This is the honest counterpart to striking the `:` command palette: the CEO phase's argument was
    /// that ~11 compile-time keys belong on screen, not behind a prompt — which obliges the cockpit to
    /// actually HAVE the `?` that lazygit/htop/btop answer this shape with. None was bound before.
    Help,
}

/// A row action in progress, pinned to the agent it was opened against.
#[derive(Debug, Clone)]
pub struct DashboardOverlay {
    /// The agent this overlay acts on, captured at open time and never re-read from the selection.
    pub target_id: String,
    pub mode:      OverlayMode,
    /// Highlighted [`menu_items`] row.
    pub cursor:    usize,
}

impl OverlayMode {
    /// Stable discriminant for assertions, since the mode itself is not comparable (`Budget` holds a
    /// `tui_input::Input`, which is neither `PartialEq` nor `Copy`).
    #[cfg(test)]
    pub fn kind(&self) -> &'static str {
        match self {
            OverlayMode::Menu                 => "menu",
            OverlayMode::ConfirmCancel        => "confirm_cancel",
            OverlayMode::Budget { .. }        => "budget",
            OverlayMode::ConfirmBudget { .. } => "confirm_budget",
            OverlayMode::InFlight { .. }      => "in_flight",
            OverlayMode::Result { .. }        => "result",
            OverlayMode::Help                 => "help",
        }
    }
}

/// The work a confirmed verb hands to the loop. Deliberately owned data, not a borrow: it survives
/// the key handler returning, and the loop performs it on the next iteration.
///
/// There is no new `Effect` variant for this. `Effect` is `Copy` and payload-free and `apply_effects`
/// has no `source` parameter, so an effect *cannot* carry or perform a verb (eng finding E8). This is
/// the `spawn_view.pending_exec` precedent: the key handler stores work, the loop performs it.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingVerb {
    Cancel { agent_id: String },
    /// Both the typed budget and Park — Park is a TUI-only ALIAS over `set_budget`, which is why the
    /// overlay names its CLI equivalent rather than inventing a second mental model (DX finding).
    SetBudget { agent_id: String, limit: u64, park: bool },
    /// A chat-rail turn (`converse::dispatch` — inject if the target is waiting, else spawn).
    ///
    /// Not a row verb, and it draws no overlay: the rail's own `Dispatching…` phase is its in-flight
    /// frame. It is here because it has the SAME problem this slot exists to solve, and worse —
    /// `dispatch` does a `load_snapshot` (5 s client) plus a spawn (3 s), so the cockpit's
    /// highest-frequency interaction froze the whole loop for up to ~8 s with Ctrl-C inert
    /// (`TODOS.md`'s ranked P2). Leaving it inline would also have left two contradictory I/O idioms
    /// in one function, and the next contributor copies whichever arm they read first.
    Chat { target: String, text: String },
}

impl PendingVerb {
    /// The `agentctl` invocation that does the same thing. Printed in the overlay so the operator
    /// learns the fallback path for when the TUI is the thing that is broken, and so incident notes
    /// are copy-pasteable (the single best idea from the DX phase).
    /// `conn` is [`crate::watch::source::DataSource::cli_connection_flags`] — without it the printed
    /// command re-resolves its own data source and can reach a different daemon than the frame it was
    /// printed on (/review).
    pub fn equivalent_cli(&self, conn: &str) -> String {
        match self {
            PendingVerb::Cancel { agent_id } => {
                format!("agentctl cancel {}{conn}", shell_arg(agent_id))
            }
            // Positional args, matching `verbs.rs`'s actual clap shape (`set-budget <agent_id> <limit>`).
            // A printed command the operator cannot paste is worse than none: it teaches a wrong CLI.
            PendingVerb::SetBudget { agent_id, limit, .. } => {
                format!("agentctl set-budget {} {limit}{conn}", shell_arg(agent_id))
            }
            // `inject` is the equivalent only for an already-waiting agent; a first turn spawns. The
            // rail does not print this today — it exists so the mapping stays written down in one place.
            PendingVerb::Chat { target, .. } => {
                format!("agentctl inject {} '<text>'{conn}", shell_arg(target))
            }
        }
    }

    /// Present-tense label for the in-flight frame. Present tense on purpose: nothing has been
    /// confirmed by the scheduler yet at this point.
    pub fn in_flight_label(&self) -> String {
        match self {
            PendingVerb::Cancel { agent_id } => format!("cancelling {agent_id}…"),
            PendingVerb::SetBudget { agent_id, park: true, .. } => format!("parking {agent_id}…"),
            PendingVerb::SetBudget { agent_id, .. } => format!("setting budget on {agent_id}…"),
            PendingVerb::Chat { target, .. } => format!("sending to {target}…"),
        }
    }
}

/// Quote an agent id for the copy-pasteable command line.
///
/// An id containing a space pastes as TWO positional arguments and the command fails — the printed line
/// has to be runnable, not merely parseable (Codex's adversarial pass). Single quotes with the standard
/// `'\''` escape, applied only when needed so the common case stays clean. Ids that would break the
/// request URL are refused earlier, at the `DataSource` boundary.
pub fn shell_arg(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@'));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The limit that parks `windowed_spent` — `None` when parking is unsafe.
///
/// **`0` means UNLIMITED**, everywhere: `scheduler.rs`'s admission gate, `agent/mod.rs`'s per-turn
/// check, and the `DataSource::set_budget` contract. And `set_token_budget` writes the CHECKPOINTED
/// `cfg.token_budget`. So `Park = set_budget(windowed_spent)` on an agent with no recorded spend —
/// *exactly* the agent an operator reaches for a stop button on — would REMOVE its budget cap
/// permanently, across restart, in Park's primary use case. Eng finding E1, the increment's worst
/// footgun and invisible in the UI it would have shipped behind.
///
/// The floor is not just `0`: the current turn's tokens land before the admission gate re-runs, so a
/// limit a few hundred tokens above the recorded spend parks nothing and merely lowers the cap.
pub const PARK_FLOOR_TOKENS: u64 = 1_000;

pub fn park_limit(windowed_spent: u64) -> Option<u64> {
    (windowed_spent >= PARK_FLOOR_TOKENS).then_some(windowed_spent)
}

/// Would parking at `limit` WIDEN the agent's current cap instead of tightening it?
///
/// /review's red team, and the sharpest finding in this increment: `windowed_spent > token_budget` is
/// the NORMAL state of an exhausted agent, because the admission gate is checked BEFORE the turn
/// (`scheduler.rs`'s `b != 0 && a.windowed_spent() >= b`) — an agent admitted at 99 999/100 000 whose
/// turn costs 20 k ends at 119 999/100 000, which the Dashboard renders as `119k/100k`. Park there would
/// have called `set_budget(119_999)`: a 20% RAISE of the operator's configured ceiling, written to the
/// CHECKPOINTED `cfg.token_budget`, from the one verb this whole increment frames as a soft stop — and
/// in one keypress, while the TYPED budget path refuses to widen anything without a second gate (M2).
///
/// It is also pointless in that state: at or past its ceiling the agent is already parked or terminal.
/// So Park is blocked rather than clamped, with copy that says to use Cancel.
pub fn park_would_widen(limit: u64, current: &BudgetKind) -> bool {
    match current {
        // Unlimited (0) is not a ceiling to widen — any positive limit is strictly tighter.
        BudgetKind::Unlimited => false,
        BudgetKind::Tokens(cap) => limit > *cap,
    }
}

/// One row-action menu entry. Disabled items still RENDER, with the reason — an item that vanishes
/// teaches the operator nothing, and Park's unavailability is exactly the thing they need to know
/// before reaching for Cancel.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub label:   String,
    pub detail:  String,
    pub action:  MenuAction,
    /// `Some(reason)` ⇒ the item is disabled and Enter is a no-op. The reason renders under the
    /// highlighted row, so a blocked Enter needs no error state of its own.
    pub blocked: Option<String>,
}

/// What Enter on a menu row does. Descriptive, not executable: the handler builds the [`PendingVerb`]
/// so verb construction stays in one place, and `Cancel`/`SetBudget` route through their own gate
/// rather than arming from the menu.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuAction {
    /// Reversible and one call, so it arms straight from the menu — the whole reason it is ranked
    /// first (design finding H4).
    Park { limit: u64 },
    /// Opens the numeric field.
    SetBudget,
    /// Opens the second gate.
    Cancel,
}

impl MenuItem {
    pub fn enabled(&self) -> bool {
        self.blocked.is_none()
    }
}

/// The graded menu for `target`: reversible first, irreversible last.
///
/// Pure so the render, the cursor bounds, and Enter's dispatch all read the SAME list — three copies
/// of "which item is where" is how a menu ends up acting on a different row than it highlights.
///
/// `budget_resettable` comes from the connected agentd (`[scheduler] budget_reset_interval > 0`) and
/// decides what Park MEANS. Both answers are worse than "a reversible pause", which is why the label
/// carries the difference:
///
/// - **Window configured** (the shipped CoS config, 86400 s): exhaustion DEFERS, and then
///   `maybe_roll_budget_window` calls `reset_budget_window()` on **every** task at the next rollover,
///   rebasing windowed spend to 0, after which `drain_deferred` re-admits the agent
///   (`scheduler.rs`). So the park **expires by itself** — the operator who parks a runaway at 18:00
///   has not held it until they act, they have paused it for up to one window.
/// - **No window** (`budget_reset_interval = 0`, the config DEFAULT — only the CoS configs set one):
///   exhaustion calls `handle_agent_terminal` — a kill. Park IS Cancel by another mechanism.
///
/// Eng finding E2 named half of this; /review's security + testing specialists independently caught
/// the other half, that the first case auto-revives. Saying "raise the limit to revive" implied a hold
/// the runtime does not provide, in the deployment where Park is most likely to be used.
pub fn menu_items(target: &AgentInfo, budget_resettable: bool) -> Vec<MenuItem> {
    let mut items = Vec::new();

    match park_limit(target.windowed_spent).filter(|limit| !park_would_widen(*limit, &target.budget)) {
        Some(limit) => items.push(MenuItem {
            label:   if budget_resettable { "Park (until rollover)".to_string() }
                     else { "Park (ends it)".to_string() },
            detail:  if budget_resettable {
                format!("cap at the {limit} tokens spent — resumes by itself at the next window rollover")
            } else {
                format!("cap at the {limit} tokens spent — no reset window, so this ENDS the agent")
            },
            action:  MenuAction::Park { limit },
            blocked: None,
        }),
        None => {
            // Two ways Park can be unavailable, and they need different copy. Disabled items still
            // RENDER with the reason: an item that vanishes teaches nothing, and this is exactly what
            // the operator needs to know before reaching for Cancel.
            let reason = match park_limit(target.windowed_spent) {
                // Would widen the cap — see `park_would_widen`. Also means the agent is already at or
                // past its ceiling, i.e. already parked or terminal, so Park has nothing to do.
                Some(limit) => format!(
                    "Park unavailable: {} has already spent {} of its {} cap, so capping at the spend \
                     would RAISE the limit (and the change is checkpointed). It is already at its \
                     ceiling — use Cancel to stop it.",
                    target.id, limit, target.budget.display(),
                ),
                // The DX phase's replacement copy, verbatim: what, why, what to do.
                None => format!(
                    "Park unavailable: {} has {} recorded window spend, and budget 0 means unlimited. \
                     Use Cancel or set a positive budget.",
                    target.id, target.windowed_spent,
                ),
            };
            items.push(MenuItem {
                label:   "Park".to_string(),
                detail:  "unavailable at this spend".to_string(),
                action:  MenuAction::Park { limit: 0 }, // never reachable: `blocked` makes Enter a no-op
                blocked: Some(reason),
            });
        }
    }

    items.push(MenuItem {
        label:   "Set budget".to_string(),
        detail:  format!("current: {}", target.budget.display()),
        action:  MenuAction::SetBudget,
        blocked: None,
    });

    items.push(MenuItem {
        label:   "Cancel".to_string(),
        detail:  "stop the agent and its spawned subtree".to_string(),
        action:  MenuAction::Cancel,
        blocked: None,
    });

    items
}

/// The value the budget field opens with. Prefilled with the CURRENT limit rather than empty:
/// design finding M2 — an empty field plus `0` submits "unlimited", the exact inverse of what an
/// operator reaching for a budget dialog wants.
pub fn budget_prefill(budget: &BudgetKind) -> String {
    match budget {
        BudgetKind::Tokens(n) => n.to_string(),
        BudgetKind::Unlimited => String::new(),
    }
}

/// Does this budget change need the second gate? `0` REMOVES the cap and a raise WIDENS it; both are
/// the opposite of the reason the dialog was opened, so neither is one keypress away (M2).
pub fn budget_needs_second_gate(new_limit: u64, current: &BudgetKind) -> bool {
    if new_limit == 0 {
        return true;
    }
    match current {
        BudgetKind::Unlimited => false,           // any positive limit is strictly tighter
        BudgetKind::Tokens(n) => new_limit > *n,  // a raise
    }
}

impl DashboardOverlay {
    /// Open the row-action menu for `target_id`.
    pub fn menu(target_id: impl Into<String>) -> Self {
        Self { target_id: target_id.into(), mode: OverlayMode::Menu, cursor: 0 }
    }

    /// Open the `?` key map. No target: it is about the cockpit, not a row.
    pub fn help() -> Self {
        Self { target_id: String::new(), mode: OverlayMode::Help, cursor: 0 }
    }

    /// Resolve the pinned target against the CURRENT agent list. `None` means the agent is gone —
    /// the caller renders "no longer present" and makes the verb a no-op, mirroring the Approvals
    /// view's "Approval already resolved" branch rather than inventing a new state.
    pub fn target<'a>(&self, agents: &'a [AgentInfo]) -> Option<&'a AgentInfo> {
        agents.iter().find(|a| a.id == self.target_id)
    }
}

/// Minimum frame a centred overlay can be drawn in. Below this the caller degrades to a
/// footer-style single-line prompt instead of a box whose borders would eat every content row.
/// Same "pure fits predicate" idiom as `views::converse_rail_fits` / `MIN_TOPOLOGY_WIDTH`.
pub const MIN_OVERLAY_WIDTH: u16 = 34;
pub const MIN_OVERLAY_HEIGHT: u16 = 7;

pub fn overlay_fits(width: u16, height: u16) -> bool {
    width >= MIN_OVERLAY_WIDTH && height >= MIN_OVERLAY_HEIGHT
}

/// Clamp any rect into `frame`. This is the guard that keeps `Clear` from indexing outside the
/// buffer and panicking the render thread.
///
/// Kept as its own function with its own test on purpose. `overlay_rect`'s arithmetic below is
/// already escape-proof, which means a test that only drives `overlay_rect` passes whether or not
/// this clamp is applied — a genuinely vacuous test, caught by running the negative control instead
/// of assuming it. So the backstop is tested directly, and it stays a backstop against a future edit
/// to that arithmetic.
pub fn clamp_to_frame(r: Rect, frame: Rect) -> Rect {
    r.intersection(frame)
}

/// Box width for `frame`. Split out from [`overlay_rect`] because the body has to be WRAPPED to this
/// width before its line count can be used as the box height — and a hardcoded wrap width is how the
/// first real pty frame of this overlay ended up reading "…stop at the n" with the rest of the
/// sentence past the border. Width does not depend on the body, so this ordering is sound.
pub fn overlay_width(frame: Rect) -> u16 {
    frame.width.saturating_sub(4).min(72).max(MIN_OVERLAY_WIDTH.min(frame.width))
}

/// Usable text columns inside the box: width minus the two borders.
pub fn overlay_text_width(frame: Rect) -> usize {
    overlay_width(frame).saturating_sub(2) as usize
}

/// Centred overlay rect, clamped to `frame`.
///
/// Width tracks the frame (a fixed 60 would overflow an 80-col terminal once borders and padding are
/// counted) and is capped so the dashboard stays visible around it. Never returns a rect outside
/// `frame`, and may return an EMPTY one on a frame too small to hold anything — callers must check
/// `is_empty()` and fall back to a single-line prompt, because an empty rect is the one remaining
/// input that would still reach `Clear`.
pub fn overlay_rect(frame: Rect, want_height: u16) -> Rect {
    let width  = overlay_width(frame);
    let height = want_height.min(frame.height.saturating_sub(2)).max(1);
    // saturating_add: `frame.x + …` overflows `u16` on a high-origin rect — a debug panic on the RENDER
    // thread, the same class of exit-the-cockpit failure E3 is about (Codex's adversarial pass). The
    // clamp below then pulls a saturated origin back inside the frame.
    let x = frame.x.saturating_add(frame.width.saturating_sub(width) / 2);
    let y = frame.y.saturating_add(frame.height.saturating_sub(height) / 2);
    clamp_to_frame(Rect { x, y, width, height }, frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::reader::BudgetKind;

    fn agent(id: &str) -> AgentInfo {
        AgentInfo {
            id: id.to_string(),
            status: "running".to_string(),
            status_detail: None,
            context_tokens: 0,
            budget: BudgetKind::Unlimited,
            windowed_spent: 0,
            tools: vec![],
            parent_id: None,
            sandbox: None,
            egress_brokered: 0,
            egress_denied: 0,
            tier: "native".to_string(),
            isolation: String::new(),
            pid: 0,
            attention: vec![],
        }
    }

    /// E3: the whole reason this module exists. `Clear` indexes without intersecting, so any rect
    /// that escapes the frame panics the render thread and drops the operator out of the cockpit.
    #[test]
    fn overlay_rect_never_escapes_the_frame() {
        for (w, h) in [(1, 1), (2, 1), (10, 3), (34, 7), (80, 24), (140, 40), (200, 50)] {
            let frame = Rect { x: 0, y: 0, width: w, height: h };
            let r = overlay_rect(frame, 9);
            assert!(
                r.right() <= frame.right() && r.bottom() <= frame.bottom(),
                "overlay escaped the frame at {w}x{h}: {r:?}"
            );
            assert!(r.x >= frame.x && r.y >= frame.y, "negative offset at {w}x{h}: {r:?}");
        }
    }

    /// The backstop, tested directly — feeding it a rect that DOES escape, which `overlay_rect`
    /// itself cannot currently produce. Without this, the clamp is untested insurance: reverting the
    /// `.intersection()` left every `overlay_rect` test green (verified by running the negative
    /// control), because the arithmetic is independently escape-proof.
    #[test]
    fn clamp_to_frame_pulls_an_escaping_rect_back_inside() {
        let frame = Rect { x: 0, y: 0, width: 80, height: 24 };
        let escaping = Rect { x: 70, y: 20, width: 40, height: 20 }; // right=110, bottom=40
        let c = clamp_to_frame(escaping, frame);
        assert!(c.right() <= 80 && c.bottom() <= 24, "clamp failed: {c:?}");
        assert_eq!((c.width, c.height), (10, 4), "keeps the in-frame remainder");
        // Fully outside collapses to empty, which callers must treat as "no box".
        let outside = Rect { x: 200, y: 200, width: 10, height: 10 };
        assert!(clamp_to_frame(outside, frame).is_empty());
    }

    /// A non-zero-origin frame (the overlay is drawn over a content area, not the whole screen).
    #[test]
    fn overlay_rect_respects_a_frame_that_does_not_start_at_origin() {
        let frame = Rect { x: 10, y: 5, width: 60, height: 20 };
        let r = overlay_rect(frame, 9);
        assert!(r.x >= 10 && r.y >= 5);
        assert!(r.right() <= 70 && r.bottom() <= 25, "{r:?}");
    }

    #[test]
    fn overlay_rect_is_centred_and_capped_on_a_wide_frame() {
        let frame = Rect { x: 0, y: 0, width: 200, height: 50 };
        let r = overlay_rect(frame, 9);
        assert_eq!(r.width, 72, "capped so the dashboard stays visible around it");
        assert_eq!(r.height, 9);
        assert_eq!(r.x, (200 - 72) / 2);
    }

    /// The fits predicate is what callers use to choose box-vs-line BEFORE computing geometry.
    #[test]
    fn overlay_fits_floor() {
        assert!(!overlay_fits(33, 24));
        assert!(!overlay_fits(80, 6));
        assert!(overlay_fits(MIN_OVERLAY_WIDTH, MIN_OVERLAY_HEIGHT));
        assert!(overlay_fits(140, 40));
    }

    /// C1/E5: the pinned id is resolved against the CURRENT list, so a snapshot that retargets the
    /// selection cannot move the overlay's target.
    #[test]
    fn target_resolves_the_pinned_id_not_the_selection() {
        let overlay = DashboardOverlay::menu("scout-2");
        let agents = vec![agent("cos-coordinator"), agent("scout-2")];
        assert_eq!(overlay.target(&agents).map(|a| a.id.as_str()), Some("scout-2"));
    }

    // ── E1/E2: the Park footgun, and the menu it is rendered through ─────────────────

    /// The single most important assertion in this increment.
    ///
    /// `park_limit(0) == None` is what stops `Park` from calling `set_budget(0)` — which means
    /// UNLIMITED in the scheduler and is written to the CHECKPOINTED config, so it would un-cap a
    /// runaway permanently, across restart, on exactly the agent an operator reaches for a stop button
    /// on. Asserting `park_limit(5_000) == Some(5_000)` ALONE passes with the footgun intact, which is
    /// why the zero and sub-floor cases are separate rows here.
    #[test]
    fn park_limit_refuses_zero_and_anything_below_the_floor() {
        assert_eq!(park_limit(0), None, "0 would be written as UNLIMITED — never park at 0");
        assert_eq!(park_limit(1), None);
        assert_eq!(park_limit(PARK_FLOOR_TOKENS - 1), None,
            "below the floor the next turn's tokens land before the gate re-runs, so it parks nothing");
        assert_eq!(park_limit(PARK_FLOOR_TOKENS), Some(PARK_FLOOR_TOKENS));
        assert_eq!(park_limit(47_000), Some(47_000));
    }

    #[test]
    fn menu_blocks_park_at_zero_spend_and_says_why() {
        let items = menu_items(&agent("scout-2"), true);
        let park = &items[0];
        assert_eq!(park.label, "Park");
        assert!(!park.enabled(), "Park must be disabled at zero recorded spend");
        let reason = park.blocked.as_deref().unwrap();
        assert!(reason.contains("0 means unlimited"), "must explain WHY, not just refuse: {reason}");
        assert!(reason.contains("Use Cancel or set a positive budget"), "and what to do instead: {reason}");
    }

    /// The Park truth table, both directions — and the reason this test was REWRITTEN during /review.
    ///
    /// The first version asserted only that a deployment with no reset window does not say "revive".
    /// Two /review specialists (security + testing, independently) caught the other half: with a window
    /// configured — the shipped CoS config — `maybe_roll_budget_window` calls `reset_budget_window()` on
    /// EVERY task at the next rollover and `drain_deferred` re-admits, so the park **expires by itself**.
    /// The old copy, "raise the limit to revive", promised a hold the runtime does not provide, in
    /// exactly the deployment where an operator reaches for Park at 2 am.
    #[test]
    fn park_states_which_of_its_two_meanings_applies() {
        let mut a = agent("scout-2");
        a.windowed_spent = 47_000;

        // Window configured ⇒ a pause with a deadline the operator did not choose.
        let with_window = &menu_items(&a, true)[0];
        assert!(with_window.label.contains("until rollover"), "{with_window:?}");
        assert!(with_window.detail.contains("resumes by itself"),
            "must not imply the agent stays parked until the operator acts: {with_window:?}");
        assert!(!with_window.detail.contains("raise the limit to revive"),
            "the old copy promised a hold the runtime does not provide: {with_window:?}");

        // No window ⇒ exhaustion calls handle_agent_terminal. That is a kill, and the label says so.
        let no_window = &menu_items(&a, false)[0];
        assert!(no_window.label.contains("ends it"), "{no_window:?}");
        assert!(no_window.detail.contains("ENDS the agent"), "{no_window:?}");
        assert!(!no_window.detail.contains("resumes"), "{no_window:?}");

        // Offered in both cases: a budget-based stop is legitimate either way; only its meaning differs.
        assert!(with_window.enabled() && no_window.enabled());
    }

    /// /review's red team: the sharpest finding in this increment. `windowed_spent > token_budget` is the
    /// NORMAL state of an exhausted agent (the admission gate is pre-turn, so the last admitted turn
    /// overshoots), and Park there would have RAISED the operator's configured ceiling — checkpointed, in
    /// one keypress, from the verb framed as a soft stop, while the typed path gates every widening.
    #[test]
    fn park_refuses_to_widen_a_cap_it_would_otherwise_raise() {
        assert!(park_would_widen(119_999, &BudgetKind::Tokens(100_000)), "the overshoot case");
        assert!(!park_would_widen(47_000, &BudgetKind::Tokens(100_000)), "a genuine tightening");
        assert!(!park_would_widen(100_000, &BudgetKind::Tokens(100_000)), "equal is not a widening");
        assert!(!park_would_widen(47_000, &BudgetKind::Unlimited),
            "unlimited is not a ceiling — any positive limit is tighter");

        // And the menu refuses, with copy that names the alternative.
        let mut over = agent("scout-2");
        over.windowed_spent = 119_999;
        over.budget = BudgetKind::Tokens(100_000);
        let park = &menu_items(&over, true)[0];
        assert!(!park.enabled(), "Park must not arm a raise: {park:?}");
        let reason = park.blocked.as_deref().unwrap();
        assert!(reason.contains("would RAISE the limit"), "{reason}");
        assert!(reason.contains("use Cancel to stop it"), "must name what to do instead: {reason}");

        // The same agent UNDER its cap is still parkable — the guard must not disable the feature.
        let mut under = agent("scout-2");
        under.windowed_spent = 47_000;
        under.budget = BudgetKind::Tokens(100_000);
        assert!(menu_items(&under, true)[0].enabled());
    }

    /// Order is a safety property, not cosmetics: the reversible verb is the one you land on and the
    /// irreversible one is the one you travel to (design finding H3/H4).
    #[test]
    fn menu_ranks_the_reversible_verb_first_and_cancel_last() {
        let mut a = agent("scout-2");
        a.windowed_spent = 47_000;
        let items = menu_items(&a, true);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels[0].starts_with("Park"), "{labels:?}");
        assert_eq!(&labels[1..], ["Set budget", "Cancel"]);
        assert_eq!(items[0].action, MenuAction::Park { limit: 47_000 });
        assert!(items.iter().all(|i| i.enabled()), "all three are available at real spend");
    }

    /// M2: the field opens on the CURRENT limit. An empty field plus Enter reads as "no cap", which is
    /// the inverse of why the dialog was opened.
    #[test]
    fn budget_prefill_uses_the_current_limit() {
        assert_eq!(budget_prefill(&BudgetKind::Tokens(200_000)), "200000");
        assert_eq!(budget_prefill(&BudgetKind::Unlimited), "",
            "there is no current limit to prefill — and 0 would mean 'confirm unlimited'");
    }

    /// The second gate fires on the two changes that WIDEN authority, and on nothing else — a gate that
    /// fires on every submit trains the operator to press Enter twice by reflex.
    #[test]
    fn second_gate_fires_on_removal_and_raises_only() {
        assert!(budget_needs_second_gate(0, &BudgetKind::Tokens(200_000)), "0 removes the cap");
        assert!(budget_needs_second_gate(0, &BudgetKind::Unlimited), "0 is always gated");
        assert!(budget_needs_second_gate(300_000, &BudgetKind::Tokens(200_000)), "a raise");
        assert!(!budget_needs_second_gate(100_000, &BudgetKind::Tokens(200_000)), "a tightening");
        assert!(!budget_needs_second_gate(200_000, &BudgetKind::Tokens(200_000)), "unchanged");
        assert!(!budget_needs_second_gate(1, &BudgetKind::Unlimited),
            "any positive limit on an uncapped agent is strictly tighter");
    }

    /// Park is a TUI-only alias over `set-budget`, so it must name the command it aliases or it
    /// becomes a hidden second mental model (DX finding).
    #[test]
    fn equivalent_cli_is_a_runnable_command_for_every_verb() {
        assert_eq!(
            PendingVerb::Cancel { agent_id: "scout-2".into() }.equivalent_cli(""),
            "agentctl cancel scout-2"
        );
        assert_eq!(
            PendingVerb::SetBudget { agent_id: "scout-2".into(), limit: 47_000, park: true }
                .equivalent_cli(""),
            "agentctl set-budget scout-2 47000",
            "Park must resolve to the set-budget command it is an alias for"
        );
        // The flags matter: a flagless command re-resolves its own source and can reach a different
        // daemon than the frame that printed it (/review's api-contract finding).
        assert_eq!(
            PendingVerb::Cancel { agent_id: "scout-2".into() }
                .equivalent_cli(" --url http://10.0.0.4:7999"),
            "agentctl cancel scout-2 --url http://10.0.0.4:7999"
        );
        // An id with a space would paste as TWO positional args and fail — the printed line has to be
        // RUNNABLE, not merely parseable (Codex's adversarial pass).
        assert_eq!(
            PendingVerb::Cancel { agent_id: "my agent".into() }.equivalent_cli(""),
            "agentctl cancel 'my agent'"
        );
        assert_eq!(
            PendingVerb::Cancel { agent_id: "it's".into() }.equivalent_cli(""),
            r"agentctl cancel 'it'\''s'",
            "and the quote itself must be escaped, or the command line ends early"
        );
    }

    /// `overlay_rect` is called with a content area, not the whole frame, so its origin is non-zero —
    /// and `frame.x + …` overflows `u16` at a high origin, which is a debug PANIC on the render thread
    /// (Codex's adversarial pass). The clamp then pulls the saturated origin back inside.
    #[test]
    fn overlay_rect_survives_an_absurdly_high_origin() {
        let frame = Rect { x: u16::MAX - 40, y: u16::MAX - 10, width: 40, height: 10 };
        let r = overlay_rect(frame, 9);
        assert!(r.right() <= frame.right() && r.bottom() <= frame.bottom(), "{r:?}");
        assert!(r.x >= frame.x && r.y >= frame.y, "{r:?}");
    }

    /// Present tense, because at that point nothing has been confirmed by the scheduler.
    #[test]
    fn in_flight_labels_are_present_tense() {
        assert_eq!(PendingVerb::Cancel { agent_id: "s".into() }.in_flight_label(), "cancelling s…");
        assert_eq!(
            PendingVerb::SetBudget { agent_id: "s".into(), limit: 5, park: true }.in_flight_label(),
            "parking s…"
        );
    }

    #[test]
    fn target_is_none_once_the_agent_is_gone() {
        let overlay = DashboardOverlay::menu("scout-2");
        // The agent finished; the snapshot now holds only the coordinator (which `apply_snapshot`
        // would have auto-selected — the exact retarget hazard).
        let agents = vec![agent("cos-coordinator")];
        assert!(
            overlay.target(&agents).is_none(),
            "a vanished target must resolve to None, never fall back to row 0"
        );
    }
}
