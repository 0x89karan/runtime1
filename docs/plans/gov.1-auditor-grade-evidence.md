# gov.1 — Auditor-grade evidence substrate (pre-zk, REQUIRED)

**Status: DEFERRED at the /autoplan premise gate, 2026-08-08.** Not scheduled. 6/6 adverse
cross-model CEO consensus (both voices DEFER), presented to the operator as a User Challenge;
operator chose to go straight to product data — bring the stack up, confirm the shadow cycle,
flip `native_cron_shadow`, start the 14-day measure, then `audit-S3`.
**Deferred, not struck:** the gap is real and the analysis below is intact. Three findings were
filed as standalone TODOs so they do not die with this deferral (`gov1-evidence-durability-01`,
`gov1-capdenied-01`, `gov1-claimfix-01`). Revisit when a buyer is named or the operator's
"functional for my need first" condition is met.
**Ladder position:** `attn.4 ✅ → **gov.1** → gov.2 → zk.0 → zk.1 → zk.2 (partner-gated) → zk.3`
Ladder ratified by operator 2026-08-06 (`docs/plans/zk.1-hybrid-crypto-verification.md`).
attn.4 shipped v0.122.0, so this is the next rung.
**Ancestor:** the DEFERRED `ux.6b` signed action ledger — specifically its **B2** substrate
(sign a projection of `flight.jsonl`), which ux.6b's own analysis preferred over B1
(per-action receipts) on fsync grounds.
**Effort as scoped upstream:** M (human ~2 wks / CC ~2–3 days).

---

## Objective

Make the evidence record **auditor-grade**: widen what is attested beyond inference calls,
move *verification trust* outside the host boundary, and ship a bundler that produces an
audit packet an external party can check.

This is "the increment that changes what can be claimed." Today the chain proves *what the
agent asked the model*. Nothing else an agent does carries attestation.

## Scope (as inherited from the ratified ladder)

**T1 — Coverage (ux.6b B2).** Sign a *projection* of the flight event stream covering tool
invocations, capability verdicts, approvals/denials, cancels, budget mutations, and egress
denials. Batched off the hot path — a projection writer, NOT a per-call fsync. ux.6b's B1
fsync analysis is controlling. Prereq already landed: ux.6a's boot-read/rotation de-trap.

**T2 — Custody.** The SIGNING key stays on the host (unavoidable without `tee.1`). What moves
outside the boundary is *verification trust*: a customer-PINNED verify key with an enrollment
step, out-of-band `(pubkey, max_seq)` anchor publication, and a countersigning hook.
**Claim ceiling: rewind- and key-swap-DETECTION, not third-party evidence of conduct.**
THREAT_MODEL §8.7.1 ("the signer is the audited party") survives gov.1 intact and must not be
re-claimed away.

**T3 — Audit packet (operator decision D6).** `agentctl audit-packet` produces BOTH variants
from day one: **full** (chain segments + signed projection + anchors + schema version;
verifier = the policy-owning security team, plaintext receipts readable) and
**commitment-only** (commitments + anchors + proof slots; verifier = external party).
Accepted known risk: until gov.2's trust root exists the commitment-only variant verifies
little, so its format must version so gov.2/zk.2 can strengthen it without churn.

**T4 — Export.** A documented SIEM/OTel schema for the projection. obs.1–3 already export
flight events, so this is a schema doc + gap check, not a new exporter.

**NOT in gov.1:** RFC 3161 timestamping (`zk-01`, needs an external-service decision), TEE
attestation (pairs with `mv.3`), any zk dependency.

---

## PRE-FILED TENSIONS — this /autoplan must consume every one

The ladder's own rule: *"the per-increment question lists are their mandatory inputs."*

### PT-0 (NEW, P1) — the denial class this projection covers is structurally empty

Filed as `gov1-premise-01` from the p7.7-ar-03 premise gate, 2026-08-08. Full evidence:
`docs/plans/p7.7-ar-03-egress-counters-http.md`.

T1's coverage list names **"egress denials"**. In the shipped configuration that class never
fires, and neither do native budget denials:

| Path | Gate | Shipped value | Fires? |
|---|---|---|---|
| `egress_denied` event | `[egress] proxy_addr` | **unset** in both configs | no — proxy never starts (`egress.rs:637`) |
| `receipt_denial_once` (signed refusal receipt) | `if sched.budget_reset_interval == 0` | **86400** | no |
| `AgentAdmissionDenied` `:1999`, `:2082` | legacy no-window paths only | **86400** | no |
| `AgentAdmissionDenied` **`:2178`** | **ungated** — records, THEN `if reset_interval > 0` defers | — | **YES** |
| `AgentAdmissionDenied{reason:"shutdown"}` `:1294`, `:1971` | ungated | — | yes, but ux.6a settled shutdown is not a policy verdict |

Under a window, budget exhaustion is a **deferral** (ux.8′), and `scheduler.rs:2192` forbids
receipting a deferral.

**Corrected 2026-08-08 after the Codex CEO voice caught an overstatement in this section's
first draft** (which claimed 4 of 5 admission sites were gated — that came from a
nearest-preceding-string heuristic, not brace analysis). The accurate statement:

- **No REFUSAL is ever signed** in the shipped config. `egress_denied` never fires; no signed
  refusal receipt fires. That half of PT-0 stands, and it is the load-bearing half.
- **But real signal DOES exist**: `agent_admission_denied` at `:2178` fires whenever an agent
  hits a budget ceiling, followed by a deferral. It is admission *pressure*, not refusal.
  Nothing in `agentctl/` or `surfaces/` counts it.

**So the defect is not "nothing to sign" — it is "the label would lie."** If T1 signs a
"denials" class populated by egress denials, that class is empty forever. If it signs
`agent_admission_denied` under a "denials" label, it misdescribes a deferral as a refusal —
putting the boundary on record as refusing work it is actually going to do, which is the exact
error `scheduler.rs:2192` refuses to make in the receipt path. Either way, ux.6a's
`record_denied`-with-zero-callers defect gets recreated one layer up **with a signature on it**.

**This /autoplan must resolve:** the projection covers the classes that actually fire
(`agent_admission_denied` with its reason, **deferrals as deferrals**, `ActionReceiptEmitted`),
labelled honestly — or gov.1 states plainly in THREAT_MODEL that refusal coverage is
empty-by-construction under the default config. Silently signing an empty or mislabelled class
is not an option.

### PT-1 (RT-6 / C5, HIGH) — signing a projection of a designed-lossy log

`flight.jsonl` is best-effort by invariant ("logging must never crash an agent") and rotates
by copy-truncate. Batching leaves a **systematically unsigned pre-crash tail** — and attn.3
*measured* SIGTERM landing mid-tool-call. Group-commit was never analyzed.

Must answer: projection **source of truth is the in-memory event stream, NOT a tail of the
copy-truncating file**; bounded unsigned tail + a **signed gap record** on restart;
group-commit vs batch tradeoff with numbers.

### PT-2 — canonical serialization + schema version ownership

Heterogeneous agentd versions across a fleet must produce byte-stable, verifiable records.
Who owns the schema version, and how does a verifier hold N versions?

### PT-3 — SIEM redaction delta

`OTEL_REDACT_PREVIEWS=true` means the SIEM copy is **not byte-for-byte** the signed record.
An auditor reconciling the two will find a mismatch. State the relationship explicitly.

### PT-4 (RT-4 / C8, CRITICAL) — "never payloads" is contradicted by the packet itself

Chain receipts carry plaintext (action, target, principal); the flight projection carries
tool inputs; verifying the chain REQUIRES reading receipts. The full-variant packet cannot
claim payload privacy. Wording must not regress into that claim.

### PT-5 (RT-4 / C3, CRITICAL) — "fail" does not exist

A violating host simply does not submit. The real signal is present/absent, and fail-open gap
records make outages indistinguishable from cover-ups at fleet scale. gov.1's anchor cadence
is where this is either addressed or explicitly deferred to gov.2's expected-batch registry.

### PT-6 (RT-9 / C10, HIGH) — ladder economics

Both red-team voices put realistic pre-gate capacity at **attn.4 + gov.1**. attn.4 is done.
That makes gov.1 the whole remaining pre-gate budget — so scope discipline here is the
schedule, not a preference.

### PT-7 (RT-10 / C4, HIGH — already fixed upstream, do not regress)

Earlier wording confused verify-key custody with signing-key custody. The signing key stays
on the host. Any prose implying otherwise is a regression.

---

## Live-context tensions specific to right now

- **The CoS stack is mid-dogfood and unvalidated.** attn.4's native cron ships in shadow mode
  (`native_cron_shadow = true`) and has **not** had a confirmed cycle. CLAUDE.md's own roadmap
  steps 2–4 (bring up with `up -d`, confirm shadow fire times, flip the flag, start the 14-day
  measure) are still open with zero days of data. gov.1 is ~2–3 CC days that do not advance it.
- **`audit-S3` (P1) is open**: `SecretRewriter` is documented as shipped and is absent, in a
  deployment holding live Gmail OAuth and now carrying an HTTP-reachable job trigger
  (THREAT_MODEL §9.5, added by attn.2-R5).
- **mv gate:** deprioritized by standing operator override (2026-08-07). Not a factor here.

## Open questions for the gate

1. **Coverage vs PT-0.** Does gov.1 widen the denial classes to what actually fires, or ship
   the empty class with an honest THREAT_MODEL note?
2. **Sequencing.** Does gov.1 start now, or after the shadow-mode confirmation closes the
   attn.4 loop (a ~1-day wait, not a ~2-week one)?
3. **Packet scope under PT-6.** D6 mandates both variants. Under the capacity finding, is
   commitment-only a format stub (versioned, mostly empty) or fully built?

---

# /autoplan Phase 1 — CEO REVIEW RESULT (2026-08-08)

## CEO DUAL VOICES — CONSENSUS TABLE

```
═══════════════════════════════════════════════════════════════════════
  Dimension                              Claude   Codex   Consensus
  ─────────────────────────────────────  ───────  ──────  ────────────
  1. Premises valid?                     NO       NO      CONFIRMED false
  2. Right problem to solve now?         NO       NO      CONFIRMED no
  3. Scope calibration correct?          NO       NO      CONFIRMED no
  4. Alternatives explored enough?       NO       NO      CONFIRMED no
  5. Named buyer / market risk covered?  NO       NO      CONFIRMED no
  6. 6-month trajectory sound?           NO       NO      CONFIRMED no
═══════════════════════════════════════════════════════════════════════
  Verdicts: Claude = DEFER.  Codex = DEFER.   6/6 CONFIRMED adverse.
```

### F1 (CRITICAL) — the deployment gov.1 would instrument does not exist

Not "mid-dogfood and unvalidated" as this plan's own Live-context bullet said. **Zero
containers, zero volumes.** The `cos-data` volume attn.3 measured its 414k-token finding
against is destroyed. attn.4's shadow mode has not had "no confirmed cycle" — it has had
**no cycle**, and no filesystem on which to have had one. CLAUDE.md roadmap steps 2–4 are all
downstream of one `docker compose up -d` that has not happened.

### F2 (CRITICAL) — the chain has never produced durable production evidence

`evidence.jsonl` exists in exactly one place in this repo: `agentd/tests/fixtures/`. A test
fixture. The contrast is self-demonstrating in `docker-compose.yml`: briefs are **bind-mounted**
(`${AGENTOS_OUTPUT_DIR}:/data/output`) and five survive on disk back to 2026-07-16.
`evidence.jsonl` and `flight.jsonl` live in the named `cos-data` volume and **are gone.**

**The signed chain was less durable than the unsigned markdown.** gov.1 proposes coverage,
custody, and a two-variant packet on a substrate that has not survived the operator's own
workflow, and lists retention nowhere. ux.6b — gov.1's ancestor — already recorded what
auditors accept: immutability by storage (WORM / Object Lock in the customer's account).
**One line bind-mounting the evidence path out of the named volume does more for
"auditor-grade" than T1+T2+T3 combined**, because it is the only one addressing a failure that
has actually happened.

### F3 (CRITICAL) — PT-0 corrected again, and the correction changes what to build

PT-0's *conclusion* is stronger than stated: under the shipped config **no denial receipt of
any kind is reachable**, so the chain is 100% `allowed` **by construction** — the exact
`record_denied`-with-zero-callers defect ux.6a retired, still live one layer down.

But the class analysis was incomplete in both earlier drafts. **`capability_denied` fires in
production, independently of budget and egress, from four sites** — `tools/mod.rs:197`,
`tools/mod.rs:222`, `scheduler.rs:2426`, and `scheduler.rs:2738` (the `RunJob` check that
attn.2-R5 just made HTTP-reachable) — **and has no receipt path at all.** Verified.

So Open Question 1 has a different answer than this plan assumed. It is not "cover what fires
vs. document empty." It is: **one denial class is real and unattested; two are structurally
unreachable.** And the real one — the runtime actually refusing a tool call — is the only item
here a buyer would care about. If coverage work is ever done, `capability_denied` is the whole
of it.

Also flagged: **CLAUDE.md's line asserting ux.6a "made denials real" is false** under the
shipped config, and it is now load-bearing in plans.

### F4 (CRITICAL) — PT-6's premise is void

PT-6 ("realistic pre-gate capacity is attn.4 + gov.1, so scope discipline here is the
schedule") is a capacity argument against the 2026-10-01 gate. **That gate is deprioritized by
standing operator override (2026-08-07).** gov.1 is not racing anything and cannot claim
scarcity as scoping discipline. Its last urgency argument has expired.

Worth naming: this ladder has now survived 6/6 adverse cross-model CEO consensus, a red team
producing 6 CRITICAL themes, a STRIKE on its direct premise ancestor (p7.7-ar-03), and this
6/6 — on three stacked operator overrides (D4, D5, D6). Not proof it is wrong; a reason to
require a *new* argument rather than a fourth override.

### F5 (CRITICAL) — contradicts the operator's own standing directive

CLAUDE.md records the operator verbatim: *"remove it from all consideration till **i have the
system functional for my need first**."* That is a priority ordering, not just a
deprioritization. gov.1 builds governance infrastructure for a hypothetical buyer and makes
the system functional for no one's need, least of all the operator's.

### F6 (HIGH) — `audit-S3` dominates gov.1 on gov.1's own terms

`SecretRewriter` is absent (THREAT_MODEL §585 documents its absence). Tool output reaches the
model unscrubbed, in a deployment holding live Gmail OAuth with a newly HTTP-reachable trigger.
gov.1 makes the *record* of a credential leak more verifiable; audit-S3 prevents the leak.
Commercially it is not close either: credential scrubbing is on every buyer checklist,
log chaining is on none — per gov.1's own landscape research.

### F7 (HIGH) — "M / ~2–3 CC days" is not credible

Eight pre-filed tensions, four CRITICAL, none mechanical; scope unresolved at estimation time
(Open Question 3 asks whether to build a deliverable D6 mandates). Blast radius is the worst in
the codebase: `evidence.rs` is read at boot on a **fail-closed** path in a `panic = "abort"`
PID-1 process, where ux.6a already found a boot panic and a rotation cascade. Repo base rate:
attn.4 was narrower and still produced a cross-restart double-fire bug at `/review` and a 32s
SIGTERM regression at `/qa`. Realistic 1.5–2×.

### F8 (HIGH) — the title is falsified by the plan's own scope

T2 sets the ceiling at *"rewind- and key-swap-DETECTION, not third-party evidence of conduct"*
and affirms §8.7.1. "Auditor-grade" and "not third-party evidence of conduct" cannot both be
true. Deeper: CLAUDE.md's invariant is *"logging is best-effort and must never crash an
agent"* — a signed projection of a best-effort stream is a **signed best-effort record**.
Signing does not upgrade the semantics of the source. If this ever ships it is
`gov.1-rewind-detection`, with the claim ceiling in the title.

### F9 (MEDIUM) — reopen D6

Commitment-only "verifies little" until gov.2. Building a format that verifies little so a
later rung can strengthen it was justified by PT-6's capacity framing, which F4 voids.

## The carve-out both voices converged on (~4 hours, not 2–3 days)

1. Correct PT-0's class table (done in this file) and **de-claim** in THREAT_MODEL + CLAUDE.md:
   the chain is 100% `allowed` by construction in the shipped config, and `capability_denied`
   — the one real denial class — is unattested.
2. **Bind-mount `evidence.jsonl` / `flight.jsonl` out of the deletable named volume** (F2).
   Minutes of work; fixes the only evidence failure that has actually occurred.
3. Then `docker compose up -d` and get the shadow-mode cycle.

## Acceptance criteria (draft — superseded by the review above)

1. A projection record exists for every covered class, produced from the in-memory stream,
   with a signed gap record proving where coverage stopped across a SIGTERM.
2. `agentctl audit-packet` emits both variants; the full variant verifies offline against a
   pinned verify key on a machine that never ran agentd.
3. A deliberately rewound/truncated segment is DETECTED; the test asserts recomputed values,
   not types (prior learning `assert-value-not-type-in-flight-log-guards`, 10/10).
4. THREAT_MODEL states the claim ceiling and, per PT-0, the true coverage of the denial class.
5. Binary size guard (6 MB) untouched; no new heavyweight dependency.

---

## GSTACK REVIEW REPORT

| Run | Status | Findings |
|---|---|---|
| Primary (plan authored w/ 8 pre-filed tensions from the ladder's red-team addendum) | complete | PT-0 authored, then corrected twice under adversarial pressure |
| Codex CEO voice | complete | No named buyer; "auditor-grade" overclaims; sequencing weak; both packet variants = roadmap theater. Caught PT-0 overstatement (`:2178` is ungated). Verdict DEFER |
| Claude CEO subagent (independent) | complete | 9 findings, 5 CRITICAL. F1 stack does not exist; F2 chain never durable; F3 `capability_denied` is the one real unattested class; F4 PT-6 void; F5 contradicts operator directive. Verdict DEFER |
| Consensus table | complete | **6/6 CONFIRMED adverse** |
| Phases 2–4 (Design / Eng / DX) | not run | Deferred at the Phase 1 premise gate — no scope survived to review |

**VERDICT: DEFERRED.** CROSS-MODEL absorbed — both voices independently DEFER, converging on
the same ~4h carve-out. Operator decision at the User Challenge gate: skip even the carve-out,
go straight to the shadow-mode confirmation and the 14-day product measure, then `audit-S3`.

**Self-correction on the record:** this review's own PT-0 was wrong twice. Draft 1 claimed
"4 of 5 `AgentAdmissionDenied` sites gated" (from a nearest-preceding-string heuristic, not
brace analysis) — caught by Codex. Draft 2 still missed `capability_denied` entirely — caught
by the Claude voice. The repo's standing lesson held again: trace the path to the artifact, and
do not trust a structural claim that was not read block by block.

NO UNRESOLVED DECISIONS
