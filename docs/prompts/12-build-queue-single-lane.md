# 12 — Build queue: dx.6 → cheap wins → ux.0 → split (build-session kickoff)

**Paste the block below to start / resume the build session.** This is the current master queue
(2026-07-11, after PR #103). It supersedes the old `11-cred6-credential-resilience.md` (that increment
was split by the CEO review into `cred.6` broker migration + `cred.7` resilience).

**Plans referenced:** `docs/plans/{cos-polish,memory-routing,cred.6-broker-migration,cred.7-credential-resilience,ux-cockpit}.md`.
**Sequencing rationale:** one lane until the ux.0 refactor is merged, then split into two isolated
worktrees. ux.0 churns the whole `agentctl/src/watch/` tree, so landing it solo first avoids
coordinating a big refactor across two sessions. See `docs/plans/ux-cockpit.md` "Sequencing".

## Merge discipline (STANDING — every branch, every lane)

Learned the hard way: cheap-wins (v0.75.0) merged ahead of dx.6 (v0.74.0) because each branch
pre-baked its own version and they merged out of order, stranding dx.6 in a DIRTY PR and inverting
the version. These two rules make that impossible — they apply to **every** branch, single-lane or
parallel:

- **RULE 1 — Serialize the *merge*, not the *build*.** Build branches in parallel if you like, but
  only **one PR merges to `main` at a time**. **Always rebase onto current `main` immediately before
  merging.** **Never merge a branch that is behind `main`.**
- **RULE 2 — Assign the version bump + CHANGELOG entry at *merge* time**, based on current `main` —
  **not** when you cut the branch. Pre-baked versions are what caused the 0.74/0.75 inversion.

When two lanes touch the same file (notably `cos.agents.toml` — ux.0 edits `[management] bind_addr`,
cos-polish #1 edits `FsWrite`, cred.6 edits `[credential_gateway]`): different keys, no logic clash;
RULE 1 (rebase-before-merge) resolves it — whoever merges second rebases.

---

```
Work the CoS cluster as ONE lane until the ux.0 refactor is merged, then split into two. Repo discipline: one increment per branch; /autoplan → build → /review → /qa → /ship; main shippable at each step; update docs/ROADMAP.md in the same PR; `make clippy-linux` before pushing Linux-gated code (agentctl/surfaces).
MERGE DISCIPLINE (every branch): RULE 1 — only one PR merges to main at a time; ALWAYS rebase onto current main immediately before merging; NEVER merge a branch behind main. RULE 2 — assign the version bump + CHANGELOG entry at MERGE time (based on current main), not at branch-cut. (These prevent the out-of-order version inversion that stranded dx.6.)

STEP 0 — Sync main (PR #103 merged as e35376d6; make sure your local main is current).
- Commit or stash WIP, then: `git checkout main && git pull --ff-only origin main`.
- Confirm the merged planning docs are present: docs/plans/{cred.6-broker-migration,cred.7-credential-resilience,cos-polish,memory-routing,ux-cockpit}.md and the updated ROADMAP (Phase 10 + Track UX).

STEP 1 — dx.6: fast local dev-image loop + on-demand multi-arch publish (do FIRST — it unblocks everything below).
Problem: publish-docker runs on every main push and QEMU-builds arm64 (~60–90 min), so every increment waits on CI for a pullable image.
Scope:
  a) `make dev-image` = `docker build --target runtime-full -t agentos:dev .` — NATIVE single-arch (arm64 on Apple Silicon), no QEMU/amd64. Add a buildkit cargo cache mount to the Dockerfile builder stage (`RUN --mount=type=cache,target=/src/target ...`) so post-source-change rebuilds are incremental. Add an `AGENTOS_IMAGE` / compose override so `cos` runs against the local tag.
  b) Gate `.github/workflows/ci.yml` `publish-docker`: change its trigger from every-main-push to `workflow_dispatch` and/or a `v*` tag (keep the test jobs on main pushes so main stays green).
  c) DEPLOYMENT.md: add the local-dev-image quickstart + a "cut a release image" step (dispatch/tag), and state the tradeoff — `:latest` no longer auto-updates per merge; publish once per batch.
  d) TODOS.md: defer native ARM64 CI runners (private repo → paid; revisit if we go public or want fast releases).
Acceptance: a Rust source change → `make dev-image` yields a runnable arm64 image in ~2 min cached, no CI; merging to main does NOT trigger the multi-arch publish; publish-docker still produces the full manifest when dispatched/tagged. /plan-eng-review the CI trigger change.

STEP 2 — Cheap wins (parallel, orthogonal, trivial — verify each with `make dev-image`):
  - cos-polish #9: publish the Google OAuth app to Production (kills the weekly 7-day Testing-mode expiry) + fix MCP_SERVERS.md:119 + docs.
  - secret-redaction (cred.6 P0): strip token-endpoint bodies at oauth_mcp.py:430 + credential/mod.rs:341,371; folds cred.5-ar-01. Ships independently.
  (Optional to fold in here since it's trivial config and the single most visible fix: cos-polish #1 — add `FsWrite {prefix="./output"}` to cos-orchestrator so the brief actually lands as a file.)

STEP 3 — ux.0 SOLO (the async single-loop refactor). Do this alone, land it, and MERGE before starting any parallel work — it churns the whole agentctl/src/watch/ tree and is a rebase magnet.
  Scope per docs/plans/ux-cockpit.md ux.0: convert `agentctl watch` sync-poll → one `tokio::select!` loop (keys + persistent /api/v1/events SSE + ~30ms render tick), DataSource pushes into an mpsc channel, bounded event ring; host-loopback reachability (cos deployment binds management 0.0.0.0 + publishes 127.0.0.1:7999:7999; agentd DEFAULT bind stays 127.0.0.1). Preserve every existing view's behavior (the p1.1-style behavior-preserving refactor) and --plain. /plan-eng-review required (behavior-preserving + unauthenticated-API-on-host-loopback THREAT_MODEL note).

STEP 4 — After ux.0 is merged, split into TWO lanes in SEPARATE git worktrees (never two sessions in the same tree — shared index + cargo target/ will stomp). Partition:
  CoS lane (agentd/config/sidecar — no watch/ overlap):
    cos-polish rest (#1 if not done, #2, #4, #5, #7, #8) → memory-routing → cred.6 (broker migration; config flip + auth-retest gate, do NOT re-break v0.73.2 auth) → cred.7 (resilience).
  Cockpit lane (agentctl client, rebased on the ux.0 refactor):
    ux.9 → ux.2 (fold cos-polish #3 memory-pane colon-segments here — it's watch/memory.rs) → ux.1 (fold cos-polish #6 orchestrate-REPL fix here — ux.1 refactors orchestrate.rs) → ux.8 → ux.3.
  Coordination: cos.agents.toml (both copies) is edited by ux.0 (bind_addr, already landed), cred.6 (credential block), and cos-polish #1 (FsWrite) — rebase cred.6 last; different keys, so no logic clash, just merge ordering.

Start at STEP 0 and STEP 1. Run /autoplan on dx.6 before building.
```

---

## After the split

Once ux.0 is merged and you split into two lanes, the per-lane detail lives in:
- **Cockpit lane:** `docs/prompts/09-ux-cockpit.md` — resume at **ux.9** (ux.0 already landed in STEP 3 above); fold cos-polish #3/#6 in as noted.
- **CoS lane:** the plan docs directly (`cos-polish.md`, `memory-routing.md`, `cred.6-broker-migration.md`, `cred.7-credential-resilience.md`) — no separate prompt needed; work them in order.
- **Later (Phase 11):** `docs/prompts/10-skills-subsystem.md`.
