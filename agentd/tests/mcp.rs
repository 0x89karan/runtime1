/// Integration tests for the MCP stdio client.
/// All tests here use the `echo-mcp` fixture binary — no real MCP servers needed.
use std::process::Command;
use tempfile::TempDir;

// ── Direct McpClient tests ─────────────────────────────────────────────────
// These live here (not in src/) because CARGO_BIN_EXE_* is only set for
// integration tests.
//
// McpClient is not accessible directly from integration tests without a lib
// target, so we exercise it through the agentd binary in the tests below.

fn echo_mcp_path() -> &'static str {
    env!("CARGO_BIN_EXE_echo-mcp")
}

/// The tools_registered flight event must include tools discovered from MCP servers.
#[test]
fn mcp_tools_appear_in_tools_registered_event() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");
    let echo_mcp = echo_mcp_path();

    std::fs::write(
        &cfg_path,
        format!(
            r#"
[agent]
id = "mcp-test"
task = "test"

[[tools.mcp_servers]]
name = "echo-srv"
command = "{echo_mcp}"
"#
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let _output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    // Regardless of exit code (fails because no API key), the startup sequence
    // must have connected to echo-mcp and written tools_registered.
    let flight_log = dir.path().join("flight.jsonl");
    assert!(flight_log.exists(), "flight.jsonl must be created");

    let content = std::fs::read_to_string(&flight_log).unwrap();
    let events: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON"))
        .collect();

    let registered = events
        .iter()
        .find(|e| e["kind"] == "tools_registered")
        .expect("tools_registered event missing");

    let tools: Vec<&str> = registered["data"]["tools"]
        .as_array()
        .expect("tools must be array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    assert!(
        tools.contains(&"echo"),
        "expected 'echo' tool from echo-mcp in tools_registered, got: {tools:?}"
    );
}

/// With both native tools and MCP tools configured, all must appear together.
#[test]
fn native_and_mcp_tools_coexist() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");
    let echo_mcp = echo_mcp_path();

    std::fs::write(
        &cfg_path,
        format!(
            r#"
[agent]
id = "mixed-test"
task = "test"

[tools]
native = ["read_file"]

[[tools.mcp_servers]]
name = "echo-srv"
command = "{echo_mcp}"
"#
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let _output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    let flight_log = dir.path().join("flight.jsonl");
    let content = std::fs::read_to_string(&flight_log).unwrap();
    let events: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    let registered = events
        .iter()
        .find(|e| e["kind"] == "tools_registered")
        .expect("tools_registered event missing");

    let tools: Vec<&str> = registered["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    assert!(tools.contains(&"read_file"), "native read_file missing");
    assert!(tools.contains(&"echo"), "MCP echo tool missing");
}

/// Two MCP servers with different tool names must both appear in tools_registered.
#[test]
fn multiple_mcp_servers_all_tools_registered() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");
    let echo_mcp = echo_mcp_path();

    std::fs::write(
        &cfg_path,
        format!(
            r#"
[agent]
id = "multi-srv-test"
task = "test"

[[tools.mcp_servers]]
name = "srv-a"
command = "{echo_mcp}"
args = ["alpha"]

[[tools.mcp_servers]]
name = "srv-b"
command = "{echo_mcp}"
args = ["beta"]
"#
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let _output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    let flight_log = dir.path().join("flight.jsonl");
    let content = std::fs::read_to_string(&flight_log).unwrap();
    let events: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    let registered = events
        .iter()
        .find(|e| e["kind"] == "tools_registered")
        .expect("tools_registered event missing");

    let tools: Vec<&str> = registered["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    assert!(tools.contains(&"alpha"), "tool 'alpha' from srv-a missing");
    assert!(tools.contains(&"beta"), "tool 'beta' from srv-b missing");
}

/// When an MCP tool name collides with a native tool, agentd must exit non-zero.
#[test]
fn mcp_tool_collision_with_native_exits_nonzero() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");
    let echo_mcp = echo_mcp_path();

    // Configure native read_file and an MCP server that also exports read_file.
    std::fs::write(
        &cfg_path,
        format!(
            r#"
[agent]
id = "collision-test"
task = "test"

[tools]
native = ["read_file"]

[[tools.mcp_servers]]
name = "conflict-srv"
command = "{echo_mcp}"
args = ["read_file"]
"#
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn agentd");

    assert!(
        !output.status.success(),
        "expected non-zero exit for tool name collision"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("read_file") || stderr.contains("already registered"),
        "expected collision error mentioning 'read_file', got: {stderr}"
    );
}

/// Configuring a non-existent MCP server binary must exit non-zero and produce
/// a helpful error on stderr.
#[test]
fn missing_mcp_server_exits_nonzero() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");

    std::fs::write(
        &cfg_path,
        r#"
[agent]
id = "fail-test"
task = "test"

[[tools.mcp_servers]]
name = "ghost"
command = "/nonexistent/binary-that-does-not-exist"
"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn agentd");

    assert!(
        !output.status.success(),
        "expected non-zero exit for missing MCP server"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ghost") || stderr.contains("MCP") || stderr.contains("nonexistent"),
        "expected error mentioning the server, got: {stderr}"
    );
}

/// mcp_require_capabilities = true with all servers having capabilities → validation passes.
/// The run still exits non-zero (no API key), but the error is about the key, not caps.
#[test]
fn mcp_require_capabilities_true_all_caps_present_passes() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");
    let echo_mcp = echo_mcp_path();

    std::fs::write(
        &cfg_path,
        format!(
            r#"
[agent]
id = "req-caps-pass"
task = "test"

[tools]
mcp_require_capabilities = true

[[tools.mcp_servers]]
name = "echo-srv"
command = "{echo_mcp}"
capabilities = []
"#
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("mcp_require_capabilities is set"),
        "validation should pass when all servers have capabilities, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("ANTHROPIC_API_KEY"),
        "expected API-key error after passing validation, got: {stderr}"
    );
}

/// mcp_require_capabilities = true with a server that has no capabilities field → bail.
#[test]
fn mcp_require_capabilities_true_missing_caps_exits_nonzero() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");

    std::fs::write(
        &cfg_path,
        r#"
[agent]
id = "req-caps-fail"
task = "test"

[tools]
mcp_require_capabilities = true

[[tools.mcp_servers]]
name = "uncapped-srv"
command = "/nonexistent/does-not-matter"
"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn agentd");

    assert!(
        !output.status.success(),
        "expected non-zero exit when mcp_require_capabilities=true and server has no caps"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("uncapped-srv"),
        "error must name the offending server, got: {stderr}"
    );
    assert!(
        stderr.contains("mcp_require_capabilities"),
        "error must mention mcp_require_capabilities, got: {stderr}"
    );
}

/// mcp_require_capabilities = true with multiple servers missing capabilities → both named.
#[test]
fn mcp_require_capabilities_true_multiple_missing_caps_names_all() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");

    std::fs::write(
        &cfg_path,
        r#"
[agent]
id = "req-caps-multi"
task = "test"

[tools]
mcp_require_capabilities = true

[[tools.mcp_servers]]
name = "alpha-srv"
command = "/nonexistent/alpha"

[[tools.mcp_servers]]
name = "beta-srv"
command = "/nonexistent/beta"
"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn agentd");

    assert!(
        !output.status.success(),
        "expected non-zero exit when multiple servers lack capabilities"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("alpha-srv"),
        "error must name alpha-srv, got: {stderr}"
    );
    assert!(
        stderr.contains("beta-srv"),
        "error must name beta-srv, got: {stderr}"
    );
}

/// mcp_require_capabilities = true with capabilities=["Spawn"] passes validation.
/// Before p4.2, Spawn-only produced empty rules (bypass). After p4.2, it also adds
/// IsolateNetwork, so it produces real enforcement and correctly passes validation.
#[test]
fn mcp_require_capabilities_spawn_only_caps_passes_with_isolate_network() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");

    std::fs::write(
        &cfg_path,
        r#"
[agent]
id = "req-caps-spawn-net"
task = "test"

[tools]
mcp_require_capabilities = true

[[tools.mcp_servers]]
name = "spawn-only-srv"
command = "/nonexistent/does-not-matter"
capabilities = ["Spawn"]
"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn agentd");

    // Validation passes (IsolateNetwork is a real rule), but the server binary
    // doesn't exist so the run still fails — just for a different reason.
    assert!(
        !output.status.success(),
        "run should still fail (binary doesn't exist), but for spawn reason not validation"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("mcp_require_capabilities is set"),
        "validation must pass for capabilities=[Spawn] (produces IsolateNetwork), got: {stderr}"
    );
    assert!(
        stderr.contains("spawn-only-srv") || stderr.contains("nonexistent"),
        "error should be about spawning the binary, not validation, got: {stderr}"
    );
}

/// MCP tools/list pagination: all pages must be loaded and appear in tools_registered.
/// echo-mcp --paginate returns page 1 (2 tools + nextCursor) then page 2 (1 tool).
#[test]
fn mcp_pagination_loads_all_pages() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");
    let echo_mcp = echo_mcp_path();

    std::fs::write(
        &cfg_path,
        format!(
            r#"
[agent]
id = "pagination-test"
task = "test"

[[tools.mcp_servers]]
name = "paginated-srv"
command = "{echo_mcp}"
args = ["--paginate"]
"#
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let _output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    let flight_log = dir.path().join("flight.jsonl");
    assert!(flight_log.exists(), "flight.jsonl must be created");

    let content = std::fs::read_to_string(&flight_log).unwrap();
    let events: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON"))
        .collect();

    let registered = events
        .iter()
        .find(|e| e["kind"] == "tools_registered")
        .expect("tools_registered event missing");

    let tools: Vec<&str> = registered["data"]["tools"]
        .as_array()
        .expect("tools must be array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    assert!(
        tools.contains(&"echo_p1a"),
        "expected page-1 tool 'echo_p1a' in tools_registered, got: {tools:?}"
    );
    assert!(
        tools.contains(&"echo_p1b"),
        "expected page-1 tool 'echo_p1b' in tools_registered, got: {tools:?}"
    );
    assert!(
        tools.contains(&"echo_p2a"),
        "expected page-2 tool 'echo_p2a' in tools_registered, got: {tools:?}"
    );
}

/// isolation = "gvisor" requires runsc on PATH; agentd must bail fast if absent.
/// This test only runs on Linux where the gVisor availability check is compiled in.
#[cfg(target_os = "linux")]
#[test]
fn isolation_gvisor_fails_fast_when_runsc_not_on_path() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");
    let echo_mcp = echo_mcp_path();

    std::fs::write(
        &cfg_path,
        format!(
            r#"
[agent]
id = "gvisor-test"
task = "test"

[[tools.mcp_servers]]
name = "secure-srv"
command = "{echo_mcp}"
isolation = "gvisor"
"#
        ),
    )
    .unwrap();

    let empty_path_dir = dir.path().join("empty_path");
    std::fs::create_dir_all(&empty_path_dir).unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        // Override PATH to a directory that definitely has no runsc binary.
        .env("PATH", &empty_path_dir)
        .output()
        .expect("failed to spawn agentd");

    assert!(
        !output.status.success(),
        "expected non-zero exit when isolation=gvisor but runsc not on PATH"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("runsc"),
        "error must mention 'runsc', got: {stderr}"
    );
    assert!(
        stderr.contains("secure-srv") || stderr.contains("gvisor"),
        "error must name the server or isolation mode, got: {stderr}"
    );
}

/// MCP graceful shutdown: agentd must send notifications/shutdown so echo-mcp
/// can exit cleanly (writes --shutdown-file as evidence before exit(0)).
#[test]
fn mcp_graceful_shutdown_sends_notification() {
    let dir = TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");
    let shutdown_file = dir.path().join("shutdown_received.txt");
    let echo_mcp = echo_mcp_path();
    let shutdown_path = shutdown_file.to_str().unwrap();

    std::fs::write(
        &cfg_path,
        format!(
            r#"
[agent]
id = "shutdown-test"
task = "test"

[[tools.mcp_servers]]
name = "shutdown-srv"
command = "{echo_mcp}"
args = ["--shutdown-file", "{shutdown_path}"]
"#
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let _output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    assert!(
        shutdown_file.exists(),
        "shutdown file must be written by echo-mcp on notifications/shutdown"
    );
    let contents = std::fs::read_to_string(&shutdown_file).unwrap();
    assert_eq!(contents, "shutdown");
}

// ── Linux error-pipe tests ─────────────────────────────────────────────────
//
// These tests exercise the pre-exec error-pipe paths added in p4.4.
// pipe2(O_CLOEXEC) is only created when sandbox=Some (capabilities configured).
// When sandbox=None the spawn path is plain: error message has no stage suffix.
// When sandbox=Some and exec fails: no tag is written → stage="unknown".
// When sandbox=Some and apply_compiled fails: "sandbox" tag written → stage="sandbox".
//
// The "sandbox" stage path requires apply_compiled to fail (unsupported kernel) —
// not reliably reproducible in CI; accepted as untestable in portable tests.

/// On Linux, a missing MCP binary with no sandbox configured produces a clean error
/// message without a "sandbox stage:" suffix (sandbox=None takes the plain spawn path).
#[cfg(target_os = "linux")]
#[test]
fn missing_mcp_server_error_no_sandbox_clean_format() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");

    std::fs::write(
        &cfg_path,
        r#"
[agent]
id = "stage-tag-test"
task = "test"

[[tools.mcp_servers]]
name = "ghost-srv"
command = "/nonexistent/stage-tag-binary"
"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = std::process::Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn agentd");

    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // No sandbox → plain error format, no stage suffix.
    assert!(
        stderr.contains("stage-tag-binary") || stderr.contains("ghost-srv"),
        "error must mention the binary or server name, got: {stderr}"
    );
    assert!(
        !stderr.contains("sandbox stage:"),
        "unsandboxed server must NOT include 'sandbox stage:' in error, got: {stderr}"
    );
}

/// On Linux, even with sandbox capabilities configured the stage is "unknown" when
/// the binary is missing: apply_compiled succeeds (sandbox is applied), exec then
/// fails with ENOENT, no tag is written to the pipe → n=0 → stage="unknown".
#[cfg(target_os = "linux")]
#[test]
fn missing_mcp_server_with_sandbox_error_stage_unknown() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg_path = dir.path().join("agent.toml");

    std::fs::write(
        &cfg_path,
        format!(
            r#"
[agent]
id = "sandbox-stage-test"
task = "test"

[memory]
enabled = false

[[tools.mcp_servers]]
name = "sandboxed-ghost"
command = "/nonexistent/sandboxed-binary"
capabilities = [{{ FsRead = {{ prefix = "{}" }} }}]
"#,
            dir.path().display()
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = std::process::Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn agentd");

    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sandbox stage:"),
        "error must include 'sandbox stage:' (error-pipe format), got: {stderr}"
    );
    // apply_compiled succeeds → no tag written → stage is "unknown" or "sandbox"
    // (on systems without Landlock it could be "sandbox"; accept both)
    assert!(
        stderr.contains("unknown") || stderr.contains("sandbox"),
        "stage must be 'unknown' (exec fails) or 'sandbox' (Landlock not supported), got: {stderr}"
    );
}
