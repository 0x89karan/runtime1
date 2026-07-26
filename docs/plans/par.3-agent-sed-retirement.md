# par.3 — `agent)`-mode sed retirement

> **VERDICT: DEFERRED (2026-07-26, /autoplan premise gate).** Both CEO voices ranked par.3 below the UX
> tail with zero user value; Codex said STRIKE outright, Claude said build-only-with-a-hard-test-else-strike.
> The one finding that would have justified a cheap hardening — a guard "blind spot" on surviving
> installed-absolute paths — was **verified against code and found overstated** (see correction below): the
> `agent)` guard's second ERE (`^\s*"/usr/lib/agentos/docker/`, added by audit.1 at `entrypoint.sh:369`)
> already fails the boot closed on that exact case, and single-line `args` arrays are rewritten correctly by
> the sed. So the live risk is covered; only positive CI coverage of the 3 installed-absolute templates is
> missing, and even that fails closed via the guard. **Decision: leave the working sed untouched, move to the
> UX tail (ux.2b/ux.3/ux.10).** Revisit only as a build-time Docker/QEMU artifact generator if sed retirement
> ever becomes a real priority — never a one-off `agentctl` path flag (that adds a boot-brickable CLI/config
> contract to a layout-agnostic binary, per both voices). This doc is kept for the grounding + verdict.

**Finding-1 correction (post-gate, code-verified):** the CEO Claude voice's HIGH finding claimed a surviving
`/usr/lib/agentos/docker/…` is absolute so `guard_no_relative_paths` can't catch it. That is wrong — the
`agent)` call passes a second ERE `^[[:space:]]*"/usr/lib/agentos/docker/` (`entrypoint.sh:369`, audit.1),
which catches a surviving line-start installed-absolute path and fails the boot closed. The multi-line-array
failure mode Claude imagined is exactly what that ERE covers; single-line arrays the sed rewrites fine. The
residual is coverage-only (docker-smoke positively pins `scout` but not `watcher`/`cron-agent`/`webhook-agent`),
not a silent-corruption hole. This correction is *why* the verdict landed at DEFER rather than "cheap hardening."

---

<!-- historical draft below (superseded by the DEFERRED verdict above) -->
# par.3 — `agent)`-mode sed retirement (original draft)

Branch: `par.3-agent-sed-retirement` (proposed) · Base: `main` (v0.110.0)
Kind: config/boot refactor with a **genuine reshape-or-strike premise** (like par.2). Last audit-tail
item of substance. Autoplan this before building — the value may not exceed the blast radius.

Grounded 2026-07-26 by a full sed/distro sweep (findings in this file's §Grounding). par.2 already
proved the Docker↔QEMU *config unification* is structurally test-pinned and can't go declarative; par.2
explicitly deferred **the `agent)`-mode sed** to par.3 as "a different input class: dynamic template
output + arbitrary `AGENT_TASK` prose; wider blast radius."

---

## Premise (the thing autoplan must rule on first)

**Claim:** the `agent)`-mode boot-time sed (`docker/entrypoint.sh:353-356`) is a fragile,
correctness-sensitive rewrite of runtime-generated config that should be retired by moving Docker-path
resolution up-source into `agentctl spawn` template lowering, so `agentctl spawn --dry-run` emits
layout-correct MCP paths directly and the entrypoint needs no post-processing.

**Why it might be TRUE (build it, reshaped to agentctl lowering):**
- The sed runs on the **dynamic** output of `agentctl spawn "$TEMPLATE" --task "$TASK" --dry-run`
  (`entrypoint.sh:346`), not a static file — so it's inherently a text-munge of generated output, the
  exact anti-pattern "the loop never munges its own generated config" would flag.
- It is **line-anchored to `^args =`** *specifically because* a bare `/args/` address corrupts an
  `AGENT_TASK` that merely mentions "args" (regression F3 in h7.2-ar-01). That fragility is latent: a
  template whose `args` wraps across lines, or a future field named `*args*`, silently breaks it.
- Moving resolution into agentctl makes the paths **compile-tested Rust** with the layout chosen by an
  explicit parameter, not a regex — and deletes the whole "did a relative path survive the rewrite?"
  guard-and-grep dance for the agent path.

**Why it might be FALSE (strike it / leave the sed):**
- The sed **works today** and is **belt-and-suspenders guarded**: `guard_no_relative_paths /data/agent.toml`
  (`entrypoint.sh:367-370`) fails closed, and CI `docker-smoke` pins the rewritten output
  (`ci.yml:519-532` scout dry-run; the `cos-broken-relative.toml` negative control at :550-573).
- Its input is **first-party** (checked-in templates), never attacker-supplied — the par.2 "net-negative
  security" logic (don't trade a first-party footgun for a real injection surface) partly applies.
- Moving it into `agentctl` means agentctl must learn the **target layout** (Docker `/etc/agentd` vs
  installed `/usr/lib/agentos/docker` vs dev-relative). That's new surface in a binary that today is
  layout-agnostic; done wrong it re-introduces exactly the divergence par.2 walled off.
- Blast radius touches the boot/PID-1-adjacent path; a regression is a **boot brick**, un-catchable by
  per-PR unit tests (only docker-smoke/qemu catch it).

**Autoplan's job:** decide build-reshaped vs. strike, and if build, lock the mechanism (D1–D3 below).
Default recommendation going in: **build, reshaped to agentctl lowering** — but only if D1 keeps agentctl
layout-agnostic-by-default (no hard-coded container paths in the binary).

---

## If BUILD — scope (agent) only)

Retire **only** the two `agent)`-mode sed rules. The `cos)` sed (static-file rewrite, 6 rules) stays;
it's lower blast radius and is *not* the item par.3 was filed for. The Docker↔QEMU unification stays
walled off (par.2). The absent semantic-kb URL rewrite (AUDIT-v0.86:166) is a separate cos)-side gap —
out of scope here.

### The two rules to retire (`entrypoint.sh:353-356`)
```
-e '/^args[[:space:]]*=/s|"\.\./docker/|"/etc/agentd/|g'          # dev-relative templates → container
-e '/^args[[:space:]]*=/s|"/usr/lib/agentos/docker/|"/etc/agentd/|g'  # installed-absolute → container
```
Both map MCP-sidecar `args` paths to the container layout `/etc/agentd/`. Seven templates carry
dev-relative `../docker/…`; three carry installed-absolute `/usr/lib/agentos/docker/…`.

### Decisions for autoplan

- **D1 — how does agentctl learn the target layout?**
  - (a) An explicit `agentctl spawn --mcp-dir <dir>` flag (entrypoint passes `--mcp-dir /etc/agentd`);
    agentctl stays layout-agnostic, the container names its own path. **Recommended** — no container
    paths compiled into the binary; the boot script still owns the layout, just declaratively.
  - (b) An env var `AGENTOS_MCP_DIR` read by agentctl lowering. Same idea, ambient not explicit.
  - (c) agentctl resolves `../docker/`→absolute against a `--base`/CWD anchor (reuses the cap.3
    `fs_anchor` idea). More magic; risks re-encoding layout knowledge in Rust.

- **D2 — what does lowering actually rewrite, and where?**
  Only the `args` array entries of `[[tools.mcp_servers]]` in the lowered template output, replacing a
  known template-relative/installed prefix with `--mcp-dir`. Must NOT touch task prose or any other
  field (preserve the `^args=`-anchoring intent, but as structured TOML edits, not regex). Decision:
  edit the parsed TOML value in agentctl's spawn lowering (structured, so prose is untouchable by
  construction) vs. a narrower string pass.

- **D3 — keep the boot guard?**
  Keep `guard_no_relative_paths /data/agent.toml` even after the sed is gone (defense in depth — it now
  asserts agentctl did its job, cheaply). Keep the docker-smoke assertions; update only if a message or
  path changes. **Recommended: keep both guards.**

## Acceptance (if BUILD)
- `agent)` mode contains **no sed**; `agentctl spawn --dry-run` (with the D1 mechanism) emits
  `/etc/agentd/…` MCP paths directly for all 10 templates.
- `guard_no_relative_paths /data/agent.toml` still runs and still fails closed on a planted relative path.
- CI `docker-smoke` scout dry-run (`ci.yml:519-532`) stays green unchanged; the `cos-broken-relative.toml`
  negative control still exits 1 (it targets `cos)`, unaffected).
- A new agentctl unit test: lowering with `--mcp-dir X` rewrites every sidecar `args[0]` prefix to X and
  leaves task prose byte-identical (covers the F3 regression class in Rust, permanently).
- The `cos)` sed, the QEMU fork, and `distro_cos2b_topology` are untouched and green.
- `cargo build/clippy --workspace --all-targets -D warnings` clean; `cargo test --workspace` green. No `cargo fmt`.

## Risk
MEDIUM. Boot-path adjacent; the guard + docker-smoke are the safety net. The one real trap is D1 leaking
container-specific paths into the layout-agnostic `agentctl` binary — the recommended flag form avoids it.

## Explicitly NOT in scope (the par.2 wall + separate items)
- Docker↔QEMU config unification (structurally test-pinned — par.2, do-not-do).
- The `cos)`-mode sed retirement (static file, lower blast radius; only via a future build-time generator).
- The absent semantic-kb URL rewrite (AUDIT-v0.86:166 — separate cos)-side gap).
- port-7999 shared constant (trivial config dedup — separate).

---

## /autoplan — CEO dual voices (2026-07-26)

```
CEO DUAL VOICES — CONSENSUS TABLE
  Dimension                            Claude subagent   Codex          Consensus
  ─────────────────────────────────── ───────────────── ────────────── ──────────
  1. Premise valid (worth building)?   CONDITIONAL       NO (strike)    DISAGREE
  2. Right problem / right reframing?   agentctl+HARDtest strengthen sed DISAGREE
  3. Scope calibration (agent) only)?   OK, document it   awkward split  LEAN-OK
  4. Blast radius proportionate?        only w/ test      no             DISAGREE
  5. Priority vs UX tail?               BELOW UX tail     n/a            CONFIRMED below-UX
  6. Premises overstated?               YES (3)           YES (3)        CONFIRMED overstated
```

**Both agree:** zero user value; last in queue; rank **below** the UX tail; the plan's premises are
overstated ("compile-tested Rust proves path correctness" — no; "un-catchable by unit tests" — docker-smoke
is a per-PR gate; "no post-processing" — structured lowering is still post-processing).

**Codex — STRIKE.** Working first-party rewrite + fail-closed guard + CI smoke = retiring it is aesthetic
debt payment, not leverage. Moving into `agentctl` adds a CLI/config-layout contract that can brick Docker
boot "while pretending to be cleanup," and doesn't eliminate the rewrite (regex→Rust prefix logic). 10x-simpler:
leave the sed, strengthen the `agent` docker-smoke assertion (today it only checks `id="scout"`; also assert
`/etc/agentd/http_mcp.py` present + no `../docker/` or `/usr/lib/agentos/docker/` survives).

**Claude — BUILD only with a hard test, else STRIKE, rank below UX regardless.** Finding 1 (HIGH): the reshape
changes lowering for all 10 templates but CI positively pins only `scout` (dev-relative); the 3 installed-absolute
templates (`watcher`, `cron-agent`, `webhook-agent`) traverse the *other* rule with zero coverage — and a
surviving `/usr/lib/agentos/docker/…` is **absolute**, so `guard_no_relative_paths` is **blind** to it (fails
silently at sidecar startup). So: with an all-10-real-templates lowering test asserting zero surviving
`../docker/` AND `/usr/lib/agentos/docker/`, the reshape is strictly *safer* than the scout-only sed; without
that test it's *more* dangerous. Finding 3: the two prefix conventions are principled (installed-absolute is
correct for live `agentctl spawn watcher` on the distro where there's no `../docker` anchor) — normalizing to
one prefix is a trap; agentctl must strip BOTH. Lock D1(a) explicit `--mcp-dir`; reject ambient/anchor forms.

**Synthesis / the emergent third option:** both voices' cheap-win converges — the one real defect is the
installed-absolute blind spot in the *current* guard, and the one real asset is a template-iterating test.
Both can be harvested WITHOUT the boot-path refactor: **STRIKE the agentctl reshape, keep the sed, and add the
hardening test + docker-smoke assertion that closes the blind spot.** Closes par.3 as *hardened*, boot contract
untouched, ~1hr. → surfaced at the gate as the premise/User-Challenge decision (NOT auto-decided).

---

## Grounding (2026-07-26 sweep — full facts the decisions rest on)
- **Two boot-time sed pipelines, both in `docker/entrypoint.sh`:** `cos)` (6 rules, :239-246, static file
  `/etc/agentd/cos.agents.toml`→`/data/cos.agents.toml`) and `agent)` (2 rules, :353-356, on
  `agentctl spawn --dry-run` output). QEMU/distro boot uses **no sed** — it ships a pre-absolutized fork
  (`distro/overlay/etc/agentd/cos.agents.toml`) selected via kernel cmdline `agentd.config=`.
- **Three target layouts:** Docker (`/data`, `/etc/agentd`), QEMU (`/run/memory`, `/run/output`,
  `/usr/lib/agentos/docker`), dev/cargo (relative). Docker reaches its layout by sed; QEMU by forking.
- **`agent)` line-anchoring is load-bearing:** `/^args[[:space:]]*=/` prevents `AGENT_TASK`-prose
  corruption (h7.2-ar-01 F3).
- **Pinning tests:** `agentd/tests/cos_spawn_caps_subset.rs` (`distro_cos2b_topology` asserts the QEMU
  fork runs no `semantic-kb` sidecar — the structural pin); `agentd/tests/distro_packaging.rs`
  (every distro-referenced sidecar is packaged); CI `docker-smoke` (`ci.yml:450`) pins the rewritten
  output positively + negatively, incl. `.github/fixtures/cos-broken-relative.toml`.
- **No config-native `${VAR}`/envsubst exists** in agentd; `config.rs` has no expansion (only two
  `std::env::var` reads for management flags). par.2's /autoplan verdict: if unification is ever revived,
  do it as a **build-time generator** from one annotated source, never runtime `${VAR}` (loader-injection
  surface → PID-1 boot brick).
