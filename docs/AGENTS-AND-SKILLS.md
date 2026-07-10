# Agents, Sub-agents, and Skills — conceptual reference

Reference note capturing the "what really is an agent/sub-agent, and how do skills fit" discussion
(2026-07-10). Complements `docs/DESIGN.md` (the full thesis) and `docs/plans/skills-subsystem.md`
(the Phase 11 build plan). This is the *why/what*; the plan is the *how*.

---

## 1. What an agent *is* in AgentOS

An agent is a running **`AgentTask`** in the scheduler — the OS's unit of work, the way a process is
Linux's unit ("agents are the primitive"). Concretely it is a bundle of:

- an **identity** (`id`),
- a **context** (the running message history it perceives),
- a **budget** (tokens / $ — metered and bounded; a locked invariant),
- a set of **capabilities** (`capability.rs` — `FsRead`, `FsWrite`, `KbRead`, `KbWrite`, `Credential`,
  `Spawn`, `ShellExec`, …),
- a **tool registry** (native tools + MCP servers — MCP is the tool ABI),
- a **model config** and a **tier** (`Native` = in-process cooperative; `Universal` = gVisor-isolated),
- a **memory namespace**.

It runs the perceive → infer → act loop (`AgentTask::step() → AgentEffect`) until it completes or
parks. It can be instantiated from a **template** (a preset identity) or spawned live via the
management API / `spawn_agent`.

## 2. What a sub-agent *is* — a relationship, not a type

The key insight: **there is no "agent class" vs "sub-agent class."** A sub-agent is an ordinary agent
that another agent spawned. When an agent calls the `spawn_agent` tool (which requires the `Spawn`
capability), the scheduler records, in `SchedulerState`:

- a **`parent_map`** edge (child → parent),
- a **`spawn_depths`** entry, bounded by **`max_spawn_depth`** (no infinite spawn trees),
- a **`child_seq`**-derived name,
- a **scoped capability set** — the child receives a *subset* of the parent's grants. This is the
  *one* place AgentOS enforces least-privilege, despite agents being mutually trusting (single-tenant,
  in-process). Scoping here is about least-privilege, not distrust.

Sub-agents communicate via **mailboxes** (`send_message`). Native-tier children run in the same
process under the cooperative scheduler; universal-tier children run gVisor-isolated. The
**orchestrator** (orch.1) is simply an agent parked in `AgentStatus::Waiting` between operator turns.

So: **sub-agent = agent + a parent edge + tighter capabilities.** That uniformity is the elegant
core of the "agents are the primitive" thesis.

## 3. The "skills" naming collision (read before touching skills)

`AgentCard.skills: Vec<String>` (`config.rs:450`) **already exists**, but it is **free-form
advertising *tags*** for A2A agent discovery (e.g. `["research", "write"]`) — how one agent finds
another that can do a thing. It is **not** executable, not packaged, and **not** what Claude / Hermes
mean by "skills." **Leave `AgentCard.skills` exactly as-is.** The Phase 11 concept is a *different
thing* (`Capability::Skill` + a skills catalogue); never overload the card's tag field.

## 4. What a skill *is* — the missing procedural layer

A **skill** is packaged *procedural knowledge*: a directory with a `SKILL.md` (YAML frontmatter:
name / description / when-to-use / license + a markdown body) plus optional scripts and resources,
*progressively disclosed* — the agent always sees name+description (cheap), loads the body only when
relevant, and can run bundled scripts on demand.

A skill is a **recipe that composes the agent's tools**. It belongs to a layer none of the existing
primitives occupy:

| Layer | Primitive | Answers |
|---|---|---|
| Permission | `Capability` | May I act? |
| Action | Tool (MCP) | What can I do? |
| Identity | Template | Who am I? |
| Knowledge | Memory / KB | What do I know? |
| **Procedure** | **Skill** | **How do I do this task?** |

Distinctions that matter:
- **Skill ≠ tool.** A tool is a verb (an action); a skill is a procedure that *composes* verbs.
- **Skill ≠ template.** A template defines *an agent*; a skill is portable across agents — one agent
  loads many skills.
- **Skill ≠ memory.** Memory is what an agent recalls; a skill is a reusable how-to it loads and runs.

## 5. How AgentOS makes agents compatible with skills

The fit is clean because a skill is *just files + a procedure*, and governed file execution is what
AgentOS is. The model (see Phase 11 plan for the increment breakdown):

1. **Skills as a mounted catalogue + a discovery/load tool** — mirror the templates catalogue. A
   `skill_list` surfaces name+description only (progressive disclosure); `skill_load` pulls the body
   in when relevant. Same paging discipline the memory substrate already has.
2. **Skill scripts execute under the sandbox** — bundled scripts run via `ShellExec` under
   Landlock/seccomp/gVisor + capability scoping + the flight recorder. **This is where AgentOS beats a
   bare skills runtime (Claude Code, Hermes): skill execution is capability-scoped and recorded — a
   governed host, not trust-by-default.** A skill can never exceed the loading agent's capability
   envelope, even if its script tries.
3. **Skill access is a capability** — `Capability::Skill { name }` (empty = any), matching the
   existing prefix-subset pattern. A sub-agent can only load the skills it was granted; least-privilege
   between agents, extended to procedures.
4. **Progressive disclosure via the context substrate** — a large skill body pages in via
   `memory/context.rs` (`MemoryPressure`) so loading a heavy skill doesn't blow the budget.
5. **Learned skills — self-improvement at the right layer** — AgentOS cannot RL the model weights
   (cognition is remote; a locked decision), but it *can* synthesize skills at the **file layer**:
   distill a successful trajectory (`distill_on_complete`, p5.6) into a candidate `SKILL.md`,
   **operator-approval-gated** before it is trusted (the approval gate, p7.4). This delivers
   Hermes-style "creates skills from experience" — governed, weightless, and operator-owned — without
   contradicting the constitution.

## 6. The format decision (locked)

Adopt the portable **Anthropic Agent Skills `SKILL.md` format**, for the same reason MCP is the tool
ABI: be a great *host* for the ecosystem's standard rather than invent a parallel one. Every Claude
Code / claude.ai skill becomes loadable; the sandbox is what makes running it safe.

---

*Provenance: distilled from the 2026-07-10 planning discussion that followed the Hermes Agent
comparison. Build plan: `docs/plans/skills-subsystem.md`. Cockpit that surfaces skills:
`docs/plans/ux-cockpit.md`.*
