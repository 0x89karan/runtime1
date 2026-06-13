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
| `events` | `EventKind` enum + stable string serialization | business logic |
| `flight_recorder` | the event log (append-only JSONL writer) | business logic |
| `config` | the TOML spec | runtime state |
| `memory` | `MemoryStore` trait + `RedbStore` backend; `validate_segment` | scheduling, agent logic |

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
| `agent_spawned` | agent created (id, model, tools, limits, task_preview) |
| `perceive` | a task/event enters the agent's context |
| `inference_request` | before calling the gateway (msg count, tool count) |
| `inference_response` | after (stop_reason, token usage, running total, preview) |
| `tool_call` | before invoking a tool (id, name, input_preview) |
| `tool_result` | after (ok + preview, or error) |
| `observe` | tool results folded back into context |
| `agent_completed` | terminal: produced a final answer |
| `budget_exceeded` | terminal: per-agent token budget blown |
| `max_turns_reached` | terminal: hit the turn cap |
| `agent_failed` | terminal: inference error terminated the agent (p1.2+) |
| `agent_scheduled` | scheduler admitted the agent's inference request (p1.3+) |
| `agent_deferred` | inference deferred: concurrency cap full; includes priority + seq (p1.3+) |
| `agent_admission_denied` | terminal: global token budget exhausted; agent cannot run (p1.3+) |
| `error` | a stage failed (stage, error) |
| `capability_denied` | tool invocation blocked by capability check (tool, required, agent id) (p1.4+) |
| `message_sent` | agent sent a message to another agent (from, to) (p1.6+) |
| `message_received` | agent received a message (from, to) (p1.6+) |
| `agent_card_registered` | agent card recorded at scheduler seed (id, name, skills) (p1.6+) |
| `fuse_mounted` | `/agents` FUSE filesystem mounted (mount_point) (p3.1+) |
| `fuse_unmounted` | `/agents` FUSE filesystem unmounted (p3.1+) |
| `fuse_skipped` | FUSE mount skipped (non-Linux or `NO_FUSE` set) (p3.1+) |
| `sandbox_applied` | kernel sandbox applied to MCP server subprocess (server, rules) (p3.3+) |
| `sandbox_skipped` | MCP server spawned without sandbox (server, reason) (p3.3+) |
| `tools_registered` | tool registry populated at boot (tool_count) (p0+) |
| `agent_child_result_delivered` | child agent result delivered to spawning parent (parent_id, child_id) (p1.5+) |
| `agent_checkpointed` | agent state checkpointed to disk (agent_id, turn) (p3.2+) |
| `agent_restored` | agent state restored from checkpoint (agent_id, turn) (p3.2+) |
| `system_shutdown_requested` | SIGTERM or SIGINT received; graceful shutdown initiated (p2.3+) |
| `memory_read` | `kv_get` completed (agent, found: bool) (p5.1+) |
| `memory_write` | `kv_set` committed (agent, bytes: usize) (p5.1+) |
| `memory_unavailable` | store open or transaction failed; kv tools not registered (stage, hint, error) (p5.1+) |
| `memory_quarantined` | corrupt store renamed to `.corrupt`; fresh store opened (path) (p5.1+) |

Adding events: new behavior gets new kinds, in the same snake_case style, with a
small flat `data` object. The table above is the canonical reference — update it
when a new event kind lands.

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
Configure it under `[[tools.mcp_servers]]` (name, command, args, optional capabilities).
The stdio client (`agentd/src/tools/mcp.rs`) spawns it, does the `initialize` handshake,
lists tools, and registers each as a `Tool`. No code change needed to add a server — only config.

To sandbox the server subprocess (p3.3+), add a `capabilities` array to the server entry.
The `sandbox/` crate compiles Landlock FS rules + seccomp-bpf into a `CompiledSandbox`
applied via `pre_exec`. Omitting `capabilities` runs the server unsandboxed (warn emitted).

### Add a memory namespace

Memory namespaces use the `agent:<scope>` convention (e.g. `agent:scratch`, `agent:notes`).
To give an agent read/write access to a namespace:

1. Add `KbRead`/`KbWrite` capabilities in the agent's `capabilities` array in `agent.toml` /
   `agents.toml`:
   ```toml
   capabilities = [
     { KbWrite = { segment = "agent:scratch" } },
     { KbRead  = { segment = "agent:scratch" } },
   ]
   ```
2. Add `kv_get` and/or `kv_set` to `native` in `[tools]`:
   ```toml
   [tools]
   native = ["read_file", "kv_get", "kv_set"]
   ```
   **Note:** `kv_get`/`kv_set` are NOT included in `"all"` — they must be listed explicitly.
3. `KbRead { segment: "agent" }` is a prefix grant: it permits reading any namespace
   starting with `agent:` or `agent/`. Use the tightest scope your agent actually needs.
4. `KbWrite { segment: "" }` (empty) always denies — the empty segment is a sentinel for
   "capability type check only", never a wildcard write grant.

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
