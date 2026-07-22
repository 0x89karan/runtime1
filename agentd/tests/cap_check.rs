//! cap.1 acceptance: the two historical misconfigurations fail `agentd check --strict`,
//! and the real shipped config is clean in default mode.
//!
//! Fixtures live in `agentd/tests/fixtures/` (they PARSE but must FAIL check) — a third
//! category distinct from `.github/fixtures` (mounted into images) and must-not-parse
//! specs. Do NOT move them into `.github/fixtures`.

use std::path::Path;

use agentd::check::{check_path, Severity};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
}

#[test]
fn gmail_missing_server_credential_fails_strict_and_default() {
    let p = fixture("cap_gmail_missing_server.toml");
    // The wiring cross-check is an error in BOTH modes (provably inert grant).
    let strict = check_path(&p, true).unwrap();
    let default = check_path(&p, false).unwrap();
    assert!(strict.has_errors(), "Gmail misconfig must fail --strict");
    assert!(default.has_errors(), "Gmail misconfig must fail default too (always-inert)");
    assert!(
        strict.findings.iter().any(|f| f.severity == Severity::Error && f.message.contains("Gmail-outage")),
        "expected the Gmail-outage-class error, got: {:?}",
        strict.findings
    );
}

#[test]
fn relative_fswrite_warns_default_errors_strict() {
    let p = fixture("cap_relative_fswrite.toml");
    let default = check_path(&p, false).unwrap();
    let strict = check_path(&p, true).unwrap();
    assert!(!default.has_errors(), "relative FsWrite is a warning in default mode: {:?}", default.findings);
    assert!(default.findings.iter().any(|f| f.severity == Severity::Warning));
    assert!(strict.has_errors(), "relative FsWrite must be an error under --strict");
}

#[test]
fn rewritten_cos_config_passes_strict() {
    // The actual boot gate (review F5): mimic the entrypoint sed (./output → /data/output)
    // and assert `check --strict` is error-free on the rewritten artifact the runtime execs.
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("cos.agents.toml"),
    )
    .unwrap();
    let rewritten = src.replace("./output", "/data/output");
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("cos.agents.toml");
    std::fs::write(&p, rewritten).unwrap();
    let report = check_path(&p, true).unwrap();
    let errors: Vec<_> = report.findings.iter().filter(|f| f.severity == Severity::Error).collect();
    assert!(errors.is_empty(), "rewritten cos.agents.toml must pass --strict (boot gate); got: {errors:?}");
}

#[test]
fn real_cos_config_is_clean_in_default_mode() {
    // Acceptance: `agentd check` (default) on the shipped cos.agents.toml has no errors
    // (the source config has a relative ./output FsWrite, which is a WARNING in default —
    // the entrypoint rewrites it to absolute before running --strict at boot).
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("cos.agents.toml");
    let report = check_path(&p, false).unwrap();
    let errors: Vec<_> = report.findings.iter().filter(|f| f.severity == Severity::Error).collect();
    assert!(errors.is_empty(), "real cos.agents.toml must be error-free in default mode; got: {errors:?}");
}
