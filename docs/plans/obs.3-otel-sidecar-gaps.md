# obs.3 — OTLP Sidecar Gap Remediation

<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260626-172611.md -->

## Context

obs.2 (v0.43.0) shipped three deferred items from obs.1 and fixed five adversarial findings.
Two known gaps were explicitly deferred to obs.3 in TODOS.md:

- **obs.2-ar-01 (P3):** Copy-truncate rotation undetected when new file has grown past old offset
- **obs.2-ar-02 (P3):** OTLP backend-down export failures not surfaced in stats line

This increment closes both gaps. All changes are confined to the `otel/` crate.
No `agentd`, `surfaces`, `sandbox`, or `agentctl` changes.

---

## Problem Statement

### Gap 1: Copy-truncate false-negative

`FileTailer::poll()` detects rotation via:
1. Changed `(dev, ino)` — catches rename/delete-and-recreate (logrotate default)
2. `cur_len < self.offset` — catches copy-truncate when new file is still small

The blind spot: if logrotate copies the file and the new file receives more appends
**before the next poll tick**, `cur_len >= self.offset` and `rotated = false`.
The tailer then seeks to `self.offset` in the *new* file, skipping all lines written
from byte 0 to `self.offset`.

Example:
- Old file: 50 000 bytes. Tailer has `offset = 50 000`.
- Logrotate copies file, truncates it to 0, new data starts flowing.
- 500 ms later: new file is 52 000 bytes.
- Tailer polls: `cur_len (52 000) >= offset (50 000)` → `rotated = false`.
- Tailer seeks to byte 50 000, reads only the 2 000-byte tail. Bytes 0–50 000 skipped.

**Why mtime doesn't work:** After copy-truncate, the new file's mtime is the truncation
time, which is *later* than the stored last_mtime. A backward mtime check
(`cur_mtime < last_mtime`) would never fire for rotation — only for NTP clock steps
(a false positive). Mtime alone cannot distinguish "same inode, normal append" from
"same inode, truncated and regrew past old offset."

Fix: **content sentinel** — store the last 64 bytes read at `self.offset` and compare
on the next poll. If the file grew but those 64 bytes changed, it was truncated and
rewritten; treat as rotation. Note: this substantially reduces (not eliminates) the
blind spot — byte-identical content at the sentinel window would still evade detection,
but nanosecond timestamps in every JSONL line make this astronomically unlikely in practice.

Implementation sketch:
- Add `last_sentinel: Vec<u8>` (initialized to `vec![]`) to `FileTailer`.
- After reading lines in `poll()`, if `new_offset >= SENTINEL_SIZE`: seek to `new_offset - SENTINEL_SIZE`,
  read 64 bytes into `self.last_sentinel`. Skip capture when `new_offset < SENTINEL_SIZE` (prevents u64 underflow).
- On the next poll, when `cur_dev == self.dev && cur_ino == self.ino && cur_len >= self.offset`:
  skip sentinel check if `self.offset < SENTINEL_SIZE` OR `self.last_sentinel.len() != SENTINEL_SIZE` (not yet
  populated — prevents false-positive rotation on first append after `from_beginning=false` start).
  Otherwise seek to `self.offset - SENTINEL_SIZE`, read 64 bytes, compare. If different → `rotated = true`.
- Add three unit tests: fast-grow (rotated=true), no-false-positive (rotated=false after append), startup-FP
  prevention (first append after `from_beginning=false` → rotated=false).
- Sentinel size constant: `SENTINEL_SIZE: usize = 64`.

### Gap 2: Backend-down drop invisibility

`SPANS_DROPPED` only counts channel backpressure drops (when `mpsc::Sender::try_send`
fails because the 10 000-slot channel is full). Backend-level drops — spans that passed
through the channel but were lost because the OTLP endpoint was unreachable — are
invisible in the stats line.

`provider.force_flush()` returns `Vec<ExportResult>`. We already log each
`Err(e)` to stderr (added in obs.2). We do not count them.

Additionally, the `BatchSpanProcessor` internally retries and eventually drops spans
when the backend is persistently down. The SDK does not expose these counts directly,
but `force_flush()` returning errors is a reliable proxy for "export failed this cycle."

Fix: count `force_flush()` errors as `export_drops` (a new separate counter, distinct
from `SPANS_DROPPED` which tracks channel-level drops). Report both in the periodic
stats line and in the `record_drops()` OTLP metric call.

Implementation sketch:
- Add `export_drops: u64` field to main loop state (alongside `exported_count`, `flushed_on_rotation`).
- In SIGTERM, SIGINT, and periodic stats paths: use `spawn_blocking` with `provider.clone()`:
  ```rust
  let p = provider.clone();  // SdkTracerProvider wraps Arc — clone is O(1)
  let results = tokio::task::spawn_blocking(move || p.force_flush()).await?;
  for r in results { if let Err(e) = r { export_drops += 1; eprintln!("..."); } }
  ```
- In SIGTERM and SIGINT handlers: after force_flush(), print a final stats line with
  `export_drops` before breaking (currently no final stats line is emitted at shutdown).
- Note: `export_drops` counts flush-attempt failures, not spans. One error may represent
  one or thousands of lost spans. The counter is a signal that the backend had problems,
  not an exact drop count. Document this in a code comment.
- Update stats line: `exported={N} open={M} dropped={D} export_drops={E} flushed_on_rotation={R}`.
- Add `export_drops_counter: Counter<u64>` to `TokenCounter` in `exporter.rs` (description: "OTLP export flush-attempt failures", unit: "failures") — separate from the existing channel-drops counter so dashboards can distinguish backpressure drops (spans) from backend failures (flush attempts).
- Known limitation (obs.3-ar-01): `BatchSpanProcessor` has its own 2048-slot internal queue
  whose drops are not counted by any counter. The `OTEL_BSP_MAX_QUEUE_SIZE` env var can tune
  this limit as a mitigation. SDK fork or wrapper required to surface the drop count — deferred.

---

## Acceptance Criteria

### Gap 1 (copy-truncate)
- [ ] `FileTailer` stores `last_sentinel: Vec<u8>` (last 64 bytes at last-consumed offset), initialized to `vec![]`
- [ ] `SENTINEL_SIZE: usize = 64` constant
- [ ] After reading lines in `poll()`, capture sentinel bytes into `self.last_sentinel` — skip capture when `new_offset < SENTINEL_SIZE` (prevents u64 underflow on small files)
- [ ] On next poll (same dev/ino, cur_len >= offset): read 64 bytes at offset−64 and compare with `last_sentinel`; if different → `rotated = true`
- [ ] Skip sentinel check when `self.offset < SENTINEL_SIZE` (file too small)
- [ ] Skip sentinel check when `self.last_sentinel.len() != SENTINEL_SIZE` (not yet populated — prevents false-positive on first append after `from_beginning=false` start)
- [ ] New unit test (fast-grow): write large block → poll (captures sentinel) → truncate + write different content past old offset → poll → assert `rotated = true`
- [ ] New unit test (no false positive): write large block → poll (captures sentinel) → append small amount → poll → assert `rotated = false` and new lines returned
- [ ] New unit test (startup FP): open with `from_beginning=false` against pre-existing large file → append new content → first poll → assert `rotated = false` (sentinel not yet populated, check skipped)
- [ ] Existing copy-truncate test still passes (small file case)

### Gap 2 (export drops)
- [ ] `export_drops: u64` tracked in main loop
- [ ] SIGTERM + SIGINT handlers: `let p = provider.clone(); let results = tokio::task::spawn_blocking(move || p.force_flush()).await?;` — count errors → `export_drops`; print final stats line before break
- [ ] Periodic stats path: `let p = provider.clone(); let results = tokio::task::spawn_blocking(move || p.force_flush()).await?;` (non-blocking; `provider.clone()` required — `SdkTracerProvider` wraps `Arc`, clone is cheap)
- [ ] Periodic stats path counts force_flush errors → `export_drops`; `eprintln!` each error
- [ ] Code comment: "export_drops counts flush-attempt failures, not spans; one error may represent many lost spans"
- [ ] Stats line: `exported=N open=M dropped=D export_drops=E flushed_on_rotation=R`
- [ ] `export_drops` delta reported via a NEW `export_drops_counter` (separate from `record_drops()` channel counter); add `export_drops_counter: Counter<u64>` to `TokenCounter` with description "OTLP export flush-attempt failures" and unit "failures"
- [ ] Integration note: export-drop counting not unit-testable without a real failing OTLP endpoint (document this)
- [ ] TODOS.md: add obs.3-ar-01 (BatchSpanProcessor internal 2048-slot queue drops uncounted)

### Both gaps
- [ ] `cargo test --workspace` passes (≥1006 tests, likely a few new)
- [ ] `cargo clippy -- -D warnings` clean
- [ ] CHANGELOG.md updated
- [ ] TODOS.md: obs.2-ar-01 and obs.2-ar-02 marked resolved
- [ ] ROADMAP.md: obs.3 entry added and marked ✅
- [ ] CLAUDE.md: obs.3 status entry added

---

## Files Changed

| File | Change |
|---|---|
| `otel/src/tail.rs` | Add `last_sentinel`, content-sentinel rotation detection, new tests (fast-grow + no-FP) |
| `otel/src/main.rs` | Add `export_drops`, force_flush counting, spawn_blocking periodic path, final stats line at shutdown |
| `otel/src/exporter.rs` | Add `export_drops_counter: Counter<u64>` to `TokenCounter`; new `record_export_drops()` method |
| `CHANGELOG.md` | obs.3 entry |
| `TODOS.md` | Close obs.2-ar-01 and obs.2-ar-02 |
| `docs/ROADMAP.md` | Add obs.3 entry, mark ✅ |
| `CLAUDE.md` | Add obs.3 status block |

No new dependencies. No API surface changes. No FUSE changes. No agentd changes.

---

## Version

`v0.44.0` (patch increment from v0.43.0; HARNESS-only change)

---

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 2 | CLEAN | mode: HOLD_SCOPE, 0 critical gaps |
| Codex Review | outside voice (eng) | Independent 2nd opinion | 1 | Issues resolved | 1 critical (sentinel init FP), 2 real (spawn_blocking scope + clone), 2 minor |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | CLEAN | 5 issues resolved: E1 capture guard, E2 FP test, E3 separate metric, E4 sentinel init, E5 spawn_blocking scope |
| Design Review | — | N/A (no UI changes) | 0 | SKIPPED | — |
| DX Review | — | N/A (no user-facing API) | 0 | SKIPPED | — |

**CODEX:** Critical: sentinel `last_sentinel=[]` on first append after `from_beginning=false` start → false-positive rotation. Fixed by skipping check when `last_sentinel.len() != SENTINEL_SIZE`. Real: SIGTERM/SIGINT force_flush() blocking + missing `provider.clone()` for spawn_blocking. Both resolved in acceptance criteria.

**CROSS-MODEL:** Primary review caught E1 (capture guard), E2 (FP test), E3 (metric units), and noted SIGTERM blocking as P3. Codex independently caught the more severe sentinel initialization bug (E4) and confirmed the SIGTERM concern (E5). Full consensus — no disagreements.

**VERDICT:** CEO + ENG CLEARED — ready to implement.

NO UNRESOLVED DECISIONS
