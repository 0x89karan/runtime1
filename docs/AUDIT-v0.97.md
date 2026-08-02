# AgentOS Audit — v0.97

**Date:** 2026-07-24 · **Scope:** full security + correctness audit of everything landed since
`AUDIT-v0.86.md` (v0.86 → v0.97: audit.1, ci.1, ux.8′, ux.11a/b/c, cap.1/2/2b, ux.12, ux.13), plus
re-confirmation of the open v0.86 remediation tail.

**Method:** 8 parallel per-subsystem adversarial auditors (read-only, high-effort) → each finding
independently verified by a second agent instructed to *refute* it (false positives dropped). 40
agents, 0 errors. **32 raw findings → 31 survived verification (1 refuted).**

**Tally (post-verification, adjusted severity):** 3 P1 · 12 P2 · 16 P3.

The headline: per-increment review is not catching **latent / cross-cutting** defects. This audit
found a bug in code shipped *this session* (F13, ux.13 cancel-resurrection), a flagship-dead-on-arm64
gap (F1), realistic state-loss (F10), and that the ux.12 approval gate is largely defeated by the
adjacent ungated routes (F15) — none of which the increments' own reviews surfaced.

---

## P1 — fix before the next feature increment

### P1-1 · arm64 CoS is non-functional — rootfs ships no python3/openssl
`distro/buildroot.aarch64.config:22` · category: distro-brick · conf 8
`buildroot.aarch64.config` omits `BR2_PACKAGE_PYTHON3` + `BR2_PACKAGE_OPENSSL` (the x86_64 config has
both). The shared overlay Makefile still copies `cos.agents.toml` + the `*_mcp.py` sidecars into the
arm64 rootfs, and those declare `command="python3"` — so on arm64 the cron/OAuth/Telegram sidecars
never start and **the flagship never fires a run or produces a brief**. Masked because `distro-aarch64`
CI is `make -n` only (never a real build/boot) and sidecar-tests run on the runner's python, never the
musl rootfs. The config header's "Diff" comment is also wrong (doesn't mention the omission).
**Fix:** add `BR2_PACKAGE_PYTHON3=y` + `BR2_PACKAGE_OPENSSL=y`; factor a shared package fragment both
arch configs include; add a real arm64 rootfs python-presence assertion so the dry-run lane stops
lying green.

### P1-2 · flight.jsonl has no rotation anywhere (audit86-P1-2, re-confirmed OPEN)
`agentd/src/flight_recorder.rs:83` · unbounded-growth · conf 9
`record()`/`record_streamed()` append with no cap/rotation; nothing in the entrypoint/init/image
rotates it. Streaming-default-on emits one line per SSE chunk → an always-on CoS accretes ~10–100
MB/day and eventually **fills the cos-data volume, at which point the co-located durable writers
(checkpoint.json, runs.redb, evidence.jsonl) start failing** and the flagship loses durability.
**Fix:** size-threshold copy-truncate self-rotation in the recorder (the otel `tail.rs` sentinel
already follows copy-truncate); keep it best-effort.

### P1-3 · short_term grows unbounded for parked/orchestrated agents + full-cloned into every checkpoint (audit86-P1-3, re-confirmed OPEN)
`agentd/src/agent/mod.rs:591` · unbounded-growth · conf 7
`short_term` is only extended; its sole drain is post-run distillation, which a never-terminating
orchestrator/parked agent never reaches. `to_checkpoint()` clones the full `short_term` into every
`AgentCheckpoint`, written per-turn — so RAM + checkpoint size grow linearly with lifetime turn count.
An always-on breaker independent of P1-2.
**Fix:** cap `short_term` (ring/eviction) + threshold-based distillation for parked/orchestrated
agents (not only at run completion).

---

## P2 — schedule into the remediation batch

**P2-1 (→P1 candidate) · checkpoint.json deleted immediately after restore → crash-loop erases all CoS state**
`agentd/src/main.rs:1223` · data-loss · conf 8 (verifier: re-rate P2→P1)
On restore, `remove_file("checkpoint.json")` runs *before* the first new checkpoint. A deterministic
startup crash after restore (bad config, OOM, panic in seed) means the next boot finds no checkpoint
and starts fresh — **entire CoS conversation, token accounting, parked children, pending approvals
permanently lost**, and the trigger recurs every boot. **Fix:** rename → `checkpoint.json.restored` on
load; delete only after the first successful `checkpoint_all`; load `.restored` if the primary is
absent. (This is the run.1 `.restored` item — treat as P1.)

**P2-2 · Universal-tier inference spend excluded from the global ceiling** (audit86-P2-7)
`agentd/src/scheduler.rs:810` · unmetered-spend · conf 9
`state.tokens_spent` is written only by the native inference path; universal-tier (subprocess) spend
is metered only per-workload in `EgressProxy` and never reaches `global_windowed_spent()`. A universal
agent with `token_budget=50M` burns 50M while the global 10M ceiling reads untouched — the global
$/token bound is silently bypassed, violating "cognition is bounded" at the global level. **Fix:** plumb
the egress metering point into `state.tokens_spent` + pre-forward-reject on the global window.

**P2-3 · ux.12 approval-token gate covers only approve/deny; the stronger spawn/inject/budget/caps routes stay ungated on the same surface**
`agentd/src/management.rs:95` · auth-inconsistency · conf 7
`POST /api/v1/spawn` is unauthenticated **and** forwards caller-supplied `capabilities` **verbatim with
no attenuation** (`scheduler.rs:2789`) — any `:7999` peer can spawn a top-level agent holding
`Credential{Google}` + `Mcp{google_oauth}` + `Spawn`. The ux.12 gate hardens the weakest route
(approve/deny) while leaving arbitrary-capability spawn open on the identical reachability surface, so
its marginal value against the bridge peer it targets is near-zero. **Fix:** gate ALL mutating routes
with the same token (or ux.5 bearer); `/spawn` must refuse to mint privileged caps (Credential/Spawn)
without an explicit operator flag.

**P2-4 · Operator cancel of a parked parent is silently reversed when its awaited child later terminates (ux.13 resurrection)**
`agentd/src/scheduler.rs:1160` · correctness · conf 7 · **regression in code shipped this session**
Cancel a parked root (e.g. the CoS trigger awaiting a `run_job` child): `handle_agent_terminal(parent)`
consumes the flag + records `outcomes[parent]=cancelled` but (root, not a key in `awaiting`) does NOT
remove the parent from `state.agents`. When the running child later terminates, the child-delivery path
checks only `agents.contains_key(parent)` (TRUE) → re-steps the parent; its flag is already consumed, so
the gate does NOT re-funnel → **a fresh inference is scheduled, the cancelled trigger resurrects, spends
more budget, and the audit trail flips `AgentCancelled`→`done`.** (ux.13 panic-safety itself holds; this
correctness gap is untested — the parked-parent test stops at the flag state.) **Fix:** gate the parent
re-step on `!outcomes.contains_key(parent) && !cancel_requested.contains_key(parent)` (or remove/sentinel
the funneled root); add a run()-level cancel-parent-then-drive-child test.

**P2-5 · tool_override silently discards KbWrite segment scoping on the injection-exposed inbox job** (cos-kbwrite-override-cap, escalated P3→P2)
`agentd/src/tools/mcp.rs:1004` · capability-bypass · conf 8
In the semantic profile, `tool_override=true` replaces native `kb_put` (which returns
`KbWrite{input.segment}`) with an `McpTool` whose `required_capability_for` returns only
`Mcp{semantic-kb, kb_put}`. The invoke gate checks only that. So `cos-inbox` (the node that ingests
attacker-authored Gmail, holding `KbWrite` for `mail:raw`/`ops:entities` only) can
`kb_put(segment='ops:briefs', key='{date}', ...)` and it's allowed — **overwriting the curator's genuine
daily brief** (same key scheme → same Qdrant point id). The declared narrower KbWrite grants are dead and
misleading. Bounded to KB integrity, semantic-profile-only. **Fix:** on the override path, AND the derived
`KbWrite/KbRead{input.segment}` with the Mcp grant for the well-known KB tools; and make `agentd check`
ERROR when a wildcard Mcp cap coexists with narrower KbWrite/KbRead on a tool_override server.

**P2-6 · Missed cron fire silently skips the daily brief; interval mode phase-shifts** (audit86-P2-3)
`docker/cron_mcp.py:253` · silently-nonfunctional · conf 8
`_NEXT_FIRE_TS` is in-memory only; `_init` always recomputes strictly-future. If agentd is down across a
scheduled fire (reboot, `compose pull && up`), the fire is silently dropped — the morning brief never
runs for that window, no signal. **Fix:** persist last/next-fire under `/data`; fire-on-startup-once
catch-up if a fire was missed while down.

**P2-7 · Broker credential-attach + forward happy-path has zero automated coverage** (ci.2)
`agentd/src/credential/mod.rs:1270` · test-coverage · conf 9
Every gateway integration test asserts only *rejection* branches (401/403/429/503). No test sets up a
mock upstream + valid credential and asserts the SUCCESS path (header allow-list forward, Bearer attach,
D3 query filter, caller-Authorization dropped, response relay). A regression that leaks a header or drops
the D3 filter ships green on the product's core security seam. **Fix:** the ci.2 loopback-mock-upstream
E2E asserting the credential is attached, caller creds dropped, only allow-listed params forwarded.

**P2-8 · Distro Makefile packages sidecars via a hand-maintained `cp` list; nothing verifies configs' referenced paths exist in the rootfs** (the distro-brick class that bit cap.2 + ux.12)
`distro/Makefile:144` · packaging-drift · conf 7
The recipe hard-codes the `cp` list despite computing `PYTHON_MCP_SRCS := $(wildcard …)`. A new/renamed
sidecar referenced in a config but not added to the `cp` line boots then fails every affected tool call.
**Fix:** drive the `cp` from the wildcard; add a test that every `[[tools.mcp_servers]]` `command`/`args`
path in the distro overlay resolves to a packaged file.

**P2-9 · runs.redb has no retention/prune; list() + publish_brief() full-scan every record** (ux.11b-ar-03)
`agentd/src/runs/store.rs:347` · efficiency/unbounded · conf 7
Both iterate + deserialize the whole RUNS table every call; nothing prunes. Grows monotonically over the
always-on lifetime; `publish_brief`'s full scan runs on the single writer lane per cron. Bounded-low at
v1 volume, unbounded over years. **Fix:** age/count retention in the writer + a time-indexed key so
window queries avoid a full deserialize.

**P2-10 · par.1/par.2 still not shipped — cross-boundary drift (denylist dup, string-matched event kinds, config fork, sed pipeline)** (audit86-P2-12/13/14)
`docker/entrypoint.sh:239` · known-open · conf 8
The acute *silent-brick* risk is now closed (audit.1's `agentd check --strict` + boot guards + docker-smoke
negative fixture turn a missed sed rule into a loud failure). What remains is structural drift: the
env-denylist is duplicated in 3 places with no sync check; agentctl matches event kinds by raw string;
the distro config is a hand-mirrored fork. Still P2. **Fix:** par.1 (golden-diff parity test + EventKind
`as_str()` exhaustiveness) + par.2 (single env-expanded config for both Docker + QEMU, delete the sed
pipeline + overlay fork).

**P2-11 · ci.2 open: sidecar self-tests + broker run against the runner's python, never the shipped image/rootfs**
`.github/workflows/ci.yml:370` · test-blind-spot · conf 7
Exactly the lane gap that hid P1-1. **Fix:** run the sidecar marker contract inside `agentos:full` (and,
for distro, against the built rootfs python) + the ci.2 broker E2E.

**P2-12 · test-flake-01 / streaming_two_agents: workspace suite intermittently fails under high test parallelism**
`TODOS.md:207` · green-lie hazard · conf 6
Async scheduler/egress/streaming tests race under high `--test-threads` (shared ports / `sched.run()`
completion → `outcomes[id]` index panic). 100% green at `-j1`. Low risk on 2–4 core runners, latent as
contention rises. **Fix:** isolate resources (unique ports, per-test runtime) or `serial_test`/nextest +
pinned CI threads.

---

## P3 — hardening / latent (sweep opportunistically)

Trust boundary
- **cap.3 / AbsPathPrefix not built** — FsRead/FsWrite identity is raw byte-prefix (symlink/case/relative); native agents lack an OS sandbox backstop. `capability.rs:150`.

Credential broker
- **caps.redb persistence is fully dead** — counts never written; per-agent caps never survive restart (cred.4-ar-01 worse than documented). `credential/mod.rs:579` — either implement the flush or delete the dead API + document in-memory-only.
- **cred.6-ar-01 `%26` param smuggling** — passthrough allow-list filters on literal `&`; percent-decode keys before the check. `credential/mod.rs:1090`.
- **token_url refresh not IP-pinned (TOCTOU/rebinding)** — re-resolves DNS after the SSRF check on the refresh-token path. `credential/mod.rs:439`.
- **normalize_path_segment misses compound-encoded traversal** — only rejects exact `%2e`/`%2e%2e`. `credential/mod.rs:531`.

Memory / KB (latent)
- **tool_override voids KbRead too** (companion to P2-5; segment caps non-functional when semantic-kb active). `mcp.rs:1004`.
- **semantic-kb-injective** — non-injective `_collection_name` + segment-blind `_point_id`; fold raw segment into point_id. `semantic_kb_mcp.py:271`.
- **semantic-kb eviction is startup-only; SEMANTIC_MAX_ENTRIES is a no-op**. `semantic_kb_mcp.py:1072`.
- **L1/L2 segment grammar divergence** — `/`-bearing and >128-char segments pass agentd, sidecar rejects. `semantic_kb_mcp.py:146`.

Flight / runs / checkpoint
- **orphaned `checkpoint.json.*.tmp` never swept**. `checkpoint.rs:203`.
- **brief attention-set excludes ALL config_seed agents, not just the orchestrator** (hung seeds hidden). `runs/store.rs:444`.

Budget / metering
- **StopReason::MaxTokens terminates unconditionally, ignoring budget_resettable** — reopens the P0-2 self-brick class (ux.8-ar-02, verifier: →P2). `agent/mod.rs:699` — gate on `!budget_resettable`.
- **universal-tier agents are uncancellable** — a runaway universal workload has no operator escape hatch (ux.13 cascade gap). `scheduler.rs:2620` — route Cancel to `universal_agents` (deregister the proxy key).

Telegram / egress
- **§8.7 egress content audit still NOT IMPLEMENTED** — email-derived approval args + full brief (NOT length-capped) egress to api.telegram.org unscanned. `telegram_mcp.py:282`.
- **GET /approvals + /brief expose email-derived content unauthenticated** on the 0.0.0.0 bind (writes gated, reads not). `management.rs:95` — accepted pending ux.5; extend the token to reads if cheap.
- **outbound GC doesn't re-push a reused id whose stale binding is still pending** — comment overclaims the checkpoint-reset fix. `telegram_mcp.py:338`.

---

## Refuted (1)
- *(scheduler)* "drain_deferred admits a deferred agent without checking cancel_requested" — REFUTED: a
  cancelled deferred agent is purged from `state.deferred` in the cancel branch of `handle_agent_terminal`
  before any drain, so no stale entry is admitted. (The ux.13 fix already covers it.)

---

## Proposed remediation build order

Foundation-first, one increment per branch, `main` shippable between:

1. **audit.2 — the acute batch** (P1 + the two data-loss/resurrection items): P1-1 (arm64 python, ~trivial
   config), P2-1 (checkpoint `.restored`, treat as P1), P2-4 (ux.13 cancel-resurrection — a hotfix to code
   I just shipped). Small, high-severity, independent. Do first.
2. **run.1 — the durability cluster** (long-open v0.86): P1-2 (flight rotation), P1-3 (short_term cap +
   parked-distillation), P2-6 (cron catch-up), P2-9 (runs retention). One coherent "always-on durability"
   increment.
3. **cap.4 / auth-consistency**: P2-3 (gate all mutating routes + attenuate `/spawn`), P2-5 (tool_override
   KbWrite scoping) + its P3 KbRead companion. Closes the real trust-boundary gaps.
4. **ci.2 + P2-11 + P2-8**: broker success-path E2E, sidecar-tests-in-image, distro packaging test. Closes
   the test blind spots that hid P1-1 and the broker seam.
5. **budget.1**: P2-2 (universal-tier global metering) + the MaxTokens self-brick P3 + universal-cancel P3.
6. **par.1/par.2**: the drift class (P2-10) — kills the sed pipeline + config fork.
7. **P3 sweep** (SSRF hardening, semantic-kb latent, egress content) — fold into the above or a `sec.3`.

Cross-cutting theme for future increments: **latent-functionality is the blind spot.** Two features
shipped silently dead (semantic-kb colons, memory-routing) and a just-shipped increment had a
correctness bug its own review missed. Consider a standing "does it work end-to-end / is there a
non-mock test" checklist item, and prefer run()-level tests for control-plane changes.
