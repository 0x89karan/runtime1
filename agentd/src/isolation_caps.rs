use surfaces::snapshot::IsolationCapsSummary;

const TIER_FULL:       &str = "full";
const TIER_CAPABILITY: &str = "capability";
const TIER_NONE:       &str = "none";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SECCOMP_ACTIONS_AVAIL: &str = "/proc/sys/kernel/seccomp/actions_avail";

/// Probe what isolation capabilities this device actually has and compute an
/// honest device-level tier ("full" / "capability" / "none").
///
/// - `runsc`: Path to the `runsc` (gVisor) binary, or None when absent.
/// - `landlock`: True when the kernel reports Landlock ABI ≥ 1 (Linux ≥ 5.13).
/// - `seccomp`: True when this build can apply seccomp-bpf rules (x86_64 only).
///   NOTE: this probes the kernel's advertised capability, not process privilege.
///   In containers with `no-new-privileges`, `apply_compiled()` may still return
///   EPERM. Tracked as ma.4-ar-seccomp-probe in TODOS.md.
/// - `arch`: CPU architecture string, e.g. "x86_64" or "aarch64".
/// - Tier: "full" when all three present; "capability" when any one is present;
///   "none" when none are available.
///
/// This function never panics; all detection is fallback-safe.
pub fn probe() -> IsolationCapsSummary {
    let runsc = crate::universal::which_runsc()
        .map(|p| p.display().to_string());

    let landlock = detect_landlock();
    let seccomp  = detect_seccomp();
    let arch     = std::env::consts::ARCH.to_string();

    let tier = classify_tier(runsc.is_some(), landlock, seccomp);

    IsolationCapsSummary {
        runsc,
        landlock,
        seccomp,
        arch,
        tier: tier.to_string(),
    }
}

/// Classify isolation tier from the three capability flags.
/// Extracted for direct unit-testing across all input combinations.
///
/// - "full":       all three present (runsc + landlock + seccomp)
/// - "capability": at least one present
/// - "none":       none present
pub(crate) fn classify_tier(runsc: bool, landlock: bool, seccomp: bool) -> &'static str {
    if runsc && landlock && seccomp {
        TIER_FULL
    } else if runsc || landlock || seccomp {
        TIER_CAPABILITY
    } else {
        TIER_NONE
    }
}

/// Detect Landlock: available on Linux when kernel reports ABI ≥ 1.
/// Always false on non-Linux targets.
fn detect_landlock() -> bool {
    #[cfg(target_os = "linux")]
    {
        sandbox::landlock_available()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Detect seccomp-bpf enforcement capability.
/// Only x86_64 Linux builds of agentd can apply DenySpawn via seccomp-bpf
/// (see sandbox/src/lib.rs). On aarch64 or non-Linux, always false.
///
/// Limitation: reads the host kernel's advertised capability; does not verify
/// process-level privilege. See NOTE in probe() docstring.
fn detect_seccomp() -> bool {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        std::fs::read_to_string(SECCOMP_ACTIONS_AVAIL)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- classify_tier: exhaustive table over all 8 input combinations ---

    #[test]
    fn classify_tier_full_requires_all_three() {
        assert_eq!(classify_tier(true, true, true), "full");
    }

    #[test]
    fn classify_tier_runsc_only_is_capability() {
        // gVisor installed but no kernel sandboxing — still a real isolation capability.
        assert_eq!(classify_tier(true, false, false), "capability");
    }

    #[test]
    fn classify_tier_landlock_only_is_capability() {
        assert_eq!(classify_tier(false, true, false), "capability");
    }

    #[test]
    fn classify_tier_seccomp_only_is_capability() {
        assert_eq!(classify_tier(false, false, true), "capability");
    }

    #[test]
    fn classify_tier_runsc_and_landlock_is_capability() {
        assert_eq!(classify_tier(true, true, false), "capability");
    }

    #[test]
    fn classify_tier_runsc_and_seccomp_is_capability() {
        assert_eq!(classify_tier(true, false, true), "capability");
    }

    #[test]
    fn classify_tier_landlock_and_seccomp_is_capability() {
        assert_eq!(classify_tier(false, true, true), "capability");
    }

    #[test]
    fn classify_tier_none_when_all_absent() {
        assert_eq!(classify_tier(false, false, false), "none");
    }

    // --- probe(): live detection, no fixed inputs ---

    #[test]
    fn probe_returns_valid_tier() {
        let caps = probe();
        assert!(
            matches!(caps.tier.as_str(), "full" | "capability" | "none"),
            "tier must be full, capability, or none; got: {}",
            caps.tier
        );
    }

    #[test]
    fn probe_arch_is_non_empty() {
        let caps = probe();
        assert!(!caps.arch.is_empty(), "arch must not be empty");
    }

    #[test]
    fn probe_seccomp_false_on_non_x86_64() {
        let result = detect_seccomp();
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        assert!(!result, "seccomp must be false on non-x86_64 or non-Linux");
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let _ = result; // live probe result; just ensure no panic
    }

    #[test]
    fn probe_no_panic_on_missing_proc_file() {
        let _ = detect_seccomp();
        let _ = detect_landlock();
    }

    // --- IsolationCapsSummary serialization ---

    #[test]
    fn isolation_caps_summary_serializes_correctly() {
        let caps = IsolationCapsSummary {
            runsc:    Some("/usr/bin/runsc".to_string()),
            landlock: true,
            seccomp:  false,
            arch:     "aarch64".to_string(),
            tier:     "capability".to_string(),
        };
        let json = serde_json::to_string(&caps).unwrap();
        assert!(json.contains("\"runsc\":\"/usr/bin/runsc\""));
        assert!(json.contains("\"landlock\":true"));
        assert!(json.contains("\"seccomp\":false"));
        assert!(json.contains("\"arch\":\"aarch64\""));
        assert!(json.contains("\"tier\":\"capability\""));
    }

    #[test]
    fn isolation_caps_summary_null_runsc_serializes() {
        let caps = IsolationCapsSummary {
            runsc:    None,
            landlock: false,
            seccomp:  false,
            arch:     "x86_64".to_string(),
            tier:     "none".to_string(),
        };
        let json = serde_json::to_string(&caps).unwrap();
        assert!(json.contains("\"runsc\":null"));
        assert!(json.contains("\"tier\":\"none\""));
    }
}
