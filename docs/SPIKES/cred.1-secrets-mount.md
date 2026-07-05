<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260705-183926.md -->
# cred.1 — Secrets mount + README fix (Mac unblock)

**Status:** Implemented — branch `cred.1-secrets-mount`, base v0.57.0
**Branch:** cred.1-secrets-mount
**Version target:** v0.58.0
**Depends on:** nothing (ships first in the cred.* track)
**Design doc:** `docs/plans/credential-manager.md` § cred.1

---

## Goal

Unblock the Mac-native Docker `agent` service for OAuth-gated templates (currently
`google-agent`). Today `docker compose run --rm agent TEMPLATE_NAME=google-agent` fails
silently or falls back to in-container OAuth because the `agent` service has no secrets
volume — even though `agentctl auth google` already wrote credentials to
`~/.agentos-secrets/google.json` on the host. Three fixes, no new Rust code.

---

## Root cause

`cos` gets `${HOME}/.agentos-secrets:/run/secrets:ro` in `docker-compose.yml`; `agent`
does not. Consequence: `oauth_mcp.py` finds no `/run/secrets/google.json` and falls
back to the in-container OAuth flow (or `OAUTH_REFRESH_TOKEN` env var), which requires
port forwarding and manual credential management — defeating the `agentctl auth google`
setup step.

Additionally, `README.md` line 130–131 makes a false claim:

> "The container never sees your OAuth client credentials directly — only the refresh
> token written by the one-time setup step."

`write_secrets_file()` in `agentctl/src/auth/google.rs` stores **client_id +
client_secret + refresh_token** in `google.json`. The file contains full OAuth client
credentials, not just the refresh token.

---

## Scope

### In scope

1. **`docker-compose.yml`** — add `- ${HOME}/.agentos-secrets:/run/secrets:ro` to
   the `agent` service (mirror `cos`). Strip the stale `OAUTH_CLIENT_ID`,
   `OAUTH_CLIENT_SECRET`, `OAUTH_REFRESH_TOKEN`, and `OAUTH_CALLBACK_PORT` env
   passthrough lines from the `agent` service (these are the in-container OAuth vars
   that cred.1 deprecates; they will be gated in cred.2 with a formal deprecation
   notice). Actually — **do not strip them yet**: the design doc specifies that cred.2
   handles deprecation. cred.1 only adds the mount. Keep the env passthrough lines so
   as not to break users who relied on them before cred.1.
   
   Final change: add exactly one line to `agent.volumes`:
   ```yaml
   - ${HOME}/.agentos-secrets:/run/secrets:ro
   ```

2. **`README.md`** — two fixes:

   **2a.** Fix lines 129–131 (false claim): replace with accurate text reflecting that
   `google.json` contains `client_id`, `client_secret`, and `refresh_token`; both
   `cos` and `agent` services mount it read-only after cred.1.

   **2b.** Add `mkdir -p ~/.agentos-secrets` as step 1 in the "One-time setup" block
   (currently line 107), before `agentctl auth google`. The CLI writes
   `~/.agentos-secrets/google.json` via an atomic tmp→rename; if the directory does
   not exist, the write fails. First-timers have no indication to create it.

3. **`docker/entrypoint.sh`** — two preflight fixes:

   **3a. `agent` mode** — add fail-fast preflight for OAuth-gated templates. When
   `TEMPLATE_NAME` is `google-agent`, check that `/run/secrets/google.json` exists
   OR that the env-var fallback is complete. Fail with a clear, actionable error.

   Added as a new arm in the **existing** `case "$TEMPLATE" in` block (lines 122-131).

   ```bash
      google-agent)
        # TODO cred.3: replace with broker startup validation (generic preflight from gated_requires)
        # Fail only when BOTH the secrets file is absent AND the env-var fallback is incomplete.
        # Users who set OAUTH_CLIENT_ID + OAUTH_CLIENT_SECRET + OAUTH_REFRESH_TOKEN still work.
        if [ ! -s /run/secrets/google.json ] && {
          [ -z "${OAUTH_CLIENT_ID:-}" ] ||
          [ -z "${OAUTH_CLIENT_SECRET:-}" ] ||
          [ -z "${OAUTH_REFRESH_TOKEN:-}" ]
        }; then
          if [ ! -d /run/secrets ]; then
            echo ""
            echo "  ERROR: Secrets volume not mounted."
            echo ""
            echo "  The docker-compose.yml agent service mounts ~/.agentos-secrets."
            echo "  If running with 'docker run', add: -v ~/.agentos-secrets:/run/secrets:ro"
            echo ""
          else
            echo ""
            echo "  ERROR: Google credentials not provisioned."
            echo ""
            echo "  Run on your Mac (once):"
            echo "    agentctl auth google \\"
            echo "      --client-id YOUR_CLIENT_ID \\"
            echo "      --client-secret YOUR_CLIENT_SECRET"
            echo ""
            echo "  Then re-run:"
            echo "    docker compose run --rm agent"
            echo ""
          fi
          exit 1
        fi
        ;;
   ```

   Note: `-s` checks file exists AND is non-empty. Two-branch error distinguishes
   "volume not mounted" from "file absent". Env-var fallback preserves backwards compat.

   **3b. `cos` mode** — fix pre-existing bug (dx.1 regression): the cos preflight
   (line 95) prints `agentctl auth google` without `--client-id`/`--client-secret`
   flags. A first-timer following this error immediately hits a CLI error. Since
   cred.1 already touches `entrypoint.sh`, fix it here:

   ```bash
   # Change line 95 from:
   echo "    agentctl auth google"
   # To:
   echo "    agentctl auth google \\"
   echo "      --client-id YOUR_CLIENT_ID \\"
   echo "      --client-secret YOUR_CLIENT_SECRET"
   ```

4. **`CHANGELOG.md`** — add cred.1 entry under v0.58.0.

5. **`docs/ROADMAP.md`** — check off cred.1.

### Out of scope

- Stripping in-container OAuth env vars (`OAUTH_CLIENT_ID` etc.) — that is cred.2
  (deprecation + unified secrets substrate).
- Adding `agentos.env` sourcing to the entrypoint — cred.2.
- `CredentialBroker`, `CredentialStore` trait, any Rust code — cred.3+.
- QEMU secrets mount being read-only — cred.2.
- Rewriting `RUNBOOK.md` — cred.2.
- `journaler` / `memory-custodian` preflight (already handled with a `requires /run/memory`
  error case in the `agent` mode switch).

---

## Implementation details

### 1. `docker-compose.yml` — add secrets volume to `agent`

Under the `agent.volumes` key (currently only `- agent-data:/data` and a commented
workspace bind), add:

```yaml
      # Secrets provisioned by `agentctl auth google` on the Mac host (read-only)
      - ${HOME}/.agentos-secrets:/run/secrets:ro
```

Position: directly below the `agent-data:/data` line, above the commented workspace
bind. This matches `cos` layout exactly.

The comment text from `cos` is copied verbatim so future readers can grep for it.

Also update the existing comment on the `HOME=/data` env line (currently references the
wrong path `~/.agentos-oauth/google.json`; also falsely implies in-container token
refresh works):
```yaml
      # HOME=/data: persists checkpoint/flight files on the named volume across --rm runs.
      # NOTE: agentctl auth google must run on the Mac host (real $HOME), not the container.
      # Token refresh write-back is blocked by the :ro secrets mount (see cred.1-ki-01).
      - HOME=/data
```

### 2. `README.md` — two changes

**2a. Fix the false claim** — lines 129–131 currently read:
```
`~/.agentos-secrets/google.json` is mounted at `/run/secrets` inside the
container (read-only). The container never sees your OAuth client credentials
directly — only the refresh token written by the one-time setup step.
```

Replace with accurate, scope-bounded text:
```
Credentials provisioned by `agentctl auth google` are stored as
`~/.agentos-secrets/google.json` (containing `client_id`, `client_secret`, and
`refresh_token`) and mounted read-only at `/run/secrets` in the provided
`docker-compose.yml` services. The mount is `:ro` — the container cannot
modify the file.
```

**2b. Add `mkdir -p` as step 1** — in the "One-time setup" block (line 107), insert
before the `agentctl auth google` step:
```bash
# 1. Create the secrets directory on your Mac host (one time)
mkdir -p ~/.agentos-secrets
```
Renumber remaining steps accordingly (current step 1 → step 2, etc.).

### 3. `docker/entrypoint.sh` — Two entrypoint fixes

**3a. `agent` mode — OAuth preflight (new arm)**

Merge the OAuth preflight into the **existing** `case "$TEMPLATE" in` block at lines
122-131 (which already handles `code-aware|langchain-worker` and
`journaler|memory-custodian`). Do NOT introduce a second `case` block.

Add a new `google-agent)` arm immediately before `esac`:
```bash
      google-agent)
        # TODO cred.3: replace with broker startup validation (generic preflight from gated_requires)
        # Fail only when BOTH the secrets file is absent AND the env-var fallback is incomplete.
        # Users who set OAUTH_CLIENT_ID + OAUTH_CLIENT_SECRET + OAUTH_REFRESH_TOKEN still work.
        if [ ! -s /run/secrets/google.json ] && {
          [ -z "${OAUTH_CLIENT_ID:-}" ] ||
          [ -z "${OAUTH_CLIENT_SECRET:-}" ] ||
          [ -z "${OAUTH_REFRESH_TOKEN:-}" ]
        }; then
          if [ ! -d /run/secrets ]; then
            echo ""
            echo "  ERROR: Secrets volume not mounted."
            echo ""
            echo "  The docker-compose.yml agent service mounts ~/.agentos-secrets."
            echo "  If running with 'docker run', add: -v ~/.agentos-secrets:/run/secrets:ro"
            echo ""
          else
            echo ""
            echo "  ERROR: Google credentials not provisioned."
            echo ""
            echo "  Run on your Mac (once):"
            echo "    agentctl auth google \\"
            echo "      --client-id YOUR_CLIENT_ID \\"
            echo "      --client-secret YOUR_CLIENT_SECRET"
            echo ""
            echo "  Then re-run:"
            echo "    docker compose run --rm agent"
            echo ""
          fi
          exit 1
        fi
        ;;
```

The TODO comment makes the future removal intentional (cred.3 will drive preflights
generically from `gated_requires` in the template TOML instead of hardcoded names).

**3b. `cos` mode — fix truncated preflight command (dx.1 regression)**

`entrypoint.sh` line 95 currently prints `agentctl auth google` without flags.
A first-timer following the error immediately hits a CLI usage error with no progress.

Replace lines 94-96:
```bash
# From:
      echo "  Run on your Mac (once):"
      echo "    agentctl auth google"
      echo ""
# To:
      echo "  Run on your Mac (once):"
      echo "    agentctl auth google \\"
      echo "      --client-id YOUR_CLIENT_ID \\"
      echo "      --client-secret YOUR_CLIENT_SECRET"
      echo ""
```

### 4. No Rust changes

Zero changes to any `.rs` file. The Rust test count stays at 1062. This is a pure
Docker / docs increment.

---

## Tests

Manual acceptance test (the only meaningful test for this increment):

```bash
# 0. Create secrets dir (if first time)
mkdir -p ~/.agentos-secrets

# 1. Ensure credentials are provisioned on the Mac host
ls ~/.agentos-secrets/google.json   # must exist

# 2. Build a fresh image
docker compose build agent

# 3. Smoke: scout (no OAuth) still works
TEMPLATE_NAME=scout AGENT_TASK="What is 2+2?" docker compose run --rm agent

# 4a. Preflight fires correctly when google.json absent AND no env-var fallback
mkdir -p /tmp/empty-secrets
docker compose run --rm \
  -v /tmp/empty-secrets:/run/secrets:ro \
  -e TEMPLATE_NAME=google-agent \
  -e AGENT_TASK="test" \
  agent
# Expected: exits with "ERROR: Google credentials not provisioned."

# 4b. Env-var fallback still works (backwards compat — env vars present, no file)
# NOTE: the preflight passes because all three env vars are set — the env-var fallback
# branch is satisfied. DRY_RUN_ONLY=1 does NOT bypass the preflight; it just controls
# whether agentd actually runs after the preflight succeeds.
TEMPLATE_NAME=google-agent \
  AGENT_TASK="test" \
  OAUTH_CLIENT_ID=cid \
  OAUTH_CLIENT_SECRET=cs \
  OAUTH_REFRESH_TOKEN=rt \
  DRY_RUN_ONLY=1 \
  docker compose run --rm \
  -v /tmp/empty-secrets:/run/secrets:ro \
  agent
# Expected: prints rendered TOML and exits 0 (preflight passed via env-var fallback)

# 5. google-agent succeeds with real credentials (secrets volume mounted)
TEMPLATE_NAME=google-agent AGENT_TASK="List my 3 most recent unread Gmail subjects" \
  docker compose run --rm agent
```

No new Rust unit tests needed. The `entrypoint.sh` preflight runs before `DRY_RUN_ONLY`
is checked, so `DRY_RUN_ONLY=1` does NOT bypass the preflight for `google-agent` without
credentials — that is intentional and tested in step 4b with env-var fallback.

---

## Acceptance criteria

- `docker compose run --rm agent` with `TEMPLATE_NAME=google-agent` and a valid
  `~/.agentos-secrets/google.json` runs the agent without manual OAuth setup.
- Same run with credentials absent (or secrets dir not mounted) prints a clear,
  actionable `agentctl auth google` error and exits 1.
- Env-var fallback (`OAUTH_CLIENT_ID` + `OAUTH_CLIENT_SECRET` + `OAUTH_REFRESH_TOKEN`)
  still works without `google.json` present (backwards compat for existing users).
- `docker compose run --rm agent` with `TEMPLATE_NAME=scout` is unaffected.
- `cos` service behavior is unchanged.
- `cos` mode preflight now prints the full `agentctl auth google --client-id ... --client-secret ...` command.
- README quickstart includes `mkdir -p ~/.agentos-secrets` as step 1.
- README no longer contains the false "never sees your OAuth client credentials" claim.
- `cargo test` still passes (no Rust changes, count stays at 1062).

---

## Architecture — component relationship

```
Mac host
  ~/.agentos-secrets/google.json  (client_id, client_secret, refresh_token)
         │  :ro bind mount (cred.1 adds this to `agent` service)
         ▼
  Docker container /run/secrets/google.json
         │  read by
         ▼
  docker/oauth_mcp.py  ← MCP server (stdio, spawned by agentd)
         │  OAuth API calls
         ▼
  Google APIs (Gmail, Drive)

Preflight (new in cred.1):
  entrypoint.sh agent mode
    case $TEMPLATE in
      google-agent) → check /run/secrets/google.json OR env-var fallback
                     → fail fast with actionable error if neither present
    esac
  ↓
  agentctl spawn $TEMPLATE --task "$TASK" --dry-run
```

No new components. cred.1 wires an existing pattern (`cos` service) to the `agent` service.

## Known issues deferred to later increments

- **cred.1-ki-01:** `:ro` mount does **not** block token refresh write-back. `oauth_mcp.py` reads credentials from `/run/secrets/google.json` at startup and writes updated refresh tokens to `~/.agentos-oauth/google.json` (inside the container, on the writable named volume at `/data`). The actual limitation is: if Google rotates the refresh token and the user wants to update the canonical on-host credentials, they must re-run `agentctl auth google --force` on the Mac host. This is narrow; access tokens typically live 1h. Also: the `:ro` annotation protects against accidental writes but does not prevent a privileged container from remounting the bind mount read-write (tracked as a known boundary in the security model).
- **cred.1-ki-02:** `${HOME}` expansion in compose volume path is OS-dependent. On Linux CI runners `$HOME` may be `/root`. This is a pre-existing bug in the `cos` service; cred.1 inherits it. Fix in cred.2 (unified secrets substrate).

## Risk

**Low.** All three changes are additive and strictly Docker / docs:
- Adding a volume to `agent` cannot break the existing `cos` service.
- The entrypoint preflight is a no-op for all non-OAuth templates.
- The README fix is documentation only.
- Failure mode: user has `~/.agentos-secrets/` but no `google.json`. The new preflight
  catches this and tells them what to do. Before cred.1, the same scenario silently
  fell through to in-container OAuth (worse UX).
