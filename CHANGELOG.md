# Changelog

All notable changes to agentd are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [v0.115.0] - 2026-07-28

### Added
- **ux.13-TUI — row-scoped control verbs in `agentctl watch`.** ux.13 shipped `Cancel`/`SetBudget`/
  `SetCaps` end to end (management API, FUSE control, CLI, and `DataSource` methods) but no view invoked
  them: `docs/ROADMAP.md` recorded "**TUI keys deferred**". So the operator could not stop a runaway agent
  from the screen showing it to them, which is friction #1 of the approved design doc and the inverse of
  the track's north star ("you can act on what you see"). `[x]` on a Dashboard row now opens a graded
  row-action overlay.
  - **Park** — `set_budget` at the spend already recorded. Its label states which of two things it is,
    because neither is "a reversible pause": with `budget_reset_interval > 0` the park **expires by
    itself** at the next window rollover (`maybe_roll_budget_window` rebases every task's windowed spend,
    then `drain_deferred` re-admits), and with no window configured exhaustion **ends the agent**. Gated
    twice: `park_limit()` returns `None` below a 1 000-token floor (`0` means UNLIMITED and
    `set_token_budget` writes the CHECKPOINTED config, so parking a zero-spend agent would have un-capped
    it permanently, across restart, in Park's primary use case), and `park_would_widen()` blocks it when
    the recorded spend EXCEEDS the current cap — the normal post-exhaustion state, since the admission
    gate is checked before the turn, where capping at the spend would have RAISED the operator's ceiling.
  - **Set budget** — numeric field prefilled with the current limit, `0 = unlimited` stated on the field,
    and a second gate for any removal or raise. A typo is rejected in place, never parsed as `0`.
  - **Cancel** — its own confirm, showing "at least N" of the cascade from a cycle-safe `descendants()`
    walk, and reporting the SERVER's count (native subtree plus universal agents parented in) rather than
    the client's floor. `agentctl cancel` prints the same count in the same words.
  - **Verbs are performed by the loop, never the key handler.** `HttpSource`'s confirm client blocks up to
    3 s, so the confirm keypress writes `App.pending_verb` plus an `InFlight` frame and returns; the loop
    draws, then `drain_pending_verb` makes the call. Measured on a real pty: the frame is on the wire
    0.02 s after Enter against a server taking 2.5 s to answer. `handle_overlay_key` takes no `source`
    parameter, so that is a compile-time property. Buffered keystrokes typed during the call are
    discarded (Resize applied, Ctrl-C honoured) so two impatient presses cannot dismiss the result and
    then quit the cockpit.
  - **`?` — the first help key this cockpit ever had**, rendering from the same `DASHBOARD_KEYS` table as
    the footer so the two cannot drift, and documenting the keys the footer has no room for.
  - **`budget_resettable`** on the scheduler snapshot, HTTP `/api/v1/snapshot`, and FUSE
    `system/budget` (`{"spent":N,"total":0,"resettable":BOOL}`), because the cockpit cannot otherwise
    know what Park means. `agentctl` reads either wire name and defaults to `false` — the cautious
    reading, and the config default.
  - **A client-side `cancelling…` row marker** on its own line beside the attention idiom, since no
    `AgentStatus::Cancelling` exists and a cancelled row otherwise reads `running` for a whole turn and
    then vanishes. It escalates to `NOT CANCELLED` for anything the source could not confirm (FUSE
    writes, inferred descendants) and settles to `cancelled by you` once the scheduler confirms —
    without which a cancelled agent presents as a bare red `failed`, indistinguishable from a crash.

### Fixed
- **The Approvals confirm dialog rendered by INDEX while acting on a pinned id.** `update_approvals`
  replaces the whole list in Confirm mode and clamps only the index, so an approval resolving out of band
  (Telegram, `agentctl approve`, expiry) could leave the dialog showing one item's id/kind/**risk**/summary
  while `[a]` approved another. Cancel is not a security boundary; the approval gate is. One resolver
  (`App::confirm_item`) now serves the renderer and all three write paths, and a vanished pin reads as
  "already resolved" instead of a live-looking field set of `?`.
- **The Dashboard footer clipped its own resize hint.** Measured 162 columns with `[l]ogs` present, so
  `q quit` began at column 114 and `(resize to 115+ cols…)` at 122 — on the only widths where that branch
  renders. Now bounded per state (the narrow footer is drawn below 115 columns, so it gets its own 80-col
  budget) and asserted against a rendered 80x24 frame, which no string-length check can fake.
- **`agentctl cancel|set-budget|set-caps` printed raw HTTP errors** while the cockpit explained them.
  Both now share `explain_verb_error`, in surface-neutral wording.
- Agent ids that would re-point a mutation URL (`/ \ ? # %`, control characters) are refused at the
  `DataSource` boundary, and every id rendered in a dialog or printed command is sanitized and shell-quoted.

### Changed
- `DataSource::cancel` returns the server's cascade count; new `confirms_mutations()`,
  `supports_auto_approve_kind()`, and `cli_connection_flags()` let the cockpit stop claiming effects a
  source cannot deliver: over FUSE a queued command reads "cannot confirm the scheduler accepted it",
  `[d]` no longer claims a standing auto-approve rule where no HTTP route registers one, and every
  printed `Equivalent: agentctl …` carries the flags that reach THIS daemon.
- The chat rail's `converse::dispatch` moved onto the same `pending_verb` slot (TODOS.md's ranked P2,
  downgraded to P3): the echo and `Dispatching…` are drawn before the call, though the call still runs on
  the loop thread.

## [v0.114.0] - 2026-07-27

### Added
- **ux.10 (sub-part A) — the `[l]` Logs view in `agentctl watch`.** Live tail of
  `docker compose logs --follow --timestamps --no-color --tail 500`, so an operator no longer has to drop
  out of the cockpit and open a second terminal to see what the sidecars, agentd, and the credential
  broker are printing. Completes ux.10 (sub-part B shipped in v0.113.0; C struck as redundant).
  - **Sync, not tokio.** `agentctl watch` has no tokio runtime, so the child is a `std::process::Command`
    whose stdout is read on a background `std::thread` and pushed into the existing `sync_channel` as
    `AppEvent::LogLines(Vec<LogLine>)` (mirroring `pump.rs`'s producer + `catch_unwind`→`ProducerDied`
    guard). Lines land in a bounded 2 000-line ring (`LOG_RING_CAP`, mirroring `EVENT_RING_CAP`).
  - **`[l]`, not `[g]`** — `[g]` is the Spawn view's "generate preview", and `g`/`G` stay as in-view
    top/bottom scroll. Per-service `Tab` filter, `/` search (highlight + `n`/`N`, not a filter — a matching
    log line is only useful with its context), follow mode with `PAUSED`/`FOLLOW` state, `[t]` toggles
    relative↔absolute timestamps.
  - **Docker-context gated** on a startup `docker compose ps --all --quiet` probe (`--all` so a *stopped*
    project's history is still readable — the postmortem case). One flag gates both the key and the legend
    entry, so the cockpit can never advertise a key that does nothing; on bare agentd/QEMU the view is
    simply absent. The probe is bounded by a 3 s deadline and runs before the signal handlers, so a wedged
    Docker daemon can neither hang startup nor swallow Ctrl-C.
  - **No orphaned tail process.** `docker compose` is a CLI plugin: `docker` forks `docker-compose` and
    hands it our pipe, so `--follow` (which never EOFs) would strand it. `Producers` owns the `Child` and
    tears down the whole **process group** on `Drop` (`process_group(0)` + `kill(-pid)` + `wait`).
    Regression-tested with a fake `docker` that forks a grandchild — no daemon required.

### Fixed
- **ux.10-A hardening from `/review` + `/qa`** (all found before landing):
  - Service attribution used a substring match, which mislabeled every `cos` line as `agent` in this
    repo's own compose project (`agentos-cos-1` contains "agent"); now anchored to `-` segment boundaries.
  - The reader is bounded and lossy: `read_line` grew without limit on a newline-less record (a container
    could exhaust memory) and treated one invalid UTF-8 byte as EOF, permanently killing the tail. Records
    are now capped at 4 KB, returned immediately on truncation (liveness — a mid-record stall must not
    freeze other services' lines), resynchronized at the next newline, and decoded lossily.
  - Channel backpressure is waited out (250 ms) instead of dropped on sight: `docker compose logs` writes
    line-by-line, so `/qa`'s 5 000-line burst lost **4 479 lines (~90%)** before this change and loses none
    after. Loss past the deadline is still counted and shown in the header.
  - `n`/`N` deadlocked on a match inside the final viewport (offset-derived stepping read its own clamp
    back as "no movement"); stepping is now by match index.
  - `sanitize()` also strips DEL and C1 controls (log payloads are untrusted bytes), the header's search
    query is sanitized on the editing path and clipped, and search matching no longer allocates per line
    per frame.
- **ux.10-A, second review round** (specialist + red-team passes, all found before landing):
  - **Redraw storm.** `AppEvent::LogLines` requested a frame unconditionally, so a chatty compose project
    rebuilt the *Dashboard* at the 33 Hz tick ceiling (vs once per snapshot before) — and the slower drain
    then manufactured the very drops the header reported. Log events now only request a frame while the
    Logs view is on screen; the ring, service registry, and drop counter still fill off-view. Measured:
    150 bytes of terminal output in 4 s on the Dashboard vs 2 556 in the Logs view, same tail.
  - **Paste could freeze the cockpit.** Bracketed paste was inserted char-by-char into `tui-input`, which
    is O(n²), on the main render thread — a ~100 KB paste blocked the loop with Ctrl-C, `q`, and SIGTERM
    all inert (raw mode suppresses the tty's SIGINT and the loop isn't polling). Paste is now capped at
    8 192 chars, stripped of control characters, and spliced in one rebuild at the cursor. Fixes the four
    pre-existing sub-part B input sites too.
  - **Priority inversion.** The backpressure window let the log tail hold channel slots for 250 ms while
    every authoritative producer (snapshot, approvals, SSE) is drop-on-full — log verbosity could freeze
    the agent table silently. Cut to 60 ms (≈2 drain ticks), which still lands zero drops on the
    5 000-line flood.
  - **`--tail` is per container, not per project**: a flat 500 across this repo's 4 services replayed
    exactly the whole 2 000-line ring, so nothing but backfill was ever visible. Now a project budget
    divided by the container count.
  - Quit path bounded (a wedged `docker` could hang `Producers::drop` with the terminal still in raw
    mode); startup probe left in agentctl's own process group so Ctrl-C reaches it; the Logs view names
    the compose **project** in its title (it comes from the CWD, while the rest of the cockpit describes
    whatever `--url` points at); a min-height guard replaces an empty bordered box on a short terminal;
    the search-query header windows around the **cursor** instead of tail-keeping (tail-keeping hid the
    cursor when editing the start of a long query); a bare timestamp with no payload is parsed as an
    empty line instead of rendering the stamp as the message; `match_cursor` is invalidated on ring
    eviction; the render path makes one match pass per frame instead of three ring scans; and the
    `ProducerDied("logs")` string is a shared constant, since `step()` branches on it.
  - +24 tests (1 713 total), including the ones that pin the previously-unpinned parts: the docker gate's
    every branch (fake `docker` on PATH, no daemon needed), the tail's argv, `pump_lines`' normal path and
    batch cap, EINTR retry, the exactly-at-cap boundary, `sanitize`'s new DEL/C1 classes, and negative
    controls for the state resets that deleting would otherwise leave green.

## [v0.113.0] - 2026-07-27

### Changed
- **ux.10 (sub-part B) — real input widgets in `agentctl watch`.** The 5 hand-rolled `push(c)`/`pop()`
  text inputs are now `tui-input` (converse rail, memory search, inspector search, approvals
  reject-reason) and `tui-textarea` (spawn task field, multi-line) — so the operator gets cursor
  movement (Left/Right/Home/End/Ctrl-A), word-delete, and bracketed paste instead of end-only append.
  - Deps pinned exactly (`tui-input = "0.14"` + `tui-textarea = "0.7.0"`) to hold a **single ratatui
    0.29 / crossterm 0.28** — `tui-input` 0.15 would force a ratatui-0.30 bump and 0.10 would link a
    second ratatui; both avoided.
  - `step_key` now threads the full crossterm `KeyEvent` to the 5 focused-input handlers, intercepting
    `Enter`/`Esc`/`Tab` (+ rail Up/Down) before delegating so per-view semantics survive: converse
    `Enter`=send (busy-guard + reset preserved), spawn `Tab`=focus-cycle / `Enter`=newline / `[r]`=submit.
    Every other view keeps its `KeyCode` dispatch unchanged.
  - Bracketed paste enabled in `TermGuard` (paired disable on both `Drop` and the panic hook); `Event::Paste`
    routes to the focused widget.
  - The visible cursor now follows the real edit position — a true terminal cursor in the converse rail
    (`set_cursor_position` + `visual_scroll`) and a char-boundary-safe glyph at `input.cursor()` in the
    inline fields (multibyte-tested). Reshaped from the 2026-07-16 plan at `/autoplan` (sync-loop, not tokio;
    dep pins; spawn Enter/Tab) and the cursor defect caught at `/review`.
  - **Sub-part A (Logs view)** and **sub-part C (color-eyre, struck as redundant)** are not in this release;
    A is a separate follow-on (`docs/plans/ux.10-tui-polish.md`).

## [v0.112.0] - 2026-07-27

### Added
- **ux.3 — spawn custom agents on the fly over HTTP (addresses the p7.3-ar-02 cluster).** The `agentctl
  watch` Spawn view can now spawn a custom-capability agent into an already-running instance over the
  management API. The load-bearing bug: the client `SpawnRequest` never carried `capabilities`/`priority`,
  so over `--url`/HTTP the operator's toggled caps + priority were **silently dropped** (a custom-cap spawn
  became a default privileged spawn or a second exec'd agentd).
  - Client `SpawnRequest` now carries typed `Option<Vec<Capability>>` + `Option<u32>` priority (the shared
    `agentd::capability::Capability`, `skip_serializing_if`), so the wire body matches the server's
    `OperatorSpawnRequest`.
  - One shared resolver (`resolve_config`/`spawn_request_from_config`) makes the Spawn-view preview and the
    POST body semantically identical (same fields + values); the JSON-vs-TOML preview keys off the active
    source, not local FUSE presence.
  - The Spawn action routes **inline** through `DataSource::spawn()` → `POST /api/v1/spawn` when the source
    is HTTP (matches the approve/converse precedent — no second `agentd` exec'd, no terminal teardown, errors
    shown in the Spawn view); FUSE mode still writes `/agents/control`, and the always-erroring FUSE `spawn()`
    stub is unreachable from the Spawn view.
  - A sticky `pending_focus` auto-drops the operator into the new agent across the snapshot-insertion race;
    a privileged-cap refusal (cap.4, **400**) surfaces the server's reason verbatim (reject-not-clamp).
  - Reshaped from the draft at `/autoplan` (both eng voices: the preview and spawn payload diverged, a local
    `ANTHROPIC_API_KEY` gate would block remote HTTP, auto-focus had a race) and hardened at `/review` (Codex:
    `HttpSource::spawn` no longer fabricates an `agent_id` on a malformed response, which would have pinned
    focus to a non-existent agent forever). Scope: closes the TUI Spawn-view gap; the standalone `agentctl
    spawn` CLI-subcommand exec (the literal p7.3-ar-02) remains a P3 residual (operators use the TUI/API).

## [v0.111.0] - 2026-07-26

### Added
- **ux.2b — Idle + Error attention signals (closes cos-ux-01).** Two new `AttentionReason` variants on
  ux.2a's substrate, so the operator dashboard finally distinguishes a silently-wedged or errored agent
  from a healthy one at a glance.
  - **Idle** is computed **read-time** (`AgentSnapshot::idle_signal(now, threshold)` merged at the FUSE
    `/agents/<id>/attention` and HTTP `/api/v1/snapshot` surfaces), never at snapshot build — a build-time
    computation would freeze in the exact hung-tool wedge it exists to catch (the scheduler doesn't even
    tick `update_snapshot` while a tool future hangs). Allowlist is `status == Running` only; every
    parked/terminal status is intentionally quiet. Threshold 180s (`surfaces::IDLE_THRESHOLD_SECS`).
  - `last_event_at` is a runtime-only monotonic `Instant`, stamped once at the `enqueue_or_defer` effect
    choke point (covers `Infer`/`CallTools`/`SpawnAgent`/`RunJob`/`SendMessage`/`RequestApproval`), and
    re-seeded fresh on checkpoint restore (never serialized, mirrors `last_pressure`) — a restored agent
    starts fresh, never instantly idle.
  - **Error** fires when a tool call returns an error while the agent keeps running (inference errors already
    terminate → `Failed`); set/cleared centrally in `AgentTask::provide_tool_results` so it covers the async
    batch AND every synthetic reject path uniformly, auto-clearing on the next all-ok batch.
  - Reshaped from the original draft at `/autoplan` (both eng voices caught the draft computed idle at
    snapshot-build and missed the `Infer` stamp), then hardened at `/review` (Codex caught synthetic tool
    errors bypassing `last_error` and a universal-tier stale-snapshot false-idle). The landmine-guard test
    `idle_is_read_time_advances_on_same_snapshot` enforces the read-time architecture by construction.

## [v0.110.0] - 2026-07-26

### Fixed
- **doc.1 — audit-tail documentation drift (AUDIT-v0.86 P3-6).** Brought the reference docs current with
  the code shipped across the AUDIT-v0.97 remediation:
  - `docs/CONVENTIONS.md` — added the 8 event kinds emitted since the taxonomy table was last touched
    (`capabilities_resolved`, `agent_spawn_denied`, `capabilities_set`, `budget_set`, `budget_reset`,
    `agent_cancelled`, `runs_unavailable`, `brief_written`) and the FUSE-surface rows for the files added
    since p5.7 (`windowed_spend`, `tools`, `parent`, `sandbox`, `credentials`, `attention`, `/agents/control`,
    the `/agents/system/*` pseudofiles). Corrected the status-value row (`awaiting_approval`, not
    `awaiting_approval:<id>` — only `awaiting_child` carries the id) and rewrote the inode-allocation
    guidance to the real scheme (`OFF_*` on a `DIR_STEP = 20` stride, invariant `OFF_* < DIR_STEP - 1`;
    global pseudofiles use explicit low static inodes `INO_KB=9 … INO_SYS_CREDENTIALS=19`).
  - `docs/THREAT_MODEL.md` — §1.2 rewritten for the credential broker (the gateway holds the raw secret;
    the sidecar gets only per-request brokered access — a refreshed short-lived bearer for `oauth-bearer`,
    an injected configured key for `api-key-header`/`api-key-query`) plus the honest `passenv` fallback;
    §5.2 corrected (`cargo audit` **is** in CI); version pointer made version-agnostic.
  - `docs/RUNBOOK.md` / `docs/cos-guide.html` — Brave/passenv fix + `mcp_tool_called` → `tool_call`.

### Added
- **doc.1 — `agentd/tests/conventions_completeness.rs`.** Every `EventKind::ALL` value must appear in the
  CONVENTIONS event-taxonomy **table** — the check is scoped to that table's rows (not a whole-doc
  substring match), so a kind documented only in prose no longer false-passes table drift. A drift guard
  for the reference docs, matching the existing par.1 as_str/ALL guards.
- **doc.1 — `docker/cockpit.toml [memory]` block** (config P3-5 sub-item): explicit `max_entries_per_segment`
  / `max_entry_age_days` / `store_path` so the flagship's memory bounds are config-owned, not defaulted.

## [v0.109.0] - 2026-07-26

### Fixed
- **p3.1 — scheduler no longer aborts the whole runtime on a missing-agent effect (audit86-P3-3).**
  The `EffectResult` handling used `state.agents.get(&id).expect(...)` (and bare `[&id]` indexing) in
  both the inference and tools arms; under `panic = "abort"`, a future path that removed an agent
  mid-effect would kill the entire runtime. Every production agent-map access there is now a
  `let Some(..) else { record an Error event; continue }` (SetCaps returns `Err`) — a missing agent is
  recorded and skipped, never panics ("the loop never panics on bad input"). A /review catch fixed a
  subtlety in the mechanical conversion: the inference arm's shared `drain_deferred` is now run
  unconditionally (via a labeled block) so a slot-deferred agent is still admitted when the result's
  agent vanished — otherwise, under `max_concurrent_inferences = 1`, a deferred agent could strand.
- **p3.1 — crash-orphaned checkpoint tmp files are swept (audit86-P3-5).** `save()` writes
  `checkpoint.json.{pid}.{nanos}.tmp` then renames; a crash between the two leaked the tmp forever.
  `load()` now sweeps stale sibling tmp files, matching only `checkpoint.json.*.tmp` and only when older
  than 60s (so it can't disturb a concurrent instance's in-flight tmp).

### Not done (recorded)
- **audit86-P3-4 (checkpoint `FORMAT_VERSION` bump) struck as a do-not-do.** Bumping would make an old
  binary refuse a new checkpoint on rollback and discard CoS state; the code already documents that
  staying at 4 is deliberate (the added fields are additive `#[serde(default)]`, so both directions are
  already safe). Only bump for a genuinely breaking, non-additive schema change.

## [v0.108.0] - 2026-07-26

### Documentation
- **budget.1-ar-02 — the universal-tier global ceiling is a post-hoc SOFT cap, now documented honestly
  (not "fixed").** Both /autoplan review voices corrected the premise and recommended against building
  reservation machinery: the overshoot is not bounded by `max_concurrent_inferences` (that gates only
  the native in-process tier; the egress proxy accept loop is an unbounded `tokio::spawn` per
  connection), the whole surface is dormant in prod (cos ships no `universal` agents and no
  `[egress] proxy_addr`, so the proxy never starts), and a universal-only reservation can't make the
  *combined* native+universal ceiling hard anyway. The window is a single-tenant spend guardrail (not a
  security boundary) with the same shape as the native gate. The `egress.rs` pre-forward gate comment now
  states this accurately, and a new test (`global_budget_meter_is_a_posthoc_soft_cap`) pins the real
  bound — the tier 429s on the *next* request once `windowed >= ceiling` — instead of the false
  `N×max_tokens` bound. A hard cap (proxy concurrency semaphore + both-tier reservation) is filed as a
  follow-up gated on universal fan-out actually being enabled in prod.

## [v0.107.0] - 2026-07-25

### Fixed
- **cap.3 — FS-capability matching was CWD-blind + a boot containment hole (audit86-P1-8).** Capability
  path matching compared relative prefixes by string-identity while the `capability.rs` doc falsely
  claimed "relative paths fail-safe to deny" (the v0.86.2 root-cause class). A relative grant `./output`
  authorized `./output/x` regardless of where agentd was launched — the operator couldn't reason about
  the real absolute region. Now a startup-captured CWD anchor + a single `anchor_abs()` chokepoint in
  `satisfies()` absolutizes both grant and request, so matching is absolute-vs-absolute.
  - This **changes** mixed relative/absolute decisions (it is not representational): a relative request
    that lands inside an absolute grant (or vice versa) now correctly **allows** where the old lexical
    match wrongly denied — sound because the runtime resolves against the same anchor, so no over-grant
    and no previously-working flow breaks (both adversarial reviews confirmed zero reachable ALLOW→DENY).
  - **Closed a pre-existing boot exfiltration hole:** `main.rs`'s store/evidence/key containment guards
    now anchor both sides, so a config with an absolute memory store inside a **relative** MCP FS prefix
    (which the kernel sandbox resolves against CWD → the sandboxed server gets a dir *containing* the
    store) now fails at boot instead of silently passing the lexical guard.
  - Fail-closed on a grant that escapes the anchor (leading `..`, which would otherwise anchor to `/`
    and authorize the whole filesystem); no-chdir invariant hardened with a debug-build assert.

## [v0.106.0] - 2026-07-25

### Fixed
- **budget.1-ar-01 — MaxTokens truncation reported clean success for one-shot agents.** Under a reset
  window (the recommended prod config, which sets `budget_resettable` for every agent), a model
  `max_tokens` truncation returned `AgentEffect::Completed(truncated_text)` for ANY agent — so a
  one-shot job or a spawned child fed its silently-truncated output to the parent as a finished answer.
  (Filed at the AUDIT-v0.97 holistic review.) Now a new `AgentEffect::CompletedTruncated` variant is
  role-gated by the scheduler: a resident/orchestrated agent still **parks and is resumable** (the CoS
  self-brick fix, audit86-P0-2, preserved byte-for-byte), while a one-shot/child **fails** through the
  existing terminal funnel — the parent receives an `is_error` `ToolResult` naming the truncation, a
  sealed job gets a "failed" signal (cap.2b shielding intact), and `run_tracker` records "failed". The
  new variant forces every match site (scheduler + CLI shim) to handle truncation explicitly.
  P0-2 is now pinned at BOTH the AgentTask layer and — newly — the scheduler dispatch layer (previously
  unpinned there). Autoplan dual-voice reviewed (D1=new-variant, D2=fail-one-shot); both adversarial
  passes approved with no findings.

## [v0.105.0] - 2026-07-25

### Fixed
- **par.1-ar-01 — agentctl error view was blind to tool + inference errors.** The operator "Errors"
  filter (`inspector.rs`) and the red-colour rule (`views.rs`) matched `"kind":"tool_error"` /
  `"inference_error"` — kind strings agentd never emits — so only `agent_failed` ever showed; every
  tool failure and inference failure was invisible in the one view meant to surface them. (Found by
  par.1's exhaustiveness guard.) Now a shared `is_error_event(line)` predicate — used by BOTH sites,
  killing the duplicated dead-string list — matches the seven real error kinds: `agent_failed`,
  `error`, `mcp_http_error`, `fuse_control_error`, `egress_proxy_failed`, `credential_refresh_failed`
  (by kind), plus `tool_result` when `data.is_error` is true (an AND-guard, since a successful
  `tool_result` carries `is_error:false`). par.1's `AGENTCTL_KIND_MATCHES`/`KNOWN_NONCANONICAL`
  allowlists updated to the real strings; both drift-guard tests stay green. Autoplan dual-voice
  reviewed (D1=7 kinds).

## [v0.104.0] - 2026-07-25

### Fixed
- **Unbreak `main` — oauth in-image test-22** (P0, ci.2 escaped bug): ci.2's docker-smoke job runs
  `oauth_mcp.py --test` *inside the built image*, where test 22 (the `google.json` schema-drift guard)
  reads a fixture resolved relative to the script — `/etc/agentd/../tests/fixtures/google.json`, which
  isn't shipped in the image. It `fail()`ed, turning a legitimately-absent test fixture into a red
  `main`. Test 22 now **SKIPs when the fixture is absent** (`FileNotFoundError` only); a present-but-
  malformed/drifted fixture still `FAIL`s, so the guard keeps its teeth on the runner. A new
  `repo_consistency` assert pins the fixture's existence so the skip can never silently disable the
  guard repo-wide.
- **audit86-P3-1 — UTF-8 panic in the credential gateway**: three `token_url[..len().min(64)]`
  byte-slices (`credential/mod.rs`) panicked on a multi-byte char straddling byte 64 (reachable via a
  malformed operator secrets file). Replaced with a `token_url_preview()` char-boundary helper
  (`.chars().take(64)`, the existing `MAX_NARRATIVE_CHARS` idiom). Upholds "the loop never panics on
  bad input". Error-string-only; matching still uses the full string.
- **audit86-P3-2 — raw exception leaks in oauth_mcp error responses**: four sites returned the raw
  exception string (`broker_request_failed` — the primary broker path — `request_failed`,
  `Internal error`, and a stderr WARNING). Scrubbed to `type(exc).__name__` (matching the cred.5-ar-01
  siblings); HTTPError status paths untouched. New self-tests T36/T37.

### Added
- **run.1-ar-01 — call-site regression tests** for three run.1 durability fixes that were only
  helper-tested (so deleting the wiring left every test green): `close_segment → prune` (age-driven),
  the flight-recorder metadata-seed rotation path (sparse `set_len`), and the `MemoryPaged`
  `cap_short_term` drain (pre-seeded, asserts `evicted > 0` AND `len == MAX_SHORT_TERM`). Each is
  negative-control-verified (red when the fix is neutralized).

## [v0.103.0] - 2026-07-25

### Added
- **par.1 — drift guards** (6th AUDIT-v0.97 increment; tests-only, upholds no invariant directly but
  makes two cross-boundary duplications tamper-evident so a future edit — notably par.2 — can't silently
  desync them):
  - **Env-sanitization denylist parity** (P2-12): `docker/entrypoint.sh` and `distro/overlay/init`
    carry hand-mirrored boot-env secret denylists. `agentd/tests/env_denylist_parity.rs` asserts their
    token sets are equal (a drift panics naming the source + offending token) and that the boot loaders'
    `LD_*` keys are a subset of `docker/shell_mcp.py`'s (different-purpose) linker-hijack blocklist.
  - **EventKind string exhaustiveness** (P2-13): added `EventKind::as_str()`/`EventKind::ALL` as the
    single source of truth (unit tests prove `as_str()` matches the serde `snake_case` wire form and that
    `ALL` stays exhaustive). `agentctl/tests/event_kind_strings.rs` pins every flight-event kind string
    agentctl matches on to a real variant, so an event rename breaks the test rather than the TUI at runtime.
  - Both guards negative-control-verified (perturb → fail naming source, revert → green).

### Fixed (AUDIT-v0.97 holistic-stack review, 2026-07-25)
Cross-model /review over the whole `main..par.1` stack (6 increments). Codex caught three the
per-increment passes and the Claude adversarial pass missed; two are fixed here, two deferred (TODOS
budget.1-ar-01/-02, run.1-ar-01):
- **`/spawn` FsRead exfiltration closed** (cap.4 gap, Codex High): `is_privileged_spawn_cap` classified
  every `FsRead` as safe regardless of prefix, so `FsRead { prefix: "/" }` let an un-opted-in `/spawn`
  caller mint an agent reading any file (egress signing key, OAuth cache, checkpoints, mounted secrets)
  via `read_file`/`list_dir`. `FsRead` is now privileged (requires `AGENTOS_ALLOW_PRIVILEGED_SPAWN=1`);
  the bounded read paths a benign caller needs stay covered by `KbRead { segment }` + `RunsRead`.
- **Corrupt primary checkpoint no longer suppresses the `.restored` fallback** (audit.2 gap, Codex Medium):
  `load()` quarantined a garbled primary and gave up even when a valid pre-restore copy existed. It now
  falls back to `.restored` (quarantining the bad primary), completing the crash-after-restore resilience
  story. Also silenced a spurious "could not rename" warning on benign repeat-restart recovery.

### Known issues
- **par.1-ar-01** (P2, pre-existing, surfaced by the par.1 exhaustiveness guard): agentctl's Inspector
  "Errors" filter and red colour rule match `"kind":"tool_error"`/`"inference_error"` — strings agentd
  never emits — so tool-level and inference-level errors are invisible in the operator's error view (only
  `agent_failed` shows). Fix is a behavioral change (tool errors are `tool_result` + `is_error=true`, a
  data-field check, not a string swap), deferred out of tests-only par.1. Tracked by a self-shrinking
  `KNOWN_NONCANONICAL` allowlist test.

## [v0.102.0] - 2026-07-25

### Fixed
- **budget.1 — metering completeness** (5th AUDIT-v0.97 increment; upholds the "cognition is always
  accounted and bounded" invariant across both execution tiers):
  - **P2-2 universal-tier spend now counted + globally bounded** — universal (subprocess) inference,
    mediated by the HTTP egress proxy, was never added to `tokens_spent`, so the global window excluded
    it entirely. One shared `EgressProxy` + `GlobalBudgetMeter` now folds it in
    (`windowed = (native − native_anchor) + (universal − universal_anchor)`, separate anchors so a
    restart leaves the native window unchanged and forgives ephemeral universal spend) and
    pre-forward-rejects (429) on global exhaustion.
  - **MaxTokens self-brick** — `StopReason::MaxTokens` was an unconditional hard-fail; now gated on
    `!budget_resettable` so a resident agent parks/continues instead of bricking (reopened P0-2 class).
  - **universal-tier cancellable** — Cancel now reaches universal agents (flag → async drain
    deregisters the ephemeral egress key + emits `AgentCancelled`); the run loop polls `control_rx`
    while universal agents are live (fixing a review-caught starvation of universal-only Cancels).

## [v0.101.0] - 2026-07-25

### Added / Testing
- **ci.2 — close the AUDIT-v0.97 test blind-spots** (4th increment):
  - **P2-8** distro-packaging guard: `distro/Makefile` `cp` now driven from the `docker/*_mcp.py`
    wildcard, + `agentd/tests/distro_packaging.rs` asserts every `/usr/lib/agentos/docker/<x>.py`
    referenced in the distro overlay configs resolves to a real file (fails at the workspace-test
    gate, not QEMU boot). Would have caught both prior distro-bricks (cap.2, ux.12).
  - **P2-7** broker credential attach+drop coverage: extracted a pure `build_upstream_headers()`
    (behavior-preserving) + a test asserting the gateway credential is attached and caller
    `Authorization`/`X-Forwarded-For`/`Cookie` are dropped, for OauthBearer + ApiKeyHeader. A full
    TLS-loopback forward E2E is deferred (ci.2-ar-01 — the gateway is https-only + IP-pinned).
  - **P2-11** in-image sidecar tests: `docker-smoke` now runs each `*_mcp.py --test` with the shipped
    image's python + an `import ssl` check — the in-image python the runner-python job never exercised
    (the lane that hid P1-1 arm64).

## [v0.100.0] - 2026-07-25

### Fixed
- **cap.4 — auth-consistency + capability-scoping** (3rd AUDIT-v0.97 increment):
  - **P2-3 management-API auth** — the ux.12 `X-Approval-Token` gate covered only approve/deny while
    `/spawn`, `/inject`, `/budget/*`, `/agents/*/{cancel,caps}` were ungated on the same `:7999`
    surface. Now the gate (`is_mutating_route` + `approval_token_ok`) covers the **entire mutating
    surface** when `AGENTOS_APPROVAL_SECRET` is set; reads stay ungated; unset stays open. agentctl
    sends the token on all mutations. Additionally, **`/spawn` is deny-by-default on capabilities** —
    without `AGENTOS_ALLOW_PRIVILEGED_SPAWN=1` it mints only read-only-local caps
    (`KbRead`/`FsRead`/`RunsRead`); tools (`Mcp`), network, writes, spawn, run_job, brief-publish,
    credentials, and unrestricted `null` are refused (a denylist was fragile — `Mcp{google_oauth}`,
    not the inert agent-level `Credential`, is the real live-Gmail vector).
  - **P2-5 tool_override KB scoping** — the invoke gate now derives + enforces `KbWrite/KbRead{segment}`
    by tool name (byte-identical to the native tools) in addition to the `Mcp` grant, so segment
    scoping survives semantic-kb's `tool_override`. The injection-exposed cos-inbox job can no longer
    overwrite the curator's brief despite its wildcard `Mcp` grant.

## [v0.99.0] - 2026-07-25

### Fixed
- **run.1 — durability cluster of the AUDIT-v0.97 sweep** (2nd increment):
  - **P1-2 flight.jsonl rotation** — copy-truncate in place at 100 MB (AtomicU64 size counter, under
    the recorder's file mutex, same inode so the otel `tail.rs` sentinel follows it). Best-effort;
    closes the always-on disk-fill that starved the co-located durable writers.
  - **P1-3 short_term cap** — ring-drop oldest paged summaries beyond `MAX_SHORT_TERM` so a
    never-terminating orchestrator's per-turn checkpoint clone stays bounded (short_term is evicted
    summaries, never live tool-call/result pairs). Mid-run distillation of dropped context deferred.
  - **P2-6 cron missed-fire catch-up** — persist next-fire to `/data`; fire once on boot if a fire was
    missed while down. Schedule-fingerprinted (mode + raw cron/interval) so a config change across
    restart cannot trigger a spurious catch-up.
  - **P2-9 runs.redb retention prune** — bound by count/age (5000/90d), closed records only; bounds the
    `list()`/`publish_brief()` full-scan. Time-indexed query re-key deferred.

## [v0.98.0] - 2026-07-25

### Fixed
- **audit.2 — acute batch of the AUDIT-v0.97 remediation sweep** (first of ~7 increments):
  - **P1-1 arm64 CoS was non-functional** — `distro/buildroot.aarch64.config` omitted
    `BR2_PACKAGE_PYTHON3`/`OPENSSL`, so the Python MCP sidecars couldn't run on arm64 and the flagship
    never produced a brief. Mirror the x86_64 package set. (CI's `make -n` dry-run masked it; the
    real-build gap is tracked as ci.2/P2-11.)
  - **P1-class P2-1 checkpoint crash-loop state loss** — restore deleted `checkpoint.json` before the
    first new save, so a deterministic startup crash after restore erased all CoS state. Now rename →
    `checkpoint.json.restored` (recoverable via a `load()` fallback); `save()` consumes the copy on
    success; `load()` quarantines a corrupt source (primary OR `.restored`) to `<name>.corrupt`.
  - **P2-4 ux.13 cancel-resurrection** — a cancelled parked trigger (root awaiting a `run_job` child)
    revived when the child later terminated (flipping `AgentCancelled`→done, spending more). The
    child-delivery re-step is now gated on `!outcomes && !cancel_requested` for the parent.

### Added
- `docs/AUDIT-v0.97.md` — full 8-lane fan-out security+correctness audit (3 P1 · 12 P2 · 16 P3) with a
  prioritized remediation build order. This release begins that remediation.

## [v0.97.0] - 2026-07-24

### Added
- **Control verbs (ux.13)** — the final "trust after absence" cockpit increment. New
  `ControlCommand::{Cancel, SetCaps}` + `AgentCancelled`/`CapabilitiesSet` flight events, reachable
  via management HTTP (`POST /api/v1/agents/{id}/cancel` + `/caps`), the FUSE control file, and
  `agentctl {cancel, set-budget, set-caps}`.
  - **Cancel** stops a runaway/stuck agent (and its spawned subtree) without killing agentd. Guarantee:
    *no new world-affecting dispatch after cancel* — a scheduler-side `cancel_requested` map (not
    checkpointed) plus a gate at the top of `enqueue_or_defer` funnel a flagged agent when its in-flight
    future returns (a running agent stays until then, so nothing panics). Cascade-cancels the
    `parent_map` subtree (skips the `"operator"` root), closes each run as `"cancelled"`, emits one
    `AgentCancelled` per node, and purges the agent from the deferred queue + pending approvals.
  - **SetCaps** narrows a running agent's capabilities without a respawn (revoke/narrow-only
    misconfig-repair, NOT a security response): per-capability `capability_covered_by` (unrestricted
    accepts any narrow; inert caps rejected); the scheduler recomputes the tool specs so the model's
    live tool list actually shrinks. Widening is fail-closed (400 "narrow-only; to widen, respawn").
  - **SetBudget** keeps the ux.11a semantics; ux.13 only adds the `agentctl set-budget` CLI +
    DataSource surface, routed through a ≥3s confirm client (the 3 confirm-channel verbs) so they
    don't spuriously fail on the server's 2s confirm wait.

### Fixed
- **/review P0 (cross-model):** the cancel "parked" predicate used `awaiting.contains_key`, which
  matched a *running* spawned child (a key in `awaiting` while its future is live) → funneling it
  removed it from the agents map and the pending-result arm would panic. Fixed to
  `awaiting.values().any(|v| v.parent_id == node)` (matches the parked *parent*), with two
  deterministic regression tests.

### Docs
- ROADMAP ux.13 built-note; THREAT_MODEL note that the cancel/caps control routes are loopback-trusted
  and intentionally ungated (like spawn/inject/budget) — Cancel only terminates, SetCaps is fail-closed.

## [v0.96.0] - 2026-07-24

### Added
- **Telegram reach (ux.12)** — a two-way Telegram bridge: deliver the morning brief and push pending
  approvals to your phone, and relay **approve/deny** replies to the CoS. New `docker/telegram_mcp.py`
  is a **no-tools stdio MCP server** (empty `tools/list`; a background bridge thread, not an
  agent-facing tool) spawned inside the `cos` container so it can reach the loopback management API.
  Pulled ahead of ux.13 (reach-before-verbs). No remote inject. Wired into both `cos.agents.toml` files
  + `docker-compose.yml` (all optional).
- **Route-scoped approval auth** — `POST /api/v1/approvals/*/{approve,deny}` now require a
  constant-time-matched `X-Approval-Token` header when `AGENTOS_APPROVAL_SECRET` is set (env). Closes
  the unauthenticated-writer exposure that the Telegram bridge would otherwise create on the guessable,
  sequential `act_{seq}` approval ids (see THREAT_MODEL §9.6). Unset ⇒ routes stay open (pre-ux.12);
  full API auth remains ux.5. `agentctl` sends the header from the same env var.
- Sidecar security discipline: `from.id` + private-chat allowlist; **relay-only + re-verify** (re-GET
  pending + args-hash match before POST, closing a deleted-checkpoint `act_{seq}` cross-generation
  collision); `update_id` dedup with a durable offset; fail-closed on POST errors; bot token + approval
  secret never logged. Length-capped `args_json` preview (Telegram is a new egress sink, §8.7).

### Fixed
- **Optional path no longer bricks CoS** — the sidecar is declared unconditionally but now runs
  **inert** (answers the MCP handshake, starts no bridge thread) when `TELEGRAM_BOT_TOKEN` /
  `TELEGRAM_CHAT_ID` are unset, instead of `exit(1)` which failed agentd's MCP boot.
- `distro/Makefile` now packages `telegram_mcp.py` into the QEMU rootfs (the distro config referenced
  it; both caught by `/review`).

### Docs
- THREAT_MODEL §9.6 (Telegram remote approve/deny writer), DEPLOYMENT Telegram setup + the host-side
  `agentctl` approval-secret requirement, ROADMAP ux.12 note.

## [v0.95.0] - 2026-07-23

### Added
- **Orchestrator de-privilege via sealed jobs** (cap.2b) — the REAL closure of audit P1-10 (cap.2
  shipped only the accidental-over-grant floor). New `Capability::RunJob` + `run_job(job_id)` native
  tool (job_id is the only input) + `[[jobs]]` config (`Job { id, capabilities, task, max_turns,
  token_budget }`) + `dispatch_run_job`. A sealed job's capabilities and task template are owned by
  config, NOT the caller: `dispatch_run_job` attaches the job's caps directly (the `capability_covered_by`
  subset check is bypassed soundly — the trust root moved from the injectable parent to config), the
  `{date}` slot is server-stamped (no caller-supplied param), and the child's output is never
  delivered to the caller.
- **`AwaitingParent.deliver_content` delivery gate** — `spawn_agent` (trusted delegation) still
  delivers the child's answer; `run_job` (`deliver_content=false`) delivers only an agentd-authored
  `"job X completed"` / `"job X failed"` signal, never the child's (email-derived) output or a raw
  error string. Checkpoint-safe (`AwaitingEntry.deliver_content`, serde default true so pre-cap.2b
  spawn awaits restore correctly).
- `agentd check` lints `[[jobs]]` capabilities (MCP-server / KB-segment existence, credential wiring,
  FS prefix) and includes them in the `CapabilitiesResolved` boot event.

### Changed
- **CoS pipeline restructured** (dev + distro `cos.agents.toml`): the orchestrator is de-privileged to
  a summary-free cron TRIGGER holding only `{Mcp{cron_trigger}, RunJob}` — no Gmail, Credential, KB,
  FsWrite, BriefPublish, or Spawn. Gmail fetch is the `cos-inbox` job; brief assembly + `FsWrite` +
  `publish_brief` moved into the KB-only `cos-curator` job. `spawn_agent` (cap.2 floor) is unchanged
  for trusted operator-driven delegation.
- The `spawn_attenuation_documents_injection_bypass` test was renamed
  `spawn_agent_floor_is_not_injection_defense` (the floor is unchanged; the sealed `run_job` path is
  what closes injection).

### Notes
- **Audit P1-10 CLOSED** against the pinned claim: *no child obtains live Gmail via injection, and no
  untrusted-data-reading node holds spawn/credential authority.* NOT "injection defeated" — an
  injected curator can still write a misleading brief (integrity, not credential exfil; detective
  controls only). See THREAT_MODEL §9.5; north star is data-taint (recorded, not built).
- Follow-ups: `cap.2b-ar-01` (P3 — sealed-job pipeline date can skew across UTC midnight; fails safe;
  default 08:00 cron unaffected).

## [v0.94.0] - 2026-07-23

### Added
- **Spawn attenuation floor** (cap.2, audit P1-10). `SpawnConfig.capabilities:
  Option<Vec<Capability>>` + a `capabilities` property on the `spawn_agent` tool schema let the
  orchestrator spawn each child with a least-privilege set. Absent = inherit the parent's full set
  (backward compat). `dispatch_spawn` validates every requested cap is covered by the parent and
  **rejects the whole spawn** (reject, not clamp) with a new `AgentSpawnDenied` flight event when it
  is not. The CoS orchestrator now scopes its children — the curator is KB-only (no
  `Mcp{google_oauth}`), so its flight log shows no Gmail tool specs.
- **`capability_covered_by(parent, child)`** — a sound subset predicate in `capability.rs`, distinct
  from `satisfies` (which is a runtime *invocation* check). `Net` and `Mcp` get real containment
  (`satisfies` returned `true` unconditionally for `Net` and vacuously for an empty child `Mcp` tool
  list — both unsound as subset tests); multi-entry `Mcp` grants union per-server. Exhaustive
  no-wildcard match = compile-time drift guard for new `Capability` variants.
- Regression guard `cos_spawn_caps_subset.rs`: pins each CoS config's documented child profiles ⊆
  its orchestrator's capabilities (caught a dev/distro drift where distro children requested a
  `semantic-kb` sidecar the QEMU config does not run).

### Notes
- **Scope (CEO-gated, both models + user):** cap.2 closes *accidental over-grant*, NOT the
  injected-orchestrator prompt-injection threat P1-10 names — the orchestrator holds Gmail and picks
  child caps while reading untrusted email, so an injected orchestrator can grant Gmail from its own
  set and the subset check passes. That bypass is encoded as a passing test
  (`spawn_attenuation_documents_injection_bypass`). **Audit P1-10 stays OPEN**; real closure is
  cap.2b (orchestrator de-privilege). `max_turns` passthrough was cut → cap.2-ar-01.

## [v0.93.0] - 2026-07-23

### Added
- **`agentd check` — capability declaration-surface linter** (cap.1, from the v0.86 audit).
  Catches config that *looks* granted but is inert or wrong — the silent-fail-closed /
  Gmail-outage class — at test, CI, and container-boot time, instead of a mysterious runtime
  no-op. Checks MCP-server-name existence, KB-segment existence, tier-legality, relative FS
  prefixes, and a **credential wiring cross-check** (a `Credential{provider}` granted to an
  agent but carried by no stdio MCP server → the broker token is empty → every call fails
  silently — the actual historical Gmail bug). `--strict` (used at container boot) elevates
  relative FS prefixes to hard errors; default mode warns.
- One shared **`tier_legality`** resolver (in `capability.rs`) decides which
  (capability × context: agent / stdio-MCP / HTTP-MCP) pairs are enforced vs inert — used by
  both the linter and the new **`CapabilitiesResolved`** boot event (logs each agent's +
  server's effective set). A no-wildcard match makes a new `Capability` variant fail to
  compile until its tier legality is declared (drift guard).
- Reject bare `agent` / `agent/` / `agent:` KB-segment grants (they defeat per-agent memory
  isolation — audit-C2 / P1-11).

### Changed
- The container entrypoint (`cos)` mode) now runs `agentd check --strict` on the rewritten
  config instead of `grep` guards — real parsing, fail-closed on a mis-wired grant.
- Framing (reviewed): under the single-tenant, mutually-trusting constitution this is
  **misconfiguration ergonomics**, not defense against a malicious agent; it unblocks cap.2
  (spawn attenuation). Fixed two misleading `cos.agents.toml` comments that claimed the inert
  agent-level `Credential` grant was load-bearing.

## [v0.92.0] - 2026-07-22

### Added
- **Morning brief** (ux.11c): the Chief of Staff now publishes a durable daily brief the
  operator can pull at any time — `agentctl brief` and `GET /api/v1/brief` — so waking up
  and reading "what happened overnight" needs no `runs_query` typing and no flight-log
  reading. Reframed at plan review from a live chat-rail push to a **pull** surface after
  both review voices found the rail is a lossy live stream with no replay-on-attach (a 6am
  brief would be gone by the 9am attach — push and absence are mutually exclusive).
- agentd **authors the brief's facts deterministically** from `runs.redb` (a new `BRIEFS`
  table + `publish_brief` composer): run count, failures, spend, and the failing/blocked
  run IDs — windowed by run **completion** (not start time), so a run that started before
  the window but failed inside it is never silently dropped. The model contributes only
  optional narrative color and cannot fake the facts.
- **`agentctl brief`** renders attention-first: `📋 1 failed · 2 need approval · 12 runs`,
  then the failing/blocked lines with run + agent IDs, then `✓ N others ok`; a quiet night
  states `Quiet night — 0 runs` explicitly (a present brief is the liveness signal). "N need
  approval" is overlaid **live** from the scheduler snapshot, not the at-compose-time record.
- **`publish_brief` native tool** behind a new **`Capability::BriefPublish`** (granted to the
  CoS) — the brief is an operator trust surface. `BriefWritten` flight event (informational).
- ux.12 (Telegram) is now a pure consumer of `GET /api/v1/brief`.

## [v0.91.0] - 2026-07-22

### Added
- **Durable run history** (ux.11b-substrate): agentd now records a per-segment run
  record for every agent lifecycle in a new `runs.redb` — `{agent_id, segment_seq,
  parent_id, start_reason, start/end, status, stop_reason, last_error, approvals_count,
  spend}`. Records are authored from **authoritative in-process scheduler transitions**
  (config-seed / child / operator / universal spawn → terminal), never derived from the
  best-effort flight log, so a dropped event can't drop a run. Per-segment spend is
  Δ`context_tokens()` (native tier; `null` for proxy-metered universal agents).
- **`runs_query` native tool** (new `Capability::RunsRead`, granted to the CoS) lets an
  agent ask "what happened overnight?" over the run store — filter by
  `agent_id`/`parent_id`/`status`/time window, newest-first.
- **`GET /api/v1/runs`** — read-only run history for `agentctl`/operators, same filters.

### Changed
- Run writes go through an off-loop `mpsc` writer task (a single writer, `spawn_blocking`)
  so recording never stalls or crashes the scheduler — best-effort, like the flight
  recorder. The writer is drained at shutdown so terminal runs never leak as `running`;
  a clean shutdown closes any still-open segment as `interrupted`.

### Notes
- ux.11b was **split** at its autoplan gate into this substrate (run store + query) and
  ux.11c (the catch-up digest + CoS-written morning brief). Checkpoint `FORMAT_VERSION`
  untouched; `runs.redb` is a new, separate, versioned store — additive and rollback-safe.
- Deferred (see TODOS): FUSE `/agents/runs` (`ux.11b-ar-02`), mid-run park/resume segment
  boundaries (`ux.11b-ar-01`), `runs.redb` retention/prune (`ux.11b-ar-03`).

## [v0.90.0] - 2026-07-21

### Added
- **Per-agent spend is now visible** (ux.11a): each agent's **windowed** spend
  (spend within the current budget window) is surfaced on the management-API
  snapshot, as a new FUSE file `/agents/<id>/windowed_spend`, and rendered in the
  agentctl TUI budget cell (`47k/100k`, `1.2M/2.0M`, or `47k spent` when
  unlimited — width-bounded to the column).
- **Runtime per-agent budget control** — `POST /api/v1/budget/set`
  `{"target":{"agent":"<id>"},"limit":<u64>}` (`limit:0` = unlimited) sets an
  agent's token ceiling live: reports `old_limit`→`limit`, revives the agent
  immediately if the raise gives it room, and 404s an unknown agent. Also exposed
  on the `/agents/control` FUSE surface as `{"set_budget":{...}}`. The change
  mutates the checkpointed budget, so it survives a restart. **Per-agent only** —
  the global ceiling is immutable config and returns 400.
- **`BudgetSet` flight-recorder event** on every runtime budget change.

### Changed
- **The `BudgetRisk` attention signal now keys on windowed spend**, not lifetime
  spend — so it clears and re-arms across budget-window resets instead of latching
  on forever after the first window. Unlimited agents (`token_budget == 0`) never
  fire it.

### Notes
- ux.11 was **split 2-way** at its autoplan CEO gate: this release is the
  budget-visibility half (closes the ux.8′ P1 "visible + settable spend" debt).
  Trust-after-absence (durable run store + digest + morning brief) is ux.11b,
  landing on its own gate. No breaking changes — the new snapshot field is
  additive and checkpoint `FORMAT_VERSION` is untouched.

## [v0.89.0] - 2026-07-21

### Fixed
- **A budget-capped agent bricked the whole process instead of pausing**
  (ux.8′, P0-2). When lifetime token spend hit `global_token_budget`, the
  scheduler terminated the agent — under the long-lived CoS this meant the
  assistant died on day 2 and never came back. Enforcement moved to admission:
  an over-budget agent now **defers** (process stays alive, work is held) and
  resumes automatically when its window rolls over. The self-brick is gone.

### Added
- **Rolling budget windows** (ux.8′): `[scheduler] budget_reset_interval`
  (seconds; `0` = legacy lifetime enforcement, the default) caps spend per
  window instead of over the process lifetime. The window rebases on wall-clock
  — at the top of the scheduler loop and on a 60s idle tick — using
  division-based catch-up so a long sleep advances the correct number of whole
  windows in one step (no loop-spin). The token meter is monotonic and never
  zeroes: windowed spend is `lifetime − window_anchor`, so the lifetime
  accounting the flight recorder and receipts depend on stays intact across
  every rollover.
- **Manual budget reset** — `POST /api/v1/budget/reset` with
  `{"target":"global"}` or `{"target":{"agent":"<id>"}}` rebases the window
  anchor to current spend (clears the ceiling without destroying the lifetime
  meter), reports `spent_before` → new `window_start`, and returns 404 for an
  unknown agent. Also exposed on the `/agents/control` FUSE surface as
  `{"reset_budget":{...}}` (fire-and-forget).
- **`BudgetReset` flight-recorder event** — every automatic rollover and manual
  reset is recorded with the window it opened and the spend it forgave.
- CoS configs now run a 24h budget window (`budget_reset_interval = 86400`):
  the shipped distro overlay caps at `global_token_budget = 50_000_000`/day, the
  repo dev default at `10_000_000`/day.

### Notes
- Checkpoint `FORMAT_VERSION` stays **4** — the new fields
  (`window_anchor`, `budget_window_start`, `global_window_anchor`) are additive
  with serde defaults, so a v0.89.0 checkpoint still loads on v0.88.0 (the
  window simply reads as unset). Rollback-safe.

## [v0.88.0] - 2026-07-18

### Fixed
- **`AllowNetConnect` TCP port enforcement never worked on Landlock-active
  kernels** — the sandbox passed rule type `3` to `landlock_add_rule`, but the
  kernel ABI defines `LANDLOCK_RULE_NET_PORT = 2`, so every net-port rule was
  rejected with `EINVAL` and `compile()` failed outright on kernels ≥ 6.7 with
  Landlock enabled (it only "worked" where Landlock was inactive and the
  sandbox degraded). Found by the very first CI run of the sandbox test suite
  on a real Linux runner — the exact artifact-vs-source gap this release's CI
  overhaul exists to close.

### Added
- **CI now tests the artifact, not just the source** (ci.1). Every PR builds the
  real Docker image and boots it four ways: a credential-free CoS dry-run, an
  agent dry-run that must render the requested template, a binary error probe,
  and a negative-control fixture that must *refuse* to boot with the offending
  line named — the PR-#124 "relative path survives the boot rewrite" class can
  no longer land silently.
- **A nightly end-to-end run of the shipped image against a mock provider** —
  a real agent cycle (tool call included) at zero API cost. The mock dispatches
  on request content, self-tests in CI, and refuses wrong endpoints, so a
  regression in the agent loop, tool plumbing, or capability checks surfaces
  the next morning instead of at a user's machine.
- **Release guards that make bad publishes refuse instead of shipping**
  (`scripts/release-guard.sh`): a tag must be on `main`, match Cargo.toml,
  exceed every prior version, and target an unpublished version — probed
  fail-closed (auth failures and network errors abort rather than pass) across
  all three version manifests, with a serialized pre-push re-check closing the
  race between concurrent publishes. A 24-scenario harness
  (`scripts/test-release-guard.sh`) runs on every push, alongside self-tests
  for the mock provider and negative controls proving the sidecar contract's
  failure branch fires.
- **Every bundled MCP sidecar self-tests in CI** — nine `docker/*_mcp.py`
  servers must exit 0 *and* print their `self-test PASSED` marker (either
  alone can lie). `weather_mcp.py` gained the `--test` mode it was missing.
  `make test-harness` mirrors the same contract locally.
- **The QEMU 2-boot continuity test now runs monthly** (was manual-only, and
  red for months without anyone noticing), with a preflight that names the
  missing secret instead of failing mysteriously in-VM, and QEMU stderr
  captured for VM-level diagnosis.

### Changed
- **CI covers the whole workspace**: `surfaces` (96 tests, incl. Linux-gated
  FUSE glue), `sandbox` (35), and `otel` (34) build, lint, and test in CI for
  the first time — with FUSE headers installed up front, root-workspace caches
  that actually hit (the old per-crate paths cached nothing), and the sandbox
  crate added to both aarch64 clippy lanes so arch-conditional regressions
  can't ship uncompiled.
- **Tag publishes now wait for the sidecar and harness test jobs** — a red
  self-test blocks `:latest` instead of riding along inside it.
- Docker/QEMU boots share the same env denylist: `distro/overlay/init` mirrors
  the entrypoint's `GREP_OPTIONS`/`POSIXLY_CORRECT`/guard-bypass filtering.
- `docs/DEPLOYMENT.md` documents every guard refusal with its remediation,
  the required-status-check setup, and the release operating rules (linear
  versioning, tag spacing, safe re-run paths).

## [v0.87.0] - 2026-07-17

### Fixed
- **The default QEMU boot config could never boot** — `distro/overlay/etc/agentd/agent.toml`
  used `model_id`, a key that has never existed on `ModelConfig` (`deny_unknown_fields`),
  so every non-CoS QEMU boot panicked PID-1 at config parse (audit86-P0-1). Renamed to
  `model`; a new parse-all test keeps every checked-in config bootable from now on.
- **The librarian-semantic template told operators to set the wrong API key** — its
  `[gated]` badge and spawn warning named `VOYAGE_API_KEY` while its sidecar
  (`semantic_kb_mcp.py`) reads `OPENAI_API_KEY`, so following the instructions produced
  an agent whose every `kb_put` fails. Badge, warning, card wording, and
  `docs/MCP_SERVERS.md` (full env table + two dead `--profile semantic` commands) now
  match the code. A token-consistency test fails if a template ever names an env var
  that appears nowhere in the product's sources (`docker/`, `agentd/src`).
- **A missed path rewrite at container boot now refuses to boot instead of failing
  silently at runtime** — the `cos)`/`agent)` sed pipeline gained general negative
  assertions (both quote styles, positive-form path-key check, args-line anchoring) that
  name the surviving line, both remediations, a credential-free repro command, and an
  `AGENTOS_SKIP_PATH_GUARDS=1` escape hatch. This kills the v0.86.2 bug class (silent
  `capability_denied` discoverable only inside a running container).
- **Boot-guard hardening from adversarial review** — a task prompt quoting a repo path
  (e.g. `"../docker/x"`) no longer bricks the boot (guards are line-anchored, never
  whole-file over user text); the sed rewrite no longer corrupts task text that mentions
  "args"; guard grep errors fail the boot instead of passing silently; behavior flags
  (`DRY_RUN_ONLY`, `AGENTOS_SKIP_PATH_GUARDS`) and grep-behavior vars can no longer be
  injected via the secrets file.

### Added
- **`agentd/tests/config_parse_all.rs`** — every checked-in agent-spec TOML (docker/,
  agentd/, distro overlay) is parse+validate+lowering-proven in `cargo test`, with
  negative-control fixtures (unknown key, insecure HTTP server, duplicate agent id).
- **`agentd/tests/repo_consistency.rs`** — CLAUDE.md's canonical `**Current version:**`
  line is test-enforced against `agentd/Cargo.toml` (a stale status line now fails the
  build — closes cred.3.2-ar-02 after three drift recurrences), and template
  `gated_requires` env vars must exist in product sources.
- **`cos` dry-run mode** — `docker run --rm -e DRY_RUN_ONLY=1 <image> cos` verifies the
  config rewrite + guards with zero credentials; documented in DEPLOYMENT.md
  ("Verifying config changes") alongside the `agent`-mode dry run.

### Changed
- CHANGELOG reordered newest-first (v0.86.1/v0.86.2 had been appended below v0.86.0).
- TODOS.md: six long-fixed entries verified against code and struck (audit-S1/S2,
  F-012, F-015, cred.3-ar-02, cred.3.1-adv-01); build order updated — ux.8 (budget
  truth) now ships before ux.10 (TUI polish) per the audit.1 review-gate decision.

## [v0.86.2] - 2026-07-16

### Fixed
- **CoS orchestrator's `write_file` calls were silently denied in Docker despite
  a correctly-configured `FsWrite` grant** — `docker/entrypoint.sh`'s `cos)` sed
  pipeline rewrote the `FsWrite` capability grant (`prefix = "./output"` ->
  `/data/output`) but never touched the matching `write_file(path='./output/...')`
  instruction baked into the same TOML's task prompt. At runtime the grant became
  absolute while the LLM was still told to call `write_file` with a relative path,
  which can never satisfy an absolute-prefix capability check under
  `agentd/src/capability.rs`'s `satisfies()`. Root-caused via live `flight.jsonl`
  `capability_denied` events, not the orchestrator's own self-report (which
  claimed a vague "operator should enable FsWrite" — a misdiagnosis of the same
  class as v0.86.1's Gmail bug). Fixed by extending the sed pipeline to rewrite
  the prompt instruction alongside the grant, plus a fail-fast startup guard that
  now exits with a clear error if this literal ever desyncs again instead of
  denying writes silently. `/review` (adversarial pass) confirmed exact
  single-match correctness with no collateral rewrite side effects; `/qa`
  verified live in Docker against real Gmail — a real brief landed on disk with
  zero `capability_denied` events post-fix.

## [v0.86.1] - 2026-07-15

### Fixed
- **CoS Inbox agent could never read Gmail despite a valid, broker-managed OAuth session**
  — every Gmail API call was rejected with 403 `credential_denied`/`no_providers_configured`.
  Root cause: `agentd/src/main.rs`'s credential proxy token derives its `allowed_providers`
  list from the `google_oauth` MCP **server's own** `capabilities` field, not the owning
  agent's — `cos.agents.toml`'s `google_oauth` server only granted `Net` access, so the
  broker registered an empty `allowed_providers` regardless of the OAuth token's validity.
  Fixed in both `agentd/cos.agents.toml` and the distro overlay copy. `/review` on the fix
  (security + adversarial passes, both clean) confirmed it's minimally scoped and
  non-exploitable, and found a real testability gap: the pre-existing regression test
  hand-duplicated the credential-derivation logic instead of exercising the real function —
  extracted to `credential_allowed_providers()`, called from both the production closure and
  the tests (now including an end-to-end check against the real config files). Two related,
  pre-existing findings (Curator's over-broad Gmail credential inheritance via spawn; three
  older templates still on the pre-cred.6 raw-secret pattern) filed as `cos-dev-02`/`03` in
  `TODOS.md`, not fixed in these commits.
- **`docs/cos-guide.html`'s Step 06 CoS-start instructions were broken** — following them
  exactly (a bare `docker run ... cos`) failed immediately with a DNS lookup error for
  `semantic-kb-mcp`, since h8.1/memory-routing made the semantic KB sidecar a Compose-only
  dependency. Replaced with `docker compose up cos`.
- **Local Qdrant healthcheck failed permanently** — `qdrant/qdrant:v1.13.6`'s image has no
  `curl`/`wget`/`nc`, so the `CMD curl` healthcheck in `docker-compose.yml` always failed at
  the exec step, blocking `docker compose up cos` on "dependency unhealthy" even though Qdrant
  was actually serving. Switched to a pure TCP probe via bash's `/dev/tcp`.
- 1422 workspace tests (+2 versus the ux.1 baseline above: the google_oauth capability guard
  from the credential fix, plus the end-to-end regression test added during its `/review`).

## [v0.86.0] - 2026-07-13

### Added (ux.1 — Converse)
- **Permanent chat rail on `agentctl watch`'s Dashboard view** (agent table `Min(72)` |
  rail `Length(32)`), honoring the project's locked D1 "one unified screen" decision
  instead of a 10th full-screen tab, as the rough scope originally proposed.
- **`agentd/src/events.rs`**: `EventKind::InferenceStreamDelta` — one text chunk of a
  streaming inference response, recorded per-chunk on the hot streaming path
  (`scheduler.rs`'s `make_infer_future`/`print_fut`) so remote SSE subscribers (the chat
  rail) see live token-by-token output. Previously, streamed chunks were written only to
  `agentd`'s own local stdout and never reached `/api/v1/events` — a false premise the
  original plan assumed away, independently confirmed by three separate traces (manual +
  Claude subagent + Codex) during `/autoplan` Eng review.
- **`agentd/src/flight_recorder.rs`**: `FlightRecorder::record_streamed()` — broadcasts
  the full chunk text live over SSE while capping the `flight.jsonl` disk copy at 256
  bytes (`STREAM_DELTA_DISK_TEXT_CAP`), preserving the log's existing preview/audit-metadata
  contract rather than turning it into a full model-output transcript store.
- **`agentctl/src/watch/converse.rs`** (new): `ConverseState` per-target state machine
  (Idle → Dispatching → Streaming → flush), `ConverseView` (`HashMap<AgentId,
  ConverseState>` — per-target, so a backgrounded conversation keeps streaming while
  another is focused), `dispatch()` (spawn-or-resume, ported verbatim from
  `orchestrate.rs`), and the four terminal-event field-path lookups ported byte-for-byte
  rather than re-derived — `orchestrator_exited`'s top-level `agent` field is a hardcoded
  literal `"agentd"` (only `data.agent_id` is valid), and `agent_failed` has no
  `data.agent_id` at all. 64KB `current_reply` cap, 200-turn history ring, chunk_seq gap
  detection (dropped-chunk note, not silent splicing), 30s client-side dispatch timeout.
- **`Tab`** toggles rail focus (reusing `Memory`/`Spawn`'s existing sub-pane-cycling
  idiom); **`r`** retargets the rail to the selected table row's agent. `[c]` stays bound
  to Credentials — the rough scope's original `[c]`-for-chat proposal collided with the
  already-shipped Credentials hotkey (cred.5, v0.68.0), caught during Design review.
- `agentctl orchestrate`'s CLI gains a cheap early-continue on `inference_stream_delta`
  events in `drain_until_turn_complete` (T10) so it skips wasted work per chunk, but it
  still does NOT consume the delta stream for display and does NOT call `converse.rs`'s
  `dispatch()`/`on_flight_event()` — it kept its own duplicated spawn/inject logic and
  still block-then-prints the server-capped 512-char `orchestrator_turn_complete.answer`
  field, exactly as before this branch. The plan originally called for both (see
  `docs/plans/ux.1-converse.md`'s Pass 5), but neither landed in the diff — caught by
  `/ship`'s Step 8 plan-completion audit, which found this changelog entry had claimed
  otherwise. Filed as TODOs rather than fixed under ship-time pressure; see `TODOS.md`'s
  ux.1 section.
- `docs/INTERFACE.md` §3 annotated as superseded-by-shipped-implementation (stale
  number-key/tab-bar keymap sketch, predates Phase 6).
- **`/autoplan` review found and fixed two critical issues before implementation**: (1)
  this branch was cut ahead of `ux.2a-attention` in violation of the roadmap's own
  sequencing — paused, merged ux.2a first, re-cut clean; (2) the live-streaming premise
  above. `/review`'s adversarial pass (Codex + manual critical pass) then caught and fixed
  five more real bugs post-implementation: acceptance criterion 3 (scroll/follow — the
  transcript had no scroll offset at all, meaning it silently clipped at the top on any
  conversation longer than the visible area) was never implemented; a missing
  double-submit guard (a second `Enter` mid-turn would call `dispatch()` again and,
  since the agent is no longer `"waiting"` server-side, attempt to re-spawn the SAME
  `agent_id` instead of injecting); `Tab` unconditionally focused the chat rail even when
  a narrow terminal hides it, silently swallowing every keystroke into an invisible input
  box; two `state.history.push_back()` call sites in `mod.rs` bypassed the 200-turn ring
  cap and unread counter by writing to the `VecDeque` directly instead of through
  `ConverseState::push_history()`; and a duplicated `200` `max_turns` magic-number literal
  (independently matching `orchestrate.rs`'s own CLI default by coincidence, not by
  reference) was unified into `converse::DEFAULT_MAX_TURNS`. A separate adversarial
  subagent pass then caught 5 more, sharper bugs: a **critical panic** — byte-index
  slicing at the 64KB truncation cap (`&text[..remaining]`) crashes the whole TUI the
  moment the cutoff lands mid-UTF8-character (em dash, smart quotes, emoji — all common
  in normal model output), fixed with a stable-Rust `floor_char_boundary` walk-back; the
  30s dispatch timeout was measured from dispatch *start*, not last activity, so any turn
  streaming longer than 30s total got killed mid-stream and its real reply silently
  discarded — fixed by refreshing `last_event_at` on every delta instead of only at
  dispatch time; deltas were validated only by `chunk_seq` (which resets to 0 every turn)
  with no `turn_seq` check, allowing a stale/late delta from a previous turn to be spliced
  into a new one — fixed by rejecting any delta whose `turn_seq` doesn't match the turn
  currently accumulating; a stray delta arriving after `flush()` unconditionally reopened
  `Streaming` with no `last_event_at` set, permanently wedging the target behind the
  double-submit guard with no timeout escape — fixed by rejecting deltas while `Idle`; and
  the `▼ N new` unread counter incremented once per delta CHUNK instead of once per
  logical reply, showing "▼ 200+ new" for a single streamed message — fixed by gating the
  bump to the first delta of each turn. Full plan + dual-voice review trail:
  `docs/plans/ux.1-converse.md`.
- Two architectural findings from the adversarial pass — `dispatch()` blocking the whole
  TUI for up to ~8s worst case, and the shared SSE broadcast channel now carrying much
  higher-frequency traffic — are real but bounded (not correctness bugs) and filed as
  TODOs rather than fixed in this pass; see `TODOS.md`'s ux.1 section.
- **Interactive QA against the real compiled binaries then found one critical bug the
  entire review pipeline above had missed**: typing the letter `q` into the focused chat
  rail quit the whole TUI mid-keystroke — `handle_dashboard_key` correctly captured it as
  literal input, but `step_key`'s outer "`q` quits the Dashboard" check didn't know about
  rail focus and fired anyway. Fixed by gating that check on `!rail_focused`. Also fixed:
  the `Tab` handler's chrome-row estimate and `render_dashboard`'s own layout constants
  were two independently-maintained copies of the same literal — unified into
  `views::dashboard_chrome_rows()`.
- **`/ship`'s own review pipeline (Steps 7-11) then found and fixed 6 more real bugs on
  top of all of the above** — a coverage audit found `dispatch()`'s "target already
  waiting → inject, not spawn" branch had zero test coverage anywhere (every caller used
  a mock reporting an empty agent list); a review-army specialist pass found the Enter
  handler's `Ok(resolved_id)` arm used `get_mut` instead of `entry().or_default()`,
  silently dropping all state (and permanently discarding the operator's just-sent
  message) whenever the server resolves a different agent id than requested; a red-team
  pass then found the entire chat rail was silently, completely non-functional whenever
  `agentctl watch` runs over FUSE instead of `--url` HTTP (the default local mode on
  AgentOS's own target Linux platform) — `FuseSource` supports neither `spawn()` nor
  `event_stream_url()`, so every message either failed outright or hung at
  "Dispatching..." for the full 30s timeout, forever, since this session's own QA only
  ever exercised `--url` mode. Fixed with a capability gate mirroring `orchestrate.rs`'s
  existing `event_stream_url()` check. A cross-model adversarial pass (Claude subagent +
  2 independent Codex passes, all converging on the same core defects) then found: the
  64KB `current_reply` cap didn't actually cap anything past the first overflow (it kept
  re-appending its own truncation marker on every subsequent chunk, growing unboundedly);
  `orchestrator_turn_complete` was discarding the full text already accumulated via
  streaming deltas and using the server's 512-char preview instead, collapsing every long
  reply back down to a snippet the instant it finished; resizing the terminal below the
  rail's fit floor hid the rail but left it focused, silently swallowing keystrokes; and
  the rail's scroll offset counted logical turns instead of wrapped visual rows, pinning
  long streamed replies to the top instead of following the live tail. Plan completion
  audit and CEO scope decisions filed 8 further findings (dispatch-collision error
  messages, cross-turn guards not surviving an `agentd` crash, unbounded `ConverseView`
  growth, a still-unshared `orchestrate.rs`/`converse.rs` helper, and others) as TODOs —
  see `TODOS.md`'s ux.1 section for the complete list.
- 1420 workspace tests total (+52 new versus the pre-`/ship` baseline: coverage-audit,
  review-army, and adversarial-pass regression tests across agentd's
  `scheduler`/`flight_recorder` and agentctl's `converse`/`mod`/`views`); otel's
  `event_kind_coverage` exhaustiveness guard updated for the new `EventKind` variant.

## [v0.85.0] - 2026-07-13

### Added (ux.2a — Attention)
- **Outcome/risk signals on the cockpit Dashboard**: `AttentionReason` enum (`ApprovalPending |
  Degraded | BudgetRisk | EvaluationUnavailable`, declaration order doubles as tie-break/
  routing priority) + `AttentionSignal` struct (`surfaces/src/snapshot.rs`), added to
  `AgentSnapshot` and served over both FUSE (`/agents/<id>/attention`, new `OFF_ATTENTION`
  offset) and the management HTTP API (reused `AgentSnapshot`'s `Serialize` impl).
- **`derive_attention()`** (`agentd/src/scheduler.rs`): computes Approval-pending (reads
  `pending_approvals` directly, not the `.take(100)`-capped snapshot vector), Budget-risk (via
  `memory::context::assess()`), and Degraded (fires on `!token_fresh` OR
  `attention_reason.is_some()`, closing a gap where cred.7's health-state-machine flags for
  ApiKey providers were invisible while `token_fresh` stayed true) signals from already-existing
  scheduler/credential state — no new instrumentation.
- **Dashboard rendering** (`agentctl/src/watch/views.rs`): new `ATTN` column, always-visible
  summary line ("N need attention · M unavailable"), stacked reason line per flagged agent,
  persistent `AgentDetail` attention strip, `--plain` markers + reason text — reused glyph/
  classification logic (`classify_attention`, `attention_glyph_and_style`) shared by all three
  render paths so they can't drift apart.
- **Actionability-driven Enter-key routing** (`agentctl/src/watch/mod.rs`): `ApprovalPending` →
  `View::Approvals`, `Degraded` → `View::Credentials`, else → `View::AgentDetail` — a
  deliberately separate axis from severity-driven row color (Approval always wins routing even
  though Degraded is more severe).
- Reframe of the original "Observe" plan (`docs/plans/ux.2-observe.md`, preserved for
  reference) toward outcome/risk signals per CEO dual-voice review; full CEO/Design/Eng/DX
  review in `docs/plans/ux.2-attention-evidence.md`. Does **not** close `cos-ux-01` — the Idle
  signal needs new `AgentTask` fields that don't exist yet, deferred to **ux.2b**.

### Fixed (ship-review findings — 3 rounds of dual-voice adversarial review)
- Two CRITICAL bugs from the initial `/review` pass: `AttentionSignal.since` was rendered as a
  raw Unix epoch instead of elapsed time; `EvaluationUnavailable` was dead code, never actually
  constructed, so a failed FUSE/HTTP read silently degraded to "clean" instead of a distinct
  not-evaluated state.
- FUSE `lookup()` had no match arm for `"attention"` under `ParentKind::AgentDir` — `readdir`
  listed the file but opening it by path on a real FUSE mount returned ENOENT, making the
  feature unreachable via the primary native transport. The identical, pre-existing gap for
  `"credentials"` (cred.5) was fixed in the same pass.
- `BudgetRisk`/`EvaluationUnavailable`(config-drift)/`Degraded`(never-tracked-onset) signals
  recomputed `since: now` (or fell back to it) on every scheduler tick — displayed as a
  misleading "0s ago" for potentially long-standing issues. Now render as "active" via a
  shared `age_display()` helper.
- `read_agent_attention`'s `read_trimmed()` collapsed every `io::Error` (not just `NotFound`)
  to `None` → Clean, contradicting the documented "never silently collapse to Clean"
  guarantee. New `read_trimmed_checked()` distinguishes error kinds.
- `--plain` output concatenated the attention marker directly against the status bracket
  (`"[OK][failed]"`) with no separator — a positional-parser regression risk for the CI/
  non-TTY mode this format exists for.

### Deferred (logged in `TODOS.md`)
- `filter_agent_id` on `ApprovalsViewState` (P1, upgraded from P2): Enter-routing to Approvals
  resets to `selected_idx: 0` instead of the specific approval, which can highlight the wrong
  agent's request when 2+ approvals are pending simultaneously.
- `Vec<AttentionSignal>` deserialization is all-or-nothing (P2): a single unrecognized future
  reason (from ux.2b, not yet built) would wipe all signals for an agent. Not exploitable
  until ux.2b adds new variants.
- ux.2b (P1): Idle + Error attention signals, closes `cos-ux-01` fully.

1377 workspace tests (up from 1327 at v0.84.0), clippy clean (including `make clippy-linux`
for the Linux-gated FUSE changes).

## [v0.84.0] - 2026-07-13

### Added (cred.7 — credential resilience)
- **3-way failure classifier** (`agentd/src/credential/mod.rs`): `RecoveryKind` enum
  (`Reauth | ConfigFix | SecretReplace`) + `FailureClass` enum (`Retryable |
  AttentionRequired { recovery_kind }`). `CredentialError` struct replaces bare `String` as
  the `get_or_refresh()` error type — OAuth error body inspection maps `invalid_grant` /
  `invalid_client` / `token_expired` → `Reauth`; other HTTP errors → `Retryable`.
- **Per-provider health state machine**: `ProviderHealthState` enum (`Healthy |
  AttentionRequired { recovery_kind, reason, since }`) stored on `GatewayState`. Health
  transitions on every `handle_credential_request()` — success → Healthy, attention error →
  AttentionRequired. Exposed in `CredentialGateway::snapshot()` via 3 new `ProviderHealth`
  fields (`attention_reason`, `recovery_kind`, `attention_since` — all `skip_serializing_if =
  "Option::is_none"`).
- **Proactive OAuth refresh background task**: one `tokio::spawn` per OAuth provider in
  `CredentialGateway::start()` running `proactive_refresh_loop()`. Wakes 5 min before expiry
  (`PROACTIVE_REFRESH_LEAD_SECS = 300`), calls `get_or_refresh()` to renew the token before
  the agent needs it. On contention (`try_peek_expiry()` returns `None`), sleeps 5 min and
  retries.
- **`POST /api/v1/credentials/<provider>/reset-attention`** management API endpoint: clears
  health state to Healthy, invalidates cached token, clears last error, emits
  `CredentialRecovered` flight event. Returns `{"reset": "<provider>"}` on success, 404 on
  unknown provider, 503 when no credential gateway is configured. Provider name validated
  against configured-providers allowlist (path-traversal protection).
- **Checkpoint persistence of provider health** (`agentd/src/checkpoint.rs`):
  `ProviderHealthCheckpoint { recovery_kind, reason, since }` struct; `credential_health:
  HashMap<String, ProviderHealthCheckpoint>` on `SchedulerCheckpoint` with `#[serde(default)]`
  for backward compat (FORMAT_VERSION stays at 4). Early checkpoint peek in `main.rs` restores
  health state before `CredentialGateway::start()`.
- **`credential_attention_required`** and **`credential_recovered`** flight event kinds
  (`agentd/src/events.rs`, `docs/CONVENTIONS.md`).
- **Re-auth path for `agentctl auth google`** (`agentctl/src/auth/google_device.rs` +
  `util.rs`): reads existing `google.json` when no CLI credentials provided; `--force` guard
  only applies to new credentials; `write_secrets_file_ext()` preserves `token_url` across
  re-auth; `sync_all()` in `write_state_atomic()` for crash-safe OAuth state writes.

## [v0.83.0] - 2026-07-13

### Changed (cred.6 — CoS broker migration)
- **CoS migrated to credential broker mode** (`agentd/cos.agents.toml` +
  `distro/overlay/etc/agentd/cos.agents.toml`): the `google_oauth` MCP sidecar now holds **no
  raw refresh token in memory-at-rest**. `OAUTH_CLIENT_SECRET` and `OAUTH_REFRESH_TOKEN` are
  removed from `google_oauth`'s `passenv`; the Rust credential gateway reads `google.json`
  directly and issues access tokens. `FsRead /run/secrets` capability removed from `google_oauth`;
  `Credential{Google}` grant added to the orchestrator so spawned inbox agents route through the
  broker.
- **`passthrough_query_params` allowlist** (`agentd/src/config.rs`,
  `agentd/src/credential/mod.rs`): new `Vec<String>` field on `ProviderConfig` — per-param
  allowlist for query string forwarding. Default empty = no params forwarded (preserves D3
  injection prevention). CoS Google provider configured with `["maxResults", "q", "format",
  "pageToken", "includeSpamTrash"]` so Gmail API calls work through the broker.
- **`state_path = "/run/memory/oauth/google.json"`** in both CoS configs: OAuth access-token
  cache written to the writable 9p memory mount (`/run/memory`) in both Docker and QEMU modes.

### Fixed (review pass)
- **T35b self-reference** (`agentd/src/credential/mod.rs`): guard string now constructed from
  parts to check the filter idiom specifically, preventing the test from passing even if the
  allowlist logic is removed from `handle_credential_request`.
- **`passthrough_query_params` non-empty assertion** added to `cos_config_broker_mode_and_no_fs_read`
  — catches accidental removal of Gmail query params from either cos config.
- **Stale `--profile semantic` comments** removed from `docker-compose.yml`; updated to reflect
  that qdrant and semantic-kb-mcp are always-on with no profile gate.
- **`EMBED_DIM` comment** in `docker/semantic_kb_mcp.py` corrected: defaults to `0` (not `1536`)
  for unknown models, with a startup warning.

## [v0.82.0] - 2026-07-12

### Added (ux.9 — Cockpit mode)
- **`cockpit` entrypoint mode** (`docker/entrypoint.sh`): the new zero-arg default for
  `docker run agentos:full` (no command). Cold-starts `agentd` with a minimal, agent-free
  config (`docker/cockpit.toml`) and attaches `agentctl watch` — opening the cockpit shows
  the empty system state, it doesn't spend API tokens on demo work automatically. FUSE is
  used opportunistically when the container is `--privileged`; otherwise `agentctl watch`
  transparently falls back to the management API over HTTP. Requires `-it` (fails fast with
  an actionable message otherwise, instead of hanging).
- **`[scheduler] allow_empty_agents`** (`agentd/src/config.rs`): opt-in config flag letting
  `agentd` cold-start with zero agents (`Config::agent_configs()` returns an empty `Vec`
  instead of erroring) — the mechanism `cockpit.toml` uses to boot empty.
- **`make compose-config-check`**: guards that `docker-compose.yml`'s `cos`/`agent` services'
  explicit `command:` lines keep overriding the image's default `CMD` regardless of what it is.

### Changed
- **BREAKING:** the Dockerfile's default `CMD` changed from `shell` to `cockpit` — a bare
  `docker run agentos:full` (no command) now boots the cockpit TUI instead of a bash shell.
  `docker compose up cos` / `docker compose run --rm agent` are unaffected (both set an
  explicit `command:`). To get the old shell behavior back: `docker run -it agentos:full shell`.
- **`agentctl watch` now installs a SIGTERM/SIGINT handler** (`agentctl/src/watch/mod.rs`):
  fixes every `docker stop`/`kill` against the live TUI leaving the operator's terminal stuck
  in raw mode + the alternate screen (previously relied only on a panic hook + `Drop`, neither
  of which runs on an uncaught signal's default disposition).
- `Dockerfile`'s `runtime-core` stage now installs `curl` — it was silently missing, which
  also affected `orchestrate)` mode's pre-existing cold-start healthz poll (used since v0.66.0).

### Fixed
- **Checkpoint bleed-through**: `cockpit)` now runs `agentd` from `/data` instead of
  `/workspace` (the operator's bind-mounted files directory) and removes any stale
  `checkpoint.json` before each launch — matching `cos)`/`agent)`'s existing pattern. Running
  from `/workspace` would silently restore agents from a prior `demo`/`run`/cockpit session's
  checkpoint on the same mount, spending tokens despite the zero-agent config.

## [v0.81.0] - 2026-07-12

### Added (memory-routing)
- **L2 semantic KB for CoS email bodies** (`docker/semantic_kb_mcp.py`): routes `kb_put` /
  `kb_get` / `kb_search` through the Qdrant-backed semantic sidecar using OpenAI
  `text-embedding-3-small` (1536-dim) embeddings. Inbox agent stores raw email bodies keyed by
  Gmail message ID; on subsequent runs `kb_get` returns the cached body, eliminating the ~820k
  token/run cost of re-fetching all messages from Gmail.
- **`mail:raw` KB segment** declared as `[[memory.segments]]` in `agentd/cos.agents.toml`
  (`class = "scratch"`, last-writer-wins keyed by message ID); `tool_override = true` on the
  `semantic-kb` MCP server block routes all `kb_*` calls to Qdrant when the sidecar is live.
- **`OPENAI_API_KEY` preflight** in `docker/entrypoint.sh` (`cos` mode) — exits with an
  actionable error if the key is absent; added to `docker-compose.yml` `cos` env block.
- **`_EMBED_MODEL_DIMS` dict** in `semantic_kb_mcp.py` — maps known OpenAI embedding models
  to their output dimensions; startup warning when `EMBED_MODEL` is unknown (prevents silent
  Qdrant dimension-mismatch errors when using non-default models).
- **Privacy note** in `docs/DEPLOYMENT.md` — discloses that email body plaintext (up to 8 KB
  per message) is transmitted to OpenAI for vectorisation, with opt-out instructions.

### Security
- **`OPENAI_API_KEY` added to `PASSENV_BLOCKLIST`** (`agentd/src/tools/mcp.rs`): prevents the
  OpenAI key from leaking to stdio MCP subprocesses that declare it in `passenv`.

### Fixed
- **Pre-migration eviction** (`semantic_kb_mcp.py`): points stored before the `ts` field was
  added were silently skipped during TTL eviction. Now treated as maximally old and evicted.
- **`semantic-kb-mcp` on dual Docker network** (`docker-compose.yml`): service is now on both
  `cos-net` and `agent-net` so both the `cos` and `agent` Compose services can reach it after
  the ux.0b network-segmentation change.

## [v0.80.0] - 2026-07-12

### Added
- **`[management] allow_non_loopback`** (Track UX cockpit, ux.0b): explicit deployment opt-in
  (default `false`) that lets `agentd`'s management API bind a non-loopback address; the
  fail-closed guard still refuses `0.0.0.0` for any config that doesn't set the flag.

### Changed
- **`agentctl watch --url http://localhost:7999` now works directly against the Docker `cos`
  container from the Mac host** — no `docker exec`/`docker compose exec` workaround needed.
  `agentd/cos.agents.toml` + the QEMU overlay config both opt in (`bind_addr = "0.0.0.0"` +
  `allow_non_loopback = true`); `docker-compose.yml` publishes the port pinned to host loopback
  (`127.0.0.1:7999:7999`, never bare `7999`). This also fixes a pre-existing bug where the QEMU
  deployment's `0.0.0.0` bind silently failed the loopback guard and the management API never
  started there at all.
- **`docker-compose.yml` network segmentation**: `cos` and `agent` (+ its `semantic`-profile
  sidecars) now run on separate Compose networks (`cos-net` / `agent-net`) instead of sharing
  Compose's single default bridge, so `agent`'s untrusted/web-fetching template workloads can't
  reach `cos`'s unauthenticated management API over the network.
- `docs/DEPLOYMENT.md` and `docs/RUNBOOK.md` updated to the new direct-host-URL workflow.
- `docs/THREAT_MODEL.md` documents the management API's unauthenticated exposure, the accepted
  deployment-hygiene gaps, and defers per-session auth to the future web-cockpit increment (ux.5).


## [v0.79.0] - 2026-07-12

### Fixed — KB segment visibility at startup (cos-polish #3)

- **`agentd/src/memory/store.rs`** — `set_segment_class` now also registers the
  namespace in the `NAMESPACES` redb table (count=0) when the namespace is not already
  present. Previously, configured KB segments (`ops:briefs`, `ops:entities`) were written
  only to the `META` table; `list_namespaces()` (which reads `NAMESPACES`) returned empty
  until a data write occurred, so FUSE `/agents/kb/` showed no segment directories at
  startup and `agentctl watch [m]` rendered the "no KB data" banner even when segments
  were configured. The `is_none()` guard is idempotent: existing namespaces with data
  (count > 0 in `NAMESPACES`) are never overwritten. 2 new tests:
  `set_segment_class_registers_namespace_before_write` and
  `set_segment_class_does_not_reset_existing_namespace_count`. 1292 workspace tests pass.

## [v0.78.0] - 2026-07-12

### Fixed — CoS config calibration

- **`agentd/cos.agents.toml`** — `kb_put` parameter name fixed: `value=` → `content=` in all 4
  `kb_put` calls (orchestrator STEP 3b and curator STEPs 2–4). The schema enforces
  `additionalProperties:false`; wrong parameter name caused every KB write to silently fail.
- **`agentd/cos.agents.toml`** — KB Segment Reference table added to orchestrator and curator task
  prompts; agents were using storage class names (`log`, `scratch`) as segment names instead of
  the correct values (`ops:briefs`, `ops:entities`), so all KB reads and searches returned empty.
- **`agentd/cos.agents.toml`** — KB log-class key semantics corrected: `ops:briefs` description
  updated to "key arg ignored (runtime assigns hex seq)" — the runtime ignores the caller-supplied
  key for log-class segments and assigns `format!("{seq:016x}")` internally.
- **`agentd/cos.agents.toml`** — STEP 6 now explicitly requires a formatted markdown string for
  `write_file content`; prevents agents from passing a JSON dump as the brief file content.
- **`agentd/cos.agents.toml`** — inbox agent `token_budget` raised `500_000 → 1_500_000`; live
  spend was ~820k tokens, causing mid-run budget exhaustion and truncated briefs.
- **`agentd/cos.agents.toml`** — `global_token_budget = 10_000_000` (was 0/unlimited); provides a hard
  daily spend ceiling across all 3 CoS agents (~3 full cycles at 1.5M inbox + 500k curator).
- **`agentd/cos.agents.toml`** — corrected two inaccurate comments: child-ID collision comment (says
  "terminated children remain in the scheduler's outcomes map" — they do not for awaited children);
  and ops:briefs table entry (claimed "keyed by YYYY-MM-DD" — the log class ignores that key).
- **`distro/overlay/etc/agentd/cos.agents.toml`** — synced all above fixes to the production QEMU
  overlay; added clarifying comment that `global_token_budget = 0` is intentional for always-on
  production (dev config uses 10_000_000 as a daily ceiling).
- **`templates/orchestrator.template.toml`** — `max_turns = 200 → 20_000`; the old value killed a
  cron-based orchestrator before it produced its first brief. Added `checkpoint_interval_turns = 1`.
- **`agentd/src/config.rs`** — 4 new tests guard these invariants: `cos_agents_toml_parses_cleanly`
  (both dev + overlay parse as valid `Config`), `cos_agents_toml_no_kb_put_value_param` (rejects
  `value=` in any `kb_put` call), `cos_agents_toml_kb_segments_are_known` (all segment names in
  prompts must appear in `[[memory.segments]]`), `cos_agents_toml_step6_requires_markdown_content`.
- **`TODOS.md`** — logged pre-existing findings from adversarial review: spawn_agent budget schema max
  (F1/P2), inbox least-privilege caps (F2/P2), curator max_turns=20 exhaustion (F5/P2), orchestrator
  token_budget non-restartable (F6/P3), maxResults=50 context-window tradeoff (F7/note).

## [v0.77.0] - 2026-07-12

### Changed
- **ux.0** (Track UX cockpit): refactor `agentctl watch` from a synchronous poll-render loop to a non-blocking, event-pushed single loop (Option B: background `std::thread` producers + a bounded `std::sync::mpsc` `sync_channel` + `crossterm` `poll(30ms)` + `try_recv`; no async runtime). Behavior-preserving across all TUI views + `--plain`; a pure `step()` state function; SSE producer with total-timeout reconnect + backoff-reset-on-healthy-close + `Invalidated`/snapshot reconciliation; bounded per-tick drain (livelock guard); `TermGuard` panic-hook gated to the main thread + `catch_unwind` producer sentinels. Foundation for ux.1/ux.2. Host-loopback reachability split to ux.0b. 408 tests (5 new); Codex + Claude adversarial review. `agentctl`-only.

## [v0.76.0] - 2026-07-11

### Added — fast local dev-image loop + on-demand multi-arch publish

- **`make dev-image`** — builds the full Docker image (`runtime-full` target) locally and tags it
  `agentos:dev`. Native arm64 on Apple Silicon (no QEMU). Second run ~2 min with the BuildKit cargo
  registry cache warm.
- **`make dev-image-core`** — builds the Rust-only core image (`runtime-core`) as `agentos:dev-core`
  for faster agentd/agentctl-only iteration.
- **`AGENTOS_IMAGE` compose override** — both `cos` and `agent` services now accept
  `AGENTOS_IMAGE=agentos:dev docker compose up cos` to run against a pre-built local image.
  Default is `agentos:dev` (matching `make dev-image` output); Compose builds from source if absent.
- **`.env.example`** — copy to `.env` to persist `AGENTOS_IMAGE` without re-exporting every session.
- **`publish-docker` gated to `workflow_dispatch` / `v*` tag** — a push to `main` no longer triggers
  the 60-90 min QEMU arm64 Docker build. Publish by dispatching the workflow from the Actions UI or
  pushing a version tag (`git tag v0.76.0 && git push origin v0.76.0`).
- **BuildKit parser directive** (`# syntax=docker/dockerfile:1`) added to `Dockerfile`.
- **`docs/DEPLOYMENT.md`** — new "Dev image" section (local loop, tag glossary, cutting a release);
  stale `<details>` block replaced with a pointer to the new section.
- **README.md** — contributor inner-loop one-liner added.
- **TODOS.md** — deferred: native ARM64 CI runners (P4), cross-compiled arm64 Docker (P3).

## [v0.75.0] - 2026-07-11

### Fixed

- **secret-redaction** — OAuth token-refresh error bodies (HTTP non-2xx responses from
  the token endpoint) are no longer included in error strings, flight-recorder events,
  or `provider_last_error`. Only the HTTP status code is retained. Closes `cred.5-ar-01`.
  (`docker/oauth_mcp.py` `_do_refresh` + `_exchange_code`, `agentd/src/credential/mod.rs`)
- **Google OAuth Testing-mode trap** — `MCP_SERVERS.md` no longer steers operators into
  Testing mode (7-day token expiry). Docs now guide operators to publish the OAuth app to
  Production. Adds `invalid_grant` to the error reference table. (`docs/MCP_SERVERS.md`,
  `docs/DEPLOYMENT.md`)

## [cos-dogfood-2] - 2026-07-11 (v0.73.2)

### Fixed — oauth_mcp: check_auth never refreshed from a stored refresh token

- **Root cause**: at startup, `oauth_mcp.py` loads a refresh token (from
  `google.json` or `OAUTH_REFRESH_TOKEN` env var) and sets
  `_auth_state = "authorized"` as a lazy-fetch hint — but leaves `_access_token`
  unset.  `handle_oauth_check_auth` had two ready-paths that were both unreachable
  in this state: the fast path requires `_access_token` to be truthy, and the
  env-refresh branch guards on `_auth_state != "authorized"`.  The function fell
  through to `no_session`, triggering the interactive OAuth dance (which dead-ends
  in Docker).
- **Fix**: `handle_oauth_check_auth` now checks for any available refresh token
  (`_cfg["OAUTH_REFRESH_TOKEN"]` or `_refresh_token`) regardless of `_auth_state`,
  and calls `_ensure_fresh_token()` whenever there is no live access token.  Covers
  both the startup lazy-fetch case and access-token expiry during a long session.
- **Tests**: T30 (file-provided rt, `_auth_state="authorized"`, `_access_token=None`
  → `check_auth` refreshes silently, returns `ready=true`) and T31 (token-file-stored
  rt, `_cfg["OAUTH_REFRESH_TOKEN"]` empty → same outcome). Both fail without the fix.
  Total self-tests: 29 → 31.

## [cos-dogfood] - 2026-07-10 (v0.73.1)

### Fixed — Mac+Docker CoS Gmail flow (live dogfood)

- **FsRead sandbox grant**: `google_oauth` MCP server in both `agentd/cos.agents.toml`
  (Docker/dev) and `distro/overlay/etc/agentd/cos.agents.toml` (QEMU production) now
  declares `FsRead { prefix = "/run/secrets" }`. On Docker Desktop's kernel 6.10
  (LinuxKit), Landlock FS enforcement is active; without this grant the sidecar
  silently cannot open `/run/secrets/google.json`. Guarded by
  `cos_config_google_oauth_grants_fs_read_secrets` test covering both configs.
- **Honest Landlock V4 message**: The `tracing::warn!` emitted when `Net{ports}`
  is declared but Landlock ABI V4 is unavailable previously hardcoded "kernel < 6.7"
  — factually wrong when a kernel ≥ 6.7 ships without `CONFIG_SECURITY_LANDLOCK=y`
  (e.g. LinuxKit). Extracted `net_landlock_v4_unavailable_message()` helper +
  `sandbox::landlock_abi_version()` public API. The message now reports the actual
  detected ABI version. Guarded by `net_landlock_v4_unavailable_message_no_hardcoded_kernel_version` test.
- **README redirect URI**: Removed incorrect "add `http://127.0.0.1:8585` to
  Authorized redirect URIs" step. Desktop app OAuth clients allow all loopback
  redirects automatically per RFC 8252 §7.3; Google Cloud Console has no redirect
  URI field for that client type.

## [h8.2] - 2026-07-10 (v0.73.0)

### Added — `agentos:full` Docker image tier

- **E1 — Tiered Dockerfile**: single `Dockerfile` gains two named runtime stages
  (`runtime-core` and `runtime-full`) sharing one builder stage. `runtime-core`
  contains only the three Rust binaries (`agentd`, `agentctl`, `agentos-otel`) plus
  `fuse3`/`bash`/`jq`; no Python. `runtime-full` extends `runtime-core` with
  `python3` and all standard MCP servers (`shell_mcp.py`, `http_mcp.py`,
  `search_mcp.py`, `oauth_mcp.py`, `cron_mcp.py`, `fs_watch_mcp.py`,
  `webhook_mcp.py`, `semantic_kb_mcp.py`) + `weather-agent.toml`. Operators
  target the desired tier via `docker build --target runtime-core|runtime-full`.

- **E2 — CI publishes both tiers**: `publish-docker` in `ci.yml` now runs two
  sequential `docker/build-push-action@v6` steps sharing the GHA layer cache.
  Tags published per push to `main`:
  - `:core` + `:vX.Y.Z-core` — Rust-only tier
  - `:full` + `:latest` + `:vX.Y.Z-full` + `:vX.Y.Z` — batteries-included tier
  `runtime-core` layers are fully cached before the `runtime-full` step, so the
  second step adds only the Python `apk add` + file copies (~5 min on arm64 QEMU).

- **E3 — README tier table**: Docker quickstart updated with a core/full comparison
  table (tags, contents, when to use each).

## [dx.4] - 2026-07-09 (v0.72.0)

### Added — Pre-built distro images + device auth flow

- **E1 — Pre-built x86_64 distro images**: `release.yml` gains `build-distro-x86_64` job that
  builds the Buildroot rootfs with the release `agentd` binary, runs a QEMU boot smoke test, and
  attaches `agentos-VERSION-x86_64-bzImage`, `agentos-VERSION-x86_64-rootfs.cpio.gz`, and
  `agentos-VERSION-x86_64-SHA256SUMS` (separate from binary `SHA256SUMS`) to the GitHub Release.
  Buildroot cache key shared with `qemu-boot.yml`; `timeout-minutes: 90`; full apt-get dep list
  (`libelf-dev libssl-dev bc bison flex`).

- **E2 — `agentctl auth google --device`** (RFC 8628 Device Authorization Grant): new
  `agentctl/src/auth/google_device.rs` module implements the device code flow. Prints a URL +
  short code; polls Google's token endpoint until authorized, expired, or 30 min monotonic
  deadline. RFC-compliant error handling: `authorization_pending` → retry; `slow_down` → +5 s
  additive backoff (not doubling, per §3.5); `expired_token` / `invalid_grant` → clear error
  message. `access_type=offline` in device auth POST ensures a `refresh_token` is returned.
  `option_env!("OAUTH_CLIENT_ID"/"OAUTH_CLIENT_SECRET")` compile-time embed with runtime
  `env` override. Terminal escape sequences stripped from URL/code display.

- **E3 — DEPLOYMENT.md fast path**: `docs/DEPLOYMENT.md` Path 2 now leads with a fast path
  (download prebuilt images via `install.sh`; no Buildroot required) and a separate "slow path"
  (build from source). Step 4 offers device flow as the primary headless option with instructions
  for credential ownership when running as the `agentos` service user.

- **E4 — `install.sh` convenience installer**: `install.sh` at repo root; detects arch (errors
  clearly for non-x86_64 with Docker path hint); resolves latest tag via grep/sed (no jq dep);
  downloads bzImage + rootfs + distro-specific SHA256SUMS; verifies checksums before writing;
  copies to `/opt/agentos/` with sudo; prints credential ownership reminder for the service user.

- **DRY refactor**: `auth::util` new module with `secrets_file_path()` and `write_secrets_file()`
  shared by PKCE (`google.rs`) and device flow (`google_device.rs`).

## [dx.4b] - 2026-07-09 (v0.71.0)

### Fixed — Mac + Docker CoS first-run failures (F1–F4)

- **F4 — BLOCKER: `Net{ports}` → deny-all on Docker Desktop / pre-6.7 kernels**: `caps_to_rules_inner`
  had a bug where declaring `Net{ports=[443]}` on a kernel without Landlock ABI V4 (Docker Desktop,
  Linux < 6.7) pushed `SandboxRule::IsolateNetwork` despite the operator explicitly declaring network
  access. The Gmail token refresh failed with "Not authenticated", the agent fell back to an in-container
  browser OAuth dance, and the run hung indefinitely. Fixed: the V4-unavailable arm now emits a loud
  `tracing::warn!` and `continue`s — no `IsolateNetwork` is pushed, so the server gets unrestricted
  network access (best-effort allow, matching FS degrade behaviour). Three new tests:
  `net_ports_v4_unavailable_degrades_to_allow_not_deny`, `net_ports_v4_available_emits_allow_connect_both_ports`,
  `no_net_cap_still_isolates_on_no_v4_kernel`. Old `caps_to_rules_net_with_ports_pre_v4_falls_back_to_isolate_network`
  test removed (it was asserting the wrong behaviour).

- **F1 — Missing `ANTHROPIC_API_KEY` setup in DEPLOYMENT.md Path 1**: The guide never told users
  to write the key into `~/.agentos-secrets/agentos.env`. Users who had it in a different shell
  saw the container exit immediately with "ANTHROPIC_API_KEY is not set". Added an explicit
  "step 0" (`mkdir -p ~/.agentos-secrets ~/.agentos-output; printf 'ANTHROPIC_API_KEY=...\n' >> ~/.agentos-secrets/agentos.env; chmod 600`) before `docker compose up`.

- **F2 — `agentctl watch` unreachable from Mac host**: Port 7999 is loopback-only inside the
  container and is not published to the host. Step 3 in DEPLOYMENT.md Path 1 now reads
  `docker compose exec cos agentctl watch` (runs inside the container where the FUSE mount and
  management API are live). Added an explanatory note for clarity.

- **F3 — Briefs written to container-only `/data/output`, not visible on host**: The `cos`
  service had no bind mount for output. Added
  `${AGENTOS_OUTPUT_DIR:-${HOME}/.agentos-output}:/data/output` to the `cos` service volumes in
  `docker-compose.yml` so the CoS entrypoint's `/data/output` write path lands on the Mac host.
  DEPLOYMENT.md updated to match (`mkdir -p ~/.agentos-output` in setup; brief location note).

## [orch.2] - 2026-07-08 (v0.70.0)

### Fixed — Orchestrator hardening (closes 6 orch.1 ARs + 2 pre-conditions)

- **ar-01 — Checkpoint/restore for waiting orchestrated agents**: `SchedulerCheckpoint`
  (FORMAT_VERSION 3→4) gains `waiting_agents: Vec<String>` + `orchestrated_agents: Vec<String>`
  fields (`#[serde(default)]` for backward compat). `from_checkpoint` now restores the actual
  `terminal` flag from the checkpoint instead of hardcoding `false`. Seed loop gains an early
  `continue` guard for agents in `state.waiting` so restored waiting agents are not immediately
  deleted by `handle_agent_terminal`.

- **ar-02 — Spawn confirmation**: `OperatorSpawnRequest` gains `confirm_tx: Option<oneshot::Sender<String>>`
  (`#[serde(skip)]`). `dispatch_operator_spawn_inner` sends the resolved agent ID after insert.
  `POST /api/v1/spawn` awaits the oneshot with a 2 s timeout and returns **201 Created** +
  `{"agent_id":"..."}` on success; 503 on timeout.

- **ar-03 — Answer cap**: `OrchestratorTurnComplete.answer` capped at 512 chars with inline
  `[output truncated — full text streamed above]` note when the answer is longer.

- **ar-05 — State split (dual-purpose race)**: `state.waiting: HashSet<String>` split into
  `orchestrated` (permanent orchestrated membership) and `waiting` (currently parked). `dispatch_operator_spawn_inner`
  inserts into `orchestrated` (not `waiting`) on creation. `AgentCompleted` checks `orchestrated.contains`
  and inserts into `waiting` when parking. `handle_agent_terminal` now removes from **both** sets —
  consolidates 3 call-site removals and fixes the C2 phantom-entry leak where `state.orchestrated`
  was never cleared at termination.

- **ar-06 — SSE keepalive (partial)**: Management server `GET /api/v1/events` sends `": ping\n\n"`
  (SSE comment) every 30 s via `tokio::time::interval` + `tokio::select!`. Mitigates LB idle-timeout
  drops and triggers OS TCP keepalives on quiet links. Client-side: `orchestrate.rs` uses
  `.timeout(None)` — a true network partition without TCP RST still causes an indefinite `reader.lines()`
  hang; tracked as `orch.2-ar-04` (P3). `agentctl orchestrate` error message on stream-end now
  includes a resume command hint.

- **ar-07 — Quit/exit handling**: `agentctl orchestrate` REPL checks for `quit` / `exit` input
  before injecting into the agent and prints a session-pause message with the resume command.

- **audit-O1 — Template MCP capability grants**: `templates/cron-agent.template.toml`,
  `templates/watcher.template.toml`, and `templates/webhook-agent.template.toml` each gain an
  `mcp` capability entry for their trigger server; deny-by-default was silently hiding all tools.

- **audit-C3 — Checkpoint fsync durability**: `write_mode_600` calls `f.sync_all().await` after
  flush; `CheckpointStore::save()` fsyncs the parent directory after the tmp→final rename.

### Changed
- `HttpSource::spawn()` now reads `agent_id` from the response body (was `spawned`) to match the
  new 201 response body. `post_json` now surfaces the response body text on non-2xx for diagnostics.

### Post-review fixes (adversarial review pass)
- **spawn_client timeout**: `HttpSource` gains a dedicated `spawn_client` (3 s) for `POST /api/v1/spawn`.
  The previous `mutation_client` (500 ms) timed out before the management server's 2 s scheduler
  confirmation window, causing agentctl to see a spurious failure while the server created the agent
  anyway. (`agentctl/src/watch/source.rs`)
- **drain loop hang on orchestrator_exited**: `drain_until_turn_complete` now handles `orchestrator_exited`
  events and returns an error immediately rather than blocking forever when an inject is rejected
  (e.g., concurrent inject from a second REPL session). (`agentctl/src/orchestrate.rs`)

### Tests
- 7 new tests total: `waiting_agents_restore_from_checkpoint`, `orchestrated_agents_restore_from_checkpoint`,
  `handle_agent_terminal_clears_both_sets`, `build_checkpoint_includes_waiting_agents` (scheduler.rs);
  `from_checkpoint_restores_terminal_true` (agent/mod.rs);
  `answer_truncation_caps_at_512_chars`, `inject_guard_rejects_non_waiting_agent` (scheduler.rs coverage).
  Total: 1255 workspace tests.

---

## [dx.3] - 2026-07-08 (v0.69.0)

### Added — Linux QEMU production path

- **`distro/buildroot.config`**: `BR2_PACKAGE_PYTHON3=y` + `BR2_PACKAGE_OPENSSL=y` — enables
  Python3 and its ssl module so the stdlib-only MCP sidecars run inside the rootfs.
- **`distro/Makefile`** — three changes:
  - `RUN_NETDEV` variable: `user,id=net0,hostfwd=tcp:127.0.0.1:7999-:7999,...` wired only
    into `make run` (not `make test`), preventing port conflicts on CI hosts.
  - Python MCP overlay target: copies `docker/*.py` to `overlay/usr/lib/agentos/docker/`
    at build time (source of truth stays in `docker/`; overlay is generated, not committed).
  - `clean` target: removes `overlay/usr/lib/` alongside existing overlay artifacts.
- **`distro/overlay/init`**: kernel cmdline `agentd.config=<path>` config selection —
  parses `/proc/cmdline` for `agentd.config=`, falls back to `/etc/agentd/agent.toml`.
  `make run`/`make test` unaffected (no `agentd.config=` in their cmdline).
- **`distro/overlay/etc/agentd/cos.agents.toml`** (new): QEMU-mode CoS config with
  absolute MCP paths, `/run/memory/memory.redb` store, `bind_addr = "0.0.0.0"` management
  API, and `/run/output` FsWrite capability.
- **`agentd/cos.agents.toml`**: `[management] enabled = true` so `agentctl watch` works in
  dev mode. Default `bind_addr = "127.0.0.1"` (loopback — safe for local cargo run).
- **`distro/agentos-cos.service`** (new): systemd unit for the Linux host. Runs
  `qemu-system-x86_64` as `User=agentos` with `-accel kvm`, loopback hostfwd on ports
  7999 and 8080, 512 MB RAM, and `ExecStartPre` to create writable dirs before boot.
- **`docs/DEPLOYMENT.md`** (new): two-page operator guide covering Mac+Docker and Linux QEMU
  paths, complete `agentos.env` template (including `TRIGGER_CRON`), SSH tunnel instructions
  for remote `agentctl watch`, and troubleshooting commands.

## [cred.5] - 2026-07-08 (v0.68.0)

### Added — Credential Control Plane visibility

- **`CredentialSnapshot` + `ProviderHealth`** in `surfaces/src/snapshot.rs` — new
  types carrying gateway-enabled status, configured provider names, and per-provider
  health (token_fresh, last_refresh_at, expires_at, last_error). Derive `Serialize`;
  `SchedulerSnapshot` gains `credential_snapshot: Option<CredentialSnapshot>`.
- **Per-agent credential fields on `AgentSnapshot`** — four new fields:
  `credential_providers: Vec<String>`, `credential_request_counts: HashMap<String,u64>`,
  `credential_denied_counts: HashMap<String,u64>`,
  `credential_last_access_at: HashMap<String,u64>`.
- **5 new cred.5 observability maps on `CredentialGateway`** (all `std::sync::RwLock`):
  `denied_counters`, `last_access`, `provider_expiry`, `provider_refresh_ts`,
  `provider_last_error`. Updated atomically in the gateway request path.
- **`CredentialRegistry` converted to `std::sync::RwLock`** — all 4 methods now sync;
  enables `snapshot()` and `agent_grant_for()` to be called from the sync
  `update_snapshot()` path without blocking.
- **`INO_SYS_CREDENTIALS = 19`** — new FUSE virtual file `/agents/system/credentials`
  emitting `CredentialSnapshot` JSON when the gateway is active; `"enabled": false`
  sentinel when disabled.
- **`OFF_CREDENTIALS = 13`** — new per-agent FUSE file `/agents/<id>/credentials`
  emitting `{providers, request_counts, denied_counts, last_access_at}` JSON.
- **`GET /api/v1/credentials`** on management HTTP API (`:7999`) — returns
  `CredentialSnapshot` JSON or `{"enabled": false}` when gateway is off.
- **`agentctl watch` `[c]` Credentials pane** — `View::Credentials` TUI view showing
  gateway status, configured providers, per-provider `[fresh]`/`[stale]` badges,
  expiry timestamp, last refresh, and last error.
- **`render_plain` credentials section** — always emitted before the memory early-return;
  shows `credentials: gateway not configured`, `credentials: gateway disabled`, or
  per-provider health lines with token_fresh / expires_at / last_refresh / last_error.
- **`DataSource` updated** — `FuseSource` reads `/agents/system/credentials`;
  `HttpSource` fetches `/api/v1/credentials`; both parse into `SysCredentials`.
- **3 new FUSE tests** — `fuse_system_credentials_no_gateway`,
  `fuse_system_credentials_with_gateway`, `fuse_per_agent_credentials_file_produces_json`.
- **4 new `render_plain` tests** — `render_plain_credentials_not_configured_shows_message`,
  `render_plain_credentials_disabled_shows_disabled`,
  `render_plain_credentials_fresh_token_appears`,
  `render_plain_credentials_stale_token_shows_error`.
- **4 new `credentials_from_json` tests** in `source.rs`.

### Fixed

- **Early-return ordering** — `render_plain` credentials block moved before the
  `/agents/kb` directory check so credentials are always output in `--plain` mode.

## [ma.4] - 2026-07-08 (v0.67.0)

### Added — Isolation-tier detection + honest per-device reporting

- **`agentd::isolation_caps::probe()`** — new module that probes device-level isolation
  capabilities at startup and computes a coarse tier. Calls `which_runsc()` (gVisor),
  `sandbox::landlock_available()` (Landlock ABI ≥ 1), and reads
  `/proc/sys/kernel/seccomp/actions_avail` (x86_64 Linux only). Never panics; all
  detection is fallback-safe.
- **Tier taxonomy:** `full` = runsc AND landlock AND seccomp present; `capability` =
  at least one present (including runsc-only); `none` = none detected.
- **`IsolationCapsSummary`** on `SchedulerSnapshot` — Serialize-only struct carrying
  `tier`, `arch`, `runsc` (path or null), `landlock`, `seccomp`.
- **`INO_SYS_ISOLATION = 18`** — new FUSE virtual file `/agents/system/isolation` that
  emits `IsolationCapsSummary` as JSON using `serde_json` (not hand-rolled).
- **`IsolationProbed`** flight event emitted at startup with the full summary.
- **`SysIsolation`** struct in `agentctl/src/watch/reader.rs` for JSON deserialization.
- **`agentctl watch` System view** — color-coded isolation row: green=full,
  yellow=capability, red=none; legend footer; `--plain` mode emits
  `isolation_tier:` / `isolation_arch:` lines.
- **`ma.4-ar-01 (P3)`** — `require_isolation_tier` config key deferred to TODOS.md.

### Fixed

- **Tier logic:** `classify_tier()` extracted as a pure function; middle branch fixed
  from `landlock || seccomp` to `runsc || landlock || seccomp` so that gVisor-only
  deployments report `capability` instead of incorrectly falling through to `none`.
- **`which_runsc()`** now checks the executable bit (`mode & 0o111 != 0`) on Unix
  in addition to `is_file()`; non-executable `runsc` files no longer register as present.
- **`IsolationCapsSummary::default()`** now uses `std::env::consts::ARCH.to_string()`
  instead of an empty string for the `arch` field.

## [orch.1] - 2026-07-07 (v0.66.0)

### Added — Interactive agent orchestrator

- **`agentctl orchestrate`** — new REPL subcommand: spawn an orchestrated agent with an
  initial task, receive its answer, continue the conversation across turns with a persistent
  SSE connection (no per-turn reconnect race). Exits cleanly on EOF or Ctrl-D; re-prompts on
  empty input.
- **`agentctl inject <id> <text>`** — new CLI subcommand to inject a user turn into any
  waiting agent from outside the REPL.
- **`POST /api/v1/spawn`** on the management API — spawn an agent from JSON (`task`,
  optional `id`, `max_turns`, `orchestrated` flag); returns the resolved agent ID.
- **`POST /api/v1/agents/:id/inject`** — inject a user turn into a waiting agent over HTTP.
  Returns 400 on invalid/empty input, 503+`Retry-After` on full channel.
- **`AgentStatus::Waiting`** — new scheduler state for orchestrated agents parked between
  turns. Reflected in `/agents/<id>/status` FUSE file, snapshot API, and `agentctl watch`
  (shown as `⏸waiting`).
- **`OrchestratorTurnComplete` SSE event** — fired when an orchestrated agent parks after
  completing a turn; carries `agent_id` and `answer`. Used by `agentctl orchestrate` to know
  when to prompt for the next input.
- **`OrchestratorDispatched` / `OrchestratorInjected` / `OrchestratorExited`** — three new
  flight events covering spawn, inject, and error paths.
- **`templates/orchestrator.template.toml`** — new catalogue template with
  `max_turns = 200`, `token_budget = 200000`, streaming enabled.
- **`docker/entrypoint.sh orchestrate` mode** — auto-detects a running agentd via healthz;
  cold-starts one if absent, waits 15 s, then execs `agentctl orchestrate`. Forwards
  SIGTERM/SIGINT to agentd so graceful checkpoint fires on `docker stop`.
- **`AGENTD_MANAGEMENT_ENABLED=true` env var** — enables the management HTTP API without
  editing TOML; also respects `AGENTD_MANAGEMENT_PORT`.

### Fixed

- `agentctl orchestrate` resume path now requires `status == "waiting"` (previously
  injected into running agents, causing silent REPL deadlock).
- `drain_until_turn_complete` now handles `agent_completed` events and bails with a clear
  error rather than hanging indefinitely if the agent exits without parking.
- Management API `/api/v1/agents/:id/inject` validates `agent_id` against `[a-zA-Z0-9_-]`
  (consistent with `validate_child_id` used on all other inject paths).
- `entrypoint.sh orchestrate` cold-start path now kills agentd and installs a SIGTERM trap
  before `wait`, preventing an indefinitely-blocked container after the REPL exits.

## [cred.4b] - 2026-07-07 (v0.65.0)

### Changed — Credential-agnostic MCP servers

- **`docker/oauth_mcp.py`** — `_load_config()` broker short-circuit: when
  `AGENTD_CREDENTIAL_GATEWAY_URL` is set, only routing config (`OAUTH_PROVIDER_NAME`,
  `ALLOWED_HOSTS`) is loaded; raw secrets file and credential env vars are never read
  into this process.
- **`docker/oauth_mcp.py`** — `handle_oauth_start_auth`, `handle_oauth_check_auth`, and
  `handle_oauth_call_api` all gate on `_BROKER_URL and _BROKER_TOKEN` (both required).
  URL-only misconfiguration (missing `AGENTD_CREDENTIAL_TOKEN`) returns a
  `broker_token_missing` error across all three handlers.
- **`docker/oauth_mcp.py`** — `OAUTH_PROVIDER_NAME` validated against `[a-zA-Z0-9_-]+`
  at startup to prevent path-traversal in broker URL construction.
- **`docker/search_mcp.py`** — legacy `BRAVE_SEARCH_API_KEY` direct-access path emits a
  deprecation warning to stderr (once per process, not per request).
- **6 new self-tests** (T24–T29) in `oauth_mcp.py` covering broker-mode paths (total 29/29).
- **ROADMAP `cred.4` marked ✓** — acceptance criterion "tool process holds no raw credential
  in memory-at-rest" is now fully satisfied. No Rust changes.

## [h8.1] - 2026-07-07 (v0.64.0)

### Added — Layer-2 semantic memory sidecar

- **`docker/semantic_kb_mcp.py`** — HTTP MCP sidecar backed by Qdrant + Voyage AI embeddings.
  Exposes `kb_put` / `kb_get` / `kb_search` (vector-similarity). Self-tests T1–T18; no external
  services needed (`VOYAGE_MOCK_EMBEDDINGS=1`). 18/18 self-tests pass.
- **`allow_insecure_local`** — new `McpServerConfig` field; permits `http://` URLs for
  Docker-internal peer services (emits `tracing::warn!` at startup; `https://` still required
  for all others).
- **`tool_override`** — new `McpServerConfig` field; MCP tools silently shadow same-named native
  tools via `ToolRegistry::register_override()`. Restricted: `request_approval` and `spawn_agent`
  are PROTECTED_TOOLS and cannot be shadowed (startup error).
- **`templates/librarian-semantic.template.toml`** — first Layer-2 template; gated on
  `VOYAGE_API_KEY`.
- **`docker-compose.yml`** — `qdrant` (v1.13.6, pinned, with healthcheck) and `semantic-kb-mcp`
  services under `profiles: [semantic]`.
- **`docker/Dockerfile.semantic-kb-mcp`** — HEALTHCHECK + `/healthz` endpoint.
- **`docs/MCP_SERVERS.md`** — Semantic KB section documenting the sidecar setup.

### Security hardening (pre-landing review)

- `_KEY_RE` tightened to exclude `?#@%+=` preventing URL injection via segment names interpolated
  into Qdrant collection URLs.
- Negative `Content-Length` guard: reject with 400 before body-size check.
- `_qdrant()` response capped at `MAX_QDRANT_RESPONSE` (4 MB) preventing OOM from oversized responses.
- `_ensure_collection` bare `except RuntimeError: pass` narrowed to re-raise non-404 errors.
- IPv4-mapped IPv6 (`::ffff:172.17.0.x`) now accepted by `_is_private()` for Docker-internal hosts.
- `SIDECAR_SECRET` optional inbound auth token (`X-Sidecar-Token` header, `secrets.compare_digest`).
  Default off; Docker-network isolation is the boundary.
- `register_override()` now returns `Result<()>` and emits `tracing::warn!` when displacing a tool;
  `PROTECTED_TOOLS = ["request_approval", "spawn_agent"]` cannot be shadowed.
- Startup cap-mismatch warning: when `tool_override=true`, agents with `KbRead`/`KbWrite` but no
  matching `Mcp{server=...}` grant are warned at startup.

### Tests
- 2 new Rust tests: `tool_override_protected_tools_are_blocked`, `tool_override_non_protected_tool_succeeds`.
- 5 new config tests: `http_server_allow_insecure_local_ok`, `http_server_insecure_local_rejected_without_flag`,
  `http_insecure_local_rejects_embedded_credentials`, `tool_override_field_parses_true`,
  `tool_override_field_defaults_false`.
- 5 new Python self-tests: T8 (SSRF classification), T9 (oversize content), T10 (not-found),
  T11 (empty key), T12 (segment URL injection).
- 3 additional Python self-tests (coverage audit): T13 (SIDECAR_SECRET auth — correct/wrong/missing/empty),
  T14 (GET /healthz returns 200 + `{status:ok}`), T15 (negative Content-Length returns JSON-RPC -32700).
- 1 additional Python self-test (pre-landing): T16 (kb_search non-404 Qdrant error propagates as isError).
- 2 additional Python self-tests (adversarial review): T17 (`params: null` body handled gracefully),
  T18 (non-object JSON body returns -32700 parse error, not AttributeError crash).
- Hardening fixes (pre-landing + security + adversarial review):
  - Voyage AI response capped at `MAX_VOYAGE_RESPONSE` (4 MB) on both success and error paths
  - Qdrant error response body capped at `MAX_QDRANT_RESPONSE` (4 MB) — closes OOM gap in error path
  - `_handle_kb_search` bare `except RuntimeError` narrowed to re-raise non-404 errors
  - `send_message` added to `PROTECTED_TOOLS` — MCP servers with `tool_override` cannot shadow inter-agent messaging
  - `params: null` JSON-RPC body treated as `{}` (was `AttributeError` → TCP RST)
  - Non-object JSON body (`[1,2,3]`, `42`) returns `-32700` parse error (was `AttributeError` → TCP RST)
  - `docker-compose.yml` `agent` depends on `semantic-kb-mcp` (`required: false`) — fixes startup race
  - `docs/MCP_SERVERS.md` self-test count corrected from 7 to 18
- Total workspace tests: 1190 Rust + 18 Python self-tests.

---

## [cred.3.2] - 2026-07-06 (v0.62.0)

### Security / hardening (Groups A–E + post-review hardening)
- **ar-10** — `is_ssrf_blocked()` and `extract_host()` moved to `loopback_proxy.rs` as the
  canonical SSRF guard; both proxies import from there — no diverging private copies.
- **ar-04 / D2** — IP pinning: `GatewayState::new()` DNS-resolves each `upstream_base` hostname
  at startup and pins the resolved IP into the `reqwest::Client` via `ClientBuilder::resolve()`.
  DNS rebinding is blocked for the process lifetime.
- **ar-04c** — OAuth `token_url` SSRF check: `get_or_refresh()` resolves and SSRF-checks the
  token endpoint before posting to it. Empty DNS iterator (`NOERROR NODATA`) now warns and
  continues instead of silently bypassing the check (ADV-1/ADV-2).
- **OOM fix / D14** — `upstream_resp.bytes().await` → `bytes_stream()` per-chunk accumulator;
  size cap enforced incrementally.
- **D3 / query sanitization** — Inbound query string always discarded; MCP servers cannot inject
  URL parameters into the upstream forwarded URL.
- **ar-07 / multi-agent attribution** — `owning_agent_id()` helper extracted; single-agent mode
  returns the agent ID; multi-agent mode uses `"shared"` sentinel (all MCP servers share the
  global pool); zero-agent falls back to `server.name`. Prevents false attribution of all
  credential accesses to `agent[0]` in multi-agent configs.
- **base_builder() / drift guard** — `loopback_proxy::base_builder()` extracted; `GatewayState::new()`
  now calls it instead of re-implementing all four `reqwest::Client::builder()` settings, closing
  the drift risk the module doc warned about.

### Known limitations (documented, not fixed — deferred to cred.4)
- `token_url` hostname is SSRF-checked at lookup-time but not pinned in the reqwest client;
  a TOCTOU window exists between the check and the OAuth POST. Mitigated by operator control of
  secrets files. See THREAT_MODEL §8.3.
- D3 query discard strips ALL query params including functional ones (e.g. search terms for
  GET-based APIs). MCP servers using `ApiKeyQuery` providers must encode params in the URL path.

### Documentation
- **THREAT_MODEL.md** — version header bumped to v0.62.0; §8.3 updated (IP pinning landed);
  §8.7 expanded with S2/S3 ratified de-claims.
- **RUNBOOK.md** — version updated to v0.62.0; §11.11 credential broker ops added.
- **CLAUDE.md** — cred.3.2 status line added.

### Tests
- T26–T35b + T36/T37 + `owning_agent_id` + `base_builder` drift guard (21 new tests total):
  startup SSRF rejection, empty-DNS-iterator guard, IP pin path with public IP literal,
  token_url SSRF rejection, bytes_stream enforcement, ar-07 single/multi-agent attribution,
  base_builder delegation, self-referential assertion fixes in T28/T34/T35b, and more.
- G1–G5 coverage gap tests (5 new): `token_url` userinfo rejection (behavioral),
  `upstream_base` userinfo rejection (behavioral), plus structural source-scan guards for
  the warn-and-continue DNS Err arms in `get_or_refresh()` and `GatewayState::new()`,
  and the 502 `upstream_body_error` arm in the bytes_stream loop.
- Total workspace tests: 1164 (up from 1139).

---

## [cred.3.1] - 2026-07-06 (v0.61.0)

### Security / hardening (10 gate items, every item has a failing-without-fix test)
- **ar-10** — `loopback_proxy.rs` (new crate module): shared `build_loopback_client()` used by
  both `EgressProxy` and `CredentialGateway`; drift between the two client configurations is now
  a compile error rather than a runtime divergence.
- **ar-04** — SSRF guard on `upstream_base`: DNS resolution at startup, then `is_ssrf_blocked()`
  rejects loopback / link-local (169.254.x.x IMDS) / RFC 1918 / fc00::/7 unique-local /
  IPv4-mapped IPv6 (`::ffff:x.x.x.x`). `extract_host()` now correctly handles IPv6 literal
  URLs (`[::1]`) and rejects userinfo (`user@host`); malformed URLs are now a hard startup
  error (not a silent skip). DNS failure warns to preserve air-gapped environments.
- **ar-08** — `PASSTHROUGH_HEADERS` allow-list replaces `SCRUB_HEADERS` deny-list in the
  credential gateway forwarder; only 6 known-safe headers are forwarded, all others are dropped.
- **ar-06** — `OAuthTokenCache::load_from_disk()`: reads persisted `OAuthState` on broker
  startup so a valid token survives daemon restarts; expired tokens and empty `access_token`
  are discarded and the broker starts cold.
- **ar-07** — Deny-by-default fast path for empty `allowed_providers`: returns HTTP 403
  `credential_denied / no_providers_configured` immediately instead of falling through to
  `None`-match behavior.
- **S1** — OV-1 startup invariant: egress Ed25519 signing key path must not fall inside any MCP
  server's `FsRead` sandbox prefix; fails fast at boot with a diagnostic message.
- **S2** — Removed `"content_audited": true` from `EgressBrokered` events; the field was
  hardcoded and never reflected actual scanning. `EventKind::EgressBrokered` doc comment updated.
- **S3** — De-claimed `SecretRewriter` / `BoundarySecretRedacted` from CLAUDE.md (p7.5 block)
  and THREAT_MODEL.md; those features were planned but never built.
- **ar-09 (doc)** — `docs/ROADMAP.md`: cred.4 and orch.1 now list `cred.3.1` as a prerequisite.
- **THREAT_MODEL §8.6–8.7** — Universal-tier agents have no credential path (intentional,
  tracked for cred.4/cred.5). Egress content audit is explicitly NOT implemented.

### Tests added (T18–T24 + adversarial-review fixes, every test fails without its fix)
- T18–T20: `is_ssrf_blocked` loopback, link-local/IMDS, RFC 1918; public-IP non-blocking;
  `extract_host` basic + rejects non-HTTPS; IMDS/RFC-1918 SSRF gate assertion.
- T21: `load_from_disk` pre-populates cache from valid state file; absent file starts cold;
  expired token starts cold; empty `access_token` starts cold.
- T22: Live-gateway integration test — registers a token with empty `allowed_providers`,
  makes a real HTTP request, asserts HTTP 403 + `reason: no_providers_configured`.
  (Previous version constructed its own JSON and never called `handle_credential_request`.)
- T23: `include_str!("../egress.rs")` source scan — fails if `"content_audited"` reappears.
- T24: `test_loopback_proxy_shared_client_builds` — both egress and credential configs build.
- SSRF follow-ups: `is_ssrf_blocked` IPv4-mapped IPv6 (`::ffff:192.168.1.1`) and fc00::/7
  unique-local; `extract_host` rejects userinfo and correctly handles IPv6 literal brackets.

### Post-ship adversarial review fixes (3 confirmed findings from Claude + Codex reviewers)
- **F1 — percent-encoded path traversal** (`normalize_path_segment`): `%2e%2e` components
  now filtered alongside literal `..`; `%2e` also filtered. Upstream server path normalization
  (e.g. `/v1/%2e%2e/secret` → `/secret`) can otherwise produce traversal outside the base path.
  New test `test_normalize_path_segment_blocks_pct_encoded_traversal` fails without the fix.
- **F2 — `x-goog-user-project` billing injection**: removed from `PASSTHROUGH_HEADERS`.
  A compromised MCP server could inject this header to redirect API quota and charges to an
  arbitrary GCP project. Blocked by the `PASSTHROUGH_HEADERS` injection-risk assertion test.
- **F3 (Codex Critical) — `None` capabilities grants all credential providers**: changed
  `None => all_providers` to `None => vec![]` in the credential-env build logic; credential
  providers must now be granted explicitly via `capabilities`. New test
  `none_capabilities_yields_empty_credential_providers` fails without the fix.

Total workspace tests: 1139 (up from 1112; figure covers all workspace crates).

## [cred.3] - 2026-07-06 (v0.60.0)

### Added
- `agentd/src/credential/mod.rs` (new, ~955 lines): `CredentialGateway` — second
  OS-assigned loopback HTTP listener that MCP servers call to access credentials without
  holding them directly. Implements TOML-driven provider adapters (`oauth-bearer`,
  `api-key-header`, `api-key-query`), ephemeral per-spawn credential tokens
  (`AGENTD_CREDENTIAL_TOKEN` + `AGENTD_CREDENTIAL_GATEWAY_URL` injected by agentd),
  header scrubbing (strips `Authorization`, `Host`, `X-Subscription-Token`, and the
  provider's configured `header_name` before attaching auth), `OAuthTokenCache` with
  atomic state writes (tmp→rename, mode 0600), and `CredentialRegistry`
  (`tokio::sync::RwLock`-backed).
- `agentd/src/capability.rs`: `CredentialProvider` enum (`Google`, `BraveSearch`,
  `Custom(String)`) and `Capability::Credential { provider: CredentialProvider }`.
  `satisfies()` / `satisfies_type()` updated.
- `agentd/src/config.rs`: `AuthStyle` enum, `ProviderConfig`, `CredentialGatewayConfig`
  (opt-in, default disabled). Config gains `[credential_gateway]` table.
- `agentd/src/events.rs`: 5 new `EventKind` variants: `CredentialEgressBrokered`,
  `CredentialAccessed`, `CredentialRefreshFailed`, `CredentialNotProvisioned`,
  `CredentialDenied`.
- `agentd/src/tools/mcp.rs`: `PASSENV_BLOCKLIST` extended with `BRAVE_SEARCH_API_KEY`,
  `OAUTH_REFRESH_TOKEN`, `OAUTH_CLIENT_SECRET`, `OAUTH_ACCESS_TOKEN`,
  `AGENTD_CREDENTIAL_TOKEN`, `AGENTD_CREDENTIAL_GATEWAY_URL`. `McpClient::spawn()`
  gains a `credential_env` parameter (applied last, highest priority, with collision
  warning).
- `agentd/src/main.rs`: credential gateway started before MCP loop; UUID4 token per
  stdio MCP server spawn; tokens deregistered at shutdown. `caps_to_rules()` gains
  `Capability::Credential` arm (no-op at cred.3; enforcement in cred.4+).
- `docker/search_mcp.py`: dual-path — broker path via `AGENTD_CREDENTIAL_GATEWAY_URL` +
  `AGENTD_CREDENTIAL_TOKEN`; legacy `BRAVE_SEARCH_API_KEY` env fallback preserved.
- `docker/oauth_mcp.py`: `oauth_call_api` routes via broker when `AGENTD_CREDENTIAL_GATEWAY_URL`
  is set; falls back to legacy PKCE flow otherwise.
- `docs/THREAT_MODEL.md` §8 Credential Gateway (§8.1–8.5): token identity, in-process
  credential isolation, loopback SSRF, 9p write integrity, header scrubbing.
- `docs/CONVENTIONS.md`: 5 new event kind rows.

### Changed
- `agentd/src/egress.rs`: `ProxyRegistry` converted from `std::sync::RwLock` to
  `tokio::sync::RwLock`; `register`, `deregister_by_key`, `entry_for_key` are now
  `async fn`. All callers in `main.rs` and `scheduler.rs` updated.

### Security
- Credential broker strips caller-supplied credential headers before attaching auth —
  prevents MCP server from injecting a forged `Authorization` header to the upstream.
- Ephemeral token per-MCP-spawn deregistered on exit — minimal blast radius if a token
  is leaked after the spawn exits.
- `credential_refresh_failed` emitted even when an atomic write fails on QEMU 9p but the
  current access token still works — prevents silent recovery-blocking token loss.
- All new broker-managed env var names added to `PASSENV_BLOCKLIST` — prevents passenv
  from tunneling raw secrets to subprocesses.

## [cred.2] - 2026-07-05 (v0.59.0)

### Added
- `docker/entrypoint.sh`: parses `/run/secrets/agentos.env` as `KEY=value` pairs
  before `check_api_key` on every mode. Uses a safe `while read` loop (no shell sourcing)
  that rejects keys with non-identifier characters and digit-leading names. File wins if
  the same key exists in both compose env and the secrets file (intentional: secrets file
  is authoritative). Values with embedded `=` (e.g., base64 tokens) are preserved correctly.
- `check_api_key` error message updated: now shows both the secrets-file path
  (`~/.agentos-secrets/agentos.env`) and the `-e ANTHROPIC_API_KEY=...` option.
- `docker-compose.yml`: volume mounts now use `${AGENTOS_SECRETS_DIR:-${HOME}/.agentos-secrets}`
  so Linux CI runners can set `AGENTOS_SECRETS_DIR` to override the `${HOME}` expansion
  (fixes cred.1-ki-02).
- `distro/overlay/init`: guest-side 9p mount for `secrets0` now passes `,ro` — belt-and-suspenders
  with the server-side `readonly=on` in `distro/Makefile`.
- `tests/fixtures/google.json`: checked-in schema fixture (`client_id`, `client_secret`,
  `refresh_token`) — cross-language contract between `agentctl auth google` (writer) and
  `docker/oauth_mcp.py` (reader).
- `docker/oauth_mcp.py`: 2 new self-tests (22, 23) — schema-drift guard verifying the fixture
  field names map to expected config keys; missing-key test confirms explicit error over silent
  empty value. Total self-tests: 23.
- `.github/workflows/ci.yml`: `build-macos` job — agentctl build + clippy + tests on
  `macos-latest` (covers PKCE primitives, RFC test vector, `agentctl auth google` host-side logic).

### Changed
- `distro/Makefile`: QEMU `secrets0` virtfs mount is now read-only (`readonly=on`).
  The host-path `~/.agentos-secrets` is exported to the guest as read-only; agents and
  MCP servers inside QEMU cannot modify the host secrets directory.
- `docs/RUNBOOK.md`: added `PARTIALLY STALE` banner; §11 credentials section rewritten —
  `agentctl auth google` + secrets-file flow replaces the old `OAUTH_*` env-var export
  instructions and `~/.agentos-oauth/google.json` path (which no longer exists).

### Breaking
- `docker-compose.yml` `agent` service: **`OAUTH_CLIENT_ID`, `OAUTH_CLIENT_SECRET`,
  `OAUTH_REFRESH_TOKEN`, and `OAUTH_CALLBACK_PORT` removed from the `environment` block.**
  These vars are no longer injected into the container by `docker compose run`.
  **Migration:** use `agentctl auth google` to write `~/.agentos-secrets/google.json`
  (see RUNBOOK §11.3). The entrypoint's google-agent preflight still accepts a complete
  set of `OAUTH_CLIENT_ID` + `OAUTH_CLIENT_SECRET` + `OAUTH_REFRESH_TOKEN` env vars
  passed via `docker run -e` for users not using compose.

## [cred.1] - 2026-07-05 (v0.58.0)

### Added
- `docker-compose.yml`: `agent` service now mounts `${HOME}/.agentos-secrets:/run/secrets:ro`,
  matching the `cos` service. `docker compose run --rm agent TEMPLATE_NAME=google-agent` now
  picks up credentials written by `agentctl auth google` without manual patching.
- `docker/entrypoint.sh`: `google-agent)` arm in the `agent` mode template switch — fail-fast
  preflight with a two-branch error (volume not mounted vs. credentials absent). Accepts either
  `/run/secrets/google.json` (recommended) or a complete `OAUTH_CLIENT_ID` + `OAUTH_CLIENT_SECRET`
  + `OAUTH_REFRESH_TOKEN` env-var fallback (backwards compat for existing users).

### Fixed
- `README.md`: removed the false "container never sees your OAuth client credentials" claim.
  `google.json` stores `client_id`, `client_secret`, and `refresh_token`. Added accurate text
  and `mkdir -p ~/.agentos-secrets` as step 1 in the one-time setup flow.
- `docker/entrypoint.sh` cos preflight: the `agentctl auth google` command in the error message
  now includes `--client-id` and `--client-secret` flags (was truncated since dx.1 / v0.52.0).
- `docker/entrypoint.sh` cos preflight: guard changed from `[ ! -f ]` (existence) to
  `[ ! -s ]` (existence + non-zero size), consistent with the `google-agent` arm. A zero-byte
  file no longer passes the cos preflight silently.
- `docker/entrypoint.sh` google-agent error message: re-run instruction now includes
  `TEMPLATE_NAME=google-agent AGENT_TASK="..."` to avoid a second error on bare `docker compose run`.
- `docker-compose.yml`: corrected the stale `HOME=/data` comment (wrong path reference).
- `docs/SPIKES/cred.1-secrets-mount.md`: corrected cred.1-ki-01 — `oauth_mcp.py` writes
  refresh tokens to the named volume (`/data`), not `/run/secrets/`; `:ro` does not block
  token refresh write-back.

## [ma.2] - 2026-07-05 (v0.57.0)

### Added
- `distro/buildroot.aarch64.config` — Buildroot defconfig for aarch64 (`BR2_aarch64=y`,
  `BR2_LINUX_KERNEL_IMAGE=y` for raw `Image` format, arm64 generic `defconfig`).
- `distro/kernel-extras.aarch64.config` — kernel config fragment for the QEMU `virt`
  machine: inherits all x86_64 flags (9P, virtio, FUSE, Landlock, seccomp, namespaces)
  plus `CONFIG_VIRTIO_MMIO=y` (ARM MMIO bus) and `CONFIG_SERIAL_AMBA_PL011_CONSOLE=y`
  (PL011 UART for `console=ttyAMA0`; without this the guest boots silently).
- `distro-aarch64` CI job: `make -n build ARCH=aarch64` + `make -n run ARCH=aarch64`
  dry-run validates Makefile variable expansion without running QEMU or Buildroot.
  Full build + HVF boot is a local developer workflow on Apple Silicon.

### Changed
- `distro/Makefile` parameterized by `ARCH` (default `x86_64`):
  - `ARCH=aarch64` selects `qemu-system-aarch64 -M virt`, `Image` kernel, `ttyAMA0`
    console, `aarch64-unknown-linux-musl` binary, and `output/aarch64/` output dir.
  - HVF/KVM/TCG acceleration auto-detected: macOS → `-accel hvf -cpu host`;
    Linux with `/dev/kvm` → `-accel kvm -cpu host`; fallback → `-accel tcg -cpu cortex-a72`.
  - Buildroot output goes to `build/output-$(ARCH)/` (separate trees; no clobber on arch switch).
  - `overlay/usr/bin/agentd` copy uses `$(MUSL_TARGET)` to pick the correct cross binary.
  - x86_64 `OUTPUT_DIR` stays `output/` — no CI churn or muscle-memory breakage.
- `distro/README.md` updated with Apple Silicon quickstart and both-arch directory layout.

### Fixed
- HVF acceleration for aarch64 guests now correctly gates on Apple Silicon (`UNAME_M=arm64`);
  Intel Macs previously received `-accel hvf` which fails with "invalid accelerator" on
  aarch64 guests (HVF is host-arch-specific).
- KVM detection changed from `test -e /dev/kvm` to `test -w /dev/kvm`; the old check
  selected `-accel kvm` even when the user lacked group membership, causing a confusing
  QEMU "Permission denied" error at boot rather than a graceful TCG fallback.
- `ARCH` values other than `x86_64`/`aarch64` now fail immediately with a clear Make error
  instead of silently falling through to x86_64 defaults.
- `overlay/usr/bin/agentd` and `overlay/usr/bin/agentctl` now declare the source musl
  binary as a real-file prerequisite; Make re-copies when the source is newer, preventing
  a stale x86_64 binary from being embedded in an aarch64 rootfs after an arch switch
  without `make clean`.

### Notes
- `make build ARCH=aarch64` and `make build` (x86_64) coexist on disk: separate
  `build/output-aarch64/` and `output/aarch64/` vs `build/output-x86_64/` and `output/`.
- gVisor (`runsc`) has no aarch64 release; universal-tier templates degrade gracefully.
- `DenySpawn` seccomp filter is `#[cfg(target_arch = "x86_64")]` — documented gap;
  `CONFIG_SECCOMP=y` still set in aarch64 kernel config for other seccomp uses.

## [ma.3] - 2026-07-04 (v0.56.0)

### Added
- `publish-docker` CI job: builds and pushes a multi-arch Docker image
  (`linux/amd64` + `linux/arm64`) to `ghcr.io/0x89karan/runtime1:latest` and
  `ghcr.io/0x89karan/runtime1:v{semver}` on every push to `main`. Gated on
  `build-and-test`, `build-aarch64`, and `audit` all passing — a broken Rust build
  or failing audit never publishes an image. Uses `docker/setup-qemu-action@v3` + `docker/setup-buildx-action@v3` +
  GHA layer caching (`type=gha,mode=max`) for faster arm64 rebuilds (~8-12 min cached,
  20-30 min cold). `provenance: false` ensures compatibility with Docker clients < 24.x.
- Apple Silicon Mac, ARM cloud, and ARM single-board computer users can now
  `docker pull ghcr.io/0x89karan/runtime1:latest` and run agentd natively — no
  Rosetta emulation, no "wrong platform" warning.

### Notes
- One-time manual step after first merge: set ghcr.io package visibility to Public
  (GitHub repo → Packages → agentos → Package Settings → Change visibility → Public).
- `provenance: false` disables OCI SBOM attestations. Remove it before adding SBOM tooling.
- `docker compose up` still uses `build: .` (local build) — pulling the published image
  requires `image: ghcr.io/0x89karan/runtime1:latest` in docker-compose.yml (deferred).

## [ma.1] - 2026-07-04 (v0.55.0)

### Added
- `build-aarch64` CI job: cross-compiles `agentd` and `agentctl` to
  `aarch64-unknown-linux-musl` via `cross` + QEMU emulation on every push.
  Includes `cross clippy -- -D warnings` and `cross test` (QEMU-emulated),
  with per-binary size guard (≤ 6 MB). Job has `timeout-minutes: 45` to
  prevent indefinite hang under QEMU. Closes TODOS P4.
- `Cross.toml` at repo root pinning `ghcr.io/cross-rs/aarch64-unknown-linux-musl:0.2.5`
  for reproducible `ring` cross-compilation (avoids breakage on Docker image updates).
- `make clippy-aarch64` Makefile target: runs `cross clippy` for both crates against
  `aarch64-unknown-linux-musl` with Docker + `cross` preflight checks. Use before
  pushing any code that changes `#[cfg(target_arch)]`-gated behavior.
- CLAUDE.md `aarch64-gated code` gate: documents the `make clippy-aarch64` requirement
  for arch-conditional changes, mirrors the existing `make clippy-linux` guidance.

### Known Arch Gaps (documented, not fixed in ma.1)
- `DenySpawn` (seccomp-bpf): no-op on aarch64 — `#[cfg(target_arch = "x86_64")]` guard;
  `EnforcementStatus.spawn_enforcement` = `"none"` already correct. Fix in ma.4.
- gVisor/runsc (universal tier): no aarch64 build; `which_runsc()` returns `None`;
  `BestEffort` behavior. Fix when runsc ships aarch64 support.
- Landlock FS, IsolateNetwork, IsolateMount, Landlock V4 net: all work on aarch64
  (same syscall numbers 444–446; `unshare` available on capable kernels).

## [dx.2] - 2026-07-04 (v0.54.0)

### Added
- HTTP approval surface: `POST /api/v1/approvals/:id/approve` and `POST /api/v1/approvals/:id/deny`
  routes on the management API, allowing operators to approve/deny pending agent actions without a
  FUSE mount.
- `control_tx: Option<mpsc::Sender<ControlCommand>>` on `ApiState`; HTTP approve/deny routes send
  `ControlCommand::Approve`/`Reject` to the scheduler's control channel.
- Fail-closed: 503 + `Retry-After: 1` if channel is full or unavailable; 404 on unknown ID; 400 on
  empty ID. No silent grant under any error condition.
- `ApprovalHttpApproved` and `ApprovalHttpDenied` flight events with `{id, agent_id}` data.
- `DataSource` trait extended with `load_approvals()`, `approve()`, `deny()` — both `FuseSource`
  and `HttpSource` implement all four methods.
- `HttpSource` uses a separate `mutation_client` (500 ms timeout) to prevent TUI freeze on approve/deny.
- Optimistic local removal: on `Ok(())` from `approve()`/`deny()`, the item is immediately removed
  from `approvals_items` to prevent stale-entry flicker for one tick.
- `agentctl approve <id>` and `agentctl deny <id> [--reason "..."]` CLI subcommands
  (`agentctl/src/approve.rs`), both auto-detecting FUSE vs. HTTP.
- `run_plain()` now calls `source.load_approvals()` so HTTP+plain mode shows pending approvals.
- `AgentInfo.status_detail: Option<String>` field parsed from HTTP snapshot JSON; shown in the
  AgentDetail view (e.g. approval ID while agent is `awaiting_approval`).
- FUSE control channel always wired on Linux (removed `maybe_session.is_some()` gate).
- 3 resolver-chain tests for `detect_source()`, 7 HTTP route tests (403/404/503 guards, happy paths,
  SSE framing), integration test `approve_happy_path_sends_command`.

### Fixed
- Resolved p7.7-ar-01 (SSE test), p7.7-ar-02 (detect_source tests), p7.7-ar-04 (status_detail).

## [p7.7] - 2026-07-04 (v0.53.0)

### Added
- Management HTTP API on `127.0.0.1:7999` (loopback-only, hyper v1). Routes:
  - `GET /healthz` — liveness probe.
  - `GET /api/v1/snapshot` — full `SchedulerSnapshot` JSON (agents, system stats).
  - `GET /api/v1/approvals` — pending approval queue.
  - `GET /api/v1/memory/:ns?limit=&offset=` — paginated Tier-3 KB entries (max 100 per page).
  - `GET /api/v1/events` — SSE fan-out of raw flight-recorder events.
- `[management]` section in agent TOML config: `enabled` (default false), `port` (default 7999),
  `bind_addr` (default "127.0.0.1").
- `ManagementStarted` and `ManagementRequest` flight events.
- `broadcast::Sender<String>` added to `FlightRecorder` via `with_broadcast()` builder; every
  `record()` call also sends the JSON line to the channel for SSE consumers.
- `agentctl/src/watch/source.rs` — `DataSource` trait with `FuseSource` (existing FUSE
  filesystem) and `HttpSource` (management API). `detect_source()` auto-detects: explicit
  `--url` flag → FUSE `system/` dir present → HTTP health-check on `127.0.0.1:7999` → error.
- `agentctl watch --url <http://HOST:PORT>` flag (also `AGENTCTL_URL` env var) to connect
  `agentctl watch` to a remote or host-side management API without requiring a FUSE mount.
- Manual `Serialize` impl for `AgentSnapshot` emitting `status` as a flat string plus optional
  `status_detail` for tuple variants (`AwaitingChild`, `AwaitingApproval`).
- `Serialize` derived for all other snapshot types (`SchedulerSnapshot`, `SandboxSummary`,
  `ServerEnforcement`, `PendingActionView`).

### Changed
- `agentd` and `agentctl` versions bumped to 0.53.0.

### Fixed
- SSE stream framing: flight-recorder lines are now `trim_end_matches('\n')` before wrapping
  in `data: ...\n\n`, preventing spurious triple-newline (empty event) after each real event.

## [dx.1] - 2026-07-04 (v0.52.0)

### Added
- `agentctl auth google` subcommand: runs PKCE OAuth2 authorization-code flow on the host
  Mac, writes `~/.agentos-secrets/google.json` (mode 0600, atomic tmp→rename). Args:
  `--client-id` / `--client-secret` (or env vars), `--port` (default 8585), `--force`.
  SHA256/base64url PKCE with RFC test-vector unit test.
- `~/.agentos-secrets:/run/secrets:ro` volume bind in `docker-compose.yml` `cos` service.
  Replaces seven hardcoded Google OAuth env vars with a single pre-provisioned secrets file.
- `entrypoint.sh cos` mode preflight: exits immediately with an actionable error if
  `/run/secrets/google.json` is absent, rather than hanging inside the agent loop.
- `oauth_mcp.py` reads `/run/secrets/google.json` at startup; env vars override if non-empty
  (backward compat). Hardcoded Google OAuth URL defaults eliminate `OAUTH_AUTH_URL`,
  `OAUTH_TOKEN_URL`, `OAUTH_SCOPES`, `OAUTH_ALLOWED_HOSTS`, `OAUTH_PROVIDER_NAME` from
  user-facing config. `oauth_start_auth` logs a deprecation notice when a refresh token is
  already present in the secrets file.

### Changed
- `agentctl` version bumped to 0.52.0. `agentd` version bumped to 0.52.0.
- `docker-compose.yml` `cos` service: removed `OAUTH_CLIENT_ID`, `OAUTH_CLIENT_SECRET`,
  `OAUTH_REFRESH_TOKEN`, `OAUTH_AUTH_URL`, `OAUTH_TOKEN_URL`, `OAUTH_SCOPES`,
  `OAUTH_ALLOWED_HOSTS`, `OAUTH_PROVIDER_NAME`, `OAUTH_CALLBACK_PORT` from env block.
  Required user config is now only `ANTHROPIC_API_KEY` in the shell.

## [h7.4] - 2026-07-03 (v0.51.0)

### Changed
- `ModelConfig.streaming` now defaults to `true` (was `false`). All agents stream text
  progressively to stdout by default. Configs that omit `streaming` silently gain streaming;
  set `streaming = false` to opt out for headless/batch use cases.
- `AnthropicGateway` reqwest client now sets `connect_timeout(10s)` in addition to the
  existing `timeout(120s)`. TCP handshake failures now surface in 10 s instead of 120 s.

### Fixed
- Agents running in Docker (including `google-agent`) appeared to hang indefinitely with
  zero terminal output. Root cause: `streaming = false` default blocked all stdout until
  `AgentEffect::Completed`. With the OAuth flow, Turn 2's URL was silently consumed before
  `oauth_check_auth` started blocking on port 8585, making the flow impossible to complete.
- `infer_with_stream` (streaming path, now the default) was missing the `is_connect()` retry
  that `infer()` (non-streaming) already had. With `connect_timeout(10s)` now active, TCP
  handshake failures on the streaming path would fast-fail without retry. Fixed: `send_once`
  now used for both paths with identical one-retry-on-`is_connect()` semantics.
- `make_infer_future`'s streaming branch never emitted `InferenceTransportRetried` even when
  `transport_retries = 1` was correctly computed and returned. The non-streaming branch already
  emitted this event. Fixed: added the `transport_retries > 0` guard to the streaming arm;
  also added `transport_retries` to the `InferenceStreamCompleted` payload for consistency.

## [h7.2-ar-01] - 2026-07-01 (v0.50.0)

### Added
- Generic `agent` Docker entrypoint mode: `TEMPLATE_NAME=<name> AGENT_TASK="..." docker compose run --rm agent`
  lowers any single-agent template to a valid TOML config via `agentctl spawn --dry-run`, rewrites
  `../docker/` paths to `/etc/agentd/` (Docker layout), and execs `agentd`. Covers `scout`,
  `librarian`, `google-agent`, and all future templates without per-template entrypoint cases.
- `DRY_RUN_ONLY=1` env var exits before `exec agentd` and prints the rendered `/data/agent.toml`
  — enables smoke testing path rewriting without a live API key.
- `docker-compose.yml` `agent` service: named `agent-data` volume, `HOME=/data` for OAuth token
  persistence, `AGENTOS_REPO_TEMPLATES_DIR` for explicit template resolution, full OAuth + web-search
  env wiring, per-template task examples as inline comments, Google Cloud Console setup instructions.
- `set -o pipefail` added to `docker/entrypoint.sh` — catches `agentctl` failures that `set -e`
  alone misses in pipeline context.

### Fixed
- Removed static `8585:8585` port binding from both `cos` and `agent` compose services — eliminates
  the port conflict when both services are configured. OAuth callback now requires `--service-ports`.
- `agentctl list-templates` printed on template-not-found error so the valid template names are
  immediately visible without reading docs.
- `code-aware` template exits with a clear error (requires `runsc`/gVisor not in standard image)
  instead of failing silently mid-run when `runsc` is not found.

## [con.1] - 2026-06-30 (v0.49.0)

### Fixed
- TCP keepalive (`SO_KEEPALIVE`, 15 s probe interval) on the Anthropic reqwest client keeps
  Docker NAT conntrack entries alive through long MCP wait periods — fixes silent connection
  drops on the third inference call in multi-turn cos.1 runs.
- Retry once on `is_connect()` errors (stale pooled connection reuse); non-streaming path only.
  Streaming retry is intentionally omitted (would cause duplicate stdout + double billing).
- Removed `streaming = false` stopgap that was previously sed-patched into the Docker cos
  entrypoint; native TCP keepalive makes the workaround unnecessary.

### Added
- `InferenceTransportRetried` flight event emitted when a stale-connection retry succeeds
  (`agent_id`, `model`, `retries: u32` in payload).
- `InferenceResponse.transport_retries: u32` field (`#[serde(default)]` for checkpoint compat).
- OTEL exhaustiveness guard (`otel/tests/event_kind_coverage.rs`) updated for the new event.
- Docker `cos` mode in `entrypoint.sh` — fully self-contained CoS launch with `/data` volume
  and absolute-path rewriting (replaces ad-hoc dev workflow).
- OAUTH_CALLBACK_PORT support in `oauth_mcp.py` — fixes callback port binding inside Docker.

## [cos.1] - 2026-06-27 (v0.48.0)

### Added
- `agentd/cos.agents.toml` — three-agent Chief of Staff system: Executive Orchestrator
  (cron-triggered, always-on, `max_turns=200_000`, `token_budget=5_000_000_000`), Inbox Agent
  (read-only Gmail via OAuth2 sidecar), Curator (KB persistence to `ops:briefs`/`ops:entities`).
- `templates/cos-orchestrator.template.toml` — orchestrator template with lifetime-correct defaults;
  `gated_requires` warning for OAuth + cron env vars.
- `templates/cos-inbox.template.toml` — read-only Gmail analyst; explicit capability scoping
  (no Spawn, no FsWrite, MCP-only); `memory.enabled=false`.
- `templates/cos-curator.template.toml` — KB curator (Haiku model, mechanical writes); scoped to
  `ops:briefs` (log) + `ops:entities` (scratch) only.
- `docs/RUNBOOK.md §11` — Chief of Staff runbook: prerequisites, env-var block, first-run OAuth
  dance, brief location, 7 verification commands, agentctl watch keys, known limits, troubleshooting.
- Template catalogue test updated to expect 13 templates (was 10).

### Architecture
- Composes 8 shipped primitives without new core code: cron (h7.3) + OAuth (h7.2) +
  scheduler/spawn (p1) + KB (p5) + approval gate (p7.4) + egress/receipts (p7.5) +
  gVisor floor (p7.6) + OTLP (obs.1–3).
- Child agents use date-stamped IDs (`inbox-YYYY-MM-DD`, `curator-YYYY-MM-DD`) to avoid
  outcome-map collision across daily cycles.
- Orchestrator prompt has explicit LOOP_BACK step instructing the model to call
  `wait_for_trigger` again after writing the brief (prevents premature FinalAnswer).
- Trust story demonstrable: token-absence, egress-denial, OTLP+signed-receipts,
  approval-gated send, bounded cost.

## [h7.3] - 2026-06-27 (v0.47.0)

### Added
- `docker/cron_mcp.py` — cron/interval trigger MCP server; `wait_for_trigger()` poll-and-retry
  design (MCP_TIMEOUT=30s constraint); supports 5-field cron (UTC) and `every N(s|m|h)` intervals;
  bounded grammar (exit 1 on unsupported tokens); POSIX DOW mapping `(weekday+1)%7`; debounce via
  `_wait_start`; `TRIGGER_MAX_WAIT_S` global abort; 5 self-tests.
- `docker/fs_watch_mcp.py` — filesystem watch trigger MCP server; polls via `os.scandir` every
  `TRIGGER_POLL_INTERVAL_S` (default 2s); tracks mtime_ns + size + inode (detects delete+recreate);
  `TRIGGER_IGNORE_PATTERNS` (fnmatch globs); `TRIGGER_QUIET_PERIOD_S` debounce; 6 self-tests.
- `docker/webhook_mcp.py` — HTTP webhook trigger MCP server; `ThreadingHTTPServer` (no HOL
  blocking); Content-Length cap before read (64 KB); `hmac.compare_digest` HMAC-SHA256; timestamp
  tolerance ±5 min always applied; queue full → 429; `rejected_count` in waiting response; 6
  self-tests.
- `templates/cron-agent.template.toml` — scheduled agent template (cron or interval trigger).
- `templates/webhook-agent.template.toml` — webhook-driven agent template.
- `templates/watcher.template.toml` — updated: `gated_requires` removed (now fully operational),
  wired to `fs_watch_mcp.py`, sample tasks added.
- `docs/MCP_SERVERS.md` — Trigger Servers section with "How trigger agents work" explanation,
  per-server TOML snippets, webhook security notes, and curl example.
- 2 new Rust template tests: `catalogue_watcher_no_longer_gated`,
  `catalogue_trigger_templates_lower_to_valid_config`.
- `Makefile test-harness` — extended to self-test all 6 MCP servers (3 existing + 3 trigger).
- Plan: `docs/plans/h7.3-event-trigger-mcp-servers.md` (26 autoplan decisions, all mechanical).

### Changed
- `docs/ROADMAP.md` — h7.3 marked complete (v0.47.0).
- `docs/MCP_SERVERS.md` — webhook "replay protection" note clarified: timestamp window is ±5 min
  (not nonce-based; sub-window replays require HMAC + application-level dedup).
- `templates/webhook-agent.template.toml` — added comment that changing `TRIGGER_WEBHOOK_PORT`
  requires updating `net_ports` in both capability sections; noted `TRIGGER_WEBHOOK_SECRET`
  strongly recommended when `write_file` is enabled.
- Workspace tests: 1025 → 1027.

### Fixed
- `docker/cron_mcp.py` — `_advance_next_fire()` wrapped in try/except `RuntimeError`; a
  non-repeating cron schedule no longer crashes the server after the first trigger fires.
  Returns `{status: "timeout", message: "No future fire time..."}` instead.
- `docker/cron_mcp.py` — moved `import os` to top-level imports (was inadvertently at module bottom).
- `docker/webhook_mcp.py` — `OSError` on port bind is now caught in `_init()` with a clean
  error message + `sys.exit(1)` instead of a raw Python traceback.

## [h7.2] - 2026-06-27 (v0.46.0)

OAuth MCP Sidecar (harness increment) — generic OAuth2 authorization-code + PKCE Python MCP server,
Google agent template, and full operator quickstart docs.

- **`docker/oauth_mcp.py`** — new Python MCP server with three tools:
  - `oauth_start_auth()` → starts local `127.0.0.1:<random>/callback` server, returns PKCE auth URL
  - `oauth_check_auth()` → exchanges code for tokens after browser flow; returns `{ready: bool, scopes: [...]}`
  - `oauth_call_api(url, method?, headers?, body?)` → authenticated HTTPS call, auto-refresh on 401, host allowlist enforced
- **PKCE (RFC 7636 S256)** — `secrets.token_urlsafe(64)` → 86-char verifier (safe below 128-char hard ceiling)
- **Tool state machine** — `idle → pending → authorized`; `oauth_call_api` before auth returns `{error: "auth_not_ready"}`
- **Threading** — callback server runs on daemon thread; all token state protected by `threading.Lock`
- **AuthSession dataclass** — explicit session object (state, code_verifier, redirect_uri, expires_at, server, thread, result, lock)
- **CSRF protection** — `state` nonce validated exactly in callback (RFC 6749 §10.12); path must be `GET /callback`
- **SSRF dual-layer** — hostname allowlist (`OAUTH_ALLOWED_HOSTS`) + IP block (`_is_ssrf_blocked`) rejects loopback/RFC1918/link-local
- **Token file** — refresh token atomic-written to `~/.agentos-oauth/<OAUTH_PROVIDER_NAME>.json` (mode 0600, dir 0700)
- **`OAUTH_REFRESH_TOKEN` bypass** — env var skips the dance entirely; `oauth_check_auth` returns ready immediately
- **Startup validation** — server prints `oauth_mcp: missing required env OAUTH_CLIENT_ID` and exits 1 if required vars absent
- **`host_not_allowed` error body** — `{"error": "host_not_allowed", "host": "<rejected>"}` for actionable diagnosis
- **`--test` self-test** — 10-case matrix (no real credentials required); `python3 docker/oauth_mcp.py --test`
- **`templates/google-agent.template.toml`** — new template; pre-sets 5 of 7 env vars for Google (AUTH_URL, TOKEN_URL, SCOPES, ALLOWED_HOSTS, PROVIDER_NAME); operator sets only `OAUTH_CLIENT_ID` + `OAUTH_CLIENT_SECRET`; `gated_requires` warning with GCP Console note; includes `gmail.googleapis.com` in allowed hosts
- **`docs/MCP_SERVERS.md`** — new `oauth_mcp` section: full TOML snippet, GCP console setup checklist (incl. "Desktop app" callout), 2-var export list, approval dance sequence (3 steps), error reference table; updated Known servers note to remove "deferred to future increment" caveat

## [h7.1] - 2026-06-26 (v0.45.0)

Standard MCP Servers (harness increment) — three first-party Python MCP servers in `docker/`,
a new `ShellExec` subprocess-sandbox capability, and template updates for scout, code-aware, and librarian.

- **`docker/shell_mcp.py`** — `run_command` tool: `shell=True` subprocess, 30 s default/120 s max timeout,
  stdout/stderr capped at 64 KB, `--test` self-test mode. Exit code, stdout, and stderr returned as JSON.
- **`docker/http_mcp.py`** — `fetch_url` tool: HTTPS-only, no redirect following (returns `is_redirect: true`
  + Location header for 3xx), response body capped at 4 MB. `--test` self-test mode.
- **`docker/search_mcp.py`** — `web_search` tool: Brave Search API, `count` param (1–10), graceful
  `isError: true` with setup instructions when `BRAVE_SEARCH_API_KEY` is absent. `--test` mode.
- **`ShellExec` capability** (`agentd/src/capability.rs`) — subprocess sandbox capability; when present in
  an MCP server's `capabilities`, suppresses `DenySpawn` so the server subprocess can fork/exec shell
  commands. `agentctl spawn` parses alias `shell-exec`; `display_cap()` renders `"ShellExec"` in the TUI.
- **Template updates** — scout gains `http_fetch` + `web_search` MCP blocks + 2 new sample tasks;
  code-aware gains `shell_exec` + `http_fetch` (both with `isolation = "gvisor"`); librarian gains
  `http_fetch` + `web_search` + `net_ports = [443]` capability.
- **`docs/MCP_SERVERS.md`** — new "Standard servers (bundled)" section with per-server table, self-test
  commands, and copy-paste TOML snippets.
- **`agentd/agent.toml`** — commented examples for all 3 standard servers.
- **`Makefile`** `test-harness` target runs `--test` self-tests for all 3 servers.
- **`passenv` field on `McpServerConfig`** — forward named env vars from the parent process into stdio
  MCP server subprocesses. Required for `BRAVE_SEARCH_API_KEY` since `env_clear()` strips the env.
  Templates updated: `passenv = ["BRAVE_SEARCH_API_KEY"]` on `web_search` servers.
- **search_mcp.py fixes** — removed `Accept-Encoding: gzip` (urllib cannot decode gzip; caused silent
  failure on every live query); non-integer `count` now caught with try/except instead of crashing the server.
- **shell_mcp.py fixes** — non-integer `timeout_s` now caught; `start_new_session=True` + `os.killpg`
  for proper process group cleanup on timeout (eliminates orphan grandchild processes).
- **JSON parse error responses** — all three Python servers now return a JSON-RPC `-32700` Parse error
  response on `json.JSONDecodeError` instead of silently returning (which caused Rust MCP client to wait
  30 s then mark the connection broken for the rest of the session).
- **Post-SIGKILL communicate timeout** (`shell_mcp.py`) — after `os.killpg`, `communicate()` is called
  with `timeout=5` to prevent indefinite blocking when a grandchild escapes the process group.
- **HTTP method allowlist** (`http_mcp.py`) — only `GET POST PUT DELETE PATCH HEAD OPTIONS` accepted;
  CONNECT and TRACE rejected to prevent port-scanning and request-smuggling vectors.
- **SSRF loopback/RFC1918 block** (`http_mcp.py`) — DNS-resolved IP of the target host is checked
  against `ipaddress.ip_address.is_loopback / .is_private / .is_link_local` before any connection.
  Blocks `127.x`, `::1`, `169.254.x`, `10.x`, `172.16-31.x`, `192.168.x`.
- **LD_PRELOAD/linker var stripping** (`shell_mcp.py`) — `LD_PRELOAD`, `LD_LIBRARY_PATH`, `LD_AUDIT`,
  `LD_DEBUG`, `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH` are stripped from any agent-supplied `env`
  dict before merging with the process environment.
- **`PASSENV_BLOCKLIST`** (`agentd/src/tools/mcp.rs`) — `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`
  are blocked from passenv forwarding; scheduler overwrites the key with an ephemeral scoped key after
  spawn, so forwarding it here would expose the production key to an untrusted subprocess.
- **`McpPassenvForwarded` flight event** — emitted after each MCP server spawn when `passenv` is
  non-empty; records `forwarded`, `blocked`, and `absent` name lists (never values).
- 1025 workspace tests pass (up from 1009).

## [obs.3] - 2026-06-26 (v0.44.0)

OTLP sidecar gap remediation — copy-truncate fast-grow detection (content sentinel) + export-drop counting.

- **Content sentinel** (`otel/src/tail.rs`) — `FileTailer` stores `last_sentinel: Vec<u8>` (last 64 bytes
  at last-consumed offset). On each poll, when same inode and `cur_len >= offset`, the sentinel window is
  re-read and compared; a mismatch means copy-truncate rotation occurred between polls. Three guards prevent
  false positives: (1) skip check when `offset < SENTINEL_SIZE`; (2) skip check when `last_sentinel.len() !=
  SENTINEL_SIZE` (not yet populated — prevents spurious rotation on first append after `from_beginning=false`
  startup); (3) skip sentinel capture when `new_offset < SENTINEL_SIZE` (prevents u64 underflow). Fixes
  obs.2-ar-01.
- **Export-drop counting** (`otel/src/main.rs` + `otel/src/exporter.rs`) — `export_drops: u64` tracks
  `force_flush()` error count. SIGTERM, SIGINT, and periodic stats paths all use
  `tokio::task::spawn_blocking(move || p.force_flush())` (non-blocking; `SdkTracerProvider` wraps `Arc`,
  `clone()` is O(1)). SIGTERM/SIGINT print a final stats line before break. Periodic stats path records
  `export_drops` delta via new `export_drops_counter` OTLP metric (`agentos.otel.export_drops`, unit
  "failures"), separate from channel-drop counter. Code comment: "export_drops counts flush-attempt
  failures, not spans; one error may represent many lost spans." Fixes obs.2-ar-02.
- **Stats line** — updated format: `exported=N open=M dropped=D export_drops=E flushed_on_rotation=R`.
- **New tests** — 3 new `FileTailer` tests: `test_tail_copy_truncate_fast_grow` (sentinel detects
  copy-truncate when new file grows past old offset); `test_tail_sentinel_no_false_positive` (normal append
  does not trigger rotation); `test_tail_startup_no_false_positive` (first append after `from_beginning=false`
  start does not trigger rotation).
- 1009 workspace tests pass (up from 1006).

## [obs.2] - 2026-06-26 (v0.43.0)

OTLP sidecar hardening — batch exporter, validation unit tests, log rotation flush.

- **`BatchSpanProcessor`** (`otel/src/exporter.rs`) — replaced `with_simple_exporter` with
  `BatchSpanProcessor::builder` + `BatchConfigBuilder` (`max_export_batch_size=512`,
  `max_export_timeout=30s`); `OTEL_EXPORT_BATCH_DELAY_MS` env var (default: 5000ms) wires into
  `with_scheduled_delay`; startup banner now includes `batch_delay_ms`.
- **SIGTERM flush** (`otel/src/main.rs`) — `tokio::signal::unix` SIGTERM handler calls
  `sb.drain_all(now_ns, "shutdown")` + `provider.force_flush()` before exit; prints
  `"agentos-otel: shutdown — flushed N open spans"`; handles short-run sessions (<30s) before
  the idle watchdog fires.
- **Log rotation flush** (`otel/src/main.rs` + `otel/src/span_builder.rs`) — `rotated` flag
  from `tailer.poll()` now handled: calls `sb.reset_for_rotation(now_ns)` which drains all open
  spans AND resets `trace_id`/`run_id`/`run_span_id`/`agent_span_ids`/`span_counter` to prevent
  phantom span relationships across file rotations; rotation-flushed spans tagged with
  `forced_close=log_rotated`; tracked separately as `flushed_on_rotation` (not counted in
  `exported_count`); printed in periodic stats line.
- **Validation error improvements** (`otel/src/main.rs`) — world-writable error now includes
  `(fix: chmod o-w <path>)`; embedded-credentials error includes OTLP_HEADERS alternative;
  absolute-path error includes example path; help text updated to `'true' or '1'` for all
  boolean env vars.
- **Validation unit tests** (8 new in `otel/src/main.rs`) — `validate_log_path_rejects_relative`,
  `validate_log_path_rejects_non_jsonl`, `validate_log_path_accepts_valid_missing_file`,
  `validate_log_path_rejects_world_writable` (unix-gated), `validate_endpoint_rejects_non_http`,
  `validate_endpoint_rejects_embedded_credentials`, `validate_endpoint_accepts_http`,
  `validate_endpoint_accepts_https`.
- **TODOS.md** — obs.1-ar-01/02/03 resolved; copy-truncate detection gap and backend-down
  invisibility added as obs.2-ar-01/02.
- 1006 workspace tests pass (up from 998).

## [obs.1] - 2026-06-26 (v0.42.0)

OTLP observability sidecar — `agentos-otel` tails `flight.jsonl` and exports OpenTelemetry traces to any OTLP backend (Jaeger, Grafana Tempo, Honeycomb, etc.).

- **`otel/`** (new workspace crate `agentos-otel`) — standalone binary; `tail.rs` with `(dev, ino, offset)` triple tracking for log rotation (rename + copy-truncate); `span_builder.rs` state machine that reconstructs spans from flight events (agent/turn/inference/tool hierarchy); `exporter.rs` using OTLP HTTP/protobuf via `opentelemetry-otlp 0.17.0` + `opentelemetry_sdk 0.24.0`; `semconv.rs` with GenAI semconv v1.29.0 attribute constants; `otel/tests/event_kind_coverage.rs` compile-time exhaustiveness guard over all 58 `EventKind` variants.
- **Trace model** — `scheduler_started.run_id` (UUID v4, hyphens stripped → 32-hex) is the OTLP trace ID; agents are child spans; inference/tool calls are grandchild spans; orphan events synthesize missing parent spans.
- **Policies** — duplicate open event force-closes existing span as `UNFINISHED (reason=duplicate_open)`; inactivity watchdog (default 30 s, `OTEL_IDLE_TIMEOUT_SECS`) drains open spans; backpressure channel capped at 10,000 spans (`agentos.otel.spans_dropped` counter).
- **agentd changes** — 2 new `EventKind` variants: `SchedulerStarted` (emits `run_id` UUID v4 + `config_hash`) and `SchedulerStopped` (emits `run_id` + `agent_count`); `uuid 1.x` dep added to agentd; events emitted around `scheduler.run()` in `main.rs`.
- **`docker/otel-compose.yml`** (new) — Jaeger all-in-one with OTLP ports 4317/4318 and UI at 16686; `OTEL_REDACT_PREVIEWS=true` guidance.
- **`docs/CONVENTIONS.md`** — `scheduler_started` and `scheduler_stopped` rows added to event taxonomy table.
- **Env vars** — `FLIGHT_LOG_PATH`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME` (default: `agentos`), `OTEL_TAIL_FROM_BEGINNING`, `OTEL_POLL_INTERVAL_MS`, `OTEL_IDLE_TIMEOUT_SECS`, `OTEL_REDACT_PREVIEWS`, `OTEL_SESSION_ID`, `OTEL_EXPORT_PROTOCOL`.
- **Token metrics** — `gen_ai.client.token.usage` OTLP counter with labels `gen_ai.system`, `gen_ai.request.model`, `session_id`, `token.type` (input/output); extracted from closed `gen_ai.chat` spans and forwarded to the same OTLP endpoint via a separate `SdkMeterProvider`.
- **Security** — `FLIGHT_LOG_PATH` validated (absolute, `.jsonl` extension, not world-writable); `OTEL_EXPORTER_OTLP_ENDPOINT` validated (`http://`/`https://` only, embedded credentials rejected); `otel/` is a separate workspace crate, keeping OTLP deps out of the 6 MB `agentd` binary; `parse_ts` uses saturating arithmetic to prevent u64 overflow on malformed far-future timestamps.
- **Dockerfile** — `agentos-otel` added to the builder and runtime stages alongside `agentd` and `agentctl`.
- 998 workspace tests pass.

## [p7.6] - 2026-06-25 (v0.41.0)

Universal-tier isolation floor — gVisor/runsc child process wrapping for agent workloads that host untrusted or foreign-framework code.

- **`agentd/src/universal.rs`** (new) — `UniversalAgent` struct with `ephemeral_key` field; `spawn()` takes `ephemeral_key: &str`, clears child env, injects PATH/HOME/USER/LANG/TMPDIR + per-agent ephemeral key + `ANTHROPIC_BASE_URL`; `stdin(Stdio::null())` to avoid shared stdin fd; `kill()` (SIGTERM → 5 s → SIGKILL); `try_wait()`, `pid()`, `wall_seconds()`; `which_runsc()` probes PATH.
- **`agentd/src/config.rs`** — `AgentTier` enum (`Native` | `Universal`); 5 new `AgentConfig` fields: `tier`, `command`, `args`, `isolation`, `max_wall_seconds` (all `#[serde(default)]`).
- **`agentd/src/scheduler.rs`** — `universal_agents: HashMap<String, UniversalAgent>` on `SchedulerState`; spawn block generates per-agent ephemeral key and registers it in `proxy_registry`; deregisters on exit/timeout/shutdown; spawn/wall-timeout/nonzero-exit failures propagated to `state.outcomes`; duplicate ID check covers both native and universal agent maps; `poll_universal_agents()` enforces `max_wall_seconds` and inserts into `outcomes`; `update_snapshot()` encodes actual isolation as `"universal:gvisor"` or `"universal:none"` in the tier field.
- **`agentd/src/events.rs`** — 3 new event kinds: `UniversalAgentStarted`, `UniversalAgentExited`, `UniversalAgentIsolationDegraded` (note: `UniversalOutputTruncated` removed — stdout is inherited, not buffered).
- **`surfaces/src/snapshot.rs`** — `tier: Option<String>` + `pid: Option<u32>` on `AgentSnapshot`.
- **`surfaces/src/agents_fs.rs`** — `OFF_TIER = 11`, `OFF_PID = 12`; 13 fixed inodes per agent dir; `tier`/`pid` virtual files.
- **`agentctl/src/watch/reader.rs`** — reads `tier` file (parses `"universal:gvisor"` → `tier="universal"`, `isolation="gvisor"`); adds `isolation: String` to `AgentInfo`.
- **`agentctl/src/watch/views.rs`** — universal agents show `N/A` for context tokens; AgentDetail badge shows actual isolation (`ISO: gvisor` or `ISO: none`) from snapshot; plain-mode output includes tier info.
- **`distro/kernel-extras.config`** — gVisor/KVM comment block added.
- **`docs/CONVENTIONS.md`** — 3 new event rows + 2 new FUSE path rows (`tier`, `pid`).
- **`templates/langchain-worker.template.toml`** (new) — universal-tier template with `gated_requires = "gvisor"`.
- **Security hardening (post-review)** — deregister ephemeral key before `kill()` in both shutdown and wall-timeout paths to close SIGTERM auth window; `egress_addr=None` with universal agents upgraded from `tracing::warn!` to `anyhow::bail!` (fail-fast at startup); native-tier `command` field now rejected at startup.
- 979 workspace tests pass.

## [p7.5b] - 2026-06-24 (v0.40.0)

Universal-tier HTTP forwarding proxy — real key-routing gateway replacing the 501 stub.

- **`agentd/src/egress.rs`** — `ProxyRegistry` (`RwLock<HashMap<String, ProxyEntry>>`);
  `ProxyPolicy { allowed_hosts, token_budget_remaining }`; `register()` / `deregister_by_key()`
  / `entry_for_key()`; `start_http_proxy()` binds hyper v1 listener + routes requests via
  `handle_proxy_request()`; ephemeral key identity via `x-api-key` header; real
  `ANTHROPIC_API_KEY` lives only in proxy memory; hop-by-hop header stripping (Host,
  Content-Length, Transfer-Encoding, Connection); 8 MB response cap → 502; 120 s upstream
  timeout → 504; `Accept: text/event-stream` → 501 with structured `detail` field;
  `json_error_response()` helper; `record_proxy_failed()` flight event; `start_http_stub()`
  kept for backward compat; 12 new unit tests.
- **`agentd/src/scheduler.rs`** — `egress_addr: Option<SocketAddr>` + `proxy_registry:
  Option<Arc<ProxyRegistry>>` on `SchedulerState`; builder methods `with_egress_addr()` +
  `with_proxy_registry()`; `egress_addr()` getter; `update_snapshot()` writes
  `http://{addr}` string; 4 new tests.
- **`surfaces/src/snapshot.rs`** — `egress_addr: Option<String>` on `SchedulerSnapshot`.
- **`surfaces/src/agents_fs.rs`** — `INO_SYS_EGRESS_ADDR = 17`; `sys_file_content()` arm
  returns address or `"not configured\n"`; `SystemDir` lookup + `getattr`/`open` range
  checks + `readdir` entry; 2 new tests.
- **`agentd/src/main.rs`** — captures `real_api_key` before env overwrite; constructs
  `ProxyRegistry`; `start_http_proxy()` → `egress_bound_addr`; fail-closed on bind error;
  passes `with_egress_addr()` + `with_proxy_registry()` to scheduler.
- **RUNBOOK §9** — "HTTP egress proxy" section: enabling, discovering bound port, wiring a
  workload, streaming limitation, verifying proxy started; `[egress]` example in config ref.
- **`agentd/agent.toml`** — commented `[egress]` example block.
- **QA hardening** — 5 CRITICAL/HIGH fixes from ship review: pre-forward budget check (429),
  upstream error message sanitized (no `format!("{e}")`), buffer overflow check before extend,
  `Ordering::AcqRel` on budget `fetch_update` to close TOCTOU race, accept loop `continue` on
  transient errors; content-type allowlist (only `application/json` passes); float token parsing
  (`as_f64()`) for robustness; test helpers DRY-ified via `start_test_proxy` + `register_workload`;
  `proxy_strips_ephemeral_inserts_real_key` rewired to mock upstream (no real API calls in tests).
- **Adversarial-review hardening** — H-2: `start_http_proxy_impl` visibility narrowed (was
  `pub(crate)`, tests use `super::*`); loopback-only assertion added; M-2: `content-type` removed
  from `PASSTHROUGH_HEADERS` — proxy always sets `application/json` upstream regardless of workload
  value; two `unwrap()` in response builder paths changed to `expect()`; H-1 TOCTOU over-spend
  and M-3 `anthropic-beta` passthrough deferred to p7.6 (see TODOS.md `p7.5b-ar-*`).
- **968 workspace tests** (up from 945 in p7.5, +23 in p7.5b).

## [p7.5] - 2026-06-23 (v0.39.0)

Native-tier egress governance — tamper-evident signed audit receipts, boundary
secret rewriting, in-process inference mediator, and offline chain verifier.

- **`agentd/src/evidence.rs`** — `EvidenceWriter` with Ed25519 signing (via `ring`)
  and SHA-256 hash-chaining; `ActionReceipt` / `ReceiptBody` serde types; genesis
  hash = 64 zero hex chars; `record_allowed()` / `record_denied()` return u64 seq;
  `verify_chain()` for offline verification; private key written at 0600 permissions
  on Unix; `resume_chain()` on restart; 5 unit tests.
- **`agentd/src/egress.rs`** — `EgressProxy { writer, recorder }`; `record_inference()`
  emits `EgressBrokered` + `ActionReceiptEmitted`; `record_denied()` emits `EgressDenied`
  + `ActionReceiptEmitted`; `start_http_stub()` binds hyper v1 HTTP server returning 501
  on all paths (p7.5b readiness).
- **Boundary secret rewriting** — after `AnthropicGateway::from_env()` captures the real
  `ANTHROPIC_API_KEY`, `main.rs` overwrites the env var with `sk-ant-PLACEHOLDER-agentd`
  so a memory dump of the agent process yields an inert string.
- **`[egress]` TOML config** — `EgressConfig { evidence_path, key_path, proxy_addr }`;
  `#[serde(default)]`; fail-closed startup if `EvidenceWriter::open()` fails; evidence
  path resolved to absolute at startup (OV-1 pattern).
- **Scheduler threading** — `SchedulerState.egress: Option<Arc<EgressProxy>>`; builder
  `Scheduler::with_egress()`; `make_infer_future()` calls `record_inference()` on both
  streaming and non-streaming paths after a successful response.
- **4 new `EventKind` variants** — `EgressBrokered`, `EgressDenied`,
  `ActionReceiptEmitted`, `EgressProxyFailed`.
- **`agentctl verify`** — offline chain verifier subcommand; reads `evidence.jsonl` +
  Ed25519 public key file; prints `chain ok: N receipts verified` on success.
- **Inspector `Egress` filter** — cycles `All → Errors → Sandbox → CapDenied → Egress → All`
  in `agentctl watch`; matches `egress_brokered`, `egress_denied`, `action_receipt_emitted`.
- **`docs/CONVENTIONS.md`** — 4 new event rows for the egress tier.
- **`ring` / `hyper` / `hyper-util` / `http-body-util`** made explicit in `Cargo.toml`
  (all were already transitive; now declarative).
- 945 workspace tests (up from 932); 13 new tests.

## [p7.4] - 2026-06-22 (v0.38.0)

Human-in-the-loop approval gate: agents can pause and ask the operator for
explicit approval before executing high-risk actions. The operator resolves
pending approvals from the `agentctl watch` TUI or any shell script.

- **`request_approval` native tool** — `kind`, `risk`, `summary`, `args_json`
  inputs; returns `{"approved":true}` or `{"approved":false,"reason":"..."}`;
  available without capability grant (implicit; requires `HumanApprovals` cap).
- **`AgentEffect::RequestApproval`** — new scheduler effect; agent yields until
  operator resolves via `/agents/control`.
- **`ParkedApproval`** / `pending_approvals: HashMap<String, ParkedApproval>`
  on `SchedulerState` — approval ID counter, parked sender, snapshot fields.
- **`ControlCommand` tagged enum** — `{"approve":{"id":"…"}}`,
  `{"approve":{"id":"…","auto_approve_kind":"write_file"}}`,
  `{"reject":{"id":"…","reason":"…"}}`; existing spawn path unchanged.
- **`PendingActionView`** in `surfaces/` — `id`, `agent_id`, `kind`, `risk`,
  `summary`, `args_json`, `age_secs`; exposed via `SchedulerSnapshot`.
- **`INO_APPROVALS = 16`** — read-only root-level FUSE pseudofile
  `/agents/approvals`; JSONL one `PendingActionView` per line; `[]\n` sentinel
  when empty.
- **`AgentStatus::AwaitingApproval(String)`** — new variant; renders as
  `awaiting_approval:<id>` in FUSE status file and `agentctl` table.
- **Checkpoint FORMAT_VERSION 2→3** — `pending_approvals` field added with
  `#[serde(default)]` for backward compat.
- **Flight events** — `ApprovalRequested`, `ApprovalGranted`, `ApprovalRejected`.
- **`agentctl watch` Approvals view** (`[a]` from dashboard) — list of pending
  actions with ID/agent/kind/risk/summary/age columns; `Enter` opens 3-option
  confirm dialog (`[a]pprove`, `[d]` approve+don't-ask-again, `[r]eject`);
  reject reason text input; `write_control_command()` helper (libc::close flush
  guard); `approvals_items` refreshed every tick (time-critical).
- **`agentctl/src/watch/approvals.rs`** — `ApprovalsMode` enum,
  `ApprovalsViewState` struct; 4 unit tests.
- **`agentctl/src/watch/reader.rs`** — `PendingAction` struct +
  `read_approvals()` (handles `[]\n` sentinel + JSONL).
- **`views.rs`** — `render_approvals()` TUI function; `awaiting_approval`
  case in `status_style()` (magenta); `[a]pprove` hint in dashboard footer;
  approvals section in `render_plain()`.
- **`docs/CONVENTIONS.md`** — 3 new event rows, FUSE path row for
  `/agents/approvals`, `awaiting_approval:<id>` added to status table.
- 932 workspace tests (up from 902); 30 new tests.

## [p7.3] - 2026-06-21 (v0.37.0)

Write JSON to `/agents/control` to inject a new agent into a running scheduler
without restarting it. `agentctl watch` spawn view routes through the control
surface when available, staying in the TUI with a green banner after injection.

- **`agentd/src/control.rs`** — `OperatorSpawnRequest` (task, id, max_turns,
  token_budget, priority, capabilities) + `parse_control_command()` with
  empty-task rejection; 5 unit tests.
- **FUSE surface** (`surfaces/`) — `INO_CONTROL = 15` write-only pseudo-file at
  `/agents/control`; per-fh `write_buffers` (64 KiB cap → EFBIG); dispatches
  on `flush()` and `release()`; `perm 0o222`; `read()` returns empty bytes;
  `MountOption::RO` removed; `mount()` accepts `Option<ControlDispatch>`.
- **`ControlDispatch`** — `Arc<dyn Fn(&[u8]) -> i32 + Send + Sync>` callback
  (opaque, avoids circular dep); `try_send` in main.rs returns EBUSY if channel
  full; explicit `libc::close()` in agentctl propagates the error.
- **Scheduler** (`agentd/src/scheduler.rs`) — `with_control(rx)` builder;
  two-case `'main` loop (select on `control_rx` or break when empty, interleave
  when pending); `dispatch_operator_spawn()` (ID validation, collision guard,
  `validate_child_id`, inserts into `parent_map`); gated on `maybe_session`
  (fixes deadlock when FUSE not mounted).
- **`agentctl watch`** — `SpawnOutcome` enum (`InjectedViaControl` keeps TUI,
  `FellBackToExec` replaces process); JSON preview when control surface present;
  green banner on successful injection.
- **Flight events** — `FuseControlReceived`, `FuseControlError`.
- **`docs/CONTROL_SURFACE.md`** — operator reference (wire format, errno table,
  shell examples, TUI integration, EBUSY footgun warning).
- 902 workspace tests (up from 889); 13 new tests.

## [p7.2] - 2026-06-20 (v0.36.0)

Set `streaming = true` in `[model]` and text chunks print to stdout as the
model generates them, instead of waiting for the full response. The existing
run-to-completion path is unchanged — `streaming` defaults to `false`.

- **`InferenceRequest.streaming: bool`** — flag propagated from `ModelConfig`
  via `agent/mod.rs`; `DeferredInfer` carries it transparently.
- **`InferenceGateway::infer_with_stream()`** — new async trait method with
  default fallback (drops channel, calls `infer()`); `AnthropicGateway`
  overrides with a real SSE parser.
- **SSE parser** (`inference/anthropic.rs`) — `parse_sse_event()` helper +
  `parse_sse_stream()`; CRLF-safe; 1 MB line cap; `text_delta` → channel;
  `input_json_delta` → tool accumulator; index-ordered block assembly; sender
  `Err` check to abort on dropped receiver.
- **SSE correctness hardening** — `TextDelta` for an unregistered block index
  now returns `Err` instead of silently drifting state; `input_json` accumulator
  capped at 4 MB (`MAX_TOOL_INPUT_BYTES`) matching the non-streaming body limit;
  empty `input_json` (tool called with no arguments) folds to `{}` instead of
  failing JSON parse.
- **`make_infer_future()`** helper (`scheduler.rs`) — extracts the 30-line
  streaming dispatch block; used by both `enqueue_or_defer` and `drain_deferred`.
- **Scheduler streaming** — `tokio::join!(infer_fut, print_fut)`; async stdout
  via `tokio::io::AsyncWriteExt`; final `\n` after stream; BrokenPipe early
  return (silenced, not fatal); `[agent-id]` prefix for multi-agent runs;
  chunk count only incremented on successful `write_all` + `flush`.
- **Double-print suppression** — `Arc<Mutex<HashSet<String>>> streamed_agents`
  on `Scheduler`; main.rs reads it after `run()` and skips `println!` for
  agents that already streamed.
- **Flight events** — `InferenceStreamStarted` + `InferenceStreamCompleted`
  (with `text_chunks_emitted`; `event_taxonomy_completeness` test updated).
- **`docs/CONVENTIONS.md`** — two new event table rows.
- 889 workspace tests (up from 862); 27 new tests.

## [p7.1] - 2026-06-20 (v0.35.0)

You can now connect agentd to hosted MCP services (Linear, GitHub, and any
Streamable-HTTP-capable server) without running a local subprocess — add a
`url` + `headers_env` block to `[[tools.mcp_servers]]` and the agent gains
those tools automatically. Implements the MCP spec 2025-03-26 HTTP transport.

- **`McpBackend` trait** (`agentd/src/tools/mcp.rs`) — unified interface over
  stdio (`McpClient`) and HTTP (`McpHttpClient`); `McpTool.client` changed from
  `Arc<McpClient>` to `Arc<dyn McpBackend>`; `transport_kind()` returns `"stdio"`
  or `"http"`.
- **`McpHttpClient`** — single-POST JSON-RPC client with SSE state machine;
  `Mcp-Session-Id` header capture; `read_bounded_http_body()` with streaming
  byte-count guard (4 MB limit); `parse_sse_stream()` per SSE spec; 30 s
  `MCP_TIMEOUT`; multi-page `tools/list` with 100-page guard (warns on truncation).
- **Config** (`config.rs`) — `McpServerConfig` extended with `url: Option<String>`
  and `headers_env: HashMap<String,String>`; `command` now `#[serde(default)]`;
  `is_http()` + `validate()` (mutual-exclusion guard; `https://` required;
  embedded credentials in URL rejected).
- **Header-secret safety** — values read from env at startup; never logged;
  only header names appear in error messages.
- **`main.rs`** — transport dispatch (`is_http()` branch) before
  `mcp_require_capabilities` / gVisor / sandbox compile; `McpHttpConnected`
  event on connect; HTTP servers skip sandbox (externally isolated);
  `ServerEnforcement.transport` field (`"stdio"` | `"http"`).
- **FUSE + agentctl surfaces** — `ServerEnforcement.transport` exposed in
  `/agents/system/sandbox`, `/agents/<id>/sandbox` JSON; `agentctl watch`
  shows transport in sandbox rows; plain-mode output includes `transport=`.
- **Flight events** — `mcp_http_connected` (server_name, url, session_id_present)
  + `mcp_http_error` (server_name, http_status, method); CONVENTIONS.md updated.
- **docs/MCP_SERVERS.md** — new file with known HTTP MCP server URLs (Linear, GitHub).
- **`agent.toml`** — commented HTTP server example block added.
- **reqwest** `stream` feature added; **tokio** `net` feature added.
- **Security hardening**: `notify()` body drain bounded (OOM fix); 10 s
  `connect_timeout` (fast-fail on unreachable hosts); redirect following disabled
  (auth header leak prevention).
- **Tests**: 21 new tests (SSE parser, McpServerConfig validation, transport
  rendering, HTTP sandbox rows, taxonomy completeness); 862 total workspace tests
  (up from 841).

## [p6.8] - 2026-06-19 (v0.34.0)

Sandbox-enforcement surface + flight-log inspector for `agentctl watch`.

- **`SandboxSummary` + `ServerEnforcement`** (`surfaces/src/snapshot.rs`) — replaces
  `sandbox_applied: bool` on `SchedulerSnapshot` with a full `SandboxSummary { any_sandboxed,
  servers: Vec<ServerEnforcement>, degradations: Vec<String> }`. `ServerEnforcement` carries
  per-MCP-server fields: `name`, `isolation`, `landlock`, `seccomp`, `spawn_enforcement`,
  `namespace_net`, `namespace_mount`, `landlock_net`.
- **`/agents/<id>/sandbox` FUSE virtual file** (`surfaces/src/agents_fs.rs`) — `OFF_SANDBOX=10`,
  11th per-agent inode slot, emits JSON with the per-agent server enforcement list. Updated
  `alloc_dir`, `prune_dead_agent`, readdir, lookup, getattr, `file_content_for_ino`.
- **`/agents/system/sandbox` expanded** — now emits full `SandboxSummary` JSON including
  `servers[]` and `degradations[]` arrays (was boolean `applied` only).
- **`accessible_server_names: Vec<String>`** on `AgentSnapshot` — names of MCP servers the
  agent has `Mcp`-capability access to; populated from `AgentTask` in `scheduler.rs`.
- **`main.rs` sandbox builder** — builds `ServerEnforcement` per MCP server
  (`#[cfg(target_os = "linux")]`), detects `landlock_net_unavailable` and
  `spawn_enforcement_unavailable_arch` degradations.
- **`agentctl` reader expansion** (`agentctl/src/watch/reader.rs`) — `SysSandbox` gains
  `servers` + `degradations` fields (`#[serde(alias = "applied")]` for backward compat);
  `AgentSandbox` struct; `read_agent_sandbox()` helper; `AgentInfo.sandbox` field.
- **Agent-detail sandbox row** (`views.rs`) — shows per-server flags
  (`landlock`, `seccomp`, `net_ns`, `mount_ns`) inline in the detail pane.
- **System-view degradation warnings** — yellow warning rows appear for each entry in
  `SysSandbox.degradations` (e.g. "landlock_net_unavailable").
- **`View::Inspector`** (`[i]` key in `agentctl watch`) — new `agentctl/src/watch/inspector.rs`
  module with `InspectorState`: loads last 512 KB of `flight.jsonl` (load-once model,
  `[r]` to reload); `[Tab]` cycles filter (All → Errors → Sandbox → CapDenied); `[/]`
  substring search; color-coded body (red=errors, cyan=sandbox, yellow=cap_denied); scroll
  with ↑/↓/j/k/PgUp/PgDn/Home/End.
- **`MAX_INSPECTOR_LINES=500`** cap on loaded flight-log lines.
- **Tests**: 14 new inspector tests, 11 new FUSE tests (incl. unrestricted-caps path, restricted-empty
  path, named-accessible-server intersection, sys sandbox with servers+degradations), 10 new reader
  tests (incl. render_plain sandbox blocks), 1 new checkpoint test (parent_map serde default) —
  total workspace test count: 841.

## [p6.7] - 2026-06-18 (v0.33.0)

Starter catalogue — 7 committed templates covering every AgentOS primitive layer.

- **6 new templates** (`templates/`) — `librarian` (Landlock MCP sandbox), `journaler`
  (Phase-5 durable memory), `coordinator` (spawn + bus), `code-aware` (gVisor isolation),
  `watcher` (trigger-gated, ships as honest one-shot scanner), `memory-custodian`
  (shared KB curation). Each has `sample_tasks`, `showcases`, and `gated_requires` where
  applicable.
- **`TemplateMeta.gated_requires: Option<String>`** — new field in `agentd/src/template.rs`.
  When set, `agentctl spawn` prints a pre-flight warning before exec so operators know
  about Phase-5 memory, gVisor, or event-trigger dependencies.
- **`TemplateEntry.sample_tasks: Vec<String>`** — catalogue listing now carries sample
  tasks; `agentctl list-templates` shows DESCRIPTION (not SHOWCASES) as the primary column
  with showcases on a sub-line for scannability.
- **TUI Spawn view pre-fill** — `SpawnViewState` pre-fills `task_input` with
  `sample_tasks[0]` when navigating to a template with an empty task field.
- **22 new tests** (14 catalogue in `agentd/src/template.rs`, 1 gated_requires parse in
  `agentctl/src/spawn.rs`, 3 prefill/reset in `agentctl/src/watch/app.rs`, 2 truncation
  safety in `agentctl/src/list.rs`, 2 coverage for `load_spawn_templates` + boundary paths).
- Total test count: 808 workspace (agentd lib 396 + agentctl 259 + surfaces 32 + sandbox + integration).

## [p6.6] - 2026-06-18 (v0.32.0)

Spawn view for `agentctl watch`. The new `[n]ew` view is the first interactive/write
form in the TUI — it lets the operator pick a template, fill in a task, toggle
capability grants (pre-checked from the template's `suggested_caps`, deny-by-default),
preview the generated `agent.toml`, then spawn agentd (mode a: generate-and-exec).

- **`agentctl/src/watch/spawn.rs`** — new module: `SpawnTemplate` (name, source,
  description, showcases, suggested_caps); `load_spawn_templates()` (lazy via
  `TemplateResolver`); `display_cap()` struct-form formatter ("FsRead {/workspace}").
- **`View::Spawn`** in `agentctl watch`; `[n]` from Dashboard; `[Tab]` cycles through
  `TemplatePicker → TaskField → CapToggles → ActionGenerate → ActionSpawn → wrap`.
- **`SpawnViewState`** — lazy-loaded templates (once on first entry), cap toggles
  `Vec<(Capability, String, bool)>` (all pre-checked), `task_input`, `preview`,
  `result_msg`, `pending_exec: Option<PendingSpawn>`.
- **Key bindings** — `[g]` generate preview; `[r]` spawn (sets `pending_exec`);
  `Esc` in TaskField defocuses (does not exit view); `Esc`/`q` elsewhere back to Dashboard.
- **`pending_exec` pattern** — `handle_spawn_key` sets `pending_exec`; `run_tui`
  detects it after the event match, breaks the loop, drops `CleanupGuard` (restoring
  the terminal), then calls `execute_pending_spawn` which resolves the template,
  writes a `NamedTempFile`, and `exec`s agentd (Unix `execvp`, replacing the TUI process).
- **`render_spawn()`** in `views.rs` — 5-row layout (header / picker / task / mid-split /
  footer); focused section border highlighted in Yellow; action buttons in mid-split right
  pane; preview shows first 20 lines of generated TOML.
- **Plain mode** — `render_plain` appends a `spawn:` section listing the template catalogue
  (loaded only if the Spawn view was entered; otherwise shows "none loaded").
- **`agentctl/src/spawn.rs`** — `exec_agentd`, `resolve_agentd`, `format_cap` promoted
  to `pub(crate)` for reuse from the watch module.
- **Adversarial review fixes** (landed in the same increment):
  - Cap toggles now revoke baseline caps — `PendingSpawn` gains `disabled_caps: Vec<Capability>`;
    `do_generate` and `execute_pending_spawn` strip them via `caps.retain(|c| !disabled.contains(c))`
    so unchecking a suggested cap that is also in the template `[capabilities]` section actually
    removes it from the generated TOML.
  - `flush()` added before `keep()` in `execute_pending_spawn` — matches CLI `spawn.rs:173`;
    without it the OS could discard buffered TOML bytes when `execvp` replaces the process.
- **793 tests** (+45 new: 8 watch/spawn.rs, 9 app.rs, 8 mod.rs, 0 views.rs — render
  functions tested via render_plain and existing TUI infrastructure; +21 from adversarial
  review coverage: disabled_caps, flush guard, r/g keystroke passthrough, API key guard).

## [p6.5] - 2026-06-18 (v0.31.0)

Memory view for `agentctl watch`. The new `[m]emory` view lets operators browse
per-agent short-term and long-term memory stores, plus shared KB segments, all
with provenance metadata. Data flows through the existing FUSE virtual filesystem
— no direct redb dependency in agentctl. Degrades gracefully when Phase 5 is absent.

- **`agentctl/src/watch/memory.rs`** — new module: `MemoryEntry`, `AgentMemory`,
  `KbSegment` data types; `read_agent_memory()`/`read_kb_segments()` FUSE readers;
  `filter_entries()`/`filter_short_term()` client-side substring filters;
  `MAX_DISPLAY_ENTRIES = 20` / `MAX_SEARCH_ENTRIES = 100` constants.
- **`View::Memory`** in `agentctl watch`; `[m]` from Dashboard; true-tab pane model
  (`[Tab]` cycles Short-term → Long-term → KB); per-pane scroll offsets preserved
  across tab switches; `[/]` search mode filters all three panes; `Esc`/`q` back.
- **`MemoryPaneState`** — `search_query`, `search_active`, `short_term_scroll`,
  `long_term_scroll`, `kb_scroll`, `pane`, `absence`; `active_scroll_mut()` helper.
- **KB pane** always accessible regardless of selected agent — KB data is persistent
  and independent of live agent state.
- **Absence handling** — `MemoryAbsence::Subsystem` when `/agents/kb/` missing;
  `MemoryAbsence::Empty` when present but no segments written; documented messages
  with doc pointer in both cases.
- **Provenance formatting** — nanosecond u64 `ts` (long-term) → RFC3339 UTC via
  chrono; RFC3339 string `ts` (KB) displayed as-is with sub-second stripped;
  `[log]`/`[scratch]`/`[canon]` class badges on KB segments.
- **Plain mode** — `render_plain` dumps all agents' short-term + long-term (first 5
  entries each) plus all KB segments; skips cleanly when Phase 5 absent.
- **727 tests** (+55 new: 22 memory.rs, 14 app.rs, 13 views.rs, 3 mod.rs, 3 other).

## [p6.4] - 2026-06-18 (v0.30.0)

Topology view for `agentctl watch`. The new `[t]opology` view renders the live spawn tree
and directed message graph derived from the scheduler snapshot and an optional
`flight.jsonl` tail (up to 512 KB). Key additions:

- **`parent_id: Option<String>`** on `AgentSnapshot`; populated from an insert-only
  `parent_map: HashMap<String,String>` in `SchedulerState`, persisted in checkpoints
  with `#[serde(default)]` for backwards compatibility.
- **`OFF_PARENT = 9`** — new FUSE virtual file `/agents/<id>/parent`; `reader.rs` reads
  it into `AgentInfo.parent_id`.
- **`agentctl/src/watch/topology.rs`** — `TopologyGraph`, `build_graph()` (512 KB tail
  cap, directed edges from `message_sent` events, cycle guard), `render_tree()`,
  `status_badge()`, `parse_message_edges()`.
- **`View::Topology`** in `agentctl watch`; key `t` from Dashboard; `Esc`/`q` returns
  to Dashboard; ↑/↓/j/k scrolls; fixed legend footer outside scrollable region; minimum
  terminal width 60 cols.
- **`--log-path`** CLI flag on `agentctl watch` for message edge data.
- **Plain mode topology section**: `topology: <id> parent=<id>|none status=<status>`.
- **`coordinator-demo.agents.toml`** — acceptance fixture: coordinator + 2 scouts.
- 455 tests pass (macOS; Linux adds FUSE surface + sandbox tests); `make clippy-linux`
  required for FUSE surface changes.

### Fixed (adversarial review)
- `parse_message_edges` now reads `data.to` (not top-level `to`) to match the
  `FlightRecorder` event schema — message edges were always empty against real
  `flight.jsonl` because `"to"` is nested under `"data"`.
- Test fixtures updated to use the correct flight-log event structure.
- `topology_scroll` reset to 0 when switching into the Topology view so
  scroll state does not carry over stale offsets from a prior visit.
- Clippy: `map_or(true, …)` → `is_none_or`; `map_or(false, …)` → `is_some_and`;
  `#[allow(clippy::too_many_arguments)]` on the private recursive tree renderer.

## [p6.3] - 2026-06-17 (v0.29.0)

Read-only TUI dashboard. `agentctl watch` opens a live ratatui view of all running agents,
their status, token budgets, and tools. Three views: Dashboard (agent table), Agent Detail
(expanded per-agent), and System (provider, tokens, queue, sandbox status).

### Added
- `agentctl watch [--agents-dir /agents] [--interval N] [--plain] [--no-plain]` — live TUI dashboard.
  - **Dashboard view**: agent table with ID / Status (colour-coded) / Context tokens / Budget / Tool count.
  - **Agent detail view**: expanded view showing status, context, budget, and tool list for the selected agent.
  - **System view**: provider model+backend, global tokens spent, deferred queue depth, sandbox status.
  - **Plain mode**: `--plain` or auto-detected non-TTY stdout emits plain-text snapshots; `--no-plain` forces TUI.
  - **Startup validation**: fails fast with a clear error if `{agents-dir}/system/` is not mounted.
  - **CleanupGuard**: restores terminal on both normal exit and panic via `Drop` + `std::panic::set_hook`.
  - Key bindings: ↑/↓/j/k select, Enter → detail, `s` → system, `q`/Ctrl-C → quit, Esc → back.
- `surfaces/`: FUSE virtual filesystem amendments (Linux-gated):
  - `DIR_STEP` bumped 10→20 to accommodate 9 per-agent virtual files without inode collision.
  - `OFF_TOOLS = 8` — new `tools` virtual file per agent directory listing capability-filtered tool names.
  - `/agents/system/` directory with four virtual files: `budget` (`{spent, total}`), `queue` (`{depth}`),
    `sandbox` (`{applied}`), `provider` (`{model, backend}`).
  - `SchedulerSnapshot` gains `queue_depth`, `provider_model`, `sandbox_applied` fields.
  - `AgentSnapshot` gains `tools: Vec<String>` populated from `AgentTask::spec_names()`.
- `agentd`: `AgentTask::spec_names()` returns capability-filtered tool names from pre-built `specs`.
- `agentd/src/scheduler.rs`: `update_snapshot()` sets `tools` + `queue_depth`; `main.rs` sets
  `provider_model` and tracks `any_sandbox_applied` across MCP server loop.
- CI: agentctl binary size guard bumped 4 MB → 6 MB (ratatui+crossterm add ~1–1.5 MB).

### Changed
- `agentctl` version bumped to `0.29.0`.
- `agentd` version bumped to `0.29.0`.

### Fixed (pre-landing review)
- `io::stdout().is_terminal()` called twice in `run()` → cached as `is_tty` local.
- Cross-crate sentinel literals (`"unlimited"`, `"(none)"`) replaced with named constants in
  both `surfaces/src/agents_fs.rs` and `agentctl/src/watch/reader.rs` with sync comments.
- `run_plain`: flush stdout after each snapshot block so piped readers see complete output;
  SIGINT terminates cleanly via OS default handler (no raw mode is active).
- `AgentTask::spec_names()` now returns `&[String]` from a cached `tool_names` field
  built at construction, eliminating per-tick per-element String allocation in snapshot path.
- `sanitize()` helper added to `views.rs`: strips control chars (< 0x20 except tab) from
  error strings before rendering, preventing ANSI injection via OS error messages.
- `debug_assert!` in `AgentsFs::alloc_dir()` promoted to `assert!` so inode pool exhaustion
  is caught in release builds before silent corruption occurs.

## [p6.2] - 2026-06-17 (v0.28.0)

Operator CLI. Agents can now be spawned from templates without editing TOML files.

### Added
- `agentctl/` workspace crate — new operator CLI binary.
- `agentctl list-templates` — tab-aligned table of templates from repo catalogue and
  `~/.agentos/templates/`, showing name, source (Repo/User), and showcases.
- `agentctl spawn <name> --task "..." [--cap-add ...] [--dry-run]` — resolves a template,
  lowers it to an `agent.toml`, writes it atomically via tempfile rename, then `exec`s agentd.
- `TemplateCard.suggested_caps: Vec<Capability>` — guards `--cap-add` without `--force`;
  uses real `Capability` type (single vocabulary, not alias strings).
- `parse_cap_alias()` — maps flat CLI syntax (`fs-read:<path>`, `net:<ports>`, etc.) to
  `Capability` values; rejects relative paths, bare `net`, and `mcp:...`.
- `cap_add_allowed_by_suggestion()` — FsRead/FsWrite ancestor-of semantics; KbRead/KbWrite
  prefix match; Net port-subset check; Spawn exact match.
- `--dry-run` — prints parseable TOML to stdout with provenance header; does not require
  agentd to be on PATH.
- `--force` — bypasses suggested_caps guard.
- `ANTHROPIC_API_KEY` preflight check before exec.
- Sibling + PATH agentd resolution; `--agentd-path` override.
- Distro: `/usr/bin/agentctl` + `/etc/agentd/templates/` in QEMU image overlay.

### Changed
- `agentd` gains a `[lib]` target so `agentctl` can import `agentd::template`,
  `agentd::capability`, and `agentd::config` types.
- `agentd` version bumped to `0.28.0`.

## [p6.1] - 2026-06-17 (v0.27.0)

Phase 6 begins. Agents are now discoverable before they run: `*.template.toml` files in `templates/` describe an agent's capabilities, tools, and sample tasks. `TemplateResolver` loads from the repo catalogue and `~/.agentos/templates/` (user overrides), then lowers to a plain `Config` for `agentd`.

### Added
- `agentd::template` — new public module with `TemplateConfig`, `TemplateMeta`,
  `TemplateCapabilities`, `TemplateCard`, `TemplateResolver`, `TemplateSource`,
  `TemplateEntry`. `TemplateConfig::to_agent_config()` lowers a template to a plain
  `Config` with template-only keys stripped.
- `templates/scout.template.toml` — scout agent as first catalogue entry.
- `TemplateResolver::from_env()` convenience constructor (`~/.agentos/templates/`).
- Path-traversal rejection in `TemplateResolver::resolve()`.
- Name identity check in `resolve()` and `list()` — mismatched `[template].name` vs
  filename stem is rejected (`resolve`) or skipped with `tracing::warn!` (`list`).
- Absolute path validation on `[capabilities].fs_read`/`fs_write` in `to_agent_config()`.
- `list()` deduplicates by name (user dir wins); emits `tracing::warn!` on parse errors
  instead of silently discarding the file.
- 22 unit tests.

### Changed
- `to_agent_config()` now preserves `[agent].capabilities` (e.g. `Mcp` grants with no
  sugar form): sugar caps are built first, then existing agent caps are appended, so
  previously-discarded `Mcp` grants are no longer lost.
- `Config`, `ToolsConfig`, `SchedulerConfig`, `MemoryConfig`, `McpServerConfig`,
  `IsolationMode`, `SeedEntry`, `SegmentConfig`, `MutabilityClass` now derive
  `Serialize` (and `Clone` where missing), unblocking `agentctl` TOML write in p6.2.
- `CONVENTIONS.md` — new "Templates" section.

### Security
- Missing `[capabilities]` in a template lowers to `agent.capabilities = Some([])`
  (deny-all), never `None` (unrestricted).
- Relative paths in `[capabilities].fs_read`/`fs_write` are rejected at lowering time.

---

## [p5.9] - 2026-06-16 (v0.26.0)

Phase 5 hardening (audit remediation) — closes the P1 findings from `docs/AUDIT-phase-5.md`
(resolution table in §8). Each fix ships with a regression test that fails pre-fix. Gate before Phase 6.

### Fixed
- **F-01:** working-memory paging is keyed on a retained-context estimate
  (`memory::context::estimate_context_tokens`) instead of cumulative lifetime spend, which only grew
  and re-paged every turn once budget crossed 90%. Lifetime spend still drives the budget guard +
  advisory. Test: `paging_stops_when_context_below_target`.
- **F-02:** `RedbStore::open` quarantines only on confirmed corruption
  (`StorageError::Corrupted` / `Io(InvalidData)`); lock, permission, transient I/O, and
  upgrade-required errors surface without renaming a valid store. Timestamped `.corrupt` path.
  Test: `transient_open_error_is_not_quarantined`.
- **F-03:** eviction floor is wired to the live write path (`set_segment_limits` → `put`/`append`
  self-trim); canon segments are never evicted (guarded in `evict()`). Tests:
  `eviction_runs_through_live_path`, `canon_is_not_evicted`.
- **F-04:** `debug_assert_counters` reconciles the NAMESPACES counter vs actual key count after every
  mutation. Test: `namespace_counter_matches_key_count`.
- **F-07a:** `page_turns` alternating-role invariant is a runtime `Err` (was a `debug_assert!`).
- **F-09:** `spawn_agent.child_id` is validated (`validate_child_id`) — rejects traversal / namespace
  separators. Tests: `validate_child_id_*`, `spawn_rejects_invalid_child_id`.
- **F-16:** `spawn_agent`/`send_message` batched with other tools no longer terminates the agent; it
  returns `is_error` results for every call and re-infers so the model retries the sole tool alone.

### Added
- Operator segment seeding: `[[memory.segments]]` `seed = [{ key, value }]` (F-14); demo `agents.toml`
  now parses + runs (also adds `spawn_agent`/`send_message`/`list_agents` to `native`, F-15).
- Root `.gitignore` (workspace `target/`, `*.redb`, `.gstack/`, `.DS_Store`).
- 2-boot continuity test: `two_boot_continuity_at_store_level`.

### Changed
- CI + `make clippy-linux` run `cargo clippy --all-targets` (F-13); fixed all surfaced test-only lints.

## [p5.8] - 2026-06-15 (v0.25.0)

Phase 5 hardening: security invariants, FUSE inode pruning, memory store index, docs completeness.

### Added
- **Startup invariant (OV-1):** `memory.store_path` must be absolute and must not fall inside any
  MCP server's `FsRead`/`FsWrite` sandbox prefix. Checked on every startup via `anyhow::ensure!`;
  `..` traversal is resolved by `normalize_path` before comparison. Test:
  `store_path_inside_sandbox_prefix_fails_startup`.
- **`NAMESPACES` redb table:** `TableDefinition<&str, u64>` maintained atomically on every
  `put`, `append`, `delete`, and `evict`. `list_namespaces()` is now O(k) (k = number of distinct
  namespaces) instead of O(n) full ENTRIES scan. One-time backfill on first open of pre-p5.8 stores.
- **`prune_dead_agent()` in `AgentsFs`** (ar-01): lazy pruning in `readdir(Root)` for terminated
  agents. Cleans all 6 inode maps: `dir_inodes`, `inode_to_id`, `dyn_ino_kind`, `lt_key_ino`,
  `kb_seg_ino`, `kb_key_ino`. Shared segments (no `agent/{id}/` prefix) are not pruned.
- **`dyn_file_content()` match dispatch clarified** (ar-02): removed tautological
  `debug_assert!(matches!(...))` inside `LtFile` and `KbFile` arms; the enclosing `match`
  already guarantees the variant — added explanatory comments instead.
- **`getattr` ENOENT guard for memory dirs** (ar-03): `OFF_MEMORY_DIR` (+5) and `OFF_LONG_TERM_DIR`
  (+7) return `ENOENT` when `self.memory.is_none()`. `OFF_SHORT_TERM` (+6) exempted — still served
  from `AgentSnapshot.short_term_previews`.
- **Memory demo `agents.toml`**: two-agent KB write→search→read demo exercising `canon`/`scratch`
  segments, `KbRead`/`KbWrite` capabilities, `spawn_agent`, and `global_token_budget`.
- **CONVENTIONS.md completeness**: `memory_distilled` row added to the Phase-5 event table
  (was missing from p5.3). `event_taxonomy_completeness` test asserts all 9 `memory_*`/`kb_*`
  EventKind strings appear in CONVENTIONS.md.
- **THREAT_MODEL.md §7 expanded** to §7.1–7.6 (memory substrate threats): §7.3 KB exfiltration
  channel, §7.4 prompt-injection persistence, §7.5 `memory.redb` at rest, §7.6 availability.
  Old §7 Summary renumbered to §8.
- **9 new tests** (476 total up from 467): 4 NAMESPACES tests in `store.rs`, 3 surfaces tests
  (`inode_map_pruned_on_snapshot_update`, `getattr_memory_dir_enoent_when_no_store`,
  `getattr_short_term_ok_when_no_store`), 2 main.rs tests
  (`store_path_inside_sandbox_prefix_fails_startup`, `event_taxonomy_completeness`).

### Fixed
- **NAMESPACES backfill non-fatal**: a transient I/O failure (ENOSPC, NFS timeout) during the
  one-time post-upgrade backfill no longer quarantines a valid pre-p5.8 store. On write failure
  the store opens successfully; `list_namespaces()` falls back to O(n) scan until next restart.
- **`ar-03` guard extended to `is_dir_ino()` and `parent_kind()`**: the `getattr()` ENOENT guard
  for `memory/` and `long_term/` when no memory store is configured is now also applied in
  `is_dir_ino()` (prevents stale-inode `opendir` success) and `parent_kind()` (prevents `readdir`
  returning a partial listing instead of propagating ENOENT to the caller).

### Changed
- `agents.toml` rewritten as a memory demo (writer + spawned reader, `project:meta` canon seed,
  `project:research` scratch segment, `claude-haiku-4-5-20251001`, 100k global budget).
- TODOS.md: p5.7-ar-01/02/03/04 closed; p5.7-ar-05 (`MAX_DIR_KEYS` silent truncation) deferred to p6+.

---

## [p5.7] - 2026-06-14 (v0.24.0)

FUSE memory surface: `/agents/<id>/memory/` and `/agents/kb/` read-only directories
expose agent short-term/long-term memory and shared KB segments to control-plane tools.

### Added
- **`surfaces::MemoryAccess` trait**: minimal read-only interface (`list_namespaces`,
  `list_keys`, `get_entry`) defined in the `surfaces` leaf crate so `AgentsFs` can
  browse memory without a circular dependency.
- **`MemoryStore::list_namespaces()`**: default-impl trait method on `MemoryStore`
  (returns empty); overridden by `RedbStore` to scan ENTRIES for distinct namespace
  prefixes.
- **`MemoryAccessBridge`** in `main.rs` (Linux-only): wraps `Arc<dyn MemoryStore>` and
  implements `MemoryAccess` via `iter()` / `get()` / `list_namespaces()`.
- **`AgentSnapshot::short_term_previews: Vec<String>`**: bounded projection (≤20 items)
  of the agent's Tier-2 short-term buffer, formatted `"t{turn} {role}: {preview}"`.
  Populated by `update_snapshot` in the scheduler.
- **FUSE inode scheme extended** (new offsets within per-agent 10-slot window):
  `+5 memory/` (dir), `+6 memory/short_term` (file), `+7 memory/long_term/` (dir).
  Fixed inode `9` for top-level `kb/` dir. Dynamic pool at `≥1_000_000` for
  `memory/long_term/<key>`, `kb/<seg>/`, and `kb/<seg>/<key>`.
- **`/agents/<id>/memory/short_term`**: renders `short_term_previews` from snapshot.
- **`/agents/<id>/memory/long_term/<key>`**: reads live from `MemoryAccess`; up to
  100 keys listed per directory to bound snapshot size.
- **`/agents/kb/<seg>/<key>`**: operator-visible KB browse; only namespaces without
  `agent/` prefix appear; up to 100 keys per segment.
- **`mount()` signature updated** to accept `Option<Arc<dyn MemoryAccess>>`; memory
  subtrees only appear when the store is configured.
- **467 tests** (up from 406): 9 new surfaces tests in initial commit (`memory_subtree_lists_short_and_long_term`,
  `short_term_file_reflects_snapshot_previews`, `kb_segment_browse_returns_entry_content`,
  `large_memory_entry_read_does_not_panic`, `memory_view_stale_snapshot_does_not_tear_ongoing_read`,
  plus updated `all_eight_inodes_registered_after_alloc` and `file_name_for_offset_covers_all_files`);
  13 regression tests added during review/QA hardening passes.

### Changed
- **`MemoryAccessBridge`** errors now emit `tracing::warn!` instead of silently returning
  empty/`None` — `list_namespaces`, `list_keys`, and `get_entry` all log the error and
  the namespace/key on failure, making FUSE surface issues visible in the diagnostic log.
- **`MemoryStore::list_keys(namespace)`** added as a new trait method (default-impl on
  `MemoryStore`, overridden by `RedbStore`): scans ENTRIES keys for a given namespace
  prefix without deserializing values, cutting per-readdir allocation in half for
  `long_term/<key>` and `kb/<seg>/<key>` listings.

### Fixed
- **`getattr(INO_KB)` returns `ENOENT` when no memory store is configured**: previously
  the `kb/` directory appeared in `getattr` responses even when `self.memory.is_none()`,
  making it visible but empty and inconsistent with `readdir`. Now `ENOENT` is returned
  at the `getattr` level to match the `readdir` behavior.
- **`alloc_dir()` inode pool exhaustion guard**: added `debug_assert!` to detect if the
  fixed-inode counter reaches `DYNAMIC_INO_START` (1 000 000), which would corrupt inode
  lookups silently. Fires in debug/test builds; the fixed pool is large enough for
  any realistic agent count.
- **Slash/NUL key filter in `LongTermDir` and `KbSegDir` readdir**: keys containing
  `/` or `\0` are now silently skipped before being emitted as FUSE directory entries.
  Such keys would have caused FUSE to corrupt the virtual path tree or cause kernel
  EINVAL errors on directory listing.
- **Slash/NUL segment filter in `Kb` readdir**: `list_namespaces()` results are now
  additionally filtered for `/` and `\0` characters beyond the existing `agent/` prefix
  filter, preventing malformed segment names from corrupting the `kb/` directory tree.
- **`KbSegDir` readdir no longer panics on map divergence**: the `self.kb_seg_ino[&segment]`
  index access (which could panic if `kb_seg_ino` and `dyn_ino_map` diverge) is replaced
  with `.get()` + `EIO` reply, consistent with the "loop never panics on bad input" invariant.
- **`wrapping_sub` consistency in `file_content_for_ino`**: plain `ino - dir_ino`
  subtraction replaced with `ino.wrapping_sub(*dir_ino)` to match the wrapping arithmetic
  used in every other offset calculation in `agents_fs.rs`.

## [p5.6] - 2026-06-14 (v0.23.0)

Eviction & summarization: per-segment capacity/age eviction floor and optional
end-of-run short-term distillation.

### Added
- **`MemoryStore::evict()`** trait method and `RedbStore` implementation: drops oldest
  entries beyond `max_entries` and/or older than `max_age_secs`, removing ENTRIES +
  INDEX postings + AGE + META doc_count in a single atomic transaction. Returns
  `Vec<EvictedEntry>` with key + reason (`"capacity"` or `"age"`).
- **`AGE` redb table**: composite key → Unix timestamp (seconds). Written atomically
  with every `put()` and `append()` write; removed on `delete()` and eviction.
- **`EvictedEntry`** struct in `memory/mod.rs`: `key: String`, `reason: String`.
- **`EventKind::MemoryEvicted`** in `events.rs`: serializes as `"memory_evicted"`;
  data shape: `{ segment, key, reason }`.
- **Config fields** on `[memory]`: `max_entries_per_segment: Option<usize>`,
  `max_entry_age_days: Option<u64>`, `distill_on_complete: bool` (default false).
- **`Scheduler::with_distillation(store)`** builder: attaches a memory store and
  enables end-of-run short-term distillation. For each completed agent whose
  `short_term` buffer is non-empty, makes one budget-bounded inference call to
  summarize the paged turns and writes the result to `agent/{id}/distilled/…` under
  Tier 3. Respects the global token budget guard; off by default (demos unchanged).
- **`docs/CONVENTIONS.md`**: `memory_evicted` row added to event taxonomy table.
- 406 tests (up from 397): 5 new eviction store tests (`evicts_oldest_beyond_capacity`,
  `evicts_entries_past_max_age`, `eviction_removes_index_postings`,
  `evict_empty_namespace_returns_empty`, `evict_below_capacity_does_nothing`),
  2 scheduler tests (`distill_on_complete_promotes_to_tier3`,
  `distill_disabled_no_extra_inference`), 2 config tests.

## [p5.5] - 2026-06-14 (v0.22.0)

Retrieval as tool: `kb_search` with BM25-lite inverted index over the shared KB.

### Added
- **`kb_search` tool** (`tools/native.rs`): BM25-lite ranked retrieval over a KB
  segment. Requires `KbRead` capability. Inputs: `segment`, `query`, optional `author`
  filter, optional `limit` (default 10, max 50). Output: flat JSON with `hits` (content +
  provenance expanded), `terms_matched`. All-stopword queries return a structured empty
  with `note` field.
- **Inverted index** (`memory/index.rs`): `tokenize()` (lowercase, split non-alphanumeric,
  skip stopwords + >64-byte tokens), `term_frequencies()`. 21-word stoplist.
- **`INDEX` redb table**: key = `"{namespace}\x00{word}"`, value = JSON posting list.
  ENTRIES + INDEX + META updated atomically in a single write transaction per put/append/delete.
- **`doc_count:{namespace}` META key**: tracks corpus size for BM25 IDF; incremented on
  new-key writes, decremented on delete.
- **`MemoryStore::search()`** trait method: `(hits, terms_matched)` return; `SearchHit`
  struct; `RedbStore` implements full BM25-lite; `SimpleStore` test mock uses brute-force
  linear scan.
- **`KbSearch` flight event**: `agent_id`, `segment`, `query_preview` (64-char truncated),
  `hits`, `terms_matched`.
- 397 tests (up from 376): 7 new `store::tests` (ranking, namespace isolation, author
  filter, write/delete round-trip, append indexing, posting-list pruning, stopword guard),
  5 `store::tests` coverage additions (put-overwrite deindex, search-None error, author
  no-provenance include), 2 `tools::tests` flight-event + query-preview tests,
  1 integration test (multi-write ordered hits with provenance), 2 `native::tests`
  (KbSearch missing-segment and empty-query guards).

### Fixed
- `append()` used `is_empty()` on a `String` to detect new keys; replaced with
  `is_none()` on the `Option` so an existing entry whose value is `""` does not
  re-increment `doc_count`, preventing permanent BM25 IDF drift.
- `search()` now skips zero-score candidates (consistent with `SimpleStore` mock) so
  documents whose only posting-list entry is a race artifact do not appear in results.
- Query terms are deduplicated and capped at 64 unique terms before scoring to bound
  worst-case BM25 work regardless of repeated terms in the LLM-supplied query.

### Security
- `kb_search` gated behind `KbRead` capability on the queried segment — same enforcement
  as `kb_get`.
- Cross-segment search returns an error (not silently returning cross-namespace data).
- Stale posting entries (post-delete race) silently skipped during scoring.
- Query term deduplication + 64-term cap prevents adversarial O(n²) scoring via repeated terms.

## [p5.4] - 2026-06-14 (v0.21.0)

Shared KB MVP: multi-agent segmented knowledge base with three mutability classes
(`canon` / `log` / `scratch`), runtime-stamped provenance, and capability-gated
`kb_put` / `kb_get` tools.

### Added
- **`KbPut` / `KbGet` tools** (`tools/native.rs`): Tier-4 KB tools gated behind
  `KbWrite`/`KbRead` capabilities. `kb_put` enforces mutability class: canon → deny,
  log → auto-generated monotonic hex key, scratch → caller key + incrementing version.
  Provenance (`agent_id`, `turn`, `task_fp`, `ts`, `citation`) stamped from `ToolContext`.
- **`MemoryStore` trait extensions**: `segment_class`, `set_segment_class`,
  `next_log_seq` — implemented in `RedbStore` via the META table.
- **`[[memory.segments]]` TOML config**: `SegmentConfig { name, class }` in
  `MemoryConfig`; `main.rs` seeds classes into the store at startup.
- **`memory_write` / `memory_read` events extended** with `tier: 4` and `class`
  fields for KB operations.
- **THREAT_MODEL.md §7.1/§7.2**: KB poisoning and cross-agent exfiltration analysis.
- 376 tests (up from 336). 6 new feature tests + 30 new tests from pre-landing review:
  RedbStore segment_class/next_log_seq/next_scratch_version persistence, kb tool
  exclusion from "all", store=None silent-skip, kv_set canon/log denial, event field
  assertions.

### Fixed (pre-landing review)
- **`kv_set` bypassed canon/log enforcement**: `KvSet::invoke` now checks `segment_class`
  before writing, matching the invariant enforced by `KbPut`.
- **Scratch version TOCTOU**: `next_scratch_version()` added to `MemoryStore` trait and
  `RedbStore`; `KbPut` scratch branch atomically bumps the counter before constructing
  the entry, preventing two concurrent writers from producing identical version numbers.
- **Enum duplication**: `config::SegmentClass` replaced by re-export of
  `memory::MutabilityClass` (with `serde::Deserialize`); manual 3-arm translation in
  `main.rs` eliminated.
- **`seg_class:` / `log_seq:` prefixes**: extracted to named constants in `store.rs`.

## [p5.3.5] - 2026-06-14 (infra-only, no version bump)

Detachable memory volume: `memory.redb` (Tiers 3/4) now lives on a persistent,
re-attachable host volume rather than the ephemeral output mount. Kill + respawn the
AgentOS container and re-attach the same volume for knowledge continuity.

### Changed
- **`distro/overlay/init`**: added `memory0` 9p virtfs mount to `/run/memory`.
- **`distro/Makefile`**: added `-virtfs local,path=$(HOME)/.agentos-memory,...` to
  `QEMU_FLAGS`; `prereqs` creates `~/.agentos-memory/` on first run; `test` target
  uses a per-run temp directory for the memory volume.
- **`distro/overlay/etc/agentd/agent.toml`**: `store_path = "/run/memory/memory.redb"`.
- **`agentd/src/config.rs`**: doc comment updated with container deployment guidance.
- **CI guard confirmed at 6 MB** (`MAX_BYTES=6291456`); stale "≤ 4 MB" references in
  ROADMAP.md and RUNBOOK.md corrected.
- No crate logic changed; no schema migration; default `store_path` unchanged.

## [p5.3] - 2026-06-14 (v0.20.0)

Per-Agent Long-Term Memory + Checkpoint Coexistence: agents can now explicitly
distil knowledge to a durable Tier-3 store (`mem_remember`) and retrieve it
across restarts (`mem_recall`). Memory survives clean exit; checkpoints do not.

### Added
- **`ToolContext`** struct (`tools/mod.rs`): `{ agent_id, turn, task_fp }` —
  runtime-stamped, unforgeable provenance injected into every `Tool::invoke`.
  `task_fp` is an FNV-1a 64-bit hash of the agent's initial task text (16 hex
  chars), recomputed from the checkpoint on restore.
- **`MemRemember`** tool (`tools/native.rs`): `mem_remember { content, tags }` —
  stores a JSON entry `{ content, tags, provenance: { agent_id, turn, ts, task_fp } }`
  under `agent/{id}` namespace with a nanosecond-timestamp key. Max 8 KiB per entry.
  No capability required (implicit self-grant). Emits `memory_distilled` flight event.
- **`MemRecall`** tool (`tools/native.rs`): `mem_recall { query, limit }` — iterates
  `agent/{id}` namespace, filters by substring match (content + tags, case-insensitive),
  returns JSON array newest-first. Default limit 10, max 50.
- **`EventKind::MemoryDistilled`** (`events.rs`) — emitted by `ToolRegistry::invoke`
  post-call for `mem_remember`.
- **`register_native`** updated: `"mem_remember"` and `"mem_recall"` are explicit
  opt-in names (like `kv_get`/`kv_set`); silently skipped if `store = None`.

### Changed
- `Tool::invoke` signature: `async fn invoke(&self, input, ctx: &ToolContext)` —
  all existing `impl Tool` updated to accept `_ctx: &ToolContext`.
- `ToolRegistry::invoke` takes `ctx: &ToolContext` instead of `agent_id: &str`.
- `AgentTask` gains `task_fp: String` field (runtime-only; recomputed on restore).
- `agentd` version: 0.19.0 → 0.20.0.

### Fixed (ship review)
- `MemRemember`/`MemRecall`: namespace now validated via `validate_segment` (consistent
  with `kv_get`/`kv_set`; rejects agent IDs with spaces or null bytes).
- `MemRemember` size guard now checks the serialized entry (`content + tags + provenance`)
  instead of `content` alone; tags could previously cause the stored value to exceed 8 KiB.
- `MemRecall` now rejects empty `query` with an explicit error instead of silently returning
  all entries (empty string matched every record).
- `MemRemember` key generation propagates system clock errors instead of falling back to
  key `0x0000000000000000` (which could silently overwrite other entries).
- `MemoryDistilled` event docstring corrected (removed `key`/`segment` fields that were
  never emitted; actual payload is `{ agent, turn, items: 1 }`).

### Tests
- 336 tests (up from 322 in p5.2). New: 15 unit tests for `MemRemember`/`MemRecall`
  (remember→recall, tag match, cross-agent isolation, oversized content, no-cap,
  store-absent skip, not-in-all, registry post-call hook; coverage gap tests: missing
  field errors, limit clamping, default limit, newest-first ordering, MemoryDistilled
  event emission).

## [p5.2] - 2026-06-14 (v0.19.0)

Per-Agent Short-Term Memory + Paging: Tier-2 eviction buffer; agents under
budget pressure page old turns out of active context instead of hitting
`budget_exceeded`.

### Added
- **`memory/context.rs`**: `MemoryPressure` enum, `assess()` (budget % → pressure
  level), `page_count()` (pairs eligible for eviction), `page_turns()` (two-pass
  serialize-then-drain, preserving alternating-role invariant). Constants:
  `SOFT_THRESHOLD = 0.75`, `HARD_THRESHOLD = 0.90`.
- **`MemItem`** struct (`memory/context.rs`): `{ turn: u32, role: Role,
  content_preview: String, blocks_json: String }` — serializable paged turn pair.
  `role: Role` (typed, not `String`).
- **`short_term: Vec<MemItem>`** field on `AgentTask` and `AgentCheckpoint`
  (`#[serde(default)]` for v1 back-compat).
- **Paging in `step_need_infer`**: Soft pressure → `MemoryPressureAdvisory` event
  (advisory only, no text injection, edge-triggered on None→Soft transition only).
  Hard pressure → `page_turns()` evicts oldest pairs; on success emits `MemoryPaged`;
  on serde error emits `Error` and skips. Hard pressure with context too short to page
  emits one advisory on first entry instead of silently doing nothing.
- **`to_checkpoint` / `from_checkpoint`** explicitly updated to include `short_term`.
- **Flight events**: `EventKind::MemoryPressureAdvisory`, `EventKind::MemoryPaged`.
- **FORMAT_VERSION 1 → 2**: additive bump; v1 checkpoints load with `short_term = []`.
- **`short_term_depth()`** public accessor on `AgentTask`.
- **FORMAT_VERSION migration policy** documented in `docs/CONVENTIONS.md`.
- Both new events added to CONVENTIONS.md event table; `memory` module boundary updated.

### Changed
- `FORMAT_VERSION` in `checkpoint.rs`: 1 → 2.
- `agentd` version: 0.18.0 → 0.19.0.

### Fixed
- `MemoryPressureAdvisory` no longer spams the flight log — edge-triggered (fires
  once on transition, not every turn at soft/hard pressure).
- `content_preview` in `MemItem` was always empty for `ToolUse` blocks; now uses
  the tool name as preview (e.g. `"read_file"`).
- `debug_assert!` in `page_turns` validates alternating-role invariant before drain.

### Tests
- 322 tests (up from 304 in p5.1). New: 14 unit tests covering all acceptance
  criteria (AC1–AC14 from plan).

## [p5.1] - 2026-06-14 (v0.18.0)

Storage Primitive: durable key/value store backed by redb 4.1.0.

### Added
- **`MemoryStore` trait** (`memory/store.rs`): `get`, `put`, `append`, `delete`,
  `iter`, `meta_version`. Sync methods; `Send + Sync`.
- **`RedbStore`** (`memory/redb_store.rs`): redb 4.1.0 implementation.
  Namespace+key encoding: `"{ns}\x00{key}"`. Handles `DatabaseAlreadyOpen`
  (retry with fresh open after brief delay), corrupt db (renamed to `.corrupt`
  and recreated). `RedbStore::open()` on macOS / `RedbStore::try_open()` internal.
- **`[memory]` config block** (`config.rs`): `store_path` (default `"memory.redb"`)
  and `enabled` (default `true`).
- **`kv_get` / `kv_set` tools** (`tools/native.rs`): structured namespace + key
  fields; `spawn_blocking` for sync redb calls; `MAX_KV_VALUE_BYTES = 256 KiB`
  per-value limit enforced in `kv_set`. **Not** included in `native = ["all"]`;
  require explicit listing.
- **`KbRead` / `KbWrite` capabilities** (`capability.rs`): `segment` field with
  `:` / `/` delimiter-boundary validation; `satisfies()` extended.
- **Flight events**: `MemoryRead`, `MemoryWrite`, `MemoryError`, `MemoryStoreOpened`.
  `ToolRegistry::invoke` emits memory events after successful kv tool calls.
- **`docs/INTERFACE.md`** — agent-facing interface doc (tools, capabilities, events).
- **`docs/SPIKES/p5.1-storage-primitive.md`** — implementation notes.

### Fixed
- Adversarial review (p5.1): `kv_set` now enforces `MAX_KV_VALUE_BYTES = 256 KiB`
  to prevent unbounded redb entry growth.

## [p4.7] - 2026-06-13 (v0.17.0)

Pre-Phase-5 cleanup sprint. Addresses all P0/P1 findings from
`docs/AUDIT-phase-4-6.md`. No new features.

### Security
- **F-001 (P0): MCP subprocesses no longer inherit parent environment.**
  `McpClient::spawn` now calls `env_clear()` then re-adds a vetted allowlist
  (`PATH`, `HOME`, `USER`, `LANG`, `LC_ALL`, `TMPDIR`) plus any explicit
  `env` map declared in `[[tools.mcp_servers]]`. `ANTHROPIC_API_KEY` and all
  other secrets are no longer passed to MCP server subprocesses.
  `McpServerConfig` gains an `env: HashMap<String, String>` field (default empty).
  `docs/THREAT_MODEL.md §1.3` documents the env isolation contract.
- **F-002 (P1): `Net{ports}` on pre-V4 kernel falls back to `IsolateNetwork`.**
  Previously, declaring `Net{ports:[443]}` on a kernel < 6.7 silently resulted in
  no network isolation at all (worse than declaring no `Net` capability).
  Now `caps_to_rules` detects V4 availability via a new public `sandbox::landlock_v4_available()`
  function and emits `IsolateNetwork` (deny-all) with a `tracing::warn!` on pre-V4 kernels.
  Documented in `THREAT_MODEL.md BP-4a`.
- **F-003 (P1): uid/gid_map written after `unshare(CLONE_NEWUSER)`.**
  `apply_compiled_inner` now writes `/proc/self/setgroups=deny`, `uid_map`, and
  `gid_map` (1:1 mapping of the real uid) after a successful `unshare`. Without
  this, the subprocess ran as the overflow uid (`nobody`/65534) for DAC purposes,
  silently defeating `AllowFsRead`/`AllowFsWrite` Landlock grants for user-owned
  files with modes < 0644.

### Fixed
- **F-004 (P1): FUSE `read()` `offset + size` overflow fixed.**
  `agents_fs.rs:305` now uses `offset.saturating_add(size as usize)` to prevent
  a panic in debug mode on kernel-supplied offsets near `usize::MAX`.
- **F-005 (P1): Mailbox drain moved to after `step_with_response`.**
  Previously `drain_mailbox` ran between `provide_inference` (stores response but
  doesn't push it to messages) and `step_with_response` (pushes the assistant turn).
  Injected messages were silently stitched before the assistant reply they were
  conceptually delivered after. Now the drain runs after `step_with_response` so
  injected messages land on the *next* turn's user message.
- **F-010 (P2): MCP UTF-8 validated once at newline, not per fill-chunk.**
  `read_line_bounded` now accumulates raw bytes in a `Vec<u8>` and calls
  `String::from_utf8` once at the newline. Per-chunk `str::from_utf8` failed on
  multi-byte codepoints spanning the 8 KB BufReader boundary.
- **F-011 (P1): Checkpoint version probed before full deserialization.**
  `CheckpointStore::load` now deserializes a `VersionProbe { format_version }`
  stub first, distinguishing "too new" (explicit refusal with a clear message) from
  "corrupt" (serde error). Tmp files now use a unique name
  (`checkpoint.json.<pid>.<nanos>.tmp`) to prevent races between concurrent agentd
  processes sharing a working directory.
- **F-014 (P1): README `##Status` updated to reflect Phases 0–4 complete (v0.16.0).**
- **Ship-review: `apply_compiled_inner` no longer heap-allocates in fork child.**
  `format!("0 {uid} 1\n")` inside `apply_compiled_inner` violated the function's
  async-signal-safe contract — `format!` calls `malloc`, which can deadlock in a
  multi-threaded process (Tokio runtime) if the allocator mutex is held by another
  thread at the moment of `fork`. Replaced with a stack-allocated `id_map_entry`
  helper that writes "0 {id} 1\n" into a `[u8; 16]` without any allocation.
- **Ship-review: checkpoint tmp-name uniqueness window fixed.**
  `tmp_path()` used `subsec_nanos()` (wraps 0–999,999,999 every second) instead of
  `as_nanos()` (monotonically increasing since UNIX epoch). Two saves within the same
  process in the same wall-clock second could produce the same tmp filename. Changed to
  `d.as_nanos() as u64` for true monotonic uniqueness.

### Documentation
- **F-013 (P1): CONVENTIONS.md event taxonomy completed.**
  Added 6 missing `EventKind` rows (`tools_registered`, `agent_child_result_delivered`,
  `agent_checkpointed`, `agent_restored`, `system_shutdown_requested`, `fuse_skipped`)
  and added `events.rs` to the module boundary table. Added an assertion test in
  `events.rs` that pins every variant's serialized string so the table can't drift.
- Updated `THREAT_MODEL.md`: new §1.3 (env isolation), BP-4a (port→deny-all fallback),
  log-injection note.
- **Demo config (`agents.toml`) now exercises admission control and per-agent capability grants.**
  `global_token_budget = 200_000`, `max_concurrent_inferences = 4`, both agents
  have `capabilities = [{ FsRead = { prefix = "." } }]`. Commented MCP example included.
- **CI**: Added `audit` job running `cargo audit` on every push.
- **TODOS.md**: Added TODOS entries for F-006, F-007, F-008, F-009, F-012 (deferred P2 findings).

## [p4.6] - 2026-06-13 (v0.16.0)

### Added
- **Landlock V4 TCP port enforcement**: New `SandboxRule::AllowNetConnect { port: u16 }` enforces
  per-port TCP connects via Landlock ABI V4 (Linux kernel ≥ 6.7). `Net { hosts, ports: Vec<u16> }`
  capability gains a `ports` field (`#[serde(default)]` for backward compat). `caps_to_rules()`
  maps `Net.ports` to `AllowNetConnect` rules. ABI version is detected at runtime via
  `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)`: V4 (≥ 6.7) enables
  enforcement; older kernels degrade silently (BestEffort). Hostname enforcement remains advisory
  (Landlock restricts ports, not hostnames).
- **`EnforcementStatus.landlock_net`**: New boolean field on `EnforcementStatus` and
  `SandboxApplied { enforced }` payload. Allows operators to distinguish V4 net enforcement
  (TCP ports restricted) from V3/degraded (no net enforcement).
- **`run_probe --log-path` fix**: `run_probe` now threads `log_path: PathBuf` through to
  `FlightRecorder::new` via `resolve_log_path()`, honouring the CLI flag. Previously it always
  wrote to `"flight.jsonl"` regardless of `--log-path`.

### Fixed
- **Landlock FS lockout on net-only configs (CRITICAL)**: When only `AllowNetConnect` rules were
  present and no FS rules, `build_landlock_ruleset` set `handled_access_fs=ACCESS_FS_HANDLED`
  with zero path-beneath rules. After `landlock_restrict_self`, ALL filesystem access was denied
  (EACCES on every open/read/write). Fixed in two places:
  - `compile()`: ABI version is now queried before the `has_landlock_rules` gate. On V3 kernels
    with only net rules, `has_landlock_rules=false`, so no ruleset is created at all (correct
    BestEffort degradation).
  - `build_landlock_ruleset()`: `handled_access_fs = if path_entries.is_empty() { 0 } else
    { ACCESS_FS_HANDLED }`. A net-only V4 ruleset correctly declares no FS handling.
- **`is_noop_deny_spawn` false positive for V4 net enforcement**: Added `&& !enf.landlock_net`
  check so active V4 port enforcement is not treated as a no-op sandbox.
- **Port 0 validation in `caps_to_rules()`**: Port 0 is not a valid TCP port (kernel returns
  EINVAL). `caps_to_rules()` now skips port 0 with `tracing::warn` rather than forwarding it to
  `AllowNetConnect`.
- **`PREVIEW_CHARS` constant**: Named constant replacing magic numbers `80` and `200` in
  `run_probe`; ensures truncation lengths are consistent.
- **Stale `agentd/Cargo.lock`**: Removed nested `agentd/Cargo.lock` (recorded v0.8.0); the
  workspace-root `Cargo.lock` is authoritative (v0.16.0).

### Tests
- 253 tests (up from 244 at p4.5). Coverage additions:
  - `noop_deny_spawn_false_when_landlock_net_active`: V4 net enforcement → not a noop
  - `caps_to_rules_net_port_zero_is_skipped`: port 0 filtered before AllowNetConnect
  - `allow_net_connect_only_no_fs_rules_does_not_lock_out_fs`: net-only compile must succeed
  - `compile_net_only_has_landlock_rules_iff_v4_available`: BestEffort consistency check
  - `no_fuse_env_var_falsy_values_do_not_activate`: AGENTOS_NO_FUSE=0/false/no/"" are falsy
  - `log_path_flag_without_value_exits_nonzero`: --probe --log-path (missing arg) exits non-zero
  - `allow_net_connect_enforcement_status_reflects_abi` (Linux): fd/flag consistency on V3/V4
  - `allow_net_connect_with_fs_rule_compiles_together` (Linux): combined FS+net compiles

## [p4.5] - 2026-06-13 (v0.15.0)

### Added
- **`--log-path <file>` CLI flag and `log_path` TOML field**: Override the flight
  recorder destination. Precedence: CLI `--log-path` > TOML `log_path` > default
  `"flight.jsonl"`. `--log-path` missing its value argument now fails with a clear
  error instead of silently falling back. `run_agent` helper `resolve_log_path`
  encapsulates the precedence chain.
- **aarch64 DenySpawn noop detection**: On non-x86_64 targets where seccomp is not
  compiled, a sandbox with only `DenySpawn` produces no kernel mechanism. The runtime
  now detects this and emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }`
  instead of a misleading `SandboxApplied` with all-false fields. Detection is gated
  on `DenySpawn` specifically in the rule set (not `!is_empty()`) to avoid false
  positives when FS rules were also present but Landlock is unavailable.
- **`EventKind` extracted to `events.rs`**: The `EventKind` enum moved from
  `flight_recorder.rs` into its own `events.rs` module and is re-exported from
  `flight_recorder` for backward compat. Makes the event taxonomy a first-class module.
- **`BR2_CCACHE`**: Buildroot ccache enabled in `distro/buildroot.config`. Subsequent
  clean builds use the host cache (~2 min vs ~30 min).

### Fixed
- `is_noop_deny_spawn` call site now checks specifically for `SandboxRule::DenySpawn`
  in the rule set rather than `!is_empty()`, preventing a misleading
  `"deny-spawn-unsupported-arch"` diagnostic when FS or network rules were present.

### Tests
- 244 tests (up from 225 at p4.4). Coverage additions:
  - `parse_log_path`: 4 cases including trailing-flag-no-value (documents silent-None contract)
  - `filter_positional_args`: 4 cases including both flags together
  - `resolve_log_path`: 3 precedence-chain cases
  - `is_noop_deny_spawn`: 6 cases covering all 4 enforcement fields + has_rules variants

## [p4.4] - 2026-06-13 (v0.14.0)

### Added
- **`checkpoint.json` mode 0600**: `write_mode_600()` creates the tmp file with
  `O_CREAT|O_EXCL|mode(0o600)` plus unlink-retry, guaranteeing 0600 even if a
  stale tmp file exists at a different mode. `rename(2)` atomically replaces the
  final `checkpoint.json`. Checkpoint is now owner-readable only regardless of umask.
- **pre_exec sandbox error pipe**: `McpClient::spawn` on Linux creates a
  `pipe2(O_CLOEXEC)` error pipe *only* when a sandbox is configured. On spawn
  failure the error message includes `"(sandbox stage: 'sandbox'|'unknown')"` so
  operators can distinguish a sandbox-apply failure from a missing-binary error.
  Unsandboxed servers produce a clean error without the stage suffix.
- **`--no-fuse` CLI flag + `AGENTOS_NO_FUSE` env var**: `agentd --no-fuse agent.toml`
  or `AGENTOS_NO_FUSE=1 agentd agent.toml` skips the FUSE mount and emits a
  `FuseSkipped` flight event. `AGENTOS_NO_FUSE=0/false/no` correctly disables the
  flag (any other non-empty value enables it). Makes CI output clean.
- **`EventKind::FuseSkipped`**: new flight event kind emitted when `--no-fuse` is
  active; preserves the CONVENTIONS.md invariant that every meaningful step is
  recorded (analogous to `SandboxSkipped`).
- **`sandbox_probe` integration tests (Linux)**: 3 tests in `tests/integration.rs`
  — `allowed_path_read_succeeds`, `denied_path_read_fails`, `deny_spawn_blocks_fork`
  (x86_64 only) — verify Landlock + seccomp enforcement end-to-end using the
  `sandbox_probe` fixture binary.

### Fixed
- THREAT_MODEL.md §3.2–3.3: updated to reflect checkpoint mode restriction.

## [p4.3] - 2026-06-12 (v0.13.0)

### Added
- **`docs/THREAT_MODEL.md`**: full threat model covering secret handling,
  flight-recorder data sensitivity, checkpoint.json exposure, budget-exhaustion DoS
  guards, supply chain posture, and sandbox bypass vectors (BP-1 through BP-6) with
  explicit "not yet fixed" labels for each known gap.

### Fixed
- **`ToolCall` event now logs `input_preview` (≤200 chars) instead of the full,
  untruncated tool input**: prevents large file contents and any short secrets
  passed as tool arguments from landing verbatim in `flight.jsonl`.
- **`ToolResult` error path now logs `error` as ≤200-char preview**: previously the
  error message (which may echo back tool arguments) was logged verbatim.
- **`AgentSpawned` event now logs `task_preview` (≤200 chars) instead of the full
  task string** on both the TOML-config path (`main.rs`) and the dynamic spawn path
  (`scheduler.rs`); both now use `truncate()` with the `…` truncation marker.
- **`truncate()` and `PREVIEW_CHARS` made `pub` in `agentd::agent`**: previously
  private, preventing reuse from `main.rs` and `scheduler.rs`.

### Known Limitations (TODOS.md)
- `checkpoint.json` has no encryption or restricted file permissions; tracked as
  P3 TODOS entry for a future increment.
- 200-char truncation does not prevent short secrets (≤200 chars) in tool
  arguments; operational guidance: pass secrets via environment, not tool inputs.
- `cargo audit` CVE scanning not yet in CI.

### Tests
- All 216 tests pass (macOS; +1 new unit test for ToolResult error truncation).

## [p4.2] - 2026-06-11

### Added
- **`IsolateNetwork` and `IsolateMount` `SandboxRule` variants**: applied via `unshare(CLONE_NEWUSER | CLONE_NEWNET/CLONE_NEWNS)` in `pre_exec`. BestEffort degradation if kernel policy blocks user namespaces (`EPERM`/`ENOSYS`).
- **`Net` capability now enforced at kernel level**: `caps_to_rules()` adds `IsolateNetwork` whenever the `Net` capability is absent. MCP servers without an explicit `Net` grant are network-isolated by default. Previously `Net` was advisory-only.
- **`isolation = "gvisor"` field on `[[tools.mcp_servers]]`**: wraps the server command with `runsc do [--network=none] --`. agentd fails fast at startup if `runsc` is not found on PATH. gVisor's Sentry handles all syscall interception — Landlock/seccomp/namespace pre_exec is skipped for gVisor-mode servers.
- **`EnforcementStatus` extended**: `namespace_net: bool` and `namespace_mount: bool` fields added. `SandboxApplied` event payload extended with `isolation`, `namespace_net`, `namespace_mount` fields.
- **`CONFIG_USER_NS=y`, `CONFIG_NET_NS=y`, `CONFIG_UTS_NS=y`** in `distro/kernel-extras.config` for QEMU image.

### Changed
- **Breaking:** `capabilities = []` now also produces `IsolateNetwork` (network-isolated). Previously it produced only `DenySpawn`. Servers that need outbound access must add `Net` to their capabilities list.
- **`capabilities = ["Spawn"]` behavior**: previously produced empty rules (caught by `mcp_require_capabilities` as a bypass). Now produces `[IsolateNetwork]` — a real enforcement rule. The config is valid; the server can spawn children but cannot reach the network.

### Known Limitations (TODOS.md)
- `runsc do` is experimental; full OCI bundle integration deferred.
- `clone3()` bypass remains in the namespace-only path (gVisor fixes it).
- `CLONE_NEWPID` for PID namespace requires a re-fork; deferred.

### Tests
- **209 tests pass** (macOS + CI).
- 8 new sandbox unit tests (`isolate_network/mount` variants, `enforcement_status` namespace fields).
- 3 new config unit tests (`isolation` field parsing).
- 7 updated `caps_to_rules` unit tests reflecting `IsolateNetwork` default.
- 1 new integration test: `isolation_gvisor_fails_fast_when_runsc_not_on_path` (Linux only).

## [p4.1] - 2026-06-11

### Added
- **`EnforcementStatus` struct** in `sandbox/src/lib.rs`: `{ landlock: bool, seccomp: bool, spawn_enforcement: &'static str }` — returned by `CompiledSandbox::enforcement_status()` and included in `SandboxApplied` flight events, so operators can distinguish kernels where Landlock or seccomp degraded to a no-op.
- **`mcp_require_capabilities = true`** flag in `[tools]` config: when set, startup fails if any MCP server would run unsandboxed (missing `capabilities` field OR field present but `caps_to_rules()` produces empty rules). Lists all offending server names in the error message.
- **CI binary size guard**: new workflow step checks that the x86_64-unknown-linux-musl release binary is ≤ 4 MB (4 194 304 bytes); fails with a clear message if exceeded.

### Fixed
- **aarch64 BPF gate**: seccomp-bpf fork/vfork block is now gated under `#[cfg(target_arch = "x86_64")]`. On aarch64 (and other non-x86_64 arches), `DenySpawn` emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }` instead of installing a no-op filter that silently claims enforcement.
- **`compile()` moved to `main.rs`**: `McpClient::spawn` no longer calls `compile()` internally. The parent compiles rules before fork and passes `Option<CompiledSandbox>` directly, keeping the child's `pre_exec` closure allocation-free.
- **`mcp_require_capabilities` bypass**: validation now calls `caps_to_rules()` to check for empty effective rules, not just `capabilities.is_none()`. `capabilities = ["Spawn"]` (which maps to zero kernel rules) is correctly rejected.
- **`SandboxSkipped` on non-Linux with capabilities**: the `had_sandbox` variable is captured before the compiled sandbox is consumed by `McpClient::spawn`, fixing a case where the non-Linux `SandboxSkipped` event was never emitted for servers with capabilities configured.
- **Misleading sandbox log**: the "MCP server running unsandboxed" warning now distinguishes between "no capabilities field" and "capabilities produce no effective rules".

### Tests
- **208 tests pass** (macOS + CI).
- 6 `EnforcementStatus` unit tests in `sandbox/src/lib.rs`.
- 4 `mcp_require_capabilities` integration tests in `agentd/tests/mcp.rs`, including a regression test for the `capabilities = ["Spawn"]` bypass.
- `MAX_BYTES` named constant replaces bare `4194304` in the CI size guard script.

## [p3.3] - 2026-06-11

### Added
- **`sandbox/` crate**: new Rust library crate (`sandbox`) in the workspace. Provides
  kernel-level enforcement for MCP server subprocesses via two mechanisms:
  - **Landlock LSM** (Linux 5.13+): filesystem path-beneath rules. `AllowFsRead { prefix }`
    grants `ReadFile | ReadDir`; `AllowFsWrite { prefix }` grants all ABI V1 flags except
    Execute. BestEffort — degrades silently on older kernels without breaking startup.
  - **seccomp-bpf** (`DenySpawn` rule): classic BPF filter installed in `pre_exec` that
    blocks `fork(2)` and `vfork(2)` on x86_64, preventing the MCP server from spawning
    new child processes. Exec is intentionally left unblocked (the initial `execve` that
    loads the MCP binary must succeed); Landlock FS rules persist across exec.
- **`capabilities` field on `[[tools.mcp_servers]]`**: optional array of capability objects
  (`FsRead { prefix }`, `FsWrite { prefix }`, `Net { hosts }`, `Mcp { server, tools }`,
  `Spawn`). When present, a sandbox is compiled and applied to the server subprocess before
  exec. When absent, the server runs unsandboxed with a `tracing::warn!` and a
  `SandboxSkipped` flight event. `capabilities = []` with no `Spawn` produces a
  `DenySpawn`-only sandbox (fork/vfork blocked; no FS restriction).
- **`caps_to_rules()` adapter** in `main.rs`: converts agent `Capability` values to
  `SandboxRule` values — `FsRead`/`FsWrite` map 1:1; `Spawn` suppresses `DenySpawn`;
  `Net`/`Mcp` are advisory (kernel-level net enforcement deferred to Landlock ABI V4).
- **`EventKind::SandboxApplied` / `SandboxSkipped`**: emitted in `flight.jsonl` after
  each MCP server spawn, recording which rules were applied or why the sandbox was skipped.
- **`CONFIG_SECCOMP=y` / `CONFIG_SECCOMP_FILTER=y`** added to `distro/kernel-extras.config`.
- **`docs/SPIKES/p3.3-ebpf-lsm.md`**: implementation spike doc covering raw syscall ABI,
  BPF filter construction, execute-bit exclusion, known limitations, and CI gate.

### Fixed
- **`O_NOFOLLOW` on Landlock path fds**: `open_path_fd` now passes `O_NOFOLLOW` so a
  symlink at the configured prefix cannot redirect the Landlock allowance to another dir.
- **`SandboxApplied` accuracy**: only emitted on Linux (non-Linux is a no-op platform);
  not emitted when compiled rules are empty (e.g. `capabilities = [{ Spawn }]` only).
- **Empty `caps_to_rules` result treated as no sandbox**: `capabilities=[{Spawn}]` maps to
  zero kernel rules and now correctly emits `SandboxSkipped` rather than a misleading
  `SandboxApplied { rules: [] }`.

### Tests
- **180 tests pass** (macOS + CI); Linux-gated tests (`allow_fs_write_*`, `combined_fs_*`,
  `deny_spawn_bpf_includes_vfork_on_x86_64`) verified by CI.
- 6 `caps_to_rules` unit tests in `main.rs`.
- 3 `McpServerConfig` capability TOML parse tests in `config.rs`.
- 1 `sandbox_event_kinds_serialize_to_snake_case` test in `flight_recorder.rs`.
- 5 sandbox-crate tests: `PartialEq`, Landlock rule construction, combined Landlock+BPF,
  vfork BPF instruction count (expects 6: `load + fork + vfork + allow`).

## [p3.2] - 2026-06-10

### Added
- **`agentd/src/checkpoint.rs`**: new module — `CheckpointStore` (atomic
  `tmp → rename` writes), `AgentCheckpoint`, `SchedulerCheckpoint`,
  `AwaitingEntry` serde types; `FORMAT_VERSION = 1`.
- **`AgentTask::to_checkpoint()`** / **`from_checkpoint()`** / **`is_terminal()`**:
  serialise/deserialise agent working state; `from_checkpoint` always clears
  `terminal` to guard against the terminal-race (OV-2); `is_terminal` lets the
  scheduler filter finished agents from checkpoint writes.
- **Periodic auto-checkpoint**: `SchedulerConfig::checkpoint_interval_turns`
  (default `1`); fires at every `provide_tool_results` boundary when the agent
  turn count is a non-zero multiple of the interval.
- **SIGTERM checkpoint**: when the scheduler's SIGTERM handler fires it calls
  `checkpoint_all()` before exiting; if the save fails the error is recorded and
  shutdown continues without crashing.
- **Corrupt-checkpoint recovery**: if `checkpoint.json` exists but fails to
  parse, `main.rs` renames it to `checkpoint.json.corrupt` and boots fresh.
- **Full restore**: `Scheduler::new()` accepts an optional `SchedulerCheckpoint`;
  restores `awaiting` map, per-agent mailboxes, `tokens_spent`, `child_seq`, and
  `spawn_depths`; orphan children in the checkpoint (not in the TOML spec) are
  also restored.
- **New flight events**: `AgentCheckpointed { agent_id }`,
  `AgentRestored { agent_id }`, `CheckpointFailed { reason }`.
- **`agentd/.gitignore`**: `checkpoint.json` and `checkpoint.json.corrupt`
  excluded from version control.

### Changed
- `SchedulerConfig` gains `checkpoint_interval_turns: u32`; default `1`; `0`
  disables periodic checkpointing.
- `Scheduler::new()` signature gains a 7th argument
  `Option<SchedulerCheckpoint>`; existing call-sites in `main.rs` updated.
- `InferenceResponse` and `MailMessage` derive `Serialize` (required by checkpoint
  serialisation).
- `Makefile` `clippy-linux` target: add `rustup component add clippy` before the
  cargo invocation so the Docker image works on aarch64 hosts.
- Test helper `sched_cfg()` sets `checkpoint_interval_turns: 0` to prevent
  concurrent scheduler tests from racing on `./checkpoint.json.tmp`; dedicated
  checkpoint tests explicitly opt in with `checkpoint_interval_turns: 1`.

### Tests
- 9 new unit tests in `agentd/src/scheduler.rs` (checkpoint restore, periodic
  checkpoint, `AgentCheckpointed` flight event, test-isolation mutex for
  `sigterm_drains_scheduler`).
- 5 new unit tests in `agentd/src/agent/mod.rs` (`is_terminal`, `to_checkpoint`,
  `from_checkpoint`, roundtrip).
- 1 new unit test in `agentd/src/flight_recorder.rs` (checkpoint event
  serialisation).
- 10 unit tests in `agentd/src/checkpoint.rs` (serde roundtrips, save/load,
  corrupt handling).
- Total: **175 tests** (174 pass; 1 live-API integration skipped).

## [p3.1] - 2026-06-10

### Added
- **`surfaces/` crate**: new Rust library crate (`surfaces`) sibling to `agentd/`;
  root `Cargo.toml` promoted to a workspace with `members = ["agentd", "surfaces"]`
  and the release profile moved there.
- **`surfaces::snapshot`**: `SchedulerSnapshot`, `AgentSnapshot`, `AgentStatus`
  (`Running`, `Deferred`, `AwaitingChild(String)`, `Done`, `Failed`); shared via
  `Arc<RwLock<SchedulerSnapshot>>` between scheduler and FUSE handler.
- **`surfaces::agents_fs`** (Linux-only FUSE handler): `AgentsFs` implements
  `fuser::Filesystem`; inode scheme (root=1, agent dirs from 1010 step 10, file
  offsets +1..+4); four virtual files per agent (`status`, `context_size`, `budget`,
  `flight`); TTL=0 (no kernel caching); `read_flight_tail()` scans last 64 KB of
  `flight.jsonl`, returns up to 20 matching lines per agent.
- **`surfaces::agents_fs::mount()`**: spawns FUSE `BackgroundSession` on Linux;
  no-op stub on other platforms — clean build everywhere.
- **`Scheduler` snapshot plumbing**: `Scheduler::new()` accepts a 7th argument
  `Arc<RwLock<SchedulerSnapshot>>`; `update_snapshot()` is called after the seed loop
  and after every effect result, keeping the snapshot current.
- **`AgentTask` getters**: `context_tokens()` and `task_preview(max_chars)` added
  to `agent/mod.rs` for snapshot population.
- **`EventKind::FuseMounted` / `FuseUnmounted`**: emitted in `main.rs` when
  `agentd` mounts/unmounts `/agents`.
- **`distro/overlay/agents/.gitkeep`**: creates the `/agents` mount point in the
  Buildroot rootfs overlay.
- **`CONFIG_FUSE_FS=y`** in `distro/kernel-extras.config` so the QEMU VM can
  serve FUSE mounts.
- **15 unit tests** in `surfaces/src/agents_fs.rs` covering inode allocation, file
  content rendering, read slicing, and flight tail parsing.

### Changed
- **`fuser` dependency** is in `[target.'cfg(target_os = "linux")'.dependencies]`
  to avoid `pkg-config --libs fuse` failing on macOS during `cargo check/test`.
- All `#[cfg(target_os = "linux")]`-gated items that are also needed by tests use
  `#[cfg(any(test, target_os = "linux"))]` so the test suite runs on all platforms.

## [p2.5] - 2026-06-09

### Added
- **MCP tools/list pagination**: `McpClient::spawn` now follows `nextCursor` in a
  cursor-based loop until all pages are exhausted. Previously only the first page was
  fetched; tools on page 2+ were silently dropped.
- **`McpClient::shutdown()` method**: sends `notifications/shutdown` (JSON-RPC notification,
  no id), waits up to 5 s for the server to exit cleanly, then escalates to SIGTERM, waits
  another 5 s, and lets `kill_on_drop` deliver the final SIGKILL. Servers that flush WAL or
  release locks on clean exit now get the chance to do so.
- **Graceful shutdown on all exit paths**: `run_agent` in `main.rs` calls
  `client.shutdown().await` for each MCP client on three exit paths: successful completion,
  `AnthropicGateway::from_env` failure, and `Scheduler::new` failure. The previous
  code used `?` early-return on the latter two, causing SIGKILL-only teardown.
- **`StopReason::MaxTokens` → `AgentEffect::Failed`**: when the model is cut off
  mid-generation the agent now emits a `BudgetExceeded` flight event and returns
  `AgentEffect::Failed("model generation hit max_tokens limit …")` instead of silently
  returning `Ok("")`. Callers can now distinguish a truncated response from a real empty answer.
- **`nix` dependency** (`v0.29`, `signal` feature) promoted from dev-dependency to
  dependency so `kill(SIGTERM, …)` is available in production `shutdown()`.
- **`tokio` `fs` feature** added to `Cargo.toml` for `tokio::fs` in native tools.

### Changed
- **Native tools use `tokio::fs`**: `ReadFile`, `WriteFile`, and `ListDir` now use
  `tokio::fs::read_to_string`, `tokio::fs::write`, `tokio::fs::create_dir_all`, and
  `tokio::fs::read_dir` with the async entry iterator. Previously they used blocking
  `std::fs` calls on the tokio thread pool, which would have stalled concurrent agents.

### Tests
- 2 new unit tests in `agent/mod.rs`: `max_tokens_with_no_text_returns_failed`,
  `max_tokens_with_partial_text_returns_failed`.
- 2 new integration tests in `tests/mcp.rs`: `mcp_pagination_loads_all_pages` (asserts
  all three tools from a two-page echo-mcp paginated server appear in `tools_registered`);
  `mcp_graceful_shutdown_sends_notification` (asserts echo-mcp writes a file on
  `notifications/shutdown` before exiting).
- `echo-mcp` fixture updated: `--paginate` flag returns two-page tool list with
  `nextCursor`; `--shutdown-file <path>` flag writes `"shutdown"` to path on notification.

## [p2.3] - 2026-06-09

### Added
- **SIGTERM/SIGINT handling in `Scheduler::run()`**: replaced the `while let
  Some(er) = pending.next().await` loop with `loop { tokio::select! { ... } }`.
  Signal arms set `shutdown_requested = true` and break, causing in-flight futures
  to be dropped and the existing deferred-queue drain to run.
- **`EventKind::SystemShutdownRequested`** flight event: emitted with
  `{ "signal": "SIGTERM" }` or `{ "signal": "SIGINT" }` when a signal fires.
- **`tokio` `signal` feature** added to `Cargo.toml`; **`nix` dev-dependency**
  (v0.29, `signal` feature) added for test-side signal delivery.
- **`sigterm_drains_scheduler` test**: sends SIGTERM 50 ms into a 30-second gateway
  delay; asserts `run()` returns in < 5 s and the flight log contains the shutdown
  event.

### Not in scope
- Graceful MCP shutdown (SIGTERM + drain before SIGKILL) → p2.5
- Essential mounts: already done in `distro/overlay/init` (p2.2)
- Zombie reaping: already handled by tokio (owns SIGCHLD; competing handler disallowed)

## [p2.2] - 2026-06-09

### Added
- **`distro/` Buildroot external tree**: x86_64 musl + BusyBox; `make build` produces
  `output/bzImage` + `output/rootfs.cpio.gz` (cpio initramfs).
- **`/init` PID-1 script** (`distro/overlay/init`): mounts proc/sys/devtmpfs, mounts two
  virtio-9p host directories (`secrets0` → `/run/secrets/`, `output0` → `/run/output/`),
  sources `agentos.env`, and `exec`s agentd. Drops to busybox sh on mount/secret failure.
- **virtio-9p kernel config** (`distro/kernel-extras.config`): `CONFIG_9P_FS`, `CONFIG_NET_9P`,
  `CONFIG_NET_9P_VIRTIO`, `CONFIG_VIRTIO_NET`, `CONFIG_IP_PNP_DHCP` applied on top of
  `x86_64_defconfig`.
- **`make prereqs / build / run / test / clean / distclean`**: `test` boots with `-no-reboot`
  and confirms an `agent_completed` or `budget_exceeded` event in `output/test-run/flight.jsonl`.
- **Demo agent config** (`distro/overlay/etc/agentd/agent.toml`): Haiku model, native tools
  only, writes a greeting to `/run/output/greeting.txt`. Validates the full boot-to-inference path.
- **No system CA certs needed**: agentd's bundled `webpki-roots` (via `reqwest rustls-tls`)
  provides Mozilla CAs; the rootfs carries no `ca-certificates` package.

## [0.7.0] - 2026-06-09

### Changed
- **`reqwest` TLS backend**: switched from `native-tls` to `rustls-tls` (`default-features = false, features = ["json", "rustls-tls"]`). No longer requires OpenSSL headers at build time or system OpenSSL at runtime.

### Build
- **Static musl binary**: `cross build --target x86_64-unknown-linux-musl --release` produces a `static-pie linked, stripped` ELF binary (~3.1 MB) with no dynamic dependencies. Use `cross` (Docker-based) from macOS; on Linux with musl toolchain available, `cargo build --target x86_64-unknown-linux-musl --release` works directly.

## [0.6.0] - 2026-06-09

### Added
- **`AgentCard { id, name, description, skills }`**: derived from `AgentConfig` at scheduler seed time. Emits `agent_card_registered` flight event per agent.
- **`AgentConfig` identity fields**: optional `name`, `description`, `skills` TOML fields (all with `#[serde(default)]`). `name` defaults to `id` when absent.
- **`bus.rs` module**: `MailMessage { from, content }` and `Mailboxes = HashMap<String, Vec<MailMessage>>`. Canonical home for A2A bus primitives.
- **`list_agents` tool**: returns a sorted JSON array of all registered `AgentCard`s. No capability required — available to every agent.
- **`send_message` tool + `AgentEffect::SendMessage { call_id, to, content }`**: sole-call tool intercepted by the scheduler. Delivers message to recipient's mailbox; synthesizes an immediate `ToolResult` so the sender continues. Unknown recipient returns an `is_error` tool result (no panic, no crash).
- **Mailbox drain before each inference**: `drain_mailbox` is called after `provide_inference`/`provide_tool_results` and before `step()`. `AgentTask::inject_messages` appends mail as a `Block::Text` to the last `User` message, preserving the Anthropic API's strict alternating-role requirement.
- **Shutdown drain fix**: `shutdown_requested: bool` in `SchedulerState`. `drain_deferred` now checks this flag and emits `agent_admission_denied { reason: "shutdown" }` instead of re-queuing agents that can never run.
- **New flight events**: `AgentCardRegistered`, `MessageSent`, `MessageReceived`.
- **9 new unit tests** covering: `inject_messages` appends to last User msg; empty inject is noop; sole-call guard for `send_message`; missing `to` field error; `send_message` delivery + `message_sent` event; unknown-recipient error; `AgentCard` name defaulting; explicit name/skills round-trip; TOML parsing of new identity fields.

### For contributors
- `dispatch_send_message` in `scheduler.rs` handles the full message lifecycle: recipient validation → mailbox push → `MessageSent` flight event → synthesize ToolResult → re-enqueue sender.
- `register_native` gains a third `cards: Option<Arc<Vec<AgentCard>>>` parameter; pass `None` in tests.
- `agents.toml` example updated with `name`, `description`, `skills` fields on both agents.

## [0.5.0] - 2026-06-09

### Added
- **`spawn_agent` tool**: an agent with the `Spawn` capability calls `spawn_agent{task, child_id?, priority?, token_budget?}` to create a child agent. The child runs to completion; its result is injected back into the parent as a `ToolResult` so the parent can continue. The call must be the sole tool use in its turn.
- **`SchedulerState` refactor**: all mutable scheduler run-loop state consolidated into a single `SchedulerState` struct (`agents`, `outcomes`, `pending`, `deferred`, `in_flight`, `tokens_spent`, `awaiting`, `child_seq`, `spawn_depths`, `max_spawn_depth`). Eliminates the previous 13-loose-locals pattern.
- **`AgentEffect::SpawnAgent { call_id, config }`**: new variant intercepted by the scheduler before any tool `invoke()`. The agent state machine recognizes a `spawn_agent` tool-use response and returns this effect instead of `CallTools`.
- **Spawn depth limit**: `max_spawn_depth: u32` in `[scheduler]` TOML (default 4). If exceeded, the parent receives an `is_error` tool result instead of a child being created.
- **Child admission denial**: if a child's first inference is denied (budget or slot exhausted), the parent receives an `is_error` tool result and continues running.
- **`Capability::Spawn` enforcement**: `dispatch_spawn` checks the parent's cap set; absence of `Spawn` returns an `is_error` tool result to the parent rather than creating a child.
- **`agent_child_result_delivered` flight event**: emitted when a child's result is injected into its parent, carrying `{child_id, parent_id, call_id, success}`.
- **`SpawnAgentTool`** in `native.rs`: registered as a stub tool so it appears in `filtered_specs` for agents with `Spawn` capability. Its `invoke()` is a safety net that always errors (the scheduler intercepts before `invoke` is reached).
- **Child ID naming**: auto-generated as `"{parent_id}-child-{seq}"` with a monotonic counter.
- **Child inherits parent's capabilities and `model_cfg`**: spawned child uses the same model and capability set as its parent (unless overridden).

### Fixed
- `Capability::Spawn` was previously hard-coded to always return `false` in `satisfies()`; it now correctly checks whether the granted set contains `Spawn`.
- `SchedulerConfig::Default` now returns `max_spawn_depth = 4` instead of `0` (the derived `Default` was overriding the serde default, silently disabling all spawning for Rust-constructed configs).

### For contributors
- `SpawnConfig` struct in `config.rs`: `{ child_id: Option<String>, task: String, priority: u32, token_budget: Option<u64> }`.
- `dispatch_spawn` in `scheduler.rs` handles the full spawn lifecycle: cap check → depth check → child ID → child `AgentTask` creation → awaiting registration → seeding.
- `handle_agent_terminal` routes child completions to the parent via `provide_tool_results` + `step` + `enqueue_or_defer`; non-child completions go straight to `outcomes`.
- `send_message` deferred to p1.6 (Agent Cards increment).

## [0.4.0] - 2026-06-08

### Added
- **Capability system** (`capabilities` TOML field on `[[agents]]`/`[agent]`):
  least-privilege tool grants — `FsRead{prefix}`, `FsWrite{prefix}`, `Net{hosts}`,
  `Mcp{server, tools}`, `Spawn`. Absent field = unrestricted (backward compat);
  `capabilities = []` = deny all.
- **Capability enforcement at `ToolRegistry::invoke`**: the single unbypassable
  boundary; denials emit a `capability_denied` flight event with data `{tool, required}`
  (the agent id is in the event's top-level `agent` field) and return an `is_error`
  tool result to the agent.
- **`filtered_specs`**: agents only receive the tool specs they are authorized to
  call in their inference context — no wasted inference turns on inaccessible tools.
- **`normalize_path`**: resolves `..` components without filesystem access before
  prefix matching, blocking directory traversal (e.g. `/workspace/../etc/passwd`
  is correctly denied against a `/workspace` prefix grant).
- **`satisfies_type`**: type-level capability check used by `filtered_specs` —
  "does this agent have any FsRead capability?" vs. "can they access this specific path?"
- **`McpTool` server provenance**: `server_name` field on `McpTool` enables
  `Mcp{server, tools}` capability gating on per-server MCP tool access.

### For contributors
- New `agentd/src/capability.rs`: `Capability` enum, `normalize_path`, `satisfies`,
  `satisfies_type`. All capability logic lives here; no policy is embedded in tools.
- `Tool` trait gains `fn required_capability_for(&self, input: &Value) -> Option<Capability>`
  (default `None`). Path-based tools return the actual access path at invocation time.
- `ToolRegistry::invoke` gains `(agent_id, cap_set, recorder)` params.
- `run_tools_sequential` gains `cap_set: Option<&[Capability]>` param; threaded through
  to `invoke`. Driver passes `None` (backward compat).
- `Scheduler::new` calls `filtered_specs(cap_set)` per agent instead of shared `specs()`.

## [0.3.0] - 2026-06-08

### Added
- **Metered scheduling & admission control** (`[scheduler]` TOML section): cap total
  token spend across all agents with `global_token_budget` and limit how many model
  calls can run concurrently with `max_concurrent_inferences`. Both default to `0`
  (unlimited), preserving all prior behavior.
- **Priority-based deferred queue**: each agent carries a `priority: u32` field
  (default `0`). When the concurrency cap is full, the agent's inference is queued and
  admitted in descending-priority order (FIFO within a band) when a slot opens.
- **Admission-control flight events**: `agent_scheduled`, `agent_deferred`, and
  `agent_admission_denied` appear in `flight.jsonl`, giving full observability into
  scheduler decisions.

### Fixed
- `in_flight` underflow guards promoted from `debug_assert!` (compiled out in release)
  to `assert!`, ensuring the invariant is enforced in production builds.

### For contributors
- `SchedulerConfig` struct in `config.rs` carries `global_token_budget` and
  `max_concurrent_inferences`; wired into `Scheduler::new` via `main.rs`.
- `DeferredInfer` type with a custom `Ord` drives the `BinaryHeap` deferred queue.
- `drain_deferred` / `enqueue_or_defer` manage the admission lifecycle; both are
  tagged with `TODO(p1.x)` noting a planned `SchedulerState` refactor.

## [0.2.0] - 2026-06-08

### Added
- **Multi-agent scheduler**: Run multiple agents concurrently on independent tasks with a
  single `agentd agents.toml` invocation. Agents share a gateway and tool registry; each
  runs its own perceive → infer → act → observe loop without blocking the others.
- **`[[agents]]` config form**: Declare multiple agents in one TOML file using the
  `[[agents]]` array. The original `[agent]` single-agent form is fully backward-compatible.
- **`agents.toml` example**: Ships a two-agent example config alongside the existing
  `agent.toml`.
- **`AgentFailed` flight event**: Emitted when an agent terminates due to an inference
  error, completing the `AgentSpawned` ↔ terminal-event symmetry in the flight log.
- Non-zero exit code when any agent fails; individual per-agent errors logged with agent ID.

### For contributors
- Agent loop refactored into a sans-IO state machine (`AgentTask` + `AgentEffect`).
  `step()` → `AgentEffect` drives the loop; the scheduler performs all async IO and
  feeds results back via `provide_inference()` / `provide_tool_results()`. Enables
  concurrent IO across agents without threads.
- `driver::run` is now a single-agent backward-compat shim; the scheduler is the
  primary execution engine for all runs.
- `AgentSpawned` events are emitted before gateway initialization so startup events
  always appear in the flight log even when API key setup fails.
- `run_tools_sequential` extracted as `pub(crate)` in `agent/mod.rs`, shared by the
  driver and the scheduler.

### Fixed
- MCP child processes are now properly cleaned up on agent failure: `run_agent` returns
  `Err` instead of calling `std::process::exit(1)` while `mcp_clients` is still in scope,
  ensuring `kill_on_drop` fires before the process exits.
- Guard added for `stop_reason=tool_use` responses that contain no `ToolUse` blocks —
  previously would have sent an empty User message to the API.

## [0.1.0] - 2026-06-07

Initial release: config loader, flight recorder, `InferenceGateway` trait, Anthropic
backend, tool ABI, native file tools, MCP stdio client, and a single-agent
perceive → infer → act → observe loop.
