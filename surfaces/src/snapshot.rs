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
}

#[derive(Clone)]
pub struct AgentSnapshot {
    pub id:            String,
    pub status:        AgentStatus,
    pub turn:          u32,
    pub context_tokens: u64,
    pub token_budget:  u64,
    pub task_preview:  String,
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
