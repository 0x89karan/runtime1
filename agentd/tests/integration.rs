use std::{io::Write, process::Command};
use tempfile::TempDir;

/// Invoking with no args should use `agent.toml` from CWD.
/// We verify this by checking the error refers to the API key (i.e. config WAS
/// found and parsed) rather than to a missing file.
#[test]
fn no_args_uses_default_agent_toml() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nid = \"default-test\"\ntask = \"smoke\"\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    // Without an API key the run fails — but the config was found and parsed,
    // so the error must mention the missing key, not a missing file.
    assert!(
        !output.status.success(),
        "expected non-zero exit when ANTHROPIC_API_KEY is absent"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ANTHROPIC_API_KEY"),
        "expected API-key error (confirming agent.toml was loaded), got stderr: {stderr}"
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

/// Startup events (agent_spawned + tools_registered) are written to the flight
/// log before the API call is made — so they appear even when the run fails
/// due to a missing API key. This test covers those events without network.
#[test]
fn startup_events_written_to_flight_log() {
    let dir = TempDir::new().expect("tempdir");
    let cfg_path = dir.path().join("agent.toml");
    std::fs::write(
        &cfg_path,
        "[agent]\nid = \"test-agent\"\ntask = \"smoke test\"\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(&cfg_path)
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    // Binary exits non-zero (no API key) but must still write the startup events.
    assert!(
        !output.status.success(),
        "expected non-zero exit when ANTHROPIC_API_KEY is absent"
    );

    let flight_log = dir.path().join("flight.jsonl");
    assert!(flight_log.exists(), "flight.jsonl must be created on startup");

    let content = std::fs::read_to_string(&flight_log).unwrap();
    let events: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line must be valid JSON"))
        .collect();

    let spawned = events
        .iter()
        .find(|e| e["kind"] == "agent_spawned")
        .expect("agent_spawned event missing");
    assert_eq!(spawned["agent"], "test-agent");
    assert!(spawned["turn"].is_null());
    assert!(spawned["ts"].is_string());
    assert_eq!(spawned["data"]["model"], "claude-sonnet-4-6");
    // task is logged as task_preview (never the full task string verbatim)
    assert!(spawned["data"]["task_preview"].is_string(), "agent_spawned must have task_preview field");
    assert_eq!(spawned["data"]["task_preview"], "smoke test");
    assert!(spawned["data"]["task"].is_null(), "agent_spawned must NOT have bare task field");

    let registered = events
        .iter()
        .find(|e| e["kind"] == "tools_registered")
        .expect("tools_registered event missing");
    assert!(registered["data"]["tools"].is_array());
}

/// Long task strings are truncated to 200 chars in the agent_spawned event.
#[test]
fn agent_spawned_truncates_long_task() {
    let dir = TempDir::new().expect("tempdir");
    let long_task = "a".repeat(300);
    let cfg_content = format!(
        "[agent]\nid = \"trunc-agent\"\ntask = \"{long_task}\"\n"
    );
    std::fs::write(dir.path().join("agent.toml"), cfg_content).unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let _ = Command::new(bin)
        .arg(dir.path().join("agent.toml"))
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to spawn agentd");

    let content = std::fs::read_to_string(dir.path().join("flight.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON"))
        .collect();

    let spawned = events
        .iter()
        .find(|e| e["kind"] == "agent_spawned")
        .expect("agent_spawned event missing");

    let preview = spawned["data"]["task_preview"]
        .as_str()
        .expect("task_preview must be a string");
    assert!(preview.ends_with('…'), "long task must end with ellipsis");
    assert!(preview.chars().count() <= 201, "task_preview must not exceed 200 chars + ellipsis");
}

/// Live end-to-end agent run — skipped when ANTHROPIC_API_KEY is not set.
#[test]
fn live_agent_run_produces_final_answer() {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("ANTHROPIC_API_KEY not set — skipping live agent test");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    let cfg_path = dir.path().join("agent.toml");
    // Minimal task: just answer directly, no tools required.
    std::fs::write(
        &cfg_path,
        r#"
[agent]
id = "live-test"
task = "Reply with exactly the single word DONE and nothing else."
max_turns = 3
token_budget = 10000
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
        "expected exit 0 for live agent run, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.trim().is_empty(), "expected final answer on stdout");

    let flight_log = dir.path().join("flight.jsonl");
    let content = std::fs::read_to_string(&flight_log).unwrap();
    let kinds: Vec<&str> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            v["kind"].as_str().map(|s| s.to_string()).map(|s| s.leak() as &str)
        })
        .collect();

    for expected in &["agent_spawned", "tools_registered", "perceive",
                      "inference_request", "inference_response", "agent_completed"] {
        assert!(
            kinds.contains(expected),
            "missing flight event '{expected}' — got: {kinds:?}"
        );
    }
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

// ── --no-fuse / AGENTOS_NO_FUSE tests ────────────────────────────────────────
//
// These tests verify the flag and env-var paths added in p4.4.  On macOS the
// `let _ = no_fuse` branch fires (the FUSE code is Linux-only), so we can
// exercise flag parsing and arg-stripping without a FUSE kernel module.

#[test]
fn no_fuse_flag_accepted_with_config_path() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nid = \"no-fuse-test\"\ntask = \"smoke\"\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .args(["--no-fuse", dir.path().join("agent.toml").to_str().unwrap()])
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("AGENTOS_NO_FUSE")
        .output()
        .expect("spawn agentd");

    // Should fail on missing API key, not on an unknown argument.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "expected non-zero exit (no API key)"
    );
    assert!(
        stderr.contains("ANTHROPIC_API_KEY"),
        "expected API-key error, got: {stderr}"
    );
}

#[test]
fn no_fuse_flag_stripped_before_config_routing() {
    // --no-fuse must be stripped so the remaining positional arg is still the
    // config path, not "--no-fuse".  If stripping fails the binary would try to
    // open a file called "--no-fuse" and emit a different error.
    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .args(["--no-fuse", "/nonexistent/agent.toml"])
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("AGENTOS_NO_FUSE")
        .output()
        .expect("spawn agentd");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    // The error must mention the nonexistent path (config load failure),
    // NOT "ANTHROPIC_API_KEY" (which would mean we somehow passed config stage).
    assert!(
        stderr.contains("nonexistent") || stderr.contains("agent.toml"),
        "expected missing-config error, got: {stderr}"
    );
    assert!(
        !stderr.contains("--no-fuse"),
        "--no-fuse must not appear in error output (flag must be stripped): {stderr}"
    );
}

#[test]
fn no_fuse_env_var_accepted() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nid = \"env-no-fuse-test\"\ntask = \"smoke\"\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg(dir.path().join("agent.toml"))
        .env("AGENTOS_NO_FUSE", "1")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("spawn agentd");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ANTHROPIC_API_KEY"),
        "expected API-key error (AGENTOS_NO_FUSE accepted), got: {stderr}"
    );
}

#[test]
fn no_fuse_with_probe_routes_correctly() {
    // --no-fuse must be stripped so --probe is still seen as the first arg.
    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .args(["--no-fuse", "--probe"])
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("AGENTOS_NO_FUSE")
        .output()
        .expect("spawn agentd");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    // Should fail with "requires a prompt argument", not "unknown argument".
    assert!(
        stderr.contains("prompt") || stderr.contains("--probe"),
        "expected probe-arg error, got: {stderr}"
    );
}

#[test]
fn no_fuse_env_var_with_probe_routes_correctly() {
    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg("--probe")
        .env("AGENTOS_NO_FUSE", "1")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("spawn agentd");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("prompt") || stderr.contains("--probe"),
        "expected probe-arg error, got: {stderr}"
    );
}

#[test]
fn no_fuse_with_default_agent_toml() {
    // --no-fuse with no config path should fall back to agent.toml in CWD.
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nid = \"no-fuse-default\"\ntask = \"smoke\"\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .arg("--no-fuse")
        .current_dir(dir.path())
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("AGENTOS_NO_FUSE")
        .output()
        .expect("spawn agentd");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ANTHROPIC_API_KEY"),
        "expected API-key error (fell back to agent.toml), got: {stderr}"
    );
}

/// AGENTOS_NO_FUSE falsy values ("0", "false", "no", "") must NOT activate the
/// no-fuse path — they should behave as if the variable is unset.
#[test]
fn no_fuse_env_var_falsy_values_do_not_activate() {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join("agent.toml"),
        "[agent]\nid = \"falsy-no-fuse\"\ntask = \"smoke\"\n",
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_agentd");
    for falsy in &["0", "false", "no", ""] {
        let output = Command::new(bin)
            .arg(dir.path().join("agent.toml"))
            .env("AGENTOS_NO_FUSE", falsy)
            .env_remove("ANTHROPIC_API_KEY")
            .output()
            .unwrap_or_else(|e| panic!("spawn agentd (AGENTOS_NO_FUSE={falsy}): {e}"));

        // The process should still attempt to run (fails only on missing API key).
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("ANTHROPIC_API_KEY"),
            "AGENTOS_NO_FUSE={falsy:?} must not suppress normal startup; got: {stderr}"
        );
    }
}

/// --log-path without a following value must exit non-zero with an error message.
#[test]
fn log_path_flag_without_value_exits_nonzero() {
    let bin = env!("CARGO_BIN_EXE_agentd");
    let output = Command::new(bin)
        .args(["--probe", "--log-path"])
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("spawn agentd");

    assert!(
        !output.status.success(),
        "--log-path without value must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--log-path") && stderr.contains("requires"),
        "expected '--log-path requires a value' in stderr, got: {stderr}"
    );
}

// ── sandbox_probe integration tests ──────────────────────────────────────────
//
// These tests verify Landlock + seccomp enforcement end-to-end by spawning the
// `sandbox_probe` fixture binary with sandbox rules applied INSIDE the binary
// (after exec), not via pre_exec.
//
// Applying Landlock via pre_exec (before execve) causes ACCESS_FS_READ_FILE to
// block the kernel's ELF loader from reading the probe binary itself, since
// READ_FILE (bit 2) is in ACCESS_FS_HANDLED and the binary lives outside the
// allowed prefix.  The fix: probe applies rules to itself at startup after exec,
// at which point all shared libraries are already loaded.
//
// Gated to Linux: Landlock and seccomp-bpf are Linux-only mechanisms.
// The sandbox_probe binary is declared as [[bin]] in Cargo.toml and compiled as
// part of the normal test build, so CARGO_BIN_EXE_sandbox-probe is always valid.

#[cfg(target_os = "linux")]
mod sandbox_probe_tests {
    use tempfile::TempDir;
    use tokio::process::Command;

    fn probe_bin() -> &'static str {
        env!("CARGO_BIN_EXE_sandbox-probe")
    }

    /// AllowFsRead on a tmpdir grants read access to files inside it.
    #[tokio::test]
    async fn allowed_path_read_succeeds() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("allowed.txt");
        std::fs::write(&file, b"hello").unwrap();

        // The probe applies AllowFsRead{dir} to itself after exec, then reads
        // the file.  File is inside the allowed prefix → exit 0.
        let status = Command::new(probe_bin())
            .arg("--sandbox-read")
            .arg(dir.path().to_str().unwrap())
            .arg("--path")
            .arg(file.to_str().unwrap())
            .status()
            .await
            .unwrap();
        assert_eq!(
            status.code(),
            Some(0),
            "read inside AllowFsRead prefix must succeed (exit 0)"
        );
    }

    /// AllowFsRead on a tmpdir denies reads outside the granted prefix.
    #[tokio::test]
    async fn denied_path_read_fails() {
        let sandbox_dir = TempDir::new().unwrap();
        // Use a second tmpdir as the denied path — guaranteed to exist (unlike
        // /etc/hostname which may be absent in containers), and outside the prefix.
        let outside_dir = TempDir::new().unwrap();
        let outside_file = outside_dir.path().join("secret.txt");
        std::fs::write(&outside_file, b"secret").unwrap();

        // The probe applies AllowFsRead{sandbox_dir} to itself, then tries to read
        // outside_file which is in outside_dir → exit 1.
        let status = Command::new(probe_bin())
            .arg("--sandbox-read")
            .arg(sandbox_dir.path().to_str().unwrap())
            .arg("--path")
            .arg(outside_file.to_str().unwrap())
            .status()
            .await
            .unwrap();
        assert_eq!(
            status.code(),
            Some(1),
            "read outside AllowFsRead prefix must be denied (exit 1)"
        );
    }

    /// Running sandbox_probe with no args exits 2 (usage error).
    #[tokio::test]
    async fn no_args_exits_usage_error() {
        let status = Command::new(probe_bin())
            .status()
            .await
            .unwrap();
        assert_eq!(status.code(), Some(2), "no args must exit 2 (usage)");
    }

    /// --sandbox-read with no prefix argument exits 2.
    #[tokio::test]
    async fn sandbox_read_missing_prefix_exits_2() {
        let status = Command::new(probe_bin())
            .arg("--sandbox-read")
            .status()
            .await
            .unwrap();
        assert_eq!(status.code(), Some(2), "--sandbox-read with no prefix must exit 2");
    }

    /// --sandbox-read with prefix but no --path exits 2.
    #[tokio::test]
    async fn sandbox_read_missing_path_flag_exits_2() {
        let status = Command::new(probe_bin())
            .args(["--sandbox-read", "/tmp"])
            .status()
            .await
            .unwrap();
        assert_eq!(status.code(), Some(2), "--sandbox-read missing --path must exit 2");
    }

    /// --sandbox-read with --path but no path argument exits 2.
    #[tokio::test]
    async fn sandbox_read_missing_path_arg_exits_2() {
        let status = Command::new(probe_bin())
            .args(["--sandbox-read", "/tmp", "--path"])
            .status()
            .await
            .unwrap();
        assert_eq!(status.code(), Some(2), "--sandbox-read --path with no arg must exit 2");
    }

    /// --path with no argument exits 2.
    #[tokio::test]
    async fn path_flag_missing_arg_exits_2() {
        let status = Command::new(probe_bin())
            .arg("--path")
            .status()
            .await
            .unwrap();
        assert_eq!(status.code(), Some(2), "--path with no arg must exit 2");
    }

    /// DenySpawn blocks fork(2) inside the sandboxed process (x86_64 only).
    ///
    /// Uses libc::fork() directly (not Command::new) so we exercise the actual
    /// fork syscall (57) that the seccomp BPF filter blocks.  Command::new uses
    /// clone3(435) which bypasses the filter (documented BP-1 in THREAT_MODEL.md).
    #[cfg(target_arch = "x86_64")]
    #[tokio::test]
    async fn deny_spawn_blocks_fork() {
        // The probe applies DenySpawn to itself, then calls libc::fork() directly.
        // The seccomp BPF kills the process via SIGSYS → no clean exit code.
        let status = Command::new(probe_bin())
            .arg("--sandbox-deny-spawn")
            .status()
            .await
            .unwrap();
        // SIGSYS kills the process — it has no exit code (status.code() is None).
        // assert_ne(code, Some(0)) would pass even for exit 2 (usage error); check
        // for signal-kill explicitly.
        assert!(
            status.code().is_none(),
            "DenySpawn must kill via SIGSYS (no exit code); got {:?}",
            status.code()
        );
    }
}
