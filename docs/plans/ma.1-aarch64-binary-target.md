<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260704-181636.md -->
# ma.1 — aarch64 Binary Target

**Version:** v0.55.0 (proposed)
**Branch:** ma.1
**Depends on:** nothing (core is arch-agnostic today)
**Track:** MA (multi-arch reach)
**Size:** small-medium (CI config + cross-build wiring, no core behavior change)

---

## Goal

`agentd` and `agentctl` build and test for `aarch64-unknown-linux-musl`. This is the
binary prerequisite for ma.2 (ARM64 distro + HVF boot on Apple Silicon), ma.3
(multi-arch container images), and ma.4 (isolation-tier reporting). The aarch64 binary
is the thing that can run on a $60 Raspberry Pi or a Graviton cloud instance.

---

## Background / Current State

- CI builds `x86_64-unknown-linux-musl` today (via `rustup target add x86_64-unknown-linux-musl`)
- The `sandbox` crate already guards seccomp-bpf behind `#[cfg(target_arch = "x86_64")]`
  (fixed in p4.5); on aarch64 `DenySpawn` is a no-op returning `Ok(())`
- Landlock syscall numbers (444-446) are identical on x86_64 and aarch64 — Landlock
  FS rules work on aarch64 with no code change
- No aarch64 CI runner exists today (TODOS.md P4 flagged this; deferred until now)
- `cross` (cargo install cross) is the standard Rust tool for cross-compilation via Docker

---

## Scope

### What's in

1. **Cross-compilation target** — add `aarch64-unknown-linux-musl` via `cross` in CI
   - `rustup target add aarch64-unknown-linux-musl` + cross-linker in CI
   - Use `cross` (Docker-based) for aarch64 builds to avoid needing a native linker
   - Both binaries: `agentd` and `agentctl`

2. **Multi-arch CI matrix** — extend `ci.yml` with an aarch64 build+test job
   - Separate job (not a matrix — different tool chain required)
   - `cross build --target aarch64-unknown-linux-musl --release`
   - `cross test --target aarch64-unknown-linux-musl` (runs tests inside QEMU)
   - Cache cross build artifacts in GitHub Actions

3. **Per-arch size guard** — ≤ 6 MB for both x86_64 and aarch64 binaries
   - Existing x86_64 guard stays unchanged
   - New aarch64 guard added after cross build

4. **Document arch-conditional gaps** — in CLAUDE.md and CONVENTIONS.md
   - seccomp DenySpawn: confirmed no-op on aarch64 (capability-layer only)
   - IsolateNetwork / IsolateMount: `unshare` works on aarch64 kernels with user namespaces
   - Landlock: same syscall numbers, same behavior
   - `EnforcementStatus.spawn_enforcement` = `"none"` on aarch64 (already correct)

5. **TODOS.md P4 resolution** — mark the aarch64 CI runner item complete

### What's NOT in

- arm64 Buildroot distro / QEMU HVF boot → ma.2
- Multi-arch container images → ma.3
- Isolation-tier detection → ma.4
- Any change to the core scheduler, inference, memory, or tool subsystems
- macOS aarch64 (Darwin) cross-compile — Linux-musl target only

---

## Implementation Plan

### Step 1: Add cross to CI
Install `cross` in the CI job. Cross uses Docker internally to run the musl
cross-compiler toolchain for aarch64.

```yaml
- name: Install cross
  run: cargo install cross --locked
```

### Step 2: Add aarch64 build job to ci.yml
New job `build-aarch64` parallel to `build-and-test`. Include `timeout-minutes: 45`
because QEMU-emulated tests run 3-5x slower than native:

```yaml
build-aarch64:
  runs-on: ubuntu-latest
  timeout-minutes: 45
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - uses: Swatinem/rust-cache@v2
      with:
        workspaces: |
          agentd
          agentctl
        key: aarch64
    - name: Install cross (pre-built binary — faster than cargo install)
      uses: taiki-e/install-action@v2
      with:
        tool: cross
    - name: Build agentd (aarch64 musl)
      run: cross build --release --target aarch64-unknown-linux-musl
      working-directory: agentd
    - name: Build agentctl (aarch64 musl)
      run: cross build --release --target aarch64-unknown-linux-musl
      working-directory: agentctl
    - name: Clippy agentd (aarch64)
      run: cross clippy --target aarch64-unknown-linux-musl -- -D warnings
      working-directory: agentd
    - name: Clippy agentctl (aarch64)
      run: cross clippy --target aarch64-unknown-linux-musl -- -D warnings
      working-directory: agentctl
    - name: Test agentd (aarch64, QEMU)
      run: cross test --target aarch64-unknown-linux-musl
      working-directory: agentd
    - name: Test agentctl (aarch64, QEMU)
      run: cross test --target aarch64-unknown-linux-musl
      working-directory: agentctl
    # Size guard runs from repo root (no working-directory) — cargo workspace puts
    # aarch64-musl artifacts at <repo-root>/target/, same as the x86_64 guard above.
    - name: Binary size guard — agentd aarch64 ≤ 6 MB
      run: |
        SIZE=$(stat -c %s target/aarch64-unknown-linux-musl/release/agentd)
        echo "agentd aarch64: ${SIZE} bytes"
        [ "${SIZE}" -le 6291456 ] || { echo "ERROR: exceeds 6 MB"; exit 1; }
    - name: Binary size guard — agentctl aarch64 ≤ 6 MB
      run: |
        SIZE=$(stat -c %s target/aarch64-unknown-linux-musl/release/agentctl)
        echo "agentctl aarch64: ${SIZE} bytes"
        [ "${SIZE}" -le 6291456 ] || { echo "ERROR: exceeds 6 MB"; exit 1; }
```

### Step 3: Add `Cross.toml` to workspace root
Pin the cross Docker image version to avoid breakage when cross releases a new image:

```toml
# Cross.toml — workspace root
[target.aarch64-unknown-linux-musl]
image = "ghcr.io/cross-rs/aarch64-unknown-linux-musl:0.2.5"
```

This ensures `ring` (which needs `aarch64-linux-musl-gcc` during `build.rs`) always gets
the correct toolchain. The `ring` 0.17 build is known-working on this image.

### Step 4: Verify no arch-conditional compilation errors
Run `cross check --target aarch64-unknown-linux-musl` locally before CI to catch
any hidden `cfg`-gated code that fails to compile on aarch64.

### Step 5: Add `make clippy-aarch64` to Makefile
Mirror the existing `make clippy-linux` target. Include a Docker preflight so the error
is actionable when Docker is not running:

```makefile
.PHONY: clippy-aarch64
clippy-aarch64:
	@docker info >/dev/null 2>&1 || { echo "ERROR: Docker must be running for aarch64 cross-compilation (cross uses Docker+QEMU)"; exit 1; }
	cd agentd && cross clippy --target aarch64-unknown-linux-musl -- -D warnings
	cd agentctl && cross clippy --target aarch64-unknown-linux-musl -- -D warnings
```

### Step 6: Update docs
- CLAUDE.md: update "Linux-gated code" section — require `make clippy-aarch64` before
  pushing any code that changes `#[cfg(target_arch)]`-gated behavior; document Docker +
  `cross` prerequisites; note purpose of `Cross.toml`
- TODOS.md: resolve P4
- CHANGELOG.md: v0.55.0 entry

---

## Acceptance Criteria

1. `cross build --release --target aarch64-unknown-linux-musl` succeeds for both `agentd` and `agentctl`
2. `cross test --target aarch64-unknown-linux-musl` passes (all tests) for both crates
3. Both aarch64 binaries ≤ 6 MB (verified by CI size guard)
4. x86_64 CI job unmodified and still passes
5. TODOS.md P4 marked resolved
6. No behavior change to the running system (pure build-target + CI addition)

---

## Known Arch Gaps (documented, not fixed in ma.1)

| Feature | x86_64 | aarch64 | Notes |
|---|---|---|---|
| DenySpawn (seccomp-bpf) | Full | No-op | `#[cfg(target_arch="x86_64")]` guard; clone3 inspection deferred to ma.4 |
| Landlock FS | Full | Full | Same syscall numbers (444-446) on both arches |
| IsolateNetwork | Full | Full | unshare works on capable kernels |
| IsolateMount | Full | Full | same |
| Landlock V4 net | Full | Full | same ABI |
| gVisor/runsc (universal tier) | Full | No-op (returns Err) | runsc has no aarch64 build; `which_runsc()` returns None; BestEffort behavior |

The `EnforcementStatus.spawn_enforcement` field already returns `"none"` on non-x86_64.
ma.4 will surface this gap honestly via the isolation-tier reporting API.

---

## Test plan

- `cross test` runs the full workspace test suite under QEMU (aarch64) in CI
- All 1096 existing tests must pass on aarch64 (same behavior, different arch)
- No new tests needed for this increment (pure build wiring)

---

## Files Changed

- `.github/workflows/ci.yml` — add `build-aarch64` job (with `taiki-e/install-action`, `cross clippy`, size guard from repo root, `timeout-minutes: 45`)
- `Cross.toml` — new; pins aarch64-unknown-linux-musl image version for `ring` compat
- `Makefile` — add `clippy-aarch64` target with Docker preflight (mirrors `clippy-linux`)
- `CLAUDE.md` — update "Linux-gated code" section: add aarch64 clippy gate, `cross` prerequisite, Docker requirement, `Cross.toml` purpose
- `TODOS.md` — resolve P4
- `CHANGELOG.md` — v0.55.0 entry

---

## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|---------------|-----------|-----------|---------|
| 1 | CEO | `cross` as cross-compile tool | Mechanical | P3 | Industry standard for Rust aarch64 musl cross-compile | `cargo-zigbuild`, native linker |
| 2 | CEO | Size guard ≤ 6 MB for aarch64 | Mechanical | P1 | Consistent with x86_64 guard; ARM binary will be similar size | Separate limit |
| 3 | CEO | DenySpawn documented as no-op, not fixed in ma.1 | Mechanical | P5 | Fix belongs in ma.4 (isolation-tier reporting), not here | Implement aarch64 seccomp now |
| 4 | CEO | QEMU-emulated `cross test` accepted as CI gate | Taste | P3 | Best available without ARM runner; catches compile + logic bugs | Native ARM runner (ubuntu-24.04-arm) |
| 5 | Eng | **[BUG FIX]** Remove `working-directory: agentd` from size guard steps | Mechanical | P5 | Cargo workspace puts artifacts at `<repo-root>/target/`, not `agentd/target/`; size guard must stat from repo root | Leave bug in |
| 6 | Eng | Use `taiki-e/install-action@v2` for `cross` install | Mechanical | P3 | Saves ~5 min per CI run vs. `cargo install cross --locked` | cargo install |
| 7 | Eng | Add `cross clippy --target aarch64-unknown-linux-musl` step | Mechanical | P1 | Matches CLAUDE.md policy; catches aarch64-specific lints | Skip clippy on aarch64 |
| 8 | Eng | Add `Cross.toml` pinning image version for `ring` compat | Mechanical | P5 | `ring` needs aarch64-linux-musl-gcc; pinning prevents silent breakage on image updates | Float to latest |
| 9 | Eng | Add gVisor/runsc to Known Arch Gaps table | Mechanical | P1 | `runsc` has no aarch64 build; `which_runsc()` returns None; BestEffort already correct | Hide the gap |
| 10 | DX | Add `timeout-minutes: 45` to `build-aarch64` CI job | Mechanical | P1 | QEMU tests run 3-5x slower; without timeout CI hangs indefinitely on test deadlock | Accept infinite CI wait |
| 11 | DX | Add `make clippy-aarch64` Makefile target | Mechanical | P3 | Mirrors `make clippy-linux`; developer needs a local command to vet aarch64 changes without reading CI output | Only use CI for aarch64 clippy |
| 12 | DX | CLAUDE.md "Linux-gated code" update to include aarch64 gate + Docker + `cross` prereqs | Mechanical | P5 | Without this, developer pushes aarch64 cfg-gated code without a local gate → aarch64-only CI failures | Leave CLAUDE.md silent on aarch64 |
| 13 | DX | Defer TTHW (first-time experience) documentation to ma.2 | Taste | P3 | ma.1 is CI wiring; ma.2 brings a concrete device target where TTHW becomes meaningful | Document TTHW now in ma.1 |
| 14 | Gate | ma.4 isolation-tier reporting deferred — ma.1 ships pure build wiring only | Taste | P3 | Independent concerns; bundling raises diff complexity and delays the aarch64 binary | Bundle ma.4 into ma.1 |
| 15 | Gate | QEMU emulation (`cross test`) chosen over native ARM runner | Taste | P3 | Works today with no billing changes; `ubuntu-24.04-arm` beta availability unconfirmed for this repo | Native ARM runner |

