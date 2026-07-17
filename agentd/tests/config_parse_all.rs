//! Parse-proves every checked-in agent-spec TOML (audit.1 / audit86-P1-7).
//!
//! The P0-1 class this kills: `distro/overlay/etc/agentd/agent.toml` shipped with a
//! `model_id` key that `ModelConfig` (deny_unknown_fields) rejects — the default QEMU
//! boot panicked PID-1 at config parse and no test noticed. Every spec file now goes
//! through the same entry points boot uses: `toml::from_str::<Config>` (main.rs),
//! per-server `McpServerConfig::validate()`, and `Config::agent_configs()` lowering.

use agentd::config::Config;
use std::path::{Path, PathBuf};

/// Directories holding checked-in agent-spec TOMLs, relative to CARGO_MANIFEST_DIR
/// (= `agentd/`). Never CWD-relative — an integration test's CWD is cargo's choice.
const SPEC_DIRS: &[&str] = &[".", "../docker", "../distro/overlay/etc/agentd"];

/// Known non-spec TOMLs excluded by filename (crate/tool config, not agent specs).
/// Everything else with a `.toml` extension in SPEC_DIRS is treated as a spec, so
/// future spec files are auto-covered — but a new tool-config TOML (e.g. adding
/// `deny.toml`) must be added here or this test fails with a confusing parse error.
const NON_SPEC_FILES: &[&str] = &["Cargo.toml", "clippy.toml", "rustfmt.toml", "deny.toml"];

/// Collects `*.toml` spec files in `dir` (non-recursive), minus NON_SPEC_FILES.
fn spec_files(dir: &Path) -> Vec<PathBuf> {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read spec dir {}: {e}", dir.display()));
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !NON_SPEC_FILES.contains(&n))
        })
        .collect();
    files.sort();
    files
}

/// Runs one spec file through the real boot-time checks. Returns an error string
/// naming the file and the failing stage so the test output is directly actionable.
///
/// Boot checks NOT reproduced here (they need runtime context): main.rs's OV-1
/// filesystem `ensure!`s (egress key vs. MCP FsRead prefixes — path resolution
/// depends on the boot CWD) and anything requiring live env/sockets.
fn check_spec(path: &Path) -> Result<(), String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: unreadable: {e}", path.display()))?;
    let cfg: Config = toml::from_str(&raw)
        .map_err(|e| format!("{}: does not parse as Config: {e}", path.display()))?;
    for server in &cfg.tools.mcp_servers {
        server
            .validate()
            .map_err(|e| format!("{}: MCP server validation failed: {e}", path.display()))?;
    }
    let agents = cfg
        .agent_configs()
        .map_err(|e| format!("{}: agent lowering failed: {e}", path.display()))?;
    // Duplicate agent ids are rejected by the scheduler at boot (scheduler.rs
    // "duplicate agent id" ensure!), not by agent_configs() — reproduce that
    // check here so a dup-id spec can't pass the test and still brick PID-1.
    let mut seen = std::collections::HashSet::new();
    for agent in &agents {
        if !seen.insert(&agent.id) {
            return Err(format!(
                "{}: duplicate agent id {:?} (scheduler rejects this at boot)",
                path.display(),
                agent.id
            ));
        }
    }
    Ok(())
}

#[test]
fn every_checked_in_spec_parses_and_validates() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut checked = 0usize;
    let mut failures = Vec::new();

    for dir in SPEC_DIRS {
        let dir = manifest_dir.join(dir);
        let files = spec_files(&dir);
        // Empty-dir guard: a directory move must fail the test, not silently
        // shrink its coverage to zero.
        assert!(
            !files.is_empty(),
            "no spec TOMLs found in {} — directory moved or glob broken?",
            dir.display()
        );
        for file in files {
            checked += 1;
            if let Err(msg) = check_spec(&file) {
                failures.push(msg);
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {checked} checked-in spec files failed:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// Negative control for the P0-1 class: a config with an unknown [model] key must be
/// rejected at parse (deny_unknown_fields), proving the positive test can catch it.
#[test]
fn fixture_with_unknown_model_key_fails_parse() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/broken_model_id.toml");
    let raw = std::fs::read_to_string(&path).expect("fixture must exist");
    let parsed: Result<Config, _> = toml::from_str(&raw);
    let err = parsed.expect_err("model_id fixture must fail to parse (P0-1 class)");
    assert!(
        err.to_string().contains("model_id"),
        "parse error must name the unknown key, got: {err}"
    );
}

/// Negative control for the dup-id stage: two [[agents]] sharing an id must be
/// rejected by check_spec (mirrors the scheduler's boot-time ensure!).
#[test]
fn fixture_with_duplicate_agent_ids_fails_check() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dup_agent_id.toml");
    let err = check_spec(&path).expect_err("dup-id fixture must fail check_spec");
    assert!(
        err.contains("duplicate agent id"),
        "error must name the dup-id class, got: {err}"
    );
}

/// Negative control for the validate() stage: a config that PARSES but declares a
/// plaintext-HTTP MCP server without `allow_insecure_local` must fail validation.
#[test]
fn fixture_with_insecure_http_server_fails_validate() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/invalid_http_server.toml");
    let raw = std::fs::read_to_string(&path).expect("fixture must exist");
    let cfg: Config = toml::from_str(&raw).expect("fixture must parse — it fails at validate()");
    let err = cfg.tools.mcp_servers[0]
        .validate()
        .expect_err("plaintext http:// without allow_insecure_local must fail validate()");
    assert!(
        err.to_string().contains("allow_insecure_local"),
        "validate error must point at the fix, got: {err}"
    );
}
