# budget.1-ar-02 — universal-tier budget is a soft cap under concurrency

Branch: `budget.1-ar-02-reservation-metering` · Base: `main` (v0.107.0)
Kind: metering-correctness. **Design-bearing** — and the premise ("is a reservation system worth
building?") is itself the first question, given this session's pattern of reshaping over-scoped work.

## The gap (filed budget.1-ar-02 at the AUDIT-v0.97 holistic review; both models called it bounded)
Both the per-workload budget and the global window are **post-hoc**: the egress proxy checks the
counter BEFORE forwarding (`egress.rs:463` per-workload `== 0`; `:480` global `windowed() >= ceiling`)
but accounts the spend AFTER the response (`:585` per-workload `fetch_update` subtract; `:591`
`add_universal`). So N concurrent universal requests all pass the pre-forward gate when the budget is
near the limit, then all forward and all spend → the window overshoots by up to
`(N−1) × per_request_tokens` before the next request 429s.

## PREMISE CHECK (resolve FIRST — do not auto-decide)
Is this worth a reservation system? The overshoot is **bounded and small for the real config**:
- Concurrency is capped: `[scheduler] max_concurrent_inferences = 3` (prod cos config). So N ≤ 3.
- Per-request tokens ≤ `max_tokens` (output) + input; prod `max_tokens = 8192`.
- Ceiling = `global_token_budget = 50_000_000` (prod). Worst-case overshoot ≈ `2 × (8192 + input)` ≈
  ~20–50k tokens over 50M = **~0.1%**. And "cognition is bounded" holds asymptotically (the very next
  request 429s). Both the budget.1-ar-01 review and the holistic review noted this is the SAME post-hoc
  shape the NATIVE tier already has — not a regression, an inherent property.
So the honest options are a spectrum:
  - **(P-fix) Full reservation** — reserve before forward, reconcile after. Hard cap. Real complexity
    (atomic reserve/release, reconciliation on success AND failure, the reservation-leak hazard).
  - **(P-mid) Lightweight reserve** — reserve `max_tokens` only (a known, bounded upper bound; skip
    input estimation) with an RAII release guard, reconcile to actual on response. Closes the overshoot
    to sub-request granularity with far less surface.
  - **(P-doc) Document the bound + tighten nothing** — the overshoot is provably ≤ `N×max_tokens`,
    negligible vs the ceiling; document it as an accepted bound (like the native tier), add a test
    asserting the bound, and DON'T build reservation machinery. Cheapest; "cognition is bounded" already
    holds asymptotically.
The gauntlet must decide whether the complexity of P-fix/P-mid is justified, or whether P-doc is the
right call for a single-tenant system where the overshoot is ~0.1%.

## Design (IF a reservation is built — P-fix/P-mid)
- **D1 — what to reserve.** `max_tokens` (the hard output cap, in the request body) alone (P-mid), or
  `max_tokens + input_estimate` (P-fix; input estimated from body length since the proxy doesn't
  tokenize). Reserving max_tokens-only under-reserves by the input size (a smaller residual overshoot);
  reserving an input estimate risks over-reserving (false 429s) if the estimate is high. Lean: reserve
  `max_tokens` (known, exact upper bound on the dominant term) — the input under-reservation is a
  second-order residual, far smaller than today's full-request overshoot.
- **D2 — atomic reserve mechanism.** A `fetch_update` that atomically checks `current + reservation <=
  ceiling` → add reservation, else fail (429). Same for the per-workload counter. Needs a "reserved"
  accounting distinct from "spent" OR a single counter that's reserved-up-front then reconciled.
- **D3 — reconciliation (THE critical correctness point).** On response: release the reservation and
  add the ACTUAL spend (net = actual − reservation applied to the counter). On FAILURE (upstream error,
  connection drop, timeout, panic): the full reservation MUST be released — a leaked reservation
  permanently strands budget and eventually deadlocks the tier at a false-exhausted state. Use an RAII
  drop-guard so EVERY exit path (incl. `?`/early-return/await-cancel) releases. This is where a naive
  implementation breaks.
- **D4 — scope.** Egress proxy (universal tier) only, or also the NATIVE tier (scheduler pre-inference
  gate, same post-hoc shape)? The universal path is self-contained (this file); native reservation is a
  scheduler change (bigger, different code). Lean: universal-only here; file native as a follow-up if
  P-fix is chosen (the native overshoot is equally bounded/small).

## Acceptance criteria (draft — depends on premise outcome)
- If P-doc: a test asserts the worst-case overshoot is ≤ `N × max_tokens` and the tier 429s at the next
  request after the ceiling; the bound is documented where the pre-forward check lives; NO reservation
  machinery. `cargo build/clippy/test` green.
- If P-mid/P-fix: N concurrent universal requests at the ceiling do NOT overshoot beyond one in-flight
  request's reservation; a failed/dropped request releases its reservation (no leak — a test drives an
  upstream error and asserts the counter returns to pre-request); reconciliation to actual on success;
  no false-429 regression on the happy path. Green.

## NOT in scope
- Native-tier reservation (unless P-fix explicitly folds it in) — follow-up.
- Changing `max_concurrent_inferences` or the ceiling.

## Risk
LOW-MEDIUM. The metering path is contained (egress.rs). The real hazard is D3 (reservation leak on a
failure path deadlocking the tier) — only relevant if a reservation is built; P-doc has ~zero risk.

---

## /autoplan RESOLVED (2026-07-26) — P-doc, reshaped down. Both voices unanimous.
Both Eng voices corrected the plan's premise and said DON'T build reservation machinery:
- **N≤3 was FALSE** — `max_concurrent_inferences` gates only native in-process inference (`state.in_flight`);
  the egress proxy accept loop is an unbounded `tokio::spawn` per connection, so proxy concurrency is
  unbounded. Ceiling is 10M (not the stale 50M). The proposed "overshoot ≤ N×max_tokens" test would encode
  a bound that doesn't exist.
- **DORMANT in prod** — cos ships no `tier="universal"` agents and no `[egress] proxy_addr`, so `main.rs` never
  starts the proxy. Building async reservation (with a real leak hazard) for a path prod never runs is unjustified.
- **A universal-only reservation can't make the combined ceiling hard** — the ceiling is native+universal; the
  native term stays post-hoc regardless. P-fix universal-only doesn't achieve its own goal.
- Single-tenant spend guardrail (not security); native has the identical shape unfixed; bounded asymptotically.

**Shipped (P-doc):** the `egress.rs` gate comment now documents the accepted post-hoc soft-cap semantics honestly
(unbounded proxy concurrency, dormant in prod, hard cap needs both-tier reservation); a new
`global_budget_meter_is_a_posthoc_soft_cap` test pins the REAL bound (429 on the next request once
`windowed >= ceiling`; overshoot accepted), NOT the false N×max_tokens bound. Reservation + a proxy concurrency
cap filed as a follow-up gated on universal fan-out being enabled in prod. No reservation machinery built.
