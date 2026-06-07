use std::{io::Write, process::Command};
use tempfile::TempDir;

/// Invoking with no args should use the default `agent.toml` from CWD.
#[test]
fn no_args_uses_default_agent_toml() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nid = \"default-test\"\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn agentd");

    assert!(
        output.status.success(),
        "expected exit 0 when default agent.toml exists, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── Probe-mode tests ──────────────────────────────────────────────────────────

#[test]
fn probe_missing_key_exits_nonzero() {
    let bin = env!("CARGO_BIN_EXE_agentd");
    let dir = TempDir::new().expect("tempdir");

    let output = Command::new(bin)
        .args(["--probe", "hello"])
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    assert!(
        !output.status.success(),
        "expected non-zero exit when ANTHROPIC_API_KEY is unset"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ANTHROPIC_API_KEY"),
        "expected key name in error message, got stderr: {stderr}"
    );
}

#[test]
fn probe_missing_prompt_arg_exits_nonzero() {
    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg("--probe")
        .output()
        .expect("failed to spawn agentd");

    assert!(
        !output.status.success(),
        "expected non-zero exit when --probe has no prompt argument"
    );
}

/// Live API test — skipped automatically when ANTHROPIC_API_KEY is not set.
#[test]
fn probe_live_returns_nonempty_text() {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("ANTHROPIC_API_KEY not set — skipping live probe test");
        return;
    }

    let bin = env!("CARGO_BIN_EXE_agentd");
    let dir = TempDir::new().expect("tempdir");

    let output = Command::new(bin)
        .args(["--probe", "Reply with exactly the word PONG and nothing else."])
        .current_dir(dir.path())
        .output()
        .expect("failed to spawn agentd");

    assert!(
        output.status.success(),
        "expected exit 0 for live probe, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.trim().is_empty(), "expected non-empty response on stdout");

    // Flight log should have inference_request + inference_response events.
    let flight_log = dir.path().join("flight.jsonl");
    assert!(flight_log.exists(), "flight.jsonl was not created");

    let content = std::fs::read_to_string(&flight_log).unwrap();
    let events: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSONL"))
        .collect();

    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();

    assert!(
        kinds.contains(&"inference_request"),
        "expected inference_request event, got kinds: {kinds:?}"
    );
    assert!(
        kinds.contains(&"inference_response"),
        "expected inference_response event, got kinds: {kinds:?}"
    );

    // Token usage must be present.
    let resp_event = events
        .iter()
        .find(|e| e["kind"] == "inference_response")
        .unwrap();
    assert!(
        resp_event["data"]["input_tokens"].as_u64().unwrap_or(0) > 0,
        "expected non-zero input_tokens"
    );
    assert!(
        resp_event["data"]["output_tokens"].as_u64().unwrap_or(0) > 0,
        "expected non-zero output_tokens"
    );
}

#[test]
fn bad_config_path_exits_nonzero() {
    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg("/nonexistent/agentd-test-path.toml")
        .output()
        .expect("failed to spawn agentd");

    assert!(
        !output.status.success(),
        "expected non-zero exit for missing config, got: {:?}",
        output.status
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent") || stderr.contains("agentd-test-path"),
        "expected path in error message, got stderr: {stderr}"
    );
}

#[test]
fn happy_path_writes_flight_log() {
    let dir = TempDir::new().expect("tempdir");

    let cfg_path = dir.path().join("agent.toml");
    std::fs::write(
        &cfg_path,
        r#"
[agent]
id = "test-agent"
task = "smoke test"
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
        output.status.success(),
        "expected exit 0 for valid config, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let flight_log = dir.path().join("flight.jsonl");
    assert!(flight_log.exists(), "flight.jsonl was not created");

    let content = std::fs::read_to_string(&flight_log).unwrap();
    let event: serde_json::Value = serde_json::from_str(content.trim())
        .expect("flight.jsonl should contain valid JSONL");

    assert_eq!(event["kind"], "agent_spawned");
    assert_eq!(event["agent"], "test-agent");
    assert!(event["turn"].is_null());
    assert!(event["ts"].is_string());
    assert_eq!(event["data"]["model"], "claude-sonnet-4-6");

    assert!(
        output.stdout.is_empty(),
        "stdout should be empty in p0.1 (nothing written to stdout)"
    );
}

#[test]
fn invalid_toml_exits_nonzero() {
    use tempfile::NamedTempFile;

    let mut tmp = NamedTempFile::new().expect("tempfile");
    writeln!(tmp, "this is not valid toml ][[[").unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(tmp.path())
        .output()
        .expect("failed to spawn agentd");

    assert!(
        !output.status.success(),
        "expected non-zero exit for invalid TOML"
    );
}
