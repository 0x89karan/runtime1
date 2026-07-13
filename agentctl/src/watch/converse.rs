//! ux.1 — Converse: per-target chat state + shared spawn/resume + terminal-event
//! recognition, shared by `agentctl watch`'s Dashboard chat rail and `agentctl orchestrate`'s
//! CLI REPL. One behavior, two front ends.
//!
//! The four terminal event kinds do NOT share a uniform field-path shape (verified against
//! `agentd/src/scheduler.rs` during Eng review) — this module ports `orchestrate.rs`'s
//! existing per-kind lookups verbatim rather than re-deriving them as "shared general
//! knowledge", per that finding.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use super::source::{DataSource, SpawnRequest};

/// Per-target chat state. Keyed by agent id in `ConverseView::targets` — NOT a single
/// global, so retargeting away from a streaming target does not lose its in-flight reply
/// (Eng Phase 1 architecture finding).
#[derive(Debug, Clone, PartialEq)]
pub enum ConversePhase {
    Idle,
    Dispatching,
    Streaming,
}

/// Max size of `current_reply` before truncation (mirrors agentd's own tool-input
/// accumulator cap pattern, p7.2).
pub const CURRENT_REPLY_CAP_BYTES: usize = 64 * 1024;

/// Max flushed turns retained per target (ring buffer; oldest dropped first).
pub const MAX_HISTORY_TURNS: usize = 200;

/// Client-side dispatch timeout — if no terminal/delta event arrives within this window,
/// the rail surfaces an inline resume hint rather than hanging silently (Eng Phase 1
/// dead-air finding).
pub const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Default `max_turns` for a fresh orchestrated spawn. Shared between the Dashboard rail
/// (`mod.rs`'s Enter handler) and `orchestrate.rs`'s `--max-turns` CLI default so the two
/// front ends can't silently drift apart (caught during /review — they were previously
/// two independent `200` literals).
pub const DEFAULT_MAX_TURNS: u32 = 200;

/// One flushed turn in a target's transcript history.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: TurnRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TurnRole {
    Operator,
    Assistant,
    /// Inline error/resume-hint line (Section 2's rescue table) — rendered distinctly
    /// (yellow, `!` prefix) from a normal assistant reply.
    System,
}

#[derive(Debug, Clone)]
pub struct ConverseState {
    pub phase: ConversePhase,
    /// Turn currently accumulating (STREAMING) or awaiting (DISPATCHING). None when IDLE.
    pub current_reply: String,
    /// Last chunk_seq seen for the in-flight turn; used to detect gaps (dropped chunks).
    pub last_chunk_seq: Option<u64>,
    /// The `turn_seq` of the currently-accumulating reply (set on the first delta after
    /// a dispatch). `chunk_seq` alone resets to 0 every turn (agentd/src/scheduler.rs's
    /// `make_infer_future` scopes it per-call) so it cannot distinguish a stale/late
    /// delta from a PREVIOUS turn from a legitimate one — caught during /review's
    /// adversarial pass as a real cross-turn contamination risk. A delta whose
    /// `turn_seq` doesn't match is rejected, not spliced in.
    pub current_turn_seq: Option<u64>,
    /// Ring buffer of flushed turns, oldest dropped first at MAX_HISTORY_TURNS.
    pub history: VecDeque<Turn>,
    /// Time of the LAST event (dispatch OR delta) for this target. The 30s timeout is
    /// dead-air detection — no activity for 30s — NOT a total-turn-duration cap. (Bug
    /// caught during /review: this used to be set once at dispatch and never refreshed,
    /// so any turn streaming longer than 30s total got killed mid-stream, discarding a
    /// real in-progress reply — routine for verbose or tool-using turns.) `None` when idle.
    pub last_event_at: Option<Instant>,
    /// True while auto-scrolled to the bottom (the default). Streaming/new turns never
    /// yank the view when `false` (acceptance criterion 3) — disarmed by manual scroll-up,
    /// re-armed by `G`/`End`.
    pub follow: bool,
    /// Lines scrolled up from the natural "follow" (bottom) position. 0 while `follow`.
    pub scroll_up_lines: u16,
    /// Count of new turns/deltas that arrived while `!follow` — drives the `▼ N new`
    /// indicator. Reset to 0 when follow is re-armed.
    pub new_since_scroll: usize,
}

impl Default for ConverseState {
    fn default() -> Self {
        Self {
            phase: ConversePhase::Idle,
            current_reply: String::new(),
            last_chunk_seq: None,
            current_turn_seq: None,
            history: VecDeque::new(),
            last_event_at: None,
            follow: true,
            scroll_up_lines: 0,
            new_since_scroll: 0,
        }
    }
}

impl ConverseState {
    /// Push a turn into history, enforcing the `MAX_HISTORY_TURNS` ring cap and the
    /// `▼ N new` unread counter. The only correct way to add a history entry — callers
    /// (including `mod.rs`'s key handler) must never touch `history` directly.
    pub fn push_history(&mut self, role: TurnRole, text: String) {
        self.history.push_back(Turn { role, text });
        while self.history.len() > MAX_HISTORY_TURNS {
            self.history.pop_front();
        }
        self.note_content_changed();
    }

    /// Bump the `▼ N new` counter when content changes while scrolled away from the
    /// bottom — streaming/new turns must never yank the view back to bottom on their
    /// own (acceptance criterion 3); this is how the operator learns something arrived.
    fn note_content_changed(&mut self) {
        if !self.follow {
            self.new_since_scroll += 1;
        }
    }

    /// Flush `current_reply` (or a system/error line) into history and return to IDLE.
    fn flush(&mut self, role: TurnRole, text: String) {
        self.push_history(role, text);
        self.current_reply.clear();
        self.last_chunk_seq = None;
        self.current_turn_seq = None;
        self.last_event_at = None;
        self.phase = ConversePhase::Idle;
    }

    /// Append a streaming delta chunk. Rejects deltas for a target that's already back
    /// to IDLE (a stray/late delta arriving after `flush()` must never silently reopen
    /// streaming state — it would wedge the target permanently, since a phantom
    /// Streaming with no `last_event_at` can never time out) and deltas whose `turn_seq`
    /// doesn't match the turn currently being accumulated (cross-turn contamination
    /// guard — `chunk_seq` alone resets every turn and can't tell turns apart). Detects
    /// a chunk_seq gap within the accepted turn (dropped chunk, not reordered) and
    /// appends a dim gap-note rather than silently splicing. Idempotent on an
    /// exact-duplicate chunk_seq replay. All three bugs caught during /review's
    /// adversarial pass.
    fn append_delta(&mut self, turn_seq: u64, chunk_seq: u64, text: &str) {
        if self.phase == ConversePhase::Idle {
            return; // stray delta after flush — no-op, never reopens streaming state
        }
        let is_first_delta_of_turn = self.current_turn_seq.is_none();
        match self.current_turn_seq {
            None => self.current_turn_seq = Some(turn_seq),
            Some(t) if t != turn_seq => return, // delta from a different turn — reject
            Some(_) => {}
        }

        if let Some(last) = self.last_chunk_seq {
            if chunk_seq == last {
                return; // duplicate replay — no-op, not a re-append
            }
            if chunk_seq > last + 1 {
                self.current_reply.push_str("[response may be incomplete — connection was busy] ");
            }
        }
        self.last_chunk_seq = Some(chunk_seq);
        self.last_event_at = Some(Instant::now());
        self.phase = ConversePhase::Streaming;

        if self.current_reply.len() + text.len() > CURRENT_REPLY_CAP_BYTES {
            let remaining = CURRENT_REPLY_CAP_BYTES.saturating_sub(self.current_reply.len());
            // Byte-safe truncation: `remaining` is a raw byte count with no relationship
            // to `text`'s UTF-8 char boundaries. Slicing at an arbitrary byte offset
            // panics if it falls mid-character (em dash, smart quotes, emoji — all
            // common in normal model output). Walk back to the nearest valid boundary.
            let safe_end = floor_char_boundary(text, remaining.min(text.len()));
            self.current_reply.push_str(&text[..safe_end]);
            self.current_reply.push_str("[...truncated at 64KB — full reply may be longer]");
        } else {
            self.current_reply.push_str(text);
        }
        // Bump the unread counter once per logical reply (first delta of a turn), not
        // once per chunk — otherwise a single streamed reply while scrolled up shows
        // "▼ 200+ new" instead of "▼ 1 new" (caught during /review's adversarial pass).
        if is_first_delta_of_turn {
            self.note_content_changed();
        }
    }

    /// Scroll up (toward older content), disarming follow on the first press.
    pub fn scroll_up(&mut self) {
        self.follow = false;
        self.scroll_up_lines = self.scroll_up_lines.saturating_add(1);
    }

    /// Scroll down (toward newer content). Re-arms follow once back at the bottom.
    pub fn scroll_down(&mut self) {
        self.scroll_up_lines = self.scroll_up_lines.saturating_sub(1);
        if self.scroll_up_lines == 0 {
            self.re_follow();
        }
    }

    /// `G`/`End` — jump back to the bottom and clear the unread counter.
    pub fn re_follow(&mut self) {
        self.follow = true;
        self.scroll_up_lines = 0;
        self.new_since_scroll = 0;
    }

    /// True when this target has gone quiet for DISPATCH_TIMEOUT since its LAST event
    /// (dispatch or delta) — dead-air detection, not a cap on total turn duration.
    pub fn is_dispatch_timed_out(&self, now: Instant) -> bool {
        matches!(self.phase, ConversePhase::Dispatching | ConversePhase::Streaming)
            && self.last_event_at.map(|t| now.duration_since(t) >= DISPATCH_TIMEOUT).unwrap_or(false)
    }
}

/// Stable-Rust equivalent of the unstable `str::floor_char_boundary`: the largest byte
/// index `<= index` that lands on a valid UTF-8 char boundary. Used to truncate model
/// output safely — an arbitrary byte offset can fall mid-character (em dash, smart
/// quotes, emoji) and panic a raw `&text[..n]` slice.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut idx = index;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Per-view state for the Dashboard chat rail: one `ConverseState` per target ever
/// dispatched to this session, plus which target the input box is currently bound to.
#[derive(Debug, Clone, Default)]
pub struct ConverseView {
    pub targets: HashMap<String, ConverseState>,
    pub active_target: String,
    /// True when the rail (not the agent table) has keyboard focus. Toggled by `Tab`.
    pub rail_focused: bool,
    pub input: String,
}

impl ConverseView {
    pub fn new(default_target: &str) -> Self {
        Self {
            targets: HashMap::new(),
            active_target: default_target.to_string(),
            rail_focused: false,
            input: String::new(),
        }
    }

    /// Retarget the rail to `agent_id`. Does not touch the previous target's state —
    /// it keeps streaming/dispatching in the background (tmux "active pane" model).
    pub fn retarget(&mut self, agent_id: &str) {
        self.active_target = agent_id.to_string();
        self.targets.entry(self.active_target.clone()).or_default();
    }

    /// Fold one Flight SSE event into whichever target's state it belongs to.
    /// A no-op for any agent_id not already a key in `targets` — prevents the map from
    /// silently growing to every agent in the fleet the moment any of them completes a
    /// turn (Eng Phase 1 HashMap-membership finding). Called unconditionally regardless
    /// of active view/focus, so backgrounded targets keep updating (Section 1's
    /// "dashboard behind stays live" requirement).
    pub fn on_flight_event(&mut self, value: &serde_json::Value) {
        let kind = value["kind"].as_str().unwrap_or("");
        match kind {
            "inference_stream_delta" => {
                let agent_id = value["data"]["agent_id"].as_str().unwrap_or("");
                let Some(state) = self.targets.get_mut(agent_id) else { return };
                let turn_seq = value["data"]["turn_seq"].as_u64().unwrap_or(0);
                let chunk_seq = value["data"]["chunk_seq"].as_u64().unwrap_or(0);
                let text = value["data"]["text"].as_str().unwrap_or("");
                state.append_delta(turn_seq, chunk_seq, text);
            }
            "orchestrator_turn_complete" => {
                // Field path verbatim from orchestrate.rs:158 — top-level `agent` is the
                // real id here (consistent, unlike orchestrator_exited below).
                let agent_id = value["agent"].as_str().or_else(|| value["data"]["agent_id"].as_str()).unwrap_or("");
                let Some(state) = self.targets.get_mut(agent_id) else { return };
                let answer = value["data"]["answer"].as_str().unwrap_or("").to_string();
                state.flush(TurnRole::Assistant, answer);
            }
            "agent_failed" => {
                // Field path verbatim from orchestrate.rs:165 — top-level `agent` ONLY;
                // `data` has no agent_id for this kind (Eng Phase 3 landmine finding).
                let agent_id = value["agent"].as_str().unwrap_or("");
                let Some(state) = self.targets.get_mut(agent_id) else { return };
                let reason = value["data"]["reason"].as_str().unwrap_or("unknown");
                state.flush(TurnRole::System, format!("Agent failed: {reason} — press r to retarget, or Enter to resume elsewhere"));
            }
            "orchestrator_exited" => {
                // Field path verbatim from orchestrate.rs:181 — top-level `agent` is a
                // HARDCODED LITERAL "agentd" for this kind; only `data.agent_id` is valid
                // (Eng Phase 3 landmine finding — do NOT trust the top-level field here).
                let agent_id = value["data"]["agent_id"].as_str().unwrap_or("");
                let Some(state) = self.targets.get_mut(agent_id) else { return };
                let reason = value["data"]["reason"].as_str().unwrap_or("unknown");
                state.flush(TurnRole::System, format!("Inject rejected (reason: {reason}) — press Enter to resume"));
            }
            "orchestrator_injected" => {
                // Field path verbatim from orchestrate.rs — top-level `agent` is real.
                let agent_id = value["agent"].as_str().unwrap_or("");
                if let Some(state) = self.targets.get_mut(agent_id) {
                    state.phase = ConversePhase::Dispatching;
                    state.last_event_at = Some(Instant::now());
                }
            }
            _ => {}
        }
    }

    /// Check every target for a client-side dispatch timeout (Eng Phase 1 dead-air
    /// finding) and flush a resume-hint system line for any that fired. Call once per
    /// tick from the render loop (cheap: HashMap of a handful of entries). Returns
    /// `true` if any target's state changed, so the caller knows to trigger a redraw.
    pub fn check_dispatch_timeouts(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        for state in self.targets.values_mut() {
            if state.is_dispatch_timed_out(now) {
                state.flush(
                    TurnRole::System,
                    "No response after 30s — this agentd may not support live streaming yet (upgrade agentd) or the connection dropped. Press Enter to retry.".to_string(),
                );
                changed = true;
            }
        }
        changed
    }
}

/// Spawn-or-resume: if `agent_id` is currently `"waiting"` in the snapshot, inject;
/// otherwise spawn a fresh orchestrated agent with that id. Ported verbatim from
/// `orchestrate.rs`'s existing branch (Eng Phase review: pure logic, zero behavior
/// drift risk from extracting it) — shared by both the CLI and the TUI rail.
pub fn dispatch(
    source: &dyn DataSource,
    agent_id: &str,
    text: &str,
    max_turns: u32,
) -> Result<String, String> {
    let snap = source.load_snapshot();
    let agent_alive = snap.agents.iter().any(|a| a.id == agent_id && a.status == "waiting");

    if agent_alive {
        source.inject(agent_id, text)?;
        Ok(agent_id.to_string())
    } else {
        let req = SpawnRequest {
            task:         text.to_string(),
            id:           Some(agent_id.to_string()),
            max_turns:    Some(max_turns),
            token_budget: None,
            orchestrated: true,
        };
        source.spawn(&req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(agent_id: &str, chunk_seq: u64, text: &str) -> serde_json::Value {
        serde_json::json!({
            "kind": "inference_stream_delta",
            "data": { "agent_id": agent_id, "turn_seq": 1, "chunk_seq": chunk_seq, "text": text }
        })
    }

    /// Test helper: retarget + mark the target as Dispatching, matching what a real
    /// `Enter`-triggered dispatch does before any delta can legitimately arrive.
    /// `append_delta` rejects deltas while `Idle` (the stray-delta-after-flush fix from
    /// /review), so tests exercising delta accumulation must set this up explicitly now.
    fn retarget_and_dispatch(view: &mut ConverseView, agent_id: &str) {
        view.retarget(agent_id);
        view.targets.get_mut(agent_id).unwrap().phase = ConversePhase::Dispatching;
    }

    #[test]
    fn untracked_target_event_is_noop() {
        let mut view = ConverseView::new("orch-default");
        // No dispatch happened — "scout-3" is not a key in targets.
        view.on_flight_event(&delta("scout-3", 0, "hello"));
        assert!(!view.targets.contains_key("scout-3"), "untracked target must not be inserted");
    }

    #[test]
    fn tracked_target_accumulates_deltas() {
        let mut view = ConverseView::new("orch-default");
        retarget_and_dispatch(&mut view, "orch-default");
        view.on_flight_event(&delta("orch-default", 0, "Hello "));
        view.on_flight_event(&delta("orch-default", 1, "world"));
        let state = view.targets.get("orch-default").unwrap();
        assert_eq!(state.current_reply, "Hello world");
        assert_eq!(state.phase, ConversePhase::Streaming);
    }

    #[test]
    fn duplicate_chunk_seq_is_idempotent_noop() {
        let mut view = ConverseView::new("t");
        retarget_and_dispatch(&mut view, "t");
        view.on_flight_event(&delta("t", 0, "Hello"));
        view.on_flight_event(&delta("t", 0, "Hello")); // replay
        assert_eq!(view.targets.get("t").unwrap().current_reply, "Hello");
    }

    #[test]
    fn chunk_seq_gap_appends_gap_note() {
        let mut view = ConverseView::new("t");
        retarget_and_dispatch(&mut view, "t");
        view.on_flight_event(&delta("t", 0, "A"));
        view.on_flight_event(&delta("t", 5, "B")); // gap: 1..4 missing
        let text = &view.targets.get("t").unwrap().current_reply;
        assert!(text.contains("may be incomplete"), "gap note missing: {text}");
        assert!(text.ends_with('B'));
    }

    #[test]
    fn buffer_overflow_truncates_at_cap() {
        let mut view = ConverseView::new("t");
        retarget_and_dispatch(&mut view, "t");
        let big = "x".repeat(CURRENT_REPLY_CAP_BYTES + 100);
        view.on_flight_event(&delta("t", 0, &big));
        let text = &view.targets.get("t").unwrap().current_reply;
        assert!(text.contains("truncated at 64KB"));
        assert!(text.len() < big.len());
    }

    #[test]
    fn retarget_mid_stream_keeps_prior_target_running() {
        let mut view = ConverseView::new("orchestrator");
        retarget_and_dispatch(&mut view, "orchestrator");
        view.on_flight_event(&delta("orchestrator", 0, "still going"));
        view.retarget("scout-3"); // operator retargets away mid-stream
        view.on_flight_event(&delta("orchestrator", 1, " ..."));
        assert_eq!(view.targets.get("orchestrator").unwrap().current_reply, "still going ...");
        assert_eq!(view.active_target, "scout-3");
    }

    #[test]
    fn orchestrator_exited_uses_data_agent_id_not_top_level_agent_literal() {
        let mut view = ConverseView::new("t");
        view.retarget("t");
        view.targets.get_mut("t").unwrap().phase = ConversePhase::Dispatching;
        let ev = serde_json::json!({
            "kind": "orchestrator_exited",
            "agent": "agentd", // hardcoded literal server-side — must NOT be trusted
            "data": { "agent_id": "t", "reason": "already_running" }
        });
        view.on_flight_event(&ev);
        let state = view.targets.get("t").unwrap();
        assert_eq!(state.phase, ConversePhase::Idle, "flush must have fired for target 't', not for 'agentd'");
        assert!(state.history.back().unwrap().text.contains("already_running"));
    }

    #[test]
    fn agent_failed_uses_top_level_agent_not_data_agent_id() {
        let mut view = ConverseView::new("t");
        view.retarget("t");
        view.targets.get_mut("t").unwrap().phase = ConversePhase::Dispatching;
        let ev = serde_json::json!({
            "kind": "agent_failed",
            "agent": "t",
            "data": { "reason": "budget_exceeded" } // no agent_id field, by design
        });
        view.on_flight_event(&ev);
        let state = view.targets.get("t").unwrap();
        assert_eq!(state.phase, ConversePhase::Idle);
        assert!(state.history.back().unwrap().text.contains("budget_exceeded"));
    }

    #[test]
    fn history_ring_buffer_caps_at_max_turns() {
        let mut state = ConverseState::default();
        for i in 0..(MAX_HISTORY_TURNS + 10) {
            state.push_history(TurnRole::Assistant, format!("turn {i}"));
        }
        assert_eq!(state.history.len(), MAX_HISTORY_TURNS);
        assert_eq!(state.history.front().unwrap().text, "turn 10");
    }

    // ── ux.1 acceptance criterion 3: scroll/follow ──────────────────────────────

    #[test]
    fn follow_defaults_true_and_new_content_does_not_bump_unread_counter() {
        let mut state = ConverseState::default();
        assert!(state.follow);
        state.push_history(TurnRole::Assistant, "hi".to_string());
        assert_eq!(state.new_since_scroll, 0, "unread counter must stay 0 while following");
    }

    #[test]
    fn scroll_up_disarms_follow_and_new_content_bumps_unread_counter() {
        let mut state = ConverseState::default();
        state.scroll_up();
        assert!(!state.follow, "scrolling up must disarm follow");
        assert_eq!(state.scroll_up_lines, 1);
        state.push_history(TurnRole::Assistant, "new message".to_string());
        assert_eq!(state.new_since_scroll, 1, "new content while scrolled up must bump the unread counter");
    }

    #[test]
    fn streaming_delta_while_scrolled_up_also_bumps_unread_counter() {
        // append_delta rejects deltas while Idle, so start Dispatching.
        let mut state = ConverseState { phase: ConversePhase::Dispatching, ..Default::default() };
        state.scroll_up();
        state.append_delta(0, 0, "streaming text");
        assert_eq!(state.new_since_scroll, 1, "a streaming delta must never silently yank the view — it must count as unread too");
    }

    #[test]
    fn scroll_down_to_zero_re_arms_follow() {
        let mut state = ConverseState::default();
        state.scroll_up();
        state.scroll_up();
        assert_eq!(state.scroll_up_lines, 2);
        state.scroll_down();
        assert!(!state.follow, "still 1 line up, follow stays disarmed");
        state.scroll_down();
        assert!(state.follow, "back at the bottom, follow re-arms");
    }

    #[test]
    fn re_follow_resets_scroll_and_unread_counter() {
        let mut state = ConverseState::default();
        state.scroll_up();
        state.push_history(TurnRole::Assistant, "msg".to_string());
        assert!(state.new_since_scroll > 0);
        state.re_follow();
        assert!(state.follow);
        assert_eq!(state.scroll_up_lines, 0);
        assert_eq!(state.new_since_scroll, 0, "End/G must clear the unread counter");
    }

    #[test]
    fn dispatch_timeout_fires_after_30s_not_before() {
        let mut state = ConverseState {
            phase: ConversePhase::Dispatching,
            last_event_at: Some(Instant::now() - Duration::from_secs(29)),
            ..Default::default()
        };
        assert!(!state.is_dispatch_timed_out(Instant::now()), "must not fire before 30s");

        state.last_event_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(state.is_dispatch_timed_out(Instant::now()), "must fire after 30s");
    }

    #[test]
    fn timeout_does_not_fire_mid_stream_when_deltas_keep_arriving() {
        // The bug caught during /review: last_event_at was never refreshed on delta
        // receipt, so any turn streaming longer than 30s total got killed mid-stream
        // even while tokens were actively arriving. This is the regression test.
        let mut state = ConverseState {
            phase: ConversePhase::Dispatching,
            last_event_at: Some(Instant::now() - Duration::from_secs(25)),
            ..Default::default()
        };
        // A fresh delta arrives well within the old dispatch window but close to 30s.
        state.append_delta(0, 0, "still going");
        // last_event_at must have been refreshed to "now" by the delta, not left at
        // the stale dispatch-time value.
        assert!(!state.is_dispatch_timed_out(Instant::now()), "a delta must refresh the dead-air clock, not just the original dispatch time");
    }

    #[test]
    fn delta_from_a_different_turn_is_rejected_not_spliced() {
        let mut state = ConverseState { phase: ConversePhase::Dispatching, ..Default::default() };
        state.append_delta(1, 0, "turn one text");
        assert_eq!(state.current_reply, "turn one text");
        // A stray delta claiming turn_seq=2 (a different turn) must be rejected, not
        // appended — chunk_seq alone can't tell turns apart since it resets to 0 per turn.
        state.append_delta(2, 0, "turn two text");
        assert_eq!(state.current_reply, "turn one text", "delta from a different turn_seq must not be spliced in");
    }

    #[test]
    fn delta_after_flush_does_not_reopen_streaming_state() {
        // The bug caught during /review: append_delta unconditionally set
        // phase = Streaming regardless of current phase. A stray delta arriving after
        // flush() (target already back to Idle) would silently reopen Streaming with
        // last_event_at still None — is_dispatch_timed_out.unwrap_or(false) on None
        // means this phantom state can NEVER time out, permanently wedging the target
        // behind the Enter-key busy guard.
        let mut state = ConverseState::default();
        assert_eq!(state.phase, ConversePhase::Idle);
        state.append_delta(1, 0, "late/stray chunk");
        assert_eq!(state.phase, ConversePhase::Idle, "a delta while Idle must not reopen Streaming");
        assert!(state.current_reply.is_empty(), "a rejected stray delta must not be appended");
    }

    #[test]
    fn buffer_truncation_is_char_boundary_safe_not_byte_index_panic() {
        // The critical bug caught during /review: byte-index slicing at the 64KB cap
        // panics if the cutoff falls mid-UTF8-character. Build a reply that lands the
        // cap exactly on a multi-byte character (em dash, 3 bytes) and confirm no panic.
        let mut state = ConverseState { phase: ConversePhase::Dispatching, ..Default::default() };
        // Fill to exactly one byte short of the cap with ASCII, so the NEXT push's
        // multi-byte character is guaranteed to straddle the boundary.
        let filler = "x".repeat(CURRENT_REPLY_CAP_BYTES - 1);
        state.append_delta(1, 0, &filler);
        // "—" (em dash) is 3 bytes in UTF-8; pushing it now forces the truncation logic
        // to cut mid-character unless floor_char_boundary is applied.
        state.append_delta(1, 1, "—more text after the dash");
        assert!(state.current_reply.len() <= CURRENT_REPLY_CAP_BYTES + 64, "must truncate, not accumulate unbounded");
        assert!(state.current_reply.is_char_boundary(state.current_reply.len()), "result must end on a valid UTF-8 boundary");
    }

    #[test]
    fn idle_state_never_times_out() {
        let state = ConverseState::default();
        assert!(!state.is_dispatch_timed_out(Instant::now()));
    }
}
