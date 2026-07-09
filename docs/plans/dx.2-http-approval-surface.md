# dx.2 — HTTP Approval Surface (fail-closed)

<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260704-165114.md -->

## Goal

Allow `agentctl watch --url http://HOST:7999` to approve/deny agent actions without
a FUSE mount. Today the Approvals view (`[a]` key) reads from `/agents/approvals`
(FUSE-only) and writes to `/agents/control` (FUSE-only). This makes the management
HTTP API a second-class citizen for daily-use approval workflows.

**Fail-closed invariant:** any HTTP transport failure in approve/deny returns an error;
the action stays pending. Never silently grant.

## What exists (p7.7)

- `GET /api/v1/approvals` — returns pending actions ✓
- `GET /api/v1/snapshot` — returns full scheduler state ✓
- `DataSource` trait — `load_snapshot()` only (no approval read/write) ✗
- FUSE approve/deny writes to `/agents/control` ✓
- TUI Approvals view reads FUSE-only, writes FUSE-only ✗

## Dependencies

All complete: p7.7 (Management HTTP API), p7.4 (Approval gate), p7.3 (FUSE control surface).

## Deferred items from p7.7 that feed into dx.2

- ar-01 (LOW): SSE `/api/v1/events` has no unit test
- ar-02 (LOW): `detect_source()` FUSE→HTTP→bail fallback untested
- ar-03 (LOW): `egress_brokered`/`egress_denied` hardcoded 0 in `HttpSource`
- ar-04 (LOW): `status_detail` not threaded through `agent_info_from_json`
- ar-05 (INFO): loopback guard is post-bind (no change needed)

## Scope

### E1: HTTP approve/deny routes in management API

**Files:** `agentd/src/management.rs`, `agentd/src/events.rs`

New routes:
- `POST /api/v1/approvals/:id/approve` — sends `ControlCommand::Approve { id, edits: None, auto_approve_kind: None }`
- `POST /api/v1/approvals/:id/deny` — body: optional `{"reason":"..."}` — sends `ControlCommand::Reject { id, reason }`

`ApiState` gains `control_tx: Option<tokio::sync::mpsc::Sender<ControlCommand>>`.

Fail-closed cases:
- `control_tx` is `None` → 503 Service Unavailable with `Retry-After: 1` header
- Channel full (`TrySendError::Full`) → 503 with `Retry-After: 1` header
- Empty or invalid approval_id → 400 Bad Request
- Unknown approval_id (not in `pending_actions` snapshot) → 404 Not Found

New event kinds: `ApprovalHttpApproved`, `ApprovalHttpDenied` (both recorded in `management.rs`).
Event payload must include `id` and `agent_id` to match existing approval event schema.

`management::start()` signature gains:
`control_tx: Option<tokio::sync::mpsc::Sender<crate::control::ControlCommand>>`

New tests (in management.rs):
- `approve_returns_503_without_control_tx`
- `deny_returns_503_without_control_tx`
- `approve_empty_id_returns_400`
- `approve_unknown_id_returns_404` (double-approve guard: second call on resolved action returns 404)
- `approve_happy_path_sends_command` — integration test: real `control_tx`/`control_rx` pair, assert channel receives `ControlCommand::Approve { id: "act_0", .. }`
- SSE endpoint test: `sse_content_type_and_framing` (fixes ar-01)

### E2: Scheduler always wired to control channel

**File:** `agentd/src/main.rs`

Remove the `maybe_session.is_some()` gate on Linux. Always wire:
```rust
#[cfg(target_os = "linux")]
let scheduler = scheduler.with_control(control_rx);
```

Rationale: HTTP API may send Approve/Reject commands even when FUSE is not mounted.
Pass `Some(control_tx.clone())` to `management::start()`.

### E3: DataSource trait extension

**File:** `agentctl/src/watch/source.rs`

Add to trait:
```rust
fn load_approvals(&self) -> Vec<PendingAction>;
fn approve(&self, id: &str) -> Result<(), String>;
fn deny(&self, id: &str, reason: Option<&str>) -> Result<(), String>;
```

`FuseSource` implementation:
- `load_approvals()` → `reader::read_approvals(&self.agents_dir)`
- `approve(id)` → `write_control_command(&self.agents_dir, &json!({"approve":{"id":id}}).to_string())`
- `deny(id, reason)` → `write_control_command(...)` with optional reason field

`HttpSource` implementation (fail-closed):
- `load_approvals()` → `GET /api/v1/approvals` → parse via `pending_action_from_json()`
- `approve(id)` → `POST /api/v1/approvals/{id}/approve` with **500ms timeout** (distinct from 5s read timeout) → any non-2xx or transport error → `Err(message)`. Never retry.
- `deny(id, reason)` → `POST /api/v1/approvals/{id}/deny` with body `{"reason":...}` (optional); same 500ms timeout.

Mutation timeout rationale: the TUI is single-threaded; a blocked approve/deny call freezes the terminal for up to 5s under the default read timeout. 500ms is long enough for a loopback call while keeping the TUI responsive.

Move `write_control_command` from `mod.rs` to `source.rs` as `pub(crate)`. Keep `#[cfg(unix)]` guard (the function uses `libc::close`).

New helper: `pending_action_from_json(v: &Value) -> PendingAction`.

Fix ar-04: parse `status_detail` in `agent_info_from_json()`.

Add tests:
- `detect_source_fuse_path_returns_fuse_source` (ar-02)
- `detect_source_fallback_to_http_when_no_fuse` (ar-02)
- `pending_action_from_json_basic`
- `http_source_approve_fails_without_server` (fail-closed: Err when unreachable)

### E4: TUI wiring + plain-mode fix

**Files:** `agentctl/src/watch/app.rs`, `agentctl/src/watch/mod.rs`

`app.rs` changes:
- Add `pub fn update_approvals(&mut self, items: Vec<PendingAction>)` method
- Remove `reader::read_approvals` call from `apply_snapshot()` (it reads FUSE directly)
- On `Ok(())` from `source.approve()`/`source.deny()`, optimistically remove the entry from `approvals_items` to avoid stale-entry flicker (server may still show it for up to 1 tick).

`mod.rs` changes:
- `run_tui` main loop: add `let approvals = source.load_approvals(); app.update_approvals(approvals);`
- `run_plain()`: add same `source.load_approvals()` + `app.update_approvals(approvals)` call after `apply_snapshot` (plain-mode approval output was FUSE-only; fixes HTTP+plain regression)
- `handle_approvals_key` signature: add `source: &dyn DataSource` param
- Replace all `write_control_command(&app.agents_dir, ...)` calls with `source.approve(id)` / `source.deny(id, reason)`
- On `Err`, set `approvals_view.result_msg` — do NOT clear it on the next tick (it persists until operator navigates away, matching existing behavior)
- Update call sites in both the main loop and the re-entry loop after spawn

Tests:
- Add `struct TestSource` (minimal mock DataSource) in mod.rs test module
- Update all `handle_approvals_key(KeyCode::..., &mut app)` calls to `handle_approvals_key(KeyCode::..., &mut app, &TestSource::fail())`

### E4b: `agentctl approve` / `agentctl deny` CLI subcommands

**File:** `agentctl/src/main.rs`, new `agentctl/src/approve.rs`

New subcommands:
```
agentctl approve <id> [--url http://HOST:7999]
agentctl deny <id> [--reason "..."] [--url http://HOST:7999]
```

Both subcommands:
1. Call `detect_source(url_flag, AGENTCTL_URL_env)` — same auto-detection as `watch`
2. Call `source.approve(id)` / `source.deny(id, reason)`
3. On `Ok(())`: print `Approved {id}` / `Denied {id}` to stdout, exit 0
4. On `Err(msg)`: print `error: {msg}` to stderr, exit 1 — never retry (fail-closed)

`--reason` is optional for `deny`; omitting sends no reason field.

Tests:
- `approve_subcommand_exits_1_on_error` (mock HttpSource returns Err)
- `deny_subcommand_exits_0_on_success` (mock FuseSource returns Ok)

### E5: Fix remaining deferred items

- **ar-04** (status_detail): Done in E3 — `agent_info_from_json` parses `status_detail` field
- **ar-01** (SSE test): Done in E1
- **ar-02** (detect_source tests): Done in E3
- **ar-03** (egress_brokered/egress_denied): Deferred — requires snapshot schema change; out of dx.2 scope

### E6: Doc and event taxonomy updates

- `otel/tests/event_kind_coverage.rs`: add 2 new event kind arms
- `docs/CONVENTIONS.md`: add 2 new rows to event table
- `TODOS.md`: close ar-01, ar-02, ar-04; keep ar-03, ar-05
- `CHANGELOG.md`: add dx.2 entry
- `CLAUDE.md`: update with dx.2 completion and test count
- `docs/ROADMAP.md`: mark dx.2 complete
- Version: 0.53.0 → 0.54.0

## Out of scope

- ar-03 (egress stats in HTTP mode): snapshot schema change; deferred
- ar-05 (loopback guard pre-bind): info only; no change
- `approve` with `edits` JSON body over HTTP (FUSE path only for now)
- `auto_approve_kind` in HTTP path (TUI "don't ask again" button — deferred)
- HTTP-mode source indicator in TUI (nice-to-have; defer to dx.3)
- Auth/multi-client concurrency model (loopback-only; deferred to security hardening phase)

## Acceptance

1. `cargo build && cargo clippy -- -D warnings && cargo test` pass clean
2. `POST /api/v1/approvals/act_0/approve` → 200 when control_tx is present; 503 when absent
3. `POST /api/v1/approvals/act_0/approve` (second call on same id, already resolved) → 404
4. `HttpSource::approve()` returns `Err(_)` when server is unreachable (fail-closed verified by test)
5. `agentctl watch --url http://HOST:7999` approvals view shows pending actions
6. `agentctl approve <id> --url http://HOST:7999` exits 0 on success, 1 on error
7. `agentctl watch --url ... --plain` shows pending approvals (not empty list)
8. FUSE path is unchanged (no regression in FuseSource behavior)
9. `otel/tests/event_kind_coverage.rs` compiles with `ApprovalHttpApproved`, `ApprovalHttpDenied`
10. Test count grows from 1081 to ≥1115

## Implementation order

1. `agentd/src/events.rs` — add 2 new event kinds (unblocks management.rs compile)
2. `agentd/src/management.rs` — E1: routes + control_tx + 404 guard + integration test + SSE test
3. `agentd/src/main.rs` — E2: always wire control_rx; pass control_tx to management
4. `agentctl/src/watch/source.rs` — E3: trait extension + FuseSource + HttpSource (500ms timeout) + ar-04 + ar-02
5. `agentctl/src/watch/app.rs` — E4: update_approvals() method, optimistic removal, remove direct FUSE call
6. `agentctl/src/watch/mod.rs` — E4: route approvals through source in run_tui + run_plain + key handler
7. `agentctl/src/main.rs` + `agentctl/src/approve.rs` — E4b: approve/deny CLI subcommands
8. `otel/tests/event_kind_coverage.rs` — E6: new event kind arms
9. Doc updates — E6

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|-----------|-----------|----------|---------|
| 1 | CEO | Add `agentctl approve/deny` CLI subcommands to dx.2 scope | SCOPE | Daily-use requires non-TUI path | SSH operators can't use TUI; CLI makes it scriptable | Defer to dx.3 |
| 2 | CEO | Narrow "completes daily-use" to TUI + CLI (not "all daily-use workflows") | FRAMING | Accuracy | edits/auto_approve/auth deferred; claim must match scope | Keep broad claim |
| 3 | Eng | Add 404 when approval ID not found in pending_actions | CORRECTNESS | Fail-closed | Second POST on resolved action should be 404 not silent 200 | Return 200 always |
| 4 | Eng | 500ms mutation timeout on approve/deny HTTP calls | UX | Terminal responsiveness | Single-threaded TUI freezes for up to 5s otherwise | Use same 5s timeout |
| 5 | Eng | Add `approve_happy_path_sends_command` integration test | TESTING | Record everything | 503/400 tests don't verify the core control-flow path | Unit tests only |
| 6 | Eng | Add `Retry-After: 1` header on 503 channel-full | UX | Operator clarity | Signals transient failure vs. permanent error | No header |
| 7 | DX | Fix plain-mode: call `source.load_approvals()` in `run_plain()` | CORRECTNESS | No silent regressions | HTTP+plain mode shows empty approvals without this fix | Leave as-is |
| 8 | DX | Optimistic local removal after approve Ok(()) | UX | Responsiveness | Avoids stale-entry flicker for up to 1 tick | Wait for server |
