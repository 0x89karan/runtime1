# TODOS

## Phase 0 — Technical Debt

**~~P2 — Sync I/O in native tool impls (p0.5)~~** ✓ Done in p2.5.
- `ReadFile`, `WriteFile`, `ListDir` migrated to `tokio::fs` (non-blocking).

**~~P2 — ToolRegistry::register should error on collision (p0.5)~~** ✓ Done in p0.5.

**~~P3 — Per-agent capability scoping for native file tools (p1.4)~~** ✓ Done in p1.4.

**P3 — FsRead/FsWrite enforcement assumes absolute paths (p1.4)**
- `normalize_path` handles `..` components but does not resolve symlinks or expand `~`.
  Relative paths fail-safe to deny (no prefix match), but are not explicitly rejected.
- Action: validate absolute paths at invocation, or document the assumption; symlink
  traversal prevention requires Phase 4 sandbox.

**P3 — Symlink traversal not blocked by capability prefix check (p1.4)**
- A symlink inside a granted prefix can point outside it. `normalize_path` uses
  `Path::components()` — no filesystem access, so symlinks are not resolved.
- Action: Phase 4 sandbox (seccomp/namespaces) is the correct enforcement layer.

**~~P3 — 2 MB binary target needs re-evaluation at p0.2~~** ✓ Done in p2.1.
- Switched to `rustls-tls`; static musl binary is 3.1 MB (vs ~1.4 MB macOS debug).
  Acceptable for Phase 2; a dedicated size-audit increment is p2.4.

**~~P3 — flight.jsonl CWD footgun for multi-agent (p1.2)~~** ✓ Resolved in p1.2.
- Resolution: single shared `flight.jsonl` + per-event `agent` field (CONVENTIONS.md invariant).
  All events emitted by `Scheduler::run()` carry the agent_id. Consumers filter by `agent` key.
- **P3 — stdout ordering for multi-agent answers**: answers are printed in completion order (fastest
  agent first), not in config declaration order. Fine for p1.2; a flag or ordered output mode
  may be desirable in a future increment.

**P3 — Net capability is advisory (p1.4 intentional)**
- `satisfies()` returns `true` unconditionally for `Net{..}` regardless of the granted set.
  This is intentional at p1.4 — no Net tools exist yet and Phase 4 network namespaces are
  the correct enforcement layer. Documents as advisory in the enum docstring.
- Action: wire real enforcement when the first Net tool lands, or in Phase 4.

**P3 — Net enforcement via Landlock ABI v4 not yet wired (p3.3 deferred)**
- `Net { hosts }` capability is advisory at the kernel layer. Landlock ABI V4 (Linux 6.7)
  adds `LANDLOCK_ACCESS_NET_BIND_TCP` / `LANDLOCK_ACCESS_NET_CONNECT_TCP`, which would
  enforce per-host connection rules. Our kernel (6.6 LTS) supports V4 but the sandbox
  crate only uses V1 FS rules.
- Action: extend `SandboxRule` with `AllowNetConnect { host, port }`, bump to V4, and
  enforce in `sandbox/src/lib.rs` during Phase 4.

**P3 — MCP server without `capabilities` runs unsandboxed with warn-only (p3.3)**
- `McpServerConfig.capabilities = None` (the default, for backward compat) bypasses
  all kernel enforcement. Only a `tracing::warn!` and `SandboxSkipped` flight event are
  emitted. A malicious or buggy MCP server with no capabilities field has full OS access.
- Action: Phase 4 — add a `[tools] mcp_require_capabilities = true` global flag that
  makes sandboxing mandatory and rejects startup when any server omits `capabilities`.

**P3 — `required_capability_for → None` tools are always visible (p1.4 design)**
- Tools that return `None` from `required_capability_for` appear in `filtered_specs` even
  when `cap_set = Some([])`. This is the documented contract ("tools that require no cap
  gating"). All current tools (read_file, write_file, list_dir, MCP) do declare a cap.
- Action: revisit if a tool with `None` should be hidden under deny-all; update the docstring
  and `filtered_specs` comment to make the policy explicit.

**P3 — Case-sensitive path prefix matching on case-insensitive filesystems (p1.4)**
- `normalize_path` + `starts_with` are case-sensitive. On macOS (HFS+ case-insensitive)
  a grant of `/Workspace` does not match `/workspace/file`. Production target is Linux
  (case-sensitive ext4/btrfs) so this is a dev-environment edge case, not a security gap.
- Action: document the Linux-only assumption or add a config flag for CI on macOS.

**~~P3 — SchedulerState refactor (p1.5)~~** ✓ Done in p1.5.

**~~P3 — Shutdown drain re-enqueues to exited poll loop (p1.5 red-team)~~** ✓ Done in p1.6.
- Added `shutdown_requested: bool` to `SchedulerState`; `drain_deferred` now
  checks the flag and emits `agent_admission_denied { reason: "shutdown" }`
  instead of re-enqueueing onto the already-exited poll loop.

**P3 — EventKind enum in flight_recorder.rs → events.rs at p0.4**
- Once all 11 Phase-0 kinds are actively emitted, extract to its own module.
- Keeps `flight_recorder.rs` focused on I/O, not taxonomy.
- Action: extract during p0.4 implementation.

**~~P2 — MCP tools/list pagination not followed (p0.5 adversarial review)~~** ✓ Done in p2.5.
- `McpClient::spawn` now follows `nextCursor` in a loop until all pages are loaded.

**~~P2 — MCP graceful shutdown (p0.5 adversarial review)~~** ✓ Done in p2.5.
- `McpClient::shutdown()` sends `notifications/shutdown`, waits 5s for clean exit, escalates to SIGTERM, then SIGKILL.

**~~P2 — StopReason::MaxTokens produces empty Ok("") (pre-existing, p0.4)~~** ✓ Done in p2.5.
- `StopReason::MaxTokens` now emits `BudgetExceeded` flight event and returns `AgentEffect::Failed`.

**P3 — Buildroot ccache volume not wired (p2.2)**
- `make build` recompiles the toolchain from source on every `distclean`. Buildroot
  supports ccache via `BR2_CCACHE=y`; wiring a persistent Docker volume or host cache
  dir would cut rebuild time from ~30 min to ~2 min on subsequent clean builds.
- Action: add `BR2_CCACHE=y` + cache-dir Makefile variable in a future p2.x hardening pass.

**P3 — agentd flight log path is hard-coded to CWD (p2.2)**
- `flight.jsonl` is always written to the process CWD. In the VM this is `/run/output`
  (the host-visible 9p mount), which works. On the host it depends on the user's shell CWD.
- Action: add a `--log-path <file>` CLI flag (or `[agent] log_path` TOML field) in p2.3
  or a dedicated increment so the log destination is explicit in all environments.

**P2 — Linux-gated code not verifiable on macOS dev machines (p3.1 lesson)**
- `#[cfg(target_os = "linux")]` blocks (fuser, libc usage in surfaces/) are never
  compiled locally on macOS, so `cargo clippy -- -D warnings` is a false green.
  Three CI failures on p3.1 (getattr signature, missing libc dep, two clippy lints)
  all came from this blind spot.
- Action: run `make clippy-linux` (Docker) from repo root before pushing any branch
  that touches Linux-gated code. Target added to workspace Makefile; rule added to
  CLAUDE.md quality gate.

**P3 — No `--no-fuse` flag for CI and host dev environments (p3.1)**
- `surfaces::agents_fs::mount()` is called unconditionally in main.rs. On host Linux dev machines
  without FUSE available (or in CI), the `Err` path logs a warn and continues — correct but
  noisy. A `--no-fuse` CLI flag (or `AGENTOS_NO_FUSE` env var) would explicitly skip mount()
  without a warning, making CI output clean and intentional.
- Blocked by: nothing. Pure main.rs change.
- Action: add `--no-fuse` flag alongside the existing CLI args in a future p3.x increment.

## Completed

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
