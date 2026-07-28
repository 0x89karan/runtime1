<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/ux.3b-command-palette-autoplan-restore-20260728-114059.md -->
# ux.13-TUI — row-scoped control verbs (Cancel / SetBudget / SetCaps) + confirm overlay

**Track:** UX (operator cockpit)
**Branch:** `ux.13-tui-verbs` (off `main` at v0.114.0)
**Depends on:** ux.13 (verbs + ControlCommand + DataSource methods, v0.97.0), ux.10-B (tui-input, v0.113.0)
**Status:** ✅ **/autoplan COMPLETE + APPROVED 2026-07-28** (all 4 phases, 3 dual-voice; final gate
"approve as-is"). Ready to build in the locked order below. Was `ux.3b — : command palette`; the
palette is **STRUCK**.

## LOCKED BUILD ORDER (safety before features — approved at the final gate)

| # | Item | Closes |
|---|------|--------|
| 1 | `overlay_rect` clamp + `is_empty()` fallback + 1×1/10×3 tests | E3 (render-thread panic) |
| 2 | `q`-quit gate (`!overlay_was_open`, captured pre-dispatch) + overlay owns the whole keyboard | C3, E4 |
| 3 | `Overlay { target_id }` pinned at open + `overlay_target()` resolve-at-use | C1, E5 |
| 4 | Approvals Confirm renders from the pinned id (+ honest "already resolved") | C2, E12 |
| 5 | `App.pending_verb` + `drain_pending_verb` after the shutdown check, before `event::poll`; `.take()` first | H1, E8 |
| 6 | `park_limit() -> Option<u64>` (None at 0) + `budget_resettable` in the snapshot; gate/relabel Park | E1, E2 |
| 7 | Cancel: report the SERVER's count; cycle-safe `descendants()`; preview labelled "at least N" | E6, E7 |
| 8 | `confirms_mutations()` (default false, HTTP true) + honest FUSE tense | C5, E9 |
| 9 | client-side `pending_cancel` → `cancelling…` row marker | M8 |
| 10 | footer clip (assert `len() <= 114`) + `?` help overlay | V3, H2 |
| 11 | the 5 rewritten error strings + `Equivalent: agentctl …` hint in every overlay/result | DX |

Taste decisions taken at the gate: Park **stays** (disabled at zero spend, gated on
`budget_resettable`); `converse::dispatch()` **migrates** onto the same `pending_verb` slot (closes the
ranked P2 at TODOS.md:457); `?` **stays in scope**; the Approvals fix **rides along** (it is the
highest-security item here). Was
`ux.3b — : command palette`; the palette is **STRUCK**. Full CEO record below.

---

## What this is now

`ux.13` shipped `ControlCommand::{Cancel, SetBudget, SetCaps}` end to end — management API, FUSE
control, `agentctl` CLI subcommands, and `DataSource` trait methods with working HTTP + FUSE impls
(`watch/source.rs:64-75`, impls `:124-138`). **No view invokes any of them.** `docs/ROADMAP.md:1235`
records the gap: *"**TUI keys deferred** (CLI/HTTP/FUSE cover the surface — a convenience
follow-up)."*

So the operator cannot stop a runaway agent from the screen that is showing it to them. That is
friction #1 of the approved design doc, and the track's north star is *"k9s's defining trait is that
you can act on what you see."* This increment wires the last mile.

## Scope

**V1 — row-scoped verbs on the selected Dashboard agent.**

| Key | Verb | Flow | DataSource |
|---|---|---|---|
| `x` | Cancel | confirm overlay (agent id + "this cannot be undone") -> `cancel()` | HTTP + FUSE |
| `b` | SetBudget | numeric `tui-input` in the overlay -> `set_budget()` | HTTP + FUSE |
| `C` | SetCaps | revoke/narrow only (pick from the agent's current caps) -> `set_caps()` | HTTP + FUSE |

**V2 — the modal overlay subsystem.** `Clear` over a centred `Rect` on top of the live view, so the
dashboard stays visible behind the prompt. This is the half of ux.3b that survived: the verbs are the
consumer that justifies it. Reuses the Approvals view's confirm idiom (`ApprovalsMode::Confirm`) and
ux.10-B's `tui-input` for the numeric field.

**V3 — the footer clip fix** (measured defect, in blast radius, 30 min). Corrected numbers from the
Eng phase: narrow+`[l]ogs` = **162 cols**, wide+logs = **148**. The sharper evidence: in the
narrow variant `q quit` starts at **column 114** and `(resize to 115+ cols…)` at **column 122** — and
that branch only renders when width < 115, so **the resize hint is invisible at every width where its
own branch is active**. Acceptance must assert a WIDTH (`len() <= 114`), not `contains("q quit")`,
which passes with the bug intact.

## Key choices to settle in Design/Eng review

1. Key letters: `x` for cancel (lazygit/k9s idiom) vs `d` (delete) vs `Ctrl-c` (taken by quit). `b`
   and `C` are free on the Dashboard today; `c` is taken by Credentials.
2. Confirm strength: single confirm for Cancel, or type-the-id for a destructive verb?
3. Where overlay state lives: a new `App.overlay: Option<Overlay>` vs per-view modes like
   `ApprovalsMode`.
4. DataSource gating per verb — mandatory, not optional. The ux.1 chat rail shipped silently
   non-functional over FUSE (`ROADMAP.md:1158`); every affordance must check the source first.
5. SetCaps UX: narrowing means presenting the agent's CURRENT caps to deselect from. Does the
   snapshot carry per-agent capabilities today, or is that a plumbing add?

## NOT in scope

- The `:` command palette, a command registry, fuzzy matching (struck — see CEO record).
- `publish_brief` as an operator verb (it is a capability-gated agent tool).
- Pause/resume (deferred by the design doc).
- Live capability GRANT (revoke/narrow only — cf. audit86-P1-10).
- `audit86-P1-9` (inert wrong-tier grants) — standalone decision, wrong blast radius here.
- A `?` help overlay — worth doing, but a separate afternoon; this increment's overlay makes it
  nearly free afterwards.

# PHASE 2 — DESIGN REVIEW (/autoplan, 2026-07-28)

## Dual voices — consensus

```
DESIGN DUAL VOICES — CONSENSUS TABLE
═══════════════════════════════════════════════════════════════════════
  Dimension                              Claude   Codex   Consensus
  ────────────────────────────────────── ──────── ─────── ───────────
  1. Target identity safe?                NO       NO     CONFIRMED
  2. States specified?                    NO (6/18) NO    CONFIRMED
  3. Destructive-action weight right?     NO       PARTLY  CONFIRMED
  4. Key choice sound?                    PARTLY   PARTLY  CONFIRMED
  5. Overlay layout specified?            NO       NO     CONFIRMED
  6. SetCaps buildable?                   NO       NO     CONFIRMED
═══════════════════════════════════════════════════════════════════════
Both: CUT SetCaps from V1; PIN the target at overlay-open; specify the states.
Claude adds 4 CRITICALs, two of which are bugs in ALREADY-SHIPPED code.
Ratings (Claude): hierarchy 3/10 · states 2/10 · destructive-safety 3/10 ·
keys 5/10 · overlay 3/10 · SetCaps 1/10.
```

## CRITICALs (all code-verified)

**C1 — the target retargets under the operator's finger.** `apply_snapshot` clears a vanished
selection then **auto-selects row 0** (`app.rs:589-618`; test `apply_snapshot_clears_selection_when_agent_disappears`
at `:822` asserts exactly this). Snapshots fold every ~30 ms from a producer thread, independent of
keystrokes. Sequence: select runaway `scout-2` → press `x` → `scout-2` self-terminates in the 400 ms
before the second keypress → selection silently becomes `cos-coordinator` → Enter **cancels the
coordinator, cascading to its children**. FIX: `Overlay { verb, target_id }` captured at open; render
from the pinned id via `agents.iter().find(...)`, never `selected_agent()`; explicit `Vanished` state
(auto-close lets the queued Enter fall through to the table).

**C2 — the idiom we planned to copy already has this bug.** `render_approvals`' Confirm arm renders by
INDEX (`views.rs:1682`) while `handle_approvals_key` acts by pinned `confirmed_id` (`mod.rs:1118-1121`),
and `update_approvals` replaces the list in Confirm mode, clamping only the index (`app.rs:686-693`).
If the list reorders while the dialog is up, **it displays one item and approves another.** The pin
protects the write; nothing protects the display. FIX: render from the pinned id in both places —
2-line drive-by fix in this branch, since it is the reference implementation the new code copies.

**C3 — `q` inside a Dashboard overlay quits the cockpit.** `step_key` appends `Effect::Quit` for
`Char('q')` whenever the pre-dispatch view was Dashboard and the rail was unfocused (`mod.rs:583`) —
and the app TEACHES `q` as dismiss (`ApprovalsMode::Confirm` binds `Esc | Char('q')`, `mod.rs:1160`).
An operator who learned that loses the cockpit mid-incident. Same class as the ux.1 rail bug that put
`rail_was_focused` there. FIX: extend the guard with `&& !overlay_was_open`, captured BEFORE dispatch;
bind `q` as dismiss inside the overlay; unit-test beside `step_key_q_while_rail_focused_types_not_quits`.

**C4 — Cancel cascades to the whole subtree; the confirm shows one id.** `ControlCommand::Cancel`
walks `parent_map` and flags every native descendant plus universal agents parented into the subtree
(`scheduler.rs:2800-2872`); the route returns `count`. On this repo's own fixture
(`coordinator-demo.agents.toml`) `x` on the coordinator kills **three** agents. The operator confirms
a blast radius nobody showed them. Data is already client-side (`AgentInfo.parent_id` `reader.rs:173`,
`App.topology` rebuilt per tick). FIX: list descendants at open —
`Cancel cos-coordinator → also cancels scout-1, scout-2 (3 agents)`.

**C5 — over FUSE the success message is fabricated, and decision #4 has no mechanism.**
`FuseSource::{cancel,set_budget,set_caps}` write through `write_control_command`, whose only error
signal is `close(2)`; the dispatch closure returns 0 once the command is QUEUED (`main.rs:1018-1027`).
Everything the scheduler actually decides arrives with `confirm_tx: None` and lands in the flight
recorder as `FuseControlError` (`scheduler.rs:2786-2797`). So over FUSE, `"agent not found"`,
`"SetCaps is narrow-only"` and `"capability is inert"` **all return `Ok(())` to the TUI** — a footer
reading "Cancelled cos-inbox" would be a lie. And "DataSource gating per verb" has no mechanism: both
sources implement all three verbs, and the trait's only introspection is `event_stream_url()`.
FIX: add `fn confirms_mutations(&self) -> bool` (false for FUSE, true for HTTP); past tense ONLY when
the source confirms, else `"Cancel requested — watching <id>"`; state in the plan that FUSE gets a
strictly weaker product — a decision, not an accident.

## HIGH

- **H1 — "in-flight" is architecturally unrepresentable as designed.** `source.cancel()` runs on the
  RENDER thread (`step` → `step_key` → `handle_dashboard_key`) and `HttpSource`'s confirm client has a
  3 s timeout, so Enter freezes the whole cockpit with no frame drawn; no spinner is possible from
  inside `send()`. FIX: two-phase — the confirm keypress sets `Overlay::InFlight` + `Effect::Redraw`
  WITHOUT calling the source; the loop draws; the next iteration drains a `pending_verb` slot and does
  the blocking call. ~20 lines in `run_tui_loop`, and it generalises to the `converse::dispatch` P2
  (TODOS.md:457).
- **H3 — the `x` precedent is factually inverted.** lazygit binds `x` to OPEN THE ROW-ACTION MENU
  (delete is `d`); k9s uses `Ctrl-D`/`Ctrl-K` behind a dialog with explicit Cancel; lazydocker `x` =
  action menu; htop `k` = signal PICKER. The convergent idiom is **one key → graded menu → confirm the
  destructive item**. RESHAPE: `x` opens a row-action overlay; three verbs behind one key; the
  irreversible action is the one you travel to.
- **H4 — a reversible soft-stop already exists and goes unoffered.** `SetBudget` parks an agent when
  `limit <= windowed_spent` (`scheduler.rs:2762`), and raising it later revives via `drain_deferred`
  (`:2775-2779`). One call, reversible — what an operator watching spend climb at 2 am wants FIRST.
  The plan made the irreversible option the fast one. FIX: **Park** as a first-class menu item ranked
  above Cancel. Highest value-per-line item in the review.
- **H5 — SetCaps has no data behind it (both voices).** Not in `AgentSnapshot` (`snapshot.rs:253-267`),
  not in `AgentInfo` (`reader.rs:161-186`), no `capabilities` FUSE file (`agents_fs.rs:285-291`
  offsets). And `SetCaps` REPLACES the whole set with a coverage check (`scheduler.rs:2886-2905`), so
  revoking one cap means transmitting all the others; `capabilities_unrestricted` agents have no list
  at all. **CUT from V1.**
- **H2 — the footer cannot absorb three keys.** Measured 148/162 cols today; `+ x cancel b budget C caps`
  → 176/190, so the brand-new keys clip off every real terminal — immediately after striking the palette
  ON discoverability grounds. FIX: one key (H3), row verbs FIRST in the string (precedent: `r` leads),
  and pull `?` in-scope (the plan already concedes it is nearly free once the overlay exists).

## MEDIUM (carried into the build)

M2 `0` means UNLIMITED in both the trait doc and the scheduler — a cleared field + `0` REMOVES the cap
(inverse of intent): prefill current limit, render `0 = unlimited`, second confirm for `0` or any raise.
M3 no result channel on the Dashboard (`spawn_banner` dies on the next keypress and adds a chrome row
that can push the rail below its fit floor) → render into footer row 2 like `render_approvals`.
M4 overlay geometry has no floor (`Clear` is unused in the tree today), hides the very row it is about,
and is absent from `on_resize`: fixed `Rect` + `overlay_fits(w,h)` in the `converse_rail_fits` idiom,
render the target's live row INSIDE the box, anchor away from the selected row.
M5 401/403 (cap.4/ux.12 `X-Approval-Token`) needs its own copy, detected at startup.
M6 overlay key routing must sit at the top of `handle_dashboard_key` AHEAD of the `rail_focused` early
return, intercepting Tab/Enter/Esc/q, plus a `route_paste` arm or a pasted budget silently vanishes.
M7 `C` collides with terminal copy: under the kitty protocol `Ctrl+Shift+C` reports `Char('C')` +
`CTRL|SHIFT`, which misses the `ctrl_c` guard and hits a bare `Char('C')` arm — terminal copy would
revoke capabilities. Fold into the `x` menu.
M8 there is no `Cancelling` state anywhere (`cancel_requested` is scheduler-private; no
`AgentStatus::Cancelling`), so a successfully-cancelled row reads `running` for a whole turn then
vanishes → client-side `pending_cancel` map rendered as `cancelling…`, cleared on row disappearance or
the `agent_cancelled` flight event, escalating to `cancel not confirmed` after ~60 s.

## V1 as reshaped by design review

`x` opens a **row-action overlay** pinned to `target_id`, showing the target's live row and the
cancel cascade:

| Item | Semantics | Reversible |
|---|---|---|
| **Park** | `set_budget(limit = windowed_spent)` — parks at next boundary | ✅ raise the limit to revive |
| **Set budget** | numeric `tui-input`, prefilled, `0` guarded | ✅ |
| **Cancel** | Enter-on-item + confirm; shows the subtree count | ❌ |

Cut: SetCaps (own snapshot-plumbing increment). Added: `?` help overlay (nearly free once the overlay
exists, and the honest counterpart to striking the palette). Kept: the footer clip fix.

# PHASE 3 — ENG REVIEW (/autoplan, 2026-07-28)

```
ENG DUAL VOICES — CONSENSUS TABLE
═══════════════════════════════════════════════════════════════════════
  Dimension                              Claude   Codex   Consensus
  ────────────────────────────────────── ──────── ─────── ───────────
  1. Architecture sound?                  NO       NO     CONFIRMED
  2. Test coverage sufficient?            NO       NO     CONFIRMED
  3. Performance/liveness risks handled?  NO       NO     CONFIRMED
  4. Security boundaries covered?         N/A*     PARTLY  CONFIRMED
  5. Error paths handled?                 NO       NO     CONFIRMED
  6. Deployment risk manageable?          YES      YES    CONFIRMED
═══════════════════════════════════════════════════════════════════════
*operator authority is a non-issue by locked decision #2 — /spawn already MINTS
 capabilities (management.rs:501-509), strictly more authority than cancelling.
Both verdicts: RESHAPE (fix E1-E3 and re-spec M4/M6 before build).
```

## CRITICALs

**E1 — Park REMOVES the budget at zero spend, and the change is checkpointed.** `Park =
set_budget(limit = windowed_spent)`, but `windowed_spent` is `0` for any agent that has not completed
a turn — *exactly* when an operator reaches for a stop. And `limit == 0` means **unlimited**
everywhere (`scheduler.rs:2762`, `:1820`, `agent/mod.rs:242`), while `set_token_budget` writes the
CHECKPOINTED `cfg.token_budget` (`agent/mod.rs:239-248`). So the design phase's "highest
value-per-line" primitive un-caps a runaway **permanently, across restart**, in its primary use case.
M2's guard only covers the TYPED field. FIX: pure `park_limit(windowed_spent) -> Option<u64>`
returning `None` for `0` (and below a floor, since the next turn's tokens land before the gate
re-runs); `None` ⇒ the Park item renders DISABLED with "no spend recorded yet — use Cancel or set a
budget". `0` must never reach `set_budget` from the Park path.

**E2 — Park's advertised reversibility depends on a config field the cockpit cannot see.** Per-agent
exhaustion **defers** only `if budget_reset_interval > 0` (`scheduler.rs:1850-1870`); otherwise it
falls into `handle_agent_terminal(… "admission denied: agent_budget_exhausted")` — a **kill**. Default
is `0` (`config.rs:457`); only the CoS configs set `86400`. Nothing in `Snapshot`/`AgentInfo`/
`SysBudget` carries it. So on a plain `agentd agent.toml`, **Park IS Cancel, mislabelled as safe.**
FIX: expose `budget_resettable: bool` (the scheduler already computes exactly this at
`scheduler.rs:1545`) and gate/relabel the item — ~6 lines — or drop the word "reversible" entirely.

**E3 — `Clear` is the one widget in the tree that panics on an out-of-frame `Rect`.** ratatui
`clear.rs:42-48` indexes `buf[(x,y)]` with **no** `intersection(buf.area)`; `Buffer::index_of` panics
"index outside of buffer". `Block::render_ref` DOES intersect (`block.rs:703`), which is why every
existing view survives sloppy geometry. `Clear` is unused in this repo today, so V2 introduces the
first widget that can kill the render thread from arithmetic — and a render panic exits the cockpit
mid-incident with the runaway still running. FIX: `overlay_rect(f.area()).intersection(f.area())` +
`is_empty()` footer-only fallback; unit-test `overlay_rect` at 1×1 / 10×3 / 200×50 plus one
`TestBackend` render at 10×3 (`TestBackend` ships in ratatui 0.29 — no new dependency).

## HIGH

- **E4 — the overlay must own the WHOLE keyboard, not intercept four keys (corrects M6).** Anything
  not intercepted falls through to the nav arms (`mod.rs:817-854`): `s`/`t`/`m`/`n`/`a`/`c`/`i`/`l`
  change `app.view` while `overlay` stays `Some`, and `step_key` dispatches on `app.view`
  (`mod.rs:556`) — so the next key goes to a different view's handler with a modal drawn over it;
  `j`/`k` desync the highlight from the pinned target. FIX: unconditional early return at the top of
  `handle_dashboard_key`, the idiom already used twice (`logs_view.search_active` `mod.rs:870-885`,
  `SpawnFocus::TaskField` `:917-926`). Unmapped keys inside the overlay are NO-OPS, not fall-through.
- **E5 — drop C1's `Vanished` state; copy the approvals resolve-at-use idiom.** `apply_snapshot`
  (`app.rs:586-618`) knows nothing about overlays and should not. The repo already solved this without
  a state: `approvals_items.iter().find(|i| i.id == id)` with an honest else-branch (`mod.rs:1083`,
  `:1096`). FIX: `Overlay` carries only `target_id` + cursor/input; one
  `overlay_target(&App) -> Option<&AgentInfo>` used by BOTH renderer and key handler; `None` renders
  "target no longer present" and makes confirm a no-op. Removes a state, removes the queued-Enter
  hazard, removes an `app.rs` coupling.
- **E6 — report the SERVER's cascade count; the client preview is a floor.** The route already returns
  it (`management.rs:667`, `count` = native subtree + universal agents parented in,
  `scheduler.rs:2864-2873`) and `HttpSource::cancel` **discards the body** (`source.rs:194-196`).
  The client walk uses a snapshot up to 1 s stale with no universal-tier parentage, so the numbers can
  legitimately differ. FIX: `cancel(&self, id) -> Result<u64, String>`; label the pre-confirm preview
  "at least N".
- **E7 — the descendant walk must be cycle-safe.** A `parent_id` cycle is a TESTED reality here
  (`topology.rs:386-391` exists because it happened); a naive frontier walk hangs the render thread.
  FIX: copy the scheduler's guard verbatim (`&& !subtree.contains(child)`, `scheduler.rs:2819`) in a
  pure `descendants(&TopologyGraph, &str)`, with the cycle fixture as a test.

## MEDIUM (mechanism — both voices agree, Codex's placement analysis + Claude's constraints)

**E8 — two-phase drain placement, and NO new `Effect` variant.** `run_tui_loop` order is
① SHUTDOWN check ② `drain_events` ③ `check_dispatch_timeouts` ④ `event::poll` → `step`
⑤ `if dirty { term.draw }`. The drain goes **after ① and before ④** (both voices; Codex prefers after
②/③, Claude notes before ② is marginally better since the InFlight frame is already flushed).
Failure modes if misplaced: **after ④** → a keystroke queued during the freeze is dispatched first, so
a second Enter arms a second verb (double cancel) and a `q` returns `Effect::Quit` so the loop exits
and the cancel **never sends** while the footer claims it is in flight; **before ①** → SIGTERM/Ctrl-C
is delayed by a 3 s confirm call; **from the key handler** → the spinner never renders.
No new `Effect`: it is `#[derive(Copy)]`, payload-free, and `apply_effects` has no `source` param, so
it *cannot* perform the call. Use `App.pending_verb: Option<PendingVerb>` — the established
"key handler stores work, loop performs it" precedent (`spawn_view.pending_exec`). Two musts:
`.take()` BEFORE the call (else an early return re-fires every 30 ms — a cancel storm), and extract
`drain_pending_verb(&mut App, &dyn DataSource) -> Vec<Effect>` because `run_tui_loop` needs a real
`Terminal` and has zero coverage (the reason `on_resize` was extracted). Return `Effect::Reconcile` on
success.

**E9 — `confirms_mutations()` is the right size today; default `false`; the doubles are the trap.**
Accurate as whole-source *for these three verbs* (FUSE: all via `write_control_command` with
`confirm_tx: None`; HTTP: all via `post_confirm`), but already per-verb for others
(`approve_with_kind` works on FUSE, degrades on HTTP). Default `false` keeps all five test doubles
compiling (`TestSource`, `ResolvesDifferentIdSource`, `FuseSpawnGuard`, `MockSource`,
`FuseLikeSource`) — and that silence IS the hazard: two of them already override `event_stream_url()`
to look like HTTP. Override to `true` on those two in the same commit and assert both real impls.

**E10 — M5's startup auth probe is not implementable.** The TUI already sends `X-Approval-Token`
(`source.rs:189-191`, read once at construction `:176`), and there is **no unauthenticated way** to
learn whether agentd has a secret — the gate list is exactly the mutating routes
(`management.rs:136-140`); `/healthz` and `/snapshot` reveal nothing. FIX: drop the probe; classify the
response with a pure `explain_verb_error(&str)` mapping 401/403 → "agentd requires an approval token —
set AGENTOS_APPROVAL_SECRET and restart agentctl watch", 503 → retryable.

**E11 — operator authority is a NON-ISSUE; say so in the plan so nobody "hardens" it later.**
Single-tenant (locked decision #2), and the cockpit already exposes `/spawn`, which MINTS capabilities
— strictly more authority than cancelling. Two asymmetries worth a line each: over FUSE the verb is
unauthenticated by construction (cap.4's deliberate design — `:7999` is the gated surface), and Cancel
on a universal-tier root flags the subprocess set instead of the native funnel, so count semantics
differ by tier.

**E12 — C2 (the approvals display/act split) is the highest-SECURITY item here, not a drive-by.**
Cancel is not a security boundary; the approval gate is. If an item resolves out-of-band
(Telegram/CLI/another approval) the dialog shows item B's id/kind/**risk**/summary while `[a]` approves
item A. The `unwrap_or(("?","?","?","?","?"))` fallback becomes the "already resolved" case and must
READ as that — 3-4 lines, not 2. The `"already resolved"` branch (`mod.rs:1096`) has **no test today**;
write both directions.

## What looks simple but isn't

- **"Anchor the overlay away from the selected row" (M4) is NOT implementable as written.** Dashboard
  rows have VARIABLE height (1 or 2 depending on attention signals, `views.rs:375`) and the table is
  rendered **stateless** (`f.render_widget(table, …)`, `views.rs:409` — no `TableState`), so there is
  no scroll offset and no way to know the selected row's screen `y`; with more agents than rows it may
  be off-screen. KEEP the other half (render the target's live row INSIDE the box); DROP the anchoring,
  or accept a `render_stateful_widget` migration as separate scope.
- **This leaves two contradictory I/O idioms in one function:** verb I/O deferred to the loop while the
  Enter arm still calls `converse::dispatch()` inline for ~8 s (`mod.rs:690`, TODOS.md:457). Either
  migrate chat onto the same `pending_verb` slot (+~40 lines, closes a ranked P2) or leave a
  load-bearing comment — otherwise the next contributor copies whichever arm they read first.
- **`?` is NOT "nearly free"** — same clamped geometry, same keyboard-ownership return, own frame, and
  the help text is a second copy of the key list that will drift from the footer unless both render
  from one table.
- **The 3 s stall lets the 256-slot channel fill**, so `LogLinesDropped` will tick during a cancel.
  Cosmetically alarming; worth a note.

## Test plan (21 rows, with copy-templates and negative controls)

Written to `~/.gstack/projects/0x89karan-runtime1/` as the test-plan artifact. Highlights of the
negative-control discipline this repo has repeatedly needed:

- **Every overlay-vs-quit assertion must go through `step_key`**, not `handle_dashboard_key` — the
  latter bypasses the `Quit` push (`mod.rs:583`) and the `spawn_banner` clear, so a
  `handle_dashboard_key`-level test **passes with E4/C3 reverted**.
- Pinned-target control: fold a snapshot that retargets `selected_id`, then assert
  `selected_id != overlay.target_id`. A test that never folds a snapshot passes with `selected_agent()`.
- `park_limit(0) == None` asserted explicitly — `park_limit(500) == Some(500)` alone passes with the
  E1 footgun intact.
- Drain-once: loop the drain 3× with an `AtomicUsize` double and assert count == 1; a single-iteration
  test passes with a re-arming bug.
- `overlay_rect` tiny cases only — a 200×50 test passes with unclamped arithmetic; only 1×1 / 10×3
  catches the `Clear` panic.
- Footer: assert `len() <= 114` on an extracted `dashboard_hints()`; `contains("q quit")` passes with
  the clip bug.
- Result copy: assert BOTH sources — a single double inherits the `false` default and passes with the
  gate reverted.
- One pty E2E via `.claude/skills/run-agentctl-watch/driver.py` (`--step key:x --step snap:overlay`) —
  the only test that can catch "the InFlight frame never flushed before the blocking call".

# PHASE 3.5 — DX REVIEW (/autoplan, 2026-07-28)

Voices: Codex (full pass) + primary. Stated honestly: this phase ran ONE independent voice, not two —
the earlier phases' subagents consumed the session's budget. Tagged `[codex + primary]`, not
`[codex+subagent]`. **DX score: 7/10** — learnable flow, error copy and CLI/TUI vocabulary need work.

- **Time to first cancel: 4 keystrokes** with the row already selected (`x` → move to Cancel → Enter →
  confirm Enter). Discovery is footer-first, then the overlay teaches the verbs. Without `?`, the
  source-mode behaviour (what FUSE can and cannot confirm) stays undiscoverable without reading source.
- **Error copy: 5 of 5 proposed strings fail WHAT/WHY/WHAT-TO-DO.** Replacements to adopt verbatim:
  - target gone → `No action sent: <id> is no longer in the snapshot. It may have finished or been removed; dismiss and select another running agent.`
  - 401/403 → `Action refused: approval token missing or wrong (HTTP 401/403). Export the same AGENTOS_APPROVAL_SECRET used by agentd, then restart agentctl watch.`
  - 503 → `Action not sent: agentd control channel is unavailable or busy (HTTP 503: <detail>). Wait a second and retry; if it persists, restart agentd.`
  - FUSE queued → `Request queued over FUSE; this path cannot confirm the scheduler accepted it. Watch <id>, or check Inspector for fuse_control_error.`
  - budget 0 → `Park unavailable: <id> has 0 recorded window spend, and budget 0 means unlimited. Use Cancel or set a positive budget.`
- **CLI/TUI mental-model drift.** `agentctl cancel` prints `cancel requested … next step boundary`; the
  TUI must MIRROR that tense, not say "cancelled" (independently confirms the design phase's honest-tense
  finding). `verbs.rs` has NO interactive confirmation, so the TUI adds a step the CLI lacks — fine, but
  the copy must say so. **`Park` is a TUI-only alias** over `set-budget` and will create a hidden second
  model unless the overlay names its equivalent.
- **Docs debt when this ships:** README's `agentctl` command list is stale (no cancel/set-budget/set-caps);
  DEPLOYMENT shows curl where it should show `agentctl set-budget` + `[x]`, and its
  `AGENTOS_APPROVAL_SECRET` text implies the gate is only approve/deny; CLAUDE.md's "next is ux.3b `:`
  palette" line must change; and **`docs/ROADMAP.md:1235` actively claims "TUI keys deferred"** — that
  line is the thing this increment falsifies.
- **Highest-value DX idea nobody had named: print the equivalent CLI command in every overlay and result
  line.** `Equivalent: agentctl cancel scout-2`. It teaches the fallback path for when the TUI is the
  thing that is broken, collapses the two mental models into one, and makes incident notes copy-pasteable.
  Cheap, and it is the single best line in this phase.

---

# PHASE 1 — CEO REVIEW (/autoplan, 2026-07-28)

## Dual voices — consensus table

```
CEO DUAL VOICES — CONSENSUS TABLE
═══════════════════════════════════════════════════════════════════════
  Dimension                              Claude   Codex   Consensus
  ────────────────────────────────────── ──────── ─────── ───────────
  1. Premises valid?                     NO       NO      CONFIRMED
  2. Right problem to solve?              NO       NO      CONFIRMED
  3. Scope calibration correct?           NO       NO      CONFIRMED
  4. Alternatives sufficiently explored?  NO       NO      CONFIRMED
  5. Competitive/market risks covered?    NO       NO      CONFIRMED
  6. 6-month trajectory sound?            NO       NO      CONFIRMED
═══════════════════════════════════════════════════════════════════════
Claude verdict: RESHAPE — palette STRUCK, overlay half redirected to ux.13's
                deferred TUI verbs.
Codex verdict:  DEFER — assumed discoverability; direct verbs or ux.6 win on
                value per unit effort.
Consensus: 6/6 CONFIRMED against the plan as written. Zero disagreements.
```

Six-for-six against is the strongest adverse consensus this repo's autoplan has produced.
The two voices differ only in remedy (reshape vs defer), not in diagnosis.

## 0A — Premise challenge

| Premise (stated or implied) | Verdict | Evidence |
|---|---|---|
| P1 — discoverability of ~11 keys is a real operator pain | **FALSE (borrowed)** | Single-tenant cockpit, one user, who wrote the keys. CLAUDE.md locked decision #2. |
| P2 — the footer is the only discovery mechanism and it truncates | **HALF-TRUE, and the true half is a 30-min bug** | measured (CORRECTED in Phase 3 — my Phase-1 numbers were wrong): narrow **154**, narrow+`[l]ogs` **162**; wide **140**, wide+logs **148**. The narrow variant is 14 cols LONGER, so below 115 cols the operator loses the tail incl. `q quit`. `views.rs:426-432`. |
| P3 — "no reusable infra" is now false, so the palette is cheap | **TRUE but irrelevant** | ux.10-B's `tui-input` + `KeyEvent` threading are real. Cheapness is not a reason. |
| P4 — the key surface keeps growing | **UNSUPPORTED** | no 11th view named; ~14 of 26 letters used; per-view namespacing already works. |
| P5 (unstated) — `agentctl watch` is still where the operator spends time | **UNSTATED, and post-ux.12 doubtful** | Telegram (ux.12) + `agentctl brief` (ux.11c) moved the daily surface off the TUI. Plan cites zero dogfood data while claiming pressure is "measurably higher". |
| P6 (unstated) — the operator runs a narrow terminal | **UNSTATED** | the chat rail needs 115+ cols, so a chat user already runs wide; the truncation case is a mode the cockpit already flags as degraded. |

## 0B — What already exists (the finding that reframes the increment)

`ux.13` shipped `ControlCommand::{Cancel, SetBudget, SetCaps}` end to end — management API,
FUSE control, `agentctl` CLI subcommands, AND `DataSource` trait methods with working HTTP +
FUSE impls (`watch/source.rs:64-75`, impls `:124-138`). **No view invokes any of them.**

| Verb | Plumbing | Reachable from the cockpit |
|---|---|---|
| approve / deny | ✅ | ✅ Approvals view |
| spawn / inject | ✅ | ✅ Spawn view + converse rail |
| **cancel** | ✅ | ❌ CLI only |
| **set_budget** | ✅ | ❌ CLI only |
| **set_caps** | ✅ | ❌ CLI only |

`docs/ROADMAP.md:1235` records it in as many words: *"**TUI keys deferred** (CLI/HTTP/FUSE
cover the surface — a convenience follow-up)."* Those three verbs are frictions **#1** and
**#4** of the approved design doc (*"Runaway/stuck agent, no kill"*, *"Wrong config/caps,
forced respawn"*), and the track's north star is *"k9s's defining trait is that you can act on
what you see"* (`ROADMAP.md:1277`). **You cannot stop a runaway agent from the screen that is
showing it to you.**

## 0C-bis — Implementation alternatives

| # | Approach | Effort (human / CC) | Net-new capability | Verdict |
|---|---|---|---|---|
| 1 | ux.3b as written (palette + modal) | ~3d / ~2h | ~none once the impossible + duplicated commands are subtracted | both voices against |
| 2 | **ux.13-TUI: row-scoped Cancel/SetBudget/SetCaps + confirm overlay** | ~1.5d / ~1h | **stop a runaway from the cockpit** | **both voices for** |
| 3 | Footer clip fix + `?` help overlay (no `?` is bound today) | ~0.5d / ~20m | delivers ~90% of ux.3b's claimed value | do regardless |
| 4 | Skip to ux.6 evidence view | ~4d / ~3h | serves BOTH products (cockpit + mv governance, EU AI Act Art.12) | highest strategic leverage |

## Competitive evidence (settles the palette question)

k9s's `:` exists because its noun space is **unbounded and runtime-discovered** (every resource
kind plus arbitrary CRDs) — unprintable on a footer, so `:` is load-bearing. The tools whose
shape actually matches agentctl went the other way: **lazygit** (fixed panels, rich verbs) ships
a `?`/`x` keybinding menu and **no** palette; **htop** F1; **btop** `h`. `vim`/`emacs` have
`:`/`M-x` because their verb space is user-extensible. agentctl has **10 compile-time views**,
all already on screen. No `?` key is bound anywhere today.

## NOT in scope (proposed)

- The `:` palette itself (struck by both voices).
- Fuzzy matching (prefix suffices at ~15 commands; a matcher crate fights CLAUDE.md's "justify
  every new crate" / "light runtime").
- `publish_brief` as an operator command — it is a capability-gated AGENT tool
  (`Capability::BriefPublish`); the operator surface is read-only `GET /api/v1/brief`. The
  original plan's command list contained an impossible command.
- "Reload the inspector" — already `[r]` inside the Inspector view. Pure duplication.
- **audit86-P1-9** — struck from this plan. Capability semantics, wrong reviewers, wrong blast
  radius; TODOS.md already prescribes a standalone decision.

## 6-month regret

**Likely:** the palette ships, gets used twice to confirm it works, then never again — `s`/`t`/`m`
are one keystroke and `:sys<Enter>` is five. `mod.rs` + `views.rs` carry a command registry, a
per-source gating table, and ~40 tests keeping a dead path green; every new view pays a
registration tax. It reads as the moment a single-tenant OS started building for imaginary users.

**Expensive:** a CoS loops at 2am, you attach the cockpit, watch spend climb, and cannot stop it
from the screen showing it — because Cancel was deferred as "a convenience follow-up" and the
follow-up got spent on a command palette.

## Sequencing (value per unit effort, for the one real user)

1. **ux.13-TUI verbs + confirm overlay** — closes a recorded deferral; delivers the north star.
2. **`converse::dispatch()` blocks the whole TUI ~8s** (TODOS.md:457, P2) — the cockpit's
   highest-frequency interaction freezes everything incl. Ctrl-C. Lived friction, daily surface.
3. **Footer clip + `?` overlay** — one afternoon; ~90% of ux.3b's claimed value.
4. **ux.6 evidence** — the only queue item serving two products.
5–7. ux.5 (gated on Telegram dogfood), ux.7 (gated on digest), ux.3b palette (below all).

## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 1 | CEO | Run both voices | Mechanical | P6 | always dual-voice |
| 2 | CEO | Strike `publish_brief` from the command list | Mechanical | P4 (DRY)/feasibility | it is an agent tool, not an operator verb — impossible as specified |
| 3 | CEO | Strike "reload inspector" | Mechanical | P4 (DRY) | already `[r]` in-view |
| 4 | CEO | Strike audit86-P1-9 from this plan | Mechanical | P3 | wrong blast radius; TODOS prescribes a standalone decision |
| 5 | CEO | Answer Q4 (fuzzy vs prefix) = prefix | Mechanical | P5 | 15 commands; no crate |
| 6 | CEO | Footer clip fix happens regardless of the outcome | Mechanical | P2 (blast radius, <1d) | measured real defect, 30 min |
| 7 | CEO | Palette vs verbs vs defer | **USER CHALLENGE** | — | both voices 6/6 against the plan as written — NOT auto-decided, goes to the gate |
