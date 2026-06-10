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

**Phases 0–2 complete; Phase 3 in progress (p3.1 landed).** `agentd/` is a
working Rust binary. Phases 0–2 built the full single/multi-agent loop, config,
flight recorder, Anthropic gateway, tool ABI, native tools, MCP stdio client,
cooperative scheduler, capability system, agent spawning, agent cards, rustls
static binary, Buildroot rootfs + QEMU boot, signal handling, MCP pagination,
and graceful shutdown.

Phase 3 (Surfaces):

- **p3.1** (done): `/agents` FUSE virtual filesystem — `surfaces/` crate;
  `AgentsFs` + `SchedulerSnapshot`; each running agent appears as a directory
  with `status`, `context_size`, `budget`, `flight` virtual files; inode scheme
  (root=1, dirs from 1010 step 10); `Arc<RwLock<SchedulerSnapshot>>` shared
  between scheduler and FUSE handler; `FuseMounted`/`FuseUnmounted` flight
  events; `fuser` dep Linux-only; `CONFIG_FUSE_FS=y` in kernel-extras.config;
  15 unit tests in `surfaces`.

**Next: `p3.2` — Agent checkpoint / restore. See `docs/ROADMAP.md`.**

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
- **Linux-gated code requires a Linux clippy pass before pushing.** Any code
  under `#[cfg(target_os = "linux")]` (e.g. `surfaces/src/agents_fs.rs`) is
  never compiled on macOS, so local clippy is a false green. Run
  `make clippy-linux` from the repo root (requires Docker) before pushing a
  branch that touches Linux-gated code. This mirrors the CI step exactly.
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
cargo run -- agent.toml          # single agent
cargo run -- agents.toml         # multiple agents concurrently (p1.2+)
tail -f flight.jsonl             # watch it think
```

No OpenSSL dependency since p2.1 (`rustls-tls`). For a static musl build:
```bash
# requires `cross` (cargo install cross) and Docker
cross build --target x86_64-unknown-linux-musl --release
```

## Repo layout

```
agentos/                   the repo root (run `claude` here)
  CLAUDE.md                this file
  README.md                project overview
  CHANGELOG.md             notable changes per release
  TODOS.md                 open technical-debt items and completed increments
  docs/
    DESIGN.md              full design & research (the "why")
    ROADMAP.md             the staged build plan (the work queue)
    CONVENTIONS.md         how to extend the codebase consistently
  agentd/                  the runtime (Rust crate)
    Cargo.toml             manifest
    agent.toml             single-agent example spec
    agents.toml            multi-agent example spec (p1.2+)
    README.md              runtime-specific quickstart
    src/
      main.rs              boot: load config -> wire gateway + tools -> run scheduler
      config.rs            TOML agent spec (single [agent] + multi [[agents]] forms)
      flight_recorder.rs   append-only JSONL event log
      scheduler.rs         cooperative multi-agent scheduler (p1.2+)
      agent/
        mod.rs             AgentTask state machine: step() → AgentEffect (p1.1+)
        driver.rs          single-agent backward-compat shim
      inference/
        mod.rs             InferenceGateway trait + neutral message/tool types
        anthropic.rs       remote backend (Anthropic Messages API)
      tools/
        mod.rs             Tool trait + registry
        native.rs          built-in read_file / write_file / list_dir
        mcp.rs             real MCP stdio client -> tools
  surfaces/                Phase 3: system surfaces (p3.1+)
    Cargo.toml             manifest (fuser dep Linux-only)
    src/
      lib.rs               re-exports snapshot types + agents_fs module
      snapshot.rs          SchedulerSnapshot / AgentSnapshot / AgentStatus
      agents_fs.rs         AgentsFs FUSE handler + mount() (Linux); stub (others)
  distro/                  Phase 2: Buildroot external tree + QEMU boot
    Makefile               build / run / test / prereqs / clean
    buildroot.config       Buildroot defconfig (x86_64 musl, busybox, cpio.gz)
    kernel-extras.config   kernel fragment: virtio-net + virtio-9p + FUSE
    overlay/
      init                 /init PID-1 sh script
      agents/              mount point for /agents FUSE filesystem (p3.1)
      usr/bin/agentd       (gitignored; copied by make build)
      etc/
        resolv.conf        nameserver 10.0.2.3 (QEMU SLIRP DNS)
        agentd/
          agent.toml       demo agent config
```

Future phases add siblings to `agentd/`: `sandbox/` (Phase 4: isolation profiles).

When in doubt about *what* to build next, the roadmap decides. When in doubt
about *how*, conventions decide. When in doubt about *why*, the design doc decides.

## Skill routing

When the user's request matches an available skill, invoke it via the Skill tool. When in doubt, invoke the skill.

Key routing rules:
- Product ideas/brainstorming → invoke /office-hours
- Strategy/scope → invoke /plan-ceo-review
- Architecture → invoke /plan-eng-review
- Design system/plan review → invoke /design-consultation or /plan-design-review
- Full review pipeline → invoke /autoplan
- Bugs/errors → invoke /investigate
- QA/testing site behavior → invoke /qa or /qa-only
- Code review/diff check → invoke /review
- Visual polish → invoke /design-review
- Ship/deploy/PR → invoke /ship or /land-and-deploy
- Save progress → invoke /context-save
- Resume context → invoke /context-restore
- Author a backlog-ready spec/issue → invoke /spec
