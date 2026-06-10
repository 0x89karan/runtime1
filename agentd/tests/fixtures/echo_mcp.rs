/// Minimal MCP server fixture for integration tests.
/// Speaks newline-delimited JSON-RPC 2.0 over stdin/stdout.
///
/// CLI args:
///   [tool_name]              — name of the single tool exposed (default: "echo")
///   --paginate               — tools/list returns two pages via nextCursor
///   --shutdown-file <path>   — write "shutdown" to path on notifications/shutdown, then exit(0)
use std::io::{BufRead, BufReader, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut tool_name = "echo".to_string();
    let mut paginate = false;
    let mut shutdown_file: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--paginate" => paginate = true,
            "--shutdown-file" => {
                i += 1;
                shutdown_file = args.get(i).cloned();
            }
            other if !other.starts_with("--") => tool_name = other.to_string(),
            _ => {}
        }
        i += 1;
    }

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // Tracks which tools/list page we're on for --paginate mode.
    let mut tools_list_page: u32 = 0;

    for line in BufReader::new(stdin.lock()).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let method = msg["method"].as_str().unwrap_or("");

        // Handle notifications (no "id") before the id check.
        if msg.get("id").is_none() {
            if method == "notifications/shutdown" {
                if let Some(path) = &shutdown_file {
                    let _ = std::fs::write(path, "shutdown");
                }
                std::process::exit(0);
            }
            continue;
        }

        let id = msg["id"].clone();

        let result = match method {
            "initialize" => serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": { "name": "echo-mcp", "version": "0.1.0" }
            }),
            "tools/list" => {
                if paginate {
                    let page = tools_list_page;
                    tools_list_page += 1;
                    match page {
                        0 => serde_json::json!({
                            "tools": [
                                {
                                    "name": format!("{tool_name}_p1a"),
                                    "description": "Page 1, tool A.",
                                    "inputSchema": { "type": "object" }
                                },
                                {
                                    "name": format!("{tool_name}_p1b"),
                                    "description": "Page 1, tool B.",
                                    "inputSchema": { "type": "object" }
                                }
                            ],
                            "nextCursor": "page2"
                        }),
                        _ => serde_json::json!({
                            "tools": [{
                                "name": format!("{tool_name}_p2a"),
                                "description": "Page 2, tool A.",
                                "inputSchema": { "type": "object" }
                            }]
                        }),
                    }
                } else {
                    serde_json::json!({
                        "tools": [{
                            "name": tool_name,
                            "description": "Returns the input text unchanged.",
                            "inputSchema": {
                                "type": "object",
                                "properties": { "text": { "type": "string" } },
                                "required": ["text"]
                            }
                        }]
                    })
                }
            }
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
