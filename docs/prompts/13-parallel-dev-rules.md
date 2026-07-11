# 13 — Parallel dev: rules of engagement (standing coordination reference)

**Give this to EVERY session that builds AgentOS when more than one session is active.** It is not
a one-increment kickoff — it is the standing contract that keeps two (or more) parallel sessions from
colliding.

**Why this exists (2026-07-11):** two Claude sessions collided on `ux.0` — both landed on the *same*
increment in the *same* worktree with *opposite* approaches (async `tokio::select!` vs threads), and
the session that skipped the gstack loop pushed an async build claiming "tests pass" that had a runtime
panic (`reqwest::blocking` inside an async context on approve/deny/spawn). `/autoplan` on the same plan
caught that exact bug in under an hour. These rules turn that lesson into procedure.

Paste the block below into each session.

---

```
PARALLEL DEV — RULES OF ENGAGEMENT (read before doing anything)

Two Claude sessions are building AgentOS in parallel. Follow these exactly.

=== 1. LANES — build only your lane, never another's ===
- COCKPIT lane (owns agentctl/watch + the cockpit UX): ux.0 -> ux.9 -> ux.2 -> ux.1 -> ux.8 -> ux.3.
- COS-BACKEND lane (owns agentd/config/sidecars): cos-polish (rest) -> memory-routing -> cred.6 -> cred.7.
- Confirm which lane is YOURS before touching code. NEVER build an increment outside your lane. NEVER
  edit, commit to, or push another lane's branch. If unsure which lane you own, STOP and ask the
  operator — do not guess and build.

=== 2. WORKTREE ISOLATION — one session, one worktree, one branch ===
- Each session works in its OWN git worktree on its OWN branch. Cockpit = ../agentos-cockpit;
  backend = its own worktree (e.g. ../agentos-cos).
- NEVER work in a worktree another session is using. (Git forbids two worktrees on one branch anyway; a
  shared tree means you stomp each other's index, cargo target/, and uncommitted edits — that is exactly
  what broke ux.0.)
- Before starting: run `git worktree list` and `git branch --show-current`. If your worktree/branch shows
  commits or edits authored by a different session (check commit trailers: Co-Authored-By / Claude-Session),
  STOP and surface it — do NOT revert-and-continue in a loop, and do NOT build on top of it.

=== 3. THE GSTACK LOOP — mandatory, every increment, no substitutes ===
Run all of these, in order, as explicit skill invocations — never skip one, never replace one with a
hand-spawned subagent:
  /autoplan  ->  build  ->  /review  ->  /qa  ->  /ship
- /autoplan FIRST, before a single line of code. It is the front door: it locks the architecture decision
  with dual-voice review + operator gates.
- WHY NON-NEGOTIABLE (the ux.0 lesson): the session that skipped /autoplan built ux.0 as async
  tokio::select! and pushed it claiming "tests pass." /autoplan on the same plan caught, in under an hour,
  that reqwest::blocking (used by approve/deny/spawn) PANICS inside an async context — the exact bug that
  shipped commit had, untested. The loop is not ceremony; it catches the thing that panics in production.
- "Tests pass" is NOT "reviewed." Run /review and /qa as their own skills; do not fold them into /ship.

=== 4. MERGE DISCIPLINE (standing — see docs/prompts/12-build-queue-single-lane.md) ===
- RULE 1: only ONE PR merges to main at a time; ALWAYS rebase onto current main immediately before merging;
  NEVER merge a branch behind main.
- RULE 2: assign the version bump + CHANGELOG entry at MERGE time (based on current main), not at branch-cut.

=== 5. SHARED FILES ===
- If both lanes touch one file (e.g. cos.agents.toml), different keys -> no logic clash; whoever merges
  SECOND rebases (RULE 1 handles it).

=== 6. WHEN IN DOUBT, SURFACE — don't fight ===
Any of: a commit on your branch you didn't make, stray edits reappearing, another session's work in your
tree, ambiguous lane ownership -> STOP and report to the operator with the evidence. NEVER force-push or
revert another session's pushed work without the operator's explicit say-so.

START: confirm your lane (§1) and your isolated worktree (§2). Then run /autoplan on your next increment (§3).
```
