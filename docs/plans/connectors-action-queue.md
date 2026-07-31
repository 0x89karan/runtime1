<!-- /autoplan restore point: ~/.gstack/projects/0x89karan-runtime1/connectors-autoplan-restore-20260729-155848.md -->
# brief.1 — make the brief's action list actually usable

**Status:** RESHAPED at the /autoplan premise gate (2026-07-29). Both CEO voices returned RESHAPE.
**Was:** "action-queue.1 — the morning action queue", itself reframed from the unwritten
"Connectors" track. Design doc:
`~/.gstack/projects/0x89karan-runtime1/0x89karan-connectors-shaping-design-20260729-062000.md`

---

## What the reviews found, and why the scope collapsed

The plan proposed a KB-resident action queue with provider-native identity and derived
completion. Verification killed three of its load-bearing claims:

**1. ~95% of it already ships.** `cos.agents.toml:427` already writes carried-forward items —
`kb_put(segment='ops:entities', key='open:{date}:{N}', content=<item text>)` — and the brief
already ends with `## Response Needed` (From | Subject | Ask | Deadline, `:442`) and
`## Open Items (carried forward)` (`:447`), delivered to `~/.agentos-output/brief-{date}.md`.

It fails for exactly two prompt-level reasons:
- **`open:{date}:{N}` re-keys every morning.** Date plus a model-chosen ordinal is not
  provider-native, so an unresolved item gets a fresh key daily and `ops:entities` accumulates a
  new entry per item per day forever.
  > **CORRECTED at /review (2026-07-29).** This was filed as the cause of the brief re-listing
  > handled items. **It is not.** Nothing reads those keys — `kb_search` is scoped to a single
  > segment and there is no list/scan/prefix tool, so `open:*` is write-only by construction. The
  > re-listing is produced by the curator's `kb_search(segment='ops:briefs', …)`, which returns
  > whole historical brief JSONs, each holding that day's entire `open_items` array — and nothing
  > ever removes an entry for a resolved item. Re-keying is still worth doing (it stops the
  > unbounded accumulation and makes each item addressable) but it does **not** satisfy success
  > criterion 1. See `brief.2` in `TODOS.md`.
- **No thread reference.** The Response Needed table has no thread ID and no permalink, so you
  read the list and then hunt the thread by hand.

**2. The previous draft's chosen storage is invisible in the deployment that runs.**
`cos.agents.toml:183` sets `tool_override = true` on the semantic-kb sidecar, and the config's own
comment at `:172` states *"ALL kb_put/kb_search/kb_get calls route to this sidecar (Qdrant…)"*.
FUSE `kb/`, the TUI `[m]` view, and `GET /api/v1/memory/:ns` all read the **L1 redb** store;
Qdrant is on `agent-net` and not host-exposed. So "read visibility is free on three existing
surfaces" was worth **zero**, and the open question "does `[m]` render a queue legibly?" was moot —
it would render nothing.

**3. The stated reason for rejecting a runtime-owned table was invalid.** `runs/mod.rs:28-29`
mandates *"All fields added after v1 must be `#[serde(default)]` so an old `runs.redb` still
deserializes (E5)"*, and `BriefRecord` already carries `items: Vec<BriefItem>`. Additive
extension needs no new table and no `format_version` bump, so the `audit86-P3-4` objection did
not apply. Recorded because it means the runtime landing zone is **cheaper than costed** while the
KB landing zone is **worth less than costed** — that inversion is why the scope collapsed.

## Scope — three prompt edits, one config file

`agentd/cos.agents.toml` only. No Rust, no new surface, no schema change.

1. **Inbox agent: capture and pass through `threadId`** per actionable thread. Gmail's
   `users.messages.list` returns `threadId` per message, so the data is already available through
   `oauth_call_api`; the prompt simply never asks for it. Today only
   `{thread_count_reviewed}` (a count) survives into the brief.
2. **Curator: key open items by thread ID** — `open:{threadId}` instead of `open:{date}:{N}`.
   Enabling fact, verified: `ops:entities` is already `class = "scratch"`
   (`cos.agents.toml:154-155`), which requires a caller key and does last-writer-wins with a
   version bump. So keying dedupes correctly. (The log-class duplicate hazard applies to
   `ops:briefs` at `:149-150`, not here.)
3. **Brief template: add a thread permalink column** to the existing Response Needed table —
   `https://mail.google.com/mail/u/0/#inbox/{threadId}` — so each row is one click from the thread.

## Explicitly NOT in scope

No new KB segment. No `ActionItem` vector on `BriefRecord`. No runtime table. No control verbs.
No operator write path. No approval gate. No `gmail.send`. No Calendar/Linear/Slack/Notion/GitHub
scopes. No item-type registry, executor abstraction, or per-provider derivation layer — the
operator's *"I will keep expanding this scope till it covers my full workflows"* is the
platform-vision pattern, and the correct response is a narrower increment, not a framework.
**Hard-code email.**

## Premises (ratified at the gate)

- **P1** The unit of work is an action list the operator walks, not a set of connectors.
- **P2** Item identity must be provider-native. This is no longer an aspiration — it is *the bug*.
- **P4** Read-only first; execution is a later increment.
- **P5 (amended)** Completion is derived from source-of-truth: the reply is in the thread, so the
  item is absent from tomorrow's list. With thread-ID keying this needs no stored state at all,
  which is why the storage argument became moot.
- **P3 deferred, not struck.** Every action type the operator named is a write, so the approval
  gate is on the critical path for all of them — but nothing in this increment writes.

## The premise still unmeasured, stated plainly

**Demand evidence is weak and this increment does not fix that.** The load-bearing claim — *"more
capability beats more observability"* — came from the ux.6a roadmap review, not from observation.
Success criterion 3 below is unfalsifiable without a baseline, and the one-week tally *is* the
baseline. This increment is justified as a **defect fix plus a better instrument**, not as a bet
on demand: with thread IDs present, the tally becomes a diff between what the brief listed and
what you actually did, which beats a notebook.

**If the tally comes back at ~2 actions a morning, build nothing further.** Read the inbox.

## Open questions — all three answered at /review (2026-07-29)

1. **Does `oauth_call_api`'s Gmail response surface `threadId`?** **Yes.** `messages.list` returns
   `threadId` beside every `id`, `messages.get?format=metadata` returns it at top level,
   `oauth_call_api` passes the raw body through, and the broker's `passthrough_query_params` has no
   `fields` entry, so nothing strips it. No explicit field list needed. One gap found and fixed:
   the `mail:raw` dedup-hit path skips the per-message `get`, and the cached line carried no
   `threadId` — it does now.
2. **Do legacy `open:{date}:{N}` entries need a clear?** **No, accepted explicitly.** They are
   unreachable (nothing reads `open:*` at all) and cost only storage. Under `tool_override` they
   sit in Qdrant, whose only reclamation is a 30-day TTL sweep that runs **once at sidecar process
   start** — `SEMANTIC_MAX_ENTRIES` is a documented no-op — so the real purge condition is "the
   next sidecar restart ≥30 days after the last legacy write". They cannot double-list, because
   they were never listed.
3. **`#inbox/` or `#all/`?** **`#all/`**, as built: a thread you finished replying to gets
   archived, and `#inbox/` would then 404. `/u/0/` is retained but is a genuine weak spot — it is
   the browser's *first signed-in* Google account, not an account identity, so an operator with
   several Google accounts gets the wrong mailbox. Now stated in the prompt rather than implied.

## Non-negotiables

- **Item keys must be provider-native.** A model-generated key cannot survive re-derivation and
  is the defect being fixed.
- **`ops:entities` must stay `scratch`-class.** Log-class ignores the caller key and auto-assigns
  a monotonic seq, which would guarantee duplicates and reintroduce the bug.
- **No provider writes.** The moment one appears the approval gate is required and this is a
  different increment.
- **Do not touch `ops:briefs`.** It is log-class and its append semantics are already a documented
  accepted tradeoff under `tool_override`.

## Success criteria

1. An item the operator handled does **not** appear in the next morning's brief.
2. Every Response Needed row is one click from its thread.
3. One week of tallies exists, against which the next increment is decided.

## Outcome at /review (2026-07-29) — criterion 1 is NOT met

/review ran 4 specialists, a Claude adversarial pass, and two Codex passes. Five of the six
independently reached the same conclusion, and Codex returned `Reject` / two `[P1]`s. What
changed as a result:

- **Criterion 1: OPEN.** Re-keying cannot deliver it (see the correction above). Deferred to
  `brief.2`: let the inbox job read the open set and check, per open thread, whether the newest
  message is from the operator. Needs `KbRead` on `ops:entities` plus a per-thread Gmail read —
  a real increment, not a prompt edit. The false causal claim is removed from this plan, from
  both configs, and from the handoff.
- **Criterion 2: MET on the markdown brief** (`~/.agentos-output/brief-{date}.md`), which is the
  documented read path in `DEPLOYMENT.md`. Note it is **not** met on Telegram: that bridge pushes
  the runtime-authored `BriefRecord` from `GET /api/v1/brief`, not this markdown file.
- **Criterion 3: unaffected.** The instrument lands, which was the point.

Defects found and fixed in the same pass:
- The brief measured **8 660 bytes at this plan's own documented maxima against a hard 8 192-byte
  cap** — an over-size entry fails the inbox write, the curator then finds no input and stops, and
  there is **no brief that morning**. Caps tightened, twice: /review set `important ≤8` plus bounds
  on `from`, open-item text and `focus_recommendation`, and **/qa then found that sizing was against
  the wrong number** (see below).
- Three other copies of these prompts shipped stale, including the **QEMU production config**.
  All are now mirrored and pinned by a test over all four sources.
- The wrapped `kb_put(` this increment introduced **blinded two existing prompt guards** (they
  scanned one line at a time). The scanner now joins continuation lines, with a negative control.
- `if thread_id is present` was wrong against `thread_id: null` — the field is always present, so
  every non-thread item would collide on one `open:None` key.
- `stable_slug_of_item_text` was undefined; it is now an exact rule, within the store's 128-char
  and charset limits.
- A model-supplied `thread_id` went unvalidated into a markdown href in a document the operator
  clicks. Links now require `^[0-9a-f]{1,20}$`.
- No tolerant read for the old bare-string `open_items` shape, combined with "ALWAYS include
  thread_id", invited a fabricated id — a valid-looking link to the wrong thread.

## Outcome at /qa (2026-07-29) — two more criticals, both about the daily write surviving

/qa drove a **real `agentd`** against a fake `/v1/messages`, so the scheduler, the native `kb_put`
tool, the scratch-class path and the redb store were all real. Full report:
`.gstack/qa-reports/qa-report-agentos-cos-2026-07-29.md`.

- **QA-1: /review sized the brief against raw JSON, and the store does not.** `kb_put` embeds the
  brief as a JSON *string* inside a provenance wrapper, so every `"` becomes `\"` — about **605 bytes**
  of overhead, not the ~220 assumed. Real margin after /review's fix was **39 bytes**, not 644. Now
  `important ≤6` with summaries/asks and open-item text at 100 → **1 255 bytes** spare, pinned by a
  test bound to the real `MAX_MEM_CONTENT_BYTES` and negative-controlled at three points.
- **QA-2: the caps said "chars" and both stores count bytes.** With 80-character CJK subject lines the
  brief hit 9 163 wrapped bytes and the real store **rejected it** — no brief that morning, for an
  operator whose only sin is receiving mail in a non-Latin script. Caps are now in bytes with the 2–3×
  multiplier stated, and STEP 4 gained a **shrink-and-retry ladder** so an over-size brief degrades to
  a shorter one instead of vanishing. Both halves verified on the real store. Durable fix (runtime-side
  truncation) filed as `brief-04`.
- **QA-3 (no defect):** key length and charset limits differ between the sidecar (128 chars, bars `/`)
  and native L1 (longer, permits `/` and `.`). The slug rule shipped here is the intersection, so it is
  valid on both paths.

## Outcome at /ship (2026-07-29) — the fixes themselves had nine defects

/ship ran a fix-review round scoped to `130af4c5..74d0f140` (the /review and /qa fixes), plus
testing and security specialists on the same range. **They found nine defects in the fixes**, five
of them mutation-proven false greens in guards I had just written. This is the third consecutive
round in this increment where the previous round's fixes were the defect source.

Security, in the fixes:
- **The hex guard covered the wrong field.** `thread_id` was locked down; `from`/`subject`/`ask`/
  `summary`/open-item `text`/`focus_recommendation` still went into markdown raw. One email with
  the subject `Payment overdue [Pay now](https://evil.example)` yields a live attacker link — no
  escape trick needed. Now entity-escaped by rule in both configs; `brief-03` re-rated **P3 → P1**.
- **The shrink ladder turned fail-loud into fail-silent.** It dropped by model-assigned `urgency`
  (a judgement of sender-written text) and truncated the model's own analysis while preserving
  sender bytes, and the omission was recorded nowhere the operator reads. Rewritten: shed
  sender-written bytes first, drop by age not urgency, an `omitted` record in the JSON, a
  `⚠ Shortened to fit` line in the brief, and a guaranteed-fit floor rung.
- **The ladder had no terminal state** — "do not report failure" after 3 attempts meant reporting
  success with nothing persisted. Now ends with `BRIEF WRITE FAILED (size)`.

Correctness, in the fixes:
- **`deadline` was uncapped** in the prompt but modelled at 10 bytes in the test; 8 verbose
  deadlines ate the entire margin. Now `≤20 bytes` everywhere.
- **The ladder did not recover the case it was written for**: a fully non-Latin brief measured
  16 060 B and was still 9 181 B after all three rungs — 989 B over. Hence the floor rung.
- **`person:{normalized_name}` was undefined** in the same breath as "which is why the slug rule is
  exact rather than left to you". Now uses the same slug rule.
- **The key-limits sentence was factually wrong** (stated the sidecar's rule as universal; L1 in
  fact permits `/` and `.` and rejects spaces and apostrophes) and worded differently per config.
  Now states the intersection, identically in both.

Guards that did not guard (all mutation-proven, all now negative-controlled):
- `kb_calls()` bounded the join at 4 lines and counted parens inside string literals, so a 5-line
  call or a `)` in a quoted argument escaped every guard built on it. Window widened to 12, quoted
  spans stripped, and an unbalanced capture is now a loud failure rather than a silent truncation.
- The open-item key guard asserted `raw.contains("open:{thread_id}")` — satisfied by a *comment*.
  It now asserts on the extracted calls, which also makes it spelling-independent.
- The link guard grepped for a regex literal that appears three times per config, so deleting the
  actual STEP 4 control left it green. Now bound to the control sentence and its fail-closed branch.
- The cap-drift guard cross-checked 2 of 9 caps, so `open_items ≤10 → ≤30` shipped green while the
  real entry was 1 765 B over. Now driven from the constants, whitespace- and `<=`-tolerant.
- The ladder guard pinned only L1's `entry too large`, not the sidecar's `content too large` — the
  phrase the path production actually uses. Both pinned, plus a new test asserting
  `HIT_CONTENT_CAP == MAX_MEM_CONTENT_BYTES` so the two backends' caps cannot drift apart.

Two new executable properties replace prose: a CJK brief built to the prompt's own "roughly a
third" rule must fit, and the ladder's floor rung must fit with 2 KiB spare.

**What /qa could not verify, stated plainly:** whether the model obeys any of these instructions.
`thread_id` emission, the `^[0-9a-f]{1,20}$` link guard, the tolerant read, the slug rule and the retry
ladder are all prompt adherence, which needs a real model against real Gmail — no API key, no Docker,
no OAuth token on this machine, and faking Gmail would mean disabling the broker's SSRF controls. The
mechanics beneath the prompt are proven; the prompt's own effect is not. **The first real brief is that
test**, and something in it will likely be wrong.

## Review phases

- **CEO:** both voices RESHAPE. Converged on: reshape to the one-day fix, run the tally, and note
  that `audit-S3`, naming mv design partners, and `p7.7-ar-03` all outrank this track.
- **Design:** STRUCK — no UI. The brief markdown is the surface and it already exists; the KB
  surface question is moot per finding 2.
- **Eng:** the class check (`scratch`, verified) and the thread-ID availability gap are folded into
  scope and open questions above. Remaining Eng work is small enough to fold into build.
- **DX:** not applicable — no operator-facing command or flag changes.
