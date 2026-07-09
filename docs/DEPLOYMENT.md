# AgentOS Deployment Guide

Two paths: **Mac + Docker** (quickest) and **Linux QEMU** (production, KVM-accelerated).
Both run the same Chief of Staff agent and speak the same `agentctl watch` surface.

---

## Path 1 — Mac + Docker

**Prerequisites:** Docker Desktop

```bash
# 0. Provision secrets (one-time — survives terminal restarts)
mkdir -p ~/.agentos-secrets ~/.agentos-output
printf 'ANTHROPIC_API_KEY=sk-ant-...\n' >> ~/.agentos-secrets/agentos.env
chmod 600 ~/.agentos-secrets/agentos.env

# 1. One-time Google OAuth (writes ~/.agentos-secrets/google.json)
agentctl auth google

# 2. Start the CoS stack
docker compose up -d cos

# 3. Monitor (runs inside the container — FUSE and API are container-local)
docker compose exec cos agentctl watch

# 4. Approve Gmail OAuth (first run only)
#    Press [a] in agentctl watch — click the approval URL in your browser
```

Briefs appear in `~/.agentos-output/brief-YYYY-MM-DD.md` (the container writes to
`/data/output`, which is bind-mounted to this host directory).

> **Note:** `agentctl watch` on the Mac host won't work directly — port 7999 is
> loopback-only inside the container and not published to the host. Use
> `docker compose exec cos agentctl watch` to run inside the container.

---

## Path 2 — Linux QEMU

**Prerequisites:** `qemu-system-x86_64`, `/dev/kvm` accessible, Python3 (on build host only)

### Step 1 — Build the rootfs (on the Linux host)

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

Run the PKCE OAuth flow on your Mac (where you have a browser):

```bash
# On Mac:
agentctl auth google          # opens browser, writes ~/.agentos-secrets/google.json

# Copy to the Linux host:
scp ~/.agentos-secrets/google.json agentos@server:/home/agentos/.agentos-secrets/
```

Extract the refresh token and add it to `agentos.env` to skip future browser dances:

```bash
# On the Linux host:
REFRESH=$(sudo -u agentos jq -r .refresh_token /home/agentos/.agentos-secrets/google.json)
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
| Google refresh token | `agentos.env` | `agentctl auth google` → extract → scp |
| `google.json` | `.agentos-secrets/google.json` | `agentctl auth google` → scp to Linux host |

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
