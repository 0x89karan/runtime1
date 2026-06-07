/// Minimal MCP server fixture for integration tests.
/// Speaks newline-delimited JSON-RPC 2.0 over stdin/stdout.
/// Exposes a single tool named by the first CLI arg (defaults to "echo").
/// The tool returns the `text` argument unchanged.
use std::io::{BufRead, BufReader, Write};

fn main() {
    let tool_name: String = std::env::args().nth(1).unwrap_or_else(|| "echo".into());

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in BufReader::new(stdin.lock()).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Notifications have no "id" — skip them (no response expected).
        let id = match msg.get("id") {
            Some(id) => id.clone(),
            None => continue,
        };

        let method = msg["method"].as_str().unwrap_or("");
        let result = match method {
            "initialize" => serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": { "name": "echo-mcp", "version": "0.1.0" }
            }),
            "tools/list" => serde_json::json!({
                "tools": [{
                    "name": tool_name,
                    "description": "Returns the input text unchanged.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "text": { "type": "string" } },
                        "required": ["text"]
                    }
                }]
            }),
            "tools/call" => {
                let text = msg["params"]["arguments"]["text"]
                    .as_str()
                    .unwrap_or("(empty)");
                match text {
                    "TRIGGER_ERROR" => serde_json::json!({
                        "content": [{ "type": "text", "text": "deliberate error" }],
                        "isError": true
                    }),
                    "TRIGGER_ERROR_NO_CONTENT" => serde_json::json!({
                        "content": [],
                        "isError": true
                    }),
                    _ => serde_json::json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": false
                    }),
                }
            }
            other => {
                // Return a proper JSON-RPC error envelope (not nested inside result).
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("unknown method: {other}") }
                });
                let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap());
                let _ = out.flush();
                continue;
            }
        };

        let resp = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap());
        let _ = out.flush();
    }
}
