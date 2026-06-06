# AgentOS / agentd — Project Memory

You are working on **AgentOS**: a Linux-based operating system where **agents are
the primitive, not applications**, designed to be **super light**. `agentd` is its
runtime. In the full system this process *is* the userspace (PID 1 / the boot
target); today it runs as an ordinary binary on a normal distro.

Read `docs/DESIGN.md` for the full thesis, architecture, and rationale.
Read `docs/ROADMAP.md` for the build plan — **this is the work queue.**
Read `docs/CONVENTIONS.md` before adding a subsystem, tool, or provider.

## Locked decisions — constitutional, do not drift

These were decided deliberately. Do not relitigate or quietly violate them:

1. **Cognition is remote.** The device is a thin agent host. The model is an API
   call behind `InferenceGateway`. There are **no local model weights** and no
   local inference engine. (Adding a *local backend* later is allowed only as a
   new `impl InferenceGateway`, never as a core assumption.)
2. **Single-tenant.** This is an OS for one individual. Agents are mutually
   trusting and run **in-process**. Do not add multi-user isolation, per-user
   auth, or tenancy boundaries. (Capability *scoping between agents* is in scope
   — see the roadmap — but that is about least-privilege, not distrust.)

## Current status

**Nothing built yet.** This repo currently holds only docs. The first work is
**Phase 0**: bring up the `agentd` runtime from a fresh Cargo crate (`p0.1`)
through to a working single-agent perceive → infer → act → observe loop talking
real MCP over stdio (`p0.5`). See `docs/ROADMAP.md`. Phase 1 (scheduler +
inter-agent bus + capabilities) follows once Phase 0's exit criteria are met.

The "Repo layout" and "Commands" sections below describe the **target state**
after Phase 0 lands. Until `p0.1` is done, the `agentd/` directory doesn't exist.

## How to work here

- **Work the roadmap in order.** Each increment in `docs/ROADMAP.md` is a small,
  self-contained unit of work with explicit dependencies and acceptance criteria.
  Implement exactly one per branch; do not bundle several together. `main` stays
  shippable at every step. The roadmap's "How to use this with gstack" section
  describes the per-increment loop (`/plan-eng-review` or `/autoplan` → build →
  `/review` → `/qa` → `/ship`).
- **Preserve behavior across refactors.** Phase 1 begins by refactoring the loop
  into a steppable state machine; the single-agent path must keep working
  identically (the flight-recorder output for the demo should not regress).
- **Build, lint, and test before every commit:** `cargo build && cargo clippy --
  -D warnings && cargo test`. Do not commit code that does not compile or that
  has clippy warnings.
- **Match the existing style.** Small modules, narrow traits, minimal
  dependencies. This is meant to be a *light* runtime — justify every new crate.
- Update `docs/ROADMAP.md` (check off the increment) and any affected doc in the
  same PR as the code.

## Invariants you must preserve

- **Record everything.** Every meaningful step an agent takes emits a structured
  flight-recorder event. New behavior gets new event kinds (see the taxonomy in
  `docs/CONVENTIONS.md`). Logging is best-effort and must never crash an agent.
- **Cognition is metered.** Token/$ usage is always accounted and bounded. New
  scheduling never removes the budget guard; it builds on it.
- **Secrets come from the environment, never config or code.** `ANTHROPIC_API_KEY`
  and friends are read from env. Never log a secret. Never write one to disk.
- **Tools go behind the `Tool` trait.** Anything an agent does to the world is a
  `Tool`. **MCP is the tool ABI** — prefer exposing capabilities as MCP servers;
  native tools exist only for zero-dependency convenience.
- **The loop never panics on bad input.** Provider/tool/parse failures become
  recorded errors and `Result`, not panics.

## gstack

Use `/browse` from gstack for all web browsing. **Never use `mcp__claude-in-chrome__*` tools.**

Available skills: `/office-hours`, `/plan-ceo-review`, `/plan-eng-review`, `/plan-design-review`, `/design-consultation`, `/design-shotgun`, `/design-html`, `/review`, `/ship`, `/land-and-deploy`, `/canary`, `/benchmark`, `/browse`, `/connect-chrome`, `/qa`, `/qa-only`, `/design-review`, `/setup-browser-cookies`, `/setup-deploy`, `/setup-gbrain`, `/retro`, `/investigate`, `/document-release`, `/document-generate`, `/codex`, `/cso`, `/autoplan`, `/plan-devex-review`, `/devex-review`, `/careful`, `/freeze`, `/guard`, `/unfreeze`, `/gstack-upgrade`, `/learn`.

## Commands

Runtime code lives in `agentd/`; run cargo from there.

```bash
cd agentd

# Build
cargo build                      # debug
cargo build --release            # ~2 MB size-optimized binary

# Quality gate (run before committing)
cargo clippy -- -D warnings
cargo test

# Run an agent (logs to stderr; final answer to stdout; events to flight.jsonl)
export ANTHROPIC_API_KEY=sk-...
cargo run -- agent.toml
tail -f flight.jsonl             # watch it think
```

Build needs OpenSSL dev headers (`libssl-dev` + `pkg-config` on Debian/Ubuntu;
preinstalled on macOS) because Phase 0 uses native-tls. Phase 2 switches the
`reqwest` features to `rustls-tls` for static musl builds — see the roadmap.

## Repo layout

```
agentos/                   the repo root (run `claude` here)
  CLAUDE.md                this file
  README.md                project overview
  docs/
    DESIGN.md              full design & research (the "why")
    ROADMAP.md             the staged build plan (the work queue)
    CONVENTIONS.md         how to extend the codebase consistently
  agentd/                  the Phase 0 / Phase 1 runtime (Rust crate)
    Cargo.toml             manifest (size-optimized release profile)
    agent.toml             example agent spec
    README.md              runtime-specific quickstart
    src/
      main.rs              boot: load config -> wire gateway + tools -> run agent
      config.rs            TOML agent spec (secrets via env)
      flight_recorder.rs   append-only JSONL event log
      agent.rs             THE LOOP: perceive -> infer -> act -> observe (+ budget)
      inference/
        mod.rs             InferenceGateway trait + neutral message/tool types
        anthropic.rs       remote backend (Anthropic Messages API)
      tools/
        mod.rs             Tool trait + registry
        native.rs          built-in read_file / write_file / list_dir
        mcp.rs             real MCP stdio client -> tools
```

Future phases add siblings to `agentd/`: `distro/` (Phase 2: Buildroot + boot),
`surfaces/` (Phase 3: `/agents` FUSE), `sandbox/` (Phase 4: isolation profiles).

When in doubt about *what* to build next, the roadmap decides. When in doubt
about *how*, conventions decide. When in doubt about *why*, the design doc decides.
