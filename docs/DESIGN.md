# AgentOS — Design & Research Document

> **Working name:** *AgentOS* (placeholder — rename freely; "Nucleus", "Axon", and "Substrate" are all on the table).
> **Status:** v1 — research synthesis + reference architecture + build plan.
> **One-line thesis:** A minimal, Linux-based operating system whose primitive unit of execution, scheduling, and isolation is the *agent*, not the *application*.

---

## 0. The decision that gates everything else

Before any code, two forks determine ~80% of the architecture. The rest of this document gives a recommended default for each, but you should consciously choose:

1. **Where does cognition live?** *Remote* (the device is a thin agent host; the model is an API call) or *local/on-device* (weights ship with the OS). This single choice is the difference between an OS that is genuinely tiny and one where "light" means "a lean shell around a necessarily heavy model."
2. **Who runs on a box?** *Single-tenant* (one user's agents, mutually trusting) or *multi-tenant* (untrusted agents sharing hardware). This decides how hard the isolation problem is, and therefore how much of the system is security machinery.

**Recommended default for v1:** *remote cognition, single-tenant.* It is the fastest path to a working, genuinely-light system, and every harder variant is an extension of it rather than a rewrite. The whole roadmap below is structured so that local cognition and multi-tenancy are *additive*.

---

## Part 1 — Thesis and first principles

### 1.1 What "agents as the primitive" actually means

In a conventional OS the kernel's managed unit is the **process** and the human-facing unit is the **application**. Everything — the scheduler, the filesystem, IPC, init/systemd, the shell — is built around launching and supervising processes that run deterministic instruction streams.

"Agents as the primitive" means replacing that center of gravity. The managed unit becomes an **agent**: a long-lived, stateful, *non-deterministic* entity defined by a goal/role, a working context (its "RAM"), a connection to a model (its "CPU"), a set of capabilities exposed as tools, a persistent memory, and a lifecycle (spawn → run → suspend → resume → fork/delegate → terminate). There are no "applications" in the traditional sense; capabilities are exposed *to agents* as tools, and the things a user would normally "open" are instead things an agent *invokes*.

Concretely, each classical OS concept gets re-pointed:

| Classical OS | AgentOS analogue |
|---|---|
| Process / thread | Agent (the schedulable, isolatable unit) |
| Application | A *role* an agent plays; capabilities are tools, not apps |
| `exec()` a binary | Spawn an agent from a spec (role + tools + budget + memory) |
| Shell / login | A natural-language intent interface, or a **root/orchestrator agent** |
| init / systemd (PID 1) | Agent supervisor (`agentd`) that brings up and watches agents |
| Syscall table | The **tool ABI** — standardize on MCP; tools are "syscalls to the world" |
| IPC (pipes, D-Bus) | Structured inter-agent messages (A2A / ACP semantics) |
| Filesystem | A minimal real FS **plus** a memory substrate agents address directly |
| CPU time (the scarce resource scheduled) | Inference slots, token/$ budget, and tool rate limits |
| `/proc` | `/agents/<id>/{context,memory,tools,budget}` control plane via FUSE |

This table is the spine of the whole design. If a feature can't be mapped onto the right-hand column, it probably belongs to the host Linux layer and should be left alone.

### 1.2 First principles

- **The kernel is not the innovation.** Linux already does memory protection, drivers, scheduling primitives, namespaces, and cgroups extremely well. The win is the *userspace reconception*, plus a thin set of agent-native kernel *surfaces* (a control-plane VFS, capability enforcement). Resist the urge to write a new kernel.
- **Cognition is metered.** Unlike CPU time, every "thought" costs money, energy, and latency. Budgets (tokens, dollars, wall-clock) are first-class, schedulable resources — not afterthoughts.
- **Agents are event-driven and mostly idle.** They block on inference and tool I/O for seconds at a time. The system should assume an agent sits idle waiting for an event (message, timer, world-change) — the MemGPT insight. This shapes scheduling and suspend/resume.
- **Agents are semi-trusted at best.** They are non-deterministic and may be prompted into adversarial behavior. Capability scoping, sandboxing, and audit are core OS services here, not optional add-ons.
- **Build on the emerging standards, don't fork them.** MCP (tools), A2A/ACP (agent-to-agent), and DIDs (identity) are consolidating fast. Treat them as the POSIX of this world.

### 1.3 Explicit non-goals (for v1)

No desktop/GUI. No general-purpose application ecosystem. No attempt to be a daily-driver Linux. No new kernel and no new programming language. No multi-tenant hardening *yet*. Saying no to these is what keeps the system small and shippable.

---

## Part 2 — Prior art and the gap you're filling

Understanding what already exists keeps you from rebuilding it and sharpens what's actually novel.

**Karpathy's "LLM-as-OS" framing** is the conceptual anchor: the model is the CPU, the context window is RAM, model weights are ROM, retrieval/long-term stores are disk, and tool/API calls are syscalls. It's a metaphor, not a system — but it's the right mental model and your abstractions should line up with it.

**MemGPT (now Letta)** turned part of that metaphor into a mechanism: a two-tier memory hierarchy (in-context "main memory" vs. out-of-context "external memory"), where the model itself issues function calls to page information in and out — virtual memory for context windows. Its event-driven loop (the agent idles until an event) is directly reusable. Borrow the memory-paging and event model wholesale.

**AIOS (Rutgers, COLM 2025)** is the closest existing thing and the most important to differentiate from. It proposes an "AIOS kernel" — a *userspace runtime* that isolates LLM/tool resources from agent applications and provides six modules: scheduler, context manager, memory manager, storage manager, tool manager, and access control. Crucially, **AIOS runs *on top of* a normal Linux as an application-layer SDK + daemon. It does not replace the OS, and agents there are still ordinary processes managed by a library.**

> **This is your gap.** You're proposing to push "agent as primitive" *down the stack* — into a minimal distro where the agent runtime *is* the userspace (PID 1 / the boot target), where there are no general-purpose apps, and where agent state is exposed through kernel-adjacent surfaces. AIOS is a great blueprint for the *runtime's internal modules*; your contribution is making those modules the operating system rather than a framework running on one — and doing it in a super-light footprint.

**The protocol layer** is the connective tissue and is maturing in 2025–26:
- **MCP** (Anthropic) — tool/context access over JSON-RPC; the de-facto "USB-C for AI," with broad adoption and code-execution support. Use it as your tool ABI.
- **A2A** (Google → Linux Foundation, 2025) — agent-to-agent discovery and task lifecycle via "Agent Cards" over HTTP/JSON-RPC/SSE. Use its semantics for your inter-agent bus, even internally.
- **ACP** (IBM → Linux Foundation) — REST-native peer messaging for *local* coordination; a lighter middle layer between MCP and A2A. A good fit for on-box agent chatter.
- **ANP** — internet-scale agent networking with W3C DID identity; relevant if/when agents cross machine and org boundaries.

The standard 2026 guidance — MCP for tools, A2A when you need cross-vendor agent coordination, ACP for lighter peer messaging — maps neatly onto a layered design.

---

## Part 3 — Levels of ambition (pick your altitude)

"Agents as primitive" admits a spectrum. Be explicit about which level you're targeting, because it sets scope.

**Level 0 — Runtime on a normal distro.** Standard Linux + an `agentd` daemon managing agents (this is essentially AIOS). Easy, but agents aren't really the OS primitive — they're processes a library supervises. *Good as a Phase-0/1 spike, not the end state you described.*

**Level 1 — Minimal distro + agent-init.** A stripped Linux (Buildroot/Alpine-class) whose entire userspace is the agent runtime. `agentd` is PID 1 (or supervised by a tiny init). Boot brings up the agent runtime, not a login shell. No general-purpose apps are installed; the only "programs" are the runtime, tool-servers (MCP), and (optionally) a model server. **This is the sweet spot for your stated vision** — genuinely agent-native, genuinely Linux-based, genuinely light, and buildable by a small team.

**Level 2 — Agent-native kernel surfaces.** Level 1 plus first-class kernel-adjacent interfaces: a `/agents` control-plane filesystem via FUSE/9P (`/agents/<id>/context`, `/memory`, `/tools`, `/budget`), capability enforcement via seccomp + eBPF + optionally a custom LSM, and checkpoint/restore (CRIU) for suspend/resume/migration. Still Linux-based — you're adding modules and a control plane, not a kernel. *Target this incrementally after Level 1 works.*

**Level 3 — New microkernel / unikernel.** Abandon Linux for seL4, a unikernel, or something bespoke. Out of scope given your "Linux-based" constraint, but worth knowing it's the frontier if you ever outgrow Linux's assumptions.

**Recommendation:** ship **Level 1**, designed so that **Level 2** features slot in without rearchitecting. The roadmap in Part 9 follows exactly this.

---

## Part 4 — Reference architecture

Five layers, bottom to top. The novelty is concentrated in the runtime layer (4.3).

### 4.1 Hardware layer
CPU + optional GPU/NPU (only if cognition is local). Target can be an edge box, a NUC/mini-PC, a server, or a cloud VM. Keep the hardware assumptions minimal for v1.

### 4.2 Minimal Linux kernel layer
A custom-configured kernel: strip unused drivers and subsystems; keep cgroups v2, namespaces, seccomp, eBPF, and FUSE/9P; keep KVM only if you'll host agent/tool microVMs. Pair with **musl libc** and a **BusyBox** userland for an Alpine-class base. Built via **Buildroot** for v1 (simple, fast, produces a minimal immutable rootfs); reach for **Yocto** only if you later need to support many boards or a layered vendor ecosystem.

### 4.3 The agent runtime — `agentd` (the heart)
This replaces init + shell + the application model. It is PID 1 or supervised by a tiny PID 1. Its modules mirror AIOS but are reframed so the agent — not the process — is the unit. Components:

- **Lifecycle manager & scheduler.** Spawns, suspends, resumes, forks, and terminates agents. Event-driven dispatch. Critically, it schedules the *real* scarce resources — inference slots (GPU or API concurrency), token/$ budgets, and tool rate limits — not CPU time. (See Part 5.1; this is the hardest and most interesting subsystem.)
- **Context manager.** Owns each agent's working context (the "RAM"): assembly, compaction, and eviction, paging between context and long-term memory in the MemGPT style.
- **Memory substrate.** Long-term memory: an embedded structured store + a vector index + Karpathy-style "compiled markdown wiki" files. Exposed to agents through a clean API and (at Level 2) through `/agents/<id>/memory`.
- **Tool/capability manager.** Tools are the "installed programs." **Standardize on MCP** as the tool ABI; tool-servers run as sandboxed processes (isolation tier chosen per risk — see 4.4). This is your "syscall table for the world."
- **Inter-agent message bus.** Structured messaging between agents using **A2A/ACP** semantics (Agent Cards for discovery, task lifecycle for delegation). Replaces D-Bus/pipes. ACP for cheap on-box chatter; A2A when crossing machine/org boundaries.
- **Access control / capability system.** Every agent runs with a capability set: which tools, which memory regions, which agents it may message, and a hard token/$/wall-clock budget. Enforced at the runtime boundary in v1; hardened with seccomp/eBPF/LSM at Level 2.
- **Inference gateway.** Abstracts "where cognition comes from": a remote provider API and/or a local engine (llama.cpp / vLLM / Ollama). Manages request batching and the KV cache (the literal "RAM" of the analogy). For local cognition, this is where quantization choices live.
- **Flight recorder.** A structured, append-only event log of every prompt, tool call, message, and decision. Doubles as the debugging tool (Part 5.5) and the audit/governance substrate. Build this on day one — non-determinism makes it non-optional.

### 4.4 Isolation layer
Isolation is *tiered by risk*, not one-size-fits-all:
- **Light (single-tenant default):** Linux namespaces + seccomp + cgroups. Enough when all agents trust each other.
- **Medium:** **gVisor** — a userspace kernel intercepting ~70–80% of syscalls; ~10–20% overhead; strong isolation without a VM. Good for risky tools on a shared kernel.
- **Heavy:** **Firecracker** or **Cloud Hypervisor** microVMs — hardware-enforced isolation, ~5MB and ~125ms per instance; required for genuinely untrusted/multi-tenant agents. Cloud Hypervisor (or QEMU-microvm) over Firecracker if you need GPU passthrough for local inference inside the VM.
- **Featherweight (pure-compute tools):** **WASM** (wasmtime/WasmEdge) — microsecond starts, strong sandbox, but no persistent FS/full-OS integration. Ideal for stateless tool logic.

Choose per tool/agent at spawn time; the capability manager records the chosen tier in the flight recorder.

### 4.5 Interface layer
No GUI. The "shell" is an intent interface — a natural-language console and/or an API — and the entry point is a **root/orchestrator agent** that behaves like a login shell but is itself an agent. Everything a human does enters the system as a message to this agent.

---

## Part 5 — The hard problems (be honest about these)

These are the parts that are genuinely unsolved or genuinely difficult. They're also where the interesting work — and any publishable/defensible novelty — lives.

### 5.1 Scheduling non-CPU-bound, cost-metered, GPU-scarce work
Classic CPU scheduling assumes many CPU-bound processes competing for cores; you time-slice. Agents break every assumption: they're mostly blocked on inference/tool I/O for seconds; their scarce resources are *inference slots, token/$ budget, and rate limits*; KV-cache eviction makes "context switching" an agent on the GPU genuinely expensive; and they're event-driven. So the scheduler is really a **multi-resource admission controller + dispatcher** — closer to a database query scheduler or a Borg/Kubernetes resource manager than to a CFS-style time-slicer. Borrow: cgroups for hard caps, weighted fair queuing for inference slots, deadline-aware scheduling for interactive agents, backpressure for overload, and a "token budget" accounting dimension treated like a cgroup. **This is the subsystem most worth prototyping early and getting right.**

### 5.2 Capability enforcement against a non-deterministic actor
The agent decides what to do at runtime, by generating text. You cannot enumerate its behavior in advance. Enforcement therefore has to be *external and unbypassable*: capabilities are checked at the tool-invocation boundary and at the syscall boundary (seccomp/eBPF/LSM at Level 2), never inside the agent's own reasoning. Study capability-based systems (seL4, Capsicum, Fuchsia) for the model. High-impact actions need human-in-the-loop checkpoints and hard kill-switches. This is make-or-break for trust and is genuinely hard.

### 5.3 The memory substrate
Decide whether memory is layered *on* a normal filesystem or *replaces* it as the primary abstraction. Recommended: keep a minimal real FS (you need one for the kernel, binaries, and logs) but expose the *agent-facing* world through the memory substrate + the `/agents` control plane. Open questions: consistency under concurrent agent writes, eviction/compaction policy, and how much to lean on the "compiled markdown wiki" (write-time distillation) vs. runtime retrieval (vector search). The two are complementary; most systems will want both.

### 5.4 Checkpoint / restore / migration
Agents are long-lived and stateful, so you want to snapshot an agent's full state (context + memory pointers + position-in-task) and restore or migrate it across machines — the agent analogue of CRIU/process migration. This enables true suspend/resume and load-balancing, but agent state spans the model context, the runtime, *and* external stores, so a clean snapshot boundary is non-trivial. Design the state model with this in mind from the start.

### 5.5 Non-determinism, testing, and debugging
The same input can produce different behavior. Conventional debugging and regression testing partly break. The mitigation is the **flight recorder** (4.3): full, replayable event logs; deterministic replay where possible (fixed seeds, recorded tool responses); and evaluation harnesses rather than unit-test-only thinking. Invest here early or debugging will dominate your time later.

### 5.6 The "light vs. local cognition" tension (restating the fork)
"Super light" and "local model" pull against each other — weights and inference dominate footprint and resource use. Resolve it consciously: **remote cognition** → the runtime is genuinely tiny and the device is a thin agent host; **local cognition** → "light" means a lean OS wrapped around a necessarily heavy model, leaning hard on small/quantized models (GGUF via llama.cpp). v1 default is remote.

---

## Part 6 — The "super light" strategy

Concrete levers, roughly in order of impact:

1. **Remote cognition for v1.** The single biggest lever — no weights on the box.
2. **musl + BusyBox base**, custom-stripped kernel (only needed drivers/subsystems), immutable rootfs. Alpine-class footprint.
3. **Compiled runtime, not interpreted.** A single static `agentd` binary. (Language choice in Part 7.)
4. **WASM tools** where possible instead of full container images — kilobytes, not hundreds of megabytes.
5. **Embeddable storage** (a single-file structured store + an embedded vector index) rather than standing up servers.
6. **If local cognition is required:** small quantized models, aggressive KV-cache management, and treat the model server as the one heavyweight component the rest of the system is deliberately lean around.

A useful framing: report your footprint as *idle RAM*, *image size*, *boot-to-first-agent time*, and *agents-per-GB* — these make "light" measurable rather than rhetorical (see Part 10).

---

## Part 7 — Recommended tech stack

Opinionated defaults; each is defensible and swappable.

- **Runtime language: Rust.** Memory safety matters enormously when the entire system mediates untrusted, non-deterministic agent actions; you also get small static binaries, excellent async (tokio), and clean FFI. *Alternative:* Go — simpler concurrency and it's what gVisor is written in, at the cost of GC and larger binaries. Pick Rust unless team familiarity strongly favors Go.
- **Base/build: Buildroot** (musl + BusyBox, immutable rootfs). Move to Yocto only at multi-board scale.
- **Init: `agentd` as PID 1**, or a ~100-line custom PID 1 that supervises `agentd`.
- **Tool ABI: MCP.** Inter-agent: **ACP** on-box, **A2A** across boundaries.
- **Inference: provider API** (v1) with a pluggable gateway; **llama.cpp / vLLM / Ollama** for local later.
- **Memory: SQLite + an embedded vector index** (e.g., sqlite-vec or similar) + markdown wiki files. All embeddable, all light.
- **Isolation: namespaces+seccomp → gVisor → Firecracker/Cloud Hypervisor**, tiered by risk.
- **Identity: signed agent identities now; W3C DIDs** if/when you go cross-machine (ANP).
- **Observability: the structured flight recorder** as the single source of truth for debugging *and* audit.

---

## Part 8 — How to research and flesh this out

A concrete methodology for turning this v1 into a buildable spec, ordered to de-risk early.

1. **Pin the thesis and non-goals.** Lock the two forks (Part 0), the target hardware, and the intended user. Write a one-page "what "agent as primitive" means *for us*" so scope stops drifting.
2. **Survey prior art deliberately** (largely done here): AIOS (internal module design), MemGPT/Letta (memory paging + event loop), Karpathy LLM-OS (mental model), and the MCP/A2A/ACP/ANP stack (interop). Read the AIOS and MemGPT papers in full; skim the protocol specs for the parts you'll implement.
3. **Survey the systems substrate** (also largely done here): Buildroot/Yocto/Alpine, the isolation technologies, init systems, FUSE/9P, eBPF/LSM, and CRIU. Stand up a Buildroot "hello world" image early so the build pipeline is real, not theoretical.
4. **Map novel problems onto solved analogues.** Scheduling → Borg/Kubernetes + DB query schedulers. Capabilities → seL4/Capsicum/Fuchsia. Checkpoint/restore → CRIU. Message passing → microkernel IPC. Reading these saves you from reinventing badly.
5. **Spike the riskiest unknowns first** (in priority order): the resource-aware scheduler, the capability sandbox, and boot-to-agent on a minimal image. Time-box each spike; the goal is to retire risk, not to build production code.
6. **Define success metrics up front** (Part 10) so "is it working / is it light" has objective answers.
7. **Write the spec → prototype → iterate.** This document is the seed of the spec; each spike feeds corrections back into it.

---

## Part 9 — Implementation roadmap

Sequenced so each phase is independently demoable and so Level-1 → Level-2 is additive. This is the build order for when you start coding.

**Phase 0 — The spike (proof of concept).** On a normal distro, write `agentd` v0: a single-agent runtime that loads a config, runs one agent loop (perceive → infer via the remote gateway → act via an MCP tool → observe), and logs everything to the flight recorder. *Goal: prove the agent-loop-as-process model end to end.* Smallest possible thing that thinks and acts.

**Phase 1 — Multi-agent + scheduler + bus.** Many agents; the event-driven resource-aware scheduler with token/concurrency budgets (Part 5.1); the ACP/A2A message bus; and the capability/access-control system enforced at the runtime boundary. Still on a normal distro. *Goal: the runtime is real and the hard scheduling/capability problems have first answers.*

**Phase 2 — The distro.** A Buildroot image where `agentd` is PID 1 (or supervised by a tiny init), with a stripped kernel, musl/BusyBox, and an immutable rootfs. It boots straight into the agent runtime with no login shell. *Goal: this is now "the OS," and it's measurably light.*

**Phase 3 — Agent-native kernel surfaces (Level 2).** The `/agents` control-plane VFS via FUSE; capability enforcement hardened with seccomp/eBPF (and an LSM if needed); checkpoint/restore via CRIU for suspend/resume/migration; the memory substrate promoted to a first-class service. *Goal: agents are primitives at the kernel surface, not just in userspace.*

**Phase 4 — Hardening & ecosystem.** The full isolation tiering (WASM/gVisor/microVM) chosen per risk; audit/governance/human-in-the-loop and kill-switches built on the flight recorder; DID-based identity; a packaging story for tools-as-MCP-"apps"; and optional local inference. *Goal: multi-tenant-capable, governable, and extensible.*

---

## Part 10 — Success metrics

Make "agent-native" and "light" measurable:

- **Footprint:** image size; idle RAM; *agents-per-GB* of RAM.
- **Latency:** boot-to-first-agent; cold agent-spawn time; suspend/resume time.
- **Throughput:** concurrent active agents per box; inference-slot utilization under load.
- **Economy:** tokens/$ per completed task; budget-enforcement accuracy (do agents actually stop at their cap?).
- **Isolation:** which tier each workload runs in; measured overhead per tier; absence of cross-agent leakage in red-team tests.
- **Observability:** can any agent run be fully reconstructed/replayed from the flight recorder? (Yes/no is the bar.)

---

## Part 11 — Open questions I need your call on

These are the inputs that would let me tailor the architecture and write the actual code-level spec:

1. **Cognition: remote, local, or hybrid?** (Gates footprint and the inference gateway design.)
2. **Tenancy: single-user device or multi-tenant host?** (Gates how much of the system is isolation/security machinery.)
3. **Target hardware and primary use case.** Edge device, mini-PC, server, or cloud VM — and *what are these agents for*? (A personal automation box, an agent server for a product, a research platform, and an embedded controller all bias the design differently.)
4. **Team size / language preference.** (Rust vs. Go; how ambitious Phase 1 can be.)
5. **How far down the stack do you want to go in v1 — Level 1 (ship) or push toward Level 2 (research)?**

---

## Appendix A — Glossary

- **Agent:** stateful, non-deterministic entity (role + context + model + tools + memory + lifecycle); the OS primitive here.
- **Tool / capability:** an external function an agent can invoke (MCP server); the "syscall to the world."
- **Context:** an agent's in-window working memory ("RAM").
- **Memory substrate:** long-term store (structured + vector + markdown wiki) the agent pages to/from.
- **`agentd`:** the userspace agent runtime; PID 1 or supervised by a tiny init.
- **Flight recorder:** append-only structured log of all agent activity; debugging + audit substrate.
- **Isolation tier:** namespaces/seccomp → gVisor → microVM → WASM, chosen per risk.

## Appendix B — Reading list

- *AIOS: LLM Agent Operating System* (Mei et al., COLM 2025) — the runtime-module blueprint and the thing to differentiate from. arXiv:2403.16971.
- *MemGPT: Towards LLMs as Operating Systems* (arXiv:2310.08560) — memory paging + event-driven loop.
- Karpathy's "LLM as the new OS / Software 3.0" talks — the mental model.
- MCP, A2A, ACP, ANP specifications + the 2025–26 agent-interoperability surveys (e.g., arXiv:2505.02279, 2506.05364).
- Firecracker, gVisor, and Kata documentation; the 2026 AI-agent sandboxing comparisons — isolation tradeoffs.
- Buildroot and Yocto documentation — the build substrate.
- seL4 / Capsicum / Fuchsia capability models; CRIU — for capabilities and checkpoint/restore.
