# Changelog

All notable changes to agentd are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [p4.4] - 2026-06-13 (v0.14.0)

### Added
- **`checkpoint.json` mode 0600**: `write_mode_600()` creates the tmp file with
  `O_CREAT|O_EXCL|mode(0o600)` plus unlink-retry, guaranteeing 0600 even if a
  stale tmp file exists at a different mode. `rename(2)` atomically replaces the
  final `checkpoint.json`. Checkpoint is now owner-readable only regardless of umask.
- **pre_exec sandbox error pipe**: `McpClient::spawn` on Linux creates a
  `pipe2(O_CLOEXEC)` error pipe *only* when a sandbox is configured. On spawn
  failure the error message includes `"(sandbox stage: 'sandbox'|'unknown')"` so
  operators can distinguish a sandbox-apply failure from a missing-binary error.
  Unsandboxed servers produce a clean error without the stage suffix.
- **`--no-fuse` CLI flag + `AGENTOS_NO_FUSE` env var**: `agentd --no-fuse agent.toml`
  or `AGENTOS_NO_FUSE=1 agentd agent.toml` skips the FUSE mount and emits a
  `FuseSkipped` flight event. `AGENTOS_NO_FUSE=0/false/no` correctly disables the
  flag (any other non-empty value enables it). Makes CI output clean.
- **`EventKind::FuseSkipped`**: new flight event kind emitted when `--no-fuse` is
  active; preserves the CONVENTIONS.md invariant that every meaningful step is
  recorded (analogous to `SandboxSkipped`).
- **`sandbox_probe` integration tests (Linux)**: 3 tests in `tests/integration.rs`
  — `allowed_path_read_succeeds`, `denied_path_read_fails`, `deny_spawn_blocks_fork`
  (x86_64 only) — verify Landlock + seccomp enforcement end-to-end using the
  `sandbox_probe` fixture binary.

### Fixed
- THREAT_MODEL.md §3.2–3.3: updated to reflect checkpoint mode restriction.

## [p4.3] - 2026-06-12 (v0.13.0)

### Added
- **`docs/THREAT_MODEL.md`**: full threat model covering secret handling,
  flight-recorder data sensitivity, checkpoint.json exposure, budget-exhaustion DoS
  guards, supply chain posture, and sandbox bypass vectors (BP-1 through BP-6) with
  explicit "not yet fixed" labels for each known gap.

### Fixed
- **`ToolCall` event now logs `input_preview` (≤200 chars) instead of the full,
  untruncated tool input**: prevents large file contents and any short secrets
  passed as tool arguments from landing verbatim in `flight.jsonl`.
- **`ToolResult` error path now logs `error` as ≤200-char preview**: previously the
  error message (which may echo back tool arguments) was logged verbatim.
- **`AgentSpawned` event now logs `task_preview` (≤200 chars) instead of the full
  task string** on both the TOML-config path (`main.rs`) and the dynamic spawn path
  (`scheduler.rs`); both now use `truncate()` with the `…` truncation marker.
- **`truncate()` and `PREVIEW_CHARS` made `pub` in `agentd::agent`**: previously
  private, preventing reuse from `main.rs` and `scheduler.rs`.

### Known Limitations (TODOS.md)
- `checkpoint.json` has no encryption or restricted file permissions; tracked as
  P3 TODOS entry for a future increment.
- 200-char truncation does not prevent short secrets (≤200 chars) in tool
  arguments; operational guidance: pass secrets via environment, not tool inputs.
- `cargo audit` CVE scanning not yet in CI.

### Tests
- All 216 tests pass (macOS; +1 new unit test for ToolResult error truncation).

## [p4.2] - 2026-06-11

### Added
- **`IsolateNetwork` and `IsolateMount` `SandboxRule` variants**: applied via `unshare(CLONE_NEWUSER | CLONE_NEWNET/CLONE_NEWNS)` in `pre_exec`. BestEffort degradation if kernel policy blocks user namespaces (`EPERM`/`ENOSYS`).
- **`Net` capability now enforced at kernel level**: `caps_to_rules()` adds `IsolateNetwork` whenever the `Net` capability is absent. MCP servers without an explicit `Net` grant are network-isolated by default. Previously `Net` was advisory-only.
- **`isolation = "gvisor"` field on `[[tools.mcp_servers]]`**: wraps the server command with `runsc do [--network=none] --`. agentd fails fast at startup if `runsc` is not found on PATH. gVisor's Sentry handles all syscall interception — Landlock/seccomp/namespace pre_exec is skipped for gVisor-mode servers.
- **`EnforcementStatus` extended**: `namespace_net: bool` and `namespace_mount: bool` fields added. `SandboxApplied` event payload extended with `isolation`, `namespace_net`, `namespace_mount` fields.
- **`CONFIG_USER_NS=y`, `CONFIG_NET_NS=y`, `CONFIG_UTS_NS=y`** in `distro/kernel-extras.config` for QEMU image.

### Changed
- **Breaking:** `capabilities = []` now also produces `IsolateNetwork` (network-isolated). Previously it produced only `DenySpawn`. Servers that need outbound access must add `Net` to their capabilities list.
- **`capabilities = ["Spawn"]` behavior**: previously produced empty rules (caught by `mcp_require_capabilities` as a bypass). Now produces `[IsolateNetwork]` — a real enforcement rule. The config is valid; the server can spawn children but cannot reach the network.

### Known Limitations (TODOS.md)
- `runsc do` is experimental; full OCI bundle integration deferred.
- `clone3()` bypass remains in the namespace-only path (gVisor fixes it).
- `CLONE_NEWPID` for PID namespace requires a re-fork; deferred.

### Tests
- **209 tests pass** (macOS + CI).
- 8 new sandbox unit tests (`isolate_network/mount` variants, `enforcement_status` namespace fields).
- 3 new config unit tests (`isolation` field parsing).
- 7 updated `caps_to_rules` unit tests reflecting `IsolateNetwork` default.
- 1 new integration test: `isolation_gvisor_fails_fast_when_runsc_not_on_path` (Linux only).

## [p4.1] - 2026-06-11

### Added
- **`EnforcementStatus` struct** in `sandbox/src/lib.rs`: `{ landlock: bool, seccomp: bool, spawn_enforcement: &'static str }` — returned by `CompiledSandbox::enforcement_status()` and included in `SandboxApplied` flight events, so operators can distinguish kernels where Landlock or seccomp degraded to a no-op.
- **`mcp_require_capabilities = true`** flag in `[tools]` config: when set, startup fails if any MCP server would run unsandboxed (missing `capabilities` field OR field present but `caps_to_rules()` produces empty rules). Lists all offending server names in the error message.
- **CI binary size guard**: new workflow step checks that the x86_64-unknown-linux-musl release binary is ≤ 4 MB (4 194 304 bytes); fails with a clear message if exceeded.

### Fixed
- **aarch64 BPF gate**: seccomp-bpf fork/vfork block is now gated under `#[cfg(target_arch = "x86_64")]`. On aarch64 (and other non-x86_64 arches), `DenySpawn` emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }` instead of installing a no-op filter that silently claims enforcement.
- **`compile()` moved to `main.rs`**: `McpClient::spawn` no longer calls `compile()` internally. The parent compiles rules before fork and passes `Option<CompiledSandbox>` directly, keeping the child's `pre_exec` closure allocation-free.
- **`mcp_require_capabilities` bypass**: validation now calls `caps_to_rules()` to check for empty effective rules, not just `capabilities.is_none()`. `capabilities = ["Spawn"]` (which maps to zero kernel rules) is correctly rejected.
- **`SandboxSkipped` on non-Linux with capabilities**: the `had_sandbox` variable is captured before the compiled sandbox is consumed by `McpClient::spawn`, fixing a case where the non-Linux `SandboxSkipped` event was never emitted for servers with capabilities configured.
- **Misleading sandbox log**: the "MCP server running unsandboxed" warning now distinguishes between "no capabilities field" and "capabilities produce no effective rules".

### Tests
- **208 tests pass** (macOS + CI).
- 6 `EnforcementStatus` unit tests in `sandbox/src/lib.rs`.
- 4 `mcp_require_capabilities` integration tests in `agentd/tests/mcp.rs`, including a regression test for the `capabilities = ["Spawn"]` bypass.
- `MAX_BYTES` named constant replaces bare `4194304` in the CI size guard script.

## [p3.3] - 2026-06-11

### Added
- **`sandbox/` crate**: new Rust library crate (`sandbox`) in the workspace. Provides
  kernel-level enforcement for MCP server subprocesses via two mechanisms:
  - **Landlock LSM** (Linux 5.13+): filesystem path-beneath rules. `AllowFsRead { prefix }`
    grants `ReadFile | ReadDir`; `AllowFsWrite { prefix }` grants all ABI V1 flags except
    Execute. BestEffort — degrades silently on older kernels without breaking startup.
  - **seccomp-bpf** (`DenySpawn` rule): classic BPF filter installed in `pre_exec` that
    blocks `fork(2)` and `vfork(2)` on x86_64, preventing the MCP server from spawning
    new child processes. Exec is intentionally left unblocked (the initial `execve` that
    loads the MCP binary must succeed); Landlock FS rules persist across exec.
- **`capabilities` field on `[[tools.mcp_servers]]`**: optional array of capability objects
  (`FsRead { prefix }`, `FsWrite { prefix }`, `Net { hosts }`, `Mcp { server, tools }`,
  `Spawn`). When present, a sandbox is compiled and applied to the server subprocess before
  exec. When absent, the server runs unsandboxed with a `tracing::warn!` and a
  `SandboxSkipped` flight event. `capabilities = []` with no `Spawn` produces a
  `DenySpawn`-only sandbox (fork/vfork blocked; no FS restriction).
- **`caps_to_rules()` adapter** in `main.rs`: converts agent `Capability` values to
  `SandboxRule` values — `FsRead`/`FsWrite` map 1:1; `Spawn` suppresses `DenySpawn`;
  `Net`/`Mcp` are advisory (kernel-level net enforcement deferred to Landlock ABI V4).
- **`EventKind::SandboxApplied` / `SandboxSkipped`**: emitted in `flight.jsonl` after
  each MCP server spawn, recording which rules were applied or why the sandbox was skipped.
- **`CONFIG_SECCOMP=y` / `CONFIG_SECCOMP_FILTER=y`** added to `distro/kernel-extras.config`.
- **`docs/SPIKES/p3.3-ebpf-lsm.md`**: implementation spike doc covering raw syscall ABI,
  BPF filter construction, execute-bit exclusion, known limitations, and CI gate.

### Fixed
- **`O_NOFOLLOW` on Landlock path fds**: `open_path_fd` now passes `O_NOFOLLOW` so a
  symlink at the configured prefix cannot redirect the Landlock allowance to another dir.
- **`SandboxApplied` accuracy**: only emitted on Linux (non-Linux is a no-op platform);
  not emitted when compiled rules are empty (e.g. `capabilities = [{ Spawn }]` only).
- **Empty `caps_to_rules` result treated as no sandbox**: `capabilities=[{Spawn}]` maps to
  zero kernel rules and now correctly emits `SandboxSkipped` rather than a misleading
  `SandboxApplied { rules: [] }`.

### Tests
- **180 tests pass** (macOS + CI); Linux-gated tests (`allow_fs_write_*`, `combined_fs_*`,
  `deny_spawn_bpf_includes_vfork_on_x86_64`) verified by CI.
- 6 `caps_to_rules` unit tests in `main.rs`.
- 3 `McpServerConfig` capability TOML parse tests in `config.rs`.
- 1 `sandbox_event_kinds_serialize_to_snake_case` test in `flight_recorder.rs`.
- 5 sandbox-crate tests: `PartialEq`, Landlock rule construction, combined Landlock+BPF,
  vfork BPF instruction count (expects 6: `load + fork + vfork + allow`).

## [p3.2] - 2026-06-10

### Added
- **`agentd/src/checkpoint.rs`**: new module — `CheckpointStore` (atomic
  `tmp → rename` writes), `AgentCheckpoint`, `SchedulerCheckpoint`,
  `AwaitingEntry` serde types; `FORMAT_VERSION = 1`.
- **`AgentTask::to_checkpoint()`** / **`from_checkpoint()`** / **`is_terminal()`**:
  serialise/deserialise agent working state; `from_checkpoint` always clears
  `terminal` to guard against the terminal-race (OV-2); `is_terminal` lets the
  scheduler filter finished agents from checkpoint writes.
- **Periodic auto-checkpoint**: `SchedulerConfig::checkpoint_interval_turns`
  (default `1`); fires at every `provide_tool_results` boundary when the agent
  turn count is a non-zero multiple of the interval.
- **SIGTERM checkpoint**: when the scheduler's SIGTERM handler fires it calls
  `checkpoint_all()` before exiting; if the save fails the error is recorded and
  shutdown continues without crashing.
- **Corrupt-checkpoint recovery**: if `checkpoint.json` exists but fails to
  parse, `main.rs` renames it to `checkpoint.json.corrupt` and boots fresh.
- **Full restore**: `Scheduler::new()` accepts an optional `SchedulerCheckpoint`;
  restores `awaiting` map, per-agent mailboxes, `tokens_spent`, `child_seq`, and
  `spawn_depths`; orphan children in the checkpoint (not in the TOML spec) are
  also restored.
- **New flight events**: `AgentCheckpointed { agent_id }`,
  `AgentRestored { agent_id }`, `CheckpointFailed { reason }`.
- **`agentd/.gitignore`**: `checkpoint.json` and `checkpoint.json.corrupt`
  excluded from version control.

### Changed
- `SchedulerConfig` gains `checkpoint_interval_turns: u32`; default `1`; `0`
  disables periodic checkpointing.
- `Scheduler::new()` signature gains a 7th argument
  `Option<SchedulerCheckpoint>`; existing call-sites in `main.rs` updated.
- `InferenceResponse` and `MailMessage` derive `Serialize` (required by checkpoint
  serialisation).
- `Makefile` `clippy-linux` target: add `rustup component add clippy` before the
  cargo invocation so the Docker image works on aarch64 hosts.
- Test helper `sched_cfg()` sets `checkpoint_interval_turns: 0` to prevent
  concurrent scheduler tests from racing on `./checkpoint.json.tmp`; dedicated
  checkpoint tests explicitly opt in with `checkpoint_interval_turns: 1`.

### Tests
- 9 new unit tests in `agentd/src/scheduler.rs` (checkpoint restore, periodic
  checkpoint, `AgentCheckpointed` flight event, test-isolation mutex for
  `sigterm_drains_scheduler`).
- 5 new unit tests in `agentd/src/agent/mod.rs` (`is_terminal`, `to_checkpoint`,
  `from_checkpoint`, roundtrip).
- 1 new unit test in `agentd/src/flight_recorder.rs` (checkpoint event
  serialisation).
- 10 unit tests in `agentd/src/checkpoint.rs` (serde roundtrips, save/load,
  corrupt handling).
- Total: **175 tests** (174 pass; 1 live-API integration skipped).

## [p3.1] - 2026-06-10

### Added
- **`surfaces/` crate**: new Rust library crate (`surfaces`) sibling to `agentd/`;
  root `Cargo.toml` promoted to a workspace with `members = ["agentd", "surfaces"]`
  and the release profile moved there.
- **`surfaces::snapshot`**: `SchedulerSnapshot`, `AgentSnapshot`, `AgentStatus`
  (`Running`, `Deferred`, `AwaitingChild(String)`, `Done`, `Failed`); shared via
  `Arc<RwLock<SchedulerSnapshot>>` between scheduler and FUSE handler.
- **`surfaces::agents_fs`** (Linux-only FUSE handler): `AgentsFs` implements
  `fuser::Filesystem`; inode scheme (root=1, agent dirs from 1010 step 10, file
  offsets +1..+4); four virtual files per agent (`status`, `context_size`, `budget`,
  `flight`); TTL=0 (no kernel caching); `read_flight_tail()` scans last 64 KB of
  `flight.jsonl`, returns up to 20 matching lines per agent.
- **`surfaces::agents_fs::mount()`**: spawns FUSE `BackgroundSession` on Linux;
  no-op stub on other platforms — clean build everywhere.
- **`Scheduler` snapshot plumbing**: `Scheduler::new()` accepts a 7th argument
  `Arc<RwLock<SchedulerSnapshot>>`; `update_snapshot()` is called after the seed loop
  and after every effect result, keeping the snapshot current.
- **`AgentTask` getters**: `context_tokens()` and `task_preview(max_chars)` added
  to `agent/mod.rs` for snapshot population.
- **`EventKind::FuseMounted` / `FuseUnmounted`**: emitted in `main.rs` when
  `agentd` mounts/unmounts `/agents`.
- **`distro/overlay/agents/.gitkeep`**: creates the `/agents` mount point in the
  Buildroot rootfs overlay.
- **`CONFIG_FUSE_FS=y`** in `distro/kernel-extras.config` so the QEMU VM can
  serve FUSE mounts.
- **15 unit tests** in `surfaces/src/agents_fs.rs` covering inode allocation, file
  content rendering, read slicing, and flight tail parsing.

### Changed
- **`fuser` dependency** is in `[target.'cfg(target_os = "linux")'.dependencies]`
  to avoid `pkg-config --libs fuse` failing on macOS during `cargo check/test`.
- All `#[cfg(target_os = "linux")]`-gated items that are also needed by tests use
  `#[cfg(any(test, target_os = "linux"))]` so the test suite runs on all platforms.

## [p2.5] - 2026-06-09

### Added
- **MCP tools/list pagination**: `McpClient::spawn` now follows `nextCursor` in a
  cursor-based loop until all pages are exhausted. Previously only the first page was
  fetched; tools on page 2+ were silently dropped.
- **`McpClient::shutdown()` method**: sends `notifications/shutdown` (JSON-RPC notification,
  no id), waits up to 5 s for the server to exit cleanly, then escalates to SIGTERM, waits
  another 5 s, and lets `kill_on_drop` deliver the final SIGKILL. Servers that flush WAL or
  release locks on clean exit now get the chance to do so.
- **Graceful shutdown on all exit paths**: `run_agent` in `main.rs` calls
  `client.shutdown().await` for each MCP client on three exit paths: successful completion,
  `AnthropicGateway::from_env` failure, and `Scheduler::new` failure. The previous
  code used `?` early-return on the latter two, causing SIGKILL-only teardown.
- **`StopReason::MaxTokens` → `AgentEffect::Failed`**: when the model is cut off
  mid-generation the agent now emits a `BudgetExceeded` flight event and returns
  `AgentEffect::Failed("model generation hit max_tokens limit …")` instead of silently
  returning `Ok("")`. Callers can now distinguish a truncated response from a real empty answer.
- **`nix` dependency** (`v0.29`, `signal` feature) promoted from dev-dependency to
  dependency so `kill(SIGTERM, …)` is available in production `shutdown()`.
- **`tokio` `fs` feature** added to `Cargo.toml` for `tokio::fs` in native tools.

### Changed
- **Native tools use `tokio::fs`**: `ReadFile`, `WriteFile`, and `ListDir` now use
  `tokio::fs::read_to_string`, `tokio::fs::write`, `tokio::fs::create_dir_all`, and
  `tokio::fs::read_dir` with the async entry iterator. Previously they used blocking
  `std::fs` calls on the tokio thread pool, which would have stalled concurrent agents.

### Tests
- 2 new unit tests in `agent/mod.rs`: `max_tokens_with_no_text_returns_failed`,
  `max_tokens_with_partial_text_returns_failed`.
- 2 new integration tests in `tests/mcp.rs`: `mcp_pagination_loads_all_pages` (asserts
  all three tools from a two-page echo-mcp paginated server appear in `tools_registered`);
  `mcp_graceful_shutdown_sends_notification` (asserts echo-mcp writes a file on
  `notifications/shutdown` before exiting).
- `echo-mcp` fixture updated: `--paginate` flag returns two-page tool list with
  `nextCursor`; `--shutdown-file <path>` flag writes `"shutdown"` to path on notification.

## [p2.3] - 2026-06-09

### Added
- **SIGTERM/SIGINT handling in `Scheduler::run()`**: replaced the `while let
  Some(er) = pending.next().await` loop with `loop { tokio::select! { ... } }`.
  Signal arms set `shutdown_requested = true` and break, causing in-flight futures
  to be dropped and the existing deferred-queue drain to run.
- **`EventKind::SystemShutdownRequested`** flight event: emitted with
  `{ "signal": "SIGTERM" }` or `{ "signal": "SIGINT" }` when a signal fires.
- **`tokio` `signal` feature** added to `Cargo.toml`; **`nix` dev-dependency**
  (v0.29, `signal` feature) added for test-side signal delivery.
- **`sigterm_drains_scheduler` test**: sends SIGTERM 50 ms into a 30-second gateway
  delay; asserts `run()` returns in < 5 s and the flight log contains the shutdown
  event.

### Not in scope
- Graceful MCP shutdown (SIGTERM + drain before SIGKILL) → p2.5
- Essential mounts: already done in `distro/overlay/init` (p2.2)
- Zombie reaping: already handled by tokio (owns SIGCHLD; competing handler disallowed)

## [p2.2] - 2026-06-09

### Added
- **`distro/` Buildroot external tree**: x86_64 musl + BusyBox; `make build` produces
  `output/bzImage` + `output/rootfs.cpio.gz` (cpio initramfs).
- **`/init` PID-1 script** (`distro/overlay/init`): mounts proc/sys/devtmpfs, mounts two
  virtio-9p host directories (`secrets0` → `/run/secrets/`, `output0` → `/run/output/`),
  sources `agentos.env`, and `exec`s agentd. Drops to busybox sh on mount/secret failure.
- **virtio-9p kernel config** (`distro/kernel-extras.config`): `CONFIG_9P_FS`, `CONFIG_NET_9P`,
  `CONFIG_NET_9P_VIRTIO`, `CONFIG_VIRTIO_NET`, `CONFIG_IP_PNP_DHCP` applied on top of
  `x86_64_defconfig`.
- **`make prereqs / build / run / test / clean / distclean`**: `test` boots with `-no-reboot`
  and confirms an `agent_completed` or `budget_exceeded` event in `output/test-run/flight.jsonl`.
- **Demo agent config** (`distro/overlay/etc/agentd/agent.toml`): Haiku model, native tools
  only, writes a greeting to `/run/output/greeting.txt`. Validates the full boot-to-inference path.
- **No system CA certs needed**: agentd's bundled `webpki-roots` (via `reqwest rustls-tls`)
  provides Mozilla CAs; the rootfs carries no `ca-certificates` package.

## [0.7.0] - 2026-06-09

### Changed
- **`reqwest` TLS backend**: switched from `native-tls` to `rustls-tls` (`default-features = false, features = ["json", "rustls-tls"]`). No longer requires OpenSSL headers at build time or system OpenSSL at runtime.

### Build
- **Static musl binary**: `cross build --target x86_64-unknown-linux-musl --release` produces a `static-pie linked, stripped` ELF binary (~3.1 MB) with no dynamic dependencies. Use `cross` (Docker-based) from macOS; on Linux with musl toolchain available, `cargo build --target x86_64-unknown-linux-musl --release` works directly.

## [0.6.0] - 2026-06-09

### Added
- **`AgentCard { id, name, description, skills }`**: derived from `AgentConfig` at scheduler seed time. Emits `agent_card_registered` flight event per agent.
- **`AgentConfig` identity fields**: optional `name`, `description`, `skills` TOML fields (all with `#[serde(default)]`). `name` defaults to `id` when absent.
- **`bus.rs` module**: `MailMessage { from, content }` and `Mailboxes = HashMap<String, Vec<MailMessage>>`. Canonical home for A2A bus primitives.
- **`list_agents` tool**: returns a sorted JSON array of all registered `AgentCard`s. No capability required — available to every agent.
- **`send_message` tool + `AgentEffect::SendMessage { call_id, to, content }`**: sole-call tool intercepted by the scheduler. Delivers message to recipient's mailbox; synthesizes an immediate `ToolResult` so the sender continues. Unknown recipient returns an `is_error` tool result (no panic, no crash).
- **Mailbox drain before each inference**: `drain_mailbox` is called after `provide_inference`/`provide_tool_results` and before `step()`. `AgentTask::inject_messages` appends mail as a `Block::Text` to the last `User` message, preserving the Anthropic API's strict alternating-role requirement.
- **Shutdown drain fix**: `shutdown_requested: bool` in `SchedulerState`. `drain_deferred` now checks this flag and emits `agent_admission_denied { reason: "shutdown" }` instead of re-queuing agents that can never run.
- **New flight events**: `AgentCardRegistered`, `MessageSent`, `MessageReceived`.
- **9 new unit tests** covering: `inject_messages` appends to last User msg; empty inject is noop; sole-call guard for `send_message`; missing `to` field error; `send_message` delivery + `message_sent` event; unknown-recipient error; `AgentCard` name defaulting; explicit name/skills round-trip; TOML parsing of new identity fields.

### For contributors
- `dispatch_send_message` in `scheduler.rs` handles the full message lifecycle: recipient validation → mailbox push → `MessageSent` flight event → synthesize ToolResult → re-enqueue sender.
- `register_native` gains a third `cards: Option<Arc<Vec<AgentCard>>>` parameter; pass `None` in tests.
- `agents.toml` example updated with `name`, `description`, `skills` fields on both agents.

## [0.5.0] - 2026-06-09

### Added
- **`spawn_agent` tool**: an agent with the `Spawn` capability calls `spawn_agent{task, child_id?, priority?, token_budget?}` to create a child agent. The child runs to completion; its result is injected back into the parent as a `ToolResult` so the parent can continue. The call must be the sole tool use in its turn.
- **`SchedulerState` refactor**: all mutable scheduler run-loop state consolidated into a single `SchedulerState` struct (`agents`, `outcomes`, `pending`, `deferred`, `in_flight`, `tokens_spent`, `awaiting`, `child_seq`, `spawn_depths`, `max_spawn_depth`). Eliminates the previous 13-loose-locals pattern.
- **`AgentEffect::SpawnAgent { call_id, config }`**: new variant intercepted by the scheduler before any tool `invoke()`. The agent state machine recognizes a `spawn_agent` tool-use response and returns this effect instead of `CallTools`.
- **Spawn depth limit**: `max_spawn_depth: u32` in `[scheduler]` TOML (default 4). If exceeded, the parent receives an `is_error` tool result instead of a child being created.
- **Child admission denial**: if a child's first inference is denied (budget or slot exhausted), the parent receives an `is_error` tool result and continues running.
- **`Capability::Spawn` enforcement**: `dispatch_spawn` checks the parent's cap set; absence of `Spawn` returns an `is_error` tool result to the parent rather than creating a child.
- **`agent_child_result_delivered` flight event**: emitted when a child's result is injected into its parent, carrying `{child_id, parent_id, call_id, success}`.
- **`SpawnAgentTool`** in `native.rs`: registered as a stub tool so it appears in `filtered_specs` for agents with `Spawn` capability. Its `invoke()` is a safety net that always errors (the scheduler intercepts before `invoke` is reached).
- **Child ID naming**: auto-generated as `"{parent_id}-child-{seq}"` with a monotonic counter.
- **Child inherits parent's capabilities and `model_cfg`**: spawned child uses the same model and capability set as its parent (unless overridden).

### Fixed
- `Capability::Spawn` was previously hard-coded to always return `false` in `satisfies()`; it now correctly checks whether the granted set contains `Spawn`.
- `SchedulerConfig::Default` now returns `max_spawn_depth = 4` instead of `0` (the derived `Default` was overriding the serde default, silently disabling all spawning for Rust-constructed configs).

### For contributors
- `SpawnConfig` struct in `config.rs`: `{ child_id: Option<String>, task: String, priority: u32, token_budget: Option<u64> }`.
- `dispatch_spawn` in `scheduler.rs` handles the full spawn lifecycle: cap check → depth check → child ID → child `AgentTask` creation → awaiting registration → seeding.
- `handle_agent_terminal` routes child completions to the parent via `provide_tool_results` + `step` + `enqueue_or_defer`; non-child completions go straight to `outcomes`.
- `send_message` deferred to p1.6 (Agent Cards increment).

## [0.4.0] - 2026-06-08

### Added
- **Capability system** (`capabilities` TOML field on `[[agents]]`/`[agent]`):
  least-privilege tool grants — `FsRead{prefix}`, `FsWrite{prefix}`, `Net{hosts}`,
  `Mcp{server, tools}`, `Spawn`. Absent field = unrestricted (backward compat);
  `capabilities = []` = deny all.
- **Capability enforcement at `ToolRegistry::invoke`**: the single unbypassable
  boundary; denials emit a `capability_denied` flight event with data `{tool, required}`
  (the agent id is in the event's top-level `agent` field) and return an `is_error`
  tool result to the agent.
- **`filtered_specs`**: agents only receive the tool specs they are authorized to
  call in their inference context — no wasted inference turns on inaccessible tools.
- **`normalize_path`**: resolves `..` components without filesystem access before
  prefix matching, blocking directory traversal (e.g. `/workspace/../etc/passwd`
  is correctly denied against a `/workspace` prefix grant).
- **`satisfies_type`**: type-level capability check used by `filtered_specs` —
  "does this agent have any FsRead capability?" vs. "can they access this specific path?"
- **`McpTool` server provenance**: `server_name` field on `McpTool` enables
  `Mcp{server, tools}` capability gating on per-server MCP tool access.

### For contributors
- New `agentd/src/capability.rs`: `Capability` enum, `normalize_path`, `satisfies`,
  `satisfies_type`. All capability logic lives here; no policy is embedded in tools.
- `Tool` trait gains `fn required_capability_for(&self, input: &Value) -> Option<Capability>`
  (default `None`). Path-based tools return the actual access path at invocation time.
- `ToolRegistry::invoke` gains `(agent_id, cap_set, recorder)` params.
- `run_tools_sequential` gains `cap_set: Option<&[Capability]>` param; threaded through
  to `invoke`. Driver passes `None` (backward compat).
- `Scheduler::new` calls `filtered_specs(cap_set)` per agent instead of shared `specs()`.

## [0.3.0] - 2026-06-08

### Added
- **Metered scheduling & admission control** (`[scheduler]` TOML section): cap total
  token spend across all agents with `global_token_budget` and limit how many model
  calls can run concurrently with `max_concurrent_inferences`. Both default to `0`
  (unlimited), preserving all prior behavior.
- **Priority-based deferred queue**: each agent carries a `priority: u32` field
  (default `0`). When the concurrency cap is full, the agent's inference is queued and
  admitted in descending-priority order (FIFO within a band) when a slot opens.
- **Admission-control flight events**: `agent_scheduled`, `agent_deferred`, and
  `agent_admission_denied` appear in `flight.jsonl`, giving full observability into
  scheduler decisions.

### Fixed
- `in_flight` underflow guards promoted from `debug_assert!` (compiled out in release)
  to `assert!`, ensuring the invariant is enforced in production builds.

### For contributors
- `SchedulerConfig` struct in `config.rs` carries `global_token_budget` and
  `max_concurrent_inferences`; wired into `Scheduler::new` via `main.rs`.
- `DeferredInfer` type with a custom `Ord` drives the `BinaryHeap` deferred queue.
- `drain_deferred` / `enqueue_or_defer` manage the admission lifecycle; both are
  tagged with `TODO(p1.x)` noting a planned `SchedulerState` refactor.

## [0.2.0] - 2026-06-08

### Added
- **Multi-agent scheduler**: Run multiple agents concurrently on independent tasks with a
  single `agentd agents.toml` invocation. Agents share a gateway and tool registry; each
  runs its own perceive → infer → act → observe loop without blocking the others.
- **`[[agents]]` config form**: Declare multiple agents in one TOML file using the
  `[[agents]]` array. The original `[agent]` single-agent form is fully backward-compatible.
- **`agents.toml` example**: Ships a two-agent example config alongside the existing
  `agent.toml`.
- **`AgentFailed` flight event**: Emitted when an agent terminates due to an inference
  error, completing the `AgentSpawned` ↔ terminal-event symmetry in the flight log.
- Non-zero exit code when any agent fails; individual per-agent errors logged with agent ID.

### For contributors
- Agent loop refactored into a sans-IO state machine (`AgentTask` + `AgentEffect`).
  `step()` → `AgentEffect` drives the loop; the scheduler performs all async IO and
  feeds results back via `provide_inference()` / `provide_tool_results()`. Enables
  concurrent IO across agents without threads.
- `driver::run` is now a single-agent backward-compat shim; the scheduler is the
  primary execution engine for all runs.
- `AgentSpawned` events are emitted before gateway initialization so startup events
  always appear in the flight log even when API key setup fails.
- `run_tools_sequential` extracted as `pub(crate)` in `agent/mod.rs`, shared by the
  driver and the scheduler.

### Fixed
- MCP child processes are now properly cleaned up on agent failure: `run_agent` returns
  `Err` instead of calling `std::process::exit(1)` while `mcp_clients` is still in scope,
  ensuring `kill_on_drop` fires before the process exits.
- Guard added for `stop_reason=tool_use` responses that contain no `ToolUse` blocks —
  previously would have sent an empty User message to the API.

## [0.1.0] - 2026-06-07

Initial release: config loader, flight recorder, `InferenceGateway` trait, Anthropic
backend, tool ABI, native file tools, MCP stdio client, and a single-agent
perceive → infer → act → observe loop.
