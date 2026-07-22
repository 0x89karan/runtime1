<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/ux.11c-autoplan-restore-20260722-135136.md -->
# ux.11c — Trust after absence (the morning brief, UX half)

**Predecessor:** ux.11b-substrate (v0.91.0, PR #131) shipped the durable substrate —
`runs.redb` authored from authoritative scheduler transitions, `runs_query` native tool
(new `Capability::RunsRead`, granted to the CoS), and `GET /api/v1/runs`.
**Design doc (APPROVED):** `~/.gstack/projects/0x89karan-runtime1/0x89karan-ux-control-panel-design-20260718-204837.md`
**Split origin:** ux.11b autoplan gate (2026-07-21) split ux.11b into substrate + this UX half,
after both CEO voices + both Eng voices CONFIRMED the mechanism (G2, E7, E8). The CEO/Eng
findings for THIS increment are already locked in `docs/plans/ux.11b-trust-after-absence.md`;
they are carried forward here as requirements, not re-opened.
**Roadmap:** `docs/ROADMAP.md` ux.11 → ux.11c row.

## Problem

ux.11b made history *durable and queryable* — the CoS can answer "why did scout fail at 3am"
via `runs_query`, and `GET /api/v1/runs` exposes it. But nothing yet **pushes** yesterday to
the operator. The design-doc bar for this half: **"Can the owner wake up, understand yesterday,
and unblock today?"** ux.11c delivers the *understand yesterday* delivery: the CoS composes a
morning brief over `runs_query` and surfaces it in the chat rail, so waking up and reading the
brief needs no `runs_query` typing and no flight.jsonl reading.

Honest bar (E8): the brief lands in the **chat rail inside the TUI** — this is "understand
yesterday **at the terminal**". The terminal-free half (phone) is ux.12 (Telegram). ux.11c
does not claim otherwise.

## What already exists (leverage map)

- **`runs_query` native tool + `Capability::RunsRead`** (ux.11b) — the CoS already holds both
  (granted in `agentd/cos.agents.toml` + `distro/overlay/etc/agentd/cos.agents.toml`). The
  brief is composed by *calling this tool*, no new query path.
- **CoS cron loop** (`agentd/cos.agents.toml`): `cron_trigger` MCP server (`docker/cron_mcp.py`)
  wakes the orchestrator on schedule; the orchestrator already writes `ops:briefs` KB +
  `./output/brief-YYYY-MM-DD.md` every cycle (`max_turns=200_000`). This IS the cadence (G2).
- **Rail render dispatch:** `agentctl/src/watch/converse.rs::on_flight_event()` matches on
  event `kind` and flushes a turn into the chat rail (`orchestrator_turn_complete`,
  `agent_failed`, etc.). A `brief_written` arm slots straight in. Shared by the TUI Dashboard
  rail and `agentctl orchestrate`'s CLI REPL.
- **EventKind pattern:** `agentd/src/events.rs` (snake_case `kind_str`), with the
  `otel/tests/event_kind_coverage.rs` exhaustiveness guard + `otel/src/span_builder.rs` mapping.
- **Flight recorder from a tool:** native tools receive the recorder handle; `MemoryWrite`,
  `KbSearch` etc. already emit events from inside `tools/native.rs`.

## Settled mechanism (locked in the ux.11b autoplan — carried forward, not re-opened)

- **G2 — the CoS drives the brief; agentd only records + renders.** No scheduler catch-up
  trigger. The scheduler cannot `Inject` the CoS (Inject requires `state.waiting`; the CoS,
  a self-driving cron-loop agent, is never waiting). The cron loop is the cadence; an overnight
  restart is handled by the cron server re-firing on its next schedule (catch-up-on-demand),
  not a scheduler timer.
- **E7 — advance the brief window only on a *successful* brief write**, and bound the long-gap
  brief (away 7 days must not dump 700 runs). Re-interpreted for the G2 world below (D2).
- **E8 — honest bar** ("at the terminal"): stated in Problem; reflected in ROADMAP + design-doc.
- **E9 (done in ux.11b PR)** — design-doc P2 already flipped to "authored from transitions."

## Scope (ux.11c — REFRAMED to the deterministic pull core, CEO gate 2026-07-22)

The acceptance bar is a **pull** surface, not a live-rail push (F1). agentd is the **authority**
for the factual spine (F2): it composes counts/window/outcomes/IDs from `runs.redb`; the model
contributes only narrative color.

- **A1. Persisted, structured brief in `runs.redb`.** A new `BRIEFS` table in the existing
  `RunsStore` (reuses ux.11b open/quarantine/`META` infra — no new file). One row per brief:
  `{ brief_id, created_at, window_from, window_to, run_count, failed_count, approvals_pending,
  spend_total, items: [{run_id, agent_id, status, spend, stop_reason}], narrative }`. Newest-first
  read; this is the durable delivery state (resolves D2 — see below).
- **A2. Deterministic composer + `publish_brief` native tool.** The CoS calls
  `publish_brief(narrative?)` at the end of its cron turn. **agentd**, not the model, then:
  (a) derives the window as `[last persisted brief's window_to .. now]` (D2 — explicit delivery
  state in `runs.redb`, not KB content); (b) reads `runs.redb` for that window and computes the
  factual spine; (c) merges the model's optional narrative; (d) persists the `BRIEFS` row;
  (e) emits `BriefWritten`. The model cannot fake the facts (F2/F8). Opt-in registration in
  `register_native` like `runs_query`; **capability-gated** — new `Capability::BriefPublish`
  (F8/D4), granted to the CoS in both `cos.agents.toml`.
- **A3. `BriefWritten` flight event** (`kind_str` = `"brief_written"`), payload = the structured
  spine (attention-first fields). Added to `events.rs`, the otel coverage guard (`=> false`,
  informational), and `span_builder.rs` (no span).
- **A4. `GET /api/v1/brief`** (+ `RunsAccess`-style read on the store) — returns the latest
  persisted brief as structured JSON + a rendered text block. `?n=` optional history. Works
  regardless of attach time (F1). Linux FUSE `/agents/brief` is **out** (defer with ux.11b-ar-02).
- **A5. `agentctl brief` pull command** — hits `GET /api/v1/brief` (honors `--url`/`AGENTCTL_URL`),
  renders **attention-first** (F4): `📋 1 failed · 2 need approval · 12 runs · <window>`, then the
  failed/blocked lines **with run + agent IDs** (F6), then an `✓ N others ok` roll-up; quiet night
  prints `📋 Quiet night — 0 runs since <window>` (F5). Always prints *something*.
- **A6. CoS prompt update** (`agentd/cos.agents.toml` + `distro/overlay/etc/agentd/cos.agents.toml`):
  the cron brief turn calls `publish_brief` (optional narrative) after its existing `ops:briefs`
  write. Additive; the facts no longer depend on the model passing them.
- **A7. Docs:** ROADMAP ux.11c → shipped; DEPLOYMENT `agentctl brief` + `GET /api/v1/brief`;
  design-doc "at the terminal (pull)" honesty note; CHANGELOG.
- **A8. (BONUS, NOT acceptance bar) live-rail render.** A `"brief_written"` arm in
  `converse.rs::on_flight_event()` that renders the brief line when a brief fires *while watching*,
  and/or seek-last-brief-on-attach. Ship only if cheap and after A1–A7 are green; F1 means this can
  never be the sole surface.

## NOT in scope
- TUI **Runs view** (deferred at ux.11b C5).
- Telegram / any off-terminal reach → **ux.12** (now a pure consumer of `GET /api/v1/brief`).
  Cancel / SetCaps → ux.13.
- ux.2b idle/error attention fold-in (kept separate, per ux.11b OD5).
- `runs.redb` retention/prune + O(n) `list()` → ux.11b-ar-03. FUSE `/agents/brief` → ux.11b-ar-02.

## Open Decisions — RESOLVED at the CEO gate
- **D1 — emission mechanism:** native `publish_brief` tool, but **agentd owns the facts** (F2), not
  a `kb_put` side-effect and not model-passed fields. RESOLVED.
- **D2 — brief window / "advance only on success" (E7):** window = `[last persisted BRIEFS row's
  window_to .. now]`. Delivery state is now **explicit in `runs.redb`** (not the fragile
  ops:briefs-timestamp of the original plan — Codex #7). A failed/skipped morning is naturally
  covered by the next brief's window. RESOLVED.
- **D3 — long-gap bound (E7 size):** `runs_query`/window read clamped (≤100 items, ux.11b clamp);
  the persisted brief keeps all counts but the `items` list is capped with a `+N more` overflow
  count; `GET /api/v1/brief` can page. RESOLVED.
- **D4 — capability:** **yes**, new `Capability::BriefPublish` (F8) — the brief is an operator
  trust surface. RESOLVED.

## Success criteria (REFRAMED — pull)
Morning after absence: `agentctl brief` returns the latest persisted brief, attention-first
(failed/blocked with run+agent IDs, then `✓ N others ok`, or `Quiet night — 0 runs`) — no
`runs_query` typing, no flight.jsonl reading — **regardless of when the operator attaches**. It
survives an overnight restart (window derived from the last persisted `BRIEFS` row). A brief
whose `publish_brief` write failed does NOT advance the window (next cron re-covers the gap).
`GET /api/v1/brief` returns the same, structured, with the **live** "N need approval" overlay.
Live-rail render (A8) is a bonus for while-attached only, never the sole surface.

---

## Phase 3 — Eng Review (autoplan)

### Eng dual voices — consensus table
```
  Dimension                              Claude   Codex   Consensus
  ────────────────────────────────────── ──────── ─────── ─────────
  1. Concurrency safe (no deadlock)?      YES      YES     CONFIRMED — redb serializes; no cycle
  2. Write-routing mechanism              own-txn  mpsc    DISAGREE → taste (both agree the INVARIANT: 1 atomic txn, advance-on-commit)
  3. "N need approval" computable in tool? NO(HIGH) NO(HIGH) CONFIRMED — live scheduler state; compute at endpoint from snapshot
  4. Window semantics correct?            NO(HIGH) partial CONFIRMED — window by end_ts/still-open, NOT start_ts; first-ever = now−24h
  5. BriefWritten emission path correct?  NO(HIGH) n/a     CONFIRMED — no recorder in tool; emit via central invoke hook
  6. Advance-only-on-success atomic?      YES(cond) YES(cond) CONFIRMED — iff window_to on row + single txn
  7. Edge cases covered?                  gaps     gaps    CONFIRMED — quiet-night row, count-over-full-window, retain failures on clamp
```

### Findings (both models, all folded into the revised scope above/below)
- **G1 (HIGH, both) — "N need approval" is LIVE state, not `runs.redb`.** `pending_approvals`
  lives in `SchedulerState` (scheduler.rs:120); `RunRecord.approvals_count` is a cumulative
  request counter, not "currently pending"; a run parked on approval stays `status="running"`.
  **Fix:** compute the headline count at `GET /api/v1/brief` from `state.snapshot.read()
  .pending_actions.len()` (ApiState already carries `SharedSnapshot`); DROP/rename the persisted
  `approvals_pending` row field. Caveat: `pending_actions` is ≤100-capped (note it). "Need
  approval" is correctly a *now* fact, not frozen-at-brief-time.
- **G2 (HIGH, Claude, code-verified) — window by completion, not `start_ts`.** `RunFilter` filters
  `rec.start_ts` (store.rs:332). A run that starts 11pm (in window), is `running` at the 6am brief,
  then fails 7am has `start_ts < next window_from` → **never reported failed in any brief.** Fix:
  a dedicated composer query = `(end_ts ∈ [from,to)) OR status==running`, not `list()`'s start_ts
  semantics.
- **G3 (HIGH, Claude, code-verified) — `BriefWritten` cannot be emitted "from the tool."**
  `ToolContext` has no recorder (tools/mod.rs:17); tool events fire centrally in
  `ToolRegistry::invoke`'s post-call `match name` hook (mod.rs:213+, cf. the `kb_search` arm).
  **Fix:** `publish_brief` returns the structured spine JSON as its result; add a `"publish_brief"`
  arm to the central match that parses it into the `BriefWritten` payload. The hook fires only on
  `Ok`, so a failed persist → the tool returns `Err` → no event (advance-on-success holds).
- **G4 (taste — write-routing DISAGREE) — own write txn on disjoint tables (recommended) vs
  route through run_writer mpsc.** Claude: give `RunsStore::publish_brief(narrative) ->
  Result<BriefRecord>` its own single `begin_write` (read RUNS + insert BRIEFS in ONE txn),
  invoked via `spawn_blocking` on the shared `Arc<RunsStore>` exactly like `runs_query`'s read;
  `run_writer` owns RUNS/OPEN_BY_AGENT/META, `publish_brief` owns BRIEFS only; `window_to` lives
  on the row (never META, to avoid seq-counter contention). Codex: extend the mpsc with a
  `PublishBrief{reply}` oneshot command so one lane owns all writes. **Recommend own-txn (P5/P3):**
  the mpsc is fire-and-forget with no reply path — routing through it means inventing a oneshot
  protocol AND moving compose into the writer, just to return the spine the tool already needs to
  return. Own-txn is simpler and the disjoint-table split makes contention a single fsync wait,
  once per cron. Both satisfy advance-on-commit.
- **G5 (HIGH/MED, both) — window first-ever bound + advance-on-success.** First brief has no prior
  row → `window_from = now − 24h` (documented; pre-first-brief history stays in `runs_query`).
  Advance-on-success holds iff `window_to` is on the row and the insert is the advancing commit
  (G4). RESOLVED.
- **G6 (MED, both) — quiet night MUST persist a row** (`run_count=0`, non-empty rendered text,
  window advanced) — else "no brief" == "broken pipeline" (contradicts F5). Test it.
- **G7 (MED, both) — counts over the FULL window, then clamp items.** `list(limit=100)` cannot give
  correct `run_count`/`failed_count`/`spend_total` over a 700-run window. Dedicated composer:
  scan the full window for aggregates, **retain all failed/blocked/running items**, clamp only the
  `✓ N others ok` roll-up, record `overflow_count`.
- **G8 (MED, Codex) — success criteria still said "watch"** → fixed above to `agentctl brief`.
- **G9 (LOW, both) — wiring is mostly present.** `register_native` already takes
  `Option<Arc<RunsStore>>`; `main.rs:396` already passes it. New compiler-forced touch points:
  `PublishBrief` tool + `register_native` branch (`publish_brief` && `runs.is_some()`);
  `Capability::BriefPublish` (forces `satisfies()` arm capability.rs:139 + `caps_to_rules` no-op
  arm main.rs:1460); `GET /api/v1/brief` + `RunsStore::latest_brief()/list_briefs(n)`; grants in
  **BOTH** `agentd/cos.agents.toml` and `distro/overlay/etc/agentd/cos.agents.toml` (BriefPublish
  cap + `"publish_brief"` tool — easy to update one, forget the other).
- **G10 (LOW, Claude) — double-fire idempotency:** if cron double-fires or the model calls twice,
  the 2nd call writes a spurious empty row. Guard: no-op if the newest brief is younger than a
  threshold and its window would be empty.

### Decision Audit Trail (Eng)
| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 7 | Eng | G1 "N need approval" computed live at endpoint | Mechanical (correctness) | P1 completeness | pending state isn't in runs.redb; must be a now-fact |
| 8 | Eng | G2 window by end_ts/still-open, not start_ts | Mechanical (correctness) | P1 completeness | else overnight-completed failures silently vanish |
| 9 | Eng | G3 emit BriefWritten via central invoke hook | Mechanical (correctness) | P5 explicit | tools have no recorder; matches kb_search pattern |
| 10 | Eng | G4 own write txn on disjoint tables | **TASTE** (Codex: mpsc) | P5 explicit / P3 pragmatic | mpsc is fire-and-forget; own-txn returns the spine + surfaces failure simply |
| 11 | Eng | G5 first-ever window = now−24h | Mechanical | P3 pragmatic | morning-brief semantics; bounds the first scan's reported counts |
| 12 | Eng | G6 quiet-night persists a row | Mechanical (invariant) | P1 completeness | brief presence = liveness signal |
| 13 | Eng | G7 counts over full window, retain failures on clamp | Mechanical (correctness) | P1 completeness | truncated list can't name the failing run (defeats F6) |

## Phase 2 (Design) + Phase 3.5 (DX) — folded note
This is a backend/no-UI increment: the only "UI" is terminal text (`agentctl brief`) and a JSON
endpoint. **Design** substance = the brief's information hierarchy, already decided by F4
(attention-first: failed/approvals before counts), F5 (explicit quiet-night + all-clear states),
F6 (name run/agent IDs so it's actionable, not a dead end), F7 (structured storage, per-surface
render). No component/screen/state-machine surface beyond that. **DX** substance = the agent- and
operator-facing seams: `publish_brief` tool naming (verb, consistent with `runs_query`),
`agentctl brief` subcommand (guessable, honors `--url`/`AGENTCTL_URL` like `agentctl watch`),
`GET /api/v1/brief` (mirrors `/api/v1/runs` incl. 503-when-unconfigured), `Capability::BriefPublish`
(explicit grant, error → `CapabilityDenied`). Examined; no additional findings beyond the CEO/Eng
set above.

---

## Phase 1 — CEO Review (autoplan)

### CEO dual voices — consensus table
```
  Dimension                              Claude    Codex    Consensus
  ────────────────────────────────────── ───────── ──────── ─────────
  1. Right problem now?                   yes*      caveats  CONFIRMED (*core is right; delivery surface is wrong)
  2. Live-rail push works for absence?    NO(CRIT)  NO       CONFIRMED — rail is lossy live SSE, no replay-on-attach; 6am brief gone by 9am
  3. Emission deterministic?              NO(HIGH)  NO       CONFIRMED — LLM-compliance ("model calls publish_brief") violates record-everything
  4. `BriefWritten` abstraction right?    caveats   NO       CONFIRMED — hardcodes a throwaway text blob; want structured + per-surface render
  5. Sequencing vs ux.12?                 don't-delay don't-delay CONFIRMED — do NOT delay ux.12 for a terminal-only render
  6. Scope correctly bounded?            reframe   too-small CONFIRMED — as written it's plumbing; the pull core is the real, smaller increment
```
Both voices independently reach the **same reframe**: the durable, **deterministic pull core**
(composer + `BriefWritten` authored in agentd from `runs.redb` + a `GET /api/v1/brief` endpoint +
`agentctl brief` pull command) is the right, robust increment — and it is **less code** than the
cross-platform live-rail arm the plan centers on.

### Findings (both models)
- **F1 (CRITICAL, Claude, code-verified) — the live rail cannot show a brief that fired during
  absence.** `agentctl/src/watch/pump.rs` documents the `/api/v1/events` SSE as *"lossy and
  reconciled by the snapshot poll"*; the snapshot reconciles agent **state**, not the rail
  transcript. `converse.rs::on_flight_event()` only mutates the rail on a *live* event; there is
  no replay-from-history on attach (`read_flight_tail` serves the inspector view only). A
  `brief_written` at 6am is gone when the operator attaches at 9am. **A3's ephemeral rail push
  is the wrong acceptance bar.** Fix: a **pull** surface — `GET /api/v1/brief` + `agentctl brief`
  (reads the latest persisted brief), robust regardless of attach time; optionally seek-and-render
  the last brief on attach as a *bonus*.
- **F2 (HIGH, both) — emission must be deterministic, not LLM-compliance.** A2/A4 fire the brief
  only if the CoS prompt reliably calls `publish_brief` every cron turn — a prompt edit / context
  truncation / model swap silently stops it, invisibly, violating the *record-everything*
  invariant. Fix: agentd composes the **factual spine** (count/window/outcomes) deterministically
  from `runs.redb`; the model contributes only narrative color. This also lets agentd stop
  trusting the model to pass facts, collapsing A2+A4.
- **F3 (HIGH, both) — the reusable core is the composer + event + endpoint; the TUI live-rail
  render (A3) is the throwaway part.** Reframe ux.11c to ship the substrate so ux.12 (Telegram)
  and any web cockpit are pure consumers of one endpoint. Do NOT delay ux.12 for a terminal-only
  render.
- **F4 (HIGH, Claude) — lead with the trust signal, not a vanity count.** `"📋 12 runs …"` buries
  the answer to "did anything break / need me?" Lead with attention:
  `"📋 1 failed, 2 need approval · 12 runs · <window>"`; state all-clear explicitly.
- **F5 (MED, Claude) — always emit, including quiet nights** ("Quiet night — 0 runs"). Otherwise a
  missing brief is indistinguishable from a broken pipeline; presence of the brief is the liveness
  signal.
- **F6 (MED, Claude) — name run/agent IDs** so the operator can act today (manual command) and
  ux.13 can later attach one-tap verbs to the same lines. Don't ship a dead-end newspaper.
- **F7 (MED, both) — store a structured brief** (run summary as data + short narrative), render
  per-surface; don't bake a 4 KiB rail-tuned text blob as the interchange format (Telegram re-cuts
  it otherwise).
- **F8 (LOW, Claude) — content-injection axis:** the CoS summarizes untrusted email; a deterministic
  factual spine (F2) means an injected model can't fake the header. Demarcate narrative as
  model-authored. Gate `publish_brief` (Codex #6) — the rail/brief is an operator trust surface.

### Decision Audit Trail
| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 1 | CEO | Reframe live-rail-push → deterministic **pull** core (F1/F2/F3) | **USER CHALLENGE + feasibility** | n/a (never auto-decided) | Both models; F1 code-verified that the live rail cannot serve the absence case |
| 2 | CEO | F2 deterministic factual spine from runs.redb | Mechanical (invariant) | P1 completeness | record-everything can't depend on LLM compliance |
| 3 | CEO | F4 lead with attention, not run count | Mechanical (product) | P1 completeness | the trust question is "did anything need me?" |
| 4 | CEO | F5 always emit incl. quiet nights | Mechanical | P1 completeness | brief presence = liveness signal |
| 5 | CEO | F6 name run/agent IDs | Mechanical | P1 completeness | not a dead-end; ux.13 verb anchor |
| 6 | CEO | F8 gate publish_brief | Mechanical (security) | P5 explicit | brief is an operator trust surface (spoof/injection) |


---

**STATUS: APPROVED** (autoplan, 2026-07-22, HEAD 6ebe6c49) — reframed to pull core at the CEO gate; G4=own-txn at the final gate. Ready to build.
