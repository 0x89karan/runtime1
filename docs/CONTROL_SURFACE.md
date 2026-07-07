# /agents/control — operator control surface

`/agents/control` is a write-only pseudo-file in the FUSE virtual filesystem
that lets an operator inject new agents into a running `agentd` scheduler
**without restarting the process or writing a config file**.

## Requirements

- `agentd` must be running with FUSE enabled (Linux only; skipped on macOS / when
  `NO_FUSE=1` is set).
- The filesystem must be mounted (default: `/agents/`).

## Wire format

Write a single JSON object to the file. Two command variants are supported.

### Spawn a new agent

```json
{
  "spawn": {
    "task":         "summarise the latest GitHub issues",
    "id":           "issue-summariser",
    "max_turns":    20,
    "token_budget": 100000,
    "priority":     0,
    "capabilities": ["kb_read", "kb_write"],
    "orchestrated": false
  }
}
```

Bare `{"task":"...", ...}` (without the `"spawn"` wrapper) is accepted for back-compat.

| Field | Type | Default | Notes |
|---|---|---|---|
| `task` | string | **required** | Initial task text. Must not be empty. |
| `id` | string | `"operator-N"` | Agent ID. Validated: no `/` or `..`. |
| `max_turns` | u32 | scheduler default | Maximum turn count. |
| `token_budget` | u64 | scheduler default | Hard token ceiling. |
| `priority` | u32 | 0 | Reserved; not yet used by the scheduler. |
| `capabilities` | string[] | none | Extra capabilities to grant. |
| `orchestrated` | bool | `false` | When `true`, agent parks after each response awaiting the next `inject`. Used by `agentctl orchestrate` (orch.1+). |

### Inject a user turn into a waiting agent (orch.1+)

```json
{
  "inject": {
    "agent_id": "orch-default",
    "text":     "now narrow it down to the three most important issues"
  }
}
```

| Field | Type | Notes |
|---|---|---|
| `agent_id` | string | Target agent. Validated: `[a-zA-Z0-9_-]` only; must not be empty. |
| `text` | string | User turn text. Must not be empty. Max 64 KiB. |

Errors: `EINVAL` if `agent_id` or `text` is empty or `agent_id` contains illegal characters; `EBUSY` if the channel is full.

## Shell example

```bash
echo '{"task":"list all MCP tools available","id":"tool-scout"}' \
  > /agents/control
```

## `agentctl watch` integration

When `agentd` is running, `agentctl watch` detects `/agents/control` and
uses it automatically:

- The `[n]` spawn form generates a JSON preview (not TOML) when the control
  surface is available. Press `[g]` to generate, `[r]` to inject.
- After injection the TUI shows a green success banner and stays open so you
  can watch the new agent appear in the Dashboard.
- If `agentd` is not running (control file absent), the TUI falls back to
  writing a temporary TOML config and exec'ing `agentd`.

## Error handling

| errno | Meaning |
|---|---|
| `EBUSY` | Scheduler channel full (16-slot backpressure); retry. |
| `EROFS` | No `ControlDispatch` registered (FUSE mounted but dispatch wiring absent). |
| `EINVAL` | JSON parse error or `task` is empty. See `flight.jsonl` for details. |

Errors are recorded as `fuse_control_error` events in `flight.jsonl`. Successful
injections are recorded as `fuse_control_received`.

## Known footgun

`agentctl spawn` (the non-TUI CLI subcommand) **does not use /agents/control**.
It always execs a fresh `agentd` process. Use `agentctl watch` → `[n]` for live
injection when `agentd` is already running.
