pub mod anthropic;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Provider-agnostic inference trait. New backends implement this in their own
/// submodule and are wired in via `config.model.provider`.
#[async_trait]
pub trait InferenceGateway: Send + Sync {
    async fn infer(&self, request: InferenceRequest) -> Result<InferenceResponse>;
    fn model_id(&self) -> &str;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A single content block within a message. The `type` tag is stable across providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

/// A single turn in the conversation history.
#[derive(Debug, Clone)]
pub struct Msg {
    pub role: Role,
    pub blocks: Vec<Block>,
}

/// JSON-Schema description of a tool advertised to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug)]
pub struct InferenceRequest {
    pub system: Option<String>,
    pub messages: Vec<Msg>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: u32,
}

#[derive(Debug, Clone)]
pub struct InferenceResponse {
    pub blocks: Vec<Block>,
    pub stop_reason: StopReason,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}

impl StopReason {
    pub fn from_api_str(s: &str) -> Self {
        match s {
            "end_turn" => Self::EndTurn,
            "tool_use" => Self::ToolUse,
            "max_tokens" => Self::MaxTokens,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::EndTurn => "end_turn",
            Self::ToolUse => "tool_use",
            Self::MaxTokens => "max_tokens",
            Self::Other(s) => s.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_reason_roundtrip() {
        let cases = [
            ("end_turn", StopReason::EndTurn),
            ("tool_use", StopReason::ToolUse),
            ("max_tokens", StopReason::MaxTokens),
            ("stop_sequence", StopReason::Other("stop_sequence".to_string())),
        ];
        for (s, r) in cases {
            assert_eq!(StopReason::from_api_str(s), r);
            assert_eq!(r.as_str(), s);
        }
    }

    #[test]
    fn block_serde_text_roundtrip() {
        let block = Block::Text {
            text: "hello world".to_string(),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hello world");
    }

    #[test]
    fn block_serde_tool_use_roundtrip() {
        let block = Block::ToolUse {
            id: "toolu_abc".to_string(),
            name: "list_dir".to_string(),
            input: serde_json::json!({"path": "."}),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_use");
        assert_eq!(json["id"], "toolu_abc");
        assert_eq!(json["name"], "list_dir");
        // Verify it also deserializes back correctly.
        let back: Block = serde_json::from_value(json).unwrap();
        assert!(matches!(back, Block::ToolUse { id, .. } if id == "toolu_abc"));
    }

    #[test]
    fn block_serde_tool_result_roundtrip() {
        let block = Block::ToolResult {
            tool_use_id: "toolu_123".to_string(),
            content: "file contents".to_string(),
            is_error: false,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["tool_use_id"], "toolu_123");
        assert_eq!(json["is_error"], false);
    }

    #[test]
    fn role_serde_roundtrip() {
        let user = serde_json::to_value(&Role::User).unwrap();
        assert_eq!(user, "user");
        let assistant = serde_json::to_value(&Role::Assistant).unwrap();
        assert_eq!(assistant, "assistant");
        let back: Role = serde_json::from_value(user).unwrap();
        assert_eq!(back, Role::User);
    }
}
