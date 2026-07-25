# par.1-ar-01 — agentctl error view is blind to tool + inference errors

Branch: `par.1-ar-01-error-view` · Base: `main` (v0.104.0)
Kind: behavioral agentctl (TUI) fix. Real operator-facing bug found by par.1's exhaustiveness guard.

## The bug
agentctl's operator error view matches flight-event **kind strings that agentd never emits**:
- **Errors filter** (`inspector.rs:47-49`): `"kind":"tool_error"` ‖ `"kind":"inference_error"` ‖ `"kind":"agent_failed"`.
- **Red-colour rule** (`views.rs:1229-1230`): the same two dead strings.

`tool_error` and `inference_error` are **not** `EventKind`s (verified against `events.rs::as_str()`).
So the operator's "Errors" filter and the red highlight catch ONLY `agent_failed` — every tool
failure and inference failure is invisible in the one view meant to surface them. par.1 documented
these two dead strings in `KNOWN_NONCANONICAL` and its `known_noncanonical_entries_are_actually_absent`
test was written to force this fix (it shrinks the day the strings stop being matched).

## What errors actually look like (from `events.rs`)
- **Tool failure:** `EventKind::ToolResult` → `"tool_result"` with `data: { error, is_error: true }`
  (events.rs:117). The error-ness is a DATA FIELD (`data.is_error == true`), not the kind — a plain
  `tool_result` is a success. This is why it "isn't a string swap" (the original par.1-ar-01 note).
- **Inference failure / agent death:** `EventKind::AgentFailed` → `"agent_failed"` (already matched).
- **General error:** `EventKind::Error` → `"error"` (a real kind, NOT currently matched).
- **Other error-class kinds:** `EventKind::McpHttpError` → `"mcp_http_error"`, `EventKind::FuseControlError`
  → `"fuse_control_error"` (real kinds; currently invisible in the Errors view).

## Design decisions (for the gauntlet)
- **D1 — which kinds count as "an error"?** Minimum to fix the reported bug: `agent_failed`, `error`,
  and `tool_result`+`is_error`. Completeness argues also `mcp_http_error` + `fuse_control_error`
  (they're error-class and an operator scanning "Errors" wants them). Recommendation: include all
  five (the two http/fuse ones are cheap and it's the operator's error view — completeness wins).
  `capability_denied` stays in its own `CapDenied` filter (not folded in).
- **D2 — how to match `tool_result` + is_error?** The whole inspector predicate is substring-based
  (`line.contains(...)`). Two options: (A) substring — `contains("\"kind\":\"tool_result\"") &&
  contains("\"is_error\":true")` (consistent with existing style, but `is_error` is nested under
  `data` so it relies on the compact-serde form `"is_error":true` with no space — verify the exact
  serialization, and the AND avoids a false-positive on a non-tool line that happens to carry
  is_error); (B) parse the JSON line once and check `kind`/`data.is_error` structurally (robust, but
  introduces JSON parsing into a hot per-line path that's currently allocation-free substring). Lean
  (A) for consistency + zero-alloc, with a test pinning the exact serialized form so a data-shape
  change is caught. Confirm whether any OTHER event carries `is_error:true` (deny/rejected — events.rs:125
  mentions "is_error result" for operator-rejected actions) so the tool_result AND-guard is necessary.
- **D3 — DRY the predicate.** The filter (`inspector.rs`) and the colour rule (`views.rs`) currently
  DUPLICATE the dead-string list. Extract ONE shared `is_error_event(line: &str) -> bool` (in
  inspector.rs or a shared module) used by BOTH, so they can never again disagree — this is the exact
  drift class par.1 guards. Do NOT fix them independently.
- **D4 — clean up par.1's allowlist.** Remove `tool_error`/`inference_error` from agentctl's
  `KNOWN_NONCANONICAL` (event_kind_strings.rs) since they're no longer matched. par.1's
  `known_noncanonical_entries_are_actually_absent` test is designed to REQUIRE this — confirm it
  passes after the allowlist shrinks (and would fail if the dead strings were left in the code).

## Acceptance criteria
- The Errors filter + the red-colour rule share ONE predicate and both surface: `agent_failed`,
  `error`, `tool_result`+`is_error:true`, `mcp_http_error`, `fuse_control_error`. A plain (success)
  `tool_result` is NOT flagged.
- Tests: an error `tool_result` (is_error:true) IS matched; a success `tool_result` is NOT; each of
  the always-error kinds is matched; the shared predicate is used by both sites (a test or structural
  guarantee). Update the existing `inspector_filter_errors_matches_error_events` test (it currently
  asserts the DEAD `tool_error`/`inference_error` strings match — that assertion must flip).
- `KNOWN_NONCANONICAL` no longer lists the two dead strings; par.1's guard tests green.
- `cargo build/clippy --workspace --all-targets -D warnings` clean; `cargo test --workspace` green.
  No `cargo fmt`.

## NOT in scope
- budget.1-ar-01 (MaxTokens resident signal) — separate behavioral increment.
- cap.3 / budget.1-ar-02 — design-bearing, separate.

## Risk
LOW-MEDIUM. Small agentctl TUI change, no runtime/scheduler impact. The one real risk is D2 (matching
`is_error` correctly against the actual serialized form) — a wrong substring would either miss tool
errors (bug persists) or false-flag successes (noise). Pinned by a serialization-form test.

---

## /autoplan adjustments (2026-07-25) — dual-voice Eng review (Codex + Claude subagent)
Premise CONFIRMED (both: bug real, fix direction sound). Both recommended **adjust**. Applied:

- **D2 verified + settled.** Fail tool serializes (agent/mod.rs:1025) as
  `{...,"kind":"tool_result","data":{...,"is_error":true,...}}` — compact `serde_json::to_string`, so
  `"is_error":true` (no space), nested under `data`. Use substring option (A) with the AND-guard:
  `line.contains("\"kind\":\"tool_result\"") && line.contains("\"is_error\":true")`. Zero-alloc, ≤5
  `contains()`/line over ≤500 tail lines — negligible. The AND-guard is load-bearing: `fuse_control_error`
  ALSO carries `data.is_error:true` (matched by KIND, not the tool predicate); the model-facing
  `Block::ToolResult{is_error}` sites are NOT flight lines; embedded error previews are JSON-escaped
  (`\"`) so `"is_error":true` can't appear inside a string value. Success = `is_error:false`.
- **D3 settled.** `pub(crate) fn is_error_event(line: &str) -> bool` in `inspector.rs`; `views.rs` calls
  `super::inspector::is_error_event(s)` (already imports `super::inspector::InspectorFilter`; same
  `watch/` module tree; free fn over `&str` — no borrow/visibility issue).
- **D4 CORRECTED (my plan was wrong).** Must edit BOTH lists in `event_kind_strings.rs`: DELETE
  `tool_error`/`inference_error` from `AGENTCTL_KIND_MATCHES` (:26-27) AND from `KNOWN_NONCANONICAL`
  (removing from only one orphans them → `every_agentctl_kind_match_is_a_real_event_kind` fails). ADD
  the newly-matched real strings to `AGENTCTL_KIND_MATCHES` with site comments: `tool_result`, `error`,
  `mcp_http_error`, `fuse_control_error` (+ `egress_proxy_failed`, `credential_refresh_failed` if D1
  picks 7). **Correction:** the earlier claim that par.1's `known_noncanonical_entries_are_actually_absent`
  test "forces" this cleanup is FALSE — fixing production while touching neither list leaves both guards
  green. This is manual hygiene, not test-forced. (Do not ship claiming the test enforces it.)
- **TWO tests break, not one** (my plan undercounted): (1) `inspector_filter_errors_matches_error_events`
  (:166) asserts the DEAD strings match — flip it; (2) `inspector_state_rebuild_applies_filter` (:257)
  seeds `{"kind":"tool_error"}`+`{"kind":"tool_result"}` under Errors and asserts len==1 — after the fix
  that's 0 matches → re-fixture (e.g. `agent_failed` + a success `tool_result`, or a `tool_result` with
  `is_error:true`). Note `inspector_state_rebuild_caps_scroll` (:272) currently passes by coincidence
  (empty list) — re-fixture for clarity.

### D1 — the one open decision for the gate: 5 kinds vs 7
Codex: exactly **5** (`agent_failed`, `error`, `tool_result`+is_error, `mcp_http_error`,
`fuse_control_error`); don't broaden into policy/denial kinds (`capability_denied`, `egress_denied`,
`budget_exceeded` — those are status/policy with their own filters). Claude: also fold in **2** more
unambiguous *failure* kinds currently invisible in every filter — `egress_proxy_failed` (failed to
write a receipt) and `credential_refresh_failed`. Recommendation: **7** (completeness — they are
failures by name, not policy/denial; omitting them silently repeats the exact "error view blind to
errors" bug). `capability_denied`/`egress_denied`/`budget_exceeded` stay OUT (policy/status, separate
filters) either way.
