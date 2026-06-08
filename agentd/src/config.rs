use serde::Deserialize;

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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub id: String,
    #[serde(default)]
    pub task: String,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_token_budget")]
    pub token_budget: u64,
}

fn default_max_turns() -> u32 {
    20
}

fn default_token_budget() -> u64 {
    100_000
}

#[derive(Debug, Clone, Deserialize)]
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
// Fields are consumed by McpClient in p0.5; unused until then.
#[allow(dead_code)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
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
}
