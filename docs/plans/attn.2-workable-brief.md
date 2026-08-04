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

---

## Phase 1 CEO Review — R3/R4/R5 (2026-08-04, /autoplan)

**Scope:** R3, R4, R5 only. R1/R2 shipped v0.119.0, not re-opened.

### Dual voices

**Codex** (bottom line): build R4 now, split R3, do NOT build R5 as specified before attn.4.
R5's sentinel is "manual schedule hint," not manual fire — it is read inside
`handle_wait_for_trigger`, which only runs when the trigger agent is alive and polling. The plan's
own text admits `touch` is a silent no-op if the trigger is failed/deferred/cancelled — exactly
attn.4's failure mode. The plan's own line 299 already names the alternative ("make the fire a
control-API verb") and drops it in one clause. Also: R4's "no content lost" is false for the KB
side — `kb_put(segment='ops:briefs', key='{date}', ...)` has no `{ts}`, confirmed at
`cos.agents.toml:481`; only the file gets timestamped.

**Claude subagent** (independent, no prior-phase context): same conclusion via different evidence.
Found `~/.agentos-output/.fire-now` already exists on disk, **0 bytes, 3 days stale** (verified:
`mtime=Aug 1 11:29:13`) — AC14 (unlink-without-firing at init) is not a nicety, it is the
difference between R5 shipping safely and firing an unrequested cycle the instant it deploys.
Found a NEW gap neither the plan nor Codex named: a manually-fired child is not a `config_seed`
row, so `publish_brief`'s stats aggregation (`runs/store.rs:489-504`) folds it into
`run_count`/`spend_total` — the exact corruption `attn.1a-04` (TODOS.md:429) already named for the
*future* attn.1b loop, reintroduced NOW by R5. Also found `resultSizeEstimate` on Gmail's existing
`messages.list` response already gives R3.4's suppressed-count for free — no second call needed.
Also found `TRIGGER_MAX_WAIT_S` is ALREADY a live, unfixed instance of the exact env-var-parity bug
R5 must not repeat (`passenv`'d at `cos.agents.toml:194`, absent from `docker-compose.yml`).

All four decisive claims (KB key has no `{ts}`, `.fire-now` stale on disk, `TRIGGER_MAX_WAIT_S`
missing from compose, the `reset-attention`/`inject` route precedent) were independently
re-verified against the code before this gate.

### CEO DUAL VOICES — CONSENSUS TABLE

```
  Dimension                            Claude    Codex    Consensus
  ──────────────────────────────────── ───────── ───────── ─────────
  1. Premises valid?                   R3/R4 yes R3/R4 yes CONFIRMED (R3/R4)
                                        R5 no     R5 no     CONFIRMED (R5 premise weak)
  2. Right problem to solve?           yes       yes       CONFIRMED
  3. Scope calibration correct?        R5 over-  R5 wrong  CONFIRMED — R5 needs reshaping
                                        built     mechanism
  4. Alternatives explored?            no (D1)   no (API   CONFIRMED — both name the SAME
                                                  verb)     missed alternative independently
  5. 6-month regret?                   R5 medium R5        CONFIRMED
                                                  foolish
```

**Both models independently converged on the same architectural fix for R5** (sentinel →
management-API verb), via different evidence paths. This is a **USER CHALLENGE**: both models
agree the user's stated direction (build R3+R4+R5 now, as written) should change for R5
specifically.

### Scope decisions (cherry-picks, presented to operator — not auto-decided)

- **D1** — R5: filesystem sentinel (as written) vs. management-API verb (`management.rs` already
  has 3 precedent routes: `inject`, `cancel`, `reset-attention` — all unauthenticated-but-
  network-scoped, same threat posture R5 needs). API verb removes 4 edge-case surfaces the sentinel
  carries (stale-at-init, catch-up race, lossy unlink-then-touch, 3-site env-var parity) at the cost
  of requiring network reachability to :7999 instead of a bare file touch.
- **D2** — R3.4: derive `suppressed_count` from Gmail's existing `resultSizeEstimate` (free, zero
  new broker requests, moots the `attn.1a-01` interaction entirely) vs. the plan's original second
  `-in:inbox` call.
- **New finding, not in original plan** — R5 (either shape) needs a `config_seed`-style stats
  exclusion or an explicit RUNBOOK note accepting `run_count`/`spend_total` inflation.
- **New finding** — R3.2's null-`thread_id` dedup case has no specified matching rule.

### Completion Summary

Both CEO voices ran to completion; 4 decisive claims independently re-verified against the code.
No disagreement between Codex and the Claude subagent on any dimension. Recommendation: **Approach
C** — ship R3+R4 now (zero shared surface with attn.4, and R4 is an active data-loss bug that has
already fired twice in 3 days of real production data), reshape R5 per D1 before building it.

---

## R5 REDESIGNED — management-API verb (accepted D1, replaces the sentinel design above)

**Gate decision (2026-08-04):** Approach C. R3+R4 ship as specified (R3.4 uses D2). R5 is rebuilt
as an HTTP verb on the existing management API, not a filesystem sentinel.

### Architecture, verified against the code (not assumed)

`dispatch_run_job` (`scheduler.rs:2510`) is the function a real `run_job` tool-use call reaches.
Traced its dependency on a live parent:

- **Capability check** uses `parent_cap_set`, an argument — no live-agent lookup required.
- **Success path** (job lookup, depth limit, child-id derivation, child spawn) degrades
  gracefully for a parent that doesn't exist: `state.spawn_depths.get(&parent_id)...unwrap_or(0)`,
  `state.agents.get(&parent_id).map(...).unwrap_or_default()` for model_cfg. Verified at
  `scheduler.rs:2570-2622`.
- **Completion delivery** (`scheduler.rs:1368-1412`, where a finished child looks up its
  `AwaitingParent`) computes `parent_live = state.agents.contains_key(&parent_id) && ...` and
  **cleanly no-ops delivery if false** — it still records `AgentChildResultDelivered`, it just
  skips re-stepping. Verified at `scheduler.rs:1408-1412`.

**Conclusion: the API route must use a synthetic parent id that is NEVER a real `AgentTask`** —
e.g. `"manual-fire"`, not `"cos-orchestrator"`. Reusing the real trigger's id would risk injecting
a `call_id`-labeled `tool_result` into the trigger's own transcript with no matching `tool_use` if
the trigger happens to be live and parked at the moment of the API call — the same malformed-
transcript risk class attn.3 (v0.120.0) just fixed on the checkpoint side. A synthetic id that is
never a live agent makes `parent_live` always false, so delivery is a clean no-op that only
records the flight event — no transcript is ever touched.

### R5 acceptance criteria (revised — replaces the sentinel-shaped ACs 13-17 above)

- **New route:** `POST /api/v1/cos/fire` (or similar — Eng review names it), same
  unauthenticated-but-network-scoped posture as `/api/v1/agents/:id/inject` and
  `/api/v1/credentials/:provider/reset-attention`.
- Calls `dispatch_run_job` (or a thin wrapper) with `parent_id = "manual-fire"` (a label, never a
  real agent), `parent_cap_set = Some(vec![Capability::RunJob])` (config-derived, matching what
  cap.2b already grants the real trigger — no widening), `job_id` fixed to `"cos-inbox"` (the
  curator fires itself via the existing inbox→curator handoff, OR the route fires both in sequence
  — Eng review decides).
- **Collision guard already exists and is reused, not rebuilt**: step 5's
  `state.agents.contains_key(&child_id) || state.outcomes.contains_key(&child_id)` check is
  identical whether the caller was the LLM trigger or this new route — a manual fire racing a
  scheduled fire on the same date collides exactly like two scheduled fires would.
- **Stats exclusion (new finding from Phase 1, not in the original R5 scope at all):** a
  manually-fired child must not corrupt `run_count`/`spend_total` the way `attn.1a-04`
  (TODOS.md:429) already names for the future attn.1b loop. `run_tracker.open()` at
  `scheduler.rs:2638` needs a `start_reason` distinct from `"run_job"` for this path — e.g.
  `"manual_fire"` — so `publish_brief`'s stats aggregation (`runs/store.rs:489-504`, the same
  `!= "config_seed"` filter pattern) can exclude it the same way.
- **No filesystem sentinel, no new capability, no new env var, no 3-site config-parity
  requirement** — this removes AC13-17 (stale-init, catch-up race, lossy unlink, env-var parity)
  entirely. They do not apply to an HTTP route.
- **New observability event** (Phase 1 Section 8 finding): `ManualFireTriggered{child_id, source}`
  recorded at the route handler, before dispatch — so a brief with an unusual `run_count` is
  traceable to "someone hit the endpoint on this date," not silent.

### R3.4 revised (D2 accepted)

`suppressed_count` is derived from Gmail's `resultSizeEstimate` field on the STEP 2
`messages.list` response already being fetched — **not** a second `-in:inbox` call. Zero new
broker requests; moots the `attn.1a-01` interaction the original R3.4 raised.

### Open questions for Phase 3 (Eng review) — do not resolve these in CEO synthesis

1. Exact route path and method naming (`/api/v1/cos/fire` vs. matching `/api/v1/agents/...`
   conventions elsewhere).
2. Does the route dispatch `cos-inbox` only (letting the existing inbox→curator handoff carry
   the rest) or both jobs explicitly? The real trigger's prompt calls them sequentially with a
   completion-signal wait in between — does a single HTTP call need to replicate that wait, or
   fire-and-forget both?
3. `start_reason = "manual_fire"` — confirm `run_tracker.open()`'s signature accepts an arbitrary
   string here today, or whether it needs a typed enum change.
4. R3.2's null-`thread_id` dedup matching rule (Phase 1 Section 4 gap, still unresolved).

---

## Phase 3 Eng Review — R3/R4/R5-redesigned (2026-08-04, /autoplan)

### Cross-model coverage gap (not a disagreement of opinion — one voice missed a P0 the other found)

**Codex found, independent Claude subagent did NOT flag:** `dispatch_run_job`'s `reject()` helper
(`scheduler.rs:2534-2545`) does `state.agents[&parent_id].priority()`, `.cap_set_cloned()`, and
`state.agents.get_mut(&parent_id).unwrap()` — three separate `HashMap` index/unwrap sites, all of
which **panic on a missing key**. Every rejection branch (capability denied, unknown job_id, depth
exceeded, child-id collision) calls `reject()`. With `parent_id = "manual-fire"` — a synthetic id
the CEO gate's design deliberately never inserts into `state.agents` — **any rejection panics the
whole process**. On `panic = "abort"`, PID 1: a same-day collision (the single most likely
real-world trigger — firing the endpoint twice) crashes agentd entirely.

**Independently re-verified against the code** (not taken on trust): confirmed at
`scheduler.rs:2534` — `let priority = state.agents[&parent_id].priority();` — `Index`, not `.get()`.

This means `dispatch_run_job` **cannot be called as-is** with a synthetic parent. It needs an
internal fix mirroring the pattern `handle_agent_terminal` already uses for delivery
(`parent_live` check, skip the touch if false) — applied to `reject()`, not just to completion
delivery.

### Independent Claude eng subagent — three additional silent-failure gaps, none flagged by the
### CEO gate or by Codex

1. **`child_model_cfg` fallback** (`scheduler.rs:2626-2630`) derives from
   `state.agents.get(&parent_id)...unwrap_or_default()`. For a synthetic parent this silently
   returns `ModelConfig::default()` (`max_tokens: 4096`) instead of the configured `8192` — every
   existing `run_job` caller is a real, live agent, so this fallback arm has **never executed in
   production**; the manual-fire route would be the first caller that ever hits it. Silent, no
   error, would look like an unrelated truncation flake.
2. **`is_mutating_route`** (`management.rs:139-152`) is a hand-enumerated match, not a blanket
   rule. A new route not added to it is **unauthenticated even when `AGENTOS_APPROVAL_SECRET` is
   configured** — strictly weaker than the `inject`/`reset-attention` precedents it's meant to
   match, by omission, not design. `cap.4`/AUDIT-v0.97 P2-3 fixed this exact hole once already for
   the whole mutating surface.
3. **Stats exclusion doesn't exist as a reusable pattern.** `runs/store.rs:489-495`: the
   `start_reason != "config_seed"` filter is **only inside the `still_running` arm**.
   `terminal_in_window` (the branch a manually-fired job's *completion* actually matches) has **no
   `start_reason` filter at all**. The CEO gate's "reuse the config_seed pattern" doesn't cover
   this branch — it's new code at a different branch, not reuse.

**Also resolved definitively** (open question #2 from the CEO gate): there is **no automatic
inbox→curator handoff** anywhere in Rust. `handle_agent_terminal` skips delivery unconditionally
when the parent isn't live; the real trigger's own prompt (STEP 2/3) is the *only* mechanism that
currently sequences the two jobs. **A route firing `cos-inbox` only will never produce a brief.**
Recommended design: `job_id` as an explicit request parameter (fire one job per call), not
auto-chaining — auto-chaining via an SSE subscriber is possible but does not survive an `agentd`
restart (new gap: no checkpoint, no restart hook, silently drops the second job).

**Also found:** R3.4/D2 still needs real (small) Rust work — `resultSizeEstimate` reaching the
model is sound, but getting it into an *auditable flight event* (not just brief markdown) needs a
`PublishBrief` schema extension (`BriefRecord`, `#[serde(default)]`, same shape as
`attention_overflow`'s existing addition) — this was understated as "zero Rust work" in the CEO
synthesis. R4.3's doc citations are miscited (wrong line numbers in 2 of 3 sites,
`docs/DEPLOYMENT.md:499`'s `tail -f` missed entirely, `architecture-diagram.html:337`'s node label
not checked). R3.2's null-`thread_id` dedup now has a concrete rule: never fuzzy-match null-
`thread_id` items against anything — treat null as "unknown identity," exact `thread_id` match
only.

### ENG DUAL VOICES — CONSENSUS TABLE

```
  Dimension                            Codex      Claude subagent   Consensus
  ──────────────────────────────────── ────────── ───────────────── ─────────
  1. Architecture sound as specified?  NO (panic) NO (3 silent gaps) CONFIRMED not sound as written
  2. Test coverage sufficient?         n/a        11 gaps, 10 tests  Claude subagent only — no disagreement
  3. Security threats covered?         NO (auth   NO (same finding, CONFIRMED — same finding, independent
                                        allowlist) independent)      discovery
  4. Silent-failure risk?              1 P0        3 P0/P1           CONFIRMED — 4 total, additive not overlapping
  5. Sequencing (job chaining)?        NO auto-    NO auto-chain,    CONFIRMED — same conclusion, same evidence
                                        chain       same evidence
  6. Deployment risk manageable?       —           reversible, 4/5   Claude subagent only
```

**Single-voice critical findings, flagged per the skill's own rule** (a single voice finding a
CRITICAL still counts, regardless of the other voice's silence): the `reject()` panic (Codex only)
and the `child_model_cfg` fallback / stats-exclusion-branch gap (Claude subagent only) are BOTH
real, BOTH independently re-verified against the code in this session, and NEITHER is contradicted
by the other voice — this is additive coverage, not disagreement.

### Completion Summary

R5 as redesigned is not a "thin API route." It needs: a `dispatch_run_job` safety fix (parent-
live-aware `reject()`), an explicit default-model parameter (not the `unwrap_or_default()`
fallback), a new `ControlCommand` variant (management.rs has no direct scheduler-state access —
confirmed, it only owns `control_tx`), an `is_mutating_route` allowlist entry, a `start_reason`
parameter threaded through `dispatch_run_job`, and a new stats-exclusion filter on
`terminal_in_window`. This is a real engineering scope, not a config/prompt edit — the plan's own
effort table (~40min CC) is now stale for R5 specifically.

---

## Decision Audit Trail

| # | Phase | Decision | Class | Principle | Rationale |
|---|-------|----------|-------|-----------|-----------|
| 1 | CEO | Run R3/R4/R5 through /autoplan as one combined review | Mechanical | P6 bias to action | Plan doc already treats them as one cohesive spec with a stated cross-dependency (R4 precedes R5) |
| 2 | CEO | **Approach C: R3+R4 now, redesign R5** | **USER CHALLENGE** | — | Both voices independently converged: the sentinel is a silent no-op exactly when a manual fire would be needed (budget-deferred trigger) |
| 3 | CEO | **D1: R5 as a management-API verb, not a sentinel** | **USER CHALLENGE** | — | Both voices independently named the same alternative; removes 4 sentinel-specific edge-case surfaces |
| 4 | CEO | D2: suppressed_count via resultSizeEstimate | Mechanical | P5 explicit | Free (already-fetched response field), zero new broker requests, moots the attn.1a-01 interaction |
| 5 | Eng | R5's synthetic-parent design needs a dispatch_run_job safety fix before ANY caller can use it | Mechanical | — | Codex-verified: reject() panics on a missing HashMap key; every rejection path hits this with a synthetic parent |
| 6 | Eng | R3.4/D2 needs a small PublishBrief schema addition, not zero Rust work | Mechanical | P1 completeness | resultSizeEstimate reaching the model doesn't produce a Rust-auditable flight event on its own |
| 7 | Final | **Ship R3+R4 this cycle; file R5 as its own increment (attn.2-R5)** | **OPERATOR GATE** | — | R5 grew from "thin route" to real engineering (4 silent-failure bugs, one a PID-1 crash risk) — same split pattern as attn.3/attn.4 earlier today |

## GSTACK REVIEW REPORT

- **Verdict:** APPROVED as scoped. R3+R4 ready to build. R5 REDESIGNED (API verb) is
  fully spec'd but deferred to its own increment — not ready to build without further
  scoping of the 4 findings below.
- **Voices:** CEO codex + 1 subagent (0 unavailable); Eng codex + 1 subagent (0 unavailable).
  Zero disagreement between voices on any dimension across both phases — all findings additive.
- **User Challenges, both resolved:** (1) R5-as-sentinel → R5-as-API-verb (D1); (2) ship
  R3+R4 now despite attn.4 being unbuilt, since neither shares any surface with it.
- **Ships now:** R3 (D2 variant) + R4, as specified in this document's R3/R4 sections plus
  the corrections logged above (KB key `{ts}`, corrected doc citations, small PublishBrief
  schema addition for suppressed_count).
- **Does NOT ship this increment:** R5. Filed as `attn.2-R5` in TODOS.md with its four
  concrete requirements (dispatch_run_job parent-live-aware reject(), explicit default-model
  param, new ControlCommand variant, is_mutating_route entry, start_reason threading +
  terminal_in_window stats filter).
