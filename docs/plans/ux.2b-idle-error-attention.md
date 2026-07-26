<!-- /autoplan restore point: ~/.gstack/projects/0x89karan-runtime1/ux.2b-idle-error-attention-autoplan-restore-20260726-140619.md -->
# ux.2b — Idle + Error attention signals (closes cos-ux-01)

Branch: `ux.2b-idle-error-attention` · Base: `main` (v0.110.0)
Kind: additive feature on a purpose-built substrate. First increment of the UX tail (ux.2b → ux.3 → ux.10).
Track plan: `docs/plans/ux-tail-track.md` §1. Active attention plan: `docs/plans/ux.2-attention-evidence.md`
(NOT `ux.2-observe.md`, which is SUPERSEDED).

## Premise

The operator dashboard (`agentctl watch`) surfaces *attention signals* — why an agent needs a human's eyes.
ux.2a shipped the substrate (`AttentionReason` enum, `derive_attention()`, exhaustive `views.rs` render) and
**deliberately reserved two variants for this increment** — the doc-comment at `surfaces/src/snapshot.rs:163`
reads: *"`Error` and `Idle` are deferred to a follow-on increment (ux.2b) — their prerequisite [substrate]."*
Today an agent that silently wedged (no events for minutes) or hit a tool/inference error looks identical to
a healthy one on the dashboard. cos-ux-01 (found during live Gmail testing) is exactly this gap: the operator
can't see "this agent is stuck" or "this agent errored" at a glance.

**Claim:** add the two reserved variants (`Idle`, `Error`) end-to-end, deriving `Idle` from a new
wall-clock `last_event_at` on `AgentTask` and `Error` from a new `last_error`, so the dashboard shows both.

This is additive on a substrate built for it — low risk, real operator value, closes a tracked UX gap. The
premise is well-founded; the real decisions are the derivation details (idle threshold, the Waiting carve-out,
read-time vs snapshot-time computation), not whether to build.

## Grounding (verified 2026-07-26)

- `AttentionReason` (`surfaces/src/snapshot.rs:168`): `ApprovalPending, Degraded, BudgetRisk,
  EvaluationUnavailable`. `severity()` (:192) and `label()` (:204) are exhaustive `match`es; adding a variant
  is compiler-forced everywhere. `Degraded`=Critical, `BudgetRisk`/`EvaluationUnavailable`=Warning,
  `ApprovalPending`=Info (routing precedence is separate from severity — :178).
- `derive_attention(AttentionInputs)` (`scheduler.rs:3179`) + `struct AttentionInputs<'a>` (:3167); called at
  the snapshot build site (:3306) and in tests (:7017). Test suite starts at :7053.
- `AgentTask` (`agent/mod.rs:67`): `last_event_at` / `last_error` / `error_count` confirmed **ABSENT** today.
- `views.rs` render path is exhaustive-match (compiler-forced); `reader.rs` mirrors it for the plain (non-TUI)
  reader; FUSE exposes attention at `agents_fs.rs` (`/agents/<id>/attention`, JSON — added ux.2a).

## Delta (the work)

1. **`AgentTask` fields** (`agent/mod.rs:67-102`): add `last_event_at: <wall-clock instant>` and
   `last_error: Option<String>` (+ `error_count: u32` only if a count is cheap and useful for the label).
   **Idle is DERIVED at read time, never stored.** Fields default such that a just-spawned agent is not
   instantly "idle" (seed `last_event_at` at spawn).
2. **Stamp `last_event_at`** at the `CallTools` dispatch site (`scheduler.rs:1873-1887` — holds `&mut state`
   synchronously; the tool-loop future can't mutate `AgentTask`, so the stamp MUST be here, not inside the
   future). Also stamp at the spawn / send / approval interception sites where the scheduler already holds
   `&mut state`. Stamp `last_error` (+ bump `error_count`) where a tool/inference error is recorded.
3. **Extend `AttentionInputs` + `derive_attention`** (`scheduler.rs:3167/3179`) to emit `Idle` (when
   `now − last_event_at > threshold` AND status is not `Waiting`/orchestrated-parked) and `Error` (when
   `last_error` is set). Honor the plan's **Waiting-status idle carve-out**: a parked/orchestrated agent is
   *intentionally* quiet, not stuck — it must never read as `Idle`.
4. **Enum + render**: add `Idle`, `Error` to `AttentionReason` + their `severity()`/`label()` arms
   (`Error`→Critical, `Idle`→Warning, subject to review), + the `views.rs` match arms (`classify_attention`,
   `attention_glyph_and_style`, `age_display`) + the `reader.rs` mirror.
5. **FUSE**: plumb both through `/agents/<id>/attention` (`agents_fs.rs`) — no new inode, they ride the
   existing attention JSON.

## LANDMINE (plan-flagged — must respect)

**Idle is `now − last_event_at` computed at READ time** (in the FUSE/HTTP handler), NOT server-side at
snapshot-build time. If it's computed when the snapshot is built, idle "freezes" at the last snapshot and FUSE
vs HTTP disagree. So `derive_attention` computes idle against the *reader's* now, or the reader re-derives the
idle bit from a carried `last_event_at`. Decision to lock at eng review: does the snapshot carry
`last_event_at` (so each surface computes idle freshly) vs. does `derive_attention` take `now` as an input.

## Decisions for autoplan

- **D1 — idle threshold.** A fixed default (e.g. 120s?) vs. configurable (`cockpit.toml`)? Recommend a fixed
  const for ux.2b (simplest, one number to tune later), documented, with a `set_*_for_test` seam.
- **D2 — read-time computation shape.** Snapshot carries `last_event_at` and each surface computes
  `now − last_event_at` (freshest, but every surface must remember to) vs. `derive_attention(now, …)` recomputed
  per read (centralizes the logic). Recommend the latter — one derivation, called per read with the reader's now.
- **D3 — `error_count` in scope?** Add the counter + show "N errors" in the label, or just the latest
  `last_error` string? Recommend latest-error-only for ux.2b unless the count is free.
- **D4 — does `Error` auto-clear?** When an agent recovers (next successful step), does `last_error` reset?
  Recommend: yes — a successful `CallTools`/step clears `last_error` so a transient error doesn't stick forever.

## Acceptance

- `AttentionReason::{Idle, Error}` exist with `severity()`/`label()`; every exhaustive match (`views.rs`,
  `reader.rs`, serialization) handles them (compiler-forced).
- An agent with no events for > threshold and status ≠ Waiting shows `Idle` on the TUI, plain reader, and
  `/agents/<id>/attention` FUSE JSON; a parked/orchestrated (Waiting) agent does NOT.
- An agent whose last tool/inference call errored shows `Error` with the message surfaced; a subsequent
  successful step clears it (per D4).
- Idle is computed at read time — a unit/integration test asserts idle advances between two reads of the same
  snapshot without a new snapshot build (guards the landmine).
- `derive_attention` unit tests extended: Idle fires past threshold, does NOT fire for Waiting, Error fires on
  `last_error`, precedence with existing signals is sane (an ApprovalPending + Idle agent routes correctly).
- `cargo build/clippy --workspace --all-targets -D warnings` clean; `cargo test --workspace` green. No `cargo fmt`.
- Flight-recorder invariant preserved: any new stamp/record is best-effort, never panics.

## REVISED MECHANISM (post-/autoplan 2026-07-26 — supersedes Delta + D1-D4 above)

Both eng voices (Codex + Claude subagent) independently reshaped the *how* (premise unchanged). The plan as
originally drafted would have shipped broken. Locked corrections (all CONFIRMED by both unless noted):

- **M1 (CRITICAL) — Idle is READ-TIME, not in `derive_attention`.** `derive_attention` runs only at
  snapshot-build (`scheduler.rs:3306`); FUSE (`agents_fs.rs:479`) and HTTP (`management.rs:245`) just
  serialize the stored `attention` vec. Worse: in the hung-tool wedge, `update_snapshot` isn't even ticked
  (the 250ms tick arm only runs when `pending.is_empty()`), so a build-time idle FREEZES in exactly the
  cos-ux-01 scenario. → Idle becomes a read-time helper `AgentSnapshot::idle_signal(now, threshold) ->
  Option<AttentionSignal>` computed from the carried `last_event_at` + `status`. `Error` STAYS in
  `derive_attention` (it's a state fact — `last_error` present/absent — captured by the `update_snapshot`
  after any errored step; add `last_error` to `AttentionInputs`).
- **M2 — D2 = option (a) (option (b) is infeasible).** `derive_attention` takes scheduler-internal inputs
  (`pending_approvals`, `credential_snapshot`) that don't exist at the read surface, so "call it per-read"
  can't work. Carry `last_event_at` (Unix secs `u64`) on `AgentSnapshot`; the shared `surfaces` helper
  `idle_signal(now, threshold)` is merged in at the **two server read surfaces only** — FUSE `:479` and the
  HTTP handler's pre-serialize pass (`management.rs:245`, already clones under the read lock). `agentctl`
  stays dumb (renders whatever arrives) so FUSE- and HTTP-fed clients agree. SSE (`management.rs:437`)
  carries tokens/stdout, not snapshot JSON — NOT a third injection site.
- **M3 — stamp `last_event_at` ONCE at the top of `enqueue_or_defer`** (`scheduler.rs:1813+`), the universal
  effect chokepoint that holds `&mut state` synchronously and covers `Infer`/`CallTools`/`SpawnAgent`/
  `RunJob`/`SendMessage`/`RequestApproval`. This subsumes the 4 sites the draft listed **plus `Infer`** (the
  draft's critical miss — a pure-reasoning agent would false-Idle). Every result → `step()` → `enqueue_or_defer`,
  so this also covers "results just arrived." Terminal effects (`Completed`/`Failed`) are Done/Failed status,
  where Idle is suppressed anyway.
- **M4 — Idle carve-out = ALLOWLIST `status == Running`** (not a Waiting denylist). A genuinely-wedged agent
  resolves to `Running` (`:3298` fallback), so the allowlist keeps the real signal while closing every
  false-positive: `Waiting`, `Deferred`, `AwaitingChild`, `AwaitingApproval`, `Done`, `Failed` all suppress.
- **M5 — `Error` scoped to TOOL errors on a still-Running agent.** An inference error already →
  `handle_agent_terminal` → `Failed` status (`:829-858`), so an Error signal there is moot. Stamp `last_error`
  when `EffectResult::Tools` carries an `is_error` block; **auto-clear (D4)** when a subsequent
  `EffectResult::Tools` returns all-ok (not merely "next step" — an `Infer` step isn't tool-recovery proof).
  Latest-error string only, **no `error_count`** (D3).
- **M6 — Idle DEFINITION + threshold (resolves the in-flight-Idle concern).** Idle = **"no completed
  progress event in N seconds."** A tool/inference that legitimately runs longer than N still reads Idle —
  and that is INTENDED: an operator *wants* to see a tool hanging for minutes (that IS cos-ux-01). So no
  per-agent busy-tracking. **N default = 180s** (120s was borderline-low given long streaming/tool calls),
  a `surfaces`-crate const with a `#[cfg(test)]` seam (D1).

## DECIDED at the gate (2026-07-26)
- **D-clock → Instant, runtime-only** (not checkpointed; re-seeded to now in `new()` + `from_checkpoint()`).
- **D-threshold → 180s** (`surfaces`-crate const with a `#[cfg(test)]` seam).
- Plan APPROVED as reshaped (M1-M6 + these two). Ready to build.

## OPEN DECISIONS for the gate (the 2 the voices didn't settle — now DECIDED above)

- **D-clock — Instant vs SystemTime for `last_event_at`.** DISAGREE. Claude: `Instant`, **runtime-only, NOT
  checkpointed**, re-seeded to "now" in `new()` AND `from_checkpoint()` (mirrors the `last_pressure` reset at
  `agent/mod.rs:271` — time-relative runtime state isn't carried across a restart); monotonic, immune to
  wall-clock jumps; snapshot-build converts to carried Unix secs via `now_unix.saturating_sub(elapsed)`.
  Codex: `SystemTime` Unix secs, checkpointed, so idle survives a restart. **Recommend Claude's Instant/
  runtime-only** — a freshly-restored agent hasn't acted yet, so re-seeding to now is correct; checkpointing
  would make an agent idle-before-crash instantly false-read Idle on restore, and adds a wall-clock-jump
  failure mode for no benefit.
- **D-threshold-value — 180s** (recommended) is a dial the operator will feel: too low = healthy long tools
  nag; too high = a wedge sits unflagged. 180s is the both-voices compromise; surfaced so you can set it.

## Landmine-guard test (the acceptance gate that ENFORCES the architecture)
Build ONE `AgentSnapshot` with `last_event_at = T`; assert `idle_signal(T+10, 180) == None` and
`idle_signal(T+200, 180) == Some(Idle)` **without rebuilding the snapshot.** If idle were computed inside
`derive_attention` (internal clock), this test literally cannot be written — so its existence proves the
read-time build. Plus: suppression per non-Running status (one assert each), Error fires+auto-clears in
`derive_attention`, and precedence (ApprovalPending+Idle routes to ApprovalPending — verify `agentctl`'s
`classify_attention` picks by `Ord`, not vec position; place `Error` above `BudgetRisk`, `Idle` last in the
`AttentionReason` declaration; `Error`→Critical, `Idle`→Warning severity).

## /review (Codex adversarial) — outcome (2026-07-26)
Verdict FIX → two Medium must-fixes applied, one Low deferred as documented:
- **F1 (fixed) — synthetic tool errors bypassed `last_error`.** The scan lived only in the async
  `EffectResult::Tools` handler, so synthetic `is_error` reject blocks (spawn-denied, run_job reject,
  approval reject, send_message failure, no-control approval) never set Error. **Fix:** moved the
  is_error → set/clear logic INTO `AgentTask::provide_tool_results`, through which ALL tool-result paths
  (real + synthetic) funnel — one place, uniform. Empty batch leaves the prior error untouched. Test:
  `provide_tool_results_sets_and_clears_last_error`.
- **F2 (fixed) — universal-tier false-idle.** Universal snapshots anchored `last_event_at_unix = now_unix`,
  so a STALE universal snapshot (>threshold, scheduler blocked on native effects) would false-read Idle.
  **Fix:** anchor `u64::MAX` (saturating_sub → 0 forever, staleness-proof — universal liveness is
  proxy-tracked, never idle-eligible). Test: `attention_file_merges_read_time_idle` covers the u64::MAX
  never-idle control.
- **F3 (deferred, Low) — backward wall-clock jump.** The read-time age uses `now_unix.saturating_sub(anchor)`;
  a backward system-time step can briefly suppress Idle on a wedged agent until wall time catches up
  (forward jumps → early Idle). Accepted as a known edge for the simple Unix-anchor: idle is a best-effort
  liveness hint, NTP steps are small/rare and self-correcting, and the monotonic-read alternative (carry
  `snapshot_built_at: Instant` + age-at-build) adds real complexity for a rare, non-critical drift. Revisit
  only if it bites in practice.
- **Sound-per-area (Codex confirmed):** checkpoint omission (no `last_event_at`/`last_error` serialized,
  re-seeded on restore), manual `Serialize` field_count 20 matches, allowlist correct, no inference
  double-signal, multibyte-safe `chars().take(160)`, panic-safe `get_mut` stamp.
- Added FUSE read-time merge test (`attention_file_merges_read_time_idle`) — Codex's coverage-gap note.

## NOT in scope
- ux.3 (spawn-on-the-fly) and ux.10 (TUI polish) — later in the tail.
- Per-agent busy/in-flight tracking (M6 defines Idle to not need it).
- Monotonic read-time age machinery (F3) — deferred; the Unix anchor is the explicit-over-clever choice.
- New attention *reasons* beyond Idle/Error (the enum stays at 6).
- Reworking the routing-precedence model (ux.2a's severity-vs-routing split stays as-is).

## Risk
LOW. Additive on a substrate built for exactly these two variants; exhaustive matches make omissions
compile errors. The one care point is the read-time-idle landmine (D2) — get that wrong and idle is stale
but not broken.
