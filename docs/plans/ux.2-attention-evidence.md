<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/ux.2-observe-attention-evidence-autoplan-restore-20260713-131006.md -->
# ux.2a — Attention: outcome/risk-centric cockpit home view (reframe of "Observe")

**Partially addresses `cos-ux-01`** (Approval/Budget/Degraded visibility) — **does NOT close it**
(the founding busy-vs-hung incident is the Idle signal, deferred to a follow-on, ux.2b — see
"Eng Review — Idle/Error rescope" below).

## Goal

Give the cockpit operator a home view that answers "what needs my attention" before it answers
"what is every agent doing right now" — replacing the raw-activity-detail framing of the
superseded `docs/plans/ux.2-observe.md` with one centered on outcomes, risk, and
decisions-needed, per the reframe decided at that plan's Phase 4 gate.

## Why this reframe (carried forward from the superseded plan's CEO Dual Voices)

Codex's CEO-phase review of the "Observe" framing argued: `docs/PRODUCT-THESIS.md`'s
load-bearing build-priority list ranks the approval gate **#3** above observability **#4** — the
thesis's north star is trusting agents *without* having to watch them, not a more detailed
dashboard to watch them with. A table optimized for terminal dwell time (tool names, idle
timers) is in tension with that promise. The user, at the Phase 4 gate, chose this reframe over
shipping the fully-reviewed "Observe" plan.

**Real tension this reframe must resolve, not ignore (found during this plan's own Step 0A,
below):** `docs/ROADMAP.md`'s own "North star (2026-07-11)" note explicitly states ux.0/2/1/8
exist to make the cockpit "live, watchable, chattable, tunable" — i.e. watchability is the
roadmap's own stated design intent for this exact increment, not an oversight Codex is catching.
This reframe is not "Codex was right and the roadmap was wrong" — it's a real, load-bearing
strategic tension between two legitimate framings (watchable-by-default vs. needs-attention-only)
that this CEO review must adjudicate on its own merits, not inherit as already-settled.

## Naming correction (Mechanical, auto-decided — P5 explicit)

Codex's original phrase was "Attention & Evidence." **"Evidence" collides with an already-scoped,
unrelated future increment:** `docs/ROADMAP.md` line 1130 / `docs/plans/ux-cockpit.md` line 269
already name **ux.6 — Evidence view**, which surfaces the signed Ed25519 receipt chain
(`evidence.jsonl`) + `agentctl verify` — a specific cryptographic-audit feature, unrelated to this
increment's actual scope (outcome/risk-centric activity surface). Using "Evidence" for both
would confuse two different features in every future roadmap reference. **This plan is named
"ux.2 — Attention"** (file name kept as `ux.2-attention-evidence.md` for continuity with the
already-created restore point; the roadmap/master-plan rename to "Attention" happens as part of
this plan's TODOS/doc-update pass, not silently).

## What already exists (carried forward, code-verified in the superseded plan's Eng Review —
not re-verified from scratch here, but available for this review's own 0B/0C-bis steps)

- `surfaces::AgentSnapshot` has no risk/outcome-centric fields today (only activity-adjacent
  ones the superseded plan was going to add: none of those landed — no code was written).
- An `Approvals` view (`[a]` key) already exists as a first-class, discoverable cockpit view
  (`agentctl/src/watch/views.rs:153`'s footer hints, shipped p7.4/v0.38.0) — this is the most
  directly relevant existing mechanism for an outcome/risk-centric home view: it already
  surfaces "things needing a decision." The open question for this review: does Attention
  *replace* Dashboard-as-home-view with something Approvals-shaped, or does it add risk/outcome
  signals *onto* the existing Dashboard rows, demoting (not removing) activity detail to a
  secondary view? This is the central architecture question for 0C-bis below.
- `agentd/src/credential/mod.rs`'s spend-cap system (cred.4) and `ProviderHealth` (cred.5) already
  track risk-adjacent signals (budget exhaustion, provider auth failures) that could feed an
  outcome/risk surface without new instrumentation.
- `PRODUCT-THESIS.md`'s full text (already read in the superseded plan's CEO phase) — approval
  gate, egress mediator, isolation floor, observability, CoS harness, in that priority order.
- The superseded plan's Eng Review (`docs/plans/ux.2-observe.md`, "Eng Review revises/reopens
  Correction #1/#2") found two real code-level facts still relevant here regardless of framing:
  (1) `agentd/src/scheduler.rs`'s `run_tools_sequential` has no mutable `AgentTask` access —
  any per-agent field this plan adds must be stamped at the `CallTools` dispatch site, not
  inside the tool-call loop; (2) `agentctl/src/watch/pump.rs` (ux.0) already produces a live SSE
  `AppEvent::Flight` stream for HTTP-mode cockpit — any live "risk event" surface should consume
  this, not invent new transport.

## NOT in scope (deferred, pending this review's own scope pass — placeholder, confirmed below)

- ux.6 (Evidence/receipt-chain view), ux.4 (proactive push/notifications), ux.1 (chat rail),
  ux.3 (spawn-into-running-instance), ux.8 (budget control) — separate, already-sequenced
  increments per `docs/ROADMAP.md`.

## Premise correction — none found yet (0A below is the first real pass)

This section will be updated by Step 0A if the premise challenge finds anything to correct.

## CEO Review (Phase 1)

### Step 0A — Premise Challenge

**Premise 1: cos-ux-01's pain point is real and this increment must still solve it.** VALID,
not manufactured — `TODOS.md:768-781` documents a specific, already-observed incident (inbox
agent fetching 20 Gmail messages, operator sees only `running` + a growing context counter, no
way to tell busy from hung). Whatever this increment's information hierarchy becomes, an
operator must still be able to answer "is this agent stuck?" — the reframe changes *what leads*,
not whether this need gets met at all. **⚠ Amended by the Eng Dual Voices rescope (Phase 3,
below): this increment ("ux.2a") does NOT itself deliver the "is it stuck" answer — that's the
Idle signal, deferred to ux.2b once its prerequisite fields actually exist in code. This premise
still holds as the reason ux.2b must exist; it is not satisfied by ux.2a alone.**

**Premise 2: "outcome/risk/decision-needed signals should lead; raw activity detail should be
secondary" is the user's already-made directional choice, not re-litigated here.** Confirmed at
the superseded plan's Phase 4 gate. Not re-challenged.

**Premise 3 (the one this review must actually test): does `PRODUCT-THESIS.md`'s priority
ordering (approval gate #3 above observability #4) support a *UI information-hierarchy* claim,
or only a *build-sequencing* claim?** These are different things. The thesis document ranks
which **subsystems** to build first (a resourcing/roadmap decision) — it does not, on its own
text, say anything about how an *already-built* observability layer's UI should be organized.
Codex's reframe treats the build-priority ordering as evidence for a UI-hierarchy conclusion;
that's a plausible inference, not one the thesis states directly. **This is a real, load-bearing
nuance** — if the thesis only supports build-sequencing, the reframe's strongest citation is
weaker than it read at the prior gate, and Attention should be scoped as "add outcome/risk
signals prominently" rather than "activity detail no longer matters at the Dashboard level."

**Premise 4: the roadmap's own "North star (2026-07-11)" note — `docs/ROADMAP.md:1148-1150`,
"ux.0/2/1/8 make it live, watchable, chattable, tunable" — is in direct tension with Premise 3's
reframe, not silently compatible with it.** This is the sharpest finding of this Step 0A. The
roadmap's own most recent design-intent statement says the cockpit should be *watchable* — this
increment's own numbered slot in that same roadmap. The reframe doesn't falsify this tension; it
resolves it by choosing *what's watched first* (outcomes/risk) over *what's watched by default*
(raw activity) — but this plan must say so explicitly, not pretend the tension doesn't exist.

**Premise Gate — user confirmation required (this is the one non-auto-decided question):**
**User confirmed: Augment (recommended).** The Dashboard stays the single home view; outcome/
risk/decision-needed signals lead its information hierarchy, activity detail (tool name, idle
timer) stays visible but demoted, not removed or relocated. This materially changes this
review's scope: most of the superseded "Observe" plan's engineering substrate (scheduler-tracked
`AgentSnapshot` fields, the `CallTools`-dispatch-site fix, the FUSE/HTTP stream-pane transport
findings) remains directly reusable — this increment adds NEW risk/outcome fields and changes
which fields the Design phase renders first, rather than replacing the mechanism wholesale.

### 0B — Existing Code Leverage

| Sub-problem | Existing code |
|---|---|
| "What needs a decision right now" | `Approvals` view (`[a]`, `views.rs:153`) — already exists, already discoverable, but is a *separate* view an operator must switch to; Attention's job is surfacing a *count/summary* of pending approvals on the Dashboard itself, not rebuilding Approvals |
| "What's at risk" (budget) | `cred.4`'s spend-cap system + `AgentSnapshot`'s existing `token_budget`/`context_tokens` fields (budget-bar rendering already exists per the superseded plan's Design phase, `MemoryPressure` 75/90 thresholds) |
| "What's blocked/degraded" | `ProviderHealth` (cred.5), `SandboxSummary`/`ServerEnforcement` (p6.8) — provider auth failures, sandbox degradations already tracked, not yet surfaced on the Dashboard row itself |
| "What's erroring" | The superseded plan's already-reviewed `error_count`/`last_error_at` mechanism (Eng-Review-fixed batch-aggregation semantics carry over unchanged) |
| "What's busy vs. hung" (cos-ux-01's original ask) | The superseded plan's `last_activity`/`idle_secs` mechanism, demoted to secondary row position per the premise gate, not removed |
| Live event stream (context/evidence for any of the above) | `pump.rs`'s existing SSE `AppEvent::Flight` (ux.0) + the superseded plan's Eng-Review-fixed FUSE timer-repoll — same infra, reused unchanged |

### 0C — Dream State Mapping

```
CURRENT: Dashboard shows STATUS/TURN/TOKENS/$/AGE only. No risk/outcome signal exists anywhere
  on the home screen. Approvals, budget risk, and provider health each require switching views.
  Operator must actively hunt across 3+ views to answer "does anything need me right now?"

THIS PLAN: Dashboard's home row leads with a single-glyph "needs attention" indicator (derived
  from: pending approval count > 0, budget risk threshold crossed, provider/sandbox degradation,
  OR a hard error) — one glance answers "does ANYTHING need me." Activity detail (LAST-TOOL,
  idle-amber) still renders on the same row, demoted to a secondary column. Selecting an
  attention-flagged row surfaces WHY (approval pending / budget / degraded / errored) without
  a second view-switch for the common case.

12-MONTH IDEAL: the cockpit is an exception-driven console — an operator who trusts the fleet
  glances once, sees nothing needs attention, and does something else. When ux.4 (proactive push)
  lands, the same "needs attention" signal this increment defines becomes the payload for a
  desktop notification / webhook, closing the loop from "must glance at a terminal" to "gets
  told." This increment is the signal-definition layer ux.4 will build on, not a dead end.
```

### 0C-bis — Implementation Alternatives (auto-decided: P1 completeness + P5 explicit)

```
APPROACH A: New `needs_attention` derived field, computed at snapshot-read time from EXISTING
  signals (no new instrumentation) — **⚠ REVISED TWICE. CEO Dual Voices (below) first fixed the
  4→5-signal list to include Idle. Eng Dual Voices (Phase 3) then found the Idle/Error signals
  require fields (`idle_secs`/`error_count`) that DO NOT EXIST anywhere in the actual codebase —
  only in the superseded plan's never-implemented design — and rescoped them OUT of this
  increment (now "ux.2a"), deferred to a follow-on "ux.2b." See "Eng Review — Idle/Error
  rescope" below for the full finding. This is the CURRENT, correct scope: 3 signals, not 5.**
  Summary: `update_snapshot()` (agentd/src/scheduler.rs:2052) derives `attention: Vec<
    AttentionSignal>` (a small typed struct — `reason`, `severity` (Info/Warning/Critical),
    `since: u64`, `evidence: Option<String>` — not a bare enum, per the CEO Dual Voices synthesis)
    by checking, in priority order: (1) pending-approval count for this agent > 0 — **read from
    `state.pending_approvals` directly, NOT the already-`.take(100)`-capped `pending_actions`
    vector on `SchedulerSnapshot` (Eng Dual Voices, Codex finding) — this means `derive_attention`
    runs against scheduler state, not purely as post-processing on the assembled snapshot**, (2)
    budget past the existing `MemoryPressure` hard threshold (`AgentSnapshot.token_budget`/
    `context_tokens`, NOT cred.4 — 0B's original citation was wrong, corrected in Phase 3 Step 0),
    (3) `ProviderHealth` showing `!token_fresh` **alone — NOT `AND last_error present`** (Eng
    Dual Voices, Codex finding: a missing API-key env var sets `token_fresh:false` without
    necessarily setting `last_error`, and the original rule would silently miss that real
    degraded case). Idle and Error are NOT part of this increment (see rescope below).
  Effort: S (narrower than the original S-M estimate, now that Idle/Error's `AgentTask`/
    checkpoint work — the superseded plan's actual remaining scope — is correctly excluded)
  Risk: Low
  Pros: All 3 signals are genuinely zero/near-zero new instrumentation once the 2 Eng-phase bugs
    above are fixed; the "augment, don't replace" premise stays a Dashboard-rendering change plus
    one derived field, not new `AgentTask`/checkpoint plumbing; ships fast, directly testable.
  Cons: Does NOT fully close cos-ux-01 (the founding incident IS the Idle signal, deferred to
    ux.2b) — must be stated plainly to the user/TODOS.md, not softened. Priority order (which
    reason wins when 2+ fire) needed explicit specification — done in the Design phase's
    Reference table 2, since revised there too (Idle/Error rows now marked "ux.2b").
  Reuses: `state.pending_approvals` (existing scheduler state, corrected read site), cred.5
    `ProviderHealth` (existing, corrected rule), `AgentSnapshot`'s existing `token_budget`/
    `context_tokens` + `MemoryPressure` (existing, corrected source).

APPROACH B: New dedicated "risk score" subsystem with its own weighting/config
  Summary: a new `RiskEngine` component computes a continuous 0-100 risk score per agent from
    a weighted combination of signals, configurable via TOML, surfaced as a color gradient
    rather than a binary "needs attention" flag.
  Effort: L
  Risk: Medium
  Pros: More expressive than a binary flag; could rank agents by severity, not just flag/no-flag.
  Cons: New config surface, new subsystem, no existing precedent in this codebase for
    continuous-score UI (every existing signal — budget threshold, error count — is
    binary/discrete); over-engineered relative to cos-ux-01's actual ask (busy vs. hung is a
    binary question) and the reframe's own actual ask (is anything wrong, yes/no).
  Reuses: same 4 subsystems as A, but adds a new aggregation/weighting layer on top.

APPROACH C: Rely entirely on the existing Approvals view; add only a Dashboard badge/count
  Summary: Dashboard gains a single global "N pending approvals" badge (not per-row); per-row
    signals (budget risk, errors, degradation) are NOT added to the Dashboard at all — an
    operator still switches to Approvals/Credentials/Sandbox views for those.
  Effort: XS
  Risk: Low
  Pros: Smallest possible diff.
  Cons: Doesn't actually deliver the reframe's core ask (per-agent, at-a-glance risk/outcome
    visibility) — a global count answers "is anything pending" but not "which agent, why" without
    still hunting across views; doesn't meaningfully improve on the status quo for 3 of the 4
    signal types identified in 0B.
```

**RECOMMENDATION: Approach A.** It is the only one that delivers the reframe's actual ask
(per-agent attention signal, derived from real existing subsystems) without inventing new
tracking infrastructure or a config surface the codebase has no precedent for. B is rejected as
over-engineered relative to what cos-ux-01 and the reframe both actually need (binary questions,
not a continuous score). C is rejected as under-delivering the reframe's stated goal. Auto-
decided: Mechanical (P1 completeness rules out C on under-delivery; P5 explicit rules out B's
unrequested complexity).

### 0D — Mode-Specific Analysis (SELECTIVE EXPANSION)

**Complexity check:** files touched by Approach A: `agentd/src/scheduler.rs` (`update_snapshot`
gains the aggregation — needs read access to the Approvals store, cred.4's spend-cap state, and
cred.5/p6.8's health/sandbox summaries; **open question for Eng phase: does `update_snapshot`'s
current scope already have handles to all 4, or does wiring access to even one of them count as
new coupling** — this is the one item this CEO pass cannot fully resolve without an Eng-level
code read, flagged forward, not hand-waved), `surfaces/src/snapshot.rs` (`needs_attention` field
+ `AttentionReason` enum), `surfaces/src/agents_fs.rs` (FUSE surface for the new field, on the
already-existing per-agent virtual file, not a new one), `agentd/src/management.rs` (JSON field),
`agentctl/src/watch/reader.rs` + `source.rs` (read the field), `agentctl/src/watch/views.rs`
(render — the actual hierarchy reorder), `agentctl/src/watch/app.rs` (row highlight state) — **8
files**, at the smell threshold, not over it (vs. the superseded plan's 10 — smaller because this
is a pure-aggregation layer over already-instrumented subsystems, not new per-tool-call tracking).

**Minimum set:** all 8 are load-bearing — a signal that stops at `snapshot.rs` without reaching
`views.rs` isn't visible; skipping `agents_fs.rs`/`management.rs` breaks one of the two transports.

**Expansion scan:**
- *10x check:* a generalized "risk registry" other subsystems (ux.4, ux.8, future connectors)
  could register signals into, instead of `update_snapshot` hardcoding 4 specific checks.
- *Delight opportunities:* (1) clicking the attention glyph jumps directly to the relevant view
  (Approvals/Credentials/Sandbox) instead of just flagging the row; (2) a Dashboard-level summary
  line ("2 need attention") above the table, not just per-row glyphs; (3) attention reasons
  stack (an agent can be both budget-risk AND erroring) — show the count, not just the first hit.
- *Platform potential:* the generalized risk registry (10x check) is genuine future
  infrastructure for ux.4 (push needs exactly this signal as its payload) and ux.8 (budget alerts).

**Cherry-pick ceremony (auto-decided, P2 boil-lakes + P3 pragmatic):**
- Generalized risk registry (10x check): **DEFERRED to TODOS.md.** Same reasoning as the
  superseded plan's event-bus deferral — genuine platform value, but expands this increment from
  "aggregate 4 existing signals" to "design an extensible registry API" before ux.4 (its actual
  first real consumer) exists to validate the design against. Revisit when ux.4 starts.
- Click-through to relevant view (delight #1): **ADD to this plan's scope.** In blast radius
  (the row-selection/view-switch mechanism already exists per the superseded plan's Design
  Review), directly serves the reframe's core promise (see it, act on it, no hunting), <1 day.
- Dashboard-level summary line (delight #2): **ADD to this plan's scope.** Nearly free once the
  per-row signal exists (a single aggregate count over the same data), and is arguably the single
  highest-value delivery of "trust agents without watching" — an operator can read ONE line
  without even looking at the table.
- Stacked attention reasons (delight #3): **ADD to this plan's scope.** In blast radius (the
  `AttentionReason` enum from Approach A becomes `Vec<AttentionReason>`, not a bigger change),
  and directly prevents a real correctness gap: an agent that's both erroring AND budget-risk
  showing only "errored" would hide the budget problem until the error clears.

**Accepted scope additions:** click-through-to-view, Dashboard summary line, stacked attention
reasons (all 3, per above).

### 0E — Temporal Interrogation

```
HOUR 1 (foundations): implementer needs to know NOW — the exact read-access question flagged in
  0D's complexity check (does update_snapshot already reach the Approvals store / cred.4 / cred.5
  / p6.8, or does each require new plumbing) — this determines whether Approach A is really S-M
  effort or bigger; Eng phase must answer this before implementation starts, not discover it
  mid-build.
HOUR 2-3 (core logic): AttentionReason's priority order when 2+ signals fire (0C-bis's flagged
  open question) — Eng phase fixes this concretely, not left to implementer taste.
HOUR 4-5 (integration): whether click-through-to-view (accepted delight #1) needs new navigation
  state in app.rs or can reuse the existing Enter-to-AgentDetail pattern extended to route
  elsewhere depending on AttentionReason — a real design decision, not obvious from the mockup.
HOUR 6+ (rough edges): what a stale/cleared attention reason looks like transiently (an approval
  gets granted between one poll and the next — does the row flicker attention→clear, or is there
  a brief "resolved" acknowledgment state) — Design phase's job, flagged forward.
```

### 0F — Mode Selection

**SELECTIVE EXPANSION confirmed** (feature enhancement on an existing, shipped system — the
Dashboard/cockpit already exists; this augments its hierarchy, doesn't rebuild it). Matches the
"Augment" premise-gate answer exactly.

### CEO Dual Voices

Dispatched Claude subagent (Agent tool, foreground, independent) and Codex (Bash, parallel —
timing overlap with the subagent is acceptable since neither reads the other's output) against
this plan file. Both returned; findings below.

**CLAUDE SUBAGENT (CEO — strategic independence) — verbatim summary:** Finding A (CRITICAL):
Approach A's `needs_attention` derivation omits `idle_secs`/stalled-progress — the exact
cos-ux-01 incident (agent hangs mid-tool-call, not erroring, not over budget, not degraded, not
awaiting approval) shows NO attention glyph under this plan as originally scoped, directly
contradicting 0C's own dream-state promise ("one glance answers... does ANYTHING need me").
Finding B (HIGH): the reframe's review cost is disproportionate to its engineering delta — the
superseded plan had already cleared 4 full dual-voice review phases reaching a ready-to-implement
state, including a prior, reasoned CEO-phase rejection of this exact reframe; restarting the
whole pipeline for what turns out to be "mostly the same substrate plus an aggregation layer and
a column reorder" is a real process cost worth naming, not silently absorbing. A cheaper
"Approach D" (resequence the superseded plan's already-vetted fields/layout, bolt on an
approvals-count line, build no new aggregation subsystem at all) was never considered in 0C-bis
and should have been, since it most directly matches the "Augment" premise-gate answer at far
lower cost. Premise soundness: 0A's analysis is sound and not glossed over, but the "Augment"
resolution is satisfied cosmetically (the data's still there) not functionally (the signal that's
supposed to trigger attention on it doesn't fire for the founding incident). 6-month regret: (1)
the exact hang scenario recurs, undetected, on a feature whose own changelog says "closes
cos-ux-01"; (2) a standing lesson worth logging — don't discard an already-Eng+DX-reviewed plan
without a materially new finding, not just a change of taste at the gate. Conclusion: needs
another pass — fix Finding A now, and seriously weigh Approach D before continuing.

**CODEX (CEO — strategy challenge) — verbatim summary:** Argues this reframe is still only a
"half-reframe" — it optimizes a terminal home view when the 10x version would define an actual
"exception contract": AgentOS interrupts the operator only for bounded, actionable reasons, and
every non-interruption is defensible. Approach A is "UI aggregation, not product trust" — it
produces a row glyph but doesn't answer the real Chief-of-Staff question ("what did you decide
NOT to interrupt me about, and why was that safe"), and has no severity taxonomy, escalation
policy, suppression policy, or operator SLA. Argues the roadmap's "watchable" north-star tension
is resolved too conveniently ("what's watched first") — proposes "calibrated watchability"
(compact default, fast drilldown, causal evidence) instead of "attention beats observation." The
plan may satisfy neither cos-ux-01's narrow operational-ambiguity ask nor a genuine strategic
risk-routing product sharply, by splitting the difference. Argues deferring the "generalized risk
registry" to TODOS/ux.4 is the WRONG deferral — ux.2 is already the first real consumer, and a
small shared domain contract (`AttentionEvent`/`AttentionState`: reason, severity, owner, source,
first_seen, last_seen, resolved_at, action target, suppressibility, evidence pointer) should be
defined now so ux.4 (push) and ux.6 (evidence) don't each invent their own semantics later. Flags
the current 4 signals as mostly runtime-health checks, not true outcome risks — proposes 6
additional CoS-relevant risk types (stale objective, low-confidence draft, external deadline risk,
duplicate/conflicting agents, data freshness, silent non-action). Competitive risk: a Dashboard
glyph isn't a defensible moat against SaaS CoS competitors; the defensible wedge is signed
receipts/boundary-mediated actions/least-privilege approvals — the plan doesn't connect its work
to that wedge. Flags the 3 accepted delight additions (click-through, summary line, stacked
reasons) as UI polish added before the underlying semantic model is validated.

**Verification against actual plan text (not taken at face value):** Claude subagent's Finding A
verified directly — `docs/plans/ux.2-attention-evidence.md`'s own Approach A text (pre-fix)
listed exactly 4 checks, no idle/stalled trigger; confirmed by direct re-read before writing this
section. **Fixed above, in Approach A's text itself**, not just noted here — a 5th priority-0
check (sustained idle while Running) now leads the list, reusing the superseded plan's
already-fully-specified idle-amber + Waiting-carve-out logic verbatim (zero new design work,
since that logic was already Eng-reviewed once).

**CEO DUAL VOICES — CONSENSUS TABLE:**
```
═══════════════════════════════════════════════════════════════
  Dimension                           Claude  Codex  Consensus
  ──────────────────────────────────── ─────── ─────── ─────────
  1. Premises valid?                   PARTIAL PARTIAL CONFIRMED gap (0A sound; Augment's
                                                               concrete instantiation incomplete)
  2. Right problem to solve?           YES*    NO**   DISAGREE (different reasons — see below)
  3. Scope calibration correct?        NO***   NO****  CONFIRMED gap (opposite directions)
  4. Alternatives sufficiently explored?NO      NO     CONFIRMED gap (both name a missing option)
  5. Competitive/market risks covered? N/A     NO      N/A (Claude didn't assess this angle)
  6. 6-month trajectory sound?         NO      NO      CONFIRMED gap (different failure modes)
═══════════════════════════════════════════════════════════════
* Claude: right problem (cos-ux-01 + reframe), current execution just has a hole (Finding A).
** Codex: reframe itself is still too narrow/UI-centric; the real problem is an exception
  contract, not a Dashboard glyph. *** Claude: scope should SHRINK (Approach D, minimal diff).
**** Codex: scope should DEEPEN (a shared AttentionEvent/AttentionState domain contract, built
  now, not deferred).
```

**This is NOT a User Challenge.** Neither model recommends reversing either of the user's two
already-made gate decisions (reframe toward Attention; Augment not Replace) — both accept those
as given and argue about scope/architecture WITHIN them. But it IS a sharp, well-argued,
opposite-direction disagreement (shrink vs. deepen) on top of a Mechanical, undisputed bug
(Finding A) — handled in two parts:

**Part 1 (Mechanical, fixed now, not a taste call):** Finding A is a concrete, verifiable defect
— the plan's own stated goal is falsified by its own signal list. Fixed directly in Approach A's
text above (5th priority-0 idle/hung check, reusing already-reviewed logic). No reasonable
argument defends shipping without it; both models would agree once shown the gap.

**Part 2 (Taste Decision — shrink vs. deepen — auto-decided, surfaced at gate):** Auto-decided
**middle synthesis, not full concession to either side (P2 boil-lakes + P3 pragmatic):**
- **Adopt Codex's data-modeling suggestion, reject his scope-expansion suggestion.** Upgrade
  `needs_attention: Option<AttentionReason>` to `attention: Vec<AttentionSignal>` (struct with
  `reason`/`severity`/`since`/`evidence: Option<String>`) — cheap (a data-shape choice, not new
  instrumentation), and directly buys forward-compatibility with ux.4 (push payload) and ux.6
  (evidence linking) without inventing a registry, config surface, escalation/suppression policy,
  or operator SLA now. Codex's proposed 6 additional signal types (stale objective, low-confidence
  draft, deadline risk, duplicate agents, data freshness, silent non-action) are rejected for THIS
  increment — none have any existing instrumentation anywhere in this codebase; building
  detection for them means inventing entirely new subsystems (an output-quality evaluator, a
  duplicate-work detector, a staleness tracker) that don't exist and aren't scoped on the roadmap
  anywhere. This is the same "out of blast radius" reasoning that already correctly rejected
  Approach B (continuous risk score) in 0C-bis — logged to TODOS as a genuine future direction,
  not silently dropped, not built now.
- **Reject Claude's full Approach D (skip the new signal entirely, just resequence raw columns).**
  The reframe's actual payoff — answering "does anything need me" from ONE derived signal instead
  of scanning multiple raw columns — is exactly what both of the user's gate decisions were
  asking for; reverting to raw-columns-only would silently undo Augment's point. Claude's
  cost-consciousness is folded in by keeping the new type as small as possible (a struct, not a
  registry) rather than adopting Approach D wholesale.
- **Codex's "calibrated watchability over attention-beats-observation" framing (point 3) is
  already answered by the user's own Augment choice**, not a new correction — activity detail
  stays visible (demoted, not hidden), which IS "compact default, fast drilldown" in Codex's own
  terms. Noted as a clarification, not a fix.
- **Codex's competitive-moat point (point 7)** is directionally correct but not this increment's
  job — the defensible wedge (signed receipts, boundary-mediated actions) is ux.6/cred.3-7's
  territory, already built or already scoped. Logged to TODOS as a forward-link: once ux.6 ships,
  an `AttentionSignal`'s `evidence` field should be able to point at a receipt-chain entry — cheap
  to enable now (the field is already `Option<String>`), not cheap to build the linkage itself
  before ux.6 exists.
- **Claude's Finding B (process-cost)** — logged as a standing process lesson (see
  `feedback_gstack_loop`-style memory), not actioned by reversing course again: the user already
  made this call explicitly at the prior gate; re-litigating "should we have reframed at all" a
  third time would itself be the "stale deliberation" P6 warns against.

**Flagged prominently for Phase 4 gate:** the shrink-vs-deepen disagreement, and this auto-
decision's specific synthesis (typed struct yes, registry/config/SLA no, 6 extra signal types
no) — the user may weigh this differently than the middle path chosen here.

### Error & Rescue Registry

| Failure | Detection | Message shown | Recovery |
|---|---|---|---|
| `update_snapshot` gains new read-access to 3 subsystems it didn't read before (Approvals store, cred.4 spend caps, cred.5/p6.8 health) — one of those reads panics or is unavailable | Each subsystem already has its own established error-handling convention (`MemoryUnavailable`-style graceful degradation, per CONVENTIONS.md) — Eng phase must confirm each read site follows the existing pattern (Option/Result, never unwrap) | N/A if the pattern holds | Eng phase verifies each of the 3 new read sites against the existing convention before merge |
| 2+ `AttentionSignal`s fire simultaneously for one agent | Not a failure — explicitly designed for (0D's "stacked reasons" accepted scope item) | All active signals shown, highest-severity leads | N/A — working as intended |
| `AttentionSignal`'s `evidence: Option<String>` field is populated with an ID that no longer resolves (e.g. an approval that was already granted between poll and click-through) | Click-through (accepted delight #1) attempts to navigate to the referenced view/item | "This item was already resolved" message, not a broken navigation or panic | Falls back to the relevant view's default state (e.g. Approvals list) rather than erroring |
| The idle/hung signal (0-priority, newly fixed) races the same `Waiting`-status carve-out logic already Eng-reviewed in the superseded plan | N/A — reusing that logic verbatim, not reimplementing it | N/A | No new risk; already-tested code path |

### Failure Modes Registry

| # | Failure mode | Blast radius | Severity | Mitigation |
|---|---|---|---|---|
| 1 | Idle/hung signal omitted from `AttentionReason` (Claude subagent Finding A) | Defeats the plan's own stated goal; recurs on the exact founding incident | CRITICAL | Fixed in-plan: 5th priority-0 check added to Approach A, reusing already-reviewed idle-amber logic |
| 2 | Shrink-vs-deepen scope disagreement (CEO Dual Voices Part 2) left unresolved into Design phase | Design phase can't proceed sensibly without knowing the data shape | HIGH | Auto-decided: typed `AttentionSignal` struct (not bare enum, not a registry) — synthesis logged, flagged at gate |
| 3 | Codex's 6 additional signal types (stale objective, low-confidence draft, etc.) silently expected by a future reader of this plan | None of these have existing instrumentation; building them would be new-subsystem-scale work | MEDIUM | Explicitly rejected for this increment with reasoning, logged to TODOS as future direction, not silently ignored |
| 4 | Process cost of re-running the full 4-phase pipeline for what turns out to be a modest engineering delta (Claude subagent Finding B) | Time/token cost this session; a precedent for future taste-driven restarts | MEDIUM | Logged as a standing process lesson, not reversed (user's restart decision stands) |
| 5 | Competitive-moat gap (Codex point 7) — Attention alone isn't a differentiator | Low direct blast radius for this increment; real for the product's broader positioning | LOW | Logged to TODOS as a forward-link (evidence field → future ux.6 receipt), not this increment's job |

### 11-Section Deep Review (per `sections/review-sections.md`) — scoped to the actual delta

Sections 1-2 (Architecture/Data model), 6 (Tests), and 7 (Performance) already received deep,
code-verified treatment in the superseded plan for the SHARED substrate (scheduler-tracked field
mechanics, FUSE/HTTP transport, checkpoint/restore, batch-aggregation semantics) — that analysis
carries forward unchanged (it wasn't reframe-specific, it was implementation-mechanism-specific,
and the mechanism is unchanged). This section covers only what's genuinely NEW to Attention.

**Section 1 (Architecture) — new delta:** `update_snapshot` gains 3 new read dependencies
(Approvals store, cred.4, cred.5/p6.8) it didn't have before — a real, new coupling increase
(unlike the superseded plan's fields, which only touched `AgentTask` itself). Eng phase must
verify each of these 3 subsystems exposes a read-only, panic-safe accessor `update_snapshot` can
call without taking a lock the scheduler's own tick already holds (a genuine new risk class the
superseded plan didn't have, since it never read cross-subsystem state). Flagged forward to Eng
Review, not resolved here (requires reading `credential/mod.rs` and the Approvals store's actual
locking model).

**Section 2 (Code quality) — new delta:** the `AttentionSignal` struct (reason/severity/since/
evidence) needs one canonical priority-ordering function (5 checks → the highest-severity one
wins for row-color, all shown in the stacked list) — one function, not five call sites each
picking their own precedence, per DRY.

**Section 3 (Security):** no new attack surface — `evidence: Option<String>` is operator-facing
text already sourced from existing, already-trusted internal state (approval IDs, provider
names), not user/agent-controlled input requiring new validation.

**Section 4 (Data/UX edge cases):** what does an agent with ZERO active signals render as? (An
explicit "OK" state, not a blank cell — matches the superseded Design phase's "empty states are
features" principle.) Flagged for Design phase.

**Section 5 (Cross-crate quality):** none — no OTEL-style dedup question here, this is
`agentd`-internal aggregation only.

**Sections 8-10 (Observability/Deployment/Future):** unchanged from the superseded plan's
analysis (same flight-recorder-everything convention, same plain revert/no-migration rollback
posture, same reversibility — this increment doesn't change any of those properties).

**Section 11 (Design):** UI scope confirmed (this triggers Phase 2, next).

### Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|-----------------|-----------|-----------|----------|
| 1 | Reframe premise gate (prior plan's Phase 4) | Reframe toward Attention | User Challenge → user chose reframe | — | User's explicit choice, not auto-decided | Keep "Observe" as-is |
| 2 | This plan's premise gate | Augment, not Replace | User gate | — | User's explicit choice | Replace Dashboard with an Approvals-shaped home view |
| 3 | Naming | "Attention" not "Attention & Evidence" | Mechanical | P5 explicit | "Evidence" collides with already-scoped ux.6 | Keep Codex's original phrase, confuse two roadmap items |
| 4 | 0C-bis | Approach A (aggregation layer over existing signals) | Mechanical | P1+P5 | Only approach matching Augment without over/under-building | B (risk score, over-engineered), C (badge-only, under-delivers) |
| 5 | 0D | 3 expansion candidates: registry deferred, 3 delights accepted | Taste (cherry-pick) | P2 boil-lakes, P3 pragmatic | Registry premature without a 2nd consumer; delights cheap+high-value | Building the registry now |
| 6 | CEO Dual Voices, Finding A | Add idle/hung as priority-0 `AttentionReason` | Mechanical | P1 completeness | Confirmed bug — plan's own stated goal was falsified by its own signal list | Shipping without it |
| 7 | CEO Dual Voices, Part 2 | Typed `AttentionSignal` struct (reason/severity/since/evidence); NOT a full registry/contract-v1 | Taste (shrink-vs-deepen disagreement) | P2 boil-lakes, P3 pragmatic | Middle synthesis — cheap forward-compatibility without inventing SLA/suppression/escalation policy this increment doesn't need yet | Claude's full Approach D (no new signal at all); Codex's full "Attention contract v1" (registry + policy layer) |
| 8 | CEO Dual Voices | Codex's 6 additional signal types (stale objective, etc.) | Taste (Codex disagreement) | P2 boil-lakes | Zero existing instrumentation for any of them; would require new subsystems, not aggregation | Building detection for any of the 6 now |
| 9 | CEO Dual Voices | Process-cost concern (Claude Finding B) | Taste, logged not reversed | P6 bias-toward-action | User's restart decision already made explicitly; re-litigating it a 3rd time is the stale deliberation P6 warns against | Reversing the reframe decision again |

### TODOS.md updates (proposed, not yet written — pending Phase 4 gate)

1. Generalized risk/attention registry with escalation/suppression policy (Codex's "Attention
   contract v1") — revisit when ux.4 (push) needs a real 2nd consumer to validate the design
   against. (P2, L effort)
2. Codex's 6 additional outcome-risk signal types (stale objective, low-confidence draft,
   deadline risk, duplicate/conflicting agents, data freshness, silent non-action) — each would
   need its own detection subsystem; none exist today. (P3, L effort, speculative)
3. `AttentionSignal.evidence` → ux.6 receipt-chain linkage, once ux.6 ships. (P3, S effort,
   blocked on ux.6)
4. Standing process lesson: don't discard an already-Eng+DX-reviewed plan without a materially
   new finding — a change of taste at the final gate should trigger a scoped amendment, not
   automatically a full pipeline restart, unless the reframe is large enough to invalidate the
   prior Design/Eng work (this one was, since the information hierarchy itself changed — but the
   next one might not be). (informational, logged for future /autoplan runs)

### Completion Summary

```
+====================================================================+
|            MEGA PLAN REVIEW — COMPLETION SUMMARY (CEO)             |
+====================================================================+
| Mode selected        | SELECTIVE EXPANSION                          |
| Premise gates        | 2 passed (reframe: prior plan; Augment: here)|
| Step 0               | 4 premises named; 1 real nuance found (P3/P4)|
| 0C-bis               | 3 approaches; A chosen, revised post-review  |
| 0D                    | 1 deferred, 3 accepted (all cheap, in-radius)|
| CEO Dual Voices       | 1 CRITICAL bug found+fixed; 1 taste synthesis|
| Error/Failure registries | 4 + 5 rows, all mitigated or logged      |
| Sections (delta-scoped) | 2 new findings (Section 1 coupling, Section 2 DRY); rest unchanged from superseded plan |
+====================================================================+
```

## PHASE 1 COMPLETE

The CEO review's most important outcome: a dual-voice pass caught that this plan's own headline
mechanism — the signal meant to answer "does anything need me" — didn't fire for the exact
incident (cos-ux-01) that motivates the whole increment. Fixed directly in Approach A before
Design review inherits a broken spec. The two reviewers also diverged sharply on how far to take
the reframe (Claude: shrink to a minimal resequence; Codex: deepen into a shared domain contract)
— resolved with a middle synthesis (a small typed struct, not a registry) that's flagged
prominently for the user's final review rather than silently picked. Proceeding to Phase 2
(Design Review) — UI scope confirmed.

## Design Review (Phase 2)

### Step 0 — Design Scope Assessment

**UI scope confirmed** (new Dashboard column/glyph, new color/priority hierarchy, a click-through
interaction, a summary line). **Reused from the superseded plan's Design Review, not redone:**
the overall screen layout (Dashboard table → stream pane → AgentDetail), the `--plain` parity
approach, the terminal-width degradation strategy, the freeze/scope interaction model, the
in-TUI legend convention — none of those change under Attention; only the Dashboard row's
information *hierarchy* and *leading signal* change. **Genuinely new for this phase:** how the
`AttentionSignal` struct (reason/severity/since/evidence, stacked) renders as a leading glyph +
color, how click-through navigation works, and how the new Dashboard summary line composes with
the existing budget-bar/status-dot conventions.

### Visual mockup — ASCII, v2 (v1 found self-contradictory by the Design dual-voice pass — see
"Design Review revises the mockup" below; this version fixes it, verified self-consistent)

**Signal → color → stacked-line template (the table Pass 3 should have included from the start):**

| Signal | ATTN glyph | Color | Stacked-line template | Built this increment? |
|---|---|---|---|---|
| Approval pending | `⚠` | Cyan (matches existing `waiting`-status convention — "needs your input," not "broken") | `⚠ approval pending · {since} ago` | **ux.2a — yes** |
| Budget risk | `⚠` | Amber (matches existing `MemoryPressure` threshold convention) | `⚠ budget risk ({pct}%) · {since} ago` | **ux.2a — yes** |
| Degraded (credential/provider) | `⚠` | Red | `⚠ {provider} degraded · {since} ago` | **ux.2a — yes** |
| Clean (evaluated, zero signals) | `·` (dim) | Grey/dim | none | **ux.2a — yes** |
| Evaluation-unavailable (a read dependency failed for this agent) | `?` | Amber | `? evaluation unavailable ({subsystem}) · {since} ago` | **ux.2a — yes** |
| Idle/stalled | `⚠` | Amber | `⚠ possibly stuck · {idle_secs}s ago` | **ux.2b — design only, not built** (needs `AgentTask` fields that don't exist yet; Eng Dual Voices rescope, below) |
| Error | `⚠` | Red | `⚠ {error} · {since} ago` | **ux.2b — design only, not built** (same reason) |

**One glyph column, three possible states — never a blank cell (closes the Design dual-voice
CRITICAL finding: "clean" was previously defined as an absence, indistinguishable from
"not evaluated" or "evaluation failed").** STATUS (the existing green/cyan/red/grey dot —
running/waiting/error/terminated) is a *different axis* than ATTN and stays a separate, labeled
column — the two were conflated into one ambiguous glyph in v1.

**⚠ v3 CORRECTION (DX Review, both models independently found this): v2's mockup — and the
superseded plan's before it — was drawn against a Dashboard that was never actually shipped.**
Verified directly: `agentctl/src/watch/views.rs:105-121`'s real `render_dashboard` header is
`Agent ID | Status | Context | Budget | Tools` — five columns, no `TURN`/`LAST-TOOL`/`TOKENS`/
`$`/`AGE` anywhere. `agentctl/src/watch/reader.rs:114-133`'s `AgentInfo` struct confirms this:
it has `id`/`status`/`status_detail`/`context_tokens`/`budget`/`tools`/`parent_id`/`sandbox`/
`egress_brokered`/`egress_denied`/`tier`/`isolation`/`pid` — no `turn`, no `last_tool`, no `age`.
Those columns were always aspirational (the master plan's rough-scope text, never built — the
superseded plan's own Eng Review already confirmed "no code was written" for them). **The
"Augment, not Replace" premise means augmenting the REAL 5-column table, not the imagined
9-column one** — this is actually simpler, since none of ux.2a's 3 signals need a "last tool
call" text field to render at all.

**Mockup rows below use ux.2a's 3 real signals (Approval, Budget, Degraded) plus Clean and
Evaluation-unavailable, against the real 5-column baseline. No Idle/Error examples (ux.2b). The
AgentDetail mockup further below still shows an idle example as a labeled ux.2b preview — that
one is unaffected by this correction, since `AgentDetail`'s own layout was never claimed to match
any existing code, only described in prose and now drawn fresh.**

```
┌─ agentctl watch ── Dashboard ─────────────────────────────────────────────┐
│ 2 need attention                                                          │  ← NEW summary line,
│                                                                            │    always rendered
│ Agent ID    Status            ATTN  Context  Budget       Tools           │  ← NEW column, right
│ gmail-09    running           ⚠     1200     [==..] .02   3               │    after Status —
│             ⚠ approval pending · 2m ago                                   │    leads, per hierarchy
│ curator-07  awaiting_child    ·     1100     [===.] .01   2               │  ← clean, no signal
│ scout-a     running           ⚠     890      [=...] .01   4               │
│             ⚠ google_oauth degraded · 5m ago                              │  ← Degraded: provider
│ librarian   running           ·     620      [=...] .01   2               │    token stale
│ auditor-3   running           ?     410      [=...] .01   3               │  ← evaluated, clean
│             ? evaluation unavailable (credential_gateway) · 12s ago       │  ← credential-health
└────────────────────────────────────────────────────────────────────────────┘    read timed out —
  ↑/↓ select  Enter: view detail  [s]ystem  [t]opology  [m]emory  [n]ew  [a]pprove  NEVER "clean"
  [c]reds  [i]nspector  q quit
  Legend: ⚠ needs attention   · checked, clear   ? couldn't check
```
**Footer is a genuine 2-row layout change, not free (DX Review, Codex finding).** The real
`render_dashboard` reserves exactly one footer row (`Constraint::Length(1)`, `views.rs:47`) —
adding the legend line means the layout gains a second footer row (`Constraint::Length(2)` split
into hints + legend, or two adjacent `Length(1)` rows), a small but real change to
`header_footer_layout`, not something that fits inside the existing single line. Stated
explicitly here so Eng phase doesn't discover it mid-build.

**Click-through copy softened (DX Review, Codex finding): "jump to attention source" overpromised
discoverability** for the 2 of 3 signal types that route to unscoped, fleet-wide views (Approval
→ `View::Approvals` unscoped; Degraded → `View::Credentials`/`View::System` unscoped) — an
operator reading "jump to attention source" in the footer would reasonably expect to land
exactly on the relevant item, which isn't true yet for those two. Changed to the more modest
"Enter: view detail" — accurate for all cases (AgentDetail is genuinely agent-scoped; the other
two are "at least you're in the right view," communicated honestly via the stacked reason line
and Reference table 2's own fallback column, not oversold in the footer's own copy.

**Legend line added (DX Review, both models flagged its absence):** matches this codebase's own
established convention for explaining color/glyph density — the System view already does exactly
this (`views.rs:340-345`, a dim-gray line under the relevant status). v1/v2 claimed to reuse this
convention but never actually added an equivalent line for the 3 new glyphs; fixed here.
**`[i]nspector` added to the footer (free, same line already being touched):** pre-existing gap
(`i` is bound, `mod.rs:475`, but was never in the footer hints) — not introduced by this plan,
fixed opportunistically since the footer is already being edited for this increment's own reason.

**Information hierarchy (unchanged reasoning from v1/v2, now depicted against the REAL table):**
(1) the always-visible summary line; (2) the ATTN column, leading (right after Status, the
table's actual first content column); (3) Context/Budget/Tools, unchanged, not demoted — there
was never a "LAST-TOOL" column to demote, since it never existed. The Augment premise here means:
add ATTN + the summary line to what's actually there today, don't invent a bigger table first.

### 7-Pass Review (delta-scoped; scores below are POST-v3-fix, reflecting the real-baseline
mockup — the Design dual-voice section documents what v1/v2 actually scored and why, per this
plan's own established "no grade inflation" discipline from the superseded plan)

**Pass 1 (Information Hierarchy) — 9/10.** Reasoning unchanged; the "counts agents not signals"
resolution stands. The v3 mockup now depicts the hierarchy against the table that actually ships.

**Pass 2 (Interaction States) — 9/10 (was self-contradictory in v1, fixed).** Three explicit
states, three distinct renderings: Clean (dim `·`, evaluated, nothing active), Attention (`⚠` +
stacked line(s)), Evaluation-unavailable (`?`, amber, its own stacked line — a failed read on any
of the 3 new subsystem dependencies renders THIS, never silently collapses into Clean). This
directly closes the CRITICAL gap: in a system whose premise is "silence means trust," a signal
source failing silently and rendering as "nothing wrong" would invert that promise — now it
can't, because Clean and Evaluation-unavailable are visually distinct, always.

**Pass 3 (Specificity) — 9/10 (was internally inconsistent in v1, fixed).** All 5 signal types
now have an assigned color (table above) and a single consistent stacked-line template each; the
v2 mockup's rows now actually match their captions (v1's idle row broke its own stated template).

**Pass 4 (Emotional arc / journey) — unchanged from v1, still sound.** Summary line → flagged
rows → click-through → resolution. One addition: the Evaluation-unavailable state's journey is
"operator sees `?`, knows the *signal* (not the agent) is untrusted right now, can still check
the agent directly via AgentDetail" — a distinct, honest arc from "clean" or "attention."

**Pass 5 (Click-through target resolution) — REVISED (was CRITICAL: 2 of 3 routing targets don't
exist as described; verified against actual code — `ApprovalsViewState` has no agent-filter
field, `agentctl/src/watch/approvals.rs:14-22`; there is no `View::Sandbox` variant, only
`View::System`/`View::Credentials`, both global/fleet-wide, `views.rs:274,1041`).** Corrected
spec: `Enter` on a flagged row routes based on the SAME canonical priority function Section 2
(CEO Dual Voices) already calls for — row-color and click-through resolution are explicitly the
SAME resolution, not two independently-specified rules that could disagree. Routing, honestly
scoped to what exists today:
- Approval-reason → `View::Approvals`, **unscoped** (the existing global list; no agent
  pre-filter this increment — `ApprovalsViewState` gaining a `filter_agent_id: Option<String>`
  is real, new, valuable follow-up work, NOT free, logged to TODOS, not silently implied as
  already-scoped like v1 did).
- Degradation-reason from sandbox enforcement (p6.8) → `View::System`, unscoped.
- Degradation-reason from credential/provider health (cred.5) → `View::Credentials`, unscoped
  (split from v1's blended "Sandbox/Credentials" bucket — these are two distinct signal sources
  per 0B's own table and should route to two distinct existing views, not one ambiguous one).
- Idle/error-reason → `AgentDetail` (unchanged default, this one IS already agent-scoped today).
- Evaluation-unavailable → `AgentDetail` (same as idle/error — the safest fallback when a signal
  source itself can't be trusted).
- No active signal → `AgentDetail` (unchanged default).
**Honest compromise, stated explicitly, not buried:** for 2 of 6 destinations (Approvals,
System/Credentials), click-through lands on an unscoped, fleet-wide view this increment — the
"no hunting" promise is only fully delivered for idle/error/evaluation-unavailable signals
until the scoping follow-up (TODOS) lands. This is a real, acknowledged partial delivery, not a
silent gap.

**Pass 6 (Responsive/width) — corrected (v3: no `LAST-TOOL` column exists to truncate).** The
real table's 5 columns already use `Constraint::Min(20)` (Agent ID) + 4 fixed-width columns
(`views.rs:132-138`) — adding one more `Constraint::Length(1)` for ATTN doesn't meaningfully
threaten width the way the superseded plan's 9-column table did. No new drop-order logic is
needed this increment (unlike the superseded plan, which genuinely needed one); if a future
column is added (e.g. ux.2b's eventual `LAST-TOOL`), that increment specifies its own drop order
against whatever the table looks like by then — not invented speculatively here.

**Pass 7 (Accessibility) — corrected twice this round (DX Review, both models).** `--plain`
renders, per agent row: **`[!]` for any active signal, `[OK]` for Clean, `[?]` for
Evaluation-unavailable** — standardized on `[OK]`, not the `[.]` this Pass originally used, which
directly contradicted Reference table 1's own `[OK]` (Codex's finding: two places in this same
document specified two different plain-text markers for the identical Clean state). Every marker
is now **followed by the reason text inline**, matching the pattern `AgentDetail`'s `--plain`
spec already uses (`⚠ possibly stuck · 34s ago` becomes, in `--plain`, `[!] approval pending ·
2m ago` — not bare `[!]`). With only 3 signal types now (down from the originally-imagined 5),
collapsing all of them to a bare `[!]` would make Approval, Budget, and Degraded indistinguishable
in plain mode — a real, avoidable information loss the Claude subagent's DX pass caught. Fixed:
marker + reason text both render, and the marker set is now internally consistent across the
whole document, not just within this Pass.

### Design Dual Voices

Dispatched Claude subagent (Agent tool, foreground) and Codex (Bash, parallel) against this plan
file's Design section (v1 of the mockup, before the fixes above), with the CEO Dual Voices
findings included in Codex's prompt only (subagent stays independent per convention).

**CLAUDE SUBAGENT (design — independent review) — verbatim summary:** Verified against actual
code (`app.rs`, `views.rs`, `approvals.rs`), not taken at face value. CRITICAL: Pass 5's
click-through targets don't exist as described — `ApprovalsViewState` has no agent-filter field;
there is no `View::Sandbox` variant (sandbox info lives inside global `View::System`);
`render_credentials()` is fleet-wide, unscoped. 2 of 3 routing branches land the operator in an
unscoped list, reintroducing the "hunt across views" problem the reframe exists to eliminate.
CRITICAL: Pass 2's "clean" state was defined as a literal absence (no glyph, no line) — visually
identical to "not evaluated" or "evaluation silently failed," which inverts the product's own
"silence means trust" premise given the CEO phase's own Error Registry admits the 3 new read
dependencies can fail. HIGH: the v1 mockup contradicted its own captions — the leading column
showed `⏳`/`!`, never the `⚠` the prose promised; an unlabeled `●` status-dot column had no
header entry. MEDIUM-HIGH: color specified for only 2 of 5 signal types; the stacked-line format
was violated by the mockup's own idle-row example. Recommendation: redraw before proceeding to
Design Dual Voices consensus (i.e., before this very section) — the findings are code-verifiable
defects, not taste.

**CODEX (design — UX challenge) — verbatim summary:** Confirms the information hierarchy itself
is right (summary → attention → demoted activity → budget/age) and the "counts agents not
signals" resolution is correct. But: Pass 2's "reused verbatim" treatment of interaction states
is insufficient — the NEW summary line has its own untreated states (loading, empty-fleet,
partial-data, one-subsystem-erroring, stale-snapshot) that v1 never addressed, only the per-row
clean state was covered. Pass 5's routing rule has 3 real gaps beyond the subagent's (already-
verified) code-existence problem: (a) no tie-break rule when 2+ signals share the same severity;
(b) **"highest severity" is not the same as "most actionable"** — an approval-pending signal may
be lower "severity" than a raw error but is uniquely the ONE signal type the operator can
directly resolve by clicking through, and severity-only routing can send the operator to a less
useful destination; (c) "Sandbox/Credentials" is genuinely two destinations for two distinct
signal sources (p6.8 vs. cred.5), independently confirming the subagent's code finding from a
different angle (UX coherence, not just code-existence). Pass 7's `--plain` spec still leaked a
raw `⚠` glyph into the stacked-reason example, contradicting its own "plain mode uses text
markers" claim. Requires two concrete reference tables before Eng can build from this: a
per-STATE table (summary line / row marker / inline block / Enter behavior / `--plain` output)
and a per-`AttentionReason` routing table (severity / display label / marker / primary route /
evidence route / fallback / tie-break rank). Conclusion: directionally right, not
implementation-ready without those two tables.

**Verification / synthesis:** Both models independently converged on Pass 2 (states) and Pass 5
(routing) as the two weakest sections, from genuinely different angles — the subagent verified
Pass 5's routing targets don't exist in code; Codex verified Pass 5's routing *logic* has a real
UX flaw (severity ≠ actionability) even where the targets DO exist. Both are real, not
overlapping duplicates of the same finding. The mockup fixes above (v2) already close: the
clean/attention/evaluation-unavailable 3-state ambiguity (both models' concern), the
mockup-contradicts-its-own-caption problem (subagent), the 5-signal color table (subagent), and
splits "Sandbox/Credentials" into two distinct routing branches (both models, converging from
different angles). **Not yet closed by the v2 mockup fix, closed here:**

**Fix 1 — severity ≠ actionability (Codex's sharpest, unresolved point).** Row COLOR stays
severity-driven (matches the color table above — an operator scanning color wants "how bad,"
which IS severity). **Click-through ROUTING is actionability-driven, not severity-driven**:
approval-pending, if active at all, always wins the Enter-routing decision regardless of what
else is active and regardless of relative severity — it is the one signal type resolved by a
direct operator action, so it should never be routed *around*. This is a one-line rule, not a
new subsystem: routing priority is `Approval > Degraded > Error > Idle > EvaluationUnavailable`
(deliberately NOT the same order as color-severity), stated explicitly as two independent
orderings from now on, per Codex's finding that conflating them was the actual bug.

**Fix 2 — tie-break within the same severity/routing tier.** Deterministic, not implementer's
choice: ties break by the fixed enum declaration order already established in Approach A
(Idle → Approval → Budget → Degraded → Error) — no new mechanism, reuses the order the struct
already has.

**Fix 3 — fleet-wide (not just per-agent) partial-data semantics for the summary line.** The
per-agent `?` (evaluation-unavailable) state, fixed above, does NOT cover the case Codex raised:
if a signal SOURCE fails for the whole fleet at once (e.g. the Approvals store itself is
unreachable), "N need attention" could read as a confident count when it's actually incomplete.
**Fix:** the summary line becomes `{N} need attention` normally, or `{N} need attention · {M}
unavailable` when `M > 0` agents are in the evaluation-unavailable state — never silently drops
the caveat. `0 need attention` with `M > 0` unavailable is never rendered as bare "0 need
attention" (which would read as an all-clear it can't actually back up).

**Reference table 1 — per-STATE (Codex's required fix, delivered):**

| State | Summary line | Row ATTN marker | Inline block | Enter behavior | `--plain` |
|---|---|---|---|---|---|
| Loading (first poll not yet returned) | not rendered (table itself shows the existing "waiting for first snapshot…" placeholder, unchanged from ux.9) | N/A | N/A | N/A | same placeholder text |
| Empty fleet (0 agents) | not rendered (existing empty-cockpit state, unchanged from ux.9) | N/A | N/A | N/A | unchanged |
| Clean (evaluated, 0 signals) | counts toward the denominator only | `·` dim | none | → `AgentDetail` | `[OK]` |
| Attention (≥1 signal) | counts toward `{N}` | `⚠` (color per signal table) | `⚠ {reason} · {since}` | per Fix 1's routing priority | `[!] {reason} · {since}` (marker + reason text, not bare — Pass 7's DX-review fix) |
| Evaluation-unavailable (this agent's read failed) | counts toward `{M}` in ` · {M} unavailable` | `?` amber | `? evaluation unavailable ({subsystem}) · {since}` | → `AgentDetail` | `[?] evaluation unavailable ({subsystem}) · {since}` |
| Fleet-wide subsystem failure (Fix 3) | `{N} need attention · {M} unavailable` — never bare `{N} need attention` when `M>0` | (each affected agent shows `?` individually — same as the row-level state above, aggregated) | — | — | — |

**Reference table 2 — per-`AttentionReason` (Codex's required fix, delivered):**

**Routing priorities renumbered to be globally unique (DX Review, Claude subagent finding: v2 had
Degraded and Error both at "2," a real duplicate that would confuse ux.2b's implementer even
though it was harmless today with Error unbuilt). Each reason now has one number, ordered by
actionability, most-actionable first:**

| Reason | Severity (color) | Display label | Routing priority (unique now) | Primary route | Fallback if unscoped | Built this increment? |
|---|---|---|---|---|---|---|
| Approval pending | Info (cyan) | "approval pending" | **1 (highest — always wins, per Fix 1)** | `View::Approvals`, unscoped this increment | Operator manually finds the row in the unscoped list (TODOS: add `filter_agent_id`) | **ux.2a — yes** |
| Degraded — credential (cred.5) | Critical (red) | "credential degraded" | 2 | `View::Credentials`, unscoped | Operator manually finds the provider row | **ux.2a — yes** |
| Degraded — sandbox (p6.8) | Critical (red) | "sandbox degraded" | 3 | `View::System`, unscoped | Operator manually finds the agent's section | **Deferred — not Eng-verified this pass.** `SandboxSummary` is a startup-time, largely-static snapshot (set once in `main.rs`), unlike `ProviderHealth`'s dynamic per-cycle refresh — whether it's a meaningful per-agent *runtime* attention trigger at all needs its own Eng check, not assumed. Logged to TODOS, not built until verified. |
| Error | Critical (red) | "{error text}" | 4 | `AgentDetail` | N/A — already agent-scoped | **ux.2b — needs `AgentTask` fields that don't exist yet** |
| Budget risk | Warning (amber) | "budget risk (N%)" | 5 | `AgentDetail` (budget bar is already per-agent, no separate view needed) | N/A | **ux.2a — yes** |
| Idle/stalled | Warning (amber) | "possibly stuck" | 6 | `AgentDetail` | N/A — already agent-scoped | **ux.2b — same reason as Error** |
| Evaluation-unavailable | Warning (amber) | "evaluation unavailable ({subsystem})" | 7 (lowest — never routes anywhere but the safe default) | `AgentDetail` | N/A | **ux.2a — yes** |

**CEO-parallel consensus table (Design):**
```
═══════════════════════════════════════════════════════════════
  Dimension                           Claude  Codex  Consensus
  ──────────────────────────────────── ─────── ─────── ─────────
  1. Information hierarchy right?      YES     YES    CONFIRMED
  2. States fully specified?           NO*     NO**   CONFIRMED gap (different angles, both fixed)
  3. Click-through fully specified?    NO***   NO**** CONFIRMED gap (different angles, both fixed)
  4. Specificity vs. generic patterns? NO*****  PARTIAL CONFIRMED gap (fixed: 2 reference tables)
  5. --plain/accessibility complete?   N/A      NO     Fixed (glyph leak removed, tables added)
═══════════════════════════════════════════════════════════════
* Subagent: mockup contradicted its own state captions. ** Codex: summary-line's OWN states
(loading/partial/stale) were never addressed, only per-row clean was. *** Subagent: 2 of 3
routing targets don't exist in code. **** Codex: severity-vs-actionability conflation, no
tie-break rule, blended destination. ***** Subagent: color/template inconsistency undercut the
plan's own claimed 9/10 specificity score.
```

**Not a User Challenge** — both models found concrete, fixable specification gaps in a design
document, not a disagreement about the increment's direction. All findings fixed directly above
(mockup v2, Pass 2/5/7 rewrites, 2 reference tables, the severity-vs-actionability routing fix).

### AgentDetail mockup (gap found post-review — Enter routes here for several signal types per
Reference table 2, but its own layout was never actually drawn, only described in prose;
adding it now rather than leaving Eng phase to invent it)

**Note: the example below uses an Idle signal ("possibly stuck") to show the attention strip's
general shape — Idle itself is ux.2b, not built this increment (Eng Dual Voices rescope). The
strip mechanism is identical regardless of which signal fires; this is a preview of how ux.2b
plugs into the same UI, not a claim that this exact row ships now.**

```
┌─ agentctl watch ── AgentDetail: gmail-09 ─────────────────────────────────┐
│ ⚠ possibly stuck · 34s ago                                                │  ← persistent attention
│                                                                            │    strip: renders iff
│                                                                            │    ≥1 signal active,
│                                                                            │    ALWAYS the top line
│ STATUS running   TURN 5   TOKENS 1.2k/50k   BUDGET [==..] .02             │
│ infer 2.3s · tool 34.1s (in flight) · idle 34s                            │
│                                                                            │
│ Activity                                                                  │
│  14:32:07  → oauth_call_api(url=".../gmail/v1/...")     (still running)   │
│  14:31:40  ✓ web_search("competitor pricing") → 12 results                │
└────────────────────────────────────────────────────────────────────────────┘
```
Clean-agent case: the attention strip line is **absent entirely** (not rendered blank) — same
"silence is a real state, not a missing feature" rule as the Dashboard (Pass 2's 3-state fix).
Multiple active signals (e.g. idle AND budget-risk) stack as multiple strip lines, highest-
severity first — same ordering rule as the Dashboard's stacked reason lines (Design Fix 2's
tie-break order), not a separately-invented rule for this view. `--plain` renders the same strip
as a `[!] {reason}` line per active signal, immediately after the existing status/context/
budget line, before the activity block — matching the marker convention from Reference table 1.


### Design Review — "NOT in scope"

Per-view agent-scoping for `Approvals`/`System`/`Credentials` (the `filter_agent_id` follow-up
from Fix table 2) — real, valuable, but new surface area outside this increment's 8-file
estimate; logged to TODOS, not built now. Full "Attention contract v1" (escalation/suppression
policy, operator SLA) — already deferred at the CEO phase, unchanged here.

### Design Review — TODOS.md updates (proposed)

5. `ApprovalsViewState` gains `filter_agent_id: Option<String>` (pre-select/filter to one
   agent's pending approvals) — closes the "unscoped click-through" honest compromise in
   Reference table 2. (P2, S-M effort)
6. `View::System`/`View::Credentials` gain an analogous "jump to and highlight this agent's
   section" behavior for degradation click-through. (P2, S-M effort, same motivation as #5)
7. Screen-reader-stable text labels (Codex's accessibility sub-point) beyond `[!]`/`[.]`/`[?]`
   markers, if TTS/screen-reader use of `agentctl watch` is ever reported as a real need — not
   speculative work without a real user report. (P3, informational)

### Design Completion Summary

```
+====================================================================+
|          MEGA PLAN REVIEW — COMPLETION SUMMARY (DESIGN)             |
+====================================================================+
| Mockup            | v1 → v2 (2 CRITICAL, 1 HIGH, 2 MEDIUM-HIGH fixed) |
| Pass 1 (Hierarchy)| 9/10 — confirmed sound both models                |
| Pass 2 (States)   | 9/10 (was self-contradictory) — 3-state fix + fleet-wide caveat (Fix 3) |
| Pass 3 (Specificity)| 9/10 (was inconsistent) — full 5-signal color/template table |
| Pass 4 (Journey)  | unchanged, sound                                  |
| Pass 5 (Routing)  | REVISED — 2 fake destinations fixed + severity-vs-actionability fix (Fix 1) + tie-break (Fix 2) |
| Pass 6 (Width)    | unchanged, sound                                  |
| Pass 7 (Access.)  | glyph leak removed from --plain spec               |
| Design Dual Voices | 2 models, both independently found real gaps; 2 reference tables delivered |
+====================================================================+
```

## PHASE 2 COMPLETE

The Design phase's own dual-voice pass caught that the ASCII mockup — the artifact the review
exists to check — contradicted its own captions, and that the click-through routing spec named
two destinations (`View::Sandbox`, agent-scoped `Approvals`) that don't exist in the actual code
today. Both were fixed directly: a redrawn, internally-consistent mockup with a 3-state glyph
model (clean/attention/evaluation-unavailable, closing a real "silence means trust" inversion
risk), and an honestly-scoped routing table that separates severity (drives color) from
actionability (drives routing — approval-pending always wins click-through, regardless of
severity). Proceeding to Phase 3 (Eng Review).

## Eng Review (Phase 3)

### Step 0 — Scope Challenge (resolves the CEO phase's flagged open question, code-verified)

**CEO Section 1 flagged: "does `update_snapshot` already reach the Approvals store / cred.4 /
cred.5 / p6.8, or does wiring access to even one of them count as new coupling?" Answered now,
directly from the code, more favorably than the CEO phase worried:**

- **Approvals: already fully available, zero new coupling.** `SchedulerSnapshot.pending_actions:
  Vec<PendingActionView>` (`surfaces/src/snapshot.rs:136`) already exists, already carries
  `agent_id` per entry (`snapshot.rs:108`), already built every `update_snapshot` cycle
  (`scheduler.rs:2289-2311`) from `state.pending_approvals`. The attention aggregation needs only
  to filter this ALREADY-BUILT vector by `agent_id`, not read a new subsystem.
- **Provider/credential degradation: already fully available, zero new coupling.**
  `SchedulerSnapshot.credential_snapshot: Option<CredentialSnapshot>` (`snapshot.rs:146`) already
  exists, carrying `provider_health: Vec<ProviderHealth>` system-wide; `AgentSnapshot` already
  carries `credential_providers: Vec<String>` per agent (cred.5, already shipped). Correlating
  "is this agent's provider degraded" is a name-match join between two ALREADY-PRESENT fields on
  the SAME already-assembled `SchedulerSnapshot`, not new cross-subsystem access.
- **Budget risk: 0B's original citation was wrong, corrected here — no cred.4 access needed at
  all.** Direct search of `agentd/src/credential/mod.rs` found no exposed per-agent spend-cap
  struct/field reachable from `SchedulerSnapshot` today — cred.4's spend caps are enforced at the
  `CredentialGateway` layer and aren't currently projected into the snapshot. **Corrected
  signal source: reuse the ALREADY-EXISTING `MemoryPressure` 75/90% threshold on
  `AgentSnapshot.token_budget`/`context_tokens`** (the inference token budget, already powering
  the Design phase's budget-bar rendering) — this is the actual "is this agent running low"
  signal an operator cares about today, already fully available, and keeps 0C-bis's "zero new
  instrumentation" pledge honest (0B's cred.4 citation is struck; the signal itself is unchanged
  in meaning — "budget risk" — just sourced from data already on `AgentSnapshot` instead of a
  subsystem that doesn't expose it yet).
- **Idle/hung: already fully available** (superseded plan's Eng-reviewed mechanism, reused
  verbatim, per the CEO-phase fix).

**Net result: the aggregation is a pure post-processing/correlation step over data ALL of which
already exists on the assembled `SchedulerSnapshot`/`AgentSnapshot` structures — genuinely ZERO
new cross-subsystem reads, zero new locking risk.** This is a materially better finding than the
CEO phase's flagged open question anticipated; Approach A's effort estimate (S-M) holds, and the
"new coupling" risk in CEO Section 1 does not materialize.

### Section 1 — Architecture

**⚠ Corrected below per Eng Dual Voices (see that section for the full finding) — the version
that follows is the fixed architecture, not the original claim.**

```
agentd/src/scheduler.rs (enqueue_or_defer / the update_snapshot call site — has &state directly,
  BEFORE SchedulerSnapshot's own pending_actions field gets .take(100)-capped at scheduler.rs:2160)
  │ NEW: derive_attention(&state.pending_approvals, &credential_snapshot, &agents_being_built)
  │      reads state.pending_approvals DIRECTLY (untruncated source — Eng Dual Voices, Codex
  │      finding: filtering the already-capped snapshot vector would silently drop Approval
  │      signals for agents past the first 100 pending approvals)
  ▼
Per agent: Approval (state.pending_approvals, agent_id match) — Degraded (credential_snapshot's
  provider_health, matched against AgentSnapshot.credential_providers by name; fires on
  `!token_fresh` ALONE, not `AND last_error present` — Eng Dual Voices, Codex finding: a missing
  API-key env var sets token_fresh:false without necessarily setting last_error, and the
  original AND-rule would silently miss that real degraded case) — Budget (AgentSnapshot's own
  token_budget/context_tokens against the existing MemoryPressure threshold; NOT cred.4, corrected
  in Phase 3 Step 0).
  │ writes attention: Vec<AttentionSignal> onto each AgentSnapshot
  ▼
surfaces/src/snapshot.rs (AgentSnapshot) — new field, manual Serialize (same silent-drop trap the
  superseded plan found for its own fields — explicit serialize_field call + test required)
  ├──▶ surfaces/src/agents_fs.rs (FUSE) — **⚠ corrected: this is a genuinely NEW virtual file,
  │     not a change to an existing one** (Eng Dual Voices, Codex finding: FUSE doesn't serialize
  │     the whole AgentSnapshot as JSON — it serves fixed per-agent files via explicit offsets,
  │     `agents_fs.rs:368`). Needs the same multi-touch-point pattern the superseded plan's own
  │     Eng Review already found for its fields: a new OFF_* const, two readdir offset arrays,
  │     the file_name_for_offset match arm, the getattr arm, the read arm, the directory-listing
  │     tuple list — 6-7 edits, one bullet, not "reuse the existing file."
  └──▶ agentd/src/management.rs (JSON API) — new field in the existing snapshot JSON
                              │
              ┌───────────────┴───────────────┐
              ▼                               ▼
agentctl/src/watch/reader.rs (FUSE)     agentctl/src/watch/source.rs (HTTP)
              └───────────────┬───────────────┘
                              ▼
      agentctl/src/watch/views.rs (render: summary line, ATTN column, stacked lines, routing)
      + agentctl/src/watch/app.rs (Enter-routing per the Design phase's Reference table 2,
        now showing only the 3 ux.2a signals — Idle/Error rows marked "ux.2b, not built")
```
Coupling: `derive_attention` reads `state.pending_approvals` (scheduler-internal, not a new
external subsystem) and `credential_snapshot` (already computed each cycle) — no NEW subsystem
dependency, but note the read site moved from "pure post-processing on the assembled snapshot"
(the original claim) to "during snapshot assembly, before the approval-list truncation" (the
corrected architecture) — a real, if small, change to where this code lives, not just what it
reads. **Locking (Eng Dual Voices, Codex finding, softened from the original "zero risk" claim):**
`update_snapshot` already calls `gw.snapshot()` inside its `try_write()` lock
(`scheduler.rs:2175`) — adding `derive_attention` inside that same lock extends writer-hold time
slightly. Not catastrophic (the same lock already does comparable work), but not literally zero
risk as originally stated; accepted as-is, logged, not blocking.

**Shadow paths:** an agent with a `credential_providers` entry that doesn't match any name in
`provider_health` → no Degraded signal (silent non-match, acceptable — an unconfigured provider
isn't real for this agent). A `pending_approvals` entry whose `agent_id` matches no current agent
→ filtered out naturally, no special-case needed. **Pre-existing, not introduced by this plan
(Eng Dual Voices, subagent finding):** `update_snapshot`'s `try_write()` can silently skip an
entire cycle under lock contention — the ATTN column and summary line would be one tick stale in
that case, same as every other field today; logged to the Design phase's state table as an
accepted, pre-existing staleness tolerance, not a new gap this plan introduces.

### Section 2 — Code Quality

**DRY:** one `derive_attention` function, one `AttentionSignal` struct, one priority-ordering
table (Design phase's Reference table 2, now 3 rows not 5) consumed by both row-color (severity)
and Enter-routing (actionability).

**Checkpoint/restore:** `attention: Vec<AttentionSignal>` is NOT checkpointed — derived fresh
every cycle from state that's either already-checkpointed (`pending_approvals`, `token_budget`)
or runtime-only (`credential_snapshot`), matching the `last_pressure` "derive fresh" precedent.
**Corrected (Eng Dual Voices, both models):** the original claim that "`error_count` is already
checkpointed" was false — that field doesn't exist in this increment's scope at all (Idle/Error
deferred to ux.2b) — this section no longer makes any claim about it.

**Missing edge case, corrected:** the fleet-wide "M unavailable" count (Design Fix 3) fires when
a configured provider's `provider_health` entry shows `!token_fresh` (matching the corrected
Degraded rule above) — NOT when `credential_snapshot` itself is `None` (that means "no gateway
configured," a different, non-degraded state, unchanged existing semantics).

### Section 3 — Test Review (rescoped to ux.2a's 3 signals; Idle/Error tests move to ux.2b)

1. `derive_attention` unit tests: one per signal type (**approval, budget, degraded — 3, not 5**),
   verifying the correct `AttentionSignal` from constructed scheduler state.
2. Approval-cap test (Eng Dual Voices, Codex finding, NEW): construct >100 pending approvals for
   one agent's fleet, assert the 101st+ still produces an Approval signal (guards against
   silently reading the capped `pending_actions` vector instead of `state.pending_approvals`).
3. Degraded-rule test (Eng Dual Voices, Codex finding, NEW): a provider with `token_fresh:false`
   and `last_error: None` (the missing-API-key case) still produces a Degraded signal (guards
   against the original overly-strict `AND last_error` rule).
4. Priority/routing test: Approval + Degraded both active on one agent — assert row-color picks
   the higher-severity one (Degraded/Critical), assert Enter-routing still picks Approval (Design
   Fix 1's actionability-over-severity split) — the one test directly encoding that rule.
5. Tie-break test: Degraded + Budget (same severity tier scenario, no Approval involved) — assert
   the fixed enum-order tie-break (Design Fix 2) is deterministic.
6. Fleet-wide partial-degradation test: one provider's `token_fresh:false` — assert `M
   unavailable` reflects only agents using that specific provider.
7. Serialization test: assert `attention` appears in the management-API JSON AND is readable via
   the new FUSE virtual file (two separate assertions, per the corrected FUSE architecture above
   — not one combined check, since they're now known to be two different code paths).

### Section 4 — Performance

`derive_attention` runs once per `update_snapshot` cycle, O(agents × configured_providers) for
the credential-health join (small, bounded — providers are a fixed, small, operator-configured
set) — no new performance risk class introduced.

### Eng Dual Voices

Dispatched Claude subagent (Agent tool, foreground) and Codex (Bash, parallel) against this
plan's Eng Review section, specifically to verify or refute Step 0's central "zero new
cross-subsystem reads" claim against the actual code.

**CLAUDE SUBAGENT (eng — independent review) — verbatim summary:** The claim holds for 3 of 5
signals (Approval, Degraded, Budget) — verified directly against `scheduler.rs`/`snapshot.rs`,
all three really are pure joins over already-assembled `SchedulerSnapshot` data. **It does NOT
hold for Idle and Error — CRITICAL.** Grepped `agentd/src`, `surfaces/src`, `agentctl/src` for
`error_count`/`idle_secs`/`last_error_at`/`last_activity`: zero matches anywhere in the actual
codebase. These fields exist ONLY as a fully-designed, never-implemented plan in the superseded
`ux.2-observe.md` — whose own text this very plan cites as "already Eng-reviewed once, zero new
instrumentation," when the superseded plan's own "What already exists" section (this plan's line
43-44) already admits "none of those landed — no code was written." The Eng Review section
contradicts this plan's own earlier admission. This understates the file list and effort
estimate materially — the superseded plan's 10-file, "at the smell threshold" estimate was being
used as an unfavorable comparison point that no longer holds once corrected. Also confirmed: the
manual-Serialize silent-drop risk is real (accurate as stated); the "derive fresh, don't
checkpoint" principle is sound and matches the `last_pressure` precedent, but citing
`error_count` as "already checkpointed" is false since the field doesn't exist; a medium finding
on silent snapshot-update skips under `try_write()` lock contention (pre-existing, not
introduced, but unaddressed in the Design phase's state table); a low finding on `update_snapshot`'s
actual code order (agents built before pending_actions/credential_snapshot, opposite of what
`derive_attention` needs — a reorder or backfill pass, not a one-directional pipeline as drawn).

**CODEX (eng — architecture challenge) — verbatim summary:** Independently confirms the same
CRITICAL finding via direct grep of `surfaces/src/snapshot.rs`/`agent/mod.rs`. Additionally: HIGH
— `pending_actions` is capped `.take(100)` in `update_snapshot` (`scheduler.rs:2160`); filtering
this already-truncated vector silently drops Approval signals for any agent whose approval
didn't make the cap — the fix must read `state.pending_approvals` directly (the untruncated
source), meaning `derive_attention` can't run purely as post-processing on the already-assembled
`SchedulerSnapshot` even for Approval. HIGH — FUSE exposure is mischaracterized: FUSE doesn't
serialize the whole `AgentSnapshot`, it serves fixed per-agent virtual files via explicit offsets
(`agents_fs.rs:368`) — adding `attention` needs a genuinely new virtual file (its own offset
const, two readdir arrays, `file_name_for_offset` arm, `getattr` arm, `read` arm, directory
listing), the same multi-touch-point pattern the superseded plan's own Eng Review already found
for ITS new field (not a one-line "reuse the existing file" change as this plan's Section 1
diagram claimed). MEDIUM — `CredentialGateway::snapshot()` sets `token_fresh: false` for a
missing API-key env var without necessarily setting `last_error` (`credential/mod.rs:1298`) —
the plan's Degraded rule ("`token_fresh: false` AND `last_error` present") would silently miss
this real, configured-but-unusable-provider case; should fire on `!token_fresh` alone. MEDIUM —
checkpoint/restore analysis is "overconfident" for the same root reason as the CRITICAL finding.
LOW/MEDIUM — the "zero locking risk" claim should be softened: `update_snapshot` currently calls
`gw.snapshot()` inside its `try_write()` lock; deriving attention inside that same lock would
extend writer-hold time (not catastrophic, but not zero risk as stated). Bottom line: "directionally
plausible only for Budget and part of Credential/Approval... the hidden work is idle/error state
design, FUSE surface design, approval cap semantics, and restore semantics."

**Consensus table:**

| Finding | Claude subagent | Codex | Independently confirmed against code | Resolution |
|---|---|---|---|---|
| Idle/Error signals require unbuilt fields, not "zero new work" | ✅ CRITICAL | ✅ CRITICAL | Yes — zero grep matches for `idle_secs`/`error_count` anywhere in the real codebase | **Rescoped out of this increment — see below** |
| Approval-signal read must bypass the `.take(100)` cap | — (not raised) | ✅ HIGH | Yes — `scheduler.rs:2160` | Fixed: read `state.pending_approvals` directly, not the already-capped snapshot vector |
| FUSE exposure mischaracterized (needs a new virtual file, not a one-line reuse) | — (not raised) | ✅ HIGH | Yes — `agents_fs.rs:368`'s offset-based file model | Fixed: Section 1 corrected, 7-touch-point FUSE work added back to the file list |
| Degraded rule misses `token_fresh:false` with no `last_error` | — (not raised) | ✅ MEDIUM | Yes — `credential/mod.rs:1298` | Fixed: rule is now `!token_fresh` alone |
| Checkpoint/restore claims overconfident | ✅ (same root cause) | ✅ MEDIUM | Yes | Resolved by the rescope — no longer claims anything about unbuilt fields |
| "Zero locking risk" overstated | — (not raised) | ✅ LOW-MEDIUM | Yes — `gw.snapshot()` already runs inside `try_write()` | Softened in Section 1/4, logged as accepted minor risk |
| Silent snapshot-skip under lock contention (pre-existing) | ✅ MEDIUM | — (not raised) | Yes | Logged to Design state table as an accepted, pre-existing staleness tolerance, not a new gap |

**Not a User Challenge — but the most consequential finding of this entire reframe.** Both
models independently, via direct code verification, found that this plan's central engineering
claim was false for 2 of 5 signals. This is Mechanical (P1 completeness), not taste: the fix is
dictated by what the code actually supports today, not a preference.

**RESCOPE (Mechanical, auto-decided — P2 boil-lakes + P3 pragmatic):** Split into two
increments, reusing rather than discarding the superseded plan's still-valid design work:

- **ux.2a — "Attention v1" (THIS plan, corrected scope): Approval + Degraded + Budget signals
  only.** All three are genuinely zero/near-zero new instrumentation once the 2 bugs above are
  fixed (Approval reads `state.pending_approvals` directly; Degraded fires on `!token_fresh`
  alone). File list, mockup, and reference tables below are corrected to this narrower scope.
- **ux.2b — follow-on (not this increment): Idle + Error signals.** These require exactly the
  engineering work the superseded `ux.2-observe.md` plan already fully designed and dual-voice
  reviewed (new `AgentTask` fields, the `CallTools`-dispatch-site fix for in-flight visibility,
  the `error_count`/`last_error_at` batch-aggregation semantics, checkpoint persistence for the
  error fields) — that work is NOT wasted, it becomes ux.2b's ready-made spec, requiring
  re-verification against current code (main has moved since it was written) but not a redesign
  from scratch. Once built, ux.2b wires Idle/Error into the SAME `AttentionSignal`/routing/
  rendering mechanism ux.2a builds — no rework of ux.2a's mechanism needed, just two more
  variants added to an already-extensible enum.

**Honest consequence for cos-ux-01 (must be stated plainly, not softened):** ux.2a does NOT
fully close cos-ux-01. The founding incident (agent hangs mid-tool-call, not erroring, not
degraded, not over budget) is exactly the Idle signal — deferred to ux.2b. ux.2a closes the
*Approval/Degraded/Budget* dimensions of "does anything need me," which is real, shippable value,
but the specific incident that named cos-ux-01 remains open until ux.2b lands. `TODOS.md`'s
cos-ux-01 entry should be annotated (not closed) accordingly — see TODOS updates below.

### Eng Review — Decision Audit Trail additions

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|-----------------|-----------|-----------|----------|
| 10 | Eng/Dual-voice | **Rescoped: Idle + Error signals moved to ux.2b, out of this increment ("ux.2a")** | Mechanical | P1 completeness | Both models independently found `idle_secs`/`error_count` don't exist in the actual codebase — the "zero new work" claim was false; correct scope is dictated by what code exists today | Keeping Idle/Error in-scope on a false premise |
| 11 | Eng/Dual-voice | Approval signal reads `state.pending_approvals` directly, not the `.take(100)`-capped snapshot vector | Mechanical | P1 completeness | Confirmed bug: filtering the capped vector silently drops signals past the 100th pending approval | Filtering the already-assembled, already-capped vector |
| 12 | Eng/Dual-voice | Degraded rule fires on `!token_fresh` alone, not `AND last_error` | Mechanical | P1 completeness | Confirmed bug: a missing API-key env var sets `token_fresh:false` without necessarily setting `last_error` — the AND-rule silently misses a real degraded case | Keeping the stricter, buggy rule |
| 13 | Eng/Dual-voice | Sandbox degradation (p6.8) deferred, not Eng-verified this pass | Taste (conservative, not a confirmed bug) | P1 completeness | `SandboxSummary` is largely static/startup-time, unlike `ProviderHealth`'s dynamic refresh — whether it's a meaningful runtime attention trigger needs its own check, not assumed on the strength of an unverified 0B citation | Shipping it on the same unverified assumption that already broke twice this session (cred.4, then Idle/Error) |

### Eng Review — TODOS.md updates (additive)

11. **Annotate `TODOS.md`'s `cos-ux-01` entry — do NOT close it when ux.2a ships.** ux.2a
    delivers Approval/Budget/Degraded attention signals but does NOT close the founding incident
    (agent hangs mid-tool-call, not erroring/degraded/over-budget) — that's the Idle signal,
    deferred to ux.2b. Add a note: "Partially addressed by ux.2a (2026-07-13) — Approval/Budget/
    Degraded visibility shipped; the original busy-vs-hung incident itself remains open until
    ux.2b (Idle/Error signals, reusing `docs/plans/ux.2-observe.md`'s already-designed mechanism)."
12. **ux.2b: build the superseded plan's Idle/Error mechanism**, re-verified against current
    `main` (which has moved since that plan was written) rather than assumed still accurate —
    the exact lesson this rescope just taught. (P1, effort matches the superseded plan's own
    S-M/M estimate, since it's largely that plan's ready-made spec)
13. Sandbox-degradation (p6.8) as an attention signal — needs its own Eng verification of
    `SandboxSummary`'s actual per-agent dynamics before being added to ux.2a or ux.2b. (P3, S
    effort to verify, unknown effort to build depending on what's found)
14. `filter_agent_id` on `ApprovalsViewState` / agent-jump on `View::System`/`View::Credentials`
    (already logged as Design TODOS #5/#6, cross-referenced here since it's the same "unscoped
    click-through" compromise Reference table 2 documents).
15. **Trace/span view in `AgentDetail`** (user-requested during this session, explicitly scoped
    as a follow-on, tackled after ux.7): reuse `otel/src/span_builder.rs`'s hierarchy-computation
    logic (trace/span/parent-id construction from `flight.jsonl`), NOT the `agentos-otel` crate
    itself (which pulls in the full `opentelemetry`/`tonic` gRPC stack — an unjustified dependency
    for a TUI client). Extract the pure hierarchy logic into a small shared crate both `otel` and
    `agentctl` can depend on, rather than duplicating it. Sequenced after ux.7 (Run replay) per
    user decision — related but distinct (ux.7 replays/scrubs past runs; this is a live structural
    view of the current run). (P3, effort TBD — needs its own scoping pass when picked up)

### Eng Completion Summary

```
+====================================================================+
|            MEGA PLAN REVIEW — COMPLETION SUMMARY (ENG)             |
+====================================================================+
| Step 0 (Scope Challenge) | 1 CRITICAL rescope (Idle/Error → ux.2b), initially claimed resolved favorably, corrected by Dual Voices |
| Section 1 (Arch)         | 2 bugs fixed (Approval-cap read site, FUSE mischaracterization); locking claim softened |
| Section 2 (Quality)      | Checkpoint claim corrected (no longer references unbuilt fields) |
| Section 3 (Tests)        | 7 tests, rescoped to 3 signals + 2 new bug-guard tests (cap, degraded-rule) |
| Section 4 (Perf)         | No new findings |
| Eng Dual Voices          | 2 models, BOTH independently found the same CRITICAL scope error via direct code grep; Codex found 5 additional real bugs the subagent didn't |
| Rescope                  | ux.2a (this plan): Approval+Budget+Degraded. ux.2b (follow-on): Idle+Error, reusing the superseded plan's ready-made design |
| cos-ux-01                | Partially addressed by ux.2a, NOT closed — stated plainly, not softened |
+====================================================================+
```

## PHASE 3 COMPLETE

The Eng Review's dual-voice pass caught the most consequential defect of this entire reframe:
the plan's central engineering claim — "zero new instrumentation" — was true for 3 of 5 signals
and false for 2, because the Idle/Error signals depend on fields that exist only in the
superseded plan's design, never actually implemented. Both models found this independently via
direct code grep, matching the same rigor that caught the analogous mistake in the original
"Observe" plan's own Eng Review. Rather than discard that unbuilt work, it's preserved as ux.2b's
ready-made spec. ux.2a — Approval, Budget, and Degraded signals — ships now as real, honestly-
scoped value; the founding cos-ux-01 incident itself stays open until ux.2b lands, stated
plainly in TODOS.md rather than quietly implied as closed. Codex also found 2 additional real
bugs (the approval-count cap, the credential-degradation rule) that the subagent didn't surface,
both fixed directly. Proceeding to Phase 3.5 (DX Review) — DX scope was already confirmed during
Phase 0 of the superseded plan and carries forward (same TUI, same `agentctl` surface).

## DX Review (Phase 3.5)

### Step 0 — DX Scope

Same scoping as the superseded plan's DX Review: **CLAUDE.md single-operator internal CLI**, not
a public SDK — Passes 1/2/3/6 apply (Getting Started, CLI/TUI Design, Error Messages, Dev
Environment/`--plain`); Passes 4/5/7/8 (Docs, Upgrade, Community, Measurement) don't have a real
referent here, same reasoning as before, not re-litigated.

### DX Dual Voices

Dispatched Codex (Bash, parallel) and a Claude subagent (Agent tool, foreground) against the
plan as it stood after the Eng rescope — specifically to catch anything the rescope itself might
have left inconsistent, and to apply the 4 DX passes fresh.

**CLAUDE SUBAGENT (DX — independent review) — verbatim summary, the most consequential finding
of this pass:** Verified directly against `agentctl/src/watch/views.rs:105-121` (`render_dashboard`)
and `reader.rs:114-133` (`AgentInfo`): **the entire mockup — v1 and v2 — was drawn against a
Dashboard that was never shipped.** The real table today is 5 columns (`Agent ID | Status |
Context | Budget | Tools`), not the 9-column `STATUS/TURN/LAST-TOOL/TOKENS/$/AGE` layout every
prior mockup assumed (inherited from the master plan's aspirational rough-scope text, never
built — the superseded plan's own Eng Review already confirmed this). All width/hierarchy/Pass-1
scoring in both prior Design passes was against a fictitious baseline. Also found: no glyph
legend for `⚠/·/?` despite the plan claiming to reuse this codebase's established inline-legend
convention (`views.rs:340-345`, System view); `--plain` Dashboard spec defined only bracket
markers, not reason text, unlike `AgentDetail`'s spec — real information-loss risk once the
signal set narrowed to 3 (Approval/Budget/Degraded would all collapse to indistinguishable
`[!]`); the title still said "closes cos-ux-01," contradicting the plan's own honest rescope
text; Reference table 2's routing priorities had Degraded and Error both at "2" — harmless today
(Error unbuilt) but would confuse ux.2b's implementer. Recommendation: needs one more pass — fix
the baseline before Eng finalizes; not blocking, the core 3-signal mechanism is sound.

**CODEX (DX — developer/operator experience) — verbatim summary:** Independently confirmed the
stale "closes cos-ux-01" title/premise-text overclaim (HIGH), citing the same contradiction with
the Eng rescope's own text. Additionally: the mockup's 2-line footer isn't free — the real
Dashboard reserves exactly one footer row (`Constraint::Length(1)`, `views.rs:47`) — adding a
legend line is a genuine layout change, not something that fits the existing single line
(MEDIUM). The footer copy "Enter: jump to attention source" overpromises for the 2 of 3 signal
types that route to unscoped views — `ApprovalsViewState` confirmed to have no agent-filter
field (`approvals.rs:14`), `System`/`Approvals`/`Credentials` confirmed to be global `View`
variants (`app.rs:19,29,31`) (MEDIUM). `--plain` marker set was internally inconsistent — Pass 7
said `[.]` for Clean, Reference table 1 said `[OK]` for the same state, two different answers to
the same question in the same document (LOW-MEDIUM). Clean's `·` glyph has no inline text by
design and needs the legend to be interpretable at all — converges with the subagent's finding
(LOW). Recommendation: needs one more pass, not blocking.

**Consensus table:**

| Finding | Claude subagent | Codex | Independently confirmed against code | Resolution |
|---|---|---|---|---|
| Entire mockup baseline is fictitious (5 real columns vs. 9 assumed) | ✅ (primary finding) | — (not raised) | Yes — `views.rs:105-121`, `reader.rs:114-133` | **Fixed: mockup redrawn (v3) against the real 5-column table** |
| No glyph legend for `⚠/·/?` | ✅ | ✅ (converges, "Clean depends on unexplained glyph") | Yes — `views.rs:340-345` precedent | Fixed: legend line added |
| Footer legend line is a real layout change, not free | — (not raised) | ✅ | Yes — `views.rs:47`'s `Constraint::Length(1)` | Fixed: stated explicitly as a 2-row layout change |
| Title/premise text still says "closes cos-ux-01" | ✅ | ✅ (both, independently) | Yes — plan's own contradicting rescope text | Fixed: title and Premise 1 both amended |
| `--plain` collapses 3 distinct signals to one bare marker | ✅ | (converges via marker-inconsistency finding) | Yes | Fixed: marker + reason text, matching `AgentDetail`'s pattern |
| `--plain` marker for Clean inconsistent (`[.]` vs `[OK]`) | — (not raised) | ✅ | Yes — 2 places in the same document disagreed | Fixed: standardized on `[OK]` |
| Reference table 2's duplicate routing priority (Degraded=Error=2) | ✅ | — (not raised) | Yes | Fixed: renumbered 1-7, all unique |
| "Jump to attention source" overpromises for 2 unscoped routes | — (not raised) | ✅ | Yes — `approvals.rs:14`, `app.rs:19,29,31` | Fixed: softened to "Enter: view detail" |

**Not a User Challenge.** Every finding is a concrete, code-verifiable specification defect —
several inherited all the way from the original "Observe" plan's own mockup, never caught until
this pass specifically checked the mockup's baseline against real code rather than against the
master plan's aspirational text. This is the same lesson as the Eng phase's rescope, one layer
up: verify against what ships, not what was once envisioned.

### DX Scorecard

```
+====================================================================+
|                  DX REVIEW — SCORECARD (Phase 3.5)                 |
+====================================================================+
| Pass 1 (Getting Started)     | 1 HIGH found + fixed (fictitious mockup baseline)          |
| Pass 2 (CLI/TUI Design)      | 2 MEDIUM found + fixed (legend line + layout, click copy)  |
| Pass 3 (Error Messages)      | 1 LOW found + fixed (Clean glyph needs the legend)         |
| Pass 6 (Dev Environment)     | 2 findings found + fixed (--plain marker consistency + reason text) |
| Dual Voices                  | 2 models; the mockup-baseline finding was Claude-subagent-only, everything else had at least partial convergence |
| User Challenge?               | None — all findings mechanical spec defects                |
+====================================================================+
```

## PHASE 3.5 COMPLETE

The DX Review's sharpest finding closes out a mistake that had actually been present since the
very first draft of the original "Observe" plan and was carried forward unnoticed through 3 full
review phases across two increments: the ASCII mockup was drawn against a 9-column Dashboard
that was never built, not the real 5-column one. Caught only because this pass checked the
mockup against `views.rs`/`reader.rs` directly instead of trusting the master plan's own
aspirational rough-scope text. The corrected v3 mockup is simpler than what it replaces — the
real Augment premise turns out to need less new surface than assumed, not more. Proceeding to
Phase 4 (Final Approval Gate).

## GSTACK REVIEW REPORT

| Run | Status | Findings |
|---|---|---|
| CEO Review (Phase 1) | ✅ Complete, dual-voice | 1 CRITICAL fixed (missing idle/hung trigger — later itself rescoped by Eng), 1 Taste (shrink-vs-deepen AttentionSignal depth), 1 prior Taste carried forward (Codex's original "Attention & Evidence" reframe recommendation — already resolved by the user choosing to reframe) |
| Design Review (Phase 2) | ✅ Complete, dual-voice | 2 CRITICAL fixed (2/3 click-through routes didn't exist in code; clean-state ambiguity), 2 reference tables delivered, severity-vs-actionability routing split |
| Eng Review (Phase 3) | ✅ Complete, dual-voice | 1 CRITICAL rescope (Idle/Error signals require unbuilt fields — split into ux.2a/ux.2b), 2 additional real bugs fixed (approval-count cap, credential-degradation rule), locking claim softened |
| DX Review (Phase 3.5) | ✅ Complete, dual-voice | 1 HIGH fixed (entire mockup baseline was fictitious — inherited unnoticed from the original plan), 4 more findings fixed (legend, layout, plain-mode markers, click copy) |

**VERDICT: Ready to build, with 2 taste decisions flagged below and one honest scope reduction
that must not be silently glossed over when this ships.**

### Plan Summary

**ux.2a — Attention**: adds a `derive_attention` aggregation to the cockpit's Dashboard, showing
one leading `ATTN` column (`⚠`/`·`/`?`) plus a summary line ("N need attention"), built from 3
real signals — pending approvals, budget risk, and credential/provider degradation — all
genuinely reusing existing data, zero/near-zero new instrumentation. It does NOT include the
Idle/Error signals originally planned; those require fields that don't exist in the codebase yet
and are deferred to a follow-on, ux.2b, which can reuse the superseded `ux.2-observe.md` plan's
already-complete design almost as-is.

### Decisions Made: 24 total (19 auto-decided/mechanical, 2 taste, 3 explicit user gates)

### Your Choices (taste decisions)

**Choice 1: AttentionSignal's depth — small typed struct vs. a fuller shared domain contract**
(from CEO Dual Voices). Codex argued for a richer "Attention contract v1" (severity taxonomy,
escalation/suppression policy, operator SLA, shared across ux.2/ux.4/ux.6). I recommend the
smaller struct (`reason`/`severity`/`since`/`evidence`) — cheap, forward-compatible, no policy
layer this increment doesn't need yet. Picking the fuller contract now would expand scope
significantly and design policy (what's allowed to interrupt you) before ux.4 exists to validate
it against.

**Choice 2: Sandbox-degradation (p6.8) — build now or verify-then-decide** (from Eng Dual
Voices). I deferred it, uncertain whether `SandboxSummary`'s largely-static, startup-time nature
makes it a meaningful *runtime* attention trigger at all (unlike `ProviderHealth`'s dynamic
refresh). I recommend deferring to a quick Eng-only verification pass before deciding whether it
belongs in ux.2a or ux.2b — 1-line downstream impact: if it turns out trivial to add, it's a
cheap addition to ux.2a; if not, it's clean to add to ux.2b alongside Idle/Error.

### Auto-Decided: 19 decisions — see the Decision Audit Trail entries throughout the plan file
(CEO rows 1-9, Eng rows 10-13, plus the DX-phase fixes applied directly with rationale inline).

### Review Scores

- CEO: premise gate passed (Augment); 1 critical fix, 1 taste flagged.
- CEO Voices: Codex [strategic reframe challenge, partially adopted], Claude subagent [validated
  premises, found the idle/hung gap], Consensus: divergent on scope depth, convergent on the
  core mechanism being sound.
- Design: 2 critical fixes (fake routing targets, clean-state ambiguity), now DX-corrected to
  match the real Dashboard baseline.
- Design Voices: both models found real, non-overlapping defects; consensus confirmed the
  hierarchy itself was right throughout.
- Eng: 1 critical rescope (Idle/Error → ux.2b), 2 additional bugs fixed.
- Eng Voices: both models independently found the SAME central defect via direct code grep —
  highest-confidence finding of the whole pipeline.
- DX: 1 high fix (fictitious mockup baseline, inherited unnoticed since the original plan), 4
  more fixed.
- DX Voices: both models converged on the "closes cos-ux-01" overclaim; the mockup-baseline
  finding was subagent-only but code-verified beyond doubt.

### Cross-Phase Themes

**Theme: "verify against what actually ships, not what was once designed or assumed" —
appeared independently in ALL FOUR phases.** CEO Review found `PRODUCT-THESIS.md`'s build-
priority list doesn't itself dictate UI hierarchy (an inference, not a fact). Eng Review found
2 of 5 signals depended on fields that exist only in an unimplemented prior plan. DX Review
found the entire mockup was drawn against a Dashboard that was never built. This is the single
highest-confidence signal from this whole autoplan run — not a one-off mistake, a recurring
failure mode of trusting design documents over source code, caught only because every phase in
this pipeline independently re-verified against the actual repository rather than the prior
phase's word.

### Deferred to TODOS.md (15 items total — see each phase's "TODOS.md updates" section for full
detail; headline items)

- ux.2b: Idle + Error attention signals (reuses the superseded plan's design).
- `filter_agent_id` on `ApprovalsViewState`; agent-jump on `View::System`/`View::Credentials`.
- Sandbox-degradation (p6.8) as an attention signal — pending its own Eng verification.
- `cos-ux-01` annotated, not closed, until ux.2b lands.
- Trace/span view in `AgentDetail` (user-requested, scoped after ux.7).
- Generalized "Attention contract v1" (Codex's fuller domain model) — revisit when ux.4 exists.
- Inspector's pre-existing dead `InspectorFilter::Errors` chip (unrelated bug, found incidentally).

### Implementation Tasks (aggregated across phases)

- [ ] **P1 — `derive_attention` function (agentd/scheduler.rs)**: Approval (read `state.pending_approvals`
      directly, not the capped snapshot vector), Budget (existing `MemoryPressure` threshold),
      Degraded (credential, `!token_fresh` alone) — 3 signals, priority-ordered per Reference table 2.
- [ ] **P1 — `AttentionSignal` struct + `attention: Vec<AttentionSignal>` field on `AgentSnapshot`**
      (manual `Serialize` — remember the `serialize_field` call, has bitten this codebase before).
- [ ] **P1 — new FUSE virtual file** for `attention` (7-touch-point pattern: OFF_* const, 2
      readdir arrays, file_name_for_offset, getattr, read, directory listing).
- [ ] **P1 — management-API JSON field**, `reader.rs`/`source.rs` parsing.
- [ ] **P1 — Dashboard rendering**: ATTN column (leading, after Status), summary line, legend
      line (2-row footer layout change), stacked reason lines, `--plain` markers + reason text.
- [ ] **P2 — AgentDetail attention strip** (persistent, top line, stacks multiple signals).
- [ ] **P2 — Enter-routing** per Reference table 2 (actionability-driven, distinct from
      severity-driven row color).
- [ ] **P2 — 7 tests** per Eng Review Section 3 (signal-derivation ×3, approval-cap guard,
      degraded-rule guard, priority/routing split, tie-break, fleet-wide partial-degradation,
      serialization).
- [ ] **P3 — `TODOS.md` update**: annotate `cos-ux-01` as partially addressed, not closed.
- [ ] **P3 — `docs/ROADMAP.md`/`docs/plans/ux-cockpit.md` scope note**: ux.2 → ux.2a/ux.2b split,
      "Attention" naming (not "Attention & Evidence," collides with ux.6).

NO UNRESOLVED DECISIONS.
