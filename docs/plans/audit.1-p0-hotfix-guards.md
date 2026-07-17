<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/audit.1-autoplan-restore-20260717-182044.md -->
# audit.1 — P0 hotfix + guard batch

**Source:** `docs/AUDIT-v0.86.md` §6 (build order), §9 (ratified decisions, 2026-07-17).
**Branch:** `audit.1`. **Depends on:** nothing. **Ships:** this week.

## Problem

The v0.86.2 full-system audit found two P0s and a cluster of cheap, high-leverage
guards. This increment stops the bleeds that are hours-not-days to fix and kills the
bug *classes* (not just the instances) with tests that would have caught them.

Already landed in `a22a51c0` (not in scope here): TODOS delta (audit86-* entries +
re-ratings per D5), ROADMAP build-order header strike-throughs.

## Scope

### 1. P0-1 — fix the unbootable default QEMU config

`distro/overlay/etc/agentd/agent.toml:16` uses `model_id = "claude-haiku-4-5-20251001"`.
`ModelConfig` (`agentd/src/config.rs:503-509`) has field `model` and is
`#[serde(deny_unknown_fields)]` — the default QEMU boot panics PID-1 at config parse.
Fix: rename the key to `model`.

Also fix the sibling bug flagged in the audit P3 list: **`agentd/agent.toml:76-81`**
(verified location; the overlay file is 26 lines) — the comment example places
`capabilities` under `[tools]`, which is a parse error if followed (`ToolsConfig`,
`config.rs:546-556`, is `deny_unknown_fields` with no such field; `capabilities`
belongs on `[agent]`, `config.rs:466`). Move the example to the correct table.

### 2. P1-7 — config parse-all test (kills the P0-1 class)

New integration test `agentd/tests/config_parse_all.rs`:
- Glob `docker/*.toml`, `agentd/*.toml` (the example/fixture specs, not Cargo.toml),
  and `distro/overlay/etc/agentd/*.toml` relative to the workspace root.
- Each file must parse through the real `Config` loader (same entry point `main.rs` uses).
- Must fail with the offending file's path and the serde error in the message.
- Deliberately-broken TOML fixture test (negative control) proving the test catches
  the P0-1 class.
- Guard: test must fail loudly if a glob matches zero files (protects against the
  test silently passing after a directory move).
- Runs under `cargo test` from `agentd/`, so existing CI covers it with no workflow
  changes (full workspace CI is ci.1, not this increment).

### 3. P1-5 — entrypoint sed-rewrite negative-assertion guards

`docker/entrypoint.sh` rewrites dev-mode relative paths at container boot with sed.
Today's guard (`entrypoint.sh:159-164`) covers 1 of 6 rules in `cos)` and checks only
the prompt half of the v0.86.2 bug pair; the `agent)` case (`:231-238`) has no guard.

- `cos)`: after the rewrite, fail the boot if ANY relative path survives in
  `/data/cos.agents.toml` — the **general assertion** in BOTH quote styles
  (`"\.\./`, `"\./`, `'\.\./`, `'\./`), plus a **positive-form path-key assertion**
  replacing bare-filename literals: any of `^(store_path|evidence_path|key_path)\s*=\s*"[^/]`
  (a path-bearing key whose value doesn't start with `/`) fails the boot. The
  positive form is whitespace- and rename-robust — the eng pass showed the bare
  literals were byte-identical to the sed LHS (6-space `key_path` run), so a
  reformat would have broken sed and guard in lockstep. Error names the surviving
  line and points at the sed rules.
- The scan deliberately includes comment lines (documented choice): a future
  comment containing a quoted `./` path fails the boot loudly at dev time rather
  than being silently excluded — and `#`-prefix exclusion would false-negative on
  `task = """` content lines that start with `#`.
- `agent)`: general dot-slash assertion anchored to `^args[[:space:]]*=` lines
  (key-anchored, NOT the substring `/args/` — `AGENT_TASK` is user input and a task
  mentioning `args` plus a quoted `./` path must not brick boot), plus the two
  known literals (`"\.\./docker/`, `"/usr/lib/agentos/docker/`) as whole-file
  checks (nothing legitimate contains them post-rewrite). Placed BEFORE the
  `DRY_RUN_ONLY` early-exit so dry runs exercise it.
- **Behavior change, stated honestly:** an operator bind-mounting a custom
  `cos.agents.toml` containing quoted relative paths boots today (misconfigured but
  running) and refuses to boot after this change. That ratchet is intentional
  fail-loud; the error text must say what to change (use absolute paths in
  container configs).
- **Error-text spec (DX, one unified message — not fragments):** on guard trip,
  print: (1) the surviving line WITH line number (`grep -n`) and the file checked
  (`/data/cos.agents.toml`); (2) the problem ("relative path survived the boot
  rewrite"); (3) BOTH remediation branches — custom bind-mounted config → "use
  absolute paths in container configs"; baked config → "a sed rule in
  docker/entrypoint.sh drifted from the source TOML — add/fix the rule"; (4) the
  repro command (`docker run --rm -e DRY_RUN_ONLY=1 <image> cos`); (5) the escape
  hatch. All grep classes use POSIX `[[:space:]]`, never `\s` (GNU extension —
  silently matches nothing under busybox grep).
- **Escape hatch (DX):** `AGENTOS_SKIP_PATH_GUARDS=1` skips the new assertions
  (single-tenant, cheap, safe) and is named in the error text — a legitimate future
  quoted `./` in task prose must not permanently brick boot with no exit.
- **Discoverability (DX F-DX1):** `DRY_RUN_ONLY` exists only as a source comment
  today — add a "Verifying config changes" block to DEPLOYMENT.md's
  troubleshooting section: both dry-run invocations (cos credential-free; agent
  with dummy `ANTHROPIC_API_KEY=x` + `TEMPLATE_NAME`/`AGENT_TASK`), the edit →
  `make dev-image` → re-run loop, and expected output.
- **`cos)` gets its own 3-line `DRY_RUN_ONLY` hook** (dump rewritten config after
  guards, exit 0), placed after the guards and gated the same way as `agent)`'s.
  Without it, acceptance 3 rests on an unrepeatable full-flagship boot (needs
  OPENAI + Google creds); with it, verification is mechanical today and ci.1's boot
  job gets its hook for free. ALL preflights are skipped under DRY_RUN_ONLY —
  `check_api_key` (`:108`) included, not just the google.json/OPENAI checks.
  Canonical verification invocation (bypasses compose so `depends_on:
  semantic-kb-mcp → qdrant` doesn't drag two sidecars into a "credential-free"
  check): `docker run --rm -e DRY_RUN_ONLY=1 <image> cos`.
- Keep the existing prompt-vs-grant grep (it checks presence of the rewritten form;
  the new guards check absence of the source form — complementary).
- Known limitation, stated honestly: this tests the *source→rewrite* transform; the
  sed *output* is never parse-tested here (agentd itself parses it at boot; a
  parse-the-output CI check rides ci.1's DRY_RUN job).
- **Sunset trigger:** if par.2 (env-expansion, deletes this pipeline) has not shipped
  by the time cap.1 lands, re-decide par.2's priority explicitly rather than letting
  the guards' adequacy silently demote it.

### 4. Librarian-semantic template gate fix (live-broken template)

`templates/librarian-semantic.template.toml:12` gates on `VOYAGE_API_KEY` but its
sidecar (`docker/semantic_kb_mcp.py`) requires `OPENAI_API_KEY`:
- export VOYAGE only ⇒ template visible, every `kb_put` fails at runtime;
- export OPENAI only ⇒ working template hidden from `agentctl list-templates`.

Fix: `gated_requires = "OPENAI_API_KEY"`, and correct the stale "Voyage AI" wording in
`description` (line 10) and `[card].description` (line 24) — `showcases` carries no
Voyage mention — to OpenAI embeddings (text-embedding-3-small). Update
`agentd/src/template.rs:1081-1082` (asserts the old value).

**Doc sweep is real work, not minutes (eng M4 + DX F6/F7):** the sweep is
**grep-driven — every `Voyage|VOYAGE` occurrence in docs/**, not a line-range
checklist (the DX pass found 4 more hits beyond the enumerated ones, including two
self-test commands at MCP_SERVERS.md:55,473). Verified sidecar var names to write:
`OPENAI_API_KEY` / `EMBED_MODEL` / `MOCK_EMBEDDINGS` (`semantic_kb_mcp.py:76-79`).
And the rewritten invocations must be *verified runnable*, not just var-renamed:
MCP_SERVERS.md:426-428,466-467 say `docker compose --profile semantic up`, but
docker-compose.yml has no `profiles:` key (semantic-kb-mcp is "always-on for CoS")
— the profile flag errors today. Budget ~45 min.

**Same-edit coherence (DX F8):** CLAUDE.md's "Latest shipped:" line updates in the
same edit as the canonical line (else the paragraph self-contradicts after ship);
README.md:17's stale "(v0.66.0)" is a known, deliberately-deferred contradiction →
doc.1 (named here so "the class is killed" isn't over-claimed — the test enforces
CLAUDE.md's canonical line only).

**Reality check (eng phase, both voices):** `gated_requires` is catalogue *metadata*,
not a gate — `agentctl list-templates` always lists the template and only appends a
`[gated]` badge (`list.rs:44-45`); `agentctl spawn` prints it as a warning
(`spawn.rs:150,157`). Nothing checks any env var. The bug is a lying label, and the
fix is making the label truthful.

**Class-kill (cross-model finding, scoped to what's real):** a consistency test that
extracts env-var-shaped tokens (`[A-Z][A-Z0-9]*(_[A-Z0-9]+)+`) from every template's
`gated_requires` prose and asserts each token appears somewhere in the product
sources (`docker/` or `agentd/src/`). Verified feasible against the current
catalogue: OAUTH_CLIENT_ID/SECRET → `oauth_mcp.py`; TRIGGER_CRON → `cron_mcp.py`;
GITHUB_TOKEN → `credential/mod.rs`; OPENAI_API_KEY → `semantic_kb_mcp.py`;
VOYAGE_API_KEY → **nowhere** (test is red today, green after the fix — proves it
works). Prose-only gates ("Phase-5 memory…", "gVisor") carry no tokens and pass
vacuously — with one live exception the DX pass caught: `MCP_SERVERS` extracts from
"See docs/MCP_SERVERS.md §oauth_mcp" in cos-inbox/cos-orchestrator and greps zero.
**Mitigation, specced day one:** strip tokens immediately followed by `.md`
(doc-filename references), and ship an in-test `ALLOWLIST: &[&str]` const (empty
after the `.md` filter, but the mechanism exists for future prose). **Failure text
(DX F-DX3):** names the template file, the unresolved token, the searched roots
(`docker/`, `agentd/src/`), and both fix options (correct the var name to what the
sidecar reads, or reword the prose / extend the allowlist if the token isn't an
env var).

**Honest claim scope:** this makes the badge/warning truthful and the template
*usable under docker compose* (where `http://semantic-kb-mcp:8020` resolves). Host
dev mode and `docker run` outside compose remain broken (port 8020 not sed-rewritten
— audit P3, tracked); the template's comment block gains a line saying so. The
broker-routed embedding provider work is deferred (strategic direction 3, not this
increment).

### 5. TODOS reconciliation — verify then strike the 6 fixed-but-open entries

Per the reconciliation note at `TODOS.md:204-211`, verify each against code, then move
to Completed (or annotate why not):
- `audit-S1`, `audit-S2` (closed in cred.3.1 / v0.61.0 as cred.3-ar-S1/S2)
- `F-012` (fsync landed v0.70.0, `checkpoint.rs:36-65,187-204`)
- `F-015` (`extra_env` blocklist enforced at `mcp.rs:144`)
- `cred.3-ar-02` (superseded by cred.5 surface + cred.7 health state)
- `cred.3.1-adv-01` (`load_from_disk` startup reload + cred.7 checkpoint persistence)

### 6. Status-line reconcile + canonical version line (cred.3.2-ar-02)

- CLAUDE.md "Current status": fix "**Next:** ux.8 (budgets) or ux.3" → the post-audit
  order **ci.1 → ux.8′ → ux.10** (UC1 resolution at the final gate, 2026-07-17:
  operator swapped ux.10 and ux.8′ — budget truth ships before TUI polish); update
  the same order in ROADMAP.md's Track-UX line and AUDIT §9's confirmed-order line.
  "Latest shipped:" updates in the same edit.
- Implement cred.3.2-ar-02: add a single canonical
  `**Current version:** vX.Y.Z (shipped YYYY-MM-DD)` line at the top of CLAUDE.md's
  "Current status" section, updated on every merge; strike the TODOS entry.
- **Class-kill (cross-model finding — without this, the version line is the fourth
  recurrence of hand-maintained status rot):** a test (rides in the same new test
  file family) asserting CLAUDE.md's canonical version line equals the
  `agentd/Cargo.toml` package version (the machine-readable truth source), and that
  CHANGELOG.md contains an entry for that exact version. Runs under `cargo test` —
  no CI workflow change. A stale line now fails the build instead of quietly
  misleading every new session.
- **CHANGELOG ordering fix (found during eng verification):** the file is not
  newest-first today — `v0.86.2` (line ~125) and `v0.86.1` (line ~146) sit *below*
  `v0.86.0` (line 6). Reorder to newest-first while in the file. This is why
  Cargo.toml (`env!("CARGO_PKG_VERSION")`, compile-time), not "newest CHANGELOG
  entry", anchors the test.
- **Ship-workflow note (eng H3 + DX F9):** every future release bumps Cargo.toml,
  which makes this test fail unless CLAUDE.md is updated in the same commit — that
  is the point, but it must not ambush the shipper. TWO mitigations: (1) an HTML
  comment next to the canonical line (`<!-- updated on every release;
  test-enforced against agentd/Cargo.toml -->`); (2) a bullet in CLAUDE.md's
  "How to work here" section — the part release sessions actually follow —
  "Every version bump updates the Current version line in this file
  (test-enforced against agentd/Cargo.toml)."
- **Version-test failure text** names the exact CLAUDE.md line to edit and prints
  expected (Cargo) vs. actual (CLAUDE.md) versions.
- ROADMAP: check off audit.1 when shipped (same-PR doc rule); build-order header
  already reconciled in `a22a51c0`.

## Acceptance criteria

1. The default QEMU config (`distro/overlay/etc/agentd/agent.toml`) parses — proven
   by `config_parse_all` running in existing CI.
2. A deliberately broken TOML in any globbed directory fails `cargo test` with the
   file path in the failure message.
3. A deliberately broken sed rule fails container boot with the surviving line
   named in the error — verified mechanically via `DRY_RUN_ONLY=1` for both modes:
   `cos)` fully credential-free (`docker run --rm -e DRY_RUN_ONLY=1 <image> cos`);
   `agent)` with a dummy key (`docker run --rm -e DRY_RUN_ONLY=1 -e
   ANTHROPIC_API_KEY=x -e TEMPLATE_NAME=scout -e AGENT_TASK=x <image> agent` —
   `check_api_key` only tests non-empty, and the google.json preflight stays
   non-bypassed per the documented cred.1 decision). CI job wiring is ci.1.
4. librarian-semantic's `[gated]` badge, `agentctl spawn` warning, and template
   prose all name `OPENAI_API_KEY` — the var `semantic_kb_mcp.py` actually reads.
   (No env-based hiding exists or is added; gating is metadata + warning by design.
   Non-compose run modes remain tracked as audit P3.)
5. The 6 TODOS entries are struck (or annotated with why not) with per-item
   verification evidence in the PR description.
6. CLAUDE.md has the canonical version line, a truthful "Latest shipped:" +
   "Next:" pointer, and a test fails when the canonical line diverges from
   `agentd/Cargo.toml`'s version (plus: CHANGELOG must contain that version's
   entry). A release-checklist bullet in CLAUDE.md's "How to work here" section
   tells shippers the line is test-enforced.
6b. Every gated template's `gated_requires` var is proven read by its referenced
   sidecar script (consistency test), so the librarian-gate bug class cannot recur
   silently.
7. `cargo build && cargo clippy -- -D warnings && cargo test` green; no Linux-gated
   or arch-gated code touched (no Docker clippy passes needed).

## Implementation decisions (resolved at review time)

- **Parse path:** the test uses the same entry point as `main.rs:96-99` —
  `toml::from_str::<Config>` on each file, then `McpServerConfig::validate()`
  (`config.rs:652`, pure — no filesystem/env side effects) on every declared server.
  Boot-time filesystem checks (OV-1 `ensure!`s) are out of the test's reach by design.
- **No new crates:** directory walking via `std::fs::read_dir` + extension filter,
  not the `glob` crate. `Cargo.toml` excluded by filename (`file_name != "Cargo.toml"`),
  so future spec files are auto-covered.
- **Empty-glob guard:** each globbed directory must yield ≥1 spec file or the test
  fails — protects against silently passing after a directory move.
- **Negative-control fixtures** live in `agentd/tests/fixtures/` (outside all globbed
  dirs): one unparseable TOML (the P0-1 class), one that parses but fails
  `validate()` (plain-`http://` URL without `allow_insecure_local`).
- **Guard placement (`agent)` case):** the negative assertion runs after the sed
  rewrite and BEFORE the `DRY_RUN_ONLY` early-exit, so `DRY_RUN_ONLY=1` exercises the
  guard too.
- **Guard shape — general assertion, not enumeration:** quote-anchored dot-slash in
  BOTH quote styles — `"\.\./`, `"\./`, `'\.\./`, `'\./` — catches every current and
  future relative path (the single-quote anchors exist because
  `cos.agents.toml:375`'s `write_file(path='./output/…` is single-quoted; a
  double-quote-only guard would miss that whole class). Only the three bare relative
  filenames (`store_path = "memory\.redb"`, `evidence_path = "evidence\.jsonl"`,
  `key_path      = "egress-key\.pkcs8"`) need naming because they carry no
  dot-slash. Quote-anchoring means unquoted prose (e.g. the `agentctl verify
  evidence.jsonl` comment at `cos.agents.toml:46`) can't false-positive a boot. This
  avoids a third hand-maintained copy of the sed LHS list.
- **`cos)` DRY_RUN_ONLY bypasses ALL preflights** — not just the Google/OpenAI
  checks but `check_api_key` too (`entrypoint.sh:108` runs first today). The dry run
  tests the rewrite+guards and must need zero credentials, or acceptance 3's "no
  credentials needed" is false.
- **Parse path also exercises lowering:** after `from_str::<Config>` +
  per-server `validate()`, the test calls `cfg.agent_configs()` (`config.rs:349-362`,
  pure; boot calls it at `main.rs:119`) so both-forms conflicts ("[agent] and
  [[agents]] both set", "no agents without allow_empty_agents") are caught too.
  `cockpit.toml` passes via `allow_empty_agents`. If `agent_configs()` (or an
  adjacent pure check) rejects duplicate agent ids, add a dup-id negative fixture;
  verify at build time rather than assuming.
- **Glob base is `env!("CARGO_MANIFEST_DIR")`** (= `agentd/`) + `../docker`,
  `../distro/overlay/etc/agentd` — never CWD-relative (an integration test's CWD is
  cargo's choice, exactly the implicit coupling the empty-dir guard exists to catch).
- **Both config forms route through the same entry point:** `Config`
  (`config.rs:26`) deliberately accepts both `[agent]` and `[[agents]]` forms, so
  one `toml::from_str::<Config>` covers `agent.toml` and `*.agents.toml` files
  alike — no per-form branching in the test.
- **Sed-guard sunset (constraint):** this is the LAST sed-guard work. The next parity
  increment (par.2) must shrink or delete the sed pipeline via env-expanded config;
  no further guard rules get added to entrypoint.sh after this branch.
- **Scope framing (accuracy):** this increment stops the P0-1 bleed and the guard
  classes around it. P0-2 (budget self-brick) is deliberately sequenced to ux.8′ per
  ratified decision D2 — audit.1 does not claim to fix both P0s.

## Non-goals

- CI workflow changes (workspace-wide test, DRY_RUN boot job, publish guards) → ci.1.
- Budget semantics (P0-2) → ux.8′ per ratified decision D2.
- `agentd check` linter, tier-legality validation → cap.1.
- Spawn attenuation → cap.2.
- flight.jsonl rotation and residency work → run.1.
- Embedding provider neutrality / broker routing for OPENAI_API_KEY → direction 3.
- QEMU cos fork parity diff → par.1.
- Embedding-provider neutrality / routing `OPENAI_API_KEY` through the broker →
  strategic direction 3 (its own small increment; this branch only makes the gate
  truthful).

---

# CEO Review (Phase 1, via /autoplan — SELECTIVE EXPANSION, 2026-07-17)

## What already exists (leverage map)

| Sub-problem | Existing code reused |
|---|---|
| Config parse entry point | `main.rs:96-99` (`toml::from_str::<Config>`); test replicates the same 3 lines |
| Per-server validation | `McpServerConfig::validate()` `config.rs:652` — pure, callable from tests |
| Template catalogue parse coverage | `template.rs:913` `catalogue_all_templates_present` — templates stay OUT of the new glob (DRY) |
| Guard error style | `entrypoint.sh:155-164` existing prompt-vs-grant guard; new guards extend the same voice |
| DRY_RUN smoke hook | `entrypoint.sh:242-247` — reused for manual verification; CI wiring stays ci.1 |
| Gate assertion to flip | `template.rs:1081-1082` asserts `VOYAGE_API_KEY` — updated with the fix |

## NOT in scope (considered, deferred/rejected)

- Templates in the parse glob — duplicate of `template.rs:913` (rejected, DRY).
- Guards for `cockpit`/`orchestrate` modes — no sed rewrite there, nothing to guard (rejected).
- Extra TODOS strikes beyond the named 6 — needs per-item verification → doc.1 (deferred).
- qemu-boot.yml automation, workspace-wide CI, image smoke — ci.1 (deferred).
- Broker routing for the embedding key — direction 3 (deferred).
- Env-expansion single-source config — par.2, the structural kill (deferred; this branch is the tourniquet and the LAST sed-guard work).

## Dream state delta

CURRENT: 5 hand-mirrored TOML surfaces, 1-of-6 sed rules guarded, default QEMU config
unbootable, live-broken template gate → THIS PLAN: every checked-in TOML parse+validate
proven in `cargo test` (already in CI), both rewrite cases guarded with named-literal
errors, truthful template gate, canonical version line → 12-MONTH IDEAL: one
env-expanded config (par.2), `agentd check` (cap.1), CI boots the artifact (ci.1).
Verdict: moves toward the ideal; adds only deliberately-temporary guard debt.

## Architecture (Section 1)

```
                    ┌──────────────────────────────┐
                    │ agentd/tests/config_parse_all │  (NEW, test-only)
                    └───────┬───────────┬──────────┘
              read_dir ../docker/*.toml │ toml::from_str::<Config>
              ../distro/overlay/etc/agentd/*.toml
              agentd/*.toml (≠Cargo.toml)│
                    ▼                   ▼
        [10 checked-in spec files]   config.rs::Config ──► McpServerConfig::validate()
                                                            (existing, unchanged)
   docker/entrypoint.sh cos)/agent)  ──sed──►  /data/*.toml ──grep guards──► boot | exit 1
   templates/librarian-semantic.toml ──gate──► agentctl list-templates (env: OPENAI_API_KEY)
```

Data-flow shadow paths (parse-all test): missing dir → read_dir error → named fail;
zero files → empty-glob guard fail; unreadable/non-UTF-8 file → `read_to_string` error
with path; bad TOML → serde error with path. All four end in a loud, attributed test
failure. Coupling: test ↔ repo layout (guarded), guard literals ↔ sed LHS (inherent to
negative assertion; documented + sunset constraint). No new runtime coupling, no state
machines, no SPOF, no new endpoints. Rollback: pure `git revert`; guards only add
failure conditions at boot of NEWLY built images.

Production failure scenario honestly stated: a legitimate future edit to
`cos.agents.toml` path values makes a sed rule miss → container refuses to boot with
the surviving literal named → operator fixes the sed rule. That is designed behavior
(fail loud at boot beats silent capability_denied at runtime — the v0.86.2 lesson).

## Error & Rescue Registry (Section 2)

| CODEPATH | WHAT CAN GO WRONG | ERROR CLASS | RESCUED? | ACTION / USER SEES |
|---|---|---|---|---|
| config_parse_all: read_dir | dir missing/moved | `std::io::Error` | Y | test fails, dir named |
| config_parse_all: read file | non-UTF-8, unreadable | `std::io::Error` | Y | test fails, path named |
| config_parse_all: parse | unknown key (P0-1 class), type error | `toml::de::Error` | Y | test fails, path + serde msg |
| config_parse_all: validate | http URL w/o allow_insecure_local, url+command both set | `anyhow::Error` | Y | test fails, server + path named |
| entrypoint cos) guard | sed rule missed a literal | grep hit → exit 1 | Y | boot aborts; literal + sed pointer printed |
| entrypoint agent) guard | args path not rewritten | grep hit → exit 1 | Y | boot aborts; same style; DRY_RUN also exercises |
| librarian gate | OPENAI_API_KEY unset | gating (existing) | Y | template hidden from list-templates (existing UX) |
| librarian gate | VOYAGE set, OPENAI unset (old bug) | — | FIXED | was: visible but every kb_put fails; now: correctly hidden |

No catch-alls introduced; every failure is named, attributed, and loud. **0 GAPS.**

## Security (Section 3)

No new attack surface: no endpoints, no new deps (std-only test), no secrets touched
(gate names a public env-var name; value never logged). Inputs are repo-baked files.
Entrypoint guards run on container-local `/data` copies. Threat: none above baseline;
the increment strictly removes a silent-failure class. **No issues found** — examined
attack surface, input validation, secrets handling, dependency delta, injection
vectors; nothing qualifies.

## Data-flow & interaction edges (Section 4)

Covered by Section 1 shadow paths plus: `Cargo.toml` exclusion (filename filter);
symlinked TOML (read_to_string follows; fine); `.toml.example` files (not matched —
intended); zero user-visible interactions beyond boot error text. **No unhandled
edges.**

## Code quality (Section 5)

3-line parse duplication between test and `main.rs` accepted over a shared-helper
refactor (P5 explicit > clever; refactor would touch main for a test's benefit).
Guard-literal duplication vs sed LHS is inherent to negative assertion — documented
with sunset constraint. Naming (`config_parse_all`) matches existing test style.
Fix included: `agentd/agent.toml:76-81` comment showing `capabilities` under
`[tools]` (schema: `ToolsConfig` `config.rs:546-556` has no such field; it belongs on
`[agent]`, `config.rs:466`) — the comment moves to the correct table.

## Test review (Section 6)

NEW CODEPATHS → coverage: parse-all loop (self-testing; negative fixtures prove it
catches both classes) · entrypoint guards (manual: local `cos` boot + `DRY_RUN_ONLY=1
docker compose run agent`; CI wiring deliberately ci.1 — flagged as taste decision at
the gate) · gate flip (`template.rs:1081` assertion updated; catalogue gating tests
already exercise hidden/visible both ways). 2am-Friday test: `cargo test
config_parse_all` green + one DRY_RUN run. Hostile-QA test: the validate-failing
fixture. Flakiness: none (no time/network/randomness/ordering). LLM/prompt surfaces:
untouched. **1 open item → taste gate (CI wiring timing).**

## Performance (Section 7)

~10 small files parsed once per test run (<100 ms); +6 greps at container boot (ms).
**No issues found** — examined test runtime, boot latency, no queries/caches/jobs.

## Observability (Section 8)

Guard failures land on stderr → `docker logs` (the operator's existing surface); test
failures land in CI logs with file attribution. No flight-recorder events involved
(host-shell + test-only scope — recorder invariant untouched). Silent-denial
surfacing (`capability_denied` counters) deliberately cap.1. **No gaps in this scope.**

## Deployment (Section 9)

No migrations, no flags needed (changes are additive failure-conditions + data fixes).
Entrypoint/template changes reach users only via next image build (`make dev-image`
locally; `v*` tag publish). Mixed-version window: none — old images keep old behavior.
Bind-mount users: guards check only known source literals; custom configs without
those literals pass through unchanged (no regression; verified reasoning against
`entrypoint.sh:146-166`). Rollback: `git revert` + rebuild. Post-ship verification:
boot cos locally, run `DRY_RUN_ONLY=1` agent, `agentctl list-templates` with/without
`OPENAI_API_KEY`. **No risks beyond designed fail-loud behavior.**

## Long-term trajectory (Section 10)

Debt added: guard-literal lockstep (explicitly temporary; sunset constraint written
into this plan). Debt removed: unbootable default config, live-broken template, 6
stale TODOS entries, drifting status lines + the recurring header-drift class (canonical
version line). Reversibility: 5/5. Knowledge concentration: none (plan + audit doc
carry the why). 1-year read: obvious. Platform note: `config_parse_all` is the natural
home for config linting until `agentd check` (cap.1) supersedes it — record that in
cap.1's plan when written.

## Section 11 (Design/UX)

SKIPPED — no UI scope (verified: the 3 grep hits were "relative/rewritten form" false
positives).

## CEO Dual Voices — consensus

```
  Dimension                             Claude    Codex     Consensus
  ────────────────────────────────────  ────────  ────────  ─────────
  1. Premises valid?                    YES(vfd)  MOSTLY    CONFIRMED
  2. Right problem to solve?            PARTLY    PARTLY    CONCERN → UC1 (P0-2 deferral)
  3. Scope calibration correct?         AMEND     CUT+ADD   DISAGREE → T2 (hygiene cut)
  4. Alternatives explored?             NO (F8)   NO (#3)   GAP → fixed by amendments
  5. Competitive risks covered?         NO (F9)   NO (#7,8) CONCERN → UC1; D6 stands
  6. 6-month trajectory sound?          IF-FIXED  IF-FIXED  CONFIRMED w/ par.2 trigger
```

Cross-model-agreed amendments applied to scope (see items 3/4/6 + Implementation
decisions): general relative-path assertion replaces literal enumeration; `cos)`
DRY_RUN_ONLY hook; gated_requires↔sidecar consistency test; version-line↔CHANGELOG
test; honest claim narrowing (compose-only); par.2 sunset trigger.

**User Challenge UC1 — RESOLVED at final gate (operator, 2026-07-17):** both models
challenged leaving P0-2 unfixed for 3 increments. Operator chose **swap ux.10 and
ux.8′** — no stopgap in audit.1; the build order becomes **audit.1 → ci.1 → ux.8′ →
ux.10** (budget truth before TUI polish). audit.1's scope is unchanged; item 6
propagates the new order to CLAUDE.md/ROADMAP/AUDIT §9.

**Taste decisions — RESOLVED at final gate:** T1 — CI wiring stays in ci.1 (next
branch; the cos DRY_RUN hook lands now so ci.1's job is ~10 lines). T2 — TODOS
reconcile + status-line work stays in this branch (ratified §6 scope; the
version-line test and CLAUDE.md edit are coupled).

# Eng Review (Phase 3, via /autoplan — FULL_REVIEW, 2026-07-17)

## Step 0 — Scope challenge

Complexity check triggers on file count (~10 files) but the batch is 6 independent,
individually-revertable items sharing one theme — challenged and accepted (autoplan
override: never reduce; precedent: cheap-wins v0.75.0). Minimum set = items 1+4;
items 2/3/6b are the class-kills that justify the branch. Search check [Layer 1]:
parse-all config tests and post-transform assertions are standard practice; no
framework built-in replaces them (toml + serde already in tree). TODOS
cross-reference: this plan executes TODOS.md:204-211's own reconciliation note;
no deferred item blocks it.

**Eng-phase verification corrected three plan assumptions (both voices confirm):**
1. `gated_requires` is metadata + warning, NOT an env gate (`list.rs:44-45`,
   `spawn.rs:150,157`) → acceptance 4 rewritten; consistency test rescoped to
   token-extraction against `docker/` + `agentd/src/` (VOYAGE_API_KEY appears in
   neither → red today, proving the test).
2. CHANGELOG is not newest-first (v0.86.2 at line ~125 below v0.86.0 at line 6) →
   version-line test anchors to `agentd/Cargo.toml`; CHANGELOG reorder added to
   item 6.
3. The general guard needs single-quote anchors too (`cos.agents.toml:375` is
   `path='./output/…`) and `cos)` DRY_RUN must bypass `check_api_key`
   (`entrypoint.sh:108`); parse test also calls `agent_configs()` (pure,
   `config.rs:349-362`).

## Architecture (Section 1)

Component boundaries unchanged; all additions are leaf-level (test file, shell
guards, metadata strings). Dependency graph in the CEO section stands. One coupling
worth naming: the version-line test couples `agentd/tests/` to repo-root docs
(`../CLAUDE.md`, `../CHANGELOG.md`) — same pattern as the parse test's `../docker`
reach; the empty/missing-file guard applies identically. Production failure
scenario per new codepath: (a) guard false-positive on a legit future edit → boot
fails loudly with the line printed — designed; (b) token test false-positive on a
future prose-only gated_requires containing an ALL_CAPS_TOKEN that isn't an env var
→ fails visibly in CI with the token named; fix is one line of prose or an
allowlist entry — acceptable, loud, and cheap. Distribution: no new artifacts; the
entrypoint change ships in the existing image pipeline. **1 issue folded (single-
quote anchors), 0 open.**

## Code quality (Section 2)

DRY: the general assertion kills the would-be third copy of sed LHS literals; the
three bare-filename anchors are the irreducible remainder. `check_api_key` bypass
under DRY_RUN keeps one code path (guard placement identical in both modes). No
new abstractions; no cyclomatic hotspots (guards are flat greps; the test is two
loops). Existing ASCII diagrams in touched files: none affected (entrypoint has no
diagrams; config.rs diagrams untouched). **2 issues folded (DRY_RUN bypass,
agent_configs call), 0 open.**

## Test review (Section 3)

```
CODE PATHS                                              COVERAGE
[+] agentd/tests/config_parse_all.rs
  ├── glob 3 dirs, ≠Cargo.toml            [★★★] self + empty-dir guard + broken fixture
  ├── from_str::<Config> per file         [★★★] RED on model_id today (negative control)
  ├── McpServerConfig::validate() loop    [★★★] http-no-flag fixture
  └── agent_configs() lowering            [★★ ] both-forms conflict caught by bail
[+] token-consistency test                [★★★] RED today (VOYAGE), GREEN after fix
[+] version-line test                     [★★★] RED if CLAUDE.md ≠ Cargo.toml
[+] entrypoint.sh guards (cos|agent)      [★★ ] DRY_RUN_ONLY manual × 2; CI job = ci.1
[+] template gate fix                     [★★★] template.rs:1081 flip + list.rs badge test
USER FLOWS
[+] operator boots cos with real creds    [★  ] one manual confirmation pre-ship
[+] operator reads truthful [gated] badge [★★ ] via list-templates test

COVERAGE: 8/9 paths ★★+ | GAPS: entrypoint guards have no automated CI run (→ ci.1,
taste decision T1 at gate) — flagged, not silent.
```

No regressions introduced (all changes additive or metadata); REGRESSION RULE not
triggered. No LLM/prompt surfaces touched (sed rewrites task text paths only —
existing rule 6 unchanged in meaning). Flakiness: zero (no time/network/order deps).
Test plan artifact: `~/.gstack/projects/0x89karan-runtime1/0x89karan-audit.1-eng-review-test-plan-20260717.md`.

## Performance (Section 4)

Test adds <100 ms to `cargo test`; guards add ~6 greps (<5 ms) to container boot;
token test greps `docker/` + `agentd/src/` once (~50 files, <200 ms). **No issues
found** — no queries, no caching, no memory growth, no hot paths.

## Failure modes (eng registry)

| CODEPATH | FAILURE | TEST? | HANDLED? | USER SEES |
|---|---|---|---|---|
| parse-all glob | dir moved | Y (empty-guard) | Y | named test failure |
| parse-all parse | unknown key | Y (fixture) | Y | path + serde msg |
| validate loop | http-no-flag | Y (fixture) | Y | server + path named |
| agent_configs | both forms set | Y (bail msg) | Y | named test failure |
| cos guard | sed miss (any quote style) | manual DRY_RUN | Y | boot abort, line printed |
| agent guard | args not rewritten | manual DRY_RUN | Y | boot abort, line printed |
| token test | stale gated_requires | Y (red today) | Y | token + template named |
| version test | stale CLAUDE.md line | Y | Y | expected vs actual printed |

**0 CRITICAL GAPS** (no row is untested AND unhandled AND silent).

## Parallelization

| Step | Modules touched | Depends on |
|---|---|---|
| Item 1 (config fixes) | distro/overlay, agentd/*.toml comments | — |
| Item 2+6b (tests) | agentd/tests/ | Item 1 (test goes green after fix) |
| Item 3 (guards) | docker/ | — |
| Item 4 (template) | templates/, agentd/src/template.rs, docs/MCP_SERVERS.md | — |
| Items 5-6 (docs) | TODOS.md, CLAUDE.md, CHANGELOG.md | — |

Lane A: Item 1 → Item 2+6b (sequential). Lane B: Item 3. Lane C: Item 4.
Lane D: Items 5-6. All lanes are conflict-free (disjoint modules); in practice one
CC session does A→B→C→D in under two hours — parallel worktrees not worth the
ceremony for this size. Sequential recommended.

## Eng Dual Voices — consensus

```
  Dimension                          Claude     Codex      Consensus
  ─────────────────────────────────  ─────────  ─────────  ─────────
  1. Architecture sound?             YES+fixes  YES+fixes  CONFIRMED (leaf-level, fixes folded)
  2. Test coverage sufficient?       AMEND      AMEND      CONFIRMED after amendments
  3. Performance risks addressed?    YES        YES        CONFIRMED (none exist)
  4. Security threats covered?       YES (1 DoS YES        CONFIRMED (H4a self-DoS fixed
                                     via prose)             by key-anchoring)
  5. Error paths handled?            YES        YES        CONFIRMED (0 critical gaps)
  6. Deployment risk manageable?     AMEND(M3)  YES        CONFIRMED (ratchet stated honestly)
```

Cross-model agreements (independently found by both): gating-is-metadata (C1↔Cdx1),
consistency-test rescope (C2↔Cdx2), Cargo.toml as version truth + CHANGELOG
out-of-order (C3↔Cdx3), single-quote guard blindness (H2↔Cdx4), DRY_RUN credential
bypass (M2↔Cdx5), only-model_id-is-red-today (verification↔Cdx6). Claude-only
(verified, folded): H1 positive-form path-key assertion, H3 ship-workflow ambush,
H4 args-substring fragility + prompt-text self-DoS, M3 bind-mount ratchet honesty,
M4 doc-sweep real cost, L1 CARGO_MANIFEST_DIR, L2 comment-scan policy, L3 comment
location. **No cross-model tension — the voices disagreed with the plan, not each
other; all findings were mechanical (code-contradicts-text) and folded.**

# DX Review (Phase 3.5, via /autoplan — DX POLISH, 2026-07-17) [subagent-only: codex errored]

## Product type + persona (0A, auto-inferred)

CLI Tool + Platform. **Persona: the solo operator-developer** (single-tenant OS by
locked decision) **plus AI coding sessions bootstrapping from CLAUDE.md** — the
canonical version line's actual audience is every future Claude session (the audit's
own finding: a stale status line "steers every new session wrong"). Tolerance: high
skill, zero patience for silent failures; expects errors in the existing entrypoint
voice (problem + cause + exact fix command).

## Empathy narrative (0B, grounded in the v0.86.2 incident)

I edit `cos.agents.toml`, rebuild, `docker compose up cos`. Today, if a sed rule
missed my new path, the container boots fine and the CoS runs — until every
`write_file` dies with `capability_denied`, which I discover hours later by shelling
into the container and grepping flight.jsonl (that was the actual v0.86.2 debugging
session). After this plan: the boot refuses in under a second, prints the surviving
line with its line number, and tells me which sed rule to check. The metric this
increment moves is not TTHW — it's **time-from-misconfig-to-diagnosis: hours →
seconds**.

## Competitive benchmark (0C, reference points — no web search; internal tooling)

terraform validate (~1 s, exact line errors) · cargo's compiler errors (three-tier
gold standard) · k8s admission webhooks (fail at apply, not at runtime). Target
tier: match `terraform validate` — sub-second, line-anchored, remediation included.
The DRY_RUN flow as specced hits it.

## Magical moment (0D, auto-decided: copy-paste demo command)

`docker run --rm -e DRY_RUN_ONLY=1 <image> cos` → prints the fully-rewritten config
+ exits 0. Zero credentials, zero sidecars, one command, mechanical proof the boot
will pass the guards. This is also ci.1's future smoke hook — the demo command IS
the CI check.

## Journey trace (0F, POLISH — friction points, auto-decided fixes)

| Stage | Trace | Friction | Resolution |
|---|---|---|---|
| Discover | plan + AUDIT §6 | none | ok |
| Install | existing image pipeline | none (no new artifacts) | ok |
| Hello world | DRY_RUN invocation | **F-DX1: `DRY_RUN_ONLY` is documented only as a source comment in entrypoint.sh** — undiscoverable | fold: add a "Verifying config changes" snippet to DEPLOYMENT.md troubleshooting (3 lines, both `cos` and `agent` invocations) |
| Real usage | guard trip at boot | F-DX2: "names the surviving line" must include the line NUMBER (`grep -n`) and the remediation ("use absolute paths; check the sed rules in entrypoint.sh") | folded into item 3 spec |
| Debug | test failures | F-DX3: token-consistency failure must name the template file AND the fix options (correct the var name, or reword prose to drop the false token) | folded into item 4 spec |
| Upgrade | next release bumps Cargo.toml | F-DX4: version-test ambush | already folded (H3 HTML comment); /ship's CHANGELOG step touches the same area — comment placed where that step looks |

## DX Scorecard (scoped to this increment's surfaces)

| Pass | Score | Evidence |
|---|---|---|
| 1 Getting started (verification flow) | 9/10 | one command, no creds; −1: discoverability fixed only via F-DX1 doc snippet |
| 2 CLI design | 9/10 | badge/warning prose becomes truthful; no interface changes |
| 3 Error messages | 9/10 | all 4 new failure paths specced three-part (problem/cause/fix) after F-DX2/F-DX3 |
| 4 Documentation | 7/10 | MCP_SERVERS.md rewrite in scope; DEPLOYMENT's wrong jq probes remain (doc.1, out of scope — noted, not silent) |
| 5 Upgrade path | 9/10 | version test + H3 comment + CHANGELOG reorder are themselves the upgrade-path fix |
| 6 Dev env/tooling | 9/10 | dry-run mode, cargo-test-only (no new tools), no creds needed |
| 7 Community/ecosystem | N/A | single-tenant personal OS; no community surface touched |
| 8 DX measurement | 8/10 | acceptance criteria are mechanically checkable; boomerang = ci.1 wires the same commands |

**Overall: 8.6/10** (scoped). TTHW n/a; the moved metric is misconfig-to-diagnosis
(hours → <5 s).

## DX Voice — findings (subagent-only; codex errored this phase)

10 findings, all folded as mechanical: F1 DEPLOYMENT.md verification block (incl.
rebuild loop) · F2 acceptance-3 credential claim was false for `agent)` — dual-mode
semantics specced honestly (cos credential-free; agent dummy-key; google.json
preflight stays per cred.1's documented intent) · F3 token test red TODAY on
`MCP_SERVERS` doc-references (not just VOYAGE) — `.md`-suffix filter + allowlist
mechanism specced · F4 unified guard error-message template (both audiences +
repro) · F5 `AGENTOS_SKIP_PATH_GUARDS=1` escape hatch, named in error text · F6/F7
doc sweep grep-driven + invocations verified runnable (`--profile semantic` no
longer exists) · F8 "Latest shipped:" same-edit + README:17 named as deferred ·
F9 acceptance-6 contradiction with decision 22 fixed + release-checklist bullet ·
F10 POSIX `[[:space:]]` only (busybox-safe). Error-message scorecard after folds:
all four new failure paths deliver problem + cause + fix + (where relevant) escape
hatch.

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|---|---|---|---|---|---|
| 1 | CEO | Approach B (hotfix + guard batch) over minimal-only or pull-par.2-forward | Mechanical | P1,P3 | Class-kill where cheap; par.2 already sequenced | A, C |
| 2 | CEO | Include per-server `validate()` in parse-all test | Mechanical | P1 | Pure fn, catches parses-but-bails-at-boot | parse-only |
| 3 | CEO | Fix `agentd/agent.toml:75-81` capabilities comment in this batch | Mechanical | P2 | Blast radius, minutes, schema-verified wrong | defer |
| 4 | CEO | Doc sweep for stale VOYAGE references | Mechanical | P2 | Minutes, same theme | defer |
| 5 | CEO | Templates NOT added to parse glob | Mechanical | P4 | Duplicate of template.rs:913 | adding them |
| 6 | CEO | No guards for cockpit/orchestrate modes | Mechanical | P3 | No sed rewrite there — nothing to guard | adding them |
| 7 | CEO | Extra TODOS strikes beyond named 6 → doc.1 | Mechanical | P3 | Needs per-item verification; scope creep | in-batch |
| 8 | CEO | Embedding-broker routing stays deferred (direction 3) | Mechanical | P3,P6 | Deliberately sequenced own increment; Codex #6 noted, gate flip suffices now | pull-in |
| 9 | CEO | General relative-path assertion replaces guard enumeration | Mechanical (cross-model) | P1,P5 | Shorter AND covers future instances; kills third hand-copy | enumeration |
| 10 | CEO | `cos)` DRY_RUN_ONLY hook added to item 3 | Mechanical (cross-model) | P1,P6 | Makes acceptance 3 mechanical; unrepeatable artisanal test otherwise | manual-only |
| 11 | CEO | gated_requires↔sidecar consistency test added to item 4 | Mechanical (cross-model) | P1 | Kills the recurrence direction 3 would otherwise cause | string-fix-only |
| 12 | CEO | Version-line↔CHANGELOG test added to item 6 | Mechanical (cross-model) | P1 | Unenforced prose = fourth recurrence guaranteed | process-promise |
| 13 | CEO | Acceptance-4 claim narrowed to compose-only | Mechanical (cross-model) | P5 | Honest scope; non-compose stays audit-P3 tracked | overstated claim |
| 14 | CEO | par.2 sunset trigger written into plan | Mechanical (cross-model) | P6 | Prevents tourniquet becoming architecture | silence |
| 15 | CEO | sec.2 timing unchanged | Mechanical | — | Ratified D6 today; single-model concern; no new evidence | relitigate |
| 16 | CEO | Skip separate spec-review subagent loop | Mechanical | P3 | Dual voices already adversarially reviewed the doc against the repo (12 tool calls) | third reviewer |
| 17 | CEO | UC1 (P0-2 stopgap / order swap) → final gate | User Challenge | — | Both models challenge ratified order; operator owns it | — |
| 18 | CEO | T1 (minimal CI job now) → final gate | Taste | — | Codex valid point vs. increment discipline | — |
| 19 | CEO | T2 (cut hygiene items) → final gate | Taste | — | Codex valid point vs. ratified batch scope | — |
| 20 | Eng | Acceptance 4 rewritten — gating is badge+warning, not env gate (list.rs:44, spawn.rs:150) | Mechanical (cross-model) | P5 | Code contradicts plan text; "iff set" unimplementable | env-filter feature |
| 21 | Eng | Consistency test = token extraction over gated_requires, scope docker/+agentd/src | Mechanical (cross-model) | P1,P5 | Per-sidecar mapping doesn't exist; tokens verified feasible; VOYAGE red proves it | per-template map |
| 22 | Eng | Version test anchors to env!("CARGO_PKG_VERSION") + CHANGELOG-contains + reorder CHANGELOG | Mechanical (cross-model) | P1 | CHANGELOG not newest-first today (v0.86.2 below v0.86.0) | newest-entry parse |
| 23 | Eng | General guard gains single-quote anchors | Mechanical (cross-model) | P1 | cos.agents.toml:375 is single-quoted; claim was false without it | double-quote only |
| 24 | Eng | Positive-form path-key assertion replaces 3 bare-filename greps | Mechanical | P1,P5 | Bare literals byte-identical to sed LHS → lockstep drift (H1) | literal greps |
| 25 | Eng | agent) guard key-anchored ^args=, literals whole-file | Mechanical | P1 | /args/ substring false-positives on user task text (H4) | substring match |
| 26 | Eng | cos DRY_RUN bypasses check_api_key; canonical invocation = docker run (not compose) | Mechanical (cross-model) | P5 | :108 runs first; compose drags qdrant into a "credential-free" check (M2) | partial bypass |
| 27 | Eng | Bind-mount ratchet stated as behavior change + remediation in error text | Mechanical | P5 | M3: "no regression" claim was false for custom configs | silent claim |
| 28 | Eng | Doc sweep re-costed (~30 min, MCP_SERVERS.md table + var-name verification) | Mechanical | P5 | M4: field list corrected (showcases has no Voyage) | "minutes" |
| 29 | Eng | Test glob base = env!("CARGO_MANIFEST_DIR") | Mechanical | P5 | L1: CWD-relative is implicit coupling | CWD-relative |
| 30 | Eng | Guard scans comment lines too (documented) | Mechanical | P5 | L2: #-exclusion false-negatives on task-block content lines | comment-exclusion |
| 31 | Eng | agent_configs() in test + dup-id fixture if it rejects dups (verify at build) | Mechanical (cross-model) | P1 | M1/Cdx6: closer to boot path; don't over-claim dup-id behavior | assume dup check |
| 32 | DX | DEPLOYMENT.md "Verifying config changes" block (both modes + rebuild loop) | Mechanical | P1 | F1: DRY_RUN_ONLY undocumented anywhere user-facing | source-comment only |
| 33 | DX | Acceptance 3 dual-mode semantics: cos credential-free, agent dummy-key; cred.1 google.json intent preserved | Mechanical | P5 | F2: claim was false for agent); changing google.json bypass would contradict documented cred.1 decision | extend bypass to agent) |
| 34 | DX | Token test: strip `.md`-suffixed tokens + ALLOWLIST const | Mechanical | P1 | F3: MCP_SERVERS false-positive is present-tense, not future | unfiltered regex |
| 35 | DX | Unified guard error template (line+No., both remediations, repro, escape) | Mechanical | P5 | F4: spec was fragmented across bullets | fragments |
| 36 | DX | AGENTOS_SKIP_PATH_GUARDS=1 escape hatch, named in error | Mechanical | P1 | F5: no exit from a prose false-positive brick | no-override doc |
| 37 | DX | Doc sweep grep-driven (every Voyage hit) + invocations verified runnable | Mechanical | P1 | F6/F7: 4 missed hits; --profile semantic doesn't exist | line-range checklist |
| 38 | DX | "Latest shipped:" same-edit; README:17 named as doc.1-deferred | Mechanical | P5 | F8: don't over-claim the class kill | silent adjacency |
| 39 | DX | Acceptance 6 rewritten to match decision 22 (Cargo.toml truth) | Mechanical | P5 | F9a: acceptance contradicted the decision it verifies | newest-entry text |
| 40 | DX | Release-checklist bullet in CLAUDE.md "How to work here" | Mechanical | P1 | F9b: HTML comment alone won't reach the /ship path | comment-only |
| 41 | DX | POSIX [[:space:]] everywhere in guard patterns | Mechanical | P5 | F10: \s is GNU-only; busybox grep silently matches nothing | mixed classes |
| 42 | Gate | UC1: swap ux.10↔ux.8′ (operator); T1: CI in ci.1; T2: hygiene stays; plan APPROVED | Operator | — | Final approval gate 2026-07-17 | stopgap-in-audit.1 |

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 1 | CLEAR (PLAN via /autoplan) | 16 proposals, 7 accepted, 4 deferred |
| Codex Review | `/codex review` | Independent 2nd opinion | 3 | ran (CEO+Eng ok; DX errored) | CEO 8 + Eng 6 concerns, all folded |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | CLEAR (PLAN via /autoplan) | 14 issues, 0 critical gaps |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | SKIPPED | no UI scope |
| DX Review | `/plan-devex-review` | Developer experience gaps | 1 | CLEAR (via /autoplan) | score: 6/10 → 8.6/10, misconfig-to-diagnosis hours → <5s |

**CROSS-MODEL:** CEO voices converged on P0-2 exposure and instance-vs-class (→ UC1
+ 7 amendments); Eng voices converged on all 6 dimensions with zero tension —
independent discovery of gating-is-metadata, Cargo.toml version truth, and
quote-style guard blindness. DX ran subagent-only (Codex exit 1).

**VERDICT:** CEO + ENG + DX CLEARED — plan APPROVED at the final gate (2026-07-17);
UC1 resolved as build-order swap (audit.1 → ci.1 → ux.8′ → ux.10 → cap.1); ready to
implement.

NO UNRESOLVED DECISIONS
