use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;

/// Device-level isolation capability report, computed once at startup by
/// `agentd::isolation_caps::probe()` and propagated into the snapshot.
///
/// Surfaces are Serialize-only; agentctl defines its own Deserialize struct.
#[derive(Clone, Serialize)]
pub struct IsolationCapsSummary {
    /// Absolute path to the `runsc` binary, or None when gVisor is not installed.
    pub runsc:     Option<String>,
    /// True when the kernel supports any Landlock ABI (≥ 1, Linux ≥ 5.13).
    pub landlock:  bool,
    /// True when this build can enforce seccomp-bpf rules (x86_64 Linux only).
    pub seccomp:   bool,
    /// CPU architecture string from `std::env::consts::ARCH` (e.g. "x86_64", "aarch64").
    pub arch:      String,
    /// Coarse device-level tier: "full" | "capability" | "none".
    pub tier:      String,
}

impl Default for IsolationCapsSummary {
    fn default() -> Self {
        Self {
            runsc:    None,
            landlock: false,
            seccomp:  false,
            arch:     std::env::consts::ARCH.to_string(),
            tier:     "none".to_string(),
        }
    }
}

/// A point-in-time snapshot of all running agents, written by the scheduler
/// and read by the FUSE handler. Uses std::sync::RwLock (not tokio) because
/// the FUSE handler runs on a plain OS thread, not inside a tokio runtime.
pub type SharedSnapshot = Arc<RwLock<SchedulerSnapshot>>;

/// Per-MCP-server sandbox enforcement record, populated at startup.
#[derive(Clone, Default, Serialize)]
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
#[derive(Clone, Default, Serialize)]
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

/// System-wide credential surface snapshot (cred.5).
///
/// Per-agent credential usage is embedded in `AgentSnapshot` fields (consistent
/// with the `accessible_server_names` pattern); this struct carries system-wide
/// gateway health and per-provider status only.
#[derive(Clone, Default, Serialize)]
pub struct CredentialSnapshot {
    /// True when `credential_gateway.enabled = true` in config.
    pub gateway_enabled:      bool,
    /// Provider names configured at startup (from `cfg.providers.keys()`).
    pub configured_providers: Vec<String>,
    /// Per-provider health; one entry per configured provider.
    pub provider_health:      Vec<ProviderHealth>,
}

/// Health of a single credential provider.
#[derive(Clone, Serialize)]
pub struct ProviderHealth {
    pub name:            String,
    /// True when a non-expired OAuth token is cached, or (for api-key providers)
    /// when the key env-var was non-empty at startup.
    pub token_fresh:     bool,
    /// Unix secs of last successful token refresh. `None` for api-key providers.
    pub last_refresh_at: Option<u64>,
    /// Unix secs of token expiry from the in-memory cache. `None` for api-key providers.
    pub expires_at:      Option<u64>,
    /// Last refresh-error string; cleared on the next successful refresh. `None` when healthy.
    pub last_error:      Option<String>,
}

/// A pending operator approval, projected from the scheduler into the snapshot.
/// Used by the FUSE `/agents/approvals` file and the `agentctl` Approvals view.
#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Default, Serialize)]
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
    /// Device-level isolation capabilities, populated at startup before FUSE mount.
    /// None until isolation_caps::probe() runs (typically available immediately).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation_caps:      Option<IsolationCapsSummary>,
    /// System-wide credential surface snapshot (cred.5). None when gateway is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_snapshot: Option<CredentialSnapshot>,
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
    // ── Credential grant fields (cred.5) — empty when no credential grant exists ──
    /// Provider names this agent's ephemeral token grants access to.
    pub credential_providers:      Vec<String>,
    /// Successful credential request count per provider since last spawn.
    pub credential_request_counts: HashMap<String, u64>,
    /// Denied credential request count per provider since last spawn.
    pub credential_denied_counts:  HashMap<String, u64>,
    /// Unix secs of last successful credential request per provider.
    pub credential_last_access_at: HashMap<String, u64>,
}

/// Manual Serialize: emits `status` as the flat string from `as_str()` (matching the FUSE
/// text format) plus an optional `status_detail` field for tuple variants.
impl Serialize for AgentSnapshot {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let detail: Option<&str> = match &self.status {
            AgentStatus::AwaitingChild(s) | AgentStatus::AwaitingApproval(s) => Some(s.as_str()),
            _ => None,
        };
        let field_count = 17 + usize::from(detail.is_some());
        let mut s = ser.serialize_struct("AgentSnapshot", field_count)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("status", self.status.as_str())?;
        if let Some(d) = detail {
            s.serialize_field("status_detail", d)?;
        }
        s.serialize_field("turn", &self.turn)?;
        s.serialize_field("context_tokens", &self.context_tokens)?;
        s.serialize_field("token_budget", &self.token_budget)?;
        s.serialize_field("task_preview", &self.task_preview)?;
        s.serialize_field("tools", &self.tools)?;
        s.serialize_field("short_term_previews", &self.short_term_previews)?;
        s.serialize_field("parent_id", &self.parent_id)?;
        s.serialize_field("accessible_server_names", &self.accessible_server_names)?;
        s.serialize_field("capabilities_unrestricted", &self.capabilities_unrestricted)?;
        s.serialize_field("tier", &self.tier)?;
        s.serialize_field("pid", &self.pid)?;
        s.serialize_field("credential_providers", &self.credential_providers)?;
        s.serialize_field("credential_request_counts", &self.credential_request_counts)?;
        s.serialize_field("credential_denied_counts", &self.credential_denied_counts)?;
        s.serialize_field("credential_last_access_at", &self.credential_last_access_at)?;
        s.end()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentStatus {
    Running,
    /// Orchestrated agent parked after completing a turn, awaiting next inject.
    Waiting,
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
            AgentStatus::Waiting               => "waiting",
            AgentStatus::Deferred              => "deferred",
            AgentStatus::AwaitingChild(_)      => "awaiting_child",
            AgentStatus::AwaitingApproval(_)   => "awaiting_approval",
            AgentStatus::Done                  => "done",
            AgentStatus::Failed                => "failed",
        }
    }
}
