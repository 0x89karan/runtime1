# par.2 — Config unification: one env-expanded config for Docker + QEMU

Branch: `par.2-config-unification` · Base: `main` (post AUDIT-v0.97 stack, v0.103.0)
Status: DRAFT for /autoplan. This is the increment I flagged as materially higher-risk than
the rest of the AUDIT-v0.97 sweep — design-bearing and not QEMU-validatable from a PR.

## Problem (AUDIT-v0.97 P2-10 / P2-14, and the audit86-P1-5/P1-8 tail)

The CoS ships in two boot paths that hand-maintain the SAME logical config with divergent paths:

1. **Docker** (`docker/entrypoint.sh`, 514 lines): reads `agentd/cos.agents.toml` (dev-relative
   paths) and **sed-rewrites** it at boot to absolute container paths (`cos)` mode ~6 sed rules;
   `agent)` mode a second sed for MCP script paths) into `/data/cos.agents.toml`, guarded by the
   P1-5 boot path-guards + `agentd check --strict`.
2. **QEMU/distro** (`distro/overlay/etc/agentd/cos.agents.toml`, a 348-line **fork**): a fully
   separate copy with absolute paths, `/run/memory` mounts, `bind_addr=0.0.0.0` + `allow_non_loopback`,
   `/usr/lib/agentos/docker/` MCP paths, and its own `[credential_gateway]`/`[management]` blocks.

Consequences the audit found:
- **Drift is luck, not process** (P2-14): cred.6 was mirrored into the fork, memory-routing was
  NOT — production QEMU CoS silently ran without email dedup. Nothing enforces parity.
- **The sed pipeline has an escape** (audit86-P1-8): the extra ERE only matches `*_path`/`*_dir`
  keys, so a bare `path = "x"` relative value slips through; "relative paths fail closed" is false.
  The audit's own note: "the class dies with the sed pipeline in par.2."
- **Two full copies** of a 300+ line config to keep in sync by hand.

par.1 (shipped) added a **denylist-parity guard** and an EventKind guard, but could only make the
*duplication* tamper-evident — it can't remove it. par.2 removes it.

## Proposed design (the part /autoplan must pressure-test)

Introduce **config-native `${VAR}` expansion** in agentd's config loader (`config.rs`, which has
NO expansion today — only two `std::env::var` reads for management flags). Author ONE canonical
`cos.agents.toml` with placeholders (`${STORE_DIR}`, `${OUTPUT_DIR}`, `${MCP_DIR}`, `${MGMT_BIND}`,
`${MGMT_ALLOW_NONLOOPBACK}`, `${STATE_DIR}`, `${TRIGGER_CRON}`, …). Each boot path sets those env
vars to its layout, then points agentd at the one config. Delete the sed pipeline AND the fork.

- Docker entrypoint: `STORE_DIR=/data OUTPUT_DIR=/data/output MCP_DIR=/etc/agentd MGMT_BIND=127.0.0.1 …`
- Distro init: `STORE_DIR=/run/memory OUTPUT_DIR=/run/output MCP_DIR=/usr/lib/agentos/docker MGMT_BIND=0.0.0.0 …`

## Open design decisions (for the dual-voice gauntlet — do NOT auto-decide the injection model)

- **D1 — Injection safety model (CRITICAL, constitutional-adjacent).** Env values are expanded into
  a TOML document that is then parsed. If expansion is raw-text substitution, a boot-var value
  containing `"\n[evil_section]\n…` could inject config structure, and a secrets-file-sourced var
  could smuggle a capability. Two candidate models:
    - **(A) Expand only into already-parsed string VALUES** — parse TOML first, then walk the tree
      substituting `${VAR}` inside string leaves only. Structurally injection-proof (can't create
      keys/sections), but can't parameterize non-string positions and is more code.
    - **(B) Raw-text expansion restricted to a fixed allowlist of boot-controlled var NAMES with
      validated values** (e.g. values must match `^[A-Za-z0-9_./:-]+$`, no newlines). Simpler,
      parameterizes anything, but safety rests on the validator + the guarantee that these vars are
      boot-script-set, never secrets-file-sourced.
  The secrets-from-env invariant + "the loop never panics" both bear on this. Recommendation to
  defend: **(A)** for structural safety, falling back to (B)+strict-validator only if (A) can't
  express a needed non-string parameter (e.g. `allow_non_loopback` bool, ports).
- **D2 — Scope.** Just `cos.agents.toml`, or also `agent.toml`/`agents.toml` and the `agent)` mode
  MCP-path sed? (Wider scope = fully retire the sed pipeline; narrower = leave `agent)` mode.)
- **D3 — Fate of the P1-5 boot path-guards + `agentd check --strict`.** After expansion produces
  absolute paths deterministically, are the grep guards still meaningful, or does `agentd check`
  on the expanded config fully replace them? (Keep `agentd check`; the grep guards likely retire
  with the sed pipeline they were guarding.)
- **D4 — QEMU-validation gate (the merge blocker I flagged).** Per-PR CI cannot boot QEMU
  (docker-smoke only runs `DRY_RUN_ONLY`, which par.2 rewrites; qemu-boot is a monthly cron). How
  do we prove the distro still boots before merge? Options: run `make -C distro test` locally once
  (heavy, needs Linux/Docker buildroot), OR trigger the qemu-boot workflow manually on the branch,
  OR gate merge on a one-off manual boot. This increment MUST NOT blind-land.
- **D5 — Retain `DRY_RUN_ONLY` + `AGENTOS_SKIP_PATH_GUARDS` semantics** in the new world (operators
  depend on the dry-run to preview the rendered config with zero secrets).

## Acceptance criteria (draft)

- One `cos.agents.toml`; `distro/overlay/etc/agentd/cos.agents.toml` fork deleted; cos-mode sed
  pipeline in `entrypoint.sh` deleted; both boot paths set the expansion env + point at the one config.
- Expansion is injection-safe per D1; a hostile env value cannot inject a config section or a capability.
- `agentd check --strict` passes on the expanded config in both layouts; `DRY_RUN_ONLY` still prints
  the rendered config with zero secrets.
- The par.1 denylist-parity guard is updated (it may go partly vacuous) and a NEW expansion test
  suite covers: happy expansion (both layouts), missing-var behavior, injection attempts (D1),
  and the memory-routing feature-set is present in BOTH layouts (closes P2-14).
- docker-smoke CI reworked to exercise the expansion path (not the deleted sed).
- **A real QEMU boot verified** (D4) before merge.

## Test plan (draft)

- Unit: config expansion (both layouts, missing var, injection corpus).
- `config_parse_all.rs`: the unified config parses + validates under both env layouts.
- docker-smoke: DRY_RUN_ONLY renders the expanded config; boot-guard/`agentd check` still fire.
- Manual/cron: `make -C distro test` QEMU boot on the branch.

## Risk

HIGH. Touches a constitutional security mechanism (the boot sanitization the P1-5 remediation
built), the config format, and both boot paths; a regression bricks production boot and per-PR CI
can't catch the QEMU half. Mitigations: D1 structural-safety model, keep `agentd check --strict`,
QEMU-boot gate at D4, incremental (cos first, then decide on `agent)` mode).

---

## /autoplan OUTCOME (2026-07-25) — RESHAPED-DOWN. The unification is retired.

Three independent voices (Codex + Claude CEO subagent + Claude Eng subagent) were **unanimous**:
do NOT build the `${VAR}`-expansion unification. It's a **User Challenge** the operator accepted
(chose "reshape-down to a 1h fix"). The decisive, code-verified reasons:

1. **The premise is partly stale.** P2-14 ("memory-routing silently missing from the QEMU fork")
   is no longer silent drift — the QEMU config's semantic-kb omission is **deliberate and
   test-pinned** by `agentd/tests/cos_spawn_caps_subset.rs::distro_cos2b_topology` (asserts the
   distro jobs never reference semantic-kb; the QEMU image ships no Qdrant/semantic-kb sidecar).
2. **`${VAR}` expansion cannot express the real divergence.** The two configs differ *structurally*
   — a whole `[[mcp_servers]]` semantic-kb block, a `mail:raw` memory segment, job-cap array
   membership, and a `10M`-vs-`50M` integer `global_token_budget`. String-leaf substitution can't
   add/remove a section or change a non-string leaf. The plan's AC ("memory-routing present in
   BOTH layouts, closes P2-14") is **mutually exclusive** with the existing green test.
3. **Net-negative security + un-CI-able failure mode.** It would trade a first-party sed footgun
   (checked-in config, single-tenant, never attacker-supplied) for a real config-injection surface
   in the loader, and its worst case (PID-1 boot brick) can't be caught by per-PR CI.
4. **The plan's env layout had a live bug** (`MGMT_BIND=127.0.0.1` for Docker) — both configs
   deliberately bind `0.0.0.0`; that would have regressed the Docker management API.

### UPDATE — the ERE fix was ALSO dropped (par.2 /autoplan-recovery, 2026-07-25)
After building the ERE tightening below, CI surfaced it as the wrong fix too: (1) its negative-control
fixture (`cos-bare-relative-dir.toml`, a bare `dir` key) is **not a valid Config** — `dir` is an unknown
field — so `config_parse_all.rs` (which requires everything in `.github/fixtures/` to parse) went red;
(2) that exposed the deeper point — **a bare `path`/`dir` key isn't reachable by a valid config** (no
such field in the schema; `agentd check --strict` rejects it before boot regardless), and the genuinely-
reachable escapes (`prefix = "output"`, relative MCP `args`) aren't caught by a key-suffix ERE anyway.
The real fix is the runtime path-identity newtype in **cap.3** (audit86-P1-8). So par.2 slimmed to
**DOCS ONLY** — the QEMU-fork header note + the P2-14 reclassification; no code, no fixture, no version
bump. The P1-5 boot-guard remainder stays OPEN, superseded by cap.3. Struck-through recipe kept for record.

### ~~What ships instead (this branch) — the ~1h targeted fix~~ (DROPPED — see UPDATE)
- ~~**Close audit86-P1-8** (the one genuinely-live pain): tighten the cos boot-guard ERE from~~
  `[a-z_]*_(path|dir)` to `[a-z_]*(path|dir)` so a **bare** `path = "x"` / `dir = "x"` (relative,
  no `./`, no `_` prefix) can no longer escape the guard. (audit86-P1-8 in capability.rs is a SEPARATE runtime-matching issue → cap.3, untouched here.) Proven by a new CI negative-control
  fixture (`cos-bare-relative-dir.toml`) that the old ERE would have booted.
- **Document the intentional divergence** in the QEMU fork header (crisp pointer to the test that
  pins it) so a future reader doesn't re-file it as drift.
- The two-config drift stays guarded by par.1 + `cos_spawn_caps_subset.rs` (already shipped).

### Deferred (filed, not built)
- `agent)`-mode sed retirement → **par.3** (different input class: dynamic template output +
  arbitrary `AGENT_TASK` prose; wider blast radius).
- If unification is ever revived: do it as a **build-time generator** from one annotated canonical
  source (CI-testable without a QEMU boot, no runtime-loader surface), NOT runtime `${VAR}`
  expansion — and only after resolving the structural (semantic-kb) profile difference.
- par.1-ar-01 (agentctl error-view) stays filed; not folded in (behavioral agentctl change,
  its own increment).
