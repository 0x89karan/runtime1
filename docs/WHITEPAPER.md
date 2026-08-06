# runtime1: An Operating System Where Agents Are the Primitive

**A whitepaper on agentOS and agentd — the substrate, the security model, and the
evidence pipeline.**

*runtime1 project · agentd v0.121.0 · August 2026 · draft, versioned with the code*

---

## Abstract

AI agents are useful precisely when you stop watching them — and that is when they are
dangerous. Today, an agent that touches real, sensitive systems (email, code, deals, a cap
table) is either babysat with per-action approval prompts, coarsely sandboxed in a container,
or handed to a multi-tenant cloud service the operator does not control. None of these lets a
*team* of agents run unsupervised while guaranteeing that each one is bounded, every action is
audited, spend is metered, and data does not leak.

runtime1 is an open-source experiment testing one idea: those guarantees are
operating-system-shaped. They belong in the boundary that runs the agent, not in the framework
that authored it and not in the prompt that instructs it. The project builds **agentOS**, a
minimal Linux-based operating system whose primitive unit of execution, scheduling, and
isolation is the agent rather than the application, and **agentd**, its ~2 MB Rust runtime
that boots as PID 1 and *is* the userspace. This paper states the thesis, describes the
architecture as built (not as aspired to), gives the security model in the vocabulary we hold
ourselves to — *enforced*, *declared*, or *not built* — and describes the evidence pipeline
that lets someone verify what an agent did without trusting the machine that did it, together
with a precise account of what that evidence cannot prove.

---

## 1. Names, once

Three names, one stack:

| Name | What it is |
|---|---|
| **runtime1** | The open-source project as a whole: the OS, the runtime, the operator tooling, the tool servers, the verification pipeline, and the docs. |
| **agentOS** | The operating system inside it: a minimal Linux base whose entire userspace is the agent runtime. No desktop, no apps, no login shell — agents and the tools they invoke. |
| **agentd** | agentOS's runtime binary: a ~2 MB size-optimized Rust daemon that boots as PID 1 and *is* the userspace. Today it also runs as an ordinary binary on a normal distro. |

Supporting cast: `agentctl` is the operator CLI and terminal cockpit; MCP servers are the
tool processes; the flight recorder, evidence chain, and memory substrate are subsystems of
agentd described below.

## 2. The problem

Agents that act on systems that matter cannot currently be trusted unsupervised. The three
available postures all fail the same test:

- **Babysitting.** Per-action approval prompts scale with the number of actions, and a
  standing 24/7 team of agents produces more actions than any human will review. Supervision
  that requires presence is not supervision for the case that matters.
- **Coarse sandboxing.** A container around the whole process bounds the blast radius of a
  crash, not of a decision. It cannot distinguish "read the inbox" from "forward the inbox,"
  cannot meter spend, and records nothing an auditor could use.
- **Hosted delegation.** A multi-tenant SaaS chief-of-staff sees your investor threads,
  customer deals, and credentials by construction. For a class of high-trust work, that is
  disqualifying regardless of the vendor's controls — the objection is structural, not
  reputational.

The missing artifact is a substrate you own: one that runs agents while you are not looking
and can afterward show you — and, on the roadmap, show a third party — exactly what happened,
with the boundaries enforced by code rather than promised by a prompt.

## 3. The thesis: agents as the primitive

In a conventional OS the kernel's managed unit is the process and the human-facing unit is the
application. agentOS replaces that center of gravity. The managed unit is an **agent**: a
long-lived, stateful, non-deterministic entity defined by a role, a working context (its RAM),
a connection to a model (its CPU), a set of capabilities exposed as tools, persistent memory,
and a lifecycle — spawn, run, suspend, resume, delegate, terminate. There are no applications;
the things a user would "open" are things an agent *invokes*.

Each classical OS concept is re-pointed:

| Classical OS | agentOS analogue |
|---|---|
| Process / thread | Agent — the schedulable, isolatable unit |
| Application | A *role* an agent plays; capabilities are tools, not apps |
| `exec()` a binary | Spawn an agent from a spec: role + tools + budget + memory |
| Shell / login | Natural-language intent, or a root orchestrator agent |
| init / systemd (PID 1) | `agentd` — the agent supervisor *is* the userspace |
| Syscall table | The tool ABI — MCP; tools are syscalls to the world |
| IPC (pipes, D-Bus) | Structured inter-agent messages |
| CPU time (the scarce resource) | **Inference slots, token/$ budgets, tool rate limits** |
| `/proc` | `/agents/<id>/{context,memory,tools,budget}` via FUSE |

This table is the spine of the design. A feature that cannot be mapped onto the right-hand
column belongs to the host Linux layer and is left alone — the kernel is not the innovation;
the userspace reconception is.

### 3.1 Two constitutional decisions

Two choices were made deliberately, early, and are treated as locked:

1. **Cognition is remote.** The device is a thin agent host; the model is an API call behind
   an inference gateway. There are no local weights and no local inference engine. This is the
   difference between an OS that is genuinely ~2 MB and one where "light" means a lean shell
   around a necessarily heavy model. A local backend may arrive later only as another gateway
   implementation, never as a core assumption.
2. **Single-tenant.** One person's agents on one box, mutually trusting, in-process. Capability
   scoping between agents exists for least privilege — a prompt-injected agent should hold the
   smallest possible blast radius — not because agents distrust each other. Multi-tenant
   hardening is explicitly out of scope.

### 3.2 Why light matters

"Super light" is not an aesthetic. Remote cognition plus a ~2 MB static binary means agentd
runs on hardware people actually own — an Apple Silicon Mac, a Raspberry Pi, an ARM home
server — not only on x86 cloud VMs. That is what makes this an *OS you own* rather than *a
cloud you rent*, and it is a precondition of the thesis: the high-trust work that motivates
the project is exactly the work its operators will not put in someone else's cloud.

## 4. Architecture

agentd is one process. Everything below runs inside it except the tool servers, which are
separate sandboxed processes.

### 4.1 The scheduler

One cooperative loop steps every agent toward its next effect; nothing runs in parallel
inside a turn, so the flight log reads as a clean sequence. The scheduler owns the two
resources that actually matter for agents:

- **Budgets.** Token and dollar spend is metered per agent and globally, with a rolling reset
  window. The admission gate is pre-turn: an agent over its window is *deferred*, not bricked,
  so a burst costs latency rather than the agent. The fuse has stopped a real runaway loop.
- **Checkpoints.** All agents are snapshotted at turn boundaries and restored after a crash or
  restart. A SIGTERM landing mid-tool-call is sealed on the checkpoint copy — never the live
  transcript — so a restore does not replay a transcript the model provider rejects. Restore is
  **at-least-once** for tool side effects, not exactly-once; that limit is stated in the
  runbook, not glossed.

**Sealed jobs.** A parent can trigger a job whose capabilities and instructions come from
config, never from the caller, and receives only a completion signal — never the job's output.
This is the answer to an injected orchestrator: a compromised trigger cannot read your mail
through a child it spawned.

### 4.2 The capability system

Every agent runs with an explicit grant set: which tools, which filesystem prefixes, which
hosts and ports, which memory segments, what spend. The checks are in-process Rust at the
tool-invocation boundary — enforcement is external to the agent's reasoning, because a
non-deterministic actor cannot be trusted to police itself. Spawned children may only receive
an attenuated subset of the parent's grants; an over-grant is rejected at spawn, not silently
clamped. A boot-time linter (`agentd check --strict`) fails closed on mis-wired grants.

### 4.3 Tools: MCP as the ABI

Anything an agent does to the world is a Tool, and MCP is the tool ABI — over stdio and
streamable HTTP. Native tools exist only for zero-dependency convenience. Tool servers run as
separate subprocesses under tiered sandboxing (§5), and event triggers (cron, filesystem,
webhook) let the world wake an agent up.

### 4.4 The credential broker

Agents and tool servers never hold raw credentials. OAuth tokens and API secrets live behind a
broker inside agentd; sidecars receive an ephemeral broker token valid for one spawn. Upstream
hosts are allowlisted and IP-pinned at startup. A memory dump of an agent yields inert
placeholders — credential exfiltration is killed structurally. (This does not stop data being
laundered *through* an allowlisted channel; that is a different threat, and we say so.)

### 4.5 The memory substrate

Four tiers with provenance: the in-context working set; per-agent short- and long-term stores
on an embedded key-value engine (redb); a shared knowledge base with namespaces and mutability
classes (`log`, `scratch`, `canon` — the store enforces who may write what and how keys are
assigned); and a semantic index behind an optional sidecar. Memory is exposed to agents as
tools and to the operator through the filesystem surface. The semantic tier degrades honestly:
without an embeddings key, search returns an explicit empty rather than arbitrary
nearest-neighbours, and degraded writes land in a separate namespace.

### 4.6 The inference gateway

The only way to reach a model, and the one place cognition enters the system. Remote by
construction; metered, retried, streamed, and receipted. Every request logs the numbers that
decide context paging (`retained_tokens_est`, `paging_limit`, `paging_limit_source`) — added
after a production diagnosis had to be done by mounting a Docker volume and inferring intent
from tool-call previews.

### 4.7 The flight recorder

An append-only structured JSONL log of every meaningful step: prompts, tool calls, capability
verdicts, approvals, budget decisions. Logging is best-effort by design and must never crash
an agent. Built on day one, because non-determinism makes it non-optional: it is the debugging
substrate, the audit substrate, and the raw material the evidence pipeline signs. The same
stream fans out over SSE to the operator surfaces.

### 4.8 The evidence chain

Model calls — allowed *and denied* — are receipted into a hash-chained, Ed25519-signed log,
rotated into independently verifiable segments, with an offline verifier (`agentctl verify`).
Its three honest limits are documented in the threat model and repeated in §6, because they
are the most load-bearing paragraphs in the project.

### 4.9 Operator surfaces

- **`agentctl watch`** — a terminal cockpit: every agent, spend, capabilities, live output,
  logs, topology, memory, and an approval queue, with row-scoped control verbs (cancel, set
  budget, park) on the same screen as the evidence.
- **`/agents` FUSE filesystem** (Linux) — the system as files; read-only except one control
  inode.
- **A management HTTP API** with SSE fan-out; every mutating route gated by a constant-time
  approval token when configured.
- **The morning brief** — the reference workload's daily artifact, delivered as markdown on
  disk and over Telegram, with staleness surfaced on the read path (a brief that stopped being
  produced announces itself, because a field written only on success can never report failure).

### 4.10 The distro

agentOS as an image: a Buildroot external tree — stripped kernel, musl, BusyBox, immutable
rootfs — that boots QEMU (x86_64 and aarch64) straight into agentd as PID 1, with the FUSE
control plane mounted and no login shell. The same binary ships in a multi-arch Docker image
for the Mac path, which is the one dogfooded daily.

## 5. The security model, stated the way we'd want to hear it

Every claim in this project carries one of three labels, and the same vocabulary runs through
the site, the docs, and the interactive architecture schematic:

- **enforced** — code stops you; tests cover it.
- **declared** — a config or prompt says so; nothing beneath it enforces it.
- **not built** — it does not exist yet, however good the spec is.

The load-bearing distinction is **capability layer vs. isolation boundary**. Landlock +
seccomp + namespaces is a *capability layer*: least-privilege on a process that still shares
the host kernel. It is not an isolation boundary strong enough for untrusted or
foreign-framework code; for that, the floor is a microVM or a user-space kernel (gVisor), and
the runtime's isolation tiers report which one a workload actually got.

Enforcement is also a function of platform, and the architecture schematic renders it that
way:

| Control | Mac · Docker (aarch64) | Linux · QEMU (x86_64) |
|---|---|---|
| agentd capability checks | **enforced** | **enforced** |
| Landlock FS confinement | unavailable | **enforced** |
| Landlock per-port net | unavailable (fails open, with a startup warning) | **enforced** |
| seccomp-bpf | unavailable | **enforced** |
| DenySpawn | unavailable (x86_64-gated) | **enforced** |
| gVisor tier | not present | declared (detected if installed) |
| Container / VM boundary | **enforced** (coarse) | **enforced** (coarse) |
| Credential broker allowlist | **enforced** | **enforced** |

What we refuse to say: "watertight." Anyone who can call a model can, in principle, launder
data through it — the model channel is itself a covert channel. What we do say: allowlist-only
egress, every allowed call metered and audited, approval gates on risky actions, structural
anti-credential-theft, a budget fuse that has actually stopped a runaway, and a tamper-evident
audit trail. Defense-in-depth plus accountability, not a hermetic seal.

## 6. Evidence: enforce in real time, prove after the fact

Agent governance splits into two halves with different physics. **Enforcement** must be native
and instantaneous — proving a policy decision in a zkVM takes seconds, an agent runtime makes
thousands of calls a day, so cryptography in the authorization path is a denial-of-service
against the runtime itself. **Evidence** must be cryptographic and checkable by someone who
does not trust you. agentd enforces; a separate pipeline proves; the prover never sits in
front of an action and never enters agentd's dependency tree.

What cryptography can prove about the log: **integrity** (not modified or reordered after
writing), **rewind detection** once custody anchoring lands, and **policy consistency** once
zk replay lands — every published decision re-evaluates to the same verdict under a committed
policy, without revealing the policy.

What no signature or proof can say: **completeness**. An action that was never journaled
produces no gap; "every action was recorded" is a property of the boundary, not of any proof
over its output. Today's chain has two further stated limits: coverage is model calls only,
and the signing key lives on the audited host — the chain proves integrity relative to a local
key, which is self-attestation, not third-party evidence. The verification ladder (coverage
and custody anchoring → auditor-side trust root → one shared policy-evaluation artifact →
decision journal with native replay → zk batch replay proofs → fleet-scale verification) is
specced and sequenced so the proof lands on top of auditor-grade evidence, never ahead of it.
The zk rung stays unbuilt until a concrete counterparty exists whose verifier must not see the
policy — when the verifier owns the policy, native replay gives the same assurance for free.
Closing the completeness seam structurally requires hardware attestation with credentials
sealed inside the attested boundary; it is sketched, honestly scoped to brokered actions, and
not scheduled.

## 7. The reference workload

The workload that drives the work is a standing **chief of staff**: three of the seventeen
shipped agent templates wired together — a de-privileged cron trigger holding only
`{cron_trigger, RunJob}`, a sealed inbox job that reads Gmail metadata (headers plus a
~200-character snippet, never bodies), and a sealed curator with no Gmail access at all that
assembles a morning brief from what the inbox job wrote. Separation of duty is structural: the
job that reads untrusted email cannot write your files, and the job that writes your files
cannot read your mail.

It is not the product. It exists to put the substrate under real conditions, and it earns its
keep by failing informatively. The uptime drought traced to a missing Docker restart policy,
not brief logic. The "handled items re-list" premise was falsified by tracing the read path.
The context-paging fix that an audit headlined was measured to be arithmetically inert on the
agent it named — and enabling it would have been an active regression, silently dropping the
oldest emails. The current increment (scheduler-native cron) exists because measurement showed
an LLM sitting on a schedule boundary burns ~3,456 inference calls a day to watch a clock.
Each of these corrections is recorded in the repo rather than edited out of it.

## 8. Prior art, and the gap

- **Karpathy's LLM-as-OS framing** is the mental model: model as CPU, context as RAM, tools as
  syscalls. A metaphor, not a system — but the abstractions here line up with it.
- **MemGPT/Letta** turned the memory half into a mechanism: two-tier memory with model-driven
  paging, and an event-driven loop where agents idle until something happens. Both are
  borrowed.
- **AIOS** (Rutgers, COLM 2025) is the closest system and the important differentiation: an
  "AIOS kernel" that is a userspace runtime *on top of* a normal Linux, where agents are still
  ordinary processes managed by a library.
- **MCP / A2A / ACP** are treated as the POSIX of this world: build on the emerging standards,
  don't fork them.

The gap runtime1 fills is pushing "agent as primitive" *down the stack*: a distro where the
agent runtime is the userspace (PID 1, the boot target), where there are no general-purpose
apps, where agent state is exposed through kernel-adjacent surfaces — and where the governance
(capabilities, budgets, evidence) is native to that layer rather than layered on as a
framework. AIOS is a blueprint for a runtime's internal modules; runtime1's contribution is
making those modules the operating system, in a super-light footprint, with an evidence story
that states its own limits.

## 9. What exists today, and what is still a hypothesis

**Built and tested** (v0.121.0): the multi-agent scheduler with token/$ metering and the
rolling budget fuse; checkpoint/restore that survives SIGTERM mid-tool-call; the capability
system with spawn-time attenuation and a strict linter; the tiered sandbox (Landlock, seccomp,
namespaces, gVisor detection); four-tier memory with provenance and mutability classes; MCP
over stdio and HTTP/SSE; streaming inference; the credential broker; the signed receipt chain
with offline verification; the `/agents` FUSE surface; the management API; the `agentctl`
cockpit; seventeen agent templates; the Buildroot/QEMU distro path; and the chief-of-staff
reference workload, dogfooded daily against a real inbox.

**Specced, not built:** full action coverage and custody anchoring, the auditor-side trust
root, the shared policy artifact, the decision journal and native replay, zk batch replay
proofs, fleet-scale verification, hardware isolation per agent, and skills as governed
objects.

**The hypothesis under test:** that a high-trust slice of work will never go to multi-tenant
SaaS and wants an owned substrate — and that putting the guarantees in an operating system
makes unsupervised agents genuinely trustworthy rather than differently untrustworthy. That is
an open question. The dogfood metric that decides the current chapter is unsentimental: does
the operator stop checking email manually for fourteen consecutive days?

"Light" is also measured, not asserted: image size, idle RAM, boot-to-first-agent,
agents-per-GB, and budget-enforcement accuracy are the tracked numbers; the runtime binary is
~2 MB and the size is CI-guarded.

## 10. How to read the rest of the project

- `docs/DESIGN.md` — the full design and research synthesis (the *why*).
- `docs/ROADMAP.md` — the staged build plan and work queue.
- `docs/THREAT_MODEL.md` — the security posture, including §8.7's account of what the
  evidence chain cannot prove.
- `docs/PRODUCT-LANGUAGE.md` — the one-page naming and claim-vocabulary reference.
- The website's interactive schematic (`site/architecture.html`) — the running system, box by
  box, with every badge honest about enforced vs. declared vs. absent, per platform.

The source of truth is the code, not this paper. Where they disagree, the code wins and this
paper gets corrected — that has already happened to bolder claims than any made here.
