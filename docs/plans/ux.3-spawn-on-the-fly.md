<!-- /autoplan restore point captured 2026-07-26 -->
# ux.3 — Spawn custom agents on the fly, CORE + auto-drop (closes p7.3-ar-02)

Branch: `ux.3-spawn-on-the-fly` · Base: `main` (v0.111.0)
Kind: wiring fix + small UX (mostly-already-built). Second increment of the UX tail (ux.2b ✅ → ux.3 → ux.10).
Track plan: `docs/plans/ux-tail-track.md` §2. **Scope DECIDED: core + auto-drop. The `:` command palette +
modal-over-dashboard are DEFERRED to a follow-on `ux.3b`** (net-new UI subsystems, no reusable infra).

## Premise

An operator can already open the Spawn view in `agentctl watch`, pick a template, toggle capabilities, and
see a rendered JSON payload — but when the running instance is reached over **HTTP** (`--url`/`AGENTCTL_URL`,
the p7.7 management API), the custom capabilities and priority are **silently dropped**, because the client
`SpawnRequest` struct never carried them. So "spawn a custom-cap agent into my running CoS" doesn't actually
apply the caps over HTTP. That's the p7.3-ar-02 gap. The server side is already complete.

**Claim:** carry `capabilities` + `priority` on the client `SpawnRequest`, route the interactive Spawn action
through `DataSource::spawn()` (the HTTP `/api/v1/spawn`) instead of the FUSE-control/exec-2nd-agentd branch,
and auto-focus the newly spawned agent. Low-risk: the server endpoint, the cap-toggle UI, the payload preview,
and `HttpSource::spawn()` all exist — this closes the wiring gap and stops silently dropping caps.

## Grounding (verified 2026-07-26, post-ux.2b)

- **Server `POST /api/v1/spawn` is complete** (`agentd/src/management.rs`): parses `OperatorSpawnRequest`
  (`control.rs:6-12` — `task, id, max_turns, token_budget, priority: Option<u32>, capabilities:
  Option<Vec<Capability>>, orchestrated`), enforces cap.4's deny-by-default privileged gate +
  `X-Approval-Token`, returns `201 {agent_id}`.
- **Client `SpawnRequest` is SHORT** (`agentctl/src/watch/source.rs:16-22`): has `task`(implied)/`id`/
  `max_turns`/`token_budget`/`orchestrated` — **`capabilities` and `priority` are ABSENT.** This is the
  load-bearing gap. `HttpSource::spawn()` (`source.rs:290`) builds the POST body from this struct, so it
  cannot send what the struct can't hold; the FUSE stub `spawn()` (`source.rs:41`) is a `_req`-ignoring
  not-supported stub.
- **Interactive Spawn action** (`execute_pending_spawn`, `mod.rs:955`) uses "try `/agents/control` write,
  else exec a 2nd agentd" (`mod.rs:213-216`) — it **never calls the HTTP `/api/v1/spawn`**, even when the
  active source is HTTP. Note `mod.rs:529`: "FUSE-mode DataSources don't support spawn()".
- **The Spawn UI already exists** (`spawn.rs`, `app.rs`): template picker, capability toggles, and a preview
  that renders the exact JSON payload. A confirmed-spawn banner exists at `mod.rs:227`.

## Delta (the real work)

1. **Add `capabilities` + `priority` to the client `SpawnRequest`** (`source.rs:16-22`) and serialize them
   into `HttpSource::spawn()`'s POST body (`source.rs:290+`), matching the server's `OperatorSpawnRequest`
   wire shape (JSON array of capability objects; omit when None so a safe-cap spawn stays minimal). **This is
   the load-bearing fix** — without it the HTTP path drops caps.
2. **Route the interactive Spawn through `DataSource::spawn()`** for the HTTP source: when the active source
   is HTTP, `execute_pending_spawn` (or its caller) calls `source.spawn(&req)` → `/api/v1/spawn` into the
   running instance, instead of the FUSE-control-write / exec-2nd-agentd branches. **Preserve** the
   FUSE-control path where FUSE is the active source (FUSE `spawn()` is a stub, so FUSE mode keeps writing
   `/agents/control`). The point is to stop exec'ing a second scheduler when a running instance is reachable.
3. **Auto-drop (auto-focus):** after a confirmed spawn, auto-focus/select the new agent in the dashboard so
   the operator "drops into" it (the confirmed banner at `mod.rs:227` already exists; the focus-routing is
   the new bit — set the selection/focus to the returned `agent_id`).
4. Keep the existing full-screen `View::Spawn` (no modal). cap.4's deny-by-default gate means a custom-cap
   spawn either uses safe caps or needs `AGENTOS_ALLOW_PRIVILEGED_SPAWN=1` + an approval token — **surface
   that clearly in the UI** (the error path must say *why* a privileged spawn was rejected, not just fail).

## Decisions for autoplan

- **D1 — client `capabilities` type.** Mirror the server `Vec<Capability>` (share the type if agentctl can
  depend on it) vs. carry `serde_json::Value`/`Vec<String>` and let the server parse. Recommend: whatever the
  cap-toggle UI already produces for the preview — reuse it, don't reshape (DRY). Confirm the preview payload
  and the `/api/v1/spawn` body are semantically identical (same fields+values) so "what you preview is what you spawn."
- **D2 — where the HTTP-vs-FUSE routing decision lives.** `execute_pending_spawn` currently takes a
  `&Path` (FUSE mountpoint). Routing through `DataSource::spawn()` means it needs the active `DataSource`
  (or a `spawn` closure). Decide: thread the `DataSource` in, vs. branch at the caller and only call
  `execute_pending_spawn` (the FUSE/exec path) when the source is FUSE. Recommend the caller-branch (smaller
  blast radius; `execute_pending_spawn` stays the FUSE fallback it already is).
- **D3 — privileged-spawn UX.** When cap.4 rejects a custom-cap spawn (needs `AGENTOS_ALLOW_PRIVILEGED_SPAWN`
  + token), the UI must show the specific reason + remedy, not a generic failure. Recommend: surface the
  server's 403 body verbatim in the Spawn view's error line.
- **D4 — auto-focus target when the spawn is orchestrated vs. one-shot.** An orchestrated agent parks
  (Waiting) awaiting inject; a one-shot runs to completion. Auto-focus both, but decide whether auto-focus
  also opens the converse rail for an orchestrated agent (so the operator can immediately inject). Recommend:
  focus only for ux.3; converse-rail auto-open is a nicety that can be a follow-on.

## Acceptance
- The client `SpawnRequest` carries `capabilities` + `priority`; a spawn over `--url` HTTP applies the toggled
  caps + priority (a test asserts the POST body to `/api/v1/spawn` contains them, matching the UI preview).
- The interactive Spawn action hits `/api/v1/spawn` when the source is HTTP (no second agentd exec'd);
  FUSE mode still writes `/agents/control`.
- A confirmed spawn auto-focuses the returned `agent_id` in the dashboard.
- A privileged-cap spawn without `AGENTOS_ALLOW_PRIVILEGED_SPAWN` shows the server's rejection reason + remedy
  in the UI, and does NOT silently succeed with clamped caps (reject-not-clamp, per cap.2/cap.4).
- `cargo build/clippy --workspace --all-targets -D warnings` clean; `cargo test --workspace` green. No `cargo fmt`.

## DECIDED at the gate (2026-07-26)
- **D2 routing → INLINE in `handle_spawn_key(…, source)`** (M3), matching the approve/deny/converse precedent.
- Plan APPROVED as reshaped (M1-M7). Scope is MEDIUM (~6-8 files + ~15 mechanical test-call-site updates),
  all necessary to make custom-cap HTTP spawn actually work. Ready to build.

## REVISED MECHANISM (post-/autoplan 2026-07-26 — supersedes Delta + D1-D4 above)

Both eng voices (Codex + Claude subagent) confirmed the premise (real silent-caps-drop bug) but reshaped the
*how* — the plan's "mostly already built" was too optimistic. Locked corrections (all CONFIRMED by both):

- **M1 — client `SpawnRequest` gets `capabilities: Option<Vec<agentd::capability::Capability>>` + `priority:
  Option<u32>`** (typed, shared — agentctl already depends on agentd + imports `Capability`; `Capability`
  derives Serialize/Deserialize). `#[serde(skip_serializing_if = "Option::is_none")]`. Serialize into
  `HttpSource::spawn()`'s POST body (source.rs:290). **Correct the plan's inverted framing:** `None`/omitted
  capabilities ≡ **unrestricted ≡ privileged** on the server (management.rs:507), NOT "safe/minimal." A safe
  spawn sends an explicit caps array; in practice every template resolves to `config.agent.capabilities =
  Some([...])`, so this is usually moot — but the framing was a latent trap.
- **M2 — "preview == spawn" must become TRUE (it isn't today).** The preview (app.rs:285) emits only
  `{task,id,capabilities}` (omits priority/max_turns/token_budget/orchestrated), AND its JSON-vs-TOML branch
  is gated on `control.exists()` (app.rs:274) = local-FUSE presence, so in pure HTTP mode the preview shows
  the TOML exec-fallback while the action sends JSON — they diverge exactly in ux.3's target mode. **Fix:** one
  shared helper resolves `PendingSpawn` → a typed `SpawnRequest` (pulling max_turns/token_budget/priority/caps
  from the lowered `AgentConfig`); serialize THAT for both the preview and the POST body; choose the
  JSON-vs-TOML preview by the **active source** (`source.event_stream_url().is_some()`), not `control.exists()`.
- **M3 — route the Spawn action INLINE in `handle_spawn_key(code, app, source)`** (Claude's call; wins the D2
  disagreement). This matches the established precedent — approve/deny (mod.rs:828+), converse (`converse::
  dispatch` → `source.spawn()`), inject, cancel, set_caps all do blocking `source.*` mutations inline on the
  main thread (reqwest::blocking). For HTTP mode: resolve → build `SpawnRequest` → `source.spawn()` inline →
  set banner + focus, **stay in the TUI**. Only FUSE mode falls through to `do_spawn()`→`pending_exec`→
  `execute_pending_spawn` (the `/agents/control` write). Gate strictly on `event_stream_url().is_some()`
  (FuseSource→None, HttpSource→Some) so the FUSE `spawn()` stub (source.rs:41, always-errors) is UNREACHABLE
  from the Spawn view, and FUSE-mode spawn is preserved. Rejected Codex's post-loop-branch: it tears down the
  terminal, rebuilds a fresh `App::new` (loses converse state), and has no live error line. Cost: `source`
  threaded into `handle_spawn_key` + `do_generate` (~15 mechanical test-call-site updates) — accepted.
- **M4 — move the local `ANTHROPIC_API_KEY` gate out of the HTTP path.** `do_spawn` (app.rs:339) refuses to
  spawn unless local `ANTHROPIC_API_KEY` is set — correct for exec-local-agentd, **wrong for remote HTTP**
  (creds live server-side). Keep the check ONLY on the FUSE/exec path; the HTTP path must not require it.
- **M5 — auto-focus via a sticky `App::pending_focus: Option<String>`.** Setting `selected_id` to the returned
  `agent_id` immediately gets wiped: `apply_snapshot` (app.rs:530) clears unknown selections then auto-selects
  row 0, and an in-flight poll predating scheduler insertion can land first. **Fix:** set `pending_focus` on
  spawn success; in `apply_snapshot`, when its id appears, promote to `selected_id` + clear pending; and don't
  let the clear-if-absent logic wipe a selection equal to `pending_focus`.
- **M6 — error surfacing + status codes.** `HttpSource::spawn()` already returns the server body verbatim
  (source.rs:304); M3's inline routing lets that `Err` populate `app.spawn_view.result_msg`. **Correct the
  status codes:** privileged-cap refusal is **400** (management.rs:511-515), missing `X-Approval-Token` is
  **401** — NOT 403. Reject-not-clamp holds (server errors, never clamps-and-succeeds).
- **M7 — `orchestrated = false` deliberately** for a one-shot template Spawn-view spawn (the converse rail
  forces `true`; the Spawn view must set `false` explicitly, not inherit a serde default). `priority` is
  config-sourced (`config.agent.priority`), **no new UI** in ux.3 — the acceptance's "toggled priority" was an
  overstatement (nothing toggles priority; the field is for API completeness). Preserve converse's wire
  behavior (its `capabilities: None` stays omitted via skip_serializing_if → unchanged from today).

## Tests (from both voices)
- `SpawnRequest{capabilities:Some([..]),priority}` serializes a POST body whose caps array matches the
  `Capability` serde shape **and** matches the preview string (guards M2's single-source-of-truth).
- HTTP-mode Spawn routes to `POST /api/v1/spawn` (httpmock, already a dev-dep) and does **not** exec a 2nd
  agentd; asserts the body carries the toggled caps.
- Privileged refusal: mock 400 + "spawn refused … privileged" → lands in `spawn_view.result_msg`, no
  banner/auto-focus.
- Auto-focus survives the race: after `spawn()` returns `agent_id`, `pending_focus`→`selected_id` resolves
  and **survives an `apply_snapshot` that lacks the agent**, then binds on the snapshot that includes it.
- FUSE mode still writes `/agents/control` (unchanged); the FUSE stub is never hit from the Spawn view.

## NOT in scope (→ ux.3b / later)
- The `:` command palette and the modal-over-live-dashboard overlay (both → **ux.3b**).
- ux.10 (TUI polish) — next in the tail.
- Any change to the server `/api/v1/spawn` endpoint or cap.4's gate (already complete; ux.3 is client wiring).

## Risk
LOW–MEDIUM. Mostly client wiring on top of a complete server endpoint. The one care point is D2 (routing the
Spawn action through the right source without breaking FUSE-mode spawn) — get it wrong and FUSE-mode spawn
regresses, or a running instance still gets a second agentd exec'd.
