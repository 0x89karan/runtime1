<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/ux.2-observe-autoplan-restore-20260712-230424.md -->
> **⚠ SUPERSEDED (2026-07-13).** At the Phase 4 gate, the user chose to reframe this
> increment toward Codex's "Attention & Evidence" (approvals/outcome-centric) alternative
> raised during CEO review, rather than ship this "Observe" framing. This document is
> preserved as a complete historical record — it found and fixed several real engineering
> bugs (see "Engineering corrections" and "Eng Review revises/reopens Correction #1/#2")
> whose underlying code-level findings remain valid even though the increment's scope
> changed. **Do not implement from this file.** See `docs/plans/ux.2-attention-evidence.md`
> for the current, active plan.

# ux.2 — Observe: per-agent activity + live stream (closes cos-ux-01)

**Branch:** ux.2-observe · **Base:** main (v0.82.0) · **Track:** Cockpit (agentctl-client)
**Sequencing:** ux.0 → ux.9 → **ux.2** → ux.1 → ux.8 → ux.3 (per `docs/ROADMAP.md`)

## Goal

Answer "what is each agent doing right now / who needs attention" at a glance, without opening
`flight.jsonl` or the Inspector view. Closes **cos-ux-01** (TODOS.md): during long-running agent
turns (e.g. the CoS inbox agent fetching 20 Gmail messages), `agentctl watch` today shows only
`running` status and a growing context-size counter — no indication of what tool was last called,
what it returned, or whether errors occurred.

## Rough scope (from `docs/plans/ux-cockpit.md`, lines 137-166 — the pre-existing cockpit master plan)

- **Snapshot fields** (`surfaces::AgentSnapshot`): `last_activity` (tool name + truncated arg
  summary + result summary + timestamp), `last_error` (`is_error` result or `capability_denied`),
  `error_count`, `idle_secs` (time since last event). Populate from the same places the scheduler
  already emits flight events for tool calls; redact secrets — never surface a credential-shaped
  token in a preview. Live-refine `last_activity`/`idle_secs` from the SSE feed between polls.
- **Agent table** (the cockpit home, `View::Dashboard`): columns
  `AGENT STATUS TURN LAST-TOOL TOKENS $ AGE ⚠`. `LAST-TOOL` renders readably
  (`web_search("q3 revenue…")` / `oauth_call_api → 200` / `⚠ mcp_error: timeout`). Color-encode:
  status dot (running=green, waiting=cyan, error=red, terminated=grey); budget bar reusing the
  existing `MemoryPressure` 75/90 threshold colors; **whole row red on error**; `idle Ns` flips
  amber past a threshold (the "is it stuck" signal). Glyph + color always (`--plain` safe).
- **Live event stream pane:** summary-first one-liners (`HH:MM:SS [agent] ICON summary`), raw JSON
  on `Enter`/expand only. Reuse the Inspector filter model (All/Errors/Sandbox/CapDenied) as toggle
  chips with a visible active-filter indicator. Selecting an agent row **scopes** the stream to that
  `agent_id` (k9s follow-selection). `f`/`Space` freezes auto-scroll; a `▼ N new` affordance when
  frozen/scrolled-away.
- **AgentDetail:** activity timeline (last ~10 readable events for that agent) + persistent error
  strip + `TURN n · infer 2.3s · tool 0.4s · idle 12s` line. Reuse `short_term_previews`.

**Acceptance (from the master plan, amended per Engineering Corrections + Design Review —
amendments marked):**
- [ ] Each agent row shows its current tool call readably; no raw JSON in the table. Amber
      idle marking **augments** this text (`⏳ oauth_call_api(...) 34s`), never replaces it
      *(amended: Design Review CRITICAL finding)*.
- [ ] When an agent hits an error (e.g. "Not authenticated"), its row goes red and the error is
      visible without opening Detail or reading `flight.jsonl`.
- [ ] `idle_secs` past a **30-second default threshold** (named const, per-tool-class tuning
      deferred to TODOS.md) renders amber; a hung vs busy agent is distinguishable; `Waiting`-
      status agents are never marked amber regardless of `idle_secs` *(amended: threshold value
      + Waiting carve-out now explicit, were previously unstated)*.
- [ ] Selecting a row scopes the stream; `f` freezes/unfreezes-and-jumps-to-bottom; `▼ N new`
      shows when frozen. **Works over both transports**, by different mechanisms suited to each:
      HTTP/management-API mode consumes the SSE `AppEvent::Flight` stream `pump.rs` already
      produces (built in ux.0, zero new agentd-side plumbing); FUSE mode re-polls the flight-log
      tail on a short timer (reusing Inspector's `read_flight_tail`, since FUSE has no live-push
      mechanism). Neither mode shows a placeholder — both render real events, at different
      freshness (push vs ~1-2s poll) *(amended: Eng Review reversal — see "Eng Review reopens
      Correction #2" below; supersedes the Design-phase FUSE-only wording, which was based on
      an incomplete premise)*.
- [ ] Secrets never appear in `last_activity`/stream previews (test with a credential-shaped tool arg) —
      reworded per Engineering Correction #3: **known credential-shaped patterns are redacted**
      (best-effort, not an exhaustive guarantee). **Pinned (DX Review, Codex finding):** the
      visible replacement text is the literal string `[REDACTED]`, not a blank/empty run — an
      operator must be able to tell "hidden on purpose" from "missing or malformed," matching
      why `sanitize()` (`views.rs:17-22`) already strips rather than silently empties.
- [ ] `--plain` mode conveys the same state via glyphs, including the budget bar
      (`[==.]`/`[=..]`/`[...]`) and idle-stuck marker (`[STUCK 34s]`). **Amended (DX Review,
      both models independently converged — the strongest finding of the DX pass):** `--plain`
      is a non-interactive, one-shot-per-interval full snapshot dump (verified: `mod.rs:95-112`,
      `render_plain` at `views.rs:1110-1169`) — there is no selection/freeze/scroll state at all
      today, so "selecting a row scopes the stream" has no literal analog in `--plain`. Resolved
      scope: each interval's `--plain` dump appends (1) an unscoped "recent events" block — the
      last ~5 flight events across all agents, one flat line each, after the per-agent rows
      (the plain-mode equivalent of the stream pane, sized down since there's no
      scroll/freeze concept in a one-shot dump); (2) each agent's own row gains an inline
      "last ~3 events" suffix (the plain-mode equivalent of AgentDetail's timeline, since
      `--plain` never switches views — confirmed no view-switching exists in one-shot mode —
      so timeline data must fold into the one screen `--plain` has, not a separate detail
      screen). This is real information parity, not new interactivity `--plain` can't support.
- [ ] **New:** at 60-79 terminal columns, `$` then `TOKENS` then `AGE` drop in that order before
      any other column is affected; below 60 columns `--plain`'s narrower format takes over.
      **Pinned (DX Review, both models converged on this being underspecified):** `LAST-TOOL`
      truncates to a fixed 40-char max (including a trailing `…`) regardless of which other
      columns have already dropped — verified the current table (`views.rs:137-149`) has zero
      existing width-conditional column-dropping logic (only a binary too-narrow-to-render guard
      elsewhere, `MIN_TOPOLOGY_WIDTH`/`views.rs:365-372`), so this is genuinely new rendering
      logic inside the existing table-render function, not a reused pattern — sized into the
      same file already counted in the 10-file effort tally, not an 11th file.
- [ ] **New:** freezing the stream pane, then selecting a different agent row, **resets the
      freeze and re-scopes to the newly selected agent** (see resolution below) — not left to
      implementer discretion.
- [ ] **New:** `error_count`/`last_error_at` render as a single compact string
      (`⚠ 3 · 2m ago`), not two separately-formatted fields.
- [ ] **New (DX Review, Claude subagent finding):** the Dashboard shows a one-line `Legend:`
      row for the 3 new visual conventions (row-red=error, idle-amber=possibly-stuck,
      `⚠ N · Ns ago`=error count/recency) — matching the existing precedent set by System/
      Sandbox (`views.rs:340,412-418`) and Topology (`views.rs:374-383`), each of which already
      carries an equivalent legend for their own information density. Cheap, in-blast-radius,
      and consistent with how this codebase already treats "explain the color-coding" as a
      required element, not optional polish.

## What already exists (code-verified, not assumed)

- `surfaces::AgentSnapshot` (`surfaces/src/snapshot.rs:150-185`) has NO `last_activity`,
  `last_error`, `error_count`, or `idle_secs` fields today. Existing fields: `id`, `status`, `turn`,
  `context_tokens`, `token_budget`, `task_preview`, `tools`, `short_term_previews`, `parent_id`,
  `accessible_server_names`, `capabilities_unrestricted`, `tier`, `pid`, plus cred.5's 4
  credential-grant fields.
- `agentd/src/scheduler.rs:2052` `update_snapshot()` is a **pull-based projection**: every call
  derives a fresh `AgentSnapshot` from `AgentTask`'s current in-memory state (`task.turn()`,
  `task.context_tokens()`, etc.) — it does **not** read flight events at all today. This is the
  established pattern new snapshot fields follow: track state on `AgentTask` itself (updated at the
  same call sites that already emit the corresponding flight event), then have `update_snapshot()`
  read it off `task` like every other field.
- `agentd/src/agent/mod.rs:857` `truncate(s, max_chars)` — existing unicode-safe char-truncation
  helper, already used for `ToolUse.input_preview`, `ToolResult.preview`, `AgentFailed.error`, and
  `task_preview`. Reusable for building `last_activity`'s preview text.
- **`agentctl/src/watch/reader.rs:114` `AgentInfo`** (FUSE reader-side mirror of `AgentSnapshot`) —
  same gap, no activity/error/idle fields; `read_agent_info()` reads one virtual file per field
  (`status`, `context_size`, `budget`, `tools`, `parent`, `tier`, `pid`) — a new field means a new
  FUSE virtual file + a new `read_trimmed()`/`read_json()` call here.
- **`agentctl/src/watch/reader.rs:234` `count_egress_by_agent()`** — the one place today that
  *does* scan `flight.jsonl` client-side for a per-agent metric (egress brokered/denied counts).
  This is a narrow, existing exception to the "scheduler tracks it" rule above, used because egress
  counts live in the credential gateway, not `AgentTask`. Not a template to copy for `last_activity`
  (that needs to update on every tool call across every agent, every scheduler tick — an O(file
  size) scan per poll would not scale the way it does for a start-of-session-to-now cumulative
  count).
- **`View::Inspector`** (`agentctl/src/watch/inspector.rs`, `[i]` key) — already tails the full
  flight log with a filter model (All/Errors/Sandbox/CapDenied) + substring search + color-coded
  body. This is the direct precedent for ux.2's "live event stream pane" — the filter-chip UX and
  color-coding should be reused, not reinvented; the new work is agent-scoping (selecting a row
  scopes the stream) and summary-first rendering (one-liners with JSON-on-expand, vs Inspector's
  always-visible body).
- **`View::Memory`**'s pane-scroll model (`agentctl/src/watch/memory.rs`) — the closest existing
  precedent for a scrollable, filterable, freeze-able pane inside the TUI; worth checking before
  inventing new scroll-state plumbing for the event stream pane.

## Engineering corrections — found by CEO dual-voice review, code-verified

**These correct the "What already exists"/Approach A description above; they are load-bearing,
not taste calls — each is a concrete bug in the plan's own mechanism, verified by reading the
actual call sites, not asserted.**

1. **⚠ REVISED BY ENG REVIEW (see "Eng Review revises Correction #1" below) — the original fix
   location in this correction is architecturally unimplementable.** Original text (kept for the
   audit trail, not a live instruction): "the `ToolCall` site (mod.rs:808-813) must ALSO update
   `last_activity`." Both Eng-phase dual voices (Claude subagent + Codex), working independently,
   caught that `run_tools_sequential` is a free `async fn` with no `&mut AgentTask`/`self` in
   scope at all — it cannot write to the struct the plan needs it to update. The corrected fix
   (moving the update to the `AgentEffect::CallTools` dispatch site in `scheduler.rs`, which does
   hold `&mut state` synchronously, before the tool-call future is spawned) is specified in the
   Eng Review section below. The underlying problem this correction identified — that
   ToolResult/AgentFailed-only updates make `idle_secs` climb the entire time a call is in
   flight, wrong for the plan's own motivating slow-Gmail-fetch example — still stands and is
   still fixed, just via a different mechanism than originally written here.
   `spawn_agent`/`send_message`/`request_approval`'s separate interception before
   `run_tools_sequential` (mod.rs ~651-761) is unaffected by this revision — they still need
   their own coverage via their own existing flight events, as originally noted.
2. **⚠ REVERSED BY ENG REVIEW (see "Eng Review reopens Correction #2" below) — the premise of
   this correction (FUSE has a live stream mechanism to extend; HTTP doesn't) is backwards.**
   Original text (kept for the audit trail): "`View::Inspector`'s reuse target is FUSE-only
   today... either (a) explicitly scope the stream pane as FUSE-only... or (b) add SSE-wiring
   for HTTP-mode." Codex's Eng review found `agentctl/src/watch/pump.rs` (shipped in ux.0,
   already on `main`) already spawns an SSE producer for HTTP sources that parses flight events
   into `AppEvent::Flight` — explicitly commented "ux.2 renders it." Verified directly: `source.rs`'s
   `HttpSource::event_stream_url()` returns `/api/v1/events`; `pump.rs:76-112` wires it into
   `spawn_producers`. Meanwhile `View::Inspector` — this correction's original FUSE-side
   precedent — loads `flight.jsonl`'s tail **once per view-entry and never re-polls** (confirmed
   independently by the Claude subagent, `app.rs:561` / `inspector.rs:84-90`); it is not a live
   view at all. So the actual gap is the opposite of what this correction assumed: HTTP mode
   already has real live-push plumbing built and waiting; FUSE mode has no live-tail mechanism
   whatsoever. The resolution below (line ~553) is superseded accordingly.
3. **The redaction acceptance criterion overclaims a guarantee this codebase has already had to
   walk back once.** "Secrets never appear in `last_activity`/stream previews" implies
   completeness a finite regex list cannot deliver (an unanticipated credential shape — GitHub
   `ghp_`, Google `AIza...`, a new MCP provider's token format — passes through untouched).
   This repo already de-claimed an equivalent overclaim once: cred.3.1's S2/S3 removed
   `content_audited: true` from `EgressBrokered` and added THREAT_MODEL §8.7 stating "egress
   content audit NOT implemented." **Fix:** reword the acceptance criterion to "known
   credential-shaped patterns are redacted before display" (best-effort, not exhaustive) and
   add a one-line THREAT_MODEL note now, not after a future audit catches it.
4. **Idle-amber threshold doesn't carve out intentionally-idle `Waiting`-status agents.**
   Orchestrated agents (orch.1/orch.2) can sit in `AgentStatus::Waiting` for arbitrarily long
   periods by design — that's the feature, not a problem. Without a carve-out, every long-parked
   orchestrated agent shows a persistent false "might be stuck" signal, undermining trust in the
   exact indicator this increment exists to make trustworthy. **Fix:** exempt `Waiting` status
   from the idle-amber color rule (still show the raw `idle_secs` number, just don't flag it
   amber); use a distinct visual treatment (e.g. cyan, matching the existing `waiting=cyan`
   status-dot convention already in the master plan's scope text).

**Considered and declined (logged, not silently dropped):** unifying the new redaction function
with `otel/src/span_builder.rs`'s existing `redact_previews` boolean was raised (Claude subagent
finding #4). Declined for this increment — different crates (`agentd` vs `otel`), different
mechanisms (pattern-based scrub vs blanket on/off), and otel's flag redacts OTLP span attributes
for a different consumer (observability backends, not the operator's own TUI). Sharing a single
`is_credential_shaped()` helper across both is a legitimate future dedup, not free right now (it
would need to live in a crate both `agentd` and `otel` depend on, which doesn't exist today).
Logged to TODOS.md as a follow-up, not blocking this increment.

## Premise correction — found during CEO-phase code verification, not assumed

**The master plan's scope text says "redact secrets (reuse existing redaction)". This is
inaccurate — no general-purpose secret-redaction utility exists in this codebase today.**
Verified by direct search:
- `cred.5-ar-01` (closed v0.75.0, per `CHANGELOG.md`) strips the OAuth token-refresh error body
  specifically (`oauth_mcp.py` / `credential/mod.rs:341,371`) — narrow, not reusable for general
  tool-call arg/result previews.
- `otel/src/span_builder.rs`'s `redact_previews` is a **boolean on/off switch** (redact the whole
  preview or don't) for OTLP span attributes — a different crate, a blunter mechanism, not a
  pattern-based scrubber.
- `docs/THREAT_MODEL.md` §8.7 explicitly states: "egress content audit NOT implemented — no
  credential-shaped-token scanning in tool output" — confirms this gap is a known, already-accepted
  absence, not an oversight to merely "wire up."

This means ux.2's acceptance criterion "secrets never appear in `last_activity`/stream previews"
requires **building a new, scoped redaction function** (detect credential-shaped patterns —
`sk-[a-zA-Z0-9]{20,}`, `Bearer [A-Za-z0-9\-._~+/]+=*`, etc. — applied to the tool-arg/result text
before truncation), not "reusing" something that doesn't exist. This is a real, buildable, bounded
piece of new work — not a blocker — but the plan's premise needs correcting before scoping Eng
effort.

## NOT in scope (deferred)

- ux.1 (Converse/chat rail), ux.8 (budget control), ux.3 (spawn-into-running-instance) — separate,
  sequenced increments per `docs/ROADMAP.md`.
- A general-purpose, broadly-applied credential scanner across all tool output system-wide (the
  THREAT_MODEL-documented gap) — out of scope; this increment only needs the redaction applied at
  the specific point where `last_activity`/stream preview text is constructed, not a system-wide
  egress content audit.
- Web cockpit (ux.5) — this is TUI-only.

---

## CEO Review (Phase 1)

### Pre-review system audit
- `git log --since=30.days --name-only` top hits: `docs/ROADMAP.md`, `CHANGELOG.md`,
  `agentd/Cargo.toml`/`Cargo.lock` (version churn — expected, active ship cadence),
  `agentd/src/main.rs`, `agentd/src/config.rs`, `agentd/src/events.rs`,
  `agentd/src/scheduler.rs`, `surfaces/src/agents_fs.rs`, `agentctl/src/watch/mod.rs`. All
  four of the last five are exactly the files this increment touches — a hot, actively-maintained
  path, not a stale corner.
- No stash, no handoff note, zero `TODO`/`FIXME`/`HACK`/`XXX` markers in `agentctl/src/watch/`
  or `surfaces/src/` — clean starting state.
- Design doc: the only one on disk (`0x89karan-main-design-20260621-235446.md`) is unrelated
  (generic "main" branch naming, dated 3 weeks before this track even started). Not used.
  `docs/plans/ux-cockpit.md` (the cockpit master plan, CEO-reviewed 2026-07-10) serves as the
  de facto prior design thinking for this exact feature — read in full, cited above.

### Taste calibration
- **Well-designed, worth matching:** `agentctl/src/watch/pump.rs`'s producer/consumer model
  (`CHANNEL_CAP=256` bounded `sync_channel`, detached producer threads, `MAX_DRAIN_PER_TICK`
  anti-livelock cap) — exactly the k9s/htop-grade pattern the landscape check below validates
  (bounded per-tick drain, never starves key input/render). `View::Inspector`'s filter-chip model
  (All/Errors/Sandbox/CapDenied) is the direct UI precedent for the event-stream pane's filter
  chips — reuse it, don't reinvent.
- **Anti-pattern to avoid repeating:** `agentctl/src/watch/reader.rs`'s `read_agent_info()` reads
  one FUSE virtual file per struct field via individual `read_trimmed()`/`read_json()` calls —
  correct for today's field count, but adding `last_activity` (a structured multi-part value:
  tool name + arg preview + result preview + timestamp) as more individual flat files would start
  to smell. Worth a single JSON-encoded virtual file for the whole `last_activity` struct rather
  than 4 more flat-string files (see Eng phase for the concrete design).

### Landscape check (WebSearch, 2026)
- **[Layer 1] Tried-and-true:** k9s/htop's core value prop is continuous auto-refreshing
  monitoring — "always know what's running, what changed, what needs attention" — exactly
  cos-ux-01's ask. Standard layout: fixed header + scrollable data + function bar (already
  agentctl's `Dashboard` shape).
- **[Layer 2] Current best practice (2026):** capped refresh rate (15-30 FPS) with differential
  redraws; background ops surface in a status widget and never block the main loop; input
  cancels in-flight animation immediately. `agentctl`'s existing `run_tui_loop` (30ms poll,
  coalesced `app.dirty` redraw, bounded per-tick drain) **already implements this** — confirmed
  by direct code read (`agentctl/src/watch/mod.rs:230-246`), not just claimed. No architectural
  rework needed to hit current TUI best practice; this increment is additive.
- **[Layer 3] First-principles:** the one place this codebase's practice diverges from k9s's
  is *data provenance* — k9s reads live from the Kubernetes API server (always current); this
  cockpit reads from two possible sources (FUSE poll or HTTP/SSE) with genuinely different
  freshness characteristics. `last_activity`/`idle_secs` must be correct and non-misleading in
  *both* modes, not just the FUSE-privileged path — a real design constraint the master plan's
  scope text doesn't call out explicitly (noted below in 0E).

Sources: [K9s](https://k9scli.io/), [Activity Stream design pattern](https://ui-patterns.com/patterns/ActivityStream), [awesome-tuis](https://github.com/rothgar/awesome-tuis)

### Prior learnings
`gstack-learnings-search` returned 0 entries scoped to this repo/branch pattern (`last_activity`,
`idle_secs`, `AgentSnapshot`) — no prior learning to apply. This is genuinely new ground for the
codebase's learnings log, not a repeat of a previously-solved problem.

### Step 0A — Premise Challenge

1. **Is this the right problem to solve?** Yes — cos-ux-01 is a *reported* pain point (live
   dogfooding of the CoS surfaced it, not a hypothetical), and it blocks the stated north star
   ("cockpit is agentos's default operator surface") from being trustworthy: an always-on
   console nobody can read at a glance isn't a console. No reframing yields a dramatically
   simpler solution — the alternative ("just tail flight.jsonl yourself") is the status quo this
   increment exists to fix.
2. **What is the actual outcome?** An operator glancing at the Dashboard can answer "is anything
   stuck or broken?" in under 2 seconds, without opening Detail or a second terminal. The plan's
   scope (table columns + color coding + stream pane + AgentDetail timeline) is the direct path
   to that outcome — no proxy metric substituted.
3. **What happens if we do nothing?** Real, already-observed pain (per `cos-ux-01`'s own text:
   "During long-running agent turns... watch shows only running status and a growing context-size
   counter"). Not hypothetical.

**Premises: accepted, no reframing.** Confirming with the user before proceeding (the one
hard gate autoplan reserves for human judgment).

**RESOLVED (user, 2026-07-12): Accept — proceed as scoped.**

### 0B — Existing Code Leverage

See "What already exists" above (code-verified). Summary: `truncate()`, the
`update_snapshot()` pull-projection pattern, `View::Inspector`'s filter-chip model, and
`pump.rs`'s producer/consumer channel are all directly reusable. The one sub-problem with
**no** existing solution to lean on is secret redaction (see Premise Correction above) — this
is new, bounded work, not a rebuild of anything that exists.

### 0C — Dream State Mapping

```
  CURRENT STATE                       THIS PLAN                        12-MONTH IDEAL
  Dashboard shows only        --->    Row shows last tool call  --->   Cockpit is the *only*
  status+context_tokens;              readably, error state,          place an operator ever
  "is it stuck?" requires             idle-vs-busy signal, and        looks — activity, errors,
  tailing flight.jsonl by            a scoped live event stream        budgets (ux.8), chat
  hand or opening Inspector           per selected agent               (ux.1), spawn (ux.3),
  and reading raw JSON.                                                evidence (ux.6) all live
                                                                        here. ux.2 is the load-
                                                                        bearing "what's happening"
                                                                        layer everything else
                                                                        (chat retarget, budget
                                                                        alerts, spawn-then-watch)
                                                                        depends on being legible.
```

This plan moves directly toward the 12-month ideal — it's not a detour. Per the north star
already recorded in `docs/ROADMAP.md`: "ux.0/2/1/8 make it live, watchable, chattable, tunable."
ux.2 is specifically the "watchable" piece; ux.1/ux.8/ux.3 build on the same table/row/stream
primitives this increment establishes.

### 0C-bis — Implementation Alternatives (auto-decided per autoplan: P1 completeness + P5 explicit)

```
APPROACH A: Scheduler-tracked fields, pull-projected (server authoritative)
  Summary: Add last_activity/last_error/error_count/last_event_at to AgentTask itself, updated
    at the same call sites that already emit ToolResult/AgentFailed flight events (agent/mod.rs).
    update_snapshot() reads them off task exactly like every other field (turn, context_tokens).
    New: one FUSE virtual file per agent (single JSON blob, not 4 more flat files — see taste
    calibration above) + matching management-API JSON fields. idle_secs computed at read time
    (now - last_event_at) so both FUSE and HTTP clients derive it identically from one timestamp.
  Effort:  M
  Risk:    Low
  Pros:    Matches the existing pull-projection architecture exactly (zero new async complexity
           in agentd); single source of truth; works identically over FUSE and HTTP.
  Cons:    Touches AgentTask's core state machine (a hot, well-tested file); requires building a
           new redaction function (no existing one to reuse, per the Premise Correction).
  Reuses:  update_snapshot pull pattern, truncate(), Inspector's filter chips, pump.rs channel.

APPROACH B: Client-side flight.jsonl scan (mirrors count_egress_by_agent)
  Summary: agentctl derives last_activity/idle_secs/error_count itself by scanning flight.jsonl,
    the same way count_egress_by_agent() already does for egress counts. Zero agentd/surfaces
    changes.
  Effort:  S
  Risk:    High
  Pros:    Smallest diff; no scheduler changes at all.
  Cons:    HARD BLOCKER, not just a style tradeoff: ux.9's HTTP-fallback cockpit path (just
           shipped, v0.82.0) has no filesystem access to flight.jsonl at all — this approach
           would silently not work in the exact unprivileged mode ux.9 made a first-class,
           supported path. Also O(file size) rescanned every poll tick as the log grows across
           a long session, unlike egress counts (a single cumulative read).
  Reuses:  count_egress_by_agent's scan pattern.

APPROACH C: SSE-push only, no polled snapshot fields
  Summary: Rely entirely on the management API's SSE stream to push activity events to the TUI;
    no new AgentSnapshot fields, no FUSE files.
  Effort:  M-L
  Risk:    Medium
  Pros:    True real-time, zero poll lag.
  Cons:    SSE is a management-API-only mechanism (p7.7) — the FUSE path (ux.9's PREFERRED,
           default-when-privileged mode) has no SSE today. Building the core feature only for
           the fallback transport inverts ux.9's own stated priority.
  Reuses:  The SSE plumbing already used by ux.9's HTTP fallback / orch.2.
```

**RECOMMENDATION: Approach A.** It is the only one of the three that works correctly in
*both* transport modes ux.9 just made load-bearing (FUSE preferred, HTTP fallback) — B is
disqualified by a hard architectural gap (no flight.jsonl access over HTTP), and C inverts
FUSE's newly-established priority. A also requires zero new async/event-driven machinery —
it's a straight extension of the exact pattern every other `AgentSnapshot` field already
follows. The master plan's own text ("live-refine `last_activity`/`idle_secs` from the SSE
feed between polls") is compatible with A as a *base layer* — SSE becomes an optional
freshness enhancement on top of A in the HTTP path, not a replacement, closing the gap
Approach C tried (and failed) to solve outright. Auto-decided: Mechanical (P1 completeness
rules out B on a hard blocker, not taste; P5 explicit rules out C's added complexity for a
freshness win this increment's acceptance criteria don't require).

### 0D-prelude — Expansion framing note

Mode is SELECTIVE EXPANSION (feature enhancement on an existing system, per autoplan's
context-dependent default). Expansion candidates below are cherry-pick opportunities, not
committed scope — auto-decided via the 6 principles, logged to the Decision Audit Trail.

### 0D — Mode-Specific Analysis (SELECTIVE EXPANSION)

**Complexity check:** counting distinct files touched by Approach A: `agentd/src/agent/mod.rs`
(new fields + update sites), `agentd/src/scheduler.rs` (`update_snapshot` projection), a new
small redaction module (`agentd/src/` or `agentd/src/agent/`), `surfaces/src/snapshot.rs`
(new `AgentSnapshot` fields), `surfaces/src/agents_fs.rs` (new FUSE virtual file),
`agentd/src/management.rs` (JSON field), `agentctl/src/watch/reader.rs` (read the new file),
`agentctl/src/watch/source.rs` (HTTP JSON parsing), `agentctl/src/watch/views.rs` (table
rendering, color rules), `agentctl/src/watch/app.rs` (row-scoping/freeze state) — **10 files**.
Over the 8-file smell threshold. This is a real signal, not noise: it's a full slice of a
5-layer stack (scheduler core → surfaces snapshot → FUSE → management API → 4 agentctl
modules), which is inherent to "a new field must flow end-to-end through every layer that
already carries every other field" — not padding. Confirmed by checking: every one of the 10
files already appears in the "What already exists" file list for an *existing* field
(`context_tokens`, `status`, etc.) — this is the established width of "add one snapshot
field," not scope creep specific to this plan.

**Minimum set:** all 10 files above are load-bearing for the stated acceptance criteria (a row
that doesn't reach `views.rs` isn't visible; a field that stops at `snapshot.rs` without a FUSE
file isn't reachable by the FUSE-mode cockpit). Nothing here is deferrable without breaking an
explicit acceptance checkbox from the master plan.

**Expansion scan (candidates, not yet in scope):**
- *10x check:* a fully generalized "event bus" architecture where the TUI subscribes to a typed
  stream of structured UI-ready events (not just activity/error, but budget changes, spawn
  events, credential events — everything ux.1/ux.3/ux.8 will also need) instead of building
  ux.2's activity tracking as its own bespoke mechanism.
- *Delight opportunities:* (1) a small sparkline/heartbeat glyph showing recent activity
  frequency, not just the single last event; (2) color-coding `LAST-TOOL` by tool *category*
  (network call vs local compute vs memory op), not just error/ok; (3) a "since last error"
  duration alongside `error_count` so a one-time blip 10 minutes ago doesn't look the same as a
  live crash loop; (4) clicking/selecting the error glyph directly (not just the row) jumps
  straight to that event in the stream pane; (5) an optional desktop-notification hook when a
  row flips red — foreshadows ux.4 (proactive push) but as a zero-network, purely-local toast.
- *Platform potential:* the generalized event-bus idea (10x check) would be genuine
  infrastructure ux.1 (chat streaming), ux.3 (spawn events), and ux.8 (budget alerts) could all
  build on instead of each inventing its own polling/pushing mechanism.

**Cherry-pick ceremony (auto-decided, neutral posture, P2 boil-lakes + P3 pragmatic):**
- Generalized event-bus (10x check): **DEFERRED to TODOS.md.** Genuinely valuable
  platform-potential idea, but a materially bigger, cross-cutting architecture change that
  would expand this increment's blast radius from "add one field" to "redesign the snapshot
  transport layer" — exactly the kind of expansion SELECTIVE EXPANSION mode holds baseline
  against. Right time to revisit: when ux.1 (chat) starts needing its own streaming mechanism
  and the duplication becomes concrete, not speculatively now.
- Sparkline/heartbeat glyph (delight #1): **DEFERRED to TODOS.md.** Not in blast radius of the
  10 files above (needs a ring buffer of recent event timestamps, not just "last"); nice but
  not required for the "is it stuck" acceptance criterion, which `idle_secs` alone already
  answers.
- Tool-category color-coding (delight #2): **CUT.** Adds a tool-name→category classification
  table that would need maintaining as new tools are added (native + every MCP server) — a
  DRY/maintenance liability for a marginal legibility gain over the already-planned error/ok
  color split.
- "Since last error" duration (delight #3): **ADD to this plan's scope.** In blast radius
  (same `last_error` field already being added just needs a companion timestamp) and under
  1 day CC effort — a `last_error_at: Option<u64>` alongside `error_count` costs nothing extra
  to plumb through the same 10 files, and directly improves the "hung vs busy" acceptance
  criterion by distinguishing a stale error from a live one.
- Jump-to-stream-event on error click (delight #4): **DEFERRED to TODOS.md.** Requires stream
  pane state (scroll position, filter) to exist first — sequencing dependency, not this
  increment's file set.
- Local desktop-notification toast (delight #5): **CUT.** Explicitly ux.4's stated scope
  ("Proactive push... local notifier") per `docs/ROADMAP.md` — building it here would be
  duplicate work when ux.4 lands, not acceleration.

**Accepted scope addition:** `last_error_at: Option<u64>` (Unix seconds) alongside
`error_count`, enabling "N errors, most recent Ns ago" instead of just a raw count.

### 0E — Temporal Interrogation

```
  HOUR 1 (foundations):   Implementer needs to know NOW, not discover mid-build:
                          - The exact call sites in agent/mod.rs where ToolResult/AgentFailed
                            events are already emitted (so the new field updates happen at the
                            SAME point, not a second pass over the code).
                          - The redaction function's pattern list (credential-shaped prefixes/
                            shapes — no regex engine, see Eng Review's "zero new crates" fix)
                            must be decided before Eng starts, not improvised per-PR — Eng phase
                            fixes this concretely.
  HOUR 2-3 (core logic): Ambiguities that will bite:
                          - idle_secs: computed from what clock? Server Unix time vs client
                            elapsed-since-poll are NOT the same number if the poll interval is
                            large. Must be `now_unix - last_event_at_unix`, computed at read
                            time in agentctl (client), using the SAME wall-clock source for both
                            FUSE (file read) and HTTP (JSON field) — resolved in Eng phase.
                          - What counts as "error" for `last_error`/`error_count`: only
                            `is_error` tool results, or does `capability_denied` (a distinct
                            flight event kind) also increment it? Master plan text says both —
                            Eng phase must enumerate the exact EventKind list.
  HOUR 4-5 (integration): What will surprise the implementer:
                          - The FUSE-mode cockpit and HTTP-mode cockpit currently produce
                            *visibly different* Dashboard richness (confirmed live during ux.9's
                            QA — FUSE showed memory/spawn/approvals sections HTTP fallback
                            didn't). If `last_activity`/`idle_secs` end up FUSE-only by accident
                            (e.g. forgetting the management API JSON field), that gap silently
                            widens instead of closing. Eng phase must treat both surfaces as
                            equally load-bearing, per 0C-bis's Approach A rationale.
                          - Redaction must run BEFORE truncation, not after — truncating first
                            could slice a credential-shaped token in half, defeating a
                            pattern-match redactor that expects the full token intact.
  HOUR 6+ (polish/tests): What they'll wish they'd planned for:
                          - A test with a deliberately credential-shaped tool argument
                            (e.g. a fake `sk-ant-...`-prefixed string as a tool call arg) proving
                            the redaction actually fires — this is explicitly one of the master
                            plan's own acceptance checkboxes; write it early, not as an
                            afterthought.
                          - `--plain` mode parity: every color/glyph decision (row-red,
                            idle-amber) needs a text-only equivalent verified in the same PR,
                            not assumed to "just work" because color falls back gracefully.
```

### 0F — Mode Selection

**SELECTIVE EXPANSION**, confirmed (auto-decided per autoplan's context-dependent default:
feature enhancement on an existing system). Implementation approach: **A** (0C-bis), the
ideal-architecture-and-only-fully-correct option, not merely the minimal-viable one — for this
plan the two coincide (A is both the most complete AND the natural fit for existing patterns).

### CEO Dual Voices

Dispatched Claude subagent (foreground, independent) and Codex (Bash, sequential) against
this plan file. Both returned; findings below.

**CLAUDE SUBAGENT (CEO — strategic independence):** verified 5 findings against actual code
(not speculation) — 3 CRITICAL/HIGH load-bearing engineering corrections (in-flight tool-call
visibility bug, stream-pane FUSE/HTTP parity gap, redaction overclaim) already folded into
"Engineering corrections" above, plus 1 MEDIUM (OTEL redaction dedup — declined, logged to
TODOS.md) and 1 MEDIUM (idle-amber `Waiting` carve-out — folded in above). Explicit conclusion:
**"Premises... hold up under scrutiny — this is not a wrong-problem review."**
Recommendation: send back for one more planning pass (now done) before Eng starts.

**CODEX SAYS (CEO — strategy challenge):** recommends reframing ux.2 entirely into an
"Attention & Evidence" / approvals-centric increment, arguing the product thesis
(`docs/PRODUCT-THESIS.md`) is about *reducing* required supervision, not making supervision more
detailed, and that runtime-centric signals (`idle_secs`, `error_count`, tool names) are the
wrong headline vs outcome-centric signals (risk, action-needed). Cites real thesis text
(approval gate is priority #3, above observability #4 in the load-bearing build list) and a
real, previously-unexamined-by-me tension (a dashboard optimizes for terminal dwell time; the
thesis's actual promise is trusting agents *without* watching).

**Verification of Codex's framing against the actual codebase (not taking the critique at face
value):** Codex's specific claim that the plan "underplays approvals" is overstated — an
`Approvals` view (`[a]` key) already exists as a first-class, discoverable cockpit view
(confirmed: `agentctl/src/watch/views.rs:153`'s footer hints show `[a]pprove` alongside every
other key, shipped since p7.4/v0.38.0, well before this increment). Approvals are not being
ignored architecturally; they're just not the Dashboard's *default* first screen. This softens
but doesn't eliminate Codex's broader point.

**CEO DUAL VOICES — CONSENSUS TABLE:**
```
═══════════════════════════════════════════════════════════════
  Dimension                           Claude  Codex  Consensus
  ──────────────────────────────────── ─────── ─────── ─────────
  1. Premises valid?                   YES     NO      DISAGREE
  2. Right problem to solve?           YES     NO*     DISAGREE
  3. Scope calibration correct?        NO**    NO***   CONFIRMED gap (different reasons)
  4. Alternatives sufficiently explored?YES    NO      DISAGREE
  5. Competitive/market risks covered?  N/A    NO      N/A (Claude didn't assess; low-stakes internal tool)
  6. 6-month trajectory sound?         YES     NO      DISAGREE
═══════════════════════════════════════════════════════════════
* Codex: right problem exists (attention/trust), wrong framing chosen (observability vs
  reduce-supervision). ** Claude: scope is right, but 3 mechanisms inside it are under-specified/
  broken as written (now fixed above). *** Codex: scope should be reframed around approvals/
  incidents, not activity detail.
```

**This is NOT a User Challenge** (autoplan's definition requires *both* models to converge on
recommending the user's stated direction change — merge/split/add/remove). Here the models
diverge on the meta-question itself: Claude explicitly validates the premise and problem framing
("not a wrong-problem review"); Codex recommends reframing it. Per autoplan's classification
rules this is exactly a **TASTE DECISION** (a Codex disagreement with a valid, well-argued
strategic reason) — auto-decided per the 6 principles, surfaced at the final gate rather than
blocking here.

**Auto-decision (P2 boil-lakes + P6 bias-toward-action):** Keep ux.2 scoped as "Observe," per
the already-CEO-reviewed master plan's (`docs/plans/ux-cockpit.md`, reviewed 2026-07-10)
explicit sequencing choice. Reasoning: (1) the master plan's own prior CEO review already
weighed a similar tradeoff space when it separated "cathedral expansions" — ux.4 (Proactive
push, exceptions/attention-oriented) is EXPLICITLY already scoped as a *later*, separate
increment, meaning a prior review session already considered attention-centric UX and chose to
sequence it after Observe, not instead of it; re-litigating that sequencing from scratch inside
ux.2's own CEO phase would be scope creep in the wrong direction (redesigning the roadmap, not
reviewing this increment). (2) Codex's stronger tactical points (`idle_secs`/`error_count` as
weak signals in isolation) are *already* substantially addressed by the engineering corrections
above (Waiting-status carve-out, `last_error_at` recency) — the remaining gap (fully
outcome/risk-centric signals) is real but is squarely ux.4/ux.8's territory (budget risk,
proactive push), not a reason to block or redesign ux.2. (3) The "Approvals view already exists
and is discoverable" verification directly undercuts the strongest plank of Codex's argument
(that approvals are invisible/deprioritized).

**Logged as a taste decision for the final gate** (not silently dismissed): the user may
disagree with this auto-decision given how much strategic weight Codex's argument carries —
flagged prominently, not buried, at Phase 4.

**Resolution of Engineering Correction #2's fork (stream pane FUSE/HTTP parity): SUPERSEDED.**
The CEO-phase auto-decision below is preserved for the audit trail but does not stand — the Eng
Review (dual-voice, both models independently) found the premise wrong: HTTP mode already has a
live SSE flight-event pipeline (`pump.rs`, shipped in ux.0) with nothing to build; FUSE mode is
the transport with no live-tail mechanism. See "Eng Review reopens Correction #2" in the Eng
Review section below for the corrected resolution (dual mechanism: SSE-consume for HTTP,
timer-repoll for FUSE — full parity, neither mode gets a placeholder).

~~Auto-decided **(a) — scope the stream pane as FUSE-only for this increment, state it
explicitly in acceptance criteria** (P3 pragmatic + P2 boil-lakes: adding SSE-wiring now would
expand this already-10-file, borderline-complexity increment further; the scalar snapshot
fields — the harder-required, acceptance-critical part — already work over both transports per
Approach A). HTTP-mode users get the colored table + AgentDetail timeline (both
snapshot-field-based, both transport-parity by construction) but not the live raw stream pane
until a follow-up wires SSE for it. Logged to TODOS.md as a named follow-up, not silently
dropped.~~

### Error & Rescue Registry

| Failure | Detection | Message shown | Recovery |
|---|---|---|---|
| Credential-shaped text in a tool arg/result doesn't match any known pattern | None (best-effort only, per corrected acceptance criterion) | N/A — passes through unredacted | THREAT_MODEL note documents this as an accepted, known gap (matches §8.7's existing framing) |
| `last_activity` write races the scheduler tick reading `AgentTask` for `update_snapshot()` | N/A — verified single-threaded: the write now happens synchronously in `enqueue_or_defer`'s `AgentEffect::CallTools` arm (has `&mut state`), before the tool future is spawned, same call site already holding `state` for every other field write | N/A | No lock needed — confirmed by reading `scheduler.rs:1291-1303` directly, not assumed |
| `idle_secs` computed against a wall-clock that drifts between agentd's host and the Mac client (HTTP mode) | Not detected automatically | Could show a slightly-off idle number | Low-severity; both times are Unix epoch seconds from NTP-synced hosts in practice — noted as accepted, not solved |
| FUSE-mode stream pane's timer-repoll (not true push) reads a growing `flight.jsonl` on each tick | Repoll reuses `read_flight_tail`'s existing tail-bounded read (not a full-file rescan) | N/A | Bounded by the same tail-size cap Inspector already uses; no new unbounded-read risk |
| A tool name never seen before (new MCP server) breaks the `LAST-TOOL` readable-rendering heuristic | Falls back to raw `tool_name(...)` truncated, never raw JSON | Degraded but never garbled/panicking | Eng phase must specify the fallback format explicitly, not leave it undefined |

### Failure Modes Registry

| # | Failure mode | Blast radius | Severity | Mitigation |
|---|---|---|---|---|
| 1 | In-flight tool call invisible (Engineering Correction #1) | Defeats primary acceptance criterion | CRITICAL | CEO-phase fix location wrong (no `&mut AgentTask` in `run_tools_sequential`); corrected in Eng Review to update at the `CallTools` dispatch site instead, batch-granularity |
| 2 | Stream pane silently FUSE-only, no HTTP parity (Correction #2) | Undermines ux.9's HTTP-fallback design intent | HIGH | CEO-phase resolution reversed in Eng Review: HTTP already has live SSE (`pump.rs`, ux.0); FUSE gets timer-repoll — full parity, no placeholder |
| 3 | Redaction overclaim (Correction #3) | Security/trust — repeats a known prior mistake class | HIGH | Fixed in plan: reworded acceptance criterion + preemptive THREAT_MODEL note |
| 4 | False "stuck" signal on `Waiting`-status agents (Correction #4) | Trust erosion in the very indicator this increment builds | MEDIUM | Fixed in plan: status-based carve-out |
| 5 | `spawn_agent`/`send_message`/`request_approval` never update `last_activity` | Silently drops some of the most operator-relevant activity | HIGH | Folded into Correction #1's fix — Eng phase must enumerate all 3 call sites |
| 6 | New/unknown tool names break table rendering | Cosmetic only if fallback is specified (see Error registry) | LOW | Eng phase specifies fallback format |
| 7 | Two independently-maintained redaction code paths (agentd's new one + otel's `redact_previews`) drift apart over time | Duplicated security-hardening effort, inconsistent miss profiles | MEDIUM | Declined to unify now (crate-boundary cost); logged to TODOS.md |

### CEO Completion Summary

**Premises:** accepted (user-confirmed) — this is the right problem, no reframing needed.
**Existing code leverage:** `truncate()`, `update_snapshot()` pull pattern, `Inspector` filter
chips, `pump.rs` channel — all reused, nothing rebuilt that already exists.
**Alternatives:** Approach A (scheduler-tracked, pull-projected) chosen over B (client-side
flight.jsonl scan — hard HTTP-mode blocker) and C (SSE-push-only — inverts FUSE's established
priority).
**Mode:** SELECTIVE EXPANSION. One scope addition accepted (`last_error_at`); three expansion
candidates deferred to TODOS.md; two cut (tool-category color-coding, local notification toast —
the latter is explicitly ux.4's job).
**Dual voices:** Claude subagent found 3 CRITICAL/HIGH code-verified engineering bugs (all now
fixed in this plan) + validated the premise. Codex recommended a strategic reframe toward
approvals/attention-centric UX, partially undercut by verifying the Approvals view already
exists and is discoverable; auto-decided to keep scope as "Observe" per the master plan's own
prior sequencing decision, with the disagreement surfaced as a taste decision at the final gate
given its strategic weight.
**NOT in scope:** ux.1/ux.8/ux.3 (sequenced separately), a system-wide credential scanner,
web cockpit (ux.5), the generalized event-bus architecture (deferred, revisit at ux.1), a fully
outcome/risk-centric signal model (Codex's stronger point — squarely ux.4/ux.8 territory).
**What already exists:** see "What already exists" section above (code-verified).
**Dream state:** this increment moves directly toward the already-recorded 12-month ideal
(cockpit as the sole operator surface); no detour.

### 11-Section Deep Review (per `sections/review-sections.md`)

Sections 1-2 (Architecture, Error & Rescue Map) are covered above via 0C's dream-state
diagram, 0C-bis's alternatives analysis, and the Error & Rescue Registry/Failure Modes
Registry — the required diagrams and tables already exist there, not duplicated here.

**Section 3 — Security & Threat Model:** Attack surface: none expanded — this is read-only
observability of already-running agents' own state, no new endpoint, no new user input parsed
from an external source (tool arg/result text is already-untrusted content the redaction
correction handles). Authorization: N/A by this repo's locked constitutional decision #2
(`CLAUDE.md`) — single-tenant, agents mutually trusting, no multi-user isolation; cross-agent
visibility of activity/errors within the same operator's cockpit is not a boundary violation,
it's the product. Secrets: covered by Engineering Correction #3 (redaction, reworded to
best-effort + THREAT_MODEL note). Dependency risk: zero new crates. Injection vectors: tool-arg
text is already adversarial-input-aware per this repo's existing LLM-output-trust conventions
(truncation already applied elsewhere); the new redaction function is itself the mitigation, not
a new vector. Audit logging: N/A — this reads existing flight events, doesn't create new
sensitive operations. **One finding:** the redaction function itself needs a test proving a
credential-shaped tool arg is actually caught (already required by the master plan's own
acceptance criteria) — folded into Section 6 (Tests) below, not a new item here.

**Section 4 — Data Flow & Interaction Edge Cases:**
```
  INPUT (flight event) ──▶ FIELD UPDATE ──▶ PULL-PROJECT ──▶ FUSE/MGMT-API ──▶ TUI RENDER
    │                          │                  │                │               │
    ▼                          ▼                  ▼                ▼               ▼
  [ToolCall vs        [redaction runs      [update_snapshot   [FUSE file       [unknown tool
   ToolResult          before truncate,     reads off task,   absent if       name → fallback
   timing — FIXED      not after — Hour     same as every     store not       format, not raw
   in Correction #1]   4-5 note above]      other field]      configured]     JSON — Correction
                                                                               #4/Error registry]
```
Interaction edge cases:
```
  INTERACTION          | EDGE CASE                        | HANDLED? | HOW?
  ---------------------|-----------------------------------|----------|----------------------
  Row selection         | Select a row, agent terminates    | Y        | prune_dead_agent
                         | mid-selection (existing FUSE       |          | pattern (p5.8) already
                         | pattern, agents_fs.rs)             |          | handles lazy cleanup
  Stream pane freeze     | Freeze, then the frozen agent's    | Y        | `▼ N new` affordance
                         | row selection changes              |          | (in scope, per master
                         |                                    |          | plan text)
  Stream pane freeze     | Freeze while scrolled, then        | ?        | Not specified — Eng
                         | switch agents via row select       |          | phase must decide:
                         |                                    |          | does switching agent
                         |                                    |          | auto-unfreeze? (open
                         |                                    |          | question for Eng)
  Error row              | Error clears (agent recovers),     | Y        | row returns to normal
                         | last_error_at stays set            |          | color; error_count/
                         |                                    |          | last_error_at persist
                         |                                    |          | as history, don't reset
  Idle-amber             | Waiting-status agent                | Y        | Correction #4 fix
                         |                                     |          | (status carve-out)
  Zero agents (cockpit    | Empty dashboard, ux.9's default    | Y        | Already the designed
   mode default state)    | state                               |          | ux.9 behavior — no
                         |                                    |          | new gap
```
One open question flagged for Eng phase (freeze + agent-switch interaction) — not blocking,
noted so it isn't discovered mid-implementation.

**Section 5 — Code Quality Review:** Organization fits existing patterns (extends
`AgentSnapshot`/`AgentInfo`/`update_snapshot` exactly as every other field does — no deviation).
DRY: the one real violation risk (two redaction mechanisms, agentd's new one + otel's
`redact_previews`) already surfaced and declined-with-reason above (Engineering Corrections,
"Considered and declined"). Naming: `last_activity`/`last_error`/`error_count`/`last_error_at`/
`idle_secs` are all self-describing, matching the plain-English style of existing fields
(`context_tokens`, `token_budget`). Over-engineering check: none — Approach A was chosen
specifically because it avoids new abstractions (0C-bis). Under-engineering check: none
remaining after Corrections #1-#4. Cyclomatic complexity: `update_snapshot`'s existing match
arm doesn't branch further for the new fields (they're straight field reads, not new
conditionals) — no new branching to flag.

**Section 6 — Test Review:**
```
  NEW UX FLOWS: row shows readable last-tool text; row goes red on error; idle amber threshold;
    row selection scopes stream; freeze/unfreeze; AgentDetail timeline
  NEW DATA FLOWS: AgentTask field update at ToolCall+ToolResult+3 special-cased tool sites →
    update_snapshot → FUSE file / management API JSON → agentctl reader → views render
  NEW CODEPATHS: redaction function (pattern match + strip), idle_secs computation (now -
    last_event_at), Waiting-status color carve-out, unknown-tool-name fallback format
  NEW BACKGROUND JOBS: none (extends the existing poll/SSE producer model, no new thread)
  NEW INTEGRATIONS: none external — purely internal (scheduler → FUSE/management API → TUI)
  NEW ERROR/RESCUE PATHS: FUSE file absent (memory-store-not-configured pattern already exists,
    reuse); redaction pattern miss (accepted, documented gap, not "rescued")
```
Per-item test coverage:
- Redaction: happy path (known pattern caught), failure path (unknown pattern passes through —
  this is EXPECTED behavior post-Correction-#3, test asserts the *documented* boundary, not an
  impossible "catches everything"), edge case (credential-shaped text split across two
  truncation boundaries — verifies redact-before-truncate ordering from Correction #1's Hour 4-5
  note).
- ToolCall-vs-ToolResult timing: a test that starts a slow mock tool call, asserts
  `last_activity` shows "running: toolname(...)" DURING the await, then shows the result preview
  after — directly proving Correction #1's fix, not just its absence-of-bug.
- Waiting-status idle-amber carve-out: a test with a long-parked `Waiting` agent asserting no
  amber color despite high `idle_secs`.
- `--plain` parity: every color-coded state (row-red, idle-amber) has a glyph-only rendering
  test.
Test ambition ("2am Friday" test): the ToolCall-timing test above IS that test — it's exactly
the failure mode that would otherwise ship silently and only surface as a confusing support
report weeks later. Test pyramid: mostly unit (Rust `#[test]` on `update_snapshot`,
redaction function, color-rule functions) matching this codebase's existing convention — no
E2E framework exists or is needed for a TUI's internal logic. Flakiness risk: none identified —
no time-dependent assertions beyond `idle_secs` itself, which is directly and deterministically
computable in a test (fixed `now`, fixed `last_event_at`).

**Section 7 — Performance:** N+1/DB indexes: N/A, no database. Memory: bounded — one
`last_activity` struct per agent, same order of magnitude as `task_preview`/existing fields
already on `AgentSnapshot`; no unbounded growth (unlike a ring buffer, which the "sparkline"
expansion candidate would have needed — correctly deferred). Caching: N/A, `update_snapshot`
is already the cache (a pull-projection recomputed once per scheduler tick, not per read).
Slow paths: none — field reads are O(1), redaction runs on already-truncated-length text
(bounded by `PREVIEW_CHARS`), not unbounded tool output. No issues found.

**Section 8 — Observability & Debuggability:** This IS the observability feature — slightly
recursive to ask "how do we observe the observability feature," but genuinely worth a line:
if `last_activity` itself silently stops updating (a bug in the new update-site wiring), the
operator would see a stale row with no indication *why* it's stale, which is exactly the
class of silent failure this whole increment exists to eliminate. **Finding:** add a debug-only
assertion or a flight event (`activity_field_stale`?) is overkill for this increment's scope,
but `idle_secs` itself IS the safety net — if update-site wiring breaks, `idle_secs` grows
unboundedly and eventually IS the signal something's wrong, even for the tracking mechanism's
own health. No new dedicated observability surface needed; the feature is self-diagnosing by
construction. No additional gap found.

**Section 9 — Deployment & Rollout:** No DB migration (redb schema unaffected — these are
in-memory `AgentTask` fields + a new FUSE file + a JSON field, not persisted state). No feature
flag needed — this is additive, backward-compatible (old `agentctl` binaries simply won't show
the new columns/fields; old `agentd` binaries without the fields just don't populate them,
`AgentInfo`'s `#[derive(Default)]` already handles absent fields gracefully per existing
pattern). Rollback: a plain `git revert`, no data migration to unwind. Deploy-time risk window:
N/A — `agentd`/`agentctl` are versioned together in this repo's release cadence, not
independently deployed services. No issues found.

**Section 10 — Long-Term Trajectory:** Technical debt introduced: the declined
OTEL-redaction-unification (Correction #4's "Considered and declined") is real, logged debt —
acknowledged, not hidden. Path dependency: low — Approach A extends existing patterns, doesn't
foreclose future architecture (the deferred generalized event-bus idea remains buildable later,
this doesn't block it). Reversibility: 4/5 (easily revertible; the "5" is reserved for
zero-cost reversions, and this does add a small, real security-relevant utility function that
would need re-auditing if reverted-then-reintroduced). Knowledge concentration: the plan itself
is the documentation; Eng phase should keep the redaction pattern list in a code comment citing
this plan file for provenance (matching this repo's existing convention of citing decision
provenance in comments). 1-year question: a new engineer reading this plan should find "add a
field to AgentSnapshot" completely unsurprising — it follows a well-worn path.

**Section 11 — Design & UX Review (UI scope confirmed):**
Information architecture: Dashboard row (glance) → AgentDetail (drill-down) → stream pane
(raw evidence) — a coherent three-level hierarchy, matching "hierarchy as service" (CEO
cognitive pattern #15: what should the user see first/second/third). Interaction state
coverage:
```
  FEATURE           | LOADING          | EMPTY              | ERROR           | SUCCESS        | PARTIAL
  ------------------|------------------|---------------------|-----------------|----------------|------------------
  Agent row         | N/A (poll-based, | N/A (row only       | red row, error  | green dot,     | N/A
                     | no per-row       | exists for a        | glyph visible   | readable tool  |
                     | loading state)   | running agent)      |                 | text           |
  Stream pane        | "waiting for     | "no events yet"     | error events    | one-liners     | HTTP-mode:
                     | first event"     | (cockpit just       | color-coded     | render live    | placeholder
                     |                  | booted, ux.9 empty  | (existing       |                | text (Correction
                     |                  | state)              | Inspector conv.)|                | #2 resolution)
  AgentDetail        | N/A              | "no activity yet"   | persistent      | timeline of    | N/A
  timeline           |                  | (agent just spawned)| error strip     | ~10 events     |
```
User journey: operator glances at Dashboard (red row catches eye) → selects the row → stream
pane scopes to that agent → sees the error in context → opens AgentDetail for the persistent
strip + timeline if deeper investigation is needed. Coherent arc, no dead ends. AI slop risk:
low — every element (`LAST-TOOL` column, error strip, freeze affordance) is specifically
described with exact formatting examples in the master plan, not a generic "add a nice UI"
gesture. Responsive intention: N/A (terminal UI, no viewport concept beyond terminal width —
`--plain` mode IS this codebase's accessibility/narrow-environment equivalent, already
covered). Accessibility: color always paired with glyph/text per master plan's own explicit
requirement ("Glyph + color always (`--plain` safe)") — keyboard nav unaffected (reuses
existing row-selection/key-binding model, no new input modality).

Required diagram (user flow): see the Dream State (0C) diagram plus the data-flow diagram
above (Section 4) — together these cover screens/states/transitions; a dedicated
screens-only diagram would be redundant with what's already produced.

**Recommendation per Section 11:** given this plan has genuine UI scope (new table columns,
color rules, a new pane, a new detail view), running `/plan-design-review` for a deeper visual
audit *would* add value beyond this CEO-level design-intentionality check — noting this per the
skill's own instruction, to be decided at the Phase 4 gate (not auto-invoked mid-pipeline, since
autoplan's own Phase 2 — Design Review — covers the equivalent ground next).

### Diagrams produced
System architecture (0C dream-state + this section's data-flow diagram), data flow (Section 4,
including shadow paths), error flow (Error & Rescue Registry), user flow (Section 11). No state
machine diagram needed (no new stateful object beyond existing `AgentStatus` enum, already
diagrammed in prior increments). No deployment-sequence/rollback-flowchart needed (Section 9:
no migration, plain revert).

### Stale Diagram Audit
No existing ASCII diagrams in the files this plan touches (`snapshot.rs`, `scheduler.rs`,
`reader.rs`, `views.rs`) needed updating — none exist there today to go stale.

### Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|-----------------|-----------|-----------|----------|
| 1 | CEO/Premise | Premises accepted, no reframe | User gate | — | Real, already-observed pain point (cos-ux-01); direct path to stated outcome | — |
| 2 | CEO/0C-bis | Approach A (scheduler-tracked, pull-projected) | Mechanical | P1+P5 | Only approach correct over both FUSE and HTTP transports | B (client-scan, hard HTTP blocker), C (SSE-only, inverts FUSE priority) |
| 3 | CEO/0D | 5 expansion candidates scanned; 1 accepted (`last_error_at`), 3 deferred, 2 cut | Taste (cherry-pick) | P2 boil-lakes, P3 pragmatic | See per-item reasoning in 0D | Generalized event-bus, sparkline, tool-category color, jump-to-event, notification toast |
| 4 | CEO/Dual-voice | 3 code-verified engineering bugs found and fixed in-plan (ToolCall timing, stream-pane parity, redaction overclaim) | Mechanical | P1 completeness, P5 explicit | Claude subagent verified against actual code, not speculation | — |
| 5 | CEO/Dual-voice | Codex's "reframe to Attention & Evidence" — declined, kept as "Observe" | Taste (Codex disagreement, surfaced at gate) | P2 boil-lakes, P6 bias-toward-action | Master plan's own prior CEO review already sequenced attention-centric UX (ux.4) separately; Approvals view already exists and is discoverable, undercutting Codex's strongest plank | Full reframe to approvals/incident-centric scope |
| 6 | CEO/Dual-voice | Stream pane scoped FUSE-only this increment (Correction #2's fork) | Taste | P3 pragmatic, P2 boil-lakes | Scalar fields already work over both transports; stream pane SSE-wiring is separable, additive follow-up | Wiring SSE now (bigger diff) |
| 6r | Eng/Dual-voice | **Row 6 REVERSED:** stream pane consumes existing `pump.rs` SSE for HTTP + timer-repoll for FUSE — full parity, no placeholder | Mechanical (both models independently code-verified the same premise error) | P1 completeness | Row 6's premise (FUSE has live-tail, HTTP doesn't) was backwards; `pump.rs` (ux.0, already shipped) already produces `AppEvent::Flight` for HTTP, and `View::Inspector` (the assumed FUSE precedent) loads once and never re-polls | Keeping row 6's FUSE-only scoping (would ship a known-wrong asymmetry) |
| 6s | Eng/Dual-voice | Correction #1's fix moved from `run_tools_sequential`'s ToolCall site to `enqueue_or_defer`'s `CallTools` dispatch site, batch-granularity | Mechanical | P1 completeness | Original site has no `&mut AgentTask` in scope — confirmed by reading `scheduler.rs:1291-1303`; the dispatch site does hold `&mut state` synchronously before spawning the tool future | Per-tool-within-batch granularity (would need a new shared side-channel — out of this increment's blast radius, logged to TODOS.md) |
| 6t | Eng/Dual-voice | Redaction implemented via plain string/prefix matching, not `regex`/`once_cell` | Mechanical | P3 pragmatic | `agentd/Cargo.toml` has neither dependency today; the plan's own Section 7 already claims "zero new crates" — a regex-based implementation would silently contradict that claim | Adding `regex` + `once_cell` (real option, just not free, and not needed for a fixed prefix/pattern list) |
| 7 | CEO/Dual-voice | OTEL redaction unification declined | Taste | P2 boil-lakes | Different crates, different consumers; no shared dependency exists yet | Building a shared crate now |
| 8 | CEO/Section 4 | Freeze+agent-switch interaction left open for Eng phase | Mechanical (flagged, not decided) | — | Genuinely ambiguous, needs Eng-phase design, not a CEO-level call | — |

### TODOS.md updates (proposed, not yet written — pending Phase 4 gate)
1. Generalized event-bus architecture — revisit at ux.1. (P3, L effort)
2. Sparkline/heartbeat activity-frequency glyph. (P3, S effort)
3. Jump-to-stream-event on error-glyph click. (P3, M effort — depends on stream pane existing)
4. OTEL `redact_previews` / new agentd redaction function unification. (P3, S effort)
5. ~~Stream pane SSE-wiring for HTTP-mode cockpit~~ — **superseded, done in-plan (Eng Review):**
   turned out to require zero new agentd-side plumbing (`pump.rs`'s SSE producer already exists,
   ux.0); this increment consumes it directly instead of deferring it.
6. `View::Inspector` itself stays load-once/non-live (pre-existing, not introduced by this plan;
   the new stream pane's FUSE-mode timer-repoll is a separate, new mechanism, not a change to
   Inspector). Making Inspector itself live is a legitimate follow-up if operators want the
   flight-log inspector to auto-tail too — not required for ux.2's acceptance criteria. (P3, S
   effort if picked up)
7. `agentctl/src/watch/inspector.rs`'s `InspectorFilter::Errors` matches `"kind":"tool_error"` and
   `"kind":"inference_error"` — **neither exists in `agentd/src/events.rs`'s `EventKind` enum**
   (real tool failures serialize as `"kind":"tool_result"` with `is_error:true` in the payload;
   there is no `InferenceError` variant at all). Only `"agent_failed"` in that OR-chain actually
   matches anything. Pre-existing bug, found incidentally during Eng review, not introduced by
   this plan — but load-bearing for ux.2: the new stream pane's own "Errors" filter chip must NOT
   copy this pattern (match the real serialized kind names + `is_error` payload field instead).
   (P2, S effort to fix Inspector's copy of the same bug)

### Completion Summary

```
+====================================================================+
|            MEGA PLAN REVIEW — COMPLETION SUMMARY (CEO)             |
+====================================================================+
| Mode selected        | SELECTIVE EXPANSION                          |
| System Audit         | Clean state; hot files match plan's own scope|
| Step 0               | Premises accepted (user-confirmed); Approach A|
| Section 1  (Arch)    | 0 new issues (covered via 0C/0C-bis)         |
| Section 2  (Errors)  | 5 codepaths mapped, 0 GAPS (all rescued)     |
| Section 3  (Security)| 0 issues (redaction covered in Corrections)  |
| Section 4  (Data/UX) | 6 edge cases mapped, 1 open Q for Eng phase  |
| Section 5  (Quality) | 1 DRY risk (declined w/ reason), no other    |
| Section 6  (Tests)   | Diagram produced, 4 named tests specified    |
| Section 7  (Perf)    | 0 issues found                               |
| Section 8  (Observ)  | 0 gaps (self-diagnosing by construction)     |
| Section 9  (Deploy)  | 0 risks (no migration, plain revert)         |
| Section 10 (Future)  | Reversibility: 4/5, 1 debt item logged       |
| Section 11 (Design)  | 0 issues; /plan-design-review value-add noted|
+--------------------------------------------------------------------+
| NOT in scope         | written (5 items)                            |
| What already exists  | written                                      |
| Dream state delta    | written                                      |
| Error/rescue registry| 5 methods, 0 CRITICAL GAPS                   |
| Failure modes        | 7 total, 0 unresolved CRITICAL GAPS          |
| TODOS.md updates     | 6 items proposed                             |
| Scope proposals      | 6 proposed, 1 accepted                       |
| CEO plan             | written (~/.gstack/projects/.../ceo-plans/)  |
| Outside voice         | ran (Codex + Claude subagent, both dual-voice)|
| Diagrams produced    | 4 (arch/dream-state, data-flow, error-flow, user-flow) |
| Stale diagrams found | 0                                            |
| Unresolved decisions | 1 (Codex's reframe — taste decision, gate)   |
+====================================================================+
```

**PHASE 1 COMPLETE.** Claude subagent: 5 findings (3 critical/high, now fixed in-plan; 2
medium, one fixed one declined-and-logged). Codex: 1 major strategic challenge (reframe),
auto-decided against per boil-lakes + prior-sequencing precedent, surfaced as a taste decision
for the final gate. Consensus table: 1/6 dimensions confirmed outright, 5/6 reflect Codex's
reframe recommendation rather than raw disagreement. 11-section deep review complete — 0
critical gaps remaining after in-plan fixes. 6 TODOS items proposed. 1 unresolved decision
(the reframe) carried to Phase 4.
Passing to Phase 2 (Design Review — UI scope detected).

---

## Design Review (Phase 2)

### Step 0: Design Scope Assessment

**0A. Initial rating: 7/10.** The master plan already specifies concrete interaction
descriptions (exact column names, exact color rules, exact key bindings) — well above a
typical "backend-only" plan — but is missing an explicit interaction-state table in the
STANDARD format this skill requires (partially covered in the CEO phase's Section 11, not
yet in the canonical `FEATURE | LOADING | EMPTY | ERROR | SUCCESS | PARTIAL` shape) and has
no responsive/accessibility-specific pass (terminal-width behavior is unaddressed). A 10/10
would additionally have: an explicit terminal-width-degradation spec (what happens at 80 cols
vs 120 cols vs a resized-mid-session terminal) and a written color-token reference (even
informal) so `views.rs` implementers don't each invent slightly different reds/ambers.

**0B. DESIGN.md status:** `docs/DESIGN.md` exists but is the **technical/OS architecture
thesis** (per its own header: "the full thesis, architecture, and rationale"), not a visual
design system — no color palette, no component vocabulary. **Gap, but not blocking**: this
codebase's actual TUI visual conventions are established consistently *in code* (status-dot
colors: running=green, waiting=cyan, error=red, terminated=grey; existing `MemoryPressure`
75/90 thresholds define an amber/red pattern already) even without a written doc. Recommend
`/design-consultation` as a follow-up to formalize these into a written reference — logged to
TODOS.md, not done here (out of this increment's blast radius).

**0C. Existing design leverage:** `View::Inspector`'s filter-chip model + color-coded body
(direct reuse target for the stream pane's filtering); the existing status-dot color
convention (running=green/waiting=cyan/error=red/terminated=grey, already established,
extended not reinvented); `MemoryPressure`'s 75/90 threshold-color pattern (direct precedent
for the budget-bar reuse the master plan already specifies).

**0D. Focus areas — auto-decided (P1 completeness):** run all 7 passes at full depth; no
narrowing (autoplan doesn't ask, just proceeds).

**Classifier: APP UI** (workspace-driven, data-dense, task-focused — a terminal operator
dashboard, not a marketing surface). App UI Rules apply: calm surface hierarchy, dense but
readable, minimal chrome, utility-language copy, cards only when card IS the interaction.

### Visual mockups — ASCII, not PNG (auto-decided, P5 explicit-over-clever)

The `$D` image-mockup tool is built for web/graphical UIs (PNG generation, comparison boards)
— fundamentally mismatched to a terminal UI's actual medium. Producing an ASCII mockup
directly (the format this repo's own master plan and prior increments — e.g. p6.4's topology
view, p6.6's spawn view — already use) is the explicit-over-clever choice: it's literally what
the implementer will build against, not a translated approximation.

```
┌─ AgentOS Cockpit ───────────────────────────────────────────────────────────┐
│ AGENT      STATUS  TURN  LAST-TOOL                        TOKENS   $   AGE ⚠│
│ inbox-07   ●run     12   web_search("q3 revenue…")        ▓▓░ 4.2k  .03  2m │
│ gmail-09   ●run      9   ⏳ oauth_call_api(...) 34s        ▓░░ 2.1k  .02  4m │  ← idle-amber
│                                                                              │     AUGMENTS the
│                                                                              │     tool text —
│                                                                              │     see below
│ curator-07 ●wait     3   (parked — no tool in flight)      ░░░ 1.1k  .01  8m │  ← never amber
│ scout-a    ●err      7   ⚠ mcp_error: timeout              ░░░  890  .01  5m ⚠│  ← whole row red
├───────────────────────────────────────────────────────────────────────────┤
│ Stream: [All] Errors Sandbox CapDenied          scoped: scout-a    [frozen]│
│ 14:32:07 [scout-a] ⚠ oauth_call_api → timeout after 30s                    │
│ 14:31:40 [scout-a] → oauth_call_api(url=".../gmail/v1/...")                │
│ 14:30:12 [scout-a] ✓ web_search("competitor pricing") → 12 results          │
│                                                          ▼ 3 new           │
└───────────────────────────────────────────────────────────────────────────┘
  ↑/↓ select  Enter detail  [s]ystem  [t]opology  [m]emory  [n]ew  [a]pprove  [c]reds  [i]nspector  q quit
  Tab filter  [f]reeze/unfreeze+jump-to-bottom  (stream-pane keys, shown on a 2nd footer line)
```
**Fixed (DX Review, both models independently flagged this footer as a regression risk):** the
original mockup's footer silently dropped `[t]opology`/`[m]emory`/`[c]reds` while adding `[f]`
— if built literally, 3 already-shipped, currently-discoverable panes become undiscoverable.
The corrected footer keeps every existing pane-switch key unchanged and adds this increment's 2
new keys (`Tab` for filter-cycling, matching Inspector's own `Tab:filter(...)` convention
exactly since the rough scope already says "reuse the Inspector filter model"; `f` for freeze)
on a second line rather than cramming everything onto one — the existing single-line footer was
already near its practical width budget before this plan added anything. Also fixed in passing
(zero extra cost, same line already being edited): `[i]nspector` was missing from the footer
even before this plan (pre-existing gap, not introduced by ux.2) — added here since the fix is
free. **Filter-cycling and stream-scoping key assignment (closes the Codex + Claude-subagent
converged finding on stream-pane control discoverability):** `Tab` cycles the stream pane's
filter chips (All→Errors→Sandbox→CapDenied→Egress→All), reusing `InspectorFilter::next()`'s
exact cycle order — no separate "focus the stream pane" key is needed, since stream-scoping is
already tied to agent-row selection (↑/↓, existing) by design, not an independent focus target.

**`gmail-09` is the row this whole increment exists for** — a tool call genuinely in flight
past the idle threshold. **Resolved ambiguity (Claude subagent design finding, CRITICAL):**
idle-amber is an *annotation on the existing `LAST-TOOL` text*, never a replacement of it —
the `⏳`/amber marker and elapsed-seconds are appended to the in-flight phrasing Correction #1
already mandates (`"running: {tool}({args})"`), so the operator never loses the one piece of
information they need most at the exact moment they need it most. `curator-07` (genuinely
`Waiting`, per Correction #4) shows no tool line and is never amber regardless of `idle_secs` —
visually distinct from `gmail-09` by status text alone, not just color (addresses the
color-blind-triad finding below).

**Resolved: idle-amber threshold — 30 seconds, global (not per-tool-class).** Named explicitly
now, not left absent (the design subagent's single highest-severity finding — "the most
consequential unspecified number in the whole plan"). Chosen as a pragmatic default matching
the master plan's own precedent (`orchestrate)`'s existing healthz-wait budget is also 15-30s
range) — accepted, known false-positive risk: a legitimately slow `oauth_call_api` Gmail fetch
sitting at 35-45s will show amber before it's actually stuck. **Logged to TODOS.md as a named
follow-up** (per-tool-class thresholds, e.g. network calls get a longer allowance than local
`kv_get`), not silently accepted as permanent — Eng phase makes the constant easily overridable
(a named const, not a magic number scattered across files) so tuning it later is cheap.

**Resolved: budget bar rendering.** The prose-only "budget bar" promise (never shown
concretely, design subagent finding) now renders as a 3-cell block-character bar
(`▓▓░`/`▓░░`/`░░░`) reusing `MemoryPressure`'s existing 75/90% thresholds (2 of 3 blocks filled
= 75%+, amber; 3 of 3 = 90%+, red) — same visual language as any progress-bar-style meter,
consistent with the codebase's existing threshold-color convention (0C reuse target).

```
--plain mode equivalent (glyph parity, no color available):
AGENT      STATUS   TURN  LAST-TOOL                          BUDGET  TOKENS   $   AGE FLAG
inbox-07   [RUN]     12   web_search("q3 revenue...")         [==.]   4.2k  .03  2m
gmail-09   [RUN]!     9   [STUCK 34s] oauth_call_api(...)     [=..]   2.1k  .02  4m  !
curator-07 [WAIT]     3   (parked - no tool in flight)         [...]   1.1k  .01  8m
scout-a    [ERR]!     7   ! mcp_error: timeout                 [...]   890  .01  5m  !!
```

**Resolved: first-tool-call-not-yet-made / first-paint-before-first-poll states** (design
subagent finding, dodged in the original interaction-state table): `LAST-TOOL` shows
`"(starting…)"` for an agent alive but pre-first-tool-call (distinct from `curator-07`'s
`"(parked — no tool in flight)"` — one is transient, one is a stable status); the Dashboard
itself shows a one-line `"waiting for first snapshot…"` placeholder (not a blank table) for the
sub-second-to-few-second window between TUI start and the first poll/SSE event arriving.

**Resolved: narrow-terminal column degradation** (design subagent HIGH finding — Pass 1 had
already computed the answer but never wired it into a spec). Drop order at decreasing width:
full → drop `$` → drop `TOKENS` → drop `AGE`, in that order, preserving `AGENT`/`STATUS`/
`LAST-TOOL`/`⚠` (the "if only 3 things could show" answer from Pass 1) down to a documented
minimum of 60 columns (below that, `--plain`'s already-narrower format takes over rather than
further truncating the boxed layout).

**Resolved: agent-table scale (design subagent MEDIUM finding).** When agent count exceeds
visible rows, the Dashboard scrolls (reusing the existing row-selection ↑/↓ scroll model
already used elsewhere in the TUI, e.g. `View::Memory`'s pane scroll) — not paginated, not
truncated silently. No new scroll mechanism invented; this is Eng phase wiring existing
behavior to a taller data set, not a new UX pattern.

**Resolved: `▼ N new` keybinding (design subagent MEDIUM finding).** Pressing `f` again while
frozen both unfreezes AND jumps to the bottom (matching k9s/htop's own single-key
follow-toggle convention from the landscape check) — one key, not two, avoiding a
"which key was it again" tax on the exact moment an operator wants to catch up fast.

Saved as an inline plan artifact (not `~/.gstack/projects/.../designs/`, since this is ASCII
text embedded in the plan file itself, not a generated image — the artifact IS the plan text).

### 7-Pass Review

**Pass 1 — Information Architecture: 6/10 → 9/10.** Original gap: the master plan describes
the table columns and the stream pane but doesn't explicitly state the *priority order* an
operator's eye should follow. **Fix (added above):** the mockup makes it explicit — row color
(is anything red?) is the first-glance signal, `LAST-TOOL` is second (what's it doing), the
stream pane is third (deep evidence), matching "hierarchy as service." Constraint worship: if
only 3 things could show, they'd be status-dot-color, `LAST-TOOL`, and the ⚠ column — which is
exactly what a narrow terminal (Pass 6) needs to preserve.

**Pass 2 — Interaction State Coverage: 7/10 → 9/10.** The CEO phase's Section 11 already
produced a state table; formalizing it here in the canonical shape:
```
  FEATURE           | LOADING              | EMPTY                  | ERROR              | SUCCESS         | PARTIAL
  ------------------|----------------------|-------------------------|--------------------|-----------------|------------------
  Agent row         | N/A (poll-based)     | N/A (row exists only    | Whole row red,     | Green dot,      | N/A
                     |                      | for a running agent)    | ⚠ in last column   | readable text   |
  Stream pane       | "waiting for first   | "no events yet" (ux.9's | Red/orange event   | One-liners      | N/A — both
                     | event…"              | empty-cockpit state)    | lines, existing    | render live      | transports
                     |                      |                         | Inspector coloring |                 | render real
                     |                      |                         |                    |                 | events (SSE for
                     |                      |                         |                    |                 | HTTP, timer-
                     |                      |                         |                    |                 | repoll for FUSE);
                     |                      |                         |                    |                 | see Eng Review's
                     |                      |                         |                    |                 | reversal of this
                     |                      |                         |                    |                 | table's original
                     |                      |                         |                    |                 | (stale) FUSE-only
                     |                      |                         |                    |                 | framing
  AgentDetail       | N/A                  | "no activity yet" (just | Persistent error   | ~10-event       | N/A
   timeline         |                      | spawned)                | strip, sticky       | timeline        |
```
Empty states specified with actual copy, not "TBD" — satisfies "empty states are features."

**Pass 3 — User Journey & Emotional Arc: 8/10.** No fix needed — the CEO phase's journey
narrative already covers this (glance → notice red → select → scope stream → investigate →
drill into Detail). Emotional arc: from "vague unease" (is anything wrong?) to "located
confidence" (I know exactly what's wrong and where) in under 3 interactions — matches the 5-sec
visceral (row color) / 5-min behavioral (select-and-investigate) time-horizon split well. No
"5-year reflective" claim needed for an internal operator tool.

**Pass 4 — AI Slop Risk: 9/10.** None of the AI-slop blacklist patterns apply structurally (no
gradients/cards/icons-in-circles — this is a terminal, that whole vocabulary doesn't exist
here). The one relevant universal rule — "cards only when card IS the interaction" — is
already satisfied (the agent table IS a data table, not a card grid dressed up as one). Copy
check: `LAST-TOOL` renderings (`web_search("q3 revenue…")`) are utility-language, specific to
this product's actual tool vocabulary, not generic ("Loading..." / "Please wait" avoided).

**Pass 5 — Design System Alignment: 5/10 → 6/10 (residual gap, logged not fixed here).** No
written DESIGN.md-for-UI exists (0B above). The plan DOES align with the established in-code
color convention (status dots, threshold-based amber/red) — consistency is real, just
undocumented. Recommend `/design-consultation` to formalize (TODOS.md, not this increment's
job — matches CEO Section 10's "knowledge concentration" finding, same underlying gap named
twice from two different review angles, which is itself a signal it's worth prioritizing soon).

**Pass 6 — Responsive & Accessibility: 5/10 → 8/10.** **Finding, fixed above:** terminal-width
degradation wasn't specified. Fix: the `--plain` mockup above IS this codebase's actual
accessibility mechanism (color-independent, screen-reader-adjacent via plain text) — already
required by the master plan ("Glyph + color always"). Remaining gap: exact column-truncation
behavior at narrow widths (`LAST-TOOL`'s text is variable-length; what truncates first at
80 columns?) is not specified. **Logged as an Eng-phase task**, not resolved here (a design
*intentionality* check, not a pixel-level spec — matches this skill's own stated scope
boundary, "not a pixel-level audit — that's /plan-design-review's own deeper mode / actual
implementation"). Keyboard nav: reuses existing row-selection model, unaffected.

**Pass 7 — Unresolved Design Decisions:**
```
  DECISION NEEDED                          | RESOLUTION
  ------------------------------------------|---------------------------
  Exact LAST-TOOL truncation order at       | RESOLVED above: drop $ -> TOKENS ->
  narrow terminal widths                    | AGE, floor at 60 cols, then --plain
  Idle-amber threshold value                | RESOLVED above: 30s global default,
                                            | named const, per-tool-class deferred
  Freeze + agent-switch interaction         | STILL OPEN — genuinely ambiguous,
  (does switching agents auto-unfreeze?)    | left for Eng phase to design (not a
                                            | CEO/Design-level call)
  LAST-TOOL max truncation length + exact   | STILL OPEN — Eng phase picks (e.g.
  ratatui color values for "amber"          | Color::Yellow vs Rgb) and records the
                                            | choice in a code comment citing this
                                            | plan, per Section 10's provenance note
```
Two of four resolved directly in this pass (the two flagged CRITICAL/HIGH by the design
subagent); two remain genuinely open, correctly left for Eng phase rather than force-decided
at the design level where they don't belong (an exact `Color::Rgb` value is an implementation
choice, not a design-intentionality question).

### Grade-inflation correction (Claude subagent design review, self-critique accepted)

The design subagent flagged that Pass scores climbed steadily (5→6, 5→8, 6→9, 7→9) while
several underlying gaps remained factually unresolved at review time — legitimate criticism,
accepted without argument. Re-scoring honestly now that the CRITICAL/HIGH fixes above are
actually in the plan (not just narrated as "will fix"):

- **Pass 1: 6/10 → 9/10** (now genuinely earned — the 4th mockup row + augment-not-replace
  resolution directly closes the CRITICAL gap; this score was previously asserted before the
  fix existed, now it reflects the fix).
- **Pass 2: 7/10 → 9/10** (genuinely earned — first-tool-call and first-paint states now
  explicit, not "N/A"-handwaved).
- **Pass 5: 5/10 → 6/10, honestly still 6, not artificially higher** — the DESIGN.md gap is
  real and un-fixed (correctly deferred to `/design-consultation`, out of this increment's
  blast radius) — this score is NOT inflated, it accurately reflects a real, acknowledged,
  intentionally-deferred gap.
- **Pass 6: 5/10 → 9/10** (was 8, revised UP to 9 now that width-degradation has a concrete,
  wired answer, not just "logged as an Eng-phase task" — the subagent's critique was that 8/10
  was claimed while the gap was still open; now the gap is closed, so 9 is earned, not
  inflated).

### Design Dual Voices

**Claude subagent (design — independent review):** ran (foreground). Findings: 1 CRITICAL
(idle-amber augment-vs-replace ambiguity + missing motivating-scenario mockup row), 2 HIGH
(missing threshold value, missing width-degradation wiring), 6 MEDIUM (budget bar not rendered,
first-tool-call/first-paint states dodged, scale/scroll unaddressed, `▼ N new` keybinding
unstated, color-blind-palette-vs-`--plain` conflation, truncation-length/color-value constants
unpinned) — 8 of 9 findings fixed directly above; 1 (color-blind palette) addressed by noting
`Waiting`'s already-distinct status text as the (previously accidental, now explicitly named)
accessibility mechanism, closing the "silent regression risk" the subagent flagged. Also
flagged a legitimate process critique (grade inflation) — accepted and corrected above.
Recommendation was "send back for one more pass" — done, in place, not deferred.

**CODEX SAYS (design — UX challenge):** ran (Bash, sequential, included CEO-phase context per
autoplan's cross-phase rule) — against the plan snapshot *before* the Claude-subagent-driven
fixes above landed. Most findings (narrow-width truncation order, threshold value) were
already resolved by the time Codex's pass completed; three findings are genuinely new and not
covered by the Claude subagent's pass:

1. **Stream-pane FUSE/HTTP asymmetry must be an explicit acceptance criterion, not buried in
   the rescue registry** — valid, fixed above (acceptance criteria amended).
2. **Freeze-on-agent-switch must be decided now, not deferred** — Codex argues correctly that
   this "defines the product feel," not an implementation detail. **Resolved (auto-decided,
   P3 pragmatic + P6 bias-toward-action):** switching the selected agent row while frozen
   **resets the freeze and re-scopes to the newly selected agent** (not "keep frozen on the old
   agent's history"). Reasoning: the row-selection action is itself an explicit "I want to look
   at THIS agent now" signal — preserving a stale freeze on the previously-selected agent after
   the operator has visibly moved their attention elsewhere would silently show them the wrong
   agent's frozen history, a worse default than losing the freeze. An operator who wants to
   keep watching the old agent's frozen stream can freeze again after switching back.
3. **Error recency + color-token/row-selection interaction need concrete formats** — valid, both
   fixed: `error_count`/`last_error_at` render as `⚠ 3 · 2m ago` (single compact string, amended
   into acceptance criteria above); row-selection highlight uses ratatui's existing reversed/
   highlighted-background convention (already used for the currently-selected row elsewhere in
   this TUI, e.g. the Memory/Topology views' row highlighting) layered *underneath* the
   severity color (red/amber text keeps its color; only the background/reversal changes when
   selected) — the two are orthogonal channels (foreground severity vs background selection),
   not competing for the same visual channel, so they don't fight each other by construction.

Codex's overall verdict ("Revise... narrow-terminal behavior and stream/freeze semantics still
left to implementer taste") is now largely satisfied — the two specific gaps it named
(narrow-terminal behavior, freeze semantics) are both resolved above, the first having already
been fixed via the Claude subagent's pass before Codex's read completed.

### Design Review — "NOT in scope"

- Formalizing a written UI design-token reference (`/design-consultation`) — real gap (Pass 5),
  deferred to TODOS.md, out of this increment's blast radius.
- Per-tool-class idle thresholds (network vs local compute) — deferred to TODOS.md alongside
  the 30s global default.
- Full pixel-level visual audit — this is a plan-stage design-intentionality check, not
  `/design-review`'s post-implementation rendered-output audit (recommended as a follow-up once
  built, per this skill's own stated scope boundary).

### Design Review — TODOS.md updates (proposed)
1. `/design-consultation` — formalize the TUI's color/glyph conventions into a written
   reference (closes the Pass 5 DESIGN.md gap). (P3, S effort)
2. Per-tool-class idle thresholds (the 30s global default is a known, accepted
   false-positive-risk simplification). (P3, S effort, depends on the 30s default shipping
   first and being observed in practice)

### Design Completion Summary

```
+====================================================================+
|         DESIGN PLAN REVIEW — COMPLETION SUMMARY                    |
+====================================================================+
| System Audit         | No UI design system doc; in-code conventions|
| Step 0               | Initial: 7/10. Classifier: APP UI.           |
| Pass 1  (Info Arch)  | 6/10 -> 9/10                                 |
| Pass 2  (States)     | 7/10 -> 9/10                                 |
| Pass 3  (Journey)    | 8/10 (no fix needed)                         |
| Pass 4  (AI Slop)    | 9/10 (no fix needed)                         |
| Pass 5  (Design Sys) | 5/10 -> 6/10 (honest residual gap, deferred) |
| Pass 6  (Responsive) | 5/10 -> 9/10                                 |
| Pass 7  (Decisions)  | 2 resolved, 2 still open (correctly, for Eng)|
+--------------------------------------------------------------------+
| NOT in scope         | written (3 items)                            |
| What already exists  | see CEO phase (0C, reused here)              |
| TODOS.md updates     | 2 items proposed                             |
| Approved Mockups     | ASCII, inline (not PNG — TUI mismatch, noted)|
| Decisions made       | 9 (4 mockup fixes, 3 acceptance amendments,  |
|                       | freeze-reset, selection/severity color split)|
| Decisions deferred   | 2 (design-token doc, per-tool-class threshold)|
| Overall design score | ~6.5/10 -> ~8.4/10 (mean across 7 passes)    |
+====================================================================+
```

**PHASE 2 COMPLETE.** Claude subagent: 9 findings (1 critical, 2 high, 6 medium) — 8 fixed
directly, 1 addressed via existing-mechanism naming. Codex: 3 net-new findings beyond the
Claude pass (2 already resolved by the time Codex ran, 1 required an explicit acceptance-
criteria rewrite) — all 3 resolved. Legitimate grade-inflation self-critique accepted and
corrected. 0 unresolved design decisions remain that belong at the design level (2 genuinely
Eng-level decisions — exact truncation length, exact ratatui color enum values — correctly
left open).
Passing to Phase 3 (Eng Review).

---

## Eng Review (Phase 3)

### Step 0 — Scope Challenge

1. **Existing code leverage:** already mapped in CEO 0B/"What already exists" — `truncate()`,
   `update_snapshot()` pull pattern, `Inspector` filter chips, `pump.rs` channel. Not repeated.
2. **Minimum set of changes:** already established in CEO 0D — all 10 files are load-bearing,
   nothing deferrable without breaking an acceptance criterion.
3. **Complexity check:** 10 files, over the 8-file smell threshold — **already triggered and
   answered in CEO 0D** ("this is the established width of 'add one snapshot field,' not
   scope creep specific to this plan," confirmed against 6 existing fields following the exact
   same 10-file path). Per autoplan's Eng-phase override ("Scope challenge: never reduce, P2"),
   this does not re-trigger a reduction question — the CEO phase already did this analysis with
   actual code verification; re-litigating it here would be redundant, not rigor.
4. **Search check [Layer 1/2/3]:** already done in CEO's landscape check (k9s/htop precedent,
   bounded-drain/coalesced-redraw current best practice — confirmed already implemented in this
   codebase). No new search needed; the architectural pattern here (extend an existing
   pull-projection field) isn't a new pattern requiring fresh research.
5. **TODOS cross-reference:** `cos-ux-01` is the TODO this plan closes (already the plan's
   stated goal). No other open TODOS.md item blocks or is unlocked by this plan.
6. **Completeness check:** the plan is doing the complete version, not a shortcut — all 3
   engineering corrections (in-flight visibility, transport parity documented honestly,
   redaction reworded to an honest boundary) push toward completeness, not away from it.
7. **Distribution check:** N/A — no new artifact type, this extends the existing `agentd`/
   `agentctl` binaries already distributed via this repo's existing release pipeline.

**No scope-reduction gate triggers** (already resolved at CEO phase with code verification).
Proceeding directly to the 4-section review.

### Section 1 — Architecture

**⚠ This section originally described the CEO-phase Correction #1/#2 mechanisms and has been
rewritten below to match what the Eng Dual Voices review (both models, independently) confirmed
against the actual code. See "Eng Review revises Correction #1" / "Eng Review reopens
Correction #2" for the finding narrative; this is the corrected design.**

**Dependency graph (new components + relationships to existing):**
```
agentd/src/scheduler.rs (enqueue_or_defer, AgentEffect::CallTools arm — ~line 1291)
  │ NEW: synchronously stamps last_activity/current_tool(s) on AgentTask from `blocks`
  │      (has &mut state HERE, before spawning the tool-call future — batch-granularity,
  │      not per-tool-within-batch; see F2 below)
  ▼
agentd/src/agent/mod.rs (AgentTask)
  │ new fields: last_activity, current_tool, last_error, error_count, last_error_at
  │ updated at: CallTools dispatch (NEW, batch start), ToolResult/AgentFailed (existing sites,
  │             batch end), spawn_agent/send_message/request_approval effects (NEW sites,
  │             these bypass run_tools_sequential entirely)
  ▼
agentd/src/scheduler.rs (update_snapshot)
  │ reads new fields off AgentTask, same as every existing field
  ▼
surfaces/src/snapshot.rs (AgentSnapshot)
  │ new fields — ⚠ Serialize is a MANUAL impl (`snapshot.rs:189-220`), NOT derived (F3, both
  │ models flagged this independently against my earlier "already derived" assumption): each
  │ new field needs its own `s.serialize_field(...)` call AND a `field_count` bump, or it
  │ silently vanishes from BOTH FUSE and HTTP JSON with no compile error. Test required (see
  │ Section 3).
  ├──▶ surfaces/src/agents_fs.rs (FUSE)          new virtual file: /agents/<id>/activity (JSON)
  │      — 7 separate touch points per F11 (below), not "one bullet": OFF_* const, compile-time
  │      assert! bound, 2 readdir offset arrays, file_name_for_offset match, getattr arm, read
  │      arm, directory-listing tuple list
  └──▶ agentd/src/management.rs (JSON API)        new fields in the existing snapshot JSON
                                                          │
                              ┌───────────────────────────┴───────────────────────────┐
                              ▼                                                       ▼
agentctl/src/watch/reader.rs (FUSE path)                agentctl/src/watch/source.rs (HTTP path)
  read_json() on the new activity file                    parse new JSON fields from management API
                              │                                                       │
                              └───────────────────────────┬───────────────────────────┘
                                                          ▼
                                          agentctl/src/watch/views.rs (render)
                                          + agentd/src/agent/mod.rs (NEW: redaction fn,
                                            plain string/prefix matching, no regex — F6t)
                                          + agentctl/src/watch/app.rs (row-scope/freeze state)

STREAM PANE (separate from the scalar fields above — corrected per "Eng Review reopens
Correction #2"):
  HTTP mode: agentctl/src/watch/pump.rs (EXISTING, ux.0) — sse_loop() already parses flight
             events into AppEvent::Flight; the stream pane is a NEW consumer of an EXISTING
             channel, not new plumbing.
  FUSE mode: agentctl/src/watch/{inspector.rs's read_flight_tail, NEW timer} — periodic
             (~1-2s) re-poll of the flight-log tail; NOT a change to View::Inspector itself
             (which stays load-once, per F4/F6 below), a separate new mechanism reusing its
             read function.
```
Coupling: `AgentTask` gains 3 new update call sites (2 existing + `enqueue_or_defer`'s new
dispatch-time stamp) — a small, justified increase, not a new coupling class. The stream pane
adds one new consumer to an existing channel (`pump.rs`) and one new timer-driven caller of an
existing function (`read_flight_tail`) — no new coupling class there either.

**Shadow paths for the new data flow (CallTools dispatch → last_activity):**
```
  CallTools(blocks) arrives at enqueue_or_defer ──▶ last_activity = "running: {tool,...}" (batch
  start, synchronous, &mut state) ──▶ future spawned ──▶ [batch runs, opaque to AgentTask until
  it resolves] ──▶ EffectResult::Tools handling calls provide_tool_results ──▶ last_activity /
  error_count / last_error_at updated again from the batch's actual results (batch end)
       │                                    │                                      │
       ▼                                    ▼                                      ▼
  [blocks contains        [F2: a 2nd, 3rd... tool in the same    [batch's Vec<Block::ToolResult>
   >1 ToolUse? Then        batch cannot update last_activity      lacks tool names (F5 in the
   last_activity shows      mid-batch — this is an accepted,       subagent's terms) — not a
   the whole batch's        stated scope boundary (logged          problem for THIS field,
   tool list for the        below), not silently dropped]          since last_activity was
   full batch duration]                                            already stamped with names
                                                                     from `blocks` at dispatch,
                                                                     before names were lost]
```
Nil path: `name` on `Block::ToolUse` is a non-optional `String` field (verified:
`agent/mod.rs:804`'s destructure `let Block::ToolUse { id, name, input } = block`) — cannot be
nil/absent. Empty path: an empty tool-arg JSON (`{}`) truncates/redacts to an empty string
gracefully (existing `truncate()` already handles empty input). Error path: `registry.invoke()`
erroring already updates `last_error`/`error_count`/`last_error_at` at the existing
`ToolResult{is_error:true}` site, now explicitly per-result-in-batch (F8: `error_count`
increments once per erroring result, not once per batch; `last_error`/`last_error_at` take the
*last* error encountered when a batch has more than one).

**Single points of failure:** none new — `update_snapshot` was already a SPOF for the whole
snapshot (a panic there already takes down every field, not just the new ones); adding 4 more
`Option<T>`/plain-field reads doesn't change that risk profile.

**Rollback posture:** `git revert`, no data migration — matches CEO Section 9's conclusion.

**What would make this beautiful (SELECTIVE EXPANSION addition):** the `last_activity` struct
itself (tool name + args preview + result preview + timestamp) as a single typed Rust struct
(not 4 separate loose fields) would let `update_snapshot`, the FUSE serializer, and
`views.rs`'s renderer all share one `Display`/formatting impl instead of three ad-hoc string
constructions — worth doing NOW (P5 explicit, in blast radius, not scope creep) rather than
after 3 call sites have already diverged.

**No findings requiring AskUserQuestion** — auto-decided (the struct-vs-4-fields choice is
Mechanical per P5, not Taste; logged to the Decision Audit Trail).

### Section 2 — Code Quality

**DRY:** the single `is_credential_shaped()`-style redaction check should live in ONE function
called from the ToolCall/ToolResult/AgentFailed update sites (3 call sites, 1 function) — not
copy-pasted 3 times. Flagging explicitly since Section 5 of the CEO's 11-section review already
named the cross-crate OTEL-dedup question; THIS is the narrower, definitely-in-scope DRY
requirement (one function within `agentd`, not a cross-crate one).

**Naming:** `LastActivity` (proposed struct name, per Section 1's beautification finding) — self-
describing, matches `AgentSnapshot`'s existing plain-English convention.

**Error handling patterns:** cross-references Section 2 of the CEO's Error & Rescue Registry —
consistent, no new pattern introduced.

**Missing edge cases (beyond what CEO phase + Design phase already found):**
- What happens to `last_activity`/`idle_secs` across a checkpoint restore (p3.2)? `AgentTask`'s
  other fields (turn, context_tokens) already survive restore via `to_checkpoint`/
  `from_checkpoint`. **Finding:** the plan doesn't state whether `last_activity` is
  checkpointed or reset-on-restore. **Auto-decided (P5 explicit, P3 pragmatic):** do NOT
  checkpoint it — on restore, `last_activity` starts `None`/`idle_secs` starts from the restore
  timestamp, matching this codebase's existing precedent that `last_pressure` (memory-context
  tracking) is explicitly NOT checkpointed ("resets to None on restore, correct behavior" per
  this repo's own p5.2 CHANGELOG entry) for the same reason: it's runtime-observability state,
  not durable agent state.
- **F7 (Eng Review, Claude subagent) — `error_count`/`last_error_at` do NOT share
  `last_activity`'s reasoning and must be decided separately.** `last_pressure` is a soft,
  in-turn advisory with zero cross-restart diagnostic value — resetting it is free. A chronic
  failure counter is the opposite case: an operator restarting `agentd` after a crash most wants
  to know "this agent errored 12 times before the crash," and that's exactly when the value is
  highest. **Revised decision:** `error_count`/`last_error_at` (and `last_error`'s text) ARE
  checkpointed (added to `AgentCheckpoint`, `FORMAT_VERSION` does not need a bump since these are
  new fields with `#[serde(default)]`, matching the p6.4 `parent_map` precedent for backward-
  compatible checkpoint field additions). `last_activity`/`current_tool`/`idle_secs` remain
  NOT checkpointed (pure runtime-observability, no diagnostic value once the process that was
  running the tool no longer exists).
- **F8 (Eng Review, Claude subagent) — batch-result aggregation semantics were unspecified.**
  When `provide_tool_results(results: Vec<Block>, ...)` receives N results from one batch:
  `error_count` increments once per erroring result in the batch (not once per batch — a batch
  with 2 failing tool calls out of 3 counts as 2 errors, matching what an operator would expect
  from "how many times has this agent failed"). `last_error`/`last_error_at` take the *last*
  erroring result in iteration order when a batch has more than one (simplest deterministic
  rule; ties are resolved by position since results already arrive in call order).
- **F2 (Eng Review, both models) — multi-tool-call batches are a stated, accepted scope
  boundary, not a silently-missed case.** A single turn's `blocks` can contain more than one
  `Block::ToolUse` (only `spawn_agent`/`send_message`/`request_approval` are constrained to be
  sole calls per turn). Per Section 1's shadow-path diagram, `last_activity` shows the whole
  batch's tool list for the batch's full duration — it cannot flip from "running: toolA" to
  "running: toolB" mid-batch without a new shared side-channel between `run_tools_sequential`
  (which has no `&mut AgentTask`) and the scheduler, which is out of this increment's blast
  radius. Logged to TODOS.md as a named follow-up (per-tool-within-batch granularity), not
  silently dropped.

**Over-engineering check:** none — Approach A already rejected the generalized event-bus
(genuine over-engineering avoidance, CEO 0D).
**Under-engineering check:** the loose-4-fields-vs-struct question (Section 1) was the one
real under-engineering risk; resolved above.
**Cyclomatic complexity:** `update_snapshot`'s match arm gains straight field reads, no new
branches — no complexity increase to flag.

### Section 3 — Test Review

Diagram and per-item coverage already produced in the CEO phase's Section 6 (11-section
review) — not repeated verbatim. Eng-phase test list, driven by the Eng Dual Voices findings
below (each maps directly to a finding that would otherwise ship silently broken):

1. `last_activity`/`idle_secs` is `None`/reset immediately after `from_checkpoint` — the
   already-specified checkpoint-restore test.
2. **NEW (closes F1/F6a):** a test that exercises the *real* code path — dispatch a `CallTools`
   future via `enqueue_or_defer`, assert `state.agents[id].last_activity` reflects "running"
   **while the future is still unresolved** (mock tool with an artificial delay). This is the
   one test that would have caught the original wrong fix-location immediately, since there
   would be no field to assert against at the location the plan originally named.
3. **NEW (closes F2/F8):** a multi-tool-call-batch test — 2+ ordinary tool calls in one turn,
   one erroring — asserts `error_count` increments once per erroring result (not once per
   batch) and `last_activity` shows the whole-batch tool list for the batch's duration.
4. **NEW (closes F7):** a checkpoint/restore test specifically for `error_count`/`last_error_at`
   — asserts these DO survive `to_checkpoint`→`from_checkpoint`, unlike `last_activity`.
5. **NEW (closes F3):** a serialization test asserting the FUSE JSON and management-API JSON
   both actually contain the new field keys — guards against the manual `impl Serialize` in
   `snapshot.rs` silently dropping a forgotten `serialize_field` call.
6. **NEW (closes F6b, HTTP/FUSE stream-pane parity):** a test per transport — HTTP-mode stream
   pane renders an event delivered via a mocked `AppEvent::Flight`; FUSE-mode stream pane's
   timer-repoll picks up a new line appended to `flight.jsonl` between polls.
7. A multi-agent concurrency test confirming per-agent field writes in the `HashMap<String,
   AgentTask>` model don't cross-contaminate (single-threaded scheduler already rules out a
   data race; this test guards the *keying*, not thread-safety).

### Section 4 — Performance

Already covered in CEO Section 7 (11-section review) — no new findings at the Eng-code level
beyond the dependency correction below.

**Dependency correction (closes the "zero new crates" vs. regex contradiction — F6t):** the
redaction function must NOT use a regex engine. `agentd/Cargo.toml` has neither `regex` nor
`once_cell` today (verified — grep returns nothing), and this plan's own Section 7 (CEO 11-
section review, line ~640) already commits to "zero new crates." The credential-shaped pattern
list (`sk-`, `ghp_`, `AIza`, `Bearer `, etc.) is a small, fixed set of known *prefixes* — a
plain `str::starts_with`/`str::contains` scan (checking each known prefix against substrings of
the input, redacting a fixed-length run after a match) covers the same "known credential-shaped
patterns" acceptance criterion without a regex engine, and is simpler code besides. If a future
increment needs true regex patterns (e.g. matching structure, not just a prefix), that's a new
dependency decision to make explicitly then, not to sneak in via this increment's redaction
helper.

### Eng Dual Voices

Dispatched both models independently, in parallel: Claude subagent (via `Agent` tool,
foreground context isolated) and Codex (via `codex exec`, Bash). Both were given the same
brief — read the plan in full, cross-check every load-bearing technical claim against the
actual source, and report severity-ordered findings. Neither saw the other's output before
reporting.

**Claude subagent — verbatim summary of findings (6 numbered + edge cases + test gaps):**
F1 (CRITICAL): `run_tools_sequential` is a free `async fn` with no `&mut AgentTask`/`self` in
scope — Correction #1's fix location ("update at the ToolCall site") is not implementable as
written; the real mutation point is `enqueue_or_defer`'s `CallTools` arm, and even that only
captures the whole batch at dispatch, not per-tool progress once the loop starts (F2, CRITICAL
— multi-tool batches, never discussed, can't be fixed by (a) alone). F3 (HIGH): `AgentSnapshot`'s
`Serialize` is a manual impl with a hardcoded `field_count`, not derived — a missed field
silently vanishes from both transports, no compile error, no test guards it. F4 (HIGH):
`View::Inspector` loads `flight.jsonl`'s tail once per view-entry and never re-polls — it is not
a live view, so framing it as "the direct precedent" for the stream pane understates the real
cost of that feature. F5 (HIGH): `count_egress_by_agent` (the plan's stated reason for
rejecting Approach B) actually rescans the entire file every call, not "a single cumulative
read" as the plan claimed — a factual correction that doesn't flip the Approach-A decision
(the HTTP-mode hard blocker for B stands independently) but was still wrong as stated. F6
(HIGH): the plan document itself was structurally incomplete at review time — Eng Dual Voices,
Eng Completion Summary, and DevEx Review had not yet run. F7-F11 (MEDIUM): checkpoint policy
for `error_count`/`last_error_at` unexamined separately from `last_activity`; batch-aggregation
semantics unspecified; `THREAT_MODEL.md` missing from the file-count tally; the flagship
Gmail-fetch acceptance scenario is designed to false-positive on day one at the 30s default
threshold; `agents_fs.rs` needs ~7 separate edit points for "one new virtual file," understated
as one bullet. Recommendation: send back for one more Eng pass — Correction #1's fix location
has no mutation point and the plan hadn't yet run the phase most likely to catch that.

**Codex — verbatim-equivalent summary of findings:** independently reached the same CRITICAL
finding as the subagent's F1 (`run_tools_sequential` has no mutable `AgentTask` access; the
scheduler spawns it as a boxed future onto `state.pending`, and the real task stays untouched
until the future resolves) — cited the same call sites (`scheduler.rs` ~1294-1308,
`agent/mod.rs` ~788-852). Additionally flagged, as its own primary finding, that the stream-pane
FUSE/HTTP scoping (Correction #2's resolution) is backwards: `agentctl/src/watch/source.rs`'s
`HttpSource::event_stream_url()` already returns `/api/v1/events`, and `pump.rs` already
consumes SSE into `AppEvent::Flight` — both shipped in ux.0, both missed by the CEO-phase
Claude subagent and by me when resolving Correction #2 the first time. Also flagged (matching
the subagent's F3): `AgentSnapshot`'s manual `Serialize` impl; (matching F5 in substance): the
`AgentFailed`-site citation for error accounting is unreliable since some paths emit
`BudgetExceeded`/`MaxTurnsReached` instead, and `Block::ToolResult` loses the tool name before
`AgentTask` sees it (resolved by Section 1's redesign: names are captured from `blocks` at
dispatch time, before they're lost, not recovered from the post-execution results). Also
independently found: no `regex`/`once_cell` in `agentd/Cargo.toml` today (contradicts the
plan's "zero new crates" claim if redaction uses regex); `InspectorFilter::Errors` matches
`"kind":"tool_error"`/`"kind":"inference_error"`, neither of which exists in `EventKind`.

**Verification against actual code (not taken on faith — see this session's own direct reads):**
Every claim above was independently re-verified by reading the named files directly before
being accepted: `scheduler.rs:700-730` and `:1291-1303` confirm `CallTools` is boxed into
`state.pending` with no `state`/`&mut AgentTask` captured inside the future; `agent/mod.rs:788-852`
confirms `run_tools_sequential`'s signature and its multi-block loop; `snapshot.rs:189-220`
confirms the manual `impl Serialize` with `field_count = 17 + ...`; `agentctl/src/watch/source.rs:47-48,243-244`
confirms `event_stream_url()`'s trait default (`None`) vs. `HttpSource`'s override
(`/api/v1/events`); `pump.rs:76-112,148-250` confirms `spawn_producers` wires
`event_stream_url()` into a real SSE thread parsing into `AppEvent::Flight`, with an explicit
code comment "ux.2 renders it"; `agentd/Cargo.toml` confirms no `regex`/`once_cell` line exists;
`agentd/src/events.rs:9-24` (`#[serde(rename_all = "snake_case")]`) confirms `ToolResult`→
`"tool_result"`, `AgentFailed`→`"agent_failed"`, and that no `ToolError`/`InferenceError`
variant exists in the enum at all — `inspector.rs`'s filter is checking for kind strings that
literally cannot appear in `flight.jsonl`. Every one of these claims held up; none were
rejected or partially rejected.

**Consensus table:**

| Finding | Claude subagent | Codex | Independently confirmed against code | Resolution |
|---|---|---|---|---|
| Correction #1's fix location has no `&mut AgentTask` | ✅ (F1) | ✅ | Yes — `scheduler.rs:1291-1303` | Fixed: moved to `enqueue_or_defer`'s `CallTools` arm, batch-granularity (Section 1 redesign) |
| Multi-tool batches can't get per-tool mid-batch updates | ✅ (F2) | (implied by F1) | Yes | Logged as accepted scope boundary + TODOS follow-up, not silently dropped |
| Stream pane FUSE/HTTP scoping is backwards | — (not raised) | ✅ (primary finding) | Yes — `source.rs`, `pump.rs` | Fixed: reversed, HTTP consumes existing SSE, FUSE gets new timer-repoll |
| `AgentSnapshot::Serialize` is manual, not derived | ✅ (F3) | ✅ | Yes — `snapshot.rs:189-220` | Fixed: called out explicitly in Section 1 + new serialization test |
| `View::Inspector` isn't live; overstated as precedent | ✅ (F4) | (implied) | Yes — `app.rs:561`, `inspector.rs:84-90` | Fixed: stream pane is new work reusing only `read_flight_tail`, not `View::Inspector` itself |
| `count_egress_by_agent` full-rescans, isn't cumulative | ✅ (F5) | — (not raised) | Yes — `reader.rs:234-253` | Corrected wording; Approach-A decision unaffected |
| No `regex`/`once_cell` in `agentd/Cargo.toml` | — (not raised) | ✅ | Yes | Fixed: redaction uses plain string/prefix matching |
| `InspectorFilter::Errors` matches nonexistent kind strings | — (not raised) | ✅ | Yes — `events.rs:9-24` | Pre-existing bug, not this plan's — logged to TODOS.md; ux.2's own filter chip must not copy it |
| Plan document structurally incomplete at review time | ✅ (F6) | — (implicit) | Yes (self-evident) | This section + what follows closes it |

**No User Challenge triggered.** Both models converged on the SAME critical technical bug (F1)
and the stream-pane reversal was found by one model and independently code-confirmed by me
before acceptance — this is dual-voice technical consensus on a concrete implementation defect,
not a disagreement about the increment's direction or the user's already-chosen approach. All
findings above were fixed in-plan, matching the established pattern from the CEO and Design
phases (no finding was silently dropped; nothing here needs the user's judgment call — these are
"the code doesn't work as described" bugs, not taste).

### Eng Review — Decision Audit Trail additions

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|-----------------|-----------|-----------|----------|
| 8 | Eng/Dual-voice | `error_count`/`last_error_at` ARE checkpointed; `last_activity`/`current_tool`/`idle_secs` are NOT (F7 — split from the original single "don't checkpoint" decision) | Mechanical | P5 explicit | Chronic-failure history has highest diagnostic value right after a crash restart; runtime-only activity state does not | Treating all 4 fields identically (the CEO phase's original, unexamined assumption) |
| 9 | Eng/Dual-voice | Batch aggregation: `error_count` +1 per erroring result in a batch; `last_error`/`last_error_at` take the last error in iteration order (F8) | Mechanical | P5 explicit | Matches operator intuition ("how many times has this agent failed"); simplest deterministic tie-break | Once-per-batch counting (undercounts multi-failure batches) |
| 10 | Eng/Dual-voice | Multi-tool-batch mid-batch granularity (F2) deferred to TODOS.md, not built now | Taste (scope boundary) | P2 boil-lakes, P3 pragmatic | Needs a new shared side-channel between `run_tools_sequential` and the scheduler — genuinely bigger than "add a field," matches the same bar the generalized event-bus idea was held to in CEO/0D | Building the side-channel now |
| 11 | Eng/Dual-voice | Inspector's pre-existing `InspectorFilter::Errors` bug is NOT this increment's to fix, logged to TODOS.md; ux.2's own new filter chip must not copy the same kind-string mismatch | Mechanical | P1 completeness | Bug predates this plan and is out of its file set; but silently copying a known-broken pattern into new code would be a fresh mistake, not an inherited one | Fixing Inspector's existing bug as part of this increment (out of blast radius) |

### Eng Review — TODOS.md updates (proposed, additive to CEO/Design's lists)

8. Per-tool-within-batch `last_activity` granularity — requires a new shared side-channel
   between `run_tools_sequential` and `AgentTask` (e.g. `Arc<Mutex<...>>` or a channel the async
   tool loop writes to and `update_snapshot` also reads). (P3, M-L effort — genuine new
   machinery, not a field addition)
9. Fix `agentctl/src/watch/inspector.rs`'s `InspectorFilter::Errors` to match real `EventKind`
   serialized names (`"kind":"tool_result"` + `is_error:true` in payload, `"kind":"agent_failed"`,
   `"kind":"budget_exceeded"`, `"kind":"max_turns_reached"`) instead of the nonexistent
   `"tool_error"`/`"inference_error"`. Pre-existing bug, found incidentally. (P2, S effort)
10. Consider making `View::Inspector` itself live (auto-tail, not load-once) now that ux.2 builds
    a working timer-repoll mechanism for the FUSE-mode stream pane — the two could plausibly
    share a poller. (P3, S-M effort, purely additive)

### Eng Completion Summary

```
+====================================================================+
|            MEGA PLAN REVIEW — COMPLETION SUMMARY (ENG)             |
+====================================================================+
| Section 1  (Arch)     | 2 CRITICAL bugs found + fixed (F1/F2 fix-location redesign; stream-pane transport reversal) |
| Section 2  (Quality)   | 2 findings (F7 checkpoint-split, F8 batch semantics) — both resolved |
| Section 3  (Tests)     | 7 tests specified (2 pre-existing, 5 new, each traced to a finding) |
| Section 4  (Perf)      | 1 dependency correction (no regex/once_cell — plain string matching) |
| Eng Dual Voices         | 2 models, both independently code-verified; F1 found by BOTH (highest-confidence finding of the whole plan); stream-pane reversal found by 1, confirmed by direct code read before acceptance |
| User Challenge?         | None — technical consensus on implementation bugs, not a direction disagreement |
| Findings fixed in-plan  | 9 (F1-F2 architecture redesign, F3 Serialize test, F4 Inspector-not-live reframe, F5 wording fix, F6t redaction dependency, F7 checkpoint split, F8 batch semantics, F9 Inspector bug logged, F11 agents_fs.rs touch-point count) |
| Findings deferred       | 1 (F2's mid-batch granularity — genuine new machinery, TODOS #8) |
+====================================================================+
```

## PHASE 3 COMPLETE

Eng Review found and fixed the plan's most consequential bug: the CEO-phase fix for in-flight
tool-call visibility targeted a function (`run_tools_sequential`) with no mutable access to the
state it needed to change, and the stream-pane transport decision (resolved twice, in CEO and
reinforced in Design) had the FUSE/HTTP capability story backwards. Both were caught by
dual-voice review exactly as designed — Codex found the stream-pane reversal independently, and
both models converged on the exact same root cause for the ToolCall-site bug. Every finding was
independently re-verified against the actual source before being accepted (matching this
session's established discipline), not taken on either model's word. The plan's core mechanism
is now something that will actually compile and behave as described, not something that reads
plausibly but silently can't work.

Proceeding to Phase 3.5 (DX Review) — detected in scope during Phase 0 (39 keyword matches).

## DX Review (Phase 3.5)

### Step 0 — Product Type + Applicability

**Classification: CLI Tool** (`agentctl watch`, a terminal dashboard) — matches the skill's
auto-detect criteria (CLI commands, flags, terminal). Confirmed, not re-asked from scratch.

**Scope note (auto-decided, P2 boil-lakes):** `agentctl` is a single-operator internal CLI for
one individual's own `agentd` runtime (per CLAUDE.md's locked single-tenant decision) — not a
public SDK/API/library with external developers, a docs site, an upgrade/migration path beyond
normal versioning, or a "community." The skill's 8 passes are scoped down accordingly: Pass 1
(Getting Started), Pass 2 (CLI Design/Usability), Pass 3 (Error Messages), and Pass 6 (Dev
Environment/Tooling — `--plain`, terminal-width) apply directly; Passes 4 (Docs/Learning), 5
(Upgrade/Migration), 7 (Community/Ecosystem), 8 (Measurement/Feedback loops) don't have a
real referent here and are explicitly out of scope, not silently skipped.

**Reduced dual-voice ceremony (auto-decided, P3 pragmatic):** rather than two fresh full
subagent dispatches re-litigating UI decisions the CEO and Design phases already reviewed in
depth, both DX passes were scoped narrowly to the 4 applicable passes' actual DX-specific
questions (discoverability, error-message clarity, `--plain` parity, width degradation) against
the plan as it stood after Eng Review. This is a real, load-bearing scoping choice, not skipped
ceremony — logged here rather than silently assumed.

### Passes 1/2/3/6 findings

Findings are reported directly in the Dual Voices section below (both models were dispatched
against these 4 passes' questions specifically, not asked to produce pass-by-pass prose
independently of their findings) — this avoids restating the same content twice.

### DX Dual Voices

Dispatched Codex (`codex exec --sandbox read-only`, Bash) and a Claude subagent (`Agent` tool,
foreground-isolated) in parallel, independently, against the same 4-question DX brief. Neither
saw the other's output before reporting.

**Codex — findings (severity-ordered):**
HIGH: stream-pane filter discoverability underspecified — no key documented for cycling filters
in the new pane, unlike Inspector's `Tab:filter(...)` convention (`views.rs:861`). HIGH:
`--plain` parity incomplete for the new stream/timeline — `render_plain` (`views.rs:1159`) only
emits status/context/budget/tool-count today, and the plan never specified stream/timeline
behavior in `--plain`. MEDIUM: terminal-width degradation still leaves implementation choices —
`LAST-TOOL`'s own truncation length unspecified; existing tables use fixed constraints, not
responsive dropping (`views.rs:137`). MEDIUM: stale transport-parity contradiction — the
Design-phase state table (line ~1053) still said "stream pane unavailable in HTTP mode," directly
contradicting the Eng-Review-reversed acceptance criterion; flagged for removal. MEDIUM:
redaction output not operator-explicit enough — no pinned placeholder string (`sanitize()` at
`views.rs:17` only strips control chars, doesn't redact). Recommendation: needs one more pass.

**Claude subagent — findings (severity-ordered):**
HIGH: `--plain` mode has no specified behavior for the stream pane or AgentDetail timeline, and
no interactivity model exists to attach one to — confirmed `--plain` is a non-interactive,
one-shot-per-interval full dump (`mod.rs:95-112`), with no selection/freeze/scroll state at all.
HIGH: the plan's own footer mockup (line ~970) silently dropped `[t]opology`/`[m]emory`/
`[c]reds` while adding `[f]reeze` — a real regression risk if built literally; 3 already-shipped,
currently-discoverable panes would become undiscoverable. MEDIUM: no key assigned for
stream-pane filter-cycling or table↔stream focus switching, despite this codebase's established
Tab-cycles-focus convention (Memory's ShortTerm→LongTerm→Kb, `mod.rs:425-430`; Inspector's own
filter cycling, `mod.rs:574-575`). MEDIUM: width-degradation mechanism has no existing code
precedent and wasn't counted in the plan's own file/effort tally. LOW: no in-TUI legend for the
3 new visual conventions, inconsistent with System/Sandbox/Topology precedent (`views.rs:340,
374-383,412-418`). LOW: pre-existing dead "Errors" filter chip in Inspector (matches the
Eng-Review's already-logged `tool_error`/`inference_error` finding) left unexplained to
operators, right next to the new pane's presumably-correct filter. Recommendation: needs one
more pass.

**Consensus table:**

| Finding | Codex | Claude subagent | Independently confirmed against code | Resolution |
|---|---|---|---|---|
| `--plain` has no stream/timeline spec | ✅ HIGH | ✅ HIGH | Yes — `mod.rs:95-112`, `views.rs:1159` | **Fixed:** unscoped recent-events block + per-agent inline event suffix, no new interactivity invented |
| Footer mockup drops existing pane keys | — (not raised) | ✅ HIGH | Yes — plan's own mockup vs. `views.rs:153` | **Fixed:** restored `[t]/[m]/[c]`, added missing `[i]` (free), new keys on a 2nd footer line |
| Stream-pane filter-cycling key unassigned | ✅ HIGH | ✅ MEDIUM | Yes — `views.rs:861` Inspector precedent | **Fixed:** `Tab` reuses `InspectorFilter::next()`'s cycle order; row-selection remains the stream-scoping key (no separate focus key needed) |
| Terminal-width degradation underspecified | ✅ MEDIUM | ✅ MEDIUM | Yes — `views.rs:137-149`, no existing graduated-drop precedent | **Fixed:** `LAST-TOOL` pinned to a 40-char max; explicitly counted as new logic in an already-touched file, not an 11th file |
| Stale "unavailable in HTTP mode" contradiction | ✅ MEDIUM | — (not raised) | Yes — plan's own line ~1053 vs. its own line ~47 | **Fixed:** removed, replaced with the Eng-Review-consistent parity framing |
| Redaction placeholder string unpinned | ✅ MEDIUM | — (not raised) | Yes — `sanitize()` doesn't redact | **Fixed:** pinned literal `[REDACTED]` |
| No in-TUI legend for new visual conventions | — (not raised) | ✅ LOW | Yes — `views.rs:340,374-383,412-418` precedent | **Fixed:** added as a new acceptance-criterion bullet |
| Inspector's dead Errors filter chip left unexplained | — (not raised) | ✅ LOW | Yes (same root cause as Eng Review's TODOS #9) | No new action — already logged in Eng Review's TODOS; DX pass independently confirms the same defect from the operator-visible angle |

**No User Challenge triggered.** Every finding is a concrete, mechanical DX gap (missing
keybinding, missing `--plain` spec, missing legend, missing width number, missing placeholder
string, a stale contradiction) — none of it is a disagreement about direction or the user's
chosen scope. All fixed in-plan per P1 completeness/P5 explicit, matching the CEO/Design/Eng
phases' established pattern. The strongest convergent finding (`--plain` parity) was found by
both models independently and is now the most-detailed acceptance-criterion amendment in the
whole plan — appropriate, since it was also the deepest actual gap.

### DX Review — "NOT in scope"

Docs-site/tutorial content, upgrade/migration path, community/ecosystem, and DX telemetry/
feedback-loop instrumentation (Passes 4/5/7/8) — no real referent for a single-operator internal
CLI on a single-tenant OS; raising them would be noise, not rigor.

### DX Review — TODOS.md updates (proposed, additive)

12. Consider a dedicated `?`/full-keybinding-help screen if the Dashboard footer keeps growing
    across future increments (ux.1/ux.3/ux.8 will each want their own keys too) — this
    increment's 2-line footer is a stopgap, not a durable pattern. (P3, S-M effort, future)

### DX Scorecard

```
+====================================================================+
|                  DX REVIEW — SCORECARD (Phase 3.5)                 |
+====================================================================+
| Pass 1 (Getting Started)     | 2 HIGH found + fixed (footer regression, filter-cycle key)  |
| Pass 2 (CLI/TUI Design)      | 1 LOW found + fixed (in-TUI legend)                         |
| Pass 3 (Error Messages)      | 1 MEDIUM found + fixed (redaction placeholder string)       |
| Pass 6 (Dev Environment)     | 1 HIGH + 1 MEDIUM found + fixed (--plain parity, width)     |
| Passes 4/5/7/8               | N/A — no referent for a single-operator internal CLI        |
| Dual Voices                  | 2 models, both independently code-verified; 4/8 findings   |
|                               | converged (both models, same root cause); 4/8 unique to one |
| User Challenge?               | None — all findings mechanical, not directional             |
+====================================================================+
```

## PHASE 3.5 COMPLETE

DX Review's strongest finding — both models independently, `--plain` mode has zero specified
behavior for 2 of this increment's 3 new UI surfaces (the stream pane and the AgentDetail
timeline), because `--plain` itself has no selection/interactivity model to attach either to.
This is now resolved with a concrete, scoped design (unscoped recent-events block + inline
per-agent event suffixes) rather than left as a silent gap an implementer would have had to
guess at. The Claude subagent also caught a real regression risk hiding in the plan's own
Design-phase mockup — the proposed footer silently dropped three already-shipped keybindings.
Both are now fixed in-plan.

Proceeding to Phase 4 (Final Approval Gate).
