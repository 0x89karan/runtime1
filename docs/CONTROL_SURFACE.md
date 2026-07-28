# /agents/control — operator control surface

`/agents/control` is a write-only pseudo-file in the FUSE virtual filesystem
that lets an operator inject new agents into a running `agentd` scheduler
**without restarting the process or writing a config file**.

## Requirements

- `agentd` must be running with FUSE enabled (Linux only; skipped on macOS / when
  `NO_FUSE=1` is set).
- The filesystem must be mounted (default: `/agents/`).

## Wire format

Write a single JSON object to the file. One tagged verb per write:

| Verb | Shape | Since |
|---|---|---|
| `spawn` | `{"spawn":{"task":"…", …}}` (bare `{"task":"…"}` also accepted) | p7.3 |
| `approve` | `{"approve":{"id":"act_1","edits":{…},"auto_approve_kind":"write_file"}}` (`edits` / `auto_approve_kind` optional) | p7.4 |
| `reject` | `{"reject":{"id":"act_1","reason":"path looks unsafe"}}` (`reason` optional) | p7.4 |
| `inject` | `{"inject":{"agent_id":"…","text":"…"}}` | orch.1 |
| `reset_budget` | `{"reset_budget":{"target":{"agent":"cos"}}}` — or `{"reset_budget":{"target":"global"}}`. Rebases the window anchor to current spend | ux.8′ |
| `set_budget` | `{"set_budget":{"target":{"agent":"cos"},"limit":50000}}` — `limit: 0` = **UNLIMITED**, not "stop". A `"global"` target is rejected | ux.11a |
| `cancel` | `{"cancel":{"agent_id":"scout-1"}}` — stops at the next step boundary and cascades to the spawned subtree | ux.13 |
| `set_caps` | `{"set_caps":{"agent_id":"scout-1","capabilities":[{"KbRead":{"segment":"ops:briefs"}}]}}` — narrow/revoke only; widening is rejected | ux.13 |

`target` is typed, not a bare string, so an agent literally named `global` can never collide with
the global window: `"global"` or `{"agent":"<id>"}`.

**The FUSE path is fire-and-forget.** A `write()` that succeeds means the command parsed and was
queued to the scheduler channel — *not* that the scheduler accepted it. Only the management API
(`:7999`) carries a confirmation channel back, which is why `agentctl watch` over FUSE says "cannot
confirm the scheduler accepted it" instead of reporting an outcome (`DataSource::confirms_mutations()`,
ux.13-TUI). If you need the verdict (the cancel cascade count, the old→new budget), use HTTP.

The two most-used verbs in detail:

### Spawn a new agent

```json
{
  "spawn": {
    "task":         "summarise the latest GitHub issues",
    "id":           "issue-summariser",
    "max_turns":    20,
    "token_budget": 100000,
    "priority":     0,
    "capabilities": [
      { "KbRead":  { "segment": "ops:briefs" } },
      { "KbWrite": { "segment": "ops:briefs" } }
    ],
    "orchestrated": false
  }
}
```

Bare `{"task":"...", ...}` (without the `"spawn"` wrapper) is accepted for back-compat.

| Field | Type | Default | Notes |
|---|---|---|---|
| `task` | string | **required** | Initial task text. Must not be empty. |
| `id` | string | `"operator-N"` | Agent ID. Validated `[a-zA-Z0-9_-]` only (`validate_child_id`): `:`, `/`, and `.` are reserved because the id becomes the memory-namespace prefix, so whitespace and dots are rejected too — not just `/` and `..`. |
| `max_turns` | u32 | scheduler default | Maximum turn count. |
| `token_budget` | u64 | scheduler default | Hard token ceiling. |
| `priority` | u32 | 0 | Reserved; not yet used by the scheduler. |
| `capabilities` | `Capability[]` | none | Extra capabilities to grant. **Not bare strings** — the `Capability` enum serializes PascalCase-externally-tagged: `{"FsRead":{"prefix":"…"}}`, `{"FsWrite":{"prefix":"…"}}`, `{"Net":{"hosts":[…],"ports":[…]}}`, `{"Mcp":{"server":"…","tools":[…]}}`, `{"KbRead":{"segment":"…"}}`, `{"KbWrite":{"segment":"…"}}`, `{"Credential":{"provider":"…"}}`, and the unit variants `"Spawn"`, `"ShellExec"`, `"RunsRead"`, `"BriefPublish"`, `"RunJob"`. See `agentd/src/capability.rs`. |
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
| `agent_id` | string | Target agent. Must not be empty. **No charset check here** — unlike `spawn`, `parse_control_command` only rejects an empty id; an id that matches no live agent is simply dropped by the scheduler. (`agentctl` sanitizes ids client-side before they reach a URL — ux.13-TUI.) |
| `text` | string | User turn text. Must not be empty. Max 64 KiB. |

Errors: `EINVAL` if `agent_id` or `text` is empty, `text` exceeds 64 KiB, or (for `spawn`) the `id` contains illegal characters; `EBUSY` if the channel is full.

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
- `[a]` resolves pending approvals (`approve` / `reject`), and `[x]` on a Dashboard
  row runs the ux.13 verbs — *Park* (`set_budget` at the spend already recorded),
  *Set budget*, *Cancel* (ux.13-TUI, v0.115.0).
- **Over FUSE those verbs cannot be confirmed.** The overlay says so rather than
  claiming an outcome: a queued command reads "cannot confirm the scheduler accepted
  it", and a cancelled row shows `NOT CANCELLED` until a later snapshot proves
  otherwise. Run `agentctl watch --url http://localhost:7999` when you need the
  verdict — the cancel cascade count, the old→new budget
  (`DataSource::confirms_mutations()`).
- The reverse trade also exists: `[d]` ("don't ask again for this kind") registers a
  standing auto-approve rule **only over FUSE**, because `auto_approve_kind` rides on
  the `approve` control command and no HTTP route carries it. Over `--url` the TUI
  approves the one action and says so plainly rather than implying a policy.

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
