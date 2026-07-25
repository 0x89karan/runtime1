# budget.1-ar-01 — MaxTokens truncation reports clean success for one-shot agents

Branch: `budget.1-ar-01-maxtokens-resident` · Base: `main` (v0.105.0)
Kind: behavioral scheduler/agent fix. **Regression-risky** — the wrong fix reopens the CoS
self-brick (audit86-P0-2). Found by the AUDIT-v0.97 holistic /review (filed as budget.1-ar-01).

## The bug
`agent/mod.rs:724`: `StopReason::MaxTokens` hard-fails ONLY when `!budget_resettable`. But
`scheduler.rs:1510` sets `budget_resettable` for **every** agent whenever `budget_reset_interval > 0`
(the recommended prod config). So under prod config, a `MaxTokens` truncation for ANY agent falls
through to the recoverable arm (`agent/mod.rs:739`), which extracts the (truncated) text, records
`AgentCompleted`, sets `terminal = true`, and returns `AgentEffect::Completed(truncated_text)`.

For a **resident/orchestrated** agent that's correct (it parks and is resumable — the fix for P0-2).
For a **one-shot job or a spawned child** (`deliver_content = true`) it's wrong: the model's response
was truncated mid-generation, but the agent reports **clean completion** and feeds the **partial
output to its parent** as if it were the finished answer. Silent truncation presented as success.

Rare-trigger (only when the model actually hits its per-response `max_tokens` output cap), non-security.
The sealed-job CoS path is shielded (cap.2b `deliver_content=false` → the trigger gets an agentd
signal, not the job text), but a plain spawned child with `deliver_content=true` is exposed.

## Hard constraint (do NOT violate)
The fix must PRESERVE "a resident/orchestrated agent does not brick on a single MaxTokens truncation"
(audit86-P0-2 / ux.8-ar-02). Do NOT just revert to `AgentEffect::Failed` for resettable agents.

## Where the knowledge lives (the crux)
The resident-vs-one-shot distinction is in the **scheduler**, not on `AgentTask`:
- `state.orchestrated` (set) — resident/REPL agents (resumed via `resume_for_orchestration`).
- `state.awaiting: child_id → AwaitingParent { deliver_content }` — spawned children + whether output
  is delivered to the parent.
- `handle_agent_terminal` — the terminal funnel that delivers/cleans up on completion.
`AgentTask::step()` (where the MaxTokens arm is) is blind to all of this — it only has `budget_resettable`.

## Design options (for the gauntlet)
- **D1 — where the role-decision lives.**
  - **(A) Signal down:** thread a `resumable`/`resident` bool onto `AgentTask` (set by the scheduler at
    wire/construction from `orchestrated` membership). MaxTokens arm: resumable → park (as today);
    else → distinct outcome. Simple locally, but couples AgentTask to a scheduler concept + must be
    kept correct across spawn/restore.
  - **(B) Report up (recommended lean):** AgentTask reports the truncation *distinctly* — a `truncated:
    bool` on the Completed effect, or a new `AgentEffect::CompletedTruncated(text)` — and the SCHEDULER,
    which already knows the role, decides in `handle_agent_terminal`: orchestrated → park/resume
    (unchanged, no brick); one-shot/child → the D2 behavior. Keeps the role-knowledge where it already
    is; AgentTask stays role-agnostic. Bigger surface (new effect/variant + scheduler arm) but cleaner.
- **D2 — behavior for a one-shot / delivered child on truncation.** Options: (i) **Fail** it (parent
  sees an error, not partial text) — closest to pre-window semantics but loses the partial work; (ii)
  **Deliver but flag** — complete with the truncated text AND mark it `truncated:true` in the event +
  the delivered payload so the parent/operator isn't misled; (iii) **Continue-generate** — append a
  continuation turn and re-infer so the model finishes (most correct, biggest change, risks runaway
  turns). Lean (ii) as the honest, low-risk default; (iii) is a separate larger increment.
- **D3 — scope / minimal-viable.** Is the full role-gated behavior (B+ii) in scope, or is the
  "minimum honest interim" (just add `truncated:true` to the AgentCompleted event + delivered content
  for ALL resettable truncations, no role-gating) enough for this increment? The interim stops the
  *silent* part cheaply; the role-gating is the complete fix. Boil-the-lake argues the complete fix,
  but the regression surface (scheduler `handle_agent_terminal`) is exactly the P0-2 danger zone.

## Acceptance criteria (draft)
- A resident/orchestrated agent hitting MaxTokens still PARKS + is resumable — NEVER fails/bricks
  (a test pinning the P0-2 fix stays green).
- A one-shot / `deliver_content=true` child hitting MaxTokens no longer reports clean success with
  silently-truncated text — per D2, it either fails or its completion+delivery is explicitly flagged
  `truncated`.
- The `truncated` signal (if D2=ii) is visible in the flight event AND in what the parent receives.
- `cargo build/clippy --workspace --all-targets -D warnings` clean; `cargo test --workspace` green.
  No `cargo fmt`. New tests cover: resident-truncation-parks (P0-2 preserved), one-shot-truncation-<D2>.

## NOT in scope
- budget.1-ar-02 (universal soft-ceiling / reservation metering) — design-bearing, separate.
- Continue-generation (D2-iii) if the gauntlet defers it — separate larger increment.

## Risk
MEDIUM-HIGH for a small increment: the blast radius is the scheduler's terminal funnel +
the MaxTokens arm, which is exactly where the P0-2 self-brick lives. Every change must be checked
against "resident agent survives a MaxTokens truncation."

---

## /autoplan RESOLVED (2026-07-25) — dual-voice Eng (Codex + Claude subagent). Premise confirmed. Design:
- **D1 = B (new variant).** Add `AgentEffect::CompletedTruncated(String)` — NOT a `truncated` bool on
  `Completed`. Exhaustiveness forces BOTH production match sites (scheduler dispatch ~`scheduler.rs:1933`
  and the CLI shim `agent/driver.rs:85`) to consciously handle truncation; a bool would let
  `driver.rs:85` (`Completed(answer) => Ok(answer)`) silently perpetuate the bug. Option A (a flag on
  `AgentTask`) REJECTED — `orchestrated` is *dynamic* scheduler state (an agent can enter it at
  REPL-park, after `AgentTask::new`), so a threaded flag goes stale; stale-false re-bricks a resident
  (a P0-2 regression). `AgentTask` stays role-agnostic; the scheduler decides via `state.orchestrated`.
- **D2 = i (Fail one-shot/child).** `budget_resettable` changes ONLY the resident park-not-brick path;
  a one-shot/child truncation reverts to the no-window baseline = Fail. Zero new plumbing — the scheduler
  routes a non-orchestrated `CompletedTruncated` through the existing `handle_agent_terminal(Err(...))`:
  `deliver_content=true` → parent gets an `is_error=true` `ToolResult` naming the truncation (parent SEES
  it); sealed job → "failed" signal (cap.2b shielding intact); `run_tracker` → status "failed" (honest).
  (D2=ii deliver-but-flag-via-content-marker is a trivial optional follow-up if salvaging spent partial
  work is later judged worth the run_tracker honesty gap.)
- **D3 = full role-gated** (reject minimal/event-only): the parent LLM reads `ToolResult.content`, not the
  flight event, so a JSON field never reaches it — the fix must touch the delivery path (one
  `state.orchestrated.contains()` away).

### Build recipe
1. `agent/mod.rs`: split the MaxTokens arm three ways —
   `MaxTokens if !budget_resettable => Failed(...)` (legacy, unchanged);
   `MaxTokens => { record BudgetExceeded{recoverable:true}; CompletedTruncated(answer) }` (resettable);
   `EndTurn | Other(_) => Completed(answer)` (clean). Add the `CompletedTruncated(String)` variant.
2. `scheduler.rs` dispatch (~:1933): `CompletedTruncated(answer) =>` if `state.orchestrated.contains(&id)`
   run the SAME park code as `Completed` (OrchestratorTurnComplete, `waiting.insert`, do NOT call
   `handle_agent_terminal`); else `handle_agent_terminal(id, Err(anyhow!("model output truncated at
   max_tokens (partial response discarded)")), …)`.
3. `agent/driver.rs:85`: `CompletedTruncated(_) => Err(...)` (a lone CLI agent is a one-shot; matches
   non-resettable semantics — call out in the PR as an intentional CLI behavior change).
### Tests
- Update the unit test `max_tokens_resettable_agent_parks_not_bricked` (~`mod.rs:1881`) to expect
  `CompletedTruncated(answer)` (same payload) — it currently asserts `Completed`, will go red otherwise.
- Keep `max_tokens_with_partial_text_returns_failed` (non-resettable legacy) green, unchanged.
- NEW scheduler test 1 (pins P0-2 at the dispatch layer — currently UNPINNED there): orchestrated agent
  + `CompletedTruncated` → `state.waiting.contains(id)`, agent still in `state.agents`, `outcomes` empty.
- NEW scheduler test 2 (proves the one-shot fix): child in `awaiting{deliver_content:true}` +
  `CompletedTruncated` → parent receives `ToolResult{is_error:true}` (or `outcomes[child]` is `Err`).
- No checkpoint/restore change (both confirmed `budget_resettable` is recomputed on restore;
  `CompletedTruncated` is a transient step-time signal). No `in_flight` accounting change (no new futures).
