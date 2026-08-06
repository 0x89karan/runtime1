# zk.1 — Hybrid Cryptographic Verification (zkVM receipts + egress hash chain)

**Status:** RESHAPED at the /autoplan premise gate, 2026-08-06. The inline-zkVM design is
REJECTED (6/6 adverse cross-model consensus, operator-ratified). The surviving work is the
increment ladder in "RESHAPED OUTCOME" at the end of this file: **attn.4 → gov.1 → gov.2 →
zk.0 → zk.1 → zk.2 (partner-gated) → zk.3** (gov.2 scheduled by operator decision D5,
2026-08-06, overriding the named-unscheduled recommendation). Partner naming proceeds in
parallel, zero engineering. Each increment gets its own /autoplan at pickup.
**Relationship to existing work:** successor/extension to the deferred `ux.6b`
(signed action ledger). Touches `agentd/src/evidence.rs`, `agentd/src/egress.rs`,
`flight.jsonl`, and the scheduler admission path.

---

## Objective (operator's brief, verbatim intent)

Implement an enterprise-grade verification system that mathematically proves runtime1
security policies are enforced without revealing proprietary rules, while
simultaneously maintaining a high-throughput, tamper-evident audit log of all
external agent actions.

## Core Architecture

The system bifurcates verification into two distinct pipelines to avoid the severe
latency and memory overhead of proving stochastic, I/O-heavy agent logic inside a
zero-knowledge environment.

### 1. The zkVM Authorization Engine (deterministic logic)

The deterministic components of runtime1 — specifically the core Scheduler, token
metering, and policy evaluation rules (written in Rust) — are compiled into a RISC-V
binary and executed within a Zero-Knowledge Virtual Machine (zkVM).

- **Implementation target:** a performant zkVM such as RISC Zero or SP1. Both
  natively compile Rust to RISC-V and use GPU acceleration and cryptographic
  precompiles to reduce proving times.
- **Execution flow:** when a sandboxed agent requests an action (e.g. executing a
  specific tool or querying a domain), the zkVM evaluates the request against the
  configured budget and security policy.
- **Output:** a cryptographic receipt — a zero-knowledge argument of computational
  integrity — proving the policy was strictly adhered to. Proof is succinct (often
  under 250 KB), generated in seconds (~9–16 s for lightweight tasks in RISC Zero),
  and verifies the outcome without disclosing the proprietary rules or context data
  to the auditor.

### 2. The Egress Hash Chain (high-throughput I/O)

Because compiling high-volume network traffic, Python memory spaces, and streaming
LLM inference through a zkVM would cripple throughput, actual execution is deferred
to the Egress Mediator outside the zkVM.

- **Implementation target:** an append-only cryptographic data structure linked via
  Ed25519 signatures.
- **Execution flow:** once an action is authorized, the Egress Mediator executes the
  network request. For every action it generates a fixed-size hash of the payload
  and appends the precise hash of the previous record.
- **Output:** an immutable `evidence.jsonl` flight recorder. Pure hashing, no ZK
  circuits — thousands of concurrent actions with near-zero latency overhead.

### The Verifier Experience

At the end of an autonomous session, an enterprise security team receives a unified
**Audit Packet** containing both proofs:

1. Verify the zkVM receipt in milliseconds → mathematically proves the scheduler
   correctly enforced all admission and security policies.
2. Verify the hash chain → the sequential flight recorder of actual external API
   calls has not been tampered with or truncated.

Hybrid: mathematical certainty for compliance without bottlenecking real-time agent
execution.

---

## Repo context the review must engage (pre-filed tensions)

1. **THREAT_MODEL §8.7.1 (ratified de-claim):** the chain's signer is the audited
   party; deletion/rotation seams are undetectable from the chain alone. A zk proof
   of policy evaluation does not prove the host submitted *every* action for
   evaluation — the omission seam survives both pipelines.
2. **ux.6b analysis (DEFERRED, 2026-07-29):** B1 (widen per-action receipts) puts a
   synchronous fsync on every tool call inside a fail-closed writer; B2 (sign a
   projection of flight.jsonl) was the preferred substrate. Key custody — moving the
   key outside the boundary — was identified as "the increment that would actually
   change what can be claimed."
3. **Approved mv design doc rejected Approach C (ledger-first):** "dogfooding
   requires none of it yet; becomes relevant right before the external gate."
4. **Governance-buyer reality (ux.6b CEO phase):** buyers ask for SOC 2/ISO 42001
   auditor reports, SIEM export, WORM storage, RBAC/SSO — cryptographic chaining
   appears on none of those lists.
5. **Scheduler is async/IO-bound tokio:** only an extracted pure policy-evaluation
   function is zkVM-provable, not "the core Scheduler."
6. **Light-runtime invariant:** RISC Zero/SP1 are heavyweight dependency trees
   (~2 MB size-optimized agentd binary today; 6 MB CI guard).
7. **mv external gate:** 2026-10-01, 0/10 partners named, 0/3 demos booked. Prior
   CEO voices ranked naming partners above all governance engineering, four times.
8. **Roadmap next:** attn.4 (scheduler-native cron) is the queued increment — the
   one that brings the brief back.

---

# /autoplan Phase 1 — CEO Review (mode: SELECTIVE EXPANSION, auto-selected)

## Pre-review system audit (summary)

- Branch `main`, clean; last increment attn.2 R3+R4 (v0.121.0). Queued roadmap item: **attn.4**.
- `agentd/src/evidence.rs` (1,763 lines): Ed25519 hash chain, segment rotation, bounded tail
  resume, `record_allowed`/`record_denied` — **pipeline 2 of this proposal substantially exists.**
- `agentd/src/egress.rs` (1,810 lines): the "Egress Mediator" — but per ux.6a, the HTTP proxy
  path "never starts in production"; production egress is the credential broker + native
  scheduler admission.
- Design doc found: `karan-mv-governed-agent-runtime-design-20260720-114634.md` (APPROVED) —
  **its Approach C (ledger-grade-first) was explicitly rejected**: "dogfooding requires none of
  it yet; becomes relevant right before the external gate."
- Prior learnings applied: `assert-value-not-type-in-flight-log-guards` (10/10) — any zk/chain
  guard tests must assert recomputed values, not types; `audit-subagents-must-run-in-an-isolated-worktree` (10/10).

## Landscape check (Layer 1/2/3)

- **Layer 1 (tried and true):** enterprise audit evidence = SOC 2 Type II / ISO 42001 auditor
  reports, SIEM export, WORM storage (S3 Object Lock) in the customer's account. Hash chains and
  zk proofs appear on none of the standard buyer checklists (ux.6b CEO phase, reconfirmed).
- **Layer 2 (new and popular):** zkVM proving is real and cheapening — SP1 ~$0.02/proof on
  16×RTX-5090 clusters, RISC Zero ~$0.10/proof, Bonsai PaaS, Stellar verifies Groth16 receipts
  on-chain. Buyers today are overwhelmingly blockchain/rollup projects, not enterprise security
  teams. No evidence found of enterprise agent-governance buyers demanding zk receipts in 2026.
- **Layer 3 (first principles):** a zk proof moves the trust boundary from "trust my policy
  code ran" to "verify my policy code ran on these inputs" — genuinely stronger. But
  **completeness of the inputs stays self-attested** (THREAT_MODEL §8.7.1): the host chooses
  what to submit for proving. The proof is sound; the *claim* "mathematically proves policies
  are enforced" is not, because omission is invisible. The differentiator that survives first
  principles is ENFORCEMENT (the fuse actually stopping the runaway) + a COMPLETE record —
  which is ux.6b B2 + key custody, not zk.

## 0A. Premise Challenge

| # | Premise (from the brief) | Verdict |
|---|---|---|
| P1 | Enterprises need mathematical proof of policy enforcement without rule disclosure | **ASSUMED, contradicted by prior buyer analysis.** 0/10 design partners named; nobody has asked. ux.6b: buyers ask for SOC 2/SIEM/WORM/RBAC. |
| P2 | A zkVM receipt "mathematically proves the scheduler enforced all admission policies" | **OVERCLAIMS.** Proves correct evaluation of submitted requests; cannot prove all requests were submitted (§8.7.1 omission seam). "All" is unprovable from inside the boundary. |
| P3 | The core Scheduler + metering + policy rules compile to RISC-V | **PARTIALLY FALSE.** `scheduler.rs` is async tokio, I/O- and wall-clock-bound. Only an extracted pure policy-evaluation function (capability match, budget admission, tier legality) is provable. That extraction is real, useful engineering — and unbuilt. |
| P4 | 9–16 s proving doesn't bottleneck because execution is outside the zkVM | **CONTRADICTS THE PLAN'S OWN FLOW**, which puts the zkVM at action-request time ("when a sandboxed agent requests an action, the zkVM evaluates"). Inline = 9–16 s admission latency on a runtime whose CoS makes thousands of calls/day. The workable shape is native enforcement in real time + batch re-execution proof after the fact. |
| P5 | The Egress Hash Chain is to-be-built | **ALREADY ~80% EXISTS** (`evidence.rs`: Ed25519 chain, rotation, verifier). What's missing is COVERAGE (inference-only today → ux.6b B2) and KEY CUSTODY (self-attestation → customer-held/countersigned). The plan rebuilds a thing that exists and skips the two gaps that matter. |
| P6 | Timing: build this now | **CONFLICTS with three ratified decisions:** mv doc rejected ledger-first; ux.6b deferred with named gate conditions (none true); roadmap next = attn.4, and four consecutive CEO voices ranked naming design partners above governance engineering. |

**What happens if we do nothing:** the runtime keeps its honest de-claims (§8.7.1), attn.4
restores the brief, and the zk idea waits for a design partner to ask. No user-visible pain
today; the pain is hypothetical until a partner exists.

## 0B. Existing Code Leverage

| Sub-problem | Existing code |
|---|---|
| Tamper-evident append-only receipts | `evidence.rs` (chain, rotation, tail verify) — built |
| Offline verifier | `agentctl verify <segment> <pub>` — built |
| Denial receipts | ux.6a wired native scheduler admission — built |
| Complete action record | `flight.jsonl` (every action class, unsigned) — built; signing projection = ux.6b B2 |
| Policy evaluation logic | `capability_covered_by`, tier_legality resolver, budget admission gate — built, but interleaved with async scheduler state |
| zkVM guest/prover/verifier | nothing — new, heavyweight |
| Key custody outside boundary | nothing — named in TODOS as the increment "that would actually change what can be claimed" |

## 0C. Dream State

```
CURRENT STATE                      THIS PLAN (as written)               12-MONTH IDEAL
Signed inference-only chain,       + zkVM inline authorization          Governed-agent runtime a paying
self-attested key, unsigned        + rebuilt per-action hash chain      design partner audits: complete
flight log; honest de-claims;      + "mathematical certainty" claim     signed action record (B2), key
no design partners named           (omission seam survives, claim       custody outside boundary, SIEM/
                                   overclaims; partners still 0)        WORM export; zk policy-privacy
                                                                        IF a partner demands it
```

The plan moves toward the ideal on ~1 of 4 axes (verification tech) and away on claim-honesty
(re-introduces the exact overclaim ux.6a spent an increment retracting).

## 0C-bis. Implementation Alternatives

```
APPROACH A: The brief as written (inline zkVM authorization + new hash chain)
  Effort:  XL (human: months / CC: weeks — new crate, RISC-V guest, GPU/hosted proving, CI)
  Risk:    High (latency architecture contradiction; light-runtime invariant broken;
           claim overclaims; zero partner demand evidence)
  Reuses:  little — rebuilds evidence.jsonl
  Completeness for the stated goal: 4/10 (proves the wrong "all")

APPROACH B: Evidence-first (ux.6b B2 + key custody + audit packet) — zk deferred
  Summary: sign a projection of flight.jsonl (full action coverage), move the verify key
           outside the boundary (customer-held key + out-of-band (pubkey, max-seq) anchor,
           optional RFC 3161 timestamping), ship `agentctl audit-packet` bundler.
  Effort:  M (human: ~2 wks / CC: ~2-3 days)   Risk: Low
  Reuses:  evidence.rs, flight.jsonl, agentctl verify, obs export
  Completeness: 8/10 (closes coverage + custody + rewind-detection; omission-by-
           process-kill remains, documented)

APPROACH C: zk batch re-execution proof as the mv demo differentiator
  Summary: extract pure policy-eval crate (no_std-compatible; good engineering regardless);
           nightly/session-end batch proof via SP1/RISC Zero that every logged admission
           decision re-evaluates to the same verdict under the (private) policy — proof
           attached to the audit packet. Native enforcement stays real-time.
  Effort:  L (human: ~4-6 wks / CC: ~1-2 wks)   Risk: Med (dep weight quarantined in a
           separate prover binary, NOT in agentd; proving cost ~$0.02-0.10/batch)
  Completeness: 7/10 for the policy-privacy claim; still cannot prove completeness
  Gate:    a named design partner asking for policy-privacy proofs
```

**RECOMMENDATION: B now-ish (still behind attn.4 per roadmap order), C gated on a partner ask,
A rejected as stated.** B is the only approach that closes gaps §8.7.1 already names; C is the
honest version of the zk idea (batch replay proof, never inline); A's inline-authorization shape
contradicts its own latency section and re-introduces a retracted claim.

## 0D. SELECTIVE EXPANSION analysis

Complexity check: Approach A touches scheduler, new workspace crate, dependency tree, CI, and
the distro image — well past the 8-file smell with two new services (prover, verifier). Minimum
change achieving the stated *outcome* (tamper-evident complete audit + defensible policy-privacy
story) is B, with C as the additive proof layer.

Expansion candidates (cherry-pick, auto-decided per P2 blast-radius rule):
1. Out-of-band `(pubkey, max_seq)` anchor publishing — S, in blast radius → **fold into B** (it is
   the cheapest rewind detection and §8.7.1 names it).
2. RFC 3161 timestamp of segment heads — S/M → **defer to TODOS** (needs an external service
   choice; not required for the packet to be useful).
3. `agentctl audit-packet` bundler — S, in blast radius → **fold into B**.
4. SIEM/OTel export schema doc — S, partially exists (obs.1–3) → **fold into B as a doc task**.
5. Customer-held key ceremony doc — S → **fold into B** (custody is B's core).

## 0E. Temporal Interrogation (for whichever approach survives the gate)

- HOUR 1: which checks constitute "the policy"? (capability match, budget admission, tier
  legality — name them; everything else is out.) Where is the pure boundary drawn?
- HOUR 2–3: determinism — budget windows read the clock; config reads disk. All inputs must
  become explicit arguments or the extraction (and any future proof) is fiction.
- HOUR 4–5: key ceremony UX; anchor publication target; packet format versioning.
- HOUR 6+: prover-down semantics for C — fail-closed bricks the runtime, fail-open makes the
  proof decorative; must be decided in the plan, not discovered in prod.

## 0F. Mode

SELECTIVE EXPANSION (autoplan override), committed. Approach decision goes to the premise gate
as it is inseparable from the premise verdicts.

## Step 0.5 — Dual Voices

### CLAUDE SUBAGENT (CEO — strategic independence)
9 findings: F1 CRITICAL — "mathematically proves policies are enforced" is false at the trust
boundary (proves correct evaluation of *submitted* inputs; completeness stays self-attested; an
artifact that looks like certainty while carrying the omission seam is an overclaim generator —
the failure mode ux.6a existed to retire). F2 CRITICAL — wrong problem vs the 2026-10-01 gate
(0/10, 0/3); relitigates the mv doc's rejected Approach C at higher cost. F3 HIGH — auditors
accept immutability by storage, evaluated against frameworks they know; a 250 KB STARK receipt
maps to no control in any framework; hiding rules from the verifier is the opposite of an audit.
F4 HIGH — scheduler not compilable as claimed; dual compilation guarantees enforced-vs-proven
drift (a false-green *machine*); 9–16 s inline is ~4–6 actions/min; state provenance recreates
the completeness problem inside the proof. F5 MED — pipeline 2 already exists (evidence.rs);
plan misdescribes it as new/immutable and routes through a proxy that never starts in
production. F6 MED — TEE attestation, external anchoring (RFC 3161/Rekor), customer-held key,
WORM export all dominate zk on the real threat model and were never analyzed. F7 HIGH —
6-month regret: gate passes with 0 partners; buyer's engineer asks "who holds the key?" and
"what stops the host from not submitting an action?" and the story collapses live. F8 MED —
no moat in zk for a single-dev runtime; the demoable moat is enforcement + complete record.
F9 — reframe: the artifact that makes a partner's security team say yes = B2 + custody +
SIEM/WORM export. **Verdict: REJECT/DEFER zk.1 entirely; sequence attn.4 → partner naming →
custody+export+B2 if pulled; zk only as batch replay, never inline.**

### CODEX SAYS (CEO — strategy challenge)
"Reject as written — strategically incoherent: cryptographic theater while the actual gate is
commercial validation." zk gives "mathematical certainty about a subset chosen by the party
being audited." Inline 9–16 s proving "is not an authorization path; that is a denial-of-service
primitive against your own runtime" — if proving is async it is not enforcement, so label batch
replay proofs as audit evidence, never authorization. RISC Zero/SP1 in agentd violates the
architecture; "if zk ever happens it belongs in a separate prover binary, never in PID 1."
The hash chain substantially exists; the real gaps are coverage + key custody (ux.6b). Reframe:
"governed-agent runtime with real enforcement, complete audit packet, customer-owned evidence
custody, and SIEM/WORM integration." Foolish-in-6-months: zkVM infra before one buyer demands
it; docs claiming auditor-grade certainty while the host controls input completeness — "that
claim will collapse under the first serious security review." Replacement sequence: attn.4 +
partner outreach now; ux.6b B2 + external key custody + audit packet + SIEM/WORM next; batch zk
prover only if pulled by a named buyer.

### CEO DUAL VOICES — CONSENSUS TABLE
```
═══════════════════════════════════════════════════════════════
  Dimension                            Claude  Codex  Consensus
  ───────────────────────────────────  ──────  ─────  ─────────
  1. Premises valid?                   NO      NO     CONFIRMED invalid
  2. Right problem to solve?           NO      NO     CONFIRMED wrong-now
  3. Scope calibration correct?        NO      NO     CONFIRMED miscalibrated (XL, wrong seq)
  4. Alternatives sufficiently         NO      NO     CONFIRMED insufficient (TEE/anchor/
     explored?                                        custody/WORM unexamined)
  5. Competitive/market risks covered? NO      NO     CONFIRMED uncovered
  6. 6-month trajectory sound?         NO      NO     CONFIRMED unsound
═══════════════════════════════════════════════════════════════
6/6 adverse, cross-model. Zero disagreements → no taste decisions from this phase.
One USER CHALLENGE: both models recommend rejecting the stated direction.
```

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|----------|
| 1 | CEO | Review mode = SELECTIVE EXPANSION | Mechanical | autoplan override | Feature-track proposal on an existing subsystem | other modes |
| 2 | CEO | Fold anchor-publishing, audit-packet bundler, SIEM schema doc, key-ceremony doc into Approach B | Mechanical | P2 blast radius, <1d CC each | All inside evidence.rs/agentctl blast radius; each closes a §8.7.1-named gap | building them standalone |
| 3 | CEO | Defer RFC 3161 timestamping to TODOS.md | Mechanical | P3 pragmatic | Needs an external-service choice; packet useful without it | inclusion now |
| 4 | CEO | Approach A/B/C selection | USER CHALLENGE | never auto-decided | Both models reject A (the user's stated direction) and converge on B-then-C-gated | — |

## Premise gate round 1 (2026-08-06) — operator relaxes F4

Operator decision at the gate: **inline zk authorization is dropped.** The zk machinery runs
post hoc; a verifier sweep runs every few hours across the fleet ("thousands of agents" —
i.e. the mv fleet-scale deployment, not the single-tenant box). Inline proving is explicitly
"later, maybe" — not in this increment.

Consequences ratified into the plan:
- The zkVM component becomes a **batch replay prover in a separate binary** (never in agentd,
  never in the PID-1 boot path) proving: every admission decision in the published log
  re-evaluates to the same verdict under the (private) policy.
- The live enforcement path and the prover MUST share one **pure policy-eval crate** (single
  compiled artifact) or the proof attests a policy that is not the one enforced (F4b drift).
- The honest claim is **policy-consistency of the published log**, not "proof of enforcement."
  F1 (completeness self-attested) and custody remain open and are addressed only by the
  ux.6b-B2 coverage work + key/anchor custody work, not by the proof.

## Premise gate round 2 (2026-08-06) — operator decisions D2/D3

- **D2 = A:** gov.1 (coverage + custody + audit packet) before any zk work, and the zk track
  is mapped as named increments now so zk lands useful, not decorative.
- **D3 = A:** attn.4 stays first in the build queue; partner naming proceeds in parallel.

---

# RESHAPED OUTCOME — the zk track, sequenced to be useful

**Claim discipline (binding on every increment below):** docs and demos may claim
*"policy-consistency of the published, custody-anchored log"* — never "proof of enforcement"
or "mathematical certainty that all policies were enforced." Anchoring cadence narrows the REWIND seam
only — deletion/truncation of already-journaled records (see zk.3). The omission seam
(§8.7.1: actions never journaled at all) is untouched by any cadence and closes only for
tee.1's brokered action class; say so wherever the packet is described. (Corrected at the
2026-08-06 red-team pass — the prior wording here recommitted the §8.7.1 overclaim.) This is the ux.6a lesson applied prospectively.

## Queue position

1. **attn.4** — scheduler-native cron (roadmap next; unchanged).
2. **Parallel, zero engineering:** name mv design partners (gate 2026-10-01, 0/10, 0/3).
3. Then the ladder below, one branch per increment, each through its own /autoplan.

## gov.1 — Auditor-grade evidence substrate (pre-zk, REQUIRED)

The increment that changes what can be claimed. Effort M (human ~2 wks / CC ~2–3 days).
- **Coverage (ux.6b B2):** sign a *projection* of `flight.jsonl` — tool invocations,
  capability verdicts, approvals/denials, cancels, budget mutations, egress denials — batched
  off the hot path (a projection writer, NOT a per-call fsync; ux.6b's B1 fsync analysis is
  controlling). Prereq already landed: ux.6a boot-read/rotation de-trap.
- **Custody (wording corrected at red-team, RT-10/C4):** the SIGNING key stays on the host —
  unavoidable without tee.1. What gov.1 moves outside the boundary is verification trust:
  customer-PINNED verify key with an enrollment step (so a host re-mint is an alarm, not a
  fresh history), out-of-band `(pubkey, max_seq)` anchor publication, countersigning hook.
  Claim ceiling: rewind- and key-swap-DETECTION, not third-party evidence of conduct —
  §8.7.1's "the signer is the audited party" survives gov.1 intact.
- **Packet (operator decision D6, 2026-08-06):** `agentctl audit-packet` bundler produces
  BOTH variants from day one — **full** (chain segments + signed projection + anchors +
  schema version; verifier = the policy-owning security team, plaintext receipts readable)
  and **commitment-only** (commitments + anchors + proof slots; verifier = external party).
  Red-team warning accepted as known risk: until gov.2's trust root exists, the
  commitment-only variant verifies little — its format must version so gov.2/zk.2 can
  strengthen it without churn.
- **Export:** documented SIEM/OTel schema for the projection (obs.1–3 already export
  flight events; this is a schema doc + gap check, not a new exporter).
- NOT in gov.1: RFC 3161 timestamping (TODOS: needs an external-service decision), TEE
  attestation (recorded as considered; pairs with mv.3, revisit there), any zk dependency.

## gov.2 — Auditor-side trust root (SCHEDULED by operator decision D5, 2026-08-06)

Operator override: both red-team voices recommended named-but-unscheduled; the operator
scheduled it as the rung after gov.1. Scope (from RT-2/C9):
- **Key enrollment:** a host's evidence signing key is registered once; a re-minted key is an
  alarm, not a fresh history (closes the `EvidenceWriter::open` re-mint seam for enrolled
  fleets).
- **Anchor registry:** the auditor-held record of `(agent id, pubkey, anchored seq ranges)`.
- **Expected-batch registry:** enrollment, cadence SLA, per-agent sequence continuity,
  offline/decommission semantics — what makes "missing batch" a detectable event.
- **Image pinning:** prover/verifier artifact hashes mapped to reviewed source (reproducible
  builds needed by zk.2; the registry schema lands here).
- **Known tension its /autoplan must resolve:** the red team held this presumes mv.1 (vsock
  control plane) at minimum; scheduled anyway. The /autoplan must either scope a
  control-plane-free v1 (e.g. registry as files in customer-held storage) or explicitly
  declare the mv.1 dependency and hold.

## zk.0 — Pure policy-eval extraction (zero ZK dependencies)

Extract a `policy-eval` workspace crate: capability matching (`capability_covered_by`), tier
legality, budget-admission verdicts as pure functions `f(request, policy, state_snapshot) →
verdict` with ALL inputs explicit (clock, config, window state passed in, never read).
The live scheduler path consumes this same crate — one compiled artifact, so the policy that
will later be proven IS the policy enforced (kills F4b drift by construction).
- Acceptance: zero behavior change (mutation-tested equivalence against the pre-extraction
  verdicts on recorded fixtures); crate is `no_std`-compatible; no new deps in agentd.
- Worth building even if zk never happens: testability + drift-proofing. Can be built before
  a partner ask without violating the gate.

## zk.1 — Decision journal + native replay verifier (still zero ZK dependencies)

- Emit a canonical, deterministic **admission decision record** (explicit inputs + verdict,
  stable serialization) into gov.1's signed projection.
- `agentctl verify-decisions`: native re-execution of `policy-eval` over the journal,
  confirming every logged verdict reproduces. This is the *exact statement* the zk proof will
  later make — checked natively first, for free, at native speed.
- Acceptance: a verifier sweep over N days of real dogfood journal reproduces 100% of
  verdicts; any nondeterminism (clock, config drift) is found HERE, not inside a circuit.

## zk.2 — Batch replay prover (first ZK dependency; PARTNER-GATED)

- **Gate:** a named design partner asking for policy-privacy proofs in writing (ux.6b's gate
  condition), or an explicit operator override recorded at the increment's /autoplan.
- Separate prover binary/workspace member (SP1 or RISC Zero guest wrapping the `policy-eval`
  crate). NEVER a dependency of agentd; never in the PID-1 boot path; 6 MB CI guard untouched.
- Proves per batch (session/day): "every decision record in this committed journal range
  re-evaluates to the same verdict under a policy whose commitment is X" — policy private,
  journal commitments public. Proof attached to the audit packet.
- **Open decision for its /autoplan:** local-GPU vs hosted proving (Bonsai/Succinct). Hosted
  proving ships the policy to the proving service as witness — which defeats
  "without revealing proprietary rules" unless the service is trusted; local GPU proving
  (~$0.02–0.10/batch at 2026 prices) keeps the policy on-box. This tension must be resolved
  before building, not after.
- Prover-down semantics: fail-open with a LOUD gap record in the packet (a missing proof is
  visible, never silently absent); enforcement is native and unaffected.

## zk.3 — Fleet verifier sweep (mv-scale)

The operator's "verifier every few hours across thousands of agents": a control-plane job
that verifies packets + anchors across hosts and alerts on gaps. **Corrected at the red-team
pass (RT-3/C9):** what this cadence buys is REWIND/TRUNCATION detection at batch granularity
— deletion of already-journaled records after anchoring is detectable. Omission of
never-journaled actions is NOT detected: an action that never entered admission produces no
sequence gap; the in-process writer simply never writes the line. And "missing batch = alert"
requires an auditor-side expected-batch registry (enrollment, cadence SLA, per-agent sequence
continuity, offline/decommission semantics) that no current rung builds — see the gov.2 open
decision in the red-team addendum.

## zk.4 (named, unscheduled) — inline/low-latency proving revisit

Only if a buyer demands enforcement-time proofs. Re-evaluate proving latency then; 2026
numbers (~9–16 s) make it a non-starter, per the round-1 gate decision.

## Deferred to TODOS.md

- `zk-01` (P3): RFC 3161 timestamping of segment heads — external-service choice needed.
- `zk-02` (P3): TEE attestation (Nitro/SEV-SNP/TDX) considered as the only tech addressing
  "did the host run this code"; pairs with mv.3's microVM tier — engage there, not before.

## GSTACK REVIEW REPORT (CEO phase — superseded by the red-team addendum at end of file)

| Run | Status | Findings |
|-----|--------|----------|
| CEO phase (autoplan, SELECTIVE EXPANSION) | complete | 6 premises challenged: P2/P3/P4 wrong as stated, P1/P6 assumed/conflicting, P5 rebuilds existing code |
| Claude CEO subagent (independent) | complete | 9 findings: 2 CRITICAL (F1 omission overclaim, F2 opportunity cost), 3 HIGH, 3 MED, 1 reframe |
| Codex CEO voice (gpt-5.5, read-only) | complete | Reject-as-written; inline zk = "DoS primitive against your own runtime"; converged on identical replacement sequence |
| Consensus | 6/6 adverse | Zero disagreements; one USER CHALLENGE, resolved at gate rounds 1–2 |
| Eng / Design / DX phases | not run | Premise gate reshaped the increment (ux.6-precedent); each ladder increment gets its own /autoplan at pickup |

VERDICT: RESHAPED — inline-zkVM design rejected (CROSS-MODEL absorbed); ladder
attn.4 → gov.1 → zk.0 → zk.1 → zk.2 (partner-gated) → zk.3 ratified by operator (D1=interrogate,
F4 relaxed to post-hoc; D2=A; D3=A).

NO UNRESOLVED DECISIONS

---

# Post-gate amendments (2026-08-06, F6 exploration + operator framing)

## Operator's value framing for zk.2/zk.3 (binding context for future increments)

The zk artifact's purpose is a **privacy-preserving compliance check run by an auditor-held
verifier across a fleet of agents**: the auditor (or a customer's security team / an external
compliance service) runs the verifier over many agents' audit packets on a few-hours cadence
and learns ONLY pass/fail per batch — never the policy contents, never the action payloads.
This is the fleet-scale mv deployment story, not a single-box feature.

## Amendments from the TEE/anchoring exploration

1. **gov.1 open decision (promoted from zk-01/P3):** the anchor mechanism must be decided at
   gov.1's /autoplan — bare out-of-band file vs RFC 3161 TSA vs Rekor-style transparency log
   (they compose). Public-log metadata leakage vs TSA single-trust noted.
2. **mv.0 criterion added:** confidential-guest support joins the FC-vs-CH decision criteria.
   As of 2026-08: Cloud Hypervisor v52 (release note 2026-05-14,
   cloudhypervisor.org/blog/cloud-hypervisor-v52.0-released) launches SEV-SNP guests on KVM
   with measured boot; Firecracker's public feature list shows no documented native SNP/TDX
   support. Both statements are time-sensitive — re-verify at mv.0 before they decide a
   hypervisor (red-team RT-11/C12).
3. **tee.1 filed (named, unscheduled):** "attested governor" — policy-eval crate + evidence
   writer + credential broker inside a measured SEV-SNP guest, signing key sealed to the
   measurement. Key insight to preserve: credentials sealed in the enclave make the audit
   path STRUCTURALLY unavoidable for brokered actions — the only mechanism in the reviewed
   space that closes (rather than narrows) the omission seam, for that action class.
   Gated on mv.1 (vsock control plane) + a partner deployment on TEE-capable x86 hardware.
   Cannot be dogfooded on the Mac.

---

# Red-team addendum (2026-08-06) — adversarial pass on the RESHAPED ladder

Operator-requested. Two independent voices attacked the ladder + amendments + the operator's
fleet-auditor framing ("auditor-held verifier sweeps a fleet's packets every few hours, learns
only pass/fail, never policy or payloads"). Claude red-team (worktree-safe, repo-verified):
RT-1..RT-12. Codex (gpt-5.5, read-only): C1..C12. **Cross-model verdict: the ladder's ordering
discipline (native-first, prover-out-of-PID-1, partner gates) is right; the fleet-auditor zk
story as framed is not yet a coherent security claim.**

## Consensus map (both voices, independently)

| Theme | Claude | Codex | Severity |
|---|---|---|---|
| Proof binding undefined: without public agent id + log root + policy commitment, one agent's proof replays for another; with them, auditor learns far more than pass/fail | RT-1/2 | C1 | CRITICAL |
| Policy commitment X never bound to the DEPLOYED policy — host proves against permissive policy, enforces another; unsalted commitment is dictionary-attackable (low-entropy TOML), salted kills continuity detection | RT-1 | C2 | CRITICAL |
| "Never payloads" contradicted by the packet itself: chain receipts carry plaintext (action, target, principal); flight projection carries tool inputs; verifying the chain REQUIRES reading receipts | RT-4 | C8 | CRITICAL |
| "Fail" doesn't exist: a violating host just doesn't submit a proof; real signal is present/absent, and fail-open gap records make outages indistinguishable from cover-ups at fleet scale | RT-4 | C3 | CRITICAL |
| Auditor-side trust root unbuilt: key enrollment (EvidenceWriter re-mints keys at will today), anchor registry, expected-batch registry, guest-image↔source pinning (reproducible builds) — no rung constructs any of it | RT-2 | C9 | CRITICAL |
| zk.3/preamble recommitted the §8.7.1 omission overclaim (anchoring detects rewind, not omission) | RT-3 | C9 | CRITICAL — **fixed in this file** |
| gov.1 custody wording confused verify-key with signing-key custody | RT-10 | C4 | HIGH — **fixed in this file** |
| gov.1 signs a projection of a DESIGNED-LOSSY log (best-effort writes, copy-truncate rotation); batching leaves a systematically unsigned pre-crash tail (SIGTERM lands mid-tool-call, measured in attn.3); group-commit never analyzed | RT-6 | C5 | HIGH |
| zk.0 "zero behavior change" under-scoped: admission path entangles wall-clock rebases, meter/queue/slot state, defer-vs-deny side effects; no_std conflicts with std::path-based capability matching (FS_ANCHOR OnceCell); acceptance fixtures can't exist before zk.1's journal — rungs mis-ordered as stated | RT-7 | C6 | HIGH |
| zk.1 determinism vs runtime policy mutation (SetBudget/SetCaps write checkpointed config; spawn attenuation; approvals): either commitment churn (leaks operator behavior) or everything decision-relevant moves into self-attested state; journal/crate versioning unaddressed | RT-8 | C7 | HIGH |
| Ladder economics: zk.0+zk.1 are unconditional governance prework ahead of the gate that outranks them; realistic pre-gate capacity = attn.4 + gov.1 | RT-9 | C10 | HIGH |
| tee.1 "structurally unavoidable" narrower than framed: brokered class only; allowed-channel exfil still invisible; measured code ≠ measured POLICY CONFIG; SEV-SNP sealed state needs rollback protection (external monotonic counter) | RT-11 | C11 | CRITICAL(Codex)/MED(Claude) |
| Metadata leakage even in commitment-only variant: batch counts, timing, commitment-change events = operator interventions; "pass/fail only" should read "pass/absent + traffic-analysis metadata" | RT-12 | C8 | MED |
| Amendment hygiene: future-dated header; CH52/Firecracker claims uncited | RT-11 | C12 | MED — **fixed in this file** |

**Claude-only, flagged regardless (RT-5, CRITICAL-premise):** in the mv deployment model the
verifier-holder is the customer's security team — who OWNS the policy. Policy privacy from the
auditor only matters when auditor ≠ policy-owner (e.g. an external compliance service auditing
competing tenants). If no such party can be named in writing, zk.2 adds zero over zk.1's
native replay verifier run fleet-wide on cron, and the framing quietly re-imports rejected
premise P1. Unpriced: few-hours proving across thousands of agents ≈ $120–600+/day per 1k
agents for a check the native verifier does in milliseconds for free.

## The sharpened zk.2 gate (adopted — strengthens the existing partner gate)

zk.2's gate condition is now: **a party who must verify policy compliance but must NOT see the
policy, named in writing** (plus the existing named-design-partner requirement). If every
candidate verifier is the policy owner, zk.2/zk.3-as-zk collapse into "run
`agentctl verify-decisions` fleet-wide on cron" — a zk.1 feature.

## Questions the next /autoplan runs MUST answer (from this pass)

- **gov.1:** projection source of truth (in-memory event stream, NOT tailing the
  copy-truncating file); bounded unsigned tail + signed gap record on restart; group-commit
  vs batch analysis; canonical serialization + schema version ownership across heterogeneous
  agentd versions; SIEM redaction delta (OTEL_REDACT_PREVIEWS=true means SIEM copy ≠ signed
  record byte-for-byte).
- **zk.0:** snapshot semantics as SPEC (single clock read, materialized state struct, sampling
  point named in scheduler.rs) declared as a behavior change; pure-verdict vs state-transition
  split; drop or justify no_std given std::path capability matching; acceptance corpus via
  temporary shadow-journal from live dogfood.
- **zk.1:** policy/state partition (what is committed policy vs self-attested state);
  journal schema version + policy-eval crate hash in every record; verifier holds N versions.
- **zk.2:** public-input set + replay domain separator; policy-commitment registration
  ceremony (who pins X, where, rotation authorization); salted-vs-unsalted commitment
  decision; proving cost model at fleet cadence.
- **zk.3:** two packet variants (full: policy-owning security team; commitment-only: external
  verifier — and what the latter actually verifies); expected-batch registry ownership;
  batch padding/bucketing vs traffic analysis.
- **tee.1:** action-class boundary stated explicitly with bypass list ("no ambient
  credentials / no raw network" as PRECONDITION, not implication); policy config into the
  measurement or the RT-1 ceremony; sealed-state rollback protection.

## GSTACK REVIEW REPORT

| Run | Status | Findings |
|-----|--------|----------|
| CEO phase (autoplan, 2026-08-06) | complete | Original inline-zkVM design rejected 6/6 cross-model; reshaped to ladder at premise gate |
| Red-team pass (operator-requested, 2026-08-06) | complete | Claude RT-1..12 + Codex C1..12; 6 CRITICAL themes confirmed cross-model; 3 textual defects fixed in-file; zk.2 gate sharpened |
| Eng / Design / DX phases | not run | Each ladder increment gets its own /autoplan at pickup; the per-increment question lists above are their mandatory inputs |

VERDICT: LADDER STANDS with corrections absorbed (CROSS-MODEL absorbed); fleet-auditor zk
framing DOWNGRADED from claim to hypothesis pending the named-party gate; the three
structural decisions were resolved by the operator on 2026-08-06:

**Operator resolutions (round 3, 2026-08-06):**
- **D4 = keep ladder as ratified** (zk.0/zk.1 stay unconditional after gov.1/gov.2).
  OPERATOR OVERRIDE of the cross-model recommendation (both voices: gate them with zk.2).
  The override stands, attn.1b-precedent style; the red team's warning is on the record —
  zk.0's acceptance corpus needs a shadow journal, so its /autoplan must confront the
  rung-ordering finding (RT-7) regardless.
- **D5 = gov.2 scheduled** as the rung after gov.1. OPERATOR OVERRIDE (recommendation was
  named-but-unscheduled). Its /autoplan must resolve the mv.1 tension recorded in its scope.
- **D6 = both packet variants designed in gov.1** (full for the policy-owning team,
  commitment-only for external verifiers). OPERATOR OVERRIDE of the internal-only
  recommendation; decorative-format risk accepted and recorded at the gov.1 packet bullet.

NO UNRESOLVED DECISIONS
