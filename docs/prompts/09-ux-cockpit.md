# 09 — Operator Cockpit (Track UX) — build-session kickoff

Paste the block below. Full plan: `docs/plans/ux-cockpit.md`. Build the increments in order,
one per branch; start with **ux.0**.

---

```
TASK: Operator Cockpit (Track UX) — turn `agentctl watch` into a live cockpit the operator drives.
Full plan (read it first): docs/plans/ux-cockpit.md

WHY
`agentctl watch` is read-only, and `agentctl orchestrate` is a separate CLI REPL — you watch OR
chat, never both. The management API (:7999) already has the whole backbone: POST /api/v1/spawn
(201 into the RUNNING instance), POST /api/v1/agents/:id/inject, GET /api/v1/events (SSE), snapshot,
credentials. So this is mostly an agentctl-client effort on a built substrate.

LOCKED DECISIONS (do not relitigate)
- Unified live cockpit, NOT three more [key] tabs: one screen = k9s-style agent table + pinned chat
  rail + live event stream + input box, with a ':' command palette over the existing letter keys.
  This requires an async-loop refactor FIRST (ux.0), preserving every current view's behavior — the
  analog of the p1.1 loop→state-machine refactor.
- Publish host-loopback: the Docker `cos` deployment binds management to 0.0.0.0 IN THE CONTAINER and
  publishes 127.0.0.1:7999:7999, so `agentctl watch --url http://localhost:7999` works from the Mac
  host. agentd DEFAULT bind stays 127.0.0.1 — only the deployment config opts in, published to host
  loopback only.

ARCHITECTURE (the backbone — build once, in ux.0)
One `tokio::select!` loop, three producers → one channel: crossterm keys + the persistent
/api/v1/events SSE feed + a ~30ms render tick that coalesces redraws. DataSource PUSHES into the
channel (not polled). NEVER .await an SSE/inference read on the render thread (the #1 ratatui-chat
bug). Bounded event ring (tail, don't accumulate). Preserve --plain (glyph + color everywhere).

INCREMENTS (one per branch, in this order — main shippable at each step)
  ux.0  Async single-loop refactor + host-loopback reachability. No new feature; existing views now
        update live from SSE; `watch --url localhost:7999` works from the host. /plan-eng-review the
        refactor (must preserve behavior). agentd default bind stays loopback (test it).
  ux.2  Observe — closes cos-ux-01. Snapshot gains last_activity / last_error / error_count /
        idle_secs (redact secrets). Agent table columns AGENT STATUS TURN LAST-TOOL TOKENS $ AGE ⚠;
        row-red on error; idle→amber "stuck" signal; budget bar reuses MemoryPressure 75/90 colors.
        Live event stream pane (summary-first, JSON on Enter, filter chips, row-scopes-stream,
        f=freeze, ▼N new). AgentDetail activity timeline + error strip + turn/timing line.
  ux.1  Converse — fold orchestrate.rs into the cockpit as the chat rail (key [c]). Factor its
        spawn-or-resume + drain_until_turn_complete guards into shared source helpers (CLI REPL must
        still work). Streaming green; target in input-box border title (retarget any agent via the
        selected row, tmux active-pane model); follow:bool + ▼N new (never yank scroll); Enter send /
        Alt+Enter newline; Esc/Ctrl+C cancels stream; inject-rejected/timeout = inline system line +
        resume hint, never hang.
  ux.3  Spawn custom on the fly — closes p7.3-ar-02. Repoint SpawnViewState from exec-a-2nd-agentd to
        POST /api/v1/spawn into the RUNNING instance; extract a shared spawn-routing helper so the CLI
        `agentctl spawn` detects a live agentd and routes to it (exec fresh only if none running). Add
        ⟨custom⟩ mode (freeform task + full deny-by-default cap toggles + tool/connector select).
        Spawn form is the one justified modal (producer keeps running behind it); preview the lowered
        config before launch; on 201 auto-select the new row + drop into its detail/converse. Add the
        ':' command palette (:agents :topology :memory :spawn :approvals :inspect) alongside letter keys.

NON-NEGOTIABLE: every code item = fix + a test that FAILS without it + adversarial verification, not
"applied." No doc left with a stale claim. Linux-gated agentctl/surfaces code needs `make clippy-linux`
before push. Update docs/ROADMAP.md (check off the increment) + DEPLOYMENT.md (ux.0 reachability) +
THREAT_MODEL.md (ux.0 unauthenticated-API-on-host-loopback note) in the same PR as the code.

DONE (whole track) = from the Mac host, `agentctl watch --url http://localhost:7999` opens one live
screen where the operator sees per-agent last-tool + errors at a glance, chats the orchestrator and
injects specific agents with streaming replies, and spawns a custom sub-agent into the running
instance that appears immediately and can be talked to — no second process, no restart. Per-increment:
/autoplan → build → /review → /qa → /ship.
```
