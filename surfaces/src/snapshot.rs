use std::sync::{Arc, RwLock};

/// A point-in-time snapshot of all running agents, written by the scheduler
/// and read by the FUSE handler. Uses std::sync::RwLock (not tokio) because
/// the FUSE handler runs on a plain OS thread, not inside a tokio runtime.
pub type SharedSnapshot = Arc<RwLock<SchedulerSnapshot>>;

/// Per-MCP-server sandbox enforcement record, populated at startup.
#[derive(Clone, Default)]
pub struct ServerEnforcement {
    pub name:             String,
    /// "stdio" | "http" — populated from McpBackend::transport_kind().
    pub transport:        String,
    /// "none" | "gvisor"
    pub isolation:        String,
    pub landlock:         bool,
    pub seccomp:          bool,
    /// "fork_vfork_only" | "none"
    pub spawn_enforcement: String,
    pub namespace_net:    bool,
    pub namespace_mount:  bool,
    pub landlock_net:     bool,
}

/// Startup-time sandbox posture summary for the whole system.
/// Reflects what was compiled and applied when MCP servers were spawned;
/// not a live runtime probe of current kernel enforcement state.
#[derive(Clone, Default)]
pub struct SandboxSummary {
    /// True when at least one MCP server had sandbox rules applied.
    pub any_sandboxed:  bool,
    /// Per-server enforcement records (one per configured MCP server).
    pub servers:        Vec<ServerEnforcement>,
    /// Canonical degradation strings for known policy gaps.
    /// "landlock_net_unavailable" — deny-all network fallback active (Landlock V4 unavailable).
    /// "spawn_enforcement_unavailable_arch" — DenySpawn no-op on non-x86_64.
    pub degradations:   Vec<String>,
}

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
    /// Startup-time sandbox posture. Set once in main.rs after MCP servers spawn.
    pub sandbox:             SandboxSummary,
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
    /// Parent agent ID if this agent was spawned by another agent; None for
    /// top-level agents loaded from config.
    pub parent_id: Option<String>,
    /// Names of MCP servers this agent has Mcp-capability access to.
    /// Used to build the per-agent sandbox view (agent → server → enforcement chain).
    pub accessible_server_names: Vec<String>,
    /// True when the agent's capabilities field is None (unrestricted access to all
    /// registered servers). Disambiguates from an empty accessible_server_names list.
    pub capabilities_unrestricted: bool,
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
