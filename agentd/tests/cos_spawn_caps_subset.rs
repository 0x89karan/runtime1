//! Guards the cap.2b CoS topology: the orchestrator is a DE-PRIVILEGED cron trigger, and the
//! Gmail/KB/file-write authority lives on config-declared `[[jobs]]` (the trust root), NOT on
//! the schedule-exposed trigger. Runtime enforcement is `run_job` (config caps, deliver_content
//! =false) + `agentd check`; this test pins the CONFIG so a future edit that re-privileges the
//! trigger (or leaks Gmail into the curator job) fails here rather than shipping a reopened P1-10.
//!
//! Loads each real config via the boot loader and asserts, per config:
//!   - the cos-inbox job holds Gmail (`Mcp{google_oauth}`) — the one node that touches email;
//!   - the cos-curator job is Gmail-FREE (no `Mcp{google_oauth}`, no Credential, no Spawn) yet
//!     owns the brief (`FsWrite` + `BriefPublish`).
//!
//! The two configs now diverge on WHO holds the clock, so the trigger invariant is asserted
//! per-config rather than shared (attn.4 T7, 2026-08-08):
//!   - **dev/docker** (`cos.agents.toml`) declares ZERO `[[agents]]`. The scheduler fires the
//!     jobs natively from their `schedule =` keys. The invariant is therefore STRONGER, not
//!     weaker: NO agent may hold `RunJob` at all, so nothing but the scheduler can fire a job.
//!     Re-adding an LLM trigger here would resurrect the cos-curator double-fire that T7 closed
//!     (the native path fires curator on a wall clock; the legacy path fired it when inbox
//!     COMPLETED, and `reject_if_job_already_running` only refuses while a run is LIVE).
//!   - **distro/QEMU** carries no per-job `schedule =`, so it still needs the LLM trigger, and
//!     the original cap.2b assertion still applies there: cos-orchestrator holds ONLY
//!     {cron_trigger, RunJob} — no Gmail, Credential, KB, FsWrite, BriefPublish, or Spawn.

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

/// Assert the cap.2b DE-PRIVILEGED-TRIGGER invariant. Only applies to configs that still have
/// an LLM trigger — i.e. distro/QEMU, which carries no per-job `schedule =` (attn.4 T7).
fn assert_llm_trigger_is_deprivileged(rel_path: &str) {
    let cfg = load(rel_path);
    let orch = orchestrator_caps(&cfg);

    // The trigger is de-privileged: RunJob + cron_trigger, and NOTHING dangerous.
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
}

/// Assert that NOTHING but the scheduler can fire a job (attn.4 T7). The dev/docker config
/// fires `[[jobs]]` natively from their `schedule =` keys, so an agent holding `RunJob` is not
/// merely redundant — it reintroduces the cos-curator DOUBLE-FIRE this increment closed.
fn assert_no_agent_can_fire_jobs(rel_path: &str) {
    let cfg = load(rel_path);
    for agent in &cfg.agents {
        let caps = agent.capabilities.clone().unwrap_or_default();
        assert!(
            !caps.iter().any(|c| matches!(c, Capability::RunJob)),
            "{rel_path}: agent '{}' holds RunJob, but this config fires [[jobs]] natively from \
             their `schedule =` keys. Two live dispatch paths double-fire cos-curator: the \
             native tick fires it on a wall clock while an LLM trigger fires it when cos-inbox \
             COMPLETES, and `reject_if_job_already_running` only refuses while a run is LIVE — \
             the two do not overlap, so nothing stops the second. Either delete the trigger \
             (attn.4 T7) or remove the per-job `schedule =` keys, not both.",
            agent.id
        );
    }
}

/// Assert the shared cap.2b JOB invariants (identical across both configs).
fn assert_cos2b_topology(rel_path: &str) {
    let cfg = load(rel_path);

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

/// attn.4 T7: the dev/docker config has no LLM trigger at all — the scheduler owns the clock.
/// Pins BOTH halves: zero agents declared, and (independently) no agent may hold RunJob, so
/// this keeps guarding even if an agent is added back for some unrelated reason.
#[test]
fn dev_config_has_no_llm_trigger() {
    let cfg = load("cos.agents.toml");
    // RunJob check FIRST, deliberately. It carries the specific double-fire explanation, and
    // ordering it after `is_empty` would let that broader assertion shadow it — which would
    // make the RunJob guard un-mutation-testable through this test (it can never be the one
    // that fires). Both are proven independently only in this order.
    assert_no_agent_can_fire_jobs("cos.agents.toml");
    assert!(
        cfg.agents.is_empty(),
        "cos.agents.toml must declare zero [[agents]] — the cos-orchestrator was deleted in \
         attn.4 T7 because it existed only to poll a clock (~3456 turns/day). Found: {:?}",
        cfg.agents.iter().map(|a| &a.id).collect::<Vec<_>>()
    );
    // The native path must actually be live, or deleting the trigger leaves nothing firing.
    assert!(
        !cfg.scheduler.native_cron_shadow,
        "cos.agents.toml has no LLM trigger, so native_cron_shadow MUST be false — otherwise \
         the schedule is computed and logged but never dispatched, and no brief is ever produced."
    );
    for id in ["cos-inbox", "cos-curator"] {
        assert!(
            job(&cfg, id).schedule.is_some(),
            "job '{id}' must carry a `schedule =` key — with the LLM trigger deleted it is the \
             only thing that can fire it"
        );
    }
}

#[test]
fn distro_cos2b_topology() {
    assert_cos2b_topology("../distro/overlay/etc/agentd/cos.agents.toml");
    // Distro/QEMU has no per-job `schedule =`, so it still relies on the LLM trigger and the
    // original cap.2b de-privilege invariant still applies there.
    assert_llm_trigger_is_deprivileged("../distro/overlay/etc/agentd/cos.agents.toml");
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
