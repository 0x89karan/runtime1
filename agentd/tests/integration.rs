use std::process::Command;

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
fn invalid_toml_exits_nonzero() {
    use std::io::Write;
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
