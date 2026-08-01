# attn.1 — the interrupt tier

**Design doc:** `~/.gstack/projects/0x89karan-runtime1/0x89karan-main-design-20260801-cos-attention-router.md`
**Status:** DRAFT — pending `/autoplan`
**Branch:** `attn.1-interrupt-tier`

## Why

**Evidence base — read from the artifact, not from the code.** `~/.agentos-output/` holds three
briefs: `2026-07-16`, `2026-07-23`, `2026-07-31`. The 07-31 one is the **first brief produced since
brief.1 shipped**, and it is the source for every claim below.

The CoS has exactly **one urgency level: "tomorrow at 08:00."** The 07-31 brief carries **17
actionable items** (6 Important, 5 Response Needed rows, 5 open items) at a single priority, in one
file, written at 02:55. Two are marked `**2026-07-31 TODAY**`:

> `<a colleague>` — *"Upgrade GCP account — John lacks permission to do
> it himself."* Deadline: **TODAY**.

That is verbatim the operator's own interrupt-tier criterion — *someone senior blocked on you, right
now* — and it sat in a file, at the same priority as a TOKEN2049 ticket promo, until he happened to
open it. Nothing about the pipeline could treat those two differently, because there is only one tier.

**A correction, recorded so it does not propagate.** An earlier draft of this plan and its design doc
cited an "Apple Business org deleted, admin login blocked" security incident as the motivating
example. **That string appears in no brief file.** It came from the `BriefRecord` runtime narrative
(`agentctl brief`), a different artifact, and could not be substantiated. The GCP/John item above
is the verifiable replacement. This is the `verify-the-artifact-not-just-the-code` failure mode
recurring inside the very increment that logged it — the check caught it, one round earlier than
brief.1 managed.

Office hours (2026-08-01) reframed the CoS from *email digest* to **attention router** and locked
three tiers — **interrupt / morning / never**. `morning` ships. `never` half-works (the 2026-07-31
brief reviewed 20 items and skipped 30). **`interrupt` does not exist.** This increment builds it.

This is also the first CoS work that makes the agent *act* rather than *report*, which is the only
version that exercises the trust substrate the whole thesis rests on.

## Success measure (D4)

14 days after this ships: **does the operator stop checking email manually?** PASS = Gmail opened
because an interrupt said so, or during the morning brief, not otherwise.

Paired computable proxy (built in `attn.2`, not here): the already-handled rate of morning-brief
items. Rising rate = failing D4.

**Kill condition:** still checking manually after 14 days with no fall in already-handled rate ⇒ the
attention-router thesis is wrong for this operator; fall back to "nothing interrupts, make the daily
brief unmissable."

---

# CEO REVIEW (Phase 1, /autoplan 2026-08-01) — mode: SELECTIVE EXPANSION

## 0A. Premise challenge — 1 of 5 premises survives

| # | Premise | Verdict |
|---|---|---|
| P1 | The CoS surfaces every item at a single priority | **VERIFIED** — 17 actionable items, one file, one time |
| P2 | The single tier is *why* urgent items go unactioned | **CONTRADICTED** — see below |
| P3 | An interrupt tier is the highest-value next increment | **CONTRADICTED** — opportunity cost + root cause |
| P4 | Telegram is an acceptable interrupt channel | **CONTESTED** — inverts the leak profile |
| P5 | "I stop checking email" is measurable by `attn.1` | **FALSE as scoped** — the proxy is deferred to `attn.2` |

**P2 fails on the artifact.** All three briefs already contained urgent, blocking items — 07-16: MIT
signature deadlines + laptop approvals; 07-23: invoice/GCP/GitHub budget; 07-31: GCP/John + RDI.
The brief **is already surfacing the urgent things.** It is not a tiering failure.

**The actual root cause, found in the compose file.** `docker-compose.yml` has **no `restart:` policy
on any service** (verified: zero matches). So `cos` is hand-started and never comes back — not on
failure, not on Docker restart, not on reboot. `RestartCount=0`. That is why there are 3 briefs in 15
days. Combined with the committed `TRIGGER_INTERVAL=every 2m` default, the pipeline oscillated between
two failure states, neither of them tiering:

| State | Cause | Result |
|---|---|---|
| not running | no `restart:` policy, hand-started | no brief at all, most days |
| running catastrophically | `every 2m` default | 31 cycles, ~4.1M tokens in one morning |

Both fixes are **compose one-liners.** Neither needs an agent, a tool, a table, or a route.

**P5 fails on sequencing.** `attn.1` ships the behaviour change and defers the only computable
measurement (`attn.2`'s already-handled check) to the *next* increment. That is backwards: the
increment that changes behaviour must carry the instrument that judges it.

**P4 is worse than the design doc admitted.** Verified: `THREAT_MODEL.md` §8.7 already names
`api.telegram.org` "a real confidentiality sink", and `PRODUCT-THESIS.md:36` defines the beachhead as
someone who "won't put investor threads, customer deals, and cap-table context into someone else's
cloud." A digest sends everything at low sensitivity density. **An interrupt tier selects the most
sensitive items and sends only those** — it inverts the leak profile in the worst direction.

## 0B. Existing code leverage — what already exists

| Sub-problem | Existing code | Reuse? |
|---|---|---|
| publish a typed record → operator surface | `publish_brief` + `BriefPublish` + run-writer lane, `native.rs:876` | direct precedent |
| management read route | `GET /api/v1/brief`, polled by the sidecar | mirror |
| off-machine delivery + dedup | `docker/telegram_mcp.py`, durable `_delivered` state `:218` | extend |
| Gmail fetch through the broker | generic `oauth_call_api`; params allowlisted not paths (`cos.agents.toml:100`) | **no new sidecar needed** |
| sealed job with config-owned caps | cap.2b `[[jobs]]` + `run_job` | mirror |
| keeping it running | **nothing — no `restart:` policy exists** | the actual gap |

Nothing here is a rebuild. The one genuine gap is the cheapest thing in the plan.

## 0C. Dream state

```
  CURRENT STATE                    THIS PLAN                      12-MONTH IDEAL
  brief runs ~1/5 days,       →    adds a 2nd loop + a 3rd    →   attention router across
  container never restarts,       delivery channel to a          many channels, learned
  17 items at one priority        system that isn't running      urgency, acts unsupervised
```

**The plan moves sideways, not toward the ideal.** The 12-month ideal needs a *reliable substrate
with observed behaviour data*. `attn.1` adds surface area to an unobserved system, and the learned-
urgency destination is explicitly blocked on a replied-to signal that neither `attn.1` nor today's
pipeline can produce.

## 0C-bis. Implementation alternatives (MANDATORY)

```
APPROACH A: attn.0 — make it run, then look (RECOMMENDED)
  Summary: restart: unless-stopped + the daily-cron default already on this branch.
           Run 14 days. Log the D4 self-report. Then decide if a tier is needed.
  Effort:  XS (human ~30 min / CC ~5 min)
  Risk:    Low
  Pros:    Fixes the VERIFIED root cause; starts the measurement clock today;
           costs ~180k tokens/day less than attn.1; cannot be wrong about the tier
           because it does not build one; unblocks the mv gate work in parallel.
  Cons:    Ships no new capability; the operator waits 2 weeks for a verdict;
           feels like a non-increment.
  Reuses:  docker-compose.yml only. Zero Rust.

APPROACH B: attn.1 as written — full interrupt tier
  Summary: publish_interrupt + InterruptPublish + INTERRUPTS table + route +
           sidecar delivery + sealed 30-min triage job, 4 config copies.
  Effort:  L (human ~3-4 days / CC ~3-4 h)
  Risk:    High
  Pros:    Delivers the office-hours design in full; exercises the trust substrate;
           the interrupt can carry an action, which is the thesis wedge.
  Cons:    Built on a premise the artifact contradicts; defers its own measure;
           worsens the Telegram leak profile precisely where it hurts most;
           ~180k tokens/day standing cost; 4-copy config drift risk.
  Reuses:  publish_brief pattern, telegram sidecar, oauth_call_api.

APPROACH C: attn.0 + the measurement instrument (no tier)
  Summary: A's compose fix, PLUS attn.2's already-handled check moved forward so
           the 14 days produce a COMPUTED number, not a self-report.
  Effort:  S (human ~1 day / CC ~45 min)
  Risk:    Low-Med
  Pros:    Same root-cause fix as A, but the 14 days yield falsifiable data;
           the already-handled check is independently useful (suppresses resolved
           items) and needs no broker change or new scope; creates the replied-to
           signal that learned urgency later requires.
  Cons:    More than A; still ships no interrupt; the check costs one extra
           threads/{id} call per brief item.
  Reuses:  gmail.readonly, existing broker allowlist, curator prompt.
```

**RECOMMENDATION: Approach C.** It fixes the verified root cause (P1/explicit-over-clever), and
unlike A it makes the 14-day verdict *falsifiable* rather than a self-report — which is the exact
defect Codex identified in P5. It is the smallest change that produces real data, and every
downstream option (including B) is better decided with that data than without it.

## 0D. Complexity check (SELECTIVE EXPANSION)

Approach B touches: `native.rs`, `runs.rs`, `management.rs`, `config.rs`, `capability.rs`, `main.rs`,
`telegram_mcp.py`, `docker-compose.yml`, and **4 copies** of the CoS prompt config = 12+ files, 1 new
capability, 1 new redb table, 1 new route, 1 new job. That is over the 8-file smell threshold and
introduces 2+ new subsystems — for a hypothesis the artifact does not yet support.

## 0E. Temporal interrogation

```
  HOUR 1   Which of the 4 prompt-config copies is authoritative? (Docker seds agentd/cos.agents.toml)
  HOUR 2-3 Interrupt dedup: runtime-authoritative per thread_id, or sidecar? (answer: both, runtime wins)
  HOUR 4-5 Quiet hours — does an invoice justify 03:00? UNRESOLVED, and asymmetric: one bad
           03:00 buzz costs more trust than five missed items.
  HOUR 6+  "I wish I'd known the container had no restart policy before building a second loop."
```

## Findings this phase produced (beyond the plan)

1. **`brief-05` is CLOSED with evidence.** `CLAUDE.md:176-177` claims brief.1 "has never produced a
   brief" and that the Response Needed table has "no Thread column." **Both are now false.** The
   07-31 brief has the Thread column, 12 valid hex thread permalinks, and the escaping rule applied.
   brief.1 is verified working in production. Correct CLAUDE.md.
2. **Criterion 1's symptom is not observed.** `brief.2` assumes handled items *re-list*. The artifact
   shows the opposite: 07-23 and 07-31 both report **no carry-forward at all** ("first brief in the
   store"), while 07-16 did carry 2 items forward. Carry-forward worked once and then stopped. This
   confirms `brief-06` and means `attn.2`'s framing must be "make carry-forward work and suppress
   resolved", not "stop re-listing."
3. **New defect — the brief is barely readable as plain text.** 37 HTML entities in one file
   (`&#40;`×6, `&#60;`×8, `&#62;`×8, `&#8212;`×6, `&#36;`×2, `&#39;`). `John Doe
   &#40;redacted@example.com&#41;` is what the operator reads in a terminal. brief.1's escaping rule
   over-escapes: only `[` and `]` can forge a link; parens, dashes, quotes and `$` are collateral.
   Strengthens the case for `brief-04` (runtime-authored markdown) and is a concrete, visible defect.
4. **Weak signal on engagement, honestly caveated.** File atimes: 07-16 read 1 minute after being
   written; 07-23 not accessed until 07-30 (7 days later); 07-31's atime is contaminated by my own
   read this session and is unusable. One clean data point suggests prompt reading, one suggests a
   7-day lag. Not enough to conclude, and stated here so nobody later cites it as proof.

## 0.5 CEO dual voices — CONSENSUS TABLE

Source: `codex+subagent`. Both foreground, neither seeing the other.

```
═══════════════════════════════════════════════════════════════════════
  Dimension                              Claude   Codex   Consensus
  ─────────────────────────────────────  ───────  ──────  ───────────
  1. Premises valid?                     NO       NO      CONFIRMED
  2. Right problem to solve?             NO       NO      CONFIRMED
  3. Scope calibration correct?          NO       NO      CONFIRMED
  4. Alternatives explored?              NO       NO      CONFIRMED
  5. Competitive/market risk covered?    NO       NO      CONFIRMED
  6. 6-month trajectory sound?           NO       NO      CONFIRMED
═══════════════════════════════════════════════════════════════════════
  6/6 CONFIRMED adverse. 0 disagreements.
  Codex verdict:   "reshape hard, probably do not build attn.1 yet"
  Claude verdict:  "RESHAPE / DEFER"
```

**6 of 6 adverse, unanimous.** Both arrived independently at: the plan diagnoses *latency*; the
artifacts say the problem is *uptime*.

### The findings that survive verification

| # | Finding | Verified how |
|---|---|---|
| **C1** | "One urgency level: tomorrow at 08:00" is false — the shipped default was `every 2m`. There was **no 08:00.** The real single tier was "whenever the operator hand-starts `docker compose up`." | `docker-compose.yml` git history; 3 briefs at 3 unrelated wall-clock times (09:13, 16:01, 02:55) |
| **C2** | **The replacement example had already resolved inside the same brief.** `:5` "GCP upgrade blocked — 0x89karan must perform the upgrade today" and `:6` "GCP billing now live — Account upgraded to paid tier" are both listed 🔴 as open actions. The upgrade had happened. | read verbatim, above |
| **C4** | D1-qualifying items run **~3/week** (07-16 ≈3, 07-23 ≈3-4, 07-31 ≈1-and-stale). 48 fires/day × 14 d = **672 agent turns for ~6 interrupts — 112 fires per hit.** CLAUDE.md's standing gate: "if it is ~2 actions a morning, build nothing further." | counted across all 3 artifacts |
| **C5** | The three brief files contain the **mv design-partner pipeline** — live warm threads with Mayfield, MIT, Kong (domain-verified), plus Microsoft/GitHub, NTT, Amex by name. The plan mined these files for interrupt candidates and did not notice they answer the *only irreversible deadline on the roadmap* (2026-10-01, 0 of 10 partners). | `grep` on brief files: `@mayfield.com`×2, `@mit.edu`×2, `@konghq.com`×1 |
| **H1** | **`newer_than:1h` is not valid Gmail syntax.** `newer_than`/`older_than` take `d`/`m`/`y` only. The repo's working query is `newer_than:1d` (`cos.agents.toml:370`). Invalid → returns the whole unread set at `maxResults=25` → the 1.8% cost claim collapses. | Gmail operator set; repo's own usage |
| **H2** | **"No coupling between the loops" is false.** `global_token_budget = 10_000_000` (`cos.agents.toml:126`) is shared, and ux.8′ made exhaustion *defer* — so a triage misfire at 07:40 **defers the 08:00 brief.** The experimental loop can starve the working one. Also shares the OAuth broker and cred.7 health state. | line 126, verified |
| **H3** | No rate limit. `maxResults=25` + per-`thread_id` idempotency means one fire can emit **25 interrupts**, and one incident spanning threads emits one each — 07-23 contains literally "Bounced emails × 4". | plan scope §5 vs artifact |
| **H4** | The rule regex fires on "**Your Servcorp invoice is now ready**" (routine office invoice) and `DeadlineWithin24h` fires on "**Last chance for US$399**" (conference promo). Both would buzz at night. Quiet hours shipped as an open decision. | `brief-2026-07-31.md:17,21` |
| **M3** | **The `never` tier does not exist.** CoS holds `gmail.readonly` — nothing archives, nothing labels. "Skipped: 30" is a stat line in a markdown file. So the 30 skipped subjects have **never been seen**, and interrupt *recall* is unmeasurable. | scope of `gmail.readonly` |
| **M4** | Ship **tier + reason + permalink only — no `subject`, no `from`.** The operator taps through to Gmail anyway. This removes most of the leak *and* deletes most of the escaping problem that `publish_interrupt`'s security design exists to solve. | design consequence |
| **H6** | The 4.1M-token cost fix is an **uncommitted working-tree edit on an unlanded branch**. A stray `git checkout` deletes it. | `git status` |
| **M1** | D2's cost table used 300k/cycle against a measured 132k. | **caught independently and already corrected above** |

### Alternatives neither document considered

| | Alternative | Cost |
|---|---|---|
| **A1** | **Gmail's own filters** — VIP list + the same regex → star/important; phone set to high-priority-only. D1's entire rules tier with **zero code, zero tokens, zero new third party** (the mail is already at Google, so no new leak at all), and **zero uptime dependency — it works while the laptop is shut**, which attn.1 provably does not. | 20 min in a browser |
| **A3** | **Raise the existing pipeline 1× → 3×/day** (08:00/13:00/18:00). At the measured 132k/cycle ≈ 400k/day ≈ 4% of the window. Cuts worst-case latency 24 h → 8 h. No new capability, table, route, channel, or security surface. | 4 config lines |
| **A2** | "Make the daily brief unmissable" — named in the design doc only as the post-failure fallback, never evaluated on merit, despite the doc conceding it is "much less work." | — |

### The 6-month regret, concretely

It is 2026-10-01. The mv gate passes with 0 named partners and 0 booked demos, mv is struck **by its
own rule**, and the repo contains attn.1 + attn.2 + attn.3 — a personal email notifier of demonstrated
marginal value for one user — on a runtime whose commercial identity expired while it was being built.
**The 8 names were in the inbox the whole time.**

And the kill condition fires for the wrong reason: ~47 of ~50 daily items still arrive at 08:00, the
`never` tier does not exist, so D4 is **structurally guaranteed to read FAIL** regardless of how good
attn.1 is — retiring a possibly-correct thesis on an underpowered n=1 measure taken while the operator
is at TOKEN2049 and UC Berkeley.

---

# DECISION AT THE PREMISE GATE (2026-08-01)

**Both CEO voices returned RESHAPE/DEFER, 6/6 adverse, zero disagreements. The operator overrode the
challenge and reaffirmed the direction.** Decision id `5ef9f33a`. Recorded reasoning: the operator can
price the cost of a delayed escalation and neither model can, and the models evaluated attn.1 as an
email feature when the stated intent is substrate for a larger agentic system.

**attn.1 PROCEEDS**, with all twelve verified defects closed as *build preconditions* — not as
follow-ups — plus `E1` (uptime) landed first, because without it the increment measures nothing.

The adverse findings above are NOT struck. They stand as the record, and three of them are now
acceptance criteria (C4's expected-rate check, H5's instrument, M3's recall measurement).

## Scope (DECIDED)

### 0. `E1` — uptime, first, non-negotiable

The verified root cause of "3 briefs in 15 days". `docker-compose.yml` has no `restart:` policy on any
of its 5 services, while the Linux/QEMU path already has `Restart=on-failure` + `RestartSec=10`
(`distro/agentos-cos.service:38`). The Mac path the operator actually uses never got it.

- `restart: unless-stopped` on `cos`, `qdrant`, `semantic-kb-mcp`. **Not** on `agent` (it is a
  run-to-completion one-shot; `unless-stopped` would restart-loop a finished template).
- A launchd plist so the stack survives reboot and a closed laptop.
- A liveness signal: the brief's own `Stats` block gains `last successful cycle: <ts>`, so a silently
  dead loop is visible on the surface the operator already reads. **H2 corollary** — there is currently
  no operator-visible signal when the triage loop dies.

### 1. `docker-compose.yml` schedule fix — LAND ON `main` TODAY, STANDALONE (H6)

Already applied in the working tree. `TRIGGER_INTERVAL=${TRIGGER_INTERVAL:-every 2m}` → daily cron:

```yaml
- TRIGGER_CRON=${TRIGGER_CRON-0 8 * * *}
- TRIGGER_INTERVAL=${TRIGGER_INTERVAL-}
```

Single-dash `${VAR-default}` so an explicitly empty `TRIGGER_CRON=` still selects interval mode.
Verified with `docker compose config` in both modes.

**This must not ride on attn.1's fate.** It is a live cost bug — the default it replaces burned 4.1M
tokens in one morning — and it is currently an uncommitted working-tree edit on an unlanded branch,
where a stray `git checkout` deletes it. Land it independently, first.

### 2. `publish_interrupt` — new native tool

Mirrors `publish_brief` (`agentd/src/tools/native.rs:876`):

- New `Capability::InterruptPublish`; **not** in `"all"`, requires explicit listing (same gate as
  `BriefPublish`, `native.rs:1194`).
- Routes through the run-writer lane so it is ordered after run transitions.
- New `INTERRUPTS` table in `runs.redb`.
- Added to the `main.rs:1536` tier-legality arm alongside `BriefPublish`.

**M4 — the payload carries no sender-controlled text.** This is the single highest-leverage fix in the
review and it changes the tool's shape:

```rust
publish_interrupt {
    thread_id:   String,   // validated ^[0-9a-f]{1,20}$
    tier_reason: enum { AccessIncident, MoneyBlocked, SeniorBlocked, DeadlineWithin24h },
    why:         String,   // AGENT-authored, <=200 bytes, entity-escaped by the runtime
}
```

No `subject`. No `from`. The delivered message is *tier + reason + thread permalink*, and the operator
taps through to Gmail — where they were going anyway. Consequences, both good:

1. **The privacy inversion largely closes.** `THREAT_MODEL.md` §8.7 already names `api.telegram.org` a
   confidentiality sink, and `PRODUCT-THESIS.md:36` defines the beachhead as someone who will not put
   investor/customer/cap-table context in someone else's cloud. Interrupts would have selected the
   *most* sensitive items for third-party delivery. Sending no subject and no sender removes most of it.
2. **The escaping problem mostly evaporates.** `brief-03`/`brief-04` exist because sender-controlled
   text reaches a rendered surface. With `why` agent-authored and no sender fields, the remaining
   attack surface is one bounded string the runtime escapes. attn.1 no longer needs to wait on
   `brief-04`, and no longer spends its security budget there.

**Dedup and rate limiting are correctness requirements, enforced in the runtime (H3).**

- Idempotent per `thread_id`: a second call for an already-interrupted thread is recorded, not
  redelivered.
- **Hard rate cap: ≤1 per hour, ≤3 per day.** `maxResults=25` plus per-thread idempotency means one
  fire could otherwise emit 25 buzzes, and one incident spanning threads emits one each — the 07-23
  brief contains literally "Bounced emails × 4". Overflow beyond the cap folds into the morning brief.
- Treat the cap as a **security control**, not UX polish: it bounds the blast radius of a mis-tuned
  rule and of an inbox flood.

**H4 — quiet hours, LOCKED here, not deferred.** The plan previously shipped this as an open decision;
the artifact shows why that was unsafe. The rule regex fires on `brief-2026-07-31.md:21` — "Your
Servcorp invoice is now ready", a routine recurring office invoice — and `DeadlineWithin24h` fires on
`:17` — "Last chance for US$399", a conference ticket promo. Both would have buzzed at night.

- Outside **07:00–23:00 local**: only `AccessIncident` may deliver.
- Anything delivering at night requires **two independent signals** — VIP sender **and** rule hit.
- Everything else waits for the morning brief.

### 3. `GET /api/v1/interrupts` — management route

Mirrors `GET /api/v1/brief`. Returns undelivered interrupts, newest first, bounded.

### 4. Telegram delivery

`docker/telegram_mcp.py` gains an interrupts poll beside its approvals + brief polls, with a real
message format (not the raw dict repr of `brief-02`) carrying tier, reason, and permalink. Dedup via
the existing durable `_delivered` state (`:218`).

`AGENTOS_APPROVAL_SECRET` must be set whenever `TELEGRAM_*` is (`docker-compose.yml:56`).

### 5. The triage job

New sealed `[[jobs]]` entry (cap.2b pattern) in `agentd/cos.agents.toml` **and its three mirror
copies** — `distro/overlay/etc/agentd/cos.agents.toml` plus both templates. Four copies; editing one
is a partial deploy.

- Cron trigger, **every 30 min**.
- Haiku.
- Caps: `Mcp(google_oauth)` + `InterruptPublish`. No KB write, no file write, no spawn.
- Uses the existing generic `oauth_call_api` — no new Python sidecar.
- **H1 — the query. `newer_than:1h` is INVALID Gmail syntax**; `newer_than`/`older_than` accept
  `d`/`m`/`y` only, and the repo's own working query is `newer_than:1d` (`cos.agents.toml:370`), which
  is evidence `1h` was never tried. An invalid or ignored unit returns the whole unread set at
  `maxResults=25`, which would blow the token estimate by ~10× and put 100% of the correctness load on
  runtime dedup from day one. **Use `after:<epoch-seconds>`**, computed per fire, and filter
  client-side on `internalDate`. Verify against the live API before relying on either.
- `format=metadata` only. Never `format=full`.
- Rules in-prompt: VIP senders, and `/invoice|payment|overdue|suspended|deleted|breach|unauthorized/`.
- **Uncertain → MORNING**, always.

**H2 — the loops are NOT decoupled, and the fence is mandatory.** `global_token_budget = 10_000_000`
(`cos.agents.toml:126`) is shared, and ux.8′ made exhaustion **defer rather than brick** — so a triage
misfire at 07:40 would *defer the 08:00 brief*, making the tier that works hostage to the experimental
one. They also share the OAuth broker and cred.7 provider-health state, and 48 authenticated
calls/day multiplies refresh-failure surface.

- Hard sub-budget: the triage job gets an explicit `token_budget` of **≤500k/day**, ~5% of the window,
  so it cannot starve the brief.
- The liveness signal from §0 covers the silent-death case.

### Cost

| Cadence | Fires/day | Tokens/day | % of 10M window |
|---|---:|---:|---:|
| triage loop @ 15 min | 96 | ~355k | 3.6% |
| **triage loop @ 30 min (chosen)** | 48 | **~180k** | **1.8%** |
| full pipeline @ 30 min | 48 | ~6.3M | 63% |
| full pipeline @ 15 min | 96 | ~12.7M | 127% — over |

Per-cycle full-pipeline cost is the **measured** 4.1M ÷ 31 = ~132k. An earlier draft used 300k and
claimed 144% at 30 min; that was wrong, and D2's "architectural, not a tuning problem" was overstated.
The two-loop design stands on a weaker basis: the full pipeline emits a *brief*, not an *interrupt*.

### 6. `H5` — the instrument ships WITH the behaviour change

Non-negotiable, and a reversal of the original plan. `attn.1` must not ship a behaviour change while
deferring its own measure to `attn.2`.

- **The already-handled check moves into this increment.** For each morning-brief item, check
  `threads/{id}` for still-`UNREAD` and for a reply from the operator after the counterparty's last
  message. Verified buildable as-is: no broker change, no new OAuth scope — the broker allowlists query
  params not paths (`cos.agents.toml:100`), `gmail.readonly` covers `threads/{id}`, and `labelIds`
  return regardless of `format`.
- This doubles as the fix for the `brief.2` criterion-1 P1, and its framing must be **"make
  carry-forward work and suppress resolved"**, not "stop re-listing" — the artifact shows 07-23 and
  07-31 both reported *no carry-forward at all* while 07-16 carried 2 items. Carry-forward worked once
  and then stopped (confirms `brief-06`).
- **It is also the fix for C2**, the review's sharpest finding: the plan's own motivating example
  ("GCP upgrade blocked — must do today") sat directly above "GCP billing now live — account upgraded",
  both 🔴, both open. The example was already resolved.

### 7. `M3` — measure recall, because it is currently unmeasurable

The `never` tier **does not exist**: the CoS holds `gmail.readonly`, so nothing archives and nothing
labels. "Skipped (low priority): 30" is a stat line in a markdown file, and those 30 subjects have
never been seen by anyone. So the interrupt tier's *recall* — how many urgent items today's pipeline
drops — is unknown.

**One prompt line: dump the 30 skipped subjects into the brief for the first 14 days.** Costs nothing
and is the only way to learn whether the interrupt rules have a false-negative problem.

### 8. `C4` — the expected-rate check, as an acceptance criterion

The review counted D1-qualifying items across all three briefs at **~3/week** (07-16 ≈3, 07-23 ≈3-4,
07-31 ≈1 and stale). At 48 fires/day that is **~112 fires per genuine hit**, and CLAUDE.md carries a
standing gate: "if it is ~2 actions a morning, build nothing further."

This is not a reason to stop — that was decided at the gate — but it **is** recorded as the number to
check against. After 14 days, if the observed interrupt rate is ≤3/week, the honest conclusion is that
latency was never the binding constraint, and the tier should be retired rather than tuned.

**And D4 must be re-scoped, because as written it cannot pass.** With ~47 of ~50 daily items still
arriving at 08:00 and no `never` tier, "do I stop checking email manually" is structurally guaranteed
to read FAIL regardless of how good attn.1 is. Judging attn.1 by it would retire a possibly-correct
thesis on an underpowered measure — taken, per the briefs, while the operator is at TOKEN2049 and UC
Berkeley. **Revised measure for attn.1 specifically:** of the interrupts delivered, how many did the
operator act on within an hour, and how many did they mark as not worth the buzz. Both come back
through the existing Telegram reply channel. D4's habit measure stays as the *track's* goal, judged
after `attn.2` and the `never` tier exist.

## Alternatives on the record (not chosen, kept because they are cheap and may still win)

| | Alternative | Cost | Status |
|---|---|---|---|
| A1 | Gmail's own filters + phone high-priority-only — D1's rules tier with zero code, zero tokens, zero new third party, and it works while the laptop is shut | 20 min | **Run alongside.** It is free and it is the control group. |
| A3 | Raise the existing pipeline 1× → 3×/day (08/13/18), ~4% of the window, cuts worst-case latency 24 h → 8 h | 4 config lines | Deferred. Reconsider if the 14-day interrupt rate is ≤3/week. |
| A2 | "Make the daily brief unmissable" | — | Superseded by §0 (uptime) + §7 (recall). |

## Test plan

- `publish_interrupt` rejects malformed `thread_id`; accepts valid; idempotent per thread.
- **Rate cap is enforced in the runtime**: 25 candidates in one fire yield ≤1 delivery; ≤3/day holds
  across fires; overflow is recorded and folded into the brief.
- **Quiet hours**: at 03:00 local, only `AccessIncident` with two signals delivers; `MoneyBlocked` with
  one signal does not. Both directions asserted.
- **No sender-controlled text can reach the delivered message** — assert the composed payload contains
  no `subject`/`from`-derived substring, driven from a fixture whose subject contains `[`, `]`, `(`,
  `)`, `|` and a leading `!`. Mutation-verified.
- **Sub-budget fence**: a triage job that exhausts its 500k cannot defer the 08:00 brief.
- **The Gmail query is valid**: assert the constructed URL uses `after:<epoch>` and never
  `newer_than:<n>h`. This is a regression guard for H1.
- Triage-job caps agree across all four config copies (extend `COS_PROMPT_SOURCES` in
  `agentd/src/config.rs`).
- `docker compose config` asserts both schedule modes AND that `restart:` is present on `cos`,
  `qdrant`, `semantic-kb-mcp` and **absent** on `agent`.
- Already-handled check: a thread with an operator reply after the counterparty's last message is
  suppressed; an unread one is not.

**Every guard added here gets a mutation check** — mutate the code to reintroduce the original bug and
confirm the guard fails, then mutate a harmless reformat and confirm it does not. brief.1 produced five
mutation-proven false greens in guards written one round earlier; a guard that has not been
mutation-tested is assumed non-functional.

## Explicitly out of scope

- Learned per-thread urgency — needs the replied-to signal, which §6 creates. Downstream.
- Agent-decided `never` tier — rules only, and note §7: there is no archiving capability at all.
- Channels beyond Telegram. `attn.3` (presence-routed local notify) stays deferred, and §2's
  no-subject payload is what makes that deferral tolerable.
- Brief prettification — though the 37 HTML entities in the 07-31 brief are a real readability defect
  (`Jane Doe &#40;jane@example.com&#41;` is what the operator reads); filed for `brief-04`.

---

# ENG REVIEW (Phase 3, /autoplan 2026-08-01)

## Architecture — the `publish_brief` mirror, and where it stops being one

```
  TRIAGE JOB (new, 48×/day)                       BRIEF PIPELINE (existing, 1×/day)
  ┌──────────────────────────┐                    ┌──────────────────────────┐
  │ cron trigger (30 min)    │                    │ cron trigger (daily)     │
  └────────────┬─────────────┘                    └────────────┬─────────────┘
               │ run_job                                       │ run_job
               ▼                                               ▼
  child_id = "{job_id}-{date}"  ◄── scheduler.rs:2469 ──►  child_id = "{job_id}-{date}"
               │                    DATE-KEYED, NOT PER-FIRE            │
               ▼                                               ▼
  ┌──────────────────────────┐                    ┌──────────────────────────┐
  │ Mcp(google_oauth)        │                    │ inbox → KB → curator     │
  │ InterruptPublish  (NEW)  │                    │ BriefPublish             │
  └────────────┬─────────────┘                    └────────────┬─────────────┘
               │                                               │
               └──────────────┬────────────────────────────────┘
                              ▼
                    ┌────────────────────┐
                    │  run-writer lane   │  runs/mod.rs:138,281 (FIFO, ordered)
                    └─────────┬──────────┘
                              ▼
                    ┌────────────────────┐
          NEW ─────►│ INTERRUPTS  BRIEFS │  runs.redb
                    └─────────┬──────────┘
                              ▼
          GET /api/v1/interrupts    GET /api/v1/brief
                              │
                              ▼
                    telegram_mcp.py  (durable _delivered, :218/:237)

  ══ SHARED, AND THAT IS THE PROBLEM ══
  global_token_budget = 10_000_000        cos.agents.toml:126
  budget_reset_interval = 86_400 (24 h)   cos.agents.toml:127
  OAuth broker + cred.7 provider health   (48 authenticated calls/day)
```

`publish_brief` is a sound **durability** template and a poor **safety** template. It is safe because
agentd authors the factual spine and the model contributes only bounded narrative
(`native.rs:876`, `runs/store.rs:431`). `publish_interrupt` would persist a **model-selected**
`thread_id`, a **model-selected** `tier_reason`, and model-authored `why` — three untrusted claims
where `publish_brief` has none.

## ENG DUAL VOICES — CONSENSUS TABLE

Source: `codex+subagent`.

```
═══════════════════════════════════════════════════════════════════════
  Dimension                              Claude   Codex   Consensus
  ─────────────────────────────────────  ───────  ──────  ───────────
  1. Architecture sound?                 see E1-E4  NO    DISAGREE→see below
  2. Test coverage sufficient?           NO       NO      CONFIRMED
  3. Performance/cost risks addressed?   NO       NO      CONFIRMED
  4. Security threats covered?           NO       NO      CONFIRMED
  5. Error paths handled?                NO       NO      CONFIRMED
  6. Deployment risk manageable?         NO       NO      CONFIRMED
═══════════════════════════════════════════════════════════════════════
```

## E1 (CRITICAL, mine) — one budget deferral silently kills the tier for 24 hours

A compound failure across three mechanisms that are individually correct:

| Step | Mechanism | Reference |
|---|---|---|
| 1 | `child_id = format!("{job_id}-{date}")` — keyed to the **day**, not the fire | `scheduler.rs:2469` |
| 2 | Collision guard rejects if a child with that id is **still live** in `state.agents` | `scheduler.rs:2482` |
| 3 | ux.8′ made budget exhaustion **defer, not terminate** — the agent stays live in `state.agents` while its inference waits in `state.deferred` | `scheduler.rs:106,727` |
| 4 | The deferred inference waits for the window rollover: **`budget_reset_interval = 86400`** | `cos.agents.toml:127` |

So: the triage child is deferred once (global budget pressure, a slow Gmail call, a retry backoff), and
**every one of the up-to-47 remaining fires that day derives the same `child_id` and is rejected**, each
emitting an `EventKind::Error` — with nothing on any operator surface. The interrupt tier dies silently
for up to a day, and the operator's only signal is its absence.

The root cause is named in the code itself. `scheduler.rs:2478-2481`:

> *"a same-day re-trigger re-runs (harmless — brief is log-append/LWW; **cron fires once daily**).
> Same-day idempotency is intentionally not enforced."*

**The `[[jobs]]` collision guard was written under a once-daily assumption. attn.1 violates it at the
foundation.** This is not a bug in cap.2b; it is attn.1 using a mechanism outside its design envelope.

**Fix:** make the triage job's child id per-fire, not per-day (`{job_id}-{date}-{HHMM}` or a monotonic
seq), **and** add the §0 liveness signal so a stalled loop is visible. Do not paper over it with a
longer cadence.

**Corrections to my own analysis, recorded:** I first hypothesised that fires 2–48 would be rejected
outright. That is wrong — `handle_agent_terminal:1253` removes a completed child from `state.agents`,
and because it is in `awaiting` it never reaches `state.outcomes` (the `else` at `:1346` catches only
non-children), so same-day re-runs work when the prior fire **completed**. Codex was right on that
point. I also worried run history would collapse to one row/day; also wrong — `RunTracker` segments by
`"{agent_id}:{segment_seq}"` (`runs/mod.rs:32,68`), so the 48 fires stay distinguishable. Only the
**still-live** case bites, which is exactly the deferral case.

## E2 (CRITICAL, Codex, verified) — the ≤500k/day budget fence does not exist

The plan's load-bearing safety claim. `run_job` sets `token_budget: job.token_budget`
(`scheduler.rs:2498`) — that is **per spawned `AgentTask`, per fire**, not per job per day. Same-day
re-runs are allowed by design (`:2478`). So 48 fires × 500k = **up to 24M against a 10M global
ceiling**, bounded only by the shared global budget. And once global is exhausted every agent defers
(`:1841`) — which is E1's trigger, so E2 causes E1.

**A per-job `token_budget` is not a fence. It is a per-fire cap.**

**Fix:** durable per-job/day accounting in the runtime, or a genuinely separate budget pool for the
triage lane. Until one exists, the claim "the triage job cannot starve the 08:00 brief" must be struck
from the plan — it is false, and it is the reason the whole two-loop design was called safe.

## E3 (CRITICAL, Codex + verified) — quiet hours would run on UTC

`§2`'s locked rule is "outside 07:00–23:00 **local**, only `AccessIncident`". Nothing in the stack can
express that:

- No `chrono::Local` / `localtime` use anywhere in `agentd/src` — **6** `Utc::now()` call sites, zero local.
- No `tzdata` installed in the image and **no `TZ` in the `cos` environment** (`docker-compose.yml:30`).
- `run_job` stamps dates with `chrono::Utc` (`scheduler.rs:2468`); `cron_mcp.py` is UTC-only by design (`:14,:279`).

A naive `07:00–23:00` check therefore means **UTC**, shifting the operator's quiet window by 7–8 hours
and inverting it: 03:00 Pacific is 10:00 UTC, squarely inside "awake". **The single decision the plan
locked to prevent a 03:00 buzz would cause one.** DST makes it worse.

**Fix:** an explicit configured IANA timezone (config field, not env inference), `tzdata` in the image,
and tests at both boundaries **and** across a DST transition.

## E4 (HIGH, Codex) — prompt injection can publish a false interrupt

The triage job holds `Mcp(google_oauth)` + `InterruptPublish`, and `oauth_call_api` permits broad
Gmail-host calls subject only to a host allowlist (`oauth_mcp.py:620`). M4's no-subject payload fixes
*confidentiality* and does nothing for *integrity*: an injected email can instruct the agent to call
`publish_interrupt` with a plausible `thread_id`, the highest `tier_reason`, and attacker-chosen `why`.

Rate limits bound blast radius; they do not preserve correctness. And an interrupt is a **higher**-trust
surface than the brief — it arrives with implied authority, out of band, at any hour.

**Fix:** treat `tier_reason` as an untrusted claim. Either the runtime independently re-checks the
thread's Gmail metadata against the rule inputs before delivering, or the delivered message states the
tier is model-asserted. Add `AccessIncident` (the only tier that pierces quiet hours) to a
runtime-verified-only set.

## E5 (HIGH, Codex, verified) — `publish_interrupt` must join `PROTECTED_TOOLS`

`tools/mod.rs:97` lists exactly four: `request_approval`, `spawn_agent`, `send_message`,
`publish_brief`. The comment above it states the reason precisely — the central invoke hook emits the
operator-trust event **by tool name**, so an MCP override could emit it "without ever persisting a
brief." An interrupt event is strictly more trusted than a brief event. Omitting it reintroduces a
vulnerability that was already found and fixed once, by review, on the tool this one is modelled on.

## E6 (HIGH, Codex) — idempotency/rate state cannot live in the sidecar

The plan puts dedup in both runtime and sidecar with "runtime wins". Correct instinct, but the sidecar
half is not durable: `telegram_mcp.py:237` degrades to memory-only when the state path is not writable,
and the default depends on env (`:517`). The only correct point is inside the `publish_interrupt` write
transaction in `runs.redb`, beside `INTERRUPTS` — before delivery fanout and before prompt-driven
retries.

Consequence the plan misses: if `/api/v1/interrupts` returns "undelivered", the runtime **cannot know
delivery status** — only the sidecar can. That needs an explicit **delivery-ack or lease** model, or
interrupts will re-deliver after a sidecar restart, or be lost.

## E7 (MEDIUM, Codex) — `after:<epoch>` passes the broker but the agent cannot compute it

Good news: `q` is allowlisted (`cos.agents.toml:99`) and the broker forwards whole `q=` pairs without
parsing values (`credential/mod.rs:1135`), so `q=after:<epoch>` is not blocked. Existing tests cover
only `newer_than:1d` (`credential/mod.rs:3008`).

The real gap: **a prompt-driven agent cannot reliably compute epoch seconds**, and there is no clock
tool. **Fix:** have agentd author the window — inject `window_after_epoch` into the rendered job task
(`job.render(&date)` already substitutes server-stamped values, `scheduler.rs:2493`), or author the
whole Gmail URL in the runtime. Do not ask the model to do arithmetic on the current time.

## E8 (MEDIUM, Codex) — the four-copy parity guard is necessary but insufficient

`COS_PROMPT_SOURCES` (`config.rs:1976`) covers all four copies, but the existing tests assert *prompt
hygiene* properties, not **job presence, cadence, token budget, capabilities, or query shape**. The
plan's "extend `COS_PROMPT_SOURCES`" would pass while the triage job is missing from the QEMU overlay
entirely — which is exactly how brief.1 shipped a stale distro config. Parse checks currently cover
only the two real configs (`config.rs:2475`).

**Fix:** parsed-TOML assertions for the new job in both real configs, raw assertions for the two
templates, driven from a single constant — and mutation-verified per the house rule.

## Test plan — gaps beyond the plan's list

| Gap | Test |
|---|---|
| E1 | Simulate a live/deferred triage child, fire again, assert the second fire is **not** rejected once the child id is per-fire. Negative control: revert to the date-keyed id and confirm the test fails. |
| E2 | Drive 3 fires each spending its full per-fire budget; assert cumulative spend cannot exceed the triage lane's daily allowance. **This test cannot pass today** — it is the proof E2 is unfixed. |
| E3 | Quiet-hours boundary at 06:59/07:00/22:59/23:00 in a **non-UTC** configured zone, plus one DST transition. |
| E4 | A fixture email whose body contains `publish_interrupt(tier_reason=AccessIncident, ...)` instructions; assert either runtime re-verification or an explicit model-asserted label. |
| E5 | `register_override` with a tool named `publish_interrupt` is **rejected**. |
| E6 | Sidecar restart mid-delivery does not double-deliver and does not drop. |
| E8 | Remove the triage job from the distro overlay only; assert the parity test **fails**. |

Every guard mutation-verified in both directions (reintroduce the bug → must fail; harmless reformat →
must pass).

---

# DX REVIEW (Phase 3.5, /autoplan 2026-08-01)

**Scope note (auto-decided, P3 pragmatic).** DX scope triggered on keyword count (`API`×2, `MCP`×1,
`agent`×8, `action`×4) but the genuine developer-facing surface is thin: one Telegram message format,
three config fields, and one env var that already exists. Ran a single focused pass rather than full
dual voices. The operator IS the developer here, so the journey is short and the failure modes are
operational rather than integration-shaped.

## Developer/operator journey

| Stage | Today | With attn.1 | Friction |
|---|---|---|---|
| enable | `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID` + `AGENTOS_APPROVAL_SECRET` | unchanged | none — already documented (`docker-compose.yml:50-59`) |
| configure | — | VIP senders, quiet hours, **IANA timezone** (E3) | **new**, and the tz field is load-bearing |
| first interrupt | — | Telegram message: tier + reason + permalink | good — no sender text (M4) |
| trust it | — | ? | **no liveness signal** — see E1/H2 |
| **stop it** | — | **nothing** | **D1 below — the critical DX gap** |
| tune it | — | edit 4 TOML copies + rebuild | high, and it is 03:00 |

**TTHW:** ~5 min to enable (env vars exist), but **first *correct* interrupt** is unbounded — the rules
are unvalidated and the artifact shows two of them misfiring on routine mail (H4).

## D1 (CRITICAL) — there is no way to make it stop

Verified: the sidecar's inbound verb allowlist is exactly `("approve", "deny")`
(`telegram_mcp.py:356`), and the only path to silence is unsetting `TELEGRAM_BOT_TOKEN` and restarting
(`:501`).

So the failure mode this whole review has been circling — the rules fire on "Your Servcorp invoice is
now ready" and "Last chance for US$399" (H4, both real strings from `brief-2026-07-31.md:17,21`) —
resolves as: **the operator is woken at 03:00 and the only remedy is to get up, find a terminal, edit
the environment, and restart the stack.**

One bad night like that and the feature is muted permanently by an operator who no longer trusts it,
which is D4 failing for a reason that has nothing to do with the classifier.

**Fix, and it is cheap because the two-way relay already exists.** Add inbound verbs beside
`approve`/`deny`, using the same parser (`:349`) and allowlist (`:356`):

- `mute 8h` / `mute today` — stop interrupt delivery, keep the morning brief. Must be **durable**
  (survive sidecar restart) and must **auto-expire**, so a panicked 03:00 mute does not silently
  disable the tier forever.
- `mute` with no argument → default 8h, and the confirmation states the expiry time explicitly.
- The mute state belongs in the runtime (`runs.redb`), not the sidecar — same reasoning as E6, since
  `telegram_mcp.py:237` degrades to memory-only when its state path is not writable.

This is a **safety control**, not a convenience: it is the operator's only brake on an autonomous
process that has been granted permission to wake them.

## D2 (HIGH) — the interrupt message must say what it cannot do

With M4's no-subject payload, the message is tier + reason + permalink. An operator reading
"🔴 AccessIncident — money movement blocked" at 03:00 has no way to know the tier is a **model
assertion** (E4) rather than a verified fact. Given E4 is unfixed, the message must not present a
model claim in the register of a system alert.

**Fix:** the composed message names its own provenance in one short line, and the tier vocabulary avoids
implying verification until the runtime actually verifies it.

## D3 (MEDIUM) — error messages for the three new failure paths

The repo's own standard (from prior DX work) is problem + cause + fix. The new paths:

| Path | Required message |
|---|---|
| triage fire rejected by the collision guard (E1) | name the live child, its age, and that the loop is stalled — **not** a bare `EventKind::Error` |
| quiet hours suppressed a delivery | record it, so a missed urgent item is explainable after the fact |
| rate cap folded an interrupt into the brief (H3) | the brief must say so, or a suppressed interrupt looks like a missed one |

All three are currently silent, and silence on these paths is what makes E1 invisible.

## D4 (MEDIUM) — the tz field is a new footgun

E3 requires an explicit IANA timezone. If it is optional with a UTC default, every operator who does
not set it gets quiet hours shifted 7–8 hours — the exact bug E3 identifies, now as a default rather
than an oversight.

**Fix:** make it **required** when the triage job is enabled, and fail the boot gate (`agentd check`)
if the job is declared without it. cap.1's `CapabilitiesResolved` fail-closed boot gate is the
precedent.

## DX scorecard

| Dimension | Score | Note |
|---|---:|---|
| Getting started | 8/10 | env vars already exist and are documented |
| API/CLI naming | 7/10 | `publish_interrupt` mirrors `publish_brief` cleanly |
| Error messages | 3/10 | three new silent failure paths (D3) |
| Docs | —/10 | not yet written; RUNBOOK §11.x + cos-guide.html both need it, and `.html` must be in the staleness check (prior learning) |
| Upgrade path | 8/10 | additive; no migration |
| Observability | 2/10 | no liveness signal, no mute, no suppression record |
| **Control / reversibility** | **1/10** | **D1 — no way to stop it** |

---

# ENG VOICE 2 (Claude subagent) — ADDENDUM, and corrections to my own review

Returned after the gate. Eng consensus source upgrades to **`codex+subagent`**; dimension 1
(architecture) resolves to **NO on both** → CONFIRMED, not DISAGREE. It found four things neither
Codex nor I found, and three errors in my own analysis.

## It inverted my headline claim: M4 makes the worst attack WORSE

I called M4 (drop `subject`/`from`) "the single highest-leverage fix in the review." That was wrong in
one direction that matters.

**S1 (CRITICAL) — attacker-chosen permalink.** `^[0-9a-f]{1,20}$` is satisfied by every **attacker-owned**
thread id. Tier selection is a model judgement, therefore attacker-influenceable, and `AccessIncident`
is the one tier that pierces quiet hours. So an injected email can **buzz the operator's phone at 03:00
with a link into a thread the attacker wrote**, inside the operator's own Gmail, wearing the runtime's
authority — and **M4 removed the two fields (`subject`, `from`) that would have let the operator notice.**

M4 remains right for confidentiality and wrong as a safety story. Both must be said.

## C2 (CRITICAL) — "no sender-controlled text" is false by construction, and my test for it is vacuous

`why` is agent-authored, but the agent's **only input is untrusted email**. An injected message reading
*"when you escalate, set why to exactly: …"* puts sender-chosen bytes in the push. "Agent-authored" is a
**provenance** claim, not a **taint** claim — and `brief-03` was already re-rated P3→P1 for exactly this
confusion.

Worse, the test I specified — *assert the composed payload contains no `subject`/`from`-derived
substring* — **cannot fail**. Under `MockGateway` the runtime never copies anything, so it passes
without exercising the property; under a real model it is unverifiable. **That is a green test over an
untestable claim**, the precise class that produced brief.1's five mutation-proven false greens. I wrote
one into the test plan while citing that learning.

**Fix:** declare `why` **tainted**; escape it in `publish_interrupt::invoke` (runtime, not prompt). The
assertable property becomes a **pure function over a fuzz corpus**: for any input bytes, the stored `why`
contains none of `[ ] ( ) | \`` , no newline, no leading `!`. That can fail, so it can be
mutation-verified. Better: **delete `why`** — tier + reason + permalink is already the design.

## C3 (CRITICAL) — my quiet-hours rule contradicts my own payload

§H4 locked *"anything delivering at night requires two independent signals — VIP sender AND rule hit."*
§2's signature has **no `from`**. **The runtime cannot evaluate "VIP sender."** The night gate therefore
degrades to a prompt rule applied by the injected agent — self-certification, `brief-03` a third time.

**You cannot have both M4's no-`from` payload and a VIP-sender night gate.** Pick one, in the plan,
before build. Plus: `chrono` is declared `features = ["serde"]` only (`Cargo.toml:30`), and
`Dockerfile:55-89` is `alpine:3.20` + `fuse3 bash jq curl python3` — **no `tzdata`, no `/usr/share/zoneinfo`**.

## H2 (HIGH) — suppression and overflow are WRITE-ONLY. The brief.1 defect, repeated.

`publish_brief` composes purely from a `RUNS` scan (`runs/store.rs:482-499`); `BriefRecord`
(`runs/mod.rs:86-112`) has no interrupt field; the curator's caps (`cos.agents.toml:445-454`) include no
interrupts read; the only planned reader is the sidecar's HTTP poll.

⇒ A rate-capped or quiet-hours-held interrupt is stored and surfaced **nowhere**. §2's "overflow folds
into the morning brief" is **unimplementable in scope**, and §8's acceptance criterion (observed
interrupt rate) has nothing to read. CLAUDE.md's own warning: *"trace the READ path before believing
any KB re-key claim."* I quoted that learning in this very plan and then repeated the defect.

## H3 (HIGH) — the sidecar redelivers interrupts forever

`telegram_mcp.py:317-330`: every `poll_management()` builds `pending_ids` from `GET /api/v1/approvals`
and then **deletes every `_delivered` key not in that set.** Interrupt keys are not approval ids →
evicted each poll → **redelivered forever.** The runtime rate cap does not help: it caps *publish*, not
*delivery*. I cited `:218` — which is only the `global` declaration in `_load_state`. The GC at `:326`
is the part that matters and I did not read it.

## H4 / H5 (HIGH) — table shape and the clock trap

- **`BRIEFS` is seq-keyed and never pruned**: `prune()` (`store.rs:319-351`) touches only `RUNS`;
  `MAX_RUNS`/`MAX_RUN_AGE_SECS` apply there alone. Fine at 1 brief/day, not at 48 fires/day × ≤25
  candidates. Per-`thread_id` idempotency also becomes a **full scan per publish on the writer lane**.
  Needs two tables (`INTERRUPTS` seq→record, `INTERRUPT_BY_THREAD` thread_id→seq) + retention, decided
  **before** build — retrofitting means bumping `RUNS_SCHEMA_VERSION` (`:28`), for which **there is no
  migration path.**
- **The clock fails CLOSED.** A `≤1/hour` limiter storing `last_ts` breaks on a forward jump: after NTP
  corrects, `now.saturating_sub(last) == 0` → **no interrupts at all, silently, no visible cause.**
  `publish_brief` already carries the scar for this exact shape (`store.rs:459-462`). And
  `RunsStore::open` quarantines a corrupt file and opens fresh (`:109-129`) → the limiter **silently
  resets to 0 used**, on the control the plan calls a security control.

## Corrections to my own review (3)

1. **My citations were wrong.** `main.rs:1536` is the **sandbox-rule** match in `caps_to_rules_inner`,
   not tier legality. Tier legality is `capability.rs:486-514` and needs **two** arms (`:493` Agent,
   `:509` StdioMcp) because it deliberately has no wildcard. Full compile-forced blast radius of one new
   variant: `capability.rs` `satisfies` (~`:330`), `capability_covered_by` (`:391`), `tier_legality`
   (`:493`,`:509`); `main.rs:1537`; `agentctl/src/spawn.rs:243,:333,:348`;
   `agentctl/src/watch/spawn.rs:56-72`. `management.rs:67 is_privileged_spawn_cap` is deny-by-default
   and needs **no** change.
2. **"Four copies" is wrong for the job.** `[[jobs]]` appears in **2** files (`agentd/cos.agents.toml`,
   the distro overlay); **all 18 templates have 0** — they are standalone single-agent specs. So my
   test-plan item "triage-job caps agree across all four copies (extend `COS_PROMPT_SOURCES`)" is
   **unwritable**: `COS_PROMPT_SOURCES` already lists all four and needs no extending, and two of them
   have no jobs at all. Replace with a **2-source jobs-parity** assertion.
3. **H6's regression guard has nothing to assert against.** No Rust code builds a Gmail URL — the model
   does, from prompt text (`cos.agents.toml:369-375` is an instruction). The enforceable form is a
   **prompt-source** guard: assert no `newer_than:\d+[hms]` across `COS_PROMPT_SOURCES`.

## Also new

- **M6 — 48 triage children/day swamp the existing brief.** `publish_brief` counts every run terminal in
  window and sums `spend` (`store.rs:489-504`), including `still_running` unconditionally (`:492-494`).
  That corrupts `run_count`, `spend_total` and the attention list — **on the very surface §0 uses for its
  liveness signal.** Exclude triage children by identity (the `config_seed` escape hatch at `:494`).
- **M8 — the broker has no per-agent cap here.** `[credential_gateway.providers.google]` sets no
  `max_requests_per_agent` and there is no `caps_db_path`; `None` ⇒ unlimited
  (`credential/mod.rs:1081`). 48 fires/day is an unbounded multiplier on Gmail quota and on the shared
  cred.7 health state. **One config line.**
- **M7 — the new route is unauthenticated.** `approval_token_ok` (`management.rs:109-117`) gates only
  approve/deny; GETs are open (`:985`). Acceptable *only* because M4 stripped subject/from — so if `why`
  survives C2, this publishes sender-influenced text unauthenticated.
- **M5 — no event kinds planned**, and `agentd/tests/conventions_completeness.rs` **hard-fails** until
  new kinds are backtick-documented in `CONVENTIONS.md`. This is a build gate, not a nicety.
- **M9 — there is no compose test infrastructure**: nothing in `agentd/tests/` or `.github/workflows/`
  references `docker-compose.yml`, and there is no YAML crate. Use `include_str!` text assertions in the
  `distro_packaging.rs` style; do **not** add a YAML dependency.

## Its one unambiguous agreement

> *"§1 landing standalone is right and is the one part of this plan I would ship today."*

Three voices, three phases, same conclusion.
