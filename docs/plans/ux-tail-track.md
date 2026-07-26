# UX tail — track plan (ux.2b · ux.3 · ux.10)

Interactive track-level planning, 2026-07-26. Grounded against current `main` (v0.109.0) by a
read-only sweep; decisions made interactively by the operator. **Sequence: ux.2b → ux.3 → ux.10.**
One increment per branch, `main` shippable at each step. Deep-dive each (draft → /plan-eng-review or
/autoplan → build → /review → /qa → /ship) when it's built; this doc locks scope + records the
grounding so those sessions start from facts, not the stale roadmap prose.

Each closes a tracked gap: ux.2b → **cos-ux-01**, ux.3 → **p7.3-ar-02**.

---

## 1. ux.2b — Idle + Error attention signals (closes cos-ux-01)

**Verdict: design VALID, zero drift.** ux.2a landed exactly the promised substrate — the
`AttentionReason` enum (`surfaces/src/snapshot.rs:168-175`, currently `ApprovalPending, Degraded,
BudgetRisk, EvaluationUnavailable`) was *deliberately* built to accept `Idle`/`Error` as additive
variants (doc-comment at :163-165), `derive_attention()`/`AttentionInputs`
(`scheduler.rs:3167-3262`) pre-plans for them, and the `views.rs` render path is compiler-forced
exhaustive so nothing is silently missed. Active plan: `docs/plans/ux.2-attention-evidence.md`
(the `ux.2-observe.md` file is SUPERSEDED — do not implement from it).

**Delta (verified unstarted):**
1. Add fields to `AgentTask` (`agentd/src/agent/mod.rs:67-102` — confirmed ABSENT today):
   `last_event_at` (wall-clock) + `last_error` (+ `error_count` if useful). Idle is DERIVED, not stored.
2. Stamp `last_event_at` at the `CallTools` dispatch site (`scheduler.rs:1873-1887`, which holds
   `&mut state` synchronously — the tool-loop future can't mutate `AgentTask`, so the stamp must
   happen here; the "CallTools-dispatch-site fix" the plan names is still needed + applicable), plus
   the spawn/send/approval interception sites; stamp `last_error` where a tool/inference error is recorded.
3. Extend `AttentionInputs` + `derive_attention` to emit `Idle` and `Error`, honoring the plan's
   `Waiting`-status idle carve-out (a parked/orchestrated agent isn't "idle").
4. Add the 2 enum variants + their `severity()`/`label()` + the `views.rs` match arms
   (`classify_attention`, `attention_glyph_and_style`, `age_display`) + `reader.rs` mirror.
5. Plumb through FUSE (`agents_fs.rs`).

**LANDMINE (plan-flagged):** idle = `now − last_event_at` computed at **read time** (FUSE/HTTP handler),
NOT server-side at snapshot time — else FUSE and HTTP disagree and idle "freezes" at the last snapshot.

**Size:** small, low-risk, additive. Good first increment.

---

## 2. ux.3 — Spawn custom on the fly, CORE + auto-drop (closes p7.3-ar-02)

**Scope DECIDED: core + auto-drop. The `:` command palette + modal-over-dashboard are DEFERRED to a
follow-on `ux.3b`** (both are net-new UI subsystems with no reusable infra — not worth bundling).

**Grounding:** the headline "custom caps" is mostly already built —
- `POST /api/v1/spawn` is complete (`management.rs:478-522`): parses `OperatorSpawnRequest`
  (`control.rs:6-20`: `task, id, max_turns, token_budget, priority, capabilities, orchestrated`),
  cap.4 deny-by-default privileged gate + `X-Approval-Token`, returns `201 {agent_id}`.
- The HTTP client `HttpSource::spawn()` exists (`source.rs:290-316`); the cap-toggle UI + template
  picker + preview (renders the exact JSON payload) all exist (`spawn.rs`, `app.rs:242-318`).
- The interactive Spawn view spawns via **FUSE-control-write or exec-a-2nd-agentd**
  (`mod.rs:955-1022`) — it never calls the HTTP `/spawn`.

**Delta (the real, small work):**
1. **Add `capabilities` (+ `priority`) to the client `SpawnRequest`** (`source.rs:16-23` — MISSING them
   today, so the HTTP path silently drops caps). This is the load-bearing fix.
2. Route `execute_pending_spawn` (`mod.rs:955`) through `DataSource::spawn()` (the source abstraction),
   so it uses `/api/v1/spawn` into the running instance instead of FUSE-control/exec branches. Preserve
   the FUSE path where that's the active source; the point is to stop exec'ing a 2nd agentd.
3. **Auto-drop:** after a confirmed spawn, auto-focus the new agent (banner already exists at
   `mod.rs:227`; the focus-routing is the new bit).
Keep the existing full-screen `View::Spawn` (no modal). cap.4's deny-by-default gate means a custom-cap
spawn either uses safe caps or needs `AGENTOS_ALLOW_PRIVILEGED_SPAWN=1` — surface that clearly in the UI.

**DEFERRED → ux.3b:** `:` command palette, modal-over-live-dashboard overlay.

---

## 3. ux.10 — TUI polish, ALL 3 sub-parts + deps

**Scope DECIDED: all three.** Size is a non-issue — agentctl is ~2.48 MB vs the 6 MB CI guard
(~3.5 MB headroom); the 3 new deps add ~200 KB. Plan: `docs/plans/ux.10-tui-polish.md`.

1. **`[g]` Logs view** — tail `docker compose logs --follow` (host subprocess) into a 2000-line
   `VecDeque` ring buffer (`logs.rs` + `docker.rs`, both new) with per-service filter + `/` search +
   follow mode. **Docker-context-gated** (D1: `docker compose ps --quiet` at startup; absent on bare
   agentd; logs come from the host subprocess, no agentd protocol coupling — clean).
2. **Input ergonomics** — swap the **5** confirmed hand-rolled `push(c)/pop()` sites (all in
   `mod.rs`): converse rail (:608), memory search (:464), inspector search (:780), deny reason (:859)
   → `tui-input`; spawn task field (:710) → `tui-textarea` (multi-line).
3. **`color-eyre` panic hook** — guaranteed terminal restore even on a **non-main-thread** panic (the
   existing `Drop`-guard at `mod.rs:154-171` only covers the main render thread — genuinely additive).

**LANDMINE:** `[g]` is ALREADY bound (Spawn view → "generate preview", `mod.rs:750`). Key handling is
per-view so no technical clash, but pick a **different, free global key** for Logs (resolve at build —
don't ship two meanings for `g`). Also: the plan's `make check-size` target does NOT exist (the size
guard lives only in CI `ci.yml`) — either add the target or drop that acceptance line.

**Deps to add to `agentctl/Cargo.toml`:** `tui-input`, `tui-textarea`, `color-eyre` (none present today).

---

## Deferred out of this track
- **ux.3b** — `:` command palette + modal-over-dashboard overlay (split from ux.3).
- Evidence-gated expansions **ux.6 / ux.5 / ux.7** (post-tail, per roadmap).
- The `orchestrate.rs`/`converse.rs` shared-module refactor + the CLI-shows-truncated-reply TODOs
  (ux.1 leftovers) are separate, not part of this tail.
