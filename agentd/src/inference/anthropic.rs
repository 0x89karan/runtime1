use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

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
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            client,
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
}

/// Block accumulator for SSE streaming — one entry per content_block_start index.
#[cfg_attr(test, derive(Debug))]
enum BlockAcc {
    Text { buf: String },
    ToolUse { id: String, name: String, input_json: String },
}

/// Parsed result of a single SSE `data:` line. Returns `None` for lines that
/// need no action (ping, comment, unknown event type).
#[cfg_attr(test, derive(Debug))]
enum SseAction {
    InputTokens(u32),
    BlockStart { index: usize, acc: BlockAcc },
    TextDelta { index: usize, text: String },
    InputJsonDelta { index: usize, json: String },
    MessageDelta { stop_reason: String, output_tokens: u32 },
    MessageStop,
    ApiError(String),
}

fn parse_sse_event(event_type: &str, data: &serde_json::Value) -> Option<SseAction> {
    match event_type {
        "message_start" => {
            let tokens = data.pointer("/message/usage/input_tokens")?.as_u64()? as u32;
            Some(SseAction::InputTokens(tokens))
        }
        "content_block_start" => {
            let index = data["index"].as_u64()? as usize;
            let block = &data["content_block"];
            let acc = match block["type"].as_str()? {
                "text" => BlockAcc::Text { buf: String::new() },
                "tool_use" => BlockAcc::ToolUse {
                    id:         block["id"].as_str().unwrap_or("").to_string(),
                    name:       block["name"].as_str().unwrap_or("").to_string(),
                    input_json: String::new(),
                },
                _ => return None,
            };
            Some(SseAction::BlockStart { index, acc })
        }
        "content_block_delta" => {
            let index = data["index"].as_u64()? as usize;
            let delta = &data["delta"];
            match delta["type"].as_str()? {
                "text_delta" => {
                    let text = delta["text"].as_str()?.to_string();
                    if text.is_empty() { return None; }
                    Some(SseAction::TextDelta { index, text })
                }
                "input_json_delta" => {
                    let json = delta["partial_json"].as_str().unwrap_or("").to_string();
                    Some(SseAction::InputJsonDelta { index, json })
                }
                _ => None,
            }
        }
        "message_delta" => {
            let stop_reason = data.pointer("/delta/stop_reason")?.as_str()?.to_string();
            let output_tokens = data.pointer("/usage/output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            Some(SseAction::MessageDelta { stop_reason, output_tokens })
        }
        "message_stop" => Some(SseAction::MessageStop),
        "error" => {
            let msg = data.pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
                .to_string();
            Some(SseAction::ApiError(msg))
        }
        _ => None,
    }
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
            model:      &self.model,
            max_tokens: request.max_tokens,
            system:     request.system.as_deref(),
            messages:   request.messages.iter().map(msg_to_anthropic).collect(),
            tools:      request.tools.iter().map(|t| AnthropicTool {
                name:         &t.name,
                description:  &t.description,
                input_schema: &t.input_schema,
            }).collect(),
            stream: false,
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
            blocks:       resp.content.iter().filter_map(json_to_block).collect(),
            stop_reason:  StopReason::from_api_str(&resp.stop_reason),
            input_tokens: resp.usage.input_tokens,
            output_tokens: resp.usage.output_tokens,
        })
    }

    async fn infer_with_stream(
        &self,
        request: InferenceRequest,
        chunk_tx: UnboundedSender<String>,
    ) -> Result<InferenceResponse> {
        let body = AnthropicReq {
            model:      &self.model,
            max_tokens: request.max_tokens,
            system:     request.system.as_deref(),
            messages:   request.messages.iter().map(msg_to_anthropic).collect(),
            tools:      request.tools.iter().map(|t| AnthropicTool {
                name:         &t.name,
                description:  &t.description,
                input_schema: &t.input_schema,
            }).collect(),
            stream: true,
        };

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body)
            .send()
            .await
            .context("sending streaming request to Anthropic API")?;

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

        parse_sse_stream(resp, chunk_tx).await
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}

/// Reads an SSE response body line-by-line and assembles an `InferenceResponse`.
/// Text deltas are forwarded to `chunk_tx`; the channel is dropped when this
/// function returns (either Ok or Err).
async fn parse_sse_stream(
    resp: reqwest::Response,
    chunk_tx: UnboundedSender<String>,
) -> Result<InferenceResponse> {
    const MAX_LINE_BYTES: usize = 1024 * 1024; // 1 MB per-line guard
    // Aggregate cap matching the non-streaming read_bounded_http_body limit.
    const MAX_TOOL_INPUT_BYTES: usize = 4 * 1024 * 1024;

    let mut stream = resp.bytes_stream();
    let mut line_buf = Vec::<u8>::new();
    let mut event_type = String::new();

    let mut input_tokens: u32 = 0;
    let mut output_tokens: u32 = 0;
    let mut stop_reason = String::from("end_turn");
    // Keyed by SSE index (insertion-ordered for in-order assembly).
    let mut blocks: HashMap<usize, BlockAcc> = HashMap::new();

    macro_rules! send_chunk {
        ($text:expr) => {{
            // If receiver dropped (print_fut exited on BrokenPipe), silently
            // discard remaining chunks and keep reading SSE to completion so
            // the agent succeeds and the inference response is not lost.
            let _ = chunk_tx.send($text);
        }};
    }

    while let Some(chunk_result) = stream.next().await {
        let chunk: Bytes = chunk_result.context("reading SSE stream")?;
        for byte in chunk {
            if byte == b'\n' {
                let raw_line = std::str::from_utf8(&line_buf)
                    .context("SSE line is not valid UTF-8")?;
                let line = raw_line.trim_end_matches('\r');

                if line.is_empty() {
                    // Blank line = end of one event block; reset event_type.
                    event_type.clear();
                } else if let Some(rest) = line.strip_prefix("event: ") {
                    event_type = rest.to_string();
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    if rest == "[DONE]" {
                        line_buf.clear();
                        continue;
                    }
                    let data: serde_json::Value = serde_json::from_str(rest)
                        .context("parsing SSE data JSON")?;

                    match parse_sse_event(&event_type, &data) {
                        Some(SseAction::InputTokens(t)) => input_tokens = t,
                        Some(SseAction::BlockStart { index, acc }) => {
                            blocks.insert(index, acc);
                        }
                        Some(SseAction::TextDelta { index, text }) => {
                            if let Some(BlockAcc::Text { buf }) = blocks.get_mut(&index) {
                                buf.push_str(&text);
                                // Only stream the chunk when the block exists; a
                                // TextDelta with no registered block means a
                                // malformed stream where the chunk cannot be
                                // accumulated, producing silent output/state drift.
                                send_chunk!(text);
                            } else {
                                bail!("TextDelta for unregistered block index {index}");
                            }
                        }
                        Some(SseAction::InputJsonDelta { index, json }) => {
                            if let Some(BlockAcc::ToolUse { input_json, .. }) = blocks.get_mut(&index) {
                                if input_json.len() + json.len() > MAX_TOOL_INPUT_BYTES {
                                    bail!("tool input JSON exceeded {MAX_TOOL_INPUT_BYTES} byte limit");
                                }
                                input_json.push_str(&json);
                            }
                        }
                        Some(SseAction::MessageDelta { stop_reason: sr, output_tokens: ot }) => {
                            stop_reason = sr;
                            output_tokens = ot;
                        }
                        Some(SseAction::MessageStop) => {
                            // Assemble response in SSE index order.
                            let mut indexed: Vec<(usize, BlockAcc)> = blocks.drain().collect();
                            indexed.sort_by_key(|(i, _)| *i);
                            let result_blocks: Vec<Block> = indexed
                                .into_iter()
                                .map(|(_, acc)| -> Result<Block> {
                                    Ok(match acc {
                                        BlockAcc::Text { buf } => Block::Text { text: buf },
                                        BlockAcc::ToolUse { id, name, input_json } => {
                                            // Empty input_json means no delta events arrived
                                            // (tool called with no arguments) → use {}.
                                            let input = if input_json.is_empty() {
                                                serde_json::Value::Object(Default::default())
                                            } else {
                                                serde_json::from_str(&input_json)
                                                    .context("parsing tool call input JSON from SSE stream")?
                                            };
                                            Block::ToolUse { id, name, input }
                                        }
                                    })
                                })
                                .collect::<Result<Vec<_>>>()?;
                            return Ok(InferenceResponse {
                                blocks:       result_blocks,
                                stop_reason:  StopReason::from_api_str(&stop_reason),
                                input_tokens,
                                output_tokens,
                            });
                        }
                        Some(SseAction::ApiError(msg)) => {
                            bail!("Anthropic streaming error: {msg}");
                        }
                        None => {}
                    }
                }

                line_buf.clear();
            } else {
                if line_buf.len() >= MAX_LINE_BYTES {
                    bail!("SSE line exceeded 1 MB limit");
                }
                line_buf.push(byte);
            }
        }
    }

    bail!("SSE stream ended without message_stop event")
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
    fn block_to_json_tool_result_not_error() {
        let b = Block::ToolResult {
            tool_use_id: "toolu_xyz".to_string(),
            content: "ok".to_string(),
            is_error: false,
        };
        let v = block_to_json(&b);
        assert_eq!(v["is_error"], false);
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

    #[test]
    fn msg_to_anthropic_assistant_role() {
        use super::super::{Block, Msg, Role};
        let msg = Msg {
            role: Role::Assistant,
            blocks: vec![Block::Text {
                text: "pong".to_string(),
            }],
        };
        let wire = msg_to_anthropic(&msg);
        assert_eq!(wire.role, "assistant");
    }

    #[test]
    fn json_to_block_missing_type_returns_none() {
        assert!(json_to_block(&serde_json::json!({})).is_none());
        assert!(json_to_block(&serde_json::json!({"text": "hi"})).is_none());
    }

    #[test]
    fn json_to_block_text_null_text_returns_none() {
        assert!(json_to_block(&serde_json::json!({"type": "text", "text": null})).is_none());
        assert!(json_to_block(&serde_json::json!({"type": "text"})).is_none());
    }

    #[test]
    fn json_to_block_tool_use_missing_fields_returns_none() {
        // missing id
        assert!(json_to_block(&serde_json::json!({"type": "tool_use", "name": "foo", "input": {}})).is_none());
        // missing name
        assert!(json_to_block(&serde_json::json!({"type": "tool_use", "id": "toolu_1", "input": {}})).is_none());
        // missing input
        assert!(json_to_block(&serde_json::json!({"type": "tool_use", "id": "toolu_1", "name": "foo"})).is_none());
    }

    // ── parse_sse_event unit tests ────────────────────────────────────────────

    #[test]
    fn sse_message_start_extracts_input_tokens() {
        let data = serde_json::json!({ "message": { "usage": { "input_tokens": 42 } } });
        match parse_sse_event("message_start", &data) {
            Some(SseAction::InputTokens(42)) => {}
            other => panic!("expected InputTokens(42), got {other:?}"),
        }
    }

    #[test]
    fn sse_content_block_start_text() {
        let data = serde_json::json!({ "index": 0, "content_block": { "type": "text", "text": "" } });
        match parse_sse_event("content_block_start", &data) {
            Some(SseAction::BlockStart { index: 0, acc: BlockAcc::Text { .. } }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sse_content_block_start_tool_use() {
        let data = serde_json::json!({
            "index": 1,
            "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {} }
        });
        match parse_sse_event("content_block_start", &data) {
            Some(SseAction::BlockStart {
                index: 1,
                acc: BlockAcc::ToolUse { id, name, .. },
            }) if id == "toolu_1" && name == "read_file" => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sse_text_delta_empty_returns_none() {
        let data = serde_json::json!({ "index": 0, "delta": { "type": "text_delta", "text": "" } });
        assert!(parse_sse_event("content_block_delta", &data).is_none());
    }

    #[test]
    fn sse_text_delta_non_empty() {
        let data = serde_json::json!({ "index": 0, "delta": { "type": "text_delta", "text": "Hello" } });
        match parse_sse_event("content_block_delta", &data) {
            Some(SseAction::TextDelta { index: 0, text }) if text == "Hello" => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sse_message_delta_extracts_stop_and_output_tokens() {
        let data = serde_json::json!({
            "delta": { "stop_reason": "end_turn" },
            "usage": { "output_tokens": 99 }
        });
        match parse_sse_event("message_delta", &data) {
            Some(SseAction::MessageDelta { stop_reason, output_tokens: 99 })
                if stop_reason == "end_turn" => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sse_error_event() {
        let data = serde_json::json!({ "error": { "type": "overloaded_error", "message": "overloaded" } });
        match parse_sse_event("error", &data) {
            Some(SseAction::ApiError(msg)) if msg == "overloaded" => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sse_unknown_event_type_returns_none() {
        let data = serde_json::json!({});
        assert!(parse_sse_event("ping", &data).is_none());
        assert!(parse_sse_event("", &data).is_none());
    }

    // ── parse_sse_stream integration tests (mock HTTP server) ─────────────────

    fn build_sse_body(events: &[(&str, &str)]) -> String {
        events
            .iter()
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .collect()
    }

    #[tokio::test]
    async fn sse_stream_text_only() {
        use httpmock::MockServer;
        let server = MockServer::start_async().await;
        let body = build_sse_body(&[
            ("message_start",    r#"{"type":"message_start","message":{"usage":{"input_tokens":10}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#),
            ("content_block_stop",  r#"{"type":"content_block_stop","index":0}"#),
            ("message_delta",       r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#),
            ("message_stop",        r#"{"type":"message_stop"}"#),
        ]);
        let _mock = server.mock_async(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(body);
        }).await;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let client = Client::builder().build().unwrap();
        let resp = client
            .post(format!("{}/v1/messages", server.base_url()))
            .send().await.unwrap();
        let result = parse_sse_stream(resp, tx).await.unwrap();

        let mut chunks = Vec::new();
        while let Ok(c) = rx.try_recv() { chunks.push(c); }

        assert_eq!(chunks, vec!["Hello", " world"]);
        assert_eq!(result.input_tokens, 10);
        assert_eq!(result.output_tokens, 5);
        assert_eq!(result.stop_reason, StopReason::EndTurn);
        assert_eq!(result.blocks.len(), 1);
        assert!(matches!(&result.blocks[0], Block::Text { text } if text == "Hello world"));
    }

    #[tokio::test]
    async fn sse_stream_tool_only_zero_text_chunks() {
        use httpmock::MockServer;
        let server = MockServer::start_async().await;
        let body = build_sse_body(&[
            ("message_start",    r#"{"type":"message_start","message":{"usage":{"input_tokens":8}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"/tmp\"}"}}"#),
            ("content_block_stop",  r#"{"type":"content_block_stop","index":0}"#),
            ("message_delta",       r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}"#),
            ("message_stop",        r#"{"type":"message_stop"}"#),
        ]);
        let _mock = server.mock_async(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(body);
        }).await;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let client = Client::builder().build().unwrap();
        let resp = client
            .post(format!("{}/v1/messages", server.base_url()))
            .send().await.unwrap();
        let result = parse_sse_stream(resp, tx).await.unwrap();

        assert!(rx.try_recv().is_err(), "tool-only stream should send 0 chunks");
        assert_eq!(result.stop_reason, StopReason::ToolUse);
        assert_eq!(result.blocks.len(), 1);
        assert!(matches!(&result.blocks[0], Block::ToolUse { name, .. } if name == "read_file"));
    }

    #[tokio::test]
    async fn sse_stream_error_event_returns_err() {
        use httpmock::MockServer;
        let server = MockServer::start_async().await;
        let body = build_sse_body(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#),
            ("error",         r#"{"type":"error","error":{"type":"overloaded_error","message":"Service overloaded"}}"#),
        ]);
        let _mock = server.mock_async(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(body);
        }).await;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let client = Client::builder().build().unwrap();
        let resp = client
            .post(format!("{}/v1/messages", server.base_url()))
            .send().await.unwrap();
        let err = parse_sse_stream(resp, tx).await.unwrap_err();
        assert!(err.to_string().contains("Service overloaded"), "got: {err}");
    }

    #[tokio::test]
    async fn sse_stream_crlf_lines_parsed_correctly() {
        use httpmock::MockServer;
        let server = MockServer::start_async().await;
        // CRLF line endings (common in HTTP/1.1 SSE)
        let body = concat!(
            "event: message_start\r\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\r\n",
            "\r\n",
            "event: content_block_start\r\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\r\n",
            "\r\n",
            "event: content_block_delta\r\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\r\n",
            "\r\n",
            "event: message_delta\r\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\r\n",
            "\r\n",
            "event: message_stop\r\n",
            "data: {\"type\":\"message_stop\"}\r\n",
            "\r\n",
        );
        let _mock = server.mock_async(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(body);
        }).await;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let client = Client::builder().build().unwrap();
        let resp = client
            .post(format!("{}/v1/messages", server.base_url()))
            .send().await.unwrap();
        let result = parse_sse_stream(resp, tx).await.unwrap();

        let mut chunks = Vec::new();
        while let Ok(c) = rx.try_recv() { chunks.push(c); }
        assert_eq!(chunks, vec!["hi"]);
        assert_eq!(result.stop_reason, StopReason::EndTurn);
    }

    #[tokio::test]
    async fn sse_stream_no_message_stop_returns_err() {
        use httpmock::MockServer;
        let server = MockServer::start_async().await;
        // Stream ends without message_stop
        let body = build_sse_body(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#),
        ]);
        let _mock = server.mock_async(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(body);
        }).await;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let client = Client::builder().build().unwrap();
        let resp = client
            .post(format!("{}/v1/messages", server.base_url()))
            .send().await.unwrap();
        let err = parse_sse_stream(resp, tx).await.unwrap_err();
        assert!(err.to_string().contains("message_stop"), "got: {err}");
    }

    #[tokio::test]
    async fn sse_stream_blocks_assembled_in_index_order() {
        use httpmock::MockServer;
        let server = MockServer::start_async().await;
        // Two blocks: tool_use at index 0, text at index 1 — but we insert in that order already.
        // The assembler must sort by index.
        let body = build_sse_body(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_0","name":"search","input":{}}}"#),
            ("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#),
            ("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"done"}}"#),
            ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
            ("message_stop",  r#"{"type":"message_stop"}"#),
        ]);
        let _mock = server.mock_async(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(body);
        }).await;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let client = Client::builder().build().unwrap();
        let resp = client
            .post(format!("{}/v1/messages", server.base_url()))
            .send().await.unwrap();
        let result = parse_sse_stream(resp, tx).await.unwrap();

        assert_eq!(result.blocks.len(), 2);
        assert!(matches!(&result.blocks[0], Block::ToolUse { name, .. } if name == "search"),
            "expected ToolUse at index 0, got {:?}", result.blocks[0]);
        assert!(matches!(&result.blocks[1], Block::Text { text } if text == "done"),
            "expected Text at index 1, got {:?}", result.blocks[1]);
    }

    // ── Additional coverage tests added during ship review ────────────────────

    #[test]
    fn sse_input_json_delta_parsed() {
        let data = serde_json::json!({
            "index": 0,
            "delta": { "type": "input_json_delta", "partial_json": "{\"k\":\"v\"}" }
        });
        match parse_sse_event("content_block_delta", &data) {
            Some(SseAction::InputJsonDelta { index: 0, json }) if json == "{\"k\":\"v\"}" => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sse_content_block_start_unknown_type_returns_none() {
        // Extended-thinking or future block types must be silently skipped.
        let data = serde_json::json!({
            "index": 0,
            "content_block": { "type": "thinking", "thinking": "..." }
        });
        assert!(parse_sse_event("content_block_start", &data).is_none());
    }

    #[test]
    fn sse_message_stop_returns_action() {
        assert!(matches!(
            parse_sse_event("message_stop", &serde_json::json!({})),
            Some(SseAction::MessageStop)
        ));
    }

    #[tokio::test]
    async fn sse_stream_http_non2xx_returns_err() {
        use httpmock::MockServer;
        let server = MockServer::start_async().await;
        let _mock = server.mock_async(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/messages");
            then.status(401)
                .header("content-type", "application/json")
                .body(r#"{"error":{"type":"authentication_error","message":"Invalid API key"}}"#);
        }).await;

        let client = Client::builder().build().unwrap();
        let resp = client
            .post(format!("{}/v1/messages", server.base_url()))
            .send().await.unwrap();

        // Mirror the status check that infer_with_stream performs before parse_sse_stream.
        assert!(!resp.status().is_success(), "mock must return non-2xx");
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or(text);
        let err_str = format!("Anthropic API {status}: {msg}");
        assert!(
            err_str.contains("401") || err_str.contains("Invalid API key"),
            "expected HTTP 401 error, got: {err_str}"
        );
    }

    #[tokio::test]
    async fn sse_stream_orphaned_text_delta_returns_err() {
        use httpmock::MockServer;
        let server = MockServer::start_async().await;
        // Send a text_delta for index 0 without a preceding content_block_start — malformed stream.
        let body = build_sse_body(&[
            ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#),
            // No content_block_start for index 0 — orphaned delta below.
            ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ghost"}}"#),
        ]);
        let _mock = server.mock_async(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/messages");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(body);
        }).await;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let client = Client::builder().build().unwrap();
        let resp = client
            .post(format!("{}/v1/messages", server.base_url()))
            .send().await.unwrap();
        let err = parse_sse_stream(resp, tx).await.unwrap_err();
        assert!(
            err.to_string().contains("unregistered block"),
            "expected orphaned-block error, got: {err}"
        );
    }
}
