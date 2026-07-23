//! Guards the cap.2 CRITICAL /review finding: the CoS orchestrator spawns its
//! inbox/curator children with an explicit `capabilities` set (in the task prompt),
//! and cap.2's `dispatch_spawn` REJECTS the whole spawn if any requested cap is not
//! covered by the parent (`capability_covered_by`). A dev/distro config drift — where
//! the orchestrator's declared caps no longer cover what the prompt tells it to request
//! — would brick the daily brief at the first inbox spawn, and no other test would catch
//! it (the child caps live in prompt TEXT, invisible to `agentd check`).
//!
//! This test reads each config's REAL orchestrator capabilities via the boot loader and
//! asserts every documented child-profile cap is covered. If someone removes a cap from
//! an orchestrator (the exact distro-drift that shipped semantic-kb/mail:raw child caps
//! against a parent that lacked them), this fails.

use agentd::capability::{capability_covered_by, Capability, CredentialProvider};
use agentd::config::Config;
use std::path::Path;

fn mcp(server: &str) -> Capability {
    Capability::Mcp { server: server.to_string(), tools: vec![] }
}
fn kb_read(seg: &str) -> Capability {
    Capability::KbRead { segment: seg.to_string() }
}
fn kb_write(seg: &str) -> Capability {
    Capability::KbWrite { segment: seg.to_string() }
}
fn cred_google() -> Capability {
    Capability::Credential { provider: CredentialProvider::Google }
}

/// Load a cos config (path relative to `agentd/` = CARGO_MANIFEST_DIR) and return the
/// cos-orchestrator's declared capability set.
fn orchestrator_caps(rel_path: &str) -> Vec<Capability> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = root.join(rel_path);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let cfg: Config = toml::from_str(&text)
        .unwrap_or_else(|e| panic!("cannot parse {}: {e}", path.display()));
    let orch = cfg
        .agents
        .iter()
        .find(|a| a.id == "cos-orchestrator")
        .unwrap_or_else(|| panic!("no cos-orchestrator agent in {}", path.display()));
    orch.capabilities
        .clone()
        .unwrap_or_else(|| panic!("cos-orchestrator in {} has no capabilities", path.display()))
}

fn assert_child_covered(parent: &[Capability], child: &[Capability], label: &str) {
    for cap in child {
        assert!(
            capability_covered_by(parent, cap),
            "{label}: child cap {cap:?} is NOT covered by the orchestrator's set — the spawn \
             would be rejected with AgentSpawnDenied and the daily brief would break. \
             Either grant this cap on the orchestrator or drop it from the child profile."
        );
    }
}

#[test]
fn dev_cos_child_profiles_are_subset_of_orchestrator() {
    let parent = orchestrator_caps("cos.agents.toml");
    // Inbox profile (STEP 3 of the orchestrator prompt).
    assert_child_covered(
        &parent,
        &[
            mcp("google_oauth"),
            mcp("semantic-kb"),
            kb_read("mail:raw"),
            kb_write("mail:raw"),
            cred_google(),
        ],
        "dev inbox",
    );
    // Curator profile (STEP 4).
    assert_child_covered(
        &parent,
        &[
            mcp("semantic-kb"),
            kb_read("ops:entities"),
            kb_write("ops:briefs"),
            kb_write("ops:entities"),
        ],
        "dev curator",
    );
    // Acceptance criterion: the curator profile is Gmail-free — google_oauth is NOT in it,
    // so filtered_specs omits every Gmail tool from the curator's flight log. (The parent
    // HOLDS google_oauth, so this is attenuation, not an accident of the parent lacking it.)
    let curator = [
        mcp("semantic-kb"),
        kb_read("ops:entities"),
        kb_write("ops:briefs"),
        kb_write("ops:entities"),
    ];
    assert!(
        !curator.iter().any(|c| matches!(c, Capability::Mcp { server, .. } if server == "google_oauth")),
        "curator profile must not include google_oauth (the acceptance criterion)"
    );
    assert!(
        parent.iter().any(|c| matches!(c, Capability::Mcp { server, .. } if server == "google_oauth")),
        "dev orchestrator should hold google_oauth so the curator's omission is real attenuation"
    );
}

#[test]
fn distro_cos_child_profiles_are_subset_of_orchestrator() {
    let parent = orchestrator_caps("../distro/overlay/etc/agentd/cos.agents.toml");
    // The distro (QEMU prod) runs NO semantic-kb sidecar, so its inbox profile is
    // narrower than dev's — this is the exact drift the /review CRITICAL finding caught.
    assert_child_covered(&parent, &[mcp("google_oauth"), cred_google()], "distro inbox");
    assert_child_covered(
        &parent,
        &[
            kb_read("ops:entities"),
            kb_write("ops:briefs"),
            kb_write("ops:entities"),
        ],
        "distro curator",
    );
    // Guard the drift directly: the distro orchestrator must NOT declare semantic-kb
    // (no sidecar). If a future edit adds it here without adding the server, cap.1's
    // `agentd check` wiring cross-check is the backstop; this documents the expectation.
    assert!(
        !parent.iter().any(|c| matches!(c, Capability::Mcp { server, .. } if server == "semantic-kb")),
        "distro orchestrator unexpectedly declares Mcp{{semantic-kb}} — if the QEMU image now \
         runs the sidecar, update the child profiles + this guard together"
    );
}
