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

| Server | Tool(s) | Capability needed | Env var |
|--------|---------|-------------------|---------|
| `docker/shell_mcp.py`  | `run_command` | `ShellExec` in subprocess caps | none |
| `docker/http_mcp.py`   | `fetch_url`   | `Net { ports = [443] }` in subprocess caps | none |
| `docker/search_mcp.py` | `web_search`  | `Net { ports = [443] }` in subprocess caps | `BRAVE_SEARCH_API_KEY` |
| `docker/oauth_mcp.py`  | `oauth_start_auth`, `oauth_check_auth`, `oauth_call_api` | `Net { hosts = [...], ports = [443] }` | `OAUTH_CLIENT_ID`, `OAUTH_CLIENT_SECRET` |

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

### oauth_mcp (Google Gmail + Drive)

`docker/oauth_mcp.py` handles the full OAuth2 authorization-code + PKCE flow so an agent can
call any OAuth-protected API without storing credentials in config files. Google is the
first example; other providers (Slack, GitHub, Notion) work by changing the env vars.

#### Google Cloud Console setup

> **Required once per project.** Skip if you already have a client ID and secret.

1. Go to [console.cloud.google.com](https://console.cloud.google.com) and select or create a project.
2. **APIs & Services → Library** → enable "Gmail API" and "Google Drive API".
3. **APIs & Services → OAuth consent screen** → choose **External** → fill in App name and your email.
   Add your email as a **Test user** if the app is in testing mode.
4. **APIs & Services → Credentials → Create Credentials → OAuth client ID**.
5. Application type: **Desktop app** ← required for the localhost callback to work.
6. Copy the **Client ID** and **Client Secret**.

#### Environment variables

Export these before starting agentd:

```bash
# Required (2 vars):
export OAUTH_CLIENT_ID="<your-client-id>.apps.googleusercontent.com"
export OAUTH_CLIENT_SECRET="<your-client-secret>"

# Pre-set for Google (copy these exactly — no need to change):
export OAUTH_AUTH_URL="https://accounts.google.com/o/oauth2/v2/auth"
export OAUTH_TOKEN_URL="https://oauth2.googleapis.com/token"
export OAUTH_SCOPES="https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/drive.readonly"
export OAUTH_ALLOWED_HOSTS="accounts.google.com,oauth2.googleapis.com,www.googleapis.com,gmail.googleapis.com"
export OAUTH_PROVIDER_NAME="google"

# Optional — skip the browser dance if you already have a refresh token:
# export OAUTH_REFRESH_TOKEN="<your-refresh-token>"
```

#### Agent TOML config

```toml
[[tools.mcp_servers]]
name    = "google_oauth"
command = "python3"
args    = ["../docker/oauth_mcp.py"]
passenv = [
  "OAUTH_CLIENT_ID", "OAUTH_CLIENT_SECRET",
  "OAUTH_AUTH_URL",  "OAUTH_TOKEN_URL",
  "OAUTH_SCOPES",    "OAUTH_ALLOWED_HOSTS",
  "OAUTH_PROVIDER_NAME",
  "OAUTH_REFRESH_TOKEN",   # optional: skip the dance
]
capabilities = [
  { Net = { hosts = [
    "accounts.google.com",
    "oauth2.googleapis.com",
    "www.googleapis.com",
    "gmail.googleapis.com",
  ], ports = [443] } },
]
```

Agent capabilities section:
```toml
[capabilities]
mcp = [{ server = "google_oauth", tools = [] }]
```

#### Approval dance (3 steps)

The agent handles authorization automatically — no `agentctl oauth login` command needed:

1. **Agent calls `oauth_start_auth`** → receives an authorization URL.
2. **Agent surfaces the URL via `request_approval`** → you see it in the Approvals pane of
   `agentctl watch` (press `[a]`). Click the URL, sign in with Google, and grant the
   requested permissions in your browser.
3. **Agent calls `oauth_check_auth`** → if the browser flow completed, returns
   `{ ready: true, scopes: [...] }`. The agent can now call `oauth_call_api` to read Gmail,
   list Drive files, etc.

The refresh token is saved to `~/.agentos-oauth/google.json` (mode 0600). On subsequent runs,
the agent picks up the saved token — no browser flow required unless the token is revoked.

#### Error reference

| Error | Meaning | Fix |
|-------|---------|-----|
| `oauth_mcp: missing required env OAUTH_CLIENT_ID` | Server exited at startup | Export `OAUTH_CLIENT_ID` before starting agentd |
| `oauth_mcp: missing required env OAUTH_CLIENT_SECRET` | Server exited at startup | Export `OAUTH_CLIENT_SECRET` before starting agentd |
| `{"error": "auth_not_ready"}` | Agent called `oauth_call_api` before auth completed | Call `oauth_start_auth` first, complete browser flow, then `oauth_check_auth` |
| `{"error": "host_not_allowed", "host": "..."}` | URL hostname not in `OAUTH_ALLOWED_HOSTS` | Add hostname to `OAUTH_ALLOWED_HOSTS` env var |
| `{"error": "timeout"}` from `oauth_check_auth` | 10-minute browser flow window expired | Call `oauth_start_auth` again to get a fresh URL |

Self-test (no credentials required):
```bash
python3 docker/oauth_mcp.py --test
```

---

## Known servers

| Service | URL | Auth header | Env var (example) | Notes |
|---------|-----|-------------|-------------------|-------|
| Linear  | `https://mcp.linear.app/mcp` | `Authorization` | `LINEAR_MCP_TOKEN` | Bearer token from Linear API settings |
| GitHub  | `https://api.githubcopilot.com/mcp/` | `Authorization` | `GITHUB_MCP_TOKEN` | Bearer token (PAT with `repo` scope) |

> Note: OAuth-based services (Gmail, Google Drive, etc.) use `docker/oauth_mcp.py` — see
> the **oauth_mcp** section in Standard servers above for setup instructions.

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
