# runtime1 — Product Language (one page)

The canonical way to name and describe this project, everywhere: site, docs, README,
talks, issues. When copy drifts from this page, fix the copy or fix this page — never
let them disagree silently.

## The three names

| Name | What it names | Write it as |
|---|---|---|
| **runtime1** | The open-source project as a whole: OS + runtime + `agentctl` + tool servers + verification pipeline + docs. "The repo," "the experiment." | always lowercase, even at sentence start |
| **agentOS** | The operating system: a minimal Linux base whose entire userspace is the agent runtime. The artifact the project exists to build. | lowercase `agent`, capital `OS` |
| **agentd** | The runtime: a ~2 MB size-optimized Rust daemon that boots as PID 1 and *is* the userspace. Today it also runs as an ordinary binary on a normal distro. | always lowercase, code-styled (`agentd`) when referring to the binary |

Rules of use:

- The **project** does things like exist, ship, document, and test a thesis → *runtime1*.
- The **OS** is what agents live inside; it replaces the process/application model → *agentOS*.
- The **runtime** schedules, enforces, meters, records, and checkpoints → *agentd*.
- Don't substitute one for another. "runtime1 enforces capabilities" is wrong; agentd does.
  "agentd is an experiment" is wrong; runtime1 is.
- Supporting names: `agentctl` (operator CLI + TUI cockpit), the **flight recorder**
  (append-only event log), the **evidence chain** (signed receipts), the **morning brief**
  (the reference workload's artifact), the **chief of staff** (the reference workload — never
  "the product").

## Canonical descriptions

**One-liner.** runtime1 is an open-source project building agentOS: an operating system where
agents are the primitive, not applications.

**Short (elevator).** runtime1 builds agentOS, a Linux-based operating system whose primitive
is the agent, not the application, and agentd, its ~2 MB Rust runtime that boots as PID 1.
Every agent is bounded by the capabilities it was granted, every tool and model call is
brokered, metered, and recorded, and everything an agent does lands in an evidence trail you
can verify — so agents can run unsupervised on systems that matter, on hardware you own.

**Long (about page / talk intro).** Add to the short version: cognition is remote by design —
the device is a thin agent host and there are no local model weights, which is what keeps the
OS genuinely light and lets it run on hardware people own. It is single-tenant by design — one
person's agents, mutually trusting; capability scoping is least-privilege hygiene, not a
tenancy boundary. It is an experiment, dogfooded daily against a real inbox, built in the
open, with nothing to buy and nothing to sign up for — and it is simultaneously an argument
that might be wrong.

## The claim vocabulary (the house rule)

Every capability claim carries exactly one label, in all copy:

- **enforced** — code stops you; tests cover it.
- **declared** — a config or prompt says so; nothing beneath it enforces it.
- **not built** — specced or considered, does not exist.

Enforcement is per-platform (kernel sandboxing is real on Linux x86_64, unavailable on the
Mac Docker path) — say which platform, or say both.

## Words we use, words we refuse

| Say | Never say | Why |
|---|---|---|
| defense-in-depth plus accountability | "watertight," "impenetrable," "hermetic" | the model channel is itself a covert channel; sophisticated readers distrust absolutes |
| capability layer (Landlock/seccomp/namespaces) | "isolation boundary" for that layer | it shares the host kernel; the isolation floor is a microVM or gVisor |
| self-attested, tamper-evident receipts | "forensic evidence," "proof of enforcement" | the signer is the audited party; completeness is a boundary property no proof can grant |
| "policy-consistency of the published, custody-anchored log" (future zk claim) | "mathematical certainty that policy was enforced" | the proof inherits the log's completeness limits |
| the reference workload / worked example (chief of staff) | "the product" | it exists to stress the substrate, not to be sold |
| an open-source experiment | "the company," founder language | there is nothing to buy; the site says so on purpose |
| deferred, not bricked (budget exhaustion) | "killed," "terminated" (unless it truly ends the agent) | precision about the fuse builds trust in it |

## Tone

State limits before anyone asks. Corrections stay in the history, not edited out. Numbers over
adjectives ("~2 MB, CI-guarded" beats "tiny"). If a sentence would survive on a competitor's
landing page unchanged, sharpen it until it wouldn't.
