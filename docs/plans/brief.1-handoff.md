# brief.1 — next-session handoff

**`/review`, `/qa` and a `/ship` fix-review round are all DONE (2026-07-29). Pick up at the
`/ship` version bump.**

**The `/ship` round found NINE defects in the /review + /qa fixes**, five of them mutation-proven
false greens in guards written one round earlier. Two were security-relevant and both were mine: the
hex guard protected `thread_id`, the one field an attacker does not need (a subject line reading
`Payment overdue [Pay now](https://evil.example)` was copied into a table cell verbatim), and the
shrink ladder converted a loud fail-closed into a silent partial brief that dropped items by
model-assigned `urgency` — a judgement of sender-written text. Both fixed; `brief-03` re-rated
**P3 → P1**. Full account in `docs/plans/connectors-action-queue.md` under "Outcome at /ship".

**The pattern worth carrying forward: in this increment, every round's fixes were the next round's
defect source.** Three rounds, three times. Do not treat a fix as safe because it came from a
review — run the guards against mutations that reintroduce the original bug, and check the guard
fails. Seven such controls are recorded in the plan doc and all seven now bite.

**/qa found two more criticals, both about whether the brief gets written at all.** It drove a real
`agentd` against a fake provider, so the store and every guard were real. QA-1: /review sized the 8 KiB
budget against raw JSON, but `kb_put` embeds the brief as a JSON string inside a provenance wrapper
(~605 B, not the ~220 assumed) — real margin was **39 bytes**, now 1 255. QA-2: the caps said "chars"
and both stores count **bytes**, so CJK subject lines silently blew the limit and the operator got no
brief; caps are now in bytes and STEP 4 shrinks-and-retries instead of failing. Both proven on the real
store. Full report: `.gstack/qa-reports/qa-report-agentos-cos-2026-07-29.md`.

**What /qa could NOT verify:** whether the model obeys any of the prompt instructions (thread_id
emission, the hex link guard, the tolerant read, the slug rule, the retry ladder). That needs a real
model against real Gmail — no API key, no Docker, no OAuth token here, and faking Gmail would mean
disabling the broker's SSRF controls. **The first real brief is that test.** The CHANGELOG must not
claim otherwise.

## State

- **Branch:** `brief.1-action-list`. Not pushed. No PR.
- **`main`:** at `3a470cc5` = **v0.116.0** (ux.6a), merged, tagged, images published to
  `ghcr.io/0x89karan/runtime1:v0.116.0` for both arches. That increment is fully closed.
- **Reviewed and fixed:** `/review` ran 4 specialists + a Claude adversarial pass + two Codex
  passes. **9 criticals.** Codex returned `Reject` and two `[P1]`s. All fixes applied; the
  workspace gate is green. See "What /review changed" below.
- **Plan:** `docs/plans/connectors-action-queue.md` (titled brief.1; approved at the /autoplan
  final gate, then corrected at /review). **Design doc:**
  `~/.gstack/projects/0x89karan-runtime1/0x89karan-connectors-shaping-design-20260729-062000.md`.

## What /review changed — read this before anything else

**The increment's stated premise was wrong.** brief.1 was built on "the brief re-lists handled
items because the curator keys them `open:{date}:{N}`". Nothing reads those keys: `kb_search` is
single-segment and there is no list/scan tool, so `open:*` is write-only by construction. The
re-listing comes from `kb_search(segment='ops:briefs', …)` returning whole historical brief JSONs.
Five of six independent passes reached this conclusion.

The user's call at the /review gate: **ship the instrument, de-claim the fix.** So:
- Criterion 1 (handled items stop appearing) is **OPEN**, filed as **brief.2** in `TODOS.md`, and
  the false causal claim is deleted from the plan, both configs, and this doc.
- Criterion 2 (one click to the thread) is **met on the markdown brief**, not on Telegram
  (`brief-02`).
- Criterion 3 (the one-week tally) is unaffected — the instrument lands, which was the point.

Defects found and fixed, all in the same 30 lines:
1. **The brief was already over its own cap.** Measured 8 660 B at the prompt's documented maxima
   against a hard 8 192 B limit → the inbox write fails, the curator finds no input and stops, and
   there is **no brief that morning**. Now `important ≤8` plus explicit bounds on `from`,
   open-item text, and `focus_recommendation`: 7 548 B, 644 B headroom.
2. **Three copies shipped stale**, including `distro/overlay/etc/agentd/cos.agents.toml` — the
   QEMU production config. All four sources are now mirrored and pinned by a test.
3. **The wrapped `kb_put(` this increment introduced blinded two existing guards** (they scanned
   one line at a time; mutation-proven). The scanner now joins continuation lines, with a negative
   control test so it cannot go blind again.
4. `if thread_id is present` was wrong against `thread_id: null` — every non-thread item would
   collide on a single `open:None` key.
5. `stable_slug_of_item_text` was undefined; now an exact rule inside the store's 128-char and
   charset limits, with an explicit "skip and report" path for a rejected write.
6. A model-supplied `thread_id` went unvalidated into a markdown href in a document the operator
   clicks. Links now require `^[0-9a-f]{1,20}$`.
7. No tolerant read for the old bare-string `open_items` shape; combined with "ALWAYS include
   thread_id" that invited a fabricated id — a valid-looking link to the wrong thread.

New tests (all three negative-control verified by mutation):
`cos_prompts_key_open_items_by_thread_not_date`, `cos_prompts_use_content_not_value_for_kb_put`,
`cos_prompts_never_interpolate_unvalidated_thread_id_into_a_link`, plus
`kb_call_scanner_sees_wrapped_calls` as the scanner's own guard. They run over **all four** prompt
sources via `COS_PROMPT_SOURCES` — the old guards covered two.

Also fixed while mirroring: `templates/cos-curator.template.toml` used `value=` in all three
`kb_put` calls — the bug fixed in the configs at v0.77.0 and guarded ever since, except the guard
was scoped to two files, so that shipped template's KB writes have persisted nothing for ~40
releases. The new guard covers the templates.

## What brief.1 is, in three sentences

The morning brief already emitted `## Response Needed` and `## Open Items (carried forward)`, and
the curator already persisted carried-forward items — but it keyed them `open:{date}:{N}`.
`ops:entities` is **scratch-class**, so a *stable* key would have overwritten itself; a date-plus-
ordinal key instead minted a fresh entry every morning, so every unresolved item reappeared
forever. brief.1 keys them by **Gmail `threadId`** and adds a thread permalink so each row is one
click from the thread.

## Do next, in order

1. ~~`/review`~~ — **DONE**, see above.
2. ~~`/qa`~~ — **DONE.** The rig is reusable: a fake `/v1/messages` returning scripted `tool_use`
   blocks plus a minimal config declaring `ops:entities` as scratch drives the real native `kb_put`
   path end to end. Files in the session scratchpad under `qa/`; the pattern is worth rebuilding for
   any future KB-shape change. Two gotchas cost time: a `curl` smoke-test against the fake server
   consumes script step 0 and silently shifts every step, and `streaming` defaults to **true** so the
   fake must either serve SSE or the config must set `streaming = false`.
3. **`/ship`** — version bump is `agentd/Cargo.toml` + the test-enforced `CLAUDE.md`
   "Current version" line + `CHANGELOG.md`, guarded by `agentd/tests/repo_consistency.rs`.
   Next version is **v0.117.0**. There is no `VERSION` file and no `package.json`, so
   gstack's `gstack-version-bump` CLI does **not** apply here. The CHANGELOG entry must say
   criterion 1 is unmet and point at `brief.2` — do not let it claim the de-dup works.

## Review targets — all resolved, recorded here so /qa does not re-derive them

1. **`open_items` shape change (array of strings → `{text, thread_id}`).** Every producer and
   consumer in the repo has now been enumerated: the two `cos.agents.toml` files and the two
   `templates/cos-*.template.toml` files. All four are mirrored. Nothing in Rust or Python reads
   `open_items` — it exists only inside prompts and inside stored brief JSON. Historical stored
   briefs are old-shape, which is why the curator now has an explicit tolerant-read instruction.
2. **`threadId` reaches the inbox agent — yes, confirmed.** `messages.list` returns it beside every
   `id`, `messages.get?format=metadata` returns it top-level, `oauth_call_api` passes the raw body
   through, and the broker's `passthrough_query_params` has no `fields` entry to strip it. One real
   gap was found: the `mail:raw` dedup-hit path skips the per-message `get` and the cached line
   carried no `threadId`. Fixed — it is now the first field of the cached line.
3. **The slug fallback was NOT deterministic** and could also produce keys the store rejects. Now
   an exact rule, renamed `open:nothread:{slug}` (the old `open:x:` sentinel was undocumented).
   Honest caveat: the slug's *input* is a fresh LLM paraphrase each morning, so cross-day stability
   for non-thread items is still best-effort. It costs nothing today because nothing reads it.
4. **Permalink form: `#all/` confirmed correct.** `/u/0/` is retained but is the browser's *first
   signed-in* Google account, not an account identity — an operator with several Google accounts
   gets the wrong mailbox. Now stated in the prompt instead of implied.

## Verified facts — do NOT re-derive these, and do not assume otherwise

These cost real effort to establish this session and three of them **inverted** an earlier
recommendation. All are code-checked.

- **`tool_override = true` on semantic-kb** (`cos.agents.toml:183`) means **every**
  `kb_put`/`kb_search`/`kb_get` from the CoS lands in **Qdrant**, not `memory.redb`. The config
  says so itself at `:172`. FUSE `kb/`, the TUI `[m]` Memory view, and `GET /api/v1/memory/:ns`
  all read **L1 redb**, and Qdrant is on `agent-net` and not host-exposed. **So KB-resident state
  is invisible to the operator in the shipped deployment.** This is already true of `ops:briefs`
  and `ops:entities` today and nobody noticed — which is itself evidence about how much those
  surfaces are used.
- **`ops:entities` is `class = "scratch"`** (`:154-155`) → caller keys dedupe (last-writer-wins,
  version bump). **`ops:briefs` is `class = "log"`** (`:149-150`) → the L1 store **ignores** the
  caller key and assigns a monotonic seq. Never key-dedupe against a log-class segment.
- **`BriefRecord` is already structured and additively extensible.** It carries
  `items: Vec<BriefItem>`, and `runs/mod.rs:28-29` mandates `#[serde(default)]` on all post-v1
  fields so an old `runs.redb` still deserializes. So adding an `actions` vector needs **no new
  table and no `format_version` bump** — the `audit86-P3-4` objection does not apply here. If
  brief.1's tally justifies a runtime-owned queue later, **this is the landing zone**, and it is
  cheaper than the earlier plan costed it.
- **The CoS is read-only by scope**: `GOOGLE_SCOPES = "gmail.readonly drive.readonly"`
  (`docker/oauth_mcp.py:62`), no Calendar. And `oauth_mcp.py` is **not** a Gmail connector — it is
  a generic OAuth2+PKCE REST client with exactly three tools (`oauth_start_auth`,
  `oauth_check_auth`, `oauth_call_api`) and env-overridable scopes. So "add a connector" is
  scopes + endpoints + a capability grant, **not** a new sidecar. Never estimate connector work as
  N sidecars.
- **The operator cannot write to KB at all.** `/api/v1/memory/:ns` is GET-only; FUSE `write()`
  returns `EROFS` on every inode except `INO_CONTROL`; `kb_put` is an agent tool gated by
  `Capability::KbWrite { segment }`. Any operator-initiated mutation must route through the model
  via `inject`.
- **`test-flake-01` reproduces on `main`** (2 of 8 whole-suite runs, `scheduler::tests::streaming_*`).
  `--test-threads=1` is 100% green. Do not attribute it to a diff.
- ~~**Five `agentctl` tests need a live Docker daemon**~~ **CORRECTED at /ship (2026-07-29): they do
  NOT need a daemon.** `docker::tests` installs a fake `docker` shell script on `PATH`
  (`with_fake_docker`), so the daemon is irrelevant — verified failing with Docker Desktop DOWN and
  also *passing* with it DOWN. The real mechanism is machine load against a 3-second deadline:
  `PROBE_TIMEOUT` (`agentctl/src/docker.rs:192`) bounds a probe that spawns `/bin/sh` and drains its
  pipe; under contention it times out, `detect_docker_context()` returns `None`, and the test's
  `.expect(…)` panics. Folded into `test-flake-01` in `TODOS.md` with the fix options. Do not
  attribute these to a diff, and do not attribute them to Docker.

## The `/qa` caveat — read before planning it

This is a **prompt** change, so no unit test can validate it. Real verification means driving an
actual brief cycle and checking that (a) `threadId` survives the inbox → curator hop, (b) an item
handled yesterday is absent today, and (c) the links resolve. That needs the CoS against real
Gmail. **The ux.6a QA rig pattern will not help** — a fake `/v1/messages` cannot produce real
Gmail thread IDs. Be honest in the QA report about what was and wasn't verified rather than
inventing a green.

**The real QA is the one-week tally** (below).

## The unmeasured premise — the most important thing in this handoff

brief.1 does **not** fix the demand-evidence gap. The load-bearing claim behind this whole track —
*"more capability beats more observability"* — came from the ux.6a roadmap review, **not from
observation**. Both /autoplan CEO voices flagged this and both said the same thing: ship the
instrument, then measure. /review reinforced it: the increment now ships as *an instrument plus
seven defect fixes*, with its headline claim withdrawn — so the tally is the only thing that can
justify continuing this track.

**The assignment for the operator:** for one week, after each morning brief, tally every action
then done by hand — type and count. With thread IDs now present, this is a diff between what the
brief listed and what actually happened.

**If the tally is ~2 actions a morning, build nothing further on this track.** The right answer is
to read the inbox. Do not let a future session quietly skip this and jump to the runtime queue.

## Still open, and both CEO voices ranked all three ABOVE this track

1. **Name mv design partners, or strike mv.** External gate is **2026-10-01** (~9 weeks), needs
   **10 named humans and 3 booked demos**, has zero of each, and no `mv.*` increment is scheduled.
   Zero engineering. The design doc's own warning applies: "an unnamed gate date is how 'deferred'
   becomes 'never'" — the date is now named and nothing is scheduled against it.
2. **`p7.7-ar-03`** (~half a day) — `HttpSource::load_snapshot()` hardcodes `egress_brokered` /
   `egress_rejected` to 0 while FUSE reads them live. Since ux.6a made denials real, the cockpit
   now reports a **false `0 denied`** for a governance signal in HTTP mode. Same defect class
   ux.6a existed to remove.
3. **`audit-S3`** (P1) — no `SecretRewriter` exists, so tool output reaches the model unscrubbed.
   The claim-vs-reality half is already de-claimed in the docs, so this is a *missing defense*,
   not a live lie. Note `CLAUDE.md` still says `audit86-P1-9` is "the only live P1", which is
   stale — `audit-S3` is un-struck at `TODOS.md:864`. Also open: `audit86-P1-9` is a ~20-minute
   scope decision, not an increment.

## Standing constraints (non-negotiable)

- **Pipeline order, each an explicit skill call, no substitutes:** `/autoplan` → build →
  `/review` → `/qa` → `/ship` → `/land-and-deploy`.
- **Never push a `v*` tag without explicit instruction.** Tags are a manual release gate.
- **The user gates all merges.** Create PRs; do not merge unprompted.
- **Never run `cargo fmt`.**
- **Quality gate is workspace-wide from the repo root:**
  `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
- One increment per branch; `main` stays shippable.
- Secrets come from the environment only — never logged, never written to disk.
