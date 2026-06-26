// GenAI semantic conventions — OTel semconv v1.29.0 (December 2024)
#![allow(dead_code)]
// https://opentelemetry.io/docs/specs/semconv/gen-ai/

pub const GEN_AI_SYSTEM: &str = "gen_ai.system";
pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";
pub const GEN_AI_REQUEST_MAX_TOKENS: &str = "gen_ai.request.max_tokens";
pub const GEN_AI_RESPONSE_MODEL: &str = "gen_ai.response.model";
pub const GEN_AI_RESPONSE_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";
pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
pub const GEN_AI_OPERATION_NAME: &str = "gen_ai.operation.name";

// AgentOS-specific attributes (agentos.* namespace)
pub const AGENTOS_AGENT_ID: &str = "agentos.agent_id";
pub const AGENTOS_AGENT_TURN: &str = "agentos.agent_turn";
pub const AGENTOS_TOOL_NAME: &str = "agentos.tool_name";
pub const AGENTOS_SPAN_SYNTHESIZED: &str = "agentos.synthesized";
pub const AGENTOS_CLOSE_REASON: &str = "agentos.close_reason";
pub const AGENTOS_RUN_ID: &str = "agentos.run_id";
pub const AGENTOS_SESSION_ID: &str = "agentos.session_id";

// Metric names
pub const METRIC_TOKEN_USAGE: &str = "gen_ai.client.token.usage";
pub const METRIC_OPERATION_DURATION: &str = "gen_ai.client.operation.duration";
pub const METRIC_SPANS_DROPPED: &str = "agentos.otel.spans_dropped";

// gen_ai.operation.name values
pub const OP_CHAT: &str = "chat";
pub const OP_TOOL_CALL: &str = "execute_tool";

// gen_ai.system value
pub const SYSTEM_ANTHROPIC: &str = "anthropic";
