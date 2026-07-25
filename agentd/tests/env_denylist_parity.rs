//! par.1 / AUDIT-v0.97 P2-10 (audit86-P2-12) — cross-boundary env-denylist drift guard.
//!
//! The boot-time secrets-file loader refuses to export a set of dangerous env-var
//! keys (interpreter/linker/tool-behavior hijacks + boot-behavior flags) out of an
//! operator-supplied `agentos.env`. That denylist is **hand-mirrored** in two
//! standalone boot scripts that cannot share a sourced library:
//!
//!   * `docker/entrypoint.sh`  — the Docker `cos`/`agent` entrypoint (bash)
//!   * `distro/overlay/init`   — the QEMU/bare-metal PID-1 init (busybox sh)
//!
//! A comment in `distro/overlay/init` explicitly says "Kept in sync with
//! docker/entrypoint.sh's loader (ci.1; mechanized diff → par.1)". This test is
//! that mechanized diff: the two denylists MUST be identical, and a drift names the
//! offending source + tokens instead of shipping a boot script that silently exports
//! `LD_PRELOAD` (or forgets to) on one platform but not the other.
//!
//! A third, DIFFERENT-purpose blocklist lives in `docker/shell_mcp.py`
//! (`_LINKER_ENV_BLOCKLIST`): it strips dynamic-linker vars from an *agent-supplied*
//! env dict before spawning a subprocess. It is intentionally NOT equal to the boot
//! denylist (different threat surface). The invariant we can assert across the
//! boundary is the shared linker-hijack floor: every `LD_*` key the boot scripts
//! refuse must also be stripped by shell_mcp (so removing one shared protection
//! trips the guard). shell_mcp legitimately covers MORE (LD_AUDIT/LD_DEBUG/DYLD_*).

use std::collections::BTreeSet;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/agentd
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// A `KEY` token is an env-var identifier: all-caps letters/digits/underscore,
/// starting with a letter or underscore. This is exactly the shape of a denylist
/// entry and NOT the shape of a shell glob-validation token (`*[!A-Za-z0-9_]*`,
/// `''`, `[0-9]*`, `'#'*`), which is how we exclude the key-syntax `case` arms.
fn is_env_ident(tok: &str) -> bool {
    !tok.is_empty()
        && tok.chars().next().map(|c| c.is_ascii_uppercase() || c == '_').unwrap_or(false)
        && tok
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Extract the denylisted env-var keys from a POSIX `sh`/`bash` boot script: every
/// identifier-shaped token appearing in a `case "$..." in TOK|TOK|...) continue ;;`
/// arm. (`continue` = "skip exporting this key" in both loaders.)
fn boot_denylist(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in src.lines() {
        let line = line.trim();
        if !(line.starts_with("case ") && line.contains(" in ") && line.contains("continue")) {
            continue;
        }
        // Grab the pattern list between " in " and the closing ")".
        let after_in = match line.split_once(" in ") {
            Some((_, rest)) => rest,
            None => continue,
        };
        let patterns = match after_in.split_once(')') {
            Some((pats, _)) => pats,
            None => continue,
        };
        for tok in patterns.split('|') {
            let tok = tok.trim().trim_matches('\'').trim();
            if is_env_ident(tok) {
                out.insert(tok.to_string());
            }
        }
    }
    out
}

/// Extract the quoted string entries of `docker/shell_mcp.py`'s
/// `_LINKER_ENV_BLOCKLIST = frozenset({ "LD_PRELOAD", ... })`.
fn shell_mcp_linker_blocklist(src: &str) -> BTreeSet<String> {
    let start = src
        .find("_LINKER_ENV_BLOCKLIST")
        .expect("shell_mcp.py must define _LINKER_ENV_BLOCKLIST");
    let brace = src[start..]
        .find('{')
        .map(|i| start + i)
        .expect("frozenset opening brace");
    let end = src[brace..]
        .find('}')
        .map(|i| brace + i)
        .expect("frozenset closing brace");
    let body = &src[brace + 1..end];
    let mut out = BTreeSet::new();
    // Collect content of each "double-quoted" token.
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            let mut s = String::new();
            for c2 in chars.by_ref() {
                if c2 == '"' {
                    break;
                }
                s.push(c2);
            }
            if is_env_ident(&s) {
                out.insert(s);
            }
        }
    }
    out
}

#[test]
fn boot_env_denylist_is_identical_across_docker_and_distro() {
    let entrypoint = boot_denylist(&read("docker/entrypoint.sh"));
    let init = boot_denylist(&read("distro/overlay/init"));

    // Sanity: the extractor actually found the denylist (guards against a silent
    // "empty == empty" pass if a refactor renames the loop).
    assert!(
        entrypoint.len() >= 10,
        "extractor found too few denylist keys in docker/entrypoint.sh ({}); \
         did the loader move? found: {entrypoint:?}",
        entrypoint.len()
    );
    for anchor in ["LD_PRELOAD", "PATH", "AGENTOS_SKIP_PATH_GUARDS", "PYTHONPATH"] {
        assert!(
            entrypoint.contains(anchor),
            "expected sentinel {anchor} in docker/entrypoint.sh denylist: {entrypoint:?}"
        );
    }

    if entrypoint != init {
        let only_docker: Vec<_> = entrypoint.difference(&init).collect();
        let only_distro: Vec<_> = init.difference(&entrypoint).collect();
        panic!(
            "boot env-denylist DRIFTED between docker/entrypoint.sh and distro/overlay/init.\n  \
             only in docker/entrypoint.sh: {only_docker:?}\n  \
             only in distro/overlay/init:  {only_distro:?}\n  \
             These two loaders are hand-mirrored (see the sync comment in distro/overlay/init); \
             a divergence exports a dangerous key on one platform but not the other. \
             Update BOTH (par.2 will collapse them to one env-expanded source)."
        );
    }
}

#[test]
fn boot_denylisted_linker_vars_are_stripped_by_shell_mcp() {
    let boot = boot_denylist(&read("docker/entrypoint.sh"));
    let shell = shell_mcp_linker_blocklist(&read("docker/shell_mcp.py"));

    // The shared floor: every LD_* the BOOT loaders refuse must also be stripped by
    // shell_mcp's agent-env sanitizer. shell_mcp may (and does) cover more.
    let boot_ld: BTreeSet<_> = boot.iter().filter(|k| k.starts_with("LD_")).cloned().collect();
    assert!(
        !boot_ld.is_empty(),
        "expected the boot denylist to include LD_* linker vars: {boot:?}"
    );
    let missing: Vec<_> = boot_ld.difference(&shell).collect();
    assert!(
        missing.is_empty(),
        "shell_mcp.py's _LINKER_ENV_BLOCKLIST no longer strips linker vars the boot \
         loaders refuse: {missing:?}. The shared linker-hijack protection drifted; \
         re-add them to docker/shell_mcp.py (it may cover more, never fewer)."
    );
}
