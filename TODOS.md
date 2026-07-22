# TODOS

## Full-system audit (v0.86.2) — Open (2026-07-17)

Findings from the whole-system audit (`docs/AUDIT-v0.86.md`, prompt 14). Full evidence,
structural analysis, and the recommended build order live in that doc; this section is
the tracked-debt delta. `[new]` = not previously tracked; `[re-rate]` = existed at a
lower priority, raised here with reasoning. The build-order increment IDs referenced
below (`audit.1`, `ci.1`, `cap.1`, `cap.2`, `run.1`, `par.1/2`, `doc.1`, `sec.2`, etc.)
are defined in `docs/AUDIT-v0.86.md §6`.

### P0

- ~~**audit86-P0-1 (P0) [new] — Default QEMU boot config does not parse.**~~ **[FIXED in audit.1 (v0.87.0): `model_id` → `model`; `config_parse_all.rs` proves every checked-in spec parses.]**
  `distro/overlay/etc/agentd/agent.toml:16` uses `model_id`; `ModelConfig` (`config.rs:503-509`,
  `deny_unknown_fields`) has no such field — the key never existed. Reproduced: agentd
  exits with an unknown-field error at line 16. `/init:67` falls back to this file for any
  boot without an `agentd.config=` cmdline, so `make run`/`make test`/prebuilt-image boots
  panic PID-1. Production CoS (passes `agentd.config=cos.agents.toml`) is unaffected.
  Invisible because `qemu-boot.yml` is `workflow_dispatch`-only and already red. Fix: rename
  to `model`; add the parse-all test (audit86-P1-7) so the class can't recur. → `audit.1`.
- ~~**audit86-P0-2 (P0) [new] — Always-on CoS self-bricks in ~1–2 days.**~~ **[FIXED in ux.8′ (v0.89.0): rolling window via `[scheduler] budget_reset_interval` (0 = legacy lifetime) with wall-clock rebase (loop-top + 60s idle tick, division-based catch-up); an over-budget agent now DEFERS at admission instead of terminating; monotonic meter never zeroes (windowed = lifetime − anchor); `POST /api/v1/budget/reset` escape hatch; `BudgetReset` event; prod flipped to 24h window. Proven live in /qa (exhaust→defer→survive→auto-revive). 1455 tests.]**
  `cos.agents.toml:109-110` documents `global_token_budget` as a "hard daily spend ceiling",
  but `tokens_spent` is lifetime-monotonic (`scheduler.rs:434,698`) and checkpoint-restored
  (`checkpoint.rs:125`) with no reset path anywhere. The 24/7 flagship hits permanent
  `agent_admission_denied`; the only shipped remedy (`rm checkpoint.json`, `cos.agents.toml:54`)
  destroys all conversation state. Fix: budget reset window (`budget_reset_interval`
  re-basing `tokens_spent`) and/or a management-API budget-reset endpoint; decide the
  semantics first (see audit open question D2). → `ux.8′`.

### P1

- ~~**audit86-P1-1 (P1) [re-rate; = audit-C1 mechanism] — Per-agent budget only enforced under `ToolUse`.**~~ **[FIXED in ux.8′ (v0.89.0): per-agent windowed enforcement moved to admission (`enqueue_or_defer` defers, doesn't terminate) with a pre-inference gate + ToolUse backstop; text-only park/inject agents are now capped every turn. `drain_deferred` re-checks the per-agent cap on every completion (ship-adversarial F-A). Windowed, so it rolls over with the window.]**
  `agent/mod.rs:624-636` is the sole check site; `step_need_infer` (`:413-556`) checks
  `max_turns` but never the budget, so a text-only orchestrated agent accrues unbounded spend
  across the EndTurn→park→inject cycle. Fix: budget fail-fast at the top of `step_need_infer`. → `ux.8′`.
- **audit86-P1-2 (P1) [re-rate from P3] — flight.jsonl has no rotation anywhere.**
  `flight_recorder.rs:23-33` appends with no cap; nothing in `entrypoint.sh`, `overlay/init`,
  or the image rotates it. The otel sidecar *detects* rotation nobody performs. With
  streaming default-on, `InferenceStreamDelta` per SSE chunk ⇒ ~10–100 MB/day for a 24/7 CoS,
  filling `cos-data` in months. Fix: size-threshold copy-truncate self-rotation in
  `FlightRecorder::record` (the otel sentinel already survives copy-truncate). → `run.1`.
- **audit86-P1-3 (P1) [re-rate; = p5.2-ar-01/audit-C8] — `short_term` grows unbounded for parked agents.**
  `agent/mod.rs:67-68,475` only ever `extend`s; its sole consumer (distillation) runs
  post-run only (`scheduler.rs:811`), which a never-terminating orchestrated agent never
  reaches. Full-cloned into every checkpoint (interval 1 turn) and every snapshot tick ⇒
  unbounded RAM + linearly growing per-turn checkpoint writes. Fix: cap depth + threshold
  distillation for parked agents. → `run.1`.
- ~~**audit86-P1-4 (P1) [new] — CI tests only the agentd+agentctl packages.**~~ **[FIXED in ci.1 (v0.88.0): build-and-test is workspace-wide (build/clippy/test `--workspace --all-targets`); sandbox added to both aarch64 clippy lanes; verified by /qa (1430 tests).]**
  `ci.yml:44-61` pins `working-directory`; `surfaces` (96 tests incl. Linux-gated
  `agents_fs.rs`), `sandbox` (34), and `otel` (34) test suites are never executed in CI on
  any target, and their clippy is not reliably denied. The "1420 workspace tests" figure is
  a local-only guarantee. Fix: `cargo test/clippy --workspace --all-targets` from repo root. → `ci.1`.
- ~~**audit86-P1-5 (P1→ci.1 remainder) — entrypoint sed-rewrite pipeline has zero test coverage.**~~ **[audit.1: guards + dry-run hooks. ci.1 (v0.88.0): docker-smoke CI job runs cos/agent DRY_RUN_ONLY + binary probe + PR-#124-class negative fixture on every PR; the fixture exercises the extra-pattern concat branch (positive-form hit → `17:store_path` named). Still unexercised: the grep rc>=2 internal-error refusal branch (needs a grep malfunction to trigger — accepted residual). Known coverage limit (ship adversarial): the extra ERE only matches `*_path`/`*_dir` keys — a bare `path = "x"`/`dir = "x"` relative value without `./` escapes it; the class dies with the sed pipeline in par.2.]**
  `entrypoint.sh:242-247`'s `DRY_RUN_ONLY` hook (cred.2) is invoked by nothing; the v0.86.2
  guard (`:159`) covers 1 of 6 `cos)` rewrite rules and checks the *prompt* half of the
  v0.86.2 pair, not the *grant* half (`prefix = "./output"`), so the inverse desync passes.
  The `agent)` case (`:231-238`) has no guard at all. PR #124's bug class remains open on the
  other rules. Fix: negative-assertion block after each rewrite (fail if `"../docker/`,
  `"memory.redb"`, `"evidence.jsonl"`, `"egress-key.pkcs8"`, `"./output` survive) + a CI job
  invoking `DRY_RUN_ONLY=1`. → `ci.1` / `audit.1`.
- ~~**audit86-P1-6 (P1) [new] — Any `v*` tag on a stale commit silently publishes `:latest`.**~~ **[FIXED in ci.1 (v0.88.0): `scripts/release-guard.sh` (ancestry, tag==Cargo, monotonicity, per-caller fail-closed reuse over all three version manifests) called by both ci.yml's release-guard job (`--check image`) and release.yml's (`--check release`); 24-scenario harness committed as `scripts/test-release-guard.sh` (self-counting: `EXPECTED=24`), run by the harness-tests CI job on every push.]**
  `publish-docker` (`ci.yml:193-254`) and `release.yml` fire on any `v*` tag, check out the
  tag ref, and push `:latest`/`:full`/`:core` + release binaries with no
  `merge-base --is-ancestor origin/main` check and no tag==Cargo-version check (image tags
  derive from Cargo.toml, artifacts from `GITHUB_REF_NAME` — they can diverge). This is the
  v0.86.0-tag-behind-main incident with no guard. Fix: two-line ancestry + version-match
  check in publish-docker. → `ci.1`.
- ~~**audit86-P1-7 (P1) [new] — No parse check for any checked-in TOML outside `templates/`.**~~ **[FIXED in audit.1 (v0.87.0): `agentd/tests/config_parse_all.rs` — parse + validate + lowering + dup-id over docker/, agentd/, distro overlay, with negative-control fixtures.]**
  `template.rs` has catalogue tests, but nothing globs `docker/*.toml`, `agentd/*.toml`, or
  `distro/overlay/etc/agentd/*.toml`; audit86-P0-1 is the proof. Fix:
  `agentd/tests/config_parse_all.rs` asserting `Config` deserializes each (~1 h). → `audit.1`.
- **audit86-P1-8 (P1) [new] — "Relative paths fail closed" is false; path identity is string identity.**
  `capability.rs:27-29` documents fail-closed, but `normalize_path` (`:76-94`) strips `CurDir`,
  so a relative grant matches a relative request textually and CWD-blind; production dev-mode
  (`cos.agents.toml:247,375`) depends on the undocumented behavior. This is the v0.86.2 root
  cause. Fix: `AbsPathPrefix` newtype absolutizing at deserialization + at
  `required_capability_for` (`native.rs:88/128/169`, `main.rs:1345-1349`). → `cap.3`.
- **audit86-P1-9 (P1) [new] — Wrong-tier capability grants are silent no-ops (6 of 9 combos inert).**
  Agent-level `Credential` (`capability.rs:69`, "deferred") and `Net` (`:162`, returns true
  unconditionally) are decorative; HTTP MCP servers discard `capabilities`/`isolation`
  (`config.rs:604-628`, `main.rs:468-513`) and are *exempted* from `mcp_require_capabilities`
  (`main.rs:361`, `.filter(!is_http())`) with no warning — the operator's mandatory-sandbox
  switch does not cover remote tool servers. The Gmail-outage class. Fix: tier-legality
  validation in `Config::validate()` (hard error for HTTP servers carrying security fields —
  see open question D4; warn for inert agent-level grants) + a `CapabilitiesResolved` boot
  event logging each agent's and server's effective set. → `cap.1`.
- **audit86-P1-10 (P1) [re-rate; = cos-dev-02, was P2] — Spawn inheritance is all-or-nothing; Curator inherits live Gmail.**
  `SpawnConfig` (`config.rs:412-422`) has no capabilities field; `dispatch_spawn`
  (`scheduler.rs:1586`) does `parent_cap_set.clone()`. The Curator (processes
  attacker-influenceable email text daily) inherits `Credential{Google}`, so a prompt-injection
  payload surviving into curation can call live Gmail through the broker with every hardening
  layer cooperating. Post-cred.6 the broker made capability scoping load-bearing; this makes
  it decorative for children. The `cos.agents.toml:438-446` "inbox caps: Mcp{google_oauth}
  only" comment is fiction. Fix: `capabilities: Option<Vec<Capability>>` on `SpawnConfig`,
  validated ⊆ parent (subset check mandatory or the field becomes an escalation vector); merge
  cos-polish-adv-F2 + F5 (`max_turns` passthrough, `scheduler.rs:1597`). → `cap.2`.

### P2

- **ci.2 (P2) [named deferral from ci.1] — Broker→oauth_mcp→provider fake-provider E2E.**
  The nightly E2E (ci.1/3b) exercises agentd→mock-Anthropic only; the credential-broker
  seam (CredentialGateway → oauth_mcp sidecar → provider token flow, cred.3–cred.7) has
  no scheduled empirical signal. Scope: fake OAuth provider + fake API sidecar in the
  harness, containerized broker round-trip, flight/receipt asserts. Also fold in
  (ship adversarial F10): sidecar self-tests currently run against the git CHECKOUT's
  python3, not the shipped image — a Dockerfile COPY omission or image-python
  incompatibility ships undetected; run them inside `agentos:full` here. This named
  entry replaces the ci.1 plan's "after smoke proves stable" never-trigger
  (`docs/plans/ci.1-ci-tests-the-artifact.md` §NOT-in-scope).
- **ux.8-ar-01 (P2) [new, ux.8′ /review] — Deferred-drain has no per-agent budget reservation / fair aging.**
  On a window rollover `drain_deferred` admits by priority/slot count while the freshly-reset
  window budget is shared — the first-admitted requests can consume the whole new window, so under
  a tight window with many agents a lower-priority deferred agent can repeat-starve every window
  (`scheduler.rs` drain_deferred admit loop). Low impact for the 3-agent CoS with a 50M window;
  becomes real as agent count grows or the window tightens. Fix: per-agent budget reservation or
  fair-aging on the drain. Deferred by user decision at the ux.8′ review gate.
- **ux.11a-ar-01 (P3) [new, ux.11a /review] — SetBudget/ResetBudget 200 before the change is durable.**
  `POST /api/v1/budget/set` (and the pre-existing `/budget/reset`) reply OK after scheduler
  confirmation but before a checkpoint is written; checkpoints only fire at tool boundaries /
  shutdown. A crash in that window loses the operator's live budget change. Accepted for ux.11a
  (consistent with reset; forcing a checkpoint per mutation is heavier than the S scope). Fix
  option: schedule an async checkpoint after a confirmed budget mutation. Both models flagged; low.
- **ux.11a-ar-02 (P3) [new, ux.11a /review] — `/api/v1/budget/reset` skips agent-id validation.**
  ux.11a added empty/charset validation to `/budget/set`; the pre-existing `/budget/reset` still
  dispatches any `Agent(String)` (empty/malformed → scheduler traffic → misleading 404 not 400).
  Mirror the set-path validation onto reset. Pre-existing (out of ux.11a scope); cheap. (Codex.)
- **ux.11b-ar-01 (P3) [new, ux.11b-substrate] — park/resume is not a run-segment boundary in v1.**
  v1 models a segment as entry→terminal; an agent that parks (orchestrated `Waiting`, or `pending_approvals`
  approval-park) stays in ONE open segment until it finally terminates (approval-park still increments
  `approvals_count` on the open segment — G6). This is correct for the CoS daily children (spawn→terminal,
  the runs that matter) but means an orchestrate-REPL agent shows one long segment, not one-per-turn. Fix
  (deferred): close on park + reopen (`resume`) on inject/approval-resolve. Design-doc D3 wanted this;
  descoped from the substrate to keep the wiring bounded. NOTE when implementing (ship review): once park
  closes segments, the config-seed loop must SKIP agents restored into `waiting`/`pending_approvals` (else
  restart mints phantom `running` segments for idle parked agents), and inject/approval-resolve must emit
  the `resume` open. Shutdown already closes remaining native segments as `interrupted`, so nothing leaks
  `running` across a clean stop.
- **ux.11b-ar-03 (P2) [new, ux.11b-substrate /review] — `runs.redb` has no retention/prune; `list()` is O(n).**
  `RunsStore::list` full-scans + deserializes every record per query, and nothing prunes old runs, so an
  always-on CoS accreting inbox+curator child runs daily grows `runs.redb` monotonically and slows every
  `runs_query` / `GET /api/v1/runs` over months (both models, ship review — the flight.jsonl-no-rotation
  class). Low impact at v1 volume (~1k runs/year, list limit-capped at 100 results), but fix before it
  matters: age/count retention prune in the writer + a time-indexed table so from/to/limit avoid a full
  scan. (Companion to audit86-P1-2 flight rotation → run.1.)
- **ux.11b-ar-02 (P3) [new, ux.11b-substrate] — FUSE `/agents/runs` not shipped.**
  ux.11b-substrate ships the read surfaces the CoS + operators use: the `runs_query` native tool and
  `GET /api/v1/runs`. The FUSE `/agents/runs` mirror (G7) needs a `RunsAccess` trait in `surfaces` +
  threading through `AgentsFs::new`; deferred as additive. Add when a FUSE consumer needs it.
- **ux.11a-ar-03 (P3) [new, ux.11a /ship] — universal-tier `windowed_spent` always 0 in the snapshot.**
  Universal-tier agents are proxy-metered, not AgentTask-metered, so `update_snapshot` hardcodes
  `windowed_spent: 0` (mirroring the pre-existing `context_tokens: 0`). The TUI/FUSE can therefore show
  `0/<budget>` for a universal agent whose proxy budget is actually depleted (Codex ship review). Not a
  regression (consistent with the already-shipped context_tokens=0), but misleading. Fix: surface the
  proxy `token_budget_remaining` atomic into the universal snapshot's windowed_spent, or omit the field
  for universal agents. Needs proxy-spend plumbing → ux.11b/later scope.
- **ux.8-ar-02 (P3) [new, ux.8′ /ship] — `MaxTokens` self-terminates regardless of `budget_resettable`.**
  The provider `MaxTokens` stop reason terminates the agent even under an active budget window, so a
  single over-long completion can end an otherwise-resumable agent (`agent/mod.rs`). Pre-existing
  (predates ux.8′) and orthogonal to the budget-window mechanism, so left out of the P0-2 hotfix.
  Fix: treat `MaxTokens` as a recoverable turn boundary (continue/retry with a lower cap) rather than
  a terminal outcome. Low impact — surfaces only on pathological single-turn output.
  `scheduler.rs:183-184,243-247`, `agent/mod.rs:206-221`. Since the CoS never terminates, a
  `docker compose pull && up` shipping a new prompt/budget/model silently changes nothing
  until the checkpoint is deleted — image upgrades never reach the always-on agent. Fix:
  fingerprint the TOML config into the checkpoint; warn/opt-in re-seed on divergence
  (see open question D3). → `run.1`.
- **audit86-P2-2 (P2) [new] — Checkpoint deleted immediately after restore; a crash-loop erases everything.**
  `main.rs:1127-1129` removes `checkpoint.json` before any new save exists, so a second
  crash (OOM/bad config) permanently loses the CoS conversation + tokens + parked children.
  Fix: rename `.restored`, delete only after the first successful `checkpoint_all`. → `run.1`.
- **audit86-P2-3 (P2) [new] — Missed cron fire silently skips the daily brief.**
  `cron_mcp.py:196,247,253` keeps `_NEXT_FIRE_TS` in process memory only; a crash/restart
  spanning a scheduled fire drops the brief (interval mode phase-shifts). Fix: persist
  last-fired under `/data`; fire-on-startup-if-missed. → `run.1`.
- **audit86-P2-4 (P2) [new] — evidence.jsonl cannot rotate safely.**
  Hash chain (`evidence.rs:4-5,38`); `resume_chain` re-hashes the whole file at every boot
  (`:184-197`, O(file) forever); rename-rotate is silently ignored; archive-and-restart
  drops to GENESIS with no segment manifest. Fix: chain-aware segment rotation (new segment's
  first `chain_prev_hash` = prior segment's last hash); teach `agentctl verify` non-genesis
  starts. → `run.1`.
- **audit86-P2-5 (P2) [new] — Terminal root agents + several scheduler maps leak/grow monotonically.**
  Terminal agents are removed from `state.agents` only in the child branch (`scheduler.rs:959`);
  `parent_map` is insert-only and checkpointed (`:1663,1934,2472`), as are
  `spawn_depths`/`mailboxes`/`streamed_agents`/`outcomes`. A never-terminating deployment
  with periodic operator spawns leaks whole conversations in RAM and grows checkpoint.json.
  Fix: clear all per-agent map entries in `handle_agent_terminal` (`:941`). → `run.1`.
- **audit86-P2-6 (P2) [new] — At-least-once turn replay can duplicate irreversible actions.**
  `scheduler.rs:718-733`: a crash after tool execution but before the boundary checkpoint
  re-runs the turn. CoS blast radius today: duplicate `ops:briefs` entry (append-only, ignores
  the key, `cos.agents.toml:359`); a duplicate outbound email if L1 send is enabled. Fix:
  keyed/idempotent brief writes; idempotency keys before any irreversible tool ships. → `run.1`.
- **audit86-P2-7 (P2) [new] — Universal-tier inference spend excluded from the global budget.**
  `egress.rs:127-165` never increments `state.tokens_spent` (only writers: `scheduler.rs:698,885`),
  so "always accounted" is violated for an entire tier. Fix: plumb a shared counter from the
  proxy into scheduler state. → `ux.8′`.
- **audit86-P2-8 (P2) [new] — Docker base images pinned by mutable tag, not digest.**
  `Dockerfile:3,55`, `Dockerfile.semantic-kb-mcp:1`. Fix: `@sha256` digests + Dependabot. → `sec.2`.
- **audit86-P2-9 (P2) [new] — Embedding key bypasses the broker; librarian-semantic template is live-broken.**
  `OPENAI_API_KEY` provisioned raw via compose (`docker-compose.yml:155-156`), the exact
  thing ROADMAP:72-73 said must not happen. `templates/librarian-semantic.template.toml:10-12`
  gates on `VOYAGE_API_KEY` (test-enforced, `template.rs:1081-1082`) while its sidecar requires
  `OPENAI_API_KEY` (`semantic_kb_mcp.py:161`): export VOYAGE ⇒ every `kb_put` fails; export
  OPENAI only ⇒ template hidden. Fix the gate now (`audit.1`); `EMBED_PROVIDER`/`EMBED_API_URL`
  env triple + broker `Custom` provider as the increment (`eco.1`/future direction 3).
- **audit86-P2-10 (P2) [new] — Python sidecar self-tests never run in CI.**
  No workflow has a python step; oauth (30 checks incl. schema-drift), cron (6), fs_watch (6),
  etc. run only locally. Fix: `for f in docker/*_mcp.py; do python3 "$f" --test; done`. → `ci.1`.
- **audit86-P2-11 (P2) [new] — Published images are never executed before push.**
  `publish-docker` builds and pushes without so much as `agentd --help`. Fix: image smoke run
  pre-push; longer term a compose-boot job asserting flight.jsonl shape. → `ci.1`.
- **audit86-P2-12 (P2) [new] — Shell env-sanitization denylist hand-duplicated across boot scripts.**
  `entrypoint.sh:16-18` ≡ `overlay/init:49-51` verbatim; `shell_mcp.py:39` a third larger
  variant; zero sync checks (a security addition to one won't propagate). Fix: golden-diff CI
  test (a sourced lib is impossible — `init` is standalone busybox in an initramfs). → `par.1`.
- **audit86-P2-13 (P2) [new] — agentctl matches flight-event kinds by raw string.**
  `inspector.rs:48-54`, `reader.rs:340-341`, `converse.rs:289`, `orchestrate.rs:164`,
  `views.rs:1194` — despite depending on the agentd crate; a rename in `events.rs` compiles
  and silently blanks TUI filters/streams. Fix: export `EventKind::as_str()` and use it, or
  copy the otel exhaustiveness-test pattern. → `par.1`.
- **audit86-P2-14 (P2) [new] — QEMU CoS fork silently missing the entire memory-routing feature set.**
  `distro/overlay/etc/agentd/cos.agents.toml` lacks the `semantic-kb` server, `mail:raw`
  segment/caps, and the dedup prompt step — production QEMU CoS runs without email dedup. The
  fork header's "Key differences" list doesn't mention it; cred.6 *was* mirrored into the fork,
  memory-routing was *not* — mirror discipline is luck, not process. Fix: declare in the header
  now; the normalized-diff parity test (`par.1`) is the real fix. → `par.1`.
- **audit86-P2-15 (P2) [re-rate] — Priority bumps for existing items given broker-live + cockpit-default state:**
  `ux.0b-ar-02` P3→P2 (`allow_non_loopback` unscoped bypass of an unauthenticated API that
  is now default-on, with a bare-Linux quickstart documented); `ux.9-ar-05` P3→P2 (no CI job
  builds the Docker image — Docker is now the primary distribution and two ux.9 criticals were
  caught only by manual local runs — folded into `ci.1`); `orch.2-ar-03` P3→P2 (SSE broadcast
  lag drops `OrchestratorTurnComplete`; per-token `InferenceStreamDelta` on the same 1024-slot
  channel made "buffer fills" the expected case, not a rare one — fold with the ux.1 SSE-channel
  TODO). `cos-polish-adv-F2` P3→P2 (same root cause + fix site as audit86-P1-10; merge into `cap.2`).

### P3 and reconciliation

- **audit86-P3-1 (P3) [new] — UTF-8 panic in the credential gateway.**
  `credential/mod.rs:383,395,415` byte-slice `token_url[..len().min(64)]` can split a
  multi-byte char and panic in the request handler (reachable via a malformed operator
  secrets file). Char-boundary truncation helper. → `sec.2`/`cap.3`.
- **audit86-P3-2 (P3) [new] — `oauth_mcp.py:733` interpolates a raw exception** into the error
  response; sibling `:422` correctly scrubs to `type(exc).__name__`. Latent leak channel.
- **audit86-P3-3 (P3) [new] — `.expect` on EffectResult agent lookup** (`scheduler.rs:687,724`)
  holds today but `panic = "abort"` in release makes any future break (e.g. a `Kill` command)
  whole-runtime death. Defensive `let Some(..) else { record; continue }`.
- **audit86-P3-4 (P3) [new] — cred.7 added `credential_health` without a FORMAT_VERSION bump**
  (`checkpoint.rs:148`; violates CONVENTIONS:225-226) — silent downgrade drop. Bump to 5 in
  the next checkpoint-touching PR (`run.1`).
- **audit86-P3-5 (P3) [new] — Misc single-source gaps:** crash-orphaned `checkpoint.json.*.tmp`
  never swept (`checkpoint.rs:172-179`); `docker/cockpit.toml` lacks the `[memory]` eviction
  block both cos configs have; port 7999 duplicated across ~10 files (`config.rs:222` is the
  authority — the agentctl trio could share the constant today); port 8020/`semantic-kb-mcp`
  URL not rewritten by the `cos)` sed (breaks `docker run` outside compose); template gating
  hardcoded in `entrypoint.sh:176,180` duplicating `gated_requires`; `distro/Makefile:144-146`
  copies 7 named MCP files vs Dockerfile's wildcard (new `*_mcp.py` silently absent from QEMU);
  `agent.toml:76-81` comment places `capabilities` under `[tools]` (parse error if followed).
- **audit86-P3-6 (P3) [new] — Documentation drift** (one confirmed instance per doc, full table
  in `docs/AUDIT-v0.86.md §2`): THREAT_MODEL scoped to v0.62 with §1.2 contradicting the broker;
  `DEPLOYMENT.md:203,396,402` + `cos-guide.html:872` jq probes select `mcp_tool_called` (no such
  event kind — real is `tool_call`); `RUNBOOK.md:268` "set BRAVE_API_KEY where you launch agentd"
  (nullified by `env_clear()`); `agentd/README.md:63` streaming default documented backwards;
  CONVENTIONS event table missing `mcp_passenv_forwarded` + FUSE table missing 5 files & all of
  `/agents/system/`; README self-contradicts on version. → `doc.1` + CONVENTIONS-completeness test.
- ~~**Reconciliation — fixed-but-not-struck TODOS entries (verify, then move to Completed):**~~ **[DONE in audit.1 (2026-07-17): all 6 entries verified against code and struck in place — see each entry's FIXED/SUPERSEDED annotation.]**
  `audit-S1`/`audit-S2` (`TODOS.md:387,392`; closed in cred.3.1 / v0.61.0 as cred.3-ar-S1/S2);
  `F-012` (`:1583`; = F-05/audit-C3, fsync landed v0.70.0 — corroborated by the ops pass at
  `checkpoint.rs:36-65,187-204`); `F-015` (`:1588`; `extra_env` blocklist enforced at
  `mcp.rs:144` — corroborated by the security pass); `cred.3-ar-02` (`:1112`; superseded by
  cred.5 surface + cred.7 health state); `cred.3.1-adv-01` (`:1297`; `load_from_disk` startup
  reload ar-06 + cred.7 checkpoint persistence). Left in place pending per-item verification
  rather than blind-struck; that verification is part of `audit.1`.

## ux.1 — Open (deferred from CEO review, 2026-07-13)

- **D1 "one unified screen" scope decision** (P3): `docs/ROADMAP.md:1089` / `docs/plans/
  ux-cockpit.md:49` record a 2026-07-10 "Locked decision" that the cockpit should be one
  unified screen ("not more `[key]` tabs"), but three increments running (ux.0's prerequisite
  refactor, ux.9, ux.2a) all continued the full-screen tab model instead. ux.1 is the first to
  partially honor D1 — a permanent chat rail on `Dashboard` only, per CEO-review pause
  resolution (dual-voice finding, both Claude subagent and Codex independently flagged the
  sequencing + layout drift). Decide whether D1 extends to the other 8 views (Topology,
  Memory, Spawn, Inspector, Approvals, Credentials, System, AgentDetail) or gets formally
  re-scoped to "Dashboard is the unified home screen; specialized views stay tabbed, reached
  from it." Leaving the written decision and actual practice diverged a 4th time just defers
  the same re-litigation onto the next cockpit increment. Depends on: none.
- **Unread/background-activity badge on the chat rail's target selector** (P3, from ux.1's
  T13 — explicitly deferred during build, not silently dropped): when a backgrounded
  target's `ConverseState` transitions (new streaming activity, turn completes) while the
  operator is looking at a different target, nothing in the border title signals it — the
  Design dual-voice review's proposed fix (`┤ → orchestrator ├ [scout-3: ●2]`, reusing the
  attention-summary-line glyph+count idiom already in `views.rs`) was never implemented.
  Requires new per-target "unseen" state in `ConverseState` (`agentctl/src/watch/converse.rs`)
  with reset semantics on retarget/focus — not just cosmetic, needs its own small design
  pass on exactly when the counter clears. Depends on: none.
- **`converse::dispatch()` blocks the whole TUI, not just the rail, for up to ~8s worst
  case** (P2, found by /review's adversarial pass): `Enter` calls `dispatch()` synchronously
  on the render/key-poll thread (`agentctl/src/watch/mod.rs`'s Enter handler) — it calls
  `source.load_snapshot()` (5s timeout, `source.rs`) then `source.spawn()` (3s) or
  `source.inject()` (500ms). This mirrors the EXISTING Approvals approve/deny pattern
  (same "blocking call on the main thread" architecture, documented at `mod.rs:328` as
  deliberate) but with much longer timeouts on what's now the cockpit's highest-frequency
  interaction (chat) instead of an occasional approval. Worst case: sending a chat message
  freezes the ENTIRE Dashboard — including already-streaming background conversations,
  redraws, and Ctrl-C — for several seconds. Proper fix needs an async/background-thread
  dispatch path with a channel-based result delivery back into `step()`, not a quick patch.
  Depends on: none, but touches the Option B event-loop architecture (ux.0) — read
  `mod.rs`'s Option B doc comment before starting.
- **Shared SSE broadcast channel (`agentd/src/main.rs:114`, capacity 1024) now carries
  per-token delta traffic on top of every other event kind** (P2, found by /review's
  adversarial pass): `EventKind::InferenceStreamDelta` fires once per streamed chunk on
  the hot inference path — orders of magnitude higher frequency than any event kind that
  existed when this channel's capacity was chosen. A lagged SSE consumer (slow network, or
  blocked by the dispatch-freeze TODO above) drops a *contiguous* range of buffered
  messages indiscriminately — not just deltas, but potentially `agent_failed`,
  `orchestrator_exited`, `approval_required`, etc. sharing the same channel. Wasn't a live
  risk before ux.1 (event volume was low); is now. Needs either a capacity bump (cheap,
  partial mitigation) or a dedicated channel for high-frequency delta events so a burst
  from one streaming agent can't starve delivery of higher-stakes events to every
  subscriber. Depends on: none.
- **Retargeting `r` onto a non-orchestrated agent produces a slow, generic error** (P3,
  found by /review's adversarial pass): the collision guard rejects the spawn attempt
  server-side (`agentd/src/scheduler.rs`, "agent ID already in use") without ever sending
  `confirm_tx`, so the HTTP spawn endpoint times out after its full 2s window
  (`agentd/src/management.rs`) and returns a generic 503 "timed out waiting for agent
  creation" instead of the actual collision reason — on top of the dispatch-freeze TODO
  above, a real if low-severity papercut for a fleet with non-orchestrated agents.
  Depends on: none.
- **`agentctl orchestrate`'s CLI still truncates replies at 512 chars** (P1, found by
  `/ship`'s Step 8 plan-completion audit): the plan (`docs/plans/ux.1-converse.md` Pass 5)
  explicitly decided to fix this as a byproduct of the streaming-delta work, by having
  `orchestrate.rs`'s `drain_until_turn_complete` consume `InferenceStreamDelta` events for
  display instead of relying on the server-capped `orchestrator_turn_complete.answer`
  field. What actually shipped (T10) only adds a cheap early-continue on delta events —
  they're skipped, not accumulated — so the CLI REPL still shows the same 512-char-capped
  reply for any operator running against a non-colocated `agentd` as before this branch.
  CHANGELOG.md/CLAUDE.md briefly claimed this was fixed; corrected in the same commit that
  files this TODO. Depends on: none.
- **`agentctl orchestrate`'s CLI never adopted `converse.rs`'s shared `dispatch()`/
  `on_flight_event()` helpers** (P2, found by `/ship`'s Step 8 plan-completion audit): T3
  in the plan called for `orchestrate.rs` to be refactored onto the new shared helper (a
  "net LOC decrease" per the plan's architecture section) instead of keeping its own
  parallel spawn/inject + 4-event field-path logic. `converse.rs` was built as the shared
  module and is used by the Dashboard rail, but `orchestrate.rs` was never ported onto it
  — the two now have duplicated, independently-maintained copies of the same
  spawn-or-resume and field-path-lookup logic, with only the `DEFAULT_MAX_TURNS` constant
  actually shared. Depends on: none.
- **`record_streamed()` takes a blocking `Mutex<File>` write once per streamed chunk on
  the hot inference path** (P2, found by `/ship`'s Step 9 performance specialist):
  `scheduler.rs`'s `print_fut` calls `FlightRecorder::record_streamed()` inside the
  per-chunk `while let Some(chunk) = rx.recv().await` loop, and `record_streamed()` does a
  synchronous, blocking `writeln!` under a process-wide `std::sync::Mutex<File>` directly
  on the tokio worker thread (no `spawn_blocking`). Every other event kind fires once per
  turn/tool-call; this one fires once per SSE chunk — hundreds of blocking mutex-acquire +
  syscall pairs per streamed reply instead of one. Since the mutex is shared across every
  agent via the single `Arc<FlightRecorder>`, concurrently streaming agents (multi-agent
  mode) now serialize on this lock at token frequency — one agent's chunk write can stall
  other agents' worker threads. Related to, but distinct from, the already-filed
  broadcast-channel-capacity TODO above (that's about channel backpressure; this is about
  the disk-write path itself). Needs either batching/coalescing writes or moving the write
  off the async path via `tokio::task::spawn_blocking`. Depends on: none.
- **`render_converse_rail()` re-sanitizes and rebuilds the full transcript on every
  redraw tick** (P3, found by `/ship`'s Step 9 performance specialist): it iterates all of
  `state.history` (up to `MAX_HISTORY_TURNS=200`) plus the full `current_reply` (up to
  `CURRENT_REPLY_CAP_BYTES=64KiB`) and re-runs `sanitize()` (an allocating filter) on all
  of it every render, rather than caching already-rendered lines and only processing the
  newly-appended tail. Redraws are coalesced to roughly one per ~30ms tick, and
  `InferenceStreamDelta` now drives a redraw on effectively every tick during an active
  stream — a long-running, near-full-history rail could re-scan/re-allocate a
  non-trivial amount of text ~30x/second instead of amortizing the cost. Depends on: none.
- **`dispatch()` can't tell "never spawned" from "exists and still running"** (P1, found
  by `/ship`'s Step 9 red team pass): `converse.rs`'s `dispatch()` only checks whether the
  target's snapshot status is exactly `"waiting"` — everything else (including "agent
  exists but is mid-turn, not yet parked back to waiting") falls into the `spawn()`
  branch. The server (`scheduler.rs`'s spawn-collision guard) correctly rejects a spawn
  for an id already in `state.agents`/`state.outcomes`, but that rejection happens before
  the oneshot `confirm_tx` is ever wired up, so `management.rs`'s `/api/v1/spawn` handler
  sees the confirmation channel close and returns a generic 503 "scheduler closed
  confirmation channel" instead of the real, actionable server-side reason ("agent ID
  already in use"). Reachable in normal operation: reconnecting/restarting `agentctl
  watch` while a previously-dispatched orchestrated agent is still mid-turn (the fresh
  client-side `ConverseState` starts `Idle`, so the double-submit guard doesn't block the
  retry), or the client's own 30s `DISPATCH_TIMEOUT` false-positiving on a legitimately
  slow turn and the operator's retry racing the still-running original into this exact
  collision path — in both cases the new message is silently dropped behind a confusing
  error while the original turn continues untouched. Fix: when the target id already
  appears in the snapshot at all (regardless of "waiting" status), try `inject()` first
  and only fall back to `spawn()` when the id is genuinely absent; consider also not
  dropping `req.confirm_tx` silently in the server's collision guard so the real reason
  reaches the HTTP caller. Depends on: none.
- **Cross-turn contamination guard doesn't survive an `agentd` crash/restart mid-stream**
  (P2, found by `/ship`'s Step 9 red team pass): `append_delta()`'s guard rejects a delta
  only when its `turn_seq` differs from the turn currently accumulating; `chunk_seq` is
  local to one invocation of `print_fut` and always restarts at 0. `turn_seq` is `agentd`'s
  persisted per-agent turn counter, restored verbatim from checkpoint on crash/restart —
  if `agentd` crashes mid-turn N and resumes, the resumed process redispatches inference
  for the SAME `turn_seq=N` with `chunk_seq` starting again at 0. A client that survived
  the disconnect without giving up on that turn accepts the new `chunk_seq=0` chunk as a
  continuation (0 is neither a duplicate nor a detected gap), silently splicing the fresh
  post-restart reply onto the stale pre-crash partial text with no gap-note or visual
  break. Fix: make chunk identity unique per physical inference attempt (e.g. an
  `attempt` id set once at scheduler startup), or reset `current_turn_seq`/`current_reply`
  whenever `chunk_seq` goes backward to 0 mid-stream. Depends on: none.
- **`ConverseView.targets` grows unbounded for the life of the `agentctl watch` process**
  (P3, found by `/ship`'s Step 9 red team pass): `retarget()` and the Enter-key handler
  both insert a `ConverseState` (up to `MAX_HISTORY_TURNS=200` turns) via
  `entry().or_default()` for any agent id the operator ever retargets to, but nothing ever
  evicts an entry once the corresponding agent leaves the fleet (completes, fails, is
  reaped). In this project's typical usage (a CoS/orchestrator spawning many short-lived
  scout/worker agents over a long-running dashboard session), briefly inspecting each
  ephemeral agent accumulates one permanent `ConverseState` per agent id for the life of
  the TUI process. Fix: evict target entries whose agent id no longer appears in the
  latest snapshot and isn't `active_target`, or cap tracked targets with an LRU policy.
  Depends on: none.
- **Terminal-event handlers (`orchestrator_turn_complete`/`agent_failed`/
  `orchestrator_exited`) have no turn-identity guard, unlike `append_delta`'s `turn_seq`
  check** (P1, found by `/ship`'s Step 11 adversarial pass, Claude subagent): a stale,
  delayed completion event for a PREVIOUS turn can flush and corrupt a DIFFERENT, currently
  in-flight turn for the same `agent_id`. Reachable: (1) the 30s dead-air timeout fires on
  a legitimately slow-first-token turn A and flushes it to `Idle`; (2) the operator
  retries, a fresh `load_snapshot()` shows the agent already back to `"waiting"` (turn A
  actually completed server-side, the SSE event just lagged — plausible given the
  already-filed broadcast-channel TODO), so `inject()` starts turn B; (3) turn A's stale
  `orchestrator_turn_complete` finally arrives, tagged with the same `agent_id` — the
  handler has no way to tell it belongs to a different, already-abandoned turn, so it
  `flush()`es, silently destroying turn B's already-accumulated live content AND falsely
  disarming the double-submit guard (`phase` resets to `Idle`) mid-turn-B. Root cause is
  partly server-side: `scheduler.rs` records these three event kinds with `turn: None` and
  no `turn_seq` in `data`, unlike `InferenceStreamDelta` — a full fix needs the server to
  thread a turn identifier onto these three kinds the same way it already does for deltas.
  Depends on: none, but touches `agentd`'s event schema for 3 pre-existing event kinds
  (orch.1/orch.2), not just the ux.1 diff.
- **`resolved_id` merge in the Enter handler can clobber an unrelated, already-in-flight
  target's state** (P3, found by `/ship`'s Step 11 adversarial pass, Claude subagent —
  classified INVESTIGATE, narrow): if `resolved_id` (from `HttpSource::spawn`'s documented
  `"operator-agent"` id-collision fallback) already had its own independent
  `ConverseState` with an in-flight turn, the Enter handler's `entry(resolved_id)
  .or_default()` + unconditional `phase = Dispatching` forcibly resets that unrelated
  turn's phase/`last_event_at`, potentially interfering with its own double-submit guard
  and dead-air timeout. Only reachable through the id-fallback quirk plus a coincidental
  pre-existing entry for that literal id — narrow, not confirmed to manifest in practice.
  Depends on: none.
- **`flight.jsonl` growth rate amplified by orders of magnitude with no rotation
  mechanism** (P3, found by `/ship`'s Step 11 adversarial pass, Claude subagent —
  classified INVESTIGATE; pre-existing gap, accelerated by this branch): before ux.1,
  `flight.jsonl` grew roughly one line per turn/tool-call; after ux.1 it grows one line per
  streamed token CHUNK (each individually capped at 256 bytes, but the line COUNT is
  unbounded and proportional to reply length). No log rotation/truncation exists at all.
  Related to, but distinct from, the already-filed lock-contention TODO (that one is about
  write latency; this is about unbounded disk consumption over a session's lifetime).
  Verified the OTEL sidecar does NOT amplify this into telemetry — `InferenceStreamDelta`
  is deliberately excluded from span mapping (`otel/tests/event_kind_coverage.rs`) to avoid
  a span-per-token explosion. Depends on: none.

## ux.9 — Open (deferred from build + /review adversarial pass, 2026-07-12)

Cockpit mode (zero-agent `cockpit` entrypoint, now the Dockerfile default). `/review` dispatched
Testing/Maintainability/Security specialists + a Claude adversarial subagent + Codex adversarial
review in parallel; Codex found one CRITICAL bug (checkpoint bleed-through) and the Claude
subagent independently found one deterministic bug (terminal corruption on every SIGTERM) that
directly contradicted this increment's own earlier "verified end-to-end, docker stop cleanly
exits" claim — that claim had only exercised `--plain` mode (no raw mode, nothing to corrupt),
never the real ratatui TUI. Both were confirmed by direct code read + a live Docker reproduction,
fixed, and re-verified. Ten follow-ups total, two CRITICAL (fixed), rest are real but
proportionate follow-ons:

- **FIXED (CRITICAL, found by Codex) — stale `/workspace/checkpoint.json` silently resurrected
  agents despite `cockpit.toml`'s zero-agent config.** `agentd/src/scheduler.rs:251-260`
  unconditionally restores any checkpointed agent not present in the TOML's agent list — with
  zero agents in the TOML, *every* checkpointed agent falls into that "remaining" bucket and gets
  restored. `cockpit)` ran from `/workspace` (the operator's bind-mounted files directory, per
  `print_banner`'s own "mount your files here"), so a `checkpoint.json` left there by a prior
  `demo`/`run`/cockpit session on the same mount would silently resume and start spending tokens
  again on the next `docker run`, contradicting the whole design intent ("opening the cockpit
  shows the empty system state"). Fix: run from `/data` instead (matching `cos)`/`agent)`
  modes' existing pattern, which solved this exact problem the same way — `agent)` mode's comment
  literally says "no repo bind mount, no checkpoint bleed-through") + `rm -f
  /data/checkpoint.json` before each launch (matching `agent)` mode's "each launch starts fresh"
  rationale). Verified via a live Docker repro: planted a checkpoint with a `stale-agent` entry in
  a mounted `/workspace`, confirmed cockpit booted with `agents: (none)` and `/workspace` was
  untouched.
- **FIXED (CRITICAL/deterministic, found by Claude adversarial subagent) — every `docker
  stop`/SIGTERM against the real TUI (non-`--plain`) corrupted the operator's terminal.**
  `agentctl watch`'s TUI installed no SIGTERM/SIGINT handler of its own — only a panic hook and a
  `Drop` guard (`agentctl/src/watch/mod.rs`'s `TermGuard`). `cockpit)`'s trap sent a raw `kill(2)`
  SIGTERM directly to the `agentctl watch` process; the OS default disposition for an uncaught
  SIGTERM is immediate termination with no unwind, so neither the panic hook nor `Drop` ever ran,
  leaving the terminal stuck in raw mode + the alternate screen (needing `reset`/`stty sane` to
  recover). Fix, two parts: (1) `agentctl/src/watch/mod.rs` gained
  `install_shutdown_signal_handlers()` — a minimal `libc::signal()`-based SIGTERM/SIGINT handler
  that only sets an atomic flag (signal-safe), checked every ~30ms tick in `run_tui_loop`, letting
  the loop return normally so `TermGuard::drop()` restores the terminal before exit; new test
  `shutdown_signal_handler_sets_flag_on_sigterm_and_sigint`. (2) `docker/entrypoint.sh`'s trap now
  `wait`s on `$WATCH_PID` (not just `$AGENTD_PID`) before `exit 0` — this script is the
  container's PID 1, so without waiting, `exit 0` raced agentctl's ~30ms-later graceful shutdown
  and usually won, tearing down the whole container before the terminal-restore write could land.
  Verified via live `docker logs` byte-inspection: before the fix, `\x1b[?1049h` (enter alt
  screen) appeared with no matching `\x1b[?1049l` (leave); after both fixes, both appear plus the
  cursor-show sequence, and `docker stop` still completes in ~0.15-0.2s (no hang).
- **ux.9-ar-01 (P3) — `orchestrate)` mode never calls `check_api_key`.** Every other mode
  (`shell`, `demo`, `cos`, `agent`, and the new `cockpit`) calls it explicitly; `orchestrate)`
  was missed. An unset `ANTHROPIC_API_KEY` under `orchestrate` currently surfaces as an opaque
  `agentd`/Anthropic API error instead of the same actionable message every other mode gives.
  `docker/entrypoint.sh`'s `orchestrate)` case.
- **ux.9-ar-02 (P3) — `orchestrate)` mode's cold-start path doesn't guard `set -e` around the
  client invocation.** `docker/entrypoint.sh` (`agentctl orchestrate --url ... "$@"` in
  `orchestrate)`) is unguarded; any nonzero, non-signal exit from it aborts the script immediately
  under `set -e`, skipping the `kill`/`wait $AGENTD_PID` cleanup on the next two lines and leaking
  the backgrounded `agentd` process. `cockpit)`'s case fixes this for itself
  (`set +e; agentctl watch "$@"; rc=$?; set -e`) but the fix was not backported to `orchestrate)`.
- **ux.9-ar-04 (P3) — Three near-duplicate readiness-wait loops in `docker/entrypoint.sh`.**
  `demo)` polls `mountpoint -q /agents`; `orchestrate)` polls `curl -sf .../healthz` via
  `timeout 15 sh -c ...`; `cockpit)` polls both `/agents/system` and `.../healthz` with an added
  `kill -0` liveness check, hand-rolled as a `for` loop. Worth extracting a single shared
  `wait_for_agentd_ready()` helper someday — three copies is exactly the kind of DRY gap that
  drifts (the other two loops lack `cockpit)`'s stricter liveness check).
- **ux.9-ar-05 (P3) — No CI job builds the Docker image.** All of this increment's Docker-level
  verification (the checkpoint-restore bug, the terminal-corruption bug, the `curl` gap below)
  was found via local `docker build` + manual `docker run`, not CI — a broken
  `Dockerfile`/`entrypoint.sh` would not be caught until someone notices manually. Worth a CI job
  that at minimum runs `docker build --target runtime-core` and a smoke-test invocation. The new
  `make compose-config-check` target (ux.9) is similarly not wired into CI yet — manual-only.
- **ux.9-ar-06 (P2, found during build verification) — `curl` was never installed in the
  Docker image, silently breaking `orchestrate)` mode's cold-start healthz poll.** `Dockerfile`'s
  `runtime-core` stage ran `apk add --no-cache fuse3 bash jq` with no `curl`; `orchestrate)` has
  used `curl -sf .../healthz` since it shipped (v0.66.0). Discovered while manually verifying
  `cockpit)`'s own healthz-based readiness wait against a real built image — `curl: not found`.
  **Fixed in this increment** (added `curl` to the `apk add` line); `orchestrate)` gets the fix
  for free from the same image layer.
- **ux.9-ar-07 (P2, found by Codex, /review) — the Spawn view's `[n]` action can't inject into
  an unprivileged (HTTP-fallback) cockpit; it silently falls back to launching a second, separate
  `agentd` process instead.** `execute_pending_spawn` (`agentctl/src/watch/mod.rs`) only checks
  whether `/agents/control` (a FUSE path) exists; it never calls the `DataSource::spawn()`
  abstraction, which already has a working HTTP implementation
  (`HttpSource::spawn()` → `POST /api/v1/spawn`, `agentctl/src/watch/source.rs`). In HTTP-fallback
  cockpit (no `--privileged`), `/agents/control` never exists, so every spawn attempt takes the
  `FellBackToExec` branch: writes a temp TOML and `exec_agentd()`s a brand-new standalone agentd,
  which then fights the *original* cockpit agentd for the FUSE mountpoint and port 7999 — likely
  crashing immediately, dropping the operator out of the cockpit entirely, with the original
  zero-agent daemon now orphaned in the background. README's `[n]` pointer was corrected in this
  increment to document this limitation explicitly (with a `--privileged` or `orchestrate`/`agent`
  workaround) rather than overclaiming. Real fix: thread the `DataSource` into
  `execute_pending_spawn` and try `source.spawn()` before falling back to `exec`.
- **ux.9-ar-08 (P3, found by Codex, /review) — the readiness check can pass slightly before
  agentd's scheduler is actually processing commands.** `agentd/src/main.rs` mounts FUSE and
  starts the management API before constructing the scheduler; `cockpit)`'s readiness wait treats
  "`/agents/system` exists" or "`/healthz` returns 200" as ready. An HTTP spawn/inject/approve
  request landing in this narrow gap could time out even though the command later succeeds.
  Already partially mitigated by orch.2's existing 2s spawn-confirmation timeout tolerance
  (`POST /api/v1/spawn`); not re-verified as an *observed* failure in this increment's manual
  testing, but the code-level race is real.
- **ux.9-ar-09 (P2, found by Claude adversarial subagent, /review) — no ongoing liveness
  supervision after the initial readiness check, and no Dockerfile `HEALTHCHECK`.** If `agentd`
  crashes or hangs mid-session (after boot succeeded), `agentctl watch` degrades to showing a
  connection-error banner forever rather than exiting; nothing signals the failure to a container
  orchestrator (`docker ps` shows "Up" indefinitely). This is the SAME gap as expansion candidate
  (1) below ("crash-loop supervision") — that candidate was scoped out as a nice-to-have during
  planning; this review pass reclassifies it as a confirmed, real gap (not just a "10x" idea),
  though still not required for this increment's stated "boot two things together" acceptance
  criteria. Raise its priority when scoping the next cockpit increment.
- Five expansion candidates scoped out at CEO 0D (SELECTIVE EXPANSION, all auto-deferred —
  none required for this increment's acceptance criteria; item (1) reclassified above as a
  confirmed gap, not just a nice-to-have): (1) crash-loop supervision — detect `agentd` exiting
  unexpectedly post-boot and restart it with a visible "restarted Nx" banner (see ux.9-ar-09);
  (2) a first-boot welcome/tour overlay distinguishing a cold start from attaching to an
  already-running `agentd`; (3) printing the exact `docker exec`/`agentctl watch --url` reattach
  command for a detached session; (4) a `--template` passthrough so `cockpit --template scout
  --task "..."` boots straight into a running custom agent instead of the empty default state;
  (5) an exit summary on Ctrl-C (agents run, tokens spent, checkpoint path) instead of a bare
  terminal restore.
- **ux.9-ar-10 (P4, found by `/document-release` during `/ship`) — CLAUDE.md's per-increment
  status log has ~15 unlogged increments.** The running narrative log (`**xyz complete
  (vX.Y.Z).**` entries) stopped at dx.3 (v0.69.0); dx.4, ma.1-4, cred.1-cred.4b, h8.1/h8.2,
  orch.1/orch.2, cos-polish, dx.6, cheap-wins, and memory-routing all shipped and are documented
  elsewhere (CHANGELOG.md, ROADMAP.md) but are missing from CLAUDE.md's log. This increment
  (ux.9) added its own entry but did not backfill the gap — pre-existing debt, not introduced by
  this branch. Worth a dedicated backfill pass.

## ux.0b — Open (deferred from ship-stage adversarial pass, 2026-07-12)

Host-loopback reachability (Option A, gated `[management] allow_non_loopback`). The build-stage
`/review` adversarial pass surfaced two gaps; the ship-stage adversarial pass (Claude + Codex +
independent outside voice, all three convergent) recommended fixing the network-segmentation gap
now rather than deferring it, since it's reachable via the project's own documented quickstart
commands with no misconfiguration required — so it was fixed in this increment. One gap remains
a genuine follow-up (a design decision beyond Option A's "smallest change" scope):

- **ux.0b-ar-02 (P3) — `allow_non_loopback` is an unscoped bypass, not Docker-bridge-limited.**
  `agentd/src/management.rs`'s guard is a plain `bound.ip().is_loopback() || allow_non_loopback` —
  once true, nothing in code distinguishes a Docker-internal address from a real LAN/public NIC.
  The exact `agentd/cos.agents.toml` pattern (`bind_addr = "0.0.0.0"`, `allow_non_loopback = true`)
  would be unsafe if copy-pasted onto bare metal or a cloud VM without Docker's NAT boundary. Fix:
  consider scoping the opt-in (e.g. restrict to RFC 1918/link-local ranges) or requiring a second,
  more explicit acknowledgement. `agentd/src/management.rs:441`; THREAT_MODEL.md §9.1 documents
  this as an accepted, unresolved gap.

**~~ux.0b-ar-01 (P2) — `cos` and `agent` Compose services shared the same default bridge network.~~**
**FIXED (same PR, ship-stage adversarial round):** `docker-compose.yml` now defines explicit
`cos-net` / `agent-net` networks — `cos` is alone on `cos-net`; `agent`, `qdrant`, and
`semantic-kb-mcp` share `agent-net`. `agent` (which runs arbitrary/untrusted templates with live
`http_fetch`/`web_search`) can no longer reach `cos:7999`'s unauthenticated management API on the
Compose bridge. Verified via `docker compose config` showing each service's resolved network.

## cos-dev — Open items

- **cos-dev-01 (P3) — `write_file` silently fails in dev mode if `./output/` doesn't exist.**
  `FsWrite { prefix = "./output" }` is granted and the orchestrator task calls
  `write_file(path='./output/brief-{TODAY}.md', ...)`, but the native `write_file` tool does not
  create missing parent directories. Result: the brief lands in `ops:briefs` KB only with no
  filesystem copy; the orchestrator reports a capability-denied error that looks like a permission
  problem rather than a missing directory. Fix (either): pre-create `agentd/output/` with a
  `.gitkeep` so it exists when `cargo run` is invoked from the repo root, OR have `write_file`
  call `fs::create_dir_all` on the parent before writing. `agentd/src/tools/native.rs`.

- **cos-dev-02 (P2) — Curator agent inherits Gmail credential + tool access it never needs.**
  Found by `/review`'s adversarial pass on PR #122 (google_oauth Credential capability fix,
  2026-07-15): `cos-orchestrator`'s agent-level capabilities include
  `Credential{provider=Google}` and `Mcp{server=google_oauth}` (`agentd/cos.agents.toml`), and
  the task prompt states "spawned agents inherit the parent's full capability set." The
  **Curator** child (KB-only writer — persists briefs/entities, never touches Gmail) therefore
  also inherits Gmail credential + tool access via spawn, since capability inheritance is
  all-or-nothing per spawned child, not scoped per child's actual task. Not a live bug (Curator's
  task prompt doesn't call the OAuth tools), but a real least-privilege gap: a
  prompt-injection-compromised Curator, or a future task-prompt change, could reach Gmail with no
  code-level guard against it. Fix needs per-child capability scoping at `spawn_agent` call sites
  (currently spawned children get the parent's full set unconditionally) — larger scope than a
  quick capability tweak. Depends on: none, but touches the `Spawn` capability's inheritance model.
- **cos-dev-03 (P3) — Older Google OAuth templates still on the pre-cred.6 raw-secret pattern.**
  Found by the same `/review` pass: `templates/cos-inbox.template.toml`,
  `templates/google-agent.template.toml`, and `templates/cos-orchestrator.template.toml` all
  spawn `google_oauth`/`oauth_mcp.py` but have no `[credential_gateway]` section — they still pass
  `OAUTH_CLIENT_SECRET`/`OAUTH_REFRESH_TOKEN` directly via `passenv` to the Python sidecar
  (pre-cred.6 pattern). Since `credential_gateway.enabled` defaults to `false`, this specific
  bug class (missing server-level `Credential` capability) doesn't apply to them — they never
  build a broker token at all. But it means these templates diverge from the hardened
  `agentd/cos.agents.toml`/distro-overlay pattern: the raw OAuth secret still reaches the Python
  sidecar process for anyone using these templates instead of the CoS config. Migrate them to
  broker mode for consistency, or explicitly document why they're intentionally exempt (e.g.
  simpler single-agent demo templates where the broker's operational overhead isn't worth it).
  `templates/github-agent.template.toml` is already correctly on the broker pattern — no fix
  needed there. Depends on: none.

## v0.60 whole-system audit (2026-07-06)

Read-only audit: 7 parallel reviewers (Claude + Codex) across every crate + docs, main @ e2ec0e47.
Full ranked report: `docs/AUDIT-v0.60.html` (in repo) + artifact https://claude.ai/code/artifact/e9cb8d95-d1c0-4eeb-a2a6-0d73db3cf32f
The Phase-10 cred.3 deep-dive (separate 4-voice review) is folded into the report and the
**Phase 10 — Credential manager** section below (`cred.3-ar-01…10`; ar-06–10 added by this pass).
Verdict: **all 7 invariants hold in code; the security/audit *story* is over-claimed vs the code, and
the narrative docs have drifted.** IDs below match the report. Known-debt items already tracked
elsewhere in this file are cross-referenced, not duplicated. `[new]` / `[re-rated]` are the items this
pass surfaced.

**Remediation order (what gates what):** Wave 1 (before building further) = S1,S2,S3,C2,O1,O2 ·
Wave 2 = C1,C3,C4,C5,C8,S5,S6,S7 · Wave 3 (docs, one pass) = D1–D5,S4 · Wave 4 = O3,O4,C6,C7,C9.

### Tier 1 — security & integrity (the trust-story gap)
- ~~**audit-S1 (P1) [new] — Ed25519 receipt-signing key readable by any MCP server with `FsRead`.**~~ **[FIXED in v0.61.0 (cred.3.1) as cred.3-ar-S1: OV-1 boot `ensure!` rejects startup when `egress.key_path` falls inside any MCP FsRead/FsWrite prefix — `main.rs:1003-1019`. Verified 2026-07-17 (audit.1).]**
  Startup blocks `FsWrite` over the evidence log but not `FsRead` over `egress-key.pkcs8` (CWD default)
  → a malicious tool reads the key and forges valid receipts. Fix: reject startup if `egress.key_path`
  resolves under any MCP `FsRead`/`FsWrite` prefix; default the key outside any MCP-accessible tree.
  `config.rs:181`, `main.rs:859`, `evidence.rs:88`.
- ~~**audit-S2 (P1) [new] — `content_audited: true` hardcoded but nothing audits content.**~~ **[FIXED in v0.61.0 (cred.3.1) as cred.3-ar-S2: claim dropped from EgressBrokered; regression test T23 (`credential/mod.rs:2425-2434`) fails if it returns. Verified 2026-07-17 (audit.1).]**
  Flight event + receipt assert an audit that never happens; the receipt doesn't cover forwarded bytes.
  Fix: hash the forwarded body into the receipt, or drop the claim. `egress.rs:150`.
- **audit-S3 (P1) [new] — `SecretRewriter` / `BoundarySecretRedacted` claimed shipped (v0.39.0) but ABSENT.**
  Tool outputs reach the model unscrubbed (`read_file /proc/self/environ` leaks to context). Fix:
  implement at the `ToolRegistry::invoke` choke point (`tools/mod.rs:182`) OR correct CLAUDE.md + memory
  to say only inference-egress receipting shipped. (Doc-drift + missing defense.)
- **audit-S4 (P2) — OTEL "credential guard" (obs.1) not implemented + README default documented backwards.**
  Only preview-redaction exists; `otel/README.md:54` says `OTEL_REDACT_PREVIEWS` defaults `false`, code
  defaults `true`. Fix: add a scrub choke point in `finish()` or correct the docs. `otel/src/span_builder.rs`.
- **audit-S5 (P2) — signed chain not re-verified on resume** (same as p7.5-scope-03; live append trusts the
  tail, offline `agentctl verify` still catches). `evidence.rs:184`.
- **audit-S6 (P2) — sandbox degradation fail-open.** Landlock/`unshare(NEWNET)` silently degrade;
  `mcp_require_capabilities` checks non-empty rules, not that isolation applied → a "network-isolated"
  server keeps network on userns-disabled hosts. Fix: strict mode that aborts on requested-isolation
  failure, default-on for v0.60. `sandbox/src/lib.rs:513,714,730`, `main.rs:353`.
- **audit-S7 (P2) [new] — universal-tier gVisor runs `--network=host`.** Only Anthropic traffic is
  mediated; a compromised foreign agent does arbitrary non-Anthropic egress (undercuts governed-foreign
  pillar). Fix: default universal gVisor to no-net / proxy-only; require explicit net cap. `universal.rs:66,87`.
- **audit-S8 (P2) — `Net.hosts` advisory only** (host scoping not enforced; only ports). `capability.rs:162`.

### Tier 2 — correctness / data-loss / invariants
- **audit-C1 (P1, invariant) — token budget is soft** (overshoot per in-flight call; per-agent `EndTurn`
  unchecked). "Cognition is metered" is not a hard bound. Fix: reserve before dispatch, clamp `max_tokens`
  to remaining, refund. `scheduler.rs:1208`, `agent/mod.rs:572,600`. (Related: F-009.)
- **audit-C2 (P1) [re-rated from F-10/P2] — cross-agent Tier-3 memory isolation defeatable via the
  `agent/` prefix.** `KbRead{segment:"agent"}` satisfies `agent/anyone`; nothing reserves the prefix →
  breaks the locked "private memory never grantable" rule. Fix: reserve `agent/` as non-grantable in
  `capability.rs:219` + reject `[[memory.segments]]` under `agent/` at startup. **Supersedes F-10 severity.**
- **~~audit-C3 (P2) — checkpoint not durable (no fsync around rename).~~** = F-05. **[FIXED in v0.70.0 (orch.2): `sync_all()` + parent-dir fsync in `CheckpointStore::save()`]**
- **audit-C4 (P2) [new] — restored checkpoint deleted before the restored run re-checkpoints** → crash
  before next save loses the recovery point. Fix: keep as `.inflight` backup until a clean post-restore
  save supersedes it. `main.rs:980`.
- **audit-C5 (P2) [new] — send to a completed agent reports success and silently drops the message.**
  Fix: reject sends to terminal/outcome agents with an `is_error` ToolResult. `scheduler.rs:1865,1907`.
- **audit-C6 (P2) — `StopReason::Other`/empty end_turn completes silently as `""`.** = F-008 (tracked).
- **audit-C7 (P2) — streaming unbounded channel + stdout head-of-line blocking.** = p7.2-ar-01/02 (tracked).
- **audit-C8 (P2) — memory unbounded-growth trio:** `short_term` never trimmed + full-cloned per tick
  (F-08/p5.2-ar-01); `mem_recall` full-namespace scan (p5.3-ar-04); orphaned `scratch_ver` META keys.
- **audit-C9 (P2) — distillation off-budget + emits event on write failure.** = F-12 (tracked).

### Tier 3 — packaging / CI / operability
- **~~audit-O1 (P1) [new] — three catalogue templates unusable: `cron-agent`, `watcher`, `webhook-agent`~~**
  **[FIXED in v0.70.0 (orch.2): added `[capabilities].mcp` entries to all three templates]**
- **audit-O2 (P1) — `make test` broken (duplicate `memory0` 9p mount).** = ma.2-ar-01 (tracked; re-rate P1).
- **audit-O3 (P2, invariant) — distro boot not CI-gated (dry-run only)** → violates "every arch boot is
  CI-tested or it rots"; PID-1 failure drops to interactive shell (hangs headless QEMU); Docker only
  builds on `main` (no PR smoke). `ci.yml:231`, `distro/overlay/init:52`.
- **audit-O4 (P2) [new] — unbounded reads on remote surfaces:** `agentctl --url` reads HTTP bodies
  uncapped (OOM); `/api/v1/memory/:ns` loads the full namespace before paginating.
  `agentctl/src/watch/source.rs:91`, `management.rs:244`. (Minor: watcher sidecar `FsRead "/"`;
  OAuth callback no size bound; FUSE shared-KB inode leak; approvals `take(100)` silent/nondeterministic.)

### Tier 4 — documentation drift (no canonical "what's shipped" surface)
- **audit-D1 (P2) — RUNBOOK.md materially stale** (v0.20/v0.59 header, shipped Phase 5/6 as "future", no
  cred.3 section). Overlaps sec.1. `RUNBOOK.md:3,808`.
- **audit-D2 (P2) — ROADMAP build-order header contradicts its own detail** (header "shipped v0.57 /
  cred.1–3 upcoming" vs detail "cred.1–3 shipped v0.58–0.60"). `ROADMAP.md:46` vs `:1150`.
- **audit-D3 (P3) — roadmap has no "not-done" glyph; cred.4/cred.5 falsely marked done.** `ROADMAP.md:1184,1194`.
- **audit-D4 (P3) — CLAUDE.md status log skips cred.1/cred.2; no current-state line.** Add a canonical
  "v0.60.0 — phases 0–7 + tracks shipped; cred.4/5, orch.1, h8.x, Phase 9, MESH unshipped" line at top.
- **audit-D5 (P3) — THREAT_MODEL.md header says v0.25.0 but contains cred.3 §8;** still claims "flight not
  tamper-evident / no other credentials" (both false). Overlaps sec.1. `THREAT_MODEL.md:4,27`.

### Reconciliation actions
- **Close kv-ar-05** — timestamped `.corrupt` quarantine is already implemented (`store.rs:144`); dup of F-02.
- **F-10 → superseded by audit-C2 (P1).**
- Verified still-open (accurate triage): F-05, F-08, F-12, p5.2-ar-*, p5.3-ar-04/05, p7.1-ar-01,
  p7.2-ar-*, p7.6-ar-01, obs.3-ar-01, sec.1.

## sec.1 — THREAT_MODEL.md full update (deferred from cred.2)

**Priority: P2.** `docs/THREAT_MODEL.md` is at v0.25.0 / Phase 5.8; the codebase is now
v0.59.0. The following surfaces are missing and must be documented before sec.1 closes:

- **p7.5b egress proxy** (`EgressProxy`, `:proxy_port`): loopback HTTP proxy; real
  `ANTHROPIC_API_KEY` held by proxy, never in agent env; placeholder ephemeral keys;
  streaming unsupported (returns 501); bind must stay loopback.
- **p7.7 management API** (`:7999`): operator HTTP/SSE surface; approval routes; localhost-only
  bind guard; Docker/QEMU exposure implications if port-forwarded.
- **p7.6 universal-tier gVisor** (`runsc do`): foreign workload process boundary;
  `--network=host` loopback exposure; `ANTHROPIC_BASE_URL` + ephemeral key injection;
  no Landlock/seccomp in that path (gVisor owns isolation).
- **h7.2 OAuth callback bind**: `docker/oauth_mcp.py` binds `0.0.0.0` when
  `OAUTH_CALLBACK_PORT` is set; under privileged Docker + port forwarding this is a real
  network surface. Deprecation or documentation required.

Do **not** partially update THREAT_MODEL.md — a partial update makes the document look
authoritative while missing major surfaces. Either do the full update or leave untouched.

---

## dx.5 — Mac first-run works end-to-end (dogfood findings, 2026-07-05)

> **SUPERSEDED — folded into Phase 10 (Credential manager).** A 4-voice CEO+Eng review
> (2026-07-05) found the root cause is the absence of a coherent credential model across all
> three surfaces, not three isolated Mac bugs. dx.5 is now **`cred.1` + `cred.2`** of Phase 10
> (`docs/plans/credential-manager.md`; ROADMAP "Phase 10 — Credential manager"). Do **not**
> implement dx.5 standalone — implement the `cred.*` increments. Notably, the original
> `mac-df-02` fix (add `OAUTH_CALLBACK_PORT` to the shared template) is **wrong** under the
> reframe (it polishes the in-container OAuth path cred.2 deprecates); it is dropped. The
> findings below remain accurate as the symptom record.

First real end-to-end dogfooding of AgentOS on Apple Silicon via the Docker path.
The container agent (`scout`) runs correctly to completion; the findings below block
the `google-agent` and a smooth clean-Mac first run. Fix as a batch.
**Acceptance:** on a clean Apple Silicon Mac, `docker compose` scout **and** google-agent
both run, and a documented quickstart works, without manual patching. Not yet scheduled;
promote to `docs/ROADMAP.md` (DX track) when picked up.

**mac-df-01 (P2) — `docker compose run --service-ports` publishes nothing for the `agent` service**
- `docker-compose.yml`: the `agent` service declares no `ports:`, and `--service-ports`
  only publishes *declared* ports. The service's own comment tells users to pass
  `--service-ports` for the OAuth callback — which is a silent no-op. The browser callback
  to `http://127.0.0.1:<port>` then hits nothing → `ERR_CONNECTION_REFUSED`.
- Fix: add `ports: ["8585:8585"]` to the `agent` service (or document `-p 8585:8585` on the
  run command) and correct the misleading comment.

**mac-df-02 (P2) — `google-agent` template `passenv` omits `OAUTH_CALLBACK_PORT`**
- `templates/google-agent.template.toml` `[[...mcp_servers]] passenv` lists the OAuth client
  vars (`OAUTH_CLIENT_ID`, `OAUTH_CLIENT_SECRET`, …) but **not** `OAUTH_CALLBACK_PORT`.
  agentd spawns MCP servers with `env_clear()` + the `passenv` allowlist (`tools/mcp.rs:115`),
  so a caller-set `-e OAUTH_CALLBACK_PORT=8585` is stripped before `oauth_mcp.py` sees it.
  oauth_mcp then falls back to binding a random ephemeral loopback port (`127.0.0.1:0`,
  observed `:43151`), which is both unpredictable and unreachable from the host — so the
  in-container OAuth flow is fundamentally broken for this template.
- Fix: add `"OAUTH_CALLBACK_PORT"` to the template's `passenv` list. Pairs with mac-df-01
  (with the port fixed + forwarded, oauth_mcp binds `0.0.0.0:8585` and the callback lands).

**mac-df-03 (P2) — no host `agentctl` on Mac blocks the clean `agentctl auth google` path**
- The dx.1-intended OAuth flow (`agentctl auth google` runs on the host, binds the real
  `127.0.0.1:8585`, writes `~/.agentos-secrets/google.json`) needs a native macOS `agentctl`
  binary, which is not built or shipped. `cargo build --release -p agentctl` may not build on
  macOS (the `surfaces`/`fuser` dep is Linux-only) — needs verification.
- Fix: verify `agentctl` builds natively on macOS (gate any FUSE deps behind
  `#[cfg(target_os = "linux")]` if needed); document `cargo build -p agentctl` +
  `agentctl auth google` in RUNBOOK as the Mac OAuth path; optionally publish a Mac `agentctl`
  artifact. Also: the `agent` compose service does not mount `~/.agentos-secrets` (only `cos`
  does) — add the mount (or document it) so the host-provisioned `google.json` reaches the agent.

## dx.6 — Deferred items (v0.74.0)

**P4 — Native ARM64 CI runners** (deferred from dx.6)
- `build-aarch64` currently uses `cross` + QEMU emulation on `ubuntu-latest`. Native arm64 GHA
  runners would cut the aarch64 test job from ~45 min to ~5 min and eliminate QEMU overhead.
- Blocked by: private repo → paid GHA runner seats needed. Revisit if the repo goes public or
  a self-hosted arm64 runner is available.

**P3 — Cross-compiled arm64 Docker image** (deferred from dx.6)
- `publish-docker` builds the arm64 Docker image via QEMU emulation (~60-90 min cold).
  The `cross` toolchain (already used for musl static binaries) could cross-compile the Rust
  binaries for arm64, reducing the Docker build to a COPY + Python layer (~5 min).
- Investigate: `cross build --target aarch64-unknown-linux-musl` in the Docker builder stage,
  then COPY the pre-built binary into the runtime image. Complexity: `ring` asm requires the
  right linker; `Cross.toml` already pins the correct image.

## orch.1 — Interactive agent orchestrator (on-demand dispatch + conversational follow-up) [shipped — v0.66.0]

> **Renamed from `h8.3`** (2026-07-05) to resolve a collision with ROADMAP's `h8.3` (multi-device
> migration) and to give the concept its correct name: the former "Track ORCH" (multi-instance) was
> renamed **Track MESH** (`mesh.*`), freeing `orch.*` for the intra-instance interactive
> orchestrator. In the Phase-10 build order, `orch.1` lands after `cred.3` (foundation-first).

Supersedes the "no chat mode" dogfood finding (Bug C, 2026-07-05). Agents today are
one-shot (perceive→infer→act→complete→exit); there is no way to ask a follow-up or have
the system route a request to the right agent on demand. The `inject` primitive (p7.3) and
the CoS coordinator pattern are the building blocks. To be scoped via `/office-hours` +
`/plan-eng-review`; promote to a `docs/ROADMAP.md` phase when scheduled.

- **Core:** an orchestrator entry point that takes a natural-language request, selects/spawns
  the right template(s), runs them, and streams results back.
- **Conversational follow-up:** keep an agent alive and route follow-ups via `inject` (context
  preserved) instead of one-shot re-runs that lose all prior context.
- **Docker `chat`/`orchestrate` mode:** an entrypoint REPL wrapping the above
  (stdin → inject → stream), so a follow-up in the same window works.
- **Fold-in friction found while dogfooding:** the `agent` entrypoint wipes the checkpoint on
  every `docker compose run` (no memory across runs); Haiku's "Would you like me to dive
  deeper…?" phrasing misleads users into typing follow-ups that go nowhere (no loop reads them).

## orch.1 — Actionable remediations (v0.66.0, shipped 2026-07-07)

- **~~orch.1-ar-01 (P2) — Waiting agents excluded from checkpoint.~~**
  **[FIXED in v0.70.0 (orch.2): FORMAT_VERSION 4, `waiting_agents`/`orchestrated_agents` in `SchedulerCheckpoint`, seed loop guard, `terminal` preserved through `from_checkpoint`]**

- **~~orch.1-ar-02 (P3) — `POST /api/v1/spawn` returns 200 before scheduler confirms.~~**
  **[FIXED in v0.70.0 (orch.2): oneshot confirm channel in `OperatorSpawnRequest`; POST returns 201 + `{"agent_id":"..."}` after 2s wait]**

- **~~orch.1-ar-03 (P3) — `OrchestratorTurnComplete` carries full answer text.~~**
  **[FIXED in v0.70.0 (orch.2): answer capped at 512 chars (char-safe) in `OrchestratorTurnComplete` event]**

- **orch.1-ar-04 (P3) — Docker `wait $AGENTD_PID` hangs after `agentctl orchestrate` exits.**
  When orchestrate closes, agentd continues running (management server keeps `control_tx`
  alive). `wait $AGENTD_PID` in `entrypoint.sh` never returns; container requires
  `docker kill`. Fix: trap EXIT in the entrypoint and send SIGTERM to `$AGENTD_PID`, or
  remove the `wait` from the orchestrate mode. (`docker/entrypoint.sh`)
  **[FIXED in v0.66.0: kill + SIGTERM trap added]**

- **~~orch.1-ar-05 (P2) — `state.waiting` serves dual purpose (orchestrated flag + parked flag).~~**
  **[FIXED in v0.70.0 (orch.2): split into `orchestrated` (permanent) + `waiting` (parked); `handle_agent_terminal` clears both]**

- **orch.1-ar-06 (P3) — No SSE read timeout — REPL hangs on silent network failure.**
  **[PARTIALLY FIXED in v0.70.0 (orch.2): management SSE handler now sends `: ping\n\n` every 30 s (mitigates
  load-balancer idle timeouts). Client-side: `orchestrate.rs` SSE client uses `.timeout(None)` — no OS-level
  read deadline. On a true network partition without TCP FIN/RST (e.g., direct TCP to :7999 over an unreliable
  link), `reader.lines()` blocks indefinitely. The SSH-tunnel path documented in DEPLOYMENT.md delivers RST on
  drop, partially mitigating the risk. See orch.2-ar-04 (below) for the remaining fix.]**

- **~~orch.1-ar-07 (P3) — `quit`/`exit` keywords injected as messages instead of exiting the REPL.~~**
  **[FIXED in v0.70.0 (orch.2): `orchestrate.rs` checks for `quit`/`exit` before inject and breaks with a resume message]**

- **orch.1-ar-08 (P3) — Empty input in `agentctl orchestrate` causes silent hang.**
  When the user presses Enter on an empty line, the REPL calls `continue`, which goes back
  to `drain_until_turn_complete`. No inject was sent so the agent stays parked — no
  `orchestrator_turn_complete` event arrives, and the REPL blocks forever. Fix: on empty
  input, loop on `stdin.read_line` until a non-empty line is entered, without re-entering
  `drain_until_turn_complete`. (`orchestrate.rs` REPL loop, line 108)

- **orch.1-ar-09 (P2) — `templates/orchestrator.template.toml` uses wrong section name.**
  The file uses `[meta]` and `[meta.capabilities]` instead of `[template]` and
  `[capabilities]` (the schema `TemplateConfig` expects). As-is the file will not parse via
  `agentctl list-templates` / `agentctl spawn orchestrator`. The entrypoint.sh `orchestrate`
  mode avoids this by using `agentd /etc/agentd/agents.toml` directly. Fix: rename `[meta]`
  → `[template]`, move capabilities to a top-level `[capabilities]` section, and convert
  `[[sample_tasks]]` array-of-tables to a top-level inline array
  `sample_tasks = ["...", "..."]`. (`templates/orchestrator.template.toml`)

## orch.2 — Actionable remediations (v0.70.0, shipped 2026-07-09)

- **orch.2-ar-01 (P2) — `POST /api/v1/spawn` 503 is non-authoritative.** When the scheduler
  does not service `control_rx` within the 2s confirmation window, the management server returns
  503 (Retry-After: 1), but the `ControlCommand::Spawn` remains queued. The scheduler processes
  it after the HTTP handler returns, starting the agent anyway. Retrying with the same explicit
  agent ID will then hit the collision guard and get another 503 (agent exists). Fix: add a
  cancellation sentinel to the spawn command, or poll the snapshot for the agent before retrying.
  (`agentd/src/management.rs:321`, `agentd/src/scheduler.rs:1898`)

- **orch.2-ar-02 (P3) — `max_turns` has no effect on pure-text orchestrated agents.**
  `self.turn` is only incremented by `provide_tool_results`; agents that respond with text
  (no tool calls) never increment the turn counter, so `max_turns` never fires. Only
  `token_budget` terminates them. Fix: increment a separate REPL-turn counter in
  `push_user_turn()`, or document that `max_turns` counts tool-call rounds only. (`agentd/src/agent/mod.rs`)

- **orch.2-ar-03 (P3) — Broadcast lag can silently drop `OrchestratorTurnComplete`.**
  When the broadcast buffer (1024 slots) fills, lagged SSE clients receive
  `data: {"lagged": N}`. If `OrchestratorTurnComplete` was among the dropped events,
  `drain_until_turn_complete` blocks forever (no handler for the lagged sentinel).
  Fix: handle `"lagged"` in the drain loop by polling `/api/v1/snapshot` for
  `agent.status == "waiting"`, or increase the broadcast buffer to 16384. (`agentd/src/management.rs`)

- **orch.2-ar-04 (P3) — `agentctl orchestrate` SSE client has no read timeout (orch.1-ar-06 partial).**
  The blocking `reqwest::Client` used for the persistent SSE connection in `orchestrate.rs:52` is built
  with `.timeout(None)`. The server-side `: ping` keepalive (added in orch.2) prevents LB idle-timeouts
  and causes the OS to detect connection loss via TCP keepalives after ~2 min on most systems. However,
  on a true network partition without TCP FIN/RST, `reader.lines()` in `drain_until_turn_complete` blocks
  the OS `read()` call indefinitely — no deadline. Fix: add a TCP keepalive socket option (via `socket2`
  on the blocking client) so the kernel detects partition; or expose a per-read deadline via a separate
  reader thread + channel with a 90 s `recv_timeout`. (`agentctl/src/orchestrate.rs:52`)

## Track MA — Open (from ma.4)

- **ma.4-ar-01 (P3) — `require_isolation_tier` config key not yet implemented.**
  `probe()` runs and reports the tier honestly, but there is no config field to
  fail startup when `probe().tier < declared_tier`. Add a `[isolation]` section to
  `agentd.toml` with a `require_tier = "full" | "capability" | "none"` key; fail
  startup with a descriptive error if the probed tier is weaker than required.
  (`agentd/src/config.rs`, `agentd/src/isolation_caps.rs`, `agentd/src/main.rs`)

## Phase 5 — Open (deferred from p5.9 hardening; all P2, none data-loss-class)

See `docs/AUDIT-phase-5.md §8` for full context. p5.9 closed every P1; these P2s remain:

- **~~F-05 (P2) — `checkpoint.save()` has no `fsync` before/after rename.~~** **[FIXED in v0.70.0 (orch.2/audit-C3)]**
- **F-06 (P2) — FUSE dynamic-inode counter never reclaimed / unguarded.** Long-lived
  mounts can exhaust the counter; no overflow guard on the mount thread.
- **F-07b (P2) — `inject_messages` can append `Text` onto a ToolResult-only User turn.**
  Reintroduces 4.6 F-006 shape; `page_turns` previews `blocks.first()` (the ToolResult),
  so a paged inter-agent message is misrepresented in `short_term` / FUSE. (F-07a — the
  alternation debug-assert — was promoted to a runtime `Err` in p5.9.) Fix needs care:
  pushing a new User turn risks consecutive-User violations; prefer making the preview
  pick the first `Text` block, and/or fold injected text into the assistant-adjacent turn.
- **F-08 (P2) — `short_term` is unbounded and full-cloned every checkpoint + snapshot tick.**
  Bound it (ring buffer) and avoid the per-tick clone.
- **F-10 (P2) — private `agent/<id>` tier shares keyspace/delimiter with grantable `kb:*`.**
  Reserve the `agent/` prefix so it can't be granted via `segment:"agent"`.
- **F-11 (P2) — unconfigured segments default to writable Scratch (fail-open).**
  Consider deny-by-default for undeclared segments.
- **F-12 (P2) — distillation ignores per-agent budget; emits event even if the store
  write failed.** Charge distillation against budget; only emit on a confirmed write.

## Phase 5 — Open (deferred from p5.8)

**~~p5.7-ar-01 (P2) — Inode map entries are never pruned for terminated agents~~** ✓ Fixed in p5.8.
- `prune_dead_agent()` method added to `AgentsFs`; called in `readdir(Root)` for every agent ID
  in the current snapshot that is absent from the live agent set. Cleans all 6 maps: `dir_inodes`,
  `inode_to_id`, `dyn_ino_kind`, `lt_key_ino`, `kb_seg_ino`, `kb_key_ino`.

**~~p5.7-ar-02 (P2) — HashMap lookup in `getattr`/`read` does not assert inode kind~~** ✓ Fixed in p5.8.
- Tautological `debug_assert!(matches!(...))` removed from `dyn_file_content()` LtFile and KbFile arms;
  the enclosing `match` already guarantees the variant — explanatory `// ar-02:` comments added instead.

**~~p5.7-ar-03 (P2) — `getattr` returns `Directory` for `memory/` and `memory/long_term/` even when memory store is not configured~~** ✓ Fixed in p5.8.
- `getattr()` now returns `ENOENT` for `OFF_MEMORY_DIR` (+5) and `OFF_LONG_TERM_DIR` (+7) inodes when
  `self.memory.is_none()`. `OFF_SHORT_TERM` (+6) intentionally exempted (served from `AgentSnapshot`).

**~~p5.7-ar-04 (P3) — `list_namespaces` is O(n) full ENTRIES scan (no NAMESPACES index)~~** ✓ Fixed in p5.8.
- `NAMESPACES: TableDefinition<&str, u64>` redb table maintained atomically on every put/append/delete/evict.
  One-time backfill on first open of pre-p5.8 stores. `list_namespaces()` is now O(k) (k = distinct namespaces).

**p5.7-ar-05 (P3) — `MAX_DIR_KEYS=100` truncation is silent (no overflow marker in readdir)**
- `agents_fs.rs:capped_keys`: the cap is applied with `.take(MAX_DIR_KEYS)`. When a namespace has more
  than 100 entries, the directory listing is silently truncated. An `ls` that returns 100 entries is
  indistinguishable from one that exhausted the full set.
- Fix (or document): emit a sentinel file entry (e.g. `…truncated`) in readdir when the cap fires,
  or increase `MAX_DIR_KEYS` and add a per-call budget. Document the limit prominently in RUNBOOK.md.
  (RUNBOOK.md already documents this; a sentinel file would be the runtime signal.) Deferred to p6+.

## Phase 7 — Open (deferred from p7.5 autoplan, 2026-06-23)

**p7.5-scope-01 — Universal-tier egress deferred to p7.5b** ✅ *resolved in p7.5b (v0.40.0)*
- All three architecture problems addressed: hyper v1 (E2 ✅), ephemeral per-workload API keys
  for caller identity (E3 ✅, key-based vs. FD-pass — simpler and avoids exec boundary complications),
  loopback-only bind + FUSE `egress_addr` surface (E1 ✅). FD-pass deferred to p7.6 when microVM
  isolation requires it.
- `[[workloads]]` TOML schema not needed: workloads register at runtime via `ProxyRegistry::register()`.
- Fail-closed proxy invariant: `start_http_proxy()` → `anyhow::ensure!` on empty real key; returns
  `Err` on bind failure; scheduler fails-closed if proxy start fails.

**p7.5-scope-03 — resume_chain does not re-verify existing receipts on restart**
- On restart `EvidenceWriter::open()` re-opens an existing `evidence.jsonl` and anchors to the
  last line without verifying its signature. An attacker with file write access before restart
  can inject a poisoned anchor line; the verifier (`agentctl verify`) still catches this from
  genesis, but future receipts chain from the poisoned anchor.
- Fix in p7.5b: on open, call a lightweight `scan_last_verified_line()` that verifies the last
  N lines (or reads the last signed seq from a sidecar `.chain-head` file). Low priority while
  evidence.jsonl lives in the operator-controlled runtime dir (same threat model as the keyfile).

**p7.5-scope-02 — Allowlisted host forwarder deferred (host policy hard problem)**
- `Capability::Net.hosts` remains advisory in p7.5 and p7.5b (proxy always forwards to one
  hardcoded upstream `ANTHROPIC_MESSAGES_URL`; `allowed_hosts` field is scaffolding only).
  Full per-workload host enforcement requires: DNS rebinding mitigations, CNAME/IP canonicalization,
  IPv4/IPv6 literals, redirect following policy, link-local/RFC-1918 denials, host suffix
  confusion guards.
- Deferred to p7.6 alongside the isolation floor. Document semantic change in CONVENTIONS.md
  and CHANGELOG when proxy enforcement lands.

**p7.5b-ar-01 (HIGH) — Budget TOCTOU: concurrent requests can over-spend**
- Pre-check `load() == 0` and post-response `fetch_update(AcqRel, ...)` are not atomic.
  N concurrent requests can all pass the zero-check and collectively over-spend the budget
  by up to N × request_cost before any decrement lands. Saturating arithmetic prevents wrap,
  but the counter reaches zero only after over-spend.
- Acceptable for p7.5b single-agent soft-budget. True fix: pre-reserve an estimated cost
  via CAS before forwarding and refund the remainder post-response. Revisit in p7.6 when
  microVM workloads can fire concurrent requests.

**p7.5b-ar-02 (MEDIUM) — anthropic-beta passthrough allows workload-controlled feature flags**
- `anthropic-beta` header is forwarded verbatim. A workload can opt into experimental
  Anthropic API features (e.g. `files-api-2025-04-14`) that may have different cost profiles
  or capability gates the operator did not intend to allow.
- Low risk in current single-agent context (operator controls the workload config anyway).
  Revisit in p7.6 when multi-workload operator may want to restrict beta feature usage.
  Fix: validate against an operator-configured allowlist in `EgressConfig`.

## Phase 7 — Open (deferred from p7.6 review)

**p7.6-ar-02 (P2) — Ephemeral key uses nanosecond timestamp, not CSPRNG**
- `scheduler.rs:439`: `format!("ua-{}-{:016x}", cfg.id, ts_nanos)` — predictable from agent ID (visible at `/agents/<id>/`) + timestamp. A compromised child agent could reconstruct a sibling's key and make proxy requests billed to the sibling's budget.
- Mitigated by: proxy listens on loopback-only (single-tenant, no remote attacker), and agent IDs differ between siblings.
- Fix: replace timestamp with `rand::thread_rng().gen::<u64>()` (16 hex chars of CSPRNG). Requires adding `rand` to `agentd/Cargo.toml`.

**p7.6-ar-03 (P2) — 11 of 17 plan-required tests absent, including env isolation tests**
- Most security-relevant gaps: `universal_agent_env_injection` (child env contains ephemeral key, not real key) and `universal_agent_env_clear` (parent secrets e.g. GITHUB_TOKEN absent from child). These are integration-level tests that require spawning real processes and passing a secret through the allowlist. Also missing: `universal_agent_started_event`, `universal_agent_exit_event`, `universal_agent_sigterm_on_shutdown`, `universal_agent_gvisor_argv`, `universal_agent_send_message_is_error`, `scheduler_universal_only_does_not_exit_prematurely`, `checkpoint_compat_native_only_restores`, `egress_addr_threaded_to_universal_spawn`.
- The env_clear() + allowlist behavior is verified by code inspection (Finding 1 PASS) but not by an automated test.
- Fix: add `universal_agent_env_injection` and `universal_agent_env_clear` as integration tests that spawn a real `printenv`-style child and check the output. Then complete the remaining list.

**p7.6-ar-04 (LOW) — `universal+isolation=none` silently allowed (plan says reject)**
- `universal.rs:62–64`: the plan's validation matrix says `tier=universal + isolation=none → error`. The implementation accepts it and spawns without sandboxing. The `agent_config_universal_defaults` test explicitly asserts `isolation=none` is valid, confirming this is an intentional relaxation for development ergonomics.
- Fix: decide whether `isolation=none` is the allowed dev mode (update plan + doc) or should be rejected (add validation + update test). Currently blocked on that design decision.

**p7.6-ar-05 (LOW) — `UniversalOutputTruncated` event missing; stdout uses `Stdio::inherit()`**
- Plan specifies `universal_output_truncated` event when stdout/stderr exceeds 4 MB, with stdout/stderr captured and forwarded to the flight log. `universal.rs:95–96` uses `Stdio::inherit()` instead, meaning output goes directly to agentd's fd without capture or truncation detection.
- Acceptable scope reduction for v1 (operator sees output in terminal), but `universal_output_truncated` event kind should be added to `events.rs` as a stub so CONVENTIONS.md is internally consistent.

**p7.6-ar-01 (P2) — `dispatch_spawn` collision guard misses `universal_agents` map**
- `dispatch_spawn` checks `state.agents.contains_key(&child_id) || state.outcomes.contains_key(&child_id)` before inserting a dynamic child. Because universal agents live in the separate `state.universal_agents` map (not `state.agents`), a native agent that explicitly requests `child_id = "my-universal-agent"` would collide silently — two agents with the same ID in different maps, producing duplicate FUSE snapshots and confusing `send_message` routing.
- Only triggered by an agent-supplied `child_id` that matches a static universal agent config ID. Auto-generated IDs (`{parent_id}-child-{seq}`) never collide. Single-tenant, operator-controlled config makes this low-probability in practice.
- Fix: extend the collision guard: `|| state.universal_agents.contains_key(&child_id)`.

## Phase 7 — Open (deferred from p7.4 QA)

**p7.4-qa-01 (LOW) — Silent no-op when approval item disappears between List→Confirm mode transitions**
- Repro: operator enters Confirm mode on `act_0`; external process resolves `act_0` via FUSE;
  next tick refreshes `approvals_items` (item gone); operator presses 'a' (approve).
- `approvals_items.get(selected_idx)` returns `None` → no `write_control_command` sent,
  mode silently returns to List with no result message. User may be confused whether approve fired.
- In practice the List view correctly shows 0 items (item was already resolved), so no data loss.
- Fix path: add `result_msg = Some("Item already resolved")` in the `if let Some` else branch
  in `handle_approvals_key` Confirm arms (`agentctl/src/watch/mod.rs`).

**p7.4-qa-02 (LOW) — `read_approvals` has no unit test**
- `agentctl/src/watch/reader.rs::read_approvals()` parses the `/agents/approvals` JSONL file
  (11 lines) but has no coverage for: empty/`[]\n` sentinel, multi-item path, malformed-line skip.
- Fix path: add 3 unit tests to `reader.rs` covering these paths.

## Phase 7 — Open (deferred from p7.7)

**~~p7.7-ar-01~~ (resolved dx.2) — SSE `/api/v1/events` endpoint has no unit test**
- Fixed: `sse_content_type_and_framing` test in `management.rs` verifies the `text/event-stream`
  content-type header and `cache-control: no-cache` header. (Stream framing is not unit-testable
  without a live broadcaster; header correctness is the achievable invariant.)

**~~p7.7-ar-02~~ (resolved dx.2) — `detect_source()` fallback logic untested**
- Fixed: `detect_source_fuse_path_returns_fuse_source` and `detect_source_fallback_to_http_when_no_fuse`
  unit tests added in `source.rs`.

**p7.7-ar-03 (LOW) — `egress_brokered`/`egress_rejected` hardcoded to 0 in `HttpSource`**
- `HttpSource::load_snapshot()` maps `/api/v1/snapshot` JSON to `SchedulerSnapshot` but
  leaves `egress_brokered` and `egress_rejected` as 0 (fields not yet emitted by the HTTP
  endpoint). The FUSE path reads them from live files. Align in dx.2 when the HTTP endpoint
  exposes egress counters.

**~~p7.7-ar-04~~ (resolved dx.2) — `status_detail` not threaded through `agent_info_from_json`**
- Fixed: `agent_info_from_json` now parses `status_detail` from the JSON response;
  `AgentInfo.status_detail` is populated for `Awaiting*` variants; detail shown in AgentDetail view.

**p7.7-ar-05 (INFO) — management server loopback guard is post-bind**
- `management.rs::start()` binds the TCP listener first, then asserts the bound address is
  loopback. A misconfigured `bind_addr` (e.g. `"0.0.0.0"`) would briefly accept on all
  interfaces before the assert panics. Defense-in-depth improvement: validate `bind_addr`
  before `TcpListener::bind()` and reject non-loopback values at config parse time.
  Not exploitable in practice (the panic closes the socket immediately); deferred.

## Phase 7 / Harness — Open (from h7.2)

**~~h7.2-ar-01~~ (resolved v0.50.0) — generic `agent` entrypoint mode**
- Resolved by adding `agent)` mode to `docker/entrypoint.sh`: `agentctl spawn --dry-run` + sed
  path rewrite (`../docker/` → `/etc/agentd/`) covers all standalone templates.
- `docker-compose.yml` now has `agent` service with `HOME=/data`, `AGENTOS_REPO_TEMPLATES_DIR`,
  OAuth env, and `agent-data` named volume. `DRY_RUN_ONLY=1` env enables smoke testing without
  a live API key.

**~~h7.4~~ (resolved v0.51.0) — Docker agent hang (streaming default + connect timeout)**
- Root cause: `ModelConfig.streaming` defaulted to `false` (serde `bool::default()`), causing
  all agents to run silently until `AgentEffect::Completed`. For `google-agent`, the OAuth URL
  emitted in Turn 2 was never printed before `oauth_check_auth` started blocking on port 8585.
- Resolved by flipping default to `true` (`fn default_streaming() -> bool { true }` +
  `#[serde(default = "default_streaming")]`) and adding `connect_timeout(10s)` to the
  `AnthropicGateway` reqwest client.

## Phase 7 / Harness — Open (from h7.1)

**h7.1-ar-01 (P3) — MCP server script paths are hardcoded relative to agentd/ CWD**
- `args = ["../docker/shell_mcp.py"]` in template TOML files works only when agentd is
  invoked from `agentd/` (the CWD assumed by `cargo run`). If agentd is run from the repo
  root or an installed path, the relative path breaks.
- Future fix: support a `${AGENTOS_SCRIPTS_DIR}` interpolation token in `args` that resolves
  to the directory of the running agentd binary, or an env var the operator sets at install
  time. Deferred — relative paths work for the common `cargo run` development case.

## Phase 7 — Open (deferred from p7.3 review)

**p7.3-ar-01 (LOW) — `child_seq` consumed on auto-ID collision with existing named agents**
- `dispatch_operator_spawn` increments `state.child_seq` before the collision guard runs.
  If the auto-generated `"operator-N"` collides with a user-named agent and the command is
  rejected, `child_seq` is permanently incremented — causing gaps in numbering on the next spawn.
- Not data-loss class; only manifests if the operator boots an agent named `"operator-0"` etc.
  Silent rejection is observable via `FuseControlError` in `tail flight.jsonl`.
- Fix path: move the `child_seq` increment to after the collision guard, or generate the ID
  without incrementing (probe loop). Deferred — not worth restructuring the lock for this.

**p7.3-ar-02 (P3) — `agentctl spawn` CLI execs a second agentd instead of using /agents/control**
- The `agentctl spawn <template> --task "…"` CLI path always execs a new agentd binary,
  even when an agentd scheduler is already running with the FUSE surface mounted.
- Correct fix: detect `/agents/control` exists → write JSON there → print confirmation.
  `execute_pending_spawn()` already implements this logic in the TUI watch path; extract
  it as a shared helper so both the TUI and CLI spawn paths route correctly.
- Deferred; tracked in memory as p7.3-cli-revisit. Implement before p8.

## Phase 7 — Open (deferred from p7.2 review)

**p7.2-ar-01 (P2) — `stdout_lock` held across `write_all().await` + `flush()` per chunk**
- `scheduler.rs:make_infer_future`: `tokio::sync::Mutex<()>` is correctly held across the
  async write to prevent byte-level interleaving in multi-agent streaming runs.
- Architectural concern: when stdout is a slow pipe, all concurrent streaming agents
  serialise output at OS write speed. Correct tradeoff for the non-interleaving guarantee,
  but future improvement: dedicated writer task that owns stdout, receives chunks from all
  agents via a bounded channel (one queue-send per chunk instead of one lock+write+flush).
- Only manifests with slow consumers (`agentd … | tee slow_log`); no correctness issue.

**p7.2-ar-02 (P2) — `unbounded_channel` for SSE chunks has no backpressure**
- `scheduler.rs:make_infer_future` uses `tokio::sync::mpsc::unbounded_channel` for SSE chunks.
- If `print_fut` stalls (e.g. holding `stdout_lock` while another agent writes), the SSE
  producer can buffer the full model response in memory before the consumer makes progress.
- In practice, buffered data is bounded by `max_tokens × bytes/token`; not a practical risk
  for typical usage. Improvement: switch to `channel(64)` for natural backpressure.
- Pair with p7.2-ar-01: both resolved together by the dedicated-writer-task architecture.

## Phase 7 — Open (deferred from p7.1 review)

**p7.1-ar-01 (P2) — `McpHttpError` event defined but never emitted**
- `events.rs` defines `EventKind::McpHttpError` and CONVENTIONS.md lists it in the taxonomy,
  but `McpHttpClient::request()` and `McpTool::invoke()` never emit it.
- Threading a `FlightRecorder` into `McpHttpClient` at connect time (similar to how native
  tools access the scheduler) is the right fix path.
- Impact: HTTP tool failures are not flight-logged with the `http_status`/`method` fields
  defined in the taxonomy; they surface only as generic tool errors from the agent loop.
- Fix: pass `Arc<FlightRecorder>` + `agent_id` into `McpHttpClient::connect()`, store on
  the struct, emit `McpHttpError` in `request()` before returning `Err`. Defer to p7.2.

**p7.1-ar-03 (P3) — `agentd/tests/mcp_http.rs` integration test file not written**
- The p7.1 plan specified a live-HTTP-listener integration test suite (tokio listener,
  session-ID continuity, 4 MB guard, pagination, HTTP error status codes).
- Unit tests cover the SSE parser and config validation well; the network-path tests
  require `httpmock` or `wiremock` infrastructure that wasn't added in p7.1.
- Deferred from plan: `docs/plans/p7.1-http-sse-mcp-transport.md`. Fix in p7.2.

**p7.1-ar-02 (P3) — SSRF to RFC-1918 / link-local addresses not blocked** ✅ FIXED in h7.1
- `docker/http_mcp.py` now resolves the target hostname via `socket.getaddrinfo` and checks
  `ipaddress.ip_address.is_loopback / .is_private / .is_link_local` before opening any connection.
  Blocks loopback, `169.254.x`, `10.x`, `172.16-31.x`, `192.168.x`.

## cos.1 / Live Testing — Open

**cos-polish-adv-F1 (P2) — spawn_agent token_budget has no schema maximum and no parent-budget clamp**
- `spawn_agent` accepts any `token_budget` value without validation. An orchestrator prompt could
  inadvertently (or via injection) spawn a child with a budget exceeding the parent's remaining
  budget, allowing unbounded spend.
- Fix: (a) add `"maximum": 5_000_000` to the `token_budget` field in the `spawn_agent` JSON schema
  in `agentd/src/tools/native.rs` (see `SpawnAgentTool::input_schema`); (b) add a parent-budget
  clamp in `dispatch_spawn` (`agentd/src/scheduler.rs`) that caps the child at
  `parent.remaining_budget` — note: `dispatch_spawn` lives in `scheduler.rs`, NOT `native.rs`.
- Found during cos-polish #5+#7 adversarial review (2026-07-11); pre-existing, not introduced by diff.

**cos-polish-adv-F2 (P3) — orchestrator spawns inbox child with full parent capability set**
- `spawn_agent` inherits the parent's full capability set by default. The inbox agent only needs
  `Mcp{google_oauth}` but currently gets `KbRead/KbWrite`, `FsWrite`, `Spawn`, and `cron_trigger`
  access too, violating least-privilege.
- Fix: extend `spawn_agent` with an optional `capabilities` argument that lets the orchestrator
  restrict the child's caps at spawn time; update cos.agents.toml to pass `capabilities = ["Mcp:google_oauth"]`.
- Found during cos-polish #5+#7 adversarial review (2026-07-11); pre-existing, not introduced by diff.

**cos-polish-adv-F3 (P3) — no global token budget ceiling for the full CoS run**
- ~~`[scheduler] global_token_budget = 0` (unlimited). A runaway inbox agent with `token_budget =
  1_500_000` could exhaust API credit if spawned in a tight loop (e.g. cron misfires).~~
- **Resolved (v0.77.0):** `global_token_budget = 10_000_000` set in cos.agents.toml (allows ~3 full
  CoS cycles at 1.5M inbox + 500k curator per cycle).

**cos-polish-adv-F5 (P2) — spawned child agents receive max_turns=20 (default); curator hits MaxTurnsReached**
- `dispatch_spawn` hardcodes `max_turns: crate::config::default_max_turns()` (= 20). The `spawn_agent`
  schema and `SpawnConfig` have no `max_turns` field.
- The curator agent requires: 1× kb_get, N× kb_put (brief + persons + open-items), 1× write_file, final
  answer = ~25+ turns. Silently fails at turn 20, leaving a partial KB.
- The 1.5M inbox token budget is overspecified for a 20-turn limit; actual spend is capped by turns.
- Fix: add `max_turns: Option<u32>` to `SpawnConfig` + JSON schema in `native.rs` + `dispatch_spawn`
  in `scheduler.rs`; update orchestrator task prompt to pass `max_turns = 100` for child agents.
- Found during cos-polish #5+#7 adversarial review (2026-07-11); pre-existing in scheduler.

**cos-polish-adv-F6 (P3) — tokens_spent lifetime-monotonic; orchestrator template non-restartable after 200k tokens**
- `SchedulerCheckpoint` persists `tokens_spent`. The orchestrator template sets `token_budget = 200_000`.
  After one session consuming 200k tokens, the next restore immediately hits BudgetExceeded and the
  agent refuses to run without manual checkpoint deletion.
- `checkpoint_interval_turns = 1` means even partial sessions accumulate toward the ceiling.
- Fix paths: (a) reset `tokens_spent` on restore for interactive REPL agents, or (b) add a
  `per_session_token_budget` field that resets each startup. Document workaround in template comment.
- Found during cos-polish #5+#7 adversarial review (2026-07-11).

**cos-polish-adv-F7 (note) — maxResults=50 with format=full may exceed 200k context on heavy email days**
- Gmail `format=full` includes base64-encoded bodies + attachments. At ~50KB per message × 50 messages
  = 2.5MB JSON ≈ 600k tokens — well above claude-sonnet-4-6's 200k context window.
- When exceeded, the API returns a 400 (not a budget error); inbox agent fails silently with no brief.
- Observed tradeoff: maxResults=20 would silently truncate the candidate pool (outside-voice finding);
  maxResults=50 risks context-window failure on heavy days. A safer fix: switch to `format=metadata`
  for the listing step and fetch `format=full` only for the top-N shortlisted messages.
- Found during cos-polish #5+#7 adversarial review (2026-07-11); accepted tradeoff for now.

**cos-polish-adv-F8 (P2) — `kb_list_segments` native tool missing**
- Agents have no programmatic way to discover which KB segments exist; the current workaround is
  embedding a segment reference table directly in the task prompt, which drifts when operators add
  segments. The durable fix is a `kb_list_segments` native tool that returns the configured segment
  names + classes at runtime, letting the agent query the source of truth instead of relying on a
  static prompt.
- Implementation: add `KbListSegments` to `agentd/src/tools/native.rs`; reads from `MemoryConfig.segments`
  via `ToolContext.task_fp`; guarded by `KbRead` capability (any segment); no writes. Add `kb_list_segments`
  to relevant templates + cos.agents.toml orchestrator + curator task prompts.
- Promoted to P2 in autoplan (2026-07-12) from CEO + Eng dual-voice consensus; segment-name drift is
  the root cause of cos-polish #4 recurring.

**cos-polish-adv-F9 (P2) — no automated drift check between dev and distro overlay cos.agents.toml**
- `agentd/cos.agents.toml` (dev) and `distro/overlay/etc/agentd/cos.agents.toml` (production) must
  be kept in sync structurally (segment lists, capability grants, MCP server blocks). Currently the
  Rust test suite catches `kb_put value=` errors and segment drift, but no check flags differing
  capability sets, missing MCP server blocks, or diverging segment declarations between the two files.
- Fix: add `make lint-cos` Makefile target that: (a) parses both files with `agentd/src/config.rs`
  validation, (b) diffs `[[memory.segments]]` names + classes, (c) diffs `[[agents]]` capability
  grants. Failure fails CI. Complements the Rust tests (which already check per-file correctness).
- DX Review finding (2026-07-12): two-file maintenance is the deepest remaining DX friction for CoS operators.

**cos-ux-01 — TUI lacks per-agent progress and error visibility**
- **Partially addressed by ux.2a (2026-07-13, "Attention"):** the Dashboard now shows an `ATTN`
  column + summary line surfacing Approval-pending, Budget-risk, and Degraded (credential/
  provider) signals per agent, plus a persistent attention strip in `AgentDetail`. **NOT
  closed** — the specific incident that motivated this item (an agent hangs mid-tool-call,
  not erroring, not over budget, not degraded) is the Idle signal, deferred to a follow-on
  increment, **ux.2b**, which reuses the design already fully specified in the superseded
  `docs/plans/ux.2-observe.md` (new `AgentTask` fields for `idle_secs`/`error_count`/
  `last_error_at`, the `CallTools`-dispatch-site fix for in-flight visibility) — re-verify
  that design against current `main` before implementing, don't assume it's still accurate.
- Original text (for context, superseded by the above): during long-running agent turns
  (e.g. inbox agent fetching 20 Gmail messages), `agentctl watch` showed only `running` status
  and a growing context-size counter, with no indication of what tool the agent last called,
  what it returned, or whether errors occurred — the "busy vs. hung" ambiguity ux.2b must close.

## ux.2a — Open (deferred from build, 2026-07-13)

- **ux.2b (P1): Idle + Error attention signals.** Reuses the superseded `docs/plans/
  ux.2-observe.md` plan's already-fully-reviewed design (new `AgentTask` fields, the
  `CallTools`-dispatch-site fix, batch-aggregation semantics) — re-verify against current
  `main` before implementing, not assumed accurate. Closes `cos-ux-01` fully once landed.
- **`filter_agent_id` on `ApprovalsViewState`** (P1, upgraded from P2 — /ship Red Team
  re-assessment, 2026-07-13): Enter-routing for an Approval-pending signal currently lands on
  the unscoped global Approvals list at `selected_idx: 0`, not the specific approval that
  triggered the signal. In a multi-agent deployment with 2+ pending approvals, pressing Enter
  on Agent A's flagged Dashboard row can land the operator on Agent B's approval request
  instead — a real risk, not just a UX papercut, since this UI exists to gate risky actions
  (shell_exec, write_file) behind human approval and an operator who trusts the routing could
  approve/deny the wrong agent's action. Fix: search `app.approvals_items` for the entry whose
  `id` matches the signal's `evidence` (the approval_id) or whose `agent_id` matches the
  selected agent, set `selected_idx` to that index (fall back to 0 with a visible warning if
  not found). Add a regression test with 2+ agents having simultaneous pending approvals.
  Similarly, `View::System`/`View::Credentials` need an analogous "jump to and highlight this
  agent's section" for Degraded-signal routing.
- **Sandbox-degradation (p6.8) as an attention signal** (P3): deferred, not built — whether
  `SandboxSummary` (largely static, startup-time) is a meaningful *runtime* attention trigger
  needs its own Eng verification before adding it to either ux.2a or ux.2b.
- **Generalized "Attention contract v1"** (P3): a richer shared domain model (severity
  taxonomy, escalation/suppression policy, operator SLA) was proposed during CEO review and
  deferred as premature — revisit when ux.4 (proactive push) exists to validate the design
  against a real second consumer.
- **Trace/span view in `AgentDetail`** (P3, user-requested, sequenced after ux.7): reuse
  `otel/src/span_builder.rs`'s hierarchy-computation logic (trace/span/parent-id construction
  from `flight.jsonl`), NOT the `agentos-otel` crate itself (pulls in the full `opentelemetry`/
  `tonic` gRPC stack — unjustified dependency for a TUI client). Extract the pure hierarchy
  logic into a small shared crate both `otel` and `agentctl` can depend on.
- `docs/ROADMAP.md`/`docs/plans/ux-cockpit.md`: rename "ux.2" references to "Attention" (not
  "Attention & Evidence" — collides with ux.6's Evidence view) and split into ux.2a/ux.2b.
- **Unused `surfaces::AttentionReason::severity()`/`label()`** (P3, /review finding): defined
  and exported but never called — `agentctl` independently reimplements the same concept via
  its own mirrored `is_critical()`/`label()` in `reader.rs` (the established mirror-type
  pattern this codebase already uses for every other snapshot field). Kept as forward-looking
  API rather than removed; revisit if it stays unused after ux.2b lands, or wire `agentctl` to
  consume it directly instead of maintaining a parallel copy.
- **No cross-crate wire-compat test for `surfaces::AttentionSignal` ↔ `agentctl::reader::
  AttentionSignal`** (P3, /review finding): a future rename or serde-casing change to one side
  would compile clean on both but silently break parsing at runtime. This is a pre-existing gap
  across ALL of this codebase's mirror-type pairs (`AgentSandbox`/`ServerEnforcement`,
  `CredentialSnapshot`/`SysCredentials`, etc.), not new to ux.2a — fixing it properly means
  adding round-trip tests for every mirrored pair, out of this increment's scope.
- **Universal-tier agents never get attention signals** (P3, /review finding, Codex): hardcoded
  to `attention: vec![]` in `update_snapshot`'s universal-agent branch. Not a regression (their
  `context_tokens`/`credential_providers` were already hardcoded to 0/empty before ux.2a, so a
  budget/degraded signal couldn't have fired anyway) — but universal-tier agents have their own
  distinct egress-budget concept (`ProxyRegistry`'s ephemeral-key budget) that isn't represented
  in `AgentSnapshot` at all today. Building a real universal-tier attention signal needs new
  instrumentation reading `ProxyRegistry` state — genuine scope expansion, not a bug fix.
- **`Vec<AttentionSignal>` deserialization is all-or-nothing** (P2, /ship adversarial finding,
  Codex): both `agentctl::watch::source::agent_info_from_json` and (indirectly, via the same
  `serde_json` mechanics) the FUSE path deserialize the whole `attention` array as one unit —
  if a single element has a `reason` value the running `agentctl` doesn't recognize (e.g. a
  future `idle`/`error` variant from ux.2b, once it exists), the ENTIRE array fails to parse,
  and the current fallback replaces ALL signals for that agent with one
  `EvaluationUnavailable`, silently hiding any co-occurring `ApprovalPending`/`Degraded`/
  `BudgetRisk` signal that was in the same array. Not exploitable today (only one
  `AttentionReason` variant set exists in the wild), but **must be fixed before ux.2b adds new
  variants** — change to per-element parsing that preserves every recognized signal and reports
  only the unrecognized element(s) separately (e.g. `#[serde(other)]` catch-all variant, or a
  manual per-element `Result` fold instead of `serde_json::from_value::<Vec<_>>` on the whole
  array).

## Phase 5 — Open (deferred from p5.1–p5.5 adversarial reviews)

**p5.5-ar-01 (P3) — Posting list loading is O(n) RAM at query time**
- `RedbStore::search()` fetches the full posting list for each query term into a Vec,
  then unions candidates in a HashSet. For large segments (>100k entries), a common
  term's posting list could consume significant memory.
- Fix path: lazy iterator over posting list entries; avoid materializing the full Vec.
  Or add a cap on posting-list size returned (top-k by recency). Defer to p5.6
  or a dedicated search-performance pass.

**p5.5-ar-02 (P3) — Provenance metadata tokenized alongside content for BM25 scoring**
- `store.rs`: the full raw stored JSON (including `agent_id`, `ts`, `class`, `task_fp`
  fields from provenance) is passed to `index::tokenize()` at both write and score time.
  Queries for structural terms like `"scratch"` or `"agent"` match documents whose
  provenance contains those strings, not based on content relevance.
- Fix: extract `value["content"]` before tokenizing; fall back to raw string only when
  the value is not parseable JSON. Defer to p5.6.

**p5.5-ar-03 (P3) — Author filter silently passes entries that lack provenance**
- `store.rs:526`: `unwrap_or(true)` means entries without a parseable `provenance.agent_id`
  field always pass an `author` filter, regardless of who the caller is filtering for.
  The behavior is intentional and tested, but the tool description doesn't document it.
- Fix: add a `provenance_unknown: true` flag on affected hits, or document the inclusive
  default in the `kb_search` tool's description string. Defer to p5.6.

## Phase 10 — Credential manager — Open (deferred from /plan-eng-review cred.3, 2026-07-06)

**cred.3-ar-01 (P2) — OAuth scope granularity in capability system**

`Capability::Credential { provider: Google }` is too coarse. Gmail, Drive, Calendar, and
userinfo are different OAuth grants — an agent that can read Gmail can also call the Drive
API if the provisioned token has both scopes. The fix is `Capability::Credential { provider,
scope_set: HashSet<OAuthScope> }` checked against the token's actual scopes at the broker.

- **Why:** a compromised or over-eager agent with a single `Google` capability can call any
  API the provisioned token was granted, not just the API its task warrants.
- **How to apply:** scope this to cred.5+. cred.3 establishes the `CredentialProvider` enum;
  cred.5 refines the capability shape. The broker already sees the token's scopes at refresh
  time — the infrastructure for enforcement is there.
- **Depends on:** cred.3 (establishes capability variant shape).
- **Where to start:** `agentd/src/capability.rs` (`Credential` variant) + `agentd/src/credential/`
  (token scope introspection at refresh) + `agentctl auth google` (scope selection UI).

~~**cred.3-ar-02 (P2) — `credential_refresh_failed` persistence for agentctl when not attached**~~ **[SUPERSEDED: cred.5 credential visibility surface (FUSE+API+TUI, v0.68.0) + cred.7 `ProviderHealthState` checkpoint persistence (`checkpoint.rs:148`, v0.84.0) make refresh failures durable and visible without a live SSE client. Verified 2026-07-17 (audit.1).]**

The plan says `credential_refresh_failed` must produce a visible agentctl alert. But agentctl
may not be attached when the failure happens (overnight CoS run). The management API SSE
broadcasts the event, but it is ephemeral — if no client is connected, the event is lost.

- **Why:** a token refresh failure at 2am causes the agent to see cryptic tool errors
  ("no tools available") with no persistent diagnosis. Operator discovers it only by
  reading the flight log.
- **How to apply:** add a queryable "last credential failure" field to the management API
  (`/api/v1/status` or a new `/api/v1/credentials` endpoint) so the operator can poll
  even after the fact. `agentctl watch` shows it in the system view.
- **Depends on:** cred.3 (establishes the flight event) + cred.5 (credential surfacing).
- **Where to start:** `agentd/src/management.rs` + `surfaces/src/snapshot.rs` (new
  `CredentialStatus` field on `SchedulerSnapshot`).

**cred.3-ar-03 (P3) — Hot credential reload without agentd restart**

Rotating provisioned credentials (re-running `agentctl auth google` with new OAuth app
credentials) currently requires restarting agentd to re-ingest `/run/secrets`. If the
rotation happens mid-run, agents checkpoint, agentd restarts, agents resume — but the
checkpoint-restore window is still a disruption.

- **Why:** production credential rotations are operational events that shouldn't require
  a full scheduler restart and agent re-initialization. The checkpoint system makes this
  less painful, but the window is still visible to the user.
- **How to apply:** a SIGHUP handler or `agentctl credential reload` command that calls
  `CredentialGateway::reload_secrets()` — re-reads `/run/secrets`, validates schema,
  updates the in-memory credential state. No agent restart required.
- **Depends on:** cred.3 (establishes CredentialGateway).
- **Where to start:** `agentd/src/main.rs` (SIGHUP handler) + `agentd/src/credential/mod.rs`
  (reload path on `CredentialGateway`).

**~~cred.3-ar-04 (P2) — SSRF: upstream_base IP-level block missing~~** ✓ Fixed in cred.3.2 (IP pinning via DNS resolution + `reqwest::ClientBuilder::resolve()` at startup).

`ProviderConfig.upstream_base` is validated for `https://` at startup (cred.3 review fix)
but there is no DNS-resolution check to block private/link-local/loopback IP ranges (e.g.
`https://169.254.169.254/...` for IMDS or `https://192.168.1.1/...`). `oauth_mcp.py`
already has `_is_ssrf_blocked()` with this protection; the credential gateway does not.

- **Why:** a misconfigured or template-injected upstream pointing at an IMDS endpoint would
  cause the broker to attach live bearer tokens to IMDS requests.
- **How to apply:** resolve the hostname in `CredentialGateway::start()` (or lazily on first
  use) and reject if any resolved address is private/loopback/link-local. Mirror the logic
  from `docker/oauth_mcp.py:_is_ssrf_blocked()`.
- **Depends on:** cred.3 (`ProviderConfig` and gateway startup).
- **Where to start:** `agentd/src/credential/mod.rs` — `CredentialGateway::start()`.

**cred.3-ar-05 (P3) — OAuthTokenCache mutex held for full upstream timeout**

`get_or_refresh()` holds `self.state.lock()` across the entire token refresh network call
(up to `CREDENTIAL_REQUEST_TIMEOUT_SECS = 60 s`). All concurrent requests for the same
provider queue on this mutex. A slow or unreachable token endpoint stalls all in-flight
tool calls for that provider for up to 60 s.

- **Why:** serializing concurrent refreshes is correct; holding the lock for a long network
  call causes unnecessary queueing of already-valid requests.
- **How to apply:** add a `CREDENTIAL_REFRESH_TIMEOUT_SECS` constant (e.g. 15 s) and wrap
  the `client.post()` call in `tokio::time::timeout()`. The overall request timeout can
  remain 60 s for forwarding; this shorter timeout is specific to token refresh.
- **Depends on:** cred.3 (OAuthTokenCache mutex change from review).
- **Where to start:** `agentd/src/credential/mod.rs:get_or_refresh()`.

**~~cred.3-ar-06~~ (P1 — cred.3.1 gate) — state_path not read on startup** ✓ Fixed in cred.3.1 (v0.61.0).

`OAuthTokenCache::new()` always initializes `token: None, expires_at: 0, refresh_token: None`.
Even when a valid `state_path` file exists from a previous run (containing a rotated refresh
token), it is never read. The broker always re-fetches using the stale secrets-file refresh
token, which may have been rotated single-use by the provider.

- **Why:** Google's OAuth spec allows (and encourages) refresh-token rotation on each use.
  If the previous run wrote a new refresh token to state_path and then agentd restarted, the
  broker discards that rotated token and re-uses the original — which is now invalid.
- **How to apply:** add `OAuthTokenCache::load_from_disk(state_path)` called at first use,
  inside the mutex, before the fast-path check. Pre-populates token/expires_at/refresh_token
  if the file is valid and not expired.
- **Depends on:** cred.3 (OAuthTokenCache and state_path write path).
- **Where to start:** `agentd/src/credential/mod.rs` — `OAuthTokenCache::new()` + `get_or_refresh()`.

**~~cred.3-ar-07~~ (P1 — cred.3.1 gate) — deny-by-default provider scoping fast path** ✓ Fixed in cred.3.1 (v0.61.0).

When a token is registered with an empty `allowed_providers` list (no `Credential` capability),
a request falls through to step 5 (provider config lookup) and returns 503 "not provisioned"
rather than 403 "denied". The distinction matters: 503 implies a config problem; 403 is the
correct access-denial response.

- **Why:** an agent that should have no credential access receives a misleading error message
  that could cause the operator to provision credentials they should not have.
- **How to apply:** add an explicit fast-path in `handle_credential_request()` before step 4:
  if `allowed_providers.is_empty()`, emit `CredentialDenied` with `reason: "no_providers_configured"`
  and return 403.
- **Depends on:** cred.3 (CredentialRegistry and handle_credential_request flow).
- **Where to start:** `agentd/src/credential/mod.rs` — `handle_credential_request()` step 4.

**~~cred.3-ar-08~~ (P1 — cred.3.1 gate) — header scrubbing is a deny-list, not an allow-list** ✓ Fixed in cred.3.1 (v0.61.0).

`SCRUB_HEADERS` (7 entries) blocks specific headers but passes through all others. A compromised
MCP server can inject `X-Forwarded-For`, `X-Real-IP`, `X-Cloud-Trace-Context`, or any
provider-trusted header, turning the broker into a header injection vector.

- **Why:** the current model assumes the threat list is complete and static. An allow-list
  is the correct model: only forward headers the broker explicitly trusts.
- **How to apply:** replace SCRUB_HEADERS with `PASSTHROUGH_HEADERS` — an explicit allow-list
  of forwarded headers (`content-type`, `accept`, `accept-encoding`, `accept-language`,
  `cache-control`, plus a small set of Google-specific headers). All others are dropped.
- **Depends on:** cred.3 (SCRUB_HEADERS in credential/mod.rs).
- **Where to start:** `agentd/src/credential/mod.rs:SCRUB_HEADERS` + step 10 of `handle_credential_request()`.

**~~cred.3-ar-09~~ (P1 — cred.3.1 gate) — document shared credential service as mesh prerequisite** ✓ Fixed in cred.3.1 (v0.61.0).

The current credential broker is single-host. orch.1 (mesh orchestration) assumes a stable
broker API. This dependency must be documented in the ROADMAP before orch.1 begins.

- **Why:** without explicit documentation, orch.1 might design a conflicting credential model.
- **How to apply:** add a prerequisites note to orch.1 in docs/ROADMAP.md stating that
  cred.3.1 must be green and that multi-host broker support is deferred to cred.5+.
- **Depends on:** cred.3.1 completion.
- **Where to start:** `docs/ROADMAP.md` — orch.1 entry.

**~~cred.3-ar-10~~ (P1 — cred.3.1 gate) — extract LoopbackForwardingProxy to fix guard drift** ✓ Fixed in cred.3.1 (v0.61.0).

`EgressProxy` (egress.rs) and `CredentialGateway` (credential/mod.rs) have separate loopback
HTTP proxy implementations. Security guards (redirect policy, connect timeout, hop-by-hop
header strip, body size cap) live in two copies. A fix in one never flows to the other.

- **Why:** duplicate proxy implementations are a maintenance hazard — cred.3 review already
  found redirect policy drift. Future guards will drift again unless there is one shared struct.
- **How to apply:** extract `LoopbackForwardingProxy` in `agentd/src/loopback_proxy.rs` with
  a single `build_client()` that enforces all guards. Both EgressProxy and CredentialGateway
  delegate to it. External APIs unchanged.
- **Depends on:** cred.3 (both implementations landed).
- **Where to start:** `agentd/src/credential/mod.rs` (GatewayState::new client build) +
  `agentd/src/egress.rs` (start_http_proxy client build).

**~~cred.3-ar-S1~~ (P1 — cred.3.1 gate) — signing key readable by MCP FsRead capability** ✓ Fixed in cred.3.1 (v0.61.0).

The Ed25519 signing key at `cfg.egress.key_path` has no startup guard preventing MCP servers
with `AllowFsRead` from reading it. The existing OV-1 check only covers the evidence *data*
file (FsWrite prefix) and the memory store. An MCP server with a broad FsRead prefix that
includes `key_path` can exfiltrate the private signing key.

- **Why:** the signing key is the root of trust for the entire evidence chain. If it leaks,
  the chain can be forged.
- **How to apply:** add an OV-1 check in main.rs: `egress_key_path` must not fall inside any
  MCP server's `AllowFsRead` prefix (using `normalize_path` + `starts_with`, same pattern as
  the memory store guard in p5.8).
- **Depends on:** cred.3 (main.rs OV-1 pattern established in p5.8).
- **Where to start:** `agentd/src/main.rs` — OV-1 startup invariant block.

**~~cred.3-ar-S2~~ (P1 — cred.3.1 gate) — content_audited: true is a lie** ✓ Fixed in cred.3.1 (v0.61.0).

`EgressBrokered` events hardcode `"content_audited": true` (egress.rs:150) but no content
auditing is implemented. The flight log and OTLP sidecar record a false compliance claim.

- **Why:** this was the audit's #1 finding — security features claimed but not built.
  Any system that parses this event (OTLP sidecar, operators, future compliance tooling)
  will be misled.
- **How to apply:** remove the `"content_audited": true` field from line 150 and update the
  `EgressBrokered` doc comment in events.rs. Do NOT add content auditing in this increment —
  that is cred.3-ar-S3.
- **Depends on:** cred.3 (EgressProxy and egress events).
- **Where to start:** `agentd/src/egress.rs:150` + `agentd/src/events.rs:99`.

**cred.3-ar-S3 (P2) — SecretRewriter claimed but not built**

p7.5 was described as including "boundary secret rewriting" but only signed receipts were
built. No `SecretRewriter` struct exists. The THREAT_MODEL and CLAUDE.md p7.5 description
imply this feature is active when it is not.

- **Why:** the audit's #1 finding was security features claimed but not built. The claim
  must be corrected before cred.4/orch.1 proceed.
- **How to apply (cred.3.1):** de-claim: update THREAT_MODEL.md and CLAUDE.md p7.5 description
  to state explicitly that tool output is NOT scanned for credential-shaped tokens and that
  `SecretRewriter` is not implemented.
- **How to apply (future, P2):** build a real `SecretRewriter` in `agentd/src/tools/mod.rs`
  that scans `ToolResult` content for `sk-ant-*`, `Bearer ` and other credential patterns
  and redacts them before the flight log. At that point set `"content_audited": true` again.
  **Design requirements (cred.3.2 autoplan, eng phase):** use PER-TYPE patterns with explicit
  prefix anchors (`(?:^|\s|")`) to prevent false positives on base64 content, SHA hashes, and
  JWT payloads; include a false-positive test suite; cap match to specific credential shapes
  (`sk-ant-[A-Za-z0-9_-]{40,}`, `ya29\.[A-Za-z0-9_-]+`, `BSA[A-Za-z0-9]{32,}`).
- **Depends on:** S2 completion (content_audited field removed).
- **Where to start:** `docs/THREAT_MODEL.md` + `CLAUDE.md` (p7.5 summary) + `TODOS.md`
  (file this as ongoing P2 work).

~~**cred.3.1-adv-01 (P2) — OAuthTokenCache loses in-memory rotated refresh token on daemon restart**~~ **[FIXED: `load_from_disk()` pre-populates the cache from `state_path` at startup (cred.3.1-ar-06, test T21 `credential/mod.rs:2303`) + cred.7 checkpoint persistence. Verified 2026-07-17 (audit.1).]**

When the broker refreshes a token, the provider may return a new refresh token (rotation).
`get_or_refresh()` stores it in memory, and `write_state_atomic()` persists it to `state_path`.
But `load_from_disk()` only reads the *access token* expiry and refresh token from the secrets
file, not from `state_path`. After a restart beyond access-token lifetime, the broker re-reads
the original secrets file refresh token, which is now invalid if the provider rotated it.

- **Why:** Google (and others) rotate refresh tokens on each use. One unclean restart can
  permanently break OAuth until the user re-authenticates.
- **How to apply:** `load_from_disk()` should try `state_path` first (written by `get_or_refresh`),
  falling back to the secrets file only if `state_path` is absent or expired.
- **Depends on:** cred.3.1 (load_from_disk introduced).
- **Where to start:** `agentd/src/credential/mod.rs` — `OAuthTokenCache::load_from_disk()`.

**cred.3.1-adv-02 (P3 → FIXED in cred.3.2) — DNS rebinding bypasses startup SSRF check on `upstream_base`**

`CredentialGateway::start()` resolves `upstream_base` once at boot. A DNS TTL of zero (or
a short-TTL attacker-controlled domain) can switch from a valid public IP to `169.254.169.254`
(IMDS) after the check passes. All subsequent credential-bearing requests bypass the SSRF guard.

- **Status:** **Fixed in cred.3.2 (Group A, ar-04) via IP pinning.** Resolved IP stored in
  `GatewayState` at startup; per-request connection uses stored IP with SNI set to original hostname.
  A host that rebinds to a private IP after startup is blocked. Test T26 covers the rebinding path.
- **Why:** startup-only DNS checks are a known limitation.
- **Where fixed:** `agentd/src/credential/mod.rs` — `GatewayState` + per-request IP check.

**cred.3.1-adv-03 (P3 → FIXED in cred.3.2) — `ApiKeyQuery` key clobbered by MCP-injected duplicate query param**

For `auth_style = "api-key-query"`, the broker appends `?{key}={credential}` via reqwest's
`.query()`. A compromised MCP server can pre-inject the key param in the request URL
(`?api_key=dummy`). APIs that take the *first* query param occurrence would see `dummy` instead
of the real key, effectively disabling credential injection without breaking the request flow.

- **Status:** **Fixed in cred.3.2 (Group A, D3) — inbound query string sanitized before forwarding.**
  Inbound query string is stripped (discarded) so MCP server cannot inject params into upstream URL.
  Test T35b covers the injection path.
- **Why:** a DoS on credential injection for query-param auth.
- **Where fixed:** `agentd/src/credential/mod.rs` — step 9 upstream URL build (query stripped).

## Phase 10 — Credential manager — Open (deferred from cred.3.2 hardening, 2026-07-06)

These three items are **NON-BLOCKING** for cred.4 and orch.1 — capture here, close as a light
`cred.3.3` cleanup when convenient. Same discipline applies: fix + failing-without-fix test +
adversarial verify of the real failure path.

**cred.3.2-ar-01 (P3) — ar-10 partial: per-handler request logic still duplicated**

`loopback_proxy.rs` now owns the SSRF guards, `is_ssrf_blocked()`, `extract_host()`, and
`base_builder()` (ar-10, cred.3.1). But `egress.rs::handle_proxy_request` and
`credential/mod.rs::handle_credential_request` still exist as separate request-handler
bodies. Header-allow-list enforcement, body-cap, and path-handling logic are duplicated
per-handler — the same drift risk that caused the original ar-10 finding can recur at the
handler level.

- **Why:** a future security fix in one handler's body-cap or header filter will not flow to
  the other. SSRF root cause is fixed but handler-level guards are still two copies.
- **How to apply:** extract a shared `forward_request(client, upstream_url, inbound_req,
  auth_injector: impl Fn(…))` helper in `loopback_proxy.rs` with a pluggable auth injector
  so both handlers delegate to a single place. Accept criteria: one function applies
  redirect/SSRF/header/body-cap; both proxies route through it; a structural guard test
  asserts both handlers call the shared function (pattern analogous to the drift guard for
  `base_builder()`).
- **Depends on:** cred.3.1 (loopback_proxy.rs established).
- **Where to start:** `agentd/src/loopback_proxy.rs` (add helper) + `agentd/src/egress.rs` +
  `agentd/src/credential/mod.rs` (delegate both handlers to the new helper).

~~**cred.3.2-ar-02 (P3) — canonical status line (anti-recurrence for version/doc drift)**~~ **[IMPLEMENTED in audit.1: CLAUDE.md `**Current version:**` canonical line, test-enforced against `agentd/Cargo.toml` by `agentd/tests/repo_consistency.rs` — a stale line now fails `cargo test`.]**

cred.3.2 fixed stale version headers in RUNBOOK.md and THREAT_MODEL.md, but did not add an
anti-recurrence mechanism. CLAUDE.md "Current status" is a long prose log; there is no single
authoritative `vX.Y.Z — shipped/unshipped` line that ROADMAP and RUNBOOK can reference. The
v0.60 audit's #1 class of finding was security claimed-but-not-built; the source was doc drift.
Without an anchor line the drift will recur.

- **Why:** without a canonical current-version line, the prose log becomes the truth source
  for "what's shipped" — it is long, redundant, and diverges from ROADMAP detail as entries
  accumulate.
- **How to apply:** add a single `## Current version: vX.Y.Z (shipped YYYY-MM-DD)` line at
  the very top of the "Current status" section in CLAUDE.md, updated on every merge. ROADMAP
  and RUNBOOK reference it as the single source of truth. No CI enforcement needed (human
  discipline + the audit pattern). Accept criteria: the canonical line exists and is accurate
  for the current HEAD.
- **Depends on:** nothing.
- **Where to start:** `CLAUDE.md` — "Current status" section header.

**cred.3.2-ar-03 (P3) — api-key-header adapter runtime ATTACH-BEHAVIOR test missing**

The cred.3.2 test suite covers the `api-key-header` adapter via a config roundtrip
(`provider_cfg_api_key_header()` fixture), not via a live forwarding path. There is no test
asserting that the credential gateway actually attaches the key as the correct HTTP header on
a forwarded request. The oauth-bearer path has a behavioral integration test (T22); the
api-key-header path does not.

- **Why:** a regression in the header-injection step of `handle_credential_request()` for the
  `ApiKeyHeader` adapter would not be caught by the current tests — the roundtrip test only
  confirms the config struct serializes correctly.
- **How to apply:** add a behavioral test that spins up a mock upstream (via `httpmock` or
  a small `axum` test server), calls `GatewayState::handle_credential_request()` with an
  `api-key-header` provider, and asserts the upstream receives the request with the correct
  `Authorization` (or configured `header_name`) header set to the provisioned key. Same
  adversarial discipline: temporarily remove the header-injection step, verify test FAILS,
  restore, verify PASS.
- **Depends on:** cred.3.2 (api-key-header adapter landed).
- **Where to start:** `agentd/src/credential/mod.rs` — new test `test_api_key_header_attaches_on_forwarded_request`.

**cred.4b-ar-01 (P3) — `_BROKER_URL` not validated to be loopback-only in Python MCP scripts**

`_BROKER_URL` (`AGENTD_CREDENTIAL_GATEWAY_URL`) is used directly in `urllib.request.urlopen` in
`oauth_mcp.py` without validating that the host resolves to a loopback address. The `_is_ssrf_blocked()`
check applies only to agent-supplied target URLs, not to the broker URL itself. Practical risk is low
(the env var is injected by agentd's spawn path, not by the model), but this is a defense-in-depth gap.

- **Fix:** at startup in `_load_config()`, parse `_BROKER_URL` and assert `ipaddress.ip_address(host).is_loopback`
  (or resolve via `socket.getaddrinfo` and check all returned addresses). Emit a startup error if the host
  is non-loopback. Apply the same check to `search_mcp.py`.
- **Where to start:** `docker/oauth_mcp.py` → `_load_config()` broker short-circuit block, after
  populating `OAUTH_PROVIDER_NAME` and `ALLOWED_HOSTS`. Add `import ipaddress`.

**✓ cred.5-ar-01 (closed in v0.75.0) — OAuth token-refresh error body stripped (runtime paths)**

Token-endpoint HTTP response bodies are no longer included in runtime error strings, flight-recorder
events, or `provider_last_error`. Only the HTTP status code is retained. Fixed in
`docker/oauth_mcp.py` (`_do_refresh` + `_exchange_code`) and `agentd/src/credential/mod.rs`
(`get_or_refresh`). Source-scan tests T32–T35 (Python) and `test_token_refresh_error_does_not_include_body`
(Rust) guard against regression.

Note: `agentctl auth google` (`agentctl/src/auth/google.rs`) intentionally retains the body for
`exchange_code()` — this is an interactive operator setup CLI, not the agent runtime. Google token
endpoints return RFC 6749 error codes (e.g. `redirect_uri_mismatch`) but never echo credentials,
so operator-facing diagnostic output is safe and useful for OAuth troubleshooting.

**cred.4-ar-01 (P3) — Caps persistence is per-clean-exit only — in-flight counts lost on crash**

Per-request `persist_cap()` fire-and-forget was removed (cred.4 pre-ship review) to eliminate a
write-after-deregister race (a stale spawn_blocking task reinserts the row after `remove_agent_caps`
clears it, permanently locking out the agent on next restart). As a result, cap counters are only
persisted when the agent cleanly deregisters — a crash mid-session loses the in-flight count.

- **Why:** the race was worse than the lost-on-crash cost. Permanent lockout is user-visible and
  unrecoverable without manual DB edits; losing N counts on crash means the agent gets N extra
  requests on next restart, which is not a security boundary (caps are advisory rate-limits, not
  billing controls).
- **How to apply:** implement a dedicated periodic-flush background task (e.g. `tokio::time::interval`
  every 30 s) that snapshots all counters to `caps.redb` without racing with deregister. The task
  must be cancelled before deregister writes the final clear, so the flush and clear are sequenced.
- **Depends on:** cred.4 (caps infrastructure).
- **Where to start:** `agentd/src/credential/mod.rs` — add a flush task in `CredentialGateway::start()`
  that holds a `Weak<GatewayState>` and aborts when the state is dropped.

**cred.6-ar-01 (P3) — URL-encoded `%26` in query values passes allowlist for custom providers**

The `passthrough_query_params` filter splits on literal `&` (not percent-decoded). A query value
containing `%26` (URL-encoded `&`) passes the key-name allowlist check while encoding an extra
parameter that the upstream server may decode. E.g. `?maxResults=50%26admin=1` — key `maxResults`
passes; Gmail receives `maxResults=50&admin=1`. For Gmail this is inert. For custom `oauth-bearer`
providers with non-Google upstreams, a compromised MCP sidecar could use this to inject params
into the upstream request.

- **Why:** allowlist filtering must operate on URL-decoded keys (after percent-decoding each pair).
- **How to apply:** percent-decode each `pair` before splitting on `=` to extract the key, then
  apply the allowlist against the decoded key. `percent_decode(pair.split('=').next())`. Also
  validate that query values do not contain literal `&` after decode (or encode the value again).
- **Confidence:** 6/10 (theoretical; inert for Gmail; relevant if custom providers are used).
- **Where to start:** `agentd/src/credential/mod.rs:932` — the `filter(|pair| { ... })` closure.

**p5.4-ar-01 (P3) — Version/seq counter can be bumped without a corresponding entry**
- `tools/native.rs:KbPut::invoke`: for both Log and Scratch, the counter increment
  (`next_log_seq` / `next_scratch_version`) commits in its own write transaction before
  `store.put()`. If the `anyhow::ensure!` size check fires between the two calls, the counter
  is permanently advanced (version gap in Scratch; sequence gap in Log). A deliberately oversized
  `content` field reliably triggers this.
- Impact: low — single-tenant cooperative agents are not adversarial. Consumers of log streams
  may see non-consecutive sequence numbers.
- Fix: move the size check on raw content before the counter call, or combine counter increment
  and entry write in a single redb write transaction. Defer to p5.5 or the next tool-ABI revision.

**kv-ar-01 (P2) — `kv_get` returns `""` for both missing key and empty stored value**
- `tools/native.rs:KvGet::invoke`: `None` and `Some("")` both return `Ok(String::new())`.
  The `MemoryRead` flight event's `found` field uses a non-empty heuristic (`!result.is_empty()`),
  misclassifying an empty stored value as a cache miss.
- Fix: return a sentinel (or a separate `exists()` call) to distinguish miss from empty-value hit.
  Requires ABI change; defer to p5.4 or next tool-ABI revision.

**kv-ar-02 (P2) — `try_open` TOCTOU: `path.exists()` check before `open`/`create`**
- `memory/store.rs:try_open`: the exists check and the subsequent `open`/`create` are not atomic.
  On a concurrent `rm`, the path could disappear between the check and the open.
  Low risk for single-tenant use; note for p5.x when multi-process agents share a store path.

**kv-ar-03 (P2) — AlreadyOpen detection uses `format!("{e:?}")` string matching**
- `memory/store.rs:open`: the code matches `"AlreadyOpen"` / `"already open"` / `"already locked"` in
  the debug-formatted error string. Fragile if redb renames the variant.
- Fix: downcast `anyhow::Error` to `redb::DatabaseError` and match the typed variant.
  Safe to defer while redb is pinned to `= "4.1.0"` in Cargo.toml.

**kv-ar-04 (P2) — Silent kv tool skip when `store=None` emits no flight event**
- `tools/native.rs:register_native`: when `kv_get`/`kv_set` are listed but `store` is `None`,
  the tools are silently skipped (no registration, no warning). An agent that expects the tools
  will see `unknown tool` errors at runtime with no log to explain why.
- Fix: emit a `MemoryStoreOpened`-equivalent warning event (or `MemoryError`) in the skip branch.

**p5.2-ar-01 (P2) — `short_term` Vec grows unbounded; no size/depth cap**
- `agent/mod.rs`: `short_term.extend(items)` is never trimmed. A long-running agent that reads large
  files under sustained hard pressure accumulates unbounded `MemItem` records (each `blocks_json`
  can be MB-sized). The vec is checkpointed every N turns.
- Fix: add a `max_short_term_depth: usize` config (or size-in-bytes watermark); evict oldest items
  from `short_term` itself when the cap is hit. Defer to p5.6 (principled eviction policy).

**p5.2-ar-02 (P2) — `MemItem.turn` records eviction time, not original message turn**
- `agent/mod.rs:page_turns(…, at_turn=self.turn)`: all items in a single batch get the same
  `turn` value (the turn when eviction happened), not the turn when the message was generated.
  Items from turns 1, 2, 3 all appear as e.g. `turn=7` in `short_term`.
- Impact on p5.3: temporal ordering within a batch is lost; items appear artificially newer.
- Fix: pass `original_index: usize` derived from message position when building `MemItem`, or
  add an `evicted_at_turn: u32` alongside a separate `original_turn` field. Coordinate with p5.3.

**p5.2-ar-03 (P2) — Conservative `page_count` formula may provide no practical budget relief**
- `context.rs:page_count`: `(len-1)/4` evicts only 1 pair for 5–8 messages. Under sustained hard
  pressure an agent with 7 messages at 91% removes 2 messages; cumulative spend is unchanged; next
  turn is still Hard. Agent may burn many turns evicting tiny amounts of context.
- Fix in p5.6: replace formula with an aggressive mode (evict all available pairs when entering
  Hard for the first time) and/or a token-weighted eviction (evict until Tier-1 size drops below target).

**kv-ar-05 (P2) — Corrupt-quarantine overwrites previous `.corrupt` file**
- `memory/store.rs:try_open`: rename to `<path>.corrupt` silently discards a previous quarantine
  if a second corruption event occurs before the user investigates.
- Fix: include a timestamp or counter in the quarantine name (e.g. `memory.redb.corrupt.1234567890`).

**p5.3-ar-04 (P2) — `mem_recall` loads full namespace into memory before filtering**
- `tools/native.rs:MemRecall::invoke`: `store.iter(&ns)` returns the entire namespace as a
  `Vec<(String, String)>` before sorting and filtering. An agent that calls `mem_remember` thousands
  of times causes `mem_recall` to allocate all stored data on every search call.
- Fix: enforce a max entry count per namespace at write time in `MemRemember`, or implement a
  cursor/range API in `MemoryStore` that applies the limit at the scan level.

**p5.3-ar-05 (P2) — `task_fp` on checkpoint restore uses `messages[0].blocks[0]`; wrong when first block is not Text**
- `agent/mod.rs:from_checkpoint`: `task_fp` is recomputed from the first block of the first message.
  If the first block is a `ToolUse` or `ToolResult` (programmatically injected task), `task_text`
  becomes `""` and all such agents share the same FNV-1a fingerprint (`cbf29ce484222325`), breaking
  provenance isolation in Tier-3 memory.
- Fix: persist `task_fp` directly in `AgentCheckpoint` (serialize/restore the already-computed value
  rather than recomputing from messages). Alternatively, compute from `cp.cfg.task` if present.

**p5.3-ar-01 (P3) — `PREVIEW_CHARS` constant duplicated across two modules**
- `memory/context.rs` and `agent/mod.rs` each define their own `PREVIEW_CHARS = 200`. If they
  diverge (e.g. someone changes one and not the other), content previews in Tier-2 short_term items
  will be truncated inconsistently compared to the inline `content_preview` field built in the agent loop.
- Fix: define once in `memory/context.rs` as `pub(crate)` and import in `agent/mod.rs`.

**p5.3-ar-02 (P3) — No test for `MemoryDistilled` suppression on `mem_remember` failure**
- `tools/mod.rs:ToolRegistry::invoke`: the `MemoryDistilled` post-call hook fires only after
  `tool.invoke(...).await?` succeeds. There is no test that verifies the event is NOT emitted
  when `mem_remember` fails (e.g. oversized content routed through the registry).
- Fix: add a unit test invoking `mem_remember` via `ToolRegistry` with >8 KiB content; assert
  `Err` returned and no `memory_distilled` event recorded.

**p5.3-ar-03 (P3) — No test for `task_fingerprint()` determinism on known inputs**
- `agent/mod.rs:task_fingerprint`: private FNV-1a hash; no test pins the output for a known
  input string, so a refactor could silently shift all stored `task_fp` values.
- Fix: add a unit test with a known ASCII string (and empty string) asserting the exact 16-char
  hex output; verifies stability across refactors.

## Phase 4 — Open (deferred from p4.7 audit)

Findings from `docs/AUDIT-phase-4-6.md` that are real bugs but do not block Phase 5:

**F-006 (P2) — `inject_messages` appends `Block::Text` onto a ToolResult-only user turn**
- `agent/mod.rs:240-241`: after a tool cycle, the last user message is all `Block::ToolResult`;
  injection pushes a `Block::Text` into it yielding mixed content. Anthropic tolerates this today
  but a stricter provider (or future validation) would reject it.
- Fix: when the target user message contains any `ToolResult`, push a *new* user message instead.

**F-007 (P2) — `StopReason::MaxTokens` mislabelled as `BudgetExceeded`**
- `agent/mod.rs:333-344`: a per-response generation cap (`model.max_tokens`) is conflated with the
  cumulative per-agent `token_budget`. The agent emits `budget_exceeded` even when nowhere near budget.
- Fix: distinct event kind (e.g. `max_tokens_truncated`); don't attach the unrelated `token_budget`.

**F-008 (P2) — `StopReason::Other(_)` and empty `EndTurn` complete silently with `""`**
- `agent/mod.rs:346-371`: unknown/future stop reason or end_turn with only a filtered thinking block
  yields `Completed("")` — a silent empty answer reported as success.
- Fix: treat empty extracted text as `Failed`; handle `Other(_)` distinctly with a warning event.

**F-009 (P2) — Global token budget is a soft, post-hoc ceiling**
- `scheduler.rs:608-609`: a single inference can overshoot the ceiling by up to one inference.
- Fix: document "soft ceiling, overshoot ≤ one in-flight inference per agent" in ROADMAP/NOTES.

~~**F-012 (P2) — No `fsync` before/after checkpoint rename**~~ **[FIXED in v0.70.0 (orch.2) = F-05/audit-C3: `sync_all()` after write (`checkpoint.rs:64`) + parent-dir fsync after rename (`checkpoint.rs:196-200`). Verified 2026-07-17 (audit.1).]**
- `checkpoint.rs:47,117`: `write_all` not followed by `sync_all()`; parent dir not fsynced after rename.
  On real power loss the rename or tmp data blocks may not be durable.
- Fix: `f.sync_all().await?` after `write_all`; `File::open(parent).sync_all()` after rename.

~~**F-015 (P2) — `extra_env` can re-inject secrets via operator config**~~ **[FIXED: `extra_env` loop applies `PASSENV_BLOCKLIST` (`tools/mcp.rs:141-146`) — credential vars are blocked with a warning. Verified 2026-07-17 (audit.1).]**
- `tools/mcp.rs:100-102`: the `extra_env` loop from `McpServerConfig.env` runs after `env_clear()` with
  no denylist. An operator (or compromised `agents.toml`) can write `ANTHROPIC_API_KEY = "sk-…"` to pass
  the key explicitly to an MCP subprocess — defeating the F-001 env isolation.
- Fix: apply a short hardcoded denylist (e.g. `ANTHROPIC_API_KEY`, any `*_API_KEY`/`*_SECRET` pattern)
  before inserting `extra_env` keys. Log a warning and drop the offending key.

**F-016 (P2) — `drain_mailbox` can inject messages into a just-terminated agent**
- `scheduler.rs:325`: `drain_mailbox` runs after `sm.step()` regardless of terminal state. When `step()`
  returns `AgentEffect::Completed`/`Failed`, the agent is terminal but the mailbox is still drained and
  messages injected, only to be immediately discarded (or serialized into the terminal checkpoint).
- Fix: check `state.agents[&agent_id].is_terminal()` after `step()` and skip drain when terminal.

**F-017 (P3) — `Net{}` with empty ports grants unrestricted network (worse than no `Net`)**
- `main.rs:568-570`: `Net { ports: [] }` sets `has_net = true` (suppressing `IsolateNetwork`) then skips
  the rule loop (`ports.is_empty()` → `continue`). The result: full unrestricted network with no isolation.
  Declaring no `Net` capability gives `IsolateNetwork`; declaring `Net` with empty ports gives nothing.
- Fix (or document): either treat empty-ports `Net` as "unrestricted but acknowledged" (add a warn event),
  or change the semantics to treat empty ports as equivalent to `IsolateNetwork`. Decide in Phase 5.

## Phase 6 — Open (deferred from p6.6 adversarial review)

**p6.6-ar-01 (P3) — `execute_pending_spawn` leaks a NamedTempFile in `/tmp`**
- `agentctl/src/watch/mod.rs:execute_pending_spawn`: `tmpfile.keep()` makes the tempfile
  permanent. On every spawn, a `/tmp/<random>` agent config is left behind and never cleaned up.
  Single-tenant tool; no secrets in the file. Accepted in QA but noted for housekeeping.
- Fix: write the config to a deterministic path (e.g. `~/.agentos/last-spawn.toml`) and
  overwrite on each spawn; or pass the config via stdin/env rather than a file.

**p6.6-ar-02 (P3) — Template picker does not scroll to keep `template_idx` in view**
- `views.rs:render_spawn`: the picker renders a fixed window; if `template_idx` scrolls out
  of the visible region, the selected template is invisible.
- Fix: track a `template_scroll_offset` that follows `template_idx` (clamp to keep the selected
  row in the visible window). Deferred to next surface polish pass.

## Phase 0 — Technical Debt

**~~P2 — Sync I/O in native tool impls (p0.5)~~** ✓ Done in p2.5.
- `ReadFile`, `WriteFile`, `ListDir` migrated to `tokio::fs` (non-blocking).

**~~P2 — ToolRegistry::register should error on collision (p0.5)~~** ✓ Done in p0.5.

**~~P3 — Per-agent capability scoping for native file tools (p1.4)~~** ✓ Done in p1.4.

**~~P3 — FsRead/FsWrite enforcement assumes absolute paths (p1.4)~~** ✓ Documented in p4.5.
- Assumption documented in `Capability` enum doc comment: relative paths fail-safe
  to deny; `~` not expanded. Production target is Linux with absolute paths.

**~~P3 — Symlink traversal not blocked by capability prefix check (p1.4)~~** ✓ Documented in p4.5.
- Documented in `Capability` enum doc comment. Phase 4 namespace sandbox (p4.2)
  is the correct enforcement layer; IsolateMount/IsolateNetwork mitigate escalation paths.

**~~P3 — 2 MB binary target needs re-evaluation at p0.2~~** ✓ Done in p2.1.
- Switched to `rustls-tls`; static musl binary is 3.1 MB (vs ~1.4 MB macOS debug).
  Acceptable for Phase 2; a dedicated size-audit increment is p2.4.

**~~P3 — flight.jsonl CWD footgun for multi-agent (p1.2)~~** ✓ Resolved in p1.2.
- Resolution: single shared `flight.jsonl` + per-event `agent` field (CONVENTIONS.md invariant).
  All events emitted by `Scheduler::run()` carry the agent_id. Consumers filter by `agent` key.
- **P3 — stdout ordering for multi-agent answers**: answers are printed in completion order (fastest
  agent first), not in config declaration order. Fine for p1.2; a flag or ordered output mode
  may be desirable in a future increment.

**~~P3 — Net capability is advisory (p1.4 intentional)~~** ✓ Enforced in p4.2.
- `caps_to_rules()` now adds `IsolateNetwork` when `Net` is absent. Network isolation is
  enforced at the kernel level via `unshare(CLONE_NEWNET)` for all sandboxed MCP servers
  that don't declare a `Net` capability. `satisfies()` for `Net` remains advisory (no net
  tools exist yet), but the sandbox enforces it independently.

**~~P3 — Net enforcement via Landlock ABI v4 not yet wired (p3.3 deferred)~~** ✓ Done in p4.6.
- `AllowNetConnect { port: u16 }` added to `SandboxRule`; `Net { hosts, ports: Vec<u16> }`
  in `Capability` (`#[serde(default)]` for backward compat). `caps_to_rules()` generates
  `AllowNetConnect` rules from `Net.ports`. Runtime ABI detection: V4 (kernel ≥ 6.7) activates
  TCP port enforcement; older kernels degrade silently (BestEffort). Port-only (not host) is
  enforced at the kernel level — hostname restriction is advisory and remains in `hosts`.
  `EnforcementStatus.landlock_net` and `SandboxApplied enforced.landlock_net` field added.

**~~P3 — MCP server without `capabilities` runs unsandboxed with warn-only (p3.3)~~** ✓ Done in p4.1.
- `[tools] mcp_require_capabilities = true` flag added in p4.1. When set, startup fails
  if any MCP server has no effective sandbox rules. Default remains `false` for backward compat.

**P3 — DenySpawn does not block `clone()`/`clone3()` on x86_64 (p3.3 adversarial review)**
- seccomp filter blocks `fork(57)` and `vfork(58)` but not `clone(56)` or `clone3(435)`.
  A sandboxed MCP server can spawn child processes via `clone(SIGCHLD)`. Classic BPF cannot
  inspect `clone` flags to distinguish thread-create vs. process-create without SECCOMP_DATA_ARGS.
- **Accepted limitation.** `gVisor` (`isolation = "gvisor"`) fully mitigates this — the Sentry
  intercepts `clone3`. For namespace-only mode, this gap is documented in THREAT_MODEL.md.
  Switching to `SECCOMP_FILTER_FLAG_NEW_LISTENER` is deferred to Phase 5 if needed.

**~~P3 — DenySpawn is a no-op on aarch64 (p3.3 adversarial review)~~** ✓ Fixed in p4.5.
- `main.rs` now detects when all enforcement fields are false/none with non-empty rules
  and emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }` instead of a
  misleading `SandboxApplied` with all-false fields.

**~~P3 — SandboxApplied fires even when Landlock degrades to no-op (p3.3 adversarial review)~~** ✓ Done in p4.1.
- `SandboxApplied` payload includes `enforced.landlock` and `enforced.seccomp` booleans
  since p4.1. Operators can inspect the event to see exactly what was active.

**~~P3 — `required_capability_for → None` tools are always visible (p1.4 design)~~** ✓ Documented in p4.5.
- Policy documented in `Tool::required_capability_for` doc comment: `None` tools are
  control-plane primitives (list_agents, send_message) intentionally visible under any
  cap-set, including deny-all. Future tools that should be suppressed must return `Some`.

**~~P3 — Case-sensitive path prefix matching on case-insensitive filesystems (p1.4)~~** ✓ Documented in p4.5.
- Assumption documented in `Capability` enum doc comment. Linux production target is
  case-sensitive; macOS is a dev-environment edge case, not a security gap.

**~~P3 — SchedulerState refactor (p1.5)~~** ✓ Done in p1.5.

**~~P3 — Shutdown drain re-enqueues to exited poll loop (p1.5 red-team)~~** ✓ Done in p1.6.
- Added `shutdown_requested: bool` to `SchedulerState`; `drain_deferred` now
  checks the flag and emits `agent_admission_denied { reason: "shutdown" }`
  instead of re-enqueueing onto the already-exited poll loop.

**~~P3 — EventKind enum in flight_recorder.rs → events.rs at p0.4~~** ✓ Done in p4.5.
- `EventKind` extracted to `agentd/src/events.rs`; re-exported from `flight_recorder`
  so all existing import paths (`agentd::flight_recorder::EventKind`) remain valid.

**~~P2 — MCP tools/list pagination not followed (p0.5 adversarial review)~~** ✓ Done in p2.5.
- `McpClient::spawn` now follows `nextCursor` in a loop until all pages are loaded.

**~~P2 — MCP graceful shutdown (p0.5 adversarial review)~~** ✓ Done in p2.5.
- `McpClient::shutdown()` sends `notifications/shutdown`, waits 5s for clean exit, escalates to SIGTERM, then SIGKILL.

**~~P2 — StopReason::MaxTokens produces empty Ok("") (pre-existing, p0.4)~~** ✓ Done in p2.5.
- `StopReason::MaxTokens` now emits `BudgetExceeded` flight event and returns `AgentEffect::Failed`.

**~~P3 — Buildroot ccache volume not wired (p2.2)~~** ✓ Done in p4.5.
- `BR2_CCACHE=y` and `BR2_CCACHE_DIR=$(HOME)/.buildroot-ccache` added to
  `distro/buildroot.config`. Subsequent clean builds use the host ccache (~2 min vs ~30 min).

**~~P3 — agentd flight log path is hard-coded to CWD (p2.2)~~** ✓ Done in p4.5.
- `--log-path <file>` CLI flag and `log_path` top-level TOML field added. Precedence:
  CLI > TOML > default `"flight.jsonl"`. In the VM `log_path` can be set to
  `/run/output/flight.jsonl` to make the destination explicit.

**~~P4 — `run_probe` ignores `--log-path` (p4.5 review)~~** ✓ Done in p4.6.
- `run_probe` now accepts `log_path: PathBuf`; call site passes
  `resolve_log_path(log_path_override, None)`; uses `FlightRecorder::new(&log_path)`.
  `--probe --log-path /path/to/flight.jsonl` now works correctly.

**~~P2 — Linux-gated code not verifiable on macOS dev machines (p3.1 lesson)~~** ✓ Mitigated in p3.1.
- `make clippy-linux` target added to workspace Makefile; `CLAUDE.md` quality gate updated.
  Required before pushing any branch touching `#[cfg(target_os = "linux")]` code.

**~~P3 — checkpoint.json has no access-control or encryption (p4.3)~~** ✓ Mode restriction done in p4.4.
- `checkpoint.json` is now created with mode 0600 via `write_mode_600()` in `checkpoint.rs`.
  `rename(2)` preserves those permissions on the final file. Test `save_sets_mode_0600` added.
- Encryption at rest remains a future item (THREAT_MODEL.md §3.3).

**P4 — `runsc do` is experimental; full OCI bundle integration deferred (p4.2)**
- `isolation = "gvisor"` wraps the MCP server command with `runsc do -- <cmd>`. The `do`
  subcommand is undocumented/experimental in gVisor and may not be stable across versions.
- Action: build a minimal OCI bundle (config.json + rootfs) on the fly via `runsc run` for
  production-grade gVisor integration. Deferred because `runsc do` suffices for p4.2 exploration.

**P4 — PID namespace via `unshare()` only affects future children (p4.2)**
- `unshare(CLONE_NEWPID)` in `pre_exec` makes the *calling* process's future children be in
  a new PID namespace, but the MCP server itself (after exec) remains in the parent PID namespace.
  To put the MCP server in a new PID namespace, a second fork is needed before exec.
- Action: implement double-fork in `McpClient::spawn` using a pipe to propagate the inner PID
  back to the parent. Deferred to a future Phase 4 increment.

**P4 — `clone3()` bypass remains in namespace-only sandbox path (p4.2)**
- `DenySpawn` seccomp blocks `fork(57)` + `vfork(58)` but not `clone(56)` or `clone3(435)`.
  Combined with `IsolateNetwork`, a sandboxed MCP server can still spawn children in the
  parent PID namespace via `clone3()`. `isolation = "gvisor"` fully fixes this (Sentry intercepts
  `clone3`); the namespace-only path does not.
- Action: accept the limitation for namespace-only mode; document in operator guidance.
  gVisor is the recommended mode for truly adversarial workloads.

**~~P3 — pre_exec sandbox errors are masked as EPERM (p4.1 red-team)~~** ✓ Done in p4.4.
- `mcp.rs::McpClient::spawn` now uses a pre-exec error pipe (`pipe2 + O_CLOEXEC`) on Linux.
  On pre_exec failure, the child writes "sandbox" to the pipe; the parent reads it and includes
  "(sandbox stage: 'sandbox')" in the error message. Previously all failures surfaced as EPERM.

**~~P4 — aarch64 CI runner needed to validate DenySpawn no-op behavior (p4.1 eng review)~~** ✓ Resolved in ma.1 (v0.55.0).
- Added `build-aarch64` CI job using `cross` + QEMU emulation (`ubuntu-latest` +
  `taiki-e/install-action`). Both `agentd` and `agentctl` build, clippy, and test under
  QEMU for `aarch64-unknown-linux-musl`. Size guard (≤ 6 MB) enforced per-arch.
  `Cross.toml` pins the cross Docker image for `ring` compat. `make clippy-aarch64` added
  for local aarch64 clippy. DenySpawn no-op behavior now exercised in QEMU CI on every push.

**~~P4 — `sandbox_probe` fixture not wired to any integration test (p4.1 eng review)~~** ✓ Done in p4.4.
- 3 integration tests added in `tests/integration.rs` (Linux-gated): `allowed_path_read_succeeds`,
  `denied_path_read_fails`, `deny_spawn_blocks_exec` (x86_64-only). Tests spawn sandbox_probe
  directly with `pre_exec` + compiled sandbox rules; verify exit codes 0/1/non-0.

**~~P3 — No `--no-fuse` flag for CI and host dev environments (p3.1)~~** ✓ Done in p4.4.
- `--no-fuse` CLI flag and `AGENTOS_NO_FUSE` env var added to `main.rs`. When either is set,
  the FUSE mount is skipped with `tracing::info!` instead of attempted (no warning on CI).

## ma.2 — Open (pre-existing distro issues, not introduced by ma.2)

**ma.2-ar-01 (P3) — `make test` appends a duplicate `memory0` virtfs mount**
- `QEMU_FLAGS` already contains `-virtfs local,...,mount_tag=memory0,...` (the persistent memory
  volume). The `test` target appends a second `-virtfs` with `mount_tag=memory0` pointing at a
  local `test-memory/` directory. QEMU rejects duplicate device IDs at boot with a fatal error,
  so `make test` cannot succeed when `QEMU_FLAGS` includes the first memory0 mount.
- This is a pre-existing footgun from the three-mount model landing (p5.3.5); it was not
  introduced by ma.2 and affects both x86_64 and aarch64.
- Fix: rename the test override to `memory0` → `test-memory` (a distinct tag), and update
  `/init` to mount `test-memory` over `/run/memory` when the tag is present, falling back to
  `memory0`. Alternatively, strip the `memory0` mount from `QEMU_FLAGS` in the `test` target.

**ma.2-ar-02 (P3) — `make distclean` does not remove the aarch64 output tree**
- `distclean` runs `clean` (removes `output/`) then `rm -rf build/`. After `make build ARCH=aarch64`,
  artifacts land in `output/aarch64/`. Running `make distclean` (without `ARCH=aarch64`) removes
  `output/` (the x86_64 tree) but leaves `output/aarch64/` and the shared `build/output-aarch64/`
  cache directory on disk.
- Fix: expand `distclean` to `rm -rf build/ output/ output/aarch64/`, or better, make the recipe
  independent of `$(ARCH)` by using a glob: `rm -rf build/ output/`.

## obs.3 — Open (deferred from obs.3)

**obs.3-ar-01 (P3) — `BatchSpanProcessor` internal 2048-slot queue drops uncounted**
- The `BatchSpanProcessor` maintains its own internal 2048-slot queue (separate from the
  10,000-slot `mpsc` channel). When spans fill this queue, the SDK silently drops them.
  This drop count is not exposed through any public API and is not reflected in
  `agentos.otel.spans_dropped` (channel-level) or `agentos.otel.export_drops` (flush-level).
- Mitigation: set `OTEL_BSP_MAX_QUEUE_SIZE` env var to a higher value (e.g. 8192) in
  high-throughput deployments. Full fix requires an SDK fork or wrapper — deferred.

## Resolved — obs.2-ar-01 and obs.2-ar-02 (closed in obs.3 / v0.44.0)

**~~obs.2-ar-01 (P3) — Copy-truncate rotation undetected when new file length ≥ old offset~~** ✓ Done in obs.3.
- Fixed via content sentinel: `FileTailer` stores `last_sentinel: Vec<u8>` (last 64 bytes at
  last-consumed offset). On poll, sentinel window is re-read and compared; mismatch → rotation.
  Three guards prevent false positives. Three new unit tests cover the fix.

**~~obs.2-ar-02 (P3) — OTLP backend-down export failures not surfaced in stats~~** ✓ Done in obs.3.
- Fixed via `export_drops: u64` counter + `spawn_blocking(move || p.force_flush())` in all three
  call sites (SIGTERM, SIGINT, periodic stats). New `agentos.otel.export_drops` OTLP metric
  (unit "failures") separate from channel-drop counter. Final stats line now emitted at shutdown.

## Completed

**cos.1 — Daily Operating Brief (Chief of Staff flagship)**
- `agentd/cos.agents.toml`: three-agent system (orchestrator + inbox + curator). Orchestrator:
  `max_turns=200_000`, `token_budget=5_000_000_000`, cron-triggered via `cron_mcp.py`. Inbox agent:
  read-only Gmail via `oauth_mcp.py` (no Spawn/FsWrite). Curator: Haiku model, KB-only.
- All 4 critical eng constraints resolved: max_turns set, budget set to 5B, child IDs date-stamped
  in orchestrator task prompt, `cos.1-eng-04` (mock testing) left as a deferred integration note.
- 3 new templates: `cos-orchestrator`, `cos-inbox`, `cos-curator`.
- `docs/RUNBOOK.md §11` with full first-run OAuth dance and 7 verification commands.
- Template test updated: 10 → 13 expected templates. All 503+ tests pass.

**p6.4 — Topology view (multi-agent graph)**
- `parent_id: Option<String>` on `AgentSnapshot`; insert-only `parent_map` in `SchedulerState` + checkpoint (`#[serde(default)]` for compat).
- `OFF_PARENT = 9`: new FUSE virtual file `/agents/<id>/parent`; `reader.rs` reads it into `AgentInfo.parent_id`.
- `agentctl/src/watch/topology.rs`: `TopologyGraph`, `build_graph()` (512 KB tail cap, directed `message_sent` edges, cycle guard), `render_tree()`, `status_badge()`, `parse_message_edges()`.
- `View::Topology` in `agentctl watch`; `[t]` key; `Esc`/`q` back to Dashboard; ↑/↓ scroll; fixed legend; min 60 cols guard.
- `--log-path` CLI arg; plain-mode topology section; `coordinator-demo.agents.toml` acceptance fixture.
- 455 tests pass; `make clippy-linux` required for surfaces changes.
- **Completed:** v0.30.0 (2026-06-18)

**p6.3 — Read-only TUI dashboard (`agentctl watch`)**
- `agentctl watch` command with three views: Dashboard (agent table), AgentDetail, System.
- `ratatui` 0.29 + `crossterm` 0.28; `--plain` / auto-TTY-detection for non-interactive use.
- `CleanupGuard` (`Drop` + `std::panic::set_hook`) for terminal restore on exit and panic.
- `surfaces/` FUSE amendments: `DIR_STEP` 10→20, `OFF_TOOLS = 8`, `/agents/system/` dir
  with four virtual files (`budget`, `queue`, `sandbox`, `provider`), `SchedulerSnapshot`
  + `AgentSnapshot` field additions; 24 new surfaces tests.
- Pre-landing review hardening (6 items): `is_tty` cached, cross-crate sentinel constants,
  stdout flush in plain mode, `spec_names()` cached as `&[String]`, ANSI sanitizer,
  `debug_assert` → `assert` in `alloc_dir`.
- 565 tests pass; `make clippy-linux` clean.
- **Completed:** v0.29.0 (2026-06-17)

**p3.3 — Landlock LSM + seccomp-bpf sandbox for MCP server subprocesses**
- `sandbox/` crate: `SandboxRule` enum (`AllowFsRead`, `AllowFsWrite`, `DenySpawn`);
  `CompiledSandbox` / `compile()` / `apply_compiled()` / `apply_sandbox()` API.
- Landlock V1 FS rules via raw syscalls (444/445/446); `ACCESS_FS_HANDLED = 0x1FFE`
  (excludes Execute bit to allow initial exec of MCP binary).
- seccomp-bpf filter blocks `fork(57)` + `vfork(58)` on x86_64 only; classic BPF.
- `caps_to_rules()` in `main.rs`: converts agent `Capability` set to `SandboxRule` list.
- `SandboxApplied` / `SandboxSkipped` flight events; `O_NOFOLLOW` on Landlock path fds.
- `CONFIG_SECCOMP=y / CONFIG_SECCOMP_FILTER=y` in `distro/kernel-extras.config`.
- 180 tests pass; Linux-gated tests deferred to CI via `#[cfg(target_os = "linux")]`.
- **Completed:** v0.10.0 (2026-06-11)

**p3.1 — /agents FUSE virtual filesystem**
- `surfaces/` crate: `SchedulerSnapshot` / `AgentSnapshot` / `AgentStatus` snapshot types.
- `AgentsFs` (`fuser` 0.14, Linux-only): root dir + per-agent dirs with `status`, `context_size`,
  `budget`, `flight` virtual files. Inode scheme: root=1, dirs from 1010 step 10, files at dir+1..4.
- `mount()` spawns FUSE thread; returns `FuseMounted` guard (RAII unmount); stubs on non-Linux.
- `Scheduler::new` gains 7th `Arc<RwLock<SchedulerSnapshot>>` arg; `update_snapshot` called after
  every scheduler effect; `AgentTask::context_tokens()` + `task_preview()` supply snapshot fields.
- New flight events: `FuseMounted`, `FuseUnmounted`.
- Workspace promoted: root `Cargo.toml` with `members = ["agentd", "surfaces"]`.
- `distro/kernel-extras.config` adds `CONFIG_FUSE_FS=y`; `distro/overlay/agents/` mount point.
- 188 tests pass (all platforms); negative FUSE read offset guard added post review-army.
- **Completed:** v0.9.0 (2026-06-10)

**p2.5 — Deferred cleanup (sync I/O, MCP pagination, MaxTokens, graceful shutdown)**
- Native tools (`ReadFile`, `WriteFile`, `ListDir`) migrated to `tokio::fs`.
- `McpClient::spawn` follows `nextCursor` in a loop until all pages loaded; capped at 100 pages.
- `StopReason::MaxTokens` now emits `BudgetExceeded` flight event and returns `AgentEffect::Failed`
  instead of silent `Ok("")`.
- `McpClient::shutdown()` sends `notifications/shutdown`, waits 5s, escalates to SIGTERM then SIGKILL.
- **Completed:** v0.8.0 (2026-06-09)

**p2.3 — Boot/supervision basics (SIGTERM/SIGINT handling)**
- `loop { tokio::select! { ... } }` in `Scheduler::run()` replaces `while let`.
- SIGTERM/SIGINT arms set `shutdown_requested = true` and break; deferred drain runs as before.
- `EventKind::SystemShutdownRequested` flight event emitted on signal.
- 1 new test: `sigterm_drains_scheduler` — sends SIGTERM, asserts < 5s exit + flight event.
- Essential mounts and zombie reaping required no code (handled by `/init` and tokio respectively).
- **Completed:** p2.3 (2026-06-09)

**p0.1 — Crate scaffold + config + flight recorder**
- Created `agentd/` binary crate with Config (TOML), FlightRecorder (append-only JSONL),
  EventKind enum, CI workflow, README, LICENSE.
- All acceptance criteria met: `cargo build` + `cargo clippy -D warnings` + `cargo test` pass.
- **Completed:** v0.1.0 (2026-06-07)

**p0.2 — Inference gateway + Anthropic backend**
- Added `InferenceGateway` trait, neutral message/tool types, `AnthropicGateway`
  (Anthropic Messages API), `--probe` smoke-test mode, 120s HTTP timeout.
- All acceptance criteria met.
- **Completed:** 2026-06-07

**p0.3 — Tool ABI + native tools**
- Added `Tool` trait, `ToolRegistry` (warn on collision, sorted specs), and three
  native tools: `read_file` (100k-char cap), `write_file` (mkdir-p), `list_dir`
  (sorted, `/`-suffixed dirs). `register_native(reg, &["all"])` wires them up.
  `tools_registered` flight event emitted at startup.
- All acceptance criteria met.
- **Completed:** 2026-06-07

**p0.4 — The agent loop (perceive → infer → act → observe)**
- `agent::run()`: full perceive → infer → act → observe loop with flight events.
  Token budget guard, max-turns guard, tool errors as `is_error` blocks.
- `main.rs`: stdin fallback for task, final answer on stdout.
- All Phase 0 flight events emitted.
- **Completed:** 2026-06-07

**p0.5 — Real MCP stdio client**
- `McpClient`: newline-delimited JSON-RPC 2.0 over tokio::process::Child (kill_on_drop).
  Handshake: initialize → notifications/initialized → tools/list. `tools/call` for invocation.
- `McpTool` implements `Tool`; `isError: true` → `anyhow` error.
- `ToolRegistry::register` now errors on collision (upgraded from warn).
- `echo-mcp` fixture binary + integration tests for MCP startup, coexistence, missing-server.
- Release binary: 1.4 MB on macOS.
- **Completed:** 2026-06-07

**p1.1 — Agent as a sans-IO state machine**
- `AgentTask` + `AgentEffect` (`#[must_use]`) + `step()` + `provide_inference()` + `provide_tool_results()`.
- Terminal guard on all `provide_*` and `step()` calls; MaxTurns fires before InferenceRequest.
- `agent/mod.rs` + `agent/driver.rs` split; driver is backward-compat shim.
- Unit tests: `step_machine_text_tool_text_cycle`, `max_turns_fires_before_infer_request`, `provide_inference_on_terminal_task_is_noop`.
- **Completed:** 2026-06-08

**p1.2 — The scheduler (multi-agent, cooperative)**
- `Scheduler` in `agentd/src/scheduler.rs`: `HashMap<String, AgentTask>` + `FuturesUnordered` drive loop.
  `Scheduler::new()` validates duplicate IDs. `Scheduler::run()` owns all IO concurrently.
- `config.rs`: `[[agents]]` multi-agent form + `agent_configs()` + backward-compat `[agent]` single form.
- `run_tools_sequential` extracted as `pub(crate)` in `agent/mod.rs`, shared by driver and scheduler.
- `agents.toml`: example two-agent config.
- `main.rs`: uses Scheduler for all runs; exit non-zero if any agent fails; stdin fallback preserved for single form.
- 4 scheduler tests + 8 config tests. All 74 unit + 16 integration tests pass.
- **Completed:** 2026-06-08

**p1.3 — Metered scheduling & admission control**
- `SchedulerConfig` in `config.rs`: `global_token_budget` (u64) + `max_concurrent_inferences` (usize); wired into `Scheduler::new`.
- Per-agent `priority: u32` field (default 0); `BinaryHeap<DeferredInfer>` keyed by `(priority desc, seq asc)`.
- `enqueue_or_defer` / `drain_deferred` manage the admission lifecycle in `scheduler.rs`.
- Flight events: `agent_scheduled`, `agent_deferred`, `agent_admission_denied`.
- `in_flight` underflow guards promoted from `debug_assert!` to `assert!`.
- New config tests: `scheduler_config_explicit_values_parse`, `scheduler_config_defaults_to_unlimited`, `agent_priority_parses_from_toml`.
- **Completed:** v0.3.0 (2026-06-08)

**p1.4 — Capability system**
- `Capability` enum (`FsRead{prefix}`, `FsWrite{prefix}`, `Net{hosts}`, `Mcp{server,tools}`, `Spawn`).
- `normalize_path` + `satisfies` + `satisfies_type` in `capability.rs`.
- `Tool::required_capability_for` + enforcement at `ToolRegistry::invoke`.
- `filtered_specs(cap_set)` — per-agent model context filtering.
- `CapabilityDenied` flight event; `capability_denied` in `flight.jsonl`.
- `McpTool::server_name` for Mcp{} cap gating.
- 130 tests pass (unit + integration + MCP + MCP client).
- **Completed:** v0.4.0 (2026-06-08)

**p1.5 — Inter-agent spawn-await**
- `spawn_agent` tool: parent with `Spawn` cap creates a child agent; child runs to completion;
  result injected back into parent as a `ToolResult`. Sole-call guard enforced.
- `AgentEffect::SpawnAgent { call_id, config }` — intercepted by scheduler before `invoke()`.
- `SpawnConfig` in `config.rs`: `task` (required), `child_id`/`priority`/`token_budget` (optional).
- `SchedulerState` struct consolidates all mutable scheduler state (`agents`, `outcomes`, `pending`,
  `deferred`, `in_flight`, `tokens_spent`, `awaiting`, `child_seq`, `spawn_depths`, `max_spawn_depth`).
- `dispatch_spawn` / `handle_agent_terminal` in `scheduler.rs` manage the full spawn lifecycle.
- Spawn depth limit: `max_spawn_depth: u32` in `[scheduler]` TOML (default 4; 0 = disabled).
- `agent_child_result_delivered` flight event.
- `Capability::Spawn` `satisfies()` fix; `SchedulerConfig::Default` fix (max_spawn_depth was 0).
- `send_message` deferred to p1.6 (Agent Cards increment).
- 133 tests pass (unit + integration).
- **Completed:** v0.5.0 (2026-06-09)

**p2.1 — rustls + static musl binary**
- Switched `reqwest` from `native-tls` to `rustls-tls`; all 142 tests pass.
- Cross-compiled `x86_64-unknown-linux-musl` via `cross` (Docker); binary is `static-pie linked, stripped`, 3.1 MB.
- **Completed:** v0.7.0 (2026-06-09)

**p2.2 — Buildroot minimal rootfs**
- `distro/` external Buildroot tree: x86_64 musl + BusyBox, cpio.gz initramfs, `make build/run/test`.
- `/init` PID-1 sh: mounts proc/sys/9p shares, sources `agentos.env`, `exec`s agentd.
- Two virtio-9p mounts: `secrets0` (API key) + `output0` (flight.jsonl visible on host).
- `make test` boots with `-no-reboot`, checks flight.jsonl for `agent_completed` event.
- **Completed:** p2.2 (2026-06-09)

**p1.6 — Agent identity & Agent Cards (discovery)**
- `AgentCard { id, name, description, skills }` derived from `AgentConfig` at scheduler seed; `agent_card_registered` flight event.
- `AgentConfig` gains `name`, `description`, `skills` optional TOML fields.
- `bus.rs`: `MailMessage` + `Mailboxes`.
- `list_agents` tool: sorted JSON array of all AgentCards; no capability required.
- `send_message` tool + `AgentEffect::SendMessage`: sole-call; scheduler delivers to mailbox; synthesizes ToolResult; unknown recipient → `is_error` (no panic).
- Mailbox drain before each inference; `inject_messages` appends to last User message (no consecutive-User-message violation).
- Shutdown drain fix: `shutdown_requested` flag in `SchedulerState`.
- New flight events: `agent_card_registered`, `message_sent`, `message_received`.
- 142 tests pass (unit + integration).
- **Completed:** v0.6.0 (2026-06-09)

**p5.1 — Storage primitive (redb-backed MemoryStore)**
- `memory/` module: `MemoryStore` trait + `RedbStore` backend (`redb` 4.1.0).
- `kv_get` / `kv_set` native tools; `KbRead` / `KbWrite` capability gating.
- `MemoryStoreOpened`, `MemoryRead`, `MemoryWrite`, `MemoryError` flight events.
- `[memory]` TOML section with `enabled`, `path`, `table` fields; `memory.enabled` default false.
- `FORMAT_VERSION` stored as metadata; `try_open` with TOCTOU-noted lock; 0600 mode on db file.
- 304 tests pass.
- **Completed:** v0.18.0 (PR #26)

**p5.2 — Per-agent short-term memory + paging**
- `memory/context.rs`: `MemoryPressure` enum (`None`/`Soft`/`Hard`), `assess()`, `page_count()`, `page_turns()`.
- `MemItem { turn, role: Role, content_preview, blocks_json }` stored in `short_term: Vec<MemItem>` on `AgentTask`.
- Soft threshold 75% → edge-triggered `MemoryPressureAdvisory` (fires once on `None→Soft` transition, not every turn).
- Hard threshold 90% → `page_turns()` evicts oldest turn PAIRS (preserves alternating-role invariant); Hard+n=0 path emits advisory once on first entry.
- `last_pressure: MemoryPressure` runtime field on `AgentTask` for edge-triggering; not checkpointed (resets to None on restore, correct behavior).
- `FORMAT_VERSION` 1→2; `#[serde(default)] short_term` for backward compat; `to_checkpoint`/`from_checkpoint` updated.
- `MemoryPressureAdvisory` + `MemoryPaged` flight events; both documented in `docs/CONVENTIONS.md`.
- `content_preview` covers all three `Block` variants (`Text`, `ToolResult`, `ToolUse`); `debug_assert!` for alternating-role invariant.
- 322 tests pass (14 new unit tests covering all acceptance criteria).
- Deferred: p5.2-ar-01 (unbounded Vec → p5.6), p5.2-ar-02 (at_turn stamps → p5.3), p5.2-ar-03 (conservative eviction → p5.6).
- **Completed:** v0.19.0

**p5.3 — Per-agent long-term memory + checkpoint coexistence**
- `ToolContext` struct (`tools/mod.rs`): `{ agent_id, turn, task_fp }` injected into every `Tool::invoke`. `task_fp` = FNV-1a 64-bit hash of initial task text, 16 hex chars; recomputed on restore.
- `MemRemember` tool: `mem_remember { content, tags }` — stores JSON entry with provenance under `agent/{id}` namespace; nanosecond-timestamp key; 8 KiB limit. Returns `None` from `required_capability_for` (implicit self-grant).
- `MemRecall` tool: `mem_recall { query, limit }` — iterates `agent/{id}` namespace, substring match, newest-first. Default 10, max 50 results.
- `EventKind::MemoryDistilled` — emitted post-call by `ToolRegistry::invoke` for `mem_remember`.
- All existing `Tool::invoke` signatures updated to accept `ctx: &ToolContext`; test helpers updated in `native.rs`, `mod.rs`, `mcp_client.rs`, and `memory_integration.rs`.
- 331 tests pass (9 new; up from 322 in p5.2).
- **Completed:** v0.20.0

**p5.7 — FUSE `/agents/<id>/memory/` + `/agents/kb/`**
- `MemoryAccess` trait in `surfaces/src/lib.rs`: `list_namespaces`, `list_keys`, `get_entry`.
- `MemoryAccessBridge` newtype in `main.rs` (Linux-only) bridges `Arc<dyn MemoryStore>` → `Arc<dyn MemoryAccess>`.
- `AgentsFs` extended: `INO_KB=9`, `AGENT_NS_PREFIX`, `MAX_DIR_KEYS=100`, `MAX_SHORT_TERM_PREVIEWS=20`; dynamic inode pool (`DYNAMIC_INO_START=1_000_000`) for `LtFile`/`KbSeg`/`KbFile` inodes.
- FUSE lookup/readdir for `memory/`, `memory/short_term`, `memory/long_term/`, `memory/long_term/<key>`, `kb/`, `kb/<seg>/`, `kb/<seg>/<key>`.
- `AgentSnapshot::short_term_previews: Vec<String>` populated in `update_snapshot`.
- `MemoryStore::list_keys` added (key-only range scan, skips value deserialization).
- Correctness fixes: existence check before `alloc_dir`/`alloc_kb_seg` in lookup; single `get_entry` per LongTermDir/KbSegDir lookup; removed double RwLock in `OFF_SHORT_TERM`.
- Deferred: `list_namespaces` full-scan → p5.8 (NAMESPACES table).
- 445 tests pass (33 surfaces + 412 agentd).
- **Completed:** v0.24.0
