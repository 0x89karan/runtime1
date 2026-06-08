use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::capability::Capability;
use super::{Tool, ToolRegistry};

const READ_FILE_MAX: usize = 100_000;

pub struct ReadFile;
pub struct WriteFile;
pub struct ListDir;

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
            std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
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
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating directories for {path}"))?;
            }
        }
        std::fs::write(path, content).with_context(|| format!("writing {path}"))?;
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
        let mut entries = std::fs::read_dir(path)
            .with_context(|| format!("reading directory {path}"))?
            .map(|e| {
                let e = e.context("reading directory entry")?;
                let name = e.file_name().to_string_lossy().to_string();
                let suffix = if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    "/"
                } else {
                    ""
                };
                Ok(format!("{name}{suffix}"))
            })
            .collect::<Result<Vec<String>>>()?;
        entries.sort();
        Ok(entries.join("\n"))
    }
}

/// Register native tools by name. Pass `["all"]` to register all of them,
/// or a subset by name (e.g. `["read_file", "list_dir"]`).
/// Returns an error if any name collides with an already-registered tool.
pub fn register_native(reg: &mut ToolRegistry, names: &[String]) -> anyhow::Result<()> {
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
    fn register_native_all_registers_all_three() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()]).unwrap();
        let names = reg.tool_names();
        assert!(names.contains(&"read_file".to_string()));
        assert!(names.contains(&"write_file".to_string()));
        assert!(names.contains(&"list_dir".to_string()));
    }

    #[test]
    fn register_native_subset() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["read_file".to_string()]).unwrap();
        let names = reg.tool_names();
        assert!(names.contains(&"read_file".to_string()));
        assert!(!names.contains(&"write_file".to_string()));
        assert!(!names.contains(&"list_dir".to_string()));
    }

    #[test]
    fn register_native_empty_registers_nothing() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &[]).unwrap();
        assert!(reg.tool_names().is_empty());
    }
}
