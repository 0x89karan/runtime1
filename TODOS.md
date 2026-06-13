# TODOS

## Phase 4 — Open (deferred from p4.7 audit)

Findings from `docs/AUDIT-phase-4-6.md` that are real bugs but do not block Phase 5:

**F-006 (P2) — `inject_messages` appends `Block::Text` onto a ToolResult-only user turn**
- `agent/mod.rs:240-241`: after a tool cycle, the last user message is all `Block::ToolResult`;
  injection pushes a `Block::Text` into it yielding mixed content. Anthropic tolerates this today
  but a stricter provider (or future validation) would reject it.
- Fix: when the target user message contains any `ToolResult`, push a *new* user message instead.

**F-007 (P2) — `StopReason::MaxTokens` mislabelled as `BudgetExceeded`**
- `agent/mod.rs:333-344`: a per-response generation cap (`model.max_tokens`) is conflated with the
  cumulative per-agent `token_budget`. The agent emits `budget_exceeded` even when nowhere near budget.
- Fix: distinct event kind (e.g. `max_tokens_truncated`); don't attach the unrelated `token_budget`.

**F-008 (P2) — `StopReason::Other(_)` and empty `EndTurn` complete silently with `""`**
- `agent/mod.rs:346-371`: unknown/future stop reason or end_turn with only a filtered thinking block
  yields `Completed("")` — a silent empty answer reported as success.
- Fix: treat empty extracted text as `Failed`; handle `Other(_)` distinctly with a warning event.

**F-009 (P2) — Global token budget is a soft, post-hoc ceiling**
- `scheduler.rs:608-609`: a single inference can overshoot the ceiling by up to one inference.
- Fix: document "soft ceiling, overshoot ≤ one in-flight inference per agent" in ROADMAP/NOTES.

**F-012 (P2) — No `fsync` before/after checkpoint rename**
- `checkpoint.rs:47,117`: `write_all` not followed by `sync_all()`; parent dir not fsynced after rename.
  On real power loss the rename or tmp data blocks may not be durable.
- Fix: `f.sync_all().await?` after `write_all`; `File::open(parent).sync_all()` after rename.

**F-015 (P2) — `extra_env` can re-inject secrets via operator config**
- `tools/mcp.rs:100-102`: the `extra_env` loop from `McpServerConfig.env` runs after `env_clear()` with
  no denylist. An operator (or compromised `agents.toml`) can write `ANTHROPIC_API_KEY = "sk-…"` to pass
  the key explicitly to an MCP subprocess — defeating the F-001 env isolation.
- Fix: apply a short hardcoded denylist (e.g. `ANTHROPIC_API_KEY`, any `*_API_KEY`/`*_SECRET` pattern)
  before inserting `extra_env` keys. Log a warning and drop the offending key.

**F-016 (P2) — `drain_mailbox` can inject messages into a just-terminated agent**
- `scheduler.rs:325`: `drain_mailbox` runs after `sm.step()` regardless of terminal state. When `step()`
  returns `AgentEffect::Completed`/`Failed`, the agent is terminal but the mailbox is still drained and
  messages injected, only to be immediately discarded (or serialized into the terminal checkpoint).
- Fix: check `state.agents[&agent_id].is_terminal()` after `step()` and skip drain when terminal.

**F-017 (P3) — `Net{}` with empty ports grants unrestricted network (worse than no `Net`)**
- `main.rs:568-570`: `Net { ports: [] }` sets `has_net = true` (suppressing `IsolateNetwork`) then skips
  the rule loop (`ports.is_empty()` → `continue`). The result: full unrestricted network with no isolation.
  Declaring no `Net` capability gives `IsolateNetwork`; declaring `Net` with empty ports gives nothing.
- Fix (or document): either treat empty-ports `Net` as "unrestricted but acknowledged" (add a warn event),
  or change the semantics to treat empty ports as equivalent to `IsolateNetwork`. Decide in Phase 5.

## Phase 0 — Technical Debt

**~~P2 — Sync I/O in native tool impls (p0.5)~~** ✓ Done in p2.5.
- `ReadFile`, `WriteFile`, `ListDir` migrated to `tokio::fs` (non-blocking).

**~~P2 — ToolRegistry::register should error on collision (p0.5)~~** ✓ Done in p0.5.

**~~P3 — Per-agent capability scoping for native file tools (p1.4)~~** ✓ Done in p1.4.

**~~P3 — FsRead/FsWrite enforcement assumes absolute paths (p1.4)~~** ✓ Documented in p4.5.
- Assumption documented in `Capability` enum doc comment: relative paths fail-safe
  to deny; `~` not expanded. Production target is Linux with absolute paths.

**~~P3 — Symlink traversal not blocked by capability prefix check (p1.4)~~** ✓ Documented in p4.5.
- Documented in `Capability` enum doc comment. Phase 4 namespace sandbox (p4.2)
  is the correct enforcement layer; IsolateMount/IsolateNetwork mitigate escalation paths.

**~~P3 — 2 MB binary target needs re-evaluation at p0.2~~** ✓ Done in p2.1.
- Switched to `rustls-tls`; static musl binary is 3.1 MB (vs ~1.4 MB macOS debug).
  Acceptable for Phase 2; a dedicated size-audit increment is p2.4.

**~~P3 — flight.jsonl CWD footgun for multi-agent (p1.2)~~** ✓ Resolved in p1.2.
- Resolution: single shared `flight.jsonl` + per-event `agent` field (CONVENTIONS.md invariant).
  All events emitted by `Scheduler::run()` carry the agent_id. Consumers filter by `agent` key.
- **P3 — stdout ordering for multi-agent answers**: answers are printed in completion order (fastest
  agent first), not in config declaration order. Fine for p1.2; a flag or ordered output mode
  may be desirable in a future increment.

**~~P3 — Net capability is advisory (p1.4 intentional)~~** ✓ Enforced in p4.2.
- `caps_to_rules()` now adds `IsolateNetwork` when `Net` is absent. Network isolation is
  enforced at the kernel level via `unshare(CLONE_NEWNET)` for all sandboxed MCP servers
  that don't declare a `Net` capability. `satisfies()` for `Net` remains advisory (no net
  tools exist yet), but the sandbox enforces it independently.

**~~P3 — Net enforcement via Landlock ABI v4 not yet wired (p3.3 deferred)~~** ✓ Done in p4.6.
- `AllowNetConnect { port: u16 }` added to `SandboxRule`; `Net { hosts, ports: Vec<u16> }`
  in `Capability` (`#[serde(default)]` for backward compat). `caps_to_rules()` generates
  `AllowNetConnect` rules from `Net.ports`. Runtime ABI detection: V4 (kernel ≥ 6.7) activates
  TCP port enforcement; older kernels degrade silently (BestEffort). Port-only (not host) is
  enforced at the kernel level — hostname restriction is advisory and remains in `hosts`.
  `EnforcementStatus.landlock_net` and `SandboxApplied enforced.landlock_net` field added.

**~~P3 — MCP server without `capabilities` runs unsandboxed with warn-only (p3.3)~~** ✓ Done in p4.1.
- `[tools] mcp_require_capabilities = true` flag added in p4.1. When set, startup fails
  if any MCP server has no effective sandbox rules. Default remains `false` for backward compat.

**P3 — DenySpawn does not block `clone()`/`clone3()` on x86_64 (p3.3 adversarial review)**
- seccomp filter blocks `fork(57)` and `vfork(58)` but not `clone(56)` or `clone3(435)`.
  A sandboxed MCP server can spawn child processes via `clone(SIGCHLD)`. Classic BPF cannot
  inspect `clone` flags to distinguish thread-create vs. process-create without SECCOMP_DATA_ARGS.
- **Accepted limitation.** `gVisor` (`isolation = "gvisor"`) fully mitigates this — the Sentry
  intercepts `clone3`. For namespace-only mode, this gap is documented in THREAT_MODEL.md.
  Switching to `SECCOMP_FILTER_FLAG_NEW_LISTENER` is deferred to Phase 5 if needed.

**~~P3 — DenySpawn is a no-op on aarch64 (p3.3 adversarial review)~~** ✓ Fixed in p4.5.
- `main.rs` now detects when all enforcement fields are false/none with non-empty rules
  and emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }` instead of a
  misleading `SandboxApplied` with all-false fields.

**~~P3 — SandboxApplied fires even when Landlock degrades to no-op (p3.3 adversarial review)~~** ✓ Done in p4.1.
- `SandboxApplied` payload includes `enforced.landlock` and `enforced.seccomp` booleans
  since p4.1. Operators can inspect the event to see exactly what was active.

**~~P3 — `required_capability_for → None` tools are always visible (p1.4 design)~~** ✓ Documented in p4.5.
- Policy documented in `Tool::required_capability_for` doc comment: `None` tools are
  control-plane primitives (list_agents, send_message) intentionally visible under any
  cap-set, including deny-all. Future tools that should be suppressed must return `Some`.

**~~P3 — Case-sensitive path prefix matching on case-insensitive filesystems (p1.4)~~** ✓ Documented in p4.5.
- Assumption documented in `Capability` enum doc comment. Linux production target is
  case-sensitive; macOS is a dev-environment edge case, not a security gap.

**~~P3 — SchedulerState refactor (p1.5)~~** ✓ Done in p1.5.

**~~P3 — Shutdown drain re-enqueues to exited poll loop (p1.5 red-team)~~** ✓ Done in p1.6.
- Added `shutdown_requested: bool` to `SchedulerState`; `drain_deferred` now
  checks the flag and emits `agent_admission_denied { reason: "shutdown" }`
  instead of re-enqueueing onto the already-exited poll loop.

**~~P3 — EventKind enum in flight_recorder.rs → events.rs at p0.4~~** ✓ Done in p4.5.
- `EventKind` extracted to `agentd/src/events.rs`; re-exported from `flight_recorder`
  so all existing import paths (`agentd::flight_recorder::EventKind`) remain valid.

**~~P2 — MCP tools/list pagination not followed (p0.5 adversarial review)~~** ✓ Done in p2.5.
- `McpClient::spawn` now follows `nextCursor` in a loop until all pages are loaded.

**~~P2 — MCP graceful shutdown (p0.5 adversarial review)~~** ✓ Done in p2.5.
- `McpClient::shutdown()` sends `notifications/shutdown`, waits 5s for clean exit, escalates to SIGTERM, then SIGKILL.

**~~P2 — StopReason::MaxTokens produces empty Ok("") (pre-existing, p0.4)~~** ✓ Done in p2.5.
- `StopReason::MaxTokens` now emits `BudgetExceeded` flight event and returns `AgentEffect::Failed`.

**~~P3 — Buildroot ccache volume not wired (p2.2)~~** ✓ Done in p4.5.
- `BR2_CCACHE=y` and `BR2_CCACHE_DIR=$(HOME)/.buildroot-ccache` added to
  `distro/buildroot.config`. Subsequent clean builds use the host ccache (~2 min vs ~30 min).

**~~P3 — agentd flight log path is hard-coded to CWD (p2.2)~~** ✓ Done in p4.5.
- `--log-path <file>` CLI flag and `log_path` top-level TOML field added. Precedence:
  CLI > TOML > default `"flight.jsonl"`. In the VM `log_path` can be set to
  `/run/output/flight.jsonl` to make the destination explicit.

**~~P4 — `run_probe` ignores `--log-path` (p4.5 review)~~** ✓ Done in p4.6.
- `run_probe` now accepts `log_path: PathBuf`; call site passes
  `resolve_log_path(log_path_override, None)`; uses `FlightRecorder::new(&log_path)`.
  `--probe --log-path /path/to/flight.jsonl` now works correctly.

**~~P2 — Linux-gated code not verifiable on macOS dev machines (p3.1 lesson)~~** ✓ Mitigated in p3.1.
- `make clippy-linux` target added to workspace Makefile; `CLAUDE.md` quality gate updated.
  Required before pushing any branch touching `#[cfg(target_os = "linux")]` code.

**~~P3 — checkpoint.json has no access-control or encryption (p4.3)~~** ✓ Mode restriction done in p4.4.
- `checkpoint.json` is now created with mode 0600 via `write_mode_600()` in `checkpoint.rs`.
  `rename(2)` preserves those permissions on the final file. Test `save_sets_mode_0600` added.
- Encryption at rest remains a future item (THREAT_MODEL.md §3.3).

**P4 — `runsc do` is experimental; full OCI bundle integration deferred (p4.2)**
- `isolation = "gvisor"` wraps the MCP server command with `runsc do -- <cmd>`. The `do`
  subcommand is undocumented/experimental in gVisor and may not be stable across versions.
- Action: build a minimal OCI bundle (config.json + rootfs) on the fly via `runsc run` for
  production-grade gVisor integration. Deferred because `runsc do` suffices for p4.2 exploration.

**P4 — PID namespace via `unshare()` only affects future children (p4.2)**
- `unshare(CLONE_NEWPID)` in `pre_exec` makes the *calling* process's future children be in
  a new PID namespace, but the MCP server itself (after exec) remains in the parent PID namespace.
  To put the MCP server in a new PID namespace, a second fork is needed before exec.
- Action: implement double-fork in `McpClient::spawn` using a pipe to propagate the inner PID
  back to the parent. Deferred to a future Phase 4 increment.

**P4 — `clone3()` bypass remains in namespace-only sandbox path (p4.2)**
- `DenySpawn` seccomp blocks `fork(57)` + `vfork(58)` but not `clone(56)` or `clone3(435)`.
  Combined with `IsolateNetwork`, a sandboxed MCP server can still spawn children in the
  parent PID namespace via `clone3()`. `isolation = "gvisor"` fully fixes this (Sentry intercepts
  `clone3`); the namespace-only path does not.
- Action: accept the limitation for namespace-only mode; document in operator guidance.
  gVisor is the recommended mode for truly adversarial workloads.

**~~P3 — pre_exec sandbox errors are masked as EPERM (p4.1 red-team)~~** ✓ Done in p4.4.
- `mcp.rs::McpClient::spawn` now uses a pre-exec error pipe (`pipe2 + O_CLOEXEC`) on Linux.
  On pre_exec failure, the child writes "sandbox" to the pipe; the parent reads it and includes
  "(sandbox stage: 'sandbox')" in the error message. Previously all failures surfaced as EPERM.

**P4 — aarch64 CI runner needed to validate DenySpawn no-op behavior (p4.1 eng review)**
- The fix in T1 (gate BPF with `#[cfg(target_arch = "x86_64")]`) emits `SandboxSkipped` on
  non-x86_64 when DenySpawn is requested. There is no aarch64 GitHub Actions runner in CI
  to verify the behavior. Current `ubuntu-latest` runners are x86_64.
- Action: add a self-hosted aarch64 runner or QEMU-emulated job to CI when one becomes
  available; until then the logic is unit-tested but not E2E verified on real hardware.

**~~P4 — `sandbox_probe` fixture not wired to any integration test (p4.1 eng review)~~** ✓ Done in p4.4.
- 3 integration tests added in `tests/integration.rs` (Linux-gated): `allowed_path_read_succeeds`,
  `denied_path_read_fails`, `deny_spawn_blocks_exec` (x86_64-only). Tests spawn sandbox_probe
  directly with `pre_exec` + compiled sandbox rules; verify exit codes 0/1/non-0.

**~~P3 — No `--no-fuse` flag for CI and host dev environments (p3.1)~~** ✓ Done in p4.4.
- `--no-fuse` CLI flag and `AGENTOS_NO_FUSE` env var added to `main.rs`. When either is set,
  the FUSE mount is skipped with `tracing::info!` instead of attempted (no warning on CI).

## Completed

**p3.3 — Landlock LSM + seccomp-bpf sandbox for MCP server subprocesses**
- `sandbox/` crate: `SandboxRule` enum (`AllowFsRead`, `AllowFsWrite`, `DenySpawn`);
  `CompiledSandbox` / `compile()` / `apply_compiled()` / `apply_sandbox()` API.
- Landlock V1 FS rules via raw syscalls (444/445/446); `ACCESS_FS_HANDLED = 0x1FFE`
  (excludes Execute bit to allow initial exec of MCP binary).
- seccomp-bpf filter blocks `fork(57)` + `vfork(58)` on x86_64 only; classic BPF.
- `caps_to_rules()` in `main.rs`: converts agent `Capability` set to `SandboxRule` list.
- `SandboxApplied` / `SandboxSkipped` flight events; `O_NOFOLLOW` on Landlock path fds.
- `CONFIG_SECCOMP=y / CONFIG_SECCOMP_FILTER=y` in `distro/kernel-extras.config`.
- 180 tests pass; Linux-gated tests deferred to CI via `#[cfg(target_os = "linux")]`.
- **Completed:** v0.10.0 (2026-06-11)

**p3.1 — /agents FUSE virtual filesystem**
- `surfaces/` crate: `SchedulerSnapshot` / `AgentSnapshot` / `AgentStatus` snapshot types.
- `AgentsFs` (`fuser` 0.14, Linux-only): root dir + per-agent dirs with `status`, `context_size`,
  `budget`, `flight` virtual files. Inode scheme: root=1, dirs from 1010 step 10, files at dir+1..4.
- `mount()` spawns FUSE thread; returns `FuseMounted` guard (RAII unmount); stubs on non-Linux.
- `Scheduler::new` gains 7th `Arc<RwLock<SchedulerSnapshot>>` arg; `update_snapshot` called after
  every scheduler effect; `AgentTask::context_tokens()` + `task_preview()` supply snapshot fields.
- New flight events: `FuseMounted`, `FuseUnmounted`.
- Workspace promoted: root `Cargo.toml` with `members = ["agentd", "surfaces"]`.
- `distro/kernel-extras.config` adds `CONFIG_FUSE_FS=y`; `distro/overlay/agents/` mount point.
- 188 tests pass (all platforms); negative FUSE read offset guard added post review-army.
- **Completed:** v0.9.0 (2026-06-10)

**p2.5 — Deferred cleanup (sync I/O, MCP pagination, MaxTokens, graceful shutdown)**
- Native tools (`ReadFile`, `WriteFile`, `ListDir`) migrated to `tokio::fs`.
- `McpClient::spawn` follows `nextCursor` in a loop until all pages loaded; capped at 100 pages.
- `StopReason::MaxTokens` now emits `BudgetExceeded` flight event and returns `AgentEffect::Failed`
  instead of silent `Ok("")`.
- `McpClient::shutdown()` sends `notifications/shutdown`, waits 5s, escalates to SIGTERM then SIGKILL.
- **Completed:** v0.8.0 (2026-06-09)

**p2.3 — Boot/supervision basics (SIGTERM/SIGINT handling)**
- `loop { tokio::select! { ... } }` in `Scheduler::run()` replaces `while let`.
- SIGTERM/SIGINT arms set `shutdown_requested = true` and break; deferred drain runs as before.
- `EventKind::SystemShutdownRequested` flight event emitted on signal.
- 1 new test: `sigterm_drains_scheduler` — sends SIGTERM, asserts < 5s exit + flight event.
- Essential mounts and zombie reaping required no code (handled by `/init` and tokio respectively).
- **Completed:** p2.3 (2026-06-09)

**p0.1 — Crate scaffold + config + flight recorder**
- Created `agentd/` binary crate with Config (TOML), FlightRecorder (append-only JSONL),
  EventKind enum, CI workflow, README, LICENSE.
- All acceptance criteria met: `cargo build` + `cargo clippy -D warnings` + `cargo test` pass.
- **Completed:** v0.1.0 (2026-06-07)

**p0.2 — Inference gateway + Anthropic backend**
- Added `InferenceGateway` trait, neutral message/tool types, `AnthropicGateway`
  (Anthropic Messages API), `--probe` smoke-test mode, 120s HTTP timeout.
- All acceptance criteria met.
- **Completed:** 2026-06-07

**p0.3 — Tool ABI + native tools**
- Added `Tool` trait, `ToolRegistry` (warn on collision, sorted specs), and three
  native tools: `read_file` (100k-char cap), `write_file` (mkdir-p), `list_dir`
  (sorted, `/`-suffixed dirs). `register_native(reg, &["all"])` wires them up.
  `tools_registered` flight event emitted at startup.
- All acceptance criteria met.
- **Completed:** 2026-06-07

**p0.4 — The agent loop (perceive → infer → act → observe)**
- `agent::run()`: full perceive → infer → act → observe loop with flight events.
  Token budget guard, max-turns guard, tool errors as `is_error` blocks.
- `main.rs`: stdin fallback for task, final answer on stdout.
- All Phase 0 flight events emitted.
- **Completed:** 2026-06-07

**p0.5 — Real MCP stdio client**
- `McpClient`: newline-delimited JSON-RPC 2.0 over tokio::process::Child (kill_on_drop).
  Handshake: initialize → notifications/initialized → tools/list. `tools/call` for invocation.
- `McpTool` implements `Tool`; `isError: true` → `anyhow` error.
- `ToolRegistry::register` now errors on collision (upgraded from warn).
- `echo-mcp` fixture binary + integration tests for MCP startup, coexistence, missing-server.
- Release binary: 1.4 MB on macOS.
- **Completed:** 2026-06-07

**p1.1 — Agent as a sans-IO state machine**
- `AgentTask` + `AgentEffect` (`#[must_use]`) + `step()` + `provide_inference()` + `provide_tool_results()`.
- Terminal guard on all `provide_*` and `step()` calls; MaxTurns fires before InferenceRequest.
- `agent/mod.rs` + `agent/driver.rs` split; driver is backward-compat shim.
- Unit tests: `step_machine_text_tool_text_cycle`, `max_turns_fires_before_infer_request`, `provide_inference_on_terminal_task_is_noop`.
- **Completed:** 2026-06-08

**p1.2 — The scheduler (multi-agent, cooperative)**
- `Scheduler` in `agentd/src/scheduler.rs`: `HashMap<String, AgentTask>` + `FuturesUnordered` drive loop.
  `Scheduler::new()` validates duplicate IDs. `Scheduler::run()` owns all IO concurrently.
- `config.rs`: `[[agents]]` multi-agent form + `agent_configs()` + backward-compat `[agent]` single form.
- `run_tools_sequential` extracted as `pub(crate)` in `agent/mod.rs`, shared by driver and scheduler.
- `agents.toml`: example two-agent config.
- `main.rs`: uses Scheduler for all runs; exit non-zero if any agent fails; stdin fallback preserved for single form.
- 4 scheduler tests + 8 config tests. All 74 unit + 16 integration tests pass.
- **Completed:** 2026-06-08

**p1.3 — Metered scheduling & admission control**
- `SchedulerConfig` in `config.rs`: `global_token_budget` (u64) + `max_concurrent_inferences` (usize); wired into `Scheduler::new`.
- Per-agent `priority: u32` field (default 0); `BinaryHeap<DeferredInfer>` keyed by `(priority desc, seq asc)`.
- `enqueue_or_defer` / `drain_deferred` manage the admission lifecycle in `scheduler.rs`.
- Flight events: `agent_scheduled`, `agent_deferred`, `agent_admission_denied`.
- `in_flight` underflow guards promoted from `debug_assert!` to `assert!`.
- New config tests: `scheduler_config_explicit_values_parse`, `scheduler_config_defaults_to_unlimited`, `agent_priority_parses_from_toml`.
- **Completed:** v0.3.0 (2026-06-08)

**p1.4 — Capability system**
- `Capability` enum (`FsRead{prefix}`, `FsWrite{prefix}`, `Net{hosts}`, `Mcp{server,tools}`, `Spawn`).
- `normalize_path` + `satisfies` + `satisfies_type` in `capability.rs`.
- `Tool::required_capability_for` + enforcement at `ToolRegistry::invoke`.
- `filtered_specs(cap_set)` — per-agent model context filtering.
- `CapabilityDenied` flight event; `capability_denied` in `flight.jsonl`.
- `McpTool::server_name` for Mcp{} cap gating.
- 130 tests pass (unit + integration + MCP + MCP client).
- **Completed:** v0.4.0 (2026-06-08)

**p1.5 — Inter-agent spawn-await**
- `spawn_agent` tool: parent with `Spawn` cap creates a child agent; child runs to completion;
  result injected back into parent as a `ToolResult`. Sole-call guard enforced.
- `AgentEffect::SpawnAgent { call_id, config }` — intercepted by scheduler before `invoke()`.
- `SpawnConfig` in `config.rs`: `task` (required), `child_id`/`priority`/`token_budget` (optional).
- `SchedulerState` struct consolidates all mutable scheduler state (`agents`, `outcomes`, `pending`,
  `deferred`, `in_flight`, `tokens_spent`, `awaiting`, `child_seq`, `spawn_depths`, `max_spawn_depth`).
- `dispatch_spawn` / `handle_agent_terminal` in `scheduler.rs` manage the full spawn lifecycle.
- Spawn depth limit: `max_spawn_depth: u32` in `[scheduler]` TOML (default 4; 0 = disabled).
- `agent_child_result_delivered` flight event.
- `Capability::Spawn` `satisfies()` fix; `SchedulerConfig::Default` fix (max_spawn_depth was 0).
- `send_message` deferred to p1.6 (Agent Cards increment).
- 133 tests pass (unit + integration).
- **Completed:** v0.5.0 (2026-06-09)

**p2.1 — rustls + static musl binary**
- Switched `reqwest` from `native-tls` to `rustls-tls`; all 142 tests pass.
- Cross-compiled `x86_64-unknown-linux-musl` via `cross` (Docker); binary is `static-pie linked, stripped`, 3.1 MB.
- **Completed:** v0.7.0 (2026-06-09)

**p2.2 — Buildroot minimal rootfs**
- `distro/` external Buildroot tree: x86_64 musl + BusyBox, cpio.gz initramfs, `make build/run/test`.
- `/init` PID-1 sh: mounts proc/sys/9p shares, sources `agentos.env`, `exec`s agentd.
- Two virtio-9p mounts: `secrets0` (API key) + `output0` (flight.jsonl visible on host).
- `make test` boots with `-no-reboot`, checks flight.jsonl for `agent_completed` event.
- **Completed:** p2.2 (2026-06-09)

**p1.6 — Agent identity & Agent Cards (discovery)**
- `AgentCard { id, name, description, skills }` derived from `AgentConfig` at scheduler seed; `agent_card_registered` flight event.
- `AgentConfig` gains `name`, `description`, `skills` optional TOML fields.
- `bus.rs`: `MailMessage` + `Mailboxes`.
- `list_agents` tool: sorted JSON array of all AgentCards; no capability required.
- `send_message` tool + `AgentEffect::SendMessage`: sole-call; scheduler delivers to mailbox; synthesizes ToolResult; unknown recipient → `is_error` (no panic).
- Mailbox drain before each inference; `inject_messages` appends to last User message (no consecutive-User-message violation).
- Shutdown drain fix: `shutdown_requested` flag in `SchedulerState`.
- New flight events: `agent_card_registered`, `message_sent`, `message_received`.
- 142 tests pass (unit + integration).
- **Completed:** v0.6.0 (2026-06-09)
