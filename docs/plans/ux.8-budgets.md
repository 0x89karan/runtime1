# ux.8′ — Budget integrity hotfix (P0-2) [autoplan-approved, split 2026-07-20]

Status: APPROVED (autoplan: CEO dual-voice + Eng/Design/DX subagent; split from full-scope on dual-model User Challenge)
Branch: ux.8
Design doc: `~/.gstack/projects/0x89karan-runtime1/0x89karan-ux-control-panel-design-20260718-204837.md` ("trust after absence")
Closes: audit86-P0-2 (CoS self-brick / unbounded prod) + P1-1 (budget only enforced under ToolUse). Ratifies D2.

## What changed at the autoplan gate (why this is now narrow)

Both CEO voices (Claude + Codex) independently recommended **splitting** the originally-confirmed full scope. Per-agent spend visibility (snapshot/FUSE/TUI), `SetBudget`, and the `BudgetRisk` attention re-key are **deferred to ux.11** (which touches those surfaces anyway). ux.8′ is now purely the P0-2 integrity fix: **restore the constitutional "bounded" invariant and make the always-on CoS survive its budget window.** User accepted the split 2026-07-20.

Verified state at gate: no reset mechanism exists (grep clean); TODOS P0-2 open; prod `cos.agents.toml` ships `global_token_budget = 0` (unbounded — the live invariant violation), dev ships `10_000_000` (bricks in ~1 day); `rm checkpoint.json` guidance still live; no budget gate in `step_need_infer`.

## Scope (P0-2 hotfix only)

1. Rolling-window budget with a **monotonic counter + window-delta view** (NOT rebase-to-zero).
2. Reset fires on a **wall-clock loop tick**, independent of agent liveness.
3. Budget-exhausted always-on agent **parks** (does not terminate) so the reset re-admits it.
4. Per-agent **pre-inference fail-fast gate** in `step_need_infer` (P1-1).
5. `ControlCommand::ResetBudget` + `POST /api/v1/budget/reset` (the D2 manual escape hatch; confirm-channel, reports old→new).
6. **Config flip is a deliverable** + E2E proof.

Deferred to ux.11: per-agent spend on snapshot/FUSE/TUI, `SetBudget` (runtime cap mutation), `BudgetRisk`→"spend risk" re-key. Deferred to ux.13: FUSE-control write path.

## Design (grounded; folds every autoplan critical)

### A. Monotonic counter + window-delta view (Eng F8 + CEO F3 — CRITICAL)
Do **not** zero `total_input`/`total_output` (they feed `context_tokens()`, snapshot, `BudgetRisk`, paging — `agent/mod.rs:225,431-438`). Keep them lifetime-monotonic. Add a persisted **`window_anchor_spent: u64`** (global: on `SchedulerState`; per-agent: on `AgentTask`/`AgentCheckpoint`). "Windowed spend" = `lifetime_spent − window_anchor_spent`. Admission/gate compare the delta against the ceiling. Reset = advance the anchor to current lifetime spend + advance `window_start`. Double-reset impossible by construction; the meter survives for future billing.

### B. Wall-clock reset on a loop tick (Eng F1/F2/F4/F5 — CRITICAL)
- `budget_window_start`: **wall-clock Unix seconds** (`SystemTime`, matching `scheduler.rs:2110/467`), persisted in the checkpoint. On load: if `0`/absent → init to `now` (migration: an epoch-default anchor must NOT trigger a spurious reset). Elapsed via **`now.saturating_sub(start)`** (NTP step-back safe).
- One `fn maybe_rebase_windows(&mut state, sched)` called **once at the top of every scheduler select-loop iteration**, before any event dispatch — not at admission (the three admission read sites `scheduler.rs:824/1221/1280` are downstream of it). Advance by **division, not a loop**: `let n = elapsed / interval; if n>0 { anchor = lifetime; window_start += n*interval; }`.
- **Park-not-terminate (resolves F2):** on budget exhaustion the always-on agent parks into `waiting` with a `budget_exhausted` reason instead of `handle_agent_terminal` (`scheduler.rs:1300-1315`). The loop-tick reset re-admits agents parked on budget. Without this the terminated CoS is never revived and the increment fails its purpose. (Non-orchestrated one-shot agents may still terminate — decide per-agent by whether it's the resident/orchestrated agent; test both.)

### C. Per-agent fail-fast gate (P1-1 — CRITICAL)
Add the pre-inference check at the top of `step_need_infer` (`agent/mod.rs:~439`, AFTER the MaxTurns check `:416` so precedence is deliberate, before the paging block): if **windowed** spend ≥ `token_budget`, park/fail per B. Route the existing paging advisory (`:447`) through the **same windowed metric** (Eng F7) so it re-arms after a reset. Reconcile the comparator with the `:624` backstop (**pick `>=` or `>` for both**; keep `:624` as backstop). Define per-agent **`token_budget == 0` = unlimited** (match the global convention `scheduler.rs:1280`) so a zero-budget agent doesn't brick at spend 0 (Eng F9).

### D. Config + reset endpoint (D2)
- `SchedulerConfig.budget_reset_interval: u64` (secs, `#[serde(default)]`=0=off; also add to the hand-written `Default` impl `config.rs:398-408` — Eng F13). Doc-comment the semantic: when >0, `global_token_budget` is a per-window ceiling.
- `ControlCommand::ResetBudget { target: Target }` where **`Target = Global | Agent(String)`** (typed, not a string collision — Eng F12); `TaggedCommand` stub for ux.13 FUSE parity. Dispatch arm in `dispatch_control_command`.
- `POST /api/v1/budget/reset`: mirror the **spawn** confirm-channel (`management.rs:321-338`), not fire-and-forget inject — return `{target, spent_before, reset_to:0, window_start}`; **404** on unknown agent; 400 malformed; 503 on channel full (DX F5/F6).

### E. The config flip — the actual P0 fix (CEO F1 + DX F1/F2 — CRITICAL, was missing)
- `distro/overlay/etc/agentd/cos.agents.toml`: set a real `global_token_budget` (windowed ceiling) + `budget_reset_interval = 86400`. **Removing the `= 0` unbounded line is the fix that closes the live invariant violation.**
- `agentd/cos.agents.toml`: `budget_reset_interval = 86400`; delete the `rm checkpoint.json` guidance (`:49-54,228-230`) and rewrite to point at the reset window/endpoint.
- Config parse/consistency assertion (guard against a ceiling-with-no-reset combo: `tracing::warn!` when `global_token_budget>0 && budget_reset_interval==0` — DX F4).
- `docs/DEPLOYMENT.md`: "Budget windows" section + a reset-endpoint curl.

### F. Failure legibility (DX F8/F9)
- `global_budget_exhausted` admission-denied payload + error string gain a `remedy` field naming `budget_reset_interval` / the reset endpoint (the brick names its own fix).
- New `EventKind::BudgetReset` (`events.rs:9` variant + the **compile-time** exhaustiveness guard `otel/tests/event_kind_coverage.rs` — Eng F15, hard build break until added). Payload: `{target, spent_before, window_start, interval_secs, windows_advanced}` (DX F9; `windows_advanced` documents restart catch-up).

## Accepted tradeoff (CEO F5, documented not fixed)
Fixed rolling window: an agent that burns its window early goes dark until the next reset. v1 accepts this; token-bucket / soft-cap degradation (cheaper model before hard-deny) is noted as a future option, not built. Documented in DEPLOYMENT.md so it's a known behavior, not a surprise.

## Roadmap correction (both CEO voices)
Soften ROADMAP:1229 "per-agent spend feeds the mv track's per-VM budgets" — rolling-anchor + one shared interval is anti-aligned with per-VM calendar-aligned billing (`docs/PRODUCT-THESIS.md`). The one thing that DOES help mv later is the monotonic counter (§A). Reword to say so.

## Test plan (Eng F19 regression-critical set)
- **The P0 test:** capped flagship spends to ceiling → parks (not terminates) → advance mock wall-clock past interval → loop tick rebases → agent re-admitted and resumes. This is the definition of "P0-2 closed."
- Reset with **zero live agents** (the real self-brick scenario, not just an active agent).
- Restart every `interval-1` for 3× interval wall time → **exactly one** reset (anchor honored, no never-reset, no double-reset).
- Migrated checkpoint with absent `budget_window_start` → no spurious reset.
- Fail-fast: text-only orchestrated agent (EndTurn→park→inject) drives windowed spend past budget → parks at `step_need_infer` BEFORE the next inference (P1-1 regression).
- Windowed metric vs `context_tokens()` divergence after reset (proves §A didn't corrupt context).
- Per-agent `token_budget=0` = unlimited (no brick at spend 0).
- Reset endpoint: confirm-channel reports old→new; unknown target → 404; malformed → 400.
- `default 0` backward-compat: unset interval → behavior identical to today.
- Config consistency: ceiling-with-no-reset warn fires.

## Not in scope
Per-agent spend UI (snapshot/FUSE/TUI), `SetBudget`, `BudgetRisk` re-key → **ux.11**. FUSE-control write path → ux.13. Token-bucket/soft-cap → future (documented tradeoff). Per-agent independent intervals → future (v1: one scheduler interval).

## Dependencies
None blocking. Feeds ux.11 (windowed-spend data + the monotonic meter). Roadmap reshape edit already on this branch (8c25e307).
