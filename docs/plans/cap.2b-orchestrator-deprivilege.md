# cap.2b — orchestrator de-privilege (real P1-10 closure)

**Source:** `docs/AUDIT-v0.86.md` P1-10 (= cos-dev-02) + cap.2 CEO gate (2026-07-23). **Depends:** cap.2 (v0.94.0 ✅ — the attenuation FLOOR + `capability_covered_by` this builds on). **Predecessor:** `main` at v0.94.0.

## Problem (the threat cap.2 explicitly left OPEN)

cap.2 shipped the attenuation *floor*: a child's requested caps must be ⊆ the parent's. The CEO gate (both models + user) established this closes **accidental over-grant** but NOT the injected-orchestrator prompt-injection threat P1-10 actually names:

- `cos-orchestrator` (`cos.agents.toml:239-265`) holds **`{Spawn, Mcp{google_oauth}, Credential{Google}}`** AND reads the last 24h of Gmail directly (prompt STEP 3) + ingests the inbox agent's summary of untrusted email (STEP 3b).
- It is simultaneously: (a) the node that reads attacker-influenceable data, (b) the holder of live Gmail, and (c) the spawn authority that picks child caps + child task text.
- So an injected orchestrator spawns the curator (or any child) **with `Mcp{google_oauth}` from its own set** — the cap.2 subset check passes — and hands it a malicious task. The bypass is real and is encoded as the passing test `spawn_attenuation_documents_injection_bypass` (`scheduler.rs`).

**The insight (cap.2 CEO Finding F2):** the subset check only becomes load-bearing when the GRANTING node is itself attenuated. cap.2 built the primitive and applied it one level too low. cap.2b applies it where it matters — the orchestrator.

## Why tool-input attenuation can't fix this (settled at cap.2 gate)

The cap-chooser (orchestrator LLM) is downstream of untrusted data. Any mechanism where the injectable node picks child caps is defeated by injection. cap.2b must move cap authority OFF the injectable node.

## Candidate approaches (for the pipeline to pressure-test)

- **A1 — De-privilege the orchestrator + config-declared spawn profiles.** The orchestrator holds NO `Mcp{google_oauth}`/`Credential` and does not free-choose child caps. Instead the scheduler owns **config-declared spawn profiles** (`[[spawn_profiles]]` or similar): the orchestrator calls `spawn_agent(profile="cos-inbox")` and the SCHEDULER attaches that profile's fixed caps (Gmail for inbox, KB-only for curator). Injection can name a profile but cannot mint new caps. **Open:** can the injected orchestrator still hand the inbox profile a malicious *task*? → may require the profile to also fix the task template (orchestrator supplies only data-slots, e.g. TODAY), not free-form task text.
- **A2 — Static inbox→curator pipeline.** Declare inbox + curator as first-class config (not dynamic spawns) with fixed caps + fixed tasks wired at config time; the orchestrator becomes a thin cron trigger that does NOT read email or summaries. **Open:** the CoS re-spawns date-stamped children daily on cron; a static pipeline needs a re-run-on-trigger mechanism (scheduler support). Bigger scheduler change.
- **A3 — Keep the orchestrator out of the untrusted-data path only.** Minimal: orchestrator drops Gmail/Credential and never reads raw email/the inbox summary back into its context (it already routes the summary through KB via `ops:entities`). But it still needs to spawn a Gmail-capable inbox → still needs to hold or delegate Gmail somehow → collapses into A1 (profiles) unless spawn caps come from config.

## Rough scope (pre-review)
- Some form of **scheduler-owned child caps** (profiles or static pipeline) so the orchestrator need not hold Gmail to spawn a Gmail-capable child.
- Orchestrator cap set loses `Mcp{google_oauth}` + `Credential{Google}` (and possibly free-form child-task authority).
- CoS config (dev + distro) restructured accordingly; the `cos_spawn_caps_subset.rs` guard updated; the bypass test flips from "documents the gap" to "the gap is closed" (green becomes: injected orchestrator CANNOT get Gmail to the curator).
- `agentd check` awareness of the new profile/pipeline declaration (cap.1 tie-in).

## Acceptance (draft)
- An injected orchestrator (one that names profiles + supplies task data) **cannot** cause any child to receive `Mcp{google_oauth}` unless that child's config-declared profile grants it AND its task is config-fixed.
- The daily brief still works end-to-end (inbox fetches Gmail, curator writes KB-only brief).
- The former `spawn_attenuation_documents_injection_bypass` test is replaced by a test proving the bypass is closed.
- Audit P1-10 marked CLOSED.

## PREMISE GATE DECISION (user, 2026-07-23): **Reshape to real closure (A2)**

Locked shape (both CEO voices' recommendation):
- Orchestrator → **summary-free cron trigger**: reads NOTHING its children produce; loses `Mcp{google_oauth}`, `Credential{Google}`, `FsWrite{./output}`, `BriefPublish`, and the KB writes it no longer needs.
- **Inbox + curator = static-tasked children of the ONE CoS pipeline.** The scheduler owns their caps AND task templates; the orchestrator supplies only a validated data-slot (date). Codex's sealed-job framing (`run_job(profile, params)`, fixed caps + fixed task) is the mechanism to pressure-test in Eng vs a static two-stage pipeline declaration.
- **Brief assembly + `FsWrite{./output}` + `BriefPublish` move INTO the curator** (curator = KB-read + FsWrite + BriefPublish; NO Gmail, NO Spawn). Injected node becomes the curator → worst case "bad brief / poisoned KB", no live-credential path.
- Scoped to the one pipeline — **NO general `[[spawn_profiles]]` registry**. The genuinely-new scheduler bit is **re-run-a-fixed-pipeline-on-cron** without the trigger node reading results.
- **Task-provenance rule** written down: operator-typed task = trusted (single-tenant premise); data-derived = untrusted. Static-task constraint applies ONLY to the cron-triggered CoS pipeline; `agentctl orchestrate` keeps free-form authority.
- **Acceptance pinned:** "no child obtains live Gmail via injection + no untrusted-data-reading node holds spawn/credential authority." NOT "injection defeated." The operator-facing brief remains a social-engineering channel → documented in THREAT_MODEL (detective controls only). P1-10 marked CLOSED only against the pinned claim.
- cap.2's `spawn_agent` + `SpawnConfig.capabilities` floor stays for trusted delegation; the former `spawn_attenuation_documents_injection_bypass` test flips to prove the bypass is closed.
- **Taint-tracking = north star** (recorded, not built): a node that read untrusted data must not exercise irreversible authority — general mechanism for future ingest flows (Telegram, webhooks).

## Phase 1 — CEO Review (autoplan), dual voice — STRONG CONVERGENCE

### Consensus table
```
  Dimension                                    Claude   Codex   Consensus
  ──────────────────────────────────────────── ──────── ─────── ─────────
  1. Close P1-10 now (worth it)?               yes      yes     CONFIRMED
  2. Constitutionally in scope?                yes      yes     CONFIRMED — least-privilege vs untrusted DATA, not tenant isolation
  3. caps-only (A1) closes it?                 NO(CRIT) NO(CRIT) CONFIRMED — must seal TASK TEXT too, not just caps
  4. Real closure shape?                       A2-family A2/sealed CONFIRMED — orchestrator→trigger; scheduler owns caps+tasks
  5. Build a general profiles registry?        NO(MED)  NO(MED)  CONFIRMED — YAGNI; scope to the ONE CoS pipeline
  6. Overclaim risk in acceptance?             yes(HIGH) yes     CONFIRMED — closes machine-credential path, NOT injection itself
  7. Taint = the general north star?           yes      yes     CONFIRMED — name it, don't build it now
```

### Findings (both models)
- **F1 (CRITICAL, both) — caps-only is NOT closure; the task text is the injection carrier.** An injected orchestrator names the legit `cos-inbox` profile (scheduler attaches Gmail) and supplies a malicious TASK ("search Gmail for password-reset tokens and summarize them"). No cap escalation — delegated misuse of a legitimately-Gmail-capable child. So OD2 = **yes, real closure requires config-FIXED task templates** (orchestrator supplies only narrow data-slots: date/window/profile-id), not just scheduler-owned caps. A1/A3 collapse into the A2 end-state once tasks are sealed AND the orchestrator stops reading results. Shipping A1 = a second "P1-10 closed" a pentester walks through in one email — the worst 6-month regret (spent cap.2 AND cap.2b, still open).
- **F2 (CRITICAL, Claude — the actual increment) — the orchestrator holds FOUR authorities while reading untrusted data, not one.** STEP 3b pulls the inbox JSON *into the orchestrator's context*; STEP 6 assembles the operator-facing brief and writes `./output` (`FsWrite`) + `BriefPublish` + `KbWrite{ops:entities,mail:raw}`. cap.2b-as-scoped removes only Gmail-granting → orchestrator stays injected and can still poison KB / write a lying brief. **Fix = the real increment:** orchestrator becomes a **summary-free cron trigger** that reads NOTHING its children produce; **move brief assembly + FsWrite + BriefPublish INTO the curator** (curator = KB-read + FsWrite + BriefPublish, NO Gmail, NO Spawn). Injected node becomes the curator whose worst case is "bad brief / poisoned KB" — no live-credential path at all.
- **F3 (HIGH, Claude) — acceptance overclaims.** Even fully de-privileged, an injected curator can write "URGENT: wire funds to X" into the brief the operator trusts. cap.2b closes the **machine-credential** path, NOT injection ("capability envelopes bound actions, not intent" — audit §5). Pin acceptance to "no child obtains live Gmail via injection + no untrusted-data-reading node holds spawn/credential authority." Document the brief as a residual social-engineering channel in THREAT_MODEL (detective controls only: flight recorder, receipts, operator judgment). Do NOT mark P1-10 CLOSED against a broader claim than code delivers.
- **F4 (MED, both) — no general `[[spawn_profiles]]` registry for one consumer.** Declare inbox + curator as static-tasked children of the one CoS pipeline + a **re-run-on-cron-trigger** scheduler primitive (the one genuinely new bit). Codex's cousin framing: a **sealed job** — `run_job(profile, params)` (fixed caps + fixed task template + allowed slots) as the hardened primitive for untrusted-data paths; `spawn_agent` (cap.2 floor) stays for trusted delegation. Scope the increment around the re-run primitive, not an abstraction.
- **F5 (MED, Claude) — resolve on TASK PROVENANCE, don't cripple orchestration globally.** `agentctl orchestrate` (orch.1/orch.2) is a deliberate bet on dynamic operator-driven spawning with free-form tasks. The distinguishing variable: **operator-typed task = trusted** (the single-tenant premise) vs **data-derived task = untrusted**. Scope the static-task constraint to the **cron-triggered CoS data pipeline ONLY**; operator-triggered orchestration keeps free-form authority. Write this rule down so a future increment doesn't "harden" dynamic orchestration into uselessness.
- **F6 (MED, Claude — severity honesty) — "accept + document" deserved a paragraph.** Realistic injected-CoS outcome TODAY is **integrity/manipulation** (poisoned KB, misleading brief), NOT credential breach/RCE: Gmail is read-only (no send scope), `oauth_call_api` is host-pinned + SSRF/IP-hardened (cred.3.2), single-tenant = no lateral token value. Worth closing; don't sell as plugging RCE. Claude's read: don't accept — do the CHEAP close (the static daily-brief pipeline, F1+F2), which beats both "accept" and "build a profiles subsystem." Honest question the user owns: does this outrank **run.1** (flagship self-bricks on log/state growth) for the single real user?
- **F7 (framing, both) — constitution guard line.** In scope as least-privilege (mutual trust is a property of *agents*, not of the *data* they ingest; reducing authority co-located with untrusted-data ingestion is the carve-in). DRIFT line: the moment cap.2b reaches for per-agent auth tokens / agent-vs-agent isolation / distrust-premised sandboxing, it's in the forbidden zone. Correct primitive = **data-taint, not agent-distrust**.
- **F8 (note, both) — taint is the general north star.** "A node that ingested untrusted data must not exercise irreversible authority" generalizes via a taint bit (mark agent/KB-segment tainted on untrusted read → deny credential/spawn/egress while tainted). Too much machinery for this increment; do the static pipeline as an explicit **down-payment toward taint**, recorded so the next data-ingest flow (Telegram, webhooks) doesn't re-litigate by hand.

### Decision Audit Trail (CEO)
| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 1 | CEO | Shape: A2 end-state vs A1 floor vs accept+document | **PREMISE (gate)** | n/a | both models; caps-only ships a bypassable closure |
| 2 | CEO | Seal child TASK templates (not just caps) | folds into premise | P1 completeness | task text is the injection carrier (F1) |
| 3 | CEO | Move brief assembly + FsWrite + BriefPublish into curator | folds into premise | P1 completeness | orchestrator must read nothing untrusted (F2) |
| 4 | CEO | No general profiles registry — scope to one pipeline | Mechanical | P3/P4 | YAGNI; one consumer (F4) |
| 5 | CEO | Task-provenance rule (operator=trusted, data=untrusted) | Mechanical | P5 explicit | don't cripple orch track (F5) |
| 6 | CEO | Acceptance = "no live Gmail via injection", not "injection defeated" | Mechanical | P5 explicit | no overclaim (F3) |
| 7 | CEO | Taint = north star, not built now | Mechanical | P3 pragmatic | down-payment (F8) |

## Phase 3 — Eng Review (autoplan), dual voice — TIGHT CONVERGENCE

### Consensus table
```
  Dimension                                   Claude   Codex   Consensus
  ─────────────────────────────────────────── ──────── ─────── ─────────
  1. Mechanism: sealed run_job + [[jobs]]     yes      yes     CONFIRMED (NOT static [[agents]] re-run, NOT spawn_agent+profile)
  2. deliver_content=false is the crux fix    yes      yes     CONFIRMED
  3. Job caps from config bypass subset check  yes      yes     CONFIRMED — sound (trust root = config, not parent)
  4. cap.2 spawn_agent untouched               yes      yes     CONFIRMED
  5. Zero-param server-stamped date            yes      yes     CONFIRMED — drop the slot; simpler AND safer
  6. Liveness (pending-future) is load-bearing yes(CRIT) yes    CONFIRMED — keep orchestrator a live cron-polling agent
```

### Locked implementation blueprint (both voices)
**A1 — sealed job primitive.**
- `AgentEffect::RunJob { call_id, job_id }` — a **sole-only, scheduler-handled** effect (sibling of `SpawnAgent` in `agent/mod.rs:750-782`); a plain tool can't touch scheduler state.
- `Capability::RunJob` (unit cap) gates a new `run_job` native tool (`required_capability_for → RunJob`).
- `config.rs`: `struct Job { id, token_budget, capabilities: Vec<Capability>, task: String }` + `#[serde(default)] pub jobs: Vec<Job>` on `Config`.
- `dispatch_run_job` (sibling of `dispatch_spawn`) — extract shared `materialize_child(id, task, caps, budget, deliver_content)`. Child caps = `job.capabilities.clone()` from config — **the `capability_covered_by` subset check (`scheduler.rs:1911`) is NOT run on this path** (sound: job caps are operator-authored config, not orchestrator-chosen; the trust root moves from parent-caps to config). Enforces: caller holds `RunJob`, `job_id` exists, depth limit.

**A2 — sequencing without reading (the crux, F2).** The leak is `scheduler.rs:1066-1095` in `handle_agent_terminal`: a child's full `answer` is injected into the parent unconditionally.
- Add `deliver_content: bool` to `AwaitingParent` (`scheduler.rs:73`) + `AwaitingEntry` (`checkpoint.rs:112`, `#[serde(default = <true>)]` so old checkpoints restore as content-delivering).
- `spawn_agent` → `true` (back-compat: trusted delegation still gets the answer). `run_job` → `false`.
- When `false`: replace the delivered content with an **agentd-authored signal** — `"job '<child_id>' completed"` on success; a bounded **error-CLASS token** on failure (NOT `e.to_string()` — F7(b): a raw error could echo an email subject back into the trigger).
- Pipeline: cron fires → `run_job("cos-inbox")`; inbox fetches Gmail + **writes its OperatingBrief JSON to `ops:entities:inbox-{date}` itself** (STEP 3b moves INTO the inbox job); terminates → trigger gets `"completed"` (no content) → `run_job("cos-curator")`; curator reads inbox JSON **from KB**, assembles + writes + publishes the brief; terminates. Email-derived content travels inbox→KB→curator, never through the trigger.

**A3 — no injectable task carrier (zero-param, both voices' refinement).** The de-privileged orchestrator reads nothing untrusted, so the only date it can pass is cron's `fired_utc`. **The scheduler stamps `{date}` server-side from wall-clock UTC; `run_job` takes only `job_id`, zero params** — no slot, no regex, no injection surface. (`Job.task` is a config-fixed template with `{date}` substituted by the scheduler.) A param-slot with strict validation is the fallback only if operator date-backfill ever becomes a real need (not stated).

**A4 — de-privilege + jobs config (dev + distro `cos.agents.toml`).**
- Orchestrator caps → `[ Mcp{cron_trigger}, RunJob ]`. **Drops:** Spawn, Mcp{google_oauth}, Mcp{semantic-kb}, Credential{Google}, all KbRead/KbWrite, FsWrite, RunsRead, BriefPublish. Task shrinks to: loop wait_for_trigger → run_job(cos-inbox) → run_job(cos-curator) → loop. Add `run_job` to root `[tools] native` (global registry; agents narrowed by caps, not the native list).
- `[[jobs]] cos-inbox` (budget 1_500_000): Mcp{google_oauth} + Mcp{semantic-kb} + KbRead/KbWrite{mail:raw} + KbWrite{ops:entities} + Credential{Google}; task = inbox prompt + write OperatingBrief JSON to ops:entities. (`request_approval` is cap-free → first-run OAuth still works.)
- `[[jobs]] cos-curator` (**budget 200k→~500k**, F7(d) — brief assembly + kb_search + RunsRead or it bricks mid-brief): Mcp{semantic-kb} + KbRead{ops:entities,ops:briefs} + KbWrite{ops:briefs,ops:entities} + RunsRead + FsWrite{./output} + BriefPublish; task = read inbox JSON from KB, assemble markdown, write_file, publish_brief. **Owns the brief** (F2).
- Distro: same shape, narrower (no semantic-kb sidecar; FsWrite `/run/output`).
- `agentd check` (`check.rs`): new loop over `cfg.jobs` mirroring the per-agent loop (`check.rs:124-158`) — MCP-server existence, KB-segment existence, Credential wiring cross-check. Both configs pass `--strict`.

**A5 — tests.** (1) bypass-closed headline: de-privileged trigger, injected `run_job("cos-curator")` + Gmail-smuggle attempt → curator materializes with exactly the config caps (no google_oauth); unknown job_id → error; trigger's `filtered_specs` has no `spawn_agent` (no Spawn); `RunJob` effect has no caps field (compile-time). (2) daily-brief E2E (cron→inbox→curator→loop; brief written+published). (3) sequencing/no-read: curator's KbRead sees inbox's KB write AND the trigger's post-inbox ToolResult is the fixed signal, NOT the JSON (`deliver_content=false` proof). (4) slot moot under zero-param (or `Job::render` unit tests if slot kept). (5) back-compat: cap.2 spawn_attenuation tests green untouched; rename `spawn_attenuation_documents_injection_bypass` → `spawn_agent_floor_is_not_injection_defense` (still green — spawn_agent floor unchanged). (6) config guard: extend `cos_spawn_caps_subset.rs` with a `cos_jobs` guard (orchestrator lacks Gmail/Credential/FsWrite/BriefPublish/Spawn; inbox job has Gmail; curator has BriefPublish+FsWrite, not Gmail) + `AwaitingEntry.deliver_content` checkpoint round-trip.

### Eng findings not folded above
- **F7(a) LIVENESS (CRITICAL, decisive):** agentd exits when `state.pending` empty (`scheduler.rs:620`); the orchestrator's perpetual `wait_for_trigger` future is the anchor. Sealed-job keeps the live cron-polling agent → safe. Literal static-`[[agents]]`-re-run would delete that future → idle-but-not-firing (the [[feedback_control_rx_hang]] class). This is THE decisive reason for sealed-job over static-pipeline.
- **F7(e):** `ops:briefs` log-class restart-overwrite note (cos.agents.toml:164) now owned by curator — carry the comment.

### Decision Audit Trail (Eng)
| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 8 | Eng | Sealed run_job + [[jobs]] (not static agents / not spawn_agent+profile) | Mechanical | P5/P3 | smallest correct change; preserves liveness (F7a) |
| 9 | Eng | deliver_content=false + agentd-authored signal | Mechanical | P1 completeness | the crux — closes the content leak (F2) |
| 10 | Eng | Zero-param server-stamped date | Mechanical | P5 explicit | both voices; removes the slot injection surface |
| 11 | Eng | Error-CLASS token on failure, not e.to_string() | Mechanical | P1 completeness | closes the error-path echo hole (F7b) |
| 12 | Eng | Curator budget 200k→~500k | Mechanical | P1 completeness | assembly+kb_search+RunsRead or it bricks (F7d) |
| 13 | Eng | checkpoint deliver_content serde default true | Mechanical | P5 explicit | old checkpoints restore correctly (F7c) |

## FINAL GATE — APPROVED (2026-07-23)
Premise = reshape to real closure (user CEO-gate decision). Both CEO voices + both Eng voices
converged; **0 taste decisions, 0 user challenges** at the Eng phase (the zero-param refinement
both models raised independently → mechanical). Mechanism: sealed `run_job` + `[[jobs]]` +
`deliver_content=false`. Acceptance pinned to "no live Gmail via injection" (not "injection
defeated"); brief social-engineering residual → THREAT_MODEL; taint = recorded north star.
Ready to implement.

## Open decisions (for autoplan) — RESOLVED
- **OD1 — profiles vs static pipeline vs hybrid** (A1/A2/A3). Which mechanism, and how much scheduler change.
- **OD2 — task authority.** Does the injected orchestrator retain free-form child-task text (injection carrier), or are tasks config-fixed with data-slots only?
- **OD3 — how does the orchestrator trigger without reading untrusted data?** Does it read the brief/summary at all, or is it purely a cron→pipeline trigger?
- **OD4 — migration/back-compat.** cap.2's `SpawnConfig.capabilities` (tool-input caps) — kept as the floor for non-CoS spawns, or superseded by profiles for CoS?
- **OD5 — single-tenant constitution check.** Is this least-privilege-against-injection (in scope) vs multi-tenant isolation (out of scope, constitutionally forbidden)? Frame it.
