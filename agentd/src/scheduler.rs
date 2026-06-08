use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
    pin::Pin,
    sync::Arc,
};

use futures::stream::{FuturesUnordered, StreamExt};
use serde_json::json;

use crate::{
    agent::{run_tools_sequential, AgentEffect, AgentTask},
    config::{AgentConfig, ModelConfig, SchedulerConfig},
    flight_recorder::{EventKind, FlightRecorder},
    inference::{Block, InferenceGateway, InferenceRequest, InferenceResponse},
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

/// An inference request waiting for a slot + budget to open.
struct DeferredInfer {
    priority: u32,
    seq:      u64,
    agent_id: String,
    request:  InferenceRequest,
    turn:     u32,
}

impl PartialEq for DeferredInfer {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}
impl Eq for DeferredInfer {}

impl Ord for DeferredInfer {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority pops first; FIFO (lower seq) breaks ties.
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for DeferredInfer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub struct Scheduler {
    agents:   HashMap<String, AgentTask>,
    sched:    SchedulerConfig,
    gateway:  Arc<dyn InferenceGateway + Send + Sync>,
    registry: Arc<ToolRegistry>,
    recorder: Arc<FlightRecorder>,
}

impl Scheduler {
    /// Create a scheduler for the given agent configs. Returns Err on duplicate agent IDs.
    pub fn new(
        agent_configs: Vec<AgentConfig>,
        model_cfg: &ModelConfig,
        sched: SchedulerConfig,
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
        Ok(Self { agents, sched, gateway, registry, recorder })
    }

    /// Run all agents concurrently until every one reaches a terminal state.
    /// Returns a map from agent_id to Ok(answer) or Err.
    pub async fn run(self) -> HashMap<String, anyhow::Result<String>> {
        let Self { mut agents, sched, gateway, registry, recorder } = self;

        let mut outcomes: HashMap<String, anyhow::Result<String>> = HashMap::new();
        let mut pending: FuturesUnordered<PendingFut> = FuturesUnordered::new();
        let mut deferred: BinaryHeap<DeferredInfer> = BinaryHeap::new();
        let mut deferred_seq: u64 = 0;
        let mut in_flight: usize = 0;
        let mut tokens_spent: u64 = 0;

        // Seed: step each agent once to kick off the first effect.
        let ids: Vec<String> = agents.keys().cloned().collect();
        for id in ids {
            let priority = agents[&id].priority();
            let (effect, turn) = {
                let sm = agents.get_mut(&id).unwrap();
                let t = sm.turn();
                (sm.step(&recorder), t)
            };
            enqueue_or_defer(
                effect, id, turn, priority,
                &mut outcomes, &mut deferred, &mut deferred_seq,
                &mut in_flight, tokens_spent, &sched,
                &gateway, &registry, &recorder, &mut pending,
            );
        }

        while let Some(er) = pending.next().await {
            match er {
                EffectResult::Inference { agent_id, result: Err(e) } => {
                    debug_assert!(in_flight > 0, "in_flight underflow on inference error");
                    in_flight -= 1;
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
                    drain_deferred(
                        &mut deferred, &mut in_flight, tokens_spent,
                        &sched, &gateway, &recorder, &mut pending, &mut outcomes,
                    );
                }
                EffectResult::Inference { agent_id, result: Ok(resp) } => {
                    debug_assert!(in_flight > 0, "in_flight underflow on inference success");
                    in_flight -= 1;
                    let new_tokens =
                        u64::from(resp.input_tokens) + u64::from(resp.output_tokens);
                    let priority = agents[&agent_id].priority();
                    let (effect, turn) = {
                        let sm = agents
                            .get_mut(&agent_id)
                            .expect("agent_id in EffectResult must be present in agents map");
                        sm.provide_inference(resp, &recorder);
                        let t = sm.turn();
                        (sm.step(&recorder), t)
                    };
                    tokens_spent = tokens_spent.saturating_add(new_tokens);
                    // Drain deferred agents first (they were waiting for a slot to open),
                    // then re-enqueue the completing agent's next step. This gives queued
                    // agents priority over the agent that just ran — intentional fairness policy.
                    drain_deferred(
                        &mut deferred, &mut in_flight, tokens_spent,
                        &sched, &gateway, &recorder, &mut pending, &mut outcomes,
                    );
                    enqueue_or_defer(
                        effect, agent_id, turn, priority,
                        &mut outcomes, &mut deferred, &mut deferred_seq,
                        &mut in_flight, tokens_spent, &sched,
                        &gateway, &registry, &recorder, &mut pending,
                    );
                }
                EffectResult::Tools { agent_id, results } => {
                    let priority = agents[&agent_id].priority();
                    let (effect, turn) = {
                        let sm = agents
                            .get_mut(&agent_id)
                            .expect("agent_id in EffectResult must be present in agents map");
                        sm.provide_tool_results(results, &recorder);
                        let t = sm.turn();
                        (sm.step(&recorder), t)
                    };
                    enqueue_or_defer(
                        effect, agent_id, turn, priority,
                        &mut outcomes, &mut deferred, &mut deferred_seq,
                        &mut in_flight, tokens_spent, &sched,
                        &gateway, &registry, &recorder, &mut pending,
                    );
                }
            }
        }

        // Any agents still in the deferred queue never got a slot — admission denied.
        while let Some(d) = deferred.pop() {
            recorder.record(
                &d.agent_id,
                None,
                EventKind::AgentAdmissionDenied,
                json!({ "reason": "scheduler_shutdown_with_pending_items", "tokens_spent": tokens_spent }),
            );
            outcomes.insert(
                d.agent_id,
                Err(anyhow::anyhow!("admission denied: scheduler shut down with pending items")),
            );
        }

        outcomes
    }
}

/// Drain the deferred queue, admitting agents until the cap or budget is hit.
/// Agents that can never be admitted (budget exhausted) are denied immediately.
// TODO(p1.x): introduce a SchedulerState struct to reduce the argument count here and in enqueue_or_defer.
#[allow(clippy::too_many_arguments)]
fn drain_deferred(
    deferred: &mut BinaryHeap<DeferredInfer>,
    in_flight: &mut usize,
    tokens_spent: u64,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    recorder: &Arc<FlightRecorder>,
    pending: &mut FuturesUnordered<PendingFut>,
    outcomes: &mut HashMap<String, anyhow::Result<String>>,
) {
    let budget_ok  = |spent: u64| sched.global_token_budget == 0 || spent < sched.global_token_budget;
    let slot_ok    = |inf: usize| sched.max_concurrent_inferences == 0 || inf < sched.max_concurrent_inferences;

    // If budget is permanently exhausted, deny everything in the queue.
    if !budget_ok(tokens_spent) {
        while let Some(d) = deferred.pop() {
            recorder.record(
                &d.agent_id,
                None,
                EventKind::AgentAdmissionDenied,
                json!({ "reason": "global_budget_exhausted", "tokens_spent": tokens_spent }),
            );
            outcomes.insert(
                d.agent_id,
                Err(anyhow::anyhow!("admission denied: global token budget exhausted")),
            );
        }
        return;
    }

    // Admit as many as slots allow.
    while !deferred.is_empty() && slot_ok(*in_flight) {
        let d = deferred.pop().expect("checked non-empty");
        *in_flight += 1;
        recorder.record(
            &d.agent_id,
            Some(d.turn),
            EventKind::AgentScheduled,
            json!({ "reason": "slot_opened", "in_flight": *in_flight }),
        );
        let gw = Arc::clone(gateway);
        let id = d.agent_id;
        pending.push(Box::pin(async move {
            EffectResult::Inference { agent_id: id, result: gw.infer(d.request).await }
        }));
    }
}

/// Dispatch an AgentEffect: immediately schedule inference if admission passes,
/// defer it if the concurrency cap is full, deny it if the budget is exhausted,
/// or record terminal effects (Completed/Failed) directly.
#[allow(clippy::too_many_arguments)] // TODO(p1.x): collapse into SchedulerState
fn enqueue_or_defer(
    effect: AgentEffect,
    agent_id: String,
    turn: u32,
    priority: u32,
    outcomes: &mut HashMap<String, anyhow::Result<String>>,
    deferred: &mut BinaryHeap<DeferredInfer>,
    deferred_seq: &mut u64,
    in_flight: &mut usize,
    tokens_spent: u64,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
    pending: &mut FuturesUnordered<PendingFut>,
) {
    let slot_ok   = sched.max_concurrent_inferences == 0 || *in_flight < sched.max_concurrent_inferences;
    let budget_ok = sched.global_token_budget == 0 || tokens_spent < sched.global_token_budget;

    match effect {
        AgentEffect::Infer(req) => {
            if slot_ok && budget_ok {
                *in_flight += 1;
                recorder.record(
                    &agent_id,
                    Some(turn),
                    EventKind::AgentScheduled,
                    json!({ "in_flight": *in_flight }),
                );
                let gw = Arc::clone(gateway);
                let id = agent_id;
                pending.push(Box::pin(async move {
                    EffectResult::Inference { agent_id: id, result: gw.infer(req).await }
                }));
            } else if !budget_ok {
                recorder.record(
                    &agent_id,
                    Some(turn),
                    EventKind::AgentAdmissionDenied,
                    json!({ "reason": "global_budget_exhausted", "tokens_spent": tokens_spent }),
                );
                outcomes.insert(
                    agent_id,
                    Err(anyhow::anyhow!("admission denied: global token budget exhausted")),
                );
            } else {
                // Slot full — defer.
                let seq = *deferred_seq;
                *deferred_seq += 1;
                recorder.record(
                    &agent_id,
                    Some(turn),
                    EventKind::AgentDeferred,
                    json!({ "priority": priority, "seq": seq, "in_flight": *in_flight }),
                );
                deferred.push(DeferredInfer { priority, seq, agent_id, request: req, turn });
            }
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

    fn end_turn(text: &str, input_tok: u32, output_tok: u32) -> InferenceResponse {
        InferenceResponse {
            blocks:        vec![Block::Text { text: text.to_string() }],
            stop_reason:   StopReason::EndTurn,
            input_tokens:  input_tok,
            output_tokens: output_tok,
        }
    }

    fn agent_cfg(id: &str, task: &str) -> AgentConfig {
        AgentConfig {
            id:           id.to_string(),
            task:         task.to_string(),
            max_turns:    5,
            token_budget: 100_000,
            priority:     0,
        }
    }

    fn agent_cfg_pri(id: &str, task: &str, priority: u32) -> AgentConfig {
        AgentConfig { priority, ..agent_cfg(id, task) }
    }

    fn model_cfg() -> crate::config::ModelConfig {
        crate::config::ModelConfig {
            provider:   "mock".to_string(),
            model:      "mock-model".to_string(),
            max_tokens: 4096,
        }
    }

    fn sched_cfg(global_token_budget: u64, max_concurrent_inferences: usize) -> SchedulerConfig {
        SchedulerConfig { global_token_budget, max_concurrent_inferences }
    }

    fn unlimited() -> SchedulerConfig {
        sched_cfg(0, 0)
    }

    fn recorder() -> (Arc<FlightRecorder>, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let rec = FlightRecorder::new(tmp.path()).unwrap();
        (Arc::new(rec), tmp)
    }

    fn make_scheduler(
        agents: Vec<AgentConfig>,
        sched: SchedulerConfig,
        gw: impl InferenceGateway + Send + Sync + 'static,
    ) -> Scheduler {
        // _tmp dropped here but the open fd remains valid on Unix until FlightRecorder drops.
        let (rec, _tmp) = recorder();
        Scheduler::new(
            agents,
            &model_cfg(),
            sched,
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
        )
        .unwrap()
    }

    // ── p1.2 regression: unlimited config behaves identically ─────────────

    #[tokio::test]
    async fn scheduler_runs_two_agents_concurrently() {
        let gw = MockGateway::new(vec![
            end_turn("alpha done", 10, 5),
            end_turn("beta done",  10, 5),
        ]);
        let sched = make_scheduler(
            vec![agent_cfg("alpha", "task a"), agent_cfg("beta", "task b")],
            unlimited(),
            gw,
        );
        let outcomes = sched.run().await;
        assert_eq!(outcomes.len(), 2);
        assert!(!outcomes["alpha"].as_ref().unwrap().is_empty());
        assert!(!outcomes["beta"].as_ref().unwrap().is_empty());
    }

    #[tokio::test]
    async fn scheduler_back_compat_single_agent() {
        let gw = MockGateway::new(vec![end_turn("solo answer", 10, 5)]);
        let sched = make_scheduler(vec![agent_cfg("solo", "do something")], unlimited(), gw);
        let outcomes = sched.run().await;
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes["solo"].as_ref().unwrap(), "solo answer");
    }

    #[tokio::test]
    async fn scheduler_one_agent_fails_other_completes() {
        struct PartialFailGateway {
            calls: Arc<Mutex<u32>>,
        }
        #[async_trait::async_trait]
        impl InferenceGateway for PartialFailGateway {
            async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
                let mut n = self.calls.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    Err(anyhow::anyhow!("simulated failure"))
                } else {
                    Ok(end_turn("ok", 5, 3))
                }
            }
            fn model_id(&self) -> &str { "partial" }
        }
        let gw = PartialFailGateway { calls: Arc::new(Mutex::new(0)) };
        let sched = make_scheduler(
            vec![agent_cfg("agent-a", "task a"), agent_cfg("agent-b", "task b")],
            unlimited(),
            gw,
        );
        let outcomes = sched.run().await;
        let failed  = outcomes.values().filter(|r| r.is_err()).count();
        let success = outcomes.values().filter(|r| r.is_ok()).count();
        assert_eq!(failed,  1);
        assert_eq!(success, 1);
    }

    #[test]
    fn scheduler_new_rejects_duplicate_ids() {
        let gw = Arc::new(FailGateway);
        let (rec, _tmp) = recorder();
        let registry = Arc::new(ToolRegistry::new());
        let result = Scheduler::new(
            vec![agent_cfg("dup", "task 1"), agent_cfg("dup", "task 2")],
            &model_cfg(),
            unlimited(),
            gw,
            registry,
            rec,
        );
        assert!(result.err().unwrap().to_string().contains("duplicate agent id"));
    }

    #[tokio::test]
    async fn scheduler_zero_agents_returns_empty() {
        let gw = Arc::new(FailGateway);
        let (rec, _tmp) = recorder();
        let registry = Arc::new(ToolRegistry::new());
        let sched = Scheduler::new(vec![], &model_cfg(), unlimited(), gw, registry, rec).unwrap();
        assert!(sched.run().await.is_empty());
    }

    // ── p1.3: concurrency cap serializes agents ───────────────────────────

    #[tokio::test]
    async fn scheduler_cap1_serializes_two_agents() {
        // With cap=1, only one inference may be in-flight at a time.
        // MockGateway returns responses in FIFO order; both agents should complete.
        let gw = MockGateway::new(vec![
            end_turn("first done",  10, 5),
            end_turn("second done", 10, 5),
        ]);
        let sched = make_scheduler(
            vec![agent_cfg("a", "task a"), agent_cfg("b", "task b")],
            sched_cfg(0, 1),
            gw,
        );
        let outcomes = sched.run().await;
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes["a"].is_ok() || outcomes["b"].is_ok(), "at least one must succeed");
        let total_ok = outcomes.values().filter(|r| r.is_ok()).count();
        assert_eq!(total_ok, 2, "both agents should complete");
    }

    // ── p1.3: global token budget denies when exhausted ───────────────────

    #[tokio::test]
    async fn scheduler_budget_exhausted_denies_second_agent() {
        // Setup: cap=1 forces one of the two agents into the deferred queue at seed
        // time (HashMap iteration order is non-deterministic; either can be admitted).
        // budget=10: the admitted agent gets a response costing 10 tokens (8+2).
        // After completion tokens_spent=10, budget_ok(10) = (10 < 10) = false, so
        // drain_deferred denies the still-deferred agent.
        let gw = MockGateway::new(vec![
            end_turn("winner", 8, 2),  // 10 tokens total
        ]);
        let sched = make_scheduler(
            vec![agent_cfg("a", "task a"), agent_cfg("b", "task b")],
            sched_cfg(10, 1),
            gw,
        );
        let outcomes = sched.run().await;
        assert_eq!(outcomes.len(), 2);
        let ok_count  = outcomes.values().filter(|r| r.is_ok()).count();
        let err_count = outcomes.values().filter(|r| r.is_err()).count();
        assert_eq!(ok_count,  1, "exactly one agent should complete");
        assert_eq!(err_count, 1, "exactly one agent should be denied");
        let denied_err = outcomes.values().find(|r| r.is_err()).unwrap().as_ref().unwrap_err();
        assert!(
            denied_err.to_string().contains("admission denied"),
            "error should mention admission denied, got: {denied_err}",
        );
    }

    // ── p1.3: deferred agent is admitted when slot opens ─────────────────

    #[tokio::test]
    async fn scheduler_deferred_admitted_after_slot_opens() {
        // cap=1: both agents try to infer at seed; one is admitted, the other deferred
        // (HashMap order is non-deterministic). When the admitted agent's inference
        // completes, drain_deferred admits the waiting one.
        let gw = MockGateway::new(vec![
            end_turn("alpha", 5, 5),
            end_turn("beta",  5, 5),
        ]);
        let sched = make_scheduler(
            vec![agent_cfg("alpha", "task a"), agent_cfg("beta", "task b")],
            sched_cfg(0, 1),
            gw,
        );
        let outcomes = sched.run().await;
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes.values().filter(|r| r.is_ok()).count(), 2, "both must complete");
    }

    // ── p1.3: priority ordering in deferred queue ─────────────────────────

    /// Gateway that records the first user-message text of each infer() call,
    /// allowing tests to verify admission order deterministically.
    struct TrackingGateway {
        call_log: Arc<Mutex<Vec<String>>>,
        responses: Mutex<Vec<InferenceResponse>>,
    }
    impl TrackingGateway {
        fn new(responses: Vec<InferenceResponse>) -> (Self, Arc<Mutex<Vec<String>>>) {
            let log = Arc::new(Mutex::new(Vec::new()));
            (Self { call_log: Arc::clone(&log), responses: Mutex::new(responses) }, log)
        }
    }
    #[async_trait::async_trait]
    impl InferenceGateway for TrackingGateway {
        async fn infer(&self, req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
            if let Some(msg) = req.messages.first() {
                if let Some(Block::Text { text }) = msg.blocks.first() {
                    self.call_log.lock().unwrap().push(text.clone());
                }
            }
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(anyhow::anyhow!("TrackingGateway: no more responses"));
            }
            Ok(q.remove(0))
        }
        fn model_id(&self) -> &str { "tracking" }
    }

    #[tokio::test]
    async fn scheduler_priority_high_runs_before_low() {
        // cap=1: exactly one agent seeds (HashMap order is non-deterministic).
        // The other two are deferred simultaneously.
        // Regardless of which agent wins the seed lottery, the deferred pair
        // is always admitted in priority order (higher priority first).
        //
        // Verification: record infer() call order via TrackingGateway.
        // The seed winner is position 0; positions 1 and 2 are the deferred pair.
        // For any permutation, priorities[order[1]] > priorities[order[2]].
        let priorities: std::collections::HashMap<&str, u32> = [
            ("anchor_task", 5),
            ("high_task",  10),
            ("low_task",    0),
        ].iter().cloned().collect();

        let (gw, call_log) = TrackingGateway::new(vec![
            end_turn("r1", 5, 5),
            end_turn("r2", 5, 5),
            end_turn("r3", 5, 5),
        ]);
        let sched = make_scheduler(
            vec![
                agent_cfg_pri("anchor", "anchor_task",  5),
                agent_cfg_pri("high",   "high_task",   10),
                agent_cfg_pri("low",    "low_task",     0),
            ],
            sched_cfg(0, 1),
            gw,
        );
        let outcomes = sched.run().await;
        assert_eq!(outcomes.values().filter(|r| r.is_ok()).count(), 3, "all must complete");

        let order = call_log.lock().unwrap().clone();
        assert_eq!(order.len(), 3, "each agent must call infer exactly once");

        // The two deferred agents (positions 1 and 2) must be in priority order.
        let p1 = priorities[order[1].as_str()];
        let p2 = priorities[order[2].as_str()];
        assert!(
            p1 > p2,
            "deferred agents must be admitted highest-priority-first, \
             but got order {:?} (p1={}, p2={})",
            order, p1, p2,
        );
    }
}
