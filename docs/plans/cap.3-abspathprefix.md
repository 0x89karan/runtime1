# cap.3 — path-capability matching is CWD-blind string-identity (audit86-P1-8)

Branch: `cap.3-abspathprefix` · Base: `main` (v0.106.0)
Kind: security/correctness capability fix. **Design-bearing** — the core question ("absolutize
against WHAT base?") is security-critical. This is the v0.86.2 root cause.

## The bug (audit86-P1-8)
`capability.rs` docs (`:27`) claim "**Absolute paths assumed. Relative paths fail-safe to deny.**"
The reality: `normalize_path` (`:91`) resolves `.`/`..` but KEEPS relative paths relative (its own
comment: "Relative paths are kept relative"). So a relative grant `FsWrite{"output"}` and a relative
request `write_file("output/x")` both normalize to `output` / `output/x`, and `satisfies` does
`norm_req.starts_with(norm_granted)` → **MATCH**. That is CWD-blind string-identity, NOT fail-closed:
- The same relative grant matches regardless of the process CWD (the grant and the request are never
  anchored to a real directory).
- `main.rs:265` only enforces an absolute `store_path` WHEN an MCP FS prefix is present — it does NOT
  require the FS-capability *prefixes* themselves to be absolute. Relative prefixes slip through.
Production dev-mode (`agentd/cos.agents.toml`) DEPENDS on relative prefixes (the boot sed rewrites them
to absolute only for the container/QEMU path; dev runs them relative). So the current behavior is
load-bearing for the dev workflow even though it's unsound.

## Design decisions (for the gauntlet — do NOT auto-decide D1)
- **D1 — absolutize against WHAT base? (the crux, security-critical).** A relative grant `output` must
  become `<base>/output` to be soundly matchable. And the REQUEST (`write_file("output/x")`) resolves
  at call-time against the process CWD. For matching to be sound, BOTH sides must anchor to the SAME
  base. Candidates:
    - **(A) Process CWD captured once at startup** — matches today's runtime reality (relative paths
      resolve against CWD), and if agentd's CWD is stable both sides agree. Risk: if anything `chdir`s,
      grant (load-time CWD) and request (call-time CWD) diverge.
    - **(B) The config file's directory** — deterministic + independent of runtime CWD, but the tool
      executes from the agent's CWD, so a request `output/x` (resolved against call-CWD) wouldn't match
      a grant anchored to config-dir unless they coincide.
    - **(C) Require absolute prefixes in config (reject relative at load), and absolutize REQUESTS at
      call-time against CWD.** Fail-closed for grants (matches the doc's claim), sound matching. Cost:
      breaks the dev workflow's relative configs unless dev configs move to absolute (or a dev-only
      base is configured).
    - **(D) A single explicitly-configured `[fs] root`/workspace base** that BOTH grant-absolutization
      and request-absolutization use — most sound (one anchor, CWD-independent), but adds config surface.
  The security goal: a relative grant must NOT silently authorize a path the operator didn't intend
  (CWD-blind over-grant), AND must not silently DENY a legitimate dev path (breaking dev). Recommendation
  to defend: **(A) capture CWD once at startup + absolutize both grant and request against it**, with a
  fail-closed assert that agentd's CWD doesn't change — preserves dev, makes matching sound, minimal
  config surface. Fall back to (C) if the "CWD is stable" assumption is judged too fragile.
- **D2 — where the absolutization happens.** A serde newtype `AbsPathPrefix` that absolutizes on
  deserialization (grants become absolute at config-load), + absolutizing the request in
  `required_capability_for` / `satisfies` (native.rs:104/144/185). Newtype makes the invariant
  type-enforced (a `Capability::FsRead` can't hold a non-absolute prefix). But serde deser doesn't
  have the base in context easily — may need a post-parse pass with the base injected. Decide:
  type-enforced-newtype vs a load-time absolutization pass.
- **D3 — backward-compat / dev workflow.** Whatever D1 picks must keep `agentd/cos.agents.toml` (dev,
  relative prefixes) working AND the container/QEMU path (already-absolute post-sed) working. A test
  must prove both. The boot-guard + the (dropped par.2) ERE are orthogonal — this is the RUNTIME
  capability match, not the boot config sanitizer.
- **D4 — the doc + `main.rs` conditional enforcement.** Update the `capability.rs` doc to state the
  ACTUAL post-fix semantics. Reconcile `main.rs:265`'s "only enforce absolute when MCP FS prefix
  present" with the new absolutization (does it become unconditional? does the newtype subsume it?).

## Acceptance criteria (draft)
- A relative FS-capability grant is anchored to a well-defined base (D1) at load; matching is
  absolute-vs-absolute, CWD-blind string-identity is gone. A test proves a relative grant does NOT
  authorize a path outside the intended (absolutized) prefix, and DOES authorize inside it.
- The dev workflow (relative `cos.agents.toml`) still works; the container/QEMU (absolute) path still
  works. Both pinned by tests.
- The `capability.rs` doc matches the real behavior. `main.rs` enforcement reconciled.
- `cargo build/clippy --workspace --all-targets -D warnings` clean; `cargo test --workspace` green.
  No `cargo fmt`. New tests: CWD-blind-over-grant is closed; dev-relative + prod-absolute both match.

## NOT in scope
- budget.1-ar-02 (reservation metering) — the other design-cluster increment, separate.
- The boot-config path sanitizer (par.2/par.3) — a different layer.
- audit86-P3-1 (credential UTF-8 truncation) — already shipped in hardening.1.

## Risk
~~MEDIUM-HIGH~~ **LOW-MEDIUM (reframed — see below).** Behavior-preserving; the only real risk is
the no-chdir invariant the anchor relies on. Pinned by tests on both sides.

---

## /autoplan RESOLVED (2026-07-25) — dual-voice Eng (Codex + Claude subagent). Both ADJUST + reframe.

**REFRAMING (both models, verified against the code): there is NO live FS-escape exploit.** A relative
grant confines to its subtree exactly as an absolute one does — `output/../secret` normalizes to
relative `secret` → `starts_with("output")` = false → denied; `/etc/passwd` → absolute `RootDir` ≠
`Normal("output")`, component-wise → denied; the empty-prefix-matches-everything hole is ALREADY closed
(`!is_empty()` guard, capability.rs:159/171). The genuine defects are (a) the doc (`capability.rs:27`)
"Relative paths fail-safe to deny" is **FALSE** — they match by relative string-identity; (b)
**CWD-blindness is a latent footgun** — the same config authorizes a *different absolute directory*
depending on invocation CWD, so an operator can't reason about the authorized region. This increment =
"make matching sound + make the doc true + kill CWD-blindness," NOT exploit closure. Operator chose to
build it.

### Design (settled)
- **D1 = A: freeze the process CWD once at startup, absolutize BOTH grant and request against it.**
  Critically **behavior-preserving** — absolutizing both sides prepends a constant clean prefix; every
  allow/deny decision is identical to today (verified: exact, sub-path, `..`-escape, absolute-req/
  relative-grant, relative-req/absolute-grant — all unchanged). In-tree precedent: `main.rs:1046-1064`
  already absolutizes `evidence_path`/`egress_key_path` against `current_dir()` this way. The anchor
  must be the base *execution* uses (live CWD) — verified prod agentd NEVER chdirs (the only
  `set_current_dir` is a checkpoint TEST, scheduler.rs:5565; scheduler.rs:294 already documents the
  no-chdir assumption). B/C/D rejected (B: config-dir ≠ launch-dir; C: breaks checked-in dev config;
  D: `[fs] root` adds config surface against the "light" ethos, unjustified by no live threat).
- **D2 = single chokepoint in `satisfies`, NOT the `AbsPathPrefix` newtype.** serde CAN'T see the base
  (buries an env/FS dep in deserialization, breaks pure `toml::from_str` unit tests) and only covers
  the GRANT half — the REQUEST side (native.rs:104/142/183) is built programmatically, never via serde.
  A newtype ripples across config-load/native/spawn-attenuation/template/FUSE for zero safety over a
  one-line chokepoint. Instead: keep `prefix: String`; add `fn anchor_abs(p: &Path) -> PathBuf` reading
  a startup-captured `OnceLock<PathBuf>` CWD anchor, applied to BOTH granted and required prefixes
  inside the two FS arms of `satisfies` (capability.rs:152-176). Both request+grant flow through
  `satisfies`, so ONE edit covers both halves + fixes spawn-attenuation (`capability_covered_by →
  satisfies`) consistently. Add a `set_anchor_for_test(PathBuf)` seam so tests stay pure.
- **D4 = reconcile + rewrite the doc.** `main.rs:265` conditional STAYS (it decides *whether* to run
  the store/prefix containment, orthogonal to the base) — but absolutize `store_path` + the MCP prefixes
  against the SAME anchor before the containment check (main.rs:259/280/498/511/1069/1076); the
  `is_absolute()` requirement becomes redundant-but-harmless (keep as a clear error). Rewrite the
  `capability.rs:20-37,88` doc to the real post-fix semantics ("grant + request absolutized against
  agentd's startup working dir; absolute-vs-absolute; agentd must not chdir"). NOTE (acknowledge, don't
  necessarily change): `template.rs:200-210` ALREADY require-absolute for `fs_read`/`fs_write` (option
  C) — but `[[agents]].capabilities` goes through config.rs, not template.rs, so the dev relative cap
  bypasses it. The codebase already contains BOTH policies; call out the deliberate split.

### Tests
- relative dev grant (`./output`) allows an in-subtree request + denies an out-of-subtree one, AND its
  normalized prefix is now ABSOLUTE (`<startup_cwd>/output`).
- prod absolute grant (`/run/output`) unchanged (allow/deny identical).
- mixed relative-grant/absolute-request (and vice versa) still deny.
- pin the no-chdir invariant (assert-once, or a test that a chdir would diverge — documented).
- the two fixtures already exist: `agentd/cos.agents.toml:410` (`./output` dev) + distro (`/run/output`).
