# DEPLOYMENT-TOPOLOGY — multi-arch reach + multi-instance coordination

**Status:** design + increment plan (planning session, 2026-07). Not yet built. Companion to
`docs/DESIGN.md` (thesis), `docs/PRODUCT-THESIS.md` (positioning), `docs/ROADMAP.md` (queue).
**Purpose:** outline the direction so `/autoplan` (or `/plan-eng-review` per item) can build it
out in the increments defined in §4. Fold these into `ROADMAP.md` when picked up in the build
session.

---

## 0. TL;DR + the vision stance

Two moves, one direction: make AgentOS **run on the devices people own** (multi-arch) and
**coordinate many instances** (coordination).

**This fulfills the vision, it does not dilute it.** "Agent as the OS primitive on a thin,
owned, remote-cognition host" *requires* running where the primitives run. A single-arch OS is
nearly a contradiction; Linux is an OS *because* it runs on x86, ARM, RISC-V. The locked
decisions (**super-light + remote cognition**) were portability theses all along, and the most
on-thesis device — a $60 owned box in your closet running your chief of staff 24/7, data never
leaving except to the model API — **literally cannot exist without aarch64.** Multi-arch is the
line between "a cloud service you rent" (drifts toward the SaaS you define yourself against) and
"an OS you own on your own hardware" (the wedge).

**The guardrail that keeps it the vision:** *arch and hypervisor are build-target details;
they never leak into the agent model.* "agentd is PID 1 of whatever userspace it inhabits —
x86 VM, ARM VM, container, or bare metal."

---

## 1. Device matrix

The killer property: **remote cognition means the device only runs a ~4-6 MB binary and reaches
a model API.** A device that could never host a 7B model hosts agents fine.

| Device | Arch | What it gives | Isolation tier* |
|---|---|---|---|
| Linux server / VPS / cloud VM | x86_64 | Home turf; QEMU+KVM (PID 1) or container | Full |
| Apple Silicon Mac | aarch64 | Fast OS boot (HVF), daily dev machine | Full (in VM) |
| Raspberry Pi 4/5, SBCs | aarch64 | **Owned, always-on CoS on a $60 box** — the wedge, physical | Degraded on stock kernels |
| ARM cloud (Graviton/Ampere/Axion/Cobalt) | aarch64 | ~20-40% cheaper per-perf; the fleet | Full |
| NAS / ARM home servers | aarch64 | 24/7 owned personal-ops host | Varies |
| Jetson / ARM edge | aarch64 | Edge agent host (cognition still remote) | Varies |
| Intel Mac / Windows | x86_64 | via Docker/WSL2 Linux VM | Full (in VM) |
| Phone / tablet | — | Client to a remote instance, not a host | n/a |

*Isolation tier: **Full** = microVM/gVisor + Landlock/seccomp; **Degraded** = capability layer
only (thin/stock kernel lacks the floor). See §3 guardrail 2.

**The two hard floors multi-arch does NOT remove:** (1) **network to a model API** (remote-
cognition lock — nothing offline); (2) a **Linux userspace** (native / container / VM). Phones,
tablets, and air-gapped devices stay out. Multi-arch widens the range *within* these floors.

---

## 2. Orchestration model (multiple instances)

**AgentOS already has the coordination primitives — they operate *inside* one instance today.**
Multi-instance = federate them across the instance boundary.

Built (intra-instance): the **A2A bus** (`send_message`, Agent Cards for discovery), `spawn_agent`,
checkpoint/restore, the detachable memory volume, and the **management API (p7.7)** which makes
each instance network-addressable. That last one is the substrate for cross-instance control.

### The topology ladder
1. **One instance** — one device, one trust domain, many mutually-trusting agents. *(today)*
2. **Multi-instance, one host** — N single-tenant instances on one machine (e.g. per project/
   trust-domain); needs a local supervisor.
3. **Multi-device mesh** — instances across laptop + home server + cloud, coordinating.
4. **Multi-tenant fleet** — many *owners'* instances (the enterprise/CSP case).

### The trust fork (decides the control plane)
- **Your mesh (2 & 3):** all instances are yours → mutually trusting → a **lightweight
  AgentOS-native coordinator** (talks each instance's `:7999` API + a federated A2A bus).
- **Fleet of different owners (4):** mutually *distrusting* → tenancy boundary = the microVM
  (Firecracker model) + a control plane that does not trust tenants → heavier, K8s/CRD-shaped.
  **Defer** until an enterprise case demands it.

### The load-bearing architecture — separate compute from memory
> **AgentOS instances are the compute** (light, portable, per-device, ephemeral). The **memory
> sidecar (h8.1 HelixDB)** is the **shared, durable brain.** A device spins up an instance, works,
> checkpoints, and can vanish; memory + the context graph persist in the sidecar and are shared.

Devices run ephemeral compute; the memory sidecar is shared state; the management API + a
federated A2A bus are the coordination plane. This is *why h8.1 matters for coordination* — it's
the shared brain a mesh coordinates through, not just a smarter brief.

---

## 3. Guardrails (preserve the vision while widening reach)

1. **Arch/hypervisor never leak into the core.** No `if docker`, no arch-conditional *concepts*
   in the runtime. The isolation layer's unavoidable `#[cfg(target_arch)]` branches stay contained.
2. **State the isolation tier per device — never let breadth outrun trust.** "Runs on a Pi" must
   ship with "isolation tier: capability-only." Breadth widening faster than the trust guarantees
   is the real dilution risk for a security-first product.
3. **The CoS harness stays the flagship.** Device breadth + fleet are substrate; they don't
   displace the product.
4. **Every arch's boot is CI-tested, or it rots.** The x86 QEMU CI silently broke once. An
   untested aarch64 boot will do the same. "aarch64 boot green in CI" is a first-class requirement.

---

## 4. Increments (`/autoplan`-ready)

Each is a self-contained unit with goal / depends-on / scope / acceptance — the ROADMAP style.
Run `/autoplan` (or `/plan-eng-review`) per increment. Two tracks: **ma.\*** (multi-arch reach),
**mesh.\*** (coordination).

### Track MA — multi-arch reach

**ma.1 — aarch64 binary target**
- *Goal:* `agentd`/`agentctl` build + test for `aarch64-unknown-linux-musl`.
- *Depends on:* nothing (core is arch-agnostic today).
- *Scope:* add the cross target; multi-arch CI matrix (x86_64 + aarch64); per-arch size guard
  (≤ 6 MB each); resolve any arch-conditional gaps (e.g. the existing aarch64 seccomp no-op).
- *Acceptance:* both binaries build + tests pass in CI; sizes documented; no core behavior change.

**ma.2 — arm64 distro + HVF boot**
- *Goal:* the pure-OS boot runs fast on Apple Silicon.
- *Depends on:* ma.1.
- *Scope:* arm64 Buildroot config; parameterize `distro/Makefile` + configs by `$ARCH`;
  `qemu-system-aarch64 -M virt -accel hvf -cpu host`; keep x86_64 path intact.
- *Acceptance:* `make run ARCH=aarch64` boots agentd as PID 1 near-native on an Apple-Silicon
  host; **aarch64 boot green in CI** (ARM runner or emulated); x86_64 boot unchanged.

**ma.3 — multi-arch container images**
- *Goal:* `docker pull` runs native on ARM Macs / ARM cloud.
- *Depends on:* ma.1.
- *Scope:* multi-arch manifests (`linux/amd64` + `linux/arm64`) for `agentos:core`/`:full`,
  published from CI to ghcr.
- *Acceptance:* `docker run` pulls the native-arch image on both an x86 and an ARM host; no
  emulation warning.

**ma.4 — isolation-tier detection + honest reporting**
- *Goal:* every deployment states which isolation tier is actually active.
- *Depends on:* ma.1 (independent of ma.2/3).
- *Scope:* at startup, detect the available floor (microVM/gVisor vs. capability-only vs.
  Landlock/seccomp availability by arch+kernel); emit a flight event + expose via the management
  API + `agentctl`; a one-line startup log.
- *Acceptance:* on a full-kernel host it reports "Full"; on a stock-Pi-class kernel it reports
  "capability-only" with the specific missing pieces; the info is visible in `agentctl`.

### Track MESH — multi-instance coordination

**mesh.1 — instance identity + fleet registry**
- *Goal:* instances are discoverable by a coordinator.
- *Depends on:* p7.7 (management API).
- *Scope:* stable instance identity; a lightweight registry (static config or discovery) mapping
  instance → its `:7999` endpoint + Agent Cards.
- *Acceptance:* a coordinator can enumerate N instances and reach each one's management API.

**mesh.2 — federated A2A bus**
- *Goal:* agents on different instances/devices discover + message each other.
- *Depends on:* mesh.1, p1.5/p1.6 (bus + Agent Cards).
- *Scope:* extend `send_message`/`list_agents` across the instance boundary over the management
  API; cross-instance Agent Card discovery; addressing (`agent@instance`).
- *Acceptance:* agent A on instance X messages agent B on instance Y; the exchange is recorded on
  both; discovery lists remote agents.

**mesh.3 — shared memory sidecar**
- *Goal:* the h8.1 memory sidecar is network-addressable and shared across instances (compute/
  memory separation).
- *Depends on:* h8.1 (HelixDB memory sidecar).
- *Scope:* run the sidecar as a shared service (not laptop-local); instances point at it over the
  HTTP/SSE transport; provenance carries the writing instance.
- *Acceptance:* two instances read/write one shared memory/context graph with correct provenance.

**mesh.4 — `agentctl mesh` (lightweight coordinator)**
- *Goal:* one operator view + control over a mesh of your instances.
- *Depends on:* mesh.1, ma.4.
- *Scope:* `agentctl mesh` lists instances, their agents, spend, status, isolation tier (via each
  `:7999` API); place/spawn/message across the mesh; the "my trusting mesh" control plane.
- *Acceptance:* from one terminal, see + act on all your instances across devices.

**mesh.5 — agent migration across instances** *(= h8.3)*
- *Goal:* move a running agent between instances/devices.
- *Depends on:* mesh.1-3, p3.2 (checkpoint), p5.3.5 (memory volume).
- *Scope:* serialize checkpoint + memory reference into a portable artifact; restore on another
  instance; identity continuity.
- *Acceptance:* an agent mid-task on instance X resumes on instance Y with state intact.

**mesh.6 — multi-tenant fleet control plane** *(deferred / enterprise)*
- *Goal:* orchestrate many *owners'* instances safely.
- *Depends on:* mesh.1-4, the microVM isolation floor (ma.2 / p7.6).
- *Scope:* microVM-per-tenant + a distrusting control plane (K8s operator/CRD or equivalent);
  per-tenant identity, quotas, audit isolation.
- *Acceptance:* two mutually-distrusting tenants' instances run isolated under one control plane.
- *Note:* heaviest, enterprise-only. Do not build until a concrete enterprise case demands it.

---

## 5. Dependency spine + sequencing

```
ma.1 aarch64 binary ─┬─▶ ma.2 arm64+HVF boot (fast OS on Mac/ARM)
                     ├─▶ ma.3 multi-arch images
                     └─▶ ma.4 isolation-tier reporting
p7.7 mgmt API ──▶ mesh.1 registry ─┬─▶ mesh.2 federated A2A ─┐
                                   └─▶ mesh.4 agentctl mesh ├─▶ mesh.5 migration
h8.1 memory sidecar ──▶ mesh.3 shared memory ───────────────┘
                                                             mesh.6 multi-tenant (deferred)
```

**Recommended order:** ma.1 → ma.2 (unlocks fast OS on your Mac + ARM devices) → ma.4 (honesty) →
ma.3 (images). In parallel once p7.7 + h8.1 land: mesh.1 → mesh.2/mesh.3 → mesh.4 → mesh.5.
mesh.6 deferred. **Fold into `ROADMAP.md` at pickup;** flag the ma.\* arch decision **before
dx.3/dx.4 freeze x86-only** (parameterize the deployment by arch from the start, not retrofit).

---

## 6. Using this with `/autoplan`

Each §4 increment is a self-contained unit: `/autoplan <increment>` (CEO→Eng→DX) for the
architectural ones (ma.2, mesh.2, mesh.3, mesh.6), `/plan-eng-review` for the mechanical ones
(ma.1, ma.3, ma.4, mesh.1). Keep one increment per branch; `main` stays shippable; each carries
its own tests + the guardrails from §3 (arch out of core; isolation-tier honesty; CoS flagship;
arch boot CI-tested). Build the CoS/harness/memory tracks to a stable point first — this is
substrate reach, not the flagship.
