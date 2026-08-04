<!-- /autoplan restore point: ~/.gstack/projects/0x89karan-runtime1/attn.3-real-context-window-autoplan-restore-20260802-142600.md -->
# attn.3 — Repair malformed history before every request, and make the next diagnosis free

**Status:** RESHAPED at the /autoplan gate (2026-08-02). Original title was "Give paging a
real context window"; that premise was **falsified by measurement** — see §0.
**Closes:** `audit118-R2`.
**Demotes:** `audit118-R1` from P0 to P2, **blocked** (§3).
**Spawns:** `attn.4` — scheduler-native cron (§4). **The brief is not expected to return
until attn.4 ships.**
**Branch:** `attn.3-real-context-window` (name kept; the branch is already pushed against it)
**Predecessor:** attn.2 R1+R2 (v0.119.0, merged `db9cccb3`).

---

## 0. MEASURED AT THE GATE — what actually stopped the brief

Codex's CEO voice asked for a cheap diagnostic before building. The live volume
`agentos_cos-data` still exists, so it was run: `docker run --rm -v agentos_cos-data:/data:ro`
over `flight.jsonl` (573 KB, 2026-08-01T17:49:15 → 18:18:37, 29 min).

1. **Only `cos-orchestrator` ever ran.** `cos-inbox` and `cos-curator` appear ZERO times.
   There is no brief because the jobs never fired.
2. **All 63 tool calls are the same call:** `wait_for_trigger(timeout_s=20)`, each returning
   `{"status":"waiting","next_fire_utc":"2026-08-02T08:00:00Z"}` — 14 hours away.
3. `msg_count` climbs 1 → 126 then **FREEZES**. Two `error` + two `agent_failed`, 3.5 min
   apart, identical: `Anthropic API 400 ... messages.125: 'tool_use' ids were found without
   'tool_result' blocks immediately after: toolu_01FonBTdyrQmHudK1ChQAHcZ`.
4. 63 responses: **414,016** input tokens, 3,622 output. Final request input **11,569**.
   Slope ≈ **159 tokens per poll pair**.
5. **Zero** `memory_paged`, zero `memory_pressure_advisory`, zero budget/defer events.
6. `docker/cron_mcp.py:31` `MAX_TIMEOUT_S = 25`, sized to fit under `agentd/src/tools/mcp.rs:23`
   `MCP_TIMEOUT = 30s`.

### The real causal chain

```
1. Orchestrator polls wait_for_trigger(20s) to watch a cron 14 h away
   -> one full inference turn per ~20 s, 2 messages appended per turn,
      whole transcript resent  ->  126 msgs / 414k tokens in 29 min
2. History goes malformed at messages.125  ->  repeating 400  ->  agent_failed   <- attn.3 (R2)
3. Process exits 1; container restart hits the missing-embeddings-key boot gate  <- attn.2, FIXED
4. 08:00 arrives with nothing running  ->  no brief
```

### ⚠ Two independent proofs that R1 (the original plan) does not fix this

- **Measured distance.** Retained context at the final request: **11,569** tokens. The
  proposed trigger was 90% × (200,000 − 8,192) = **172,627**. Measured is **15× below** it.
- **Arithmetic inevitability.** With the measured slope p ≈ 159 tokens/poll-pair, the 10M/24h
  global window dies at turn `sqrt(2·10e6/159)` ≈ **355** (~2.6 h at the observed 2.24 req/min),
  while paging would first fire at turn `172,627/159` ≈ **1,086**. **Budget exhaustion beats
  paging by ~3×, always.** Re-keying the denominator to the context window leaves the trigger
  dying of budget having never emitted `memory_paged`.

Therefore `AUDIT-v0.118.md` R1's "independent, **sufficient** explanation for three briefs in
fifteen days" is **overclaimed**, and so was this plan's original "the RUNTIME half". Both are
corrected here. R1 remains a **real latent bug** (the denominator IS wrong) — it is simply not
this bug, and it is not P0.

**The polling was known and accepted, not overlooked.** `cos.agents.toml:271-273`:
`# CRITICAL: max_turns MUST be high. At 25 s/poll the trigger burns ~3456 turns/day just
waiting for the cron.` The limits were raised to *accommodate* the furnace. The following
comment — "The trigger's per-turn context is now tiny (it reads nothing), so this headroom is
generous" — is the false assumption: tiny per turn, but the transcript accumulates.

---

## 1. Scope of THIS increment

### ⚠ A1 WITHDRAWN at the eng gate — third falsification, and the decisive one

A1 was "run the repair before every `InferenceRequest`". It is **not being built.** The free
flight-log check the eng review asked for pinned the real mechanism, and it is the restore path:

```
18:13:31  tool_call  toolu_01FonBTdyrQmHudK1ChQAHcZ   (a 20-second wait_for_trigger)
18:13:47  system_shutdown_requested        <- SIGTERM, 16s into a 20s call
          ...no tool_result. 63 tool_call vs 62 tool_result across the run.
          ...the half-finished turn is CHECKPOINTED
18:13:57  agent_restored  turn 62
18:13:58  error: messages.125 tool_use without tool_result   <- ONE SECOND after restore
18:13:58  agent_failed
          (identical cycle again at 18:17:22 / :32 / :33)
```

`AUDIT-v0.118.md` R2 asserts "the live 400 loop was **in-memory**: there is no
`checkpoint.json` in the volume." That inference is **wrong**: there are 65
`agent_checkpointed` and 2 `agent_restored` events, and the 400 lands one second after each
restore, twice. It is the restore path — which is exactly what **attn.2's
`repair_dangling_tool_uses` already fixes**, merged as `db9cccb3` the day *after* this log was
recorded. The `agent_restored` payload here is a bare `{"turn":62}` with no repair fields: the
signature of the pre-attn.2 binary.

Two further reasons A1 was the wrong build:

- **No demonstrated producer.** The eng review's candidate mechanism (a `ToolUse` block pushed
  under a non-`ToolUse` stop reason via a cut stream) was **refuted by the log**: the offending
  id *does* appear in a `tool_call` event, so it was dispatched, and every `stop_reason` in the
  run is `tool_use`, never `end_turn`. It also traced all 16 `step()` sites and found
  `live_call_ids` is `∅` on every live path (an awaiting parent is never stepped; an
  approval-parked agent is never stepped; `Inject` refuses any agent not in `waiting`).
- **It would have removed the circuit breaker.** Today the repeating 400 kills the trigger at
  ~29 minutes. With the live history repaired, the trigger survives its own malformed state and
  keeps polling until the 10M window dies at ~2.6 h — making the furnace *more* expensive.

### A3 — Stop CREATING the malformed checkpoint (the actual producer)

The producer is SIGTERM landing mid-tool-call. It recurs on every restart. attn.2 makes it
*survivable*; nothing makes it *not happen*.

Fix, at `build_scheduler_checkpoint` (`scheduler.rs:3742`) — the single site where every
checkpoint's agent transcripts are built, so all callers are covered by construction: repair
each agent's checkpointed transcript before it is written.

**The repair applies to the checkpoint COPY, never to the live transcript.** That is not a
shortcut, it is the correct semantics: a checkpoint answers "what if we died right now", and in
that world the in-flight tool result never arrives. The live agent may still receive the real
result and carry on, untouched. It also makes attn.2's existing synthetic wording
("Interrupted by a restart before this tool produced a result") **literally true** — a
checkpoint is only ever read back after a restart — where A1 on the hot path would have made
that same sentence a lie.

Live ids come from `awaiting.values()` **and** `pending_approvals.values()` — `.values()`,
because `awaiting` is keyed by CHILD id and `pending_approvals` by APPROVAL id, so the keys are
not call ids (the ux.13 P0 confusion). Restore rebuilds both tables from this same checkpoint,
so anything listed there gets its real result on the way back up.

### A2 — Make the next diagnosis free (instrumentation, behaviour-free)

Everything in §0 required mounting a Docker volume and reconstructing intent from tool-call
previews. `EventKind::InferenceRequest` currently carries only
`{model, msg_count, tool_count}`. Add:

- `retained_tokens` — `estimate_context_tokens(&self.messages)`
- `paging_limit_tokens` — the value the paging decision actually compares against
- `paging_limit_source` — the literal `"token_budget"`

**⚠ The obvious name is a trap.** The original draft called the second field `context_limit`.
R1 is *not* being fixed here, so the value is still `token_budget` — a **spend ceiling**. A field
named `context_limit` holding a spend ceiling is precisely the kind of false-guarantee label that
produced this plan's own falsified premise, and a future session reading `context_limit: 5000000000`
would conclude the context window was configured at 5e9. Naming the source in the payload makes the
wrongness self-evident in the log and turns the instrumentation into evidence *for* fixing R1.

No behaviour change, no new config. This is the single cheapest thing in the increment and it
is what would have caught both R1 falsifications before a plan was written. It also makes
attn.4's before/after measurable without volume archaeology.

**Honest limitation, recorded because §0 exists:** `estimate_context_tokens` walks `messages`
only. `agent/mod.rs:898` also sends `tools: self.specs.clone()` (the Gmail + semantic-KB
schemas, plausibly 5–15k tokens for the inbox job), and it uses a 4-chars-per-token heuristic.
So `retained_tokens` is a **documented undercount**, not ground truth. The provider reports the
real number as `input_tokens` at `message_start` (`anthropic.rs:85`). Emitting the estimate
labelled as an estimate is still strictly better than emitting nothing; keying *decisions* off
it is a separate question, deferred to attn.4 with R1.

---

## 2. Acceptance criteria — RESULT

| # | Criterion | Status |
|---|-----------|--------|
| 1 | A3: a checkpoint built from an in-flight tool call is well-formed | ✅ `checkpoint_seals_an_inflight_tool_call_so_restore_cannot_400` |
| 2 | The LIVE transcript is not mutated by the checkpoint repair | ✅ `checkpoint_repair_does_not_touch_the_live_transcript` |
| 3 | Negative control: a promised call is NOT sealed | ✅ `checkpoint_does_not_seal_a_call_the_scheduler_promised_to_answer` |
| 4 | A well-formed transcript passes through unchanged | ✅ `checkpoint_leaves_a_well_formed_transcript_byte_identical` (serialized compare, not `len()`) |
| 5 | A2 fields present, asserted from the EMITTED JSON | ✅ `inference_request_event_carries_the_paging_numbers_honestly_named` |
| 6 | Workspace gate green from the repo root | ✅ clippy clean; tests below |
| 7 | Affected docs updated in the same PR | RUNBOOK + CONVENTIONS taxonomy row |

### Mutation controls — 5 of 6 guards proven, and the sixth is named honestly

Every mutation below hit its anchor (verified by grepping for the mutated text before running,
because a mutation whose anchor misses looks exactly like proof):

| Mutation | Turned red |
|---|---|
| M1 remove the repair entirely | AC1 + AC2 |
| M2 `awaiting.values()` → `.keys()` (the ux.13 P0) | **only** AC3, the negative control |
| M3 `paging_limit` → `context_limit` | AC5 |
| M4 drop the `_est` undercount marker | AC5 |
| M5 mislabel source as `"context_window"` | AC5 |

**AC4 is a no-op-safety guard and M1 does NOT kill it — correctly.** Removing the repair also
leaves a well-formed transcript alone, so nothing can kill AC4 except a future *overreach*. It
is not mutation-proven and is not claimed to be. Recording that beats inflating the count.

### False greens to avoid (enumerated by the eng review — treat as a checklist)

- Testing `repair_dangling_tool_uses()` **directly** instead of through a real `step()`.
- Testing only the checkpoint-restore path (that is attn.2's coverage, already green).
- Repairing the request clone and returning a terminal response, so the stale
  `AgentTask.messages` is never exercised a second time (wrong-design #2, invisible without AC2).
- Exercising only the distillation path at `scheduler.rs:1191`.
- Missing the deferred-replay carrier (`scheduler.rs:1874`).
- A malformed fixture where a **later, non-immediate** `tool_result` accidentally satisfies the
  assertion — the provider rule is "immediately following turn", so the fixture must be
  position-sensitive.
- Asserting only that an event was logged, rather than that the **gateway received** a repaired
  request.
- Note for AC6: `main.rs:1568` (probe mode) also emits an `inference_request` with the old shape,
  and distillation sends a request with **no** event at all. An assertion phrased as "every
  `inference_request` carries these fields" is therefore false as written — scope it to the agent
  loop or update the probe.

**Not an acceptance criterion, deliberately:** "a brief appears." It will not, until attn.4.
Claiming otherwise would be the fourth consecutive increment asserting an unverified product
outcome (brief.1's prompt adherence, ux.6a's `record_denied` with zero production callers,
attn.1a's recreate gap).

---

## 3. audit118-R1 — demoted to P2 and BLOCKED, not merely deferred

R1 stays open as a real bug: `assess(retained, self.cfg.token_budget)` (`agent/mod.rs:813`)
divides a context measurement by a spend ceiling. But it must **not** be "fixed" by simply
turning paging on, because **paging is lossy and unrecoverable**, verified in code:

- `cap_short_term` (`agent/mod.rs:522-528`) does `self.short_term.drain(0..overflow)` — a
  silent drop; the return value is only a count.
- The sole consumer of `short_term` is post-run distillation, gated on
  `memory.distill_on_complete`, which **defaults `false`** (`config.rs:196`) and is **not set
  in `cos.agents.toml`**. It never runs for the CoS.
- There is **no in-run recall path**. `mem_recall` reads the redb store, not `short_term`.
- `page_count` returns `(len−1)/4`, so a fire sheds ~a quarter of the transcript, oldest-first.

For `cos-inbox`, whose entire job is to read the morning's email and emit today's brief,
oldest-first eviction discards **the emails it read first**, with no way to recall them. That
turns "no brief" into "quietly incomplete brief" — and the operator's own success measure is
"do I stop checking email manually," so a silently-truncated brief is the worst possible
outcome. `memory_paged` would be in the log the whole time, looking like the fix working.

**Interlock, to be written into `TODOS.md` and `CONVENTIONS.md`:** paging must not be enabled
on any agent whose output the operator reads until it is non-lossy (distillation on by default
plus an in-run recall path). Until then, R1's wrong denominator is *inert*, and inert is safer
than enabled. Fixing R1 in isolation would be an active regression.

Also recorded for whoever picks R1 up:
- A single `[model]` table (`config.rs:34`) means one `context_window` for all three agents.
  The trigger and the jobs need different limits, so the paging limit is a **per-agent**
  property even though the context window is a model property.
- The paging FLOOR is reachable: `page_count` returns 0 for any transcript of ≤4 messages, and
  `cos-inbox` pulls ~20 Gmail messages into a *single* `tool_result`. A 3-message transcript
  can exceed any limit with nothing to page.
- **Prompt caching does not exist anywhere in the inference path** (`cache_control` /
  `prompt_caching`: zero hits in `agentd/src/`; the only `anthropic-beta` references are an
  `egress.rs` passthrough allowlist). The dominant cost term in §0 is resending an identical
  prefix every turn, which is the textbook case for it, and it is **non-lossy** where paging is
  not. It belongs in the R1/attn.4 analysis and was never considered.

---

## 4. attn.4 — scheduler-native cron (the chosen 10× reframe, NOT in this increment)

Selected at the gate over raising `MCP_TIMEOUT` globally (B1) and a per-tool long-poll
allowance (B2). B1 was rejected as blunt: `MCP_TIMEOUT` covers every MCP call (gmail, qdrant,
semantic-kb, oauth, shell_exec), so a hung sidecar that fails in 30 s today would pin an agent
for minutes, on a process intended to run as PID 1 with `panic = "abort"`.

Goal: **no LLM sits on the schedule boundary at all.** Give `[[jobs]]` a schedule and fire them
from the scheduler. Removes ~3,456 daily inference calls, deletes the transcript-growth problem
rather than bounding it, and *strengthens* cap.2b (a de-privileged LLM node that holds nothing
beats no node at all only if the node is required — it is not).

**Verified NOT a config flip.** `config.rs` `Job` has **no** schedule field and `scheduler.rs`
has no native job cron; the `cron_trigger` MCP server plus the polling agent is currently the
*only* scheduling mechanism. Real design questions attn.4 must settle at its own /autoplan:

- Cron parsing in Rust (a new dependency, or port `cron_mcp.py`'s hand-rolled 5-field parser?).
- Missed-fire / catch-up semantics. `run.1` already shipped "cron catch-up" for something else;
  reconcile rather than reinvent.
- **Timezone.** CLAUDE.md: nothing in the stack can express local time — no `chrono::Local`, no
  `tzdata` in the image, no `TZ` in the cos env. "08:00" silently means UTC.
- Durability across restart (the MCP server persists `next_fire_ts` in `cron_state.json` today;
  the scheduler would need an equivalent).
- Threat-model delta: deleting the trigger agent removes a node `THREAT_MODEL.md` currently
  describes. cap.2b's write-up needs updating, not just the code.
- What happens to `cron_mcp.py`, `fs_watch_mcp.py`, and `webhook_mcp.py` (h7.3 shipped three
  event-trigger servers). Does native scheduling deprecate one, or all three?

---

## 5. Explicitly NOT in scope

- Fixing R1 (§3 — demoted and blocked; fixing it alone is a regression).
- attn.4 / B3 itself (§4).
- Prompt caching (belongs with the R1/attn.4 cost analysis).
- Bringing the stack up. With the furnace still running that costs ~$50/day of inference at the
  measured slope to watch a clock. Worth doing once attn.3 lands only to confirm the 400 loop is
  gone via the new instrumentation, then bring it back down.
- `audit118-R3..R10`.

## 6. Risks

- **The repair now runs on the hot path**, before every request rather than twice per boot. It
  must be cheap and must not mutate a well-formed transcript (AC5).
- **Dropping a live `tool_use` would break a running parent/child handoff.** AC4 is the guard.
  `state.awaiting` is keyed by **child** id and parents live in `.values()` — the exact
  confusion that produced ux.13's P0. Read it carefully.
- **attn.3 does not restore the brief.** If it is reported as doing so, that is the overclaim
  pattern this plan exists to correct.

---

## Decision Audit Trail

| # | Phase | Decision | Class | Principle | Rationale |
|---|-------|----------|-------|-----------|-----------|
| 1 | CEO | Run the cheap diagnostic before building | Mechanical | P6 bias to action | Codex asked; volume still existed; it falsified the premise in ~20 min |
| 2 | CEO | **Re-scope to the measured causes** | **OPERATOR GATE** | — | D1. R1 measured 15x from mattering and 3x arithmetically unreachable |
| 3 | CEO | **B3 scheduler-native cron** for the furnace | **OPERATOR GATE** | — | D2. Retires the problem class instead of bounding it |
| 4 | CEO | Split B3 out as attn.4, not this branch | Taste | P5 explicit | B3 is a feature (no `Job.schedule`, no native cron); one increment per branch |
| 5 | CEO | R1 → P2 and **BLOCKED**, not merely deferred | Taste | P1 completeness | Paging is lossy; fixing R1 alone is an active regression |
| 6 | Eng | Repair `AgentTask.messages`, not the request | Mechanical | P5 explicit | `:900` clones; request-level repair is not durable |
| 7 | Eng | `step()` takes live ids → compiler-enforced coverage | Taste | P5 explicit | 10 call sites; hand-hooking is the five-time false-green pattern |
| 8 | Eng | `paging_limit_tokens` + `_source`, not `context_limit` | Mechanical | P5 explicit | Value is a spend ceiling; the honest name prevents the next false premise |
| 9 | Eng | Keep restore-path `repair_and_record` | Mechanical | P4 DRY | Records a distinct `AgentRestored`; belt-and-braces, not dead |
| 10 | Eng | Add AC2 (durability across a 2nd request) | Mechanical | P1 completeness | The only AC that catches wrong-design #2 |

## Review consensus

```
CEO DUAL VOICES
  Dimension                          Claude   Codex   Consensus
  1. Premises valid?                 NO       NO      CONFIRMED (both falsified the headline)
  2. Right problem to solve?         NO       NO      CONFIRMED (reframe to the poll furnace)
  3. Scope calibration correct?      NO       NO      CONFIRMED (R1 is not P0)
  4. Alternatives explored?          NO       NO      CONFIRMED (caching + native cron absent)
  5. Competitive/market risk?        HIGH     HIGH    CONFIRMED (brief is a commodity; cost/mo)
  6. 6-month trajectory sound?       NO       NO      CONFIRMED (both: work the mv gate)
  Voices: codex + 1 subagent. The CEO *executor* subagent died on an API
  error mid-run -> phase tagged [degraded]; 2 of 3 intended voices landed.

ENG DUAL VOICES
  Dimension                          Claude   Codex   Consensus
  1. Architecture sound?             (pending) NO     Codex found 2 P0s, both verified in code
  2. All request sites enumerated?   (pending) NO     plan named the wrong site entirely
  3. Hot-path cost acceptable?       (pending) YES    O(n), allocates, no panic path
  4. Event consumers safe?           (pending) YES    inspector substring-filters; ALL is kind-only
  5. Naming honest?                  (pending) NO     context_limit -> paging_limit_*
  6. False-green risk?               (pending) HIGH   8 named traps, now an AC checklist

DESIGN: skipped, no UI scope (0 term matches).
DX: folded into Eng — the only DX surface is the log field naming (decision 8) and
    the RUNBOOK triage entry (AC8). No separate voices run; scope did not warrant it.
```

## Cross-phase theme

**Naming and claims outrun the code, in both phases.** CEO found the plan's own headline
falsified by the plan's own cited measurement; Eng found a field name (`context_limit`) that
would encode a spend ceiling as a context window. Same defect class, one increment apart, and
it is the class `CLAUDE.md` already warns about twice. The correction in both cases was to read
the artifact instead of the summary.

## GSTACK REVIEW REPORT

- **Verdict:** APPROVED as reshaped. Original plan **rejected** by measurement.
- **Voices:** CEO codex + 1 subagent (1 died, `[degraded]`); Eng codex + 1 subagent.
- **Premise falsified at the gate**, with two independent proofs (15x measured distance;
  3x arithmetic inevitability). Original headline withdrawn in-file rather than softened.
- **Two P0 design errors** found and corrected before any code was written, both verified
  against the code rather than accepted on assertion.
- **Operator gates:** D1 re-scope to measured causes; D2 B3 scheduler-native cron.
- **Ships:** A1 (durable in-loop repair, compiler-enforced coverage) + A2 (honest
  instrumentation). **Does NOT ship:** R1, B3, prompt caching.
- **Explicitly not claimed:** that this restores the brief. It does not.
