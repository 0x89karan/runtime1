use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub agent: AgentConfig,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
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
        assert_eq!(cfg.agent.id, "scout");
        assert_eq!(cfg.agent.max_turns, 10);
        assert_eq!(cfg.agent.token_budget, 50_000);
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
        assert_eq!(cfg.agent.max_turns, 20);
        assert_eq!(cfg.agent.token_budget, 100_000);
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
    fn unknown_top_level_field_is_error() {
        let raw = r#"
[agent]
id = "scout"

[typo_section]
foo = "bar"
"#;
        let result: Result<Config, _> = toml::from_str(raw);
        assert!(result.is_err(), "expected Err for unknown top-level field");
    }
}
