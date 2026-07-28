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
    // ── cred.7 resilience fields ──────────────────────────────────────────────
    /// Human-readable reason the provider entered AttentionRequired; None when healthy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention_reason: Option<String>,
    /// Recovery action the operator should take: "reauth" | "config_fix" | "secret_replace".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_kind:    Option<String>,
    /// Unix secs when the provider first entered AttentionRequired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention_since:  Option<u64>,
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
    /// Is `[scheduler] budget_reset_interval > 0` (ux.13-TUI)? When false, per-agent budget
    /// exhaustion TERMINATES the agent instead of deferring it until the next window — so a
    /// budget-based soft stop ("Park" in `agentctl watch`) is not reversible on this deployment.
    /// Default `false`, matching the config default: only the CoS configs set an interval.
    pub budget_resettable:   bool,
}

/// Why an agent's `attention` signal fired — see `docs/plans/ux.2-attention-evidence.md`.
///
/// Declaration order is also tie-break/routing-priority order (highest first): a row with
/// both `ApprovalPending` and `Degraded` active always resolves ties toward `ApprovalPending`.
/// `Error` (ux.2b) sits above `BudgetRisk` — a tool error the operator can act on outranks a
/// budget warning — and `Idle` (ux.2b) is last: least urgent, a "went quiet" liveness hint.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AttentionReason {
    ApprovalPending,
    Degraded,
    /// A tool call returned an error while the agent kept running (ux.2b). Inference errors
    /// terminate the agent (→ `Failed` status), so they never surface here; this is the
    /// "tool errored, agent is still going" case, auto-cleared on the next all-ok tool batch.
    Error,
    BudgetRisk,
    /// A signal source itself couldn't be read this cycle (e.g. the credential gateway
    /// snapshot was unavailable) — never silently rendered as "clean," always its own signal.
    EvaluationUnavailable,
    /// No completed progress event in `IDLE_THRESHOLD_SECS` (ux.2b). Computed READ-TIME from
    /// the carried `last_event_at_unix` (see `AgentSnapshot::idle_signal`), never at snapshot
    /// build — a build-time computation would freeze in the exact hung-tool wedge this catches.
    Idle,
}

/// Row-color severity — a SEPARATE axis from routing priority (`AttentionReason`'s declaration
/// order). E.g. `Degraded` is `Critical` (red) but does not win routing over `ApprovalPending`
/// (`Info`/cyan), which is more actionable even though less severe. Computed from `reason`
/// rather than stored, so severity can never drift out of sync with the reason it describes.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSeverity {
    Info,
    Warning,
    Critical,
}

impl AttentionReason {
    pub fn severity(&self) -> AttentionSeverity {
        match self {
            AttentionReason::ApprovalPending       => AttentionSeverity::Info,
            AttentionReason::Degraded              => AttentionSeverity::Critical,
            AttentionReason::Error                 => AttentionSeverity::Critical,
            AttentionReason::BudgetRisk             => AttentionSeverity::Warning,
            AttentionReason::EvaluationUnavailable => AttentionSeverity::Warning,
            AttentionReason::Idle                  => AttentionSeverity::Warning,
        }
    }

    /// Short, human-readable label for the stacked reason line (the `{reason}` in
    /// `⚠ {reason} · {since} ago`). `evidence` supplies the specific detail (approval ID,
    /// provider name); this supplies the fixed part of the sentence.
    pub fn label(&self) -> &'static str {
        match self {
            AttentionReason::ApprovalPending       => "approval pending",
            AttentionReason::Degraded              => "degraded",
            AttentionReason::Error                 => "error",
            AttentionReason::BudgetRisk             => "budget risk",
            AttentionReason::EvaluationUnavailable => "evaluation unavailable",
            AttentionReason::Idle                  => "idle",
        }
    }
}

/// One active attention signal for an agent. An agent's `attention: Vec<AttentionSignal>` is
/// empty for "clean" (evaluated, nothing active) and non-empty for "needs attention" — the
/// distinction between "clean" and "couldn't evaluate" is carried by `EvaluationUnavailable`
/// being present, never by absence alone (see `docs/plans/ux.2-attention-evidence.md`'s Design
/// Review, Pass 2 — collapsing "clean" and "unavailable" into the same empty state was a
/// CRITICAL finding in that review and must not regress).
#[derive(Clone, Debug, Serialize)]
pub struct AttentionSignal {
    pub reason:   AttentionReason,
    /// Unix seconds when this signal is believed to have started (approximate for
    /// signals derived from a monotonic `Instant`; exact for signals with a real
    /// wall-clock source like `ProviderHealth.last_refresh_at`).
    pub since:    u64,
    /// Short pointer to more context: an approval ID, a provider name, a budget
    /// percentage — whatever disambiguates *which* instance of this reason fired.
    pub evidence: Option<String>,
}

#[derive(Clone)]
pub struct AgentSnapshot {
    pub id:            String,
    pub status:        AgentStatus,
    pub turn:          u32,
    pub context_tokens: u64,
    pub token_budget:  u64,
    /// Spend within the current budget window (ux.11a): `context_tokens − window_anchor`.
    /// This is what the per-agent budget is enforced against; equals lifetime spend under
    /// legacy (interval=0) budgets. 0 for universal-tier agents (proxy-tracked spend).
    pub windowed_spent: u64,
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
    /// Active attention signals (ux.2a) — empty means "evaluated, clean," NOT "not evaluated."
    /// See `AttentionReason::EvaluationUnavailable` for the "couldn't tell" case.
    pub attention: Vec<AttentionSignal>,
    /// Unix seconds of this agent's last completed progress event (ux.2b), derived at snapshot
    /// build from the runtime-only `AgentTask.last_event_at` monotonic clock. The `Idle` signal
    /// is computed READ-TIME from this by `idle_signal`, so it advances between reads without a
    /// new snapshot (0 for universal-tier agents, which are never idle-evaluated).
    pub last_event_at_unix: u64,
}

/// Default seconds of no completed progress event before an agent flags `Idle` (ux.2b). A
/// tool/inference legitimately running longer than this DOES flag — intended: an operator wants
/// to see a tool hanging for minutes (the cos-ux-01 wedge). Read-time only; tune here.
pub const IDLE_THRESHOLD_SECS: u64 = 180;

impl AgentSnapshot {
    /// READ-TIME `Idle` signal: `Some` when this agent is `Running` and has had no completed
    /// progress event for more than `threshold_secs` as measured against the reader's `now_unix`.
    /// Computed per read (not at snapshot build) so idle is never frozen at the last snapshot —
    /// the load-bearing correctness property for the hung-tool wedge (ux.2b M1). The allowlist is
    /// `Running` only: every parked status (`Waiting`/`Deferred`/`AwaitingChild`/`AwaitingApproval`)
    /// is intentionally quiet, and terminal (`Done`/`Failed`) agents aren't "wedged."
    pub fn idle_signal(&self, now_unix: u64, threshold_secs: u64) -> Option<AttentionSignal> {
        if self.status != AgentStatus::Running {
            return None;
        }
        if now_unix.saturating_sub(self.last_event_at_unix) > threshold_secs {
            Some(AttentionSignal {
                reason:   AttentionReason::Idle,
                since:    self.last_event_at_unix,
                evidence: None,
            })
        } else {
            None
        }
    }
}

/// Manual Serialize: emits `status` as the flat string from `as_str()` (matching the FUSE
/// text format) plus an optional `status_detail` field for tuple variants.
///
/// ⚠ Adding a field to `AgentSnapshot`? You MUST add a matching `serialize_field` call below
/// AND bump `field_count` — this impl is hand-written, not derived, so a forgotten field
/// silently vanishes from both FUSE and management-API JSON with no compile error (this bit
/// ux.2's own design phase once already; there is a regression test for this in
/// `agentd/src/scheduler.rs`'s `attention_signal_serializes_on_agent_snapshot` — do not remove it).
impl Serialize for AgentSnapshot {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let detail: Option<&str> = match &self.status {
            AgentStatus::AwaitingChild(s) | AgentStatus::AwaitingApproval(s) => Some(s.as_str()),
            _ => None,
        };
        let field_count = 20 + usize::from(detail.is_some());
        let mut s = ser.serialize_struct("AgentSnapshot", field_count)?;
        s.serialize_field("id", &self.id)?;
        s.serialize_field("status", self.status.as_str())?;
        if let Some(d) = detail {
            s.serialize_field("status_detail", d)?;
        }
        s.serialize_field("turn", &self.turn)?;
        s.serialize_field("context_tokens", &self.context_tokens)?;
        s.serialize_field("token_budget", &self.token_budget)?;
        s.serialize_field("windowed_spent", &self.windowed_spent)?;
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
        s.serialize_field("attention", &self.attention)?;
        s.serialize_field("last_event_at_unix", &self.last_event_at_unix)?;
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

#[cfg(test)]
mod idle_tests {
    use super::*;
    use std::collections::HashMap;

    /// Minimal AgentSnapshot for exercising `idle_signal` in isolation.
    fn snap(status: AgentStatus, last_event_at_unix: u64) -> AgentSnapshot {
        AgentSnapshot {
            id: "a".into(), status, turn: 0, context_tokens: 0, token_budget: 0,
            windowed_spent: 0, task_preview: String::new(), tools: vec![],
            short_term_previews: vec![], parent_id: None, accessible_server_names: vec![],
            capabilities_unrestricted: false, tier: None, pid: None,
            credential_providers: vec![], credential_request_counts: HashMap::new(),
            credential_denied_counts: HashMap::new(), credential_last_access_at: HashMap::new(),
            attention: vec![], last_event_at_unix,
        }
    }

    /// THE LANDMINE GUARD (ux.2b M1): idle is computed READ-TIME from a carried anchor. Advancing
    /// `now` on the SAME snapshot flips Idle on — provable only because idle is NOT baked at build.
    /// If idle were computed inside `derive_attention` with an internal clock, this test could not
    /// be written (you cannot advance `now` without rebuilding), so its existence enforces the
    /// read-time architecture.
    #[test]
    fn idle_is_read_time_advances_on_same_snapshot() {
        let t: u64 = 1_000_000;
        let s = snap(AgentStatus::Running, t);
        // Just after the last event: not idle.
        assert!(s.idle_signal(t + 10, 180).is_none(), "10s < 180s threshold must not be idle");
        // Past the threshold, WITHOUT rebuilding the snapshot: idle appears.
        let sig = s.idle_signal(t + 200, 180).expect("200s > 180s threshold must be idle");
        assert_eq!(sig.reason, AttentionReason::Idle);
        assert_eq!(sig.since, t, "Idle carries the real onset (last_event_at), not now");
    }

    /// Idle allowlist is `Running` ONLY (ux.2b M4): every parked/terminal status is intentionally
    /// quiet and must never false-read Idle, even far past the threshold.
    #[test]
    fn idle_suppressed_for_every_non_running_status() {
        let t: u64 = 1_000_000;
        let far = t + 10_000; // way past any threshold
        for status in [
            AgentStatus::Waiting,
            AgentStatus::Deferred,
            AgentStatus::AwaitingChild("c".into()),
            AgentStatus::AwaitingApproval("act_1".into()),
            AgentStatus::Done,
            AgentStatus::Failed,
        ] {
            let label = status.as_str().to_string();
            assert!(
                snap(status, t).idle_signal(far, 180).is_none(),
                "status {label} must never read Idle",
            );
        }
        // Control: Running with the same age DOES read Idle.
        assert!(snap(AgentStatus::Running, t).idle_signal(far, 180).is_some());
    }

    /// Routing precedence is declaration order (`Ord`): ApprovalPending wins ties, Error outranks
    /// BudgetRisk, and Idle is last. agentctl picks the top signal by `min` over this order, so the
    /// order here is load-bearing (ux.2b placed Error above BudgetRisk, Idle last).
    #[test]
    fn attention_reason_routing_order() {
        use AttentionReason::*;
        assert!(ApprovalPending < Degraded);
        assert!(Degraded < Error);
        assert!(Error < BudgetRisk);
        assert!(BudgetRisk < EvaluationUnavailable);
        assert!(EvaluationUnavailable < Idle);
        // The min of a mixed set is the highest-priority reason.
        let mut set = [Idle, BudgetRisk, Error, ApprovalPending];
        set.sort();
        assert_eq!(set[0], ApprovalPending, "ApprovalPending always routes first");
    }
}
