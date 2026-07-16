# Build-session prompts

Pasteable kickoffs that hand a scoped chunk of the roadmap to a fresh build session. Each points at
the authoritative plan in `docs/plans/`. **Start with the master queue.** Completed prompts are kept
and marked `✅ Shipped` for reference (see the banner at the top of each).

## Active / pending

| Prompt | What | Status |
|---|---|---|
| **`12-build-queue-single-lane.md`** | **Master queue** — dx.6 → cheap wins → ux.0 (solo) → split into CoS + cockpit lanes | **active — start here** |
| **`14-full-system-audit.md`** | **Full end-to-end audit** — tactical + structural + strategic; 11 dimensions, Skills (Phase 11) deep-dive, future-directions, `docs/plans/`-style deliverable. Seed: `14-full-system-audit-findings.md` | on-demand (fresh session) |
| **`13-parallel-dev-rules.md`** | **Rules of engagement when 2+ sessions build in parallel** — lanes, worktree isolation, the gstack loop, merge discipline | **standing — give to every session** |
| `09-ux-cockpit.md` | Cockpit lane (Track UX) detail — resume at ux.9 after ux.0 merges | pending (post-split) |
| `10-skills-subsystem.md` | Phase 11 — skills subsystem | future |
| `70-custom-harness-prompt.md` | Requirements source for the personal-ops harness (referenced by `docs/HARNESS-OPS-PLAN.md`) | reference, not a kickoff |

## Shipped / historical (kept for reference)

| Prompt | Shipped as |
|---|---|
| `01-audit.md` | Phase 4.6 (v0.16.0) |
| `02-memory-design.md` | Phase 5 memory subsystem (p5.1–p5.9) |
| `03-memory-roadmap.md` | Phase 5 (p5.1–p5.9) |
| `04-interface.md` | Phase 6 (p6.1–p6.8) |
| `05-runbook.md` | produced `RUNBOOK.md` |
| `06-cred31-hardening.md` | cred.3.1 (v0.61.0, #85) |
| `07-cred32-completion.md` | cred.3.2 (v0.62.0, #87) |
| `08-mac-docker-cos-first-run.md` | dx.4b (v0.71.0, #96) |
| `11-cred6-credential-resilience.md` | ♻️ superseded — split into cred.6 (broker migration) + cred.7 (resilience); see prompt 12 |
