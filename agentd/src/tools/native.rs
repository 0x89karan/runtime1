use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::capability::Capability;
use crate::config::AgentCard;
use crate::memory::{validate_segment, MemoryStore};
use super::{Tool, ToolRegistry};

const READ_FILE_MAX: usize = 100_000;
const MAX_KV_VALUE_BYTES: usize = 256 * 1024; // 256 KiB per stored value

pub struct ReadFile;
pub struct WriteFile;
pub struct ListDir;
/// Read a value from the durable key/value store.
/// NOT registered under `native = ["all"]` — requires explicit listing.
pub struct KvGet {
    pub store: Arc<dyn MemoryStore>,
}

/// Write a value to the durable key/value store.
/// NOT registered under `native = ["all"]` — requires explicit listing.
pub struct KvSet {
    pub store: Arc<dyn MemoryStore>,
}

pub struct SpawnAgentTool;
pub struct ListAgentsTool {
    pub cards: Arc<Vec<AgentCard>>,
}
pub struct SendMessageTool;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns up to 100,000 characters."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, input: &Value) -> Option<Capability> {
        let path = input["path"].as_str().unwrap_or("").to_string();
        Some(Capability::FsRead { prefix: path })
    }

    async fn invoke(&self, input: Value) -> Result<String> {
        let path = input["path"].as_str().context("path must be a string")?;
        let content =
            tokio::fs::read_to_string(path).await.with_context(|| format!("reading {path}"))?;
        if content.chars().count() <= READ_FILE_MAX {
            Ok(content)
        } else {
            let truncated: String = content.chars().take(READ_FILE_MAX).collect();
            Ok(format!("{truncated}\n[truncated at {READ_FILE_MAX} chars]"))
        }
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating parent directories if needed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to write" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, input: &Value) -> Option<Capability> {
        let path = input["path"].as_str().unwrap_or("").to_string();
        Some(Capability::FsWrite { prefix: path })
    }

    async fn invoke(&self, input: Value) -> Result<String> {
        let path = input["path"].as_str().context("path must be a string")?;
        let content = input["content"].as_str().context("content must be a string")?;
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("creating directories for {path}"))?;
            }
        }
        tokio::fs::write(path, content).await.with_context(|| format!("writing {path}"))?;
        Ok(format!("wrote {} chars to {path}", content.chars().count()))
    }
}

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List entries in a directory. Directories are suffixed with /."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the directory" }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, input: &Value) -> Option<Capability> {
        let path = input["path"].as_str().unwrap_or("").to_string();
        Some(Capability::FsRead { prefix: path })
    }

    async fn invoke(&self, input: Value) -> Result<String> {
        let path = input["path"].as_str().context("path must be a string")?;
        let mut dir = tokio::fs::read_dir(path)
            .await
            .with_context(|| format!("reading directory {path}"))?;
        let mut entries = Vec::new();
        while let Some(e) = dir.next_entry().await.context("reading directory entry")? {
            let name = e.file_name().to_string_lossy().to_string();
            let suffix = if e.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                "/"
            } else {
                ""
            };
            entries.push(format!("{name}{suffix}"));
        }
        entries.sort();
        Ok(entries.join("\n"))
    }
}

/// Extract and validate namespace + key from tool input.
/// Returns a human-readable error when the format is wrong.
/// Note: this runs inside `invoke`, after the capability check — format errors
/// are only visible to agents that already passed the capability gate.
fn extract_ns_key(input: &Value) -> Result<(String, String)> {
    let namespace = input["namespace"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("namespace must be a string (e.g. \"agent:scratch\")"))?;
    let key = input["key"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("key must be a string (e.g. \"my-note\")"))?;
    validate_segment(namespace, "namespace")
        .map_err(|e| anyhow::anyhow!("{e}; use namespace like \"agent:scratch\""))?;
    validate_segment(key, "key").map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok((namespace.to_string(), key.to_string()))
}

#[async_trait]
impl Tool for KvGet {
    fn name(&self) -> &str {
        "kv_get"
    }

    fn description(&self) -> &str {
        "Read a value from the durable key/value store. \
         Use namespace \"agent:scratch\" for ephemeral scratch notes. \
         Pass namespace and key as separate fields — e.g. namespace=\"agent:scratch\", key=\"my-note\"."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "Memory namespace granted to this agent (e.g. \"agent:scratch\")"
                },
                "key": {
                    "type": "string",
                    "description": "Key within the namespace (e.g. \"my-note\")"
                }
            },
            "required": ["namespace", "key"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, input: &Value) -> Option<Capability> {
        // For type-level check (input=Null), return empty segment = "any KbRead".
        if input.is_null() {
            return Some(Capability::KbRead { segment: "".to_string() });
        }
        let namespace = input["namespace"].as_str().unwrap_or("").to_string();
        Some(Capability::KbRead { segment: namespace })
    }

    async fn invoke(&self, input: Value) -> Result<String> {
        let (namespace, key) = extract_ns_key(&input)?;
        let store = Arc::clone(&self.store);
        let ns = namespace.clone();
        let k = key.clone();
        let result = tokio::task::spawn_blocking(move || store.get(&ns, &k))
            .await
            .context("kv_get spawn_blocking join")??;
        // Known limitation: both a missing key and a key storing "" return the empty string.
        // The MemoryRead flight event's `found` field uses the non-empty heuristic. Fixing this
        // properly requires a sentinel return value or a separate exists() call (p5.x).
        match result {
            Some(v) => Ok(v),
            None => Ok(String::new()),
        }
    }
}

#[async_trait]
impl Tool for KvSet {
    fn name(&self) -> &str {
        "kv_set"
    }

    fn description(&self) -> &str {
        "Write a value to the durable key/value store. \
         Use namespace \"agent:scratch\" for ephemeral scratch notes that persist across turns. \
         Pass namespace and key as separate fields — e.g. namespace=\"agent:scratch\", key=\"my-note\". \
         Requires KbWrite capability for the namespace."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "Memory namespace granted to this agent (e.g. \"agent:scratch\")"
                },
                "key": {
                    "type": "string",
                    "description": "Key within the namespace (e.g. \"my-note\")"
                },
                "value": {
                    "type": "string",
                    "description": "Value to store"
                }
            },
            "required": ["namespace", "key", "value"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, input: &Value) -> Option<Capability> {
        if input.is_null() {
            return Some(Capability::KbWrite { segment: "".to_string() });
        }
        let namespace = input["namespace"].as_str().unwrap_or("").to_string();
        Some(Capability::KbWrite { segment: namespace })
    }

    async fn invoke(&self, input: Value) -> Result<String> {
        let (namespace, key) = extract_ns_key(&input)?;
        let value = input["value"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("value must be a string"))?
            .to_string();
        if value.len() > MAX_KV_VALUE_BYTES {
            anyhow::bail!(
                "value too large: {} bytes exceeds limit of {} bytes",
                value.len(),
                MAX_KV_VALUE_BYTES
            );
        }
        let store = Arc::clone(&self.store);
        let ns = namespace.clone();
        let k = key.clone();
        let v = value.clone();
        tokio::task::spawn_blocking(move || store.put(&ns, &k, &v))
            .await
            .context("kv_set spawn_blocking join")??;
        Ok(format!("stored {} bytes at {namespace}:{key}", value.len()))
    }
}

#[async_trait]
impl Tool for SpawnAgentTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Spawn a sub-agent to complete a task and return its answer. \
         Must be the sole tool call in a turn. \
         Requires the Spawn capability."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task":         { "type": "string",  "description": "The sub-task for the child agent to complete" },
                "child_id":     { "type": "string",  "description": "Optional stable ID for the child (auto-generated if absent)" },
                "priority":     { "type": "integer", "description": "Scheduling priority (higher runs first). Default 0." },
                "token_budget": { "type": "integer", "description": "Token ceiling for the child. Inherits parent budget if absent." }
            },
            "required": ["task"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, _input: &Value) -> Option<Capability> {
        Some(Capability::Spawn)
    }

    async fn invoke(&self, _input: Value) -> Result<String> {
        // spawn_agent is intercepted by step_with_response() before reaching invoke().
        // If this is called, something bypassed the normal effect dispatch path.
        Err(anyhow::anyhow!(
            "spawn_agent must be intercepted by the scheduler; invoke() should never be called"
        ))
    }
}

#[async_trait]
impl Tool for ListAgentsTool {
    fn name(&self) -> &str {
        "list_agents"
    }

    fn description(&self) -> &str {
        "List all registered agents in the system. Returns a JSON array of agent cards \
         (id, name, description, skills)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, _input: &Value) -> Option<Capability> {
        None
    }

    async fn invoke(&self, _input: Value) -> Result<String> {
        let mut cards = self.cards.as_ref().clone();
        cards.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(serde_json::to_string(&cards)?)
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a message to another agent's mailbox. The recipient will receive it \
         before their next inference step. Must be the sole tool call in a turn."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to":      { "type": "string", "description": "ID of the recipient agent" },
                "content": { "type": "string", "description": "Message content to deliver" }
            },
            "required": ["to", "content"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, _input: &Value) -> Option<Capability> {
        None
    }

    async fn invoke(&self, _input: Value) -> Result<String> {
        // send_message is intercepted by the scheduler before reaching invoke().
        Err(anyhow::anyhow!(
            "send_message must be intercepted by the scheduler; invoke() should never be called"
        ))
    }
}

/// Register native tools by name. Pass `["all"]` to register all general-purpose
/// tools, or a subset by name (e.g. `["read_file", "list_dir"]`).
/// Returns an error if any name collides with an already-registered tool.
///
/// `cards` is required when registering `list_agents`; ignored otherwise.
///
/// **Note:** `kv_get` and `kv_set` are NOT included in `"all"`. They must be
/// requested explicitly (e.g. `native = ["kv_get", "kv_set"]`) because they
/// require `KbRead`/`KbWrite` capability grants — auto-registering them for
/// every agent would produce noisy capability-denied events for agents that
/// have no memory capability.
pub fn register_native(
    reg: &mut ToolRegistry,
    names: &[String],
    cards: Option<Arc<Vec<AgentCard>>>,
    store: Option<Arc<dyn MemoryStore>>,
) -> anyhow::Result<()> {
    let all = names.iter().any(|n| n == "all");
    let want = |name: &str| all || names.iter().any(|n| n == name);
    if want("read_file") {
        reg.register(Box::new(ReadFile))?;
    }
    if want("write_file") {
        reg.register(Box::new(WriteFile))?;
    }
    if want("list_dir") {
        reg.register(Box::new(ListDir))?;
    }
    if want("spawn_agent") {
        reg.register(Box::new(SpawnAgentTool))?;
    }
    if want("list_agents") {
        let c = cards.clone().unwrap_or_else(|| Arc::new(vec![]));
        reg.register(Box::new(ListAgentsTool { cards: c }))?;
    }
    if want("send_message") {
        reg.register(Box::new(SendMessageTool))?;
    }
    // kv_get / kv_set — NOT included in "all"; require explicit opt-in.
    if names.iter().any(|n| n == "kv_get") {
        if let Some(s) = store.clone() {
            reg.register(Box::new(KvGet { store: s }))?;
        }
    }
    if names.iter().any(|n| n == "kv_set") {
        if let Some(s) = store.clone() {
            reg.register(Box::new(KvSet { store: s }))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn read_file_returns_cargo_toml() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let result = ReadFile
            .invoke(json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(!result.is_empty());
        assert!(result.contains("agentd"));
    }

    #[tokio::test]
    async fn read_file_missing_path_errors() {
        let err = ReadFile
            .invoke(json!({"path": "/nonexistent/p0.3-test-file.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("reading"));
    }

    #[tokio::test]
    async fn read_file_missing_input_key_errors() {
        let err = ReadFile.invoke(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("path"));
    }

    #[tokio::test]
    async fn read_file_truncates_large_files() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("big.txt");
        // Write slightly more than 100k chars (all ASCII).
        let content = "x".repeat(READ_FILE_MAX + 50);
        std::fs::write(&path, &content).unwrap();
        let result = ReadFile
            .invoke(json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(result.contains("[truncated at"));
        assert!(result.chars().count() <= READ_FILE_MAX + 100);
    }

    #[tokio::test]
    async fn write_file_creates_and_reads_back() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.txt");
        WriteFile
            .invoke(json!({"path": path.to_str().unwrap(), "content": "hello p0.3"}))
            .await
            .unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello p0.3");
    }

    #[tokio::test]
    async fn write_file_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a/b/c/out.txt");
        WriteFile
            .invoke(json!({"path": path.to_str().unwrap(), "content": "nested"}))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "nested");
    }

    #[tokio::test]
    async fn write_file_missing_path_errors() {
        let err = WriteFile.invoke(json!({"content": "hi"})).await.unwrap_err();
        assert!(err.to_string().contains("path"));
    }

    #[tokio::test]
    async fn write_file_missing_content_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.txt");
        let err = WriteFile
            .invoke(json!({"path": path.to_str().unwrap()}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("content"));
    }

    #[tokio::test]
    async fn list_dir_suffixes_dirs() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("file.txt"), "").unwrap();
        let result = ListDir
            .invoke(json!({"path": dir.path().to_str().unwrap()}))
            .await
            .unwrap();
        assert!(result.contains("subdir/"), "dirs must end with /");
        assert!(result.contains("file.txt"));
        assert!(!result.contains("file.txt/"));
    }

    #[tokio::test]
    async fn list_dir_missing_path_key_errors() {
        let err = ListDir.invoke(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("path"));
    }

    #[tokio::test]
    async fn list_dir_empty_dir_returns_empty_string() {
        let dir = TempDir::new().unwrap();
        let result = ListDir
            .invoke(json!({"path": dir.path().to_str().unwrap()}))
            .await
            .unwrap();
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn list_dir_missing_path_errors() {
        let err = ListDir
            .invoke(json!({"path": "/nonexistent/p0.3-dir"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("reading directory"));
    }

    #[tokio::test]
    async fn list_dir_output_is_sorted() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("z.txt"), "").unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        std::fs::write(dir.path().join("m.txt"), "").unwrap();
        let result = ListDir
            .invoke(json!({"path": dir.path().to_str().unwrap()}))
            .await
            .unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines, vec!["a.txt", "m.txt", "z.txt"]);
    }

    #[test]
    fn register_native_all_registers_six_base_tools_not_kv() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None, None).unwrap();
        let names = reg.tool_names();
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"list_dir".to_string()));
        assert!(names.contains(&"spawn_agent".to_string()));
        assert!(names.contains(&"list_agents".to_string()));
        assert!(names.contains(&"send_message".to_string()));
        // kv tools must NOT be in "all"
        assert!(!names.contains(&"kv_get".to_string()), "kv_get must not be in 'all'");
        assert!(!names.contains(&"kv_set".to_string()), "kv_set must not be in 'all'");
    }

    #[test]
    fn spawn_agent_tool_requires_spawn_capability() {
        let tool = SpawnAgentTool;
        let cap = tool.required_capability_for(&serde_json::Value::Null);
        assert!(matches!(cap, Some(Capability::Spawn)));
    }

    #[tokio::test]
    async fn spawn_agent_invoke_returns_error() {
        let tool = SpawnAgentTool;
        let err = tool
            .invoke(serde_json::json!({ "task": "test" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("intercepted"));
    }

    #[test]
    fn register_native_subset() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["read_file".to_string()], None, None).unwrap();
        let names = reg.tool_names();
        assert!(names.contains(&"read_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
        assert!(!names.contains(&"list_dir".to_string()));
    }

    #[test]
    fn register_native_empty_registers_nothing() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &[], None, None).unwrap();
        assert!(reg.tool_names().is_empty());
    }

    // ── In-memory store for unit tests ──────────────────────────────────────

    use std::collections::HashMap;
    use std::sync::Mutex;
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
        fn append(&self, ns: &str, key: &str, value: &str) -> anyhow::Result<()> {
            let k = format!("{}\x00{}", ns, key);
            let mut m = self.0.lock().unwrap();
            let e = m.entry(k).or_default();
            if !e.is_empty() { e.push('\n'); }
            e.push_str(value);
            Ok(())
        }
        fn delete(&self, ns: &str, key: &str) -> anyhow::Result<bool> {
            Ok(self.0.lock().unwrap().remove(&format!("{}\x00{}", ns, key)).is_some())
        }
        fn iter(&self, ns: &str) -> anyhow::Result<Vec<(String, String)>> {
            let prefix = format!("{}\x00", ns);
            let m = self.0.lock().unwrap();
            Ok(m.iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(k, v)| (k[prefix.len()..].to_string(), v.clone()))
                .collect())
        }
        fn meta_version(&self) -> anyhow::Result<u64> { Ok(1) }
    }

    // ── Gap ④: extract_ns_key error paths ───────────────────────────────────

    #[tokio::test]
    async fn kv_get_missing_namespace_errors() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let err = tool.invoke(json!({"key": "my-note"})).await.unwrap_err();
        assert!(err.to_string().contains("namespace"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_get_missing_key_errors() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let err = tool.invoke(json!({"namespace": "agent:scratch"})).await.unwrap_err();
        assert!(err.to_string().contains("key"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_get_invalid_namespace_chars_errors() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let err = tool
            .invoke(json!({"namespace": "bad namespace!", "key": "my-note"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("namespace"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_set_missing_value_field_errors() {
        let tool = KvSet { store: SimpleStore::new_arc() };
        let err = tool
            .invoke(json!({"namespace": "agent:scratch", "key": "my-note"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("value"), "got: {err}");
    }

    // ── Gap ⑥: KvGet miss path returns "" ───────────────────────────────────

    #[tokio::test]
    async fn kv_get_miss_returns_empty_string() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let result = tool
            .invoke(json!({"namespace": "agent:scratch", "key": "nonexistent"}))
            .await
            .unwrap();
        assert_eq!(result, "", "absent key must return empty string");
    }

    // ── Gap ⑤: required_capability_for both branches ────────────────────────

    #[test]
    fn kv_get_required_cap_null_returns_empty_segment() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let cap = tool.required_capability_for(&Value::Null);
        assert!(matches!(&cap, Some(Capability::KbRead { segment }) if segment.is_empty()));
    }

    #[test]
    fn kv_get_required_cap_with_input_returns_namespace() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let cap = tool.required_capability_for(&json!({"namespace": "agent:scratch", "key": "k"}));
        assert!(matches!(&cap, Some(Capability::KbRead { segment }) if segment == "agent:scratch"));
    }

    #[test]
    fn kv_set_required_cap_null_returns_empty_segment() {
        let tool = KvSet { store: SimpleStore::new_arc() };
        let cap = tool.required_capability_for(&Value::Null);
        assert!(matches!(&cap, Some(Capability::KbWrite { segment }) if segment.is_empty()));
    }

    // ── Finding #6: MAX_KV_VALUE_BYTES enforced in kv_set ───────────────────────

    #[tokio::test]
    async fn kv_set_oversized_value_errors() {
        let tool = KvSet { store: SimpleStore::new_arc() };
        let big = "x".repeat(MAX_KV_VALUE_BYTES + 1);
        let err = tool
            .invoke(json!({"namespace": "agent:scratch", "key": "big", "value": big}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_set_max_size_value_succeeds() {
        let tool = KvSet { store: SimpleStore::new_arc() };
        let exact = "x".repeat(MAX_KV_VALUE_BYTES);
        let result = tool
            .invoke(json!({"namespace": "agent:scratch", "key": "big", "value": exact}))
            .await
            .unwrap();
        assert!(result.contains("bytes"), "got: {result}");
    }

    // ── Gap ⑦: register_native with store=None + kv tools → silent skip ──────

    #[test]
    fn register_native_kv_tools_without_store_silently_skips() {
        let mut reg = ToolRegistry::new();
        register_native(
            &mut reg,
            &["kv_get".to_string(), "kv_set".to_string()],
            None,
            None,
        )
        .unwrap();
        assert!(
            !reg.tool_names().contains(&"kv_get".to_string()),
            "kv_get must not be registered without store"
        );
        assert!(
            !reg.tool_names().contains(&"kv_set".to_string()),
            "kv_set must not be registered without store"
        );
    }
}
