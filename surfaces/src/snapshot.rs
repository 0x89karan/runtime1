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

/// A pending operator approval, projected from the scheduler into the snapshot.
/// Used by the FUSE `/agents/approvals` file and the `agentctl` Approvals view.
#[derive(Clone, Debug)]
pub struct PendingActionView {
    /// Unique approval ID: "act_{seq}".
    pub id:        String,
    /// ID of the agent waiting for resolution.
    pub agent_id:  String,
    /// Action kind, e.g. "write_file".
    pub kind:      String,
    /// Operator-visible severity: "low" | "medium" | "high".
    pub risk:      String,
    /// One-sentence summary of the proposed action.
    pub summary:   String,
    /// JSON-serialized args the agent will pass to the underlying tool.
    pub args_json: String,
    /// Seconds elapsed since the approval was requested.
    pub age_secs:  u64,
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
    /// Current approval queue (bounded to ≤100 entries).
    pub pending_actions:     Vec<PendingActionView>,
    /// Bound address of the HTTP egress proxy, e.g. "http://127.0.0.1:9100".
    /// None when the proxy is not configured.
    pub egress_addr:         Option<String>,
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
    /// Execution tier: "native" | "universal". None defaults to "native" for backward compat.
    pub tier: Option<String>,
    /// PID of the child process for universal-tier agents. None for native-tier agents.
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentStatus {
    Running,
    Deferred,
    AwaitingChild(String),
    /// Agent called `request_approval` and is parked until resolved.
    /// The String is the approval ID (e.g. "act_3").
    AwaitingApproval(String),
    Done,
    Failed,
}

impl AgentStatus {
    pub fn as_str(&self) -> &str {
        match self {
            AgentStatus::Running               => "running",
            AgentStatus::Deferred              => "deferred",
            AgentStatus::AwaitingChild(_)      => "awaiting_child",
            AgentStatus::AwaitingApproval(_)   => "awaiting_approval",
            AgentStatus::Done                  => "done",
            AgentStatus::Failed                => "failed",
        }
    }
}
