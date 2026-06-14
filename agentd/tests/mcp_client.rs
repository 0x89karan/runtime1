/// Direct McpClient / McpTool tests.
/// These use the lib target to access agentd's internals without spawning the
/// full binary, which means they can exercise McpTool::invoke paths that the
/// binary-level integration tests cannot reach.
use agentd::tools::{mcp::{McpClient, McpTool}, Tool, ToolContext};
use serde_json::json;

fn ctx() -> ToolContext {
    ToolContext { agent_id: "test".to_string(), turn: 0, task_fp: String::new() }
}

fn echo_mcp_path() -> &'static str {
    env!("CARGO_BIN_EXE_echo-mcp")
}

/// McpTool::invoke returns the echoed text on success.
#[tokio::test]
async fn mcp_tool_invoke_returns_text() {
    let (client, specs) = McpClient::spawn(echo_mcp_path(), &[], None, &Default::default())
        .await
        .expect("failed to spawn echo-mcp");
    assert_eq!(specs.len(), 1, "echo-mcp must expose exactly one tool");
    let tool = McpTool::new(client, specs.into_iter().next().unwrap(), "echo-mcp".to_string());
    let result = tool.invoke(json!({"text": "hello world"}), &ctx()).await.unwrap();
    assert_eq!(result, "hello world");
}

/// McpTool::invoke converts isError:true + text content into an Err.
#[tokio::test]
async fn mcp_tool_invoke_is_error_with_text_propagates_error() {
    let (client, specs) = McpClient::spawn(echo_mcp_path(), &[], None, &Default::default())
        .await
        .expect("failed to spawn echo-mcp");
    let tool = McpTool::new(client, specs.into_iter().next().unwrap(), "echo-mcp".to_string());
    let err = tool
        .invoke(json!({"text": "TRIGGER_ERROR"}), &ctx())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("deliberate error"),
        "expected error message from server, got: {err}"
    );
}

/// McpTool::invoke falls back to a generic error message when isError:true but
/// content is empty (no text parts).
#[tokio::test]
async fn mcp_tool_invoke_is_error_no_content_uses_fallback() {
    let (client, specs) = McpClient::spawn(echo_mcp_path(), &[], None, &Default::default())
        .await
        .expect("failed to spawn echo-mcp");
    let tool = McpTool::new(client, specs.into_iter().next().unwrap(), "echo-mcp".to_string());
    let err = tool
        .invoke(json!({"text": "TRIGGER_ERROR_NO_CONTENT"}), &ctx())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("returned an error"),
        "expected fallback error message, got: {err}"
    );
}
