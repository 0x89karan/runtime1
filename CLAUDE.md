# AgentOS / agentd — Project Memory

You are working on **AgentOS**: a Linux-based operating system where **agents are
the primitive, not applications**, designed to be **super light**. `agentd` is its
runtime. In the full system this process *is* the userspace (PID 1 / the boot
target); today it runs as an ordinary binary on a normal distro.

Read `docs/DESIGN.md` for the full thesis, architecture, and rationale.
Read `docs/ROADMAP.md` for the build plan — **this is the work queue.**
Read `docs/CONVENTIONS.md` before adding a subsystem, tool, or provider.

## Locked decisions — constitutional, do not drift

These were decided deliberately. Do not relitigate or quietly violate them:

1. **Cognition is remote.** The device is a thin agent host. The model is an API
   call behind `InferenceGateway`. There are **no local model weights** and no
   local inference engine. (Adding a *local backend* later is allowed only as a
   new `impl InferenceGateway`, never as a core assumption.)
2. **Single-tenant.** This is an OS for one individual. Agents are mutually
   trusting and run **in-process**. Do not add multi-user isolation, per-user
   auth, or tenancy boundaries. (Capability *scoping between agents* is in scope
   — see the roadmap — but that is about least-privilege, not distrust.)

## Current status

**Current version:** v0.117.0 (shipped 2026-07-29)
<!-- Updated on every release; test-enforced against agentd/Cargo.toml by
     agentd/tests/repo_consistency.rs — a stale line here fails cargo test. -->

**Latest shipped:** brief.1 (v0.117.0) — **the morning brief's action items are addressable, and the
brief survives its own size limit. Its headline claim is WITHDRAWN.** Three prompt edits to the CoS
pipeline (`agentd/cos.agents.toml` + the three other copies) gave every item its Gmail `threadId`, a
thread permalink per row, and provider-native `open:{threadId}` keys instead of `open:{date}:{N}`.
Plan: `docs/plans/connectors-action-queue.md`.
- **The premise was wrong and five of six review passes found it independently** (Codex: `Reject`).
  Handled items still re-list. Nothing reads the `open:*` keys — `kb_search` is single-segment and no
  list/scan/prefix tool exists in either backend, so they are **write-only by construction**; the
  re-listing comes from `kb_search(segment='ops:briefs')` returning whole historical briefs, nothing
  deletes a resolved item, and neither job can observe resolution (curator has no Gmail; the inbox
  job's 24 h query cannot tell "replied to" from "quiet"). **Criterion 1 is OPEN → `brief.2`.**
  Do not let a future session re-derive this: trace the READ path before believing any KB re-key claim.
- **The brief was over its own cap before this shipped** — 8 660 B at the prompt's documented maxima
  against a hard 8 192 B, i.e. no brief that morning with no visible cause. The first fix mis-sized it
  too: `kb_put` measures the JSON-escaped payload inside a provenance wrapper (~600 B more than raw
  JSON), leaving 39 B of real margin. Caps are now in **bytes** (the store counts bytes, so non-Latin
  subjects blew a character-stated limit), plus a shed-and-retry ladder with a guaranteed-fit floor and
  a `⚠ Shortened to fit` line so a shortened brief is never mistaken for a complete one.
- **Three rounds, and every round's fixes were the next round's defect source.** /review 9 criticals →
  /qa 2 more (real `agentd` + fake provider) → /ship's fix-review round 9 more, **five of them
  mutation-proven false greens in guards written one round earlier**: a 4-line scanner window, parens
  counted inside string literals, an assertion a comment satisfied, a regex grep matching two unrelated
  sites, and a cap check covering 2 of 9 caps. All seven controls are now mutation-verified.
- **The security fix first guarded the wrong field:** `thread_id` was locked to `^[0-9a-f]{1,20}$`
  while `subject`/`from`/`ask` still reached markdown raw — `Payment overdue [Pay now](https://evil…)`
  in a subject needed no escape trick. Now entity-escaped by rule; **`brief-03` re-rated P3 → P1**
  because a prompt rule is not enforcement (real fix: runtime-authored markdown, `brief-04`).
- **Prompt adherence is UNVERIFIED and unverifiable here** (no API key, no Docker, no OAuth token;
  faking Gmail means disabling the broker's SSRF controls). The first real brief is the test. **The
  one-week operator tally decides whether this track continues at all** — if it is ~2 actions a
  morning, build nothing further.

**Prev:** ux.6a (v0.116.0) — **de-claimed the receipt chain and closed the `evidence.jsonl`
boot trap.** ux.6 was planned as an "Evidence view" surfacing the signed chain under the roadmap's label
"Provable accountability"; both CEO voices returned RESHAPE and the increment was **split at the /autoplan
premise gate**: ux.6a (this) ships the honesty + durability half with **no UI**, and `ux.6b` (the signed
action ledger) is DEFERRED, named and specified. Plans: `docs/plans/ux.6a-declaim-and-detrap.md`,
`docs/plans/ux.6b-signed-action-ledger.md`.
- **The chain could not say "no."** `EvidenceWriter::record_denied` had **zero production callers** — its
  only two call sites in the workspace were tests — so a 100%-`allowed` receipt log was a property of the
  CODE, not of any run. Wiring only the HTTP proxy would not have fixed it: `egress.rs` says in the code
  that "this proxy never starts in production". The production-reachable denial is **native scheduler
  admission**, now wired. Proven in /qa against a real agentd: `action="inference" verdict="denied"
  principal="qa-runner"`, chain still verifies.
- **Denial receipts are EDGE-TRIGGERED, and that is a security control.** `write_receipt` fsyncs under a
  mutex, so a receipt per attempt would let a retry loop force unbounded fsync'd writes to the file
  `agentd` reads at boot and — with rotation — roll the audit log to evict older segments. So the flight
  event fires per attempt; the signed receipt fires once per `(agent, reason)` episode. Deferral is NOT
  denial (ux.8′), and shutdown is not a policy verdict — neither is receipted.
- **Closes `audit86-P2-4` + `audit-S5`**, both filed against `run.1`, which shipped without them.
  `resume_chain` used to `read_to_string` the WHOLE file at every boot on a **fail-closed** path; it now
  reads a bounded 64 KiB tail, repairs a torn tail, and signature-checks the tail receipt (warn, never
  refuse — it verified *nothing* before, so failing closed would have bricked anyone who archived or
  hand-edited the file). Measured in /qa: 1 KiB → 0.14 s vs 30 MiB → 0.16 s. **Rotation needed no format
  or verifier change** — genesis anchoring only ever blocked in-place truncation, never rename, so each
  segment is a complete independently-verifiable chain.
- **/review found 6 CRITICALs across three rounds, and the third round found one in the fixes.** A boot
  panic (`hex_decode` byte-sliced a `&str`; `panic = "abort"` + PID 1), an unbounded `seq` from an
  unverified receipt, a rotation cascade that unlinked the live inode while `write_receipt` returned Ok,
  a **false-green test of mine** that never entered the code it claimed to guard, and — in the fix itself —
  a fallible unlink placed after the live rename that reintroduced the same symptom.
- **Honesty is the point.** `THREAT_MODEL.md` §8.7.1 now states the three real limits: coverage is model
  calls only; the signer **is** the audited party (self-attestation, not third-party evidence); and
  deletion/rotation seams are undetectable from the chain alone. `ROADMAP.md`'s "Provable accountability"
  and `PRODUCT-THESIS.md`'s "action receipts" are corrected, and the **mv external gate date is now named**
  (earlier of mv.3 or 2026-10-01) — it had never been recorded despite being that doc's one assigned action.

**Prev:** ux.13-TUI (v0.115.0) — **row-scoped control verbs in `agentctl watch`**. ux.13 had
shipped Cancel/SetBudget/SetCaps end to end with **no view invoking them** (`ROADMAP.md` said "TUI keys
deferred"), so the operator could not stop a runaway from the screen showing it. `[x]` opens a graded
row-action overlay (Park / Set budget / Cancel), `?` is the first help key this cockpit ever had, and the
measured footer clip (162 → ≤114 cols, with the narrow variant bounded at 80) is fixed. Was ux.3b — the
`:` palette is **STRUCK** (6/6 adverse CEO consensus; lazygit/htop answer this shape with `?`, and k9s
only needs `:` because its noun space is runtime-discovered). Plan: `docs/plans/ux.13-tui-verbs.md`.
- **Verbs run on the LOOP, never the key handler** (`App.pending_verb` + `drain_pending_verb`, placed
  after the shutdown check and before `event::poll`): `HttpSource`'s confirm client blocks up to 3 s, so a
  call during key dispatch froze the cockpit with no frame drawn. `handle_overlay_key` takes no `source`
  parameter, making it a compile-time property. The chat rail migrated onto the same slot (TODOS P2 → P3).
- **Park is guarded twice, and its LABEL carries the truth.** `park_limit()` → `None` below a 1 000-token
  floor (`0` ≡ UNLIMITED and `set_token_budget` writes the CHECKPOINTED config, so a zero-spend park
  un-capped the runaway permanently), and `park_would_widen()` blocks the normal post-exhaustion state
  (`windowed_spent > token_budget`, since the admission gate is pre-turn) where capping at the spend would
  RAISE the ceiling. New `budget_resettable` on snapshot + FUSE decides the wording, because with a window
  the park **self-expires at the next rollover** and without one it **ends the agent** — "reversible" was
  true of neither. **Both halves proven against a real agentd in /qa.**
- **/review (6 specialists + red team + a fix-review round) found 5 CRITICALs**, three in code this
  increment wrote and two in the review's own fixes: the Approvals dialog rendered by INDEX while acting on
  a pinned id (the approval gate IS the authority boundary); the footer clip was still shipping at 80 cols
  with a green test; a destructive verb could be armed below the overlay's size floor with nothing on
  screen; `park_would_widen` (above); and an appended cancel marker that regressed at the narrow widths the
  same commit had just claimed to support.
- **/qa drove the real TUI against a REAL agentd** (fake `/v1/messages` keeping agents genuinely alive via
  `ANTHROPIC_BASE_URL`), which is what proved semantics rather than frames: `budget_set` in the flight log,
  `running → deferred`, turns frozen 1699 → 1699; and with no reset window the parked agent really ends up
  `failed`. Found QA-1 — a cancelled row read a bare red `failed` with nothing attributing it to the
  operator (now `⨯ cancelled by you`).

**Prev:** ux.10 sub-part A (v0.114.0) — the `[l]` Logs view, completing ux.10. Its worst defect (90% of a
log burst dropped) was invisible to 1 689 passing tests and only appeared when the real binary was driven.

**Prev:** ux.10 sub-part B (v0.113.0) — real input widgets (`tui-input`/`tui-textarea` across the 5
hand-rolled inputs; single ratatui 0.29 held by exact pins; `step_key` threads the full `KeyEvent`).
Sub-part C (color-eyre) STRUCK at the /autoplan gate as redundant (`TermGuard` already restores on panic).
Before that, ux.3 (v0.112.0) — spawn custom agents on the fly over HTTP (p7.3-ar-02 cluster); CLI-subcommand
exec stays a P3 residual.

**UX tail:** ux.2b (v0.111.0) idle+error attention (closes cos-ux-01) → ux.3 (v0.112.0) → ux.10-B
(v0.113.0) → **ux.10-A (v0.114.0) — tail complete.** (Tags are a manual gate, but the tail IS tagged:
v0.113.0, v0.114.0 and v0.115.0 are all pushed. This line previously claimed "none tagged past
v0.113.0", which was stale and misled a session into repeating it — check `git ls-remote --tags`, not
this file.)

**AUDIT-v0.97 remediation — COMPLETE** (sweep + tail, v0.98.0→v0.109.0). Full audit: `docs/AUDIT-v0.97.md`.
Every increment ran plan→build→review→qa→ship; a holistic cross-model /review + per-increment /autoplan
reshaped scope in both directions (killed 2 over-scoped refactors, upgraded 1, struck 1 audit item as
a data-loss do-not-do).
- **Sweep stack** (v0.98–0.103): audit.2 (arm64 python, checkpoint `.restored`, ux.13 resurrection),
  run.1 (durability: flight rotation, `short_term` cap, cron catch-up, runs retention), cap.4
  (auth-consistency: whole-surface :7999 gate + deny-by-default `/spawn` + tool_override KB scoping),
  ci.2 (test blind-spots), budget.1 (metering completeness — universal spend folded into the global
  window + MaxTokens self-brick guard + universal-cancel), par.1 (drift guards). The holistic /review
  then fixed 2 escaped defects (FsRead `/spawn` exfil → privileged; checkpoint corrupt-primary →
  `.restored` fallback).
- **par.2** — config-unification RESHAPED to docs-only (`${VAR}` expansion can't express the deliberate +
  test-pinned structural Docker/QEMU config divergence).
- **hardening.1** (v0.104.0) — test+safety batch + unbroke `main`'s docker-smoke (ci.2 in-image oauth
  fixture escaped bug).
- **Behavioral:** par.1-ar-01 (v0.105.0) operator error view surfaces real tool/inference errors;
  budget.1-ar-01 (v0.106.0) MaxTokens truncation role-gate — one-shot fails, resident still parks (P0-2
  preserved) via a new `AgentEffect::CompletedTruncated`.
- **Design cluster:** cap.3 (v0.107.0) FS-capability matching anchored to startup CWD + closed a p5.8
  boot containment hole; budget.1-ar-02 (v0.108.0, P-doc) universal soft-cap documented honestly
  (reservation deferred — dormant path, single-tenant spend guardrail).
- **P3 tail:** p3.1 (v0.109.0) scheduler never aborts on a missing-agent effect + orphaned-checkpoint-tmp
  sweep. (audit86-P3-4 struck: bumping FORMAT_VERSION would cause rollback data-loss.)

**Audit tail — effectively closed.** par.3 (`agent)`-mode sed retirement) **DEFERRED at its /autoplan
premise gate** (2026-07-26): both CEO voices ranked it below the UX tail with zero user value (Codex STRIKE);
the guard "blind spot" that might have justified a cheap hardening was code-verified as overstated
(`entrypoint.sh:369`'s audit.1 ERE already fails the boot closed on a surviving installed-absolute path).
The working sed stays; revisit only as a build-time generator if it ever matters (`docs/plans/par.3-*.md`).
Only residual: port-7999 shared constant (trivial low-value config dedup).

**Next (roadmap):** **brief.2 is NOT the automatic next step** — gate it on the one-week operator
tally (see brief.1 above). Both CEO voices ranked the whole brief track BELOW three open items:
(1) name mv design partners or strike mv (external gate 2026-10-01, needs 10 named humans + 3 booked
demos, has zero of each, zero engineering); (2) `p7.7-ar-03` (~half a day — `HttpSource` hardcodes
`egress_brokered`/`egress_rejected` to 0, so the cockpit reports a false `0 denied` now that ux.6a
made denials real); (3) `audit-S3` (P1, no `SecretRewriter`). Also newly P1: **`brief-03`** —
sender-written markdown reaches the operator's brief and escaping it is a prompt rule, not
enforcement; the real fix (runtime-authored brief markdown from the typed `BriefRecord`) shares a
landing zone with `brief-04` and the two are probably one increment.
ux.3b is CLOSED as ux.13-TUI (the palette struck). Next per the plan's CEO
sequencing: **ux.6 evidence** (the only queue item serving two products — cockpit + mv governance, EU AI
Act Art.12), then evidence-gated ux.5/ux.7; Phase 11 skills + Phase 9 eBPF remain the two end-of-queue
tracks. Also open: **audit86-P1-9** needs a standalone 20-minute scope decision (are inert wrong-tier
capability grants the intended declare-then-lint design, or a gap?) — now the only live P1 in `TODOS.md`.
Residuals: port-7999 shared constant (trivial), agentctl `spawn` CLI-subcommand exec (P3), SetCaps has no
TUI (no snapshot data behind it, and it REPLACES the whole set), and the four P3s ux.13-TUI's reviews
opened (blocking verb on the loop thread, Park's rollover deadline, an HTTP route for `[d]`,
`pending_focus` stickiness).

Full per-increment completion notes: `docs/STATUS.md`.

## How to work here

- **Work the roadmap in order.** Each increment in `docs/ROADMAP.md` is a small,
  self-contained unit of work with explicit dependencies and acceptance criteria.
  Implement exactly one per branch; do not bundle several together. `main` stays
  shippable at every step. The roadmap's "How to use this with gstack" section
  describes the per-increment loop (`/plan-eng-review` or `/autoplan` → build →
  `/review` → `/qa` → `/ship`).
- **Preserve behavior across refactors.** Phase 1 begins by refactoring the loop
  into a steppable state machine; the single-agent path must keep working
  identically (the flight-recorder output for the demo should not regress).
- **Build, lint, and test before every commit — workspace-wide, from the repo
  root:** `cargo build --workspace && cargo clippy --workspace --all-targets --
  -D warnings && cargo test --workspace`. CI enforces exactly this across all
  five crates (ci.1) — per-crate commands from `agentd/` miss
  surfaces/sandbox/otel lints and go red in CI. (First workspace run rebuilds
  into the root `target/` — one-time cost.) Do not commit code that does not
  compile or that has clippy warnings.
- **Every version bump updates the "Current version" line in this file.** The
  line at the top of "Current status" is test-enforced against
  `agentd/Cargo.toml` (`agentd/tests/repo_consistency.rs`) — a release commit
  that bumps Cargo.toml without updating CLAUDE.md fails CI.
- **Linux-gated code requires a Linux clippy pass before pushing.** Any code
  under `#[cfg(target_os = "linux")]` (e.g. `surfaces/src/agents_fs.rs`) is
  never compiled on macOS, so local clippy is a false green. Run
  `make clippy-linux` from the repo root (requires Docker) before pushing a
  branch that touches Linux-gated code. This mirrors the CI step exactly.
- **aarch64-gated code requires an aarch64 clippy pass before pushing.** Any code
  under `#[cfg(target_arch = "x86_64")]` or `#[cfg(not(target_arch = "x86_64"))]`
  (e.g. `sandbox/src/lib.rs` DenySpawn gate) has different behavior on aarch64.
  Run `make clippy-aarch64` from the repo root (requires Docker and `cross` installed
  via `cargo install cross --locked`) before pushing a branch that changes
  arch-conditional behavior. `Cross.toml` at the repo root pins the Docker image
  version so `ring`'s `build.rs` gets the correct `aarch64-linux-musl-gcc`.
- **Run the TUI, don't just test it.** `agentctl watch` is a ratatui TUI: it needs a
  real pty AND a window size, so piping into it renders an empty frame that makes
  every assertion pass vacuously. The project skill
  `.claude/skills/run-agentctl-watch/` is the verified path — a stdlib pty driver
  (`driver.py`) that sends keys and captures readable frames, plus a fake `docker`
  that reproduces the compose CLI-plugin fork so the `[l]` Logs view and its
  process teardown can be exercised with no daemon. Use it for any `watch` change;
  ux.10-A's worst defect (90% of a log burst dropped) was invisible to 1 689
  passing tests and only appeared when the real binary was driven.
- **Match the existing style.** Small modules, narrow traits, minimal
  dependencies. This is meant to be a *light* runtime — justify every new crate.
- Update `docs/ROADMAP.md` (check off the increment) and any affected doc in the
  same PR as the code.

## Invariants you must preserve

- **Record everything.** Every meaningful step an agent takes emits a structured
  flight-recorder event. New behavior gets new event kinds (see the taxonomy in
  `docs/CONVENTIONS.md`). Logging is best-effort and must never crash an agent.
- **Cognition is metered.** Token/$ usage is always accounted and bounded. New
  scheduling never removes the budget guard; it builds on it.
- **Secrets come from the environment, never config or code.** `ANTHROPIC_API_KEY`
  and friends are read from env. Never log a secret. Never write one to disk.
- **Tools go behind the `Tool` trait.** Anything an agent does to the world is a
  `Tool`. **MCP is the tool ABI** — prefer exposing capabilities as MCP servers;
  native tools exist only for zero-dependency convenience.
- **The loop never panics on bad input.** Provider/tool/parse failures become
  recorded errors and `Result`, not panics.

## gstack

Use `/browse` from gstack for all web browsing. **Never use `mcp__claude-in-chrome__*` tools.**

Available skills: `/office-hours`, `/plan-ceo-review`, `/plan-eng-review`, `/plan-design-review`, `/design-consultation`, `/design-shotgun`, `/design-html`, `/review`, `/ship`, `/land-and-deploy`, `/canary`, `/benchmark`, `/browse`, `/connect-chrome`, `/qa`, `/qa-only`, `/design-review`, `/setup-browser-cookies`, `/setup-deploy`, `/setup-gbrain`, `/retro`, `/investigate`, `/document-release`, `/document-generate`, `/codex`, `/cso`, `/autoplan`, `/plan-devex-review`, `/devex-review`, `/careful`, `/freeze`, `/guard`, `/unfreeze`, `/gstack-upgrade`, `/learn`.

## Commands

Runtime code lives in `agentd/`; run agents from there. The pre-commit quality
gate is workspace-wide and runs from the **repo root** (see "How to work here").

```bash
cd agentd

# Build
cargo build                      # debug
cargo build --release            # ~2 MB size-optimized binary

# Quality gate (run before committing) — from the REPO ROOT, not agentd/
(cd .. && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace)

# Run an agent (logs to stderr; final answer to stdout; events to flight.jsonl)
export ANTHROPIC_API_KEY=sk-...
cargo run -- agent.toml          # single agent
cargo run -- agents.toml         # multiple agents concurrently (p1.2+)
tail -f flight.jsonl             # watch it think
```

No OpenSSL dependency since p2.1 (`rustls-tls`). For a static musl build:
```bash
# requires `cross` (cargo install cross) and Docker
cross build --target x86_64-unknown-linux-musl --release
```

## Repo layout

```
agentos/                   the repo root (run `claude` here)
  CLAUDE.md                this file
  README.md                project overview
  CHANGELOG.md             notable changes per release
  TODOS.md                 open technical-debt items and completed increments
  docs/
    DESIGN.md              full design & research (the "why")
    ROADMAP.md             the staged build plan (the work queue)
    STATUS.md              detailed per-increment completion notes (this file's old log)
    CONVENTIONS.md         how to extend the codebase consistently
    SPIKES/                exploratory spike docs (implementation notes per increment)
  agentd/                  the runtime (Rust crate)
    Cargo.toml             manifest
    agent.toml             single-agent example spec
    agents.toml            multi-agent example spec (p1.2+)
    README.md              runtime-specific quickstart
    src/
      main.rs              boot: load config -> wire gateway + tools -> run scheduler
      config.rs            TOML agent spec (single [agent] + multi [[agents]] forms)
      flight_recorder.rs   append-only JSONL event log
      scheduler.rs         cooperative multi-agent scheduler (p1.2+)
      agent/
        mod.rs             AgentTask state machine: step() → AgentEffect (p1.1+)
        driver.rs          single-agent backward-compat shim
      inference/
        mod.rs             InferenceGateway trait + neutral message/tool types
        anthropic.rs       remote backend (Anthropic Messages API)
      tools/
        mod.rs             Tool trait + registry
        native.rs          built-in read_file / write_file / list_dir
        mcp.rs             real MCP stdio client -> tools
  templates/               Phase 6: agent template catalogue (p6.1+)
    scout.template.toml    read-only researcher; first catalogue entry
  surfaces/                Phase 3: system surfaces (p3.1+)
    Cargo.toml             manifest (fuser dep Linux-only)
    src/
      lib.rs               re-exports snapshot types + agents_fs module
      snapshot.rs          SchedulerSnapshot / AgentSnapshot / AgentStatus
      agents_fs.rs         AgentsFs FUSE handler + mount() (Linux); stub (others)
  sandbox/                 Phase 3: kernel sandbox for MCP subprocesses (p3.3+)
    Cargo.toml             manifest (Linux-only raw syscall dependencies)
    src/
      lib.rs               SandboxRule enum + CompiledSandbox + compile()/apply_compiled()
  distro/                  Phase 2: Buildroot external tree + QEMU boot
    Makefile               build / run / test / prereqs / clean
    buildroot.config       Buildroot defconfig (x86_64 musl, busybox, cpio.gz)
    kernel-extras.config   kernel fragment: virtio-net + virtio-9p + FUSE + SECCOMP
    overlay/
      init                 /init PID-1 sh script
      agents/              mount point for /agents FUSE filesystem (p3.1)
      usr/bin/agentd       (gitignored; copied by make build)
      etc/
        resolv.conf        nameserver 10.0.2.3 (QEMU SLIRP DNS)
        agentd/
          agent.toml       demo agent config
```

Phase 6 adds further siblings: `agentctl/` (p6.2 operator CLI), more templates (p6.7 starter catalogue).

`agentctl/` layout (p6.2+):

```
agentctl/                operator CLI binary
  src/
    main.rs              arg dispatch
    list.rs              list-templates subcommand (p6.2)
    spawn.rs             spawn <template> subcommand (p6.2)
    inject.rs            inject <id> <text> subcommand (p7.3+)
    orchestrate.rs       orchestrate REPL — spawn + multi-turn SSE loop (orch.1+)
    watch/
      mod.rs             watch entry point; run_plain / run_tui
      app.rs             App state machine + View enum
      reader.rs          reads /agents/ FUSE files → AgentInfo
      views.rs           ratatui render functions
      topology.rs        TopologyGraph + build_graph() + render_tree() (p6.4)
```

`agentd/coordinator-demo.agents.toml` — multi-agent fixture for topology testing (coordinator + 2 scouts).

When in doubt about *what* to build next, the roadmap decides. When in doubt
about *how*, conventions decide. When in doubt about *why*, the design doc decides.

## Skill routing

When the user's request matches an available skill, invoke it via the Skill tool. When in doubt, invoke the skill.

Key routing rules:
- Product ideas/brainstorming → invoke /office-hours
- Strategy/scope → invoke /plan-ceo-review
- Architecture → invoke /plan-eng-review
- Design system/plan review → invoke /design-consultation or /plan-design-review
- Full review pipeline → invoke /autoplan
- Bugs/errors → invoke /investigate
- QA/testing site behavior → invoke /qa or /qa-only
- Code review/diff check → invoke /review
- Visual polish → invoke /design-review
- Ship/deploy/PR → invoke /ship or /land-and-deploy
- Save progress → invoke /context-save
- Resume context → invoke /context-restore
- Author a backlog-ready spec/issue → invoke /spec
