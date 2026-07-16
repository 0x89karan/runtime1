# ux.10 — TUI polish: log streaming + input ergonomics + ecosystem crates

**Track:** UX (operator cockpit)
**Branch:** `ux.10-tui-polish` (off `main` after ux.1/v0.86.0)
**Depends on:** ux.1 (converse rail, shipped v0.86.0), ux.9 (cockpit mode, shipped v0.82.0), p7.7 (management HTTP API, shipped v0.53.0)
**Status:** planning, 2026-07-16.

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
