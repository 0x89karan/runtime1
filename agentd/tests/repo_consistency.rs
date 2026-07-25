//! Repo-consistency tests (audit.1): mechanically enforce prose that used to rot.
//!
//! 1. Template `gated_requires` tokens must be real: every env-var-shaped token in a
//!    template's gated_requires prose must appear somewhere in the product sources
//!    (`docker/` or `agentd/src/`). Kills the librarian-semantic class: the template
//!    gated on VOYAGE_API_KEY while its sidecar read OPENAI_API_KEY, and nothing
//!    noticed (audit86 P2; the "gate" is a badge + spawn warning, so a wrong var name
//!    silently misleads the operator).
//! 2. CLAUDE.md's canonical version line must equal this crate's Cargo version, and
//!    CHANGELOG.md must contain that version's entry. Kills the status-rot class
//!    (cred.3.2-ar-02 — filed after the third recurrence of hand-maintained version
//!    prose drifting; this is the enforcement that makes the fourth impossible).

use std::path::{Path, PathBuf};

/// Tokens that look like env vars but are known-legitimate prose. Empty today —
/// doc-filename references (`FOO.md`) are filtered structurally below. Add a token
/// here only when rewording the template prose is worse than the exception.
const ALLOWLIST: &[&str] = &[];

/// Extracts env-var-shaped tokens: `[A-Z][A-Z0-9]*(_[A-Z0-9]+)+` — uppercase words
/// containing at least one underscore (plain `PATH` or `MCP` don't qualify; tokens
/// always start with an uppercase letter by construction, and a trailing `_` is
/// excluded). Tokens immediately followed by `.md` are doc-filename references,
/// not env vars (e.g. "See docs/MCP_SERVERS.md"), and are skipped.
///
/// Byte-indexing is UTF-8-safe by construction: a token starts only on an
/// ASCII-uppercase byte and consumes only ASCII bytes, so every slice boundary
/// lands on a char boundary.
fn env_tokens(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_uppercase() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_uppercase() || bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            let interior_underscore = word.contains('_') && !word.ends_with('_');
            let is_doc_ref = text[i..].starts_with(".md");
            if interior_underscore && !is_doc_ref && !ALLOWLIST.contains(&word) {
                tokens.push(word.to_string());
            }
        } else {
            i += 1;
        }
    }
    tokens
}

/// Recursively collects file contents under `dir` (skipping anything unreadable —
/// binary files can't contain the ASCII tokens we search for anyway; skipped
/// content can only SHRINK the corpus, which fails closed for this test).
/// Uses `entry.file_type()` (does not follow symlinks) so a symlinked directory
/// can never cause a recursion cycle.
fn read_tree(dir: &Path, out: &mut String) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let Ok(file_type) = entry.file_type() else { continue };
        let path = entry.path();
        if file_type.is_dir() {
            read_tree(&path, out);
        } else if file_type.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                out.push_str(&content);
                out.push('\n');
            }
        }
    }
}

fn template_files(manifest_dir: &Path) -> Vec<PathBuf> {
    let dir = manifest_dir.join("../templates");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".template.toml"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no templates found in {}", dir.display());
    files
}

#[test]
fn gated_requires_tokens_exist_in_product_sources() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Search corpus: everything under docker/ (Python sidecars, entrypoint, compose)
    // plus agentd/src/ (Rust-side providers, e.g. GITHUB_TOKEN in credential/mod.rs).
    let mut corpus = String::new();
    read_tree(&manifest_dir.join("../docker"), &mut corpus);
    read_tree(&manifest_dir.join("src"), &mut corpus);
    assert!(!corpus.is_empty(), "search corpus is empty — repo layout changed?");
    // Whole-token matching, not substring: a truncated var (`OPENAI_API`) must not
    // pass because it's a prefix of the real one somewhere in the corpus. Extract
    // the corpus's own token set once (also avoids re-scanning MBs per template
    // token). Known limitation: a token kept alive only by a comment still counts —
    // "the product mentions it" is the invariant, not "code reads it".
    let corpus_tokens: std::collections::HashSet<String> =
        env_tokens(&corpus).into_iter().collect();

    let mut failures = Vec::new();
    for file in template_files(manifest_dir) {
        let raw = std::fs::read_to_string(&file).expect("template readable");
        let parsed: toml::Value = toml::from_str(&raw)
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", file.display()));
        let Some(gated) = parsed
            .get("template")
            .and_then(|t| t.get("gated_requires"))
            .and_then(|g| g.as_str())
        else {
            continue; // ungated template — nothing to check
        };
        let tokens: std::collections::BTreeSet<String> = env_tokens(gated).into_iter().collect();
        for token in tokens {
            if !corpus_tokens.contains(&token) {
                failures.push(format!(
                    "{}: gated_requires names `{token}`, but nothing under docker/ or \
                     agentd/src/ mentions it. Fix: correct the var name to what the \
                     sidecar actually reads, reword the prose if `{token}` is not an \
                     env var, or (last resort) add it to ALLOWLIST in {}",
                    file.display(),
                    file!(),
                ));
            }
        }
    }
    assert!(failures.is_empty(), "stale gated_requires:\n  {}", failures.join("\n  "));
}

/// Unit tests for the token extractor itself — without these, an extractor
/// regression that returns zero tokens makes the gated_requires test pass
/// vacuously across the whole catalogue (the guard silently stops guarding).
#[test]
fn env_tokens_extracts_and_filters() {
    assert_eq!(env_tokens("needs OPENAI_API_KEY set"), vec!["OPENAI_API_KEY"]);
    assert_eq!(
        env_tokens("OAUTH_CLIENT_ID and OAUTH_CLIENT_SECRET (Google)"),
        vec!["OAUTH_CLIENT_ID", "OAUTH_CLIENT_SECRET"]
    );
    assert!(
        env_tokens("See docs/MCP_SERVERS.md for setup").is_empty(),
        ".md doc-filename references are not env vars"
    );
    assert!(
        env_tokens("runsc must be on PATH; the MCP server").is_empty(),
        "underscore-free uppercase words don't qualify"
    );
    assert!(
        env_tokens("TRAILING_ token").is_empty(),
        "trailing-underscore words don't qualify"
    );
    assert_eq!(
        env_tokens("emoji ⚠ then TRIGGER_CRON at end"),
        vec!["TRIGGER_CRON"],
        "multi-byte UTF-8 must not break scanning; end-of-string token kept"
    );
}

/// Negative control: the real audit86-P2 rot token must extract as a token and
/// must NOT exist in the product corpus — proving the detection path can fire.
#[test]
fn stale_voyage_token_would_be_detected() {
    assert_eq!(env_tokens("VOYAGE_API_KEY"), vec!["VOYAGE_API_KEY"]);
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut corpus = String::new();
    read_tree(&manifest_dir.join("../docker"), &mut corpus);
    read_tree(&manifest_dir.join("src"), &mut corpus);
    let corpus_tokens: std::collections::HashSet<String> =
        env_tokens(&corpus).into_iter().collect();
    assert!(
        !corpus_tokens.contains("VOYAGE_API_KEY"),
        "VOYAGE_API_KEY crept back into docker/ or agentd/src/ — if a template \
         gates on it again, the consistency test would now pass wrongly"
    );
}

#[test]
fn claude_md_version_line_matches_cargo_and_changelog() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo_version = env!("CARGO_PKG_VERSION");

    let claude_md_path = manifest_dir.join("../CLAUDE.md");
    let claude_md = std::fs::read_to_string(&claude_md_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", claude_md_path.display()));
    let marker = "**Current version:** v";
    let line = claude_md
        .lines()
        .find(|l| l.contains(marker))
        .unwrap_or_else(|| {
            panic!(
                "CLAUDE.md has no canonical version line (`{marker}X.Y.Z …`) — \
                 add it at the top of the \"Current status\" section"
            )
        });
    let after = &line[line.find(marker).unwrap() + marker.len()..];
    let claimed: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    // A sentence period directly after the version would be swallowed by
    // take_while — trim it so "v0.86.2." can't false-mismatch "0.86.2".
    let claimed = claimed.trim_end_matches('.').to_string();
    assert_eq!(
        claimed, cargo_version,
        "CLAUDE.md's canonical version line says v{claimed} but agentd/Cargo.toml \
         says {cargo_version}. Edit the `**Current version:**` line in CLAUDE.md's \
         \"Current status\" section (this is release-checklist step; see the HTML \
         comment beside the line)."
    );

    let changelog = std::fs::read_to_string(manifest_dir.join("../CHANGELOG.md"))
        .expect("CHANGELOG.md readable");
    let heading = format!("## [v{cargo_version}]");
    assert!(
        changelog.contains(&heading),
        "CHANGELOG.md has no `{heading}` entry for the current Cargo version"
    );
}

/// hardening.1 (/review catch): `docker/oauth_mcp.py`'s schema-drift guard (self-test 22) reads
/// `tests/fixtures/google.json` relative to its own path, and now SKIPs when that file is absent
/// — correct in-image (the fixture isn't shipped), but it means deleting or moving the fixture in
/// the repo would silently disable the drift guard with CI staying green. This runner-side assert
/// pins the fixture's existence so a "skip" can only ever mean "in-image", never "someone moved it".
/// If the fixture is intentionally relocated, update oauth_mcp.py's path AND this test together.
#[test]
fn oauth_schema_drift_fixture_present_on_runner() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/google.json");
    assert!(
        fixture.exists(),
        "tests/fixtures/google.json is missing — oauth_mcp.py's schema-drift guard (self-test 22) \
         reads it and SKIPs when absent, so removing/renaming it silently disables the guard \
         repo-wide. Restore it, or move it and update oauth_mcp.py's fixture path + this assert."
    );
}
