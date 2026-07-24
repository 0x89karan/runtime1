use serde::Serialize;

/// Taxonomy of flight-recorder event kinds.
///
/// Every meaningful step an agent takes emits one of these variants.
/// Keep this in sync with `docs/CONVENTIONS.md`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    AgentSpawned,
    ToolsRegistered,
    Perceive,
    InferenceRequest,
    InferenceResponse,
    ToolCall,
    ToolResult,
    Observe,
    AgentCompleted,
    AgentFailed,
    AgentScheduled,
    AgentDeferred,
    AgentAdmissionDenied,
    BudgetExceeded,
    BudgetReset,
    /// Operator set a per-agent token budget at runtime (ux.11a SetBudget).
    BudgetSet,
    /// Operator cancelled an agent at runtime (ux.13). Emitted once per cancelled node,
    /// including cascaded children (`cause: "cascade from <parent>"`).
    AgentCancelled,
    /// Operator narrowed an agent's capabilities at runtime (ux.13 SetCaps).
    CapabilitiesSet,
    MaxTurnsReached,
    CapabilityDenied,
    /// A spawn was rejected because the child requested a capability not covered by
    /// the parent's set (cap.2 spawn attenuation, fail-closed — reject, not clamp).
    AgentSpawnDenied,
    AgentChildResultDelivered,
    AgentCardRegistered,
    AgentCheckpointed,
    AgentRestored,
    MessageSent,
    MessageReceived,
    SystemShutdownRequested,
    FuseMounted,
    FuseUnmounted,
    /// FUSE mount intentionally skipped via --no-fuse / AGENTOS_NO_FUSE.
    FuseSkipped,
    /// MCP server subprocess sandboxed via Landlock + seccomp before exec.
    SandboxApplied,
    /// MCP server configured without `capabilities`; running unsandboxed.
    SandboxSkipped,
    /// A `kv_get` call completed successfully (data shape documented in events.rs).
    /// data: { agent, namespace, key, found: bool }
    MemoryRead,
    /// A `kv_set` call committed successfully.
    /// data: { agent, namespace, key, bytes: usize }
    MemoryWrite,
    /// Memory store open or transaction failed; store unavailable.
    /// data: { stage: "open"|"version_check"|"schema_init", hint, error }
    MemoryUnavailable,
    /// Corrupt store renamed to `.corrupt`; new empty store opened.
    /// data: { path }
    MemoryQuarantined,
    /// runs.redb could not be opened; run history unavailable this boot (ux.11b).
    /// data: { hint, error }
    RunsUnavailable,
    /// CoS published a morning brief (ux.11c); authored deterministically from runs.redb.
    /// Informational (no span). data: { agent_id, brief_id, window_from, window_to,
    /// run_count, failed_count, spend_total }
    BriefWritten,
    /// Effective capability set for an agent or MCP server, computed once at boot from the
    /// shared `tier_legality` resolver (cap.1). "Computed once and logged" — descriptive,
    /// enforcement unchanged. data: { kind: "agent"|"mcp_server", name, enforced: [str],
    /// inert: [{cap, reason}] }
    CapabilitiesResolved,
    /// Token spend reached SOFT_THRESHOLD (75%); advisory only, no eviction.
    /// data: { agent, turn, tokens_spent_pct: f64, soft_threshold: f64 }
    MemoryPressureAdvisory,
    /// Oldest turn pairs evicted from active context into short_term (Tier 2).
    /// data: { agent, turn, pages_moved: usize, short_term_depth: usize, tokens_spent_pct: f64 }
    MemoryPaged,
    /// Agent called `mem_remember`; content committed to Tier-3 long-term storage.
    /// data: { agent, turn, items: 1 }
    MemoryDistilled,
    /// Agent called `kb_search`; inverted index queried.
    /// data: { agent_id, segment?, query_preview, hits: usize, terms_matched: usize }
    KbSearch,
    /// Entry evicted from a KB segment (capacity or age floor).
    /// data: { segment, key, reason: "capacity"|"age" }
    MemoryEvicted,
    /// HTTP MCP server connected successfully after initialize + tools/list.
    /// data: { server_name, url, session_id_present: bool }
    McpHttpConnected,
    /// HTTP MCP server returned a non-2xx status or JSON-RPC error during a tool call.
    /// data: { server_name, http_status: u16, method }
    McpHttpError,
    /// MCP server spawned with passenv; lists which names were forwarded, blocked, or absent.
    /// data: { server, forwarded: [str], blocked: [str], absent: [str] }
    McpPassenvForwarded,
    /// SSE streaming inference started for an agent turn.
    /// data: { agent_id, model }
    InferenceStreamStarted,
    /// SSE streaming inference completed successfully.
    /// data: { agent_id, text_chunks_emitted: u64, input_tokens: u32, output_tokens: u32 }
    InferenceStreamCompleted,
    /// One text chunk of a streaming inference response. Recorded per-chunk on the hot
    /// streaming path (agentd/src/scheduler.rs's print_fut loop) so remote SSE subscribers
    /// (e.g. agentctl watch's chat rail, ux.1) can render live token-by-token output —
    /// before this event existed, chunks were only ever written to agentd's own local
    /// stdout and never reached `/api/v1/events`.
    /// data: { agent_id, turn_seq: u64, chunk_seq: u64, text }
    InferenceStreamDelta,
    /// Operator wrote a valid spawn command to /agents/control; agent queued.
    /// data: { task_preview, id }
    FuseControlReceived,
    /// Operator command via /agents/control could not be dispatched (bad ID, collision, etc.).
    /// data: { error, is_error: true }
    FuseControlError,
    /// Agent called request_approval; action parked pending operator decision.
    /// data: { agent_id, approval_id, kind, risk, summary }
    ApprovalRequested,
    /// Operator approved a pending action; agent resumed.
    /// data: { agent_id, approval_id, edits_applied: bool, auto_approve_kind? }
    ApprovalGranted,
    /// Operator rejected a pending action; agent resumed with is_error result.
    /// data: { agent_id, approval_id, reason? }
    ApprovalRejected,
    /// Egress call permitted and action receipt written.
    /// data: { agent, kind, dest, input_tokens, output_tokens }
    EgressBrokered,
    /// Egress call denied by policy; receipt written.
    /// data: { agent, attempted_dest }
    EgressDenied,
    /// Action receipt appended to evidence.jsonl.
    /// data: { agent, verdict, chain_seq }
    ActionReceiptEmitted,
    /// Egress proxy failed to initialise or write a receipt.
    /// data: { error }
    EgressProxyFailed,
    /// Universal-tier agent child process started.
    /// data: { isolation: "none"|"gvisor", pid, command }
    UniversalAgentStarted,
    /// Universal-tier agent child process exited.
    /// data: { pid, exit_code: i32|null, wall_seconds: u64 }
    UniversalAgentExited,
    /// gVisor isolation requested but runsc not found; falling back to no isolation.
    /// data: { agent_id }
    UniversalAgentIsolationDegraded,
    /// Scheduler started; provides a stable run_id (UUID v4) used as the OTLP trace root.
    /// data: { run_id, config_hash }
    SchedulerStarted,
    /// Scheduler stopped after graceful shutdown.
    /// data: { run_id, agent_count }
    SchedulerStopped,
    /// A stale pooled connection caused a connect error; the request was retried once and succeeded.
    /// data: { agent_id, model, retries: u32 }
    InferenceTransportRetried,
    /// Management HTTP API bound and ready.
    /// data: { addr }
    ManagementStarted,
    /// Management HTTP API received a request.
    /// data: { method, path, status: u16 }
    ManagementRequest,
    /// Operator approved a pending action via the HTTP management API.
    /// data: { id, agent_id }
    ApprovalHttpApproved,
    /// Operator denied a pending action via the HTTP management API.
    /// data: { id, agent_id, reason? }
    ApprovalHttpDenied,
    /// Credential broker forwarded a request to an upstream provider.
    /// data: { agent_id, provider, path, response_status: u16, response_bytes: usize }
    CredentialEgressBrokered,
    /// Credential token access audited (capability check passed).
    /// data: { agent_id, provider, path, method }
    CredentialAccessed,
    /// Credential refresh (OAuth token rotation) failed.
    /// data: { provider, error, token_written: bool }
    CredentialRefreshFailed,
    /// Credential not provisioned — MCP server gets 503.
    /// data: { provider, hint }
    CredentialNotProvisioned,
    /// Credential access denied — agent lacks Credential capability.
    /// data: { agent_id, provider }
    CredentialDenied,
    /// Per-agent per-provider request-count cap exceeded; request rejected with 429.
    /// data: { agent_id, provider, count, limit }
    CredentialCapExceeded,
    /// Orchestrator spawned an agent in waiting mode.
    /// data: { task_preview, agent_id }
    OrchestratorDispatched,
    /// Orchestrator injected a new user turn into a waiting agent.
    /// data: { agent_id, text_len: usize }
    OrchestratorInjected,
    /// Orchestrated agent completed a turn and parked, awaiting next inject.
    /// data: { agent_id, answer }
    OrchestratorTurnComplete,
    /// Orchestrated agent exited because the target agent was not found.
    /// data: { agent_id, reason: "agent_not_found" }
    OrchestratorExited,
    /// Device-level isolation capabilities probed at startup.
    /// data: { tier, arch, runsc: path|null, landlock: bool, seccomp: bool }
    IsolationProbed,
    /// Provider entered AttentionRequired state (first time after a clean run).
    /// data: { provider, recovery_kind: "reauth"|"config_fix"|"secret_replace", reason }
    CredentialAttentionRequired,
    /// Provider recovered from AttentionRequired (operator reset or successful refresh).
    /// data: { provider, source: "reset_attention"|"proactive_refresh"|"foreground_request" }
    CredentialRecovered,
    Error,
}

impl EventKind {
    /// Canonical wire string for this event kind — byte-identical to the serde
    /// `snake_case` serialization written to `flight.jsonl`'s `"kind"` field.
    ///
    /// This is the **single source of truth** for code that matches events by raw
    /// string rather than by variant — notably `agentctl`'s flight-log filters
    /// (`watch/inspector.rs`, `watch/views.rs`, `watch/converse.rs`,
    /// `orchestrate.rs`, `watch/topology.rs`), which depend on the `agentd` crate
    /// but historically hard-coded the strings. A renamed variant that used to
    /// silently blank a TUI filter now breaks `as_str_matches_serde` (canonical
    /// drift) and `agentctl`'s `event_kind_strings` guard (raw-string drift).
    /// (audit86-P2-13 / AUDIT-v0.97 P2-10 / par.1)
    ///
    /// Exhaustive by construction: adding a variant without a string here is a
    /// compile error, forcing the author to pick its canonical form.
    pub const fn as_str(&self) -> &'static str {
        match self {
            EventKind::AgentSpawned => "agent_spawned",
            EventKind::ToolsRegistered => "tools_registered",
            EventKind::Perceive => "perceive",
            EventKind::InferenceRequest => "inference_request",
            EventKind::InferenceResponse => "inference_response",
            EventKind::ToolCall => "tool_call",
            EventKind::ToolResult => "tool_result",
            EventKind::Observe => "observe",
            EventKind::AgentCompleted => "agent_completed",
            EventKind::AgentFailed => "agent_failed",
            EventKind::AgentScheduled => "agent_scheduled",
            EventKind::AgentDeferred => "agent_deferred",
            EventKind::AgentAdmissionDenied => "agent_admission_denied",
            EventKind::BudgetExceeded => "budget_exceeded",
            EventKind::BudgetReset => "budget_reset",
            EventKind::BudgetSet => "budget_set",
            EventKind::AgentCancelled => "agent_cancelled",
            EventKind::CapabilitiesSet => "capabilities_set",
            EventKind::MaxTurnsReached => "max_turns_reached",
            EventKind::CapabilityDenied => "capability_denied",
            EventKind::AgentSpawnDenied => "agent_spawn_denied",
            EventKind::AgentChildResultDelivered => "agent_child_result_delivered",
            EventKind::AgentCardRegistered => "agent_card_registered",
            EventKind::AgentCheckpointed => "agent_checkpointed",
            EventKind::AgentRestored => "agent_restored",
            EventKind::MessageSent => "message_sent",
            EventKind::MessageReceived => "message_received",
            EventKind::SystemShutdownRequested => "system_shutdown_requested",
            EventKind::FuseMounted => "fuse_mounted",
            EventKind::FuseUnmounted => "fuse_unmounted",
            EventKind::FuseSkipped => "fuse_skipped",
            EventKind::SandboxApplied => "sandbox_applied",
            EventKind::SandboxSkipped => "sandbox_skipped",
            EventKind::MemoryRead => "memory_read",
            EventKind::MemoryWrite => "memory_write",
            EventKind::MemoryUnavailable => "memory_unavailable",
            EventKind::MemoryQuarantined => "memory_quarantined",
            EventKind::RunsUnavailable => "runs_unavailable",
            EventKind::BriefWritten => "brief_written",
            EventKind::CapabilitiesResolved => "capabilities_resolved",
            EventKind::MemoryPressureAdvisory => "memory_pressure_advisory",
            EventKind::MemoryPaged => "memory_paged",
            EventKind::MemoryDistilled => "memory_distilled",
            EventKind::KbSearch => "kb_search",
            EventKind::MemoryEvicted => "memory_evicted",
            EventKind::McpHttpConnected => "mcp_http_connected",
            EventKind::McpHttpError => "mcp_http_error",
            EventKind::McpPassenvForwarded => "mcp_passenv_forwarded",
            EventKind::InferenceStreamStarted => "inference_stream_started",
            EventKind::InferenceStreamCompleted => "inference_stream_completed",
            EventKind::InferenceStreamDelta => "inference_stream_delta",
            EventKind::FuseControlReceived => "fuse_control_received",
            EventKind::FuseControlError => "fuse_control_error",
            EventKind::ApprovalRequested => "approval_requested",
            EventKind::ApprovalGranted => "approval_granted",
            EventKind::ApprovalRejected => "approval_rejected",
            EventKind::EgressBrokered => "egress_brokered",
            EventKind::EgressDenied => "egress_denied",
            EventKind::ActionReceiptEmitted => "action_receipt_emitted",
            EventKind::EgressProxyFailed => "egress_proxy_failed",
            EventKind::UniversalAgentStarted => "universal_agent_started",
            EventKind::UniversalAgentExited => "universal_agent_exited",
            EventKind::UniversalAgentIsolationDegraded => "universal_agent_isolation_degraded",
            EventKind::SchedulerStarted => "scheduler_started",
            EventKind::SchedulerStopped => "scheduler_stopped",
            EventKind::InferenceTransportRetried => "inference_transport_retried",
            EventKind::ManagementStarted => "management_started",
            EventKind::ManagementRequest => "management_request",
            EventKind::ApprovalHttpApproved => "approval_http_approved",
            EventKind::ApprovalHttpDenied => "approval_http_denied",
            EventKind::CredentialEgressBrokered => "credential_egress_brokered",
            EventKind::CredentialAccessed => "credential_accessed",
            EventKind::CredentialRefreshFailed => "credential_refresh_failed",
            EventKind::CredentialNotProvisioned => "credential_not_provisioned",
            EventKind::CredentialDenied => "credential_denied",
            EventKind::CredentialCapExceeded => "credential_cap_exceeded",
            EventKind::OrchestratorDispatched => "orchestrator_dispatched",
            EventKind::OrchestratorInjected => "orchestrator_injected",
            EventKind::OrchestratorTurnComplete => "orchestrator_turn_complete",
            EventKind::OrchestratorExited => "orchestrator_exited",
            EventKind::IsolationProbed => "isolation_probed",
            EventKind::CredentialAttentionRequired => "credential_attention_required",
            EventKind::CredentialRecovered => "credential_recovered",
            EventKind::Error => "error",
        }
    }

    /// Every variant, for enumeration in tests and string-set guards. Kept complete
    /// by `all_is_exhaustive` (a variant added without an entry here fails to compile).
    pub const ALL: &'static [EventKind] = &[
        EventKind::AgentSpawned,
        EventKind::ToolsRegistered,
        EventKind::Perceive,
        EventKind::InferenceRequest,
        EventKind::InferenceResponse,
        EventKind::ToolCall,
        EventKind::ToolResult,
        EventKind::Observe,
        EventKind::AgentCompleted,
        EventKind::AgentFailed,
        EventKind::AgentScheduled,
        EventKind::AgentDeferred,
        EventKind::AgentAdmissionDenied,
        EventKind::BudgetExceeded,
        EventKind::BudgetReset,
        EventKind::BudgetSet,
        EventKind::AgentCancelled,
        EventKind::CapabilitiesSet,
        EventKind::MaxTurnsReached,
        EventKind::CapabilityDenied,
        EventKind::AgentSpawnDenied,
        EventKind::AgentChildResultDelivered,
        EventKind::AgentCardRegistered,
        EventKind::AgentCheckpointed,
        EventKind::AgentRestored,
        EventKind::MessageSent,
        EventKind::MessageReceived,
        EventKind::SystemShutdownRequested,
        EventKind::FuseMounted,
        EventKind::FuseUnmounted,
        EventKind::FuseSkipped,
        EventKind::SandboxApplied,
        EventKind::SandboxSkipped,
        EventKind::MemoryRead,
        EventKind::MemoryWrite,
        EventKind::MemoryUnavailable,
        EventKind::MemoryQuarantined,
        EventKind::RunsUnavailable,
        EventKind::BriefWritten,
        EventKind::CapabilitiesResolved,
        EventKind::MemoryPressureAdvisory,
        EventKind::MemoryPaged,
        EventKind::MemoryDistilled,
        EventKind::KbSearch,
        EventKind::MemoryEvicted,
        EventKind::McpHttpConnected,
        EventKind::McpHttpError,
        EventKind::McpPassenvForwarded,
        EventKind::InferenceStreamStarted,
        EventKind::InferenceStreamCompleted,
        EventKind::InferenceStreamDelta,
        EventKind::FuseControlReceived,
        EventKind::FuseControlError,
        EventKind::ApprovalRequested,
        EventKind::ApprovalGranted,
        EventKind::ApprovalRejected,
        EventKind::EgressBrokered,
        EventKind::EgressDenied,
        EventKind::ActionReceiptEmitted,
        EventKind::EgressProxyFailed,
        EventKind::UniversalAgentStarted,
        EventKind::UniversalAgentExited,
        EventKind::UniversalAgentIsolationDegraded,
        EventKind::SchedulerStarted,
        EventKind::SchedulerStopped,
        EventKind::InferenceTransportRetried,
        EventKind::ManagementStarted,
        EventKind::ManagementRequest,
        EventKind::ApprovalHttpApproved,
        EventKind::ApprovalHttpDenied,
        EventKind::CredentialEgressBrokered,
        EventKind::CredentialAccessed,
        EventKind::CredentialRefreshFailed,
        EventKind::CredentialNotProvisioned,
        EventKind::CredentialDenied,
        EventKind::CredentialCapExceeded,
        EventKind::OrchestratorDispatched,
        EventKind::OrchestratorInjected,
        EventKind::OrchestratorTurnComplete,
        EventKind::OrchestratorExited,
        EventKind::IsolationProbed,
        EventKind::CredentialAttentionRequired,
        EventKind::CredentialRecovered,
        EventKind::Error,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn kind_str(k: EventKind) -> String {
        let v = serde_json::to_value(k).unwrap();
        match v {
            Value::String(s) => s,
            _ => panic!("EventKind serialized to non-string"),
        }
    }

    #[test]
    fn event_kind_serialized_strings() {
        assert_eq!(kind_str(EventKind::AgentSpawned), "agent_spawned");
        assert_eq!(kind_str(EventKind::ToolsRegistered), "tools_registered");
        assert_eq!(kind_str(EventKind::Perceive), "perceive");
        assert_eq!(kind_str(EventKind::InferenceRequest), "inference_request");
        assert_eq!(kind_str(EventKind::InferenceResponse), "inference_response");
        assert_eq!(kind_str(EventKind::ToolCall), "tool_call");
        assert_eq!(kind_str(EventKind::ToolResult), "tool_result");
        assert_eq!(kind_str(EventKind::Observe), "observe");
        assert_eq!(kind_str(EventKind::AgentCompleted), "agent_completed");
        assert_eq!(kind_str(EventKind::AgentFailed), "agent_failed");
        assert_eq!(kind_str(EventKind::AgentScheduled), "agent_scheduled");
        assert_eq!(kind_str(EventKind::AgentDeferred), "agent_deferred");
        assert_eq!(kind_str(EventKind::AgentAdmissionDenied), "agent_admission_denied");
        assert_eq!(kind_str(EventKind::BudgetExceeded), "budget_exceeded");
        assert_eq!(kind_str(EventKind::MaxTurnsReached), "max_turns_reached");
        assert_eq!(kind_str(EventKind::CapabilityDenied), "capability_denied");
        assert_eq!(kind_str(EventKind::AgentSpawnDenied), "agent_spawn_denied");
        assert_eq!(kind_str(EventKind::AgentChildResultDelivered), "agent_child_result_delivered");
        assert_eq!(kind_str(EventKind::AgentCardRegistered), "agent_card_registered");
        assert_eq!(kind_str(EventKind::AgentCheckpointed), "agent_checkpointed");
        assert_eq!(kind_str(EventKind::AgentRestored), "agent_restored");
        assert_eq!(kind_str(EventKind::MessageSent), "message_sent");
        assert_eq!(kind_str(EventKind::MessageReceived), "message_received");
        assert_eq!(kind_str(EventKind::SystemShutdownRequested), "system_shutdown_requested");
        assert_eq!(kind_str(EventKind::FuseMounted), "fuse_mounted");
        assert_eq!(kind_str(EventKind::FuseUnmounted), "fuse_unmounted");
        assert_eq!(kind_str(EventKind::FuseSkipped), "fuse_skipped");
        assert_eq!(kind_str(EventKind::SandboxApplied), "sandbox_applied");
        assert_eq!(kind_str(EventKind::SandboxSkipped), "sandbox_skipped");
        assert_eq!(kind_str(EventKind::MemoryRead), "memory_read");
        assert_eq!(kind_str(EventKind::MemoryWrite), "memory_write");
        assert_eq!(kind_str(EventKind::MemoryUnavailable), "memory_unavailable");
        assert_eq!(kind_str(EventKind::MemoryQuarantined), "memory_quarantined");
        assert_eq!(kind_str(EventKind::MemoryPressureAdvisory), "memory_pressure_advisory");
        assert_eq!(kind_str(EventKind::MemoryPaged), "memory_paged");
        assert_eq!(kind_str(EventKind::MemoryDistilled), "memory_distilled");
        assert_eq!(kind_str(EventKind::KbSearch), "kb_search");
        assert_eq!(kind_str(EventKind::MemoryEvicted), "memory_evicted");
        assert_eq!(kind_str(EventKind::McpHttpConnected), "mcp_http_connected");
        assert_eq!(kind_str(EventKind::McpHttpError), "mcp_http_error");
        assert_eq!(kind_str(EventKind::McpPassenvForwarded), "mcp_passenv_forwarded");
        assert_eq!(kind_str(EventKind::InferenceStreamStarted), "inference_stream_started");
        assert_eq!(kind_str(EventKind::InferenceStreamCompleted), "inference_stream_completed");
        assert_eq!(kind_str(EventKind::FuseControlReceived), "fuse_control_received");
        assert_eq!(kind_str(EventKind::FuseControlError), "fuse_control_error");
        assert_eq!(kind_str(EventKind::ApprovalRequested), "approval_requested");
        assert_eq!(kind_str(EventKind::ApprovalGranted), "approval_granted");
        assert_eq!(kind_str(EventKind::ApprovalRejected), "approval_rejected");
        assert_eq!(kind_str(EventKind::EgressBrokered), "egress_brokered");
        assert_eq!(kind_str(EventKind::EgressDenied), "egress_denied");
        assert_eq!(kind_str(EventKind::ActionReceiptEmitted), "action_receipt_emitted");
        assert_eq!(kind_str(EventKind::EgressProxyFailed), "egress_proxy_failed");
        assert_eq!(kind_str(EventKind::UniversalAgentStarted), "universal_agent_started");
        assert_eq!(kind_str(EventKind::UniversalAgentExited), "universal_agent_exited");
        assert_eq!(kind_str(EventKind::UniversalAgentIsolationDegraded), "universal_agent_isolation_degraded");
        assert_eq!(kind_str(EventKind::SchedulerStarted), "scheduler_started");
        assert_eq!(kind_str(EventKind::SchedulerStopped), "scheduler_stopped");
        assert_eq!(kind_str(EventKind::InferenceTransportRetried), "inference_transport_retried");
        assert_eq!(kind_str(EventKind::ManagementStarted), "management_started");
        assert_eq!(kind_str(EventKind::ManagementRequest), "management_request");
        assert_eq!(kind_str(EventKind::ApprovalHttpApproved), "approval_http_approved");
        assert_eq!(kind_str(EventKind::ApprovalHttpDenied), "approval_http_denied");
        assert_eq!(kind_str(EventKind::CredentialEgressBrokered), "credential_egress_brokered");
        assert_eq!(kind_str(EventKind::CredentialAccessed), "credential_accessed");
        assert_eq!(kind_str(EventKind::CredentialRefreshFailed), "credential_refresh_failed");
        assert_eq!(kind_str(EventKind::CredentialNotProvisioned), "credential_not_provisioned");
        assert_eq!(kind_str(EventKind::CredentialDenied), "credential_denied");
        assert_eq!(kind_str(EventKind::CredentialCapExceeded), "credential_cap_exceeded");
        assert_eq!(kind_str(EventKind::OrchestratorDispatched), "orchestrator_dispatched");
        assert_eq!(kind_str(EventKind::OrchestratorInjected), "orchestrator_injected");
        assert_eq!(kind_str(EventKind::OrchestratorTurnComplete), "orchestrator_turn_complete");
        assert_eq!(kind_str(EventKind::OrchestratorExited), "orchestrator_exited");
        assert_eq!(kind_str(EventKind::IsolationProbed), "isolation_probed");
        assert_eq!(kind_str(EventKind::CredentialAttentionRequired), "credential_attention_required");
        assert_eq!(kind_str(EventKind::CredentialRecovered), "credential_recovered");
        assert_eq!(kind_str(EventKind::RunsUnavailable), "runs_unavailable");
        assert_eq!(kind_str(EventKind::BriefWritten), "brief_written");
        assert_eq!(kind_str(EventKind::CapabilitiesResolved), "capabilities_resolved");
        assert_eq!(kind_str(EventKind::Error), "error");
    }

    /// `as_str()` is the canonical-string single source of truth, so it must be
    /// byte-identical to the serde serialization for EVERY variant — otherwise the
    /// two forms drift and a consumer that trusts `as_str()` (agentctl) filters on
    /// a string that never appears in `flight.jsonl`. (par.1 / AUDIT-v0.97 P2-10)
    #[test]
    fn as_str_matches_serde() {
        for k in EventKind::ALL {
            assert_eq!(
                k.as_str(),
                kind_str(*k),
                "EventKind::{k:?}.as_str() drifted from its serde serialization"
            );
        }
        // No two variants may share a wire string, or a string match is ambiguous.
        let mut seen = std::collections::HashSet::new();
        for k in EventKind::ALL {
            assert!(seen.insert(k.as_str()), "duplicate wire string: {}", k.as_str());
        }
    }

    /// `EventKind::ALL` must list every variant. The exhaustive `match` makes adding
    /// a variant without extending `ALL` a compile error (the developer is forced to
    /// touch this arm, at which point they add it to the array the assert below counts).
    #[test]
    fn all_is_exhaustive() {
        fn touch(k: EventKind) {
            #[allow(clippy::match_like_matches_macro)]
            match k {
                EventKind::AgentSpawned
                | EventKind::ToolsRegistered
                | EventKind::Perceive
                | EventKind::InferenceRequest
                | EventKind::InferenceResponse
                | EventKind::ToolCall
                | EventKind::ToolResult
                | EventKind::Observe
                | EventKind::AgentCompleted
                | EventKind::AgentFailed
                | EventKind::AgentScheduled
                | EventKind::AgentDeferred
                | EventKind::AgentAdmissionDenied
                | EventKind::BudgetExceeded
                | EventKind::BudgetReset
                | EventKind::BudgetSet
                | EventKind::AgentCancelled
                | EventKind::CapabilitiesSet
                | EventKind::MaxTurnsReached
                | EventKind::CapabilityDenied
                | EventKind::AgentSpawnDenied
                | EventKind::AgentChildResultDelivered
                | EventKind::AgentCardRegistered
                | EventKind::AgentCheckpointed
                | EventKind::AgentRestored
                | EventKind::MessageSent
                | EventKind::MessageReceived
                | EventKind::SystemShutdownRequested
                | EventKind::FuseMounted
                | EventKind::FuseUnmounted
                | EventKind::FuseSkipped
                | EventKind::SandboxApplied
                | EventKind::SandboxSkipped
                | EventKind::MemoryRead
                | EventKind::MemoryWrite
                | EventKind::MemoryUnavailable
                | EventKind::MemoryQuarantined
                | EventKind::RunsUnavailable
                | EventKind::BriefWritten
                | EventKind::CapabilitiesResolved
                | EventKind::MemoryPressureAdvisory
                | EventKind::MemoryPaged
                | EventKind::MemoryDistilled
                | EventKind::KbSearch
                | EventKind::MemoryEvicted
                | EventKind::McpHttpConnected
                | EventKind::McpHttpError
                | EventKind::McpPassenvForwarded
                | EventKind::InferenceStreamStarted
                | EventKind::InferenceStreamCompleted
                | EventKind::InferenceStreamDelta
                | EventKind::FuseControlReceived
                | EventKind::FuseControlError
                | EventKind::ApprovalRequested
                | EventKind::ApprovalGranted
                | EventKind::ApprovalRejected
                | EventKind::EgressBrokered
                | EventKind::EgressDenied
                | EventKind::ActionReceiptEmitted
                | EventKind::EgressProxyFailed
                | EventKind::UniversalAgentStarted
                | EventKind::UniversalAgentExited
                | EventKind::UniversalAgentIsolationDegraded
                | EventKind::SchedulerStarted
                | EventKind::SchedulerStopped
                | EventKind::InferenceTransportRetried
                | EventKind::ManagementStarted
                | EventKind::ManagementRequest
                | EventKind::ApprovalHttpApproved
                | EventKind::ApprovalHttpDenied
                | EventKind::CredentialEgressBrokered
                | EventKind::CredentialAccessed
                | EventKind::CredentialRefreshFailed
                | EventKind::CredentialNotProvisioned
                | EventKind::CredentialDenied
                | EventKind::CredentialCapExceeded
                | EventKind::OrchestratorDispatched
                | EventKind::OrchestratorInjected
                | EventKind::OrchestratorTurnComplete
                | EventKind::OrchestratorExited
                | EventKind::IsolationProbed
                | EventKind::CredentialAttentionRequired
                | EventKind::CredentialRecovered
                | EventKind::Error => {
                    // Every variant must appear in ALL. If you just added a variant to
                    // the enum, the compiler pointed you here — add it to `EventKind::ALL`.
                    assert!(
                        EventKind::ALL.iter().any(|x| x.as_str() == k.as_str()),
                        "EventKind::{k:?} is missing from EventKind::ALL"
                    );
                }
            }
        }
        for k in EventKind::ALL {
            touch(*k);
        }
    }
}
