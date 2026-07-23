# AgentOS Full-System Audit — v0.86.2 (2026-07-17)

Deliverable for `docs/prompts/14-full-system-audit.md`. Method: constitution docs read
in full; `cargo clippy -- -D warnings` + `cargo test` verified green from `agentd/`;
eight parallel deep-dive passes (security, capability structure, panic/CI, deployment
parity, roadmap/TODOS, skills, ops/reliability, docs/ecosystem) with every finding
anchored to file:line; the two P0s independently re-verified before writing this
document. Findings marked **[NEW]** are not currently tracked in TODOS.md; findings
marked **[TRACKED↑]** exist in TODOS.md but this audit re-rates their priority.

---

## 1. Executive summary

**Security findings (none are P0/P1 exploitable-today; the architecture is sound for
the single-author model it claims):**

- **No exploitable code vulnerability found.** Every THREAT_MODEL mitigation checked
  holds in current code: loopback guard, compose port scoping + `cos-net`/`agent-net`
  segmentation, QEMU hostfwd loopback scoping, `PASSENV_BLOCKLIST` on all three env
  paths, broker log hygiene (no cred.5-ar-01 siblings found), header/query allowlists,
  `classify_tier()` correct across all 8 combinations. `cargo audit`: 0 vulnerabilities
  across 361 crates, and CI runs it with zero ignored advisories.
- **The one real security-shaped gap is the wrong-tier no-op pattern, and it extends
  further than known:** HTTP MCP servers silently discard `capabilities`/`isolation`
  and are *exempted* from `mcp_require_capabilities` (`main.rs:361`) — the operator's
  "everything must be sandboxed" switch does not cover remote tool servers, with no
  warning. Same family: agent-level `Credential` and `Net` grants are decorative.
- **Spawn capability inheritance is the most consequential open security item**
  (cos-dev-02, re-rated): `dispatch_spawn` photocopies the parent's full cap set
  (`scheduler.rs:1586`), so the Curator — which processes attacker-influenceable email
  text daily — inherits `Credential{Google}`. Post-cred.6, the broker made capability
  scoping load-bearing; all-or-nothing inheritance makes it decorative for children.
  A prompt-injection payload that survives into the curation step can call live Gmail
  APIs and every layer of the credential hardening will faithfully cooperate.
- A **path to third-party security review** exists and is enumerable (§7, direction 7):
  the blocker is the unauthenticated control plane (ux.5 auth), then at-rest
  encryption, egress token scanning (cred.3-ar-S3), HTTP-MCP validation, and a
  THREAT_MODEL refresh (currently 24 releases stale).

**Architecture / completeness findings:**

- **P0-1: the QEMU default boot config does not parse.** `distro/overlay/etc/agentd/agent.toml:16`
  uses `model_id`, a key that has never existed on `ModelConfig` (reproduced by
  running agentd against it). Every default `make run`/`make test`/prebuilt-image boot
  without the CoS cmdline panics PID-1. Invisible because the only QEMU smoke test is
  `workflow_dispatch`-only and its last five runs were already red. **[NEW]**
- **P0-2: the always-on flagship bricks itself in ~1–2 days.** `global_token_budget`
  is documented as a "hard **daily** spend ceiling" (`cos.agents.toml:109`) but
  `tokens_spent` is lifetime-monotonic and checkpoint-restored with no reset mechanism
  anywhere; the shipped remediation (`rm checkpoint.json`) destroys all conversation
  state. Two more clocks kill longer horizons: flight.jsonl has no rotation anywhere
  (streaming deltas ⇒ ~10–100 MB/day), and `short_term` plus several scheduler maps
  grow monotonically for a never-terminating agent. **The runtime is architected for
  runs, not residency** — that's the largest single gap between the product claim
  ("always-on Chief of Staff") and the code. **[NEW / TRACKED↑]**
- **CI tests the code, not the system.** `surfaces/`, `sandbox/`, and `otel/`
  (~164 tests) are never executed in CI on any target; the entrypoint sed pipeline has
  zero coverage (the DRY_RUN hook cred.2 added is invoked by nothing — the v0.86.2 bug
  shipped through exactly this hole); images are published without ever being run; and
  any `v*` tag on a stale commit silently publishes `:latest` (the v0.86.0 staleness
  incident has no guard). The "1420 workspace tests" figure is a local-only guarantee. **[NEW]**
- **The capability abstraction's matcher is sound; its declaration surface is what
  keeps producing bugs.** One enum, five declaration sites with inconsistent
  validation, two enforcement interpreters that each silently ignore most variants
  (6 of 9 variant×tier combinations are inert no-ops), string-typed path identity, and
  no check tying the hand-written copies of any grant together. Both recent production
  bugs (Gmail credential tier, v0.86.2 path) were declaration-surface bugs. Verdict:
  keep the two-tier model, fix the surface (§4, structural finding S1).
- **Docker/QEMU dual maintenance has already failed in both directions:** the QEMU
  fork missed the entire memory-routing feature set (while catching cred.6 — mirror
  discipline is luck, not process), and the default overlay config is unbootable.
  Recommend Docker-first as the declared source of truth with the distro derived via
  config env-expansion (§4, S2; feeds strategic direction 6).
- **Reference docs rot while increment docs stay accurate.** THREAT_MODEL is scoped to
  v0.62 (24 releases stale) and its §1.2 describes the credential architecture the
  project spent five increments eliminating; RUNBOOK and DEPLOYMENT contain
  instructions that silently fail (env advice that `env_clear()` nullifies; jq probes
  for an event kind that doesn't exist); one shipped template is live-broken
  (`librarian-semantic` gates on `VOYAGE_API_KEY` while its sidecar requires
  `OPENAI_API_KEY`).
- **The panic invariant is in excellent shape** — all 74 non-test unwrap/expect/unsafe
  hits classify as safe; every untrusted-input parse path is `Result`-based. One real
  exception found by inspection rather than grep: byte-slice truncation in the
  credential gateway can split a UTF-8 character and panic (`credential/mod.rs:383,395,415`).

---

## 2. Findings table

Severity · area (audit dimension) · evidence · fix. Deduplicated across passes;
overlapping discoveries merged into one row.

### P0

| # | Area | Evidence | Finding → fix |
|---|---|---|---|
| P0-1 | 5 deploy parity | `distro/overlay/etc/agentd/agent.toml:16` vs `config.rs:503-509` (`deny_unknown_fields`) | Default QEMU boot config uses `model_id`; field is `model` — PID-1 panics at parse on any non-CoS boot. Reproduced. → rename key; add the P1-7 parse-all test so the class dies. **[NEW]** |
| P0-2 | 9/11 ops | `cos.agents.toml:109-110`; `scheduler.rs:434,698`; `checkpoint.rs:125` | `global_token_budget` documented "daily" but lifetime-monotonic + checkpoint-restored; no reset path; 24/7 CoS hits permanent `agent_admission_denied` in ~1–2 days → budget reset window (`budget_reset_interval`) and/or management-API budget-reset endpoint; fold into ux.8. **[NEW]** |

### P1

| # | Area | Evidence | Finding → fix |
|---|---|---|---|
| P1-1 | 9 budget | `agent/mod.rs:624-636` (only check site) vs `:413-556` (`step_need_infer` checks `max_turns` only) | Per-agent budget enforced only under `StopReason::ToolUse`; a text-only orchestrated agent spends unbounded across inject cycles. Confirms audit-C1's mechanism → budget fail-fast at top of `step_need_infer`. **[TRACKED↑ audit-C1]** |
| P1-2 | 11 ops | `flight_recorder.rs:23-33`; nothing in entrypoint.sh / `overlay/init` / image | flight.jsonl has no rotation anywhere; otel detects rotation nobody performs; ~10–100 MB/day with streaming deltas → size-threshold copy-truncate self-rotation in `FlightRecorder::record` (otel sentinel already survives it). **[TRACKED↑ from P3]** |
| P1-3 | 11 ops | `agent/mod.rs:67-68,475`; distillation only post-run (`scheduler.rs:811`) | `short_term` only grows; parked always-on agents never distill; full-cloned into every checkpoint (interval 1) and snapshot tick → cap depth + threshold distillation for parked agents. **[TRACKED↑ p5.2-ar-01/audit-C8]** |
| P1-4 | 4 CI | `ci.yml:44-61` (`working-directory` pinned to agentd/agentctl) | `surfaces` (96 tests incl. Linux-gated `agents_fs.rs`), `sandbox` (34), `otel` (34) never tested/linted in CI on any target → `cargo test/clippy --workspace --all-targets` from repo root. **[NEW]** |
| P1-5 | 4 CI / 5 parity | `entrypoint.sh:242-247` (orphaned DRY_RUN hook); `:147-165` (guard covers 1 of 6 sed rules; checks the prompt half of the v0.86.2 pair, not the grant half); `:231-238` (`agent)` case has no guard) | Sed-rewrite pipeline has zero test coverage; PR #124's bug class remains open on 5 of 6 rules → negative-assertion block after rewrite (fail if `"../docker/`, `"memory.redb"`, `"evidence.jsonl"`, `"egress-key.pkcs8"`, `"./output` survive) + same for `agent)`; CI job invoking `DRY_RUN_ONLY=1`. **[NEW]** |
| P1-6 | 9 publish | `ci.yml:193-254`; `release.yml:40` | Any `v*` tag on any commit publishes `:latest` + release binaries; no `merge-base --is-ancestor origin/main` check, no tag==Cargo-version check (image tags from Cargo.toml, artifacts from `GITHUB_REF_NAME` — can diverge) → two-line guard in publish-docker. **[NEW; the v0.86.0 staleness incident]** |
| P1-7 | 5 parity | no test globs `docker/*.toml`, `agentd/*.toml`, `distro/overlay/etc/agentd/*.toml` | No parse check for any checked-in TOML outside `templates/` — P0-1's enabler → `agentd/tests/config_parse_all.rs` (~1 h). **[NEW]** |
| P1-8 | 3 capability | `capability.rs:27-29` (doc), `:76-94` (`normalize_path` strips `CurDir`); `cos.agents.toml:247,375` | "Relative fails closed" is false: relative-vs-relative matches textually, CWD-blind; production dev-mode depends on the undocumented behavior — the v0.86.2 root cause → `AbsPathPrefix` newtype absolutizing at deserialization + at `required_capability_for` (`native.rs:88/128/169`, `main.rs:1345-1349`). **[NEW]** |
| P1-9 | 2/3 capability | `capability.rs:69` (agent-level `Credential` "deferred"), `:162` (agent-level `Net` unconditionally true); `main.rs:361` (`mcp_require_capabilities` filters out HTTP), `:468-513` (HTTP path never reads `capabilities`/`isolation`); `config.rs:604-628` | Wrong-tier grants are silent no-ops — 6 of 9 variant×tier combos inert; HTTP MCP servers exempt from the mandatory-sandbox switch with no warning. The Gmail outage class → tier-legality validation in `Config::validate()` (hard error for HTTP servers with security fields; warn for inert agent-level grants) + `CapabilitiesResolved` boot event logging each agent's and server's *effective* set. **[NEW]** |
| P1-10 | 2/6 security | `config.rs:412-422` (`SpawnConfig` has no capabilities field); `scheduler.rs:1586` (`parent_cap_set.clone()`); `cos.agents.toml:244-250,284,438-446` (the "inbox caps: Mcp{google_oauth} only" comment is fiction) | Spawn inheritance all-or-nothing; Curator (daily attacker-influenceable input) inherits live Gmail access via broker → `capabilities: Option<Vec<Capability>>` on `SpawnConfig`, validated ⊆ parent (subset check is mandatory or the field becomes an escalation vector); merge cos-polish-adv-F2 + F5 (`max_turns` passthrough, `scheduler.rs:1597`). **[TRACKED↑ cos-dev-02 P2→P1]** |
| P1-11 | 3 capability | `capability.rs:513-514` (test asserts `agent/sub` satisfied by `agent/` grant) | audit-C2 confirmed still open: `KbRead{segment:"agent"}` defeats per-agent Tier-3 memory isolation — breaks a locked memory rule; small fix, already P1 in TODOS → reject bare `agent`/`agent/` prefix grants (or require full `agent/<id>`). **[TRACKED, confirmed]** |

### P2 (grouped)

**Always-on correctness (dim 11):**

| Evidence | Finding → fix |
|---|---|
| `scheduler.rs:183-184,243-247`; `agent/mod.rs:206-221` | Restored agents take cfg/model/task/prompt **from the checkpoint, overriding the TOML**; since the CoS never terminates, `docker compose pull && up` shipping a new prompt/budget/model silently changes nothing → fingerprint TOML config into checkpoint; warn/opt-in re-seed on divergence. **[NEW]** |
| `main.rs:1127-1129` | Checkpoint deleted immediately after restore; a crash-loop permanently erases the CoS conversation on the second crash → rename `.restored`, delete after first successful `checkpoint_all`. **[NEW]** |
| `docker/cron_mcp.py:196,247,253` | `_NEXT_FIRE_TS` process-memory only; restart spanning a fire silently skips the daily brief → persist last-fired under `/data`; fire-on-startup-if-missed. **[NEW]** |
| `evidence.rs:4-5,38,107-109,184-197` | evidence.jsonl cannot rotate safely (hash chain; O(file) re-hash at every boot; rename ignored; archive restarts at GENESIS with no segment manifest) → chain-aware segment rotation; teach `agentctl verify` non-genesis starts. **[NEW]** |
| `scheduler.rs:959` (child-only removal), `:1663,1934,2472` (`parent_map` insert-only + checkpointed), `:1020,2368` (`outcomes`), mailboxes/`spawn_depths`/`streamed_agents` same class | Terminal **root** agents leak whole conversations in RAM; several per-agent maps grow with all agents ever spawned and are checkpointed → clear per-agent entries in `handle_agent_terminal` (`:941`). **[TRACKED↑ audit-C8 subset]** |
| `scheduler.rs:718-733` | One-turn at-least-once replay window: duplicate `ops:briefs` append (append-only ignores key); duplicate outbound email possible if L1 send enabled → keyed brief writes; idempotency keys before any irreversible tool ships. **[NEW]** |
| `egress.rs:127-165` | Universal-tier inference spend never enters `state.tokens_spent` — global budget silently excludes a tier ("always accounted" violated) → plumb shared counter. **[NEW]** |

**Security-adjacent (dim 2):**

| Evidence | Finding → fix |
|---|---|
| `Dockerfile:3,55`; `Dockerfile.semantic-kb-mcp:1` | Base images pinned by mutable tag → `@sha256` digests + Dependabot digest updates. **[NEW]** |
| `docker-compose.yml:155-156`; `semantic_kb_mcp.py:53,76,161`; `templates/librarian-semantic.template.toml:10-12`; `template.rs:1081-1082` | Embedding key (`OPENAI_API_KEY`) provisioned raw via compose, bypassing the broker — exactly what ROADMAP:72-73 said must not happen; **live bug:** template gates on `VOYAGE_API_KEY`, sidecar requires `OPENAI_API_KEY` (export VOYAGE ⇒ every `kb_put` fails; export OPENAI only ⇒ template hidden) → fix the gate now; `EMBED_PROVIDER`/`EMBED_API_URL` env triple + broker `Custom` provider as the increment. **[NEW]** |
| TODOS re-rates (dim 6) | **[TRACKED↑]** ux.0b-ar-02 P3→P2 (`allow_non_loopback` unscoped bypass; management API now default-on, bare-Linux quickstart documented); ux.9-ar-05 P3→P2 (no CI job builds the Docker image — now the primary distribution); orch.2-ar-03 P3→P2 (SSE lag drops `OrchestratorTurnComplete`; per-token deltas made "buffer full" the expected case). |

**CI / parity (dims 4-5):**

| Evidence | Finding → fix |
|---|---|
| no python step in any workflow | Python sidecar self-tests (oauth 30 checks incl. schema-drift, cron 6, fs_watch 6, …) never run in CI → `for f in docker/*_mcp.py; do python3 "$f" --test; done`. **[NEW]** |
| `publish-docker` | Images pushed without ever being executed → `docker run --rm <image> agentd --help` (or DRY_RUN boot) pre-push; longer term a compose-boot job asserting flight.jsonl shape. **[NEW]** |
| `entrypoint.sh:16-18` vs `overlay/init:49-51` vs `shell_mcp.py:39` | Shell env-sanitization denylist duplicated verbatim in two boot scripts + a third variant; zero sync checks → golden-diff CI test (sourced lib impossible; init is standalone busybox). **[NEW]** |
| `agentctl` `inspector.rs:48-54`, `reader.rs:340-341`, `converse.rs:289`, `orchestrate.rs:164`, `views.rs:1194` | Event kinds matched by raw string despite the agentd crate dependency — a rename compiles and silently blanks TUI filters/streams → `EventKind::as_str()` export or the otel-style literal-vs-serde exhaustiveness test. **[NEW]** |
| overlay `cos.agents.toml` vs `agentd/cos.agents.toml` | QEMU CoS fork silently missing the entire memory-routing feature set (semantic-kb server, `mail:raw` segment/caps, dedup prompt step) — production QEMU CoS lacks email dedup; the fork header's "Key differences" list doesn't mention it → declare in header now; normalized-diff parity test (S2) as the fix. **[NEW]** |
| broker reach (dim 10) | Credential broker unusable by unmodified third-party MCP servers (only in-repo Python servers rewritten, cred.4b) — first-party tools brokered, ecosystem tools back to raw-env `passenv`; two-tier security story → see strategic direction 3 / eco.1. **[NEW, structural]** |

**Docs (dim 8)** — one confirmed drift instance per doc, as required:

| Doc | Evidence | Drift |
|---|---|---|
| THREAT_MODEL.md | `:4` v0.62.0 scope; `:53` §1.2 | 24 releases stale; §1.2 "no other credentials; servers get creds via their own env" contradicts `PASSENV_BLOCKLIST`'s 10 vars and the broker architecture itself; §5.2 "cargo audit not in CI" now false (`ci.yml:270-282`) |
| DEPLOYMENT.md (+cos-guide.html:872) | `:203,396,402` | Three troubleshooting jq probes select `kind=="mcp_tool_called"` — **event kind does not exist** (real: `tool_call`, `events.rs:15`); always false-negatives |
| RUNBOOK.md | `:268` vs `mcp.rs:134` | "set BRAVE_API_KEY where you launch agentd" — `env_clear()` makes this silently do nothing; needs `passenv`. Also `:355+` says p5.2+/p6.x "will land" (~58 releases ago) |
| agentd/README.md | `:63` vs `config.rs:518-520` | Streaming default documented `false`; has been `true` since v0.51.0. Also `:59` lists 5 of 9 capability variants; `Net` "advisory" is wrong (Landlock V4 ports) |
| CONVENTIONS.md | `:58-136`; `:203-218` vs `agents_fs.rs:47-89` | Event table missing `mcp_passenv_forwarded` (75 vs 76 variants); FUSE table omits 5 per-agent files + all of `/agents/system/` + `/agents/control`; the "+8, +9" inode guidance would collide (offsets consumed to +14) |
| README.md | `:17` vs `:145` | "complete (v0.66.0)" headline vs same file citing v0.86.0 — self-contradicting 128 lines apart |
| ROADMAP.md | `:59,60,63` vs `:1382` | Build-order header lists cheap-wins/cos-polish/cred.7 as pending; all shipped (v0.75/0.79/0.84) — third recurrence of the audit-D2 class; cred.3.2-ar-02 (canonical status line) was filed to prevent exactly this and never implemented |
| DESIGN.md | `:183` | "SQLite + sqlite-vec" vs actual redb + Qdrant + remote embeddings; `:181` ACP never adopted (deliberately aspirational, but ROADMAP:28-29 mandates same-PR doc updates) |
| CLAUDE.md | status line | "Next: ux.8 or ux.3" vs ROADMAP:64 inserting ux.10 (2026-07-16) — steers every new session wrong |

### P3 (compact)

`credential/mod.rs:383,395,415` byte-slice truncation can split UTF-8 → panic in the gateway request handler (char-boundary helper) **[NEW]** · `oauth_mcp.py:733` interpolates raw exception (sibling `:422` scrubs to type name) **[NEW]** · `scheduler.rs:687,724` `.expect` on EffectResult lookup — invariant holds today but `panic=abort` makes any future break whole-runtime death (defensive `let Some .. else`) · `evidence.rs:142` mutex-poison cascade on receipt path · cred.7 added `credential_health` without a FORMAT_VERSION bump (`checkpoint.rs:148`; CONVENTIONS:225-226 policy violation) → bump to 5 next checkpoint PR · crash-orphaned `checkpoint.json.*.tmp` never swept (`checkpoint.rs:172-179`) · `docker/cockpit.toml` lacks the `[memory]` eviction block both cos configs have · port 7999 hand-duplicated across ~10 files (`config.rs:222` is the authority; the agentctl trio could share the constant today) · port 8020/`semantic-kb-mcp` URL not rewritten by the `cos)` sed — `docker run` outside compose breaks silently · template gating lists hardcoded in `entrypoint.sh:176,180` duplicating `gated_requires` (the `:186` TODO admits it) · `distro/Makefile:144-146` copies 7 named MCP files vs Dockerfile's wildcard — new `*_mcp.py` silently absent from QEMU rootfs · `agent.toml:76-81` comment places `capabilities` under `[tools]` (parse error if followed) · SIGTERM abandons in-flight inference (billed, unaccounted, re-issued — double spend) · cargo-audit compiled from source each CI run · docs claim a "4 MB guard" that doesn't exist (only 6 MB guards) · TODOS.md has ~6 fixed-but-not-struck entries (audit-S1/S2, F-012, F-015, cred.3-ar-02, cred.3.1-adv-01) inflating the apparent open count · the audit prompt itself says `THREAT_MODEL.md` is at repo root; it lives at `docs/THREAT_MODEL.md`.

---

## 3. What was verified and holds (so it isn't re-audited next time)

- Panic invariant: 1,753 grep hits, 1,684 in tests, all 74 non-test hits classify SAFE;
  every untrusted-input parse path (Anthropic SSE, MCP framing, TOML, flight-log
  parsing, management bodies, FUSE) is `Result`-based with bounds.
- Checkpointing: atomic + fsync'd (tmp 0600 → rename → dir fsync), every turn + SIGTERM;
  FORMAT_VERSION floor policy works (v1 fixtures load); volumes survive `compose pull && up`.
- Email dedup is crash-replay-safe (`kb_get` before fetch + stable uuid5 Qdrant IDs).
- Otel event-kind coverage guard is mechanically complete (wildcard-free match over
  `EventKind`; new variants break the otel build).
- The management-API loopback guard, compose network segmentation, and QEMU hostfwd
  scoping all hold exactly as THREAT_MODEL §9 describes.
- `cargo audit`: clean; CI job present with no suppressed advisories.

---

## 4. Structural findings

These are the "this abstraction will keep producing bugs until it changes" items.

**S1 — The capability system: keep two tiers, rebuild the declaration surface.**
The matcher (`satisfies`, normalization, boundary-safe KB prefixes, deny-on-empty) is
sound and well-tested. The two tiers (kernel sandbox compiled at server spawn vs.
per-invocation runtime checks) are mechanically different things and should not merge —
one MCP server legitimately serves agents with different grants, and a merged
per-invocation set has no coherent kernel semantics. What must change is the
declaration surface: (1) an `agentd check` config linter run at test time, CI time, and
container boot (replacing the entrypoint grep) that validates absolute prefixes,
Mcp-server-name existence, KB-segment existence, tier-legality, and prompt-path-literal
vs. FsWrite-grant consistency; (2) a `CapabilitiesResolved` boot event logging each
agent's and server's effective set — the "computed once and logged" property without
changing enforcement; (3) the `AbsPathPrefix` newtype so path identity stops being
string identity; (4) spawn attenuation (P1-10). The enum split into
`AgentCapability`/`SandboxCapability` is the eventual fix if linter warnings prove
noisy — defer it.

**S2 — Deployment duality: Docker is already the source of truth; make it official.**
Evidence that hand-mirroring has failed: the QEMU fork missed memory-routing entirely
while catching cred.6 (discipline is luck); the default overlay config has been
unbootable for weeks with its only smoke test manual and red; the v0.86.2 guard covers
one of six sed rules. Three-tier fix: (T1) parse-all test + negative-assertion guards
(hours); (T2) CI parity test — env-denylist byte-diff, sed-LHS-literals-exist-in-source
check, and a normalized `toml::Value` diff of the two cos configs with a declared
allowlist of intentional differences (a day); (T3) `${VAR}` expansion for path-valued
config fields (`AGENTOS_STATE_DIR`/`OUTPUT_DIR`/`MCP_DIR`) so a single `cos.agents.toml`
serves both platforms and the sed pipeline + overlay fork are deleted (one increment).
T3 is the structural kill; T1/T2 are the tourniquet.

**S3 — "Always-on" is a product claim the runtime doesn't yet implement.**
Budget semantics (lifetime vs. daily), log growth, checkpoint-vs-config precedence,
missed cron fires, and monotonic in-memory state are all "process that runs then exits"
assumptions surviving into a residency product. This isn't one bug; it's a lens. The
run.1 increment below packages the fixes; the deeper point is that every future
increment should ask "what does this do on day 30 of a single process?" — no soak test
or `schedule:` CI trigger exists today to answer it empirically.

**S4 — CI validates components, not the artifact.** Workspace-subset testing, an
untested boot pipeline, unbooted published images, an unguarded tag→publish path, and
mocked boundaries at exactly the cross-file seams where the last three production bugs
lived (broker→oauth_mcp→provider has zero automated end-to-end coverage). ci.1 below.

**S5 — The broker's ecosystem boundary.** The credential broker — the product's
flagship security differentiator — works only for servers rewritten to call it. Any
third-party MCP server falls back to raw env via `passenv`, silently forfeiting audit,
spend caps, and per-agent grants. Either a transparent-proxy wrapper tier (unmodified
servers get injected credentials at the network layer) or, at minimum, `agentctl mcp
add` making the tradeoff visible at onboarding time. This decides whether "MCP is the
tool ABI" means the ecosystem's MCP or only the project's own.

**S6 — Reference docs need an owner.** Increment-deliverable docs stay accurate;
cross-cutting reference docs (THREAT_MODEL, RUNBOOK, CONVENTIONS tables, README config
reference) rot because no increment owns them. Mechanical fixes: the CONVENTIONS event
table gets a completeness test against `EventKind` (the otel pattern, already proven);
THREAT_MODEL gets refreshed as part of the security-review gate (direction 7); a
`doc.1` sweep closes the current drift table.

---

## 5. Skills subsystem (Phase 11) gap analysis

Input for `/plan-eng-review` on the first skills increment. The locked decisions
(SKILL.md format; deny-by-default `Capability::Skill{name}`; governed execution;
quarantine-for-synthesized) survive this audit. The plan's gaps are in the *how*:

**Blocking discoveries (things the plan assumes that the code doesn't have):**

1. **`ToolContext` carries no capability set** (`tools/mod.rs:17-22`) — "list only
   granted skills" is unimplementable as designed. The registry has `cap_set` in hand
   at exactly the right moment; plumb it through. This is a mechanical but wide
   refactor that should land as its own enabler increment (skill.0 below).
2. **No discovery channel.** Claude Code injects skill metadata into the system prompt;
   AgentOS has nothing equivalent — `skill_list` would have to be spontaneously called.
   Without level-1 disclosure (inject granted skills' name+description into the initial
   context at `AgentTask` construction), the feature is inert. Must be in skill.1.
3. **"Sub-agents get a subset of parent grants" is currently vacuous** — same
   `dispatch_spawn` full-clone as P1-10. Skills sequencing therefore *depends on* the
   spawn-attenuation increment (cap.2 below), or the plan's D2 is fiction for children.
4. **The plan's script gate is named wrong:** `ShellExec` is not an agent-level gate
   (`capability.rs:63-67`); the actual idiom is `Mcp { server = "shell_exec" }`.
5. **skill.2's central promise cannot be enforced by the current sandbox.** The sandbox
   is per-MCP-server and boot-time-static (`main.rs:518-524`) — not per-agent, not
   per-skill, not per-invocation. A script runs inside the *server's* envelope shared
   by all agents; nothing verifies server ⊆ agent, and per-skill resource scoping
   requires new machinery (per-invocation Landlock re-exec, per-skill server instances,
   or gVisor execution). "No new exec path" and the resource-scoping acceptance
   criterion are mutually exclusive — the skill.2 eng review must choose.
6. **skill.3's approval object doesn't fit** `ParkedApproval` (it contains a parked
   agent; a synthesized skill has none). Needs a second pending-item type on the
   approvals surface.

**Security requirements for day one (skill.1, not deferred):**

- **The skills directory is a persistence-escalation channel:** any agent or MCP server
  holding `FsWrite` over it can author instructions every future granted agent
  executes — THREAT_MODEL §7.4's memory-poisoning attack at the procedural layer,
  strictly worse. The repo already has the exact guard pattern (OV-1 boot `ensure!`,
  `main.rs:238-260, 975-1030`); apply it to skills dirs vs. all agent+server `FsWrite`
  prefixes. ~30 lines.
- Record a content hash + `source` provenance (`repo|operator|synthesized`) in
  `SkillLoaded` from day one, so signing can arrive later without an event-schema break.
- Ratify now, as plan text: **quarantine-by-default for ALL non-operator-placed
  skills**, not just synthesized ones. "Mutually trusting agents" survives skills only
  while the catalogue is operator-curated; the moment any non-operator write path
  exists, D4's quarantine is the load-bearing control.
- Capability envelopes bound *actions*, not *intent*: a malicious triage skill can
  exfiltrate entirely within the CoS's legitimate `Credential{Google}` + egress grants.
  Detective controls (flight recorder, receipts, spend caps) are the mitigation; say so
  in THREAT_MODEL rather than implying the sandbox contains it.

**Interop corrections (SKILL.md format):** `when-to-use` is not an Anthropic
frontmatter field (it lives inside `description`) — make it optional; `SkillMeta` must
not use `deny_unknown_fields` (ecosystem skills carry `license`/`metadata`) — the
opposite of the `TemplateMeta` choice; `allowed-tools` must be reinterpreted as
advisory + preflight-warning (it must never expand the envelope); enforce the spec's
name charset (lowercase-alnum-hyphen ≤64 = dirname), which kills traversal by
construction; `skill_load` should return the skill's absolute directory path (scripts
reference `scripts/foo.py` relative to it) and hard-cap body size (the 64 KB shell_mcp
precedent) because context paging would otherwise evict a loaded recipe mid-procedure
with only a 200-char preview left.

**Templates-vs-skills verdict: separate catalogues, deliberately.** Templates are
resolved client-side by agentctl at spawn and lowered once (identity); skills are
resolved inside the daemon at tool-call time, N times per lifetime (procedure) — a
genuinely new runtime surface, not a reuse. Build only the convergence points:
`TemplateCapabilities.skills` sugar and `suggested_caps` accepting `Skill` (the spawn
view renders toggles generically already). Record this as a decision so a future
"merge the catalogues" refactor doesn't happen by drift.

**Revised increment split:** **skill.0** (ToolContext cap plumbing — pure refactor,
~half-day) → **skill.1** (catalogue + load + `Capability::Skill`, amended: the
`satisfies_type` empty-name arm so the `filtered_specs` Null-probe doesn't hide the
tools; `parse_cap_alias`/`format_cap`/`cap_add_allowed_by_suggestion` arms; tolerant
`SkillMeta`; size cap; OV-1 skills-dir guard; provenance hash; level-1 disclosure
injection; non-goals stated: runtime grants, FUSE surface, scripts) → **skill.2a**
(scripts via the existing shell_exec envelope, honestly scoped: gate =
`Mcp{shell_exec}`; boot contract check that the server's FS grants cover skills dirs;
THREAT_MODEL states the true boundary is the server's envelope) → **skill.2b** (per-
skill resource scoping — the real governance win, costed honestly: per-invocation
sandboxed `skill-run` helper with skill-scoped `CompiledSandbox` rules intersected with
the loading agent's caps; this is the only security-novel increment) → **skill.3**
(synthesis + quarantine, with the new pending-item type; the `pending/` dir passes the
OV-1 guard too).

---

## 6. Recommended build order

Roadmap-format increments closing the highest-priority gaps, sequenced against the
open tracks (ux.10/ux.8/ux.3 queued; cos-dev open; personal.1 unplanned; mesh deferred).
Rationale for the order: stop the two P0 bleeds and the untested-artifact class first
(they are cheap), make the budget system truthful before building its UI (ux.8), close
the one real security escalation path (spawn attenuation) before skills make prompts
more programmable, then structural cleanups.

**audit.1 — P0 hotfix + guard batch** *(ships this week; no dependencies)*
Fix `model_id` → `model` in the overlay; `agentd/tests/config_parse_all.rs` globbing
all three TOML directories; entrypoint negative-assertion guards for both `cos)` and
`agent)` cases; fix the librarian-semantic `VOYAGE_API_KEY`→`OPENAI_API_KEY` gate;
strike the ~6 fixed-but-open TODOS entries; reconcile the ROADMAP build-order header +
CLAUDE.md status line (third recurrence — also add the cred.3.2-ar-02 canonical status
line while there).
*Acceptance:* default QEMU config parses in CI; a deliberately broken TOML fails
`cargo test`; a deliberately broken sed rule fails container boot with a named literal.

**ci.1 — CI tests the artifact** *(depends: audit.1)*
`cargo test/clippy --workspace --all-targets` from root (surfaces/sandbox/otel gated);
publish-docker gains `merge-base --is-ancestor origin/main` + tag==Cargo-version
checks; Python sidecar `--test` step; `docker run -e DRY_RUN_ONLY=1` entrypoint job;
image smoke (`agentd --help`) before push; automate-or-retire the red qemu-boot.yml
(audit.1's parse test already covers the config half without booting).
*Acceptance:* a PR breaking a surfaces test goes red; a `v*` tag off-main refuses to
publish; the PR-#124 bug class reproduced in a fixture fails CI.

**ux.8′ — budgets, absorbed into the queued ux.8** *(depends: nothing; do before or as ux.8)*
The cockpit budget panel must sit on truthful semantics: budget reset window /
management-API reset endpoint (P0-2); budget check in `step_need_infer` (P1-1);
universal-tier accounting (P2); spawn `token_budget` clamp (cos-polish-adv-F1) +
`max_turns` passthrough. Decide the budget *semantics* first — see open question D2.
*Acceptance:* a 24/7 CoS crosses its window boundary and keeps running inside the new
window; a text-only orchestrated agent stops at its per-agent budget; TUI shows spend
against the window.

**cap.1 — capability declaration surface** *(depends: audit.1; small-medium)*
`agentd check` linter (absolute prefixes, Mcp-server existence, KB-segment existence,
tier-legality incl. hard error for HTTP servers with security fields, prompt-literal vs
FsWrite consistency); `CapabilitiesResolved` boot event; entrypoint switches from grep
to `agentd check`; denial surfacing (per-agent `capability_denied` counter in snapshot
+ ATTN wiring — closes the "silent fail-closed" seed finding); fix audit-C2 (reject
bare `agent/` KB grants).
*Acceptance:* the historical Gmail misconfiguration and the v0.86.2 misconfiguration
both fail `agentd check` in a regression fixture; repeated identical denials surface in
the Dashboard ATTN column.

**cap.2 — spawn attenuation FLOOR** *(depends: cap.1; SHIPPED v0.94.0; P1-10 PARTIAL — see cap.2b)*
`SpawnConfig.capabilities` validated ⊆ parent via `capability_covered_by` (Net & Mcp given real
containment — raw `satisfies` was unsound for both; no-wildcard-arm drift guard); out-of-parent →
`AgentSpawnDenied` (reject, not clamp); cos orchestrator prompt scopes children (curator KB-only,
no `Mcp{google_oauth}`). **Reframed at the autoplan CEO gate (both models, USER decision
2026-07-23):** this closes ACCIDENTAL over-grant only — NOT the injected-orchestrator threat P1-10
names (the orchestrator holds Gmail + chooses child caps while reading untrusted email, so an
injected orchestrator grants Gmail from its own set and the subset check passes). So **P1-10 stays
OPEN**; the real closure is **cap.2b** (de-privilege the orchestrator / static untrusted-data
pipeline). `max_turns` passthrough was CUT (resource limit, not a capability → cap.2-ar-01).
*Acceptance (met):* curator's flight log shows no Gmail tool specs; an out-of-parent child request
is rejected with `AgentSpawnDenied`; the injected-orchestrator bypass is encoded as a passing test
(`spawn_attenuation_documents_injection_bypass`) so the floor is never mistaken for the ceiling.

**run.1 — residency hardening** *(depends: ux.8′; medium)*
flight.jsonl self-rotation (P1-2); `short_term` cap + threshold distillation for parked
agents (P1-3); terminal-root cleanup + per-agent map clearing (P2); checkpoint
`.restored` crash-loop guard; TOML-fingerprint divergence detection; cron missed-fire
persistence; evidence.jsonl chain-aware segment rotation + `agentctl verify` segment
support; FORMAT_VERSION bump to 5 (carrying the cred.7 field it should have had).
*Acceptance:* a simulated 30-day run (accelerated soak test — also add the missing
`schedule:` CI soak job) holds RSS and disk bounded; kill -9 during restore preserves
state; a config edit + restart provably reaches the running agent or warns.

**par.1 — parity tests** *(depends: audit.1; ~1 day)* — S2 Tier 2 as specified.
**doc.1 — reference-doc sweep** *(any time; pairs with the security gate)* — close the
§2 drift table; CONVENTIONS event-table completeness test; THREAT_MODEL refresh happens
in sec.2 below.
**cap.3 — `AbsPathPrefix` newtype** *(depends: cap.1)* — makes P1-8 unrepresentable;
lets the Docker rewrite shrink to prompt text only.
**par.2 — env-expansion single-source config** *(depends: par.1)* — S2 Tier 3; deletes
the sed pipeline and the overlay fork.
**eco.1 — `agentctl mcp add`** *(independent)* — config generation + suggested caps +
runtime-missing/broker-eligible warnings; the highest-leverage third-party-MCP friction
reducer short of the broker wrapper tier.
**sec.2 — security-review gate milestone** *(before any second user; see direction 7)*
— ux.5 auth (bearer + Origin/Host), at-rest encryption for memory.redb/checkpoint,
SecretRewriter (cred.3-ar-S3), image digest pinning, THREAT_MODEL rewrite, triage the
two unsound transitive deps.

**Skills sequencing:** skill.0 + skill.1 slot after cap.2 (skill.1's subset-grant story
depends on it) and after ux.10/ux.8 per the current roadmap order — realistically the
next quarter's track, not this month's. skill.2a/2b follow only after sec.2 exists as a
named milestone, since scripts widen the blast radius of everything above. personal.1
stays unblocked-but-unplanned; it is orthogonal (pure harness) and can interleave
anywhere after an /autoplan pass; nothing in this audit raises its priority.

Suggested next five branches, in order: **audit.1 → ci.1 → ux.10 (already queued;
unchanged) → ux.8′ → cap.1**.

---

## 7. Future directions (options, not decisions)

Anything touching a **locked decision** is flagged ⚠ and requires deliberate operator
sign-off to relitigate; nothing here should be treated as a default.

**1. Skills as a shareable ecosystem.** *What:* skills move from local catalogue to
shared/imported artifacts (eventually a marketplace). *Why:* it's the natural moat —
procedural knowledge compounds where tools commoditize. *Cost:* trust machinery
(signing, provenance, review UX) that doesn't exist; the audit's skill.1 provenance
hash + quarantine-by-default are the cheap down payments. *Tension:* ⚠ the
single-tenant lock's *spirit* — "mutually trusting agents" was scoped to agents, not
skill authors; a shared skill is a third party's instructions running inside your
trust boundary. *Sizing:* multi-phase bet; do not start before skill.1–2b prove local
value and sec.2 exists. *Decision needed:* whether AgentOS's differentiation is
"governed skills host" (lean in) or "personal runtime" (skills stay local).

**2. Templates/skills convergence.** Audit answer: they are *not* one primitive
(§5) — resolved at different times, by different processes, for different lifetimes.
Recommend recording "separate catalogues, shared capability sugar" as a numbered
decision in the skills plan now, closing this question before Phase 11 ships rather
than after. *Sizing:* a paragraph, not an increment.

**3. Cognition-provider neutrality beyond chat.** *What:* embeddings currently
hardcode OpenAI inside the sidecar, the key bypasses the broker, and the shipped
template gates on the wrong provider (live bug). *Options:* (a) small increment —
`EMBED_PROVIDER`/`EMBED_API_URL`/auth-style env triple in the sidecar + route the key
through the broker as a `Custom` provider + fix the gate; (b) declare chat-only
neutrality explicitly in DESIGN.md and accept the vendor tie. *Tension:* not a locked
decision (embeddings postdate it), but the "no vendor lock-in for cognition" spirit.
*Recommendation:* (a) — it's small and the bug forces a touch anyway. *Sizing:* small
increment (harness-only + one broker config).

**4. Notification/approval beyond the TUI.** ux.4 (SSE → local notifier + signed
webhook) is already accepted and is the right first step: outbound-only, through the
broker, deny-by-default — it does *not* change the thin-host framing. The audit adds
one connective observation: approvals + budget alerts + `credential_attention_required`
+ the new capability-denial signal (cap.1) should all ride the same channel; design
ux.4's event taxonomy for that from the start. A companion mobile surface *would*
change the framing (state off-device) — ⚠ flag for sign-off, defer until ux.4 proves
insufficient. *Sizing:* ux.4 medium; mobile = multi-phase.

**5. Cost/budget transparency as a first-class surface.** The audit hardens this from
a UX nicety into a correctness requirement: the budget system currently can't support
a truthful dashboard (P0-2, P1-1). Elevate ux.8 to next-after-ux.10 and absorb ux.8′
(§6). An always-on agent spending real dollars with a lifetime-monotonic "daily"
budget is a trust bug, not a missing panel. *Sizing:* medium (semantics) + the
already-planned panel.

**6. Distro vs. Docker priority.** ⚠ Touches the founding thesis (PID-1 boot,
DESIGN.md Level 1). The evidence this cycle is one-directional: every increment was
built and debugged on Docker; the QEMU path accumulated an unbootable default config,
a red manual smoke test, and a missing flagship feature — nobody noticed for weeks.
Three honest options: (a) **Docker-first, distro-derived** — par.2 makes one config
serve both, ci.1 keeps QEMU parse-checked, boot-tested quarterly; the thesis stays
alive at low cost (recommended); (b) re-commit to the appliance as the near-term
product (then Phase 9 eBPF and dx.4 images deserve the next slots — nothing this cycle
supports that); (c) sunset the distro to a demo. This should be decided explicitly —
right now option (a) is happening *by default*, which the roadmap itself warns against.
*Sizing:* (a) is par.1+par.2, already in the build order.

**7. Formal security review as a distribution gate.** Name **sec.2** (§6) as the
go/no-go gate for "anyone but the author holds credentials in this." The audit's
checklist for what a reviewer needs: authenticated control plane (the blocker —
localhost CSRF is live today per THREAT_MODEL §9.3), at-rest encryption for
memory.redb (which now stores email bodies) and checkpoints, egress token scanning,
HTTP-MCP capability validation, digest-pinned images, gVisor-mandatory for
credential-touching servers, a current THREAT_MODEL. None of this blocks single-author
daily use today; all of it blocks user #2. *Sizing:* multi-increment milestone;
schedule it when a second user becomes plausible, not after.

**8. Multi-device.** ⚠ In tension with the single-device framing. Nothing this cycle
increases its urgency; the substrate pieces (checkpoint portability h8.3, detachable
memory, mesh.\*) remain deliberately deferred. The one cheap thing worth doing now:
keep checkpoint FORMAT_VERSION discipline tight (the cred.7 miss, P3) because every
future migration story rides on it. Otherwise: explicitly *not now*.

---

## 8. Open questions for the operator

D1. **Is the QEMU/distro path a supported product surface or a thesis demo?** Decides
    how much of par.1/par.2 to schedule and whether qemu-boot.yml gets automated or
    retired (direction 6).
D2. **What should `global_token_budget` mean for an always-on deployment** — rolling
    daily window, per-cycle, or lifetime-with-reset-endpoint? Product semantics needed
    before ux.8′ implements them.
D3. **Config-vs-checkpoint precedence on restart:** should a TOML edit win over a
    restored agent's checkpointed config (with a warning), or stay checkpoint-wins
    (with divergence detection only)? Today's silent checkpoint-wins is the worst of
    both.
D4. **HTTP MCP servers with security fields: hard error or warning?** Hard error is
    correct-by-default but breaks any existing config that harmlessly carries the
    fields; warning preserves back-compat but repeats the silent-no-op pattern.
D5. **Accept the TODOS re-ratings** (ux.0b-ar-02, ux.9-ar-05, orch.2-ar-03,
    cos-polish-adv-F2, flight-growth → P2; cos-dev-02 → P1)? If yes, this audit's
    findings table becomes the TODOS delta in the same PR that lands audit.1.
D6. **When does sec.2 trigger?** Propose: the moment a second person's credential (or
    a second operator) is planned, treat sec.2 as a blocking milestone; until then it
    stays a named, scheduled-later gate.
D7. **Skills timing:** hold Phase 11 behind cap.2 + ux.8′ as recommended, or pull
    skill.0/skill.1 forward as the next track after ux.10? (skill.1 is small and
    high-demo-value; the cost is opening the procedural-injection surface before the
    denial-surfacing and spawn-attenuation guards exist.)
D8. **personal.1:** commission an /autoplan pass now (it's unblocked and pure harness)
    or leave it parked behind the cockpit track? Nothing in this audit forces either.

---

## 9. Ratified decisions (operator, 2026-07-17)

All eight open questions were decided; each took the audit's recommendation.

| # | Decision |
|---|---|
| D1 | **Docker-first, distro-derived.** Docker is the declared source of truth. par.2 makes one config serve both platforms (deletes the sed pipeline + overlay fork); ci.1 keeps QEMU parse-checked; boot-test quarterly. |
| D2 | **Rolling daily window + reset API.** `global_token_budget` becomes a per-24h-window ceiling (`budget_reset_interval`), plus a management-API budget-reset endpoint. Implemented by ux.8′. |
| D3 | **TOML wins for config on restart.** On divergence, config fields (prompt, model, budget) re-seed from TOML with a logged warning; conversation state still restores from the checkpoint. |
| D4 | **Hard error** for HTTP MCP servers carrying `capabilities`/`isolation` fields. A security field that does nothing must not parse. |
| D5 | **All TODOS re-ratings accepted** (ux.0b-ar-02, ux.9-ar-05, orch.2-ar-03, cos-polish-adv-F2, flight-growth → P2; cos-dev-02 → P1). The findings table becomes the TODOS delta in the audit.1 PR. |
| D6 | **sec.2 gates on the second user.** The moment a second person's credential or a second operator is planned, sec.2 is a blocking milestone; until then it is a named, scheduled-later gate. |
| D7 | **Skills held behind cap.2 + ux.8′.** Phase 11 does not start before denial surfacing and spawn attenuation exist; realistically next quarter's track. |
| D8 | **personal.1 stays parked.** No /autoplan pass now; interleave later when there's slack. |

Confirmed build order: **audit.1 → ci.1 → ux.8′ → ux.10 → cap.1** (then cap.2, run.1,
par.1/par.2 per §6). *Revised 2026-07-17 at the audit.1 review gate: the operator
swapped ux.10 and ux.8′ after both review models flagged that leaving P0-2 (budget
self-brick) three increments out contradicts the always-on product claim — budget
truth now ships before TUI polish.*
