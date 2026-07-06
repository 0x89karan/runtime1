use agentd::capability::Capability;
use agentd::template::{TemplateEntry, TemplateSource};

/// A loaded template entry with its suggested capabilities.
#[derive(Debug, Clone)]
pub struct SpawnTemplate {
    pub name:           String,
    pub source:         TemplateSource,
    pub description:    String,
    pub showcases:      String,
    /// Pre-loaded from `[card].suggested_caps`.  Empty for templates without a `[card]`.
    pub suggested_caps: Vec<Capability>,
    /// Example task strings from `sample_tasks`. Used to pre-fill the task field.
    pub sample_tasks:   Vec<String>,
}

/// Load all templates from the default resolver, returning them sorted by name.
///
/// Calls `resolver.resolve()` for each entry to capture `suggested_caps` from
/// `[card]`.  Entries that fail to resolve are skipped (not emitted as ghost
/// entries with empty caps); errors are accumulated and returned as `load_error`.
pub fn load_spawn_templates() -> (Vec<SpawnTemplate>, Option<String>) {
    let resolver = crate::build_resolver(None, None);
    let entries = match resolver.list() {
        Ok(e)  => e,
        Err(e) => return (vec![], Some(format!("failed to list templates: {e:#}"))),
    };
    let mut out = Vec::with_capacity(entries.len());
    let mut resolve_errors: Vec<String> = Vec::new();
    for TemplateEntry { name, source, description, showcases, sample_tasks: _ } in entries {
        let (suggested_caps, full_sample_tasks) = match resolver.resolve(&name) {
            Ok((cfg, _)) => {
                let caps = cfg.card.map(|c| c.suggested_caps).unwrap_or_default();
                (caps, cfg.sample_tasks)
            }
            Err(e) => {
                resolve_errors.push(format!("{name}: {e:#}"));
                continue;
            }
        };
        out.push(SpawnTemplate { name, source, description, showcases, suggested_caps, sample_tasks: full_sample_tasks });
    }
    let load_error = if resolve_errors.is_empty() {
        None
    } else {
        Some(format!("failed to resolve: {}", resolve_errors.join(", ")))
    };
    (out, load_error)
}

/// Format a `Capability` for display in the Spawn view cap-toggle list.
///
/// Uses struct-form display ("FsRead {/workspace}") consistent with the
/// INTERFACE.md mockup and the System/Agent-detail views.
pub fn display_cap(cap: &Capability) -> String {
    match cap {
        Capability::FsRead  { prefix }   => format!("FsRead  {{{prefix}}}"),
        Capability::FsWrite { prefix }   => format!("FsWrite {{{prefix}}}"),
        Capability::KbRead  { segment }  => format!("KbRead  {{{segment}}}"),
        Capability::KbWrite { segment }  => format!("KbWrite {{{segment}}}"),
        Capability::Net { ports, .. } => {
            let p: Vec<String> = ports.iter().map(|p| p.to_string()).collect();
            if p.is_empty() { "Net".to_string() } else { format!("Net     {{ports: {}}}", p.join(",")) }
        }
        Capability::Spawn => "Spawn".to_string(),
        Capability::ShellExec => "ShellExec".to_string(),
        Capability::Mcp { server, .. } => format!("Mcp     {{server: {server}}}"),
        Capability::Credential { provider } => format!("Credential {{{provider:?}}}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentd::capability::Capability;

    #[test]
    fn display_cap_fs_read() {
        let s = display_cap(&Capability::FsRead { prefix: "/workspace".into() });
        assert!(s.contains("FsRead"), "must show FsRead");
        assert!(s.contains("/workspace"), "must show path");
    }

    #[test]
    fn display_cap_fs_write() {
        let s = display_cap(&Capability::FsWrite { prefix: "/out".into() });
        assert!(s.contains("FsWrite"));
        assert!(s.contains("/out"));
    }

    #[test]
    fn display_cap_kb_read() {
        let s = display_cap(&Capability::KbRead { segment: "agent:notes".into() });
        assert!(s.contains("KbRead"));
        assert!(s.contains("agent:notes"));
    }

    #[test]
    fn display_cap_kb_write() {
        let s = display_cap(&Capability::KbWrite { segment: "project:logs".into() });
        assert!(s.contains("KbWrite"));
        assert!(s.contains("project:logs"));
    }

    #[test]
    fn display_cap_spawn() {
        let s = display_cap(&Capability::Spawn);
        assert_eq!(s, "Spawn");
    }

    #[test]
    fn display_cap_net_with_ports() {
        let s = display_cap(&Capability::Net { hosts: vec![], ports: vec![443, 80] });
        assert!(s.contains("Net"));
        assert!(s.contains("443"));
        assert!(s.contains("80"));
    }

    #[test]
    fn display_cap_net_empty_ports() {
        let s = display_cap(&Capability::Net { hosts: vec![], ports: vec![] });
        assert_eq!(s, "Net");
    }

    #[test]
    fn display_cap_mcp() {
        let s = display_cap(&Capability::Mcp { server: "fs".into(), tools: vec![] });
        assert!(s.contains("Mcp"));
        assert!(s.contains("fs"));
    }

    #[test]
    fn display_cap_shell_exec() {
        assert_eq!(display_cap(&Capability::ShellExec), "ShellExec");
    }
}
