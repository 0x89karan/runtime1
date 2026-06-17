use std::sync::{Arc, RwLock};

/// A point-in-time snapshot of all running agents, written by the scheduler
/// and read by the FUSE handler. Uses std::sync::RwLock (not tokio) because
/// the FUSE handler runs on a plain OS thread, not inside a tokio runtime.
pub type SharedSnapshot = Arc<RwLock<SchedulerSnapshot>>;

#[derive(Clone, Default)]
pub struct SchedulerSnapshot {
    pub agents:              Vec<AgentSnapshot>,
    pub global_tokens_spent: u64,
    pub in_flight:           usize,
    /// Number of agents currently deferred (waiting for an inference slot).
    pub queue_depth:         usize,
    /// Model identifier for the configured inference backend (e.g. "claude-sonnet-4-6").
    /// Set once at startup before the FUSE mount; empty string until then.
    pub provider_model:      String,
    /// True if at least one MCP server had a kernel sandbox applied at startup.
    pub sandbox_applied:     bool,
}

#[derive(Clone)]
pub struct AgentSnapshot {
    pub id:            String,
    pub status:        AgentStatus,
    pub turn:          u32,
    pub context_tokens: u64,
    pub token_budget:  u64,
    pub task_preview:  String,
    /// Capability-filtered list of tool names available to this agent.
    pub tools:         Vec<String>,
    /// Bounded preview of Tier-2 short-term memory (max 20 items).
    /// Each entry is formatted as `"t{turn} {role}: {content_preview}"`.
    /// Empty when the agent has no paged turns or memory is disabled.
    pub short_term_previews: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentStatus {
    Running,
    Deferred,
    AwaitingChild(String),
    Done,
    Failed,
}

impl AgentStatus {
    pub fn as_str(&self) -> &str {
        match self {
            AgentStatus::Running           => "running",
            AgentStatus::Deferred          => "deferred",
            AgentStatus::AwaitingChild(_)  => "awaiting_child",
            AgentStatus::Done              => "done",
            AgentStatus::Failed            => "failed",
        }
    }
}
