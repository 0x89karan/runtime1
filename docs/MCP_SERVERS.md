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
| `docker/shell_mcp.py`    | `run_command` | `ShellExec` in subprocess caps | none |
| `docker/http_mcp.py`     | `fetch_url`   | `Net { ports = [443] }` in subprocess caps | none |
| `docker/search_mcp.py`   | `web_search`  | `Net { ports = [443] }` in subprocess caps | `BRAVE_SEARCH_API_KEY` |
| `docker/oauth_mcp.py`    | `oauth_start_auth`, `oauth_check_auth`, `oauth_call_api` | `Net { hosts = [...], ports = [443] }` | `OAUTH_CLIENT_ID`, `OAUTH_CLIENT_SECRET` |
| `docker/cron_mcp.py`     | `wait_for_trigger` | none | `TRIGGER_CRON` or `TRIGGER_INTERVAL` |
| `docker/fs_watch_mcp.py` | `wait_for_trigger` | `FsRead { prefix = "/..." }` | `TRIGGER_WATCH_PATH` |
| `docker/webhook_mcp.py`  | `wait_for_trigger` | `Net { ports = [PORT] }` | `TRIGGER_WEBHOOK_PORT` (optional) |

Self-test (no API key required):
```bash
python3 docker/shell_mcp.py --test
python3 docker/http_mcp.py  --test
python3 docker/search_mcp.py --test
python3 docker/cron_mcp.py  --test
python3 docker/fs_watch_mcp.py --test
python3 docker/webhook_mcp.py  --test
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

## Trigger servers (event-driven agents)

These three servers expose a single `wait_for_trigger()` tool that makes an
agent event-driven. Because `agentd`'s `MCP_TIMEOUT` is 30 s, the servers use
a **poll-and-retry** pattern: the tool returns within 25 s with
`{"status":"waiting"|"fired"|"timeout"}`. The agent's task instructs it to call
`wait_for_trigger()` in a loop until `status == "fired"`.

### How trigger agents work

1. Agent calls `wait_for_trigger(timeout_s=25)` as its first action.
2. If `status == "waiting"`, the condition hasn't fired yet — call again next turn.
3. If `status == "fired"`, the event occurred — proceed with the task.
4. If `status == "timeout"`, `TRIGGER_MAX_WAIT_S` was exceeded — the agent stops.

The MCP server process is **long-running** (not restarted per call), so server
state (cron schedule, filesystem snapshot, webhook queue) persists across calls.

**On checkpoint:** agentd sends SIGTERM to MCP subprocesses. `cron_mcp.py`
recomputes the next fire time on restart. `fs_watch_mcp.py` rebuilds the
directory snapshot (changes during the downtime window become the new baseline
and are NOT reported). `webhook_mcp.py` loses its in-memory queue — events
that arrived during downtime are permanently lost; for reliability requirements,
write events to durable storage before POSTing.

### cron_mcp

Fires on a cron schedule (UTC) or fixed interval.

```bash
# Requires: export TRIGGER_CRON or TRIGGER_INTERVAL
export TRIGGER_CRON="0 9 * * 1-5"   # weekdays at 09:00 UTC
# or:
export TRIGGER_INTERVAL="every 1h"   # every hour (mutually exclusive)
export TRIGGER_MAX_WAIT_S=86400      # optional: abort after 24 hours
```

```toml
[[tools.mcp_servers]]
name    = "cron_trigger"
command = "python3"
# Adjust path to repo location:
args    = ["/usr/lib/agentos/docker/cron_mcp.py"]
passenv = ["TRIGGER_CRON", "TRIGGER_INTERVAL", "TRIGGER_MAX_WAIT_S"]
```

Cron grammar: `*`, `*/N`, integer, comma-list. Only UTC timezone.
DOW field: 0=Sunday (POSIX), 7=Sunday alias. Day-of-week range: `1-5` (Mon–Fri).
Interval units: `s` (seconds), `m` (minutes), `h` (hours).
Unsupported tokens cause `exit 1` at startup with a clear error message.

### fs_watch_mcp

Fires when files in a watched directory change (create / modify / delete).

```bash
# Requires: export TRIGGER_WATCH_PATH
export TRIGGER_WATCH_PATH=/workspace/src
export TRIGGER_POLL_INTERVAL_S=2       # polling granularity (default 2s)
export TRIGGER_IGNORE_PATTERNS="*.pyc,__pycache__,*.swp"
export TRIGGER_QUIET_PERIOD_S=1        # debounce window (default 1s)
export TRIGGER_MAX_WAIT_S=3600         # optional
```

```toml
# Requires: export TRIGGER_WATCH_PATH=/path/to/watch
[[tools.mcp_servers]]
name    = "fs_watch"
command = "python3"
args    = ["/usr/lib/agentos/docker/fs_watch_mcp.py"]
passenv = [
    "TRIGGER_WATCH_PATH",
    "TRIGGER_POLL_INTERVAL_S",
    "TRIGGER_IGNORE_PATTERNS",
    "TRIGGER_QUIET_PERIOD_S",
    "TRIGGER_MAX_WAIT_S",
]
capabilities = [{ FsRead = { prefix = "/" } }]
```

Change detection tracks mtime, file size, and inode — delete+recreate at the
same size is correctly detected. Ignore patterns use `fnmatch` and are applied
to filenames. Debounce coalesces rapid writes into a single fire event.

### webhook_mcp

Fires when an HTTP POST arrives at the configured port.

```bash
export TRIGGER_WEBHOOK_PORT=9000         # default
export TRIGGER_WEBHOOK_HOST=127.0.0.1   # default — loopback only
export TRIGGER_WEBHOOK_SECRET=my-secret # optional HMAC-SHA256 key
export TRIGGER_MAX_WAIT_S=3600           # optional
```

```toml
[[tools.mcp_servers]]
name    = "webhook_trigger"
command = "python3"
args    = ["/usr/lib/agentos/docker/webhook_mcp.py"]
passenv = [
    "TRIGGER_WEBHOOK_PORT",
    "TRIGGER_WEBHOOK_HOST",
    "TRIGGER_WEBHOOK_SECRET",
    "TRIGGER_MAX_WAIT_S",
]
capabilities = [{ Net = { ports = [9000] } }]
```

**Webhook security:**
- `X-Timestamp` header (Unix epoch integer) is always validated — rejects
  requests outside ±5 minutes even without HMAC (limits replay window to ±5 min; no nonce/dedup for sub-window replays).
- When `TRIGGER_WEBHOOK_SECRET` is set, validates `X-Hub-Signature-256: sha256=<hex>`
  using `hmac.compare_digest` (timing-safe).
- Body capped at 64 KB (checked from `Content-Length` header before reading).
- Queue full (> 10 pending events) returns HTTP 429.
- `rejected_count` is included in every `wait_for_trigger` response so the
  agent knows how many requests were rejected while it was waiting.

**Sending a webhook (example):**
```bash
BODY='{"event": "push", "branch": "main"}'
TS=$(date +%s)
SIG=$(echo -n "$BODY" | openssl dgst -sha256 -hmac "$TRIGGER_WEBHOOK_SECRET" | awk '{print $2}')
curl -X POST http://127.0.0.1:9000/ \
  -H "Content-Type: application/json" \
  -H "X-Timestamp: $TS" \
  -H "X-Hub-Signature-256: sha256=$SIG" \
  -d "$BODY"
```

---

## Known servers

| Service | URL | Auth header | Env var (example) | Notes |
|---------|-----|-------------|-------------------|-------|
| Linear  | `https://mcp.linear.app/mcp` | `Authorization` | `LINEAR_MCP_TOKEN` | Bearer token from Linear API settings |
| GitHub  | `https://api.githubcopilot.com/mcp/` | `Authorization` | `GITHUB_MCP_TOKEN` | Bearer token (PAT with `repo` scope) |

> Note: OAuth-based services (Gmail, Google Drive, etc.) use `docker/oauth_mcp.py` — see
> the **oauth_mcp** section in Standard servers above for setup instructions.

## GitHub API (credential broker)

The `github-agent` template uses the **credential broker** (cred.4) to route GitHub
PAT requests through the `api-key-header` adapter with `header_value_prefix = "Bearer"`,
so the `Authorization: Bearer <PAT>` header is constructed and injected by the broker
rather than passed as a plain env var to the MCP server subprocess.

```toml
# In agent.toml (or generated from github-agent.template.toml):
[credential_gateway.providers.github]
auth_style           = "api-key-header"
upstream_base        = "https://api.github.com"
header_name          = "Authorization"
header_value_prefix  = "Bearer"
secret_key           = "GITHUB_TOKEN"
max_requests_per_agent = 500

[[tools.mcp_servers]]
name    = "http_fetch"
command = "python3"
args    = ["../docker/http_mcp.py"]
capabilities = [
    { Net = { hosts = ["api.github.com"], ports = [443] } },
    { Credential = { provider = { Custom = "github" } } },
]
```

Set `GITHUB_TOKEN` in your environment before starting agentd:

```bash
export GITHUB_TOKEN="ghp_your_personal_access_token"
```

The PAT needs `repo` and `read:org` scopes for most repository and PR operations.

**Broker vs. direct env passthrough:** The credential gateway intercepts each request
from `http_mcp.py`, injects `Authorization: Bearer <token>` from the broker's secret
store, and enforces a per-agent cap (`max_requests_per_agent`). `GITHUB_TOKEN` is in
`PASSENV_BLOCKLIST` so the raw key is never forwarded to the subprocess directly.

**Spend cap:** `max_requests_per_agent = 500` limits total API calls per agent session.
The cap is tracked in memory and cleared when the agent exits. It resets on agentd
restart — it is not persisted across crashes (only clean exits reset the counter).

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
