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

## Current build order (next up)

Everything through Phase 7, the harness (`h7.*`), `obs.*`, `cos.1`, multi-arch (`ma.1–3`), and the
credential broker (`cred.1`–`cred.3`) is shipped (v0.60.0). **Foundation-first** order for the
remaining work — one increment per branch, `main` shippable between, each through `/autoplan`
(or `/plan-eng-review`) → build → `/review` → `/qa` → `/ship`:

- ~~`cred.1` (v0.58.0) · `cred.2` (v0.59.0) · `cred.3` (v0.60.0) · `cred.3.1` (v0.61.0) · `cred.3.2` (v0.62.0)~~ ✅ shipped — credential broker core + hardening complete.
- ~~`orch.1` (v0.66.0)~~ ✅ shipped — interactive orchestrator: `AgentStatus::Waiting` + `agentctl orchestrate` REPL + HTTP spawn/inject API.
- ~~`h8.1` (v0.64.0)~~ ✅ shipped — Layer-2 semantic memory sidecar (Qdrant + Voyage AI).
- ~~`cred.4` (v0.63.0) · `cred.4b` (v0.65.0)~~ ✅ shipped — spend caps + credential-agnostic MCP servers.
1. ~~**cred.5** (v0.68.0) · **ma.4** (v0.67.0)~~ ✅ shipped — credential control plane visibility + isolation-tier honesty.
2. ~~**dx.3** (v0.69.0) · **dx.4** (v0.71.0)~~ ✅ shipped — Linux QEMU production path + prebuilt images + device auth (`agentctl auth google --device`) + install.sh.
3. ~~**h8.2** (v0.73.0)~~ ✅ shipped — `agentos:core` (Rust-only) + `agentos:full` (Python harness) image split; CI publishes both tiers with shared GHA layer cache.
4. ~~**dx.6** (v0.74.0)~~ ✅ shipped — `make dev-image` fast local loop; `publish-docker` gated to `workflow_dispatch`/`v*` tag; `AGENTOS_IMAGE` compose override; `.env.example`; DEPLOYMENT.md dev quickstart.
5. **cheap wins** — Google OAuth app → Production (kills weekly 7-day Testing-mode token expiry; `MCP_SERVERS.md` fix); secret-redaction (`oauth_mcp.py:430`, `credential/mod.rs:341,371`; folds `cred.5-ar-01`). Ships independently.
6. **cos-polish (rest)** — brief → file (`FsWrite` cap on orchestrator), KB findability (colon-segment reader, `kb_search` scope), inbox budget, orchestrate REPL race fix, `max_turns` guard.
7. ~~**memory-routing** (v0.81.0)~~ ✅ shipped — raw emails → h8.1 semantic L2 (OpenAI text-embedding-3-small); CoS email dedup via `kb_get`; fixes ~820k token/run blowup.
8. ~~**cred.6** (v0.83.0)~~ ✅ shipped — CoS broker migration; `passthrough_query_params` allowlist (D3 + Gmail params); google_oauth sidecar holds no raw credential at rest.
9. **cred.7** — credential resilience (on top of broker mode).
10. **Track UX cockpit** (agentctl-client): ux.0 (async watch refactor — land solo before splitting) → ux.9 → ux.2 → ux.1 → ux.8 → ux.3.
11. **Phase 9** — kernel observability (`ebpf.*` / `sink.1`); heavy, privileged, appliance-oriented; last.

Detailed queue + single-lane→split rules: `docs/prompts/12-build-queue-single-lane.md`.

**Deferred:** Track MESH (`mesh.1–6`, multi-instance) and ROADMAP `h8.3` (multi-device migration).
**Planned:** Track PERSONAL (`personal.1+`, operator workflow brain via gbrain — after h8.1).

Why foundation-first: the broker (`cred.3`) lands before `h8.1`/`orch.1` because `h8.1` introduces a
remote embedding API key that should ride the broker rather than become another ad-hoc secret;
deployment (`dx.3/4`) waits until you target Linux hardware; observability (Phase 9) is the heaviest
and least urgent. (If daily conversational use is the priority instead, swap to interactivity-first:
`cred.1 → cred.2 → orch.1 → cred.3 → h8.1`.)

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

### ✓ p5.3.5 — Detachable memory volume (distro/infra)
**Depends on:** p5.1 (`store_path`). Independent of p5.4+ (infra-only — no crate logic, no
schema, default unchanged), so it builds in parallel. **Run it next / alongside p5.4**,
before relying on container-respawn for memory continuity, so the shared KB lands on a
proper durable home rather than being retrofitted. Makes the durable store (`memory.redb`,
Tiers 3/4) a **separate, persistent, re-attachable volume** — a dedicated `memory0` 9p
mount (`~/.agentos-memory/` → `/run/memory`), distinct from `secrets0` (in) and the
disposable `output0` (out). Kill + respawn the container → re-attach the same volume →
knowledge continuity (run-continuity stays with `checkpoint.json`, a separate concern).
Design: `docs/DESIGN-memory.md` §6 (detachable volume); full spec + exact distro diff:
`docs/PHASE-5-PLAN.md` p5.3.5. **Acceptance:** a `kv_set` value written in one boot is
readable after a fresh boot (2-boot QA); wiping `/run/output` / `make clean` does not lose
memory; default `store_path` unchanged. (redb is single-writer → sequential container
generations; concurrent multi-container needs the Layer-2 KB service.)

### ✓ p5.4 — Shared KB MVP (namespace + mutability classes + provenance)
**Depends on:** p5.3. Multi-agent segmented KB: one namespace axis, three classes
(`canon` read-only / `log` append-only / `scratch` mutable LWW+version). `kb_put`/
`kb_get`; `KbRead`/`KbWrite` enforced across agents; `[[memory.segments]]` config
seeds canon. The DESIGN-memory §4 worked example (agent A logs, later agent B
retrieves with provenance, no Net) ships as an integration test. Extends
`memory_write`/`memory_read` with `tier:4` + `class`. **Acceptance:** +6 tests;
worked-example test passes; THREAT_MODEL §7.1/§7.2 stubbed.

### ✅ p5.5 — Retrieval as tool (lexical search)
**Depends on:** p5.4. `kb_search { segment?, query, author?, limit? }` over a
tokenized inverted index (`memory/index.rs`, BM25-lite in Rust) maintained
transactionally with entry writes. No embeddings, no network. Event: `kb_search`.
**Acceptance:** +6 tests; ranked, segment-scoped, author-filterable; `KbRead`-gated.

### ✅ p5.6 — Eviction & summarization
**Depends on:** p5.5. Per-segment capacity/age eviction floor (drops oldest + index
postings in one txn); optional end-of-run distillation (`distill_on_complete`,
default off — one budget-bounded inference promoting short-term to Tier 3). Event:
`memory_evicted`. **Acceptance:** +5 tests; eviction observable; distillation respects
the budget guard and is off by default (demos unchanged).

### ✅ p5.7 — `/agents/<id>/memory/` + `/agents/kb/` FUSE (read-only)
**Depends on:** p5.4, p4.7 (F-004). Memory observable from the control plane,
following the existing inode scheme: per-agent `memory/{short_term,long_term/}` and an
operator `kb/<segment>/` browse (not an agent capability — does not bypass `KbRead`).
Read-only; bounded snapshot projection. **Acceptance:** +4 tests; `make clippy-linux`
clean; `cat /agents/<id>/memory/long_term/<key>` works on the QEMU image; F-004
regression test passes.

### ✅ p5.8 — Phase 5 hardening pass
**Depends on:** p5.1–p5.7. THREAT_MODEL §7 written in full; the **p4.6-shaped startup
invariant** asserted (`memory.redb` path must not fall inside any MCP server's FS
sandbox prefix); a demo `agents.toml` that actually exercises memory (KbWrite+Net
agent, KbRead agent, seeded canon, spawning, non-zero global budget); CONVENTIONS
table completeness check (+14 rows total across Phase 5); TODOS swept. **Acceptance:**
+3 tests; sandbox-path assertion test passes; memory-demo flight log shows
`memory_write`/`kb_search`/`memory_read`/`memory_paged`; binary ≤ 6 MB.

### ✓ p5.9 — Phase 5 hardening (audit remediation) — **gate before Phase 6**
**Status:** landed. All P1s closed (F-01/02/03/04/09/16) + F-07a/13/14/15; each with a
regression test that fails pre-fix. `cargo build`/`clippy --all-targets`/`test` +
`make clippy-linux` clean; musl 3.07 MB. Remaining P2s in `TODOS.md`. Resolution table
in `docs/AUDIT-phase-5.md §8`. Live Stage-1 re-run + QEMU 2-boot pending a fresh key.

**Depends on:** p5.8. Closes the P1 findings from `docs/AUDIT-phase-5.md` (the analogue of
p4.7 after the 4.6 audit): **F-01** paging driven by lifetime spend, not context size →
re-target + edge-gate so it stops shredding context; **F-02** `store.open()` quarantines a
valid store on a transient I/O error → classify errors, only quarantine real corruption;
**F-03** the p5.6 eviction floor is implemented but never called → wire it + protect canon;
**F-09** validate `spawn_agent.child_id` (forgeable memory namespace/provenance); **F-04**
assert counter/key consistency (silent "empty memory" via the FUSE surface); **F-16**
spawn_agent/send_message mixed with other tools terminates the agent → return a
recoverable `is_error` (Stage-1 live finding — it kills the flagship multi-agent demo);
**F-14/F-15** fix the broken `agents.toml` demo (unsupported `seed` field; missing
`spawn_agent` tool). Plus run the behavioral QA the fast ships skipped (single/multi-agent,
kv/mem/kb paths, capability denial, sandbox enforcement, 2-boot continuity) and close the
`clippy --all-targets` gate. P2s tracked in `TODOS.md`. Full spec + exact diffs:
`docs/PHASE-5-PLAN.md` p5.9. **Acceptance:** each P1 has a regression test that fails
pre-fix; behavioral QA green; musl ≤ 6 MB. F-01/F-02 corroborated by an independent codex
cross-check; F-14/F-15/F-16 confirmed live against the API (audit §6/§7).

**Exit criteria for Phase 5:** a finding written by one agent is retrievable with
provenance by a later, differently-capability-scoped agent over a capability-gated
shared KB; working memory pages under budget pressure; long-term memory survives
restart; memory is browsable under `/agents`; the binary is still ≤ 6 MB; and the
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

### ✅ p6.1 — Template schema + on-disk catalogue (v0.27.0)
**Depends on:** p5.1 (Capability vocab + MemoryConfig). `*.template.toml` schema (superset
of `agent.toml`: `[template]`, suggested `[capabilities]` deny-by-default, `[card]`,
`sample_tasks`). On-disk: `templates/` (repo) + `~/.agentos/templates/` (user),
user-overrides-repo precedence. CLI-consumable, no UI. `Config` + sub-structs gain
`Serialize` unblocking p6.2 TOML write. 22 tests.
**Acceptance:** a template parses, resolves by name with correct precedence, and
lowers to a valid `Config` when a task override is provided (templates with `task = ""`
require a caller-supplied task); tests cover precedence + the strip-template-keys path.

### ✅ p6.2 — `agentctl list-templates` / `agentctl spawn <template>`
**Depends on:** p6.1. New `agentctl/` workspace member (the operator CLI). `spawn`
generates an `agent.toml` from a template + `--task`/`--cap-add` overrides
(deny-by-default base) and execs `agentd`. No daemon, works in QEMU. **Acceptance:**
`agentctl spawn scout --task "…"` runs the scout end-to-end; `list-templates` shows
name·source·showcases; capability overrides never exceed the template's suggestions
without an explicit flag.

### ✅ p6.3 — Read-only TUI: dashboard + agent detail + system (v0.29.0)
**Depends on:** p6.2, p3.1 (FUSE). `ratatui`+`crossterm` in `agentctl`; reads
`/agents/<id>/{status,context_size,budget,flight}` + the proposed `/agents/system`
surface. Requires `surfaces/` amendments: status enrichment (awaiting-inference vs
awaiting-tool), `/agents/<id>/tools`, `/agents/system/{budget,queue,sandbox,provider}`
(expose the deferred-queue count, today omitted from the snapshot). **Acceptance:** the
three views render live against a running multi-agent demo over both a local mount and a
QEMU serial console; `make clippy-linux` clean for the FUSE amendments.

### ✅ p6.4 — Topology view (multi-agent graph) — v0.30.0
**Depends on:** p6.3, p1.5/p1.6 (bus + cards). `parent_id: Option<String>` on
`AgentSnapshot`; `parent_map: HashMap<String,String>` (insert-only) in `SchedulerState`
+ checkpoint; `OFF_PARENT = 9` FUSE virtual file `/agents/<id>/parent`; `topology.rs`
module with `TopologyGraph`, `build_graph()` (512KB flight.jsonl tail, directed edges,
cycle guard), `render_tree()`; `View::Topology` in `agentctl watch` (`[t]` key, scroll,
fixed legend footer, min 60 cols guard); `--log-path` arg; plain mode topology section;
`coordinator-demo.agents.toml` acceptance fixture; 455 tests pass. **Acceptance met.**

### ✅ p6.5 — Memory view
**Depends on:** **p5.7** (`/agents/<id>/memory/`, `/agents/kb/<segment>/`) and the
`PHASE-5-PLAN.md §E` contracts (provenance schema, versioned store, `memory_*`/`kb_*`
events). Read-only browse of per-agent short/long-term stores and shared KB segments,
with provenance shown; lexical search box (Layer 1). Labels semantic search as
available only via an attached Layer-2 MCP KB. **Acceptance:** the journaler's long-term
entries and a shared `project:` segment are browsable with provenance; the tab degrades
gracefully ("memory subsystem not present") when Phase 5 is absent.

### ✅ p6.6 — Spawn view
**Depends on:** p6.2, p6.3. Template picker → task field → capability toggles
(pre-checked from the template, deny-by-default) → preview generated `agent.toml` →
spawn. Mode (a) generate-and-exec ships here; mode (b) inject-into-running-scheduler
depends on the writable **`/agents/control`** surface (its own sub-task). **Acceptance:**
spawning from the form produces the same `agent.toml` the CLI would and starts the agent;
no capability is granted beyond the template without an explicit toggle.

### ✅ p6.7 — Starter catalogue (the committed templates)
**Depends on:** p6.1 (+ p5.x for memory-dependent templates). Ship the 7 templates:
scout, librarian, journaler, code-aware, watcher, coordinator, memory-custodian — each
with its showcase rationale. journaler/memory-custodian are Phase-5-gated; watcher is
marked trigger-gated (no event-trigger mechanism exists yet). **Acceptance:** every
non-gated template spawns and runs its sample task; the demo set exercises sandbox +
capabilities + bus + (when present) memory.

### ✅ p6.8 — Sandbox-enforcement surface + edge-case polish (v0.34.0)
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

## Core vs. Harness

After Phase 6 the agentOS **core** (`agentd` + `agentctl`) is complete. It can run
capability-scoped agents with memory, observation, sandbox enforcement, and a full
operator interface. The test for whether something belongs in the core: *does every
agent need it, regardless of what it does?*

From here two tracks diverge:

- **Core** — small, additive changes that belong inside `agentd`/`agentctl` because
  they are protocol-level or infrastructure-level, not capability-level. These keep
  the binary at the existing size budget (≤ 6 MB CI guard).
- **Harness** (`agentos-std`, shipped as `agentos:full`) — the standard library of MCP
  servers, sidecars, and configurations that operators compose from. Written in any
  language. Versioned independently of the runtime. Never compiled into `agentd`.
  Delivered as a Docker image layer on top of `agentos:core`.

The boundary is enforced mechanically: anything that would grow the `agentd` binary
beyond 6 MB belongs in the harness.

**p7.1 — HTTP/SSE MCP transport** ✅ (v0.35.0) [CORE]
Client-side Streamable HTTP transport (MCP spec 2025-03-26). `McpBackend` trait unifies
stdio and HTTP; `McpHttpClient` with SSE state machine, session-ID capture, bounded-body
streaming, 30 s timeout; `url` + `headers_env` config fields; `https://` enforcement;
`mcp_http_connected` / `mcp_http_error` flight events; `transport` field on
`ServerEnforcement`; `docs/MCP_SERVERS.md` directory. Core rationale: MCP is the tool
ABI; the client (both transports) belongs in the runtime alongside `infer()`. `reqwest`
was already a dependency since p0.2; the `stream` feature added negligible binary delta.

---

## Phase 7 — Core additions

Two small, additive changes to the runtime. Both are protocol or infrastructure concerns
that cannot live in a sidecar without losing the security or abstraction properties that
make them meaningful.

**p7.2 — Streaming inference** [CORE] ✅ done (v0.36.0)
`infer_with_stream()` on `InferenceGateway`; `AnthropicGateway` SSE parser
(`parse_sse_event` + `parse_sse_stream`); `make_infer_future` scheduler helper;
`tokio::join!` dispatch with async stdout, BrokenPipe abort, final newline;
`Arc<Mutex<HashSet>>` side-channel for double-print suppression; `streaming: bool`
on `ModelConfig` and `InferenceRequest`; `InferenceStreamStarted` +
`InferenceStreamCompleted` flight events; 889 tests.
**p7.3 complete (v0.37.0).** FUSE write control surface: `agentd::control` module with
`OperatorSpawnRequest` + `parse_control_command`; `ControlDispatch = Arc<dyn Fn(&[u8]) -> i32 + Send + Sync>`
in `surfaces`; `INO_CONTROL = 15` write-only pseudo-file in `AgentsFs` with per-fh write buffers,
`process_control_flush`, and `MountOption::RO` removed; `with_control()` builder on `Scheduler`
with `default_model_cfg: ModelConfig` for operator-spawned agents; `dispatch_operator_spawn()`
emits `FuseControlReceived` / `FuseControlError`; `SpawnOutcome` enum in `agentctl watch` with
`InjectedViaControl` path (JSON payload, green banner, TUI re-entry) and `FellBackToExec` fallback;
`do_generate()` shows JSON preview when control surface is present; `docs/CONTROL_SURFACE.md`
operator reference; 2 new flight events in CONVENTIONS.md; 894 tests.

**p7.3 — Write-capable FUSE control surface** [CORE] *(superseded by p7.3 complete above)*

**p7.4 — Approval gate (human-in-the-loop primitive)** [CORE] ✅ *complete (v0.38.0)*
`request_approval` native tool parks agents pending operator resolution; `/agents/approvals`
FUSE pseudofile (JSONL, `INO_APPROVALS=16`); `ControlCommand` extended with `Approve`/`Reject`
variants; `AgentStatus::AwaitingApproval`; checkpoint FORMAT_VERSION 2→3; `agentctl watch`
Approvals view (`[a]`); 932 workspace tests. Full spec: `docs/plans/p7.4-approval-gate.md`.

**p7.5 — Egress mediator (governance linchpin) — vertical slice** [CORE] ✅ *complete (v0.39.0)*
Native-tier egress governance: Ed25519 + SHA-256 hash-chained `evidence.jsonl` receipts;
boundary secret rewriting (`ANTHROPIC_API_KEY` → placeholder in agent env at startup);
`EgressProxy` in-process mediator intercepts inference results and calls `EvidenceWriter`;
`agentctl verify` offline chain verifier; Inspector `Egress` filter in `agentctl watch`;
hyper v1 HTTP stub (p7.5b readiness); 4 new flight events (`egress_brokered`,
`egress_denied`, `action_receipt_emitted`, `egress_proxy_failed`); 937 workspace tests.
Universal-tier (netns proxy) deferred to p7.5b after p7.6 isolation floor. Full spec:
`docs/plans/p7.5-egress-mediator.md`.

**p7.5b — Universal-tier HTTP forwarding proxy** [CORE] ✅ *complete (v0.40.0)*
Real HTTP forwarding proxy replacing the `start_http_stub()` 501 stub: `ProxyRegistry`
(`RwLock<HashMap<ephemeral_key, ProxyEntry>>`); per-workload ephemeral key identity in
`x-api-key` header; real `ANTHROPIC_API_KEY` lives only in proxy memory; hop-by-hop header
stripping; 8 MB response cap; 120 s upstream timeout; SSE/streaming requests → 501
(deferred to p7.5c); structured `detail` error field; signed action receipts + flight events;
FUSE `/agents/system/egress_addr` (INO 17); `[egress] proxy_addr` TOML config; fail-closed
bind; RUNBOOK §9 egress proxy section; 960 workspace tests. Full spec:
`docs/plans/p7.5b-universal-tier-proxy.md`.

**▣ p7.6 — Isolation floor (microVM / gVisor) for the universal tier** [CORE] *(v0.41.0)*
prerequisite for hosting untrusted/foreign code)*
The capability layer (Landlock/seccomp/namespaces) is least-privilege on a shared host kernel —
**not** an isolation boundary for untrusted, agent-generated, or foreign-framework code (one
kernel exploit from host compromise). The real floor is a **microVM (Firecracker, dedicated
guest kernel)** or a **user-space kernel (gVisor)**. Native-tier agents can run with the
capability layer alone; the *universal tier* needs this floor underneath the egress netns.
**Couples to observability** (eBPF for native/Firecracker-guest; gVisor remote sink for gVisor —
host eBPF is blind inside gVisor). A dual-backend (gVisor when nested-KVM is unavailable,
Firecracker when hardware isolation is demanded) is the resilient posture. Design context:
`docs/PRODUCT-THESIS.md` security model + `docs/OBSERVABILITY-PLAN.md`.

**obs.1 — flight→OTLP sidecar + GenAI semconv** [HARNESS] ✅ *(v0.42.0)*
Export the existing flight-event stream as OpenTelemetry: run=trace, agent=span, turn/inference/
tool/egress=child spans, tokens/$=metrics, GenAI `gen_ai.*` semconv. Ships as the `agentos-otel`
sidecar (tails `flight.jsonl`; keeps the heavy OTEL deps out of the ≤6 MB core); optional
cargo-feature in-core exporter later. W3C `traceparent` injected at the egress mediator so
hosted foreign workloads join the same trace. The value is interop with standard backends, not
new signal. Full design: `docs/OBSERVABILITY-PLAN.md`.

**obs.2 — OTLP sidecar hardening** [HARNESS] ✅ *(v0.43.0)*
Three deferred items from obs.1 adversarial review: (1) `BatchSpanProcessor` migration —
replaces `with_simple_exporter`; `OTEL_EXPORT_BATCH_DELAY_MS` tunable (default 5 s); SIGTERM
flush via `provider.force_flush()` + `sb.drain_all`; (2) validation unit tests — 8 new tests
for `validate_log_path` + `validate_endpoint` rejection paths; (3) log rotation flush —
`rotated` flag wired to `SpanBuilder::reset_for_rotation()` which drains open spans AND resets
trace context; rotation spans tagged `forced_close=log_rotated`; `flushed_on_rotation` counter
in stats. Known gaps deferred to obs.3: copy-truncate detection miss + backend-down drop
invisibility.

**obs.3 — OTLP sidecar gap remediation** [HARNESS] ✅ *(v0.44.0)*
Closes obs.2-ar-01 (copy-truncate fast-grow false-negative) and obs.2-ar-02 (backend-down drop
invisibility). Gap 1: content sentinel — `FileTailer` stores `last_sentinel: Vec<u8>` (64 bytes
at last-consumed offset); on poll, sentinel window is re-read and compared; mismatch → rotation;
three guards prevent false positives (small file, unpopulated sentinel, u64 underflow); 3 new
unit tests. Gap 2: `export_drops: u64` + `spawn_blocking(move || p.force_flush())` at all three
call sites; final stats line at shutdown; new `agentos.otel.export_drops` OTLP counter (unit
"failures") separate from channel-drop counter. Known gap obs.3-ar-01: `BatchSpanProcessor`
internal 2048-slot queue drops uncounted; mitigate via `OTEL_BSP_MAX_QUEUE_SIZE` env var.

---

## Phase 7 — Standard library (harness)

These ship as MCP server implementations in `agentos-std`, packaged in the `agentos:full`
Docker image. None of them touch `agentd`. Operators attach them via
`[[tools.mcp_servers]]` with explicit capability grants. Each is a small, independently-
versioned process in any language.

**h7.1 — Standard MCP servers** [HARNESS] ✅ v0.45.0
Three first-party MCP servers that make the existing template catalogue useful without
operator setup:
- `shell_exec` — runs shell commands, returns stdout/stderr/exit code; requires a new
  `ShellExec` capability (deny-by-default); sandbox applies `DenySpawn` + file grants
  derived from the agent's `FsRead`/`FsWrite` caps.
- `http_fetch` — fetches any HTTPS URL, returns a bounded body; requires `Net`; Landlock
  V4 port rules from p4.6 apply.
- `web_search` — thin wrapper over a search API (Brave Search or a local SearXNG
  sidecar); `Net`-gated.
These servers follow the pattern established by `docker/weather_mcp.py`: small, stdlib-
only implementations of the MCP JSON-RPC protocol over stdio.

**h7.2 — OAuth MCP sidecar** [HARNESS] ✓ done (v0.46.0)
Handles OAuth2 authorization-code flow with a local callback server; stores tokens in
the system keychain (read from env at agent startup, preserving the secrets-from-env
invariant); presents authenticated HTTP calls as MCP tools. Unlocks Gmail, Google Drive,
Calendar, and other OAuth-gated services. `agentd` sees it as any other MCP server — no
core changes.

**h7.3 — Event trigger MCP servers** [HARNESS] ✅ v0.47.0
Makes the `watcher` template (previously `gated_requires = "event-triggers"`) fully
operational. Three poll-and-retry MCP servers (`cron_mcp.py`, `fs_watch_mcp.py`,
`webhook_mcp.py`) each expose `wait_for_trigger()`. Agent loops calling it until
`status == "fired"`. Constraint: MCP_TIMEOUT=30s prevents true blocking; servers return
within 25s with "waiting"|"fired"|"timeout". Two new templates (`cron-agent`,
`webhook-agent`); watcher `gated_requires` removed. 26 autoplan decisions (all
mechanical), 18 self-tests (6 per server), 1027 workspace tests.

---

## Chief of Staff — the flagship workload  ← **NEXT (gate before Phase 8)**

The substrate is complete (core + observability + full h7.x harness) but unproven against a
real workload, and the wedge is still n=1 conviction. The Chief of Staff is the flagship that
proves both. Build the thin vertical slice, get it *used* on a real inbox, **then** Phase 8.

**cos.1 — Daily Operating Brief (vertical slice)** [HARNESS / flagship] *(done — v0.48.0)*
An always-on, cron-triggered Chief of Staff that produces a Daily Operating Brief from Gmail,
read-only (autonomy L0), with the full trust story *demonstrable*: the agent never holds the
OAuth token (it lives in the `google_oauth` sidecar), egress is confined to Gmail + the model
gateway (off-domain → `egress_denied`), every step is in OTLP + the signed `evidence.jsonl`
(verifies offline), and any send (opt-in L1) routes through `request_approval`. It is a
**composition** of shipped pieces — cron (h7.3) + OAuth (h7.2) + scheduler/spawn (P1) + KB
(P5) + approval gate (p7.4) + egress/receipts (p7.5) + gVisor floor (p7.6) + OTLP (obs.1–3) —
not new infrastructure: 3 subagents (orchestrator/inbox/curator), one workflow, one system.
**Real acceptance: customer-zero runs it on their own inbox and it's useful.** Full spec:
`docs/plans/cos.1-chief-of-staff-slice.md` (= `HARNESS-OPS-PLAN.md` Phase O1).

**con.1 — TCP keepalive + transport retry** [CORE] *(done — v0.49.0)*
Fixes Docker NAT conntrack silently dropping idle Anthropic API connections during long MCP
waits in multi-turn cos.1 runs. `tcp_keepalive(15s)` on the reqwest client keeps conntrack
alive; `send_once()` + `is_connect()` retry retries once on stale-pool connect errors
(non-streaming only — streaming retry is unsafe). `InferenceTransportRetried` flight event.
OTEL coverage guard (`otel/tests/event_kind_coverage.rs`) updated. Removes the `streaming =
false` stopgap previously patched into the cos Docker entrypoint.

**h7.4 — Streaming-by-default + connect timeout** [HARNESS] *(done — v0.51.0)*
Fixes Docker agent silent hang and `google-agent` OAuth URL invisibility. `ModelConfig.streaming`
default flipped to `true` (`fn default_streaming() -> bool { true }` +
`#[serde(default = "default_streaming")]`); `Default` impl updated. `connect_timeout(10s)` added
to the `AnthropicGateway` reqwest client. `infer_with_stream` gains `is_connect` retry on stale-
pool connect errors (streaming path, parallel to the non-streaming retry in con.1).
`InferenceTransportRetried` emitted from the streaming path. 4 new streaming-default tests
(defaults_to_true, can_be_disabled, can_be_enabled, default_impl_streaming_is_true);
1030 workspace tests.

---

## Platform DX — Deployment experience + consistent operator surface

**Why this exists:** cos.1 proved the runtime works. The deployment story — daily
use on Mac, production on Linux, observable from outside the container — still has
sharp edges. These increments close the gap before Phase 8 harness extensions, which
assume a stable operator experience. Three architectural decisions drive the work:

1. `agentd` serves an HTTP management API on `:7999` — the single surface that
   works identically on Mac (Docker port binding) and Linux (QEMU hostfwd or local).
2. `~/.agentos-secrets/` is the host-side secrets directory — mounted read-only into
   the container or VM; no OAuth or API key env vars needed at runtime.
3. The FUSE surface stays as an internal fast path; `agentctl` becomes a thin HTTP
   client that doesn't depend on filesystem access.

**dx.1 — Mac Docker DX: secrets model + agentctl auth** *(done — v0.52.0)*
Brings the CoS harness to `docker compose up -d cos` with no OAuth env var boilerplate.

Scope:
- `docker/oauth_mcp.py`: read `/run/secrets/google.json` first; fall back to
  `OAUTH_REFRESH_TOKEN` env var for backward compat. Hardcode Google OAuth URLs
  (`OAUTH_AUTH_URL`, `OAUTH_TOKEN_URL`, `OAUTH_SCOPES`, `OAUTH_ALLOWED_HOSTS`,
  `OAUTH_PROVIDER_NAME`) as module-level defaults — remove them from user-facing config.
- `docker-compose.yml`: add `~/.agentos-secrets:/run/secrets:ro` volume bind; expose
  ports `7999:7999` and `8080:8080`; remove the 5 hardcoded Google URL env vars +
  `OAUTH_REFRESH_TOKEN` from the environment block.
- `agentctl auth google` subcommand: runs OAuth PKCE flow on the host (local callback
  server on port 8585), writes `~/.agentos-secrets/google.json` (mode 0600). This is
  the one-time provisioning step — the browser dance moves to the host, outside any
  container.
- `agentctl auth` guards: clear error if `~/.agentos-secrets/` doesn't exist (mkdir
  prompt); re-auth if token file already present (overwrite with confirmation).

Acceptance:
- `agentctl auth google` completes the PKCE flow and writes the token file.
- `docker compose up -d cos` starts the CoS harness with no OAuth env vars in the
  shell — only `ANTHROPIC_API_KEY` + `OAUTH_CLIENT_ID` + `OAUTH_CLIENT_SECRET`.
- `docker compose logs -f cos` shows the cron agent waking and running the brief.
- Existing `OAUTH_REFRESH_TOKEN` env var bypass still works (backward compat).

**p7.7 — Management HTTP API** *(done — v0.53.0)*
The structural change that makes `agentctl watch` work identically on Mac and Linux,
and surfaces deep observability without filesystem access.

Scope:
- `agentd/src/management.rs` (new): `ManagementServer` binds `0.0.0.0:7999` (port
  configurable via `[management] port` in agent TOML, disabled by default, enabled
  when `[management] enabled = true`). Uses `tokio` + `hyper` (already a transitive
  dep via `reqwest`).
- Routes:
  - `GET /api/v1/agents` → JSON array of `AgentSnapshot` (same struct as FUSE surface).
  - `GET /api/v1/agents/:id` → single `AgentSnapshot` or 404.
  - `GET /api/v1/stream` → SSE stream; every `FlightRecorder::emit()` call fans out to
    all connected SSE subscribers via a `tokio::sync::broadcast` channel (capacity 1024).
    Events are the same JSONL structs written to disk, serialized as `data: <json>\n\n`.
  - `GET /api/v1/approvals` → pending approvals from `ApprovalStore`.
  - `GET /api/v1/memory/:ns` → memory entries for namespace (proxies `MemoryStore`).
- `agentctl`: add `--url` flag (default `http://localhost:7999`); auto-detect mode —
  if `/agents/` FUSE mount is readable, use FUSE (fast path); else use HTTP API. Env
  var `AGENTCTL_URL` overrides. All existing `agentctl watch` views (Dashboard,
  AgentDetail, Topology, Memory, Approvals, Inspector) work via HTTP API.
- `ManagementStarted` + `ManagementRequest` flight events.
- New tests: management server binds, routes return correct JSON, SSE fan-out delivers
  events to two concurrent subscribers, FUSE fallback to HTTP in agentctl.

Acceptance:
- `agentctl watch --url http://localhost:7999` on the Mac host (outside Docker) shows
  the same Dashboard view as running `agentctl watch` inside the container.
- Inspector view streams live flight events over SSE with no polling lag.
- Killing the management server (port unavailable) → agentctl falls back to FUSE with
  a `[warn] management API unreachable, using FUSE` message.
- Binary stays ≤ 6 MB (hyper already present; no new heavy deps).

**~~dx.2~~ — HTTP approval surface** *(done — v0.54.0)*
POST approve/deny routes on the management API (:7999); fail-closed (503+Retry-After on
channel full); 404 on unknown ID; `ApprovalHttpApproved`/`ApprovalHttpDenied` flight events;
`DataSource` trait extended with `load_approvals()`/`approve()`/`deny()`; `HttpSource` with
500 ms mutation timeout; `agentctl approve`/`agentctl deny` CLI subcommands; optimistic local
removal in TUI; `status_detail` parsed from HTTP snapshot JSON; FUSE control channel always
wired on Linux. Resolved p7.7-ar-01, p7.7-ar-02, p7.7-ar-04. 1096 workspace tests.

~~**dx.3 — Linux QEMU production**~~ ✅ *(v0.69.0 — `distro/buildroot.config`: Python3+OpenSSL; `distro/Makefile`: RUN_NETDEV with loopback hostfwd:7999/8080, Python overlay target, clean fix; `distro/overlay/init`: kernel cmdline `agentd.config=` config selection; `distro/overlay/etc/agentd/cos.agents.toml`: QEMU-mode CoS config (bind_addr=0.0.0.0); `agentd/cos.agents.toml`: [management] enabled=true; `distro/agentos-cos.service`: systemd unit (User=agentos, loopback hostfwd, ExecStartPre mkdir); `docs/DEPLOYMENT.md`: two-page operator guide with complete agentos.env template and SSH tunnel instructions)*

---

## Planned — multi-arch reach + multi-instance coordination

Design + `/autoplan`-ready increments live in **`docs/DEPLOYMENT-TOPOLOGY.md`**; fold them in here
when picked up. Two tracks — substrate reach, not the flagship (the CoS harness stays that):

- **Track MA (multi-arch):** ~~`ma.1` aarch64 binary target~~ ✅ *(v0.55.0 — cross+QEMU CI, Cross.toml, size guard, make clippy-aarch64)* · ~~`ma.2` arm64 distro + `qemu-system-aarch64 -accel hvf` boot~~ ✅ *(v0.57.0 — aarch64 Buildroot config, PL011 UART, HVF/KVM/TCG auto-detect, ARCH= Makefile, distro-aarch64 CI dry-run)* · ~~`ma.3` multi-arch container images~~ ✅ *(v0.56.0 — ghcr.io linux/amd64+linux/arm64 manifest, QEMU buildx, GHA cache, gated on both Rust CI jobs)* · ~~`ma.4` isolation-tier detection + honest per-device reporting~~ ✅ *(v0.67.0 — probe() in isolation_caps.rs, IsolationCapsSummary in surfaces, INO_SYS_ISOLATION FUSE file, isolation_probed flight event, agentctl watch System view color-coded tier)* **Decide multi-arch before dx.3/dx.4 freeze
  x86-only** — parameterize the deployment by `$ARCH` from the start rather than retrofit.
- **Track MESH (multi-instance):** `mesh.1` instance registry (on p7.7) · `mesh.2` federated A2A across
  instances · `mesh.3` shared memory sidecar (on h8.1 — compute/memory separation) · `mesh.4`
  `agentctl mesh` (lightweight "my-mesh" coordinator) · `mesh.5` agent migration (= h8.3) · `mesh.6`
  multi-tenant control plane (deferred/enterprise).

Guardrails (from `DEPLOYMENT-TOPOLOGY.md` §3): arch/hypervisor never leak into the core; state the
isolation tier per device (breadth must not outrun trust); every arch's boot is CI-tested or it rots.

**Naming:** `mesh.*` is *multi-instance* coordination (above). The single word "orchestrator" now
denotes the *intra-instance* interactive dispatcher — **`orch.1`** (conversational follow-ups via
p7.3 `inject`; see `TODOS.md` and the build order). Formerly mis-filed as `h8.3`; ROADMAP's `h8.3`
stays multi-device migration.

### ✓ orch.2 — Orchestrator hardening (v0.70.0)
**Depends on:** orch.1.
**Goal:** close 6 orch.1 action remediations + 2 pre-conditions discovered during review.
**Scope:**
- **ar-01** (checkpoint): orchestrated waiting agents checkpointed with `terminal=true`; `from_checkpoint` restores actual terminal flag; seed loop guard prevents immediate deletion; `waiting_agents`/`orchestrated_agents` in `SchedulerCheckpoint` (FORMAT_VERSION 4).
- **ar-02** (spawn confirmation): oneshot channel from management API to scheduler; `POST /api/v1/spawn` returns 201 + `{"agent_id":"..."}` after confirmed insertion (2 s timeout → 503).
- **ar-03** (answer cap): `OrchestratorTurnComplete.answer` capped at 512 chars with inline `[output truncated — full text streamed above]` note.
- **ar-05** (state split): `state.waiting` split into `orchestrated: HashSet<String>` (persistent membership) + `waiting: HashSet<String>` (currently parked); `handle_agent_terminal` consolidates both removals (eliminates C2 phantom-entry leak).
- **ar-06** (SSE keepalive): 30 s `": ping"` SSE comment from management server; `agentctl orchestrate` gets improved timeout error message with resume command.
- **ar-07** (quit/exit): `agentctl orchestrate` checks for `quit`/`exit` input before inject and prints session-pause message with resume command.
- **audit-O1**: 3 event-trigger templates (`cron-agent`, `watcher`, `webhook-agent`) gain `mcp` capability grant (deny-by-default was hiding all tools silently).
- **audit-C3** (fsync durability): `write_mode_600` adds `sync_all()` after flush; `CheckpointStore::save()` fsyncs parent directory after rename.
**Acceptance:** `cargo build && cargo clippy -- -D warnings && cargo test` all green; FORMAT_VERSION 4 checkpoints round-trip; waiting orchestrated agents survive restart.

---

## Track UX — Operator cockpit (Converse · Observe · Spawn)

Turn `agentctl watch` from a read-only dashboard into the surface the operator *drives*. Closes the
reach/usability gap (surfaced by a Hermes Agent comparison) and is the operator half of the CoS
direction. The management API (`:7999`, orch.1/orch.2) already carries the whole backbone — spawn,
inject, SSE — so this is mostly an `agentctl`-client effort. Full plan: **`docs/plans/ux-cockpit.md`**;
build-session prompt: `docs/prompts/09-ux-cockpit.md`.

**Locked decisions (2026-07-10):** (1) *Unified* live cockpit — one screen (k9s agent table + pinned
chat rail + live event stream + input box + `:` palette), not more `[key]` tabs; this needs an
async-loop refactor first (**ux.0**), preserving current behavior (p1.1-style). (2) *Publish
host-loopback* — the Docker `cos` deployment binds management to `0.0.0.0` in-container and publishes
`127.0.0.1:7999:7999`; **agentd default bind stays `127.0.0.1`**.

Backbone: one `tokio::select!` loop, three producers (keys + `/api/v1/events` SSE + ~30 ms render
tick) → one channel; `DataSource` pushes, never `.await` on the render thread.

- ~~**ux.0** — Async single-loop foundation~~ ✅ **shipped (v0.77.0)** — `agentctl watch` refactored to
  a non-blocking event-pushed single loop (Option B: background threads + bounded `sync_channel`, no async).
  Behavior-preserving; pure `step()`; SSE reconnect + reconciliation; livelock/panic guards; 408 tests.
  **Host-loopback reachability split to ux.0b** (`docs/plans/ux.0b-host-loopback-reachability.md`) — it hit
  the `management.rs` fail-closed loopback guard + unauthenticated-API Docker-bridge exposure (needs a
  security decision).
- ~~**ux.0b** — Host-loopback reachability~~ ✅ **shipped** — Option A (gated override):
  `[management] allow_non_loopback` opt-in (default false) relaxes the fail-closed loopback guard;
  `agentd/cos.agents.toml` + the QEMU overlay set `bind_addr = "0.0.0.0"` + `allow_non_loopback = true`
  (also fixes the pre-existing QEMU management-API-refuses-to-start conflict); `docker-compose.yml`
  publishes `127.0.0.1:7999:7999` (never bare `7999`), with `cos`/`agent` split onto separate Compose
  networks (`cos-net`/`agent-net`) so `agent`'s untrusted/web-fetching templates can't reach `cos`'s
  unauthenticated management API on the bridge (ux.0b-ar-01, fixed same PR after convergent ship-stage
  adversarial review); `docs/DEPLOYMENT.md` + `docs/RUNBOOK.md` now use
  `agentctl watch --url http://localhost:7999` directly from the Mac host, no `docker exec` workaround;
  THREAT_MODEL.md §9 documents the unauthenticated-API exposure this accepts under the single-tenant
  lock, deferring per-session auth to **ux.5** and the `allow_non_loopback` unscoped-bypass gap to
  `ux.0b-ar-02` (TODOS.md).
- **ux.2** — Observe (closes **cos-ux-01**): `last_activity`/`last_error`/`idle_secs` on the snapshot;
  agent-table `LAST-TOOL` + row-red-on-error + `idle→amber` stuck signal; live summary-first event
  stream (JSON on expand, filter chips, freeze, row-scopes-stream); AgentDetail timeline.
- **ux.1** — Converse: fold `orchestrate.rs` into the cockpit as the chat rail (`[c]`); streaming
  green; retarget any agent via the selected row; `follow`/`▼ N new`; inline errors, never hang.
- **ux.3** — Spawn custom on the fly (closes **p7.3-ar-02**): repoint the Spawn view from
  exec-a-2nd-agentd to `POST /api/v1/spawn` into the running instance; `⟨custom⟩` mode (deny-by-default
  caps + tool/connector select); modal-over-live-dashboard; preview before launch; auto-drop into the
  new agent; `:` command palette.

**Cathedral expansions (accepted 2026-07-10, CEO review — SCOPE EXPANSION):** the "CoS you live with."
- **ux.4** — Proactive push: SSE sink → local notifier + *optional* signed webhook to one operator-owned
  endpoint (approval/error/brief/skill events). New **outbound egress** → routes through the credential
  broker (cred.3) + THREAT_MODEL note; deny-by-default. `/plan-eng-review` (security-sensitive).
- **ux.6** — Evidence view: surface the signed Ed25519 receipt chain (`evidence.jsonl`) + inline
  `agentctl verify` + per-agent "chain verified" badge. Provable accountability.
- **ux.5** — Local web cockpit: self-contained host-loopback SPA over the management API/SSE (same
  converse/observe/spawn surface in a browser; still single-tenant, still loopback). ⚠ Browser ≠ FUSE
  boundary — needs an Origin/Host allowlist (DNS-rebinding guard) to land first.
- **ux.7** — Run replay: reconstruct + scrub an agent's run from `flight.jsonl` + checkpoints.
- **ux.8** — Live budget control (added 2026-07-11): a cockpit panel to view + set per-agent and global
  token budgets over a new management-API budget endpoint. Fixes the live-run "500k too small" finding.
  `/plan-eng-review` (live config writes).
- ~~**ux.9** — Cockpit mode~~ ✅ **shipped (v0.82.0)** — `docker/entrypoint.sh`'s `cockpit)` case:
  the new zero-arg Docker default, cold-starts `agentd` with a zero-agent config
  (`docker/cockpit.toml`) and attaches `agentctl watch` (non-exec'd, preserving signal handling).
  FUSE preferred when `--privileged`; transparently falls back to the management API over HTTP
  otherwise (`agentctl watch`'s existing `detect_source`). Two critical bugs found and fixed
  during `/review`: checkpoint bleed-through from a stale `/workspace/checkpoint.json` (now runs
  from `/data`, matching `cos)`/`agent)`), and terminal corruption on `docker stop` (agentctl
  gained its own SIGTERM/SIGINT handler). Full plan: `docs/plans/ux.9-cockpit-mode.md`.

> **North star (2026-07-11):** the cockpit is **agentos's default operator surface** — an always-on
> status/debug/control console (k9s/htop for agents), not an optional tool. ux.9 makes it the default;
> ux.0/2/1/8 make it live, watchable, chattable, tunable.

Sequencing (updated 2026-07-11 — cockpit-as-default): core **ux.0 → ux.9 (boot into TUI) → ux.2 (activity)
→ ux.1 (chat — *bumped*) → ux.8 (budgets) → ux.3**, then expansions **ux.6 → ux.4 → ux.5 → ux.7**, then
**skills (Phase 11) last**. One increment per branch, `main` shippable at each step. **Parallel, independent
of the cockpit — do first (makes the CoS usable today):** `cos-polish` (`docs/plans/cos-polish.md` — the
8 bugs from live testing: brief-not-written, KB-unfindable, orchestrate errors, undersized budgets) and
`memory-routing` (`docs/plans/memory-routing.md` — raw emails → harness Layer 2, also fixes the token
blowup). **Connectors** stays a parallel unwritten track (Calendar/GitHub/Linear/Slack/Notion via the
credential broker); it must not block the cockpit path.

---

## Phase 8 — Harness extensions

**h8.1 — Layer 2 semantic memory** [HARNESS] ✅ *v0.64.0 — complete*
`docker/semantic_kb_mcp.py` HTTP MCP sidecar (port 8020) backed by Qdrant (vector store)
+ Voyage AI embeddings (`voyage-3-lite` default, 512-dim). Exposes `kb_put` / `kb_get` /
`kb_search` with the same interface as the Layer-1 BM25 tools (p5.5); `tool_override = true`
lets agents upgrade to vector search without changing task prompts. `allow_insecure_local = true`
permits Docker-internal `http://` URLs. `templates/librarian-semantic.template.toml` + Compose
`--profile semantic` for zero-config start. 7 self-tests; SSRF guard on `QDRANT_URL`.
(Note: HelixDB was evaluated but uses a graph-DSL API incompatible with the sidecar model;
Qdrant selected for its simple REST vector API.)

**h8.2 — `agentos:full` Docker distribution** [HARNESS] ✅ v0.73.0
Formally packages the harness into a versioned Docker image pair. `agentos:core` contains
only `agentd` + `agentctl` (the existing `Dockerfile`). `agentos:full` extends it with
all standard MCP servers (h7.1), the OAuth sidecar (h7.2), event trigger servers (h7.3),
Qdrant/semantic-kb (h8.1), and the full template catalogue. Operators choose the image tier for
their use case; the core runtime is identical in both.

**h8.3 — Multi-device agent migration** [HARNESS, horizon]
Serialize a running agent's full state (checkpoint + memory volume + config) into a
portable artifact and restore it on another device. The checkpoint format (p3.2) and
detachable memory volume (p5.3.5) provide the groundwork; the remaining work is a
transfer protocol and identity continuity. Delivered as a command in `agentctl` — not a
core runtime change. No plan doc yet; revisit when a concrete use case demands it.

---

## Track PERSONAL — Operator workflow brain

The **operator memory** complement to the agent memory layers. While Layer 1 (BM25) and
Layer 2 (vector, h8.1) are agent-owned — agents write during task execution — the personal
track gives the *operator* a persistent semantic brain that spans projects, deployments, and
sessions. Agents can read it (as a `canon` MCP source) but the operator controls what goes in.

Architecture: **gbrain** (`github.com/garrytan/gbrain`) as the backend, exposed to agentOS
agents via its MCP server (`gbrain serve` / remote HTTP transport). gbrain handles PGLite or
Supabase storage, Voyage AI `voyage-code-3` embeddings, and cross-machine sync via a private
artifacts repo — no need to rebuild this layer.

Layer mapping:

```
Layer 1: in-process BM25 (per-agent session, native)
Layer 2: Qdrant sidecar (persistent, per-deployment, agent write/read)   — h8.1
Layer 3: gbrain (persistent, cross-deployment, operator-owned)           — personal.x
```

**personal.1 — gbrain MCP integration** [HARNESS] *(depends on: h8.1)*
Wire gbrain into the agentOS Docker stack as an optional operator KB layer. Agents that hold
`KbRead`-equivalent capability for the `personal` source can query it via `mcp__gbrain__search`
/ `mcp__gbrain__query`. The operator runs `/setup-gbrain` once on the Mac host; the Docker
stack mounts the gbrain socket or connects to a Supabase URL. A new `researcher.template.toml`
demonstrates agents that combine Layer 2 runtime KB writes with Layer 3 operator knowledge
lookups. No `agentd` Rust changes needed; pure HARNESS + template + docs work.
No plan doc yet.

---

## Phase 9 — Kernel observability (eBPF) [CORE, privileged]

The **observe** complement to the sandbox's **enforce**, and the syscall-level ground truth
that the flight recorder (records what agentd *chose* to) and the egress proxy (sees brokered
calls) cannot. Closes the universal-tier audit gap: even a TLS-pinning foreign agent's *actual*
file/network/syscall behavior is observed. Note: p3.3's "eBPF/LSM" deliberately used Landlock+
seccomp — there is **no eBPF code yet**; this is a new subsystem. Lift: `aya` (pure-Rust eBPF),
kernel BTF/CO-RE + `CONFIG_BPF*`, elevated privilege (`CAP_BPF`/`CAP_SYS_ADMIN` — the observer
outranks the agents it watches), Linux-gated, kernel-version floor. Tractable on the appliance
(controlled kernel); degrades "run anywhere." Full design: `docs/OBSERVABILITY-PLAN.md`.

- **ebpf.1** — aya scaffold + capability + kernel-config + a single per-child-PID syscall-trace probe.
- **ebpf.2** — network + file-access probes.
- **ebpf.3** — perf / latency.
- **ebpf.4** — surface integration: kernel events as flight/OTEL span-events; `/agents/<id>/syscalls`.
- **ebpf.5** — policy-violation detection (eBPF sees an action Landlock should have blocked → alert).
- **sink.1** — gVisor remote-sink listener (`seccheck.Sink`: ingest + decode the Sentry's
  protobuf syscall stream over a Unix socket). Required for gVisor-isolated workloads, where
  **host eBPF is blind**. The kernel-observability mechanism is conditional on the p7.6 floor:
  eBPF for native/Firecracker-guest, the sink for gVisor — pick per the chosen floor.

(Sequencing across Phases 7-9: **p7.5 egress mediator → obs.1 OTLP → Phase 9 eBPF**. p7.5 is the
prerequisite — you can't observe what you don't broker. See `docs/PRODUCT-THESIS.md` for why
observability is ~half the product.)

---

## Phase 10 — Credential manager

Goal: give AgentOS **one credential model across all surfaces**. Today every MCP server reads
its own secrets file or env, and the three surfaces (Docker `agent`, Docker `cos`, QEMU boot)
handle secrets differently. This phase makes one in-process broker own provisioning, the OAuth
lifecycle, per-agent capability scoping, and audit, so tools become credential-agnostic —
generalizing the `EgressProxy` model-key broker (p7.5b) to *all* credentials. Full design:
`docs/plans/credential-manager.md`. **Subsumes dx.5** (`cred.1` + `cred.2`). Decisions: broker is
**in-process** (extends `EgressProxy`), not a sidecar; the "MCP gateway" is an **authenticating
egress proxy** so tools never hold raw credentials; two-tier storage (`~/.agentos-secrets` `:ro`
provisioning + `/run/state/oauth` writable cache). `cred.1` is near-term (the Mac unblock) despite
the phase number; `cred.3+` gate on `/plan-eng-review`.

### ✓ cred.1 — Immediate unblock (secrets mount + README fix) [v0.58.0]
**Depends on:** nothing (ship first — the "test it today" increment).
**Goal:** the google-agent runs on a clean Apple-Silicon Mac via host-auth.
**Scope:** mount `${HOME}/.agentos-secrets:/run/secrets:ro` into the Docker `agent` service
(mirror `cos`); fix the false `README.md` "container never sees your OAuth client credentials"
claim; add `mkdir -p ~/.agentos-secrets` guidance + a fail-fast preflight when
`/run/secrets/google.json` is absent for OAuth templates.
**Acceptance:** clean Apple-Silicon Mac runs `docker compose` scout **and** google-agent via
`agentctl auth google`, no manual patching.

### ✓ cred.2 — Unified secrets substrate [v0.59.0]
**Depends on:** cred.1.
**Goal:** one host-OS-neutral credentials story across all three surfaces.
**Scope:** Docker entrypoint sources `/run/secrets/agentos.env` if present (guarded, before
`check_api_key`, no clobber of compose env) to match QEMU's `ANTHROPIC_API_KEY` channel; make the
QEMU 9p secrets mount read-only; deprecate + gate in-container OAuth (strip `OAUTH_*` from the
`agent` compose block; deprecation notice; record the `0.0.0.0` bind in `THREAT_MODEL.md`);
rewrite the stale `RUNBOOK.md` (v0.20.0) with one "Credentials & first run" section; de-Mac the
strings.
**Acceptance:** the one story works on Docker `agent`, Docker `cos`, and QEMU; no misleading docs.
Tests: writer/reader schema-drift guard, entrypoint DRY_RUN smoke, `agentos.env` source-safety,
`macos-latest` CI build for `agentctl`.

### ✓ cred.3 — Broker core (the credential manager) [v0.60.0]
**Depends on:** cred.2 + `/plan-eng-review` on the plan's open questions.
**Goal:** one in-process broker owns provisioning, OAuth lifecycle, scoping, and audit.
**Scope:** `CredentialBroker` in `agentd/src/egress.rs` (extends `EgressProxy`): provisioning
ingest; OAuth refresh/rotation → `/run/state/oauth`; `CredentialStore` trait (file backend);
`Capability::Credential { provider }` + enforcement + audit events; **inject-at-spawn** —
agentd hands each MCP server only the credentials that agent's capabilities allow, replacing
per-server file reads and the ad-hoc `passenv`/`extra_env` path in `agentd/src/tools/mcp.rs`.
**Acceptance:** MCP servers get scoped credentials via the broker with audit; a capability-denied
agent cannot obtain a provider credential. Broker unit tests + inject-at-spawn integration test.

### ✓ cred.3.1 — Broker hardening gate (v0.61.0)
**Depends on:** cred.3.
**Goal:** close 10 security gaps found in the cred.3 audit before any code consumes the broker.
**Scope:** ar-04 (SSRF DNS check on `upstream_base`); ar-06 (OAuth state loaded from disk on
startup so token survives daemon restarts); ar-07 (deny-by-default fast path for empty
`allowed_providers`); ar-08 (header allow-list replacing deny-list); ar-09 (doc: cred service
is orch.1 prerequisite); ar-10 (shared `LoopbackForwardingProxy` so egress and credential
clients can't drift); S1 (OV-1 startup guard: signing key path must not fall inside any MCP
FsRead sandbox prefix); S2 (remove `content_audited: true` lie from `EgressBrokered` events);
S3 (de-claim `SecretRewriter` from CLAUDE.md/THREAT_MODEL — never built); THREAT_MODEL §8.6–8.7
(universal-tier has no credential path; egress content audit is NOT implemented).
Every gate item: fix + a test that fails without it + adversarial verification.
**Acceptance:** all 10 items closed; `cargo test` green; clippy clean; docs true.

### ✓ cred.4 — Egress gateway (the "MCP of MCP servers") [v0.63.0 spend caps + cred.4b credential-agnostic MCP]
**Depends on:** cred.3 + cred.3.1 (hardening gate).
**Goal:** tools never hold raw credentials.
**Scope:** authenticating egress proxy — tools call upstream APIs through the broker
unauthenticated, the broker attaches the credential and forwards; rewrite `oauth_mcp.py` /
`http_mcp.py` / `search_mcp.py` to be credential-agnostic. Generalizes p7.5 boundary rewriting +
p7.5b forwarding proxy to all providers.
**Acceptance:** a tool process holds no raw credential in env or memory-at-rest; outbound calls
are authenticated at the broker; a denied provider is blocked.

### ✓ cred.5 — Surfacing + hardening (optional)
**Depends on:** cred.3.
**Goal:** operator visibility + lifecycle polish.
**Scope:** `/agents/credentials` FUSE view + `agentctl` credential pane (per-agent provider
grants, last access, token expiry); rotation policy; alternate `CredentialStore` backends (OS
keychain / vault).
**Acceptance:** operator sees per-agent credential grants + last access via FUSE and `agentctl`;
rotation policy configurable.

### ✓ cred.6 — Migrate the CoS to broker mode (close Phase 10 for the flagship) [v0.83.0]
**Depends on:** cred.3–cred.5 (broker infra, all shipped) + cred.4b (broker-capable sidecars).
**Why:** Phase 10's goal — "one credential model, tools hold no raw credential in memory-at-rest" —
is **half-delivered**: the broker exists but the flagship CoS still runs the legacy file path
(sidecar reads `/run/secrets/google.json` directly, `OAUTH_PROVIDER_NAME` + `FsRead /run/secrets`).
This migrates it. Mostly **config** (add `[credential_gateway]` + a `Credential{Google}` grant to
both `cos.agents.toml` files) + an **end-to-end auth retest gate** (do not re-break v0.73.2 auth).
Do-first P0 (ships independently): **secret-redaction** of token-endpoint bodies (`oauth_mcp.py:430`,
`credential/mod.rs:341,371`; folds cred.5-ar-01). Full plan: `docs/plans/cred.6-broker-migration.md`.
**Acceptance:** the CoS authenticates + reads Gmail through the broker; the sidecar process holds no
raw refresh token; auth retest passes; no token/secret in any log/event.

### ✓ cred.7 — Credential resilience (refresh/failure recovery, on top of broker mode) [v0.84.0]
**Depends on:** cred.6 (broker migration).
**Goal:** terminal-failure detection + operator surfacing + resume-without-restart + multi-agent
dedup, provider-agnostic in the gateway. Hardened by `/autoplan` (2026-07-10). Full plan:
`docs/plans/cred.7-credential-resilience.md`. (Was cred.6 before the 2026-07-11 CEO review split the
broker migration out as its own prerequisite increment.)
**Acceptance:** a revoked/expired token → one classified attention approval + `CredentialAttentionRequired`
event; operator fixes the credential on the host → gateway picks it up with no restart → `CredentialRecovered`;
transient blips retry without false alarms; multi-agent → one approval not N.

---

## Phase 11 — Skills subsystem (procedural knowledge, governed)

The missing **procedural layer**: a skill is a packaged, portable *recipe* an agent loads on demand
and executes *using its tools* — distinct from capabilities (permission), tools (actions), templates
(identity), and memory (knowledge). AgentOS's edge is that skill execution is **capability-scoped,
sandboxed, and flight-recorded** — a governed skills *host*, not a trust-by-default runtime. Full plan:
**`docs/plans/skills-subsystem.md`**; build-session prompt: `docs/prompts/10-skills-subsystem.md`.

**Layer:** Capability = *may I act* · Tool(MCP) = *what can I do* · Template = *who am I* · Memory =
*what I know* · **Skill = *how do I do this task***. Skills sit above tools, orthogonal to templates
(one agent, many skills).

**⚠ Naming collision:** `AgentCard.skills` (`config.rs:450`) already exists but is free-form A2A
advertising *tags*, NOT loadable procedures — leave it as-is. The new concept is `Capability::Skill` +
a skills catalogue.

**Locked decisions (2026-07-10):** (1) Anthropic Agent Skills `SKILL.md` format (interop over control,
same reasoning as MCP-is-the-ABI); (2) deny-by-default access via `Capability::Skill { name }`,
sub-agents get a *subset* of the parent's grants; (3) governed execution — skill scripts run only via
`ShellExec` under the sandbox + flight recorder, never exceeding the loading agent's capability
envelope; (4) synthesized skills are quarantined until operator-approved.

- **skill.1** — Catalogue + discovery/load + `Capability::Skill` (substrate; instruction-only skills).
  New `agentd/src/skill.rs` mirroring `template.rs`; `skill_list` (granted, name+desc only) +
  `skill_load` (body, cap-gated); `SkillListed`/`SkillLoaded` events; 1–2 example skills.
- **skill.2** — Sandboxed script execution + resources. Script-bearing skills require `ShellExec` and
  run through the existing sandbox path (no new exec path); resources readable only via a skill-scoped
  FS cap; large bodies page via `memory/context.rs`; THREAT_MODEL §. `/plan-eng-review` this one.
- **skill.3** — Skill synthesis from experience (governed, weightless — no RL/weights). Extend
  `distill_on_complete` (p5.6) to infer a candidate `SKILL.md` on a successful run (off by default),
  **quarantined until operator-approved** via the approvals surface (p7.4). Ties to Track PERSONAL for
  where approved skills live. `SkillSynthesized`/`Approved`/`Rejected` events.

Sequencing: **skill.1 → skill.2 → skill.3**, one increment per branch, `main` shippable at each step.
skill.1 alone is useful (a CoS triage recipe); skill.3 is the optional, most speculative tier — build
it only after skill.1/2 prove out in the CoS. The UX cockpit *surfaces* skills (spawn-with-skills,
loaded-skill in the activity view); it does not own them.
