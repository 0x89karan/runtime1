# Next-session prompt — ux.13-TUI, resume at build step 4

> **SPENT — do not paste this. ux.13-TUI shipped as v0.115.0 on 2026-07-28** (`CHANGELOG.md`,
> `docs/STATUS.md`). The build it resumes is finished, /review and /qa are done, and the numbers
> below are mid-branch snapshots: the workspace is at **1816 tests**, not 1728, and v0.115.0 is
> released rather than "next". Kept only as the record of where the build stood at the handoff.
> The plan of record is `docs/plans/ux.13-tui-verbs.md`.

Paste everything below the line into a fresh session started from the repo root.

---

Continue AgentOS development: keep building **ux.13-TUI** (row-scoped control verbs in
`agentctl watch`). Resume at **build step 4**.

## STATE

- Repo: `/Users/0x89karan/dev/GitHub/agentOS`. Read `CLAUDE.md` first.
- Branch: **`ux.13-tui-verbs`**, 5 commits, based on `main` at `f9785dea` (= v0.114.0).
  Nothing pushed. No PR yet.
- `main` is v0.114.0, tagged, and the ghcr.io images are published (`:latest`, `:core`,
  `:full`, `:v0.114.0*`). That release is fully closed — don't revisit it.
- Workspace: **1728 tests passing**, clippy clean. The one agentd failure you may see is the
  KNOWN flake `streaming_two_agents_populates_both_in_streamed_agents` — it fails only under
  parallel load and passes with `--test-threads=1`. Not a regression unless you touched
  scheduler streaming code.

## THE PLAN IS ALREADY LOCKED — DO NOT RE-AUTOPLAN

`docs/plans/ux.13-tui-verbs.md` is the authority. It carries a full 4-phase `/autoplan`
record (CEO, Design, Eng, DX — three phases with dual voices, **6/6 adverse consensus in
each**, final gate approved "as-is"). Read these sections before writing code:

1. **`## LOCKED BUILD ORDER`** (near the top) — the 11-item sequence, safety before features.
2. **`# PHASE 2 — DESIGN REVIEW`** — the four CRITICALs and the state enumeration.
3. **`# PHASE 3 — ENG REVIEW`** — E1-E12 plus the 21-row test plan with copy-templates and
   negative controls.

The increment was RESHAPED at the premise gate: it began as `ux.3b — : command palette`, and
the palette is **STRUCK** (borrowed multi-user pain over 10 compile-time views; lazygit/htop/
btop answer this shape with `?`, and k9s only needs `:` because its noun space is
runtime-discovered). What it became: wire ux.13's **deferred** TUI verbs. `docs/ROADMAP.md:1235`
records the gap — *"TUI keys deferred"* — and `cancel`/`set_budget`/`set_caps` have working
`DataSource` impls with **zero** call sites in any view. The operator cannot stop a runaway
from the screen showing it to them.

## DONE (step 1, commit `abdabc20`)

The four safety items, because each is a way the cockpit could crash on or lie to an operator
mid-incident:

- `agentctl/src/watch/overlay.rs` (new): `DashboardOverlay { target_id, mode }` — Dashboard-owned,
  not a global `App.overlay`; `overlay_rect` + `clamp_to_frame`; `overlay_fits`;
  `target(&agents)` resolve-at-use.
- The overlay owns the entire keyboard (unconditional early return at the top of
  `handle_dashboard_key`), `q` no longer quits from inside it (`overlay_was_open` captured
  BEFORE dispatch), `x` opens it pinned to the selected agent, and it RENDERS
  (`views::render_dashboard_overlay`, drawn last over the content area).
- 597 agentctl tests. All three fixes were negative-controlled by reverting them and confirming
  the test fails.

**Only `OverlayMode::Menu` exists.** The verb modes and `App.pending_verb` were deliberately
NOT committed — an unconstructed variant is dead code, not groundwork. They arrive with their
handlers.

## YOUR NEXT STEP — step 4: the Approvals render-by-pinned-id fix

This is the **highest-security item in the increment** and a bug in already-shipped code.
Independent of the verbs, so it lands on its own.

`render_approvals`' Confirm arm renders by INDEX (`views.rs`, `app.approvals_items.get(av.selected_idx)`)
while `handle_approvals_key` acts on the pinned `confirmed_id` (`mod.rs`), and
`update_approvals` replaces the list in Confirm mode, clamping only the index (`app.rs`). If an
item resolves out-of-band (Telegram, CLI, another approval) the dialog shows item B's
id/kind/**risk**/summary while `[a]` approves item A. Cancel is not a security boundary; the
approval gate is.

Fix: a `confirm_item(app: &App) -> Option<&PendingAction>` resolving from `confirmed_id`, used by
the RENDERER; an explicit "this approval was already resolved" body when `None` (the existing
`unwrap_or(("?","?","?","?","?"))` fallback becomes that case, so it is 3-4 lines, not 2). The
`"already resolved"` branch in `handle_approvals_key` has **no test today** — write both
directions.

Then continue down the locked order: 5 (`pending_verb` + the two-phase drain), 6 (`park_limit`
+ `budget_resettable`), 7 (server cascade count + cycle-safe `descendants`), 8
(`confirms_mutations` + honest FUSE tense), 9 (`cancelling…` marker), 10 (footer clip +
`?` overlay), 11 (the 5 rewritten error strings + `Equivalent: agentctl …` hints).

## TRAPS THE REVIEWS ALREADY FOUND — do not rediscover these

- **`token_budget == 0` means UNLIMITED** (`scheduler.rs`, `agent/mod.rs`) and
  `set_token_budget` writes the **checkpointed** `cfg.token_budget`. `Park =
  set_budget(windowed_spent)` on an agent with 0 recorded spend therefore **un-caps the runaway
  permanently, across restart** — in Park's primary use case. `park_limit()` must return
  `Option<u64>` and `None` at 0.
- **Park is only reversible when `budget_reset_interval > 0`** (default `0`; only the CoS configs
  set it). Otherwise exhaustion calls `handle_agent_terminal` — a kill. The cockpit cannot see
  that field, so expose `budget_resettable` (the scheduler already computes it) or drop the word
  "reversible" from the UI.
- **Every overlay-vs-quit assertion must go through `step_key`**, never `handle_dashboard_key` —
  the latter bypasses the `Effect::Quit` push, so such a test passes with the fix reverted.
- **`Effect` is `Copy` and payload-free** and `apply_effects` has no `source` param, so it cannot
  perform a verb call. Use the `App.pending_verb` slot (`spawn_view.pending_exec` precedent), and
  `.take()` BEFORE the call or an early return re-fires it every 30 ms — a cancel storm.
- **The drain goes after the SHUTDOWN check and before `event::poll`.** After `poll`, a keystroke
  queued during the freeze is dispatched first, so a second Enter arms a second cancel and a `q`
  exits the loop while the footer claims the verb is in flight.
- **"Anchor the overlay away from the selected row" is NOT implementable** — rows have variable
  height and the table renders stateless (no `TableState`), so the selected row's screen `y` is
  unknowable. Render the target's live row INSIDE the box instead (already done).
- **`tui_input::Input` has no `PartialEq`/`Copy`** — match on `OverlayMode::kind()`, don't write
  `assert_eq!` against a mode.
- **Run the negative controls, don't assume them.** Doing so on step 1 caught a vacuous test of
  mine: `overlay_rect_never_escapes_the_frame` passed with the clamp removed, because the
  arithmetic is independently escape-proof.

## WORKFLOW (standing, per the user)

`/autoplan` → build → `/review` → `/qa` → `/ship` → `/land-and-deploy`, each an explicit skill
call, no substitutes. **`/autoplan` is already satisfied for this increment** — do not re-run it.

- Gate before every commit, workspace-wide from the repo root:
  `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
  **Never run `cargo fmt`.**
- To drive the TUI, use the project skill **`/run-agentctl-watch`**
  (`.claude/skills/run-agentctl-watch/`): a pty driver plus a fake `docker`. A TUI needs a real
  pty AND a window size — piping into it renders an empty frame that makes every assertion pass
  vacuously.
- `/qa` is a browser skill; this repo's only web surface is agentd's management API on `:7999`
  (boot it from `docker/cockpit.toml` with the `/data` paths rewritten to a tmpdir; no real API
  key needed). For TUI work drive `/run-agentctl-watch` as well.
- **The user gates ALL merges and tags.** Never push a `v*` tag without an explicit instruction.
- The release commit bumps `agentd/Cargo.toml` + the `**Current version:**` line in `CLAUDE.md`
  (test-enforced by `repo_consistency`) + `CHANGELOG.md`. Next version is **v0.115.0**.

## AFTER THIS INCREMENT

`docs/plans/ux.13-tui-verbs.md`'s CEO sequencing section ranks what follows: the
`converse::dispatch()` ~8 s TUI freeze (`TODOS.md`, P2 — closed as a side effect if step 5
migrates chat onto the same `pending_verb` slot), then the footer clip + `?`, then **ux.6
evidence** (the only queue item serving two products). Also open: `audit86-P1-9` needs a
standalone 20-minute scope decision — are inert wrong-tier capability grants the intended
declare-then-lint design, or a gap? It is now the ONLY live P1 in `TODOS.md`; four others were
struck as already-shipped in commit `1d0256ff`.
