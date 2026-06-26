// Compile-time exhaustiveness guard: every EventKind variant must be mentioned here.
// Adding a new EventKind to agentd without updating span_builder.rs will cause this test
// to fail to compile, prompting the implementer to decide how it maps to spans.

use agentd::flight_recorder::EventKind;

#[allow(dead_code)]
fn assert_all_event_kinds_handled(kind: EventKind) -> bool {
    match kind {
        // Lifecycle — agent spans open/close
        EventKind::AgentSpawned => true,
        EventKind::AgentCompleted => true,
        EventKind::AgentFailed => true,

        // Inference — map to gen_ai.chat spans
        EventKind::InferenceRequest => true,
        EventKind::InferenceResponse => true,
        EventKind::InferenceStreamStarted => true,   // alias for InferenceRequest
        EventKind::InferenceStreamCompleted => true, // alias for InferenceResponse

        // Tools — map to tool.* spans
        EventKind::ToolCall => true,
        EventKind::ToolResult => true,

        // Egress — span event only (no egress_completed to measure latency)
        EventKind::EgressBrokered => true,

        // Scheduler lifecycle — trace root
        EventKind::SchedulerStarted => true,
        EventKind::SchedulerStopped => true,

        // Not mapped to spans (informational / infra events)
        EventKind::ToolsRegistered => false,
        EventKind::Perceive => false,
        EventKind::Observe => false,
        EventKind::AgentScheduled => false,
        EventKind::AgentDeferred => false,
        EventKind::AgentAdmissionDenied => false,
        EventKind::BudgetExceeded => false,
        EventKind::MaxTurnsReached => false,
        EventKind::CapabilityDenied => false,
        EventKind::AgentChildResultDelivered => false,
        EventKind::AgentCardRegistered => false,
        EventKind::AgentCheckpointed => false,
        EventKind::AgentRestored => false,
        EventKind::MessageSent => false,
        EventKind::MessageReceived => false,
        EventKind::SystemShutdownRequested => false,
        EventKind::FuseMounted => false,
        EventKind::FuseUnmounted => false,
        EventKind::FuseSkipped => false,
        EventKind::SandboxApplied => false,
        EventKind::SandboxSkipped => false,
        EventKind::MemoryRead => false,
        EventKind::MemoryWrite => false,
        EventKind::MemoryUnavailable => false,
        EventKind::MemoryQuarantined => false,
        EventKind::MemoryPressureAdvisory => false,
        EventKind::MemoryPaged => false,
        EventKind::MemoryDistilled => false,
        EventKind::KbSearch => false,
        EventKind::MemoryEvicted => false,
        EventKind::McpHttpConnected => false,
        EventKind::McpHttpError => false,
        EventKind::McpPassenvForwarded => false,
        EventKind::FuseControlReceived => false,
        EventKind::FuseControlError => false,
        EventKind::ApprovalRequested => false,
        EventKind::ApprovalGranted => false,
        EventKind::ApprovalRejected => false,
        EventKind::EgressDenied => false,
        EventKind::ActionReceiptEmitted => false,
        EventKind::EgressProxyFailed => false,
        EventKind::UniversalAgentStarted => false,
        EventKind::UniversalAgentExited => false,
        EventKind::UniversalAgentIsolationDegraded => false,
        EventKind::Error => false,
    }
}

#[test]
fn event_kind_coverage_compiles() {
    // If this compiles, every EventKind variant is accounted for above.
    // Use AgentSpawned as a representative sample to exercise the function.
    assert!(assert_all_event_kinds_handled(EventKind::AgentSpawned));
    assert!(!assert_all_event_kinds_handled(EventKind::Error));
}
