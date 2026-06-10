use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::capability::Capability;
use crate::config::AgentCard;
use super::{Tool, ToolRegistry};

const READ_FILE_MAX: usize = 100_000;

pub struct ReadFile;
pub struct WriteFile;
pub struct ListDir;
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

/// Register native tools by name. Pass `["all"]` to register all of them,
/// or a subset by name (e.g. `["read_file", "list_dir"]`).
/// Returns an error if any name collides with an already-registered tool.
///
/// `cards` is required when registering `list_agents`; ignored otherwise.
pub fn register_native(
    reg: &mut ToolRegistry,
    names: &[String],
    cards: Option<Arc<Vec<AgentCard>>>,
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
    fn register_native_all_registers_all_six() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None).unwrap();
        let names = reg.tool_names();
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"list_dir".to_string()));
        assert!(names.contains(&"spawn_agent".to_string()));
        assert!(names.contains(&"list_agents".to_string()));
        assert!(names.contains(&"send_message".to_string()));
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
        register_native(&mut reg, &["read_file".to_string()], None).unwrap();
        let names = reg.tool_names();
        assert!(names.contains(&"read_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
        assert!(!names.contains(&"list_dir".to_string()));
    }

    #[test]
    fn register_native_empty_registers_nothing() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &[], None).unwrap();
        assert!(reg.tool_names().is_empty());
    }
}
