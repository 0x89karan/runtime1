<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/ci.1-autoplan-restore-20260717-220957.md -->
# ci.1 — CI tests the artifact

**Source:** `docs/AUDIT-v0.86.md` §6 (build order; audit finding S4 "CI validates
components, not the artifact"), ratified decisions §9 (D1 Docker-first).
**Branch:** `ci.1`. **Depends on:** audit.1 (✓ shipped v0.87.0 — the DRY_RUN hooks
and parse-all test this increment wires into CI).

## Problem

CI today tests two of five workspace crates, never runs the Python sidecars' self-
tests, never boots or even builds the Docker image on PRs, and lets any `v*` tag on
any commit publish `:latest` (the v0.86.0 staleness incident). The entrypoint
rewrite pipeline that produced the v0.86.2 bug has dry-run hooks (audit.1) but no CI
job invoking them. The audit's verdict: the last three production bugs lived at
exactly the cross-file seams CI doesn't cover.

## Scope

### 1. Workspace-wide Rust CI (audit86-P1-4)

Replace the per-crate build/clippy/test steps in `build-and-test` with workspace
invocations from the repo root:
- `cargo build --workspace --verbose`, `cargo clippy --workspace --all-targets --
  -D warnings`, `cargo test --workspace --verbose`.
- `surfaces` pins `fuser 0.14 default-features=false` (pure-Rust path, no
  pkg-config) and ALREADY compiles on the ubuntu runner (agentd depends on it) —
  so no apt packages are expected; verify empirically on the first CI run and add
  `libfuse-dev pkg-config` only if the build proves otherwise. What's new in CI is
  running surfaces' 96 TESTS (they run on macOS locally by design —
  `#[cfg(any(test, target_os = "linux"))]`) and, more valuable, clippy over the
  Linux-gated fuser glue (the false-green `make clippy-linux` exists for).
- Brings `sandbox` (34 tests) and `otel` (34 tests incl. the event-kind coverage
  guard) into CI.
- Keep the release/musl builds and 6 MB size guards unchanged (they are per-crate
  deliberately — musl artifacts and sizes are per-binary).
- **Newly-linted crates must pass `-D warnings` — known failure to fix in this
  increment:** `sandbox/src/lib.rs:1036` `assert_eq!(s.seccomp, true, ...)` trips
  `clippy::bool-assert-comparison` → `assert!`. Sweep the other never-linted
  crates the same way on the first local workspace-clippy run.
- **Runner-dependency honesty:** sandbox/agentd tests exercise Landlock/seccomp in
  subprocesses; fine on current ubuntu-24.04 runners (agentd's integration tests
  already run there today), but it IS a kernel dependency — noted, not hidden.
- rust-cache (eng F9 — today's config caches NOTHING: workspace builds write to
  root `target/` while the action caches `agentd/target`+`agentctl/target`, which
  never exist; every current CI run rebuilds from scratch): switch build-and-test
  to the root workspace entry (`.`) — this is a fix, not just a tweak. Mirror the
  same correction into release.yml's rust-cache. The new multi-GB root target
  cache shares the 10 GB quota with buildkit-core/full + Buildroot — watch
  eviction on the first weeks; `cache-all-crates`/prune settings if it thrashes.
  aarch64 job keeps its keyed entry (different target).

### 2. Python sidecar self-tests (audit86 P2)

New fast job `sidecar-tests` (ubuntu, no deps beyond python3 — sidecars are
stdlib-only). **Invariant (corrected twice in review): exit codes are NOT the contract —
`weather_mcp.py --test` with CI's /dev/null stdin EOFs instantly and FALSELY
PASSES rc 0 (empirically verified), so a flagless sidecar is invisible to rc
checks.** The job asserts the success MARKER per file: every existing self-test
prints `<name>: self-test PASSED` (verified in all 8); the loop requires
`--test 2>&1 | grep -q "self-test PASSED"` with a per-file timeout. Give
`weather_mcp.py` a minimal marker-printing `--test` in this increment so every
glob match passes the marker check (no allowlist, no count thresholds).
`semantic_kb_mcp.py` runs with `MOCK_EMBEDDINGS=1`.

### 3. Docker image build + DRY_RUN boot job (ux.9-ar-05 + audit86-P1-5 remainder)

New job `docker-smoke`, `if: github.event_name == 'pull_request' ||
(github.event_name == 'push' && github.ref == 'refs/heads/main')` — this excludes
feature-branch pushes (double-build) AND tag pushes (`on.push.tags` triggers the
workflow too). Build with `load: true` (build-push-action does not load
single-platform images into the daemon by default — the smokes need it).
**Cold-start honesty (eng F10):** PR caches read from main's scope, and main has
never written it — until the first post-merge main push populates the cache, PR
builds are COLD (~20-40 min for the alpine LTO release build, amd64). Expect that
on the first PRs; the <10 min budget applies from the second main push onward:
- Build `runtime-full` for linux/amd64 only. **Cache direction (corrected in
  review):** GHA caches are branch-scoped — PR runs can only READ main's cache,
  never warm the release cache. So: `cache-from: type=gha,scope=buildkit-full`
  with NO cache-to on PRs; only main pushes write the shared scope (protects the
  10 GB quota from PR churn evicting release layers).
- Positive smokes against the built image:
  - `docker run --rm -e DRY_RUN_ONLY=1 <image> cos` → exit 0, output contains
    `store_path = "/data/memory.redb"`, no quoted relative path.
  - `docker run --rm -e DRY_RUN_ONLY=1 -e ANTHROPIC_API_KEY=x -e
    TEMPLATE_NAME=scout -e AGENT_TASK=x <image> agent` → exit 0.
  - Binary probe: `docker run --rm -e ANTHROPIC_API_KEY=x <image> run
    /nonexistent.toml` → exits nonzero with agentd's config-not-found error (the
    dummy key is required — `run)` calls check_api_key BEFORE agentd, so without
    it the probe asserts the wrong error; agentd has no `--help`, and we do NOT
    add runtime surface inside a CI increment).
- **Negative control (the PR-#124 class, acceptance 3):** bind-mount
  `.github/fixtures/cos-broken-relative.toml` over **`/etc/agentd/cos.agents.toml`**
  (the exact path `cos)` reads — Dockerfile:67; mounting over agents.toml would
  false-pass). **Fixture contents matter (eng F4):** it must carry the
  `write_file(path='./output/…` literal so the pre-existing v0.86.2 prompt-vs-grant
  grep passes (else the boot fails with THAT unrelated error), plus the planted
  relative path no sed rule knows. Assert on `"relative path survived the boot
  rewrite"` + a line number in stderr.
- **CUT (cross-model F2/#2):** the two unexercised guard-branch exercises
  (grep rc>=2 injection, extra-pattern concat) are dropped — they test machinery
  par.2 is scheduled to delete; they die with the sed pipeline. Documented here so
  the audit.1 coverage note has a disposition.
- **Red-run ergonomics (DX):** every smoke/sidecar step captures output to a file
  and `cat`s it on failure (a bare `grep -q` red swallows the cause); failure paths
  emit `::error title=…::problem / likely cause / repro command` annotations;
  docker-smoke sets `timeout-minutes: 50` with a comment that <10 min is the
  warm-cache budget (a cold first PR must be slow-not-red, and GitHub's default
  360-min is not the issue — an aggressive 10-15 min setting would be).
- **Branch protection:** docker-smoke (and the workspace test job) added to
  required status checks — `pull_request` triggering alone does NOT gate merges.
  The add/remove `gh api` commands live in DEPLOYMENT.md's ops section (DX: a PR
  body is not durable; the flake policy makes removal/re-add a recurring op, and
  job renames silently un-gate — the doc records the load-bearing check names).

### 3b. Nightly artifact E2E — mock-provider agent cycle (UC1 ✅ APPROVED at gate, 2026-07-18)

The strongest cross-model finding: everything above still tests boot plumbing, not
the artifact DOING ITS JOB. Proposed (pending gate approval): a nightly
`schedule:` job that runs the full image with `ANTHROPIC_BASE_URL` pointed at a
tiny stdlib-python mock serving canned `/v1/messages` responses (override verified
at `inference/anthropic.rs:27`; zero API cost), runs a single-agent config to
completion, and asserts `flight.jsonl` contains `agent_completed` and no
`capability_denied`. **Mock shape (both voices, reconciled):** streaming defaults true and the SSE
parser bails on non-SSE bodies — so the job runs its OWN bind-mounted fixture
config (`run /fixtures/nightly.toml`, native tools only, `[model] streaming =
false`) and the mock serves one plain Anthropic-JSON response. (Claude's pass
verified an SSE mock is also feasible — Content-Type never inspected — but plain
JSON + fixture config is the explicit version; SSE coverage stays with
anthropic.rs's httpmock tests.) Mechanics verified: flight.jsonl lands in the
bind-mounted CWD; mock reachable via a second container on a user network or
host-gateway; FUSE absence is non-fatal (main.rs warns and continues) — no
--privileged. **Lands in its OWN workflow file (eng F13):** a `schedule:` trigger
inside ci.yml would run every un-`if`-guarded job nightly. This is the audit's "longer term compose-boot job asserting
flight.jsonl shape" pulled forward as the cheap 80% — it also gives S3
(residency) its first scheduled empirical signal. Broker→oauth→provider
fake-provider E2E stays out (named follow-up: ci.2 in TODOS).

### 4. Publish guards (audit86-P1-6 — the v0.86.0 staleness incident)

In `publish-docker` (and mirrored in `release.yml`):
- **One shared script, two callers (review fix — don't hand-mirror the guard into
  two workflows, the disease this repo keeps treating):** `scripts/release-guard.sh`
  invoked by both publish-docker and release.yml.
- **Ancestry guard:** resolve the tag to a commit first (`git rev-parse
  "$GITHUB_REF_NAME"^{commit}` — annotated tags make `GITHUB_SHA` the tag OBJECT),
  then `git merge-base --is-ancestor <commit> origin/main || fail`. Checkout with
  `fetch-depth: 0`.
- **Tag==Cargo guard:** strip `v` from `GITHUB_REF_NAME`, assert equality with the
  cargo-metadata version; fail before any build. Also in release.yml (artifacts are
  named from the tag while images come from Cargo.toml — divergible today).
- **Dispatch-path branch (eng F6 + DX correction):** the script cases on the ref —
  `refs/tags/v*` runs ancestry + tag==Cargo + monotonicity; **the REUSE guard is
  keyed on the Cargo VERSION and runs on BOTH ref cases** (dispatch also pushes
  `:v$CARGO_VERSION` tags — ci.yml:236,252 — so an unbumped main dispatch would
  silently overwrite a published version through the other door). Consequently the
  guard step runs AFTER the ghcr login step (the reuse check needs the token) —
  the earlier "before login" claim is corrected.
- **Monotonicity guard:** tag version must exceed the highest existing `v*` tag
  **excluding the tag being pushed** (with fetch-depth:0 the pushed tag is already
  in refs/tags — a naive max includes itself and fails every legitimate release):
  `git tag -l 'v*' | grep -vx "$GITHUB_REF_NAME" | sort -V | tail -1`.
  Enforce the strict 3-part `v[0-9]+.[0-9]+.[0-9]+` format first (regex; this repo
  uses no prereleases), then compare with `sort -V` — documented so nobody feeds it
  semver prerelease strings it doesn't handle. Bootstrap: pass when no prior tag.
- **Reuse guard (eng finding — deletion hole):** a deleted-then-recreated tag has no
  surviving ref for monotonicity to see. Before building, fail if the version was
  already published: `gh release view "v$V"` succeeds OR the ghcr manifest for
  `:v$V` exists (`docker manifest inspect`, token from the existing login step) →
  refuse. "Never republish a used version" beats "newer than visible tags".
- **release.yml structure (eng F11):** the guard runs as a separate FIRST job that
  both build-release and the distro job `needs:` — otherwise the GitHub Release is
  created before the second job's guard copy would run. Error text notes the
  tag-before-main-push race is fail-closed ("if the commit is on main, re-run").
- Honesty note: a bad tag still burns the build-and-test/aarch64 matrix (tags
  trigger the whole workflow); the guard saves the publish, not the CI minutes.
- Out of scope → sec.2 (already sequenced there): digest pinning, provenance/
  signing, post-push image verification.

### 5. overlay/init denylist sync (the golden-diff TEST moves to par.1)

audit.1 added `GREP_OPTIONS|POSIXLY_CORRECT` and
`AGENTOS_SKIP_PATH_GUARDS|DRY_RUN_ONLY` denylist lines to `docker/entrypoint.sh`;
`distro/overlay/init:50` still has the old list — live drift. **In ci.1: the
two-line sync of `overlay/init` only** (harmless in QEMU, defense-in-depth).
**Moved out (review fix — this was par.1's scope smuggled in):** the golden-diff
test family (case-pattern normalization, shell_mcp.py superset assertion) is
audit S2 Tier 2 = par.1 by the audit's own definition; it goes back there. The
sync closes the live hole; par.1 mechanizes recurrence.

### 6. qemu-boot.yml: MONTHLY cron (UC2 ✅ decided at gate, 2026-07-18) + workflow_dispatch

The audit offered automate-or-retire. D1 ratified "QEMU parse-checked in CI,
boot-tested quarterly" — that's automate on a quarterly `schedule:` cron plus the
existing `workflow_dispatch`. Likely bonus: the red runs plausibly failed on
P0-1 itself (the workflow boots the default overlay config whose `model_id` key
made agentd panic at parse — fixed in v0.87.0). Add the schedule, trigger one
manual run after merge to confirm green, and record the outcome in the PR.
If it's still red for a different reason, file the finding as a TODO rather than
blocking ci.1 (the workflow costs API credits and ~25 min).

**UC2 resolution: monthly cron** (inside the 60-day schedule-disable window;
~monthly bisect; + ANTHROPIC_API_KEY-missing preflight message; dispatch kept).
Original decision brief: **UC2 (both models, decided at gate):** a QUARTERLY cron is a dead gauge —
GitHub disables `schedule:` workflows after 60 days of repo inactivity, so a
~90-day cadence can silently self-disable; and a quarterly-red run has ~90 days of
commits to bisect. Both models recommend monthly cron OR gating the existing
2-boot test on minor-version release tags, with "the run is GREEN" (not "a run
was recorded") as the acceptance criterion. D1's text ("boot-tested quarterly")
is honored by either; the operator picks the instrument.

### 7. Local gate parity (DX, both voices — HIGH)

After item 1, CI enforces workspace-wide `-D warnings`; CLAUDE.md's documented gate
(`cargo build && cargo clippy -- -D warnings && cargo test` from `agentd/`) misses
surfaces/sandbox/otel/agentctl — a contributor following the doc exactly goes red
in CI. In scope: CLAUDE.md's "Build, lint, test" bullet becomes the workspace-wide
commands from repo root (with a note that the first run rebuilds into root
`target/`); `make test-harness` globs `docker/*_mcp.py` with the same marker check
+ `MOCK_EMBEDDINGS=1` as CI (today it runs 6 of 9 sidecars). DEPLOYMENT.md's
"Cutting a release image" section is rewritten with the four guards and a
remediation per refusal — including the reuse-refusal redo path (delete BOTH the
GitHub release AND the ghcr manifest), which must also appear in the guard's own
error text.

## Cutline + budget (review addition, F9/#7/#9)

The always-on CoS still self-bricks in ~1-2 days (audit86-P0-2, fixed next
increment) — ci.1 must not eat ux.8′'s runway. Budget: 2 days CC. The increment
core is items 1, 2, 4 + item 3's positive smokes + negative fixture + item 5's
two-line sync. If day 3 threatens: cut 3b (nightly E2E → TODOS as ci.2 seed),
cut item 6 (qemu cadence → TODOS), ship the core. CI-latency budget: docker-smoke
must stay under ~10 min on a warm cache (amd64-only, cache-from main); if it
flakes twice in a week, it drops from required checks pending a fix — a bypassed
red check is worse than none.

## Non-goals

- `agentctl` raw-string event-kind matching (audit86-P2-13) — Rust refactor/test,
  not CI plumbing; stays tracked in TODOS for its own small increment.
- Compose-boot job asserting flight.jsonl shape (audit's "longer term") — after
  ci.1's smoke proves stable.
- Multi-arch (arm64) PR builds — publish keeps building both; PR smoke is amd64.
- Coverage tooling/percent gates, third-party CI services.
- cargo-audit compile-from-source speedup (audit P3) — cheap add if trivial
  (`taiki-e/install-action: cargo-audit`), else defer.

## Acceptance criteria

1. A PR that breaks a `surfaces`/`sandbox`/`otel` test goes red — proven on this
   very PR (workflows run from the branch), with one deliberate local negative
   before push.
2. A `v*` tag on a commit that is not an ancestor of main refuses to publish in
   BOTH workflows; a tag whose version ≠ Cargo.toml, or a version already
   published (release or ghcr manifest exists), fails before any build.
3. The PR-#124 fixture (mounted over `/etc/agentd/cos.agents.toml`) fails
   docker-smoke with the surviving line named.
4. The sidecar job fails if ANY `docker/*_mcp.py` fails to print the
   `self-test PASSED` marker under `--test` (rc alone is provably insufficient —
   weather's EOF false-pass), and passes with all 9 green after weather gains its
   minimal self-test.
5. `overlay/init` denylist synced (2 lines); byte-identical `case` lines verified
   by eye in this PR — the mechanized golden-diff test lands in par.1.
6. qemu-boot.yml carries the UC2-decided cadence; the post-merge dispatch run is
   GREEN (red = fixing or explicit retirement becomes in-scope; a recorded-but-red
   run does NOT satisfy this).
6b. If UC1 approved: nightly E2E green on its first scheduled or dispatched run,
   flight.jsonl uploaded as an artifact.
7. Local==CI: CLAUDE.md gate bullet is workspace-wide; `make test-harness` runs all
   9 sidecars marker-checked; DEPLOYMENT.md documents all four guard refusals with
   remediations + the branch-protection add/remove commands.
7. Full local gate green; no Linux-gated code touched beyond `overlay/init`
   (shell, not Rust — no clippy-linux needed; workspace clippy on ubuntu CI now
   covers `surfaces` anyway).

---

# CEO Review (Phase 1, via /autoplan — SELECTIVE EXPANSION, 2026-07-18)

## What already exists (leverage map)

| Sub-problem | Existing asset reused |
|---|---|
| Dry-run boot verification | audit.1's `DRY_RUN_ONLY` hooks (cos credential-free; agent dummy-key) |
| Config parse coverage | `config_parse_all.rs` (runs in workspace tests automatically) |
| Image build caching | publish-docker's `buildkit-full` GHA scope (PRs read it) |
| Version truth source | cargo-metadata extraction already in publish-docker |
| Sidecar self-tests | 8 of 9 `docker/*_mcp.py` already implement `--test` |
| QEMU boot harness | qemu-boot.yml 2-boot continuity (needs cadence + green, not a rewrite) |
| Mock-provider hook | `ANTHROPIC_BASE_URL` override (`inference/anthropic.rs:27`) — enables 3b at $0 |

## NOT in scope (considered, deferred/rejected)

- Broker→oauth→provider fake-provider E2E → **ci.2** (named TODOS entry ships in this PR — the "after smoke proves stable" never-trigger is replaced with a name).
- Golden-diff denylist TEST → par.1 (audit S2 Tier 2's own definition); only the 2-line init sync ships here.
- Guard-branch exercises (grep rc>=2, extra-pattern concat) → CUT; die with the sed pipeline (par.2).
- Digest pinning / provenance / post-push verification → sec.2 (sequenced).
- arm64 PR builds; coverage-percent tooling; event-kind string matching (audit86-P2-13) → tracked, separate.
- cargo-audit binary install (`taiki-e/install-action`) — folded in ONLY if it's a one-line swap at build time; otherwise P3 stays.

## Dream state delta

CURRENT: CI proves 2/5 crates on one OS; artifacts publish unbuilt-unbooted; any tag
publishes `:latest` → THIS PLAN: all 5 crates + sidecars tested, image built+booted
on every PR, releases ancestry/version/monotonicity-guarded, (pending UC1) nightly
mock-provider agent cycle → 12-MONTH IDEAL: par.2 deletes the sed pipeline (guards
shrink), ci.2 covers the broker seam, sec.2 signs and pins what ships. Moves toward
it; the tourniquet-vs-structure boundary is now written down instead of implied.

## Sections 1–10 (CI-plumbing plan; findings folded above where mechanical)

**1 Architecture.** New job graph: `build-and-test(workspace) ─┬─ docker-smoke(PR/main)
├─ sidecar-tests ├─ [3b nightly E2E] └─ publish-docker→scripts/release-guard.sh←release.yml`.
Coupling introduced deliberately at ONE point (shared guard script) to avoid two-workflow
drift. SPOF: GHA cache quota (10 GB) — addressed by PR-no-cache-to. Rollback: revert the
workflow file; required-checks flip is operator-reversible. **Folded: F5 cache direction,
F8 shared script.**
**2 Error/rescue.** Every new job fails loud in the PR UI; guard script failures name the
violated invariant (ancestry/version/monotonicity) and the offending tag/commit; sidecar
loop prints the failing file; docker-smoke negative control asserts the guard's own error
text. GAP none — but note red-check bypass risk is a human behavior, mitigated by the
flake policy in the cutline. **0 critical gaps.**
**3 Security.** publish jobs keep `packages:write` scoped as today; guard script runs
before login (fails cheap, no token exposure); fixture TOML contains no secrets; nightly
E2E mock returns canned text (no key in repo — dummy env). GITHUB_TOKEN untouched.
**No issues found** beyond what sec.2 owns (signing/pinning).
**4 Data flow.** Tag push → resolve ^{commit} → ancestry → version == Cargo → monotonic →
build. Shadow paths: unfetchable main (fail closed), malformed tag (rev-parse fails →
fail closed), first-ever tag for monotonicity (bootstrap: allow if no prior v* exists).
**Folded: annotated-tag path.**
**5 Quality.** No hand-mirroring (shared script); sidecar invariant is
allowlist-empty-by-construction; workflow `if:` conditions commented with why. Premise
corrections from review baked in (fuser/apt, tests-run-locally nuance). **Folded: F7.**
**6 Tests.** The increment IS tests; its own negative controls: break-a-surfaces-test
(acceptance 1 — proven on the PR itself), fixture boot-fail (acceptance 3), sidecar
missing---test (weather fixed, then the invariant enforces), guard script unit-tested
via bash with fake refs in a temp repo (small, listed in eng test plan). **1 open → UC1.**
**7 Performance.** CI-latency budget stated (docker-smoke <10 min warm); workspace test
adds surfaces/sandbox/otel (~2-3 min); sidecar job <1 min; publish path +3 guard commands
(~seconds). Quota pressure addressed. **No issues.**
**8 Observability.** Red checks in PR UI = the product. Nightly E2E (if approved) emits
its flight.jsonl as a job artifact for post-mortems. qemu-boot outcome recorded in PR.
**9 Deploy/rollout.** Workflow changes take effect on the PR that carries them
(pull_request context runs the NEW workflow from the branch — so acceptance 1/3 are
provable on this very PR). Required-checks flip after merge (operator one-liner). No
migrations. Mixed state: none (workflows are per-ref).
**10 Trajectory.** Debt added: none new (guards live in one script; fixture is 10 lines).
Debt retired: untested-crates hole, unguarded publish, dead qemu cadence. Reversibility
5/5. The par.2 sunset (from audit.1) still governs the entrypoint guards this reuses.

## CEO Dual Voices — consensus

Recorded above the premise gate: 10 Codex findings, 10 Claude findings (F1–F10),
consensus table in conversation. Cross-model agreements applied as amendments
(items 1–5 rewrites, cutline); **UC1** (nightly mock-provider E2E in-or-out) and
**UC2** (qemu cadence: monthly cron vs release-tag-gated vs quarterly-as-ratified)
held for the final gate. No premise disputes survived verification (F7 corrections
adopted).

---

# Eng Review (Phase 3, via /autoplan — FULL_REVIEW, 2026-07-18)

## Step 0 verification highlights

Claude's pass ran the actual suites: workspace clippy `-D warnings` PASSES on macOS
(all 5 crates, --all-targets); surfaces 96 + sandbox 17 + otel 35 tests pass; all 9
sidecar `--test` runs executed (exposing weather's rc-0 false-pass). Codex found the
one known Linux clippy trap (`sandbox/src/lib.rs:1036` bool-assert-comparison).
No FUSE mounts, no privileges, no network in any newly-CI'd test (verified against
`agents_fs.rs:1416+` in-memory snapshots; `apply_compiled` no-ops on empty rules).
Residual risk: first Linux workspace-clippy run may surface more never-fatal
warnings — pre-push proof via `docker run rust:latest cargo clippy --workspace
--all-targets -- -D warnings` (slow, ~20 min; budgeted).

## Eng Dual Voices — consensus

```
  Dimension                        Claude       Codex        Consensus
  1. Architecture sound?           YES+13 fixes YES+8 fixes  CONFIRMED (fixes folded)
  2. Test coverage sufficient?     AMEND(F1,F4) AMEND(#2,#6) CONFIRMED after folds
  3. Performance risks addressed?  AMEND(F9,10) AMEND(#8)    CONFIRMED (cache/cold-start honesty)
  4. Security threats covered?     YES          YES          CONFIRMED (guards; rest→sec.2)
  5. Error paths handled?          AMEND(F3,F6) AMEND(#5)    CONFIRMED after folds
  6. Deployment risk manageable?   YES(F11)     YES          CONFIRMED (guard-first job)
```

Cross-model agreements: acceptance-criteria contradictions (F2↔#6), fixture mount
target (F4-adjacent↔#2), mock-can't-be-plain-JSON-on-default-config (F7↔#5,
reconciled: fixture config with streaming=false + JSON mock), monotonicity needs
edges (F5↔#3/#4, complementary — self-exclusion + reuse-guard + strict format).
Claude-only (verified, folded): F1 marker assertion, F3 probe key, F6 dispatch
branch, F9 rust-cache-caches-nothing discovery, F10 load:true + cold-start, F11
release.yml guard-first job, F13 separate nightly workflow. Codex-only (folded):
sandbox clippy trap, runner-kernel honesty. **Resolved tension:** "never republish
a used version" (Codex) vs "delete-and-retag is legitimate" (Claude) → both guards
ship; deleting BOTH the GitHub release and the ghcr manifest is the explicit
operator path to redo a version.

## Failure modes (eng registry)

| CODEPATH | FAILURE | CAUGHT BY | USER SEES |
|---|---|---|---|
| workspace clippy | newly-linted warning | this PR's CI run | red check, warning text |
| sidecar loop | missing/false-pass --test | marker grep | red job, file named |
| docker-smoke build | cold cache overrun | timeout budget noted | slow-not-red first PRs |
| smoke negative | fixture trips wrong guard | F4 fixture spec | asserted message match |
| release guard | off-main/mismatch/reused tag | guard-first job | named invariant, no build |
| guard on dispatch | ref=main | F6 case branch | guards skipped, note printed |
| nightly E2E (UC1) | mock shape drift | JSON+streaming=false fixture | red nightly, flight.jsonl artifact |

**0 critical gaps** (every failure is loud and attributed).

## Parallelization

Lane A: ci.yml workspace changes + sandbox clippy fix. Lane B: sidecar job +
weather --test. Lane C: docker-smoke + fixtures. Lane D: release-guard script +
two workflow wirings. Lane E: init sync + qemu cadence. A-E disjoint files except
ci.yml (A/B/C touch it — sequential within one session). Practically: one CC
session, order A→D→B→C→E (+3b if approved); est. 1.5-2 days incl. CI iteration.

---

# DX Review (Phase 3.5, via /autoplan — DX POLISH, 2026-07-18)

Persona: the solo contributor whose PRs face the new checks + the operator reading
release refusals. TTHW n/a; the moved metric is red-check-to-understanding.

## DX Dual Voices — consensus

```
  Dimension                      Claude      Codex      Consensus
  1. Red-check ergonomics        AMEND(#3)   AMEND(#2)  CONFIRMED after folds (output
                                                        capture, ::error, timeout-minutes)
  2. Local gate == CI            NO (#1)     NO (#1)    AGREE-HIGH → scope item 7
  3. Release refusal exits       AMEND(#4)   AMEND(#3)  CONFIRMED (DEPLOYMENT refusal
                                                        table + redo path in error text)
  4. Docs findable/durable       NO (#5)     NO (#4)    AGREE (gh api cmds → DEPLOYMENT)
  5. Friction honesty            AMEND       OK         cold-start + eviction sentences
  6. Guard coherence             NO (#2)     —          Claude-only HIGH: dispatch
                                                        republish hole + auth ordering — folded
```

Scorecard (scoped): red-check ergonomics 9/10 after folds · local-gate parity 9/10
(item 7) · release ergonomics 9/10 (refusal table) · docs durability 9/10 · friction
honesty 8/10 (eviction nondeterminism stated) · overall **8.8/10**. Codex gate
inputs: UC1 in as non-required nightly; UC2 monthly > release-gated (smaller bisect
window, no release-day latency; add ANTHROPIC_API_KEY-missing preflight message).

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail (compact — full reasoning in the phase sections above)

| # | Phase | Decision (auto, principle) | Rejected |
|---|---|---|---|
| 1 | CEO | Cut guard-branch exercises — die with par.2 (P3/P5, cross-model) | keep gold-plating |
| 2 | CEO | Golden-diff TEST → par.1; 2-line init sync stays (P3, cross-model) | smuggled scope |
| 3 | CEO | Sidecar invariant replaces count-threshold (P1, cross-model) | ≥8 count |
| 4 | CEO | Guards: shared script, tag^{commit}, monotonicity, sec.2 boundary (P1/P5) | per-workflow copies |
| 5 | CEO | Cache direction corrected: PRs read-only (P5, fact) | PR cache-to |
| 6 | CEO | Premise fixes: fuser/apt, tests-run-locally, branch-protection reality (P5) | stale premises |
| 7 | CEO | Cutline + budget + flake policy added (P6) | unbounded scope |
| 8 | CEO | ci.2 named for broker E2E; "after smoke stable" never-trigger replaced (P6) | unnamed deferral |
| 9 | Eng | sandbox clippy fix in scope; sweep before push (P1) | discover-in-CI |
| 10 | Eng | Marker assertion (`self-test PASSED`) is the sidecar contract — rc provably insufficient (P1) | rc+timeout |
| 11 | Eng | Fixture carries write_file literal so the RIGHT guard trips (P1) | naive fixture |
| 12 | Eng | Probe with dummy key (check_api_key runs first) (P5) | keyless probe |
| 13 | Eng | Monotonicity self-excludes pushed tag; strict vX.Y.Z + sort -V (P5) | naive max |
| 14 | Eng | Guard branches on ref type; reuse check spans both paths post-login (P1, + DX) | tag-only guards |
| 15 | Eng | Nightly: own workflow file; fixture config streaming=false + JSON mock (P5, reconciled) | SSE mock / ci.yml schedule |
| 16 | Eng | rust-cache root fix in ci.yml AND release.yml (caches nothing today) (P1) | ci.yml only |
| 17 | Eng | load:true; cold-start honesty; timeout-minutes 50 (P5, + DX) | optimistic budget |
| 18 | DX | CLAUDE.md workspace gate + test-harness 9/9 (scope item 7) (P1, cross-model) | stale local gate |
| 19 | DX | DEPLOYMENT refusal table + redo path in error text (P1, cross-model) | prose-only exits |
| 20 | DX | Branch-protection commands in DEPLOYMENT, PR links (P5, cross-model) | PR-body-only |
| 21 | Gate | UC1 + UC2 → operator (below) | auto-deciding |


## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 1 | CLEAR (PLAN via /autoplan) | 16 proposals, 8 accepted, 4 deferred |
| Codex Review | `/codex` voices | Independent 2nd opinion | 3/3 ran | CLEAR | CEO 10 + Eng 8 + DX 5 findings, all folded |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | CLEAR (PLAN via /autoplan) | 21 issues folded, 0 critical gaps |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | SKIPPED | no UI scope |
| DX Review | `/plan-devex-review` | Developer experience gaps | 1 | CLEAR (via /autoplan) | score 6→8.8/10; local-gate parity + refusal table |

**CROSS-MODEL:** all three phases ran dual voices (Codex responsive 3/3 this run);
agreements folded as amendments; the one philosophical tension (never-republish vs
delete-and-retag) was reconciled into the double guard. Claude's eng pass executed
the suites; Codex caught the Linux clippy trap — complementary, zero open tension.

**VERDICT:** CEO + ENG + DX CLEARED — plan APPROVED at the final gate (2026-07-18);
UC1 approved (nightly mock-provider E2E, own workflow, non-required); UC2 = monthly
cron. Ready to implement.

NO UNRESOLVED DECISIONS
