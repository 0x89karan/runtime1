<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260801-124521.md -->

# attn.2 — make the CoS run, and make its brief readable

**Status:** APPROVED at the `/autoplan` final gate 2026-08-02. Build R1–R5 as corrected below.
**Issue:** #164. Supersedes the four-stage spec filed earlier the same day.

**Gate record.** CEO phase: 6/6 adverse (Codex + independent Claude subagent, 0 disagreements),
both RESHAPE — accepted, plan reshaped. Eng phase: 0 disagreements, both voices recommended
**R1+R2 only**. **The operator overrode and chose full R1–R5 scope.** That override stands and is
not re-litigated here; every P0/P1 the review raised is applied to the spec instead.

> **Build against THIS document, not the review conversation.** Five of the original spec's own
> claims were falsified during review, including one false green I wrote. Each is corrected inline
> and marked ⚠ CORRECTED.

## Why the original four-stage plan was reshaped

The original proposed: `.rN` brief rotation, a manual fire, an automatic handled-check via
per-thread Gmail reply inspection, and a manual handled-override backed by a KB `handled:{thread_id}`
set plus a curator `FsRead` grant. Three of those four are now deleted.

### The pipeline is down, and none of the original stages touched why

```
agentos-cos-1   Exited (1)   restart=unless-stopped   RestartCount=10   ExitCode=1
```

`docker/entrypoint.sh:222-234` exits 1 when `OPENAI_API_KEY` is unset. v0.118.0's
`restart: unless-stopped` turned that into a **ten-restart loop** — the scenario the /review
performance specialist predicted when they demanded the log cap (the cap is why this is not also a
disk incident). The original **acceptance criterion 1** ("`touch` produces a brief within 30 s, no
restart") was therefore **unreachable**: the feature could have been built and shipped without ever
being testable.

### Gmail archive is the handled-set, and the query ignores it

`agentd/cos.agents.toml:370` is `q=newer_than:1d` — no `in:inbox`, no `is:unread`, no label filter.
**The operator confirmed at the gate that they archive a thread once they have dealt with it.** That
gesture is free, already maintained, and strictly more complete than a reply-check: it covers the
resolved-without-replying case (invoice paid, upgrade completed, ticket declined) that the original
plan used to argue automatic detection was insufficient. It deletes Stages 3 and 4.

All three briefs are also **saturated at the `maxResults=50` cap** (15+35, 20+30, 20+30): a third of
every cycle is spent fetching mail that is then discarded.

### `.rN` rotation was a prompt rule masquerading as enforcement

The brief write happens in the **curator's prompt** (`cos.agents.toml:529`), so "preserve the old
file as `.rN`" meant instructing an LLM to `list_dir`, parse suffixes, compute max+1, `read_file`,
copy, then write — five ordered calls, correctly, every morning, forever. That is `brief-03`
repeated, the finding this repo re-rated P3 → P1 because *a prompt rule is not enforcement*.

### The `FsRead` grant argument was wrong

`FsRead` grants `list_dir` (`native.rs:185`) — directory *enumeration* over a host-shared dir,
forever. Read and write are not symmetric across that boundary: write lets a compromised curator
corrupt; read lets it **exfiltrate**, and the curator has broker egress. Under the reshape it is not
needed, so the decision disappears rather than being argued.

### Overwrites already destroyed two briefs and corrupted the project's own diagnosis

| Brief | Born | Last modified | Verdict |
|---|---|---|---|
| `brief-2026-07-16.md` | 09:13:09 | 09:13:09 | written once |
| `brief-2026-07-23.md` | 09:03:52 | **16:01:37** | **overwritten ~7 h later** |
| `brief-2026-07-31.md` | 01:09:47 | **02:55:41** | **overwritten ~1 h 46 m later** |

CLAUDE.md records "three briefs in fifteen days (09:13, 16:01, 02:55 — three unrelated times)" as
three runs at three odd hours. **Two of the three are second writes to the same file.** There were at
least five runs, not three. The KB has the same defect and the config already knew: the
`tool_override` comment (`cos.agents.toml:174-181`) calls it an "accepted tradeoff… low operational
risk since the trigger restarts are rare." `RestartCount=10` falsifies "rare".

---

## R1 — the OpenAI key stops being a boot gate

Make `docker/entrypoint.sh:222-234` warn instead of `exit 1`, and run the sidecar in its existing
mock-embeddings mode when the key is absent.

**⚠ CORRECTED — this cannot be done in `entrypoint.sh` alone.** `semantic-kb-mcp` is a **separate
Compose service** whose `environment:` block passes `OPENAI_API_KEY` and `QDRANT_URL` and **not**
`MOCK_EMBEDDINGS` (`docker-compose.yml` ~:238). Setting mock mode inside the `cos` entrypoint cannot
affect another container. R1 must add `MOCK_EMBEDDINGS` to the **sidecar service's** environment.

The sidecar needs the key only **per call**, not at startup — it warns at boot
(`semantic_kb_mcp.py:1049-1053`) and `/healthz` does not embed, so `depends_on: service_healthy`
is already satisfied without a key. So R1's diagnosis (the entrypoint gate is the only blocker) was
right; its remedy was not.

**⚠ CORRECTED — R1 is unsafe without R1.3.** `kb_search` embeds the query and asks Qdrant for
nearest neighbours (`semantic_kb_mcp.py:363`). With zero vectors everywhere, Qdrant returns `limit`
**arbitrary** points at meaningless scores, and the curator renders them as
`## Open Items (carried forward)`. Today that section honestly reads *"No items carried forward…"*.
Mock mode without a guard replaces an **honest empty with confident noise** — a silent wrong answer,
strictly worse than the current bug.

Required:

- **R1.1** `MOCK_EMBEDDINGS` on the **sidecar** service, selected when no key is present.
- **R1.2** Entrypoint warns instead of exiting. The warning must name what is off (semantic search),
  what still works (the whole brief path), and how to enable it. Silent degradation is the failure
  mode this repo keeps re-learning.
- **R1.3** Under mock mode `kb_search` returns **explicit empty** with a note. The idiom already
  exists three lines up — the 404 arm returns `{"hits": [], …, "note": "segment is empty"}`. Never
  return arbitrary neighbours.
- **R1.4 Namespace isolation.** Mock zero-vectors written into a collection that already holds real
  1536-dim embeddings **permanently degrade it, and restoring the key does not restore quality**.
  Mock mode must use a separate collection namespace, or refuse to write into a populated one.
- **R1.5** Fix the latent `EMBED_DIM` inconsistency: `_embed` falls back to `EMBED_DIM or 1536`
  (`:78`) while `_ensure_collection` uses `"size": EMBED_DIM` with no fallback (`:246-262`), so an
  unlisted `EMBED_MODEL` yields a dim-0 collection receiving 1536-dim vectors. Latent today; R1
  makes the mock path production-reachable for the first time.
- **The Google OAuth gate at `:218` keeps failing closed.** No Gmail means no brief; that one is the
  critical path. **Negative-control test required** — R1 must not widen it.
- `restart: unless-stopped` stays. R1 removes the *reason* for the loop, not the policy attn.1a
  shipped deliberately.

## R2 — survive restore (`attn.1a-05`)

**⚠ CORRECTED — my stated justification was false.** The original plan claimed an agent parked on
`request_approval` "loses the approval and dies". It does not: `ParkedApproval` is checkpointed
(`checkpoint.rs:127-129`, "written to checkpoint so in-flight approvals survive restart") and the
seed loop explicitly skips those agents (`scheduler.rs:686-688`).

**The real mechanism.** The seed loop skips approval-parked agents and agents in `state.waiting`,
but has **no arm for a parent awaiting a child** — while `state.awaiting` *is* restored
(`scheduler.rs:574-580`). The CoS trigger parks exactly that way
(`state.awaiting.insert(child_id, AwaitingParent { parent_id, call_id, deliver_content: false })`).
On restore it is re-stepped, reaches `step_need_infer`, and ships the dangling `run_job` `tool_use`
to Anthropic. That is the observed `agent_restored → inference_request → 400 → agent_failed`
sequence exactly.

This is the same data-structure confusion as the ux.13 P0 (`awaiting.values()` vs `contains_key`).

**⚠ CORRECTED — "drop the partial turn" destroys the states R2 exists to protect.** Both resume
paths deliver a `ToolResult` keyed by a `call_id` persisted in the **scheduler** checkpoint
(approval grant `scheduler.rs:2586-2591`, reject arm `:2637`, child-result delivery `:1310-1315`).
Dropping a dangling `tool_use` whose id lives in `pending_approvals` or `awaiting` produces a
`tool_result` with no matching `tool_use` — **the same 400, from the other side.**

**⚠ CORRECTED — the stated location cannot implement the correct repair.**
`from_checkpoint(cp, specs)` (`agent/mod.rs:300`) takes two params, neither carrying scheduler
state. The call sites are `scheduler.rs:351` and `:365`, inside the block that destructures
`cp_awaiting` (`:312`) and `cp_pending_approvals` (`:318`) — the information is in scope *there*.

Required, and **both halves are needed**; either alone leaves a live bug:

- **R2.1 The missing skip.** Add an awaiting-parent arm to the seed loop (`scheduler.rs:686-691`),
  matching the two existing skip patterns. Without it, a message-only repair lets the trigger infer
  successfully, `run_job` **again** (a duplicate token-spending cycle), and then 400 anyway when the
  original child returns a `ToolResult` for a `call_id` no longer present.
- **R2.2 State-aware repair at the call site.** Never drop a dangling `tool_use` whose id is in
  `cp_pending_approvals` or `cp_awaiting` — those are legitimately pending. Synthesize an error
  `tool_result` only for genuinely orphaned interrupted batches.
- **R2.3 Pin one strategy.** If the dangling `tool_use` is the **only** block, the whole `Msg` must
  be dropped: stripping blocks leaves `blocks: []`, which the API rejects ("all messages must have
  non-empty content"). In the mixed case (text + tool_use), dropping the Msg loses the assistant's
  text. Choose and state it; do not ship "repair or drop".
- **R2.4** Assert in the plan that turn/budget accounting is safe under a drop: `to_checkpoint`
  persists `turn`/`total_input`/`total_output`/`window_anchor` independently of `messages`, and
  `provide_inference` accumulates spend (`mod.rs:424-425`) before the assistant turn is pushed;
  `turn` increments only in `provide_tool_results` (`:566`), which never ran. One turn is paid for
  and not counted — the conservative direction.

**This is not a rare SIGTERM race.** `default_checkpoint_interval_turns()` returns **1**
(`config.rs:449-451`) and `cos.agents.toml` never overrides it, and the periodic write snapshots
*all* agents (`scheduler.rs:956-962`). While the inbox child burns up to 600 turns, **every one of
those checkpoints contains the trigger mid-`run_job`.** Any restart during a cycle lands on a
poisoned checkpoint.

## R3 — make the brief readable

Edits land in **both** `cos.agents.toml` copies with the correct path in each (see the Drift Trap).

### R3.1 Inbox-scope the query

**⚠ CORRECTED — `in:inbox` + `newer_than:1d` yields no carry-forward.** A three-day-old thread is
not `newer_than:1d`, so acceptance criterion 5 and the "unhandled thread from Tuesday reappears on
Friday" claim are in direct conflict with keeping the recency bound. **The recency bound must be
dropped or widened.** This is the 2am-Friday failure: an old unresolved item silently vanishes.

**⚠ CORRECTED — the value needs percent-encoding.** `passthrough_query_params`
(`credential/mod.rs:1129-1147`) is confirmed a **name-only** allowlist and `q` is on it, so no code
change is needed. But the sidecar forwards the raw query string with no encoding
(`oauth_mcp.py:667`), and a raw space in a request target is malformed. The prompt must specify
`q=in%3Ainbox` form, not `q=in:inbox newer_than:1d`.

**⚠ PRECONDITION — measure inbox depth first.** The whole premise rests on an unmeasured quantity.
If the inbox holds more than ~50 un-archived threads, `in:inbox` returns the 50 newest and "not
archived" stops meaning "not handled" for everything older — the same saturation, no gain. One
`messages.list?q=in:inbox` call settles it. `pageToken` is allowlisted (`:99`) but never used, so
there is no pagination today.

### R3.2 Dedup Important against Response Needed

`cos.agents.toml:541` draws Important from "important + response_needed", so every response-needed
item renders twice.

**⚠ CORRECTED — measurements and criterion.** Actual 07-31 counts are **6 bullets + 6 table rows +
5 open items = 17** (not 6+7+7=20), and the two rows for one sender are **two different threads**,
not a duplicate. More important: "no item appears in both sections" applied to this artifact would
delete **5 of the 6 Important bullets**, collapsing the section the operator reads first and
destroying the 🔴/🟡 urgency signal. **Correct shape:** Important shows all actions with urgency;
the table shows only the non-Important remainder. (One merged action list is `brief.3`'s
one-typed-brief and is out of scope here.)

Pure-notification open items ("a meeting was cancelled", "an unviewed report exists") should not
occupy action rows — 5 of 7 on 07-31 were of this kind.

### R3.3 Stop over-escaping

**⚠ CORRECTED — the original premise was false.** The rule names five characters
(`[ ] ( ) |`), but the measured census of the 07-31 brief is:

| Entity | Char | Count | Named in the rule? |
|---|---|---|---|
| `&#62;` `&#60;` | `>` `<` | 16 | **no** |
| `&#8212;` | em-dash | 6 | **no** |
| `&#40;` `&#41;` | `(` `)` | 12 | yes |
| `&#36;` `&#39;` | `$` `'` | 3 | **no** |

**25 of 37 are characters the rule never mentions, and zero are `[`/`]`.** The model generalised to
HTML-escaping anything markdown-ish — it even escaped its own prose em-dashes. Removing `(`/`)`
deletes 12 of 37 and leaves the address exactly as broken. The fix is a **negative instruction**:
*escape only these; never HTML-escape any other character.* Different edit, different adherence risk.

**⚠ CORRECTED — `|` must stay escaped.** `subject`, `from`, `ask` and `deadline` render into
markdown **table cells** (`:544-546`). A raw `|` in a sender-controlled subject splits the row and
shifts every later cell — including moving the Thread permalink out of its column. The exposure is
**untested, not absent** (no 07-31 subject happened to contain a pipe). `(`/`)` are genuinely safe
once `[`/`]` are escaped, since a `)` cannot close a link the sender never opened. **Narrowing is
`[ ] |`, not `[ ]`.**

**⚠ CORRECTED — this touches a `THREAT_MODEL` §9.5-cited guard.** `config.rs:2233-2239` asserts all
five entities appear verbatim in the prompt. Removing `(`/`)` fails it. Amending that test is a
review-gated decision, and per the house rule the narrowed guard needs **mutation verification in
both directions**. The original Testing Plan did not mention it.

### R3.4 Emit `suppressed_count`

**⚠ CORRECTED — not obtainable by a prompt edit, and it contradicts the original plan's own note.**
Two problems: (a) suppression happens *inside Gmail*; a query returning only inbox mail cannot
report what it excluded, so the count needs a **second Gmail call** (e.g. `-in:inbox`) — which the
original text explicitly denied doing, in the same breath as citing `attn.1a-01`; (b) **a prompt
cannot emit a flight event.** Event kinds are emitted by `agentd`; there is no model-callable event
tool and `publish_brief` writes a `BriefRecord` narrative, not an event. So R3.4 is Rust work plus
the `CONVENTIONS.md` build gate.

This still must ship: if suppression is silent, the measure that decides this track's future reports
zero and looks like a track with no problem to solve.

### R3.5 Update the stale config comment

`cos.agents.toml:455-469` prescribes exactly the deleted Stage 3 ("let the inbox job read the open
set and check, per open thread, whether the newest message is from the operator"), and asserts two
things R3 makes false: that `kb_search(ops:briefs)` is "the ONLY source of carried-forward items",
and that "the inbox job only queries `q=newer_than:1d`". This repo has a documented instance of a
stale note misleading a later session. Update in both copies.

## R4 — non-destructive brief writes

**⚠ CORRECTED — this was a false green, and it is the most important correction in this document.**
`Job::render()` is `self.task.replace("{date}", date)` and its doc comment says `{date}` is *"the
only placeholder"* (`config.rs:91-95`; call site `scheduler.rs:2467-2493`). The curator has no clock,
no shell, and no time tool. So `write_file(path='./output/brief-{date}T{HHMMSS}Z.md')` reaches the
model with a **literal `{HHMMSS}`** — producing a *new fixed filename that still truncates* — or the
model hallucinates a time. Worse: the original criterion 9 mutation control ("revert to the fixed
filename") **would have passed either way, because the shipped behaviour is the fixed filename.**

Required:

- **R4.1** Extend `render()` with a `{ts}` token (server-stamped, same trusted path as `{date}`),
  plus a guard test pinning prompt/substitution agreement so a prompt using `{ts}` cannot ship
  against a runtime that does not substitute it.
- **R4.2** Prompt writes `brief-{date}T{ts}Z.md` in both copies.
- **R4.3** Update the operator docs that hardcode the fixed name: `DEPLOYMENT.md:194`, `:498`,
  `RUNBOOK.md:741` all use `cat …/brief-$(date +%Y-%m-%d).md`. The `ls brief-*.md` globs
  (`DEPLOYMENT.md:497`, `RUNBOOK.md:740`, `distro/agentos-cos.service:29`,
  `docs/architecture-diagram.html:403`) survive unchanged.
- **R4.4** Correct the false comment at **`scheduler.rs:2480`** — "harmless — brief is
  log-append/LWW" is wrong twice: the file truncates and the KB record is overwrite-by-UUID5.

Verified safe: `BriefRecord` (`runs/mod.rs:87-112`) carries no filename, so `publish_brief` and
`agentctl brief` are untouched; the entrypoint desync guard and its sed rule anchor on the path
*prefix* only (`entrypoint.sh:245`).

**Note:** this is not "one token, ~5 min". Reversibility drops from 5 to 4 (a Rust change plus tests).

## R5 — manual fire

**⚠ CORRECTED — SECURITY. The sentinel must not live in `./output`.** The curator holds the only
`FsWrite` grant in the file, `prefix = "./output"` (`:451`), and the curator is the agent that
ingests untrusted email. A successful injection can `write_file('./output/.fire-now')` and trigger
unscheduled full cycles. Arithmetic: a cycle costs `1_500_000` (inbox) + `500_000` (curator) = 2M
against `global_token_budget = 10_000_000` on an 86400s window — **five injected self-fires exhaust
the day and the pipeline silently stops.** This is a write-to-control-flow path, and it is the same
read/write asymmetry used to reject the `FsRead` grant. Put the sentinel in a directory the curator
cannot write, or make the fire a control-API verb.

**⚠ CORRECTED — the env var needs a compose entry too, or R5 ships inert.** The original named only
`passenv` (`agentd/…:194`, `distro/…:122`). A *new* var is also absent from the compose
`environment:` block, and with "no default → feature off" the default deployment has R5 **disabled**
— criteria 10–12 could not pass without an undocumented operator step. The `distro` path
(`agentos-cos.service`, `overlay/init`) is a third site. The parity test must cover all of them, not
just `passenv`.

**⚠ CORRECTED — the named skip event cannot be emitted by the sidecar.** `cron_mcp.py` is a
sandboxed stdio subprocess (`DenySpawn + IsolateNetwork`, `main.rs:1489-1495`) with no view of
`state.agents`. The collision guard lives in Rust at **`scheduler.rs:2484`** and currently emits a
bare `EventKind::Error`. The named event must be emitted there — Rust work plus the `CONVENTIONS.md`
gate.

**⚠ CORRECTED — `outcomes` retention means criteria 9 and 12 exercise different branches.** The
guard is `state.agents.contains_key(&child_id) || state.outcomes.contains_key(&child_id)`, so a
*completed* same-day cycle collides too. "Two fires on the same date produce two files" and
"collides with a running cycle → named skip" are not the same path.

Remaining requirements:

- Wake is `min(_NEXT_FIRE_TS, now + timeout_s)` and the prompt passes **`timeout_s=20`**
  (`cos.agents.toml:300`), not 25 — plus a full LLM round-trip. Criterion 10's "within 30 s" is
  tight; state it as a target with the round-trip acknowledged.
- **Must not call `_advance_next_fire()`** (a 07:00 manual run must not cancel 08:00) — but note
  that also skips `_persist_next()`, and `_WAIT_START` reset must be decided explicitly (it matters
  when `TRIGGER_MAX_WAIT_S` is set).
- **Catch-up race:** if `_NEXT_FIRE_TS` is already past (`_apply_catchup` returning `now`), a manual
  fire and the catch-up fire race for the same `child_id`.
- **Unlink-before-fire is lossy**: a crash between unlink and fire drops the request, and
  `exists`/`unlink` races a second `touch`. Use an atomic claim (rename) or document lossy semantics
  explicitly.
- **A pre-existing sentinel must be unlinked at init WITHOUT firing.** `~/.agentos-output/.fire-now`
  **already exists** (empty, 2026-08-01 11:29:13), so without this the first wake after R5 ships
  fires an unrequested brief from a day-stale sentinel. Acceptance criterion, not a nicety.
- **Liveness caveat, stated honestly:** the sentinel is read inside `handle_wait_for_trigger`, which
  only runs when the model calls the tool. If the trigger is `failed`, deferred on budget, or
  cancelled, `touch` is a **silent no-op** — precisely the situation an operator reaches for a manual
  fire. R2 is what makes R5 reliable; R5 alone does not rescue a wedged trigger.

## Architecture

```
  BEFORE (as shipped, currently Exited(1) x10)
  ┌──────────┐   cron only    ┌───────────┐  q=newer_than:1d  ┌─────────┐
  │ cron_mcp │───────────────▶│ cos-inbox │──────────────────▶│  Gmail  │
  └──────────┘                └─────┬─────┘  (ignores triage)  └─────────┘
                                    │ kb_put ops:entities
                                    ▼
                              ┌───────────┐  kb_search ops:briefs (brief-06, broken-empty)
                              │cos-curator│──────┐
                              └─────┬─────┘      └──▶ needs key ──▶ entrypoint exit 1 ◀─ R1
                                    │ write_file (TRUNCATES) ◀─ R4
                                    ▼
                            ./output/brief-{date}.md   ── 0% delivery rate

  AFTER (R1–R5)
  ┌──────────┐  cron + sentinel  ┌───────────┐  inbox-scoped   ┌─────────┐
  │ cron_mcp │──────────────────▶│ cos-inbox │────────────────▶│  Gmail  │
  └──────────┘   ▲               └─────┬─────┘  archive=handled└─────────┘
                 │ R5: sentinel in a   │ kb_put (mock-safe, isolated namespace)
                 │ dir the curator     ▼
                 │ CANNOT write   ┌───────────┐  kb_search → explicit empty under mock ◀─ R1.3
                 └───────────────┐│cos-curator│  carry-forward comes from the inbox, not the KB
                                 │└─────┬─────┘
                                 │      │ write_file, {ts}-stamped ◀─ R4 (needs render() change)
                                 │      ▼
                    ./output/brief-{date}T{ts}Z.md  + suppressed_count (Rust event ◀─ R3.4)
```

## Error & Rescue Map

| Codepath | What can go wrong | Rescued? | Action | Operator sees |
|---|---|---|---|---|
| `entrypoint.sh:222` | `OPENAI_API_KEY` absent | Y (R1.2) | warn; sidecar mock mode | named degrade line |
| `entrypoint.sh:218` | Google OAuth absent | Y (unchanged) | `exit 1` — correct | existing error block |
| `_embed` mock | zero vectors | Y (R1.4) | isolated namespace | real collection unpoisoned |
| `kb_search` mock | arbitrary neighbours | Y (R1.3) | explicit empty + note | honest "none carried forward" |
| `_ensure_collection` | unlisted `EMBED_MODEL` → dim 0 | Y (R1.5) | fallback to 1536 | startup error, not silent |
| seed loop | awaiting-parent re-stepped | Y (R2.1) | skip arm | agent survives |
| restore | dangling `tool_use` | Y (R2.2) | state-aware repair | no 400 |
| restore | tool_use is the only block | Y (R2.3) | drop whole `Msg` | no empty-content 400 |
| resume | `ToolResult` for dropped id | Y (R2.2) | never drop pending ids | approval/child delivers |
| Gmail query | 0 in-inbox messages | Y | explicit "nothing to action" | not mistaken for failure |
| Gmail query | inbox deeper than cap | **N — GAP** | no pagination exists | `suppressed_count` + shortened banner |
| table render | raw `\|` in subject | Y (R3.3) | keep `&#124;` | Thread cell stays in column |
| `write_file` | same-date re-fire | Y (R4.1) | `{ts}` substituted in Rust | no loss |
| sentinel | stale at boot | Y (R5) | unlink, do not fire | no spurious brief |
| sentinel | curator-authored (injection) | Y (R5) | outside curator `FsWrite` | no self-fire |
| sentinel | crash between unlink and fire | **N — accepted** | documented lossy | re-`touch` |
| run_job | child-id collision | Y (R5) | named skip event in Rust | reason, not bare `Error` |
| trigger wedged | `touch` is a no-op | **N — accepted** | R2 is the real fix | documented in RUNBOOK |

**Three rows are honestly N.** The original registry claimed "No row is RESCUED=N", which was a
stronger claim than the work supported — this repo's own lesson is that a green-looking control is
worse than a missing one.

## The Drift Trap

| | Dev / Docker | Distro / QEMU |
|---|---|---|
| file | `agentd/cos.agents.toml` | `distro/overlay/etc/agentd/cos.agents.toml` |
| `FsWrite` prefix | `./output` (:451) | `/run/output` (:347) |
| brief write | `./output/brief-…` (:529) | `/run/output/brief-…` (:420) |
| `passenv` | :194 | :122 |

Plus, for R5's env var: the compose `environment:` block **and** `distro/agentos-cos.service` /
`distro/overlay/init`. The parity test must cover every site, not just `passenv`.

## Acceptance Criteria

1. With no `OPENAI_API_KEY`, `docker compose up -d cos` boots, logs one line naming semantic search
   as degraded, and produces a brief. `RestartCount` stays 0.
2. **Negative control:** with no Google OAuth token the boot still fails closed.
3. Under mock mode, `kb_search` returns explicit empty — never arbitrary hits. Mutation-verified.
4. Mock vectors never enter a collection holding real embeddings.
5. An agent checkpointed mid-`tool_use` restores and its **next real inference succeeds**.
6. An agent parked on `request_approval` survives a restart and the grant still delivers.
7. A restored awaiting-parent is **not** re-stepped and does **not** run a duplicate cycle.
8. A thread the operator archived does not appear in the next brief; one left in the inbox does,
   **including one first seen three days earlier**.
9. Every action appears once; the Important section still carries urgency markers and is not
   collapsed to a single row.
10. An address renders as `name@example.com`; a subject containing `[x](url)` still cannot produce a
    live link; a subject containing `|` does not shift the Thread cell. All three mutation-verified.
11. `suppressed_count` appears in the brief and in a flight event.
12. Two fires on the same date produce two files; no content is lost. Mutation control must fail
    when `{ts}` substitution is removed — **verify the control actually fails.**
13. `touch <sentinel>` produces a brief within ~30 s with no restart; a double `touch` produces one;
    `next_fire_ts` unchanged.
14. A sentinel present at boot is removed and does not fire.
15. The curator cannot write the sentinel path (capability test).
16. A collision emits a named skip event; the completed-cycle branch is tested separately.
17. All config sites agree on the new env var; parity test fails if not.

## Testing Plan

| Layer | What | Count |
|---|---|---|
| Unit (Python) | mock `_embed`; `kb_search` explicit-empty; namespace isolation; `EMBED_DIM` fallback | +5 |
| Unit (Python) | sentinel present/absent/unwritable/stale-at-init; no `_advance_next_fire`; catch-up race | +6 |
| Unit (Rust) | awaiting-parent seed skip (+ negative control: non-awaiting agent IS stepped) | +2 |
| Unit (Rust) | state-aware repair: orphaned dropped, pending-approval preserved, awaiting preserved, tool-only Msg dropped whole | +4 |
| Unit (Rust) | `render()` `{ts}` substitution + prompt/substitution agreement guard | +3 |
| Config parity | env var across all sites; per-deployment paths; both `cos.agents.toml` copies | +3 |
| Guard amendment | narrowed escaping rule, mutation-verified both directions | +2 |
| Integration | boot with no key → brief; boot with no OAuth → still closed | +2 |
| Integration | manual fire end-to-end against a real `agentd` | +2 |
| Prompt fixture | dedup, de-escape, link-forgery and pipe-injection negative controls | +4 |

**⚠ The `/qa` fake provider does not enforce `tool_use`/`tool_result` pairing**, so it will accept a
still-broken history — a false green of exactly the shape the house rule warns about. R2's tests must
assert the pairing invariant directly, not rely on the fake accepting the request.

**House rule:** every guard mutation-verified in both directions. The v0.118.0 round produced six
false greens, one where the mutation's own anchor missed and the pass looked like proof. This plan
already contained one (R4).

## Out of scope

| Item | Why |
|---|---|
| Stage 3 (per-thread reply check) | Deleted. `in:inbox` covers it; the original predicate was self-contradictory — a replied thread is not `UNREAD`. |
| Stage 4 (KB handled-set + operator write path) | Deleted. Archive is the handled-set. If 14 days shows otherwise, the surface is the authenticated Telegram verb. |
| Curator `FsRead` / `.rN` rotation | Replaced by `{ts}` naming. |
| `POST /api/v1/memory` | Would add a write surface to an API `THREAT_MODEL.md` §9.7 calls permanently exposed. |
| TUI brief view / Telegram delivery | **The brief still has a 0% delivery rate after R1–R5.** Telegram is built (ux.12) and unconfigured; strongest candidate for the next increment. |
| `brief-04` runtime-authored markdown | R3.2/R3.3 are rendering concerns expressed as prompt rules — the same pattern this plan indicts elsewhere. `brief-04` is the enforcement version and will re-litigate them. Coupling accepted knowingly. |
| `attn.1b`, per-fire `child_id`, `tzdata` | Gated elsewhere. |

## Effort (revised after review)

| Item | Human | CC | Reversibility |
|---|---|---|---|
| R1 boot + mock safety | ~4 h | ~30 min | 5 |
| R2 restore (both halves) | ~5 h | ~35 min | 4 |
| R3 prompt + Rust event | ~6 h | ~45 min | 4 |
| R4 `{ts}` + docs | ~2 h | ~20 min | 4 |
| R5 manual fire | ~5 h | ~40 min | 4 |

R1+R2 alone deliver a pipeline that runs and stays up, and remain the honest first commit even under
full scope.

## Sequencing

**R1.3 must ship with or before R1.** Mock mode without the explicit-empty guard replaces an honest
empty with confident noise. **R2.1 and R2.2 ship together.** **R4 precedes R5**, or manual double
fires still overwrite. Measure inbox depth before R3.1.

Recorded at the gate: the mv design-partner deadline is 2026-10-01, 0 of 10 named, zero engineering
required, and the briefs on disk already name roughly seven domain-verified candidates. CLAUDE.md
calls it "the only irreversible item". Not a reason to skip R1–R5 — recorded because it competes for
the same attention.

## Related

- `attn.1a-05` (P1) — **is R2.** Currently bricking the CoS.
- `attn.1a-01` (P1) — `max_requests_per_agent` lifetime counter. **R3.4 adds a Gmail call against
  it**, contrary to the original plan's claim that it did not.
- `brief-06` — carry-forward. **Retired, not fixed:** `in:inbox` makes it unnecessary.
- `brief-03` / `brief-04` — sender-written markdown; R3.3 narrows the rule and amends its guard.
- v0.118.0 — shipped the restart policy whose interaction with the R1 boot gate produced the loop.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 1 | CLEAR (via /autoplan) | 4 proposals, 1 accepted, 3 deleted; premise reshaped |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | ISSUES OPEN (via /autoplan) | 16 issues, 3 critical gaps — all encoded in the spec |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | SKIPPED | no UI scope (0 matches) |
| DX Review | `/plan-devex-review` | Developer experience gaps | 1 | CLEAR (scoped) | 1 finding: mock vectors poison a populated collection → R1.4 |
| Outside Voice | dual-voice per phase | Independent cross-model | 2 | ran | CEO 6/6 adverse, Eng 6/6 adverse, 0 disagreements |

**CROSS-MODEL:** Codex and the independent Claude subagent agreed on every dimension in both
phases (0 disagreements across 12). Three findings were reached independently by both voices *and*
the primary review: the Gmail query is the real handled-set bug; `.rN` rotation is a prompt rule not
enforcement; the plan optimises an artifact with a 0% delivery rate.

**VERDICT:** CEO CLEARED — reshaped and approved. ENG ISSUES OPEN by design: 16 findings including
3 P0s are recorded in the spec as ⚠ CORRECTED requirements rather than fixed in the plan text, so
the build works from corrected requirements. Both eng voices recommended R1+R2 only; **the operator
overrode at the final gate and chose full R1–R5 scope.** That override is recorded, not re-argued.

NO UNRESOLVED DECISIONS
