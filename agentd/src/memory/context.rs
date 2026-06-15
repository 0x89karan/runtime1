use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::inference::{Block, Msg, Role};

pub const SOFT_THRESHOLD: f64 = 0.75;
pub const HARD_THRESHOLD: f64 = 0.90;

const PREVIEW_CHARS: usize = 200;

/// Memory pressure level based on fraction of token_budget consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryPressure {
    None,
    Soft,
    Hard,
}

/// A single paged turn stored in Tier 2 (short-term eviction buffer).
///
/// `blocks_json` preserves raw blocks for p5.3 recall without a schema change.
/// `role` uses the typed enum to avoid serde fragility with raw strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemItem {
    pub turn:            u32,
    pub role:            Role,
    pub content_preview: String,
    pub blocks_json:     String,
}

/// Map a token count to a pressure level relative to `token_budget`.
///
/// Thresholds are static in p5.2; tunable per-agent thresholds are p5.6.
/// Paging is independent of `memory.enabled` — short_term lives on AgentTask,
/// not in the redb store.
///
/// **F-01:** the *paging* decision feeds this the RETAINED-CONTEXT estimate
/// (see [`estimate_context_tokens`]), not lifetime spend. Lifetime spend only
/// grows, so once it crossed `HARD_THRESHOLD` the old code re-paged every turn
/// and shredded context. The retained estimate falls after paging, which
/// edge-gates the loop. Lifetime spend is still used for the budget guard and
/// advisory telemetry.
pub fn assess(tokens: u64, token_budget: u64) -> MemoryPressure {
    if token_budget == 0 {
        return MemoryPressure::None;
    }
    let pct = tokens as f64 / token_budget as f64;
    if pct >= HARD_THRESHOLD {
        MemoryPressure::Hard
    } else if pct >= SOFT_THRESHOLD {
        MemoryPressure::Soft
    } else {
        MemoryPressure::None
    }
}

/// Rough token estimate of the *retained* context — what the next inference
/// request would resend (`messages`). Uses a ~4-chars-per-token heuristic over
/// every block's textual payload. This drives the F-01 paging decision: paging
/// fires when the working set itself is large relative to the budget, and stops
/// once paging has shrunk it back below threshold (an edge-gate that the old
/// lifetime-spend signal could never reach, since spend never decreases).
pub fn estimate_context_tokens(messages: &[Msg]) -> u64 {
    let chars: usize = messages
        .iter()
        .flat_map(|m| &m.blocks)
        .map(|b| match b {
            Block::Text { text } => text.len(),
            Block::ToolResult { content, .. } => content.len(),
            Block::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
        })
        .sum();
    (chars / 4) as u64
}

/// Number of complete turn PAIRS eligible for eviction.
///
/// Returns 0 when `messages.len() <= 2` (initial task + at most one assistant
/// reply), because there are no complete pairs to drain without destroying the
/// only assistant turn.
///
/// TODO(p5.6): replace FIFO-oldest with a principled eviction policy.
pub fn page_count(messages: &[Msg]) -> usize {
    if messages.len() <= 2 {
        return 0;
    }
    (messages.len() - 1) / 4
}

/// Evict `n` oldest complete turn PAIRS from `messages` into `MemItem` records.
///
/// Invariants maintained:
/// - `messages[0]` (initial task) is never evicted
/// - After eviction, `messages` is still role-alternating
///
/// Serializes blocks before draining — if any serialization fails the whole
/// call returns `Err` and `messages` is left unchanged.
pub fn page_turns(messages: &mut Vec<Msg>, n: usize, at_turn: u32) -> Result<Vec<MemItem>> {
    if n == 0 {
        return Ok(vec![]);
    }
    let to_drain = 2 * n;
    if to_drain >= messages.len() {
        anyhow::bail!(
            "page_turns: to_drain={to_drain} >= messages.len()={}; \
             caller must ensure page_count(messages) == n",
            messages.len()
        );
    }

    // F-07: index 1 must be Assistant for the alternating-role invariant to hold.
    // This was a debug_assert! (compiled out in release, where a violation would
    // silently corrupt the paged history). Promote it to a runtime error so a
    // release build refuses to page rather than mangling the transcript.
    if messages.len() >= 2 && messages[1].role != Role::Assistant {
        anyhow::bail!(
            "page_turns: messages[1] must be Role::Assistant \
             (alternating-role invariant violated); refusing to page"
        );
    }

    // Serialize BEFORE draining so messages is unchanged on failure.
    let mut items = Vec::with_capacity(to_drain);
    for msg in &messages[1..=to_drain] {
        let content_preview = msg
            .blocks
            .first()
            .map(|b| match b {
                Block::Text { text } => truncate_preview(text),
                Block::ToolResult { content, .. } => truncate_preview(content),
                Block::ToolUse { name, .. } => truncate_preview(name),
            })
            .unwrap_or_default();

        let blocks_json = serde_json::to_string(&msg.blocks)
            .map_err(|e| anyhow::anyhow!("page_turns: serde error for blocks: {e}"))?;

        items.push(MemItem {
            turn: at_turn,
            role: msg.role.clone(),
            content_preview,
            blocks_json,
        });
    }

    // All items serialized successfully — now drain.
    messages.drain(1..=to_drain);

    Ok(items)
}

fn truncate_preview(s: &str) -> String {
    let mut chars = s.chars().peekable();
    let out: String = chars.by_ref().take(PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        format!("{out}…")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::{Block, Msg, Role};

    fn text_msg(role: Role, text: &str) -> Msg {
        Msg { role, blocks: vec![Block::Text { text: text.to_string() }] }
    }

    fn tool_result_msg(content: &str) -> Msg {
        Msg {
            role:   Role::User,
            blocks: vec![Block::ToolResult {
                tool_use_id: "tu_1".to_string(),
                content:     content.to_string(),
                is_error:    false,
            }],
        }
    }

    // AC1: assess returns None below SOFT_THRESHOLD
    #[test]
    fn assess_none_below_soft() {
        assert_eq!(assess(74_999, 100_000), MemoryPressure::None);
        assert_eq!(assess(0, 100_000), MemoryPressure::None);
    }

    // AC2: assess returns Soft at exactly SOFT_THRESHOLD
    #[test]
    fn assess_soft_at_threshold() {
        assert_eq!(assess(75_000, 100_000), MemoryPressure::Soft);
    }

    // AC3: assess returns Hard at exactly HARD_THRESHOLD
    #[test]
    fn assess_hard_at_threshold() {
        assert_eq!(assess(90_000, 100_000), MemoryPressure::Hard);
    }

    // AC4: assess returns Hard above HARD_THRESHOLD
    #[test]
    fn assess_hard_above_threshold() {
        assert_eq!(assess(95_000, 100_000), MemoryPressure::Hard);
        assert_eq!(assess(100_001, 100_000), MemoryPressure::Hard);
    }

    // AC5: page_count returns 0 for messages.len() == 1
    #[test]
    fn page_count_zero_for_single_message() {
        let messages = vec![text_msg(Role::User, "task")];
        assert_eq!(page_count(&messages), 0);
    }

    // AC6: page_count returns 0 for messages.len() == 2
    #[test]
    fn page_count_zero_for_two_messages() {
        let messages =
            vec![text_msg(Role::User, "task"), text_msg(Role::Assistant, "reply")];
        assert_eq!(page_count(&messages), 0);
    }

    // AC7: page_count returns correct value for larger lists (len=9 → 2)
    #[test]
    fn page_count_correct_for_larger_list() {
        let messages: Vec<Msg> = (0..9)
            .map(|i| {
                if i % 2 == 0 {
                    text_msg(Role::User, "u")
                } else {
                    text_msg(Role::Assistant, "a")
                }
            })
            .collect();
        assert_eq!(page_count(&messages), 2);
    }

    // AC8: page_turns preserves alternating-role invariant after paging
    #[test]
    fn page_turns_preserves_alternating_roles() {
        let mut messages = vec![
            text_msg(Role::User, "task"),       // [0] initial — never paged
            text_msg(Role::Assistant, "a1"),    // [1] pair 1 assistant
            text_msg(Role::User, "tr1"),        // [2] pair 1 user/tool-result
            text_msg(Role::Assistant, "a2"),    // [3] pair 2 assistant
            text_msg(Role::User, "tr2"),        // [4] pair 2 user/tool-result
            text_msg(Role::Assistant, "a3"),    // [5] next (kept)
        ];
        let n = page_count(&messages); // = (6-1)/4 = 1
        let items = page_turns(&mut messages, n, 3).unwrap();

        assert_eq!(items.len(), 2, "1 pair = 2 items");
        assert_eq!(items[0].role, Role::Assistant);
        assert_eq!(items[1].role, Role::User);

        // Remaining: [User(task), Asst(a2), User(tr2), Asst(a3)]
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[1].role, Role::Assistant);
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[3].role, Role::Assistant);
    }

    // AC9: page_turns leaves initial task message intact (index 0 never paged)
    #[test]
    fn page_turns_never_pages_initial_task() {
        let mut messages = vec![
            text_msg(Role::User, "initial task"),
            text_msg(Role::Assistant, "answer"),
            tool_result_msg("tool output"),
            text_msg(Role::Assistant, "answer2"),
            tool_result_msg("tool output 2"),
        ];
        let n = page_count(&messages); // = (5-1)/4 = 1
        page_turns(&mut messages, n, 0).unwrap();

        if let Block::Text { text } = &messages[0].blocks[0] {
            assert_eq!(text, "initial task", "initial task block must be preserved");
        } else {
            panic!("initial task block must be Text");
        }
    }

    #[test]
    fn page_turns_zero_n_is_noop() {
        let mut messages =
            vec![text_msg(Role::User, "task"), text_msg(Role::Assistant, "reply")];
        let items = page_turns(&mut messages, 0, 0).unwrap();
        assert!(items.is_empty());
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn mem_item_serde_roundtrip() {
        let item = MemItem {
            turn:            3,
            role:            Role::Assistant,
            content_preview: "hello world".to_string(),
            blocks_json:     r#"[{"type":"text","text":"hello world"}]"#.to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: MemItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.turn, 3);
        assert_eq!(back.role, Role::Assistant);
        assert_eq!(back.content_preview, "hello world");
    }

    #[test]
    fn assess_zero_budget_returns_none() {
        assert_eq!(assess(0, 0), MemoryPressure::None);
        assert_eq!(assess(100, 0), MemoryPressure::None);
    }

    // F-01: paging is driven by retained context size, which FALLS after paging,
    // so the loop converges (bounded pages) instead of re-paging every turn the
    // way the old lifetime-spend signal did (spend never decreases).
    #[test]
    fn paging_stops_when_context_below_target() {
        let budget = 1_000u64; // tokens
        let big = "x".repeat(1_000); // ~250 tokens at 4 chars/token
        // [User task, Assistant big, User big, Assistant big, User big]
        let mut messages = vec![text_msg(Role::User, "task")];
        for i in 0..4 {
            messages.push(if i % 2 == 0 {
                text_msg(Role::Assistant, &big)
            } else {
                text_msg(Role::User, &big)
            });
        }
        // Starts Hard: retained ≈ budget.
        assert_eq!(
            assess(estimate_context_tokens(&messages), budget),
            MemoryPressure::Hard,
            "test setup must start above the Hard threshold"
        );

        // Page as the agent loop does, until retained pressure clears.
        let mut pages = 0;
        while assess(estimate_context_tokens(&messages), budget) == MemoryPressure::Hard {
            let n = page_count(&messages);
            if n == 0 {
                break;
            }
            page_turns(&mut messages, n, 0).unwrap();
            pages += 1;
            assert!(pages <= 4, "paging must converge, not loop unbounded (F-01)");
        }
        assert!(pages >= 1, "should have paged at least once");
        assert_ne!(
            assess(estimate_context_tokens(&messages), budget),
            MemoryPressure::Hard,
            "paging must drop retained context below the Hard threshold and stop"
        );
    }

    #[test]
    fn estimate_context_tokens_grows_and_shrinks() {
        let mut messages = vec![text_msg(Role::User, "task")];
        let before = estimate_context_tokens(&messages);
        messages.push(text_msg(Role::Assistant, &"y".repeat(400)));
        let after = estimate_context_tokens(&messages);
        assert!(after > before, "estimate must grow with added content");
        assert_eq!(estimate_context_tokens(&[]), 0);
    }

    // F-07: alternating-role violation must be a runtime Err (not a debug-only
    // assert that compiles out in release and silently mangles the transcript).
    #[test]
    fn page_turns_errs_when_index_one_not_assistant() {
        let mut messages = vec![
            text_msg(Role::User, "task"),
            text_msg(Role::User, "not-assistant"), // invariant violation
            text_msg(Role::Assistant, "a"),
            text_msg(Role::User, "u"),
            text_msg(Role::Assistant, "a2"),
        ];
        let before = messages.len();
        assert!(page_turns(&mut messages, 1, 0).is_err());
        assert_eq!(messages.len(), before, "messages unchanged on Err");
    }

    #[test]
    fn page_turns_err_on_overcount() {
        let mut messages =
            vec![text_msg(Role::User, "task"), text_msg(Role::Assistant, "reply")];
        // n=2 but only 1 pair exists — must Err
        assert!(page_turns(&mut messages, 2, 0).is_err());
        // messages must be unchanged
        assert_eq!(messages.len(), 2);
    }
}
