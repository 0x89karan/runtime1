# AgentOS Deployment Guide

Two paths: **Mac + Docker** (quickest) and **Linux QEMU** (production, KVM-accelerated).
Both run the same Chief of Staff agent and speak the same `agentctl watch` surface.

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
#    Create Credentials → OAuth client ID → Desktop app.
#    Authorized redirect URI: http://127.0.0.1:8585   (must be FREE; if not, use
#    `agentctl auth google --port <N>` and register http://127.0.0.1:<N> instead).
#    Copy the Client ID + Client Secret.

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
docker run --rm -it \
  --name agentos-cos \
  --privileged \
  -e ANTHROPIC_API_KEY \
  -e "TRIGGER_INTERVAL=every 2m" \
  -v ~/.agentos-secrets:/run/secrets:ro \
  -v ~/.agentos-output:/data/output \
  -v ~/.agentos-data:/data \
  ghcr.io/0x89karan/runtime1:full cos

# 7. Watch the agents (second terminal; needs the --name from step 6)
docker exec -it agentos-cos agentctl watch

# 8. Read the brief (after the first cycle, ~2-3 min)
cat ~/.agentos-output/brief-$(date +%Y-%m-%d).md
```

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

**Logs & receipts:** inspect `~/.agentos-data/flight.jsonl` with `jq`; verify the signed action-receipt
chain with `agentctl verify ~/.agentos-data/evidence.jsonl`.

<details><summary><strong>Build from source instead of the published image (dev)</strong></summary>

`docker compose up -d cos` builds the image from your local checkout and runs the same CoS; monitor
with `docker compose exec cos agentctl watch`. Use this when you're changing `agentd`/`agentctl` and
want your local build rather than the released image.
</details>

### Troubleshooting

- **Container exits with "ANTHROPIC_API_KEY is not set":** the key wasn't in the shell that ran
  `docker run`. Re-`export` it here, or use the `agentos.env` approach above.
- **Browser shows `ERR_CONNECTION_REFUSED` during auth:** redirect-URI mismatch — confirm
  `http://127.0.0.1:8585` is registered in the Google OAuth app and that `8585` was free.
- **No brief after a few minutes:** check the cron fired and Gmail was reached —
  `jq 'select(.kind=="mcp_tool_called")' ~/.agentos-data/flight.jsonl | tail`.
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

# Brief output
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

## Troubleshooting

```bash
# Was the agent started?
sudo journalctl -u agentos-cos --no-pager | grep -i "agentd\|error\|panic"

# Did it connect to Gmail?
jq 'select(.kind == "mcp_tool_called")' /home/agentos/.agentos-output/flight.jsonl | head -5

# Are credentials valid?
agentctl verify /home/agentos/.agentos-output/evidence.jsonl   # signed receipt chain

# Did cron fire?
jq 'select(.kind == "mcp_tool_called") | select(.data.tool == "wait_for_trigger")' \
   /home/agentos/.agentos-output/flight.jsonl | jq .data.result | tail -3

# Management API reachable?
curl -s http://localhost:7999/healthz | jq .

# Common issues:
#   "failed to mount secrets0" — agentos-secrets dir missing or wrong owner
#   "ANTHROPIC_API_KEY not set" — agentos.env missing or misspelled variable
#   QEMU exits immediately — check /opt/agentos/{bzImage,rootfs.cpio.gz} exist
#   cron never fires — TRIGGER_CRON not set in agentos.env
```
