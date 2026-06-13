pub mod mcp;
pub mod native;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::capability::{satisfies, satisfies_type, Capability};
use crate::flight_recorder::{EventKind, FlightRecorder};
use crate::inference::ToolSpec;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn invoke(&self, input: Value) -> Result<String>;

    /// The specific capability required to invoke this tool with the given
    /// `input`. Called at invocation time so path-based tools can return the
    /// actual access path (e.g. `FsRead { prefix: input["path"] }`).
    ///
    /// Returns `None` for tools that require no capability gating. Such tools
    /// are always visible in `filtered_specs` and always invocable, even when
    /// the agent's cap-set is `Some([])` (deny-all). This is intentional: tools
    /// like `list_agents` and `send_message` are control-plane primitives that
    /// should not be suppressible by a FS/Net capability scope. If a future
    /// tool should be hidden under a deny-all cap-set, it must declare a
    /// synthetic capability and return `Some(...)` here.
    fn required_capability_for(&self, _input: &Value) -> Option<Capability> {
        None
    }
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

    /// Register a tool. Returns an error if a tool with the same name is
    /// already registered — callers must resolve the conflict explicitly.
    pub fn register(&mut self, tool: Box<dyn Tool>) -> Result<()> {
        let name = tool.name().to_string();
        if self.tools.contains_key(&name) {
            return Err(anyhow::anyhow!(
                "tool '{name}' is already registered; \
                 use a different name or remove the conflicting registration"
            ));
        }
        self.tools.insert(name, tool);
        Ok(())
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

    /// Returns specs for only the tools this agent is allowed to see and use.
    ///
    /// `None` cap-set = unrestricted (all tools visible, backward compat).
    /// `Some([])` = deny all (empty spec list sent to the model).
    /// `Some([...])` = include only tools whose `required_capability_for` is
    /// satisfied by at least one entry in `cap_set`, plus tools that declare
    /// no required capability (`required_capability_for` returns `None`).
    pub fn filtered_specs(&self, cap_set: Option<&[Capability]>) -> Vec<ToolSpec> {
        let Some(caps) = cap_set else {
            return self.specs();
        };
        let mut specs: Vec<ToolSpec> = self
            .tools
            .values()
            .filter(|t| {
                // Use Value::Null as a probe — path-based tools return an empty prefix,
                // which satisfies_type treats as "has any FsRead/FsWrite cap?" (type-level).
                // The actual path-specific check happens at invocation time in `invoke`.
                match t.required_capability_for(&Value::Null) {
                    None => true,
                    Some(required) => satisfies_type(caps, &required),
                }
            })
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

    /// Invoke a tool, enforcing the agent's capability set.
    ///
    /// `cap_set = None` bypasses capability checking (backward-compat single-
    /// agent driver path). `cap_set = Some(caps)` checks the tool's required
    /// capability against `caps`; on denial, records a `CapabilityDenied` event
    /// and returns an error without calling the tool.
    pub async fn invoke(
        &self,
        name: &str,
        input: Value,
        agent_id: &str,
        cap_set: Option<&[Capability]>,
        recorder: &FlightRecorder,
    ) -> Result<String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool: {name}"))?;

        if let Some(caps) = cap_set {
            if let Some(required) = tool.required_capability_for(&input) {
                if !satisfies(caps, &required) {
                    recorder.record(
                        agent_id,
                        None,
                        EventKind::CapabilityDenied,
                        serde_json::json!({
                            "tool": name,
                            "required": serde_json::to_value(&required)
                                .unwrap_or_else(|_| format!("{required:?}").into()),
                        }),
                    );
                    return Err(anyhow::anyhow!(
                        "capability denied: tool '{name}' requires {required:?}"
                    ));
                }
            }
        }

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
    use tempfile::NamedTempFile;

    fn recorder() -> (FlightRecorder, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let rec = FlightRecorder::new(tmp.path()).unwrap();
        (rec, tmp)
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let reg = ToolRegistry::new();
        let (rec, _tmp) = recorder();
        let err = reg
            .invoke("nonexistent", serde_json::json!({}), "a", None, &rec)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn duplicate_registration_returns_error() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["read_file".to_string()], None).unwrap();
        let err = register_native(&mut reg, &["read_file".to_string()], None).unwrap_err();
        assert!(err.to_string().contains("read_file"));
        assert!(err.to_string().contains("already registered"));
    }

    #[tokio::test]
    async fn registry_specs_and_names_are_sorted() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None).unwrap();
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

    #[test]
    fn filtered_specs_none_cap_set_returns_all() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None).unwrap();
        assert_eq!(reg.filtered_specs(None).len(), reg.specs().len());
    }

    #[test]
    fn filtered_specs_empty_cap_set_returns_only_no_cap_tools() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None).unwrap();
        let specs = reg.filtered_specs(Some(&[]));
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        // list_agents and send_message require no capability; they remain visible.
        assert!(names.contains(&"list_agents"), "list_agents must be visible without capabilities");
        assert!(names.contains(&"send_message"), "send_message must be visible without capabilities");
        // Capability-gated tools must be hidden.
        assert!(!names.contains(&"read_file"), "read_file must be hidden without FsRead capability");
        assert!(!names.contains(&"write_file"), "write_file must be hidden without FsWrite capability");
        assert!(!names.contains(&"list_dir"), "list_dir must be hidden without FsRead capability");
        assert!(!names.contains(&"spawn_agent"), "spawn_agent must be hidden without Spawn capability");
    }

    #[test]
    fn filtered_specs_fs_read_only_excludes_write() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None).unwrap();
        let caps = [Capability::FsRead { prefix: "/".to_string() }];
        let specs = reg.filtered_specs(Some(&caps));
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"read_file"), "read_file should be visible");
        assert!(names.contains(&"list_dir"), "list_dir should be visible");
        assert!(!names.contains(&"write_file"), "write_file should be hidden");
    }

    #[tokio::test]
    async fn capability_denied_event_emitted_and_error_returned() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["write_file".to_string()], None).unwrap();
        let (rec, tmp) = recorder();

        // Grant only FsRead — write_file requires FsWrite, so it should be denied.
        let caps = [Capability::FsRead { prefix: "/".to_string() }];
        let err = reg
            .invoke(
                "write_file",
                serde_json::json!({"path": "/tmp/x", "content": "hi"}),
                "test-agent",
                Some(&caps),
                &rec,
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("capability denied"));

        // Verify the flight event was recorded.
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(event["kind"], "capability_denied");
        assert_eq!(event["agent"], "test-agent");
        assert_eq!(event["data"]["tool"], "write_file");
        // `required` is serialized as structured JSON, not a debug string
        assert_eq!(event["data"]["required"]["FsWrite"]["prefix"], "/tmp/x");
    }

    #[tokio::test]
    async fn capability_granted_invoke_succeeds() {
        // An agent with the matching FsWrite cap MUST be able to invoke write_file.
        // This is the "granted agent succeeds" half of the p1.4 acceptance criterion.
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["write_file".to_string()], None).unwrap();
        let (rec, _tmp) = recorder();
        let tmp_dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let path = tmp_dir.path().join("test.txt").to_string_lossy().to_string();

        let caps = [Capability::FsWrite { prefix: "/tmp".to_string() }];
        let result = reg
            .invoke(
                "write_file",
                serde_json::json!({"path": path, "content": "hello"}),
                "test-agent",
                Some(&caps),
                &rec,
            )
            .await;
        assert!(result.is_ok(), "granted cap should allow write_file: {result:?}");
    }

    #[test]
    fn filtered_specs_spawn_visible_with_cap_hidden_without() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None).unwrap();

        // With Spawn capability: spawn_agent should appear in specs.
        let caps_with_spawn = [Capability::Spawn];
        let specs_with = reg.filtered_specs(Some(&caps_with_spawn));
        let names_with: Vec<&str> = specs_with.iter().map(|s| s.name.as_str()).collect();
        assert!(names_with.contains(&"spawn_agent"), "spawn_agent must be visible when Spawn cap granted");

        // Without Spawn capability (e.g. only FsRead): spawn_agent must be hidden.
        let caps_no_spawn = [Capability::FsRead { prefix: "/".to_string() }];
        let specs_without = reg.filtered_specs(Some(&caps_no_spawn));
        let names_without: Vec<&str> = specs_without.iter().map(|s| s.name.as_str()).collect();
        assert!(!names_without.contains(&"spawn_agent"), "spawn_agent must be hidden without Spawn cap");
    }
}
