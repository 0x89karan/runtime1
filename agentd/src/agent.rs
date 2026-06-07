use anyhow::Result;
use serde_json::json;

use crate::{
    config::{AgentConfig, ModelConfig},
    flight_recorder::{EventKind, FlightRecorder},
    inference::{Block, InferenceGateway, InferenceRequest, Msg, Role, StopReason},
    tools::ToolRegistry,
};

fn truncate(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars().peekable();
    let out: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{out}…")
    } else {
        out
    }
}

/// Drive the perceive → infer → act → observe loop until the model produces a
/// final answer, the token budget is exhausted, or `max_turns` is reached.
/// Tool errors are returned to the model as `is_error` results — the loop
/// never panics on bad tool input or a failed tool call.
pub async fn run(
    agent_id: &str,
    task: &str,
    cfg: &AgentConfig,
    model_cfg: &ModelConfig,
    gateway: &dyn InferenceGateway,
    registry: &ToolRegistry,
    recorder: &FlightRecorder,
) -> Result<String> {
    recorder.record(
        agent_id,
        None,
        EventKind::Perceive,
        json!({ "source": "task", "preview": truncate(task, 200) }),
    );

    let mut messages = vec![Msg {
        role: Role::User,
        blocks: vec![Block::Text {
            text: task.to_string(),
        }],
    }];
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let specs = registry.specs();

    for turn in 0..cfg.max_turns {
        recorder.record(
            agent_id,
            Some(turn),
            EventKind::InferenceRequest,
            json!({
                "model": gateway.model_id(),
                "msg_count": messages.len(),
                "tool_count": specs.len(),
            }),
        );

        let response = match gateway
            .infer(InferenceRequest {
                system: None,
                messages: messages.clone(),
                tools: specs.clone(),
                max_tokens: model_cfg.max_tokens,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                recorder.record(
                    agent_id,
                    Some(turn),
                    EventKind::Error,
                    json!({ "stage": "inference", "error": e.to_string() }),
                );
                return Err(e);
            }
        };

        total_input += u64::from(response.input_tokens);
        total_output += u64::from(response.output_tokens);
        let total = total_input + total_output;

        recorder.record(
            agent_id,
            Some(turn),
            EventKind::InferenceResponse,
            json!({
                "stop_reason": response.stop_reason.as_str(),
                "input_tokens":  response.input_tokens,
                "output_tokens": response.output_tokens,
                "total_tokens":  total,
            }),
        );

        messages.push(Msg {
            role: Role::Assistant,
            blocks: response.blocks.clone(),
        });

        match response.stop_reason {
            StopReason::EndTurn | StopReason::MaxTokens | StopReason::Other(_) => {
                // TODO(p2): MaxTokens with no Text block produces an empty Ok("").
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
                    agent_id,
                    Some(turn),
                    EventKind::AgentCompleted,
                    json!({
                        "turns": turn + 1,
                        "total_tokens": total,
                        "answer_preview": truncate(&answer, 200),
                    }),
                );
                return Ok(answer);
            }

            StopReason::ToolUse => {
                if total > cfg.token_budget {
                    recorder.record(
                        agent_id,
                        Some(turn),
                        EventKind::BudgetExceeded,
                        json!({ "total_tokens": total, "budget": cfg.token_budget }),
                    );
                    return Err(anyhow::anyhow!(
                        "token budget exceeded ({total} > {})",
                        cfg.token_budget
                    ));
                }

                let mut results: Vec<Block> = Vec::new();
                for block in &response.blocks {
                    let Block::ToolUse { id, name, input } = block else {
                        continue;
                    };

                    recorder.record(
                        agent_id,
                        Some(turn),
                        EventKind::ToolCall,
                        json!({ "id": id, "name": name, "input": input }),
                    );

                    let (content, is_error) = match registry.invoke(name, input.clone()).await {
                        Ok(s) => {
                            recorder.record(
                                agent_id,
                                Some(turn),
                                EventKind::ToolResult,
                                json!({
                                    "id": id, "name": name,
                                    "is_error": false,
                                    "preview": truncate(&s, 200),
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

                recorder.record(
                    agent_id,
                    Some(turn),
                    EventKind::Observe,
                    json!({ "result_count": results.len() }),
                );

                messages.push(Msg {
                    role: Role::User,
                    blocks: results,
                });
            }
        }
    }

    let total = total_input + total_output;
    recorder.record(
        agent_id,
        Some(cfg.max_turns.saturating_sub(1)),
        EventKind::MaxTurnsReached,
        json!({ "max_turns": cfg.max_turns, "total_tokens": total }),
    );
    Err(anyhow::anyhow!(
        "max turns ({}) reached without a final answer",
        cfg.max_turns
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, ModelConfig};
    use crate::inference::{InferenceResponse, StopReason};
    use std::sync::{Arc, Mutex};
    use tempfile::NamedTempFile;

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

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> InferenceResponse {
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

    #[tokio::test]
    async fn direct_answer_returns_text() {
        let gw = MockGateway::new(vec![end_turn("hello from mock")]);
        let reg = ToolRegistry::new();
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

        // Turn 1: model asks to read a file. Turn 2: model returns final answer.
        let gw = MockGateway::new(vec![
            tool_use(
                "call_1",
                "read_file",
                serde_json::json!({ "path": path.to_str().unwrap() }),
            ),
            end_turn("the file says: agent data"),
        ]);

        let mut reg = ToolRegistry::new();
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
        // Model calls a tool that doesn't exist in registry — must not panic.
        let gw = MockGateway::new(vec![
            tool_use("call_1", "nonexistent_tool", serde_json::json!({})),
            end_turn("tool failed gracefully"),
        ]);
        let reg = ToolRegistry::new(); // no tools registered
        let (rec, _tmp) = recorder();

        let answer = run("a", "task", &agent_cfg(5, 100_000), &model_cfg(), &gw, &reg, &rec)
            .await
            .unwrap();
        assert_eq!(answer, "tool failed gracefully");
    }

    #[tokio::test]
    async fn budget_exceeded_returns_error() {
        // budget = 20 tokens; each response costs 15 (10 in + 5 out).
        // After the first tool-use response total = 15, within budget.
        // After the second response total = 30 > 20 → budget_exceeded before act.
        let gw = MockGateway::new(vec![
            // First response: tool_use. total=15, within budget → dispatch.
            tool_use("c1", "no_tool", serde_json::json!({})),
            // Second response: tool_use. total=30, over budget → stop.
            tool_use("c2", "no_tool", serde_json::json!({})),
        ]);
        let reg = ToolRegistry::new();
        let (rec, _tmp) = recorder();

        let err = run("a", "task", &agent_cfg(10, 20), &model_cfg(), &gw, &reg, &rec)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("budget exceeded"), "got: {err}");
    }

    #[tokio::test]
    async fn max_turns_reached_returns_error() {
        // max_turns=2; mock keeps returning tool_use forever.
        let gw = MockGateway::new(vec![
            tool_use("c1", "no_tool", serde_json::json!({})),
            tool_use("c2", "no_tool", serde_json::json!({})),
        ]);
        let reg = ToolRegistry::new();
        let (rec, _tmp) = recorder();

        let err = run("a", "task", &agent_cfg(2, 1_000_000), &model_cfg(), &gw, &reg, &rec)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("max turns"), "got: {err}");
    }

    #[tokio::test]
    async fn inference_error_propagates() {
        // Gateway returns an error on the first call.
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
        let reg = ToolRegistry::new();
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
        // Multi-byte chars: "á" is 2 bytes but 1 char.
        let unicode = "áéíóú";
        assert_eq!(truncate(unicode, 3), "áéí…");
        assert_eq!(truncate(unicode, 5), "áéíóú");
    }
}
