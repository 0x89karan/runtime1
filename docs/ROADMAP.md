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

## Phase 5 — Memory substrate

Goal: the missing subsystem — a four-tier memory substrate (in-context working,
per-agent short-term, per-agent long-term, shared knowledge base) on one
capability-segmented store. See DESIGN.md Part 4.3 / 5.3 and the full design in
`docs/DESIGN-memory.md`. Per-increment build detail (files, types, tests, events,
acceptance) lives in `docs/PHASE-5-PLAN.md` — the entries here mirror the Phase 1/4
depth; the plan doc is what each increment `/autoplan`s against.

Architectural calls (from `docs/DESIGN-memory.md`): **redb** (pure-Rust, ~0.6 MB) is
the store — bundled SQLite would breach the 4 MB CI guard and dynamic SQLite breaks
the static-musl binary. Memory is **memory-as-tool** (no new `AgentEffect`/`Block`).
Eviction is **runtime-floor + agent-policy** (defer while budget allows, force at a
hard ceiling). Retrieval is an **explicit tool, never automatic injection**
(metered-cognition lock). No vectors/embeddings in Phase 5 (preserves
cognition-is-remote). `checkpoint.json` **coexists** as crash recovery; it does not
become the memory store.

### ✓ p4.7 — Pre-Phase-5 cleanup (audit blockers) — **prerequisite**
**Depends on:** p4.6. Closes the `docs/AUDIT-phase-4-6.md` findings that block Phase 5
or compound once a second writer touches working memory: F-001 (MCP subprocess env
leak — `env_clear` + allowlist), F-009 (`Arc<[Msg]>` request — no per-turn deep
clone), F-005/F-006 (mailbox-injection ordering), F-011 (checkpoint version probe
before deserialize + unique tmp name), F-004 (FUSE read overflow), F-013/F-014 (six
event kinds into CONVENTIONS; README phase status). Demo flight logs stay
byte-for-byte identical. **Acceptance:** build/clippy/test/clippy-linux clean; +≥6
tests; binary size unchanged. (F-002/F-003 sandbox-net hardening are recommended as a
non-blocking **p4.8**, not gating Phase 5.)

### ✅ p5.1 — Storage primitive (redb behind `MemoryStore`)
**Depends on:** p4.7. The substrate behind a thin trait — `agentd/src/memory/{mod,store}.rs`,
`RedbStore` over `memory.redb` (mode 0600, quarantine-on-corrupt, never deleted on
success). New `Capability::KbRead { segment }` / `KbWrite { segment }` (prefix match
like `FsRead`, deny-by-default). Single agent uses it via `kv_get`/`kv_set` native
tools over a `scratch:` namespace; demoable in isolation. Events: `memory_read`,
`memory_write`, `memory_unavailable`, `memory_quarantined`. **Acceptance:** +6 tests;
**binary size delta documented, must stay ≤ 4 MB** (≈ +0.6 MB expected).
**Shipped:** 304 tests pass. Binary 2.1 MB (macOS) / 3.8 MB (x86_64-linux-musl);
redb 4.1.0 added ≈ +0.2 MB (macOS) / +0.7 MB (musl); well under ≤ 4 MB guard.
Post-adversarial-review: `MAX_KV_VALUE_BYTES = 256 KiB` enforced in `kv_set`; 5 minor
findings deferred to TODOS.md (kv-ar-01 through kv-ar-05).
`docs/INTERFACE.md` (Phase 6 design) added in the same branch — docs-only, no code impact.

### ✅ p5.2 — Per-agent short-term + paging (v0.19.0)
**Depends on:** p5.1. `memory/context.rs` with `MemoryPressure`, `assess()`,
`page_count()`, `page_turns()`. `MemItem { turn, role: Role, content_preview, blocks_json }`.
Soft threshold (75%) → `memory_pressure_advisory` event only. Hard threshold (90%) →
force-evict oldest turn PAIRS (preserves alternating-role invariant). `short_term: Vec<MemItem>`
on `AgentTask` and `AgentCheckpoint` (`#[serde(default)]`); `FORMAT_VERSION` 1 → 2.
`to_checkpoint`/`from_checkpoint` explicitly updated. `docs/CONVENTIONS.md` updated with
FORMAT_VERSION migration policy and both new events. 14 acceptance tests; 322 tests pass.

### ✅ p5.3 — Per-agent long-term + checkpoint coexistence
**Depends on:** p5.1, p5.2. `mem_remember`/`mem_recall` over the agent's own
`agent/<id>` namespace (implicit self-grant; cross-agent Tier-3 read needs `KbRead`).
Durable across runs — `memory.redb` persists where `checkpoint.json` is deleted on
success. Runtime-stamped, unforgeable provenance. Event: `memory_distilled` (manual
remember). **Acceptance:** +4 tests; after clean completion `memory.redb` exists and
`checkpoint.json` does not; provenance unforgeable.

### ▢ p5.4 — Shared KB MVP (namespace + mutability classes + provenance)
**Depends on:** p5.3. Multi-agent segmented KB: one namespace axis, three classes
(`canon` read-only / `log` append-only / `scratch` mutable LWW+version). `kb_put`/
`kb_get`; `KbRead`/`KbWrite` enforced across agents; `[[memory.segments]]` config
seeds canon. The DESIGN-memory §4 worked example (agent A logs, later agent B
retrieves with provenance, no Net) ships as an integration test. Extends
`memory_write`/`memory_read` with `tier:4` + `class`. **Acceptance:** +6 tests;
worked-example test passes; THREAT_MODEL §7.1/§7.2 stubbed.

### ▢ p5.5 — Retrieval as tool (lexical search)
**Depends on:** p5.4. `kb_search { segment?, query, author?, limit? }` over a
tokenized inverted index (`memory/index.rs`, BM25-lite in Rust) maintained
transactionally with entry writes. No embeddings, no network. Event: `kb_search`.
**Acceptance:** +6 tests; ranked, segment-scoped, author-filterable; `KbRead`-gated.

### ▢ p5.6 — Eviction & summarization
**Depends on:** p5.5. Per-segment capacity/age eviction floor (drops oldest + index
postings in one txn); optional end-of-run distillation (`distill_on_complete`,
default off — one budget-bounded inference promoting short-term to Tier 3). Event:
`memory_evicted`. **Acceptance:** +5 tests; eviction observable; distillation respects
the budget guard and is off by default (demos unchanged).

### ▢ p5.7 — `/agents/<id>/memory/` + `/agents/kb/` FUSE (read-only)
**Depends on:** p5.4, p4.7 (F-004). Memory observable from the control plane,
following the existing inode scheme: per-agent `memory/{short_term,long_term/}` and an
operator `kb/<segment>/` browse (not an agent capability — does not bypass `KbRead`).
Read-only; bounded snapshot projection. **Acceptance:** +4 tests; `make clippy-linux`
clean; `cat /agents/<id>/memory/long_term/<key>` works on the QEMU image; F-004
regression test passes.

### ▢ p5.8 — Phase 5 hardening pass
**Depends on:** p5.1–p5.7. THREAT_MODEL §7 written in full; the **p4.6-shaped startup
invariant** asserted (`memory.redb` path must not fall inside any MCP server's FS
sandbox prefix); a demo `agents.toml` that actually exercises memory (KbWrite+Net
agent, KbRead agent, seeded canon, spawning, non-zero global budget); CONVENTIONS
table completeness check (+14 rows total across Phase 5); TODOS swept. **Acceptance:**
+3 tests; sandbox-path assertion test passes; memory-demo flight log shows
`memory_write`/`kb_search`/`memory_read`/`memory_paged`; binary ≤ 4 MB.

**Exit criteria for Phase 5:** a finding written by one agent is retrievable with
provenance by a later, differently-capability-scoped agent over a capability-gated
shared KB; working memory pages under budget pressure; long-term memory survives
restart; memory is browsable under `/agents`; the binary is still ≤ 4 MB; and the
single-agent demo's flight-event sequence is unchanged when memory is unused. Full
detail and per-increment acceptance: `docs/PHASE-5-PLAN.md`.

---

## Phase 6 — Interface and agent catalogue

Goal: give the operator a real, legible interface over the multi-agent runtime, and a
template catalogue that surfaces *what can run and how to run it* — without breaking the
"CLI is the contract" rule or the super-light budget. Full design in `docs/INTERFACE.md`.

Architectural calls (from `docs/INTERFACE.md`): a **`ratatui` TUI shipped as a separate
`agentctl` binary** (not baked into `agentd`, so the 4 MB guard is untouched; a GUI's
browser/WebView weight can't live in the QEMU image). The interface is a **read-only
view** over the existing `/agents/<id>/*` FUSE files + `flight.jsonl` — new data gets a
`surfaces/` amendment, never a parallel data plane. The one write need (spawn) is a
narrow `/agents/control` endpoint; everything else stays daemon-free. **Templates**
(`*.template.toml`, a superset of `agent.toml`) make agents discoverable before they
run; `agentctl spawn <template>` generates an `agent.toml` and execs `agentd`.

### ▢ p6.1 — Template schema + on-disk catalogue
**Depends on:** Phase 4.6. `*.template.toml` schema (superset of `agent.toml`:
`[template]`, suggested `[capabilities]` deny-by-default, `[[tools.mcp_servers]]`,
`[memory]` segments, `[card]`, `sample_tasks`). On-disk: `templates/` (repo) +
`~/.agentos/templates/` (user), user-overrides-repo precedence. CLI-consumable, no UI.
**Acceptance:** a template parses, resolves by name with correct precedence, and
generates a valid `agent.toml`; tests cover precedence + the strip-template-keys path.

### ▢ p6.2 — `agentctl list-templates` / `agentctl spawn <template>`
**Depends on:** p6.1. New `agentctl/` workspace member (the operator CLI). `spawn`
generates an `agent.toml` from a template + `--task`/`--cap-add` overrides
(deny-by-default base) and execs `agentd`. No daemon, works in QEMU. **Acceptance:**
`agentctl spawn scout --task "…"` runs the scout end-to-end; `list-templates` shows
name·source·showcases; capability overrides never exceed the template's suggestions
without an explicit flag.

### ▢ p6.3 — Read-only TUI: dashboard + agent detail + system
**Depends on:** p6.2, p3.1 (FUSE). `ratatui`+`crossterm` in `agentctl`; reads
`/agents/<id>/{status,context_size,budget,flight}` + the proposed `/agents/system`
surface. Requires `surfaces/` amendments: status enrichment (awaiting-inference vs
awaiting-tool), `/agents/<id>/tools`, `/agents/system/{budget,queue,sandbox,provider}`
(expose the deferred-queue count, today omitted from the snapshot). **Acceptance:** the
three views render live against a running multi-agent demo over both a local mount and a
QEMU serial console; `make clippy-linux` clean for the FUSE amendments.

### ▢ p6.4 — Topology view (multi-agent graph)
**Depends on:** p6.3, p1.5/p1.6 (bus + cards). The spawn tree + message graph, derived
from `flight.jsonl` (`agent_spawned.parent_id`, `agent_child_result_delivered`,
`message_sent/received`) + snapshot `AwaitingChild`. The hard view — a time-evolving
derived graph. v1: spawn tree + completed edges; message edges layered after. Optional
`/agents/<id>/edges` surface to avoid log-scraping. **Acceptance:** a coordinator demo's
spawn tree and at least one live message edge render correctly.

### ▢ p6.5 — Memory view
**Depends on:** **p5.7** (`/agents/<id>/memory/`, `/agents/kb/<segment>/`) and the
`PHASE-5-PLAN.md §E` contracts (provenance schema, versioned store, `memory_*`/`kb_*`
events). Read-only browse of per-agent short/long-term stores and shared KB segments,
with provenance shown; lexical search box (Layer 1). Labels semantic search as
available only via an attached Layer-2 MCP KB. **Acceptance:** the journaler's long-term
entries and a shared `project:` segment are browsable with provenance; the tab degrades
gracefully ("memory subsystem not present") when Phase 5 is absent.

### ▢ p6.6 — Spawn view
**Depends on:** p6.2, p6.3. Template picker → task field → capability toggles
(pre-checked from the template, deny-by-default) → preview generated `agent.toml` →
spawn. Mode (a) generate-and-exec ships here; mode (b) inject-into-running-scheduler
depends on the writable **`/agents/control`** surface (its own sub-task). **Acceptance:**
spawning from the form produces the same `agent.toml` the CLI would and starts the agent;
no capability is granted beyond the template without an explicit toggle.

### ▢ p6.7 — Starter catalogue (the committed templates)
**Depends on:** p6.1 (+ p5.x for memory-dependent templates). Ship the 7 templates:
scout, librarian, journaler, code-aware, watcher, coordinator, memory-custodian — each
with its showcase rationale. journaler/memory-custodian are Phase-5-gated; watcher is
marked trigger-gated (no event-trigger mechanism exists yet). **Acceptance:** every
non-gated template spawns and runs its sample task; the demo set exercises sandbox +
capabilities + bus + (when present) memory.

### ▢ p6.8 — Sandbox-enforcement surface + edge-case polish
**Depends on:** p6.3, p4.1–p4.6. `EnforcementStatus` surfaced in System + Agent-detail
from `/agents/system/sandbox` + `/agents/<id>/sandbox`; **prominent degradation warnings**
(Landlock ABI < V4 → net not enforced — the F-002 silent-degradation case made visible;
aarch64 DenySpawn no-op; gVisor in effect; net-ns absent because a `Net` cap is present).
Logs/inspector filters finalized. **Acceptance:** a kernel without Landlock V4 shows the
degraded-net warning; a gVisor server shows `isolation=gvisor`; the inspector filters by
kind/agent/capability_denied/sandbox_skipped/error.

**Exit criteria for Phase 6:** an operator can, from `agentctl` over the QEMU serial
console or SSH, see every running agent and its spend/status, drill into one agent's
flight + tools + sandbox enforcement, see the spawn/message topology, browse memory
(when Phase 5 is present), spawn a new agent from a template, and answer "what did agent
X do" without hand-written `jq` — all as views over the unchanged CLI/FUSE contract,
with `agentd` still ≤ 4 MB.

---

## Beyond

Re-homed into Phase 5 (above): the memory substrate (Layer 1 — embedded, lexical).
Remaining:
- **Layer 2 — external hybrid (semantic + keyword) KB over MCP.** Attach a real search
  engine (Postgres+pgvector+FTS, or Qdrant/Meilisearch) as a sandboxed MCP server;
  embeddings come from a **remote embedding API** (Voyage AI canonical; Cohere/OpenAI
  viable), preserving the remote-cognition lock — no embedding weights on the `agentd`
  host. Stdio-sidecar is reachable within Phase 5; a **networked KB needs an HTTP/SSE
  MCP transport** added to the MCP client (its own increment), with the p4.6 Landlock
  V4 TCP-port rules as the enforcement layer. Design: `docs/DESIGN-memory.md` §4
  (two storage layers) + §9 Q1 (decided).
- Additional inference backends (incl. a local `impl InferenceGateway`, and a remote
  `embed()` method on the gateway if embeddings are ever pulled in-process rather than
  into the KB sidecar), richer A2A/ACP interop, and multi-device agent migration.

Re-homed into Phase 6 (above): the human interface layer (operator TUI + agent
catalogue). Beyond Phase 6: an event-trigger surface (unlocks the Watcher template —
the daemon-shaped agent), and write-capable memory/control surfaces.
