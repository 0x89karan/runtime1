use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::{Block, InferenceGateway, InferenceRequest, InferenceResponse, Msg, Role, StopReason};

const API_VERSION: &str = "2023-06-01";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

pub struct AnthropicGateway {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
}

impl AnthropicGateway {
    pub fn from_env(model: impl Into<String>) -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY not set")?;
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self {
            client: Client::new(),
            api_key,
            base_url,
            model: model.into(),
        })
    }
}

// ── Wire types (Anthropic-specific, not exported) ─────────────────────────────

#[derive(Serialize)]
struct AnthropicReq<'a> {
    model: &'a str,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<&'a str>,
    messages: Vec<AnthropicMsg>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool<'a>>,
}

#[derive(Serialize)]
struct AnthropicMsg {
    role: &'static str,
    content: Vec<serde_json::Value>,
}

#[derive(Serialize)]
struct AnthropicTool<'a> {
    name: &'a str,
    description: &'a str,
    input_schema: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct AnthropicResp {
    content: Vec<serde_json::Value>,
    stop_reason: String,
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

// ── Mapping helpers ───────────────────────────────────────────────────────────

fn block_to_json(block: &Block) -> serde_json::Value {
    match block {
        Block::Text { text } => serde_json::json!({"type": "text", "text": text}),
        Block::ToolUse { id, name, input } => serde_json::json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        }),
        Block::ToolResult { tool_use_id, content, is_error } => serde_json::json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        }),
    }
}

fn msg_to_anthropic(msg: &Msg) -> AnthropicMsg {
    AnthropicMsg {
        role: match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        },
        content: msg.blocks.iter().map(block_to_json).collect(),
    }
}

/// Converts a raw Anthropic response block to a neutral `Block`. Returns `None`
/// for unknown block types (e.g. `thinking`) — callers filter them out.
fn json_to_block(v: &serde_json::Value) -> Option<Block> {
    match v.get("type")?.as_str()? {
        "text" => Some(Block::Text {
            text: v.get("text")?.as_str()?.to_string(),
        }),
        "tool_use" => Some(Block::ToolUse {
            id: v.get("id")?.as_str()?.to_string(),
            name: v.get("name")?.as_str()?.to_string(),
            input: v.get("input")?.clone(),
        }),
        _ => None,
    }
}

// ── InferenceGateway impl ─────────────────────────────────────────────────────

#[async_trait]
impl InferenceGateway for AnthropicGateway {
    async fn infer(&self, request: InferenceRequest) -> Result<InferenceResponse> {
        let body = AnthropicReq {
            model: &self.model,
            max_tokens: request.max_tokens,
            system: request.system.as_deref(),
            messages: request.messages.iter().map(msg_to_anthropic).collect(),
            tools: request
                .tools
                .iter()
                .map(|t| AnthropicTool {
                    name: &t.name,
                    description: &t.description,
                    input_schema: &t.input_schema,
                })
                .collect(),
        };

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await
            .context("sending request to Anthropic API")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.pointer("/error/message")
                        .and_then(|m| m.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or(text);
            bail!("Anthropic API {status}: {msg}");
        }

        let resp: AnthropicResp = resp
            .json()
            .await
            .context("parsing Anthropic response body")?;

        Ok(InferenceResponse {
            blocks: resp.content.iter().filter_map(json_to_block).collect(),
            stop_reason: StopReason::from_str(&resp.stop_reason),
            input_tokens: resp.usage.input_tokens,
            output_tokens: resp.usage.output_tokens,
        })
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_to_json_text() {
        let b = Block::Text {
            text: "hi".to_string(),
        };
        let v = block_to_json(&b);
        assert_eq!(v["type"], "text");
        assert_eq!(v["text"], "hi");
    }

    #[test]
    fn block_to_json_tool_use() {
        let b = Block::ToolUse {
            id: "toolu_abc".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({"path": "/tmp/x"}),
        };
        let v = block_to_json(&b);
        assert_eq!(v["type"], "tool_use");
        assert_eq!(v["id"], "toolu_abc");
        assert_eq!(v["name"], "read_file");
        assert_eq!(v["input"]["path"], "/tmp/x");
    }

    #[test]
    fn block_to_json_tool_result() {
        let b = Block::ToolResult {
            tool_use_id: "toolu_abc".to_string(),
            content: "contents".to_string(),
            is_error: true,
        };
        let v = block_to_json(&b);
        assert_eq!(v["type"], "tool_result");
        assert_eq!(v["tool_use_id"], "toolu_abc");
        assert_eq!(v["is_error"], true);
    }

    #[test]
    fn json_to_block_text() {
        let v = serde_json::json!({"type": "text", "text": "hello"});
        let b = json_to_block(&v).unwrap();
        assert!(matches!(b, Block::Text { text } if text == "hello"));
    }

    #[test]
    fn json_to_block_tool_use() {
        let v = serde_json::json!({
            "type": "tool_use",
            "id": "toolu_123",
            "name": "list_dir",
            "input": {"path": "."}
        });
        let b = json_to_block(&v).unwrap();
        assert!(matches!(b, Block::ToolUse { id, name, .. } if id == "toolu_123" && name == "list_dir"));
    }

    #[test]
    fn json_to_block_unknown_type_returns_none() {
        let v = serde_json::json!({"type": "thinking", "thinking": "..."});
        assert!(json_to_block(&v).is_none());
    }

    #[test]
    fn msg_to_anthropic_role_mapping() {
        use super::super::{Block, Msg, Role};
        let msg = Msg {
            role: Role::User,
            blocks: vec![Block::Text {
                text: "ping".to_string(),
            }],
        };
        let wire = msg_to_anthropic(&msg);
        assert_eq!(wire.role, "user");
        assert_eq!(wire.content[0]["type"], "text");
    }
}
