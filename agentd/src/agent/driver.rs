use anyhow::Result;
use serde_json::json;

use crate::{
    config::{AgentConfig, ModelConfig},
    flight_recorder::{EventKind, FlightRecorder},
    inference::{Block, InferenceGateway},
    tools::ToolRegistry,
};

use super::{AgentEffect, AgentTask};

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
    let specs = registry.specs();
    let mut sm = AgentTask::new(agent_id, task, cfg, model_cfg, specs);

    loop {
        match sm.step(recorder) {
            AgentEffect::Infer(req) => {
                let response = match gateway.infer(req).await {
                    Ok(r) => r,
                    Err(e) => {
                        recorder.record(
                            agent_id,
                            Some(sm.turn()),
                            EventKind::Error,
                            json!({ "stage": "inference", "error": e.to_string() }),
                        );
                        return Err(e);
                    }
                };
                sm.provide_inference(response, recorder);
            }

            AgentEffect::CallTools(blocks) => {
                let mut results: Vec<Block> = Vec::new();
                for block in &blocks {
                    let Block::ToolUse { id, name, input } = block else {
                        continue;
                    };

                    recorder.record(
                        agent_id,
                        Some(sm.turn()),
                        EventKind::ToolCall,
                        json!({ "id": id, "name": name, "input": input }),
                    );

                    let (content, is_error) = match registry.invoke(name, input.clone()).await {
                        Ok(s) => {
                            recorder.record(
                                agent_id,
                                Some(sm.turn()),
                                EventKind::ToolResult,
                                json!({
                                    "id": id, "name": name,
                                    "is_error": false,
                                    "preview": super::truncate(&s, 200),
                                }),
                            );
                            (s, false)
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            recorder.record(
                                agent_id,
                                Some(sm.turn()),
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
                sm.provide_tool_results(results, recorder);
            }

            AgentEffect::Completed(answer) => return Ok(answer),
            AgentEffect::Failed(msg) => return Err(anyhow::anyhow!("{msg}")),
        }
    }
}
