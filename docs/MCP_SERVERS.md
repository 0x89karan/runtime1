# MCP Server Directory

This document lists known MCP servers and their HTTP endpoints for use with
agentd's Streamable HTTP transport (p7.1+). Each entry shows the TOML config
snippet to add to your `agent.toml`.

## Transport options

| Transport | Config key | Isolation | When to use |
|-----------|-----------|-----------|-------------|
| stdio     | `command` + `args` | Landlock / seccomp (process sandbox) | Local servers, dev tools, filesystem access |
| HTTP      | `url` + `headers_env` | External (server-side) | Hosted services: Linear, GitHub, Attio, etc. |

For stdio config, see the commented example in `agentd/agent.toml`.

For HTTP config:

```toml
[[tools.mcp_servers]]
name        = "<service>"
url         = "https://<service-endpoint>/mcp"
headers_env = { Authorization = "MY_API_KEY_ENV_VAR" }
# ^ value is the env var NAME, not the secret itself.
# Export: MY_API_KEY_ENV_VAR="Bearer sk-..."
```

HTTP servers are externally isolated — no `command`, `args`, `isolation`, or
`capabilities` fields apply.

## Standard servers (bundled)

These servers ship in `docker/` and require only Python 3 (no additional
package installs). Use paths relative to the directory where agentd is invoked
(typically `agentd/`).

| Server | Tool | Capability needed | Env var |
|--------|------|-------------------|---------|
| `docker/shell_mcp.py` | `run_command` | `ShellExec` in subprocess caps | none |
| `docker/http_mcp.py`  | `fetch_url`   | `Net { ports = [443] }` in subprocess caps | none |
| `docker/search_mcp.py` | `web_search` | `Net { ports = [443] }` in subprocess caps | `BRAVE_SEARCH_API_KEY` |

Self-test (no API key required):
```bash
python3 docker/shell_mcp.py --test
python3 docker/http_mcp.py  --test
python3 docker/search_mcp.py --test
```

### shell_exec

```toml
[[tools.mcp_servers]]
name    = "shell_exec"
command = "python3"
# Path relative to agentd/ (where cargo run is invoked)
args    = ["../docker/shell_mcp.py"]
capabilities = [
  { ShellExec = {} },
  { FsRead  = { prefix = "/workspace" } },
  { FsWrite = { prefix = "/tmp" } },
]
```

Note: `ShellExec` in the subprocess capabilities suppresses `DenySpawn` so
the server can fork/exec shell commands. The agent also needs
`mcp = [{ server = "shell_exec", tools = [] }]` in its `[capabilities]` section.

### http_fetch

```toml
[[tools.mcp_servers]]
name    = "http_fetch"
command = "python3"
args    = ["../docker/http_mcp.py"]
capabilities = [{ Net = { hosts = [], ports = [443] } }]
```

Only HTTPS URLs are accepted. Response body is capped at 4 MB. Redirects are
not followed — the Location header is returned so the agent can decide.

### web_search

```toml
[[tools.mcp_servers]]
name     = "web_search"
command  = "python3"
args     = ["../docker/search_mcp.py"]
passenv  = ["BRAVE_SEARCH_API_KEY"]   # forward the key into the subprocess
capabilities = [{ Net = { hosts = ["api.search.brave.com"], ports = [443] } }]
```

Set `BRAVE_SEARCH_API_KEY` before starting agentd. The `passenv` field
forwards it into the subprocess (MCP servers run with a restricted environment
that does not inherit the full parent env). Returns `isError: true` with a
setup message if the key is absent.

---

## Known servers

| Service | URL | Auth header | Env var (example) | Notes |
|---------|-----|-------------|-------------------|-------|
| Linear  | `https://mcp.linear.app/mcp` | `Authorization` | `LINEAR_MCP_TOKEN` | Bearer token from Linear API settings |
| GitHub  | `https://api.githubcopilot.com/mcp/` | `Authorization` | `GITHUB_MCP_TOKEN` | Bearer token (PAT with `repo` scope) |

> Note: OAuth-based services (Gmail, Google Drive, etc.) require a future
> `auth_provider` field that is not yet implemented. Deferred to a future increment.

## Security notes

- Header **values** are read from environment variables at startup and never
  written to disk or logged. Only the header **name** (e.g. `Authorization`)
  appears in error messages.
- `url` must start with `https://` — plain `http://` is rejected at validation.
- HTTP servers are not subject to `mcp_require_capabilities` (that gate applies
  to stdio servers only, since HTTP servers handle isolation themselves).
- **RFC-1918 / link-local addresses are not blocked.** `validate()` only checks
  for the `https://` scheme; an operator-supplied URL like
  `https://169.254.169.254/...` passes. In the single-tenant threat model the
  operator controls config, so the risk is low. Structural blocking of metadata
  endpoints is a future hardening item. (See TODOS.md p7.1-ar-02.)
