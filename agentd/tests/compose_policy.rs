//! attn.1a — `docker-compose.yml` policy guards.
//!
//! Why these exist: the CoS produced three briefs in fifteen days, and the cause was not
//! the brief pipeline. It was that **no compose service had a `restart:` policy**, so the
//! stack only ran while someone had hand-typed `docker compose up`. The Linux/QEMU path
//! already had the equivalent (`Restart=on-failure` in `distro/agentos-cos.service`); the
//! Mac path — the one actually dogfooded — never did.
//!
//! The second guard is the inverse: `agent` must NEVER get a restart policy, because it is
//! a run-to-completion one-shot and `unless-stopped` would restart-loop a finished agent
//! forever, re-spending tokens on every exit.
//!
//! Text assertions rather than YAML parsing: there is no YAML crate in this workspace and
//! `CONVENTIONS.md` asks that new dependencies be justified.
//!
//! ⚠ Be honest about what that costs. An earlier version of this comment claimed
//! "`docker compose config` in CI is the real semantic check". **No workflow runs
//! `docker compose` at all** — `docker-smoke` only does `docker build` + `docker run`. So
//! nothing validates this file's YAML structure; a bad indent or a duplicate key ships
//! unvalidated. Filed as `attn.1a-02`. These assertions check POLICY, not syntax.

const COMPOSE: &str = include_str!("../../docker-compose.yml");

/// Extract one service's body from the `services:` mapping, by name.
///
/// Line-based, anchored to the `services:` section, and terminated on the first non-blank
/// line indented less than 4 spaces — mirroring the already-correct extractor in
/// `agentd/src/main.rs` (`compose_management_port_is_loopback_pinned`).
///
/// The naive version of this (substring search + "first line at exactly 2-space indent
/// ends the block") was wrong in two ways /review caught: `service_block("cos-data")`
/// returned a **volume**, and the last service absorbed the trailing top-level `volumes:`
/// key because 0-indent lines did not terminate it. Today's call sites happened to be
/// unaffected, which is exactly why the negative control below matters.
///
/// Returns the body WITHOUT the `  <name>:` header line.
fn service_block(name: &str) -> Option<String> {
    let lines: Vec<&str> = COMPOSE.lines().collect();
    let services_at = lines.iter().position(|l| l.trim_end() == "services:")?;
    let header = format!("  {name}:");
    // Only look inside the services mapping, and stop at the next top-level key.
    let services_end = lines[services_at + 1..]
        .iter()
        .position(|l| !l.trim().is_empty() && !l.starts_with(' '))
        .map(|i| services_at + 1 + i)
        .unwrap_or(lines.len());
    let start = lines[services_at + 1..services_end]
        .iter()
        .position(|l| l.trim_end() == header)
        .map(|i| services_at + 1 + i)?;
    let end = lines[start + 1..services_end]
        .iter()
        .position(|l| !l.trim().is_empty() && !l.starts_with("    "))
        .map(|i| start + 1 + i)
        .unwrap_or(services_end);
    Some(lines[start + 1..end].join("\n"))
}

/// The declared value of `restart:`, from the first non-comment line, or `None`.
///
/// A bare `block.contains("restart: unless-stopped")` is comment-BLIND and asymmetric with
/// `declares_restart` below, which does filter `#` lines. The cos block's own comment
/// discusses `unless-stopped` vs `always`, so any reword containing the literal would let a
/// real `restart: always` ship green — and `always` is precisely the policy that makes
/// `docker compose stop` un-stoppable. (/review, testing specialist.)
fn restart_value(block: &str) -> Option<String> {
    block
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix("restart:").map(|v| v.trim().trim_matches('"').to_string()))
}

/// Every service name declared under `services:`, so a policy assertion cannot be scoped to
/// a hardcoded list that a future service silently escapes. (/review, Codex.)
fn all_service_names() -> Vec<String> {
    let lines: Vec<&str> = COMPOSE.lines().collect();
    let Some(services_at) = lines.iter().position(|l| l.trim_end() == "services:") else {
        return Vec::new();
    };
    let services_end = lines[services_at + 1..]
        .iter()
        .position(|l| !l.trim().is_empty() && !l.starts_with(' '))
        .map(|i| services_at + 1 + i)
        .unwrap_or(lines.len());
    lines[services_at + 1..services_end]
        .iter()
        .filter(|l| l.starts_with("  ") && !l.starts_with("   ") && !l.trim_start().starts_with('#'))
        .filter_map(|l| l.trim().strip_suffix(':').map(str::to_string))
        .collect()
}

/// A logging option's value from the first non-comment line (`max-size`, `max-file`, ...).
fn logging_opt(block: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    block
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix(&needle).map(|v| v.trim().trim_matches('"').to_lowercase()))
}

/// Does this block declare a real SERVICE-LEVEL `restart:` key?
///
/// Pinned to exactly 4-space indent: a `restart:` nested deeper (under `labels:`,
/// `deploy:`, ...) is not the service's restart policy, and matching it would be a false
/// green. (/review, Codex.) Comment lines are excluded so a comment cannot satisfy it.
fn declares_restart(block: &str) -> bool {
    block.lines().any(|l| {
        l.starts_with("    ")
            && !l.starts_with("     ")
            && l.trim_start().starts_with("restart:")
    })
}

#[test]
fn standing_services_restart_unless_stopped() {
    // These three are STANDING. The startup chain is
    // cos -> semantic-kb-mcp -> qdrant, each `condition: service_healthy` — cos depends_on
    // ONLY semantic-kb-mcp, and qdrant is one hop further out, so qdrant staying dead
    // blocks the CoS transitively rather than directly. If any stays dead the pipeline
    // cannot recover.
    for svc in ["cos", "qdrant", "semantic-kb-mcp"] {
        let block = service_block(svc).unwrap_or_else(|| panic!("service {svc} missing from docker-compose.yml"));
        assert!(
            declares_restart(&block),
            "service `{svc}` has no `restart:` policy. This is the attn.1a root-cause fix: \
             without it the CoS only runs while someone hand-types `docker compose up`, \
             which produced 3 briefs in 15 days."
        );
        assert_eq!(
            restart_value(&block).as_deref(),
            Some("unless-stopped"),
            "service `{svc}` must use `unless-stopped`, not `always`/`on-failure`: an \
             explicit `docker compose stop` has to STAY stopped or the operator cannot turn \
             the pipeline off. Asserted on the parsed value, not a substring, so a comment \
             mentioning the right policy cannot mask a wrong key."
        );
    }
}

#[test]
fn every_restarting_service_also_caps_its_logs() {
    // A restart policy without a log cap is unbounded disk growth: Docker's json-file
    // default has NO max-size, and before `restart:` a failed service exited ONCE and
    // stopped writing. Now a permanently-failing container reprints its error forever
    // (Docker backs off to a 60 s ceiling, so ~1440 restarts/day) with nothing reclaiming
    // it. The two policies must travel together, so assert the coupling rather than the
    // individual values — that way a future `restart:` cannot be added without a cap.
    let names = all_service_names();
    assert!(
        names.len() >= 4,
        "expected to discover every service under `services:`, found {names:?}"
    );
    for svc in &names {
        let block = service_block(svc).unwrap_or_else(|| panic!("service {svc} missing"));
        if declares_restart(&block) {
            let cap = logging_opt(&block, "max-size").unwrap_or_else(|| {
                panic!(
                    "service `{svc}` declares `restart:` but no logging `max-size:` — a \
                     restarting container with uncapped json-file logs grows without bound."
                )
            });
            // Parse it. `max-size: "0"` means UNLIMITED to Docker's json-file driver and a
            // bare `contains("max-size:")` accepted it, as did `10g`. (/review.)
            let mb: u64 = cap.strip_suffix('m').and_then(|n| n.parse().ok()).unwrap_or_else(|| {
                panic!(
                    "service `{svc}`: max-size {cap:?} must be stated in megabytes. Note \
                     `0` means UNLIMITED to Docker — it is not a cap."
                )
            });
            assert!(
                (1..=100).contains(&mb),
                "service `{svc}`: max-size {mb}m is not a real cap for a log file"
            );
            let files: u64 = logging_opt(&block, "max-file")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| {
                    panic!(
                        "service `{svc}`: max-size without max-file — Docker's default is 1, \
                         so rotation DISCARDS the only segment and the last error is lost \
                         rather than retained."
                    )
                });
            assert!(files >= 2, "service `{svc}`: max-file {files} keeps no history");
        }
    }
}

#[test]
fn one_shot_agent_service_has_no_restart_policy() {
    // The inverse guard. `agent` runs a single TEMPLATE_NAME/AGENT_TASK and exits;
    // a restart policy would loop it forever and re-spend tokens on every exit.
    let block = service_block("agent").expect("service agent missing from docker-compose.yml");
    assert!(
        !declares_restart(&block),
        "service `agent` must NOT declare a `restart:` policy — it is a run-to-completion \
         one-shot, and `unless-stopped` would restart-loop a finished agent forever, \
         re-running its task and re-spending tokens each time."
    );
}

#[test]
fn schedule_defaults_to_daily_cron_not_a_short_interval() {
    // The committed `every 2m` default ran the full Gmail pipeline 31x in one morning
    // (~4.1M tokens against a 10M/day window) with no indication anything was wrong.
    let block = service_block("cos").expect("cos service missing");
    assert!(
        block.contains("TRIGGER_CRON=${TRIGGER_CRON-0 8 * * *}"),
        "cos must default TRIGGER_CRON to the daily 08:00 cron"
    );
    // Structural, not substring: find the TRIGGER_INTERVAL line and assert its default is
    // EMPTY, whatever spelling is used. The previous version matched only the literal
    // "TRIGGER_INTERVAL:-every", so `${TRIGGER_INTERVAL:-2m}` would have passed.
    let interval_line = block
        .lines()
        .map(|l| l.trim())
        .find(|l| l.starts_with("- TRIGGER_INTERVAL="))
        .expect("cos must declare TRIGGER_INTERVAL (empty) so the mode choice is explicit");
    let default_part = interval_line
        .split_once("TRIGGER_INTERVAL-")
        .or_else(|| interval_line.split_once("TRIGGER_INTERVAL:-"))
        .map(|(_, rest)| rest.trim_end_matches('}'))
        .unwrap_or("");
    assert!(
        default_part.is_empty(),
        "TRIGGER_INTERVAL must default to EMPTY — a non-empty default is the 4.1M-token \
         bug (a plain `docker compose up cos` ran the pipeline 31x in one morning). \
         Found default {default_part:?} in: {interval_line}"
    );
    // NOTE: no separate single-dash assertion here. The literal in the first assert above
    // already pins `${TRIGGER_CRON-0 8 * * *}` including its single dash, so a third
    // `contains("TRIGGER_CRON=${TRIGGER_CRON-0 8")` check was a strict SUBSTRING of it and
    // could never fail independently — /review caught that the mutation credited to it was
    // actually caught by assertion 1. A guard that cannot fail alone is not a guard.
}

#[test]
fn service_block_extraction_is_not_vacuous() {
    // Negative control (house rule: a guard that cannot fail is not a guard).
    // If `service_block` silently returned the whole file, every assertion above would
    // pass for the wrong reason — `cos`'s block would appear to contain `agent`'s keys.
    let cos = service_block("cos").expect("cos");
    let agent = service_block("agent").expect("agent");
    assert!(cos.contains("command: cos"), "cos block should hold its own command");
    assert!(
        !cos.contains("TEMPLATE_NAME"),
        "cos block leaked into the agent service — extraction is broken, so the \
         restart-policy assertions above are meaningless"
    );
    assert!(agent.contains("TEMPLATE_NAME"), "agent block should hold its own env");
    assert!(
        !agent.contains("command: cos"),
        "agent block leaked into the cos service"
    );
    assert!(service_block("no-such-service").is_none());
    // Helper negative control: a comment naming the RIGHT policy must not satisfy a block
    // that actually declares the WRONG one. This is the false-green the bare `contains`
    // would have allowed.
    assert_eq!(
        restart_value("    # prefer `restart: unless-stopped` here\n    restart: always\n")
            .as_deref(),
        Some("always"),
        "a comment must never mask the real restart: value"
    );
    assert_eq!(logging_opt("    # max-size: \"999m\"\n    max-size: \"10m\"\n", "max-size").as_deref(), Some("10m"));
    // The flaw /review found: the naive extractor was not anchored to `services:`, so a
    // top-level VOLUME name resolved as if it were a service. `cos-data` is a volume.
    assert!(
        service_block("cos-data").is_none(),
        "service_block must not resolve a top-level volume as a service — that was the \
         latent bug the previous negative control failed to catch"
    );
    // And the last service must not absorb the trailing top-level `volumes:` key.
    let last = service_block("semantic-kb-mcp").expect("semantic-kb-mcp");
    assert!(
        !last.contains("qdrant-data:"),
        "the final service block leaked into the top-level `volumes:` mapping"
    );
}

/// attn.2 R1 (/review): the sidecar must receive `SEMANTIC_DEGRADED` with an EMPTY default.
///
/// This one line is the sole production path into the sidecar's AUTO branch, and AUTO is the
/// only path a key-less deployment takes — the cos entrypoint cannot export into a different
/// container, and Compose cannot express "set this only when that other variable is empty".
/// A `:-0` or `:-1` default would pin the mode and silently re-break the key-less boot back to
/// fail-every-kb_put. Nothing else in the repo covers this file's runtime behaviour: this
/// suite's own header notes no workflow runs `docker compose` at all.
#[test]
fn semantic_kb_passes_degraded_through_with_an_auto_default() {
    let block = service_block("semantic-kb-mcp").expect("semantic-kb-mcp service missing");
    let line = block
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.starts_with('#'))
        .find(|l| l.starts_with("- SEMANTIC_DEGRADED"))
        .expect(
            "semantic-kb-mcp must pass SEMANTIC_DEGRADED through — it is the only production \
             path into the sidecar's AUTO degrade branch",
        );
    assert_eq!(
        line, "- SEMANTIC_DEGRADED=${SEMANTIC_DEGRADED:-}",
        "the default must expand EMPTY so the sidecar resolves AUTO from its own \
         OPENAI_API_KEY; a `:-0`/`:-1` default pins the mode and re-breaks the key-less boot"
    );
}
