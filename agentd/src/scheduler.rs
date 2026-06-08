use std::{collections::HashMap, pin::Pin, sync::Arc};

use futures::stream::{FuturesUnordered, StreamExt};
use serde_json::json;

use crate::{
    agent::{run_tools_sequential, AgentEffect, AgentTask},
    config::{AgentConfig, ModelConfig},
    flight_recorder::{EventKind, FlightRecorder},
    inference::{Block, InferenceGateway, InferenceResponse},
    tools::ToolRegistry,
};

type PendingFut = Pin<Box<dyn std::future::Future<Output = EffectResult> + Send>>;

enum EffectResult {
    Inference {
        agent_id: String,
        result:   anyhow::Result<InferenceResponse>,
    },
    Tools {
        agent_id: String,
        results:  Vec<Block>,
    },
}

pub struct Scheduler {
    agents:   HashMap<String, AgentTask>,
    gateway:  Arc<dyn InferenceGateway + Send + Sync>,
    registry: Arc<ToolRegistry>,
    recorder: Arc<FlightRecorder>,
}

impl Scheduler {
    /// Create a scheduler for the given agent configs. Returns Err on duplicate agent IDs.
    pub fn new(
        agent_configs: Vec<AgentConfig>,
        model_cfg: &ModelConfig,
        gateway: Arc<dyn InferenceGateway + Send + Sync>,
        registry: Arc<ToolRegistry>,
        recorder: Arc<FlightRecorder>,
    ) -> anyhow::Result<Self> {
        let specs = registry.specs();
        let mut agents = HashMap::with_capacity(agent_configs.len());
        for cfg in agent_configs {
            anyhow::ensure!(
                !agents.contains_key(&cfg.id),
                "duplicate agent id: {}",
                cfg.id
            );
            let task = AgentTask::new(&cfg.id, &cfg.task, &cfg, model_cfg, specs.clone());
            agents.insert(cfg.id.clone(), task);
        }
        Ok(Self { agents, gateway, registry, recorder })
    }

    /// Run all agents concurrently until every one reaches a terminal state.
    /// Returns a map from agent_id to Ok(answer) or Err.
    pub async fn run(self) -> HashMap<String, anyhow::Result<String>> {
        let Self { mut agents, gateway, registry, recorder } = self;

        let mut outcomes: HashMap<String, anyhow::Result<String>> = HashMap::new();
        let mut pending: FuturesUnordered<PendingFut> = FuturesUnordered::new();

        // Seed: step each agent once to kick off the first InferenceRequest.
        let ids: Vec<String> = agents.keys().cloned().collect();
        for id in ids {
            let (effect, turn) = {
                let sm = agents.get_mut(&id).unwrap();
                let t = sm.turn();
                (sm.step(&recorder), t)
            };
            enqueue(effect, id, turn, &mut outcomes, &gateway, &registry, &recorder, &mut pending);
        }

        while let Some(er) = pending.next().await {
            match er {
                EffectResult::Inference { agent_id, result: Err(e) } => {
                    recorder.record(
                        &agent_id,
                        None,
                        EventKind::Error,
                        json!({ "stage": "inference", "error": e.to_string() }),
                    );
                    recorder.record(
                        &agent_id,
                        None,
                        EventKind::AgentFailed,
                        json!({ "reason": "inference_error", "error": e.to_string() }),
                    );
                    outcomes.insert(agent_id, Err(e));
                }
                EffectResult::Inference { agent_id, result: Ok(resp) } => {
                    let (effect, turn) = {
                        let sm = agents
                            .get_mut(&agent_id)
                            .expect("agent_id in EffectResult must be present in agents map");
                        sm.provide_inference(resp, &recorder);
                        let t = sm.turn();
                        (sm.step(&recorder), t)
                    };
                    enqueue(
                        effect, agent_id, turn, &mut outcomes,
                        &gateway, &registry, &recorder, &mut pending,
                    );
                }
                EffectResult::Tools { agent_id, results } => {
                    let (effect, turn) = {
                        let sm = agents
                            .get_mut(&agent_id)
                            .expect("agent_id in EffectResult must be present in agents map");
                        sm.provide_tool_results(results, &recorder);
                        let t = sm.turn();
                        (sm.step(&recorder), t)
                    };
                    enqueue(
                        effect, agent_id, turn, &mut outcomes,
                        &gateway, &registry, &recorder, &mut pending,
                    );
                }
            }
        }

        outcomes
    }
}

#[allow(clippy::too_many_arguments)]
fn enqueue(
    effect: AgentEffect,
    agent_id: String,
    turn: u32, // only consumed by the CallTools arm; unused in Infer/Completed/Failed arms
    outcomes: &mut HashMap<String, anyhow::Result<String>>,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
    pending: &mut FuturesUnordered<PendingFut>,
) {
    match effect {
        AgentEffect::Infer(req) => {
            let gw = Arc::clone(gateway);
            let id = agent_id;
            pending.push(Box::pin(async move {
                EffectResult::Inference { agent_id: id, result: gw.infer(req).await }
            }));
        }
        AgentEffect::CallTools(blocks) => {
            let reg = Arc::clone(registry);
            let rec = Arc::clone(recorder);
            let id = agent_id;
            pending.push(Box::pin(async move {
                let results = run_tools_sequential(&id, turn, &blocks, &reg, &rec).await;
                EffectResult::Tools { agent_id: id, results }
            }));
        }
        AgentEffect::Completed(answer) => {
            outcomes.insert(agent_id, Ok(answer));
        }
        AgentEffect::Failed(msg) => {
            outcomes.insert(agent_id, Err(anyhow::anyhow!("{msg}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::AgentConfig,
        flight_recorder::FlightRecorder,
        inference::{Block, InferenceGateway, InferenceRequest, InferenceResponse, StopReason},
        tools::ToolRegistry,
    };
    use std::sync::Mutex;
    use tempfile::NamedTempFile;

    // ── Test helpers ─────────────────────────────────────────────────────────

    struct MockGateway {
        responses: Arc<Mutex<Vec<InferenceResponse>>>,
    }

    impl MockGateway {
        fn new(responses: Vec<InferenceResponse>) -> Self {
            Self { responses: Arc::new(Mutex::new(responses)) }
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
        fn model_id(&self) -> &str { "mock" }
    }

    struct FailGateway;

    #[async_trait::async_trait]
    impl InferenceGateway for FailGateway {
        async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
            Err(anyhow::anyhow!("network down"))
        }
        fn model_id(&self) -> &str { "fail" }
    }

    fn end_turn(text: &str) -> InferenceResponse {
        InferenceResponse {
            blocks:       vec![Block::Text { text: text.to_string() }],
            stop_reason:  StopReason::EndTurn,
            input_tokens: 10,
            output_tokens: 5,
        }
    }

    fn agent_cfg(id: &str, task: &str) -> AgentConfig {
        AgentConfig {
            id:           id.to_string(),
            task:         task.to_string(),
            max_turns:    5,
            token_budget: 100_000,
        }
    }

    fn model_cfg() -> crate::config::ModelConfig {
        crate::config::ModelConfig {
            provider:   "mock".to_string(),
            model:      "mock-model".to_string(),
            max_tokens: 4096,
        }
    }

    fn recorder() -> (Arc<FlightRecorder>, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let rec = FlightRecorder::new(tmp.path()).unwrap();
        (Arc::new(rec), tmp)
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn scheduler_runs_two_agents_concurrently() {
        let gw = Arc::new(MockGateway::new(vec![
            end_turn("alpha done"),
            end_turn("beta done"),
        ]));
        let (rec, _tmp) = recorder();
        let registry = Arc::new(ToolRegistry::new());

        let sched = Scheduler::new(
            vec![agent_cfg("alpha", "task a"), agent_cfg("beta", "task b")],
            &model_cfg(),
            gw,
            registry,
            rec,
        )
        .unwrap();

        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 2);
        // Both agents should complete successfully
        let alpha = outcomes["alpha"].as_ref().unwrap();
        let beta  = outcomes["beta"].as_ref().unwrap();
        // The mock returns responses in FIFO order; with concurrent execution the exact
        // assignment depends on scheduling order, so we just check both are non-empty.
        assert!(!alpha.is_empty(), "alpha answer must be non-empty");
        assert!(!beta.is_empty(),  "beta answer must be non-empty");
    }

    #[tokio::test]
    async fn scheduler_back_compat_single_agent() {
        let gw = Arc::new(MockGateway::new(vec![end_turn("solo answer")]));
        let (rec, _tmp) = recorder();
        let registry = Arc::new(ToolRegistry::new());

        let sched = Scheduler::new(
            vec![agent_cfg("solo", "do something")],
            &model_cfg(),
            gw,
            registry,
            rec,
        )
        .unwrap();

        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes["solo"].as_ref().unwrap(), "solo answer");
    }

    #[tokio::test]
    async fn scheduler_one_agent_fails_other_completes() {
        // alpha gets FailGateway; beta needs its own gateway with a success response.
        // Since a single gateway is shared, we simulate the failure via the FIFO queue:
        // the first infer() call (alpha) returns Err, the second (beta) returns Ok.
        struct PartialFailGateway {
            calls: Arc<Mutex<u32>>,
        }
        #[async_trait::async_trait]
        impl InferenceGateway for PartialFailGateway {
            async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
                let mut n = self.calls.lock().unwrap();
                *n += 1;
                // First call fails, subsequent calls succeed.
                if *n == 1 {
                    Err(anyhow::anyhow!("simulated failure"))
                } else {
                    Ok(InferenceResponse {
                        blocks:       vec![Block::Text { text: "ok".to_string() }],
                        stop_reason:  StopReason::EndTurn,
                        input_tokens: 5,
                        output_tokens: 3,
                    })
                }
            }
            fn model_id(&self) -> &str { "partial" }
        }

        let gw = Arc::new(PartialFailGateway { calls: Arc::new(Mutex::new(0)) });
        let (rec, _tmp) = recorder();
        let registry = Arc::new(ToolRegistry::new());

        // agent-a starts first (HashMap iteration order is not guaranteed, but we check
        // that exactly one fails and one succeeds, regardless of which is which).
        let sched = Scheduler::new(
            vec![agent_cfg("agent-a", "task a"), agent_cfg("agent-b", "task b")],
            &model_cfg(),
            gw,
            registry,
            rec,
        )
        .unwrap();

        let outcomes = sched.run().await;

        let failed  = outcomes.values().filter(|r| r.is_err()).count();
        let success = outcomes.values().filter(|r| r.is_ok()).count();
        assert_eq!(failed,  1, "exactly one agent should fail");
        assert_eq!(success, 1, "exactly one agent should succeed");
    }

    #[test]
    fn scheduler_new_rejects_duplicate_ids() {
        let gw = Arc::new(FailGateway);
        let (rec, _tmp) = recorder();
        let registry = Arc::new(ToolRegistry::new());

        let result = Scheduler::new(
            vec![agent_cfg("dup", "task 1"), agent_cfg("dup", "task 2")],
            &model_cfg(),
            gw,
            registry,
            rec,
        );
        let err = result.err().expect("expected Err on duplicate agent id");
        assert!(err.to_string().contains("duplicate agent id"));
    }

    #[tokio::test]
    async fn scheduler_zero_agents_returns_empty() {
        let gw = Arc::new(FailGateway);
        let (rec, _tmp) = recorder();
        let registry = Arc::new(ToolRegistry::new());

        let sched = Scheduler::new(vec![], &model_cfg(), gw, registry, rec).unwrap();
        let outcomes = sched.run().await;
        assert!(outcomes.is_empty(), "zero agents should yield empty outcomes map");
    }
}
