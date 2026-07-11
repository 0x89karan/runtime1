# ux.0 — Live single-loop foundation + host-loopback reachability

**Increment:** ux.0 (Track UX cockpit — the first cockpit-lane increment; the p1.1-analog refactor).
**Branch:** ux.0-async-watch-loop
**Depends on:** dx.6 (shipped, v0.76.0 — `make dev-image` for local verify), p7.7 (management API `:7999`), orch.1/orch.2 (SSE `/api/v1/events`).
**Full spec:** `docs/plans/ux-cockpit.md` → "ux.0" section. This doc adds the implementation decision + behavior-preservation strategy for `/plan-eng-review`.

## Problem

`agentctl watch` is a **synchronous poll-render loop**:
- `run_tui` (`watch/mod.rs`): `loop { snap = source.load_snapshot(); draw; if event::poll(tick_ms) { read key; handle } }`. The render thread **blocks** on `event::poll`, and the snapshot is re-fetched every tick regardless of change. There is a second nested loop (`mod.rs:232`) for the pending-spawn-exec path.
- `run_plain`: `loop { load_snapshot; print; sleep(interval) }`.
- `DataSource` (`watch/source.rs:25`) is a **sync trait** (`load_snapshot`, `load_approvals`, `approve`, `deny`, `spawn`, `event_stream_url`). `HttpSource` uses three `reqwest::blocking::Client`s; `FuseSource` reads FUSE files. `event_stream_url()` already returns `…/api/v1/events` (source.rs:244), and `orchestrate.rs` already consumes that SSE stream via a blocking `BufReader` line loop — but `watch` never opens it.

Result: views only update on the poll interval (not live), and there's no way to stream chat/events without blocking the render. Everything downstream in the cockpit lane (ux.2 live stream, ux.1 streaming chat) needs a non-blocking, event-pushed loop first.

## The decision for eng review — async runtime vs. background threads

The spec (`ux-cockpit.md`) names a `tokio::select!` loop. The **requirement** underneath it is narrower and is what actually matters:

> One loop, three producers (keys + SSE feed + ~30 ms render tick), one channel. **Never `.await`/block an SSE or inference read on the render path.** DataSource *pushes* into the channel; bounded event ring; `--plain` preserved.

Two ways to satisfy that requirement:

- **Option A — `tokio::select!` (literal spec).** Add `tokio` (rt+macros+time+sync), `reqwest` `stream` (async) alongside the existing `blocking`, and `crossterm` `event-stream`. The loop selects over a crossterm `EventStream`, an async SSE byte stream, and a `tokio::time::interval`.
  - ✅ Matches the spec verbatim; idiomatic for future async work.
  - ❌ **The decisive con (corrected 2026-07-11 by the Eng-review outside voice):** `approve`/`deny`/`spawn` use `reqwest::blocking::Client`, which **panics** (`Cannot start a runtime from within a runtime`) when called from inside a `tokio::select!` async context. Option A forces converting every mutation to async reqwest or wrapping each in `spawn_blocking` — extra surface for zero functional gain. ❌ **NOTE the earlier "6 MB / no-tokio-today" argument was factually wrong** and is retracted: `reqwest`'s `blocking` feature already runs an internal `tokio` runtime, so `tokio 1.x` is *already* in `Cargo.lock` and linked into agentctl. Size is not the deciding factor (the guard still applies, but Option B's delta is ~0 regardless). ❌ Larger, riskier diff for a *behavior-preserving* refactor.

- **Option B — background producer threads + `std::sync::mpsc` (RECOMMENDED).** Keep agentctl fully synchronous. Spawn:
  1. an **SSE producer thread** running the exact blocking `BufReader` line-loop `orchestrate.rs` already uses, parsing `data:` lines → typed events → `Sender`;
  2. a **snapshot producer thread** polling `load_snapshot` on a slow (~1 s) tick → `Sender` (fallback for fields not carried by events).
  The main render loop: `crossterm::event::poll(30 ms)` for keys + `rx.try_recv()` drain for pushed events/snapshots + coalesced redraw on the tick.
  - ✅ **Zero new dependencies** (std threads + std mpsc); **no size-guard risk**; keeps the "super-light" thesis intact. ✅ Reuses the already-shipped, already-hardened blocking-SSE pattern. ✅ Smallest diff that satisfies the requirement; keys never block on SSE (it's on its own thread), redraws coalesce on the 30 ms poll.
  - ❌ Not literally `tokio::select!` — it's `poll + channel.try_recv`. (Same architecture; different primitive.) ❌ A stalled SSE producer thread must be detectable/replaceable (heartbeat + reconnect-with-backoff).

**Recommendation: Option B (LOCKED — premise gate confirmed 2026-07-11; both review voices concur).** It delivers the exact required architecture (one loop, three producers, one channel, non-blocking render) with zero new deps, avoids the `reqwest::blocking`-in-async-context panic that Option A would trigger on every mutation, and reuses the proven `orchestrate.rs` blocking-SSE loop. The `tokio::select!` phrasing in the spec is the reference idiom, not a hard constraint. Record the current agentctl musl release size as a baseline and assert the post-refactor delta is ~0 (CI guards ≤ 6 MB at `ci.yml`).

## Scope (Option B)

1. **`watch/source.rs`:** add an `events()` producer API to `DataSource` — a method that, given a `Sender<AppEvent>`, spawns/owns the background producer(s) and returns a handle. `HttpSource` implements the SSE producer (persistent `/api/v1/events` connection, reconnect-with-backoff, heartbeat). `FuseSource` implements a poll-only producer (no SSE over FUSE — documented degradation). Mutations (`approve`/`deny`/`spawn`) stay blocking and synchronous (called from key handlers, off the render-critical path — bounded client timeouts already exist).
2. **`watch/mod.rs`:** replace both `run_tui` loops with one loop: `poll(30ms)` keys + `rx.try_recv()` drain + coalesced redraw. Fold the nested spawn-exec loop (`mod.rs:232`) into the single loop (preserve exec-on-exit semantics). `run_plain` keeps a slow poll producer (no live stream needed in plain mode, but it must still print correctly).
3. **Bounded event ring:** an in-`App` ring buffer (cap ~1–2 k, tail-drop), mirroring `MAX_DISPLAY_ENTRIES` discipline. Feeds ux.2's stream later.
4. **Host-loopback reachability — SPLIT OUT to ux.0b** (autoplan final gate, 2026-07-11). The Eng review (C1) found `bind_addr = 0.0.0.0` collides with the fail-closed guard at `management.rs:438` (enforced by `loopback_guard_rejects_non_loopback`) and would expose the unauthenticated API to Docker-bridge peers — a real security decision, not a config edit. It moves to its own security-reviewed increment **ux.0b** (`docs/plans/ux.0b-host-loopback-reachability.md`): decide gated-override vs loopback-proxy, fix the pre-existing QEMU `0.0.0.0`/guard conflict, add the THREAT_MODEL note. **ux.0 stays a pure, behavior-preserving loop refactor** — no bind/compose/deployment changes. (It keeps only the agentd-default-`127.0.0.1` assertion as a no-op regression check.)

## Behavior preservation (the hard requirement — this is a refactor)

Every existing view must behave **identically**: Dashboard, AgentDetail, System, Topology, Memory, Spawn, Inspector, Approvals, Credentials; every key binding; `--plain`; the spawn-exec-on-exit path; all key handlers (`handle_approvals_key`, etc.). The only *visible* change: views now refresh live between snapshot polls. Strategy:
- Keep `App` state + all `handle_*_key` functions and all `views::*` render fns unchanged in signature; only the *drive* loop changes.
- The existing `watch` unit tests (`TestSource`, mod.rs:702+) must pass unchanged; add tests for the new loop behavior.

## Acceptance (from ux-cockpit.md ux.0 + this plan)

- [ ] Every existing view behaves identically, but updates live between snapshot polls (test: an SSE event mutates a row without a full poll cycle).
- [ ] The render loop never blocks on an SSE read (test: a stalled SSE producer thread does not freeze key handling).
- [ ] SSE producer reconnects with backoff after a drop; a dead producer is detected (heartbeat), not silently stuck.
- [ ] Event ring is bounded (test: 10 k events → memory flat, oldest dropped).
- [ ] `agentctl watch --url http://localhost:7999` connects from the Mac host against the Docker `cos` (manual, via `make dev-image`).
- [ ] agentd default bind is still `127.0.0.1` (config-assertion test); only the cos deployment binds `0.0.0.0`.
- [ ] **agentctl musl release binary stays ≤ 6 MB** (Option B: expected ~no change; Option A: blocking gate).
- [ ] `--plain` conveys the same state.
- [ ] `make clippy-linux` clean (watch/ reads FUSE — Linux-gated) + `cargo test` green.

## Files touched
`agentctl/src/watch/source.rs` (producer API + SSE producer), `agentctl/src/watch/mod.rs` (single loop), `agentctl/src/watch/app.rs` (bounded event ring), `agentd/cos.agents.toml` + `distro/overlay/etc/agentd/cos.agents.toml` (bind_addr), `docker-compose.yml` (ports), `docs/DEPLOYMENT.md`, `THREAT_MODEL.md`, `docs/ROADMAP.md` (check off ux.0), `CHANGELOG.md` (at merge, per RULE 2).

## Non-goals (later cockpit-lane increments)
The unified cockpit layout, chat rail, live-stream pane, budget panel, custom spawn — ux.1/ux.2/ux.3/ux.8. ux.0 is *only* the loop foundation + reachability, behavior-preserving.

---

## Eng review (autoplan Phase 3) — architecture, folded findings, tests

**Voices:** Claude subagent (13 findings, absorbed below) + Codex (Phase 3 dual voice). Mode: HOLD SCOPE.

### Architecture (Option B)

```
                 agentctl watch (single OS thread = render loop)
   ┌───────────────────────────────────────────────────────────────┐
   │  loop {                                                         │
   │    crossterm::event::poll(30ms) ── key? ─► step(&mut App, ev)   │
   │    while let Ok(ev)=rx.try_recv() ──────► step(&mut App, ev)    │  step() is PURE:
   │    if app.dirty { terminal.draw(App); app.dirty=false }         │  (&mut App, AppEvent)
   │  }                                                              │   -> Vec<Effect>
   └──────────────▲───────────────────────▲────────────────────────┘   Effect = Redraw
                  │ mpsc::Receiver<AppEvent>│                            | Quit | RunExec
        ┌─────────┴──────────┐   ┌──────────┴───────────┐               | Mutation(...)
        │ SSE producer thread│   │ snapshot producer thr │
        │ (HttpSource only)  │   │ every args.interval   │  detached / daemon threads:
        │ BufReader::lines()  │   │ load_snapshot()->Event│  never join()ed; unwound by
        │ .timeout(Some(45s)) │   │ (reconciliation)      │  process exit / total-timeout
        │ reconnect+backoff   │   └───────────────────────┘
        └─────────────────────┘
   mutations (approve/deny/spawn) stay SYNC reqwest::blocking, called inline from
   step()'s Effect::Mutation handler on the main thread (off the SSE/stream hot path).
```

### Folded findings (outside voice absorbed; each becomes plan scope)

- **F1 [CRITICAL] SSE liveness = total-timeout reconnect, NOT idle-timeout.** `reqwest::blocking` exposes only a whole-request timeout; a silently-dropped stream (NAT/sleep/OOM, no FIN) blocks `BufReader::lines()` forever and the 30s server `: ping` can't help. Set the SSE client `.timeout(Some(45s))` (> the 30s ping); no bytes for 45s → error → reconnect with backoff. Honestly document that idle-timeout liveness is the one place Option A would be cleaner.
- **F2 [HIGH] Extract a pure `step(&mut App, AppEvent) -> Vec<Effect>`** (`Effect = Redraw | Quit | RunExec(cfg) | Mutation(..)`). This is the actual p1.1 "steppable state machine" analog; unit-test coalescing, quit, exec, and inject WITHOUT a terminal. Current loop has zero test coverage (tests hit only `handle_*_key`).
- **F3 [HIGH] Lossy-stream contract.** `/api/v1/events` is a `broadcast` fan-out (p7.7) with no replay; events during a reconnect gap are lost. The **snapshot poller is authoritative**; the event ring may have holes. ux.2 must be built on this contract.
- **F4 [HIGH] FUSE default is degraded.** `detect_source` prefers FUSE; `FuseSource::event_stream_url()` is `None` → local operators get 1s-poll, not the live loop. Fix: steer local operators to `--url` (or prefer HTTP when both reachable); the marquee "SSE mutates a row without a poll" criterion is HttpSource-only.
- **F5 [HIGH] Fix a real current bug.** Today `run_tui` calls `load_snapshot()` + a credentials GET + `load_approvals()` synchronously on the render path every tick (up to ~5s freeze on a slow server). Moving these to the snapshot producer thread is the fix — explicit acceptance criterion, not just a "fallback."
- **F6 [MED] Snapshot producer honors `args.interval`**, not a hardcoded 1s (else `--interval 5` is silently overridden = regression).
- **F7 [MED] Producer threads are detached/daemon; NEVER `join()`** (a thread blocked in `lines()` can't be cancelled; join would hang). `events()` returns a handle that carries un-joined `JoinHandle`s + a best-effort `AtomicBool` stop flag checked between reads; real unwind is process exit / total-timeout.
- **F8 [MED] Coalesce redraws:** a `dirty` flag, drain ALL pending events then draw once per ~30ms tick (per-event draw = flicker + CPU). Bound the channel (`sync_channel` drop-on-full, preserving tail-drop) so a blocked mutation can't grow it unbounded.
- **F9 [MED] Fold the two loops safely:** on `InjectedViaControl` keep the SAME producers running + set the banner; on the exec branch restore the terminal (`drop(guard)`) BEFORE `execvp` (producers die with `execvp`). Every producer parse error → `continue` (mirror `orchestrate.rs`; never `unwrap`).
- **F10 [MED] THREAT_MODEL:** `0.0.0.0`-in-container exposes the UNAUTHENTICATED management API to **every peer on the Docker bridge network**, not just host loopback. Pin compose to `127.0.0.1:7999:7999` (never bare `7999:7999`); the note must cover the container-peer vector.
- **F11 [LOW]** `run_plain` left byte-for-byte unchanged (already non-blocking; a producer adds risk for no gain).
- **F12 [LOW]** The reused SSE parser is `"data: "`-prefix, single-line only — fine for ux.0 (server controls both ends); ux.2's richer stream needs a real SSE field parser.
- **F13 [LOW]** Lazy loads in key handlers (Inspector/Spawn/Memory reads) still block main — out of ux.0 scope, but don't claim "render never blocks on I/O" unqualified.

### Test diagram (codepath → coverage)

| Codepath / flow | Test | Exists? |
|---|---|---|
| `step()` coalescing: N buffered events → ≤1 Redraw/tick | unit (pure `step`) | NEW |
| `step()` quit + exec-on-exit Effect | unit | NEW (loop was untested) |
| `step()` InjectedViaControl keeps producers + banner | unit | NEW |
| SSE half-open (silent, no FIN) → reconnect within 45s | integration (stub stream goes silent) | NEW |
| Reconnect gap → next snapshot reconciles (ring may have holes) | integration | NEW |
| snapshot producer period tracks `--interval` | unit/integration | NEW |
| stalled SSE producer → key handling still advances ≤1 tick | integration | NEW |
| bounded channel/ring: 10k events → memory flat, oldest dropped | unit | NEW |
| agentd default `bind_addr == 127.0.0.1` | config assertion | NEW |
| `--plain` output unchanged | snapshot test | NEW |
| existing `handle_*_key` behavior | unit | EXISTS (keep green) |
| agentctl musl size delta ~0 vs baseline (≤6 MB) | CI guard `ci.yml` | EXISTS |

### Expanded acceptance (supersedes the first-draft list; folds F1–F13)

All original acceptance criteria PLUS: F1 total-timeout reconnect; F2 pure `step()` extracted + tested; F3 lossy-reconnect contract documented; F4 FUSE-degradation steer + HttpSource-only marquee criterion; F5 render-no-longer-blocks-on-per-tick-GETs; F6 `--interval` honored; F7 detached producers never joined; F8 coalesced single redraw/tick + bounded channel; F9 exec/inject fold with terminal-restore-before-execvp + no producer panics; F10 THREAT_MODEL container-peer note + compose pinned to `127.0.0.1:7999:7999`; size baseline recorded.

### Decision audit trail

| # | Phase | Decision | Class | Principle | Rationale |
|---|-------|----------|-------|-----------|-----------|
| 1 | CEO | Premise confirmed: loop refactor + reachability bundled, behavior-preserving | User gate | — | user-confirmed premise |
| 2 | CEO | Mode = HOLD SCOPE | Mechanical | P6 | behavior-preserving refactor, no feature scope |
| 3 | Eng | Option B (threads+mpsc) over A (tokio::select!) | Confirmed (both voices) | P5 explicit | reqwest::blocking panics in async ctx; reuse proven loop; ~0 size delta |
| 4 | Eng | Reachability stays bundled in ux.0 (not split) | Taste→user-confirmed | P3 | 1-line config edits, cohesive with spec |
| 5 | Eng | SSE liveness = total-timeout reconnect | Mechanical | P5 | idle-timeout not available in reqwest::blocking |
| 6 | Eng | Extract pure `step()` | Auto | P5 explicit | testability; the real p1.1 analog |
| 7 | Eng | Snapshot poller is authoritative (stream is lossy) | Auto | P1 | broadcast has no replay |
| 8 | Eng | Bound the channel (sync_channel drop-on-full) | Auto | P2 | prevent unbounded growth on blocked mutation |
| 9 | Eng | Compose pinned 127.0.0.1:7999:7999 + THREAT_MODEL container-peer note | Auto (security) | P1 | unauthenticated API on Docker bridge |

### Codex Eng voice — additional/sharper findings (Phase 3 dual voice)

Codex corroborated F1–F13 and sharpened three, plus one that supersedes the plan's reachability approach:

- **C1 [CRITICAL — supersedes reachability plan] The `0.0.0.0` bind collides with a fail-closed guard.** `agentd/src/management.rs:438` does `ensure!(bound.ip().is_loopback(), "refusing to bind on non-loopback…")`, with a test `loopback_guard_rejects_non_loopback` (`:640`). So the plan's `bind_addr = "0.0.0.0"` **will not start** — and `distro/overlay/etc/agentd/cos.agents.toml:28` *already* sets `0.0.0.0` (pre-existing conflict on main; the QEMU management API likely fails the guard — flag to the build session/operator). Docker `-p` cannot reach a container service bound to `127.0.0.1`, so reachability genuinely needs one of: (a) a **gated override** — a `[management] allow_non_loopback = true` opt-in that relaxes the guard for the deployment, publish pinned to `127.0.0.1:7999:7999`, THREAT_MODEL note on Docker-bridge-peer exposure of the **unauthenticated** API; (b) a **loopback-forwarding proxy** inside the container (agentd stays `127.0.0.1`; a forwarder listens `0.0.0.0:7999` — reuse the cred.3.1 `LoopbackForwardingProxy` seam); or (c) **split reachability out of ux.0** and decide (a)/(b) in its own security-reviewed increment.
- **C2 [MED] Panic-hook restoration.** `mod.rs:127` replaces the panic hook and discards the original rather than restoring it (`:213`). A bigger loop refactor raises terminal-corruption risk. Fix: a real terminal/session guard owning raw mode + alt screen + original-hook restore.
- **C3 [HIGH, refines F8] Channel bounding.** Use `sync_channel` + `try_send`, coalesce overflow into a single `EventsDropped(n)`/`SnapshotNeeded` signal, and NEVER block producer threads on UI consumption (blocking `send` can deadlock producer shutdown). Test 10k events against **channel** memory, not just `App` ring length.
- **C4 [refines F3] Treat reconnect + parse-fail + broadcast `{"lagged":n}` (management.rs:278) as one `Invalidated` event** → immediate snapshot+approvals fetch + gap marker. SSE is lossy; snapshot is authoritative.

### ENG DUAL VOICES — consensus
Architecture sound (Option B): CONFIRMED. Behavior-preservation hazards mapped: CONFIRMED (both). SSE half-open/lifecycle: CONFIRMED. Lossy-stream reconciliation: CONFIRMED. Channel bounding: CONFIRMED. **Reachability `0.0.0.0` guard conflict: CONFIRMED CRITICAL by Codex** → decision required (surfaced at gate).

---

## GSTACK REVIEW REPORT

**Pipeline:** /autoplan (CEO + Eng; Design/DX documented scope-skips — no new UI/API). **Branch:** ux.0-async-watch-loop.
**Voices:** Eng — Claude subagent (13 findings) + Codex 0.139.0 (9 findings, incl. the C1 CRITICAL guard conflict). Consensus 6/6 CONFIRMED.

| Gate | Decision |
|---|---|
| Premise | Confirmed: loop refactor + reachability, behavior-preserving |
| Architecture | Option B (threads + `std::sync::mpsc`), CONFIRMED both voices; async rationale corrected (reqwest::blocking already links tokio; A panics in async ctx) |
| Reachability (final gate, security) | **Split to ux.0b** — `0.0.0.0` collides with the `management.rs:438` fail-closed guard + exposes the unauthenticated API to Docker peers |

**Outcome:** ux.0 = pure behavior-preserving loop refactor (Option B), 17 findings folded into scope/acceptance/test-diagram; reachability + its security decision → `ux.0b`. Plan APPROVED for build.

VERDICT: APPROVED — build ux.0 (Option B). Reachability deferred to ux.0b with a security review.

NO UNRESOLVED DECISIONS
