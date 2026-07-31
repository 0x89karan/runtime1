# AgentOS Deployment Guide

Two paths: **Mac + Docker** (quickest) and **Linux QEMU** (production, KVM-accelerated).
Both run the same Chief of Staff agent and speak the same `agentctl watch` surface.

---

## Dev image — fast local loop (contributors)

Skip the published image and build locally in ~15 min first run, ~2 min subsequent
(cargo registry cache hit). **This is the inner-loop workflow for contributors
iterating on agentd/agentctl.** Onboarding and first-run TTHW are unchanged.

```bash
# Build the full image (Python MCP + Rust binaries) — for the CoS dogfood path
make dev-image          # → agentos:dev (native arm64 on Apple Silicon, no QEMU)

# Build the Rust-only core image — faster, for agentd/agentctl-only changes
make dev-image-core     # → agentos:dev-core
# NOTE: the cos and agent compose services need the full image (Python MCP harness).
# Use dev-image-core only for custom TOML setups that don't use standard MCP servers.

# Run the CoS using your local image (no env var needed — agentos:dev is the default)
docker compose up cos

# Or run a named template against your local image
docker compose run --rm agent
```

**If you have a pre-built published image and want to force it:**

```bash
AGENTOS_IMAGE=ghcr.io/0x89karan/runtime1:full docker compose up cos
```

**Tag glossary:**

| Tag | Source | When to use |
|-----|--------|-------------|
| `agentos:dev` | `make dev-image` (local) | Daily inner loop on your machine |
| `agentos:dev-core` | `make dev-image-core` (local) | Rust-only changes, no Python MCP |
| `ghcr.io/…:full` | Published multi-arch (`workflow_dispatch` or `v*` tag) | Pull for production |
| `ghcr.io/…:latest` | Same as `:full`, most recent publish | Convenience alias |

**Notes:**
- `docker compose pull` fails for local image names — use `make dev-image` instead.
- `AGENTOS_IMAGE` must be unset or a non-empty string; `AGENTOS_IMAGE=` (empty) is an error.
- If `AGENTOS_IMAGE` names an image that doesn't exist locally, Compose builds it from source.
  Run `make dev-image` first to avoid a surprise 15-min build.
- If a build gets stuck or produces stale layers, clear the BuildKit cache: `docker builder prune`.
- **BREAKING (ux.9, v0.82.0+):** the image's zero-arg default changed from `shell` to `cockpit` —
  a bare `docker run agentos:dev` now cold-starts `agentd` with zero agents and attaches
  `agentctl watch`, instead of dropping into a bash shell. This does NOT affect
  `docker compose up cos` / `docker compose run --rm agent` — both set an explicit `command:`
  in `docker-compose.yml` that overrides the image default regardless of what it is (verify
  with `make compose-config-check`). To get the old shell behavior back: `docker run -it
  agentos:dev shell`.

## Cutting a release image

Published images are **not** built on every merge to `main`. To publish:

```bash
# Option A — manual dispatch (from the Actions UI, must be on main branch)
# Go to: GitHub → Actions → CI → Run workflow → main

# Option B — push a version tag (must match agentd/Cargo.toml and exceed
# every prior v* tag — see the release guards below)
git tag vX.Y.Z
git push origin vX.Y.Z
# The CI workflow builds linux/amd64 + linux/arm64 and pushes :full, :latest, :vX.Y.Z
```

### Release guards (ci.1) — what a refused publish means

Every publish path runs `scripts/release-guard.sh` first. A refusal names its
invariant; the exits:

| Refusal | Meaning | Your exit |
|---|---|---|
| **tag not on main** | The tagged commit isn't an ancestor of `origin/main` (the v0.86.0 staleness class). | If the commit IS on main (tag raced the branch push): re-run the job. Otherwise delete the tag, re-tag the intended main commit, push. |
| **version mismatch** | `vX.Y.Z` ≠ `agentd/Cargo.toml`. | Bump Cargo.toml (and CLAUDE.md's version line — test-enforced) and re-tag, or re-tag with the Cargo version. |
| **non-monotonic tag** | The version isn't newer than every other `v*` tag — it would re-point `:latest` backwards. | Choose a version above the named one. |
| **version already published** | This caller's artifact for `vX.Y.Z` already exists (checked on tag pushes AND `workflow_dispatch` — an unbumped dispatch must not overwrite a published version). | Never republish a used version. To redo intentionally: delete BOTH the GitHub release/tag AND the ghcr manifest, then re-tag. Otherwise bump. |
| **probe failed (fail-closed)** | The reuse probe could not get an explicit not-found verdict (auth failure, rate limit, network error, missing ghcr login). The guard refuses rather than publish blind. | Fix the probe's environment (the error output names it) and re-run the job — nothing was published. |

Tag format is strictly `vMAJOR.MINOR.PATCH` (no prereleases). Both workflows
run on every tag push; each one's guard probes **only its own artifact**
(`--check release` in release.yml, `--check image` in ci.yml publish-docker) —
otherwise each would refuse on seeing the other's freshly created output.
publish-docker additionally re-runs the guard as `--check image-prepush` inside
its concurrency group just before the pushes (closing the concurrent-publish
race); that mode refuses only a **complete** published manifest, so a partial
manifest left by a crashed prior run stays repairable with a re-run.
Consequence: after a successful publish, **"Re-run all jobs" correctly refuses**
(the artifact now exists) — retry a flaked downstream job with **"Re-run failed
jobs"**, which skips the already-green guard job (in BOTH workflows the guard is
its own job for exactly this reason). Also note:

- A `workflow_dispatch` publish pushes `:vCARGO_VERSION` images and thereby
  **consumes that version for images** — tagging the same version later
  half-publishes (Release created, images refused). Bump before tagging.
- **Versioning is strictly linear** (deliberate: single-user OS, `:latest`
  re-points on every release). A backport tag like v0.87.1 after v0.88.0
  exists is refused by monotonicity — there is no override path.
- **Don't push two release tags in close succession**: release.yml finishes in
  minutes but publish-docker trails ~35–50 min behind `build-aarch64`, so a
  newer tag arriving in that window makes the older tag's image publish refuse
  on monotonicity (Release + binaries exist, images don't). Space tags by an
  hour, or re-run the older publish after deleting the newer tag if the order
  was a mistake.

### Required status checks (branch protection — operator ops)

Merges are gated by branch protection, not by workflows merely running. The
load-bearing check names are `build-and-test`, `docker-smoke`,
`sidecar-tests`, and `harness-tests` — **renaming a job silently un-gates it**
until this setting is updated. Set (or restore after a flaky-check removal)
with:

```bash
gh api -X PUT repos/0x89karan/runtime1/branches/main/protection/required_status_checks/contexts \
  --input - <<< '["build-and-test","docker-smoke","sidecar-tests","harness-tests"]'
```

Remove a flaking check temporarily (ci.1 flake policy: a check that flakes twice
in a week drops from required pending a fix — a bypassed red check is worse than
none) by PUTting the list without it, then restore.

---

## Path 1 — Mac + Docker

> **Canonical source for the Mac + Docker quickstart.** The rendered guide
> `docs/cos-guide.html` (published at
> https://claude.ai/code/artifact/936e816a-d052-4799-85ff-8acfe71ee544) is a view of this section —
> **when these steps change, update `docs/cos-guide.html` and redeploy the artifact** so the two do
> not drift.

**Prerequisites:** Docker Desktop · Anthropic API key (`sk-ant-…`) · Google Cloud account (free tier)
· Rust + Cargo (macOS has no prebuilt `agentctl`, so you build it once; Linux can download the
release binary instead).

### One-time setup

```bash
# 1. Build agentctl (needed on your Mac to run the Google OAuth flow)
cargo build --release --bin agentctl
sudo cp target/release/agentctl /usr/local/bin/agentctl   # optional: add to PATH

# 2. Google OAuth — console.cloud.google.com → enable the Gmail API →
#    Create Credentials → OAuth client ID → Desktop app → copy the Client ID + Secret.
#    NOTE: a Desktop-app client has NO "redirect URI" field — 127.0.0.1 loopback is
#    allowed automatically (RFC 8252). agentctl uses port 8585; it must be free, or
#    pass `agentctl auth google --port <N>`. (Nothing to register in the console.)
#    ⚠️  Publish to Production (OAuth consent screen → Publish App) — Testing mode
#    tokens expire after 7 days causing silent auth failures. gmail.readonly doesn't
#    require Google verification; just accept the "unverified app" warning.

# 3. Authorize Gmail (a browser opens; writes ~/.agentos-secrets/google.json)
agentctl auth google \
  --client-id     "YOUR_CLIENT_ID.apps.googleusercontent.com" \
  --client-secret "YOUR_CLIENT_SECRET"

# 4. Host directories (runtime state + brief output)
mkdir -p ~/.agentos-output ~/.agentos-data
```

### Run

```bash
# 5. Pull the published image (agentos:full bundles the MCP servers + OAuth sidecar the CoS needs)
docker pull ghcr.io/0x89karan/runtime1:full

# 6. Start the CoS — keep the `export` and `docker run` in the SAME terminal
export ANTHROPIC_API_KEY=sk-ant-...
export OPENAI_API_KEY=sk-...          # required for semantic KB (email body embeddings)
docker run --rm -it \
  --name agentos-cos \
  --privileged \
  -p 127.0.0.1:7999:7999 \
  -e ANTHROPIC_API_KEY \
  -e OPENAI_API_KEY \
  -e "TRIGGER_INTERVAL=every 2m" \
  -v ~/.agentos-secrets:/run/secrets:ro \
  -v ~/.agentos-output:/data/output \
  -v ~/.agentos-data:/data \
  ghcr.io/0x89karan/runtime1:full cos

# 7. Watch the agents (second terminal, directly from the Mac host — no docker exec needed)
agentctl watch --url http://localhost:7999

# 8. Read the brief (after the first cycle, ~2-3 min)
cat ~/.agentos-output/brief-$(date +%Y-%m-%d).md
```

**Reading the brief:** the `Thread` column links straight to each Gmail conversation, and a literal
dash means the thread id was missing or malformed (fail-closed by design). **If the brief starts with
`⚠ Shortened to fit`, it is incomplete** — a morning's mail exceeded the 8 KiB store limit, so the
inbox job shed content and told you rather than producing nothing. Check Gmail for anything
time-critical. Full anatomy and the known limitations (handled items can reappear; sender-text
escaping is a prompt rule, not enforcement) are in `docs/RUNBOOK.md` §11.6.

**Schedule:** `TRIGGER_INTERVAL="every 2m"` is for testing and only accepts `every N(s|m|h)`. For a
daily brief use the **separate** cron variable: `-e "TRIGGER_CRON=0 8 * * *"` (08:00 UTC) — a cron
expression in `TRIGGER_INTERVAL` will not parse.

**API key across shells:** `-e ANTHROPIC_API_KEY` forwards it from the shell you `export`ed in. Run
`docker run` in that same terminal, or put `ANTHROPIC_API_KEY=sk-ant-...` in
`~/.agentos-secrets/agentos.env` (the entrypoint sources it) so it survives new terminals.

**Talk to it interactively** instead of the autonomous cron brief:
`docker run --rm -it -e ANTHROPIC_API_KEY ghcr.io/0x89karan/runtime1:full orchestrate`.

**Stopping:** `Ctrl+C` checkpoints gracefully; state persists in `~/.agentos-data`, so the next run
resumes.

### Telegram reach (ux.12) — optional: brief + approve/deny from your phone

Enable the two-way Telegram bridge to receive the morning brief and approve/deny pending actions
from your phone. Omit these vars and the CoS runs exactly as above (the TUI stays canonical).

```bash
# 1. Create a bot: message @BotFather on Telegram → /newbot → copy the token.
# 2. Get your numeric user id: message @userinfobot → it replies with your id.
# 3. Pick a strong random approval secret (gates the approve/deny routes; see THREAT_MODEL §9.6):
export AGENTOS_APPROVAL_SECRET="$(openssl rand -hex 32)"
export TELEGRAM_BOT_TOKEN="123456:ABC-..."      # from BotFather (secret — never commit/log)
export TELEGRAM_CHAT_ID="123456789"             # your numeric user id (private chat only)

# 4. Add the -e flags to the `docker run` above (or the compose env — see docker-compose.yml):
#      -e TELEGRAM_BOT_TOKEN -e TELEGRAM_CHAT_ID -e AGENTOS_APPROVAL_SECRET
```

Notes: the bot token is the crown jewel — env-only, never logged; a leaked token can read your
brief/approval text but cannot *approve* (that needs the separate `AGENTOS_APPROVAL_SECRET` and your
`from.id`). Set `AGENTOS_APPROVAL_SECRET` whenever Telegram is on — without it the approve/deny
routes are unauthenticated on the Docker bridge (THREAT_MODEL §9.2/§9.6). Reply `approve <id>` or
`deny <id> [reason]` to a pushed approval. If the bridge or Telegram is down, nothing blocks —
approvals just stay pending in the TUI.

**⚠ If you set `AGENTOS_APPROVAL_SECRET`, host-side `agentctl` needs it too.** The gate is not just
approve/deny: cap.4 put it on the whole **mutating** surface — every `POST`, which today means
`/spawn`, `/agents/:id/inject`, `/agents/:id/cancel`, `/agents/:id/caps`, `/budget/set`,
`/budget/reset`, and `/credentials/:provider/reset-attention`
(`management.rs`'s `is_mutating_route`). So `agentctl watch --url …`, `agentctl approve --url …`,
`agentctl cancel --url …` and the cockpit's `[x]` row actions run from your Mac all need
`AGENTOS_APPROVAL_SECRET` exported in *that* shell, or they get `HTTP 401` (the TUI says: "Action
refused: approval token missing or wrong … Export the same AGENTOS_APPROVAL_SECRET used by agentd,
then restart agentctl watch"). Read-only routes (`/snapshot`, `/healthz`, `/brief`, `/runs`) are
ungated. (FUSE-mode `agentctl watch` inside the container is unaffected — it writes the
`/agents/control` file, which is deliberately not gated, since `:7999` is the boundary that is.)

**Logs & receipts:** inspect `~/.agentos-data/flight.jsonl` with `jq`; verify the signed receipt chain
with `agentctl verify ~/.agentos-data/evidence.jsonl ~/.agentos-data/egress-key.pub` (it takes **two**
arguments — this was documented with one and failed as printed until ux.6a). The chain covers **model
calls only**, and verifies against a locally-held key; see `THREAT_MODEL.md` §8.7. Once the chain passes
32 MiB it rotates to `evidence.jsonl.1` … `.3`; each segment verifies independently with the same
command.

**Privacy note — email embeddings:** The CoS uses OpenAI's Embeddings API
(`text-embedding-3-small`) to store email bodies as semantic vectors in Qdrant. Plain-text
email body content (up to 8 KB per message) is transmitted to OpenAI for vectorisation.
Review [OpenAI's data usage policies](https://openai.com/policies/api-data-usage-policies)
before enabling the semantic KB on inboxes that contain sensitive content. To run without
OpenAI (L1 BM25 only), comment out the `[tools.mcp_servers]` semantic-kb block in
`cos.agents.toml` — the CoS will operate without email dedup and semantic search.

For building from source and iterating locally, see **Dev image** at the top of this guide.

### Updating & clean re-test

The image is the code — a running container never updates itself, and a restart **resumes prior
agents from the checkpoint** (by design; the CoS survives restarts). After pulling a fix, start fresh
so a failed agent from an earlier run doesn't reload:

```bash
docker rm -f agentos-cos 2>/dev/null || true              # stop the old container (--rm cleans on exit)
rm -f ~/.agentos-data/checkpoint.json 2>/dev/null || true # drop stale/failed agents (usually already gone)
docker pull ghcr.io/0x89karan/runtime1:full             # get the newest build
# then re-run step 6
```

- **Started with `docker compose`?** Stop with `docker compose down` (add `-v` only to also wipe the
  named volumes — that's a data reset).
- **Full reset** (only if a run *still* reloads stale state): `rm -rf ~/.agentos-data/*` regenerates
  the memory store, flight log, and signing keys. Google credentials live in `~/.agentos-secrets` and
  are **not** touched.
- **Clean up old images:** `docker image ls ghcr.io/0x89karan/runtime1`, then `docker image prune`
  or `docker rmi <id>`. Pin an exact tag (`:v0.73.2`) instead of `:full`/`:latest` for a reproducible
  build.

### Troubleshooting

- **Inbox agent stuck at "Not authenticated" / `no_session` (with a valid `google.json`):** your image
  predates the **v0.73.2** Gmail-auth fix (older images never refresh the stored token). Re-pull
  `agentos:full` (see *Updating & clean re-test* above) and restart.
- **Container exits with "ANTHROPIC_API_KEY is not set":** the key wasn't in the shell that ran
  `docker run`. Re-`export` it here, or use the `agentos.env` approach above.
- **Browser shows `ERR_CONNECTION_REFUSED` during auth:** port `8585` was already in use when
  `agentctl auth google` started, so the loopback callback couldn't bind. Re-run with `--port
  <free-port>` — Desktop clients accept any loopback port; there is nothing to register.
- **No brief after a few minutes:** check the cron fired and Gmail was reached —
  `jq 'select(.kind=="tool_call")' ~/.agentos-data/flight.jsonl | tail`.
- **Can't `docker exec` to watch:** the container must have been started with `--name agentos-cos`.

---

## Path 2 — Linux QEMU

**Prerequisites:** `qemu-system-x86_64`, `/dev/kvm` accessible (x86_64 only)

### Fast path — download prebuilt images (recommended)

No build toolchain required. Installs the latest release in ~2 min.

```bash
# Step 0 — install agentctl (needed for Google auth)
TAG=$(curl -fsSL https://api.github.com/repos/0x89karan/runtime1/releases/latest \
  | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/' | head -1)
curl -fsSL -o /usr/local/bin/agentctl \
  "https://github.com/0x89karan/runtime1/releases/download/${TAG}/agentctl-${TAG}-x86_64-linux-musl"
chmod +x /usr/local/bin/agentctl

# Step 1 — install prebuilt images
curl -fsSL https://github.com/0x89karan/runtime1/releases/latest/download/install.sh -o install.sh
# Review the script before running it:
bash install.sh
```

If verification fails, re-run the `bash install.sh` command. Do not proceed if SHA256 fails.

Skip to **Step 2** (create user) after install completes.

### Slow path — build from source (developer option)

```bash
git clone https://github.com/0x89karan/runtime1.git
cd agentos/distro
make build   # downloads Buildroot, compiles kernel + rootfs; ~30 min first time
             # ccache cuts subsequent rebuilds to ~2 min
sudo mkdir -p /opt/agentos
sudo cp output/bzImage output/rootfs.cpio.gz /opt/agentos/
```

### Step 2 — Create the system user

```bash
sudo useradd -m -r -s /usr/sbin/nologin agentos
sudo mkdir -p /home/agentos/.agentos-secrets
```

### Step 3 — Provision secrets

Create `/home/agentos/.agentos-secrets/agentos.env` with **all** of the following:

```bash
# Required: Anthropic API access
ANTHROPIC_API_KEY=sk-ant-...

# Required: Google OAuth app (create at console.cloud.google.com)
# ⚠️  Publish to Production (OAuth consent screen → Publish App) to avoid 7-day token expiry.
# gmail.readonly doesn't require Google verification — accept the "unverified" warning.
OAUTH_CLIENT_ID=<your-client-id>.apps.googleusercontent.com
OAUTH_CLIENT_SECRET=<your-client-secret>

# Pre-set Google URLs — copy these exactly
OAUTH_AUTH_URL=https://accounts.google.com/o/oauth2/v2/auth
OAUTH_TOKEN_URL=https://oauth2.googleapis.com/token
OAUTH_SCOPES=https://www.googleapis.com/auth/gmail.readonly
OAUTH_ALLOWED_HOSTS=accounts.google.com,oauth2.googleapis.com,www.googleapis.com,gmail.googleapis.com
OAUTH_PROVIDER_NAME=google

# Cron schedule for daily brief (UTC). "0 8 * * *" = 08:00 UTC every day.
TRIGGER_CRON=0 8 * * *

# Optional: set after first OAuth dance to skip the browser on restart
# OAUTH_REFRESH_TOKEN=<value from google.json>
```

Protect the file:

```bash
sudo chmod 600 /home/agentos/.agentos-secrets/agentos.env
sudo chown agentos:agentos /home/agentos/.agentos-secrets/agentos.env
```

> **Security note:** dx.3 stores OAuth client credentials in plaintext in `agentos.env`.
> The credential broker (cred.3) path for QEMU mode is deferred to a future increment.
> Keep this file read-only to the `agentos` user on a host with full-disk encryption.

### Step 4 — Provision Google credentials (one-time)

**Option A — headless server (no browser needed, recommended for Linux VPS):**

```bash
# Requires OAUTH_CLIENT_ID and OAUTH_CLIENT_SECRET in env or agentos.env.
agentctl auth google --device
# Prints a URL and a short code. Visit the URL on any device (phone, Mac, etc.)
# and enter the code. Authorization is complete in ~30 seconds.
# Credentials are written to ~/.agentos-secrets/google.json
```

If you ran this as your own user (not `agentos`), copy the credentials:

```bash
sudo cp ~/.agentos-secrets/google.json /home/agentos/.agentos-secrets/google.json
sudo chown agentos:agentos /home/agentos/.agentos-secrets/google.json
```

**Option B — Mac/Linux with browser:**

```bash
# On Mac (or any machine with a browser):
agentctl auth google          # opens browser, writes ~/.agentos-secrets/google.json

# Copy to the Linux host:
scp ~/.agentos-secrets/google.json agentos@server:/home/agentos/.agentos-secrets/
```

Extract the refresh token and add it to `agentos.env` to skip future browser dances:

```bash
# On the Linux host:
REFRESH=$(sudo -u agentos python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['refresh_token'])" \
  /home/agentos/.agentos-secrets/google.json)
echo "OAUTH_REFRESH_TOKEN=${REFRESH}" | sudo tee -a /home/agentos/.agentos-secrets/agentos.env
```

### Step 5 — Smoke test (optional but recommended)

Before installing the service, verify QEMU boots and the management API is reachable:

```bash
# Terminal 1: boot QEMU (note: uses agent.toml, not cos config — fine for smoke test)
make -C agentos/distro run

# Terminal 2: confirm API is reachable
agentctl watch --url http://localhost:7999
# Press Ctrl-A then X to quit QEMU
```

### Step 6 — Install and start the service

```bash
sudo cp agentos/distro/agentos-cos.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now agentos-cos
```

### Step 7 — Monitor

```bash
# Service logs (QEMU stdout — kernel boot + init messages)
sudo journalctl -u agentos-cos -f

# AgentOS dashboard (from the Linux host — loopback only)
agentctl watch --url http://localhost:7999

# Remote monitoring (from Mac or another machine):
ssh -L 7999:localhost:7999 agentos@server   # in a terminal
agentctl watch --url http://localhost:7999   # in another terminal

# Morning brief (ux.11c — durable pull; shows the latest even after an overnight restart)
agentctl brief                                  # attention-first: failures/approvals + run IDs, then counts
agentctl brief --url http://localhost:7999      # explicit endpoint (honors AGENTCTL_URL)
agentctl brief --n 5                            # the last 5 briefs
curl -s localhost:7999/api/v1/brief             # structured JSON: {brief, approvals_pending}

# Full brief markdown (written by the CoS in addition to the pull surface)
ls /home/agentos/.agentos-output/brief-*.md
tail -f /home/agentos/.agentos-output/brief-$(date +%Y-%m-%d).md

# Approval workflow
agentctl approve <id>           # approve pending requests
agentctl deny <id> --reason ... # deny with reason
# Or use agentctl watch [a] key for the interactive Approvals pane
```

---

## Credential provisioning reference

| Credential | File | Method |
|-----------|------|--------|
| Anthropic API key | `agentos.env` | Manual — copy from console.anthropic.com |
| Google OAuth app | `agentos.env` | Manual — create at console.cloud.google.com |
| Google refresh token | `agentos.env` | `agentctl auth google [--device]` → extract → add to env |
| `google.json` | `.agentos-secrets/google.json` | `agentctl auth google --device` (headless) or `agentctl auth google` → scp |

The `google.json` file is used by `oauth_mcp.py` as an alternative to env vars (checked first
when `AGENTD_CREDENTIAL_GATEWAY_URL` is set — credential broker path, available in future).

---

## Budget windows (ux.8′)

The always-on CoS is metered so token spend is always bounded. Two `[scheduler]`
knobs govern it (shipped CoS configs set both):

```toml
[scheduler]
global_token_budget   = 50_000_000   # ceiling across all agents
budget_reset_interval = 86400        # rolling window in seconds (86400 = daily)
```

- **With `budget_reset_interval > 0`** (recommended, and the shipped default),
  `global_token_budget` is a **per-window** ceiling: spend accrues, and every
  `budget_reset_interval` seconds the window rolls over and the ceiling refreshes.
  The agent never permanently self-bricks. Per-agent `token_budget` uses the same
  window. `token_budget = 0` (or `global_token_budget = 0`) means **unlimited**.
- **With `budget_reset_interval = 0`** (legacy), the ceiling is **lifetime** — once
  exhausted the agent permanently denies until the checkpoint is reset. agentd logs
  a startup warning if you set a ceiling with no window. Do **not** use `rm
  checkpoint.json` to recover (it destroys conversation state); set a window instead.

**Force a reset now** (the manual escape hatch — e.g. a burst blew the window early):

```bash
# Global window:
curl -sX POST localhost:7999/api/v1/budget/reset -d '{"target":"global"}'
#   → {"target":"global","spent_before":<N>,"reset_to":0}
# A single agent's window:
curl -sX POST localhost:7999/api/v1/budget/reset -d '{"target":{"agent":"cos-orchestrator"}}'
#   → 404 if the agent id is unknown
```

Each reset emits a `budget_reset` event to `flight.jsonl`
(`{target, spent_before, window_start, interval_secs, windows_advanced}`) so a
spend drop reads as a scheduled rollover, not data loss.

**Set a per-agent budget at runtime** (ux.11a — raise a ceiling without a respawn;
raising revives a deferred agent immediately, and the change survives a restart):

```bash
agentctl set-budget cos-orchestrator 50000000 --url http://localhost:7999
#   limit 0 = UNLIMITED (it removes the cap — it does not mean "stop")
```

From the cockpit, `agentctl watch` → select the row → **`[x]`** → *Set budget* does the same thing,
and the overlay prints the equivalent CLI line (with the `--url` flag for this session) so an incident
note is copy-pasteable. `[x]` also offers:

- **Park** — cap the budget at the spend already recorded. Read the label, because it means two
  different things and neither is "held until you say otherwise":
  - **`budget_reset_interval > 0`** (the CoS configs, 86400): the agent is deferred and then
    **resumes by itself at the next window rollover**, because the rollover rebases every agent's
    windowed spend to 0. Raising the limit revives it sooner. A pause with a deadline you did not pick.
  - **`budget_reset_interval = 0`** (the default): exhaustion terminates the agent. Park **ends it**.
- **Cancel** — irreversible, cascades to the spawned subtree, and the confirm shows how many agents
  that is (`agentctl cancel <id>` prints the same count).

The raw route is still there if you need it — it takes the same `{target, limit}` body the CLI sends:

```bash
curl -sX POST localhost:7999/api/v1/budget/set \
  -d '{"target":{"agent":"cos-orchestrator"},"limit":50000000}'
#   → {"target":"cos-orchestrator","old_limit":<N>,"limit":50000000}
#   → 404 if the agent id is unknown; limit:0 = unlimited
#   → 400 for {"target":"global"} — the global ceiling is immutable config, not runtime-settable
```

Per-agent **windowed** spend is visible without reading `flight.jsonl`: the agentctl
TUI budget cell (`47k/100k`), the management snapshot (`windowed_spent` per agent), and
the FUSE file `/agents/<id>/windowed_spend`. A `budget_set` event records each change.

**Accepted tradeoff (early-burn blackout):** a fixed window means an agent that
burns its budget early in the window goes idle until the next rollover, rather
than degrading to a cheaper model. For a personal always-on assistant this is the
v1 behavior; size `global_token_budget` for your peak day, or force a reset with
the endpoint above. (Continuous refill / soft-cap degradation is possible future
work, not shipped.)

---

## Troubleshooting

### Verifying config changes (dry run — no credentials needed)

After editing `agentd/cos.agents.toml` (or the entrypoint's sed rules), verify the
container-boot rewrite and its path guards without any secrets:

```bash
make dev-image                                            # rebuild after each edit (tags agentos:dev)
docker run --rm -e DRY_RUN_ONLY=1 agentos:dev cos         # prints rewritten config, exit 0
# agent mode needs a template + a dummy key (only checked for non-emptiness):
docker compose run --rm -e DRY_RUN_ONLY=1 -e ANTHROPIC_API_KEY=x \
  -e TEMPLATE_NAME=scout -e AGENT_TASK=x agent
```

Success prints the fully-rewritten TOML and exits 0. A failed rewrite exits 1 and
names the surviving line (the boot guards catch relative paths the sed rules
missed — quoted `./`-style values and positive-form `*_path`/`*_dir` keys; a bare
relative `path = "x"`/`dir = "x"` value without `./` is a known residual, see
TODOS.md audit86-P1-5). `AGENTOS_SKIP_PATH_GUARDS=1` overrides the guards if your
custom config legitimately contains quoted relative paths.

```bash
# Was the agent started?
sudo journalctl -u agentos-cos --no-pager | grep -i "agentd\|error\|panic"

# Did it connect to Gmail?
jq 'select(.kind == "tool_call")' /home/agentos/.agentos-output/flight.jsonl | head -5

# Is the signed receipt chain intact? (NOT a credential check — this label was wrong, and the
# command was missing its second argument, so it failed as printed. Both fixed in ux.6a.)
agentctl verify /home/agentos/.agentos-output/evidence.jsonl \
                /home/agentos/.agentos-output/egress-key.pub

# Did cron fire?
jq 'select(.kind == "tool_call") | select(.data.tool == "wait_for_trigger")' \
   /home/agentos/.agentos-output/flight.jsonl | jq .data.result | tail -3

# Management API reachable?
curl -s http://localhost:7999/healthz | jq .

# Common issues:
#   "failed to mount secrets0" — agentos-secrets dir missing or wrong owner
#   "ANTHROPIC_API_KEY not set" — agentos.env missing or misspelled variable
#   QEMU exits immediately — check /opt/agentos/{bzImage,rootfs.cpio.gz} exist
#   cron never fires — TRIGGER_CRON not set in agentos.env
```
