# Conventions

How to extend `agentd` without the codebase drifting. Read this before adding a
subsystem, tool, or provider. (For *what* to build, see `ROADMAP.md`; for *why*,
`DESIGN.md`.)

## Ethos

- **Light.** This is meant to be a small, fast runtime. Justify every dependency;
  prefer the standard library and small focused crates. The release profile is
  size-optimized (`opt-level = "z"`, LTO, strip, `panic = "abort"`) — keep it that way.
- **Narrow seams.** Subsystems talk through small traits (`InferenceGateway`,
  `Tool`). New subsystems get their own module and a narrow interface, not a web of
  cross-calls.
- **Mechanism vs policy.** The agent is mechanism (it runs a loop); the scheduler is
  policy (budget, concurrency, priority). Don't push policy into the agent or
  mechanism into config.

## Module boundaries

| Module | Owns | Don't put here |
|---|---|---|
| `agent` | the sans-IO loop state machine | IO, scheduling policy |
| `scheduler` | driving many agents, budget/concurrency policy, performing IO | per-agent loop logic |
| `inference` | provider abstraction + neutral message types | tool logic |
| `tools` | the `Tool` ABI, native tools, MCP client | agent/loop logic |
| `capability` | what an agent is allowed to do | enforcement of unrelated concerns |
| `bus` | agent addressing, messaging, spawn | scheduling internals |
| `flight_recorder` | the event log | business logic |
| `config` | the TOML spec | runtime state |

When a new subsystem appears in the roadmap, add a module; don't bolt it onto an
existing one.

## Error handling

- Use `anyhow::Result` at boundaries; add context with `.map_err(|e| anyhow!(...))`
  or `.context(...)`. Errors should say *which* thing failed (path, server name, etc.).
- **The agent loop never panics on bad input.** Provider, tool, and parse failures
  become recorded errors and a `Result` / an `is_error` tool result — never `panic!`,
  `unwrap()`, or `expect()` on runtime data. (`unwrap` is fine on truly-invariant
  internal state and in tests.)
- Tool failures are normal control flow: capture them as `Block::ToolResult { is_error:
  true, .. }` and let the agent react, rather than aborting the run.

## Flight-recorder event taxonomy

Every meaningful step emits exactly one event via `rec.record(turn, kind, data)`.
`kind` is a stable snake_case string; `data` is a JSON object. **Record everything;
logging is best-effort and must never crash an agent.** Previews of long text/tool
output are truncated (~200 chars) — never log secrets or full file contents.

Phase 0 kinds (canonical — do not rename):

| kind | when |
|---|---|
| `agent_spawned` | agent created (id, model, tools, limits) |
| `perceive` | a task/event enters the agent's context |
| `inference_request` | before calling the gateway (msg count, tool count) |
| `inference_response` | after (stop_reason, token usage, running total, preview) |
| `tool_call` | before invoking a tool (id, name, input) |
| `tool_result` | after (ok + preview, or error) |
| `observe` | tool results folded back into context |
| `agent_completed` | terminal: produced a final answer |
| `budget_exceeded` | terminal: token budget blown |
| `max_turns_reached` | terminal: hit the turn cap |
| `agent_failed` | terminal: inference error terminated the agent (p1.2+) |
| `error` | a stage failed (stage, error) |

Adding events: new behavior gets new kinds, in the same snake_case style, with a
small flat `data` object. Conventions for upcoming phases:

- Scheduler (p1.3): `scheduled`, `deferred`, `admission_denied` (include agent id,
  reason, budget/slot state).
- Capabilities (p1.4): `capability_denied` (tool, required capability, agent id).
- Bus (p1.5): `message_sent`, `message_received` (from, to); a spawned child emits its
  own `agent_spawned` under its own id, with a `parent` field.

Keep the recorder agent-tagged: in multi-agent phases, every event must carry the
acting agent's id so a single `flight.jsonl` is demultiplexable.

## Extension recipes

### Add an inference backend
1. New file `agentd/src/inference/<provider>.rs`; `pub mod <provider>;` in `inference/mod.rs`.
2. `impl InferenceGateway` — map the neutral `Block`/`Msg`/`ToolSpec` types to/from the
   provider's wire format; return `InferenceResponse` with `stop_reason` and token usage.
3. Read credentials from **env only**. Add a base-URL env override.
4. Wire it into the `match config.model.provider` in `main.rs` (and later the scheduler).
> Reminder of the locked decision: remote-only is the default. A *local* backend is
> permitted solely as another `impl InferenceGateway`, never as a core assumption.

### Add a native tool
1. In `agentd/src/tools/native.rs`, define a struct and `impl Tool` (name, description,
   `input_schema` as JSON Schema, async `invoke`).
2. Validate inputs in `invoke`; return a helpful `anyhow` error on bad input.
3. Register it in `register_native`'s table.
4. From p1.4 on, declare the capability the tool requires so the registry can gate it.
> Native tools are for zero-dependency convenience. Prefer exposing real capabilities
> as **MCP servers** — MCP is the tool ABI.

### Connect an MCP server
Configure it under `[[tools.mcp_servers]]` (name, command, args). The stdio client
(`agentd/src/tools/mcp.rs`) spawns it, does the `initialize` handshake, lists tools, and
registers each as a `Tool`. No code change needed to add a server — only config.

## Config

- An agent is a TOML spec. Secrets are **never** in config — env only.
- Provide serde defaults for optional fields (see `config.rs`) so specs stay terse.
- When extending config (multi-agent, capabilities), keep older single-agent specs
  working where reasonable, or migrate the sample and note it in the PR.

## Diagnostics vs. the flight recorder

Two separate channels — don't conflate them:
- **`tracing`** → human diagnostics to **stderr** (`RUST_LOG` controls level). For
  operators watching a run.
- **Flight recorder** → structured **agent activity** to `flight.jsonl`. The durable,
  machine-readable record of what agents did.

The agent's **final answer** goes to **stdout** and nothing else does, so `agentd` is
pipeline-friendly.

## Testing

- Unit-test each module. **Loop and scheduler tests must not hit the network** — use
  the `#[cfg(test)] MockGateway` (added in p1.1) that returns canned
  `InferenceResponse`s, including tool-use turns.
- For the MCP transport, test against a tiny mock stdio server (a fixture that speaks
  the JSON-RPC handshake) rather than a real external server.
- Keep `agent.toml`'s demo as a living smoke test: its flight-event sequence is the
  regression baseline for the single-agent path — don't let refactors change it.
