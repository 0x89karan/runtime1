# agentd

The AgentOS runtime. Agents are the primitive; `agentd` runs them.

## Quickstart

1. Export your API key:
   ```sh
   export ANTHROPIC_API_KEY=sk-ant-...
   ```

2. Run an agent:
   ```sh
   cargo run -- agent.toml
   ```

3. Watch the flight log:
   ```sh
   tail -f flight.jsonl
   ```

> **Note:** OpenSSL dev headers (`libssl-dev` + `pkg-config` on Debian/Ubuntu) are
> required because the Anthropic backend uses `native-tls`. They are preinstalled on
> macOS. Phase 2 switches to `rustls-tls` for static musl builds.

## Configuration

Agents are specified as TOML files. Two forms are supported:

**Single agent** — `agent.toml` (backward-compatible):
```toml
[agent]
id    = "scout"
task  = "List the project and report."
```

**Multiple agents** — `agents.toml` (p1.2+):
```toml
[[agents]]
id   = "surveyor"
task = "List agentd/src and describe each file."

[[agents]]
id   = "analyst"
task = "Read Cargo.toml and explain each dependency."
```

Both forms share `[model]`, `[tools]`, and `[scheduler]` sections. See `agent.toml` and
`agents.toml` for annotated examples. Cannot mix both forms in one file.

### Config reference

| Field | Default | Description |
|---|---|---|
| `[agent].id` / `[[agents]].id` | *required* | Unique identifier for this agent |
| `[agent].task` / `[[agents]].task` | `""` | The task to perform (single-agent form reads from stdin if empty) |
| `[agent].max_turns` / `[[agents]].max_turns` | `20` | Turn limit before `max_turns_reached` event |
| `[agent].token_budget` / `[[agents]].token_budget` | `100000` | Per-agent cumulative token ceiling (input + output) |
| `[[agents]].priority` | `0` | Scheduling priority — higher runs before lower when the concurrency cap is full |
| `[agent].capabilities` / `[[agents]].capabilities` | absent (unrestricted) | Least-privilege tool grants. Absent = all tools; `[]` = deny all; `[{FsRead={prefix="/workspace"}}]` = scoped access. Variants: `FsRead{prefix}`, `FsWrite{prefix}`, `Net{hosts}` (advisory), `Mcp{server,tools}`, `Spawn` (reserved). Prefix must be an absolute path. |
| `model.provider` | `"anthropic"` | Inference backend |
| `model.model` | `"claude-sonnet-4-6"` | Model identifier passed to the provider |
| `model.max_tokens` | `4096` | Max tokens per inference response |
| `tools.native` | `[]` | Native tools: `["all"]` or `["read_file", "write_file", "list_dir"]` |
| `scheduler.global_token_budget` | `0` | Global token ceiling across all agents; `0` = unlimited |
| `scheduler.max_concurrent_inferences` | `0` | Max in-flight model calls at once; `0` = unlimited |

Secrets are **never** in config — read from environment only (`ANTHROPIC_API_KEY`).

## Flight log

Events are appended to `flight.jsonl` in the working directory as newline-delimited JSON:

```json
{"ts":"2026-06-07T00:00:00Z","agent":"scout","turn":null,"kind":"agent_spawned","data":{"model":"claude-sonnet-4-6",...}}
```

`turn` is `null` for lifecycle events (`agent_spawned`, `agent_completed`) and a 1-based
integer for per-turn events (`perceive`, `inference_request`, ...).

## Commands

```sh
cd agentd

# Build
cargo build                      # debug
cargo build --release            # size-optimized (~2 MB target)

# Quality gate (run before committing)
cargo clippy -- -D warnings
cargo test

# Run (single agent)
cargo run -- agent.toml

# Run (multiple agents — p1.2+)
cargo run -- agents.toml

# Verbose diagnostics
RUST_LOG=debug cargo run -- agents.toml
```
