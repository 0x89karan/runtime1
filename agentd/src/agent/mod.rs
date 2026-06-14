#[cfg(test)]
pub mod driver;

use serde_json::json;

pub const PREVIEW_CHARS: usize = 200;

use crate::{
    config::{AgentConfig, ModelConfig, SpawnConfig},
    flight_recorder::{EventKind, FlightRecorder},
    inference::{Block, InferenceRequest, InferenceResponse, Msg, Role, StopReason, ToolSpec},
    memory::{
        context::{assess, page_count, page_turns, MemoryPressure, SOFT_THRESHOLD},
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
    /// Emitted when the model calls `send_message` as its sole tool in a turn.
    /// The scheduler delivers the message and synthesizes a ToolResult.
    SendMessage { call_id: String, to: String, content: String },
    Completed(String),
    Failed(String),
}

pub struct AgentTask {
    agent_id:        String,
    cfg:             AgentConfig,
    model_cfg:       ModelConfig,
    messages:        Vec<Msg>,
    specs:           Vec<ToolSpec>,
    total_input:     u64,
    total_output:    u64,
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
}

impl AgentTask {
    pub fn new(
        agent_id: &str,
        task: &str,
        cfg: &AgentConfig,
        model_cfg: &ModelConfig,
        specs: Vec<ToolSpec>,
    ) -> Self {
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
            total_input: 0,
            total_output: 0,
            turn: 0,
            stored_response: None,
            terminal: false,
            short_term: vec![],
            last_pressure: MemoryPressure::None,
            task_fp: task_fingerprint(task),
        }
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
        }
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
        Self {
            agent_id:        cp.agent_id,
            cfg:             cp.cfg,
            model_cfg:       cp.model_cfg,
            messages:        cp.messages,
            specs,
            total_input:     cp.total_input,
            total_output:    cp.total_output,
            turn:            cp.turn,
            stored_response: cp.stored_response,
            terminal:        false,
            short_term:      cp.short_term,
            last_pressure:   MemoryPressure::None,
            task_fp:         task_fingerprint(&task_text),
        }
    }

    /// Total tokens consumed so far (input + output). Used by the snapshot.
    pub fn context_tokens(&self) -> u64 {
        self.total_input + self.total_output
    }

    /// Number of turn pairs currently in the Tier-2 eviction buffer.
    pub fn short_term_depth(&self) -> usize {
        self.short_term.len()
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

        // Memory pressure check: assess current token spend against budget.
        // Paging gives N+1 relief (the next inference request will be smaller);
        // it cannot reduce already-spent tokens for the current turn.
        // Advisory events are edge-triggered (fire once on transition, not every turn).
        let total_spent = self.total_input + self.total_output;
        let tokens_spent_pct = if self.cfg.token_budget > 0 {
            total_spent as f64 / self.cfg.token_budget as f64
        } else {
            0.0
        };
        let current_pressure = assess(total_spent, self.cfg.token_budget);
        match &current_pressure {
            MemoryPressure::None => {}
            MemoryPressure::Soft => {
                if self.last_pressure == MemoryPressure::None {
                    recorder.record(
                        &self.agent_id,
                        Some(self.turn),
                        EventKind::MemoryPressureAdvisory,
                        json!({
                            "agent":             &self.agent_id,
                            "turn":              self.turn,
                            "tokens_spent_pct":  tokens_spent_pct,
                            "soft_threshold":    SOFT_THRESHOLD,
                        }),
                    );
                }
            }
            MemoryPressure::Hard => {
                let n = page_count(&self.messages);
                if n > 0 {
                    match page_turns(&mut self.messages, n, self.turn) {
                        Ok(items) => {
                            let pages_moved = items.len();
                            self.short_term.extend(items);
                            recorder.record(
                                &self.agent_id,
                                Some(self.turn),
                                EventKind::MemoryPaged,
                                json!({
                                    "agent":             &self.agent_id,
                                    "turn":              self.turn,
                                    "pages_moved":       pages_moved,
                                    "short_term_depth":  self.short_term.len(),
                                    "tokens_spent_pct":  tokens_spent_pct,
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
                    // Hard pressure but context too short to page — log once on entry.
                    recorder.record(
                        &self.agent_id,
                        Some(self.turn),
                        EventKind::MemoryPressureAdvisory,
                        json!({
                            "agent":             &self.agent_id,
                            "turn":              self.turn,
                            "tokens_spent_pct":  tokens_spent_pct,
                            "soft_threshold":    SOFT_THRESHOLD,
                            "note":              "hard pressure, context too short to page",
                        }),
                    );
                }
            }
        }
        self.last_pressure = current_pressure;

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
            system: None,
            messages: self.messages.clone(),
            tools: self.specs.clone(),
            max_tokens: self.model_cfg.max_tokens,
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
            StopReason::MaxTokens => {
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

            StopReason::EndTurn | StopReason::Other(_) => {
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
                    }),
                );
                self.terminal = true;
                AgentEffect::Completed(answer)
            }

            StopReason::ToolUse => {
                if total > self.cfg.token_budget {
                    recorder.record(
                        &self.agent_id,
                        Some(self.turn),
                        EventKind::BudgetExceeded,
                        json!({ "total_tokens": total, "budget": self.cfg.token_budget }),
                    );
                    self.terminal = true;
                    return AgentEffect::Failed(format!(
                        "token budget exceeded ({total} > {})",
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
                        self.terminal = true;
                        return AgentEffect::Failed(
                            "spawn_agent must be the sole tool call per turn; \
                             cannot mix with other tools"
                                .to_string(),
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

                // Intercept send_message before tool dispatch — must be sole call.
                let send_idx = call_blocks.iter().position(|b| {
                    matches!(b, Block::ToolUse { name, .. } if name == "send_message")
                });

                if let Some(idx) = send_idx {
                    if call_blocks.len() > 1 {
                        self.terminal = true;
                        return AgentEffect::Failed(
                            "send_message must be the sole tool call per turn; \
                             cannot mix with other tools"
                                .to_string(),
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
            stop_reason: StopReason::EndTurn,
            input_tokens: 10,
            output_tokens: 5,
        }
    }

    fn tool_use_resp(id: &str, name: &str, input: serde_json::Value) -> InferenceResponse {
        InferenceResponse {
            blocks: vec![Block::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
            }],
            stop_reason: StopReason::ToolUse,
            input_tokens: 10,
            output_tokens: 5,
        }
    }

    fn agent_cfg(max_turns: u32, token_budget: u64) -> AgentConfig {
        AgentConfig {
            id: "test-agent".to_string(),
            task: String::new(),
            max_turns,
            token_budget,
            priority: 0,
            capabilities: None,
            name: None,
            description: String::new(),
            skills: vec![],
        }
    }

    fn model_cfg() -> ModelConfig {
        ModelConfig {
            provider: "mock".to_string(),
            model: "mock-model".to_string(),
            max_tokens: 4096,
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
        register_native(&mut reg, &["read_file".to_string()], None, None).unwrap();
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
            stop_reason: StopReason::ToolUse,
            input_tokens: 10,
            output_tokens: 5,
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
    fn step_spawn_mixed_with_other_tools_returns_failed() {
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
                stop_reason: StopReason::ToolUse,
                input_tokens: 10,
                output_tokens: 5,
            },
            &rec,
        );

        let eff = sm.step(&rec);
        assert!(
            matches!(&eff, AgentEffect::Failed(msg) if msg.contains("sole tool call")),
            "expected Failed when spawn_agent is mixed with other tools"
        );
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
                stop_reason:   StopReason::ToolUse,
                input_tokens:  10,
                output_tokens: 5,
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
                    stop_reason:   StopReason::ToolUse,
                    input_tokens:  5,
                    output_tokens: 3,
                })
            }
            fn model_id(&self) -> &str { "spawn-gw" }
        }

        let mut reg = crate::tools::ToolRegistry::new();
        register_native(&mut reg, &["spawn_agent".to_string()], None, None).unwrap();
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
            stop_reason:   StopReason::ToolUse,
            input_tokens:  10,
            output_tokens: 5,
        };
        task.provide_inference(response, &rec);
        let effect = task.step(&rec);
        assert!(matches!(effect, AgentEffect::Failed(_)), "mixed send_message + other tool must fail");
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
            stop_reason:   StopReason::ToolUse,
            input_tokens:  10,
            output_tokens: 5,
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
            blocks: vec![],
            stop_reason: StopReason::MaxTokens,
            input_tokens: 10,
            output_tokens: 5,
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
            blocks: vec![Block::Text { text: "partial answer cut".to_string() }],
            stop_reason: StopReason::MaxTokens,
            input_tokens: 10,
            output_tokens: 5,
        };
        task.provide_inference(response, &rec);
        let effect = task.step(&rec);
        assert!(
            matches!(effect, AgentEffect::Failed(_)),
            "MaxTokens with partial text must be Failed, not Completed (partial text discarded)"
        );
    }

    #[test]
    fn context_tokens_accumulates_input_and_output() {
        let (rec, _tmp) = recorder();
        let mut task = AgentTask::new("t", "task", &agent_cfg(5, 100_000), &model_cfg(), vec![]);
        assert_eq!(task.context_tokens(), 0, "starts at zero before any inference");
        let response = InferenceResponse {
            blocks: vec![],
            stop_reason: StopReason::EndTurn,
            input_tokens: 100,
            output_tokens: 50,
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
    fn from_checkpoint_uses_fresh_specs_and_clears_terminal() {
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
            terminal:        true, // saved as true — must be reset to false
            short_term:      vec![],
        };
        let task = AgentTask::from_checkpoint(cp, vec![fresh_spec]);
        assert_eq!(task.agent_id, "agent-42");
        assert_eq!(task.turn, 3);
        assert_eq!(task.total_input, 100);
        assert_eq!(task.total_output, 50);
        assert!(!task.terminal, "terminal must be false after restore");
        assert_eq!(task.specs[0].name, "new_tool", "specs must come from fresh registry");
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
        let budget = 100u64;
        let cfg = agent_cfg(20, budget);
        let mut sm = AgentTask::new("pg", "task", &cfg, &model_cfg(), vec![]);

        // 2 full tool cycles at 20+20=40 tokens each → total=80 → 80% < HARD_THRESHOLD
        // After each cycle: messages gains 2 entries (Assistant + User)
        for i in 0..2usize {
            let _ = sm.step(&rec);
            sm.provide_inference(
                InferenceResponse {
                    blocks:        vec![Block::ToolUse {
                        id:    format!("c{i}"),
                        name:  "no_tool".to_string(),
                        input: serde_json::json!({}),
                    }],
                    stop_reason:   StopReason::ToolUse,
                    input_tokens:  20,
                    output_tokens: 20,
                },
                &rec,
            );
            let _ = sm.step(&rec); // → CallTools (pushes Assistant msg)
            sm.provide_tool_results(
                vec![Block::ToolResult {
                    tool_use_id: format!("c{i}"),
                    content:     "r".to_string(),
                    is_error:    false,
                }],
                &rec,
            ); // pushes User(tool_results)
        }
        // total=80, messages.len()=5

        // One more inference (5+6=11 tokens) → total=91 → 91% > HARD_THRESHOLD
        let _ = sm.step(&rec);
        sm.provide_inference(
            InferenceResponse {
                blocks:        vec![Block::ToolUse {
                    id:    "c2".to_string(),
                    name:  "no_tool".to_string(),
                    input: serde_json::json!({}),
                }],
                stop_reason:   StopReason::ToolUse,
                input_tokens:  5,
                output_tokens: 6,
            },
            &rec,
        );
        let _ = sm.step(&rec); // → CallTools (pushes Assistant)
        sm.provide_tool_results(
            vec![Block::ToolResult {
                tool_use_id: "c2".to_string(),
                content:     "r2".to_string(),
                is_error:    false,
            }],
            &rec,
        );
        // total=91, messages.len()=7

        // Next step_need_infer: Hard pressure, page_count(7)=1 pair → page!
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
                blocks:        vec![Block::ToolUse {
                    id:    "c0".to_string(),
                    name:  "no_tool".to_string(),
                    input: serde_json::json!({}),
                }],
                stop_reason:   StopReason::ToolUse,
                input_tokens:  37,
                output_tokens: 38,
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
}
