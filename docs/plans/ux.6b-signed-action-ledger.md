# ux.6b — Signed action ledger (DEFERRED)

**Status:** DEFERRED at the /autoplan premise gate, 2026-07-29. Not scheduled. Split out of
`ux.6-evidence-view.md`; the honesty + durability half shipped as `ux.6a-declaim-and-detrap.md`.

**Why deferred, not struck:** the idea is sound and the gap is real — it is just not next, and one
version of it was already rejected at a higher gate. Keeping it named and specified so a future gate
can pick it up with the analysis intact.

---

## The gap this would close

The receipt chain covers **egress (inference) calls only**. Sole producer: `agentd/src/egress.rs`.
Everything else an agent does that carries governance weight — tool invocations and their results,
capability verdicts, approval grant/deny, cancels, budget mutations, egress denials — is recorded in
`flight.jsonl`, which is **unsigned and unchained**.

So today the chain proves *what the agent asked the model*, not *what the agent did*. Any
"provable accountability" or EU AI Act Art. 12 traceability claim needs the second thing.

## Two candidate substrates

**B1 — Widen the egress chain.** Emit `ActionReceipt`s for the additional action classes.
- Cost is understated by the original plan: `write_receipt` does `flush()` + `sync_data()` **per
  receipt on the caller's thread** (`evidence.rs:170-175`). Today that is once per inference call,
  amortized against a network round-trip, so it is invisible. Widening puts a **synchronous fsync on
  every tool call and capability check**, inside a fail-closed writer.
- Also 10x's the size of a file that `agentd` reads at boot (see ux.6a) — so ux.6a's de-trap work is
  a hard prerequisite.
- **Already rejected once:** the approved mv design doc
  (`~/.gstack/projects/0x89karan-runtime1/0x89karan-mv-governed-agent-runtime-design-20260720-114634.md`)
  considered "Approach C: Ledger-to-Act-grade-first — harden receipts (retention, chain continuity
  across restarts, verifier UX) before hypervisor work" and rejected it: *"dogfooding requires none
  of it yet; becomes relevant right before the external gate."* Re-proposing B1 without engaging that
  decision is relitigation.

**B2 — Sign a projection of `flight.jsonl` (preferred starting point).** Rather than build a second,
parallel, half-empty record, treat the flight log as the canonical one and add attestation over it.
- `flight.jsonl` **already covers every action class.**
- `View::Inspector` (`agentctl/src/watch/inspector.rs`) already reads it, bounded (512 KB / 500-line
  tail) and searchable, with `All / Errors / Sandbox / CapDenied / Egress` filters.
- run.1 already rotates it (100 MB cap); obs.1–obs.3 already export it via the OTLP sidecar.
- Signing becomes an *optional attestation layer over the real record* instead of a competing one,
  and it retires the coverage problem rather than funding it.

## The constraint that limits the value of either

**Key custody.** `EvidenceWriter::open` generates the signing key locally and rewrites the `.pub`
from the private key on every open; `verify_chain` reads that same `.pub`. The chain therefore proves
integrity **relative to a key the signer holds** — self-attestation, when the signer is precisely the
party whose repudiation would be at issue. Third-party-grade evidence requires the key to leave the
boundary: customer-held key, external timestamping, or control-plane countersigning. **That is the
increment that would actually change what can be claimed**, and it is a different piece of work from
either B1 or B2.

## What a governance buyer actually asks for (do not skip this before building)

Roughly in priority order, from the CEO phase: SOC 2 Type II / ISO 27001, increasingly ISO 42001 —
an *auditor's report*, not a hash chain; log export into the customer's own SIEM/OTel/S3 with a
documented schema plus retention and deletion guarantees; RBAC/SSO/SCIM over who can read or delete
the record; immutability by **storage** (WORM / S3 Object Lock, in the customer's account) which is
what auditors accept; incident-reconstruction narrative, DPA, subprocessor list, pen-test.

Cryptographic chaining appears on none of those lists. Art. 12 requires automatic lifetime logging
and traceability; it does not require Ed25519. The differentiator versus Portkey/LiteLLM/Langfuse is
**enforcement** — the budget fuse actually stopping the runaway loop, capabilities actually denying,
the kill actually landing — plus a record complete enough to prove it happened. The signature is the
least load-bearing part.

## Gate conditions — build this when at least one is true

- A design partner (or the mv external gate, now dated in the roadmap) asks for the artifact.
- The operator hits a real question that `Inspector` cannot answer.
- The key can be moved outside the boundary, making verification mean something to a third party.

Until then `ux.6a`'s de-claim keeps the docs honest about what exists, which is the cheap half of the
value.

## If it is ever picked up

Start from **B2**, not B1. Prerequisite: ux.6a's boot-read/rotation de-trap must have landed.
Re-read the mv doc's rejected-approaches list first and engage it explicitly rather than around it.
