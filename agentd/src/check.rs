//! `agentd check` — the capability declaration-surface linter (cap.1).
//!
//! Validates a config the way the runtime matcher (`satisfies`) can't at invocation time:
//! it catches grants that *look* granted but are inert or wrong, which otherwise fail
//! closed in production with no signal (the Gmail-outage / v0.86.2 class). It reuses the
//! runtime parse+lower path (`toml::from_str::<Config>` + `McpServerConfig::validate` +
//! `Config::agent_configs`) and the ONE shared [`tier_legality`] resolver — it is a static
//! linter and never boots anything (no subprocess/FUSE/proxy).
//!
//! Severity model (cap.1 F5): errors always fail; `--strict` (container boot) additionally
//! elevates relative FS prefixes to errors. Warnings never fail.

use std::collections::HashSet;
use std::path::Path;

use crate::capability::{
    credential_provider_key, is_bare_agent_segment, kb_segment_satisfies, tier_legality, CapContext,
    Capability, Legality,
};
use crate::config::{Config, IsolationMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct CheckReport {
    pub findings: Vec<Finding>,
}

impl CheckReport {
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }
    fn error(&mut self, msg: String) {
        self.findings.push(Finding { severity: Severity::Error, message: msg });
    }
    fn warn(&mut self, msg: String) {
        self.findings.push(Finding { severity: Severity::Warning, message: msg });
    }
}

/// A CWD-blind or otherwise-untrustworthy FS prefix: not absolute, OR padded with
/// leading/trailing whitespace (which passes a naive `starts_with('/')` but breaks the
/// runtime's byte-exact `starts_with` match → silent denial — review F4).
fn is_suspect_fs_prefix(p: &str) -> bool {
    !p.starts_with('/') || p != p.trim()
}

/// True if some declared `[memory].segments` entry covers this grant (exact or prefix).
fn kb_segment_declared(grant: &str, declared: &HashSet<&str>) -> bool {
    declared.iter().any(|seg| *seg == grant || kb_segment_satisfies(grant, seg))
}

/// Lint an already-parsed config. `strict` = container-boot profile (relative FS prefixes
/// become errors). Pure — no I/O, no side effects.
pub fn check_config(cfg: &Config, strict: bool) -> CheckReport {
    let mut r = CheckReport::default();
    let rel_is_error = strict;

    let server_names: HashSet<&str> =
        cfg.tools.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    let seg_names: HashSet<&str> =
        cfg.memory.segments.iter().map(|s| s.name.as_str()).collect();

    // Which credential providers does some stdio MCP server actually carry? Only these end
    // up in a broker token's allowed_providers (main.rs credential_allowed_providers).
    let mut providers_provided: HashSet<String> = HashSet::new();
    for s in &cfg.tools.mcp_servers {
        if s.is_http() {
            // HTTP transport reads only url/headers_env; sandbox fields are silently
            // discarded (P1-9). Declaring them is a hard error — the operator believes the
            // server is sandboxed when it is not.
            if s.capabilities.is_some() {
                r.error(format!(
                    "HTTP MCP server '{}' declares `capabilities` — the HTTP transport \
                     silently discards them (the server is NOT sandboxed). Remove them, or \
                     use a stdio `command` server if you need a sandbox.",
                    s.name
                ));
            }
            if s.isolation != IsolationMode::None {
                r.error(format!(
                    "HTTP MCP server '{}' declares `isolation` — silently discarded by the \
                     HTTP transport. Remove it.",
                    s.name
                ));
            }
        } else {
            for cap in s.capabilities.iter().flatten() {
                if let Capability::Credential { provider } = cap {
                    providers_provided.insert(credential_provider_key(provider));
                }
                if let Capability::FsRead { prefix } | Capability::FsWrite { prefix } = cap {
                    if is_suspect_fs_prefix(prefix) {
                        relative_prefix_finding(&mut r, rel_is_error, &format!(
                            "MCP server '{}'", s.name), prefix);
                    }
                }
                if let Legality::Inert(why) = tier_legality(cap, CapContext::StdioMcp) {
                    r.warn(format!("MCP server '{}': capability {cap:?} is inert here ({why})", s.name));
                }
            }
        }
    }

    // Agents (lowered the same way boot does).
    let agents = match cfg.agent_configs() {
        Ok(a) => a,
        Err(e) => {
            r.error(format!("config lowering failed (agent_configs): {e}"));
            return r;
        }
    };
    let mut providers_referenced: HashSet<String> = HashSet::new();
    for a in &agents {
        // Unrestricted agent (capabilities omitted): it can invoke every registered tool,
        // including credential-brokering MCP servers, but declares no Credential grant — so
        // the wiring cross-check below cannot verify it (review: Codex F2). Surface honestly.
        if a.capabilities.is_none() {
            r.warn(format!(
                "agent '{}' declares no capabilities (unrestricted) — it can use any tool, \
                 so `agentd check` cannot verify its credential wiring or least-privilege",
                a.id
            ));
        }
        for cap in a.capabilities.iter().flatten() {
            match cap {
                Capability::Mcp { server, .. } => {
                    if !server_names.contains(server.as_str()) {
                        r.error(format!(
                            "agent '{}' grants Mcp {{ server = \"{server}\" }} but no \
                             [[mcp_servers]] with that name exists",
                            a.id
                        ));
                    }
                }
                Capability::KbRead { segment } | Capability::KbWrite { segment } => {
                    if is_bare_agent_segment(segment) {
                        r.error(format!(
                            "agent '{}' grants a bare '{segment}' KB segment — this satisfies \
                             every other agent's per-agent memory namespace, defeating \
                             isolation. Grant the full 'agent/<id>' form.",
                            a.id
                        ));
                    } else if !kb_segment_declared(segment, &seg_names) {
                        r.warn(format!(
                            "agent '{}' grants KB segment '{segment}' but no [memory].segments \
                             entry declares it",
                            a.id
                        ));
                    }
                }
                Capability::Credential { provider } => {
                    providers_referenced.insert(credential_provider_key(provider));
                }
                Capability::FsRead { prefix } | Capability::FsWrite { prefix }
                    if is_suspect_fs_prefix(prefix) =>
                {
                    relative_prefix_finding(&mut r, rel_is_error, &format!("agent '{}'", a.id), prefix);
                }
                _ => {}
            }
            // Inert agent-level grant → warning. Credential is Inert at agent-level too, but
            // its real problem (or non-problem) is the wiring cross-check below — don't
            // double-report it here.
            if !matches!(cap, Capability::Credential { .. }) {
                if let Legality::Inert(why) = tier_legality(cap, CapContext::Agent) {
                    r.warn(format!("agent '{}': capability {cap:?} is inert at agent level ({why})", a.id));
                }
            }
        }
    }

    // Credential wiring cross-check (G1 — the true Gmail-outage class). A provider granted
    // to an agent but carried by NO stdio MCP server yields an empty broker token → every
    // brokered call for it fails silently. This is an error in BOTH modes (the real config
    // carries Credential on the google_oauth server, so it passes; only a mis-wired config
    // trips it).
    for p in &providers_referenced {
        if !providers_provided.contains(p) {
            r.error(format!(
                "Credential provider '{p}' is granted to an agent but no stdio MCP server \
                 carries a matching Credential capability — the broker token will never \
                 include it, so every call fails silently (the Gmail-outage class). Add \
                 `{{ Credential = {{ provider = \"{p}\" }} }}` to the server that brokers it."
            ));
        }
    }

    // The mandatory-sandbox switch doesn't cover HTTP servers (no subprocess to sandbox).
    if cfg.tools.mcp_require_capabilities && cfg.tools.mcp_servers.iter().any(|s| s.is_http()) {
        r.warn(
            "mcp_require_capabilities is on, but HTTP MCP servers are exempt from the \
             mandatory-sandbox switch (they have no subprocess to sandbox) — remote tool \
             servers run unsandboxed."
                .to_string(),
        );
    }

    r
}

/// Build the `CapabilitiesResolved` boot-event payloads (cap.1 A2) — one per agent and per
/// MCP server, splitting each declared cap into enforced vs inert via the SAME
/// `tier_legality` resolver the linter uses. Descriptive only; enforcement is unchanged.
pub fn capabilities_resolved_events(cfg: &Config) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    if let Ok(agents) = cfg.agent_configs() {
        for a in &agents {
            out.push(resolved_payload(
                "agent",
                &a.id,
                a.capabilities.as_deref().unwrap_or(&[]),
                CapContext::Agent,
            ));
        }
    }
    for s in &cfg.tools.mcp_servers {
        let ctx = if s.is_http() { CapContext::HttpMcp } else { CapContext::StdioMcp };
        out.push(resolved_payload("mcp_server", &s.name, s.capabilities.as_deref().unwrap_or(&[]), ctx));
    }
    out
}

fn resolved_payload(kind: &str, name: &str, caps: &[Capability], ctx: CapContext) -> serde_json::Value {
    let mut enforced = Vec::new();
    let mut inert = Vec::new();
    for c in caps {
        match tier_legality(c, ctx) {
            Legality::Enforced => enforced.push(format!("{c:?}")),
            Legality::Inert(why) => {
                inert.push(serde_json::json!({ "cap": format!("{c:?}"), "reason": why }))
            }
        }
    }
    serde_json::json!({ "kind": kind, "name": name, "enforced": enforced, "inert": inert })
}

fn relative_prefix_finding(r: &mut CheckReport, rel_is_error: bool, who: &str, prefix: &str) {
    let msg = format!(
        "{who} has a non-absolute or whitespace-padded FS prefix '{prefix}' — it matches \
         textually only (CWD-blind; a container's working dir is unpredictable, and stray \
         whitespace breaks the byte-exact runtime match). Use a clean absolute path."
    );
    if rel_is_error {
        r.error(msg);
    } else {
        r.warn(msg);
    }
}

/// Load a config the way boot does (parse + lower + per-server transport validation) and
/// lint it. Returns the report; `Err` only on unreadable/unparseable input (a structural
/// failure, distinct from lint findings).
pub fn check_path(path: &Path, strict: bool) -> anyhow::Result<CheckReport> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("{}: unreadable: {e}", path.display()))?;
    let cfg: Config = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("{}: does not parse as a Config: {e}", path.display()))?;
    let mut report = CheckReport::default();
    // Per-server transport legality (url-xor-command, https) — a structural error class.
    for s in &cfg.tools.mcp_servers {
        if let Err(e) = s.validate() {
            report.error(format!("MCP server '{}': {e}", s.name));
        }
    }
    let mut lint = check_config(&cfg, strict);
    report.findings.append(&mut lint.findings);
    Ok(report)
}

/// `agentd check [--strict] <path>` entry point: lint, print findings, fail-closed on any
/// error-severity finding. Returns `Err` so the CLI exits non-zero (the container-boot gate).
pub fn run_check(path: &Path, strict: bool) -> anyhow::Result<()> {
    let report = check_path(path, strict)?;
    let (mut errs, mut warns) = (0u32, 0u32);
    for f in &report.findings {
        match f.severity {
            Severity::Error => {
                errs += 1;
                eprintln!("error: {}", f.message);
            }
            Severity::Warning => {
                warns += 1;
                eprintln!("warning: {}", f.message);
            }
        }
    }
    // Honesty boundary (F8 + review F1): a clean check does not prove runtime readiness, and
    // the credential wiring cross-check is config-global (some stdio server carries each
    // referenced provider) — not per-server-binding.
    eprintln!(
        "agentd check{}: {} error(s), {} warning(s) on {} \
         (validates capability DECLARATIONS only — not credential presence/scope, per-server \
         credential binding, MCP-server boot, or OAuth validity)",
        if strict { " --strict" } else { "" },
        errs,
        warns,
        path.display()
    );
    if report.has_errors() {
        anyhow::bail!("agentd check found {errs} error(s) — config would fail closed at runtime");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("test config parses")
    }

    // A minimal well-wired config: agent grants Credential{Google}, and a stdio server
    // carries it → wiring satisfied; absolute FS prefix; declared segment.
    const WIRED: &str = r#"
[[tools.mcp_servers]]
name = "google_oauth"
command = "python3"
capabilities = [{ Credential = { provider = "Google" } }]

[[memory.segments]]
name = "ops:briefs"
class = "log"

[[agents]]
id = "cos"
task = "t"
capabilities = [
  { Credential = { provider = "Google" } },
  { KbRead = { segment = "ops:briefs" } },
  { FsWrite = { prefix = "/data/output" } },
]
"#;

    #[test]
    fn wired_config_is_clean() {
        let r = check_config(&parse(WIRED), true);
        assert!(!r.has_errors(), "well-wired config must pass --strict: {:?}", r.findings);
    }

    #[test]
    fn gmail_missing_server_credential_fails() {
        // Agent references Credential{Google} but NO server carries it → the Gmail bug.
        let cfg = parse(
            r#"
[[tools.mcp_servers]]
name = "google_oauth"
command = "python3"

[[agents]]
id = "cos"
task = "t"
capabilities = [{ Credential = { provider = "Google" } }]
"#,
        );
        let r = check_config(&cfg, false);
        assert!(r.has_errors(), "unwired credential must be an error even in default mode");
        assert!(r.findings.iter().any(|f| f.message.contains("Gmail-outage")));
    }

    #[test]
    fn relative_fswrite_is_contextual() {
        let cfg = parse(
            r#"
[[agents]]
id = "a"
task = "t"
capabilities = [{ FsWrite = { prefix = "./output" } }]
"#,
        );
        assert!(!check_config(&cfg, false).has_errors(), "relative prefix warns in default mode");
        assert!(check_config(&cfg, true).has_errors(), "relative prefix errors in --strict");
    }

    #[test]
    fn http_server_with_sandbox_fields_fails() {
        let cfg = parse(
            r#"
[[tools.mcp_servers]]
name = "remote"
url = "https://example.com/mcp"
capabilities = [{ FsRead = { prefix = "/x" } }]

[[agents]]
id = "a"
task = "t"
capabilities = []
"#,
        );
        let r = check_config(&cfg, false);
        assert!(r.has_errors(), "HTTP server carrying capabilities must error");
        assert!(r.findings.iter().any(|f| f.message.contains("silently discards")));
    }

    #[test]
    fn bare_agent_kb_grant_fails() {
        let cfg = parse(
            r#"
[[agents]]
id = "a"
task = "t"
capabilities = [{ KbRead = { segment = "agent" } }]
"#,
        );
        assert!(check_config(&cfg, false).has_errors());
    }

    #[test]
    fn unknown_mcp_server_fails() {
        let cfg = parse(
            r#"
[[agents]]
id = "a"
task = "t"
capabilities = [{ Mcp = { server = "nope", tools = [] } }]
"#,
        );
        assert!(check_config(&cfg, false).has_errors());
    }

    #[test]
    fn whitespace_padded_absolute_prefix_flagged_in_strict() {
        // Review F4: "/foo " starts with '/' but breaks the byte-exact runtime match.
        let cfg = parse(
            "[[agents]]\nid = \"a\"\ntask = \"t\"\ncapabilities = [{ FsWrite = { prefix = \"/foo \" } }]\n",
        );
        assert!(check_config(&cfg, true).has_errors(), "trailing-whitespace prefix must error in --strict");
        assert!(!check_config(&cfg, false).has_errors(), "warns in default mode");
    }

    #[test]
    fn unrestricted_agent_warns() {
        // Review Codex F2: capabilities omitted → wiring unverifiable → warning (not error).
        let cfg = parse("[[agents]]\nid = \"a\"\ntask = \"t\"\n");
        let r = check_config(&cfg, true);
        assert!(!r.has_errors(), "unrestricted agent is a warning, not a boot-brick");
        assert!(r.findings.iter().any(|f| f.message.contains("unrestricted")));
    }

    #[test]
    fn inert_agent_net_grant_warns_not_errors() {
        let cfg = parse(
            r#"
[[agents]]
id = "a"
task = "t"
capabilities = [{ Net = { hosts = [], ports = [] } }]
"#,
        );
        let r = check_config(&cfg, true);
        assert!(!r.has_errors(), "inert agent-level Net is a warning, not an error");
        assert!(r.findings.iter().any(|f| f.severity == Severity::Warning));
    }
}
