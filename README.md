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

**Phases 0–7.2 complete (v0.36.0).** `agentd` is a working Rust binary.
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
and streaming: p7.1 ships the Streamable HTTP MCP transport (`McpHttpClient`,
`McpBackend` trait, `url` + `headers_env` config, `mcp_http_connected` flight
event) so agentd can connect to hosted MCP services like Linear and GitHub
without running a local subprocess; p7.2 adds opt-in SSE streaming inference
(`streaming = true` in `[model]`) so tokens print to stdout as they arrive.
See [`docs/ROADMAP.md`](docs/ROADMAP.md) for the full increment list.

## Repo structure

```
agentos/                   ← run `claude` here
├── README.md              this file
├── CLAUDE.md              project memory for Claude Code
├── CHANGELOG.md           notable changes per release
├── TODOS.md               open technical-debt items and completed increments
├── docs/
│   ├── DESIGN.md          full design & research — the why
│   ├── ROADMAP.md         the staged build plan — the what
│   ├── CONVENTIONS.md     how to extend the codebase — the how
│   └── MCP_SERVERS.md     known HTTP MCP server URLs + config snippets (p7.1+)
├── agentd/                the runtime (Rust crate)
│   ├── agent.toml         single-agent example
│   └── agents.toml        multi-agent example (p1.2+)
├── templates/             Phase 6: agent template catalogue (p6.1+)
├── agentctl/              Phase 6: operator CLI — list-templates, spawn, watch (p6.2+)
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
