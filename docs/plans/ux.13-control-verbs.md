# ux.13 — Control verbs (Cancel, SetBudget, SetCaps)

**Increment:** ux.13 (UX cockpit reshape, increment 4 of 4). Lands after cap.1 (SetCaps
wants its capability-validation machinery) and stays consistent with cap.2's ⊆-parent check.
**Branch:** `ux.13-control-verbs`
**Design of record:** `~/.gstack/projects/0x89karan-runtime1/0x89karan-ux-control-panel-design-20260718-204837.md` (APPROVED); reshape plan-of-record `docs/plans/ux.11-trust-after-absence.md`.

## Problem (lived operator frictions this closes)

From the design doc's four frictions, ux.13 closes two:
- **#1 Runaway/stuck agent, no kill.** Today the only remedy is killing the whole
  agentd/container. There is no cancel/pause/resume anywhere in code or plan.
- **#4 Wrong config/caps, forced respawn.** Changing a running agent's budget or
  capabilities required destroying and respawning it.

The write surface today is spawn/inject/approve/deny only. ux.13 adds the "k9s verbs":
stop a runaway in two keypresses; narrow an over-broad agent's caps (misconfig repair) without
a respawn. **Scope honesty (CEO):** these are console verbs (TUI/CLI/HTTP) — they serve the
operator who is *at the console*. The trust-after-absence marquee ("cancel a 3am runaway from
your phone") is gated on ux.12 (Telegram reach); ux.13 builds the management-API substrate ux.12
will drive. Design the verb confirm path to also support an async/deferred confirm (a chat
round-trip is human-latency-bound), so a blocking `oneshot` today does not force a rework at ux.12.

## Scope (fixed by the design doc; expansions decided at the gate)

Three verbs on the existing `ControlCommand` → management-API + FUSE-control + TUI pattern:

### Cancel (the real work)
- New `ControlCommand::Cancel { agent_id, confirm_tx }` (confirm channel like SetBudget,
  not fire-and-forget like Inject — so HTTP can 404 an unknown agent).
- **v1 guarantee: no new world-affecting dispatch after Cancel** (drawn at WORLD-ACTION, not at
  "one inference call" — a "step" is inference-only; tools/spawn/send/run_job are *separate*
  futures dispatched from the just-returned inference). **Implementation (CORRECTED by Eng
  dual-voice — the earlier `Inference{Ok}`-handler placement was wrong):**
  - **Guard at the TOP of `enqueue_or_defer` (scheduler.rs:1617, before the `match effect`),**
    NOT in the `Inference{Ok}` handler. This is the single choke point every caller funnels
    through (`Inference{Ok}`, `Tools`, `Approve`/`Reject`, `Inject`, child-delivery). Checking
    only `Inference{Ok}` lets `Tools → step → Infer → enqueue_or_defer(Infer)` schedule one more
    full inference before stopping; the top-of-`enqueue_or_defer` guard funnels the `Infer` arm
    too, and gates the not-yet-dispatched `SpawnAgent`/`RunJob` effect on the same pass — which
    closes the mid-spawn cascade leak for free (see children, below). Guard:
    `if state.cancel_requested.contains(&agent_id) { handle_agent_terminal(cancelled); return; }`.
  - **Flag home: `SchedulerState.cancel_requested: HashSet<String>`** (mirror `waiting`,
    scheduler.rs:102) — NOT on `AgentTask` (that drags it into `to_checkpoint`, agent/mod.rs:220,
    so a cancel would wrongly survive restart). Cleared in `handle_agent_terminal` so a reused
    agent id can't inherit a stale flag.
  - **Running agent: set the flag ONLY — never funnel immediately.** A running agent has one
    in-flight future; funneling now removes it from `state.agents`, and when the future resolves
    `EffectResult::Inference/Tools` does `state.agents.get_mut(&id).expect(...)` → **panic**
    (violates "the loop never panics", scheduler.rs:792/827). The gate funnels it when the future
    returns (`in_flight` already decremented before that branch, scheduler.rs:751/782 — so the
    funnel does NOT break the `in_flight` assert). Honest UX copy: "cancel after current
    operation", not "instant kill".
  - **Parked agents funnel immediately AND require extra purges `handle_agent_terminal` does not
    do:** remove from `state.deferred` (else `drain_deferred` pops the stale entry → `in_flight++`
    → pushes a future for a removed agent → panic; precedent purge at scheduler.rs:1587) and from
    `state.pending_approvals` (else a dangling approval can be approved against a dead agent).
    "Parked" = id in `deferred`/`waiting`/`pending_approvals` or a parent in `awaiting` (reuse the
    status derivation at scheduler.rs:2920-2942); else it's running (flag-only).
- **Mid-stream INFERENCE abort = STRETCH, default OUT.** Dropping the live SSE future needs
  net-new per-agent `AbortHandle` machinery in the `FuturesUnordered` (scheduler.rs:1649) plus
  careful `in_flight` accounting so an aborted future doesn't trip the underflow `assert!`
  (scheduler.rs:748/779). The world-action gate makes this unnecessary for safety — it only saves
  the tail tokens of one in-flight call. In only if genuinely cheap.
- **Terminal representation — RESOLVED (D-ENG): reuse `AgentStatus::Failed` in the live snapshot;
  record the distinction in the event + run record.** A new `AgentStatus::Cancelled` variant is
  more than match-site drift — cancelled children are removed from `state.agents` so they vanish
  from the live snapshot entirely (terminal fact lives only in runs.redb), and a cancelled root's
  status derivation collapses `Err` → `Failed` (scheduler.rs:2920), so a real variant needs a side
  `cancelled` set consulted at the outcomes check to render at all. Not worth it. Instead: emit
  `AgentCancelled` and pass `status="cancelled"` (or reuse existing `"interrupted"`) to
  `RunTracker::close` (runs/mod.rs:206, free-form). runs.redb (ux.11b) is the terminal-history
  surface where cancellation is actually observed.
- **Cancel-with-live-children — RESOLVED to cascade-cancel (CEO consensus).** Rationale
  (asymmetry): orphan-and-adopt leaves zombie children burning budget + writing with no consumer
  — unbounded, unrecoverable spend violating "cognition is metered" and re-creating the runaway
  one level down ("I cancelled it but it's still spending"). Park requires pause/resume (OUT).
  Cascade bounds spend, needs no suspend machinery, and its only downside (killing a useful child)
  is *recoverable* via respawn. **Implementation:**
  - Discover the subtree by BFS over `state.parent_map` (scheduler.rs:122, child→parent,
    scheduler-authoritative) — NOT the agentctl p6.4 `TopologyGraph` (a separate-process
    flight-event read-model). Do NOT treat the `"operator"` root sentinel (scheduler.rs:2650) as
    a cancelable node.
  - Each cascaded node emits its OWN `AgentCancelled` with `cause: "cascade from <parent-id>"`
    (never a silent vanish — preserves "record everything"): parked nodes emit in the Cancel
    dispatch; running nodes emit in the `enqueue_or_defer` gate.
  - A cascaded child that is `AwaitingParent` to the (already-gone) cancelled parent routes through
    the child-delivery branch and records the expected "parent not found" log (scheduler.rs:1160,
    no panic) — this is fine; state it so it's not mistaken for a bug.
  - The "mid-spawn race" is not a Rust async race (single-threaded `select!` loop); the logical
    leak (a child in the parent's just-returned response, not yet in `parent_map`) is closed by
    the top-of-`enqueue_or_defer` gate funneling the parent's `SpawnAgent` effect before dispatch.

### SetBudget (semantics DONE; operator surface + a bug remain — NOT "near-zero")
- The *core semantics* already exist and MUST NOT be re-implemented: `ControlCommand::SetBudget`
  (control.rs:63) flows FUSE + HTTP (`POST /api/v1/budget/set`, management.rs:453), applies via
  `AgentTask::set_token_budget` (agent/mod.rs:203), emits `BudgetSet`. ux.11a shipped it. No new
  semantics (design doc + roadmap warn against double-claim).
- **But the operator surface is real work (CEO caught the undercount):** `agentctl` has NO
  `set-budget` subcommand (main.rs) and the `DataSource` trait has no budget/cancel/caps mutation
  methods (source.rs:26). ux.13 adds the CLI subcommand + DataSource methods (HttpSource +
  FuseSource) + TUI edit affordance.
- **Bug to fix (CEO+Eng, confirmed):** agentctl's generic `post_mutation` uses a 500ms client
  (source.rs:113/115) while the server confirm channel waits up to 2s (`timeout(2s, confirm_rx)`,
  management.rs:497) — so a confirm-channel verb spuriously reports failure on a *succeeded*
  mutation. This is a NEW-code trap (no set_budget/cancel/set_caps in `DataSource` yet), not an
  existing bug. **Fix:** route the three confirm-channel verbs (cancel/set-budget/set-caps)
  through a client whose timeout exceeds 2s — reuse the existing 3s `spawn_client`
  (source.rs:121, rename `confirm_client`, shared so ux.12's async round-trip inherits it); keep
  the 500ms client for fire-and-forget approve/deny only.

### SetCaps (revoke/narrow-only — misconfig-repair ergonomics, NOT a security response)
- New `ControlCommand::SetCaps { agent_id, capabilities, confirm_tx }`.
- **Honest framing (CEO, both voices):** this is friction-#4 *misconfig-repair ergonomics*
  (narrow an over-broad running agent without respawn), **NOT** a security/injection response.
  cap.2b already established that runtime cap-narrowing is not the injection defense (authority
  was moved off the injectable node via sealed jobs); narrowing takes effect only at the next
  `cap_set_cloned` boundary, so it does not stop an in-flight injected action. The Problem
  section is corrected accordingly (was "narrow a *misbehaving* agent" → implies security).
- **Revoke/narrow-only in v1.** Validation (Eng-corrected): `capability_covered_by` is
  `(parent: &[Capability], child: &Capability)` (capability.rs:245) — a slice + a *single* cap,
  not two slices. Narrow check = `new_caps.iter().all(|c| capability_covered_by(&current, c))`,
  fail-closed on any uncovered cap → reject (400 / EINVAL) "SetCaps is narrow-only; to widen,
  respawn." **Handle `current == None` (unrestricted):** `cfg.capabilities.is_none()` means
  covers-everything — accept any concrete `new` (narrowing an unrestricted agent is the *most
  common* misconfig-repair). Once `Some`, you can never widen back to `None` (that's a widen →
  rejected) — document this one-way edge. Live *grant* stays OUT (posture change; audit86-P1-10).
- **Mutator shape (Eng-corrected — the `set_capabilities(Vec<Capability>)` signature is NOT
  implementable):** `AgentTask` holds no `ToolRegistry`; the caps→specs builder is
  `registry.filtered_specs(caps)` (tools/mod.rs:126), and only the scheduler holds `registry`. So
  the `SetCaps` arm in `dispatch_control_command` computes `registry.filtered_specs(Some(&new))`
  and calls `set_capabilities(&mut self, new_caps, new_specs)` which overwrites `cfg.capabilities`,
  `specs`, AND `tool_names` together (the model tool list is `self.specs`, the snapshot reads
  `spec_names()`→`tool_names`, agent/mod.rs:154). Overwriting only `cfg.capabilities` leaves the
  model seeing tools it can no longer call.
- **Checkpoint round-trip is fine (state it):** restore recomputes `filtered_specs` from
  `cfg.capabilities` and ignores saved specs (scheduler.rs:286/301/340), so a persisted narrow
  survives restart with matching specs — provided the mutator writes `cfg.capabilities` (it does).
- Use `tier_legality(cap, CapContext::Agent)` (capability.rs:383, `Inert` for Net/ShellExec/
  Credential) to reject/warn a SetCaps targeting an agent-context-inert cap (misleading no-op).
- New runtime `CapabilitiesSet` flight event (`snake_case` → `"capabilities_set"`), data
  `{target, old: [Capability], new: [Capability]}` mirroring `BudgetSet` (events.rs:26).

### Explicitly OUT
- Pause/resume (deferred by design doc).
- Live cap *grant* / widening (posture change; audit86-P1-10).
- Remote inject (cut in design doc).
- Mid-stream SSE abort unless cheap (stretch).

## Surfaces (all three, per the existing pattern)

| Surface | Hook | Reuse |
|---|---|---|
| `ControlCommand` enum + parse | control.rs:31 (arms) + control.rs:120 `parse_control_command` (+ `TaggedCommand` control.rs:87) | confirm_tx idiom control.rs:58 |
| Management HTTP | new `match` arms in management.rs:120 `route()`; `state.control_tx` | model on SetBudget (management.rs:453 + confirm channel), NOT fire-and-forget Inject |
| FUSE `/agents/control` | **no FUSE changes** beyond `parse_control_command` — surface is verb-agnostic (agents_fs.rs write→control_dispatch, main.rs:1000) | — |
| agentctl TUI | `DataSource` trait methods (source.rs:26) + HttpSource/FuseSource impls; key dispatch (watch/mod.rs); two-keypress confirm dialog | approvals confirm-dialog pattern (mod.rs:878); **SetCaps reuses spawn.rs cap-toggle widget** (spawn.rs `toggle_cap_at`/`enabled_caps`) |
| agentctl CLI | `agentctl cancel <id>` / `set-budget` / `set-caps` subcommands → same DataSource | inject.rs pattern |
| Scheduler apply | new arms in `dispatch_control_command` | Cancel→`handle_agent_terminal` (scheduler.rs:1065); SetBudget→existing; SetCaps→new mutator |
| Flight events | events.rs enum: add `AgentCancelled`, `CapabilitiesSet` | mirror `BudgetSet` (events.rs:26) |

## Decisions

- **D-CEO — cancel-with-live-children — RESOLVED: cascade-cancel** (both CEO voices; see Cancel
  section for the asymmetry rationale + per-child causal-event rider).
- **USER CHALLENGE — sequencing (ux.13 before ux.12).** Operator explicitly chose ux.13 first
  over the recommended ux.12-first. Both CEO voices note this optimizes the cockpit for the
  *present* operator, not the absent-operator thesis, and that Cancel adds little the ux.8′
  budget guard doesn't already bound for the *absent* case. Neither says "don't build it" (the
  HTTP verbs are ux.12's substrate). Surfaced at the gate; operator's direction is the default.
- **D-ENG — Cancel terminal representation — RESOLVED: reuse `AgentStatus::Failed`** in the live
  snapshot, record cancellation via the `AgentCancelled` event + `RunTracker::close("cancelled")`
  (see Cancel section). A new enum variant needs a side `cancelled` set to render at all and buys
  almost nothing (cancelled children leave the snapshot entirely).
- **D-ENG — mid-stream inference abort — RESOLVED: default OUT** (the world-action dispatch gate
  makes it unnecessary for safety; in only if the AbortHandle change is genuinely small and
  preserves the `in_flight` invariant).
- **D-minor — verb naming — RESOLVED: keep `SetCaps`** (roadmap continuity); the narrow-only
  contract is explicit in the reject message + docs (vs renaming to `NarrowCaps`).

## Acceptance criteria

- `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` clean.
- Cancel: waiting/deferred/queued agent stops immediately; running agent stops within one
  in-flight call; `AgentCancelled` event emitted; terminal status visible on FUSE snapshot +
  TUI + runs.redb; children handled per D-CEO and recorded.
- SetBudget: verified to already conform (no re-implementation); TUI/CLI affordance present.
- SetCaps: narrow succeeds and takes effect next step; a widening request is rejected
  fail-closed (400/EINVAL) with a clear reason; `CapabilitiesSet` event emitted; an inert-cap
  target is rejected/warned.
- All three verbs reachable via management HTTP, FUSE control, and agentctl (TUI + CLI).
- Negative-control test per new guard (a widening SetCaps is denied; cancel of unknown id 404s).
- Linux-gated FUSE path: `make clippy-linux` before push. Docs updated (ROADMAP checkbox,
  MCP/DEPLOYMENT if surface docs affected) in the same PR.

## Test plan (Eng dual-voice; all network-free via the mock gateway, agent/mod.rs:1017)

**Cancel — the panic-safety cases are the point:**
- Cancel of a **running** agent (mock returns a tool_use; cancel arrives; tool+inference future
  returns) → agent terminates cleanly, **no panic**, `state.agents` consistent, `in_flight` back
  to 0 (the scheduler.rs:779 assert must not trip). *This is Finding 2.1 — the whole point.*
- Cancel of a **deferred** agent (`max_concurrent_inferences=1`, a queued `DeferredInfer`) then
  trigger `drain_deferred` → stale entry purged, no future scheduled for the removed agent.
- Cancel of an **awaiting-approval** agent → `pending_approvals` purged; no dangling approval.
- Cancel unknown id → 404 (confirm channel `Err`).
- **Cascade:** parent + child + grandchild via mock spawns; cancel parent → exactly one
  `AgentCancelled` per node with correct `cause`; a child that was in the parent's just-returned
  response is **never created** (gate funnels the `SpawnAgent` effect); `"operator"` root untouched.

**SetCaps — negative controls + the stale-surface bug:**
- Widen denied for **each** `Capability` variant (drives `all()` + `capability_covered_by` per
  variant); narrow of an **unrestricted** (`None`) agent succeeds → concrete `Some`; inert cap
  (Net/ShellExec/Credential) target rejected/warned; unknown id → 404.
- **Stale-surface:** after a narrow, `spec_names()` shrinks **and** the next
  `InferenceRequest.tools` (`self.specs`) no longer contains the removed tools.
- **Checkpoint round-trip:** SetCaps → `to_checkpoint` → `from_checkpoint` with a fresh registry
  → `spec_names` reflect the narrowed caps (proves restore-recompute honors persisted caps).

**SetBudget:** regression only (ux.11a semantics unchanged) + assert the new CLI/DataSource path
uses the ≥3s confirm client (a slow-path integration assertion; the 500ms client is a coin flip).

**Cross-surface:** the same verb via management HTTP, FUSE control write, and agentctl DataSource
produces an identical scheduler effect.

## Decision Audit Trail

| # | Phase | Decision | Class | Principle | Rationale |
|---|-------|----------|-------|-----------|-----------|
| 1 | CEO | Cancel guarantee = "no new world-action" (not "one call") | Mechanical | P1/P5 | Both voices: step is inference-only; tools/spawn/send are separate futures |
| 2 | CEO | Children → cascade-cancel + per-child causal events | Mechanical | P1 | Both voices; orphan=unbounded/unrecoverable spend, park=pause/resume debt |
| 3 | CEO | SetCaps framed as misconfig-repair, NOT security response | Mechanical | P5 | cap.2b established runtime narrowing ≠ injection defense |
| 4 | CEO | SetBudget scope corrected (CLI+DataSource+timeout, not near-zero) | Mechanical | P1 | Both voices found the operator-surface undercount |
| 5 | CEO | Confirm path must support async (ux.12 chat round-trip) | Mechanical | P2 | Avoids ux.12 rework of a blocking oneshot |
| 6 | Eng | Guard at top of enqueue_or_defer (single choke point) | Mechanical | P5 | Both voices; also closes mid-spawn cascade leak for free |
| 7 | Eng | Running agent = flag-only (never immediate funnel) | Mechanical | P1 | Both: immediate funnel → get_mut().expect() panic |
| 8 | Eng | cancel_requested: HashSet on SchedulerState (not AgentTask) | Mechanical | P5 | AgentTask would checkpoint → cancel survives restart |
| 9 | Eng | Cancel purges deferred + pending_approvals | Mechanical | P1 | Else drain_deferred panics; dangling approval |
| 10 | Eng | Cascade via state.parent_map (not p6.4 read-model) | Mechanical | P4 | parent_map is scheduler-authoritative; skip "operator" root |
| 11 | Eng | set_capabilities(new_caps, new_specs) — scheduler computes specs | Mechanical | P5 | AgentTask holds no registry; filtered_specs is scheduler-side |
| 12 | Eng | Narrow check = new.iter().all(covered_by(current, c)) + None handling | Mechanical | P5 | Correct signature; unrestricted None = covers-all |
| 13 | Eng | D-ENG terminal status = reuse Failed + event + runs "cancelled" | Mechanical | P3/P5 | New variant needs a side set to render; buys ~nothing |
| 14 | Eng | Confirm-channel verbs use ≥3s client (reuse spawn_client) | Mechanical | P1 | 500ms < 2s server confirm wait = spurious failure |
| U1 | CEO | Sequencing (ux.13 before ux.12) | USER CHALLENGE | — | Operator's call; both voices prefer reach-first (surfaced at gate) |
