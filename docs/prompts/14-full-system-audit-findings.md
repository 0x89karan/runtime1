# Session findings — tactical seed for the AgentOS audit

Concrete, already-confirmed patterns from one live debugging session (fixing
the v0.86.2 `write_file`/FsWrite Docker bug). Useful as a prior for the
capability-system and CI/test-coverage dimensions of the audit, but these are
tactical, not structural or strategic — don't let them dominate the audit or
substitute for the broader dimensions in the main prompt.

1. **The "two files must agree" bug class.** Two real bugs in recent history
   (a Gmail credential-tier mismatch, a relative/absolute `write_file` path
   mismatch) were both caused by a literal hand-duplicated across files with no
   automated check that they stayed in sync. Search for other instances of
   this general pattern and propose single-sourcing or an assertion test for
   each instance found.
2. **LLM self-reports are not reliable operational signal.** An always-on
   orchestrator agent self-reported a false claim about its own capabilities
   in this session (verified false against the flight log — it claimed child
   spawning was disabled when the log showed three successful spawns). Is
   there a tooling gap here — should there be a lightweight, structural way to
   cross-check an agent's self-narration against ground truth automatically,
   rather than requiring manual log inspection every time something looks off?
3. **Fail-closed capability denials are silent by default.** Nothing surfaces
   `capability_denied` events proactively to an operator today. For a system
   whose core safety property is "agents fail closed," silent denial is a
   real usability and security-audit risk.
4. **Generated-config fail-fast guards are cheap and worth generalizing** to
   every code path that rewrites/lowers a config file at runtime. (One was
   added in `docker/entrypoint.sh`'s `cos)` case as part of the v0.86.2 fix —
   check whether equivalent generated-config paths, like the `agent)` case's
   template-to-TOML lowering, have anything similar.)
5. **Published image staleness relative to `main` has no automated guard** —
   the `v0.86.0` tag once pointed at a commit several fixes behind `main`
   with nothing catching it automatically. Worth a CI check or a documented
   cadence.
