#[cfg(test)]
pub mod driver;

use serde_json::json;

pub const PREVIEW_CHARS: usize = 200;

/// Cap on the Tier-2 `short_term` eviction buffer (AUDIT-v0.97 P1-3). A never-terminating agent
/// (the always-on orchestrator, parked/REPL agents) never reaches run-completion distillation —
/// short_term's only other drain — so without this it grows unbounded and is cloned into every
/// per-turn checkpoint. Generous enough that a normal completing agent distills long before it.
pub(crate) const MAX_SHORT_TERM: usize = 1000;

use crate::{
    config::{AgentConfig, ModelConfig, SpawnConfig},
    flight_recorder::{EventKind, FlightRecorder},
    inference::{Block, InferenceRequest, InferenceResponse, Msg, Role, StopReason, ToolSpec},
    memory::{
        context::{assess, estimate_context_tokens, page_count, page_turns, MemoryPressure, SOFT_THRESHOLD},
        MemItem,
    },
    tools::{ToolContext, ToolRegistry},
};

/// FNV-1a 64-bit hash of `task`, formatted as 16 lowercase hex chars.
/// Deterministic across Rust versions (no random seed). Used as the `task_fp`
/// provenance field embedded in Tier-3 memory entries.
fn task_fingerprint(task: &str) -> String {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    let mut hash = FNV_OFFSET;
    for b in task.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

#[must_use = "AgentEffect names the IO the scheduler must perform; ignoring it stalls the agent"]
pub enum AgentEffect {
    Infer(InferenceRequest),
    /// Only Block::ToolUse variants; step() filters the rest before returning.
    CallTools(Vec<Block>),
    /// Emitted when the model calls `spawn_agent` as its sole tool in a turn.
    /// The scheduler intercepts this before any tool `invoke()` is called.
    SpawnAgent { call_id: String, config: SpawnConfig },
    /// Emitted when the model calls `run_job` as its sole tool in a turn (cap.2b).
    /// The scheduler materializes the config-declared job (fixed caps + task) and delivers
    /// only a completion signal back (never the child's output). `job_id` is the only input.
    RunJob { call_id: String, job_id: String },
    /// Emitted when the model calls `send_message` as its sole tool in a turn.
    /// The scheduler delivers the message and synthesizes a ToolResult.
    SendMessage { call_id: String, to: String, content: String },
    /// Emitted when the model calls `request_approval` as its sole tool in a turn.
    /// The scheduler parks the agent until an operator approves or rejects.
    RequestApproval { call_id: String, action: crate::config::PendingActionRequest },
    Completed(String),
    /// A resettable agent whose response was truncated at `max_tokens` (budget.1-ar-01). Distinct
    /// from `Completed` so the scheduler MUST consciously role-gate it: a resident/orchestrated
    /// agent PARKS + resumes (never bricks — audit86-P0-2), while a one-shot/child FAILS (the parent
    /// sees an `is_error` result, not silently-truncated text presented as a finished answer). The
    /// `String` is the partial text (used by the resident park path, discarded on the fail path).
    CompletedTruncated(String),
    Failed(String),
}

pub struct AgentTask {
    agent_id:        String,
    cfg:             AgentConfig,
    model_cfg:       ModelConfig,
    messages:        Vec<Msg>,
    specs:           Vec<ToolSpec>,
    /// Cached tool names derived from `specs` at construction time; avoids
    /// per-element String allocation on every scheduler snapshot tick.
    tool_names:      Vec<String>,
    total_input:     u64,
    total_output:    u64,
    /// Budget-window anchor (ux.8′): lifetime spend at the current window's start.
    /// Windowed spend = `context_tokens() − window_anchor`. Kept separate from
    /// `total_input/total_output` (which stay monotonic and feed context size,
    /// snapshots, and paging) so a budget reset never corrupts those signals.
    window_anchor:   u64,
    /// True when a scheduler budget-reset window is active (ux.8′ / C1). When set,
    /// per-agent budget exhaustion must NOT terminate the agent — the scheduler
    /// defers it instead so the next window rollover revives it (park-not-terminate).
    /// Runtime-only: derived from `SchedulerConfig.budget_reset_interval > 0` after
    /// construction, not checkpointed (recomputed on restore).
    budget_resettable: bool,
    turn:            u32,
    /// None = NeedInfer state; Some = ResponseStored state.
    stored_response: Option<InferenceResponse>,
    /// Set to true after Completed or Failed to guard provide_* calls.
    terminal:        bool,
    /// Tier-2 eviction buffer: turns paged out of active context.
    pub(crate) short_term: Vec<MemItem>,
    /// Last observed pressure level; used to edge-trigger advisory events.
    /// Runtime-only — not checkpointed (resets to None on restore, which is correct).
    last_pressure: MemoryPressure,
    /// Stable 16-hex fingerprint of the initial task (FNV-1a 64-bit, deterministic).
    /// Embedded in Tier-3 provenance via ToolContext; not checkpointed (recomputed on restore).
    task_fp: String,
    /// Monotonic time of the last completed progress event (ux.2b) — anchors the read-time
    /// `Idle` attention signal. Runtime-only, NOT checkpointed: re-seeded to now in both `new()`
    /// and `from_checkpoint()`, exactly like `last_pressure`. A freshly-restored agent hasn't
    /// acted yet, so it starts fresh and is never instantly idle; `Instant` (not `SystemTime`)
    /// keeps it immune to wall-clock jumps. Converted to Unix secs at snapshot build.
    last_event_at: std::time::Instant,
    /// Latest tool error while the agent kept running (ux.2b) — drives the `Error` attention
    /// signal; auto-cleared on the next all-ok tool batch. Runtime-only, not checkpointed.
    last_error: Option<String>,
}

impl AgentTask {
    pub fn new(
        agent_id: &str,
        task: &str,
        cfg: &AgentConfig,
        model_cfg: &ModelConfig,
        specs: Vec<ToolSpec>,
    ) -> Self {
        let tool_names = specs.iter().map(|s| s.name.clone()).collect();
        Self {
            agent_id: agent_id.to_string(),
            cfg: cfg.clone(),
            model_cfg: model_cfg.clone(),
            messages: vec![Msg {
                role: Role::User,
                blocks: vec![Block::Text {
                    text: task.to_string(),
                }],
            }],
            specs,
            tool_names,
            total_input: 0,
            total_output: 0,
            window_anchor: 0,
            budget_resettable: false,
            turn: 0,
            stored_response: None,
            terminal: false,
            short_term: vec![],
            last_pressure: MemoryPressure::None,
            task_fp: task_fingerprint(task),
            last_event_at: std::time::Instant::now(),
            last_error: None,
        }
    }

    /// ux.2b: stamp a completed progress event. Called at the scheduler's universal effect
    /// choke point (`enqueue_or_defer`), so every real step refreshes it and a busy agent
    /// never false-reads `Idle`.
    pub fn mark_event(&mut self) {
        self.last_event_at = std::time::Instant::now();
    }

    /// ux.2b: seconds since the last completed progress event — converted to a Unix-secs
    /// anchor at snapshot build for the read-time `AgentSnapshot::idle_signal`.
    pub fn last_event_elapsed_secs(&self) -> u64 {
        self.last_event_at.elapsed().as_secs()
    }

    /// ux.2b: the latest still-running tool error, if any.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Mark whether a scheduler budget-reset window is active (ux.8′ / C1). Set by
    /// the scheduler right after construction/restore. When true, per-agent budget
    /// exhaustion defers (scheduler-side) rather than terminating the agent.
    pub fn set_budget_resettable(&mut self, resettable: bool) {
        self.budget_resettable = resettable;
    }

    /// Test-only: seed lifetime spend so `windowed_spent()` reflects it, without
    /// driving a full inference round. Used by scheduler budget tests (ux.8′).
    #[cfg(test)]
    pub(crate) fn test_set_spend(&mut self, tokens: u64) {
        self.total_input = tokens;
        self.total_output = 0;
    }

    /// Current turn index. The driver reads this when recording EventKind::Error
    /// after a gateway failure, to preserve the correct turn number in the log.
    pub fn turn(&self) -> u32 {
        self.turn
    }

    /// Stable fingerprint of the initial task, for Tier-3 provenance stamps.
    pub fn task_fp(&self) -> &str {
        &self.task_fp
    }

    /// Names of tools visible to this agent.
    /// The `specs` list is already capability-filtered at construction time.
    pub fn spec_names(&self) -> &[String] {
        &self.tool_names
    }

    /// Names of MCP servers this agent has Mcp-capability access to.
    /// Used by the snapshot surface to build the per-agent sandbox view.
    pub fn accessible_server_names(&self) -> Vec<String> {
        match &self.cfg.capabilities {
            None => vec![],
            Some(caps) => caps
                .iter()
                .filter_map(|cap| {
                    if let crate::capability::Capability::Mcp { server, .. } = cap {
                        Some(server.clone())
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }

    /// True when the agent has no capability constraints (capabilities = None in config),
    /// meaning it has unrestricted access to all registered MCP servers.
    pub fn is_capabilities_unrestricted(&self) -> bool {
        self.cfg.capabilities.is_none()
    }

    /// Scheduling priority from config. Higher value = runs before lower.
    pub fn priority(&self) -> u32 {
        self.cfg.priority
    }

    /// Returns a clone of the agent's capability set for use in tool dispatch.
    /// `None` = unrestricted; `Some([])` = deny all.
    pub fn cap_set_cloned(&self) -> Option<Vec<crate::capability::Capability>> {
        self.cfg.capabilities.clone()
    }

    /// Per-agent token budget. Used by the scheduler to set child budgets.
    pub fn token_budget(&self) -> u64 {
        self.cfg.token_budget
    }

    /// Set the per-agent token budget at runtime (ux.11a SetBudget). Mutates the
    /// **checkpointed** field (`cfg.token_budget`) so an operator's live change survives
    /// a restart — a runtime-only override would be silently reverted to the config value
    /// on restore (the audit86-P2-1 checkpoint-overrides-live-change class). `0` = unlimited.
    /// Returns the previous budget so the caller can report old→new.
    pub fn set_token_budget(&mut self, limit: u64) -> u64 {
        let old = self.cfg.token_budget;
        self.cfg.token_budget = limit;
        old
    }

    /// Narrow the agent's capabilities at runtime (ux.13 SetCaps, revoke/narrow-only).
    /// The scheduler validates the narrow and recomputes the tool specs from the new cap
    /// set (`registry.filtered_specs`) — `AgentTask` holds no registry, so the specs are
    /// passed in. Overwrites the **checkpointed** `cfg.capabilities` (survives restart; on
    /// restore the scheduler recomputes `filtered_specs` from it, so specs stay consistent)
    /// AND the cached model-facing `specs`/`tool_names`, so the model's tool list, the FUSE
    /// snapshot (`spec_names`), and tool dispatch all agree — otherwise the model would keep
    /// seeing tools it can no longer call. Returns the previous capability-set length.
    pub fn set_capabilities(
        &mut self,
        new_caps: Vec<crate::capability::Capability>,
        new_specs: Vec<ToolSpec>,
    ) -> usize {
        let old_len = self.cfg.capabilities.as_ref().map_or(0, |c| c.len());
        self.cfg.capabilities = Some(new_caps);
        self.tool_names = new_specs.iter().map(|s| s.name.clone()).collect();
        self.specs = new_specs;
        old_len
    }

    /// Clone of the model configuration. Used by the scheduler to seed child agents.
    pub fn model_cfg_cloned(&self) -> ModelConfig {
        self.model_cfg.clone()
    }

    /// True when the agent has reached a terminal state (Completed or Failed).
    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    /// Snapshot all agent state to a serializable checkpoint.
    pub fn to_checkpoint(&self) -> crate::checkpoint::AgentCheckpoint {
        crate::checkpoint::AgentCheckpoint {
            agent_id:        self.agent_id.clone(),
            cfg:             self.cfg.clone(),
            model_cfg:       self.model_cfg.clone(),
            messages:        self.messages.clone(),
            specs:           self.specs.clone(),
            total_input:     self.total_input,
            total_output:    self.total_output,
            turn:            self.turn,
            stored_response: self.stored_response.clone(),
            terminal:        self.terminal,
            short_term:      self.short_term.clone(),
            window_anchor:   self.window_anchor,
        }
    }

    /// Repair a restored conversation so it satisfies the Messages API's pairing rule:
    /// every `tool_use` must be answered by a `tool_result` in the following user turn.
    ///
    /// **attn.2 R2 / attn.1a-05.** A checkpoint taken while a tool call was in flight stores an
    /// assistant turn ending in an unanswered `tool_use`. Replaying it verbatim makes the very
    /// next inference fail with *"tool_use ids were found without tool_result blocks"*, the agent
    /// goes `failed`, and — because the failure path checkpoints again on shutdown — it
    /// re-poisons its own checkpoint, so every later boot repeats it. This is not a rare
    /// shutdown race: `default_checkpoint_interval_turns()` is 1 and the periodic write snapshots
    /// every agent, so while a child burns its turns, each write captures the parent mid-call.
    ///
    /// `live_call_ids` is the crux and the reason this cannot live in `from_checkpoint`, which
    /// sees only one agent's checkpoint. A dangling `tool_use` is **legitimate** when the
    /// scheduler has durable state promising to answer it later:
    ///   * `awaiting` — the parent called `run_job`/`spawn_agent`; the child's completion
    ///     delivers a `ToolResult` for that `call_id`.
    ///   * `pending_approvals` — the agent called `request_approval`; grant or reject delivers.
    ///
    /// Answering those here would be actively harmful: the real result arrives later and lands
    /// as a `tool_result` with no matching `tool_use`, which is the same API error from the
    /// other side. So live ids are left strictly alone.
    ///
    /// Only genuinely **orphaned** ids are repaired — a call interrupted before the scheduler
    /// recorded any promise to answer it. They get a synthetic error `tool_result` rather than
    /// having their blocks stripped, because stripping the only block from a tool-only assistant
    /// turn leaves `blocks: []`, which the API rejects for a different reason. Appending is also
    /// honest: the model is told the call was interrupted instead of silently losing it.
    ///
    /// Returns the repaired ids so the caller can record them.
    pub fn repair_dangling_tool_uses(
        messages: &mut Vec<Msg>,
        live_call_ids: &std::collections::HashSet<String>,
    ) -> Vec<String> {
        fn tool_use_ids(m: &Msg) -> Vec<String> {
            m.blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect()
        }

        // Plan: per assistant turn, which of its tool_use ids are unanswered.
        let mut plan: Vec<(usize, Vec<String>)> = Vec::new();
        for (i, m) in messages.iter().enumerate() {
            if m.role != Role::Assistant {
                continue;
            }
            let ids = tool_use_ids(m);
            if ids.is_empty() {
                continue;
            }

            // A turn holding even ONE live id is left entirely alone (/review, security
            // specialist). Repairing only the orphan would insert a user turn now, and the
            // live call's real result later arrives via `provide_tool_results`, which always
            // pushes a NEW user turn — giving assistant[live, orphan] / user[orphan] /
            // user[live], where the live tool_use is no longer answered in the turn
            // immediately after it. That is the same 400 this function exists to prevent.
            // Unreachable today because every awaiting-producing tool is sole-only
            // (`reject_batched_sole_tool` answers the whole batch if one is co-batched), but
            // nothing here depends on that, so do not rely on it.
            if ids.iter().any(|id| live_call_ids.contains(id)) {
                continue;
            }

            // "Answered" means answered in the IMMEDIATELY FOLLOWING user turn — the rule the
            // provider actually enforces (/review, maintainability specialist). Scanning the
            // whole history instead would treat a result that landed a turn too late as
            // satisfied and ship the same 400. That shape is reachable: `push_user_turn` and
            // an orchestration inject both put a plain user turn between an assistant
            // tool_use and its result.
            let answered: std::collections::HashSet<&str> = messages
                .get(i + 1)
                .filter(|n| n.role == Role::User)
                .map(|n| {
                    n.blocks
                        .iter()
                        .filter_map(|b| match b {
                            Block::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();

            let orphaned: Vec<String> =
                ids.into_iter().filter(|id| !answered.contains(id.as_str())).collect();
            if !orphaned.is_empty() {
                plan.push((i, orphaned));
            }
        }

        let repaired: Vec<String> = plan.iter().flat_map(|(_, ids)| ids.clone()).collect();

        // Apply back-to-front so earlier indices stay valid.
        for (idx, ids) in plan.into_iter().rev() {
            // The API requires the following user turn to BEGIN with tool_result blocks, in
            // the same order as the assistant turn's tool_use blocks. Rebuild that leading
            // run rather than prepending or appending blindly: a turn can already carry a
            // trailing `Block::Text` (an inject), and blind prepend/append breaks either the
            // leading-run rule or the same-order rule.
            let order = tool_use_ids(&messages[idx]);
            let synthetic: Vec<Block> = ids
                .into_iter()
                .map(|id| Block::ToolResult {
                    tool_use_id: id,
                    content: "Interrupted by a restart before this tool produced a result. \
                              No result is available — do not assume it succeeded; re-run it if \
                              the outcome still matters."
                        .to_string(),
                    is_error: true,
                })
                .collect();

            // A partially-answered batch already has a user turn holding the siblings'
            // results; the missing ones must join THAT turn, not a new one after it.
            let merge_into_next = messages
                .get(idx + 1)
                .map(|n| {
                    n.role == Role::User
                        && n.blocks.iter().any(|b| matches!(b, Block::ToolResult { .. }))
                })
                .unwrap_or(false);

            if merge_into_next {
                let next = &mut messages[idx + 1];
                let (mut results, others): (Vec<Block>, Vec<Block>) = next
                    .blocks
                    .drain(..)
                    .partition(|b| matches!(b, Block::ToolResult { .. }));
                results.extend(synthetic);
                results.sort_by_key(|b| match b {
                    Block::ToolResult { tool_use_id, .. } => {
                        order.iter().position(|id| id == tool_use_id).unwrap_or(usize::MAX)
                    }
                    _ => usize::MAX,
                });
                results.extend(others);
                next.blocks = results;
            } else {
                messages.insert(idx + 1, Msg { role: Role::User, blocks: synthetic });
            }
        }

        repaired
    }

    /// Restore an agent from a checkpoint, using `specs` from the current registry
    /// rather than the saved specs (guards against stale tool lists after restart).
    pub fn from_checkpoint(
        cp: crate::checkpoint::AgentCheckpoint,
        specs: Vec<ToolSpec>,
    ) -> Self {
        // Recompute task_fp from the initial task message (messages[0]) on restore.
        // Clone to owned String before moving cp.messages into the struct below.
        let task_text = cp.messages.first()
            .and_then(|m| m.blocks.first())
            .and_then(|b| if let Block::Text { text } = b { Some(text.clone()) } else { None })
            .unwrap_or_default();
        let tool_names = specs.iter().map(|s| s.name.clone()).collect();
        Self {
            agent_id:        cp.agent_id,
            cfg:             cp.cfg,
            model_cfg:       cp.model_cfg,
            messages:        cp.messages,
            specs,
            tool_names,
            total_input:     cp.total_input,
            total_output:    cp.total_output,
            window_anchor:   cp.window_anchor,
            budget_resettable: false,
            turn:            cp.turn,
            stored_response: cp.stored_response,
            terminal:        cp.terminal,
            short_term:      cp.short_term,
            last_pressure:   MemoryPressure::None,
            task_fp:         task_fingerprint(&task_text),
            // Runtime-only, re-seeded fresh on restore (like last_pressure): a restored agent
            // hasn't acted yet, so it must not read as idle the instant it comes back (ux.2b).
            last_event_at:   std::time::Instant::now(),
            last_error:      None,
        }
    }

    /// Total tokens consumed so far (input + output). Used by the snapshot.
    pub fn context_tokens(&self) -> u64 {
        self.total_input + self.total_output
    }

    /// Read-only view of the conversation history. TEST-ONLY today: the restore-repair path
    /// mutates `AgentCheckpoint.messages` directly, before `from_checkpoint` is ever called,
    /// so it does not flow through here. Kept public for assertions; the agent owns mutation.
    pub fn messages(&self) -> &[Msg] {
        &self.messages
    }

    /// Spend within the current budget window (ux.8′): lifetime minus the window
    /// anchor. This is what the per-agent budget is enforced against; a window
    /// reset advances the anchor so the delta returns toward zero without
    /// touching the monotonic lifetime counters.
    pub fn windowed_spent(&self) -> u64 {
        self.context_tokens().saturating_sub(self.window_anchor)
    }

    /// Rebase the budget window to the current spend (ux.8′ reset). Returns the
    /// pre-reset windowed spend so the caller can report old→new / emit an event.
    pub fn reset_budget_window(&mut self) -> u64 {
        let old = self.windowed_spent();
        self.window_anchor = self.context_tokens();
        old
    }

    /// Number of turn pairs currently in the Tier-2 eviction buffer.
    pub fn short_term_depth(&self) -> usize {
        self.short_term.len()
    }

    /// Bound `short_term` to `MAX_SHORT_TERM` (AUDIT-v0.97 P1-3). Ring-buffer: drop the OLDEST
    /// paged summaries beyond the cap; returns the count evicted. Keeps RAM and the per-turn
    /// `to_checkpoint` clone bounded for never-terminating agents (whose only other drain,
    /// run-completion distillation, never fires). Safe/turn-granular — these are already-evicted
    /// per-turn summaries, not live tool-call/response pairs.
    fn cap_short_term(&mut self) -> usize {
        let overflow = self.short_term.len().saturating_sub(MAX_SHORT_TERM);
        if overflow > 0 {
            self.short_term.drain(0..overflow);
        }
        overflow
    }

    /// Number of messages in the active context window.
    #[cfg(test)]
    pub(crate) fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// First `max_chars` Unicode scalar values of the agent's task string.
    pub fn task_preview(&self, max_chars: usize) -> String {
        self.cfg.task.chars().take(max_chars).collect()
    }

    /// Advance the state machine by one step.
    ///
    /// In NeedInfer state: emits Perceive (turn 0 only), then InferenceRequest,
    /// returns Infer(req). Checks MaxTurns first and returns Failed if exceeded.
    ///
    /// In ResponseStored state: emits InferenceResponse, then returns
    /// Completed / Failed(BudgetExceeded) / CallTools depending on stop_reason.
    pub fn step(&mut self, recorder: &FlightRecorder) -> AgentEffect {
        if self.terminal {
            recorder.record(
                &self.agent_id,
                Some(self.turn),
                EventKind::Error,
                json!({ "stage": "step", "error": "called on terminal task" }),
            );
            return AgentEffect::Failed("step called on terminal task".into());
        }
        if let Some(response) = self.stored_response.take() {
            self.step_with_response(response, recorder)
        } else {
            self.step_need_infer(recorder)
        }
    }

    /// Store the inference response and accumulate token counts. Emits NO events.
    /// The subsequent step() call emits InferenceResponse and dispatches further.
    pub fn provide_inference(&mut self, response: InferenceResponse, recorder: &FlightRecorder) {
        if self.terminal {
            recorder.record(
                &self.agent_id,
                Some(self.turn),
                EventKind::Error,
                json!({ "stage": "provide_inference", "error": "called on terminal task" }),
            );
            return;
        }
        self.total_input += u64::from(response.input_tokens);
        self.total_output += u64::from(response.output_tokens);
        self.stored_response = Some(response);
    }

    /// Append tool results to message history and emit the Observe event.
    /// ToolCall and ToolResult events are emitted by the driver inline (per-tool,
    /// interleaved) to preserve ToolCall₁→ToolResult₁→ToolCall₂→ToolResult₂ order.
    pub fn provide_tool_results(&mut self, results: Vec<Block>, recorder: &FlightRecorder) {
        if self.terminal {
            recorder.record(
                &self.agent_id,
                Some(self.turn),
                EventKind::Error,
                json!({ "stage": "provide_tool_results", "error": "called on terminal task" }),
            );
            return;
        }
        // ux.2b: a tool error while the agent keeps running drives the `Error` attention signal;
        // a clean batch auto-clears it. Centralized HERE (not at the scheduler dispatch site) so
        // EVERY tool-result path updates it uniformly — the async `EffectResult::Tools` batch AND
        // every synthetic `is_error` reject block (spawn-denied, run_job reject, approval reject,
        // send_message failure, no-control approval), all of which funnel through this method. An
        // empty batch (turn-advance only) leaves the prior error untouched.
        if !results.is_empty() {
            self.last_error = results.iter().find_map(|b| match b {
                Block::ToolResult { is_error: true, content, .. } => {
                    Some(content.chars().take(160).collect::<String>())
                }
                _ => None,
            });
        }
        recorder.record(
            &self.agent_id,
            Some(self.turn),
            EventKind::Observe,
            json!({ "result_count": results.len() }),
        );
        self.messages.push(Msg {
            role: Role::User,
            blocks: results,
        });
        self.turn += 1;
    }

    /// F-16: a sole-only tool (`spawn_agent` / `send_message`) was batched with
    /// other tool calls in one turn. Models routinely batch tool calls, so
    /// terminating the agent over it (the old behavior) needlessly kills the
    /// spawn/bus flows. Instead, recover: emit an `is_error` ToolResult for
    /// EVERY tool_use this turn (none are executed, to keep retries idempotent)
    /// telling the model to retry the sole tool alone, then re-infer.
    ///
    /// Every `tool_use` block must get a matching `tool_result` or the next
    /// inference request is malformed — so we answer all of `call_blocks`.
    fn reject_batched_sole_tool(
        &mut self,
        sole_name: &str,
        call_blocks: &[Block],
        recorder: &FlightRecorder,
    ) -> AgentEffect {
        let results: Vec<Block> = call_blocks
            .iter()
            .filter_map(|b| {
                let Block::ToolUse { id, name, .. } = b else {
                    return None;
                };
                let content = if name == sole_name {
                    format!(
                        "`{sole_name}` must be the only tool call in a turn. None of \
                         this turn's tool calls were executed. Retry with `{sole_name}` \
                         as the sole tool call."
                    )
                } else {
                    format!(
                        "`{name}` was not executed: it was batched with `{sole_name}`, \
                         which must be called alone. Call it in a separate turn."
                    )
                };
                Some(Block::ToolResult {
                    tool_use_id: id.clone(),
                    content,
                    is_error: true,
                })
            })
            .collect();
        self.provide_tool_results(results, recorder);
        self.step_need_infer(recorder)
    }

    /// Inject pending mailbox messages into this agent before inference.
    ///
    /// Appends each message as a Text block to the last User message in history.
    /// This avoids creating a consecutive User message (which violates the Anthropic
    /// API's strict alternating role requirement).
    pub fn inject_messages(&mut self, messages: Vec<crate::bus::MailMessage>, recorder: &FlightRecorder) {
        if messages.is_empty() {
            return;
        }
        let text = messages
            .iter()
            .map(|m| format!("[Message from {}]: {}", m.from, m.content))
            .collect::<Vec<_>>()
            .join("\n\n");

        recorder.record(
            &self.agent_id,
            Some(self.turn),
            EventKind::MessageReceived,
            json!({ "count": messages.len(), "preview": truncate(&text, PREVIEW_CHARS) }),
        );

        // Append to the last User message to avoid consecutive-User-message violations.
        if let Some(last_user) = self.messages.iter_mut().rev().find(|m| m.role == Role::User) {
            last_user.blocks.push(Block::Text { text });
        } else {
            self.messages.push(Msg {
                role: Role::User,
                blocks: vec![Block::Text { text }],
            });
        }
    }

    /// Reset terminal flag so this agent can receive and process a new turn.
    /// Used exclusively by the orchestration path after a Completed effect.
    pub fn resume_for_orchestration(&mut self) {
        self.terminal = false;
    }

    /// Push a new User turn for orchestration injection.
    ///
    /// Unlike `inject_messages` (which appends to the last User message),
    /// this creates a new User message after the Assistant response, forming
    /// the proper [User(task), Assistant(answer), User(inject)] sequence.
    pub fn push_user_turn(&mut self, text: String, recorder: &FlightRecorder) {
        recorder.record(
            &self.agent_id,
            Some(self.turn),
            EventKind::MessageReceived,
            json!({ "count": 1, "preview": truncate(&text, PREVIEW_CHARS) }),
        );
        self.messages.push(Msg {
            role: Role::User,
            blocks: vec![Block::Text { text }],
        });
    }

    fn step_need_infer(&mut self, recorder: &FlightRecorder) -> AgentEffect {
        // MaxTurns check fires BEFORE emitting InferenceRequest, matching Phase 0
        // behavior where the for-loop exits and records with max_turns-1 as turn.
        if self.turn >= self.cfg.max_turns {
            let total = self.total_input + self.total_output;
            recorder.record(
                &self.agent_id,
                Some(self.cfg.max_turns.saturating_sub(1)),
                EventKind::MaxTurnsReached,
                json!({ "max_turns": self.cfg.max_turns, "total_tokens": total }),
            );
            self.terminal = true;
            return AgentEffect::Failed(format!(
                "max turns ({}) reached without a final answer",
                self.cfg.max_turns
            ));
        }

        // Per-agent budget fail-fast (P1-1 / ux.8′). Checked BEFORE emitting the
        // inference request — the ToolUse backstop below only fires *after* an
        // overspending call, so a text-only orchestrated agent (EndTurn→park→
        // inject cycle) could accrue unbounded spend without it. Enforced on
        // WINDOWED spend so it re-arms after a budget-window reset. token_budget
        // 0 = unlimited (matches the global convention). MaxTurns takes
        // precedence (checked above) so a maxed-out agent reports "max turns".
        //
        // C1: only TERMINATE when no reset window is active (interval == 0). Under
        // a window (`budget_resettable`) the scheduler's enqueue_or_defer DEFERS an
        // over-per-agent-budget Infer instead, so the next rollover revives the
        // agent (park-not-terminate). Terminating here would strip waiting/
        // orchestrated and permanently brick a resident agent (audit86-P0-2 class).
        let windowed = self.windowed_spent();
        if !self.budget_resettable && self.cfg.token_budget != 0 && windowed >= self.cfg.token_budget {
            recorder.record(
                &self.agent_id,
                Some(self.turn),
                EventKind::BudgetExceeded,
                json!({
                    "windowed_tokens": windowed,
                    "budget":          self.cfg.token_budget,
                    "stage":           "pre_inference",
                }),
            );
            self.terminal = true;
            return AgentEffect::Failed(format!(
                "token budget exceeded ({windowed} >= {})",
                self.cfg.token_budget
            ));
        }

        // Memory pressure (p5.2; F-01 fix in p5.9). Two DISTINCT signals:
        //   - lifetime spend   → budget advisory + telemetry. Monotonic: never falls.
        //   - retained context → the PAGING decision. Falls after paging, so the
        //     loop edge-gates instead of re-paging every turn once spend crosses
        //     90% (the old bug: paging keyed on lifetime spend shredded context).
        // Paging gives N+1 relief (the next inference request is smaller); it
        // cannot reduce already-spent tokens for the current turn.
        // Advisory events are edge-triggered (fire once on transition).
        // Windowed spend (ux.8′) so the advisory re-arms after a budget reset
        // instead of staying stuck on a monotonic lifetime total.
        let total_spent = self.windowed_spent();
        let tokens_spent_pct = if self.cfg.token_budget > 0 {
            total_spent as f64 / self.cfg.token_budget as f64
        } else {
            0.0
        };

        // Budget advisory — edge-triggered on lifetime spend crossing SOFT.
        let spend_pressure = assess(total_spent, self.cfg.token_budget);
        if spend_pressure == MemoryPressure::Soft && self.last_pressure == MemoryPressure::None {
            recorder.record(
                &self.agent_id,
                Some(self.turn),
                EventKind::MemoryPressureAdvisory,
                json!({
                    "agent":            &self.agent_id,
                    "turn":             self.turn,
                    "tokens_spent_pct": tokens_spent_pct,
                    "soft_threshold":   SOFT_THRESHOLD,
                }),
            );
        }

        // Paging — driven by RETAINED CONTEXT SIZE, which shrinks after paging.
        let retained = estimate_context_tokens(&self.messages);
        let retained_pct = if self.cfg.token_budget > 0 {
            retained as f64 / self.cfg.token_budget as f64
        } else {
            0.0
        };
        if assess(retained, self.cfg.token_budget) == MemoryPressure::Hard {
            let n = page_count(&self.messages);
            if n > 0 {
                match page_turns(&mut self.messages, n, self.turn) {
                    Ok(items) => {
                        let pages_moved = items.len();
                        self.short_term.extend(items);
                        let evicted_overflow = self.cap_short_term();
                        recorder.record(
                            &self.agent_id,
                            Some(self.turn),
                            EventKind::MemoryPaged,
                            json!({
                                "agent":              &self.agent_id,
                                "turn":               self.turn,
                                "pages_moved":        pages_moved,
                                "short_term_depth":   self.short_term.len(),
                                "short_term_evicted": evicted_overflow,
                                "retained_pct":       retained_pct,
                                "tokens_spent_pct":   tokens_spent_pct,
                            }),
                        );
                    }
                    Err(e) => {
                        recorder.record(
                            &self.agent_id,
                            Some(self.turn),
                            EventKind::Error,
                            json!({ "stage": "page_turns", "error": e.to_string() }),
                        );
                    }
                }
            } else if self.last_pressure != MemoryPressure::Hard {
                // Hard retained pressure but context too short to page — log once.
                recorder.record(
                    &self.agent_id,
                    Some(self.turn),
                    EventKind::MemoryPressureAdvisory,
                    json!({
                        "agent":          &self.agent_id,
                        "turn":           self.turn,
                        "retained_pct":   retained_pct,
                        "soft_threshold": SOFT_THRESHOLD,
                        "note":           "hard pressure, context too short to page",
                    }),
                );
            }
        }
        self.last_pressure = spend_pressure;

        if self.turn == 0 {
            let preview = self
                .messages
                .first()
                .and_then(|m| m.blocks.first())
                .and_then(|b| {
                    if let Block::Text { text } = b {
                        Some(truncate(text, PREVIEW_CHARS))
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            recorder.record(
                &self.agent_id,
                None,
                EventKind::Perceive,
                json!({ "source": "task", "preview": preview }),
            );
        }

        recorder.record(
            &self.agent_id,
            Some(self.turn),
            EventKind::InferenceRequest,
            json!({
                "model":      &self.model_cfg.model,
                "msg_count":  self.messages.len(),
                "tool_count": self.specs.len(),
            }),
        );

        AgentEffect::Infer(InferenceRequest {
            system:     None,
            messages:   self.messages.clone(),
            tools:      self.specs.clone(),
            max_tokens: self.model_cfg.max_tokens,
            streaming:  self.model_cfg.streaming,
        })
    }

    fn step_with_response(
        &mut self,
        response: InferenceResponse,
        recorder: &FlightRecorder,
    ) -> AgentEffect {
        let total = self.total_input + self.total_output;

        recorder.record(
            &self.agent_id,
            Some(self.turn),
            EventKind::InferenceResponse,
            json!({
                "stop_reason":   response.stop_reason.as_str(),
                "input_tokens":  response.input_tokens,
                "output_tokens": response.output_tokens,
                "total_tokens":  total,
            }),
        );

        self.messages.push(Msg {
            role: Role::Assistant,
            blocks: response.blocks.clone(),
        });

        match response.stop_reason {
            // Legacy (no reset window): a truncated response hard-terminates. Under a reset
            // window (`budget_resettable`) this would permanently brick a resident agent on ONE
            // truncated response (audit86-P0-2 / ux.8-ar-02 self-brick class), so the resettable
            // case falls through to the recoverable turn-end arm below.
            StopReason::MaxTokens if !self.budget_resettable => {
                recorder.record(
                    &self.agent_id,
                    Some(self.turn),
                    EventKind::BudgetExceeded,
                    json!({ "total_tokens": total, "budget": self.cfg.token_budget }),
                );
                self.terminal = true;
                AgentEffect::Failed(
                    "model generation hit max_tokens limit (truncated response)".into(),
                )
            }

            // EndTurn/Other, and a RESETTABLE MaxTokens truncation (arm 1 caught the non-resettable
            // case). A resident/orchestrated agent parks and is reactivatable rather than bricked; the
            // scheduler role-gates on the returned effect — `CompletedTruncated` for a truncation (a
            // one-shot/child then FAILS instead of silently delivering partial text as a finished
            // answer), plain `Completed` for a clean turn end (budget.1-ar-01). The RECORDING below is
            // identical for both, so the resident park flow's dependencies are unchanged.
            StopReason::EndTurn | StopReason::Other(_) | StopReason::MaxTokens => {
                let truncated = matches!(response.stop_reason, StopReason::MaxTokens);
                if truncated {
                    recorder.record(
                        &self.agent_id,
                        Some(self.turn),
                        EventKind::BudgetExceeded,
                        json!({ "total_tokens": total, "budget": self.cfg.token_budget,
                                "recoverable": true,
                                "note": "max_tokens truncation; resident agent kept resumable (budget_resettable)" }),
                    );
                }
                let answer = response
                    .blocks
                    .iter()
                    .find_map(|b| {
                        if let Block::Text { text } = b {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();

                recorder.record(
                    &self.agent_id,
                    Some(self.turn),
                    EventKind::AgentCompleted,
                    json!({
                        "turns":          self.turn + 1,
                        "total_tokens":   total,
                        "answer_preview": truncate(&answer, PREVIEW_CHARS),
                        "truncated":      truncated,
                    }),
                );
                self.terminal = true;
                if truncated {
                    AgentEffect::CompletedTruncated(answer)
                } else {
                    AgentEffect::Completed(answer)
                }
            }

            StopReason::ToolUse => {
                // Backstop to the pre-inference gate in step_need_infer: windowed
                // spend, `>=`, 0 = unlimited — consistent with that gate (ux.8′).
                // C1: like the pre-inference gate, only terminate when no reset
                // window is active. Under a window the scheduler defers the next
                // Infer instead (park-not-terminate); the agent finishes the
                // current turn's tools and is deferred on its next inference.
                let windowed = self.windowed_spent();
                if !self.budget_resettable && self.cfg.token_budget != 0 && windowed >= self.cfg.token_budget {
                    recorder.record(
                        &self.agent_id,
                        Some(self.turn),
                        EventKind::BudgetExceeded,
                        json!({ "windowed_tokens": windowed, "budget": self.cfg.token_budget }),
                    );
                    self.terminal = true;
                    return AgentEffect::Failed(format!(
                        "token budget exceeded ({windowed} >= {})",
                        self.cfg.token_budget
                    ));
                }

                let call_blocks: Vec<Block> = response
                    .blocks
                    .into_iter()
                    .filter(|b| matches!(b, Block::ToolUse { .. }))
                    .collect();

                if call_blocks.is_empty() {
                    self.terminal = true;
                    return AgentEffect::Failed(
                        "model returned stop_reason=tool_use with no ToolUse blocks".to_string(),
                    );
                }

                // Intercept spawn_agent before tool dispatch — it must be the sole call.
                let spawn_idx = call_blocks.iter().position(|b| {
                    matches!(b, Block::ToolUse { name, .. } if name == "spawn_agent")
                });

                if let Some(idx) = spawn_idx {
                    if call_blocks.len() > 1 {
                        return self.reject_batched_sole_tool(
                            "spawn_agent",
                            &call_blocks,
                            recorder,
                        );
                    }
                    let Block::ToolUse { id: call_id, input, .. } = &call_blocks[idx] else {
                        unreachable!("filtered to ToolUse above")
                    };
                    let config: SpawnConfig = match serde_json::from_value(input.clone()) {
                        Ok(c) => c,
                        Err(e) => {
                            self.provide_tool_results(
                                vec![Block::ToolResult {
                                    tool_use_id: call_id.clone(),
                                    content: format!(
                                        "spawn_agent input could not be parsed: {e}"
                                    ),
                                    is_error: true,
                                }],
                                recorder,
                            );
                            return self.step_need_infer(recorder);
                        }
                    };
                    return AgentEffect::SpawnAgent {
                        call_id: call_id.clone(),
                        config,
                    };
                }

                // Intercept run_job before tool dispatch — sole call, like spawn_agent
                // (cap.2b). The ONLY input is `job_id`; caps + task come from config, so
                // there is deliberately no `capabilities`/`task`/`params` field to parse.
                let run_job_idx = call_blocks.iter().position(|b| {
                    matches!(b, Block::ToolUse { name, .. } if name == "run_job")
                });

                if let Some(idx) = run_job_idx {
                    if call_blocks.len() > 1 {
                        return self.reject_batched_sole_tool("run_job", &call_blocks, recorder);
                    }
                    let Block::ToolUse { id: call_id, input, .. } = &call_blocks[idx] else {
                        unreachable!("filtered to ToolUse above")
                    };
                    let job_id = match input.get("job_id").and_then(|v| v.as_str()) {
                        Some(j) => j.to_string(),
                        None => {
                            self.provide_tool_results(
                                vec![Block::ToolResult {
                                    tool_use_id: call_id.clone(),
                                    content: "run_job requires a string `job_id`".to_string(),
                                    is_error: true,
                                }],
                                recorder,
                            );
                            return self.step_need_infer(recorder);
                        }
                    };
                    return AgentEffect::RunJob { call_id: call_id.clone(), job_id };
                }

                // Intercept send_message before tool dispatch — must be sole call.
                let send_idx = call_blocks.iter().position(|b| {
                    matches!(b, Block::ToolUse { name, .. } if name == "send_message")
                });

                if let Some(idx) = send_idx {
                    if call_blocks.len() > 1 {
                        return self.reject_batched_sole_tool(
                            "send_message",
                            &call_blocks,
                            recorder,
                        );
                    }
                    let Block::ToolUse { id: call_id, input, .. } = &call_blocks[idx] else {
                        unreachable!("filtered to ToolUse above")
                    };
                    let to = match input["to"].as_str() {
                        Some(s) => s.to_string(),
                        None => {
                            self.provide_tool_results(
                                vec![Block::ToolResult {
                                    tool_use_id: call_id.clone(),
                                    content: "send_message requires a `to` string field".to_string(),
                                    is_error: true,
                                }],
                                recorder,
                            );
                            return self.step_need_infer(recorder);
                        }
                    };
                    let content = match input["content"].as_str() {
                        Some(s) => s.to_string(),
                        None => {
                            self.provide_tool_results(
                                vec![Block::ToolResult {
                                    tool_use_id: call_id.clone(),
                                    content: "send_message requires a `content` string field".to_string(),
                                    is_error: true,
                                }],
                                recorder,
                            );
                            return self.step_need_infer(recorder);
                        }
                    };
                    return AgentEffect::SendMessage {
                        call_id: call_id.clone(),
                        to,
                        content,
                    };
                }

                // Intercept request_approval before tool dispatch — must be sole call.
                let approval_idx = call_blocks.iter().position(|b| {
                    matches!(b, Block::ToolUse { name, .. } if name == "request_approval")
                });

                if let Some(idx) = approval_idx {
                    if call_blocks.len() > 1 {
                        return self.reject_batched_sole_tool(
                            "request_approval",
                            &call_blocks,
                            recorder,
                        );
                    }
                    let Block::ToolUse { id: call_id, input, .. } = &call_blocks[idx] else {
                        unreachable!("filtered to ToolUse above")
                    };
                    let action: crate::config::PendingActionRequest =
                        match serde_json::from_value(input.clone()) {
                            Ok(a) => a,
                            Err(e) => {
                                self.provide_tool_results(
                                    vec![Block::ToolResult {
                                        tool_use_id: call_id.clone(),
                                        content: format!(
                                            "request_approval input could not be parsed: {e}"
                                        ),
                                        is_error: true,
                                    }],
                                    recorder,
                                );
                                return self.step_need_infer(recorder);
                            }
                        };
                    return AgentEffect::RequestApproval {
                        call_id: call_id.clone(),
                        action,
                    };
                }

                AgentEffect::CallTools(call_blocks)
            }
        }
    }
}

/// Run `blocks` sequentially, emitting ToolCall + ToolResult events inline.
/// Shared by `driver::run` (single-agent shim) and `Scheduler::run` (multi-agent).
/// Per-tool errors become `Block::ToolResult { is_error: true }` — no top-level Err.
pub(crate) async fn run_tools_sequential(
    agent_id: &str,
    turn: u32,
    task_fp: &str,
    blocks: &[Block],
    registry: &ToolRegistry,
    cap_set: Option<&[crate::capability::Capability]>,
    recorder: &FlightRecorder,
) -> Vec<Block> {
    let ctx = ToolContext {
        agent_id: agent_id.to_string(),
        turn,
        task_fp: task_fp.to_string(),
    };
    let mut results: Vec<Block> = Vec::new();
    for block in blocks {
        let Block::ToolUse { id, name, input } = block else {
            continue;
        };

        recorder.record(
            agent_id,
            Some(turn),
            EventKind::ToolCall,
            json!({ "id": id, "name": name, "input_preview": truncate(&input.to_string(), PREVIEW_CHARS) }),
        );

        let (content, is_error) = match registry
            .invoke(name, input.clone(), &ctx, cap_set, recorder)
            .await
        {
            Ok(s) => {
                recorder.record(
                    agent_id,
                    Some(turn),
                    EventKind::ToolResult,
                    json!({
                        "id": id, "name": name,
                        "is_error": false,
                        "preview": truncate(&s, PREVIEW_CHARS),
                    }),
                );
                (s, false)
            }
            Err(e) => {
                let msg = e.to_string();
                recorder.record(
                    agent_id,
                    Some(turn),
                    EventKind::ToolResult,
                    json!({
                        "id": id, "name": name,
                        "is_error": true,
                        "error": truncate(&msg, PREVIEW_CHARS),
                    }),
                );
                (msg, true)
            }
        };

        results.push(Block::ToolResult {
            tool_use_id: id.clone(),
            content,
            is_error,
        });
    }
    results
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars().peekable();
    let out: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{out}…")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::driver::run;
    use crate::config::{AgentConfig, ModelConfig};
    use crate::inference::{InferenceGateway, InferenceRequest, InferenceResponse, StopReason};
    use std::sync::{Arc, Mutex};
    use tempfile::NamedTempFile;

    // ── Test helpers ─────────────────────────────────────────────────────────────

    struct MockGateway {
        responses: Arc<Mutex<Vec<InferenceResponse>>>,
    }

    impl MockGateway {
        fn new(responses: Vec<InferenceResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
            }
        }
    }

    #[async_trait::async_trait]
    impl InferenceGateway for MockGateway {
        async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(anyhow::anyhow!("MockGateway: no more responses queued"));
            }
            Ok(q.remove(0))
        }
        fn model_id(&self) -> &str {
            "mock-model"
        }
    }

    fn end_turn(text: &str) -> InferenceResponse {
        InferenceResponse {
            blocks: vec![Block::Text {
                text: text.to_string(),
            }],
            stop_reason:       StopReason::EndTurn,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        }
    }

    fn tool_use_resp(id: &str, name: &str, input: serde_json::Value) -> InferenceResponse {
        InferenceResponse {
            blocks: vec![Block::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
            }],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        }
    }

    fn agent_cfg(max_turns: u32, token_budget: u64) -> AgentConfig {
        AgentConfig {
            id:              "test-agent".to_string(),
            task:            String::new(),
            max_turns,
            token_budget,
            priority:        0,
            capabilities:    None,
            name:            None,
            description:     String::new(),
            skills:          vec![],
            tier:            crate::config::AgentTier::Native,
            command:         None,
            args:            vec![],
            isolation:       crate::config::IsolationMode::None,
            max_wall_seconds: 0,
        }
    }

    fn model_cfg() -> ModelConfig {
        ModelConfig {
            provider:  "mock".to_string(),
            model:     "mock-model".to_string(),
            max_tokens: 4096,
            streaming: false,
        }
    }

    fn recorder() -> (FlightRecorder, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let rec = FlightRecorder::new(tmp.path()).unwrap();
        (rec, tmp)
    }

    // ── Driver-level tests (async, exercise the full stack) ───────────────────

    #[tokio::test]
    async fn direct_answer_returns_text() {
        let gw = MockGateway::new(vec![end_turn("hello from mock")]);
        let reg = crate::tools::ToolRegistry::new();
        let (rec, _tmp) = recorder();

        let answer = run("a", "say hi", &agent_cfg(5, 100_000), &model_cfg(), &gw, &reg, &rec)
            .await
            .unwrap();
        assert_eq!(answer, "hello from mock");
    }

    #[tokio::test]
    async fn tool_call_cycle_resolves() {
        use crate::tools::native::register_native;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "agent data").unwrap();

        let gw = MockGateway::new(vec![
            tool_use_resp(
                "call_1",
                "read_file",
                serde_json::json!({ "path": path.to_str().unwrap() }),
            ),
            end_turn("the file says: agent data"),
        ]);

        let mut reg = crate::tools::ToolRegistry::new();
        register_native(&mut reg, &["read_file".to_string()], None, None, None, None).unwrap();
        let (rec, _tmp) = recorder();

        let answer = run(
            "a",
            "read the file",
            &agent_cfg(5, 100_000),
            &model_cfg(),
            &gw,
            &reg,
            &rec,
        )
        .await
        .unwrap();
        assert_eq!(answer, "the file says: agent data");
    }

    #[tokio::test]
    async fn failing_tool_is_returned_as_error_block_not_panic() {
        let gw = MockGateway::new(vec![
            tool_use_resp("call_1", "nonexistent_tool", serde_json::json!({})),
            end_turn("tool failed gracefully"),
        ]);
        let reg = crate::tools::ToolRegistry::new();
        let (rec, _tmp) = recorder();

        let answer = run("a", "task", &agent_cfg(5, 100_000), &model_cfg(), &gw, &reg, &rec)
            .await
            .unwrap();
        assert_eq!(answer, "tool failed gracefully");
    }

    #[tokio::test]
    async fn budget_exceeded_returns_error() {
        let gw = MockGateway::new(vec![
            tool_use_resp("c1", "no_tool", serde_json::json!({})),
            tool_use_resp("c2", "no_tool", serde_json::json!({})),
        ]);
        let reg = crate::tools::ToolRegistry::new();
        let (rec, _tmp) = recorder();

        let err = run("a", "task", &agent_cfg(10, 20), &model_cfg(), &gw, &reg, &rec)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("budget exceeded"), "got: {err}");
    }

    #[tokio::test]
    async fn max_turns_reached_returns_error() {
        let gw = MockGateway::new(vec![
            tool_use_resp("c1", "no_tool", serde_json::json!({})),
            tool_use_resp("c2", "no_tool", serde_json::json!({})),
        ]);
        let reg = crate::tools::ToolRegistry::new();
        let (rec, _tmp) = recorder();

        let err = run("a", "task", &agent_cfg(2, 1_000_000), &model_cfg(), &gw, &reg, &rec)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("max turns"), "got: {err}");
    }

    #[tokio::test]
    async fn inference_error_propagates() {
        struct FailGateway;
        #[async_trait::async_trait]
        impl InferenceGateway for FailGateway {
            async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
                Err(anyhow::anyhow!("network down"))
            }
            fn model_id(&self) -> &str {
                "fail-model"
            }
        }
        let reg = crate::tools::ToolRegistry::new();
        let (rec, _tmp) = recorder();

        let err = run("a", "task", &agent_cfg(5, 100_000), &model_cfg(), &FailGateway, &reg, &rec)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("network down"), "got: {err}");
    }

    #[test]
    fn truncate_helper_respects_char_boundary() {
        let s = "abcdefghij";
        assert_eq!(truncate(s, 5), "abcde…");
        assert_eq!(truncate(s, 20), "abcdefghij");
        let unicode = "áéíóú";
        assert_eq!(truncate(unicode, 3), "áéí…");
        assert_eq!(truncate(unicode, 5), "áéíóú");
    }

    #[tokio::test]
    async fn tool_call_event_emits_input_preview_field() {
        let tmp = NamedTempFile::new().unwrap();
        let rec = crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap();
        let registry = crate::tools::ToolRegistry::new();

        let blocks = vec![Block::ToolUse {
            id:    "call_1".to_string(),
            name:  "unknown_tool".to_string(),
            input: serde_json::json!({ "key": "value" }),
        }];
        run_tools_sequential("agent", 0, "", &blocks, &registry, None, &rec).await;

        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let event: serde_json::Value = content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .find(|e: &serde_json::Value| e["kind"] == "tool_call")
            .expect("tool_call event missing");

        assert!(event["data"].get("input_preview").is_some(), "must have input_preview");
        assert!(event["data"]["input"].is_null(), "must NOT have bare input field");
    }

    #[tokio::test]
    async fn tool_call_event_truncates_long_input() {
        let tmp = NamedTempFile::new().unwrap();
        let rec = crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap();
        let registry = crate::tools::ToolRegistry::new();

        let long_val = "x".repeat(300);
        let blocks = vec![Block::ToolUse {
            id:    "call_1".to_string(),
            name:  "unknown_tool".to_string(),
            input: serde_json::json!({ "content": long_val }),
        }];
        run_tools_sequential("agent", 0, "", &blocks, &registry, None, &rec).await;

        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let event: serde_json::Value = content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .find(|e: &serde_json::Value| e["kind"] == "tool_call")
            .expect("tool_call event missing");

        let preview = event["data"]["input_preview"].as_str().expect("input_preview must be string");
        assert!(preview.ends_with('…'), "long input must end with ellipsis");
        assert!(preview.chars().count() <= PREVIEW_CHARS + 1, "preview must not exceed PREVIEW_CHARS");
    }

    #[tokio::test]
    async fn tool_result_error_event_truncates_long_error_message() {
        struct LongErrorTool;

        #[async_trait::async_trait]
        impl crate::tools::Tool for LongErrorTool {
            fn name(&self) -> &str { "long_error_tool" }
            fn description(&self) -> &str { "always fails with a long error" }
            fn input_schema(&self) -> serde_json::Value { serde_json::json!({}) }
            async fn invoke(&self, _: serde_json::Value, _ctx: &crate::tools::ToolContext) -> anyhow::Result<String> {
                anyhow::bail!("{}", "e".repeat(PREVIEW_CHARS + 100))
            }
        }

        let tmp = NamedTempFile::new().unwrap();
        let rec = crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap();
        let mut registry = crate::tools::ToolRegistry::new();
        registry.register(Box::new(LongErrorTool)).unwrap();

        let blocks = vec![Block::ToolUse {
            id:    "call_err".to_string(),
            name:  "long_error_tool".to_string(),
            input: serde_json::json!({}),
        }];
        run_tools_sequential("agent", 0, "", &blocks, &registry, None, &rec).await;

        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let event: serde_json::Value = content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .find(|e: &serde_json::Value| e["kind"] == "tool_result" && e["data"]["is_error"] == true)
            .expect("tool_result error event missing");

        let error_str = event["data"]["error"].as_str().expect("error field must be a string");
        assert!(error_str.ends_with('…'), "long error must end with ellipsis");
        assert!(error_str.chars().count() <= PREVIEW_CHARS + 1, "error must not exceed PREVIEW_CHARS");
    }

    // ── State-machine unit tests (sync, no network) ───────────────────────────

    #[test]
    fn provide_tool_results_sets_and_clears_last_error() {
        // ux.2b: a tool error in the batch → last_error set (drives Error attention); a clean
        // batch → cleared (auto-clear); an empty batch → left unchanged. Centralizing here means
        // every synthetic reject/error path (spawn-denied, run_job reject, …) gets it for free.
        let (rec, _tmp) = recorder();
        let cfg = agent_cfg(5, 1_000_000);
        let mut sm = AgentTask::new("err-test", "task", &cfg, &model_cfg(), vec![]);
        assert_eq!(sm.last_error(), None);

        sm.provide_tool_results(
            vec![Block::ToolResult { tool_use_id: "t1".into(), content: "boom: refused".into(), is_error: true }],
            &rec,
        );
        assert_eq!(sm.last_error(), Some("boom: refused"), "an is_error result must set last_error");

        // Empty batch leaves it unchanged.
        sm.provide_tool_results(vec![], &rec);
        assert_eq!(sm.last_error(), Some("boom: refused"), "an empty batch must not clear last_error");

        // Clean batch auto-clears.
        sm.provide_tool_results(
            vec![Block::ToolResult { tool_use_id: "t2".into(), content: "ok".into(), is_error: false }],
            &rec,
        );
        assert_eq!(sm.last_error(), None, "an all-ok batch must clear last_error");
    }

    #[test]
    fn step_machine_text_tool_text_cycle() {
        let (rec, _tmp) = recorder();
        let cfg = agent_cfg(5, 1_000_000);

        let mut sm = AgentTask::new("sm-test", "do the thing", &cfg, &model_cfg(), vec![]);

        // Turn 0: step → Infer
        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Infer(_)), "expected Infer on turn 0");

        // Provide tool-use response
        sm.provide_inference(
            tool_use_resp("tu_1", "dummy", serde_json::json!({})),
            &rec,
        );

        // step → CallTools
        let eff = sm.step(&rec);
        let AgentEffect::CallTools(blocks) = eff else {
            panic!("expected CallTools after tool-use response");
        };
        assert_eq!(blocks.len(), 1, "expected one ToolUse block");

        // Provide tool results → advance to turn 1
        sm.provide_tool_results(
            vec![Block::ToolResult {
                tool_use_id: "tu_1".to_string(),
                content: "tool output".to_string(),
                is_error: false,
            }],
            &rec,
        );

        // Turn 1: step → Infer
        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Infer(_)), "expected Infer on turn 1");

        // Provide final answer
        sm.provide_inference(end_turn("final answer"), &rec);

        // step → Completed
        let eff = sm.step(&rec);
        let AgentEffect::Completed(answer) = eff else {
            panic!("expected Completed");
        };
        assert_eq!(answer, "final answer");
    }

    #[test]
    fn max_turns_fires_before_infer_request() {
        let (rec, tmp) = recorder();
        let cfg = agent_cfg(1, 1_000_000);

        let mut sm = AgentTask::new("mt-test", "task", &cfg, &model_cfg(), vec![]);

        // Turn 0: step → Infer (turn 0 < max_turns 1 → allowed)
        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Infer(_)));

        // Provide tool-use so step returns CallTools
        sm.provide_inference(
            tool_use_resp("tu_1", "dummy", serde_json::json!({})),
            &rec,
        );
        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::CallTools(_)));

        // provide_tool_results advances turn to 1
        sm.provide_tool_results(vec![], &rec);

        // Now turn == max_turns → MaxTurnsReached fires before InferenceRequest
        let eff = sm.step(&rec);
        let AgentEffect::Failed(msg) = eff else {
            panic!("expected Failed after max_turns exhausted");
        };
        assert!(msg.contains("max turns"), "unexpected message: {msg}");

        // Verify: flight log contains max_turns_reached but no inference_request at turn 1
        let log = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(log.contains("max_turns_reached"), "MaxTurnsReached event missing from log");
        let infer_at_turn_1 = log
            .lines()
            .filter(|l| l.contains("\"inference_request\"") && l.contains("\"turn\":1"))
            .count();
        assert_eq!(
            infer_at_turn_1, 0,
            "InferenceRequest must not be emitted at turn 1 when MaxTurns fires"
        );
    }

    #[test]
    fn provide_inference_on_terminal_task_is_noop() {
        let (rec, tmp) = recorder();
        let cfg = agent_cfg(5, 1_000_000);

        let mut sm = AgentTask::new("term-test", "task", &cfg, &model_cfg(), vec![]);

        // Drive to Completed
        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Infer(_)));
        sm.provide_inference(end_turn("done"), &rec);
        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Completed(_)));

        // Now call provide_inference on a terminal task — must not panic
        let pre_turn = sm.turn();
        sm.provide_inference(end_turn("ignored"), &rec);

        // Turn should not change
        assert_eq!(sm.turn(), pre_turn, "turn must not advance on terminal noop");

        // An error event should have been recorded
        let log = std::fs::read_to_string(tmp.path()).unwrap();
        let has_error = log
            .lines()
            .filter(|l| {
                l.contains("\"error\"") && l.contains("provide_inference")
            })
            .count();
        assert_eq!(has_error, 1, "expected one error event for terminal provide_inference");
    }

    #[test]
    fn step_on_terminal_task_returns_failed() {
        let (rec, tmp) = recorder();
        let cfg = agent_cfg(5, 1_000_000);
        let mut sm = AgentTask::new("term-test-step", "task", &cfg, &model_cfg(), vec![]);

        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Infer(_)));
        sm.provide_inference(end_turn("done"), &rec);
        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Completed(_)));

        // step() on a terminal task must return Failed, not panic
        let eff = sm.step(&rec);
        assert!(
            matches!(&eff, AgentEffect::Failed(msg) if msg.contains("terminal")),
            "expected Failed(terminal)"
        );
        let log = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(log.lines().any(|l| l.contains("\"error\"") && l.contains("\"step\"")));
    }

    #[test]
    fn provide_tool_results_on_terminal_task_is_noop() {
        let (rec, tmp) = recorder();
        let cfg = agent_cfg(5, 1_000_000);
        let mut sm = AgentTask::new("term-test-tools", "task", &cfg, &model_cfg(), vec![]);

        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Infer(_)));
        sm.provide_inference(end_turn("done"), &rec);
        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Completed(_)));

        let pre_turn = sm.turn();
        sm.provide_tool_results(vec![], &rec);
        assert_eq!(sm.turn(), pre_turn, "turn must not advance on terminal noop");

        let log = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(log.lines().any(|l| l.contains("\"error\"") && l.contains("provide_tool_results")));
    }

    // ── p1.5: spawn detection unit tests ──────────────────────────────────────

    fn spawn_use_resp(call_id: &str, task: &str) -> InferenceResponse {
        InferenceResponse {
            blocks: vec![Block::ToolUse {
                id: call_id.to_string(),
                name: "spawn_agent".to_string(),
                input: serde_json::json!({ "task": task }),
            }],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        }
    }

    #[test]
    fn step_detects_spawn_agent_returns_spawn_effect() {
        let (rec, _tmp) = recorder();
        let cfg = agent_cfg(5, 1_000_000);
        let mut sm = AgentTask::new("spawner", "do the thing", &cfg, &model_cfg(), vec![]);

        // Turn 0 → Infer
        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Infer(_)));

        // Provide a spawn_agent tool-use response
        sm.provide_inference(spawn_use_resp("spawn_1", "summarise the repo"), &rec);

        // step → SpawnAgent (not CallTools)
        let eff = sm.step(&rec);
        assert!(
            matches!(&eff, AgentEffect::SpawnAgent { call_id, config }
                if call_id == "spawn_1" && config.task == "summarise the repo"),
            "expected SpawnAgent effect"
        );
    }

    #[test]
    fn step_spawn_mixed_with_other_tools_recovers_via_is_error() {
        // F-16: spawn_agent batched with another tool must NOT terminate the
        // agent. Models routinely batch; the runtime recovers by injecting an
        // is_error ToolResult for every call and re-inferring so the model can
        // retry spawn_agent alone.
        let (rec, _tmp) = recorder();
        let cfg = agent_cfg(5, 1_000_000);
        let mut sm = AgentTask::new("spawner-mix", "task", &cfg, &model_cfg(), vec![]);

        let eff = sm.step(&rec);
        assert!(matches!(eff, AgentEffect::Infer(_)));

        // Response contains spawn_agent mixed with another tool call
        sm.provide_inference(
            InferenceResponse {
                blocks: vec![
                    Block::ToolUse {
                        id: "spawn_1".to_string(),
                        name: "spawn_agent".to_string(),
                        input: serde_json::json!({ "task": "subtask" }),
                    },
                    Block::ToolUse {
                        id: "call_2".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({ "path": "/tmp/x" }),
                    },
                ],
                stop_reason:       StopReason::ToolUse,
                input_tokens:      10,
                output_tokens:     5,
                transport_retries: 0,
            },
            &rec,
        );

        let eff = sm.step(&rec);
        assert!(
            matches!(&eff, AgentEffect::Infer(_)),
            "expected recovery (Infer) when spawn_agent is batched, not termination"
        );
        assert!(!sm.terminal, "batched spawn_agent must not terminate the agent");
        // Every tool_use in the batch must have a matching is_error tool_result.
        let last = sm.messages.last().expect("a user message with tool results");
        assert_eq!(last.role, Role::User);
        let errs: Vec<_> = last
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::ToolResult { is_error: true, .. }))
            .collect();
        assert_eq!(errs.len(), 2, "both batched calls must get an is_error result");
    }

    #[test]
    fn step_spawn_agent_invalid_input_returns_failed() {
        // A malformed spawn_agent call (missing required `task` field) must inject
        // an is_error ToolResult and re-request inference so the model can retry.
        // The agent must NOT be marked terminal.
        let (rec, _tmp) = recorder();
        let cfg = agent_cfg(5, 1_000_000);
        let mut sm = AgentTask::new("spawner-bad", "task", &cfg, &model_cfg(), vec![]);

        let _ = sm.step(&rec); // → Infer

        sm.provide_inference(
            InferenceResponse {
                blocks: vec![Block::ToolUse {
                    id:    "spawn_bad".to_string(),
                    name:  "spawn_agent".to_string(),
                    // Missing required `task` field — deserialization will fail.
                    input: serde_json::json!({ "child_id": "orphan" }),
                }],
                stop_reason:       StopReason::ToolUse,
                input_tokens:      10,
                output_tokens:     5,
                transport_retries: 0,
            },
            &rec,
        );

        // After the parse failure, step() must inject an error ToolResult and
        // return Infer (not Failed) so the model can recover.
        let eff = sm.step(&rec);
        assert!(
            matches!(&eff, AgentEffect::Infer(_)),
            "expected Infer (recoverable) for unparseable spawn input"
        );
        // Agent must NOT be terminal — it should still accept provide_inference.
        assert!(
            !matches!(&eff, AgentEffect::Failed(_)),
            "parse failure must not terminate the agent"
        );
    }

    #[tokio::test]
    async fn driver_rejects_spawn_agent_effect() {
        // The single-agent driver does not support spawn-await; it must return Err.
        use crate::tools::native::register_native;

        struct SpawnGateway;
        #[async_trait::async_trait]
        impl InferenceGateway for SpawnGateway {
            async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
                Ok(InferenceResponse {
                    blocks: vec![Block::ToolUse {
                        id:    "spawn_1".to_string(),
                        name:  "spawn_agent".to_string(),
                        input: serde_json::json!({ "task": "sub-task" }),
                    }],
                    stop_reason:       StopReason::ToolUse,
                    input_tokens:      5,
                    output_tokens:     3,
                    transport_retries: 0,
                })
            }
            fn model_id(&self) -> &str { "spawn-gw" }
        }

        let mut reg = crate::tools::ToolRegistry::new();
        register_native(&mut reg, &["spawn_agent".to_string()], None, None, None, None).unwrap();
        let (rec, _tmp) = recorder();

        let err = run(
            "driver-spawn",
            "spawn something",
            &agent_cfg(5, 1_000_000),
            &model_cfg(),
            &SpawnGateway,
            &reg,
            &rec,
        )
        .await
        .unwrap_err();

        assert!(
            err.to_string().contains("single-agent driver"),
            "driver must reject spawn_agent, got: {err}"
        );
    }

    #[tokio::test]
    async fn run_tools_sequential_skips_non_tool_use_blocks() {
        let tmp = NamedTempFile::new().unwrap();
        let rec = crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap();
        let registry = crate::tools::ToolRegistry::new();

        let blocks = vec![
            Block::Text { text: "some text".to_string() },
            Block::ToolUse {
                id:    "call_1".to_string(),
                name:  "unknown_tool".to_string(),
                input: serde_json::json!({}),
            },
        ];

        let results = run_tools_sequential("agent", 0, "", &blocks, &registry, None, &rec).await;

        // Text block skipped; unknown tool returns an error result (not a panic)
        assert_eq!(results.len(), 1, "only the ToolUse block should produce a result");
        let Block::ToolResult { tool_use_id, is_error, .. } = &results[0] else {
            panic!("expected ToolResult");
        };
        assert_eq!(tool_use_id, "call_1");
        assert!(is_error, "unknown tool should produce is_error=true result");
    }

    // ── p1.6: inject_messages / send_message tests ───────────────────────────

    #[test]
    fn inject_messages_appends_to_last_user_message() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "initial task", &agent_cfg(5, 100_000), &model_cfg(), vec![]);

        assert_eq!(task.messages.len(), 1);

        task.inject_messages(
            vec![crate::bus::MailMessage { from: "alice".to_string(), content: "hello".to_string() }],
            &rec,
        );

        // Must still be 1 message (appended, not pushed)
        assert_eq!(task.messages.len(), 1, "inject_messages must NOT push a new Msg");
        // The single User message should now have 2 blocks
        assert_eq!(task.messages[0].blocks.len(), 2, "Text block should have been appended");
        if let Block::Text { text } = &task.messages[0].blocks[1] {
            assert!(text.contains("alice"), "injected block must reference sender");
            assert!(text.contains("hello"), "injected block must contain message content");
        } else {
            panic!("expected Block::Text");
        }
    }

    #[test]
    fn inject_messages_empty_list_is_noop() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(5, 100_000), &model_cfg(), vec![]);
        let original_len = task.messages[0].blocks.len();

        task.inject_messages(vec![], &rec);

        assert_eq!(task.messages[0].blocks.len(), original_len, "empty inject must not modify messages");
    }

    #[test]
    fn step_send_message_sole_call_guard() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "send messages", &agent_cfg(5, 100_000), &model_cfg(), vec![]);

        let response = InferenceResponse {
            blocks: vec![
                Block::ToolUse {
                    id:    "msg_1".to_string(),
                    name:  "send_message".to_string(),
                    input: serde_json::json!({"to": "b", "content": "hello"}),
                },
                Block::ToolUse {
                    id:    "extra_1".to_string(),
                    name:  "read_file".to_string(),
                    input: serde_json::json!({"path": "/tmp/x"}),
                },
            ],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        };
        task.provide_inference(response, &rec);
        let effect = task.step(&rec);
        // F-16: batched send_message recovers via is_error ToolResults, not termination.
        assert!(
            matches!(effect, AgentEffect::Infer(_)),
            "batched send_message must recover (Infer), not fail"
        );
        assert!(!task.terminal, "batched send_message must not terminate the agent");
    }

    #[test]
    fn step_send_message_missing_to_is_error_not_panic() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "send messages", &agent_cfg(5, 100_000), &model_cfg(), vec![]);

        let response = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    "msg_bad".to_string(),
                name:  "send_message".to_string(),
                input: serde_json::json!({"content": "no recipient"}),
            }],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        };
        task.provide_inference(response, &rec);
        let effect = task.step(&rec);
        // Missing `to` → synthesizes error ToolResult and returns Infer (step_need_infer).
        assert!(matches!(effect, AgentEffect::Infer(_)), "missing `to` must produce Infer (via error ToolResult), not panic");
    }

    #[test]
    fn max_tokens_with_no_text_returns_failed() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(5, 100_000), &model_cfg(), vec![]);

        // MaxTokens with no Text block — the truncated-generation bug case.
        let response = InferenceResponse {
            blocks:            vec![],
            stop_reason:       StopReason::MaxTokens,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        };
        task.provide_inference(response, &rec);
        let effect = task.step(&rec);
        assert!(
            matches!(effect, AgentEffect::Failed(_)),
            "MaxTokens with no text must be Failed, not Completed"
        );
        if let AgentEffect::Failed(msg) = effect {
            assert!(
                msg.contains("max_tokens"),
                "failure message must mention max_tokens, got: {msg}"
            );
        }
    }

    #[test]
    fn max_tokens_with_partial_text_returns_failed() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(5, 100_000), &model_cfg(), vec![]);

        // MaxTokens WITH a partial Text block — partial text must be discarded (D1).
        let response = InferenceResponse {
            blocks:            vec![Block::Text { text: "partial answer cut".to_string() }],
            stop_reason:       StopReason::MaxTokens,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        };
        task.provide_inference(response, &rec);
        let effect = task.step(&rec);
        assert!(
            matches!(effect, AgentEffect::Failed(_)),
            "MaxTokens with partial text must be Failed, not Completed (partial text discarded)"
        );
    }

    #[test]
    fn max_tokens_resettable_agent_parks_not_bricked() {
        // budget.1 / AUDIT-v0.97 P3 + budget.1-ar-01: a resident agent under a reset window
        // (`budget_resettable`) must NOT be hard-Failed on a single truncated response — that was
        // the self-brick class. It returns the DISTINCT `CompletedTruncated` effect (not plain
        // `Completed`), carrying the partial text, so the scheduler can role-gate: a resident/
        // orchestrated agent parks + resumes on this, while a one-shot/child fails. This test pins
        // the "must not Fail/brick" half (the P0-2 guarantee) at the AgentTask layer.
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(5, 100_000), &model_cfg(), vec![]);
        task.set_budget_resettable(true);

        let response = InferenceResponse {
            blocks:            vec![Block::Text { text: "partial but usable".to_string() }],
            stop_reason:       StopReason::MaxTokens,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        };
        task.provide_inference(response, &rec);
        let effect = task.step(&rec);
        match effect {
            AgentEffect::CompletedTruncated(answer) => {
                assert_eq!(answer, "partial but usable", "partial text is carried for the resident park path");
            }
            AgentEffect::Failed(msg) => panic!("resettable MaxTokens must NOT brick (Failed): {msg}"),
            _ => panic!("resettable MaxTokens must be CompletedTruncated"),
        }
    }

    #[test]
    fn context_tokens_accumulates_input_and_output() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(5, 100_000), &model_cfg(), vec![]);
        assert_eq!(task.context_tokens(), 0, "starts at zero before any inference");
        let response = InferenceResponse {
            blocks:            vec![],
            stop_reason:       StopReason::EndTurn,
            input_tokens:      100,
            output_tokens:     50,
            transport_retries: 0,
        };
        task.provide_inference(response, &rec);
        assert_eq!(task.context_tokens(), 150);
    }

    // ── p3.2: checkpoint tests ────────────────────────────────────────────────

    #[test]
    fn is_terminal_false_for_new_task() {
        let task = AgentTask::new("t", "task", &agent_cfg(5, 100_000), &model_cfg(), vec![]);
        assert!(!task.is_terminal());
    }

    #[test]
    fn is_terminal_true_after_completion() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(5, 100_000), &model_cfg(), vec![]);
        let _ = task.step(&rec);
        task.provide_inference(end_turn("done"), &rec);
        let _ = task.step(&rec);
        assert!(task.is_terminal());
    }

    #[test]
    fn to_checkpoint_captures_all_fields() {
        use crate::inference::ToolSpec;
        let spec = ToolSpec {
            name: "read_file".to_string(),
            description: "reads a file".to_string(),
            input_schema: serde_json::json!({}),
        };
        let cfg = agent_cfg(7, 50_000);
        let task = AgentTask::new("agent-42", "do the thing", &cfg, &model_cfg(), vec![spec.clone()]);
        let cp = task.to_checkpoint();
        assert_eq!(cp.agent_id, "agent-42");
        assert_eq!(cp.turn, 0);
        assert_eq!(cp.total_input, 0);
        assert_eq!(cp.total_output, 0);
        assert!(!cp.terminal, "terminal must be false in checkpoint");
        assert!(cp.stored_response.is_none());
        assert_eq!(cp.messages.len(), 1);
        assert_eq!(cp.specs[0].name, "read_file");
    }

    #[test]
    fn from_checkpoint_uses_fresh_specs() {
        use crate::checkpoint::AgentCheckpoint;
        use crate::inference::ToolSpec;

        let stale_spec = ToolSpec {
            name: "old_tool".to_string(),
            description: "stale".to_string(),
            input_schema: serde_json::json!({}),
        };
        let fresh_spec = ToolSpec {
            name: "new_tool".to_string(),
            description: "fresh".to_string(),
            input_schema: serde_json::json!({}),
        };

        let cp = AgentCheckpoint {
            agent_id:        "agent-42".to_string(),
            cfg:             agent_cfg(7, 50_000),
            model_cfg:       model_cfg(),
            messages:        vec![],
            specs:           vec![stale_spec],
            total_input:     100,
            total_output:    50,
            turn:            3,
            stored_response: None,
            terminal:        false,
            short_term:      vec![],
            window_anchor:   0,
        };
        let task = AgentTask::from_checkpoint(cp, vec![fresh_spec]);
        assert_eq!(task.agent_id, "agent-42");
        assert_eq!(task.turn, 3);
        assert_eq!(task.total_input, 100);
        assert_eq!(task.total_output, 50);
        assert!(!task.terminal, "non-terminal checkpoint restores as non-terminal");
        assert_eq!(task.specs[0].name, "new_tool", "specs must come from fresh registry");
    }

    #[test]
    fn from_checkpoint_restores_terminal_true() {
        use crate::checkpoint::AgentCheckpoint;
        // Orchestrated agents are checkpointed with terminal=true so the seed loop
        // does not step them on restore. from_checkpoint must preserve this state;
        // resume_for_orchestration() clears it when the next inject arrives.
        let cp = AgentCheckpoint {
            agent_id:        "orch-01".to_string(),
            cfg:             agent_cfg(200, 200_000),
            model_cfg:       model_cfg(),
            messages:        vec![],
            specs:           vec![],
            total_input:     42,
            total_output:    10,
            turn:            1,
            stored_response: None,
            terminal:        true,
            short_term:      vec![],
            window_anchor:   0,
        };
        let task = AgentTask::from_checkpoint(cp, vec![]);
        assert!(task.terminal, "orchestrated waiting agent must restore as terminal=true");
        assert_eq!(task.agent_id, "orch-01");
    }

    // ── attn.2 R2 / attn.1a-05: dangling tool_use repair on restore ──────────────
    // These pin the pairing invariant the Messages API enforces. Note the /qa harness
    // uses a FAKE provider that does NOT enforce tool_use/tool_result pairing, so an
    // integration test there would pass against a still-broken history. The invariant
    // has to be asserted directly, here.

    fn live(ids: &[&str]) -> std::collections::HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    /// Every tool_use in the history is answered in the immediately-following user turn.
    /// This is exactly what the provider checks.
    fn assert_well_paired(messages: &[Msg]) {
        for (i, m) in messages.iter().enumerate() {
            if m.role != Role::Assistant {
                continue;
            }
            for b in &m.blocks {
                if let Block::ToolUse { id, .. } = b {
                    let answered = messages.get(i + 1).is_some_and(|n| {
                        n.role == Role::User
                            && n.blocks.iter().any(|nb| {
                                matches!(nb, Block::ToolResult { tool_use_id, .. }
                                         if tool_use_id == id)
                            })
                    });
                    assert!(answered, "tool_use {id} has no tool_result in the next user turn");
                }
            }
            assert!(!m.blocks.is_empty(), "assistant turn {i} has empty blocks — the API rejects this");
        }
    }

    fn tool_only_assistant(id: &str) -> Msg {
        Msg {
            role: Role::Assistant,
            blocks: vec![Block::ToolUse {
                id: id.to_string(),
                name: "run_job".to_string(),
                input: json!({}),
            }],
        }
    }

    #[test]
    fn repair_answers_an_orphaned_tool_use() {
        let mut msgs = vec![
            Msg { role: Role::User, blocks: vec![Block::Text { text: "go".into() }] },
            tool_only_assistant("toolu_orphan"),
        ];
        let repaired = AgentTask::repair_dangling_tool_uses(&mut msgs, &live(&[]));
        assert_eq!(repaired, vec!["toolu_orphan".to_string()]);
        assert_well_paired(&msgs);
        let last = msgs.last().unwrap();
        assert_eq!(last.role, Role::User);
        assert!(matches!(&last.blocks[0],
                Block::ToolResult { tool_use_id, is_error, .. }
                if tool_use_id == "toolu_orphan" && *is_error));
    }

    /// THE negative control. A dangling tool_use that the scheduler has promised to answer
    /// (run_job/spawn_agent await, or a parked approval) must be left untouched. Answering it
    /// here would make the real result arrive later as a tool_result with no matching
    /// tool_use — the same 400, from the other side.
    #[test]
    fn repair_leaves_live_awaited_calls_alone() {
        let mut msgs = vec![
            Msg { role: Role::User, blocks: vec![Block::Text { text: "go".into() }] },
            tool_only_assistant("toolu_awaited"),
        ];
        let before = msgs.len();
        let repaired =
            AgentTask::repair_dangling_tool_uses(&mut msgs, &live(&["toolu_awaited"]));
        assert!(repaired.is_empty(), "a promised call must not be repaired: {repaired:?}");
        assert_eq!(msgs.len(), before, "no turn may be appended for a live call");
    }

    #[test]
    fn repair_is_a_noop_when_every_call_is_already_answered() {
        let mut msgs = vec![
            tool_only_assistant("toolu_done"),
            Msg {
                role: Role::User,
                blocks: vec![Block::ToolResult {
                    tool_use_id: "toolu_done".into(),
                    content: "ok".into(),
                    is_error: false,
                }],
            },
        ];
        let snapshot = format!("{:?}", msgs);
        let repaired = AgentTask::repair_dangling_tool_uses(&mut msgs, &live(&[]));
        assert!(repaired.is_empty());
        assert_eq!(format!("{:?}", msgs), snapshot, "answered history must not be rewritten");
    }

    /// A partially-answered batch: the missing result must join the EXISTING user turn.
    /// Inserting a second user turn after it would leave the first tool_use answered in a
    /// turn that is no longer adjacent to its assistant message.
    #[test]
    fn repair_merges_into_a_partial_result_turn() {
        let mut msgs = vec![
            Msg {
                role: Role::Assistant,
                blocks: vec![
                    Block::ToolUse { id: "a".into(), name: "t".into(), input: json!({}) },
                    Block::ToolUse { id: "b".into(), name: "t".into(), input: json!({}) },
                ],
            },
            Msg {
                role: Role::User,
                blocks: vec![Block::ToolResult {
                    tool_use_id: "a".into(),
                    content: "ok".into(),
                    is_error: false,
                }],
            },
        ];
        let repaired = AgentTask::repair_dangling_tool_uses(&mut msgs, &live(&[]));
        assert_eq!(repaired, vec!["b".to_string()]);
        assert_eq!(msgs.len(), 2, "must merge into the existing user turn, not insert a new one");
        assert_well_paired(&msgs);
    }

    /// Mixed text + tool_use: repairing must not strip the assistant's prose, and must not
    /// leave the turn with empty blocks (which the API rejects for a different reason).
    #[test]
    fn repair_preserves_assistant_text_and_never_empties_a_turn() {
        let mut msgs = vec![Msg {
            role: Role::Assistant,
            blocks: vec![
                Block::Text { text: "calling the tool now".into() },
                Block::ToolUse { id: "x".into(), name: "t".into(), input: json!({}) },
            ],
        }];
        AgentTask::repair_dangling_tool_uses(&mut msgs, &live(&[]));
        assert!(
            matches!(&msgs[0].blocks[0], Block::Text { text } if text == "calling the tool now"),
            "assistant prose must survive the repair"
        );
        assert_well_paired(&msgs);
    }

    /// A turn mixing a promised and an orphaned call is left ENTIRELY alone.
    ///
    /// Answering only the orphan would be worse than doing nothing: the live call's real
    /// result arrives later via `provide_tool_results`, which pushes its own user turn, so the
    /// history becomes assistant[live, orphan] / user[orphan] / user[live] and the live
    /// tool_use is no longer answered in the turn immediately after it — the same 400 this
    /// function removes. Leaving the turn intact keeps the real delivery adjacent.
    #[test]
    fn repair_leaves_a_mixed_live_and_orphaned_turn_entirely_alone() {
        let mut msgs = vec![Msg {
            role: Role::Assistant,
            blocks: vec![
                Block::ToolUse { id: "promised".into(), name: "run_job".into(), input: json!({}) },
                Block::ToolUse { id: "orphan".into(), name: "read_file".into(), input: json!({}) },
            ],
        }];
        let before = msgs.len();
        let repaired = AgentTask::repair_dangling_tool_uses(&mut msgs, &live(&["promised"]));
        assert!(repaired.is_empty(),
            "a turn containing a live call must not be rewritten at all, got {repaired:?}");
        assert_eq!(msgs.len(), before,
            "no user turn may be inserted after a turn holding a live call — the real result \
             must stay adjacent to its tool_use");
    }

    /// Two interrupted assistant turns in one history. The plan is applied `.rev()` precisely
    /// so that inserting after the later turn cannot shift the earlier turn's index; applying
    /// it front-to-back would land the second repair after the WRONG assistant message,
    /// leaving a still-unpaired history on the fail-closed restore path. Every other repair
    /// test has exactly one orphaned turn, so none of them exercise the ordering — deleting
    /// `.rev()` left the whole suite green until /review's testing specialist proved it.
    #[test]
    fn repair_handles_multiple_orphaned_turns_without_shifting_indices() {
        let mut msgs = vec![
            tool_only_assistant("toolu_first"),
            Msg { role: Role::User, blocks: vec![Block::Text { text: "inject".into() }] },
            tool_only_assistant("toolu_second"),
        ];
        let repaired = AgentTask::repair_dangling_tool_uses(&mut msgs, &live(&[]));
        assert_eq!(
            repaired,
            vec!["toolu_first".to_string(), "toolu_second".to_string()],
            "both interrupted turns must be repaired"
        );
        assert_well_paired(&msgs);
        // Each synthetic result must sit immediately after ITS OWN assistant turn.
        for id in ["toolu_first", "toolu_second"] {
            let ai = msgs
                .iter()
                .position(|m| {
                    m.blocks.iter().any(
                        |b| matches!(b, Block::ToolUse { id: bid, .. } if bid == id),
                    )
                })
                .unwrap_or_else(|| panic!("{id} vanished from the history"));
            assert!(
                matches!(&msgs[ai + 1].blocks[0],
                         Block::ToolResult { tool_use_id, .. } if tool_use_id == id),
                "{id}'s result is not in the turn immediately after its own tool_use — the \
                 plan was applied front-to-back and the indices shifted"
            );
        }
    }

    /// A result that exists but landed a turn TOO LATE must still be repaired.
    /// Scanning the whole history for "is this id answered anywhere" would call this healthy
    /// and ship a 400, because the provider only accepts results in the turn immediately
    /// after the tool_use. Reachable via `push_user_turn` or an orchestration inject.
    #[test]
    fn repair_treats_a_misplaced_tool_result_as_unanswered() {
        let mut msgs = vec![
            tool_only_assistant("toolu_late"),
            Msg { role: Role::User, blocks: vec![Block::Text { text: "an inject".into() }] },
            Msg {
                role: Role::User,
                blocks: vec![Block::ToolResult {
                    tool_use_id: "toolu_late".into(),
                    content: "arrived too late".into(),
                    is_error: false,
                }],
            },
        ];
        let repaired = AgentTask::repair_dangling_tool_uses(&mut msgs, &live(&[]));
        assert_eq!(repaired, vec!["toolu_late".to_string()],
            "a result one turn too late does not satisfy the provider's adjacency rule");
        assert_well_paired(&msgs);
    }

    /// The merged turn must BEGIN with tool_results, in the assistant turn's tool_use order,
    /// even when it already carries a trailing text block from an inject.
    #[test]
    fn repair_rebuilds_the_leading_result_run_in_tool_use_order() {
        let mut msgs = vec![
            Msg {
                role: Role::Assistant,
                blocks: vec![
                    Block::ToolUse { id: "a".into(), name: "t".into(), input: json!({}) },
                    Block::ToolUse { id: "b".into(), name: "t".into(), input: json!({}) },
                    Block::ToolUse { id: "c".into(), name: "t".into(), input: json!({}) },
                ],
            },
            Msg {
                role: Role::User,
                blocks: vec![
                    Block::ToolResult { tool_use_id: "c".into(), content: "ok".into(), is_error: false },
                    Block::Text { text: "an inject landed here".into() },
                ],
            },
        ];
        AgentTask::repair_dangling_tool_uses(&mut msgs, &live(&[]));
        let ids: Vec<&str> = msgs[1].blocks.iter().filter_map(|b| match b {
            Block::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        }).collect();
        assert_eq!(ids, vec!["a", "b", "c"],
            "results must be ordered to match the assistant turn's tool_use order");
        // The leading run must be unbroken: every block before the first non-result is a result.
        let first_non_result = msgs[1].blocks.iter()
            .position(|b| !matches!(b, Block::ToolResult { .. }))
            .unwrap_or(msgs[1].blocks.len());
        assert_eq!(first_non_result, 3,
            "all three tool_results must form the LEADING run; a text block must not split it");
        assert_well_paired(&msgs);
    }

    #[test]
    fn checkpoint_roundtrip_preserves_messages_and_turn() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("rt", "roundtrip task", &agent_cfg(10, 200_000), &model_cfg(), vec![]);

        // Advance one tool cycle so turn = 1 and messages > 1
        let _ = task.step(&rec);
        task.provide_inference(tool_use_resp("c1", "dummy", serde_json::json!({})), &rec);
        let _ = task.step(&rec);
        task.provide_tool_results(
            vec![Block::ToolResult { tool_use_id: "c1".to_string(), content: "out".to_string(), is_error: false }],
            &rec,
        );
        assert_eq!(task.turn, 1);

        let cp = task.to_checkpoint();
        let restored = AgentTask::from_checkpoint(cp, vec![]);
        assert_eq!(restored.turn, 1);
        assert_eq!(restored.messages.len(), task.messages.len());
        assert!(!restored.is_terminal());
    }

    // ── p5.2: memory paging tests ─────────────────────────────────────────────

    // AC10: AgentTask with hard pressure calls page_turns, emits MemoryPaged
    #[test]
    fn step_hard_pressure_emits_memory_paged() {
        let (rec, tmp) = recorder();
        // F-01: paging is driven by RETAINED CONTEXT size, not lifetime spend.
        // Build a large working set (big tool results) until it crosses the Hard
        // threshold (90% of budget). Inference token counts stay tiny so the
        // budget guard never trips first.
        let budget = 1_000u64;
        let cfg = agent_cfg(20, budget);
        let mut sm = AgentTask::new("pg", "task", &cfg, &model_cfg(), vec![]);

        let big = "x".repeat(1_300); // ~325 tokens at 4 chars/token
        for i in 0..3usize {
            let _ = sm.step(&rec); // → Infer
            sm.provide_inference(
                InferenceResponse {
                    blocks:            vec![Block::ToolUse {
                        id:    format!("c{i}"),
                        name:  "no_tool".to_string(),
                        input: serde_json::json!({}),
                    }],
                    stop_reason:       StopReason::ToolUse,
                    input_tokens:      5,
                    output_tokens:     5,
                    transport_retries: 0,
                },
                &rec,
            );
            let _ = sm.step(&rec); // → CallTools (pushes Assistant tool_use)
            sm.provide_tool_results(
                vec![Block::ToolResult {
                    tool_use_id: format!("c{i}"),
                    content:     big.clone(),
                    is_error:    false,
                }],
                &rec,
            ); // pushes User(big tool result)
        }
        // messages.len()==7; retained ≈ 3*325 ≈ 975 tokens ≈ 97.5% → Hard.

        // Next step_need_infer: Hard retained pressure, page_count(7)=1 pair → page!
        let _ = sm.step(&rec);

        let log = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(log.contains("memory_paged"), "MemoryPaged event must be emitted under hard pressure");

        let paged: Vec<serde_json::Value> = log
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .filter(|e: &serde_json::Value| e["kind"] == "memory_paged")
            .collect();
        assert_eq!(paged.len(), 1, "exactly one memory_paged event");
        assert!(
            paged[0]["data"]["pages_moved"].as_u64().unwrap_or(0) > 0,
            "pages_moved must be > 0"
        );
        assert!(
            sm.short_term_depth() > 0,
            "short_term must be non-empty after hard paging"
        );
    }

    // run.1-ar-01: the cap_short_term() call at the MemoryPaged site (agent/mod.rs:611) is only
    // regression-covered if short_term is PRE-SEEDED to the cap before paging — otherwise
    // `short_term_evicted` is 0 and a no-op'd drain still passes. This drives the real call site.
    #[test]
    fn step_hard_pressure_caps_short_term_when_preseeded_full() {
        use crate::inference::Role;
        use crate::memory::MemItem;
        let (rec, tmp) = recorder();
        let budget = 1_000u64;
        let cfg = agent_cfg(20, budget);
        let mut sm = AgentTask::new("pg-cap", "task", &cfg, &model_cfg(), vec![]);

        // Build hard retained pressure exactly like step_hard_pressure_emits_memory_paged.
        let big = "x".repeat(1_300);
        for i in 0..3usize {
            let _ = sm.step(&rec);
            sm.provide_inference(
                InferenceResponse {
                    blocks:            vec![Block::ToolUse {
                        id:    format!("c{i}"),
                        name:  "no_tool".to_string(),
                        input: serde_json::json!({}),
                    }],
                    stop_reason:       StopReason::ToolUse,
                    input_tokens:      5,
                    output_tokens:     5,
                    transport_retries: 0,
                },
                &rec,
            );
            let _ = sm.step(&rec);
            sm.provide_tool_results(
                vec![Block::ToolResult {
                    tool_use_id: format!("c{i}"),
                    content:     big.clone(),
                    is_error:    false,
                }],
                &rec,
            );
        }

        // PRE-SEED short_term to exactly the cap. Now the paging step's `short_term.extend(items)`
        // pushes it over MAX_SHORT_TERM, so cap_short_term() at :611 MUST evict `pages_moved`.
        for t in 0..MAX_SHORT_TERM {
            sm.short_term.push(MemItem {
                turn:            t as u32,
                role:            Role::Assistant,
                content_preview: format!("seed {t}"),
                blocks_json:     "[]".to_string(),
            });
        }
        assert_eq!(sm.short_term_depth(), MAX_SHORT_TERM, "pre-seeded full");

        let _ = sm.step(&rec); // Hard pressure → page_turns → extend → cap_short_term() at :611

        let paged: Vec<serde_json::Value> = std::fs::read_to_string(tmp.path())
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .filter(|e: &serde_json::Value| e["kind"] == "memory_paged")
            .collect();
        assert_eq!(paged.len(), 1, "exactly one memory_paged event");
        let evicted = paged[0]["data"]["short_term_evicted"].as_u64().unwrap_or(0);
        // BOTH must hold — without the cap call, evicted would be 0 AND depth would exceed the cap.
        assert!(evicted > 0, "cap_short_term must evict the overflow at the paging site, got {evicted}");
        assert_eq!(
            sm.short_term_depth(),
            MAX_SHORT_TERM,
            "short_term stays bounded to the cap after paging over a full buffer"
        );
    }

    // AC11: Soft pressure emits MemoryPressureAdvisory event; no text injected into messages
    #[test]
    fn step_soft_pressure_emits_advisory_no_injection() {
        let (rec, tmp) = recorder();
        let budget = 100u64;
        let cfg = agent_cfg(20, budget);
        let mut sm = AgentTask::new("soft", "task", &cfg, &model_cfg(), vec![]);

        // 1 tool cycle at 37+38=75 tokens → 75% = exactly SOFT_THRESHOLD
        let _ = sm.step(&rec);
        sm.provide_inference(
            InferenceResponse {
                blocks:            vec![Block::ToolUse {
                    id:    "c0".to_string(),
                    name:  "no_tool".to_string(),
                    input: serde_json::json!({}),
                }],
                stop_reason:       StopReason::ToolUse,
                input_tokens:      37,
                output_tokens:     38,
                transport_retries: 0,
            },
            &rec,
        );
        let _ = sm.step(&rec); // → CallTools
        sm.provide_tool_results(
            vec![Block::ToolResult {
                tool_use_id: "c0".to_string(),
                content:     "r".to_string(),
                is_error:    false,
            }],
            &rec,
        );
        // total=75, messages.len()=3

        let msg_count_before = sm.message_count();
        let _ = sm.step(&rec); // step_need_infer: Soft pressure

        let log = std::fs::read_to_string(tmp.path()).unwrap();
        assert!(
            log.contains("memory_pressure_advisory"),
            "MemoryPressureAdvisory event must be emitted at soft threshold"
        );
        assert!(
            !log.contains("memory_paged"),
            "MemoryPaged must NOT be emitted at soft threshold"
        );
        assert_eq!(
            sm.message_count(),
            msg_count_before,
            "messages must not change under soft pressure"
        );
    }

    #[test]
    fn cap_short_term_bounds_and_drops_oldest() {
        // AUDIT-v0.97 P1-3: short_term is bounded (ring-buffer) so a never-terminating agent's
        // RAM + per-turn checkpoint clone stay bounded. Oldest paged summaries drop first.
        use crate::memory::MemItem;
        use crate::inference::Role;
        let cfg = agent_cfg(20, 1_000_000);
        let mut sm = AgentTask::new("cap-st", "task", &cfg, &model_cfg(), vec![]);
        let total = MAX_SHORT_TERM + 500;
        for t in 0..total {
            sm.short_term.push(MemItem {
                turn:            t as u32,
                role:            Role::Assistant,
                content_preview: format!("item {t}"),
                blocks_json:     "[]".to_string(),
            });
        }
        let evicted = sm.cap_short_term();
        assert_eq!(evicted, 500, "overflow beyond the cap is evicted");
        assert_eq!(sm.short_term_depth(), MAX_SHORT_TERM, "bounded to the cap");
        assert_eq!(sm.short_term.first().unwrap().turn, 500, "oldest 500 dropped (ring-buffer)");
        assert_eq!(sm.short_term.last().unwrap().turn, (total - 1) as u32, "newest retained");
        assert_eq!(sm.to_checkpoint().short_term.len(), MAX_SHORT_TERM, "checkpoint clone is now bounded");
    }

    // AC14: to_checkpoint/from_checkpoint preserve short_term (items not zeroed on restore)
    #[test]
    fn checkpoint_roundtrip_preserves_short_term() {
        use crate::memory::MemItem;
        use crate::inference::Role;

        let (rec, _tmp) = recorder();
        let cfg = agent_cfg(20, 1_000_000);
        let mut sm = AgentTask::new("cp-st", "task", &cfg, &model_cfg(), vec![]);

        // Manually push a MemItem into short_term to simulate prior paging
        sm.short_term.push(MemItem {
            turn:            1,
            role:            Role::Assistant,
            content_preview: "evicted content".to_string(),
            blocks_json:     r#"[{"type":"text","text":"evicted"}]"#.to_string(),
        });

        let cp = sm.to_checkpoint();
        assert_eq!(cp.short_term.len(), 1, "to_checkpoint must include short_term");
        assert_eq!(cp.short_term[0].turn, 1);

        let restored = AgentTask::from_checkpoint(cp, vec![]);
        assert_eq!(
            restored.short_term.len(),
            1,
            "from_checkpoint must restore short_term (not zero it)"
        );
        assert_eq!(restored.short_term[0].turn, 1);
        assert_eq!(restored.short_term[0].content_preview, "evicted content");

        let _ = rec; // suppress unused warning
    }

    #[test]
    fn task_preview_handles_multibyte_unicode() {
        let cfg = AgentConfig {
            task: "こんにちは世界".to_string(),
            ..agent_cfg(5, 100_000)
        };
        let task = AgentTask::new("t", &cfg.task, &cfg, &model_cfg(), vec![]);
        let preview = task.task_preview(5);
        assert_eq!(preview.chars().count(), 5);
        assert_eq!(preview, "こんにちは");
        assert_eq!(task.task_preview(100), "こんにちは世界");
    }

    // ── ux.8′ budget-window tests ─────────────────────────────────────────────

    /// EndTurn response with the given input/output token counts.
    fn end_turn_tokens(input: u32, output: u32) -> InferenceResponse {
        InferenceResponse {
            blocks:            vec![Block::Text { text: "answer".to_string() }],
            stop_reason:       StopReason::EndTurn,
            input_tokens:      input,
            output_tokens:     output,
            transport_retries: 0,
        }
    }

    #[test]
    fn windowed_spent_and_reset_keeps_lifetime_monotonic() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(50, 100), &model_cfg(), vec![]);
        let _ = task.step(&rec); // Infer (turn 0)
        task.provide_inference(end_turn_tokens(55, 55), &rec);
        let _ = task.step(&rec); // Completed
        assert_eq!(task.context_tokens(), 110);
        assert_eq!(task.windowed_spent(), 110);

        let old = task.reset_budget_window();
        assert_eq!(old, 110, "reset returns pre-reset windowed spend");
        assert_eq!(task.windowed_spent(), 0, "windowed spend rebases to 0");
        assert_eq!(task.context_tokens(), 110, "reset must NOT touch the monotonic lifetime counter");
    }

    #[test]
    fn pre_inference_gate_catches_text_only_overspend() {
        // P1-1: a text-only orchestrated agent (EndTurn→park→inject) never hits
        // the ToolUse backstop, so the pre-inference gate in step_need_infer must
        // catch the overspend before the next inference.
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(50, 100), &model_cfg(), vec![]);
        let _ = task.step(&rec); // Infer (turn 0)
        task.provide_inference(end_turn_tokens(55, 55), &rec); // 110 > budget 100
        let eff = task.step(&rec);
        assert!(
            matches!(eff, AgentEffect::Completed(_)),
            "EndTurn completes even over budget — the P1-1 hole the gate closes"
        );
        assert_eq!(task.windowed_spent(), 110);

        // Orchestration resume + inject → the NEXT step_need_infer must fail-fast.
        task.resume_for_orchestration();
        task.push_user_turn("again".to_string(), &rec);
        let eff = task.step(&rec);
        match eff {
            AgentEffect::Failed(m) => assert!(
                m.contains("budget exceeded"),
                "expected pre-inference budget failure, got: {m}"
            ),
            _ => panic!("expected AgentEffect::Failed from the pre-inference gate"),
        }
    }

    #[test]
    fn zero_token_budget_is_unlimited() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(50, 0), &model_cfg(), vec![]);
        let _ = task.step(&rec);
        task.provide_inference(end_turn_tokens(10_000, 10_000), &rec);
        let _ = task.step(&rec); // Completed
        task.resume_for_orchestration();
        task.push_user_turn("again".to_string(), &rec);
        let eff = task.step(&rec);
        assert!(
            matches!(eff, AgentEffect::Infer(_)),
            "token_budget = 0 means unlimited — must never brick on budget"
        );
    }

    #[test]
    fn reset_rearms_the_pre_inference_gate() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(50, 100), &model_cfg(), vec![]);
        let _ = task.step(&rec);
        task.provide_inference(end_turn_tokens(55, 55), &rec); // 110 > 100
        let _ = task.step(&rec);
        // Would fail the gate now; reset the window and it must proceed.
        let _ = task.reset_budget_window();
        task.resume_for_orchestration();
        task.push_user_turn("again".to_string(), &rec);
        let eff = task.step(&rec);
        assert!(
            matches!(eff, AgentEffect::Infer(_)),
            "windowed budget must re-arm after a reset"
        );
    }
}
