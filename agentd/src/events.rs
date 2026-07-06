use serde::Serialize;

/// Taxonomy of flight-recorder event kinds.
///
/// Every meaningful step an agent takes emits one of these variants.
/// Keep this in sync with `docs/CONVENTIONS.md`.
#[derive(Debug, Serialize)]
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
    MaxTurnsReached,
    CapabilityDenied,
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
    Error,
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
        assert_eq!(kind_str(EventKind::Error), "error");
    }
}
