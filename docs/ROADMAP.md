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

### ▢ p0.5 — Real MCP stdio client
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

### ▢ p1.1 — Agent as a sans-IO state machine
**Depends on:** p0.5
**Goal:** Make the loop drivable one step at a time so a scheduler can own it and
interpose on every inference call. The agent should *describe* what it needs next,
not perform IO itself.
**Scope:** `agentd/src/agent.rs` (refactor; consider splitting into `agentd/src/agent/`).
- Introduce `AgentTask`: the agent config + working context (`Vec<Msg>`) + token
  ledger + turn counter + status.
- `step(&mut self) -> AgentEffect`, where `AgentEffect ∈ { Infer(InferenceRequest),
  CallTools(Vec<ToolUse>), Completed(String), Failed(String) }`.
- `provide_inference(InferenceResponse)` and `provide_tool_results(Vec<Block>)`
  feed results back in.
- Keep a thin `run()` driver that performs the IO inline so Phase 0 behavior is
  **byte-for-byte identical** in the flight log.
- Add a test-only `MockGateway` (returns canned responses) under `#[cfg(test)]`.
**Acceptance:** demo unchanged; a unit test drives the state machine through a full
text→tool→text cycle with no network.

### ▢ p1.2 — The scheduler (multi-agent, cooperative)
**Depends on:** p1.1
**Goal:** Run many agents concurrently; the scheduler drives each agent's effects
and performs the IO (inference + tools).
**Scope:** new `agentd/src/scheduler.rs`; `agentd/src/config.rs` (support `[[agents]]`); `agentd/src/main.rs`.
- `Scheduler` owns the set of `AgentTask`s, a ready queue, the gateway, and the
  registry. Loop: pick a ready agent → `step` → fulfill the effect (await
  inference / invoke tools, concurrently across agents) → feed back → repeat.
- Config grows to multiple agents; keep single-agent config accepted (back-compat).
**Acceptance:** boot 2+ agents on independent tasks; they run concurrently to
completion; flight events are interleaved and tagged by agent id.

### ▢ p1.3 — Metered scheduling & admission control
**Depends on:** p1.2
**Goal:** Enforce a **global** cognition budget and concurrency under scarcity —
defer rather than overspend. This is the core research problem; treat it as such.
**Scope:** `agentd/src/scheduler.rs` (+ a small `agentd/src/budget.rs` if it helps).
- Global token/$ ceiling across all agents (per-agent budgets still apply).
- Max in-flight inference concurrency cap; optional token-rate limiter.
- A policy: priority + fair-share; when the ceiling/cap is hit, agents enter a
  waiting state and are admitted as budget/slots free.
**Acceptance:** with a low global ceiling and concurrency cap of 1, two agents are
serialized, one deferred until budget frees; total spend never exceeds the ceiling;
scheduler logs `scheduled` / `deferred` / `admission_denied`.

### ▢ p1.4 — Capability system (least privilege)
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

### ▢ p1.5 — Inter-agent bus + sub-agents (A2A/ACP)
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

### ▢ p1.6 — Agent identity & Agent Cards (discovery)
**Depends on:** p1.5
**Goal:** Each agent advertises an identity + skills so others can discover what it
can do (A2A Agent Card).
**Scope:** small additions to `agentd/src/bus.rs` / a registry; a `discover` tool.
- `AgentCard { id, name, description, skills }`; a registry the bus consults; a
  `list_agents` / `discover` tool.
**Acceptance:** agents are enumerable with their cards; A can discover B's advertised
skill before messaging it.

**Exit criteria for Phase 1:** multiple capability-scoped agents run under a
budget-aware scheduler, discover and message each other, and spawn sub-agents — all
fully recorded. The runtime is now an OS for agents, still running on a normal distro.

---

## Phase 2 — The distro (bootable & light)

Goal: turn the runtime into a minimal bootable image where `agentd` is the userspace.
See DESIGN.md Parts 4 & 6.

### ▢ p2.1 — rustls + static musl binary
**Depends on:** Phase 1. Switch `reqwest` to `default-features = false, features =
["json", "rustls-tls"]`; build `--target x86_64-unknown-linux-musl`.
**Acceptance:** a static `agentd` runs with no system OpenSSL dependency.

### ▢ p2.2 — Buildroot minimal rootfs
**Depends on:** p2.1. Buildroot config (musl + busybox) producing a tiny rootfs that
boots straight to `agentd` as the boot target.
**Acceptance:** QEMU boots directly into `agentd` running an agent.

### ▢ p2.3 — Boot/supervision basics
**Depends on:** p2.2. `agentd` (or a tiny init in front of it) handles PID-1 duties:
signals, zombie reaping, essential mounts, clean shutdown.
**Acceptance:** clean boot and shutdown in QEMU.

### ▢ p2.4 — Image size budget
**Depends on:** p2.3. Measure and trim toward the "super light" target; add a CI size
check.
**Acceptance:** documented image size with a CI guard against regressions.

---

## Phase 3 — OS surfaces (agents as first-class kernel objects)

Goal: make "agent as primitive" visible at the system level. See DESIGN.md Part 4 (L2).

### ▢ p3.1 — `/agents` FUSE filesystem
Each running agent appears as a directory (`status`, `context_size`, `budget`,
`flight` tail). **Acceptance:** `ls /agents`, `cat /agents/<id>/status` work against
the live runtime.

### ▢ p3.2 — Agent checkpoint / restore
Persist an agent's working context + ledger to disk and resume it (app-level first;
CRIU exploration later). **Acceptance:** suspend an agent, restart `agentd`, resume it.

### ▢ p3.3 — eBPF/LSM enforcement (exploratory)
Enforce capability scopes (p1.4) at the syscall boundary for tool subprocesses.
**Acceptance:** spike doc + prototype showing a denied syscall.

---

## Phase 4 — Isolation & hardening

Goal: defense-in-depth for tools/agents. See DESIGN.md Part 5.

### ▢ p4.1 — Per-tool sandboxing (seccomp + namespaces)
Run tool-servers sandboxed; map each capability set to a sandbox profile.
**Acceptance:** a tool cannot exceed its capability at the OS level.

### ▢ p4.2 — Stronger isolation option (gVisor / microVM)
For untrusted tools/agents, run under gVisor or a Cloud Hypervisor microVM.
**Acceptance:** a tool runs in the chosen sandbox with measured overhead.

### ▢ p4.3 — Security review pass
Threat model: secret handling, flight-recorder redaction, budget-exhaustion DoS,
supply chain. **Acceptance:** documented threat model + fixes landed.

---

## Beyond

Memory substrate (two-tier in-context / external memory from DESIGN.md), additional
inference backends (incl. a local `impl InferenceGateway`), richer A2A/ACP
interop, and multi-device agent migration. Re-plan these into a stack when Phase 1–2
land.
