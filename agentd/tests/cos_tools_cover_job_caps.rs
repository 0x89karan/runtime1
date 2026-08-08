//! Pins `[tools] native` against what the `[[jobs]]` actually need.
//!
//! **Why this exists.** Native tools are registered from the `[tools] native` LIST
//! (`register_native` in `tools/native.rs`), NOT derived from declared capabilities. So a
//! capability and its tool can drift apart in either direction, and nothing at boot complains:
//! a job can hold `BriefPublish` while `publish_brief` is unregistered, and the job will run,
//! call nothing, and produce no brief — with no error anywhere.
//!
//! That is not hypothetical. During attn.4 T7 (2026-08-08) a line-range delete that removed the
//! `cos-orchestrator` block also took out the adjacent `[tools]` section. Every existing test
//! stayed green — `agentctl jobs` validates schedules only, and the cap.2b topology test reads
//! capabilities, never the tool list — and the live boot afterwards registered 6 tools instead
//! of 12, leaving `cos-curator` unable to write or publish the brief. This test is the guard
//! that was missing.
//!
//! The mapping is capability -> the tool that capability is useless without.

use agentd::capability::Capability;
use agentd::config::Config;
use std::path::Path;

fn load(rel_path: &str) -> Config {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_path);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()))
}

/// The native tool each capability is inert without. `Mcp`/`Net`/`Credential` are served by
/// stdio sidecars rather than the native registry, so they have no entry here.
fn required_tool(cap: &Capability) -> Option<&'static str> {
    match cap {
        Capability::KbRead { .. } => Some("kb_get"),
        Capability::KbWrite { .. } => Some("kb_put"),
        Capability::FsWrite { .. } => Some("write_file"),
        Capability::BriefPublish => Some("publish_brief"),
        Capability::RunsRead => Some("runs_query"),
        Capability::RunJob => Some("run_job"),
        _ => None,
    }
}

#[test]
fn dev_native_tools_cover_every_job_capability() {
    let rel = "cos.agents.toml";
    let cfg = load(rel);
    let native = &cfg.tools.native;
    let has = |t: &str| native.iter().any(|n| n == t || n == "all");

    assert!(
        !native.is_empty(),
        "{rel}: [tools] native is empty or missing — every native tool is unregistered and the \
         jobs will run but do nothing. This is exactly how attn.4 T7 first shipped."
    );

    for job in &cfg.jobs {
        for cap in &job.capabilities {
            if let Some(tool) = required_tool(cap) {
                assert!(
                    has(tool),
                    "{rel}: job '{}' declares {cap:?}, but '{tool}' is not in [tools] native. \
                     The capability is granted and the tool does not exist, so the job runs, \
                     calls nothing, and fails silently — no error, no brief.",
                    job.id
                );
            }
        }
    }
}

/// The curator owns the brief. If either of its two output tools is missing, the pipeline
/// completes and produces nothing — the failure this whole file exists to make loud.
#[test]
fn curator_can_actually_emit_a_brief() {
    let rel = "cos.agents.toml";
    let cfg = load(rel);
    let native = &cfg.tools.native;
    for tool in ["write_file", "publish_brief"] {
        assert!(
            native.iter().any(|n| n == tool || n == "all"),
            "{rel}: '{tool}' missing from [tools] native — cos-curator holds the capability but \
             cannot emit the brief. The run would look successful and produce no output."
        );
    }
}

/// attn.4 T7: nothing may fire a job except the scheduler. `run_job` registered here would not
/// itself break that (capability checks still gate the call, and this config declares no
/// agents), but its presence means an agent added later only needs the capability, not a
/// config change — so keep the tool absent as defence in depth alongside the RunJob capability
/// assertion in `cos_spawn_caps_subset.rs`.
#[test]
fn dev_does_not_register_run_job() {
    let rel = "cos.agents.toml";
    let cfg = load(rel);
    assert!(
        !cfg.tools.native.iter().any(|n| n == "run_job" || n == "all"),
        "{rel}: 'run_job' is registered, but attn.4 T7 made the scheduler the only thing that \
         fires [[jobs]]. Re-adding it re-opens the cos-curator double-fire path the moment any \
         agent is declared with RunJob."
    );
}
