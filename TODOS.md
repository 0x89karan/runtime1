# TODOS

## attn.4 review residuals — Open (2026-08-07, filed at /review)

Three findings from `/review`'s adversarial + security specialist passes on attn.4 (scheduler-
native cron), judged as genuine design questions rather than mechanical fixes — the review's
other findings (a real double-fire bug across restarts, a dead enum variant, a boot-init
history-discard bug, a croner field-count leniency gap, and several test-coverage gaps) were
all fixed directly in the same pass; these three were not.

- **attn.4-clock-01 (P2) — no sanity bound on the system clock before trusting it to compute
  schedules at boot.** `agentd` is explicitly designed to eventually run as PID 1 on bare
  metal, where NTP may not have synced yet at boot. A garbage clock (e.g. near-epoch-zero)
  produces a garbage `next_fire_ts`/occurrence identity, gets checkpointed, and the instant
  NTP steps the clock forward every job whose stale `next_fire_ts` is now in the past fires
  once as a "catch-up" — not a thundering herd, but a fire at an unpredictable,
  clock-dependent moment rather than the operator's intended schedule, with nothing
  distinguishing "clock was wrong" from "we were legitimately down." Needs a design decision
  (refuse to trust `now` before some sanity threshold? require an NTP-synced signal at boot?)
  before it's buildable, not a mechanical fix. Found by the adversarial review subagent.
- **attn.4-croner-01 (P3) — `croner`'s panic-safety on adversarial/malformed input is
  unverified.** `Job::validate_schedule` (config.rs) is called for every job at boot and is
  documented to degrade ONE job to manual-fire-only on a parse error — never fail the whole
  boot. That contract only holds if `croner::Cron::from_str` always returns `Err` rather than
  panicking on some pathological input (huge field values, unusual unicode, extreme ranges).
  Nothing in this codebase pins that contract with a fuzz/property test against the actual
  pinned `croner` version. Low priority: `croner` is a maintained third-party crate, not
  hand-rolled code we're the primary auditor of, and this is a boot-time-only path.
- **attn.4-ratelimit-01 (P2) — native cron removes the only soft rate-limiting friction on
  side-effecting scheduled jobs.** Before attn.4, an LLM had to actively decide to call
  `run_job` each time (a weak throttle). A native `schedule` now fires unconditionally on
  every matching cron tick, bounded only by the pre-existing per-fire `token_budget` (already
  documented elsewhere as "per-fire, not a daily fence"). A schedule like `"* * * * *"` on a
  job holding write/Gmail capabilities would now fire every minute with no LLM in the loop to
  notice something is wrong — a config typo has a bigger blast radius than it used to. Not a
  new authz bypass (same actor who can edit `cos.agents.toml` could already grant itself any
  capability directly), but a real operational-risk increase worth a considered answer: a
  per-job max-fires-per-window guard independent of `token_budget`, or at minimum an
  `agentctl jobs` warning when a schedule fires more often than some threshold. Found by the
  security specialist.
- **attn.4-watch-01 (P2) — Task T4's `agentctl watch` row was never built.** T4's stated scope
  was both a CLI dry-run (`agentctl jobs`, built) and a live dashboard row surfacing next/last
  fire, occurrence id, skip reason, and fingerprint (not built). `surfaces::JobScheduleView` is
  populated in `update_snapshot` but has no consumer in `agentctl/src/watch/` — the data is
  exported and then read by nothing. Filed at plan-completion audit, not discovered later.
  Operator visibility until this lands: `agentctl jobs <config>` (dry-run) plus the
  `job_fired`/`job_fire_skipped`/`job_schedule_degraded` flight events via the existing Logs
  view. See `docs/plans/attn.4-scheduler-native-cron.md` T4.

## attn.2-R5 fix-batch /review residuals — Open (2026-08-07, filed at /review)

One finding from `/review`'s adversarial pass on the T1-T5 fix batch, judged a genuine design
question rather than a mechanical fix — the pass's other findings (a checkpoint/restore
lease-loss bug, a permanently-dropped cron occurrence under a concurrent manual fire, an
incomplete leak fix, several stale comments, a missing `?` help-overlay key, and an audit-log
gap on four pre-dispatch failure branches) were all fixed directly in the same pass.

- **attn.2-R5-string-01 (P3) — `agentctl`'s `explain_verb_error` string-matches a literal
  ("timed out waiting for run_job") owned by a DIFFERENT crate (`agentd/src/management.rs`).**
  No shared constant, no cross-crate test pinning the coupling. If that server-side wording
  ever changes, the client-side match silently breaks and falls through to the generic "Action
  not sent, retry" advice — which is actively wrong for this one case (the fire may have
  already started; blind retry risks a second concurrent run). A real fix needs either a small
  shared crate both `agentd` and `agentctl` depend on for this one literal, or a structured
  machine-readable error code in the JSON body instead of prose-matching — genuine design work,
  not a mechanical fix, and no wording change is pending today that would trip it. Operator
  chose to file rather than fix at the review gate (2026-08-07).

## zk track — Open (reshaped at /autoplan premise gate, 2026-08-06)

Plan + increment ladder: `docs/plans/zk.1-hybrid-crypto-verification.md`. The inline-zkVM
"hybrid cryptographic verification" brief was rejected 6/6 cross-model and reshaped by the
operator into **attn.4 → gov.1 (ux.6b B2 coverage + key custody + audit packet, BOTH packet
variants per D6) → gov.2 (auditor-side trust root: key enrollment + anchor registry +
expected-batch registry + image pinning; SCHEDULED by operator override D5) → zk.0 (pure
policy-eval crate) → zk.1 (decision journal + native replay verifier) → zk.2 (batch replay
prover, PARTNER-GATED — gate sharpened at the 2026-08-06 red-team pass to "a party who must
verify compliance but must NOT see the policy, named in writing"; separate binary, never in
agentd/PID-1) → zk.3 (fleet verifier sweep)**. A cross-model red-team pass (Claude RT-1..12 +
Codex C1..12, 2026-08-06) confirmed 6 CRITICAL themes; per-increment mandatory question lists
live in the plan's red-team addendum — each /autoplan MUST consume its list. zk.0/zk.1 stay
unconditional by operator override D4 (both voices recommended gating them with zk.2).

- **zk-01 (P3) — RFC 3161 timestamping of evidence segment heads.** Deferred from gov.1's
  scope: needs an external-service choice (TSA selection, availability semantics) before it is
  buildable. The `(pubkey, max_seq)` out-of-band anchor in gov.1 covers rewind detection
  without it.
- **zk-02 (P3) — TEE attestation (Nitro/SEV-SNP/TDX) considered, not scheduled.** The only
  technology in the review that addresses "did the host run this code" (the omission seam zk
  cannot close). Pairs naturally with mv.3's microVM tier — engage it there, not before. Do
  not let a future session re-derive the comparison: {custody fix, external anchor, WORM
  export, TEE, zk} was run against the two real auditor questions (completeness, custody) and
  zk lost on both; zk's surviving value is policy-privacy of the replay proof (zk.2).

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
- ~~**audit86-P1-2 (P1) [re-rate from P3] — flight.jsonl has no rotation anywhere.**~~ **[FIXED in run.1 (v0.99.0), struck 2026-07-28 after a code check: `flight_recorder.rs:22-64` rotates in place at `MAX_FLIGHT_BYTES = 100 MB` — an `AtomicU64` size counter seeded from file metadata at open (`:43`) and bumped per write, `set_len(0)` under the recorder's file mutex so the inode is preserved and the otel `tail.rs` sentinel follows it, best-effort (a failed rotation falls through to appending rather than crashing an agent). Two tests: `record_rotates_in_place_when_over_cap` + `new_seeds_size_from_metadata_so_a_preexisting_oversized_log_rotates` (the run.1-ar-01 regression for the metadata seed). CHANGELOG v0.99.0.]**
  `flight_recorder.rs:23-33` appends with no cap; nothing in `entrypoint.sh`, `overlay/init`,
  or the image rotates it. The otel sidecar *detects* rotation nobody performs. With
  streaming default-on, `InferenceStreamDelta` per SSE chunk ⇒ ~10–100 MB/day for a 24/7 CoS,
  filling `cos-data` in months. Fix: size-threshold copy-truncate self-rotation in
  `FlightRecorder::record` (the otel sentinel already survives copy-truncate). → `run.1`.
- ~~**audit86-P1-3 (P1) [re-rate; = p5.2-ar-01/audit-C8] — `short_term` grows unbounded for parked agents.**~~ **[FIXED in run.1 (v0.99.0), struck 2026-07-28 after a code check: `agent/mod.rs:12` `MAX_SHORT_TERM = 1000` + `cap_short_term()` (`:361-372`) ring-drops the OLDEST paged summaries beyond the cap, so RAM and the per-turn `to_checkpoint` clone stay bounded for a never-terminating agent whose only other drain (run-completion distillation) never fires. Safe because these are already-evicted per-turn summaries, not live tool-call/result pairs. Tests assert the checkpoint clone is bounded (`:2355`). CHANGELOG v0.99.0. **Remainder (by design, not a P1):** mid-run distillation of the dropped context is still deferred — the summaries are discarded, not folded into long-term memory.]**
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
- ~~**audit86-P1-5 (P1→ci.1 remainder) — entrypoint sed-rewrite pipeline has zero test coverage.**~~ **[audit.1: guards + dry-run hooks. ci.1 (v0.88.0): docker-smoke CI job runs cos/agent DRY_RUN_ONLY + binary probe + PR-#124-class negative fixture on every PR; the fixture exercises the extra-pattern concat branch (positive-form hit → `17:store_path` named). Still unexercised: the grep rc>=2 internal-error refusal branch (needs a grep malfunction to trigger — accepted residual). Known coverage limit (ship adversarial): the extra ERE only matches `*_path`/`*_dir` keys — a bare `path = "x"`/`dir = "x"` relative value without `./` escapes it; the class dies with the sed pipeline in par.2. **[STILL OPEN — par.2 /autoplan-recovery (2026-07-25): an ERE-tightening fix to `[a-z_]*(path|dir)` was built then DROPPED — a bare `path`/`dir` key is not reachable by a VALID config (no such field in the Config schema; `agentd check --strict` rejects it before boot anyway), and its negative-control fixture broke `config_parse_all` (which requires `.github/fixtures/` to parse). The genuinely-reachable escapes (`prefix = "output"`, relative MCP `args`) aren't caught by the key-suffix ERE either — the real fix is the runtime path-identity newtype in cap.3 (audit86-P1-8), which absolutizes at deserialization instead of string-matching in a boot grep. This remainder is superseded by cap.3, not by a bigger grep.]** **[cap.3 SHIPPED in v0.107.0 (see audit86-P1-8 above) — the path-identity absolutization this was waiting on is in, so this remainder is closed too (noted 2026-07-28).]**]**
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
- ~~**audit86-P1-8 (P1) [new] — "Relative paths fail closed" is false; path identity is string identity.**~~ **[FIXED in cap.3 (v0.107.0), struck 2026-07-28 after a code check: `capability.rs`'s `anchor_abs` (`:60-75`) is the SINGLE place the CWD anchor enters matching — it absolutizes BOTH grant and request against the startup CWD (`fs_anchor`) before normalizing, so matching is absolute-vs-absolute and no longer string-identity/CWD-blind. The doc comments name this fix explicitly ("cap.3 / audit86-P1-8", `:110-115`). Deliberately NOT behaviour-preserving: a relative request inside an absolute grant now correctly ALLOWS where the old lexical `starts_with` denied. cap.3 also closed the p5.8 boot containment hole. CHANGELOG v0.107.0.]**
  `capability.rs:27-29` documents fail-closed, but `normalize_path` (`:76-94`) strips `CurDir`,
  so a relative grant matches a relative request textually and CWD-blind; production dev-mode
  (`cos.agents.toml:247,375`) depends on the undocumented behavior. This is the v0.86.2 root
  cause. Fix: `AbsPathPrefix` newtype absolutizing at deserialization + at
  `required_capability_for` (`native.rs:88/128/169`, `main.rs:1345-1349`). → `cap.3`.
- **audit86-P1-9 (P1) [new] — Wrong-tier capability grants are no-ops (6 of 9 combos inert).** **[PARTIAL — no longer SILENT as of cap.1 (v0.93.0), re-checked 2026-07-28: the shared `tier_legality` resolver + `agentd check` now REPORT every inert grant (`Legality::Inert(why)` at `check.rs:108/176/224`, for `CapContext::Agent` and `CapContext::StdioMcp`), and cap.1 added a `CapabilitiesResolved` boot event + a fail-closed entrypoint gate. So an operator is told. What remains is the substance: the grants are still INERT at those tiers, and the open question is whether that is the intended design (declare-then-lint) or a gap to close by making them load-bearing. Needs a scope decision, not more code — raise it at the next planning pass rather than leaving it to read as an unaddressed P1.]**
  Agent-level `Credential` (`capability.rs:69`, "deferred") and `Net` (`:162`, returns true
  unconditionally) are decorative; HTTP MCP servers discard `capabilities`/`isolation`
  (`config.rs:604-628`, `main.rs:468-513`) and are *exempted* from `mcp_require_capabilities`
  (`main.rs:361`, `.filter(!is_http())`) with no warning — the operator's mandatory-sandbox
  switch does not cover remote tool servers. The Gmail-outage class. Fix: tier-legality
  validation in `Config::validate()` (hard error for HTTP servers carrying security fields —
  see open question D4; warn for inert agent-level grants) + a `CapabilitiesResolved` boot
  event logging each agent's and server's effective set. → `cap.1`.
- **cap.1b (P2) [new, cap.1 /autoplan split] — runtime capability-denial surfacing.**
  cap.1 shipped the config/boot-time declaration surface (`agentd check` + `CapabilitiesResolved`).
  The runtime half — a per-agent `capability_denied` counter on the snapshot + a Dashboard ATTN
  column — was split out (different surface: `surfaces`/`agentctl`; cap.2 does not depend on it).
  When built: the first denial of a **novel (agent, capability) pair surfaces (N=1)** — dedup only
  the repeats (the one-shot-give-up Gmail case) — and it rides the **shared attention channel**
  (audit §direction 4), not a distinct one. Prompt-path-literal ↔ FsWrite consistency was demoted
  to a `agentd check` warning (brittle free-text heuristic); revisit only if dogfood wants it.
- **cap.1-ar-01 (P2) [new, cap.1 /review] — credential wiring cross-check is config-global, not per-server.**
  `agentd check` errors only when NO stdio MCP server anywhere carries a referenced `Credential{provider}`,
  but the runtime broker token is built **per-server** (each stdio server's token = its OWN Credential caps,
  main.rs `credential_allowed_providers`). So a config with a stray server carrying `Credential{Google}`
  while the `google_oauth` server the agent actually calls carries none PASSES `check --strict` but fails
  silently at runtime — the exact Gmail-outage class the linter advertises. Does NOT fire on the shipped
  single-credential-server config. Precise detection needs a provider→server binding the config doesn't
  express today (the honesty-boundary line now states the limitation). Tighten in a follow-up (may fold into
  cap.2's per-child attenuation). (Both review voices.)
- **cap.1-ar-02 (P3) [new, cap.1 /review] — `agentd check` doesn't mirror the `mcp_require_capabilities` boot check.**
  When `mcp_require_capabilities = true`, the runtime fails boot if a stdio server's effective sandbox rules
  are empty (`caps_to_rules` empty, main.rs). `agentd check` only warns about the HTTP exemption; it doesn't
  reproduce the stdio empty-rules test, so `check` can pass a config that then fails boot. Loud-fail (clear
  boot error), not the silent class cap.1 targets, so lower priority. Fix = move the effective-rule predicate
  to lib and apply it in `check_config`. (Codex review.)
- **cap.1-ar-03 (P3) [new, cap.1 /review] — distro/QEMU init doesn't run `agentd check`.**
  Only the Docker `entrypoint.sh cos)` path is gated with `agentd check --strict`; the distro/QEMU `/init`
  boot path is not. The distro config is already clean, so informational — add the check to `/init` for parity.
- ~~**audit86-P1-10 (P1) [re-rate; = cos-dev-02, was P2] — Spawn inheritance is all-or-nothing; Curator inherits live Gmail.**~~ **[CLOSED in cap.2b (v0.95.0), struck 2026-07-28: the entry's "STILL OPEN for the injected-orchestrator threat → cap.2b" note predates cap.2b actually shipping. cap.2 (v0.94.0) shipped the attenuation FLOOR (`SpawnConfig.capabilities` + `capability_covered_by`, reject-not-clamp); cap.2b (v0.95.0) shipped the REAL closure — sealed `run_job` with config-owned caps/task + `AwaitingParent.deliver_content = false` so a trigger gets a completion signal and never the job output, plus a de-privileged cron trigger. CHANGELOG v0.95.0 states it directly: "Audit P1-10 CLOSED against the pinned claim", with a no-read proof and a negative-control-verified topology guard in /qa. Residual documented in THREAT_MODEL §9.5: a brief social-engineering vector, not capability inheritance.]**
  `SpawnConfig` (`config.rs:412-422`) has no capabilities field; `dispatch_spawn`
  (`scheduler.rs:1586`) does `parent_cap_set.clone()`. The Curator (processes
  attacker-influenceable email text daily) inherits `Credential{Google}`, so a prompt-injection
  payload surviving into curation can call live Gmail through the broker with every hardening
  layer cooperating. Post-cred.6 the broker made capability scoping load-bearing; this makes
  it decorative for children. **cap.2 (v0.94.0)** shipped the attenuation FLOOR:
  `SpawnConfig.capabilities` + a sound subset check (`capability_covered_by` — Net & Mcp given
  real containment, no-wildcard-arm drift guard) + `AgentSpawnDenied` (reject not clamp) + CoS
  prompt scoping the curator KB-only (no `Mcp{google_oauth}`). **This closes accidental
  over-grant only.** The CEO gate (both models) established that tool-input caps CANNOT close the
  injected-orchestrator threat this item names: the orchestrator holds Gmail and chooses the
  child's caps while reading untrusted email, so an injected orchestrator grants the curator
  Gmail *from its own set* and the subset check passes. That gap is **encoded as a passing test**
  (`spawn_attenuation_documents_injection_bypass`) so it can't be mistaken for closed. → `cap.2b`.
- ~~**cap.2b (P1) [= the real P1-10 closure] — De-privilege the CoS orchestrator.**~~ **[DONE — sealed `run_job` + `[[jobs]]` + de-privileged cron trigger; `deliver_content=false` completion-only delivery; brief authoring moved to the KB-only curator job; both cos configs migrated + `agentd check` clean; P1-10 CLOSED against the pinned claim (no live Gmail via injection), brief social-engineering residual documented in THREAT_MODEL §9.5; north-star data-taint recorded. Shipped in the cap.2b increment (see docs/plans/cap.2b-orchestrator-deprivilege.md).]**
  The subset check (cap.2) only becomes load-bearing once the GRANTING node is itself attenuated.
  cap.2b moved cap + task authority off the injectable trigger onto config-owned sealed jobs, so an
  injected trigger can neither mint caps nor redirect work nor read job output. North star (still
  open): data-taint — caps unavailable to any agent downstream of an untrusted-data read (generalizes
  to Telegram/webhook ingest). The former `spawn_attenuation_documents_injection_bypass` test was
  renamed `spawn_agent_floor_is_not_injection_defense` (the spawn_agent floor is unchanged; the
  sealed run_job path is what closes injection).
- ~~**semantic-kb-colon (P1) [found in cap.2b live test] — L2 semantic KB never persisted colon segments (Qdrant 422).**~~ **[FIXED — `docker/semantic_kb_mcp.py` `_collection_name` now sanitizes `[^A-Za-z0-9_]`→`_` before the `kb_` prefix.]**
  `_collection_name(segment)` returned `f"kb_{segment}"`, so `ops:entities`/`ops:briefs`/`mail:raw`
  mapped to Qdrant collections `kb_ops:entities` etc. — and **Qdrant rejects `:` in collection
  names** (`HTTP 422: collection name cannot contain ':' char`). So EVERY `kb_put` to a colon
  segment through the semantic-kb sidecar (tool_override) has silently failed since memory-routing
  (v0.81) — the L2 "semantic email dedup / brief persistence" never actually wrote to Qdrant for
  these segments. Latent because the sidecar's own `_validate_segment` allows colons and the unit
  tests use MOCK_EMBED (never hit real Qdrant collection creation). cap.2b surfaced it: the
  inbox→curator handoff MUST go through the KB, so the curator found `ops:entities` empty. Fix
  sanitizes the collection name (colons → `_`), applied in the ONE mapping used by
  create/put/get/search so reads/writes align. No data migration needed (all prior writes 422'd).
  Regression fence added (`test_t20` — direct assertion on the mapping, since the MOCK_EMBED
  suite uses colon-free segments and would pass even if the sanitizer regressed). Follow-up:
  consider a non-mock Qdrant smoke in CI.
- **semantic-kb-injective (P3) [/review, cross-model Claude+Codex] — `_collection_name` is non-injective and `_point_id` is segment-blind.**
  Any char outside `[A-Za-z0-9_]` maps to `_`, so segments differing only by a sanitized char
  (`ops:entities` vs `ops.entities`/`ops-entities`/`ops entities`) collapse to one Qdrant
  collection; combined with `_point_id(key)` (no segment component), a collision would let
  same-key writes across the two logical segments silently overwrite each other and reads cross
  the boundary. NOT triggerable with the current 5 segments (all map distinct — `test_t20`
  asserts this), so P3. Real fix when a colliding segment is ever needed: fold the segment into
  `_point_id`, or hash the raw segment into the collection name.
- **cos-kbwrite-override-cap (P3) [/review, Claude adversarial] — verify agentd enforces the KbWrite segment cap on the `tool_override` path.**
  `cos-inbox` routes `kb_put` to the semantic-kb sidecar via `tool_override=true`. The sidecar's
  `_validate_segment` accepts any colon segment; segment-scoping is agentd's KbWrite cap, not the
  sidecar's. Pre-existing since h8.1, but the colon fix above WIDENS the blast radius (colon
  segments now actually persist to Qdrant instead of 422-ing), so a cap-bypass write would now
  land real data. Confirm agentd re-checks the KbWrite segment on overridden MCP tool calls;
  if it doesn't, an injected inbox agent could write to the curator-owned `ops:briefs`.
- ~~**cap.2b-ar-02 (P2) [found in cap.2b live test] — cos-inbox job balloons context/spend via `format=full`.**~~ **[FIXED — inbox now fetches `format=metadata` (From/Subject/Date + Gmail `snippet`, no base64 body) in both cos configs; triage is snippet-based, context stays ~50×≤1 KB, budget reverted 5M→1.5M. Also added `metadataHeaders` to the broker `passthrough_query_params` allowlist in both configs — /review (Codex) caught that the broker was silently stripping the header filter, so Gmail returned ALL headers (still no base64 body, which is why the live test stayed bounded, but larger than intended). Tradeoff: brief detail is snippet-level, not full-body. Deeper option (bound ALL oauth_call_api output in the oauth_mcp sidecar) left as a future hardening.]**
  The inbox job's task fetches each message with `oauth_call_api(...messages/{id}?format=full)`, which
  returns the entire raw payload incl. base64 body parts. Those huge tool results accumulate in the
  agent's context, and re-sending the growing context every turn balloons cumulative spend — observed
  **~1.8M tokens by turn 6**, tripping the (old 1.5M) budget → `deferred`, so the pipeline stalled and
  no brief landed. Temporary unblock: bumped `cos-inbox` `token_budget` to 5M (both configs). Real fix:
  lighten what enters context — fetch `format=metadata` for triage and/or extract only the plaintext
  body part with a hard per-message cap BEFORE it becomes a tool result (the current "truncate to 8 KB"
  is applied by the model to `kb_put` AFTER it has already seen the full response, so it doesn't bound
  context). Watch for a context-WINDOW overflow too (budget can't fix that). Pre-existing (the pre-cap.2b
  spawned inbox did the same fetch at the same 1.5M budget); cap.2b surfaced it in a clean run.
- **cap.2b-ar-01 (P3) [new; found in cap.2b /review] — sealed-job pipeline date can skew across UTC midnight.**
  `dispatch_run_job` server-stamps `{date}` = `Utc::now()` independently for each `run_job` call
  (`scheduler.rs`). The CoS pipeline is two calls (cos-inbox then cos-curator); if they straddle
  UTC midnight (a cron near 00:00 UTC, or a first-run OAuth delay), inbox writes
  `ops:entities/inbox-<old-date>` but the curator renders its task to read `inbox-<new-date>` and
  finds nothing → the brief fails (cleanly — curator's "missing → error, stop"). NOT a security
  issue and the default `TRIGGER_CRON="0 8 * * *"` never hits it; fails SAFE (no stale/wrong data).
  Fix: carry ONE pipeline date across both jobs — e.g. a strictly-validated (`^\d{4}-\d{2}-\d{2}$`,
  no injection surface) optional `date` param on `run_job` that the trigger sets once from the cron
  `fired_utc`, or a scheduler-held per-cron pipeline date. Chose docs-over-fix at /review: a fixed
  handoff key would remove the skew but introduce a worse SILENT stale-brief failure mode.
- **cap.2-ar-01 (P3) [new] — spawn `max_turns` passthrough (split out of cap.2).**
  `SpawnConfig` has no `max_turns`; children get `default_max_turns()` (`scheduler.rs` step 4).
  `cos-polish-adv-F5` (curator hits `MaxTurnsReached` at the default) wanted this merged into
  cap.2, but the CEO gate cut it: `max_turns` is a resource limit, not a capability, and bundling
  it dilutes a security review (one-increment-per-branch). Add `max_turns: Option<u32>` to
  `SpawnConfig` + tool schema; `dispatch_spawn` uses it. Small cos-polish-class follow-up.

### P2

- **ci-cache-01 (P3) [new; found post-cap.2 /land] — Docker build cache (`type=gha`) is dead → cold 80-min release builds.**
  GitHub retired the legacy Actions Cache service (2025); buildx `cache-to/cache-from: type=gha`
  now returns `not_found` on export. The `hotfix-publish-docker-cache` branch added
  `ignore-error=true` so the RED release is fixed (image push already succeeded; only the
  cache export was fatal — broke v0.93.0 + v0.94.0 CI-on-tag). But caching is still effectively
  OFF: `cache-from` misses too, so multi-arch release builds run cold (~1h23m observed on
  v0.94.0). Restore build-cache speed: bump `docker/setup-buildx-action` to a version bundling
  BuildKit ≥0.16 that speaks the gha cache v2 API, OR switch to `type=registry` cache
  (`cache-to: type=registry,ref=ghcr.io/0x89karan/runtime1:buildcache,mode=max`). Perf only —
  releases publish fine after the hotfix.
- **audit118-R1 (P2, was P0) [AUDIT-v0.118 — DEMOTED + BLOCKED at attn.3's /autoplan gate] —
  context paging divides retained tokens by `token_budget`, a SPEND ceiling.** Real bug:
  `context.rs:43` / `agent/mod.rs:813`; there is no `context_window` concept in the codebase.
  **But the audit's severity and its causal claim were both wrong, disproven by measurement:**
  (a) at the observed failure retained context was **11,569 tokens** against a would-be corrected
  trigger of 172,627 — **15× away**, so re-keying would have changed nothing; (b) with the measured
  slope of ~159 tokens/poll-pair the 10M global window dies at turn ~355 while paging would first
  fire at turn ~1,086, so on the trigger **budget exhaustion beats paging by ~3×, always** — the fix
  is arithmetically inert on the very agent the audit headlined. Its "independent, **sufficient**
  explanation for three briefs in fifteen days" is **withdrawn**.
  **BLOCKED, not merely deferred: fixing this alone is an active REGRESSION.** Paging is lossy and
  unrecoverable — `cap_short_term` (`agent/mod.rs:522-528`) does `drain(0..overflow)` silently,
  `memory.distill_on_complete` defaults `false` (`config.rs:196`) and is unset in `cos.agents.toml`,
  nothing recalls `short_term` in-run, and `page_count` sheds ~a quarter of the transcript
  oldest-first. On `cos-inbox` that discards the emails it read first: "no brief" becomes "quietly
  incomplete brief", with `memory_paged` in the log looking like the fix working.
  **Preconditions before touching it:** (1) paging must be non-lossy (distillation on plus an in-run
  recall path); (2) the limit is a **per-agent** property — a single top-level `[model]` table
  (`config.rs:34`) cannot say "trigger 2.5k, jobs 190k"; (3) evaluate **prompt caching** first —
  `cache_control`/`prompt_caching` have **zero** hits in `agentd/src/`, it attacks the same dominant
  cost term (an identical prefix resent every turn) and is non-lossy where paging is not;
  (4) `estimate_context_tokens` does not count the tool schemas `agent/mod.rs:898` actually sends,
  so the numerator is a documented undercount; (5) the paging FLOOR is reachable — `page_count`
  returns 0 for any transcript of ≤4 messages, and `cos-inbox` pulls ~20 emails into ONE
  `tool_result`. `attn.3` shipped `paging_limit` + `paging_limit_source` on `inference_request` so
  the wrongness is now legible in the log.
- **audit118-R2 — CLOSED. Its premise was wrong; the real defect is closed by attn.2 + attn.3.**
  The audit said "the live 400 loop was in-memory (no `checkpoint.json` in the volume)" and
  prescribed a pairing check before every `InferenceRequest`. **The volume disproves it:** 65
  `agent_checkpointed` + 2 `agent_restored` events, and the 400 lands **one second after each
  restore**, twice (18:13:57→:58, 18:17:32→:33). It is the RESTORE path, already fixed by attn.2
  (`db9cccb3`). The **producer** is SIGTERM landing mid-tool-call — dispatched 18:13:31, SIGTERM
  18:13:47, 16 s into a 20 s `wait_for_trigger`; 63 `tool_call` vs 62 `tool_result` — and `attn.3`
  closes that at `build_scheduler_checkpoint` so a malformed transcript is never persisted.
  **The in-loop repair was built and then WITHDRAWN**: no producer could be demonstrated (the
  candidate mechanism was refuted — the id *does* appear in a `tool_call` and every `stop_reason`
  is `tool_use`, never `end_turn`; `live_call_ids` is `∅` at all 16 `step()` sites), and it would
  have **removed the circuit breaker** — the 400 currently kills the trigger at ~29 min, whereas a
  repaired trigger polls on until the 10M window dies at ~2.6 h. Do not re-add it without a
  demonstrated producer.
- **attn.3-qa-01 (P2) [attn.3 /qa, proven live] — `repair_and_record` emits a SECOND
  `agent_restored`, so anything counting restores double-counts exactly when a repair fires.**
  `main.rs:1258` already records one `AgentRestored` per restored agent; `scheduler.rs:114` records
  another when a restore repair happens. Observed live in two /qa arms — `agent_restored` appears
  twice for the same agent. **This is the identical defect class /review fixed one function over**
  in `build_scheduler_checkpoint` (72dad0e4), and the same reasoning applies. Pre-existing from
  attn.2, not introduced by attn.3, but its sibling was fixed in this increment and this one was not.
  Fix: collect the repaired ids into `SchedulerRestored` and fold them into main.rs's single
  `AgentRestored`, mirroring how `checkpoint_all` now folds the checkpoint-side repair. Deliberately
  NOT bundled into attn.3 — it needs plumbing through the restore return type, which is more surgery
  than a log-hygiene fix warrants at the end of an increment. The field-name half **is** fixed
  (`repaired_ids` + `repaired_count`, matching the checkpoint side).
- **attn.2-R5 — CLOSED (2026-08-07), superseded by a redesign, not the original spec.** The
  original design (`docs/plans/attn.2-workable-brief.md`, "R5 REDESIGNED") built the route on
  `dispatch_run_job` + a synthetic parent id, and four findings were verified against it (below,
  kept for the record). The shipped design instead reuses `dispatch_scheduled_job` — the SAME
  primitive `tick_native_jobs` (attn.4) already calls, with `parent_id: None` and no caller to
  reject through — which sidesteps findings (1) and (2) **by construction**, not by fixing them:
  there is no synthetic parent id, so `reject()`'s panic-on-missing-key path and the
  nonexistent-parent `child_model_cfg` fallback are never reached at all. (3) is closed directly
  (`/api/v1/jobs/*/run` is in `is_mutating_route`, pinned by test). (4) is closed by threading a
  distinct `start_reason = "manual_fire"` through `spawn_job_child`/`dispatch_scheduled_job`
  (native-tick fires keep `"run_job"`) — **deliberately NOT** also excluding `manual_fire` from
  `runs/store.rs`'s stats arms the way `config_seed` is excluded from `still_running`: a manual
  fire is real work, not a restore non-event, and hiding it from `agentctl runs` would make
  debugging "what happened when I clicked fire now" harder, not easier. Tagging, not hiding, is
  the fix. Auto-chaining `cos-inbox`→`cos-curator` was in scope for the original spec; the
  shipped design fires one job per call, unchanged from that finding's conclusion (chaining
  doesn't survive a restart with no checkpoint hook).
  - **Concurrent-dedup guard CLOSED (2026-08-07, retroactive /autoplan review, cross-model-
    confirmed CRITICAL).** `reserve_job_child_id`'s guard is a pure string collision check on
    the derived `child_id` — but the native tick (`{job_id}-{ts}`), a manual fire
    (`{job_id}-manual-{nanos}`), and an agent's own `run_job` call (`{job_id}-{date}`) each
    construct a deliberately disjoint id shape, so it could never detect "this job already has
    a live run" across paths. Fixed by `reject_if_job_already_running` (a `job_id → live
    child_ids` lease in `SchedulerState`), checked before every dispatch path and cleared on
    termination — see THREAT_MODEL.md §9.5's update.
  - **New residual, not fixed:** still no rate limit — the guard above blocks OVERLAPPING
    fires of the same job, not repeated SEQUENTIAL ones. Fire, wait for it to terminate, fire
    again: nothing throttles that pattern. Manual-trigger analog of `attn.4-ratelimit-01`.
  - **New residual, not fixed:** `cos-inbox`'s `ops:entities` key is `inbox-{date}` with no
    `{ts}` (unlike the curator's R4-fixed `ops:briefs` key) — a manual fire on a day cos-inbox
    already ran overwrites that day's real inbox data with the manual run's, last-writer-wins.
    Pre-existing (any same-day re-fire already had this risk — attn.2-ts-01's same class), not
    introduced here, but manual fire makes it much easier to trigger by hand. The TUI's confirm
    overlay must warn about this explicitly.
  - **New residual, not fixed:** FUSE-source parity. The TUI's Jobs view and fire-now verb only
    work over the HTTP source (`agentctl watch --url`); `DataSource::run_job`'s FUSE
    implementation is not built (the FUSE producer side, `surfaces/src/agents_fs.rs`, has no
    `system/jobs` file yet either — same gap as `attn.4-watch-01`, now shared by two features).
    Matches how the operator actually runs `agentctl watch` today (HTTP, against the Dockerized
    `cos`), but is a real gap for the Linux/QEMU FUSE path.
  - Original findings, verified against the SUPERSEDED design, kept for the record: (1)
    `dispatch_run_job`'s `reject()` indexed `state.agents[&parent_id]` — panics on a missing
    key — on every rejection path; a synthetic never-live parent id would have crashed `agentd`
    on the single most likely real trigger (firing twice, hitting the collision guard). (2)
    `child_model_cfg` fell back to `ModelConfig::default()` (4096 max_tokens) for a nonexistent
    parent, silently halving the configured 8192 budget. (3) `is_mutating_route` is a
    hand-enumerated allowlist; a new route omitted from it ships unauthenticated even with
    `AGENTOS_APPROVAL_SECRET` configured — the exact hole cap.4/AUDIT-v0.97 P2-3 fixed once
    already. (4) `start_reason != "config_seed"` was proposed but only implemented in
    `still_running`, never `terminal_in_window` — the branch a completed manual fire actually
    matches.
- **attn.4 (P1) [attn.3 /autoplan, operator-chosen] — the CoS burns ~3,456 inference calls a day
  to watch a clock. Scheduler-native cron.** MEASURED: 63 of 63 tool calls in a 29-min window were
  `wait_for_trigger(timeout_s=20)`, each answering "next fire 14 h from now"; 414,016 input tokens
  **to wait**, growing ~159 tokens/poll-pair because the whole transcript is resent. That is the
  dominant cost driver and the reason the 10M/24h window empties in ~2.6 h — **not** paging (R1).
  `cos.agents.toml:271` documents the furnace as a `max_turns` sizing problem and raises the limits
  to accommodate it. `MAX_TIMEOUT_S = 25` (`cron_mcp.py:31`) is a consequence of the global
  `MCP_TIMEOUT = 30s` (`mcp.rs:23`), so raising the timeout globally would let any hung sidecar pin
  an agent for minutes on a PID-1 process — rejected at the gate. Chosen fix: give `[[jobs]]` a
  schedule and fire them from the scheduler, removing the LLM from the schedule boundary entirely
  (which *strengthens* cap.2b). **Verified NOT a config flip:** `Job` has no schedule field and
  `scheduler.rs` has no native job cron. Open design questions in
  `docs/plans/attn.3-real-context-window.md` §4, incl. the **timezone gap** (no `chrono::Local`, no
  `tzdata`, no `TZ` — "08:00" silently means UTC) and the fate of the other two h7.3 trigger servers.
  **The brief is not expected to return until this ships.**
- **audit118-R3 (P1) [AUDIT-v0.118] — three PRODUCT-THESIS security claims the code does not
  implement.** `:63` network-namespace egress confinement (`universal.rs:68` passes
  `--network=host`); `:65` eBPF syscall auditing (no eBPF exists); `:69`/`:108` per-tool approval
  gates (`config.rs:495` — the action is passed BY the agent to an ordinary tool). Also
  `THREAT_MODEL.md:258` lists `IsolateMount` under "what the sandbox enforces" with zero production
  callers, and `:139` bases the checkpoint risk-window on `CheckpointStore::remove()`, which does not
  exist. Outward-facing doc; fix the claims or build the controls.
- **audit118-R4 (P1) [AUDIT-v0.118, mutation-proven] — vacuous self-matching test guards, 5th
  instance.** `credential/mod.rs` does `include_str!("mod.rs")` and asserts on bare literals that its
  own assertion text satisfies. Proven: deleting the 502 Err arm and removing the OAuth refresh
  timeout both left the suite green. Worst: `cos_spawn_caps_subset.rs:49-59` matches
  `Capability::Mcp { .. }` — a wildcard — while guarding the cap.2b P1-10 criterion, so a
  `shell_exec` grant to the schedule-exposed trigger passes. Fix: scan only the production region
  (`.split("#[cfg(test)]").next()`), and pin the server name in the capability arm.
- **audit118-R5 (P1) [AUDIT-v0.118] — `mcp.rs:382` byte-slices a `&str`.**
  `&raw.trim()[..raw.trim().len().min(256)]` panics on a multi-byte boundary; `panic = "abort"` + PID
  1 = full-system failure. The correct version is at `mcp.rs:821` **in the same file**. One line.
  Same class as the ux.6a `hex_decode` boot panic.
- **audit118-R6 (P1) [AUDIT-v0.118] — `CompletedTruncated` makes the durable record contradict the
  outcome.** `agent/mod.rs:976` emits `AgentCompleted` on both branches; `scheduler.rs:2170` converts
  the effect to a failure and emits nothing (`AgentFailed` appears only at `:741`/`:753`/`:962`). So
  `flight.jsonl` says completed while `runs.redb` says failed. Fix: give it a kind, or emit
  `AgentFailed`.
- **audit118-R7 (P2) [AUDIT-v0.118, mutation-proven] — `EventKind::ALL` is not compile-time
  exhaustive.** Adding a variant to the enum + `as_str()` + `touch()` but not `ALL` compiles and
  leaves every guard green, because `all_is_exhaustive` iterates `ALL` to check `ALL`. All three
  consumers are downstream of the same unenforced array. Fix: generate `ALL` from the `as_str()`
  match. Repairs four guards at once.
- **audit118-R8 (P2) [AUDIT-v0.118] — docs that fail if followed literally.** The
  `MCP_SERVERS.md:173` `[capabilities]` snippet is a top-level table serde silently drops;
  `passenv = ["BRAVE_SEARCH_API_KEY"]` is blocklisted at `mcp.rs:46` and ships in two templates;
  `RUNBOOK.md:916` tells the operator to `rm checkpoint.json` to clear spend (the window self-resets
  and the file holds live run state); `cos.agents.toml:52` greps for `egress_rejected` when the kind
  is `egress_denied`; `README.md:17` Status is 52 releases stale; `AGENTD_MANAGEMENT_ENABLED` and
  `SIDECAR_SECRET` are undocumented. Full list in `docs/AUDIT-v0.118.md` R6.

- **attn.2 (P1) — R1+R2 SHIPPED (v0.119.0); R3/R4/R5 still open. Original ask: there is no way
  to fire the CoS on demand.**
  **Shipped in v0.119.0:** R1 (a missing `OPENAI_API_KEY` no longer fails the boot; degrades
  honestly with `kb_search` returning an explicit empty and writes namespaced to
  `kbdegraded_*`) and R2 (`attn.1a-05` closed — awaiting-parent seed skip + state-aware
  dangling-`tool_use` repair at the `from_checkpoint` call site).
  **Still open:** R3 (inbox-scoped Gmail query so archive means handled, dedup Important vs
  Response Needed, narrowed escaping, `suppressed_count`), R4 (non-destructive brief writes —
  needs a `{ts}` token in `Job::render()`, which only substitutes `{date}`), R5 (the manual
  fire itself — and its sentinel must NOT live in curator-writable `./output`).
  **Blocked behind `AUDIT-v0.118.md` R1**: none of R3–R5 can be measured while paging is keyed
  to the spend budget and the pipeline burns its window without emitting a brief.
  The operator asked for this at the start of the 2026-08-01 session ("a way to see active and open
  emails via a manual sync more than once a day") and again after the first daily cron proved
  unusable. Today the only fire path is the cron, so testing a change means either waiting for
  08:00 UTC or restarting — **and restarting is what triggers `attn.1a-05`**, so the obvious
  workaround is the one thing that must not be used.
  **Design (verified against the code, no new capability or API route needed):** `wait_for_trigger`
  already wakes every `timeout_s` to poll (`docker/cron_mcp.py:350` — it sleeps to
  `min(next_fire, now+timeout_s)`, then fires only if `now >= _NEXT_FIRE_TS`). Have it also check a
  sentinel file on each wake; if present, unlink it and fire. Put the sentinel in the directory that
  is **already bind-mounted to the host** (`~/.agentos-output` → `/data/output`, `docker-compose.yml`),
  so the operator runs `touch ~/.agentos-output/.fire-now` with no new route, no new capability, and
  **no restart**. Unlink-before-fire makes it single-shot and idempotent under a double touch.
  Must not disturb the scheduled fire: a manual fire should NOT call `_advance_next_fire()`, or a
  manual run at 07:00 would silently cancel the 08:00 scheduled one.
  Also needs: a flight event so a manual fire is distinguishable from a scheduled one in the audit
  trail, and a note in RUNBOOK §11.12. An `agentctl` wrapper is optional sugar — the `touch` is the
  contract.
  **SPECCED 2026-08-02 → issue #164, and it grew.** Once the operator read a brief for the first
  time they added three more requirements — a re-run must "update the brief and bring it up to date.
  not be destructive", and handled items must stop being listed, with a **manual override** for the
  ones they dealt with off-thread. That makes this a worklist change, not a trigger: the full spec is
  `docs/plans/attn.2-workable-brief.md`, staged 1–4 with the manual fire as Stage 2.
  **Stage 1 is a correctness fix that stands alone and should ship first:** `write_file` truncates
  (`tools/native.rs:157`), so a same-day re-run **destroys** the earlier brief — and the comment that
  permits same-day re-runs (`scheduler.rs:2478`, "harmless — brief is log-append/LWW") is wrong about
  the file. Needs `/autoplan` before any build; the capability question (curator has `FsWrite` on
  `./output` but no `FsRead`) is deliberately left open for its security pass.

- ~~**attn.1a-05 (P1)**~~ **[FIXED — attn.2 R2, see below] — restarting an agent that is
  parked mid-tool-call BRICKS it permanently, and `restart: unless-stopped` now walks into this by
  itself.** Observed on the real CoS, not inferred. Full mechanism:
  1. `wait_for_trigger` (`docker/cron_mcp.py:350`) sleeps until `min(next_fire, now+timeout_s)` and
     returns `waiting` when it is not yet time, so the orchestrator loops on it. It is parked
     **mid-tool-call ~99% of its life**.
  2. A restart in that window checkpoints/restores a conversation whose last message is an assistant
     turn ending in a `tool_use` block with **no following `tool_result`**.
  3. The Anthropic Messages API rejects that shape outright:
     `400 messages.125: tool_use ids were found without tool_result blocks immediately after:
     toolu_01FonBTdyrQmHudK1ChQAHcZ`. The agent dies on its **first** inference after restore
     (`agent_failed reason=inference_error`).
  4. **It is self-perpetuating.** On shutdown it checkpoints the same poisoned history, so every
     subsequent boot fails identically. Recovery required deleting BOTH `/data/checkpoint.json` and
     `/data/checkpoint.json.restored` — a normal restart cannot escape it.
  **NOT CoS-specific.** `checkpoint.rs` contains **zero** `tool_use`/`tool_result` handling (grep
  verified), and `from_checkpoint` (`agent/mod.rs:300`) restores `messages: cp.messages` verbatim with
  no validation. Any long park is exposed.
  **⚠ ONE CLAIM IN THIS ENTRY WAS WRONG** (found at /autoplan, 2026-08-02): "an agent parked awaiting
  an operator approval across a restart loses the approval AND dies" is FALSE. `ParkedApproval` is
  checkpointed (`checkpoint.rs:127-129`) and the seed loop already skips those agents
  (`scheduler.rs:686-688`). The real gap was narrower and different: the seed loop skips
  approval-parked agents and `state.waiting`, but had **no arm for a parent awaiting a child**, even
  though `state.awaiting` IS restored. The CoS trigger parks exactly that way when it calls
  `run_job`, so on restore it was re-stepped and shipped its dangling `run_job` tool_use.
  **FIXED in attn.2 R2, both halves — either alone leaves a live bug:**
  - **R2.1** an awaiting-parent skip arm in the seed loop. Without it, repairing messages alone lets
    the trigger infer cleanly, call `run_job` a second time (a duplicate, fully-paid cycle), and then
    400 anyway when the original child's result arrives for a call_id no longer present.
  - **R2.2** `AgentTask::repair_dangling_tool_uses`, called at the `from_checkpoint` CALL SITE in
    `scheduler.rs` — not inside `from_checkpoint`, which sees one agent and cannot know which
    dangling ids the scheduler has promised to answer. Ids in `awaiting`/`pending_approvals` are left
    strictly alone (answering them would make the real result land as a `tool_result` with no
    matching `tool_use` — the same 400 in reverse); only orphaned ids get a synthetic error result.
  Also worth recording: this was never a rare shutdown race. `default_checkpoint_interval_turns()`
  is **1** (`config.rs:449-451`) and the periodic write snapshots every agent, so while a child burns
  its turns, **every** checkpoint captures the parent mid-call.
  **Severity is raised by attn.1a-core itself:** `restart: unless-stopped` (v0.118.0) makes restarts
  automatic, so a crash while parked now self-inflicts this without anyone touching the container.
  Before that change the trap needed a human to run a restart.
  **Fix at `agent/mod.rs:300`:** on restore, if the trailing message is an assistant turn ending in a
  `tool_use` with no matching `tool_result`, either synthesise a `tool_result` carrying an
  "interrupted by restart" error (preferred — preserves turn structure and tells the model what
  happened, so it can retry the call) or drop the trailing assistant message. **Test:** checkpoint a
  task mid-tool-call, restore, and assert the first outbound inference request has no dangling
  `tool_use` — mutation-verified by removing the guard.

- **attn.1a-01 (P1) [new, found by /review security specialist 2026-08-01] — `max_requests_per_agent` is a
  process-lifetime counter, not a rate limit. Setting it on the CoS google provider SILENTLY KILLS Gmail
  after ~1-2 weeks.** attn.1a-core shipped this cap at 400 and it was **removed at /review** before landing.
  Verified mechanism, do not re-derive:
  - The broker token for `google_oauth` is minted **once at boot** and attributed to a **static principal** —
    `owning_agent_id()` (`main.rs:1667`) returns the single `[[agents]]` id (`cos-orchestrator`), **not** the
    date-keyed `cos-inbox-YYYY-MM-DD` job child.
  - The counter is an increment-only `AtomicU64` keyed `(agent_id, provider)` in `state.counters`
    (`credential/mod.rs:1081-1095`). Its **only** clearing site is `deregister_token`
    (`credential/mod.rs:1674`), whose **only** production caller is agentd **shutdown** (`main.rs:1374`).
    No per-agent-terminal reset, no window, no management route (`reset-attention` clears health/token
    cache only), never persisted to `caps.redb`.
  - One inbox cycle costs ~1 `messages.list` + up to 50 metadata GETs ≈ **30-55 broker requests**. At 400 the
    ceiling lands in **~7-13 days** of normal daily briefs, after which **every** Gmail call returns
    `429 credential_cap_exceeded` until the container restarts. That is precisely the silent-stoppage failure
    attn.1a exists to fix, caused by attn.1a — and `restart: unless-stopped` from the same commit is what
    keeps the process alive long enough to hit the wall.
  - **The security value was also overstated:** because the counter dies with the process and restart is now
    automatic, a runaway or injected agent **resets its own ceiling by crashing**. The cap bounds one process
    lifetime, not a day.
  **Before setting this on any long-lived provider**, do one of: (a) reset per job run (call
  `deregister_token`, or key the token per `run_job` child so each `cos-inbox-<date>` gets its own counter);
  (b) make it windowed on the same 86400 s rollover as the budget window; or (c) size it explicitly as a
  process-lifetime ceiling (~20 000 ≈ a year of daily cycles) and say so in the config comments.
  **Whichever path, the test must drive >cap requests through the real gateway with one boot-minted token and
  assert the reset happens** — a config-parity test that only checks the number is present and equal stays
  green for all three shapes. (A parity test of exactly that inadequate shape was written and then removed
  here; its design is worth reusing only as a *companion* to the behavioural test.)
  Also note for the config comments when it lands: an explicit `0` means **deny all requests**, not
  "unlimited" — absent/`None` is unlimited. `0` is the value an operator would most plausibly write meaning
  "no limit", and it would fail the pipeline closed.

- **attn.1a-04 (P2) [new, found by /ship plan audit 2026-08-01] — infra/triage runs still count into
  the brief's own stats, so the liveness signal sits on a surface a sub-daily loop will corrupt.**
  attn.1a's plan assigned this to attn.1a-core (as "M6") and it was dropped silently — caught only
  because /ship's plan-completion audit compared the build record against the diff.
  `publish_brief` counts every run terminal in window and sums `spend`
  (`agentd/src/runs/store.rs:489-504`), including `still_running` unconditionally (`:492-494`); the
  `start_reason != "config_seed"` check at `:494` is pre-existing and only guards still-running
  config-seed rows. So once `attn.1b`'s 30-minute triage loop exists, ~48 extra children/day will
  swamp `run_count`, `failed_count`, `spend_total` and the attention list — **on the very surface
  attn.1a-core added `last successful cycle` reporting to.** Fix: exclude infra/triage children by
  identity using the `config_seed` escape hatch already present at `:494`. **Must land WITH the loop
  that creates the children, not before** — shipping it now would be dead code with no consumer,
  which is the `ux.6a record_denied` pattern (zero production callers for a whole release). Test
  T-M6 rides with it: assert 48 infra runs in the window do not displace brief attention items or
  corrupt `spend_total`.

- **attn.1a-02 (P2) [new, /review 2026-08-01] — nothing validates `docker-compose.yml` semantics in CI.**
  `agentd/tests/compose_policy.rs` justifies its hand-rolled string matching by claiming "`docker compose
  config` in CI is the real semantic check". **No workflow runs `docker compose` at all** — verified by grep
  across `.github/workflows/`; `docker-smoke` only does `docker build` + `docker run`. So a structurally
  invalid compose file (bad indentation, duplicate key) ships unvalidated. Fix: add a `docker compose config
  -q` step to the `docker-smoke` job, which already has Docker available. The false claim was corrected in
  the test header at /review; the gap it named is still open.

- **attn.1a-03 (P2) [new, /review 2026-08-01; scope reduced 2026-08-01 at /document-release] — two
  compose-block extractors, one shared helper missing.** The correctness half is **FIXED in the same
  branch**: `service_block()` in `agentd/tests/compose_policy.rs` is now anchored to the `services:`
  mapping and terminates on the first non-blank line indented under 4 spaces, with negative controls
  pinning both original bugs (`service_block("cos-data")` must resolve nothing because it is a top-level
  *volume*, and `semantic-kb-mcp`'s block must not absorb the trailing top-level `volumes:` key). Do not
  re-derive the original description — it is stale. **What remains:** `agentd/src/main.rs:2408`
  (`compose_management_port_is_loopback_pinned`) has a second line-based extractor doing the same job, and
  the two still disagree about whether the header line is included. Fix: extract one shared helper.

- **test-flake-01 (P2) [new; found in cap.2 /ship] — Full-suite `cargo test --workspace` is flaky under high parallelism.**
  **attn.1a evidence (2026-08-01) — the original "flaky under high parallelism" framing is CONFIRMED,
  and the class is wider than recorded: THREE distinct clusters fail non-deterministically.** Four
  full/partial runs of the same tree in one session, machine saturated throughout (concurrent
  `cargo clippy --workspace`, a second `cargo test --workspace`, Docker Desktop, and subagents):

  | Run | Failed | `agentctl docker::tests` | `agentd scheduler::streaming` | `agentd/tests/mcp.rs` |
  |---|---|---|---|---|
  | A | **7× `mcp.rs`** | ok (690/690) | ok (911/911) | ✗ |
  | B | 2× `scheduler::streaming` | ok | ✗ | ok |
  | C | 2× `docker::tests` | ✗ | ok | ok |
  | isolated | `docker::tests` ×2 | ✗ | ok (4/4) | — |
  | E (/ship 2026-08-01) | 1× `scheduler::streaming` | ok | ✗ (1 of the 2) | ok |

  The failures **move between clusters run to run**, which is the signature of load sensitivity rather
  than a defect. All three clusters spawn subprocesses under bounded timeouts:
  `docker.rs:192 PROBE_TIMEOUT = 3s` (fake `docker` on `PATH`), the scheduler streaming tests index
  `state.agents`/`streamed_agents` by key and **panic rather than assert** when a spawn has not landed,
  and `agentd/tests/mcp.rs` spawns real Python MCP servers (failures at `:63 :123 :184 :237 :323 :519
  :630`).

  **⚠ A WRONG CORRECTION WAS COMMITTED HERE AND THEN REVERTED — read this before re-deriving it.**
  An earlier version of this entry claimed the `docker::tests` pair "fails reproducibly, is not a
  flake, and the label has been hiding a real failure," on the strength of two isolated runs plus a
  parent-commit run. That conclusion was **wrong**: run A shows the same pair passing 690/690 in the
  same tree, and the "isolated" runs were not isolated in the way that mattered — 52 s for 9 tests
  shows the machine was still saturated. Two red runs on a loaded machine are not evidence of a
  deterministic failure. **Confirm a cluster is green on an idle machine before declaring anything
  about it.**

  Still worth doing, on the original framing: these panics are `unwrap`-shaped index lookups and hard
  timeouts. Poll-with-deadline instead of indexing, and size the timeouts for a loaded CI box.

  **attn.1a update (2026-08-01): the `agentd` scheduler cluster —
  `scheduler::tests::streaming_dispatch_emits_flight_events_and_populates_streamed_agents`
  (`scheduler.rs:5767`, "agent not found") and
  `scheduler::tests::streaming_two_agents_populates_both_in_streamed_agents`
  (`scheduler.rs:6108`, "no entry found for key").** Both failed in a full-workspace run and both
  passed in the same tree when run isolated (`cargo test -p agentd --lib scheduler::tests::streaming`
  → 4/4 ok). **Attributable load cause:** the full run was executing concurrently with a background
  `cargo clippy --workspace --all-targets` and a second `cargo test --workspace` from the same
  session, so the machine was oversubscribed — i.e. this instance was self-inflicted, not evidence of
  a new defect. Recorded because it widens the known class beyond `agentctl docker::tests` into the
  `agentd` scheduler's streaming tests, which index `state.agents`/`streamed_agents` by key and panic
  rather than assert when a spawn has not landed yet. Both panics are `unwrap`-shaped index lookups;
  if this recurs unprovoked, the fix is to poll-with-deadline for the key instead of indexing.
  **brief.1 update (2026-07-29): a SECOND, mechanically-explained cluster in the same class —
  `agentctl` `docker::tests`.** `detect_docker_context_requires_exit_zero_and_at_least_one_container`,
  `kill_tail_kills_the_forked_grandchild_not_just_the_direct_child` and
  `a_probe_whose_grandchild_holds_stdout_still_returns_within_the_deadline` failed 2-of-3 and then
  3-of-9 on a loaded machine, and all three passed in a quieter run of the *same* tree. Mechanism is
  concrete, not mysterious: `PROBE_TIMEOUT = Duration::from_secs(3)` (`agentctl/src/docker.rs:192`)
  bounds a probe that spawns `/bin/sh` (a fake `docker` on `PATH`) and drains its pipe; under
  contention that exceeds 3 s, `compose()` returns empty stdout, `detect_docker_context()` returns
  `None`, and the `.expect("one container must be detected as a project")` panics.
  **Corrects a stale note in `docs/plans/brief.1-handoff.md`**, which attributed these to "need a
  live Docker daemon" — they use a fake `docker` on `PATH`, so the daemon is irrelevant (verified
  failing with Docker Desktop DOWN *and* passing with it DOWN). Fix for this cluster specifically:
  raise `PROBE_TIMEOUT` for tests or inject it, or put `docker::tests` behind `serial_test`.
  **ux.6a update (2026-07-29): independently reproduced AT THE MERGE BASE** (`92f208f7`, 2 of 8
  whole-suite runs), so it is confirmed pre-existing and must not be attributed to a diff that
  merely raises the hit rate. ux.6a adds ~30 fsync-heavy evidence tests to the same parallel
  pool, which does raise it. `--test-threads=1` remains 100% green.
  On a high-core machine (many `--test-threads`), `cargo test --workspace` intermittently fails a
  *varying* set of async scheduler/egress tests (`streaming_*`, `stream_delta_*`,
  `spawn_attenuation_documents_injection_bypass`, `egress::proxy_large_request_body_returns_413`) —
  the failure is a `sched.run().await` not completing → `outcomes["id"]` index panic, or a port/proxy
  race. All pass in isolation and `--test-threads=1` is 100% green (1535/0). Pre-existing (spans tests
  far older than cap.2); NOT a logic bug. Low CI risk today (GitHub runners are 2-4 core → little
  contention, hence green history), but latent. Fix: give the contended async tests their own
  resources (unique ports, per-test tokio runtime, or `serial_test` on the streaming/egress group);
  optionally pin CI to `--test-threads` or adopt `cargo-nextest` with retries. Not cap.2 scope.
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
- **ux.11c-ar-01 (P3) [new, ux.11c /review] — `GET /api/v1/brief` "N need approval" undercounts past 100.**
  The live overlay reports `snapshot.pending_actions.len()`, but the snapshot is built with `.take(100)`
  (scheduler.rs), so with 100+ simultaneously-pending approvals the brief header shows "100 need approval"
  when there are more (Codex + Claude review). Pathological for single-tenant (100+ parked approvals never
  happens in practice), and documented at the call site. Fix if it ever matters: add an uncapped
  `pending_actions_total` to `SchedulerSnapshot` and read that for the count only.
- **ux.11c-ar-02 (P3) [pre-existing, flagged ux.11c /review] — `runs_query` is not in `PROTECTED_TOOLS`.**
  ux.11c added `publish_brief` to `PROTECTED_TOOLS` (an MCP `tool_override` could otherwise shadow it and
  make the central hook emit `BriefWritten` without persisting a brief). The sibling `runs_query` (ux.11b)
  has the same shadowing exposure but lower impact (read-only; no trust event emitted), so it was left out
  of ux.11c scope. Consider adding `runs_query` to `PROTECTED_TOOLS` too for consistency. (Codex review.)
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
- ~~**audit86-P2-4 (P2) [new] — evidence.jsonl cannot rotate safely.**~~ **[FIXED in ux.6a.]**
  Hash chain (`evidence.rs:4-5,38`); `resume_chain` re-hashes the whole file at every boot
  (`:184-197`, O(file) forever); rename-rotate is silently ignored; archive-and-restart
  drops to GENESIS with no segment manifest. Was assigned to `run.1`, which shipped without it.
  **Fixed more cheaply than this entry proposed.** It called for "chain-aware segment rotation
  (new segment's first `chain_prev_hash` = prior segment's last hash); teach `agentctl verify`
  non-genesis starts" — none of which was necessary. Genesis anchoring only ever blocked
  *in-place truncation*, never rename: each renamed segment is a COMPLETE genesis-anchored
  chain, so the **shipped** verifier passes on every segment with no format change, no anchor
  records, and no `FORMAT_VERSION` bump (which is what keeps it clear of the audit86-P3-4
  rollback trap). Delivered: bounded 64 KiB tail resume, torn-tail repair, rename rotation to
  `.1`…`.3` (32 MiB each, oldest dropped with a log line, not silently). Residual, documented
  in `THREAT_MODEL.md` §8.7.1 rather than fixed: the **seam between segments is unprovable**.
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
- ~~**audit86-P2-12 (P2) [new] — Shell env-sanitization denylist hand-duplicated across boot scripts.**~~ **[FIXED in par.1 (v0.103.0): `agentd/tests/env_denylist_parity.rs` asserts `entrypoint.sh` ≡ `overlay/init` token sets (drift panics naming the source + token) and the boot `LD_*` keys ⊆ `shell_mcp.py`'s (different-purpose) linker blocklist — the audit's "same list 3×" framing was inexact; the real relationship is equal-pair + subset-floor, and that is what the test encodes. Negative-control-verified.]**
- ~~**audit86-P2-13 (P2) [new] — agentctl matches flight-event kinds by raw string.**~~ **[FIXED in par.1 (v0.103.0): added `EventKind::as_str()`/`ALL` (single source of truth, unit-proven against serde wire form + exhaustive); `agentctl/tests/event_kind_strings.rs` pins every kind string agentctl matches to a real variant so a rename breaks the test not the TUI. Surfaced a real pre-existing bug → par.1-ar-01.]**
- **par.1-ar-01 (P2) [new; found by the par.1 exhaustiveness guard] — agentctl's error view is blind to tool + inference errors.**
  Inspector "Errors" filter (`inspector.rs:47-48`) and the red colour rule (`views.rs:1229-1230`) match
  `"kind":"tool_error"`/`"inference_error"` — neither is ever emitted by agentd. Tool failures are
  `tool_result` + `data.is_error=true`; inference failures are `agent_failed`/`error` ("inference_error"
  appears only as a `reason` *field*). Net: the operator's Errors filter/colour catches only `agent_failed`.
  Fix is behavioral (the filter must check the `is_error` data field for `tool_result`, not a kind-string
  swap), so it was deferred out of tests-only par.1. Tracked by a self-shrinking `KNOWN_NONCANONICAL`
  allowlist test that fails the day it's fixed. → fold into par.2 or a small agentctl fix.
- ~~**audit86-P2-14 (P2) [new] — QEMU CoS fork silently missing the entire memory-routing feature set.**~~ **[RECLASSIFIED in par.2 (docs-only, 2026-07-25 /autoplan) — this is NOT drift, it is a DELIBERATE + TEST-PINNED divergence, and the original "make both configs match" framing was wrong.** The QEMU image ships **no Qdrant + no semantic-kb sidecar** (that stack is a Docker/compose concern), so the distro CoS correctly runs the native L1 KB only. `agentd/tests/cos_spawn_caps_subset.rs::distro_cos2b_topology` **asserts** the distro jobs never reference semantic-kb (a match would be an inert/mis-wired grant). The three /autoplan voices (Codex + 2 Claude subagents) unanimously found that "one env-expanded config for both" (the original par.2) CANNOT express this — it's a structural section difference, not a path substitution — and that closing P2-14 by "adding semantic-kb to QEMU" would REVERSE a deliberate decision and break that test. **Done:** the QEMU fork header now documents the omission as intentional + points at the pinning test. **Residual product question (NOT a bug):** if QEMU should ever gain semantic KB, that is a distro Qdrant+OpenAI lift, filed as a future distro increment — not a config-unification task.]**
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
- ~~**audit86-P3-3 (P3) [new] — `.expect` on EffectResult agent lookup** (`scheduler.rs:687,724`)~~ **[FIXED in p3.1 (v0.109.0): every production agent-map access in BOTH EffectResult arms (not just the `.expect`s — the `[&id]` indexing panics too) is now a `let Some(..) else { record EventKind::Error(site,agent) ; continue/fall-through }`; SetCaps returns `Err`. /review caught + fixed a subtlety: the inference arm's shared `drain_deferred` was restructured OUTSIDE the agent guard (a labeled `'agent:` block) so a slot-deferred agent is still admitted when the result-agent vanished — else a `max_concurrent_inferences=1` deferred agent would strand. Structurally panic-free; effect loop is inline in `select!` so no focused unit seam.]**
- ~~**audit86-P3-4 (P3) [new] — cred.7 added `credential_health` without a FORMAT_VERSION bump**~~ **[STRUCK as a DO-NOT-DO (p3.1 verification, 2026-07-26): the audit item conflicts with a sound documented decision. `checkpoint.rs:16-23` deliberately stays at 4 because every added field (incl. `credential_health`, already `#[serde(default)]` "Absent in v1–v4 → empty") is PURELY ADDITIVE, so both directions are already safe. Bumping to 5 would make an OLD binary refuse a NEW checkpoint on ROLLBACK (`format_version > FORMAT_VERSION` → rename corrupt → DISCARD all CoS state) — the exact data loss that decision fights. The "silent downgrade drop" the audit feared is graceful degradation (an old binary loses transient cred-health awareness), NOT corruption. Only bump for a genuinely BREAKING non-additive schema change.]**
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

**AUDIT-v0.97 holistic-stack review (2026-07-25, cross-model /review over main..par.1 — 6 increments).**
Two Codex High/Medium findings were FIXED in the review (F1 FsRead-privileged in `management.rs`,
F4 checkpoint `.restored`-fallback in `checkpoint.rs`); the two below are deferred with a precise fix
direction. Claude adversarial pass was SOUND on all 5 focus areas; Codex caught F1/F3/F4 that Claude missed.
- **budget.1-ar-01 (P2) [new; Codex Medium, holistic review] — `MaxTokens` truncation now reports clean
  completion for ALL resettable agents, not just resident ones.** `agent/mod.rs:724` only hard-fails
  `StopReason::MaxTokens` when `!budget_resettable`, but `scheduler.rs:1510` sets `budget_resettable` for
  *every* agent whenever `budget_reset_interval > 0` (the recommended prod config). So a one-shot job /
  spawned child that hits the model's per-response `max_tokens` output cap falls through to the recoverable
  arm (`:739`), which sets `terminal=true` and returns `AgentEffect::Completed(truncated_text)` — silent
  truncation presented as success, and (for `deliver_content=true` children) fed to the parent. Rare-trigger
  (only on a genuinely truncated generation) and non-security; the sealed-job CoS path is shielded by
  cap.2b `deliver_content=false`. Fix needs a resident/resumable signal on `AgentTask` (doesn't exist today)
  so the recoverable path gates on "resident+parked", not the global window flag — do NOT just revert the
  self-brick fix (reopens audit86-P0-2). Minimum honest interim: mark the completion `truncated:true` so the
  parent isn't misled. → future increment.
- ~~**budget.1-ar-02 (P3) [new; Codex High + Claude INVESTIGATE, both agree bounded] — universal-tier global
  ceiling is a soft cap under concurrency.**~~ **[RESOLVED as P-doc in budget.1-ar-02 (v0.108.0), 2026-07-26
  /autoplan — both review voices said DON'T build reservation machinery, and corrected the premise: (1) the
  overshoot is NOT `(N−1)×max_tokens` with N≤3 — `max_concurrent_inferences` gates only the native in-process
  tier; the egress proxy's accept loop is an unbounded `tokio::spawn` per connection, so proxy N is unbounded;
  (2) the whole surface is DORMANT in prod — cos ships no `tier="universal"` agents and no `[egress] proxy_addr`,
  so `main.rs` never starts the proxy; (3) a universal-only reservation CANNOT make the combined (native+universal)
  ceiling hard — the native term stays post-hoc regardless — so P-fix can't achieve its own goal without also
  reserving the native scheduler path (much bigger). The window is a single-tenant SPEND GUARDRAIL, not a
  security boundary (constitutional #2); the native tier has the identical pre-check/post-account shape unfixed;
  "cognition is bounded" holds asymptotically. **Done:** the `egress.rs` gate comment now documents the accepted
  post-hoc soft-cap semantics honestly, and `global_budget_meter_is_a_posthoc_soft_cap` pins the REAL bound (429
  on the next request once `windowed >= ceiling`), NOT the false `N×max_tokens` bound. **Follow-up (filed, gated
  on universal fan-out actually being enabled in prod):** if a hard cap is ever needed, add (a) a proxy-side
  concurrency semaphore (the real latent hole for future universal configs) and (b) reservation-before-forward
  on BOTH tiers, with an RAII release-guard capturing the actual-deducted amount (the `saturating_sub` phantom-
  budget trap) and covering the failure early-returns at egress.rs :499/:539/:552 + await-cancellation.]**
- **run.1-ar-01 (P3) [new; testing-specialist, holistic review] — durability fixes are helper-tested, not
  call-site-tested.** Three run.1 fixes assert the helper in isolation but not the wiring that invokes it, so
  deleting the call site leaves every test green while the bug returns: `runs/store.rs:309` `close_segment →
  prune` (only `prune()` is tested directly), `agent/mod.rs:611` `cap_short_term` call in the `MemoryPaged`
  path, and `flight_recorder.rs:43` the metadata-seed rotation path (tests seed the counter manually, never
  from file metadata on open). Also the par.1 agentctl kind-string guard pins a hand-maintained mirror, not a
  scan of agentctl source — a NEW `"kind":"…"` match added to agentctl and never added to the list isn't
  caught. Add call-site / metadata-seed / source-scan tests. → a test-hardening pass.
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
- **`pending_focus` can stick forever and later hijack the selection** (P3, pre-existing ux.3 M5,
  found by Codex during ux.13-TUI's /review, 2026-07-28): `App::apply_snapshot` refuses to clear a dead
  selection while it equals `pending_focus` (`app.rs`), and `pending_focus` is only dropped when an agent
  with that id appears. A spawn that never materializes therefore pins the selection indefinitely, and if
  an UNRELATED agent later takes that id — CoS ids are config-fixed and cron respawns them — the fold
  silently moves the operator's selection onto it. Wants a deadline on `pending_focus` (drop it after N
  ticks or on the next snapshot that lacks the id, whichever the spawn race actually needs). Not touched
  by ux.13-TUI: the row-action overlay pins its OWN target and does not read the selection. Depends on:
  none.
- **`detect_source_fallback_to_http_when_no_fuse` fails whenever agentd is running locally** (P3,
  found by /qa 2026-07-28): the test probes `http://127.0.0.1:7999/healthz` and asserts the fallback
  does NOT resolve to HTTP, so it goes red for any developer who has agentd up — which is this repo's
  own documented workflow (`cargo run -- agent.toml` + `agentctl watch`), and could bite CI if the port
  is ever bound. Pre-existing (predates ux.13-TUI); left untouched because /qa's rules forbid modifying
  existing tests. Fix: give `detect_source` an injectable probe URL, or have the test occupy an
  ephemeral port and assert against that instead of a well-known one. Depends on: none.
- **The Budget cell rounds an overshoot into invisibility below 10k** (P3, found by /qa 2026-07-28):
  `format_budget_cell(2700, Tokens(2000))` renders `2k/2k`, so a real over-cap agent (the state where
  Park is refused, since capping at the spend would RAISE the ceiling) reads as merely "at its limit"
  in the table. Above 10k it is visible (`119k/100k`). The row-action overlay explains it in full, so
  this is cosmetic; the honest fix is one decimal place when spend exceeds the cap. Pre-existing
  formatter, shared by every budget cell. Depends on: none.
- **Park cannot name its own deadline** (P3, found by /review's security specialist, 2026-07-28):
  with `budget_reset_interval > 0` a parked agent resumes at the next window rollover, and the cockpit
  can only say *that* it will, not *when* — `budget_resettable` is a bool. The honest copy shipped
  ("resumes by itself at the next budget-window rollover"), but an operator parking a runaway at 2 am
  wants "resumes in ~6h". Needs `budget_window_start` + `budget_reset_interval` (or a precomputed
  `next_rollover_at`) on the snapshot next to `budget_resettable`, then a relative-time render in
  `menu_items` + `verb_requested_text`. Cheap once someone wants it; the plumbing pattern is exactly
  what `budget_resettable` just established. Depends on: none.
- **`approve_with_kind` has no HTTP route, so `[d]` is FUSE-only** (P3, found by /review's security
  specialist, 2026-07-28): `HttpSource` inherits the trait's plain-approve fallback, so "don't ask
  again for this kind" registers no standing rule over `--url`. ux.13-TUI made the TUI SAY so
  (`supports_auto_approve_kind()`, and the result line now reads "…but 'don't ask again' is FUSE-only")
  rather than silently claiming a policy that does not exist — but the capability is still missing on
  the surface most operators use. Needs an `auto_approve_kind` field on the approve route +
  `HttpSource::approve_with_kind` + the override returning true. Depends on: none.
- **`converse::dispatch()` still blocks the loop for the duration of its HTTP calls** (P3,
  downgraded from P2 by ux.13-TUI): the *user-visible* half is fixed. `Enter` no longer calls
  `dispatch()` from the key handler — it pushes the optimistic echo, sets the rail's
  `Dispatching…` phase, and parks the turn in `App.pending_verb`; `run_tui_loop`'s
  `drain_pending_verb` performs the call on the next iteration, after that frame is flushed
  (measured on a real pty: echo + `...` on the wire 0.02 s after Enter, against a server that
  took 2.5 s to answer the spawn). What REMAINS is that the call still runs on the loop thread,
  so for its duration (worst case ~8 s: `load_snapshot` 5 s + `spawn` 3 s) redraws are paused
  and queued keystrokes — including Ctrl-C, which raw mode delivers as a key event, not a
  signal — are not serviced. Closing that needs the background-thread dispatch path with
  channel-based result delivery into `step()` described in the original entry; the `pending_verb`
  slot is the seam it would plug into. Depends on: none.
- **`denied_edges` still leaks for UNIVERSAL-tier agents** (P2, filed by ux.6a's fix-review red
  team): `forget_agent` is wired only into `handle_agent_terminal`, which `scheduler.rs` itself
  documents as "a native-only funnel". The two WIRED proxy denial sites (`egress.rs` budget and
  global-window) belong to universal-tier workloads, torn down via `reg.deregister_by_key(...)`
  with no `forget_agent` call — and those are the only NON-terminal denials, i.e. the only place
  the edge trigger does real work and the only place the entry is never reclaimed. Fix: call
  `forget_agent` alongside every `deregister_by_key`, ideally via one helper so the two cannot
  drift, and extend `handle_agent_terminal_clears_egress_deny_edges` to a universal agent.
  Depends on: none.
- **`agentctl verify` does not know rotated segments exist** (P2, filed by ux.6a): after the
  first rotation the documented command prints `chain ok: 3 receipts verified` while ~100 000
  receipts sit in `.1`–`.3`, with no warning and no `--all`. `rotate_segments`' own doc leans on
  this reader ("the only readers are `agentctl verify` and the operator"), and no FUSE or HTTP
  surface exposes evidence, so there is no second reader to catch the narrowed scope. Fix: glob
  the siblings, verify each independently, print a per-segment summary, and state that the seam
  between segments is not cryptographically linked. Depends on: none.
- **Rotation is not crash-durable: no parent-directory fsync** (P3, filed by ux.6a): durability
  rests on `sync_data()` (fdatasync), which does not persist the DIRENT of a newly created file
  — and rotation creates a new file every 32 MiB and performs four un-fsynced renames. A crash
  just after a rotation can lose the new segment's dirent, discarding receipts `write_receipt`
  already acknowledged. Pre-existing in kind (p7.5 had it for the first-ever file) but rotation
  makes it recur. Fix: `File::open(parent)?.sync_all()` after the renames and after creating the
  replacement. Depends on: none.
- **Rotation-at-open runs BEFORE torn-tail repair** (P3, filed by ux.6a): a process killed
  mid-write on an at-cap file leaves a never-fsynced fragment; the next boot renames that
  fragment into `.1` untouched and repairs a brand-new empty file instead, so
  `agentctl verify evidence.jsonl.1` fails with a parse error — i.e. the tamper detector fires
  on a crash, in a subsystem whose claim is that a verify failure means tampering. Fix: resume
  (and repair) before rotating at open, or repair the segment in place after the rename.
  Depends on: none.
- **The parked-for-eviction name has no reaper** (P3, filed by ux.6a): if the park succeeds and
  the shift then fails, rotation latches off and a full 32 MiB segment of signed receipts is
  stranded at `evidence.jsonl.evicting` — a name no reader knows about — so the documented
  128 MiB retention bound silently becomes 160 MiB. Self-heals only in that the next successful
  park renames over it. p3.1 set the opposite precedent with its orphaned-checkpoint-tmp sweep.
  Fix: sweep it at `open`, ideally renaming it into the first free numbered slot so the receipts
  stay verifiable, and report the choice in `resume_note`. Depends on: none.
- **OV-1's FsWrite guard does not cover rotated segments** (P3, filed by ux.6a): `main.rs`
  checks only `evidence_path`, while rotation now creates `.1`/`.2`/`.3` (plus a transient
  `.evicting`) holding up to 96 MiB of the same signed receipts. `Path::starts_with` is
  component-wise, so a directory prefix is still caught, but an explicit
  `FsWrite { prefix = "<dir>/evidence.jsonl.1" }` passes the gate and hands a sandboxed MCP
  server write access to a segment of the chain. Fix: check every name the writer can produce,
  or simpler and drift-proof, assert the evidence file's PARENT directory is outside every
  FsWrite prefix. Depends on: none.
- **Segment eviction is not durably recorded** (P3, filed by ux.6a): `rotate_segments` announces
  the deletion of ~32 MiB of signed receipts on `tracing` only. `EvidenceWriter` holds no
  `FlightRecorder`, so nothing durable records which segment went — which sits badly against the
  "Record everything" invariant precisely where the record being destroyed IS the audit record,
  and against THREAT_MODEL §8.7.1, which names an out-of-band `(pubkey, max seq)` as the only
  way to detect rewind. Fix: pass the recorder in (or surface the dropped segment for `main.rs`
  to record) and emit the segment name, receipt count and final `seq` before unlinking. Also
  document the deletion in DEPLOYMENT.md and consider making the count configurable.
  Depends on: none.
- **`write_receipt` fsyncs under a `std::sync::Mutex` on tokio worker threads** (P3, filed by
  ux.6a): `evidence.rs`'s `write_receipt` holds the `Inner` lock across `writeln!` + `flush()` +
  `sync_data()`, and its callers run inside async contexts (`egress.rs`'s proxy handler,
  `scheduler.rs`'s `make_infer_future`). Benign **today** because receipt volume is bounded by
  real work: one per inference (amortized against a network round-trip, invisible) plus, since
  ux.6a, at most one per deny episode per agent. **It becomes a live problem the moment anything
  makes receipts per-request** — which `ux.6b`'s B1 chain-widening explicitly would, putting a
  synchronous fsync on every tool call and capability check. Any such change must FIRST move the
  write off the reactor onto a dedicated writer lane, exactly as ux.11b did with `run_writer`.
  Depends on: none (but is a hard prerequisite for ux.6b B1).
- **The receipt chain's signing key never leaves the boundary, so verification is
  self-attestation** (P3, filed by ux.6a; the honest fix for the whole
  delete-and-restart-at-0 class): `EvidenceWriter::open` generates the Ed25519 key locally and
  rewrites the `.pub` from the private key on every open; `verify_chain` reads that same `.pub`.
  So anyone who can write the directory can mint a key and rewrite the log from genesis, and
  deleting the file restarts the chain at 0 and still verifies. Segment seams (ux.6a rotation)
  are likewise unprovable. ux.6a **documented** all of this (`THREAT_MODEL.md` §8.7.1) rather
  than fixing it, because fixing it means moving the key outside the boundary: customer-held
  key, external timestamping, or control-plane countersigning, plus an out-of-band record of
  `(pubkey, max seq)` to make rewind detectable. **This — not chain widening — is what would
  change what can honestly be claimed**, and it is the piece a governance buyer would actually
  need. Depends on: a decision about where the second party lives (mv control plane?).
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
- **cos-dev-04 (P3) — the same two templates are now ALSO stale on the R3/R4 prompt-logic
  fixes.** Found reviewing attn.2 R3+R4 (2026-08-04): `templates/cos-inbox.template.toml` still
  queries `q=newer_than:1d` (R3.1 replaced this with `q=in:inbox` in both shipped configs — an
  un-archived old thread otherwise vanishes after one day) and reports no `suppressed_count`
  (R3.4). `templates/cos-curator.template.toml` has no CLASSIFICATION RULE (R3.2 — an item can
  still land in both `important` and `response_needed`), no sender-text neutralisation rule at
  all (R3.3's `[`/`]`/`|` escaping — a bare `templates/cos-curator.template.toml` markdown brief
  is exposed to the exact injected-link risk THREAT_MODEL.md §9.5 documents for the hardened
  configs), and still writes the fixed `brief-{date}.md` (R4 — a same-day re-run truncates it).
  `agentd/src/config.rs`'s `COS_PROMPT_SOURCES` test array already includes both templates and
  deliberately filters the `{ts}`/link/escaping assertions to `dev`/`overlay` only ("the
  templates do not author the markdown brief" — true for cos-inbox, but cos-curator DOES), so
  CI is green while an operator using `agentctl spawn cos-curator` gets none of these fixes.
  Same fix shape as cos-dev-03: mirror the fixes into both templates, or add an explicit banner
  in each stating they are a stripped-down demo, not the hardened path. Depends on: none.
- **attn.2-ts-01 (P2) — `{ts}` collision resistance rests on the wall clock, not a monotonic
  identifier.** Found at /ship's adversarial pass (Codex + Claude subagent, cross-model
  agreement, 2026-08-04): `dispatch_run_job`'s nanosecond-resolution `%H%M%S%9f` (the QA fix
  in this same increment, replacing 1-second `%H%M%S` after a live-driven collision) shrinks
  the same-day-refire collision window from "one wall-clock second" to "the same nanosecond",
  but does not eliminate it — a repeated or backward-stepped system clock (NTP correction, VM
  clock skew) can still produce two identical `ts` values, silently reproducing the exact
  overwrite R4 exists to prevent, now at much lower but nonzero probability. A scheduler-owned
  monotonic sequence number, per-run UUID, or an explicit uniqueness check + retry on
  `dispatch_run_job`'s existing collision guard would close this fully. Not fixed here: R3+R4's
  stated scope was "non-destructive for a same-day re-fire under normal operation", not
  "collision-proof under adversarial clock manipulation" — the nanosecond fix already closes
  the measured, reproduced failure mode; this is defense-in-depth for a much rarer trigger.
  Revisit now that attn.2-R5 (manual fire) has shipped — it makes rapid re-fires routine
  rather than exceptional; the manual-fire confirm overlay warns about the SAME-DAY overwrite
  risk in prose, but this clock-skew collision is a narrower, code-level gap that prose can't
  close. Depends on: none.
- **attn.2-esc-01 (P3) — the sender-text neutralisation rule has never covered `<`/`>`, in
  either the pre- or post-R3.3 entity list.** Found at /ship's adversarial pass (Claude
  subagent): verified against `git show <pre-attn.2-commit>:agentd/cos.agents.toml` — the
  ORIGINAL 5-entity rule (`[` `]` `(` `)` `|`) never named `<`/`>` either, so R3.3's narrowing
  to 3 entities did not regress or "reopen" anything; this is a pre-existing gap, not
  introduced by this PR. Sender-controlled email text (`from`/`subject`/`ask`/`summary`/
  open-item `text`/`focus_recommendation`) can still carry raw `<`/`>`, which is latent (not
  live) against today's consumers — a terminal `cat` and the ratatui TUI do not interpret
  HTML — but would become a live HTML/script-injection vector for any future consumer that
  renders the brief as HTML (a web viewer, a markdown-to-HTML bridge). This is the same
  documented limitation as `brief-03`/`brief-04` ("escaping is a prompt rule, not code
  enforcement" — the real fix is runtime-authored markdown from the typed `BriefRecord`, not
  another prompt-text patch) — file alongside that work rather than as a standalone escape-
  list addition. Depends on: brief-04 (runtime-authored brief markdown).

## brief.1 — Open items (from /review, 2026-07-29)

- **brief.2 (P1) — The morning brief still re-lists items the operator already handled.**
  This is brief.1's success criterion 1, and brief.1 does **not** deliver it. Found by /review
  (5 of 6 independent passes; Codex returned `Reject` plus two `[P1]`s). brief.1's premise was
  that `open:{date}:{N}` keying caused the re-listing. It does not: nothing reads those keys.
  `kb_search` is scoped to a single segment (`docker/semantic_kb_mcp.py`) and there is no
  list/scan/prefix tool, so `open:*` is **write-only by construction** — the re-key stops
  `ops:entities` accumulating an entry per item per day, and nothing more. The re-listing comes
  from the curator's `kb_search(segment='ops:briefs', …)`, which returns whole historical brief
  JSONs each holding that day's entire `open_items` array, with no filter for what is now done.
  Two structural blockers, either of which alone is fatal: (a) **nothing ever removes an entry
  for a resolved item** — there is no `kb_delete`, and the Qdrant TTL sweep runs once at sidecar
  process start; (b) **neither job can observe resolution** — the curator has no Gmail access, and
  the inbox job's query (⚠ UPDATED by R3.1/attn.2: `q=in:inbox`, was `q=newer_than:1d`) cannot tell
  "replied to" from "quiet" either — inbox presence answers "not archived", not "not yet handled".
  Note the naive fix is a trap: dropping the `ops:briefs` union makes carried-forward items
  vanish the moment a thread goes quiet, which is worse than re-listing.
  Likely shape: let the inbox job read the open set and check, per open thread, whether the newest
  message is from the operator. Needs `{ KbRead = { segment = "ops:entities" } }` on `cos-inbox`
  plus a per-thread Gmail read (read-only scope suffices), and an enumeration path for `open:*`.
  **Gate this on the one-week tally** (`docs/plans/connectors-action-queue.md`): if the operator is
  taking ~2 actions a morning, do not build it. Both CEO voices ranked this whole track below
  `audit-S3`, naming mv design partners, and `p7.7-ar-03`.

- **brief-05 (P1) [new, from /autoplan 2026-07-30] — the whole brief track is being planned against
  artifacts that do not exist. Run the pipeline for seven mornings BEFORE any further brief.* work.**
  Found when an /autoplan CEO voice did the thing three prior review rounds never did: looked at the
  output instead of the source. Verified independently:
  - `~/.agentos-output/` holds **two briefs in fifteen days** — `brief-2026-07-16.md`,
    `brief-2026-07-23.md`. Nothing since. `docker info` reports the daemon down, so nothing is
    producing one now either.
  - **brief.1 (v0.117.0) has never produced a brief.** The real Response Needed table is
    `| From | Subject | Ask | Deadline |` — four columns, **no Thread column**. Every claim about
    what brief.1 changed for the operator is currently unobserved.
  - The model **already escapes** table-breaking characters unprompted: `brief-2026-07-23.md:31`
    contains `Hackathon \| Tech Discussion`, written before brief.1 said anything about escaping.
  This is the same failure mode as brief.1's premise, one level up: we keep verifying code and not
  verifying reality. The tally named in `docs/plans/connectors-action-queue.md` has **zero days of
  data** and has now been deferred twice. It is `docker compose up` and it gates the entire track.

- **brief-06 (P2) [new, from /autoplan 2026-07-30] — brief.2's premise is contradicted by the only
  field evidence there is.** `brief-2026-07-23.md:39` says verbatim:
  *"ℹ️ No prior-day briefs found in KB — all items below originate from today's inbox scan."* —
  while `brief-2026-07-16.md` existed and the curator's STEP 2 had written `ops:briefs`. So the
  carry-forward path found **nothing**, which is the *inverse* of brief.2's stated defect (items
  re-listing forever). Six of the ten "carried forward" items on 07-23 also appear on 07-16, meaning
  the model re-derived them from the inbox rather than reading the KB. **Do not build brief.2 until
  this is resolved** — the defect may be "carry-forward silently finds nothing", not "carry-forward
  never forgets", and those need opposite fixes. Note brief.1's `/review` established that
  `kb_search(segment='ops:briefs')` is the only carry-forward source; this suggests it returns
  nothing useful in practice, which no review has yet explained.

- **brief-03 note (P1 rating CONTESTED, from /autoplan 2026-07-30).** Both CEO voices argued the P1
  is inflated and should be P2: `THREAT_MODEL.md` §9.5 already accepts, open by design, that an
  injected curator can write a *misleading brief* (worked example: `"URGENT: wire funds to X"`), and
  escaping does nothing about fabricated prose — so brief-03 closes a narrow hole in a wall with a
  deliberate wider one. The sender's link is also already live in the operator's Gmail; the brief
  adds disguise, not access. **Rating left at P1 pending the tally** rather than changed unilaterally,
  because the re-rate argument and the deflation argument both now rest on unobserved artifacts
  (`brief-05`). The genuinely high-severity half of §9.5 is *persistence* — untrusted open-item text
  read back into the curator every morning in a context holding `FsWrite` + `BriefPublish` — and
  escaping is the control that helps least with it. Plan (DEFERRED, with the reshape named):
  `docs/plans/brief.3-runtime-authored-brief.md`.

- **brief-07 (P2) [new, from /autoplan 2026-07-30] — `agentctl brief` prints model-authored strings
  unsanitised.** `agentctl/src/brief.rs:61-124` interpolates `narrative`, `last_error`, `stop_reason`
  and `agent_id` straight into terminal output. `narrative` is written by the curator, whose context
  holds untrusted email, so it is sender-influenced — terminal escape sequences in a `println!`.
  The workspace's only control-char stripper is `agentctl/src/watch/memory.rs:261-263`
  (`sanitize_str`, not exported). **~30 minutes, and it lands on a surface the operator actually
  uses** — unlike the markdown file. Deliberately unbundled from brief-03: it has nothing to do with
  a markdown emitter.

- **brief-04 (P2) [new, from /qa 2026-07-29] — the OperatingBrief byte budget depends on a model
  counting UTF-8 bytes.** /qa proved on a real `agentd` that the caps are enforced in bytes at two
  places (`MAX_MEM_CONTENT_BYTES` on the JSON-escaped-and-wrapped entry for L1, `HIT_CONTENT_CAP` on
  raw content for the Qdrant sidecar) while the prompt can only *ask* the model to respect them. With
  80-character CJK subject lines the brief reached 9 163 wrapped bytes against 8 192 and the write was
  rejected — and an over-size write means no brief that morning. Mitigated in the prompt (caps restated
  in bytes with the 2–3× multiplier spelled out, `important ≤6`, and a shrink-and-retry ladder in
  STEP 4 that drops the lowest-urgency entries rather than failing), and pinned by
  `config::tests::cos_operating_brief_at_documented_maxima_fits_the_kb_entry_limit` with ≥512 bytes of
  required margin. But prompt-level byte budgeting cannot be made airtight. Durable fix: enforce on the
  runtime side — have `kb_put` (or a thin wrapper the inbox job calls) truncate deterministically and
  report what it dropped, instead of rejecting the whole entry. That turns a silent total loss into a
  bounded, visible degradation. Evidence + measurements:
  `.gstack/qa-reports/qa-report-agentos-cos-2026-07-29.md`.

- **brief-01 (P3) — `templates/cos-inbox.template.toml` still fetches `format=full`.**
  Line 79 instructs `messages/{id}?format=full`, which the real configs ban in capital letters
  (`cos.agents.toml`: "NEVER request format=full: a single message can be 50–100 KB of base64 and
  will blow your token budget and context window"). That guardrail was cap.2b-ar-02 and it never
  reached the template catalogue. Pre-existing; left alone at /review because deciding whether the
  template should mirror the metadata-only fetch or stay a simpler demo is a judgment call, not a
  mechanical fix. Same file's `open_items` shape and thread-id handling *were* mirrored.

- **brief-02 (P3) — Permalinks never reach Telegram.** The Thread column added by brief.1 exists
  only in `~/.agentos-output/brief-{date}T{ts}Z.md` (timestamped filename since R4/attn.2). The
  Telegram bridge pushes `GET /api/v1/brief`
  (`docker/telegram_mcp.py`), which is the runtime-authored `BriefRecord` (facts from `runs.redb`
  plus a bounded model `narrative`) — a different artifact that has no thread field. If Telegram
  is the surface the operator actually reads each morning, brief.1's criterion 2 is not met there.
  `BriefRecord` is additively extensible (`runs/mod.rs` mandates `#[serde(default)]` on post-v1
  fields, so no new table and no `format_version` bump), which makes this cheap if it matters.

- **brief-03 (P1) [re-rated from P3 at /ship, 2026-07-29] — sender-controlled markdown reaches the
  operator's brief, and escaping it is a model instruction rather than enforcement.** `from`,
  `subject`, `ask`, `summary`, open-item `text`, `deadline` and `focus_recommendation` are all
  written by whoever emailed the operator and land in bullets and table cells. A subject reading
  `Payment overdue [Pay now](https://evil.example)` renders as a live attacker link — no escape
  trick, no `thread_id` involvement, one email.
  **Why the re-rate:** filed P3 on the assumption that the `thread_id` hex guard was the control.
  /ship's security pass showed the guard covers the one field an attacker does *not* need. And
  brief.1 made this worse before mitigating it: the brief previously contained no links at all, so
  a bracketed link was anomalous; the new Thread column normalises `[open](https://mail.google.com/…)`
  throughout the document. `THREAT_MODEL.md` §9.5's own argument — operator judgment cannot detect a
  link whose text is decoupled from its destination — applies with more force to Subject than to
  `thread_id`. A zero-cost phishing primitive into a document whose entire purpose is to be clicked,
  next to a pipeline that legitimately asks the operator to open an OAuth URL, is not P3.
  **Mitigated in-prompt now** (both configs instruct entity-escaping of `[ ] ( ) |` plus dropping a
  leading `!`, pinned by `config::tests::cos_prompts_never_interpolate_unvalidated_thread_id_into_a_link`),
  but a prompt rule is not enforcement. **Real fix:** have the runtime author the brief markdown from
  the typed `BriefRecord` and escape there — same landing zone as `brief-04`, and the two should
  probably be one increment.

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
- ~~**audit-S5 (P2) — signed chain not re-verified on resume**~~ **[FIXED in ux.6a: `resume_chain` now
  verifies the TAIL receipt's Ed25519 signature at open and reports via `EvidenceWriter::resume_note()`
  → an `egress_proxy_failed` flight event with `reason: "evidence_chain_resume"`.]** (same as
  p7.5-scope-03; live append trusts the tail, offline `agentctl verify` still catches).
  `evidence.rs:184`. **It warns and boots — deliberately, and this is not a partial fix.** Before ux.6a
  `resume_chain` verified *nothing* (no JSON parse, no `seq` check, no chain-hash check, no signature),
  so it failed only on an I/O error or invalid UTF-8. Failing closed on a bad tail would therefore have
  created a NEW unbootable `agentd` for anyone who archived, copied, or hand-edited `evidence.jsonl`.
  Full verification stays `agentctl verify`'s job, per segment, offline.
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

**~~p7.7-ar-03~~ (STRUCK 2026-08-08 at its /autoplan premise gate) — the premise was false; `0 denied` is CORRECT**
- **This entry previously stated three things that are all false.** Corrected here rather than
  deleted, because the false version was cited as a priority ranking in CLAUDE.md across four
  sessions. Plan + full evidence: `docs/plans/p7.7-ar-03-egress-counters-http.md`.
  Three independent voices (Claude subagent, Codex, primary) converged; 5/6 CONFIRMED adverse.
- **FALSE claim 1: "The FUSE path reads them from live files."** It does not. `agents_fs.rs`
  exposes `system/egress_addr` and nothing per-agent. BOTH transports get these numbers from the
  same client-side scanner, `count_egress_by_agent` (`reader.rs:364`), which greps `flight.jsonl`
  for literal `"egress_brokered"`/`"egress_denied"`. The real split is whether `--log-path` was
  passed (a CLI arg with no default), not FUSE-vs-HTTP.
- **FALSE claim 2: "ux.6a made denials real [in this counter]."** `EgressDenied` has two emitters,
  both in `egress.rs`: `record_denied` (`:303`, `#[deprecated]`, no production callers) and
  `record_denied_policy` (`:358`), whose only production call sites (`:609`, `:643`) are inside
  `handle_proxy_request` (`:481`). `egress.rs:637` states in code that the proxy "never starts in
  production"; neither shipped config sets `[egress] proxy_addr`. ux.6a made denials real in the
  **receipt chain**, not the flight log — `receipt_denial_once` emits `ActionReceiptEmitted` and
  deliberately NOT `EgressDenied` (`egress.rs:367`: "so the same fact is not recorded twice").
- **FALSE claim 3: "HTTP mode shows a false 0 denied."** `0` is the truth *for the metric the
  pane counts*. The pane counts `egress_denied`, which never fires (claim 2). All three
  `receipt_denial_once` calls are behind `if sched.budget_reset_interval == 0` and both configs
  ship `86400`, so no signed refusal receipt fires either. Under a window, budget exhaustion is a
  **deferral** (ux.8′) and `scheduler.rs:2192` forbids receipting a deferral.
- **CORRECTION to an earlier draft of this entry (caught by the Codex voice, 2026-08-08).** An
  earlier version claimed "4 of the 5 `AgentAdmissionDenied` sites are gated". That was
  overstated — it came from a nearest-preceding-string heuristic, not brace analysis. The
  accurate breakdown, verified by reading each block:

  | Site | Reason | Reachable in shipped config? |
  |---|---|---|
  | `scheduler.rs:1294` | `shutdown` | yes (at shutdown; not a policy verdict per ux.6a) |
  | `scheduler.rs:1971` | `shutdown` | yes (same) |
  | `scheduler.rs:1999` | `global_budget_exhausted` | **no** — inside `if !budget_ok { if reset_interval > 0 { return; } … }` |
  | `scheduler.rs:2082` | `agent_budget_exhausted` | **no** — inside the `terminate_legacy` loop, populated only on the no-window path |
  | `scheduler.rs:2178` | budget (either) | **YES — fires unconditionally**, then `if reset_interval > 0` defers. Only the RECEIPT is gated |

  So there IS a real production signal for budget pressure: `agent_admission_denied` at `:2178`,
  followed by a deferral. It is simply not a *refusal*, not receipted, and **not counted by any
  cockpit surface**. This does not change the strike (the pane counts `egress_denied`, and nothing
  counts `agent_admission_denied`), but it matters for `gov1-premise-01` below: gov.1 has a real
  class to cover, if it covers the right one.
- **Why this was not merely closed:** the counter is measuring the wrong thing, and the surface an
  operator would reach for (Inspector `Errors` filter, `inspector.rs:74`) omits
  `agent_admission_denied` from its seven kinds. The real gap — *the cockpit cannot show WHY an
  agent stopped, and under a window the honest answer is "deferred", which has no denial event by
  design* — is **folded into `gov.1`** (operator decision, 2026-08-08), which already owns the
  denial-coverage question. See `gov1-premise-01` below.

**gov1-premise-01 (P1) [new, from p7.7-ar-03's /autoplan, 2026-08-08] — gov.1 plans to sign a
projection whose denial class is structurally empty**
- `gov.1`'s coverage bullet (`docs/plans/zk.1-hybrid-crypto-verification.md`) lists the classes its
  signed projection will cover: *"tool invocations, capability verdicts, approvals/denials, cancels,
  budget mutations, **egress denials**"*. Per the p7.7-ar-03 evidence above, **egress denials never
  fire in the shipped config**, no signed refusal receipt fires either, and native budget pressure
  degrades to a **deferral** under `budget_reset_interval = 86400`.
- **The class that DOES fire is `agent_admission_denied` (`scheduler.rs:2178`), followed by a
  deferral.** So gov.1 has real signal available — "the runtime declined to admit this turn right
  now" — but it is admission *pressure*, not refusal, and signing it under a "denials" label would
  misdescribe it. Both CEO voices flagged this independently.
- If gov.1 ships as currently scoped, it produces a signed, auditor-facing log whose denial section
  can never contain a denial — **ux.6a's `record_denied`-with-zero-callers defect recreated one
  layer up, with a signature on it.** A chain that structurally cannot say "no" is the exact thing
  ux.6a's de-claim work existed to stop claiming.
- **gov.1's /autoplan MUST consume this before scoping coverage.** Either the projection covers the
  classes that actually fire (`agent_admission_denied`, deferrals, `ActionReceiptEmitted`), or the
  increment states plainly that denial coverage is empty-by-construction in the default config.

**test-env-01 (P2) [new, 2026-08-08] — `cargo test --workspace` FAILS whenever the operator's
own agentd is running**
- `agentctl/src/watch/source.rs::detect_source_fallback_to_http_when_no_fuse` asserts
  `detect_source(None, tmp)` returns `Err` "when neither FUSE nor HTTP is reachable". The
  fallback probes the default management port, so if anything is listening on
  `127.0.0.1:7999` the call SUCCEEDS and the test fails.
- Reproduced deterministically 2026-08-08: fails with `docker compose up -d cos` running
  (compose publishes `127.0.0.1:7999->7999`), passes the moment `cos` is stopped. Nothing to do
  with the code under test.
- **Why it matters more than a flake:** this project's whole premise is the operator dogfooding
  a live CoS. The quality gate CLAUDE.md mandates before every commit therefore fails in the
  exact configuration the project asks the operator to be in, which trains people to ignore a
  red suite. It also means CI green ≠ local green for anyone running the stack.
- Fix: bind the probe to an ephemeral port injected by the test, or have `detect_source` take
  the port as a parameter so the test can point at a closed one. Do NOT "fix" it by asserting
  either outcome — that would make the guard unfalsifiable.

**attn.4-curator-offset-01 (P1) [new, MEASURED in a live shadow cycle, 2026-08-08] — native
cron changes `cos-curator` from a sequential handoff to a fixed offset, and the measured margin
is ~2 minutes**
- **Measured, not reasoned.** A live shadow-equivalence cycle was run with both paths forced onto
  the same clock (`[[jobs]].schedule = "*/10 * * * *"` + `TRIGGER_CRON="*/10 * * * *"`), on a real
  agentd against real Gmail:

  | Job | Native shadow computed | Legacy LLM actually fired | Verdict |
  |---|---|---|---|
  | `cos-inbox` | 19:20:00.013 (lag +0.01s) | 19:20:03 | **equivalent** — 3s delta is `wait_for_trigger(timeout_s=20)` poll granularity; native is the more punctual of the two |
  | `cos-curator` | 19:15:00 / 19:25:00 (fixed offset) | **19:23:04** | **NOT equivalent — semantic difference** |

- **The legacy path is a sequential handoff.** `cos-inbox` completed 19:23:01; `cos-curator`
  spawned 19:23:04, three seconds later. Curator waits for inbox to FINISH — that sequencing
  lives in the orchestrator's prompt loop, not in Rust. Native cron instead fires curator on a
  fixed wall-clock offset regardless of inbox's state. `cos.agents.toml` already admits this
  ("there is NO automatic inbox→curator handoff in Rust… a fixed offset is the simplest native
  equivalent"); what this cycle adds is the NUMBER.
- **`cos-inbox` took 2m58s. The production offset is 5 minutes. Margin ≈ 2m02s.** If inbox ever
  runs long (heavier mail day, slow Gmail, a retry), native fires curator BEFORE inbox has
  written `mail:raw` / `ops:entities`. Curator's `kb_search` then returns nothing and it
  assembles a brief from missing data — **no error, just a thin or empty brief with no visible
  cause.** Same silent-failure shape as `brief-06` (carry-forward found nothing) and the
  `suppressed_count` gap.
- **RESOLVED (partially) 2026-08-08 — operator chose option (a): offset widened 5 → 15 min.**
  `cos-curator` now `schedule = "15 8 * * *"`. Margin goes from ~2m to ~12m, i.e. `cos-inbox`
  may run ~5× longer than the measured 2m58s before the brief silently degrades. Options (b)
  accept-and-watch and (c) a real native inbox→curator dependency were not taken; (c) remains
  the only actual fix and is still unbuilt.
- **This still gates `native_cron_shadow = false`, just with more headroom.** Widening buys
  margin; it does not make the two paths equivalent. The native path still fires curator on a
  wall clock while the legacy path waits for inbox to COMPLETE. **The offset is a deadline, not
  a delay** — if inbox ever overruns 15 min the brief degrades silently, exactly as before.
  Anyone shortening this must re-measure inbox's runtime first, not reason about it.
- **Residual risk unchanged and unmonitored:** nothing detects "curator ran before inbox
  finished." There is no assertion, no event, and no brief field that would reveal it — the
  failure looks like a quiet news day. A cheap guard (curator records whether the inbox
  occurrence it expected is present) does not exist and is not filed as built.
- **Related, same cycle: native tick latency is not tight.** Shadow-log lag vs `intended_fire_ts`
  measured at +0.01s, +1.97s, **+14.25s** across three fires. That is the scheduler's idle-sleep
  granularity, not a defect — but "native cron" here means "within seconds to tens of seconds,"
  which matters for any two jobs scheduled close together.
- **Still open and unchanged: the double-fire risk on flip.** With `native_cron_shadow = false`
  and the orchestrator's polling prompt still registered (task T7 unbuilt), BOTH paths fire. The
  three `child_id` schemes are documented as coexisting at `scheduler.rs:2784` and the collision
  guard is a plain STRING check, so it cannot dedupe `{job_id}-{intended_fire_ts}` (native)
  against `{job_id}-{date}` (legacy `run_job`). T7 should land with the flip, not after it.

**gov1-evidence-durability-01 (P1) [new, from gov.1's /autoplan, 2026-08-08] — the signed
evidence chain does not survive a `docker compose down`; the unsigned briefs do**
- `evidence.jsonl` exists in exactly ONE place in this repo: `agentd/tests/fixtures/evidence/`.
  A test fixture. Measured 2026-08-08: after the operator took the stack down, `docker volume ls`
  returned **zero volumes** and the chain was gone with them.
- The contrast is self-demonstrating in `docker-compose.yml`: briefs are **bind-mounted**
  (`${AGENTOS_OUTPUT_DIR:-${HOME}/.agentos-output}:/data/output`) and five survive on disk back to
  2026-07-16. `evidence.jsonl` and `flight.jsonl` live in the named `cos-data` volume and do not.
  **The signed chain is less durable than the unsigned markdown.**
- This invalidates any "auditor-grade"/retention claim before coverage or custody work is even
  scoped — ux.6b already recorded that what auditors accept is immutability by STORAGE (WORM /
  Object Lock in the customer's account), and no plan lists retention.
- **Fix is minutes:** bind-mount the evidence + flight paths out of the deletable named volume,
  same shape as the brief mount. Do this before any further governance increment.

**gov1-capdenied-01 (P2) [new, from gov.1's /autoplan, 2026-08-08] — `capability_denied` is the
ONE denial class that actually fires in production, and it is unattested**
- Fires from four production sites, independently of budget and egress: `tools/mod.rs:197`,
  `tools/mod.rs:222` (the tool choke point), `scheduler.rs:2426`, and `scheduler.rs:2738` (the
  `RunJob` capability check that attn.2-R5 just made HTTP-reachable). Verified by grep.
- **It has no receipt path at all.** So the runtime's only real "no" — refusing a tool call — is
  the one thing the signed chain never records, while the chain is 100% `allowed` by construction
  under the shipped config (see `p7.7-ar-03` above).
- Consequence for any future gov.1: coverage is not "widen everything." If coverage work is ever
  done, `capability_denied` is the whole of it; egress and budget denials are documentation, not
  code.

**gov1-claimfix-01 (P2) [new, from gov.1's /autoplan, 2026-08-08] — CLAUDE.md and THREAT_MODEL
carry a denial claim the shipped config falsifies**
- CLAUDE.md's line that ux.6a "made denials real" is false under the shipped config: no
  `egress_denied` fires (proxy never starts), and no signed refusal receipt fires (all three
  `receipt_denial_once` sites are behind `budget_reset_interval == 0`; both configs ship `86400`).
  ux.6a made denials real in the **receipt-emitting code**, not in any reachable production path.
- It is now load-bearing: it was cited as justification in `p7.7-ar-03` (STRUCK) and again in
  gov.1's PT-0. Two increments built on it before it was checked.
- **Fix (~20 min, docs only):** correct the CLAUDE.md line, and state in THREAT_MODEL that the
  chain is 100% `allowed` by construction in the default config and that `capability_denied` is
  unattested. No code.

**watch-egress-01 (P2) [new, from p7.7-ar-03's /autoplan, 2026-08-08] — unbounded per-tick
`flight.jsonl` scan in the AgentDetail view**
- `app.rs:818` calls `count_egress_by_agent` on EVERY tick (default `--interval 1`) while
  AgentDetail is open; `reader.rs:364` `BufReader`-line-scans the whole file with no size bound.
  run.1 caps `flight.jsonl` at 100 MB, so worst case is a 100 MB scan per second.
- The in-code justification — *"egress events are sparse so the scan is cheap"* — is wrong: scan
  cost tracks FILE SIZE, not event density, and `egress_brokered` fires once per inference so the
  file grows every turn. The neighbouring topology scan (`app.rs:812`) is deliberately view-gated
  and comment-bounded at 512 KB; this one is not.
- `count_egress_by_agent` also has **zero test coverage** (defined once, called once, never
  asserted). Fix rides with the cockpit honesty change: delete the scanner and the `Egress:` row,
  or bound and test it.

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

**p7.3-ar-02 (P3) — PARTIALLY addressed by ux.3 (spawn-on-the-fly); CLI-subcommand residual still OPEN**
- **Done (ux.3, the higher-value part):** the `agentctl watch` Spawn view now routes the interactive spawn
  INLINE through `DataSource::spawn()` → `POST /api/v1/spawn` when the active source is HTTP
  (`event_stream_url().is_some()`), so a spawn lands in the already-running scheduler and carries the
  operator's toggled `capabilities` + `priority` (the client `SpawnRequest` was silently dropping both —
  the load-bearing fix). FUSE mode still writes `/agents/control` via `execute_pending_spawn()`.
- **Still open (the LITERAL p7.3-ar-02):** the standalone `agentctl spawn <template> --task "…"` **CLI
  subcommand** still execs a second agentd instead of detecting a running instance and routing to it. ux.3
  did NOT touch that path. Low value (operators drive spawns through `agentctl watch` / the management API,
  not the one-shot CLI), so this residual stays a P3 nit — do only if a running-instance CLI route is
  actually wanted; the extract-shared-helper approach from the original entry still applies.

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
- **Partially addressed by brief.1 (v0.117.0) — NOT closed.** brief.1 added `COS_PROMPT_SOURCES` in
  `agentd/src/config.rs` and now pins *prompt-text* invariants across **four** sources (both configs
  plus both `templates/cos-*.template.toml`): open-item key scheme, the link-security rule, `content=`
  vs `value=`, and the byte-budget caps. That closes the axis that actually bit — brief.1 shipped its
  fix to one of four copies, and the QEMU production config would have kept the old behaviour. It does
  **NOT** do what this item asks: no diff of capability sets, MCP server blocks, or segment
  classes, and no `make lint-cos` target. Note the two files are deliberately structurally different
  (the overlay has no semantic-kb sidecar, test-pinned by `cos_spawn_caps_subset.rs`), so a naive
  whole-file diff is wrong — the remaining work is a *selective* structural comparison.

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

- **ci.2-ar-01 (P3) [deferred from ci.2] — broker full TLS-loopback forward E2E.**
  ci.2 P2-7 unit-tests the credential attach/drop transform (`build_upstream_headers`) directly, but
  a full request-through-the-gateway-to-a-mock-upstream E2E isn't covered: the gateway is https-only +
  IP-pinned with a non-injectable cert store, so a plaintext mock upstream is rejected at startup.
  Needs an `allow_insecure_local`-style gateway test-seam or a mock-TLS harness. The security-critical
  drop/attach + D3 query filter ARE covered; this is the last-mile transport E2E.
