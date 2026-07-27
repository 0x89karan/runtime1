# ux.10 sub-part A — next-session handoff

Paste the fenced block below into a fresh session to continue the next development leg (the `[l]` Logs
view in `agentctl watch`). Written 2026-07-27 with `main` at v0.113.0. The full ux.10 `/autoplan`
already ran (both eng voices); A's mechanism is locked in `docs/plans/ux.10-tui-polish.md` — this handoff
front-loads the non-obvious, already-decided bits so nothing gets re-derived or re-tripped.

```
Continue AgentOS development: build ux.10 sub-part A — the Logs view in `agentctl watch`.

CONTEXT
- Repo: /Users/0x89karan/dev/GitHub/agentOS. main is at v0.113.0 (tagged). Read CLAUDE.md first.
- ux.10 is the last core UX-tail item. Sub-part B (input widgets) shipped v0.113.0; sub-part C
  (color-eyre) was STRUCK as redundant. Sub-part A is what's left.
- The FULL ux.10 plan already went through /autoplan (both eng voices). The mechanism for A is
  LOCKED in docs/plans/ux.10-tui-polish.md — read the "DECIDED at the /autoplan gate" +
  "/autoplan eng consensus" sections; they are authoritative and supersede the older 2026-07-16 body.
  So A goes STRAIGHT TO BUILD — do NOT re-autoplan it. But first re-verify the plan's file:line
  refs against current main (they were current mid-session; confirm before editing).

THE TASK (ux.10 sub-part A — `[l]` Logs view), locked decisions:
- agentctl watch is a SYNC crossterm loop (run_tui_loop, event::poll(30ms)); step() runs on the
  main thread, NOT in tokio (there is no tokio runtime). So:
- Tail `docker compose logs --follow --timestamps` (KEEP the service prefix — do NOT pass
  --no-log-prefix, or service filtering has nothing to parse) via std::process::Command, stdout
  piped, read on a background std::thread that try_sends a new AppEvent::LogLine into the existing
  sync_channel. Mirror pump.rs's stream_once + the catch_unwind->ProducerDied guard.
- MUST-FIX orphan leak (HIGH): the --follow child never EOFs and the reader thread is never joined,
  so on exit it ORPHANS. `Producers` must own the Child and child.kill() it in Drop (that EOFs the
  pipe + unblocks the reader). Add a manual QA step: "no `docker compose logs` process survives quit."
- Wiring: spawn_producers consumes the tx internally. Simplest: spawn the log producer EAGERLY at
  loop entry, gated on a startup docker-detect bool on App; Drop-kill handles teardown.
- Docker-context gated: `docker compose ps --quiet` at startup -> store bool on App -> gate the `[l]`
  binding + legend entry; absent on bare agentd / `--url` HTTP mode (test the dispatch gate, not the
  shell-out).
- Key = `[l]` (NOT `[g]` — that's Spawn's generate). `g`/`G` stay as in-view top/bottom scroll.
- Batching: CHANNEL_CAP=256 + 256-drain-per-tick will drop/starve under high log volume -> use
  AppEvent::LogLines(Vec<..>) batches + a visible "N lines dropped" accounting.
- Ring buffer: bounded VecDeque ~2000 lines, oldest dropped (mirror EVENT_RING_CAP=2000 in app.rs).
- `/` search reuses sub-part B's tui-input widget (already in the tree). Per-service filter (Tab
  cycles services parsed from the log prefix).
- New files: agentctl/src/watch/logs.rs (LogsView state + render) + agentctl/src/docker.rs
  (detect_docker_context + spawn_compose_logs). New View::Logs arm in step_key; legend update in
  views.rs.

WORKFLOW (per CLAUDE.md + gstack loop)
- build -> /review (Codex adversarial on the diff) -> /qa -> /ship -> then STOP at the user's merge gate.
- A has real runtime surface (unlike B): /qa can verify docker-context gating, the ring-buffer bound,
  and — importantly — the orphan-child kill (spin up a compose project or a fake `docker` on PATH,
  open/close the Logs view, assert no orphaned child). Do drive that at runtime, not just tests.
- Build/clippy/test WORKSPACE-WIDE from the repo root:
  `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
  NEVER run `cargo fmt`. Known flake: agentd `streaming_two_agents_*` fails under parallel load,
  passes single-threaded — not a regression if you didn't touch scheduler streaming code.
- If you delegate the build to a subagent, ALWAYS diff-check its work + run Codex /review — this
  session's delegated builds each shipped a real defect (an overclaim, a fabricated id, a misplaced
  cursor) that passing tests missed and only review/verify caught.
- User gates ALL merges and tags. NEVER push a v* tag without an explicit instruction. Bump the
  version line in CLAUDE.md + CHANGELOG on the release commit (test-enforced by repo_consistency).

STATE
- Merged but UNTAGGED (cut whenever): v0.110.0 (doc.1), v0.111.0 (ux.2b), v0.112.0 (ux.3). v0.113.0 tagged.
- Deferred: par.3 (struck at premise gate — see docs/plans/par.3-*.md), ux.10-C (struck).
- After ux.10-A: ux.3b (`:` palette + modal), evidence-gated ux.6/ux.5/ux.7, then Phase 11 skills / Phase 9 eBPF.

Start by reading CLAUDE.md + docs/plans/ux.10-tui-polish.md, re-verify the A line-refs against main, then build.
```
