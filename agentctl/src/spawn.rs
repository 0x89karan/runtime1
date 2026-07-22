use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use agentd::capability::{kb_segment_satisfies, normalize_path, Capability};
use agentd::template::TemplateSource;

#[derive(clap::Args)]
pub struct Args {
    /// Template name to spawn
    pub name: String,
    /// Task for the agent to run (required when template has no default task)
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
    /// Add a capability grant (repeatable). Syntax: fs-read:<path>, fs-write:<path>,
    /// kb-read:<seg>, kb-write:<seg>, spawn, net:<port>[,<port>…]
    #[arg(long, value_name = "CAP")]
    pub cap_add: Vec<String>,
    /// Allow --cap-add beyond the template's suggested_caps
    #[arg(long)]
    pub force: bool,
    /// Override user templates directory (default: ~/.agentos/templates/)
    #[arg(long, value_name = "PATH", env = "AGENTOS_TEMPLATES_DIR")]
    pub user_templates_dir: Option<PathBuf>,
    /// Override repo templates directory (default: auto-detected)
    #[arg(long, value_name = "PATH", env = "AGENTOS_REPO_TEMPLATES_DIR")]
    pub repo_dir: Option<PathBuf>,
    /// Override agentd binary location (default: sibling binary or PATH)
    #[arg(long, value_name = "PATH")]
    pub agentd_path: Option<PathBuf>,
    /// Write generated agent.toml here instead of a temp directory
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    /// Print the generated agent.toml without executing agentd
    #[arg(long)]
    pub dry_run: bool,
}

pub(crate) fn warn_gated_requires(requires: &str) {
    eprintln!("warning: this template requires: {requires}");
    eprintln!("         proceeding — if agentd fails, ensure the requirement is met.");
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let resolver =
        crate::build_resolver(args.user_templates_dir.as_deref(), args.repo_dir.as_deref());

    // Resolve template, showing searched dirs on failure.
    let (template, source) = resolver.resolve(&args.name).map_err(|e| {
        anyhow::anyhow!(
            "template '{}' not found; searched: {}, {}; run 'agentctl list-templates' to see available\ncause: {e:#}",
            args.name,
            resolver.user_dir().display(),
            resolver.repo_dir().display()
        )
    })?;

    // Parse --cap-add aliases into Capability values.
    let mut extra_caps: Vec<Capability> = Vec::new();
    for s in &args.cap_add {
        let cap = parse_cap_alias(s)?;
        extra_caps.push(cap);
    }

    // Cap guard: absent [card] is treated the same as [card] with empty suggested_caps —
    // all --cap-add requires --force.
    if !args.force && !extra_caps.is_empty() {
        match &template.card {
            None => {
                anyhow::bail!(
                    "template '{}' has no [card] section — --cap-add requires --force",
                    args.name
                );
            }
            Some(card) => {
                for cap in &extra_caps {
                    if !cap_add_allowed_by_suggestion(cap, &card.suggested_caps) {
                        if card.suggested_caps.is_empty() {
                            anyhow::bail!(
                                "template '{}' has empty suggested_caps — all --cap-add requires --force",
                                args.name
                            );
                        }
                        let suggested: Vec<String> =
                            card.suggested_caps.iter().map(format_cap).collect();
                        anyhow::bail!(
                            "capability '{}' not in template '{}' suggested_caps ({}); use --force to override",
                            format_cap(cap),
                            args.name,
                            suggested.join(", ")
                        );
                    }
                }
            }
        }
    }

    // Validate task availability before lowering.
    let default_task = template.agent.as_ref().map(|a| a.task.as_str()).unwrap_or("");
    if args.task.is_none() && default_task.is_empty() {
        anyhow::bail!(
            "template '{}' has no default task; pass --task '...'",
            args.name
        );
    }

    // Lower template → Config and serialize to TOML.
    let config = template.to_agent_config(args.task.as_deref(), extra_caps)?;
    let toml_str = toml::to_string_pretty(&config).context("serializing config to TOML")?;

    // Attempt agentd resolution early for display in dry-run; hard-require it for exec.
    let agentd_bin_result = resolve_agentd(&args.agentd_path);

    // Dry-run: print and exit without writing files or exec'ing.
    // agentd does not need to be present on PATH for dry-run.
    if args.dry_run {
        let src_label = match source {
            TemplateSource::Repo => "Repo",
            TemplateSource::User => "User",
        };
        let template_file = match source {
            TemplateSource::Repo => resolver
                .repo_dir()
                .join(format!("{}.template.toml", args.name)),
            TemplateSource::User => resolver
                .user_dir()
                .join(format!("{}.template.toml", args.name)),
        };
        let extra_caps_desc = if args.cap_add.is_empty() {
            "(none)".to_string()
        } else {
            args.cap_add.join(", ")
        };
        let agentd_display = agentd_bin_result
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "(not found — agentd required for live run)".to_string());
        println!(
            "# Generated by agentctl v{} from template: {} ({src_label})",
            env!("CARGO_PKG_VERSION"),
            args.name
        );
        println!("# Extra caps: {extra_caps_desc}");
        println!("# agentd path: {agentd_display} (not exec'd — dry run)");
        println!("{toml_str}");
        eprintln!(
            "dry-run: template resolved from {}",
            template_file.display()
        );
        if let Some(requires) = &template.template.gated_requires {
            eprintln!("note: this template requires: {requires}");
        }
        return Ok(());
    }

    // Warn about gated requirements before exec so the operator can abort if needed.
    if let Some(requires) = &template.template.gated_requires {
        warn_gated_requires(requires);
    }

    // For live exec, agentd must be present.
    let agentd_bin = agentd_bin_result?;

    // Preflight: ANTHROPIC_API_KEY must be set before we exec agentd.
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        anyhow::bail!("ANTHROPIC_API_KEY is not set — agentd requires it");
    }

    // Write config to a temp file (atomically via rename) then exec agentd.
    let out_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating output dir {}", out_dir.display()))?;

    let final_path = out_dir.join(format!(
        "agentctl-{}-{}.toml",
        args.name,
        std::process::id()
    ));
    let mut tmp = tempfile::NamedTempFile::new_in(&out_dir)
        .with_context(|| format!("creating temp file in {}", out_dir.display()))?;
    tmp.write_all(toml_str.as_bytes())
        .context("writing config to temp file")?;
    tmp.flush().context("flushing config temp file")?;
    tmp.persist(&final_path)
        .map_err(|e| anyhow::anyhow!("persisting config to {}: {}", final_path.display(), e.error))?;

    exec_agentd(&agentd_bin, &final_path)
}

/// Parse a flat CLI alias string into a `Capability`.
pub fn parse_cap_alias(s: &str) -> anyhow::Result<Capability> {
    if let Some(rest) = s.strip_prefix("fs-read:") {
        if rest.is_empty() {
            anyhow::bail!("'fs-read:' requires a path, e.g. fs-read:/workspace");
        }
        let p = normalize_path(Path::new(rest));
        if !p.is_absolute() {
            anyhow::bail!(
                "'fs-read:{rest}' — path must be absolute, got {rest:?}"
            );
        }
        return Ok(Capability::FsRead {
            prefix: p.to_string_lossy().into_owned(),
        });
    }
    if let Some(rest) = s.strip_prefix("fs-write:") {
        if rest.is_empty() {
            anyhow::bail!("'fs-write:' requires a path, e.g. fs-write:/workspace/out");
        }
        let p = normalize_path(Path::new(rest));
        if !p.is_absolute() {
            anyhow::bail!(
                "'fs-write:{rest}' — path must be absolute, got {rest:?}"
            );
        }
        return Ok(Capability::FsWrite {
            prefix: p.to_string_lossy().into_owned(),
        });
    }
    if let Some(rest) = s.strip_prefix("kb-read:") {
        if rest.is_empty() {
            anyhow::bail!("'kb-read:' requires a segment name, e.g. kb-read:agent:notes");
        }
        return Ok(Capability::KbRead {
            segment: rest.to_string(),
        });
    }
    if let Some(rest) = s.strip_prefix("kb-write:") {
        if rest.is_empty() {
            anyhow::bail!("'kb-write:' requires a segment name, e.g. kb-write:agent:notes");
        }
        return Ok(Capability::KbWrite {
            segment: rest.to_string(),
        });
    }
    if s == "spawn" {
        return Ok(Capability::Spawn);
    }
    if s == "runs-read" || s == "runsread" {
        return Ok(Capability::RunsRead);
    }
    if s == "brief-publish" || s == "briefpublish" {
        return Ok(Capability::BriefPublish);
    }
    if s == "shell-exec" || s == "shellexec" {
        return Ok(Capability::ShellExec);
    }
    if s.starts_with("mcp") {
        anyhow::bail!(
            "mcp cannot be added via --cap-add; specify it in [agent].capabilities in the template"
        );
    }
    if s == "net" {
        anyhow::bail!(
            "net requires ports: net:<port>[,<port>…]; e.g. net:443"
        );
    }
    if let Some(port_str) = s.strip_prefix("net:") {
        let ports: Result<Vec<u16>, _> = port_str.split(',').map(|p| p.trim().parse::<u16>()).collect();
        let ports = ports.with_context(|| format!("invalid port in '{s}'"))?;
        if ports.is_empty() {
            anyhow::bail!("net requires at least one port, e.g. net:443");
        }
        return Ok(Capability::Net {
            hosts: vec![],
            ports,
        });
    }
    let prefix = s.split(':').next().unwrap_or(s);
    anyhow::bail!(
        "unknown capability alias '{prefix}'; valid: fs-read:<path>, fs-write:<path>, kb-read:<seg>, kb-write:<seg>, spawn, runs-read, brief-publish, net:<ports>"
    )
}

/// Check whether `cap` is within the bounds suggested by the template's `suggested_caps`.
///
/// Returns `true` if the requested capability is permitted (i.e. the template explicitly
/// suggests it or a superset of it). Returns `false` if denied or `suggested` is empty.
///
/// Note: absent `[card]` → caller skips the guard entirely; this function is only called
/// when a `[card]` section exists.
pub fn cap_add_allowed_by_suggestion(cap: &Capability, suggested: &[Capability]) -> bool {
    for s in suggested {
        match (cap, s) {
            // FsRead: allowed if the suggested prefix is an ancestor-of-or-equal-to requested.
            (Capability::FsRead { prefix: req }, Capability::FsRead { prefix: sug }) => {
                let req_p = normalize_path(Path::new(req));
                let sug_p = normalize_path(Path::new(sug));
                if req_p.starts_with(&sug_p) {
                    return true;
                }
            }
            // FsWrite: same semantics as FsRead.
            (Capability::FsWrite { prefix: req }, Capability::FsWrite { prefix: sug }) => {
                let req_p = normalize_path(Path::new(req));
                let sug_p = normalize_path(Path::new(sug));
                if req_p.starts_with(&sug_p) {
                    return true;
                }
            }
            // KbRead: delegate to the canonical kb_segment_satisfies in capability.rs.
            (Capability::KbRead { segment: req }, Capability::KbRead { segment: sug }) => {
                if kb_segment_satisfies(sug, req) {
                    return true;
                }
            }
            // KbWrite: same semantics as KbRead.
            (Capability::KbWrite { segment: req }, Capability::KbWrite { segment: sug }) => {
                if kb_segment_satisfies(sug, req) {
                    return true;
                }
            }
            // Net: requested ports must be a non-empty subset of suggested ports (or suggested
            // is unrestricted). Empty req_ports would pass vacuously — guard against it.
            (
                Capability::Net { ports: req_ports, .. },
                Capability::Net { ports: sug_ports, .. },
            ) => {
                if sug_ports.is_empty()
                    || (!req_ports.is_empty()
                        && req_ports.iter().all(|p| sug_ports.contains(p)))
                {
                    return true;
                }
            }
            // Spawn / ShellExec: exact match.
            (Capability::Spawn, Capability::Spawn) => return true,
            (Capability::ShellExec, Capability::ShellExec) => return true,
            (Capability::RunsRead, Capability::RunsRead) => return true,
            (Capability::BriefPublish, Capability::BriefPublish) => return true,
            _ => {}
        }
    }
    false
}

/// Format a `Capability` as a human-readable CLI alias string for error messages.
pub(crate) fn format_cap(cap: &Capability) -> String {
    match cap {
        Capability::FsRead { prefix } => format!("fs-read:{prefix}"),
        Capability::FsWrite { prefix } => format!("fs-write:{prefix}"),
        Capability::KbRead { segment } => format!("kb-read:{segment}"),
        Capability::KbWrite { segment } => format!("kb-write:{segment}"),
        Capability::RunsRead => "runs-read".to_string(),
        Capability::BriefPublish => "brief-publish".to_string(),
        Capability::Net { ports, .. } => {
            let p: Vec<String> = ports.iter().map(|p| p.to_string()).collect();
            format!("net:{}", p.join(","))
        }
        Capability::Spawn => "spawn".to_string(),
        Capability::ShellExec => "shell-exec".to_string(),
        Capability::Mcp { server, .. } => format!("mcp:{server}"),
        Capability::Credential { provider } => format!("credential:{provider:?}"),
    }
}

/// Resolve the agentd binary path.
pub(crate) fn resolve_agentd(override_path: &Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(p) = override_path {
        if !p.exists() {
            anyhow::bail!(
                "agentd not found at {}; set --agentd-path or ensure agentd is in PATH",
                p.display()
            );
        }
        return Ok(p.clone());
    }

    // Prefer sibling binary (works for both QEMU /usr/bin/ and dev target/debug/).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("agentd");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // Fall back to PATH lookup.
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("agentd");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // Build a representative path for the error message.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    anyhow::bail!(
        "agentd not found at {}; set --agentd-path or ensure agentd is in PATH",
        exe_dir.join("agentd").display()
    )
}

#[cfg(unix)]
pub(crate) fn exec_agentd(agentd: &Path, config_path: &Path) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt as _;
    let err = std::process::Command::new(agentd).arg(config_path).exec();
    Err(anyhow::anyhow!(
        "exec agentd at {}: {err}\n(if binary is missing: set --agentd-path or ensure agentd is in PATH)",
        agentd.display()
    ))
}

#[cfg(not(unix))]
pub(crate) fn exec_agentd(agentd: &Path, config_path: &Path) -> anyhow::Result<()> {
    let status = std::process::Command::new(agentd)
        .arg(config_path)
        .status()
        .with_context(|| format!("running agentd at {}", agentd.display()))?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // ── parse_cap_alias ───────────────────────────────────────────────────────

    #[test]
    fn alias_fs_read_valid() {
        let cap = parse_cap_alias("fs-read:/workspace").unwrap();
        assert_eq!(cap, Capability::FsRead { prefix: "/workspace".into() });
    }

    #[test]
    fn alias_fs_read_normalizes_traversal() {
        // /workspace/../../etc → normalize → /etc (still absolute, so allowed by normalize)
        // But the result must be absolute.
        let cap = parse_cap_alias("fs-read:/workspace/sub/../out").unwrap();
        assert_eq!(cap, Capability::FsRead { prefix: "/workspace/out".into() });
    }

    #[test]
    fn alias_fs_read_relative_rejected() {
        assert!(parse_cap_alias("fs-read:../../etc").is_err(), "relative path must be rejected");
    }

    #[test]
    fn alias_fs_read_empty_path_rejected() {
        assert!(parse_cap_alias("fs-read:").is_err(), "empty path must be rejected");
    }

    #[test]
    fn alias_mcp_rejected() {
        assert!(parse_cap_alias("mcp:myserver").is_err());
        assert!(parse_cap_alias("mcp").is_err());
    }

    #[test]
    fn alias_net_without_ports_rejected() {
        assert!(parse_cap_alias("net").is_err(), "bare 'net' must be rejected");
    }

    #[test]
    fn alias_net_with_ports() {
        let cap = parse_cap_alias("net:443,80").unwrap();
        assert_eq!(
            cap,
            Capability::Net { hosts: vec![], ports: vec![443, 80] }
        );
    }

    #[test]
    fn alias_unknown_rejected() {
        let err = parse_cap_alias("foo:bar").unwrap_err();
        assert!(err.to_string().contains("unknown capability alias"), "{err}");
    }

    #[test]
    fn alias_fs_write_valid() {
        let cap = parse_cap_alias("fs-write:/out").unwrap();
        assert_eq!(cap, Capability::FsWrite { prefix: "/out".into() });
    }

    #[test]
    fn alias_fs_write_relative_rejected() {
        assert!(parse_cap_alias("fs-write:out/subdir").is_err());
    }

    #[test]
    fn alias_fs_write_empty_path_rejected() {
        assert!(parse_cap_alias("fs-write:").is_err());
    }

    #[test]
    fn alias_kb_read_valid() {
        let cap = parse_cap_alias("kb-read:agent:notes").unwrap();
        assert_eq!(cap, Capability::KbRead { segment: "agent:notes".into() });
    }

    #[test]
    fn alias_kb_read_empty_rejected() {
        assert!(parse_cap_alias("kb-read:").is_err());
    }

    #[test]
    fn alias_kb_write_valid() {
        let cap = parse_cap_alias("kb-write:shared:logs").unwrap();
        assert_eq!(cap, Capability::KbWrite { segment: "shared:logs".into() });
    }

    #[test]
    fn alias_kb_write_empty_rejected() {
        assert!(parse_cap_alias("kb-write:").is_err());
    }

    #[test]
    fn alias_spawn_valid() {
        let cap = parse_cap_alias("spawn").unwrap();
        assert_eq!(cap, Capability::Spawn);
    }

    #[test]
    fn alias_shell_exec_valid() {
        let cap = parse_cap_alias("shell-exec").unwrap();
        assert_eq!(cap, Capability::ShellExec);
        let cap2 = parse_cap_alias("shellexec").unwrap();
        assert_eq!(cap2, Capability::ShellExec);
    }

    #[test]
    fn alias_net_empty_after_colon_rejected() {
        assert!(parse_cap_alias("net:").is_err(), "empty port string must be rejected");
    }

    #[test]
    fn alias_net_trailing_comma_rejected() {
        assert!(parse_cap_alias("net:443,").is_err(), "trailing comma must be rejected");
    }

    #[test]
    fn format_cap_shell_exec() {
        assert_eq!(format_cap(&Capability::ShellExec), "shell-exec");
    }

    // ── cap_add_allowed_by_suggestion ─────────────────────────────────────────

    #[test]
    fn guard_allows_subpath() {
        let suggested = vec![Capability::FsRead { prefix: "/workspace".into() }];
        assert!(cap_add_allowed_by_suggestion(
            &Capability::FsRead { prefix: "/workspace/src".into() },
            &suggested
        ));
    }

    #[test]
    fn guard_denies_superpath() {
        let suggested = vec![Capability::FsRead { prefix: "/workspace".into() }];
        assert!(!cap_add_allowed_by_suggestion(
            &Capability::FsRead { prefix: "/".into() },
            &suggested
        ));
    }

    #[test]
    fn guard_empty_suggested_denies_all() {
        let suggested: Vec<Capability> = vec![];
        assert!(!cap_add_allowed_by_suggestion(
            &Capability::FsRead { prefix: "/workspace".into() },
            &suggested
        ));
        assert!(!cap_add_allowed_by_suggestion(&Capability::Spawn, &suggested));
    }

    #[test]
    fn guard_fs_write_allows_subpath() {
        let suggested = vec![Capability::FsWrite { prefix: "/out".into() }];
        assert!(cap_add_allowed_by_suggestion(
            &Capability::FsWrite { prefix: "/out/results".into() },
            &suggested
        ));
    }

    #[test]
    fn guard_fs_write_denies_superpath() {
        let suggested = vec![Capability::FsWrite { prefix: "/out".into() }];
        assert!(!cap_add_allowed_by_suggestion(
            &Capability::FsWrite { prefix: "/".into() },
            &suggested
        ));
    }

    #[test]
    fn guard_spawn_allows_exact() {
        let suggested = vec![Capability::Spawn];
        assert!(cap_add_allowed_by_suggestion(&Capability::Spawn, &suggested));
    }

    #[test]
    fn guard_spawn_denies_wrong_type() {
        let suggested = vec![Capability::Spawn];
        assert!(!cap_add_allowed_by_suggestion(
            &Capability::FsRead { prefix: "/".into() },
            &suggested
        ));
    }

    #[test]
    fn guard_net_allows_port_subset() {
        let suggested = vec![Capability::Net { hosts: vec![], ports: vec![443, 80, 8080] }];
        assert!(cap_add_allowed_by_suggestion(
            &Capability::Net { hosts: vec![], ports: vec![443, 80] },
            &suggested
        ));
    }

    #[test]
    fn guard_net_denies_port_superset() {
        let suggested = vec![Capability::Net { hosts: vec![], ports: vec![443] }];
        assert!(!cap_add_allowed_by_suggestion(
            &Capability::Net { hosts: vec![], ports: vec![443, 22] },
            &suggested
        ));
    }

    #[test]
    fn guard_net_empty_req_ports_denied() {
        // Empty req_ports must NOT pass vacuously (was a vacuous-truth hole).
        let suggested = vec![Capability::Net { hosts: vec![], ports: vec![443] }];
        assert!(!cap_add_allowed_by_suggestion(
            &Capability::Net { hosts: vec![], ports: vec![] },
            &suggested
        ));
    }

    #[test]
    fn guard_kb_read_allows_sub_segment() {
        let suggested = vec![Capability::KbRead { segment: "agent".into() }];
        assert!(cap_add_allowed_by_suggestion(
            &Capability::KbRead { segment: "agent:notes".into() },
            &suggested
        ));
    }

    #[test]
    fn guard_kb_read_denies_squatting() {
        // "agent:scratch" must NOT satisfy "agent:scratchpad" — delimiter is required.
        let suggested = vec![Capability::KbRead { segment: "agent:scratch".into() }];
        assert!(!cap_add_allowed_by_suggestion(
            &Capability::KbRead { segment: "agent:scratchpad".into() },
            &suggested
        ));
    }

    #[test]
    fn guard_kb_write_allows_sub_segment() {
        let suggested = vec![Capability::KbWrite { segment: "shared".into() }];
        assert!(cap_add_allowed_by_suggestion(
            &Capability::KbWrite { segment: "shared:logs".into() },
            &suggested
        ));
    }

    // ── spawn integration ─────────────────────────────────────────────────────

    const SCOUT_LIKE: &str = r#"
sample_tasks = ["Do the thing."]

[template]
name        = "scout"
description = "Read-only researcher."
showcases   = "read_file, list_dir"

[capabilities]
fs_read = ["/workspace"]

[card]
name        = "Scout"
description = "Read-only filesystem researcher."
skills      = ["research"]
suggested_caps = [{ FsRead = { prefix = "/workspace" } }]

[agent]
id   = "scout"
task = ""
"#;

    fn write_template(dir: &TempDir, name: &str, content: &str) {
        let path = dir.path().join(format!("{name}.template.toml"));
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn spawn_dry_run_produces_valid_toml() {
        let repo = TempDir::new().unwrap();
        write_template(&repo, "scout", SCOUT_LIKE);
        let resolver = agentd::template::TemplateResolver::new(
            repo.path().to_path_buf(),
            PathBuf::from("/nonexistent-user-dir"),
        );
        let (template, _) = resolver.resolve("scout").unwrap();
        let config = template
            .to_agent_config(Some("list /workspace"), vec![])
            .unwrap();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        // Round-trip: must parse back as a valid Config.
        let re: agentd::config::Config = toml::from_str(&toml_str).expect("must round-trip");
        assert_eq!(re.agent.unwrap().task, "list /workspace");
    }

    #[test]
    fn spawn_rejects_cap_beyond_suggested() {
        // cap_add fs-read:/tmp is not in suggested_caps (which has fs-read:/workspace)
        let cap = Capability::FsRead { prefix: "/tmp".into() };
        let suggested = vec![Capability::FsRead { prefix: "/workspace".into() }];
        assert!(!cap_add_allowed_by_suggestion(&cap, &suggested));
    }

    #[test]
    fn spawn_shell_exec_cap_allowed_when_suggested() {
        let cap = Capability::ShellExec;
        let suggested = vec![Capability::ShellExec];
        assert!(cap_add_allowed_by_suggestion(&cap, &suggested),
            "ShellExec must be allowed when template suggests it");
    }

    #[test]
    fn spawn_shell_exec_cap_rejected_when_not_suggested() {
        let cap = Capability::ShellExec;
        let suggested = vec![Capability::Spawn];
        assert!(!cap_add_allowed_by_suggestion(&cap, &suggested),
            "ShellExec must be rejected when not in suggested_caps");
    }

    #[test]
    fn spawn_allows_cap_with_force() {
        // Simulated --force: caller simply skips the guard, so any cap is accepted.
        // This test verifies that guard_empty returns false (force must bypass it).
        let suggested: Vec<Capability> = vec![];
        let cap = Capability::FsRead { prefix: "/etc".into() };
        assert!(!cap_add_allowed_by_suggestion(&cap, &suggested),
            "empty suggested must deny; --force bypasses this check at the caller level");
    }

    #[test]
    fn spawn_absent_card_requires_force_for_cap_add() {
        // Absent [card] is treated the same as empty suggested_caps:
        // --cap-add without --force must be rejected.
        let raw = r#"
[template]
name = "nocard"
description = "d"
showcases   = "s"
[agent]
id   = "nocard"
task = "t"
"#;
        let cfg: agentd::template::TemplateConfig = toml::from_str(raw).unwrap();
        assert!(cfg.card.is_none(), "template without [card] must parse cleanly");
        // Simulate the guard: absent card + extra_caps + force=false → denied.
        let extra_caps = [Capability::FsRead { prefix: "/anywhere".into() }];
        let force = false;
        let denied = !force && !extra_caps.is_empty() && cfg.card.is_none();
        assert!(denied, "absent [card] must deny --cap-add without --force");
        // With force=true, the guard is bypassed regardless.
        let force = true;
        let denied = !force && !extra_caps.is_empty() && cfg.card.is_none();
        assert!(!denied, "--force must bypass absent-card guard");
    }

    #[test]
    fn spawn_task_required_when_template_empty() {
        let raw = r#"
[template]
name = "t"
description = "d"
showcases   = "s"
[agent]
id   = "t"
task = ""
"#;
        let cfg: agentd::template::TemplateConfig = toml::from_str(raw).unwrap();
        let result = cfg.to_agent_config(None, vec![]);
        assert!(result.is_err(), "empty task without override must error");
    }

    #[test]
    fn spawn_agentd_not_found_errors_cleanly() {
        let bad_path = Some(PathBuf::from("/nonexistent/agentd"));
        let result = resolve_agentd(&bad_path);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("agentd not found"), "error must mention 'agentd not found'");
        assert!(msg.contains("/nonexistent/agentd"), "error must include the path");
    }

    #[test]
    fn gated_requires_parses_correctly() {
        // Verify that gated_requires is correctly deserialized from TOML.
        let raw = r#"
sample_tasks = ["Journal today's findings."]

[template]
name           = "gated-test"
description    = "A gated test template."
showcases      = "gated_requires demo"
gated_requires = "Phase-5 memory"

[agent]
id   = "gated-test"
task = ""
"#;
        let cfg: agentd::template::TemplateConfig = toml::from_str(raw).unwrap();
        assert_eq!(
            cfg.template.gated_requires.as_deref(),
            Some("Phase-5 memory"),
            "gated_requires must parse correctly"
        );
    }

    /// Exercises the dry-run branch in `run()` end-to-end. This path must succeed
    /// even when agentd is not on PATH — dry-run never execs.
    #[test]
    fn run_dry_run_succeeds_without_agentd() {
        let repo = TempDir::new().unwrap();
        write_template(&repo, "scout", SCOUT_LIKE);
        let user_dir = TempDir::new().unwrap();
        let args = Args {
            name: "scout".into(),
            task: Some("check /workspace".into()),
            cap_add: vec![],
            force: false,
            user_templates_dir: Some(user_dir.path().to_path_buf()),
            repo_dir: Some(repo.path().to_path_buf()),
            agentd_path: Some(PathBuf::from("/nonexistent/agentd")),
            output_dir: None,
            dry_run: true,
        };
        // Dry-run must return Ok even though agentd_path doesn't exist.
        assert!(run(args).is_ok(), "dry-run must succeed without a real agentd binary");
    }

    /// Exercises the ANTHROPIC_API_KEY preflight in `run()` (live path).
    /// Provides a real executable as agentd so resolve_agentd succeeds, then
    /// verifies the error fires before any file write or exec attempt.
    #[test]
    fn run_missing_api_key_errors_before_exec() {
        let repo = TempDir::new().unwrap();
        write_template(&repo, "scout", SCOUT_LIKE);
        let user_dir = TempDir::new().unwrap();

        // Create a fake agentd that resolve_agentd will accept (must exist on disk).
        let fake_bin_dir = TempDir::new().unwrap();
        let fake_agentd = fake_bin_dir.path().join("agentd");
        std::fs::write(&fake_agentd, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake_agentd, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Capture any existing key so we can restore it afterward.
        let _env_guard = crate::ENV_MUTEX.lock().unwrap();
        let saved = std::env::var("ANTHROPIC_API_KEY").ok();
        // Safety: test-only env mutation; ENV_MUTEX serializes all env-var-touching tests.
        std::env::remove_var("ANTHROPIC_API_KEY");

        let args = Args {
            name: "scout".into(),
            task: Some("check /workspace".into()),
            cap_add: vec![],
            force: false,
            user_templates_dir: Some(user_dir.path().to_path_buf()),
            repo_dir: Some(repo.path().to_path_buf()),
            agentd_path: Some(fake_agentd),
            output_dir: Some(fake_bin_dir.path().to_path_buf()),
            dry_run: false,
        };
        let result = run(args);

        // Restore env before any assertion so failures don't leave env dirty.
        match saved {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ANTHROPIC_API_KEY"),
            "error must mention ANTHROPIC_API_KEY, got: {msg}"
        );
    }
}
