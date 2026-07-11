# AgentOS

A Linux-based operating system where **agents are the primitive, not applications**
— designed to be **super light**. The runtime is [`agentd`](agentd/). In the full
system, that process *is* the userspace (PID 1 / the boot target); today it runs
as an ordinary binary on a normal distro.

Two design decisions are constitutional. See [`CLAUDE.md`](CLAUDE.md) and
[`docs/DESIGN.md`](docs/DESIGN.md) for the full rationale.

- **Cognition is remote.** The device is a thin agent host; the model is an API
  call behind `InferenceGateway`. No local weights.
- **Single-tenant.** One person's box; agents are mutually trusting.

## Status

**Phases 0–7 + cred + obs + orch.1 complete (v0.66.0).** `agentd` is a working Rust binary.
Phases 0–2 built the full single/multi-agent loop, config, flight recorder,
inference gateway, tools, MCP stdio client, cooperative scheduler, capability
system, agent spawning, agent cards, rustls static binary, Buildroot rootfs +
QEMU boot, signal handling, MCP pagination, and graceful shutdown. Phase 3
added the `/agents` FUSE virtual filesystem (`surfaces/`) and the
Landlock LSM + seccomp-bpf sandbox crate (`sandbox/`) for MCP server
subprocesses. Phase 4 hardened the sandbox: pre-exec error pipe, binary size
CI guard, THREAT_MODEL, flight-event redaction, seccomp clone3 filter, and
Landlock V4 TCP port enforcement. Phase 5 added the persistent memory substrate
(redb-backed `MemoryStore`, short-term paging, long-term per-agent memory,
shared KB with mutability classes, BM25 search, eviction + summarization, FUSE
memory surface, and a hardening pass covering the OV-1 startup invariant and
inode pruning). Phase 6 added the template schema + on-disk catalogue
(`agentd::template`, `templates/` directory, `TemplateResolver`), the
`agentctl` operator CLI (`list-templates`, `spawn`, `watch`), a live TUI
dashboard (Dashboard / AgentDetail / System / Topology / Memory / Spawn /
Inspector views), and the sandbox-enforcement surface. Phase 7 adds connectivity
and streaming: Streamable HTTP MCP transport, SSE streaming inference, FUSE write
control surface, approval gate, Ed25519 signed action receipts, HTTP forwarding
proxy, gVisor/runsc universal-tier isolation, and the management HTTP API
(`:7999`) with SSE fan-out and `agentctl watch --url`. The `cred` track added a
credential broker gateway (`CredentialGateway` + `OAuthTokenCache`) so MCP tool
processes hold no raw credentials. The `obs` track added an OTLP sidecar
(`agentos-otel`) with span hierarchy, token metrics, and copy-truncate rotation
detection. orch.1 (v0.66.0) adds the interactive orchestrator: `AgentStatus::Waiting`,
`POST /api/v1/spawn`, `POST /api/v1/agents/:id/inject`, the `agentctl orchestrate`
REPL, and `docker/entrypoint.sh orchestrate` mode. ma.1 + ma.2 + ma.3 ship aarch64
CI, arm64 distro with HVF boot, and a multi-arch Docker image to
`ghcr.io/0x89karan/runtime1`.
See [`docs/ROADMAP.md`](docs/ROADMAP.md) for the full increment list.

## Repo structure

```
agentos/                   ← run `claude` here
├── README.md              this file
├── CLAUDE.md              project memory for Claude Code
├── CHANGELOG.md           notable changes per release
├── TODOS.md               open technical-debt items and completed increments
├── Cross.toml             cross-compilation image pin for aarch64 (ring compat)
├── docs/
│   ├── DESIGN.md          full design & research — the why
│   ├── ROADMAP.md         the staged build plan — the what
│   ├── CONVENTIONS.md     how to extend the codebase — the how
│   └── MCP_SERVERS.md     known HTTP MCP server URLs + config snippets (p7.1+)
├── agentd/                the runtime (Rust crate)
│   ├── agent.toml         single-agent example
│   └── agents.toml        multi-agent example (p1.2+)
├── templates/             Phase 6: agent template catalogue (p6.1+)
├── agentctl/              Phase 6+: operator CLI — list-templates, spawn, watch, inject, orchestrate, approve, deny, verify (p6.2+)
├── surfaces/              Phase 3: /agents FUSE virtual filesystem (p3.1+)
├── sandbox/               Phase 3: Landlock LSM + seccomp-bpf sandbox (p3.3+)
└── distro/                Phase 2: Buildroot external tree + QEMU boot
```

The repo is a single monorepo across phases, not a per-phase split.

## Quickstart

```bash
cd agentd
export ANTHROPIC_API_KEY=sk-...
cargo run -- agent.toml           # single agent
cargo run -- agents.toml          # multiple agents concurrently (p1.2+)
tail -f flight.jsonl              # watch the flight log
```

## Docker quickstart

**Contributors (fast inner loop):** build locally — native arm64 on Apple Silicon, no QEMU, ~2 min on second run.

```bash
make dev-image                                   # → agentos:dev
docker compose up cos                            # uses agentos:dev by default
```

See `docs/DEPLOYMENT.md` for the full local dev loop and release publishing guide.

**Published images** are pushed on `workflow_dispatch` or a `v*` tag push (not every merge).
Two image tiers published to `ghcr.io/0x89karan/runtime1` (multi-arch: `linux/amd64` + `linux/arm64`):

| Tier | Tag | Contents | When to use |
|------|-----|----------|-------------|
| **core** | `:core`, `:vX.Y.Z-core` | `agentd` + `agentctl` + `agentos-otel` + fuse3/bash/jq. No Python. | Custom MCP configs or HTTP-only MCP endpoints |
| **full** | `:full`, `:latest`, `:vX.Y.Z` | core + python3 + all standard MCP servers (shell, http, search, oauth, cron, fs_watch, webhook, semantic-kb) + template catalogue | Ready-to-run with the full harness |

```bash
# Full tier (batteries-included) — same as :latest
docker pull ghcr.io/0x89karan/runtime1:full

# Core tier (Rust runtime only)
docker pull ghcr.io/0x89karan/runtime1:core

# Chain-of-scouts research agent
export ANTHROPIC_API_KEY=sk-ant-...
docker run --rm -e ANTHROPIC_API_KEY ghcr.io/0x89karan/runtime1:full cos

# Interactive multi-turn orchestrator REPL (starts agentd + agentctl orchestrate)
docker run --rm -it -e ANTHROPIC_API_KEY ghcr.io/0x89karan/runtime1:full orchestrate
```

If the package is private, set it to Public once: GitHub repo → Packages →
agentos → Package Settings → Change visibility → Public.

## Docker quickstart (cos + Google)

The `cos` (chain-of-scouts) service runs on Docker and requires only your
Anthropic API key at launch time. Google credentials are provisioned once on
the Mac host via `agentctl auth google` and mounted into the container as a
read-only secrets volume.

**One-time setup:**

```bash
# 1. Create the secrets directory on your Mac host
mkdir -p ~/.agentos-secrets

# 2. Get Google OAuth credentials
#    console.cloud.google.com → APIs & Services → Credentials
#    Create (or edit) an OAuth 2.0 Client ID of type "Desktop app".
#    No redirect URI to register — Desktop clients allow 127.0.0.1 loopback
#    redirects automatically per RFC 8252 (agentctl uses http://127.0.0.1:8585).

# 3. Provision credentials on the Mac host (runs the PKCE OAuth2 flow in a
#    local browser tab, writes ~/.agentos-secrets/google.json)
agentctl auth google \
  --client-id YOUR_CLIENT_ID \
  --client-secret YOUR_CLIENT_SECRET

# 4. Set your Anthropic key
export ANTHROPIC_API_KEY=sk-ant-...

# 5. Start the cos service
docker compose up -d cos
docker compose logs -f cos        # watch the agent run
```

Credentials provisioned by `agentctl auth google` are stored as
`~/.agentos-secrets/google.json` (containing `client_id`, `client_secret`, and
`refresh_token`) and mounted read-only at `/run/secrets` in the provided
`docker-compose.yml` services. The mount is `:ro` — the container cannot
modify the file.
