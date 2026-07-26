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

**Current version:** v0.110.0 (shipped 2026-07-26)
<!-- Updated on every release; test-enforced against agentd/Cargo.toml by
     agentd/tests/repo_consistency.rs — a stale line here fails cargo test. -->

**Latest shipped:** doc.1 (v0.110.0) — audit-tail doc-drift fix (P3-6): CONVENTIONS event-taxonomy +
FUSE-surface tables brought current + a new `conventions_completeness` test that fails on event-table
drift (scoped to the taxonomy table, not whole-doc), THREAT_MODEL §1.2/§5.2 corrected for the broker +
CI cargo-audit, RUNBOOK/cos-guide passenv fixes, `cockpit.toml [memory]` block added. /review (Codex)
caught its own drift — the first inode-guidance rewrite was wrong (real invariant `OFF_* < DIR_STEP - 1`)
and the completeness test was substring-weak; both fixed. (p3.1 v0.109.0 is TAGGED + deployed.)

**AUDIT-v0.97 remediation — COMPLETE** (sweep + tail, v0.98.0→v0.109.0). Full audit: `docs/AUDIT-v0.97.md`.
Every increment ran plan→build→review→qa→ship; a holistic cross-model /review + per-increment /autoplan
reshaped scope in both directions (killed 2 over-scoped refactors, upgraded 1, struck 1 audit item as
a data-loss do-not-do).
- **Sweep stack** (v0.98–0.103): audit.2 (arm64 python, checkpoint `.restored`, ux.13 resurrection),
  run.1 (durability: flight rotation, `short_term` cap, cron catch-up, runs retention), cap.4
  (auth-consistency: whole-surface :7999 gate + deny-by-default `/spawn` + tool_override KB scoping),
  ci.2 (test blind-spots), budget.1 (metering completeness — universal spend folded into the global
  window + MaxTokens self-brick guard + universal-cancel), par.1 (drift guards). The holistic /review
  then fixed 2 escaped defects (FsRead `/spawn` exfil → privileged; checkpoint corrupt-primary →
  `.restored` fallback).
- **par.2** — config-unification RESHAPED to docs-only (`${VAR}` expansion can't express the deliberate +
  test-pinned structural Docker/QEMU config divergence).
- **hardening.1** (v0.104.0) — test+safety batch + unbroke `main`'s docker-smoke (ci.2 in-image oauth
  fixture escaped bug).
- **Behavioral:** par.1-ar-01 (v0.105.0) operator error view surfaces real tool/inference errors;
  budget.1-ar-01 (v0.106.0) MaxTokens truncation role-gate — one-shot fails, resident still parks (P0-2
  preserved) via a new `AgentEffect::CompletedTruncated`.
- **Design cluster:** cap.3 (v0.107.0) FS-capability matching anchored to startup CWD + closed a p5.8
  boot containment hole; budget.1-ar-02 (v0.108.0, P-doc) universal soft-cap documented honestly
  (reservation deferred — dormant path, single-tenant spend guardrail).
- **P3 tail:** p3.1 (v0.109.0) scheduler never aborts on a missing-agent effect + orphaned-checkpoint-tmp
  sweep. (audit86-P3-4 struck: bumping FORMAT_VERSION would cause rollback data-loss.)

**Audit tail — effectively closed.** par.3 (`agent)`-mode sed retirement) **DEFERRED at its /autoplan
premise gate** (2026-07-26): both CEO voices ranked it below the UX tail with zero user value (Codex STRIKE);
the guard "blind spot" that might have justified a cheap hardening was code-verified as overstated
(`entrypoint.sh:369`'s audit.1 ERE already fails the boot closed on a surviving installed-absolute path).
The working sed stays; revisit only as a build-time generator if it ever matters (`docs/plans/par.3-*.md`).
Only residual: port-7999 shared constant (trivial low-value config dedup).

**Next (roadmap):** the UX tail — ux.2b/ux.3/ux.10 (the last picks up the deferred ux.13 cancel-key),
then evidence-gated ux.6/ux.5/ux.7; Phase 11 skills + Phase 9 eBPF remain the two end-of-queue tracks.

Full per-increment completion notes: `docs/STATUS.md`.

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
- **Build, lint, and test before every commit — workspace-wide, from the repo
  root:** `cargo build --workspace && cargo clippy --workspace --all-targets --
  -D warnings && cargo test --workspace`. CI enforces exactly this across all
  five crates (ci.1) — per-crate commands from `agentd/` miss
  surfaces/sandbox/otel lints and go red in CI. (First workspace run rebuilds
  into the root `target/` — one-time cost.) Do not commit code that does not
  compile or that has clippy warnings.
- **Every version bump updates the "Current version" line in this file.** The
  line at the top of "Current status" is test-enforced against
  `agentd/Cargo.toml` (`agentd/tests/repo_consistency.rs`) — a release commit
  that bumps Cargo.toml without updating CLAUDE.md fails CI.
- **Linux-gated code requires a Linux clippy pass before pushing.** Any code
  under `#[cfg(target_os = "linux")]` (e.g. `surfaces/src/agents_fs.rs`) is
  never compiled on macOS, so local clippy is a false green. Run
  `make clippy-linux` from the repo root (requires Docker) before pushing a
  branch that touches Linux-gated code. This mirrors the CI step exactly.
- **aarch64-gated code requires an aarch64 clippy pass before pushing.** Any code
  under `#[cfg(target_arch = "x86_64")]` or `#[cfg(not(target_arch = "x86_64"))]`
  (e.g. `sandbox/src/lib.rs` DenySpawn gate) has different behavior on aarch64.
  Run `make clippy-aarch64` from the repo root (requires Docker and `cross` installed
  via `cargo install cross --locked`) before pushing a branch that changes
  arch-conditional behavior. `Cross.toml` at the repo root pins the Docker image
  version so `ring`'s `build.rs` gets the correct `aarch64-linux-musl-gcc`.
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

Runtime code lives in `agentd/`; run agents from there. The pre-commit quality
gate is workspace-wide and runs from the **repo root** (see "How to work here").

```bash
cd agentd

# Build
cargo build                      # debug
cargo build --release            # ~2 MB size-optimized binary

# Quality gate (run before committing) — from the REPO ROOT, not agentd/
(cd .. && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace)

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
    STATUS.md              detailed per-increment completion notes (this file's old log)
    CONVENTIONS.md         how to extend the codebase consistently
    SPIKES/                exploratory spike docs (implementation notes per increment)
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
  templates/               Phase 6: agent template catalogue (p6.1+)
    scout.template.toml    read-only researcher; first catalogue entry
  surfaces/                Phase 3: system surfaces (p3.1+)
    Cargo.toml             manifest (fuser dep Linux-only)
    src/
      lib.rs               re-exports snapshot types + agents_fs module
      snapshot.rs          SchedulerSnapshot / AgentSnapshot / AgentStatus
      agents_fs.rs         AgentsFs FUSE handler + mount() (Linux); stub (others)
  sandbox/                 Phase 3: kernel sandbox for MCP subprocesses (p3.3+)
    Cargo.toml             manifest (Linux-only raw syscall dependencies)
    src/
      lib.rs               SandboxRule enum + CompiledSandbox + compile()/apply_compiled()
  distro/                  Phase 2: Buildroot external tree + QEMU boot
    Makefile               build / run / test / prereqs / clean
    buildroot.config       Buildroot defconfig (x86_64 musl, busybox, cpio.gz)
    kernel-extras.config   kernel fragment: virtio-net + virtio-9p + FUSE + SECCOMP
    overlay/
      init                 /init PID-1 sh script
      agents/              mount point for /agents FUSE filesystem (p3.1)
      usr/bin/agentd       (gitignored; copied by make build)
      etc/
        resolv.conf        nameserver 10.0.2.3 (QEMU SLIRP DNS)
        agentd/
          agent.toml       demo agent config
```

Phase 6 adds further siblings: `agentctl/` (p6.2 operator CLI), more templates (p6.7 starter catalogue).

`agentctl/` layout (p6.2+):

```
agentctl/                operator CLI binary
  src/
    main.rs              arg dispatch
    list.rs              list-templates subcommand (p6.2)
    spawn.rs             spawn <template> subcommand (p6.2)
    inject.rs            inject <id> <text> subcommand (p7.3+)
    orchestrate.rs       orchestrate REPL — spawn + multi-turn SSE loop (orch.1+)
    watch/
      mod.rs             watch entry point; run_plain / run_tui
      app.rs             App state machine + View enum
      reader.rs          reads /agents/ FUSE files → AgentInfo
      views.rs           ratatui render functions
      topology.rs        TopologyGraph + build_graph() + render_tree() (p6.4)
```

`agentd/coordinator-demo.agents.toml` — multi-agent fixture for topology testing (coordinator + 2 scouts).

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
