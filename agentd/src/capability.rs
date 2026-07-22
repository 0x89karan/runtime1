use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Credential provider selector for `Capability::Credential`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum CredentialProvider {
    Google,
    BraveSearch,
    /// Custom provider — must match a `[credential_gateway.providers.<name>]` TOML entry.
    Custom(String),
}

/// A permission an agent may be granted.
///
/// `None` cap-set = unrestricted (backward compat).
/// `Some([])` = deny all.
///
/// Prefix semantics for `FsRead`/`FsWrite`:
///   - In a *granted* capability: the directory root the agent may access.
///   - In a *required* capability (returned by `required_capability_for`): the
///     actual path being accessed at invocation time.
///
/// `satisfies` tests `normalize(actual).starts_with(normalize(granted_prefix))`.
///
/// **Absolute paths assumed.** Relative paths fail-safe to deny (no prefix match
/// since `normalize` does not resolve relative roots). Callers should pass
/// absolute paths; `~` expansion is not performed.
///
/// **Case-sensitive.** `starts_with` is byte-exact. On case-insensitive
/// filesystems (macOS HFS+) a grant of `/Workspace` will not match
/// `/workspace/file`. Production target is Linux ext4/btrfs (case-sensitive),
/// so this is a dev-environment edge case, not a security gap.
///
/// **Symlinks not resolved.** A symlink inside a granted prefix can point
/// outside it. OS-level isolation (Phase 4 sandbox) is the correct enforcement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Capability {
    FsRead { prefix: String },
    FsWrite { prefix: String },
    /// Network access grant. `hosts` is advisory (not kernel-enforced at the
    /// capability layer). `ports`, when non-empty, drives Landlock V4 TCP port
    /// enforcement via `caps_to_rules()` → `AllowNetConnect` sandbox rules.
    /// Empty `ports` means no port restriction (all outgoing TCP allowed, same as
    /// pre-p4.6 behaviour). Configs without a `ports` field deserialise as empty.
    Net {
        hosts: Vec<String>,
        #[serde(default)]
        ports: Vec<u16>,
    },
    Mcp { server: String, tools: Vec<String> },
    /// Grants permission to spawn child agents via `spawn_agent`. Enforced by the scheduler.
    Spawn,
    /// Read access to a memory namespace segment (prefix-match on namespace).
    /// `KbRead { segment: "agent:scratch" }` grants read access to all keys
    /// whose namespace equals or starts with `"agent:scratch"` followed by a
    /// segment delimiter (`:` or `/`) or end-of-string.
    KbRead { segment: String },
    /// Write access to a memory namespace segment (same prefix semantics as KbRead).
    KbWrite { segment: String },
    /// Subprocess sandbox capability: when listed in an MCP server's `capabilities`,
    /// suppresses `DenySpawn` so the server subprocess can fork/exec shell commands.
    /// Identical to `Spawn` in `caps_to_rules_inner`; distinct so config is self-documenting.
    /// Not used for agent-level gating — use `Mcp { server = "shell_exec" }` for that.
    ShellExec,
    /// Grants an agent (or MCP server) access to a specific credential provider
    /// via the credential broker. Audited; enforcement (deny-without-cap) deferred to cred.4+.
    Credential { provider: CredentialProvider },
    /// Read access to durable run history via `runs_query` (ux.11b). Unit grant —
    /// run history is a single dataset (errors/spend/parent), so it is NOT modeled
    /// as a KB segment (KbRead would be too loose).
    RunsRead,
}

/// Normalize a path by resolving `.` and `..` components without filesystem
/// access. Relative paths are kept relative (a leading `..` is preserved).
/// Enforcement assumes absolute paths; relative paths fail-safe to deny.
pub fn normalize_path(p: &Path) -> PathBuf {
    let mut components: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a Normal component — preserve leading `..` on relative paths.
                match components.last() {
                    Some(Component::Normal(_)) => {
                        components.pop();
                    }
                    _ => components.push(c),
                }
            }
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Type-level capability check used by `ToolRegistry::filtered_specs`.
///
/// Unlike `satisfies`, this function answers "could this tool EVER be invoked?"
/// rather than "can this specific invocation proceed?". For path-based capabilities,
/// an empty `prefix` in `required` is treated as a wildcard meaning "any path of this
/// type" — so `FsRead { prefix: "" }` matches any granted `FsRead` capability.
/// Same semantics for `KbRead`/`KbWrite { segment: "" }`.
/// For `Mcp`, the full server+tools check still applies (the tool name is static).
pub fn satisfies_type(granted: &[Capability], required: &Capability) -> bool {
    match required {
        Capability::FsRead { prefix } if prefix.is_empty() => {
            granted.iter().any(|g| matches!(g, Capability::FsRead { .. }))
        }
        Capability::FsWrite { prefix } if prefix.is_empty() => {
            granted.iter().any(|g| matches!(g, Capability::FsWrite { .. }))
        }
        Capability::KbRead { segment } if segment.is_empty() => {
            granted.iter().any(|g| matches!(g, Capability::KbRead { .. }))
        }
        Capability::KbWrite { segment } if segment.is_empty() => {
            granted.iter().any(|g| matches!(g, Capability::KbWrite { .. }))
        }
        // Empty Custom string → type-level check: "has any Credential cap?"
        Capability::Credential { provider: CredentialProvider::Custom(s) } if s.is_empty() => {
            granted.iter().any(|g| matches!(g, Capability::Credential { .. }))
        }
        other => satisfies(granted, other),
    }
}

/// Returns `true` if `required` is covered by at least one entry in `granted`.
///
/// - `FsRead`/`FsWrite`: normalize both sides, then `starts_with`.
/// - `Mcp`: server names must match and every required tool must appear in the
///   granted tools list. An empty granted tools list (`[]`) grants all tools on
///   that server (wildcard).
/// - `Net`: advisory at this layer — always `true`. Kernel-level TCP port
///   enforcement is handled by `caps_to_rules()` → `AllowNetConnect` (p4.6+).
/// - `Spawn`: `true` if any granted cap is `Capability::Spawn`; required by `spawn_agent`.
pub fn satisfies(granted: &[Capability], required: &Capability) -> bool {
    match required {
        Capability::FsRead { prefix: req_path } => {
            let norm_req = normalize_path(Path::new(req_path));
            granted.iter().any(|g| {
                if let Capability::FsRead { prefix: g_prefix } = g {
                    let norm_granted = normalize_path(Path::new(g_prefix));
                    // An empty (non-absolute) granted prefix is not a valid grant —
                    // it would match every path via starts_with semantics. Fail-safe to deny.
                    !norm_granted.as_os_str().is_empty() && norm_req.starts_with(&norm_granted)
                } else {
                    false
                }
            })
        }
        Capability::FsWrite { prefix: req_path } => {
            let norm_req = normalize_path(Path::new(req_path));
            granted.iter().any(|g| {
                if let Capability::FsWrite { prefix: g_prefix } = g {
                    let norm_granted = normalize_path(Path::new(g_prefix));
                    // An empty (non-absolute) granted prefix is not a valid grant — fail-safe.
                    !norm_granted.as_os_str().is_empty() && norm_req.starts_with(&norm_granted)
                } else {
                    false
                }
            })
        }
        Capability::Net { .. } => true,
        Capability::Mcp {
            server: req_server,
            tools: req_tools,
        } => granted.iter().any(|g| {
            if let Capability::Mcp {
                server: g_server,
                tools: g_tools,
            } = g
            {
                if g_server != req_server {
                    return false;
                }
                // Empty granted tools = wildcard (grants all tools on the server).
                g_tools.is_empty() || req_tools.iter().all(|t| g_tools.contains(t))
            } else {
                false
            }
        }),
        Capability::Spawn => granted.iter().any(|g| matches!(g, Capability::Spawn)),
        Capability::KbRead { segment: req_seg } => {
            if req_seg.is_empty() {
                return false; // empty segment is never a valid requirement
            }
            granted.iter().any(|g| {
                if let Capability::KbRead { segment: g_seg } = g {
                    kb_segment_satisfies(g_seg, req_seg)
                } else {
                    false
                }
            })
        }
        Capability::KbWrite { segment: req_seg } => {
            if req_seg.is_empty() {
                return false; // empty segment is never a valid requirement
            }
            granted.iter().any(|g| {
                if let Capability::KbWrite { segment: g_seg } = g {
                    kb_segment_satisfies(g_seg, req_seg)
                } else {
                    false
                }
            })
        }
        Capability::ShellExec => granted.iter().any(|g| matches!(g, Capability::ShellExec)),
        Capability::Credential { provider: req_prov } => granted.iter().any(|g| {
            matches!(g, Capability::Credential { provider: gp } if gp == req_prov)
        }),
        Capability::RunsRead => granted.iter().any(|g| matches!(g, Capability::RunsRead)),
    }
}

/// Return true if `granted_prefix` covers `required_segment`.
///
/// An empty granted segment is NEVER a valid grant (fail-safe to deny, mirrors
/// the FsRead empty-prefix guard). Otherwise, `required` must equal `granted`
/// or start with `granted` followed by a segment delimiter (`:` or `/`).
/// This prevents `"agent:scratch"` from matching `"agent:scratchpad"`.
pub fn kb_segment_satisfies(granted: &str, required: &str) -> bool {
    if granted.is_empty() {
        return false; // empty grant is not valid
    }
    if required == granted {
        return true;
    }
    // required must start with granted + a delimiter (':' or '/')
    if let Some(rest) = required.strip_prefix(granted) {
        rest.starts_with(':') || rest.starts_with('/')
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_removes_dotdot() {
        assert_eq!(
            normalize_path(Path::new("/workspace/../etc")),
            PathBuf::from("/etc")
        );
    }

    #[test]
    fn normalize_path_removes_dot() {
        assert_eq!(
            normalize_path(Path::new("/workspace/./src")),
            PathBuf::from("/workspace/src")
        );
    }

    #[test]
    fn normalize_path_chained_dotdot() {
        assert_eq!(
            normalize_path(Path::new("/workspace/src/../lib")),
            PathBuf::from("/workspace/lib")
        );
    }

    #[test]
    fn normalize_path_relative_preserved() {
        // Leading `..` on a relative path is kept — enforcement will fail-safe to deny.
        assert_eq!(
            normalize_path(Path::new("../etc")),
            PathBuf::from("../etc")
        );
    }

    #[test]
    fn satisfies_fs_read_subpath_ok() {
        let caps = vec![Capability::FsRead {
            prefix: "/workspace".to_string(),
        }];
        assert!(satisfies(
            &caps,
            &Capability::FsRead {
                prefix: "/workspace/src/main.rs".to_string()
            }
        ));
    }

    #[test]
    fn satisfies_fs_read_exact_prefix_ok() {
        let caps = vec![Capability::FsRead {
            prefix: "/workspace".to_string(),
        }];
        assert!(satisfies(
            &caps,
            &Capability::FsRead {
                prefix: "/workspace".to_string()
            }
        ));
    }

    #[test]
    fn satisfies_fs_read_outside_prefix_denied() {
        let caps = vec![Capability::FsRead {
            prefix: "/workspace".to_string(),
        }];
        assert!(!satisfies(
            &caps,
            &Capability::FsRead {
                prefix: "/etc/passwd".to_string()
            }
        ));
    }

    #[test]
    fn satisfies_fs_read_traversal_denied() {
        // The critical traversal test: `..` in the requested path must not escape
        // the granted prefix after normalization.
        let caps = vec![Capability::FsRead {
            prefix: "/workspace".to_string(),
        }];
        assert!(!satisfies(
            &caps,
            &Capability::FsRead {
                prefix: "/workspace/../etc/passwd".to_string()
            }
        ));
    }

    #[test]
    fn satisfies_empty_cap_set_denies_all() {
        assert!(!satisfies(
            &[],
            &Capability::FsRead {
                prefix: "/workspace/x".to_string()
            }
        ));
    }

    #[test]
    fn satisfies_empty_granted_prefix_denies_all_paths() {
        // An empty-prefix granted capability must NOT match any path.
        // Without this guard, Path::starts_with("") returns true for all paths.
        let caps = vec![Capability::FsRead { prefix: "".to_string() }];
        assert!(!satisfies(&caps, &Capability::FsRead { prefix: "/etc/passwd".to_string() }));
        assert!(!satisfies(&caps, &Capability::FsRead { prefix: "/workspace/x".to_string() }));
        let caps_write = vec![Capability::FsWrite { prefix: "".to_string() }];
        assert!(!satisfies(&caps_write, &Capability::FsWrite { prefix: "/tmp/x".to_string() }));
    }

    #[test]
    fn satisfies_mcp_wildcard_tools() {
        let caps = vec![Capability::Mcp {
            server: "echo".to_string(),
            tools: vec![],
        }];
        assert!(satisfies(
            &caps,
            &Capability::Mcp {
                server: "echo".to_string(),
                tools: vec!["any_tool".to_string()]
            }
        ));
    }

    #[test]
    fn satisfies_mcp_explicit_tools_subset_ok() {
        let caps = vec![Capability::Mcp {
            server: "echo".to_string(),
            tools: vec!["echo_text".to_string(), "echo_json".to_string()],
        }];
        assert!(satisfies(
            &caps,
            &Capability::Mcp {
                server: "echo".to_string(),
                tools: vec!["echo_text".to_string()]
            }
        ));
    }

    #[test]
    fn satisfies_mcp_tool_not_in_granted_denied() {
        let caps = vec![Capability::Mcp {
            server: "echo".to_string(),
            tools: vec!["echo_text".to_string()],
        }];
        assert!(!satisfies(
            &caps,
            &Capability::Mcp {
                server: "echo".to_string(),
                tools: vec!["other_tool".to_string()]
            }
        ));
    }

    #[test]
    fn satisfies_mcp_wrong_server_denied() {
        let caps = vec![Capability::Mcp {
            server: "echo".to_string(),
            tools: vec![],
        }];
        assert!(!satisfies(
            &caps,
            &Capability::Mcp {
                server: "other".to_string(),
                tools: vec!["any".to_string()]
            }
        ));
    }

    #[test]
    fn satisfies_spawn_requires_spawn_grant() {
        // Granted Spawn → allowed.
        let caps = vec![Capability::Spawn];
        assert!(satisfies(&caps, &Capability::Spawn));
        // Empty cap-set → denied.
        assert!(!satisfies(&[], &Capability::Spawn));
        // Only non-Spawn caps → denied.
        let other = vec![Capability::FsRead { prefix: "/workspace".to_string() }];
        assert!(!satisfies(&other, &Capability::Spawn));
    }

    #[test]
    fn satisfies_net_advisory_always_true() {
        assert!(satisfies(
            &[],
            &Capability::Net {
                hosts: vec!["api.example.com".to_string()],
                ports: vec![],
            }
        ));
    }

    #[test]
    fn net_ports_field_defaults_empty_on_deserialize() {
        // Existing TOML without `ports` must deserialise cleanly (backward compat).
        let toml_str = r#"Net = { hosts = ["api.example.com"] }"#;
        let cap: Capability = toml::from_str(toml_str).expect("deserialise Net without ports");
        if let Capability::Net { ports, .. } = cap {
            assert!(ports.is_empty(), "ports should default to empty");
        } else {
            panic!("expected Net capability");
        }
    }

    #[test]
    fn net_ports_parses_when_present() {
        let toml_str = r#"Net = { hosts = [], ports = [443, 80] }"#;
        let cap: Capability = toml::from_str(toml_str).expect("deserialise Net with ports");
        if let Capability::Net { ports, .. } = cap {
            assert_eq!(ports, vec![443u16, 80]);
        } else {
            panic!("expected Net capability");
        }
    }

    // ── satisfies_type direct tests ──────────────────────────────────────────

    #[test]
    fn satisfies_type_fs_read_empty_prefix_matches_any_fs_read_grant() {
        // satisfies_type is the type-level visibility check used by filtered_specs.
        // Required with empty prefix = "has any FsRead cap?"
        let caps = [Capability::FsRead { prefix: "/workspace".to_string() }];
        assert!(satisfies_type(&caps, &Capability::FsRead { prefix: "".to_string() }));
    }

    #[test]
    fn satisfies_type_fs_write_empty_prefix_matches_any_fs_write_grant() {
        let caps = [Capability::FsWrite { prefix: "/tmp".to_string() }];
        assert!(satisfies_type(&caps, &Capability::FsWrite { prefix: "".to_string() }));
    }

    #[test]
    fn satisfies_type_fs_read_empty_prefix_no_grant_returns_false() {
        // No FsRead in the cap set → type check returns false.
        let caps = [Capability::FsWrite { prefix: "/tmp".to_string() }];
        assert!(!satisfies_type(&caps, &Capability::FsRead { prefix: "".to_string() }));
    }

    // ── KbRead / KbWrite prefix-match tests ─────────────────────────────────

    #[test]
    fn kb_read_prefix_match_exact_ok() {
        let caps = vec![Capability::KbRead { segment: "agent:scratch".to_string() }];
        assert!(satisfies(&caps, &Capability::KbRead { segment: "agent:scratch".to_string() }));
    }

    #[test]
    fn kb_read_prefix_match_sub_segment_ok() {
        let caps = vec![Capability::KbRead { segment: "agent".to_string() }];
        // "agent:scratch" starts with "agent:" — colon delimiter, allowed.
        assert!(satisfies(&caps, &Capability::KbRead { segment: "agent:scratch".to_string() }));
    }

    #[test]
    fn kb_write_empty_segment_denies() {
        let caps = vec![Capability::KbWrite { segment: "".to_string() }];
        assert!(
            !satisfies(&caps, &Capability::KbWrite { segment: "agent:scratch".to_string() }),
            "empty-segment grant must deny all"
        );
    }

    #[test]
    fn kb_read_boundary_check_prevents_prefix_squatting() {
        // "agent:scratch" must NOT match a grant of "agent:scratch" to access
        // "agent:scratchpad" — the boundary check must catch this.
        let caps = vec![Capability::KbRead { segment: "agent:scratch".to_string() }];
        assert!(
            !satisfies(&caps, &Capability::KbRead { segment: "agent:scratchpad".to_string() }),
            "granted 'agent:scratch' must not match 'agent:scratchpad'"
        );
    }

    #[test]
    fn kb_write_boundary_slash_delimiter_ok() {
        let caps = vec![Capability::KbWrite { segment: "agent".to_string() }];
        // "agent/sub" starts with "agent/" — slash delimiter, allowed.
        assert!(satisfies(&caps, &Capability::KbWrite { segment: "agent/sub".to_string() }));
    }

    #[test]
    fn kb_required_empty_segment_always_denied() {
        let caps = vec![Capability::KbRead { segment: "agent:scratch".to_string() }];
        // An empty required segment is a programming error — always denied.
        assert!(!satisfies(&caps, &Capability::KbRead { segment: "".to_string() }));
    }

    #[test]
    fn satisfies_type_kb_read_empty_segment_matches_any_kb_read_grant() {
        let caps = [Capability::KbRead { segment: "agent:scratch".to_string() }];
        assert!(satisfies_type(&caps, &Capability::KbRead { segment: "".to_string() }));
    }

    #[test]
    fn satisfies_type_kb_write_empty_segment_matches_any_kb_write_grant() {
        let caps = [Capability::KbWrite { segment: "agent:scratch".to_string() }];
        assert!(satisfies_type(&caps, &Capability::KbWrite { segment: "".to_string() }));
    }

    #[test]
    fn satisfies_type_kb_read_empty_no_grant_returns_false() {
        let caps = [Capability::KbWrite { segment: "agent:scratch".to_string() }];
        assert!(!satisfies_type(&caps, &Capability::KbRead { segment: "".to_string() }));
    }

    #[test]
    fn satisfies_type_non_fs_delegates_to_satisfies() {
        // For non-empty prefixes, satisfies_type delegates to satisfies.
        let caps = [Capability::FsRead { prefix: "/workspace".to_string() }];
        // Matching non-empty prefix → true
        assert!(satisfies_type(&caps, &Capability::FsRead { prefix: "/workspace/file.rs".to_string() }));
        // Non-matching non-empty prefix → false
        assert!(!satisfies_type(&caps, &Capability::FsRead { prefix: "/etc/passwd".to_string() }));
    }

    // ── Gap ②: empty cap-set denies KbRead explicitly ────────────────────────

    #[test]
    fn satisfies_empty_cap_set_denies_kb_read() {
        assert!(!satisfies(&[], &Capability::KbRead { segment: "agent:scratch".to_string() }));
    }

    // ── Gap ③: cross-type KbRead/KbWrite isolation ───────────────────────────

    #[test]
    fn kb_write_grant_does_not_satisfy_kb_read_requirement() {
        let caps = vec![Capability::KbWrite { segment: "agent:scratch".to_string() }];
        assert!(
            !satisfies(&caps, &Capability::KbRead { segment: "agent:scratch".to_string() }),
            "KbWrite grant must not satisfy KbRead requirement"
        );
    }

    #[test]
    fn kb_read_grant_does_not_satisfy_kb_write_requirement() {
        let caps = vec![Capability::KbRead { segment: "agent:scratch".to_string() }];
        assert!(
            !satisfies(&caps, &Capability::KbWrite { segment: "agent:scratch".to_string() }),
            "KbRead grant must not satisfy KbWrite requirement"
        );
    }

    // ── ShellExec capability tests ───────────────────────────────────────────

    #[test]
    fn satisfies_shell_exec_requires_shell_exec_grant() {
        let caps = vec![Capability::ShellExec];
        assert!(satisfies(&caps, &Capability::ShellExec));
    }

    #[test]
    fn satisfies_shell_exec_denied_without_grant() {
        assert!(!satisfies(&[], &Capability::ShellExec));
        let other = vec![Capability::Spawn];
        assert!(!satisfies(&other, &Capability::ShellExec), "Spawn must not satisfy ShellExec");
    }

    #[test]
    fn shell_exec_deserializes_from_toml() {
        // Confirm unit variant round-trip. toml requires a table root, so wrap in a struct.
        // Unit variants with #[serde(rename_all = "PascalCase")] serialize as string "ShellExec".
        let toml_str = r#"capabilities = ["ShellExec"]"#;
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Wrapper { capabilities: Vec<Capability> }
        let w: Wrapper = toml::from_str(toml_str).expect("ShellExec must deserialize from 'capabilities = [\"ShellExec\"]'");
        assert_eq!(w.capabilities[0], Capability::ShellExec);
        // Serialize the wrapper back and confirm the round-trip produces "ShellExec".
        let ser = toml::to_string(&w).expect("Wrapper with ShellExec must serialize");
        assert!(ser.contains("ShellExec"), "serialized TOML must contain 'ShellExec', got: {ser}");
        // Re-parse to confirm full round-trip.
        let w2: Wrapper = toml::from_str(&ser).expect("re-parse must succeed");
        assert_eq!(w2, w, "round-trip must be identical");
    }
}
