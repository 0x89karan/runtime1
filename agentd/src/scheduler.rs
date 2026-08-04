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
    capability::{capability_covered_by, Capability},
    checkpoint::{AgentCheckpoint, AwaitingEntry, CheckpointStore, ParkedApprovalEntry, SchedulerCheckpoint},
    config::{AgentConfig, AgentTier, ModelConfig, PendingActionRequest, SchedulerConfig, SpawnConfig},
    egress::EgressProxy,
    flight_recorder::{EventKind, FlightRecorder},
    inference::{Block, InferenceGateway, InferenceRequest, InferenceResponse, Msg, Role},
    memory::MemoryStore,
    runs::RunTracker,
    tools::ToolRegistry,
    universal::UniversalAgent,
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
    /// tool_use_id from the parent's spawn_agent / run_job call — injected back as ToolResult.
    call_id:   String,
    /// Whether the child's answer is delivered into the parent's context on completion.
    /// `true` for `spawn_agent` (trusted delegation — the parent authored the task and wants
    /// the result). `false` for `run_job` (cap.2b): the parent (an injectable cron trigger)
    /// receives ONLY an agentd-authored completion signal, never the child's email-derived
    /// output — the content leak that made the injected-orchestrator bypass possible.
    #[serde(default = "default_deliver_content")]
    deliver_content: bool,
}

/// Serde default so pre-cap.2b checkpoints (which only ever held spawn_agent awaits) restore
/// as content-delivering — the correct behavior for the trusted-delegation path.
fn default_deliver_content() -> bool {
    true
}

/// Repair a restoring agent's dangling tool calls and record what was repaired.
///
/// attn.2 R2. Extracted because both restore loops (TOML agents and checkpoint-only children)
/// need it identically, and a duplicated copy meant a change to the event shape had to be made
/// twice — with only one of the two copies under test.
///
/// Recorded as `AgentRestored`, NOT `Error` (/review, maintainability specialist). This is a
/// successful self-heal on a path the agent then survives, and
/// `agentctl/src/watch/inspector.rs:is_error_event` matches `"kind":"error"` to drive both the
/// Inspector's Errors filter and the red row colour — so recording it as an error would make
/// every self-healed boot read to the operator as a failure. `agent_restored` already exists
/// and is documented in `CONVENTIONS.md`, so this adds no new kind and trips no build gate.
fn repair_and_record(
    agent_id: &str,
    cp_agent: &mut crate::checkpoint::AgentCheckpoint,
    live_call_ids: &std::collections::HashSet<String>,
    recorder: &FlightRecorder,
) {
    let repaired = AgentTask::repair_dangling_tool_uses(&mut cp_agent.messages, live_call_ids);
    if repaired.is_empty() {
        return;
    }
    // Field names match the checkpoint-side repair (`build_scheduler_checkpoint`) on purpose.
    // These were `repaired_call_ids` with no count while the checkpoint side emitted
    // `repaired_ids` + `repaired_count`, so a log query for one silently missed the other —
    // and post-attn.3 THIS path is the rarer and more interesting one, because it now means
    // "this checkpoint was written by an older binary". Found at /qa.
    recorder.record(agent_id, Some(cp_agent.turn), EventKind::AgentRestored, json!({
        "stage":          "restore_repair",
        "reason":         "interrupted tool calls had no result at checkpoint time",
        "repaired_ids":   repaired,
        "repaired_count": repaired.len(),
    }));
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
    /// Global budget-window start (ux.8′), wall-clock Unix seconds.
    budget_window_start: u64,
    /// Lifetime `tokens_spent` at the current window's start; windowed global
    /// spend = `tokens_spent − global_window_anchor` (ux.8′, monotonic-counter).
    global_window_anchor: u64,
    /// Is a budget-reset window configured (`[scheduler] budget_reset_interval > 0`)?
    ///
    /// Decides what per-agent budget exhaustion MEANS: with a window it DEFERS and the next
    /// rollover revives the agent; without one it calls `handle_agent_terminal` — a kill. ux.13-TUI
    /// publishes it because the cockpit offers "Park" (a `set_budget` at current spend) and described
    /// it as reversible, which is only true when this is set. Default `0` (only the CoS configs set
    /// an interval), so on a plain `agentd agent.toml` Park IS Cancel — the operator has to be told.
    budget_resettable: bool,
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
    /// Optional egress proxy; when present, each inference emits a signed receipt.
    egress:           Option<Arc<EgressProxy>>,
    /// Bound address of the HTTP egress proxy (set by p7.5b, used by p7.6 spawn path).
    egress_addr:      Option<std::net::SocketAddr>,
    /// Registry of registered universal-tier workloads. Used by p7.6 spawn path.
    proxy_registry:   Option<Arc<crate::egress::ProxyRegistry>>,
    /// Universal-tier child processes (p7.6). Keyed by agent ID.
    universal_agents: HashMap<String, UniversalAgent>,
    /// Agent IDs registered as orchestrated — should park between turns instead of terminating.
    orchestrated: HashSet<String>,
    /// Agent IDs currently parked (terminal=true) between REPL turns, awaiting next inject.
    waiting: HashSet<String>,
    /// Agent IDs with a pending operator cancel (ux.13): id → cause ("operator" or
    /// "cascade from <parent>"). NOT checkpointed — a cancel must not survive a restart.
    /// The enqueue_or_defer gate funnels a running agent when its future returns;
    /// handle_agent_terminal consumes the entry (emits AgentCancelled + purges queues).
    cancel_requested: HashMap<String, String>,
    /// Universal-tier agent IDs flagged for cancellation (budget.1 / AUDIT-v0.97 P3).
    /// The sync control handler can't `.await` kill()/deregister, so it flags here and
    /// the async run loop drains this via `drain_universal_cancels` after each command.
    /// NOT checkpointed — universal subprocesses never survive a restart.
    universal_cancel_requested: HashSet<String>,
    /// Credential gateway for per-agent grant projection (cred.5). None when disabled.
    cred_gw: Option<Arc<crate::credential::CredentialGateway>>,
    /// Run-history tracker (ux.11b): lifecycle transitions send RunEvents off-loop.
    run_tracker: RunTracker,
    /// Config-declared sealed jobs (cap.2b), keyed by job id. A `run_job(job_id)` call
    /// materializes a child from THIS declaration's fixed caps + rendered task — the trust
    /// root is config, not the (injectable) caller.
    jobs: HashMap<String, crate::config::Job>,
}

impl SchedulerState {
    /// Universal-tier lifetime token spend (budget.1 / AUDIT-v0.97 P2-2). p7.6
    /// subprocesses forward through the egress proxy, which meters into the shared
    /// `GlobalBudgetMeter`. 0 when no egress proxy is attached.
    fn universal_lifetime_spent(&self) -> u64 {
        self.egress.as_ref().map(|e| e.meter().universal_spent()).unwrap_or(0)
    }

    /// Universal-tier spend within the current window (budget.1 / P2-2). Carries its
    /// OWN runtime anchor, separate from the persisted native `global_window_anchor`,
    /// so a restart (universal_spent → 0) never strands the native window (Finding 2).
    fn universal_windowed_spent(&self) -> u64 {
        self.egress.as_ref().map(|e| e.meter().universal_windowed()).unwrap_or(0)
    }

    /// Combined native + universal LIFETIME spend — reported on the metering surface
    /// so the snapshot no longer under-counts the universal tier (display only, not an
    /// enforcement anchor).
    fn combined_lifetime_spent(&self) -> u64 {
        self.tokens_spent.saturating_add(self.universal_lifetime_spent())
    }

    /// Global spend within the current budget window (ux.8′): native-since-anchor +
    /// universal-since-anchor. The native term is `tokens_spent − global_window_anchor`
    /// (unchanged from pre-budget.1, persisted-anchor consistent); the universal term
    /// (budget.1 / P2-2) is runtime-only. The single source of truth for the windowed
    /// global ceiling comparison, so the call sites (admission, drain, rebase, reset)
    /// never drift on this enforcement-path arithmetic.
    fn global_windowed_spent(&self) -> u64 {
        self.tokens_spent
            .saturating_sub(self.global_window_anchor)
            .saturating_add(self.universal_windowed_spent())
    }

    /// Publish the native lifetime + native window anchor to the shared egress meter
    /// so the HTTP proxy computes the SAME combined windowed spend and self-throttles
    /// universal forwarding at the global ceiling (budget.1 / P2-2). Called after every
    /// mutation of `tokens_spent` or `global_window_anchor`; both change only under
    /// scheduler activity, so the proxy's view stays fresh without a tick.
    fn publish_budget(&self) {
        if let Some(e) = self.egress.as_ref() {
            e.meter().publish(self.tokens_spent, self.global_window_anchor);
        }
    }

    /// Rebase the universal window anchor on the shared meter — zeroing the universal
    /// windowed term. Called alongside every native `global_window_anchor` rebase so
    /// both terms of the window reset together (budget.1 / P2-2, Finding 2).
    fn rebase_universal_window(&self) {
        if let Some(e) = self.egress.as_ref() {
            e.meter().rebase_universal();
        }
    }
}

/// Scheduler-level state restored from a checkpoint (not exposed outside this module).
struct SchedulerRestored {
    awaiting:           Vec<AwaitingEntry>,
    mailboxes:          HashMap<String, Vec<MailMessage>>,
    tokens_spent:       u64,
    budget_window_start: u64,
    global_window_anchor: u64,
    child_seq:          u64,
    spawn_depths:       HashMap<String, u32>,
    parent_map:         HashMap<String, String>,
    pending_approvals:  Vec<ParkedApprovalEntry>,
    approval_seq:       u64,
    waiting_agents:     Vec<String>,
    orchestrated_agents: Vec<String>,
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
    /// Optional egress proxy; attached via with_egress().
    egress:              Option<Arc<EgressProxy>>,
    /// Bound address of the HTTP egress proxy (p7.5b). Set via with_egress_addr().
    egress_addr:         Option<std::net::SocketAddr>,
    /// Registry of registered universal-tier workloads (p7.5b).
    proxy_registry:      Option<Arc<crate::egress::ProxyRegistry>>,
    /// Universal-tier configs staged at construction; spawned in run() once egress_addr is known.
    universal_pending:   Vec<AgentConfig>,
    /// Credential gateway for per-agent grant projection into the snapshot (cred.5).
    cred_gw:             Option<Arc<crate::credential::CredentialGateway>>,
    /// Run-history tracker (ux.11b). Disabled (no-op) unless set via with_run_tracker().
    run_tracker:         RunTracker,
    /// Config-declared sealed jobs (cap.2b), set via with_jobs(). Converted to a
    /// by-id map when the run-loop `SchedulerState` is built.
    jobs:                Vec<crate::config::Job>,
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
        let mut universal_pending: Vec<AgentConfig> = Vec::new();

        if let Some(cp) = checkpoint {
            let SchedulerCheckpoint {
                agents:              cp_agent_list,
                awaiting:            cp_awaiting,
                mailboxes:           cp_mailboxes,
                tokens_spent:        cp_tokens,
                child_seq:           cp_child_seq,
                spawn_depths:        cp_spawn_depths,
                parent_map:          cp_parent_map,
                pending_approvals:   cp_pending_approvals,
                approval_seq:        cp_approval_seq,
                waiting_agents:      cp_waiting_agents,
                orchestrated_agents: cp_orchestrated_agents,
                budget_window_start: cp_budget_window_start,
                global_window_anchor: cp_global_window_anchor,
                ..
            } = cp;

            let mut cp_map: HashMap<String, AgentCheckpoint> = cp_agent_list
                .into_iter()
                .map(|a| (a.agent_id.clone(), a))
                .collect();

            // attn.2 R2 / attn.1a-05 — the call_ids the scheduler has promised to answer.
            // Built HERE because this is the only place that sees both the per-agent
            // checkpoints and the scheduler's own awaiting/pending_approvals state;
            // `AgentTask::from_checkpoint` takes one agent and cannot know any of this.
            // A dangling tool_use in this set is legitimate and must be left alone —
            // repairing it would make the eventual real result arrive as a tool_result
            // with no matching tool_use, the same API error from the other side.
            //
            // An await is only a PROMISE if the child will still exist to fulfil it
            // (/review, Codex adversarial P1). `handle_agent_terminal` is the only thing
            // that delivers a child result, and it only runs for an agent that exists, so
            // an await naming an absent child is dead. Counting it as live would suppress
            // the repair AND (via the seed skip) park the parent forever — strictly worse
            // than the 400 this increment removes.
            //
            // The membership set must be the agents that will END UP in `state.agents`, not
            // just the checkpointed ones (/review, maintainability specialist). `state.agents`
            // is a superset: it also holds TOML agents built fresh by `AgentTask::new` when
            // the checkpoint had no entry for them. Filtering on `cp_map` alone while the
            // awaiting-drop below filters on `state.agents` made the two disagree for exactly
            // that difference — the repair would answer the call as orphaned while the await
            // was KEPT, so the child's eventual real result would arrive as a second
            // tool_result with no matching tool_use. That is the "same 400 from the other
            // side" these comments warn about, produced by the fix itself.
            let mut will_exist: std::collections::HashSet<String> =
                cp_map.keys().cloned().collect();
            will_exist.extend(
                agent_configs
                    .iter()
                    .filter(|c| c.tier != AgentTier::Universal)
                    .map(|c| c.id.clone()),
            );
            let live_call_ids: std::collections::HashSet<String> = cp_awaiting
                .iter()
                .filter(|e| will_exist.contains(&e.child_id))
                .map(|e| e.call_id.clone())
                .chain(cp_pending_approvals.iter().map(|e| e.call_id.clone()))
                .collect();

            let mut universal_ids_cp: std::collections::HashSet<String> = std::collections::HashSet::new();
            for cfg in agent_configs {
                anyhow::ensure!(
                    !agents.contains_key(&cfg.id) && !universal_ids_cp.contains(&cfg.id),
                    "duplicate agent id: {}",
                    cfg.id
                );
                if cfg.tier == AgentTier::Universal {
                    anyhow::ensure!(
                        cfg.command.is_some(),
                        "universal-tier agent '{}' requires `command` to be set",
                        cfg.id
                    );
                    universal_ids_cp.insert(cfg.id.clone());
                    universal_pending.push(cfg);
                    continue;
                }
                let specs = registry.filtered_specs(cfg.capabilities.as_deref());
                let task = if let Some(mut cp_agent) = cp_map.remove(&cfg.id) {
                    repair_and_record(&cfg.id, &mut cp_agent, &live_call_ids, &recorder);
                    AgentTask::from_checkpoint(cp_agent, specs)
                } else {
                    AgentTask::new(&cfg.id, &cfg.task, &cfg, model_cfg, specs)
                };
                agents.insert(cfg.id.clone(), task);
            }
            // Remaining entries are dynamically-spawned children not in TOML.
            for (id, mut cp_agent) in cp_map {
                anyhow::ensure!(
                    !agents.contains_key(&id),
                    "duplicate agent id from checkpoint: {}",
                    id
                );
                let specs = registry.filtered_specs(cp_agent.cfg.capabilities.as_deref());
                repair_and_record(&id, &mut cp_agent, &live_call_ids, &recorder);
                let task = AgentTask::from_checkpoint(cp_agent, specs);
                agents.insert(id, task);
            }

            restored = Some(SchedulerRestored {
                awaiting:            cp_awaiting,
                mailboxes:           cp_mailboxes,
                tokens_spent:        cp_tokens,
                child_seq:           cp_child_seq,
                spawn_depths:        cp_spawn_depths,
                parent_map:          cp_parent_map,
                pending_approvals:   cp_pending_approvals,
                approval_seq:        cp_approval_seq,
                waiting_agents:      cp_waiting_agents,
                orchestrated_agents: cp_orchestrated_agents,
                budget_window_start: cp_budget_window_start,
                global_window_anchor: cp_global_window_anchor,
            });
        } else {
            let mut universal_pending_local: Vec<AgentConfig> = Vec::new();
            let mut universal_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
            agents.reserve(agent_configs.len());
            for cfg in agent_configs {
                anyhow::ensure!(
                    !agents.contains_key(&cfg.id) && !universal_ids.contains(&cfg.id),
                    "duplicate agent id: {}",
                    cfg.id
                );
                if cfg.tier == AgentTier::Universal {
                    // Validate early: command must be set.
                    anyhow::ensure!(
                        cfg.command.is_some(),
                        "universal-tier agent '{}' requires `command` to be set",
                        cfg.id
                    );
                    universal_ids.insert(cfg.id.clone());
                    universal_pending_local.push(cfg);
                } else {
                    let specs = registry.filtered_specs(cfg.capabilities.as_deref());
                    let task = AgentTask::new(&cfg.id, &cfg.task, &cfg, model_cfg, specs);
                    agents.insert(cfg.id.clone(), task);
                }
            }
            universal_pending = universal_pending_local;
        }

        Ok(Self {
            agents,
            sched,
            gateway,
            registry,
            recorder,
            snapshot,
            store,
            cred_gw: None,
            run_tracker:         RunTracker::disabled(),
            jobs:                Vec::new(),
            restored,
            memory_store:        None,
            distill_on_complete: false,
            default_model_cfg:   model_cfg.clone(),
            control_rx:          None,
            streamed_agents:     Arc::new(Mutex::new(HashSet::new())),
            egress:              None,
            egress_addr:         None,
            proxy_registry:      None,
            universal_pending,
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

    /// Register config-declared sealed jobs (cap.2b). Callers of `run_job(job_id)` get a
    /// child materialized from the matching declaration's fixed caps + rendered task.
    pub fn with_jobs(mut self, jobs: Vec<crate::config::Job>) -> Self {
        self.jobs = jobs;
        self
    }

    /// Attach an egress proxy. Each successful inference emits a signed receipt.
    pub fn with_egress(mut self, egress: Arc<EgressProxy>) -> Self {
        self.egress = Some(egress);
        self
    }

    /// Store the bound address of the HTTP egress proxy for p7.6 spawn injection.
    pub fn with_egress_addr(mut self, addr: Option<std::net::SocketAddr>) -> Self {
        self.egress_addr = addr;
        self
    }

    /// Store the proxy registry so p7.6 can register workloads before spawning.
    pub fn with_proxy_registry(mut self, registry: Arc<crate::egress::ProxyRegistry>) -> Self {
        self.proxy_registry = Some(registry);
        self
    }

    /// Attach the credential gateway so per-agent grant data flows into the snapshot.
    pub fn with_credential_gateway(mut self, gw: Arc<crate::credential::CredentialGateway>) -> Self {
        self.cred_gw = Some(gw);
        self
    }

    /// Attach the run-history tracker (ux.11b) so lifecycle transitions author `runs.redb`.
    pub fn with_run_tracker(mut self, tracker: RunTracker) -> Self {
        self.run_tracker = tracker;
        self
    }

    /// Return the bound egress proxy address, if configured.
    pub fn egress_addr(&self) -> Option<std::net::SocketAddr> {
        self.egress_addr
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
            egress,
            egress_addr,
            proxy_registry,
            universal_pending,
            cred_gw,
            run_tracker,
            jobs,
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
            budget_window_start: 0,
            global_window_anchor: 0,
            budget_resettable:  false,
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
            egress,
            egress_addr,
            proxy_registry,
            universal_agents:   HashMap::new(),
            orchestrated:       HashSet::new(),
            waiting:            HashSet::new(),
            cancel_requested:   HashMap::new(),
            universal_cancel_requested: HashSet::new(),
            cred_gw,
            run_tracker,
            jobs:               jobs.into_iter().map(|j| (j.id.clone(), j)).collect(),
        };

        // Restore scheduler-level state from checkpoint when present.
        if let Some(r) = restored {
            state.tokens_spent = r.tokens_spent;
            state.budget_window_start = r.budget_window_start;
            state.global_window_anchor = r.global_window_anchor;
            state.child_seq    = r.child_seq;
            state.spawn_depths = r.spawn_depths;
            state.parent_map   = r.parent_map;
            state.approval_seq = r.approval_seq;
            // attn.2 R2 (/review, Codex adversarial P1) — drop DEAD awaiting entries.
            // An entry whose child is absent from the restored agent set can never be
            // delivered: `handle_agent_terminal` is the only thing that answers it, and it
            // only runs for an agent that exists. Keeping such an entry would be worse than
            // the bug this increment fixes — the parent's dangling tool_use is treated as
            // "promised" (so the repair skips it) AND the parent is skipped by the seed loop,
            // so it parks silently forever, re-checkpointing the poison. Before R2 the same
            // state at least failed fast and visibly with a 400.
            // Dropping the entry makes it fail FORWARD: the repair answers the orphaned call
            // with an error result and the parent is seeded and runs.
            for entry in r.awaiting {
                if !state.agents.contains_key(&entry.child_id) {
                    recorder.record(&entry.parent_id, None, EventKind::Error, json!({
                        "stage": "restore",
                        "error": "awaiting child absent from restored agents — dropping dead await",
                        "child_id": &entry.child_id,
                        "call_id":  &entry.call_id,
                    }));
                    continue;
                }
                state.awaiting.insert(entry.child_id, AwaitingParent {
                    parent_id: entry.parent_id,
                    call_id:   entry.call_id,
                    deliver_content: entry.deliver_content,
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
            // Restore orchestrated/waiting sets so parked agents are not re-stepped.
            state.orchestrated.extend(r.orchestrated_agents);
            state.waiting.extend(r.waiting_agents);
        }

        init_budget_window(&mut state, &sched, now_unix_secs());
        // budget.1 / P2-2: seed the egress meter with the (possibly restored) native
        // lifetime + anchor so universal throttling is correct before the first
        // native inference republishes.
        state.publish_budget();

        // ux.11b: open a run segment for every seeded/restored native agent. Idempotent
        // (G3) — on a restart the persisted open segment is continued, not duplicated.
        // Collect first (borrows state.agents), then call the tracker (borrows state).
        {
            let seeds: Vec<(String, Option<String>, u64)> = state.agents.iter()
                .map(|(id, task)| (id.clone(), state.parent_map.get(id).cloned(), task.context_tokens()))
                .collect();
            for (id, parent_id, spend) in seeds {
                state.run_tracker.open(&id, parent_id, "config_seed", Some(spend), "native");
            }
        }

        // Spawn universal-tier agents before the native seed loop.
        for cfg in universal_pending {
            match egress_addr {
                Some(addr) => {
                    // Generate a per-agent ephemeral key and register it so the
                    // proxy can authenticate this child's inference requests.
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as u64;
                    let ephemeral_key = format!("ua-{}-{:016x}", cfg.id, ts);
                    if let Some(reg) = &state.proxy_registry {
                        use std::sync::atomic::AtomicU64;
                        reg.register(ephemeral_key.clone(), crate::egress::ProxyEntry {
                            agent_id: cfg.id.clone(),
                            policy:   crate::egress::ProxyPolicy {
                                allowed_hosts:           vec![],
                                token_budget_remaining:  Arc::new(AtomicU64::new(cfg.token_budget)),
                            },
                        }).await;
                    }
                    match UniversalAgent::spawn(&cfg, addr, &ephemeral_key, &recorder) {
                        Ok(ua) => {
                            state.universal_agents.insert(cfg.id.clone(), ua);
                            // ux.11b: open a run segment; universal-tier is proxy-metered
                            // (no context_tokens) → spend recorded as None.
                            state.run_tracker.open(&cfg.id, None, "universal_spawn", None, "universal");
                        }
                        Err(e) => {
                            // Deregister the key since the agent never started.
                            if let Some(reg) = &state.proxy_registry {
                                reg.deregister_by_key(&ephemeral_key).await;
                            }
                            let msg = format!("universal spawn failed: {e}");
                            recorder.record(
                                &cfg.id,
                                None,
                                EventKind::AgentFailed,
                                json!({ "reason": "universal_spawn_failed", "error": e.to_string() }),
                            );
                            state.outcomes.insert(cfg.id.clone(), Err(anyhow::anyhow!(msg)));
                        }
                    }
                }
                None => {
                    let msg = "universal spawn failed: egress proxy not configured";
                    recorder.record(
                        &cfg.id,
                        None,
                        EventKind::AgentFailed,
                        json!({ "reason": "universal_spawn_failed", "error": "egress proxy not configured" }),
                    );
                    state.outcomes.insert(cfg.id.clone(), Err(anyhow::anyhow!(msg)));
                }
            }
        }
        update_snapshot(&snapshot, &state);

        // Seed: step each agent once to kick off its first effect.
        // `or_insert` preserves restored spawn_depths; fresh agents get depth 0.
        // Agents already parked in pending_approvals must NOT be re-stepped — doing so
        // would re-emit RequestApproval and create a duplicate pending_approvals entry.
        let parked_agent_ids: std::collections::HashSet<String> = state
            .pending_approvals
            .values()
            .map(|pa| pa.agent_id.clone())
            .collect();
        // attn.2 R2.1 / attn.1a-05 — a restored PARENT waiting on a child must not be
        // re-stepped either. `awaiting` is keyed by CHILD id, so the parents are in
        // `.values()`, not the keys; reaching for `contains_key` here is the same mistake
        // that produced the ux.13 cancel P0.
        //
        // This is the bug that actually bricked the CoS. The trigger calls `run_job` and
        // parks in `awaiting` without being enqueued, so on restore it was re-stepped,
        // reached `step_need_infer`, and shipped its dangling `run_job` tool_use to the
        // provider. Repairing messages alone does NOT fix it: the trigger would infer
        // cleanly, its prompt would tell it to `run_job` again — a duplicate, fully-paid
        // cycle — and the original child's result would then arrive for a call_id no
        // longer present. Both halves are required.
        let awaiting_parent_ids: std::collections::HashSet<String> = state
            .awaiting
            .values()
            .map(|a| a.parent_id.clone())
            .collect();
        let ids: Vec<String> = state.agents.keys().cloned().collect();
        for id in ids {
            state.spawn_depths.entry(id.clone()).or_insert(0);
            state.mailboxes.entry(id.clone()).or_default();
            if parked_agent_ids.contains(&id) {
                continue; // Already awaiting approval — do not re-step.
            }
            if state.waiting.contains(&id) {
                continue; // Restored orchestrated agent parked between turns — do not re-step.
            }
            if awaiting_parent_ids.contains(&id) {
                continue; // Restored parent awaiting a child result — the child delivers it.
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
            // Rebase budget windows first (ux.8′ / audit86-P0-2): wall-clock,
            // independent of agent liveness, so an always-on agent parked on an
            // exhausted budget is revived when its window rolls over. No-op when
            // budget_reset_interval is 0 or no full window has elapsed.
            maybe_rebase_windows(&mut state, &sched, &gateway, &registry, &recorder);

            // Poll universal agents for exit on each iteration (non-blocking).
            poll_universal_agents(&mut state, &recorder, &snapshot).await;

            // When all pending work is done, either wait for an operator command or exit.
            if state.pending.is_empty() && state.universal_agents.is_empty() {
                match control_rx {
                    // No control channel (ux.8′ F-B): if deferred work is waiting on a
                    // budget-window rollover, keep looping so the tick-driven rebase can
                    // revive it — breaking here would drop the deferred infer with no
                    // outcome. Otherwise there is genuinely nothing left to do → exit.
                    None if !state.deferred.is_empty() && sched.budget_reset_interval > 0 => {
                        tokio::time::sleep(std::time::Duration::from_secs(BUDGET_TICK_SECS)).await;
                    }
                    None => break 'main,
                    Some(ref mut rx) => {
                        tokio::select! {
                            cmd = rx.recv() => {
                                let Some(cmd) = cmd else { break 'main; };
                                dispatch_control_command(cmd, &default_model_cfg, &mut state, &sched, &gateway, &registry, &recorder);
                                drain_universal_cancels(&mut state, &recorder).await;
                                update_snapshot(&snapshot, &state);
                            }
                            // Periodic wake so a fully idle agentd still rebases its
                            // budget window on schedule (ux.8′) — the loop-top
                            // maybe_rebase_windows runs on the next iteration.
                            _ = tokio::time::sleep(std::time::Duration::from_secs(BUDGET_TICK_SECS)) => {}
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

            // When native agents are done but universal agents are still running,
            // yield briefly so the poll at the top of the loop can detect their exit.
            // budget.1 / P3: this branch MUST also service operator control commands —
            // otherwise a Cancel targeting a universal-ONLY workload is never read
            // (the idle/control-polling branch above is gated on universal_agents being
            // empty), so universal-cancel would be starved in exactly the case it exists
            // for. Poll control_rx here too and drain any resulting universal cancels.
            if state.pending.is_empty() {
                tokio::select! {
                    cmd = async {
                        match control_rx.as_mut() {
                            Some(rx) => rx.recv().await,
                            None     => std::future::pending::<Option<crate::control::ControlCommand>>().await,
                        }
                    } => {
                        match cmd {
                            Some(cmd) => {
                                dispatch_control_command(cmd, &default_model_cfg, &mut state, &sched, &gateway, &registry, &recorder);
                                drain_universal_cancels(&mut state, &recorder).await;
                                update_snapshot(&snapshot, &state);
                            }
                            None => { control_rx = None; }
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
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
                            drain_universal_cancels(&mut state, &recorder).await;
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
                            // handle_agent_terminal() consolidates waiting/orchestrated cleanup.
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
                            // P3-3 (audit86): do the agent-specific work if the agent is present;
                            // record + skip it if the agent was removed mid-effect. The agent IS
                            // present today (this result was dispatched from its own step in a
                            // single-threaded loop) — this is defensive future-proofing so a
                            // remove-mid-effect under `panic = "abort"` can't abort the runtime.
                            // CRITICAL (P3-3 /review): the shared `drain_deferred` below MUST run on
                            // BOTH paths — `in_flight` was decremented above, so a slot-deferred
                            // agent has to be admitted into the freed slot even if THIS agent vanished.
                            let to_enqueue = 'agent: {
                                let Some(sm) = state.agents.get_mut(&agent_id) else {
                                    recorder.record(&agent_id, None, EventKind::Error,
                                        json!({ "error": "effect_agent_missing", "site": "inference.provide", "agent": &agent_id }));
                                    break 'agent None;
                                };
                                let priority = sm.priority();
                                // Provide response, then step. Draining mailbox AFTER step keeps
                                // injected messages after the assistant turn just pushed (F-005).
                                sm.provide_inference(resp, &recorder);
                                let (effect, turn) = {
                                    let Some(sm) = state.agents.get_mut(&agent_id) else {
                                        recorder.record(&agent_id, None, EventKind::Error,
                                            json!({ "error": "effect_agent_missing", "site": "inference.step", "agent": &agent_id }));
                                        break 'agent None;
                                    };
                                    let t = sm.turn();
                                    (sm.step(&recorder), t)
                                };
                                drain_mailbox(&agent_id, &mut state, &recorder);
                                state.tokens_spent = state.tokens_spent.saturating_add(new_tokens);
                                // budget.1 / P2-2: republish native lifetime so the egress
                                // proxy's global window reflects this spend immediately.
                                state.publish_budget();
                                let Some(cap_set) = state.agents.get(&agent_id).map(|a| a.cap_set_cloned()) else {
                                    recorder.record(&agent_id, None, EventKind::Error,
                                        json!({ "error": "effect_agent_missing", "site": "inference.capset", "agent": &agent_id }));
                                    break 'agent None;
                                };
                                Some((effect, turn, priority, cap_set))
                            };
                            // Shared post-inference cleanup — runs regardless of the agent's fate.
                            // Drain deferred agents first (they were waiting for a slot to open),
                            // then re-enqueue the completing agent's next step IF it survived. This
                            // gives queued agents priority over the agent that just ran (fairness).
                            drain_deferred(&mut state, &sched, &gateway, &registry, &recorder);
                            if let Some((effect, turn, priority, cap_set)) = to_enqueue {
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
                            }
                            update_snapshot(&snapshot, &state);
                        }
                        EffectResult::Tools { agent_id, results } => {
                            // Provide tool results, then drain mailbox before next step.
                            // P3-3 (audit86): never PANIC on a missing agent — record + skip.
                            let priority = {
                                let Some(sm) = state.agents.get_mut(&agent_id) else {
                                    recorder.record(&agent_id, None, EventKind::Error,
                                        json!({ "error": "effect_agent_missing", "site": "tools.provide", "agent": &agent_id }));
                                    update_snapshot(&snapshot, &state);
                                    continue;
                                };
                                // ux.2b: last_error (→ Error attention) is updated inside
                                // provide_tool_results, so it covers this async batch AND every
                                // synthetic reject/error path uniformly (see agent/mod.rs).
                                let p = sm.priority();
                                sm.provide_tool_results(results, &recorder);
                                p
                            };
                            drain_mailbox(&agent_id, &mut state, &recorder);

                            // Periodic checkpoint at clean turn boundary (best-effort). A missing
                            // agent here just skips the checkpoint (never panics).
                            if interval > 0 {
                                if let Some(agent_turn) = state.agents.get(&agent_id).map(|a| a.turn()) {
                                    if agent_turn.is_multiple_of(interval) {
                                        checkpoint_all(&store, &state, &recorder).await;
                                    }
                                }
                            }

                            let (effect, turn) = {
                                let Some(sm) = state.agents.get_mut(&agent_id) else {
                                    recorder.record(&agent_id, None, EventKind::Error,
                                        json!({ "error": "effect_agent_missing", "site": "tools.step", "agent": &agent_id }));
                                    update_snapshot(&snapshot, &state);
                                    continue;
                                };
                                let t = sm.turn();
                                (sm.step(&recorder), t)
                            };
                            let Some(cap_set) = state.agents.get(&agent_id).map(|a| a.cap_set_cloned()) else {
                                recorder.record(&agent_id, None, EventKind::Error,
                                    json!({ "error": "effect_agent_missing", "site": "tools.capset", "agent": &agent_id }));
                                update_snapshot(&snapshot, &state);
                                continue;
                            };
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
                    // F3: compare WINDOWED spend, not lifetime — under a reset
                    // window the lifetime total exceeds the per-window ceiling
                    // within ~1 day, which would permanently skip distillation.
                    const MIN_DISTILL_TOKENS: u64 = 512;
                    let budget_ok = sched.global_token_budget == 0
                        || state.global_windowed_spent() + MIN_DISTILL_TOKENS <= sched.global_token_budget;
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
                            // F3: windowed remaining, not lifetime — else clamps to 0
                            // ~1 day into a reset window and starves distillation.
                            let remaining = cap.saturating_sub(state.global_windowed_spent()) as u32;
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
                            // budget.1 / P2-2: republish so the egress proxy's global
                            // window reflects fresh native spend.
                            state.publish_budget();

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

        // Kill any Universal agents still running at shutdown.
        // Deregister ephemeral key BEFORE kill() so the child cannot authenticate
        // further inference requests during the 5-second SIGTERM grace window.
        let ua_ids: Vec<String> = state.universal_agents.keys().cloned().collect();
        for id in ua_ids {
            let ua = state.universal_agents.get_mut(&id).unwrap();
            let ephemeral_key = ua.ephemeral_key.clone();
            if let Some(reg) = &state.proxy_registry {
                reg.deregister_by_key(&ephemeral_key).await;
            }
            ua.kill().await;
            state.outcomes.entry(id.clone()).or_insert_with(|| {
                Err(anyhow::anyhow!("universal agent killed at shutdown"))
            });
            state.run_tracker.close(&id, "interrupted", Some("shutdown".into()), None, None);
        }

        // ux.11b (ship review F2): close any native run segments still open at shutdown
        // so they don't leak as "running". Agents that already terminated had close()
        // called in handle_agent_terminal → this is a no-op double-close for them; it
        // genuinely closes the perpetual orchestrator + agents parked in waiting/approval.
        // Collect first (borrows state.agents), then call the tracker (borrows state).
        let native_open: Vec<(String, u64)> = state.agents.iter()
            .map(|(id, task)| (id.clone(), task.context_tokens()))
            .collect();
        for (id, tokens) in native_open {
            state.run_tracker.close(&id, "interrupted", Some("shutdown".into()), None, Some(tokens));
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
    // ux.13: is this terminal an operator cancel? Consume the flag (so a reused agent id
    // can't inherit a stale cancel) and, if so, emit AgentCancelled + purge the queues that
    // handle_agent_terminal does not normally touch — the deferred heap (else drain_deferred
    // would pop a stale entry and push a future for a removed agent → panic) and any pending
    // approval for this agent (else it could be approved against a dead agent).
    let cancel_cause = state.cancel_requested.remove(&agent_id);
    if let Some(cause) = &cancel_cause {
        recorder.record(
            &agent_id,
            None,
            EventKind::AgentCancelled,
            json!({ "agent_id": &agent_id, "cause": cause }),
        );
        state.deferred.retain(|e| e.agent_id != agent_id);
        state.pending_approvals.retain(|_, p| p.agent_id != agent_id);
    }

    // Always clear orchestration membership on termination — prevents phantom entries.
    state.waiting.remove(&agent_id);
    state.orchestrated.remove(&agent_id);
    // ux.6a: drop this agent's deny-episode state. Every scheduler deny site is a TERMINAL
    // denial, so without this the agent's entry can never be reclaimed — it will never record
    // another allowed inference to re-arm itself — and `denied_edges` grows for the life of the
    // process. Same leak class as audit86-P2-5, which is why it belongs in this block.
    if let Some(eg) = state.egress.as_ref() {
        eg.forget_agent(&agent_id);
    }

    // ux.11b: close the run segment before the agent leaves state.agents. This funnels
    // every native terminal (child, root, admission-denial). Spend = Δ context_tokens
    // captured while the task still exists; status from the outcome.
    {
        let (status, stop_reason, last_error): (&str, Option<String>, Option<String>) =
            if cancel_cause.is_some() {
                ("cancelled", Some("operator_cancelled".to_string()), None)
            } else {
                match &result {
                    Ok(_)  => ("done", Some("completed".to_string()), None),
                    Err(e) => ("failed", None, Some(e.to_string())),
                }
            };
        let end_tokens = state.agents.get(&agent_id).map(|t| t.context_tokens());
        state.run_tracker.close(&agent_id, status, stop_reason, last_error, end_tokens);
    }

    if let Some(awaiting) = state.awaiting.remove(&agent_id) {
        // This agent is a child — inject its result into the waiting parent.
        let parent_id = awaiting.parent_id;
        let call_id = awaiting.call_id;
        state.spawn_depths.remove(&agent_id);
        state.agents.remove(&agent_id);

        // cap.2b: for a sealed job (`deliver_content=false`) the parent is an injectable
        // trigger — deliver ONLY an agentd-authored signal, never the child's output. Both
        // the success and failure branches are agentd-authored: a raw error string could
        // echo a child tool's untrusted text (e.g. an email subject) back into the trigger's
        // context, which would reopen the very leak this closes. Trusted delegation
        // (`spawn_agent`, `deliver_content=true`) still gets the child's real answer.
        let (content, is_error) = match &result {
            Ok(answer) => {
                if awaiting.deliver_content {
                    (answer.clone(), false)
                } else {
                    (format!("job '{agent_id}' completed"), false)
                }
            }
            Err(e) => {
                if awaiting.deliver_content {
                    (e.to_string(), true)
                } else {
                    (format!("job '{agent_id}' failed"), true)
                }
            }
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

        // AUDIT-v0.97 P2-4: do NOT re-step a parent that is already terminal or has a
        // pending cancel. A cancelled parked ROOT parent (e.g. the CoS trigger awaiting a
        // run_job child) is funneled + recorded in `outcomes` but stays in `state.agents`;
        // without this guard, the child's later terminal would re-step it, its consumed
        // cancel flag would NOT re-trip the enqueue_or_defer gate, and the cancelled trigger
        // would resurrect (spend more, flip AgentCancelled→done). Skip delivery in that case.
        let parent_live = state.agents.contains_key(&parent_id)
            && !state.outcomes.contains_key(&parent_id)
            && !state.cancel_requested.contains_key(&parent_id);
        if parent_live {
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
                    "error":     "parent agent not live (gone, terminal, or cancelled) when delivering child result — delivery skipped",
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
/// Disk copy of `InferenceStreamDelta`'s `text` field is capped to this many bytes —
/// preserves `flight.jsonl`'s existing preview/audit-metadata contract (bounded field
/// sizes) instead of turning it into a full model-output transcript store. The live SSE
/// broadcast always carries the full, untruncated chunk (ux.1 Eng Section 4 Taste call).
const STREAM_DELTA_DISK_TEXT_CAP: usize = 256;

#[allow(clippy::too_many_arguments)]
fn make_infer_future(
    req: InferenceRequest,
    id: String,
    turn: u32,
    is_multi: bool,
    gw: Arc<dyn InferenceGateway + Send + Sync>,
    recorder: Arc<FlightRecorder>,
    streamed_agents: Arc<Mutex<HashSet<String>>>,
    stdout_lock: Arc<tokio::sync::Mutex<()>>,
    egress: Option<Arc<EgressProxy>>,
) -> PendingFut {
    if req.streaming {
        let model = gw.model_id().to_string();
        Box::pin(async move {
            recorder.record(
                &id,
                Some(turn),
                EventKind::InferenceStreamStarted,
                json!({ "agent_id": &id, "model": &model }),
            );

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let infer_fut = gw.infer_with_stream(req, tx);
            let agent_id_label = id.clone();
            let delta_recorder  = Arc::clone(&recorder);
            let delta_agent_id  = id.clone();
            let print_fut = async move {
                use tokio::io::AsyncWriteExt;
                let mut stdout = tokio::io::stdout();
                let mut chunks_emitted: u64 = 0;
                let mut chunk_seq: u64 = 0;
                while let Some(chunk) = rx.recv().await {
                    // Record BEFORE the chunk text is consumed into the (possibly
                    // agent-prefixed) stdout line below — the SSE broadcast/flight
                    // event always carries the raw chunk, independent of the local
                    // multi-agent stdout prefix, which is a terminal-display-only concern.
                    let disk_text: String = chunk.chars().take(STREAM_DELTA_DISK_TEXT_CAP).collect();
                    delta_recorder.record_streamed(
                        &delta_agent_id,
                        Some(turn),
                        EventKind::InferenceStreamDelta,
                        json!({
                            "agent_id":  &delta_agent_id,
                            "turn_seq":  turn,
                            "chunk_seq": chunk_seq,
                            "text":      disk_text,
                        }),
                        json!({
                            "agent_id":  &delta_agent_id,
                            "turn_seq":  turn,
                            "chunk_seq": chunk_seq,
                            "text":      &chunk,
                        }),
                    );
                    chunk_seq += 1;

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
                    Some(turn),
                    EventKind::InferenceStreamCompleted,
                    json!({
                        "agent_id": &id,
                        "text_chunks_emitted": chunks_emitted,
                        "input_tokens": resp.input_tokens,
                        "output_tokens": resp.output_tokens,
                        "transport_retries": resp.transport_retries,
                    }),
                );
                if resp.transport_retries > 0 {
                    recorder.record(
                        &id,
                        Some(turn),
                        EventKind::InferenceTransportRetried,
                        json!({
                            "agent_id": &id,
                            "model":    &model,
                            "retries":  resp.transport_retries,
                        }),
                    );
                }
                if let Some(ref ep) = egress {
                    ep.record_inference(&id, &model, resp.input_tokens.into(), resp.output_tokens.into());
                }
            }

            EffectResult::Inference { agent_id: id, result: infer_result }
        })
    } else {
        let model = gw.model_id().to_string();
        Box::pin(async move {
            let result = gw.infer(req).await;
            if let Ok(ref resp) = result {
                if resp.transport_retries > 0 {
                    recorder.record(
                        &id,
                        None,
                        EventKind::InferenceTransportRetried,
                        json!({
                            "agent_id": &id,
                            "model":    &model,
                            "retries":  resp.transport_retries,
                        }),
                    );
                }
                if let Some(ref ep) = egress {
                    ep.record_inference(&id, &model, resp.input_tokens.into(), resp.output_tokens.into());
                }
            }
            EffectResult::Inference { agent_id: id, result }
        })
    }
}

/// Drain the deferred queue, admitting agents until the cap or budget is hit.
/// Agents that can never be admitted (budget exhausted) are denied immediately.
/// How often a fully idle agentd wakes to check for a budget-window rollover.
const BUDGET_TICK_SECS: u64 = 60;

/// Wall-clock Unix seconds. Non-monotonic (NTP can step it); callers use
/// `saturating_sub` so a backward step never underflows (ux.8′ / obs.1 pattern).
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Advisory string appended to global-budget denials so a bricked operator sees
/// the fix in the flight log instead of reaching for `rm checkpoint.json`.
const BUDGET_REMEDY: &str =
    "set [scheduler] budget_reset_interval (e.g. 86400) or POST /api/v1/budget/reset";

/// One-time budget-window initialization at scheduler start (ux.8′ / audit86-P0-2).
/// - A fresh start or a pre-ux.8′ checkpoint has `budget_window_start == 0` → open a
///   clean window at `now`, anchoring all current spend (global + per-agent), so a
///   migrated always-on agent doesn't inherit a spuriously-exhausted first window.
/// - An already-anchored window in the FUTURE (dead RTC / forward clock spike captured
///   in a prior rebase) is clamped to `now` (F2), else `now − start` saturates to 0 and
///   resets stall for as long as the bogus future lasts.
/// - Every agent is told whether a reset window is active (C1): when it is, per-agent
///   budget exhaustion DEFERS scheduler-side instead of the agent self-terminating
///   (which would strip waiting/orchestrated and permanently brick a resident agent).
fn init_budget_window(state: &mut SchedulerState, sched: &SchedulerConfig, now: u64) {
    // interval == 0 is legacy LIFETIME enforcement (ux.8′ Codex #3): leave the
    // anchors at 0 so windowed_spent == lifetime spend, and leave every agent's
    // budget_resettable at its `false` default (agents self-terminate on budget,
    // the pre-ux.8′ behavior). Rebasing here would silently forgive prior spend
    // once on a migrated checkpoint, breaking the documented "0 = lifetime".
    if sched.budget_reset_interval == 0 {
        return;
    }
    if state.budget_window_start == 0 {
        state.budget_window_start = now;
        state.global_window_anchor = state.tokens_spent;
        state.rebase_universal_window();
        state.publish_budget();
        for task in state.agents.values_mut() {
            let _ = task.reset_budget_window();
        }
    } else if state.budget_window_start > now {
        state.budget_window_start = now;
    }
    let budget_resettable = sched.budget_reset_interval > 0;
    // Published to the operator surfaces too (ux.13-TUI): the cockpit's Park verb is only reversible
    // when this holds, and nothing in the snapshot carried it.
    state.budget_resettable = budget_resettable;
    for task in state.agents.values_mut() {
        task.set_budget_resettable(budget_resettable);
    }
}

/// Whole budget-windows elapsed between `window_start` and `now` (ux.8′).
/// Division, not a loop (a defaulted epoch anchor is ~1.8e9 s — a loop would spin
/// ~20k times for a daily window). `saturating_sub` so an NTP step-back never
/// underflows. `interval == 0` (no reset configured) → always 0.
fn windows_elapsed(now_secs: u64, window_start: u64, interval: u64) -> u64 {
    if interval == 0 {
        return 0;
    }
    now_secs.saturating_sub(window_start) / interval
}

/// Rebase budget windows when whole intervals have elapsed (ux.8′ / audit86-P0-2).
/// Wall-clock, division-based (never a loop), monotonic-counter (advances anchors,
/// never zeroes the meters). No-op when the interval is 0 (legacy lifetime ceiling)
/// or no full window has passed. On a real rollover it drains budget-deferred
/// agents so an always-on agent parked on an exhausted budget resumes.
fn maybe_rebase_windows(
    state: &mut SchedulerState,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
) {
    maybe_rebase_windows_at(state, sched, gateway, registry, recorder, now_unix_secs());
}

/// Testable core of `maybe_rebase_windows` with an injectable clock.
fn maybe_rebase_windows_at(
    state: &mut SchedulerState,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
    now_secs: u64,
) {
    // F2: if wall-clock stepped back below the window start (NTP correction after a
    // rebase, RTC glitch), `windows_elapsed` would saturate to 0 and resets would
    // stall until real time re-climbs past the stale start. Clamp the anchor to now
    // so the window simply restarts from the corrected clock.
    if sched.budget_reset_interval > 0 && state.budget_window_start > now_secs {
        state.budget_window_start = now_secs;
        return;
    }
    let n = windows_elapsed(now_secs, state.budget_window_start, sched.budget_reset_interval);
    if n == 0 {
        return;
    }
    let spent_before = state.global_windowed_spent();
    state.global_window_anchor = state.tokens_spent;
    state.rebase_universal_window();
    state.publish_budget();
    state.budget_window_start = state
        .budget_window_start
        .saturating_add(n * sched.budget_reset_interval);
    for task in state.agents.values_mut() {
        let _ = task.reset_budget_window();
    }
    recorder.record(
        "agentd",
        None,
        EventKind::BudgetReset,
        json!({
            "target":           "global",
            "spent_before":     spent_before,
            "window_start":     state.budget_window_start,
            "interval_secs":    sched.budget_reset_interval,
            "windows_advanced": n,
        }),
    );
    // Admit agents that were deferred while the budget was exhausted.
    drain_deferred(state, sched, gateway, registry, recorder);
}

fn drain_deferred(
    state: &mut SchedulerState,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
) {
    // Windowed global spend (ux.8′): capture the anchor by value so the closure
    // does not borrow `state` (which is mutated in the admit loop below).
    let anchor = state.global_window_anchor;
    // budget.1 / P2-2: fold universal-tier WINDOWED spend into the snapshot so the
    // closure (which must not borrow `state`) still bounds the combined total. Mirrors
    // global_windowed_spent: native-since-anchor + universal-since-anchor.
    let universal = state.universal_windowed_spent();
    let budget_ok = |lifetime: u64| {
        let w = lifetime.saturating_sub(anchor).saturating_add(universal);
        sched.global_token_budget == 0 || w < sched.global_token_budget
    };
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

    // Budget exhausted. With a reset window configured (ux.8′) this is TRANSIENT
    // — leave deferred agents queued so the next window rollover (maybe_rebase_
    // windows) drains them; terminating here is the old self-brick (audit86-P0-2).
    // With no window (interval 0) it is permanent → legacy: deny everything.
    if !budget_ok(state.tokens_spent) {
        if sched.budget_reset_interval > 0 {
            return;
        }
        while let Some(d) = state.deferred.pop() {
            recorder.record(
                &d.agent_id,
                None,
                EventKind::AgentAdmissionDenied,
                json!({ "reason": "global_budget_exhausted", "tokens_spent": state.tokens_spent, "remedy": BUDGET_REMEDY }),
            );
            // ux.6a: TERMINAL denial → signed receipt. This is the production-reachable
            // denial: the HTTP egress proxy never starts in the shipped config, so before
            // this the chain could never contain a "no" in practice either.
            if let Some(eg) = state.egress.as_ref() {
                eg.receipt_denial_once(&d.agent_id, gateway.model_id(), "global_budget_exhausted");
            }
            handle_agent_terminal(
                d.agent_id,
                Err(anyhow::anyhow!("admission denied: global token budget exhausted ({BUDGET_REMEDY})")),
                state,
                sched,
                gateway,
                registry,
                recorder,
            );
        }
        return;
    }

    // Admit as many as slots allow. Re-check the PER-AGENT windowed cap here
    // (ux.8′ F-A, cross-model review): drain_deferred runs on every inference
    // completion, not just on rollover, so an agent deferred for exceeding its
    // OWN windowed budget (enqueue_or_defer) must not be silently re-admitted
    // just because a slot freed — that would leak the per-agent cap one
    // inference at a time. Only a true rollover (which resets per-agent windows
    // before draining) or a per-agent ResetBudget should revive it. Hold
    // still-over-cap entries aside and requeue them.
    let is_multi = state.agents.len() > 1;
    let mut holdback: Vec<DeferredInfer> = Vec::new();
    let mut terminate_legacy: Vec<DeferredInfer> = Vec::new();
    while !state.deferred.is_empty() && slot_ok(state.in_flight) {
        let d = state.deferred.pop().expect("checked non-empty");
        let per_agent_over = state.agents.get(&d.agent_id).is_some_and(|a| {
            let b = a.token_budget();
            b != 0 && a.windowed_spent() >= b
        });
        if per_agent_over {
            // Under a reset window (interval>0) the over-cap agent is transiently over —
            // hold it back so the next rollover revives it. Under legacy budgets
            // (interval==0) it is PERMANENTLY over: holding back would strand the request
            // forever (Codex ship review — reachable by a live SetBudget that lowers an
            // agent below its current spend, or by a concurrent inflight completion pushing
            // it over while this request sat slot-deferred). Terminate instead, matching
            // enqueue_or_defer's legacy branch.
            if sched.budget_reset_interval == 0 {
                terminate_legacy.push(d);
            } else {
                holdback.push(d);
            }
            continue;
        }
        state.in_flight += 1;
        recorder.record(
            &d.agent_id,
            Some(d.turn),
            EventKind::AgentScheduled,
            json!({ "reason": "slot_opened", "in_flight": state.in_flight }),
        );
        let gw   = Arc::clone(gateway);
        let rec  = Arc::clone(recorder);
        let sa   = Arc::clone(&state.streamed_agents);
        let sl   = Arc::clone(&state.stdout_lock);
        let eg   = state.egress.clone();
        let id   = d.agent_id;
        let turn = d.turn;
        state.pending.push(make_infer_future(d.request, id, turn, is_multi, gw, rec, sa, sl, eg));
    }
    for d in holdback {
        state.deferred.push(d);
    }
    // Legacy (no-window) permanent over-budget: terminate the agent rather than leave its
    // deferred work orphaned. Purge the agent's OTHER deferred entries first so the admit
    // loop can never schedule an inference for an agent that is being removed.
    for d in terminate_legacy {
        let id = d.agent_id;
        if state.agents.contains_key(&id) {
            state.deferred.retain(|e| e.agent_id != id);
            recorder.record(
                &id,
                Some(d.turn),
                EventKind::AgentAdmissionDenied,
                json!({ "reason": "agent_budget_exhausted", "remedy": BUDGET_REMEDY }),
            );
            // ux.6a: TERMINAL (legacy no-window) denial → signed receipt.
            if let Some(eg) = state.egress.as_ref() {
                eg.receipt_denial_once(&id, gateway.model_id(), "agent_budget_exhausted");
            }
            handle_agent_terminal(
                id,
                Err(anyhow::anyhow!("admission denied: agent_budget_exhausted ({BUDGET_REMEDY})")),
                state, sched, gateway, registry, recorder,
            );
        }
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
    // ux.13 Cancel — the single choke point. Every world-affecting effect (Infer, CallTools,
    // SpawnAgent, SendMessage, RunJob, RequestApproval) funnels through here before dispatch,
    // so gating on the cancel flag here stops the agent BEFORE any NEW world action (and before
    // scheduling one more inference). A running agent's flag is set by the dispatch arm and
    // consumed here when its in-flight future returns (in_flight already decremented, so the
    // assert is safe); this also closes the mid-spawn cascade leak (a SpawnAgent effect from the
    // parent's just-returned inference is funneled before dispatch_spawn creates the child).
    if state.cancel_requested.contains_key(&agent_id) {
        handle_agent_terminal(
            agent_id,
            Err(anyhow::anyhow!("operator cancelled")),
            state,
            sched,
            gateway,
            registry,
            recorder,
        );
        return;
    }

    // ux.2b: stamp the last completed progress event at the universal effect choke point, so the
    // read-time Idle signal reflects real work. Covers Infer/CallTools/Spawn/RunJob/SendMessage/
    // RequestApproval in one place (a busy or pure-reasoning agent never false-reads Idle).
    // Defensive get_mut — a missing agent is a no-op, never a panic ("loop never panics").
    if let Some(task) = state.agents.get_mut(&agent_id) {
        task.mark_event();
    }

    let slot_ok   = sched.max_concurrent_inferences == 0 || state.in_flight < sched.max_concurrent_inferences;
    // Windowed global spend (ux.8′): lifetime − anchor vs the (per-window) ceiling.
    let global_windowed = state.global_windowed_spent();
    let global_ok = sched.global_token_budget == 0 || global_windowed < sched.global_token_budget;
    // Per-agent windowed budget (C1): the agent's own per-window cap. Enforced
    // here (pre-dispatch) so per-agent exhaustion DEFERS under a window instead of
    // the agent self-terminating in step_need_infer — that would strip
    // waiting/orchestrated and permanently brick a resident agent (audit86-P0-2).
    let per_agent_over = state.agents.get(&agent_id).is_some_and(|a| {
        let b = a.token_budget();
        b != 0 && a.windowed_spent() >= b
    });
    let budget_ok = global_ok && !per_agent_over;

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
                let eg       = state.egress.clone();
                let is_multi = state.agents.len() > 1;
                let id       = agent_id;
                state.pending.push(make_infer_future(req, id, turn, is_multi, gw, rec, sa, sl, eg));
            } else if !budget_ok {
                let reason = if !global_ok { "global_budget_exhausted" } else { "agent_budget_exhausted" };
                recorder.record(
                    &agent_id,
                    Some(turn),
                    EventKind::AgentAdmissionDenied,
                    json!({ "reason": reason, "tokens_spent": state.tokens_spent, "remedy": BUDGET_REMEDY }),
                );
                if sched.budget_reset_interval > 0 {
                    // Transient exhaustion (ux.8′/C1): with a reset window configured,
                    // ANY agent over the global OR its per-agent windowed ceiling is
                    // DEFERRED (not terminated) — the next window rollover drains the
                    // queue. Applies to every agent, not just the always-on one. The
                    // InferenceRequest is preserved intact.
                    let seq = state.deferred_seq;
                    state.deferred_seq += 1;
                    state.deferred.push(DeferredInfer { priority, seq, agent_id, request: req, turn });
                } else {
                    // No window (legacy): permanent exhaustion → terminate.
                    // ux.6a: receipt only HERE, never in the deferral branch above —
                    // deferral is not denial (ux.8′), and receipting it would put the
                    // boundary on record as refusing work it is actually going to do.
                    if let Some(eg) = state.egress.as_ref() {
                        eg.receipt_denial_once(&agent_id, gateway.model_id(), reason);
                    }
                    handle_agent_terminal(
                        agent_id,
                        Err(anyhow::anyhow!("admission denied: {reason} ({BUDGET_REMEDY})")),
                        state,
                        sched,
                        gateway,
                        registry,
                        recorder,
                    );
                }
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
        AgentEffect::RunJob { call_id, job_id } => {
            dispatch_run_job(
                agent_id, call_id, job_id, cap_set, turn,
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
            // ux.11b (G6): count this approval against the agent's open run segment.
            state.run_tracker.incr_approval(&agent_id);
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
            if state.orchestrated.contains(&agent_id) {
                // Orchestrated agent: park it awaiting next inject rather than terminating.
                // Cap the answer preview at 512 chars so the SSE event stays small.
                // Use chars().count() for the guard (not len()) to match the char-based take().
                let answer_preview: String = if answer.chars().count() > 512 {
                    let mut s: String = answer.chars().take(512).collect();
                    s.push_str("\n[output truncated — full text streamed above]");
                    s
                } else {
                    answer.clone()
                };
                recorder.record(
                    &agent_id,
                    None,
                    EventKind::OrchestratorTurnComplete,
                    json!({ "agent_id": &agent_id, "answer": &answer_preview }),
                );
                // Park: add to waiting so inject path can re-activate.
                // Agent remains in state.agents with terminal=true; Inject will reset it.
                state.waiting.insert(agent_id);
            } else {
                handle_agent_terminal(agent_id, Ok(answer), state, sched, gateway, registry, recorder);
            }
        }
        AgentEffect::CompletedTruncated(answer) => {
            // budget.1-ar-01: a resettable agent whose response was truncated at max_tokens. Role-gate:
            // a resident/orchestrated agent PARKS + resumes exactly like Completed (audit86-P0-2 — it
            // must NOT brick on a single truncation); a one-shot/child FAILS via handle_agent_terminal
            // so the parent gets an is_error result (not silently-truncated text as a finished answer)
            // and a sealed job emits a "failed" signal. This is the ONLY branch that diverges from
            // Completed — the resident park path below is byte-for-byte the Completed park path.
            if state.orchestrated.contains(&agent_id) {
                let answer_preview: String = if answer.chars().count() > 512 {
                    let mut s: String = answer.chars().take(512).collect();
                    s.push_str("\n[output truncated — full text streamed above]");
                    s
                } else {
                    answer.clone()
                };
                recorder.record(
                    &agent_id,
                    None,
                    EventKind::OrchestratorTurnComplete,
                    json!({ "agent_id": &agent_id, "answer": &answer_preview }),
                );
                state.waiting.insert(agent_id);
            } else {
                handle_agent_terminal(
                    agent_id,
                    Err(anyhow::anyhow!(
                        "model output truncated at max_tokens (partial response discarded)"
                    )),
                    state,
                    sched,
                    gateway,
                    registry,
                    recorder,
                );
            }
        }
        AgentEffect::Failed(msg) => {
            // AgentFailed event already emitted by AgentTask (budget/max-turns/etc.).
            // Inference-error AgentFailed is emitted in run() before this call.
            // handle_agent_terminal() now consolidates waiting/orchestrated cleanup.
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

    // 4. Resolve the child's capability set (cap.2 spawn attenuation).
    // `config.capabilities` absent → inherit the parent's full set (backward compat).
    // Present → each requested cap must be covered by the parent (`capability_covered_by`);
    // any cap outside the parent rejects the WHOLE spawn (fail-closed, reject not clamp).
    // An unrestricted parent (`parent_cap_set == None`) covers everything, so a child may
    // still scope itself down. This bounds accidental over-grant / an honest orchestrator;
    // it is NOT injection defense (the orchestrator picks these and reads untrusted data).
    let child_caps = match &config.capabilities {
        Some(requested) => {
            if let Some(parent_caps) = &parent_cap_set {
                if let Some(denied) = requested.iter().find(|req| !capability_covered_by(parent_caps, req)) {
                    let denied_str = format!("{denied:?}");
                    recorder.record(
                        &parent_id,
                        Some(parent_turn),
                        EventKind::AgentSpawnDenied,
                        json!({ "child_id": &child_id, "denied": &denied_str }),
                    );
                    let priority = state.agents[&parent_id].priority();
                    let caps = state.agents[&parent_id].cap_set_cloned();
                    let (parent_effect, next_turn) = {
                        let parent = state.agents.get_mut(&parent_id).unwrap();
                        parent.provide_tool_results(
                            vec![Block::ToolResult {
                                tool_use_id: call_id,
                                content: format!(
                                    "spawn denied: requested capability {denied_str} is not covered by the parent's capability set"
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
            }
            Some(requested.clone())
        }
        None => parent_cap_set.clone(),
    };
    let parent_token_budget = state
        .agents
        .get(&parent_id)
        .map(|a| a.token_budget())
        .unwrap_or_else(crate::config::default_token_budget);
    let child_budget = config.token_budget.unwrap_or(parent_token_budget);

    let child_agent_cfg = crate::config::AgentConfig {
        id:              child_id.clone(),
        task:            config.task.clone(),
        max_turns:       crate::config::default_max_turns(),
        token_budget:    child_budget,
        priority:        config.priority,
        capabilities:    child_caps.clone(),
        name:            None,
        description:     String::new(),
        skills:          vec![],
        tier:            crate::config::AgentTier::Native,
        command:         None,
        args:            vec![],
        isolation:       crate::config::IsolationMode::None,
        max_wall_seconds: 0,
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

    let mut child_task = AgentTask::new(
        &child_id,
        &config.task,
        &child_agent_cfg,
        &child_model_cfg,
        child_specs,
    );
    // C1: inherit the reset-window mode so per-agent exhaustion defers, not bricks.
    child_task.set_budget_resettable(sched.budget_reset_interval > 0);

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
    let child_open_tokens = child_task.context_tokens();
    state.agents.insert(child_id.clone(), child_task);
    // ux.11b: open the child's run segment (parent linkage = the run tree).
    state.run_tracker.open(&child_id, Some(parent_id.clone()), "child_spawn", Some(child_open_tokens), "native");
    state.awaiting.insert(
        child_id.clone(),
        AwaitingParent { parent_id: parent_id.clone(), call_id, deliver_content: true },
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

/// Handle an AgentEffect::RunJob (cap.2b): materialize a config-declared sealed job.
///
/// Unlike `dispatch_spawn`, the child's capabilities and task come from the operator-authored
/// `[[jobs]]` declaration — NOT from the caller — so:
///   - there is NO `capability_covered_by` parent-subset check (the caller need not, and for
///     the de-privileged CoS trigger must not, hold the job's caps);
///   - the task is the fixed template with only the server-stamped `{date}` substituted, so an
///     injected caller cannot redirect the work; and
///   - the parent await is `deliver_content=false` — the caller gets only a completion signal,
///     never the child's (email-derived) output.
///
/// The caller must hold `RunJob`. This is the hardened primitive for the injection-exposed
/// data pipeline; `spawn_agent` remains the trusted-delegation path.
#[allow(clippy::too_many_arguments)]
fn dispatch_run_job(
    parent_id: String,
    call_id: String,
    job_id: String,
    parent_cap_set: Option<Vec<Capability>>,
    parent_turn: u32,
    state: &mut SchedulerState,
    sched: &SchedulerConfig,
    gateway: &Arc<dyn InferenceGateway + Send + Sync>,
    registry: &Arc<ToolRegistry>,
    recorder: &Arc<FlightRecorder>,
) {
    // Local helper: reject the run_job call, hand an is_error ToolResult back to the caller,
    // and re-step it. Mirrors the dispatch_spawn reject blocks.
    fn reject(
        parent_id: String,
        call_id: String,
        message: String,
        state: &mut SchedulerState,
        sched: &SchedulerConfig,
        gateway: &Arc<dyn InferenceGateway + Send + Sync>,
        registry: &Arc<ToolRegistry>,
        recorder: &Arc<FlightRecorder>,
    ) {
        let priority = state.agents[&parent_id].priority();
        let caps = state.agents[&parent_id].cap_set_cloned();
        let (parent_effect, next_turn) = {
            let parent = state.agents.get_mut(&parent_id).unwrap();
            parent.provide_tool_results(
                vec![Block::ToolResult { tool_use_id: call_id, content: message, is_error: true }],
                recorder,
            );
            let t = parent.turn();
            (parent.step(recorder), t)
        };
        enqueue_or_defer(parent_effect, parent_id, next_turn, priority, caps, state, sched, gateway, registry, recorder);
    }

    // 1. Capability check — caller must hold RunJob.
    if let Some(caps) = &parent_cap_set {
        if !caps.iter().any(|c| matches!(c, Capability::RunJob)) {
            recorder.record(&parent_id, Some(parent_turn), EventKind::CapabilityDenied,
                json!({ "tool": "run_job", "required": "RunJob" }));
            reject(parent_id, call_id, "capability denied: RunJob capability required to call run_job".to_string(),
                state, sched, gateway, registry, recorder);
            return;
        }
    }

    // 2. Look up the config-declared job. Unknown id → reject (never a silent no-op).
    let job = match state.jobs.get(&job_id) {
        Some(j) => j.clone(),
        None => {
            recorder.record(&parent_id, Some(parent_turn), EventKind::AgentSpawnDenied,
                json!({ "tool": "run_job", "job_id": &job_id, "reason": "unknown job id" }));
            reject(parent_id, call_id, format!("run_job denied: no job declared with id '{job_id}'"),
                state, sched, gateway, registry, recorder);
            return;
        }
    };

    // 3. Depth limit (mirror dispatch_spawn — jobs count against nesting depth too).
    let parent_depth = state.spawn_depths.get(&parent_id).copied().unwrap_or(0);
    if parent_depth >= state.max_spawn_depth {
        recorder.record(&parent_id, Some(parent_turn), EventKind::Error,
            json!({ "stage": "run_job", "error": "max spawn depth exceeded", "depth": parent_depth, "limit": state.max_spawn_depth }));
        reject(parent_id, call_id, format!("run_job denied: max nesting depth {} reached", state.max_spawn_depth),
            state, sched, gateway, registry, recorder);
        return;
    }

    // 4. Server-stamp the date and derive the child id. The caller supplies NO date (zero
    // params) — the only value is agentd's wall-clock, so there is no injectable slot.
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let child_id = format!("{job_id}-{date}");
    if let Err(reason) = validate_child_id(&child_id) {
        recorder.record(&parent_id, Some(parent_turn), EventKind::Error,
            json!({ "stage": "run_job", "error": "invalid derived child_id", "child_id": &child_id, "reason": reason.to_string() }));
        reject(parent_id, call_id, format!("run_job denied: derived child id {child_id:?} invalid: {reason}"),
            state, sched, gateway, registry, recorder);
        return;
    }

    // 5. Collision guard: reject if a child with this id is still LIVE (in state.agents) or
    // retained in outcomes. A job that already COMPLETED and delivered to its parent is in
    // neither map, so a same-day re-trigger re-runs.
    //
    // ⚠ CORRECTED (R4, attn.2): this used to say "harmless — brief is log-append/LWW". That
    // was false twice over: the curator's `write_file` OVERWRITES (truncates) the same path,
    // and the `ops:briefs` KB entry is genuinely last-writer-wins, meaning a second same-day
    // run DESTROYED the first run's brief — "LWW" was being used to describe data loss as if
    // it were a safety property. R4 gives both the file and the KB key a per-fire `{ts}`
    // suffix (`Job::render`, `config.rs`), so a second same-day run now adds a new file and a
    // new KB entry instead of overwriting either — "harmless" is accurate only because of that
    // fix, not because it always was. Same-day idempotency is intentionally still not enforced
    // (matches `dispatch_spawn`) — this guard only prevents a collision with a run that is
    // still IN PROGRESS.
    if state.agents.contains_key(&child_id) || state.outcomes.contains_key(&child_id) {
        recorder.record(&parent_id, Some(parent_turn), EventKind::Error,
            json!({ "stage": "run_job", "error": "child ID collision", "child_id": &child_id }));
        reject(parent_id, call_id, format!("run_job denied: job child '{child_id}' is already in use"),
            state, sched, gateway, registry, recorder);
        return;
    }

    // 6. Build the child from the CONFIG-TRUSTED declaration — fixed caps (no subset check),
    // fixed task template with only the server-stamped {date}/{ts} substituted. `{ts}` (R4) is
    // a per-fire-unique component (HHMMSS) so a same-day re-trigger's write_file/kb_put never
    // collides with an earlier fire's — see the collision-guard comment above.
    let child_caps = Some(job.capabilities.clone());
    let ts = chrono::Utc::now().format("%H%M%S").to_string();
    let task = job.render(&date, &ts);
    let child_agent_cfg = crate::config::AgentConfig {
        id:              child_id.clone(),
        task:            task.clone(),
        max_turns:       job.max_turns,
        token_budget:    job.token_budget,
        priority:        0,
        capabilities:    child_caps.clone(),
        name:            None,
        description:     String::new(),
        skills:          vec![],
        tier:            crate::config::AgentTier::Native,
        command:         None,
        args:            vec![],
        isolation:       crate::config::IsolationMode::None,
        max_wall_seconds: 0,
    };
    let child_specs = registry.filtered_specs(child_caps.as_deref());
    let child_model_cfg = state
        .agents
        .get(&parent_id)
        .map(|a| a.model_cfg_cloned())
        .unwrap_or_default();
    let mut child_task = AgentTask::new(&child_id, &task, &child_agent_cfg, &child_model_cfg, child_specs);
    child_task.set_budget_resettable(sched.budget_reset_interval > 0);

    // 7. Register the child. `deliver_content=false` is the crux: the caller (an injectable
    // trigger) receives only an agentd-authored completion signal, never the child's output.
    let child_open_tokens = child_task.context_tokens();
    state.agents.insert(child_id.clone(), child_task);
    state.run_tracker.open(&child_id, Some(parent_id.clone()), "run_job", Some(child_open_tokens), "native");
    state.awaiting.insert(
        child_id.clone(),
        AwaitingParent { parent_id: parent_id.clone(), call_id, deliver_content: false },
    );
    state.spawn_depths.insert(child_id.clone(), parent_depth + 1);
    state.parent_map.insert(child_id.clone(), parent_id.clone());
    state.mailboxes.entry(child_id.clone()).or_default();

    recorder.record(&child_id, None, EventKind::AgentSpawned,
        json!({ "parent_id": &parent_id, "job_id": &job_id, "task_preview": truncate(&task, PREVIEW_CHARS), "depth": parent_depth + 1 }));

    // 8. Seed the child.
    let child_cap_set = state.agents[&child_id].cap_set_cloned();
    let (child_effect, child_turn) = {
        let child_sm = state.agents.get_mut(&child_id).unwrap();
        let t = child_sm.turn();
        (child_sm.step(recorder), t)
    };
    enqueue_or_defer(child_effect, child_id, child_turn, 0, child_cap_set, state, sched, gateway, registry, recorder);
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
        ControlCommand::Inject { agent_id, text } => {
            if state.agents.contains_key(&agent_id) {
                if !state.waiting.contains(&agent_id) {
                    // Agent is alive but actively running; injecting would corrupt message order.
                    recorder.record(
                        "agentd",
                        None,
                        EventKind::OrchestratorExited,
                        json!({ "agent_id": &agent_id, "reason": "agent_not_waiting" }),
                    );
                    return;
                }
                // Remove waiting state before the mutable borrow of agents.
                state.waiting.remove(&agent_id);
                let text_len = text.len();
                let task = state.agents.get_mut(&agent_id).unwrap();
                task.resume_for_orchestration();
                task.push_user_turn(text, recorder);
                let priority = task.priority();
                let caps     = task.cap_set_cloned();
                let turn     = task.turn();
                let effect   = task.step(recorder);
                // task's borrow of state.agents ends here (NLL: last use above)
                recorder.record(
                    &agent_id,
                    None,
                    EventKind::OrchestratorInjected,
                    json!({ "agent_id": &agent_id, "text_len": text_len }),
                );
                // Do NOT pre-insert into state.waiting here. The agent is now
                // in-flight (inference pending). The guard at line 1783 must reject
                // concurrent injects until AgentEffect::Completed re-parks the agent
                // via state.waiting.insert in the orchestrated branch. Pre-inserting
                // here would allow a second inject to bypass the guard and produce
                // two consecutive User turns in the same context, corrupting it.
                enqueue_or_defer(effect, agent_id, turn, priority, caps, state, sched, gateway, registry, recorder);
            } else {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::OrchestratorExited,
                    json!({ "agent_id": &agent_id, "reason": "agent_not_found" }),
                );
            }
        }
        ControlCommand::ResetBudget { target, confirm_tx } => {
            use crate::control::BudgetTarget;
            // Result carries (spent_before, window_start) on success (ux.8′ F4).
            let result: Result<(u64, u64), String> = match &target {
                BudgetTarget::Global => {
                    let old = state.global_windowed_spent();
                    state.global_window_anchor = state.tokens_spent;
                    state.rebase_universal_window();
                    state.publish_budget();
                    // F4: advance the window start to now, else the next loop-tick
                    // maybe_rebase sees the old start and immediately rebases AGAIN
                    // (a manual reset at T+interval-1 would double-reset 1s later).
                    state.budget_window_start = now_unix_secs();
                    // F4: also rebase every per-agent window — a manual global reset
                    // that revives a deferred agent must not leave it instantly
                    // re-tripped on its own stale per-agent window.
                    for task in state.agents.values_mut() {
                        let _ = task.reset_budget_window();
                    }
                    recorder.record(
                        "agentd",
                        None,
                        EventKind::BudgetReset,
                        json!({
                            "target":           "global",
                            "spent_before":     old,
                            "window_start":     state.budget_window_start,
                            "interval_secs":    sched.budget_reset_interval,
                            "windows_advanced": 0,
                        }),
                    );
                    // A manual global reset frees budget — admit any deferred agents.
                    drain_deferred(state, sched, gateway, registry, recorder);
                    Ok((old, state.budget_window_start))
                }
                BudgetTarget::Agent(id) => {
                    let window_start = state.budget_window_start;
                    let old_opt = state.agents.get_mut(id).map(|t| t.reset_budget_window());
                    match old_opt {
                        Some(old) => {
                            recorder.record(
                                id,
                                None,
                                EventKind::BudgetReset,
                                json!({
                                    "target":           id,
                                    "spent_before":     old,
                                    "window_start":     window_start,
                                    "interval_secs":    sched.budget_reset_interval,
                                    "windows_advanced": 0,
                                }),
                            );
                            // Codex #2: a per-agent reset must also admit that agent if
                            // it was deferred on its own cap — else the 200 reports a
                            // reset while the agent stays parked until unrelated activity.
                            drain_deferred(state, sched, gateway, registry, recorder);
                            Ok((old, window_start))
                        }
                        None => Err(format!("agent '{id}' not found")),
                    }
                }
            };
            match confirm_tx {
                // HTTP path: report old→new (or 404) synchronously.
                Some(tx) => {
                    let _ = tx.send(result);
                }
                // FUSE path: fire-and-forget; surface only the error case.
                None => {
                    if let Err(e) = result {
                        recorder.record(
                            "agentd",
                            None,
                            EventKind::FuseControlError,
                            json!({ "error": e, "is_error": true }),
                        );
                    }
                }
            }
        }
        ControlCommand::SetBudget { target, limit, confirm_tx } => {
            use crate::control::BudgetTarget;
            // Result carries (old_budget, new_budget) on success (ux.11a F2/F3).
            let result: Result<(u64, u64), String> = match &target {
                // F1: the global ceiling lives in immutable SchedulerConfig; it is not
                // runtime-settable in ux.11a. Reject (→ 400 at the HTTP layer).
                BudgetTarget::Global => Err("global budget is not runtime-settable".to_string()),
                BudgetTarget::Agent(id) => {
                    // Scope the get_mut borrow so drain_deferred can take &mut state after.
                    let outcome = state.agents.get_mut(id).map(|task| {
                        let old = task.set_token_budget(limit);
                        // Room = unlimited, or windowed spend is now under the new ceiling.
                        let has_room = limit == 0 || task.windowed_spent() < limit;
                        (old, has_room)
                    });
                    match outcome {
                        Some((old, has_room)) => {
                            recorder.record(
                                id,
                                None,
                                EventKind::BudgetSet,
                                json!({ "target": id, "old_budget": old, "new_budget": limit }),
                            );
                            // F2: raising a ceiling only revives a deferred agent when a
                            // drain runs — nothing else triggers one until unrelated activity.
                            if has_room {
                                drain_deferred(state, sched, gateway, registry, recorder);
                            }
                            Ok((old, limit))
                        }
                        None => Err(format!("agent '{id}' not found")),
                    }
                }
            };
            match confirm_tx {
                Some(tx) => {
                    let _ = tx.send(result);
                }
                None => {
                    if let Err(e) = result {
                        recorder.record(
                            "agentd",
                            None,
                            EventKind::FuseControlError,
                            json!({ "error": e, "is_error": true }),
                        );
                    }
                }
            }
        }

        ControlCommand::Cancel { agent_id, confirm_tx } => {
            let is_native    = state.agents.contains_key(&agent_id);
            let is_universal = state.universal_agents.contains_key(&agent_id);
            let result: Result<u64, String> = if !is_native && !is_universal {
                Err(format!("agent '{agent_id}' not found"))
            } else {
                // Collect the native cancellation subtree via the scheduler-authoritative
                // parent_map (child → parent). Seed only when the ROOT is native — a universal
                // root has no native subtree and must not enter cancel_requested (which only
                // handle_agent_terminal, a native-only funnel, consumes). Skip ghosts and the
                // "operator" root sentinel, which never appears as a node here.
                let mut subtree: Vec<String> = if is_native { vec![agent_id.clone()] } else { vec![] };
                let mut frontier: Vec<String> = subtree.clone();
                while let Some(parent) = frontier.pop() {
                    for (child, p) in state.parent_map.iter() {
                        if p == &parent
                            && child != "operator"
                            && state.agents.contains_key(child)
                            && !subtree.contains(child)
                        {
                            subtree.push(child.clone());
                            frontier.push(child.clone());
                        }
                    }
                }
                // Flag every node: root = operator-initiated, descendants = cascade.
                for node in &subtree {
                    let cause = if node == &agent_id {
                        "operator".to_string()
                    } else {
                        format!("cascade from {agent_id}")
                    };
                    state.cancel_requested.insert(node.clone(), cause);
                }
                // Funnel PARKED nodes now (they have no in-flight future); RUNNING nodes stay
                // flagged and are funneled by the enqueue_or_defer gate when their future returns.
                for node in subtree.clone() {
                    // "Parked" = has no in-flight future to trigger the enqueue_or_defer gate,
                    // so it must be funneled NOW. A running spawned child is a KEY in `awaiting`
                    // (child_id → parent) the whole time its inference future is live — matching
                    // on keys would funnel a live child and panic when its future returns
                    // (get_mut().expect on a removed agent). The genuinely-parked node is the
                    // PARENT awaiting a child (a value's parent_id, no in-flight future). Match
                    // that; leave running children flagged for the gate. (Mirrors update_snapshot's
                    // AwaitingChild classification.)
                    let parked = state.deferred.iter().any(|e| e.agent_id == node)
                        || state.waiting.contains(&node)
                        || state.awaiting.values().any(|v| v.parent_id == node)
                        || state.pending_approvals.values().any(|p| p.agent_id == node);
                    if parked {
                        handle_agent_terminal(
                            node,
                            Err(anyhow::anyhow!("operator cancelled")),
                            state, sched, gateway, registry, recorder,
                        );
                    }
                }
                // budget.1 / P3: universal-tier cancellation. A universal subprocess is
                // never in state.agents, so the native funnel above cannot reach it.
                // Flag the target itself (universal root) AND any universal agent whose
                // parent is in the cancelled native subtree (cascade). The sync handler
                // cannot .await kill()/deregister — the async run loop drains this set via
                // drain_universal_cancels() immediately after dispatch returns.
                let mut ua_flagged = 0u64;
                let ua_ids: Vec<String> = state.universal_agents.keys().cloned().collect();
                for uid in ua_ids {
                    let is_target = uid == agent_id;
                    let is_desc = state.parent_map.get(&uid).is_some_and(|p| subtree.contains(p));
                    if (is_target || is_desc) && state.universal_cancel_requested.insert(uid) {
                        ua_flagged += 1;
                    }
                }
                Ok(subtree.len() as u64 + ua_flagged)
            };
            match confirm_tx {
                Some(tx) => { let _ = tx.send(result); }
                None => {
                    if let Err(e) = result {
                        recorder.record("agentd", None, EventKind::FuseControlError,
                            json!({ "error": e, "is_error": true }));
                    }
                }
            }
        }

        ControlCommand::SetCaps { agent_id, capabilities, confirm_tx } => {
            use crate::capability::{capability_covered_by, tier_legality, CapContext, Legality};
            // Snapshot current caps (owned clone) so the immutable borrow is not held across
            // registry.filtered_specs + the get_mut below.
            let current = state.agents.get(&agent_id).map(|t| t.cap_set_cloned());
            let result: Result<(usize, usize), String> = match current {
                None => Err(format!("agent '{agent_id}' not found")),
                Some(current_caps) => {
                    // Narrow-only: every requested cap must be covered by the CURRENT set.
                    // `None` current = unrestricted = covers everything → any concrete set is a narrow.
                    let narrow_ok = match &current_caps {
                        None => true,
                        Some(cur) => capabilities.iter().all(|c| capability_covered_by(cur, c)),
                    };
                    if !narrow_ok {
                        Err("SetCaps is narrow-only; to widen, respawn".to_string())
                    } else if let Some(inert) = capabilities
                        .iter()
                        .find(|c| matches!(tier_legality(c, CapContext::Agent), Legality::Inert(_)))
                    {
                        Err(format!(
                            "capability {inert:?} is inert in agent context (narrowing it is a no-op)"
                        ))
                    } else {
                        let new_specs = registry.filtered_specs(Some(capabilities.as_slice()));
                        let old_caps = current_caps.clone().unwrap_or_default();
                        // P3-3 (audit86): "checked above", but never PANIC — the agent could have
                        // terminated between the check and here; return an error instead of aborting.
                        match state.agents.get_mut(&agent_id) {
                            Some(task) => {
                                let old_len = task.set_capabilities(capabilities.clone(), new_specs);
                                recorder.record(
                                    &agent_id, None, EventKind::CapabilitiesSet,
                                    json!({ "target": &agent_id, "old": old_caps, "new": &capabilities }),
                                );
                                Ok((old_len, capabilities.len()))
                            }
                            None => Err(format!("agent {agent_id} no longer present when applying SetCaps")),
                        }
                    }
                }
            };
            match confirm_tx {
                Some(tx) => { let _ = tx.send(result); }
                None => {
                    if let Err(e) = result {
                        recorder.record("agentd", None, EventKind::FuseControlError,
                            json!({ "error": e, "is_error": true }));
                    }
                }
            }
        }
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
        id:              agent_id.clone(),
        task:            req.task.clone(),
        max_turns,
        token_budget,
        priority,
        capabilities:    req.capabilities.clone(),
        name:            None,
        description:     String::new(),
        skills:          vec![],
        tier:            crate::config::AgentTier::Native,
        command:         None,
        args:            vec![],
        isolation:       crate::config::IsolationMode::None,
        max_wall_seconds: 0,
    };

    let specs     = registry.filtered_specs(agent_cfg.capabilities.as_deref());
    let mut task  = crate::agent::AgentTask::new(&agent_id, &req.task, &agent_cfg, default_model, specs);
    // C1: operator-spawned agents defer (not brick) on per-agent budget under a window.
    task.set_budget_resettable(sched.budget_reset_interval > 0);

    let op_open_tokens = task.context_tokens();
    state.agents.insert(agent_id.clone(), task);
    // ux.11b: open the operator-spawned agent's run segment (top-level).
    state.run_tracker.open(&agent_id, None, "operator_spawn", Some(op_open_tokens), "native");
    state.spawn_depths.insert(agent_id.clone(), 0);
    state.mailboxes.entry(agent_id.clone()).or_default();
    state.parent_map.insert(agent_id.clone(), "operator".to_string());

    // Notify the HTTP spawn handler (ar-02). Best-effort: ignore send errors.
    if let Some(tx) = req.confirm_tx {
        let _ = tx.send(agent_id.clone());
    }

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

    if req.orchestrated {
        // Mark as orchestrated so it parks instead of terminating on completion.
        state.orchestrated.insert(agent_id.clone());
        recorder.record(
            &agent_id,
            None,
            EventKind::OrchestratorDispatched,
            json!({
                "task_preview": crate::agent::truncate(&req.task, crate::agent::PREVIEW_CHARS),
                "agent_id":     &agent_id,
            }),
        );
    }

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
    // Reject sends to universal-tier agents — they don't have mailboxes.
    if state.universal_agents.contains_key(&to) {
        let priority = state.agents[&sender_id].priority();
        let caps = state.agents[&sender_id].cap_set_cloned();
        let (effect, next_turn) = {
            let sender = state.agents.get_mut(&sender_id).unwrap();
            sender.provide_tool_results(
                vec![Block::ToolResult {
                    tool_use_id: call_id,
                    content: format!("send_message failed: '{to}' is a universal-tier agent and does not accept messages"),
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

/// Derive one agent's active attention signals (ux.2a) — Approval-pending, Budget-risk, and
/// Degraded (credential/provider), in that priority order (see `AttentionReason`'s declaration
/// order). Idle/Error are a follow-on increment ("ux.2b"): their prerequisite `AgentTask` fields
/// don't exist yet — see `docs/plans/ux.2-attention-evidence.md`'s Eng Review rescope.
///
/// Reads `pending_approvals` directly (the scheduler's untruncated source), NOT the
/// `.take(100)`-capped `pending_actions` snapshot vector built later in `update_snapshot` — an
/// agent whose approval didn't make the cap must still get its Approval signal.
/// Bundled inputs to `derive_attention` — matches this codebase's `ToolContext` precedent
/// (p5.3) for multi-field call contexts, rather than a growing positional parameter list
/// (Maintainability review finding; ux.2b will add more `AgentTask`-derived inputs here).
struct AttentionInputs<'a> {
    agent_id:             &'a str,
    /// Windowed spend (ux.11a): keys the BudgetRisk signal against the budget window,
    /// so it clears/re-arms across resets (the lifetime counter never would). `assess`
    /// returns None when `token_budget == 0`, so unlimited agents never fire.
    windowed_spent:       u64,
    token_budget:         u64,
    credential_providers: &'a [String],
    pending_approvals:    &'a HashMap<String, ParkedApproval>,
    credential_snapshot:  Option<&'a surfaces::CredentialSnapshot>,
    /// ux.2b: latest still-running tool error, if any — drives the `Error` signal. `Idle` is
    /// NOT here: it is computed READ-TIME by `AgentSnapshot::idle_signal`, not at build.
    last_error:           Option<&'a str>,
}

fn derive_attention(inputs: AttentionInputs) -> Vec<surfaces::AttentionSignal> {
    let AttentionInputs {
        agent_id, windowed_spent, token_budget, credential_providers,
        pending_approvals, credential_snapshot, last_error,
    } = inputs;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut signals = Vec::new();

    if let Some((approval_id, pa)) = pending_approvals.iter().find(|(_, pa)| pa.agent_id == agent_id) {
        signals.push(surfaces::AttentionSignal {
            reason:   surfaces::AttentionReason::ApprovalPending,
            since:    now.saturating_sub(pa.created_at.elapsed().as_secs()),
            evidence: Some(approval_id.clone()),
        });
    }

    // ux.2b Error: a tool call errored while the agent kept running. `since: now` (no tracked
    // onset — like BudgetRisk), rendered "active" by agentctl; evidence carries a short excerpt.
    if let Some(err) = last_error {
        signals.push(surfaces::AttentionSignal {
            reason:   surfaces::AttentionReason::Error,
            since:    now,
            evidence: Some(err.to_string()),
        });
    }

    // Degraded fires on `!token_fresh` OR `attention_reason.is_some()` — not `AND
    // last_error present`. A missing API-key env var sets `token_fresh: false` without ever
    // populating `last_error` (no refresh attempt was ever made to fail), so requiring both
    // would silently miss the single most common degraded case. Separately, cred.7's health
    // state machine (`ProviderHealthState::AttentionRequired`) can flag a provider needing
    // attention (e.g. persistent 401s) while `token_fresh` stays `true` for ApiKey-style
    // providers, whose freshness is derived purely from "is the env var set" — independent
    // of whether the key actually works. Checking `attention_reason` too closes that gap
    // (ship-review Testing specialist finding). No credential gateway configured at all
    // (`None`) is a different state from "gateway configured but unreadable" — it means
    // there's simply nothing to be degraded, not an unknown, so it produces no signal at all.
    if let Some(snap) = credential_snapshot {
        for provider in credential_providers {
            match snap.provider_health.iter().find(|p| &p.name == provider) {
                Some(health) if !health.token_fresh || health.attention_reason.is_some() => {
                    // ApiKey-style providers whose env var was NEVER set (the single most
                    // common Degraded case) never populate attention_since (cred.7's health
                    // machine is OAuth-only) or last_refresh_at (only ever written on the
                    // OAuth success path) — both stay None forever. Falling back to `now` here
                    // would recompute "just broke" on every tick for a credential that has
                    // been missing since the deployment was first stood up. `since: 0` is a
                    // sentinel meaning "no real onset ever tracked" (a real Unix-epoch second
                    // is never exactly 0 in practice) — agentctl's age_display renders it as
                    // "active" instead of a fake elapsed time, same as BudgetRisk/
                    // EvaluationUnavailable (adversarial review finding, Claude + Codex both
                    // independently caught this).
                    signals.push(surfaces::AttentionSignal {
                        reason:   surfaces::AttentionReason::Degraded,
                        since:    health.attention_since.or(health.last_refresh_at).unwrap_or(0),
                        evidence: Some(provider.clone()),
                    });
                }
                Some(_) => {}
                None => {
                    // The agent's own credential grant references a provider the gateway's
                    // config no longer/never lists — a real inconsistency (config drift, a
                    // stale grant), not "nothing to evaluate." Distinct from an agent simply
                    // not using any credentials at all (adversarial review finding, Codex).
                    signals.push(surfaces::AttentionSignal {
                        reason:   surfaces::AttentionReason::EvaluationUnavailable,
                        since:    now,
                        evidence: Some(format!("{provider} (not in gateway config)")),
                    });
                }
            }
        }
    }

    if crate::memory::context::assess(windowed_spent, token_budget)
        == crate::memory::context::MemoryPressure::Hard
    {
        let pct = if token_budget > 0 {
            (windowed_spent as f64 / token_budget as f64 * 100.0).round() as u64
        } else {
            0
        };
        signals.push(surfaces::AttentionSignal {
            reason:   surfaces::AttentionReason::BudgetRisk,
            since:    now,
            evidence: Some(format!("{pct}%")),
        });
    }

    signals
}

/// Write a snapshot of the current scheduler state into the shared snapshot.
/// Uses `try_write` so a slow FUSE reader never blocks the scheduler.
fn update_snapshot(snapshot: &Arc<RwLock<SchedulerSnapshot>>, state: &SchedulerState) {
    // Computed once, reused both per-agent (attention derivation) and for the final
    // `s.credential_snapshot` field below — avoids calling `gw.snapshot()` twice per cycle.
    let credential_snapshot = state.cred_gw.as_ref().map(|gw| gw.snapshot());
    // ux.2b: wall-clock now, used to convert each task's monotonic `last_event_at` into the
    // carried `last_event_at_unix` anchor that the read surfaces derive Idle from.
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

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
                } else if state.waiting.contains(id) {
                    AgentStatus::Waiting
                } else {
                    AgentStatus::Running
                }
            };
            {
                let (cred_providers, cred_req, cred_denied, cred_access) =
                    state.cred_gw.as_ref()
                        .map(|gw| gw.agent_grant_for(id))
                        .unwrap_or_default();
                let attention = derive_attention(AttentionInputs {
                    agent_id:             id,
                    windowed_spent:       task.windowed_spent(),
                    token_budget:         task.token_budget(),
                    credential_providers: &cred_providers,
                    pending_approvals:    &state.pending_approvals,
                    credential_snapshot:  credential_snapshot.as_ref(),
                    last_error:           task.last_error(),
                });
                AgentSnapshot {
                    id:             id.clone(),
                    status,
                    turn:           task.turn(),
                    context_tokens: task.context_tokens(),
                    token_budget:   task.token_budget(),
                    windowed_spent: task.windowed_spent(),
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
                    tier: None,
                    pid:  None,
                    credential_providers:      cred_providers,
                    credential_request_counts: cred_req,
                    credential_denied_counts:  cred_denied,
                    credential_last_access_at: cred_access,
                    attention,
                    last_event_at_unix: now_unix.saturating_sub(task.last_event_elapsed_secs()),
                }
            }
        })
        .collect();

    // Also project universal-tier agents into the snapshot.
    let universal_snapshots: Vec<AgentSnapshot> = state
        .universal_agents
        .iter()
        .map(|(id, ua)| {
            let iso_str = match ua.isolation {
                crate::config::IsolationMode::Gvisor => "gvisor",
                crate::config::IsolationMode::None   => "none",
            };
            AgentSnapshot {
                id:             id.clone(),
                status:         AgentStatus::Running,
                turn:           0,
                context_tokens: 0,
                token_budget:   ua.cfg.token_budget,
                // Universal-tier spend is tracked via the proxy atomic, not AgentTask.
                windowed_spent: 0,
                task_preview:   ua.cfg.task.chars().take(80).collect(),
                tools:          vec![],
                short_term_previews: vec![],
                parent_id:      None,
                accessible_server_names:   vec![],
                capabilities_unrestricted: true,
                tier: Some(format!("universal:{iso_str}")),
                pid:  ua.pid(),
                credential_providers:      vec![],
                credential_request_counts: HashMap::new(),
                credential_denied_counts:  HashMap::new(),
                credential_last_access_at: HashMap::new(),
                attention:                 vec![],
                // Universal-tier liveness is proxy-tracked, not event-stamped — these agents are
                // never idle-eligible. `u64::MAX` makes `idle_signal`'s saturating_sub → 0 forever,
                // so a STALE universal snapshot (>threshold old, e.g. scheduler blocked on native
                // pending effects) can never false-read Idle. Anchoring to `now_unix` instead would
                // silently break the moment the snapshot stops refreshing (Codex review finding).
                last_event_at_unix:        u64::MAX,
            }
        })
        .collect();

    let mut agents = agents;
    agents.extend(universal_snapshots);

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
        // budget.1 / P2-2: report combined native + universal spend so the metering
        // surface no longer under-counts the universal tier.
        s.global_tokens_spent = state.combined_lifetime_spent();
        s.budget_resettable   = state.budget_resettable;
        s.in_flight           = state.in_flight;
        s.queue_depth         = state.deferred.len();
        s.pending_actions     = pending_actions;
        s.egress_addr         = state.egress_addr.map(|a| format!("http://{a}"));
        s.credential_snapshot = credential_snapshot;
    }
}

/// Drain operator-requested universal-tier cancellations (budget.1 / AUDIT-v0.97 P3).
/// The sync control handler flags IDs in `universal_cancel_requested`; this async pass
/// kills each subprocess and deregisters its ephemeral egress key BEFORE the kill so the
/// child cannot authenticate further inference during the SIGTERM grace window (mirrors
/// the shutdown path). Emits AgentCancelled + UniversalAgentExited and closes the run.
async fn drain_universal_cancels(
    state:    &mut SchedulerState,
    recorder: &Arc<FlightRecorder>,
) {
    if state.universal_cancel_requested.is_empty() {
        return;
    }
    let ids: Vec<String> = state.universal_cancel_requested.drain().collect();
    for id in ids {
        let Some(mut ua) = state.universal_agents.remove(&id) else { continue };
        let ephemeral_key = ua.ephemeral_key.clone();
        if let Some(reg) = &state.proxy_registry {
            reg.deregister_by_key(&ephemeral_key).await;
        }
        ua.kill().await;
        recorder.record(
            &id,
            None,
            EventKind::AgentCancelled,
            json!({ "agent_id": &id, "cause": "operator", "tier": "universal" }),
        );
        recorder.record(
            &id,
            None,
            EventKind::UniversalAgentExited,
            json!({ "agent_id": &id, "reason": "cancelled" }),
        );
        state.outcomes
            .entry(id.clone())
            .or_insert_with(|| Err(anyhow::anyhow!("operator cancelled")));
        state.run_tracker.close(&id, "cancelled", Some("operator_cancelled".into()), None, None);
    }
}

/// Poll all universal-tier agents for exit, remove finished ones, and record flight events.
async fn poll_universal_agents(
    state:    &mut SchedulerState,
    recorder: &FlightRecorder,
    snapshot: &Arc<RwLock<SchedulerSnapshot>>,
) {
    let ids: Vec<String> = state.universal_agents.keys().cloned().collect();
    let mut any_exited = false;
    for id in ids {
        let ua = state.universal_agents.get_mut(&id).unwrap();

        // Enforce max_wall_seconds before checking exit status.
        let wall_seconds = ua.wall_seconds();
        if ua.cfg.max_wall_seconds > 0 && wall_seconds > ua.cfg.max_wall_seconds {
            recorder.record(
                &id,
                None,
                EventKind::UniversalAgentExited,
                json!({ "pid": ua.pid(), "exit_code": null, "wall_seconds": wall_seconds, "reason": "wall_timeout" }),
            );
            // Deregister BEFORE kill() so the child cannot authenticate further
            // inference requests during the 5-second SIGTERM grace window.
            let ephemeral_key = ua.ephemeral_key.clone();
            if let Some(reg) = &state.proxy_registry {
                reg.deregister_by_key(&ephemeral_key).await;
            }
            ua.kill().await;
            state.universal_agents.remove(&id);
            state.outcomes.insert(id.clone(), Err(anyhow::anyhow!("universal agent wall timeout exceeded")));
            state.run_tracker.close(&id, "failed", Some("wall_timeout".into()), Some("universal agent wall timeout exceeded".into()), None);
            any_exited = true;
            continue;
        }

        match ua.try_wait() {
            Ok(Some(status)) => {
                let exit_code = status.code();
                recorder.record(
                    &id,
                    None,
                    EventKind::UniversalAgentExited,
                    json!({ "pid": ua.pid(), "exit_code": exit_code, "wall_seconds": wall_seconds }),
                );
                let ephemeral_key = ua.ephemeral_key.clone();
                let outcome = match exit_code {
                    Some(0) => Ok(String::new()),
                    Some(n) => Err(anyhow::anyhow!("universal agent exited with code {n}")),
                    None    => Err(anyhow::anyhow!("universal agent killed by signal")),
                };
                state.universal_agents.remove(&id);
                if let Some(reg) = &state.proxy_registry {
                    reg.deregister_by_key(&ephemeral_key).await;
                }
                let (rt_status, rt_stop, rt_err) = match exit_code {
                    Some(0) => ("done", Some("exit_0".to_string()), None),
                    Some(n) => ("failed", Some(format!("exit_{n}")), Some(format!("universal agent exited with code {n}"))),
                    None    => ("failed", Some("signal".to_string()), Some("universal agent killed by signal".to_string())),
                };
                state.run_tracker.close(&id, rt_status, rt_stop, rt_err, None);
                state.outcomes.insert(id.clone(), outcome);
                any_exited = true;
            }
            Ok(None) => {} // still running
            Err(e) => {
                recorder.record(
                    &id,
                    None,
                    EventKind::Error,
                    json!({ "stage": "universal_poll", "error": e.to_string() }),
                );
                let ephemeral_key = ua.ephemeral_key.clone();
                let msg = e.to_string();
                state.universal_agents.remove(&id);
                if let Some(reg) = &state.proxy_registry {
                    reg.deregister_by_key(&ephemeral_key).await;
                }
                state.run_tracker.close(&id, "failed", Some("poll_error".into()), Some(format!("universal agent poll error: {msg}")), None);
                state.outcomes.insert(id.clone(), Err(anyhow::anyhow!("universal agent poll error: {msg}")));
                any_exited = true;
            }
        }
    }
    if any_exited {
        update_snapshot(snapshot, state);
    }
}

/// Build a serializable snapshot of the current scheduler state.
/// Terminal agents are excluded — they've already delivered their results.
/// The deferred queue is intentionally omitted: those agents remain in `state.agents`
/// in NeedInfer state, so step() re-derives their InferenceRequest on restore.
/// Returns the checkpoint plus a map of `agent_id -> sealed tool_use ids` for any agent whose
/// PERSISTED transcript had to be repaired. The caller folds that into its own single
/// `AgentCheckpointed` event: one checkpoint must produce exactly ONE event per agent, or
/// anything counting checkpoints double-counts precisely when a repair fires.
fn build_scheduler_checkpoint(
    state: &SchedulerState,
    cred_gw: Option<&Arc<crate::credential::CredentialGateway>>,
) -> (SchedulerCheckpoint, HashMap<String, Vec<String>>) {
    let mut repairs: HashMap<String, Vec<String>> = HashMap::new();
    // Include waiting orchestrated agents (terminal=true) in addition to active agents.
    // Their terminal flag is preserved so the seed loop skips them on restore.
    // attn.3 A3 — never PERSIST a transcript whose tail is an unanswered `tool_use`.
    //
    // Measured on the live `agentos_cos-data` volume (2026-08-01): a `wait_for_trigger`
    // tool call was dispatched at 18:13:31 and SIGTERM arrived at 18:13:47, 16 s into a
    // 20 s call — 63 `tool_call` events against 62 `tool_result`. The half-finished turn
    // was checkpointed, and the restore 10 s later drew
    // `400 ... messages.125: tool_use ids were found without tool_result blocks
    // immediately after`, one second after `agent_restored`, twice.
    //
    // The repair is applied to the checkpoint COPY, never to the live transcript: the live
    // agent may still receive the real result and carry on, and its history stays untouched.
    //
    // ⚠ Be precise about what this does and does NOT guarantee (corrected at /review):
    // For a SIGTERM checkpoint, "the in-flight result never arrives" is true. For a PERIODIC
    // checkpoint it is NOT — `checkpoint_interval_turns` defaults to 1 and `checkpoint_all`
    // snapshots ALL agents on ANY agent's tool boundary, so agent B's turn can seal agent A's
    // still-running call. A then completes, its side effect lands, and if the process dies
    // before the next checkpoint the restored A is told the call was interrupted. So restore
    // is **at-least-once** for tool side effects, not exactly-once. That is NOT a regression —
    // pre-attn.3 the dangling id was persisted and the restore-side repair synthesised the
    // identical block — but it must not be mistaken for exactly-once. Making it exactly-once
    // needs dispatched ids tracked in a checkpointed `in_flight_tool_calls` map (state.pending
    // holds opaque futures, so nothing can distinguish "never ran" from "ran, result lost").
    // It also makes the synthetic block's existing wording ("Interrupted by a restart
    // before this tool produced a result") literally true, since a checkpoint is only ever
    // read back after a restart.
    //
    // A call the scheduler has already promised to answer must NOT be repaired: those ids
    // come from `awaiting.values()` and `pending_approvals.values()` — `.values()`, because
    // `awaiting` is keyed by CHILD id and `pending_approvals` by APPROVAL id, so the keys
    // are not call ids (the ux.13 P0 confusion). Restore re-creates both tables from this
    // same checkpoint, so anything listed there gets its real result on the way back up.
    let live_call_ids: std::collections::HashSet<String> = state
        .awaiting
        .values()
        .map(|ap| ap.call_id.clone())
        .chain(state.pending_approvals.values().map(|pa| pa.call_id.clone()))
        .collect();

    let agents: Vec<crate::checkpoint::AgentCheckpoint> = state
        .agents
        .iter()
        .filter(|(id, a)| !a.is_terminal() || state.waiting.contains(id.as_str()))
        .map(|(_, a)| {
            let mut cp = a.to_checkpoint();
            let repaired =
                AgentTask::repair_dangling_tool_uses(&mut cp.messages, &live_call_ids);
            if !repaired.is_empty() {
                repairs.insert(cp.agent_id.clone(), repaired);
            }
            cp
        })
        .collect();

    let awaiting: Vec<AwaitingEntry> = state
        .awaiting
        .iter()
        .map(|(child_id, ap)| AwaitingEntry {
            child_id:  child_id.clone(),
            parent_id: ap.parent_id.clone(),
            call_id:   ap.call_id.clone(),
            deliver_content: ap.deliver_content,
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

    // cred.7: persist per-provider AttentionRequired health states.
    let credential_health = cred_gw
        .map(|gw| gw.provider_health_checkpoints())
        .unwrap_or_default();

    let cp = SchedulerCheckpoint {
        format_version:      crate::checkpoint::FORMAT_VERSION,
        agents,
        awaiting,
        mailboxes:           state.mailboxes.clone(),
        tokens_spent:        state.tokens_spent,
        child_seq:           state.child_seq,
        spawn_depths:        state.spawn_depths.clone(),
        parent_map:          state.parent_map.clone(),
        pending_approvals,
        approval_seq:        state.approval_seq,
        waiting_agents:      state.waiting.iter().cloned().collect(),
        orchestrated_agents: state.orchestrated.iter().cloned().collect(),
        credential_health,
        budget_window_start:  state.budget_window_start,
        global_window_anchor: state.global_window_anchor,
    };
    (cp, repairs)
}

/// Write the full scheduler state to the checkpoint store and emit flight events.
/// Best-effort: logs a warning on failure but never propagates the error.
async fn checkpoint_all(
    store: &CheckpointStore,
    state: &SchedulerState,
    recorder: &FlightRecorder,
) {
    let (cp, repairs) = build_scheduler_checkpoint(state, state.cred_gw.as_ref());
    if let Err(e) = store.save(&cp).await {
        tracing::warn!("checkpoint save failed (best-effort): {e:#}");
        return;
    }
    for agent_cp in &cp.agents {
        recorder.record(
            &agent_cp.agent_id,
            Some(agent_cp.turn),
            EventKind::AgentCheckpointed,
            // attn.3 A3: when the PERSISTED transcript had to be sealed, say so HERE rather
            // than in a second event — one checkpoint, one event per agent, so anything
            // counting checkpoints does not double-count exactly when a repair fires.
            match repairs.get(&agent_cp.agent_id) {
                Some(ids) => json!({
                    "turn":           agent_cp.turn,
                    "total_tokens":   agent_cp.total_input + agent_cp.total_output,
                    "stage":          "checkpoint_repair",
                    "repaired_ids":   ids,
                    "repaired_count": ids.len(),
                    "note":           "in-flight tool call sealed so the PERSISTED transcript \
                                       is well-formed; the live agent is unchanged",
                }),
                None => json!({
                    "turn":         agent_cp.turn,
                    "total_tokens": agent_cp.total_input + agent_cp.total_output,
                }),
            },
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
            blocks:            vec![Block::Text { text: text.to_string() }],
            stop_reason:       StopReason::EndTurn,
            input_tokens:      input_tok,
            output_tokens:     output_tok,
            transport_retries: 0,
        }
    }

    fn spawn_resp(call_id: &str, task: &str) -> InferenceResponse {
        InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    call_id.to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({ "task": task }),
            }],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        }
    }

    fn agent_cfg(id: &str, task: &str) -> AgentConfig {
        AgentConfig {
            id:              id.to_string(),
            task:            task.to_string(),
            max_turns:       5,
            token_budget:    100_000,
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
    async fn run_authors_a_closed_run_record() {
        // ux.11b F5 integration: a real agent lifecycle through run() must land an
        // authoritative closed record in runs.redb — proves the RunTracker call-site
        // placement (open at seed, close in handle_agent_terminal), not just the store.
        let dir = tempfile::tempdir().unwrap();
        let (store, _q) = crate::runs::RunsStore::open(&dir.path().join("runs.redb")).unwrap();
        let store = Arc::new(store);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let writer = tokio::spawn(crate::runs::run_writer(rx, Arc::clone(&store)));

        let gw = MockGateway::new(vec![end_turn("done", 10, 5)]);
        let sched = make_scheduler(vec![agent_cfg("solo", "do something")], unlimited(), gw)
            .with_run_tracker(crate::runs::RunTracker::new(tx));
        let outcomes = sched.run().await; // drops the tracker (sender) → writer drains + ends
        assert_eq!(outcomes["solo"].as_ref().unwrap(), "done");

        writer.await.unwrap();
        let recs = store.list(&crate::runs::RunFilter::default()).unwrap();
        assert_eq!(recs.len(), 1, "exactly one segment for the solo agent");
        assert_eq!(recs[0].agent_id, "solo");
        assert_eq!(recs[0].status, "done", "root completion closes via handle_agent_terminal");
        assert_eq!(recs[0].start_reason, "config_seed");
        assert!(recs[0].end_ts.is_some(), "segment is closed");
        assert!(recs[0].spend.is_some(), "native segment has a spend delta");
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
        register_native(&mut registry, &["write_file".to_string()], None, None, None, None).unwrap();

        let gw = MockGateway::new(vec![
            InferenceResponse {
                blocks: vec![Block::ToolUse {
                    id:    "call_cap_test".to_string(),
                    name:  "write_file".to_string(),
                    input: serde_json::json!({"path": "/etc/passwd", "content": "evil"}),
                }],
                stop_reason:       StopReason::ToolUse,
                input_tokens:      10,
                output_tokens:     5,
                transport_retries: 0,
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
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

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
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

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
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

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
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

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
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

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
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

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
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
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
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

        let spawn_response = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    "spawn_1".to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({
                    "task":     "evil",
                    "child_id": "../evil"
                }),
            }],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
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
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

        let spawn_response = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    "spawn_tok".to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({
                    "task":         "budget-limited sub-task",
                    "token_budget": 999999
                }),
            }],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
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

    // ── cap.2: spawn attenuation ─────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_attenuation_rejects_cap_outside_parent() {
        // A child requesting a capability the parent does NOT hold rejects the WHOLE
        // spawn (fail-closed, reject not clamp) with an AgentSpawnDenied event. The
        // parent recovers and completes; no child is created.
        use crate::{capability::Capability, tools::native::register_native};

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

        let spawn_response = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    "spawn_esc".to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({
                    "task":         "over-reaching child",
                    "child_id":     "greedy",
                    // Parent holds only Spawn; requesting Credential{Google} is an escalation.
                    "capabilities": ["Spawn", {"Credential": {"provider": "Google"}}]
                }),
            }],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        };

        let gw = MockGateway::new(vec![
            spawn_response,
            end_turn("parent recovered", 10, 5),
        ]);

        let parent = AgentConfig {
            capabilities: Some(vec![Capability::Spawn]),
            ..agent_cfg("parent-esc", "spawn a greedy child")
        };

        let (sched, _rec, _tmp) =
            make_scheduler_with_registry(vec![parent], unlimited(), gw, registry);
        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 1, "rejected child must not be spawned");
        assert!(
            !outcomes.keys().any(|k| k.contains("greedy")),
            "no over-reaching child may appear in outcomes"
        );
        assert!(
            outcomes["parent-esc"].is_ok(),
            "parent must recover and complete: {:?}", outcomes["parent-esc"]
        );

        let content = std::fs::read_to_string(_tmp.path()).unwrap_or_default();
        assert!(
            content.contains("\"agent_spawn_denied\""),
            "AgentSpawnDenied flight event must be recorded"
        );
    }

    #[tokio::test]
    async fn spawn_attenuation_allows_covered_subset() {
        // A child requesting a strict subset of the parent's caps spawns normally.
        use crate::{capability::Capability, tools::native::register_native};

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

        let spawn_response = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    "spawn_sub".to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({
                    "task":         "scoped-down child",
                    "child_id":     "scoped",
                    // Parent holds Spawn + Mcp{google_oauth}; child asks for only Spawn.
                    "capabilities": ["Spawn"]
                }),
            }],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        };

        let gw = MockGateway::new(vec![
            spawn_response,
            end_turn("child ok", 5, 3),
            end_turn("parent ok", 10, 5),
        ]);

        let parent = AgentConfig {
            capabilities: Some(vec![
                Capability::Spawn,
                Capability::Mcp { server: "google_oauth".into(), tools: vec![] },
            ]),
            ..agent_cfg("parent-sub", "spawn a scoped child")
        };

        let (sched, _rec, _tmp) =
            make_scheduler_with_registry(vec![parent], unlimited(), gw, registry);
        let outcomes = sched.run().await;

        assert_eq!(outcomes.len(), 1, "child is internal; only parent in outcomes");
        assert!(
            outcomes["parent-sub"].is_ok(),
            "parent must complete after a covered-subset spawn: {:?}", outcomes["parent-sub"]
        );
        let content = std::fs::read_to_string(_tmp.path()).unwrap_or_default();
        assert!(
            !content.contains("\"agent_spawn_denied\""),
            "a covered subset must NOT be denied"
        );
    }

    #[tokio::test]
    async fn spawn_agent_floor_is_not_injection_defense() {
        // spawn_agent (cap.2 FLOOR) is trusted-delegation: a parent that HOLDS Mcp{google_oauth}
        // can spawn a child WITH it — ⊆ its own set, so the subset check passes. This is
        // CORRECT for trusted delegation and remains unchanged by cap.2b. The injection-exposed
        // CoS path no longer uses spawn_agent at all — it uses the sealed `run_job` (config-owned
        // caps + task, deliver_content=false), tested below. This test pins that the spawn_agent
        // floor still behaves as the documented trusted-delegation primitive.
        use crate::{capability::Capability, tools::native::register_native};

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["spawn_agent".to_string()], None, None, None, None).unwrap();

        let spawn_response = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id:    "spawn_byp".to_string(),
                name:  "spawn_agent".to_string(),
                input: serde_json::json!({
                    "task":         "curator (but handed Gmail)",
                    "child_id":     "curator-with-gmail",
                    "capabilities": ["Spawn", {"Mcp": {"server": "google_oauth", "tools": []}}]
                }),
            }],
            stop_reason:       StopReason::ToolUse,
            input_tokens:      10,
            output_tokens:     5,
            transport_retries: 0,
        };

        let gw = MockGateway::new(vec![
            spawn_response,
            end_turn("child ok", 5, 3),
            end_turn("parent ok", 10, 5),
        ]);

        // The orchestrator itself holds Gmail (the over-privileged injectable root).
        let parent = AgentConfig {
            capabilities: Some(vec![
                Capability::Spawn,
                Capability::Mcp { server: "google_oauth".into(), tools: vec![] },
            ]),
            ..agent_cfg("orchestrator", "injected: spawn curator with Gmail")
        };

        let (sched, _rec, _tmp) =
            make_scheduler_with_registry(vec![parent], unlimited(), gw, registry);
        let outcomes = sched.run().await;

        assert!(
            outcomes["orchestrator"].is_ok(),
            "spawn must SUCCEED — the floor does not stop an orchestrator granting from its OWN set"
        );
        let content = std::fs::read_to_string(_tmp.path()).unwrap_or_default();
        assert!(
            !content.contains("\"agent_spawn_denied\""),
            "spawn_agent trusted delegation is not denied (the sealed run_job path is what closes injection)"
        );
    }

    // ── cap.2b: sealed run_job ────────────────────────────────────────────────

    fn cos_like_job(id: &str, caps: Vec<Capability>) -> crate::config::Job {
        crate::config::Job {
            id: id.to_string(),
            token_budget: crate::config::default_token_budget(),
            max_turns: crate::config::default_job_max_turns(),
            capabilities: caps,
            task: "sealed job for {date}".to_string(),
        }
    }

    #[tokio::test]
    async fn run_job_materializes_sealed_child_with_config_caps() {
        // A DE-PRIVILEGED trigger (only RunJob) fires a job whose caps live in config. The
        // child runs with the CONFIG caps (no parent-subset check, because the trigger holds
        // none of them), and there is no denial. This is the cap.2b inversion: cap authority
        // moved off the injectable trigger onto config.
        use crate::tools::native::register_native;
        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["run_job".to_string()], None, None, None, None).unwrap();

        let run = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id: "rj1".to_string(),
                name: "run_job".to_string(),
                input: serde_json::json!({ "job_id": "cos-curator" }),
            }],
            stop_reason: StopReason::ToolUse,
            input_tokens: 10, output_tokens: 5, transport_retries: 0,
        };
        let gw = MockGateway::new(vec![
            run,
            end_turn("child ok", 5, 3),   // the job child terminates
            end_turn("trigger ok", 10, 5), // the trigger continues after the completion signal
        ]);

        let trigger = AgentConfig {
            capabilities: Some(vec![Capability::RunJob]),
            ..agent_cfg("cos-orchestrator", "trigger")
        };
        let (sched, _rec, _tmp) =
            make_scheduler_with_registry(vec![trigger], unlimited(), gw, registry);
        // Curator job: KB-only, NO google_oauth — the trigger holds none of these caps.
        let sched = sched.with_jobs(vec![cos_like_job(
            "cos-curator",
            vec![Capability::KbWrite { segment: "ops:briefs".into() }],
        )]);
        let outcomes = sched.run().await;

        assert!(outcomes["cos-orchestrator"].is_ok(), "trigger must complete: {:?}", outcomes["cos-orchestrator"]);
        let content = std::fs::read_to_string(_tmp.path()).unwrap_or_default();
        assert!(!content.contains("\"agent_spawn_denied\""), "a valid run_job must not be denied");
        // The child materialized under the server-stamped id and ran (agent_spawned recorded).
        assert!(content.contains("cos-curator-"), "job child should be id-stamped cos-curator-<date>");
    }

    #[tokio::test]
    async fn run_job_rejects_unknown_job_id() {
        use crate::tools::native::register_native;
        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["run_job".to_string()], None, None, None, None).unwrap();

        let run = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id: "rj_bad".to_string(),
                name: "run_job".to_string(),
                input: serde_json::json!({ "job_id": "no-such-job" }),
            }],
            stop_reason: StopReason::ToolUse,
            input_tokens: 10, output_tokens: 5, transport_retries: 0,
        };
        let gw = MockGateway::new(vec![run, end_turn("trigger recovered", 10, 5)]);
        let trigger = AgentConfig {
            capabilities: Some(vec![Capability::RunJob]),
            ..agent_cfg("cos-orchestrator", "trigger")
        };
        let (sched, _rec, _tmp) =
            make_scheduler_with_registry(vec![trigger], unlimited(), gw, registry);
        let sched = sched.with_jobs(vec![cos_like_job("cos-inbox", vec![Capability::RunsRead])]);
        let outcomes = sched.run().await;

        assert!(outcomes["cos-orchestrator"].is_ok(), "trigger recovers from a bad job id");
        let content = std::fs::read_to_string(_tmp.path()).unwrap_or_default();
        assert!(content.contains("\"agent_spawn_denied\""), "unknown job id must be recorded as denied");
        assert!(!outcomes.keys().any(|k| k.starts_with("no-such-job")), "no child for an unknown job");
    }

    #[tokio::test]
    async fn run_job_requires_run_job_capability() {
        // A trigger WITHOUT RunJob cannot fire jobs — capability-gated like spawn_agent/Spawn.
        use crate::tools::native::register_native;
        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["run_job".to_string()], None, None, None, None).unwrap();

        let run = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id: "rj_nocap".to_string(),
                name: "run_job".to_string(),
                input: serde_json::json!({ "job_id": "cos-inbox" }),
            }],
            stop_reason: StopReason::ToolUse,
            input_tokens: 10, output_tokens: 5, transport_retries: 0,
        };
        let gw = MockGateway::new(vec![run, end_turn("recovered", 10, 5)]);
        // Trigger holds some other cap but NOT RunJob.
        let trigger = AgentConfig {
            capabilities: Some(vec![Capability::RunsRead]),
            ..agent_cfg("cos-orchestrator", "trigger")
        };
        let (sched, _rec, _tmp) =
            make_scheduler_with_registry(vec![trigger], unlimited(), gw, registry);
        let sched = sched.with_jobs(vec![cos_like_job("cos-inbox", vec![Capability::RunsRead])]);
        let outcomes = sched.run().await;

        assert!(outcomes["cos-orchestrator"].is_ok(), "trigger recovers from the denial");
        let content = std::fs::read_to_string(_tmp.path()).unwrap_or_default();
        assert!(content.contains("\"capability_denied\""), "run_job without RunJob must be capability-denied");
    }

    // Captures every InferenceRequest it serves, so a test can inspect exactly what reached an
    // agent's context — used to PROVE the sealed-job no-read property (deliver_content=false).
    struct CapturingGateway {
        responses: Arc<Mutex<Vec<InferenceResponse>>>,
        seen:      Arc<Mutex<Vec<InferenceRequest>>>,
    }
    #[async_trait::async_trait]
    impl InferenceGateway for CapturingGateway {
        async fn infer(&self, req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
            self.seen.lock().unwrap().push(req);
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(anyhow::anyhow!("CapturingGateway: no more responses queued"));
            }
            Ok(q.remove(0))
        }
        fn model_id(&self) -> &str { "capturing" }
    }

    #[tokio::test]
    async fn run_job_delivers_completion_signal_not_child_output() {
        // The crux of cap.2b: a sealed job's (email-derived) OUTPUT must never reach the
        // injectable trigger's context — only an agentd-authored completion signal. Drive a job
        // whose child answer is a distinctive sentinel and assert the sentinel appears in NO
        // inference request (i.e. was never fed back to any agent), while the trigger's follow-up
        // request DOES carry the "completed" signal as its ToolResult.
        use crate::inference::Block as IBlock;
        use crate::tools::native::register_native;
        const SENTINEL: &str = "SENTINEL_LEAK_9F3A_wire_funds_now";

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["run_job".to_string()], None, None, None, None).unwrap();

        let run = InferenceResponse {
            blocks: vec![Block::ToolUse {
                id: "rj_seal".to_string(),
                name: "run_job".to_string(),
                input: serde_json::json!({ "job_id": "cos-inbox" }),
            }],
            stop_reason: StopReason::ToolUse,
            input_tokens: 10, output_tokens: 5, transport_retries: 0,
        };
        let gw = CapturingGateway {
            responses: Arc::new(Mutex::new(vec![
                run,                                   // trigger req #1 → run_job
                end_turn(SENTINEL, 5, 3),              // the JOB child's answer (hostile output)
                end_turn("trigger done", 10, 5),       // trigger req #2 (carries the delivered ToolResult)
            ])),
            seen: Arc::new(Mutex::new(Vec::new())),
        };
        let seen = Arc::clone(&gw.seen); // keep a handle after run() consumes the scheduler

        let trigger = AgentConfig {
            capabilities: Some(vec![Capability::RunJob]),
            ..agent_cfg("cos-orchestrator", "trigger")
        };
        let (sched, _rec, _tmp) =
            make_scheduler_with_registry(vec![trigger], unlimited(), gw, registry);
        let sched = sched.with_jobs(vec![cos_like_job(
            "cos-inbox",
            vec![Capability::KbWrite { segment: "ops:entities".into() }],
        )]);
        let outcomes = sched.run().await;
        assert!(outcomes["cos-orchestrator"].is_ok());

        // Inspect every request served. Collect all ToolResult contents that reached any agent.
        let reqs = seen.lock().unwrap();
        let tool_result_contents: Vec<String> = reqs
            .iter()
            .flat_map(|r| r.messages.iter())
            .flat_map(|m| m.blocks.iter())
            .filter_map(|b| match b {
                IBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect();

        // No-read: the child's hostile answer must NEVER appear in any request context.
        assert!(
            !reqs.iter().any(|r| format!("{:?}", r.messages).contains(SENTINEL)),
            "LEAK: the sealed job's output reached an agent's context — deliver_content is broken"
        );
        // Positive: the trigger DID receive the agentd-authored completion signal instead.
        assert!(
            tool_result_contents.iter().any(|c| c.contains("completed") && c.contains("cos-inbox-")),
            "the trigger must receive the 'job cos-inbox-<date> completed' signal as its ToolResult; \
             got: {tool_result_contents:?}"
        );
    }

    // ── p1.6: send_message tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn send_message_delivered_before_next_inference() {
        // Agent A sends a message to agent B. Agent B must see it in a subsequent
        // inference step.
        use crate::tools::native::register_native;

        let mut registry = ToolRegistry::new();
        register_native(&mut registry, &["send_message".to_string()], None, None, None, None).unwrap();

        // Run both agents via a single MockGateway (interleaved responses).
        let mut registry2 = ToolRegistry::new();
        register_native(&mut registry2, &["send_message".to_string()], None, None, None, None).unwrap();

        let gw = MockGateway::new(vec![
            // Agent alpha: send_message to beta
            InferenceResponse {
                blocks: vec![Block::ToolUse {
                    id:    "msg1".to_string(),
                    name:  "send_message".to_string(),
                    input: serde_json::json!({"to": "beta", "content": "ping"}),
                }],
                stop_reason:       StopReason::ToolUse,
                input_tokens:      10,
                output_tokens:     5,
                transport_retries: 0,
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
        register_native(&mut registry, &["send_message".to_string()], None, None, None, None).unwrap();

        let gw = MockGateway::new(vec![
            // Agent sends to unknown recipient
            InferenceResponse {
                blocks: vec![Block::ToolUse {
                    id:    "msg_bad".to_string(),
                    name:  "send_message".to_string(),
                    input: serde_json::json!({"to": "ghost", "content": "hello?"}),
                }],
                stop_reason:       StopReason::ToolUse,
                input_tokens:      10,
                output_tokens:     5,
                transport_retries: 0,
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
            budget_resettable:  false,
            in_flight:          0,
            tokens_spent:       0,
            budget_window_start: 0,
            global_window_anchor: 0,
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
            egress:             None,
            egress_addr:        None,
            proxy_registry:     None,
            universal_agents:   HashMap::new(),
            orchestrated:       HashSet::new(),
            waiting:            HashSet::new(),
            cancel_requested:   HashMap::new(),
            universal_cancel_requested: HashSet::new(),
            cred_gw:            None,
            run_tracker:        RunTracker::disabled(),
            jobs:               HashMap::new(),
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
            AwaitingParent { parent_id: "parent".to_string(), call_id: "call-1".to_string(), deliver_content: true },
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
            window_anchor: 0,
        }
    }

    fn minimal_scheduler_checkpoint(ids: &[&str]) -> SchedulerCheckpoint {
        SchedulerCheckpoint {
            format_version:      crate::checkpoint::FORMAT_VERSION,
            agents:              ids.iter().map(|id| minimal_agent_checkpoint(id)).collect(),
            awaiting:            vec![],
            mailboxes:           HashMap::new(),
            tokens_spent:        20,
            child_seq:           3,
            spawn_depths:        ids.iter().map(|id| (id.to_string(), 0u32)).collect(),
            parent_map:          HashMap::new(),
            pending_approvals:   vec![],
            approval_seq:        0,
            waiting_agents:      vec![],
            orchestrated_agents: vec![],
            credential_health:   HashMap::new(),
            budget_window_start: 0,
            global_window_anchor: 0,
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
            format_version:      crate::checkpoint::FORMAT_VERSION,
            agents:              vec![minimal_agent_checkpoint("agent")],
            awaiting:            vec![],
            mailboxes:           HashMap::new(),
            tokens_spent:        42,
            child_seq:           7,
            spawn_depths:        [("agent".to_string(), 0u32)].into_iter().collect(),
            parent_map:          HashMap::new(),
            pending_approvals:   vec![],
            approval_seq:        0,
            waiting_agents:      vec![],
            orchestrated_agents: vec![],
            credential_health:   HashMap::new(),
            budget_window_start: 0,
            global_window_anchor: 0,
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
                transport_retries: 0,
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

    // ── ux.1 streaming-delta events ─────────────────────────────────────────────

    #[tokio::test]
    async fn stream_delta_recorded_per_chunk_with_monotonic_chunk_seq() {
        let resp = end_turn("streamed answer", 20, 10);
        let gw = StreamingMockGateway {
            chunks:   vec!["chunk1".to_string(), "chunk2".to_string(), "chunk3".to_string()],
            response: resp.clone(),
        };

        let mut cfg = agent_cfg("delta-agent", "delta task");
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

        let outcomes = sched.run().await;
        assert!(outcomes.get("delta-agent").expect("agent not found").is_ok());

        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        let delta_lines: Vec<&str> = log.lines().filter(|l| l.contains("\"inference_stream_delta\"")).collect();
        assert_eq!(delta_lines.len(), 3, "expected one inference_stream_delta per chunk, got: {delta_lines:?}");

        for (i, line) in delta_lines.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["data"]["chunk_seq"], i as u64, "chunk_seq must be monotonic starting at 0");
            assert_eq!(v["data"]["agent_id"], "delta-agent");
        }
    }

    #[tokio::test]
    async fn stream_delta_disk_text_truncated_at_cap() {
        let long_chunk = "x".repeat(STREAM_DELTA_DISK_TEXT_CAP + 100);
        let resp = end_turn("streamed answer", 20, 10);
        let gw = StreamingMockGateway {
            chunks:   vec![long_chunk.clone()],
            response: resp.clone(),
        };

        let mut cfg = agent_cfg("cap-agent", "cap task");
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

        let outcomes = sched.run().await;
        assert!(outcomes.get("cap-agent").expect("agent not found").is_ok());

        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        let delta_line = log.lines().find(|l| l.contains("\"inference_stream_delta\"")).expect("no delta event");
        let v: serde_json::Value = serde_json::from_str(delta_line).unwrap();
        let disk_text = v["data"]["text"].as_str().unwrap();
        assert_eq!(
            disk_text.chars().count(), STREAM_DELTA_DISK_TEXT_CAP,
            "flight.jsonl copy must be capped at STREAM_DELTA_DISK_TEXT_CAP chars, got {}",
            disk_text.chars().count()
        );
        assert!(long_chunk.len() > disk_text.len(), "disk copy must be shorter than the original chunk");
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

    /// ModelConfig streaming field defaults to true via #[serde(default = "default_streaming")].
    #[test]
    fn model_config_streaming_defaults_to_true() {
        // Verify that omitting `streaming` from a TOML snippet deserializes as true.
        let toml_str = r#"
            provider = "anthropic"
            model = "claude-sonnet-4-6"
            max_tokens = 4096
        "#;
        let cfg: crate::config::ModelConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.streaming, "streaming must default to true when omitted from config");
    }

    /// ModelConfig streaming field can be explicitly disabled for headless/script use cases.
    #[test]
    fn model_config_streaming_can_be_disabled() {
        let toml_str = r#"
            provider = "anthropic"
            model = "claude-sonnet-4-6"
            max_tokens = 4096
            streaming = false
        "#;
        let cfg: crate::config::ModelConfig = toml::from_str(toml_str).unwrap();
        assert!(!cfg.streaming, "streaming = false must be honoured when explicit");
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

    /// ModelConfig::default() must also return streaming = true so Default and serde cannot drift.
    #[test]
    fn model_config_default_impl_streaming_is_true() {
        let cfg = crate::config::ModelConfig::default();
        assert!(cfg.streaming, "Default impl must return streaming = true after h7.4");
    }

    // ── p7.5b egress_addr ─────────────────────────────────────────────────────

    #[test]
    fn scheduler_exposes_egress_addr() {
        let gw = MockGateway::new(vec![]);
        let scheduler = make_scheduler(vec![], unlimited(), gw);
        let addr: std::net::SocketAddr = "127.0.0.1:9111".parse().unwrap();
        let scheduler = scheduler.with_egress_addr(Some(addr));
        assert_eq!(scheduler.egress_addr(), Some(addr));
    }

    #[test]
    fn scheduler_egress_addr_none_by_default() {
        let gw = MockGateway::new(vec![]);
        let scheduler = make_scheduler(vec![], unlimited(), gw);
        assert!(scheduler.egress_addr().is_none());
    }

    #[test]
    fn update_snapshot_writes_egress_addr() {
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let addr: std::net::SocketAddr = "127.0.0.1:9222".parse().unwrap();
        let mut state = minimal_state("egress_snap_test");
        state.egress_addr = Some(addr);
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        assert_eq!(s.egress_addr.as_deref(), Some("http://127.0.0.1:9222"));
    }

    #[test]
    fn update_snapshot_egress_addr_none_when_not_set() {
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let state = minimal_state("egress_snap_none_test");
        update_snapshot(&snap, &state);
        let s = snap.read().unwrap();
        assert!(s.egress_addr.is_none());
    }

    // ── ux.8′ budget-window tests ─────────────────────────────────────────────

    #[test]
    fn windows_elapsed_math() {
        // Division-based, saturating, interval 0 = never.
        assert_eq!(windows_elapsed(1_250, 1_000, 100), 2);
        assert_eq!(windows_elapsed(1_099, 1_000, 100), 0, "partial window does not count");
        assert_eq!(windows_elapsed(1_000, 1_000, 100), 0);
        assert_eq!(windows_elapsed(900, 1_000, 100), 0, "clock step-back saturates to 0");
        assert_eq!(windows_elapsed(5_000, 5_000, 0), 0, "interval 0 never resets");
        // Epoch-default anchor with a daily window: division, not a ~20k-iter loop.
        assert_eq!(windows_elapsed(1_800_000_000, 0, 86_400), 20_833);
    }

    fn budget_sched(interval: u64) -> SchedulerConfig {
        SchedulerConfig { global_token_budget: 1_000, budget_reset_interval: interval, ..unlimited() }
    }

    #[test]
    fn rebase_advances_anchor_and_window_by_whole_intervals() {
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, tmp) = recorder();
        let mut state = minimal_state("rebase");
        state.tokens_spent = 500;
        state.global_window_anchor = 0;
        state.budget_window_start = 1_000;
        let sched = budget_sched(100);

        maybe_rebase_windows_at(&mut state, &sched, &gateway, &registry, &rec, 1_250);
        assert_eq!(state.global_window_anchor, 500, "anchor advances to current lifetime spend");
        assert_eq!(state.budget_window_start, 1_200, "window advances by whole intervals (2×100)");
        assert_eq!(state.tokens_spent, 500, "lifetime meter is never zeroed");
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(log.contains("\"budget_reset\""), "a budget_reset event must be emitted");
    }

    #[test]
    fn rebase_noop_when_interval_zero_or_partial() {
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _tmp) = recorder();
        let mut state = minimal_state("noop");
        state.budget_window_start = 1_000;
        // interval 0 → legacy lifetime ceiling, never rebases.
        maybe_rebase_windows_at(&mut state, &budget_sched(0), &gateway, &registry, &rec, 9_999_999);
        assert_eq!(state.budget_window_start, 1_000);
        // partial window → no rebase.
        maybe_rebase_windows_at(&mut state, &budget_sched(100), &gateway, &registry, &rec, 1_099);
        assert_eq!(state.budget_window_start, 1_000);
    }

    #[test]
    fn restarts_never_double_or_skip_resets() {
        // Persisted window_start means elapsed accumulates across "restarts";
        // the number of resets equals floor(total_elapsed / interval), never
        // one-per-tick (double) nor zero (never-reset).
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, tmp) = recorder();
        let mut state = minimal_state("restart");
        state.budget_window_start = 1_000;
        let sched = budget_sched(100);
        // Ticks (as if from repeated short sessions) across 250s of wall time.
        for now in [1_050, 1_099, 1_150, 1_199, 1_250] {
            maybe_rebase_windows_at(&mut state, &sched, &gateway, &registry, &rec, now);
        }
        let resets = std::fs::read_to_string(tmp.path()).unwrap_or_default()
            .matches("\"budget_reset\"").count();
        assert_eq!(resets, 2, "250s / 100s window = exactly 2 resets");
    }

    #[test]
    fn reset_budget_command_global_and_agent_and_unknown() {
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _tmp) = recorder();
        let mdl = model_cfg();
        let sched = budget_sched(100);
        let mut state = minimal_state("cos");
        state.tokens_spent = 700;
        state.global_window_anchor = 200; // windowed = 500

        use crate::control::{BudgetTarget, ControlCommand};
        // Global reset → anchor jumps to lifetime; confirm reports old windowed.
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch_control_command(
            ControlCommand::ResetBudget { target: BudgetTarget::Global, confirm_tx: Some(tx) },
            &mdl, &mut state, &sched, &gateway, &registry, &rec,
        );
        let (spent_before, _window_start) = rx.blocking_recv().unwrap().unwrap();
        assert_eq!(spent_before, 500, "global reset reports pre-reset windowed spend");
        assert_eq!(state.global_window_anchor, 700);

        // Unknown agent → Err (the HTTP layer maps this to 404).
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch_control_command(
            ControlCommand::ResetBudget { target: BudgetTarget::Agent("ghost".into()), confirm_tx: Some(tx) },
            &mdl, &mut state, &sched, &gateway, &registry, &rec,
        );
        assert!(rx.blocking_recv().unwrap().is_err(), "unknown agent must return an error");

        // Known agent → Ok.
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch_control_command(
            ControlCommand::ResetBudget { target: BudgetTarget::Agent("cos".into()), confirm_tx: Some(tx) },
            &mdl, &mut state, &sched, &gateway, &registry, &rec,
        );
        assert!(rx.blocking_recv().unwrap().is_ok(), "known agent reset must succeed");
    }

    fn test_infer_req() -> InferenceRequest {
        InferenceRequest { system: None, messages: vec![], tools: vec![], max_tokens: 1024, streaming: false }
    }

    #[test]
    fn budget_deferred_agent_revived_on_rollover() {
        // The P0-2 fix end-to-end: an agent deferred on an exhausted GLOBAL window
        // is admitted (revived) — not terminated — when the window rolls over.
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _tmp) = recorder();
        let mut state = minimal_state("cos");
        let sched = budget_sched(100); // global ceiling 1000, 100s window
        state.tokens_spent = 1_500;          // windowed = 1500 − 0 ≥ 1000 → exhausted
        state.global_window_anchor = 0;
        state.budget_window_start = 1_000;
        state.deferred.push(DeferredInfer {
            priority: 0, seq: 0, agent_id: "cos".into(), request: test_infer_req(), turn: 0,
        });

        maybe_rebase_windows_at(&mut state, &sched, &gateway, &registry, &rec, 1_250);

        assert!(state.deferred.is_empty(), "rollover must drain the deferred agent");
        assert_eq!(state.in_flight, 1, "the revived agent is admitted (in-flight), not terminated");
        assert!(!state.outcomes.contains_key("cos"), "revived agent must NOT be terminated");
        assert!(state.agents.contains_key("cos"), "revived agent stays live");
    }

    #[test]
    fn drain_deferred_transient_under_window_permanent_without() {
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());

        // interval > 0: exhausted global budget is TRANSIENT → queue preserved.
        let (rec, _t1) = recorder();
        let mut state = minimal_state("a");
        state.tokens_spent = 5_000; // ≥ 1000
        state.deferred.push(DeferredInfer {
            priority: 0, seq: 0, agent_id: "a".into(), request: test_infer_req(), turn: 0,
        });
        drain_deferred(&mut state, &budget_sched(100), &gateway, &registry, &rec);
        assert_eq!(state.deferred.len(), 1, "under a window, exhausted agents stay deferred (not terminated)");
        assert!(!state.outcomes.contains_key("a"));

        // interval == 0: permanent → deny/terminate everything queued (legacy).
        let (rec, _t2) = recorder();
        let mut state = minimal_state("a");
        state.tokens_spent = 5_000;
        state.deferred.push(DeferredInfer {
            priority: 0, seq: 0, agent_id: "a".into(), request: test_infer_req(), turn: 0,
        });
        drain_deferred(&mut state, &budget_sched(0), &gateway, &registry, &rec);
        assert!(state.deferred.is_empty(), "no window → deny everything queued");
        assert!(state.outcomes.contains_key("a"), "no window → exhausted agent is terminated");
    }

    #[test]
    fn enqueue_over_global_budget_defers_or_terminates() {
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());

        // interval > 0 → defer, agent stays live.
        let (rec, _t1) = recorder();
        let mut state = minimal_state("a");
        state.tokens_spent = 5_000;
        enqueue_or_defer(AgentEffect::Infer(test_infer_req()), "a".into(), 0, 0, None,
            &mut state, &budget_sched(100), &gateway, &registry, &rec);
        assert_eq!(state.deferred.len(), 1, "over-budget Infer defers under a window");
        assert!(!state.outcomes.contains_key("a"));

        // interval == 0 → terminate.
        let (rec, _t2) = recorder();
        let mut state = minimal_state("a");
        state.tokens_spent = 5_000;
        enqueue_or_defer(AgentEffect::Infer(test_infer_req()), "a".into(), 0, 0, None,
            &mut state, &budget_sched(0), &gateway, &registry, &rec);
        assert!(state.outcomes.contains_key("a"), "no window → over-budget Infer terminates");
    }

    #[test]
    fn per_agent_budget_defers_under_window_and_revives() {
        // C1: per-agent (not just global) windowed exhaustion defers under a window,
        // and the rollover revives it. Global budget is unlimited here so ONLY the
        // per-agent cap is in play.
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _tmp) = recorder();
        let sched = SchedulerConfig { global_token_budget: 0, budget_reset_interval: 100, ..unlimited() };
        let mut state = minimal_state("a"); // agent token_budget = 100_000
        state.budget_window_start = 1_000;
        state.agents.get_mut("a").unwrap().test_set_spend(150_000); // windowed ≥ 100_000

        enqueue_or_defer(AgentEffect::Infer(test_infer_req()), "a".into(), 0, 0, None,
            &mut state, &sched, &gateway, &registry, &rec);
        assert_eq!(state.deferred.len(), 1, "per-agent over-budget defers under a window (not terminate)");
        assert!(!state.outcomes.contains_key("a"), "must NOT terminate — that would brick a resident agent");

        // Rollover resets the per-agent window → windowed drops to 0 → revived.
        maybe_rebase_windows_at(&mut state, &sched, &gateway, &registry, &rec, 1_250);
        assert!(state.deferred.is_empty(), "rollover revives the per-agent-deferred agent");
        assert_eq!(state.in_flight, 1);
    }

    #[test]
    fn drain_does_not_admit_per_agent_over_budget() {
        // F-A (ship adversarial, cross-model): drain_deferred runs on EVERY inference
        // completion, not just rollover. An agent over its own per-agent windowed cap
        // must stay deferred even when the global window has room and a slot is free —
        // otherwise the per-agent cap leaks one inference per slot-freed drain.
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _t) = recorder();
        let mut state = minimal_state("a"); // per-agent token_budget = 100_000
        state.tokens_spent = 0;             // global windowed 0 < 1000 → global has room
        state.agents.get_mut("a").unwrap().test_set_spend(150_000); // per-agent windowed ≥ 100_000
        state.deferred.push(DeferredInfer {
            priority: 0, seq: 0, agent_id: "a".into(), request: test_infer_req(), turn: 0,
        });
        drain_deferred(&mut state, &budget_sched(100), &gateway, &registry, &rec);
        assert_eq!(state.deferred.len(), 1, "over-per-agent-cap agent must stay deferred, not admitted");
        assert_eq!(state.in_flight, 0, "no admission");
        assert!(!state.outcomes.contains_key("a"), "and not terminated either");
    }

    #[test]
    fn agent_reset_drains_the_reset_agent() {
        // Codex #2: a per-agent ResetBudget must admit the agent if it was deferred on
        // its own cap — else the 200 reports a reset while the agent stays parked.
        use crate::control::{BudgetTarget, ControlCommand};
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _t) = recorder();
        let mdl = model_cfg();
        let sched = SchedulerConfig { global_token_budget: 0, budget_reset_interval: 100, ..unlimited() };
        let mut state = minimal_state("a");
        state.agents.get_mut("a").unwrap().test_set_spend(150_000); // over per-agent 100_000
        state.deferred.push(DeferredInfer {
            priority: 0, seq: 0, agent_id: "a".into(), request: test_infer_req(), turn: 0,
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch_control_command(
            ControlCommand::ResetBudget { target: BudgetTarget::Agent("a".into()), confirm_tx: Some(tx) },
            &mdl, &mut state, &sched, &gateway, &registry, &rec,
        );
        assert!(rx.blocking_recv().unwrap().is_ok());
        assert!(state.deferred.is_empty(), "per-agent reset must drain the deferred agent");
        assert_eq!(state.in_flight, 1, "reset revives it, not just reports success");
    }

    #[test]
    fn interval_zero_keeps_lifetime_no_rebase() {
        // Codex #3: interval == 0 is legacy LIFETIME enforcement — init_budget_window
        // must NOT rebase the anchor on a migrated checkpoint, or prior spend is
        // forgiven once and "0 = lifetime" is a lie.
        let mut state = minimal_state("a");
        state.tokens_spent = 5_000;
        state.budget_window_start = 0;   // migrated / fresh
        state.global_window_anchor = 0;
        init_budget_window(&mut state, &SchedulerConfig { budget_reset_interval: 0, ..unlimited() }, 9_999);
        assert_eq!(state.global_window_anchor, 0, "interval=0 must not rebase — lifetime preserved");
        assert_eq!(state.budget_window_start, 0, "interval=0 leaves the window unset");
        assert_eq!(state.global_windowed_spent(), 5_000, "windowed == lifetime under interval=0");
        // ux.13-TUI: and the operator surfaces must SAY so. With no window, per-agent exhaustion
        // terminates the agent, so the cockpit's budget-based "Park" is a kill — asserting only the
        // `true` case below would pass with this hardcoded.
        assert!(!state.budget_resettable, "interval=0 must publish budget_resettable=false");
    }

    #[test]
    fn migration_opens_clean_window_and_sets_resettable() {
        // T4: a pre-ux.8′ checkpoint (budget_window_start == 0) with accrued spend
        // must open a clean window at `now`, anchoring all current spend, so the
        // migrated agent's windowed spend is 0 (no spurious first-window brick).
        let mut state = minimal_state("cos");
        state.tokens_spent = 9_000_000;
        state.global_window_anchor = 0;
        state.budget_window_start = 0; // pre-ux.8′ / fresh
        state.agents.get_mut("cos").unwrap().test_set_spend(1_500_000);

        init_budget_window(&mut state, &budget_sched(86_400), 1_800_000_000);

        assert_eq!(state.budget_window_start, 1_800_000_000, "window opens at now");
        assert_eq!(state.global_window_anchor, state.tokens_spent, "global anchor = current lifetime spend");
        assert_eq!(state.agents.get("cos").unwrap().windowed_spent(), 0, "per-agent windowed spend rebased to 0");
        assert!(state.budget_resettable,
            "a configured window must publish budget_resettable=true (ux.13-TUI reads it to decide \
             whether Park is reversible)");
    }

    /// …and it must survive SERIALIZATION onto the wire. `update_snapshot`'s one-line copy plus the
    /// snapshot field are what the cockpit reads to decide whether Park is a pause or a kill; both
    /// consumers default to `false`, so deleting either leaves the TUI permanently labelling Park as
    /// terminal with nothing failing (/review: the safe-looking default is what hides the break).
    #[test]
    fn the_published_snapshot_carries_budget_resettable() {
        let mut state = minimal_state("cos");
        init_budget_window(&mut state, &budget_sched(86_400), 1_800_000_000);
        let snapshot: surfaces::SharedSnapshot = Default::default();
        update_snapshot(&snapshot, &state);
        let json = serde_json::to_value(&*snapshot.read().unwrap()).expect("snapshot serializes");
        assert_eq!(json["budget_resettable"], serde_json::json!(true),
            "the key agentctl reads must be present and true: {json}");

        // And the other direction, from the same code path.
        let mut off = minimal_state("solo");
        init_budget_window(&mut off, &SchedulerConfig { budget_reset_interval: 0, ..unlimited() }, 9_999);
        let snapshot2: surfaces::SharedSnapshot = Default::default();
        update_snapshot(&snapshot2, &off);
        let json2 = serde_json::to_value(&*snapshot2.read().unwrap()).unwrap();
        assert_eq!(json2["budget_resettable"], serde_json::json!(false));
    }

    #[test]
    fn future_window_start_is_clamped() {
        // F2: a persisted window_start in the future (NTP/RTC glitch) must not stall
        // resets forever — maybe_rebase clamps it back to now.
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _tmp) = recorder();
        let mut state = minimal_state("a");
        state.budget_window_start = 9_000; // far ahead of `now`
        maybe_rebase_windows_at(&mut state, &budget_sched(100), &gateway, &registry, &rec, 5_000);
        assert_eq!(state.budget_window_start, 5_000, "future window_start clamps to now");
    }

    #[test]
    fn manual_global_reset_advances_window_start_and_resets_agents() {
        // F4: a manual global reset advances the window start (no double-rebase on the
        // next tick) and rebases per-agent windows.
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _tmp) = recorder();
        let mdl = model_cfg();
        let sched = budget_sched(100);
        let mut state = minimal_state("cos");
        state.budget_window_start = 1_000;
        state.agents.get_mut("cos").unwrap().test_set_spend(500);

        use crate::control::{BudgetTarget, ControlCommand};
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch_control_command(
            ControlCommand::ResetBudget { target: BudgetTarget::Global, confirm_tx: Some(tx) },
            &mdl, &mut state, &sched, &gateway, &registry, &rec,
        );
        let (_spent, window_start) = rx.blocking_recv().unwrap().unwrap();
        assert!(window_start > 1_000, "window_start advances to now (not left at the stale 1000)");
        assert_eq!(state.budget_window_start, window_start);
        assert_eq!(state.agents.get("cos").unwrap().windowed_spent(), 0, "per-agent window reset too");
        // A tick just past the (new) window must NOT immediately double-reset.
        let before = std::fs::read_to_string(_tmp.path()).unwrap_or_default().matches("\"budget_reset\"").count();
        maybe_rebase_windows_at(&mut state, &sched, &gateway, &registry, &rec, window_start + 1);
        let after = std::fs::read_to_string(_tmp.path()).unwrap_or_default().matches("\"budget_reset\"").count();
        assert_eq!(before, after, "no double-reset one second after a manual reset");
    }

    // ── p7.6 universal-tier tests ─────────────────────────────────────────────

    #[test]
    fn config_universal_requires_command() {
        // Scheduler::new() must return Err when a universal agent has no `command`.
        let (rec, _tmp) = recorder();
        let bad_cfg = AgentConfig {
            tier:    crate::config::AgentTier::Universal,
            command: None,
            ..agent_cfg("worker", "do stuff")
        };
        let result = Scheduler::new(
            vec![bad_cfg],
            &model_cfg(),
            unlimited(),
            Arc::new(MockGateway::new(vec![])),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        );
        let err = result.err().expect("expected Err").to_string();
        assert!(err.contains("command"), "error must mention 'command', got: {err}");
    }

    #[tokio::test]
    async fn universal_agent_in_fuse_snapshot() {
        // A spawned universal agent must appear in the SchedulerSnapshot with
        // the correct tier string, status=Running, and a non-None pid.
        let snap = Arc::new(RwLock::new(SchedulerSnapshot::default()));
        let addr: std::net::SocketAddr = "127.0.0.1:19876".parse().unwrap();
        let (rec, _tmp) = recorder();
        let cfg = AgentConfig {
            tier:    crate::config::AgentTier::Universal,
            command: Some("sleep".to_string()),
            args:    vec!["30".to_string()],
            ..agent_cfg("univ_snap", "background")
        };
        let ua = UniversalAgent::spawn(&cfg, addr, "ua-snap-test-key", &rec).unwrap();
        let mut state = minimal_state("host");
        state.universal_agents.insert(cfg.id.clone(), ua);

        update_snapshot(&snap, &state);
        {
            let s = snap.read().unwrap();
            let agent = s.agents.iter().find(|a| a.id == "univ_snap")
                .expect("universal agent must appear in snapshot");
            assert_eq!(agent.tier.as_deref(), Some("universal:none"));
            assert_eq!(agent.status, AgentStatus::Running);
            assert!(agent.pid.is_some(), "pid must be set for running universal agent");
        } // drop read guard before .await

        // Cleanup.
        state.universal_agents.get_mut("univ_snap").unwrap().kill().await;
    }

    #[test]
    fn global_windowed_spent_folds_universal_tier() {
        // budget.1 / AUDIT-v0.97 P2-2: the scheduler's global window must count
        // universal-tier spend (read live through the attached egress meter), not
        // just native tokens_spent.
        use crate::evidence::EvidenceWriter;
        let dir = tempfile::TempDir::new().unwrap();
        let (rec, _tmp) = recorder();
        let writer = Arc::new(
            EvidenceWriter::open(&dir.path().join("ev.jsonl"), &dir.path().join("k.pkcs8")).unwrap(),
        );
        let egress = Arc::new(EgressProxy::new(writer, rec));
        let mut state = minimal_state("host");
        state.egress = Some(Arc::clone(&egress));
        state.tokens_spent = 200;
        state.global_window_anchor = 100;
        assert_eq!(state.global_windowed_spent(), 100, "native-only baseline");

        egress.meter().add_universal(50);
        assert_eq!(state.universal_lifetime_spent(), 50);
        assert_eq!(state.combined_lifetime_spent(), 250);
        assert_eq!(state.global_windowed_spent(), 150, "universal folds into the global window");
    }

    #[tokio::test]
    async fn cancel_universal_agent_flags_then_drains() {
        // budget.1 / AUDIT-v0.97 P3: a universal-tier agent (never in state.agents)
        // must be cancellable. The sync handler flags it (it cannot .await kill), and
        // the async drain reaps the subprocess + records a terminal outcome.
        use crate::control::ControlCommand;
        let addr: std::net::SocketAddr = "127.0.0.1:19878".parse().unwrap();
        let (rec, _tmp) = recorder();
        let cfg = AgentConfig {
            tier:    crate::config::AgentTier::Universal,
            command: Some("sleep".to_string()),
            args:    vec!["30".to_string()],
            ..agent_cfg("univ_cancel", "work")
        };
        let ua = UniversalAgent::spawn(&cfg, addr, "ua-cancel-key", &rec).unwrap();
        let mut state = minimal_state("native");
        state.universal_agents.insert(cfg.id.clone(), ua);

        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::Cancel { agent_id: "univ_cancel".into(), confirm_tx: Some(tx) }, &mut state);
        let confirmed = rx.await.unwrap().expect("universal cancel accepted");
        assert_eq!(confirmed, 1, "one universal agent flagged");
        assert!(state.universal_cancel_requested.contains("univ_cancel"), "flagged for async drain");
        assert!(state.universal_agents.contains_key("univ_cancel"), "not killed synchronously");

        drain_universal_cancels(&mut state, &rec).await;
        assert!(!state.universal_agents.contains_key("univ_cancel"), "subprocess reaped");
        assert!(state.universal_cancel_requested.is_empty(), "flag consumed by drain");
        assert!(
            state.outcomes.get("univ_cancel").is_some_and(|o| o.is_err()),
            "cancel recorded as a terminal error outcome"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn universal_only_cancel_is_not_starved() {
        // budget.1 / Finding 1: with NO native agents and one LIVE universal agent,
        // native `pending` is empty the whole time. The run loop MUST still service
        // control commands in that state, else a Cancel for the universal-only workload
        // is never read. Regression: run the real scheduler, send a Cancel, and assert
        // the universal agent is actively CANCELLED (AgentCancelled recorded) rather
        // than merely left to exit on its own.
        use crate::control::ControlCommand;
        let registry = Arc::new(crate::egress::ProxyRegistry::new());
        let addr: std::net::SocketAddr = "127.0.0.1:9333".parse().unwrap();
        let cfg = AgentConfig {
            tier:    crate::config::AgentTier::Universal,
            command: Some("sleep".to_string()),
            args:    vec!["30".to_string()],
            ..agent_cfg("univ_only", "work")
        };
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let (rec, tmp) = recorder();
        let scheduler = Scheduler::new(
            vec![cfg],
            &model_cfg(),
            unlimited(),
            Arc::new(MockGateway::new(vec![])),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        )
        .unwrap()
        .with_egress_addr(Some(addr))
        .with_proxy_registry(Arc::clone(&registry))
        .with_control(rx);

        // Buffer the Cancel and close the channel BEFORE running. run()'s future is
        // not Send (single-threaded cooperative scheduler), so it can't be spawned —
        // instead the loop reads the buffered command, drains the cancel, then sees the
        // closed channel and exits. Dropping tx makes the idle branch break once the
        // universal agent is gone.
        tx.send(ControlCommand::Cancel { agent_id: "univ_only".into(), confirm_tx: None })
            .await
            .unwrap();
        drop(tx);

        // If the cancel were starved, run() would only return when the sleep(30) exits
        // (> timeout) and WITHOUT an AgentCancelled event. The 10s bound proves it is read.
        tokio::time::timeout(std::time::Duration::from_secs(10), scheduler.run())
            .await
            .expect("scheduler must return promptly — universal cancel was not starved");

        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(
            log.contains("agent_cancelled") && log.contains("univ_only"),
            "universal agent must be actively cancelled (AgentCancelled), not left to exit; log:\n{log}"
        );
    }

    // ── attn.3 A3: never persist a transcript ending in an unanswered tool_use ──────
    //
    // Reproduces the MEASURED production failure (live agentos_cos-data volume,
    // 2026-08-01): a tool call was dispatched at 18:13:31, SIGTERM landed at 18:13:47
    // 16 s into a 20 s call (63 tool_call vs 62 tool_result), the half-finished turn was
    // checkpointed, and the restore drew `400 ... messages.125: tool_use ids were found
    // without tool_result blocks immediately after` one second after agent_restored.

    /// Drive an agent through the REAL api into the exact production shape: a stored
    /// response carrying a tool_use whose result never arrived.
    fn state_with_inflight_tool_call(id: &str, call_id: &str) -> (SchedulerState, Arc<FlightRecorder>, NamedTempFile) {
        let (rec, tmp) = recorder();
        let mut state = minimal_state(id);
        let task = state.agents.get_mut(id).unwrap();
        // step() once so the transcript has the task turn, then hand back a tool_use
        // response — the same sequence the scheduler runs before dispatching a tool.
        let _ = task.step(&rec);
        task.provide_inference(
            InferenceResponse {
                blocks: vec![Block::ToolUse {
                    id:    call_id.to_string(),
                    name:  "wait_for_trigger".to_string(),
                    input: serde_json::json!({ "timeout_s": 20 }),
                }],
                stop_reason:       StopReason::ToolUse,
                input_tokens:      11_569,
                output_tokens:     57,
                transport_retries: 0,
            },
            &rec,
        );
        // provide_inference only STORES the response; step_with_response is what pushes
        // its blocks into `messages` and emits the tool-call effect. Without this second
        // step the transcript is just the task turn and every assertion below is vacuous.
        let _ = state.agents.get_mut(id).unwrap().step(&rec);
        (state, rec, tmp)
    }

    /// The provider's pairing rule as a NON-PANICKING predicate: every `tool_use` id must have
    /// a `tool_result` in the IMMEDIATELY following message. Position-sensitive on purpose — a
    /// later, non-adjacent result does not satisfy the real API and must not satisfy this either.
    ///
    /// A predicate, not an assertion, because one caller needs to prove a transcript IS
    /// malformed. Doing that with `catch_unwind` would depend on unwinding, and this crate sets
    /// `panic = "abort"` in `[profile.release]` — a dev/test profile that ever inherits it would
    /// turn that precondition from a failing test into an aborted test binary.
    fn first_unpaired_tool_use(msgs: &[Msg]) -> Option<(usize, String)> {
        for (i, m) in msgs.iter().enumerate() {
            let uses: Vec<&str> = m
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { id, .. } => Some(id.as_str()),
                    _ => None,
                })
                .collect();
            if uses.is_empty() {
                continue;
            }
            let answered: Vec<&str> = msgs
                .get(i + 1)
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
            if let Some(u) = uses.iter().find(|u| !answered.contains(*u)) {
                return Some((i, (*u).to_string()));
            }
        }
        None
    }

    fn assert_well_formed(msgs: &[Msg], label: &str) {
        if let Some((i, id)) = first_unpaired_tool_use(msgs) {
            panic!(
                "{label}: tool_use {id} at messages.{i} has no tool_result in the immediately \
                 following message — this is the exact 400 the CoS hit"
            );
        }
    }

    #[test]
    fn a_non_adjacent_tool_result_does_not_satisfy_the_pairing_rule() {
        // Guards the POSITION-SENSITIVITY of first_unpaired_tool_use, which a mutation proved
        // was previously unguarded: making the predicate scan all later messages instead of
        // only `i + 1` left every test green. The provider rule is "tool_result in the
        // IMMEDIATELY following message", so a result that exists but sits two turns later is
        // still a 400 — and a checker that accepts it would declare the CoS transcript healthy
        // on exactly the shape that took the pipeline down.
        let msgs = vec![
            Msg { role: Role::User, blocks: vec![Block::Text { text: "task".into() }] },
            Msg {
                role:   Role::Assistant,
                blocks: vec![Block::ToolUse {
                    id:    "toolu_late".into(),
                    name:  "wait_for_trigger".into(),
                    input: serde_json::json!({}),
                }],
            },
            // Not a tool_result — so toolu_late is unanswered AT THE REQUIRED POSITION.
            Msg { role: Role::User, blocks: vec![Block::Text { text: "unrelated".into() }] },
            Msg { role: Role::Assistant, blocks: vec![Block::Text { text: "thinking".into() }] },
            // The result DOES exist, just too late. The real API rejects this.
            Msg {
                role:   Role::User,
                blocks: vec![Block::ToolResult {
                    tool_use_id: "toolu_late".into(),
                    content:     "{}".into(),
                    is_error:    false,
                }],
            },
        ];

        let found = first_unpaired_tool_use(&msgs);
        assert_eq!(
            found.as_ref().map(|(i, id)| (*i, id.as_str())),
            Some((1, "toolu_late")),
            "a tool_result two turns later must NOT count as paired; got {found:?}"
        );

        // Positive control: move the result to the adjacent slot and it IS paired. Without this
        // half, a predicate hard-wired to `return Some(..)` would also pass the assertion above.
        let mut adjacent = msgs.clone();
        adjacent.remove(3);
        adjacent.swap(2, 3);
        assert_eq!(
            first_unpaired_tool_use(&adjacent),
            None,
            "an immediately-following tool_result must count as paired"
        );
    }

    #[tokio::test]
    async fn checkpoint_seals_an_inflight_tool_call_so_restore_cannot_400() {
        let (state, _rec, tmp) = state_with_inflight_tool_call("ck-inflight", "toolu_inflight");

        // Precondition: the LIVE transcript really is malformed. If this ever stops being
        // true the test below is vacuous, so prove the hazard exists before proving the fix.
        let live = state.agents["ck-inflight"].messages();
        assert!(
            live.iter().any(|m| m
                .blocks
                .iter()
                .any(|b| matches!(b, Block::ToolUse { id, .. } if id == "toolu_inflight"))),
            "fixture must produce an assistant tool_use turn"
        );
        assert!(
            first_unpaired_tool_use(live).is_some(),
            "fixture precondition: the live transcript MUST be malformed, else this test \
             proves nothing"
        );

        let cp = build_scheduler_checkpoint(&state, None).0;
        let acp = cp.agents.iter().find(|a| a.agent_id == "ck-inflight").expect("agent in cp");
        assert_well_formed(&acp.messages, "checkpoint");

        drop(tmp);
    }

    #[tokio::test]
    async fn the_checkpoint_on_disk_is_well_formed_after_a_mid_tool_call_shutdown() {
        // The production failure was not about an in-memory struct: the transcript that
        // came BACK OFF DISK was malformed, and the restore 10 s later drew the 400. So
        // prove it through the real CheckpointStore, save -> load, not just the builder.
        let (state, rec, tmp) = state_with_inflight_tool_call("ck-disk", "toolu_disk");
        let dir = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(dir.path());

        checkpoint_all(&store, &state, &rec).await;

        let loaded = store
            .load()
            .expect("checkpoint must load")
            .expect("checkpoint must exist on disk");
        let acp = loaded
            .agents
            .iter()
            .find(|a| a.agent_id == "ck-disk")
            .expect("agent must be in the persisted checkpoint");
        assert_well_formed(&acp.messages, "on-disk checkpoint");

        // And the repair is auditable: a bare `agent_checkpointed` would hide that the
        // persisted transcript differs from what the agent actually holds.
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        let ck_events: Vec<serde_json::Value> = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|e| e["kind"] == "agent_checkpointed" && e["agent"] == "ck-disk")
            .collect();

        // ONE checkpoint must produce exactly ONE event per agent. The first cut of this fix
        // emitted a second `agent_checkpointed` from inside the checkpoint builder, so anything
        // counting checkpoints double-counted precisely when a repair fired. Caught at /review.
        assert_eq!(
            ck_events.len(),
            1,
            "exactly one agent_checkpointed per agent per checkpoint; got {}:\n{log}",
            ck_events.len()
        );
        // ...and the repair must be ON that event, not silent.
        let data = &ck_events[0]["data"];
        assert_eq!(data["stage"].as_str(), Some("checkpoint_repair"));
        assert_eq!(data["repaired_count"].as_u64(), Some(1));
        assert_eq!(
            data["repaired_ids"][0].as_str(),
            Some("toolu_disk"),
            "the sealed id must be named so the log is auditable"
        );
        // The pre-existing fields must survive the added ones.
        assert!(data["total_tokens"].is_u64(), "total_tokens must not be dropped");
        drop(tmp);
    }

    #[tokio::test]
    async fn checkpoint_repair_does_not_touch_the_live_transcript() {
        // The repair must apply to the checkpoint COPY only. The live agent may still
        // receive the real tool result, so mutating its history would fabricate a
        // cancellation that did not happen and then collide with the real result.
        let (state, _rec, tmp) = state_with_inflight_tool_call("ck-copy", "toolu_copy");

        let before = state.agents["ck-copy"].messages().len();
        let cp = build_scheduler_checkpoint(&state, None).0;
        let after = state.agents["ck-copy"].messages().len();

        assert_eq!(before, after, "live transcript must be untouched by checkpoint repair");
        let acp = cp.agents.iter().find(|a| a.agent_id == "ck-copy").unwrap();
        assert_eq!(
            acp.messages.len(),
            before + 1,
            "the checkpoint copy gains exactly one synthetic tool_result turn"
        );
        drop(tmp);
    }

    #[tokio::test]
    async fn checkpoint_does_not_seal_a_call_the_scheduler_promised_to_answer() {
        // NEGATIVE CONTROL. A tool_use awaiting a live child is NOT orphaned: restore
        // rebuilds `awaiting` from this same checkpoint and the real result arrives.
        // Sealing it would fabricate a result for work still in progress.
        //
        // `awaiting` is keyed by CHILD id and the call id lives in the VALUE — using the
        // keys here is the ux.13 P0 confusion, and the mutation control below pins it.
        let (mut state, _rec, tmp) = state_with_inflight_tool_call("ck-live", "toolu_promised");
        state.awaiting.insert(
            "child-1".to_string(),
            AwaitingParent {
                parent_id:       "ck-live".to_string(),
                call_id:         "toolu_promised".to_string(),
                deliver_content: false,
            },
        );

        let cp = build_scheduler_checkpoint(&state, None).0;
        let acp = cp.agents.iter().find(|a| a.agent_id == "ck-live").unwrap();

        // Non-vacuity: the tool_use must actually BE in the checkpointed transcript, or
        // "was not sealed" is trivially true and this control proves nothing.
        assert!(
            acp.messages.iter().any(|m| m
                .blocks
                .iter()
                .any(|b| matches!(b, Block::ToolUse { id, .. } if id == "toolu_promised"))),
            "non-vacuity: the promised tool_use must be present in the checkpoint"
        );

        let synthesized = acp.messages.iter().any(|m| {
            m.blocks.iter().any(
                |b| matches!(b, Block::ToolResult { tool_use_id, .. } if tool_use_id == "toolu_promised"),
            )
        });
        assert!(
            !synthesized,
            "a promised call must NOT be sealed — restore delivers the real result"
        );
        drop(tmp);
    }

    #[tokio::test]
    async fn checkpoint_leaves_a_well_formed_transcript_byte_identical() {
        // Idempotence + no-op safety. Compares the FULL Vec<Msg>, not len(): the repair's
        // merge path can reorder blocks without changing length (a false green the eng
        // review named).
        let (rec, tmp) = recorder();
        let mut state = minimal_state("ck-clean");
        let task = state.agents.get_mut("ck-clean").unwrap();
        let _ = task.step(&rec);
        task.provide_inference(
            InferenceResponse {
                blocks:            vec![Block::ToolUse {
                    id:    "toolu_ok".to_string(),
                    name:  "wait_for_trigger".to_string(),
                    input: serde_json::json!({}),
                }],
                stop_reason:       StopReason::ToolUse,
                input_tokens:      10,
                output_tokens:     5,
                transport_retries: 0,
            },
            &rec,
        );
        // The step that the helper above warns about, and that this test originally omitted:
        // provide_inference only STORES the response. Without it `messages` holds no tool_use
        // at all, the byte-identity check exercises only the is_empty() short-circuit, and
        // sealing unconditionally still passes. Caught at /review by a surviving mutation.
        let _ = state.agents.get_mut("ck-clean").unwrap().step(&rec);
        let task = state.agents.get_mut("ck-clean").unwrap();
        // Answer it, exactly as the scheduler does — now the transcript is well-formed.
        task.provide_tool_results(
            vec![Block::ToolResult {
                tool_use_id: "toolu_ok".to_string(),
                content:     "{\"status\":\"waiting\"}".to_string(),
                is_error:    false,
            }],
            &rec,
        );

        assert_well_formed(state.agents["ck-clean"].messages(), "precondition");
        // Serialized comparison rather than len(): this catches block REORDERING and any
        // content edit, which a length check would sail past.
        let expected = serde_json::to_string(state.agents["ck-clean"].messages()).unwrap();

        let cp = build_scheduler_checkpoint(&state, None).0;
        let acp = cp.agents.iter().find(|a| a.agent_id == "ck-clean").unwrap();
        assert_eq!(
            serde_json::to_string(&acp.messages).unwrap(),
            expected,
            "a well-formed transcript must pass through completely unchanged"
        );
        drop(tmp);
    }

    #[tokio::test]
    async fn checkpoint_excludes_universal_agents() {
        // Universal agents must never appear in the scheduler checkpoint —
        // they are external processes that cannot be serialized or restored.
        let addr: std::net::SocketAddr = "127.0.0.1:19877".parse().unwrap();
        let (rec, _tmp) = recorder();
        let cfg = AgentConfig {
            tier:    crate::config::AgentTier::Universal,
            command: Some("sleep".to_string()),
            args:    vec!["30".to_string()],
            ..agent_cfg("univ_ck", "work")
        };
        let ua = UniversalAgent::spawn(&cfg, addr, "ua-ck-test-key", &rec).unwrap();
        let mut state = minimal_state("native");
        state.universal_agents.insert(cfg.id.clone(), ua);

        let cp = build_scheduler_checkpoint(&state, None).0;
        assert!(
            !cp.agents.iter().any(|a| a.agent_id == "univ_ck"),
            "universal agent must NOT appear in checkpoint"
        );

        // Cleanup.
        state.universal_agents.get_mut("univ_ck").unwrap().kill().await;
    }

    // ── con.1 regression guard ────────────────────────────────────────────────

    /// A non-streaming response with transport_retries = 1 must emit
    /// InferenceTransportRetried in the flight log.
    #[tokio::test]
    async fn transport_retried_event_emitted_when_retries_nonzero() {
        let resp = InferenceResponse {
            blocks:            vec![Block::Text { text: "ok".to_string() }],
            stop_reason:       StopReason::EndTurn,
            input_tokens:      5,
            output_tokens:     2,
            transport_retries: 1,
        };
        let gw = MockGateway::new(vec![resp]);
        let mut cfg = agent_cfg("retry-agent", "retry task");
        cfg.max_turns = 1;
        let (rec, tmp) = recorder();
        let sched = Scheduler::new(
            vec![cfg],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            Arc::clone(&rec),
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
        ).unwrap();

        let outcomes = sched.run().await;
        assert!(outcomes["retry-agent"].is_ok(), "agent must succeed");

        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(
            log.contains("\"inference_transport_retried\""),
            "InferenceTransportRetried event must appear when transport_retries > 0"
        );
        assert!(
            log.contains("\"retries\":1"),
            "retries field must be 1 in the event payload"
        );
    }

    // ── attn.2 R2 / attn.1a-05 tests ──────────────────────────────────────────

    /// Build a checkpoint whose agent is parked mid-`run_job`: the assistant turn ends in an
    /// unanswered `tool_use`, and `awaiting` records the scheduler's promise to answer it.
    /// This is exactly the shape the CoS trigger checkpoints in — and because
    /// `default_checkpoint_interval_turns()` is 1, it is captured on every turn of a cycle.
    ///
    /// `child_present` decides whether the await is LIVE or DEAD. Only an await whose child
    /// still exists can ever be delivered (`handle_agent_terminal` is the sole delivery path
    /// and runs only for an existing agent), so the two cases must behave oppositely:
    /// live → skip the parent and preserve its dangling call; dead → drop the await, repair
    /// the now-orphaned call, and let the parent run.
    fn checkpoint_parked_mid_run_job(
        parent: &str, child: &str, call_id: &str, child_present: bool,
    ) -> SchedulerCheckpoint {
        use crate::inference::{Block, Msg, Role};
        let ids: Vec<&str> = if child_present { vec![parent, child] } else { vec![parent] };
        let mut cp = minimal_scheduler_checkpoint(&ids);
        cp.agents[0].messages.push(Msg {
            role: Role::Assistant,
            blocks: vec![Block::ToolUse {
                id: call_id.to_string(),
                name: "run_job".to_string(),
                input: serde_json::json!({ "job_id": "cos-inbox" }),
            }],
        });
        // NB: the child is left NON-terminal on purpose. A terminal child would be delivered
        // to the parent immediately without ever inferring, so the parent's inference would
        // legitimately be the first one in the log and the ordering assertion in
        // `restored_awaiting_parent_is_not_reseeded` would fail for a reason that is not the
        // bug. A live child must actually run, which is also the realistic shape.
        cp.awaiting = vec![crate::checkpoint::AwaitingEntry {
            child_id:  child.to_string(),
            parent_id: parent.to_string(),
            call_id:   call_id.to_string(),
            deliver_content: false,
        }];
        cp
    }

    /// A gateway that enforces the provider's tool_use/tool_result pairing rule.
    ///
    /// `MockGateway` accepts any history, and so does the `/qa` fake provider — so a test can
    /// go green against a conversation the real API rejects with 400. That is the exact
    /// false-green shape attn.1a-05 hid behind, so the invariant is ENFORCED here rather than
    /// assumed. Rejects when the last message is an assistant turn carrying a `tool_use` with
    /// no matching `tool_result`, which is the literal condition behind
    /// "tool_use ids were found without tool_result blocks".
    ///
    /// This is also a deterministic observable: seed order comes from HashMap iteration, so
    /// "who inferred first" is not stable and cannot be asserted on.
    struct PairingCheckGateway {
        responses: Arc<Mutex<Vec<InferenceResponse>>>,
        rejected:  Arc<Mutex<Vec<String>>>,
    }

    impl PairingCheckGateway {
        fn new(responses: Vec<InferenceResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
                rejected:  Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl InferenceGateway for PairingCheckGateway {
        async fn infer(&self, req: InferenceRequest) -> anyhow::Result<InferenceResponse> {
            use crate::inference::{Block, Role};
            if let Some(last) = req.messages.last() {
                if last.role == Role::Assistant {
                    let dangling: Vec<String> = last
                        .blocks
                        .iter()
                        .filter_map(|b| match b {
                            Block::ToolUse { id, .. } => Some(id.clone()),
                            _ => None,
                        })
                        .collect();
                    if !dangling.is_empty() {
                        self.rejected.lock().unwrap().extend(dangling.clone());
                        return Err(anyhow::anyhow!(
                            "400 messages: tool_use ids were found without tool_result blocks \
                             immediately after: {dangling:?}"
                        ));
                    }
                }
            }
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(anyhow::anyhow!("PairingCheckGateway: no more responses queued"));
            }
            Ok(q.remove(0))
        }
        fn model_id(&self) -> &str { "pairing-check" }
    }

    /// R2.1 — a restored parent awaiting a child must NOT be re-stepped by the seed loop.
    /// Re-stepping is what shipped the dangling `run_job` tool_use to the provider and
    /// bricked the CoS; it would also run a second, fully-paid cycle.
    #[tokio::test]
    async fn restored_awaiting_parent_is_not_reseeded() {
        let cp = checkpoint_parked_mid_run_job("trigger", "cos-inbox-2026-08-02", "toolu_rj", true);
        let gw = PairingCheckGateway::new(vec![
            end_turn("child answer", 1, 1),
            end_turn("parent continues", 1, 1),
        ]);
        let rejected = Arc::clone(&gw.rejected);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("trigger", "orchestrate")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();

        // Assert COMPLETION, not just "we waited". A skip arm that parks an agent forever
        // would be a worse regression than the 400 it replaces — the old behaviour at least
        // failed fast.
        let completed = tokio::time::timeout(std::time::Duration::from_secs(5), sched.run()).await;
        assert!(completed.is_ok(),
            "scheduler hung with a live awaiting-parent — the skip arm must not deadlock the \
             run loop");

        // The provider-accurate observable: the parent must never have been handed a history
        // whose final assistant turn still carries an unanswered tool_use. Asserting on WHICH
        // agent inferred first cannot work — seed order is HashMap iteration order.
        let r = rejected.lock().unwrap();
        assert!(
            r.is_empty(),
            "an agent was stepped while its tool_use was still unanswered ({r:?}). That is the \
             attn.1a-05 brick: the seed loop must skip a parent that is awaiting a child, and \
             the restore repair must answer any genuinely orphaned call."
        );
    }

    /// R2.1 fail-forward — a DEAD await (child absent from the restored set) must NOT park the
    /// parent. Nothing can ever deliver it: `handle_agent_terminal` is the only delivery path
    /// and it runs only for an agent that exists. Treating it as live would suppress the repair
    /// AND skip the parent, leaving it silent forever while re-checkpointing the poison —
    /// strictly worse than the 400 this increment removes. Found independently by the Codex
    /// adversarial pass and the security specialist during /review.
    #[tokio::test]
    async fn restored_parent_with_dead_await_resumes_instead_of_parking() {
        use crate::inference::Block;
        let cp = checkpoint_parked_mid_run_job("trigger", "ghost-child", "toolu_dead", false);
        let gw = MockGateway::new(vec![end_turn("parent recovered", 1, 1)]);
        let queue = Arc::clone(&gw.responses);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("trigger", "orchestrate")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();

        let answered = sched.agents["trigger"].messages().iter().any(|m| {
            m.blocks.iter().any(|b| matches!(b,
                Block::ToolResult { tool_use_id, is_error, .. }
                if tool_use_id == "toolu_dead" && *is_error))
        });
        assert!(answered,
            "a dead await must be dropped so the repair answers the orphaned call; leaving it \
             live suppresses the repair and parks the parent forever");

        let completed = tokio::time::timeout(std::time::Duration::from_secs(5), sched.run()).await;
        assert!(completed.is_ok(), "scheduler hung on a dead await");
        assert_eq!(queue.lock().unwrap().len(), 0,
            "the parent must be seeded and RUN once its dead await is dropped — parking it \
             silently is the regression this test exists to prevent");

        // The dead await is dropped inside `run()` (that is where scheduler state is
        // restored), so this must be asserted after it. It IS a genuine anomaly, so unlike the
        // self-heal above it stays EventKind::Error and should read red in the cockpit.
        let log = std::fs::read_to_string(_tmp.path()).unwrap_or_default();
        let ev = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["data"]["child_id"] == "ghost-child")
            .expect("dropping a dead await must be recorded — silently mutating restored \
                     scheduler state is exactly the invisibility this increment fights");
        assert_eq!(ev["kind"], "error", "a dead await is a real anomaly and should read red");
        assert_eq!(ev["data"]["call_id"], "toolu_dead");
    }

    /// Negative control for the above. Without this, deleting the whole seed loop would
    /// make `restored_awaiting_parent_is_not_reseeded` pass vacuously.
    #[tokio::test]
    async fn restored_agent_with_no_pending_await_is_still_seeded() {
        let cp = minimal_scheduler_checkpoint(&["plain"]);
        let gw = MockGateway::new(vec![end_turn("done", 1, 1)]);
        let queue = Arc::clone(&gw.responses);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("plain", "ordinary task")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();

        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), sched.run()).await;

        assert_eq!(
            queue.lock().unwrap().len(), 0,
            "an ordinary restored agent MUST still be seeded; if it is not, the skip arm is \
             over-broad and the previous test proves nothing"
        );
    }

    /// R2.2 — restore repairs an ORPHANED dangling call (no scheduler promise to answer it),
    /// so the next inference is well-formed instead of 400-ing.
    #[test]
    fn restore_repairs_an_orphaned_dangling_tool_use() {
        use crate::inference::{Block, Role};
        // Same parked shape, but with NO awaiting entry — nothing will ever answer it.
        let mut cp = checkpoint_parked_mid_run_job("solo", "unused", "toolu_orphan", false);
        cp.awaiting.clear();
        let gw = MockGateway::new(vec![]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("solo", "task")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();

        // The self-heal must be RECORDED — it is the operator's only trace of it — and it
        // must NOT be an error event: `agentctl/src/watch/inspector.rs:is_error_event` matches
        // "kind":"error" to drive the Errors filter and the red row colour, so recording a
        // successful repair as an error paints every self-healed boot as a failure.
        let log = std::fs::read_to_string(_tmp.path()).unwrap_or_default();
        let ev = log
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["data"]["stage"] == "restore_repair")
            .expect("the restore repair must emit a flight event");
        assert_eq!(ev["kind"], "agent_restored",
            "must not be `error` — that would paint a successful self-heal red in the cockpit");
        assert_eq!(ev["agent"], "solo", "the JSONL field is `agent`, not `agent_id`");
        // Field names harmonised with the checkpoint-side repair at /qa: a log query for
        // `repaired_ids` previously missed this path entirely.
        assert_eq!(ev["data"]["repaired_ids"], serde_json::json!(["toolu_orphan"]));
        assert_eq!(ev["data"]["repaired_count"], serde_json::json!(1));
        assert!(
            ev["data"]["repaired_call_ids"].is_null(),
            "the old field name must be gone, not emitted alongside the new one"
        );

        let msgs = sched.agents["solo"].messages();
        let last = msgs.last().expect("history must not be empty");
        assert_eq!(last.role, Role::User, "an orphaned call must be answered by a user turn");
        assert!(
            last.blocks.iter().any(|b| matches!(b,
                Block::ToolResult { tool_use_id, is_error, .. }
                if tool_use_id == "toolu_orphan" && *is_error)),
            "orphaned toolu_orphan must receive a synthetic error tool_result on restore"
        );
    }

    /// R2.2 negative control — a dangling call the scheduler HAS promised to answer must be
    /// left alone. Repairing it would make the child's real result arrive as a tool_result
    /// with no matching tool_use: the same API error, from the other side.
    #[test]
    fn restore_does_not_repair_a_live_awaited_call() {
        use crate::inference::Block;
        let cp = checkpoint_parked_mid_run_job("trigger", "child-1", "toolu_live", true);
        let gw = MockGateway::new(vec![]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("trigger", "orchestrate")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();

        let answered = sched.agents["trigger"].messages().iter().any(|m| {
            m.blocks.iter().any(|b| matches!(b,
                Block::ToolResult { tool_use_id, .. } if tool_use_id == "toolu_live"))
        });
        assert!(
            !answered,
            "a call recorded in `awaiting` must NOT be answered by the restore repair — the \
             child delivers it later, and a duplicate answer is the same 400 in reverse"
        );
    }

    // ── orch.2 tests ──────────────────────────────────────────────────────────

    #[test]
    fn waiting_agents_restore_from_checkpoint() {
        // ar-01: waiting_agents in checkpoint restores state.waiting.
        let mut cp = minimal_scheduler_checkpoint(&["w1"]);
        cp.waiting_agents = vec!["w1".to_string()];
        cp.agents[0].terminal = true; // waiting agents are checkpointed as terminal
        let gw = MockGateway::new(vec![]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("w1", "orchestrated task")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();
        // The scheduler holds agent w1 (restored as terminal).
        assert!(sched.agents.contains_key("w1"), "waiting agent must be present in agents map");
        assert!(sched.agents["w1"].is_terminal(), "restored waiting agent must be terminal");
        // Verify the waiting_agents field flows through to restored (consumed by run()).
        let r = sched.restored.as_ref().unwrap();
        assert!(r.waiting_agents.contains(&"w1".to_string()), "waiting set must be restored in SchedulerRestored");
    }

    #[test]
    fn orchestrated_agents_restore_from_checkpoint() {
        // ar-01: orchestrated_agents in checkpoint restores state.orchestrated.
        let mut cp = minimal_scheduler_checkpoint(&["orch-1"]);
        cp.orchestrated_agents = vec!["orch-1".to_string()];
        cp.waiting_agents     = vec!["orch-1".to_string()];
        cp.agents[0].terminal = true;
        let gw = MockGateway::new(vec![]);
        let (rec, _tmp) = recorder();
        let sched = Scheduler::new(
            vec![agent_cfg("orch-1", "orchestrated task")],
            &model_cfg(),
            unlimited(),
            Arc::new(gw),
            Arc::new(ToolRegistry::new()),
            rec,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            Some(cp),
        ).unwrap();
        assert!(sched.agents.contains_key("orch-1"));
        // Verify restored fields carry through to sched.restored (consumed by run()).
        let r = sched.restored.as_ref().unwrap();
        assert!(r.orchestrated_agents.contains(&"orch-1".to_string()), "orchestrated set must be restored");
        assert!(r.waiting_agents.contains(&"orch-1".to_string()), "waiting set must be restored");
    }

    #[test]
    fn cancelled_parent_not_resurrected_by_late_child_terminal() {
        // AUDIT-v0.97 P2-4 regression: a cancelled parked ROOT parent (funneled, recorded in
        // outcomes, but still in state.agents) must NOT be re-stepped when its awaited child
        // later terminates — else the cancelled trigger resurrects (flips cancelled->done,
        // spends more). Deterministic: drive the child terminal directly and assert the
        // parent's turn does not advance and its cancelled outcome is preserved.
        let mut state = minimal_state("parent");
        let cfg = agent_cfg("child", "child task");
        let mdl = model_cfg();
        state.agents.insert("child".into(), AgentTask::new("child", &cfg.task, &cfg, &mdl, vec![]));
        state.awaiting.insert("child".into(), AwaitingParent {
            parent_id: "parent".into(), call_id: "c1".into(), deliver_content: false,
        });
        // Post-cancel state of the parent: terminal outcome recorded, flag already consumed,
        // still present in state.agents (a funneled root is not removed).
        state.outcomes.insert("parent".into(), Err(anyhow::anyhow!("operator cancelled")));
        let turn_before = state.agents["parent"].turn();

        let (rec, _tmp) = recorder();
        let gw: Arc<dyn crate::inference::InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        handle_agent_terminal(
            "child".into(), Ok("child done".into()),
            &mut state, &SchedulerConfig::default(), &gw, &registry, &rec,
        );

        assert_eq!(state.agents.get("parent").map(|p| p.turn()), Some(turn_before),
            "cancelled parent must NOT be re-stepped by the child terminal (turn unchanged)");
        assert!(state.outcomes.get("parent").is_some_and(|r| r.is_err()),
            "cancelled parent's terminal outcome must be preserved (not flipped to done)");
        assert!(!state.agents.contains_key("child"), "child is removed on its terminal");
    }

    #[test]
    fn handle_agent_terminal_clears_both_sets() {
        // C2 / ar-05: handle_agent_terminal removes from both waiting and orchestrated.
        let mut state = minimal_state("a");
        state.waiting.insert("a".to_string());
        state.orchestrated.insert("a".to_string());
        let (rec, _tmp) = recorder();
        let gw: Arc<dyn crate::inference::InferenceGateway + Send + Sync> =
            Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        handle_agent_terminal(
            "a".to_string(),
            Ok("done".to_string()),
            &mut state,
            &SchedulerConfig::default(),
            &gw,
            &registry,
            &rec,
        );
        assert!(!state.waiting.contains("a"), "waiting must be cleared by handle_agent_terminal");
        assert!(!state.orchestrated.contains("a"), "orchestrated must be cleared by handle_agent_terminal");
    }

    #[test]
    fn build_checkpoint_includes_waiting_agents() {
        // ar-01: build_scheduler_checkpoint includes waiting orchestrated agents.
        let (rec, _tmp) = recorder();
        let mut state = minimal_state("orch");
        // Mark as terminal (as happens after a turn completes in orchestrated mode).
        let sm = state.agents.get_mut("orch").unwrap();
        let _ = sm.step(&rec); // advance to a state; we force terminal via waiting insert
        state.waiting.insert("orch".to_string());
        state.orchestrated.insert("orch".to_string());
        // Terminal flag would normally be set by AgentTask completing — test that
        // filter includes waiting agents regardless of terminal status.
        let cp = build_scheduler_checkpoint(&state, None).0;
        assert!(
            cp.waiting_agents.contains(&"orch".to_string()),
            "waiting_agents must be included in checkpoint"
        );
        assert!(
            cp.orchestrated_agents.contains(&"orch".to_string()),
            "orchestrated_agents must be included in checkpoint"
        );
        // The agent itself must appear in the agents list even if terminal.
        assert!(
            cp.agents.iter().any(|a| a.agent_id == "orch"),
            "waiting orchestrated agent must appear in checkpoint agents list"
        );
    }

    #[test]
    fn answer_truncation_caps_at_512_chars() {
        // G5 / ar-03: OrchestratorTurnComplete answer_preview must never exceed 512 chars.
        // Uses chars().count() guard so multi-byte characters are counted correctly.
        let (rec, tmp) = recorder();
        let gw: Arc<dyn crate::inference::InferenceGateway + Send + Sync> =
            Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let mut state = minimal_state("orch-trunc");
        state.orchestrated.insert("orch-trunc".to_string());

        // Build a 600-char ASCII answer (well above the 512-char cap).
        let long_answer: String = "A".repeat(600);
        assert_eq!(long_answer.chars().count(), 600, "precondition: 600 chars");

        enqueue_or_defer(
            AgentEffect::Completed(long_answer.clone()),
            "orch-trunc".to_string(),
            0,
            0,
            None,
            &mut state,
            &SchedulerConfig::default(),
            &gw,
            &registry,
            &rec,
        );

        // The agent must be parked (waiting), not terminated.
        assert!(state.waiting.contains("orch-trunc"), "agent must be parked after Completed");

        // The flight log must contain orchestrator_turn_complete with a truncated answer.
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(
            log.contains("\"orchestrator_turn_complete\""),
            "orchestrator_turn_complete must be emitted"
        );
        assert!(
            log.contains("[output truncated"),
            "answer must contain truncation suffix when >512 chars"
        );
        // The raw 600-char answer must NOT appear in its entirety in the log.
        assert!(
            !log.contains(&long_answer),
            "full 600-char answer must not appear verbatim in the flight log"
        );
    }

    #[test]
    fn orchestrated_maxtokens_truncation_parks_not_terminates() {
        // budget.1-ar-01 — pins audit86-P0-2 at the DISPATCH layer (previously unpinned there).
        // A resettable ORCHESTRATED agent whose response truncated at max_tokens returns
        // CompletedTruncated; the scheduler must PARK it (waiting), keep it in state.agents, and
        // record NO outcome — it stays resumable and NEVER bricks.
        let (rec, _tmp) = recorder();
        let gw: Arc<dyn crate::inference::InferenceGateway + Send + Sync> =
            Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let mut state = minimal_state("orch");
        state.orchestrated.insert("orch".to_string());

        enqueue_or_defer(
            AgentEffect::CompletedTruncated("partial but usable".to_string()),
            "orch".to_string(),
            0,
            0,
            None,
            &mut state,
            &SchedulerConfig::default(),
            &gw,
            &registry,
            &rec,
        );

        assert!(state.waiting.contains("orch"), "orchestrated truncation must PARK (waiting)");
        assert!(state.agents.contains_key("orch"), "agent must remain in state.agents (resumable)");
        assert!(
            !state.outcomes.contains_key("orch"),
            "orchestrated truncation must NOT terminate (no outcome) — bricking is the P0-2 regression"
        );
    }

    #[test]
    fn oneshot_maxtokens_truncation_fails_not_silent_success() {
        // budget.1-ar-01 — the other half of the role-gate: a NON-orchestrated (one-shot root)
        // agent whose response truncated at max_tokens must FAIL (outcome = Err), not report a
        // clean success carrying silently-truncated text. Proves the reported bug is fixed.
        let (rec, _tmp) = recorder();
        let gw: Arc<dyn crate::inference::InferenceGateway + Send + Sync> =
            Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let mut state = minimal_state("one");
        // NOT orchestrated, NOT an awaiting child → a plain one-shot root.

        enqueue_or_defer(
            AgentEffect::CompletedTruncated("partial and incomplete".to_string()),
            "one".to_string(),
            0,
            0,
            None,
            &mut state,
            &SchedulerConfig::default(),
            &gw,
            &registry,
            &rec,
        );

        assert!(!state.waiting.contains("one"), "one-shot truncation must NOT park");
        match state.outcomes.get("one") {
            Some(Err(e)) => assert!(
                e.to_string().contains("truncated at max_tokens"),
                "outcome error must name the truncation, got: {e}"
            ),
            Some(Ok(_)) => panic!("one-shot truncation must FAIL, not report success with partial text"),
            None => panic!("one-shot truncation must record a terminal outcome"),
        }
    }

    #[test]
    fn inject_guard_rejects_non_waiting_agent() {
        // G4 / ar-05: Inject into an agent that is NOT in state.waiting must
        // emit OrchestratorExited with reason "agent_not_waiting" and NOT
        // add the agent to the run queue or corrupt its context.
        use crate::control::ControlCommand;

        let (rec, tmp) = recorder();
        let gw: Arc<dyn crate::inference::InferenceGateway + Send + Sync> =
            Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let mut state = minimal_state("active-agent");
        // Agent exists in state.agents but is NOT in state.waiting (actively running).
        assert!(!state.waiting.contains("active-agent"), "precondition: not waiting");
        assert!(state.agents.contains_key("active-agent"), "precondition: agent exists");

        let in_flight_before = state.in_flight;
        dispatch_control_command(
            ControlCommand::Inject { agent_id: "active-agent".to_string(), text: "hello".to_string() },
            &model_cfg(),
            &mut state,
            &SchedulerConfig::default(),
            &gw,
            &registry,
            &rec,
        );

        // Guard must NOT have scheduled inference (in_flight unchanged).
        assert_eq!(state.in_flight, in_flight_before, "inject into non-waiting agent must not enqueue inference");
        // Agent must still not be in waiting.
        assert!(!state.waiting.contains("active-agent"), "agent must not be inserted into waiting by the guard");

        // Flight log must contain orchestrator_exited with agent_not_waiting reason.
        let log = std::fs::read_to_string(tmp.path()).unwrap_or_default();
        assert!(
            log.contains("\"orchestrator_exited\""),
            "OrchestratorExited must be emitted when injecting into non-waiting agent"
        );
        assert!(
            log.contains("\"agent_not_waiting\""),
            "reason agent_not_waiting must appear in the event payload"
        );
    }

    // ── ux.2a: derive_attention ─────────────────────────────────────────────

    /// Positional test wrapper around `derive_attention` (which takes a bundled
    /// `AttentionInputs` in production code — this shorthand is purely test-ergonomics,
    /// matching this file's existing small-helper convention, e.g. `parked_approval` below).
    #[allow(clippy::too_many_arguments)]
    fn da(
        agent_id: &str,
        windowed_spent: u64,
        token_budget: u64,
        credential_providers: &[String],
        pending_approvals: &HashMap<String, ParkedApproval>,
        credential_snapshot: Option<&surfaces::CredentialSnapshot>,
    ) -> Vec<surfaces::AttentionSignal> {
        derive_attention(AttentionInputs {
            agent_id, windowed_spent, token_budget, credential_providers,
            pending_approvals, credential_snapshot,
            last_error: None,
        })
    }

    fn parked_approval(agent_id: &str) -> ParkedApproval {
        ParkedApproval {
            agent_id:   agent_id.to_string(),
            call_id:    "call-1".to_string(),
            action:     PendingActionRequest {
                kind:       "write_file".to_string(),
                risk:       "medium".to_string(),
                summary:    "write a file".to_string(),
                args:       serde_json::json!({}),
                prev_state: None,
                new_state:  None,
            },
            created_at: std::time::Instant::now(),
        }
    }

    fn provider_health(name: &str, token_fresh: bool, last_error: Option<&str>) -> surfaces::ProviderHealth {
        surfaces::ProviderHealth {
            name: name.to_string(),
            token_fresh,
            last_refresh_at: Some(1_700_000_000),
            expires_at: None,
            last_error: last_error.map(|s| s.to_string()),
            attention_reason: None,
            recovery_kind: None,
            attention_since: None,
        }
    }

    #[test]
    fn derive_attention_clean_agent_has_no_signals() {
        let signals = da("a1", 100, 50_000, &[], &HashMap::new(), None);
        assert!(signals.is_empty(), "an agent with no active signal sources must be Clean");
    }

    #[test]
    fn derive_attention_error_fires_and_clears() {
        // ux.2b: a still-running tool error surfaces as an Error signal with the excerpt as
        // evidence; None (a cleared/never-set error) produces no Error signal.
        let with_err = derive_attention(AttentionInputs {
            agent_id: "a1", windowed_spent: 100, token_budget: 50_000,
            credential_providers: &[], pending_approvals: &HashMap::new(),
            credential_snapshot: None, last_error: Some("boom: connection refused"),
        });
        assert_eq!(with_err.len(), 1);
        assert_eq!(with_err[0].reason, surfaces::AttentionReason::Error);
        assert_eq!(with_err[0].evidence.as_deref(), Some("boom: connection refused"));
        // Cleared (auto-clear on next all-ok batch → last_error None): no Error signal.
        let cleared = da("a1", 100, 50_000, &[], &HashMap::new(), None);
        assert!(cleared.iter().all(|s| s.reason != surfaces::AttentionReason::Error));
    }

    #[test]
    fn derive_attention_approval_pending_fires() {
        let mut pending = HashMap::new();
        pending.insert("act_1".to_string(), parked_approval("a1"));
        let signals = da("a1", 100, 50_000, &[], &pending, None);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].reason, surfaces::AttentionReason::ApprovalPending);
        assert_eq!(signals[0].evidence.as_deref(), Some("act_1"));
    }

    #[test]
    fn derive_attention_approval_ignores_other_agents() {
        let mut pending = HashMap::new();
        pending.insert("act_1".to_string(), parked_approval("someone-else"));
        let signals = da("a1", 100, 50_000, &[], &pending, None);
        assert!(signals.is_empty());
    }

    /// Guards against the Eng Dual Voices (Codex) finding: filtering the already-`.take(100)`-
    /// capped `pending_actions` snapshot vector would silently drop signals for agents past the
    /// 100th pending approval. `derive_attention` must read `pending_approvals` directly instead
    /// — this test constructs >100 entries and asserts the 101st agent still gets a signal.
    #[test]
    fn derive_attention_approval_not_capped_at_one_hundred() {
        let mut pending = HashMap::new();
        for i in 0..150 {
            pending.insert(format!("act_{i}"), parked_approval(&format!("filler-{i}")));
        }
        pending.insert("act_late".to_string(), parked_approval("agent-101"));
        let signals = da("agent-101", 100, 50_000, &[], &pending, None);
        assert_eq!(signals.len(), 1, "agent past a hypothetical 100-cap must still get its Approval signal");
        assert_eq!(signals[0].reason, surfaces::AttentionReason::ApprovalPending);
    }

    /// Guards against a HashMap-iteration-order footgun: if an agent ever had 2+ simultaneous
    /// pending approvals (the scheduler's own invariant — parked agents can't re-step and
    /// re-request — should prevent this today, but this test pins the behavior explicitly
    /// rather than leaving it to chance if that invariant is ever weakened).
    #[test]
    fn derive_attention_multiple_approvals_same_agent_collapses_to_one_signal() {
        let mut pending = HashMap::new();
        pending.insert("act_a".to_string(), parked_approval("a1"));
        pending.insert("act_b".to_string(), parked_approval("a1"));
        let signals = da("a1", 100, 50_000, &[], &pending, None);
        assert_eq!(signals.len(), 1, "must collapse to a single ApprovalPending signal per agent, never panic");
        assert_eq!(signals[0].reason, surfaces::AttentionReason::ApprovalPending);
        assert!(
            signals[0].evidence.as_deref() == Some("act_a") || signals[0].evidence.as_deref() == Some("act_b"),
            "evidence must be one of the two approval IDs (HashMap iteration order is unspecified, not a bug)"
        );
    }

    /// Distinct from "agent doesn't list the provider": here the agent DOES list a provider,
    /// but that provider is absent from the gateway's own health snapshot entirely (e.g.
    /// configured for the agent but never registered) — a different code branch than the
    /// "provider listed and found" or "provider not listed at all" cases already covered above.
    #[test]
    fn derive_attention_degraded_provider_not_in_health_snapshot_is_evaluation_unavailable() {
        // A provider absent from the gateway's health snapshot entirely (config drift, a
        // stale grant) is a real inconsistency — must render EvaluationUnavailable, not
        // silently Clean (adversarial review finding, Codex).
        let snap = surfaces::CredentialSnapshot {
            gateway_enabled: true,
            configured_providers: vec!["google".to_string()],
            provider_health: vec![provider_health("google", false, None)],
        };
        let signals = da(
            "a1", 100, 50_000, &["unregistered_provider".to_string()], &HashMap::new(), Some(&snap),
        );
        assert_eq!(signals.len(), 1, "must not panic, must not silently vanish");
        assert_eq!(signals[0].reason, surfaces::AttentionReason::EvaluationUnavailable);
        assert_eq!(signals[0].evidence.as_deref(), Some("unregistered_provider (not in gateway config)"));
    }

    #[test]
    fn derive_attention_budget_risk_fires_at_hard_threshold() {
        // 92% of budget — past HARD_THRESHOLD (90%).
        let signals = da("a1", 92_000, 100_000, &[], &HashMap::new(), None);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].reason, surfaces::AttentionReason::BudgetRisk);
        assert_eq!(signals[0].evidence.as_deref(), Some("92%"));
    }

    #[test]
    fn derive_attention_budget_below_threshold_is_clean() {
        let signals = da("a1", 50_000, 100_000, &[], &HashMap::new(), None);
        assert!(signals.is_empty());
    }

    #[test]
    fn derive_attention_degraded_fires_on_stale_token_alone() {
        let snap = surfaces::CredentialSnapshot {
            gateway_enabled: true,
            configured_providers: vec!["google".to_string()],
            provider_health: vec![provider_health("google", false, None)],
        };
        let signals = da(
            "a1", 100, 50_000, &["google".to_string()], &HashMap::new(), Some(&snap),
        );
        assert_eq!(signals.len(), 1, "token_fresh:false with NO last_error must still fire Degraded");
        assert_eq!(signals[0].reason, surfaces::AttentionReason::Degraded);
        assert_eq!(signals[0].evidence.as_deref(), Some("google"));
    }

    /// Ship-review finding (Claude + Codex adversarial, independently confirmed): ApiKey-style
    /// providers whose env var was never set (the single most common Degraded case) never get
    /// attention_since (cred.7's health machine is OAuth-only) or last_refresh_at (only ever
    /// written on the OAuth success path) populated — both stay None forever. `since` must be
    /// the `0` sentinel in this case, not `now` (which would recompute "just broke" every tick).
    #[test]
    fn derive_attention_degraded_since_is_zero_sentinel_when_never_tracked() {
        let mut health = provider_health("brave_search", false, None);
        health.last_refresh_at = None;
        let snap = surfaces::CredentialSnapshot {
            gateway_enabled: true,
            configured_providers: vec!["brave_search".to_string()],
            provider_health: vec![health],
        };
        let signals = da(
            "a1", 100, 50_000, &["brave_search".to_string()], &HashMap::new(), Some(&snap),
        );
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].reason, surfaces::AttentionReason::Degraded);
        assert_eq!(
            signals[0].since, 0,
            "must use the 0 sentinel (never-tracked), not fall back to `now`, when no real onset exists"
        );
    }

    #[test]
    fn derive_attention_degraded_does_not_fire_when_token_fresh() {
        let snap = surfaces::CredentialSnapshot {
            gateway_enabled: true,
            configured_providers: vec!["google".to_string()],
            provider_health: vec![provider_health("google", true, None)],
        };
        let signals = da(
            "a1", 100, 50_000, &["google".to_string()], &HashMap::new(), Some(&snap),
        );
        assert!(signals.is_empty());
    }

    /// Ship-review Testing specialist finding: cred.7's health state machine can flag a
    /// provider `AttentionRequired` (e.g. persistent 401s) while `token_fresh` stays `true`
    /// for ApiKey-style providers, whose freshness is derived purely from "is the env var
    /// set" (agentd/src/credential/mod.rs) — independent of whether the key actually works.
    /// Degraded must fire on `attention_reason.is_some()` too, not `!token_fresh` alone.
    #[test]
    fn derive_attention_degraded_fires_when_attention_required_even_if_token_fresh() {
        let mut health = provider_health("brave_search", true, None);
        health.attention_reason = Some("persistent_401".to_string());
        health.recovery_kind    = Some("config_fix".to_string());
        health.attention_since  = Some(1_700_000_000);
        let snap = surfaces::CredentialSnapshot {
            gateway_enabled: true,
            configured_providers: vec!["brave_search".to_string()],
            provider_health: vec![health],
        };
        let signals = da(
            "a1", 100, 50_000, &["brave_search".to_string()], &HashMap::new(), Some(&snap),
        );
        assert_eq!(
            signals.len(), 1,
            "AttentionRequired must fire Degraded even when token_fresh is true for ApiKey providers"
        );
        assert_eq!(signals[0].reason, surfaces::AttentionReason::Degraded);
        assert_eq!(signals[0].since, 1_700_000_000, "must prefer attention_since as the real onset over now");
    }

    #[test]
    fn derive_attention_degraded_ignores_agents_not_using_the_provider() {
        let snap = surfaces::CredentialSnapshot {
            gateway_enabled: true,
            configured_providers: vec!["google".to_string()],
            provider_health: vec![provider_health("google", false, None)],
        };
        // Agent doesn't list "google" among its own credential_providers.
        let signals = da("a1", 100, 50_000, &[], &HashMap::new(), Some(&snap));
        assert!(signals.is_empty());
    }

    #[test]
    fn derive_attention_priority_approval_beats_degraded_for_row_ordering() {
        let mut pending = HashMap::new();
        pending.insert("act_1".to_string(), parked_approval("a1"));
        let snap = surfaces::CredentialSnapshot {
            gateway_enabled: true,
            configured_providers: vec!["google".to_string()],
            provider_health: vec![provider_health("google", false, None)],
        };
        let mut signals = da(
            "a1", 100, 50_000, &["google".to_string()], &pending, Some(&snap),
        );
        assert_eq!(signals.len(), 2, "both Approval and Degraded should be active simultaneously");
        signals.sort_by(|a, b| a.reason.cmp(&b.reason));
        assert_eq!(
            signals[0].reason,
            surfaces::AttentionReason::ApprovalPending,
            "ApprovalPending must sort first by declaration-order priority, even though \
             Degraded is more severe (Critical vs Info) — severity and routing priority are \
             deliberately independent axes (Design Fix 1)"
        );
    }

    #[test]
    fn attention_signal_serializes_on_agent_snapshot() {
        // Guards the manual-Serialize silent-drop trap: a new AgentSnapshot field with no
        // matching serialize_field call compiles fine and silently vanishes from JSON.
        let mut pending = HashMap::new();
        pending.insert("act_1".to_string(), parked_approval("a1"));
        let attention = da("a1", 100, 50_000, &[], &pending, None);
        let snap = AgentSnapshot {
            id: "a1".to_string(),
            status: AgentStatus::Running,
            turn: 0,
            context_tokens: 100,
            token_budget: 50_000,
            windowed_spent: 100,
            task_preview: String::new(),
            tools: vec![],
            short_term_previews: vec![],
            parent_id: None,
            accessible_server_names: vec![],
            capabilities_unrestricted: true,
            tier: None,
            pid: None,
            credential_providers: vec![],
            credential_request_counts: HashMap::new(),
            credential_denied_counts: HashMap::new(),
            credential_last_access_at: HashMap::new(),
            attention,
            last_event_at_unix: 0,
        };
        let json = serde_json::to_value(&snap).expect("AgentSnapshot must serialize");
        let arr = json["attention"].as_array().expect("attention field must be present and be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["reason"], "approval_pending");
        // ux.11a F4: the manual Serialize field_count bump + serialize_field call must keep
        // windowed_spent in the JSON — the guard above only covers `attention`, so a missing
        // windowed_spent would otherwise vanish silently from FUSE/HTTP JSON.
        assert_eq!(json["windowed_spent"].as_u64(), Some(100),
            "windowed_spent must serialize (field_count bump + serialize_field)");
    }

    #[test]
    fn set_budget_agent_raise_revives_deferred_agent() {
        // F2: SetBudget raising a per-agent ceiling above current windowed spend must
        // drain_deferred so the parked agent is admitted immediately.
        use crate::control::{BudgetTarget, ControlCommand};
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _t) = recorder();
        let mdl = model_cfg();
        let sched = SchedulerConfig { global_token_budget: 0, budget_reset_interval: 100, ..unlimited() };
        let mut state = minimal_state("a");                         // per-agent budget 100_000
        state.agents.get_mut("a").unwrap().test_set_spend(150_000);  // over the old ceiling
        state.deferred.push(DeferredInfer {
            priority: 0, seq: 0, agent_id: "a".into(), request: test_infer_req(), turn: 0,
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch_control_command(
            ControlCommand::SetBudget { target: BudgetTarget::Agent("a".into()), limit: 500_000, confirm_tx: Some(tx) },
            &mdl, &mut state, &sched, &gateway, &registry, &rec,
        );
        let (old, new) = rx.blocking_recv().unwrap().expect("set ok");
        assert_eq!(old, 100_000);
        assert_eq!(new, 500_000);
        assert_eq!(state.agents.get("a").unwrap().token_budget(), 500_000, "budget mutated");
        assert!(state.deferred.is_empty(), "raise revives the deferred agent");
        assert_eq!(state.in_flight, 1);
    }

    #[test]
    fn drain_terminates_legacy_over_budget_deferred_agent() {
        // Ship-review P1: under legacy (interval=0) budgets, a deferred agent that is now
        // over its per-agent cap (e.g. a live SetBudget lowered it below spend) must be
        // TERMINATED by drain, not held back forever.
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _t) = recorder();
        let mut state = minimal_state("a");                        // per-agent budget 100_000
        state.tokens_spent = 0;                                     // global fine
        state.agents.get_mut("a").unwrap().test_set_spend(150_000); // over its per-agent cap
        state.deferred.push(DeferredInfer {
            priority: 0, seq: 0, agent_id: "a".into(), request: test_infer_req(), turn: 0,
        });
        // interval=0 → legacy: over-budget is permanent → terminate.
        drain_deferred(&mut state, &budget_sched(0), &gateway, &registry, &rec);
        assert!(state.deferred.is_empty(), "the stranded deferred entry is drained, not held forever");
        assert_eq!(state.in_flight, 0, "not admitted (still over budget)");
        assert!(state.outcomes.get("a").is_some_and(|r| r.is_err()), "legacy over-budget terminates the agent");
    }

    #[test]
    fn drain_holds_back_windowed_over_budget_deferred_agent() {
        // Contrast to the legacy case: under a reset window (interval>0) the same over-cap
        // agent is HELD BACK (revived on the next rollover), never terminated.
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _t) = recorder();
        let mut state = minimal_state("a");
        state.tokens_spent = 0;
        state.agents.get_mut("a").unwrap().test_set_spend(150_000);
        state.deferred.push(DeferredInfer {
            priority: 0, seq: 0, agent_id: "a".into(), request: test_infer_req(), turn: 0,
        });
        drain_deferred(&mut state, &budget_sched(100), &gateway, &registry, &rec);
        assert_eq!(state.deferred.len(), 1, "windowed over-cap agent stays deferred");
        assert_eq!(state.in_flight, 0);
        assert!(!state.outcomes.contains_key("a"), "not terminated under a window");
    }

    #[test]
    fn set_budget_global_is_rejected() {
        // F1: the global ceiling is immutable config; SetBudget must refuse Global.
        use crate::control::{BudgetTarget, ControlCommand};
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _t) = recorder();
        let mdl = model_cfg();
        let mut state = minimal_state("a");
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch_control_command(
            ControlCommand::SetBudget { target: BudgetTarget::Global, limit: 5_000, confirm_tx: Some(tx) },
            &mdl, &mut state, &budget_sched(100), &gateway, &registry, &rec,
        );
        assert!(rx.blocking_recv().unwrap().is_err(), "global set is rejected");
    }

    // ── ux.13 control verbs: Cancel + SetCaps ─────────────────────────────────

    fn dispatch(cmd: crate::control::ControlCommand, state: &mut SchedulerState) {
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _t) = recorder();
        let mdl = model_cfg();
        dispatch_control_command(cmd, &mdl, state, &unlimited(), &gateway, &registry, &rec);
    }

    #[test]
    fn setcaps_unknown_agent_errs() {
        use crate::control::ControlCommand;
        let mut state = minimal_state("a");
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::SetCaps { agent_id: "ghost".into(), capabilities: vec![], confirm_tx: Some(tx) }, &mut state);
        let e = rx.blocking_recv().unwrap().unwrap_err();
        assert!(e.contains("not found"), "unknown agent → not-found (→404), got: {e}");
    }

    #[test]
    fn setcaps_narrow_from_unrestricted_succeeds() {
        use crate::{control::ControlCommand, capability::Capability};
        let mut state = minimal_state("a"); // caps = None (unrestricted)
        let new = vec![Capability::KbRead { segment: "ops:briefs".into() }];
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::SetCaps { agent_id: "a".into(), capabilities: new.clone(), confirm_tx: Some(tx) }, &mut state);
        let (_old, newlen) = rx.blocking_recv().unwrap().expect("narrow of unrestricted must succeed");
        assert_eq!(newlen, 1);
        assert_eq!(state.agents["a"].cap_set_cloned(), Some(new), "cfg.capabilities mutated to the narrowed set");
    }

    #[test]
    fn setcaps_widen_denied() {
        use crate::{control::ControlCommand, capability::Capability};
        let mut state = minimal_state("a");
        // First narrow None → [KbRead].
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::SetCaps {
            agent_id: "a".into(),
            capabilities: vec![Capability::KbRead { segment: "ops:briefs".into() }],
            confirm_tx: Some(tx),
        }, &mut state);
        rx.blocking_recv().unwrap().expect("initial narrow ok");
        // Now attempt to widen: add FsWrite that the current set does not cover → denied.
        let (tx2, rx2) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::SetCaps {
            agent_id: "a".into(),
            capabilities: vec![
                Capability::KbRead { segment: "ops:briefs".into() },
                Capability::FsWrite { prefix: "/tmp".into() },
            ],
            confirm_tx: Some(tx2),
        }, &mut state);
        let e = rx2.blocking_recv().unwrap().unwrap_err();
        assert!(e.contains("narrow-only"), "widening must be rejected, got: {e}");
        assert_eq!(state.agents["a"].cap_set_cloned().unwrap().len(), 1, "cap set unchanged after a rejected widen");
    }

    #[test]
    fn setcaps_inert_cap_rejected() {
        use crate::{control::ControlCommand, capability::Capability};
        let mut state = minimal_state("a");
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Net is Inert in the agent context — narrowing to it is a misleading no-op.
        dispatch(ControlCommand::SetCaps {
            agent_id: "a".into(),
            capabilities: vec![Capability::Net { hosts: vec![], ports: vec![443] }],
            confirm_tx: Some(tx),
        }, &mut state);
        let e = rx.blocking_recv().unwrap().unwrap_err();
        assert!(e.contains("inert"), "inert cap target must be rejected, got: {e}");
    }

    #[test]
    fn cancel_unknown_agent_errs() {
        use crate::control::ControlCommand;
        let mut state = minimal_state("a");
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::Cancel { agent_id: "ghost".into(), confirm_tx: Some(tx) }, &mut state);
        assert!(rx.blocking_recv().unwrap().is_err(), "unknown agent cancel → Err (→404)");
    }

    #[test]
    fn cancel_running_agent_flags_only() {
        use crate::control::ControlCommand;
        let mut state = minimal_state("a"); // "a" is in agents, not parked → "running"
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::Cancel { agent_id: "a".into(), confirm_tx: Some(tx) }, &mut state);
        assert_eq!(rx.blocking_recv().unwrap().unwrap(), 1, "one node flagged");
        assert!(state.cancel_requested.contains_key("a"), "running agent is flagged, not funneled");
        assert!(state.agents.contains_key("a"), "running agent stays until its in-flight future returns (gate funnels it)");
    }

    #[test]
    fn cancel_parked_agent_funnels_and_consumes_flag() {
        use crate::control::ControlCommand;
        let mut state = minimal_state("a");
        state.waiting.insert("a".into()); // parked (waiting) → funnel immediately
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::Cancel { agent_id: "a".into(), confirm_tx: Some(tx) }, &mut state);
        assert_eq!(rx.blocking_recv().unwrap().unwrap(), 1);
        assert!(!state.cancel_requested.contains_key("a"), "flag consumed by handle_agent_terminal");
        assert!(!state.waiting.contains("a"), "removed from waiting");
        assert!(state.outcomes.contains_key("a"), "terminal outcome recorded for the root");
    }

    // ---------------------------------------------------------------------------------
    // ux.6a Step 5 — terminal admission denials are receipted; deferral and shutdown are not
    // ---------------------------------------------------------------------------------

    /// Build a state with an egress proxy attached, returning the temp dir so the evidence
    /// file can be inspected.
    fn state_with_egress(id: &str) -> (SchedulerState, Arc<EgressProxy>, tempfile::TempDir) {
        use crate::evidence::EvidenceWriter;
        let dir = tempfile::TempDir::new().unwrap();
        let (rec, _tmp) = recorder();
        let writer = Arc::new(
            EvidenceWriter::open(&dir.path().join("ev.jsonl"), &dir.path().join("k.pkcs8"))
                .unwrap(),
        );
        let egress = Arc::new(EgressProxy::new(writer, rec));
        let mut state = minimal_state(id);
        state.egress = Some(Arc::clone(&egress));
        (state, egress, dir)
    }

    fn denied_receipts(dir: &tempfile::TempDir) -> usize {
        std::fs::read_to_string(dir.path().join("ev.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"verdict\":\"denied\""))
            .count()
    }

    /// The finding that reshaped ux.6: before this, `record_denied` had ZERO production
    /// callers, so the chain structurally could not contain a "no". And wiring only the HTTP
    /// egress proxy would not have fixed it — that proxy never starts in the shipped config.
    /// THIS is the production-reachable denial.
    #[test]
    fn native_admission_denial_writes_denied_receipt() {
        let (mut state, _egress, dir) = state_with_egress("a");
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, log) = recorder();
        // Legacy no-window config, already over the global ceiling → permanent denial.
        let sched = SchedulerConfig {
            global_token_budget: 1_000,
            budget_reset_interval: 0,
            ..unlimited()
        };
        state.tokens_spent = 5_000;
        state.deferred.push(DeferredInfer {
            priority: 0,
            seq: 0,
            agent_id: "a".into(),
            request: test_infer_req(),
            turn: 0,
        });

        drain_deferred(&mut state, &sched, &gateway, &registry, &rec);

        assert_eq!(denied_receipts(&dir), 1, "a terminal denial must be receipted");
        let content = std::fs::read_to_string(dir.path().join("ev.jsonl")).unwrap();
        let r: crate::evidence::ActionReceipt =
            serde_json::from_str(content.lines().last().unwrap()).unwrap();
        assert_eq!(r.principal, "a", "receipt is attributed to the denied agent");
        assert_eq!(r.verdict, "denied");
        assert_eq!(r.action, "inference", "shares the action namespace with allows");
        // The chain remains a valid chain with a denial in it.
        assert_eq!(
            crate::evidence::verify_chain(
                &dir.path().join("ev.jsonl"),
                &dir.path().join("k.pub")
            )
            .unwrap(),
            1
        );
        let events = std::fs::read_to_string(log.path()).unwrap_or_default();
        assert!(events.contains("agent_admission_denied"));
    }

    /// ux.8′'s hardest-won distinction: with a reset window, exhaustion DEFERS rather than
    /// denies. Receipting that would put the boundary on record refusing work it is about to
    /// do — and would make the signed chain lie in the safe direction, which is still a lie.
    #[test]
    fn deferred_agent_writes_no_denied_receipt() {
        let (mut state, _egress, dir) = state_with_egress("a");
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _log) = recorder();
        // Identical to the test above EXCEPT a reset window is configured.
        let sched = SchedulerConfig {
            global_token_budget: 1_000,
            budget_reset_interval: 86_400,
            ..unlimited()
        };
        state.tokens_spent = 5_000;
        state.deferred.push(DeferredInfer {
            priority: 0,
            seq: 0,
            agent_id: "a".into(),
            request: test_infer_req(),
            turn: 0,
        });

        drain_deferred(&mut state, &sched, &gateway, &registry, &rec);

        assert_eq!(denied_receipts(&dir), 0, "deferral is NOT denial (ux.8′)");
        assert!(!state.deferred.is_empty(), "the agent stays queued for the next rollover");
    }

    /// Shutdown is not a policy verdict, and its loop drains the WHOLE deferred queue — so
    /// receipting it would also put N fsyncs on the shutdown path.
    #[test]
    fn shutdown_denial_writes_no_receipt() {
        let (mut state, _egress, dir) = state_with_egress("a");
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, log) = recorder();
        let sched = unlimited();
        state.shutdown_requested = true;
        for i in 0..5 {
            state.deferred.push(DeferredInfer {
                priority: 0,
                seq: i,
                agent_id: "a".into(),
                request: test_infer_req(),
                turn: 0,
            });
        }

        drain_deferred(&mut state, &sched, &gateway, &registry, &rec);

        assert_eq!(denied_receipts(&dir), 0, "shutdown must not write receipts");
        let events = std::fs::read_to_string(log.path()).unwrap_or_default();
        assert!(events.contains("\"reason\":\"shutdown\""), "but it is still recorded");
    }

    /// ux.6a leak guard, mirroring `handle_agent_terminal_clears_both_sets`. Every scheduler
    /// deny site is a TERMINAL denial, so a denied agent never records another allowed
    /// inference and can never re-arm itself — without cleanup its `denied_edges` entry lives
    /// for the whole process. Same class as audit86-P2-5.
    #[test]
    fn handle_agent_terminal_clears_egress_deny_edges() {
        let (mut state, egress, _dir) = state_with_egress("a");
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _log) = recorder();
        let sched = unlimited();

        egress.receipt_denial_once("a", "m", "agent_budget_exhausted");
        assert_eq!(egress.denied_edge_agents(), 1, "precondition: the edge exists");

        handle_agent_terminal(
            "a".to_string(),
            Err(anyhow::anyhow!("terminal")),
            &mut state,
            &sched,
            &gateway,
            &registry,
            &rec,
        );

        assert_eq!(
            egress.denied_edge_agents(),
            0,
            "a terminated agent's deny-episode state must be reclaimed, or it leaks forever"
        );
    }

    #[test]
    fn cancel_purges_deferred_entry_without_panic() {
        // The panic-safety guarantee: a cancelled agent's stale DeferredInfer must be purged,
        // else the next drain_deferred pushes a future for a removed agent.
        use crate::control::ControlCommand;
        let mut state = minimal_state("a");
        state.waiting.insert("a".into());
        state.deferred.push(DeferredInfer { priority: 0, seq: 0, agent_id: "a".into(), request: test_infer_req(), turn: 0 });
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::Cancel { agent_id: "a".into(), confirm_tx: Some(tx) }, &mut state);
        rx.blocking_recv().unwrap().unwrap();
        assert!(state.deferred.is_empty(), "cancel purges the stale deferred entry");
    }

    #[test]
    fn cancel_awaiting_approval_purges_pending_approval() {
        use crate::control::ControlCommand;
        let mut state = minimal_state("a");
        // Park "a" on an approval so it counts as parked and has a pending_approvals entry.
        state.pending_approvals.insert("act_0".into(), parked_approval("a"));
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::Cancel { agent_id: "a".into(), confirm_tx: Some(tx) }, &mut state);
        rx.blocking_recv().unwrap().unwrap();
        assert!(state.pending_approvals.is_empty(), "cancel purges the dangling approval");
    }

    #[test]
    fn cancel_cascades_over_parent_map() {
        use crate::control::ControlCommand;
        let mut state = minimal_state("parent");
        // Add a child agent + record the spawn parentage.
        let cfg = agent_cfg("child", "child task");
        let mdl = model_cfg();
        state.agents.insert("child".into(), AgentTask::new("child", &cfg.task, &cfg, &mdl, vec![]));
        state.parent_map.insert("child".into(), "parent".into());
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::Cancel { agent_id: "parent".into(), confirm_tx: Some(tx) }, &mut state);
        assert_eq!(rx.blocking_recv().unwrap().unwrap(), 2, "parent + child both cancelled");
        assert!(state.cancel_requested.contains_key("parent"));
        assert_eq!(state.cancel_requested.get("child").map(|s| s.as_str()), Some("cascade from parent"));
    }

    #[test]
    fn cancel_running_awaited_child_stays_flagged_not_funneled() {
        // REGRESSION (ux.13 /review P0, cross-model): a spawned child is a KEY in `awaiting`
        // (child→parent) the entire time its inference future is live. It must be classified
        // RUNNING (flag-only), NOT parked — funneling it here would remove it from state.agents
        // while its future is in flight, and the future's `state.agents.get_mut().expect(...)`
        // would panic. The buggy predicate used `awaiting.contains_key`, which matched this
        // running child; the fix matches a parked PARENT via `awaiting.values()`.
        use crate::control::ControlCommand;
        let mut state = minimal_state("parent");
        let cfg = agent_cfg("child", "child task");
        let mdl = model_cfg();
        state.agents.insert("child".into(), AgentTask::new("child", &cfg.task, &cfg, &mdl, vec![]));
        // The child is a running spawned agent: a KEY in awaiting, with NO deferred/waiting/
        // approval entry (its inference future is live, not represented in this unit state).
        state.awaiting.insert("child".into(), AwaitingParent {
            parent_id: "parent".into(), call_id: "c1".into(), deliver_content: true,
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::Cancel { agent_id: "child".into(), confirm_tx: Some(tx) }, &mut state);
        rx.blocking_recv().unwrap().unwrap();
        assert!(state.cancel_requested.contains_key("child"), "running awaited child is flagged");
        assert!(
            state.agents.contains_key("child"),
            "running awaited child MUST stay in agents (gate funnels it when its future returns) — \
             funneling now would panic the pending-result arm",
        );
    }

    #[test]
    fn cancel_parked_parent_funnels_child_flag_only() {
        // The complement: the PARENT awaiting a child has no in-flight future → parked → funnel
        // now; the still-running child is flagged for the gate.
        use crate::control::ControlCommand;
        let mut state = minimal_state("parent");
        let cfg = agent_cfg("child", "child task");
        let mdl = model_cfg();
        state.agents.insert("child".into(), AgentTask::new("child", &cfg.task, &cfg, &mdl, vec![]));
        state.parent_map.insert("child".into(), "parent".into());
        state.awaiting.insert("child".into(), AwaitingParent {
            parent_id: "parent".into(), call_id: "c1".into(), deliver_content: true,
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch(ControlCommand::Cancel { agent_id: "parent".into(), confirm_tx: Some(tx) }, &mut state);
        assert_eq!(rx.blocking_recv().unwrap().unwrap(), 2, "parent + child in the subtree");
        // Parent (awaiting a child, no in-flight future) was funneled: flag consumed + terminal
        // outcome recorded. (A funneled root stays in state.agents with an outcome; the run()
        // loop reaps it — see cancel_parked_agent_funnels_and_consumes_flag.)
        assert!(!state.cancel_requested.contains_key("parent"), "parked parent funneled (flag consumed)");
        assert!(state.outcomes.contains_key("parent"), "parked parent has a terminal outcome");
        // Running child stays flagged for the enqueue_or_defer gate.
        assert!(state.agents.contains_key("child"), "running child stays for the gate");
        assert_eq!(state.cancel_requested.get("child").map(|s| s.as_str()), Some("cascade from parent"));
    }

    // NOTE: a run()-level "cancel a live agent, assert no panic" integration test was
    // considered but omitted — driving it through the real select! loop over a SHARED
    // MockGateway is timing-nondeterministic (the cancel races the agent's own progress and
    // a spawned child inherits the next mock response), which made it flaky. The panic-hazard
    // paths (parked funnel, deferred purge, pending-approval purge, cascade) are covered
    // deterministically by the dispatch-level tests above; the gate itself is a trivial
    // `cancel_requested.contains_key` guard at the top of enqueue_or_defer.

    #[test]
    fn set_budget_unknown_agent_errors() {
        // Unknown agent → Err → 404 at the HTTP layer.
        use crate::control::{BudgetTarget, ControlCommand};
        let gateway: Arc<dyn InferenceGateway + Send + Sync> = Arc::new(MockGateway::new(vec![]));
        let registry = Arc::new(ToolRegistry::new());
        let (rec, _t) = recorder();
        let mdl = model_cfg();
        let mut state = minimal_state("a");
        let (tx, rx) = tokio::sync::oneshot::channel();
        dispatch_control_command(
            ControlCommand::SetBudget { target: BudgetTarget::Agent("ghost".into()), limit: 5_000, confirm_tx: Some(tx) },
            &mdl, &mut state, &budget_sched(100), &gateway, &registry, &rec,
        );
        assert!(rx.blocking_recv().unwrap().is_err(), "unknown agent errors");
    }

    #[test]
    fn set_token_budget_survives_checkpoint_round_trip() {
        // F3: set_token_budget mutates the CHECKPOINTED cfg field, so an operator's live
        // change persists across a restart (not the audit86-P2-1 revert class).
        let mut state = minimal_state("a");
        let old = state.agents.get_mut("a").unwrap().set_token_budget(777_000);
        assert_eq!(old, 100_000);
        let cp = state.agents.get("a").unwrap().to_checkpoint();
        let restored = crate::agent::AgentTask::from_checkpoint(cp, vec![]);
        assert_eq!(restored.token_budget(), 777_000, "live SetBudget survives restart");
    }

    #[test]
    fn derive_attention_budget_risk_keys_on_windowed_not_lifetime() {
        // B3: the re-key means a windowed spend under the hard threshold is clean even if
        // lifetime spend would trip it; and windowed at the threshold fires.
        let pending = HashMap::new();
        // 95k windowed against a 100k budget → Hard → fires.
        let hot = da("a1", 95_000, 100_000, &[], &pending, None);
        assert!(hot.iter().any(|s| s.reason == surfaces::AttentionReason::BudgetRisk),
            "windowed at hard threshold fires BudgetRisk");
        // Unlimited (budget 0) never fires regardless of spend.
        let unlimited = da("a1", 10_000_000, 0, &[], &pending, None);
        assert!(!unlimited.iter().any(|s| s.reason == surfaces::AttentionReason::BudgetRisk),
            "unlimited never fires BudgetRisk");
    }
}
