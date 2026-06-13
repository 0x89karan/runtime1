# AgentOS Build Roadmap

This is the **work queue**. It decomposes the design (`docs/DESIGN.md`) into
ordered, dependent increments. Each increment is a self-contained unit of work
with a clear goal, dependency, scope, and acceptance criteria. Implement them in
order; keep `main` shippable at every step.

## How to use this with gstack

The increment list pairs naturally with the gstack workflow loop. For each
increment, run roughly:

1. **Plan** — `/plan-eng-review` against the increment's goal + scope, or
   `/autoplan` for a fuller pipeline on the more architectural increments
   (p1.1, p1.2, p1.3 are the obvious candidates).
2. **Build** — implement, optionally `/freeze`d to a directory while you work.
3. **Review** — `/review` on the diff.
4. **Verify** — `/qa` against the acceptance criteria listed below.
5. **Ship** — `/ship` (or `/land-and-deploy` when appropriate).

Working rules:

- **One increment per branch.** Suggested branch name = the increment id, e.g.
  `p1.1-agent-state-machine`.
- **Don't bundle.** If a change feels like two increments, split it.
- **Main stays green.** An increment's acceptance criteria must pass before the
  next one starts on top of it.
- **Same-change housekeeping:** check the increment's box here and update any
  affected doc (`CONVENTIONS.md`, `DESIGN.md`) alongside the code.

### Definition of done (every increment)

- [ ] `cargo build` (debug + release) succeeds
- [ ] `cargo clippy -- -D warnings` is clean
- [ ] `cargo test` passes (new behavior has tests; loop/scheduler tests use the mock gateway, not the network)
- [ ] `/review` clean on the diff
- [ ] `/qa` confirms the increment's acceptance criteria
- [ ] Flight-recorder events added or preserved per `docs/CONVENTIONS.md`
- [ ] The single-agent demo (`agent.toml`) still runs and its event sequence hasn't regressed
- [ ] This file and other affected docs updated in the same change

---

## Phase 0 — Single-agent spike

Goal: prove the **agent-loop-as-process** model end to end. By the end of Phase
0, one agent boots from a TOML spec and runs perceive → infer → act → observe
to completion against a real model and real MCP tools, with everything logged.

### ▣ p0.1 — Crate scaffold + config + flight recorder
**Depends on:** nothing.
**Goal:** A `cargo run` that loads a TOML config and writes a structured event to
an append-only flight log. The plumbing layer, nothing more.
**Scope:** create `agentd/` as a binary crate (Rust 2021).
- `agentd/Cargo.toml`: `tokio`, `serde`, `serde_json`, `toml`, `chrono`,
  `anyhow`, `tracing`, `tracing-subscriber`. Size-optimized release profile
  (`opt-level = "z"`, `lto = true`, `strip = true`, `codegen-units = 1`,
  `panic = "abort"`).
- `agentd/src/main.rs`: `#[tokio::main]`, init `tracing` to stderr (`RUST_LOG`
  env filter, default `info`), load config from argv[1] (default `agent.toml`),
  init the flight recorder, log an `agent_spawned` event, exit 0.
- `agentd/src/config.rs`: `Config` / `AgentConfig` / `ModelConfig` / `ToolsConfig`
  with serde defaults. Secrets are not in config — only env.
- `agentd/src/flight_recorder.rs`: `FlightRecorder` over a `Mutex<File>`, append
  JSONL: `{ts, agent, turn, kind, data}`. Best-effort; never panics.
- `agentd/agent.toml`: example with `[agent]`, `[model]`, `[tools]`.
**Acceptance:**
- `cargo build` (debug + release) succeeds; `cargo clippy -- -D warnings` clean.
- `cargo run -- agent.toml` appends one well-formed JSONL line to `flight.jsonl`
  and exits 0.
- Loading a missing/invalid config gives a single-line error and exits non-zero
  (no panic, no backtrace under normal `RUST_BACKTRACE`).

### ▣ p0.2 — Inference gateway + Anthropic backend
**Depends on:** p0.1.
**Goal:** A working call to the Anthropic Messages API behind a
provider-agnostic trait. Cognition is remote — this is the door to it.
**Scope:**
- `agentd/Cargo.toml`: add `reqwest = { version = "0.12", default-features =
  false, features = ["json", "native-tls"] }` and `async-trait`.
- `agentd/src/inference/mod.rs`: `InferenceGateway` trait (async `infer`,
  `model_id`); neutral types `Block` (Text / ToolUse / ToolResult), `Msg`,
  `ToolSpec`, `InferenceRequest`, `InferenceResponse { blocks, stop_reason,
  input_tokens, output_tokens }`.
- `agentd/src/inference/anthropic.rs`: `AnthropicGateway` reading
  `ANTHROPIC_API_KEY` (and optional `ANTHROPIC_BASE_URL`), header
  `anthropic-version: 2023-06-01`, maps neutral `Block`s to/from Anthropic
  content blocks, parses `content`, `stop_reason`, and `usage`.
- A small `--probe "hello"` mode in `main.rs` (or a dedicated test) that takes a
  prompt, calls the gateway, prints the text response.
**Acceptance:**
- The probe returns a non-empty text response against a live key.
- Missing key or 4xx/5xx becomes a recorded error and a clean exit, not a panic.
- Tokens used are logged.

### ▣ p0.3 — Tool ABI + native tools
**Depends on:** p0.2.
**Goal:** A unified registry of tools advertised to the model, with built-in
natives so the spike runs with zero external dependencies.
**Scope:**
- `agentd/src/tools/mod.rs`: `#[async_trait] trait Tool { name, description,
  input_schema, async invoke }`; `ToolRegistry` with `register`, `specs`,
  `tool_names`, async `invoke`.
- `agentd/src/tools/native.rs`: `ReadFile`, `WriteFile`, `ListDir` implementing
  `Tool` (`read_file` capped at 100k chars; `list_dir` suffixes directories with
  `/`). `register_native(reg, names)` supports `["all"]` and per-name selection.
- `main.rs`: build the registry from `config.tools.native` and log the resulting
  tool list.
**Acceptance:**
- Configured natives are registered; a `tools_registered` flight event lists them.
- A unit test invokes `read_file` against `Cargo.toml` and asserts a non-empty result.
- Invoking an unknown tool returns an `anyhow` error (never a panic).

### ▣ p0.4 — The agent loop (perceive → infer → act → observe)
**Depends on:** p0.3.
**Goal:** A complete single-agent run end to end using native tools. **This is
the Phase 0 success criterion.**
**Scope:**
- `agentd/src/agent.rs`: `Agent` from config, `run(gateway, registry, recorder,
  task)` that drives the loop until the model produces a final answer, the
  token budget is blown, or `max_turns` is hit. Tool errors come back as
  `Block::ToolResult { is_error: true, .. }` — the agent reacts, never panics.
- `main.rs`: read task from `config.agent.task` or stdin (if not a tty); print
  the final answer to stdout; logs to stderr.
- Flight events per `docs/CONVENTIONS.md`: `agent_spawned`, `perceive`,
  `inference_request`, `inference_response`, `tool_call`, `tool_result`,
  `observe`, terminal (`agent_completed` | `budget_exceeded` | `max_turns_reached`).
**Acceptance:**
- The scout demo (`agent.toml`: list the project dir, then read `Cargo.toml`,
  then report) runs end to end against the live model and produces a final
  answer on stdout.
- The flight log shows the full event sequence with token usage on each turn.
- A low `token_budget` triggers `budget_exceeded` cleanly; a tight `max_turns`
  triggers `max_turns_reached`.

### ▣ p0.5 — Real MCP stdio client
**Depends on:** p0.4.
**Goal:** Real MCP servers can be plugged in over stdio as a source of tools.
This is what makes "MCP is the tool ABI" concrete.
**Scope:**
- `agentd/src/tools/mcp.rs`: `McpClient` owning a `tokio::process::Child` with
  `kill_on_drop(true)`, talking newline-delimited JSON-RPC 2.0 over the child's
  stdin/stdout behind a `tokio::sync::Mutex`. Handshake: `initialize`
  (`protocolVersion: "2024-11-05"`) → `notifications/initialized` → `tools/list`.
  `tools/call` for invocation. `McpTool` implementing `Tool` collects text
  parts from `result.content` and surfaces `isError: true` as an `anyhow` error.
- `main.rs`: for each `[[tools.mcp_servers]]`, spawn the client, register its
  tools; keep clients alive for the run's duration.
- Update `agent.toml` example comment for attaching
  `@modelcontextprotocol/server-filesystem`.
**Acceptance:**
- Configured against `npx -y @modelcontextprotocol/server-filesystem .`, the
  agent discovers and uses the server's tools to complete a task.
- Server lifecycle is clean: spawn → handshake → use → process dies when
  `agentd` exits.
- Server stderr is preserved (inherit), not silenced — visible if a server misbehaves.

**Phase 0 exit criteria:** the scout demo runs end to end against the live
Anthropic API using a real MCP server; the flight log shows the full event
sequence; the release binary is small (~2 MB is a reasonable target on Linux).

---

## Phase 1 — From an agent to an OS

Goal: many agents under a **scheduler**, talking over an **inter-agent bus**, with
**scoped capabilities**. This is where the genuinely novel problem lives —
scheduling metered, GPU-scarce, non-CPU-bound cognitive work — so it gets the
most detail.

### ▣ p1.1 — Agent as a sans-IO state machine
**Depends on:** p0.5
**Goal:** Make the loop drivable one step at a time so a scheduler can own it and
interpose on every inference call. The agent should *describe* what it needs next,
not perform IO itself.
**Scope:** `agentd/src/agent.rs` → `agentd/src/agent/` (module split: `mod.rs` +
`driver.rs`). `agentd/src/config.rs` (added `Clone` to `AgentConfig` + `ModelConfig`).
- `AgentTask`: the agent config + working context (`Vec<Msg>`) + token ledger +
  turn counter + internal `Option<InferenceResponse>` state discriminant.
  **Deviation from this spec:** `AgentTask` carries no `AgentStatus` field. The
  scheduler infers state from the last returned `AgentEffect`; an explicit status
  field would duplicate state and require maintaining a redundant invariant.
- `step(&mut self, recorder) -> AgentEffect`, where
  `AgentEffect ∈ { Infer(InferenceRequest), CallTools(Vec<Block>), Completed(String), Failed(String) }`.
  MaxTurnsReached fires in the `NeedInfer` branch **before** emitting InferenceRequest.
- `provide_inference(response, recorder)` stores the response and accumulates
  token counts (no events emitted here).
- `provide_tool_results(results, recorder)` appends results to message history,
  emits `Observe`, and advances the turn counter.
- `pub fn turn(&self) -> u32` getter so the driver can record `EventKind::Error`
  with the correct turn number on gateway failures.
- Thin `driver::run()` that performs IO inline; emits ToolCall+ToolResult
  interleaved per-tool (preserving `ToolCall₁→ToolResult₁→ToolCall₂→ToolResult₂`
  order for byte-for-byte flight log parity with Phase 0).
- `MockGateway` and all existing tests relocated to `agent/mod.rs` under `#[cfg(test)]`.
**Acceptance:** demo unchanged; three new unit tests:
  `step_machine_text_tool_text_cycle` (sync, no network),
  `max_turns_fires_before_infer_request` (verifies D3 placement in flight log),
  `provide_inference_on_terminal_task_is_noop` (no panic + error event emitted).
All pass. `jq 'del(.ts)' flight.jsonl` produces identical output before and after
for the same task.

### ▣ p1.2 — The scheduler (multi-agent, cooperative)
**Depends on:** p1.1
**Goal:** Run many agents concurrently; the scheduler drives each agent's effects
and performs the IO (inference + tools).
**Scope:** new `agentd/src/scheduler.rs`; `agentd/src/config.rs` (support `[[agents]]`); `agentd/src/main.rs`.
- `Scheduler` owns the set of `AgentTask`s, a ready queue, the gateway, and the
  registry. Loop: pick a ready agent → `step` → fulfill the effect (await
  inference / invoke tools, concurrently across agents) → feed back → repeat.
- Config grows to multiple agents; keep single-agent config accepted (back-compat).
  **Deviation:** `AgentSpawned` is emitted in `main.rs` before gateway init (not in
  `Scheduler::run()`) to preserve the invariant that startup events are always written
  even when gateway initialization fails. Scheduler otherwise owns the drive loop.
  `run_tools_sequential` extracted as `pub(crate)` in `agent/mod.rs`, shared by
  `driver::run` (single-agent shim) and `Scheduler::run`.
**Acceptance:** boot 2+ agents on independent tasks; they run concurrently to
completion; flight events are interleaved and tagged by agent id.

### ▣ p1.3 — Metered scheduling & admission control
**Depends on:** p1.2
**Goal:** Enforce a **global** cognition budget and concurrency under scarcity —
defer rather than overspend. This is the core research problem; treat it as such.
**Scope:** `agentd/src/scheduler.rs`; `agentd/src/config.rs`; `agentd/src/flight_recorder.rs`.
- Global token ceiling across all agents (per-agent budgets still apply).
- Max in-flight inference concurrency cap (`[scheduler]` TOML section; `0` = unlimited).
- Priority + fair-share policy: `BinaryHeap<DeferredInfer>` keyed by `(priority desc,
  seq asc)`; per-agent `priority: u32` field (default 0). When cap is full, agents
  enter the deferred queue and are admitted as slots open.
- Flight events: `agent_scheduled`, `agent_deferred`, `agent_admission_denied`.
- **Deviation:** no token-rate limiter (roadmap says "optional"; acceptance criteria
  does not test it; deferred to a later TODO). No `budget.rs` — budget state lives as
  local variables in `Scheduler::run()`.
**Acceptance:** with `global_token_budget = 10` and `max_concurrent_inferences = 1`,
two agents are serialized; the second is deferred at seed, then denied when the first
inference exhausts the budget; total spend never exceeds the ceiling; flight log shows
`agent_scheduled`, `agent_deferred`, `agent_admission_denied`.

### ▣ p1.4 — Capability system (least privilege)
**Depends on:** p1.3 *(logically independent of p1.3 — could run in parallel —
but kept linear here for a simple single stack)*
**Goal:** Tools become **scoped grants**, not ambient authority.
**Scope:** new `agentd/src/capability.rs`; `agentd/src/tools/mod.rs` (enforce at invoke); `agentd/src/config.rs`.
- `Capability` set: e.g. `FsRead{prefix}`, `FsWrite{prefix}`, `Net{hosts}`,
  `Mcp{server, tools}`, `Spawn`.
- Each `AgentTask` carries granted capabilities (from config). The registry (or a
  wrapping layer) checks the tool's required capability before invoking; a denial
  becomes a recorded error returned to the agent as an `is_error` tool result.
**Acceptance:** an fs-read-only agent is denied `write_file` (event
`capability_denied`, error tool result); a granted agent succeeds; unit tests cover
the checks.

### ▣ p1.5 — Inter-agent bus + sub-agents (A2A/ACP)
**Depends on:** p1.4
**Goal:** Agents address and message each other; an agent can spawn a sub-agent and
await its result.
**Scope:** new `agentd/src/bus.rs`; `agentd/src/scheduler.rs` (new agent states); tool surface.
- Mailbox/router keyed by agent id. Runtime primitives as tools: `send_message{to,
  content}`, `spawn_agent{role, task, capabilities?}` → handle; child completion
  delivered back to the parent.
- Spawned agents become new `AgentTask`s in the scheduler; a parent awaiting a child
  enters `AwaitingAgent`. Spawning is **gated by the `Spawn` capability** (p1.4).
**Acceptance:** agent A spawns B with a sub-task; B runs (subject to budget +
capabilities); A receives B's result and uses it; flight log shows the cross-agent
exchange.

### ▣ p1.6 — Agent identity & Agent Cards (discovery) ✓
**Depends on:** p1.5
**Goal:** Each agent advertises an identity + skills so others can discover what it
can do (A2A Agent Card) and send messages.
**Delivered:**
- `AgentCard { id, name, description, skills }` derived from TOML at startup; emits
  `agent_card_registered` flight event.
- `AgentConfig` gains optional `name`, `description`, `skills` fields with serde defaults.
- `bus.rs` module: `MailMessage` + `Mailboxes` (per-agent `Vec<MailMessage>`).
- `list_agents` tool: returns sorted JSON array of all `AgentCard`s; no capability required.
- `send_message` tool (sole-call): `AgentEffect::SendMessage`; scheduler delivers to
  recipient mailbox; synthesizes `ToolResult` so sender continues; unknown recipient
  returns `is_error` tool result (no crash).
- Mailbox drain: `drain_mailbox` called before each `step()`; `inject_messages` appends
  to last User message block to satisfy Anthropic alternating-role requirement.
- Shutdown drain fix: `shutdown_requested: bool` in `SchedulerState`; `drain_deferred`
  emits `agent_admission_denied { reason: "shutdown" }` instead of silently re-queuing.
- New flight events: `agent_card_registered`, `message_sent`, `message_received`.
**Acceptance:** agents are enumerable with their cards; A can discover B's advertised
skill before messaging it; A2A messaging is fully recorded.

**Exit criteria for Phase 1:** multiple capability-scoped agents run under a
budget-aware scheduler, discover and message each other, and spawn sub-agents — all
fully recorded. The runtime is now an OS for agents, still running on a normal distro.

---

## Phase 2 — The distro (bootable & light)

Goal: turn the runtime into a minimal bootable image where `agentd` is the userspace.
See DESIGN.md Parts 4 & 6.

### ▣ p2.1 — rustls + static musl binary ✓
**Depends on:** Phase 1. Switch `reqwest` to `default-features = false, features =
["json", "rustls-tls"]`; build `--target x86_64-unknown-linux-musl`.
**Delivered:** `reqwest` switched from `native-tls` to `rustls-tls`; `cross` used
for `x86_64-unknown-linux-musl` target; binary is `static-pie linked, stripped`,
3.1 MB, no system OpenSSL dependency.
**Acceptance:** a static `agentd` runs with no system OpenSSL dependency. ✓

### ▣ p2.2 — Buildroot minimal rootfs ✓
**Depends on:** p2.1. Buildroot config (musl + busybox) producing a tiny rootfs that
boots straight to `agentd` as the boot target.
**Delivered:** `distro/` external Buildroot tree (x86_64 musl, BusyBox, cpio.gz initramfs);
`/init` PID-1 sh script; two virtio-9p mounts (secrets, output); `make build/run/test`
with QEMU `-no-reboot` + `jq` flight-event check; DNS via QEMU SLIRP (10.0.2.3);
bundled CA certs via `webpki-roots` (no system ca-certificates needed).
**Acceptance:** QEMU boots directly into `agentd` running an agent. ✓

### ▣ p2.3 — Boot/supervision basics
**Depends on:** p2.2. `agentd` (or a tiny init in front of it) handles PID-1 duties:
signals, zombie reaping, essential mounts, clean shutdown.
**Acceptance:** clean boot and shutdown in QEMU. ✓
**Delivered:** SIGTERM/SIGINT handling wired into `scheduler.rs::run()` via
`tokio::select!`; `SystemShutdownRequested` flight event; essential mounts and
zombie reaping already handled by `/init` + tokio respectively (no code needed).

### ▣ p2.4 — Image size budget ✓
**Depends on:** p2.3. Measure and trim toward the "super light" target; add a CI size
check.
**Delivered:** musl static binary measured at 3.1 MB; CI guard added (`stat -c %s`; fails
if > 4,194,304 bytes); release profile already at maximum optimization.
**Acceptance:** documented image size with a CI guard against regressions. ✓

### ▣ p2.5 — Phase 0/1 deferred-item cleanup ✓
**Depends on:** p2.4. Resolves all P2-priority deferred items before Phase 3 adds
architectural complexity. Scope is intentionally narrow — no new features, only correctness
and observability fixes.

**Items in scope:**

1. **Async-safe native tools** (`p0.5` debt): migrate `read_file`, `write_file`, `list_dir`
   from `std::fs` to `tokio::fs`. The sync calls block the tokio thread pool under concurrent
   tool dispatch, which becomes a real problem in Phase 3.
   — `TODOS.md`: "Sync I/O in native tool impls"

2. **`StopReason::MaxTokens` silent empty response** (`p0.4` debt): when the model is cut off
   mid-generation, `agent::run` currently returns `Ok("")` because no `Text` block is present.
   Return a distinct `Err(AgentError::BudgetExceeded)` or emit a `tracing::warn!` so callers
   can distinguish truncation from a genuine empty answer.
   — `TODOS.md`: "StopReason::MaxTokens produces empty Ok("")"

3. **MCP `tools/list` pagination** (`p0.5` debt): `McpClient::spawn` silently drops tools
   beyond the first page. Implement cursor-based iteration so all tools are registered.
   — `TODOS.md`: "MCP tools/list pagination not followed"

4. **MCP graceful shutdown** (`p0.5` debt): replace SIGKILL-on-drop with
   `notifications/shutdown` + SIGTERM + grace-period fallback to SIGKILL. Prevents data
   loss in stateful MCP servers.
   — `TODOS.md`: "MCP graceful shutdown"

**Out of scope** (deferred by design to Phase 4):
- Symlink traversal prevention (requires namespace sandbox)
- Net capability enforcement (no Net tools exist yet)
- Case-sensitive path matching on macOS (Linux-only production target)

**Acceptance:** ✓
- `cargo clippy -- -D warnings` clean; all existing tests pass; new tests cover each fix.
- Native tools use `tokio::fs`; no `std::fs` calls in `tools/native.rs`.
- `cargo test` includes a test that a max-tokens truncation is distinguishable from `Ok("")`.
- MCP client iterates all pages; integration test with a fixture server that returns a `nextCursor`.
- MCP shutdown sends `notifications/shutdown` before SIGTERM; integration test confirms ordering.
- All four TODOS.md entries marked done.

---

## Phase 3 — OS surfaces (agents as first-class kernel objects)

Goal: make "agent as primitive" visible at the system level. See DESIGN.md Part 4 (L2).

### ▣ p3.1 — `/agents` FUSE filesystem
Each running agent appears as a directory (`status`, `context_size`, `budget`,
`flight` tail). **Acceptance:** `ls /agents`, `cat /agents/<id>/status` work against
the live runtime.

### ▣ p3.2 — Agent checkpoint / restore
Persist an agent's working context + ledger to disk and resume it (app-level first;
CRIU exploration later). **Acceptance:** suspend an agent, restart `agentd`, resume it.

### ✓ p3.3 — eBPF/LSM enforcement (exploratory)
Enforce capability scopes (p1.4) at the syscall boundary for tool subprocesses.
**Completed:** `sandbox/` crate; Landlock V1 FS rules + seccomp-bpf fork/vfork block;
`capabilities` field on `[[tools.mcp_servers]]`; `SandboxApplied`/`SandboxSkipped` events.

---

## Phase 4 — Isolation & hardening

Goal: defense-in-depth for tools/agents. See DESIGN.md Part 5.

### ✓ p4.1 — Per-tool sandboxing (seccomp + namespaces)
Run tool-servers sandboxed; map each capability set to a sandbox profile.
**Acceptance:** a tool cannot exceed its capability at the OS level.
**Completed:** v0.11.0 — BPF arch gate (aarch64 false-positive fixed); `EnforcementStatus`
+ `CompiledSandbox::enforcement_status()`; `compile()` moved to `main.rs`; `SandboxApplied`
payload gains `enforced:{landlock,seccomp,spawn_enforcement}`; `mcp_require_capabilities`
flag; CI musl ≤4 MB size guard restored.

### ✓ p4.2 — Stronger isolation option (namespaces + gVisor)
Two-tier isolation upgrade. Layer 1 (all sandboxed servers): Linux namespaces via
`unshare(CLONE_NEWUSER | CLONE_NEWNET)` in `pre_exec`; `IsolateNetwork` and
`IsolateMount` `SandboxRule` variants; `caps_to_rules()` now enforces `Net` at the
kernel level (absent `Net` cap → `IsolateNetwork` added automatically). Layer 2
(opt-in per server): `isolation = "gvisor"` field on `[[tools.mcp_servers]]`; wraps
the server command with `runsc do [--network=none] --`; agentd fails fast at startup
if `runsc` is not on PATH. `EnforcementStatus` gains `namespace_net` and
`namespace_mount` fields. `SandboxApplied` event payload extended with `isolation`
and namespace enforcement fields. `CONFIG_USER_NS=y`, `CONFIG_NET_NS=y` in
kernel-extras.config. 209 tests pass.
**Known limitations:** `runsc do` is experimental (full OCI bundle deferred to
TODOS.md); `clone3()` bypass in the namespace-only path remains (TODOS.md);
`CLONE_NEWPID` for PID namespace requires a re-fork and is deferred.
**Acceptance:** ✓

### ✓ p4.3 — Security review pass
Threat model: secret handling, flight-recorder redaction, budget-exhaustion DoS,
supply chain. `docs/THREAT_MODEL.md` written. `ToolCall.input` → `input_preview`
(200-char truncation); `ToolResult.error` → truncated (200-char); `AgentSpawned.task`
→ `task_preview` (200-char, both `main.rs` and `scheduler.rs` dispatch_spawn paths).
`truncate()` + `PREVIEW_CHARS` made `pub` in `agent/mod.rs`. TODOS entry for
checkpoint.json encryption. **Acceptance:** ✓

### ✓ p4.4 — TODOS cleanup sprint
Four tracked TODOS items addressed in one increment:
- **checkpoint.json mode 0600**: `write_mode_600()` helper in `checkpoint.rs` creates the
  tmp file with `O_CREAT | 0600`; `rename()` preserves permissions on final file. Test added.
  THREAT_MODEL.md §3.2–3.3 updated.
- **pre_exec error propagation**: pre-exec error pipe (`pipe2 + O_CLOEXEC`) in `mcp.rs`;
  on sandbox failure the child writes "sandbox" tag; parent reads the tag and includes it
  in the spawn error message — "sandbox stage: 'sandbox'" — replacing silent EPERM.
- **sandbox_probe integration tests (Linux)**: 3 tests gated `#[cfg(target_os = "linux")]`
  in `tests/integration.rs` — AllowFsRead grants access, AllowFsRead denies out-of-prefix reads,
  DenySpawn blocks fork (x86_64 only, gated `#[cfg(target_arch = "x86_64")]`).
- **--no-fuse / AGENTOS_NO_FUSE**: `main.rs` args parsing extended; `run_agent(no_fuse: bool)`;
  FUSE mount block respects flag — skips mount with `tracing::info!` instead of attempting it.
**Acceptance:** ✓

### ✓ p4.5 — TODOS cleanup + hardening polish
Five tracked TODOS items addressed + one red-team finding fixed:
- **EventKind extraction**: `EventKind` enum moved from `flight_recorder.rs` to `events.rs`;
  re-exported from `flight_recorder` so all existing import paths remain valid.
- **aarch64 DenySpawn no-op**: `is_noop_deny_spawn()` helper detects when all enforcement
  fields are false; emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }` instead
  of misleading `SandboxApplied` with all-false fields. `has_rules` narrowed at the call
  site to check for `DenySpawn` specifically.
- **`--log-path` CLI flag**: `parse_log_path` / `resolve_log_path` / `filter_positional_args`
  helpers; `run_agent` accepts `log_path_override: Option<PathBuf>`; TOML `log_path` field
  also supported. Precedence: CLI > TOML > default `"flight.jsonl"`.
- **Buildroot ccache**: `BR2_CCACHE=y` + `BR2_CCACHE_DIR=$(HOME)/.buildroot-ccache`; subsequent
  clean builds use host cache (~2 min vs ~30 min).
- **TODOS.md housekeeping**: stale items cross-off pass; P4 item added for `run_probe`.
- **Red-team LOW — `--log-path` silent swallow**: `anyhow::bail!` when flag present but no
  value follows.
244 tests pass. **Acceptance:** ✓

### ✓ p4.6 — Landlock V4 TCP port enforcement + run_probe --log-path fix
Two items:
- **run_probe --log-path**: `run_probe` signature updated to accept `log_path: PathBuf`;
  `resolve_log_path(log_path_override, None)` passed from `main()`; uses `FlightRecorder::new`
  instead of the hard-coded `FlightRecorder::open`. `--probe --log-path /tmp/out.jsonl` now works.
- **Landlock V4 net enforcement**:
  - `AllowNetConnect { port: u16 }` added to `SandboxRule` — port-only (Landlock V4 enforces
    TCP ports, not hostnames). `Net { hosts, ports: Vec<u16> }` in `Capability` — `ports` field
    with `#[serde(default)]` (existing configs without `ports` are backward-compatible).
  - `LandlockRulesetAttrV4 { handled_access_fs, handled_access_net }` and
    `LandlockNetPortAttr { allowed_access, port }` structs; `LANDLOCK_RULE_NET_PORT = 3`;
    `LANDLOCK_ACCESS_NET_CONNECT_TCP = 1 << 1`.
  - Runtime ABI detection: `query_landlock_abi_version()` calls
    `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION=1)`.
    V4 (kernel ≥ 6.7) → 16-byte V4 struct + net rules; V3/V1 → 8-byte V1 struct, net rules
    silently skipped (BestEffort degradation). FS rules unaffected.
  - `EnforcementStatus.landlock_net: bool`; `SandboxApplied` event payload gains
    `enforced.landlock_net`. `caps_to_rules()` generates `AllowNetConnect` rules from
    `Net.ports` (empty ports = no restriction, backward compat).
249 tests pass. **Acceptance:** ✓

---

## Beyond

Memory substrate (two-tier in-context / external memory from DESIGN.md), additional
inference backends (incl. a local `impl InferenceGateway`), richer A2A/ACP
interop, and multi-device agent migration. Re-plan these into a stack when Phase 1–2
land.
