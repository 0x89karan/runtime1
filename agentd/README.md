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
> not required until p0.2, which adds the Anthropic inference backend. For now,
> `cargo build` works out of the box on macOS and Linux.

## Configuration

Agents are specified as TOML files. See `agent.toml` for an annotated example.

| Field | Default | Description |
|---|---|---|
| `agent.id` | *required* | Unique identifier for this agent |
| `agent.task` | `""` | The task to perform (read from stdin if empty and not a tty — p0.4) |
| `agent.max_turns` | `20` | Turn limit before `max_turns_reached` event |
| `agent.token_budget` | `100000` | Cumulative token ceiling (input + output) |
| `model.provider` | `"anthropic"` | Inference backend |
| `model.model` | `"claude-sonnet-4-6"` | Model identifier passed to the provider |
| `model.max_tokens` | `4096` | Max tokens per inference response |
| `tools.native` | `[]` | Native tools: `["all"]` or `["read_file", "write_file", "list_dir"]` |

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

# Run
cargo run -- agent.toml

# Verbose diagnostics
RUST_LOG=debug cargo run -- agent.toml
```
