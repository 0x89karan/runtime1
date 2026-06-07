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
