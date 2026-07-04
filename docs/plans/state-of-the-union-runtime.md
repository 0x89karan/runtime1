# State of the Union: AgentOS Runtime & Deployment

*Design session — 2026-07-04*

## What we built and where we are

AgentOS has a working runtime (`agentd`), a CoS harness (daily Gmail brief), and a
full MCP server ecosystem. The core Rust code is solid. The problem is the deployment
story — getting it running cleanly, reliably, and without friction.

### The UX friction audit

The CoS harness currently requires:

```
export ANTHROPIC_API_KEY=...
export OAUTH_CLIENT_ID=...
export OAUTH_CLIENT_SECRET=...
export OAUTH_AUTH_URL=https://accounts.google.com/o/oauth2/v2/auth
export OAUTH_TOKEN_URL=https://oauth2.googleapis.com/token
export OAUTH_SCOPES=https://www.googleapis.com/auth/gmail.readonly
export OAUTH_ALLOWED_HOSTS=accounts.google.com,...
export OAUTH_PROVIDER_NAME=google
docker compose run --service-ports --rm cos   # footgun: wrong flag = OAuth broken
```

Then a browser dance inside Docker, then fishing the refresh token out of a named
volume. This is not acceptable for a tool you want to run every day.

### Why it got complicated

The project was built in layers:
1. Core runtime (`agentd`) — runs fine anywhere, completely portable
2. FUSE surface — Linux kernel feature, needs Docker or Linux to work
3. Python MCP servers — live in `docker/` but have zero Docker dependency, they're just Python scripts
4. CoS harness — built assuming Docker because Docker conveniently solved points 2 and 3
5. OAuth — bolted onto the Docker setup, never had a clean provisioning story

**Key discovery:** almost nothing in the codebase is actually Docker or QEMU specific.

```
Docker-specific code:    ~330 lines  (Dockerfile + docker-compose.yml + entrypoint.sh)
QEMU-specific code:      ~12 files   (distro/Makefile, buildroot.config, kernel config, init)
Linux-only Rust code:    39 lines    (#[cfg(target_os = "linux")] across 33,851 total LOC)
Python MCP servers:      2,967 lines — NOT Docker-specific, just Python scripts
Everything else:         fully portable, runs on macOS today with cargo run
```

---

## The platform question

### QEMU on macOS / Apple Silicon

The `distro/` target is **x86_64 musl**. On Apple Silicon, QEMU runs that as full
software emulation (TCG) — no hardware acceleration. Slow, not practical for daily use.

Docker Desktop on macOS uses Apple's Virtualization.framework to run a **lightweight
ARM64 Linux VM** automatically. That VM gets hardware acceleration. Docker containers
run fast inside it.

**Conclusion:** QEMU is not the right Mac story. Docker is.

> **⚠️ Superseded conclusion — this holds only because `distro/` is x86_64-only.** With an
> **aarch64 distro target + `qemu-system-aarch64 -accel hvf`** (planned as `ma.1`/`ma.2` in
> `docs/DEPLOYMENT-TOPOLOGY.md`), QEMU runs the pure-OS boot **near-native on Apple Silicon** —
> so "Mac = Docker only" is no longer forced; the Mac can be a first-class *OS-boot* host. This
> matters for the OS identity (don't let the boot path become vestigial). **Decide multi-arch
> before dx.3/dx.4 freeze x86-only assumptions**, and build the deployment arch-parameterized
> from the start rather than retrofitting.

---

## The decision: Option D

**Mac (Docker):** Docker Desktop provides the VM layer transparently. agentd runs in
an Alpine container. Clean environment, fast on Apple Silicon.

**Linux server (QEMU + KVM):** Hardware-accelerated VM, agentd is PID 1 of its own
Linux kernel. This is the pure OS vision — agentd literally IS the userspace.

---

## Architecture

### Mac — Docker Paradigm

```
HOST (macOS, Apple Silicon)
┌─────────────────────────────────────────────────────────────────────────┐
│                                                                         │
│  ~/.agentos-secrets/          ~/agentos-output/                         │
│  ├── agentos.env              └── brief-2026-07-04.md  ◀── you read    │
│  │   ANTHROPIC_API_KEY=...                                              │
│  │   OAUTH_CLIENT_ID=...      http://localhost:8080  ◀── you approve   │
│  └── google.json              (approval UI)                             │
│      (written by agentctl auth google, run once on host)               │
│                                                                         │
│  Docker Desktop (Apple Virtualization.framework — invisible ARM64 VM)  │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │  Alpine container                                                 │  │
│  │  ┌─────────────────────────────────────────────────────────────┐ │  │
│  │  │  agentd (PID 1 of container)                                │ │  │
│  │  │  reads /run/secrets/agentos.env at startup                  │ │  │
│  │  │                                                             │ │  │
│  │  │  ┌─────────────┐  stdio  ┌──────────────────────────────┐  │ │  │
│  │  │  │ cron_mcp.py │◀───────▶│  cos-orchestrator agent      │  │ │  │
│  │  │  └─────────────┘         │  inbox agent                 │  │ │  │
│  │  │  ┌─────────────┐  stdio  │  curator agent               │  │ │  │
│  │  │  │oauth_mcp.py │◀───────▶│                              │  │ │  │
│  │  │  │reads        │         └──────────────┬───────────────┘  │ │  │
│  │  │  │/run/secrets/│                        │                  │ │  │
│  │  │  │google.json  │         ┌──────────────▼───────────────┐  │ │  │
│  │  │  └─────────────┘         │  HTTP :8080 approval server  │  │ │  │
│  │  │                          │  (serves when pending)       │  │ │  │
│  │  └─────────────────────────────────────────────────────────┘ │ │  │
│  │                                                               │ │  │
│  │  Volume mounts:                                               │ │  │
│  │  ~/.agentos-secrets/ → /run/secrets/  (read-only)            │ │  │
│  │  ~/agentos-output/   → /run/output/   (read-write)           │ │  │
│  │  cos-data (named)    → /data/         (state, memory.redb)   │ │  │
│  │  Port forward: 8080:8080                                      │ │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘

NETWORK
agentd → api.anthropic.com        (inference)
oauth_mcp → gmail.googleapis.com  (via OAuth token in /run/secrets/)
:8080 → your browser              (approval UI)
```

### Linux Server — QEMU Paradigm

```
HOST (Linux VPS / home server, x86_64)
┌─────────────────────────────────────────────────────────────────────────┐
│                                                                         │
│  ~/.agentos-secrets/          ~/agentos-output/                         │
│  ├── agentos.env              └── brief-2026-07-04.md  ◀── you read    │
│  └── google.json                  (via SSH or rsync)                   │
│                                                                         │
│  systemd: agentos-cos.service     http://server-ip:8080 ◀─ you approve │
│  └── runs QEMU process            (port-forwarded from VM)             │
│                                                                         │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │  QEMU/KVM (hardware accelerated — near native speed)             │  │
│  │                                                                   │  │
│  │  Buildroot Linux VM                                               │  │
│  │  ┌─────────────────────────────────────────────────────────────┐ │  │
│  │  │  agentd (PID 1 — IS the userspace)                          │ │  │
│  │  │  reads /run/secrets/agentos.env at boot                     │ │  │
│  │  │                                                             │ │  │
│  │  │  ┌─────────────┐  stdio  ┌──────────────────────────────┐  │ │  │
│  │  │  │ cron_mcp.py │◀───────▶│  cos-orchestrator agent      │  │ │  │
│  │  │  └─────────────┘         │  inbox agent                 │  │ │  │
│  │  │  ┌─────────────┐  stdio  │  curator agent               │  │ │  │
│  │  │  │oauth_mcp.py │◀───────▶│                              │  │ │  │
│  │  │  │reads        │         └──────────────┬───────────────┘  │ │  │
│  │  │  │/run/secrets/│                        │                  │ │  │
│  │  │  │google.json  │         ┌──────────────▼───────────────┐  │ │  │
│  │  │  └─────────────┘         │  HTTP :8080 approval server  │  │ │  │
│  │  │                          └──────────────────────────────┘  │ │  │
│  │  └─────────────────────────────────────────────────────────────┘ │  │
│  │                                                                   │  │
│  │  virtfs mounts (9p):                                              │  │
│  │  secrets0: ~/.agentos-secrets/ → /run/secrets/  (read-only)      │  │
│  │  output0:  ~/agentos-output/   → /run/output/   (read-write)     │  │
│  │  memory0:  ~/.agentos-memory/  → /run/memory/   (read-write)     │  │
│  │                                                                   │  │
│  │  QEMU networking:                                                 │  │
│  │  -netdev user,hostfwd=tcp::8080-:8080                            │  │
│  └───────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘

NETWORK
agentd → api.anthropic.com        (via QEMU SLIRP NAT)
oauth_mcp → gmail.googleapis.com  (via QEMU SLIRP NAT)
:8080 → your browser              (QEMU port-forward → host → your browser)
```

### What's identical across both

```
cos.agents.toml         same file, same content, runs in both
agentd binary           same binary (musl static, runs in Alpine or Buildroot)
oauth_mcp.py            same script, reads /run/secrets/google.json
~/.agentos-secrets/     same directory layout on the host
HTTP approval surface   same port 8080, same HTML
~/agentos-output/       same output directory on the host

THE ONLY DIFFERENCE: what mounts /run/secrets/ and /run/output/
  Mac:   Docker volume mount
  Linux: QEMU virtfs (9p protocol)
```

---

## What needs to be built

### Shared (both platforms)

**1. `agentctl auth <provider>`**
Runs OAuth PKCE on the host, writes `~/.agentos-secrets/google.json`. This is the
key missing piece — the browser dance moves to the host, outside any container or VM,
and only happens once.

**2. `oauth_mcp.py` secrets path**
Read from `/run/secrets/google.json` first, fall back to `OAUTH_REFRESH_TOKEN` env
var for backward compat. ~5 lines of Python. Hardcode Google OAuth URLs as defaults
so operators don't need to export them.

**3. HTTP approval surface in agentd**
When `request_approval()` fires, serve a minimal HTML page on port 8080.
Works in Docker (port binding) and QEMU (hostfwd). No TUI required for approvals.

**4. `~/.agentos-secrets/` auto-discovery**
agentd reads this directory at startup and injects `KEY=value` pairs as environment
for MCP servers. Removes the manual `passenv` list from config.

### Mac-specific

**5. `docker-compose.yml` secrets mount**
Add `~/.agentos-secrets:/run/secrets:ro` volume. Remove `OAUTH_REFRESH_TOKEN` and
the 5 hardcoded Google URLs from the environment block — those move into `oauth_mcp.py`
as compiled-in defaults.

**6. Pre-built Docker image**
Push to `ghcr.io/0x89karan/runtime1:latest` from CI. `docker compose up -d cos`
pulls and runs, no build step required.

### Linux-specific

**7. `distro/Makefile` secrets virtfs**
Add `-virtfs local,path=$HOME/.agentos-secrets,mount_tag=secrets0,...` alongside
the existing memory0 and output0 mounts.

**8. systemd unit**
`agentos-cos.service` that starts the QEMU process, restarts on failure, starts
on boot.

### The OAuth problem on headless Linux servers

`agentctl auth google` on a VPS has no browser. Three options, in order of preference:

```
A) OAuth Device Flow   User gets a short code, visits a URL on phone/laptop.
   (build this)        How gcloud auth login --no-browser works. ~50 lines Python.

B) Provision on Mac    agentctl auth google on Mac.
   then copy           scp ~/.agentos-secrets/ user@server:~/.agentos-secrets/
   (works today)       Secrets are files. Files copy. Done.

C) SSH tunnel          ssh -L 8585:localhost:8585 user@server
   (workaround)        Browser on Mac hits localhost → tunnels to server.
```

B works immediately. A is the right long-term answer and should be built before
advertising Linux server deployment to anyone else.

---

## Open decision: approval timeout behaviour

This changes what the HTTP approval surface needs to implement.

### Blocking (current behaviour)

```
agent calls request_approval()
       │
       ▼
agentd holds the agent turn ──── waits indefinitely ────▶ you open :8080
       │                                                   click Approve
       │◀──────────────────────────────────────────────────────────────
       ▼
agent continues
```

Good for: interactive sessions, decisions that genuinely need a human.  
Bad for: 08:00 cron brief while you're asleep — agent is stuck until you wake.

### Non-blocking with timeout (what production cron needs)

```
agent calls request_approval(timeout_s=3600, default="approve")
       │
       ▼
agentd holds agent turn AND starts timer
       │                    │
       │ you approve        │ timeout fires
       ▼                    ▼
agent continues      agent continues with default
                     "approval_auto: approve" logged in flight.jsonl
```

Good for: cron jobs, overnight runs — brief gets written regardless.  
Bad for: anything that sends or modifies external state (wrong default = real damage).

### Recommendation

The CoS brief is **read-only** (Gmail read + local file write). Default = approve
makes sense. The right model is:

```toml
# In agent task prompt or request_approval() call:
# kind="data-access" → safe to auto-approve after timeout
# kind="send"        → never auto-approve, always block
# kind="modify"      → never auto-approve, always block
```

Auto-approve after 1 hour for read-only operations. Block indefinitely for anything
that touches the outside world in a write direction.

**This is the open decision to make before implementing the HTTP approval surface.**

---

## Build order

```
PHASE 1 — Fix Mac Docker UX                            ~2h CC
  oauth_mcp.py reads /run/secrets/google.json
  docker-compose.yml mounts ~/.agentos-secrets/
  agentctl auth google subcommand (PKCE on host)
  Drop 6 env vars from docker-compose.yml
  Target: docker compose up -d cos. That's it.

PHASE 2 — HTTP approval surface + timeout policy       ~1h CC
  Decide: blocking vs non-blocking with timeout
  agentd serves port 8080 when approval pending
  Minimal HTML: pending action, approve/deny buttons
  Optional: timeout + default for read-only ops

PHASE 3 — Linux QEMU production                        ~1h CC
  distro/Makefile gets secrets0 virtfs mount
  cos.agents.toml paths verified for /run/ layout
  systemd unit for agentos-cos.service
  Test: ssh to server, systemctl start agentos-cos

PHASE 4 — Pre-built images + device flow               ~1h CC
  ghcr.io Docker image published from CI
  agentctl auth --device-flow for headless servers
  Target: others can run this without building anything
```

Total estimated CC time: ~5h. Human coordination + testing: ~1-2 days.

---

## What this supersedes

`docs/plans/pure-os-ux-alignment.md` — the original plan identified the right
problems but framed them as "make CoS work in QEMU on Mac." The correct frame is
"establish a clean secrets + approval model that works uniformly across Docker
(Mac) and QEMU (Linux server)." The proposed solutions are the same; the scope
and priority order are clearer here.

---

## Update — 2026-07-04

Three strategic questions surfaced after the initial doc was written.

---

### Q1: Is the lightweight OS premise being diluted?

No — but the risk is real if discipline slips.

The premise has two components. The **cognitive model**: agents as the primitive,
remote cognition, agentd as the userspace runtime. This is untouched. Docker is
~330 lines of wrapper on 33,851 lines of agent model code. agentd doesn't know
or care it's inside a container. The **substrate**: lightweight binary, no local
weights, minimal dependencies. Also untouched — the binary is 6 MB; Docker Desktop
is Apple's hardware-accelerated Virtualization.framework, not meaningfully different
from QEMU as a mechanism.

The rule that preserves the premise: **agentd must never contain Docker-specific
logic.** No `if running_in_docker` branches. No Docker SDK dependency. No
container-specific paths hardcoded in the runtime. Docker is a deployment wrapper;
it must stay invisible to the agent model.

The stronger framing: "agentd is PID 1 of whatever userspace it inhabits" —
QEMU VM, Docker container, or bare metal. The OS-ness is the agent model. The
hypervisor is a substrate detail.

---

### Q2: How do we get a consistent TUI/UX/DX across Mac and Linux?

The current `agentctl watch` reads FUSE files from `/agents/`. On Mac+Docker, FUSE
lives inside the container — you'd have to `docker exec` to reach it. That's not
a consistent operator experience.

**The answer: agentd serves an HTTP management API on :7999.**

```
agentd
├── /agents/ FUSE filesystem   (internal, agent-to-agent, pure OS fast path)
└── :7999 management API       (external, operator-facing, works everywhere)

agentctl watch
  → talks to :7999
  → Mac:   Docker port binding 7999:7999
  → Linux: localhost:7999 (or QEMU hostfwd)
  → Remote: SSH tunnel or local network
```

agentctl becomes a thin HTTP client. The FUSE surface stays as an internal
mechanism (agentd still writes /agents/ files for scheduling and inter-agent
use), but agentctl no longer depends on filesystem access. The existing views —
Dashboard, AgentDetail, Topology, Memory, Approvals, Inspector — reconstruct
identically from the HTTP API.

This also unlocks: running `agentctl watch` on a Mac while agentd runs on a
remote Linux server. No SSH tunneling into a FUSE namespace required.

This is the same model as every serious daemon: kubectl talks to the Kubernetes
API server, docker CLI talks to the daemon socket. The filesystem surface is an
optimization, not the contract.

---

### Q3: Deep observability, surfaced through that UX

The flight recorder already captures every meaningful event as structured JSONL.
OTLP sidecar (obs.1–3) already ships spans and token metrics to any OTel-compatible
backend. The missing piece is **real-time accessibility without grepping a file
inside a container**.

Three layers:

**Layer 1 — Live stream (right now).**
agentd fans out flight events over SSE at `GET /api/v1/stream`. Every
`flight_recorder.emit()` writes to disk AND broadcasts to connected SSE
subscribers. agentctl watch opens this stream; events render in the Inspector
view in real time. Zero polling, zero filesystem access.

**Layer 2 — Query API (operational snapshot).**
`GET /api/v1/agents`, `/api/v1/agents/:id`, `/api/v1/agents/:id/flight?last=100`,
`/api/v1/approvals`, `/api/v1/memory/:ns`. agentctl reconstructs its views from
structured JSON instead of parsing FUSE virtual files. Also the foundation for
any future web UI or mobile notification when an approval fires.

**Layer 3 — OTLP export (already done, analytical).**
obs.1–3 ship spans and token metrics to Grafana, Honeycomb, or any
OpenTelemetry-compatible backend. This is the "what happened over the last 30
days" tier. Layer 1+2 are "what is happening right now."

The per-agent observability that should be visible through the TUI:
- Live token spend rate (tokens/min, $ per turn, $ total)
- Current tool call: name, input preview, duration so far
- Memory pressure (short-term fill %, recent evictions)
- Flight event stream with filtering (errors, tool calls, approvals, capability denials)
- Approval queue with approve/deny without leaving the TUI

All of this is already captured in the flight recorder. The work is routing it
through the management API rather than through FUSE paths.

---

### Revised build order

The original four phases above are superseded by this ordering, which reflects
the dependency on the management API:

```
dx.1 — Mac Docker DX                                   ~2h CC
  oauth_mcp.py reads /run/secrets/google.json first
  docker-compose.yml mounts ~/.agentos-secrets/ ro
  agentctl auth google subcommand (PKCE on host, writes token file)
  Drop 6 hardcoded Google env vars from docker-compose.yml
  Port bindings: 7999 + 8080 in docker-compose.yml
  Target: docker compose up -d cos is the full daily workflow

p7.7 — Management HTTP API                             ~3h CC
  agentd serves :7999
  GET /api/v1/agents (list with snapshot)
  GET /api/v1/agents/:id (full detail)
  GET /api/v1/stream (SSE fan-out of flight events)
  GET /api/v1/approvals
  GET /api/v1/memory/:ns
  agentctl auto-detects: FUSE available → use FUSE, else → HTTP API
  Same agentctl watch UX on Mac and Linux
  Foundation for web UI / mobile notifications

dx.2 — HTTP approval surface                           ~1h CC
  agentd serves :8080 when request_approval() fires
  Minimal HTML: action description, Approve / Deny buttons
  Optional: timeout_s + default="approve" for read-only ops
  Works via Docker port binding or QEMU hostfwd
  Replaces need to docker exec into container to approve

dx.3 — Linux QEMU production                           ~1h CC
  distro/Makefile: secrets0 virtfs + 7999 + 8080 hostfwd
  systemd unit: agentos-cos.service (restart on failure, start on boot)
  cos.agents.toml paths verified for /run/secrets/ layout

dx.4 — Pre-built images + device flow                  ~1h CC
  ghcr.io/0x89karan/runtime1:latest published from CI
  agentctl auth --device-flow for headless Linux servers
  Target: anyone can run this with docker compose up, no build step
```

Total estimated CC time: ~8h. Human coordination + testing: ~2-3 days.
