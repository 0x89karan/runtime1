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

**Phase 0 complete; Phase 1 in progress.** `agentd` is a working Rust binary.
Phase 0 landed a full single-agent loop (config, flight recorder, inference
gateway, tools, MCP stdio client). Phase 1 is underway — p1.1 refactored the
agent into a sans-IO state machine; p1.2 added a cooperative multi-agent
scheduler; p1.3 added metered scheduling and admission control (`[scheduler]`
token budget + concurrency cap with a priority-based deferred queue). See
[`docs/ROADMAP.md`](docs/ROADMAP.md) for the full increment list.

## Repo structure

```
agentos/                   ← run `claude` here
├── README.md              this file
├── CLAUDE.md              project memory for Claude Code
├── CHANGELOG.md           notable changes per release
├── docs/
│   ├── DESIGN.md          full design & research — the why
│   ├── ROADMAP.md         the staged build plan — the what
│   └── CONVENTIONS.md     how to extend the codebase — the how
└── agentd/                the runtime (Rust crate)
    ├── agent.toml         single-agent example
    └── agents.toml        multi-agent example (p1.2+)
```

Future phases add siblings next to `agentd/`: `distro/` (Phase 2: Buildroot +
boot), `surfaces/` (Phase 3: `/agents` FUSE), `sandbox/` (Phase 4: isolation
profiles). The repo is a single monorepo across phases, not a per-phase split.

## Quickstart

```bash
cd agentd
export ANTHROPIC_API_KEY=sk-...
cargo run -- agent.toml           # single agent
cargo run -- agents.toml          # multiple agents concurrently (p1.2+)
tail -f flight.jsonl              # watch the flight log
```
