use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::Mutex,
    time::timeout,
};

/// Maximum time to wait for any MCP server response (handshake or tool call).
const MCP_TIMEOUT: Duration = Duration::from_secs(30);
/// Grace period after sending notifications/shutdown + SIGTERM before SIGKILL fires.
const MCP_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Maximum bytes accumulated for a single JSON-RPC response line. Checked
/// incrementally (before each chunk is appended) so the buffer never grows
/// beyond this limit plus one internal BufReader fill (~8 KB).
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum characters kept for a tool description to avoid bloating model context.
const MAX_DESC_CHARS: usize = 1024;
/// Maximum byte length for a tool name (MCP convention, matches OpenAI function names).
const MAX_NAME_LEN: usize = 64;
/// Maximum pages fetched during tools/list pagination; prevents infinite loops
/// with buggy or malicious servers that always return nextCursor.
const MCP_MAX_TOOL_PAGES: usize = 100;

use super::Tool;
use crate::capability::Capability;
use crate::inference::ToolSpec;

struct Transport {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// Single-connection JSON-RPC 2.0 client over a child process's stdio.
///
/// CONCURRENCY: `transport` is locked for the full send + receive round-trip in
/// `request()`. This serialises all in-flight calls, which is correct for a
/// single stdio pipe — the server can only handle one request at a time. Phase 1
/// will introduce parallel tool dispatch at the agent level; when that lands,
/// consider multiplexing or per-server connection pools.
///
/// SHUTDOWN: `shutdown()` sends `notifications/shutdown` + SIGTERM and waits up
/// to `MCP_SHUTDOWN_GRACE` for the process to exit. `kill_on_drop(true)` on
/// `child` ensures SIGKILL fires if the process is still alive when McpClient drops.
pub struct McpClient {
    child: Mutex<Child>,
    transport: Mutex<Transport>,
    next_id: AtomicU64,
    /// Set to `true` after a timeout cancels a request. The BufReader's internal
    /// read position is indeterminate after a cancelled future, so all subsequent
    /// calls must fail fast rather than read potentially garbled data.
    broken: AtomicBool,
}

impl McpClient {
    /// Spawn the MCP server process, run the initialize handshake, and list
    /// available tools. Returns `(client, specs)` — the caller wraps each spec
    /// as an `McpTool` and registers it in the `ToolRegistry`.
    ///
    /// `sandbox` — when `Some(compiled)`, the child process is sandboxed via
    /// Landlock + seccomp before exec. Call `sandbox::compile()` in the parent
    /// and pass the result here. When `None`, no sandbox is applied.
    pub async fn spawn(
        command: &str,
        args: &[String],
        sandbox: Option<sandbox::CompiledSandbox>,
    ) -> Result<(Arc<Self>, Vec<ToolSpec>)> {
        use std::process::Stdio;
        use tokio::process::Command;

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Apply pre-compiled sandbox in the child process before exec().
        // apply_compiled() is async-signal-safe: raw syscalls only, no allocation.
        // Gated to Linux: Landlock + seccomp are Linux-only mechanisms.
        #[cfg(not(target_os = "linux"))]
        let _ = sandbox;

        // Spawn the child. On Linux with a sandbox, use a pre-exec error pipe to
        // propagate sandbox failure stage back to the parent. On Linux without a
        // sandbox (or on non-Linux), use a plain spawn with a clean error message.
        #[cfg(target_os = "linux")]
        let mut child = {
            if let Some(compiled) = sandbox {
                // Pre-exec error pipe: only created when a sandbox is configured.
                // O_CLOEXEC on both ends: write_fd closed by exec() on success →
                // parent reads EOF. On pre_exec failure, child writes stage tag before
                // returning EPERM (exec doesn't run, so O_CLOEXEC does not fire).
                let mut fds: [libc::c_int; 2] = [-1; 2];
                let ret = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
                if ret != 0 {
                    return Err(std::io::Error::last_os_error())
                        .context("sandbox error pipe");
                }
                let (read_fd, write_fd) = (fds[0], fds[1]);

                // SAFETY: apply_compiled() uses only async-signal-safe operations (raw
                // syscalls). libc::write on write_fd is also async-signal-safe.
                // CompiledSandbox is Send + Sync.
                unsafe {
                    cmd.pre_exec(move || {
                        if sandbox::apply_compiled(&compiled).is_err() {
                            let tag = b"sandbox";
                            // Write stage tag so the parent can distinguish a sandbox
                            // failure from a missing-binary error.
                            let _ = libc::write(
                                write_fd,
                                tag.as_ptr() as *const libc::c_void,
                                tag.len(),
                            );
                            return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                        }
                        Ok(())
                    });
                }

                let spawn_result = cmd.spawn();

                // Parent closes write end: once the child either exec'd (O_CLOEXEC
                // fired) or exited after a pre_exec error, read(read_fd) returns EOF.
                unsafe { libc::close(write_fd) };

                match spawn_result {
                    Ok(c) => {
                        unsafe { libc::close(read_fd) };
                        c
                    }
                    Err(e) => {
                        // Read the stage tag written by pre_exec (if any).
                        let mut buf = [0u8; 16];
                        let n = unsafe {
                            libc::read(
                                read_fd,
                                buf.as_mut_ptr() as *mut libc::c_void,
                                buf.len(),
                            )
                        };
                        unsafe { libc::close(read_fd) };
                        // Allowlist: only accept the literal tags written by pre_exec.
                        let stage = if n > 0 {
                            let tag = std::str::from_utf8(&buf[..n as usize]).unwrap_or("");
                            if tag == "sandbox" { "sandbox" } else { "unknown" }
                        } else {
                            "unknown"
                        };
                        return Err(anyhow::anyhow!(
                            "spawning MCP server '{}' failed (sandbox stage: '{}'): {}",
                            command, stage, e
                        ));
                    }
                }
            } else {
                // No sandbox configured — plain spawn with a clean error message.
                cmd.spawn()
                    .with_context(|| format!("spawning MCP server '{command}'"))?
            }
        };

        #[cfg(not(target_os = "linux"))]
        let mut child = cmd.spawn()
            .with_context(|| format!("spawning MCP server '{command}'"))?;

        let stdin = child.stdin.take().context("child stdin unavailable")?;
        let stdout = child.stdout.take().context("child stdout unavailable")?;
        let stderr = child.stderr.take().context("child stderr unavailable")?;

        // Drain server stderr so the pipe buffer never fills and blocks the server.
        // Lines are forwarded at DEBUG level, visible with RUST_LOG=debug.
        let cmd_name = command.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(mcp_server = %cmd_name, "{line}");
            }
        });

        let client = Arc::new(Self {
            child: Mutex::new(child),
            transport: Mutex::new(Transport {
                stdin,
                stdout: BufReader::new(stdout),
            }),
            next_id: AtomicU64::new(1),
            broken: AtomicBool::new(false),
        });

        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "agentd", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await
            .context("MCP initialize")?;

        client
            .notify("notifications/initialized")
            .await
            .context("MCP notifications/initialized")?;

        let mut all_specs = Vec::new();
        let mut cursor: Option<String> = None;
        // Guard against misbehaving servers that return nextCursor forever.
        for _ in 0..MCP_MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let list = client
                .request("tools/list", params)
                .await
                .context("MCP tools/list")?;
            all_specs.extend(parse_tool_list(&list)?);
            cursor = list
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if cursor.is_none() {
                break;
            }
        }

        Ok((client, all_specs))
    }

    /// Send a JSON-RPC request and return the `result` field of the response.
    ///
    /// Times out after `MCP_TIMEOUT`. Response bytes are counted incrementally
    /// against `MAX_RESPONSE_BYTES` before allocation to prevent OOM from a
    /// malicious or buggy server. Returns an error if the client is already broken
    /// (a previous timeout left the transport in an unknown state).
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        if self.broken.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!(
                "MCP client is broken — a previous request timed out and the transport state is unknown"
            ));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut t = self.transport.lock().await;
        let line = serde_json::to_string(&msg).context("serializing JSON-RPC request")?;
        t.stdin
            .write_all(line.as_bytes())
            .await
            .context("writing to MCP server stdin")?;
        t.stdin
            .write_all(b"\n")
            .await
            .context("writing newline to MCP server stdin")?;
        t.stdin
            .flush()
            .await
            .context("flushing MCP server stdin")?;

        let result = timeout(MCP_TIMEOUT, async {
            loop {
                let Some(raw) = read_line_bounded(&mut t.stdout, MAX_RESPONSE_BYTES).await? else {
                    return Err(anyhow::anyhow!(
                        "MCP server closed stdout while waiting for response to '{method}'"
                    ));
                };

                let v: Value = serde_json::from_str(raw.trim()).with_context(|| {
                    let preview = &raw.trim()[..raw.trim().len().min(256)];
                    format!("parsing MCP response: {preview}")
                })?;

                // Skip server-sent notifications — they carry a "method" key but no "id".
                if v.get("method").is_some() {
                    continue;
                }

                // Accept both numeric and string IDs per JSON-RPC 2.0 §4.
                let id_matches = v["id"].as_u64() == Some(id)
                    || v["id"].as_str().and_then(|s| s.parse::<u64>().ok()) == Some(id);
                if !id_matches {
                    continue;
                }

                if let Some(err) = v.get("error") {
                    return Err(anyhow::anyhow!("MCP error from '{method}': {err}"));
                }
                return v
                    .get("result")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("JSON-RPC response missing 'result' for '{method}'"));
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_elapsed) => {
                self.broken.store(true, Ordering::Relaxed);
                Err(anyhow::anyhow!(
                    "MCP request '{method}' timed out after {}s",
                    MCP_TIMEOUT.as_secs()
                ))
            }
        }
    }

    async fn notify(&self, method: &str) -> Result<()> {
        if self.broken.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!(
                "MCP client is broken — a previous request timed out"
            ));
        }
        let msg = json!({ "jsonrpc": "2.0", "method": method });
        let mut t = self.transport.lock().await;
        let line = serde_json::to_string(&msg).context("serializing JSON-RPC notification")?;
        let result = timeout(MCP_TIMEOUT, async {
            t.stdin
                .write_all(line.as_bytes())
                .await
                .context("writing notification to MCP server")?;
            t.stdin
                .write_all(b"\n")
                .await
                .context("writing newline to MCP server stdin")?;
            t.stdin
                .flush()
                .await
                .context("flushing notification to MCP server")?;
            Ok::<(), anyhow::Error>(())
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_elapsed) => {
                self.broken.store(true, Ordering::Relaxed);
                Err(anyhow::anyhow!(
                    "MCP notification '{method}' timed out after {}s",
                    MCP_TIMEOUT.as_secs()
                ))
            }
        }
    }

    /// Gracefully shut down the MCP server:
    ///
    /// 1. Send `notifications/shutdown` so the server can flush state.
    /// 2. Give the server `MCP_SHUTDOWN_GRACE` to exit on its own.
    /// 3. If still running, send SIGTERM and wait another `MCP_SHUTDOWN_GRACE`.
    ///
    /// All errors are best-effort; `kill_on_drop` provides the SIGKILL backstop.
    pub async fn shutdown(&self) {
        let _ = self.notify("notifications/shutdown").await;
        let mut child = self.child.lock().await;
        // Give the server time to process the notification and exit cleanly before
        // sending SIGTERM.  Sending SIGTERM immediately races with the server reading
        // the notification from the pipe, which would prevent the clean-exit path.
        if tokio::time::timeout(MCP_SHUTDOWN_GRACE, child.wait())
            .await
            .is_ok()
        {
            return;
        }
        // Server did not exit gracefully — escalate to SIGTERM.
        if let Some(pid) = child.id() {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
        }
        let _ = tokio::time::timeout(MCP_SHUTDOWN_GRACE, child.wait()).await;
    }
}

/// Read one newline-terminated line from `reader`, counting bytes before
/// appending to avoid allocating more than `limit` bytes. Returns `None` at
/// EOF with no bytes read; returns an error if `limit` is exceeded.
async fn read_line_bounded(
    reader: &mut BufReader<ChildStdout>,
    limit: usize,
) -> Result<Option<String>> {
    let mut buf = String::new();
    let mut total = 0usize;

    loop {
        let available = reader
            .fill_buf()
            .await
            .context("reading from MCP server stdout")?;

        if available.is_empty() {
            return Ok(if total == 0 { None } else { Some(buf) });
        }

        let newline = available.iter().position(|&b| b == b'\n');
        let end = newline.map(|p| p + 1).unwrap_or(available.len());

        total += end;
        if total > limit {
            return Err(anyhow::anyhow!("MCP server response exceeded {limit} bytes"));
        }

        let chunk =
            std::str::from_utf8(&available[..end]).context("MCP server response is not valid UTF-8")?;
        buf.push_str(chunk);
        reader.consume(end);

        if newline.is_some() {
            return Ok(Some(buf));
        }
    }
}

fn parse_tool_list(result: &Value) -> Result<Vec<ToolSpec>> {
    let tools = result["tools"]
        .as_array()
        .context("tools/list result.tools must be an array")?;

    tools
        .iter()
        .map(|t| {
            let name = t["name"].as_str().context("tool.name must be a string")?;

            if name.is_empty() {
                return Err(anyhow::anyhow!("MCP server returned a tool with an empty name"));
            }
            if name.len() > MAX_NAME_LEN {
                return Err(anyhow::anyhow!(
                    "MCP tool name {name:?} exceeds {MAX_NAME_LEN} characters"
                ));
            }
            if !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(anyhow::anyhow!(
                    "MCP tool name {name:?} contains invalid characters (allowed: [a-zA-Z0-9_-])"
                ));
            }

            let raw_desc = t.get("description").and_then(|d| d.as_str()).unwrap_or("");
            let description: String = raw_desc.chars().take(MAX_DESC_CHARS).collect();

            // MCP uses camelCase "inputSchema"; store as-is.
            let input_schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or(json!({"type": "object"}));

            Ok(ToolSpec {
                name: name.to_string(),
                description,
                input_schema,
            })
        })
        .collect()
}

/// A single tool exposed by a remote MCP server.
pub struct McpTool {
    client: Arc<McpClient>,
    spec: ToolSpec,
    server_name: String,
}

impl McpTool {
    pub fn new(client: Arc<McpClient>, spec: ToolSpec, server_name: String) -> Self {
        Self { client, spec, server_name }
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn input_schema(&self) -> Value {
        self.spec.input_schema.clone()
    }

    fn required_capability_for(&self, _input: &Value) -> Option<Capability> {
        Some(Capability::Mcp {
            server: self.server_name.clone(),
            tools: vec![self.spec.name.clone()],
        })
    }

    async fn invoke(&self, input: Value) -> Result<String> {
        let result = self
            .client
            .request(
                "tools/call",
                json!({ "name": self.name(), "arguments": input }),
            )
            .await
            .with_context(|| format!("calling MCP tool '{}'", self.name()))?;

        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let msg = text_content(&result)
                .unwrap_or_else(|| format!("MCP tool '{}' returned an error", self.name()));
            return Err(anyhow::anyhow!("{msg}"));
        }

        Ok(text_content(&result).unwrap_or_default())
    }
}

fn text_content(result: &Value) -> Option<String> {
    let parts: Vec<&str> = result["content"]
        .as_array()?
        .iter()
        .filter(|p| p["type"].as_str() == Some("text"))
        .filter_map(|p| p["text"].as_str())
        .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tool_list_happy_path() {
        let result = json!({
            "tools": [{
                "name": "echo",
                "description": "echoes text",
                "inputSchema": {
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }
            }]
        });
        let specs = parse_tool_list(&result).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "echo");
        assert_eq!(specs[0].description, "echoes text");
        assert_eq!(specs[0].input_schema["properties"]["text"]["type"], "string");
    }

    #[test]
    fn parse_tool_list_missing_description_defaults_empty() {
        let result = json!({ "tools": [{ "name": "bare" }] });
        let specs = parse_tool_list(&result).unwrap();
        assert_eq!(specs[0].description, "");
        assert_eq!(specs[0].input_schema["type"], "object");
    }

    #[test]
    fn parse_tool_list_missing_tools_field_errors() {
        let result = json!({ "no_tools": [] });
        assert!(parse_tool_list(&result).is_err());
    }

    #[test]
    fn text_content_joins_multiple_parts() {
        let result = json!({
            "content": [
                { "type": "text", "text": "hello" },
                { "type": "image", "url": "x" },
                { "type": "text", "text": "world" }
            ]
        });
        assert_eq!(text_content(&result).unwrap(), "hello\nworld");
    }

    #[test]
    fn text_content_empty_returns_none() {
        let result = json!({ "content": [{ "type": "image", "url": "x" }] });
        assert!(text_content(&result).is_none());
    }

    #[test]
    fn text_content_absent_content_key_returns_none() {
        assert!(text_content(&json!({})).is_none());
    }

    #[test]
    fn parse_tool_list_null_name_errors() {
        let result = json!({ "tools": [{ "name": null }] });
        assert!(parse_tool_list(&result).is_err());
    }

    #[test]
    fn parse_tool_list_empty_name_errors() {
        let result = json!({ "tools": [{ "name": "" }] });
        let err = parse_tool_list(&result).unwrap_err();
        assert!(err.to_string().contains("empty name"), "got: {err}");
    }

    #[test]
    fn parse_tool_list_invalid_name_chars_errors() {
        for bad in &["my tool", "my.tool", "my/tool", "tool!"] {
            let result = json!({ "tools": [{ "name": bad }] });
            let err = parse_tool_list(&result).unwrap_err();
            assert!(
                err.to_string().contains("invalid characters"),
                "name={bad:?}: got: {err}"
            );
        }
    }

    #[test]
    fn parse_tool_list_too_long_name_errors() {
        let long_name = "a".repeat(MAX_NAME_LEN + 1);
        let result = json!({ "tools": [{ "name": long_name }] });
        let err = parse_tool_list(&result).unwrap_err();
        assert!(err.to_string().contains("exceeds"), "got: {err}");
    }

    #[test]
    fn parse_tool_list_description_truncated_at_max() {
        let long_desc = "x".repeat(MAX_DESC_CHARS + 100);
        let result = json!({ "tools": [{ "name": "tool", "description": long_desc }] });
        let specs = parse_tool_list(&result).unwrap();
        assert_eq!(specs[0].description.chars().count(), MAX_DESC_CHARS);
    }

    #[test]
    fn parse_tool_list_valid_name_chars_accepted() {
        for good in &["echo", "read_file", "list-dir", "Tool42", "a"] {
            let result = json!({ "tools": [{ "name": good }] });
            assert!(parse_tool_list(&result).is_ok(), "name={good:?} should be valid");
        }
    }
}
// McpClient handshake and tool-call tests live in tests/mcp.rs (integration
// tests) because CARGO_BIN_EXE_echo-mcp is only available there.
