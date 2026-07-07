<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260707.md -->

# orch.1 — Interactive Agent Orchestrator

**Increment:** orch.1  
**Version:** v0.64.0 (tentative)  
**Status:** Deferred — pending h8.1 (semantic memory layer)
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
- [ ] `agentctl chat [--template <name>] [--agents-dir <dir>] [--url <url>]` — interactive REPL:
  - If no running agent with the given ID: spawns one (via control surface or HTTP)
  - If agent is running: injects follow-up message
  - Streams response lines from `/api/v1/events` SSE (agent response turns) to stdout
  - `Ctrl+C` exits gracefully; the agent continues running (checkpoints on SIGTERM)
- [ ] `POST /api/v1/spawn` (management API) dispatches `ControlCommand::Spawn` via `control_tx`; returns `{"agent_id":"..."}` or 503
- [ ] `POST /api/v1/agents/:id/inject` dispatches `ControlCommand::Inject` via `control_tx`; returns 200 or 404/503
- [ ] `ControlCommand::Inject { agent_id, text }` is parsed from `{"inject":{"agent_id":"...","text":"..."}}` and handled in the scheduler (calls `inject_messages` on the named agent)
- [ ] `docker compose run --rm agent chat "find all TODOs"` prints the agent response to stdout
- [ ] Follow-up: `docker compose run --rm agent chat "now fix the P1 ones"` injects into the still-running agent (via HTTP if agentd is running, else spawns fresh)
- [ ] Three new flight events: `OrchestratorDispatched`, `OrchestratorInjected`, `OrchestratorExited`
- [ ] Event kind coverage test (`otel/tests/event_kind_coverage.rs`) compiles with the 3 new variants
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

### 2. `agentd/src/scheduler.rs` — Handle `ControlCommand::Inject`

In the `control_rx` drain loop (around line 1721):

```rust
ControlCommand::Inject { agent_id, text } => {
    // Find the agent by ID and inject a User message
    if let Some(task) = state.tasks.get_mut(&agent_id) {
        let msg = crate::inference::Message {
            role: crate::inference::Role::User,
            content: vec![crate::inference::ContentBlock::Text(text.clone())],
        };
        task.inject_messages(vec![msg], recorder)?;
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

**Tests:** inject into running agent pushes a User turn; inject into unknown agent_id is a noop (no panic).

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

### 6. `agentctl/src/chat.rs` (new file) — `agentctl chat` subcommand

```
agentctl chat [message] [--template <name>] [--agent-id <id>] [--agents-dir <dir>] [--url <url>]
```

**Modes:**
- **Single-shot:** `agentctl chat "find all TODOs"` — spawns agent (or reuses running one), waits for completion events, prints response, exits.
- **Interactive REPL:** `agentctl chat` (no message) — prints `> ` prompt, reads from stdin, inject/spawn loop.

**Algorithm:**
1. Auto-detect source via `detect_source()`
2. Determine agent ID: `--agent-id` flag, else `$AGENTCTL_CHAT_AGENT`, else `"chat-default"`
3. Check if agent is running by calling `source.load_snapshot()` → look for `agent_id` in active agents
4. If not running: `source.spawn(SpawnRequest { task: message, id: Some(agent_id), ... })` + record as `OrchestratorDispatched`
5. If running: `source.inject(agent_id, message)` + record as `OrchestratorInjected`
6. Subscribe to `/api/v1/events` (SSE) and print `assistant` turn `text` content to stdout until the agent's next `agent_completed` or `perceive` event (indicating it's waiting for next input)
7. In REPL mode: loop back to step 2 after printing response; exit on Ctrl+C / `quit` / `exit`

**Streaming output:** For `HttpSource`, use `reqwest` SSE stream on `/api/v1/events`. For `FuseSource` (no SSE), fall back to polling the agent's `flight` virtual file.

**`--plain` flag:** disable ANSI color, useful for piping.

**Tests:** chat single-shot dispatches spawn when agent absent; chat reuses running agent via inject; `quit` exits REPL loop.

### 7. `docker/entrypoint.sh` — `chat` mode

```bash
chat)
    # Ensure management API is enabled (required for SSE streaming)
    export AGENTD_MANAGEMENT_ENABLED=true
    export AGENTD_MANAGEMENT_PORT=7999
    
    # Start agentd in background with the coordinator template
    agentd /etc/agentd/agents.toml &
    AGENTD_PID=$!
    
    # Wait for management API to be ready
    timeout 10 sh -c 'until curl -sf http://127.0.0.1:7999/healthz >/dev/null 2>&1; do sleep 0.5; done'
    
    # If a message was provided as args, send it and exit (single-shot)
    if [ $# -gt 0 ]; then
        agentctl chat --url http://127.0.0.1:7999 "$*"
        kill $AGENTD_PID
        wait $AGENTD_PID 2>/dev/null
    else
        # Interactive REPL
        agentctl chat --url http://127.0.0.1:7999
        kill $AGENTD_PID
        wait $AGENTD_PID 2>/dev/null
    fi
    ;;
```

Requires `management.enabled = true` in the base `agents.toml` (or env var override — add `AGENTD_MANAGEMENT_ENABLED` env override to `ManagementConfig` parsing in `config.rs`).

**Tests:** entrypoint.sh `chat` mode self-test (mock agentd, verify agentctl is invoked).

### 8. `agentd/src/flight_recorder.rs` — Three new `EventKind` variants

```rust
OrchestratorDispatched,   // operator sent a spawn via orch surface
OrchestratorInjected,     // operator sent an inject into a running agent
OrchestratorExited,       // inject target not found / chat session ended
```

Update `otel/tests/event_kind_coverage.rs` to add all three variants.

### 9. `templates/chat-agent.template.toml` (new) — Long-lived chat coordinator

A template for agents designed to be used with `agentctl chat`. Key differences from other templates:
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
| T4 | Scheduler inject → `inject_messages` called | unit test in `scheduler.rs` |
| T5 | Scheduler inject unknown agent_id → noop | unit test |
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
| T16 | Event kind coverage (3 new variants) | compile-time test |

## Estimated Files Changed

| File | Change |
|------|--------|
| `agentd/src/control.rs` | +Inject variant, parser, 3 tests |
| `agentd/src/scheduler.rs` | +Inject handler in control_rx drain |
| `agentd/src/management.rs` | +2 HTTP routes, T6–T9 tests |
| `agentd/src/config.rs` | +ManagementConfig env override |
| `agentd/src/flight_recorder.rs` | +3 EventKind variants |
| `agentctl/src/watch/source.rs` | +spawn/inject on DataSource trait |
| `agentctl/src/inject.rs` | new file — inject subcommand |
| `agentctl/src/chat.rs` | new file — chat subcommand |
| `agentctl/src/main.rs` | +Inject, +Chat commands |
| `agentctl/Cargo.toml` | possibly +tokio-stream (already in agentd) |
| `docker/entrypoint.sh` | +chat) case |
| `templates/chat-agent.template.toml` | new file |
| `otel/tests/event_kind_coverage.rs` | +3 new EventKind arms |
| `docs/ROADMAP.md` | check off orch.1 |

## Autoplan Outcome (2026-07-07)

**Decision: Deferred pending h8.1.**

Rationale: The parked-state model (D2=B) was selected — agents stay alive between inject calls, preserving the full context window across a chat session. This is the right UX. However, h8.1 (HelixDB semantic memory layer) lands before orch.1 in the build order, and a richer KB substrate will inform the chat-agent template design (what to remember, what to keep live). The parked-state scheduler machinery is the bigger scheduler change in this repo since p1.1; it should be built on a stable memory foundation.

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

## Open Decisions

These need resolution before or during implementation:

1. **D1 — `agentctl chat` streaming mechanism:** When using `FuseSource` (no HTTP, no SSE), how does `agentctl chat` stream the agent's response? Options: A) require `HttpSource` for chat (error if FUSE-only), B) poll the agent's `flight` virtual file for new events.

2. **D2 — Long-lived agent template task text:** The `chat-agent` template needs a task that causes the agent to stay alive and wait for follow-up User messages. Options: A) special task string like `"<await_chat>"` recognized by the scheduler, B) a task that naturally loops (e.g., "You are a conversational assistant. Process requests as they arrive."), C) rely purely on inject — the first chat message IS the task.

3. **D3 — `ManagementConfig::enabled` default for Docker chat mode:** Currently `management.enabled = false` by default. For Docker `chat` mode to work, it needs to be true. Options: A) `AGENTD_MANAGEMENT_ENABLED=true` env override, B) `chat` entrypoint writes a temp TOML with `management.enabled = true`, C) flip the default to `true` globally.
