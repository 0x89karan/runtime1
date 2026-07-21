<!-- /autoplan restore point: (none — plan file created fresh this session) -->
# ux.11a — Budget visibility (SPLIT from ux.11 at autoplan CEO gate)

**Branch:** ux.11 · **Base:** main @ 8260ad2c (v0.89.0 shipped) · **Status:** ✅ APPROVED (autoplan 2026-07-21; per-agent SetBudget scope confirmed)
**Design doc (APPROVED):** `~/.gstack/projects/0x89karan-runtime1/0x89karan-ux-control-panel-design-20260718-204837.md`
**Roadmap entry:** `docs/ROADMAP.md` ux.11

> **SPLIT DECISION (autoplan CEO gate, 2026-07-21, User Challenge resolved):** both CEO voices
> converged that the bundled ux.11 (A trust-after-absence + B budget-visibility) is the design
> doc's rejected Approach B / a big-first-increment. User chose a **2-way split**:
> - **ux.11a (THIS PLAN)** = budget visibility (B): windowed spend on FUSE/TUI + SetBudget endpoint
>   + BudgetRisk re-key. Small, all precedents exist (SetBudget mirrors the ResetBudget path shipped
>   in ux.8′), closes the ux.8′ P1 "visible + settable per-agent spend" debt. **Ships first.**
> - **ux.11b (DEFERRED, own gate)** = trust-after-absence (A): runs.redb + authoritative run records
>   + reuse `otel/src/tail.rs` FileTailer (C2) + catch-up digest (C3) + morning brief + runs_query.
>   The CEO findings C1/C3/C4/C5 (below) are carried into ux.11b's plan. May split further (Codex's
>   11b substrate / 11c UX) at its own autoplan.

## ux.11a scope (budget visibility — active)
- **B1. Per-agent windowed spend** on snapshot → FUSE → TUI. `AgentSnapshot` already carries
  `context_tokens` (lifetime) + `token_budget`; add `windowed_spent` (populate at `update_snapshot`
  scheduler.rs:2548 from `task.windowed_spent()`; thread to reader.rs `AgentInfo` + a new FUSE offset
  + TUI render). **Rendering:** special-case unlimited (`token_budget==0` → `47k spent`, never
  `47k/0`); abbreviate k/M for the 12-col column (Design pre-flag).
- **B2. `SetBudget` runtime mutation:** `ControlCommand::SetBudget { target, limit, confirm_tx }`
  + `POST /api/v1/budget/set` + FUSE `{"set_budget":{...}}` + scheduler dispatch arm (copy the
  `ResetBudget` arm scheduler.rs:2089) + new `AgentTask::set_token_budget(&mut self, u64)` (none
  exists today). TUI edit affordance. **ux.13 boundary:** ux.11a owns the endpoint + semantics;
  ux.13 later only unifies the write path under ControlCommand+FUSE (no new semantics) — no double-claim.
- **B3. `BudgetRisk` → windowed-spend re-key:** `BudgetRisk` already fires in `derive_attention`
  (scheduler.rs:2480) off `assess(context_tokens, token_budget)`. Extend `AttentionInputs` + the
  threshold to key on **windowed** spend vs ceiling; fix label/evidence to read as spend risk.
  Additive; must not regress the existing hard-threshold tests; `token_budget==0` never fires.

## ux.11a — what already exists (leverage map)
- **SetBudget precedent:** `POST /api/v1/budget/reset` (management.rs:342) + `ControlCommand::ResetBudget`
  dispatch (scheduler.rs:2089) — shipped in ux.8′, mirror exactly.
- **windowed_spent:** already on `AgentTask` (agent/mod.rs:264); snapshot population point `update_snapshot`
  (scheduler.rs:2548).
- **BudgetRisk:** already emitted (scheduler.rs:2480); `AttentionReason::BudgetRisk` exists (snapshot.rs:168).
- **TUI:** agent-list render already shows `context_tokens`/`token_budget` (reader.rs AgentInfo:154).

## ux.11a — NOT in scope
- All of trust-after-absence (run store, tailer, digest, brief, runs_query, Runs view) → **ux.11b**.
- Telegram → ux.12. Cancel / SetCaps / pause-resume → ux.13. Remote inject → cut (design P4).

## Problem

The operator can watch agents live but cannot reconstruct what happened while away. Design doc friction #2 (lived): "something failed overnight; per-agent cost and cause were unrecoverable from a live-only flight tail." There is no durable run history, no digest, no morning brief, and per-agent spend is not surfaced anywhere (global only). The success bar for the whole UX track: **"Can the owner wake up, understand yesterday, and unblock today without opening a terminal?"** ux.11 delivers the *understand yesterday* half.

## Scope (as inherited)

ux.11 carries **two** bundles. The design-doc ux.11 row is (A); the ROADMAP note (ux.8′ split, 2026-07-20) folded (B) into it "because ux.11 touches these surfaces anyway."

### A. Trust after absence (design-doc ux.11)
- **A1. Durable run records** in a new `runs.redb` derived from `flight.jsonl`: `{agent_id, start, end, status, last_error, approvals, spend, stop_reason}`. Granularity: **per-run segment** (spawn→terminal, and a park boundary closes a segment) — recommended in the design doc, ratify at autoplan.
- **A2. flight.jsonl tailer**: a byte-offset tailer (none exists today) that derives run records incrementally and persists last-processed offset in the runs store `META` table. **Must survive copy-truncate** (see Open Decision D2).
- **A3. In-process digest timer**: a `tokio::time::interval` arm in the scheduler select! blocks (NOT cron_mcp — fewer moving parts, sidesteps audit86-P2-3 missed-fire). Fires the brief build on a configurable cadence.
- **A4. Native `runs_query` tool** for the CoS (copy `KbSearch`; opt-in registration in `register_native`; capability-gated).
- **A5. CoS-written morning brief into the chat rail**: layer a rail delivery (Inject or a new flight event the rail renders) on top of the existing `ops:briefs` KB write the cos-orchestrator already does.
- **A6. TUI "Runs" view** in agentctl (`View::Runs` + `render_runs`): last N runs → status, spend, failure reason, minimal event tail.
- **A7. Records exposed via management API (`GET /api/v1/runs`) + FUSE** (`/agents/runs` or per-agent file, Linux-gated).

### B. Budget visibility (inherited from ux.8′ split)
- **B1. Per-agent windowed spend** on snapshot → FUSE → TUI. `AgentSnapshot` already carries `context_tokens` (lifetime) + `token_budget`; add `windowed_spent` (populated at `update_snapshot`, threaded to reader.rs + a FUSE offset). **Rendering must special-case unlimited** (`47k spent`, never `47k/0`) and abbreviate to k/M for the 12-col column (Design review pre-flag).
- **B2. `SetBudget` runtime mutation**: `ControlCommand::SetBudget` + `POST /api/v1/budget/set` + FUSE `set_budget` + scheduler dispatch arm (copy `ResetBudget`) + new `AgentTask::set_token_budget` setter (none exists). **Boundary with ux.13:** ux.13 later only *unifies* SetBudget under ControlCommand + FUSE — ux.11 ships the endpoint + TUI edit. The two autoplans must not both claim new semantics; ux.11 owns the semantics, ux.13 owns the FUSE-control unification. (Per ROADMAP; confirm no double-claim.)
- **B3. `BudgetRisk` → spend-risk re-key**: `BudgetRisk` already fires in `derive_attention` off `assess(context_tokens, token_budget)`. Extend `AttentionInputs` + the threshold to key on **windowed** spend vs ceiling, fix the label/evidence to read as spend risk. Additive; must not regress the existing hard-threshold tests.

## What already exists (leverage map — from touch-point survey)
- **redb store idiom**: `agentd/src/memory/store.rs` (RedbStore, TableDefinition, open-or-create, txn patterns) — the exact template for `RunsStore`.
- **Native store-backed tool**: `KbSearch` in `tools/native.rs:703` — template for `runs_query`.
- **SetBudget precedent**: `POST /api/v1/budget/reset` (management.rs:342) + `ControlCommand::ResetBudget` dispatch (scheduler.rs:2089) — mirror exactly.
- **windowed_spent**: already on `AgentTask` (agent/mod.rs:264) from ux.8′; snapshot population point is `update_snapshot` (scheduler.rs:2548).
- **BudgetRisk**: already emitted (scheduler.rs:2480); AttentionReason::BudgetRisk exists (snapshot.rs:168).
- **Digest timer precedent**: the `maybe_rebase_windows` loop-top + 60s idle tick I added in ux.8′ (scheduler.rs:583/609) — same in-process wall-clock pattern.
- **Brief content**: cos-orchestrator already writes `ops:briefs` KB + `./output/brief-*.md` (cos.agents.toml:260). Only the rail *delivery* is new.
- **TUI view pattern**: `View` enum (watch/app.rs:14) + `render_approvals`/`render_credentials` (views.rs:1243) — template for Runs.

## NOT in scope (deferred)
- Full replay scrubber (old ux.7) — design doc keeps deferred; ux.11 ships minimal drill-down only.
- Telegram / remote reach → ux.12.
- Cancel / SetCaps / pause-resume → ux.13 (after cap.1).
- flight.jsonl full rotation policy → run.1 (unless pulled forward — see D2).
- Remote inject → cut per design-doc P4.

## Open Decisions (design doc punted these to this autoplan)

- **D1 — Scope bundling.** Ship A+B as one increment, or split into ux.11a (budget visibility B, small, continues ux.8′) + ux.11b (trust-after-absence A, the new subsystem)? The ROADMAP bundled them "because ux.11 touches these surfaces anyway," but A alone is a full increment (new redb store + tailer + timer + tool + TUI view). The v0.86 audit warned against big-first-increments. → surface at gate.
- **D2 — flight.jsonl rotation dependency.** The tailer (A2) reads flight.jsonl by byte offset, but flight.jsonl has no rotation (audit86-P1-2, slated run.1) and no offset-tracking today. Options: (a) ship offset-tracking that *tolerates* copy-truncate (detect offset > file len → reset to 0, like the obs.3 otel sentinel already does), leaving rotation to run.1; or (b) pull P1-2's size-threshold copy-truncate rotation forward into ux.11. Recommend (a): smaller, and the sentinel precedent proves it works.
- **D3 — Run-record granularity.** Per-agent-lifetime vs per-run segment (spawn→terminal, park closes a segment). Recommend per-run segment (design-doc rec).
- **D4 — Brief rail delivery mechanism.** Inject a turn (`ControlCommand::Inject`) vs a new flight event the rail renders vs a FUSE file. Recommend a new flight event (rail-visible, cross-platform, no turn-injection side effects on the CoS conversation).
- **D5 — ux.2b fold-in.** Run records already know "no events for N minutes" — do the idle/error attention signals (ux.2b, closes cos-ux-01) fold into ux.11 or stay separate? Recommend separate (keep ux.11 bounded).

## Test plan (to be expanded in eng phase)
- RunsStore: open/create, schema init, put/get run record, offset persistence in META, corruption quarantine (copy RedbStore test shape).
- Tailer: derive record from a synthetic flight.jsonl; **copy-truncate survival** (offset > len → reset); park-closes-segment boundary.
- Digest timer: fires on cadence via injectable clock (mirror the ux.8′ `maybe_rebase_windows_at` test pattern); no loop-spin.
- runs_query tool: capability gate; returns records; empty-store case.
- SetBudget: endpoint 200/404/400; dispatch mutates ceiling; windowed enforcement picks up new ceiling; FUSE parse arm.
- windowed_spend serialization: **`AgentSnapshot` manual Serialize field_count bump** (the documented trap — snapshot.rs:286) + the `attention_signal_serializes_on_agent_snapshot` regression guard must extend.
- BudgetRisk re-key: fires at windowed-spend threshold; existing hard-threshold tests adjusted, not deleted; unlimited (budget=0) never fires.
- TUI Runs view: render with 0 / N runs; unlimited-budget render (`47k spent` not `47k/0`).

## Success criteria (design doc, ux.11 slice)
Morning: a brief in the chat rail naming every run, its spend, its outcome, and anything blocked — zero flight.jsonl reading. Per-agent spend visible in the TUI. A runaway spend is visible as a BudgetRisk attention signal before the budget bricks.

---

## Phase 1 — CEO Review (autoplan)

### CEO dual voices — consensus table
```
  Dimension                            Claude   Codex   Consensus
  ──────────────────────────────────── ──────── ─────── ─────────
  1. Premises valid post-ux.8′?         partial  partial CONFIRMED (P1 half-unmet: B is orphaned ux.8′)
  2. Right problem (history) now?       yes      yes*    CONFIRMED (*Codex: sequencing vs cancel/reach)
  3. Scope calibration correct?         NO-SPLIT NO-SPLIT CONFIRMED → SPLIT
  4. Alternatives sufficiently explored? no       no      CONFIRMED (cron catch-up; existing tailer; cut A6)
  5. Competitive/market risk?           n/a      n/a     N/A (single-tenant personal OS)
  6. 6-month trajectory sound?          NO(crit) NO(high) CONFIRMED (log-as-truth; history-before-verbs)
```
Both voices: **SPLIT**. Codex → 3 increments (11a budget-visibility / 11b run-history substrate / 11c digest+brief UX). Claude → 2 (11a budget-visibility / 11b trust-after-absence). Full agreement that A+B is the design doc's *rejected* Approach B (the big-first-increment the v0.86 audit warned against), reconstituted under the ux.11 name.

### Findings that survive regardless of the split (both models, independently)
- **C1 (CRITICAL, Claude) — durable history must NOT derive from the best-effort log as source of truth.** Invariant: "Logging is best-effort and must never crash an agent." Making flight.jsonl the source of truth for run history means a dropped event = a run silently missing from the brief, breaking the "names *every* run" contract. Fix: write run records from the **authoritative scheduler state-machine transitions** (spawn/terminal/park), use the tailer only to enrich (approvals, spend detail). Also collapses most of A2's complexity.
- **C2 (HIGH, Claude) — a copy-truncate-surviving tailer ALREADY EXISTS.** `otel/src/tail.rs` `FileTailer` (312 lines, tracks (dev,ino,offset,sentinel), tested for copy-truncate + fast-grow). My touch-point map said "none exists" — it was crate-blind. Fix: extract `FileTailer` into a shared module and consume it; do NOT build a second tailer (D2 recommendation was based on wrong facts).
- **C3 (HIGH, Claude+Codex) — in-process digest timer misses the exact absence it exists for.** A live `tokio::time::interval` only fires if agentd stayed up across the fire time; overnight the machine sleeps / container restarts and the 07:00 brief is empty on the mornings it matters. Same missed-fire hole the plan dismissed cron_mcp for. Fix: **catch-up on a persisted last-brief-timestamp** (the division-based catch-up ux.8′ already uses for window rebasing), not a live interval.
- **C4 (HIGH, Claude) — per-run spend ≠ windowed_spent.** `windowed_spent` is a rolling 24h delta, not per-segment. Per-run spend must be Δ(lifetime spend) across the segment's flight events. The leverage map wrongly implied windowed_spent feeds run records.
- **C5 (MEDIUM, Claude) — the TUI Runs view (A6) is the most cuttable item.** The doc's headline is "the brief is written by an agent, not rendered by a UI." If `runs_query` answers "why did scout fail at 3am" in the chat rail, A6 is replay-scrubber-lite — defer it and add back only if brief + conversational drill-down leaves something wanting.
- **C6 (Codex, strategic) — sequencing challenge:** "trust is a ladder: know spend → stop damage → get reached → reconstruct history." Codex argues cancel (emergency brake) and remote reach (Telegram approve/deny) may outrank history/digest, since visibility without a stop action is a dashboard failure. Counterweight: the design doc deliberately chose history-first ("frequency over drama") and the operator's lived friction #2 was reconstruction. This is the user's call.

### Decision Audit Trail
| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 1 | CEO | Split A/B vs keep bundled | **USER CHALLENGE** | n/a (never auto-decided) | Both models agree the roadmap's bundle should split; **RESOLVED: user chose 2-way split** (ux.11a budget-visibility first / ux.11b trust-after-absence) |
| 2 | CEO | C1 log-as-truth → author from scheduler transitions | Mechanical (invariant) | P1 completeness | Best-effort-log-as-source-of-truth violates a stated invariant → **carried to ux.11b** |
| 3 | CEO | C2 reuse otel FileTailer | Mechanical | P4 DRY | Finished tested impl exists → **carried to ux.11b** |
| 4 | CEO | C3 catch-up not live timer | Mechanical | P1 completeness | Live interval fails the absence premise → **carried to ux.11b** |
| 5 | CEO | C5 cut A6 TUI Runs view | Taste | P3 pragmatic | Conversational drill-down may subsume it → **carried to ux.11b** |
| 6 | Eng | F1 SetBudget scope = **per-agent only** (drop Global) | Mechanical (feasibility) | P5 explicit / P3 pragmatic | Global ceiling `sched.global_token_budget` is immutable `&SchedulerConfig`; making it settable needs a SchedulerState promotion + checkpoint change (beyond S). Reject Global with 400; defer to a later increment |
| 7 | Eng | F2 SetBudget dispatch calls `drain_deferred` | Mechanical (correctness) | P1 completeness | Raising a ceiling only revives a deferred agent when drain runs; mirror `ResetBudget::Agent` |
| 8 | Eng | F3 `set_token_budget` mutates checkpointed `cfg.token_budget` | Mechanical (correctness) | P1 completeness | Avoids the P2-1 restart-revert; round-trip test |
| 9 | Eng | F4 bump manual Serialize field_count + extend guard | Mechanical | P1 completeness | Guard only checks `attention` today; won't catch a missing `windowed_spent` |
| 10 | Eng | F5 **replace** `context_tokens` with `windowed_spent` in AttentionInputs | Mechanical | P4 DRY / P5 explicit | Today's BudgetRisk already keys on lifetime spend (not context-window); re-key is a fix, and replace-not-add avoids a dead field |

## Phase 3 — Eng Review (autoplan)

### Eng dual voices — consensus table
```
  Dimension                            Claude   Codex   Consensus
  ──────────────────────────────────── ──────── ─────── ─────────
  1. SetBudget architecture sound?      NO(F1)   NO(F1)  CONFIRMED — global not settable → per-agent only
  2. Revival semantics correct?         NO(F2)   NO(F2)  CONFIRMED — must drain_deferred on raise
  3. Persistence correct?               NO(F3)   NO(F3)  CONFIRMED — mutate checkpointed cfg.token_budget
  4. Serialization safe?                NO(F4)   NO(F4)  CONFIRMED — bump field_count + extend guard
  5. BudgetRisk re-key safe?            YES(F5)  ~(F5)   CONFIRMED — safe; replace not add; keep tests
  6. Edge cases covered?                partial  partial CONFIRMED — universal-tier 404 (doc), lower=next-admission (doc)
```
Both voices agree on all six. Codex raised BudgetRisk conflation as a *risk*; the Claude voice proved it unfounded in this codebase (the signal already keys on lifetime spend, not context occupancy) → re-key is a fix, consensus CONFIRMED-safe.

### Finalized ux.11a spec (all fixes folded in)
**B1 — windowed spend visibility**
- Add `windowed_spent: u64` to `surfaces::AgentSnapshot`; **bump manual `field_count` 18→19** and add the `serialize_field` (snapshot.rs:286); populate from `task.windowed_spent()` at `update_snapshot` (scheduler.rs:2548, native) and `windowed_spent: 0` at the universal-tier literal (2595 — universal spend is proxy-tracked).
- Extend the `attention_signal_serializes_on_agent_snapshot` guard to assert `json["windowed_spent"]`.
- Thread to agentctl `AgentInfo` (reader.rs:154) + new FUSE offset (agents_fs.rs, Linux-gated) + TUI render.
- **Rendering:** dedicated compact formatter — `47k spent` when `token_budget==0` (never `47k/0`); `47k/100k`, `1.2M/2M` otherwise; k/M abbreviation; **test the cell string ≤ 12 chars**.

**B2 — SetBudget (per-agent only)**
- `ControlCommand::SetBudget { target: BudgetTarget, limit: u64, confirm_tx: Option<oneshot::Sender<Result<(u64,u64),String>>> }` (payload = `(old_budget, new_budget)`; `Err` → 404). **`BudgetTarget::Global` → 400 "global budget is not runtime-settable"** (F1).
- `TaggedCommand::SetBudget` (`#[serde(rename="set_budget")]`) + parse arm with empty-id validation (mirror `reset_budget`, control.rs:129).
- `POST /api/v1/budget/set` body `{"target":{"agent":"cos"},"limit":50000}` (`limit:0` = unlimited); 400 on missing/negative/non-integer limit; 404 unknown agent; 503 no control_tx; return `{"target":"cos","old_limit":..,"limit":..,"windowed_spent":..,"revived":bool}`. Mirror the reset handler (management.rs:342) + its test suite (management.rs:955).
- `AgentTask::set_token_budget(&mut self, u64)` = `self.cfg.token_budget = n;` (mutates the **checkpointed** field, F3).
- Scheduler dispatch arm (copy `ResetBudget::Agent`, scheduler.rs:2089): set budget → emit a new `EventKind::BudgetSet` → **`drain_deferred`** if `limit==0 || task.windowed_spent() < limit` (F2) → reply old→new; `None` agent → `Err` → 404.
- Docs: lowering takes effect at next admission (not mid-inference preempt); universal-tier agents return 404 (consistent with ResetBudget).

**B3 — BudgetRisk re-key**
- In `AttentionInputs` (scheduler.rs:2404) **replace** `context_tokens` with `windowed_spent`; populate from `task.windowed_spent()` at the `derive_attention` call site (2542). `assess()` returns None when `token_budget==0` so unlimited never fires (unchanged). Adjust the `da(...)` test helper + hard-threshold tests to feed windowed spend (**adjust, don't delete**). Label/evidence already read as spend.

### Test plan (ux.11a)
- SetBudget: endpoint 200 (old→new payload) / 404 (unknown agent) / 400 (empty/malformed/negative/Global) / 503 (no control_tx); dispatch mutates `cfg.token_budget`; **raise revives a deferred agent** (drain_deferred fires); lower → next admission defers/terminates; `to_checkpoint→from_checkpoint` round-trip persists the new budget.
- windowed_spent: manual-Serialize presence (extended guard); FUSE offset/readdir/content (Linux); universal-tier literal = 0.
- BudgetRisk: fires at windowed-spend hard threshold; `token_budget==0` never fires; existing hard-threshold tests adjusted green.
- Rendering: unlimited (`47k spent`), bounded (`47k/100k`, `1.2M/2M`), cell ≤ 12 chars.
- `make clippy-linux` before push (agents_fs.rs is a false-green on macOS).

### Design / DX (folded — proportionate for an S increment)
- **Design (rendering pass):** the one Design concern (pre-flagged in the roadmap) is the 12-col budget cell — resolved by the dedicated formatter above (unlimited special-case + k/M + ≤12-char test). No new states; the TUI edit affordance reuses the existing input pattern (tui-input, per ux.10).
- **DX (endpoint pass):** `/api/v1/budget/set` is consistent with `/budget/reset` (same `target` shape); errors name the problem (400 lists the offending field; 400 for Global names *why*). No new SDK/CLI surface beyond the agentctl edit key.
