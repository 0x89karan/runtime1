//! AUDIT-v0.97 P2-8: every sidecar a distro config references under
//! `/usr/lib/agentos/docker/` MUST exist in `docker/` so `distro/Makefile` packages it.
//!
//! The distro-brick class this kills (twice: cap.2 semantic-kb caps, ux.12 telegram_mcp): a
//! config declares `command="python3", args=["/usr/lib/agentos/docker/<x>.py"]` but the
//! sidecar isn't in `docker/`, so the QEMU rootfs boots then fails the tool call — discoverable
//! only by reading flight.jsonl inside the VM. `config_parse_all.rs` parse-proves the specs but
//! never checks the referenced files are packaged; this does. Fails at the workspace-test gate,
//! not at 03:17 UTC in a QEMU boot.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/agentd
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Extract every `/usr/lib/agentos/docker/<name>.py` referenced in a config's text.
fn referenced_sidecars(text: &str) -> Vec<String> {
    const MARKER: &str = "/usr/lib/agentos/docker/";
    let mut out = Vec::new();
    for (i, _) in text.match_indices(MARKER) {
        let rest = &text[i + MARKER.len()..];
        // A sidecar file name: [A-Za-z0-9_.-]+ up to the closing `.py`.
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
            .collect();
        if name.ends_with(".py") {
            out.push(name);
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn every_distro_referenced_sidecar_is_packaged() {
    let root = repo_root();
    let cfg_dir = root.join("distro/overlay/etc/agentd");
    let docker = root.join("docker");

    let mut checked = 0usize;
    let mut refs_total = 0usize;
    for entry in std::fs::read_dir(&cfg_dir).expect("distro config dir readable") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        checked += 1;
        let text = std::fs::read_to_string(&path).unwrap();
        for name in referenced_sidecars(&text) {
            refs_total += 1;
            let src = docker.join(&name);
            assert!(
                src.is_file(),
                "{} references /usr/lib/agentos/docker/{name} but docker/{name} does not exist \
                 — distro/Makefile would ship a rootfs that boots then fails the tool call \
                 (AUDIT-v0.97 P2-8). Add the sidecar to docker/ (the Makefile packages docker/*_mcp.py \
                 via the wildcard).",
                path.display(),
            );
        }
    }
    assert!(checked > 0, "expected at least one distro config TOML in {}", cfg_dir.display());
    assert!(refs_total > 0, "expected at least one sidecar reference across the distro configs");
}

#[test]
fn referenced_sidecars_parses_names() {
    // Guard the extractor itself (so a false-green can't hide a broken scan).
    let t = r#"args = ["/usr/lib/agentos/docker/cron_mcp.py"]
               args = ["/usr/lib/agentos/docker/telegram_mcp.py"]"#;
    assert_eq!(referenced_sidecars(t), vec!["cron_mcp.py".to_string(), "telegram_mcp.py".to_string()]);
    assert!(referenced_sidecars("no sidecars here").is_empty());
}
