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
        assert_eq!(kind_str(EventKind::Error), "error");
    }
}
