# attn.2 — make the brief a worklist you can work

**Status:** SPEC — not planned, not approved. Needs `/autoplan` before any build.
**Filed:** 2026-08-02, from operator requirements given live during the v0.118.0 session.
**Supersedes:** the `attn.2` TODOS entry, which scoped only the manual trigger (~10 lines).
**Blocks on:** nothing. **Blocked by:** nothing, but see the `attn.1a-05` interaction below.

## Context

The operator has never read a brief until 2026-08-02, fifteen days after the first one was
written. Three exist on disk. Nothing ever told them so — no notification, no TUI view, and
`agentctl brief` reads a *different* artifact (`BriefRecord`: run counts and spend, no email
fields) so it reports "No brief yet" while three briefs sit in `~/.agentos-output/`.

Having read one, the operator asked for four things. They are one coherent change, not four:

1. **A fixed daily cron.** Exists (`TRIGGER_CRON=0 8 * * *`), but at 01:00 their local time.
2. **A manual fire**, "that we will use during testing and even for later."
3. **A re-run must "update the brief and bring it up to date. not be destructive."**
4. **"A manual override for tasks that I've handled from the list, and we can stop showing
   those as well."**

Taken together these describe a **worklist that shrinks as you work it**, not a report that
regenerates. A list that re-lists what you have already dealt with is a list you stop reading —
which is precisely the observed behaviour.

## Current State (verified 2026-08-02, v0.118.0)

| Fact | Evidence |
|---|---|
| The brief file is **overwritten destructively** on any same-day re-run | `cos.agents.toml:529` writes `./output/brief-{date}.md`; `native.rs:157` is `tokio::fs::write` — truncating, no append, no versioning |
| The code justifying same-day re-runs is **wrong about this** | `scheduler.rs:2478` says "harmless — brief is log-append/LWW". The *file* is last-writer-wins **destructive**, not append. The safety argument rests on a false premise. |
| The only fire path is the cron | `wait_for_trigger` (`cron_mcp.py:350`) fires only when `now >= _NEXT_FIRE_TS` |
| Restarting to force a fire **bricks the agent** | `attn.1a-05` (P1) — observed live. A restart while parked mid-`tool_use` restores a conversation the Messages API rejects, and it re-poisons its own checkpoint on every boot. |
| The sidecar wakes every **25 s** | `DEFAULT_TIMEOUT = MAX_TIMEOUT_S = 25` (`cron_mcp.py:31-32`) — the manual-fire latency budget |
| The curator **cannot read** anything on disk | `cos.agents.toml:445-451` grants `FsWrite { prefix = "./output" }` and **no `FsRead`** |
| The KB **cannot be enumerated** | `kb_search` is single-segment; no list/scan/prefix tool in either backend. This is what made brief.1's `open:*` keys write-only. |
| But `kb_get` **is a point lookup by key** | `native.rs:671` — "Read a single knowledge-base entry by segment and key" |
| Every brief item already carries `thread_id` | brief.1 (v0.117.0); 18 references in the prompt; 12 working permalinks in the 07-31 brief |
| The management API has **no KB/memory write route** | Only `GET /api/v1/memory/:ns`. An `agentctl`-driven handled-marker has nowhere to write today. |
| Carry-forward is broken | `brief-06` — worked on 07-16 (2 items), found nothing on 07-23 **and** 07-31 |
| Telegram cannot show open items even if enabled | It polls `GET /api/v1/brief` → `BriefRecord`, which has no email fields (`runs/mod.rs:86-112`). Also `TELEGRAM_BOT_TOKEN` is unset. |

**The design consequence of rows 8–9:** a handled-set keyed `handled:{thread_id}` can be read
with a per-item `kb_get` — a point lookup, no enumeration. That is the one route through the
trap that killed brief.1. Do not design anything here that needs to *list* KB keys.

## Proposed Change

Four parts. **Stage them in this order** — each is independently shippable and each is a
prerequisite for the next. This repo punishes bundling: the last increment shipped 4 criticals
in ~340 lines.

### Stage 1 — Non-destructive writes (correctness fix, ship first)

A re-run must not destroy the earlier brief. Before writing `brief-{date}.md`, preserve any
existing file for that date as `brief-{date}.r{N}.md` (N monotonic from existing files).

This is a **correctness fix**, not a feature: today the destruction is silent and the code
comment that permits same-day re-runs is wrong about it. Ship this even if nothing else here
is ever built.

Needs the curator to see what exists → see the capability decision below.

### Stage 2 — Manual fire (the original ask)

`wait_for_trigger` already wakes every ≤25 s. On each wake, also check for a sentinel file; if
present, unlink it **first**, then fire.

- **Unlink before firing** — single-shot, and idempotent under a double `touch`.
- **Must NOT call `_advance_next_fire()`** — a manual run at 07:00 must not silently cancel the
  08:00 scheduled one.
- **Path is env-configured with no default.** Absent → feature off. There is no safe universal
  default: Docker's host-shared dir is `/data/output`, the **QEMU overlay uses `/run/output`**
  (`distro/overlay/etc/agentd/cos.agents.toml:11,94`). A hardcoded path silently no-ops on QEMU,
  and this repo has already shipped a stale distro config once (brief.1).
- **The new env var MUST be added to `passenv`** (`cos.agents.toml:194`, currently
  `["TRIGGER_CRON", "TRIGGER_INTERVAL", "TRIGGER_MAX_WAIT_S"]`) **in both config copies**, or it
  never reaches the sidecar. Evidence this is easy to miss: `CRON_STATE_DIR` supports an override
  (`cron_mcp.py:216`) that is **dead in the CoS deployment** because it is not in `passenv`.
- Copy `CRON_STATE_DIR`'s degradation pattern: unwritable/absent dir → feature disabled, log
  once, trigger still works.
- **Emit a flight event** distinguishing a manual fire from a scheduled one. New event kinds are
  a **build gate**: `agentd/tests/conventions_completeness.rs` fails until they are backtick-
  documented in `CONVENTIONS.md`.

Operator contract: `touch <output-dir>/.fire-now`. An `agentctl` wrapper is optional sugar; the
`touch` is the contract.

**Collision with the scheduled fire.** `run_job` derives `child_id = "{job_id}-{date}"` and the
guard at `scheduler.rs:2482` rejects a second live child with that id. So a manual fire during a
running cycle would be **rejected with only a bare `EventKind::Error`**. Required behaviour:
detect the in-progress cycle, unlink the sentinel, and emit a *named* skip event with a reason —
never a silent refusal. (The clean long-term fix is the per-fire `child_id` from `attn.1a` §3,
deferred to `attn.1b`; do not block Stage 2 on it.)

### Stage 3 — Automatic handled-suppression

Per brief item, check whether the operator already dealt with it by replying: `threads/{id}`
for still-`UNREAD` plus a message from the operator after the counterparty's last one. Suppress
the handled ones.

Verified buildable with **no broker change and no new OAuth scope**: the broker allowlists query
params rather than paths (`cos.agents.toml:100`), `gmail.readonly` covers `threads/{id}`, and
`labelIds` return regardless of `format`.

This is `attn.1a-04`'s companion and it also produces the computable D4 proxy (the
already-handled rate), which the 14-day measure currently lacks.

### Stage 4 — Manual handled-override

**Automatic detection is insufficient, and the operator's own brief proves it.** The 07-31 brief
lists "🔴 GCP upgrade blocked" *and* "🔴 GCP billing now live" — the upgrade had already
happened. It was handled by the upgrade occurring, **not by replying to that thread**. A
reply-check would keep showing it. Same shape for paying an invoice, doing the GCP upgrade, or
deciding against a conference ticket.

So the operator needs to say "done" directly. Write `handled:{thread_id}` into
`ops:entities`; the curator reads it per item with `kb_get` — a point lookup, no enumeration.

**The open question this spec does not settle: how does the operator write that marker?**
Candidates, with the real objection to each:

| Route | Objection |
|---|---|
| Tick a checkbox in the brief markdown | Most natural surface, zero new UI. Needs curator `FsRead` on `./output`. |
| A `handled.txt` the operator edits | Same capability need; a second file to remember. |
| `POST /api/v1/memory/:ns/:key` + `agentctl` | Adds a **write** route to an API that `THREAT_MODEL.md` §9.7 just recorded as permanently exposed and unauthenticated. Real security decision, not a convenience. |
| Telegram reply verb (`handled <id>`) | Reuses an already-authenticated two-way path with `AGENTOS_APPROVAL_SECRET`. But Telegram is unset, and §8.7 names it a confidentiality sink. |

**Recommendation: tick the brief.** The brief becomes state rather than output, which is the
same mechanism Stage 1 needs, so the two share one capability grant.

### The capability decision (Stages 1 and 4 both need it)

Grant the curator **`FsRead { prefix = "./output" }`**.

The argument that it is proportionate: the curator **already holds `FsWrite` on that exact
prefix**. An injected curator can already write arbitrary files there. Adding read on the same
prefix does not widen the blast radius much, and the directory contains only briefs the curator
itself wrote plus an operator-authored handled list.

The argument to weigh it anyway: the curator is the agent that processes **untrusted email
content**, and `./output` is host-shared. Anything the operator ever puts in that directory
becomes readable by a prompt-injected agent. This belongs in `/autoplan`'s security pass, not in
a spec's recommendation.

## Acceptance Criteria

1. `touch <output-dir>/.fire-now` produces a new brief within **30 s** (25 s poll + margin), with
   **no container restart**.
2. The sentinel is unlinked before the fire; a double `touch` produces exactly **one** brief.
3. A manual fire does **not** change `next_fire_ts` — the scheduled 08:00 run still happens, and
   `cron_state.json` proves it.
4. A manual fire while a cycle is running produces a **named** skip event with a reason, not a
   bare `EventKind::Error`, and does not disturb the running cycle.
5. A same-day re-run preserves the prior brief as `brief-{date}.r{N}.md`; **no brief content is
   ever lost**. Mutation-verified by reverting to the truncating write.
6. The sentinel path is read from an env var present in `passenv` in **both** `cos.agents.toml`
   copies; a parity test fails if they disagree or if the var is missing from `passenv`.
7. Absent/unwritable sentinel dir → feature disabled, logged once, scheduled trigger unaffected.
8. An item the operator replied to is absent from the next brief (Stage 3).
9. An item the operator marked handled is absent from the next brief, **even with no reply on the
   thread** (Stage 4). Test fixture must be the GCP case: handled without a reply.
10. The QEMU/distro path uses its own output path and is covered by the parity test.

## Testing Plan

| Layer | What | Count |
|---|---|---|
| Unit (Python) | sentinel present/absent/unwritable; unlink-before-fire; `_advance_next_fire` NOT called on manual | +5 |
| Unit (Rust) | `brief-{date}.r{N}.md` rotation incl. N≥2 and a pre-existing `.r1` | +3 |
| Config parity | env var in `passenv` in both copies; sentinel path differs correctly per deployment | +2 |
| Integration | manual fire end-to-end against a real `agentd` + seeded `runs.redb` (the `/qa` harness from v0.118.0 works for this) | +2 |
| Integration | handled-suppression: replied item, manually-marked item, and an item that is neither | +3 |

**House rule:** every guard mutation-verified in both directions. This session produced six
false greens, including one where the mutation's own anchor missed and the pass looked like proof.

## Out of Scope

- **The interrupt tier** (`attn.1b`) — still gated on 8 preconditions, two of them contradictions.
- **Per-fire `child_id`** (`attn.1a` §3) and **tzdata/IANA** (§4) — ride with `attn.1b`.
- **A TUI brief view.** The operator has stated the TUI should be canonical for agent output and
  it shows neither brief artifact. That is real and unbuilt, but it is a surface increment, not
  this one.
- **Fixing carry-forward** (`brief-06`). Stage 3/4 make the day's list correct; cross-day
  carry-forward is a separate broken thing.
- **`brief-04`** (runtime-authored markdown). The 07-31 brief carries **37 HTML entities** —
  `Jane Doe &#40;jane@example.com&#41;` is what the operator reads. If Stage 4 makes the brief an
  interaction surface, this stops being cosmetic; note the coupling, do not fix it here.

## Effort

| Stage | Human | CC |
|---|---|---|
| 1 non-destructive writes | ~2 h | ~15 min |
| 2 manual fire | ~3 h | ~25 min |
| 3 auto handled-suppression | ~1 d | ~45 min |
| 4 manual override | ~4 h | ~30 min |

Stages 1+2 together are the smallest thing that answers the operator's stated ask and unblocks
testing. 3+4 are the product change.

## Rollback

Stages 1, 2, 4 are prompt/config/sidecar changes — revert the commit. Stage 3 adds Gmail calls
per brief item; if it misbehaves, remove the check from the prompt and the brief reverts to
listing everything. No schema change, no migration, so no data-loss path. (Contrast
`attn.1a`'s `INTERRUPTS` table proposal, which would have needed a `RUNS_SCHEMA_VERSION` bump
with no migration path.)

## Related

- `attn.1a-05` (P1) — restart-during-park bricks any parked agent. **Why Stage 2 must not
  restart**, and the reason the obvious workaround is forbidden.
- `attn.1a-04` (P2) — infra runs corrupt the brief's own stats; Stage 3 is its companion.
- `brief-06` — carry-forward broken since 07-16.
- `brief-04` / `brief-03` — sender-written markdown reaches the operator; escaping is a prompt
  rule, not enforcement.
- v0.118.0 (`attn.1a-core`) — uptime + staleness reporting; PR #163.
