# Changelog

All notable changes to agentd are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [0.4.0] - 2026-06-08

### Added
- **Capability system** (`capabilities` TOML field on `[[agents]]`/`[agent]`):
  least-privilege tool grants — `FsRead{prefix}`, `FsWrite{prefix}`, `Net{hosts}`,
  `Mcp{server, tools}`, `Spawn`. Absent field = unrestricted (backward compat);
  `capabilities = []` = deny all.
- **Capability enforcement at `ToolRegistry::invoke`**: the single unbypassable
  boundary; denials emit a `capability_denied` flight event with data `{tool, required}`
  (the agent id is in the event's top-level `agent` field) and return an `is_error`
  tool result to the agent.
- **`filtered_specs`**: agents only receive the tool specs they are authorized to
  call in their inference context — no wasted inference turns on inaccessible tools.
- **`normalize_path`**: resolves `..` components without filesystem access before
  prefix matching, blocking directory traversal (e.g. `/workspace/../etc/passwd`
  is correctly denied against a `/workspace` prefix grant).
- **`satisfies_type`**: type-level capability check used by `filtered_specs` —
  "does this agent have any FsRead capability?" vs. "can they access this specific path?"
- **`McpTool` server provenance**: `server_name` field on `McpTool` enables
  `Mcp{server, tools}` capability gating on per-server MCP tool access.

### For contributors
- New `agentd/src/capability.rs`: `Capability` enum, `normalize_path`, `satisfies`,
  `satisfies_type`. All capability logic lives here; no policy is embedded in tools.
- `Tool` trait gains `fn required_capability_for(&self, input: &Value) -> Option<Capability>`
  (default `None`). Path-based tools return the actual access path at invocation time.
- `ToolRegistry::invoke` gains `(agent_id, cap_set, recorder)` params.
- `run_tools_sequential` gains `cap_set: Option<&[Capability]>` param; threaded through
  to `invoke`. Driver passes `None` (backward compat).
- `Scheduler::new` calls `filtered_specs(cap_set)` per agent instead of shared `specs()`.

## [0.3.0] - 2026-06-08

### Added
- **Metered scheduling & admission control** (`[scheduler]` TOML section): cap total
  token spend across all agents with `global_token_budget` and limit how many model
  calls can run concurrently with `max_concurrent_inferences`. Both default to `0`
  (unlimited), preserving all prior behavior.
- **Priority-based deferred queue**: each agent carries a `priority: u32` field
  (default `0`). When the concurrency cap is full, the agent's inference is queued and
  admitted in descending-priority order (FIFO within a band) when a slot opens.
- **Admission-control flight events**: `agent_scheduled`, `agent_deferred`, and
  `agent_admission_denied` appear in `flight.jsonl`, giving full observability into
  scheduler decisions.

### Fixed
- `in_flight` underflow guards promoted from `debug_assert!` (compiled out in release)
  to `assert!`, ensuring the invariant is enforced in production builds.

### For contributors
- `SchedulerConfig` struct in `config.rs` carries `global_token_budget` and
  `max_concurrent_inferences`; wired into `Scheduler::new` via `main.rs`.
- `DeferredInfer` type with a custom `Ord` drives the `BinaryHeap` deferred queue.
- `drain_deferred` / `enqueue_or_defer` manage the admission lifecycle; both are
  tagged with `TODO(p1.x)` noting a planned `SchedulerState` refactor.

## [0.2.0] - 2026-06-08

### Added
- **Multi-agent scheduler**: Run multiple agents concurrently on independent tasks with a
  single `agentd agents.toml` invocation. Agents share a gateway and tool registry; each
  runs its own perceive → infer → act → observe loop without blocking the others.
- **`[[agents]]` config form**: Declare multiple agents in one TOML file using the
  `[[agents]]` array. The original `[agent]` single-agent form is fully backward-compatible.
- **`agents.toml` example**: Ships a two-agent example config alongside the existing
  `agent.toml`.
- **`AgentFailed` flight event**: Emitted when an agent terminates due to an inference
  error, completing the `AgentSpawned` ↔ terminal-event symmetry in the flight log.
- Non-zero exit code when any agent fails; individual per-agent errors logged with agent ID.

### For contributors
- Agent loop refactored into a sans-IO state machine (`AgentTask` + `AgentEffect`).
  `step()` → `AgentEffect` drives the loop; the scheduler performs all async IO and
  feeds results back via `provide_inference()` / `provide_tool_results()`. Enables
  concurrent IO across agents without threads.
- `driver::run` is now a single-agent backward-compat shim; the scheduler is the
  primary execution engine for all runs.
- `AgentSpawned` events are emitted before gateway initialization so startup events
  always appear in the flight log even when API key setup fails.
- `run_tools_sequential` extracted as `pub(crate)` in `agent/mod.rs`, shared by the
  driver and the scheduler.

### Fixed
- MCP child processes are now properly cleaned up on agent failure: `run_agent` returns
  `Err` instead of calling `std::process::exit(1)` while `mcp_clients` is still in scope,
  ensuring `kill_on_drop` fires before the process exits.
- Guard added for `stop_reason=tool_use` responses that contain no `ToolUse` blocks —
  previously would have sent an empty User message to the API.

## [0.1.0] - 2026-06-07

Initial release: config loader, flight recorder, `InferenceGateway` trait, Anthropic
backend, tool ABI, native file tools, MCP stdio client, and a single-agent
perceive → infer → act → observe loop.
