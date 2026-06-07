use std::{io::Write, process::Command};
use tempfile::TempDir;

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
