# Phase 11 — Skills subsystem (procedural knowledge, governed)

**Subsystem:** Skills
**Increments:** skill.1 → skill.2 → skill.3
**Status:** Planned — decisions locked (2026-07-10). Not started.
**Depends on:** capability system (p3.3), templates catalogue (p6.1 — the resolver pattern to mirror),
`ShellExec` + sandbox (h7.1 / p3.3), approvals (p7.4), `distill_on_complete` (p5.6), memory paging (p5.2).
**Relationship to other tracks:** orthogonal to the UX cockpit and the connectors track; the cockpit
*surfaces* skills (spawn-with-skills, loaded-skill in the activity view) but does not define them.

## Goal

Give agents a **procedural layer**: packaged, reusable *recipes* an agent loads on demand and
executes using its tools — distinct from capabilities (permission), tools (actions), templates
(identity), and memory (knowledge). AgentOS's differentiator is that skill execution is
**capability-scoped, sandboxed, and flight-recorded** — a governed skills host, not a trust-by-default
runtime. Optionally, skills can be **synthesized from successful runs** (Hermes-style self-improvement)
without touching model weights — respecting the remote-cognition lock.

## The layer this fills

| Layer | Primitive | Answers |
|---|---|---|
| Permission | `Capability` | May I act? |
| Action | Tool (MCP) | What can I do? |
| Identity | Template | Who am I? |
| Knowledge | Memory / KB | What do I know? |
| **Procedure** | **Skill (this subsystem)** | **How do I do this task?** |

A skill is a **recipe that composes the agent's tools**. It sits *above* tools, *orthogonal* to
templates (one agent loads many skills). Templates define an agent; skills are portable across agents.

## Format (locked): Anthropic Agent Skills

Adopt the portable **`SKILL.md`** format — same reasoning as "MCP is the tool ABI": be a great *host*
for the ecosystem's standard rather than invent a parallel one. A skill is a directory:

```
my-skill/
  SKILL.md          # YAML frontmatter (name, description, when-to-use, license) + markdown body
  scripts/          # optional bundled executables (skill.2)
  resources/        # optional assets the body references (skill.2)
```

**Progressive disclosure** is the core mechanic: the agent always sees each skill's *name +
description* (cheap), loads the *body* only when relevant, and can run bundled *scripts* on demand.
Every Claude Code / claude.ai skill becomes loadable; the sandbox is what makes running it safe.

## ⚠ Naming collision — do not reuse the field

`AgentCard.skills: Vec<String>` (`config.rs:450`) already exists — but those are **free-form
advertising *tags*** for A2A agent discovery (e.g. `["research","write"]`), NOT loadable procedures.
**Leave `AgentCard.skills` exactly as-is.** The new concept is `Capability::Skill` + a skills
*catalogue*; grants live under `[capabilities]`. Never overload the card's tag field.

## Locked decisions (2026-07-10)

- **D1 — Anthropic `SKILL.md` format** (interop over control).
- **D2 — Deny-by-default access.** A mounted skill is not loadable unless granted via
  `Capability::Skill { name }` (empty `name` = any). Consistent with the capability model; sub-agents
  get a *subset* of the parent's skill grants (least-privilege between agents).
- **D3 — Governed execution.** Skill scripts run only through `ShellExec` under
  Landlock/seccomp/gVisor + capability scoping + the flight recorder. A skill can never exceed the
  loading agent's capability envelope, even if its script tries.
- **D4 — Synthesized skills are quarantined until operator-approved.** No auto-trust of
  machine-written skills; they route through the approval gate before becoming loadable.

---

## skill.1 — Catalogue + discovery/load + `Capability::Skill` (the substrate)

**Goal:** Agents can discover and load instruction-only skills, gated by capability. No script
execution yet.

**Scope:**
- `agentd/src/skill.rs` (new) — mirror `template.rs`: `SkillConfig`, `SkillMeta` (name, description,
  when-to-use, license), `SkillCard`, `SkillResolver`, `SkillSource`, `SkillEntry`. Parse `SKILL.md`
  (YAML frontmatter + markdown body). `SkillResolver::from_env()` for `~/.agentos/skills/` +
  `/etc/agentd/skills/`; **path-traversal rejection + name-identity check** (copy the p6.1 template
  hardening exactly).
- **`Capability::Skill { name }`** in `capability.rs` — add to the enum + the `satisfies`/matching
  logic using the existing empty-selector-matches-any + exact-name pattern (like `KbRead { segment }`).
- Native tools (`tools/native.rs`): `skill_list` → returns name+description+when-to-use for **granted**
  skills only (progressive disclosure — cheap on context); `skill_load { name }` → returns the full
  `SKILL.md` body, **requires `Capability::Skill { name }`**, records provenance.
- Config: agent `[skills]` grants (distinct from `AgentCard.skills`); template `TemplateCapabilities`
  gains a skill-grant list; `agentctl spawn` / the spawn view expose skill grants as toggles.
- Flight events: `SkillListed`, `SkillLoaded` (+ CONVENTIONS.md taxonomy rows).
- Ship 1–2 example skills under `skills/` (e.g. a "triage-inbox" recipe for the CoS) as the first
  catalogue entries.

**Acceptance:**
- [ ] An agent with `Capability::Skill { name:"triage-inbox" }` can `skill_list` (sees it) and `skill_load` it; an agent without the grant sees neither and `skill_load` returns `is_error` (no panic, re-infers).
- [ ] A sub-agent spawned by a granted parent inherits at most a subset of skill grants (least-privilege test).
- [ ] `SkillResolver` rejects `../` traversal and name/dir mismatch (test).
- [ ] `AgentCard.skills` tag behavior is unchanged (regression test).
- [ ] `skill_list` output is name+description only (no body) — progressive disclosure verified.
- [ ] `SkillListed` / `SkillLoaded` emitted; taxonomy-completeness test passes.

---

## skill.2 — Sandboxed script execution + resources

**Goal:** Skills that bundle scripts/assets run them **under the sandbox**, capability-scoped and
recorded — the governance win.

**Scope:**
- A skill body may reference `scripts/foo.py` / `resources/…`. Loading a script-bearing skill
  **requires `ShellExec`** (in addition to the `Skill` grant); execution goes through the existing
  `shell_mcp`/`shell_exec` path under Landlock/seccomp/gVisor. **No new execution path** — reuse the
  sandbox so a skill's script can do no more than the agent's caps allow.
- Bundled **resources** are readable only via an FS capability scoped to the skill's own directory
  (auto-granted read on the loaded skill's dir; nothing wider).
- Progressive-disclosure paging: a large skill body pages into context via `memory/context.rs`
  (`MemoryPressure`) so loading a heavy skill doesn't blow the budget.
- Flight event: `SkillScriptExecuted` (or a `skill` provenance field on the existing tool-called
  event — pick one, document it).
- `THREAT_MODEL.md` §: skills (especially imported/synthesized ones) are **semi-trusted content**;
  the enforcement boundary is the sandbox + capability envelope, never the skill's own claims. A
  skill declaring caps it wasn't granted is denied, not escalated.

**Acceptance:**
- [ ] A skill whose script tries to read outside its resource dir / open a denied network port is blocked by the sandbox (test), and the denial is flight-recorded.
- [ ] Running a script-bearing skill without `ShellExec` returns `is_error`, not execution.
- [ ] A skill's bundled resource is readable; a path just outside the skill dir is not (test).
- [ ] A large skill body pages rather than exceeding the token budget (test).
- [ ] `SkillScriptExecuted` (or the provenance field) is emitted with the skill name.

---

## skill.3 — Skill synthesis from experience (governed, weightless self-improvement)

**Goal:** Turn a successful run into a candidate skill — Hermes-style "creates skills from
experience," at the file layer, **operator-gated**, without RL or model weights.

**Scope:**
- Extend `distill_on_complete` (p5.6 / `Scheduler::with_distillation`): on a successful trajectory,
  optionally infer a candidate `SKILL.md` (name, description, when-to-use, distilled procedure),
  budget-bounded, off by default (`[skills] synthesize_on_complete = false`).
- **Quarantine + approval:** the candidate lands in a `pending/` area and is **not loadable** until
  the operator approves it via the approvals surface (`request_approval` / `/agents/approvals` /
  the cockpit approvals pane). Provenance records the originating task/trajectory (`task_fp`).
- On approval → moved into the operator skills dir (or the PERSONAL/gbrain brain — see Track PERSONAL)
  and becomes grantable. On rejection → discarded, recorded.
- Flight events: `SkillSynthesized`, `SkillApproved`, `SkillRejected`.
- Cockpit tie-in (surfaced, not built here): pending synthesized skills appear in the approvals pane;
  ux.2 shows an agent's currently-loaded skill; ux.3's spawn form offers "with skills".

**Acceptance:**
- [ ] With `synthesize_on_complete=true`, a successful run emits a `SkillSynthesized` candidate in quarantine; it is **not** listable/loadable by any agent until approved (test).
- [ ] Approving it (approvals API/CLI) makes it grantable and loadable; rejecting discards it — both recorded.
- [ ] The candidate carries provenance (originating `task_fp`) (test).
- [ ] Default off: a run with the flag unset synthesizes nothing (test).

---

## Test plan (per the project's non-negotiable)

Every code item ships with a test that **fails without the fix** + adversarial verification. Focus:
capability-gating (grant/deny, subset-on-spawn), path-traversal + name-identity rejection, the
naming-collision regression on `AgentCard.skills`, sandbox denial of an over-reaching skill script,
progressive-disclosure (list=metadata-only, body pages), and the quarantine invariant (synthesized ⇒
not loadable pre-approval). Add a taxonomy-completeness test for the new event kinds.

## Sequencing

**skill.1 (substrate) → skill.2 (governed execution) → skill.3 (synthesis).** One increment per
branch; `main` shippable at each step. skill.1 alone is useful (instruction-only skills — e.g. a
CoS triage recipe). skill.2 unlocks real automation. skill.3 is the optional, most speculative tier
(ties to Track PERSONAL/gbrain) — build it only after skill.1/2 are proven in the CoS.
`/plan-eng-review` skill.2 (the sandbox-execution boundary is security-sensitive). Run
`/autoplan` → build → `/review` → `/qa` → `/ship` per increment.

## References

- Anthropic Agent Skills (`SKILL.md` format, progressive disclosure).
- Internal: `agentd/src/template.rs` (resolver pattern to mirror), `agentd/src/capability.rs`
  (`Capability` matching), `config.rs:450` (the `AgentCard.skills` tag collision), `p5.6`
  distillation (`Scheduler::with_distillation`), `p7.4` approvals, `h7.1` `ShellExec` + sandbox,
  `memory/context.rs` (paging).
- Naming: this is the **Skills subsystem** — do not fold under UX. The cockpit surfaces it; it does
  not own it.
