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
    /// data: { agent, kind, dest, input_tokens, output_tokens, content_audited }
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
        assert_eq!(kind_str(EventKind::Error), "error");
    }
}
