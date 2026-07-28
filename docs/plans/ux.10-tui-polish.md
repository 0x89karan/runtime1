# ux.10 — TUI polish: log streaming + input ergonomics + ecosystem crates

**Track:** UX (operator cockpit)
**Branch:** `ux.10-tui-polish` (off `main` after ux.1/v0.86.0)
**Depends on:** ux.1 (converse rail, shipped v0.86.0), ux.9 (cockpit mode, shipped v0.82.0), p7.7 (management HTTP API, shipped v0.53.0)
**Status:** ✅ SHIPPED in two parts — sub-part B (inputs) v0.113.0; sub-part A (Logs view) this
increment. Sub-part C (`color-eyre`) STRUCK at the /autoplan gate as redundant. Plan body below is the
2026-07-16 original; the two sections at the END (the /autoplan gate decisions + the eng consensus
reshape) are authoritative wherever they differ.

**Sub-part A — as built** (deltas from the body, all consistent with the eng consensus):
- Key `[l]`; `g`/`G`/`Home`/`End` = in-view top/bottom, `Tab` = service filter, `/` = search
  (highlight + `n`/`N`, NOT a filter — a matching log line is only useful with its context), `[t]` =
  relative↔absolute timestamps.
- `--tail 500` bounds the initial backfill (compose replays the ENTIRE history by default, which the
  2 000-line ring would only tail-drop).
- `A2` orphan-kill went one process deeper than the plan assumed: `docker compose` is a CLI plugin, so
  `docker` forks `docker-compose` and passes our pipe down. `child.kill()` alone would strand it, so the
  tail is spawned with `process_group(0)` and torn down with `kill(-pid, SIGKILL)` + `wait()`
  (`docker::kill_tail`, regression-tested with a fake `docker` that forks a grandchild — no daemon needed).
- New files: `agentctl/src/docker.rs` (process concerns only) + `agentctl/src/watch/logs.rs`
  (`LogLine`/`LogsState` + parse + scroll/search state machine). `AppEvent::LogLines(Vec<LogLine>)` +
  `AppEvent::LogLinesDropped(usize)`; `View::Logs`; `LogsState::available` gates key AND legend.
- **Batching alone was NOT enough** (found at /qa, not by tests): `docker compose logs` writes
  line-by-line, so the pipe hands the reader one line per read and "flush when the buffer runs dry"
  degenerates to one-line batches. A 5 000-line burst then overflowed the 256-slot channel and lost
  **4 479 lines (~90%)** — honestly counted in the header, but useless as a log view. A full channel is
  now WAITED OUT for up to 250 ms (`LOG_SEND_BACKPRESSURE`, sized against the render loop's ~30 ms
  drain) before anything counts as dropped; the same burst now lands 2 000/2 000 ring lines with zero
  drops. Only a wait past the deadline counts as loss, so a wedged render loop still can't stall the
  reader.

**Sub-part A — second /review round** (5 specialist subagents + red team + `codex review`; run because the
first round was Codex-adversarial only). Highest-value finds, all fixed: the unconditional `Effect::Redraw`
on log events (Dashboard rebuilt at 33 Hz whenever the tail was chatty — the eager tail made this the
DEFAULT state, not an edge case); bracketed paste inserted per-char into `tui-input` (O(n²) on the render
thread → a big paste froze the loop with Ctrl-C inert); the 250 ms backpressure window starving the
drop-on-full authoritative producers (priority inversion → 60 ms); `--tail` being per-CONTAINER (4 services
× 500 = the whole ring). The lesson worth carrying: three of these are *interaction* defects between the
new producer and the pre-existing loop, which is exactly what a diff-scoped adversarial pass does not
surface — the specialists were dispatched with explicit "what did the others miss" framing and cross-file
scope.

**Sub-part A — the QA harness is now a committed project skill:**
`.claude/skills/run-agentctl-watch/` (SKILL.md + `driver.py` + `fake-docker.sh`). It captures the pty
driver, the three pitfalls that silently defeat TUI verification (unsized pty → blank frames; grepping raw
ratatui output → split words; `kill(pid,0)` → a zombie reads as a hang), and six fake-docker modes
(`stream`/`flood`/`giant`/`empty`/`fail`/`hang`) covering the gate, the ring cap, truncation, and the probe
deadline. Verifying the skill's own recipe before committing caught a bug in the driver — `set_size` stored
the shrunken width, so the repaint trick was a no-op and captures came back near-blank, i.e. exactly the
pitfall it documents.

**Sub-part A — /qa evidence (runtime, in a real pty; the harness used a
fake `docker` that forks a grandchild, so no daemon was needed):** gate ON → `[l]ogs` in the legend, view
streams all 3 services with correct attribution; gate OFF (real docker, daemon down) → key and legend
entry both absent, `l` inert, `q` still quits; `Tab` filter ("12 of 36 lines"), `[t]` rel↔abs, `/` live +
committed search with match counts, `n` jump, `k` → PAUSED, `G` → FOLLOW; ring holds at 2 000 under a
5 000-line flood; a 12 KB newline-less record is truncated, attributed, and the stream resyncs after it;
**no orphaned `docker compose logs`/grandchild process survived any clean quit**, and the exit status
stayed 0.

---

## Goal

Make the cockpit a complete operational console by (1) adding live Docker Compose log streaming
as a new `[g]` **Logs** view, and (2) upgrading input ergonomics across all existing views using
purpose-built ratatui ecosystem crates. No new agent capabilities; pure cockpit UX.

Today's gaps:
- You cannot see what MCP sidecars, agentd, or the credential broker are actually printing —
  you have to drop out of the TUI and run `docker compose logs` in a separate terminal.
- The task input in Spawn, the search field in Memory/Inspector, the converse input rail, and
  the reason field in deny all use hand-rolled single-char accumulation with no cursor, no
  clipboard paste, no history, and no line-wrapping.

---

## Scope

### A — Logs view (`[g]` key)

A new full-screen view that tails `docker compose logs --follow --no-log-prefix` as a child
subprocess, streams lines into a bounded ring buffer, and lets you filter by service.

**Key decisions:**

**D1 — Docker context detection.** The Logs view is only shown when a Docker context is
detected. Detection: check whether `docker compose ps --quiet 2>/dev/null` exits 0 AND returns
at least one container ID. Done once at startup; result stored on `App`. If not in Docker
(bare agentd on Linux/QEMU), the `[g]` keybinding is silently absent and not shown in the
legend.

**D2 — Subprocess model.** Spawn `docker compose logs --follow --no-log-prefix --timestamps`
as a `tokio::process::Command` with `stdout` piped. A background task reads lines and sends
them via an `mpsc` channel into the existing `AppEvent` pump (new variant `AppEvent::LogLine {
service: String, ts: String, text: String }`). The child is killed on view exit or app quit.

**D3 — Ring buffer size.** 2 000 lines in a `VecDeque<LogLine>`. Oldest lines dropped when
full. Enough for a typical session without unbounded growth.

**D4 — Service filter.** Tab cycles through `[All] [agentd] [cos] [google_oauth] [search] ...`
— services discovered from the initial `docker compose ps` output. Filter applies client-side
against the ring buffer; no re-subprocess needed.

**D5 — Line format.** Each `LogLine` stores `{ service, ts, text }`. Rendered as:
`<service dim> <ts muted> <text>`. Service name is color-coded per service (stable hash →
palette). Timestamps shown as relative (`3s ago`) or toggle to absolute with `t`.

**D6 — Widget.** Use `tui-logger`'s `TuiLoggerWidget` if the log-level metadata maps cleanly;
otherwise use a plain ratatui `List` widget with a scroll offset. Given that compose log lines
are unstructured text (not structured log records), a plain scrollable `List` is simpler and
more predictable. **Decision: plain `List` with `ScrollOffset`, NOT `tui-logger`.** `tui-logger`
is designed around Rust's `log` crate — it brings in the full log-crate integration and a
global logger, which is wrong for external process stdout.

**D7 — Scroll.** `↑`/`↓`/`j`/`k` scroll; `G` jumps to bottom (follow mode); `g` jumps to top.
When at the bottom, new lines auto-scroll (follow mode). Any upward scroll pauses follow mode;
`G` resumes it. Same pattern as the Inspector view.

**D8 — Search.** `/` opens an inline search bar (see section B — `tui-input`). Matching lines
highlighted; `n`/`N` jump between matches.

---

### B — Input ergonomics (`tui-input` + `tui-textarea`)

Replace all hand-rolled char-accumulation inputs with purpose-built widgets.

**B1 — `tui-input` (single-line).** Drop in for:
- Converse rail message input (`converse.rs`)
- Memory view search (`/` in `memory.rs`)
- Inspector view search (`/` in `inspector.rs`)
- Logs view search (`/` — new)
- Deny reason field (`approvals.rs`)

`tui-input` gives cursor movement (`←`/`→`/`Home`/`End`), word-jump (`Ctrl-←`/`Ctrl-→`),
`Ctrl-A`/`Ctrl-E`, `Ctrl-W` (delete word), `Ctrl-U` (clear line), and clipboard paste
(`Ctrl-V` / `Ctrl-Shift-V` via crossterm paste events). Zero new keybinding surface — all
standard readline/emacs keys the user already expects.

**B2 — `tui-textarea` (multi-line).** Replace the single-line task input in Spawn view
(`spawn.rs`) with a `tui-textarea` widget. Spawn tasks are often multi-line (e.g. "research X,
then Y, then Z"). `tui-textarea` supports vim and emacs keybindings, configurable per instance.
Set to emacs mode for consistency with `tui-input`. Max height: 6 rows; scrolls internally if
the task is longer.

**B3 — NOT `ratatui-image`.** Token sparklines via `ratatui-image` (sixel/kitty inline images)
require terminal feature detection and add a non-trivial dependency. Deferred — budget
visualization belongs in ux.8 (live budget control) and should use ratatui's native `Sparkline`
widget (already in ratatui 0.29, no new dep) rather than image rendering.

---

### C — Panic hook hardening (`color-eyre`)

Replace the current `std::panic::set_hook` + manual `disable_raw_mode()` call with
`color-eyre`'s panic hook, which guarantees terminal restore before printing the panic message.
Also enables pretty `eyre::Report` chain formatting throughout `agentctl`.

Already partially handled by `CleanupGuard` (`watch/mod.rs`) and the SIGTERM handler added in
ux.9 — `color-eyre` closes the remaining gap where a panic on a non-main thread can corrupt the
terminal before `CleanupGuard::drop` runs.

---

## New dependencies

| Crate | Version | Size impact | Notes |
|---|---|---|---|
| `tui-input` | `0.10` | ~15 KB | Single-line input widget |
| `tui-textarea` | `0.7` | ~80 KB | Multi-line editor |
| `color-eyre` | `0.6` | ~60 KB | Panic hook + error formatting |
| `tokio::process` | already in tree | 0 | Used for compose log subprocess |

`tui-logger` is explicitly NOT added (see D6). `ratatui-image` is explicitly NOT added (see B3).
Total new compressed binary impact: estimated < 200 KB. The 6 MB CI guard should not be
threatened, but verify with `make check-size` after build.

---

## Files touched

**New:**
- `agentctl/src/watch/logs.rs` — `LogsView` state + render + `AppEvent::LogLine` handler
- `agentctl/src/docker.rs` — `detect_docker_context()`, `spawn_compose_logs()` helpers

**Modified:**
- `agentctl/Cargo.toml` — add `tui-input`, `tui-textarea`, `color-eyre`
- `agentctl/src/watch/mod.rs` — wire `AppEvent::LogLine`, `[g]` key, follow-mode scroll, docker detection at startup, `color-eyre` panic hook
- `agentctl/src/watch/app.rs` — `View::Logs` variant, `LogsState` on `App`
- `agentctl/src/watch/converse.rs` — swap hand-rolled input for `tui-input::Input`
- `agentctl/src/watch/memory.rs` — swap search field for `tui-input::Input`
- `agentctl/src/watch/inspector.rs` — swap search field for `tui-input::Input`
- `agentctl/src/watch/approvals.rs` — swap reason field for `tui-input::Input`
- `agentctl/src/watch/spawn.rs` — swap task field for `tui-textarea::TextArea`
- `agentctl/src/watch/views.rs` — legend update (`[g] logs`), help text

---

## Acceptance criteria

1. `[g]` in `agentctl watch` (Docker context): opens Logs view showing live lines from all
   compose services; `Tab` filters to one service; `/` searches; `G` follows; `↑` pauses follow.
2. `[g]` in `agentctl watch` (bare agentd, no Docker): key is absent from legend, does nothing.
3. Converse rail, Memory search, Inspector search, deny reason: left/right cursor movement works;
   `Ctrl-W` deletes word; `Ctrl-U` clears; paste via `Ctrl-V` inserts clipboard text.
4. Spawn task field: multi-line entry works; `Enter` adds a newline; `Ctrl-Enter` (or a
   dedicated `[Submit]` button) submits the spawn (existing `Ctrl-S` / `F5` binding preserved).
5. A panic on any thread restores the terminal before printing the error — verified by
   temporarily `panic!()` in a render function.
6. `make check-size` passes (agentctl ≤ 6 MB).
7. All existing workspace tests pass; at least 10 new tests covering docker detection, ring
   buffer drop behavior, and `tui-input` integration smoke tests.

---

## Out of scope

- `ratatui-image` / sparklines — belongs in ux.8.
- `tui-logger` — wrong abstraction for external process stdout.
- Any new management API endpoints — logs come from the host subprocess, not agentd.
- Windows support for `docker compose logs` subprocess — Docker Desktop on Windows is not a
  supported deployment target.

---

## Sequencing

This increment can land before or after ux.8 (budget control) — no dependency either way.
Recommended: land ux.10 first since it's self-contained polish with no new API surface, then
ux.8 (which requires a new management API endpoint for live budget writes).

---

## DECIDED at the /autoplan gate (2026-07-27)
- **Sub-part C (color-eyre) STRUCK** — redundant (TermGuard already restores on panic; producer panics are
  `catch_unwind`'d). No anyhow→eyre migration.
- **Scope: ux.10 = B + A, built + shipped in order B → A** (B contained; A is the heavy pump-refactor; A's
  `/` search reuses B's tui-input widget). Splitting A into its own PR is fine if it lands separately.
- All CONFIRMED reshape corrections below are LOCKED. `[l]` for Logs; `tui-input =0.14` + `tui-textarea
  =0.7.0` (no ratatui bump); std-thread subprocess + `child.kill()` on `Drop`; event plumbing + spawn
  Enter/Tab + bracketed-paste per the eng consensus.

## Re-grounding addendum (2026-07-27, post-ux.2b/ux.3 — corrections to the 2026-07-16 spec)

The spec above stands, with these current-state corrections (the UX tail shipped ux.2b v0.111.0 + ux.3
v0.112.0 since it was written; `mod.rs` line numbers shifted):

- **LANDMINE — the `[g]` Logs key COLLIDES.** `[g]` is now bound to the Spawn view's "generate preview"
  (`mod.rs:752`). Key dispatch is per-view, so `[g]`-opens-Logs-from-Dashboard and `[g]`-generates-in-Spawn
  do not *technically* clash — BUT autoplan/eng-review must confirm the Logs key is dispatched only where
  it's free (and update the legend). **Decision D-key:** either keep `[g]` as a Dashboard-scoped open (if
  verified free from the Dashboard) or pick another free global key (e.g. `[l]`). Resolve at eng review;
  state the chosen key in acceptance. (Do NOT assume the bare `[g]` of the old spec is still free.)
- **`make check-size` does NOT exist** (acceptance #6 + D-size). The 6 MB guard lives only in CI (`ci.yml`)
  and measures the RELEASE binary. Verify release size at build; drop the `make check-size` acceptance line
  or add the target — do not reference a non-existent target.
- **The 5 hand-rolled input sites, current `mod.rs` key-dispatch lines** (the STATE lives in the per-view
  modules per the "Files touched" list, but the `push(c)`/`pop()` KEY HANDLING is in `mod.rs` — both need
  the swap): memory search `:465`/`:468`, converse rail `:608`/`:611`, spawn task `:711`/`:714`, inspector
  search `:823`/`:827`, approvals reject-reason `:902`/`:905`.
- **Deps confirmed ABSENT** today (only `ratatui 0.29`): `tui-input`, `tui-textarea`, `color-eyre` all to
  add. **D3 (new):** confirm the `tui-input`/`tui-textarea` versions that support **ratatui 0.29** and pin
  them; if a version forces a ratatui major bump, FLAG it (bigger blast radius) — do NOT silently bump ratatui.
- **D2 subprocess model:** the old spec says `tokio::process` + an `AppEvent` pump. Confirm `agentctl watch`
  actually has a tokio runtime + an AppEvent pump today (the watch loop may be sync `reqwest::blocking` +
  crossterm poll — ux.3's inline-mutation precedent suggests a sync main loop). If the loop is sync, the
  compose-logs child must be read on a background THREAD → `mpsc` → drained in the crossterm poll tick, NOT
  a tokio task. Resolve the runtime model at eng review before building P1.

These corrections are the delta; the D1–D8 / B1–B3 / C substance above is otherwise intact.

---

## /autoplan eng consensus (2026-07-27) — reshape (both voices: BUILD with named changes)

Both eng voices confirmed the sync-loop grounding and reshaped the *how*. CONFIRMED-by-both corrections
(fold in at build):

- **A — subprocess model:** `std::process::Command` (piped stdout) read on a background `std::thread` →
  `try_send` a new `AppEvent::LogLine` into the existing `sync_channel` (mirror `pump.rs`'s `stream_once` +
  the `catch_unwind`→`ProducerDied` guard). **Strike D2's tokio body** — there is no tokio runtime.
- **A2 (HIGH, must-fix) — orphan-child leak:** `docker compose logs --follow` never EOFs, and the reader
  thread is never joined, so on exit the child ORPHANS. `Producers` must own the `Child` and
  `child.kill()` it in `Drop` (that EOFs the pipe + unblocks the reader). Acceptance + a manual "no orphan
  `docker compose logs` after quit" QA step.
- **A3 — tx wiring:** `spawn_producers` consumes the sender internally. Simplest: spawn the log producer
  EAGERLY at loop entry, gated on the startup docker-detect bool (Drop-kill from A2 handles teardown); OR
  return a `tx` clone. Pick eager-gated.
- **A — `--no-log-prefix` breaks service filtering:** keep the compose prefix (+ `--no-color`) so lines
  carry the service name to parse/filter; the plan's `{service,ts,text}` needs it.
- **A — batching:** `CHANNEL_CAP=256` + a 256-drain-per-tick cap will drop/starve under high log volume →
  `AppEvent::LogLines(Vec<..>)` batches + a visible "N dropped" accounting.
- **Key → `[l]`** (not `[g]`): `[g]` is technically free from the Dashboard but reusing the letter across
  views is a footgun; `[l]` is free everywhere. `g`/`G` stay as in-view top/bottom scroll.
- **B2 (HIGH) — dep pins, verified empirically against ratatui 0.29 / unicode-width 0.2.0:**
  `tui-input = { version = "0.14", features = ["ratatui-crossterm"] }` (0.15 needs ratatui 0.30 → conflict;
  0.10 drags a SECOND ratatui 0.28) and `tui-textarea = "0.7.0"`. **No ratatui bump needed** with these pins.
- **B — event plumbing:** the 5 sites handle bare `KeyCode`; tui-input consumes crossterm `Event`. Pass
  `KeyEvent`/`Event` to the focused-input handlers; intercept Enter/Esc/Tab (and rail Up/Down) BEFORE
  delegating, so per-view semantics survive.
- **B3 (HIGH) — spawn Enter/Tab:** today `Enter`=focus-advance and there is **no** `Ctrl-S`/`F5` binding
  (acceptance #4's "existing Ctrl-S/F5" is phantom — delete it). tui-textarea `Enter`=newline collides →
  intercept `Tab`/`Esc` before the textarea, `Enter`=newline in-field, submit stays `[r]`/ActionSpawn-Enter.
  Don't rely on `Ctrl-Enter` (indistinguishable from Enter without kitty-keyboard).
- **B5 — paste:** `Ctrl-V` is not clipboard; needs `EnableBracketedPaste` in `TermGuard::enter` + an
  `Event::Paste` arm routed to the focused input, or DROP acceptance #3's paste clause.
- **Size:** no `make check-size` — the 6 MB guard is CI-only (`ci.yml:77`) on the **musl release** binary
  (local native release ~2.6 MB; ~3 MB+ headroom, comfortable). Verify with `cargo build --release`.

**DISAGREE → recommend STRIKE — Sub-part C (color-eyre).** Claude (code-grounded, decisive): the stated
rationale is FALSE — `TermGuard` (mod.rs:171-187) already installs a chained, terminal-restoring panic hook
with a deliberate main-thread gate, and producer-thread panics are caught by `catch_unwind`→`ProducerDied`
(pump.rs:93-108) and never reach the process hook. So the "non-main-thread panic corrupts the terminal" gap
C claims to close does not exist. "eyre formatting throughout" is an unscoped `anyhow`→`eyre` migration, not
"pure cockpit polish." Codex said build-with-changes (compose via `HookBuilder::into_hooks()`), but did not
refute the redundancy. → **Recommend striking C** (or reducing to a bare `install()` before `TermGuard` for
prettier backtraces — marginal value). Do NOT migrate anyhow→eyre here.

**Scope observation:** A (Logs) is a pump-refactoring increment with a real orphan-leak correctness risk; B
(inputs) is contained; C is redundant. Eng-recommended order: **B → A** (A's `/` search then reuses B's
tui-input widget). Splitting A into its own increment is defensible given its blast radius. → gate decision.
