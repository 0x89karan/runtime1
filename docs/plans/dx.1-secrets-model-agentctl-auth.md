<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260704-013618.md -->

# dx.1 — Mac Docker DX: Secrets Model + agentctl auth google

**Increment:** dx.1  
**Version:** v0.52.0 (tentative)  
**Status:** Planning  
**Branch:** dx.1-secrets-model-agentctl-auth  

## Goal

Bring the CoS harness to `docker compose up -d cos` with no OAuth env var boilerplate.
The entire OAuth dance moves to the host (one-time), written to `~/.agentos-secrets/google.json`.
At runtime, the container reads the pre-provisioned token from `/run/secrets/google.json`.

## Context

cos.1 proved the runtime works. Current pain points:
1. `docker compose up -d cos` requires 6+ env vars to be set, including static Google URLs that shouldn't be user-facing
2. `OAUTH_REFRESH_TOKEN` is a footgun — it's a secret, it's in the env, and users have to extract it manually
3. The OAuth PKCE dance (`oauth_start_auth`) runs inside the container — requires `--service-ports` and browser access through port 8585
4. There's no host-side provisioning tool; users must set `OAUTH_REFRESH_TOKEN` manually after running the container once

## Acceptance Criteria

- [ ] `agentctl auth google` completes the PKCE flow on the host and writes `~/.agentos-secrets/google.json` (mode 0600)
- [ ] `docker compose up -d cos` starts the CoS harness with only `ANTHROPIC_API_KEY` + `OAUTH_CLIENT_ID` + `OAUTH_CLIENT_SECRET` in the shell — no other OAuth vars
- [ ] `docker compose logs -f cos` shows the cron agent waking and running the brief
- [ ] Existing `OAUTH_REFRESH_TOKEN` env var bypass still works (backward compat)

## Scope

### 1. `docker/oauth_mcp.py` changes

- Remove `OAUTH_AUTH_URL` and `OAUTH_TOKEN_URL` from required env vars; hardcode Google defaults:
  ```python
  GOOGLE_AUTH_URL   = "https://accounts.google.com/o/oauth2/v2/auth"
  GOOGLE_TOKEN_URL  = "https://oauth2.googleapis.com/token"
  GOOGLE_SCOPES     = "https://www.googleapis.com/auth/gmail.readonly"
  GOOGLE_ALLOWED_HOSTS = "accounts.google.com,oauth2.googleapis.com,www.googleapis.com,gmail.googleapis.com"
  ```
- In `_load_config()`: make `OAUTH_AUTH_URL`, `OAUTH_TOKEN_URL`, `OAUTH_SCOPES`, `OAUTH_ALLOWED_HOSTS`, `OAUTH_PROVIDER_NAME` optional with Google defaults
- Add secrets file read before `OAUTH_REFRESH_TOKEN` env var check:
  ```python
  SECRETS_FILE = "/run/secrets/google.json"
  # In _load_config(): read SECRETS_FILE if it exists; extract refresh_token → _cfg["OAUTH_REFRESH_TOKEN"]
  ```
- Update docstring to reflect new secrets model

### 2. `docker-compose.yml` changes

For `cos` service:
- Add `~/.agentos-secrets:/run/secrets:ro` volume bind
- Expose `7999:7999` and `8080:8080` ports (for p7.7 and dx.2 respectively)
- Remove `OAUTH_AUTH_URL`, `OAUTH_TOKEN_URL`, `OAUTH_SCOPES`, `OAUTH_ALLOWED_HOSTS`, `OAUTH_PROVIDER_NAME`, `OAUTH_REFRESH_TOKEN` from env
- Keep `OAUTH_CALLBACK_PORT` (still valid for non-secrets flows)

For `agent` service:
- Same static URL removals, keep `OAUTH_REFRESH_TOKEN` with updated comment (optional, for non-secrets flows)
- Document `~/.agentos-secrets:/run/secrets:ro` as optional mount for google-agent template users

### 3. `agentctl auth google` subcommand

New `agentctl/src/auth.rs`:
```
pub fn run(args: Args) -> anyhow::Result<()>
  - Validate client_id + client_secret (from args or env OAUTH_CLIENT_ID / OAUTH_CLIENT_SECRET)
  - Ensure ~/.agentos-secrets/ exists (mkdir with prompt if missing)
  - Check if ~/.agentos-secrets/google.json already exists (confirm overwrite)
  - Run PKCE flow:
    - Generate code_verifier + code_challenge
    - Bind local HTTP server on :8585
    - Print auth URL + open browser (webbrowser)
    - Wait for callback (max 10 min)
    - Exchange code for tokens
  - Write ~/.agentos-secrets/google.json (mode 0600) with {"refresh_token": "..."}
```

In `agentctl/src/main.rs`:
- Add `Auth(auth::Args)` variant to `Commands`
- Add `auth google` as subcommand with `--client-id` and `--client-secret` flags

Dependencies:
- `open` crate for browser open (or `std::process::Command` with `open`/`xdg-open`)
- No new HTTP deps needed — callback server uses stdlib `TcpListener`

### 4. `docs/plans/state-of-the-union-runtime.md` update

Mark dx.1 as in-progress.

## What Already Exists

- `docker/oauth_mcp.py` — full PKCE flow already implemented, just needs secrets file read + defaults
- `OAUTH_REFRESH_TOKEN` bypass in `_check_auth` — just needs to also read from `/run/secrets/google.json`
- `agentctl/src/spawn.rs` — pattern for calling external process + writing files (reference for auth.rs)
- `~/.agentos-secrets/` convention — established in docs/plans/state-of-the-union-runtime.md

## Not In Scope

- p7.7 management HTTP API (dx.1 just opens port 7999, doesn't implement the API)
- dx.2 HTTP approval surface (dx.1 just opens port 8080)
- `agentctl auth` for other providers (only Google in dx.1)
- Moving `OAUTH_CLIENT_ID` / `OAUTH_CLIENT_SECRET` to secrets file (still env vars — needed for first-run provisioning before secrets exist)
- Linux QEMU secrets model (dx.3)

## Tests

- `agentctl`: `auth_google_creates_secrets_dir_if_missing`, `auth_google_rejects_overwrite_without_confirmation`, `auth_google_args_from_env_fallback`
- `oauth_mcp.py`: test_secrets_file_overrides_env_rt, test_google_defaults_applied, test_env_rt_still_works (backward compat)

## First-Run Walkthrough

```
# Prerequisites (one-time, human does this)
# 1. Go to console.cloud.google.com → APIs & Services → Credentials
# 2. Create an OAuth 2.0 Client ID (Desktop app type)
# 3. Add http://127.0.0.1:8585 to Authorized redirect URIs

# On host (one-time provisioning):
export ANTHROPIC_API_KEY=sk-ant-...
export OAUTH_CLIENT_ID=...     # from Google Cloud Console
export OAUTH_CLIENT_SECRET=... # from Google Cloud Console
agentctl auth google            # opens browser, writes ~/.agentos-secrets/google.json

# From now on, just:
docker compose up -d cos
docker compose logs -f cos      # watch the agent wake up
```

After `agentctl auth google`, `OAUTH_CLIENT_ID` and `OAUTH_CLIENT_SECRET` are in the secrets file and no longer needed in the shell.

## Error Message Specifications

### agentctl auth google — missing credentials
```
error: OAUTH_CLIENT_ID is not set
  Set it with: export OAUTH_CLIENT_ID=<your-client-id>
  Get your credentials: https://console.cloud.google.com/apis/credentials
  (Create a "Desktop app" OAuth 2.0 Client ID, add http://127.0.0.1:8585 as redirect URI)
```

### agentctl auth google — port in use
```
error: port 8585 is already in use
  Kill the conflicting process (lsof -i :8585) or use a different port:
    agentctl auth google --port 8686
  NOTE: if you change the port, also update the redirect URI in Google Cloud Console.
```

### agentctl auth google — browser open fails
```
Opening browser for Google authorization...
(If browser does not open, visit this URL manually:)
https://accounts.google.com/o/oauth2/v2/auth?...
Waiting for callback on http://127.0.0.1:8585 (timeout: 10 minutes)...
```
URL is always printed; `open`/`xdg-open` failure is a warning, not an error.

### agentctl auth google — token exchange failure
```
error: Google rejected the authorization: 401 {"error":"invalid_client"}
  Your OAUTH_CLIENT_SECRET may be wrong, or the code expired.
  Run 'agentctl auth google' again.
```

### agentctl auth google — timeout
```
error: Authorization timed out (10 minutes).
  The browser flow was not completed. Run 'agentctl auth google' to start again.
```

### agentctl auth google — overwrite confirmation
```
~/.agentos-secrets/google.json already exists. Overwrite? [y/N]
```
Default N. On non-TTY or stdin not a terminal: refuse and print:
```
error: ~/.agentos-secrets/google.json already exists. Use --force to overwrite.
```

### entrypoint.sh preflight (container startup, cos mode)
```
ERROR: Google credentials not provisioned.
  Run on your Mac (once):  agentctl auth google
  Then restart:            docker compose restart cos
```
Exit 1. This fires in `docker compose logs -f cos` before the agent wakes.

### oauth_mcp.py startup warning (fallback when preflight bypassed)
```
oauth_mcp: WARNING: /run/secrets/google.json not found and OAUTH_REFRESH_TOKEN unset
  Run: agentctl auth google
```

## CLI Spec — auth/google.rs

```
agentctl auth google [--client-id <id>] [--client-secret <secret>] [--port <port>] [--force]

Options:
  --client-id      OAuth client ID [env: OAUTH_CLIENT_ID]   (clap env attribute)
  --client-secret  OAuth client secret [env: OAUTH_CLIENT_SECRET]
  --port           Callback port (default: 8585)
  --force          Overwrite existing ~/.agentos-secrets/google.json without prompting
```

clap `env` attribute used for `--client-id` and `--client-secret` so env vars override but flags win.

Callback server implementation:
- `TcpListener::bind("127.0.0.1:{port}")?` — EADDRINUSE → print error with lsof tip + Google Console note
- Loop accepting connections:
  - Read from socket until `\r\n\r\n` (blank line ending headers)
  - Parse first line for `GET /?code=...&state=...`
  - If favicon or other non-callback: respond `HTTP/1.1 404 Not Found\r\n\r\n`, continue
  - If code= found: respond `HTTP/1.1 200 OK\r\n\r\n<html>Auth complete. Return to your terminal.</html>`, break
- Timeout: 10 minutes; on timeout print timeout error

Write path:
- Write to `~/.agentos-secrets/google.json.tmp`
- `chmod(tmp, 0o600)`
- `rename(tmp, google.json)` — atomic

google.json format:
```json
{"client_id": "...", "client_secret": "...", "refresh_token": "..."}
```

## Scope (updated)

### Additional scope items (from review)
- **entrypoint.sh**: add preflight check for `/run/secrets/google.json` in `cos` mode; exit 1 with clear error if missing
- **oauth_mcp.py `_load_config()`**: explicit precedence — read `/run/secrets/google.json` first (extract `client_id`, `client_secret`, `refresh_token`); env vars override if non-empty
- **oauth_mcp.py startup**: log warning if no secrets file and no `OAUTH_REFRESH_TOKEN` env var
- **docker-compose.yml cos service**: remove `OAUTH_CALLBACK_PORT` explicitly  
- **docker-compose.yml agent service**: update `OAUTH_REFRESH_TOKEN` comment to reference `agentctl auth google` as preferred path
- **`agentctl/src/auth/`** module with `google.rs` (not `auth.rs`)
- **Runbook/README**: update setup steps to new 4-step flow (get creds → agentctl auth → set ANTHROPIC_API_KEY → docker compose up)
- Port 7999:7999 and 8080:8080 bindings **deferred** to p7.7 and dx.2 respectively
- `~/.agentos-secrets/` is the locked secrets directory path (analogous to `~/.ssh/`)

## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| 1 | CEO | Include client_id + client_secret in google.json | Scope expansion | P1 | Complete the secrets story; after auth, zero OAuth env vars needed at runtime | client_id/secret stay env-vars-only |
| 2 | CEO | Remove OAUTH_CALLBACK_PORT from cos service | Mechanical | P5 | Callback runs on host after dx.1; dead config in compose | Keep for backward compat |
| 3 | CEO | Keep stdlib TcpListener (read to \r\n\r\n, loop for code=) | Mechanical | P5 | Sufficient for single-connection callback; no new deps | tiny_http crate |
| 4 | CEO | Lock ~/.agentos-secrets/ as secrets dir path | Mechanical | P3 | Analogous to ~/.ssh/; XDG deferred | ~/.agentos/secrets/ |
| 5 | CEO | Remove port bindings (7999/8080) from dx.1 | Mechanical | P5 | Network surface without implementation is sloppy; add in p7.7/dx.2 | Open ports now for p7.7 prep |
| 6 | CEO | Name module auth/google.rs, not auth.rs | Mechanical | P5 | Explicit naming; avoid premature provider abstraction | auth.rs with google hardcoded |
| 7 | Eng | _cfg global dict coupling | Defer | P3 | Existing debt; not dx.1's scope | Fix now |
| 8 | Eng | Add startup warning when secrets file absent and no env var | Include | P1 | Prevent silent failure; tell user what to do | Log only inside agent |
| 9 | Eng | Callback server: read to \r\n\r\n, loop for code=, send 200 HTML | Include | P1 | Browser sends favicon before callback; user needs confirmation | Single-line read |
| 10 | Eng | Pin google.json format: {client_id, client_secret, refresh_token} | Include | P1 | Consistent with CEO decision 1; AC requires zero OAuth env at runtime | {refresh_token} only |
| 11 | Eng | Explicit precedence: secrets file first, env var overrides if non-empty | Include | P5 | Prevents silent backward-compat break; must be specified not implied | Env first (breaks compat) |
| 12 | Eng | Atomic write: tmp + chmod 0600 + rename | Include | P1 | Crash between write and close leaves 0-byte file; same pattern as oauth_mcp.py | Direct write |
| 13 | Eng | Add --port flag to agentctl auth google | Include | P5 | Clear actionable error; port 8585 may be in use | Hard-code 8585 only |
| 14 | Eng | Document token rotation divergence | Document | P3 | Acceptable behavior (secrets file = initial provisioning); needs a comment | Try to sync files |
| 15 | Eng | Add one log line in oauth_start_auth for in-container-no-secrets path | Include | P1 | In-container OAuth flow intentionally broken after dx.1; should say so | Silent fail |
| 16 | DX | Add first-run walkthrough to plan | Include | P1 | Forces all ambiguities to resolve at plan time | Defer to README |
| 17 | DX | Specify exact error for missing OAUTH_CLIENT_ID/SECRET at agentctl level | Include | P5 | Actionable error with Google Console link | Generic "missing env" |
| 18 | DX | Document detached mode: `docker compose logs -f cos` follow-up | Include | P3 | Users see nothing in terminal after `up -d`; must know to check logs | Ignore |
| 19 | DX | Specify EADDRINUSE error including redirect URI note | Include | P5 | Port change requires Google Console redirect URI update (non-obvious) | Generic OS error |
| 20 | DX | Always print auth URL; browser open is best-effort | Include | P1 | Headless/SSH sessions; user must have the URL regardless | Only open browser |
| 21 | DX | Specify token exchange failure error message | Include | P5 | Actionable: tells user what to try (re-run, check secret) | Raw HTTP error |
| 22 | DX | Specify timeout feedback during wait | Include | P5 | 10-minute silent wait is confusing; "Waiting..." line + timeout message | Silent |
| 23 | DX | (TASTE) Two-level `auth google` vs flat `auth-google` | TASTE | — | Surfaced at final gate | — |
| 24 | DX | Use clap env attribute for client-id/client-secret | Include | P5 | Consistent with agentctl patterns; flag overrides env var | Manual env::var() |
| 25 | DX | Overwrite prompt [y/N] default N + --force flag | Include | P1 | Non-TTY scripted use needs --force; default N is safe | Just overwrite |
| 26 | DX | Add runbook/README update to scope | Include | P1 | User-facing docs must reflect new 4-step setup flow | Skip docs |
| 27 | DX | Defer ~/.agentos-secrets/ vs ~/.agentos/ naming | Defer | P3 | Decision locked in state-of-the-union; migration cost too high now | Rename now |
| 28 | DX | Add entrypoint.sh preflight check for /run/secrets/google.json | Include | P1 | Critical: prevents silent fail at Gmail call time; exit 1 immediately | Rely on oauth_mcp warning |
| 29 | DX | Specify updated comment for agent service OAUTH_REFRESH_TOKEN | Include | P5 | Prevents misleading "extract from container" comment after dx.1 | Leave as-is |
| 30 | Gate | Two-level `agentctl auth google` (USER DECISION) | Taste | User | gh CLI convention; scales to future providers; arg_required_else_help on auth group | Flat auth-google |

## GSTACK REVIEW REPORT

/autoplan complete. 3 phases ran (CEO, Eng, DX). Phase 2 (Design) skipped — no UI scope.

- CEO: 6 findings, 6 auto-decided. No user challenges.
- Eng: 9 findings, 9 auto-decided (7 include, 2 defer/document).
- DX: 15 findings, 14 auto-decided, 1 taste decision (surfaced at gate, user chose A).
- Total decisions: 30 (29 auto + 1 user).
- Cross-phase themes: 2 (error message completeness, silent failure prevention).
- Blockers resolved: entrypoint.sh preflight (CRITICAL), callback robustness (HIGH), 
  google.json 3-key format (MEDIUM), explicit precedence (MEDIUM), always-print URL (HIGH).
- Test plan: ~/.gstack/projects/0x89karan-runtime1/main-dx1-test-plan-*.md (28 tests)
