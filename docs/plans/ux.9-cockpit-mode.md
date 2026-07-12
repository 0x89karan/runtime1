<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/ux.9-cockpit-mode-autoplan-restore-20260712-132938.md -->
# ux.9 — Cockpit mode (Track UX cockpit)

**Increment:** ux.9 (Track UX cockpit). Next after ux.0 (async single-loop refactor, shipped v0.77.0)
and ux.0b (host-loopback reachability, shipped v0.80.0), per the lane sequencing in
`docs/prompts/13-parallel-dev-rules.md` and `docs/prompts/09-ux-cockpit.md`:
`ux.0 → ux.9 → ux.2 → ux.1 → ux.8 → ux.3`.

**Full track plan:** `docs/plans/ux-cockpit.md` (see "Added 2026-07-11" section, ux.9 entry, and the
"North star" note: *"the cockpit is agentos's default operator surface... ux.9 makes it the default."*)

## Problem

Today, watching an agentd run requires a second, separate step: start `agentd` (as its own process or
container command), then in another terminal/window run `agentctl watch` pointed at it (via FUSE
`/agents` in-container, or `--url http://host:7999` from outside). There is no single command that
boots straight into an always-on status/debug console the way `k9s`/`htop` do for their respective
subsystems. The existing `orchestrate` Docker entrypoint mode (`docker/entrypoint.sh:226-250`)
establishes the pattern for "cold-start agentd + attach a client in the foreground, one process group,
signal-forwarded" — but for `agentctl orchestrate` (the chat REPL), not `agentctl watch` (the cockpit).

## Goal (from the plan doc)

A `cockpit` entrypoint mode that starts `agentd` **and** execs `agentctl watch` in the foreground of
the same container/process group — the operator boots straight into the always-on TUI. The TUI reads
the FUSE `/agents` surface (no port needed in-container, no host loopback config needed); the flight
recorder is already the live log view (via the Inspector pane, `[i]`). `docker run -it … cockpit` =
watch-first; the existing headless `cos` mode is unaffected.

## What already exists (reuse, don't rebuild)

- **`docker/entrypoint.sh:226-250` (`orchestrate` mode)** — the exact process-group + signal-forwarding
  shape this needs: cold-start `agentd` in the background, wait for a readiness signal, `trap` SIGTERM/
  SIGINT to forward to the backgrounded `agentd` PID for a clean checkpoint, then exec the client in the
  foreground.
- **`agentctl/src/watch/source.rs:451-484` (`detect_source`)** — already auto-detects FUSE-vs-HTTP:
  checks `agents_dir.join("system").exists()` (default `/agents/system`) before falling back to the
  management API healthz probe. `agentctl watch` run with no flags already does the right thing once
  FUSE is mounted — cockpit mode does NOT need `--url` or the management API at all if it runs
  in-container with FUSE.
- **`agentd/src/main.rs:890` (`fuse_mountpoint`)** — hardcoded to `/agents`; mounted synchronously during
  `agentd` startup (before the scheduler loop begins) unless `--no-fuse`/`AGENTOS_NO_FUSE` is set.
  `agentd`'s own FUSE-mount readiness is NOT currently exposed as a wait-able signal to a wrapping shell
  script — `orchestrate` mode's `/healthz` HTTP poll is the readiness pattern that exists today; cockpit
  mode needs the FUSE analog (poll for `/agents/system` existing, mirroring `detect_source`'s own check).
- **ux.0's async single-loop refactor** — `agentctl watch`'s TUI is already a non-blocking, event-pushed
  single loop (background thread producers + bounded channel + 30ms render tick); no further work needed
  there for cockpit mode to "just work" once booted.
- **`agentctl watch --plain`** — already exists for non-TTY/CI contexts; relevant if `cockpit` mode is
  ever run without an attached terminal (should probably refuse or fall back, not silently break — an
  open question for Eng review).

## PREMISE DECISION (2026-07-12): flip the actual default, not just an opt-in mode

**Decided at the autoplan premise gate.** Landing `cockpit` as a purely opt-in third entrypoint mode
(alongside `cos`/`agent`/`orchestrate`) would satisfy "bootable in one command" but not the track's
stated north star — *"the cockpit is agentos's default operator surface."* Confirmed: `Dockerfile`
(repo root — corrected during CEO dual-voice review; there is no `docker/Dockerfile`, only
`docker/Dockerfile.semantic-kb-mcp`) already has a real, literal default — `ENTRYPOINT ["/entrypoint.sh"]`
+ `CMD ["shell"]` (`Dockerfile:79-80`) — meaning `docker run agentos:full` with **no command** currently
drops into a bash shell (`docker/entrypoint.sh:65-69`, `shell)` case: `check_api_key; print_banner;
exec bash`). This is the correct, single, surgical lever to flip: change `CMD ["shell"]` → `CMD ["cockpit"]`.

**⚠️ See the "USER CHALLENGE" section below — both CEO dual-voice reviewers (Claude + Codex)
independently found a critical gap in this decision that needs resolving before it ships.**

**Guardrail (from the premise gate's own risk callout):** this must NOT touch `docker-compose.yml`'s
`cos`/`agent` services, which both already set an **explicit** `command: cos` / `command: agent`
(`docker-compose.yml`) — explicit `command:` always overrides the image's `CMD` default, so the CoS's
unattended cron-driven production deployment is unaffected by this change. Confirmed via
`grep -rn "docker run.*agentos" docs/*.md README.md` that no existing doc relies on the bare
(no-command) `docker run` invoking `shell` mode — no doc/behavior collateral to fix.

## Scope (this increment)

- **`docker/entrypoint.sh`** — new `cockpit)` case, modeled on `orchestrate)`, with two CEO
  dual-voice-review corrections to the original draft:
  - **Corrected: do NOT `exec agentctl watch`.** Both reviewers verified `orchestrate)`'s actual
    cold-start pattern (`docker/entrypoint.sh:236-249`) does **not** `exec` the client — it runs
    `agentctl orchestrate ...` as a plain foreground command (line 246), and only AFTER it returns
    does it `kill $AGENTD_PID` + `wait $AGENTD_PID` (lines 247-248). The `trap` set on line 240 stays
    live throughout because the shell process is never replaced. My original draft said `exec agentctl
    watch`, which would replace the shell (dropping the trap) and break the graceful-shutdown
    acceptance criterion this exact plan states. `cockpit)` must copy the non-exec pattern exactly:
    plain foreground `agentctl watch`, then explicit `kill`+`wait` on the backgrounded `agentd` PID
    after it returns. (`exec` remains correct only for the "already running, attach" branch, mirroring
    `orchestrate)` line 234 — there's no backgrounded process in this shell to clean up in that case.)
  - **Corrected: explicit `check_api_key` call.** Both reviewers found `orchestrate)` itself never
    calls `check_api_key` (every other mode — `shell`, `demo`, `cos`, `agent` — does). Copying
    `orchestrate)` verbatim would silently inherit this gap, and since `cockpit` is about to become
    the zero-arg default, it's now the very first thing a new user without `ANTHROPIC_API_KEY` set
    would hit. `cockpit)` calls `check_api_key` explicitly at the top of its case, matching the other
    modes (not `orchestrate)`, which is a separate, smaller pre-existing gap logged to TODOS.md below,
    out of scope for this PR to fix).
  - Wait for FUSE `/agents/system` readiness (NOT `/healthz` — no management API needed), `trap`
    SIGTERM/SIGINT → forward to the backgrounded `agentd` PID (clean checkpoint). If `agentd` fails to
    mount FUSE within the wait window, fail loudly (mirror `orchestrate`'s 15s timeout + explicit
    error) — **but see the USER CHALLENGE below**, since a FUSE-mount timeout is the exact failure
    mode the unresolved `--privileged` gap produces.
- **`Dockerfile`** — flip `CMD ["shell"]` → `CMD ["cockpit"]` (line 80). This is THE scope-defining change
  from the premise decision: `docker run -it agentos:full` (no command) now boots straight into the
  cockpit, matching the north star. `shell` mode remains available as an explicit `docker run ... shell`
  escape hatch (unchanged, not removed). **Gated on resolving the USER CHALLENGE below.**
- **`docker-compose.yml`** — **corrected: NO compose file changes needed.** Both reviewers confirmed
  `docker compose run --rm agent cockpit` already overrides `command: agent` with zero compose edits —
  my original draft's "TBD... new service entry?" was reconciled against my own 0E analysis (which had
  already reached this conclusion) and dropped as an internal inconsistency. Only docs need to show the
  override-command invocation.
- **`docs/DEPLOYMENT.md` / `docs/RUNBOOK.md` / `README.md`** — document the new entrypoint mode AND
  update any quickstart snippet that relied on (or demonstrated) the old bare-`docker run` shell default,
  since that default is now `cockpit`. Must prominently document the `--privileged` requirement (or
  whatever the USER CHALLENGE resolution below settles on).
- **`docs/ROADMAP.md`** — check off ux.9 on ship.
- **`TODOS.md`** — log `orchestrate)` mode's pre-existing missing-`check_api_key` gap as a small
  separate follow-up (found during this review, not this PR's scope to fix on `orchestrate` itself).
- Possibly: a small FUSE-mount-event flight event or exposed readiness marker if polling `/agents/system`
  directly proves fragile (race with the FUSE library's own mount completion) — Eng review to confirm
  whether the existing synchronous-mount-before-scheduler-starts guarantee is sufficient, or whether a
  more explicit signal is needed.

## Out of scope (deferred to later increments per the track sequencing)

- Any new TUI view/pane content — ux.2 (observe), ux.1 (chat/converse), ux.8 (budget control), ux.3
  (custom spawn) all add cockpit *features*; ux.9 is purely about *booting into* the existing TUI as
  the default surface.
- Non-Docker "cockpit mode" for bare-metal/QEMU deployments — the plan doc scopes this to the Docker
  entrypoint; QEMU's `agentos-cos.service` already runs `agentd` as the sole foreground process (no
  client to boot alongside it in that deployment shape) unless a future increment adds one.
- Host-loopback / management-API changes — none needed; this is the FUSE, in-container path, distinct
  from ux.0b's host-loopback (management API) reachability work.

## Acceptance (from the plan doc + this increment's scope)

- `docker run -it … cockpit` (or `docker compose run --rm <service> cockpit`) boots `agentd` and the TUI
  as the foreground process; the operator sees the live cockpit immediately, no second terminal/command.
- **`docker run -it agentos:full` with NO command** now boots the cockpit by default (the flipped
  `CMD`) — this is the literal "cockpit is the default surface" acceptance criterion from the premise
  decision. `docker run -it agentos:full shell` still works as the explicit escape hatch.
- Ctrl-C (SIGINT) / `docker stop` (SIGTERM) shuts both `agentd` and the TUI down gracefully — checkpoint
  intact, matching `orchestrate` mode's existing signal-forwarding guarantee.
- The existing `cos` (headless, unattended/cron-driven) entrypoint mode is **provably unaffected**: its
  `docker-compose.yml` service sets an explicit `command: cos`, which overrides the image `CMD` — the
  CMD flip must not change `cos`'s behavior. This is a hard acceptance gate (test it), not just an
  assumption — the premise gate's own risk callout was "must NOT accidentally default CoS to interactive
  TUI mode."
- The existing `agent`/`orchestrate` entrypoint modes are otherwise unaffected — this is a new case in
  the entrypoint dispatch plus one `CMD` line changed, not a change to any other mode's behavior.
- **Revised (Approach D): works unprivileged AND privileged.** `agentctl watch` in cockpit mode uses
  FUSE opportunistically when the container has the capability, and transparently falls back to the
  (loopback-only, in-container) management API over HTTP otherwise — via `detect_source`'s existing,
  already-shipped fallback chain. No `--privileged` requirement, no host-loopback reachability needed
  either way (contrast with ux.0b, which is specifically about the Docker-container-to-Mac-host path;
  cockpit's management API here never needs to leave the container).
- A bare, unprivileged `docker run -it agentos:full` (no `--privileged`, no extra flags) successfully
  boots the cockpit and shows a live (if empty) agent dashboard — this is the corrected version of the
  literal acceptance test the USER CHALLENGE was about; it must actually pass in an unprivileged
  container, not just avoid crashing.
- Every code item ships with a test that fails without it (per this track's standing rule in
  `docs/prompts/09-ux-cockpit.md`); `make clippy-linux` for the new `agentd/src/config.rs` change
  (Rust, not Linux-gated specifically, but still covered by the standard quality gate).
- **(Added, DX review) A non-interactive/scripted `docker run --rm agentos:full` (no `-it`) exits
  immediately with a clear message instead of hanging** — this is the corrected version of a
  previously-undiscovered regression (see Error & Rescue Registry); explicitly test this, don't just
  test the interactive success path.
- **(Added, DX review) The dashboard's empty state is not a dead end**: the plan documents (README +
  DEPLOYMENT.md) that `[n]` opens the existing Spawn view to launch a first agent — this doesn't
  require new TUI code (p6.6 already shipped it), only a doc pointer, so it's in scope despite ux.3
  (custom spawn *polish*) being deferred.

## Open questions for review

1. Does the FUSE-mount-readiness wait need a new signal from `agentd` (e.g. a flight event, or a marker
   file), or is polling `/agents/system` (mirroring `detect_source`'s own check) sufficient given the
   mount happens synchronously before the scheduler loop starts? **Still open** — additionally, both
   CEO reviewers confirmed `agentd/src/main.rs:923-937` does NOT crash on a FUSE mount failure (logs
   `tracing::warn!` and continues with `maybe_session = None`), so a bash timeout can't distinguish
   "agentd chose to skip FUSE" from "agentd tried and failed" — the wait loop's failure message needs
   to say "FUSE mount did not become ready" rather than imply agentd itself failed.
2. ~~Does `docker-compose.yml` need a new service entry...~~ **RESOLVED** (see Scope): no compose
   changes needed, confirmed by both reviewers.
3. ~~What happens if `cockpit` mode is run without an attached TTY...~~ **RESOLVED** (Codex, verified):
   `agentctl watch` already auto-falls-back to `--plain` when stdout isn't a TTY
   (`agentctl/src/watch/mod.rs:79`) unless `--no-plain` overrides it — no new launcher policy needed,
   cockpit mode inherits this for free.
4. Is there a privileged-mode / FUSE-capability precondition (`--privileged`, `SYS_ADMIN` capability)
   that needs an explicit preflight check + actionable error message, mirroring how `orchestrate` mode
   preflights the management API healthz? **This is now the USER CHALLENGE below** — both reviewers
   independently escalated this from "open question" to "the load-bearing risk in the premise decision."
5. **NEW (Codex) — RESOLVED (auto-decided, P5 explicit-over-clever + P1 completeness):** What TOML
   config does `cockpit)` cold-start with? Copying `orchestrate)` verbatim means `/etc/agentd/
   agents.toml` — the baked demo `surveyor`/`analyst` agents — which would immediately start spending
   API tokens the moment the zero-arg default runs. "Boot into a cockpit" and "start spending tokens
   on demo work automatically" are different products, and silently spending money on boot is the kind
   of implicit side effect the explicit-over-clever principle argues against. Decided: `cockpit)`
   cold-starts `agentd` with a minimal, agent-free config (scheduler + FUSE + management surface only,
   zero `[[agents]]` entries) — matching the k9s/htop analogy the plan already invokes: opening the
   tool shows you the (empty) cluster state, it doesn't launch workloads on your behalf. Spawning
   agents into a running cockpit is explicitly ux.3's job (deferred); for now, an operator who wants
   the demo agents can still run `docker run ... agent` (unaffected) or the existing `orchestrate`
   mode. Requires a new minimal config file (e.g. `docker/cockpit.toml` — no agents, `[management]`
   for FUSE/scheduler startup only) shipped alongside `docker/agent.toml`/`docker/agents.toml`.
6. **NEW (Claude subagent):** `AGENTOS_NO_FUSE`/`--no-fuse` isn't handled — if either is set in the
   environment `cockpit)` inherits, `agentd` will deliberately skip the FUSE mount
   (`agentd/src/main.rs:895-604`, the `no_fuse` branch) and the wait-loop will poll for something that
   was never going to appear, timing out instead of failing fast with a clear "FUSE explicitly
   disabled, cockpit mode requires it" message.

## USER CHALLENGE: the premise decision's load-bearing assumption doesn't hold for the literal acceptance test

**Both CEO dual-voice reviewers (Claude subagent + Codex), working independently with no shared
context, converged on the same critical finding.** Per autoplan's rules, this is a User Challenge —
not auto-decided — because both models are recommending a change to a direction you already chose at
the premise gate.

**What you said:** "Flip the default now" — change `Dockerfile`'s `CMD` from `shell` to `cockpit`, so
`docker run -it agentos:full` with no command boots the cockpit.

**What both models found:** The plan's own acceptance test — bare `docker run -it agentos:full`, no
`--privileged` flag anywhere in the plan's Scope, Acceptance, or documented invocations — will not
actually work. Cockpit mode is FUSE-only by design (no management API, no `--url`). FUSE mounting
needs privileged/`SYS_ADMIN` container capability; `docker-compose.yml` sets `privileged: true`
explicitly for `cos`/`agent`, but that's compose-only — a bare `docker run` carries no such flag by
default, and neither existing documented bare-`docker run` recipe (`cos`, `orchestrate` — see
`README.md`) needs FUSE today, so nothing currently exercises this path. Confirmed in code
(`agentd/src/main.rs:923-937`): a failed FUSE mount does NOT crash `agentd` — it warns and continues
without `/agents`. So an unprivileged `docker run -it agentos:full` (the exact command this plan uses
to demonstrate success) would: start `agentd`, silently continue without FUSE, hit `cockpit)`'s 15s
readiness timeout, and exit with an error — where **today the identical command harmlessly drops into
a shell.** That's a regression in the most common first-run path, not an improvement.

**Why this matters:** the whole point of the premise decision was making the zero-arg default *better*.
If the literal thing a new user runs regresses from "works, drops into a shell" to "hangs 15s then
fails," the premise decision achieves the opposite of its intent for exactly the audience most likely
to hit it (anyone who doesn't already know to add `--privileged`).

**What we might be missing:** maybe nobody actually runs the bare `docker run agentos:full` invocation
in practice — if everyone goes through `docker-compose.yml` (which already sets `privileged: true`) or
through documented recipes that will be updated to include `--privileged`, this risk is more
theoretical than practical. The plan can't currently rule this out either way.

Options:
- **A) Add a fast preflight + explicit `--privileged` docs (recommended).** Keep the CMD flip.
  `cockpit)` preflights FUSE capability in ~1-2s (e.g. attempt-and-check, or a known Linux capability
  probe) BEFORE the 15s wait loop, and on failure prints an actionable error: *"cockpit mode requires
  FUSE — re-run with `--privileged` (or `--cap-add SYS_ADMIN --device /dev/fuse`)"* instead of a bare
  timeout. Every doc/quickstart that shows a bare `docker run` for cockpit-reachable modes gets
  `--privileged` added. Effort: human ~2-3 hrs / CC ~20-30 min. Closes the regression while keeping
  the north-star default.
- **B) Don't flip the bare `Dockerfile` CMD — ship cockpit as opt-in only.** Reverts to Option 1 from
  the original premise question. Zero regression risk (nothing about the default changes), but doesn't
  achieve "cockpit is the literal default" this increment — the north star would need a later
  increment (once A's preflight work lands, or once the client-bootstrap alternative below is built)
  to actually flip it. Effort: human ~1 hr less than A (skip the CMD line + preflight) / CC ~5 min less.
- **C) Pursue the deeper alternative (Claude subagent's finding): make `agentctl watch` itself the
  bootstrapper.** Instead of a Docker-only shell wrapper, teach `agentctl watch` to detect "no data
  source" and cold-start `agentd` itself, reusing `detect_source`'s existing FUSE→HTTP fallback chain
  (`agentctl/src/watch/source.rs:451-484`) so it degrades gracefully to the management API path
  (already unprivileged, like `orchestrate` mode) instead of hard-requiring FUSE. This also
  generalizes to bare-metal and the QEMU deployment this plan currently defers. Meaningfully bigger
  scope — likely its own increment, not a fit for ux.9's stated "boot two existing things together"
  size. Effort: human ~1-2 days / CC ~2-3 hrs — comparable to CEO's original Approach B, now motivated
  by a real correctness gap rather than just "cleaner architecture."

Net: A keeps this increment's scope and timeline intact and directly closes the specific regression
both models found; B is the safe retreat if you'd rather not add preflight logic right now; C is the
right long-term answer per both reviewers but is a bigger, separate increment.

**RESOLVED (user, 2026-07-12): Option A — add preflight + docs.** *(Superseded below by Approach D
after Eng dual-voice review — kept here for the audit trail, not the final design.)* The CMD flip
stays in scope. New scope items added: a FUSE-capability preflight in `cockpit)`, and `--privileged`
added to every relevant doc/quickstart.

**SUPERSEDED (user, 2026-07-12, after Eng dual-voice review): Approach D — management-API fallback,
drop the `--privileged` requirement entirely.** Both the Claude Eng subagent and Codex, independently,
found that Option A's hard FUSE/`--privileged` requirement is an unforced error: `orchestrate)` mode
already proves an unprivileged, FUSE-free pattern works in production today — it sets
`AGENTD_MANAGEMENT_ENABLED=true` and polls `/healthz` over HTTP, never touching FUSE at all. And
`agentctl watch`'s existing `detect_source` (`agentctl/src/watch/source.rs:451-484`, already shipped,
no new code) already prefers FUSE when present and **transparently falls back to the management API
over HTTP** when it isn't. Chaining these two already-correct pieces together removes the whole
`--privileged` requirement: `cockpit)` enables management like `orchestrate)` does, and the readiness
wait polls for **either** `/agents/system` (FUSE) **or** `http://127.0.0.1:7999/healthz` (HTTP)
becoming ready — whichever comes up first. `agentctl watch` run with no flags then does the right
thing automatically in both the privileged (FUSE) and unprivileged (HTTP) case. User chose this over
keeping Option A's preflight.

**Revised Scope (this increment) — supersedes the FUSE-preflight-based scope below:**
- `docker/entrypoint.sh`'s `cockpit)` case: `check_api_key` (explicit, matching Decision #7); export
  `AGENTD_MANAGEMENT_ENABLED=true` + `AGENTD_MANAGEMENT_PORT` (mirroring `orchestrate)`); cold-start
  `agentd` in the background with the cockpit config (see Decision #9's revised resolution below);
  `trap` SIGTERM/SIGINT immediately after backgrounding (before the wait loop, so early Ctrl-C is
  always caught); readiness wait polls for `/agents/system` **OR** `${_MGMT_URL}/healthz` (15s
  timeout, matching `orchestrate)`'s existing budget) — fail loudly only if **neither** becomes ready
  (this is now a true "agentd didn't start" failure, not a `--privileged`-shaped one); `shift` the
  `cockpit` argument before forwarding remaining args (fixes the argument-forwarding bug below); run
  `agentctl watch "$@"` in the **foreground, non-exec'd** (Decision #6); guard that invocation so
  `set -e` can't skip cleanup (fixes the cleanup bug below); `kill`+`wait` the backgrounded `agentd`
  PID after `agentctl watch` returns, preserving its exit code.
- **No `--privileged` requirement, no FUSE preflight code, no `--privileged` doc changes.** Every
  doc/quickstart can show the plain `docker run -it agentos:full` (or `... cockpit`) with no extra
  flags — FUSE is used opportunistically in privileged containers, HTTP otherwise, both fully
  functional. This removes essentially all of Option A's incremental scope.
- **`docker/entrypoint.sh`: fix `set -e` swallowing cleanup on non-signal exit (Eng review, confirmed
  by direct code read — `docker/entrypoint.sh:2` sets `set -e` for the whole script; the client
  invocation on line 246 in `orchestrate)` is unguarded, so any nonzero exit from
  `agentctl orchestrate`/`watch` that isn't a caught signal aborts the script immediately, skipping
  the `kill`/`wait` cleanup on lines 247-248 entirely).** This is a real, pre-existing bug in
  `orchestrate)` that the plan's Decision #6 would otherwise propagate verbatim into `cockpit)`. Fix
  for the new `cockpit)` case (not a mandatory backport to `orchestrate)`, though doing so cheaply
  while touching this code is worth considering):
  ```bash
  set +e
  agentctl watch "$@"
  rc=$?
  set -e
  kill "$AGENTD_PID" 2>/dev/null || true
  wait "$AGENTD_PID" 2>/dev/null || true
  exit "$rc"
  ```
- **Fix argument-forwarding bug (Eng review, Codex, verified):** existing entrypoint cases never
  `shift`, so `"$@"` inside `cockpit)` still contains the literal string `cockpit` as its first
  element. `agentctl watch "$@"` would become `agentctl watch cockpit --plain`, and `watch` takes no
  positional arguments (`agentctl/src/watch/mod.rs`) — this would break `docker run ... cockpit
  --plain`. Fix: `shift` immediately on entering the `cockpit)` case, before touching `"$@"` again.
- **Fix zero-agent config validation (Eng review, both Claude subagent + Codex, confirmed by direct
  code read — CRITICAL, blocks the increment as originally scoped):** `agentd/src/config.rs:338-354`
  (`Config::agent_configs()`) hard-errors when neither `[agent]` nor `[[agents]]` is set:
  `"no agents configured; set [agent] for a single agent or [[agents]] for multiple"`. `agentd/src/
  main.rs:119` calls this unconditionally near the top of `run_agent()`, well before the FUSE mount or
  management API start — so Decision #9's "minimal agent-free `cockpit.toml`" as originally described
  cannot boot at all; every cockpit invocation would fail instantly on config validation, before ever
  reaching the readiness wait. **This requires a small Rust change**, correcting Approach A's "no Rust
  code changes" framing (accurate when written, before this bug was found). Fix, per Codex's suggested
  shape (explicit opt-in, not a silent broad allowance): add a new config field —
  `[scheduler] allow_empty_agents = true` (or equivalent name decided during Eng review's second pass) —
  and extend `agent_configs()`'s match arm: `(None, true) if self.scheduler.allow_empty_agents =>
  Ok(vec![])`, else keep the existing bail. `docker/cockpit.toml` sets this flag explicitly, so a
  typo'd/empty config elsewhere in the repo still fails loudly rather than silently idling. New unit
  test: `agent_configs_allows_empty_when_opted_in` + `agent_configs_still_rejects_empty_by_default`.
- **`docs/DEPLOYMENT.md` / `docs/RUNBOOK.md` / `README.md`** — document the new entrypoint mode; no
  `--privileged` callout needed (Approach D removes that requirement).
- **`docs/ROADMAP.md`** — check off ux.9 on ship.
- **`TODOS.md`** — log: (a) `orchestrate)`'s pre-existing missing-`check_api_key` gap, (b) `orchestrate)`'s
  pre-existing `set -e`-swallows-cleanup bug (same root cause as the `cockpit)` fix above — worth a
  cheap backport, tracked separately since it's out of this PR's direct blast radius), (c) the
  `docker stop`-may-not-cleanly-stop-the-TUI-process gap (Eng review, Codex: Docker sends SIGTERM to
  PID 1 only; bash defers running its trap until the foreground `agentctl watch` child exits, so
  `agentctl watch` doesn't get a clean chance to restore the terminal on `docker stop` — mitigated by
  tracking the `agentctl watch` PID in the trap too, see Implementation Tasks below, but the
  `orchestrate)` mode shares this same latent gap unpatched).

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|-----------|-----------|----------|
| 1 | CEO/Premise | Flip `Dockerfile` `CMD` default to `cockpit` instead of shipping cockpit mode as opt-in only | User gate (not auto-decided) | — | User selected "flip the default now" at the premise gate; confirmed `docker-compose.yml`'s `cos`/`agent` explicit `command:` lines are unaffected by an image-level `CMD` change | Opt-in-mode-only approach |
| 2 | CEO/0C-bis | Approach A (shell entrypoint + CMD flip) over Approach B (Rust-native subcommand) | Mechanical | P5 explicit-over-clever, P4 DRY | Solves an already-solved problem (orchestrate mode's pattern) correctly; B's benefits (no polling loop) are marginal since FUSE mount is already synchronous pre-scheduler | Approach B (Rust-native cockpit subcommand) |
| 3 | CEO/0D | Defer all 5 expansion candidates (crash-loop supervision, welcome tour, reattach-command hint, `--template` passthrough, exit summary) to TODOS.md | Taste (cherry-pick, neutral posture) | P2 boil-lakes (not in blast radius), P3 pragmatic | None are required for stated acceptance; each expands scope beyond "boot two things together" | All 5 — logged to TODOS.md as follow-ups |
| 4 | CEO/0F | Mode = SELECTIVE EXPANSION | Mechanical | autoplan override (feature-enhancement default) | Iteration on existing system (entrypoint dispatch + TUI), not greenfield | SCOPE EXPANSION / HOLD SCOPE / REDUCTION |
| 5 | CEO/Dual-voice | USER CHALLENGE: Option A (preflight + docs) — keep CMD flip, add FUSE-capability preflight | User gate (not auto-decided) | — | Both Claude subagent + Codex independently found the bare `docker run` acceptance test would regress (15s hang then fail vs. today's harmless shell); user chose to keep the flip and close the gap with a preflight rather than retreat (B) or redesign (C) | Option B (don't flip default); Option C (client-bootstrap redesign) |
| 6 | CEO/Dual-voice | `docker/entrypoint.sh`: fix exec-breaks-signal-forwarding bug — non-exec `agentctl watch` + explicit kill/wait, mirroring `orchestrate`'s actual (not assumed) pattern | Mechanical | Correctness — verified against `orchestrate`'s real code, not the plan's initial (wrong) description | Original draft would have broken the plan's own graceful-shutdown acceptance criterion | Original `exec agentctl watch` description |
| 7 | CEO/Dual-voice | Add explicit `check_api_key` call to `cockpit)` (don't blindly inherit `orchestrate)`'s pre-existing gap) | Mechanical | Consistency with every other entrypoint mode | Cockpit is about to become the zero-arg default — first thing an API-key-less user hits | Silently copying `orchestrate)` verbatim |
| 8 | CEO/Dual-voice | `docker-compose.yml`: no changes needed (resolved internal inconsistency between Scope and 0E) | Mechanical | DRY / internal consistency | Both reviewers + my own 0E analysis already concluded override-command works with zero compose edits | New compose service entry |
| 9 | CEO/Dual-voice | Cockpit cold-starts with a minimal agent-free config, not the demo `surveyor`/`analyst` agents.toml | Taste (single-model finding, auto-decided) | P5 explicit-over-clever, P1 completeness | Matches the k9s/htop "show state, don't launch workloads" analogy the plan itself invokes; avoids silent token spend on boot | Cold-starting with `/etc/agentd/agents.toml` (demo agents) |
| 10 | Eng/Dual-voice | USER CHALLENGE #2: Approach D (management-API fallback) supersedes Option A (FUSE preflight + `--privileged`) | User gate (not auto-decided) | — | Both Claude subagent + Codex independently found `orchestrate)`'s existing unprivileged HTTP pattern + `detect_source`'s already-shipped FUSE→HTTP fallback make `--privileged` an unforced requirement; user chose to remove the requirement entirely rather than keep Option A's preflight | Option A (keep preflight + `--privileged`) |
| 11 | Eng/Dual-voice | Fix `set -e` swallowing cleanup on non-signal exit in the new `cockpit)` case | Mechanical | Correctness — confirmed by direct code read (`entrypoint.sh:2`, `:246`) | Pre-existing bug in `orchestrate)` that Decision #6 would have propagated verbatim; `cockpit)` becomes the default, raising the stakes | Copying `orchestrate)` verbatim without the `set +e`/capture-`$?` guard |
| 12 | Eng/Dual-voice | Fix argument-forwarding: `shift` before passing `"$@"` to `agentctl watch` | Mechanical | Correctness — confirmed by direct code read (no existing case shifts; `watch` takes no positional args) | Without it, `cockpit --plain` becomes `agentctl watch cockpit --plain`, breaking flag passthrough | No fix (silent breakage of any forwarded flag) |
| 13 | Eng/Dual-voice | Add `[scheduler] allow_empty_agents` opt-in + relax `Config::agent_configs()` (small Rust change) | Mechanical | Correctness — confirmed by direct code read (`config.rs:338-354`, `main.rs:119`); explicit opt-in per Codex's suggested shape, not a silent broad allowance | Decision #9's agent-free config literally cannot boot today — `agent_configs()` hard-errors on zero agents, called before FUSE mount or management start | Broadly allowing empty configs everywhere (typo risk); reverting Decision #9 to use the demo agents.toml instead |
| 14 | DX/Dual-voice | Add non-interactive/non-TTY check (`[ -t 1 ]`) to `cockpit)`, fail fast with actionable message | Mechanical | Correctness — Claude subagent proved `agentctl watch`'s non-TTY `--plain` fallback loops forever with no exit condition; confirmed by reading `agentctl/src/watch/mod.rs`'s `run_plain` | Without it, any scripted/CI bare `docker run agentos:full` that used to exit cleanly via `shell` mode now hangs until externally killed — a real, previously undiscovered regression | Leaving the infinite loop unguarded; accepting the hang as intended behavior |
| 15 | DX/Dual-voice | Rewrite Error & Rescue / Failure Modes registries to match Approach D; write the actual "Implementation Tasks" section (previously referenced 3× but missing) | Mechanical | Internal consistency — both reviewers independently caught that my own post-Approach-D plan edits left the registries describing the superseded Option A design | Registries are meant to be the authoritative error-handling spec; stale registries would have shipped wrong recovery instructions (e.g. telling operators to add `--privileged` when that requirement no longer exists) | Leaving the registries as-is; deferring the rewrite to implementation time |
| 16 | DX/Dual-voice | Add doc requirements: `[n]`-to-spawn empty-state pointer, README mode table with the `shell` escape hatch, CHANGELOG "BREAKING" migration note | Mechanical/Taste (migration-note scope was a judgment call, see DX Completion Summary) | P1 completeness (don't ship a documented dead end when a real answer exists); proportionate response to Codex's broader "compat shim" suggestion | First-run empty dashboard needs a next step; existing operators' muscle memory/scripts need a documented heads-up on the CMD-default change | Codex's suggested env-var/alias compatibility shim for one release — declined as disproportionate for this project's pre-1.0 stage; a CHANGELOG note matches existing precedent (cred.2) |

---

## CEO REVIEW — Step 0

### 0B. Existing Code Leverage

Every sub-problem this plan needs already has a load-bearing piece of existing code to build on —
this increment is orchestration/wiring, not new subsystems:

| Sub-problem | Existing code | Reuse strategy |
|---|---|---|
| Cold-start `agentd` + attach a client in the foreground, one process group, signal-forwarded | `docker/entrypoint.sh:226-250` (`orchestrate` mode) | Copy the shape exactly: background `agentd &`, `trap` SIGTERM/SIGINT → kill+wait the PID, then exec the client. Only the readiness check and the exec'd client differ. |
| Auto-detect FUSE vs HTTP so the TUI "just works" once mounted | `agentctl/src/watch/source.rs:451-484` (`detect_source`) | Zero new code needed on the `agentctl` side — cockpit mode's `agentctl watch` invocation with no flags already prefers FUSE. |
| Readiness signal to know when it's safe to exec the client | `detect_source`'s own check: `agents_dir.join("system").exists()` | Mirror this exact check in the shell wait-loop (`docker/entrypoint.sh`) rather than inventing a new signal — same source of truth as the client itself uses, so there's no risk of the two disagreeing about "ready." |
| Live, low-latency TUI once booted | ux.0's async single-loop refactor (already shipped, v0.77.0) | No further work — this is exactly the foundation ux.0 was built for. |
| Non-interactive / CI fallback | `agentctl watch --plain` (already exists) | Reused as-is for the "no TTY attached" edge case (see 0E). |

**Is this plan rebuilding anything that already exists? No.** There is no existing "boot agentd + a
client together" implementation for `watch` specifically — `orchestrate` mode is the closest analog and
is being reused as a pattern, not duplicated code (the shell case is new, but its shape mirrors
`orchestrate`'s established one, per the DRY principle — don't invent a second pattern for the same
problem).

### 0C. Dream State Mapping

```
  CURRENT STATE                       THIS PLAN                              12-MONTH IDEAL
  Operator runs `cos`/`agent`         `cockpit` becomes the literal          The cockpit is not just the
  headless, or manually pairs         image default (docker run with        default boot target — it's
  it with a second `agentctl          no command boots straight into        the place where ALL agentos
  watch` invocation in another        the TUI). Existing headless           interaction happens: observe
  terminal/window. No single-         modes (`cos`, `agent`) are            (ux.2), converse (ux.1), tune
  command "just watch it work"        untouched — this is additive,        budgets (ux.8), spawn custom
  experience exists.                  not a behavior change to              agents (ux.3), review evidence
                                       production paths.                     (ux.6), get pushed alerts
                                                                             (ux.4), and eventually a web
                                                                             version (ux.5) — k9s/htop
                                                                             for agents, fully realized.
```

This plan moves directly toward the 12-month ideal: it's the literal boot-sequencing step that the
later increments (ux.2/ux.1/ux.8/ux.3/ux.6/ux.4) all assume exists. Nothing about this increment moves
away from that ideal or needs to be undone later.

### 0C-bis. Implementation Alternatives

```
APPROACH A: Shell-script entrypoint mode + Dockerfile CMD flip (minimal viable)
  Summary: New `cockpit)` case in docker/entrypoint.sh (bash, mirrors `orchestrate`), flip
           `Dockerfile`'s `CMD ["shell"]` → `CMD ["cockpit"]`. No new Rust code.
  Effort:  S (human: ~2-3 hours / CC: ~15-20 min)
  Risk:    Low — pure orchestration/shell change; the two pieces it wires together
           (agentd's FUSE mount, agentctl's detect_source) already exist and are tested.
  Pros:    - Smallest diff; matches the `orchestrate` precedent exactly (consistency).
           - No Rust code changes means no new attack surface, no new test surface beyond
             the shell script and a docs/config assertion test.
           - Ships fast, unblocks ux.2/ux.1/ux.8/ux.3 which all assume cockpit mode exists.
  Cons:    - The FUSE-readiness wait is a polling loop in bash (mirrors detect_source's own
             check, but duplicated logic in two languages — a small DRY smell, acceptable
             since it's a 3-line poll, not real logic).
           - No new flight event for "cockpit mode started" — less observable than it could be.
  Reuses:  orchestrate mode's process-group/signal-forwarding shape; detect_source's FUSE
           readiness check (mirrored, not literally shared, since one is Rust and one is shell).

APPROACH B: Rust-native `agentd cockpit` subcommand (ideal architecture)
  Summary: Add a `cockpit` mode to agentd itself (or a thin new `agentctl cockpit` command) that
           spawns agentd as a child process from Rust, waits on an actual IPC/file-based readiness
           signal (not a polling loop), forwards signals via Rust's own signal-handling (already
           used elsewhere in agentd for checkpointing), and execs/spawns agentctl watch — all in
           one binary, no bash orchestration layer at all.
  Effort:  L (human: ~1-2 days / CC: ~2-3 hours)
  Risk:    Medium — new process-spawning + signal-forwarding code in Rust needs its own test
           surface (child-process lifecycle, signal propagation, zombie/orphan handling) that
           the shell version gets "for free" from bash's own process semantics. More moving
           parts for a problem that's fundamentally just "start two things, forward signals."
  Pros:    - No polling loop — could use a real readiness channel (e.g. a Unix socket or a
             one-shot file with inotify) instead of bash's `until [ -e ... ]; do sleep; done`.
           - Removes the docker/entrypoint.sh dependency entirely — works identically outside
             Docker (e.g. a hypothetical bare-metal "cockpit" launcher), which could serve the
             QEMU/bare-metal deployment shape this plan currently defers (see Out of scope).
           - More testable in Rust's own test harness (cargo test) vs. bash script testing.
  Cons:    - Meaningfully bigger diff for a problem that's 90% "orchestrate two existing binaries
             and forward signals" — the shell version already does this correctly today for
             `orchestrate` mode; reimplementing it in Rust is solving an already-solved problem.
           - Delays ux.2/ux.1/ux.8/ux.3, which don't need any of Approach B's extra capability.
           - The "readiness without polling" benefit is marginal: agentd's FUSE mount already
             completes synchronously before the scheduler loop starts (see 0B) — polling
             `/agents/system` for up to a few hundred ms is not a real reliability gap.

RECOMMENDATION: Choose A because the problem this increment solves — "boot two existing, already-
correct binaries together in the right order, forward signals for a clean shutdown" — is exactly
what `orchestrate` mode's shell pattern already solves correctly in production; Approach B pays a real
implementation and test-surface cost to remove a polling loop that isn't actually causing problems,
and it would delay every downstream ux.2/ux.1/ux.8/ux.3 increment that assumes cockpit mode already
exists. This maps to the explicit-over-clever and DRY-with-orchestrate-mode principles.
```

**Auto-decided (autoplan Selective Expansion override — P5 explicit-over-clever + P4 DRY dominate
in Eng-adjacent decisions; approaches are not close, no taste decision).** Approach A selected.

### 0D. Mode-Specific Analysis (SELECTIVE EXPANSION)

**Complexity check:** Approach A touches 2-3 files (`docker/entrypoint.sh`, `Dockerfile`,
`docker-compose.yml` doc/config) plus doc updates — well under the 8-file/2-new-service smell
threshold. No complexity concern.

**Minimum set of changes:** the `cockpit)` entrypoint case + the `CMD` flip are the irreducible core;
`docker-compose.yml` service exposure and doc updates are necessary companions (an entrypoint mode
nobody can discover isn't shipped), not deferrable padding.

**Expansion scan (candidates only, not yet in scope):**
- *10x check:* a `cockpit` mode that also auto-detects crash-loops (agentd exits unexpectedly) and
  restarts it with a visible "agentd restarted Nx" banner in the TUI, turning the cockpit into a
  lightweight supervisor, not just a co-launcher.
- *Delight opportunities:* (1) a first-boot welcome/tour overlay in the TUI when cockpit mode starts
  cold (vs. attaching to an already-running agentd); (2) print the exact `docker exec`/`agentctl watch
  --url` commands an operator would need if they detach and want to reattach from another terminal;
  (3) a `--template` passthrough so `docker run ... cockpit --template scout --task "..."` boots
  straight into a running custom agent's cockpit view, not just the default `agents.toml`; (4) exit
  summary on Ctrl-C (agents run, tokens spent, checkpoint path) instead of a bare terminal restore;
  (5) auto-launch cockpit mode when running the `dev-image` locally without args (matches the CMD flip
  spirit for the contributor inner loop too, not just the published image).
- *Platform potential:* none identified — this is a leaf UX feature, not infrastructure other features
  build on (ux.2/ux.1/ux.8/ux.3 build on the TUI itself, already existing, not on cockpit-mode-the-
  entrypoint specifically).

**Cherry-pick ceremony (auto-decided, autoplan override — Selective Expansion, neutral posture, P2
boil-lakes + P3 pragmatic):** All 5 candidates are genuinely nice-to-have but none is required to hit
this increment's stated acceptance criteria, and each expands blast radius (new banner/UI text, a new
CLI flag surface, crash-loop supervision logic) beyond "boot two things together." None is in the
blast radius of files this plan already touches in a way that makes it free — auto-deciding **DEFER
ALL FIVE to TODOS.md** as follow-up ideas for ux.9 or later cockpit increments, keeping this increment
at Approach A's minimal-viable scope. (Per P2: boil lakes covers the blast radius of files *this plan
touches* — a crash-loop supervisor and a `--template` passthrough are not in that radius; they're new
capability, correctly the SELECTIVE EXPANSION category of "candidate, not automatic".)

### 0E. Temporal Interrogation

```
  HOUR 1 (foundations):    Need to confirm: does agentd's FUSE mount reliably complete BEFORE the
                           process is considered "up" in every deployment shape (privileged mode
                           required, /agents mountpoint must exist and be writable by the container
                           user)? Resolved now: yes — main.rs:890 mounts synchronously pre-scheduler-
                           loop; --privileged is already required for all FUSE-using modes today
                           (docker-compose.yml already sets `privileged: true` on cos/agent).
  HOUR 2-3 (core logic):   Ambiguity: what's the exact readiness-wait timeout and failure message?
                           Resolved now (Eng review confirms): mirror orchestrate's 15s timeout +
                           explicit stderr message naming the failure (not a bare "timeout").
  HOUR 4-5 (integration):  Surprise risk: `docker-compose.yml`'s `cos`/`agent` services already set
                           `command:` explicitly — does `cockpit` even need a compose entry, or is
                           `docker compose run --rm agent cockpit` (overriding just the command) already
                           sufficient with zero compose file changes? Resolved now: zero compose file
                           changes needed for the override-command path; only docs need updating
                           (Eng review to confirm no compose changes are structurally required).
  HOUR 6+ (polish/tests):  What they'd wish they'd planned for: a test that actually proves the CMD
                           flip doesn't change `cos`'s resolved command (not just "docker-compose.yml
                           still says command: cos" — prove Docker's own command-precedence rule holds
                           for THIS image, e.g. via `docker compose config` showing the resolved command
                           is unaffected). Planned now: add this as an explicit acceptance test, not an
                           assumption.
```
(Human-team hours shown for planning-decision calibration; with CC + gstack the whole increment
compresses to roughly 20-40 minutes of implementation once these decisions are locked.)

### 0F. Mode Selection

**Auto-decided (autoplan override): SELECTIVE EXPANSION.** Rationale: this is a feature-enhancement/
iteration on an existing system (Docker entrypoint dispatch + an established TUI), not greenfield —
SELECTIVE EXPANSION is the context-dependent default for that shape, and the autoplan Phase 1 override
mandates it explicitly. HOLD SCOPE analysis was run first (0D complexity check: no concern), then the
expansion scan surfaced 5 candidates, all deferred per the cherry-pick auto-decision above. Chosen
implementation approach under this mode: Approach A (0C-bis), unchanged by mode selection — SELECTIVE
EXPANSION doesn't push toward the "ideal architecture" approach the way SCOPE EXPANSION would, and
Approach A was already the stronger pick on its own merits.

### 0.5 CEO Dual Voices — Consensus Table

Both voices ran independently (Claude subagent: fresh context, no prior review; Codex: separate
sandboxed process, same independence). Both verified claims against the actual repo rather than
trusting the plan's prose.

```
CEO DUAL VOICES — CONSENSUS TABLE:
═══════════════════════════════════════════════════════════════
  Dimension                            Claude    Codex    Consensus
  ──────────────────────────────────── ───────── ───────── ─────────
  1. Premises valid?                   NO*       NO*       DISAGREE→USER CHALLENGE (resolved: Option A)
  2. Right problem to solve?           PARTIAL   PARTIAL   CONFIRMED (mode itself is right; CMD-flip
                                                            risk was the gap, now closed)
  3. Scope calibration correct?        NO**      NO**      CONFIRMED gap (exec bug, check_api_key,
                                                            compose TBD) — all fixed
  4. Alternatives sufficiently         NO***     N/A       CONFIRMED gap — client-bootstrap alternative
     explored?                                             (C) now documented, deferred as future work
  5. Competitive/market risks covered? N/A       N/A       N/A (internal tool; k9s/htop analogy noted
                                                            as imperfect by Claude — informational only)
  6. 6-month trajectory sound?         YES†      YES†      CONFIRMED once USER CHALLENGE resolved
═══════════════════════════════════════════════════════════════
* Both flagged the --privileged/FUSE premise as the load-bearing risk — this IS the USER CHALLENGE,
  not a taste disagreement between the two voices (they agree with each other, disagree with the
  plan's original unqualified premise).
** Both independently found the exec-breaks-trap bug and the missing check_api_key call; Claude also
   flagged the docker-compose.yml Scope/0E internal inconsistency.
*** Claude's subagent surfaced the client-bootstrap alternative (Option C) as a third approach 0C-bis
    didn't consider; Codex did not raise this independently but didn't contradict it either.
† Once the FUSE/--privileged gap is closed (Option A), both agree this is a sound, low-risk increment
  that correctly sets up ux.2/ux.1/ux.8/ux.3.
```

**Cross-voice tension:** none in the end — Claude and Codex converged on the same core finding
(--privileged/FUSE gap) independently, which is why it was escalated to a User Challenge rather than
presented as a routine taste decision. No unresolved disagreement between the two voices themselves.

## NOT in scope

(Consolidates the "Out of scope" section above plus items deferred during CEO review.)

- Any new TUI view/pane content (ux.2/ux.1/ux.8/ux.3) — this increment is boot-sequencing only.
- Non-Docker cockpit mode for bare-metal/QEMU deployments — deferred; Option C (client-bootstrap)
  would naturally extend to this but is its own increment.
- Host-loopback/management-API reachability changes — none needed (distinct from ux.0b).
- The 5 expansion candidates from 0D (crash-loop supervision, welcome tour, reattach-command hint,
  `--template` passthrough, exit summary) — logged to TODOS.md as follow-ups (see TODOS section below).
- `orchestrate)` mode's own pre-existing missing-`check_api_key` gap — logged to TODOS.md, not fixed
  in this PR (out of blast radius; `cockpit)` gets the fix directly, `orchestrate)` doesn't inherit it
  retroactively as part of this increment).
- Option C (client-initiated bootstrap redesign of `agentctl watch`) — the stronger long-term
  architecture per both CEO reviewers, but a separate, larger increment; not this PR.

## What already exists (CEO Step 0B, restated for the record)

See "What already exists" section near the top of this plan — `orchestrate` mode's process-group/
signal-forwarding shape (corrected to its actual non-exec pattern), `detect_source`'s FUSE-vs-HTTP
auto-detection, `agentd`'s synchronous pre-scheduler FUSE mount, ux.0's async TUI loop, and
`agentctl watch`'s existing non-TTY `--plain` auto-fallback. Nothing in this plan rebuilds any of these.

## Error & Rescue Registry

**Rewritten for Approach D (DX dual-voice review, both Claude + Codex flagged the pre-Approach-D
version of this table as stale/wrong — it still described the superseded FUSE-preflight design).**

| Error condition | Detection | User-visible message | Recovery |
|---|---|---|---|
| `ANTHROPIC_API_KEY` unset | `check_api_key` (new, explicit call in `cockpit)`) | Same actionable message every other mode gives | Set the env var, re-run |
| Not an interactive terminal (no `-it`, e.g. CI/scripted/health-check invocation) | `[ -t 1 ]` check at the top of `cockpit)`, before backgrounding `agentd` (**new — DX review, Claude subagent finding**: without this, `agentctl watch`'s non-TTY `--plain` auto-fallback loops forever with no exit condition, hanging any scripted bare `docker run agentos:full` that used to exit cleanly via `shell` mode) | `"cockpit mode requires an interactive terminal — re-run with -it, or use a headless mode: cos / agent / orchestrate"` | Add `-it`, or use an explicit headless mode |
| Neither FUSE nor the management API becomes ready within 15s | Readiness wait loop times out (polls `/agents/system` OR `${_MGMT_URL}/healthz`) | `"agentd started but is not responding on either the FUSE surface or the management API after 15s — check agentd's stderr above"` | Inspect `agentd`'s stderr (printed above this message, never swallowed); most likely a config or startup error, not a privilege issue (Approach D removed the `--privileged` requirement) |
| Cold-start `agentd` crashes immediately (exits before either readiness signal appears) | **New: `kill -0 "$AGENTD_PID"` liveness check inside the wait loop**, not just polling for readiness — checked on every poll iteration | `"agentd exited unexpectedly during startup — see stderr above"` — fails within one poll interval (~0.5s), not the full 15s timeout | Fix the underlying `agentd` startup issue (bad config, etc.) — the actual `agentd` stderr is what explains it, this message just stops the operator waiting on a dead process |
| SIGTERM/SIGINT during boot (before readiness) | `trap` set immediately after backgrounding `agentd`, before the wait loop | Clean exit, checkpoint intact (agentd was running, just not yet ready) | N/A — this is the success path for early Ctrl-C |
| `docker stop` sends SIGTERM to PID 1 while `agentctl watch` is the foreground child | bash defers the trap until the foreground child exits (a bash signal-handling property, not a bug in this plan specifically) — `agentctl watch` may not get a clean chance to restore the terminal before Docker's SIGKILL grace period | Shared, pre-existing limitation with `orchestrate)` mode; mitigated in `cockpit)`'s own trap by also tracking and killing the `agentctl watch` PID (see Implementation Tasks) | Known limitation; logged to TODOS.md for a possible `orchestrate)` backport |

## Failure Modes Registry

| Codepath | Failure scenario | Test covers it? | Error handling exists? | Silent or visible? |
|---|---|---|---|---|
| `cockpit)` entry | Non-interactive invocation (no `-it`) | Planned (new) | Yes (new `[ -t 1 ]` check, fails immediately) | Visible — was previously a **CRITICAL, undiscovered gap** (DX review): would have hung forever, now fixed |
| `cockpit)` readiness wait | Neither FUSE nor HTTP becomes ready, times out after 15s | Planned (new) | Yes (15s timeout + message distinguishing "not ready" from a privilege issue) | Visible |
| `cockpit)` cold-start | `agentd` crashes on bad config | Planned (new — liveness check) | Yes (`kill -0` check added to the wait loop, see Implementation Tasks) | Visible — fails within ~0.5s instead of the full 15s |
| Signal forwarding | SIGTERM arrives mid-boot (before `agentctl watch` starts) | Planned (new, matches `orchestrate`'s existing behavior) | Yes (`trap` set immediately after backgrounding) | Visible (clean exit) |
| Signal forwarding | SIGTERM arrives while `agentctl watch` is running | Planned (new — corrected non-exec pattern + `set -e` guard fix) | Yes | Visible (clean shutdown, checkpoint intact) — `agentctl watch`'s own terminal cleanup on `docker stop` is a known shared limitation with `orchestrate)` (see Error & Rescue Registry) |
| `agentctl watch` exits nonzero for a non-signal reason (panic, internal error) | New `set +e`/capture-`$?` guard around the client invocation | Planned (new — regression test for the `set -e`-swallows-cleanup bug) | Yes | Visible — `agentd` still gets cleaned up, exit code preserved |
| CMD default flip | `docker-compose.yml` `cos`/`agent` accidentally affected | Planned (new — `docker compose config` assertion test) | N/A (config-level guarantee, not runtime) | Would be silent if wrong — **test explicitly required, not optional** |
| Zero-agent config | `agent_configs()` rejects it without the new opt-in flag | Planned (new — 2 unit tests, see Implementation Tasks) | Yes (explicit opt-in required, not a broad allowance) | Visible (config validation error) if the flag is missing |

**All previously-flagged critical gaps are now folded into the Implementation Tasks below** — the
prior version of this section referenced "see Implementation Tasks" three times without that section
existing anywhere in the document (DX review, Claude subagent finding); that's fixed below.

## CEO Completion Summary

- Premise Challenge (0A): right problem, right increment size; premise's `--privileged`/FUSE
  assumption was the gap → resolved via USER CHALLENGE #1 (Option A) → **later superseded by Approach D
  during Eng review** (see below) — kept in the audit trail as the first resolution, not the final one.
- Existing Code Leverage (0B): 5/5 sub-problems map to existing, reused code — no rebuilding.
- Dream State (0C): moves directly toward the 12-month cockpit-as-default-surface ideal.
- Implementation Alternatives (0C-bis): Approach A selected over B; Option C (client-bootstrap)
  surfaced during dual-voice review as a stronger long-term alternative, deferred.
- Mode-Specific Analysis (0D): SELECTIVE EXPANSION scan produced 5 candidates, all deferred to TODOS.
- Temporal Interrogation (0E): 4 ambiguities resolved during this review, 0 deferred to implementation.
- Mode Selection (0F): SELECTIVE EXPANSION, auto-decided per autoplan override.
- Dual Voices: ran (Claude subagent + Codex), both independent, both converged on the same critical
  finding — escalated to USER CHALLENGE #1, resolved (Option A, later superseded).
- NOT in scope: written (6 items).
- What already exists: written (5 reused pieces, restated).
- Decisions logged (CEO phase): 9 (1 user gate, 6 mechanical, 2 taste/auto-decided).

## Eng Completion Summary

- Scope Challenge: accepted as-is (no complexity trigger; 3-4 files touched, well under the 8-file
  smell threshold even after Approach D's revisions).
- Architecture: dual-voice review surfaced USER CHALLENGE #2 (Approach D supersedes Option A) —
  resolved by the user; also surfaced 3 confirmed bugs (`set -e` swallows cleanup, missing `shift`,
  zero-agent config can't boot) — all fixed directly (mechanical, verified by direct code read).
- Code Quality: no DRY violations beyond a noted (accepted) minor duplication of "is FUSE/HTTP ready"
  logic between bash and Rust (`detect_source`) — three near-duplicate readiness checks exist post-merge
  (Rust `detect_source`, `demo)` mode's existing loop, `cockpit)`'s new one); logged to TODOS.md as a
  possible future `wait_for_readiness()` shared helper, not blocking this increment.
- Test Review: coverage diagram folded into the Failure Modes Registry above; every new branch has a
  planned test. One real gap acknowledged and NOT closed in this plan: no CI workflow builds or runs
  the Docker image at all today (confirmed: `.github/workflows/` has no `docker build/run/compose`
  step) — this increment's tests are described but will run manually/in a follow-up CI addition, not
  as part of this PR's own CI gate. Logged to TODOS.md.
- Performance: no concerns (single-container entrypoint orchestration, not a hot path).
- Failure modes: 8 codepaths assessed (revised post-Approach-D), 0 remaining critical gaps (the
  agentd-crash-during-boot liveness check and the previously-dangling "see Implementation Tasks"
  references are now resolved — see Implementation Tasks below).
- Decisions logged (Eng phase): 4 (1 user gate — USER CHALLENGE #2 — plus 3 mechanical bug fixes).

## DX Completion Summary

- Product type: CLI/Docker entrypoint tool (developer/operator-facing, not end-user-facing).
- First five minutes: gap found (empty dashboard with no explicit "press [n] to spawn" guidance in the
  plan's own Acceptance criteria, despite the underlying Spawn view already existing and working) — fixed
  below.
- Error message quality: gap found (Error & Rescue / Failure Modes registries were stale, still
  describing the superseded Option A design) — rewritten above to match Approach D.
- Escape hatches: functionally correct (`... shell` still works, `docker-compose.yml` unaffected) but
  under-documented (README's own Docker quickstart never showed the escape hatch) — fixed below
  (README mode-table requirement added to doc scope).
- CLI naming: acceptable as-is; Codex suggested documenting "cockpit = agentd + agentctl watch" more
  explicitly to avoid operator confusion when the TUI identifies itself as "watch," not "cockpit" — added
  as a doc requirement, not a rename (renaming the TUI's internal self-identification is out of scope).
- Migration/breaking-change concern: **the most significant DX finding** — Claude's subagent proved a
  concrete, previously undiscovered bug (non-interactive/scripted bare `docker run agentos:full`
  invocations would now hang forever instead of exiting cleanly, since `agentctl watch`'s non-TTY
  `--plain` fallback has no exit condition). Fixed via the new `[ -t 1 ]` check (see Error & Rescue
  Registry above and Implementation Tasks below). Codex additionally recommended a broader
  compatibility shim (env var/alias for one release) for operators whose muscle memory/scripts expect
  the old shell default; evaluated and NOT adopted — this repo's precedent for breaking changes
  (e.g. cred.2's env-var removal) is a clear CHANGELOG "BREAKING" note, not a deprecation shim, and this
  project is pre-1.0 and still iterating rapidly; a CHANGELOG migration note + README mode-table entry
  is the proportionate fix, added to doc scope below.
- Decisions logged (DX phase): 1 mechanical fix (non-TTY check) folded into Implementation Tasks; the
  "adopt a compat shim" idea considered and declined as disproportionate for this project's stage,
  noted here rather than as a formal audit-trail row (informational judgment call, not a structural
  decision with a rejected structural alternative).

## Implementation Tasks

Synthesized from all three review phases. Each task derives from a specific finding above.

- [x] **T1 (P1, human: ~1hr / CC: ~10min)** — `docker/entrypoint.sh` — add the new `cockpit)` case:
      `check_api_key`; `shift`; non-TTY check (`[ -t 1 ] || { echo ...; exit 1; }`); export
      `AGENTD_MANAGEMENT_ENABLED=true` + `AGENTD_MANAGEMENT_PORT`; background `agentd` with the new
      cockpit config; `trap` SIGTERM/SIGINT immediately; readiness wait polling `/agents/system` OR
      `${_MGMT_URL}/healthz` with a `kill -0 "$AGENTD_PID"` liveness check each iteration; on success,
      `set +e; agentctl watch "$@"; rc=$?; set -e`; kill/wait `$AGENTD_PID`; also kill the
      `agentctl watch` PID if still present (mitigates the `docker stop`-to-TUI gap); `exit "$rc"`.
      - Surfaced by: CEO Dual-voice (exec bug), Eng Dual-voice (Approach D, set -e bug, shift bug,
        liveness check), DX Dual-voice (non-TTY check).
      - Files: `docker/entrypoint.sh`
      - Verify: manual `docker run --privileged -it agentos:full` (FUSE path) and
        `docker run -it agentos:full` (HTTP fallback path) both boot the cockpit; `docker run --rm
        agentos:full` (no `-it`) exits immediately with the non-TTY message instead of hanging.
- [x] **T2 (P1, human: ~1-2hrs / CC: ~15min)** — `agentd/src/config.rs` — add
      `[scheduler] allow_empty_agents` (or equivalent name), relax `Config::agent_configs()`'s
      `(None, true)` arm to return `Ok(vec![])` when the flag is set, else keep the existing bail.
      - Surfaced by: Eng Dual-voice (zero-agent config can't boot — critical, confirmed by direct code
        read at `config.rs:338-354` and `main.rs:119`).
      - Files: `agentd/src/config.rs`
      - Verify: new unit tests `agent_configs_allows_empty_when_opted_in` +
        `agent_configs_still_rejects_empty_by_default`; `cargo test -p agentd`.
- [x] **T3 (P1, human: ~30min / CC: ~10min)** — `Dockerfile` — flip `CMD ["shell"]` → `CMD ["cockpit"]`
      (line 80).
      - Surfaced by: CEO Premise Decision (USER CHALLENGE #1, resolved).
      - Files: `Dockerfile`
      - Verify: `docker build` succeeds; `docker inspect` shows the new default `Cmd`.
- [x] **T4 (P1, human: ~30min / CC: ~5min)** — new `docker/cockpit.toml` — minimal config: scheduler +
      management block, `allow_empty_agents = true`, zero `[[agents]]` entries.
      - Surfaced by: CEO 0D (Decision #9, cold-start config).
      - Files: `docker/cockpit.toml` (new), `Dockerfile` (COPY it alongside `docker/agent.toml`/
        `docker/agents.toml`)
      - Verify: `agentd docker/cockpit.toml` parses and starts cleanly (with T2 landed).
- [x] **T5 (P2, human: ~30min / CC: ~5min)** — `docker compose config` assertion test proving the CMD
      flip doesn't affect `cos`/`agent`'s resolved command (Codex noted `docker compose config` proves
      the YAML merge, not the built image's `CMD` — frame the test/doc claim accordingly: it protects
      against someone removing the explicit `command:` lines, not against Docker's own guarantee).
      - Surfaced by: CEO 0E (Hour 6+), Eng Dual-voice (finding #6 — test doesn't test what it claims).
      - Files: none new — this may be a `Makefile`/CI step or a documented manual check, TBD at
        implementation time; not a Rust test (no compose-parsing crate in this repo).
      - Verify: `docker compose config` output for `cos`/`agent` still shows `command: [cos]`/
        `command: [agent]` after the `Dockerfile` change.
- [x] **T6 (P2, human: ~1hr / CC: ~15min)** — Docs: `docs/DEPLOYMENT.md` / `docs/RUNBOOK.md` /
      `README.md` — document the new `cockpit` mode; add a one-line "press `[n]` to spawn your first
      agent" pointer to the existing Spawn view; add an explicit mode table (`cockpit`, `shell`, `cos`,
      `agent`, `orchestrate`) to README's Docker quickstart section (not just operator docs), including
      the `docker run -it agentos:full shell` escape hatch shown prominently, not just mentioned.
      - Surfaced by: DX Dual-voice (findings #1, #3, #4).
      - Files: `docs/DEPLOYMENT.md`, `README.md`. **`docs/RUNBOOK.md` deliberately skipped** — it
        documents zero Docker `entrypoint.sh` modes today (not `cos`/`agent`/`orchestrate` either); a
        cockpit-only mention would be out of step with the rest of that file. `docs/DEPLOYMENT.md` is
        this repo's actual Docker-operations runbook and covers it.
      - Verify: manual doc review; README's Docker quickstart section shows the mode table.
- [x] **T7 (P2, human: ~15min / CC: ~5min)** — `CHANGELOG.md` — add a "BREAKING" migration note: the
      bare `docker run agentos:full` (no command) default changed from `shell` to `cockpit`; scripts/CI
      relying on the old fast-exit shell behavior should append `shell` explicitly.
      - Surfaced by: DX Dual-voice (Codex finding — migration risk).
      - Files: `CHANGELOG.md`
      - **Deferred to `/ship`**: this repo's merge-discipline rule (`docs/prompts/13-parallel-dev-rules.md`
        RULE 2) assigns the version bump + CHANGELOG entry at merge time against current `main`, not at
        branch-cut, to avoid the exact version-slot collision this repo hit on ux.0/cos-polish. The
        migration-note content is drafted and ready; only the version heading is pending.
- [x] **T8 (P3, human: ~15min / CC: ~5min)** — `docs/ROADMAP.md` — check off ux.9 on ship.
      - Files: `docs/ROADMAP.md`
      - **Deferred to `/ship`** for the same version-collision-avoidance reason as T7 (this repo's
        shipped-line convention embeds the version number, e.g. "shipped (v0.77.0)").
- [x] **T9 (P3, human: ~30min / CC: ~10min)** — `TODOS.md` — log: (a) `orchestrate)`'s pre-existing
      missing-`check_api_key` gap, (b) `orchestrate)`'s pre-existing `set -e`-swallows-cleanup bug
      (same root cause as T1's fix, worth a cheap backport), (c) the `docker stop`-may-not-cleanly-stop-
      the-TUI-process shared limitation, (d) the 5 deferred expansion candidates from CEO 0D
      (crash-loop supervision, welcome tour, reattach-command hint, `--template` passthrough, exit
      summary), (e) the 3-near-duplicate-readiness-check DRY note, (f) no CI workflow builds/runs the
      Docker image at all (this increment's tests are manual, not CI-gated).
      - Files: `TODOS.md`

## `make clippy-linux`

T2 touches `agentd/src/config.rs` — not Linux-gated specifically, but run the standard quality gate
(`cargo build && cargo clippy -- -D warnings && cargo test`) per repo convention before shipping.

---

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` (via `/autoplan`) | Scope & strategy | 1 | ISSUES_OPEN→RESOLVED | 9 decisions (1 user gate: USER CHALLENGE #1, superseded); dual voices converged on the `--privileged`/FUSE premise gap |
| Codex Review | dual-voice (CEO+Eng+DX, each phase) | Independent 2nd opinion, every phase | 3 | CLEAR | Converged with Claude subagent every phase; no unresolved cross-voice tension |
| Eng Review | `/plan-eng-review` (via `/autoplan`) | Architecture & tests (required) | 1 | ISSUES_OPEN→RESOLVED | 4 decisions (1 user gate: USER CHALLENGE #2 — Approach D supersedes Option A); 3 confirmed bugs fixed (set -e, arg shift, zero-agent config) |
| Design Review | N/A | No UI scope detected (0 keyword matches) | 0 | SKIPPED | — |
| DX Review | `/plan-devex-review` (via `/autoplan`) | Developer/operator experience gaps | 1 | ISSUES_OPEN→RESOLVED | 3 decisions; 1 previously-undiscovered regression fixed (non-interactive `docker run` would hang forever); stale registries rewritten |

**CODEX:** ran independently in all 3 phases (CEO, Eng, DX); found real, verified issues in every
phase, always by reading the actual repo code, not trusting the plan's prose.

**CROSS-MODEL:** Claude subagent and Codex converged independently (no shared context) on both User
Challenges — the `--privileged`/FUSE gap (CEO phase) and the case for Approach D over Option A (Eng
phase) — a strong signal in both cases, which is why both were escalated to the user rather than
auto-decided. In the DX phase, Claude found the concrete non-TTY hang bug that Codex did not
independently surface (Codex's parallel finding was the broader "migration risk" framing) — informational,
not a disagreement; both threads are now closed (see Implementation Tasks T1, T7).

**VERDICT:** CEO + ENG + DX CLEARED — 2 User Challenges resolved by the user (Option A→superseded by
Approach D), all mechanical findings fixed directly in this plan, ready to implement. `/plan-eng-review`
tag from the original plan's "Needs" line is satisfied (security-sensitive: the `--privileged`
requirement was removed entirely by Approach D, reducing rather than expanding the security surface
this increment touches).

NO UNRESOLVED DECISIONS


