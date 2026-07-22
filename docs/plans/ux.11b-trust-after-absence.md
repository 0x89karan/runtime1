<!-- /autoplan restore point: (fresh plan; ux.11a shipped v0.90.0) -->
# ux.11b — Trust after absence (run history + digest + morning brief)

**Branch:** ux.11b · **Base:** main @ v0.90.0 · **Status:** ✅ APPROVED (autoplan; SPLIT — build ux.11b-substrate first)

> **SCOPE DECISION (autoplan, final):** SPLIT (both CEO voices + Eng confirmed two
> distinct correctness domains). Build order:
> - **ux.11b-substrate (THIS PLAN, build first):** `RunTracker` → versioned `runs.redb`
>   (separate file, off-loop mpsc writer) authored from ALL agent-lifecycle sites (G1);
>   per-segment spend = Δ`context_tokens()` (E3); restore continues the open segment,
>   idempotent open (E4/G3); `runs_query` native tool gated by new `Capability::RunsRead`
>   (G5); read-only `GET /api/v1/runs` + `/agents/runs` via a `RunsAccess` trait (G7).
>   **NO flight tailer** (E2). Delivers felt value now: "CoS, what happened overnight?".
> - **ux.11c-UX (DEFERRED, own gate):** the CoS emits a `BriefWritten` rail event from
>   its existing cron brief turn (calling `runs_query`); NO scheduler trigger (G2 — the
>   scheduler can't Inject the never-waiting CoS; the cron loop is the cadence). Findings
>   E7/E8 (advance-on-write, bounded long-gap, terminal-bar honesty) ride with ux.11c.
> - Update design-doc P2 (E9) in the ux.11b-substrate PR.

## ux.11b-substrate scope (active)
- **RunTracker** (G1): `open_segment`/`close_segment`/`park_segment`/`resume_segment`, routed through every `state.agents`/`outcomes`/`waiting`/`pending_approvals`/`universal_agents`/shutdown site (concrete sites listed in Phase-3 revised mechanism below). Segments built per child run (spawn→terminal, park closes/reopens); the CoS's own segment is `config_seed`, open for its lifetime.
- **`runs.redb`** (G3/G4/E5): separate file (copy RedbStore open/quarantine/`META format_version=1`); provisional open record (`end=null`); off-loop `mpsc` writer task (single writer → seq ordering; best-effort, never stalls the loop); `RunsUnavailable` event on open failure.
- **`runs_query`** (G5): native tool (copy `KbSearch`), new `Capability::RunsRead`, `{from,to,agent_id,parent_id,status,limit<=100}` newest-first stable JSON.
- **`GET /api/v1/runs` + `/agents/runs`** (G7): `RunsAccess` trait in `surfaces`, same schema/pagination; FUSE runs top-level (NOT under live agent dirs — terminal agents are pruned).
- `RunRecorded` event; `approvals_count` via new per-segment bookkeeping (G6); universal-tier spend = `null`.
**Design doc (APPROVED):** `~/.gstack/projects/0x89karan-runtime1/0x89karan-ux-control-panel-design-20260718-204837.md`
**Predecessor:** ux.11a (v0.90.0, budget visibility) shipped the surfaces this builds on.
**Roadmap:** `docs/ROADMAP.md` ux.11 → ux.11b row.

## Problem

The operator can watch agents live and now see per-agent spend (ux.11a), but still cannot reconstruct **what happened while away**. Design-doc lived friction #2: "something failed overnight; per-agent cost and cause were unrecoverable from a live-only flight tail." The UX-track bar: **"Can the owner wake up, understand yesterday, and unblock today without opening a terminal?"** ux.11b delivers the *understand yesterday* half — durable run history + an agent-written morning brief.

## Scope (trust-after-absence — the design-doc ux.11 "A" half)

Findings **C1–C5** from the ux.11 autoplan CEO gate are baked in below as requirements, not open questions.

- **A1. Durable run records** in a new `runs.redb`, one per **run segment** (spawn→terminal, and a park boundary closes a segment — D3). Fields: `{agent_id, segment_seq, start, end, status, last_error, approvals_count, spend, stop_reason, parent_id}`.
  - **C1 (CRITICAL): records are written from the authoritative scheduler state-machine transitions** (spawn / terminal / park), NOT derived from the best-effort flight log. A dropped flight event must never drop a run from history. The scheduler already knows these transitions in-process.
  - **C4: per-segment spend = Δ(lifetime spend) across the segment** — captured in-process at open/close (`task.context_tokens()` at spawn vs at close), NOT `windowed_spent`.
- **A2. flight.jsonl tailer — DROPPED (E2, both models).** Every v1 schema field is available in-process at a transition (spend via C4; `approvals_count` — the scheduler owns the queue; `last_error`/`stop_reason` on the terminal effect). The tailer's only consumers (per-approval detail, event tail) belong to the deferred Runs view (C5) — zero consumer in v1. C2's "reuse `otel/src/tail.rs`" applies to that FUTURE increment, not this one.
- **A3. Morning brief via catch-up, not a live timer.** **C3: trigger the brief on a persisted `last_brief_at` catch-up check** (loop-top + idle-tick, division-based like ux.8′ window rebasing), NOT a live `tokio::time::interval` — the machine sleeps / the container restarts overnight, so a live timer fires nothing on the mornings that matter. On next boot / next tick past the due time, build the brief covering the gap.
- **A4. Native `runs_query` tool** for the CoS (copy `KbSearch`; opt-in registration in `register_native`; capability-gated) — lets the CoS answer "why did scout fail at 3am" conversationally over the run store.
- **A5. CoS-written morning brief into the chat rail.** **D4: deliver via a new flight event** the rail renders (rail-visible, cross-platform, no turn-injection side effects on the CoS conversation), layered on top of the existing `ops:briefs` KB write the cos-orchestrator already does.
- **A6. TUI "Runs" view — DEFERRED (C5).** The doc's headline is "the brief is written by an agent, not rendered by a UI." If `runs_query` + the brief answer the operator's questions conversationally, a rendered Runs view is replay-scrubber-lite. Ship without it; add back only if dogfood shows the brief leaves something wanting.
- **A7. Run records exposed via management API (`GET /api/v1/runs`) + FUSE** (`/agents/runs`, Linux-gated). Read-only.

## What already exists (leverage map — from the ux.11 touch-point survey)
- **redb store idiom:** `agentd/src/memory/store.rs` (RedbStore, TableDefinition, open-or-create, txn patterns, corruption quarantine) — the template for `RunsStore`.
- **`otel/src/tail.rs` `FileTailer`:** copy-truncate + fast-grow tested (C2) — extract to shared if a tailer is needed.
- **Scheduler transitions:** `handle_agent_terminal` (removes from state.agents), the spawn path (`dispatch_operator_spawn_inner`), and the park path (orchestrated `Waiting`) — the authoritative A1/C1 write points.
- **Native store-backed tool:** `KbSearch` in `tools/native.rs` — template for `runs_query`.
- **Catch-up precedent:** ux.8′ `maybe_rebase_windows_at` (loop-top + 60s idle tick, division-based catch-up, injectable clock) — the A3 model.
- **Brief content:** cos-orchestrator already writes `ops:briefs` KB + `./output/brief-*.md` (cos.agents.toml) — A5 adds only rail delivery.
- **EventKind:** `agentd/src/events.rs` — add `RunRecorded` / `BriefWritten` (snake_case).

## Open Decisions (for THIS autoplan)

- **OD1 — Split ux.11b further (11b substrate / 11c UX)?** Codex's ux.11 CEO voice recommended a 3-way split: substrate (runs.redb + runs_query + API/FUSE) proves storage before the digest/brief UX layers on top. Counter: with C1 (author from transitions) the substrate is smaller than feared, and the brief is the actual user value — shipping substrate alone delivers nothing felt. Recommend: **one increment** (substrate + brief), Runs view already deferred (C5). Surface at gate.
- **OD2 — Is a flight.jsonl tailer needed at all?** C1 makes records authoritative from in-process transitions; C4 makes spend in-process too. If approvals_count is also trackable in-process (the scheduler owns the approval queue), the tailer may be unnecessary for v1 — dropping A2 entirely. Decide in eng phase against the actual transition data available.
- **OD3 — Run-record granularity:** per-run segment (spawn→terminal, park closes a segment) — recommend as stated (D3). Confirm the park-closes-segment semantics against the orchestrated-agent lifecycle.
- **OD4 — Brief cadence + config:** default interval (24h to match CoS), config key, and what "the gap" means when multiple windows were missed (one catch-up brief covering all, vs one per missed window). Recommend one catch-up brief covering the gap.
- **OD5 — ux.2b fold-in (D5):** run records know "no events for N minutes" — fold the idle/error attention signals (ux.2b, closes cos-ux-01) in, or keep separate? Recommend **separate** (keep ux.11b bounded).

## NOT in scope
- TUI Runs view (A6 — deferred per C5; re-add only if dogfood wants it).
- Telegram / remote reach → ux.12. Cancel / SetCaps → ux.13. Full replay scrubber → ux.7.
- flight.jsonl rotation policy → run.1 (ux.11b tolerates truncation via the reused FileTailer if a tailer is used at all).

## Success criteria (design doc, ux.11b slice)
Morning: a brief in the chat rail naming **every** run (authoritative, not best-effort), its spend, its outcome, and anything blocked — zero flight.jsonl reading. The CoS can answer "why did X fail overnight" via `runs_query`. The brief appears even after an overnight restart (catch-up).

---

## Phase 1 — CEO Review (autoplan)

### CEO dual voices — consensus table
```
  Dimension                            Claude   Codex   Consensus
  ──────────────────────────────────── ──────── ─────── ─────────
  1. Premises valid?                    caveats  caveats CONFIRMED (design-doc P2 stale: says "derived from flight.jsonl"; C1 flips it)
  2. Right problem (history) now?       yes      yes*    CONFIRMED (*Codex: reconsider Telegram sequencing; Claude: 11a's defer-not-brick already capped the cancel-first motivator → history-first survives)
  3. Scope calibration?                 SPLIT    SPLIT   CONFIRMED → SPLIT 11b-substrate / 11c-UX
  4. Alternatives explored?             no       no      CONFIRMED (file-vs-table not considered; no-store correctly rejected but table-in-store not)
  5. Competitive/market risk?           n/a      n/a     N/A (single-tenant personal)
  6. 6-month trajectory sound?          NO       NO      CONFIRMED (brief-before-record-model-proven; live migration on unrebuildable history)
```
Both voices: **SPLIT** + **no flight tailer in v1**. The plan's sole argument against splitting ("substrate alone delivers nothing felt") is **false** — `runs_query` makes the substrate a felt capability today ("CoS, what happened overnight?"), which is exactly what makes the split cheap.

### Findings (both models, carried into whichever increments proceed)
- **E1 (HIGH, both) — SPLIT into ux.11b-substrate + ux.11c-UX.** 11b = versioned `runs.redb` authored from scheduler transitions + per-segment spend delta + `runs_query` (+ read-only API/FUSE), **no tailer**. 11c = catch-up trigger + CoS brief + `BriefWritten` event + rail render. Two distinct correctness domains (authoritative history vs restart-safe digest timing) that shouldn't share one review; ~9–11 surfaces bundled otherwise. Do NOT split 11c further.
- **E2 (MED-HIGH, both) — OD2 = NO flight tailer in v1.** Every schema field is available in-process at a transition (spend via C4; approvals_count — the scheduler owns the queue; last_error/stop_reason on the terminal effect). The tailer's only consumers (per-approval detail, event tail) belong to the deferred Runs view (C5) — zero consumer in v1. C2's "reuse otel FileTailer" applies to that FUTURE increment, not this one. Drop A2.
- **E3 (LOW, Claude) — C4 spend must pin `context_tokens()` (monotonic lifetime), NOT `estimate_context_tokens()`** (which shrinks on paging/compaction → negative segment spend). Naming trap; spend correctness is the whole "what it cost" criterion. → 11b.
- **E4 (MED, Claude) — define checkpoint-restore segment semantics.** The brief's headline case is the overnight restart, which restores from checkpoint. Is a restored agent a new or continued segment? Recommend **continued** (context_tokens restored monotonically). Undefined = misattributed overnight spend in the exact scenario the increment serves. → 11b.
- **E5 (MED, Claude) — version `runs.redb` from v1** (additive serde-default, like checkpoint v4). The store is authored from ephemeral transitions → **unrebuildable**, so a wrong segment model becomes a live migration on the operator's accreted history. → 11b.
- **E6 (MED, Claude) — decide new-FILE vs new-TABLE in the existing MemoryStore explicitly.** Table = fewer surfaces (no second open-or-create/quarantine); separate file = blast-radius isolation + no scheduler-hot-path vs KB-read contention. "Light runtime justifies every new file" → owe the rationale. → 11b.
- **E7 (MED, Claude) — 11c: advance `last_brief_at` only on a successful `BriefWritten`**, not when the due-check fires — else a budget-deferred/busy CoS silently skips the brief on the busy morning. Long-gap briefs (away 7 days) need a size bound. → 11c.
- **E8 (MED, Claude) — honest bar:** the brief goes to the chat rail (inside the TUI), so ux.11b/c delivers "understand yesterday **at the terminal**"; the terminal-free half is ux.12 (Telegram). State it; don't let "zero flight.jsonl reading" paper over "without a terminal."
- **E9 (LOW, process, Claude) — update design-doc P2** ("derived from flight.jsonl" → authored from transitions) in the ux.11b PR (docs-in-same-PR rule).

### Decision Audit Trail
| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 1 | CEO | Split ux.11b → 11b-substrate + 11c-UX | **USER CHALLENGE** | n/a (never auto-decided) | Both models; **RESOLVED: user KEPT one increment** |
| 2 | CEO | E2 no flight tailer in v1 | Mechanical (feasibility) | P5 explicit / P3 pragmatic | No schema field needs the log; tailer's only consumer is the deferred Runs view |
| 3 | CEO | E3 spend = context_tokens() not estimate_context_tokens() | Mechanical (correctness) | P5 explicit | estimate_ shrinks on paging → negative spend |
| 4 | CEO | E4 restore continues the open segment | Mechanical (correctness) | P1 completeness | monotonic context_tokens across restore; **idempotent open** required (G3) |
| 5 | CEO | E5 version runs.redb from v1 | Mechanical | P1 completeness | unrebuildable store → schema mistakes are live migrations |
| 6 | CEO | E6 separate runs.redb file | Taste→settled | P4 DRY / P5 explicit | hot-path write-lock isolation from the KB store |
| 7 | Eng | G1 RunTracker over ALL insert/remove/park/outcome sites | Mechanical (correctness) | P1 completeness | spawn/terminal/park is incomplete; misses config-seed CoS, dispatch_spawn children, universal exits, approval-park |
| 8 | Eng | G2 brief driven by the CoS cron loop, NOT a scheduler catch-up trigger | Mechanical (feasibility) | P3 pragmatic | scheduler can't Inject a non-waiting agent; CoS already writes the brief — just add a BriefWritten event |
| 9 | Eng | G4 off-loop runs writer (mpsc, best-effort) | Mechanical (invariant) | P1 completeness | inline redb fsync stalls the async scheduler; "logging never stalls an agent" |
| 10 | Eng | G5 new `Capability::RunsRead` for runs_query | Mechanical (security) | P5 explicit | run history (errors/spend/parent) is not KB data; KbRead is too loose |

## Phase 3 — Eng Review (autoplan)

### Eng dual voices — consensus table
```
  Dimension                            Claude   Codex   Consensus
  ──────────────────────────────────── ──────── ─────── ─────────
  1. Segment write-points complete?     NO(crit) NO(crit) CONFIRMED — spawn/terminal/park misses the real topology (G1)
  2. Brief trigger feasible as designed? NO(crit) NO(high) CONFIRMED — scheduler can't Inject the non-waiting CoS (G2)
  3. Spend counter correct?             YES      YES     CONFIRMED — context_tokens() (E3) ✓
  4. Store choice/versioning sound?      YES      YES     CONFIRMED — separate file + META format_version
  5. Hot-path write safe?               NO       NO      CONFIRMED — must go off-loop (G4)
  6. Capability model correct?          NO(gap)  NO(gap) CONFIRMED — needs Capability::RunsRead (G5)
```
Both voices: **NOT-READY as originally written**; two CRITICAL topology mismatches (G1, G2). Both fixes are mechanical and make the increment *simpler*. Revised mechanism below.

### Revised mechanism (all Eng findings folded in)
- **G1 — `RunTracker`, not three named hooks.** A single scheduler-owned tracker with `open_segment(agent_id, parent_id, start_reason, spend_at_open)` / `close_segment(agent_id, status, stop_reason, last_error, spend_at_close)` / `park_segment` / `resume_segment`, called at EVERY site that enters/leaves `state.agents`, `state.outcomes`, `state.waiting`, `pending_approvals`, `state.universal_agents`, and shutdown-kill. Concretely: config-seed loop (`Scheduler::new` ~265-278), `dispatch_spawn` child insert (~1918), operator spawn (~2323), `handle_agent_terminal` (~990, funnels child+root+admission-denial), universal open (~509) + universal outcome sites (~2764/2788/2805/979), approval park (`pending_approvals` insert ~1663) + resume (`provide_tool_results` ~1976/2027).
- **The brief is built from CHILD run segments (inbox/curator daily), not the orchestrator's own.** The CoS is a self-driving cron-loop agent (`max_turns=200k`, not orchestrated) → it holds ONE perpetual open segment for its whole life. That's fine: its daily *children* are the runs the brief names. State this; the CoS's own segment is `start_reason=config_seed`, open until shutdown.
- **G2 — the CoS drives the brief; agentd only records runs.** Drop the scheduler catch-up trigger (A3). The scheduler cannot `Inject` the CoS (Inject requires `state.waiting`; the CoS never is). The CoS already writes `ops:briefs` in its cron loop — v1 adds only: (a) the CoS calls `runs_query` for the window, (b) emits a new `BriefWritten` flight event the rail renders. The cron loop IS the cadence; an overnight restart is handled by the cron loop resuming (catch-up-on-demand), not a scheduler timer. This is *more* faithful to the design doc's "the brief is written by an agent, not rendered by a UI."
- **G3 — idempotent open + crash reconciliation.** On open, check `runs.redb` for an existing open segment (`latest_open:{agent_id}` index) and no-op if present, so restart doesn't mint a second open segment. On open, persist a provisional record (`end=null`, `start_context_tokens`, `segment_seq`); on restart, continue open records by `agent_id`.
- **G4 — off-loop writer.** Transition handlers `send()` a `RunEvent` on an `mpsc::UnboundedSender` (non-blocking, best-effort — matches "logging never stalls/crashes an agent"); a dedicated writer task owns the `RunsStore` write side (single writer → segment_seq ordering). Never `begin_write().commit()` inline on the scheduler loop.
- **G5 — `Capability::RunsRead`** (new variant) gates `runs_query`; explicit opt-in in `register_native`; contract `{from,to,agent_id,parent_id,status,limit<=100}`, newest-first, stable JSON. CoS config grants it.
- **G6 — `approvals_count` needs new per-segment bookkeeping** (no cumulative counter exists today); increment on `RequestApproval`. Universal-tier spend = `null` (no `context_tokens()`; proxy-metered).
- **G7 — API/FUSE parity via a `RunsAccess` trait** in `surfaces`: `GET /api/v1/runs` + `/agents/runs` (JSONL or `/agents/runs/<seg>.json`) return the same schema/pagination. FUSE runs are top-level, NOT under live agent dirs (terminal agents are pruned from snapshots).
- **DX:** runs_query error paths mirror the memory tool; API errors mirror the memory arm (management.rs ~243); `RunsUnavailable` event on store-open failure.
