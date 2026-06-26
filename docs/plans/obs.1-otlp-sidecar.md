<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260626-001702.md -->
# obs.1 — flight→OTLP sidecar + GenAI semconv

**Track:** HARNESS (no `agentd` changes except two new flight events: `scheduler_started`/`scheduler_stopped`)
**Depends on:** p7.5 (egress mediator) ✅, p7.6 (universal tier) ✅
**Releases as:** v0.42.0 (first harness-only versioned increment)

---

## Problem

`agentd` already records everything via the flight recorder (44 event kinds, structured JSONL,
span-shaped with agent/turn/tool hierarchy and token metering). The gap is **format**: none of
the standard observability backends (Jaeger, Grafana/Tempo, Honeycomb, OTLP-compatible tooling)
can ingest `flight.jsonl` natively. The value of obs.1 is zero additional instrumentation — it
is pure translation from an internal format to the industry standard (OTLP/GenAI semconv).

A second gap: when a **universal-tier foreign agent** (p7.6) makes model calls through the
egress proxy, those calls are in a separate process without W3C trace context. They appear
as isolated egress events, not child spans under the agent's trace. The egress mediator can
inject `traceparent` headers to bridge them into the same trace.

---

## User story

As the single operator of an AgentOS deployment, I point `OTEL_EXPORTER_OTLP_ENDPOINT` at my existing Grafana/Jaeger stack and immediately see agent runs as distributed traces — with inference spans carrying token counts, tool spans showing latency, and multi-agent coordinator→scout relationships visible in the trace tree. No custom tooling required; standard OTLP dashboards work out of the box.

---

## What this is NOT

- Not new instrumentation. The flight recorder already records everything worth observing.
- Not a metrics backend. The sidecar exports to a user-supplied OTLP endpoint (Jaeger, Tempo,
  OTEL Collector, Honeycomb, etc.); it does not run one.
- Not a replacement for `flight.jsonl`. The JSONL stays as the canonical in-core record.
- Not eBPF observability. That is Phase 9 (separate roadmap item, different scope).

---

## Scope

### In scope

1. **`agentos-otel` sidecar** — new binary at `otel/` in the repo root (Python or Rust;
   decision needed). Tails `flight.jsonl`, reconstructs the trace tree, emits OTLP.
2. **Flight-event → OTLP span mapping** — explicit mapping for all 44 current event kinds:
   - `agent_spawned`/`agent_completed`/`agent_failed` = agent-level span lifecycle
   - `inference_request`/`inference_response` = child span, GenAI semconv (`gen_ai.*`)
   - `tool_call`/`tool_result` = child span
   - `egress_brokered`/`egress_denied` = child span (egress tier)
   - `memory_*` = child span
   - Other events = span events on the enclosing span
   - token metrics from `inference_response` events = OTLP metrics
3. **GenAI semantic conventions** — `gen_ai.system`, `gen_ai.request.model`,
   `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.request.max_tokens`
4. **W3C `traceparent` injection** — thin change in `agentd/src/egress.rs`: when brokering
   a call for a universal-tier agent, inject `traceparent` header so the foreign workload's
   downstream calls join the same trace. Out: 4 lines in core, OTLP context prop. spec compliant.
5. **Docker integration** — sidecar included in `agentos:full` image; `docker/` compose
   example with OTEL Collector + Jaeger or Tempo.
6. **Tests** — unit tests for the event mapping, integration test that a sample
   `flight.jsonl` produces well-formed OTLP spans + metrics.

### Out of scope (deferred)

- In-core cargo-feature OTLP exporter (deferred; sidecar approach preserves binary size)
- eBPF / gVisor sink (Phase 9)
- Metrics beyond token counts (latency percentiles, queue depth histograms — Phase 9 or
  later obs increment)
- Sampling / rate limiting of the OTLP stream

---

## Architecture

```
agentd process
  ├── flight_recorder → flight.jsonl (existing)
  └── emits scheduler_started/stopped events (2 new events, ~10 lines)

agentos-otel sidecar (new, harness-only, otel/ workspace member)
  ├── tail_reader  : tail flight.jsonl; track (dev, ino, offset); polling default
  │                  in Docker compose (inotify not available cross-container)
  ├── span_builder : reconstruct trace tree from run_id + agent_id + (agent_id,turn) key
  ├── mapping      : exhaustive EventKind→OTLP mapping; dev-depends on agentd for
  │                  compile-time exhaustiveness test
  ├── semconv      : gen_ai.* attributes, pinned to GenAI semconv v1.29.0
  └── otlp_exporter: bounded channel (10k), batch export, drop counter, redaction filter
                     → OTEL_EXPORTER_OTLP_ENDPOINT (gRPC-tonic or http-proto)

agentos:full Docker image
  ├── agentd / agentctl (existing)
  ├── agentos-otel sidecar (new)
  └── docker/otel-compose.yml — otel-collector + jaeger example (polling mode)
```

**W3C traceparent injection: DEFERRED to obs.1-core.** obs.1 is harness-only.

---

## Flight event → OTLP span mapping

Span key: `(agent_id, turn)` — composite key used throughout span_builder. Turn is per-agent, not globally unique.

| Flight event | OTLP element | Key attributes | Notes |
|---|---|---|---|
| `scheduler_started` | Start **run span** (trace root) | `run.id`, `config_hash` | run_id = UUID v4, strip hyphens → 32-hex OTLP trace ID |
| `scheduler_stopped` | End run span (OK) | `agent_count` | |
| `agent_spawned` | Start **agent span** (child of run) | `agent.id`, `agent.task_preview`, `gen_ai.request.model` | |
| `agent_restored` | Start agent span if none open for this agent | `agent.id`, `restored=true` | synthesize if joining mid-run |
| `agent_completed` | End agent span (OK) | `agent.final_answer_preview` | |
| `agent_failed` / `budget_exceeded` / `max_turns_reached` / `agent_admission_denied` | End agent span (ERROR) | `error.message` | |
| `inference_request` | Start **inference span** (child of agent span, keyed `(agent_id, turn)`) | `gen_ai.request.model`, message count | |
| `inference_stream_started` | Start inference span (alias for streaming agents) | `gen_ai.request.model`, `streaming=true` | p7.2 streaming path |
| `inference_response` | End inference span | `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.response.finish_reasons` | |
| `inference_stream_completed` | End inference span (alias for streaming agents) | `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens` | p7.2 |
| `tool_call` | Start **tool span** (child of inference span) | `tool.name`, `tool.id`, `tool.input_preview` | |
| `tool_result` | End tool span | `tool.is_error`, `tool.output_preview` | |
| `egress_brokered` | **Span event** on inference span | `egress.dest`, `egress.input_tokens`, `content_audited` | No egress_completed event → span event only |
| `egress_denied` | Span event (ERROR) | `egress.attempted_dest` | |
| `memory_read` / `memory_write` | Span events | `memory.tier`, `memory.segment`, `memory.bytes` | |
| `capability_denied` | Span event (WARN) | `capability.required`, `tool.name` | |
| `approval_requested` / `approval_granted` / `approval_rejected` | Span events | `approval.id`, `approval.kind`, `approval.risk` | |
| `universal_agent_started` / `universal_agent_exited` | Span events on agent span | `agent.pid`, `agent.isolation` | |
| All other events | Span events on agent span | raw `kind` + `data` fields | catch-all; exhaustiveness test required |

**Duplicate open event policy:** if a second `inference_request` arrives for `(agent_id, turn)` when a span is already open for that key, force-close the existing span with status `UNFINISHED` (reason: `duplicate_open`) and open a new span with the new attributes.

**Orphan event policy:** if any event arrives for an `agent_id` with no open agent span (e.g., `agent_restored` mid-run without `agent_spawned`), synthesize an agent span with `synthesized=true` attribute.

**Inactivity watchdog:** if no new bytes are appended to `flight.jsonl` for `OTEL_IDLE_TIMEOUT_SECS` (default: 30), force-close all open spans with status `UNFINISHED` (reason: `watchdog_timeout`) and flush the exporter. Prevents SIGKILL → no-spans-exported.

Token/$ **metrics** (OTLP counters, labels: `session_id`, `gen_ai.system`, `gen_ai.request.model`):
- `gen_ai.client.token.usage{input_output}` — cumulative counter, NOT per-agent-id (avoids unbounded Prometheus cardinality)
- `agentos.budget.used` / `agentos.budget.remaining` — gauge, per-run
- `agentos.otel.spans_dropped` — counter, incremented when bounded channel is full

---

## Implementation steps

### Step 1 — `otel/` Rust workspace member

**Language: Rust** (consistent with repo ethos; no Python runtime dep; compile-time exhaustiveness on EventKind mapping).

**Crate versions (pinned — Rust OTLP ecosystem has breaking changes across minor versions):**
```toml
opentelemetry = "=0.24.0"
opentelemetry_sdk = "=0.24.0"
opentelemetry-otlp = "=0.17.0"   # features = ["grpc-tonic", "http-proto", "tls-roots"]
opentelemetry-semantic-conventions = "=0.16.0"
opentelemetry-appender-tracing = "=0.24.0"  # for log export (optional)
```

Both `grpc-tonic` and `http-proto` features compiled in (supports runtime protocol selection via env). Binary size impact stays in `otel/` only — never enters `agentd`.

**Dev dependency for exhaustiveness:**
```toml
[dev-dependencies]
agentd = { path = "../agentd" }  # for compile-time EventKind exhaustiveness test
```

The `otel/` crate depends on `agentd` only as a dev-dep (test code only). The runtime sidecar parses flight.jsonl as raw JSON strings; the dev-dep enables a test that constructs every `EventKind` variant and asserts the mapping table covers it.

### Step 2 — core: scheduler_started / scheduler_stopped events

Add two minimal flight events to `agentd/src/main.rs`:
- `scheduler_started` — emitted at boot, carries `run_id` (UUID v4 generated at startup) and `config_hash`
- `scheduler_stopped` — emitted on graceful shutdown, carries `run_id` and `agent_count`

These give the sidecar a clean run boundary and a stable run ID for the trace root. Approximately 10 lines total.

**W3C traceparent injection is DEFERRED to obs.1-core** (separate increment). Both CEO review voices confirmed the injection is underspecified (W3C context propagation lifecycle, checkpoint serialization of trace context, header privacy). obs.1 stays harness-only.

### Step 3 — span_builder: trace reconstruction

The sidecar reconstructs the trace tree from `flight.jsonl`:
- `scheduler_started.run_id` → **trace ID** (run is the trace root; each agentd invocation = one trace)
- `agent_id` → root span within the trace
- `turn` → child span under the agent span
- `parent_map` (from `message_sent` edges in flight log) → inter-agent span linking
- Event timestamp (from JSONL `ts` field) → span start/end

Span lifetimes:
- Run span: open on `scheduler_started`, close on `scheduler_stopped`
- Agent span: open on `agent_spawned`, close on terminal event (`agent_completed` / `agent_failed` / etc.)
- Turn span: implicit; scopes inference + tool spans within the same turn number
- Inference span: open on `inference_request`, close on `inference_response`
- Tool span: open on `tool_call`, close on `tool_result`

**Startup tail position:** sidecar starts at EOF by default (same as `tail -f`). Only events written after sidecar startup are processed. Operator sets `OTEL_TAIL_FROM_BEGINNING=true` for historical replay (re-processes entire file from offset 0). This prevents a month-old `flight.jsonl` from flooding Jaeger with zombie partial traces on first run.

**Startup behavior:** if sidecar starts after agentd (no `scheduler_started` seen), synthesize a run_id from the first event's timestamp + first agent_id. Emit spans from that point as a partial trace (marked `partial_run=true` on root span).

**Open-span buffer limit:** max 1,000 open spans in memory. If exceeded, force-close oldest spans with status `UNFINISHED` and a span event noting the buffer limit.

**Log rotation:** track `(dev, ino, offset)` triple — stat the path each poll cycle and compare inode. On inode change (rename/copy-truncate), re-open the path and seek to 0; do not replay events already processed. On Linux, use `inotify IN_MOVE_SELF` as a hint; fall back to polling (default for Docker compose cross-container, where inotify is unavailable). Polling interval: 500 ms default (`OTEL_POLL_INTERVAL_MS`).

**Copy-truncate handling (logrotate style):** if offset > current file size (truncated in-place), seek to 0 and re-process from the new content. The sidecar must not re-emit spans for already-processed events — use the `(dev, ino, offset)` triple to guard.

### Step 4 — semconv mapping

Apply `gen_ai.*` attributes per the OpenTelemetry GenAI semantic conventions spec:
- `gen_ai.system` = `"anthropic"` (from model config)
- `gen_ai.request.model` = model ID from `inference_request.model`
- `gen_ai.usage.input_tokens` = from `inference_response.usage_input`
- `gen_ai.usage.output_tokens` = from `inference_response.usage_output`
- `gen_ai.response.finish_reasons` = from `inference_response.stop_reason`

### Step 5 — OTLP export with backpressure

Export via `OTEL_EXPORTER_OTLP_ENDPOINT` env var (gRPC default, HTTP/protobuf via
`OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`). Standard OTEL env vars; no custom config needed.

**OTLP service name:** defaults to `agentos`. Override with standard `OTEL_SERVICE_NAME` env var. This is the name operators search for in Jaeger/Grafana Tempo.

**Startup health output (DX requirement):** at startup, sidecar prints to stderr:
```
agentos-otel: tailing <FLIGHT_LOG_PATH> (from EOF)
agentos-otel: exporting to <OTEL_EXPORTER_OTLP_ENDPOINT> (service: <OTEL_SERVICE_NAME>)
```
Every 60 seconds while running (or on each batch export), print to stderr:
```
agentos-otel: exported N spans, M metrics
```
Stderr-only (not stdout). Operator can silence with `2>/dev/null`.

**Backpressure:** bounded async channel (max 10,000 span records). When full, new records are dropped and `agentos.otel.spans_dropped` counter is incremented. The drop counter is itself exported as an OTLP metric so operators know data loss occurred.

**Privacy:** `OTEL_REDACT_PREVIEWS=true` env flag silences `*.preview` attributes (task_preview, tool.input_preview, tool.output_preview, final_answer_preview) before export. Default off. Documented in README.

**Token metrics cardinality:** `gen_ai.client.token.usage` is a cumulative counter with labels `session_id` (= run_id), `gen_ai.system` (= "anthropic"), `gen_ai.request.model`. NOT labeled by `agent.id` to avoid unbounded cardinality in Prometheus-compatible backends.

**GenAI semconv version:** pinned to OpenTelemetry GenAI semantic conventions `v1.29.0` (December 2024 release). Reference URL and spec commit documented in `otel/src/semconv.rs`.

**Environment variable reference (complete list):**

| Variable | Default | Description |
|---|---|---|
| `FLIGHT_LOG_PATH` | `/run/agentos/flight.jsonl` | Path to tail. Must be absolute `.jsonl`, not world-writable. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | (required) | OTLP gRPC or HTTP endpoint. Scheme: `http://` or `https://` only. |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc` | `grpc` or `http/protobuf` |
| `OTEL_SERVICE_NAME` | `agentos` | Service name shown in Jaeger/Tempo |
| `OTEL_REDACT_PREVIEWS` | `false` | Set `true` to silence `*.preview` span attributes |
| `OTEL_IDLE_TIMEOUT_SECS` | `30` | Seconds of inactivity before force-closing open spans |
| `OTEL_POLL_INTERVAL_MS` | `500` | File poll interval (ms). Used in Docker compose (no inotify cross-container). |
| `OTEL_TAIL_FROM_BEGINNING` | `false` | Set `true` to replay entire `flight.jsonl` from offset 0 |

### Step 6 — Docker integration

- Add `agentos-otel` to `Dockerfile` and the `agentos:full` stage.
- Add `docker/otel-compose.yml` with OTEL Collector + Jaeger example.
- Sidecar reads `FLIGHT_LOG_PATH` (default `/run/agentos/flight.jsonl`) + `OTEL_EXPORTER_OTLP_ENDPOINT`.

**FLIGHT_LOG_PATH validation (startup):**
1. Reject non-absolute paths or paths not ending in `.jsonl` (path traversal guard).
2. After opening, stat the file and reject world-writable (`mode & 0o002 != 0`) — a world-writable flight log is an injection risk.
3. Validate `OTEL_EXPORTER_OTLP_ENDPOINT` scheme: only `http://` or `https://` accepted (SSRF guard; reject anything else with a fatal startup error).

**`docker/otel-compose.yml`:** sets `OTEL_REDACT_PREVIEWS=true` by default to prevent preview text from leaving the local compose stack. Specifies ports:
- Jaeger UI: `http://localhost:16686` (search for service: `agentos`)
- OTLP gRPC collector: `localhost:4317`
- OTLP HTTP collector: `localhost:4318`

**Standalone binary invocation (non-Docker path):**
```bash
FLIGHT_LOG_PATH=/path/to/flight.jsonl \
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
./agentos-otel
```
This path must be documented in `otel/README.md` alongside the Docker compose example.

### Step 7 — tests

**Span builder / mapping (unit):**
1. `flight_to_spans` — sample `flight.jsonl` → assert span count, trace ID consistency, parent-child relationships, `gen_ai.*` attributes.
2. `streaming_inference_spans` — flight.jsonl with `inference_stream_started`/`inference_stream_completed` events → assert inference spans created (streaming alias path, no zero-span regressions).
3. `mid_run_attach` — flight.jsonl starting mid-run (no `scheduler_started`) → assert partial trace synthesized with `partial_run=true`, no panic.
4. `orphan_event_synthesis` — event for agent_id with no prior `agent_spawned` → assert agent span synthesized with `synthesized=true`.
5. `duplicate_open_policy` — two consecutive `inference_request` events for same `(agent_id, turn)` → first span closed as `UNFINISHED`, new span opened.
6. `watchdog_force_close` — no new bytes after OTEL_IDLE_TIMEOUT_SECS → all open spans closed as `UNFINISHED` with reason `watchdog_timeout`.

**EventKind exhaustiveness (compile-time / unit):**
7. `eventkinds_all_mapped` — dev-dep on `agentd`; construct every `EventKind` variant in a match arm; assert no variant maps to `Unmapped` (compile-time exhaustiveness enforced).

**Privacy / redaction (unit):**
8. `redact_previews_env_flag` — `OTEL_REDACT_PREVIEWS=true` → `task_preview`, `tool.input_preview`, `tool.output_preview`, `final_answer_preview` absent from span attributes; other attributes present.

**Backpressure (unit):**
9. `drop_counter_increments` — fill channel to 10,001 span records → `agentos.otel.spans_dropped` counter = 1.

**Security / validation (unit):**
10. `flight_log_path_relative_rejected` — relative path → startup fatal error.
11. `flight_log_path_non_jsonl_rejected` — path not ending in `.jsonl` → startup fatal error.
12. `flight_log_world_writable_rejected` — world-writable file → startup fatal error.
13. `otlp_endpoint_scheme_rejected` — `OTEL_EXPORTER_OTLP_ENDPOINT=ftp://x` → startup fatal error.

**Log rotation (unit):**
14. `copy_truncate_no_replay` — simulate inode-stable truncate → sidecar seeks to 0, does not re-emit already-processed events.
15. `inode_change_reopen` — simulate inode change (rename) → sidecar re-opens file from offset 0.

**Integration:**
16. Run sidecar against `coordinator-demo.agents.toml` fixture with mock OTLP receiver → assert spans exported, coordinator→scout parent-child links present.

---

## Acceptance criteria

- [ ] `cargo build` on all workspace members (including `otel/`) succeeds
- [ ] `cargo test` passes (≥ 995 total workspace tests, up from 979)
- [ ] A sample `flight.jsonl` from `coordinator-demo.agents.toml` produces a coherent OTLP trace:
      - `scheduler_started` event → trace root span with `run_id` as trace ID
      - agents are child spans of the run span (not separate traces)
      - each inference span has correct `gen_ai.*` attributes
      - inter-agent parent-child links (coordinator → scouts) from `parent_map`
- [ ] Streaming inference agents (`inference_stream_started`/`inference_stream_completed`) produce
      inference spans (not zero spans); streaming path covered by test #2 in Step 7
- [ ] Token/$ metrics exported as OTLP counter (`gen_ai.client.token.usage`) with session+model labels
- [ ] `docker/otel-compose.yml` starts cleanly; Jaeger UI at `http://localhost:16686` shows the trace under service `agentos`
- [ ] `agentd` binary size unchanged (sidecar deps do not enter core)
- [ ] `OTEL_REDACT_PREVIEWS=true` silences all `*.preview` attributes
- [ ] FLIGHT_LOG_PATH validation rejects relative paths, non-.jsonl, world-writable files
- [ ] Sidecar prints startup banner to stderr: tailing path + OTLP endpoint + service name
- [ ] `OTEL_TAIL_FROM_BEGINNING=false` (default): sidecar starts at EOF, no zombie traces
- [ ] Standalone binary invocation documented in `otel/README.md`
- [ ] CHANGELOG.md updated

---

## Files changed / created

**New:**
- `otel/` — new workspace member (`Cargo.toml`, `src/main.rs`, `src/mapping.rs`,
  `src/exporter.rs`)
- `docker/otel-compose.yml`

**Modified:**
- `Cargo.toml` (workspace) — add `otel` member
- `agentd/src/main.rs` — emit `scheduler_started` / `scheduler_stopped` events (~10 lines)
- `CHANGELOG.md`
- `docs/ROADMAP.md` — mark obs.1 complete
- `docs/CONVENTIONS.md` — add `scheduler_started` / `scheduler_stopped` rows to event taxonomy table

Note: `agentd/src/egress.rs` W3C traceparent injection is deferred to `obs.1-core` (Decision #9).

---

## Version bump

`v0.42.0` — first harness-only increment.

---

## CEO Review Findings

### Fixes applied to plan during CEO review

1. **Trace model corrected** — trace root is run/session, not agent. Agents are root spans within the run trace. Two new flight events added to agentd: `scheduler_started` (provides `run_id`) and `scheduler_stopped`. Required for coherent multi-agent trace trees in Jaeger.
2. **Contradiction resolved** — `egress_brokered` → child span (latency-bearing). `memory_read`/`memory_write` → span events (point-in-time).
3. **Backpressure added** — `otlp_exporter` module spec: bounded async channel (max 10,000 spans), configurable batch size, drop counter metric `agentos.otel.spans_dropped`.
4. **Privacy/redaction** — `OTEL_REDACT_PREVIEWS=true` env flag silences task/tool/answer preview attributes before export. Default off; documented in README.
5. **Log export included** (cherry-picked in cherry-pick scan) — OTLP log records for non-span events; ~50 lines, same mapping code.
6. **Acceptance criteria strengthened** — trace must show run as root, agents as child spans, correct `gen_ai.usage.input_tokens`, metrics exported.

### Deferred findings (to TODOS.md)

- `obs.1-todo-01`: `agentctl trace inspect` — purpose-built timeline view in agentctl (Codex reframe finding; valid but obs.2 scope)
- `obs.1-todo-02`: Grafana dashboard JSON template for AgentOS metrics
- `obs.1-todo-03`: Trace sampling / head-based sampling config
- `obs.1-todo-04`: `agentctl trace subcommand` for flamegraph view
- `obs.1-todo-05`: GenAI semconv version pinning documentation (minor, add to README)

---

## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|-----------|-----------|-----------|---------|
| 1 | CEO | Approach B (Rust workspace) over Python | Mechanical | P3+P5 | Consistent ethos; no runtime dep; compile-time exhaustiveness | Python (50 MB layer, no type safety) |
| 2 | CEO | Include OTLP log export | Mechanical | P2 | In blast radius, ~50 lines, same mapping code | Defer |
| 3 | CEO | Defer Grafana dashboard | Mechanical | P2 | Outside blast radius | Include |
| 4 | CEO | Defer trace sampling | Mechanical | P3 | Not needed for initial deployment | Include |
| 5 | CEO | Defer agentctl trace subcommand | Mechanical | P2 | New CLI command, M effort, not in blast radius | Include |
| 6 | CEO | Add scheduler_started/stopped events to core | Mechanical | P1 | Required for correct multi-agent trace root; ~10 lines | Skip traceparent injection |
| 7 | CEO | Add OTEL_REDACT_PREVIEWS env flag | Mechanical | P1 | Privacy/data governance; previews must not silently leak | Hard-block export |
| 8 | CEO | Add backpressure + drop counter | Mechanical | P1 | Observability exporters are failure amplifiers; bounded channel prevents OOM | Unbounded channel |
| 9 | CEO | Cut W3C traceparent injection from obs.1 | Mechanical | P5 | Both CEO voices: underspecified (context propagation lifecycle, checkpoint serialization, privacy); defer to obs.1-core | Keep in obs.1 |
| 10 | CEO | Add exhaustiveness test for EventKind mapping | Mechanical | P1 | Mapping table will rot silently as new events added; compile-time match + test required | Manual tracking |
| 11 | CEO | Token metrics: session_id+model labels only | Mechanical | P1 | Per-agent-id label = unbounded Prometheus cardinality | Per-agent labels |
| 12 | CEO | Add user story | Mechanical | P5 | "Wrong problem framing" from both voices; user story makes the value proposition explicit | Skip |
| 13 | Eng | Pin exact OTLP crate versions (0.24.0/0.17.0/0.16.0) | Mechanical | P3 | Rust OTLP ecosystem has breaking changes between minor versions; unpinned dep = random breakage on `cargo update` | Unpinned semver |
| 14 | Eng | `otel/` dev-depends on `agentd` for EventKind exhaustiveness | Mechanical | P1 | Compile-time match required; sidecar parses JSON at runtime but test enforces coverage of all EventKind variants | No exhaustiveness check |
| 15 | Eng | `(dev, ino, offset)` triple for log rotation tracking | Mechanical | P1 | Offset alone misses copy-truncate and inode rename; triple correctly handles all rotation patterns | Offset only |
| 16 | Eng | Polling default for Docker compose (not inotify) | Mechanical | P3 | inotify `IN_MOVE_SELF` is unavailable cross-container boundaries; polling is the only reliable fallback for compose | inotify-only |
| 17 | Eng | Streaming inference events as aliases (`inference_stream_*` → same path as `inference_request/response`) | Mechanical | P1 | p7.2 streaming path would produce zero inference spans without this; non-streaming and streaming agents produce identical span structure | Separate code path |
| 18 | Eng | FLIGHT_LOG_PATH validation (absolute + .jsonl + not world-writable) | Mechanical | P1 | World-writable flight log = injection risk; relative paths = TOCTOU ambiguity | Trust caller |
| 19 | Eng | OTEL_EXPORTER_OTLP_ENDPOINT scheme validation (http/https only) | Mechanical | P1 | SSRF risk if arbitrary scheme accepted | Trust caller |
| 20 | Eng | Expand Step 7 to 16 tests (streaming, mid-run attach, redaction, drop counter, rotation, exhaustiveness, security) | Mechanical | P2 | Original 3 tests covered < 30% of edge cases identified in eng review | 3 tests only |
| 21 | Eng | egress_brokered → span event (not child span) | Mechanical | P1 | No `egress_completed` event exists in events.rs; cannot measure latency; zero-duration child span would be misleading | Child span |
| 22 | DX | Service name = `agentos` (override via `OTEL_SERVICE_NAME`) | Mechanical | P1 | Without a known service name, operator can't find their traces in Jaeger; hardcoded default + standard env var override is zero cognitive load | Require explicit setting |
| 23 | DX | Tail from EOF by default (`OTEL_TAIL_FROM_BEGINNING=false`) | Mechanical | P1 | Starting from offset 0 on a month-old flight.jsonl floods Jaeger with zombie partial traces; EOF default matches `tail -f` mental model | Replay all history |
| 24 | DX | Startup banner + periodic `exported N spans` stderr output | Mechanical | P1 | Silent background process = impossible to debug; operator needs confirmation the sidecar is connected and processing events | Silent operation |
| 25 | DX | Add env var reference table to Step 5 + standalone binary run example to Step 6 | Mechanical | P2 | 9 env vars scattered across 4 sections; non-Docker operators need the standalone invocation pattern | In-line only |

---

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 1 | CLEAR | 12 decisions: trace-root corrected, traceparent deferred, backpressure+privacy added, scheduler_started/stopped events added |
| Eng Review | `/plan-eng-review` | Architecture & tests | 1 | CLEAR | 9 issues: crate version pins, composite key, rotation tracking, streaming aliases, egress event fix, SSRF/injection guards, 16-test expansion |
| DX Review | `/plan-devex-review` | Developer experience gaps | 1 | CLEAR | score: 5/10 → 7/10, TTHW: 7min → <3min; 4 decisions: service name, tail position, startup banner, env var table |

**VERDICT:** CEO + ENG + DX CLEARED — ready to implement.

NO UNRESOLVED DECISIONS
