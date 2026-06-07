pub mod native;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::inference::ToolSpec;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn invoke(&self, input: Value) -> Result<String>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        if self.tools.contains_key(&name) {
            tracing::warn!(tool = %name, "tool already registered — overwriting");
        }
        self.tools.insert(name, tool);
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .values()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }

    pub async fn invoke(&self, name: &str, input: Value) -> Result<String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {name}"))?;
        tool.invoke(input).await
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::native::register_native;

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let reg = ToolRegistry::new();
        let err = reg
            .invoke("nonexistent", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn registry_specs_and_names_are_sorted() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()]);
        let names = reg.tool_names();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        let specs = reg.specs();
        let spec_names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(spec_names.contains(&"read_file"));
        assert!(spec_names.contains(&"write_file"));
        assert!(spec_names.contains(&"list_dir"));
    }
}
