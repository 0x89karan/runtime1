use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

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
/// Symlinks are NOT resolved — a symlink inside a granted prefix can point
/// outside it. OS-level isolation (Phase 4 sandbox) is the correct fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Capability {
    FsRead { prefix: String },
    FsWrite { prefix: String },
    /// Advisory — recorded but not enforced at p1.4. Phase 4 network namespace
    /// handles real enforcement. `hosts` is reserved for future use.
    Net { hosts: Vec<String> },
    Mcp { server: String, tools: Vec<String> },
    /// Reserved — no tool declares Spawn yet; always denied.
    Spawn,
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
/// For `Mcp`, the full server+tools check still applies (the tool name is static).
pub fn satisfies_type(granted: &[Capability], required: &Capability) -> bool {
    match required {
        Capability::FsRead { prefix } if prefix.is_empty() => {
            granted.iter().any(|g| matches!(g, Capability::FsRead { .. }))
        }
        Capability::FsWrite { prefix } if prefix.is_empty() => {
            granted.iter().any(|g| matches!(g, Capability::FsWrite { .. }))
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
/// - `Net`: advisory — always `true` (real enforcement is Phase 4).
/// - `Spawn`: always `false` — no tool declares this capability yet.
pub fn satisfies(granted: &[Capability], required: &Capability) -> bool {
    match required {
        Capability::FsRead { prefix: req_path } => {
            let norm_req = normalize_path(Path::new(req_path));
            granted.iter().any(|g| {
                if let Capability::FsRead { prefix: g_prefix } = g {
                    let norm_granted = normalize_path(Path::new(g_prefix));
                    norm_req.starts_with(&norm_granted)
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
                    norm_req.starts_with(&norm_granted)
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
        Capability::Spawn => false,
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
    fn satisfies_spawn_always_denied() {
        let caps = vec![Capability::Spawn];
        assert!(!satisfies(&caps, &Capability::Spawn));
    }

    #[test]
    fn satisfies_net_advisory_always_true() {
        assert!(satisfies(
            &[],
            &Capability::Net {
                hosts: vec!["api.example.com".to_string()]
            }
        ));
    }
}
