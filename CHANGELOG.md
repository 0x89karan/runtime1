# Changelog

All notable changes to agentd are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

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
