<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260708-192344.md -->

# orch.2 — Orchestrator Hardening

**Increment:** orch.2  
**Version:** v0.70.0 (tentative)  
**Status:** Planning  
**Branch:** main (not started)  
**Depends on:** orch.1 (v0.66.0) ✅ shipped  

## CEO Dual-Voice Consensus

Both CEO voices (Codex + Claude) reviewed this plan on 2026-07-08. Consensus decisions:

| Dimension | Codex CEO | Claude CEO | Decision |
|---|---|---|---|
| ar-01 fix | **Critical misdiagnosis** — `terminal=false` on restore re-enters NeedInfer without a new user turn | **Critical** — dangerous; must encode parked state first-class | Fix: checkpoint with `terminal=true` + `waiting_agents` list; restore by re-inserting into `state.waiting`; scheduler skips terminal agents; inject triggers `resume_for_orchestration()` |
| ar-05 + ar-01 coupling | Implies correct ordering | "specify implementation order explicitly" | **ar-05 first** (split `waiting` → `orchestrated`+`waiting`), **ar-01 second** (checkpoint fields map to post-split `waiting` semantics) |
| audit-O1 templates | **Critical** — three templates non-functional today; blocks adoption | 30-min fix, before orch.2 | **Include in orch.2** — add `[capabilities].mcp` to `cron-agent`, `watcher`, `webhook-agent` templates |
| audit-C3 fsync | "batch with FORMAT_VERSION bump" | "batch with this increment" | **Include** — add `sync_all()` + parent-dir fsync in `checkpoint.rs:save()` |
| audit-C2 memory isolation | Block or add explicit rationale | Not flagged | **Defer** — memory isolation guard is a separate correctness concern unrelated to orchestration; deferred to a dedicated `mem.1` hardening increment with explicit TODOS.md note |
| FORMAT_VERSION bump | Premature without C3/C4 | Batch C3/C4 | Include C3 (fsync). C4 (inflight backup) is a behavioral change requiring more thought; deferred. VERSION 3→4 with C3 included is acceptable |
| ar-02 timeout | 500ms arbitrary, causes false 503s | Raise to 2-5s | **2s timeout** — gives scheduler time under load; `agentctl orchestrate` already handles 503 gracefully |
| ar-03 + streaming=false | Not flagged | Breaks all streaming=false orchestrate users | **Note in docs + startup warning** — orchestrate mode requires `streaming=true` (default since h7.4). Add warning if orchestrator runs with `streaming=false` |

## Goal

Close the 6 open orch.1 action remediations (filed in TODOS.md), plus fix two pre-condition bugs (audit-O1 + audit-C3) discovered by the CEO review. orch.1 shipped the interactive orchestrator (AgentStatus::Waiting, POST /api/v1/spawn/inject, `agentctl orchestrate` REPL). Several correctness bugs were filed on landing; this increment fixes them.

Priority breakdown: 2× P2 (data-integrity class), 4× P3 (usability class), 2× pre-condition fixes.

## Open orch.1 ARs (status as of 2026-07-08)

Fixed before this increment:
- ~~ar-04~~ (P3) — Fixed in v0.66.0: kill + SIGTERM trap added to entrypoint.sh
- ~~ar-08~~ (P3) — Fixed in current code: inner `'input` loop handles empty input without re-entering drain

Still open (orch.2 scope):
- **ar-01 (P2)** — Waiting agents excluded from checkpoint (`scheduler.rs:2234`, `checkpoint.rs:71`)
- **ar-05 (P2)** — `state.waiting` dual-purpose race condition (`scheduler.rs:1769,1881`)
- **ar-02 (P3)** — POST /api/v1/spawn returns 200 before scheduler confirms (`management.rs:307`)
- **ar-03 (P3)** — OrchestratorTurnComplete carries full answer text (`scheduler.rs:~1368`)
- **ar-06 (P3)** — No SSE read timeout (`orchestrate.rs:51`, `management.rs` SSE handler)
- **ar-07 (P3)** — quit/exit keywords injected instead of exiting (`orchestrate.rs` REPL loop)

Pre-condition fixes (CEO review):
- **audit-O1** — Three catalogue templates non-functional (missing `Mcp{}` capability)
- **audit-C3** — Checkpoint not durable (no fsync before/after rename)

## Bug Details

### ar-05 (P2): state.waiting dual-purpose race

**Implement first — ar-01 depends on the post-split semantics.**

`state.waiting: HashSet<String>` currently serves two roles:
1. **Spawn-time flag**: at `dispatch_spawn_agent` line 1898–1899, `state.waiting.insert(agent_id)` marks the agent as "orchestrated" at creation time.
2. **Parked flag**: at turn completion (line 1364), checked to decide whether to park; inject handler (line 1764) checks and removes it.

The bug: when inject arrives and agent is actively running inference (rapid double-inject), the inject guard checks `state.waiting.contains` which is true (agent was re-inserted at line 1782 after the prior inject), passes the check, and queues a concurrent inference — corrupting message order.

**Fix**: split into two sets:
- `orchestrated: HashSet<String>` — set at spawn time, never cleared until agent terminates
- `waiting: HashSet<String>` — set only at turn-completion park path, cleared at inject time

Inject guard checks `waiting.contains` (truly parked). Turn-completion park checks `orchestrated.contains`. Checkpoint inclusion checks `orchestrated.contains`.

**Termination cleanup** (C2 fix, Phase 4 decision): Consolidate both `state.waiting.remove(&agent_id)` and `state.orchestrated.remove(&agent_id)` into `handle_agent_terminal`. Remove the three scattered `state.waiting.remove` call sites (lines 644, 1379, 1765) — they become redundant. This gives a single cleanup point; impossible to miss a future termination path.

### ar-01 (P2): Waiting agents excluded from checkpoint

**Implement second — after ar-05 split is in place.**

`build_scheduler_checkpoint` (line 2234) filters `!a.is_terminal()`. Waiting/parked orchestrated agents have `terminal=true` after completing a turn (set in `provide_inference`). So when agentd restarts mid-REPL session, all waiting agents are dropped and the conversation history is lost.

**The misdiagnosis (both CEO voices flagged)**: The original plan proposed setting `terminal=false` in the checkpoint and restoring with `terminal=false`. This is dangerous: `from_checkpoint` would create a runnable agent; the scheduler would call `step()` on it immediately without a new inject message; the agent would re-enter `NeedInfer` and re-infer from the old conversation.

**Correct fix** (four changes working together):
1. **`AgentCheckpoint.terminal`**: the field already exists (line 71) with comment "Always false when saved". Change this: for orchestrated waiting agents, save their actual `terminal=true` state. Update the comment accordingly.
2. **`from_checkpoint`** (line 217): change `terminal: false` to `terminal: cp.terminal`. This restores the actual terminal state. On restore, a parked agent has `terminal=true` → must not be stepped until inject arrives.
3. **`build_scheduler_checkpoint`**: include agents where `state.orchestrated.contains(id)` even if `is_terminal()`. Also add `waiting_agents: Vec<String>` and `orchestrated_agents: Vec<String>` to `SchedulerCheckpoint`. On restore, re-populate `state.orchestrated` and `state.waiting` BEFORE the seed loop runs.
4. **Seed loop guard** (`scheduler.rs:~517`, the startup "kick off first effect" loop): add `if state.waiting.contains(&id) { continue; }` — parallel to the existing `parked_agent_ids` guard for approval-parked agents. Without this guard, `step()` on a terminal orchestrated agent returns `AgentEffect::Failed("step called on terminal task")`, which flows into `handle_agent_terminal` → `state.agents.remove(&agent_id)` → the restored waiting agent is immediately deleted on startup.

**Why this is safe**: `resume_for_orchestration()` (line 391) explicitly sets `terminal=false` before `step()` is called. So the inject path always resets terminal correctly. The OV-2 guard (preventing normal terminal agents from being restored as runnable) is preserved because `build_scheduler_checkpoint` only includes terminal agents that are also in `state.orchestrated` — not all terminal agents.

Bump FORMAT_VERSION: 3 → 4 (backward-compatible via `#[serde(default)]` on new fields).

### audit-C3: No fsync before/after checkpoint rename

While touching `checkpoint.rs` for ar-01, also fix the F-05 durability bug: `write_mode_600` calls `f.flush()` but not `f.sync_all()`, and the parent directory is not fsynced after the atomic rename. A crash between rename and parent-dir flush can lose the checkpoint.

**Fix**: in `write_mode_600`, add `f.sync_all().await?` after `f.flush().await`. In `CheckpointStore::save()`, after `tokio::fs::rename(tmp, &self.path).await?`, open the parent dir and call `sync_all()`.

### ar-02 (P3): Spawn response before scheduler confirms

`POST /api/v1/spawn` dispatches via `try_send(ControlMsg::Spawn {...})` and immediately returns 200. If the scheduler rejects (invalid id, duplicate, budget exceeded), the caller already has 200. `agentctl orchestrate` then hangs.

**Fix**: use a `oneshot::channel` to get confirmed `agent_id` from the scheduler. Management handler awaits the receiver (2s timeout → 503 with `Retry-After: 2`). Success returns 201 with `{"agent_id": "..."}`. The scheduler sends `Ok(agent_id)` or `Err(reason)` through the channel.

`HttpSource::spawn()` in `agentctl/src/watch/source.rs` must be updated to: (1) accept 201 (not just 200), (2) parse the response body for `data["agent_id"]`, (3) surface the error body text when the server returns 400 or 503, not a generic "spawn failed".

2s timeout (raised from the originally planned 500ms based on Claude CEO finding — 500ms causes false 503s under load).

### ar-03 (P3): OrchestratorTurnComplete carries full answer text

`EventKind::OrchestratorTurnComplete` at line ~1368 emits the full agent response. For long responses this can be 50–200 KB, inflating SSE broadcast buffers and `flight.jsonl`.

**Fix**: cap `answer` at 512 chars (matching `PREVIEW_CHARS` used elsewhere). The `agentctl orchestrate` REPL reads from this field — 512 chars is sufficient for the turn-complete signal; streaming output is already visible via `text_delta` SSE events.

**Note** (Claude CEO finding): This cap makes orchestrate mode non-functional when `streaming=false` (the full answer is the only complete text source). Orchestrate mode requires `streaming=true` — already the default since h7.4. Add a startup log warning in `orchestrate.rs` if the detected streaming state is false, and document this in the orchestrator template's comments.

### ar-06 (P3): No SSE read timeout

`agentctl orchestrate` creates the SSE client with `timeout(None)` (line 51). A TCP partition without FIN causes `reader.lines()` to block forever.

**Fix** (two parts):
1. **management.rs SSE handler**: spawn a keepalive task that writes `event: ping\ndata: {}\n\n` to the SSE channel every 30 seconds. Use `try_send` to drop pings if the channel is full (client slow) rather than blocking the keepalive task.
2. **orchestrate.rs**: set `timeout(Some(Duration::from_secs(90)))`. 90s = 3 missed pings → bail with: `"SSE stream timed out for agent '{agent_id}' — is agentd still running? Resume with: agentctl orchestrate --agent-id {agent_id}"`. The `data: {}` ping events are silently ignored in `drain_until_turn_complete` (JSON parses, `kind=""` → loop continues).

### ar-07 (P3): quit/exit keywords injected as messages

Typing "quit" in `agentctl orchestrate` sends `inject("quit")` to the agent.

**Fix**: after `let Some(next) = next else { break };`, before `source.inject(...)`, check:
```rust
if next == "quit" || next == "exit" {
    eprintln!("[orchestrate] session paused. Resume with: agentctl orchestrate --agent-id {agent_id} \"your next message\"");
    return Ok(());
}
```
Also add to the `--help` text for `OrchestrateArgs`: `"Type 'quit' or 'exit' to pause the session (agent remains parked)"`.

### ar-03 DX: truncation signal

When `answer.len() == 512` (exactly at the cap), the answer was truncated but the user sees no indication. Fix: if `answer.len() == 512`, append `"\n[output truncated — full text streamed above]"` inline before printing. This avoids a startup warning while clearly signaling truncation when it actually occurs.

### audit-O1: Three catalogue templates non-functional

`templates/cron-agent.template.toml`, `templates/watcher.template.toml`, `templates/webhook-agent.template.toml` declare MCP servers (`[[tools.mcp_servers]]`) but have no `[capabilities]` section. Deny-by-default lowering strips all their tools. These templates produce silent no-ops for any operator who tries them.

**Fix**: add `[capabilities]` section to each with `mcp = ["cron_trigger"]` / `mcp = ["fs_watch"]` / `mcp = ["webhook_trigger"]` respectively.

## Implementation Order

1. **audit-O1** — template `[capabilities]` fixes (3 template files, independent of Rust changes)
2. **ar-05** — split `state.waiting` → `orchestrated` + `waiting` in `scheduler.rs`
3. **ar-01** — checkpoint changes: `AgentCheckpoint.terminal` field semantics, `from_checkpoint` terminal restore, `SchedulerCheckpoint` new fields, `build_scheduler_checkpoint` inclusion logic; + audit-C3 fsync in same `checkpoint.rs` edit
4. **ar-02** — oneshot confirmation in `management.rs` + `scheduler.rs` Spawn handler
5. **ar-03** — cap answer in `scheduler.rs`, streaming warning in `orchestrate.rs`
6. **ar-06** — SSE ping in `management.rs`, 90s timeout in `orchestrate.rs`
7. **ar-07** — quit/exit in `orchestrate.rs`

## Scope

### Files changed

- `templates/cron-agent.template.toml` — add `[capabilities]` with `mcp = ["cron_trigger"]`
- `templates/watcher.template.toml` — add `[capabilities]` with `mcp = ["fs_watch"]`
- `templates/webhook-agent.template.toml` — add `[capabilities]` with `mcp = ["webhook_trigger"]`
- `agentd/src/checkpoint.rs` — update `AgentCheckpoint.terminal` comment; add `waiting_agents` + `orchestrated_agents` to `SchedulerCheckpoint`; audit-C3 fsync in `write_mode_600` + `CheckpointStore::save()`; bump FORMAT_VERSION 3→4
- `agentd/src/agent/mod.rs` — change `from_checkpoint` `terminal: false` → `terminal: cp.terminal`
- `agentd/src/scheduler.rs` — split `waiting` → `orchestrated`+`waiting`; fix checkpoint inclusion; cap OrchestratorTurnComplete answer; spawn oneshot channel; SSE ping keepalive
- `agentd/src/management.rs` — SSE ping heartbeat task; spawn oneshot response; raise ar-02 response code 200→201
- `agentctl/src/orchestrate.rs` — 90s SSE read timeout; quit/exit pause; truncation signal; streaming=false warning
- `agentctl/src/watch/source.rs` — `HttpSource::spawn()` handle 201, parse body, surface error text

### Files NOT changed

- No new crates or features
- No config schema changes (orch.2 adds no new TOML fields)
- No new API routes beyond the 201 response-code change on /api/v1/spawn

## Acceptance Criteria

- [ ] **audit-O1**: `agentctl spawn cron-agent --task "..."` dry-run shows `cron_trigger` in tools list (not empty)
- [ ] **ar-05**: Rapid double-inject via `POST /api/v1/agents/:id/inject` (agent mid-turn) returns 503 "agent_not_waiting" — no concurrent inference
- [ ] **ar-01**: After `agentctl orchestrate` parks an agent: `kill -TERM $(pgrep agentd)` + restart agentd → `agentctl orchestrate --agent-id orch-default "follow up"` resumes without losing history (agent ID recoverable from prior session output)
- [ ] **audit-C3**: `cargo test` passes a new `checkpoint_fsync_called` test verifying `sync_all` is invoked on both the data file and parent directory
- [ ] **ar-02**: `POST /api/v1/spawn` with an invalid id (`"../escape"`) returns 400 or 503 (not 200/201)
- [ ] **ar-03**: `flight.jsonl` entry for `orchestrator_turn_complete` has `answer` ≤ 512 chars
- [ ] **ar-06**: After 90s of agentd silence during orchestrate REPL, the CLI exits with "SSE stream timed out"
- [ ] **ar-07**: Typing "quit" at the orchestrate prompt exits gracefully without calling inject
- [ ] FORMAT_VERSION bumps to 4; 1260+ workspace tests pass

## Test Plan

- `template_cron_agent_has_mcp_cap` — load cron-agent.template.toml, assert `capabilities.mcp` non-empty
- `template_watcher_has_mcp_cap` — same for watcher
- `template_webhook_agent_has_mcp_cap` — same for webhook-agent
- `orchestrated_vs_waiting_sets_disjoint` — spawn an orchestrated agent, verify it's in `orchestrated` but NOT in `waiting` until a turn completes
- `double_inject_rejected` — agent mid-turn: inject returns "agent_not_waiting" without queuing
- `scheduler_waiting_agents_checkpointed` — park a waiting agent, call `build_scheduler_checkpoint`, verify it appears in `checkpoint.agents` with `terminal=true` and its ID is in `waiting_agents`
- `scheduler_restore_waiting_agents` — build a checkpoint with `waiting_agents=["w1"]`, `orchestrated_agents=["w1"]`, restore, verify `state.waiting.contains("w1")`, `state.orchestrated.contains("w1")`, and the agent is NOT stepped without inject
- `seed_loop_skips_waiting_agents` — restore a checkpoint containing a terminal waiting agent; verify the agent still exists in `state.agents` after the seed loop runs (i.e., the `state.waiting` guard prevented deletion via `handle_agent_terminal`)
- `combined_ar05_ar01_integration` — split sets (ar-05) → park agent → checkpoint → restore → verify both sets populated AND agent not enqueued until inject
- `from_checkpoint_restores_terminal_true` — build AgentCheckpoint with `terminal=true`, call `from_checkpoint`, verify `is_terminal() == true`
- `spawn_invalid_id_returns_error` — POST /api/v1/spawn with `"../bad"` id gets 400/503 (not 201)
- `orchestrator_turn_complete_answer_capped` — construct answer > 512 chars, verify event payload len ≤ 512
- `drain_until_turn_complete_timeout` — feed a reader that never delivers an event, verify bail after timeout
- `orchestrate_quit_exits_without_inject` — verify "quit" does not call `source.inject()`

## Not in Scope

- audit-C2 (memory isolation bypass via `agent/` prefix) — separate correctness domain; deferred to `mem.1`
- audit-C4 (inflight backup before checkpoint delete) — behavioral change requiring deeper thought; deferred
- orch.1-ar-09 (orchestrator.template.toml section name) — already fixed in v0.66.0
- Parallel orchestration (multiple orchestrated agents simultaneously) — deferred to orch.3+
- orch.1-ar-02 full synchronous spawn — implemented as best-effort oneshot with 2s timeout; full synchronous spawn is a deeper scheduler change

## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| D1 | CEO | ar-01: checkpoint terminal=true, not terminal=false | Critical correctness | "loop never panics on bad input" — must not re-infer | from_checkpoint restoring terminal=false would trigger step() without new user turn → re-inference from old conversation | terminal=false restore (original plan) |
| D2 | CEO | ar-05 before ar-01 | Implementation order | Correctness | waiting_agents checkpoint field must map to post-split `waiting` (truly-parked) semantics | Implementing simultaneously |
| D3 | CEO | Include audit-O1 in orch.2 | Scope addition | Product credibility | Three visible templates are silent no-ops; blocks adoption; 30-min fix | Defer to separate patch |
| D4 | CEO | Include audit-C3 (fsync) in orch.2 | Scope addition | "Record everything" invariant applies to checkpoint durability too | Batching fsync with FORMAT_VERSION bump avoids a near-term re-bump | Defer with separate version bump |
| D5 | CEO | Defer audit-C2 (memory isolation) | Scope exclusion | Narrow increment discipline | Memory isolation bypass is a separate correctness domain; doesn't affect orchestration safety; earmarked for mem.1 | Include in orch.2 |
| D6 | CEO | ar-02 timeout: 2s | Design parameter | "loop never panics" — don't create false 503s | 500ms causes false failures under load; 2s covers scheduler latency | 500ms (original), 5s (overkill) |
| D7 | CEO | ar-03: scope orchestrate mode to streaming=true | Scope clarification | Honest capability framing | streaming=false + orchestrate mode gives users no way to see full answers; add startup warning | Cap answer regardless of streaming state |
| D8 | CEO | Defer audit-C4 (inflight backup) | Scope exclusion | Narrow increment discipline | Behavioral change (keep .inflight until superseded) requires more thought; deferred | Include now |
| D9 | Phase 4 | T9 + T11 both included | Test depth | "new behavior has tests that fail without fix" | T9 isolates seed loop guard; T11 covers joint ar-05+ar-01 path; both independent and run on macOS | T11 only |
| D10 | Phase 4 | ar-03 truncation: inline note after answer | DX | Actionable feedback | `[output truncated — full text streamed above]` only appears when truncation actually occurs; not a startup warning | stderr warning / omit |
| D11 | Phase 4 | C2 cleanup: consolidate both waiting+orchestrated remove into handle_agent_terminal | Code hygiene | Single cleanup point prevents future omissions in new termination paths | waiting stays at call sites |

**Plan status: APPROVED** — 2026-07-08. Ready for implementation.
