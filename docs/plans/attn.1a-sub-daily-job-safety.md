# attn.1a — make sub-daily jobs safe (and make the CoS actually run)

**Status:** `attn.1a-core` **BUILT** 2026-08-01 (§1 §2 §5 §6). §3 §4 deferred to ride with `attn.1b`.
Split from `attn.1` at the /autoplan final gate.

## Build record — `attn.1a-core`

| § | Item | Where |
|---|---|---|
| 1 | daily-cron default | `docker-compose.yml` — landed on `main` as `3d94fde3` |
| 2 | `restart: unless-stopped` ×3, none on `agent`, launchd plist | `docker-compose.yml`, `docker/com.agentos.cos.plist` |
| 5 | brief staleness on the read path | `management.rs` (`server_now`), `agentctl/src/brief.rs` |
| 5 (M6) | exclude infra runs from brief stats | **NOT SHIPPED — deferred to `attn.1b`**, see below |
| 6 | broker request ceiling | **WITHDRAWN at /review** — see below |
| — | docs | `RUNBOOK.md` §11.12, `cos-guide.html` What's-new |

**§5's M6 half is NOT shipped, and this row exists because /ship's plan audit caught it
vanishing.** M6 was "exclude triage/infra children from the brief's `run_count`/`spend_total` by
identity, or the liveness signal sits on a surface the loop itself corrupts."
`agentd/src/runs/store.rs` is untouched by this increment. (The `start_reason != "config_seed"`
check at `store.rs:494` is PRE-EXISTING and only suppresses still-running config-seed rows.)

Deferring it is defensible under this plan's own "does it have a consumer?" test — nothing creates
48 infra children until `attn.1b`'s triage loop exists, so the exclusion would be dead code today,
which is the `ux.6a` pattern the split was made to avoid. **What was not defensible was dropping it
silently:** the build-order table assigned M6 to attn.1a-core and the build record then narrowed §5
to "brief staleness on the read path" with no residual anywhere. Now recorded as `attn.1a-04`, and
T-M6 rides with it.

**§6 was WITHDRAWN, not shipped.** `/review`'s security specialist proved
`max_requests_per_agent` is a **monotonic process-lifetime counter**, not a rate limit: the broker
token is minted once at boot and attributed to the static `cos-orchestrator` principal
(`main.rs:1667`), and the counter's only clearing site is `deregister_token` → called only at
**shutdown** (`main.rs:1374`). At ~30-55 broker requests per daily cycle, a cap of 400 would have
hard-`429`'d Gmail after ~7-13 days and stayed broken until restart — the exact silent stoppage this
increment exists to prevent, with §2's `restart: unless-stopped` keeping the process alive long
enough to reach it. Removed from both configs; full mechanism recorded as `attn.1a-01` (P1) with the
three viable fix shapes and the note that a config-parity test stays green for all of them.

**One design correction made during the build.** The plan said the liveness signal was
`last successful cycle` in the brief's `Stats` block. **That would not work** — a field written *into*
the brief only exists when the pipeline succeeded, so it can never report that the pipeline stopped.
The signal has to be on the **read** path. `GET /api/v1/brief` now stamps `server_now` and
`agentctl brief` computes age from it, so staleness is reported by the surface the operator queries,
whether or not a cycle ran. Age comes from the *server's* clock so `--url` at another host does not
render skew as staleness, and `saturating_sub` keeps a forward clock jump from claiming a
584-billion-hour-old brief.

**Mutation-verified, both directions** (6 at build time, 8 more at /review, house rule):

| Mutation | Guard that caught it |
|---|---|
| remove `cos`'s restart policy | `standing_services_restart_unless_stopped` |
| give `agent` a restart policy | `one_shot_agent_service_has_no_restart_policy` |
| restore `TRIGGER_INTERVAL:-every 2m` | `schedule_defaults_to_daily_cron_not_a_short_interval` |
| `restart: always` instead of `unless-stopped` | `standing_services_restart_unless_stopped` |
| `saturating_sub` → `wrapping_sub` | `future_created_at_reads_as_fresh…` (printed `213503982334601d 5h`) |
| skip staleness on the quiet-night early return | `quiet_night_also_carries_the_stale_warning` |
| *reverse:* comment reword + blank line | all 4 compose guards still pass ✓ |

The first attempt at mutation 1 **missed its anchor**, so the suite passed and would have looked like
proof. Anchors are now asserted before the mutation is trusted — a no-op mutation is a false green.

Also verified semantically, not just by text match: `docker compose config` resolves
`cos`/`qdrant`/`semantic-kb-mcp` → `unless-stopped` and `agent` → none.
**Parent review:** `docs/plans/attn.1-interrupt-tier.md` (926 lines: CEO + eng ×2 + DX)
**Test plan:** `~/.gstack/projects/0x89karan-runtime1/0x89karan-attn.1-interrupt-tier-test-plan-20260731-132257.md`
**Follows:** `attn.1b` (the interrupt tier) builds on this and is NOT yet ready

## Why this is its own increment

`attn.1`'s review found 4 CRITICALs. **Two of them are not attn.1 bugs at all** — they are gaps in the
`[[jobs]]` mechanism that *any* job firing more than once a day would hit, and one is a deployment gap
that explains the CoS's entire engagement problem. Fixing them inside a contested feature would bury
generally-useful runtime work and couple it to a hypothesis the artifacts don't support.

Nothing here ships a new user-facing capability. Everything here is independently correct, independently
testable, and **also unblocks the cheaper `A3` alternative** (brief 3×/day) which needs the same fixes.

All three voices converged on one sentence — the eng subagent's: *"§1 landing standalone is right and is
the one part of this plan I would ship today."*

## The reshape that kills three criticals at once

The review's most valuable output. `attn.1` assumed the triage loop would be **48 sealed `run_job`
children per day**. Make it **one long-lived resident agent** instead:

| | 48 sealed children (as planned) | 1 resident agent (chosen) |
|---|---|---|
| `child_id` collision (E1) | `"{job_id}-{date}"` is day-keyed; one deferral rejects up to 47 fires silently for 24 h | **N/A** — nothing re-dispatches |
| budget fence (E2) | `token_budget` is per-`AgentTask`, per fire → 48 × 500k against a 10M global | **one windowed budget that rebases with the global window — this IS the fence** |
| `prune()` pressure (M3/M6) | 48 closes/day, each triggering a full-table scan; brief `run_count`/`spend_total` corrupted | one long-running agent, one close |
| observability | 48 run segments/day | one agent, continuous |

This is not a workaround. A polling loop **is** a resident process, and modelling it as 48 unrelated
one-shots was the category error underneath three of the four criticals.

## Build order — split again, on the "does it have a consumer?" test

Applying the test the eng review applied to `ux.6a` (`EvidenceWriter::record_denied` shipped with **zero
production callers**, so a 100%-`allowed` receipt log was a property of the CODE, not of any run):

| § | Item | Consumer today? | Build |
|---|---|---|---|
| 1 | compose schedule fix | ✅ | **DONE — landed on `main`, `3d94fde3`** |
| 2 | `restart:` policies + launchd | ✅ this is the fix for 3 briefs in 15 days | **attn.1a-core** |
| 5 | liveness + M6 exclusion | ✅ a brief that stops running is invisible today | **attn.1a-core** |
| 6 | broker `max_requests_per_agent` | ✅ unbounded Gmail quota multiplier now | **attn.1a-core** |
| 3 | per-fire `child_id` | ❌ nothing fires sub-daily yet | **defer → ride with `attn.1b`** |
| 4 | `tzdata` + IANA config + boot gate | ❌ only `attn.1b` has a time-of-day rule | **defer → ride with `attn.1b`** |

**`attn.1a-core` = §2 + §5 + §6.** An afternoon (human ~half a day / CC ~30 min), every part exercised
by something that exists, and it fixes the verified root cause of the CoS's engagement problem. It costs
the 2026-10-01 mv gate essentially nothing.

§3 and §4 are correct fixes to a mechanism with no consumer. Landing them now would recreate exactly the
`ux.6a` pattern — two runtime changes, green tests, nothing exercising them. They stay fully specified
here and land beside the thing that needs them. Their three failing tests (T-E1/T-E2/T-E3) stay in the
test-plan artifact, unrun, as the standing proof the criticals are real.

## Scope

### 1. Land the compose schedule fix on `main` — DONE (`3d94fde3`, pushed)

Already in the working tree. `TRIGGER_INTERVAL=${TRIGGER_INTERVAL:-every 2m}` → daily cron:

```yaml
- TRIGGER_CRON=${TRIGGER_CRON-0 8 * * *}
- TRIGGER_INTERVAL=${TRIGGER_INTERVAL-}
```

Single-dash `${VAR-default}` so an explicitly empty `TRIGGER_CRON=` still selects interval mode.

**A live cost bug** — the default it replaces ran the pipeline 31× for ~4.1M tokens in one morning —
**sitting uncommitted on an unlanded branch** where a stray `git checkout` deletes it. Must not ride on
any feature's fate.

### 2. Uptime — the verified root cause of "3 briefs in 15 days"

`docker-compose.yml` has **no `restart:` policy on any of its 5 services**, while the Linux/QEMU path
already has `Restart=on-failure` + `RestartSec=10` (`distro/agentos-cos.service:37-38`). The path the
operator actually uses never got it.

- `restart: unless-stopped` on `cos`, `qdrant`, `semantic-kb-mcp`.
- **NOT** on `agent` — it is a run-to-completion one-shot; `unless-stopped` would restart-loop a
  finished template.
- A launchd plist so the stack survives reboot and a closed laptop.

### 3. Per-fire job identity (E1)

Even with the resident-agent reshape, the day-keyed `child_id` at `scheduler.rs:2469` is a live trap for
the *next* sub-daily job someone declares. The collision guard (`:2482`) + defer-not-brick (ux.8′,
`:106,:727`) + `budget_reset_interval = 86400` means one deferral silently stalls such a job for up to
24 h, emitting only `EventKind::Error`.

Make job child ids per-fire (`{job_id}-{date}-{HHMMSS}` or a monotonic seq). The code comment at
`:2478-2481` states the assumption being fixed out loud: *"cron fires once daily."*

**Two things that are NOT broken** (verified, so nobody re-derives them): a *completed* child is removed
from `state.agents` and, being in `awaiting`, never reaches `state.outcomes` (`handle_agent_terminal:1253`
vs the `else` at `:1346`) — so same-day re-runs already work. And `RunTracker` segments by
`"{agent_id}:{segment_seq}"` (`runs/mod.rs:32,68`) — run history does not collapse.

### 4. Timezone — a prerequisite for any time-of-day rule (C3/E3)

Nothing in the stack can express "local":

- No `chrono::Local` anywhere in `agentd/`, `agentctl/`, or `docker/*.py`. Six `Utc::now()` sites.
- `chrono` declared `features = ["serde"]` only (`agentd/Cargo.toml:30`).
- `Dockerfile:55-89` is `alpine:3.20` + `fuse3 bash jq curl python3` — **no `tzdata`, no
  `/usr/share/zoneinfo`**. No `TZ` in the cos environment.
- `cron_mcp.py` is UTC-only by design (`:14,:279`); `run_job` stamps `chrono::Utc` (`scheduler.rs:2468`).

Add `tzdata` to `runtime-core`, an explicit **IANA timezone config field**, and an `agentd check`
fail-closed boot gate when a time-of-day rule is declared without one (cap.1's `CapabilitiesResolved`
gate is the precedent). A fixed offset is not sufficient — the operator's own briefs put them in SGT
and PDT within one fortnight.

### 5. Liveness — so a stalled loop is visible (H2/D3)

The brief's `Stats` block gains `last successful cycle: <ts>`. Today a dead loop's only signal is its
absence.

**M6 — and it must not be corrupted by its own subject.** `publish_brief` counts every run terminal in
window and sums `spend` (`runs/store.rs:489-504`), including `still_running` unconditionally
(`:492-494`). Exclude triage/infra children by identity using the `config_seed` escape hatch already
present at `:494`, or the liveness signal sits on a surface the loop itself corrupts.

### 6. Broker request cap (M8) — one config line

`[credential_gateway.providers.google]` (`cos.agents.toml:87-99`) sets no `max_requests_per_agent`, and
there is no `caps_db_path`; `None` ⇒ **unlimited** (`credential/mod.rs:1081`). Any sub-daily loop is an
unbounded multiplier on Gmail quota and on the shared cred.7 provider-health state.

## Tests

Three of these are **written to fail against today's code** — they are the proof the criticals are real.
Full matrix in the test-plan artifact.

| id | Test | Status today |
|---|---|---|
| T-E1 | A still-live (deferred) job child does not cause the next fire to be rejected | **must fail first** |
| T-E2 | N fires cannot exceed a stated **daily** total — asserted at the level the claim is made | **must fail first** |
| T-E3 | Quiet-hours boundaries 06:59/07:00/22:59/23:00 in a configured **non-UTC** zone, plus one DST transition | **must fail first** |
| T-D3 | `docker compose config`: `restart:` present on `cos`/`qdrant`/`semantic-kb-mcp`, **absent** on `agent` | new |
| T-D2 | `docker compose config`: daily cron default; explicit empty `TRIGGER_CRON=` selects interval | new |
| T-D4 | A time-of-day rule declared without an IANA zone fails `agentd check` | new |
| T-M6 | 48 infra runs in the window do not displace brief attention items or corrupt `spend_total` | new |

**Test infrastructure note (M9):** nothing in `agentd/tests/` or `.github/workflows/` currently
references `docker-compose.yml`, and there is no YAML crate in the workspace. Use `include_str!` text
assertions in the `distro_packaging.rs` style. **Do not add a YAML dependency** (`CONVENTIONS.md:9-11`).

**House rule:** every guard mutation-verified in both directions — reintroduce the original bug (must
fail), then apply a harmless reformat (must pass). brief.1 produced five mutation-proven false greens in
guards written one round earlier, and this review found a sixth in *my own* proposed test plan (C2).

## Out of scope — deferred to `attn.1b`

Everything user-facing: `publish_interrupt`, `InterruptPublish`, the `INTERRUPTS` tables, the route,
Telegram delivery, rate caps, quiet-hours *policy*, the mute verb, the already-handled instrument, and
the triage prompt itself.

**`attn.1b` must not start until these are resolved in writing**, because the review showed they are
contradictions rather than open questions:

1. **C3 — you cannot have both M4's no-`from` payload and a VIP-sender night gate.** The runtime cannot
   evaluate "VIP sender" without a sender field. Pick one.
2. **C2 — `why` is tainted, not "agent-authored."** The agent's only input is untrusted email. Either
   escape it in `invoke` and assert as a pure function over a fuzz corpus, or delete the field.
3. **S1 — the attacker-chosen permalink.** `^[0-9a-f]{1,20}$` accepts any *attacker-owned* thread, tier
   selection is model judgement, and `AccessIncident` pierces quiet hours — so an injected email can
   buzz the phone at 03:00 with a link into a thread the attacker wrote. **M4 removed the fields that
   would have let the operator notice.** Needs a runtime-matched allowlist and a per-`tier_reason` cap
   where `AccessIncident` is the *most* bounded, not the least.
4. **H2 — suppression and overflow have no read path.** "Overflow folds into the morning brief" is
   unimplementable as scoped, and the acceptance criterion has nothing to read.
5. **H3 — the sidecar redelivers forever.** `telegram_mcp.py:317-330` deletes every `_delivered` key not
   in the current approvals set.
6. **H4 — table shape must be decided before build.** `BRIEFS` is seq-keyed and never pruned
   (`prune()` touches only `RUNS`); retrofitting means bumping `RUNS_SCHEMA_VERSION` with **no migration
   path**.
7. **H1/E5 — `publish_interrupt` must join `PROTECTED_TOOLS`** (`tools/mod.rs:97`), or `tool_override`
   on the semantic-kb sidecar can shadow it and bypass the capability, the idempotency check, and the
   rate cap.
8. **M5 — new flight event kinds are a build gate**, not a nicety:
   `agentd/tests/conventions_completeness.rs` hard-fails until they are documented in `CONVENTIONS.md`.

## Standing acceptance criteria (carried from the CEO review, unchanged)

- **C4:** if the observed interrupt rate is ≤3/week, retire the tier rather than tune it. CLAUDE.md's
  gate: *"if it is ~2 actions a morning, build nothing further."*
- **M3:** dump the ~30 skipped subjects into the brief for 14 days — the only way to measure recall,
  since the `never` tier does not exist (`gmail.readonly` cannot archive or label).
- **brief-05 is CLOSED:** `CLAUDE.md:176-177` claims brief.1 "has never produced a brief" and that the
  Response Needed table has "no Thread column." Both are false as of `brief-2026-07-31.md`. Correct it.
