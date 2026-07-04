# ma.2 — arm64 distro + HVF boot

**Status:** DEFERRED — final gate decision: ship ma.3 (Docker multi-arch) first, then ma.2
**Version target:** v0.57.0 (v0.56.0 was claimed by ma.3)
**Depends on:** ma.1 ✅ (v0.55.0)
**Branch:** `ma.2-arm64-distro`

---

## Goal

Make the pure-OS boot run fast on Apple Silicon. `make run ARCH=aarch64` boots agentd as PID 1
in a QEMU `virt` machine with HVF acceleration on an Apple Silicon Mac. The x86_64 path is
unchanged.

---

## Current state

`distro/Makefile` is fully x86_64-only:
- `QEMU := qemu-system-x86_64`
- `-kernel output/bzImage` (bzImage = x86_64 kernel image format)
- `-append "console=ttyS0"` (x86 serial console)
- `BR2_x86_64=y` baked into `distro/buildroot.config`
- Binary source path: `../target/x86_64-unknown-linux-musl/release/agentd`
- `overlay/usr/bin/agentd` target checks for x86_64 musl binary specifically

The aarch64 Rust binary exists (ma.1 shipped CI + Cross.toml), but the distro layer
has no ARM support whatsoever.

---

## Scope

### Files to create

1. **`distro/buildroot.aarch64.config`** — Buildroot defconfig for aarch64, mirrors
   `buildroot.config` with:
   - `BR2_aarch64=y` instead of `BR2_x86_64=y`
   - `BR2_LINUX_KERNEL_DEFCONFIG="defconfig"` (arm64 generic defconfig)
   - `BR2_LINUX_KERNEL_IMAGE=y` instead of `BR2_LINUX_KERNEL_BZIMAGE=y` (generates `Image`)
   - Same toolchain, packages, rootfs format, ccache settings as x86_64

2. **`distro/kernel-extras.aarch64.config`** — kernel config fragment for aarch64 `virt` machine:
   - Same 9P, virtio, FUSE, Landlock, seccomp, namespace flags as x86_64
   - Add `CONFIG_VIRTIO_MMIO=y` (ARM virt machine uses MMIO bus)
   - `CONFIG_SERIAL_AMBA_PL011=y` + `CONFIG_SERIAL_AMBA_PL011_CONSOLE=y` (UART for ttyAMA0)
   - No `CONFIG_KVM_INTEL`/`CONFIG_KVM_AMD` (x86 only; arm64 KVM is auto-selected differently)

### Files to modify

3. **`distro/Makefile`** — parameterize by `$(ARCH)` (default `x86_64`):

   ```makefile
   ARCH ?= x86_64

   # Per-arch config
   ifeq ($(ARCH),aarch64)
     BR_CONFIG       := $(CURDIR)/buildroot.aarch64.config
     KERNEL_IMAGE    := Image
     QEMU            := qemu-system-aarch64
     QEMU_MACHINE    := -M virt
     QEMU_CPU        := -cpu host
     CONSOLE         := ttyAMA0
     MUSL_TARGET     := aarch64-unknown-linux-musl
     OUTPUT_DIR      := output/aarch64
   else
     BR_CONFIG       := $(CURDIR)/buildroot.config
     KERNEL_IMAGE    := bzImage
     QEMU            := qemu-system-x86_64
     QEMU_MACHINE    :=
     QEMU_CPU        :=
     CONSOLE         := ttyS0
     MUSL_TARGET     := x86_64-unknown-linux-musl
     OUTPUT_DIR      := output
   endif

   # HVF on macOS, KVM on Linux, TCG fallback
   UNAME_S := $(shell uname -s)
   ifeq ($(ARCH),aarch64)
     ifeq ($(UNAME_S),Darwin)
       QEMU_ACCEL := -accel hvf
     else
       QEMU_ACCEL := $(shell kvm-ok >/dev/null 2>&1 && echo "-accel kvm" || echo "-accel tcg")
     endif
   else
     QEMU_ACCEL :=
   endif
   ```

   - x86_64 `OUTPUT_DIR` stays `output/` (no CI churn — existing size guards unaffected)
   - aarch64 `OUTPUT_DIR` is `output/aarch64/` (separate, co-exists cleanly)
   - Binary copy rules use `$(MUSL_TARGET)` to pick the correct cross-compile artifact
   - Buildroot build output goes to `build/output-$(ARCH)/` to avoid cross-contamination
   - `prereqs` target checks for the correct `$(QEMU)` binary

4. **`distro/overlay/init`** — no change needed. Console device (`ttyS0` vs `ttyAMA0`) is
   specified in QEMU's `-append` flag, not in the init script. The init script is arch-agnostic.

5. **`.github/workflows/ci.yml`** — add `distro-aarch64` job:
   - Validates Makefile parameterization via `make -n build ARCH=aarch64` (dry-run)
   - No actual Buildroot build in CI (30-60 min; x86_64 distro also not built in CI)
   - Documents the gap: full build + boot requires Apple Silicon Mac with HVF

6. **`docs/ROADMAP.md`** — mark ma.2 complete when shipped

7. **`CHANGELOG.md`** — v0.56.0 entry

---

## Key decisions

### D1: Output directory scheme
**Chosen: `output/` for x86_64 (unchanged), `output/aarch64/` for aarch64.**

Avoids breaking existing CI guards and local `make run` muscle memory for x86_64.
aarch64 output lands in a sibling directory; both can coexist on disk.
Buildroot's `O=` output dir parameterized as `build/output-$(ARCH)/`.

### D2: Buildroot config strategy
**Chosen: one file per arch** (`buildroot.config` + `buildroot.aarch64.config`).

Buildroot configs are opaque key=value defconfigs, not templates. Trying to share one
file would require fragile sed/awk patching. Two files are readable and independently
correct. Diff between them will be ~5 lines.

### D3: Kernel image format
- x86_64: `bzImage` (compressed, self-extracting; required by x86 BIOS/EFI)
- aarch64: `Image` (raw uncompressed; what QEMU `virt` expects for ARM64)

These are different binary formats — they cannot share a build step. QEMU `-kernel`
accepts `Image` directly for arm64 QEMU virt.

### D4: HVF/KVM/TCG accel detection
Makefile detects at build time: `$(shell uname -s)` → Darwin → `-accel hvf`;
Linux → check `kvm-ok` → `-accel kvm` or fall back `-accel tcg`.

For CI (Ubuntu, no KVM): `make -n` (dry-run) validates syntax without running QEMU.
The actual boot test is a local developer workflow.

### D5: CI strategy
**Chosen: dry-run validation only.** `make -n build ARCH=aarch64` confirms the Makefile
parameterization is syntactically correct and variable expansion works without triggering
a 60-min Buildroot cross-compilation. This is consistent with the x86_64 distro, which
also has no CI boot test today.

`distro/README.md` documents the `make run ARCH=aarch64` workflow for Apple Silicon
developers explicitly.

### D6: Console device
- x86_64 QEMU: `console=ttyS0` (ISA serial)
- aarch64 QEMU virt: `console=ttyAMA0` (PL011 UART on ARM virt board)

Kernel must have `CONFIG_SERIAL_AMBA_PL011_CONSOLE=y` for PL011; without it the guest
boots silently (no output). The init script is unaffected — console is specified via
QEMU `-append`.

### D7: virtio-net device
Keep `-device virtio-net-pci,netdev=net0` for aarch64. The QEMU `virt` machine has
PCIe, so virtio-net-pci works (requires `CONFIG_VIRTIO_PCI=y`, already present from
x86_64 config). Alternatively virtio-net-device (MMIO) also works; PCI is chosen for
consistency with x86_64.

### D8: gVisor/runsc on aarch64
gVisor has no aarch64 release. The universal-tier `which_runsc()` returns `None` on
aarch64 → graceful degradation (same behavior as documented in ma.1 known gaps).
No additional handling needed here.

### D9: seccomp DenySpawn on aarch64
`#[cfg(target_arch = "x86_64")]` gates DenySpawn — it is a no-op on aarch64.
Kernel `CONFIG_SECCOMP=y` still set in aarch64 kernel config (for other seccomp uses).
Documented gap, not a blocker.

---

## Files overview

```
distro/
  Makefile                      MODIFY (parameterize by ARCH)
  buildroot.config              unchanged (x86_64)
  buildroot.aarch64.config      CREATE (new)
  kernel-extras.config          unchanged (x86_64)
  kernel-extras.aarch64.config  CREATE (new)
  overlay/init                  unchanged (arch-agnostic)
  output/                       unchanged (x86_64)
  output/aarch64/               created by `make build ARCH=aarch64`
  README.md                     UPDATE (add ARCH=aarch64 instructions)
.github/workflows/ci.yml        MODIFY (add distro-aarch64 dry-run job)
docs/ROADMAP.md                 MODIFY (mark ma.2 done)
CHANGELOG.md                    MODIFY (v0.56.0)
```

---

## Acceptance criteria

1. `make build ARCH=aarch64` produces `distro/output/aarch64/Image` and
   `distro/output/aarch64/rootfs.cpio.gz` on an Apple Silicon Mac.
2. `make run ARCH=aarch64` launches `qemu-system-aarch64 -accel hvf` and agentd starts
   as PID 1 inside the VM.
3. `make build` (no `ARCH=`) produces `distro/output/bzImage` — x86_64 path unchanged.
4. `make -n build ARCH=aarch64` succeeds in CI (dry-run validates Makefile logic).
5. 1,096+ workspace tests still pass (no Rust code changes).

---

## NOT in scope

- CI full Buildroot build for aarch64 (30-60 min; same policy as x86_64)
- Boot test in CI (no API key; TCG too slow for 120s timeout)
- gVisor/runsc on aarch64 (no upstream aarch64 build)
- ma.3 multi-arch container images (separate increment)
- `make test ARCH=aarch64` passing with a real agent run in CI

---

## What already exists

| Sub-problem | Existing code |
|---|---|
| aarch64 Rust binary | ma.1: cross-compiled, tested, size-guarded |
| x86_64 Buildroot config | `distro/buildroot.config` |
| Kernel extras (x86_64) | `distro/kernel-extras.config` |
| 9P init script | `distro/overlay/init` — fully arch-agnostic |
| QEMU -virtfs 3-mount model | Makefile `QEMU_FLAGS` (secrets0/memory0/output0) |
| Binary size guards | CI `build-and-test` job (x86_64 only; aarch64 added in ma.1) |

---

## Implementation steps

1. Write `distro/buildroot.aarch64.config` — ~30 lines, diff from x86_64 is ~5 lines
2. Write `distro/kernel-extras.aarch64.config` — ~25 lines, add PL011 + VIRTIO_MMIO
3. Refactor `distro/Makefile` — add `ARCH` variable, arch branches, accel detection
4. Add `distro/README.md` Apple Silicon section (or update existing)
5. Add `distro-aarch64` CI job (dry-run only)
6. Bump version → v0.56.0, update CHANGELOG + ROADMAP

Total estimated CC time: ~25 minutes. No Rust code changes. Cargo.lock unchanged.

---

## Test plan

- `make -n build ARCH=aarch64` on macOS (validates Makefile without building)
- `make build ARCH=aarch64` on Apple Silicon Mac (full build, 20-30 min first time)
- `make run ARCH=aarch64` — verify QEMU boots, init runs, agentd starts
- `make build` (x86_64) — verify no regression
- `cargo test` in `agentd/` — 1,096 tests still pass (no Rust changes)

---

## CEO Review — Phase 1 Outputs

### Dream state delta

```
TODAY (post-ma.1):       aarch64 binary builds + CI-tested
THIS PLAN (ma.2):        aarch64 QEMU boot on Mac with HVF — developer loop only
12-MONTH IDEAL:          agentd on Pi/ARM server, native Docker ARM images, isolation-tier
                         honest reporting, real boot CI for both arches
GAP after ma.2:          Pi bare-metal boot absent; Docker ARM absent; isolation-tier
                         reporting absent; boot unverified in CI
```

### Error & Rescue Registry

| Error | Trigger | Severity | Rescue |
|---|---|---|---|
| Silent boot (no console output) | PL011 not enabled in kernel | High | Check kernel-extras.aarch64.config has `CONFIG_SERIAL_AMBA_PL011_CONSOLE=y` |
| virtio-net not coming up | virtio PCI vs MMIO mismatch | High | Verify `CONFIG_VIRTIO_PCI=y`; try `-device virtio-net-device` if PCI fails |
| `qemu-system-aarch64 not found` | QEMU not installed on Mac | Medium | `brew install qemu` |
| Buildroot cross-toolchain failure | Wrong aarch64 gcc target | High | Clean + rebuild; check `BR2_aarch64=y` in config |
| aarch64 binary not found in overlay | `make build ARCH=aarch64` before `cross build` | Medium | Run `cross build --release --target aarch64-unknown-linux-musl` first |
| HVF not available | macOS < 10.15 or Intel Mac | Medium | Remove `-accel hvf`, add `-accel tcg` for software emulation |
| 9P mount fails in guest | Security model mismatch | Medium | Verify `-virtfs ...,security_model=none` matches init's mount args |

### Failure Modes Registry

| Mode | Impact | Detection | Mitigation |
|---|---|---|---|
| Buildroot aarch64 config drifts from x86_64 config | Boot fails silently months later | No automated check | Add CI diff guard between configs (strips arch-specific lines) |
| CI dry-run passes but actual rootfs broken | "aarch64 boot green" claimed falsely | First real boot test | Rename acceptance criterion to "CI lint only; boot unverified" |
| gVisor templates run without gVisor on aarch64 | Silent degradation of isolation | No runtime warning | Emit startup warning when gVisor config detected without runsc |
| aarch64 DenySpawn is no-op silently | Users expect seccomp blocking | No user-facing indicator | ma.4 isolation-tier reporting should cover this |

### What already exists

See plan body §"What already exists" table.

### NOT in scope (deferred)

- Pi/ARM server bare-metal or SD-card boot image
- CI actual boot test (requires ANTHROPIC_API_KEY + 30-60 min Buildroot build)
- gVisor for aarch64 (no upstream release)
- Isolation-tier runtime reporting (ma.4)
- Docker multi-arch images (ma.3)

---

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| 1 | CEO | ma.2 before ma.3 | USER CHALLENGE | — | Both CEO voices independently flag this as mis-sequenced | ma.3 first (both models recommend) |
| 2 | CEO | CI dry-run only | TASTE | P3 (pragmatic) | x86_64 distro also has no CI boot test; consistent policy | Real TCG boot in CI (slow, needs API key) |
| 3 | CEO | Apple Silicon focus | TASTE | P6 (bias to action) | Developer DX benefit real even if Pi is the wedge | Pi bare-metal image (separate scope) |
| 4 | CEO | Two Buildroot configs | Mechanical | P5 (explicit) | Configs are opaque key=value; sharing would require fragile patching | Single parameterized config |
| 5 | Eng | bzImage target name | Bug fix | — | Hardcoded `build: $(OUTPUT_DIR)/bzImage` must become `$(OUTPUT_DIR)/$(KERNEL_IMAGE)`; aarch64 produces `Image` not `bzImage` | Leave hardcoded (would fail) |
| 6 | Eng | Overlay binary staleness | Bug fix | — | `overlay/usr/bin/agentd` cp must use `../target/$(MUSL_TARGET)/release/agentd`; add doc note that arch-switching requires `make clean` | Shared overlay (would silently bundle wrong arch binary) |
| 7 | Eng | -cpu host with TCG | Bug fix | — | TCG rejects `-cpu host`; need `-cpu cortex-a72` fallback when neither HVF nor KVM available | Leave as-is (would crash on Linux without KVM) |
| 8 | Eng | kvm-ok not portable | Bug fix | — | `kvm-ok` is Ubuntu-only; replace with `test -e /dev/kvm` | Keep kvm-ok (would silently fall to TCG on non-Ubuntu) |
| 9 | Eng | Buildroot O= hardcoded | Bug fix | — | Change `O=$(CURDIR)/build/output` → `O=$(CURDIR)/build/output-$(ARCH)` to prevent x86/aarch64 clobber | Leave hardcoded (would corrupt build on arch switch) |
| 10 | Eng | AC1/AC2 missing [local only] | Doc fix | — | Add `[local only, Apple Silicon]` qualifier to AC1+AC2; CI gate is AC4 only | Leave unmarked (confuses CI reviewers) |
| 11 | Eng | CI dry-run scope | Doc fix | — | Add `make -n run ARCH=aarch64` alongside `make -n build ARCH=aarch64` to cover more variable expansion | — |
| 12 | DX | ARCH= discoverability | Mitigated by scope | — | distro/README.md Apple Silicon section (step 5) covers this; no `make help` target required | — |
| 13 | Final gate | ma.2 vs ma.3 sequencing | USER CHALLENGE | — | User confirmed: swap to ma.3 first; ma.2 deferred until after ma.3 ships | Ship ma.2 now (two CEO voices; user agreed) |
| 14 | Final gate | CI aarch64 distro gate | Taste resolved | P3 | Dry-run only (`make -n build/run ARCH=aarch64`); consistent with x86_64 policy | Real Buildroot CI build (too slow) |
