<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260626-151846.md -->
# obs.2 — OTLP sidecar hardening (batch exporter + validation tests + rotation flush)

**Track:** HARNESS (`otel/` crate only — zero `agentd` changes)
**Depends on:** obs.1 ✅ (v0.42.0)
**Releases as:** v0.43.0

---

## Problem

obs.1 shipped the `agentos-otel` sidecar with three known weaknesses deferred to obs.2:

1. **Per-span blocking export** (`with_simple_exporter`): under high span throughput the
   export worker blocks on each OTLP network call synchronously. This fills the 10 000-span
   backpressure channel unnecessarily and inflates `SPANS_DROPPED`. A batch exporter amortizes
   the network cost across many spans.

2. **No unit tests for input validation**: `validate_log_path` and `validate_endpoint`
   guard against injection attacks (world-writable flight log, embedded credentials in endpoint
   URL), but those rejection paths have zero dedicated tests — only implicit binary-level
   coverage.

3. **Silent log rotation handling**: `FileTailer::poll` returns a `rotated: bool` flag but
   `main.rs` silently discards it. On log rotation, `SpanBuilder` retains stale open-span
   state from the old file, producing phantom spans with incorrect timestamps.

---

## User story

As the operator running `agentos-otel` next to a busy multi-agent session, I see zero dropped
spans under normal load (batch exporter handles burst), validation failures print clear
messages rather than failing silently in tests, and file rotation (e.g. `logrotate daily`)
flushes stale open spans cleanly rather than emitting phantom data to my OTLP backend.

---

## Scope

### In scope

1. **Batch exporter** (`otel/src/exporter.rs`):
   - Replace `with_simple_exporter` with a `BatchSpanProcessor` for both gRPC and HTTP paths.
     The correct API for `opentelemetry_sdk =0.24.0` is:
     ```rust
     use opentelemetry_sdk::trace::{BatchSpanProcessor, BatchConfigBuilder};
     let batch = BatchSpanProcessor::builder(exporter, opentelemetry_sdk::runtime::Tokio)
         .with_batch_config(
             BatchConfigBuilder::default()
                 .with_max_export_batch_size(512)
                 .with_scheduled_delay(Duration::from_secs(5))
                 .with_max_export_timeout(Duration::from_secs(30))
                 .build(),
         )
         .build();
     SdkTracerProvider::builder()
         .with_span_processor(batch)
         .with_config(config)
         .build()
     ```
     (`with_batch_exporter` on the builder does NOT accept `BatchConfig` in 0.24 — always use
     `BatchSpanProcessor::builder` + `with_span_processor`.)
   - Keep `try_send` in `spawn_export_worker` (unchanged). With the batch exporter, `emit_span`
     enqueues into the SDK's internal buffer and returns immediately — the channel is no longer
     a backpressure point. The `max_export_timeout = 30s` in `BatchConfig` handles endpoint-down
     scenarios at the OTLP export level. Do NOT change `try_send` → `send().await`.
   - Add graceful shutdown: on SIGTERM (or loop exit), call BOTH `sb.drain_all(now_ns, "shutdown")`
     and `provider.force_flush()` before exit. This handles short-run sessions where the 30s
     idle watchdog hasn't fired yet. Print: `"agentos-otel: shutdown — flushed {n} open spans"`.
     (Requires storing the `SdkTracerProvider` in main scope.)

2. **Validation error message improvements** (`otel/src/main.rs`):
   - World-writable error: append `(fix: chmod o-w <path>)` to the message
   - Embedded-credentials error: add `(use OTEL_EXPORTER_OTLP_HEADERS for auth instead)` 
   - Absolute-path error: add example `(e.g. /var/log/agentos/flight.jsonl)`
   - These are one-line additions to the existing `anyhow::ensure!` strings

3. **Startup banner update** (`otel/src/main.rs`):
   - Add `batch_delay_ms=5000` to the startup banner `eprintln!` call so operators
     know spans may lag before appearing in their OTLP backend
   - Add `OTEL_EXPORT_BATCH_DELAY_MS` env var (default: 5000ms) so operators can tune
     the flush interval (e.g. 500ms in dev, 5000ms in prod); wire into `BatchConfigBuilder`
   - Update help text: change `Set 'true'` → `Set 'true' or '1'` for all boolean env vars
   - Add `OTEL_EXPORT_BATCH_DELAY_MS  Batch flush interval in ms (default: 5000)` to help

4. **Validation unit tests** (`otel/src/main.rs`):
   - `validate_log_path_rejects_relative` — non-absolute path → Err
   - `validate_log_path_rejects_non_jsonl` — wrong extension → Err
   - `validate_log_path_accepts_valid_missing_file` — absolute `.jsonl` that doesn't exist yet → Ok
   - `validate_endpoint_rejects_non_http` — `ftp://…` or bare hostname → Err
   - `validate_endpoint_rejects_embedded_credentials` — `https://user:pass@host` → Err
   - `validate_endpoint_accepts_http` — `http://localhost:4318` → Ok
   - `validate_endpoint_accepts_https` — `https://otel.example.com/v1/traces` → Ok

3. **Log rotation flush** (`otel/src/main.rs`):
   - Destructure `(lines, rotated)` from `tailer.poll()` (currently uses `_rotated`)
   - When `rotated`, call `sb.drain_all(now_ns, "log_rotated")` before processing `lines`
   - Emit `eprintln!("agentos-otel: log rotation detected — flushed {} stale spans", n)` 
   - Add `forced_close: "log_rotated"` span attribute to all rotation-flushed spans (data
     integrity: these are incomplete spans, not successfully observed events; operators
     must not interpret them as complete trace evidence)
   - Do NOT count rotation-flushed spans toward `exported_count`; track separately as
     `flushed_on_rotation: u64`
   - Print `flushed_on_rotation=R` in the periodic stats line alongside
     `exported=N open=M dropped=D`
   - Emit explicit stderr message on rotation: 
     `"agentos-otel: log rotation detected — flushed {n} stale spans (marked forced_close)"`

4. **SpanBuilder state reset on rotation** (`otel/src/span_builder.rs` + `otel/src/main.rs`):
   - Add `SpanBuilder::reset_for_rotation(&mut self)` method that calls `drain_all` AND
     resets `trace_id`, `run_id`, `run_span_id`, `agent_span_ids`, and `span_counter` to
     defaults. This prevents stale parent-trace context from being attached to spans from the
     new file before a `scheduler_started` event arrives.
   - Call `sb.reset_for_rotation(now_ns)` (instead of bare `drain_all`) in the rotation handler.
   - Known limitation (deferred): `FileTailer` does not detect copy-truncate when the new file's
     length ≥ old offset. Documented in TODOS.md; fix requires rewriting the rotation detection
     to use `mtime` as a secondary signal.

### Out of scope (deferred)

- In-process `send().await` timeout tuning (the 5s `BatchConfig` flush handles this)
- Metrics-exporter batch pipeline (already uses `opentelemetry_sdk` periodic reader — not
  blocking per measurement)
- Binary size CI guard for `agentos-otel` (the 6 MB guard only applies to `agentd`)
- Additional semconv mappings (obs.3+)

---

## Architecture

No new modules. Changes are across `exporter.rs`, `span_builder.rs`, and `main.rs`.

```
otel/src/
  exporter.rs     build_provider → BatchSpanProcessor + BatchConfigBuilder
                  spawn_export_worker → try_send unchanged (SDK buffers internally)
                  new: store SdkTracerProvider for graceful flush
  span_builder.rs new: reset_for_rotation() — drain_all + clear trace/run state
  main.rs         poll loop: handle rotated flag → sb.reset_for_rotation()
                  startup banner: add batch_delay_ms
                  OTEL_EXPORT_BATCH_DELAY_MS env var wired into BatchConfigBuilder
                  validation errors: fix hints added
                  SIGTERM handler: sb.drain_all + provider.force_flush()
                  #[cfg(test)]: validate_log_path + validate_endpoint unit tests
```

---

## Acceptance criteria

- [ ] `cargo test --workspace` passes with ≥ 7 new tests in `otel/` (4 validate_log_path + 4
      validate_endpoint coverage, accounting for the "accepts_valid" Ok case)
- [ ] `cargo clippy -- -D warnings` clean on macOS
- [ ] `agentos-otel` starts successfully against a live OTLP endpoint (manual spot-check or
      docker-compose smoke test)
- [ ] Log rotation: manually truncating `flight.jsonl` during a run produces a `log rotation
      detected` stderr line and no phantom spans
- [ ] `SPANS_DROPPED` metric stays 0 under a 100-span burst in the existing integration tests

---

## Test plan

Unit tests in `otel/src/main.rs` (7 new):
- `validate_log_path_rejects_relative`
- `validate_log_path_rejects_non_jsonl`
- `validate_log_path_accepts_valid_missing_file`
- `validate_log_path_rejects_world_writable` (requires `#[cfg(unix)]`)
- `validate_endpoint_rejects_non_http`
- `validate_endpoint_rejects_embedded_credentials`
- `validate_endpoint_accepts_http`
- `validate_endpoint_accepts_https`

Existing tests in `otel/src/tail.rs` provide rotation coverage — `test_tail_copy_truncate`
already verifies `rotated=true` is returned. The `main.rs` integration is verified by the
rotation flush behavior (above acceptance criterion).

---

## Files touched

- `otel/src/exporter.rs` — BatchSpanProcessor migration, graceful SIGTERM flush
- `otel/src/span_builder.rs` — `reset_for_rotation()` method
- `otel/src/main.rs` — rotation flush handler (uses `reset_for_rotation`), `flushed_on_rotation` counter in stats, unit tests
- `otel/Cargo.toml` — no new deps (all batch exporter types already in `opentelemetry_sdk`)
- `TODOS.md` — mark obs.1-ar-01, obs.1-ar-02, obs.1-ar-03 resolved
- `CHANGELOG.md` — v0.43.0 entry
- `docs/ROADMAP.md` — mark obs.2 complete

---

## Risk

**Low.** No `agentd` changes. `opentelemetry_sdk` 0.24 already has `with_batch_exporter` —
no version bump required. The batch exporter introduces async flush timing (spans up to 5s
delayed before export), which is acceptable for a background observability sidecar. The
`send().await` change in `spawn_export_worker` introduces blocking, but the worker task is
already isolated on a tokio thread — no impact on the main poll loop.

---

## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| 1 | CEO | Accept obs.2 3-item scope | Mechanical | P3/P5 | Code-grounded, one-increment scope | — |
| 2 | CEO | Add forced_close attr to rotation-flushed spans | Mechanical | P1 | Data integrity — both CEO voices | counted-as-exported |
| 3 | CEO | Defer observability contract to obs.3 | Mechanical | P3 | Out of scope for hardening increment | expand-now |
| 4 | Eng | Fix BatchSpanProcessor API: BatchConfigBuilder + with_span_processor | Mechanical | P5 | Only correct API in sdk=0.24.0; will_not_compile otherwise | with_batch_exporter(cfg) |
| 5 | Eng | Drop send().await plan item | Mechanical | P5 | Batch exporter handles internal queue; channel-level await is wrong tool | send().await+timeout |
| 6 | Eng | Endpoint-down: max_export_timeout=30s in BatchConfigBuilder | Mechanical | P1 | SDK controls OTLP timeout at batch export level | channel-timeout |
| 7 | Eng | Add SpanBuilder::reset_for_rotation() to clear all non-open state | Mechanical | P1 | Both voices flag stale trace_id/run_id after rotation | drain_all-only |
| 8 | Eng | Note copy-truncate-not-detected as known gap, defer | Mechanical | P3 | Requires tail.rs rewrite; out of scope | fix-now |
| 9 | Eng | SIGTERM flush: drain SpanBuilder + provider.force_flush() | Taste→APPROVED | P2 | Short-run sessions lose spans without it; in blast radius | defer |
| 10 | DX | Add fix hints to validation error messages | Mechanical | P5 | Both voices; messages state what but not how | silent |
| 11 | DX | Print batch_delay_ms in startup banner | Mechanical | P5 | Both voices; 5s lag invisible to operator | no-doc |
| 12 | DX | No OTEL_FLUSH_ON_EXIT (flush unconditional on SIGTERM) | Mechanical | P5 | Codex: no no-op env vars | add-var |
| 13 | DX | Add OTEL_EXPORT_BATCH_DELAY_MS env var | Taste→APPROVED | P2 | Operator-approved; dev=500ms vs prod=5000ms | fixed-5s |
| 14 | DX | Update help text for boolean env vars | Mechanical | P5 | Minor consistency; 'true' or '1' both work | — |
| 15 | DX | SIGTERM: drain SpanBuilder + provider (both) | Mechanical | P1 | Short-run sessions need both flushes | provider-only |

---

## GSTACK REVIEW REPORT

**Status:** APPROVED

**Review scores:**
- CEO: 5/6 consensus confirmed. Both voices agreed on rotation data integrity, send().await gap. 2 plan expansions added (forced_close + export timeout).
- Eng: 4/6 consensus confirmed. Critical API error caught (BatchSpanProcessor vs with_batch_exporter). reset_for_rotation added. SIGTERM flush approved.
- DX: 5/6 consensus confirmed. Error message fix hints, batch delay documentation, OTEL_EXPORT_BATCH_DELAY_MS env var approved.

**Decisions made:** 15 total (13 auto-decided mechanical, 2 taste decisions surfaced + approved by operator)

**User challenges:** 0

**Cross-phase themes:**
1. Rotation handling completeness — CEO (data integrity) + Eng (state reset) + DX (stats) all independently flagged. Plan now addresses all three facets.
2. Batch delay operator visibility — DX both voices. Startup banner + new env var added.

**Deferred to TODOS.md:**
- Copy-truncate rotation miss when new file length ≥ old offset (tail.rs mtime signal needed)
- Backend-down SDK export failure not surfaced in stats (async SDK internals, obs.3)
- Observability contract definition (obs.3)

**Files touched (final):**
- `otel/src/exporter.rs` — BatchSpanProcessor migration + OTEL_EXPORT_BATCH_DELAY_MS + SIGTERM flush
- `otel/src/span_builder.rs` — `reset_for_rotation()` method
- `otel/src/main.rs` — rotation flush handler, validation fix hints, startup banner, unit tests
- `otel/Cargo.toml` — no new deps
- `TODOS.md` — mark obs.1-ar-01/02/03 resolved; add copy-truncate + backend-down gaps
- `CHANGELOG.md` — v0.43.0 entry
- `docs/ROADMAP.md` — mark obs.2 complete
