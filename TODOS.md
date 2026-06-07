# TODOS

## Phase 0 — Technical Debt

**P2 — Sync I/O in native tool impls (p0.5)**
- `ReadFile`, `WriteFile`, `ListDir` all use `std::fs` inside `#[async_trait]` methods,
  blocking the tokio thread. Harmless for p0.3 (sequential, small files), but will
  matter when parallel tool dispatch arrives in Phase 1.
- Action: migrate to `tokio::fs` when the first concurrent tool call path lands (p0.5 or p1.1).

**~~P2 — ToolRegistry::register should error on collision (p0.5)~~** ✓ Done in p0.5.

**P3 — Per-agent capability scoping for native file tools (p1.4)**
- `read_file`, `write_file`, `list_dir` currently have unrestricted path access.
  Intentional for p0.x (single-tenant, mutually trusting agents), but agents should
  declare required capabilities (`FsRead{prefix}`, `FsWrite{prefix}`) per CONVENTIONS.md.
- Action: implement capability gating in p1.4 when the capability registry lands.

**P3 — 2 MB binary target needs re-evaluation at p0.2**
- `reqwest` + `native-tls` (arriving in p0.2) will significantly increase binary size.
- Consider `rustls` instead of `native-tls`, or a size audit, before p2.1.
- Tracked: known from autoplan review of p0.1.

**P3 — flight.jsonl CWD footgun for multi-agent (p1.2)**
- A shared `flight.jsonl` in CWD won't work for concurrent multi-agent runs.
- Needs a per-agent path strategy or shared recorder with agent tagging.
- CONVENTIONS.md already mandates agent tagging per event; path strategy is the open question.
- Action: design at p1.2 when the scheduler is introduced.

**P3 — EventKind enum in flight_recorder.rs → events.rs at p0.4**
- Once all 11 Phase-0 kinds are actively emitted, extract to its own module.
- Keeps `flight_recorder.rs` focused on I/O, not taxonomy.
- Action: extract during p0.4 implementation.

**P2 — MCP tools/list pagination not followed (p0.5 adversarial review)**
- `McpClient::spawn` only fetches the first page of `tools/list`. A `tracing::warn!` is
  emitted when `nextCursor` is present, but the remaining pages are silently dropped.
- Action: implement cursor-based iteration in the first increment that uses a multi-page server.

**P2 — MCP graceful shutdown (p0.5 adversarial review)**
- Process teardown uses SIGKILL via `kill_on_drop(true)`. Servers needing a clean shutdown
  (flush WAL, release locks) may lose state.
- Action: send `notifications/shutdown` + SIGTERM with a grace period before SIGKILL; implement
  in Phase 1 when the scheduler owns process lifecycle.

**P2 — StopReason::MaxTokens produces empty Ok("") (pre-existing, p0.4)**
- When the model is cut off mid-generation, `agent::run` returns `Ok("")` because no `Text`
  block is present. Callers can't distinguish a real empty answer from a truncated one.
- Action: emit a `tracing::warn!` or return `Err(BudgetExceeded)` at Phase 1 iteration.

## Completed

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

**p0.5 — Real MCP stdio client**
- `McpClient`: newline-delimited JSON-RPC 2.0 over tokio::process::Child (kill_on_drop).
  Handshake: initialize → notifications/initialized → tools/list. `tools/call` for invocation.
- `McpTool` implements `Tool`; `isError: true` → `anyhow` error.
- `ToolRegistry::register` now errors on collision (upgraded from warn).
- `echo-mcp` fixture binary + integration tests for MCP startup, coexistence, missing-server.
- Release binary: 1.4 MB on macOS.
- **Completed:** 2026-06-07
