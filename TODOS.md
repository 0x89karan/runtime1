# TODOS

## Phase 0 — Technical Debt

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

## Completed

**p0.1 — Crate scaffold + config + flight recorder**
- Created `agentd/` binary crate with Config (TOML), FlightRecorder (append-only JSONL),
  EventKind enum, CI workflow, README, LICENSE.
- All acceptance criteria met: `cargo build` + `cargo clippy -D warnings` + `cargo test` pass.
- **Completed:** v0.1.0 (2026-06-07)
