<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/ux.1-converse-autoplan-restore-20260713-215153.md -->
# ux.1 — Converse: chat the orchestrator + direct any agent

**Track:** UX (operator cockpit)
**Branch:** `ux.1-converse` (off `origin/main` @ `d8f04424`, cred.7/v0.84.0 — ux.2a not yet merged)
**Depends on:** ux.0 (async single-loop refactor, shipped v0.77.0) — done. orch.1/orch.2 (management API spawn/inject + SSE) — done. `agentctl/src/orchestrate.rs` (working CLI REPL) — done.
**Status:** /autoplan in progress, 2026-07-13.

## Goal

Fold the `orchestrate.rs` REPL into the cockpit as a chat view (`[c]` key); retarget any agent;
stream replies live; never hang the UI. Closes the "the cockpit can only watch, not act" gap —
today `agentctl watch` is read-only except for approve/deny; talking to an agent requires
dropping to a *separate*, *exclusive* CLI process (`agentctl orchestrate`), and you cannot
watch and converse at the same time.

## What already exists (verified against shipped code, not the original rough-scope doc)

The rough scope for this increment lives in `docs/plans/ux-cockpit.md` (`## ux.1 —`), written
2026-07-10 before ux.0 shipped. Re-verified against `main` @ `d8f04424` just now — three
corrections to that doc's assumptions:

1. **The event pipe is already fully generic — more so than the rough scope assumed.**
   `pump.rs`'s SSE producer thread (`sse_loop`) parses every `data:` line as a bare
   `serde_json::Value` and forwards it as `AppEvent::Flight(value)` through the *same* bounded
   channel used for snapshots/approvals — there is no event-kind filtering at the producer
   level. `orchestrator_turn_complete`/`text_delta`/`inference_stream_started`/etc. (whatever
   `agentd` emits on `/api/v1/events`) already arrive at `step()` for free. **No new producer
   or plumbing work is needed** — ux.1 is consumer-side only.

2. **`orchestrate.rs`'s core loop is NOT a "factor into a shared helper" job — it needs a
   redesign, not an extraction.** `drain_until_turn_complete()` (orchestrate.rs:138) is a
   *blocking* `for line in reader.lines()` loop over its OWN dedicated SSE connection, reading
   until one specific event for one specific agent arrives, then returning. `watch`'s `step()`
   (mod.rs:323) is a *non-blocking* fold over one `AppEvent` at a time, called from a loop that
   must always fall through to key-poll + redraw within ~30ms (mod.rs:256-260, the anti-livelock
   guard). These are incompatible consumption models — you cannot call
   `drain_until_turn_complete` from `step()` without reintroducing the exact blocking-read
   problem ux.0 eliminated.
   **What IS directly reusable, unchanged:**
   - `source.inject(&agent_id, &text)` / `source.spawn(&SpawnRequest{orchestrated:true,...})` —
     already-shipped `DataSource` trait methods, used as-is.
   - The resume-vs-spawn branching (`agent_alive = snap.agents.iter().any(|a| a.id == id &&
     a.status == "waiting")` → inject; else → spawn) — pure logic, portable verbatim into a
     shared helper.
   - The event-kind knowledge (`orchestrator_turn_complete`, `agent_failed`, `agent_completed`,
     `orchestrator_exited` and their exact JSON field paths, e.g.
     `v["data"]["agent_id"]`/`v["data"]["answer"]`) — reusable as constants/match arms.
   **What must be built new:** an incremental per-target state machine (e.g.
   `ConverseState.awaiting_turn: Option<AgentId>`) that `step()`'s existing
   `AppEvent::Flight(value)` arm checks on every event, transitioning out of "awaiting" the
   moment a matching `orchestrator_turn_complete`/`agent_failed`/`agent_completed`/
   `orchestrator_exited` arrives for the tracked target — never blocking, never looping.

3. **Every existing view in `agentctl` is full-screen, single-pane, switched via the `View`
   enum** (`app.rs`: `view: View`, one field, one active view at a time — Dashboard,
   AgentDetail, System, Topology, Memory, Spawn, Inspector, Approvals, Credentials). The rough
   scope's "Target layout" ASCII mock shows a *permanent split screen* (agent table + chat rail
   + event stream + input box, all visible simultaneously) captioned "assembled progressively
   across increments" — but ux.1's own scope bullets describe `[c]` as a *focus key* ("key `[c]`
   focuses it"), which is the existing single-view-switch idiom, not a persistent split pane.
   **This is an unresolved ambiguity the rough scope doc doesn't actually resolve** — building a
   new split-pane rendering paradigm (a first for this codebase) is a materially bigger,
   riskier lift than adding one more `View::Converse` full-screen entry matching every other
   view. Needs a CEO/Design-level decision, not silently assumed either way.

Everything else in the rough scope's "What already exists" list checks out: the management API
surface (`/api/v1/spawn`, `/api/v1/agents/:id/inject`, SSE `/api/v1/events`) is live and used
today by both `orchestrate.rs` and `watch`'s existing Spawn/Approvals views; `source.rs`
already abstracts FUSE vs HTTP behind `DataSource`; `watch/spawn.rs` still execs a second
`agentd` for the in-cockpit spawn case (that's ux.3's fix, not this increment's).

## Rough scope (from `docs/plans/ux-cockpit.md`, preserved verbatim as input to review)

- `watch/converse.rs` + `ConverseState`; pinned input box + transcript rail; key `[c]` focuses it.
- **Reuse `orchestrate.rs` logic** — factor its spawn-or-resume + `drain_until_turn_complete`
  guards into `source` helpers shared by the CLI REPL and the TUI. First message → if target agent
  is `waiting`, `POST /api/v1/agents/:id/inject`; else `POST /api/v1/spawn` (`orchestrated:true`).
  Reply completion driven by `orchestrator_turn_complete` for that agent off the shared SSE feed.
- **Streaming:** append `text_delta` (and inference stream deltas) to the current turn live.
  Color-coded roles: operator input / assistant / **green = actively streaming** / yellow = system.
- **Target selector:** shown in the input-box border title (`┤ → orchestrator ├` / `┤ → agent:scout-3 ├`).
  Default target = orchestrator; selecting an agent row + a retarget action rebinds the input to
  inject into that agent. (tmux "active pane" model, not a dropdown.)
- **Scroll vs stream:** `follow: bool` — auto-scroll only when already at bottom; `▼ N new` when
  scrolled away; `End`/`G` re-arms follow. `Enter` sends, `Alt+Enter` (or `\`+Enter) newline;
  `↑/↓` = input history when the line is empty, transcript scroll once typing. `Esc`/`Ctrl+C`
  cancels the in-flight stream (map to detach/abort). Per-target input history.
- Surface `orchestrator_exited` / inject-rejected / SSE-timeout as an **inline yellow system line
  with a resume hint** — never swallow, never hang.

**Acceptance:**
- [ ] From inside `watch`, send a message, see a streamed reply, follow up multi-turn — dashboard behind stays live.
- [ ] Retarget to a worker agent and inject into it; the border title reflects the active target.
- [ ] Streaming never yanks the scroll when the operator has scrolled up; `▼ N new` appears; `G` re-follows.
- [ ] An inject rejected while the agent is in-flight shows an inline error + resume hint, no hang.
- [ ] `orchestrate.rs` CLI still works (shared helpers, no behavior regression).

## Test plan (from the project's non-negotiable, ux.1-specific line)

streaming-scroll follow flag; retarget-inject; inject-rejected inline (no hang); CLI REPL
no-regression.

## Premise Gate (Step 0A) — RESOLVED 2026-07-13, REVISED after pause + resequence

Four premises were presented to the user for confirmation (the one CEO-phase gate that
`/autoplan` never auto-decides):

1. Chat belongs **inside** `agentctl watch`, not a separate tmux pane running `orchestrate.rs`.
2. Streaming replies render **live**, token-by-token, not poll-and-show-when-done.
3. Operator can retarget **any** running agent, not just the orchestrator.
4. Layout — **originally accepted as full-screen `View::Converse`**, then reopened (see below).

**User accepted all 4** in the first pass, with Premise 4 resolved toward full-screen
`View::Converse` (matching every existing tab-switched view).

### Mid-review pause — sequencing + layout, both re-litigated

Before Phase 1 completed, both the Claude CEO subagent and Codex (dual-voice pass, run
independently, converged without seeing each other) found a critical, verified problem:
`docs/ROADMAP.md` (lines 64 and 1152) explicitly sequences **ux.2 before ux.1**, with a
stated reason (cred.7's new credential-health signals have no cockpit surface until ux.2
lands) — and `ux.2a-attention` (PR #116) was open, fully green (CI passing, already through
`/review`+`/qa`), and unmerged while this branch sat on the pre-ux.2a tip. Codex additionally
found that `docs/ROADMAP.md:1089` / `docs/plans/ux-cockpit.md:49` record an explicit
**"Locked decision (2026-07-10)"**: the cockpit should be **one unified screen** (agent
table + pinned chat rail + event stream + input box), **"not more `[key]` tabs"** — directly
contradicting the Premise 4 resolution above.

**Resolution 1 (sequencing):** user chose to pause, merge PR #116 to main (verified clean,
CI green, already reviewed — merged as `016cab4a`), then re-cut `ux.1-converse` off the new
`origin/main` tip (zero unique commits existed on the old branch, so this was a clean
re-base with no rebase conflicts). ux.2a-attention's changes (`agentctl/src/watch/app.rs`,
`mod.rs`, `reader.rs`, `source.rs`, `topology.rs`, `views.rs`, `surfaces/src/{agents_fs,
snapshot}.rs` — 887 lines) are folded in before any further ux.1 work proceeds.

**Resolution 2 (layout):** re-litigated on resume, with new evidence: `ux.2a-attention` —
the increment that had JUST shipped — also did not build the unified screen; like `ux.9`
before it, it enriched the existing tab-based `Dashboard` view (attention signals: row
coloring, `last_activity`/`last_error`/idle badges) rather than building the D1 split-pane.
Two increments running had now continued the tab pattern despite the written decision.
**User chose to honor D1 as written** rather than continue the drift: ux.1 builds a
**permanent chat rail on the `Dashboard` view** (not a new `View::Converse` tab). This
is explicitly the higher-effort, higher-risk path (no existing view in this codebase does
split-pane rendering — `render_dashboard` in `views.rs` is currently a single full-width
table, per `header_footer_layout`'s 3-row vertical split only) — accepted consciously, not
by default. **A TODO is filed** (see TODOS.md updates below) to either follow through on
D1 for the *other* 8 views eventually, or to formally retire D1's "not more tabs" language
if the project continues to prefer the tab model elsewhere — leaving it stale a 3rd time
is worse than deciding one way explicitly.

**Net effect on this plan:** Sections 2-10 of the CEO review below (error handling, security,
data flow, code quality, tests, performance, observability, deployment, trajectory) were
authored against `ConverseState`'s logic, which is layout-independent — a per-target state
machine, error rescue table, and security posture don't change based on where the transcript
renders. Those sections stand. **Section 1 (Architecture) and Section 11 (Design & UX) are
revised below** to reflect the rail-on-Dashboard layout instead of a new tab.

## Step 0B — Existing Code Leverage Map

| Sub-problem | Existing code | Reuse plan |
|---|---|---|
| Spawn-or-resume branching | `orchestrate.rs:~40-70` (`agent_alive` check → inject vs spawn) | Port verbatim as a pure function into a new shared helper (e.g. `watch/converse.rs` or a `source` method) — no I/O, trivially portable. |
| Inject/spawn transport calls | `DataSource::inject()` / `DataSource::spawn()` (`source.rs`) | Used as-is by both the CLI (`orchestrate.rs`) and the new rail — zero changes needed to the trait. |
| Turn-completion event recognition | `orchestrate.rs:138-197` (`drain_until_turn_complete`'s match arms: `orchestrator_turn_complete`, `agent_failed`, `agent_completed`, `orchestrator_exited`, exact JSON paths) | Extract the event-kind constants + field-path knowledge into shared match logic; do NOT reuse the blocking loop itself (see below). |
| Generic SSE event delivery | `pump.rs`'s `sse_loop` → `AppEvent::Flight(serde_json::Value)` | Already fully generic, zero changes — confirmed in "What already exists" above. |
| Non-blocking event fold | `mod.rs`'s `step()` (323-355), specifically the `AppEvent::Flight` arm (333-336) | Extend this arm: update `converse_view`'s per-target map **unconditionally** (regardless of active view — matches "dashboard behind stays live" even when the operator is on Topology/Memory/etc.; render only reads it when `app.view == Dashboard`). |
| Per-feature state struct pattern | `app.rs`'s per-view state struct pattern (e.g. `memory_view: MemoryPaneState`) | Add `converse_view: ConverseState` (`HashMap<AgentId, ConverseTarget>` + `active_target` + `rail_focused: bool`, see Section 1) as a new `App` field — same idiom as every other view's state struct, but consumed by `render_dashboard` rather than a dedicated `View` variant (per the layout resolution: rail on Dashboard, not a 10th tab). |
| Input handling / key routing | `mod.rs`'s `step_key()` — Dashboard's key match arms (`s`/`t`/`m`/`n`/`i`/`a`/`c` + `j`/`k`/arrows + Enter, `mod.rs:480-509`) | **CORRECTED post-dual-voice-review:** the rough scope's own `[c]` suggestion collides with the *already-shipped* Credentials hotkey (`mod.rs:507`, footer `[c]reds` — cred.5, v0.68.0, postdates the rough-scope doc). **`Tab` toggles focus** between the agent table and the chat rail instead — reuses the exact idiom `Memory`/`Spawn` already use for intra-view sub-pane cycling (`mod.rs:426,531,597`), free on Dashboard today, zero collision. A new **`r`** key (free on Dashboard) retargets the rail to the currently-selected table row's agent, only while the rail does NOT have focus (see Section 1 revision + Pass 3). |
| Render loop | `views.rs`'s `render_dashboard()` (currently a single full-width table under `header_footer_layout`'s 3-row vertical split) | **New work, not reuse:** `render_dashboard` needs a horizontal `Layout::default().direction(Direction::Horizontal)` split inside its "main content" row — agent table (`Constraint::Min(72)`, protecting the existing 6-column layout) + chat rail (`Constraint::Length(32)`) — the first split-pane layout in this codebase. See Section 1 (revised) for the concrete constraint breakdown, corrected width math below. |

**Net:** the only genuinely new code is (a) `ConverseState` + its incremental state machine
replacing `drain_until_turn_complete`'s blocking loop, and (b) the render function + key
handling for the new view. Everything else is either reused unchanged or a mechanical
port of pure logic. This directly informs 0C-bis below — the "ideal architecture" and
"minimal viable" alternatives differ only in how much of the incremental state machine
gets built now vs. deferred.

## Step 0C — Dream State Mapping

```
CURRENT (main @ d8f04424, pre-ux.1)
  agentctl watch  → read-only dashboard + approve/deny; 9 full-screen views.
  agentctl orchestrate → separate blocking CLI process; one dedicated SSE connection;
                          exclusive with `watch` (can't run both against the same
                          mental model of "what's happening" at once).
  Gap: operator must contextswitch terminals to both watch AND act.

THIS PLAN (ux.1, POST dual-voice revision)
  agentctl watch  → Dashboard gains a permanent chat rail (right, Length(32)) beside
                     the agent table (left, Min(72), unchanged widget/columns).
                     `Tab` toggles keyboard focus into the rail; `r` retargets to the
                     selected table row's agent. Same event pipe (`AppEvent::Flight`)
                     already carries every orchestrator/streaming event — consumer-side
                     only change. Retarget any agent; live token streaming; scroll-vs-
                     follow; inject-rejected surfaces inline, never hangs.
  agentctl orchestrate → unchanged CLI entry point, now backed by the SAME shared
                          spawn-or-resume + event-recognition helpers as the TUI
                          (extracted, not duplicated) — one behavior, two front ends.
  Gap closed: watch + converse in one process, one terminal, one mental model — AND
  fleet visibility is never lost while conversing (the actual D1 "one screen" vision,
  not deferred this time).

12-MONTH IDEAL (per docs/plans/ux-cockpit.md's own vision + ROADMAP Track UX)
  D1's unified cockpit taken to its full literal conclusion: every view (not just
  Dashboard) could in principle keep the rail visible, or the rail could gain its own
  event-stream pane per the original ASCII mock. ux.1 delivers the Dashboard-scoped
  slice of that vision now (see TODO 1 in TODOS.md for the "should this extend
  everywhere" open question) plus ux.8 (budget control) and ux.3 (spawn-on-the-fly)
  round out a cockpit where the operator never needs a second terminal for anything
  short of raw shell access.
```

**Dream state delta:** ux.1 now delivers MORE of the 12-month ideal than the originally
(superseded) full-screen-tab design would have — the rail-on-Dashboard IS a real instance
of the D1 unified-screen vision, just scoped to one view rather than all nine. The
remaining gap (extending it further, or the `ux.5` web-cockpit rewrite) is tracked, not
silently dropped — see TODOS.md and the Claude subagent's Finding 4b in the CEO consensus
table above.

## Step 0C-bis — Implementation Alternatives (MANDATORY, ≥2 approaches)

### Alternative A — "Minimal viable": full-screen view, poll-driven completion only

Reuse `AppEvent::Flight` exactly as delivered; `ConverseState` tracks only
`awaiting_turn: Option<AgentId>` and transitions on the terminal events
(`orchestrator_turn_complete`/`agent_failed`/`agent_completed`/`orchestrator_exited`).
**No incremental token rendering** — the transcript shows the operator's sent message
immediately, then a "..." spinner, then the full reply appears atomically when the
terminal event lands (same granularity `orchestrate.rs` has today, just non-blocking).

- Effort: ~0.5 day CC / ~1 day human.
- Pros: smallest diff; zero new rendering complexity (no partial-message mutation,
  no `text_delta` accumulation bugs to chase); ships the core value (chat without
  leaving the dashboard) fastest.
- Cons: doesn't satisfy the rough-scope's explicit **Streaming** bullet and Premise 2
  (live token-by-token render) — the user already confirmed Premise 2 as a hard
  requirement, so this alternative under-delivers against a decision just locked.

### Alternative B — "Ideal architecture": full-screen view, live streaming accumulation

`ConverseState` additionally tracks an in-progress `current_reply: String` buffer per
target. The `AppEvent::Flight` arm appends `text_delta`/`inference_stream_started`
payloads to `current_reply` live (green "actively streaming" per the rough scope),
flushing to the transcript history on the terminal event. Requires: idempotent handling
of out-of-order or duplicate deltas (SSE has no ordering guarantee across reconnects),
a bounded per-target buffer (cap, e.g. 64KB, mirroring the `text_delta` accumulator
pattern already used server-side in `agentd`'s `AnthropicGateway::parse_sse_stream`),
and clearing `current_reply` on retarget.

- Effort: ~1.5-2 days CC / ~3-4 days human.
- Pros: delivers Premises 2+3 in full; matches the rough scope's explicit acceptance
  criteria ("see a streamed reply"); reuses the exact accumulation pattern `agentd`
  already proved safe for tool-input deltas (p7.2), not a new invention.
- Cons: more surface area (buffer bounds, retarget-mid-stream edge case, duplicate-delta
  idempotency) — see Eng phase Section 4 (Test Review) for the specific edge cases this
  requires covering.

**Decision (auto-decided, P1 completeness dominates in CEO phase):** Alternative B.
Premise 2 was already confirmed by the user as a requirement in the Premise Gate above —
Alternative A would silently under-deliver against a decision just locked, which fails
principle P1 (choose completeness) and is not a genuine tradeoff once Premise 2 is fixed.
This is a **Mechanical** decision, not Taste — there's no reasonable case for A once
Premise 2 is accepted.

---

# CEO Deep Review — 11 Sections

Mode: **SELECTIVE EXPANSION** (confirmed 0F). Scope for this section: `ConverseState`
+ `View::Converse` + shared spawn/inject helper extraction + streaming accumulation
(Alternative B, locked above). All auto-decisions below use the 6 principles; CEO-phase
tiebreak is P1 (completeness) + P2 (boil lakes).

## Section 1: Architecture Review (REVISED — rail-on-Dashboard, not a new tab)

**System architecture (new components + relationships to existing):**
```
                          ┌─────────────────────────────┐
                          │      agentctl watch          │
                          │  (existing process, unchanged│
                          │   entry point)                │
                          └──────────────┬────────────────┘
                                         │
                    ┌────────────────────┼─────────────────────┐
                    │                    │                      │
             pump.rs (existing,          │              app.rs (existing)
             UNCHANGED)                  │              + NEW: converse_view field
             sse_loop → AppEvent::Flight │                (HashMap<AgentId, ConverseState>
                    │                    │                 + active_target + rail_focused: bool)
                    │                    │              NOTE: no new `View` variant —
                    │                    │              `app.view` stays `Dashboard`
                    │                    │                      │
                    └───────────►  mod.rs step()  ◄──────────────┘
                                  (existing fold fn,
                                   EXTENDED: Flight arm updates
                                   converse_view unconditionally,
                                   Dashboard's step_key() arm gains
                                   rail-focus routing — see State
                                   Machine below)
                                         │
                        ┌────────────────┼─────────────────┐
                        │                                   │
                 NEW: watch/converse.rs                source.rs (existing,
                 - spawn-or-resume helper               UNCHANGED)
                   (ported from orchestrate.rs)          DataSource::inject()/spawn()
                 - shared by BOTH watch AND orchestrate.rs
                        │
                 orchestrate.rs (existing CLI,
                 MODIFIED: now calls the shared
                 helper instead of its own copy —
                 net LOC decrease)
                        │
                 views.rs's render_dashboard()
                 MODIFIED (not additive): main-content
                 row gains a Horizontal Layout split —
                 agent table (Constraint::Min(72), protects
                 the existing 6-column widget UNCHANGED)
                 | chat rail (Constraint::Length(32), NEW
                 render_converse_rail() fn) — FIRST split-
                 pane layout in this codebase (see below;
                 CORRECTED from an initial Percentage(65/35)
                 draft — a fixed split would crush the
                 6-column table below its own floor, see
                 the width math in Design Pass 1/6).
```
**Coupling assessment:** two new couplings. (1) `mod.rs` → `watch/converse.rs` and
`orchestrate.rs` → `watch/converse.rs` — justified, the DRY fix the rough scope explicitly
calls for, moving from "two independent copies that can drift" to "one shared function, two
callers." (2) **NEW, not present in the full-screen-tab design:** `render_dashboard()` itself
now depends on `converse_view` state — previously each view's render fn only read its own
state struct; `render_dashboard` becomes the first render fn reading a SECOND feature's state
(agent table data + chat rail data) in the same frame. This is the direct cost of honoring
D1's "one screen" decision — it is real, new coupling, not free, and is the reason every
prior increment (ux.0's prerequisite refactor, ux.9, ux.2a) deferred it. Accepted consciously
per the Premise Gate's Resolution 2 above. No new coupling to `agentd`, `surfaces`, or
`sandbox` — this is 100% `agentctl`-internal.

**Minimum terminal width:** a horizontal split of an already-dense agent table needs a floor.
Following Topology's precedent (`MIN_TOPOLOGY_WIDTH: u16 = 60` in `views.rs:25`), the rail
needs its own `MIN_RAIL_WIDTH` guard — below it, `render_dashboard` should collapse to the
table-only layout with the rail hidden (not squeezed unreadable), and `Tab` should show an
inline "terminal too narrow for chat rail" hint rather than silently rendering garbled
columns. **This is a genuinely new finding versus the full-screen-tab design**, which had
no such constraint (every existing view already handles its own single-pane width). Flagged
for Phase 2 (Design Review) to size concretely.

**State machine — `ConverseState` (per-target, keyed by `AgentId`):**
```
        ┌──────────┐   send message    ┌───────────────┐
        │   IDLE    ├──────────────────►│  DISPATCHING  │  (inject or spawn in flight,
        └────┬─────┘                    └───────┬───────┘   before any event arrives)
             ▲                                   │
             │                          text_delta arrives
             │                                   ▼
             │                          ┌───────────────┐
             │                          │  STREAMING     │  (current_reply accumulating)
             │                          └───────┬───────┘
             │                                   │
             │                    orchestrator_turn_complete /
             │                    agent_completed
             │                                   │
             └───────────────────────────────────┘
                        (flush current_reply → history, back to IDLE)

  Invalid/exceptional transitions and what prevents them:
  - DISPATCHING → DISPATCHING (double-send): input box is cleared and Enter is a no-op
    while state != IDLE for the active target — prevents double-submit.
  - STREAMING → IDLE via agent_failed / orchestrator_exited: same terminal-flush path,
    but tags the flushed content as a system/error line instead of an assistant reply
    (inline yellow, per rough scope's explicit requirement — never silently swallowed).
  - Retarget while DISPATCHING/STREAMING: allowed (tmux "active pane" model per rough
    scope) — the PREVIOUS target's state machine keeps running independently in the
    background; only the input box's binding changes. This means ConverseState must be
    keyed per-target (HashMap<AgentId, ConverseState>), not a single global — this is
    the one architectural detail the rough scope's prose doesn't spell out explicitly,
    flagged here as a CEO-level finding.
```

**FINDING (auto-decided — Mechanical, P1 completeness):** the rough scope describes
`ConverseState` in the singular ("ConverseState" struct), but the retarget-mid-stream
acceptance criterion ("Retarget to a worker agent and inject into it... dashboard behind
stays live") only works correctly if each target's streaming state is independent —
otherwise retargeting away from a STREAMING orchestrator mid-reply would either block the
retarget or silently drop the in-flight stream. **Decision:** `converse_view` holds
`HashMap<AgentId, ConverseState>` (one entry per target ever conversed with this session),
plus `active_target: AgentId` for which one the input box is bound to. This is a Mechanical
decision (not Taste) — the acceptance criteria only work one way.

**Scaling:** breaks first under "operator has 50+ agents and converses with many
concurrently" — the `HashMap<AgentId, ConverseState>` is unbounded today. At 10x (500+
agents), this is still trivially small (a few KB of state per target) — not a real
scaling concern for a single-operator OS. At 100x, still fine; this is bounded by how
many agents a human can converse with, not a data-scale problem.

**Single points of failure:** the shared SSE connection (`pump.rs`'s `sse_loop`) is
already a SPOF for every view, not new to this increment — if it drops, Converse degrades
identically to how Dashboard/Topology/Memory already degrade (existing `ProducerDied`
handling). No new SPOF introduced.

**Security architecture:** no new endpoint, no new auth boundary. The TUI calls the exact
same `source.inject()` / `source.spawn()` that `orchestrate.rs` and the existing Spawn view
already call — same trust boundary (single-tenant, operator-only, per CLAUDE.md's locked
decision #2). The only new "who can call what" question is answered identically to every
other view: whoever is running `agentctl watch` on this machine.

**Production failure scenario:** SSE connection drops mid-stream (network blip, agentd
restart). Plan accounts for this: `orchestrator_exited`/timeout surfaces as an inline
yellow system line with a resume hint (explicit rough-scope requirement, carried into
Section 2 below) — never a silent hang, matching the existing `ProducerDied` pattern
already proven in production by every other view.

**Rollback posture:** 5/5 trivially reversible — this is a pure client-side, single-binary
CLI addition. `git revert` removes the view; no data migration, no server-side change, no
feature flag needed (see Section 9).

**EXPANSION/SELECTIVE addition — what would make this beautiful:** the per-target
`HashMap<AgentId, ConverseState>` design is exactly the kind of infrastructure that makes
this a platform other features build on — the same "one state machine per live target,
addressed via `AgentId`" pattern is reusable for any future feature that needs
per-agent incremental UI state (e.g. a hypothetical future "live tool-call feed" view).
Nothing further needed to capture this — it falls out of doing Section 1's finding correctly.

**No issues beyond the finding above** — architecture is otherwise a clean, additive
extension of an already-proven pattern (every other view already does exactly this:
own state struct + render fn + key handler).

## Section 2: Error & Rescue Map

```
  METHOD/CODEPATH                    | WHAT CAN GO WRONG                  | EXCEPTION CLASS
  ------------------------------------|-------------------------------------|------------------
  converse::dispatch (inject path)   | agent no longer "waiting" (raced)  | InjectRejected
                                      | HTTP/FUSE transport error          | TransportError
  converse::dispatch (spawn path)    | spawn quota / capability denied    | SpawnRejected
                                      | transport error                    | TransportError
  ConverseState::on_flight_event     | duplicate text_delta (SSE replay)  | (not an error — idempotency)
                                      | out-of-order delta after terminal  | (not an error — ignored)
                                      | current_reply exceeds buffer cap   | BufferOverflow
  SSE connection (shared, existing)  | drops mid-stream                   | ProducerDied (existing)
                                      | orchestrator_exited mid-turn       | OrchestratorExited
                                      | agent_failed mid-turn              | AgentFailed
  Input handling                     | Enter while DISPATCHING/STREAMING  | (prevented, not an error)
  ------------------------------------|-------------------------------------|------------------

  EXCEPTION CLASS      | RESCUED? | RESCUE ACTION                                    | USER SEES
  ----------------------|----------|--------------------------------------------------|------------------
  InjectRejected        | Y        | Flush as inline yellow system line + resume hint | "Agent no longer waiting — press Enter to start a new turn" (or similar)
  SpawnRejected         | Y        | Same inline yellow system line pattern           | "Spawn rejected: <reason>"
  TransportError        | Y        | Same inline yellow system line pattern           | "Connection error — retry with Enter"
  BufferOverflow        | Y        | Truncate current_reply at cap, flush what's held | Reply appears truncated with a "[...truncated]" marker, never a crash
  ProducerDied          | Y        | Existing `ProducerDied` handling (all views)     | Existing degraded-mode banner (unchanged)
  OrchestratorExited    | Y        | Inline yellow system line + resume hint          | "Orchestrator exited — Enter to resume" (explicit rough-scope requirement)
  AgentFailed           | Y        | Inline yellow system line, tagged as error       | "Agent failed: <reason>" in red/yellow
  duplicate/out-of-order delta | Y (by design) | Idempotent append keyed by turn+seq if available, else last-write |  Invisible to user (transparent)
```

**GAP check:** zero unrescued (N) rows — every new failure path either flushes an inline
system line (never silent) or is a no-op by design (duplicate delta). This is a direct,
mechanical consequence of reusing the existing "never hang, never swallow" idiom the
codebase already established in `pump.rs`'s drop-on-full backpressure and `mod.rs`'s
`ProducerDied` handling — no new pattern invented, same rescue posture as everywhere else.

**One design decision flagged (auto-decided, Mechanical, P1):** SSE has no strict ordering
guarantee across reconnects. Without a per-turn sequence number in the delta payload, true
idempotent de-dup isn't possible — the fallback is "last write wins" on the buffer position,
which is safe (no crash, no corruption) but could theoretically show a duplicated delta
in a rare reconnect-mid-stream race. **Decision:** accept last-write-wins for v1 (matches
what `agentd`'s own SSE parsing already does — no ordering guarantee assumed there either);
flag as a TODO if a sequence number becomes available in a future `agentd` release.

## Section 3: Security & Threat Model

**Attack surface expansion:** none. No new HTTP endpoint, no new FUSE file, no new MCP
capability. The TUI's new "send arbitrary text to an agent" affordance is functionally
identical to what `agentctl inject <id> <text>` already does today (p7.3, shipped) — this
increment adds a UI, not a new capability.

**Input validation:** operator-typed text is passed through to `source.inject()` /
`source.spawn()` unchanged — the same validation (or lack thereof) that the existing
`agentctl inject` CLI already has applies here; no new validation surface. Unicode/length
edge cases: ratatui's `Paragraph` wrapping handles arbitrary UTF-8 already (proven by every
other view rendering `task_preview` strings, which are operator/agent-controlled text).

**Authorization:** none needed beyond what already exists — single-tenant, operator-only
(CLAUDE.md locked decision #2, mutually-trusting in-process agents). No direct-object-
reference concern: `AgentId` targeting already works this way in Spawn/Approvals/Inject.

**Secrets/credentials:** none touched. No new env vars, no new credential provider.

**Dependency risk:** zero new crates. Reuses `ratatui`/`crossterm` (already a dependency)
and the existing `DataSource` trait.

**Data classification:** operator-typed chat text is the same class as `task_preview` /
approval `args_json` already displayed today — no PII/payment/credential handling change.

**Injection vectors:** no SQL (no DB in this crate), no shell exec, no template injection.
LLM prompt injection is out of scope for this increment — the text is a first-class user
turn (exactly what `agentctl inject` already sends), not concatenated into a system prompt
in a new way.

**Audit logging:** already covered — every inject/spawn call already emits flight events
server-side (`ControlInjected`, `OrchestratorDispatched`, etc., shipped in p7.3/orch.1).
No new audit gap.

**No findings** — this section produced zero new attack surface. Examined: endpoint
surface, input validation path, authz model, secrets, dependencies, injection vectors,
audit trail — all inherit unchanged from already-shipped, already-reviewed code paths
(p7.3 approval gate review, orch.1/orch.2 hardening passes).

## Section 4: Data Flow & Interaction Edge Cases

**Data flow (message send → transcript display):**
```
  INPUT ──▶ VALIDATION ──▶ DISPATCH ──▶ AWAIT ──▶ RENDER
    │            │              │            │         │
    ▼            ▼              ▼            ▼         ▼
 [empty       [target still  [transport   [never    [buffer
  string?]     waiting?]      fails?]      arrives?]  overflow?]
 → Enter is   → inject vs    → inline     → operator → truncate +
   a no-op      spawn         yellow        can retry   marker
   on empty     branch        system line   (Esc/      (Section 2)
                (0B)                        Ctrl+C
                                             cancels)
```
Every shadow path routes to Section 2's rescue table — no new unhandled node.

**Interaction edge cases:**
```
  INTERACTION            | EDGE CASE                          | HANDLED? | HOW?
  ------------------------|-------------------------------------|----------|--------
  Send message            | Double-Enter (double-submit)        | Y        | Enter no-op while state != IDLE (Section 1 state machine)
  Send message            | Empty input                         | Y        | Enter no-op on empty string
  Streaming reply          | Operator scrolls up mid-stream      | Y        | `follow: bool` disarms (rough scope, explicit) — no yank
  Streaming reply          | Operator retargets mid-stream       | Y        | per-target HashMap keeps prior target's stream running (Section 1 finding)
  Streaming reply          | SSE reconnects mid-stream           | Y        | last-write-wins buffer (Section 2), never crashes
  Retarget                 | Retarget to an agent that's Done/Failed | Y    | inject path's existing `agent_alive` check → falls to spawn path, which itself may reject → Section 2's SpawnRejected row
  Scroll                   | `↑/↓` with input line non-empty      | Y        | rough scope: input history vs transcript scroll disambiguated by "line empty" check (same idiom as shell history)
  Cancel                   | Esc/Ctrl+C while DISPATCHING         | Y        | maps to detach/abort per rough scope — transitions to IDLE, no dangling state
  Background (Dashboard not active) | Converse target hits terminal event while operator is on Topology/Memory/etc. | Y | ConverseState still updates (event fold is view-independent); rail is simply not rendered until the operator switches back to `View::Dashboard` — no data loss. (If Dashboard IS active but the rail doesn't have `Tab` focus, the rail renders live regardless — focus only gates keyboard input, never rendering.) Matches "dashboard behind stays live" acceptance criterion
```
**No unhandled edge case found** in this pass — every row the section's own template asks
about maps to an already-decided mechanism from Sections 1-2 above.

## Section 5: Code Quality Review

**Organization:** new code fits the existing per-view pattern exactly (state struct +
render fn + key-match arm) — zero deviation, zero new idiom to learn.

**DRY:** this increment is itself a DRY *fix* — `orchestrate.rs`'s spawn-or-resume logic
and event-kind knowledge currently exist in exactly one place (good) but were about to be
informally duplicated into the TUI before this review; the shared-helper extraction
(0B) prevents that duplication before it happens. No other DRY violation found.

**Naming:** `ConverseState`, `active_target`, `current_reply`, `awaiting_turn` — all name
what they hold, not how (matches existing `MemoryPaneState`, `SpawnViewState` naming
convention).

**Over-engineering check:** the per-target `HashMap` (Section 1) could be seen as
over-engineering vs. a single global state — but it's required by the retarget-mid-stream
acceptance criterion, not speculative. No over-engineering found.

**Under-engineering check:** none found — Section 1's finding (per-target keying) already
closes the one place this would otherwise have shipped fragile (single global state
racing on retarget).

**Cyclomatic complexity:** the extended `AppEvent::Flight` arm in `step()` gains one new
branch (dispatch to `ConverseState::on_flight_event` when `view == Converse` — actually,
better: unconditionally update `converse_view`'s per-target map regardless of active view,
per the "background stays live" edge case above, so branching stays flat — 1 new branch,
not >5). No refactor needed.

**No issues found.**

## Section 6: Test Review (CEO-level; full test diagram + artifact lands in Eng Phase 3)

```
  NEW UX FLOWS: send message (inject) · send message (spawn-new) · retarget ·
                scroll-vs-follow · cancel in-flight · resume after exit/timeout

  NEW DATA FLOWS: text_delta accumulation into current_reply · terminal-event flush ·
                  per-target state transitions

  NEW CODEPATHS: ConverseState state machine (5 states) · spawn-or-resume branch (ported) ·
                 buffer-cap truncation

  NEW BACKGROUND JOBS: none (reuses existing SSE producer thread unchanged)

  NEW INTEGRATIONS: none (reuses DataSource::inject/spawn, existing AppEvent::Flight pipe)

  NEW ERROR/RESCUE PATHS: InjectRejected · SpawnRejected · TransportError · BufferOverflow ·
                          OrchestratorExited · AgentFailed (all Section 2)
```
2am-Friday test: inject a message, kill the mock SSE mid-stream, confirm the TUI shows the
inline system line and never blocks the render loop (the exact failure ux.0 was built to
prevent). Hostile-QA test: send two messages back-to-back with no delay (double-submit
guard), retarget mid-stream three times rapidly (per-target keying holds). Chaos test:
fuzz the SSE feed with out-of-order/duplicate `text_delta` events for the same target.
Full test-type-by-item breakdown + concrete test file assignments is Eng Phase 3's job
(Section 3 there) — flagged here as satisfied by design, not yet written as code.

## Section 7: Performance Review

No DB, no N+1, no new connection pool pressure (reuses the single shared SSE connection).
**Memory:** `current_reply` capped at 64KB per target (Section 1D-bis, mirrors `agentd`'s
own `parse_sse_stream` tool-input cap) — bounded. **Slow paths:** none — this is local
process rendering, not a network-bound computation. **Caching:** not applicable (no
expensive recomputation). **No issues found.**

## Section 8: Observability & Debuggability Review

Every inject/spawn call already emits server-side flight events (`ControlInjected`,
`OrchestratorDispatched`/`OrchestratorInjected`, shipped p7.3/orch.1) — a bug reported
3 weeks post-ship is reconstructable from `flight.jsonl` alone, same as every other
cockpit action today. **Gap (auto-decided, Taste — surfaced at gate):** should the TUI
also locally log converse-specific client-side events (e.g. "buffer overflow truncated
N bytes for target X") to aid debugging a client-only rendering bug that wouldn't show
up in the server's flight log at all? **Decision:** yes, mirror the existing pattern —
`agentctl`'s other views don't maintain a separate client log today (they rely on the
Inspector view's flight-log tail), so introducing one just for Converse would be
inconsistent. Defer to a `TODO` only if a real client-side-only bug class emerges in QA.
Marked **TASTE DECISION** for the final gate given it's a judgment call, not a clear win either way.

## Section 9: Deployment & Rollout Review

No DB migration, no feature flag needed (a new keybinding on an already-installed binary —
users get it on next `cargo install`/binary update, same as every other `agentctl watch`
view added in Phase 6). No deploy-time risk window (client-only, no server-side component
changes). Rollback: `git revert`, rebuild, done — 5/5 reversible (matches Section 1).
**No issues found.**

## Section 10: Long-Term Trajectory Review

**Tech debt:** none beyond the flagged buffer-cap tradeoff (Section 2) and the observability
taste-call (Section 8) — both explicitly tracked, not silently accepted. **Reversibility:**
5/5. **Path dependency:** the per-target `HashMap<AgentId, ConverseState>` pattern (Section 1)
is explicitly designed to generalize — this makes FUTURE per-agent live-UI features easier,
not harder; net-positive path dependency. **1-year question:** a new engineer reading this
plan in 12 months would find "one more `View` variant, following the exact pattern of the
9 that came before it" self-evidently obvious — no knowledge concentration risk.
**Platform potential:** confirmed in Section 1 — the state-machine pattern is reusable.
**No issues found** beyond what's already tracked above.

## Section 11: Design & UX Review (UI scope confirmed) — REVISED for rail-on-Dashboard

**Information architecture:** with the rail resolution, the operator now sees, in one
frame: (1) the agent table (left, ~65%, unchanged — "what's happening across the fleet"),
(2) the chat rail (right, ~35% — "who I'm talking to and what was said"), (3) the input box
(bottom of the rail — "what I can do next"). This is a materially different information
hierarchy than the superseded full-screen-tab design: the operator never loses fleet
visibility while conversing (the literal point of D1's "one screen" decision), at the cost
of less horizontal space for both the table and the transcript than either had full-screen.
Left-to-right priority (table before rail) matches every other row-oriented Dashboard
convention already in this codebase — the fleet stays primary, chat is secondary/contextual.

**Interaction state coverage:**
```
  FEATURE           | LOADING              | EMPTY                    | ERROR                        | SUCCESS          | PARTIAL
  -------------------|----------------------|---------------------------|-------------------------------|------------------|------------------
  Transcript          | "..." spinner while  | Empty transcript, input   | Inline yellow system line    | Full reply       | Streaming green
                       | DISPATCHING          | box shown, no prior copy  | + resume hint (Section 2)    | flushed to       | text (Section 1
                       |                      | (first message to this   |                               | history          | state machine)
                       |                      | target)                   |                               |                  |
  Target selector      | n/a                  | Default = orchestrator    | Retarget to Done/Failed agent | Border title     | n/a
                       |                      |                           | → SpawnRejected inline        | updates          |
```
**User journey:** HOUR 1 / HOUR 6+ storyboard already produced in Step 0E above — reused
here rather than duplicated. Emotional arc: confidence (message sent, visibly echoed) →
anticipation (streaming appears live, not a dead wait) → trust (errors never hang, always
explain + offer a next action).

**AI slop risk:** n/a — this is a terminal UI (ratatui), not a web page; the "AI slop
blacklist" (purple gradients, 3-column feature grids, centered hero copy) doesn't apply to
TUI rendering. The one universal rule that DOES apply — "cards earn their existence" — is
satisfied: there's exactly one new visual unit (the Converse pane), not a decorative grid.

**Design system alignment:** no `DESIGN.md` exists for the TUI, but a consistent color
vocabulary already exists in `views.rs` (Green=running, Cyan=waiting, Yellow=deferred/system,
Red=failed, Magenta=awaiting_approval). Converse's proposed palette (operator=default,
assistant=default, **streaming=green**, system=yellow) reuses Green and Yellow with the
SAME semantic meaning they already carry elsewhere (Green="active/healthy", Yellow="needs
attention") — this is exactly right, not coincidental; flagged as a positive finding, no
new token needed. **Recommend `/plan-design-review`** for the full 7-pass audit (per this
section's own instruction) — that's Phase 2, next.

**Responsive:** terminal resize is already handled generically by ratatui's constraint
layout (proven across every existing view); the one increment-specific responsive question
— minimum terminal width for the 3-part layout (border title + transcript + input box) —
is flagged for Phase 2 to size concretely (Topology already established a "min 60 cols"
guard pattern to borrow from).

**Accessibility:** keyboard-only interaction already matches every other view (no mouse
dependency anywhere in `agentctl watch`). Screen-reader/terminal accessibility (e.g.
respecting `NO_COLOR`) is an existing gap across the whole TUI, not new to this increment —
noted as a pre-existing condition, not a new finding to block this plan on.

**No new UI issues found** beyond the responsive min-width question, explicitly deferred
to Phase 2 (Design Review) where it belongs (pixel/column-level sizing, not strategy).

---

## CEO Dual Voices — Consensus Table

Both voices ran independently (Claude subagent via Agent tool, foreground; Codex via
`codex exec -s read-only`, foreground) against the plan as it stood mid-Phase-1, before
the sequencing pause. Both converged, unprompted, on the same two findings — the strongest
possible signal short of a security-severity flag.

```
CEO DUAL VOICES — CONSENSUS TABLE:
═══════════════════════════════════════════════════════════════
  Dimension                           Claude   Codex   Consensus
  ──────────────────────────────────── ─────── ─────── ─────────
  1. Premises valid?                   FLAG     FLAG    DISAGREE→RESOLVED (sequencing premise missing; layout premise contested)
  2. Right problem to solve?           FLAG     FLAG    CONFIRMED (both: goal overstated as "cannot," actually "inconvenient" — accepted as framing note, not a scope change)
  3. Scope calibration correct?        FLAG     FLAG    CONFIRMED (both: building ahead of ux.2a risks rebase churn on shared files)
  4. Alternatives sufficiently explored?FLAG    N/A     CONFIRMED (Claude: zero-code "two terminals today" alternative never weighed in 0C-bis; noted, not actioned — see Required Outputs)
  5. Competitive/market risks covered? FLAG     N/A     CONFIRMED (Claude: "Hermes Agent" comparison cited but never substantiated in any doc; noted as a documentation gap, not this plan's to fix)
  6. 6-month trajectory sound?         FLAG     FLAG    CONFIRMED (both: "no 6-month regret" claim in 0E was asserted, not earned — ux.5's future web-cockpit rewrite risk and the D1 layout tension are real regret vectors)
═══════════════════════════════════════════════════════════════
CONFIRMED = both agree there was a real gap. DISAGREE = models differed in framing but
converged on the same underlying issue once cross-checked against files on disk.
Dimensions 1, 3, and 6 triggered the mid-review pause (sequencing + layout) — both now
RESOLVED per the Premise Gate revision above. Dimensions 2, 4, 5 are process/documentation
observations, not blocking findings — carried into TODOS.md below rather than re-opening
the plan.
```

**Cross-model tension:** none — this is the rare case where both voices converged on the
identical substantive finding without prompting from each other (Claude's subagent ran
first and had zero visibility into the Codex prompt or output). Presented per the skill's
own guidance: "cross-model agreement is a strong signal" — treated as such, not as
automatic permission to act (the pause was still routed through explicit AskUserQuestion
gates, twice, per User Sovereignty).

## Required Outputs

### "NOT in scope"
- Permanent split-pane layout for the OTHER 8 views (Topology/Memory/Spawn/Inspector/etc.) —
  D1's "one screen" decision, taken literally, would eventually apply everywhere; this plan
  scopes the rail to `Dashboard` only. Rationale: bounding blast radius to what ux.1
  actually needs; a TODO tracks the broader D1 completion question.
- The "two terminals today" zero-code alternative (run `orchestrate.rs` + `watch` side by
  side) — surfaced by the Claude subagent, correctly notes this already works today. Not
  pursued because the user's Premise 1 (chat belongs inside `watch`) was reconfirmed even
  after the pause — the convenience gap is real even if not literally impossible.
- Substantiating the "Hermes Agent" competitive comparison cited in `ux-cockpit.md` —
  a pre-existing documentation gap, not introduced by or fixable within this plan.
- ux.3 (spawn-on-the-fly fix), ux.8 (budget control), ux.5 (web cockpit) — separate
  roadmap increments, unaffected by this one except where noted (ux.5 sunk-cost risk below).

### "What already exists"
See the "What already exists" section near the top of this plan (verified against
post-ux.2a `main` @ `016cab4a`) plus Step 0B's leverage map above — both already capture
the full reuse surface (`DataSource::inject/spawn`, event-kind knowledge, the generic
`AppEvent::Flight` pipe, and now the `render_dashboard` extension point instead of a new
`View` variant).

### "Dream state delta"
Revised from Step 0C given the layout resolution: this plan now delivers MORE of the
12-month ideal than the superseded full-screen-tab design would have — the permanent
split-pane IS the D1 vision (scoped to Dashboard only, not all 9 views). The remaining
gap to the full 12-month picture is: (a) extending the rail pattern to other views if D1
is pursued further (tracked as a TODO), and (b) `ux.5`'s eventual browser-based cockpit,
which will need its own rendering layer regardless of what ratatui-specific work ships
here (the Claude subagent's Finding 4b — TUI-specific rendering code is not reusable by a
future web SPA; only the `source`/`converse.rs` helper layer carries forward). This is
accepted consciously, not newly discovered — SPA rewrites always discard presentation code.

### Error & Rescue Registry
Complete — see Section 2 above. Zero unrescued (N) rows.

### Failure Modes Registry
```
  CODEPATH                  | FAILURE MODE           | RESCUED? | TEST? | USER SEES?      | LOGGED?
  ---------------------------|------------------------|----------|-------|-----------------|--------
  converse::dispatch (inject)| target raced to !waiting| Y        | TODO  | inline yellow   | Y (server-side flight event)
  converse::dispatch (spawn) | capability/quota denied | Y        | TODO  | inline yellow   | Y (server-side flight event)
  ConverseState event fold   | buffer overflow (64KB)  | Y        | TODO  | truncation marker| N (client-only; Section 8 taste call)
  SSE (shared, existing)     | drop mid-stream         | Y (existing ProducerDied) | Y (existing) | existing degraded banner | Y (existing)
  render_dashboard split     | terminal < MIN_RAIL_WIDTH| Y (deferred to Phase 2) | TODO | rail hidden + hint | N/A
  ---------------------------|------------------------|----------|-------|-----------------|--------
```
No row has RESCUED=N + TEST=N + USER SEES=Silent simultaneously → **no CRITICAL GAP**. "TODO"
in the TEST column means "test spec not yet written" (Eng Phase 3's job, not a rescue gap) —
distinguished explicitly from an unrescued failure mode.

### TODOS.md updates (presented individually, not batched)

Two candidates surfaced, presented individually per the skill's rule:
1. **D1 scope decision** — user chose **Add to TODOS.md**. Written to `TODOS.md` under a
   new `## ux.1 — Open (deferred from CEO review, 2026-07-13)` section.
2. **Hermes Agent citation** — user chose **Skip**, not valuable enough for this plan to carry.

### Completion Summary

```
  +====================================================================+
  |            CEO PLAN REVIEW — COMPLETION SUMMARY                   |
  +====================================================================+
  | Mode selected        | SELECTIVE EXPANSION                          |
  | System Audit         | git log/diff/stash/TODO-grep clean; no design
  |                       | doc for this branch (user declined /office-hours,
  |                       | rough scope from ux-cockpit.md sufficed)     |
  | Step 0               | Premise Gate x2 (initial accept + mid-review
  |                       | pause/re-litigation); Mode=SELECTIVE EXPANSION|
  | Section 1  (Arch)     | 2 findings (per-target HashMap keying — Mechanical;
  |                       | rail-vs-tab coupling cost — accepted consciously) |
  | Section 2  (Errors)   | 8 error paths mapped, 0 GAPS                 |
  | Section 3  (Security) | 0 issues found                               |
  | Section 4  (Data/UX)  | 8 edge cases mapped, 0 unhandled              |
  | Section 5  (Quality)  | 0 issues found                                |
  | Section 6  (Tests)    | Diagram produced, 0 gaps (detail → Eng Phase 3)|
  | Section 7  (Perf)     | 0 issues found                                |
  | Section 8  (Observ)   | 1 taste call (client-side logging — deferred) |
  | Section 9  (Deploy)   | 0 risks flagged                               |
  | Section 10 (Future)   | Reversibility: 5/5, debt items: 2 (tracked)   |
  | Section 11 (Design)   | 1 new finding (min rail width — → Phase 2)    |
  +--------------------------------------------------------------------+
  | NOT in scope          | written (4 items)                            |
  | What already exists   | written (pre- and post-ux.2a)                 |
  | Dream state delta     | written (revised for rail resolution)         |
  | Error/rescue registry | 8 methods, 0 CRITICAL GAPS                   |
  | Failure modes         | 5 total, 0 CRITICAL GAPS                     |
  | TODOS.md updates      | 1 added, 1 skipped                            |
  | Scope proposals       | 0 proposed (SELECTIVE EXPANSION — no cherry-pick ceremony triggered) |
  | Outside voice         | ran (Claude subagent + Codex, both foreground) |
  | Lake Score            | 6/6 — every taste-adjacent call chose the more complete option |
  | Diagrams produced     | 4 (system architecture, state machine, data flow, interaction edge cases) |
  | Stale diagrams found  | 0 (no pre-existing diagrams in files this plan touches) |
  | Unresolved decisions  | 0 — both AskUserQuestion gates (sequencing, layout) resolved |
  +====================================================================+
```

**PHASE 1 COMPLETE.** Codex: 1 critical finding (sequencing + layout, resolved via pause).
Claude subagent: 6 findings (1 critical matching Codex, 5 process/documentation notes).
Consensus: 3/6 dimensions CONFIRMED as real gaps (all resolved), 3/6 CONFIRMED as
non-blocking documentation notes (carried to TODOS.md or explicitly dropped). Passing to
Phase 2 (Design Review) — UI scope confirmed, and Section 11 already flagged one concrete
question (minimum rail width) for Phase 2 to resolve at pixel/column level.

---

# Design Review — 7 Passes (Phase 2)

Grounded against the actual current `render_dashboard()` (post-ux.2a, `views.rs:166-280+`):
a 6-column agent table (Agent ID, Status, ATTN, Context, Budget, Tools) with `Constraint::
Min(20)` on the Agent ID column, 2-line-tall rows when an attention signal is stacked
under the ID, DarkGray header row, Blue selection highlight, plus a header bar + attention
summary line + optional spawn banner + footer already consuming 4-6 fixed rows before any
table content renders. This is a genuinely dense existing view — the rail lands on top of that.

## Pass 1: Information Architecture

Rate: **5/10 initial.** The plan's Section 11 states the priority order (table → rail →
input) but doesn't yet specify the concrete `Layout` constraints. A flat 65/35 split ignores
that the Agent ID column alone needs `Min(20)` plus a 2-line attention-reason row that's
already tight at full width — squeezing the table to 65% risks truncating agent IDs or
attention reason text, the exact thing ux.2a was just built to make visible.

**FIX TO 10:** concrete layout, not a percentage guess:
```
Horizontal split of `content_area` (post-header/summary/banner, pre-footer):
  ┌───────────────────────────────────────┬───────────────────────┐
  │ Agent table (Constraint::Min(48))      │ Chat rail             │
  │  — same 6 columns, same 2-line         │ (Constraint::Length(N)│
  │  attention-reason rows, UNCHANGED       │  where N = 28..40,    │
  │  widget code                            │  see Pass 6 for the   │
  │                                          │  exact floor)         │
  └───────────────────────────────────────┴───────────────────────┘
```
Give the **table** the `Min` constraint (protect its readability, matches ux.2a's own
investment) and the **rail** a `Length` constraint with a fixed target width (a chat
transcript reads fine at 30-40 cols — it's prose, not tabular data — while the table
does NOT read fine below ~48 cols). This is the opposite of a naive 65/35 percentage split
and is a genuine finding: **the rail should be the fixed-width pane, not the table.**
**STOP — this is a structural finding, not a taste call. Auto-decided (P5 explicit over
clever + P1 completeness — protects the just-shipped ux.2a investment): rail gets
`Constraint::Length`, table gets `Constraint::Min`.** Logged as a Mechanical decision.

## Pass 2: Interaction State Coverage

Rate: **6/10 initial** (Section 4/11 of the CEO review already specified most of this at
the strategy level; Design pass needs the concrete visual spec).
```
  FEATURE            | LOADING           | EMPTY                  | ERROR                  | SUCCESS         | PARTIAL
  --------------------|-------------------|-------------------------|-------------------------|-----------------|------------------
  Chat rail (overall) | n/a (always       | "No conversation yet —  | n/a (per-message,       | n/a             | n/a
                       | rendered)         | press Enter to start"   | see below)              |                 |
                       |                   | + border title shows    |                         |                 |
                       |                   | default target          |                         |                 |
  Message send        | Border title      | n/a                     | Inline yellow line,     | Flushed to      | Green streaming
                       | subtitle: "…"      |                         | prefixed `!`, + resume  | transcript,     | text (Section 1
                       | while DISPATCHING |                         | hint text (Section 2)   | default color   | state machine)
  Rail (narrow term)  | n/a               | n/a                     | Rail hidden entirely,   | n/a             | n/a
                       |                   |                         | `Tab` shows one-line    |                 |
                       |                   |                         | footer hint (Pass 6)    |                 |
```
**FIX TO 10:** the empty-state copy above ("No conversation yet — press Enter to start")
is a concrete first-run moment — this is the FIRST thing every operator sees the first time
they press `Tab` to focus the rail, and it should say what target they're talking to by
default (the orchestrator) so it's not a mystery. **Auto-decided (P1 completeness):**
empty state includes the default-target framing inline, not just a generic placeholder.

## Pass 3: User Journey & Emotional Arc

Rate: **7/10 initial.** CEO Step 0E already storyboarded HOUR 1 / HOUR 6+; Design pass adds
the retarget-mid-stream moment specifically, since that's the riskiest emotional beat:
```
  STEP                          | OPERATOR DOES              | OPERATOR FEELS         | PLAN SPECIFIES?
  -------------------------------|------------------------------|-------------------------|------------------
  1. Orchestrator streaming a    | Watching green text grow      | Engaged, in the loop    | Yes (Section 1)
     long reply                  |                                |                         |
  2. Notices a worker agent      | Presses row-select + retarget | Curious, slightly       | Partially — the
     row go Red (failed) in the  | action while orchestrator      | divided attention        | border-title swap
     table (still visible!)      | reply is still streaming       |                         | is spec'd, but NOT
                                  |                                |                         | how the table stays
                                  |                                |                         | selectable while rail
                                  |                                |                         | has input focus
  3. Rail now shows the failed   | Reads the failure, types a     | Confident — nothing     | Yes (per-target
     agent's (empty) transcript  | question to it                 | was lost by switching   | HashMap, Section 1)
  4. Retargets BACK to           | Presses retarget again         | Relief — the reply kept | Yes (per-target
     orchestrator                |                                | streaming in the        | keying is exactly
                                  |                                | background, not lost    | for this)
```
**FIX TO 10 — genuine finding:** step 2 exposes an unspecified interaction: when the rail
has keyboard focus (per `Tab`, Pass "Design system alignment" below), can the operator
still move the table's row selection to retarget, or must they un-focus the rail first?
**This wasn't resolved in Section 1's `rail_focused: bool` field description.**
**Auto-decided (P5 explicit over clever):** row selection (`↑/↓` on the table) only moves
when the rail does NOT have focus; retargeting is a deliberate action (`Tab` un-focuses
the rail → operator selects a row → **`r`** binds the rail to that row's agent → `Tab`
refocuses the rail). This is simpler than trying to make both panes independently
interactive at once, and matches the existing single-focus keyboard model every other view
already uses. **Logged as Mechanical** — the alternative (both panes simultaneously
interactive) has no existing precedent to reuse and adds a second focus-management system
this codebase has never needed before. (The `r` binding itself is finalized in the
post-Design dual-voice pass below, once the `[c]`/Credentials collision was found —
referenced here for narrative consistency.)

## Pass 4: AI Slop Risk

Rate: **N/A — not applicable.** Confirmed from CEO Section 11: this is a ratatui TUI, not
a web page. The blacklist (purple gradients, 3-column feature grids, centered hero copy,
emoji-as-bullets) has no surface here. The one universal rule that DOES apply — "cards
earn their existence" — is satisfied: exactly one new visual unit (the rail), not a
decorative grid. **No issues found**, examined and confirmed inapplicable, not skipped.

## Pass 5: Design System Alignment

Rate: **8/10 initial.** No `DESIGN.md` exists for this TUI (confirmed absent, both in CEO
Section 11 and here). The existing color vocabulary in `views.rs` (Green=running/healthy,
Cyan=waiting, Yellow=deferred/system/needs-attention, Red=failed, Magenta=awaiting_approval,
DarkGray=chrome/header, Blue=selection) is the de facto design system. The plan's proposed
rail palette — operator=default terminal fg, assistant=default terminal fg, **streaming=
green**, system=yellow — reuses Green and Yellow with the EXACT semantic meaning they
already carry (Green="healthy/active", Yellow="needs attention"). **No new token needed,
confirmed correct reuse, not coincidence.** **Recommend, per this pass's own instruction:**
since no `DESIGN.md` exists at all for this TUI, flag the gap once (not blocking this plan)
— `/design-consultation` could formalize the color vocabulary that's existed informally
since Phase 6. Not filed as a TODO here (already covered by TODO 1's broader D1 scope
question, which subsumes it) — noted as context only.

## Pass 6: Responsive & Accessibility

Rate: **4/10 initial → 9/10 after fix** (the one section with real, blocking-for-Phase-3
gaps — exactly what CEO Section 1/11 deferred here).

**FIX TO 10 — the concrete numbers, CORRECTED after dual-voice review found my first pass
too optimistic:** the agent table's actual column constraints, read directly from
`views.rs:286-291`, are `Min(20)` (Agent ID) + `Length(20)` (Status) + `Length(4)` (ATTN) +
`Length(10)` (Context) + `Length(12)` (Budget) + `Length(6)` (Tools) = **72 raw content
columns** at the Agent ID column's floor — my first-pass estimate of "~62" undercounted
Status alone (20, not "8"). Add ratatui `Table` cell spacing/borders (~6-8) →
**table needs ~78-80 cols minimum.** Rail needs ≥30 cols to render prose usefully (per
both dual-voice findings) + its own border (~4) → **~34 cols.** Plus a 1-col divider →
**`MIN_TOTAL_WIDTH_FOR_RAIL = 115`** (not the 95 I originally wrote — both the Claude
subagent and Codex independently caught this same undercount, converging on "low 100s"/
"114+"; 115 is the corrected, arithmetic-grounded number, not a re-guess).
Below 115 total terminal columns: hide the rail entirely, `render_dashboard` falls back
to today's table-only layout, and `Tab` shows a one-line footer hint ("terminal too
narrow for chat rail — resize to 115+ cols") instead of silently rendering broken columns.
**Considered and rejected:** the Claude subagent proposed falling back to a full-screen
chat view below the floor instead of hiding the rail. Rejected — that would require
building AND maintaining two separate rendering paths (rail + full-screen) for one
feature, which is the exact tab-based complexity D1 was chosen to avoid; "hidden + resize
hint" is simpler and 115 cols is a normal modern terminal width, not an exotic constraint.
This numeric floor is now **auto-decided (P1 completeness), logged as Mechanical**
(arithmetic from existing column constraints, not a judgment call) — corrected once, not
re-opened as ongoing taste.
- **Vertical space:** rail needs its own internal 2-way split (transcript + input box) —
  reuse `header_footer_layout`'s `Constraint::Length(N)` idiom for a fixed-height input box
  (3 rows: border top, text line, border bottom) with `Constraint::Min(1)` for the
  transcript above it — proven pattern, not new.
- **Keyboard nav:** already fully keyboard-only (matches every existing view) — no new
  a11y gap. Touch targets/mouse: not applicable (no mouse anywhere in this TUI).
- **Color contrast / NO_COLOR:** pre-existing gap across the whole TUI (not introduced by
  this plan) — noted, not blocking, consistent with CEO Section 11's finding.

## Pass 7: Unresolved Design Decisions

```
  DECISION NEEDED                              | IF DEFERRED, WHAT HAPPENS
  -----------------------------------------------|---------------------------------------------
  Exact `MIN_TOTAL_WIDTH_FOR_RAIL` numeric floor   | RESOLVED above (115 cols, corrected) — was the one real gap in my own first pass
  Rail vs table constraint type (Length vs Min)   | RESOLVED above (Pass 1) — was the one real gap
  Focus routing between table and rail            | RESOLVED above (Pass 3, then corrected below) — was unspecified
  Empty-state copy for first-ever conversation    | RESOLVED above (Pass 2)
```

## Design Dual Voices — Consensus Table + Resolutions

Both voices ran independently against the Passes-1-7 draft above (Claude subagent via
Agent tool foreground; Codex via `codex exec` foreground, given the CEO-phase findings as
context). Both found MORE than my own self-authored passes caught — this is the value of
dual-voice review, not a rubber stamp.

```
DESIGN DUAL VOICES — CONSENSUS TABLE:
═══════════════════════════════════════════════════════════════
  Dimension                              Claude   Codex   Consensus
  ──────────────────────────────────────── ─────── ─────── ─────────
  1. Info hierarchy (table left/rail right)CONFIRM  CONFIRM CONFIRMED — right hierarchy, but both flagged the width math as wrong (Pass 1/6, now corrected: 115 not 95)
  2. Interaction states specified?         FLAG     FLAG    CONFIRMED gap — unread/background indicator missing (both), input-disabled/cancel semantics vague (Codex)
  3. Journey coherent?                     FLAG     N/A     CONFIRMED gap — retarget-mid-stream breaks "confidence" arc without an unread indicator
  4. Specific vs generic UI?               MIXED    MIXED   CONFIRMED — strong on state machine/colors, weak on retarget keystroke + focus semantics (both, independently)
  5. Design system alignment?              N/A      CONFIRM Codex: color-only signaling (streaming=green) insufficient without textual glyphs (NO_COLOR/accessibility)
  6. Responsive intention?                 CONFIRM  CONFIRM CONFIRMED gap — width floor undercounted (both), vertical-height floor unspecified (Codex)
═══════════════════════════════════════════════════════════════
CONFIRMED = both agree there was a real, actionable gap in my Pass 1-7 draft.
```

**Critical finding, both models independently, verified against shipped code — `[c]` is
already the Credentials hotkey** (`mod.rs:507`, footer `[c]reds`, cred.5 v0.68.0 — shipped
after the rough-scope doc was written). Every prior reference to `[c]` for the chat rail in
this plan was wrong. **RESOLVED**: `Tab` toggles rail focus (reuses the exact idiom
`Memory`/`Spawn` already use for sub-pane cycling); `r` (free on Dashboard) retargets to
the selected row. Fixed throughout 0B, Step 0C, and Section 1 above (not left stale).

**Remaining findings and resolutions (all auto-decided, Mechanical or Taste as marked):**

1. **Retarget keystroke was never actually specified** (both models, critical) — the rough
   scope's "a retarget action" was used 6+ times without ever naming a key. **RESOLVED**:
   `r` retargets the rail to the table's currently-selected row, only when the rail does
   NOT have focus (Tab must be pressed first to leave the rail, or the operator is already
   on the table). Mechanical — there's now exactly one way to do this.
2. **Modal input-capture semantics for the rail's text box** (both, critical) — when `Tab`
   gives the rail focus, does the input box capture every keystroke (so typing "s" doesn't
   accidentally jump to `View::System`)? **RESOLVED**: reuse the EXACT proven idiom already
   in this codebase — `Spawn`'s `TaskField` focus state (`mod.rs:518-526`) already captures
   all `Char` input and only `Esc` defocuses. The rail's input box does the same: `Esc`
   returns focus to the table (equivalent to `Tab` toggling back), everything else while
   focused is literal text. Zero new pattern — DRY reuse of `Spawn`'s existing mechanism.
3. **No visual focus-indication state** (Claude subagent, high) — **RESOLVED**: focused
   pane's `Block` border uses `Style::default().fg(Color::White)`; unfocused uses the
   existing `Color::DarkGray` chrome color (matches the header bar's existing DarkGray
   convention) — reuses colors already in the palette, no new token.
4. **No background/unread indicator when a non-active target updates** (both, high —
   Claude subagent specifically ties this to the broken "confidence" emotional arc from
   Pass 3) — **RESOLVED**: the border-title target selector gains a badge when a
   backgrounded target's `ConverseState` transitions to STREAMING or a terminal event
   while not active — e.g. `┤ → orchestrator ├ [scout-3: ●2]` (dot + count of targets with
   unseen activity). Reuses the existing glyph-plus-count idiom `views.rs`'s attention
   summary line already established (`"{n} need attention"`, line 209-220) — same pattern,
   new application.
5. **Unbounded transcript history** (Claude subagent, medium) — flushed turn history (not
   `current_reply`, which is already capped at 64KB) has no cap. **RESOLVED**: cap at 200
   turns per target (ring buffer, oldest dropped first) — mirrors `EVENT_RING_CAP=2000`'s
   existing bounded-ring idiom in `app.rs`'s `events` field, scaled down since turns are
   much larger than single flight events.
6. **Optimistic-echo reconciliation on dispatch failure** (Claude subagent, medium) —
   **RESOLVED**: the optimistically-echoed operator message dims to `Color::DarkGray` and
   an error line (Section 2's rescue table) appends directly beneath it, not elsewhere in
   the transcript — visually ties the failure to the specific message that caused it.
7. **Border-title truncation for long agent IDs** (Claude subagent, medium) — **RESOLVED**:
   middle-ellipsis truncation (`scout-a1b2…-9f3d` style) at a fixed budget once the rail's
   `Length(32)` width is fixed (Pass 1) — mirrors truncation already needed nowhere else in
   this codebase (first occurrence), so this is genuinely new but trivially scoped: one
   string-formatting helper, no new state.
8. **Duplicate/out-of-order delta handling isn't really "last-write-wins" as stated**
   (Codex, medium — sharper than the CEO phase's original framing) — Codex is right that
   without a sequence number, "last write" isn't a coherent append semantic for streaming
   text (you can't "overwrite" a growing string coherently). **RESOLVED, correcting Section
   2's earlier framing**: `current_reply` is **append-only** for the duration of one
   `DISPATCHING`→`STREAMING`→flush cycle; a duplicate delta (same byte range replayed) is
   only preventable with a sequence number `agentd` doesn't currently emit. Accepted
   fallback: append blindly (matches what `agentd`'s own `parse_sse_stream` already does
   server-side — no ordering guarantee assumed there either, confirmed in CEO Section 2)
   — a rare reconnect-mid-stream race could show a visibly duplicated phrase, which is a
   cosmetic annoyance, not data corruption. TODO filed only if this proves to actually
   happen in dogfooding (no TODO filed speculatively).
9. **Color-only signaling insufficient (streaming=green, system=yellow)** (Codex, medium)
   — **RESOLVED**: every rail message line gets a textual prefix in addition to color —
   `you:`, `agent:`, `!` (system/error), `...` (streaming-in-progress marker before the
   text starts appending) — satisfies `NO_COLOR`/non-color-perceiving operators without
   inventing new UI, just consistent text prefixes already implied by the rough scope's
   own "role-coded" language.
10. **Vertical height floor unspecified** (Codex, medium) — **RESOLVED**: rail needs
    minimum 8 rows (3 for the input box per the existing `header_footer_layout`-style
    fixed-height idiom, 5 for a minimally useful transcript) — below it, same "hidden +
    resize hint" fallback as the width floor (Pass 6), not a second bespoke behavior.

All ten findings resolved via auto-decision — nine Mechanical (one arithmetically or
structurally correct answer given existing code/idioms), one Taste (item 8's "accept the
rare cosmetic duplicate vs. block on a server-side sequence-number feature request" —
flagged at the Phase 4 gate). **Zero decisions remain unresolved** entering Phase 3 (Eng).

**Cross-model tension:** none on substance — both voices converged independently on
retarget-key-undefined, focus-semantics-undefined, and width-math-wrong. The only
difference was specificity: Codex additionally caught the shipped `[c]`/Credentials
collision by grepping the actual keymap (a sharper, more concrete catch than the Claude
subagent's more conceptual framing of the same underlying "key model is unresolved" issue).

### "NOT in scope" (Design phase)
- Full-screen chat fallback below the width floor (considered, rejected — item in Pass 6).
- Screen-reader/`NO_COLOR` support beyond the textual-prefix fix (item 9) — pre-existing
  gap across the whole TUI, not this plan's to fully close.
- Extending the rail pattern to other views — tracked in TODOS.md (TODO 1), not this plan.

### "What already exists" (Design phase)
`Spawn`'s `TaskField` focus-capture idiom (item 2), `Memory`/`Spawn`'s `Tab` sub-pane
cycling (keybinding fix), the attention-summary-line glyph+count idiom (item 4),
`app.rs`'s bounded-ring pattern (`EVENT_RING_CAP`, item 5), and the existing Green/Yellow
color semantics (Pass 5) — all reused directly, zero new UI idioms invented beyond the
first split-pane layout itself (which is the plan's actual, accepted new surface area).

### Design Litmus Scorecard
```
+====================================================================+
|              DESIGN PLAN REVIEW — LITMUS SCORECARD                 |
+====================================================================+
| Pass 1  (Info Arch)   | 5/10 → 10/10 (rail=Length not Percentage, table protected) |
| Pass 2  (States)      | 6/10 → 10/10 (unread indicator + empty-state copy added)   |
| Pass 3  (Journey)     | 7/10 → 10/10 (unread indicator closes the "abandoned" gap) |
| Pass 4  (AI Slop)     | N/A — confirmed inapplicable (TUI, not web)                |
| Pass 5  (Design Sys)  | 8/10 → 9/10 (color reuse confirmed correct; DESIGN.md gap noted, not blocking) |
| Pass 6  (Responsive)  | 4/10 → 10/10 (width floor corrected 95→115; height floor added) |
| Pass 7  (Decisions)   | 4 resolved (self-authored) + 10 resolved (dual-voice) = 14/14, 0 deferred |
+--------------------------------------------------------------------+
| Overall design score  | 5.7/10 → 9.7/10                                            |
+====================================================================+
```
Design is complete for this plan; the implementer has zero UI decisions left to invent.
**Recommend `/design-review` after implementation** for visual QA against a live terminal
render — no mockup was generated (this is a ratatui TUI; ASCII diagrams in this plan serve
the mockup role, ratified above rather than a separate image-based mockup step).

**PHASE 2 COMPLETE.** Codex: 6 concerns (1 critical — keybinding collision — verified and
fixed). Claude subagent: 10 findings (3 critical, both models independently found retarget-
key and focus-semantics gaps). Consensus: 6/6 dimensions CONFIRMED as real gaps in my own
first-pass draft — the strongest possible argument for why dual-voice review runs even
when the primary reviewer (me) already produced a seemingly-thorough pass. Passing to
Phase 3 (Eng Review).

---

# Eng Review — 4 Sections (Phase 3, required shipping gate)

## Step 0: Scope Challenge

**Complexity check:** this plan now touches — post the streaming-premise finding below —
`agentd/src/events.rs`, `scheduler.rs`, `flight_recorder.rs` (3 files, cross-crate) PLUS
`agentctl/src/watch/{mod.rs,app.rs,views.rs}`, `agentctl/src/orchestrate.rs`, and a new
`agentctl/src/watch/converse.rs` (5-6 files) — **8-9 files, crossing the Eng skill's own
"8+ files or 2+ new classes/services" MUST-STOP threshold** for a scope-reduction offer.
**This gate was already satisfied**: the file list and full risk tradeoff (bigger,
cross-crate, needs its own scope challenge) was presented to the user via AskUserQuestion
at the point the streaming-premise finding surfaced (see the "Streaming blocker"
resolution below) — the user chose to expand scope with that exact cost stated. Re-asking
the identical question here would be the redundant-prompt anti-pattern the skill warns
against; this Step 0 instead documents that the gate fired and was answered, with the
scope decision, not re-litigates it.

## The critical finding that reshaped this plan

Independently confirmed **three times** — by me (reading `agentd/src/scheduler.rs`,
`events.rs`, `flight_recorder.rs`, `management.rs` directly), by Codex (dispatched with my
claim as a hypothesis to verify, not to trust), and by the Claude Eng subagent (dispatched
blind, with zero knowledge of my or Codex's findings) — all three converged on the same
result without any cross-contamination:

**`text_delta` (individual streaming token chunks) never reaches `agentctl`.**
`agentd/src/scheduler.rs`'s `make_infer_future()` (lines 1024-1125), when streaming,
writes each chunk directly to `agentd`'s own process stdout via `tokio::io::stdout()`
(`print_fut`, line 1053-1077) — it never calls `FlightRecorder::record()` per chunk. Only
two bookend events exist: `InferenceStreamStarted` (start) and `InferenceStreamCompleted`
(end, carrying a `text_chunks_emitted` COUNT, not the text). `FlightRecorder::record()` is
the ONLY thing that feeds the `broadcast_tx` channel `management.rs`'s `/api/v1/events` SSE
endpoint serves (`flight_recorder.rs:87`) — so per-chunk text never reaches SSE, never
reaches `pump.rs`'s `sse_loop`, never reaches `AppEvent::Flight`. `agentd/src/events.rs`'s
`EventKind` enum has no delta/chunk variant at all — this isn't a wiring bug, the event
type simply doesn't exist yet.

**Compounding finding (Codex + Claude subagent both independently caught this too):**
`orchestrator_turn_complete`'s `answer` field is capped at 512 chars server-side
(`scheduler.rs:1383`, confirmed real, from orch.2's own shipped work) with a code comment
pointing to "the full text streamed above" — meaning `agentd`'s own local stdout, not
anything remote. **`agentctl orchestrate`'s CLI, shipped today (orch.1/orch.2), already
silently truncates long replies when running against a non-colocated `agentd`** — this is
a genuine, pre-existing production gap this review discovered as a side effect, not
something ux.1 introduces.

**User decision (AskUserQuestion, "Streaming blocker"): expand scope.** Add the missing
`agentd`-side event (`EventKind::InferenceStreamDelta` or similar), recorded per-chunk in
the same `print_fut` loop alongside the existing stdout write, wired into the broadcast
channel — delivering Premise 2 for real, and fixing the 512-char truncation bug as a
byproduct (same underlying fix covers both, since a correct per-chunk/full-text event
obsoletes the need for a lossy 512-char preview field).

## Section 1: Architecture Review (updated for the streaming-delta expansion)

**Revised system architecture** (extends the Section 1 diagram from Phase 1 — additive,
that diagram's `agentctl`-side content stands unchanged):
```
agentd/src/events.rs
  + EventKind::InferenceStreamDelta { agent_id, turn_seq, chunk_seq, text }
    (sequence numbers on BOTH turn and chunk — Section 2's code-quality finding on
    field-path inconsistency below is exactly why this must be explicit from day one,
    not inferred per-consumer)

agentd/src/scheduler.rs's make_infer_future() print_fut loop
  MODIFIED: alongside the existing tokio::io::stdout() write (kept, for the co-located
  CLI-demo case), ALSO calls recorder.record(&id, None, EventKind::InferenceStreamDelta,
  json!({"agent_id": &id, "turn_seq": ..., "chunk_seq": chunks_emitted, "text": &chunk}))
  — this is now on the hot path for every token of every streaming inference call, a
  materially different volume/cost profile than any existing event kind (see Performance,
  Section 4, below).

agentd/src/flight_recorder.rs
  POLICY DECISION (not just code): full model output now lands in flight.jsonl, which
  today is preview/audit metadata (bounded field sizes throughout the existing event
  taxonomy), not verbatim model output storage. This is a genuine data-classification
  and storage-growth question, not a mechanical addition — flagged for the Design/
  Required-Outputs discussion below, not silently decided.

agentd/src/management.rs
  No route change (SSE already forwards whatever record() sends) — but needs a
  backpressure/volume test given the new per-token event rate (Performance, Section 4).

── downstream, agentctl side (unchanged from Phase 1/2's revised Section 1) ──
pump.rs → AppEvent::Flight(value) → mod.rs step() → converse_view (HashMap<AgentId,
ConverseState>) → render_dashboard()'s rail.
```

**FINDING (Claude Eng subagent, HIGH, verified against code) — the four terminal events'
field paths are NOT uniform, contrary to Step 0B's framing.** Read directly from
`scheduler.rs`:
```
  EVENT                    | AGENT IDENTITY FIELD                              | GOTCHA
  --------------------------|-----------------------------------------------------|------------------
  OrchestratorTurnComplete  | top-level `agent` (real id) + redundant `data.agent_id` | consistent, safe
  OrchestratorInjected      | top-level `agent` (real id)                        | consistent, safe
  OrchestratorExited        | top-level `agent` is a HARDCODED LITERAL "agentd"  | ← LANDMINE — only `data.agent_id` is valid
  AgentFailed               | top-level `agent` ONLY — `data` has NO agent_id at all | ← LANDMINE — different shape entirely
```
Step 0B's original framing ("extract the event-kind knowledge... into shared match logic")
implied this could be re-derived as general knowledge. **It cannot — it must be a literal,
byte-for-byte port of `orchestrate.rs`'s four existing `if kind ==` blocks (lines 157-189),
not a re-abstraction.** Getting `OrchestratorExited`'s identity field wrong (trusting the
top-level `agent` literal instead of `data.agent_id`) would leave a target's `ConverseState`
stuck in STREAMING/DISPATCHING forever — a direct, silent violation of this plan's own
"never hang" invariant. **Auto-decided (P5 explicit over clever, Mechanical):** `watch/
converse.rs`'s shared helper copies `orchestrate.rs`'s four match arms verbatim (same
field-path lookups, same literal-vs-nested distinction per kind), with a contract test
per kind sourced from real `agentd`-emitted fixtures (Section 3), not hand-typed JSON.

**FINDING (Claude Eng subagent, HIGH, verified against code) — Dashboard's key routing is
a flat match, not a tuple-match; the "reuse the Spawn idiom" framing undersells the
retrofit.** `mod.rs:449-512`'s `handle_dashboard_key(code: KeyCode, app: &mut App)` matches
on `code` alone across 9 shortcuts + arrows + Enter — no focus dimension exists. `Spawn`'s
`handle_spawn_key` already IS a `(focus, code)` tuple match, which is why Pass "modal input
capture" in Design Phase 2 called it a clean reuse for the RAIL's own internal Esc-capture
behavior. But making DASHBOARD ITSELF focus-aware (so `s`/`t`/`m`/etc. don't fire while the
rail has focus, and Enter sends a chat message instead of its current view-routing
behavior) requires restructuring `handle_dashboard_key` into that same tuple-match shape —
a materially bigger, riskier diff than "add one new match arm," and one that must not
regress the ~15 existing `handle_dashboard_key` unit tests (`mod.rs:927-1051`), none of
which exercise any focus/converse state today. **Auto-decided (P5 + P3 pragmatic,
Mechanical):** restructure to `handle_dashboard_key(focus: DashboardFocus, code: KeyCode,
app: &mut App)` mirroring Spawn's exact shape; existing 15 tests updated to pass
`DashboardFocus::Table` (the default) so their asserted behavior is unchanged, not
rewritten — this is a refactor-preserving-behavior task, not new behavior, and gets its
own explicit test-diff review in Section 3.

**FINDING (Claude Eng subagent, MEDIUM) — HashMap membership semantics were ambiguous,
risking silent fleet-wide growth.** If the extended `AppEvent::Flight` arm does anything
resembling `.entry(agent_id).or_insert_default()` for every matching event kind (an easy
mistake given "update unconditionally regardless of view," Step 0B), `converse_view`'s map
silently grows to include every agent in the fleet the moment any of them completes any
turn — defeating the entire "who I've actually talked to" framing behind the unread-badge
and 200-turn-ring designs. **Auto-decided (P1 completeness, Mechanical):** the map is
populated ONLY on operator-initiated dispatch (`r` retarget or first message send) —
incoming events for a target NOT already a key in the map are a no-op, never an insert.
Test: "flight event for an untracked agent does not create a map entry" (Section 3).

**FINDING (Claude Eng subagent, MEDIUM) — dead-air window on SSE reconnect unaddressed.**
`pump.rs`'s `Invalidated` path only calls `app.mark_gap()` + forces a `Reconcile` — nothing
in the plan has `ConverseState` listen to `Invalidated` or cross-check the periodic
`Snapshot` (which carries authoritative `status`, e.g. `"waiting"`). A target stuck in
DISPATCHING when the SSE connection times out (`pump.rs`'s `SSE_TOTAL_TIMEOUT = 90s`) shows
"..." with zero feedback for up to ~90+ seconds — no client-side dispatch timeout
independent of the terminal SSE event exists anywhere in the design. **This is a real gap
in Section 2's "zero unrescued rows" claim from Phase 1** — not previously caught.
**Auto-decided (P1 completeness, Mechanical):** add a client-side dispatch timeout (e.g.
30s, matching `agentd`'s own `MCP_TIMEOUT` convention) — on expiry, flush an inline yellow
"no response after 30s — the connection may have dropped" system line with a resume hint,
using the exact rescue pattern already established for every other error path (Section 2).

**FINDING (Claude Eng subagent, MEDIUM) — drop vs. reorder conflated; per-target
attribution is impossible with the current global drop counter.** `pump.rs`'s
backpressure drops events on a full channel and only increments a GLOBAL `dropped_events`/
`EventsDropped` counter — it cannot attribute a drop to a specific target, so
`ConverseState` has no way to know ITS OWN target's chunk was silently dropped (not
reordered — gone). This was moot while no real deltas existed to drop; it is NOT moot once
the streaming-delta event ships. **Auto-decided (P1 completeness, Mechanical, scoped as
part of the same expansion):** the new `EventKind::InferenceStreamDelta`'s `chunk_seq`
field (added above) lets `ConverseState` detect a gap directly (chunk_seq jumps from 4 to
6) without needing per-target drop attribution from `pump.rs` — cheaper and simpler than
plumbing per-target counters through the channel layer. On a detected gap: append a dim
inline note ("[response may be incomplete — connection was busy]") rather than silently
presenting a spliced string as if it were continuous.

**Coupling assessment (revised):** the plan is no longer "100% agentctl-internal" (Phase
1's original claim, now corrected) — `agentd`'s event taxonomy and `scheduler.rs`'s hot
streaming path gain a new, permanent per-token event-recording cost. This is real,
accepted new coupling between the operator-cockpit feature and the core inference loop's
performance profile — flagged explicitly in Performance (Section 4) below, not asserted
as free.

## Section 2: Code Quality Review

**DRY:** confirmed correct, with one correction from the finding above — Step 0B's
"extract into shared match logic" language is revised to "port verbatim, one contract
test per event kind" given the field-path inconsistency finding. Otherwise unchanged from
Phase 1's Section 5 assessment (naming, over/under-engineering, cyclomatic complexity —
no new issues from the streaming-delta expansion; the new `EventKind` variant is one enum
arm plus one `record()` call site, not a new abstraction).

**New finding — `handle_dashboard_key`'s restructuring is a genuine refactor, not an
addition.** Covered above (Section 1); the code-quality angle is specifically: this
function's ~15 existing tests must be individually re-verified to pass unchanged
(`DashboardFocus::Table` as the implicit default), not just "should still pass" — Section 3
below makes this an explicit, named test-diff task.

**No other issues found** beyond what's captured above and in Phase 1/2's still-valid
Sections 2 (error/rescue — extended above with the dispatch-timeout and gap-detection
findings) and 5 (quality).

## Section 3: Test Review (mandatory, full diagram + artifact)

```
  NEW UX FLOWS: send message (inject) · send message (spawn-new) · retarget (`r`) ·
                Tab focus-toggle · scroll-vs-follow · cancel in-flight · resume after
                exit/timeout/dead-air

  NEW DATA FLOWS: agentd: per-chunk text_delta → recorder.record() → broadcast → SSE
                  agentctl: text_delta accumulation into current_reply · terminal-event
                  flush · gap detection via chunk_seq

  NEW CODEPATHS: ConverseState state machine (5 states) · spawn-or-resume branch (ported
                 verbatim, 4 field-path lookups) · buffer-cap truncation · handle_
                 dashboard_key's DashboardFocus retrofit · client-side dispatch timeout

  NEW BACKGROUND JOBS: none new on the agentctl side; agentd's existing print_fut loop
                       gains one additional call per chunk (not a new job, a new call site)

  NEW INTEGRATIONS: agentd-internal only (record() call site) — no new external service

  NEW ERROR/RESCUE PATHS: InjectRejected · SpawnRejected · TransportError · BufferOverflow ·
                          OrchestratorExited (CORRECTED field path) · AgentFailed (CORRECTED
                          field path) · dispatch-timeout (NEW) · stream-gap-detected (NEW)
```

**Test diagram — codepath → coverage:**
```
  CODEPATH                          | TYPE        | HAPPY PATH TEST         | FAILURE TEST              | EDGE CASE TEST
  ------------------------------------|-------------|--------------------------|----------------------------|------------------
  EventKind::InferenceStreamDelta emit| Unit (agentd)| chunk recorded + broadcast| record() failure is best-effort (existing pattern) | chunk_seq monotonic across a full stream
  ConverseState::on_flight_event      | Unit        | text_delta appends to current_reply | untracked-target event is a no-op (HashMap finding) | chunk_seq gap → gap-note appended
  Terminal-event field-path lookups   | Unit, x4    | one per kind, using REAL agentd-emitted fixtures | OrchestratorExited literal-agent gotcha caught by fixture, not hand-typed JSON | AgentFailed missing data.agent_id handled
  handle_dashboard_key retrofit       | Unit (regression) | all ~15 existing tests pass with DashboardFocus::Table | typed char while rail-focused does NOT trigger a view-switch shortcut | Enter routes to chat-send when rail-focused, existing routing when table-focused
  Client dispatch timeout             | Unit        | timeout does not fire before 30s | timeout fires exactly once, flushes inline system line | timeout after SSE Invalidated (interaction with existing gap handling)
  Buffer overflow (64KB)              | Unit         | truncation marker appended | — | exactly-at-cap boundary
  Retarget mid-stream                 | Integration  | prior target keeps streaming in background | retarget to Done/Failed agent → SpawnRejected | rapid retarget x3 (HashMap finding's no-op-on-untracked doesn't fire for tracked targets)
```
**2am-Friday test:** inject a message, kill the mock SSE mid-stream, confirm the dispatch
timeout fires and the render loop never blocks (ux.0's own founding invariant). **Hostile-
QA test:** fuzz the `chunk_seq` field with out-of-order/duplicate/gapped values across a
simulated stream. **Chaos test:** saturate the shared 256-depth channel (`pump.rs`) with
synthetic Snapshot/Approvals traffic while a real stream is in flight, verify the operator
sees a gap-note rather than a silently spliced reply.

**Test plan artifact:** written to
`~/.gstack/projects/0x89karan-runtime1/0x89karan-ux.1-converse-eng-review-test-plan-20260713.md`
(the file list above, expanded into concrete test function signatures per the project's
existing `#[test]`/`#[tokio::test]` conventions in `mod.rs`/`scheduler.rs`).

## Section 4: Performance Review

**New, real cost — flagged, not asserted free (corrects Phase 1's original "no issues
found," which predates the streaming-delta discovery):** `recorder.record()` now fires
once per STREAMED TOKEN CHUNK, not once per turn — a materially higher event rate than any
existing `EventKind`. `flight.jsonl` (append-only, disk-backed) grows proportionally to
total output tokens across every streaming turn, system-wide, forever — a genuine new
storage-growth vector this plan must own, not silently inherit. **Auto-decided (P1
completeness + P2 boil-the-lake, Taste — surfaced at the gate):** two sub-decisions,
presented together since they're the same underlying question:
(a) should `InferenceStreamDelta`'s `text` field be truncated/summarized in `flight.jsonl`
(matching the existing preview/audit-metadata convention) while the FULL text still reaches
the live SSE broadcast (which doesn't need to be replayed from disk, only live-tailed)?
(b) or does the full per-chunk text belong in the durable log too (changing `flight.jsonl`'s
purpose from audit-preview to full-transcript-of-record)?
**Recommendation: (a)** — broadcast the full chunk live (SSE subscribers see everything
in real time, satisfying Premise 2), but truncate/omit the verbatim `text` field before
the JSONL disk-write specifically (keep `chunk_seq`/`agent_id`/`turn_seq` for audit
correlation, drop or cap the text itself) — preserves `flight.jsonl`'s existing
preview-metadata contract instead of quietly turning it into a full model-output store.
This is genuinely a **Taste** decision (reasonable people could pick (b) for completeness)
— flagged at Phase 4's gate, not silently decided.

**Connection/memory:** no new DB, no N+1. `current_reply`'s 64KB cap (unchanged from
Phase 1) and the 200-turn ring (Design phase) still bound client-side memory. **Slow
paths:** the new `record()` call site adds one function call per chunk to `agentd`'s
hot streaming path — negligible CPU, the real cost is disk I/O volume (addressed above).

## Eng Dual Voices — Consensus Table

```
ENG DUAL VOICES — CONSENSUS TABLE:
═══════════════════════════════════════════════════════════════
  Dimension                           Claude   Codex   Consensus
  ──────────────────────────────────── ─────── ─────── ─────────
  1. Architecture sound?               FLAG     FLAG    CONFIRMED — both independently found the streaming premise unbuildable as scoped (the dominant finding of this whole phase)
  2. Test coverage sufficient?         FLAG     FLAG    CONFIRMED — neither original CEO/Design pass had a test verifying the premise itself before building consumption logic
  3. Performance risks addressed?      FLAG     FLAG    CONFIRMED — per-token record() cost + flight.jsonl storage-growth vector, unaddressed in Phase 1's original "no issues found"
  4. Security threats covered?         FLAG     N/A     PARTIAL — Claude subagent flagged unverified body-size limits on inject/spawn HTTP handlers; not independently checked by Codex, noted as unresolved below
  5. Error paths handled?              FLAG     N/A     CONFIRMED — dead-air/dispatch-timeout gap, drop-vs-reorder conflation (Claude subagent, both real, both fixed above)
  6. Deployment risk manageable?       N/A      CONFIRM  CONFIRMED — Codex's scope-impact list (6 files, cross-crate) matches the file count in Step 0's scope-challenge tally exactly
═══════════════════════════════════════════════════════════════
CONFIRMED = both voices (or the only voice that examined that dimension) found the same
real gap. The dominant signal this phase: BOTH subagent and Codex, independently, without
seeing each other's output, caught the exact same load-bearing architectural error that
survived two full review phases (CEO + Design) unnoticed — strong evidence dual-voice
review earns its cost specifically at the phase where code-level verification matters most.
```

**Unresolved from dual voices:** item 4 (HTTP body-size limits on `inject`/`spawn`) —
Claude subagent flagged this as "unverified but flagged," not confirmed either way.
**Auto-decided (P3 pragmatic — verify before shipping, don't block the review on it):**
added as a P2 Implementation Task below (verify `management.rs`'s inject/spawn handlers
have a body-size cap; TUI paste buffers have no natural ceiling the way CLI argv does) —
not re-opened as a review finding requiring its own AskUserQuestion, since it's a
mechanical verification step, not a design decision.

## Worktree Parallelization Strategy

```
  LANE                          | FILES                                          | DEPENDS ON
  --------------------------------|--------------------------------------------------|------------------
  Lane A: agentd streaming event  | events.rs, scheduler.rs, flight_recorder.rs      | none — can start immediately
  Lane B: converse.rs + state     | watch/converse.rs (new), orchestrate.rs (port)   | none — pure logic, testable against fixtures before Lane A ships
  Lane C: Dashboard retrofit      | watch/mod.rs (handle_dashboard_key), app.rs      | none for the refactor itself; BLOCKS on Lane B for wiring converse_view in
  Lane D: render_dashboard rail   | watch/views.rs                                   | BLOCKS on Lane C (needs DashboardFocus) + Lane B (needs ConverseState to render)
```
Lanes A and B are fully independent and can run in parallel worktrees. Lane C's pure
refactor (tuple-match restructuring, 15 existing tests re-verified) can start immediately
and does not need to wait for A or B — only the final `converse_view` wiring into the
key-match arms needs Lane B's types. Lane D is necessarily last (needs both the state
shape from B and the focus-aware key routing from C). **Recommended sequencing:** A + B +
(C's pure refactor) in parallel, then wire C+B together, then D last. Given the size,
this is a genuine candidate for the project's parallel-worktree discipline
(`13-parallel-dev-rules`) rather than one long sequential branch.

### "NOT in scope" (Eng phase)
- Full backpressure/volume load-testing of per-token `record()` at fleet scale (Section 4)
  — functional tests ship with this plan; operational load characterization is post-ship.
- Extending `flight.jsonl`'s storage-growth question to a general "audit log retention
  policy" redesign — scoped to this one new event kind's specific text-truncation
  decision (Section 4's Taste call), not a system-wide policy overhaul.

### "What already exists" (Eng phase)
`Spawn`'s tuple-match focus idiom (reused for Dashboard's retrofit), `orchestrate.rs`'s
four terminal-event field-path lookups (ported verbatim, not re-abstracted), `agentd`'s
existing `record()` best-effort-never-crashes contract (CLAUDE.md invariant, unchanged),
and the existing `MCP_TIMEOUT`-style timeout convention (reused for the new client-side
dispatch timeout).

### Failure Modes Registry (Eng phase additions to Phase 1's table)
```
  CODEPATH                       | FAILURE MODE                | RESCUED? | TEST? | USER SEES?           | LOGGED?
  ---------------------------------|-------------------------------|----------|-------|------------------------|--------
  Client dispatch timeout (NEW)    | no terminal event within 30s  | Y        | Y (Section 3) | inline yellow + resume hint | Y (server-side, existing events still fire when they arrive)
  chunk_seq gap detected (NEW)     | dropped chunk (channel backpressure) | Y | Y (Section 3) | dim gap-note inline  | N (client-only; consistent with Phase 1's Section 8 taste call)
  OrchestratorExited field-path    | wrong identity field trusted   | Y (ported verbatim + tested) | Y (Section 3) | n/a (prevented, not user-visible) | n/a
  AgentFailed field-path           | same class of bug              | Y (ported verbatim + tested) | Y (Section 3) | n/a | n/a
  ---------------------------------|-------------------------------|----------|-------|------------------------|--------
```
No row has RESCUED=N + TEST=N + USER SEES=Silent → **no CRITICAL GAP**, but this table is
now materially longer than Phase 1's original — the streaming-premise discovery added
4 genuinely new failure modes that a plan built on a false wire-format assumption could
never have surfaced, because the failure modes didn't exist until the real event did.

### Completion Summary

```
+====================================================================+
|            ENG PLAN REVIEW — COMPLETION SUMMARY                   |
+====================================================================+
| Scope challenge      | 8-9 files, MUST-STOP threshold crossed —    |
|                       | already resolved via prior AskUserQuestion  |
| Section 1  (Arch)     | 1 CRITICAL (streaming premise, resolved via |
|                       | scope expansion) + 4 findings (field-path,  |
|                       | key-routing retrofit, HashMap membership,   |
|                       | dead-air timeout) — all resolved             |
| Section 2  (Quality)  | 1 finding (DRY framing correction)          |
| Section 3  (Tests)    | Full diagram + artifact written to disk     |
|                       | (~/.gstack/projects/.../eng-review-test-    |
|                       | plan-20260713.md)                            |
| Section 4  (Perf)     | 1 Taste decision (flight.jsonl storage      |
|                       | policy) — flagged at gate                    |
+--------------------------------------------------------------------+
| NOT in scope          | written (2 items)                            |
| What already exists   | written                                      |
| Failure modes         | 9 total (5 from Phase 1 + 4 new), 0 CRITICAL |
| Worktree strategy     | 4 lanes, A+B+C-refactor parallel, D last     |
| Outside voice         | ran (Claude subagent + Codex, both foreground, both independently found the critical finding) |
| Unresolved decisions  | 1 Taste (flight.jsonl text storage policy) — surfaced at Phase 4 gate |
+====================================================================+
```

**PHASE 3 COMPLETE.** Codex: 4 findings (1 critical — same as Claude subagent's, independently).
Claude subagent: 6 findings (1 critical, 5 high/medium — all resolved above). Consensus:
6/6 dimensions examined found real gaps (5/6 CONFIRMED by both or the only voice that
checked; 1/6 partial/unresolved, tracked as a P2 task). This is the single most consequential
review phase in this pipeline — it found the one finding that would have made the entire
plan undeliverable as originally scoped. Passing to Phase 3.5 (DX Review) — DX scope
confirmed in Phase 0 (agentctl is a CLI tool).

---

# DX Review — 8 Passes (Phase 3.5)

**Product type:** CLI/TUI tool (`agentctl`), single-tenant, solo-operator audience (per
CLAUDE.md's locked design). **Persona:** the existing `agentctl watch`/`orchestrate` user —
not a new-to-the-product developer; this is a returning-user discoverability problem, not
a first-install onboarding problem. **Mode:** DX POLISH (existing product, adding one
feature) per the autoplan override rules.

Both dual voices ran against the Phase 1-3 plan and — like Design and Eng before them —
found real, concrete gaps neither self-authored pass nor the earlier phases caught.
**Verdict from both: initial DX FAIL**, driven by four consistent findings:

## Pass 1: Getting Started — FAIL → fixed

**Finding (both voices, critical):** the plan never specifies a footer key-hint for the
NORMAL (≥115-col) Dashboard render — only the narrow-terminal fallback got a hint (Pass 6).
Every other view's discoverability comes entirely from the persistent footer legend
(`views.rs:306`, `[c]reds` etc.) — without an equivalent, an existing operator upgrading
`agentctl` gets zero in-app signal that `Tab`/`r` now do anything. **RESOLVED:** the
Dashboard footer gains a THIRD state (existing footer already varies by spawn-banner
presence): `" ↑/↓ select  Enter: view detail  Tab: chat  r: retarget  [s]ystem  [t]opology  [m]emory  [n]ew  [a]pprove  [c]reds  [i]nspector  q quit "` when the rail has room (≥115
cols) and the rail does NOT have focus; when the rail DOES have focus, the footer swaps to
`" Esc: back to table  Enter: send  ↑/↓: history/scroll  ...`. This is the exact same
idiom the footer already uses (a static hint string per state) — zero new UI mechanism,
just a third rendered string alongside the two that already exist.

## Pass 2: API/CLI/TUI Design — mostly consistent, one real gap named

**Finding (both voices, medium):** `Tab`/`Esc` correctly reuse existing idioms (Memory/
Spawn's sub-pane cycling, Spawn's TaskField capture). But `r` introduces something the
plan never named as a finding: every OTHER single-letter key on Dashboard means "leave
this view" (navigate to a different `View`) — `r` alone means "stay here and act on the
selected row." That's a new key *category* an operator has to learn is different, silently.
**RESOLVED:** the new footer hint (Pass 1) explicitly labels `r` distinctly from the
navigation letters (visually separated by extra spacing in the format string above,
listed first, before the `[x]` bracket-style nav keys) — a cheap, in-idiom fix (footer
copy, not new code) rather than inventing a visual distinction mechanism.

## Pass 3: Error Messages — FAIL → fixed

**Finding (both voices, high) — three specific problems, not a vague "improve copy"
note:**
1. `SpawnRejected`/`AgentFailed` interpolate a raw `<reason>` string from an unspecified
   source — if that's a raw server/HTTP error passed through verbatim, it contradicts this
   plan's own "never just an unexplained error" design intent (Section 11). **RESOLVED:**
   `<reason>` is the SAME curated string `agentd`'s existing `orchestrator_exited`/
   `agent_failed` events already carry today (`orchestrate.rs:166,183` already consume
   these; they are enum-derived, human-readable strings server-side, not raw transport
   errors) — pass through verbatim IS correct here, this was a documentation-precision gap
   in the plan, not an actual design decision left open. Clarified explicitly rather than
   left implicit.
2. Three of eight rescue-table rows (`SpawnRejected`, `AgentFailed`, the new dispatch-
   timeout message) lacked an explicit next-action verb despite the plan's own stated goal.
   **RESOLVED**, concrete copy:
   - `SpawnRejected` → `"Spawn rejected: {reason} — press Enter to retry"`
   - `AgentFailed` → `"Agent failed: {reason} — press r to retarget, or Enter to resume elsewhere"`
   - dispatch-timeout → `"No response after 30s — connection may have dropped. Press Enter to retry."`
3. `BufferOverflow`'s truncation marker didn't explain the cap to the operator.
   **RESOLVED:** `"[...truncated at 64KB — full reply may be longer]"` (states cause,
   no action needed since it's cosmetic/non-blocking, consistent with why this one row
   correctly has no verb).

## Pass 4: Documentation — FAIL → fixed (the sharpest finding of this phase)

**Finding (both voices, high, verified against actual doc files, not asserted) — this
plan's own Eng-phase file list (Step 0's 8-9 files) contains ZERO `.md` files, directly
conflicting with CLAUDE.md's own locked convention: "Update `docs/ROADMAP.md`... and any
affected doc in the same PR as the code."** Concretely:
- `docs/INTERFACE.md` §3 (lines 82-107) documents `agentctl`'s views/keybinding model —
  and is **already stale relative to shipped reality** (describes number-key + `[1]
  Dashboard`-style tab-bar navigation; the actual shipped keymap uses single letters).
  This predates ux.1 and none of Phases 1-3 caught it — a systemic miss, not one-off.
- `docs/RUNBOOK.md` and `docs/cos-guide.html` both correctly document `[c]`=Credentials
  today — **that reference is still correct and does NOT need to change** (Credentials
  keeps `[c]`; only the chat rail moved off it to `Tab`/`r`). No false alarm here, but
  worth stating explicitly rather than leaving ambiguous.
**RESOLVED — added to Implementation Tasks (Phase 4):** (a) update `docs/INTERFACE.md` §3
to reflect the ACTUAL shipped keymap (single-letter, not number/tab-bar) AND the new
Dashboard rail/focus model — this closes a pre-existing drift bug as a byproduct, not
scope creep, since the section being touched is the one this plan's own feature lands in.
(b) No changes needed to RUNBOOK.md/cos-guide.html (verified correct, noted not to avoid
someone "fixing" something that wasn't broken).

## Pass 5: Upgrade & Migration Path — one real inconsistency, one gap, both fixed

**Finding (both voices, medium) — the plan's own "no regression" claim for `orchestrate.rs`
contradicts a deliberate behavior change it makes in the same breath.** Eng Phase 3
decided to fix the pre-existing 512-char truncation of `orchestrator_turn_complete`'s
`answer` field as a byproduct of the streaming-delta work — this IS a visible, intentional
change to `orchestrate.rs`'s existing CLI output (previously-truncated replies now show
full text) for any operator running against non-colocated `agentd`. **RESOLVED:**
reclassified explicitly — "CLI REPL no-regression" (acceptance criterion + test) means
*the spawn/inject/resume mechanics and REPL loop structure are unchanged*; the truncation
fix is an intentional, positive, called-out behavior change to output completeness, not
something smuggled in under "no regression." Both stated as separate facts now, not one
overloaded claim.

**Finding (Claude subagent, medium) — two unaddressed ambiguities:**
1. Does `orchestrate.rs`'s CLI gain live token streaming too (consuming the new
   `InferenceStreamDelta` events), or does it stay block-until-terminal-event?
   **RESOLVED — added to "NOT in scope":** `orchestrate.rs` does NOT gain live streaming
   in this plan; it keeps its existing block-then-print behavior, now against
   un-truncated text. An operator who sees the `watch` rail stream live and asks "why
   doesn't my CLI do this" gets an honest, stated answer (a follow-up increment), not a
   silent gap.
2. `drain_until_turn_complete`'s blocking `for line in reader.lines()` loop
   (`orchestrate.rs:144`) reads the SAME SSE connection that will now also carry every
   per-token delta event — for a long reply, the CLI's blocking loop now scans through N
   delta lines per turn while waiting for the terminal event, a small but real, previously
   uncounted CPU/latency cost on the EXISTING CLI path. **RESOLVED — added to
   Implementation Tasks:** `drain_until_turn_complete` gets a cheap early-continue on
   `kind == "inference_stream_delta"` (one string comparison, already how it skips
   non-`data:` lines) — not covered by "CLI REPL no-regression" as originally scoped, now
   explicitly is.

## Pass 6: Developer Environment & Tooling

No new dependencies, no new build step, no new CI job — an in-tree Rust change to two
existing crates, tested the same way every other `agentctl`/`agentd` change is tested
(`cargo test`, `cargo clippy -- -D warnings`, per CLAUDE.md's existing gate). **No issues
found.**

## Pass 7: Community & Ecosystem

Not applicable in any meaningful way — single-tenant, solo-operator tool per CLAUDE.md's
locked design; no external community/plugin/pricing surface exists for `agentctl` today,
and this plan doesn't change that. **Examined, confirmed genuinely N/A, not skipped.**

## Pass 8: DX Measurement & Feedback Loops

**Finding (auto-decided, Taste, minor):** no TTHW-style metric exists for "time from
`agentctl` upgrade to first successful chat rail use" — but this product has no telemetry
pipeline at all (single-tenant, no phone-home, consistent with the whole project's
architecture) so this would be a net-new instrumentation surface, disproportionate to a
one-feature addition. **Not pursued** — consistent with Pass 7's finding that this
product's DX model doesn't include measurement infrastructure by design.

### Version-skew finding (Codex, medium — genuinely new, not covered above)

**Finding:** an operator running a NEWER `agentctl` (with this plan's rail) against an
OLDER `agentd` (without the new `InferenceStreamDelta` event) gets silence — the rail
would sit in DISPATCHING indefinitely with no delta ever arriving, indistinguishable from
a hung connection until the eventual terminal event (or the new 30s dispatch timeout,
Eng Phase 3) fires. **RESOLVED (auto-decided, P1 completeness, Mechanical):** this is
exactly what the Eng-phase dispatch timeout already catches — no NEW mechanism needed, but
the timeout's copy is revised to be version-skew-aware: `"No response after 30s — this
agentd may not support live streaming yet (upgrade agentd) or the connection dropped."`
One string change, not new code — flagged as its own Implementation Task so it isn't lost
in the general timeout-copy task from Pass 3.

### DX Scorecard
```
+====================================================================+
|              DX PLAN REVIEW — SCORECARD                             |
+====================================================================+
| Getting Started      | 3/10 → 9/10  (footer hint added)            |
| API/CLI/SDK          | 7/10 → 9/10  (r-vs-nav-key distinction noted in footer) |
| Error Messages        | 5/10 → 9/10  (3 rows fixed, reason-source clarified) |
| Documentation         | 2/10 → 8/10  (INTERFACE.md task added; RUNBOOK/cos-guide verified fine) |
| Upgrade Path          | 6/10 → 9/10  (regression claim de-conflated; 2 gaps closed) |
| Dev Environment       | 9/10 (no change)                            |
| Community             | N/A                                          |
| DX Measurement        | N/A (by design, product has no telemetry)   |
+--------------------------------------------------------------------+
| TTHW                  | N/A (returning-user discoverability, not install-to-hello-world) |
| Overall DX             | 4.8/10 → 8.8/10                              |
+====================================================================+
```

**PHASE 3.5 COMPLETE.** Codex: 5 findings (verdict: initial FAIL, all addressed above).
Claude subagent: 5 findings (including catching 2 stale `[c]` references my own "fixed
throughout" claim had missed — now corrected). Consensus: both voices independently
converged on "no footer hint" and "no docs in scope" as the two dominant gaps — the
weakest phase of this plan going in, now the most concretely improved. Passing to
Phase 4 (Final Approval Gate).

---

## Implementation Tasks (aggregated across all 4 phases)

- [ ] **T1 (P1, human: ~1d / CC: ~1h)** — agentd — Add `EventKind::InferenceStreamDelta`
  - Surfaced by: Eng Section 1 — the critical streaming-premise finding (3-way confirmed)
  - Files: `agentd/src/events.rs`, `agentd/src/scheduler.rs` (print_fut loop, ~line 1053)
  - Verify: `test_stream_delta_recorded_per_chunk`, `test_stream_delta_chunk_seq_monotonic`
- [ ] **T2 (P1, human: ~2h / CC: ~20min)** — agentd — `flight.jsonl` text-truncation policy for the new event
  - Surfaced by: Eng Section 4 — Taste decision, recommendation (a): broadcast full text live, truncate on disk
  - Files: `agentd/src/flight_recorder.rs`, `agentd/src/scheduler.rs`
  - Verify: `test_stream_delta_flight_jsonl_text_truncated`
- [ ] **T3 (P1, human: ~4h / CC: ~30min)** — agentctl — `watch/converse.rs` shared spawn/resume + event-recognition helper
  - Surfaced by: Step 0B leverage map, Eng Section 1's field-path finding
  - Files: `agentctl/src/watch/converse.rs` (new), `agentctl/src/orchestrate.rs` (refactored to call it)
  - Verify: `test_spawn_or_resume_ported_verbatim`, 4x `test_terminal_event_field_paths_*`
- [ ] **T4 (P1, human: ~1d / CC: ~1h)** — agentctl — `ConverseState` state machine + `HashMap<AgentId, ConverseState>`
  - Surfaced by: CEO Section 1, Eng Section 1 (HashMap membership finding)
  - Files: `agentctl/src/watch/app.rs`, `agentctl/src/watch/converse.rs`
  - Verify: state-machine unit tests + `test_untracked_target_event_is_noop` + `test_retarget_mid_stream_keeps_prior_target_running`
- [ ] **T5 (P1, human: ~1d / CC: ~45min)** — agentctl — `handle_dashboard_key` retrofit to `(DashboardFocus, KeyCode)` tuple match
  - Surfaced by: Eng Section 1 — key-routing finding
  - Files: `agentctl/src/watch/mod.rs`
  - Verify: all ~15 existing tests pass unchanged + 5 new focus/retarget tests
- [ ] **T6 (P1, human: ~1d / CC: ~1h)** — agentctl — `render_dashboard` horizontal split (table `Min(72)` | rail `Length(32)`) + width/height floor fallback
  - Surfaced by: Design Pass 1/6 (corrected width math)
  - Files: `agentctl/src/watch/views.rs`
  - Verify: `test_min_total_width_for_rail_115_hides_rail_below_floor`, `test_min_rail_height_8_hides_rail_below_floor`, `test_border_title_truncation_with_unread_badge_fits_32_cols`
- [ ] **T7 (P1, human: ~2h / CC: ~20min)** — agentctl — client-side dispatch timeout (30s) + version-skew-aware copy
  - Surfaced by: Eng Section 1 (dead-air finding) + DX version-skew finding
  - Files: `agentctl/src/watch/converse.rs`
  - Verify: `test_client_dispatch_timeout_fires_at_30s`, `test_client_dispatch_timeout_interacts_with_sse_invalidated`
- [ ] **T8 (P2, human: ~1h / CC: ~10min)** — agentctl — Dashboard footer: 3-state hint string (unfocused-with-rail / focused-rail / narrow-fallback)
  - Surfaced by: DX Pass 1 — the single biggest DX gap found
  - Files: `agentctl/src/watch/views.rs`
- [ ] **T9 (P2, human: ~1h / CC: ~10min)** — agentctl — error-copy fixes for `SpawnRejected`/`AgentFailed`/dispatch-timeout (add next-action verbs)
  - Surfaced by: DX Pass 3
  - Files: `agentctl/src/watch/converse.rs`
- [ ] **T10 (P2, human: ~30min / CC: ~10min)** — agentctl — `drain_until_turn_complete` early-continue on `inference_stream_delta`
  - Surfaced by: DX Pass 5 — uncounted CPU/latency cost on the existing CLI path
  - Files: `agentctl/src/orchestrate.rs`
- [ ] **T11 (P2, human: ~30min / CC: ~10min)** — agentd — verify `management.rs` inject/spawn body-size limits
  - Surfaced by: Eng dual-voice consensus, item 4 (unresolved, verification not design)
  - Files: `agentd/src/management.rs`
- [ ] **T12 (P2, human: ~2h / CC: ~20min)** — docs — update `docs/INTERFACE.md` §3 keybinding model (closes pre-existing drift + documents new rail)
  - Surfaced by: DX Pass 4
  - Files: `docs/INTERFACE.md`
- [ ] **T13 (P3, human: ~1h / CC: ~10min)** — agentctl — unread/background-activity badge on target selector
  - Surfaced by: Design dual-voice item 4
  - Files: `agentctl/src/watch/app.rs`, `views.rs`
- [ ] **T14 (P3, human: ~30min / CC: ~5min)** — agentctl — 200-turn ring buffer for flushed transcript history
  - Surfaced by: Design dual-voice item 5
  - Files: `agentctl/src/watch/app.rs`

## Cross-Phase Themes

**Theme: "the plan's own claims of completeness needed independent verification, not just
internal consistency."** Flagged in CEO Phase 1 (sequencing/layout — verified against
actual roadmap docs, not assumed), Design Phase 2 (width math, keybinding collision —
verified against actual `views.rs`/`mod.rs`), and decisively in Eng Phase 3 (the streaming
premise — verified against actual `agentd` source, 3-way independent confirmation). This is
a strong, recurring signal across independently-run phases: **every major finding in this
entire review came from checking a claim against the actual codebase, never from re-reading
the plan's own prose more carefully.** High-confidence pattern, not a one-off.

## Deferred to TODOS.md

- TODO 1 (D1 scope decision — added earlier, Phase 1).
- No new TODOS.md items from Phases 2-3.5 — everything found was either resolved inline
  (Mechanical) or promoted to an Implementation Task above (concrete, scheduled work),
  consistent with this plan's own pattern of not deferring speculatively.

---

---

## Step 0D — Mode-Specific Analysis (SELECTIVE EXPANSION)

This is a feature enhancement on an existing, working system (`watch` + `orchestrate.rs`
both ship today) — SELECTIVE EXPANSION is the correct default per the CEO skill's own
mode-selection guidance, and nothing found so far argues against it. Scope stays bounded
to: `View::Converse` + `ConverseState` + shared spawn/inject helper extraction + streaming
accumulation. Explicitly OUT: the permanent split-pane layout (deferred per 0C above),
ux.3's spawn-flow fix (separate increment), and any new MCP/event-kind plumbing (none
needed — the pipe is already generic, confirmed in "What already exists").

## Step 0E — Temporal Interrogation

```
HOUR 1   Operator runs `agentctl watch`, sees the chat rail already visible beside the
         agent table (no keypress needed to reveal it — it's part of Dashboard), presses
         `Tab` to focus it. Sees an empty transcript, input box bordered
         `┤ → orchestrator ├`. Types a task, hits Enter. Message appears in the
         transcript instantly (optimistic echo, not waiting on the network round-trip).
         Border/status shows "sending..." briefly.
HOUR 6+  Operator has gone back and forth with the orchestrator, retargeted into
         `agent:scout-3` mid-conversation to check its state directly, scrolled up
         to re-read an earlier reply (follow flag disarms), then hit `G` to re-follow
         and caught a live-streaming reply from a retry after one `orchestrator_exited`
         bounced with a resume hint. Dashboard view (Topology/Memory) has stayed
         completely live and unaffected by any of this — no view starves another.
```

No 6-month-later scenario is meaningful here (this is a UI ergonomics increment, not
a system with drift/decay risk) — the CEO skill's temporal check is satisfied by the
HOUR 1 / HOUR 6+ granularity above, which is the granularity that actually matters for
an interactive TUI feature.

## Step 0F — Mode Selection: SELECTIVE EXPANSION (confirmed)

Scope is bounded (one new `View`, one new state struct, one shared helper extraction,
no new event plumbing, no new MCP surface, no new capability). This is not a HOLD SCOPE
(there IS real new capability — acting, not just watching) and not a SCOPE REDUCTION or
full EXPANSION (the split-pane dream-state layout is explicitly deferred, not pursued).
SELECTIVE EXPANSION is confirmed as the correct default and nothing in 0A-0E argues
against it.

## ⚠ SUPERSEDED (Eng Phase 3 finding) — Step 0D/0F's "no new event plumbing" claim is wrong

Both the "no new MCP/event-kind plumbing" line in Step 0D and "no new event plumbing" in
Step 0F above were written and confirmed **before** Eng Phase 3 verified, against actual
`agentd` source (three independent traces converged: mine, Codex's, and the Claude Eng
subagent's — see Eng Phase 3 below), that `text_delta` events do NOT exist anywhere on the
wire this plan consumes. Premise 2 (live token-by-token streaming, confirmed early in this
review) requires genuinely new `agentd`-side work: a new `EventKind` recorded per-chunk in
`scheduler.rs`'s streaming path, wired into `FlightRecorder`'s broadcast channel. **User
decision (Eng Phase 3 gate): expand scope to include this agentd-side work** rather than
descope Premise 2 or split it into a separate prerequisite increment. This plan is
therefore no longer "100% agentctl-internal" (Section 1's original coupling assessment) —
it now spans `agentd/src/events.rs`, `scheduler.rs`, and `flight_recorder.rs` in addition
to the `agentctl` surface. See Eng Phase 3 below for the full architecture, test, and
scope-challenge treatment of this expansion. Left in place above rather than silently
edited, per this plan's own established discipline (Codex flagged stale-text-without-
annotation as a real implementation-drift risk during Phase 2 — this note is the fix
applied to itself).

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 1 | CLEAR (PLAN via /autoplan) | 0 proposals, 0 accepted, 0 deferred — SELECTIVE EXPANSION, 3 critical dimensions found via dual voice (sequencing + layout), all resolved via user-directed pause |
| Codex Review | `/codex review` | Independent 2nd opinion | 4 | CLEAR | ran every phase (CEO/Design/Eng/DX), 4/4 phases contributed at least one confirmed finding |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | CLEAR (PLAN via /autoplan) | 11 issues, 0 critical gaps, 1 unresolved (Taste, resolved at Phase 4 gate) |
| Design Review | `/plan-design-review` | UI/UX gaps | 1 | CLEAR (PLAN via /autoplan) | score: 6/10 → 10/10, 14 decisions |
| DX Review | `/plan-devex-review` | Developer experience gaps | 1 | CLEAR (PLAN via /autoplan) | score: 5/10 → 9/10, TTHW: n/a → n/a (returning-user discoverability, not install-flow) |

**CODEX:** ran independently in all 4 phases; found the plan's single load-bearing
architectural error (the `text_delta`/streaming-premise gap) in Eng Phase 3, matching the
Claude subagent's independent finding exactly — the strongest cross-model signal in this
review. Also independently caught the `[c]`/Credentials keybinding collision in Design
Phase 2 before the Claude subagent's more conceptual framing of the same issue.

**CROSS-MODEL:** total overlap across all 4 phases — every critical/high finding that
either voice raised was independently reached by the other voice or was outside that
voice's examined scope (never a case of one voice inventing a finding the other actively
disputed). Zero cross-model tension requiring a user tiebreak; the only user-facing
decisions were the 2 mid-review pauses (sequencing/layout, streaming scope) and the final
gate's 1 Taste call — all resolved.

**VERDICT:** CEO + DESIGN + ENG + DX CLEARED — plan APPROVED (2026-07-13), ready to
implement. 14 Implementation Tasks queued (T1-T14), 4 worktree lanes defined (Eng Phase 3),
1 TODO filed (TODOS.md, D1 scope decision).

NO UNRESOLVED DECISIONS
