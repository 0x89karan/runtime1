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

// ── sandbox_probe integration tests ──────────────────────────────────────────
//
// These tests verify that `sandbox::compile()` + `apply_compiled()` actually
// enforce restrictions end-to-end by spawning the `sandbox_probe` fixture binary
// under real Landlock / seccomp rules.
//
// Gated to Linux: Landlock and seccomp-bpf are Linux-only mechanisms.
// The sandbox_probe binary is declared as [[bin]] in Cargo.toml and compiled as
// part of the normal test build, so CARGO_BIN_EXE_sandbox-probe is always valid.

#[cfg(target_os = "linux")]
mod sandbox_probe_tests {
    use std::os::unix::process::CommandExt as _;
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

        let rules = vec![sandbox::SandboxRule::AllowFsRead {
            prefix: dir.path().to_str().unwrap().to_owned(),
        }];
        let compiled = sandbox::compile(&rules).expect("compile sandbox rules");

        let mut cmd = Command::new(probe_bin());
        cmd.args(["--path", file.to_str().unwrap()]);
        // SAFETY: apply_compiled uses only async-signal-safe raw syscalls.
        unsafe {
            cmd.pre_exec(move || {
                sandbox::apply_compiled(&compiled)
                    .map_err(|_| std::io::Error::from_raw_os_error(libc::EPERM))
            });
        }

        let status = cmd.spawn().unwrap().wait().await.unwrap();
        assert_eq!(
            status.code(),
            Some(0),
            "read inside AllowFsRead prefix must succeed (exit 0)"
        );
    }

    /// AllowFsRead on a tmpdir denies reads outside the granted prefix.
    #[tokio::test]
    async fn denied_path_read_fails() {
        let dir = TempDir::new().unwrap();

        let rules = vec![sandbox::SandboxRule::AllowFsRead {
            prefix: dir.path().to_str().unwrap().to_owned(),
        }];
        let compiled = sandbox::compile(&rules).expect("compile sandbox rules");

        let mut cmd = Command::new(probe_bin());
        // /etc/hostname is a small, always-present file that our tmpdir prefix doesn't cover.
        cmd.args(["--path", "/etc/hostname"]);
        // SAFETY: apply_compiled uses only async-signal-safe raw syscalls.
        unsafe {
            cmd.pre_exec(move || {
                sandbox::apply_compiled(&compiled)
                    .map_err(|_| std::io::Error::from_raw_os_error(libc::EPERM))
            });
        }

        let status = cmd.spawn().unwrap().wait().await.unwrap();
        assert_eq!(
            status.code(),
            Some(1),
            "read outside AllowFsRead prefix must be denied (exit 1)"
        );
    }

    /// DenySpawn blocks fork inside the sandboxed process (x86_64 only; seccomp
    /// BPF only compiles a fork/vfork filter on x86_64 per the existing arch gate).
    #[cfg(target_arch = "x86_64")]
    #[tokio::test]
    async fn deny_spawn_blocks_exec() {
        let rules = vec![sandbox::SandboxRule::DenySpawn];
        let compiled = sandbox::compile(&rules).expect("compile sandbox rules");

        let mut cmd = Command::new(probe_bin());
        cmd.arg("--exec");
        // SAFETY: apply_compiled uses only async-signal-safe raw syscalls.
        unsafe {
            cmd.pre_exec(move || {
                sandbox::apply_compiled(&compiled)
                    .map_err(|_| std::io::Error::from_raw_os_error(libc::EPERM))
            });
        }

        let status = cmd.spawn().unwrap().wait().await.unwrap();
        // DenySpawn installs a seccomp BPF that kills the process on fork(2)/vfork(2).
        // sandbox_probe --exec calls Command::new("/bin/true").status() which forks.
        // The process is killed by SIGSYS → exit code is not 0.
        assert_ne!(
            status.code(),
            Some(0),
            "DenySpawn must prevent exec (process must not exit 0)"
        );
    }
}
