<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/cap.1-autoplan-restore-20260722-183256.md -->
# cap.1 — capability declaration surface

**Source:** `docs/AUDIT-v0.86.md` §6 (build order) + S1 (capability-system recommendation).
**Depends:** audit.1 (v0.87.0 ✅ shipped). **Sequencing:** ROADMAP audit track (cap.1 → cap.2 → cap.3);
UX track pulled cap.1 forward ahead of ux.13 (SetCaps wants cap.1's validation machinery).
**Predecessor context:** ux.11c (v0.92.0) just shipped; `main` is at that.

## Problem (from the audit)

The capability **matcher** is sound and well-tested (`satisfies`, path normalization, boundary-safe KB
prefixes, deny-on-empty). What's broken is the **declaration surface** — the ways a config can *look*
granted but be inert or wrong, with no signal until something silently fails closed in production:

- **P1-9 — wrong-tier grants are silent no-ops (6 of 9 combos inert).** Agent-level `Credential`
  (`capability.rs:70`, enforcement "deferred") and `Net` (`:48`, advisory/true) are decorative; HTTP MCP
  servers discard `capabilities`/`isolation` and are *exempted* from `mcp_require_capabilities`
  (`main.rs:361` `.filter(!is_http())`) with no warning — the operator's mandatory-sandbox switch does not
  cover remote tool servers. This is the "Gmail outage" class.
- **P1-11 / audit-C2 — bare `agent`/`agent/` KB grants defeat per-agent Tier-3 memory isolation**
  (`capability.rs` test asserts `agent/sub` is satisfied by an `agent/` grant — breaks a locked memory rule).
- **Silent fail-closed** — a capability-denied call is recorded as a flight event but not surfaced to the
  operator; a misconfigured agent just quietly does nothing (the v0.86.2 root-cause class).
- The container entrypoint validates config with **`grep`**, not real parsing.

## Framing (ratified at the CEO gate)
cap.1's value is **misconfiguration ergonomics — killing the silent-fail-closed / Gmail-outage class** —
NOT defense against a malicious agent (single-tenant, mutually-trusting constitution). Strategic job: to
**unblock cap.2** (the one real security gap — Curator inheriting live Gmail via all-or-nothing spawn
inheritance). This disciplines scope toward cap.2, not gold-plating cap.1.

## Scope (cap.1 — REFRAMED + TRIMMED at the CEO gate)

- **A1. `agentd check` config linter** (new subcommand), runnable at test/CI/boot time. It **reuses the
  runtime config loader + ONE shared effective-capability resolver** — never a second interpreter of
  capability legality (F3). Checks:
  - `Mcp { server }` names exist in `[[mcp_servers]]`; `KbRead`/`KbWrite { segment }` names exist in
    `[memory].segments`;
  - **tier-legality** via the shared resolver, over a **no-wildcard match on `Capability`** so a new enum
    variant fails to compile until its tier legality is declared (F3 guard): HTTP MCP server carrying
    `capabilities`/`isolation` (silently discarded by the HTTP path) + inert agent-level `Credential`/`Net`;
  - relative `FsRead`/`FsWrite` prefixes — **contextual severity** (F5): warn in default mode, hard error in
    `--strict`. Minimal only; cap.3's `AbsPathPrefix` retires this later.
  - prompt-path-literal ↔ `FsWrite` consistency — **warning-only, best-effort, NOT acceptance-load-bearing**
    (F6).
  - **Boundary (F8):** validates the *declaration* only — cannot prove credential presence/scope, MCP boot,
    or OAuth validity (secrets from env). Stated in output.
  - **Severity (F2/F5):** wrong-tier `Credential` + relative `FsWrite` are **hard errors in `--strict`/boot
    mode** (non-optional — the acceptance classes); default mode warns for local-dev ergonomics.
- **A2. `CapabilitiesResolved` boot event** — logs each agent's + MCP server's **effective** cap set once at
  boot, via the **same shared resolver A1 uses** (F3). Descriptive only, never a second enforcement decision
  (F10). New `EventKind`.
- **A3. Entrypoint grep → `agentd check --strict`** (`docker/entrypoint.sh`) — runs on the **sed-rewritten
  `/data` config, AFTER the rewrite + template gating** (F4), fail-closed on the error class.
- **A5. Fix audit-C2 / P1-11** — reject bare `agent`/`agent/` KB grants (require full `agent/<id>`) in
  `agentd check`; matcher-level hardening only after scanning both `cos.agents.toml` confirm no live config
  depends on the bare form (OD4).

## Split out → **cap.1b** (separate increment)
- **A4. Runtime denial surfacing** — per-agent `capability_denied` counter + Dashboard ATTN column. Runtime
  observability (touches `surfaces`/`agentctl`), cap.2 doesn't depend on it. When built: first denial of a
  **novel (agent, capability) pair surfaces (N=1)**, dedup only repeats; rides the **shared ATTN channel**
  (audit §4, F9).

## Acceptance (cap.1)
- The Gmail misconfig (wrong-tier `Credential`) AND the v0.86.2 misconfig (relative `FsWrite`) both **fail
  `agentd check --strict`** in a regression fixture (hard errors, F2).
- `agentd check` (default) on the real `cos.agents.toml` is clean (no false positives).
- The tier-legality match is **wildcard-free** — a new `Capability` variant fails to compile until its tier
  is declared (guard has a test, F3).
- `CapabilitiesResolved` boot event emits each agent's + server's effective set.

## Explicitly NOT in scope
- **cap.1b** — runtime denial surfacing / ATTN column (split out above).
- **cap.2** — spawn attenuation (cap.1 unblocks it; doesn't do it).
- **cap.3** — `AbsPathPrefix` newtype (retires A1's relative handling).
- Enum split `AgentCapability`/`SandboxCapability` (S1: defer).
- Credential-**absence** detection (F8 — secrets from env).

## Phase 1 — CEO Review (autoplan)

### CEO dual voices — consensus table
```
  Dimension                              Claude   Codex   Consensus
  ────────────────────────────────────── ──────── ─────── ─────────
  1. Right problem?                       yes*     yes*    CONFIRMED (*reframe: ergonomics/no-silent-fail, NOT distrust; job = unblock cap.2)
  2. Scope calibration?                   trim     split   CONFIRMED → split A4 out; demote prompt-literal heuristic
  3. Acceptance hollow risk?              CRIT     yes     CONFIRMED — acceptance vs OD1 self-contradict; must hard-error in strict mode
  4. 6-month regret?                      YES      YES     CONFIRMED — linter must reuse ONE resolver + exhaustiveness guard, or it drifts
  5. Lint source vs resolved artifact?    NO(HIGH) NO      CONFIRMED — must lint the sed-rewritten /data config post-gating
```

### Findings (both models)
- **F1 (reframe, both) — value is misconfiguration ergonomics, not defense-against-malice.** Single-tenant
  mutual trust: none of A1–A5 defends against a malicious agent. cap.1 kills the **silent-fail-closed /
  Gmail-outage class** and is **scaffolding to unblock cap.2** (the one real security gap — Curator
  inheriting live Gmail). Ratify this framing so it disciplines scope toward cap.2, not gold-plating cap.1.
- **F2 (CRITICAL, Claude) — acceptance contradicts OD1.** Acceptance requires the Gmail misconfig
  (wrong-tier `Credential`) AND the v0.86.2 misconfig (relative `FsWrite`) to **fail** `agentd check`, but
  OD1 lists both as warn-vs-error *candidates*. If either is a warning, boot doesn't block it and prod ships
  the broken config → the increment is hollow by its own name. **Fix:** those two classes are **hard errors
  in strict/boot mode** (non-optional).
- **F3 (HIGH, both) — one effective-cap resolver + exhaustiveness guard.** The audit's root cause is "one
  enum, two enforcement interpreters, no check tying them." A1 as written risks becoming a **third**
  interpreter that drifts as the enum grows (`RunsRead`/`BriefPublish` were just added). **Fix:** A1 and
  A2 consume the SAME effective-cap resolver (one code path, also used by the boot event and later
  cap.2/SetCaps); add a **no-wildcard match** on the tier-legality table so a new `Capability` variant fails
  to compile until the table is updated.
- **F4 (HIGH, Claude) — lint the RESOLVED artifact, not the source.** The runtime loads the sed-rewritten
  `/data/cos.agents.toml`, not the `/etc` template. Linting the source false-positives on every relative
  path (they get rewritten downstream) or validates a config the runtime never loads. **Fix:** A3 runs
  `agentd check --strict` on the resolved `/data` config, **after** the sed pipeline and template gating.
- **F5 (HIGH, Claude) — contextual severity via `--strict` boot profile, not a flat error/warn table.**
  Relative prefixes are load-bearing in local dev (`cargo run -- agent.toml`) but were the prod root cause.
  So: local invocation warns; the entrypoint runs `--strict` and hard-fails relative prefixes + wrong-tier
  grants. Resolves OD1+OD2+OD4 together.
- **F6 (MED, both) — demote the prompt-path-literal ↔ FsWrite check.** Prompts are free text; a literal
  scan false-negatives (constructed/templated paths) and false-positives (incidental path-like strings) —
  it re-creates the silent-fail hole inside the tool meant to close it, and duplicates the surviving
  entrypoint grep. **Fix:** warning-only, NOT acceptance-load-bearing (or drop).
- **F7 (MED, split, both) — A4 (runtime denial surfacing) is a different surface → cap.1b.** A1/A2/A3/A5 are
  config/boot-time (static legibility); A4 is runtime observability (snapshot counter + ATTN + TUI column),
  touches `surfaces`/`agentctl`, shares no code with the linter. cap.2 does not depend on A4. Split it out.
- **F8 (MED, Claude) — acceptance honesty.** `check` closes the wrong-tier-**declaration** mode only, NOT
  credential-**absence** (secrets come from env by constitution). State the boundary so the acceptance claim
  doesn't overstate coverage.
- **F9 (LOW, Claude) — OD3 already answered by the audit (§direction 4):** capability-denial rides the
  SAME attention channel as approvals/budget/credential ATTN. Not a distinct channel. (Applies to cap.1b.)
- **F10 (LOW, Claude) — A2 MCP effective-set logging stays descriptive** (log what enforcement already
  does), never a second place that decides enforcement (the S1 line).

### Decision Audit Trail (CEO)
| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 1 | CEO | F1 reframe: ergonomics not distrust; unblock cap.2 | **PREMISE** (gate) | n/a | single-tenant mutual trust; both voices |
| 2 | CEO | F7 split A4 (denial surfacing) → cap.1b | **USER CHALLENGE** (gate) | n/a | both voices; different surface, cap.2 doesn't need it |
| 3 | CEO | F2 Gmail + v0.86.2 classes = hard errors in strict mode | Mechanical (correctness) | P1 completeness | acceptance is hollow otherwise |
| 4 | CEO | F3 one resolver + no-wildcard exhaustiveness guard | Mechanical (invariant) | P5 explicit | else the linter becomes a 3rd drifting interpreter |
| 5 | CEO | F4 lint the resolved /data artifact post-gating | Mechanical (correctness) | P1 completeness | runtime loads the rewritten file, not the template |
| 6 | CEO | F5 `--strict` boot profile (contextual severity) | Mechanical | P3 pragmatic | relative is legal in dev, fatal in container |
| 7 | CEO | F6 demote prompt-literal to warning-only | Taste→settled | P5 explicit | brittle heuristic; not acceptance-load-bearing |
| 8 | CEO | F8 state credential-absence is out of scope | Mechanical (honesty) | P1 completeness | secrets from env; check validates declaration only |

## Open decisions — RESOLVED at the CEO gate
- **OD1 (severity):** `--strict` boot profile, not a flat table (F5). Wrong-tier + relative = hard error in
  strict; warn in default.
- **OD2 (what to lint):** the resolved artifact the runtime execs (sed-rewritten `/data`, post-gating), via
  the shared loader (F4).
- **OD3 (ATTN channel):** shared attention channel per audit §4 — deferred to cap.1b (F9).
- **OD4 (bare-agent):** reject in `check`; matcher hardening only after scanning both `cos.agents.toml` for
  a live dependency on the bare form.

## Phase 3 — Eng Review (autoplan)

### Eng dual voices — consensus table
```
  Dimension                              Claude    Codex    Consensus
  ────────────────────────────────────── ───────── ──────── ─────────
  1. Credential hard-error safe?          NO(BLOCK) NO(HIGH) CONFIRMED — hard-erroring agent-level Credential bricks CoS boot
  2. Real Gmail-outage class?             wiring    wiring   CONFIRMED — server-side Credential cap MISSING (not agent wrong-tier)
  3. Shared resolver feasible?            yes*      yes*     CONFIRMED — reuse parse+lower loader (NOT run_agent); resolver in capability.rs
  4. Exhaustiveness guard?                compile   compile  CONFIRMED — no `_` arm (compile-fail); test pins values, can't force it
  5. CLI subcommand clean?                yes(gap)  yes(gap) CONFIRMED — add `check` before catch-all; --strict needs strip-list plumbing
  6. tier-legality list right?            NO        NO       CONFIRMED — per (variant × 3 tiers) matrix; universal all-inert; +ShellExec
```

### Findings (both models, all folded into the revised mechanism below)
- **G1 (BLOCKER, both) — agent-level `Credential` must NOT be a hard error.** Both `cos.agents.toml`
  (`agentd/…:256`, `distro/…:168`) grant `Credential{Google}` at the agent level; it is **100% inert today**
  (the broker token's `allowed_providers` is built ONLY from the stdio MCP server's own caps —
  `main.rs:668 credential_allowed_providers`), but A3 runs `check --strict` on the rewritten config before
  `exec agentd`, so a blunt "agent-level Credential = error" **fails CoS boot** — a loud outage from the tool
  built to prevent the silent one. **Fix:** the Credential rule is a **config-level wiring cross-check** —
  hard-error in strict iff a `Credential{provider}` is granted anywhere but **no stdio MCP server carries a
  matching `Credential` cap** (→ no broker token will ever carry it → provably inert → guaranteed silent
  denial). Passes the real config (Credential on both `google_oauth` server + orchestrator); catches the
  Gmail fixture (agent-only grant, no server). This is the true historical bug (`cos.agents.toml:216-219`).
  Also **fix the two misleading comments** in the cos configs (they claim the inert agent-level grant is
  load-bearing — the exact rot audit.1 targets).
- **G2 (both) — acceptance gap:** add "`agentd check --strict` passes the **rewritten** `cos.agents.toml`"
  to acceptance. Today A3 makes strict-on-the-real-config the boot gate, but nothing tests it until a live
  container. The Gmail fixture is "Credential granted, no matching stdio server" (G1's mechanism), NOT
  "agent-level Credential exists."
- **G3 (both) — shared resolver in `capability.rs`:** `tier_legality(cap: &Capability, ctx: CapContext)
  -> Legality` where `CapContext = {Agent, StdioMcp, HttpMcp}`, `Legality = {Enforced, Inert(&'static str)}`.
  A1 (linter) + A2 (boot event) both call it. **The Credential wiring cross-check is NOT expressible as
  `(cap, ctx) → legality`** (it depends on OTHER servers) — keep it a separate config-level check that
  consumes `tier_legality(Credential, Agent) == Inert` as a precondition. "Reuse the runtime loader" =
  parse+lower only (`toml::from_str::<Config>` + env overrides + `agent_configs()` +
  `McpServerConfig::validate()` — the `config_parse_all.rs::check_spec` template), **NEVER `run_agent`**
  (which spawns subprocesses/FUSE/proxy). No `Config::validate()` exists today — say "parse+lower loader."
- **G4 (both) — exhaustiveness is COMPILE-TIME:** `tier_legality` matches `Capability` with **no `_` arm**
  (mirror `satisfies`/`caps_to_rules_inner`); a new variant fails to compile until its legality is declared.
  Doc-comment the match `// NO wildcard: a new Capability variant must not compile until its tier legality
  is declared` (stops a future dev "fixing the build" with `_ =>`). The test pins known-variant values; it
  cannot force exhaustiveness — acceptance says "compile-fails," not "test-fails."
- **G5 (HIGH, both) — CLI `check` subcommand:** add `Some("check") =>` before the `Some(path)` catch-all in
  `main.rs` (mirrors the existing `--probe` precedent; only "breaks" a config literally named `check`).
  **`--strict` must be added to `filter_positional_args`'s strip list** (else it survives as a positional
  and misparses as the path). Require an explicit path for `check` (don't default to `agent.toml`).
- **G6 (MED, Claude) — A3 scope to `cos)` entrypoint mode only for cap.1.** `agent)` mode renders via
  `agentctl spawn --dry-run` + sed; template gating there could leave a dangling `Mcp`/`Credential`
  reference → false-positive fail-closed boot. cap.1's fixtures live in `cos)`; defer `agent)`-mode check
  until render cap/server consistency is proven.
- **G7 (HIGH, Claude) — test-fixture placement + parse-all collision.** Check fixtures **parse cleanly but
  must FAIL check** — a third category. Put them in `agentd/tests/fixtures/` (NOT `.github/fixtures`, which
  gets mounted into nightly-E2E/docker-smoke images). `config_parse_all.rs` sweeps the SOURCE cos configs
  (relative `./output`) → run `--strict` **only on rewritten artifacts**, default mode on sources (else CI
  reddens on a legit dev config).
- **G8 (LOW, both) — tier-legality matrix corrections:** agent-level `Net` inert (safe to flag), agent-level
  `ShellExec` inert (add it), HTTP-MCP discards `capabilities`+`isolation` (cleanest hard-error; real config's
  only HTTP server `semantic-kb` carries neither → no false positive), universal-agent caps all inert
  (external process, no tool registry). The `mcp_require_capabilities` HTTP exemption → **warning** only
  (not enforceable — no subprocess to sandbox).
- **G9 (LOW, Claude) — bare-agent A5 also rejects the `agent:` colon form** (`kb_segment_satisfies` treats
  `:` and `/` as delimiters, so bare `agent` defeats both `agent:<id>` and `agent/<id>`). Real configs use
  `ops:*`/`mail:raw` → nothing live breaks. Check-only (OD4).

### Decision Audit Trail (Eng)
| # | Phase | Decision | Classification | Principle | Rationale |
|---|-------|----------|----------------|-----------|-----------|
| 9  | Eng | G1 Credential = wiring cross-check, NOT agent-level hard error | Mechanical (BLOCKER fix) | P1 completeness | else `check --strict` bricks real CoS boot |
| 10 | Eng | G2 acceptance: strict passes rewritten cos.agents.toml + Gmail fixture = missing-server | Mechanical (correctness) | P1 completeness | the boot gate was untested |
| 11 | Eng | G3 tier_legality resolver in capability.rs; loader = parse+lower not run_agent | Mechanical | P5 explicit | one resolver, no boot side-effects |
| 12 | Eng | G4 no-wildcard match = compile-time guard + doc comment | Mechanical (invariant) | P5 explicit | drift-proof against enum growth |
| 13 | Eng | G5 `check` subcommand + --strict strip-list | Mechanical | P3 pragmatic | else --strict misparses as path |
| 14 | Eng | G6 A3 = cos) mode only for cap.1 | Mechanical (scope) | P3 pragmatic | agent) gating consistency unproven |
| 15 | Eng | G7 fixtures in tests/fixtures/; --strict on rewritten only | Mechanical (correctness) | P1 completeness | avoid image-mount + parse-all redness |
| 16 | Eng | G8/G9 matrix corrections + agent: colon form | Mechanical | P1 completeness | correctness of the inert table |

## Superseded open decisions (original — see RESOLVED above)
- **OD1** — `agentd check` severity model: which findings are hard errors (fail boot) vs warnings? The
  audit is explicit on tier-legality-HTTP = hard error; relative prefixes and inert agent-level grants are
  candidates for warn-vs-error. Boot must fail-closed on the error class only.
- **OD2** — does `agentd check` read a single config path (arg) or the same resolution the runtime uses?
- **OD3** — ATTN denial surfacing: reuse the existing attention-signal channel (BudgetRisk/Credential
  ATTN) or a distinct capability-denied signal? Threshold (repeated identical denials — how many)?
- **OD4** — bare-`agent` KB rejection: hard error everywhere, or warn in `check` + hard-reject in the
  matcher? (The matcher change could break existing configs — needs a scan of cos.agents.toml.)


---

**STATUS: APPROVED** (autoplan, 2026-07-22, HEAD e7372443) — reframed to misconfig-ergonomics; A4 split to cap.1b; Eng blocker (Credential wiring cross-check) folded. Ready to build.
