pub mod mcp;
pub mod native;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::capability::{satisfies, satisfies_type, Capability};
use crate::flight_recorder::{EventKind, FlightRecorder};
use crate::inference::ToolSpec;

/// Runtime context injected by the scheduler for every tool invocation.
///
/// Fields are runtime-stamped and unforgeable — the agent cannot set them
/// via tool input. `MemRemember` uses `turn` and `task_fp` for provenance.
pub struct ToolContext {
    pub agent_id: String,
    pub turn: u32,
    /// Stable 16-hex fingerprint of the agent's initial task (FNV-1a 64-bit).
    pub task_fp: String,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<String>;

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
        ctx: &ToolContext,
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
                        &ctx.agent_id,
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

        let result = tool.invoke(input, ctx).await?;

        // Post-call hook: emit memory events for kv and long-term memory tools.
        // Tool::invoke has no recorder; we emit here where both are available.
        match name {
            "kv_get" => {
                recorder.record(
                    &ctx.agent_id,
                    None,
                    EventKind::MemoryRead,
                    serde_json::json!({
                        "agent": &ctx.agent_id,
                        "found": !result.is_empty(),
                    }),
                );
            }
            "kv_set" => {
                recorder.record(
                    &ctx.agent_id,
                    None,
                    EventKind::MemoryWrite,
                    serde_json::json!({
                        "agent": &ctx.agent_id,
                        "bytes": result.len(),
                    }),
                );
            }
            "mem_remember" => {
                recorder.record(
                    &ctx.agent_id,
                    Some(ctx.turn),
                    EventKind::MemoryDistilled,
                    serde_json::json!({
                        "agent": &ctx.agent_id,
                        "turn": ctx.turn,
                        "items": 1,
                    }),
                );
            }
            _ => {}
        }

        Ok(result)
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

    fn ctx(agent_id: &str) -> ToolContext {
        ToolContext { agent_id: agent_id.to_string(), turn: 0, task_fp: String::new() }
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let reg = ToolRegistry::new();
        let (rec, _tmp) = recorder();
        let err = reg
            .invoke("nonexistent", serde_json::json!({}), &ctx("a"), None, &rec)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn duplicate_registration_returns_error() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["read_file".to_string()], None, None).unwrap();
        let err = register_native(&mut reg, &["read_file".to_string()], None, None).unwrap_err();
        assert!(err.to_string().contains("read_file"));
        assert!(err.to_string().contains("already registered"));
    }

    #[tokio::test]
    async fn registry_specs_and_names_are_sorted() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None, None).unwrap();
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
        register_native(&mut reg, &["all".to_string()], None, None).unwrap();
        assert_eq!(reg.filtered_specs(None).len(), reg.specs().len());
    }

    #[test]
    fn filtered_specs_empty_cap_set_returns_only_no_cap_tools() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None, None).unwrap();
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
        register_native(&mut reg, &["all".to_string()], None, None).unwrap();
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
        register_native(&mut reg, &["write_file".to_string()], None, None).unwrap();
        let (rec, tmp) = recorder();

        // Grant only FsRead — write_file requires FsWrite, so it should be denied.
        let caps = [Capability::FsRead { prefix: "/".to_string() }];
        let err = reg
            .invoke(
                "write_file",
                serde_json::json!({"path": "/tmp/x", "content": "hi"}),
                &ctx("test-agent"),
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
        register_native(&mut reg, &["write_file".to_string()], None, None).unwrap();
        let (rec, _tmp) = recorder();
        let tmp_dir = tempfile::TempDir::new_in("/tmp").unwrap();
        let path = tmp_dir.path().join("test.txt").to_string_lossy().to_string();

        let caps = [Capability::FsWrite { prefix: "/tmp".to_string() }];
        let result = reg
            .invoke(
                "write_file",
                serde_json::json!({"path": path, "content": "hello"}),
                &ctx("test-agent"),
                Some(&caps),
                &rec,
            )
            .await;
        assert!(result.is_ok(), "granted cap should allow write_file: {result:?}");
    }

    #[test]
    fn filtered_specs_spawn_visible_with_cap_hidden_without() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None, None).unwrap();

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

    // ── Gap ⑧: post-call hook emits MemoryRead / MemoryWrite events ──────────

    use std::sync::{Arc, Mutex};
    use crate::memory::MemoryStore;

    struct SimpleStore(Mutex<HashMap<String, String>>);
    impl SimpleStore {
        fn new_arc() -> Arc<Self> {
            Arc::new(SimpleStore(Mutex::new(HashMap::new())))
        }
    }
    impl MemoryStore for SimpleStore {
        fn get(&self, ns: &str, key: &str) -> anyhow::Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(&format!("{}\x00{}", ns, key)).cloned())
        }
        fn put(&self, ns: &str, key: &str, value: &str) -> anyhow::Result<()> {
            self.0.lock().unwrap().insert(format!("{}\x00{}", ns, key), value.to_string());
            Ok(())
        }
        fn append(&self, _ns: &str, _key: &str, _value: &str) -> anyhow::Result<()> { Ok(()) }
        fn delete(&self, _ns: &str, _key: &str) -> anyhow::Result<bool> { Ok(false) }
        fn iter(&self, _ns: &str) -> anyhow::Result<Vec<(String, String)>> { Ok(vec![]) }
        fn meta_version(&self) -> anyhow::Result<u64> { Ok(1) }
    }

    #[test]
    fn filtered_specs_kv_tools_visible_with_kb_caps_hidden_without() {
        let mut reg = ToolRegistry::new();
        register_native(
            &mut reg,
            &["kv_get".to_string(), "kv_set".to_string()],
            None,
            Some(SimpleStore::new_arc()),
        )
        .unwrap();

        // With KbRead + KbWrite: kv_get and kv_set must be visible.
        let caps = [
            Capability::KbRead { segment: "agent:scratch".to_string() },
            Capability::KbWrite { segment: "agent:scratch".to_string() },
        ];
        let specs_with_kb = reg.filtered_specs(Some(&caps));
        let names: Vec<&str> = specs_with_kb.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"kv_get"), "kv_get must be visible with KbRead cap");
        assert!(names.contains(&"kv_set"), "kv_set must be visible with KbWrite cap");

        // Without any kb cap (e.g. only FsRead): kv tools must be hidden.
        let no_kb_caps = [Capability::FsRead { prefix: "/".to_string() }];
        let specs_no_kb = reg.filtered_specs(Some(&no_kb_caps));
        let names_no_kb: Vec<&str> = specs_no_kb.iter().map(|s| s.name.as_str()).collect();
        assert!(!names_no_kb.contains(&"kv_get"), "kv_get must be hidden without KbRead cap");
        assert!(!names_no_kb.contains(&"kv_set"), "kv_set must be hidden without KbWrite cap");
    }

    #[tokio::test]
    async fn kv_get_invoke_emits_memory_read_event() {
        let mut reg = ToolRegistry::new();
        register_native(
            &mut reg,
            &["kv_get".to_string()],
            None,
            Some(SimpleStore::new_arc()),
        )
        .unwrap();
        let (rec, tmp) = recorder();
        let caps = [Capability::KbRead { segment: "agent:scratch".to_string() }];
        reg.invoke(
            "kv_get",
            serde_json::json!({"namespace": "agent:scratch", "key": "absent"}),
            &ctx("agent1"),
            Some(&caps),
            &rec,
        )
        .await
        .unwrap();
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(event["kind"], "memory_read");
        assert_eq!(event["data"]["agent"], "agent1");
        assert_eq!(event["data"]["found"], false); // key absent → result is ""
    }

    #[tokio::test]
    async fn kv_set_invoke_emits_memory_write_event() {
        let mut reg = ToolRegistry::new();
        register_native(
            &mut reg,
            &["kv_set".to_string()],
            None,
            Some(SimpleStore::new_arc()),
        )
        .unwrap();
        let (rec, tmp) = recorder();
        let caps = [Capability::KbWrite { segment: "agent:scratch".to_string() }];
        reg.invoke(
            "kv_set",
            serde_json::json!({"namespace": "agent:scratch", "key": "k", "value": "hello"}),
            &ctx("agent1"),
            Some(&caps),
            &rec,
        )
        .await
        .unwrap();
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(event["kind"], "memory_write");
        assert_eq!(event["data"]["agent"], "agent1");
        assert!(event["data"]["bytes"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn mem_remember_invoke_emits_memory_distilled_event() {
        let mut reg = ToolRegistry::new();
        register_native(
            &mut reg,
            &["mem_remember".to_string()],
            None,
            Some(SimpleStore::new_arc()),
        )
        .unwrap();
        let (rec, tmp) = recorder();
        let ctx_with_turn = ToolContext {
            agent_id: "agent42".to_string(),
            turn: 3,
            task_fp: "deadbeef".to_string(),
        };
        reg.invoke(
            "mem_remember",
            serde_json::json!({"content": "test memory entry", "tags": ["test"]}),
            &ctx_with_turn,
            None,
            &rec,
        )
        .await
        .unwrap();
        let content = std::fs::read_to_string(tmp.path()).unwrap();
        let event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(event["kind"], "memory_distilled");
        assert_eq!(event["agent"], "agent42");
        assert_eq!(event["turn"], 3u64);
        assert_eq!(event["data"]["agent"], "agent42");
        assert_eq!(event["data"]["turn"], 3u64);
        assert_eq!(event["data"]["items"], 1u64);
    }
}
