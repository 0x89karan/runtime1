use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
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
/// Env var names that must never be forwarded to MCP subprocesses via `passenv`.
/// The scheduler replaces ANTHROPIC_API_KEY with an ephemeral scoped key after
/// spawn; forwarding it here exposes the live production key before that swap fires.
pub const PASSENV_BLOCKLIST: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

use super::{Tool, ToolContext};
use crate::capability::Capability;
use crate::inference::ToolSpec;

/// Unified interface over stdio and HTTP MCP server connections.
///
/// Both `McpClient` (stdio) and `McpHttpClient` (HTTP/SSE) implement this trait.
/// `McpTool` holds `Arc<dyn McpBackend>` so tool registration is transport-agnostic.
#[async_trait]
pub trait McpBackend: Send + Sync {
    /// Send a JSON-RPC request and return the `result` field.
    async fn request(&self, method: &str, params: Value) -> Result<Value>;
    /// Send a JSON-RPC notification (no id, no response expected). Best-effort.
    async fn notify(&self, method: &str) -> Result<()>;
    /// Gracefully shut down this backend.
    async fn shutdown(&self);
    /// Returns `"stdio"` or `"http"` — used for FUSE surface display.
    fn transport_kind(&self) -> &'static str;
}

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
        extra_env: &std::collections::HashMap<String, String>,
        passenv: &[String],
    ) -> Result<(Arc<Self>, Vec<ToolSpec>)> {
        use std::process::Stdio;
        use tokio::process::Command;

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Clear the full parent environment so secrets (ANTHROPIC_API_KEY, etc.)
        // are never inherited by MCP subprocess. Re-add a vetted allowlist only.
        cmd.env_clear();
        for key in &["PATH", "HOME", "USER", "LANG", "LC_ALL", "TMPDIR"] {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }
        // Per-server env overrides from config (mcp_server.env map).
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        // Forward named vars from the parent env (mcp_server.passenv list).
        // Used for API keys that must not be hardcoded in config.
        // PASSENV_BLOCKLIST prevents forwarding Anthropic credentials — the scheduler
        // overwrites ANTHROPIC_API_KEY with an ephemeral key after spawn, so forwarding
        // the live key here would hand the production secret to an untrusted subprocess
        // before the placeholder overwrite fires.
        for name in passenv {
            if PASSENV_BLOCKLIST.contains(&name.as_str()) {
                tracing::warn!(name = %name, "passenv: blocked credential var (use ephemeral key instead)");
                continue;
            }
            if let Ok(val) = std::env::var(name) {
                cmd.env(name, val);
            }
        }

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

    pub async fn notify(&self, method: &str) -> Result<()> {
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
///
/// Bytes are accumulated raw and UTF-8 is validated once at the newline boundary.
/// Per-chunk validation on fill_buf slices would fail on multibyte codepoints
/// that span the 8 KB BufReader fill boundary (F-010).
async fn read_line_bounded(
    reader: &mut BufReader<ChildStdout>,
    limit: usize,
) -> Result<Option<String>> {
    let mut raw: Vec<u8> = Vec::new();

    loop {
        let available = reader
            .fill_buf()
            .await
            .context("reading from MCP server stdout")?;

        if available.is_empty() {
            if raw.is_empty() {
                return Ok(None);
            }
            // EOF mid-line: validate and return what we have.
            let s = String::from_utf8(raw).context("MCP server response is not valid UTF-8")?;
            return Ok(Some(s));
        }

        let newline = available.iter().position(|&b| b == b'\n');
        let end = newline.map(|p| p + 1).unwrap_or(available.len());

        if raw.len() + end > limit {
            return Err(anyhow::anyhow!("MCP server response exceeded {limit} bytes"));
        }

        raw.extend_from_slice(&available[..end]);
        reader.consume(end);

        if newline.is_some() {
            let s = String::from_utf8(raw).context("MCP server response is not valid UTF-8")?;
            return Ok(Some(s));
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

// ── McpBackend impl for McpClient (stdio) ─────────────────────────────────────

#[async_trait]
impl McpBackend for McpClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        McpClient::request(self, method, params).await
    }
    async fn notify(&self, method: &str) -> Result<()> {
        McpClient::notify(self, method).await
    }
    async fn shutdown(&self) {
        McpClient::shutdown(self).await
    }
    fn transport_kind(&self) -> &'static str {
        "stdio"
    }
}

// ── McpHttpClient (Streamable HTTP, MCP spec 2025-03-26) ──────────────────────

/// HTTP/SSE MCP client for Streamable HTTP transport.
///
/// Connects to a remote MCP server over HTTPS. Each JSON-RPC call is a POST
/// to the server's URL; the response is either `application/json` (single result)
/// or `text/event-stream` (SSE stream — the client finds the matching event).
///
/// Auth headers are injected from the host environment at connect time per the
/// secrets-from-env invariant (header values are never logged or written to disk).
pub struct McpHttpClient {
    /// reqwest client with auth headers baked into default_headers.
    client: reqwest::Client,
    url: String,
    server_name: String,
    /// Just the header names (not values) for use in error messages.
    auth_header_names: Vec<String>,
    next_id: AtomicU64,
    /// `Mcp-Session-Id` returned by the server after initialize. Sent on all subsequent requests.
    session_id: StdMutex<Option<String>>,
}

impl McpHttpClient {
    /// Connect to the HTTP MCP server: resolve auth headers from env, run the
    /// initialize handshake, and list available tools.
    ///
    /// Returns `(backend, tool_specs, session_id_present)` — the bool indicates
    /// whether the server returned an `Mcp-Session-Id` header during initialize.
    pub async fn connect(
        server_name: &str,
        url: &str,
        headers_env: &std::collections::HashMap<String, String>,
    ) -> Result<(Arc<dyn McpBackend>, Vec<ToolSpec>, bool)> {
        // Resolve auth headers from env (fail-fast on missing vars).
        let mut header_map = reqwest::header::HeaderMap::new();
        let mut auth_header_names: Vec<String> = Vec::new();
        for (header_name, env_var_name) in headers_env {
            let value = std::env::var(env_var_name).with_context(|| {
                format!(
                    "MCP server {server_name:?}: headers_env references env var {env_var_name:?} \
                     which is not set — export {env_var_name}=<value> before starting agentd"
                )
            })?;
            let hname = reqwest::header::HeaderName::from_bytes(header_name.as_bytes())
                .with_context(|| format!("MCP server {server_name:?}: invalid header name {header_name:?}"))?;
            let hval = reqwest::header::HeaderValue::from_str(&value)
                .map_err(|_| anyhow::anyhow!(
                    "MCP server {server_name:?}: header {header_name:?} value contains non-ASCII bytes"
                ))?;
            header_map.insert(hname, hval);
            auth_header_names.push(header_name.clone());
        }

        let client = reqwest::Client::builder()
            .default_headers(header_map)
            .connect_timeout(Duration::from_secs(10)) // fail fast on unreachable servers
            .timeout(MCP_TIMEOUT) // covers full request lifecycle including body streaming
            .redirect(reqwest::redirect::Policy::none()) // no auth header leakage on redirects
            .build()
            .context("building reqwest client for HTTP MCP server")?;

        let http_client = Arc::new(Self {
            client,
            url: url.to_string(),
            server_name: server_name.to_string(),
            auth_header_names,
            next_id: AtomicU64::new(1),
            session_id: StdMutex::new(None),
        });

        // Initialize handshake.
        http_client
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "agentd", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await
            .context("MCP HTTP initialize")?;

        // notifications/initialized — non-fatal if server returns 404/405.
        let _ = http_client.notify("notifications/initialized").await;

        // List tools with pagination guard.
        let mut all_specs = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MCP_MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let list = http_client
                .request("tools/list", params)
                .await
                .context("MCP HTTP tools/list")?;
            all_specs.extend(parse_tool_list(&list)?);
            cursor = list
                .get("nextCursor")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if cursor.is_none() {
                break;
            }
        }

        if cursor.is_some() {
            tracing::warn!(
                server = server_name,
                "tools/list hit the {MCP_MAX_TOOL_PAGES}-page limit; \
                 remaining tools were not loaded — contact the server operator"
            );
        }

        let session_id_present = http_client.session_id.lock()
            .ok()
            .map(|g| g.is_some())
            .unwrap_or(false);

        Ok((http_client as Arc<dyn McpBackend>, all_specs, session_id_present))
    }

    /// Build and send one POST request, handle session ID capture, return the raw response.
    async fn send_post(&self, body: &Value) -> Result<reqwest::Response> {
        let mut builder = self.client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
            .json(body);

        if let Ok(guard) = self.session_id.lock() {
            if let Some(ref sid) = *guard {
                builder = builder.header("mcp-session-id", sid.as_str());
            }
        }

        let response = builder
            .send()
            .await
            .with_context(|| format!("HTTP POST to MCP server '{}' ({})", self.server_name, self.url))?;

        // Capture session ID from response headers (Streamable HTTP spec, write-once).
        // Only accept the first session ID the server sends; ignore updates from subsequent
        // responses (e.g. notifications/initialized) to prevent server-controlled override.
        // Cap at 512 bytes — reject oversized or non-visible-ASCII values to prevent
        // downstream reqwest header-builder poisoning.
        if let Some(raw) = response.headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            if raw.len() <= 512 {
                if let Ok(mut guard) = self.session_id.lock() {
                    if guard.is_none() {
                        *guard = Some(raw.to_string());
                    }
                }
            } else {
                tracing::warn!(
                    server = %self.server_name,
                    "ignoring oversized Mcp-Session-Id ({} bytes > 512 byte limit)",
                    raw.len()
                );
            }
        }

        Ok(response)
    }
}

#[async_trait]
impl McpBackend for McpHttpClient {
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let response = self.send_post(&msg).await?;
        let status = response.status();

        if !status.is_success() {
            let headers_info = if self.auth_header_names.is_empty() {
                String::new()
            } else {
                let names = self.auth_header_names.iter()
                    .map(|n| format!("'{n}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    " — check that env var{} {} {} set and contain{} the correct value \
                     (header{} {} sent)",
                    if self.auth_header_names.len() == 1 { "" } else { "s" },
                    names,
                    if self.auth_header_names.len() == 1 { "is" } else { "are" },
                    if self.auth_header_names.len() == 1 { "s" } else { "" },
                    if self.auth_header_names.len() == 1 { "" } else { "s" },
                    if self.auth_header_names.len() == 1 { "was" } else { "were" },
                )
            };
            return Err(anyhow::anyhow!(
                "MCP server '{}' returned HTTP {} for '{}'{}",
                self.server_name, status.as_u16(), method, headers_info
            ));
        }

        let content_type = response.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = read_bounded_http_body(response)
            .await
            .with_context(|| format!("reading HTTP response from MCP server '{}'", self.server_name))?;

        let v: Value = if content_type.contains("text/event-stream") {
            parse_sse_stream(&body, id)
                .with_context(|| format!("parsing SSE response from MCP server '{}'", self.server_name))?
        } else {
            let trimmed = body.trim();
            serde_json::from_str(trimmed).with_context(|| {
                // chars().take() avoids panic on multi-byte UTF-8 char boundaries.
                let preview: String = trimmed.chars().take(256).collect();
                format!("parsing JSON response from MCP server '{}': {preview}", self.server_name)
            })?
        };

        if let Some(err) = v.get("error") {
            return Err(anyhow::anyhow!(
                "MCP server '{}' returned JSON-RPC error for '{}': {}",
                self.server_name, method, err
            ));
        }

        // Validate response ID matches (skip for servers that omit id in response).
        let resp_id = v.get("id").and_then(|i| i.as_u64())
            .or_else(|| v.get("id").and_then(|i| i.as_str()).and_then(|s| s.parse::<u64>().ok()));
        if let Some(resp_id) = resp_id {
            if resp_id != id {
                return Err(anyhow::anyhow!(
                    "MCP server '{}': response ID {resp_id} doesn't match request ID {id} for '{method}'",
                    self.server_name
                ));
            }
        }

        v.get("result")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!(
                "MCP server '{}' did not respond to '{}' with a valid JSON-RPC result — \
                 is '{}' an MCP server endpoint?",
                self.server_name, method, self.url
            ))
    }

    async fn notify(&self, method: &str) -> Result<()> {
        let msg = json!({ "jsonrpc": "2.0", "method": method });
        match self.send_post(&msg).await {
            Ok(resp) => {
                let status = resp.status();
                // 404/405/501 = server doesn't handle this notification; silently ignore.
                if !status.is_success()
                    && status != reqwest::StatusCode::NOT_FOUND
                    && status != reqwest::StatusCode::METHOD_NOT_ALLOWED
                    && status.as_u16() != 501
                {
                    tracing::warn!(
                        server = %self.server_name, method,
                        status = status.as_u16(),
                        "HTTP MCP notification returned non-success status"
                    );
                }
                // Drain body (bounded) so the TCP connection can be reused.
                // Errors here just close the connection — that's acceptable for notifications.
                let _ = read_bounded_http_body(resp).await;
            }
            Err(e) => {
                tracing::warn!(server = %self.server_name, method, "HTTP MCP notification failed: {e:#}");
            }
        }
        Ok(())
    }

    async fn shutdown(&self) {
        // Send cancellation if we have a session ID.
        let has_session = self.session_id.lock().ok()
            .map(|g| g.is_some())
            .unwrap_or(false);
        if has_session {
            let _ = self.notify("notifications/shutdown").await;
        }
        // reqwest client is connection-pooled; no explicit close needed.
    }

    fn transport_kind(&self) -> &'static str {
        "http"
    }
}

/// Read an HTTP response body with a byte-count guard to prevent OOM.
/// Uses streaming (`bytes_stream()`) so large payloads are rejected before full allocation.
async fn read_bounded_http_body(response: reqwest::Response) -> Result<String> {
    // Fast path: Content-Length check before streaming.
    if let Some(len) = response.content_length() {
        if len > MAX_RESPONSE_BYTES as u64 {
            return Err(anyhow::anyhow!(
                "MCP server HTTP response Content-Length {len} exceeds limit of {MAX_RESPONSE_BYTES} bytes"
            ));
        }
    }

    let mut stream = response.bytes_stream();
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading HTTP response body chunk")?;
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(anyhow::anyhow!(
                "MCP server HTTP response exceeded limit of {MAX_RESPONSE_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).context("MCP server HTTP response is not valid UTF-8")
}

/// Parse an SSE (text/event-stream) body and find the JSON-RPC response for `expected_id`.
///
/// Per the SSE spec (and MCP 2025-03-26):
/// - Lines starting with `data:` contribute to the current event's data (concatenated with `\n`).
/// - An empty line terminates the current event.
/// - Lines starting with `:`, `event:`, `id:`, `retry:` are skipped.
/// - The first event whose data parses as JSON-RPC with a matching `id` is returned.
fn parse_sse_stream(body: &str, expected_id: u64) -> Result<Value> {
    // Use a String accumulator instead of Vec<String>+join to avoid per-line allocations.
    // clear() retains the heap buffer for the next event.
    let mut current_data = String::new();

    let try_event = |data: &str| -> Option<Value> {
        if data.is_empty() { return None; }
        serde_json::from_str(data).ok()
    };

    let id_matches = |v: &Value| -> bool {
        v["id"].as_u64() == Some(expected_id)
            || v["id"].as_str().and_then(|s| s.parse::<u64>().ok()) == Some(expected_id)
    };

    for line in body.lines() {
        if line.is_empty() {
            // Event boundary — emit current event if it has data.
            if let Some(v) = try_event(&current_data) {
                if id_matches(&v) && (v.get("result").is_some() || v.get("error").is_some()) {
                    return Ok(v);
                }
            }
            current_data.clear();
        } else if let Some(data) = line.strip_prefix("data: ") {
            if !current_data.is_empty() { current_data.push('\n'); }
            current_data.push_str(data);
        } else if let Some(data) = line.strip_prefix("data:") {
            if !current_data.is_empty() { current_data.push('\n'); }
            current_data.push_str(data);
        }
        // Skip: ": comment", "event:", "id:", "retry:" lines per SSE spec.
    }

    // Handle trailing data without a final blank line.
    if let Some(v) = try_event(&current_data) {
        if id_matches(&v) && (v.get("result").is_some() || v.get("error").is_some()) {
            return Ok(v);
        }
    }

    Err(anyhow::anyhow!(
        "no JSON-RPC result for request ID {expected_id} found in SSE stream"
    ))
}

/// A single tool exposed by a remote MCP server.
pub struct McpTool {
    client: Arc<dyn McpBackend>,
    spec: ToolSpec,
    server_name: String,
}

impl McpTool {
    pub fn new(client: Arc<dyn McpBackend>, spec: ToolSpec, server_name: String) -> Self {
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

    async fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
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

    // ── parse_sse_stream tests ──────────────────────────────────────────────

    #[test]
    fn sse_single_event_json_response() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        let v = parse_sse_stream(body, 1).unwrap();
        assert_eq!(v["result"]["tools"], json!([]));
    }

    #[test]
    fn sse_comment_and_metadata_lines_ignored() {
        let body = concat!(
            ": ping\n",
            "event: message\n",
            "id: 42\n",
            "retry: 3000\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":5,\"result\":{\"ok\":true}}\n\n",
        );
        let v = parse_sse_stream(body, 5).unwrap();
        assert_eq!(v["result"]["ok"], true);
    }

    #[test]
    fn sse_skips_unmatched_id() {
        let body = concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":99,\"result\":{\"x\":1}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"x\":2}}\n\n",
        );
        let v = parse_sse_stream(body, 7).unwrap();
        assert_eq!(v["result"]["x"], 2);
    }

    #[test]
    fn sse_multiline_data_concatenated() {
        // SSE spec: multiple data: lines join with \n before parsing.
        let frag1 = "{\"jsonrpc\":\"2.0\",";
        let frag2 = "\"id\":3,\"result\":{\"v\":9}}";
        let body = format!("data: {frag1}\ndata: {frag2}\n\n");
        let v = parse_sse_stream(&body, 3).unwrap();
        assert_eq!(v["result"]["v"], 9);
    }

    #[test]
    fn sse_trailing_data_no_final_blank_line() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}";
        let v = parse_sse_stream(body, 2).unwrap();
        assert!(v["result"].is_object());
    }

    #[test]
    fn sse_no_matching_id_returns_error() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        assert!(parse_sse_stream(body, 9).is_err());
    }

    #[test]
    fn sse_error_response_returned() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":4,\"error\":{\"code\":-32601,\"message\":\"not found\"}}\n\n";
        let v = parse_sse_stream(body, 4).unwrap();
        assert!(v.get("error").is_some());
    }

    #[test]
    fn sse_data_without_space_after_colon() {
        // "data:" with no space is valid SSE.
        let body = "data:{\"jsonrpc\":\"2.0\",\"id\":6,\"result\":{\"bare\":true}}\n\n";
        let v = parse_sse_stream(body, 6).unwrap();
        assert_eq!(v["result"]["bare"], true);
    }

    #[test]
    fn sse_string_id_matches_u64() {
        // Some servers serialize `id` as a JSON string, not number.
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":\"8\",\"result\":{}}\n\n";
        let v = parse_sse_stream(body, 8).unwrap();
        assert!(v["result"].is_object());
    }

    #[test]
    fn sse_empty_body_returns_error() {
        assert!(parse_sse_stream("", 1).is_err());
    }
}

// McpClient handshake and tool-call tests live in tests/mcp.rs (integration
// tests) because CARGO_BIN_EXE_echo-mcp is only available there.
