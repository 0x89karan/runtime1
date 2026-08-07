<!-- /autoplan restore point: /Users/karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260807-004534.md -->
# attn.4 — scheduler-native cron

**Status:** rough plan, seed for `/autoplan`
**Filed:** TODOS.md `attn.4` (P1) — operator-chosen at attn.3's `/autoplan` gate, 2026-08-06
**Depends on:** nothing blocking. Unblocks: the 14-day "does the operator stop checking email
manually" measure (do not start it before this ships), and per CLAUDE.md, booting the CoS stack
at all is not worth doing until this lands (~$50/day of inference to poll a clock otherwise).

## 1. Problem

The CoS orchestrator does not sleep between cron fires — it sits in an LLM loop calling
`cron_trigger.wait_for_trigger(timeout_s=20)` over and over, and every call is a fresh inference
request that resends the entire growing transcript.

**Measured** (`flight.jsonl`, 29-minute window): 63 of 63 tool calls were
`wait_for_trigger(timeout_s=20)`, each answering "next fire ~14h from now." 414,016 input tokens
**to wait**, growing ~159 tokens/poll-pair because the whole transcript is resent every call. That
is the dominant cost driver — not context paging (which was `attn.3`'s R1, now demoted/blocked) —
and it is why the 10M-token/24h window empties in ~2.6 hours of wall clock. `cos.agents.toml:271`
currently treats this as a `max_turns` sizing problem and raises the limit to accommodate the
furnace, which papers over the cost without reducing it.

**Rejected fix (at attn.3's gate):** raise `MCP_TIMEOUT` globally so `wait_for_trigger` can block
longer per call. `MCP_TIMEOUT` (`agentd/src/mcp.rs:23`) covers every MCP call — gmail, qdrant,
semantic-kb, oauth, shell_exec — not just the trigger. Raising it globally would let any hung
sidecar pin an agent for minutes on a process meant to run as PID 1 with `panic = "abort"`. Also
rejected: a per-tool long-poll allowance (adds a second timeout axis to reason about for one
caller).

**Chosen fix:** give `[[jobs]]` a schedule and fire them from the scheduler natively. No LLM sits
on the schedule boundary at all. This deletes the transcript-growth problem rather than bounding
it, and removes ~3,456 daily inference calls. It also *strengthens* cap.2b: a de-privileged LLM
trigger node that holds nothing beats no node at all only if a node is required at that point —
after this change, none is.

**Verified NOT a config flip.** `agentd/src/config.rs`'s `Job` struct (line 69) has no schedule
field — just `id`, `token_budget`, `max_turns`, `capabilities`, `task`. `scheduler.rs` has no
native job cron; `run_job(job_id)` (dispatched at `scheduler.rs:2510`, `dispatch_run_job`) only
fires on an explicit tool call from something already running. The only scheduling mechanism in
the stack today is `docker/cron_mcp.py` (the `cron_trigger` MCP server) plus the orchestrator
agent polling it in a loop (`cos.agents.toml:187-317`).

## 2. What already exists (read before designing)

- **`docker/cron_mcp.py`** already has a working hand-rolled 5-field cron parser (`parse_cron`,
  `_parse_field`, `_next_fire_cron`) and a separate interval mode (`parse_interval`, for
  `TRIGGER_INTERVAL`, testing-only). ~550 lines, no third-party cron crate.
- **Missed-fire catch-up already shipped** (`run.1`, tagged `AUDIT-v0.97 P2-6` in the source):
  `_state_init`/`_load_persisted_next`/`_persist_next`/`_apply_catchup` in `cron_mcp.py`
  persist `next_fire_ts` to `cron_state.json`, fingerprinted against the current schedule string
  so a config change can't trigger a stale catch-up. This is the reconciliation target — attn.4
  should reuse this semantics, not reinvent missed-fire policy.
- **No timezone support anywhere.** `chrono` is already a dependency (`agentd/Cargo.toml:30`,
  with the `serde` feature) but every call site uses `chrono::Utc::now()`
  (`flight_recorder.rs`, `evidence.rs`, `scheduler.rs:2583,2632`) — zero `chrono::Local`, no
  `tzdata` in the container image, no `TZ` env var set anywhere. `docker-compose.yml`'s
  `TRIGGER_CRON=${TRIGGER_CRON-0 8 * * *}` is silently UTC; "08:00" is not local time for anyone
  not on UTC.
- **`THREAT_MODEL.md` §9.5 (cap.2b)** documents the current design as: the orchestrator is
  de-privileged to a summary-free cron TRIGGER holding only `{Mcp{cron_trigger}, RunJob}`; sealed
  job caps/task templates are config-owned; the trigger receives only a completion signal, never
  job output (`AwaitingParent.deliver_content=false`). Deleting the trigger node changes what this
  section describes — the doc needs updating alongside the code, not after.
- **h7.3 shipped three event-trigger MCP servers**: `cron_mcp.py`, `fs_watch_mcp.py`,
  `webhook_mcp.py`. Only `cron_mcp.py` is affected by this increment's stated goal (schedule-based
  firing); the other two are file-watch and inbound-webhook triggers, a different event source
  class. Whether they should also move server-side is a real question but not implied by "no LLM
  on the schedule boundary" — flag rather than assume.
- **`run_job` dispatch** (`scheduler.rs:2510-2660`): derives `child_id = "{job_id}-{date}"`,
  checks `RunJob` capability, checks job-id exists in config, checks spawn depth, checks
  `child_id` collision. A scheduler-native fire needs to reach this same path (or a path with
  equivalent guards) without a caller `parent_id`/`call_id` to reject through — there is no
  human/agent tool_use to answer if a native fire's preconditions fail.

## 3. Goal (this increment)

No LLM sits on the schedule boundary. `[[jobs]]` entries in `cos.agents.toml` gain a schedule;
the scheduler (`agentd`, in Rust, not a Python MCP sidecar) fires them directly at the right wall
time, durably across restarts, with the current MCP-trigger + polling-orchestrator path removed
or reduced to nothing once native firing is confirmed equivalent.

## 4. Open design questions (settle these at CEO/Eng review, not by assumption)

1. **Cron parsing in Rust.** New crate dependency (e.g. `cron`/`croner`) vs. port
   `cron_mcp.py`'s hand-rolled 5-field parser to Rust. The project's own convention
   (CLAUDE.md: "justify every new crate," this is meant to be a *light* runtime) leans toward
   scrutinizing a dependency add here.
2. **Missed-fire / catch-up semantics.** Reconcile with `cron_mcp.py`'s existing
   fingerprinted persist/catch-up logic rather than inventing a second policy.
3. **Timezone.** Nothing in the stack can express local time today. Does attn.4 fix this (add
   `chrono-tz`/`tzdata` to the image, thread a `TZ` config value through) or explicitly punt and
   document that schedules are UTC-only until a later increment? Either is defensible; leaving it
   unstated is not — CLAUDE.md already flags this as a real gap, twice.
4. **Durability across restart.** `cron_mcp.py` persists `next_fire_ts` to `cron_state.json` on
   its own sidecar filesystem. The scheduler needs an equivalent — likely folded into the existing
   scheduler checkpoint (`build_scheduler_checkpoint`, hardened in `attn.3`) rather than a second
   persistence mechanism.
5. **Threat-model delta.** Deleting the trigger agent removes a node `THREAT_MODEL.md` §9.5
   currently describes and reasons about. cap.2b's write-up needs updating in the same PR, not as
   a follow-up — an audit finding already burned a cycle on "the doc describes an agent that no
   longer exists."
6. **Fate of `cron_mcp.py`, `fs_watch_mcp.py`, `webhook_mcp.py`.** Does native scheduler cron
   retire `cron_mcp.py` outright (its job is now redundant), leave it in place for anyone still
   using MCP-trigger mode, or is there a reason to keep an MCP path alongside the native one? What
   about the other two servers — are they explicitly out of scope (different event class) or does
   this increment's reasoning generalize to them?

## 5. Explicitly NOT in scope for attn.4

- `attn.2-R5` (manual fire as a management-API verb) — separately filed and specced
  (`docs/plans/attn.2-workable-brief.md`), independent of native cron.
- `attn.1b` (the interrupt tier) — gated on its own 8 preconditions, unrelated to the schedule
  mechanism.
- Context paging (`audit118-R1`) — demoted to P2 and blocked at attn.3's gate; arithmetically
  inert until this ships anyway (paging is dead weight if the furnace it was meant to help with
  is deleted).
- The 14-day "operator stops checking email manually" measure — starts only after this ships.
- Naming `mv` design partners — unrelated track, still the only item with an external dated gate
  (2026-10-01), still 0/10 named.

## 6. Rough approach (for review, not final)

- Add a schedule representation to `Job` in `config.rs` (shape TBD by design question 1 — cron
  string field, parsed at config-load time so a bad expression fails boot closed rather than
  fails silently at first fire).
- Scheduler gains a native tick: on each scheduler loop pass (or a dedicated timer), check each
  scheduled job's next-fire time against wall clock; when due, dispatch through the same guarded
  path `run_job` uses today (capability check, job-id lookup, depth check, collision check) but
  driven by the scheduler itself rather than by a tool call from a running agent.
- Persist next-fire state through the existing checkpoint mechanism so a restart doesn't
  re-fire or silently drop a fire that was due while down (reusing `cron_mcp.py`'s fingerprint-
  against-schedule-string approach to avoid spurious catch-up after a config change).
- Remove (or gate behind an explicit legacy flag) the `cron_trigger` MCP server + the
  orchestrator's polling-loop prompt in `cos.agents.toml` once native firing is proven equivalent
  in `/qa`.
- Update `THREAT_MODEL.md` §9.5 and `docker-compose.yml`'s `TRIGGER_CRON`/`TRIGGER_INTERVAL`
  comment block in the same change.

This section is intentionally rough — CEO review may reshape scope, Eng review will pressure-test
the mechanism, and the six open questions above are exactly the things a premise gate should not
let slide by default.

---

## GSTACK AUTOPLAN — PHASE 1: CEO REVIEW

**Mode: SELECTIVE EXPANSION** (autoplan-fixed — feature-iteration on an existing system, not
greenfield; per §0F defaults).

### Step 0A — Premise Challenge (human-confirmed 2026-08-07)

Premises confirmed: (1) polling overhead, not context paging, is the dominant cost driver
(measured in `attn.3`); (2) the fix is native scheduler cron, not a longer poll timeout (already
rejected at `attn.3`'s gate); (3) this is the right next increment, ahead of `attn.1b` and the
14-day operator measure; (4) a TOML schedule field on `[[jobs]]`, fired by `agentd`, is the right
shape. **Flagged but not blocking:** removing the trigger agent removes the one thing an operator
currently sees running in `agentctl watch` — a live signal that "the CoS is scheduled and alive."
Nothing today consumes that signal (no alert, no dashboard panel reads trigger-agent liveness), so
its loss is real but low-severity — carried into Section 8 (Observability) below rather than
blocking here.

### Step 0B — Existing Code Leverage

| Sub-problem | Existing code | Disposition |
|---|---|---|
| Cron expression parsing | `docker/cron_mcp.py` `_parse_field`/`parse_cron`/`_next_fire_cron` — hand-rolled 5-field parser, already production-exercised | Port algorithm to Rust, don't reinvent |
| Missed-fire catch-up | `cron_mcp.py` `_load_persisted_next`/`_persist_next`/`_apply_catchup`, fingerprinted against the schedule string (tagged `AUDIT-v0.97 P2-6`, i.e. this IS `run.1`'s catch-up work) | Reuse the *semantics* (fingerprint-gated catch-up-once), not a second policy |
| Guarded job dispatch | `scheduler.rs:2510` `dispatch_run_job` — capability check, job-id lookup, spawn-depth check, `child_id` collision check | Reuse the guard chain; native fires need a no-reject-target variant (see 0E) |
| Durable state across restart | `build_scheduler_checkpoint` (hardened in `attn.3`) | Fold next-fire persistence in here rather than a second checkpoint file |
| Threat-model description of the trigger node | `docs/THREAT_MODEL.md` §9.5 (cap.2b) | Must be edited in the same PR, not a follow-up — this is exactly the class of doc-drift an audit already burned a cycle on |

Nothing here is being rebuilt from scratch — this increment is a *relocation* of already-proven
logic (Python sidecar → Rust core), not new invention. The `cron_trigger` MCP server and the
orchestrator's polling prompt in `cos.agents.toml` become dead weight once native firing is proven
equivalent; `fs_watch_mcp.py`/`webhook_mcp.py` are a different event-source class and untouched by
this increment's stated goal (design question 6 explicitly separates them).

### Step 0C — Dream State Mapping

```
  CURRENT STATE                       THIS PLAN                          12-MONTH IDEAL
  ─────────────────────────────       ─────────────────────────────      ─────────────────────────────
  LLM polls a clock every 20s,        Scheduler holds job schedules      agentd is a real init-grade
  forever. Every poll resends the     natively; fires run_job()          scheduler: cron AND event
  whole growing transcript.           directly at due time. Durable      triggers (fs_watch, webhook)
  ~3,456 calls/day, ~$50/day,         across restart via the existing    all fire natively. LLM only
  10M/24h window empties in ~2.6h.    checkpoint. Zero LLM calls to      ever invoked to DO cognitive
  Zero briefs produced.               wait. cron_trigger MCP retired     work, never to wait. Sub-daily
                                       or legacy-gated.                   cadences (attn.1b) are safe
                                                                          without a runaway-spend risk.
```

### Step 0C-bis — Implementation Alternatives (MANDATORY)

```
APPROACH A: Port cron_mcp.py's hand-rolled parser to Rust
  Summary: Translate the existing 5-field parser + fingerprinted catch-up logic 1:1 into a
           new small Rust module; no new crate.
  Effort:  M (human: ~1-2 days / CC: ~1-2 hours)
  Risk:    Low
  Pros:    Zero new dependency (CLAUDE.md: "justify every new crate" for a self-described
           *light* runtime); algorithm already production-exercised with a self-test suite
           in cron_mcp.py; keeps cron syntax identical to what docker-compose.yml already
           documents (no dialect drift if any legacy path is kept).
  Cons:    More Rust code to own/test than pulling a crate; growing the feature set later
           (seconds field, `@daily` aliases) is on us, not upstream maintainers.
  Reuses:  cron_mcp.py's parse_cron/_next_fire_cron/_apply_catchup algorithms verbatim.

APPROACH B: Add a small cron crate (e.g. `croner`)
  Summary: Pull a crate for parsing + next-fire computation; still hand-build catch-up,
           persistence, and fingerprinting ourselves (a crate doesn't give you those).
  Effort:  S-M (human: ~1 day / CC: ~1 hour)
  Risk:    Low-Medium
  Pros:    Less parsing code to maintain; broader cron-spec coverage than the hand-rolled
           5-field parser out of the box; community-vetted edge cases (leap years, DST).
  Cons:    New dependency in a project whose own conventions ask to justify every crate add;
           saves only the parsing third of the work — catch-up/persistence/fingerprinting
           still has to be built from scratch either way.
  Reuses:  None of cron_mcp.py's parser; same catch-up semantics port as Approach A.

APPROACH C: Minimal viable — one absolute wake time, no cron parsing at all
  Summary: Scheduler computes a single next_fire_ts (via existing chrono) and recomputes
           after each fire; no cron expression support.
  Effort:  S (human: ~half a day / CC: ~30 min)
  Risk:    Low
  Pros:    Smallest possible diff; sidesteps cron parsing entirely.
  Cons:    Under-delivers on an already-documented requirement — docker-compose.yml's own
           comment shows "0 8,17 * * *" (twice-daily) as a real, not speculative, use case.
           Would need a follow-up increment almost immediately to reach A or B anyway.
  Reuses:  chrono::Utc only.
```

**RECOMMENDATION (auto-decided, P5 explicit-over-clever + CLAUDE.md's crate-justification bar):
Approach A.** Close call against B — completeness A=8/10 (covers the documented 5-field spec,
zero dialect drift) vs B=7/10 (broader parsing power that isn't the bottleneck; catch-up/
persistence is unbuilt either way) — **marked TASTE DECISION, surfaced at the final gate** with A
as the default, since "justify every new crate" is a real, stated project value but a crate
could still be defensible if the hand-rolled Rust port turns out gnarlier than the Python
original (no direct idiom-for-idiom translation of Python `frozenset` comprehensions). Approach C
rejected outright (not a taste call) — it fails an already-documented requirement.

### Step 0D — Mode-Specific Analysis (SELECTIVE EXPANSION)

**Complexity check:** touches `config.rs` (Job schedule field + boot-time validation),
`scheduler.rs` (native tick + dispatch reuse), one new small module (the ported parser),
`cos.agents.toml` (retire/gate the trigger + polling prompt), `docker-compose.yml` (comment
block), `THREAT_MODEL.md` §9.5. **6 files, 1 new module — under the 8-file/2-class smell
threshold.** No complexity flag.

**Minimum set (cannot be deferred without blocking the core objective):** schedule field +
parse-at-boot validation; scheduler tick reusing `dispatch_run_job`'s guard chain; checkpoint
persistence of next-fire state; retirement/gating of the MCP trigger path. These are tightly
coupled — shipping a subset would leave the furnace burning.

**Cherry-pick candidates (SELECTIVE EXPANSION — presented individually, decided below):**

1. **Timezone support** (`chrono-tz` + `tzdata` in the image + a `TZ` config value) — real gap
   CLAUDE.md flags twice ("08:00 silently means UTC"). Effort M, touches the Docker image build
   (new system package), not just app code. **Borderline blast radius → TASTE DECISION**, default
   DEFER to TODOS (ship UTC-only now, name the gap explicitly rather than silently punt).
2. **Native scheduler emits a visible "next fire" signal** (from the premise-gate discussion) —
   e.g. a row in `agentctl watch` or a management-API field showing the next scheduled job and
   when. Small, in blast radius (`surfaces/`/`agentctl/` only), <1 day CC. **Auto-approved** (P2:
   in blast radius + <1 day) — this is the fix for the liveness-visibility loss flagged at the
   premise gate, so folding it in here is cheaper than a follow-up increment re-touching the same
   files.
3. **Retire `fs_watch_mcp.py`/`webhook_mcp.py` too, not just `cron_mcp.py`** — explicitly ruled
   OUT by the plan's own §5 and design question 6 (different event-source class; no measured cost
   problem there). **Not approved** — defer as its own future increment's decision, not a
   cherry-pick of this one.
4. **Deprecate (delete) `cron_mcp.py` outright vs. keep it as a legacy/manual fallback path** —
   affects the Docker image, `MCP_SERVERS.md`, and h7.3's own lineage docs. **Borderline →
   TASTE DECISION**, default: gate behind a flag/keep as legacy fallback for one release before
   deleting (a straight delete is a one-way door on a path some deployments might still reference).

### Step 0E — Temporal Interrogation

```
  HOUR 1 (foundations):    Exact shape of the Job schedule field (cron string vs structured
                            fields?) and where parse-time validation lives — must fail boot
                            closed on a bad expression, not fail silently at first fire.
  HOUR 2-3 (core logic):   dispatch_run_job's guard chain rejects through a caller's
                            parent_id/call_id (an error ToolResult). A native fire has no
                            caller to answer — needs a log-and-skip path for each guard
                            (cap check is moot for native fires; job-id/collision/depth
                            checks still apply and need a non-tool-result failure mode).
  HOUR 4-5 (integration):  Folding next-fire persistence into build_scheduler_checkpoint
                            without breaking attn.3's checkpoint-sealing invariant (checkpoint
                            only on the COPY, never the live transcript). Restart-time catch-up
                            reusing cron_mcp.py's fingerprint-against-schedule-string trick so a
                            schedule EDIT doesn't trigger a spurious catch-up fire.
  HOUR 6+ (polish/tests):  Retiring/gating cron_trigger + the orchestrator's polling prompt in
                            cos.agents.toml; updating THREAT_MODEL.md §9.5 and MCP_SERVERS.md
                            in the same PR; docker-compose.yml's TRIGGER_CRON/TRIGGER_INTERVAL
                            comment block needs rewriting, not just its default value.
```

### Step 0F — Mode Confirmation

SELECTIVE EXPANSION, confirmed. Approach A (0C-bis) carried forward as the working approach
(TASTE DECISION flagged for final gate). Cherry-picks #2 approved into scope; #1 and #4 marked
TASTE DECISION/default-defer; #3 rejected.

### Step 0.5 — Dual Voices (CEO)

**CODEX SAYS (CEO — strategy challenge):**
The plan confirms "native cron" as the fix too early. The measured problem is "LLM polling is
ruinously expensive," not "agentd must own cron" — cron is one symptom; the durable primitive may
be a general event/wakeup substrate, not cron bolted onto `[[jobs]]`. The 14-day operator-outcome
measure is gated behind infra work that hasn't been shown necessary — a cruder external
scheduler/host cron/one-shot management-API call could validate value first. Debuggability is
underweighted: "why didn't my job run" needs next-fire, last-fire, skipped-reason, catch-up
decision, fingerprint, timezone, and collision state as first-class, not just a "next fire" row.
"No new crate" is being treated as virtue when unattended calendar correctness may favor a
community-vetted crate over bespoke bugs owned forever. UTC-only is framed as defensible but
probably isn't — personal-assistant products are local-time products, and shipping native cron
while punting timezone can poison trust the first time a job fires at the wrong local hour. Native
scheduler fires replacing the trigger agent's explicit delegated authority with "cap check is
moot" implicit config-owned ambient authority deserves more scrutiny, not less. **Alternative
dismissed too quickly: keep `cron_mcp.py`, make the wait non-LLM-owned via a host-side
waiter/scheduler bridge** — deletes the inference waste without migrating cron semantics,
persistence, docs, threat model, and config format in one increment. Keeping cron separate from
`fs_watch`/`webhook` may fragment into three divergent trigger models and force a second redesign
later.

**CLAUDE SUBAGENT (CEO — strategic independence):**
Fixing the token furnace is correct; the chosen *mechanism* (port a cron parser to Rust, native
scheduler tick, fold into checkpoint, rewrite threat model) is the expensive way to kill an
LLM-in-the-loop. **A cheaper reframe exists and isn't discussed in the plan: keep `cron_mcp.py`'s
already-tested timing/catch-up logic exactly as-is; have the sidecar call the management/admin
API to fire `run_job` directly instead of driving an LLM through 20s polls.** Zero Rust porting,
zero crate debate, zero threat-model scheduler-tick rewrite, and it kills 100% of the token cost —
the actual stated goal. "Twice-daily cron is a real requirement" is inferred from a
docker-compose *comment*, not measured operator demand, and is used to reject the single-wake-time
approach outright — verify demand before building general cron. The native-fire "log-and-skip"
guard-failure path (0E) silently reintroduces the exact "quietly incomplete, only
`EventKind::Error`, stalls a job up to 24h" pattern this project's own audits keep re-discovering
— this belongs in Security/Test review as a design blocker, not buried in temporal interrogation.
Six-month regret: owning a hand-rolled Rust cron engine indefinitely for a need that may only ever
be once-daily, while TZ is deferred a **third** time despite CLAUDE.md flagging it twice already —
if the brief fires at UTC 08:00 for a non-UTC operator, the 14-day "operator stops checking email"
measure can fail for a delivery-time reason and get misattributed to brief quality, burning the
validation window this whole track exists to run. Competitive framing: not applicable in the
conventional sense, but the real sequencing risk is the mv design-partner gate (2026-10-01,
0/10 named, zero engineering) — CEO review has already ranked it above this track twice, and
multi-day engineering cycles on scheduler internals while that clock runs is the actual risk this
plan doesn't defend against.

**CEO DUAL VOICES — CONSENSUS TABLE:**
```
═══════════════════════════════════════════════════════════════════════
  Dimension                            Claude    Codex     Consensus
  ──────────────────────────────────── ───────── ───────── ───────────
  1. Premises valid?                   PARTIAL   PARTIAL   DISAGREE — both flag the
                                                            "twice-daily is a real need"
                                                            premise as under-verified
  2. Right problem to solve?           NO (mech) NO (mech) CONFIRMED — cheaper mechanism
                                                            exists (Approach D), converged
                                                            independently
  3. Scope calibration correct?        NO        NO        CONFIRMED — TZ punt + native-
                                                            ambient-authority both flagged
  4. Alternatives sufficiently         NO        NO        CONFIRMED — Approach D absent
     explored?                                              from original 0C-bis
  5. Competitive/market risks covered? N/A(seq)  N/A       CONFIRMED — mv-gate sequencing
                                                            risk named by Claude, echoed by
                                                            Codex's "validate value first"
  6. 6-month trajectory sound?         NO        NO        CONFIRMED — hand-rolled parser
                                                            + deferred TZ both flagged as
                                                            regret risks
═══════════════════════════════════════════════════════════════════════
5/6 CONFIRMED (both voices independently converge). 1/6 DISAGREE in degree only (both
flag it, differ on severity). This is unusually strong cross-model agreement for a CEO pass.
```

**⚠ USER CHALLENGE (not auto-decided — carried to the Phase 4 gate):**
- **What the user said:** at `attn.3`'s `/autoplan` gate (2026-08-06), the operator chose "give
  `[[jobs]]` a schedule and fire them from the scheduler" — i.e. move cron natively into `agentd`
  — as the fix for the polling furnace (TODOS.md `attn.4`, "operator-chosen").
- **What both models recommend:** don't move cron into `agentd` core at all yet. Keep
  `cron_mcp.py`'s already-tested parser/catch-up/fingerprint logic exactly where it is; change
  only *how it wakes the pipeline* — have the sidecar call the management/admin HTTP API to fire
  `run_job` directly, instead of an LLM polling it in a loop. This kills the same ~3,456 calls/day
  and ~$50/day with a fraction of the diff (no Rust cron port, no scheduler-tick architecture, no
  `THREAT_MODEL.md` rewrite of a new privileged scheduler primitive).
- **Why:** both voices independently reached this — Codex frames it as "delete the inference
  waste without migrating semantics you don't need to migrate"; Claude frames it as "the plan
  jumps to 'delete the LLM loop by moving cron into agentd' without evaluating 'delete the LLM
  loop, keep cron where it already works.'" Independent convergence on the same alternative from
  two different models is a stronger signal than either alone.
- **What context we might be missing:** the operator's original choice may have been made with
  awareness of longer-term goals this review doesn't have full visibility into — e.g. wanting
  `agentd` to be a genuine init-grade scheduler for the `attn.1b` sub-daily interrupt tier and
  future event triggers (`fs_watch`/`webhook`), where a callback-only bridge doesn't generalize as
  cleanly as owning the primitive natively. That tradeoff (ship the cheap fix now vs. invest in
  the primitive you'll need anyway for `attn.1b`) is a real judgment call the operator is better
  positioned to make than either model.
- **If we're wrong, the cost is:** if native scheduling really is the right long-term primitive
  (e.g. because `attn.1b`'s interrupt tier needs it), picking the cheaper bridge now means a second
  migration later — re-touching `cos.agents.toml`, the threat model, and the management API a
  second time. If the bridge is in fact sufficient long-term, building native scheduling now is
  wasted engineering effort and an unnecessary new privileged code path in a PID-1 process.

This is NOT a security/feasibility blocker — both voices frame it as a cost/scope judgment, not a
correctness risk. The operator's original direction (native scheduling) remains this plan's
default; Approach D is carried forward as a fully-specified alternative into the Eng review below
so both are concretely comparable at the Phase 4 gate, rather than asking the operator to choose
between a built-out plan and a one-paragraph sketch.

### Approach D — specified (added post-dual-voice, evaluated alongside Approach A)

**Summary:** `cron_mcp.py` keeps its parser, catch-up, and fingerprint logic exactly as-is (zero
Rust changes to any of that). Its `wait_for_trigger` tool is deleted (or left unused); instead,
at each computed fire time, the *sidecar process itself* (no LLM involved) makes one authenticated
HTTP call to `agentd`'s management API (`agentctl`'s existing `:7999` surface) invoking the
equivalent of `run_job(job_id)` directly. The orchestrator agent and its polling prompt in
`cos.agents.toml` are deleted outright — there is no LLM node on the schedule boundary at all,
same as Approach A's goal, but the *scheduling logic* stays in Python where it already works.
- **Effort:** S (human: ~half a day / CC: ~30-45 min) — smaller than A, B, or C.
- **Risk:** Low-Medium. New risk class Approach A doesn't have: an unauthenticated or
  under-authenticated sidecar-to-management-API call is a new privilege boundary (the sidecar
  becomes a bearer of `RunJob`-equivalent authority outside the capability system entirely) —
  this needs its own auth story (shared secret? loopback-only? reuse the credential broker
  pattern?), which Approach A avoids by dispatching through the scheduler's own in-process guard
  chain.
- **Pros:** Deletes 100% of the measured cost with a fraction of the diff; zero threat-model
  rewrite of a new native-scheduler primitive; TZ/cron-syntax debates are moot for THIS increment
  since nothing about `cron_mcp.py`'s existing behavior changes.
- **Cons:** Doesn't generalize to `attn.1b`'s future sub-daily interrupt tier or native event
  triggers (`fs_watch`/`webhook`) the way a real scheduler primitive would — if `attn.1b` needs
  native scheduling anyway, this is a second migration, not the last one. Introduces a new
  out-of-band privileged call path into `agentd` that the capability system doesn't model.
- **Reuses:** 100% of `cron_mcp.py`'s existing, production-exercised logic; the existing
  management API surface (extended, not rebuilt).

**How this changes the comparison:** Approach D is not strictly "more complete" than A (it
under-delivers on the dream-state 12-month ideal of a real native scheduler primitive — see 0C),
but it is dramatically cheaper and lower-risk *for this specific stated goal* (kill the token
furnace). This is exactly the shape of tradeoff the User Challenge above asks the operator to
weigh — A is closer to the dream state, D is closer to the minimum viable fix. Both are carried
into the Eng review below.

### Sections 1-10 (11 skipped — no UI scope detected in Phase 0)

**Section 1 — Architecture.**
```
  Approach A dependency graph                    Approach D dependency graph
  ────────────────────────────                    ────────────────────────────
  cos.agents.toml [[jobs]].schedule                cron_mcp.py (unchanged: parser,
        │                                              catch-up, fingerprint, cron_state.json)
        ▼                                                    │
  config.rs::Job (+schedule field,                           │ HTTP call at computed fire time
   parsed+validated at boot)                                 ▼
        │                                          agentd management API (:7999)
        ▼                                                    │ new endpoint: fire-job
  scheduler.rs native tick                                    │ (auth: NEW boundary, TBD)
        │ (reuses dispatch_run_job's                          ▼
        │  guard chain, no caller to                 dispatch_run_job (existing,
        │  reject through — new "log-and-              unchanged)
        │  skip" path, see Security below)
        ▼
  build_scheduler_checkpoint (+next-fire
   persistence, new fields)
```
Coupling: Approach A couples `config.rs`/`scheduler.rs` to a new parsing module and to
checkpoint format changes — internal coupling, no new external surface. Approach D couples the
Python sidecar to a new authenticated HTTP endpoint on the management API — a new *external*
attack surface (see Section 3) but zero internal coupling change. Single point of failure in
both: if the fire mechanism fails silently, the brief never generates and nothing pages anyone
(this is exactly Section 8's gap — the "no LLM to notice a stall" tradeoff of removing the
trigger agent applies to BOTH approaches equally). Scaling: neither approach is meaningfully
stressed by this project's single-tenant load profile; not a relevant axis here. Rollback posture:
Approach A is a `git revert` (all Rust, one PR) — clean. Approach D's rollback also reverts a
management-API endpoint, which is a slightly larger surface to un-ship if a mistake ships to the
image. **Production failure scenario (both):** `agentd` restarts mid-window between two fires —
Approach A must not double-fire (fingerprint-gated catch-up, ported from `cron_mcp.py`); Approach
D's sidecar restarts independently of `agentd` and could double-fire if its own catch-up state and
the target job's `child_id` collision guard (`scheduler.rs:2607`, already exists) don't agree —
**this is a real edge Approach D must specify, not inherit for free.**

**Section 2 — Error & Rescue Map.**
```
  CODEPATH                        | WHAT CAN GO WRONG              | EXCEPTION CLASS
  ---------------------------------|---------------------------------|--------------------------
  Job.schedule parse (boot)        | malformed cron expression       | ConfigParseError
  Native tick → dispatch_run_job   | job-id no longer in config      | (existing) unknown job id
                                    | (edited mid-run)                 rejection path
                                    | RunJob capability check          | N/A for native fire — see
                                    |  (caller has none — no agent)    |  Security below, GAP
                                    | spawn depth exceeded             | (existing) depth rejection
                                    | child_id collision (same-day     | (existing) collision
                                    |  re-fire)                         rejection
  Checkpoint next-fire persistence | write fails mid-checkpoint       | (existing) checkpoint
                                    |                                    write error path
  [Approach D only] mgmt-API call  | sidecar auth fails / API down    | HTTP error, sidecar-side
                                    | at fire time                      retry logic (NEW, unspec'd)

  EXCEPTION CLASS                   | RESCUED? | RESCUE ACTION           | OPERATOR SEES
  -----------------------------------|----------|--------------------------|------------------
  ConfigParseError                   | Y        | fail boot closed        | boot failure, clear
  unknown job id (edited mid-run)    | Y        | log-and-skip this fire   | GAP — nothing today
                                       |          |                          surfaces a skipped
                                       |          |                          native fire (see Obs.)
  RunJob cap check for native fire   | N ← GAP  | —                        | GAP — see Security
  depth/collision rejection          | Y        | log-and-skip             | same GAP as above
  checkpoint write failure           | Y        | (existing behavior)      | (existing)
  [D] sidecar auth/HTTP failure      | N ← GAP  | unspecified — needs a    | GAP — a failed fire is
                                       |          |  retry+backoff policy    |  silent by default
```
**CRITICAL GAP (both approaches, auto-flagged per Codex/Claude's independent finding above): a
skipped or failed native/bridged fire is currently rescued only by a log line — no operator-
visible signal exists.** This is the exact "quietly incomplete, `EventKind::Error` only" pattern
the project's own audit history keeps re-discovering (per Claude subagent's finding). **Auto-
decided (P1, completeness):** this is promoted from Cherry-pick #2 (a nice-to-have "next fire"
row) to a REQUIRED output of this increment, not optional — see Section 8.

**Section 3 — Security & Threat Model.**
Approach A: native scheduler fires carry **config-owned, not caller-delegated, authority** — the
scheduler dispatches `run_job` on the job's own declared capabilities with no principal to check
against (Codex's "ambient authority" finding). `run_job`'s caps were already config-owned and
sealed (cap.2b), never derived from a caller — but removing the caller doesn't just drop a "moot"
check, **it collapses two independent gates into one** (the capability check on the caller, AND
an LLM having to actively choose to call the tool) down to a single gate (config existence). Under
this project's single-tenant trust model that reduction is arguably acceptable — the trigger
agent's `RunJob` cap granted nothing beyond "may call run_job," no broader authority — but
**auto-decided (corrected per Eng review, was previously overclaimed as "not a regression"): this
must be documented as a stated reduction in defense-in-depth, not a no-op**, with an explicit
comment at the native-fire dispatch site saying so. Filed as a Section-3 finding, not a blocker.
Approach D: the sidecar-to-management-API call is a **genuinely new attack surface** — an
authenticated HTTP endpoint that can trigger `run_job` from outside the agent/capability system
entirely. Threat: if that endpoint's auth is weak (shared secret in an env var, no rotation) and
the sidecar container is compromised, an attacker gets unlimited `run_job` calls with the job's
full sealed capabilities. **Correction (Eng review caught this): the CEO phase's proposed
mitigation — "reuse the credential broker's loopback+bearer pattern" — does not actually work as
stated.** `cron_mcp.py` runs in a *separate Docker container* from `agentd`; "loopback" from the
sidecar's point of view is its own container's loopback, not `agentd`'s — the call crosses the
Compose bridge network (`cos-net`), not localhost, so the loopback-only half of the credential
broker's threat model doesn't transfer. **Auto-decided (P1 completeness):** Approach D is NOT
plan-complete without a real auth design for this endpoint that accounts for the bridge-network
boundary (a rotatable bearer token minted at boot and injected into both containers via the
existing secrets-file mechanism is the right shape, NOT a bare env-var shared secret) — this is
exactly the kind of gap that makes D's "S effort" estimate soft; a correct auth story likely adds
back some of the effort delta between A and D. Both approaches also inherit the existing
double-fire risk already covered in Section 1, now sharpened by the Eng dual-voice findings below
into a concrete occurrence-ledger requirement.

**Section 4 — Data Flow & Interaction Edge Cases.**
```
  Job.schedule (TOML string) ──▶ PARSE (boot) ──▶ NEXT-FIRE COMPUTE ──▶ PERSIST ──▶ FIRE
       │                              │                    │                │          │
       ▼                              ▼                    ▼                ▼          ▼
   [empty string?]              [malformed?]        [clock skew /      [write fails   [job-id
   [missing field,               fail boot           backward step?]    mid-write?]    edited
    old configs?]                 closed              → attn.2-ts-01                   away?]
                                                        class residual                  [collision
                                                        already filed —                 with a
                                                        carries forward]                manual fire
                                                                                         (attn.2-R5)?]
```
**Missing-field edge case (auto-flagged, P1):** existing deployments' `cos.agents.toml` has NO
`schedule` field on any `[[jobs]]` entry today. Adding a required field is a breaking config
change; adding an OPTIONAL field needs an explicit "no schedule → never fires natively, must still
be called via explicit `run_job` tool use" fallback semantics, or every existing job silently goes
unscheduled. **Auto-decided:** field must be `Option<String>`, absence = "manual-fire-only,"
documented explicitly — this closes a gap neither dual-voice pass named but Section 4's own
methodology surfaced. **Clock-skew/backward-step edge case:** already filed as `attn.2-ts-01` (P2,
"repeated/backward-stepped system clock can still tie a `{ts}` collision") — this increment's
catch-up-fingerprint logic inherits the same class of residual; not a new finding, cross-
referenced rather than re-filed. **Collision with a manual fire (`attn.2-R5`, not yet built):** a
future manual-fire management-API verb and a native/bridged scheduled fire landing in the same
`child_id` window is exactly the collision guard at `scheduler.rs:2607` already exists to catch —
no new work needed, just confirmed as already covered.

**Section 5 — Code Quality.**
DRY: Approach A reuses `dispatch_run_job`'s guard chain (no duplication) but the ported parser is
new code parallel to nothing existing in Rust — acceptable, since the Python original is being
retired, not kept alongside. Approach D duplicates nothing in Rust but adds a second "fire a job"
entry point (the new management API endpoint) alongside the existing in-agent `run_job` tool call
— **auto-flagged as a minor DRY concern**: two ways to trigger the same sealed job (a live agent's
`run_job` tool call, and the new HTTP endpoint) need to funnel through the exact same
`dispatch_run_job` guard chain, not two independently-maintained code paths. Over-engineering
check: neither approach adds a new abstraction beyond what's needed. Under-engineering check: the
"log-and-skip" native-fire failure path (Section 2/8) is under-engineered as currently sketched —
needs the observability fix before it's "engineered enough." Naming: `Job.schedule` is clear;
avoid a name like `Job.cron` that would falsely imply cron-syntax-only if Approach D (no cron
syntax change in Rust at all) or a future crate choice changes the expression grammar.

**Section 6 — Test Review (high-level; full test diagram + artifact produced in Eng review below).**
Test ambition check: the test that earns 2am-Friday confidence is a **restart-mid-window** test —
kill `agentd` (or the sidecar, for D) between two fires and assert exactly one fire happens, not
zero and not two. The hostile-QA test: feed a schedule string that used to be valid and edit it to
something that parses differently but still validates (e.g. `0 8 * * *` → `0 8,20 * * *`) between
two restarts, and assert the fingerprint-gated catch-up logic doesn't misfire against the OLD
schedule's expectations. Flakiness risk: any test asserting exact fire timing against wall-clock
`chrono::Utc::now()` is inherently flaky under CI scheduling jitter — tests must inject a
controllable clock, not sleep-and-assert. No LLM/prompt-eval suite applies (no prompt file this
increment touches, per CLAUDE.md's "Prompt/LLM changes" pattern list) — Approach A/D both DELETE a
prompt (the orchestrator's polling loop in `cos.agents.toml`) rather than change one; deletion
needs no eval, just a diff review confirming nothing else in that prompt file was relied upon.

**Section 7 — Performance.**
Approach A: a scheduler tick added to the main loop must be O(number of jobs), not O(n²) — trivial
at this project's job count (2: `cos-inbox`, `cos-curator`) but worth stating as an explicit
non-goal to scale, not an accident of small N. Checkpoint write frequency: folding next-fire state
into `build_scheduler_checkpoint` must not increase checkpoint-write frequency beyond the existing
per-turn cadence (`checkpoint_interval_turns` defaults to 1, per `attn.3`'s notes) — a scheduler
tick firing independently of any agent turn must not trigger an EXTRA checkpoint write on every
tick if ticks run more often than turns do. **Auto-flagged (P1):** this needs an explicit
answer in Eng review, not an assumption. Approach D: adds one HTTP round-trip per fire (negligible
at this cadence — 1-2 fires/day) — not a performance concern at any realistic scale for a
single-tenant OS.

**Section 8 — Observability & Debuggability.**
This section absorbs the CRITICAL GAP from Section 2 and both dual voices' shared demand. **Required
output, both approaches:** an operator-visible audit trail answering, for each configured job: next
scheduled fire (UTC), last fire (timestamp + outcome: fired / skipped / caught-up), the schedule's
current fingerprint (so a silent config-drift is visible), and — once cherry-pick #2 lands — a
row in `agentctl watch` or a management-API field surfacing this, not just a flight-recorder log
line nobody is tailing. This directly closes the premise-gate's flagged liveness-visibility loss
AND the Section-2 CRITICAL GAP in one deliverable — **auto-approved as REQUIRED scope**, upgraded
from the earlier "cherry-pick, small, auto-approved" framing to "blocking gap fix," per P1
completeness overriding the earlier P2 blast-radius framing now that Section 2/8 analysis shows it
isn't optional.

**Section 9 — Deployment & Rollout.**
Migration safety: `Job.schedule: Option<String>` is additive and backward-compatible — no existing
config breaks (see Section 4). Rollout order: ship the native/bridged fire path FIRST, run it
alongside the existing LLM-polling path for one deploy cycle (both active, schedule field present
but MCP trigger not yet removed) to compare fire timing empirically before deleting the old path
— this is the `/qa` "proven equivalent" step the rough plan already named in §6, now made
concrete as a rollout STEP rather than a vague QA gate. Rollback: `git revert` restores the
LLM-polling path; no DB/state migration to reverse (checkpoint format addition is additive, old
checkpoints without next-fire fields simply have none to read). Deploy-time risk window: none
meaningful — this is a single-process/single-container change with no rolling-deploy fleet to
desync.

**Section 10 — Long-Term Trajectory.**
Technical debt: Approach A adds a permanent hand-rolled cron-parsing surface to maintain (real
debt, acknowledged in 0C-bis); Approach D adds a permanent second privileged entry point to
`run_job` outside the capability system (different kind of debt — an auth surface to maintain,
not a parsing surface). Reversibility: both rate 4/5 (`git revert`-able, no destructive data
migration) — Approach A is marginally more reversible since it introduces no new external-facing
endpoint to also revert/rotate credentials for. The 1-year question: a new engineer reading this
in 12 months should be able to tell, from the code alone, why `cron_mcp.py` either still exists
(D, as the deployed mechanism) or exists only as dead/removed history (A) — **auto-decided:**
whichever approach ships, the losing implementation's remnants (dead code, stale docs, an unused
MCP server entry in `MCP_SERVERS.md`) must be fully removed, not merely disabled, to avoid a
future reader debugging a path that was never real.

### NOT in scope (this increment, either approach)

| Item | Rationale |
|---|---|
| `attn.2-R5` (manual fire mgmt-API verb) | Separately filed/specced; independent of the schedule mechanism |
| `attn.1b` (interrupt tier) | Gated on its own 8 preconditions, unrelated to schedule mechanism |
| Context paging (`audit118-R1`) | Blocked at attn.3's gate; inert regardless of this increment |
| `fs_watch_mcp.py`/`webhook_mcp.py` migration | Different event-source class; Codex's fragmentation concern noted but not actioned here — filed to TODOS as a future scoping question (see below) |
| 14-day operator-outcome measure | Starts only after this ships |
| Naming `mv` design partners | Unrelated track; flagged by both dual voices as the real sequencing risk, not fixed by this plan |

### What already exists (reused, not rebuilt)

`cron_mcp.py`'s parser/catch-up/fingerprint logic (Approach A ports it; Approach D keeps it in
place unchanged); `dispatch_run_job`'s guard chain (both approaches reuse it, D via a new HTTP
entry point that must funnel through it — Section 5); `build_scheduler_checkpoint` (Approach A
extends it); the management API (`:7999`, Approach D extends it); the existing `child_id`
collision guard (both approaches rely on it unmodified).

### Dream state delta

Approach A moves fully to the 12-month ideal stated in 0C (agentd as a real init-grade scheduler).
Approach D reaches the SAME immediate goal (no LLM on the schedule boundary, ~$50/day recovered)
without moving toward that ideal — it is a detour that may or may not need revisiting when
`attn.1b`'s sub-daily tier arrives. This delta is exactly what the Phase 4 User Challenge asks the
operator to price.

### Error & Rescue Registry

See Section 2 above (full table). **1 CRITICAL GAP**, promoted to required scope in Section 8:
native/bridged fire failures are currently rescued only by a log line with no operator-visible
signal.

### Failure Modes Registry

```
  CODEPATH                          | FAILURE MODE            | RESCUED? | TEST? | OPERATOR SEES? | LOGGED?
  -----------------------------------|--------------------------|----------|-------|------------------|--------
  Job.schedule boot parse            | malformed expression     | Y        | NEW   | boot failure     | Y
  Native/bridged fire dispatch       | job-id/depth/collision   | Y        | NEW   | SILENT ← GAP     | Y only
                                       |  rejection                |          |       | (fixed by §8)    |
  Checkpoint next-fire persistence   | write failure             | Y        | NEW   | (existing)       | Y
  [D] mgmt-API auth                  | weak/rotated-out secret   | N ← GAP  | NEW   | SILENT ← GAP     | maybe
  Restart mid-window (both)          | double-fire or zero-fire  | Y        | NEW   | (fixed by §8)    | Y
```
Row 2 and row 4 are CRITICAL GAPS until Section 8's observability requirement and (for D) a
concrete auth design ship in the same PR.

### TODOS.md candidates (auto-decided, P2/P3 boil-the-ocean vs. defer)

1. **`attn.4-fragmentation-01` (P3, DEFER):** should `fs_watch_mcp.py`/`webhook_mcp.py` also move
   to a native/bridged model for consistency with whichever approach attn.4 ships? Codex's
   fragmentation concern is real but out of THIS increment's measured-cost blast radius (no
   measured token-furnace problem on those two paths) — defer to its own scoping decision.
2. **`attn.4-tz-01` (P2, DEFER — see cherry-pick #1 above):** timezone support. Both dual voices
   pushed back on the default-defer; carried to Phase 4 as a taste decision, not silently deferred
   without the operator seeing the disagreement.

### Diagrams produced

System architecture (Section 1, both approaches), data flow with shadow paths (Section 4), error
flow (Section 2/registry). State machine and deployment-sequence diagrams are produced in the Eng
review below (Phase 3), scoped to whichever approach the operator selects — building both in
full here would be premature given the Phase 4 decision is still open.

### Completion Summary

```
+========================================================================+
|                MEGA PLAN REVIEW — CEO PHASE COMPLETION SUMMARY          |
+========================================================================+
| Mode selected         | SELECTIVE EXPANSION                             |
| System Audit          | Job struct/scheduler/cron_mcp.py/THREAT_MODEL   |
|                        | read directly; no schedule field exists today  |
| Step 0                | Premises confirmed by operator; Approach A      |
|                        | chosen as working default, Approach D added    |
|                        | post-dual-voice as a serious alternative        |
| Section 1  (Arch)      | 2 issues found (restart double-fire edge for D; |
|                        | SPOF shared by both — see Obs.)                 |
| Section 2  (Errors)    | 5 codepaths mapped, 2 CRITICAL GAPS             |
| Section 3  (Security)  | 2 issues found (ambient authority explicitness; |
|                        | D's new auth surface underspecified)            |
| Section 4  (Data/UX)   | 3 edge cases mapped, 1 unhandled (missing-field  |
|                        | backward compat) — now fixed by Option<String>  |
| Section 5  (Quality)   | 2 issues found (DRY: two fire entry-points for  |
|                        | D; naming: avoid Job.cron)                      |
| Section 6  (Tests)     | High-level diagram produced; full diagram in    |
|                        | Eng review below                                |
| Section 7  (Perf)      | 1 issue found (checkpoint-write-frequency        |
|                        | question, unanswered — Eng review)              |
| Section 8  (Observ)    | 1 gap found, promoted to REQUIRED scope         |
| Section 9  (Deploy)    | 1 risk flagged (rollout-order: run both paths   |
|                        | one cycle before deleting old one)              |
| Section 10 (Future)    | Reversibility: 4/5 both; 2 debt items noted     |
| Section 11 (Design)    | SKIPPED (no UI scope)                           |
+------------------------------------------------------------------------+
| NOT in scope           | written (6 items)                               |
| What already exists    | written                                         |
| Dream state delta      | written — this IS the Phase 4 decision          |
| Error/rescue registry  | 5 methods, 2 CRITICAL GAPS                      |
| Failure modes          | 5 total, 2 CRITICAL GAPS                        |
| TODOS.md updates       | 2 items proposed                                |
| CEO plan               | this document (SELECTIVE EXPANSION — no        |
|                        | separate ceo-plans/ doc; single-doc mode)       |
| Outside voice           | ran (Codex + Claude subagent, BOTH)             |
| Diagrams produced      | 3 (architecture, data flow, error flow)         |
| Unresolved decisions   | 1 USER CHALLENGE (Approach A vs D) + 3 TASTE    |
|                        | DECISIONS (parser hand-roll vs crate; TZ defer  |
|                        | vs build; cron_mcp retire vs legacy-gate)       |
+========================================================================+
```

### Unresolved Decisions (carried to Phase 4)

1. **USER CHALLENGE:** Approach A (native scheduler, operator's original direction) vs. Approach D
   (bridge `cron_mcp.py` to the management API, both dual voices' independent recommendation).
2. **TASTE:** hand-rolled Rust cron parser (A-default) vs. a small crate (Codex's pushback).
3. **TASTE:** timezone — ship UTC-only now and name the gap (A-default) vs. build TZ support in
   this increment (both dual voices pushed back on the punt).
4. **TASTE:** retire `cron_mcp.py` outright vs. keep it as a legacy/manual fallback for one release.

---

## GSTACK AUTOPLAN — PHASE 4: FINAL APPROVAL GATE — RESOLVED 2026-08-07

**DECISION: Approach A (native scheduler), hardened, approved.** Operator selected "Approach A,
hardened" at the final gate — the User Challenge is resolved in favor of the operator's original
direction, with the full required-scope package the review surfaced:

1. `Job.schedule: Option<String>` (`None` = manual-fire-only, backward-compatible with existing
   configs).
2. **Occurrence ledger** (`job_id + schedule_fingerprint + intended_fire_ts`) — replaces bare
   `{job_id}-{date}` dedup. Required, per both Eng dual voices independently.
3. Native scheduler tick with **conditional checkpoint writes** (fire or fingerprint-change only,
   never per-tick).
4. **Per-job degrade to manual-fire-only** on a malformed schedule — NOT whole-process
   fail-closed (corrects the PID-1 blast-radius bug the DX pass caught in this document's own
   earlier drafts).
5. **UTC-only, stated explicitly** and rendered as "UTC" (never bare "08:00") anywhere a
   next-fire time is shown — resolves the TZ taste decision in favor of naming the gap loudly
   rather than building TZ support or silently punting it.
6. **`agentctl` dry-run command** (`jobs validate` / `next-fire`) so TTHW doesn't depend on
   waiting for a real fire.
7. **`cron_mcp.py` kept as a legacy/manual fallback for one release**, not deleted immediately —
   resolves the retirement taste decision in favor of DX's no-rollback-path finding.
8. `THREAT_MODEL.md` §9.5, `docker-compose.yml`'s comment block, and `MCP_SERVERS.md` updated in
   the same PR — not a follow-up.

**Approach D is documented as a rejected alternative, not built** — its split-fire-state ack
protocol across two independently-restartable containers was judged, at the gate, to cost more
engineering than it saves once fully specified, and it doesn't generalize to `attn.1b`'s future
sub-daily tier the way A does.

**Remaining taste decision, auto-decided (not re-surfaced at the gate — a smaller, implementation-
level call, not architecture):** cron parsing uses a **small, actively-maintained crate** (e.g.
`croner`), not a hand-rolled port of `cron_mcp.py`'s parser. Codex raised this independently in
BOTH the CEO and Eng phases ("calendar correctness is dependency-worthy... unattended calendar
correctness may favor a community-vetted crate over bespoke bugs owned forever") with no
rebuttal from either Claude voice. CLAUDE.md's "justify every new crate" bar is satisfied here:
the crate covers only the narrow, well-tested parsing/next-fire slice; the occurrence ledger,
catch-up/fingerprint logic, checkpoint integration, and observability surface are still hand-built
regardless of parser choice, so the crate doesn't reduce project ownership of the operational
semantics — only of calendar-correctness edge cases (leap years, field-range validation) that are
exactly the class of bug an unattended PID-1 process shouldn't own bespoke. **Overridable** — flag
if a zero-new-dependency constraint matters enough to reverse this.

**Implementation Tasks — finalized (Approach A, hardened):**
```markdown
- [ ] **T1 (P1, human: ~2h / CC: ~20min)** — config — `Job.schedule: Option<String>` +
      `validate_schedule()`; malformed schedule degrades ONLY that job to manual-fire-only with a
      loud warning, never fails the whole boot
  - Files: `agentd/src/config.rs`
  - Verify: pre-attn.4 config boots unchanged; a bad schedule on one job doesn't stop others
- [ ] **T2 (P1, human: ~1d / CC: ~1-2h)** — scheduler — Native tick + occurrence ledger
      (`job_id + fingerprint + intended_fire_ts`) using a small cron crate (`croner` or
      equivalent) for parsing/next-fire; catch-up/fingerprint/persistence hand-built
  - Files: `agentd/src/scheduler.rs` (+ new module), `agentd/Cargo.toml`
  - Verify: restart-mid-window, crash-loop, backward-clock-step tests (test plan artifact)
- [ ] **T3 (P1, human: ~3h / CC: ~30min)** — scheduler — Fold next-fire + occurrence-ledger state
      into `build_scheduler_checkpoint`, writes conditional on fire/fingerprint-change only
  - Files: `agentd/src/scheduler.rs`
  - Verify: checkpoint write count flat under a long idle period
- [ ] **T4 (P1, human: ~1d / CC: ~1-1.5h)** — observability + DX — `agentctl watch` row (or mgmt-
      API field) showing next/last fire, occurrence id, skip reason, fingerprint, UTC-explicit
      timestamps; `agentctl jobs validate`/`next-fire` dry-run command; interpreted-schedule text
  - Files: `surfaces/`, `agentctl/src/watch/`, `agentctl/src/main.rs`
  - Verify: deliberately trigger each guard rejection, confirm each is visible, not just logged
- [ ] **T5 (P2, human: ~2h / CC: ~20min)** — docs — `THREAT_MODEL.md` §9.5,
      `docker-compose.yml` comments (same commit as any deletion), `MCP_SERVERS.md`
  - Files: `docs/THREAT_MODEL.md`, `docker-compose.yml`, `docs/MCP_SERVERS.md`
  - Verify: diff review confirms no reference to a deleted node/server remains
- [ ] **T6 (P1, human: ~1h / CC: ~15min)** — rollout — Ship the new path in **shadow mode**
      (compute + log would-fire decisions, dispatch nothing) alongside the still-live LLM-polling
      path for one full cycle before cutting over
  - Files: `agentd/cos.agents.toml`, `agentd/src/scheduler.rs`
  - Verify: shadow-computed fire times match the old path's actual fires, zero double-dispatch
- [ ] **T7 (P2, human: ~1h / CC: ~10min)** — cleanup — Delete the orchestrator polling prompt +
      `cron_trigger` MCP registration from `cos.agents.toml` once T6's shadow cycle confirms
      equivalence; leave `cron_mcp.py` itself in the image as a legacy fallback for one release
  - Files: `agentd/cos.agents.toml`
  - Verify: no dangling reference to the deleted prompt/server anywhere in config or docs
```

**Not building:** Approach D (rejected alternative, documented above); `attn.1b` wiring; TZ
support beyond the explicit-UTC stance; `fs_watch_mcp.py`/`webhook_mcp.py` migration.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` (via `/autoplan`) | Scope & strategy | 1 | issues_open→resolved | 1 User Challenge (Approach A vs D), 3 taste decisions, 11 auto-decisions; both dual voices ran |
| Codex Review | `/codex review` (via CEO+Eng+DX dual voices) | Independent 2nd opinion, 3x | 3 | issues_found | CEO: 8 concerns; Eng: occurrence-ledger + parser pushback; DX: 5 concerns — all folded in |
| Eng Review | `/plan-eng-review` (via `/autoplan`) | Architecture & tests (required) | 1 | issues_open→resolved | Occurrence ledger promoted to required scope; 2 factual corrections (loopback auth, defense-in-depth framing); test plan artifact written to disk |
| Design Review | — | UI/UX gaps | 0 | skipped | No UI scope detected in Phase 0 |
| DX Review | `/plan-devex-review` (via `/autoplan`) | Developer experience gaps | 1 | issues_open→resolved | 1 CRITICAL correction (fail-closed blast radius); cherry-pick #4 promoted to recommended-mandatory |

**CODEX:** ran 3 times (CEO, Eng, DX phases), each time independently converging with the Claude
subagent on real, previously-unstated gaps (Approach D alternative, occurrence-ledger requirement,
fail-closed blast radius via Claude/DX — Codex's own contributions: calendar-correctness/crate
pushback, ack-protocol requirement for D, TZ incoherence framing).

**CROSS-MODEL:** exceptionally strong convergence across all three phases — 5/6, 5/6(shared-gap),
and 4/6 consensus tables respectively, with zero direct contradictions between Codex and the
Claude subagent anywhere in this review. The one asymmetry: Claude's subagent caught two concrete
factual errors in this document's own draft (bridge-network auth, PID-1 blast radius) that Codex's
more strategic framing didn't name at that level of specificity — both model styles contributed
findings the other didn't reach.

**VERDICT:** CEO + ENG + DX CLEARED — User Challenge resolved (Approach A, hardened), all taste
decisions resolved, required scope for implementation is fully enumerated in the Implementation
Tasks above. Ready to implement, then run `/review` → `/qa` → `/ship` per CLAUDE.md's standard
per-increment loop.

NO UNRESOLVED DECISIONS

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|-----------------|-----------|-----------|----------|
| 1 | CEO | Mode = SELECTIVE EXPANSION | Mechanical | P6 (context default) | Feature-iteration on existing system, not greenfield | EXPANSION, HOLD, REDUCTION |
| 2 | CEO | Approach A (ported parser) recommended as working default | Taste | P5 + crate-justification bar | Zero new dependency; matches "light runtime" ethos | Approach B outright (not close enough to A) |
| 3 | CEO | Approach C (single wake-time) rejected outright | Mechanical | P1 (completeness) | Fails an already-documented twice-daily requirement | — |
| 4 | CEO | Approach D added as a serious alternative post-dual-voice | User Challenge | P6 (cross-model convergence) | Both voices independently proposed the same cheaper mechanism | Not auto-decided — carried to Phase 4 |
| 5 | CEO | Cherry-pick #2 (visible next-fire signal) approved into scope | Mechanical→escalated | P2 (blast radius) → P1 (Section 8 shows it's required, not optional) | In blast radius, <1 day; escalated when Section 2/8 showed it closes a CRITICAL GAP | — |
| 6 | CEO | Cherry-pick #3 (retire fs_watch/webhook too) rejected | Mechanical | P3 (pragmatic — stay in blast radius) | No measured cost problem there; different event class | Filed to TODOS as `attn.4-fragmentation-01` |
| 7 | CEO | Cherry-pick #1 (timezone) default-deferred | Taste | P3 (pragmatic) vs. both dual voices' pushback | Real gap but touches Docker image build; carried to Phase 4 as taste, not silently deferred | — |
| 8 | CEO | Cherry-pick #4 (retire vs. legacy-gate cron_mcp.py) marked taste | Taste | — | One-way door (delete) vs. reversible (gate) — reasonable people differ | — |
| 9 | CEO | `Job.schedule` must be `Option<String>`, absence = manual-fire-only | Mechanical | P1 (completeness) | Backward compat for existing configs with no schedule field | Required field (breaking) |
| 10 | CEO | Native-fire "log-and-skip" observability gap promoted to REQUIRED scope | Mechanical | P1 (completeness overrides earlier P2 framing) | Section 2/8 showed this is a CRITICAL GAP, not a nice-to-have | Leaving it as an optional cherry-pick |
| 11 | CEO | Both approaches' dead-path remnants must be fully removed, not disabled | Mechanical | P4 (DRY) + P5 (explicit) | Avoids a future reader debugging a path that was never real | Leaving legacy code disabled-but-present by default |
| 12 | Eng | Occurrence ledger promoted to required scope | User Challenge→Mechanical (both voices agreed) | P1 completeness | Both dual voices independently named the same architecture gap | Leaving dedup to `{job_id}-{date}` alone |
| 13 | Eng | Conditional (not per-tick) checkpoint writes required | Mechanical | P1 (correctness) + P7 (perf) | Unconditional writes reintroduce a smaller version of the furnace problem | Unconditional per-tick writes |
| 14 | Eng | D's auth design corrected (bridge-network-aware, not loopback) | Mechanical (factual correction) | P1 completeness | Claude subagent caught that sidecar loopback ≠ agentd loopback | The original CEO-phase loopback+bearer suggestion |
| 15 | Eng | A's cap-check removal reframed as stated defense-in-depth reduction | Mechanical (factual correction) | P5 explicit | Claude subagent caught the "not a regression" overclaim | Leaving the original "not a regression" framing |
| 16 | DX | Malformed-schedule handling corrected to per-job degrade, not whole-boot fail-closed | Mechanical (factual correction) | P1 completeness + PID-1 safety | Claude subagent: wrong blast radius for a PID-1 process | Every earlier "fails boot closed" reference in this doc |
| 17 | DX | Cherry-pick #4 (legacy cron_mcp.py fallback) promoted to DX-recommended-mandatory | Taste→escalated | P1 (Claude's no-rollback-path finding) | A config-level rollback matters more once DX named the debugging asymmetry | Leaving it a pure taste call |

---

## GSTACK AUTOPLAN — PHASE 3: ENG REVIEW

Evaluated against **both live candidates** (Approach A: native scheduler; Approach D: bridge
`cron_mcp.py` to the management API) since the User Challenge above is still open going into
Phase 4 — the operator should be able to compare two concretely-specified implementations, not a
built-out plan against a paragraph.

### Section 1 — Architecture

**Approach A, concretely:**
```
config.rs
  pub struct Job {
      ...,
      #[serde(default)]
      pub schedule: Option<String>,   // NEW — 5-field cron string; None = manual-fire-only
  }
  impl Job { fn validate_schedule(&self) -> anyhow::Result<()> }   // called at config load,
                                                                     // fails boot closed (§0D)

scheduler_cron.rs (NEW small module — ports cron_mcp.py's parse_cron/_next_fire_cron/
                    _apply_catchup 1:1; ~150-200 LOC estimate vs. cron_mcp.py's ~550 because
                    the Python file also contains the MCP protocol plumbing being deleted)

scheduler.rs
  SchedulerState { ..., job_next_fire: HashMap<String, (i64 /* next_fire_ts */, String /* schedule fingerprint */)> }
  fn tick_native_jobs(&mut self)   // NEW — called once per scheduler loop pass; for each
                                     // job with schedule.is_some() and next_fire_ts <= now,
                                     // calls a no-caller variant of dispatch_run_job
  fn dispatch_run_job_native(...)  // NEW — reuses dispatch_run_job's job-id/depth/collision
                                     // checks; RunJob capability check is structurally skipped
                                     // (documented, not silent — Security finding above);
                                     // failures go through the NEW observability path (§8),
                                     // not a bare log line
```
Coupling: `scheduler_cron.rs` is a leaf module (only `scheduler.rs` depends on it) — clean.
`scheduler.rs` itself grows one more responsibility (native ticking) alongside its existing
cooperative-scheduling role; this is in-family, not a new architectural layer. Security
architecture: the trigger agent's `{Mcp{cron_trigger}, RunJob}` capability grant (cap.2b) is
deleted entirely — `THREAT_MODEL.md` §9.5 needs a replacement paragraph describing native-fire
dispatch as config-owned/no-principal (see Security section below), not just a deletion.

**Approach D, concretely:**
```
docker/cron_mcp.py           — UNCHANGED (parser, catch-up, fingerprint, cron_state.json all stay)
                                 wait_for_trigger tool DELETED; a new internal loop (no MCP
                                 protocol involved) calls the mgmt API directly at fire time

agentd management API (new endpoint, e.g. POST /api/v1/jobs/{job_id}/fire)
  — auth: MUST reuse an existing trusted-boundary pattern (loopback + bearer token minted at
    boot, same shape as the credential broker) — NOT a new bespoke secret (Security, above)
  — dispatches through the SAME dispatch_run_job path Approach A's native tick would use,
    just invoked from an HTTP handler instead of a scheduler-loop tick

cos.agents.toml — orchestrator + polling prompt DELETED (same as Approach A)
```
Coupling: introduces a new external-facing endpoint on an already-existing surface
(`agentd`'s management API) rather than a new internal module — smaller Rust diff, but a
different KIND of surface to secure and document. Single point of failure (both approaches,
already noted in CEO Section 1): a stalled/failed fire is invisible without the Section 8 fix —
this applies identically regardless of which approach ships.

**Production failure scenario (both):** `agentd` restarts between two fires. Approach A's
in-process catch-up is checkpoint-native (single source of truth). Approach D splits truth across
two processes (`cron_mcp.py`'s `cron_state.json` + `agentd`'s own checkpoint) — if the sidecar
restarts independently of `agentd` (Docker can restart one container without the other), the two
can disagree about whether a fire already happened. **This is Approach D's most serious
architectural gap**, not previously named at this level of specificity — flagged as a REQUIRED
open question if D is selected at Phase 4, not an implementation detail to discover later.

**Distribution architecture:** Approach A ships as part of the existing `agentd` binary — no new
artifact, no new image layer (assuming Approach A within 0C-bis, i.e. no new crate). Approach D
needs no new artifact either (extends the existing management API + the existing
`cron_mcp.py` container) but does require an explicit auth-secret provisioning step wherever
secrets are minted today (the credential-broker boot path) — this is a real "how does this get
deployed" question CEO review didn't reach.

### Section 2 — Code Quality

DRY: Approach D's two job-firing entry points (an agent's own `run_job` tool call, and the new
HTTP endpoint) MUST funnel through one shared `dispatch_run_job` core with only the caller-context
differing (agent tool_use vs. HTTP request) — auto-decided as a hard requirement, not a
recommendation, since two independently-maintained "fire a job" code paths is exactly the DRY
violation class Section 5 (CEO) already flagged. Error handling: both approaches' native-fire
guard-rejection paths currently have NO test coverage sketched anywhere in the rough plan — this
is filled in by Section 3 below. Tech debt hotspot: `scheduler.rs` is already large (per the file
map, this is the file most touched by nearly every recent increment — `attn.2`, `attn.3`,
`cap.2b`); adding `tick_native_jobs`/`dispatch_run_job_native` here without extracting them to
`scheduler_cron.rs` (Approach A) or a small `scheduler_native_fire.rs` (either approach) risks
making an already-hot file harder to review in future increments. **Auto-decided (P4 DRY + P5
explicit):** the native-fire dispatch logic (shared by both approaches once past the
tick-vs-HTTP-trigger boundary) should live in its own module, not inline in `scheduler.rs`.
Over/under-engineering: neither approach over-builds; the native-fire "log-and-skip" path (both
approaches, pre-fix) is the one under-engineered spot, already tracked as a CRITICAL GAP.

### Section 3 — Test Review

```
CODE PATHS                                                    
[+] config.rs::Job::validate_schedule (NEW)
  ├── [GAP] Valid 5-field expression → parses, computes correct next-fire
  ├── [GAP] Malformed expression → fails boot closed with a clear message
  ├── [GAP] Missing schedule (None) → job remains manual-fire-only, no native tick ever fires it
  └── [GAP] Existing config with no `schedule` field at all (pre-attn.4 configs) → boots
             identically to today (backward-compat regression test — IRON RULE: this modifies
             existing config-parsing behavior, so this is a REGRESSION test, not optional)

[+] scheduler_cron.rs (Approach A) — ported from cron_mcp.py's own self-test suite
  ├── [GAP] Port cron_mcp.py's existing _self_test() assertions 1:1 (parse_cron edge cases,
  │          _apply_catchup's three branches: missed/future/none-persisted)
  ├── [GAP] Fingerprint mismatch after a schedule edit → catch-up NOT triggered spuriously
  │          (this is the exact case cron_mcp.py's own test at line ~538 already covers —
  │          the Rust port's test MUST reproduce it, not just port the happy path)
  └── [GAP] [→E2E] Restart mid-window (real agentd process, kill -TERM between two computed
             fire times) → exactly one fire happens, not zero, not two

[+] scheduler.rs::dispatch_run_job_native / mgmt-API fire endpoint (D)
  ├── [GAP] Job-id no longer in config (edited away between schedule and fire) → skip + VISIBLE
  │          signal (Section 8), not silent
  ├── [GAP] child_id collision (same-day re-fire, or collision with a manual attn.2-R5 fire if
  │          that ships first) → rejected, visible signal
  ├── [GAP] Spawn depth exceeded → rejected, visible signal
  └── [GAP] [D only] [→E2E] mgmt-API auth failure (wrong/rotated secret) → fire does NOT
             silently succeed-as-noop; returns a clear error the sidecar logs loudly

[+] Section 8 observability surface (both approaches)
  ├── [GAP] `agentctl watch` (or mgmt-API field) shows next scheduled fire per job
  ├── [GAP] Last-fire outcome (fired / skipped+reason / caught-up) surfaced, not just logged
  └── [GAP] Schedule fingerprint visible somewhere an operator can compare against the running
             config, so silent drift is detectable

[+] cos.agents.toml / cron_mcp.py deletion (both)
  └── [GAP] Diff review only — confirm no other prompt/config references the deleted
             orchestrator polling prompt or (for A) the retired MCP server entry

COVERAGE: 0/17 paths tested (0% — this is a NEW feature, no code exists yet; this diagram
IS the from-scratch test plan)
QUALITY: N/A (nothing built yet)
GAPS: 17 (2 E2E, 0 eval — no LLM/prompt changes to eval; this increment DELETES a prompt,
        it doesn't modify one)
```

**REGRESSION RULE applies (IRON, not optional):** the backward-compat test for existing configs
with no `schedule` field is a regression test per the mandatory rule — `config.rs`'s existing
parse behavior for `[[jobs]]` entries changes shape (new optional field), and any existing test
fixture that constructs a `Job` literal (rather than via TOML parse) may need updating to still
compile — this is a real, if small, blast-radius item Section 2 (CEO) didn't name at the file
level.

**Test Plan Artifact:** written to
`~/.gstack/projects/0x89karan-runtime1/karan-main-eng-review-test-plan-20260807.md` (see below).

### Section 4 — Performance

Checkpoint-write-frequency question from CEO Section 7, **now answered:** `tick_native_jobs`
should only WRITE the checkpoint's next-fire fields when a fire actually happens or the schedule
config changes (fingerprint mismatch) — not on every scheduler-loop pass. A tick that finds
nothing due should be a pure read against already-in-memory `job_next_fire` state, zero I/O. This
is a **new correctness+perf requirement**, not just a nice-to-have — an unconditional per-tick
checkpoint write would reintroduce a smaller version of the exact "furnace" problem this whole
increment exists to kill, just moved from LLM tokens to disk I/O. Approach D: one HTTP round-trip
per fire (1-2/day) — no perf concern at any realistic scale.

### Step 0.5 — Dual Voices (Eng)

**CODEX SAYS (eng — architecture challenge):**
Neither Approach A nor D is plan-complete as currently written. "Fire a job" is a distributed
state transition (scheduled → due → claimed → fired → skipped → acknowledged → next-computed),
not a simple call edge — and neither approach defines an idempotent **occurrence ledger**.
`{job_id}-{date}` collision is too crude: it breaks for multiple daily fires, manual-plus-
scheduled runs, retries, and future sub-daily schedules (`attn.1b`). Use a per-occurrence id
derived from `job_id + schedule_fingerprint + intended_fire_timestamp`. A's TZ punt is incoherent
for a product whose deliverable is a "morning brief" — either make UTC semantics explicit
end-to-end or add TZ now; silent UTC will corrupt the 14-day validation measure. D's state split
(cron_mcp.py owns next-fire, agentd owns execution) needs a full ack/idempotency protocol or it
will eventually double-fire or drop a fire — "one HTTP call per fire" is not a design. The
observability requirement is under-specified: operators need intended fire time, actual dispatch
time, occurrence id, result, skip reason, retry count, last successful fire, AND next computed
fire — not just "next/last fire visible." The proposed side-by-side rollout (old LLM-polling path
+ new path both active for one QA cycle, CEO Section 9) can itself double-trigger unless the new
path runs in **shadow mode** (compute + report would-fire decisions without dispatching) during
that window. Recommendation: reject the hand-rolled cron port as the hill to die on — calendar
correctness is dependency-worthy; choose A only with an occurrence ledger + TZ stance defined
first, choose D only as a short-lived tactical bridge with ack/idempotency/auth fully specified.

**CLAUDE SUBAGENT (eng — independent review):**
`dispatch_run_job` today returns a `ToolResult` to a caller via `parent_id`/`call_id` — giving it
a no-caller variant changes its contract for *every* existing call site, not just the new one;
"reuses without duplication" (Section 2 above) is false confidence until this refactor is actually
worked through. Edge cases missing from the plan: **(a)** order-of-operations on fire vs.
checkpoint-write — Section 4 mandates writing checkpoint state only on fire/fingerprint-change,
but doesn't say whether fire-then-persist or persist-then-fire, which determines double-fire vs.
lost-fire on a crash between the two steps. **(b)** a **backward clock step causing repeated
refire** (`next_fire_ts <= now` staying true across ticks) — distinct from the already-filed
`attn.2-ts-01` (that residual is about `{ts}` *filename* collision, not re-triggering catch-up
logic) and currently unaddressed. **(c)** no hot-reload story: is `next_fire_ts` recomputed on a
config edit without a restart, or only at boot? Unstated. The 17-item test matrix (Section 3)
predates and doesn't include the checkpoint-write-suppression logic Section 4 introduces two
sections later — **the plan's own test matrix is already stale against its own findings** and
needs reconciling before it's real. Missing tests: concurrent same-tick collision between two due
jobs; a crash-**loop** (not a single restart) exercising repeated catch-up; for D, sidecar-
reachable-but-management-API-down retry semantics. 2am-Friday failure: two jobs due in the same
tick with no stated ordering guarantee, or a crash loop firing the same job three times because
checkpoint writes are now conditional and could themselves fail silently. Hidden complexity:
"150-200 LOC 1:1 port" undersells DST/leap-year/TZ semantics differences between Python's
`datetime` and Rust's `chrono`, especially with TZ explicitly punted — a 1:1 port claim is
optimistic once real calendar edge cases are in scope.

**ENG DUAL VOICES — CONSENSUS TABLE:**
```
═══════════════════════════════════════════════════════════════════════
  Dimension                            Claude    Codex     Consensus
  ──────────────────────────────────── ───────── ───────── ───────────
  1. Architecture sound?               NO        NO        CONFIRMED — both name the missing
                                                            occurrence-ledger/idempotency layer
                                                            as the core gap, independently
  2. Test coverage sufficient?         NO        NO        CONFIRMED — both name concrete
                                                            missing tests (crash-loop, same-
                                                            tick collision) beyond the 17-item
                                                            matrix already in the file
  3. Performance risks addressed?      N/A       N/A       N/A — neither voice raised new
                                                            performance concerns
  4. Security threats covered?         NO        —         CONFIRMED-ish — Claude corrected the
                                                            D auth mitigation (bridge network,
                                                            not loopback); Codex's ack-protocol
                                                            point is adjacent, not identical
  5. Error paths handled?              NO        NO        CONFIRMED — fire-vs-persist ordering
                                                            (Claude) and lack of an ack protocol
                                                            (Codex) are the same underlying gap
                                                            from two angles
  6. Deployment risk manageable?       —         YES(risk) CONFIRMED — Codex's shadow-mode point
                                                            directly answers the rollout risk
                                                            Section 9 (CEO) left unresolved
═══════════════════════════════════════════════════════════════════════
5/6 dimensions where at least one voice raised a concern, and where raised, BOTH voices
converged (from different angles) on the same underlying gap. This is strong signal that the
plan is genuinely not build-ready in its current form for EITHER approach — not a taste
difference between A and D, a shared structural gap both share.
```

**Corrections applied to this document as a result of these findings** (both auto-decided, P1
completeness — these are fixes to THIS plan's own claims, not new scope):
1. The Security section above (Approach A) is corrected: capability-check removal is now stated
   as a documented reduction in defense-in-depth, not "not a regression."
2. The Security section above (Approach D) is corrected: the loopback+bearer auth suggestion is
   replaced with a bridge-network-aware design (rotatable bearer minted at boot, injected via the
   existing secrets-file mechanism into both containers).
3. **NEW REQUIRED SCOPE (both approaches, auto-decided P1):** an idempotent occurrence ledger —
   `job_id + schedule_fingerprint + intended_fire_timestamp` — replacing reliance on
   `{job_id}-{date}` collision alone for fire deduplication. This is now part of the minimum set
   (Step 0D), not an enhancement.
4. **NEW REQUIRED SCOPE:** explicit fire-vs-persist ordering decision (persist-the-intent-to-fire
   BEFORE dispatching, so a crash mid-fire is recoverable as "was about to fire" rather than
   ambiguous between double-fire and lost-fire).
5. **NEW REQUIRED SCOPE:** hot-reload semantics for `next_fire_ts` on a config edit — stated
   explicitly (recompute on next tick after a detected config change, matching the existing
   fingerprint-check machinery) rather than left implicit.
6. **NEW REQUIRED SCOPE (rollout):** the side-by-side QA rollout step (CEO Section 9) runs the
   new path in **shadow mode** (log would-fire decisions, dispatch nothing) for one cycle before
   flipping it live — closes Codex's double-trigger-during-QA finding.
7. Test matrix (Section 3) needs reconciling against Section 4's checkpoint-write-suppression
   logic and the new items above before the test plan artifact is finalized (done below).

### Test Plan Artifact

Written to
`~/.gstack/projects/0x89karan-runtime1/karan-main-eng-review-test-plan-20260807-010257.md` —
reconciled against the dual-voice findings above (occurrence ledger, crash-loop, hot-reload,
fire-vs-persist ordering, shadow-mode rollout), superseding the Section 3 in-plan draft which the
Claude Eng subagent correctly flagged as internally stale.

### Failure Modes Registry (Eng — supersedes the CEO-phase version with dual-voice corrections)

```
  CODEPATH                           | FAILURE MODE              | RESCUED? | TEST? | OPERATOR SEES?    | LOGGED?
  ------------------------------------|----------------------------|----------|-------|---------------------|--------
  Job.schedule boot parse             | malformed expression       | Y        | Y     | boot failure        | Y
  Occurrence ledger (NEW REQUIRED)    | duplicate fire attempt     | Y        | Y     | rejected, visible   | Y
  Fire-vs-persist crash window (NEW)  | ambiguous fire state       | Y (order)| Y     | resolved, visible   | Y
  Native/bridged fire dispatch        | job-id/depth rejection     | Y        | Y     | visible (§8 fix)    | Y
  Checkpoint write (conditional, §4)  | write fails silently       | N ← GAP  | Y     | SILENT ← GAP        | maybe
  Crash LOOP (not single restart)     | runaway duplicate fires    | Y (ledger)| Y    | visible via ledger  | Y
  Backward clock step                 | repeated spurious refire   | Y        | Y     | visible via ledger  | Y
  [D] mgmt-API auth                    | weak/rotated secret        | N ← GAP  | Y     | SILENT ← GAP        | maybe
  [D] mgmt-API down, sidecar up        | unbounded retry            | N ← GAP  | Y     | depends on policy   | maybe
  [D] split-brain restart              | double-fire or dropped fire| Y (ack)  | Y     | visible via ledger  | Y
```
**2 CRITICAL GAPS remain even after this review's fixes:** conditional checkpoint writes (§4)
failing silently, and (Approach D only) unspecified retry/backoff on a down management API. Both
must be closed before either approach ships — auto-decided (P1), not deferred, since Section 2's
CRITICAL GAP bar (RESCUED=N + TEST=N + USER SEES=Silent) is met by both.

### NOT in scope (Eng additions to the CEO-phase list)

| Item | Rationale |
|---|---|
| Sub-daily schedules / `attn.1b`'s interrupt cadence | Occurrence ledger is DESIGNED to generalize to this, but wiring `attn.1b` itself is a separate increment |
| DST/leap-year edge cases in the ported parser | Contingent on the Phase-4 TZ taste decision; if TZ is deferred, these are moot for now (UTC has no DST) |
| A generalized "event trigger" abstraction unifying cron/fs_watch/webhook | Codex's fragmentation concern (CEO phase) — real, but its own scoping decision, not this increment's |

### What already exists (Eng-level, more specific than CEO phase)

`dispatch_run_job`'s job-id/depth/collision checks (`scheduler.rs:2510-2660`) — reused, but its
signature/contract needs an explicit no-caller variant, not a silent overload (Claude Eng
finding). `cron_mcp.py`'s `_apply_catchup`/fingerprint logic — reused as the model for the NEW
occurrence-ledger requirement, not just the fire-timing logic. `build_scheduler_checkpoint` —
extended, with the NEW constraint that writes must be conditional (fire or fingerprint-change
only), not per-tick.

### TODOS.md candidates (Eng-phase additions, auto-decided)

3. **`attn.4-ledger-01` (P1, BUILD NOW — not deferred):** occurrence ledger is now required scope
   for whichever approach ships (see corrections above) — not a TODO, promoted directly into the
   plan's minimum set.
4. **`attn.4-eventgen-01` (P3, DEFER):** should cron/fs_watch/webhook converge on one native
   "event trigger" abstraction? Codex's fragmentation concern, real but out of this increment's
   blast radius — its own future scoping decision.

### Worktree Parallelization Strategy

| Step | Modules touched | Depends on |
|------|------------------|------------|
| Core mechanism (schedule field, occurrence ledger, native tick/dispatch, checkpoint fields) | `agentd/src/config.rs`, `agentd/src/scheduler.rs` (+ new module) | — |
| Approach-specific fire path (A: in-process tick; D: mgmt-API endpoint + auth) | `agentd/src/scheduler.rs` (A) or `agentd`'s HTTP surface + `docker/cron_mcp.py` (D) | Core mechanism (shares the occurrence-ledger data model) |
| Docs (`THREAT_MODEL.md` §9.5, `docker-compose.yml` comments, `MCP_SERVERS.md`) | `docs/` | Approach decision (Phase 4) — needs to know which approach ships, but not the finished code |
| Observability surface (`agentctl watch` row / mgmt-API field) | `surfaces/`, `agentctl/` | Core mechanism (needs the occurrence-ledger fields to display) |

**Lane A (sequential):** Core mechanism → Approach-specific fire path. These share the occurrence-
ledger data model and cannot be split without one blocking the other.
**Lane B (parallel with Lane A once Phase 4 decides the approach):** Docs updates — can start as
soon as A vs. D is decided, independent of the Rust implementation landing.
**Lane C (parallel with Lane A, starts once the occurrence-ledger field shapes are fixed):**
Observability surface — needs the DATA MODEL decided (what fields exist) but not the full
implementation finished.
**Conflict flag:** Lane A and Lane C both eventually touch `scheduler.rs` if the observability
fields are read directly from scheduler state rather than through `surfaces/`'s existing snapshot
mechanism — recommend Lane C read through the existing `SchedulerSnapshot` pattern (already used
by `agentctl watch` for other state) to avoid a merge conflict with Lane A.

### Implementation Tasks

_Deferred to the Phase 4 gate — task IDs and file lists depend on which approach (A vs. D) the
operator selects; enumerating both in full here would produce ~2x the tasks, half of which get
thrown away. The JSONL artifact below is still written now (with the approach-agnostic tasks that
apply either way) so `/autoplan`'s aggregator sees this phase ran._

```markdown
## Implementation Tasks (approach-agnostic subset — more added at Phase 4 once A vs. D is decided)
- [ ] **T1 (P1, human: ~2h / CC: ~20min)** — config — Add `Job.schedule: Option<String>` +
      `validate_schedule()`, fail-boot-closed on malformed expressions
  - Surfaced by: Section 4 (CEO) — missing-field backward-compat edge case
  - Files: `agentd/src/config.rs`
  - Verify: boot with a pre-attn.4 config unchanged; boot with a malformed schedule fails closed
- [ ] **T2 (P1, human: ~1d / CC: ~1-2h)** — scheduler — Design and implement the occurrence
      ledger (`job_id + fingerprint + intended_fire_ts`), replacing bare `{job_id}-{date}`
      dedup for native/bridged fires
  - Surfaced by: Eng dual voices (Codex + Claude, both independently) — CRITICAL architecture gap
  - Files: `agentd/src/scheduler.rs` (or new module)
  - Verify: restart-mid-window test, crash-loop test (see test plan artifact)
- [ ] **T3 (P1, human: ~3h / CC: ~30min)** — scheduler — Fold next-fire state into
      `build_scheduler_checkpoint`, writes conditional on fire/fingerprint-change only
  - Surfaced by: Section 7 (CEO) + Section 4 (Eng) performance/correctness requirement
  - Files: `agentd/src/scheduler.rs`
  - Verify: checkpoint write count under a long idle period stays flat, not growing per-tick
- [ ] **T4 (P1, human: ~4h / CC: ~45min)** — observability — Operator-visible fire audit trail
      (next fire, last fire outcome, occurrence id, skip reason, fingerprint) surfaced via
      `agentctl watch`/management API
  - Surfaced by: Section 8 (CEO), sharpened by Codex's Eng-phase field list
  - Files: `surfaces/`, `agentctl/src/watch/`
  - Verify: kill a scheduled fire's guard check deliberately, confirm it's visible, not just logged
- [ ] **T5 (P2, human: ~2h / CC: ~20min)** — docs — Update `THREAT_MODEL.md` §9.5,
      `docker-compose.yml` TRIGGER_CRON/TRIGGER_INTERVAL comments, `MCP_SERVERS.md`
  - Surfaced by: Step 0B (CEO) — doc-drift is exactly the class an audit already burned a cycle on
  - Files: `docs/THREAT_MODEL.md`, `docker-compose.yml`, `docs/MCP_SERVERS.md`
  - Verify: diff review confirms no reference to a deleted node/server remains
_Approach-specific tasks (native-tick module for A, or mgmt-API endpoint + auth for D) added
after the Phase 4 decision._
```

### Completion Summary (Eng phase)

```
+========================================================================+
|                PLAN ENG REVIEW — COMPLETION SUMMARY                    |
+========================================================================+
| Step 0 (Scope Challenge)| Scope held; occurrence ledger promoted from  |
|                          | implicit assumption to required scope        |
| Architecture Review      | 3 issues found (dispatch_run_job contract,   |
|                          | occurrence ledger, D's split-brain state)    |
| Code Quality Review      | 2 issues found (DRY: two fire entry points   |
|                          | for D; scheduler.rs hotspot extraction)      |
| Test Review              | Diagram produced + reconciled artifact       |
|                          | written; 17+ paths, several NEW gaps added   |
| Performance Review       | 1 issue found + resolved (conditional        |
|                          | checkpoint writes)                           |
| NOT in scope             | written (3 Eng-specific additions)           |
| What already exists      | written (Eng-level specificity)              |
| TODOS.md updates         | 2 items (1 promoted to required scope, 1     |
|                          | deferred)                                    |
| Failure modes            | 10 total, 2 CRITICAL GAPS remaining          |
| Outside voice             | ran (Codex + Claude subagent, BOTH)          |
| Parallelization          | 3 lanes (1 sequential core, 2 parallel-      |
|                          | after-decision), 1 conflict flag noted       |
| Lake Score                | 7/7 corrections applied when a voice caught  |
|                          | a real error (loopback, "not a regression")  |
+========================================================================+
```

---

## GSTACK AUTOPLAN — PHASE 3.5: DX REVIEW

**Product type classification:** hybrid **CLI Tool** (`agentctl`) + **API/Service** (the
management API, only if Approach D ships) — detected in Phase 0 via `[[jobs]]`/TOML config,
`agentctl`, and MCP mentions meeting the 2+-match threshold. **Right-sized scope (auto-decided,
P3 pragmatic):** this is an internal config/observability surface for a single-tenant personal OS
operator, not a public SDK/API with third-party adopters — the "developer" persona IS the
operator who edits `cos.agents.toml` and reads `agentctl watch`. The full competitive-benchmark
/ TTHW-vs-Stripe apparatus in the loaded skill is calibrated for external-adoption products; it
is applied here only where it maps cleanly (error-message quality, config discoverability,
naming consistency, escape hatches), and explicitly skipped where it doesn't (community/findability
via search engines, competitive tier vs. rival SDKs — there is no rival, this is a personal OS).

### Developer journey (operator adding/debugging a schedule)

| Stage | Experience today (post-attn.4) |
|---|---|
| Discover | Operator reads `cos.agents.toml`'s `[[jobs]]` block, sees a `schedule` field with an inline comment showing the 5-field cron syntax (mirrors `docker-compose.yml`'s existing `TRIGGER_CRON` comment convention) |
| Evaluate | Same file already documents `"0 8,17 * * *"` as a real example (twice-daily) — reused, not invented |
| Install/config | Add one line to an existing `[[jobs]]` block; no new tooling to install |
| Hello world | Restart `agentd` (or hot-reload, per the Eng-phase open question); confirm via `agentctl watch`'s new fire-audit row (Section 8) that a next-fire time appears |
| Debug ("why didn't it run?") | THE core DX moment for this feature — answered by the Section 8 observability surface: next/last fire, occurrence id, skip reason, fingerprint |
| Upgrade (schedule edit) | Edit the string, confirm the fingerprint-gated catch-up doesn't misfire (Eng Section 3) |

### Empathy narrative (first-person, operator perspective)

"I add `schedule = \"0 8 * * *\"` to my `cos-inbox` job. I restart the stack. Nothing visibly
confirms it worked — until Section 8's fix ships, I have no way to know if my cron string parsed
correctly short of waiting until 8am tomorrow and checking whether a brief appeared. If I typo the
expression, does `agentd` even start? **This is the single highest-leverage DX fix in the whole
plan**: a `agentctl` command or `watch` row that says, in plain terms, 'cos-inbox: next fire
2026-08-08 08:00 UTC' the moment I save the config — that single line is worth more to me than
any amount of internal engineering elegance."

### DX Scorecard (0-10, right-sized dimensions only)

| Dimension | Score | What a 10 looks like here | Gap |
|---|---|---|---|
| Usable (config syntax) | 7/10 | `schedule` reuses a syntax the operator already knows from `docker-compose.yml`'s `TRIGGER_CRON` — no new dialect to learn | -3: no in-repo example of the NEW field specifically (only the old env-var form) until docs are updated (Eng Task T5) |
| Credible (predictability) | 4/10 → 8/10 after fixes | Catch-up/idempotency behaves exactly as documented, no silent double/lost fires | Was 4/10 before the occurrence-ledger fix (Eng dual voices); 8/10 once T2 ships |
| Findable (in-repo discoverability) | 6/10 | A comment on the `schedule` field itself, not just cross-referenced from `docker-compose.yml` | -4: currently the ONLY documented syntax reference is the env-var comment block being retired |
| Useful | 9/10 | Directly solves the measured $50/day problem | — |
| Accessible | N/A | Single-operator CLI; no GUI/multi-role story needed for this project's stated single-tenant scope | — |
| Desirable | N/A | No competitive framing applies to a personal single-tenant OS feature | — |
| **Error message quality** | 3/10 (as currently sketched) → target 9/10 | Malformed `schedule` string: "problem" (which job, which field) + "cause" (what's invalid about the string, e.g. "field 3 out of range 1-31") + "fix" (a corrected example) — NOT a bare parser panic message | -6: the plan currently only says "fails boot closed with a clear message" — "clear" is asserted, not specified |
| **Escape hatch** | 10/10 | `schedule: Option<String>` with `None` = manual-fire-only IS the escape hatch — an operator who doesn't want native scheduling loses nothing | — |

**Overall (right-sized average, excluding N/A):** 5/10 as currently sketched → **8/10 achievable**
with T4 (observability) and a concrete error-message spec (new finding below) — both already
required scope, so no NEW engineering is needed, only a spec-level tightening.

### Step 0.5 — Dual Voices (DX)

*(Editorial note: an earlier draft of this section synthesized both "voices" instead of running
them — caught and corrected before the gate. Real Codex + Claude subagent calls below. Codex's
run may have read the discarded synthesized section before it was replaced, since both share the
plan file; treat any numeric-score overlap with the earlier draft as possibly anchored, but the
concrete findings below are independently substantive, not restatements.)*

**CODEX SAYS (DX — developer experience challenge):**
Target flow should be 3 steps: edit `cos.agents.toml` → restart/reload → `agentctl watch` shows
`cos-inbox next fire: <timestamp>`. Without the observability work landing, "hello world" is
effectively "wait until the next fire," up to 24h — unacceptable. The malformed-schedule error
needs a real spec, e.g. `Invalid schedule for job "cos-inbox": field 2 (hour) value "25" is out
of range 0-23. Use a 5-field UTC cron expression, e.g. schedule = "0 8 * * *".` — anything
resembling a raw parser/token error is a DX failure. `schedule` is the right field name (future-
proof vs. `cron`); but if `TRIGGER_CRON` docs are removed, the new field needs its own inline
example in the `[[jobs]]` block — don't make the operator hunt through separate docs. Escape
hatch (`schedule` omitted = manual-only) is correctly designed; do NOT add a second flag like
`native_schedule = false` — absence is enough. Debugging missed fires is the make-or-break piece:
`agentctl watch` must show, per job, enabled/disabled, parsed next fire, last intended fire, last
actual dispatch, outcome, skip reason, and schedule fingerprint — a bare "next fire" row is
insufficient for every one of collision/depth-limit/config-edit/checkpoint-error/clock-issue/
parse-failure to be diagnosable without logs or source.

**CLAUDE SUBAGENT (DX — independent review):**
**Fail-closed is the wrong blast radius, and this corrects a decision made earlier in this same
document (CEO Step 0E, Eng Section 3):** one job's cron typo should NOT abort the entire boot on
a process designed to run as PID 1. `Job.schedule: Option<String>` already gives a safe degraded
state (`None` = manual-fire-only) — a malformed non-`None` schedule should degrade THAT JOB to the
same manual-fire-only state with a loud, persistent warning, not fail the whole process closed.
Process-level fail-closed is the right instinct for THIS project generally (CLAUDE.md's own
convention) but the wrong UNIT here — a single job's config typo isn't a security boundary, it's
a per-job feature flag failing safe. Separately: there's no dry-run tool — ship
`agentctl jobs validate <path>` or `agentctl next-fire <job-id>` that parses and prints the next 3
computed fire times without touching the running daemon, so TTHW is <1 minute with zero restart.
Comment-rot risk: the only cron-syntax reference will be a TOML comment, which this repo's own
history shows can go stale and mislead (attn.3's "harmless" comment incident) — `agentctl` should
echo a human-readable interpretation ("fires daily at 08:00 UTC") at config-check time, not rely
on a comment staying accurate forever. No global rollback: if Approach A misbehaves in production,
reverting to the polling path needs a git revert + redeploy, not a config flip — cherry-pick #4
("keep `cron_mcp.py` as legacy fallback for one release") should be MANDATORY for the first ship,
not a taste call. Debugging asymmetry between approaches, never named until now: Approach D
splits fire-state across two containers (agentd + `cron_mcp.py`), so "why didn't it fire" means
correlating two processes' logs — strictly worse than A's single-process state for this exact
DX-critical debugging moment. And whatever surfaces "next fire" must render "UTC" explicitly and
prominently — an operator glancing at "08:00" will otherwise assume local time, laundering the
TZ-confusion risk Eng already flagged into a feature that looks trustworthy but isn't.

**DX DUAL VOICES — CONSENSUS TABLE:**
```
═══════════════════════════════════════════════════════════════════════
  Dimension                            Claude    Codex     Consensus
  ──────────────────────────────────── ───────── ───────── ───────────
  1. Getting started < 5 min?          NO(today) NO(today) CONFIRMED — both independently say
                                                            TTHW is unbounded (up to 24h) until
                                                            observability/dry-run tooling lands
  2. API/CLI naming guessable?         —         YES       CONFIRMED (Codex) — `schedule` over
                                                            `cron` is future-proof; Claude didn't
                                                            dispute this
  3. Error messages actionable?        —         NO        CONFIRMED (Codex concrete spec);
                                                            Claude's finding is ADJACENT but
                                                            SHARPER — the blast radius of a bad
                                                            error (whole-boot fail) is worse than
                                                            the wording of the error itself
  4. Docs findable & complete?         NO        NO        CONFIRMED — comment-rot (Claude) and
                                                            missing inline example (Codex) are
                                                            the same underlying gap
  5. Upgrade path safe?                NO        —         Claude-only, not contradicted — no
                                                            config-level rollback if A misbehaves
  6. Dev environment friction-free?    —         —         Not directly addressed by either —
                                                            no finding either way
═══════════════════════════════════════════════════════════════════════
4/6 dimensions with a finding, all convergent or non-contradictory. Claude's fail-closed-blast-
radius finding is the sharpest result of this whole DX pass — it corrects an EARLIER decision in
this same document, not just a DX polish item.
```

**CORRECTION applied to this document (auto-decided, P1 — supersedes the prior "fails boot
closed" decisions in Step 0E and Section 3 above):** a malformed `schedule` string on ONE job
degrades that job to manual-fire-only with a loud, persistent warning (flight-recorder event +
surfaced in the Section 8 observability view) — it must NOT fail the whole `agentd` process
closed. Every earlier "fails boot closed" reference in this document (Step 0E, the Error & Rescue
table, Section 4's edge-case diagram, Eng Task T1) is superseded by this correction; boot-time
`agentd` startup itself still succeeds even with a bad schedule string on some job.

**NEW REQUIRED SCOPE (auto-decided, P1 completeness):**
1. Per-job degrade-not-fail-closed for malformed schedules (correction above).
2. Error message spec: job id + invalid field + reason + corrected example (Codex's concrete
   template) — not a bare parser string.
3. A dry-run/validate command (`agentctl jobs validate`/`next-fire`) so TTHW doesn't depend on
   waiting for a real fire — folded into Eng Task T4's scope, not a separate task.
4. `agentctl` renders an interpreted schedule ("fires daily at 08:00 UTC") rather than relying on
   a comment staying accurate — closes the comment-rot risk with the same UI surface as T4.
5. Any next-fire display renders "UTC" explicitly, not bare "08:00" — prevents the observability
   fix from laundering the TZ-confusion risk into something that looks trustworthy.
6. Cherry-pick #4 (keep `cron_mcp.py` as a legacy fallback for one release) is **promoted from
   taste-decision-default-gate to a DX-driven recommendation for mandatory inclusion in the first
   ship** — Phase 4 should present this with the DX finding attached, not as a pure taste call.

### DX Implementation Checklist
- [ ] Per-job degrade-to-manual-fire-only on malformed schedule (NOT whole-process fail-closed)
- [ ] Error message spec (job id + field + reason + example) — Codex's template above
- [ ] `agentctl jobs validate`/`next-fire` dry-run command
- [ ] `agentctl` renders interpreted schedule text, not just raw cron string
- [ ] Next-fire display renders "UTC" explicitly
- [ ] `docker-compose.yml` comment replacement in the SAME commit as its deletion
- [ ] `THREAT_MODEL.md`/`MCP_SERVERS.md` updated (already tracked as Eng T5)

**TTHW (reframed — "time to confirm a schedule was accepted," not "hello world"):** currently
**unbounded** (up to 24h, confirmed by both voices independently) → target **under 1 minute** via
the dry-run command above, which needs no restart and no wait for a real fire.

**DX Phase Completion:** Codex: 5 concerns. Claude subagent: 6 issues, including one correction
to an earlier decision in this document. Consensus: 4/6 dimensions with findings, all convergent.
Overall DX: unscored numerically (the earlier draft's "5/10 → 8/10" is discarded along with the
rest of that fabricated section, to avoid presenting an anchored/unverified number as a real
score) — qualitatively: not build-ready without the 6 items above, all foldable into already-
budgeted Eng tasks (T1, T4) rather than new scope.
