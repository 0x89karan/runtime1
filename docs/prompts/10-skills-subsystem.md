# 10 — Skills subsystem (Phase 11) — build-session kickoff

Paste the block below. Full plan (read it first): `docs/plans/skills-subsystem.md`. Build the
increments in order, one per branch; start with **skill.1**.

---

```
TASK: Skills subsystem (Phase 11) — the missing PROCEDURAL layer. A skill is a packaged recipe an
agent loads on demand and runs USING its tools, gated by capability and executed under the sandbox.
Full plan (read first): docs/plans/skills-subsystem.md

LAYER (why this is its own subsystem, not a UX feature)
Capability = may I act · Tool(MCP) = what can I do · Template = who am I · Memory = what I know ·
SKILL = HOW do I do this task. Skills sit above tools, orthogonal to templates (one agent, many skills).

⚠ NAMING COLLISION — DO NOT REUSE THE FIELD
`AgentCard.skills: Vec<String>` (config.rs:450) already exists but is free-form A2A advertising TAGS,
NOT loadable procedures. Leave it exactly as-is. The new concept is `Capability::Skill` + a skills
catalogue; grants live under [capabilities]. Never overload the card's tag field. Add a regression test.

LOCKED DECISIONS
- Anthropic Agent Skills SKILL.md format (interop over control — same reasoning as MCP-is-the-ABI).
  Directory: SKILL.md (YAML frontmatter name/description/when-to-use/license + markdown body) +
  optional scripts/ + resources/. Progressive disclosure: list shows name+description only; load pulls
  the body; scripts run on demand.
- Deny-by-default access via `Capability::Skill { name }` (empty name = any). Sub-agents get a SUBSET
  of the parent's skill grants (least-privilege between agents).
- Governed execution: skill scripts run ONLY through ShellExec under Landlock/seccomp/gVisor +
  capability scoping + flight recorder. A skill can never exceed the loading agent's capability envelope.
- Synthesized skills are QUARANTINED until operator-approved (no auto-trust of machine-written skills).

INCREMENTS (one per branch, in order — main shippable at each step)
  skill.1  Catalogue + discovery/load + Capability::Skill (substrate; instruction-only skills, no exec).
           New agentd/src/skill.rs mirroring template.rs (SkillResolver::from_env, path-traversal +
           name-identity rejection). skill_list (granted skills, name+desc only) + skill_load {name}
           (full body, requires the Skill cap). SkillListed/SkillLoaded events. Ship 1-2 example skills
           (e.g. a CoS triage-inbox recipe).
  skill.2  Sandboxed script execution + resources. Script-bearing skills require ShellExec; execute via
           the existing shell_mcp/sandbox path (NO new exec path). Bundled resources readable only via an
           FS cap scoped to the skill's own dir. Large bodies page via memory/context.rs. THREAT_MODEL §:
           skills are semi-trusted content; the sandbox + cap envelope is the boundary, never the skill's
           claims. /plan-eng-review this one (security-sensitive boundary).
  skill.3  Skill synthesis from experience (governed, weightless — no RL/weights; respects the
           remote-cognition lock). Extend distill_on_complete (p5.6): infer a candidate SKILL.md on a
           successful run (off by default). Candidate is QUARANTINED — not listable/loadable until the
           operator approves via the approvals surface (p7.4). Provenance = originating task_fp. Ties to
           Track PERSONAL/gbrain for where approved skills live. SkillSynthesized/Approved/Rejected events.

NON-NEGOTIABLE: every code item = fix + a test that FAILS without it + adversarial verification, not
"applied." Key tests: cap grant/deny + subset-on-spawn; traversal/name-identity rejection; AgentCard.skills
regression; sandbox denies an over-reaching skill script; list=metadata-only + body-pages; synthesized ⇒
not-loadable-pre-approval. Taxonomy-completeness test for new event kinds. Update docs/ROADMAP.md (check
off), CONVENTIONS.md (event rows), THREAT_MODEL.md (skill.2) in the same PR as the code. Linux-gated code
needs `make clippy-linux` before push.

DONE (whole subsystem) = an agent granted a skill can discover it (name+desc), load its procedure, and run
its bundled scripts under the sandbox (capability-scoped, flight-recorded); a sub-agent only gets a subset;
and a skill synthesized from a successful run is quarantined until the operator approves it. Per-increment:
/autoplan → build → /review → /qa → /ship. Build skill.3 only after skill.1/2 prove out in the CoS.
```
