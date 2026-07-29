# AgentOS — Product Thesis & Positioning

**Status:** north-star positioning (forged in an /office-hours diagnostic + an independent
cross-model cold read + four adversarial turns). Companion to `docs/DESIGN.md` (the
technical thesis) and `docs/ROADMAP.md` (the build queue). This doc answers *why anyone
runs their agents on AgentOS, and what we are actually building toward.*

---

## North star

**An AI Chief of Staff that coordinates between a team of agents and tools in a secure,
observable manner.** The Chief of Staff is the product; **AgentOS is the secure + observable
substrate** that makes coordinating real agents over real systems trustworthy. Everything in
the roadmap serves that.

## The problem

Agents that touch real, sensitive systems (email, CRM, issues, code, cap table) can't be run
unsupervised today. You either babysit them (Claude Code's per-action prompts), sandbox the
whole process coarsely (a container), or hand your crown-jewel ops to a multi-tenant SaaS.
Nothing lets a *team* of agents coordinate across your systems while guaranteeing each one is
bounded, every action is audited, spend is metered, and your data doesn't leak — without you
watching. AgentOS is that substrate.

## The wedge (honest)

At generic n=1 use, "prompts + a container is close" — the trust wedge is weak. It is **strong
even at n=1 for one workload class: agents that are standing, multi-agent, and act on sensitive
systems unsupervised.** That is exactly the Chief of Staff: you cannot sit and approve a 24/7
team of agents touching your inbox, so the cobbled-together status quo fails. The defensible
core is the **owned, secure + observable substrate** — which SaaS chief-of-staff products
structurally cannot offer, because they *are* the multi-tenant cloud you won't trust with
sensitive ops.

**Target user (beachhead):** an operator/founder/exec who wants an always-on AI chief of staff
but won't put investor threads, customer deals, and cap-table context into someone else's
cloud, and won't trust the model's good behavior alone.

**Value-prop line:** *Your own AI chief of staff that coordinates a team of agents across your
real systems, and that you can finally trust unsupervised — because it runs on AgentOS: a tiny,
owned runtime that bounds every agent, brokers and meters every tool and model call, and audits
everything. The automation never costs you control of your data, your spend, or what it's
allowed to do.*

## Architecture: framework-agnostic runtime, two governance tiers

AgentOS is to agent frameworks what Linux is to languages and Docker is to app stacks: it does
not dictate how the agent was authored; it runs it bounded, metered, and audited.

```
Host
└─ microVM/container            ← tenant isolation (Firecracker/gVisor); one per trust domain
   └─ AgentOS instance          ← agentd PID 1, ~3 MB, single-tenant
      ├─ native agents          in-process, thin     → ENHANCED tier
      ├─ LangChain/CrewAI agent governed child proc  → UNIVERSAL tier
      ├─ headless claude/codex  governed child proc  → UNIVERSAL tier (governs what OpenClaw runs today)
      ├─ MCP tool servers       sandboxed subprocs
      └─ choke points: LLM gateway · MCP gateway · egress mediator · flight/OTEL/eBPF
```

- **Universal tier (any framework / headless Claude Code / Codex, no rewrite):** runs as a
  governed child process; a network namespace confines egress to the gateways; the workload's
  model `base_url` points at the LLM gateway so calls are metered/routed/audited without its
  cooperation; eBPF audits actual syscalls. Delivers bound + meter + audit. *Honest limits:*
  content audit/redaction is best-effort (TLS pinning), density erodes for fat Python
  workloads, and hosting them pulls AgentOS toward being a process supervisor.
- **Enhanced tier (native model + MCP):** thin (dense), full content observability (agentd is
  the model client), semantic tool governance, per-tool approval gates. Where the deepest value
  and the density advantage live. The Chief-of-Staff subagents are written here.

**The native framework is adoptable, narrowly.** Do not compete with LangGraph on orchestration
DX (crowded, late, no wedge). The native model is adopted as the *only path to the enhanced
tier* — you write native when you need the strongest governance, not when shopping for
orchestration. Keep it thin: it competes on integration, never on features.

## Security model — state it honestly, never say "watertight"

**Capability layer vs. isolation boundary (the load-bearing correction).** Landlock + seccomp
+ namespaces is a *capability* layer — least-privilege on a process that still shares the host
kernel, one kernel exploit from host compromise. It is **not** an isolation boundary strong
enough to run untrusted, agent-generated, or foreign-framework code. For the universal tier
(hosting arbitrary code) the real floor is a **microVM (Firecracker, dedicated guest kernel)
or a user-space kernel (gVisor)**. Native-tier agents (our own thin code) can run with the
capability layer alone; anything untrusted needs the isolation floor underneath. Don't conflate
the two — we have been over-crediting the capability layer.

- **Confinement is strong** on a controlled kernel (the appliance): isolation floor (microVM/
  gVisor) + network-namespace + proxy-only egress means a compromised agent/tool has nowhere
  to go but the allowlisted gateways.
- **Exfiltration is bounded, not impossible:** the model channel and the tool endpoints the
  Chief of Staff must reach are themselves potential covert channels. Anyone who can call a
  model can in principle launder data through it.
- **Deterministic anti-credential-theft (boundary secret rewriting):** the agent gets
  *placeholder* secrets in its env; the egress proxy swaps placeholder→real at the boundary.
  The agent never holds real credentials, so even a full memory dump yields inert strings.
  This kills credential exfiltration structurally (it does not stop laundering data through the
  model — different threat).
- **Tamper-evident audit, honestly scoped** (corrected in ux.6a — this bullet previously claimed
  "action receipts" and "forensic evidence", neither of which the implementation supports): the
  egress mediator emits hash-chained, Ed25519-signed **inference receipts** — model calls, allowed
  and (since ux.6a) denied. Not tool calls, capability verdicts, approvals, cancels, or budget
  decisions; those are in `flight.jsonl`, which is unsigned. And the signing key is generated and
  held by the same process, so the chain proves integrity **relative to a local key** —
  self-attestation, not third-party forensic evidence. Making it third-party-grade requires moving
  the key outside the boundary (customer-held key, external timestamping, or control-plane
  countersigning), which is unbuilt. Full scope: `THREAT_MODEL.md` §8.7.
- **The claim we make:** allowlist-only egress, every allowed call metered + audited, approval
  gates on risky actions, content redaction on the native tier, dramatically reduced blast
  radius, and a tamper-evident audit trail of what each agent actually did. Defense-in-depth +
  accountability, not a hermetic seal. Sophisticated buyers trust this framing and distrust
  "impenetrable."

**Observability is coupled to the isolation choice.** Host eBPF is *blind* inside gVisor (the
Sentry handles syscalls in user space; they never reach the host kernel), so the audit
mechanism depends on the boundary: native/microVM-guest → eBPF; gVisor → gVisor's remote sink
(`seccheck.Sink`). Picking the isolation floor and picking the observability mechanism is one
decision, not two. See `docs/OBSERVABILITY-PLAN.md`.

## Positioning decisions

- **Appliance-first.** Ship the Chief of Staff as an owned appliance on AgentOS. Greenfield, so
  the "insertion-point mismatch" objection (enterprises govern fleets from a control plane, not
  by selecting a guest image) does not bind — there is no existing workload to replace.
- **Guest userspace, not control plane (v1).** AgentOS is the single-tenant runtime inside each
  tenant's microVM; the cloud/host keeps multi-tenant isolation + orchestration. Honors the
  locked single-tenant decision.
- **Fleet / multi-tenant control plane = a separate, later bet.** If pursued, an independent
  cold read says run it control-plane-first (govern existing containers), not guest-image-first.
  Do not conflate it with the appliance; win the appliance first.
- **Owned means owned *hardware* → multi-arch is load-bearing, not dilution.** The wedge is "an
  OS you own, not a cloud you rent." The hardware people own is increasingly ARM (Apple Silicon,
  a Raspberry Pi, an ARM home server) — a $60 owned box running your chief of staff 24/7 is the
  wedge made physical, and it needs aarch64. Multi-arch reach + the multi-instance orchestration
  that follows are planned in `docs/DEPLOYMENT-TOPOLOGY.md` (guardrail: arch never leaks into the
  agent model; state the isolation tier per device so breadth doesn't outrun trust).

## Load-bearing build priorities

The thesis rests on these. Spec/build in this order:

1. **Egress mediator** (`docs/plans/p7.5-egress-mediator.md`). The linchpin of secure AND
   observable AND framework-agnostic — all three collapse without it. Net-namespace +
   proxy-only egress + a served LLM gateway + the MCP gateway + boundary secret rewriting +
   signed audit receipts. A core, service-mesh-grade subsystem.
2. **Isolation floor** (microVM/gVisor) for the universal tier. The capability layer
   (Landlock/seccomp/ns) is not enough to host untrusted/foreign code; Firecracker or gVisor
   is the real boundary. Couples to the observability choice (eBPF vs. gVisor sink). Needed
   only for the universal tier; native agents can wait on it.
3. **Approval gate** (`docs/plans/p7.4-approval-gate.md`, spec'd). The human-in-the-loop
   control for a standing fleet.
4. **Observability — OTEL + eBPF/gVisor-sink** (`docs/OBSERVABILITY-PLAN.md`). ~half the
   product; rides *on top of* the egress mediator (you can't observe what you don't broker)
   and is coupled to the isolation floor.
5. **The Chief-of-Staff harness** (`docs/HARNESS-OPS-PLAN.md`). The flagship workload that
   proves the substrate.

Vertical-slice discipline still governs: the cheapest high-value pair to ship first is
**boundary secret rewriting + signed audit receipts** on the native tier, over one real
system (Gmail) — not the full isolation/MITM/sink pipeline at once.

## What's true today vs. what's a bet

- **True / built:** capability sandbox (Landlock/seccomp/namespaces/gVisor), `IsolateNetwork`,
  Landlock V4 net ports, multi-agent scheduler with token/$ metering, 4-tier memory with
  provenance, `/agents` FUSE control plane + write surface, MCP (stdio + HTTP/SSE), streaming
  inference, checkpoint/restore, `agentctl` TUI, templates.
- **Validated (n=1, customer-zero):** the *workload* demand — an always-on chief of staff — is
  real among technical founders, confirmed firsthand (the founder wants it; reports the same
  from many founders). The *substrate-wedge* clause is also confirmed at n=1: customer-zero
  states plainly they "would not trust a SaaS with Chief-of-Staff data." That's the strongest
  dogfood signal — the buyer who chose to build an OS rather than buy a SaaS chief of staff.
- **Still a bet (generalization + build):** n=1 + hearsay is justification to build, not proof
  it generalizes. The universal-tier governance + isolation floor are partly to-build.
  **Durable framing — anchor here, not on a macro call:** the bet is *not* "SaaS is dead"
  (overstated, and it undercuts the wedge — you need SaaS to exist as the distrusted default;
  and most agent products are themselves SaaS). The bet is **"a high-trust slice of work will
  never go to multi-tenant SaaS and wants an owned substrate"** — true whether SaaS broadly
  thrives or shrinks. **Next real-world step:** ask 2-3 other founders the same concrete
  question and listen for whether *they* reach for "I won't put it in their cloud" unprompted.
  3 of 3 → wedge, not hunch.

## Provenance

Full diagnostic + the four adversarial turns + the codex cold read:
`~/.gstack/projects/0x89karan-runtime1/0x89karan-main-design-20260621-235446.md`.
