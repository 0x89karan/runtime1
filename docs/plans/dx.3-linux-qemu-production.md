# dx.3 — Linux QEMU Production Path

**Status:** approved (post-review)  
**Version target:** v0.69.0  
**Depends on:** dx.2 (v0.54.0 ✓), cred.5 (v0.68.0 ✓)

**Review summary:** 3/3 reviews complete — CEO APPROVE_WITH_NOTES, Eng NEEDS_REVISION (2 blockers fixed below), DX APPROVE_WITH_NOTES (3 blockers fixed below). All findings resolved.

---

## Problem / Motivation

The Mac Docker path (cred.1–dx.2) is complete and lets operators run the full CoS
on a Mac. dx.3 closes the loop for Linux server deployment: QEMU with KVM
acceleration, systemd supervision, hostfwd so `agentctl watch` reaches the VM from
the host, and a two-page operator guide that covers both paths.

---

## Scope

Six discrete changes, one after another — no concurrent concerns:

### 1. `distro/Makefile` — hostfwd port forwarding

Add host→guest port forwarding to the `make run` target **only** — NOT to the shared
`QEMU_FLAGS` variable (which is also used by `make test`). This avoids port conflicts
during CI where the test runner may already use port 7999 on the host.

Add a `RUN_EXTRA_FLAGS` variable and use it only in the `run` target:

```makefile
# Port forwarding — only for 'make run' (not 'make test')
RUN_NETDEV := user,id=net0,hostfwd=tcp:127.0.0.1:7999-:7999,hostfwd=tcp:127.0.0.1:8080-:8080

run: prereqs build
	mkdir -p $(CURDIR)/$(OUTPUT_DIR)/run
	$(QEMU) $(QEMU_FLAGS) \
		-netdev $(RUN_NETDEV) \
		-device virtio-net-pci,netdev=net0 \
		-virtfs local,path=$(CURDIR)/$(OUTPUT_DIR)/run,mount_tag=output0,...
```

The shared `QEMU_FLAGS` loses its `-netdev user,id=net0` and `-device virtio-net-pci,netdev=net0`
lines; each target (`run`, `test`) provides its own `-netdev`/`-device` pair.

**Security note:** hostfwd uses `127.0.0.1:7999` on the host (loopback only). On a
server with a public IP, binding `0.0.0.0:7999` would expose the unauthenticated
management API to the network. Operators who need remote `agentctl watch` should SSH
tunnel: `ssh -L 7999:localhost:7999 server`.

Port assignments:
- `7999` — agentd management HTTP API (`/api/v1/snapshot`, `/api/v1/approvals`, SSE fan-out, `agentctl watch`)
- `8080` — reserved for a future browser-friendly approval UI (currently no service; hostfwd is harmless)

### 2. `distro/buildroot.config` — Python3 + OpenSSL

Enable Python3 so the stdlib-only MCP sidecars can run inside the rootfs.
`oauth_mcp.py` uses `import ssl` and `urllib.request` for HTTPS calls to Google —
Python3's ssl module requires OpenSSL in the rootfs. Both packages are needed:

```
BR2_PACKAGE_PYTHON3=y
BR2_PACKAGE_OPENSSL=y
```

All Python MCP servers use only Python stdlib (verified: `cron_mcp.py` imports
`datetime, json, os, sys, time, uuid`; `oauth_mcp.py` imports `base64, hashlib, html,
json, os, secrets, socket, ssl, sys, tempfile, threading, time, urllib.*`). No third-party
packages needed beyond what Python3 + OpenSSL provide.

### 3. `distro/Makefile` — copy Python MCP servers into overlay

Add a step (parallel to `overlay/usr/bin/agentd`) that copies the Python servers into
`overlay/usr/lib/agentos/docker/` so Buildroot picks them up into the rootfs:

```makefile
overlay/usr/lib/agentos/docker/.gitkeep:
	mkdir -p overlay/usr/lib/agentos/docker
	cp ../docker/cron_mcp.py ../docker/oauth_mcp.py ../docker/http_mcp.py \
	   ../docker/shell_mcp.py ../docker/search_mcp.py ../docker/fs_watch_mcp.py \
	   ../docker/webhook_mcp.py overlay/usr/lib/agentos/docker/
	chmod +x overlay/usr/lib/agentos/docker/*.py
	touch $@
```

Add this target as a prerequisite of the build target (alongside `overlay/usr/bin/agentd`).

Add `overlay/usr/lib/agentos/docker/.gitkeep` to `.gitignore` (generated at build time).

Also update the `clean` target to remove the generated overlay artifacts:

```makefile
clean:
	rm -rf $(OUTPUT_DIR) overlay/usr/bin/agentd overlay/usr/bin/agentctl \
	       overlay/etc/agentd/templates/.gitkeep overlay/usr/lib/
```

### 4. `distro/overlay/init` — kernel cmdline config selection

The `init` script currently hardcodes `exec /usr/bin/agentd /etc/agentd/agent.toml`.
Add a mechanism to select the config file from the kernel cmdline so:
- `make run` and `make test` (no `agentd.config=` in cmdline) still use `agent.toml`
- The systemd unit adds `agentd.config=/etc/agentd/cos.agents.toml` to `-append` and gets CoS

Append before the final `exec`:

```sh
# Select agentd config from kernel cmdline (agentd.config=<path>), fallback to agent.toml
_CFG=$(cat /proc/cmdline | tr ' ' '\n' | grep '^agentd\.config=' | head -1 | cut -d= -f2-)
_CFG="${_CFG:-/etc/agentd/agent.toml}"
exec /usr/bin/agentd "$_CFG"
```

### 5. `distro/overlay/etc/agentd/cos.agents.toml` (new)

QEMU-mode version of the CoS config. Key differences from `agentd/cos.agents.toml` (dev mode):

- MCP server paths use `/usr/lib/agentos/docker/` (absolute, in rootfs)
- `store_path = "/run/memory/memory.redb"` (detachable memory volume)
- `[management] enabled = true` with `bind_addr = "0.0.0.0"` — **critical**: binds all VM
  interfaces so QEMU's hostfwd can reach it from the Linux host
- FsWrite capability uses `/run/output` (the host-shared output volume)
- Credentials come from agentos.env (via the existing env-file parsing in `/init`)

```toml
# Chief of Staff — QEMU/Linux production mode
# Install: /etc/agentd/cos.agents.toml (ships in the distro overlay)
# Launch:  via agentos-cos.service (systemctl start agentos-cos)

[management]
enabled   = true
port      = 7999
bind_addr = "0.0.0.0"   # expose to VM network for QEMU hostfwd

[model]
provider   = "anthropic"
model      = "claude-sonnet-4-6"
max_tokens = 8192
streaming  = true

[scheduler]
global_token_budget       = 0
max_concurrent_inferences = 3
max_spawn_depth           = 2

[egress]
evidence_path = "/run/output/evidence.jsonl"
key_path      = "/run/output/egress-key.pkcs8"

[memory]
enabled    = true
store_path = "/run/memory/memory.redb"
max_entries_per_segment = 500
max_entry_age_days      = 90

[[memory.segments]]
name  = "ops:briefs"
class = "log"

[[memory.segments]]
name  = "ops:entities"
class = "scratch"

[[tools.mcp_servers]]
name    = "cron_trigger"
command = "python3"
args    = ["/usr/lib/agentos/docker/cron_mcp.py"]
passenv = ["TRIGGER_CRON", "TRIGGER_INTERVAL", "TRIGGER_MAX_WAIT_S"]

[[tools.mcp_servers]]
name    = "google_oauth"
command = "python3"
args    = ["/usr/lib/agentos/docker/oauth_mcp.py"]
passenv = [
  "OAUTH_CLIENT_ID",
  "OAUTH_CLIENT_SECRET",
  "OAUTH_REFRESH_TOKEN",
  "OAUTH_AUTH_URL",
  "OAUTH_TOKEN_URL",
  "OAUTH_SCOPES",
  "OAUTH_ALLOWED_HOSTS",
  "OAUTH_PROVIDER_NAME",
  "OAUTH_CALLBACK_PORT",
]
capabilities = [
  { Net = { hosts = [
    "accounts.google.com",
    "oauth2.googleapis.com",
    "www.googleapis.com",
    "gmail.googleapis.com",
  ], ports = [443] } },
]

[[agents]]
id          = "cos-orchestrator"
name        = "Chief of Staff Orchestrator"
description = "Cron-triggered coordinator."
max_turns    = 200_000
token_budget = 5_000_000_000
priority     = 10
capabilities = [
  { Spawn   = {} },
  { KbRead  = { segment = "ops:briefs"   } },
  { KbWrite = { segment = "ops:briefs"   } },
  { KbRead  = { segment = "ops:entities" } },
  { KbWrite = { segment = "ops:entities" } },
  { Mcp    = { server = "cron_trigger", tools = [] } },
  { Mcp    = { server = "google_oauth",  tools = [] } },
  { FsWrite = { prefix = "/run/output" } },
]
task = """
<<COPY VERBATIM FROM agentd/cos.agents.toml — see the task = """...""" block there>>
"""
[tools]
native = ["spawn_agent","kb_get","kb_put","kb_search","write_file","request_approval"]
```

**Implementation blocker (B3):** The `task` field placeholder above MUST be replaced with
the full verbatim task text from `agentd/cos.agents.toml` before committing. Shipping the
placeholder would cause the CoS orchestrator to run with an empty task and do nothing.
There is no TOML `include` mechanism; copy-paste is the implementation path.

### 6. `agentd/cos.agents.toml` — add `[management]` section

The dev-mode file needs management enabled so `agentctl watch` works locally:

```toml
[management]
enabled = true
# bind_addr defaults to "127.0.0.1" — loopback only (safe for dev mode)
```

Place this after `[scheduler]` and before `[egress]`.

### 7. `distro/agentos-cos.service` (new)

Systemd unit for the Linux host. ExecStart invokes QEMU directly (rather than calling
`make run`) to avoid build-system dependencies at service start time. The unit assumes
the rootfs is pre-built at the standard location.

```ini
[Unit]
Description=AgentOS Chief of Staff (QEMU)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Restart=on-failure
RestartSec=10

# Run as a dedicated user — create with: useradd -m -r agentos
# Paths below use /home/agentos/; adjust if using a different home.
User=agentos

# QEMU-mode CoS invocation — assumes pre-built rootfs at /opt/agentos/
# Build: make -C /path/to/agentOS/distro build
ExecStart=/usr/bin/qemu-system-x86_64 \
  -nographic \
  -m 512M \
  -kernel /opt/agentos/bzImage \
  -initrd /opt/agentos/rootfs.cpio.gz \
  -append "console=ttyS0 quiet ip=dhcp agentd.config=/etc/agentd/cos.agents.toml" \
  -netdev user,id=net0,hostfwd=tcp:127.0.0.1:7999-:7999,hostfwd=tcp:127.0.0.1:8080-:8080 \
  -device virtio-net-pci,netdev=net0 \
  -virtfs local,path=/home/agentos/.agentos-secrets,mount_tag=secrets0,security_model=none,readonly=on,id=secrets0 \
  -virtfs local,path=/home/agentos/.agentos-memory,mount_tag=memory0,security_model=none,id=memory0 \
  -virtfs local,path=/home/agentos/.agentos-output,mount_tag=output0,security_model=none,id=output0

ReadWritePaths=/home/agentos/.agentos-memory /home/agentos/.agentos-output
ReadOnlyPaths=/home/agentos/.agentos-secrets

[Install]
WantedBy=multi-user.target
```

Notes:
- **`User=agentos`** — a dedicated system user. `User=%i` is invalid in a non-template unit.
  Create: `sudo useradd -m -r -s /usr/sbin/nologin agentos`
- **hostfwd on loopback** — `127.0.0.1:7999` prevents accidental exposure on public IPs.
  Remote access: SSH tunnel `ssh -L 7999:localhost:7999 server`.
- 512 MB RAM for CoS (vs 256 MB dev default — Python3 + 3 MCP sidecar processes)
- No `%h` — explicit `/home/agentos` paths are more predictable in system services

Install location: `/etc/systemd/system/agentos-cos.service`

### 8. `docs/DEPLOYMENT.md` (new)

Two-page operator guide covering both paths. Required content (must be in the final doc):

```
# AgentOS Deployment Guide

## Path 1 — Mac + Docker

1. Prerequisites: Docker Desktop, ANTHROPIC_API_KEY
2. agentctl auth google  (one-time OAuth — writes ~/.agentos-secrets/google.json)
3. docker compose up -d cos
4. agentctl watch  (from Mac host — connects to localhost:7999)
5. Approval workflow: [a] in agentctl watch, or POST /api/v1/approvals/:id/approve

## Path 2 — Linux QEMU

Prerequisites: qemu-system-x86_64, KVM enabled (/dev/kvm accessible), python3 (for build only)

1. Build the rootfs (on the Linux host):
   git clone ... && cd agentOS/distro && make build

2. Create the agentos system user:
   sudo useradd -m -r -s /usr/sbin/nologin agentos

3. Create /home/agentos/.agentos-secrets/agentos.env with ALL of:
   ANTHROPIC_API_KEY=sk-ant-...
   OAUTH_CLIENT_ID=<your-google-client-id>.apps.googleusercontent.com
   OAUTH_CLIENT_SECRET=<your-google-client-secret>
   OAUTH_AUTH_URL=https://accounts.google.com/o/oauth2/v2/auth
   OAUTH_TOKEN_URL=https://oauth2.googleapis.com/token
   OAUTH_SCOPES=https://www.googleapis.com/auth/gmail.readonly
   OAUTH_ALLOWED_HOSTS=accounts.google.com,oauth2.googleapis.com,www.googleapis.com,gmail.googleapis.com
   OAUTH_PROVIDER_NAME=google
   TRIGGER_CRON=0 8 * * *       # daily 08:00 UTC — adjust to taste

   > SECURITY NOTE: dx.3 stores OAuth credentials in plaintext in agentos.env.
   > The credential broker (cred.3) path for QEMU mode is deferred to a future increment.
   > Protect this file: chmod 600 /home/agentos/.agentos-secrets/agentos.env

4. Provision Google credentials (one-time, on Mac):
   agentctl auth google          # runs PKCE flow, writes ~/.agentos-secrets/google.json
   scp ~/.agentos-secrets/google.json agentos@server:/home/agentos/.agentos-secrets/
   # On server: cat /home/agentos/.agentos-secrets/google.json | jq -r .refresh_token
   # Add OAUTH_REFRESH_TOKEN=<value> to agentos.env to skip the browser dance on restart

5. Install and start the service:
   sudo cp distro/agentos-cos.service /etc/systemd/system/
   sudo cp distro/output/bzImage distro/output/rootfs.cpio.gz /opt/agentos/
   sudo systemctl daemon-reload
   sudo systemctl enable --now agentos-cos

6. Monitor from the Linux host (loopback — SSH tunnel for remote):
   agentctl watch --url http://localhost:7999
   # Remote: ssh -L 7999:localhost:7999 server, then agentctl watch --url http://localhost:7999

7. Brief output in /home/agentos/.agentos-output/brief-YYYY-MM-DD.md

## Smoke test before installing the service

   make -C distro run    # boots QEMU, forwards 7999; Ctrl-A X to quit
   # agentctl watch in a second terminal confirms the management API is reachable

## Troubleshooting

- sudo journalctl -u agentos-cos -f          # QEMU stdout (kernel + init messages)
- tail -f /home/agentos/.agentos-output/flight.jsonl
- agentctl verify /home/agentos/.agentos-output/evidence.jsonl
- grep -i "error\|panic" /home/agentos/.agentos-output/flight.jsonl
```

---

## Decisions Locked

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | Kernel cmdline (`agentd.config=<path>`) for config selection in init | Non-intrusive; preserves existing `make run`/`make test` behavior |
| D2 | Management `bind_addr = "0.0.0.0"` in QEMU overlay config | Required for QEMU hostfwd to reach the API from outside the VM |
| D3 | `bind_addr = "127.0.0.1"` (default) kept in dev mode `cos.agents.toml` | Dev mode doesn't need external access; loopback is more secure |
| D4 | `BR2_PACKAGE_PYTHON3=y` + `BR2_PACKAGE_OPENSSL=y` in buildroot.config | Python3 needed for MCP servers; OpenSSL needed for ssl module (oauth_mcp.py uses HTTPS) |
| D5 | Makefile copies Python MCP servers into `overlay/usr/lib/agentos/docker/` at build time | Keeps `docker/` as the source of truth; overlay is generated, not committed |
| D6 | `agentos-cos.service` uses a direct QEMU invocation (not `make run`) | Systemd units should not depend on make or the build system |
| D7 | 512 MB RAM for the service unit (vs 256 MB dev default) | Python3 + three MCP sidecar processes add ~50 MB each at peak |
| D8 | hostfwd goes in `make run`'s own netdev line (NOT shared QEMU_FLAGS); service unit has its own inline | `make test` uses QEMU_FLAGS too; adding hostfwd there could conflict with port 7999 on CI hosts |
| D9 | hostfwd binds to `127.0.0.1` on host (not `0.0.0.0`) | Prevents accidental exposure of the unauthenticated management API on public IPs |
| D10 | `User=agentos` (concrete dedicated user) in the systemd unit, not `User=%i` | `%i` is a template specifier, invalid in a non-template unit; would silently expand to empty string |

---

## Out of Scope (deferred)

- **Prebuilt image distribution** (dx.4): this dx.3 requires building the rootfs from source
- **Browser approval UI on :8080**: hostfwd is wired, service is not yet built; deferred
- **aarch64 QEMU service**: the systemd unit targets x86_64 only; aarch64 variant deferred
- **Credential broker in QEMU mode**: secrets come from agentos.env via the init parser; the credential gateway (cred.3) is not configured in the overlay cos.agents.toml for dx.3

---

## Test Plan

Unit/integration (no new Rust code — no cargo test additions needed):

1. **Makefile dry-run**: `make -n build` passes with new overlay targets
2. **init script**: unit-test the cmdline parsing with a mock /proc/cmdline (in CI via a trivial sh test)
3. **Overlay structure**: `make build ARCH=x86_64` (dry-run) produces `overlay/usr/lib/agentos/docker/*.py` and `overlay/etc/agentd/cos.agents.toml`
4. **cos.agents.toml parse**: `cargo run -- distro/overlay/etc/agentd/cos.agents.toml --dry-run` validates the TOML parses correctly

Acceptance (manual, on a Linux KVM host):
- `systemctl start agentos-cos` → QEMU boots, `flight.jsonl` shows `agent_started`
- `agentctl watch --url http://localhost:7999` → Dashboard shows CoS agent
- Cron fires → brief appears in `~/.agentos-output/brief-<date>.md`

---

## Version / CHANGELOG entry

```
v0.69.0 — dx.3: Linux QEMU production path
- distro/Makefile: hostfwd tcp:7999 + tcp:8080; Python3 overlay build step
- distro/buildroot.config: BR2_PACKAGE_PYTHON3=y
- distro/overlay/init: kernel cmdline agentd.config= config selection
- distro/overlay/etc/agentd/cos.agents.toml: QEMU-mode CoS config (bind_addr=0.0.0.0)
- agentd/cos.agents.toml: [management] enabled = true
- distro/agentos-cos.service: systemd unit for Linux host
- docs/DEPLOYMENT.md: two-page operator guide (Mac Docker + Linux QEMU)
```
