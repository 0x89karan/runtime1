<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/brief.3-runtime-authored-brief-autoplan-restore-20260730-223009.md -->
# brief.3 — the runtime authors the operator's brief

**Status:** **DEFERRED at the /autoplan premise gate (2026-07-30).** Not struck — the defect is
real and the reshape is named below. Both CEO voices returned adverse on 6/6 dimensions
(Codex `STRIKE`, Claude `DEFER`) and the deciding evidence was empirical, not argumentative:
**brief.1 has never produced a brief**, so the P3 → P1 re-rate that justified doing this now
describes a document that has never been generated.

**Operator decision:** run the experiment first, and ship the two cheap fixes that do not depend on
it. Do NOT return to this plan until there are seven real briefs and a tally.

**If/when it returns, it returns in the reshaped form** (CEO F5, both voices converged): ONE typed
brief — extend `BriefRecord` with the email section, escape once in the runtime, render to all three
surfaces. That closes `brief-03` AND `brief-02` together and avoids a second runtime renderer, which
is what this plan as drafted would have locked in permanently. Never build the second renderer.
**Closes:** `brief-03` (P1). **Candidate co-scope:** `brief-04` (P2) — see the scope question.
**Predecessor:** brief.1 (v0.117.0), which mitigated `brief-03` *in the prompt* and said plainly
that a prompt rule is not enforcement.

---

## The defect, in one paragraph

`from`, `subject`, `ask`, `summary`, open-item `text`, `deadline` and `focus_recommendation` are
written by whoever emailed the operator. They travel Gmail → `cos-inbox` model → KB → `cos-curator`
model → a markdown file the operator opens and clicks. A subject reading
`Payment overdue [Pay now](https://evil.example)` becomes a live attacker link. brief.1 added a
prompt rule telling the curator to entity-escape `[ ] ( ) |` and drop a leading `!`
(`agentd/cos.agents.toml:524`), pinned by `config::tests::cos_prompts_never_interpolate_unvalidated_thread_id_into_a_link`.
That test asserts **the rule is present in the file**. Nothing asserts the model obeyed it, and
nothing can.

## What the code actually says (verified, not assumed)

brief.1 shipped on a premise that turned out false, so every load-bearing claim here is
file:line-checked.

1. **`write_file` does not inspect content.** `agentd/src/tools/native.rs:120-160`: gated by
   `Capability::FsWrite { prefix }` on the *requested path* only; body is `create_dir_all` +
   `tokio::fs::write`. No escaping, no size limit, no content awareness. The markdown brief is
   authored entirely inside the prompt (`cos.agents.toml:511-562`).

2. **`BriefRecord` contains NO sender-derived data.** `agentd/src/runs/mod.rs:86-112` —
   `brief_id`, `created_at`, `window_from/to`, `run_count`, `failed_count`, `spend_total`,
   `items: Vec<BriefItem>`, `overflow_count`, `attention_overflow`, `narrative`. `BriefItem`
   (`runs/mod.rs:66-79`) is `run_id`, `agent_id`, `status`, `spend`, `stop_reason`, `last_error`.
   Every string except `narrative` comes from agentd internals.
   > **This corrects `brief-03`'s stated fix.** "Author the brief markdown from the typed
   > `BriefRecord`" cannot be done as written: `BriefRecord` is a *run-history* document. The
   > markdown brief is an *email* document. They share a word, not a schema. Either the email data
   > must reach the runtime by a new path, or the runtime must read the KB — which `publish_brief`
   > never does (`runs/store.rs:431-560` touches only `runs.redb`).

3. **Nothing in the system reads `./output/brief-*.md`.** Verified by grep across
   `*.rs|*.py|*.sh|*.toml|*.yml|Dockerfile|Makefile`: the only hits are the two prompts that write
   it, the entrypoint sed rules, capability tests, the compose/9p mounts, and `docs/RUNBOOK.md:733`.
   It is a **leaf artifact** consumed by a human with `cat`. Changing how it is produced breaks no
   downstream code.

4. **`agentctl brief` and Telegram consume a different artifact.** Both read `BriefRecord` over
   `GET /api/v1/brief` (`agentctl/src/brief.rs:25-57`, `docker/telegram_mcp.py:193-202`), not the
   markdown. So this increment does **not** fix what Telegram shows (that is `brief-02`).

5. **A third instance of the same defect class, previously unexamined.**
   `agentctl/src/brief.rs:61-124` interpolates `narrative`, `last_error`, `stop_reason` and
   `agent_id` into terminal output with **no sanitising**. `narrative` is model-authored by a
   curator whose context holds untrusted email, so it is sender-influenced. The workspace's only
   control-char stripper is `agentctl/src/watch/memory.rs:261-263` (`sanitize_str`, not exported,
   not markdown-aware). Terminal escape sequences in a `println!` are a real if lesser cousin of
   markdown link injection.

6. **All of it is new code.** No markdown crate anywhere in `Cargo.lock` (`markdown`, `comrak`,
   `pulldown`, `html-escape`, `ammonia`, `askama`, `tera`, `handlebars` → zero matches). No
   escaping helper. The five HTML entities appear in Rust **only inside a test that greps the
   prompt** (`config.rs:2233-2239`). `chrono` is already a dependency, so date formatting needs no
   new crate.

7. **The write path is deployment-specific in three ways.** `./output` (dev, `cos.agents.toml:451`),
   `/data/output` (Docker, produced by `docker/entrypoint.sh:244-245` sed), `/run/output` (QEMU,
   `distro/overlay/…:347`). The sed rewrites the capability grant and the prompt string as a
   coupled pair, with a fail-closed desync guard at `entrypoint.sh:247-262` and a CI negative
   fixture at `.github/fixtures/cos-broken-relative.toml`. If the runtime owns the path, that guard
   and fixture need re-pointing, not deleting.

8. **The KB write path only ever rejects, never truncates.** `native.rs:652-657` measures the
   *serialized envelope* (`content` embedded as a JSON string inside a provenance wrapper), so the
   brief pays for its own quoting plus ~605 bytes. The complete inventory of existing runtime
   truncation is: `read_file` output (`native.rs:111-116`, with a marker), `narrative`
   (`store.rs:433-439`, **silently**), and `items` (`store.rs:517`, with `attention_overflow`
   reported). On `kb_put` and `mem_remember` the runtime rejects the whole entry.

## Proposed scope

**One new native tool that renders the brief in Rust, replacing the prompt's markdown template.**

- `write_brief` (name TBD at Eng review) accepts the **typed** brief — the same shape the inbox job
  already produces (`important[]`, `response_needed[]`, `open_items[]`, `focus_recommendation`,
  `thread_count_reviewed`, `skipped_count`, `omitted{}`) — and returns the path it wrote.
- The runtime owns, and the prompt stops owning: the markdown structure; escaping of every
  sender-written field; the `^[0-9a-f]{1,20}$` thread-id check and permalink construction; the
  `⚠ Shortened to fit` line; the output path.
- The model keeps what only a model can do: which items matter, their order, the summaries and
  asks, `focus_recommendation`, and the narrative.
- `write_file` stays granted (other uses) but the curator's brief no longer goes through it.

**Escaping approach (to be settled at Eng review):** prefer emitting markdown that is *inert by
construction* over post-hoc escaping — e.g. table cells and bullet text passed through a single
`escape_inline()` that neutralises `[ ] ( ) | ` ` * _ #` and strips control characters, with the
permalink the only link the emitter is capable of producing. A denylist of five entities copied from
the prompt is the weaker option and should be argued for explicitly if chosen.

**Blast-radius items (P2 "boil lakes", both small):**
- `agentctl/src/brief.rs` — sanitise before printing (finding 5).
- The `entrypoint.sh` sed/guard pair and the CI fixture — re-point, do not delete (finding 7).

## The scope question this plan must answer first

`TODOS.md` says `brief-03` and `brief-04` "should probably be one increment". Two facts argue
against:

1. **They are at different hops.** `brief-03` is the *curator writing markdown*. `brief-04` is the
   *inbox job's `kb_put`* of the OperatingBrief JSON being rejected when over 8 KiB. Different
   functions, different failure modes, no shared code.
2. **`CLAUDE.md` is explicit:** "Implement exactly one per branch; do not bundle several together."

Against that: both are "move enforcement from the prompt into the runtime", and a truncate-and-report
`kb_put` is genuinely small. **Recommendation: brief-03 only, brief-04 as the immediate follow-on.**
The CEO phase should ratify or overturn this.

## Explicitly NOT in scope

- **`brief.2`** (handled items still re-list). Different defect, gated on the operator tally.
- **`brief-02`** (Telegram gets a Python dict repr, `telegram_mcp.py:311`). Real and adjacent, but it
  is a `BriefRecord` rendering problem, not a markdown-authoring one.
- Changing `BriefRecord`'s schema, unless Eng review shows the typed brief must ride on it.
- Any new provider write, scope, or approval gate.
- Making the brief prettier. This is a security and durability fix.

## Premises (for the gate — do not accept silently)

- **P1** The markdown brief is worth keeping at all. It is a leaf artifact no code reads; the
  alternative "delete it and rely on `agentctl brief` + Telegram" is a real option and cheaper.
  **This is the premise most likely to be wrong.**
- **P2** Runtime-authored markdown is enforcement in a way a prompt rule is not.
- **P3** The model must still supply the item text, so escaping (not rejection) is the right control.
- **P4** A new native tool is the right shape, rather than extending `publish_brief` or having the
  runtime read the KB.
- **P5** Doing this now beats the three items both CEO voices ranked above the brief track
  (mv design partners, `p7.7-ar-03`, `audit-S3`) — the operator tally that gates `brief.2` does
  **not** gate a P1 security fix.

## Success criteria

1. A sender-supplied `subject` containing `[x](https://evil.example)`, `|`, and a leading `!`
   appears in the brief as inert literal text, proven by a Rust test over the real emitter.
2. No prompt instruction is load-bearing for escaping: deleting the prompt's `NEUTRALISE SENDER
   TEXT` paragraph cannot reintroduce a live link (mutation-verified, per this increment's habit).
3. The brief still renders correctly for a normal day, byte-identical in structure to what v0.117.0
   produced, so the operator sees no regression.
4. `agentctl brief` no longer prints raw control characters from model-authored fields.

## Verification plan

- Rust unit tests over the emitter: the injection corpus, the CJK case, the empty/one-item cases,
  the `omitted` line, and the thread-id gate's fail-closed branch.
- Mutation controls: delete the prompt paragraph → tests still pass (proving the prompt is no longer
  the control); break `escape_inline` → tests fail.
- The `/qa` fake-provider rig from brief.1 (`<scratchpad>/qa/`) drives a real `agentd` end to end.
- Honest limit, stated up front: whether the *model* calls the new tool at all is still prompt
  adherence. The difference is that a disobedient model now produces **no brief** rather than an
  **unsafe** one — fail-closed instead of fail-open. That is the actual security gain and the plan
  should be judged on it.

---

# CEO REVIEW (Phase 1) — 2026-07-30

**Both voices say do not build this now.** Codex: `STRIKE`. Claude: `DEFER`.
Neither disputes the defect exists. Both dispute its priority, and one produced
field evidence that invalidates the plan's severity argument outright.

## The finding that decides it: I verified the code and never looked at the output

Independently verified by me after the Claude voice raised it:

| Claim | Evidence |
|---|---|
| The pipeline has produced **2 briefs in 15 days** | `~/.agentos-output/`: `brief-2026-07-16.md`, `brief-2026-07-23.md`. Nothing since. Today is 2026-07-31. |
| **brief.1 has never produced a brief** | The real Response Needed table is `\| From \| Subject \| Ask \| Deadline \|` — **four columns, no Thread column**. v0.117.0's feature exists in no artifact. |
| The pipeline **cannot** run right now | `docker info` → daemon down. The one-week tally cannot start. |
| The model **already escapes** table-breaking characters unprompted | `brief-2026-07-23.md:31` contains `Hackathon \| Tech Discussion` — a `\|` escape the model produced before brief.1 said anything about escaping. |

`brief-03`'s P3 → P1 re-rate rests on: *"brief.1 made this worse — the new Thread column
normalises `[open](https://mail.google.com/…)` throughout the document."* **That worsening is a
claim about a document that has never been generated.** Deflate the premise and P5's queue-jump
collapses with it, because P5 is justified entirely by the P1 label.

## CEO consensus table

| # | Dimension | Claude | Codex | Consensus |
|---|---|---|---|---|
| 1 | Premises valid? | NO (P1 unadjudicated, P4 wrong) | NO (P1 false, P4 probably wrong) | **CONFIRMED — premises fail** |
| 2 | Right problem to solve now? | NO — run the experiment first | NO — fix the canonical surfaces | **CONFIRMED — no** |
| 3 | Scope calibration correct? | NO — second renderer locks in divergence | NO — preserves accidental surface | **CONFIRMED — no** |
| 4 | Alternatives explored? | NO — 4 unexamined | NO — 6 unexamined | **CONFIRMED — no** |
| 5 | Security framing honest? | NO — §9.5 already accepts a wider hole | NO — inflated, link already live in Gmail | **CONFIRMED — inflated** |
| 6 | 6-month trajectory sound? | NO — mv gate dies unattended | NO — three higher-value items stay open | **CONFIRMED — no** |

0/6 confirmed sound. This is the most adverse consensus of any increment in this project.

## Where I corrected Codex

Codex called the markdown brief "non-canonical accidental surface area" and recommended deleting it.
**Wrong on the facts:** `docs/DEPLOYMENT.md:194` and `docs/RUNBOOK.md:733-734` both instruct the
operator to `cat` that exact file, and Telegram delivery is opt-in (`TELEGRAM_BOT_TOKEN`). No *code*
reads it, but it is the *documented* primary human surface. Deleting it today would be deleting the
CoS product's only real output — the Claude voice reached the same conclusion by reading the two
briefs and finding their entire value is the Response Needed table, which `agentctl brief` and
Telegram do not render at all.

## Findings both voices raised that survive independent of the priority call

- **F2 (Claude, CRITICAL).** The plan books an availability *regression* as a security win. Its own
  closing line argues the gain is "no brief rather than an unsafe brief". The observed failure mode
  of this product is *already* no brief — 8 of the last 8 days, and brief.1's headline defect was
  exactly a zero-brief morning. A typed tool call adds a new schema-mismatch path to zero-brief. For
  a once-a-day digest, **availability is the product.** No success criterion covers it.
- **F3 (Claude, CRITICAL).** `THREAT_MODEL.md` §9.5 already accepts, open by design, that an
  injected curator can write a *misleading brief* (its worked example: `"URGENT: wire funds to X"`).
  Escaping does nothing about fabricated prose. brief.3 closes a narrow hole in a wall with a
  deliberate wider one. The genuinely high-severity half of §9.5 — untrusted open-item text read
  back into the curator every morning, i.e. **persistence** — is what escaping helps least with, and
  the plan never mentions it.
- **F5 (Claude, HIGH) + Codex.** A runtime-authored brief renderer already exists
  (`publish_brief` → `BriefRecord` → `agentctl brief`/Telegram). brief.3 adds a *second* one for a
  third artifact and explicitly excludes `brief-02`, locking in two schemas and two escaping paths
  permanently. The 10x reframe both voices converge on: **one typed brief, escaped once, rendered to
  all three surfaces** — closes `brief-03` and `brief-02` together. Also unnoticed by the plan:
  `docker/telegram_mcp.py:147-153` sends with **no `parse_mode`**, so Telegram is structurally immune
  to markdown injection already.
- **F10 (Claude, MEDIUM).** Two cheap alternatives never considered. (a) Escape at the existing
  choke point: neutralise link syntax inside `write_file` for `.md` targets under the brief prefix —
  ~20 lines, no schema, no new tool, no prompt migration, and it *cannot* fail closed into "no
  brief". (b) **Revert brief.1's Thread column** — one prompt paragraph — which restores the "a
  bracketed link is anomalous" property at zero cost and returns `brief-03` to P3 by the plan's own
  argument.
- **F9 (Claude, MEDIUM) — the one thing worth shipping now.** `agentctl/src/brief.rs:61-124` prints
  model-authored `narrative`/`last_error`/`stop_reason` unsanitised. ~30 minutes, and it lands on a
  surface the operator *actually* uses. Nothing to do with a markdown emitter. Unbundle it.

## New defect found during review, filed separately

**brief.2's premise is contradicted by the only field evidence.** `brief-2026-07-23.md:39` says
verbatim: *"ℹ️ No prior-day briefs found in KB — all items below originate from today's inbox
scan."* — while `brief-2026-07-16.md` existed and STEP 2 had written `ops:briefs`. The carry-forward
path found **nothing**. That is the *inverse* of brief.2's stated defect (items re-listing forever).
Six of the ten "carried forward" items on 07-23 also appear on 07-16, meaning the model re-derived
them from the inbox rather than the KB. Given brief.1's premise being wrong is the entire reason this
plan opens with a verification manifest, this goes into `TODOS.md` against brief.2 before anyone
builds it.

## Cost of delay, the actual tiebreaker

| Item | Effort | Cost of delay |
|---|---|---|
| **mv design partners** | zero engineering | **Irreversible.** Gate 2026-10-01, 62 days out, 0 of 10 named humans, 0 of 3 demos. `ROADMAP.md:65` quotes the design doc: "an unnamed gate date is how 'deferred' becomes 'never'." |
| **`p7.7-ar-03`** | ~half a day | The cockpit shows a **false `0 denied`** — a wrong answer about governance, on the control surface, in a product whose differentiator is governance-in-the-boundary. |
| **`agentctl brief` sanitiser** | ~30 min | Small, but on the surface actually used. |
| **brief.3 as planned** | several days, high defect prior (last increment: 9 → 2 → 9 criticals, 5 mutation-proven false greens) | Recoverable. Buildable any week. |

brief.3 can be built any week. The mv gate cannot.
