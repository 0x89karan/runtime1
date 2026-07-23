//! Guards the cap.2b CoS topology: the orchestrator is a DE-PRIVILEGED cron trigger, and the
//! Gmail/KB/file-write authority lives on config-declared `[[jobs]]` (the trust root), NOT on
//! the schedule-exposed trigger. Runtime enforcement is `run_job` (config caps, deliver_content
//! =false) + `agentd check`; this test pins the CONFIG so a future edit that re-privileges the
//! trigger (or leaks Gmail into the curator job) fails here rather than shipping a reopened P1-10.
//!
//! Loads each real config via the boot loader and asserts, per config:
//!   - the cos-orchestrator holds ONLY {cron_trigger, RunJob} — no Gmail, Credential, KB,
//!     FsWrite, BriefPublish, or Spawn (it can trigger predeclared work, nothing else);
//!   - the cos-inbox job holds Gmail (`Mcp{google_oauth}`) — the one node that touches email;
//!   - the cos-curator job is Gmail-FREE (no `Mcp{google_oauth}`, no Credential, no Spawn) yet
//!     owns the brief (`FsWrite` + `BriefPublish`).

use agentd::capability::Capability;
use agentd::config::{Config, Job};
use std::path::Path;

fn load(rel_path: &str) -> Config {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_path);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()))
}

fn orchestrator_caps(cfg: &Config) -> Vec<Capability> {
    let orch = cfg.agents.iter().find(|a| a.id == "cos-orchestrator").expect("no cos-orchestrator");
    orch.capabilities.clone().expect("cos-orchestrator has no capabilities")
}

fn job<'a>(cfg: &'a Config, id: &str) -> &'a Job {
    cfg.jobs.iter().find(|j| j.id == id).unwrap_or_else(|| panic!("no [[jobs]] with id '{id}'"))
}

fn has_mcp(caps: &[Capability], server: &str) -> bool {
    caps.iter().any(|c| matches!(c, Capability::Mcp { server: s, .. } if s == server))
}

/// Assert the shared cap.2b invariants for a given config (dev or distro).
fn assert_cos2b_topology(rel_path: &str) {
    let cfg = load(rel_path);
    let orch = orchestrator_caps(&cfg);

    // 1. The trigger is de-privileged: RunJob + cron_trigger, and NOTHING dangerous.
    assert!(
        orch.iter().any(|c| matches!(c, Capability::RunJob)),
        "{rel_path}: cos-orchestrator must hold RunJob (it triggers the sealed jobs)"
    );
    assert!(has_mcp(&orch, "cron_trigger"), "{rel_path}: cos-orchestrator must hold Mcp{{cron_trigger}}");
    for cap in &orch {
        match cap {
            Capability::RunJob | Capability::Mcp { .. } => {} // RunJob + cron_trigger only
            other => panic!(
                "{rel_path}: cos-orchestrator holds {other:?} — the cap.2b trigger must be \
                 de-privileged to only {{cron_trigger, RunJob}}. Gmail/KB/FsWrite/BriefPublish/\
                 Spawn/Credential authority belongs on the [[jobs]], not the schedule-exposed node."
            ),
        }
    }
    assert!(!has_mcp(&orch, "google_oauth"), "{rel_path}: trigger must NOT hold Mcp{{google_oauth}}");

    // 2. The inbox job is the ONE node with Gmail.
    let inbox = &job(&cfg, "cos-inbox").capabilities;
    assert!(has_mcp(inbox, "google_oauth"), "{rel_path}: cos-inbox job must hold Mcp{{google_oauth}}");
    assert!(
        !inbox.iter().any(|c| matches!(c, Capability::Spawn | Capability::RunJob)),
        "{rel_path}: cos-inbox job must not hold Spawn/RunJob (leaf, cannot fan out)"
    );

    // 3. The curator job is Gmail-FREE but owns the brief. This is the acceptance criterion:
    //    an injected curator (it reads the email-derived summary) has NO path to live Gmail.
    let curator = &job(&cfg, "cos-curator").capabilities;
    assert!(
        !has_mcp(curator, "google_oauth"),
        "{rel_path}: cos-curator job must NOT hold Mcp{{google_oauth}} — filtered_specs must show \
         no Gmail tools in the curator's flight log (the P1-10 acceptance criterion)"
    );
    assert!(
        !curator.iter().any(|c| matches!(c, Capability::Credential { .. } | Capability::Spawn | Capability::RunJob)),
        "{rel_path}: cos-curator job must hold no Credential/Spawn/RunJob"
    );
    assert!(
        curator.iter().any(|c| matches!(c, Capability::FsWrite { .. })),
        "{rel_path}: cos-curator job owns the brief — must hold FsWrite"
    );
    assert!(
        curator.iter().any(|c| matches!(c, Capability::BriefPublish)),
        "{rel_path}: cos-curator job must hold BriefPublish (it publishes the brief)"
    );
}

#[test]
fn dev_cos2b_topology() {
    assert_cos2b_topology("cos.agents.toml");
}

#[test]
fn distro_cos2b_topology() {
    assert_cos2b_topology("../distro/overlay/etc/agentd/cos.agents.toml");
    // The QEMU/production config runs no semantic-kb sidecar — neither job may reference it.
    let cfg = load("../distro/overlay/etc/agentd/cos.agents.toml");
    for id in ["cos-inbox", "cos-curator"] {
        assert!(
            !has_mcp(&job(&cfg, id).capabilities, "semantic-kb"),
            "distro job '{id}' references semantic-kb, but the QEMU image runs no such sidecar \
             (it would be an inert/mis-wired grant)"
        );
    }
}
