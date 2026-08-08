# AgentOS / agentd — Project Memory

You are working on **AgentOS**: a Linux-based operating system where **agents are
the primitive, not applications**, designed to be **super light**. `agentd` is its
runtime. In the full system this process *is* the userspace (PID 1 / the boot
target); today it runs as an ordinary binary on a normal distro.

Read `docs/DESIGN.md` for the full thesis, architecture, and rationale.
Read `docs/ROADMAP.md` for the build plan — **this is the work queue.**
Read `docs/CONVENTIONS.md` before adding a subsystem, tool, or provider.

## Locked decisions — constitutional, do not drift

These were decided deliberately. Do not relitigate or quietly violate them:

1. **Cognition is remote.** The device is a thin agent host. The model is an API
   call behind `InferenceGateway`. There are **no local model weights** and no
   local inference engine. (Adding a *local backend* later is allowed only as a
   new `impl InferenceGateway`, never as a core assumption.)
2. **Single-tenant.** This is an OS for one individual. Agents are mutually
   trusting and run **in-process**. Do not add multi-user isolation, per-user
   auth, or tenancy boundaries. (Capability *scoping between agents* is in scope
   — see the roadmap — but that is about least-privilege, not distrust.)

## Current status

**⚠ OPERATOR OVERRIDE (2026-08-07): the "name mv design partners" gate below is
DEPRIORITIZED until the operator explicitly reactivates it.** Every mention of it further
down this file (the "external gate 2026-10-01" / "needs zero engineering" / "ranked above this
entire track" language, recorded across the attn.2/attn.3/attn.4 `/autoplan` sessions) is a
HISTORICAL RECORD of what was true THEN — do not treat it as a live instruction to rank mv
above other work. The operator's own words, from a retroactive `/autoplan` review that
surfaced this as a User Challenge: *"i want the mv design partner requirement relaxed till i
specifically reactivate it. remove it from all consideration till i have the system functional
for my need first."* Do not resurface mv as a priority concern, in this file's own voice or in
any `/autoplan`/`/plan-ceo-review` session, until the operator says otherwise. The gate,
deadline, and candidate pipeline are still real — this is a deliberate sequencing choice by the
operator, not a claim the opportunity is gone.

**Current version:** v0.123.0 (shipped 2026-08-07)
<!-- Updated on every release; test-enforced against agentd/Cargo.toml by
     agentd/tests/repo_consistency.rs — a stale line here fails cargo test. -->

**Latest shipped:** attn.2-R5 — manual job fire, in the TUI and the CLI (v0.123.0) — operators
can now fire any `[[jobs]]` entry on demand from `agentctl watch`'s new Jobs view (`[J]`) or
`agentctl run-job <id>`, always dispatching for real regardless of `native_cron_shadow`. This
is the answer to "how do I manually fire the brief without waiting for the schedule" — the
gap CLAUDE.md's own attn.4 entry left open (R5 was filed in TODOS.md, never built, until now).
- **Redesigned, not built to the original spec.** The original `attn.2-R5` plan
  (`docs/plans/attn.2-workable-brief.md`) built the route on `dispatch_run_job` + a synthetic
  parent id and four findings were verified against it. This ships on `dispatch_scheduled_job`
  instead (the same primitive the native tick already calls, no caller and no `RunJob`
  capability check needed) — which sidesteps two of those findings BY CONSTRUCTION (no
  synthetic parent id exists, so `reject()`'s panic-on-missing-key path and the
  nonexistent-parent model-config fallback are never reached), closes the third directly
  (`/api/v1/jobs/*/run` is in `is_mutating_route`, pinned by test), and closes the fourth with
  a new `start_reason = "manual_fire"` distinguishing a manual fire from a real scheduled one
  in `agentctl runs` — by tagging, deliberately not hiding: hiding it from run history would
  make debugging "what happened when I clicked fire now" harder, not easier.
- **New `POST /api/v1/jobs/:id/run`** — accepts only a `job_id` selecting among FIXED,
  config-declared jobs (no caller-supplied capabilities the way `/spawn` has), so it needs no
  `AGENTOS_ALLOW_PRIVILEGED_SPAWN`-style opt-in. THREAT_MODEL.md §9.5 documents the real
  tradeoff this accepts: firing a Gmail-reading job on demand now only requires reaching the
  loopback-bound management API, not being the in-process trusted orchestrator.
- **Every attempt is audit-logged regardless of outcome or transport**
  (`job_manual_fired`/`job_manual_fire_rejected`) — a deliberate departure from every other
  control command here, which logs only on the fire-and-forget FUSE path, because this is a
  new HTTP-reachable trigger for a job that may hold live Gmail access.
- **Three residuals filed, not fixed:** no rate-limit/concurrent-dedup guard (the manual-
  trigger analog of `attn.4-ratelimit-01`); firing a job on a day it already ran can overwrite
  that day's real KB data (pre-existing risk for any same-day re-fire, not introduced here);
  FUSE-source parity (the Jobs view only works over `agentctl watch --url`, since
  `agents_fs.rs` has no `system/jobs` producer file yet — the same gap as `attn.4-watch-01`).
- **Verified against the real binary, not just unit tests:** a scratch `agentd` fired
  three times (two via `curl`, one through the actual TUI, driven with a real pty), producing
  three distinct children with no collision, zero perturbation of the job's own
  schedule/occurrence-ledger state, and a complete flight-log audit trail for every attempt.

**Prev:** attn.4 — scheduler-native cron (v0.122.0) — **`[[jobs]]` now has a real
schedule and the scheduler itself fires due jobs; no LLM sits on the schedule boundary
anymore.** Ships in shadow mode by default (`native_cron_shadow = true`) — the new path
computes and logs would-fire decisions but dispatches nothing yet, running alongside the
still-live LLM-polling `wait_for_trigger` path until an operator confirms equivalence in the
field. Plan: `docs/plans/attn.4-scheduler-native-cron.md`.
- **This is the fix CLAUDE.md's own roadmap note said was required before bringing the stack
  back up.** Measured: 63/63 tool calls in a 29-minute window were `wait_for_trigger`, 414k
  tokens spent to watch a clock — ~3,456 inference calls/day, which empties the 10M/24h window
  in ~2.6h with zero briefs produced. Native `schedule` (5-field UTC cron via `croner`) removes
  the LLM from the schedule boundary entirely.
- **A new occurrence ledger** (`job_id + schedule-fingerprint + intended_fire_ts`,
  `agentd/src/scheduler_cron.rs`) replaces the too-coarse `{job_id}-{date}` scheme for native
  fires — gives every fire a stable, restart-safe identity instead of a once-daily assumption.
  Missed-fire catch-up on boot is fingerprint-gated: a restored `next_fire_ts` is trusted only
  if the schedule string hasn't changed since it was persisted.
- **A malformed `schedule` degrades ONLY that job** to manual-fire-only with a loud warning
  (`job_schedule_degraded`) — corrected mid-review from an earlier draft that said "fails boot
  closed," which would have let one operator typo brick the whole PID-1 process.
- **`/review`'s adversarial pass found and fixed a real cross-restart double-fire bug before it
  shipped** — three coordinated defects (catch-up synthesizing `now` instead of the persisted
  occurrence timestamp; boot-init discarding the ledger on every restart; the ledger update not
  guaranteed to reach disk before a crash), fixed together and proven via a new restart-across-
  a-fire test plus live SIGKILL-then-restart testing.
- **`/qa` driving the real binary found a second real bug invisible to all 970+ passing unit
  tests: a pre-existing 32-second SIGTERM-unresponsiveness regression.** The schedule-only idle
  branch used a bare `tokio::time::sleep` racing nothing, so SIGTERM landing mid-sleep waited out
  the full interval — long enough for Docker's ~10s default grace period to SIGKILL `agentd`
  before it could checkpoint. Predated attn.4 but this increment's own idle-loop keep-alive fix
  made the branch common enough to hit. Fixed with `tokio::select!`; measured 32s → 0.23s live,
  mutation-verified.
- **New `agentctl jobs <config>`** — validates every job's schedule, prints its next three fire
  times, non-zero exit on any invalid schedule (usable in CI/pre-deploy checks). Reuses the
  scheduler's own parsing/next-fire logic so it can never drift from what the real scheduler
  would do.
- **T4's scope was trimmed honestly, not silently dropped.** The plan called for both the CLI
  dry-run (built) and a live `agentctl watch` dashboard row surfacing next/last fire per job
  (NOT built — `surfaces::JobScheduleView` is populated but has no consumer in
  `agentctl/src/watch/`). Filed as `attn.4-watch-01` (P2) in TODOS.md at the plan-completion
  audit. Operator visibility until it lands: `agentctl jobs` plus the
  `job_fired`/`job_fire_skipped`/`job_schedule_degraded` flight events via the existing Logs
  view.
- **Three more residuals filed, not fixed, as genuine design questions rather than mechanical
  fixes:** `attn.4-clock-01` (P2, no sanity bound on the system clock before trusting it at
  boot — relevant once this runs as bare-metal PID 1 with no NTP guarantee yet),
  `attn.4-croner-01` (P3, `croner`'s panic-safety on adversarial input is unpinned by a test),
  `attn.4-ratelimit-01` (P2, native cron removes the soft rate-limiting friction an LLM-driven
  `run_job` call used to provide — a config typo like `schedule = "* * * * *"` on a capable job
  now has a bigger blast radius with nothing in the loop to notice).
- **Task T7 (delete the now-legacy LLM-polling prompt + `cron_trigger` MCP registration) is
  deliberately NOT built this increment** — by the plan's own rollout sequencing it only makes
  sense once a real shadow cycle confirms equivalence in the field, which requires the stack to
  actually be running first.
- **Do NOT bring the stack up or start the 14-day brief-adoption measure yet.** Per the roadmap
  note this increment closes: bring it up with `docker compose up -d` (not `start`), let shadow
  mode run at least one full cycle, confirm shadow-computed fire times match reality, THEN flip
  `native_cron_shadow = false` and start the measure.

**Prev:** attn.2 R3+R4 (v0.121.0) — **the brief is readable (exclusive
important/response_needed classification, narrowed sender-text escaping, a real
`suppressed_count`) and non-destructive to write (a per-fire `{ts}` in both the brief
filename and the KB key).** R5 (manual fire) is explicitly NOT in this increment — split
out at the /autoplan gate as its own future increment, filed as `attn.2-R5` in
TODOS.md. Plan: `docs/plans/attn.2-workable-brief.md`.
- **R3.1** dropped the `q=newer_than:1d` recency bound for `q=in:inbox` — an un-archived
  old thread was silently vanishing after one day, which is worse than a duplicate
  listing. **R3.2** gives every item EXACTLY ONE home (`important` XOR
  `response_needed`) — fixes a MEASURED shipped bug where an urgent item needing a reply
  rendered twice. **R3.3** narrowed the neutralisation rule from 5 entities to 3
  (`[`/`]`/`|`): measured against a real brief, 25 of 37 escaped entities were characters
  the old rule never named (`<`/`>`, em-dash, `$`/`'`) and zero were `[`/`]` — the model
  had generalised to over-escaping its own prose. **R3.4** adds `suppressed_count`
  (messages excluded by the 50-message Gmail fetch cap) from Gmail's own
  `resultSizeEstimate`, zero extra API calls (decision D2).
- **R4** gives `Job::render` a `{ts}` token alongside `{date}`, carried into both the
  brief filename and the `ops:briefs` KB key — a same-day re-fire used to silently
  overwrite the morning's brief on both the file and the KB side; a false comment at the
  collision guard called this "harmless — log-append/LWW," which was wrong twice over.
- **QA drove the real binary and found a real collision, not a theoretical one:** `{ts}`
  at `%H%M%S` (1-second resolution) rendered byte-identical across two `run_job` fires in
  the same trigger session, and the second `write_file` silently overwrote the first —
  the exact bug R4 exists to close, just with a narrower window. Fixed with nanosecond
  resolution. QA also found `agentctl brief`'s new `suppressed_count` line only fired in
  the non-"Quiet night" render branch, so a real brief with `run_count=0` and a nonzero
  `suppressed_count` printed nothing about it — reproduced against a live agentd (three
  published briefs, all quiet, zero suppression lines shown).
- **The mandatory /ship adversarial pass (Codex + Claude subagent, cross-model
  agreement) found two more, both fixed:** the `suppressed_count=0` CLI line read "all
  matching mail reviewed," which overclaims — the field only tracks the 50-message FETCH
  cap, not the SEPARATE "analyze up to 20 messages" cap the same prompt applies after
  fetching; reworded to "fetch cap not exceeded." And three R3 prompt fixes (the
  `q=in%3Ainbox` query, the CLASSIFICATION RULE, the `suppressed_count` computation) had
  zero `COS_PROMPT_SOURCES` pin, unlike every other measured fix this session — added and
  mutation-verified both directions.
- **Two residuals filed, not fixed, because they're out of this increment's stated
  scope:** `attn.2-ts-01` (P2) — nanosecond `{ts}` shrinks but doesn't eliminate the
  collision window; a repeated/backward-stepped system clock can still tie. `attn.2-esc-01`
  (P3) — `<`/`>` were never in the sender-text escape list, in either the pre- or
  post-R3.3 version (verified against the pre-attn.2 commit) — a pre-existing gap, not a
  regression from this PR; same class as the already-documented `brief-03`/`brief-04`
  limitation ("escaping is a prompt rule, not code enforcement").

**Prev:** attn.3 (v0.120.0) — **a SIGTERM mid-tool-call no longer persists a
transcript the provider rejects, and the numbers that decide paging are now in the log. The brief
is still NOT expected to appear — that is `attn.4`.** Plan:
`docs/plans/attn.3-real-context-window.md`.
- **THREE premises were falsified during this increment, two of them mine and one the audit's.**
  The increment was planned as "give paging a real context window" (`audit118-R1`). Codex's CEO
  voice asked for a cheap diagnostic first; the live `agentos_cos-data` volume still existed, so it
  was run — and it killed the plan. **Do not re-derive this: measure the retained curve before
  believing any paging claim.**
- **What the volume actually says** (29 min, `flight.jsonl`): only `cos-orchestrator` ever ran —
  `cos-inbox`/`cos-curator` appear **zero** times, so there was no brief because the jobs never
  fired. **All 63 tool calls are the same call**, `wait_for_trigger(timeout_s=20)`, each answering
  "next fire 14 h from now". 414,016 input tokens **to wait**. Zero `memory_paged`, zero budget or
  defer events.
- **`audit118-R1` demoted P0 → P2 and BLOCKED.** Retained context at the failure was **11,569**
  tokens against a would-be corrected trigger of 172,627 — **15× away** — and with the measured
  ~159 tokens/poll-pair the global budget dies at turn ~355 while paging first fires at ~1,086, so
  the fix is **arithmetically inert on the agent the audit headlined**. Its "independent,
  **sufficient** explanation" for the drought is **withdrawn**. It is also *blocked*, not deferred:
  **paging is lossy** (`cap_short_term` drains silently, `distill_on_complete` defaults false and is
  unset for the CoS, nothing recalls `short_term` in-run), so enabling it on `cos-inbox` would
  silently drop the oldest emails — "no brief" becomes "quietly incomplete brief" with
  `memory_paged` in the log looking like the fix working. Fixing R1 alone is an **active regression**.
- **`audit118-R2`'s premise was also wrong.** It said the 400 loop was in-memory ("no
  `checkpoint.json` in the volume"); the volume holds 65 `agent_checkpointed` and 2
  `agent_restored`, with the 400 landing **one second after each restore**, twice. Restore path —
  already fixed by attn.2. **The in-loop repair was built and then WITHDRAWN**: no producer could be
  demonstrated, and it would have **removed the circuit breaker** that currently kills the runaway
  trigger at ~29 min instead of letting it burn to the 10M ceiling.
- **What shipped instead:** the real producer is **SIGTERM landing mid-tool-call** (dispatched
  18:13:31, SIGTERM 18:13:47, 63 `tool_call` vs 62 `tool_result`). `build_scheduler_checkpoint` now
  seals it, **on the checkpoint COPY, never the live transcript** — which also makes attn.2's
  "Interrupted by a restart" wording literally true. Plus `retained_tokens_est` / `paging_limit` /
  `paging_limit_source` on `inference_request`, named that way because R1 is open and the value IS a
  spend ceiling; `context_limit` holding 5e9 would teach the next reader the window is 5e9.
- **/qa drove the real binary across five arms and corrected the brief again.** SIGTERM mid-FIFO-read
  → on-disk checkpoint **pairing violations: NONE**, restart clean (`agent_restored →
  inference_request → agent_completed`, zero errors). **My negative control was wrong:**
  `c02a376c~1` does not draw the 400 because it already carries attn.2's restore repair; QA built a
  third arm at `71414e9f~1` (pre-attn.2) which reproduces the production error verbatim. So the
  malformed **checkpoint** reproduces at the stated baseline and the **400** was already fixed one
  increment earlier — **attn.3 is defence-in-depth, not the sole fix.** Two unrequested arms also
  passed: a legacy bad checkpoint fed to the new binary still self-heals, and a *promised*
  `spawn_agent` call is correctly **not** sealed.
- **/review found 7 defects, four of them mutation-proven false greens in guards written one round
  earlier — two of those mine, in this increment.** A duplicate `agent_checkpointed`; a
  `catch_unwind` precondition; unguarded position-sensitivity (**found because a subtle mutation
  SURVIVED** while delete-the-code was caught); `retained_tokens_est` logged pre-paging with an
  `is_u64()` assertion that hardcoding `0` could not fail; and a fixture with **zero `tool_use`
  blocks** because it omitted a `step()` the helper above it warns about. Lesson recorded: mutate
  guards *subtly*, not just by deletion.
- **Honest limits, in the code and the RUNBOOK:** restore is **at-least-once** for tool side effects,
  not exactly-once (`checkpoint_interval_turns` defaults to 1 and all agents are snapshotted on any
  agent's tool boundary); the checkpoint writer does not apply the dead-child filter the restore path
  does; and a clean checkpoint does **not** prove a live agent is healthy. `attn.3-qa-01` (P2) files
  the one defect left open — `repair_and_record` still double-emits `agent_restored`, the same class
  /review fixed one function over.

**Prev:** attn.2 R1+R2 (v0.119.0) — **the CoS boots without an embeddings key and
survives restore. It still cannot produce briefs — see the audit finding below.** Plan:
`docs/plans/attn.2-workable-brief.md` (issue #164, reshaped at the /autoplan premise gate from
four stages to five corrected items; only R1 and R2 are built).
- **R1: `OPENAI_API_KEY` is no longer a boot gate.** `entrypoint.sh` exited 1 without it, and
  v0.118.0's `restart: unless-stopped` turned that into a **ten-restart loop** (measured:
  `Exited(1)`, `RestartCount=10`, CoS down a day). The key only ever bought semantic `kb_search`;
  `kb_put`/`kb_get` are point lookups the brief needs. **The Google gate still fails closed** — no
  Gmail means no brief — and `agentd/tests/entrypoint_gates.rs` now pins that asymmetry.
- **Degradation is honest, not silent.** `kb_search` returns an *explicit empty* rather than
  arbitrary nearest-neighbours: with zero vectors every point is equidistant, so un-guarded it
  returns resolved items rendered as "Open Items (carried forward)". Proven at /qa against a real
  Qdrant — the un-guarded build returns a hit at `score=0.0`. Degraded writes go to
  `kbdegraded_*`, so real embeddings are untouched and TTL eviction sweeps both namespaces.
  `SEMANTIC_DEGRADED` is tri-state and defaults to AUTO because the sidecar is a **separate
  container** the cos entrypoint cannot export into.
- **R2: `attn.1a-05` closed, both halves.** The seed loop skipped approval-parked agents and
  `state.waiting` but had **no arm for a parent awaiting a child** — which is how the CoS trigger
  parks on `run_job`. Plus a state-aware repair at the `from_checkpoint` **call site** (not inside
  it — that function cannot see which dangling ids the scheduler has promised to answer). Proven at
  /qa against the real binary: the pre-fix build reproduces the production 400 verbatim, the fixed
  build completes. **Correction: the TODOS claim that a restart loses a pending `request_approval`
  was false** — approvals were already checkpointed and seed-skipped.
- **⚠ THE PIPELINE STILL CANNOT PRODUCE BRIEFS.** `docs/AUDIT-v0.118.md` R1 (measured, not read):
  context paging is keyed to `token_budget`, a **spend ceiling**, not the model's context window —
  there is no `context_window` concept in the codebase. The Hard threshold sits at 4.5e9 retained
  tokens for the orchestrator against a 200k window, so paging can never fire. Live measurement:
  **65 inference requests in 29 minutes, 417,638 tokens, zero `memory_paged`, `output/` empty.**
  Spend is quadratic in turns. **Fix this before bringing the stack up** — otherwise it boots,
  survives restarts, and still emits nothing while burning the daily window in 2–3 hours.
- **Also open from the same audit:** the R2 repair runs only on the restore path, but the live 400
  loop was in-memory — the pairing check must run before every `InferenceRequest`.
- **/review found 16 issues including 4 mutation-proven false greens, one of them a test I wrote
  during that same review** (it read `v["agent_id"]` when the field serialises as `agent`, making
  the assertion unfailable). Lesson recorded: mutation-verifying a test and then rewriting it voids
  the proof. 27 tests, 14 mutations, all caught.

**Prev:** attn.1a-core (v0.118.0) — **the CoS now stays running, and says when it
hasn't.** The pipeline produced **three briefs in fifteen days** — note the audit has since shown
those timestamps are *two overwrites plus one write*, not three runs, so there were at least five
runs. The cause was NOT the brief logic: `docker-compose.yml` had **no `restart:`
policy on any service**, so it only ran while someone hand-typed `docker compose up`. The Linux/QEMU
path had a partial equivalent all along; the Mac path — the one dogfooded — had nothing. Plan:
`docs/plans/attn.1a-sub-daily-job-safety.md`.
- **`restart: unless-stopped` on `cos`/`qdrant`/`semantic-kb-mcp`, deliberately NOT on `agent`** (a
  run-to-completion one-shot a policy would restart-loop, re-spending tokens each exit). Plus log caps
  — a restart policy without one is unbounded growth, since a failing container now reprints forever.
  `agentd/tests/compose_policy.rs` pins the restart VALUE (so `restart: always`, which would make
  `docker compose stop` un-stoppable, fails) and couples `restart:` to a log cap.
- **⚠ Recreate required, or the whole thing is inert.** Docker fixes the policy at container
  CREATION; `docker compose start` does NOT pick it up (measured at /qa, where all three live
  containers still read `restart=no` while compose declared `unless-stopped`). `docker compose up -d`.
  Without that note this increment would have been a silent no-op — its own failure mode.
- **Staleness is on the READ path, not in the brief.** `GET /api/v1/brief` returns `server_now`;
  `agentctl brief` prints `· written 3h ago` and a banner past 26 h. The plan had said to put
  `last successful cycle` INSIDE the brief — **that cannot work**: a field written only on success can
  never report that the pipeline stopped. Server's clock (so `--url` elsewhere isn't read as
  staleness), `saturating_sub`, latest-only banner, `AGENTCTL_BRIEF_STALE_HOURS` for other cadences.
- **A broker request cap was built and then WITHDRAWN at /review.** `max_requests_per_agent` is a
  monotonic **process-lifetime** counter, not a rate limit (token minted once at boot → static
  principal; cleared only at shutdown). At ~30–55 Gmail calls/cycle, 400 would have hard-`429`'d the
  pipeline after ~7–13 days, with the new restart policy keeping the process alive to reach it — the
  exact silent stoppage this increment prevents. `attn.1a-01` (P1) has the mechanism and three fix
  shapes. **Do not set this on a long-lived provider without reading it first.**
- **Review found 6 criticals + 15 informational; QA found the recreate gap; /ship's plan audit found
  a silently-dropped item** (`M6`, now `attn.1a-04`). Three of the criticals were false-guarantee
  COMMENTS of mine, and one fix for an untested wire contract was **itself vacuous** — it re-read the
  key inside the test body, so a rename still passed. The house rule earned another instance: a guard
  that has not been mutation-tested is assumed non-functional, and a mutation whose anchor misses
  looks exactly like proof.
- **`attn.1b` (the interrupt tier) is NOT ready** — 8 preconditions, two of them contradictions: a
  no-`from` payload cannot support a VIP-sender night gate, and `^[0-9a-f]{1,20}$` accepts any
  *attacker-owned* thread id so an injected email can buzz the phone at 03:00 with a link into a
  thread the attacker wrote. Both CEO voices returned RESHAPE/DEFER on the tier (6/6 adverse); the
  operator overrode at the premise gate (decision `5ef9f33a`) and that override stands, but 1b's
  preconditions gate it.

**Prev:** brief.1 (v0.117.0) — **the morning brief's action items are addressable, and the
brief survives its own size limit. Its headline claim is WITHDRAWN.** Three prompt edits to the CoS
pipeline (`agentd/cos.agents.toml` + the three other copies) gave every item its Gmail `threadId`, a
thread permalink per row, and provider-native `open:{threadId}` keys instead of `open:{date}:{N}`.
Plan: `docs/plans/connectors-action-queue.md`.
- **The premise was wrong and five of six review passes found it independently** (Codex: `Reject`).
  Handled items still re-list. Nothing reads the `open:*` keys — `kb_search` is single-segment and no
  list/scan/prefix tool exists in either backend, so they are **write-only by construction**; the
  re-listing comes from `kb_search(segment='ops:briefs')` returning whole historical briefs, nothing
  deletes a resolved item, and neither job can observe resolution (curator has no Gmail; the inbox
  job's 24 h query cannot tell "replied to" from "quiet"). **Criterion 1 is OPEN → `brief.2`.**
  Do not let a future session re-derive this: trace the READ path before believing any KB re-key claim.
- **The brief was over its own cap before this shipped** — 8 660 B at the prompt's documented maxima
  against a hard 8 192 B, i.e. no brief that morning with no visible cause. The first fix mis-sized it
  too: `kb_put` measures the JSON-escaped payload inside a provenance wrapper (~600 B more than raw
  JSON), leaving 39 B of real margin. Caps are now in **bytes** (the store counts bytes, so non-Latin
  subjects blew a character-stated limit), plus a shed-and-retry ladder with a guaranteed-fit floor and
  a `⚠ Shortened to fit` line so a shortened brief is never mistaken for a complete one.
- **Three rounds, and every round's fixes were the next round's defect source.** /review 9 criticals →
  /qa 2 more (real `agentd` + fake provider) → /ship's fix-review round 9 more, **five of them
  mutation-proven false greens in guards written one round earlier**: a 4-line scanner window, parens
  counted inside string literals, an assertion a comment satisfied, a regex grep matching two unrelated
  sites, and a cap check covering 2 of 9 caps. All seven controls are now mutation-verified.
- **The security fix first guarded the wrong field:** `thread_id` was locked to `^[0-9a-f]{1,20}$`
  while `subject`/`from`/`ask` still reached markdown raw — `Payment overdue [Pay now](https://evil…)`
  in a subject needed no escape trick. Now entity-escaped by rule; **`brief-03` re-rated P3 → P1**
  because a prompt rule is not enforcement (real fix: runtime-authored markdown, `brief-04`).
- **Prompt adherence is UNVERIFIED and unverifiable here** (no API key, no Docker, no OAuth token;
  faking Gmail means disabling the broker's SSRF controls). The first real brief is the test. **The
  one-week operator tally decides whether this track continues at all** — if it is ~2 actions a
  morning, build nothing further.

**Prev:** ux.6a (v0.116.0) — **de-claimed the receipt chain and closed the `evidence.jsonl`
boot trap.** ux.6 was planned as an "Evidence view" surfacing the signed chain under the roadmap's label
"Provable accountability"; both CEO voices returned RESHAPE and the increment was **split at the /autoplan
premise gate**: ux.6a (this) ships the honesty + durability half with **no UI**, and `ux.6b` (the signed
action ledger) is DEFERRED, named and specified. Plans: `docs/plans/ux.6a-declaim-and-detrap.md`,
`docs/plans/ux.6b-signed-action-ledger.md`.
- **The chain could not say "no."** `EvidenceWriter::record_denied` had **zero production callers** — its
  only two call sites in the workspace were tests — so a 100%-`allowed` receipt log was a property of the
  CODE, not of any run. Wiring only the HTTP proxy would not have fixed it: `egress.rs` says in the code
  that "this proxy never starts in production". The production-reachable denial is **native scheduler
  admission**, now wired. Proven in /qa against a real agentd: `action="inference" verdict="denied"
  principal="qa-runner"`, chain still verifies.
- **Denial receipts are EDGE-TRIGGERED, and that is a security control.** `write_receipt` fsyncs under a
  mutex, so a receipt per attempt would let a retry loop force unbounded fsync'd writes to the file
  `agentd` reads at boot and — with rotation — roll the audit log to evict older segments. So the flight
  event fires per attempt; the signed receipt fires once per `(agent, reason)` episode. Deferral is NOT
  denial (ux.8′), and shutdown is not a policy verdict — neither is receipted.
- **Closes `audit86-P2-4` + `audit-S5`**, both filed against `run.1`, which shipped without them.
  `resume_chain` used to `read_to_string` the WHOLE file at every boot on a **fail-closed** path; it now
  reads a bounded 64 KiB tail, repairs a torn tail, and signature-checks the tail receipt (warn, never
  refuse — it verified *nothing* before, so failing closed would have bricked anyone who archived or
  hand-edited the file). Measured in /qa: 1 KiB → 0.14 s vs 30 MiB → 0.16 s. **Rotation needed no format
  or verifier change** — genesis anchoring only ever blocked in-place truncation, never rename, so each
  segment is a complete independently-verifiable chain.
- **/review found 6 CRITICALs across three rounds, and the third round found one in the fixes.** A boot
  panic (`hex_decode` byte-sliced a `&str`; `panic = "abort"` + PID 1), an unbounded `seq` from an
  unverified receipt, a rotation cascade that unlinked the live inode while `write_receipt` returned Ok,
  a **false-green test of mine** that never entered the code it claimed to guard, and — in the fix itself —
  a fallible unlink placed after the live rename that reintroduced the same symptom.
- **Honesty is the point.** `THREAT_MODEL.md` §8.7.1 now states the three real limits: coverage is model
  calls only; the signer **is** the audited party (self-attestation, not third-party evidence); and
  deletion/rotation seams are undetectable from the chain alone. `ROADMAP.md`'s "Provable accountability"
  and `PRODUCT-THESIS.md`'s "action receipts" are corrected, and the **mv external gate date is now named**
  (earlier of mv.3 or 2026-10-01) — it had never been recorded despite being that doc's one assigned action.

**Prev:** ux.13-TUI (v0.115.0) — **row-scoped control verbs in `agentctl watch`**. ux.13 had
shipped Cancel/SetBudget/SetCaps end to end with **no view invoking them** (`ROADMAP.md` said "TUI keys
deferred"), so the operator could not stop a runaway from the screen showing it. `[x]` opens a graded
row-action overlay (Park / Set budget / Cancel), `?` is the first help key this cockpit ever had, and the
measured footer clip (162 → ≤114 cols, with the narrow variant bounded at 80) is fixed. Was ux.3b — the
`:` palette is **STRUCK** (6/6 adverse CEO consensus; lazygit/htop answer this shape with `?`, and k9s
only needs `:` because its noun space is runtime-discovered). Plan: `docs/plans/ux.13-tui-verbs.md`.
- **Verbs run on the LOOP, never the key handler** (`App.pending_verb` + `drain_pending_verb`, placed
  after the shutdown check and before `event::poll`): `HttpSource`'s confirm client blocks up to 3 s, so a
  call during key dispatch froze the cockpit with no frame drawn. `handle_overlay_key` takes no `source`
  parameter, making it a compile-time property. The chat rail migrated onto the same slot (TODOS P2 → P3).
- **Park is guarded twice, and its LABEL carries the truth.** `park_limit()` → `None` below a 1 000-token
  floor (`0` ≡ UNLIMITED and `set_token_budget` writes the CHECKPOINTED config, so a zero-spend park
  un-capped the runaway permanently), and `park_would_widen()` blocks the normal post-exhaustion state
  (`windowed_spent > token_budget`, since the admission gate is pre-turn) where capping at the spend would
  RAISE the ceiling. New `budget_resettable` on snapshot + FUSE decides the wording, because with a window
  the park **self-expires at the next rollover** and without one it **ends the agent** — "reversible" was
  true of neither. **Both halves proven against a real agentd in /qa.**
- **/review (6 specialists + red team + a fix-review round) found 5 CRITICALs**, three in code this
  increment wrote and two in the review's own fixes: the Approvals dialog rendered by INDEX while acting on
  a pinned id (the approval gate IS the authority boundary); the footer clip was still shipping at 80 cols
  with a green test; a destructive verb could be armed below the overlay's size floor with nothing on
  screen; `park_would_widen` (above); and an appended cancel marker that regressed at the narrow widths the
  same commit had just claimed to support.
- **/qa drove the real TUI against a REAL agentd** (fake `/v1/messages` keeping agents genuinely alive via
  `ANTHROPIC_BASE_URL`), which is what proved semantics rather than frames: `budget_set` in the flight log,
  `running → deferred`, turns frozen 1699 → 1699; and with no reset window the parked agent really ends up
  `failed`. Found QA-1 — a cancelled row read a bare red `failed` with nothing attributing it to the
  operator (now `⨯ cancelled by you`).

**Prev:** ux.10 sub-part A (v0.114.0) — the `[l]` Logs view, completing ux.10. Its worst defect (90% of a
log burst dropped) was invisible to 1 689 passing tests and only appeared when the real binary was driven.

**Prev:** ux.10 sub-part B (v0.113.0) — real input widgets (`tui-input`/`tui-textarea` across the 5
hand-rolled inputs; single ratatui 0.29 held by exact pins; `step_key` threads the full `KeyEvent`).
Sub-part C (color-eyre) STRUCK at the /autoplan gate as redundant (`TermGuard` already restores on panic).
Before that, ux.3 (v0.112.0) — spawn custom agents on the fly over HTTP (p7.3-ar-02 cluster); CLI-subcommand
exec stays a P3 residual.

**UX tail:** ux.2b (v0.111.0) idle+error attention (closes cos-ux-01) → ux.3 (v0.112.0) → ux.10-B
(v0.113.0) → **ux.10-A (v0.114.0) — tail complete.** (Tags are a manual gate, but the tail IS tagged:
v0.113.0, v0.114.0 and v0.115.0 are all pushed. This line previously claimed "none tagged past
v0.113.0", which was stale and misled a session into repeating it — check `git ls-remote --tags`, not
this file.)

**AUDIT-v0.97 remediation — COMPLETE** (sweep + tail, v0.98.0→v0.109.0). Full audit: `docs/AUDIT-v0.97.md`.
Every increment ran plan→build→review→qa→ship; a holistic cross-model /review + per-increment /autoplan
reshaped scope in both directions (killed 2 over-scoped refactors, upgraded 1, struck 1 audit item as
a data-loss do-not-do).
- **Sweep stack** (v0.98–0.103): audit.2 (arm64 python, checkpoint `.restored`, ux.13 resurrection),
  run.1 (durability: flight rotation, `short_term` cap, cron catch-up, runs retention), cap.4
  (auth-consistency: whole-surface :7999 gate + deny-by-default `/spawn` + tool_override KB scoping),
  ci.2 (test blind-spots), budget.1 (metering completeness — universal spend folded into the global
  window + MaxTokens self-brick guard + universal-cancel), par.1 (drift guards). The holistic /review
  then fixed 2 escaped defects (FsRead `/spawn` exfil → privileged; checkpoint corrupt-primary →
  `.restored` fallback).
- **par.2** — config-unification RESHAPED to docs-only (`${VAR}` expansion can't express the deliberate +
  test-pinned structural Docker/QEMU config divergence).
- **hardening.1** (v0.104.0) — test+safety batch + unbroke `main`'s docker-smoke (ci.2 in-image oauth
  fixture escaped bug).
- **Behavioral:** par.1-ar-01 (v0.105.0) operator error view surfaces real tool/inference errors;
  budget.1-ar-01 (v0.106.0) MaxTokens truncation role-gate — one-shot fails, resident still parks (P0-2
  preserved) via a new `AgentEffect::CompletedTruncated`.
- **Design cluster:** cap.3 (v0.107.0) FS-capability matching anchored to startup CWD + closed a p5.8
  boot containment hole; budget.1-ar-02 (v0.108.0, P-doc) universal soft-cap documented honestly
  (reservation deferred — dormant path, single-tenant spend guardrail).
- **P3 tail:** p3.1 (v0.109.0) scheduler never aborts on a missing-agent effect + orphaned-checkpoint-tmp
  sweep. (audit86-P3-4 struck: bumping FORMAT_VERSION would cause rollback data-loss.)

**Audit tail — effectively closed.** par.3 (`agent)`-mode sed retirement) **DEFERRED at its /autoplan
premise gate** (2026-07-26): both CEO voices ranked it below the UX tail with zero user value (Codex STRIKE);
the guard "blind spot" that might have justified a cheap hardening was code-verified as overstated
(`entrypoint.sh:369`'s audit.1 ERE already fails the boot closed on a surviving installed-absolute path).
The working sed stays; revisit only as a build-time generator if it ever matters (`docs/plans/par.3-*.md`).
Only residual: port-7999 shared constant (trivial low-value config dedup).

**Next (roadmap):** **attn.4 has shipped (v0.122.0) — the scheduler-native cron path exists,
but it ships in shadow mode. Do NOT flip it live or start the 14-day measure yet.**
1. ~~**`attn.4` (P1) — give `[[jobs]]` a schedule and fire them from the scheduler**~~ ✅ shipped
   v0.122.0. `cos-inbox`/`cos-curator` both carry `schedule =` now, alongside the still-live
   LLM-polling path, gated by `native_cron_shadow = true` (default). The timezone gap from the
   original note is unchanged and deliberately out of scope: no `chrono::Local`, no `tzdata`, no
   `TZ`, so `schedule = "0 8 * * *"` means 08:00 **UTC**, not local time.
2. **Bring the stack up now** (`docker compose up -d cos`, **`up -d`, not `start`**) and let
   shadow mode run at least one full cycle. Confirm the shadow-computed fire times
   (`job_fired`/`job_fire_skipped` events, or `agentctl jobs cos.agents.toml`) match what the
   still-live LLM-polling path actually does before touching anything.
3. **Then, and only then,** flip `native_cron_shadow = false` in `cos.agents.toml`'s
   `[scheduler]` block to cut the LLM out of the schedule boundary for real.
4. **Then** the D4 measure: **does the operator stop checking email manually?** 14 days.
5. Task T7 (delete the now-legacy polling prompt + `cron_trigger` MCP registration) rides after
   step 3 confirms equivalence in the field — tracked, not abandoned.

**In parallel, needing zero engineering, and now flagged by four consecutive CEO voices:** name mv
design partners. Gate 2026-10-01, **0 of 10 named, 0 of 3 demos**. Both CEO voices in attn.3's
/autoplan ranked it above this entire track again; Codex's verdict was "attn.3 is probably a real bug
fix. It is not a company milestone. The strategic failure would be using it to defer the only dated
external validation gate." The pipeline is already inside the briefs on disk — Mayfield, MIT and Kong
(domain-verified), plus Microsoft/GitHub, NTT and Amex.

**In parallel, needing zero engineering:** name mv design partners. Gate 2026-10-01 (61 days at time
of writing), 0 of 10 named, 0 of 3 demos. Candidate contacts are already inside the briefs on disk.
Both CEO voices ranked this above the whole brief track, twice.
3. `C4` acceptance criterion: if the observed interrupt-worthy rate is **≤3/week**, retire the
   attention-router tier rather than tuning it. CLAUDE.md's standing gate: "if it is ~2 actions a
   morning, build nothing further." The three briefs analysed at /autoplan ran ~3/week.

**`attn.1b` is gated, not queued** — 8 preconditions in `docs/plans/attn.1-interrupt-tier.md`, two of
them flat contradictions (see the attn.1a-core notes above). `attn.1a`'s deferred §3 (per-fire
`child_id`) and §4 (`tzdata`/IANA timezone) ride WITH it, not before — nothing fires sub-daily until
the triage loop exists, and shipping them early is the `ux.6a record_denied` pattern.

Still the only irreversible item: **name mv design partners** (external gate 2026-10-01, needs 10
named humans + 3 booked demos, has 0 and 0, zero engineering). The /autoplan CEO review found the
pipeline **inside the brief files** — live threads with Mayfield, MIT and Kong (domain-verified), plus
Microsoft/GitHub, NTT and Amex by name. Then: `brief-07` (sanitise `agentctl brief`, ~30 min);
`p7.7-ar-03` (~half a day, kills a false `0 denied`). brief.3 returns ONLY in the reshaped
one-typed-brief form — never as a second runtime renderer.

**brief.1 IS VERIFIED WORKING — correcting a claim this file carried from 2026-07-30 to 2026-08-01.**
That claim read: "`~/.agentos-output/` has two briefs in fifteen days… the real Response Needed table
has **no Thread column** — so **brief.1 has never produced a brief**." **All three parts are now
false.** `brief-2026-07-31.md` is the first brief produced after brief.1 shipped, and it has the Thread
column, **12 working `#all/{threadId}` permalinks**, and the entity-escaping rule applied. Three briefs
exist (07-16, 07-23, 07-31). **`brief-05` is CLOSED with evidence.** The lesson is the one this file
already teaches and then fell for: a staleness claim about an artifact must be re-checked against the
artifact, not inherited from the previous session's note.

**What the artifact check on 2026-08-01 actually found** (`/autoplan` on attn.1, 3 voices, 4 CRITICALs):
- **The root cause of "3 briefs in 15 days" is uptime, not brief design.** `docker-compose.yml` had
  **no `restart:` policy on any service**, while `distro/agentos-cos.service:37-38` (the Linux path) has
  `Restart=on-failure`. The Mac path the operator actually uses never got it. Fixed in `attn.1a` §2.
- **The `every 2m` default is FIXED and landed on `main` standalone** — it ran the pipeline 31× for
  ~4.1M tokens in one morning. Default is now the 08:00 UTC daily cron, both modes `docker compose
  config`-verified.
- **`[[jobs]]` assumes once-daily and says so.** `scheduler.rs:2469` derives `child_id =
  "{job_id}-{date}"`; the collision guard (`:2482`) plus ux.8′ defer-not-brick plus
  `budget_reset_interval = 86400` means **one deferral silently stalls a sub-daily job for up to 24 h**,
  emitting only `EventKind::Error`. A per-job `token_budget` is **per-fire, not a daily fence**
  (`:2498`) — 48 fires × 500k against a 10M global.
- **Nothing in the stack can express local time.** No `chrono::Local` anywhere, no `tzdata` in the
  image, no `TZ` in the cos env — so any "07:00–23:00 local" rule silently means UTC.
- **`brief-06` confirmed and inverted.** Carry-forward found NOTHING on 07-23 **and** 07-31 ("first
  brief in the store"), while 07-16 carried 2 items. It worked once, then stopped. brief.2's premise
  (handled items *re-list*) is contradicted twice; the real defect is the opposite. Do not build it yet.
- **New:** the 07-31 brief carries **37 HTML entities** (`&#40;`, `&#60;`, `&#8212;`…), so
  `Jane Doe &#40;jane@example.com&#41;` is what the operator reads in a terminal. brief.1's
  escaping over-escapes — only `[`/`]` can forge a link. → `brief-04`.

**`attn.1b` (the interrupt tier) is NOT ready** — 8 preconditions in the plan, two of them flat
contradictions (a no-`from` payload cannot support a VIP-sender night gate; a field the agent writes
from untrusted email is tainted, not "agent-authored"). Worst finding: `^[0-9a-f]{1,20}$` accepts any
*attacker-owned* thread id, so an injected email can buzz the phone at 03:00 with a link into a thread
the attacker wrote. Both CEO voices returned RESHAPE/DEFER on the whole tier (6/6 adverse); the operator
overrode and reaffirmed (decision `5ef9f33a`) — that override stands, but 1b's preconditions gate it.

Still true, and still the only irreversible item: **name mv design partners** (gate 2026-10-01, 0 of 10
humans, 0 of 3 demos, zero engineering). The CEO review found the pipeline **inside the brief files** —
live threads with Mayfield, MIT and Kong (domain-verified), plus Microsoft/GitHub, NTT and Amex by name.
Then: `brief-07` (sanitise `agentctl brief`, ~30 min); `p7.7-ar-03` (~half a day, kills a false
`0 denied`). brief.3 returns ONLY in the reshaped one-typed-brief form
(`docs/plans/brief.3-runtime-authored-brief.md`) — never as a second runtime renderer.

**Prev-next:** **brief.2 is NOT the automatic next step** — gate it on the one-week operator
tally (see brief.1 above). Both CEO voices ranked the whole brief track BELOW three open items:
(1) name mv design partners or strike mv (external gate 2026-10-01, needs 10 named humans + 3 booked
demos, has zero of each, zero engineering); (2) `p7.7-ar-03` (~half a day — `HttpSource` hardcodes
`egress_brokered`/`egress_rejected` to 0, so the cockpit reports a false `0 denied` now that ux.6a
made denials real); (3) `audit-S3` (P1, no `SecretRewriter`). Also newly P1: **`brief-03`** —
sender-written markdown reaches the operator's brief and escaping it is a prompt rule, not
enforcement; the real fix (runtime-authored brief markdown from the typed `BriefRecord`) shares a
landing zone with `brief-04` and the two are probably one increment.
ux.3b is CLOSED as ux.13-TUI (the palette struck). Next per the plan's CEO
sequencing: **ux.6 evidence** (the only queue item serving two products — cockpit + mv governance, EU AI
Act Art.12), then evidence-gated ux.5/ux.7; Phase 11 skills + Phase 9 eBPF remain the two end-of-queue
tracks. Also open: **audit86-P1-9** needs a standalone 20-minute scope decision (are inert wrong-tier
capability grants the intended declare-then-lint design, or a gap?) — now the only live P1 in `TODOS.md`.
Residuals: port-7999 shared constant (trivial), agentctl `spawn` CLI-subcommand exec (P3), SetCaps has no
TUI (no snapshot data behind it, and it REPLACES the whole set), and the four P3s ux.13-TUI's reviews
opened (blocking verb on the loop thread, Park's rollover deadline, an HTTP route for `[d]`,
`pending_focus` stickiness).

Full per-increment completion notes: `docs/STATUS.md`.

## How to work here

- **Work the roadmap in order.** Each increment in `docs/ROADMAP.md` is a small,
  self-contained unit of work with explicit dependencies and acceptance criteria.
  Implement exactly one per branch; do not bundle several together. `main` stays
  shippable at every step. The roadmap's "How to use this with gstack" section
  describes the per-increment loop (`/plan-eng-review` or `/autoplan` → build →
  `/review` → `/qa` → `/ship`).
- **Preserve behavior across refactors.** Phase 1 begins by refactoring the loop
  into a steppable state machine; the single-agent path must keep working
  identically (the flight-recorder output for the demo should not regress).
- **Build, lint, and test before every commit — workspace-wide, from the repo
  root:** `cargo build --workspace && cargo clippy --workspace --all-targets --
  -D warnings && cargo test --workspace`. CI enforces exactly this across all
  five crates (ci.1) — per-crate commands from `agentd/` miss
  surfaces/sandbox/otel lints and go red in CI. (First workspace run rebuilds
  into the root `target/` — one-time cost.) Do not commit code that does not
  compile or that has clippy warnings.
- **Every version bump updates the "Current version" line in this file.** The
  line at the top of "Current status" is test-enforced against
  `agentd/Cargo.toml` (`agentd/tests/repo_consistency.rs`) — a release commit
  that bumps Cargo.toml without updating CLAUDE.md fails CI.
- **Linux-gated code requires a Linux clippy pass before pushing.** Any code
  under `#[cfg(target_os = "linux")]` (e.g. `surfaces/src/agents_fs.rs`) is
  never compiled on macOS, so local clippy is a false green. Run
  `make clippy-linux` from the repo root (requires Docker) before pushing a
  branch that touches Linux-gated code. This mirrors the CI step exactly.
- **aarch64-gated code requires an aarch64 clippy pass before pushing.** Any code
  under `#[cfg(target_arch = "x86_64")]` or `#[cfg(not(target_arch = "x86_64"))]`
  (e.g. `sandbox/src/lib.rs` DenySpawn gate) has different behavior on aarch64.
  Run `make clippy-aarch64` from the repo root (requires Docker and `cross` installed
  via `cargo install cross --locked`) before pushing a branch that changes
  arch-conditional behavior. `Cross.toml` at the repo root pins the Docker image
  version so `ring`'s `build.rs` gets the correct `aarch64-linux-musl-gcc`.
- **Run the TUI, don't just test it.** `agentctl watch` is a ratatui TUI: it needs a
  real pty AND a window size, so piping into it renders an empty frame that makes
  every assertion pass vacuously. The project skill
  `.claude/skills/run-agentctl-watch/` is the verified path — a stdlib pty driver
  (`driver.py`) that sends keys and captures readable frames, plus a fake `docker`
  that reproduces the compose CLI-plugin fork so the `[l]` Logs view and its
  process teardown can be exercised with no daemon. Use it for any `watch` change;
  ux.10-A's worst defect (90% of a log burst dropped) was invisible to 1 689
  passing tests and only appeared when the real binary was driven.
- **Match the existing style.** Small modules, narrow traits, minimal
  dependencies. This is meant to be a *light* runtime — justify every new crate.
- Update `docs/ROADMAP.md` (check off the increment) and any affected doc in the
  same PR as the code.

## Invariants you must preserve

- **Record everything.** Every meaningful step an agent takes emits a structured
  flight-recorder event. New behavior gets new event kinds (see the taxonomy in
  `docs/CONVENTIONS.md`). Logging is best-effort and must never crash an agent.
- **Cognition is metered.** Token/$ usage is always accounted and bounded. New
  scheduling never removes the budget guard; it builds on it.
- **Secrets come from the environment, never config or code.** `ANTHROPIC_API_KEY`
  and friends are read from env. Never log a secret. Never write one to disk.
- **Tools go behind the `Tool` trait.** Anything an agent does to the world is a
  `Tool`. **MCP is the tool ABI** — prefer exposing capabilities as MCP servers;
  native tools exist only for zero-dependency convenience.
- **The loop never panics on bad input.** Provider/tool/parse failures become
  recorded errors and `Result`, not panics.

## gstack

Use `/browse` from gstack for all web browsing. **Never use `mcp__claude-in-chrome__*` tools.**

Available skills: `/office-hours`, `/plan-ceo-review`, `/plan-eng-review`, `/plan-design-review`, `/design-consultation`, `/design-shotgun`, `/design-html`, `/review`, `/ship`, `/land-and-deploy`, `/canary`, `/benchmark`, `/browse`, `/connect-chrome`, `/qa`, `/qa-only`, `/design-review`, `/setup-browser-cookies`, `/setup-deploy`, `/setup-gbrain`, `/retro`, `/investigate`, `/document-release`, `/document-generate`, `/codex`, `/cso`, `/autoplan`, `/plan-devex-review`, `/devex-review`, `/careful`, `/freeze`, `/guard`, `/unfreeze`, `/gstack-upgrade`, `/learn`.

## Commands

Runtime code lives in `agentd/`; run agents from there. The pre-commit quality
gate is workspace-wide and runs from the **repo root** (see "How to work here").

```bash
cd agentd

# Build
cargo build                      # debug
cargo build --release            # ~2 MB size-optimized binary

# Quality gate (run before committing) — from the REPO ROOT, not agentd/
(cd .. && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace)

# Run an agent (logs to stderr; final answer to stdout; events to flight.jsonl)
export ANTHROPIC_API_KEY=sk-...
cargo run -- agent.toml          # single agent
cargo run -- agents.toml         # multiple agents concurrently (p1.2+)
tail -f flight.jsonl             # watch it think
```

No OpenSSL dependency since p2.1 (`rustls-tls`). For a static musl build:
```bash
# requires `cross` (cargo install cross) and Docker
cross build --target x86_64-unknown-linux-musl --release
```

## Repo layout

```
agentos/                   the repo root (run `claude` here)
  CLAUDE.md                this file
  README.md                project overview
  CHANGELOG.md             notable changes per release
  TODOS.md                 open technical-debt items and completed increments
  docs/
    DESIGN.md              full design & research (the "why")
    ROADMAP.md             the staged build plan (the work queue)
    STATUS.md              detailed per-increment completion notes (this file's old log)
    CONVENTIONS.md         how to extend the codebase consistently
    SPIKES/                exploratory spike docs (implementation notes per increment)
  agentd/                  the runtime (Rust crate)
    Cargo.toml             manifest
    agent.toml             single-agent example spec
    agents.toml            multi-agent example spec (p1.2+)
    README.md              runtime-specific quickstart
    src/
      main.rs              boot: load config -> wire gateway + tools -> run scheduler
      config.rs            TOML agent spec (single [agent] + multi [[agents]] forms)
      flight_recorder.rs   append-only JSONL event log
      scheduler.rs         cooperative multi-agent scheduler (p1.2+)
      agent/
        mod.rs             AgentTask state machine: step() → AgentEffect (p1.1+)
        driver.rs          single-agent backward-compat shim
      inference/
        mod.rs             InferenceGateway trait + neutral message/tool types
        anthropic.rs       remote backend (Anthropic Messages API)
      tools/
        mod.rs             Tool trait + registry
        native.rs          built-in read_file / write_file / list_dir
        mcp.rs             real MCP stdio client -> tools
  templates/               Phase 6: agent template catalogue (p6.1+)
    scout.template.toml    read-only researcher; first catalogue entry
  surfaces/                Phase 3: system surfaces (p3.1+)
    Cargo.toml             manifest (fuser dep Linux-only)
    src/
      lib.rs               re-exports snapshot types + agents_fs module
      snapshot.rs          SchedulerSnapshot / AgentSnapshot / AgentStatus
      agents_fs.rs         AgentsFs FUSE handler + mount() (Linux); stub (others)
  sandbox/                 Phase 3: kernel sandbox for MCP subprocesses (p3.3+)
    Cargo.toml             manifest (Linux-only raw syscall dependencies)
    src/
      lib.rs               SandboxRule enum + CompiledSandbox + compile()/apply_compiled()
  distro/                  Phase 2: Buildroot external tree + QEMU boot
    Makefile               build / run / test / prereqs / clean
    buildroot.config       Buildroot defconfig (x86_64 musl, busybox, cpio.gz)
    kernel-extras.config   kernel fragment: virtio-net + virtio-9p + FUSE + SECCOMP
    overlay/
      init                 /init PID-1 sh script
      agents/              mount point for /agents FUSE filesystem (p3.1)
      usr/bin/agentd       (gitignored; copied by make build)
      etc/
        resolv.conf        nameserver 10.0.2.3 (QEMU SLIRP DNS)
        agentd/
          agent.toml       demo agent config
```

Phase 6 adds further siblings: `agentctl/` (p6.2 operator CLI), more templates (p6.7 starter catalogue).

`agentctl/` layout (p6.2+):

```
agentctl/                operator CLI binary
  src/
    main.rs              arg dispatch
    list.rs              list-templates subcommand (p6.2)
    spawn.rs             spawn <template> subcommand (p6.2)
    inject.rs            inject <id> <text> subcommand (p7.3+)
    orchestrate.rs       orchestrate REPL — spawn + multi-turn SSE loop (orch.1+)
    watch/
      mod.rs             watch entry point; run_plain / run_tui
      app.rs             App state machine + View enum
      reader.rs          reads /agents/ FUSE files → AgentInfo
      views.rs           ratatui render functions
      topology.rs        TopologyGraph + build_graph() + render_tree() (p6.4)
```

`agentd/coordinator-demo.agents.toml` — multi-agent fixture for topology testing (coordinator + 2 scouts).

When in doubt about *what* to build next, the roadmap decides. When in doubt
about *how*, conventions decide. When in doubt about *why*, the design doc decides.

## Skill routing

When the user's request matches an available skill, invoke it via the Skill tool. When in doubt, invoke the skill.

Key routing rules:
- Product ideas/brainstorming → invoke /office-hours
- Strategy/scope → invoke /plan-ceo-review
- Architecture → invoke /plan-eng-review
- Design system/plan review → invoke /design-consultation or /plan-design-review
- Full review pipeline → invoke /autoplan
- Bugs/errors → invoke /investigate
- QA/testing site behavior → invoke /qa or /qa-only
- Code review/diff check → invoke /review
- Visual polish → invoke /design-review
- Ship/deploy/PR → invoke /ship or /land-and-deploy
- Save progress → invoke /context-save
- Resume context → invoke /context-restore
- Author a backlog-ready spec/issue → invoke /spec
