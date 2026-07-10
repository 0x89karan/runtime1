# Operator Cockpit — Converse · Observe · Spawn (Track UX)

**Track:** UX (operator cockpit)
**Increments:** ux.0 → ux.2 → ux.1 → ux.3
**Status:** Planned — decisions locked (2026-07-10). Not started.
**Depends on:** orch.1/orch.2 (management API spawn/inject + SSE), cred.5 (credential surface), p7.7 (`:7999`).
**Companion track (separate plan):** curated MCP connectors via the credential broker (#5 — Calendar/GitHub/Linear/Slack/Notion). Not covered here.

## Goal

Turn `agentctl watch` into the thing the operator *drives*, not just a read-only dashboard.
Three capabilities, on one live screen:

1. **Converse** — chat the orchestrator and direct/inject any specific agent, without leaving the dashboard.
2. **Observe** — see per-agent activity (last tool call, errors, idle/stuck signal) at a glance, plus a readable live event stream.
3. **Spawn on the fly** — define a *custom* sub-agent live (task + caps + tools) and fire it into the *running* instance, then immediately watch/talk to it.

This closes the "reach/usability" gap that a comparison with Hermes Agent surfaced, and it's the
operator half of the Chief-of-Staff-on-owned-hardware direction.

## What already exists (the substrate is built)

The hard parts landed in orch.1/orch.2/cred.5. The management API (`agentd/src/management.rs`, `:7999`) exposes the entire cockpit backbone:

```
GET  /healthz
GET  /api/v1/snapshot                    agent states (SchedulerSnapshot)
GET  /api/v1/events                      SSE live event stream (broadcast fan-out)
POST /api/v1/spawn                        → 201 {"agent_id":"…"} into the RUNNING instance
POST /api/v1/agents/:id/inject            → push a turn into a live agent
GET  /api/v1/approvals  + approve/deny
GET  /api/v1/credentials                  cred.5 surface
```

- `agentctl/src/orchestrate.rs` is a **working** spawn→persistent-SSE→inject REPL, with all the
  terminal-state guards (`orchestrator_turn_complete`, `agent_failed`, `agent_completed`,
  `orchestrator_exited`) already debugged. It is **CLI-only and separate from `watch`** — you
  orchestrate *or* you watch, never both.
- `agentctl/src/watch/` has views: Dashboard, AgentDetail, System, Topology, Memory, Spawn,
  Inspector, Approvals, Credentials — driven by a **synchronous poll-render loop** (`DataSource`
  is *polled* each tick). `source.rs` already abstracts FUSE vs HTTP behind `DataSource`.
- `watch/spawn.rs` (`SpawnViewState`) builds a config then **execs a *second* agentd**
  (`PendingSpawn` → `run_tui` execs the binary). Wrong for a live cockpit.

**So this track is mostly a `agentctl`-client effort on a built substrate** — low-risk, high-value.

## Locked decisions (2026-07-10)

- **D1 — Unified live cockpit + ux.0 refactor.** Not three more `[key]` tabs. One screen:
  k9s-style agent table (spine) + a pinned chat rail + a live event stream + a bottom input box,
  with a `:` command palette over the existing single-letter shortcuts. This requires converting
  `watch` from sync-poll to an async single-loop first (**ux.0**), preserving every current view's
  behavior — the direct analog of the p1.1 "loop → steppable state machine" refactor.
- **D2 — Publish host-loopback.** The Docker `cos` deployment binds the management API to
  `0.0.0.0` *inside the container* and publishes `127.0.0.1:7999:7999`, so `agentctl watch --url
  http://localhost:7999` works from the Mac host directly (supersedes the F2 `docker compose exec`
  workaround). **The agentd default stays `bind_addr = 127.0.0.1`** — only the deployment config
  opts into the wider bind, and it is published *only* to host loopback (never the LAN).

## Architecture — one loop, three producers, one channel

The non-negotiable backbone (validated by the research pass — ratatui async idiom + `claux` +
k9s). A single `tokio::select!` loop selects over:

1. **crossterm key events**
2. **the SSE / `HttpSource` event feed** (the persistent `/api/v1/events` connection — the one
   `orchestrate.rs` already opens)
3. **a ~30 ms render tick** that *coalesces* redraws

Keys and events mutate one `App`; the tick draws. `DataSource` becomes a **producer that pushes
into the channel**, not something polled synchronously. This is what keeps chat streaming, meters
updating, and the spawn modal responsive without any pane starving another.

**Pitfall (the #1 ratatui-chat bug): never `.await` an inference/SSE read on the render thread.**
The `select!` + channel split is mandatory.

### Target layout (assembled progressively across increments)

```
┌ header: instance · model · total $ · budget bar (btop-colored) ────────────┐
├──────────────────────────────┬─────────────────────────────────────────────┤
│  AGENT TABLE (k9s)            │  CHAT / CONVERSE RAIL                        │
│  NAME STATUS TURN LAST-TOOL   │  color-coded transcript (green=streaming)   │
│  ● scout run 7 web_search…    │  follow-on-bottom + "▼ N new"               │
│  ✗ writer err 2 kv_set        │                                             │
│  (select row → scopes stream  │                                             │
│   below + retargets input)    │                                             │
├──────────────────────────────┴─────────────────────────────────────────────┤
│  LIVE EVENT STREAM (summary-first · filter chips: All Err Sandbox Cap)      │
│  12:04:01 [scout] 🔧 web_search → 4 hits          (Enter = expand JSON)     │
├─────────────────────────────────────────────────────────────────────────────┤
│  INPUT  ┤ → orchestrator ├  type… (Enter send · Alt+Enter newline)          │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Navigation:** keep single-letter shortcuts (`[t]`,`[m]`,`[a]`,`[i]`,`[c]`) as the discoverable
on-ramp; add a `:` command palette (`:agents`,`:topology`,`:memory`,`:spawn`,`:approvals`,`:inspect`)
for power users. `Enter` drills down, `Esc`/`q` pops one level (never trap the operator). `Tab`
moves focus across the two live panes (brighten the focused border).

**`--plain` mode must be preserved throughout** — pair every color with a glyph (`●`/`✗`/`▲`) so
16-color / colorblind / plain operators lose nothing.

---

## ux.0 — Async single-loop foundation + host-loopback reachability

**Goal:** Convert `watch` to the `tokio::select!` loop and make the cockpit reachable from the
Mac host. **No user-visible new feature** beyond existing views now updating live from SSE.

**Scope:**
- `watch/mod.rs` / `app.rs`: replace the sync poll-render loop with one `tokio::select!` over
  (keys, SSE feed, ~30 ms render tick). Coalesce redraws behind the tick (don't draw per event).
- `source.rs`: `DataSource` gains a **push** path — a background task holds the persistent
  `/api/v1/events` SSE connection and forwards typed events into an `mpsc` channel; snapshot polls
  become a slower fallback tick (e.g. 1 s) for fields not carried by events. `FuseSource` keeps a
  poll-based producer (no SSE over FUSE — documented degradation).
- Bounded in-memory event ring (cap ~1–2 k lines; tail, don't accumulate — mirror
  `MAX_DISPLAY_ENTRIES` discipline).
- **Reachability:** `distro/overlay/etc/agentd/cos.agents.toml` + `agentd/cos.agents.toml`
  `[management] bind_addr = "0.0.0.0"` (deployment-scoped only); `docker-compose.yml` `cos` service
  gains `ports: ["127.0.0.1:7999:7999"]`. **agentd default `bind_addr` unchanged (127.0.0.1).**
  Update `docs/DEPLOYMENT.md` Path 1 step 3 to `agentctl watch --url http://localhost:7999` and
  drop the exec-only note. Add a `THREAT_MODEL.md` note: the management API is unauthenticated;
  host-loopback publish means any local process/user on the host can reach it — acceptable under
  the single-tenant lock, same trust boundary as the FUSE surface; never publish beyond loopback.

**Acceptance:**
- [ ] Every existing view (Dashboard/Detail/System/Topology/Memory/Spawn/Inspector/Approvals/Credentials) behaves identically, but now updates live between snapshot polls.
- [ ] The render loop never blocks on an SSE read or an inference stream (test: a stalled SSE producer does not freeze key handling).
- [ ] Event ring is bounded (test: 10 k events → memory stays flat, oldest dropped).
- [ ] From the Mac host (Docker `cos` up): `agentctl watch --url http://localhost:7999` connects — no `docker compose exec` needed.
- [ ] agentd default bind is still `127.0.0.1` (test/config assertion); only the cos deployment binds `0.0.0.0`.

---

## ux.2 — Observe: per-agent activity + live stream (closes cos-ux-01)

**Goal:** Answer "what is each agent doing right now / who needs attention" at a glance. Closes
**cos-ux-01** (TODOS.md).

**Scope:**
- **Snapshot fields** (`surfaces` `AgentSnapshot`): `last_activity` (tool name + truncated arg
  summary + result summary + ts), `last_error` (`is_error` result or `capability_denied`),
  `error_count`, `idle_secs` (time since last event). Populate from the flight events the
  scheduler already emits; **redact secrets** (reuse existing redaction — never surface a
  credential-shaped token in a preview). Live-refine `last_activity`/`idle_secs` from the SSE feed
  between polls.
- **Agent table** (the cockpit home): columns `AGENT  STATUS  TURN  LAST-TOOL  TOKENS  $  AGE  ⚠`.
  `LAST-TOOL` renders readably (`web_search("q3 revenue…")` / `oauth_call_api → 200` /
  `⚠ mcp_error: timeout`). Color-encode: status dot (running=green, waiting=cyan, error=red,
  terminated=grey); budget bar reusing the existing `MemoryPressure` 75/90 thresholds’ colors;
  **whole row red on error**; `idle Ns` flips amber past a threshold (the "is it stuck" signal).
  Glyph + color always (`--plain` safe).
- **Live event stream pane:** summary-first one-liners (`HH:MM:SS [agent] ICON summary`), raw JSON
  on `Enter`/expand only. Reuse the Inspector filter model (All/Errors/Sandbox/CapDenied) as
  toggle chips with a visible active-filter indicator. Selecting an agent row **scopes** the
  stream to that `agent_id` (k9s follow-selection). `f`/`Space` **freezes** auto-scroll; a
  `▼ N new` affordance when frozen/scrolled-away.
- **AgentDetail:** activity timeline (last ~10 readable events for that agent) + persistent error
  strip + `TURN n · infer 2.3s · tool 0.4s · idle 12s` line. Reuse `short_term_previews`.

**Acceptance:**
- [ ] Each agent row shows its current tool call readably; no raw JSON in the table.
- [ ] When the CoS inbox agent hits "Not authenticated" / a tool error, its row goes red and the error is visible without opening Detail or reading `flight.jsonl`.
- [ ] `idle_secs` past threshold renders amber; a hung vs busy agent is distinguishable.
- [ ] Selecting a row scopes the stream; `f` freezes it; `▼ N new` shows when frozen.
- [ ] Secrets never appear in `last_activity`/stream previews (test with a credential-shaped tool arg).
- [ ] `--plain` mode conveys the same state via glyphs.

---

## ux.1 — Converse: chat the orchestrator + direct any agent

**Goal:** Fold the `orchestrate.rs` REPL into the cockpit as the pinned chat rail; retarget any
agent.

**Scope:**
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

---

## ux.3 — Spawn a custom sub-agent on the fly (into the running instance)

**Goal:** Define + launch a *custom* agent live, into the running agentd, and immediately watch/talk
to it. Closes **p7.3-ar-02** (`agentctl spawn` execs a second agentd instead of routing to the live
instance).

**Scope:**
- **Repoint the spawn action from exec → API.** `SpawnViewState` builds the lowered config, then
  `POST /api/v1/spawn` into the running instance (the `source.spawn()` path) — remove the
  `PendingSpawn`/exec path for the in-cockpit case. Extract a shared spawn-routing helper so the
  standalone `agentctl spawn` CLI **detects a live agentd** (`/healthz` or `/agents/control`) and
  routes to it, only `exec`-ing a fresh one when none is running (the p7.3-ar-02 fix).
- **Custom mode:** add a `⟨custom⟩` entry above the template picker — blank task + the **full
  capability list** as deny-by-default toggles + a **tool/connector multi-select** (grouped: native
  tools / MCP connectors; connectors arrive with the companion #5 track). Keep the existing
  "disabled caps stripped from lowered config" revoke semantics (security).
- **The one justified modal:** the spawn form is a centered modal over the live dashboard; the SSE
  producer **keeps running behind it** so nothing is missed while filling it in. `Tab`/`Shift+Tab`
  fields, `Space` toggles a cap/connector, `Esc` cancels (confirm-if-dirty).
- **Preview before launch:** show the resolved `agent.toml` (with the existing `--dry-run`
  provenance header) so the granted caps are enumerated and auditable; `Enter` on preview launches.
- **Gated caps:** show `gated_requires` warnings inline before launch (as CLI/TUI already do).
- **Post-launch:** on `201 {agent_id}`, auto-select the new row and drop into its AgentDetail (or
  Converse targeted at it) — spawn↔observe↔converse close into one loop.
- Surface spawn rejection (bad template / missing secret) **in the form**, not a vanishing toast.

**Acceptance:**
- [ ] From `watch`, pick `⟨custom⟩`, type a task, toggle caps/tools, launch → the agent appears in the running instance's table (same process; parent edge in Topology), no second agentd, no restart.
- [ ] `agentctl spawn <template>` CLI routes to a running agentd when present; execs fresh only when none is running (p7.3-ar-02).
- [ ] Deny-by-default holds; the preview enumerates exactly the granted caps; launching never grants a cap the operator didn't see.
- [ ] A rejected spawn shows the reason in the form.
- [ ] The spawn modal does not pause the background event feed (dashboard behind stays live).
- [ ] `:` command palette navigates to the existing views (`:agents`,`:topology`,`:memory`,`:spawn`,`:approvals`,`:inspect`) alongside the letter shortcuts.

---

## Test plan (per the project's non-negotiable)

Every code item ships with a test that **fails without the fix**, plus adversarial verification —
not "applied." Key tests:
- ux.0: stalled-SSE-doesn't-freeze-keys; bounded ring under 10 k events; default-bind-is-loopback assertion; host reaches `--url localhost:7999`.
- ux.2: secret-redaction in previews; error→row-red; idle→amber; stream scoping/freeze; `--plain` parity.
- ux.1: streaming-scroll follow flag; retarget-inject; inject-rejected inline (no hang); CLI REPL no-regression.
- ux.3: spawn-into-running-instance (new row, same process); CLI route-to-live vs exec-fresh; deny-by-default preview; modal doesn't pause producer.
- A `watch`-over-HTTP integration smoke against a live `:7999` (spawn → observe activity → converse → verify via SSE).

## Cathedral expansions — accepted 2026-07-10 (CEO review, SCOPE EXPANSION)

Four increments accepted into scope. They assemble the "CoS you live with": it reaches you,
proves what it did, and can be driven from a browser or rewound. All ride substrate that already
exists (management SSE, signed Ed25519 receipts p7.5, checkpoints p3.2, flight recorder).

- **ux.4 — Proactive push** (the CoS reaches you). A push sink on the `/api/v1/events` SSE stream
  for the events that need you (approval-needed, error, brief-ready, skill-to-approve) → a local
  notifier + an *optional* signed webhook to **one operator-owned endpoint** (ntfy/Pushover/phone).
  ⚠ This is a genuine new **outbound egress path** — it must route through the credential broker /
  egress gateway (cred.3) and carry a THREAT_MODEL note; the single-tenant loopback lock covers
  *inbound*, not this. Deny-by-default; the endpoint is operator-configured, never inferred.
  Endpoint URL + signing secret come from **env, not config** (secrets rule); **SSRF-guard** the
  endpoint. Delivery is **best-effort with bounded retry/backoff**; a failed push **never blocks the
  agent loop**; emit `PushDelivered`/`PushFailed` flight events (record-everything).
  Acceptance: an approval-needed event pushes to the configured endpoint; no endpoint configured =
  no egress; the push path is brokered + flight-recorded; an unreachable endpoint retries then fails
  soft (loop unaffected) with a `PushFailed` event.
- **ux.6 — Evidence view** (provable accountability). Surface the signed receipt chain
  (`evidence.jsonl`) in the cockpit + inline `agentctl verify` + a per-agent "chain verified" badge.
  Makes the governance differentiation felt. Acceptance: tampering with a receipt shows an invalid
  badge; a clean chain shows verified.
- **ux.5 — Local web cockpit**. One self-contained HTML/JS app served on **host-loopback** by the
  management server, consuming `/snapshot` + `/events` + spawn/inject — the same surface as the TUI,
  in a browser. Still single-tenant, still loopback (D2 reachability applies; never LAN).
  ⚠ A browser reaching loopback is **not** the FUSE trust boundary: any site the operator visits can
  attempt **DNS-rebinding / cross-origin** against `:7999`. Prerequisite (land *before* ux.5): an
  **Origin/Host-header allowlist** (or a per-session token) on the management API + a THREAT_MODEL note.
  Acceptance: converse/observe/spawn work in a browser against `:7999`; no second backend; a
  cross-origin / mismatched-Host request to the API is rejected.
- **ux.7 — Run replay / time-travel**. Reconstruct + scrub an agent's run from `flight.jsonl` (+
  checkpoint boundaries): step through turns and tool calls, "what it saw and decided at 8:04am."
  Acceptance: replaying a recorded run reproduces its step sequence; scrubbing is bounded (no OOM on
  a long run).

## Sequencing (CEO-locked 2026-07-10 — observability-first, spec-review-corrected)

**Core (observability-first):** `ux.0 → ux.2` (the silent-failure fix + debugging substrate), then
the live cockpit `ux.1 → ux.3`. **This path is NOT gated on connectors.** Connectors is a **parallel
track** (plan unwritten) that slots in whenever it's ready — the intended next *value* after the
observability floor, but it must not block the cockpit on undefined work.

**Expansions (cheap-high-impact first — a deliberate switch now that the observability floor is in):**
`ux.6 (evidence, in the existing TUI cockpit) → ux.4 (push) → ux.5 (web cockpit) → ux.7 (replay)`.
ux.6 and ux.4 are cheap and high-impact; ux.5 and ux.7 are heavier. ux.6 surfaces in `agentctl watch`
(the TUI), so it precedes the web cockpit (ux.5). **ux.5 requires the Origin/Host allowlist to land
first.**

**Skills subsystem (Phase 11) lands last** — skills compose tool calls, so they're worth most after
connectors exist.

One increment per branch, **across sessions (not one)**; `main` shippable at each step; `/autoplan` →
build → `/review` → `/qa` → `/ship`. `/plan-eng-review` gates: **ux.0** (async-loop refactor must
preserve behavior), **ux.4** (outbound push egress via the broker), **ux.5** (browser-reachable
management API — DNS-rebinding).

## References

- Ratatui async event stream / full async events (the `select!` + channel idiom).
- "Rewriting Claude Code in Rust, Part 3 (claux)" — four-zone chat TUI, streaming-green, redraw-per-token.
- k9s (resource table, `:` palette, contextual keybindings, follow-selection), lazygit (focus ring, inline forms), htop/btop (color-encoded state).
- LangGraph Studio / Langfuse (per-node timeline, summary-first spans).
- Internal: `agentd/src/management.rs`, `agentctl/src/orchestrate.rs`, `agentctl/src/watch/`, TODOS `cos-ux-01` / `p7.3-ar-02`.
