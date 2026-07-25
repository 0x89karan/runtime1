use std::cell::RefCell;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Process-wide FS anchor: agentd's working directory captured once at startup.
/// FS-capability prefixes (grant + request) are absolutized against this so the
/// authorization check reasons in absolute terms (cap.3 / audit86-P1-8). It must
/// equal the CWD the runtime tool call resolves against — i.e. agentd must not
/// `chdir` after startup (scheduler.rs documents the same no-chdir assumption).
static FS_ANCHOR: OnceLock<PathBuf> = OnceLock::new();

thread_local! {
    /// Per-thread test override for the FS anchor. Lets a unit test pin a
    /// deterministic base WITHOUT a process-global mutation that would race other
    /// parallel tests. `None` in production; only `set_fs_anchor_for_test` sets it.
    static TEST_FS_ANCHOR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Capture the startup CWD as the FS anchor. Call ONCE, early in `main`, before any
/// agent runs / any `satisfies` call. Idempotent (a second call is a no-op).
pub fn init_fs_anchor() {
    let _ = FS_ANCHOR.set(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
}

/// Test-only seam: pin the FS anchor for the current thread. `#[cfg(test)]` so
/// production code can never poison the per-thread anchor. Keeps unit tests pure
/// (no dependency on the real process CWD or on `init_fs_anchor` ordering).
#[cfg(test)]
fn set_fs_anchor_for_test(base: PathBuf) {
    TEST_FS_ANCHOR.with(|a| *a.borrow_mut() = Some(base));
}

/// The active FS anchor: a per-thread test override if set, else the startup anchor,
/// else the live CWD (fallback so a process that never called `init_fs_anchor` — e.g.
/// a test binary — still absolutizes both sides against the same base).
fn fs_anchor() -> PathBuf {
    #[cfg(test)]
    if let Some(p) = TEST_FS_ANCHOR.with(|a| a.borrow().clone()) {
        return p;
    }
    match FS_ANCHOR.get() {
        Some(a) => {
            // Harden the no-chdir invariant: authorization anchors to the frozen startup
            // CWD, but the runtime tool call resolves against the LIVE CWD. If agentd ever
            // chdir'd, the two would diverge (authorize `<frozen>/x`, write `<live>/x`).
            // In debug builds, catch that divergence loudly at the authorization site.
            debug_assert!(
                std::env::current_dir().map(|c| &c == a).unwrap_or(true),
                "cap.3 no-chdir invariant violated: live CWD != frozen FS_ANCHOR — \
                 agentd must not chdir after init_fs_anchor()"
            );
            a.clone()
        }
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    }
}

/// Absolutize a capability prefix against the FS anchor, then normalize. An absolute
/// path is normalized as-is; a relative path is joined onto the anchor first. This is
/// the SINGLE place the CWD anchor enters capability matching (cap.3 / audit86-P1-8).
///
/// This is NOT behaviour-preserving vs the old CWD-blind string-identity match, and that
/// is the point: absolutizing BOTH grant and request makes matching absolute-vs-absolute,
/// which CHANGES the mixed relative/absolute cases. A relative request that lands inside
/// an ABSOLUTE grant (or an absolute request inside a relative grant) now correctly
/// ALLOWS where the old lexical `starts_with` wrongly denied — sound because the runtime
/// resolves the request against this same anchor CWD, so there is no over-grant and no
/// previously-working flow breaks. (agentd must not chdir after startup — see `fs_anchor`.)
pub fn anchor_abs(p: &Path) -> PathBuf {
    if p.is_absolute() {
        normalize_path(p)
    } else {
        normalize_path(&fs_anchor().join(p))
    }
}

/// A GRANTED FS prefix is invalid → fail-safe deny. Two cases, both checked on the
/// PRE-anchor lexical normalize (cap.3 /review):
///   1. Empty (`""`/`"."`/`"./"`) — would match every path via `starts_with`.
///   2. Leading `..` (escapes the anchor) — `anchor_abs("..")` resolves ABOVE the
///      anchor (e.g. anchor `/data` → `/`), so the grant would authorize the entire
///      filesystem. No shipped config grants `..`; this is correct fail-closed.
fn granted_prefix_is_invalid(g_prefix: &str) -> bool {
    let norm = normalize_path(Path::new(g_prefix));
    norm.as_os_str().is_empty() || matches!(norm.components().next(), Some(Component::ParentDir))
}

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
/// `satisfies` absolutizes BOTH sides against agentd's working directory —
/// captured once at startup by [`init_fs_anchor`] — then tests
/// `abs(actual).starts_with(abs(granted_prefix))` (see [`anchor_abs`]).
///
/// **Prefixes are absolutized, not assumed-absolute (cap.3 / audit86-P1-8).** A
/// relative prefix (`"./output"`) resolves to `<startup_cwd>/output`; an absolute
/// prefix is used as-is. Both grant and request are anchored to the SAME base, so
/// matching is absolute-vs-absolute. This CHANGES the old CWD-blind string-identity
/// match in the mixed relative/absolute cases (and that is the fix): a relative
/// request landing inside an absolute grant — or an absolute request inside a
/// relative grant — now correctly ALLOWS where the old lexical `starts_with` wrongly
/// denied. It is sound because the runtime resolves the request against this same
/// anchor CWD, so there is no over-grant and no previously-working flow breaks.
/// Relies on **agentd not `chdir`-ing after startup** (see `fs_anchor`).
/// (The prior doc claimed "relative paths fail-safe to deny"; that was FALSE — they
/// matched by relative string identity. cap.3 makes the representation honest.)
/// An empty/`"."` prefix, and a grant that escapes the anchor with a leading `..`,
/// still fail safe to deny (guarded before anchoring). `~` expansion is not performed.
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
    /// Publish a morning brief via `publish_brief` (ux.11c). Unit grant — the brief
    /// is an operator-facing trust surface, so writing one is gated (F8), even though
    /// agentd (not the caller) authors the facts.
    BriefPublish,
    /// Trigger a config-declared sealed job via `run_job` (cap.2b). Unit grant. The job's
    /// caps + task template are owned by config (`[[jobs]]`), NOT chosen by the caller — so
    /// an injected, untrusted-data-reading agent (the CoS cron trigger) can trigger predeclared
    /// work but cannot author privileged work or mint caps. `spawn_agent` (Spawn) stays the
    /// trusted-delegation path where the caller authors the child; `run_job` is the hardened
    /// path for the injection-exposed data pipeline. See docs/plans/cap.2b-*.
    RunJob,
}

/// Normalize a path by resolving `.` and `..` components without filesystem
/// access. Relative paths are kept relative (a leading `..` is preserved).
/// For FS-capability matching, callers absolutize first via [`anchor_abs`]
/// (cap.3); this helper is the pure lexical normalizer underneath it.
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
            let abs_req = anchor_abs(Path::new(req_path));
            granted.iter().any(|g| {
                if let Capability::FsRead { prefix: g_prefix } = g {
                    // Fail-safe on an invalid granted prefix (empty/`.`, or leading `..`
                    // that escapes the anchor) BEFORE anchoring — else it would resolve to
                    // (or above) the anchor dir and match everything. See granted_prefix_is_invalid.
                    if granted_prefix_is_invalid(g_prefix) {
                        return false;
                    }
                    abs_req.starts_with(anchor_abs(Path::new(g_prefix)))
                } else {
                    false
                }
            })
        }
        Capability::FsWrite { prefix: req_path } => {
            let abs_req = anchor_abs(Path::new(req_path));
            granted.iter().any(|g| {
                if let Capability::FsWrite { prefix: g_prefix } = g {
                    if granted_prefix_is_invalid(g_prefix) {
                        return false;
                    }
                    abs_req.starts_with(anchor_abs(Path::new(g_prefix)))
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
        Capability::BriefPublish => granted.iter().any(|g| matches!(g, Capability::BriefPublish)),
        Capability::RunJob => granted.iter().any(|g| matches!(g, Capability::RunJob)),
    }
}

/// Attenuation subset check: is `child` covered by the `parent` capability set?
///
/// This answers "is the requested child grant ⊆ the parent's grant" for **spawn
/// attenuation** (cap.2). It is deliberately NOT `satisfies`: `satisfies` is a
/// runtime *invocation* check where `required` is always a concrete access, so it
/// takes two shortcuts that are unsound as a subset test: `Net` returns `true`
/// unconditionally (advisory at this layer), and `Mcp` with an empty *required* tool
/// list passes vacuously (`all()` over `[]`). A child could exploit either to request
/// MORE than the parent holds. So `Net` and `Mcp` get real containment here; every
/// other variant IS a correct child-⊆-parent test under `satisfies` and is delegated.
///
/// The match is EXHAUSTIVE with no wildcard arm: a new `Capability` variant will not
/// compile until its containment rule is decided here (the same drift guard as
/// `tier_legality`).
pub fn capability_covered_by(parent: &[Capability], child: &Capability) -> bool {
    match child {
        // Net is checked PER PARENT ENTRY (not unioned across entries): a single grant
        // pairs its hosts with its ports, so `Net{[a],[443]}` + `Net{[b],[22]}` does NOT
        // grant `[a]:22`. Unioning hosts and ports independently would over-grant across
        // that cross-product. Per-entry `any()` can over-DENY a child covered only by the
        // union (safe direction, fail-closed) but never over-grants.
        Capability::Net { hosts, ports } => parent.iter().any(|p| {
            if let Capability::Net { hosts: ph, ports: pp } = p {
                list_covers(ph, hosts) && list_covers(pp, ports)
            } else {
                false
            }
        }),
        // Mcp tools on a server have no host×port-style pairing, so a child is covered by
        // the UNION of all same-server parent grants (review: both voices — per-entry `any()`
        // would over-deny a legitimate split grant). An empty parent tool list on any
        // matching entry is a wildcard (covers all, including a wildcard child).
        Capability::Mcp { server, tools } => {
            let same_server: Vec<&[String]> = parent
                .iter()
                .filter_map(|p| match p {
                    Capability::Mcp { server: ps, tools: pt } if ps == server => Some(pt.as_slice()),
                    _ => None,
                })
                .collect();
            if same_server.is_empty() {
                false
            } else if same_server.iter().any(|pt| pt.is_empty()) {
                true // a wildcard parent entry covers every child request on this server
            } else if tools.is_empty() {
                false // child requests ALL tools but no wildcard parent entry exists
            } else {
                tools.iter().all(|t| same_server.iter().any(|pt| pt.contains(t)))
            }
        }
        Capability::FsRead { .. }
        | Capability::FsWrite { .. }
        | Capability::Spawn
        | Capability::KbRead { .. }
        | Capability::KbWrite { .. }
        | Capability::ShellExec
        | Capability::Credential { .. }
        | Capability::RunsRead
        | Capability::BriefPublish
        | Capability::RunJob => satisfies(parent, child),
    }
}

/// Wildcard-aware list containment for the `Net`/`Mcp` attenuation checks.
///
/// An empty `parent` list is a wildcard: it covers everything, including an empty
/// (also-wildcard) child request. A non-empty `parent` covers a child only when the
/// child is a non-empty subset — an empty child ("give me all of them") is NOT
/// covered by an explicit parent. Fail-closed: containment must be provable.
fn list_covers<T: PartialEq>(parent: &[T], child: &[T]) -> bool {
    parent.is_empty() || (!child.is_empty() && child.iter().all(|c| parent.contains(c)))
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

/// The broker-registry key for a credential provider. This MUST match the runtime broker's
/// key derivation (`credential_allowed_providers` in main.rs) so the cap.1 linter's wiring
/// cross-check and the runtime broker never disagree — a second key derivation is exactly
/// the drift this increment exists to prevent (review: both voices).
pub fn credential_provider_key(p: &CredentialProvider) -> String {
    match p {
        CredentialProvider::Google => "google".to_string(),
        CredentialProvider::BraveSearch => "brave-search".to_string(),
        CredentialProvider::Custom(s) => s.clone(),
    }
}

/// A bare `agent` KB-segment grant (cap.1 A5 / audit-C2 / P1-11).
///
/// `kb_segment_satisfies` matches on a `granted` prefix followed by a delimiter, so ONLY the
/// bare `agent` namespace (no trailing delimiter) satisfies every per-agent Tier-3
/// self-namespace (`agent/<id>` AND `agent:<id>`), defeating memory isolation. `"agent/"` and
/// `"agent:"` match only their literal selves (review F2: `kb_segment_satisfies("agent/",
/// "agent/inbox")` is false — the stripped remainder has no leading delimiter), so they are
/// harmless and are NOT flagged. A legitimate grant names the full `agent/<id>`; the per-agent
/// self-namespace used by `remember`/`recall` is never a config grant.
pub fn is_bare_agent_segment(segment: &str) -> bool {
    segment == "agent"
}

/// The tier a capability is declared at. A single `Capability` variant means different
/// things (or nothing) depending on where it is declared, and the enforcement paths differ —
/// this is the "declaration surface" the cap.1 audit targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapContext {
    /// Granted to an agent (`[[agents]].capabilities`).
    Agent,
    /// Declared on a stdio MCP server (`[[mcp_servers]].capabilities`) → Landlock/seccomp.
    StdioMcp,
    /// Declared on an HTTP MCP server — the transport discards all sandbox fields.
    HttpMcp,
}

/// Whether a capability is actually enforced when declared in a given context, or inert
/// (present in config but a silent no-op — the fail-closed-with-no-signal class).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Legality {
    /// The grant is honored by some enforcement path in this context.
    Enforced,
    /// The grant is a silent no-op here; the `&'static str` explains why (for diagnostics).
    Inert(&'static str),
}

/// The ONE shared effective-capability resolver (cap.1 F3). `agentd check` (the linter) and
/// the `CapabilitiesResolved` boot event both call this — there must be exactly one place
/// that decides which (capability × context) pairs are enforced vs inert, or the linter
/// becomes a third interpreter that drifts from enforcement as the enum grows.
///
/// NO wildcard arm on `Capability`: adding a variant MUST fail to compile here until its
/// tier legality is declared for every context (the drift guard — do not "fix the build"
/// with a `_ =>` arm). `Credential { Agent }` is `Inert` here, but whether that is an ERROR
/// is decided by the config-level *wiring* cross-check in `check.rs` (it depends on whether
/// a matching stdio MCP server provides the provider), not by this per-pair function.
pub fn tier_legality(cap: &Capability, ctx: CapContext) -> Legality {
    use Capability::*;
    match ctx {
        // HTTP transport reads only url/headers_env; every sandbox/grant field is discarded.
        CapContext::HttpMcp => Legality::Inert("HTTP MCP transport discards capabilities/isolation"),
        CapContext::Agent => match cap {
            FsRead { .. } | FsWrite { .. } => Legality::Enforced,
            Mcp { .. } | Spawn | KbRead { .. } | KbWrite { .. } | RunsRead | BriefPublish | RunJob => {
                Legality::Enforced
            }
            Net { .. } => Legality::Inert("agent-level Net is advisory; native agents have no sandbox"),
            ShellExec => Legality::Inert("agent-level ShellExec is not gated; use Mcp { server }"),
            Credential { .. } => {
                Legality::Inert("agent-level Credential is not enforced; the broker token is per stdio-server")
            }
        },
        // A stdio server's own sandbox: FS/Net/Spawn/ShellExec/Credential compile to
        // Landlock/seccomp/broker rules; agent-facing caps have no server-sandbox meaning.
        CapContext::StdioMcp => match cap {
            FsRead { .. } | FsWrite { .. } | Net { .. } | Spawn | ShellExec | Credential { .. } => {
                Legality::Enforced
            }
            Mcp { .. } => Legality::Inert("Mcp grant has no meaning on a server's own sandbox"),
            KbRead { .. } | KbWrite { .. } | RunsRead | BriefPublish | RunJob => {
                Legality::Inert("agent-facing capability has no server-sandbox rule")
            }
        },
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

    // ── cap.3 (audit86-P1-8): relative FS prefixes are anchored to the startup CWD ──
    // (behaviour-preserving — same allow/deny as pre-cap.3, but the representation is
    // absolute and the doc is honest). `set_fs_anchor_for_test` pins a deterministic base.

    #[test]
    fn cap3_relative_grant_anchored_allows_in_subtree_denies_outside() {
        set_fs_anchor_for_test(PathBuf::from("/data"));
        let caps = vec![Capability::FsWrite { prefix: "./output".to_string() }];
        // In-subtree request (relative) is ALLOWED and resolves under /data/output.
        assert!(satisfies(&caps, &Capability::FsWrite { prefix: "output/brief.md".to_string() }));
        assert!(satisfies(&caps, &Capability::FsWrite { prefix: "./output/x".to_string() }));
        // Sibling / traversal / absolute-elsewhere are DENIED (unchanged from pre-cap.3).
        assert!(!satisfies(&caps, &Capability::FsWrite { prefix: "secret/x".to_string() }));
        assert!(!satisfies(&caps, &Capability::FsWrite { prefix: "output/../secret".to_string() }));
        assert!(!satisfies(&caps, &Capability::FsWrite { prefix: "/etc/passwd".to_string() }));
    }

    #[test]
    fn cap3_relative_grant_normalizes_to_an_absolute_prefix() {
        // The anchoring makes the matched prefix ABSOLUTE (kills CWD-blindness): a request
        // that is absolute-under-the-anchor matches; one under a DIFFERENT root does not.
        set_fs_anchor_for_test(PathBuf::from("/data"));
        let caps = vec![Capability::FsWrite { prefix: "output".to_string() }];
        assert!(satisfies(&caps, &Capability::FsWrite { prefix: "/data/output/x".to_string() }));
        assert!(!satisfies(&caps, &Capability::FsWrite { prefix: "/other/output/x".to_string() }));
    }

    #[test]
    fn cap3_absolute_grant_unchanged_by_anchoring() {
        // A prod-style absolute grant behaves identically regardless of the anchor.
        set_fs_anchor_for_test(PathBuf::from("/data"));
        let caps = vec![Capability::FsWrite { prefix: "/run/output".to_string() }];
        assert!(satisfies(&caps, &Capability::FsWrite { prefix: "/run/output/brief.md".to_string() }));
        assert!(!satisfies(&caps, &Capability::FsWrite { prefix: "/etc/x".to_string() }));
        // A relative request never matches an absolute grant under a different root.
        assert!(!satisfies(&caps, &Capability::FsWrite { prefix: "output/x".to_string() }));
    }

    #[test]
    fn cap3_mixed_relative_grant_absolute_request_denied_both_ways() {
        set_fs_anchor_for_test(PathBuf::from("/data"));
        // relative grant vs absolute request under a different root → deny
        let rel_grant = vec![Capability::FsRead { prefix: "output".to_string() }];
        assert!(!satisfies(&rel_grant, &Capability::FsRead { prefix: "/run/output/x".to_string() }));
        // absolute grant vs relative request (anchors under /data, not /run) → deny
        let abs_grant = vec![Capability::FsRead { prefix: "/run/output".to_string() }];
        assert!(!satisfies(&abs_grant, &Capability::FsRead { prefix: "output/x".to_string() }));
    }

    #[test]
    fn cap3_empty_grant_still_denies_after_anchoring() {
        // The empty-prefix fail-safe must survive anchoring (an empty prefix would
        // otherwise resolve to the anchor dir and match everything under it).
        set_fs_anchor_for_test(PathBuf::from("/data"));
        for empty in ["", ".", "./"] {
            let caps = vec![Capability::FsWrite { prefix: empty.to_string() }];
            assert!(
                !satisfies(&caps, &Capability::FsWrite { prefix: "data/anything".to_string() }),
                "empty/./ grant {empty:?} must still deny after anchoring"
            );
        }
    }

    #[test]
    fn cap3_no_chdir_invariant_documented() {
        // The anchor is captured ONCE and both grant+request use it, so a hypothetical
        // chdir between them would diverge (check-base != exec-base). We pin the
        // captured-once behaviour: two anchors on the same thread — the LAST set wins,
        // and BOTH sides of a single satisfies() use that one value (never two).
        set_fs_anchor_for_test(PathBuf::from("/a"));
        let caps = vec![Capability::FsWrite { prefix: "out".to_string() }];
        assert!(satisfies(&caps, &Capability::FsWrite { prefix: "out/x".to_string() }));
        // Re-pin: both sides now anchor to /b consistently; still matches (proves single anchor).
        set_fs_anchor_for_test(PathBuf::from("/b"));
        assert!(satisfies(&caps, &Capability::FsWrite { prefix: "out/x".to_string() }));
    }

    #[test]
    fn cap3_dotdot_grant_denies_everything() {
        // cap.3 /review FIX 2: a `..` grant escapes the anchor (`anchor_abs("..")` with
        // anchor /data = `/`), which would authorize the WHOLE filesystem. It must fail-closed.
        // (Correct fail-closed: pre-cap.3 a `..` grant matched `../`-prefixed requests; no
        // shipped config grants `..`.)
        set_fs_anchor_for_test(PathBuf::from("/data"));
        let caps = vec![Capability::FsRead { prefix: "..".to_string() }];
        assert!(!satisfies(&caps, &Capability::FsRead { prefix: "output/x".to_string() }));
        assert!(!satisfies(&caps, &Capability::FsRead { prefix: "/etc/passwd".to_string() }));
        let caps_w = vec![Capability::FsWrite { prefix: "../..".to_string() }];
        assert!(!satisfies(&caps_w, &Capability::FsWrite { prefix: "anything/x".to_string() }));
    }

    #[test]
    fn cap3_absolute_grant_relative_request_inside_now_allows() {
        // cap.3 /review FIX 4 (the INTENDED soundness flip — NOT behavior-preserving): an
        // absolute grant UNDER the anchor + a relative request that lands inside it now
        // correctly ALLOWS. Pre-cap.3 the lexical `starts_with("/data/output")` on the raw
        // relative request `"output/x"` wrongly DENIED. Sound because the runtime resolves the
        // request against the same anchor CWD, so `output/x` really is `/data/output/x`.
        set_fs_anchor_for_test(PathBuf::from("/data"));
        let caps = vec![Capability::FsWrite { prefix: "/data/output".to_string() }];
        assert!(
            satisfies(&caps, &Capability::FsWrite { prefix: "output/x".to_string() }),
            "relative request inside an absolute-under-anchor grant must ALLOW (soundness fix)"
        );
        // Symmetric: relative grant under anchor + absolute request inside it also allows.
        let caps2 = vec![Capability::FsRead { prefix: "output".to_string() }];
        assert!(satisfies(&caps2, &Capability::FsRead { prefix: "/data/output/x".to_string() }));
        // And an absolute request NOT under the anchored grant still denies.
        assert!(!satisfies(&caps2, &Capability::FsRead { prefix: "/etc/x".to_string() }));
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
    fn satisfies_brief_publish_requires_grant() {
        // ux.11c: publish_brief is gated (F8) — grant required, unit-matched like RunsRead.
        assert!(satisfies(&[Capability::BriefPublish], &Capability::BriefPublish));
        assert!(!satisfies(&[], &Capability::BriefPublish));
        assert!(!satisfies(&[Capability::RunsRead], &Capability::BriefPublish));
    }

    // ─────────────────────────── cap.1: tier_legality + bare-agent ───────────────────────────

    #[test]
    fn tier_legality_pins_the_matrix() {
        use Capability::*;
        // NOTE: the real drift guard is the COMPILE-TIME non-wildcard match in `tier_legality`
        // (a new variant fails to compile until declared). This test only pins known values.
        let fsr = FsRead { prefix: "/x".into() };
        let net = Net { hosts: vec![], ports: vec![] };
        let cred = Credential { provider: CredentialProvider::Google };

        // Agent context: FS enforced; Net/ShellExec/Credential inert.
        assert_eq!(tier_legality(&fsr, CapContext::Agent), Legality::Enforced);
        assert!(matches!(tier_legality(&net, CapContext::Agent), Legality::Inert(_)));
        assert!(matches!(tier_legality(&ShellExec, CapContext::Agent), Legality::Inert(_)));
        assert!(matches!(tier_legality(&cred, CapContext::Agent), Legality::Inert(_)));
        assert_eq!(tier_legality(&Spawn, CapContext::Agent), Legality::Enforced);
        assert_eq!(tier_legality(&RunsRead, CapContext::Agent), Legality::Enforced);

        // Stdio-MCP context: FS/Net/ShellExec/Credential enforced; agent-facing caps inert.
        assert_eq!(tier_legality(&net, CapContext::StdioMcp), Legality::Enforced);
        assert_eq!(tier_legality(&cred, CapContext::StdioMcp), Legality::Enforced);
        assert_eq!(tier_legality(&ShellExec, CapContext::StdioMcp), Legality::Enforced);
        assert!(matches!(tier_legality(&RunsRead, CapContext::StdioMcp), Legality::Inert(_)));

        // HTTP-MCP context: everything inert (transport discards all sandbox fields).
        assert!(matches!(tier_legality(&fsr, CapContext::HttpMcp), Legality::Inert(_)));
        assert!(matches!(tier_legality(&cred, CapContext::HttpMcp), Legality::Inert(_)));
    }

    #[test]
    fn bare_agent_segment_detection() {
        // Only bare "agent" (no delimiter) prefixes both agent/<id> and agent:<id> → dangerous.
        assert!(is_bare_agent_segment("agent"));
        // "agent/" and "agent:" match only themselves (review F2) → harmless, not flagged.
        assert!(!is_bare_agent_segment("agent/"));
        assert!(!is_bare_agent_segment("agent:"));
        assert!(!is_bare_agent_segment("agent/inbox"));
        assert!(!is_bare_agent_segment("agent:inbox"));
        assert!(!is_bare_agent_segment("ops:briefs"));
        // The fact the narrowing relies on:
        assert!(kb_segment_satisfies("agent", "agent/x")); // bare agent IS dangerous
        assert!(kb_segment_satisfies("agent", "agent:x"));
        assert!(!kb_segment_satisfies("agent/", "agent/x")); // "agent/" is NOT
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

    // ---- capability_covered_by: spawn attenuation subset check (cap.2) ----

    #[test]
    fn covered_net_wildcard_parent_covers_explicit_and_empty_child() {
        let parent = vec![Capability::Net { hosts: vec![], ports: vec![] }];
        // explicit child
        assert!(capability_covered_by(
            &parent,
            &Capability::Net { hosts: vec!["api.example.com".into()], ports: vec![443] }
        ));
        // empty (wildcard) child — only a wildcard parent covers this
        assert!(capability_covered_by(&parent, &Capability::Net { hosts: vec![], ports: vec![] }));
    }

    #[test]
    fn covered_net_explicit_parent_rejects_wildcard_child() {
        // The unsoundness `satisfies` had: an explicit parent must NOT cover a child
        // asking for everything.
        let parent = vec![Capability::Net { hosts: vec!["a.com".into()], ports: vec![443] }];
        assert!(!capability_covered_by(&parent, &Capability::Net { hosts: vec![], ports: vec![] }));
        // and `satisfies` would have (wrongly) returned true here:
        assert!(satisfies(&parent, &Capability::Net { hosts: vec![], ports: vec![] }));
    }

    #[test]
    fn covered_net_subset_ok_superset_denied() {
        let parent = vec![Capability::Net {
            hosts: vec!["a.com".into(), "b.com".into()],
            ports: vec![443, 8443],
        }];
        // subset host + port
        assert!(capability_covered_by(
            &parent,
            &Capability::Net { hosts: vec!["a.com".into()], ports: vec![443] }
        ));
        // extra host → denied
        assert!(!capability_covered_by(
            &parent,
            &Capability::Net { hosts: vec!["c.com".into()], ports: vec![443] }
        ));
        // extra port → denied
        assert!(!capability_covered_by(
            &parent,
            &Capability::Net { hosts: vec!["a.com".into()], ports: vec![22] }
        ));
    }

    #[test]
    fn covered_mcp_explicit_parent_rejects_wildcard_child() {
        // The Codex catch: satisfies is vacuously true for an empty child tool list.
        let parent = vec![Capability::Mcp {
            server: "google_oauth".into(),
            tools: vec!["gmail_read".into()],
        }];
        let child_all = Capability::Mcp { server: "google_oauth".into(), tools: vec![] };
        assert!(!capability_covered_by(&parent, &child_all), "explicit parent must not cover wildcard child");
        // `satisfies` would have wrongly allowed it:
        assert!(satisfies(&parent, &child_all));
    }

    #[test]
    fn covered_mcp_wildcard_parent_covers_explicit_and_wildcard_child() {
        let parent = vec![Capability::Mcp { server: "google_oauth".into(), tools: vec![] }];
        assert!(capability_covered_by(
            &parent,
            &Capability::Mcp { server: "google_oauth".into(), tools: vec!["gmail_read".into()] }
        ));
        assert!(capability_covered_by(
            &parent,
            &Capability::Mcp { server: "google_oauth".into(), tools: vec![] }
        ));
        // wrong server never covered
        assert!(!capability_covered_by(
            &parent,
            &Capability::Mcp { server: "github".into(), tools: vec![] }
        ));
    }

    #[test]
    fn covered_mcp_explicit_subset_ok() {
        let parent = vec![Capability::Mcp {
            server: "google_oauth".into(),
            tools: vec!["gmail_read".into(), "gmail_send".into()],
        }];
        assert!(capability_covered_by(
            &parent,
            &Capability::Mcp { server: "google_oauth".into(), tools: vec!["gmail_read".into()] }
        ));
        assert!(!capability_covered_by(
            &parent,
            &Capability::Mcp { server: "google_oauth".into(), tools: vec!["gmail_delete".into()] }
        ));
    }

    #[test]
    fn covered_mcp_unions_split_parent_grants() {
        // A child covered only by the UNION of same-server parent grants is allowed
        // (review: per-entry any() would over-deny this legitimate split).
        let parent = vec![
            Capability::Mcp { server: "gmail".into(), tools: vec!["read".into()] },
            Capability::Mcp { server: "gmail".into(), tools: vec!["send".into()] },
        ];
        assert!(capability_covered_by(
            &parent,
            &Capability::Mcp { server: "gmail".into(), tools: vec!["read".into(), "send".into()] }
        ));
        // a tool in neither entry is still denied
        assert!(!capability_covered_by(
            &parent,
            &Capability::Mcp { server: "gmail".into(), tools: vec!["read".into(), "delete".into()] }
        ));
        // child wildcard still needs a wildcard parent entry (split explicit grants don't imply all)
        assert!(!capability_covered_by(
            &parent,
            &Capability::Mcp { server: "gmail".into(), tools: vec![] }
        ));
    }

    #[test]
    fn covered_fs_narrow_ok_widen_denied() {
        let parent = vec![Capability::FsRead { prefix: "/data".into() }];
        assert!(capability_covered_by(&parent, &Capability::FsRead { prefix: "/data/sub".into() }));
        assert!(!capability_covered_by(&parent, &Capability::FsRead { prefix: "/".into() }));
        assert!(!capability_covered_by(&parent, &Capability::FsRead { prefix: "/etc".into() }));
    }

    #[test]
    fn covered_kb_narrow_ok_widen_denied() {
        let parent = vec![Capability::KbRead { segment: "ops".into() }];
        assert!(capability_covered_by(&parent, &Capability::KbRead { segment: "ops:briefs".into() }));
        assert!(!capability_covered_by(&parent, &Capability::KbRead { segment: "agent".into() }));
    }

    #[test]
    fn covered_credential_exact_ok_wrong_denied() {
        let parent = vec![Capability::Credential { provider: CredentialProvider::Google }];
        assert!(capability_covered_by(
            &parent,
            &Capability::Credential { provider: CredentialProvider::Google }
        ));
        assert!(!capability_covered_by(
            &parent,
            &Capability::Credential { provider: CredentialProvider::BraveSearch }
        ));
    }

    #[test]
    fn covered_unit_caps_present_ok_missing_denied() {
        let parent = vec![Capability::Spawn, Capability::RunsRead];
        assert!(capability_covered_by(&parent, &Capability::Spawn));
        assert!(capability_covered_by(&parent, &Capability::RunsRead));
        assert!(!capability_covered_by(&parent, &Capability::BriefPublish));
        assert!(!capability_covered_by(&parent, &Capability::ShellExec));
    }

    #[test]
    fn covered_empty_parent_covers_nothing_except_wildcards() {
        // An empty parent set (deny-all) covers no concrete grant.
        assert!(!capability_covered_by(&[], &Capability::Spawn));
        assert!(!capability_covered_by(&[], &Capability::Mcp { server: "x".into(), tools: vec![] }));
        assert!(!capability_covered_by(&[], &Capability::Credential { provider: CredentialProvider::Google }));
    }
}
