#[cfg(test)]
pub mod driver;

use serde_json::json;

const PREVIEW_CHARS: usize = 200;

use crate::{
    config::{AgentConfig, ModelConfig},
    flight_recorder::{EventKind, FlightRecorder},
    inference::{Block, InferenceRequest, InferenceResponse, Msg, Role, StopReason, ToolSpec},
    tools::ToolRegistry,
};

#[must_use = "AgentEffect names the IO the scheduler must perform; ignoring it stalls the agent"]
pub enum AgentEffect {
    Infer(InferenceRequest),
    /// Only Block::ToolUse variants; step() filters the rest before returning.
    CallTools(Vec<Block>),
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
        }
    }

    /// Current turn index. The driver reads this when recording EventKind::Error
    /// after a gateway failure, to preserve the correct turn number in the log.
    pub fn turn(&self) -> u32 {
        self.turn
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
            StopReason::EndTurn | StopReason::MaxTokens | StopReason::Other(_) => {
                // TODO(p2): MaxTokens with no Text block produces empty Ok("").
                // Surface a warning or return BudgetExceeded so callers can distinguish
                // a real answer from a mid-generation cut-off.
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
    blocks: &[Block],
    registry: &ToolRegistry,
    cap_set: Option<&[crate::capability::Capability]>,
    recorder: &FlightRecorder,
) -> Vec<Block> {
    let mut results: Vec<Block> = Vec::new();
    for block in blocks {
        let Block::ToolUse { id, name, input } = block else {
            continue;
        };

        recorder.record(
            agent_id,
            Some(turn),
            EventKind::ToolCall,
            json!({ "id": id, "name": name, "input": input }),
        );

        let (content, is_error) = match registry
            .invoke(name, input.clone(), agent_id, cap_set, recorder)
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
                        "error": msg,
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

fn truncate(s: &str, max_chars: usize) -> String {
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
        register_native(&mut reg, &["read_file".to_string()]).unwrap();
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

        let results = run_tools_sequential("agent", 0, &blocks, &registry, None, &rec).await;

        // Text block skipped; unknown tool returns an error result (not a panic)
        assert_eq!(results.len(), 1, "only the ToolUse block should produce a result");
        let Block::ToolResult { tool_use_id, is_error, .. } = &results[0] else {
            panic!("expected ToolResult");
        };
        assert_eq!(tool_use_id, "call_1");
        assert!(is_error, "unknown tool should produce is_error=true result");
    }
}
