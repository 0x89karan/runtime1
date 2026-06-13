# AgentOS — Onboarding Notes

A quick-reference for any Claude session picking up this project. The canonical
sources are `docs/DESIGN.md`, `docs/ROADMAP.md`, and `docs/CONVENTIONS.md` — read
those for depth. This file is the fast orientation.

---

## What this is

**AgentOS** is a minimal Linux-based OS where the primitive unit of execution is
the *agent*, not the application. `agentd` is its runtime — in the full system it
runs as PID 1 / the boot target; today it runs as an ordinary binary on a normal
distro.

The one-line thesis: replace the process with the agent as the OS abstraction.
The scheduler allocates inference slots and token budgets instead of CPU time.
Tools are the syscall table. `/agents` is the `/proc` of this system.

---

## Two locked architectural decisions

These are constitutional. Do not drift from them.

1. **Cognition is remote.** The device is a thin host. The model is an API call
   behind `InferenceGateway`. No local weights, no local inference engine. A local
   backend is permitted only as a new `impl InferenceGateway` — never as a core
   assumption.
2. **Single-tenant.** One user's agents, mutually trusting, running in-process.
   No multi-user isolation, no per-user auth. Capability *scoping between agents*
   is in scope (least-privilege), but that is not distrust.

---

## What has been built — phases 0 through 4

### Phase 0 — Single-agent spike
Full agent loop end-to-end: TOML config → `FlightRecorder` (append-only JSONL) →
`InferenceGateway` trait → `AnthropicGateway` (remote, `ANTHROPIC_API_KEY` from
env) → `Tool` trait + `ToolRegistry` → native tools (`read_file`, `write_file`,
`list_dir`) → MCP stdio client → perceive → infer → act → observe → done.

### Phase 1 — Multi-agent scheduler
- **p1.1**: `AgentTask` as a steppable state machine (`step()` → `AgentEffect`);
  `MockGateway` for tests; backward-compat `driver.rs` shim.
- **p1.2**: cooperative multi-agent scheduler (`scheduler.rs`); `agents.toml`
  multi-spec; concurrent execution.
- **p1.3**: token/$ budget guard at the scheduler level; `agent_scheduled` /
  `agent_deferred` / `agent_admission_denied` events.
- **p1.4**: per-agent capability system (`Capability` enum: `FsRead`, `FsWrite`,
  `Net`, `Spawn`); `Tool::required_capability_for`; `capability_denied` event.
- **p1.5**: `SchedulerState` refactor; shutdown drain; graceful SIGTERM.
- **p1.6**: agent identity + Agent Cards (A2A-style discovery); `send_message` /
  `list_agents` native tools; `message_sent` / `message_received` / `agent_card_registered` events.

### Phase 2 — Distro + static binary
- **p2.1**: `rustls-tls` (no OpenSSL dep); static musl binary via `cross`.
- **p2.2**: Buildroot minimal rootfs + QEMU boot; `distro/` external tree;
  `init` sh script as PID 1 (shells out to `agentd`).
- **p2.3**: QEMU virtio-net, signal handling, graceful shutdown in VM.
- **p2.4**: image size budget (3.1 MB static binary).
- **p2.5**: TODOS cleanup — MCP pagination (`nextCursor`), MCP graceful shutdown,
  `MaxTokens` → `BudgetExceeded`, sync I/O → `tokio::fs`.

### Phase 3 — Surfaces + sandbox
- **p3.1**: `/agents` FUSE virtual filesystem (`surfaces/` crate, Linux-only
  `fuser` dep). Each running agent appears as a directory with `status`,
  `context_size`, `budget`, `flight` virtual files. `SchedulerSnapshot` shared
  via `Arc<RwLock<_>>`.
- **p3.2**: Agent checkpoint / restore. `CheckpointStore` (atomic tmp→rename,
  mode 0600 since p4.4). Periodic auto-checkpoint every N turns. SIGTERM
  checkpoint. Corrupt checkpoint → rename to `.corrupt` + start fresh.
- **p3.3**: Landlock LSM + seccomp-bpf sandbox (`sandbox/` crate).
  `SandboxRule` enum: `AllowFsRead`, `AllowFsWrite`, `DenySpawn`.
  `compile()` + `apply_compiled()` via `pre_exec` in `McpClient::spawn`.
  `capabilities` field on `[[tools.mcp_servers]]`. `SandboxApplied` / `SandboxSkipped` events.

### Phase 4 — Isolation hardening
- **p4.1**: per-tool sandbox (`mcp_require_capabilities = true` flag; startup
  fails if any MCP server has no sandbox rules). `EnforcementStatus` struct +
  `SandboxApplied.enforced` payload.
- **p4.2**: stronger isolation options — `IsolateNetwork` (Linux network
  namespace via `unshare(CLONE_NEWNET)`) + `IsolateMount` namespace; `isolation
  = "gvisor"` wraps with `runsc do`. `caps_to_rules()` adds `IsolateNetwork`
  when `Net` is absent.
- **p4.3**: security review pass. `THREAT_MODEL.md` added. Flight-recorder field
  redaction (secrets/tokens never logged). `PREVIEW_CHARS` constant.
- **p4.4**: TODOS cleanup sprint — checkpoint mode 0600 (`write_mode_600()`),
  pre_exec error pipe (distinguishes sandbox failure from missing binary),
  `--no-fuse` CLI flag + `AGENTOS_NO_FUSE` env var, `FuseSkipped` event,
  `sandbox_probe` integration tests.
- **p4.5**: TODOS cleanup sprint — `EventKind` extracted to `events.rs`,
  aarch64 `DenySpawn` noop detection (`SandboxSkipped` reason
  `"deny-spawn-unsupported-arch"`), `--log-path` CLI flag + `log_path` TOML
  field, Buildroot ccache, 244 tests.
- **p4.6**: Landlock V4 TCP port enforcement. `AllowNetConnect { port: u16 }`
  in `SandboxRule`. `Net { hosts, ports: Vec<u16> }` capability (`#[serde(default)]`
  backward compat). ABI version queried at runtime — V4 (kernel ≥ 6.7) activates;
  older kernels degrade silently. `EnforcementStatus.landlock_net`. `run_probe`
  now honours `--log-path`. CRITICAL BUG fixed: net-only configs previously caused
  complete FS lockout (handled_access_fs set with zero path rules). 253 tests.

**Current version: v0.16.0. Phase 4 complete. Phase 5 not yet planned.**

---

## Repo layout

```
agentos/
  CLAUDE.md            project instructions + invariants (read this first)
  CHANGELOG.md         per-version change log
  TODOS.md             open items + completed with resolution notes
  docs/
    DESIGN.md          thesis, architecture, rationale
    ROADMAP.md         ordered increment work queue
    CONVENTIONS.md     how to extend consistently
    THREAT_MODEL.md    security model + known gaps
  agentd/              the runtime (Rust workspace member)
    src/
      main.rs          boot, config loading, scheduler wiring, caps_to_rules()
      config.rs        TOML spec types
      events.rs        EventKind enum (re-exported from flight_recorder)
      flight_recorder.rs  append-only JSONL event log
      scheduler.rs     cooperative multi-agent scheduler
      checkpoint.rs    CheckpointStore, AgentCheckpoint, SchedulerCheckpoint
      agent/
        mod.rs         AgentTask state machine: step() → AgentEffect
        driver.rs      single-agent backward-compat shim
      inference/
        mod.rs         InferenceGateway trait + neutral Block/Msg/ToolSpec types
        anthropic.rs   Anthropic Messages API backend
      tools/
        mod.rs         Tool trait + ToolRegistry
        native.rs      read_file, write_file, list_dir, send_message, list_agents,
                       spawn_agent
        mcp.rs         MCP stdio client → tools (pagination, graceful shutdown)
  surfaces/            FUSE /agents filesystem (Linux-only)
    src/
      lib.rs           re-exports + AgentsFs module
      snapshot.rs      SchedulerSnapshot / AgentSnapshot / AgentStatus
      agents_fs.rs     FUSE handler + mount() (Linux); stub (others)
  sandbox/             kernel sandbox for MCP subprocesses
    src/
      lib.rs           SandboxRule, CompiledSandbox, compile(), apply_compiled()
  distro/              Buildroot external tree + QEMU boot
    Makefile           build / run / test / prereqs / clean
    buildroot.config   Buildroot defconfig (x86_64 musl, busybox, cpio.gz)
    kernel-extras.config  virtio-net, virtio-9p, FUSE, SECCOMP
    overlay/
      init             /init PID-1 sh script
      etc/agentd/agent.toml  demo agent config
```

---

## Key invariants (never violate)

- **Record everything.** Every meaningful agent step emits a structured
  flight-recorder event (`rec.record(turn, kind, data)`). New behavior gets new
  event kinds. Logging is best-effort — never crash an agent from a log failure.
- **Cognition is metered.** Token/$ usage is always accounted. New scheduling
  never removes the budget guard.
- **Secrets from env only.** `ANTHROPIC_API_KEY` and friends are env vars.
  Never log a secret. Never write one to disk.
- **Tools go behind `Tool`.** Anything an agent does to the world is a `Tool`.
  MCP is the tool ABI — prefer MCP servers over native tools for real capabilities.
- **The loop never panics on bad input.** Provider/tool/parse failures become
  recorded errors and `Result`, not panics.

---

## Development commands

```bash
# All commands run from agentd/
cd agentd

cargo build                        # debug
cargo build --release              # release (~3.1 MB static musl via cross)
cargo clippy -- -D warnings        # must be clean before commit
cargo test                         # must pass before commit

# Linux-gated code (any #[cfg(target_os = "linux")] change):
make clippy-linux                  # from repo root — runs in Docker
make test-linux                    # from repo root — full suite on Linux

# Run an agent
export ANTHROPIC_API_KEY=sk-...
cargo run -- agent.toml            # single agent
cargo run -- agents.toml           # multi-agent
tail -f flight.jsonl               # watch events

# Static musl binary (requires `cross` + Docker)
cross build --target x86_64-unknown-linux-musl --release
```

---

## Open TODOS (unresolved)

- **`clone3()` bypass in namespace-only sandbox** — `DenySpawn` blocks `fork`/`vfork`
  but not `clone`/`clone3`. `isolation = "gvisor"` fully mitigates. Accepted limitation
  for namespace-only mode.
- **PID namespace via `unshare()` only affects future children** — MCP server itself
  stays in parent PID namespace. Needs double-fork with pipe. Deferred.
- **`runsc do` experimental** — gVisor `isolation = "gvisor"` uses undocumented `runsc do`.
  Production-grade OCI bundle integration deferred.
- **Checkpoint encryption at rest** — mode 0600 restricts access but no encryption.
  Noted in THREAT_MODEL.md §3.3.
- **stdout ordering for multi-agent** — answers printed in completion order, not config
  order. Fine for now; an ordered-output flag is future work.

---

## gstack workflow

The standard loop for each increment is:

```
/plan-eng-review   →   build   →   /review   →   /qa   →   /ship
```

One branch per increment (`p5.1-some-feature`). Never bundle two increments.
`main` stays shippable at every merge. Run `make clippy-linux` before pushing any
Linux-gated code.
