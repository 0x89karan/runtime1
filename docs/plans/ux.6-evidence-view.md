<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/ux.6-evidence-autoplan-restore-20260729-001554.md -->
# ux.6 — Evidence view (SUPERSEDED — split at the /autoplan premise gate)

> **SUPERSEDED 2026-07-29.** Both CEO voices returned RESHAPE; the increment was split at the gate:
> - **`ux.6a-declaim-and-detrap.md`** — de-claim the overclaiming docs + close the `evidence.rs`
>   fail-closed boot trap + wire-or-delete the dead deny path. **This is the one being built.**
> - **`ux.6b-signed-action-ledger.md`** — the signed action ledger. **DEFERRED**, named and specified.
>
> Kept for the record. Two of its own premises turned out to be wrong: **P2 is false** (the mv design
> doc exists, outside the repo, and already rejected chain-widening), and its two "non-negotiables"
> (bounded reads + inline verify) are **mutually exclusive**, because `verify_chain` must start at
> seq 0 from genesis, so a tail cannot be verified.

**Status:** draft, pre-review. Written 2026-07-29 after v0.115.0 (ux.13-TUI) landed.
**Roadmap line** (`docs/ROADMAP.md:1260`): *"Evidence view: surface the signed Ed25519 receipt chain
(`evidence.jsonl`) + inline `agentctl verify` + per-agent 'chain verified' badge. Provable
accountability. **Cross-track:** this is also the governance artifact the mv product thesis rests on
(EU AI Act Art.12) — build it once for both."*

---

## What exists today (code-verified, not assumed)

| Piece | Where | State |
|---|---|---|
| `ActionReceipt` | `agentd/src/evidence.rs:43` | `{seq, action, target, principal, verdict, ts, chain_prev_hash, signature}` — generic in shape |
| `EvidenceWriter` | `agentd/src/evidence.rs:77` | one per process, thread-safe, hash-chained + Ed25519-signed |
| `verify_chain()` | `agentd/src/evidence.rs:245` | walks the file, returns receipts verified |
| `agentctl verify` | `agentctl/src/verify.rs` | CLI: takes `<evidence.jsonl> <key.pub>`, prints `chain ok: N receipts verified` |
| **operator surface** | — | **NONE.** No snapshot field, no FUSE file, no HTTP route, no TUI view. |

Note a name collision to avoid in review: `AttentionSignal.evidence` (ux.2, `surfaces/src/snapshot.rs:243`)
is an unrelated free-text string. Different concept, same word.

## Measured facts (from the ux.13-TUI /qa rig, a real agentd run)

- **Volume:** 2 022 receipts / **754 KB** from ~10 minutes of looping agents. ~75 KB/min under load,
  and the file is append-only with **no rotation** (unlike `flight.jsonl`, which run.1 capped at 100 MB).
- **Verification cost:** `agentctl verify` on those 2 022 receipts = **1.8 s wall / 0.75 s user** (debug
  build). O(n) Ed25519. Release builds are faster; the shape is unchanged and n is unbounded.
- **Coverage — the finding that should drive this plan:** every one of the 2 022 receipts was
  `action: "inference"`, `verdict: "allowed"`. The **only producer is the egress mediator**
  (`agentd/src/egress.rs`); nothing else in the tree writes a receipt.

## The premise problem, stated before the scope

"Provable accountability" and the Art. 12 record-keeping argument are about **what the agent did**.
The chain currently proves **what the agent asked the model**. It contains no tool calls, no approvals
granted or denied, no cancels, no capability denials, no budget decisions — all of which ARE recorded,
but in `flight.jsonl`, which is unsigned and unchained.

So a view built directly on today's chain would honestly read: *"2 022 model calls, all allowed, chain
verified."* Shipping that under the label "provable accountability" would be the same class of
overclaim that v0.115.0 spent its whole review budget removing (Park's "reversible", FUSE's
"confirmed", the cancel marker's silence).

**This is the question for the CEO phase, not a detail for the Eng phase.**

## Candidate shapes (for review to choose between, not a decision)

**A — Surface what exists, labelled honestly.** A `[e]` view listing receipts with filters, a verified
badge, inline verify. Cheap. Names itself "egress receipts", not "accountability". Risk: builds a view
whose value is capped by its data, and the label invites the overclaim anyway.

**B — Widen the chain first, then surface it.** Emit receipts for the action classes that carry
governance weight (tool calls with their capability verdict, approval grant/deny, cancel, budget
changes) and then build the view over a chain that means what the label says. Bigger, touches the
scheduler's hot path, and needs a decision on what a "receipt-worthy action" is. This is the version
the mv thesis actually needs.

**C — Defer.** Nobody has asked for it. The roadmap itself calls ux.6 an *evidence-gated expansion*,
and the gate (a design partner needing the artifact) has not fired. Do the cheap queue instead.

## Premises to challenge (I am not confident in these)

- **P1 — an operator wants this.** No dogfood signal. The verbs (ux.13) closed a friction someone
  actually hit at 2 am; this closes none that has been reported.
- **P2 — the cross-track claim.** CLAUDE.md cites `docs/plans/0x89karan-mv-governed-agent-runtime-design-*.md`
  as the mv design record. **That file is not in `docs/plans/`** (verified by listing). Either the
  path is stale or the doc lives elsewhere; the "build it once for both" argument rests on a document I
  could not open, so the second consumer's requirements are unverified.
- **P3 — the chain is the right substrate.** If the governance record needs tool-level actions, the
  honest substrate might be signing a projection of `flight.jsonl` rather than widening the egress chain.
- **P4 — verify belongs in the TUI at all.** It is a seconds-scale O(n) blocking call. ux.13-TUI built
  `App.pending_verb` + `drain_pending_verb` for exactly this shape, so it is reusable — but "reusable
  machinery exists" is not a reason to build a feature.

## Non-negotiables if this ships in any form

- **No verification on the render thread.** Reuse the `pending_verb` two-phase slot. A 1.8 s freeze
  is the H1 defect ux.13-TUI just fixed.
- **Bounded reads.** The file has no rotation; a view that loads it whole will eventually load
  hundreds of MB. Tail-and-cap, like the Logs view's 2 000-line ring.
- **Say what the chain covers.** Any badge or count must name the action classes it is derived from,
  so "chain verified" cannot be misread as "everything this agent did is accounted for".
- **`evidence.jsonl` rotation is a real gap** either way — append-only and unbounded, unlike
  `flight.jsonl`. Rotating a *hash-chained* file is not the same problem as rotating a log; it needs
  its own answer (chain continuation across segments).

## Open question folded in from the queue

**`audit86-P1-9`** (the last live P1): wrong-tier capability grants are inert — 6 of 9 combos. cap.1
made them non-silent. Is that the intended declare-then-lint design, or a gap? It is a 20-minute scope
decision, and it belongs here because a capability verdict is exactly the kind of thing shape B would
put in the chain.
