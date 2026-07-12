<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/lane-cos-backend-autoplan-restore-20260711-203643.md -->
# cos-polish #5 + #7 — inbox budget + orchestrator max_turns

**Branch:** `lane/cos-backend`
**Worktree:** `agentos-cos`
**Depends on:** dx.6 (v0.76.0) — on main

## Problem

Two config values in the CoS system are miscalibrated:

**#5 — Inbox token budget too low:**
The orchestrator spawns the inbox scout with `token_budget = 500_000`. Observed live spend
is ~820k tokens (reading 50 Gmail messages, summarising, writing structured JSON output).
The agent hits the budget wall mid-run and produces a truncated brief.

**#7 — Orchestrator template max_turns too low:**
`templates/orchestrator.template.toml` has `max_turns = 200`. This is appropriate for a
one-shot research task but kills a conversational or long-running orchestrator session
before it finishes. The CoS orchestrator runs 10+ turns per cycle; a chat orchestrator
needs thousands.

## Scope

### In scope
1. `agentd/cos.agents.toml` line 207 — `token_budget = 500_000` → `token_budget = 1_500_000`
2. `agentd/cos.agents.toml` line 227 — `maxResults=50` → `maxResults=20` (reduce inbox
   context accumulation; 20 messages covers the last 24 h of a normal inbox)
3. `templates/orchestrator.template.toml` line 21 — `max_turns = 200` → `max_turns = 20_000`

### Out of scope
- cos-polish #4 (kb_search segment scope) — needs Rust verification
- cos-polish #8 (inference retry) — scheduler.rs change
- cos-polish #1/#2/#9 — already on main
- Version bump / CHANGELOG — assigned at merge time per RULE 2

## Implementation

All three are single-line config edits. No Rust changes, no new tests needed.
No binary size impact. No CI risk.

## Acceptance criteria
- `agentd/cos.agents.toml` has `token_budget = 1_500_000` for the inbox spawn
- `agentd/cos.agents.toml` has `maxResults=20` in the Gmail API URL
- `templates/orchestrator.template.toml` has `max_turns = 20_000`
- `cargo clippy` passes (no Rust touched)
- `cargo test` passes (no logic changed)
