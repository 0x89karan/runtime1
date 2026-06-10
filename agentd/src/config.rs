use serde::{Deserialize, Serialize};

use crate::capability::Capability;

/// A discoverable identity card for an agent.
/// Built from `AgentConfig` at scheduler seed time; held by `ListAgentsTool`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub id:          String,
    pub name:        String,
    pub description: String,
    pub skills:      Vec<String>,
}

impl From<&AgentConfig> for AgentCard {
    fn from(cfg: &AgentConfig) -> Self {
        Self {
            id:          cfg.id.clone(),
            name:        cfg.name.clone().unwrap_or_else(|| cfg.id.clone()),
            description: cfg.description.clone(),
            skills:      cfg.skills.clone(),
        }
    }
}

// deny_unknown_fields is intentionally omitted here to allow both [agent] and [[agents]]
// forms to coexist in the schema without serde rejecting the other key.
#[derive(Debug, Deserialize)]
pub struct Config {
    pub agent: Option<AgentConfig>,
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub scheduler: SchedulerConfig,
}

impl Config {
    /// Returns 1+ AgentConfigs from either the `[agent]` or `[[agents]]` TOML form.
    /// Errors if both are set (ambiguous), neither is set (nothing to run), or any
    /// agent id is duplicated.
    pub fn agent_configs(&self) -> anyhow::Result<Vec<AgentConfig>> {
        match (&self.agent, self.agents.is_empty()) {
            (Some(_), false) => anyhow::bail!(
                "cannot set both [agent] and [[agents]] in the same config; use one form"
            ),
            (None, true) => anyhow::bail!(
                "no agents configured; set [agent] for a single agent or [[agents]] for multiple"
            ),
            (Some(a), true) => Ok(vec![a.clone()]),
            (None, false) => Ok(self.agents.clone()),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    /// Global token ceiling across all agents. 0 = unlimited.
    #[serde(default)]
    pub global_token_budget: u64,
    /// Maximum number of in-flight inference calls at once. 0 = unlimited.
    #[serde(default)]
    pub max_concurrent_inferences: usize,
    /// Maximum spawn nesting depth. 0 = spawning disabled. Default 4.
    #[serde(default = "default_max_spawn_depth")]
    pub max_spawn_depth: u32,
    /// Checkpoint the full scheduler state every N completed turns (after tool results).
    /// 0 = SIGTERM/SIGINT only. Default 1 (every turn).
    #[serde(default = "default_checkpoint_interval_turns")]
    pub checkpoint_interval_turns: u32,
}

fn default_max_spawn_depth() -> u32 {
    4
}

fn default_checkpoint_interval_turns() -> u32 {
    1
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            global_token_budget:       0,
            max_concurrent_inferences: 0,
            max_spawn_depth:           default_max_spawn_depth(),
            checkpoint_interval_turns: default_checkpoint_interval_turns(),
        }
    }
}

/// Input to the `spawn_agent` tool — describes the child agent to create.
/// Capabilities and max_turns inherit from parent when absent.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SpawnConfig {
    /// If absent, auto-generated as `"{parent_id}-child-{seq}"`.
    pub child_id: Option<String>,
    /// The child's initial task (first user message).
    pub task: String,
    /// Scheduling priority. Default 0.
    #[serde(default)]
    pub priority: u32,
    /// Per-agent token ceiling. Inherits parent's remaining budget if absent.
    pub token_budget: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub id: String,
    #[serde(default)]
    pub task: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_token_budget")]
    pub token_budget: u64,
    /// Scheduling priority. Higher value runs before lower. Default 0 (equal priority).
    #[serde(default)]
    pub priority: u32,
    /// Tool capabilities granted to this agent.
    /// `None` (field absent) = unrestricted access to all registered tools.
    /// `Some([])` = deny all tool use.
    /// `Some([...])` = allow only the listed capabilities.
    #[serde(default)]
    pub capabilities: Option<Vec<Capability>>,
    /// Display name for this agent. Defaults to `id` if absent.
    #[serde(default)]
    pub name: Option<String>,
    /// Human-readable description of this agent's purpose.
    #[serde(default)]
    pub description: String,
    /// Free-form skill tags this agent advertises.
    #[serde(default)]
    pub skills: Vec<String>,
}

pub fn default_max_turns() -> u32 {
    20
}

pub fn default_token_budget() -> u64 {
    100_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_provider() -> String {
    "anthropic".to_string()
}

fn default_model() -> String {
    "claude-sonnet-4-6".to_string()
}

fn default_max_tokens() -> u32 {
    4096
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            max_tokens: default_max_tokens(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    #[serde(default)]
    pub native: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Capability-based sandbox rules applied to this MCP server subprocess.
    /// `None` (field absent) = no sandbox — server runs unrestricted.
    /// `Some([])` = deny-all spawn; no filesystem grants.
    /// `Some([...])` = exact capability set converted to Landlock + seccomp rules.
    #[serde(default)]
    pub capabilities: Option<Vec<Capability>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_config_round_trips() {
        let raw = r#"
[agent]
id = "scout"
task = "list the project dir and summarize Cargo.toml"
max_turns = 10
token_budget = 50000

[model]
provider = "anthropic"
model = "claude-sonnet-4-6"
max_tokens = 2048

[tools]
native = ["read_file", "list_dir"]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.agent.as_ref().unwrap().id, "scout");
        assert_eq!(cfg.agent.as_ref().unwrap().max_turns, 10);
        assert_eq!(cfg.agent.as_ref().unwrap().token_budget, 50_000);
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.model, "claude-sonnet-4-6");
        assert_eq!(cfg.model.max_tokens, 2048);
        assert_eq!(cfg.tools.native, vec!["read_file", "list_dir"]);
    }

    #[test]
    fn model_defaults_apply_when_section_absent() {
        let raw = r#"
[agent]
id = "minimal"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.model, "claude-sonnet-4-6");
        assert_eq!(cfg.model.max_tokens, 4096);
        assert_eq!(cfg.agent.as_ref().unwrap().max_turns, 20);
        assert_eq!(cfg.agent.as_ref().unwrap().token_budget, 100_000);
    }

    #[test]
    fn missing_agent_id_is_error() {
        let raw = r#"
[agent]
task = "no id here"
"#;
        let result: Result<Config, _> = toml::from_str(raw);
        assert!(result.is_err(), "expected Err when agent.id is missing");
    }

    #[test]
    fn agent_config_rejects_unknown_fields() {
        // deny_unknown_fields still applies on AgentConfig itself
        let raw = r#"
[agent]
id = "scout"
unknown_field = "bad"
"#;
        let result: Result<Config, _> = toml::from_str(raw);
        assert!(result.is_err(), "expected Err for unknown field in [agent]");
    }

    #[test]
    fn top_level_unknown_field_is_now_allowed() {
        // Config no longer has deny_unknown_fields — extra top-level keys are silently ignored
        let raw = r#"
[agent]
id = "scout"

[typo_section]
foo = "bar"
"#;
        let result: Result<Config, _> = toml::from_str(raw);
        assert!(result.is_ok(), "unknown top-level section should be ignored now");
    }

    #[test]
    fn agent_configs_single_agent_form() {
        let raw = r#"
[agent]
id = "solo"
task = "do something"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].id, "solo");
    }

    #[test]
    fn agent_configs_multi_agent_form() {
        let raw = r#"
[[agents]]
id = "alpha"
task = "task a"

[[agents]]
id = "beta"
task = "task b"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(cfgs.len(), 2);
        assert_eq!(cfgs[0].id, "alpha");
        assert_eq!(cfgs[1].id, "beta");
    }

    #[test]
    fn agent_configs_both_set_is_error() {
        let raw = r#"
[agent]
id = "solo"
task = "task"

[[agents]]
id = "alpha"
task = "task a"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let result = cfg.agent_configs();
        assert!(result.is_err(), "expected Err when both [agent] and [[agents]] are set");
        assert!(result.unwrap_err().to_string().contains("cannot set both"));
    }

    #[test]
    fn agent_configs_neither_set_is_error() {
        let raw = r#"
[model]
model = "claude-sonnet-4-6"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let result = cfg.agent_configs();
        assert!(result.is_err(), "expected Err when neither [agent] nor [[agents]] is set");
        assert!(result.unwrap_err().to_string().contains("no agents configured"));
    }

    #[test]
    fn scheduler_config_explicit_values_parse() {
        let raw = r#"
[agent]
id = "a"

[scheduler]
global_token_budget = 1000
max_concurrent_inferences = 4
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.scheduler.global_token_budget, 1000);
        assert_eq!(cfg.scheduler.max_concurrent_inferences, 4);
    }

    #[test]
    fn scheduler_config_defaults_to_unlimited() {
        let raw = r#"
[agent]
id = "a"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.scheduler.global_token_budget, 0, "0 means unlimited");
        assert_eq!(cfg.scheduler.max_concurrent_inferences, 0, "0 means unlimited");
        assert_eq!(cfg.scheduler.max_spawn_depth, 4, "default spawn depth must be 4");
    }

    #[test]
    fn scheduler_config_max_spawn_depth_explicit() {
        let raw = r#"
[agent]
id = "a"

[scheduler]
max_spawn_depth = 2
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.scheduler.max_spawn_depth, 2);
    }

    #[test]
    fn scheduler_config_default_impl_has_spawn_depth_4() {
        // SchedulerConfig::default() must return max_spawn_depth=4 so that
        // Rust-constructed configs (as used in tests) are not silently broken.
        let sc = SchedulerConfig::default();
        assert_eq!(sc.max_spawn_depth, 4);
    }

    #[test]
    fn spawn_config_deserializes_task_only() {
        let raw = r#"{"task": "do the thing"}"#;
        let sc: SpawnConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(sc.task, "do the thing");
        assert!(sc.child_id.is_none());
        assert_eq!(sc.priority, 0);
        assert!(sc.token_budget.is_none());
    }

    #[test]
    fn spawn_config_deserializes_all_fields() {
        let raw = r#"{"task":"sub","child_id":"my-child","priority":5,"token_budget":50000}"#;
        let sc: SpawnConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(sc.task, "sub");
        assert_eq!(sc.child_id.as_deref(), Some("my-child"));
        assert_eq!(sc.priority, 5);
        assert_eq!(sc.token_budget, Some(50_000));
    }

    #[test]
    fn spawn_config_missing_task_is_error() {
        let raw = r#"{"child_id": "orphan"}"#;
        let result: Result<SpawnConfig, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "SpawnConfig requires `task`");
    }

    #[test]
    fn agent_priority_parses_from_toml() {
        let raw = r#"
[[agents]]
id = "high"
task = "task"
priority = 10

[[agents]]
id = "low"
task = "task"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(cfgs[0].priority, 10);
        assert_eq!(cfgs[1].priority, 0, "absent priority defaults to 0");
    }

    #[test]
    fn capabilities_absent_defaults_to_none() {
        let raw = r#"
[agent]
id = "a"
task = "task"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert!(cfgs[0].capabilities.is_none(), "absent capabilities = unrestricted");
    }

    #[test]
    fn capabilities_empty_array_is_deny_all() {
        let raw = r#"
[agent]
id = "a"
task = "task"
capabilities = []
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(cfgs[0].capabilities, Some(vec![]));
    }

    #[test]
    fn capabilities_fs_read_round_trip() {
        let raw = r#"
[agent]
id = "a"
task = "task"
capabilities = [{ FsRead = { prefix = "/workspace" } }]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        assert_eq!(
            cfgs[0].capabilities,
            Some(vec![Capability::FsRead {
                prefix: "/workspace".to_string()
            }])
        );
    }

    #[test]
    fn capabilities_multiple_variants_round_trip() {
        let raw = r#"
[agent]
id = "a"
task = "task"
capabilities = [
  { FsRead = { prefix = "/workspace" } },
  { FsWrite = { prefix = "/tmp" } },
  { Mcp = { server = "echo", tools = ["echo_text"] } },
]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let cfgs = cfg.agent_configs().unwrap();
        let caps = cfgs[0].capabilities.as_ref().unwrap();
        assert_eq!(caps.len(), 3);
        assert_eq!(caps[0], Capability::FsRead { prefix: "/workspace".to_string() });
        assert_eq!(caps[1], Capability::FsWrite { prefix: "/tmp".to_string() });
        assert_eq!(
            caps[2],
            Capability::Mcp {
                server: "echo".to_string(),
                tools: vec!["echo_text".to_string()]
            }
        );
    }

    // ── p1.6: AgentCard tests ─────────────────────────────────────────────────

    #[test]
    fn agent_card_name_defaults_to_id() {
        let cfg = AgentConfig {
            id:           "scout".to_string(),
            task:         String::new(),
            max_turns:    20,
            token_budget: 100_000,
            priority:     0,
            capabilities: None,
            name:         None,
            description:  String::new(),
            skills:       vec![],
        };
        let card = AgentCard::from(&cfg);
        assert_eq!(card.id, "scout");
        assert_eq!(card.name, "scout", "name should default to id when absent");
        assert_eq!(card.description, "");
        assert!(card.skills.is_empty());
    }

    #[test]
    fn agent_card_explicit_name_and_skills() {
        let cfg = AgentConfig {
            id:           "reader".to_string(),
            task:         String::new(),
            max_turns:    20,
            token_budget: 100_000,
            priority:     0,
            capabilities: None,
            name:         Some("File Reader".to_string()),
            description:  "Reads files".to_string(),
            skills:       vec!["read".to_string(), "summarize".to_string()],
        };
        let card = AgentCard::from(&cfg);
        assert_eq!(card.name, "File Reader");
        assert_eq!(card.description, "Reads files");
        assert_eq!(card.skills, vec!["read", "summarize"]);
    }

    #[test]
    fn agent_config_name_description_skills_parse_from_toml() {
        let raw = r#"
[agent]
id = "assistant"
name = "My Assistant"
description = "A helpful assistant"
skills = ["research", "write"]
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        let ac = cfg.agent.unwrap();
        assert_eq!(ac.name.as_deref(), Some("My Assistant"));
        assert_eq!(ac.description, "A helpful assistant");
        assert_eq!(ac.skills, vec!["research", "write"]);
    }
}
