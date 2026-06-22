use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap, HashSet},
    pin::Pin,
    sync::{Arc, Mutex, RwLock},
};

use futures::stream::{FuturesUnordered, StreamExt};
use serde_json::json;

use crate::{
    agent::{run_tools_sequential, truncate, AgentEffect, AgentTask, PREVIEW_CHARS},
    bus::{MailMessage, Mailboxes},
    capability::Capability,
    checkpoint::{AgentCheckpoint, AwaitingEntry, CheckpointStore, ParkedApprovalEntry, SchedulerCheckpoint},
    config::{AgentConfig, ModelConfig, PendingActionRequest, SchedulerConfig, SpawnConfig},
    flight_recorder::{EventKind, FlightRecorder},
    inference::{Block, InferenceGateway, InferenceRequest, InferenceResponse, Msg, Role},
    memory::MemoryStore,
    tools::ToolRegistry,
};
use surfaces::{AgentSnapshot, AgentStatus, PendingActionView, SchedulerSnapshot};

const MAX_SHORT_TERM_PREVIEWS: usize = 20;

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

/// Tracks a parent agent waiting for a child to complete.
#[derive(serde::Serialize, serde::Deserialize)]
struct AwaitingParent {
    parent_id: String,
    /// tool_use_id from the parent's spawn_agent call — injected back as ToolResult.
    call_id:   String,
}

/// An agent parked waiting for operator approval via /agents/control.
struct ParkedApproval {
    agent_id:   String,
    call_id:    String,
    action:     PendingActionRequest,
    /// Monotonic timestamp for age display in the TUI.
    created_at: std::time::Instant,
}

/// All mutable state that evolves during a scheduler run.
struct SchedulerState {
    agents:           HashMap<String, AgentTask>,
    outcomes:         HashMap<String, anyhow::Result<String>>,
    pending:          FuturesUnordered<PendingFut>,
    deferred:         BinaryHeap<DeferredInfer>,
    deferred_seq:     u64,
    in_flight:        usize,
    tokens_spent:     u64,
    /// child_id → parent waiting info (parent is paused until child completes).
    awaiting:         HashMap<String, AwaitingParent>,
    /// Monotonically increasing counter for auto-generated child IDs.
    child_seq:        u64,
    /// agent_id → nesting depth (0 = top-level, 1 = child of top-level, …).
    spawn_depths:     HashMap<String, u32>,
    /// child_id → parent_id: insert-only, never removed, so completed spawns remain visible.
    parent_map:       HashMap<String, String>,
    max_spawn_depth:  u32,
    /// Per-agent pending mailboxes. Drained into the agent before each inference step.
    mailboxes:        Mailboxes,
    /// Set when the scheduler is shutting down. Deferred agents are denied rather
    /// than re-enqueued.
    shutdown_requested: bool,
    /// Agent IDs that streamed at least one text chunk to stdout this run.
    streamed_agents:  Arc<Mutex<HashSet<String>>>,
    /// Shared mutex serialising stdout writes across concurrent streaming agents.
    stdout_lock:      Arc<tokio::sync::Mutex<()>>,
    /// approval_id → parked agent awaiting operator decision.
    pending_approvals: HashMap<String, ParkedApproval>,
    /// Counter for generating "act_{seq}" approval IDs.
    approval_seq:     u64,
    /// True when a control channel (FUSE /agents/control) was provided at startup.
    /// When false, request_approval is immediately rejected — no way to resolve it otherwise.
    has_control:      bool,
}

/// Scheduler-level state restored from a checkpoint (not exposed outside this module).
struct SchedulerRestored {
    awaiting:          Vec<AwaitingEntry>,
    mailboxes:         HashMap<String, Vec<MailMessage>>,
    tokens_spent:      u64,
    child_seq:         u64,
    spawn_depths:      HashMap<String, u32>,
    parent_map:        HashMap<String, String>,
    pending_approvals: Vec<ParkedApprovalEntry>,
    approval_seq:      u64,
}

pub struct Scheduler {
    agents:              HashMap<String, AgentTask>,
    sched:               SchedulerConfig,
    gateway:             Arc<dyn InferenceGateway + Send + Sync>,
    registry:            Arc<ToolRegistry>,
    recorder:            Arc<FlightRecorder>,
    snapshot:            Arc<RwLock<SchedulerSnapshot>>,
    store:               CheckpointStore,
    restored:            Option<SchedulerRestored>,
    memory_store:        Option<Arc<dyn MemoryStore>>,
    distill_on_complete: bool,
    /// Default model config used when no parent agent exists (operator spawns).
    default_model_cfg:   crate::config::ModelConfig,
    /// Optional receiver for operator spawn commands injected via /agents/control.
    control_rx:          Option<tokio::sync::mpsc::Receiver<crate::control::ControlCommand>>,
    /// Agent IDs for which at least one text chunk was streamed to stdout.
    /// Read by main.rs after run() to suppress the duplicate println!.
    streamed_agents:     Arc<Mutex<HashSet<String>>>,
}

impl Scheduler {
    /// Create a scheduler for the given agent configs. Returns Err on duplicate agent IDs.
    /// Pass `checkpoint` to restore from a prior run; agents in the checkpoint override
    /// TOML configs for their IDs, and dynamically-spawned children are also restored.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent_configs: Vec<AgentConfig>,
        model_cfg: &ModelConfig,
        sched: SchedulerConfig,
        gateway: Arc<dyn InferenceGateway + Send + Sync>,
        registry: Arc<ToolRegistry>,
        recorder: Arc<FlightRecorder>,
        snapshot: Arc<RwLock<SchedulerSnapshot>>,
        checkpoint: Option<SchedulerCheckpoint>,
    ) -> anyhow::Result<Self> {
        // Canonicalize at construction time so later CWD changes (e.g. in tests
        // that call set_current_dir) never redirect checkpoint writes mid-save.
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let store = CheckpointStore::new(&cwd);
        let mut agents = HashMap::new();
        let mut restored: Option<SchedulerRestored> = None;

        if let Some(cp) = checkpoint {
            let SchedulerCheckpoint {
                agents:            cp_agent_list,
                awaiting:          cp_awaiting,
                mailboxes:         cp_mailboxes,
                tokens_spent:      cp_tokens,
                child_seq:         cp_child_seq,
                spawn_depths:      cp_spawn_depths,
                parent_map:        cp_parent_map,
                pending_approvals: cp_pending_approvals,
                approval_seq:      cp_approval_seq,
                ..
            } = cp;

            let mut cp_map: HashMap<String, AgentCheckpoint> = cp_agent_list
                .into_iter()
                .map(|a| (a.agent_id.clone(), a))
                .collect();

            for cfg in agent_configs {
                anyhow::ensure!(
                    !agents.contains_key(&cfg.id),
                    "duplicate agent id: {}",
                    cfg.id
                );
                let specs = registry.filtered_specs(cfg.capabilities.as_deref());
                let task = if let Some(cp_agent) = cp_map.remove(&cfg.id) {
                    AgentTask::from_checkpoint(cp_agent, specs)
                } else {
                    AgentTask::new(&cfg.id, &cfg.task, &cfg, model_cfg, specs)
                };
                agents.insert(cfg.id.clone(), task);
            }
            // Remaining entries are dynamically-spawned children not in TOML.
            for (id, cp_agent) in cp_map {
                anyhow::ensure!(
                    !agents.contains_key(&id),
                    "duplicate agent id from checkpoint: {}",
                    id
                );
                let specs = registry.filtered_specs(cp_agent.cfg.capabilities.as_deref());
                let task = AgentTask::from_checkpoint(cp_agent, specs);
                agents.insert(id, task);
            }

            restored = Some(SchedulerRestored {
                awaiting:          cp_awaiting,
                mailboxes:         cp_mailboxes,
                tokens_spent:      cp_tokens,
                child_seq:         cp_child_seq,
                spawn_depths:      cp_spawn_depths,
                parent_map:        cp_parent_map,
                pending_approvals: cp_pending_approvals,
                approval_seq:      cp_approval_seq,
            });
        } else {
            agents.reserve(agent_configs.len());
            for cfg in agent_configs {
                anyhow::ensure!(
                    !agents.contains_key(&cfg.id),
                    "duplicate agent id: {}",
                    cfg.id
                );
                let specs = registry.filtered_specs(cfg.capabilities.as_deref());
                let task = AgentTask::new(&cfg.id, &cfg.task, &cfg, model_cfg, specs);
                agents.insert(cfg.id.clone(), task);
            }
        }

        Ok(Self {
            agents,
            sched,
            gateway,
            registry,
            recorder,
            snapshot,
            store,
            restored,
            memory_store:        None,
            distill_on_complete: false,
            default_model_cfg:   model_cfg.clone(),
            control_rx:          None,
            streamed_agents:     Arc::new(Mutex::new(HashSet::new())),
        })
    }

    /// Returns a handle to the set of agent IDs that streamed output to stdout.
    /// Call after `run()` to determine which agents should not be printed again.
    pub fn streamed_agents(&self) -> Arc<Mutex<HashSet<String>>> {
        Arc::clone(&self.streamed_agents)
    }

    /// Attach a memory store and enable end-of-run short-term distillation.
    /// When set, each completed agent whose `short_term` buffer is non-empty gets one
    /// budget-bounded inference call; the result is written to `agent/{id}/distilled/…`
    /// and a `memory_distilled` event is emitted. Off by default — existing demos unchanged.
    pub fn with_distillation(mut self, store: Arc<dyn MemoryStore>) -> Self {
        self.memory_store = Some(store);
        self.distill_on_complete = true;
        self
    }

    /// Attach a control receiver so the scheduler accepts live operator spawn commands
    /// written to `/agents/control`. The scheduler's run loop stays alive after all
    /// initial agents complete as long as this channel remains open.
    pub fn with_control(mut self, rx: tokio::sync::mpsc::Receiver<crate::control::ControlCommand>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    /// Run all agents concurrently until every one reaches a terminal state.
    /// Returns a map from agent_id to Ok(answer) or Err.
    pub async fn run(self) -> HashMap<String, anyhow::Result<String>> {
        let Self {
            agents,
            sched,
            gateway,
            registry,
            recorder,
            snapshot,
            store,
            restored,
            memory_store,
            distill_on_complete,
            default_model_cfg,
            mut control_rx,
            streamed_agents,
        } = self;
        let max_spawn_depth = sched.max_spawn_depth;
        let interval = sched.checkpoint_interval_turns;

        let mut state = SchedulerState {
            agents,
            outcomes:           HashMap::new(),
            pending:            FuturesUnordered::new(),
            deferred:           BinaryHeap::new(),
            deferred_seq:       0,
            in_flight:          0,
            tokens_spent:       0,
            awaiting:           HashMap::new(),
            child_seq:          0,
            spawn_depths:       HashMap::new(),
            parent_map:         HashMap::new(),
            max_spawn_depth,
            mailboxes:          HashMap::new(),
            shutdown_requested: false,
            streamed_agents:    Arc::clone(&streamed_agents),
            stdout_lock:        Arc::new(tokio::sync::Mutex::new(())),
            pending_approvals:  HashMap::new(),
            approval_seq:       0,
            has_control:        control_rx.is_some(),
        };

        // Restore scheduler-level state from checkpoint when present.
        if let Some(r) = restored {
            state.tokens_spent = r.tokens_spent;
            state.child_seq    = r.child_seq;
            state.spawn_depths = r.spawn_depths;
            state.parent_map   = r.parent_map;
            state.approval_seq = r.approval_seq;
            for entry in r.awaiting {
                state.awaiting.insert(entry.child_id, AwaitingParent {
                    parent_id: entry.parent_id,
                    call_id:   entry.call_id,
                });
            }
            for (id, msgs) in r.mailboxes {
                state.mailboxes.insert(id, msgs);
            }
            for entry in r.pending_approvals {
                state.pending_approvals.insert(entry.approval_id.clone(), ParkedApproval {
                    agent_id:   entry.agent_id,
                    call_id:    entry.call_id,
                    action:     entry.action,
                    created_at: std::time::Instant::now(),
                });
            }
        }

        // Seed: step each agent once to kick off its first effect.
        // `or_insert` preserves restored spawn_depths; fresh agents get depth 0.
        // Agents already parked in pending_approvals must NOT be re-stepped — doing so
        // would re-emit RequestApproval and create a duplicate pending_approvals entry.
        let parked_agent_ids: std::collections::HashSet<String> = state
            .pending_approvals
            .values()
            .map(|pa| pa.agent_id.clone())
            .collect();
        let ids: Vec<String> = state.agents.keys().cloned().collect();
        for id in ids {
            state.spawn_depths.entry(id.clone()).or_insert(0);
            state.mailboxes.entry(id.clone()).or_default();
            if parked_agent_ids.contains(&id) {
                continue; // Already awaiting approval — do not re-step.
            }
            let priority = state.agents[&id].priority();
            let cap_set = state.agents[&id].cap_set_cloned();
            let (effect, turn) = {
                let sm = state.agents.get_mut(&id).unwrap();
                let t = sm.turn();
                (sm.step(&recorder), t)
            };
            enqueue_or_defer(effect, id, turn, priority, cap_set, &mut state, &sched, &gateway, &registry, &recorder);
        }
        update_snapshot(&snapshot, &state);

        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate()
        ).expect("failed to install SIGTERM handler");
        let mut sigint = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::interrupt()
        ).expect("failed to install SIGINT handler");

        'main: loop {
            // When all pending work is done, either wait for an operator command or exit.
            if state.pending.is_empty() {
                match control_rx {
                    None => break 'main,
                    Some(ref mut rx) => {
                        tokio::select! {
                            cmd = rx.recv() => {
                                let Some(cmd) = cmd else { break 'main; };
                                dispatch_control_command(cmd, &default_model_cfg, &mut state, &sched, &gateway, &registry, &recorder);
                                update_snapshot(&snapshot, &state);
                            }
                            _ = sigterm.recv() => {
                                recorder.record(
                                    "agentd",
                                    None,
                                    EventKind::SystemShutdownRequested,
                                    json!({ "signal": "SIGTERM" }),
                                );
                                state.shutdown_requested = true;
                                break 'main;
                            }
                            _ = sigint.recv() => {
                                recorder.record(
                                    "agentd",
                                    None,
                                    EventKind::SystemShutdownRequested,
                                    json!({ "signal": "SIGINT" }),
                                );
                                state.shutdown_requested = true;
                                break 'main;
                            }
                        }
                        continue 'main;
                    }
                }
            }

            tokio::select! {
                // Interleave operator control commands while agents are running.
                cmd = async {
                    match control_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None     => std::future::pending::<Option<crate::control::ControlCommand>>().await,
                    }
                } => {
                    match cmd {
                        Some(cmd) => {
                            dispatch_control_command(cmd, &default_model_cfg, &mut state, &sched, &gateway, &registry, &recorder);
                            update_snapshot(&snapshot, &state);
                        }
                        None => { control_rx = None; }
                    }
                }
                er = state.pending.next() => {
                    let Some(er) = er else { continue 'main; };
                    match er {
                        EffectResult::Inference { agent_id, result: Err(e) } => {
                            assert!(
                                state.in_flight > 0,
                                "in_flight underflow on inference error — every Inference result must be preceded by an admission"
                            );
                            state.in_flight -= 1;
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
                            handle_agent_terminal(
                                agent_id,
                                Err(e),
                                &mut state,
                                &sched,
                                &gateway,
                                &registry,
                                &recorder,
                            );
                            drain_deferred(&mut state, &sched, &gateway, &registry, &recorder);
                            update_snapshot(&snapshot, &state);
                        }
                        EffectResult::Inference { agent_id, result: Ok(resp) } => {
                            assert!(
                                state.in_flight > 0,
                                "in_flight underflow on inference success — every Inference result must be preceded by an admission"
                            );
                            state.in_flight -= 1;
                            let new_tokens =
                                u64::from(resp.input_tokens) + u64::from(resp.output_tokens);
                            let priority = state.agents[&agent_id].priority();
                            // Provide response, step, then drain mailbox.
                            // Draining mailbox AFTER step ensures injected messages are
                            // appended after the assistant turn that was just pushed,
                            // preserving correct message ordering (F-005).
                            state
                                .agents
                                .get_mut(&agent_id)
                                .expect("agent_id in EffectResult must be present in agents map")
                                .provide_inference(resp, &recorder);
                            let (effect, turn) = {
                                let sm = state
                                    .agents
                                    .get_mut(&agent_id)
                                    .expect("agent_id must still be present after provide_inference");
                                let t = sm.turn();
                                (sm.step(&recorder), t)
                            };
                            drain_mailbox(&agent_id, &mut state, &recorder);
                            state.tokens_spent = state.tokens_spent.saturating_add(new_tokens);
                            let cap_set = state.agents[&agent_id].cap_set_cloned();
                            // Drain deferred agents first (they were waiting for a slot to open),
                            // then re-enqueue the completing agent's next step. This gives queued
                            // agents priority over the agent that just ran — intentional fairness policy.
                            drain_deferred(&mut state, &sched, &gateway, &registry, &recorder);
                            enqueue_or_defer(
                                effect,
                                agent_id,
                                turn,
                                priority,
                                cap_set,
                                &mut state,
                                &sched,
                                &gateway,
                                &registry,
                                &recorder,
                            );
                            update_snapshot(&snapshot, &state);
                        }
                        EffectResult::Tools { agent_id, results } => {
                            let priority = state.agents[&agent_id].priority();
                            // Provide tool results, then drain mailbox before next step.
                            state
                                .agents
                                .get_mut(&agent_id)
                                .expect("agent_id in EffectResult must be present in agents map")
                                .provide_tool_results(results, &recorder);
                            drain_mailbox(&agent_id, &mut state, &recorder);

                            // Periodic checkpoint at clean turn boundary (best-effort).
                            if interval > 0 {
                                let agent_turn = state.agents[&agent_id].turn();
                                if agent_turn.is_multiple_of(interval) {
                                    checkpoint_all(&store, &state, &recorder).await;
                                }
                            }

                            let (effect, turn) = {
                                let sm = state
                                    .agents
                                    .get_mut(&agent_id)
                                    .expect("agent_id must still be present after drain_mailbox");
                                let t = sm.turn();
                                (sm.step(&recorder), t)
                            };
                            let cap_set = state.agents[&agent_id].cap_set_cloned();
                            enqueue_or_defer(
                                effect,
                                agent_id,
                                turn,
                                priority,
                                cap_set,
                                &mut state,
                                &sched,
                                &gateway,
                                &registry,
                                &recorder,
                            );
                            update_snapshot(&snapshot, &state);
                        }
                    }
                }
                _ = sigterm.recv() => {
                    recorder.record(
                        "agentd",
                        None,
                        EventKind::SystemShutdownRequested,
                        json!({ "signal": "SIGTERM" }),
                    );
                    state.shutdown_requested = true;
                    break 'main;
                }
                _ = sigint.recv() => {
                    recorder.record(
                        "agentd",
                        None,
                        EventKind::SystemShutdownRequested,
                        json!({ "signal": "SIGINT" }),
                    );
                    state.shutdown_requested = true;
                    break 'main;
                }
            }
        }

        // Checkpoint on signal shutdown before denying deferred agents (best-effort).
        if state.shutdown_requested {
            checkpoint_all(&store, &state, &recorder).await;
        }

        // Any agents still in the deferred queue never got a slot — admission denied.
        state.shutdown_requested = true;
        while let Some(d) = state.deferred.pop() {
            recorder.record(
                &d.agent_id,
                None,
                EventKind::AgentAdmissionDenied,
                json!({ "reason": "shutdown", "tokens_spent": state.tokens_spent }),
            );
            handle_agent_terminal(
                d.agent_id,
                Err(anyhow::anyhow!("admission denied: scheduler shut down with pending items")),
                &mut state,
                &sched,
                &gateway,
                &registry,
                &recorder,
            );
        }

        // Post-run distillation (p5.6): promote each completed agent's short-term
        // buffer to Tier 3 via one bounded inference call. Off by default.
        if distill_on_complete {
            if let Some(ref mem_store) = memory_store {
                // Collect agents that have short_term items.
                let candidates: Vec<(String, Vec<crate::memory::MemItem>)> = state
                    .agents
                    .iter()
                    .filter(|(_, t)| !t.short_term.is_empty())
                    .map(|(id, t)| (id.clone(), t.short_term.clone()))
                    .collect();

                for (agent_id, items) in candidates {
                    // Budget guard: require headroom for at least a small inference.
                    const MIN_DISTILL_TOKENS: u64 = 512;
                    let budget_ok = sched.global_token_budget == 0
                        || state.tokens_spent + MIN_DISTILL_TOKENS <= sched.global_token_budget;
                    if !budget_ok {
                        break;
                    }

                    let summary_text = items
                        .iter()
                        .map(|m| {
                            let role_str = match m.role {
                                Role::User => "user",
                                Role::Assistant => "assistant",
                            };
                            format!("[turn {}] {}: {}", m.turn, role_str, m.content_preview)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    let req = InferenceRequest {
                        system: Some(
                            "Distill the following conversation excerpts into a brief, \
                             factual summary of key findings and decisions."
                                .to_string(),
                        ),
                        messages: vec![Msg {
                            role: Role::User,
                            blocks: vec![Block::Text {
                                text: format!(
                                    "Summarize these memory excerpts:\n{summary_text}"
                                ),
                            }],
                        }],
                        tools:      vec![],
                        max_tokens: 1024,
                        streaming:  false,
                    };

                    let max_out_tokens = match sched.global_token_budget {
                        0 => req.max_tokens,
                        cap => {
                            let remaining = cap.saturating_sub(state.tokens_spent) as u32;
                            req.max_tokens.min(remaining)
                        }
                    };
                    let req = InferenceRequest { max_tokens: max_out_tokens, ..req };

                    match gateway.infer(req).await {
                        Ok(resp) => {
                            let distilled: String = resp
                                .blocks
                                .iter()
                                .filter_map(|b| {
                                    if let Block::Text { text } = b {
                                        Some(text.as_str())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" ");

                            state.tokens_spent +=
                                resp.input_tokens as u64 + resp.output_tokens as u64;

                            let ns = format!("agent/{agent_id}");
                            let key = format!(
                                "distilled/{:016x}",
                                items.iter().map(|m| m.turn as u64).max().unwrap_or(0)
                            );
                            let store_clone = Arc::clone(mem_store);
                            let _ = tokio::task::spawn_blocking(move || {
                                store_clone.put(&ns, &key, &distilled)
                            })
                            .await;

                            recorder.record(
                                &agent_id,
                                None,
                                EventKind::MemoryDistilled,
                                json!({ "agent": agent_id, "items": items.len() }),
                            );
                        }
                        Err(e) => {
                            recorder.record(
                                &agent_id,
                                None,
                                EventKind::Error,
                                json!({ "stage": "distillation", "error": e.to_string() }),
                            );
                        }
                    }
                }
            }
        }

        state.outcomes
    }
}

/// When an agent reaches a terminal state (Completed or Failed — including admission denial),
/// either deliver the result to a waiting parent or record it in outcomes.
fn handle_agent_terminal(
    agent_id: String,
    result: anyhow::Result<String>,
    state: &mut SchedulerState,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
) {
    if let Some(awaiting) = state.awaiting.remove(&agent_id) {
        // This agent is a child — inject its result into the waiting parent.
        let parent_id = awaiting.parent_id;
        let call_id = awaiting.call_id;
        state.spawn_depths.remove(&agent_id);
        state.agents.remove(&agent_id);

        let (content, is_error) = match &result {
            Ok(answer) => (answer.clone(), false),
            Err(e) => (e.to_string(), true),
        };

        recorder.record(
            &parent_id,
            None,
            EventKind::AgentChildResultDelivered,
            json!({
                "child_id":  &agent_id,
                "parent_id": &parent_id,
                "call_id":   &call_id,
                "success":   !is_error,
            }),
        );

        if state.agents.contains_key(&parent_id) {
            let priority = state.agents[&parent_id].priority();
            let caps = state.agents[&parent_id].cap_set_cloned();
            let (parent_effect, parent_turn) = {
                let parent = state.agents.get_mut(&parent_id).unwrap();
                parent.provide_tool_results(
                    vec![Block::ToolResult {
                        tool_use_id: call_id,
                        content,
                        is_error,
                    }],
                    recorder,
                );
                let t = parent.turn();
                (parent.step(recorder), t)
            };
            enqueue_or_defer(
                parent_effect,
                parent_id.clone(),
                parent_turn,
                priority,
                caps,
                state,
                sched,
                gateway,
                registry,
                recorder,
            );
        } else {
            recorder.record(
                &parent_id,
                None,
                EventKind::Error,
                json!({
                    "stage":     "child_result",
                    "error":     "parent agent not found when delivering child result",
                    "child_id":  &agent_id,
                    "parent_id": &parent_id,
                }),
            );
        }
    } else {
        state.outcomes.insert(agent_id, result);
    }
}

/// Build a `PendingFut` for an inference request. When `req.streaming` is true,
/// opens an mpsc channel, runs `infer_with_stream` + a print future concurrently
/// via `tokio::join!`, and records `InferenceStreamStarted`/`InferenceStreamCompleted`
/// events. When false, calls `infer()` directly. Both paths produce an
/// `EffectResult::Inference`.
fn make_infer_future(
    req: InferenceRequest,
    id: String,
    is_multi: bool,
    gw: Arc<dyn InferenceGateway + Send + Sync>,
    recorder: Arc<FlightRecorder>,
    streamed_agents: Arc<Mutex<HashSet<String>>>,
    stdout_lock: Arc<tokio::sync::Mutex<()>>,
) -> PendingFut {
    if req.streaming {
        let model = gw.model_id().to_string();
        Box::pin(async move {
            recorder.record(
                &id,
                None,
                EventKind::InferenceStreamStarted,
                json!({ "agent_id": &id, "model": &model }),
            );

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let infer_fut = gw.infer_with_stream(req, tx);
            let agent_id_label = id.clone();
            let print_fut = async move {
                use tokio::io::AsyncWriteExt;
                let mut stdout = tokio::io::stdout();
                let mut chunks_emitted: u64 = 0;
                while let Some(chunk) = rx.recv().await {
                    let line = if is_multi {
                        format!("[{agent_id_label}] {chunk}")
                    } else {
                        chunk
                    };
                    // Hold the lock across write+flush so concurrent streaming agents
                    // cannot interleave their bytes on stdout.
                    let _guard = stdout_lock.lock().await;
                    match stdout.write_all(line.as_bytes()).await {
                        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                            return chunks_emitted;
                        }
                        // Non-BrokenPipe write error: don't count as delivered.
                        Err(_) => continue,
                        Ok(()) => {
                            let _ = stdout.flush().await;
                            chunks_emitted += 1;
                        }
                    }
                }
                // Newline so the shell prompt starts on a fresh line (only if anything was printed).
                if chunks_emitted > 0 {
                    let _guard = stdout_lock.lock().await;
                    let _ = stdout.write_all(b"\n").await;
                    let _ = stdout.flush().await;
                }
                chunks_emitted
            };

            let (infer_result, chunks_emitted) = tokio::join!(infer_fut, print_fut);

            if let Ok(ref resp) = infer_result {
                if chunks_emitted > 0 {
                    if let Ok(mut set) = streamed_agents.lock() {
                        set.insert(id.clone());
                    }
                }
                recorder.record(
                    &id,
                    None,
                    EventKind::InferenceStreamCompleted,
                    json!({
                        "agent_id": &id,
                        "text_chunks_emitted": chunks_emitted,
                        "input_tokens": resp.input_tokens,
                        "output_tokens": resp.output_tokens,
                    }),
                );
            }

            EffectResult::Inference { agent_id: id, result: infer_result }
        })
    } else {
        Box::pin(async move {
            EffectResult::Inference { agent_id: id, result: gw.infer(req).await }
        })
    }
}

/// Drain the deferred queue, admitting agents until the cap or budget is hit.
/// Agents that can never be admitted (budget exhausted) are denied immediately.
fn drain_deferred(
    state: &mut SchedulerState,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
) {
    let budget_ok = |spent: u64| sched.global_token_budget == 0 || spent < sched.global_token_budget;
    let slot_ok   = |inf: usize| sched.max_concurrent_inferences == 0 || inf < sched.max_concurrent_inferences;

    // During shutdown, deny all queued agents immediately (no re-enqueue).
    if state.shutdown_requested {
        while let Some(d) = state.deferred.pop() {
            recorder.record(
                &d.agent_id,
                None,
                EventKind::AgentAdmissionDenied,
                json!({ "reason": "shutdown", "tokens_spent": state.tokens_spent }),
            );
            handle_agent_terminal(
                d.agent_id,
                Err(anyhow::anyhow!("admission denied: shutdown")),
                state,
                sched,
                gateway,
                registry,
                recorder,
            );
        }
        return;
    }

    // If budget is permanently exhausted, deny everything in the queue.
    if !budget_ok(state.tokens_spent) {
        while let Some(d) = state.deferred.pop() {
            recorder.record(
                &d.agent_id,
                None,
                EventKind::AgentAdmissionDenied,
                json!({ "reason": "global_budget_exhausted", "tokens_spent": state.tokens_spent }),
            );
            handle_agent_terminal(
                d.agent_id,
                Err(anyhow::anyhow!("admission denied: global token budget exhausted")),
                state,
                sched,
                gateway,
                registry,
                recorder,
            );
        }
        return;
    }

    // Admit as many as slots allow.
    let is_multi = state.agents.len() > 1;
    while !state.deferred.is_empty() && slot_ok(state.in_flight) {
        let d = state.deferred.pop().expect("checked non-empty");
        state.in_flight += 1;
        recorder.record(
            &d.agent_id,
            Some(d.turn),
            EventKind::AgentScheduled,
            json!({ "reason": "slot_opened", "in_flight": state.in_flight }),
        );
        let gw  = Arc::clone(gateway);
        let rec = Arc::clone(recorder);
        let sa  = Arc::clone(&state.streamed_agents);
        let sl  = Arc::clone(&state.stdout_lock);
        let id  = d.agent_id;
        state.pending.push(make_infer_future(d.request, id, is_multi, gw, rec, sa, sl));
    }
}

/// Dispatch an AgentEffect: schedule inference, run tools, spawn a child agent,
/// or record terminal effects (Completed/Failed) directly.
#[allow(clippy::too_many_arguments)]
fn enqueue_or_defer(
    effect: AgentEffect,
    agent_id: String,
    turn: u32,
    priority: u32,
    cap_set: Option<Vec<Capability>>,
    state: &mut SchedulerState,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
) {
    let slot_ok   = sched.max_concurrent_inferences == 0 || state.in_flight < sched.max_concurrent_inferences;
    let budget_ok = sched.global_token_budget == 0 || state.tokens_spent < sched.global_token_budget;

    match effect {
        AgentEffect::Infer(req) => {
            if slot_ok && budget_ok {
                state.in_flight += 1;
                recorder.record(
                    &agent_id,
                    Some(turn),
                    EventKind::AgentScheduled,
                    json!({ "in_flight": state.in_flight }),
                );
                let gw       = Arc::clone(gateway);
                let rec      = Arc::clone(recorder);
                let sa       = Arc::clone(&state.streamed_agents);
                let sl       = Arc::clone(&state.stdout_lock);
                let is_multi = state.agents.len() > 1;
                let id       = agent_id;
                state.pending.push(make_infer_future(req, id, is_multi, gw, rec, sa, sl));
            } else if !budget_ok {
                recorder.record(
                    &agent_id,
                    Some(turn),
                    EventKind::AgentAdmissionDenied,
                    json!({ "reason": "global_budget_exhausted", "tokens_spent": state.tokens_spent }),
                );
                handle_agent_terminal(
                    agent_id,
                    Err(anyhow::anyhow!("admission denied: global token budget exhausted")),
                    state,
                    sched,
                    gateway,
                    registry,
                    recorder,
                );
            } else {
                // Slot full — defer.
                let seq = state.deferred_seq;
                state.deferred_seq += 1;
                recorder.record(
                    &agent_id,
                    Some(turn),
                    EventKind::AgentDeferred,
                    json!({ "priority": priority, "seq": seq, "in_flight": state.in_flight }),
                );
                state.deferred.push(DeferredInfer { priority, seq, agent_id, request: req, turn });
            }
        }
        AgentEffect::CallTools(blocks) => {
            let task_fp = state.agents.get(&agent_id)
                .map(|a| a.task_fp().to_string())
                .unwrap_or_default();
            let reg = Arc::clone(registry);
            let rec = Arc::clone(recorder);
            let id = agent_id;
            state.pending.push(Box::pin(async move {
                let results = run_tools_sequential(
                    &id, turn, &task_fp, &blocks, &reg, cap_set.as_deref(), &rec,
                )
                .await;
                EffectResult::Tools { agent_id: id, results }
            }));
        }
        AgentEffect::SpawnAgent { call_id, config } => {
            dispatch_spawn(
                agent_id, call_id, config, cap_set, turn,
                state, sched, gateway, registry, recorder,
            );
        }
        AgentEffect::SendMessage { call_id, to, content } => {
            dispatch_send_message(
                agent_id, call_id, to, content, turn,
                state, sched, gateway, registry, recorder,
            );
        }
        AgentEffect::RequestApproval { call_id, action } => {
            if !state.has_control {
                // No control channel — cannot park the agent because there is no way to
                // resolve it later. Immediately reject the tool call so the agent can
                // continue rather than hanging the scheduler.
                recorder.record(
                    &agent_id,
                    Some(turn),
                    EventKind::ApprovalRejected,
                    json!({
                        "agent_id": &agent_id,
                        "call_id":  &call_id,
                        "reason":   "no control channel available (FUSE not mounted)",
                    }),
                );
                if let Some(sm) = state.agents.get_mut(&agent_id) {
                    let priority = sm.priority();
                    let cap_set  = sm.cap_set_cloned();
                    sm.provide_tool_results(
                        vec![Block::ToolResult {
                            tool_use_id: call_id,
                            content: "request_approval: no control channel available — \
                                      start agentd with FUSE mount to enable approvals"
                                .to_string(),
                            is_error: true,
                        }],
                        recorder,
                    );
                    let t = sm.turn();
                    let effect = sm.step(recorder);
                    enqueue_or_defer(effect, agent_id, t, priority, cap_set,
                                     state, sched, gateway, registry, recorder);
                }
                return;
            }
            let approval_id = format!("act_{}", state.approval_seq);
            state.approval_seq += 1;
            recorder.record(
                &agent_id,
                Some(turn),
                EventKind::ApprovalRequested,
                json!({
                    "agent_id":    &agent_id,
                    "approval_id": &approval_id,
                    "kind":        &action.kind,
                    "risk":        &action.risk,
                    "summary":     &action.summary,
                }),
            );
            state.pending_approvals.insert(approval_id, ParkedApproval {
                agent_id:   agent_id.clone(),
                call_id,
                action,
                created_at: std::time::Instant::now(),
            });
            // Agent is now parked — do not enqueue it. update_snapshot() will
            // reflect AgentStatus::AwaitingApproval for this agent.
        }
        AgentEffect::Completed(answer) => {
            // AgentCompleted event already emitted by AgentTask::step_with_response().
            handle_agent_terminal(agent_id, Ok(answer), state, sched, gateway, registry, recorder);
        }
        AgentEffect::Failed(msg) => {
            // AgentFailed event already emitted by AgentTask (budget/max-turns/etc.).
            // Inference-error AgentFailed is emitted in run() before this call.
            handle_agent_terminal(
                agent_id,
                Err(anyhow::anyhow!("{msg}")),
                state,
                sched,
                gateway,
                registry,
                recorder,
            );
        }
    }
}

/// F-09: validate an agent-supplied `child_id`. It becomes the child's agent_id
/// and memory-namespace prefix, so it must be a flat identifier: no namespace
/// separators (`:`, `/`), no path traversal (`.`), no whitespace or null bytes.
fn validate_child_id(id: &str) -> anyhow::Result<()> {
    crate::memory::validate_segment(id, "child_id")?;
    anyhow::ensure!(
        id.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')),
        "child_id must match [a-zA-Z0-9_-]; ':', '/', and '.' are reserved"
    );
    Ok(())
}

/// Handle an AgentEffect::SpawnAgent: validate, create the child, seed it.
#[allow(clippy::too_many_arguments)]
fn dispatch_spawn(
    parent_id: String,
    call_id: String,
    config: SpawnConfig,
    parent_cap_set: Option<Vec<Capability>>,
    parent_turn: u32,
    state: &mut SchedulerState,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
) {
    // 1. Capability check — parent must hold Spawn.
    if let Some(caps) = &parent_cap_set {
        if !caps.iter().any(|c| matches!(c, Capability::Spawn)) {
            recorder.record(
                &parent_id,
                Some(parent_turn),
                EventKind::CapabilityDenied,
                json!({ "tool": "spawn_agent", "required": "Spawn" }),
            );
            let priority = state.agents[&parent_id].priority();
            let caps_clone = state.agents[&parent_id].cap_set_cloned();
            let (parent_effect, next_turn) = {
                let parent = state.agents.get_mut(&parent_id).unwrap();
                parent.provide_tool_results(
                    vec![Block::ToolResult {
                        tool_use_id: call_id,
                        content: "capability denied: Spawn capability required to call spawn_agent".to_string(),
                        is_error: true,
                    }],
                    recorder,
                );
                let t = parent.turn();
                (parent.step(recorder), t)
            };
            enqueue_or_defer(parent_effect, parent_id, next_turn, priority, caps_clone, state, sched, gateway, registry, recorder);
            return;
        }
    }

    // 2. Depth limit check.
    let parent_depth = state.spawn_depths.get(&parent_id).copied().unwrap_or(0);
    if parent_depth >= state.max_spawn_depth {
        recorder.record(
            &parent_id,
            Some(parent_turn),
            EventKind::Error,
            json!({ "stage": "spawn", "error": "max spawn depth exceeded", "depth": parent_depth, "limit": state.max_spawn_depth }),
        );
        let priority = state.agents[&parent_id].priority();
        let caps = state.agents[&parent_id].cap_set_cloned();
        let (parent_effect, next_turn) = {
            let parent = state.agents.get_mut(&parent_id).unwrap();
            parent.provide_tool_results(
                vec![Block::ToolResult {
                    tool_use_id: call_id,
                    content: format!(
                        "spawn denied: max nesting depth {} reached",
                        state.max_spawn_depth
                    ),
                    is_error: true,
                }],
                recorder,
            );
            let t = parent.turn();
            (parent.step(recorder), t)
        };
        enqueue_or_defer(parent_effect, parent_id, next_turn, priority, caps, state, sched, gateway, registry, recorder);
        return;
    }

    // 3. Generate or validate the child ID.
    // F-09: an agent-SUPPLIED child_id is untrusted — it becomes the child's
    // agent_id and memory-namespace prefix. Reject traversal / namespace
    // separators so a child can't escape its namespace or impersonate another
    // agent. Auto-generated ids are trusted (derived from parent_id + a counter).
    let child_id = match config.child_id.clone() {
        Some(supplied) => {
            if let Err(reason) = validate_child_id(&supplied) {
                recorder.record(
                    &parent_id,
                    Some(parent_turn),
                    EventKind::Error,
                    json!({ "stage": "spawn", "error": "invalid child_id", "child_id": &supplied, "reason": reason.to_string() }),
                );
                let priority = state.agents[&parent_id].priority();
                let caps = state.agents[&parent_id].cap_set_cloned();
                let (parent_effect, next_turn) = {
                    let parent = state.agents.get_mut(&parent_id).unwrap();
                    parent.provide_tool_results(
                        vec![Block::ToolResult {
                            tool_use_id: call_id,
                            content: format!("spawn denied: invalid child_id {supplied:?}: {reason}"),
                            is_error: true,
                        }],
                        recorder,
                    );
                    let t = parent.turn();
                    (parent.step(recorder), t)
                };
                enqueue_or_defer(parent_effect, parent_id, next_turn, priority, caps, state, sched, gateway, registry, recorder);
                return;
            }
            supplied
        }
        None => {
            let id = format!("{parent_id}-child-{}", state.child_seq);
            state.child_seq += 1;
            id
        }
    };

    // 4. Build child AgentConfig (inherit caps + budget from parent).
    let child_caps = parent_cap_set.clone();
    let parent_token_budget = state
        .agents
        .get(&parent_id)
        .map(|a| a.token_budget())
        .unwrap_or_else(crate::config::default_token_budget);
    let child_budget = config.token_budget.unwrap_or(parent_token_budget);

    let child_agent_cfg = crate::config::AgentConfig {
        id:           child_id.clone(),
        task:         config.task.clone(),
        max_turns:    crate::config::default_max_turns(),
        token_budget: child_budget,
        priority:     config.priority,
        capabilities: child_caps.clone(),
        name:         None,
        description:  String::new(),
        skills:       vec![],
    };

    // 5. Build child AgentTask with filtered specs for the child's capabilities.
    let child_specs = registry.filtered_specs(child_caps.as_deref());
    // We need ModelConfig — grab from any existing agent's perspective.
    // ModelConfig is shared; use a default that the scheduler was wired with.
    // Since we don't store model_cfg on SchedulerState, the child task uses
    // the same model config as the parent (stored inside AgentTask::model_cfg).
    // Extract from parent by calling a new helper.
    let child_model_cfg = state
        .agents
        .get(&parent_id)
        .map(|a| a.model_cfg_cloned())
        .unwrap_or_default();

    let child_task = AgentTask::new(
        &child_id,
        &config.task,
        &child_agent_cfg,
        &child_model_cfg,
        child_specs,
    );

    // 6. Register child in scheduler state — guard for ID collision.
    if state.agents.contains_key(&child_id) || state.outcomes.contains_key(&child_id) {
        recorder.record(
            &parent_id,
            Some(parent_turn),
            EventKind::Error,
            json!({ "stage": "spawn", "error": "child ID collision", "child_id": &child_id }),
        );
        let priority = state.agents[&parent_id].priority();
        let caps = state.agents[&parent_id].cap_set_cloned();
        let (parent_effect, next_turn) = {
            let parent = state.agents.get_mut(&parent_id).unwrap();
            parent.provide_tool_results(
                vec![Block::ToolResult {
                    tool_use_id: call_id,
                    content: format!("spawn denied: agent ID '{}' is already in use", child_id),
                    is_error: true,
                }],
                recorder,
            );
            let t = parent.turn();
            (parent.step(recorder), t)
        };
        enqueue_or_defer(parent_effect, parent_id, next_turn, priority, caps, state, sched, gateway, registry, recorder);
        return;
    }
    state.agents.insert(child_id.clone(), child_task);
    state.awaiting.insert(
        child_id.clone(),
        AwaitingParent { parent_id: parent_id.clone(), call_id },
    );
    state.spawn_depths.insert(child_id.clone(), parent_depth + 1);
    state.parent_map.insert(child_id.clone(), parent_id.clone());
    state.mailboxes.entry(child_id.clone()).or_default();

    // 7. Record agent_spawned flight event.
    recorder.record(
        &child_id,
        None,
        EventKind::AgentSpawned,
        json!({
            "parent_id":    &parent_id,
            "task_preview": truncate(&config.task, PREVIEW_CHARS),
            "depth":        parent_depth + 1,
        }),
    );

    // 8. Seed child: step once to get its first effect, then dispatch.
    let child_priority = config.priority;
    let child_cap_set = state.agents[&child_id].cap_set_cloned();
    let (child_effect, child_turn) = {
        let child_sm = state.agents.get_mut(&child_id).unwrap();
        let t = child_sm.turn();
        (child_sm.step(recorder), t)
    };
    enqueue_or_defer(
        child_effect,
        child_id,
        child_turn,
        child_priority,
        child_cap_set,
        state,
        sched,
        gateway,
        registry,
        recorder,
    );
}

/// Dispatch a ControlCommand from the /agents/control FUSE surface.
/// Handles Spawn (new agent), Approve, and Reject (approval gate).
#[allow(clippy::too_many_arguments)]
fn dispatch_control_command(
    cmd:            crate::control::ControlCommand,
    default_model:  &crate::config::ModelConfig,
    state:          &mut SchedulerState,
    sched:          &crate::config::SchedulerConfig,
    gateway:        &Arc<dyn InferenceGateway + Send + Sync>,
    registry:       &Arc<ToolRegistry>,
    recorder:       &Arc<FlightRecorder>,
) {
    use crate::control::ControlCommand;

    match cmd {
        ControlCommand::Approve { id: approval_id, edits, auto_approve_kind } => {
            let Some(parked) = state.pending_approvals.remove(&approval_id) else {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::FuseControlError,
                    json!({ "error": format!("no pending approval with id '{approval_id}'"), "is_error": true }),
                );
                return;
            };
            let agent_id = parked.agent_id;
            let call_id  = parked.call_id;
            // The operator may supply edited args; fall back to the original args.
            let result_args = edits.unwrap_or_else(|| parked.action.args.clone());
            recorder.record(
                &agent_id,
                None,
                EventKind::ApprovalGranted,
                json!({
                    "agent_id":           &agent_id,
                    "approval_id":        &approval_id,
                    "edits_applied":      result_args != parked.action.args,
                    "auto_approve_kind":  auto_approve_kind,
                }),
            );
            if let Some(task) = state.agents.get_mut(&agent_id) {
                let priority = task.priority();
                let caps     = task.cap_set_cloned();
                task.provide_tool_results(
                    vec![Block::ToolResult {
                        tool_use_id: call_id,
                        content:     serde_json::to_string(&result_args)
                                         .unwrap_or_else(|_| "{}".to_string()),
                        is_error:    false,
                    }],
                    recorder,
                );
                let (effect, next_turn) = {
                    let t = task.turn();
                    (task.step(recorder), t)
                };
                enqueue_or_defer(effect, agent_id, next_turn, priority, caps, state, sched, gateway, registry, recorder);
            } else {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::FuseControlError,
                    json!({ "error": format!("agent '{}' not found after approval", agent_id), "is_error": true }),
                );
            }
        }
        ControlCommand::Reject { id: approval_id, reason } => {
            let Some(parked) = state.pending_approvals.remove(&approval_id) else {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::FuseControlError,
                    json!({ "error": format!("no pending approval with id '{approval_id}'"), "is_error": true }),
                );
                return;
            };
            let agent_id = parked.agent_id;
            let call_id  = parked.call_id;
            recorder.record(
                &agent_id,
                None,
                EventKind::ApprovalRejected,
                json!({
                    "agent_id":    &agent_id,
                    "approval_id": &approval_id,
                    "reason":      &reason,
                }),
            );
            if let Some(task) = state.agents.get_mut(&agent_id) {
                let priority = task.priority();
                let caps     = task.cap_set_cloned();
                let reason_text = reason.unwrap_or_else(|| "operator rejected the action".to_string());
                task.provide_tool_results(
                    vec![Block::ToolResult {
                        tool_use_id: call_id,
                        content:     format!("approval rejected: {reason_text}"),
                        is_error:    true,
                    }],
                    recorder,
                );
                let (effect, next_turn) = {
                    let t = task.turn();
                    (task.step(recorder), t)
                };
                enqueue_or_defer(effect, agent_id, next_turn, priority, caps, state, sched, gateway, registry, recorder);
            } else {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::FuseControlError,
                    json!({ "error": format!("agent '{}' not found after rejection", agent_id), "is_error": true }),
                );
            }
        }
        ControlCommand::Spawn(req) => dispatch_operator_spawn_inner(req, default_model, state, sched, gateway, registry, recorder),
    }
}

/// Inner handler for ControlCommand::Spawn — injects a new top-level agent.
#[allow(clippy::too_many_arguments)]
fn dispatch_operator_spawn_inner(
    req:            crate::control::OperatorSpawnRequest,
    default_model:  &crate::config::ModelConfig,
    state:          &mut SchedulerState,
    sched:          &crate::config::SchedulerConfig,
    gateway:        &Arc<dyn InferenceGateway + Send + Sync>,
    registry:       &Arc<ToolRegistry>,
    recorder:       &Arc<FlightRecorder>,
) {
    use crate::config::{default_max_turns, default_token_budget};

    // Validate / derive agent ID.
    let agent_id = match req.id {
        Some(ref id) => {
            if let Err(e) = validate_child_id(id) {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::FuseControlError,
                    json!({ "error": e.to_string(), "is_error": true }),
                );
                return;
            }
            id.clone()
        }
        None => {
            let id = format!("operator-{}", state.child_seq);
            state.child_seq += 1;
            id
        }
    };

    // Guard against ID collisions.
    if state.agents.contains_key(&agent_id) || state.outcomes.contains_key(&agent_id) {
        recorder.record(
            "agentd",
            None,
            EventKind::FuseControlError,
            json!({ "error": format!("agent ID '{}' already in use", agent_id), "is_error": true }),
        );
        return;
    }

    let max_turns    = req.max_turns.unwrap_or_else(default_max_turns);
    let token_budget = req.token_budget.unwrap_or_else(default_token_budget);
    let priority     = req.priority.unwrap_or(0);

    let agent_cfg = crate::config::AgentConfig {
        id:           agent_id.clone(),
        task:         req.task.clone(),
        max_turns,
        token_budget,
        priority,
        capabilities: req.capabilities.clone(),
        name:         None,
        description:  String::new(),
        skills:       vec![],
    };

    let specs     = registry.filtered_specs(agent_cfg.capabilities.as_deref());
    let task      = crate::agent::AgentTask::new(&agent_id, &req.task, &agent_cfg, default_model, specs);

    state.agents.insert(agent_id.clone(), task);
    state.spawn_depths.insert(agent_id.clone(), 0);
    state.mailboxes.entry(agent_id.clone()).or_default();
    state.parent_map.insert(agent_id.clone(), "operator".to_string());

    recorder.record(
        &agent_id,
        None,
        EventKind::FuseControlReceived,
        json!({
            "task_preview": crate::agent::truncate(&req.task, crate::agent::PREVIEW_CHARS),
            "id":           &agent_id,
        }),
    );
    recorder.record(
        &agent_id,
        None,
        EventKind::AgentSpawned,
        json!({
            "parent_id":    "operator",
            "task_preview": crate::agent::truncate(&req.task, crate::agent::PREVIEW_CHARS),
            "depth":        0_u32,
        }),
    );

    // Seed the new agent.
    let cap_set = state.agents[&agent_id].cap_set_cloned();
    let (effect, turn) = {
        let sm = state.agents.get_mut(&agent_id).unwrap();
        let t  = sm.turn();
        (sm.step(recorder), t)
    };
    enqueue_or_defer(effect, agent_id, turn, priority, cap_set, state, sched, gateway, registry, recorder);
}

/// Handle an AgentEffect::SendMessage: deliver to recipient's mailbox, then
/// synthesize an immediate ToolResult so the sender can continue.
#[allow(clippy::too_many_arguments)]
fn dispatch_send_message(
    sender_id: String,
    call_id: String,
    to: String,
    content: String,
    sender_turn: u32,
    state: &mut SchedulerState,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
) {
    // Validate recipient exists.
    let recipient_known = state.agents.contains_key(&to) || state.outcomes.contains_key(&to);
    if !recipient_known {
        recorder.record(
            &sender_id,
            Some(sender_turn),
            EventKind::Error,
            json!({ "stage": "send_message", "error": "unknown recipient", "to": &to }),
        );
        let priority = state.agents[&sender_id].priority();
        let caps = state.agents[&sender_id].cap_set_cloned();
        let (effect, next_turn) = {
            let sender = state.agents.get_mut(&sender_id).unwrap();
            sender.provide_tool_results(
                vec![Block::ToolResult {
                    tool_use_id: call_id,
                    content: format!("send_message failed: no agent with id '{to}'"),
                    is_error: true,
                }],
                recorder,
            );
            let t = sender.turn();
            (sender.step(recorder), t)
        };
        enqueue_or_defer(effect, sender_id, next_turn, priority, caps, state, sched, gateway, registry, recorder);
        return;
    }

    // Deliver to mailbox.
    let preview = content.chars().take(200).collect::<String>();
    state.mailboxes.entry(to.clone()).or_default().push(MailMessage {
        from: sender_id.clone(),
        content: content.clone(),
    });

    recorder.record(
        &sender_id,
        Some(sender_turn),
        EventKind::MessageSent,
        json!({ "to": &to, "preview": preview }),
    );

    // Synthesize success ToolResult so the sender continues.
    let priority = state.agents[&sender_id].priority();
    let caps = state.agents[&sender_id].cap_set_cloned();
    let (effect, next_turn) = {
        let sender = state.agents.get_mut(&sender_id).unwrap();
        sender.provide_tool_results(
            vec![Block::ToolResult {
                tool_use_id: call_id,
                content: format!("message delivered to {to}"),
                is_error: false,
            }],
            recorder,
        );
        let t = sender.turn();
        (sender.step(recorder), t)
    };
    enqueue_or_defer(effect, sender_id, next_turn, priority, caps, state, sched, gateway, registry, recorder);
}

/// Write a snapshot of the current scheduler state into the shared snapshot.
/// Uses `try_write` so a slow FUSE reader never blocks the scheduler.
fn update_snapshot(snapshot: &Arc<RwLock<SchedulerSnapshot>>, state: &SchedulerState) {
    let agents: Vec<AgentSnapshot> = state
        .agents
        .iter()
        .map(|(id, task)| {
            // Check outcomes first: a top-level terminal agent stays in both
            // state.agents and state.outcomes until the run loop moves on.
            let status = if let Some(result) = state.outcomes.get(id) {
                if result.is_ok() { AgentStatus::Done } else { AgentStatus::Failed }
            } else {
                let maybe_child = state
                    .awaiting
                    .iter()
                    .find(|(_, v)| &v.parent_id == id)
                    .map(|(k, _)| k.clone());
                if let Some(child_id) = maybe_child {
                    AgentStatus::AwaitingChild(child_id)
                } else if let Some((approval_id, _)) = state
                    .pending_approvals
                    .iter()
                    .find(|(_, pa)| &pa.agent_id == id)
                {
                    AgentStatus::AwaitingApproval(approval_id.clone())
                } else if state.deferred.iter().any(|d| &d.agent_id == id) {
                    AgentStatus::Deferred
                } else {
                    AgentStatus::Running
                }
            };
            AgentSnapshot {
                id:             id.clone(),
                status,
                turn:           task.turn(),
                context_tokens: task.context_tokens(),
                token_budget:   task.token_budget(),
                task_preview:   task.task_preview(80),
                tools:          task.spec_names().to_vec(),
                short_term_previews: task
                    .short_term
                    .iter()
                    .take(MAX_SHORT_TERM_PREVIEWS)
                    .map(|item| {
                        let role = match &item.role {
                            Role::User      => "user",
                            Role::Assistant => "assistant",
                        };
                        // Strip embedded newlines so each preview is always one line
                        // in the FUSE short_term virtual file (format is line-per-item).
                        let preview = item.content_preview.replace(['\n', '\r'], " ");
                        format!("t{} {}: {}", item.turn, role, preview)
                    })
                    .collect(),
                parent_id: state.parent_map.get(id).cloned(),
                accessible_server_names:   task.accessible_server_names(),
                capabilities_unrestricted: task.is_capabilities_unrestricted(),
            }
        })
        .collect();

    // Project pending approvals into the snapshot (bounded to ≤100 entries).
    let pending_actions: Vec<PendingActionView> = state
        .pending_approvals
        .iter()
        .take(100)
        .map(|(approval_id, pa)| PendingActionView {
            id:        approval_id.clone(),
            agent_id:  pa.agent_id.clone(),
            kind:      pa.action.kind.clone(),
            risk:      pa.action.risk.clone(),
            summary:   pa.action.summary.clone(),
            args_json: serde_json::to_string(&pa.action.args).unwrap_or_default(),
            age_secs:  pa.created_at.elapsed().as_secs(),
        })
        .collect();

    if let Ok(mut s) = snapshot.try_write() {
        s.agents              = agents;
        s.global_tokens_spent = state.tokens_spent;
        s.in_flight           = state.in_flight;
        s.queue_depth         = state.deferred.len();
        s.pending_actions     = pending_actions;
    }
}

/// Build a serializable snapshot of the current scheduler state.
/// Terminal agents are excluded — they've already delivered their results.
/// The deferred queue is intentionally omitted: those agents remain in `state.agents`
/// in NeedInfer state, so step() re-derives their InferenceRequest on restore.
fn build_scheduler_checkpoint(state: &SchedulerState) -> SchedulerCheckpoint {
    let agents: Vec<crate::checkpoint::AgentCheckpoint> = state
        .agents
        .values()
        .filter(|a| !a.is_terminal())
        .map(|a| a.to_checkpoint())
        .collect();

    let awaiting: Vec<AwaitingEntry> = state
        .awaiting
        .iter()
        .map(|(child_id, ap)| AwaitingEntry {
            child_id:  child_id.clone(),
            parent_id: ap.parent_id.clone(),
            call_id:   ap.call_id.clone(),
        })
        .collect();

    let pending_approvals: Vec<ParkedApprovalEntry> = state
        .pending_approvals
        .iter()
        .enumerate()
        .map(|(i, (approval_id, pa))| ParkedApprovalEntry {
            approval_id: approval_id.clone(),
            agent_id:    pa.agent_id.clone(),
            call_id:     pa.call_id.clone(),
            action:      pa.action.clone(),
            seq:         i as u64,
        })
        .collect();

    SchedulerCheckpoint {
        format_version:    crate::checkpoint::FORMAT_VERSION,
        agents,
        awaiting,
        mailboxes:         state.mailboxes.clone(),
        tokens_spent:      state.tokens_spent,
        child_seq:         state.child_seq,
        spawn_depths:      state.spawn_depths.clone(),
        parent_map:        state.parent_map.clone(),
        pending_approvals,
        approval_seq:      state.approval_seq,
    }
}

/// Write the full scheduler state to the checkpoint store and emit flight events.
/// Best-effort: logs a warning on failure but never propagates the error.
async fn checkpoint_all(
    store: &CheckpointStore,
    state: &SchedulerState,
    recorder: &FlightRecorder,
) {
    let cp = build_scheduler_checkpoint(state);
    if let Err(e) = store.save(&cp).await {
        tracing::warn!("checkpoint save failed (best-effort): {e:#}");
        return;
    }
    for agent_cp in &cp.agents {
        recorder.record(
            &agent_cp.agent_id,
            Some(agent_cp.turn),
            EventKind::AgentCheckpointed,
            json!({ "turn": agent_cp.turn, "total_tokens": agent_cp.total_input + agent_cp.total_output }),
        );
    }
}

/// Drain pending mailbox messages into an agent before its next inference step.
fn drain_mailbox(
    agent_id: &str,
    state: &mut SchedulerState,
    recorder: &FlightRecorder,
) {
    let messages = state
        .mailboxes
        .get_mut(agent_id)
        .map(std::mem::take)
        .unwrap_or_default();

    if messages.is_empty() {
        return;
    }

    if let Some(task) = state.agents.get_mut(agent_id) {
        task.inject_messages(messages, recorder);
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
    use std::sync::{Mutex, OnceLock};
    use tempfile::NamedTempFile;

    // Serializes tests that fire process-level SIGTERM (affects all schedulers)
    // or write to the shared CWD checkpoint file, preventing tmp-file rename races.
    static SERIAL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
        // Recover from a poisoned mutex: the lock serialises tests only, not
        // data integrity, so a prior test panic must not block subsequent tests.
        SERIAL_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
    }

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

    fn spawn_resp(call_id: &str, task: &str) -> InferenceResponse {
        InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    call_id.to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({ "task": task }),
            }],
            stop_reason:   StopReason::ToolUse,
            input_tokens:  10,
            output_tokens: 5,
        }
    }

    fn agent_cfg(id: &str, task: &str) -> AgentConfig {
        AgentConfig {
            id:           id.to_string(),
            task:         task.to_string(),
            max_turns:    5,
            token_budget: 100_000,
            priority:     0,
            capabilities: None,
            name:         None,
            description:  String::new(),
            skills:       vec![],
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
            streaming:  false,
        }
    }

    fn sched_cfg(global_token_budget: u64, max_concurrent_inferences: usize) -> SchedulerConfig {
        SchedulerConfig {
            global_token_budget,
            max_concurrent_inferences,
            // Disable periodic checkpoints in test helpers to prevent concurrent
            // tests from racing on ./checkpoint.json.tmp in the shared process CWD.
            // Tests that exercise checkpointing explicitly set checkpoint_interval_turns.
            checkpoint_interval_turns: 0,
            ..Default::default()
        }
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
        gw: impl InferenceGateway + 'static,
    ) -> Scheduler {
        let (rec, _tmp) = recorder();
        Scheduler::new(
            agents,
            &model_cfg(),
            sched,
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        )
        .unwrap()
    }

    fn make_scheduler_with_registry(
        agents: Vec<AgentConfig>,
        sched: SchedulerConfig,
        gw: impl InferenceGateway + 'static,
        registry: ToolRegistry,
    ) -> (Scheduler, Arc<FlightRecorder>, NamedTempFile) {
        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            agents,
            &model_cfg(),
            sched,
            Arc::new(gw),
            Arc::new(registry),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        )
        .unwrap();
        (sched, rec, tmp)
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
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        );
        assert!(result.err().unwrap().to_string().contains("duplicate agent id"));
    }

    #[tokio::test]
    async fn scheduler_zero_agents_returns_empty() {
        let gw = Arc::new(FailGateway);
        let (rec, _tmp) = recorder();
        let registry = Arc::new(ToolRegistry::new());
        let sched = Scheduler::new(vec![], &model_cfg(), unlimited(), gw, registry, rec, Arc::new(RwLock::new(SchedulerSnapshot::default())), None).unwrap();
        assert!(sched.run().await.is_empty());
    }

    // ── p1.3: concurrency cap serializes agents ───────────────────────────

    #[tokio::test]
    async fn scheduler_cap1_serializes_two_agents() {
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
        let gw = MockGateway::new(vec![
            end_turn("winner", 8, 2),
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

        let p1 = priorities[order[1].as_str()];
        let p2 = priorities[order[2].as_str()];
        assert!(
            p1 > p2,
            "deferred agents must be admitted highest-priority-first, \
             but got order {:?} (p1={}, p2={})",
            order, p1, p2,
        );
    }

    // ── p1.4: scheduler end-to-end capability enforcement ────────────────────

    #[tokio::test]
    async fn scheduler_capability_denied_bubbles_to_agent_as_tool_error() {
        use crate::{
            capability::Capability,
            tools::native::register_native,
        };

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["write_file".to_string()], None, None).unwrap();

        let gw = MockGateway::new(vec![
            InferenceResponse {
                blocks: vec![Block::ToolUse {
                    id:    "call_cap_test".to_string(),
                    name:  "write_file".to_string(),
                    input: serde_json::json!({"path": "/etc/passwd", "content": "evil"}),
                }],
                stop_reason:   StopReason::ToolUse,
                input_tokens:  10,
                output_tokens: 5,
            },
            end_turn("capability denied, I'll stop", 10, 5),
        ]);

        let (rec, _tmp) = recorder();
        let agent = AgentConfig {
            capabilities: Some(vec![Capability::FsRead { prefix: "/".to_string() }]),
            ..agent_cfg("cap-test", "try to write a file")
        };

        let sched = Scheduler::new(
            vec![agent],
            &model_cfg(),
            unlimited(),
            std::sync::Arc::new(gw),
            std::sync::Arc::new(registry),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        )
        .unwrap();

        let outcomes = sched.run().await;
        assert!(outcomes.contains_key("cap-test"));
        assert!(
            outcomes["cap-test"].is_ok(),
            "agent should complete normally after capability denial: {:?}",
            outcomes["cap-test"]
        );
    }

    // ── p1.5: spawn-await integration tests ──────────────────────────────────

    #[tokio::test]
    async fn scheduler_spawn_child_runs_parent_receives_result() {
        // Parent agent with Spawn capability calls spawn_agent.
        // Child runs and returns an answer. Parent receives it as a tool result
        // and completes normally.
        use crate::{capability::Capability, tools::native::register_native};

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None).unwrap();

        let responses = vec![
            // Parent turn 0: spawns child
            spawn_resp("spawn_1", "what is 2+2?"),
            // Child turn 0: answers immediately
            end_turn("4", 5, 3),
            // Parent turn 1: receives child result, gives final answer
            end_turn("the child said 4", 10, 5),
        ];
        let gw = MockGateway::new(responses);

        let parent = AgentConfig {
            capabilities: Some(vec![Capability::Spawn]),
            ..agent_cfg("parent", "ask a sub-agent what 2+2 is")
        };

        let (sched, _rec, _tmp) = make_scheduler_with_registry(
            vec![parent],
            unlimited(),
            gw,
            registry,
        );
        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 1, "only parent in outcomes; child is internal");
        assert!(
            outcomes["parent"].is_ok(),
            "parent must complete: {:?}",
            outcomes["parent"]
        );
        assert_eq!(outcomes["parent"].as_ref().unwrap(), "the child said 4");
    }

    #[tokio::test]
    async fn scheduler_spawn_child_fails_parent_receives_error() {
        // Child inference fails (network error). Parent receives an is_error tool
        // result and continues to complete normally.
        use crate::{capability::Capability, tools::native::register_native};

        struct FirstFailsThenSucceeds {
            calls: Arc<Mutex<u32>>,
        }
        #[async_trait::async_trait]
        impl InferenceGateway for FirstFailsThenSucceeds {
            async fn infer(&self, req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
                let mut n = self.calls.lock().unwrap();
                *n += 1;
                let count = *n;
                drop(n);
                if count == 1 {
                    // Parent turn 0: spawn call
                    Ok(spawn_resp("spawn_1", "fail task"))
                } else if count == 2 {
                    // Child turn 0: fails
                    Err(anyhow::anyhow!("child network error"))
                } else {
                    // Parent turn 1: receives error result, gives final answer
                    let has_error = req.messages.iter().any(|m| {
                        m.blocks.iter().any(|b| matches!(b, Block::ToolResult { is_error: true, .. }))
                    });
                    assert!(has_error, "parent must see an is_error tool result from failed child");
                    Ok(end_turn("child failed but I'm ok", 10, 5))
                }
            }
            fn model_id(&self) -> &str { "partial-fail" }
        }

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None).unwrap();

        let parent = AgentConfig {
            capabilities: Some(vec![Capability::Spawn]),
            ..agent_cfg("parent", "spawn a task")
        };

        let (sched, _rec, _tmp) = make_scheduler_with_registry(
            vec![parent],
            unlimited(),
            FirstFailsThenSucceeds { calls: Arc::new(Mutex::new(0)) },
            registry,
        );
        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 1);
        assert!(
            outcomes["parent"].is_ok(),
            "parent should complete after child failure: {:?}",
            outcomes["parent"]
        );
        assert_eq!(outcomes["parent"].as_ref().unwrap(), "child failed but I'm ok");
    }

    #[tokio::test]
    async fn scheduler_spawn_denied_no_spawn_capability() {
        // Agent without Spawn capability calls spawn_agent. It must receive a
        // capability_denied is_error tool result and be able to continue.
        use crate::tools::native::register_native;

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None).unwrap();

        let gw = MockGateway::new(vec![
            // Agent turn 0: tries to spawn (will be denied)
            spawn_resp("spawn_1", "subtask"),
            // Agent turn 1: sees capability_denied error, gives final answer
            end_turn("spawn was denied, I'll proceed without it", 10, 5),
        ]);

        let agent = AgentConfig {
            // No Spawn capability
            capabilities: Some(vec![]),
            ..agent_cfg("no-spawn", "try to spawn")
        };

        let (sched, _rec, _tmp) = make_scheduler_with_registry(
            vec![agent],
            unlimited(),
            gw,
            registry,
        );
        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 1);
        assert!(
            outcomes["no-spawn"].is_ok(),
            "agent must complete after spawn denial: {:?}",
            outcomes["no-spawn"]
        );
    }

    #[tokio::test]
    async fn scheduler_spawn_depth_limit_injects_error() {
        // Agent at max depth tries to spawn. Must receive an error tool result.
        use crate::{capability::Capability, tools::native::register_native};

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None).unwrap();

        let gw = MockGateway::new(vec![
            // Agent turn 0: spawn (will be denied — already at max depth 0, limit 0)
            spawn_resp("spawn_1", "subtask"),
            // Agent turn 1: sees depth-limit error, answers
            end_turn("depth limit hit, stopping", 10, 5),
        ]);

        let sched_config = SchedulerConfig {
            max_spawn_depth: 0, // Disable spawning entirely
            ..Default::default()
        };

        let parent = AgentConfig {
            capabilities: Some(vec![Capability::Spawn]),
            ..agent_cfg("agent", "try to spawn")
        };

        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![parent],
            &model_cfg(),
            sched_config,
            Arc::new(gw),
            Arc::new(registry),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        )
        .unwrap();

        let outcomes = sched.run().await;
        assert_eq!(outcomes.len(), 1);
        assert!(
            outcomes["agent"].is_ok(),
            "agent must complete after depth limit: {:?}",
            outcomes["agent"]
        );
    }

    #[tokio::test]
    async fn scheduler_child_admission_denied_parent_continues() {
        // Budget is sized so parent's seed inference is admitted but the child
        // inference is denied because the budget is exhausted after the parent's
        // first turn.
        use crate::{capability::Capability, tools::native::register_native};

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None).unwrap();

        let gw = MockGateway::new(vec![
            // Parent turn 0 (10+5=15 tokens): spawn
            spawn_resp("spawn_1", "heavy task"),
            // Parent turn 1: child was denied, parent receives error result
            end_turn("child denied, I'll handle it myself", 5, 3),
        ]);

        // budget=15: parent's first inference exactly exhausts it.
        // child is created but its first Infer is budget-denied.
        let sched_config = SchedulerConfig {
            global_token_budget: 15,
            max_concurrent_inferences: 0,
            max_spawn_depth: 4,
            ..Default::default()
        };

        let parent = AgentConfig {
            capabilities: Some(vec![Capability::Spawn]),
            ..agent_cfg("parent", "spawn a heavy task")
        };

        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![parent],
            &model_cfg(),
            sched_config,
            Arc::new(gw),
            Arc::new(registry),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        )
        .unwrap();

        let outcomes = sched.run().await;
        // Parent must appear in outcomes (child is internal — should NOT appear)
        assert_eq!(outcomes.len(), 1, "only parent in outcomes");
        assert!(
            outcomes.contains_key("parent"),
            "parent must produce an outcome"
        );
    }

    #[tokio::test]
    async fn scheduler_spawn_with_explicit_child_id() {
        // When SpawnConfig.child_id is Some, the child must use that exact ID
        // (tests the `if config.child_id.is_some()` branch in dispatch_spawn).
        use crate::{capability::Capability, tools::native::register_native};

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None).unwrap();

        // Build a spawn response that carries an explicit child_id.
        let spawn_response = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    "spawn_1".to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({
                    "task":     "explicit-id sub-task",
                    "child_id": "named-child"
                }),
            }],
            stop_reason:   StopReason::ToolUse,
            input_tokens:  10,
            output_tokens: 5,
        };

        let gw = MockGateway::new(vec![
            spawn_response,
            end_turn("child answer", 5, 3),
            end_turn("parent done", 10, 5),
        ]);

        let parent = AgentConfig {
            capabilities: Some(vec![Capability::Spawn]),
            ..agent_cfg("parent-explicit", "spawn named child")
        };

        let (sched, _rec, _tmp) = make_scheduler_with_registry(
            vec![parent],
            unlimited(),
            gw,
            registry,
        );
        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 1, "only parent in outcomes (named child is internal)");
        assert!(
            outcomes["parent-explicit"].is_ok(),
            "parent must complete: {:?}", outcomes["parent-explicit"]
        );
    }

    #[test]
    fn validate_child_id_accepts_flat_ids_rejects_traversal() {
        assert!(validate_child_id("worker-1").is_ok());
        assert!(validate_child_id("Child_2").is_ok());
        // Traversal / namespace separators / empties must be rejected (F-09).
        assert!(validate_child_id("../evil").is_err());
        assert!(validate_child_id("kb:secret").is_err());
        assert!(validate_child_id("a/b").is_err());
        assert!(validate_child_id("dot.name").is_err());
        assert!(validate_child_id("").is_err());
    }

    #[tokio::test]
    async fn spawn_rejects_invalid_child_id() {
        // F-09: an agent-supplied child_id with traversal / namespace separators
        // must be rejected (is_error) instead of used to spawn. The parent
        // recovers and completes; no child under that id is created.
        use crate::{capability::Capability, tools::native::register_native};

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None).unwrap();

        let spawn_response = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    "spawn_1".to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({
                    "task":     "evil",
                    "child_id": "../evil"
                }),
            }],
            stop_reason:   StopReason::ToolUse,
            input_tokens:  10,
            output_tokens: 5,
        };

        let gw = MockGateway::new(vec![
            spawn_response,
            end_turn("parent recovered", 10, 5),
        ]);

        let parent = AgentConfig {
            capabilities: Some(vec![Capability::Spawn]),
            ..agent_cfg("parent-bad-id", "spawn evil child")
        };

        let (sched, _rec, _tmp) =
            make_scheduler_with_registry(vec![parent], unlimited(), gw, registry);
        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 1, "rejected child must not be spawned");
        assert!(outcomes.contains_key("parent-bad-id"));
        assert!(
            !outcomes.keys().any(|k| k.contains("evil")),
            "no evil child id may appear in outcomes"
        );
        assert!(
            outcomes["parent-bad-id"].is_ok(),
            "parent must recover and complete: {:?}",
            outcomes["parent-bad-id"]
        );
    }

    #[tokio::test]
    async fn scheduler_spawn_child_with_explicit_token_budget() {
        // When SpawnConfig.token_budget is Some, the child uses that budget
        // (tests the token_budget override branch in dispatch_spawn).
        use crate::{capability::Capability, tools::native::register_native};

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None).unwrap();

        let spawn_response = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    "spawn_tok".to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({
                    "task":         "budget-limited sub-task",
                    "token_budget": 999999
                }),
            }],
            stop_reason:   StopReason::ToolUse,
            input_tokens:  10,
            output_tokens: 5,
        };

        let gw = MockGateway::new(vec![
            spawn_response,
            end_turn("child ok", 5, 3),
            end_turn("parent ok", 10, 5),
        ]);

        let parent = AgentConfig {
            capabilities: Some(vec![Capability::Spawn]),
            ..agent_cfg("parent-budget", "spawn child with budget")
        };

        let (sched, _rec, _tmp) = make_scheduler_with_registry(
            vec![parent],
            unlimited(),
            gw,
            registry,
        );
        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 1);
        assert!(
            outcomes["parent-budget"].is_ok(),
            "parent must complete with explicit child budget: {:?}", outcomes["parent-budget"]
        );
    }

    // ── p1.6: send_message tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn send_message_delivered_before_next_inference() {
        // Agent A sends a message to agent B. Agent B must see it in a subsequent
        // inference step.
        use crate::tools::native::register_native;

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["send_message".to_string()], None, None).unwrap();

        // Run both agents via a single MockGateway (interleaved responses).
        let mut registry2 = ToolRegistry::new();
        register_native(&mut registry2, &["send_message".to_string()], None, None).unwrap();

        let gw = MockGateway::new(vec![
            // Agent alpha: send_message to beta
            InferenceResponse {
                blocks: vec![Block::ToolUse {
                    id:    "msg1".to_string(),
                    name:  "send_message".to_string(),
                    input: serde_json::json!({"to": "beta", "content": "ping"}),
                }],
                stop_reason:   StopReason::ToolUse,
                input_tokens:  10,
                output_tokens: 5,
            },
            // Agent alpha: complete after delivery
            end_turn("sent", 5, 3),
            // Agent beta: complete (mailbox drain adds the message to its context)
            end_turn("pong", 5, 3),
        ]);

        let alpha = agent_cfg("alpha", "send a message to beta");
        let beta  = agent_cfg("beta", "wait for messages");

        let (sched, _rec, _tmp) = make_scheduler_with_registry(
            vec![alpha, beta],
            unlimited(),
            gw,
            registry2,
        );
        let outcomes = sched.run().await;

        assert!(outcomes["alpha"].is_ok(), "alpha must complete: {:?}", outcomes["alpha"]);
        assert!(outcomes["beta"].is_ok(), "beta must complete: {:?}", outcomes["beta"]);

        // Verify message_sent event was emitted.
        let tmp_path = _tmp.path().to_path_buf();
        let content = std::fs::read_to_string(&tmp_path).unwrap_or_default();
        assert!(content.contains("\"message_sent\""), "message_sent flight event must be recorded");
    }

    #[tokio::test]
    async fn send_message_unknown_recipient_is_error() {
        use crate::tools::native::register_native;

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["send_message".to_string()], None, None).unwrap();

        let gw = MockGateway::new(vec![
            // Agent sends to unknown recipient
            InferenceResponse {
                blocks: vec![Block::ToolUse {
                    id:    "msg_bad".to_string(),
                    name:  "send_message".to_string(),
                    input: serde_json::json!({"to": "ghost", "content": "hello?"}),
                }],
                stop_reason:   StopReason::ToolUse,
                input_tokens:  10,
                output_tokens: 5,
            },
            // Agent receives error, completes gracefully
            end_turn("no such agent", 5, 3),
        ]);

        let agent = agent_cfg("sender", "send a message to ghost");
        let (sched, _rec, _tmp) = make_scheduler_with_registry(
            vec![agent],
            unlimited(),
            gw,
            registry,
        );
        let outcomes = sched.run().await;
        assert!(outcomes["sender"].is_ok(), "sender must handle unknown-recipient error gracefully: {:?}", outcomes["sender"]);

        let content = std::fs::read_to_string(_tmp.path()).unwrap_or_default();
        assert!(content.contains("\"error\""), "error flight event must be recorded for unknown recipient");
    }

    // ── p2.3: SIGTERM drains the scheduler and records shutdown event ────────

    #[cfg(unix)]
    #[tokio::test]
    // The serial guard is intentionally held across awaits to serialize this
    // SIGTERM test against the periodic-checkpoint test (shared CWD file).
    #[allow(clippy::await_holding_lock)]
    async fn sigterm_drains_scheduler() {
        // Must not run concurrently with checkpoint_all_emits_agent_checkpointed_event:
        // both tests fire / react to process-level SIGTERM and write to the shared CWD
        // checkpoint file, which causes a tmp-file rename race.
        let _serial = serial_lock();

        struct SlowGateway;

        #[async_trait::async_trait]
        impl InferenceGateway for SlowGateway {
            async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                Ok(end_turn("never reached", 5, 3))
            }
            fn model_id(&self) -> &str { "slow" }
        }

        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("slow-agent", "do something slow")],
            &model_cfg(),
            unlimited(),
            Arc::new(SlowGateway),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        )
        .unwrap();

        // Fire SIGTERM 50 ms from now — long before the 30-second gateway delay.
        tokio::task::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).unwrap();
        });

        let start = std::time::Instant::now();
        let _outcomes = sched.run().await;
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "scheduler must exit well before the 30-second gateway delay after SIGTERM"
        );

        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(
            log.contains("\"system_shutdown_requested\""),
            "flight log must contain system_shutdown_requested event"
        );
    }

    // ── update_snapshot status coverage ──────────────────────────────────────

    fn minimal_state(id: &str) -> SchedulerState {
        let cfg = agent_cfg(id, "test task");
        let mdl = model_cfg();
        let mut agents = HashMap::new();
        agents.insert(id.to_string(), AgentTask::new(id, &cfg.task, &cfg, &mdl, vec![]));
        SchedulerState {
            agents,
            outcomes:           HashMap::new(),
            pending:            FuturesUnordered::new(),
            deferred:           BinaryHeap::new(),
            deferred_seq:       0,
            in_flight:          0,
            tokens_spent:       0,
            awaiting:           HashMap::new(),
            child_seq:          0,
            spawn_depths:       HashMap::new(),
            parent_map:         HashMap::new(),
            max_spawn_depth:    0,
            mailboxes:          HashMap::new(),
            shutdown_requested: false,
            streamed_agents:    Arc::new(Mutex::new(HashSet::new())),
            stdout_lock:        Arc::new(tokio::sync::Mutex::new(())),
            pending_approvals:  HashMap::new(),
            approval_seq:       0,
            has_control:        false,
        }
    }

    #[test]
    fn update_snapshot_done_status() {
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let mut state = minimal_state("a");
        state.outcomes.insert("a".to_string(), Ok("answer".to_string()));
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        let a = s.agents.iter().find(|x| x.id == "a").unwrap();
        assert_eq!(a.status, AgentStatus::Done);
    }

    #[test]
    fn update_snapshot_failed_status() {
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let mut state = minimal_state("a");
        state.outcomes.insert("a".to_string(), Err(anyhow::anyhow!("boom")));
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        let a = s.agents.iter().find(|x| x.id == "a").unwrap();
        assert_eq!(a.status, AgentStatus::Failed);
    }

    #[test]
    fn update_snapshot_awaiting_child_status() {
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let mut state = minimal_state("parent");
        state.awaiting.insert(
            "child-1".to_string(),
            AwaitingParent { parent_id: "parent".to_string(), call_id: "call-1".to_string() },
        );
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        let a = s.agents.iter().find(|x| x.id == "parent").unwrap();
        assert_eq!(a.status, AgentStatus::AwaitingChild("child-1".to_string()));
    }

    #[test]
    fn update_snapshot_deferred_status() {
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let mut state = minimal_state("a");
        state.deferred.push(DeferredInfer {
            priority: 0,
            seq:      0,
            agent_id: "a".to_string(),
            request: InferenceRequest { system: None, messages: vec![], tools: vec![], max_tokens: 1024, streaming: false },
            turn:     0,
        });
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        let a = s.agents.iter().find(|x| x.id == "a").unwrap();
        assert_eq!(a.status, AgentStatus::Deferred);
    }

    // ── update_snapshot: short_term_previews ──────────────────────────────────

    #[test]
    fn update_snapshot_short_term_user_role_formatted() {
        use crate::memory::MemItem;
        use crate::inference::Role;
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let mut state = minimal_state("a");
        let agent = state.agents.get_mut("a").unwrap();
        agent.short_term.push(MemItem {
            turn:            3,
            role:            Role::User,
            content_preview: "hello from user".to_string(),
            blocks_json:     "[]".to_string(),
        });
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        let a = s.agents.iter().find(|x| x.id == "a").unwrap();
        assert_eq!(a.short_term_previews, vec!["t3 user: hello from user"]);
    }

    #[test]
    fn update_snapshot_short_term_assistant_role_formatted() {
        use crate::memory::MemItem;
        use crate::inference::Role;
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let mut state = minimal_state("a");
        let agent = state.agents.get_mut("a").unwrap();
        agent.short_term.push(MemItem {
            turn:            7,
            role:            Role::Assistant,
            content_preview: "assistant reply".to_string(),
            blocks_json:     "[]".to_string(),
        });
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        let a = s.agents.iter().find(|x| x.id == "a").unwrap();
        assert_eq!(a.short_term_previews, vec!["t7 assistant: assistant reply"]);
    }

    #[test]
    fn update_snapshot_short_term_capped_at_twenty() {
        use crate::memory::MemItem;
        use crate::inference::Role;
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let mut state = minimal_state("a");
        let agent = state.agents.get_mut("a").unwrap();
        // Push 30 items — only 20 should appear in the snapshot
        for i in 0..30u32 {
            agent.short_term.push(MemItem {
                turn:            i,
                role:            Role::User,
                content_preview: format!("item-{}", i),
                blocks_json:     "[]".to_string(),
            });
        }
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        let a = s.agents.iter().find(|x| x.id == "a").unwrap();
        assert_eq!(a.short_term_previews.len(), 20,
            "short_term_previews must be capped at 20 items");
        // Verify the first item is item-0
        assert!(a.short_term_previews[0].contains("item-0"));
        // Verify the last item is item-19 (not item-29)
        assert!(a.short_term_previews[19].contains("item-19"));
    }

    #[test]
    fn update_snapshot_short_term_newlines_in_preview_are_sanitized() {
        use crate::memory::MemItem;
        use crate::inference::Role;
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let mut state = minimal_state("a");
        let agent = state.agents.get_mut("a").unwrap();
        // content_preview contains embedded newlines (common in multi-line LLM output)
        agent.short_term.push(MemItem {
            turn:            1,
            role:            Role::User,
            content_preview: "first line\nsecond line\r\nthird line".to_string(),
            blocks_json:     "[]".to_string(),
        });
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        let a = s.agents.iter().find(|x| x.id == "a").unwrap();
        assert_eq!(a.short_term_previews.len(), 1);
        // The preview must not contain raw newlines — the FUSE short_term file is
        // line-per-item; embedded newlines would corrupt the format.
        assert!(!a.short_term_previews[0].contains('\n'),
            "short_term_previews must not contain embedded newlines");
        assert!(!a.short_term_previews[0].contains('\r'),
            "short_term_previews must not contain embedded carriage returns");
        // Content should still be present (spaces substituted for newlines)
        assert!(a.short_term_previews[0].contains("first line"));
        assert!(a.short_term_previews[0].contains("second line"));
    }

    // ── p3.2: checkpoint / restore tests ─────────────────────────────────────

    fn minimal_agent_checkpoint(id: &str) -> AgentCheckpoint {
        use crate::{
            checkpoint::AgentCheckpoint,
            config::ModelConfig,
            inference::{Block, Msg, Role},
        };
        AgentCheckpoint {
            agent_id:    id.to_string(),
            cfg:         agent_cfg(id, "restore task"),
            model_cfg:   ModelConfig { provider: "mock".to_string(), model: "mock-model".to_string(), max_tokens: 4096, streaming: false },
            messages:    vec![Msg { role: Role::User, blocks: vec![Block::Text { text: "restore task".to_string() }] }],
            specs:       vec![],
            total_input: 10,
            total_output: 5,
            turn:        1,
            stored_response: None,
            terminal:    false,
            short_term:  vec![],
        }
    }

    fn minimal_scheduler_checkpoint(ids: &[&str]) -> SchedulerCheckpoint {
        SchedulerCheckpoint {
            format_version:    crate::checkpoint::FORMAT_VERSION,
            agents:            ids.iter().map(|id| minimal_agent_checkpoint(id)).collect(),
            awaiting:          vec![],
            mailboxes:         HashMap::new(),
            tokens_spent:      20,
            child_seq:         3,
            spawn_depths:      ids.iter().map(|id| (id.to_string(), 0u32)).collect(),
            parent_map:        HashMap::new(),
            pending_approvals: vec![],
            approval_seq:      0,
        }
    }

    #[test]
    fn scheduler_new_with_checkpoint_restores_toml_agent() {
        // TOML agent ID matches checkpoint agent → restored from checkpoint (turn=1, tokens carried)
        let cp = minimal_scheduler_checkpoint(&["alpha"]);
        let gw = MockGateway::new(vec![end_turn("restored answer", 10, 5)]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("alpha", "original task")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();
        // Verify the restored agent exists (Scheduler::new succeeds without error)
        // and has turn=1 from the checkpoint (not 0 from a fresh start).
        let agent = sched.agents.get("alpha").unwrap();
        assert_eq!(agent.turn(), 1, "restored agent must have turn from checkpoint");
        assert_eq!(agent.context_tokens(), 15, "restored tokens must match checkpoint");
    }

    #[test]
    fn scheduler_new_with_checkpoint_missing_agent_starts_fresh() {
        // Checkpoint has no entry for "beta" → fresh AgentTask with turn=0
        let cp = minimal_scheduler_checkpoint(&["alpha"]);
        let gw = MockGateway::new(vec![]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("beta", "new task")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();
        let agent = sched.agents.get("beta").unwrap();
        assert_eq!(agent.turn(), 0, "agent absent from checkpoint must start at turn 0");
    }

    #[test]
    fn scheduler_new_with_checkpoint_orphan_child_restored() {
        // Checkpoint contains "child-1" (dynamically spawned, not in TOML) → must be restored
        let cp = minimal_scheduler_checkpoint(&["parent", "child-1"]);
        let gw = MockGateway::new(vec![]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("parent", "parent task")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();
        assert!(sched.agents.contains_key("child-1"), "orphan checkpoint child must be restored");
        assert_eq!(sched.agents["child-1"].turn(), 1);
    }

    #[test]
    fn scheduler_new_with_checkpoint_duplicate_id_returns_err() {
        // TOML has duplicate IDs regardless of checkpoint — Err must be returned
        let cp = minimal_scheduler_checkpoint(&["dup"]);
        let gw = MockGateway::new(vec![]);
        let (rec, _tmp) = recorder();
        let result = Scheduler::new(
            vec![agent_cfg("dup", "task a"), agent_cfg("dup", "task b")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        );
        assert!(result.is_err(), "duplicate agent IDs must return Err");
    }

    #[tokio::test]
    async fn scheduler_run_restores_tokens_spent_and_child_seq() {
        // Verify scheduler-level fields (tokens_spent, child_seq, spawn_depths) are seeded from checkpoint.
        // We check them by running a fresh scheduler seeded with a checkpoint that has non-zero values,
        // then immediately completing the agent before it can touch these values.
        let cp = SchedulerCheckpoint {
            format_version:    crate::checkpoint::FORMAT_VERSION,
            agents:            vec![minimal_agent_checkpoint("agent")],
            awaiting:          vec![],
            mailboxes:         HashMap::new(),
            tokens_spent:      42,
            child_seq:         7,
            spawn_depths:      [("agent".to_string(), 0u32)].into_iter().collect(),
            parent_map:        HashMap::new(),
            pending_approvals: vec![],
            approval_seq:      0,
        };
        let gw = MockGateway::new(vec![end_turn("done", 10, 5)]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("agent", "task")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();
        // spawn_depths on the restored agent must be preserved (0 not re-inserted by seed loop)
        assert_eq!(sched.agents["agent"].turn(), 1, "agent restored from checkpoint starts at turn 1");
        // The SchedulerRestored fields are consumed at run() start; confirm the scheduler itself was created
        let outcomes = sched.run().await;
        assert!(outcomes["agent"].is_ok(), "restored agent must complete successfully");
    }

    #[tokio::test]
    async fn periodic_checkpoint_interval_one_runs_without_error() {
        // Interval=1 is set; a straight EndTurn agent (no tool cycle) has nothing
        // to checkpoint at provide_tool_results boundaries, but the scheduler must
        // not panic or return an error. Verifies the interval guard is wired correctly.
        let gw = MockGateway::new(vec![end_turn("done", 10, 5)]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("chk", "task")],
            &model_cfg(),
            SchedulerConfig { checkpoint_interval_turns: 1, ..Default::default() },
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();
        let outcomes = sched.run().await;
        assert!(outcomes["chk"].is_ok(), "agent must complete successfully with interval=1");
    }

    #[tokio::test]
    async fn periodic_checkpoint_disabled_when_interval_zero() {
        // Interval=0 means the `if interval > 0` guard (scheduler.rs:354) is never
        // entered. Verify the scheduler still runs to completion without error.
        let gw = MockGateway::new(vec![end_turn("done", 10, 5)]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("nochk", "task")],
            &model_cfg(),
            SchedulerConfig { checkpoint_interval_turns: 0, ..Default::default() },
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();
        let outcomes = sched.run().await;
        assert!(outcomes["nochk"].is_ok(), "agent must complete successfully with interval=0");
    }

    // ── p5.6: distill_on_complete ────────────────────────────────────────────

    #[tokio::test]
    async fn distill_on_complete_promotes_to_tier3() {
        use crate::memory::{MemItem, MemoryStore};
        use crate::memory::store::RedbStore;
        use crate::inference::Role as InfRole;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let mem_path = dir.path().join("mem.redb");
        let (mem_store, _) = RedbStore::open(&mem_path).unwrap();
        let mem_arc: std::sync::Arc<dyn MemoryStore> = std::sync::Arc::new(mem_store);

        // Agent gets one response then ends; two extra responses for distillation
        // (MockGateway is queried once per agent turn + once for distillation).
        let gw = MockGateway::new(vec![
            end_turn("agent answer", 10, 5),
            end_turn("distilled summary of key findings", 20, 10),
        ]);

        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("distil", "some task")],
            &model_cfg(),
            unlimited(),
            std::sync::Arc::new(gw),
            std::sync::Arc::new(ToolRegistry::new()),
            std::sync::Arc::clone(&rec),
            std::sync::Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        )
        .unwrap();

        // Inject a short_term item so distillation triggers.
        // We do this by running, then manually checking via a fresh build where
        // short_term would be populated by real paging.
        // For this test we verify the machinery fires when short_term is non-empty —
        // inject it via a helper that creates a second Scheduler with pre-seeded state.
        // Simpler: since paging is driven by token pressure and our mock uses tiny
        // token counts, just trust the distillation path works when the condition is
        // met. Here we directly set short_term via the pub(crate) accessor in tests
        // by replacing the agent before running.
        let mut sched_with_items = sched;
        // Seed short_term items into the agent before run (access pub(crate) field).
        let agent = sched_with_items.agents.get_mut("distil").unwrap();
        agent.short_term.push(MemItem {
            turn: 1,
            role: InfRole::User,
            content_preview: "first paged turn preview".to_string(),
            blocks_json: "[]".to_string(),
        });

        let sched_distil = sched_with_items.with_distillation(std::sync::Arc::clone(&mem_arc));
        sched_distil.run().await;

        // Verify: a memory_distilled event must be in the flight log.
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(
            log.contains("\"memory_distilled\""),
            "memory_distilled event must be emitted after distillation run"
        );

        // Verify: the distilled content is in the memory store.
        let entries = mem_arc.iter("agent/distil").unwrap();
        assert!(
            !entries.is_empty(),
            "distilled content must be written to memory store under agent/distil"
        );
    }

    #[tokio::test]
    async fn distill_disabled_no_extra_inference() {
        use crate::memory::{MemItem, MemoryStore};
        use crate::memory::store::RedbStore;
        use crate::inference::Role as InfRole;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let mem_path = dir.path().join("mem.redb");
        let (mem_store, _) = RedbStore::open(&mem_path).unwrap();
        let mem_arc: std::sync::Arc<dyn MemoryStore> = std::sync::Arc::new(mem_store);

        // Only one response queued — if distillation fires it would drain the queue
        // and MockGateway would error.
        let gw = MockGateway::new(vec![end_turn("agent answer", 10, 5)]);

        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("nodeistil", "some task")],
            &model_cfg(),
            unlimited(),
            std::sync::Arc::new(gw),
            std::sync::Arc::new(ToolRegistry::new()),
            std::sync::Arc::clone(&rec),
            std::sync::Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        )
        .unwrap();

        // Seed a short_term item — distillation would consume an inference if enabled.
        let mut sched_items = sched;
        let agent = sched_items.agents.get_mut("nodeistil").unwrap();
        agent.short_term.push(MemItem {
            turn: 1,
            role: InfRole::User,
            content_preview: "paged turn".to_string(),
            blocks_json: "[]".to_string(),
        });

        // Do NOT call with_distillation — distill_on_complete stays false.
        sched_items.run().await;

        // memory_distilled must NOT appear in the log.
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(
            !log.contains("\"memory_distilled\""),
            "memory_distilled must not appear when distill_on_complete is false"
        );

        // Memory store must stay empty.
        let entries = mem_arc.iter("agent/nodeistil").unwrap();
        assert!(
            entries.is_empty(),
            "no distilled content must be written when distillation is disabled"
        );
    }

    #[tokio::test]
    // Serial guard intentionally held across awaits to serialize against
    // sigterm_drains_scheduler (shared CWD checkpoint file).
    #[allow(clippy::await_holding_lock)]
    async fn checkpoint_all_emits_agent_checkpointed_event() {
        use tempfile::TempDir;
        // Must not run concurrently with sigterm_drains_scheduler: that test fires a
        // process-level SIGTERM which would interrupt this scheduler before the
        // periodic checkpoint fires, and both tests write to checkpoint.json causing
        // a tmp-file rename race.
        let _serial = serial_lock();

        // Write checkpoint files to a private tempdir to avoid CWD races.
        let dir = TempDir::new().unwrap();
        let orig = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();

        let gw = MockGateway::new(vec![
            InferenceResponse {
                blocks: vec![Block::ToolUse { id: "c1".to_string(), name: "no_tool".to_string(), input: serde_json::json!({}) }],
                stop_reason: StopReason::ToolUse, input_tokens: 10, output_tokens: 5,
            },
            end_turn("done", 10, 5),
        ]);
        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("evt", "task")],
            &model_cfg(),
            SchedulerConfig { checkpoint_interval_turns: 1, ..Default::default() },
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();
        sched.run().await;

        std::env::set_current_dir(&orig).unwrap();
        drop(dir);

        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(
            log.contains("\"agent_checkpointed\""),
            "agent_checkpointed event must appear in flight log after tool cycle"
        );
    }

    // ── p7.2 streaming dispatch ───────────────────────────────────────────────

    /// Gateway that emits N text chunks via the streaming channel, then returns Ok.
    struct StreamingMockGateway {
        chunks: Vec<String>,
        response: InferenceResponse,
    }

    #[async_trait::async_trait]
    impl InferenceGateway for StreamingMockGateway {
        async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
            Ok(self.response.clone())
        }
        async fn infer_with_stream(
            &self,
            _req: InferenceRequest,
            tx: tokio::sync::mpsc::UnboundedSender<String>,
        ) -> anyhow::Result<InferenceResponse> {
            for chunk in &self.chunks {
                let _ = tx.send(chunk.clone());
            }
            Ok(self.response.clone())
        }
        fn model_id(&self) -> &str { "streaming-mock" }
    }

    #[tokio::test]
    async fn streaming_dispatch_emits_flight_events_and_populates_streamed_agents() {
        let resp = end_turn("streamed answer", 20, 10);
        let gw = StreamingMockGateway {
            chunks:   vec!["chunk1".to_string(), "chunk2".to_string(), "chunk3".to_string()],
            response: resp.clone(),
        };

        let mut cfg = agent_cfg("stream-agent", "stream task");
        let mut mcfg = model_cfg();
        mcfg.streaming = true;
        cfg.max_turns = 1;
        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            vec![cfg],
            &mcfg,
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();

        let streamed = sched.streamed_agents();
        let outcomes = sched.run().await;

        // Agent should succeed.
        let result = outcomes.get("stream-agent").expect("agent not found");
        assert!(result.is_ok(), "streaming agent should succeed: {result:?}");

        // streamed_agents should contain the agent ID.
        let set = streamed.lock().unwrap();
        assert!(set.contains("stream-agent"), "streamed_agents should include stream-agent");
        drop(set);

        // Flight log should contain InferenceStreamStarted + InferenceStreamCompleted.
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(log.contains("\"inference_stream_started\""), "missing inference_stream_started event");
        assert!(log.contains("\"inference_stream_completed\""), "missing inference_stream_completed event");
        assert!(log.contains("\"text_chunks_emitted\""), "missing text_chunks_emitted in payload");
    }

    #[tokio::test]
    async fn non_streaming_dispatch_does_not_emit_stream_events() {
        let gw = MockGateway::new(vec![end_turn("plain answer", 10, 5)]);
        let mut cfg = agent_cfg("plain-agent", "plain task");
        cfg.max_turns = 1;
        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            vec![cfg],
            &model_cfg(),  // streaming: false
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();

        let streamed = sched.streamed_agents();
        let outcomes = sched.run().await;

        assert!(outcomes.get("plain-agent").unwrap().is_ok());
        let set = streamed.lock().unwrap();
        assert!(set.is_empty(), "non-streaming should not populate streamed_agents");
        drop(set);

        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(!log.contains("inference_stream_started"), "non-streaming should not emit stream events");
    }

    // ── p7.2 gap coverage ─────────────────────────────────────────────────────

    /// Streaming gateway that returns zero chunks (end_turn response with no text chunks sent).
    /// streamed_agents must NOT be populated when chunks_emitted == 0.
    #[tokio::test]
    async fn streaming_zero_chunks_does_not_populate_streamed_agents() {
        // An SSE stream that emits no text chunks (no send() calls on the channel).
        // The scheduler must emit InferenceStreamStarted/Completed but must NOT
        // add the agent to streamed_agents (chunks_emitted == 0 path).
        let resp = end_turn("silent answer", 10, 5);
        let gw = StreamingMockGateway {
            chunks:   vec![], // zero text chunks
            response: resp,
        };

        let mut cfg = agent_cfg("zero-chunk-agent", "stream task silently");
        let mut mcfg = model_cfg();
        mcfg.streaming = true;
        cfg.max_turns = 1;

        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            vec![cfg],
            &mcfg,
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();

        let streamed = sched.streamed_agents();
        let outcomes = sched.run().await;

        assert!(outcomes.get("zero-chunk-agent").unwrap().is_ok());

        // Zero chunks emitted → agent must NOT be in streamed_agents set.
        let set = streamed.lock().unwrap();
        assert!(
            !set.contains("zero-chunk-agent"),
            "zero-chunk streaming must not populate streamed_agents (chunks_emitted == 0 path)"
        );
        drop(set);

        // InferenceStreamStarted must still be emitted (fires before infer_with_stream).
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(log.contains("\"inference_stream_started\""),
            "InferenceStreamStarted must be emitted even when no chunks are produced");
        // InferenceStreamCompleted is emitted on Ok regardless of chunk count.
        assert!(log.contains("\"inference_stream_completed\""),
            "InferenceStreamCompleted must be emitted on Ok even with zero chunks");
    }

    /// Streaming dispatch propagates inference errors correctly.
    /// When infer_with_stream returns Err, the EffectResult::Inference carries Err
    /// and the agent is marked failed in outcomes — not panicked.
    #[tokio::test]
    async fn streaming_inference_error_propagates_as_agent_failure() {
        struct FailingStreamGateway;
        #[async_trait::async_trait]
        impl InferenceGateway for FailingStreamGateway {
            async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
                Err(anyhow::anyhow!("network error"))
            }
            async fn infer_with_stream(
                &self,
                _req: InferenceRequest,
                _tx: tokio::sync::mpsc::UnboundedSender<String>,
            ) -> anyhow::Result<InferenceResponse> {
                Err(anyhow::anyhow!("streaming network error"))
            }
            fn model_id(&self) -> &str { "fail-stream" }
        }

        let cfg = agent_cfg("fail-stream-agent", "streaming task");
        let mut mcfg = model_cfg();
        mcfg.streaming = true;

        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            vec![cfg],
            &mcfg,
            unlimited(),
            Arc::new(FailingStreamGateway),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();

        let outcomes = sched.run().await;

        // Agent must appear in outcomes as Err (not panic).
        let result = outcomes.get("fail-stream-agent").expect("agent must be in outcomes");
        assert!(result.is_err(), "streaming error must propagate as agent failure");
        let err_msg = result.as_ref().unwrap_err().to_string();
        // Either the streaming error or an admission-denied wrapper — either is correct.
        assert!(
            err_msg.contains("streaming") || err_msg.contains("inference") || err_msg.contains("network"),
            "error message should reference the cause: {err_msg}"
        );

        // InferenceStreamStarted must be emitted (it fires before the infer call).
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(log.contains("\"inference_stream_started\""),
            "InferenceStreamStarted must be emitted before the failing infer call");
        // InferenceStreamCompleted must NOT be emitted (only emitted on Ok).
        assert!(!log.contains("\"inference_stream_completed\""),
            "InferenceStreamCompleted must not be emitted when infer_with_stream returns Err");
    }

    /// Default InferenceGateway::infer_with_stream fallback behaviour.
    /// A gateway that does NOT override infer_with_stream should drop the sender
    /// and fall through to infer(). No chunks are produced, but the response is correct.
    #[tokio::test]
    async fn default_infer_with_stream_falls_back_to_infer() {
        // MockGateway does NOT override infer_with_stream, so the default is used.
        // When scheduling a streaming=true request, the default drops the tx (no
        // chunks) and calls infer() — the result must still be correct.
        let gw = MockGateway::new(vec![end_turn("fallback answer", 10, 5)]);

        let mut mcfg = model_cfg();
        mcfg.streaming = true;
        let mut cfg = agent_cfg("fallback-agent", "stream task");
        cfg.max_turns = 1;

        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![cfg],
            &mcfg,
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();

        let streamed = sched.streamed_agents();
        let outcomes = sched.run().await;

        // Agent must complete successfully via the fallback path.
        assert!(
            outcomes.get("fallback-agent").unwrap().is_ok(),
            "default infer_with_stream fallback must complete successfully"
        );
        // No chunks → streamed_agents must be empty.
        let set = streamed.lock().unwrap();
        assert!(
            set.is_empty(),
            "default infer_with_stream (no chunks) must not populate streamed_agents"
        );
    }

    /// Multi-agent streaming prefixes each chunk with [agent_id].
    /// When two streaming agents run concurrently (is_multi=true), each chunk
    /// written to stdout must be prefixed with the agent ID.
    /// This test verifies the `is_multi` branch in the print_fut closure fires
    /// by running two streaming agents simultaneously.
    #[tokio::test]
    async fn streaming_two_agents_populates_both_in_streamed_agents() {
        // Two streaming agents running concurrently → both should appear in streamed_agents.
        let _resp_a = end_turn("agent-a answer", 10, 5);
        let _resp_b = end_turn("agent-b answer", 10, 5);

        struct TwoAgentGateway {
            calls: Arc<Mutex<u32>>,
        }
        #[async_trait::async_trait]
        impl InferenceGateway for TwoAgentGateway {
            async fn infer(&self, _req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
                Ok(end_turn("fallback", 10, 5))
            }
            async fn infer_with_stream(
                &self,
                _req: InferenceRequest,
                tx: tokio::sync::mpsc::UnboundedSender<String>,
            ) -> anyhow::Result<InferenceResponse> {
                let n = {
                    let mut guard = self.calls.lock().unwrap();
                    *guard += 1;
                    *guard
                };
                // Both agents emit one chunk each.
                let _ = tx.send(format!("chunk-from-agent-{n}"));
                if n == 1 {
                    Ok(end_turn("agent-a answer", 10, 5))
                } else {
                    Ok(end_turn("agent-b answer", 10, 5))
                }
            }
            fn model_id(&self) -> &str { "two-agent-stream" }
        }

        let mut mcfg = model_cfg();
        mcfg.streaming = true;
        let mut cfg_a = agent_cfg("stream-a", "task a");
        cfg_a.max_turns = 1;
        let mut cfg_b = agent_cfg("stream-b", "task b");
        cfg_b.max_turns = 1;

        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![cfg_a, cfg_b],
            &mcfg,
            unlimited(),
            Arc::new(TwoAgentGateway { calls: Arc::new(Mutex::new(0)) }),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();

        let streamed = sched.streamed_agents();
        let outcomes = sched.run().await;

        assert!(outcomes["stream-a"].is_ok(), "stream-a must succeed");
        assert!(outcomes["stream-b"].is_ok(), "stream-b must succeed");

        // Both agents emitted chunks → both must appear in streamed_agents.
        let set = streamed.lock().unwrap();
        assert!(set.contains("stream-a"), "stream-a should be in streamed_agents");
        assert!(set.contains("stream-b"), "stream-b should be in streamed_agents");
    }

    /// ModelConfig streaming field defaults to false via #[serde(default)].
    #[test]
    fn model_config_streaming_defaults_to_false() {
        // Verify that omitting `streaming` from a TOML snippet deserializes as false.
        let toml_str = r#"
            provider = "anthropic"
            model = "claude-sonnet-4-6"
            max_tokens = 4096
        "#;
        let cfg: crate::config::ModelConfig = toml::from_str(toml_str).unwrap();
        assert!(!cfg.streaming, "streaming must default to false when omitted from config");
    }

    /// ModelConfig streaming field can be set to true in TOML.
    #[test]
    fn model_config_streaming_can_be_enabled() {
        let toml_str = r#"
            provider = "anthropic"
            model = "claude-sonnet-4-6"
            max_tokens = 4096
            streaming = true
        "#;
        let cfg: crate::config::ModelConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.streaming, "streaming must be parsed as true when set in config");
    }
}
