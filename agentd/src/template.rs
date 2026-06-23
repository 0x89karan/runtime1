use std::path::PathBuf;

use anyhow::Context as _;
use serde::Deserialize;

use crate::capability::Capability;
use crate::config::{
    AgentConfig, Config, MemoryConfig, ModelConfig, SchedulerConfig, ToolsConfig,
};

/// `[template]` section — required in every template file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateMeta {
    /// Bare name used by `agentctl spawn <name>`. Must match the filename stem.
    pub name: String,
    /// Human-readable description of what the agent does.
    pub description: String,
    /// What AgentOS-specific feature this template showcases.
    pub showcases: String,
    /// Human-readable prerequisite for gated templates.
    ///
    /// When set, `agentctl spawn` prints a warning before exec so the operator
    /// knows the template requires a specific runtime dependency (e.g. Phase-5
    /// memory, gVisor, event-trigger surface). `None` = unconditionally runnable.
    #[serde(default)]
    pub gated_requires: Option<String>,
}

/// One entry in `[capabilities].mcp`. Each becomes `Capability::Mcp { server, tools }`.
///
/// `tools = []` grants access to all tools on the named server.
#[derive(Debug, Deserialize, Default)]
pub struct McpCapEntry {
    pub server: String,
    #[serde(default)]
    pub tools: Vec<String>,
}

/// `[capabilities]` section — flat sugar format, deny-by-default.
///
/// Maps to `Capability` variants via `to_capability_vec()`. Missing `[capabilities]`
/// in a template becomes `Some([])` (deny-all) in the lowered `AgentConfig`, never `None`.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TemplateCapabilities {
    /// Paths the agent may read (absolute). Each becomes `FsRead { prefix }`.
    #[serde(default)]
    pub fs_read: Vec<String>,
    /// Paths the agent may write (absolute). Each becomes `FsWrite { prefix }`.
    #[serde(default)]
    pub fs_write: Vec<String>,
    /// KB segments the agent may read. Each becomes `KbRead { segment }`.
    #[serde(default)]
    pub kb_read: Vec<String>,
    /// KB segments the agent may write. Each becomes `KbWrite { segment }`.
    #[serde(default)]
    pub kb_write: Vec<String>,
    /// TCP ports for outgoing connections. Combined with `net_hosts` → `Net { hosts, ports }`.
    /// Either `net_ports` or `net_hosts` alone is sufficient to emit a `Net` capability.
    /// Both empty → no `Net` capability added.
    #[serde(default)]
    pub net_ports: Vec<u16>,
    /// Advisory hosts for outgoing connections (not kernel-enforced).
    /// Defaults to empty (any host) when `net_ports` is set.
    #[serde(default)]
    pub net_hosts: Vec<String>,
    /// When `true`, adds `Capability::Spawn`.
    #[serde(default)]
    pub spawn: bool,
    /// MCP servers the agent may use. Each becomes `Capability::Mcp { server, tools }`.
    #[serde(default)]
    pub mcp: Vec<McpCapEntry>,
}

impl TemplateCapabilities {
    /// Convert the flat template sugar to a `Vec<Capability>`.
    pub fn to_capability_vec(&self) -> Vec<Capability> {
        let mut caps = Vec::new();
        for p in &self.fs_read {
            caps.push(Capability::FsRead { prefix: p.clone() });
        }
        for p in &self.fs_write {
            caps.push(Capability::FsWrite { prefix: p.clone() });
        }
        for s in &self.kb_read {
            caps.push(Capability::KbRead { segment: s.clone() });
        }
        for s in &self.kb_write {
            caps.push(Capability::KbWrite { segment: s.clone() });
        }
        if !self.net_ports.is_empty() || !self.net_hosts.is_empty() {
            caps.push(Capability::Net {
                hosts: self.net_hosts.clone(),
                ports: self.net_ports.clone(),
            });
        }
        if self.spawn {
            caps.push(Capability::Spawn);
        }
        for e in &self.mcp {
            caps.push(Capability::Mcp { server: e.server.clone(), tools: e.tools.clone() });
        }
        caps
    }
}

/// `[card]` section — catalogue metadata only. NOT copied to runtime `AgentConfig`.
///
/// Runtime `AgentCard` is always derived from `[agent]` fields (id, name, description,
/// skills). Use `[card]` to describe the template in catalogue listings.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateCard {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub skills: Vec<String>,
    /// Capabilities that `agentctl spawn --cap-add` is allowed to grant without `--force`.
    /// Uses the real `Capability` type (single vocabulary — no alias strings stored here).
    /// TOML syntax: `suggested_caps = [{ FsRead = { prefix = "/workspace" } }]`
    /// Absent field (default) = no guard applied (legacy template compat).
    /// Empty vec = all `--cap-add` requires `--force`.
    #[serde(default)]
    pub suggested_caps: Vec<Capability>,
}

/// A parsed `*.template.toml` file.
///
/// Template files are a superset of `agent.toml`. Use `to_agent_config()` to lower
/// a single-agent template to a plain `Config` that `agentd` can load.
///
/// Note: `sample_tasks` must appear as a **top-level key** in the TOML file,
/// before any table header (`[model]`, `[card]`, etc.). Keys placed after a
/// table header belong to that table.
///
/// `deny_unknown_fields` is intentionally omitted — `TemplateConfig` accepts both
/// `[agent]` and `[[agents]]` forms; serde would reject the unused key otherwise
/// (same reason as `Config` in config.rs).
#[derive(Debug, Deserialize)]
pub struct TemplateConfig {
    pub template: TemplateMeta,
    /// Suggested capabilities. `None` (field absent) → deny-all (`Some([])`) in lowered config.
    pub capabilities: Option<TemplateCapabilities>,
    /// Catalogue metadata. Stripped by `to_agent_config()`; not visible at runtime.
    pub card: Option<TemplateCard>,
    /// Example task strings shown in `agentctl list`. Top-level key only.
    #[serde(default)]
    pub sample_tasks: Vec<String>,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Single-agent form. `None` for multi-agent templates.
    pub agent: Option<AgentConfig>,
    /// Multi-agent form.
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
}

impl TemplateConfig {
    /// Lower a single-agent template to a plain `Config` ready for `agentd`.
    ///
    /// Steps:
    /// 1. Errors for multi-agent templates — use the `agents` vec directly.
    /// 2. Applies `task` override (errors if `None` and template task is empty).
    /// 3. Maps `[capabilities]` → `Vec<Capability>`; absent `[capabilities]` → deny-all (`Some([])`).
    /// 4. Appends `extra_caps` (unchecked — caller owns enforcement of cap bounds vs template).
    /// 5. Returns `Config` with template-only keys (`[template]`, `[card]`, `sample_tasks`) stripped.
    pub fn to_agent_config(
        &self,
        task: Option<&str>,
        extra_caps: Vec<Capability>,
    ) -> anyhow::Result<Config> {
        anyhow::ensure!(
            self.agents.is_empty(),
            "to_agent_config() requires a single-agent template; \
             use the agents vec directly for multi-agent templates"
        );

        let mut agent = self
            .agent
            .clone()
            .ok_or_else(|| anyhow::anyhow!("template has no [agent] section"))?;

        if let Some(t) = task {
            agent.task = t.to_string();
        }
        anyhow::ensure!(
            !agent.task.is_empty(),
            "template task is empty; pass a task string via to_agent_config(Some(\"...\"), ...)"
        );

        // Validate absolute paths in [capabilities] before lowering.
        if let Some(tc) = &self.capabilities {
            for p in &tc.fs_read {
                anyhow::ensure!(
                    std::path::Path::new(p).is_absolute(),
                    "capabilities.fs_read path {p:?} must be absolute"
                );
            }
            for p in &tc.fs_write {
                anyhow::ensure!(
                    std::path::Path::new(p).is_absolute(),
                    "capabilities.fs_write path {p:?} must be absolute"
                );
            }
        }
        // Missing [capabilities] → deny-all (Some([])), never None (unrestricted).
        let mut caps = match &self.capabilities {
            Some(tc) => tc.to_capability_vec(),
            None => vec![],
        };
        // Preserve Mcp grants (or other Capability variants) placed directly in
        // [agent].capabilities — they have no sugar form in [capabilities].
        if let Some(existing) = agent.capabilities.take() {
            caps.extend(existing);
        }
        caps.extend(extra_caps);
        agent.capabilities = Some(caps);

        Ok(Config {
            agent: Some(agent),
            agents: vec![],
            model: self.model.clone(),
            tools: self.tools.clone(),
            scheduler: self.scheduler.clone(),
            memory: self.memory.clone(),
            log_path: None,
            egress: Default::default(),
        })
    }
}

/// Where a resolved template was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    Repo,
    User,
}

/// An entry in the template catalogue list.
#[derive(Debug, Clone)]
pub struct TemplateEntry {
    pub name: String,
    pub description: String,
    pub source: TemplateSource,
    pub showcases: String,
    /// Example task strings from `sample_tasks` in the template file.
    pub sample_tasks: Vec<String>,
}

/// Resolves `*.template.toml` files from user and repo directories.
///
/// Precedence: user dir wins over repo dir on name collision.
pub struct TemplateResolver {
    repo_dir: PathBuf,
    user_dir: PathBuf,
}

impl TemplateResolver {
    /// Explicit constructor. `user_dir` may not exist; missing dirs are treated as empty.
    pub fn new(repo_dir: PathBuf, user_dir: PathBuf) -> Self {
        Self { repo_dir, user_dir }
    }

    /// Convenience constructor: `user_dir = $HOME/.agentos/templates/`.
    /// Returns `Err` if `$HOME` is unset. A missing `user_dir` on disk is not an error.
    pub fn from_env(repo_dir: PathBuf) -> anyhow::Result<Self> {
        let home = std::env::var("HOME")
            .map_err(|_| anyhow::anyhow!("$HOME is not set; cannot locate ~/.agentos/templates/"))?;
        let user_dir = PathBuf::from(home).join(".agentos").join("templates");
        Ok(Self { repo_dir, user_dir })
    }

    /// Resolve a template by name.
    ///
    /// Searches `user_dir` first, then `repo_dir`. Returns the first found.
    /// Rejects names containing `/` or `..` to prevent path traversal.
    pub fn resolve(&self, name: &str) -> anyhow::Result<(TemplateConfig, TemplateSource)> {
        anyhow::ensure!(
            !name.contains('/') && !name.contains("..") && !name.is_empty(),
            "invalid template name {:?}: must not contain '/', '..', or be empty",
            name
        );

        let filename = format!("{name}.template.toml");
        let user_path = self.user_dir.join(&filename);
        let repo_path = self.repo_dir.join(&filename);

        if user_path.exists() {
            let cfg = Self::parse_file(&user_path)?;
            anyhow::ensure!(
                cfg.template.name == name,
                "template file {name:?} declares name {:?}; filename stem must match [template].name",
                cfg.template.name
            );
            return Ok((cfg, TemplateSource::User));
        }
        if repo_path.exists() {
            let cfg = Self::parse_file(&repo_path)?;
            anyhow::ensure!(
                cfg.template.name == name,
                "template file {name:?} declares name {:?}; filename stem must match [template].name",
                cfg.template.name
            );
            return Ok((cfg, TemplateSource::Repo));
        }

        anyhow::bail!(
            "template {name:?} not found; searched {:?} and {:?}",
            user_path,
            repo_path
        )
    }

    /// List all templates from both dirs. Deduplicates by name (user dir wins).
    pub fn list(&self) -> anyhow::Result<Vec<TemplateEntry>> {
        let mut entries: Vec<TemplateEntry> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // User dir first so it wins deduplication.
        for (dir, source) in [
            (self.user_dir.as_path(), TemplateSource::User),
            (self.repo_dir.as_path(), TemplateSource::Repo),
        ] {
            let read_dir = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(_) => continue, // missing dir is not an error
            };
            let mut dir_entries: Vec<_> = read_dir.filter_map(|e| e.ok()).collect();
            dir_entries.sort_by_key(|e| e.file_name());
            for entry in dir_entries {
                let path = entry.path();
                let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !fname.ends_with(".template.toml") {
                    continue;
                }
                let cfg = match Self::parse_file(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("skipping {path:?}: parse error — {e:#}");
                        continue;
                    }
                };
                let expected_name = fname.strip_suffix(".template.toml").unwrap_or("");
                if cfg.template.name != expected_name {
                    tracing::warn!(
                        "skipping {path:?}: [template].name {:?} does not match filename stem {:?}",
                        cfg.template.name,
                        expected_name
                    );
                    continue;
                }
                let name = cfg.template.name.clone();
                if seen.contains(&name) {
                    continue;
                }
                seen.insert(name.clone());
                entries.push(TemplateEntry {
                    name,
                    description: cfg.template.description.clone(),
                    source: source.clone(),
                    showcases: cfg.template.showcases.clone(),
                    sample_tasks: cfg.sample_tasks.clone(),
                });
            }
        }
        Ok(entries)
    }

    pub fn repo_dir(&self) -> &std::path::Path {
        &self.repo_dir
    }

    pub fn user_dir(&self) -> &std::path::Path {
        &self.user_dir
    }

    fn parse_file(path: &std::path::Path) -> anyhow::Result<TemplateConfig> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading template file {path:?}"))?;
        toml::from_str(&content).with_context(|| format!("parsing template {path:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_template(dir: &TempDir, name: &str, content: &str) {
        let path = dir.path().join(format!("{name}.template.toml"));
        std::fs::write(path, content).unwrap();
    }

    const MINIMAL_TEMPLATE: &str = r#"
sample_tasks = ["Do the thing."]

[template]
name        = "minimal"
description = "A minimal test template."
showcases   = "Basic parsing."

[model]
provider   = "anthropic"
model      = "claude-haiku-4-5-20251001"
max_tokens = 512

[agent]
id           = "minimal"
task         = "test task"
max_turns    = 5
token_budget = 10000
"#;

    // ── TemplateMeta ────────────────────────────────────────────────────────────

    #[test]
    fn template_meta_parses() {
        let cfg: TemplateConfig = toml::from_str(MINIMAL_TEMPLATE).unwrap();
        assert_eq!(cfg.template.name, "minimal");
        assert_eq!(cfg.template.description, "A minimal test template.");
        assert_eq!(cfg.template.showcases, "Basic parsing.");
    }

    // ── TemplateCapabilities ────────────────────────────────────────────────────

    #[test]
    fn template_capabilities_map_to_capability_vec() {
        let raw = r#"
[template]
name = "t"
description = "d"
showcases = "s"

[capabilities]
fs_read   = ["/workspace"]
fs_write  = ["/workspace/out"]
kb_read   = ["agent/kb"]
kb_write  = ["agent/kb"]
net_ports = [443]
net_hosts = []
spawn     = true

[agent]
id   = "t"
task = "t"
"#;
        let cfg: TemplateConfig = toml::from_str(raw).unwrap();
        let caps = cfg.capabilities.as_ref().unwrap().to_capability_vec();
        assert!(caps.contains(&Capability::FsRead { prefix: "/workspace".into() }));
        assert!(caps.contains(&Capability::FsWrite { prefix: "/workspace/out".into() }));
        assert!(caps.contains(&Capability::KbRead { segment: "agent/kb".into() }));
        assert!(caps.contains(&Capability::KbWrite { segment: "agent/kb".into() }));
        assert!(caps.contains(&Capability::Net {
            hosts: vec![],
            ports: vec![443],
        }));
        assert!(caps.contains(&Capability::Spawn));
    }

    #[test]
    fn net_capability_hosts_explicitly_empty() {
        let raw = r#"
[template]
name = "t"
description = "d"
showcases = "s"

[capabilities]
net_ports = [443]

[agent]
id   = "t"
task = "t"
"#;
        let cfg: TemplateConfig = toml::from_str(raw).unwrap();
        let caps = cfg.capabilities.as_ref().unwrap().to_capability_vec();
        let net = caps.iter().find(|c| matches!(c, Capability::Net { .. })).unwrap();
        assert_eq!(
            net,
            &Capability::Net { hosts: vec![], ports: vec![443] },
            "net_hosts defaults to empty vec; both sides of conversion must be explicit"
        );
    }

    // ── TemplateCard ─────────────────────────────────────────────────────────────

    #[test]
    fn template_card_parses() {
        let raw = r#"
[template]
name = "t"
description = "d"
showcases = "s"

[card]
name        = "MyAgent"
description = "A useful agent."
skills      = ["research"]

[agent]
id   = "t"
task = "t"
"#;
        let cfg: TemplateConfig = toml::from_str(raw).unwrap();
        let card = cfg.card.unwrap();
        assert_eq!(card.name, "MyAgent");
        assert_eq!(card.skills, vec!["research"]);
    }

    // ── sample_tasks ─────────────────────────────────────────────────────────────

    #[test]
    fn sample_tasks_at_top_level() {
        let raw = r#"
sample_tasks = ["Task one.", "Task two."]

[template]
name = "t"
description = "d"
showcases = "s"

[card]
name        = "T"
description = "d"

[agent]
id   = "t"
task = "t"
"#;
        let cfg: TemplateConfig = toml::from_str(raw).unwrap();
        assert_eq!(
            cfg.sample_tasks.len(),
            2,
            "sample_tasks must parse at top level, not under [card]"
        );
        assert_eq!(cfg.sample_tasks[0], "Task one.");
    }

    // ── to_agent_config ───────────────────────────────────────────────────────────

    #[test]
    fn to_agent_config_strips_template_keys() {
        let cfg: TemplateConfig = toml::from_str(MINIMAL_TEMPLATE).unwrap();
        let config = cfg.to_agent_config(None, vec![]).unwrap();

        // Template-only keys are gone; Config fields are present.
        assert!(config.agent.is_some());
        assert_eq!(config.model.model, "claude-haiku-4-5-20251001");

        // Round-trip: Config is now Serialize, so we can verify it re-parses cleanly.
        let toml_str = toml::to_string(&config).expect("Config must be serializable");
        let re: Config = toml::from_str(&toml_str).expect("re-parse must succeed");
        assert_eq!(re.agent.unwrap().id, "minimal");
    }

    #[test]
    fn to_agent_config_applies_task_override() {
        let cfg: TemplateConfig = toml::from_str(MINIMAL_TEMPLATE).unwrap();
        let config = cfg.to_agent_config(Some("my specific task"), vec![]).unwrap();
        assert_eq!(config.agent.unwrap().task, "my specific task");
    }

    #[test]
    fn to_agent_config_missing_caps_becomes_deny_all() {
        // Template has no [capabilities] section → agent.capabilities = Some([]) not None.
        let cfg: TemplateConfig = toml::from_str(MINIMAL_TEMPLATE).unwrap();
        assert!(cfg.capabilities.is_none(), "precondition: template has no [capabilities]");
        let config = cfg.to_agent_config(None, vec![]).unwrap();
        let agent = config.agent.unwrap();
        assert_eq!(
            agent.capabilities,
            Some(vec![]),
            "missing [capabilities] must produce deny-all Some([]), never None (unrestricted)"
        );
    }

    #[test]
    fn to_agent_config_empty_task_errors() {
        let raw = r#"
[template]
name = "t"
description = "d"
showcases = "s"

[agent]
id   = "t"
task = ""
"#;
        let cfg: TemplateConfig = toml::from_str(raw).unwrap();
        let result = cfg.to_agent_config(None, vec![]);
        assert!(result.is_err(), "to_agent_config(None) with empty task must return Err");
        assert!(result.unwrap_err().to_string().contains("task is empty"));
    }

    #[test]
    fn to_agent_config_multi_agent_template_errors() {
        let raw = r#"
[template]
name = "t"
description = "d"
showcases = "s"

[[agents]]
id   = "a"
task = "task a"

[[agents]]
id   = "b"
task = "task b"
"#;
        let cfg: TemplateConfig = toml::from_str(raw).unwrap();
        let result = cfg.to_agent_config(Some("task"), vec![]);
        assert!(result.is_err(), "multi-agent template must return Err from to_agent_config");
        assert!(result.unwrap_err().to_string().contains("single-agent"));
    }

    #[test]
    fn to_agent_config_merges_extra_caps() {
        let cfg: TemplateConfig = toml::from_str(MINIMAL_TEMPLATE).unwrap();
        let extra = vec![Capability::FsRead { prefix: "/extra".into() }];
        let config = cfg.to_agent_config(None, extra).unwrap();
        let caps = config.agent.unwrap().capabilities.unwrap();
        assert!(
            caps.contains(&Capability::FsRead { prefix: "/extra".into() }),
            "extra_caps must appear in output capabilities"
        );
    }

    #[test]
    fn to_agent_config_no_agent_section_errors() {
        let raw = r#"
[template]
name = "t"
description = "d"
showcases = "s"
"#;
        let cfg: TemplateConfig = toml::from_str(raw).unwrap();
        let result = cfg.to_agent_config(None, vec![]);
        assert!(result.is_err(), "missing [agent] section must return Err");
        assert!(
            result.unwrap_err().to_string().contains("[agent] section"),
            "error must mention '[agent] section'"
        );
    }

    #[test]
    fn to_agent_config_empty_string_override_errors() {
        // Passing Some("") must override the task to empty and trigger the empty-task error.
        let cfg: TemplateConfig = toml::from_str(MINIMAL_TEMPLATE).unwrap();
        let result = cfg.to_agent_config(Some(""), vec![]);
        assert!(result.is_err(), "task override with empty string must return Err");
        assert!(result.unwrap_err().to_string().contains("task is empty"));
    }

    #[test]
    fn to_agent_config_present_empty_caps_is_deny_all() {
        // A [capabilities] section that exists but has no entries must also yield deny-all.
        let raw = r#"
[template]
name = "t"
description = "d"
showcases = "s"

[capabilities]

[agent]
id   = "t"
task = "test task"
"#;
        let cfg: TemplateConfig = toml::from_str(raw).unwrap();
        assert!(cfg.capabilities.is_some(), "precondition: [capabilities] section exists");
        let config = cfg.to_agent_config(None, vec![]).unwrap();
        assert_eq!(
            config.agent.unwrap().capabilities,
            Some(vec![]),
            "present-but-empty [capabilities] must produce deny-all Some([]), same as absent"
        );
    }

    #[test]
    fn to_agent_config_preserves_agent_mcp_caps() {
        // Mcp caps can only be expressed in [agent].capabilities (no sugar form).
        // They must survive the lowering step alongside [capabilities] sugar.
        let raw = r#"
[template]
name = "t"
description = "d"
showcases = "s"

[capabilities]
fs_read = ["/workspace"]

[agent]
id   = "t"
task = "test task"
capabilities = [{ Mcp = { server = "myserver", tools = [] } }]
"#;
        let cfg: TemplateConfig = toml::from_str(raw).unwrap();
        let config = cfg.to_agent_config(None, vec![]).unwrap();
        let caps = config.agent.unwrap().capabilities.unwrap();
        assert!(
            caps.contains(&Capability::FsRead { prefix: "/workspace".into() }),
            "FsRead from [capabilities] sugar must be present"
        );
        assert!(
            caps.iter().any(|c| matches!(c, Capability::Mcp { server, .. } if server == "myserver")),
            "Mcp cap from [agent].capabilities must survive lowering"
        );
    }

    // ── TemplateResolver ──────────────────────────────────────────────────────────

    #[test]
    fn resolver_user_overrides_repo() {
        let repo = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();

        let repo_content = r#"
[template]
name = "scout"
description = "Repo version."
showcases = "repo"

[agent]
id = "scout"
task = "repo task"
"#;
        let user_content = r#"
[template]
name = "scout"
description = "User version."
showcases = "user"

[agent]
id = "scout"
task = "user task"
"#;
        write_template(&repo, "scout", repo_content);
        write_template(&user, "scout", user_content);

        let resolver = TemplateResolver::new(repo.path().to_path_buf(), user.path().to_path_buf());
        let (cfg, src) = resolver.resolve("scout").unwrap();
        assert_eq!(src, TemplateSource::User);
        assert_eq!(cfg.template.description, "User version.");
    }

    #[test]
    fn resolver_repo_fallback_when_user_absent() {
        let repo = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();

        write_template(
            &repo,
            "scout",
            r#"
[template]
name = "scout"
description = "Repo scout."
showcases = "s"

[agent]
id = "scout"
task = "t"
"#,
        );

        let resolver = TemplateResolver::new(repo.path().to_path_buf(), user.path().to_path_buf());
        let (cfg, src) = resolver.resolve("scout").unwrap();
        assert_eq!(src, TemplateSource::Repo);
        assert_eq!(cfg.template.description, "Repo scout.");
    }

    #[test]
    fn resolver_error_on_missing() {
        let repo = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        let resolver = TemplateResolver::new(repo.path().to_path_buf(), user.path().to_path_buf());
        let result = resolver.resolve("nonexistent");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("nonexistent"), "error must mention the template name");
        assert!(msg.contains("not found"), "error must say 'not found'");
    }

    #[test]
    fn resolver_path_traversal_rejected() {
        let repo = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        let resolver = TemplateResolver::new(repo.path().to_path_buf(), user.path().to_path_buf());

        assert!(resolver.resolve("../etc/passwd").is_err(), "../ must be rejected");
        assert!(resolver.resolve("sub/name").is_err(), "/ must be rejected");
        assert!(resolver.resolve("").is_err(), "empty name must be rejected");
    }

    #[test]
    fn resolver_list_deduplicates_by_name() {
        let repo = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();

        let tmpl = |desc: &str| {
            format!(
                r#"
[template]
name = "scout"
description = "{desc}"
showcases = "s"

[agent]
id = "scout"
task = "t"
"#
            )
        };
        write_template(&repo, "scout", &tmpl("repo"));
        write_template(&user, "scout", &tmpl("user"));

        let resolver = TemplateResolver::new(repo.path().to_path_buf(), user.path().to_path_buf());
        let entries = resolver.list().unwrap();
        assert_eq!(entries.len(), 1, "duplicate name must be deduplicated");
        assert_eq!(entries[0].source, TemplateSource::User, "user wins");
    }

    #[test]
    fn resolver_list_shows_both_sources() {
        let repo = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();

        write_template(
            &repo,
            "alpha",
            r#"
[template]
name = "alpha"
description = "Repo alpha."
showcases = "s"

[agent]
id = "alpha"
task = "t"
"#,
        );
        write_template(
            &user,
            "beta",
            r#"
[template]
name = "beta"
description = "User beta."
showcases = "s"

[agent]
id = "beta"
task = "t"
"#,
        );

        let resolver = TemplateResolver::new(repo.path().to_path_buf(), user.path().to_path_buf());
        let entries = resolver.list().unwrap();
        assert_eq!(entries.len(), 2);
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "repo template must appear");
        assert!(names.contains(&"beta"), "user template must appear");
        let alpha = entries.iter().find(|e| e.name == "alpha").unwrap();
        assert_eq!(alpha.source, TemplateSource::Repo, "repo template must have Repo source");
        let beta = entries.iter().find(|e| e.name == "beta").unwrap();
        assert_eq!(beta.source, TemplateSource::User, "user template must have User source");
    }

    // ── scout template smoke test ─────────────────────────────────────────────────

    #[test]
    fn scout_template_parses() {
        let raw = include_str!("../../templates/scout.template.toml");
        let cfg: TemplateConfig =
            toml::from_str(raw).expect("templates/scout.template.toml must parse");
        assert_eq!(cfg.template.name, "scout");
        assert_eq!(cfg.sample_tasks.len(), 2, "scout must have 2 sample tasks");
        assert!(cfg.capabilities.is_some(), "scout must declare [capabilities]");
        assert!(cfg.agent.is_some(), "scout must have [agent] section");
    }

    // ── catalogue tests (p6.7) ────────────────────────────────────────────────────

    fn catalogue_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("agentd/ must have a parent dir (repo root)")
            .join("templates")
    }

    fn catalogue_resolver() -> TemplateResolver {
        TemplateResolver::new(
            catalogue_dir(),
            std::path::PathBuf::from("/nonexistent-user-dir"),
        )
    }

    #[test]
    fn catalogue_all_seven_templates_present() {
        let resolver = catalogue_resolver();
        let entries = resolver.list().unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        for expected in &["scout", "librarian", "journaler", "coordinator", "code-aware", "watcher", "memory-custodian"] {
            assert!(
                names.contains(expected),
                "catalogue must contain template '{expected}'; found: {names:?}"
            );
        }
        assert_eq!(entries.len(), 7, "catalogue must have exactly 7 templates; found: {names:?}");
    }

    #[test]
    fn catalogue_non_gated_templates_lower_to_valid_config() {
        let resolver = catalogue_resolver();

        let (lib_cfg, _) = resolver.resolve("librarian").unwrap();
        let lib_config = lib_cfg.to_agent_config(Some("index /workspace/library"), vec![]).unwrap();
        assert!(lib_config.agent.is_some(), "librarian must lower to a valid Config");

        let (coord_cfg, _) = resolver.resolve("coordinator").unwrap();
        let coord_config = coord_cfg.to_agent_config(Some("spawn two scouts"), vec![]).unwrap();
        assert!(coord_config.agent.is_some(), "coordinator must lower to a valid Config");
        assert!(
            !coord_config.memory.enabled,
            "coordinator must have memory.enabled = false (no phantom memory.redb)"
        );
        let native = &coord_config.tools.native;
        assert!(native.contains(&"spawn_agent".to_string()), "coordinator must list spawn_agent in tools.native");
        assert!(native.contains(&"send_message".to_string()), "coordinator must list send_message in tools.native");
        assert!(native.contains(&"list_agents".to_string()), "coordinator must list list_agents in tools.native");
    }

    #[test]
    fn catalogue_gated_templates_lower_to_valid_config() {
        let resolver = catalogue_resolver();
        for name in &["journaler", "memory-custodian", "watcher"] {
            let (cfg, _) = resolver.resolve(name).unwrap();
            let config = cfg
                .to_agent_config(Some("test task"), vec![])
                .unwrap_or_else(|e| panic!("template '{name}' must lower without error: {e:#}"));
            assert!(config.agent.is_some(), "template '{name}' must produce a Config with [agent]");
            assert!(
                cfg.template.gated_requires.is_some(),
                "gated template '{name}' must have gated_requires set"
            );
            let req = cfg.template.gated_requires.as_deref().unwrap();
            assert!(!req.is_empty(), "gated_requires for '{name}' must not be empty");
        }
    }

    #[test]
    fn catalogue_gvisor_template_config() {
        let resolver = catalogue_resolver();
        let (cfg, _) = resolver.resolve("code-aware").unwrap();
        let config = cfg.to_agent_config(Some("analyze /workspace/repo"), vec![]).unwrap();
        assert!(config.agent.is_some());
        assert!(
            cfg.template.gated_requires.is_some(),
            "code-aware must have gated_requires (requires runsc)"
        );
        for server in &config.tools.mcp_servers {
            assert_eq!(
                server.isolation,
                crate::config::IsolationMode::Gvisor,
                "code-aware MCP server '{}' must use Gvisor isolation",
                server.name
            );
        }
    }

    #[test]
    fn catalogue_coordinator_has_spawn_cap() {
        let resolver = catalogue_resolver();
        let (cfg, _) = resolver.resolve("coordinator").unwrap();
        let card = cfg.card.as_ref().expect("coordinator must have [card] section");
        assert!(
            card.suggested_caps.contains(&Capability::Spawn),
            "coordinator suggested_caps must include Spawn"
        );
        let config = cfg.to_agent_config(Some("spawn scouts"), vec![]).unwrap();
        assert!(
            config.tools.native.contains(&"spawn_agent".to_string()),
            "coordinator tools.native must include spawn_agent"
        );
    }

    #[test]
    fn catalogue_gated_templates_have_sample_tasks() {
        let resolver = catalogue_resolver();
        let entries = resolver.list().unwrap();
        for entry in &entries {
            if ["journaler", "watcher", "memory-custodian"].contains(&entry.name.as_str()) {
                assert!(
                    !entry.sample_tasks.is_empty(),
                    "gated template '{}' must have at least one sample_task",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn catalogue_journaler_memory_segment_set() {
        let resolver = catalogue_resolver();
        let (cfg, _) = resolver.resolve("journaler").unwrap();
        let config = cfg.to_agent_config(Some("record findings"), vec![]).unwrap();
        assert!(!config.memory.segments.is_empty(), "journaler must have [[memory.segments]]");
        let seg = &config.memory.segments[0];
        assert_eq!(seg.name, "agent/journaler", "segment name must be 'agent/journaler'");
        assert_eq!(
            seg.class,
            crate::config::SegmentClass::Log,
            "segment class must be Log"
        );
        assert_eq!(
            config.memory.store_path,
            "/run/memory/memory.redb",
            "journaler must use /run/memory/memory.redb store path"
        );
    }
}
