<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260708-025145.md -->
# ma.4 — Isolation-Tier Honesty

**Track MA · Slot anytime · Estimated v0.67.0**

Depends on: ma.1 ✅, ma.2 ✅, ma.3 ✅

---

## Problem

AgentOS can now run on x86_64, aarch64 (Apple Silicon, Raspberry Pi, ARM cloud), and
in multi-arch Docker images. The system tracks *per-agent* tier (native vs. universal)
and *per-MCP-server* isolation (landlock/seccomp applied or not). But there is no
system-level probe of what isolation capabilities the **host device actually supports** —
and no honest reporting of that at startup or in the operator surfaces.

From `DEPLOYMENT-TOPOLOGY.md` §3, guardrail 2:
> "State the isolation tier per device — never let breadth outrun trust.
> 'Runs on a Pi' must ship with 'isolation tier: capability-only.'"

Currently a Raspberry Pi or a container without gVisor starts silently with no
indication that its isolation is degraded. An operator has no way to know whether
the device can enforce the full isolation floor or only parts of it.

---

## Goal

At startup, probe what isolation capabilities the device actually has, compute an
honest device-level "isolation tier" (`full` / `capability` / `none`), log a flight
event, and surface it in three places:

1. **`/agents/system/isolation`** — new FUSE virtual file (JSON, Linux-only; stub on macOS)
2. **`/api/v1/snapshot`** — the management API already serializes `SchedulerSnapshot`
3. **`agentctl watch` → System view** — show tier badge next to Sandbox line

---

## Isolation Tier Taxonomy

| Tier | Meaning | When |
|---|---|---|
| `full` | gVisor (runsc) + Landlock + seccomp all available | Linux with gVisor installed |
| `capability` | Landlock + seccomp available, no gVisor | Linux (most server/cloud images) |
| `none` | No kernel-enforced isolation | macOS, stock Pi kernel without seccomp, privileged containers |

---

## Scope

### 1. `agentd/src/isolation_caps.rs` (new, ~120 lines)

```rust
pub struct IsolationCaps {
    pub runsc:    Option<String>,   // "path/to/runsc" | null
    pub landlock: bool,
    pub seccomp:  bool,
    pub arch:     String,           // std::env::consts::ARCH
    pub tier:     String,           // "full" | "capability" | "none"
}

pub fn probe() -> IsolationCaps { ... }
```

Detection:
- `runsc`: call `which_runsc()` (already in `universal.rs`)
- `landlock` (Linux only, `#[cfg(target_os = "linux")]`): read
  `/proc/sys/kernel/landlock/status` or try
  `landlock_create_ruleset(null, 0, LANDLOCK_CREATE_RULESET_VERSION)` via
  `libc::syscall`; fallback = false on error/missing
- `seccomp` (Linux only): read `/proc/sys/kernel/seccomp/actions_avail`; any
  non-empty content = true; fallback = false
- `arch`: `std::env::consts::ARCH`
- Tier: `full` iff runsc + (landlock || seccomp); `capability` iff !runsc && (landlock || seccomp); else `none`

Emit `EventKind::IsolationProbed` flight event with full `IsolationCaps` JSON as data.

### 2. `surfaces/src/snapshot.rs`

Add to `SchedulerSnapshot`:
```rust
pub isolation_caps: Option<IsolationCaps>,
```

`IsolationCaps` (re-exported from `agentd` or mirrored in `surfaces` as a simple struct).
Since `surfaces` is a separate crate, define a mirror `IsolationCapsSummary` in `snapshot.rs`
with `runsc: Option<String>`, `landlock: bool`, `seccomp: bool`, `arch: String`, `tier: String`.
Derive `Serialize`, `Deserialize`, `Default`, `Clone`.

### 3. `surfaces/src/agents_fs.rs`

New system inode: `INO_SYS_ISOLATION` (next available after INO_SYS_EGRESS_ADDR).
New virtual file `/agents/system/isolation` with JSON content mirroring `IsolationCapsSummary`.
Wire into `readdir(INO_SYSTEM)` and `read()` + `getattr()` + `lookup()`.

### 4. `agentd/src/main.rs`

Call `isolation_caps::probe()` at startup (after existing sandbox setup, before management server starts).
Store result in `SchedulerSnapshot.isolation_caps`.
Emit `IsolationProbed` flight event.

### 5. `agentd/src/events.rs`

Add `IsolationProbed` to `EventKind` enum.
Add `"isolation_probed"` string in `kind_str()`.
Add to `event_taxonomy_completeness` test.

### 6. `agentctl/src/watch/reader.rs` + `source.rs`

Read `/agents/system/isolation` into `App.isolation_caps: Option<IsolationCaps>`.
`IsolationCaps` in agentctl = `{ tier: String, arch: String, runsc: bool, landlock: bool, seccomp: bool }`.
Parse from JSON. Wire into `FuseSource::load_snapshot()` and `HttpSource::load_snapshot()`.

### 7. `agentctl/src/watch/views.rs`

In `render_system()`, add after the Sandbox line:
```
  Isolation:  full   [runsc + landlock + seccomp]
```
or
```
  Isolation:  capability   [landlock + seccomp]
```
or
```
  Isolation:  none   [no kernel enforcement]
```
Color: `full` = green, `capability` = yellow, `none` = red.

In plain-mode output: `isolation_tier: full\nisolation_arch: aarch64\n`.

### 8. `docs/CONVENTIONS.md`

Add `isolation_probed` row to event taxonomy table.

---

## What is NOT in scope

- Detecting specific kernel versions or seccomp filter capabilities (probe result, not audit)
- Changing behavior based on detected tier (operator already chose their tier in config)
- Blocking startup on tier mismatch (advisory only — this is reporting, not enforcement)
- New FUSE agent-level files (per-device, not per-agent)
- Any changes to sandbox enforcement logic (stays in `sandbox/` crate)

---

## Acceptance Criteria

1. `agentd` emits `isolation_probed` flight event at startup with `tier`, `arch`, `runsc`,
   `landlock`, `seccomp` fields in `data`.
2. `/agents/system/isolation` returns valid JSON with those fields (Linux; stub on macOS).
3. `GET /api/v1/snapshot` JSON includes `isolation_caps` object.
4. `agentctl watch` System view shows `Isolation:` line with tier badge and capability flags.
5. `cargo test` passes; `make clippy-linux` clean (new `#[cfg]` code must pass).
6. 5+ new tests covering: `probe()` returns valid tier, FUSE file serializes correctly,
   plain-mode output includes isolation tier, tier classification logic (all 3 branches).

---

## Version

v0.67.0 — bumps from orch.1's v0.66.0.

---

## GSTACK REVIEW REPORT
<!-- /autoplan writes below this line -->

### CEO Review
- **Positioning**: sounds scoped, deliverable, fits the "DEPLOYMENT-TOPOLOGY §3 guardrail 2" promise.
- **Risk**: p7.6 already wires `AgentTier { Native, Universal }` per-agent; this adds a device-level orthogonal concept — zero conflict.
- **Taste decisions surfaced**: TD-1 (opt-in require_isolation_tier config key), TD-2 (timing: now vs defer), TD-3 (tier naming: `capability` vs `kernel-only`).

### Eng Review (dual voices — Claude + Codex)

Auto-decided:

| # | Decision | Rationale |
|---|---|---|
| D-6 | `IsolationCapsSummary` lives in `surfaces`; returned from `isolation_caps::probe()` by reference (agentd depends on surfaces, not vice-versa) | P4 DRY; surfaces must not depend on agentd |
| D-7 | seccomp field always false on aarch64 — `#[cfg(target_arch = "x86_64")]` gate | sandbox crate only enforces DenySpawn on x86_64; reporting seccomp=true on Pi would be dishonest |
| D-8 | Expose `sandbox::landlock_available() -> bool` (ABI ≥ 1, not ≥ 4) as `pub fn` instead of duplicating `libc::syscall` | avoids two independent unsafe blocks; ABI ≥ 1 = "Landlock present at all" |
| D-9 | `runsc: Option<String>` everywhere; agentctl renders as `runsc.is_some()` internally | prevents runtime JSON deserialization error when gVisor installed |
| D-10 | `probe()` called **before** FUSE mount, not just before management server | prevents window where agentctl reads stale Default values from FUSE |
| D-11 | Minimum 10 tests (up from 5) | both voices found 5 was insufficient for all FUSE patterns + tier logic table |
| D-12 | Tier logic: `full` = runsc AND landlock AND seccomp (AND, not OR) | aligns taxonomy table with code; aarch64+gVisor without seccomp → `capability`, not `full` |
| D-13 | `event_taxonomy_completeness` test must include `IsolationProbed` → `"isolation_probed"` | existing test asserts 1:1 — will fail if variant added but not listed |
| D-14 | `IsolationCapsSummary` in surfaces: NO `Deserialize` derive (surfaces = Serialize-only; agentctl defines its own parse struct) | surfaces pattern is write-only; unused derive sets wrong precedent |
| D-15 | macOS: `probe()` returns `{tier:"none",landlock:false,seccomp:false,runsc:null,arch:"aarch64"}` — FUSE stub emits same JSON (not ENOENT) | HTTP and FUSE paths return identical shapes; FuseSource on macOS reads file without special-casing |
| D-16 | Field named `isolation_caps` in `SchedulerSnapshot` (not `isolation_capabilities`) | `sandbox` = per-MCP-server process enforcement; `isolation_caps` = per-device kernel capability — distinct concepts, short name is fine |
| D-17 | `make clippy-aarch64` added to acceptance criteria | arch-conditional seccomp probe code diverges between arches |

### DX Review (Codex voice)

DX findings incorporated:
- **F1 (High)**: Tier formula inconsistency → resolved by D-12 (AND logic).
- **F2 (High)**: `capability` naming ambiguous → surfaced as taste decision TD-3.
- **F3 (Medium)**: `isolation_caps` / `runsc` inconsistency → resolved by D-9, D-16.
- **F4 (Medium)**: macOS stub underspecified → resolved by D-15.
- **F5 (Medium)**: No error on tier mismatch — accepted as out-of-scope (advisory-only is explicit in design).

### Taste Decisions (user gates)

**TD-1** — opt-in `require_isolation_tier` config key → **DEFERRED** (ma.4-ar-01 in TODOS). Advisory-only in v0.67.0. Add 1 legend line in `agentctl watch` explaining `capability` = kernel-sandbox-only (not Linux CAP_*).

**TD-3** — naming: `capability` vs `kernel-only` → **LOCKED: `capability`** (consistent with DEPLOYMENT-TOPOLOGY.md's existing `capability-only` wording; add 1-line legend in render_system).

---

### Updated Scope (reflects auto-decisions above)

1. **`sandbox/src/lib.rs`**: add `pub fn landlock_available() -> bool` (wraps existing `query_landlock_abi_version() >= 1`) + re-export `query_landlock_abi_version` as `pub fn landlock_abi_version() -> i64` if needed.

2. **`agentd/src/isolation_caps.rs`** (new, ~120 lines):
   ```rust
   // IsolationCapsSummary is defined in surfaces; this module returns it.
   use surfaces::snapshot::IsolationCapsSummary;

   pub fn probe() -> IsolationCapsSummary {
       let runsc = crate::universal::which_runsc().map(|p| p.display().to_string());
       #[cfg(target_os = "linux")]
       let landlock = sandbox::landlock_available();
       #[cfg(not(target_os = "linux"))]
       let landlock = false;
       #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
       let seccomp = std::fs::read_to_string("/proc/sys/kernel/seccomp/actions_avail")
           .map(|s| !s.trim().is_empty())
           .unwrap_or(false);
       #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
       let seccomp = false;
       let tier = if runsc.is_some() && landlock && seccomp { "full" }
           else if landlock || seccomp { "capability" }  // (or "kernel-only" — TD-3)
           else { "none" };
       IsolationCapsSummary {
           runsc,
           landlock,
           seccomp,
           arch: std::env::consts::ARCH.to_string(),
           tier: tier.to_string(),
       }
   }
   ```

3. **`surfaces/src/snapshot.rs`**: `IsolationCapsSummary { runsc: Option<String>, landlock: bool, seccomp: bool, arch: String, tier: String }` derives `Serialize, Default, Clone` (no Deserialize). Add `isolation_caps: Option<IsolationCapsSummary>` to `SchedulerSnapshot`.

4. **`surfaces/src/agents_fs.rs`**: `INO_SYS_ISOLATION = 18`; `/agents/system/isolation`; wire lookup/getattr/readdir/read. On macOS: stub returns probe() result JSON (all-false).

5. **`agentd/src/main.rs`**: call `isolation_caps::probe()` → store in `snapshot.isolation_caps` → emit `IsolationProbed` → **then** mount FUSE.

6. **`agentd/src/events.rs`**: `IsolationProbed`, `"isolation_probed"`, update `event_taxonomy_completeness` test.

7. **`agentctl/src/watch/reader.rs`**: define `struct IsolationCaps { tier: String, arch: String, runsc: Option<String>, landlock: bool, seccomp: bool }` with `Deserialize`; read `/agents/system/isolation`; wire `FuseSource::load_snapshot()` and `HttpSource::load_snapshot()`.

8. **`agentctl/src/watch/views.rs`**: add `Isolation:` line in `render_system()`. Active flags listed are whichever of `[runsc + landlock + seccomp]` are true.

9. **`docs/CONVENTIONS.md`**: `isolation_probed` row.

### Updated Acceptance Criteria

1. `agentd` emits `isolation_probed` flight event at startup with `tier`, `arch`, `runsc`, `landlock`, `seccomp` fields.
2. `/agents/system/isolation` returns valid JSON (Linux: actual probe; macOS: zeroed stub from probe()).
3. `GET /api/v1/snapshot` JSON includes `isolation_caps` object.
4. `agentctl watch` System view shows `Isolation:` line with tier badge and active flags.
5. `cargo test` passes (10+ new tests); `make clippy-linux` clean; `make clippy-aarch64` clean.
6. Tests must cover: tier classification (all 3 branches), seccomp=false on aarch64, `runsc: Option<String>` roundtrip, probe returns valid struct on macOS, FUSE JSON serializes correctly, `event_taxonomy_completeness`, plain-mode output, no-FUSE-ENOENT on macOS.
