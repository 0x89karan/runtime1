use anyhow::Result;
use serde_json::json;

use crate::{
    config::{AgentConfig, ModelConfig},
    flight_recorder::{EventKind, FlightRecorder},
    inference::InferenceGateway,
    tools::ToolRegistry,
};

use super::{run_tools_sequential, AgentEffect, AgentTask};

/// Single-agent backward-compat shim. The scheduler is the primary execution engine
/// from p1.2 onward; this function is kept for tests only. It bypasses capability
/// enforcement (passes `None` cap_set) — callers with capability-scoped agents must
/// use the Scheduler.
pub(crate) async fn run(
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
                let results =
                    run_tools_sequential(agent_id, sm.turn(), &blocks, registry, None, recorder)
                        .await;
                sm.provide_tool_results(results, recorder);
            }

            AgentEffect::Completed(answer) => return Ok(answer),
            AgentEffect::Failed(msg) => return Err(anyhow::anyhow!("{msg}")),
        }
    }
}
