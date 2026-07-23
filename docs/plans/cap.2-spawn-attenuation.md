<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/cap.2-autoplan-restore-20260723-114958.md -->
# cap.2 — spawn attenuation

**Source:** `docs/AUDIT-v0.86.md` §6 (build order) + P1-10 (= cos-dev-02). **Depends:** cap.1
(v0.93.0 ✅ shipped — the `agentd check` linter + `tier_legality`/`satisfies` matcher this
builds on). **Predecessor:** `main` at v0.93.0.

## Problem (the audit's #1 security item)

`SpawnConfig` (`config.rs:424`) has **no capabilities field**, and `dispatch_spawn`
(`scheduler.rs:1902`) does `let child_caps = parent_cap_set.clone()` — **every child inherits
the parent's FULL capability set, all-or-nothing.** The CoS Curator processes
attacker-influenceable email text daily (the inbox agent's summary of untrusted inbound mail)
and inherits `Credential{Google}`, so a prompt-injection payload surviving into curation can
call **live Gmail through the broker** with every hardening layer cooperating. Post-cred.6 the
broker made capability scoping load-bearing; spawn inheritance makes it decorative for
children. The `cos.agents.toml:297` comment "spawned agents inherit the parent's full
capability set" is accurate — and that's the bug.

**This is NOT the cap.1 ergonomics reframe.** cap.2 is genuine least-privilege attenuation
against prompt-injection-via-untrusted-data — the constitution's "capability scoping between
agents is least-privilege" applied to the one place untrusted data enters the system.

## What already exists (leverage map)
- **The matcher:** `capability::satisfies(granted, required)` already answers "does this cap
  set cover this cap?" — the exact subset primitive (cap.1 kept it sound + well-tested).
- **`dispatch_spawn`** (`scheduler.rs:1788`): already receives `parent_cap_set`, enforces
  `max_spawn_depth`, builds `child_specs = registry.filtered_specs(child_caps)`. The one line
  to change is `let child_caps = parent_cap_set.clone()` (1902).
- **`spawn_agent` tool** (`native.rs`): input_schema has task/child_id/priority/token_budget —
  add `capabilities`. `SpawnConfig` (`config.rs:424`) is the deserialize target.
- **cap.1's `tier_legality` + `agentd check`:** child cap declarations could later be linted too.

## Scope (cap.2 — from §6)

- **A1. `SpawnConfig.capabilities: Option<Vec<Capability>>`** (`config.rs`) + a `capabilities`
  property on the `spawn_agent` tool input_schema (`native.rs`). Absent = inherit parent
  (backward compat); `Some` = the child's requested set.
- **A2. Subset validation (⊆ parent) — MANDATORY, fail-closed.** In `dispatch_spawn`: if the
  child requests capabilities, every requested cap must be covered by the parent set (reuse
  `satisfies(parent_caps, &child_cap)` — the same matcher, no second interpreter). Any cap
  outside the parent → **reject the spawn with a recorded error** (new event, e.g.
  `AgentSpawnDenied`). Without the subset check the field is an escalation vector, not an
  attenuation.
- **A3. `max_turns` passthrough** (closes cos-polish-adv-F2/F5): add `max_turns: Option<u32>`
  to `SpawnConfig` + tool schema; `dispatch_spawn` uses it (currently children get the default).
- **A4. CoS prompt update** (`agentd/cos.agents.toml` + distro copy): the orchestrator passes
  per-child caps in `spawn_agent` — **inbox** = `Mcp{google_oauth}` + `Credential{Google}` +
  the KB segments it writes; **curator** = **KB-only, NO Gmail/Credential**. Fix the
  "inherits the parent's full capability set" comment (make `cos.agents.toml`'s intent true).
- **A5. `agentd check` awareness (optional, cap.1 tie-in):** N/A for tool-driven caps (they're
  runtime, not config) — note the boundary; do not extend the linter here.

## Acceptance (from §6)
- The **curator's flight log shows no Gmail tool specs** (filtered_specs omits google_oauth
  for a KB-only child).
- A child requesting a capability **outside the parent set is rejected with a recorded error**
  (subset validation, fail-closed) — regression test.
- Existing configs (no per-child caps) still inherit the parent set unchanged (backward compat).

## Open decisions (for autoplan)
- **OD1 — LLM-chosen child caps vs config-declared spawn profiles.** The audit's approach is
  tool-input caps (the orchestrator LLM passes them). But the orchestrator itself is exposed to
  injection (it reads the inbox agent's summary of untrusted email), so an injected orchestrator
  could spawn the curator *within the parent set* including `Credential{Google}` — the subset
  check bounds escalation but doesn't force curator to be KB-only. Stronger: **config-declared
  per-child spawn profiles** (a `[[agents.spawn_profiles]]` or template the orchestrator names,
  not free-chooses). Bigger change. Which threat model does cap.2 close — accidental over-grant
  (tool-input + subset is enough) or injected-orchestrator (needs config-declared)? Surface.
- **OD2 — subset semantics.** Reuse `satisfies` (prefix-aware: `FsRead{/data/x}` ⊆ `FsRead{/data}`,
  `KbRead{ops:briefs}` ⊆ `KbRead{ops}`, `Mcp` tool-subset)? Confirm no cap type makes the subset
  check unsound (e.g. `Net` where `satisfies` returns true unconditionally → a child could
  "request" Net the parent lacks and pass — needs explicit handling).
- **OD3 — reject vs clamp.** On an out-of-parent request: reject the whole spawn (fail-closed,
  recommended) or silently drop the offending cap and spawn with the intersection? Reject is
  safer + surfaces the misconfig; clamp is lenient.
- **OD4 — rejection event + surfacing.** New `AgentSpawnDenied` event? Does it ride the cap.1b
  ATTN channel later? (cap.1b not built yet.)

## CEO GATE DECISION (user, 2026-07-23): **Floor + honest (re-label)**

cap.2 ships the **attenuation floor**, not injection defense. Locked scope:
- **A1** `SpawnConfig.capabilities` + tool-schema `capabilities` property.
- **A2** SOUND subset validation (Net-safe, per-variant, fail-closed) — a dedicated
  `capability_covered_by`, NOT raw `satisfies`. Out-of-parent → reject spawn + `AgentSpawnDenied`.
- **A4** CoS prompt: curator = KB-only (prose names `Mcp{google_oauth}` as the boundary, per F4).
- **NEW** a test that spawns curator WITH `Mcp{google_oauth}` from a holder-orchestrator and
  asserts the spawn **succeeds** — the documented injected-orchestrator bypass (F6).
- **CLAIM:** "least-privilege attenuation floor + accidental-over-grant guard." **NOT** injection defense.
- **audit P1-10 stays OPEN.** Real closure = **cap.2b** (orchestrator de-privilege / static
  untrusted-data pipeline, per F2). File cap.2b in TODOS.
- **A3 (max_turns) CUT** → cos-polish follow-up (F5).

## Phase 1 — CEO Review (autoplan)

### CEO dual voices — consensus table
```
  Dimension                              Claude   Codex   Consensus
  ────────────────────────────────────── ──────── ─────── ─────────
  1. Right problem?                       yes      yes     CONFIRMED — all-or-nothing inheritance is a real defect
  2. Framing/claim correct?              NO(CRIT) NO(CRIT) CONFIRMED — closes accidental over-grant, NOT the injection threat it claims
  3. tool-input+subset closes threat?    NO       NO       CONFIRMED — chooser (orchestrator) is downstream of untrusted data
  4. subset via `satisfies` sound?       NO(HIGH) NO       CONFIRMED — Net returns true unconditionally → under-check
  5. A3 max_turns in scope?              NO       NO       CONFIRMED — resource limit, not a capability; split
  6. Close audit P1-10 with this?        NO(CRIT) NO       CONFIRMED — ship the floor, keep P1-10 open, or extend to close it
```

### Findings (both models)
- **F1 (CRITICAL, both) — the mechanism closes a different threat than the claim.** A1/A2
  (tool-input caps + subset) defend against an **honest orchestrator over-granting** (T1). The
  Problem section justifies cap.2 with **prompt-injection** (T3). Tool-input caps are
  *structurally incapable* of closing T3: the orchestrator chooses the child's caps and is
  itself downstream of untrusted email (`cos.agents.toml:3` reads Gmail directly; `:239-259`
  holds `{Spawn, Mcp{google_oauth}, Credential{Google}}`). An injected orchestrator spawns
  curator WITH `Mcp{google_oauth}` — still ⊆ its own set → **subset check passes, curator gets
  Gmail.** The subset check bounds a child to an over-privileged injectable root, which buys
  nothing against a compromised root.
- **F2 (HIGH, Claude — the constructive path) — the subset check only becomes load-bearing when
  the GRANTING node is itself attenuated.** cap.2 builds the primitive and applies it one level
  too low. Real closure = **de-privilege the orchestrator / keep it out of the untrusted-data
  path**: the node that ingests untrusted data holds no `Mcp{google_oauth}`/`Credential` and no
  spawn authority; a statically-wired inbox→curator pipeline carries the summary. Largely a
  `cos.agents.toml` change, not Rust. **Config-declared profiles alone are NOT sufficient**
  (both): an injected orchestrator just spawns the legitimately-Gmail-capable *inbox* profile
  and hands it the payload — profiles bound a role's caps, not which untrusted data reaches
  which role.
- **F3 (HIGH, both) — `satisfies` is unsound as a subset predicate; promote OD2 to MANDATORY.**
  `Capability::Net` returns `true` unconditionally in `satisfies` (advisory), so a child could
  "request" a `Net` grant the parent lacks and pass. A security primitive whose entire value is
  subset-soundness cannot ship with an under-check. Require a **per-variant monotonicity audit**
  (child ⊆ parent for EVERY `Capability` variant, a test per variant) + **fail-closed default**
  for any undecidable/wildcard variant (unknown → reject). Do NOT reuse raw `satisfies`; write
  an attenuation-specific `capability_covered_by(parent, child)` with a no-wildcard match.
- **F4 (MED, Claude — precision) — the load-bearing cap is `Mcp{google_oauth}`, NOT
  `Credential{Google}`.** Agent-level `Credential` is decorative (cap.1's own finding — the
  broker token comes from the *server's* Credential cap). `filtered_specs` omits the Gmail tool
  specs unless the child has `Mcp{google_oauth}` (`cos.agents.toml:251`). So withholding
  `Credential` from curator changes nothing; withholding `Mcp{google_oauth}` is the move. A4's
  "KB-only" is functionally right; the PROSE must name `Mcp{google_oauth}` as the boundary.
- **F5 (MED, both) — A3 (max_turns passthrough) is scope creep.** A resource limit, not a
  capability; rides along only because it touches the same struct. Violates one-increment-per-
  branch and dilutes the security review. Split to a cos-polish follow-up.
- **F6 (MED/HIGH, Claude) — encode the injected-orchestrator BYPASS as a test.** A test that
  spawns curator with `[Mcp{google_oauth}]` from an orchestrator that holds it and asserts the
  spawn **succeeds**, labeled the known injection gap. Makes the limitation honest + durable
  (the red test cap.2b/orchestrator-de-priv turns green); a prose-only limitation gets lost.
- **F7 (CRITICAL, both) — do NOT close audit P1-10 with the floor.** Recording the audit's #1
  item "done" when an hour-long pentest reopens it poisons the audit-closure ledger
  (audit.1 made closure claims test-enforced) and hits the mv track's governance-in-the-boundary
  credibility. Either re-label (P1-10 stays open, real closure = cap.2b) or extend scope to
  actually close it (F2).
- **F8 (settled, both) — OD3: reject, not clamp.** An out-of-parent request rejects the whole
  spawn (fail-closed, recorded), never silently drops the offending cap.

### Decision Audit Trail (CEO)
| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 1 | CEO | Shape: floor+honest vs close-it (orchestrator de-priv) | **PREMISE + USER CHALLENGE** (gate) | n/a | both models; the claim doesn't match the mechanism |
| 2 | CEO | F3 attenuation-specific subset, per-variant, fail-closed | Mechanical (correctness) | P1 completeness | raw `satisfies` is unsound (Net) |
| 3 | CEO | F5 cut A3 (max_turns) → follow-up | Mechanical (scope) | P3 pragmatic | resource limit, not a capability |
| 4 | CEO | F4 name Mcp{google_oauth} as the boundary, not Credential | Mechanical (precision) | P5 explicit | agent-level Credential is decorative |
| 5 | CEO | F6 encode the injected-orchestrator bypass as a test | Mechanical (honesty) | P1 completeness | limitation must be durable, not prose |
| 6 | CEO | F8 reject not clamp | Mechanical | P5 explicit | clamp is a silent footgun |

## Phase 2 — Eng Review (autoplan) — dual voice (Claude + Codex), CONVERGED

### E-F1 (CRITICAL, Codex) — `Mcp` is ALSO unsound under `satisfies` for a subset check.
`satisfies` (capability.rs:184) does `g_tools.is_empty() || req_tools.iter().all(|t| g_tools.contains(t))`.
When the CHILD requests `Mcp{server, tools: []}` (= all tools on the server), `req_tools.iter().all()`
is **vacuously true** → a child requesting ALL tools passes against a parent holding an explicit
tool SUBSET. Escalation hole. So `capability_covered_by` must special-case **both `Net` AND `Mcp`**,
not just `Net`. Everything else delegates to `satisfies` (confirmed sound: Fs/Kb prefix, Credential
exact, Spawn/ShellExec/RunsRead/BriefPublish unit).

### The A2 primitive (final)
```rust
// capability.rs — attenuation-specific subset check. EXHAUSTIVE match (no wildcard arm):
// a new Capability variant will not compile until its containment rule is decided (drift guard).
pub fn capability_covered_by(parent: &[Capability], child: &Capability) -> bool {
    match child {
        // Net: `satisfies` returns true unconditionally (advisory) — unsound here. Real containment.
        Capability::Net { hosts, ports } => parent.iter().any(|p| {
            if let Capability::Net { hosts: ph, ports: pp } = p {
                list_covers(ph, hosts) && list_covers(pp, ports)
            } else { false }
        }),
        // Mcp: `satisfies` is vacuously true for an empty child tool list — unsound. Real containment.
        Capability::Mcp { server, tools } => parent.iter().any(|p| {
            if let Capability::Mcp { server: ps, tools: pt } = p {
                ps == server && list_covers(pt, tools)
            } else { false }
        }),
        // All remaining variants: `satisfies` IS a correct child-⊆-parent test.
        Capability::FsRead { .. } | Capability::FsWrite { .. }
        | Capability::Spawn | Capability::KbRead { .. } | Capability::KbWrite { .. }
        | Capability::ShellExec | Capability::Credential { .. }
        | Capability::RunsRead | Capability::BriefPublish => satisfies(parent, child),
    }
}
// Shared wildcard-list containment: empty parent = wildcard (covers all incl. empty child);
// empty child = "requesting all", covered ONLY by an empty (wildcard) parent. Fail-closed.
fn list_covers<T: PartialEq>(parent: &[T], child: &[T]) -> bool {
    parent.is_empty() || (!child.is_empty() && child.iter().all(|c| parent.contains(c)))
}
```
Note `Net.hosts` is advisory at the kernel layer, but the subset check enforces host containment
anyway so the *grant* is genuinely ⊆ (honest attenuation, not just port-level).

### dispatch_spawn integration (scheduler.rs, between step 3 child_id and step 4)
- Read `&config.capabilities` (do NOT move — `config.task/priority/token_budget` used later; E-F4 borrow note).
- If `Some(requested)`: for each `req` in `requested`, `capability_covered_by(parent_effective, req)`
  where `parent_effective` = `parent_cap_set.as_deref()` — **but if `parent_cap_set` is None
  (unrestricted parent), ALL requests pass** (scoping-down from unrestricted is always valid).
  Any uncovered cap → reject: `EventKind::AgentSpawnDenied` with `{parent_id, child_id, denied: <cap>}`
  + is_error ToolResult to parent + `enqueue_or_defer` + return (mirror the depth/child_id blocks).
  On success `child_caps = Some(requested.clone())` (incl. `Some(vec![])` = deny-all).
- If `None`: `child_caps = parent_cap_set.clone()` (current inherit behavior — backward compat).
- `filtered_specs(child_caps.as_deref())` already narrows the child's tool specs (unchanged).

### E-F2 (settled) — parent None (unrestricted) semantics
Unrestricted parent + `Some(requested)` → pass, `child_caps = Some(requested)` (useful: unrestricted
parent can still scope a child down). Unrestricted parent + `None` → `child_caps = None` (inherit).

### Compile touch-points (all confirmed against source)
1. `SpawnConfig` (config.rs:426): add `#[serde(default)] pub capabilities: Option<Vec<Capability>>`;
   import `Capability`. SpawnConfig has no `deny_unknown_fields` — clean add.
2. spawn_agent schema (native.rs:925): add `capabilities` property describing the externally-tagged
   **PascalCase** shape (e.g. `{"Mcp":{"server":"...","tools":[]}}`, `"Spawn"`, `{"KbRead":{"segment":"..."}}`);
   keep `additionalProperties:false`. On malformed cap JSON the existing parse-fail path
   (agent/mod.rs:766-780) returns a clean is_error to the parent — no panic.
3. `EventKind::AgentSpawnDenied` (events.rs:9) + snake_case assertion in the serialization test
   (events.rs:218) + `=> false` arm in otel/tests/event_kind_coverage.rs:8 (else won't compile).

### E-F3 — Test matrix (unit: capability.rs; integration: scheduler.rs)
`capability_covered_by`: Net wildcard-parent covers explicit+empty child; explicit parent rejects
empty child; covers host+port subset; rejects extra host; rejects extra port; **Mcp explicit-parent
rejects child wildcard (the E-F1 hole)**; Mcp wildcard-parent covers explicit+wildcard child;
Fs/Kb narrow-ok / widen-fail; Credential exact-ok / wrong-fail; unit caps ok/missing.
`dispatch_spawn`: absent caps → inherit (backward compat); covered request → spawn ok + child
filtered_specs reflect narrowing; uncovered request → whole spawn rejected + `AgentSpawnDenied` +
is_error; unrestricted-parent + request → `Some(requested)`; unrestricted-parent + absent → `None`;
**BYPASS TEST (F6): orchestrator holding `Mcp{google_oauth, []}` spawns curator WITH it → SUCCEEDS**
(documents the injected-orchestrator floor limit; the red test cap.2b turns green).

## Phase 3 — DX Review (autoplan)
- **Tool ergonomics:** the PascalCase cap JSON is verbose for an LLM but is the SAME shape the whole
  system already models caps in (AgentConfig.capabilities in TOML) — consistency > novelty. The A4
  CoS prompt gives the orchestrator concrete per-child cap literals to copy, so the model isn't
  inventing the shape. Schema `description` shows one example of each cap kind it needs.
- **Error legibility:** `AgentSpawnDenied` records the specific denied cap; the parent's is_error
  ToolResult names it ("spawn denied: requested capability X not covered by parent") so a mis-scoped
  spawn is self-diagnosing, not a silent narrowing (F8 reject-not-clamp).
- **Docs/ledger (honesty is the DX deliverable here):** update ROADMAP cap.2 line; **file cap.2b**
  (orchestrator de-privilege / static untrusted-data pipeline — the real P1-10 closure) **and the
  A3 max_turns follow-up** in TODOS.md; AUDIT-v0.86.md §6 / P1-10 stays OPEN with a one-line pointer
  to cap.2b. `agentd check` is NOT extended (tool-input caps are runtime, not config — cap.1 boundary).

## BUILD COMPLETE (2026-07-23)
Implemented per the approved floor scope. 1532 workspace tests (+15 cap.2), clippy clean,
both cos.agents.toml lint clean via `agentd check`.
- `capability.rs`: `capability_covered_by` (Net+Mcp real containment, no-wildcard drift guard) +
  `list_covers` helper + 12 unit tests.
- `config.rs`: `SpawnConfig.capabilities: Option<Vec<Capability>>` (`#[serde(default)]`).
- `events.rs`: `EventKind::AgentSpawnDenied` + serialization assertion; `otel` coverage arm.
- `native.rs`: `capabilities` on spawn_agent schema (PascalCase shape doc).
- `scheduler.rs`: dispatch_spawn attenuation block (reject not clamp; unrestricted-parent = scope-down-ok)
  + 3 integration tests incl. the documented injected-orchestrator BYPASS test.
- `cos.agents.toml` (dev + distro): inbox + curator spawned with explicit `capabilities` (curator
  KB-only, no google_oauth); misleading "inherit full set" note replaced.
- Ledger: TODOS `cap.2b` (real P1-10 closure) + `cap.2-ar-01` (max_turns) filed; AUDIT §6 reframed,
  P1-10 marked PARTIAL/OPEN.
Next: /review → /qa → /ship (version v0.94.0, user to confirm).

## /review COMPLETE (2026-07-23) — dual-model adversarial, 4 fixes
Claude subagent verified the primitive SOUND (no new escalation path) + reject-path integration correct.
- **[CRITICAL, Codex structured P1] distro child caps not ⊆ distro parent** → daily brief would brick.
  The distro (QEMU prod) orchestrator lacks `Mcp{semantic-kb}` + `mail:raw` (no Qdrant sidecar);
  I'd copied dev child caps into it. Fixed: distro inbox = `Mcp{google_oauth}`+`Credential`; curator =
  ops KB only. Verified every child cap ⊆ distro parent (lines 159-174); `agentd check` clean.
- **[correctness, both voices] Mcp multi-entry over-denial** → `capability_covered_by` now UNIONS
  same-server parent Mcp tools (+ test). Net kept per-entry with a comment (independent host/port
  union is unsound — cross-product).
- **[docs, Codex] `Some([])` "deny-all" inaccurate** → corrected: capability-free tools (send_message,
  request_approval, memory) remain; only capability-gated tools are dropped. (config.rs + native.rs)
- **[docs, Codex] `null` == absent** → clarified the field doc (null never exceeds parent).
- Accepted/no-action: injected-orchestrator bypass (F1 — documented, test-encoded, cap.2b);
  child_seq burned on denial (cosmetic); CoS-prompt pseudo-TOML syntax (pre-existing, works in prod).
Post-fix: build + clippy clean, 1533 tests pass, both cos configs lint clean.

## FINAL GATE — APPROVED (2026-07-23)
Shape = **attenuation floor, honest** (user CEO-gate decision). Eng dual-voice converged; Codex's
Mcp-soundness catch (E-F1) folded in. No open decisions remain — all mechanical items resolved:
Net+Mcp special-cased & fail-closed; parent-None = scope-down-ok; reject-not-clamp; bypass encoded
as a test; P1-10 stays open (cap.2b filed); A3 cut. Ready to implement.

## NOT in scope
- cap.1b (runtime denial ATTN column), cap.3 (`AbsPathPrefix`).
- Config-declared spawn profiles if OD1 resolves to tool-input (would be a follow-up).
- Universal-tier spawn attenuation (children are native; universal is a different path).
