//! Pins the credential-preflight asymmetry in `docker/entrypoint.sh` (attn.2 R1).
//!
//! R1 turned the OpenAI preflight from `exit 1` into a warning, because an embeddings key
//! buys only semantic `kb_search` while `kb_put`/`kb_get` are point lookups the morning brief
//! depends on — and once v0.118.0 added `restart: unless-stopped`, a fatal preflight became a
//! restart loop (observed in the field: `Exited (1)`, `RestartCount=10`).
//!
//! The Google preflight is the opposite case and MUST stay fatal: no Gmail means no brief, so
//! a CoS that boots without it would run forever emitting empty briefs — a silent failure,
//! which under the same restart policy is worse than a crash. Before this file that asymmetry
//! was staked on a code comment alone; nothing asserted it.
//!
//! Text-based on purpose: the shell cannot be executed here, and `env_denylist_parity.rs`
//! already establishes parsing this file as the house pattern. No YAML/shell dependency.

const ENTRYPOINT: &str = include_str!("../../docker/entrypoint.sh");

/// The `cos)` case arm only. Anchoring matters: `entrypoint.sh` has a SECOND credential block
/// in the `agent)` arm, so a whole-file `contains("exit 1")` would pass off the wrong copy and
/// be a false green.
fn cos_block() -> &'static str {
    let start = ENTRYPOINT
        .find("\n  cos)")
        .expect("cos) case arm not found — did the entrypoint's case arms move or get renamed?");
    let rest = &ENTRYPOINT[start + 1..];
    let end = rest.find("\n    ;;").map(|e| start + 1 + e).unwrap_or(ENTRYPOINT.len());
    &ENTRYPOINT[start..end]
}

/// The body of an `if` branch, from its condition to the matching `fi` at branch indentation.
fn branch_body(block: &str, condition: &str) -> String {
    let at = block
        .find(condition)
        .unwrap_or_else(|| panic!("condition not found in the cos block: {condition}"));
    let rest = &block[at..];
    let fi = rest.find("\n    fi").unwrap_or(rest.len());
    rest[..fi].to_string()
}

#[test]
fn google_preflight_still_fails_closed() {
    let body = branch_body(cos_block(), "if [ ! -s /run/secrets/google.json ]");
    assert!(
        body.contains("exit 1"),
        "the Google credential preflight must stay FATAL. No Gmail means no brief, so booting \
         without it yields a CoS that runs forever producing empty briefs. attn.2 R1 made only \
         the OpenAI sibling non-fatal; this one must not follow it.\n--- branch body ---\n{body}"
    );
}

#[test]
fn openai_preflight_does_not_exit() {
    let body = branch_body(cos_block(), "if [ -z \"${OPENAI_API_KEY:-}\" ]");
    assert!(
        !body.contains("exit"),
        "the OpenAI preflight must NOT exit. With `restart: unless-stopped` a fatal preflight \
         is an unbounded restart loop, which is exactly the incident attn.2 R1 fixed \
         (RestartCount=10, CoS down for a day).\n--- branch body ---\n{body}"
    );
    assert!(
        body.contains("DEGRADED"),
        "the operator must be told the boot is degraded, not left to infer it from silence"
    );
}

/// Negative control for the two above. Without it, a `branch_body` that silently returned an
/// empty string (renamed condition, changed indentation) would make
/// `google_preflight_still_fails_closed` fail loudly but `openai_preflight_does_not_exit`
/// pass vacuously — the exact asymmetry that hides a broken guard.
#[test]
fn branch_extraction_is_not_vacuous() {
    let block = cos_block();
    assert!(block.len() > 200, "cos block extraction returned almost nothing: {} bytes", block.len());
    assert!(
        !block.contains("TEMPLATE_NAME"),
        "the cos block bled into the agent) arm — the terminator is wrong, so both gate tests \
         would be reading the wrong credential block"
    );
    for cond in ["if [ ! -s /run/secrets/google.json ]", "if [ -z \"${OPENAI_API_KEY:-}\" ]"] {
        let body = branch_body(block, cond);
        assert!(
            body.len() > 40,
            "branch body for {cond} is {} bytes — too short to contain a real branch, so any \
             assertion over it proves nothing",
            body.len()
        );
    }
}
