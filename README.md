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

**Phases 0–4 complete (v0.16.0).** `agentd` is a working Rust binary.
Phases 0–2 built the full single/multi-agent loop, config, flight recorder,
inference gateway, tools, MCP stdio client, cooperative scheduler, capability
system, agent spawning, agent cards, rustls static binary, Buildroot rootfs +
QEMU boot, signal handling, MCP pagination, and graceful shutdown. Phase 3
added the `/agents` FUSE virtual filesystem (`surfaces/`) and the
Landlock LSM + seccomp-bpf sandbox crate (`sandbox/`) for MCP server
subprocesses. Phase 4 hardened the sandbox: pre-exec error pipe, binary size
CI guard, THREAT_MODEL, flight-event redaction, seccomp clone3 filter, and
Landlock V4 TCP port enforcement. See [`docs/ROADMAP.md`](docs/ROADMAP.md) for
the full increment list.

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
│   └── CONVENTIONS.md     how to extend the codebase — the how
├── agentd/                the runtime (Rust crate)
│   ├── agent.toml         single-agent example
│   └── agents.toml        multi-agent example (p1.2+)
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
