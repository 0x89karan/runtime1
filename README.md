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

**Starting from scratch.** This repo currently contains only the design, the
build plan, and the conventions for what to build. The first work is **Phase 0**
— bringing up the `agentd` runtime from a fresh Cargo crate to a working
single-agent loop. See [`docs/ROADMAP.md`](docs/ROADMAP.md) for the increments
(`p0.1` … `p0.5`), then Phase 1 from there.

## Repo structure

```
agentos/                   ← run `claude` here
├── README.md              this file
├── CLAUDE.md              project memory for Claude Code
├── docs/
│   ├── DESIGN.md          full design & research — the why
│   ├── ROADMAP.md         the staged build plan — the what
│   └── CONVENTIONS.md     how to extend the codebase — the how
└── (agentd/ will be created in p0.1)
```

Future phases add siblings next to `agentd/`: `distro/` (Phase 2: Buildroot +
boot), `surfaces/` (Phase 3: `/agents` FUSE), `sandbox/` (Phase 4: isolation
profiles). The repo is a single monorepo across phases, not a per-phase split.

## Quickstart

In Claude Code, after p0.1 lands:

```bash
cd agentd
export ANTHROPIC_API_KEY=sk-...
cargo run -- agent.toml
```
