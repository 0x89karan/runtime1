<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260707-191655.md -->

# orch.1 — Interactive Agent Orchestrator

**Increment:** orch.1  
**Version:** v0.66.0 (tentative)  
**Status:** Active — h8.1 shipped (v0.64.0); cred.4b shipped (v0.65.0); both dependencies satisfied
**Branch:** orch.1-interactive-orchestrator (not started)  

## Goal

Make agentOS conversational. Today every agent run is one-shot (perceive → infer → act → complete → exit), and there is no way to send a follow-up without losing context. orch.1 adds the `inject` control primitive, an HTTP spawn/inject API, and `agentctl chat` — an interactive REPL that keeps an agent alive and routes follow-up messages into its running context. Closes the "no chat mode" dogfood finding from 2026-07-05.

## Context

The `inject` concept was introduced in p7.3 for the TUI Spawn view — when `/agents/control` exists, `execute_pending_spawn` writes a spawn JSON and the scheduler creates a new agent. But:

1. **No `ControlCommand::Inject`** — `control.rs` has `Spawn`, `Approve`, `Reject`. There is no way to inject a follow-up text into a *running* agent via the control surface.
2. **No `agentctl inject` subcommand** — p7.3 added the FUSE control surface but did not add an `inject` CLI subcommand (contrary to what CLAUDE.md says; only the TUI path was added).
3. **No HTTP spawn/inject routes** — `management.rs` at `:7999` has `GET /api/v1/snapshot`, approvals, memory, and SSE events, but no `POST /api/v1/spawn` or `POST /api/v1/agents/:id/inject`.
4. **`DataSource` trait is read-only for spawn** — no `spawn()` or `inject()` methods exist.
5. **No `agentctl chat` subcommand** — the user has no interactive REPL entry point.
6. **No Docker `chat` entrypoint mode** — `docker/entrypoint.sh` has `agent`, `explore`, `cos`, `shell`.

Dependencies already present:
- `task.inject_messages(messages, recorder)` — exists in `scheduler.rs` line ~2212
- `/api/v1/events` SSE fan-out — exists in `management.rs`, needed for streaming chat responses
- `ControlCommand::Spawn` + `parse_control_command` — in `control.rs`, Inject can reuse the parse pattern
- `execute_pending_spawn` with live injection via `/agents/control` — already in `watch/mod.rs`

## Acceptance Criteria

- [ ] `agentctl inject <agent-id> <text>` sends `{"inject":{"agent_id":"...","text":"..."}}` to `/agents/control` and returns 0
- [ ] `agentctl orchestrate [message] [--agent-id <id>] [--agents-dir <dir>] [--url <url>]` — auto-detect REPL:
  - Checks `GET /healthz` (or FUSE presence) to detect if agentd is running at `--url`
  - If agentd running + agent found: injects message into running agent
  - If agentd running + agent absent: spawns agent via management API, then injects
  - If agentd not running: starts local agentd, spawns agent, enters REPL
  - Streams response lines from `/api/v1/events` SSE (filtered by agent_id) until `OrchestratorTurnComplete`
  - `Ctrl+C` exits gracefully; agent continues running (checkpoints on SIGTERM)
- [ ] `POST /api/v1/spawn` (management API) dispatches `ControlCommand::Spawn` via `control_tx`; returns `{"agent_id":"..."}` or 503
- [ ] `POST /api/v1/agents/:id/inject` dispatches `ControlCommand::Inject` via `control_tx`; returns 200 or 404/503
- [ ] `ControlCommand::Inject { agent_id, text }` is parsed from `{"inject":{"agent_id":"...","text":"..."}}` and handled in the scheduler (calls `inject_messages` on the named agent)
- [ ] `docker compose run --rm cos orchestrate "find all TODOs"` prints the agent response to stdout
- [ ] Follow-up: `docker compose run --rm cos orchestrate "now fix the P1 ones"` — if agentd is already running (persistent compose service), injects into the still-running agent; if not, spawns fresh
- [ ] Four new flight events: `OrchestratorDispatched`, `OrchestratorInjected`, `OrchestratorTurnComplete`, `OrchestratorExited`
- [ ] Event kind coverage test (`otel/tests/event_kind_coverage.rs`) compiles with the 4 new variants
- [ ] All new behavior has tests that fail without the fix; 1200+ total workspace tests

## Scope

### 1. `agentd/src/control.rs` — Add `ControlCommand::Inject`

```rust
// New variant in ControlCommand:
Inject {
    agent_id: String,
    text:     String,
}

// New variant in TaggedCommand:
#[serde(rename_all = "lowercase")]
enum TaggedCommand {
    Spawn(OperatorSpawnRequest),
    Approve { ... },
    Reject { ... },
    Inject { agent_id: String, text: String },  // NEW
}
```

Wire format: `{"inject":{"agent_id":"scout-1","text":"follow up question here"}}`

Validation: `agent_id` must not be empty; `text` must not be empty.

**Tests:** parse valid inject, parse empty-agent-id → error, parse empty-text → error.

### 2. `agentd/src/scheduler.rs` — Handle `ControlCommand::Inject` + `AgentStatus::Waiting`

**⚠ Three fixes vs. original code — all required (eng review findings):**

**Fix A — Field name:** `state.tasks` does not exist. The field is `state.agents: HashMap<String, AgentTask>`.

**Fix B — Message type:** `task.inject_messages()` takes `Vec<crate::bus::MailMessage>`, not `Vec<inference::Message>`. Use `MailMessage { from: "operator".into(), content: text.clone() }`. Function returns `()`, so no `?`.

**Fix C — Waiting state is required:** Without a `Waiting` variant in `AgentStatus`, a chat-agent completes (`Done`) after the first response and exits. Follow-up injects hit "agent not found" and the chat loop is broken. Add `AgentStatus::Waiting` for agents that have responded and are parked awaiting the next inject.

**⚠ DX review C2 fix:** `AgentStatus` lives in `surfaces/src/snapshot.rs` (not `agentd/src/agent/mod.rs`). The `as_str()` match arm and `Serialize` impl must also be updated there. Files-touched list updated accordingly.

Corrected implementation:

```rust
// In AgentStatus enum (surfaces/src/snapshot.rs):
enum AgentStatus {
    Running,
    Waiting,    // NEW — parked, awaiting next inject
    Done(String),
    Failed(String),
}

// In ControlCommand::Inject handler (scheduler.rs control_rx drain, ~line 1721):
ControlCommand::Inject { agent_id, text } => {
    if let Some(task) = state.agents.get_mut(&agent_id) {  // NOTE: state.agents, not state.tasks
        let msg = crate::bus::MailMessage {
            from: "operator".to_string(),
            content: text.clone(),
        };
        task.inject_messages(vec![msg], recorder);  // NOTE: no ?, returns ()
        recorder.record(EventKind::OrchestratorInjected, json!({
            "agent_id": agent_id, "text_len": text.len()
        }));
    } else {
        recorder.record(EventKind::OrchestratorExited, json!({
            "agent_id": agent_id, "reason": "agent_not_found"
        }));
    }
}
```

**Scheduler state machine update:** When a chat-agent finishes its response turn and its context is empty (no pending tool calls), transition to `Waiting` instead of `Done`. The scheduler's main loop skips `Waiting` agents (no `step()` call) until an inject arrives that transitions them back to `Running`.

The chat-agent template (`max_turns = 200`, `checkpoint_interval_turns = 1`) controls how long an agent stays alive.

**`OrchestratorTurnComplete` event (required by DX finding):** When the agent transitions to `Waiting`, emit this event so `agentctl chat` knows the current response turn is done and the REPL can re-prompt the user. Without this signal, the SSE stream has no reliable "ready for next input" marker.

Add to `flight_recorder.rs`:
```rust
OrchestratorTurnComplete,   // agent finished response turn, now Waiting
```

**Inject text size limit (security finding):**
```rust
// In parse_control_command (control.rs):
anyhow::ensure!(text.len() <= 65_536, "inject text too large (max 64 KiB)");
```

**Tests:** inject into running agent pushes a User turn; inject into unknown agent_id is a noop (no panic); chat-agent transitions to Waiting after turn; inject on Waiting agent transitions back to Running; OrchestratorTurnComplete fires on transition to Waiting.

### 3. `agentd/src/management.rs` — Add spawn + inject HTTP routes

Two new routes on the loopback HTTP server:

**`POST /api/v1/spawn`**
- Body: `{"task":"...","id":"...","capabilities":[...],"max_turns":N}` (same as `OperatorSpawnRequest`)
- Dispatches `ControlCommand::Spawn(req)` via `state.control_tx.as_ref()`
- Returns: `200 {"agent_id":"...", "status":"dispatched"}` or `503 {"error":"control channel unavailable"}`
- Returns: `503 + Retry-After: 1` on full channel (matches approval pattern)

**`POST /api/v1/agents/:id/inject`**
- Body: `{"text":"..."}`
- Dispatches `ControlCommand::Inject { agent_id: id, text }` via `state.control_tx`
- Returns: `200 {}` or `404 {"error":"agent id empty"}` or `503 {"error":"control channel unavailable"}`

Both routes require `management.enabled = true` in TOML (same as current management API).

Flight events: `ManagementRequest` already covers the HTTP layer; `OrchestratorDispatched` emitted on successful spawn dispatch; `OrchestratorInjected` emitted in scheduler (not HTTP handler).

**Tests:** spawn route dispatches Spawn command; spawn route 503 when control_tx absent; inject route dispatches Inject command; inject route 400 on empty text; inject route 503 when control_tx absent.

### 4. `agentctl/src/watch/source.rs` — Extend `DataSource` trait

```rust
pub trait DataSource: Send + Sync {
    fn load_snapshot(&self) -> anyhow::Result<Snapshot>;
    fn load_approvals(&self) -> anyhow::Result<Vec<ApprovalEntry>>;
    fn approve(&self, id: &str, ...) -> anyhow::Result<()>;
    fn deny(&self, id: &str, ...) -> anyhow::Result<()>;
    // NEW:
    fn spawn(&self, req: &SpawnRequest) -> anyhow::Result<String>;  // returns agent_id
    fn inject(&self, agent_id: &str, text: &str) -> anyhow::Result<()>;
}
```

`SpawnRequest`: `{ task: String, id: Option<String>, capabilities: Vec<Capability> }`.

`FuseSource::spawn`: writes `{"spawn":{...}}` to `/agents/control` via `write_control_command`; returns the id from the request or a generated one.

`FuseSource::inject`: writes `{"inject":{"agent_id":"...","text":"..."}}` to `/agents/control`.

`HttpSource::spawn`: `POST /api/v1/spawn` with `mutation_client`.

`HttpSource::inject`: `POST /api/v1/agents/:id/inject` with `mutation_client`.

**Tests:** FuseSource inject writes correct JSON; HttpSource inject calls correct URL.

### 5. `agentctl/src/inject.rs` (new file) — `agentctl inject` CLI subcommand

```
agentctl inject <agent-id> <text> [--agents-dir <dir>] [--url <url>]
```

- Auto-detects source (FUSE vs HTTP) via `detect_source()`
- Calls `source.inject(agent_id, text)`
- Prints `injected into agent '<id>'` on success
- Exits 1 on error

Wires into `agentctl/src/main.rs` as `Commands::Inject(inject::Args)`.

**Tests:** inject command integration test with mock FUSE path.

### 6. `agentctl/src/orchestrate.rs` (new file) — `agentctl orchestrate` subcommand

```
agentctl orchestrate [message] [--template <name>] [--agent-id <id>] [--agents-dir <dir>] [--url <url>]
```

**Modes:**
- **Single-shot:** `agentctl orchestrate "find all TODOs"` — auto-detects agentd, spawns agent (or injects into running one), waits for `OrchestratorTurnComplete`, prints response, exits.
- **Interactive REPL:** `agentctl orchestrate` (no message) — prints `> ` prompt, reads from stdin, inject/spawn loop.

**Algorithm:**
1. Check `GET /healthz` at `--url` (default `http://127.0.0.1:7999`) to detect if agentd is up
2. If not up: start local agentd subprocess (requires `agentd` binary on PATH); register it for cleanup on Ctrl+C
3. Determine agent ID: `--agent-id` flag, else `$AGENTCTL_ORCH_AGENT`, else `"orch-default"`
4. Check if agent is running by calling `source.load_snapshot()` → look for `agent_id` in active agents
5. If not running: `source.spawn(SpawnRequest { task: message, id: Some(agent_id), ... })` + record as `OrchestratorDispatched`
6. If running: `source.inject(agent_id, message)` + record as `OrchestratorInjected`
7. Print `thinking...` to stderr; subscribe to `/api/v1/events` (SSE), filter by `agent_id`; listen for `OrchestratorTurnComplete` as the "ready for next input" signal
8. In REPL mode: print `> ` prompt; loop; exit on Ctrl+C / `quit` / `exit`

**Streaming output:** Use `reqwest` SSE stream on `/api/v1/events`. Listen for `OrchestratorTurnComplete` — `perceive` only fires on turn 0; `agent_completed` fires on agent exit. For `FuseSource` with no `--url`: return `Err("orchestrate requires management API; run with --url http://127.0.0.1:7999")`.

**`--plain` flag:** disable ANSI color, useful for piping.

**`trailing_var_arg = true`** on `orchestrate [message]` and `inject <id> <text>` — allows natural `agentctl orchestrate find all TODOs` without quotes.

**`"$@"` not `"$*"`** in Docker entrypoint for correct arg passing with spaces.

**Print agent ID to stderr after spawn:** `agent: orch-default` — allows `agentctl inject orch-default "..."` from a second terminal.

**Tests:** orchestrate single-shot dispatches spawn when agent absent; orchestrate reuses running agent via inject; `quit` exits REPL loop; orchestrate exits with error when FUSE-only (no --url); auto-detect healthz check triggers spawn when agentd down.

### 7. `docker/entrypoint.sh` — `orchestrate` mode

```bash
orchestrate)
    # Ensure management API is enabled (required for SSE streaming)
    export AGENTD_MANAGEMENT_ENABLED=true
    export AGENTD_MANAGEMENT_PORT=7999

    # Auto-detect: if agentd already running (persistent compose service), inject into it
    if curl -sf http://127.0.0.1:7999/healthz >/dev/null 2>&1; then
        # agentd is up — agentctl orchestrate will inject or spawn-via-API
        agentctl orchestrate --url http://127.0.0.1:7999 "$@"
    else
        # Cold start — launch agentd ourselves
        agentd /etc/agentd/agents.toml &
        AGENTD_PID=$!
        timeout 10 sh -c 'until curl -sf http://127.0.0.1:7999/healthz >/dev/null 2>&1; do sleep 0.5; done'
        agentctl orchestrate --url http://127.0.0.1:7999 "$@"
        # Agent stays running; agentd stays alive for follow-up injects
        wait $AGENTD_PID
    fi
    ;;
```

**Auto-detect behaviour:**
- `docker compose up -d agent` → agentd running → `docker compose run cos orchestrate "..."` detects healthz, injects (cross-session context preserved)
- `docker compose run cos orchestrate "..."` cold → starts agentd → after response, agentd stays alive inside container for follow-up `docker compose run`

Requires `management.enabled = true` in base `agents.toml` (satisfied by `AGENTD_MANAGEMENT_ENABLED=true` env override).

**Tests:** entrypoint.sh `orchestrate` mode self-test (mock agentd, verify agentctl is invoked); auto-detect path (healthz up → no agentd spawn).

### 8. `agentd/src/flight_recorder.rs` — Four new `EventKind` variants

```rust
OrchestratorDispatched,     // operator sent a spawn via orch surface
OrchestratorInjected,       // operator sent an inject into a running agent
OrchestratorTurnComplete,   // agent finished response turn, now Waiting (NEW — required for SSE signal)
OrchestratorExited,         // inject target not found / chat session ended
```

**Note on `OrchestratorExited` semantics:** Overloaded for both "inject miss" and "session ended." A `reason` field distinguishes the two in the payload. Future: split into `OrchestratorInjectMiss` + `OrchestratorSessionEnded` if OTEL mapping becomes painful.

Update `otel/tests/event_kind_coverage.rs` to add all four variants.

### 9. `templates/orchestrator.template.toml` (new) — Long-lived orchestrator agent

A template for agents designed to be used with `agentctl orchestrate`. Key differences from other templates:
- `max_turns = 200` (high; chat sessions can be long)
- `checkpoint_interval_turns = 1` (checkpoint after every turn for follow-up resilience)
- No `gated_requires`
- `capabilities` designed for general-purpose chat (kb_read, kb_write for memory)

### 10. `agentd/src/config.rs` — `ManagementConfig` env override

Add `AGENTD_MANAGEMENT_ENABLED` env var override so `docker/entrypoint.sh chat` can enable the management API without modifying the TOML:

```rust
// In ManagementConfig::from_env_override():
if std::env::var("AGENTD_MANAGEMENT_ENABLED").as_deref() == Ok("true") {
    self.enabled = true;
}
```

## Out of Scope

- Automatic template selection based on query text (NLP routing) — Phase orch.2
- Multi-agent chat routing (user message dispatched to multiple specialists) — Phase orch.2
- Persistent chat history across process restarts without KB (current checkpoint + KB covers this)
- Web UI for chat — not before the CLI is solid
- Streaming inference tokens character-by-character to chat output — p7.2 already streams; the SSE events carry turn-level content, not token-level; token streaming to CLI is a later UX improvement

## Security Considerations

- `agentctl chat` auto-detect uses `detect_source()` which only binds to loopback — no remote execution surface
- `POST /api/v1/spawn` and `/api/v1/agents/:id/inject` are loopback-only (same as the rest of management API at `:7999`)
- `ControlCommand::Inject` validates non-empty `agent_id` and `text` before dispatching
- `text` in inject is treated as user input, not a tool call — the same trust level as existing `inject_messages` (already used by p7.3 for operator injection)
- Docker `chat` mode runs as the same user as `agentd` — no privilege escalation

## Dependencies

- **p7.7** (management HTTP API) — required for HTTP spawn/inject routes and SSE streaming
- **p7.3** (FUSE control surface) — required for FUSE spawn/inject path
- **cred.3** (credential broker) — already shipped; MCP servers are credential-agnostic, chat mode inherits this

## Risks

1. **`inject_messages` into a completed agent** — if the agent finishes between the check and the inject, the message is silently dropped (no running task to inject into). Mitigation: the scheduler logs `OrchestratorExited` and agentctl prints a warning; user must spawn a new agent.
2. **SSE streaming cut-off** — `agentctl chat` needs to know when the agent's *current turn* is done vs. the agent having exited. The SSE stream carries `perceive` events (agent waiting) and `agent_completed` events. If the user terminates the connection early, the agent keeps running.
3. **Docker `chat` mode waits for healthz** — if management API fails to start, the `timeout 10` guard aborts with a useful error message.

## Test Plan

| # | What | How |
|---|------|-----|
| T1 | `parse_inject` bare | unit test in `control.rs` |
| T2 | `parse_inject` empty agent_id → error | unit test |
| T3 | `parse_inject` empty text → error | unit test |
| T3b | `parse_inject` text > 64 KiB → error | unit test |
| T4 | Scheduler inject → `MailMessage` pushed (not inference::Message) | unit test in `scheduler.rs` |
| T5 | Scheduler inject unknown agent_id → noop + OrchestratorExited | unit test |
| T5b | Chat-agent transitions to Waiting after turn; inject transitions back to Running | unit test |
| T5c | OrchestratorTurnComplete fires on Waiting transition | unit test |
| T6 | `POST /api/v1/spawn` dispatches Spawn | mgmt test |
| T7 | `POST /api/v1/spawn` 503 when no control_tx | mgmt test |
| T8 | `POST /api/v1/agents/:id/inject` dispatches Inject | mgmt test |
| T9 | `POST /api/v1/agents/:id/inject` 400 on empty text | mgmt test |
| T10 | `FuseSource::inject` writes correct JSON | agentctl unit test |
| T11 | `HttpSource::inject` calls correct URL | agentctl unit test |
| T12 | `agentctl inject` CLI dispatches inject | integration test |
| T13 | `agentctl chat` spawns when agent absent | unit test |
| T14 | `agentctl chat` injects when agent present | unit test |
| T15 | `AGENTD_MANAGEMENT_ENABLED` env override | config unit test |
| T16 | Event kind coverage (4 new variants incl. TurnComplete) | compile-time test |
| T17 | Chat-agent does not self-complete after first response — remains injectable | unit test |
| T18 | Inject into completed (terminal) agent → noop + OrchestratorExited | unit test |
| T19 | `agentctl chat` exits with error when FUSE-only (no --url) | unit test |
| T20 | FuseSource::spawn ID collision race → documented behavior | unit test |

## Estimated Files Changed

| File | Change |
|------|--------|
| `agentd/src/control.rs` | +Inject variant, parser, 64KiB size limit, 4 tests (T1–T3b) |
| `surfaces/src/snapshot.rs` | +AgentStatus::Waiting variant + as_str() arm + Serialize update |
| `agentd/src/scheduler.rs` | +Inject handler (MailMessage), +Waiting state transitions, +OrchestratorTurnComplete |
| `agentd/src/management.rs` | +2 HTTP routes, T6–T9 tests |
| `agentd/src/config.rs` | +ManagementConfig env override |
| `agentd/src/flight_recorder.rs` | +4 EventKind variants (incl. TurnComplete) |
| `agentctl/src/watch/source.rs` | +spawn/inject on DataSource trait with default impls |
| `agentctl/src/inject.rs` | new file — inject subcommand |
| `agentctl/src/orchestrate.rs` | new file — orchestrate subcommand (auto-detect, SSE filtered by agent_id + TurnComplete signal) |
| `agentctl/src/main.rs` | +Inject, +Orchestrate commands, +trailing_var_arg |
| `agentctl/Cargo.toml` | +stream feature to reqwest (blocking BufReader SSE) |
| `docker/entrypoint.sh` | +chat) case, "$@" not "$*" |
| `templates/orchestrator.template.toml` | new file |
| `otel/tests/event_kind_coverage.rs` | +4 new EventKind arms |
| `docs/ROADMAP.md` | check off orch.1 |

## Autoplan Outcome (2026-07-07, updated 2026-07-07)

**Decision: UNBLOCKED — h8.1 shipped (v0.64.0), cred.4b shipped (v0.65.0). Ready to implement.**

Prior rationale for deferral: build h8.1 first to stabilize the KB substrate before adding the parked-state scheduler. Both are now done. D1/D2/D3 remain the resolved decisions.

**Resolved decisions:**
- **D1:** Require `HttpSource` for `agentctl chat` — no FUSE polling. Management API (`management.enabled = true`) is a prerequisite for chat mode.
- **D2:** Parked state (full context window + KB). Scheduler gains a `Waiting` state for agents that have responded and are awaiting the next inject.
- **D3:** `AGENTD_MANAGEMENT_ENABLED=true` env var override in `docker/entrypoint.sh chat` — no TOML change, no default flip.

**Eng blockers to fix before implementation begins:**
1. Scheduler inject handler: use `MailMessage` (not `inference::Message`); push to mailbox rather than calling `inject_messages` directly; remove `?` (returns `()`).
2. `DataSource::spawn`/`inject`: add default impls returning `Err("not supported")` so `TestSource` compiles without changes.
3. `agentctl/Cargo.toml`: add `stream` feature to `reqwest`; use blocking BufReader SSE (no tokio needed).

**DX fixes to apply:**
- `trailing_var_arg = true` on `agentctl chat [message]` and `agentctl inject <id> <text>`.
- Docker entrypoint: `"$@"` not `"$*"` for chat args.
- SSE stream: filter events by `agent_id`.

**New tests to add:**
- T17: Chat-agent does not self-complete after first response — remains injectable (core invariant).
- T18: Inject into completed (terminal) agent → noop + `OrchestratorExited` event.

## Eng Review (Phase 3 — /autoplan)

### Architecture ASCII Diagram

```
agentd process                         agentctl / Docker
─────────────────────────────────────  ──────────────────────────────
                                        
  FlightRecorder                         agentctl chat
  ├─ broadcast_tx ─────────────────────► SSE reader (filtered by agent_id)
  │                                      └─ waits for OrchestratorTurnComplete
  │                                         then re-prompts user
  ├─ flight.jsonl (agentos-otel)
  │
  Scheduler (tokio select! loop)
  ├─ control_rx ◄──────── FUSE /agents/control  ◄── FuseSource::inject()
  │              ◄──────── HTTP POST /api/v1/      ◄── HttpSource::inject()
  │                        agents/:id/inject
  │
  ├─ ControlCommand::Inject { agent_id, text }
  │    └─ state.agents.get_mut(agent_id)
  │         ├─ found: MailMessage push → Running
  │         └─ not found: OrchestratorExited event
  │
  AgentTask (per agent)
  ├─ AgentStatus: Running | Waiting | Done | Failed
  │    Waiting: agent has responded, parked awaiting inject
  │    inject arrives → Running
  │    turn complete → Waiting (emits OrchestratorTurnComplete)
  │
  ├─ context deque (max_turns = 200 for chat-agent)
  └─ checkpoint_interval_turns = 1
```

### What Already Exists (Eng)

Same as CEO section + `AgentStatus` enum in `surfaces/src/snapshot.rs` (needs `Waiting` variant + `as_str()` arm + Serialize update — DX C2 fix).

### NOT in Scope (Eng)

Same as CEO; additionally: no Waiting→Done auto-expiry timer (future orch.2), no concurrent inject serialization (single-threaded control_rx drain prevents races).

### Code Quality Findings

| Finding | Severity | Decision | Principle |
|---|---|---|---|
| `state.tasks` → `state.agents` | CRITICAL | Fixed in plan | P1 |
| inference::Message → MailMessage + remove ? | CRITICAL | Fixed in plan | P1 |
| Missing AgentStatus::Waiting state machine | CRITICAL | Fixed in plan | P1 |
| SSE turn-done signal relies on perceive/agent_completed | HIGH | Fixed: OrchestratorTurnComplete added | P1 |
| Inject text unbounded | HIGH | Fixed: 64 KiB limit added to parser | P1 |
| FuseSource spawn ID collision race | MEDIUM | Documented + T20 test | P5 |
| OrchestratorExited semantically overloaded | LOW | Added `reason` field; doc note | P5 |

### Test Diagram

```
New codepaths               Test type         Coverage
──────────────────────────  ────────────────  ────────
parse_inject valid          unit (control.rs) T1
parse_inject empty id       unit              T2
parse_inject empty text     unit              T3
parse_inject >64KiB         unit              T3b
inject→MailMessage push     unit (scheduler)  T4
inject unknown agent_id     unit              T5
Waiting→Running transition  unit              T5b
OrchestratorTurnComplete    unit              T5c
POST /api/v1/spawn          mgmt test         T6
spawn 503 no control_tx     mgmt test         T7
POST /api/v1/inject         mgmt test         T8
inject 400 empty text       mgmt test         T9
FuseSource inject JSON      agentctl unit     T10
HttpSource inject URL       agentctl unit     T11
agentctl inject CLI         integration       T12
chat spawns absent agent    unit (chat.rs)    T13
chat injects running agent  unit              T14
MANAGEMENT_ENABLED env      config unit       T15
EventKind coverage (4)      compile-time      T16
chat-agent stays injectable unit              T17
inject terminal agent noop  unit              T18
chat FUSE-only error        unit              T19
spawn ID collision          unit              T20
```

### Completion Summary (Eng)

| Item | Status |
|---|---|
| Scope challenge | All 15 files in blast radius; complexity check triggers (15 files) but all are required |
| Architecture | Sound after 3 critical fixes |
| Tests | 20 tests (T1-T20) — adequate for 15-file increment |
| Performance | No N+1, no unbounded allocations (64KiB cap) |
| Security | Loopback-only, 64KiB cap, non-empty validation |
| Deployment risk | Low — additive only, no breaking changes |

---

## DX Review (Phase 3.5 — /autoplan)

### DX Scorecard

| Dimension | Score | Key finding |
|---|---|---|
| TTHW | 5/10 | `management.enabled = true` is a silent prerequisite — not in any template or starter config |
| Error messages | 5/10 | Connection refused vs. management disabled are indistinguishable; inject-miss is silent |
| CLI ergonomics | 6/10 | `trailing_var_arg` addressed; agent ID not printed on spawn; empty-input unspecified |
| Documentation gap | 4/10 | `agentd/README.md` not in files-touched; `AGENTD_MANAGEMENT_ENABLED` undocumented |
| Default behavior | 6/10 | `false` default is correct security-wise; no actionable error bridges the gap |
| Docker DX | 4/10 | `kill $AGENTD_PID` contradicts "inject into still-running agent" AC#7 |
| Observability | 4/10 | 10–30 s silent wait during inference; no spinner or thinking indicator in plan |
| Breaking changes | 7/10 | `AgentStatus::Waiting` was in wrong file (now fixed); `TestSource` default-impl gap noted |

### TTHW Assessment

Steps from zero to first working `agentctl chat`:
1. `export ANTHROPIC_API_KEY=sk-...`
2. Add `[management]\nenabled = true` to `agent.toml` **(invisible prerequisite — no template ships with this)**
3. `cargo run --bin agentd -- agent.toml &`
4. Wait for `:7999` to be ready (no readiness indicator)
5. `agentctl chat --url http://127.0.0.1:7999 "hello"`

Gap: User hits FuseSource error → tries `--url` → gets "connection refused" because management not enabled. Two different errors, same root cause. Fix: connection refused error MUST say "is `[management] enabled = true` in your TOML?"

### Critical DX Fixes Applied

| Finding | Action |
|---|---|
| C1 — Docker lifecycle contradiction (`kill` vs "inject into still-running agent") | **Taste decision surfaced at Final Gate** |
| C2 — `AgentStatus::Waiting` in wrong file (`agent/mod.rs` → `surfaces/src/snapshot.rs`) | **Fixed in plan — Eng fix applied** |
| C3 — Inject miss silent to operator | Added T18 test; M5 note for future orch.2 |

### DX Implementation Checklist

- [x] Fix `AgentStatus::Waiting` target file → `surfaces/src/snapshot.rs` (C2, done)
- [x] Fix `"$*"` → `"$@"` in entrypoint.sh code (M4, done)
- [ ] Add default impls `spawn()`/`inject()` returning `Err("not supported")` on `DataSource` — prevents `TestSource` compile error
- [ ] Add `agentd/README.md` to files-touched; document `[management] enabled = true` as chat prerequisite
- [ ] Document `AGENTD_MANAGEMENT_ENABLED` env var in `--help` output and README
- [ ] Connection-refused error must say: "is agentd running with `[management] enabled = true`?"
- [ ] Print agent ID to stderr after spawn: `agent: <id>`
- [ ] Add static `thinking...` line while waiting for `OrchestratorTurnComplete`
- [ ] Specify empty-input REPL behavior: re-prompt, no inject
- [ ] Resolve Docker lifecycle: either persistent `agentd` compose service OR remove "inject into still-running" from Docker AC (gate decision)
- [ ] Verify `reqwest stream` feature is actually needed for blocking SSE; drop if not

### Developer Journey Map

```
$ export ANTHROPIC_API_KEY=sk-...
$ cat >> agent.toml << EOF
[management]
enabled = true
EOF
$ cargo run --bin agentd -- agent.toml &
[agentd] management API listening on 127.0.0.1:7999
$ agentctl orchestrate --url http://127.0.0.1:7999 "find all TODOs"
agent: orch-default
thinking...
[orch-default] There are 12 TODO items across 4 files: ...
> find the P1 ones
thinking...
[orch-default] Three P1 items: ...
> ^C
$ # agent continues running; agentd still alive
$ agentctl inject orch-default "now fix them"   # from another terminal
```

---

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| 1 | CEO | Implementation approach: inject + HTTP routes + agentctl chat | Mechanical | P1, P5 | Complete, reuses all existing infra; alternatives (webhook, SSH) are reinvention | Webhook back-channel (B), SSH-style (C) |
| 2 | CEO | Mode: SELECTIVE EXPANSION | Mechanical | P5 | Pure additive; no existing behavior changes | — |
| 3 | CEO | Scope: keep `chat` naming (not rename to "orchestration surface") | Taste → presented at gate | P6 | User named it deliberately; dogfood-driven; both framings valid | Rename to "operator orchestration surface" |
| 4 | CEO | Scope: add parked-state memory cost analysis to plan | Mechanical | P1 | High finding from subagent; completeness requires it | — |
| 5 | CEO | SSE turn-done signal: add explicit sequence-number event to plan | Mechanical | P1 | High finding from subagent; fragile ad-hoc protocol needs design | — |
| 6 | CEO | Docker: clarify ephemeral vs persistent agent lifecycle | Mechanical | P5 | Medium finding; acceptance criteria contradicts implementation | — |
| 7 | Gate | Naming: `agentctl orchestrate` (not `chat` or `converse`) | User decision | — | Consistent with OrchestratorTurnComplete events and orch.x roadmap | chat (A), converse (B½) |
| 8 | Gate | Docker lifetime: auto-detect (healthz → inject or cold-start) | User decision | — | Zero setup; cross-session context when agentd persists; matches AC#7 | kill-on-exit (1), persistent-only (2) |

---

## CEO Review (Phase 1 — /autoplan)

### Premises

1. **"Agents are one-shot today."** TRUE — `AgentTask` completes and exits; no parking mechanism exists.
2. **"The inject primitive is the right model for conversational follow-up."** MOSTLY TRUE — reuses existing infra; Claude subagent flags framing as "CLI chatbot" vs "OS orchestration surface" (see gate).
3. **"HTTP + SSE is required for agentctl chat."** TRUE — FUSE has no native notification.
4. **"Docker chat mode closes the primary dogfood gap."** TRUE — verified by 2026-07-05 finding.

### What Already Exists (CEO)

| Sub-problem | Existing code |
|---|---|
| Inject message into running agent | `task.inject_messages()` — `scheduler.rs:~2212` |
| Control message parsing | `parse_control_command()` in `control.rs` |
| HTTP server + Axum routing | `management.rs` `:7999` |
| SSE event streaming | `/api/v1/events` fan-out — `FlightRecorder.broadcast_tx` |
| Source detection (FUSE vs HTTP) | `detect_source()` — `agentctl/src/watch/source.rs` |
| Approval mutation pattern (503 + Retry-After) | `HttpSource.mutation_client` — direct reuse |

### NOT in Scope (CEO)

- Automatic template NLP routing (orch.2)
- Multi-agent routing (orch.2)
- Web UI for chat
- Streaming inference tokens character-by-character
- Persistent sessions across process restarts without KB

### Error & Rescue Registry

| Error | Where | Rescue |
|---|---|---|
| Inject into completed agent (race) | Scheduler drain loop | Log `OrchestratorExited`; agentctl prints warning |
| SSE stream disconnection | `agentctl chat` SSE reader | Reconnect on 5xx; surface error on client exit |
| control_tx channel full | Spawn/inject HTTP routes | Return 503 + Retry-After: 1 |
| agentd not running in Docker chat | `entrypoint.sh chat` | `timeout 10 healthz` guard → actionable error |
| Agent_id not found on inject | Scheduler | Log event; no panic |

### Failure Modes Registry

| Mode | Severity | Mitigation |
|---|---|---|
| Parked agent OOMs context window | HIGH | `max_turns` limit; checkpoint after every turn |
| Turn-done signal race (SSE event ordering) | HIGH | Need explicit sequence-number in event (D5 in audit trail) |
| Docker `kill $AGENTD_PID` kills agent on chat exit | MEDIUM | Clarify ephemeral vs persistent model (D6) |
| `child_seq` collision on auto-ID (p7.3-ar-01) | LOW | Existing open TODO; doesn't block orch.1 |

### CEO Completion Summary

| Item | Status |
|---|---|
| Premises | Accepted (with framing question at gate) |
| Implementation approach | A (inject + HTTP + agentctl chat) |
| Mode | SELECTIVE EXPANSION |
| Scope | In blast radius; all 14 files justified |
| Competitive risk | N/A for single-tenant local OS; API multi-turn is different product |
| 6-month trajectory | Sound IF parked-state memory cost is analyzed and turn-done signal is explicit |

---

## Open Decisions

These need resolution before or during implementation:

1. **D1 — `agentctl chat` streaming mechanism (RESOLVED):** Require `HttpSource` for chat. `FuseSource` returns `Err("chat requires management API; run with --url http://127.0.0.1:7999")`.

2. **D2 — Parked state (RESOLVED):** `AgentStatus::Waiting` in `surfaces/src/snapshot.rs`. Agents park after turn, preserved context, await next inject.

3. **D3 — `ManagementConfig::enabled` default for Docker chat mode (RESOLVED):** `AGENTD_MANAGEMENT_ENABLED=true` env var override in `config.rs`. No TOML change, no default flip.

4. **D4 — Docker lifetime (OPEN — gate decision):** Does `docker compose run --rm agent chat "follow up"` inject into a still-running agent, or spawn a fresh one? Current plan kills agentd after chat exits (contradicts AC#7). See Final Gate.

---

## GSTACK REVIEW REPORT

| Phase | Reviewer | Score | Critical Findings | Status |
|---|---|---|---|---|
| CEO | Claude subagent | 7.5/10 | 1 taste (naming/framing), 1 medium (Docker lifetime) | Complete |
| Eng | Claude subagent + 3-fix consensus | 6.5/10 | 3 compile errors fixed (state.agents, MailMessage, Waiting file) + 1 HIGH (TurnComplete signal) | Complete |
| DX | Claude subagent | 5.5/10 | C1 Docker contradiction, C2 wrong file (fixed), C3 inject-miss silent | Complete |
| **Overall** | | **6.5/10** | **1 gate decision (Docker lifetime) + DX checklist (8 items)** | **Ready for Final Gate** |

### Auto-decided (mechanical)
- Implementation approach: A (inject + HTTP + agentctl) — P1, P5
- Mode: SELECTIVE EXPANSION — P5
- SSE signal: `OrchestratorTurnComplete` (new 4th event) — P1
- Inject size limit: 64 KiB — P1
- trailing_var_arg: enabled — P5
- `"$@"` not `"$*"` in Docker entrypoint — P5
- AgentStatus::Waiting file: `surfaces/src/snapshot.rs` — P1
- DataSource default impls: returns `Err("not supported")` — P1

### Final Gate Decisions (locked by user)
- **Naming:** `agentctl orchestrate` — consistent with `OrchestratorTurnComplete` events and orch.x roadmap series
- **Docker lifetime:** Auto-detect — `curl -sf http://127.0.0.1:7999/healthz` → inject if up, cold-start if not; agentd stays alive after exit for follow-up `docker compose run` calls
