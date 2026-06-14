use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::capability::Capability;
use crate::config::AgentCard;
use crate::memory::{validate_segment, MemoryStore, MutabilityClass};
use super::{Tool, ToolContext, ToolRegistry};

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

/// Commit a piece of knowledge to Tier-3 long-term memory under the agent's own namespace.
/// NOT registered under `native = ["all"]` — requires explicit listing.
pub struct MemRemember {
    pub store: Arc<dyn MemoryStore>,
}

/// Search Tier-3 long-term memory for entries matching a query string.
/// NOT registered under `native = ["all"]` — requires explicit listing.
pub struct MemRecall {
    pub store: Arc<dyn MemoryStore>,
}

/// Write an entry to a shared knowledge-base segment (Tier 4, p5.4+).
/// NOT registered under `native = ["all"]` — requires explicit listing.
pub struct KbPut {
    pub store: Arc<dyn MemoryStore>,
}

/// Read a single entry from a shared knowledge-base segment (Tier 4, p5.4+).
/// NOT registered under `native = ["all"]` — requires explicit listing.
pub struct KbGet {
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

    async fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
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

    async fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
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

    async fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
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

    async fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
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

    async fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
        let (namespace, key) = extract_ns_key(&input)?;
        // Guard canon and log segments — only scratch/unclassified is writable
        // via kv_set. kb_put enforces this for the structured KB path; kv_set
        // must enforce the same invariant on the raw KV path.
        match self.store.segment_class(&namespace)? {
            Some(MutabilityClass::Canon) => {
                anyhow::bail!("namespace {namespace:?} is canon: agent writes are denied");
            }
            Some(MutabilityClass::Log) => {
                anyhow::bail!(
                    "namespace {namespace:?} is a log segment: \
                     use kb_put to append a structured log entry"
                );
            }
            _ => {} // Scratch or unclassified — allow
        }
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

const MAX_MEM_CONTENT_BYTES: usize = 8 * 1024; // 8 KiB per long-term memory entry

#[async_trait]
impl Tool for MemRemember {
    fn name(&self) -> &str {
        "mem_remember"
    }

    fn description(&self) -> &str {
        "Commit a piece of knowledge to long-term memory. Persists across restarts. \
         Use this to record findings, decisions, or facts you want to recall later. \
         Stored under your own agent namespace — other agents cannot read it."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The text to remember (max 8 KiB)"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional labels to help recall later"
                }
            },
            "required": ["content"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, _input: &Value) -> Option<Capability> {
        // Implicit self-grant: always writes to agent/{ctx.agent_id}; no cap needed.
        None
    }

    async fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("content must be a string"))?;
        let tags: Vec<String> = input["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let ts_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .map_err(|e| anyhow::anyhow!("system clock error: {e}"))?;
        let key = format!("{ts_ns:016x}");
        let namespace = format!("agent/{}", ctx.agent_id);
        validate_segment(&namespace, "agent namespace")?;

        let entry = serde_json::to_string(&serde_json::json!({
            "content": content,
            "tags": tags,
            "provenance": {
                "agent_id": ctx.agent_id,
                "turn": ctx.turn,
                "ts": ts_ns,
                "task_fp": ctx.task_fp,
            }
        }))?;
        if entry.len() > MAX_MEM_CONTENT_BYTES {
            anyhow::bail!(
                "entry too large: {} bytes (content + tags + provenance) exceeds limit of {} bytes",
                entry.len(),
                MAX_MEM_CONTENT_BYTES
            );
        }

        let store = Arc::clone(&self.store);
        let ns = namespace.clone();
        let k = key.clone();
        let entry_clone = entry.clone();
        tokio::task::spawn_blocking(move || store.put(&ns, &k, &entry_clone))
            .await
            .context("mem_remember spawn_blocking join")??;

        Ok(format!(
            "remembered: key={key} namespace={namespace} bytes={}",
            entry.len()
        ))
    }
}

#[async_trait]
impl Tool for MemRecall {
    fn name(&self) -> &str {
        "mem_recall"
    }

    fn description(&self) -> &str {
        "Search your long-term memory for entries matching a query string. \
         Returns matching entries as JSON, newest first. \
         Fields: query (required), limit (optional integer, default 10, max 50)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Substring to search for in remembered content and tags"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum results to return (default 10, max 50)"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, _input: &Value) -> Option<Capability> {
        // Implicit self-grant: always reads from agent/{ctx.agent_id}; no cap needed.
        None
    }

    async fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let query = input["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("query must be a string"))?
            .to_lowercase();
        anyhow::ensure!(!query.is_empty(), "query must not be empty");
        let limit = input["limit"].as_u64().unwrap_or(10).min(50) as usize;

        let ns = format!("agent/{}", ctx.agent_id);
        validate_segment(&ns, "agent namespace")?;
        let store = Arc::clone(&self.store);
        let mut entries: Vec<(String, String)> =
            tokio::task::spawn_blocking(move || store.iter(&ns))
                .await
                .context("mem_recall spawn_blocking join")??;

        // Newest first — keys are hex-encoded nanosecond timestamps.
        entries.sort_by(|a, b| b.0.cmp(&a.0));

        let matches: Vec<serde_json::Value> = entries
            .into_iter()
            .filter_map(|(key, val)| {
                let parsed: serde_json::Value = serde_json::from_str(&val).ok()?;
                let content_hit = parsed["content"]
                    .as_str()
                    .map(|c| c.to_lowercase().contains(&query))
                    .unwrap_or(false);
                let tags_hit = parsed["tags"]
                    .as_array()
                    .map(|arr| {
                        arr.iter().any(|t| {
                            t.as_str()
                                .map(|s| s.to_lowercase().contains(&query))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                if content_hit || tags_hit {
                    let mut entry = parsed;
                    entry["key"] = serde_json::Value::String(key);
                    Some(entry)
                } else {
                    None
                }
            })
            .take(limit)
            .collect();

        Ok(serde_json::to_string(&matches)?)
    }
}

// ── Shared KB tools (Tier 4, p5.4) ─────────────────────────────────────────

#[async_trait]
impl Tool for KbPut {
    fn name(&self) -> &str { "kb_put" }

    fn description(&self) -> &str {
        "Write an entry to a shared knowledge-base segment (Tier 4). \
         The segment's mutability class is set by operator config: \
         canon segments deny agent writes; log segments append a new immutable \
         entry on each call (key auto-generated); scratch segments do \
         last-writer-wins with a caller-provided key and an incrementing version."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "segment": {
                    "type": "string",
                    "description": "KB segment name (namespace)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to store (max 8 KiB)"
                },
                "key": {
                    "type": "string",
                    "description": "Entry key — required for scratch segments, ignored for log"
                },
                "citation": {
                    "type": "string",
                    "description": "Optional source citation stored in provenance"
                }
            },
            "required": ["segment", "content"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, input: &Value) -> Option<Capability> {
        let segment = input["segment"].as_str().unwrap_or("");
        Some(Capability::KbWrite { segment: segment.to_string() })
    }

    async fn invoke(&self, input: Value, ctx: &ToolContext) -> Result<String> {
        let segment = input["segment"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("segment must be a string"))?;
        validate_segment(segment, "segment")?;

        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("content must be a string"))?;

        let citation: Option<&str> = input["citation"].as_str();

        let class = self.store.segment_class(segment)?
            .unwrap_or(MutabilityClass::Scratch);

        match class {
            MutabilityClass::Canon => {
                anyhow::bail!("segment {segment:?} is canon: agent writes are denied");
            }
            MutabilityClass::Log => {
                let seq = self.store.next_log_seq(segment)?;
                let key = format!("{seq:016x}");
                let ts = chrono::Utc::now().to_rfc3339();
                let entry = serde_json::to_string(&json!({
                    "content": content,
                    "class": "log",
                    "version": 1,
                    "provenance": {
                        "agent_id": &ctx.agent_id,
                        "turn": ctx.turn,
                        "task_fp": &ctx.task_fp,
                        "ts": ts,
                        "citation": citation,
                    }
                }))?;
                anyhow::ensure!(
                    entry.len() <= MAX_MEM_CONTENT_BYTES,
                    "entry too large: {} bytes exceeds limit of {} bytes",
                    entry.len(),
                    MAX_MEM_CONTENT_BYTES
                );
                self.store.put(segment, &key, &entry)?;
                Ok(serde_json::to_string(&json!({"key": key, "class": "log"}))?)
            }
            MutabilityClass::Scratch => {
                let key = input["key"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("key is required for scratch segments"))?;
                validate_segment(key, "key")?;
                // Atomic version bump — prevents two concurrent writers from both
                // producing the same version number for the same key.
                let version = self.store.next_scratch_version(segment, key)?;
                let ts = chrono::Utc::now().to_rfc3339();
                let entry = serde_json::to_string(&json!({
                    "content": content,
                    "class": "scratch",
                    "version": version,
                    "provenance": {
                        "agent_id": &ctx.agent_id,
                        "turn": ctx.turn,
                        "task_fp": &ctx.task_fp,
                        "ts": ts,
                        "citation": citation,
                    }
                }))?;
                anyhow::ensure!(
                    entry.len() <= MAX_MEM_CONTENT_BYTES,
                    "entry too large: {} bytes exceeds limit of {} bytes",
                    entry.len(),
                    MAX_MEM_CONTENT_BYTES
                );
                self.store.put(segment, key, &entry)?;
                Ok(serde_json::to_string(&json!({
                    "key": key,
                    "class": "scratch",
                    "version": version,
                }))?)
            }
        }
    }
}

#[async_trait]
impl Tool for KbGet {
    fn name(&self) -> &str { "kb_get" }

    fn description(&self) -> &str {
        "Read a single knowledge-base entry by segment and key. \
         Returns the full entry JSON (including provenance), or an empty string \
         when the key does not exist."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "segment": {
                    "type": "string",
                    "description": "KB segment name"
                },
                "key": {
                    "type": "string",
                    "description": "Entry key (as returned by kb_put)"
                }
            },
            "required": ["segment", "key"],
            "additionalProperties": false
        })
    }

    fn required_capability_for(&self, input: &Value) -> Option<Capability> {
        let segment = input["segment"].as_str().unwrap_or("");
        Some(Capability::KbRead { segment: segment.to_string() })
    }

    async fn invoke(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
        let segment = input["segment"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("segment must be a string"))?;
        validate_segment(segment, "segment")?;

        let key = input["key"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("key must be a string"))?;
        validate_segment(key, "key")?;

        match self.store.get(segment, key)? {
            Some(entry) => Ok(entry),
            None => Ok(String::new()),
        }
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

    async fn invoke(&self, _input: Value, _ctx: &ToolContext) -> Result<String> {
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

    async fn invoke(&self, _input: Value, _ctx: &ToolContext) -> Result<String> {
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

    async fn invoke(&self, _input: Value, _ctx: &ToolContext) -> Result<String> {
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
/// **Note:** `kv_get`, `kv_set`, `mem_remember`, `mem_recall`, `kb_put`, and
/// `kb_get` are NOT included in `"all"`. They must be requested explicitly
/// because they require memory capability grants — auto-registering them for
/// every agent would produce noisy capability-denied events.
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
    // mem_remember / mem_recall — NOT included in "all"; require explicit opt-in.
    if names.iter().any(|n| n == "mem_remember") {
        if let Some(s) = store.clone() {
            reg.register(Box::new(MemRemember { store: s }))?;
        }
    }
    if names.iter().any(|n| n == "mem_recall") {
        if let Some(s) = store.clone() {
            reg.register(Box::new(MemRecall { store: s }))?;
        }
    }
    // kb_put / kb_get — NOT included in "all"; require explicit opt-in.
    if names.iter().any(|n| n == "kb_put") {
        if let Some(s) = store.clone() {
            reg.register(Box::new(KbPut { store: s }))?;
        }
    }
    if names.iter().any(|n| n == "kb_get") {
        if let Some(s) = store.clone() {
            reg.register(Box::new(KbGet { store: s }))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ctx() -> ToolContext {
        ToolContext { agent_id: "test".to_string(), turn: 0, task_fp: String::new() }
    }

    #[tokio::test]
    async fn read_file_returns_cargo_toml() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let result = ReadFile
            .invoke(json!({"path": path.to_str().unwrap()}), &ctx())
            .await
            .unwrap();
        assert!(!result.is_empty());
        assert!(result.contains("agentd"));
    }

    #[tokio::test]
    async fn read_file_missing_path_errors() {
        let err = ReadFile
            .invoke(json!({"path": "/nonexistent/p0.3-test-file.txt"}), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("reading"));
    }

    #[tokio::test]
    async fn read_file_missing_input_key_errors() {
        let err = ReadFile.invoke(json!({}), &ctx()).await.unwrap_err();
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
            .invoke(json!({"path": path.to_str().unwrap()}), &ctx())
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
            .invoke(json!({"path": path.to_str().unwrap(), "content": "hello p0.3"}), &ctx())
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
            .invoke(json!({"path": path.to_str().unwrap(), "content": "nested"}), &ctx())
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "nested");
    }

    #[tokio::test]
    async fn write_file_missing_path_errors() {
        let err = WriteFile.invoke(json!({"content": "hi"}), &ctx()).await.unwrap_err();
        assert!(err.to_string().contains("path"));
    }

    #[tokio::test]
    async fn write_file_missing_content_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.txt");
        let err = WriteFile
            .invoke(json!({"path": path.to_str().unwrap()}), &ctx())
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
            .invoke(json!({"path": dir.path().to_str().unwrap()}), &ctx())
            .await
            .unwrap();
        assert!(result.contains("subdir/"), "dirs must end with /");
        assert!(result.contains("file.txt"));
        assert!(!result.contains("file.txt/"));
    }

    #[tokio::test]
    async fn list_dir_missing_path_key_errors() {
        let err = ListDir.invoke(json!({}), &ctx()).await.unwrap_err();
        assert!(err.to_string().contains("path"));
    }

    #[tokio::test]
    async fn list_dir_empty_dir_returns_empty_string() {
        let dir = TempDir::new().unwrap();
        let result = ListDir
            .invoke(json!({"path": dir.path().to_str().unwrap()}), &ctx())
            .await
            .unwrap();
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn list_dir_missing_path_errors() {
        let err = ListDir
            .invoke(json!({"path": "/nonexistent/p0.3-dir"}), &ctx())
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
            .invoke(json!({"path": dir.path().to_str().unwrap()}), &ctx())
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
        // kb tools must NOT be in "all"
        assert!(!names.contains(&"kb_put".to_string()), "kb_put must not be in 'all'");
        assert!(!names.contains(&"kb_get".to_string()), "kb_get must not be in 'all'");
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
            .invoke(serde_json::json!({ "task": "test" }), &ctx())
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
    use crate::memory::{MemoryStore, MutabilityClass};

    struct SimpleStore {
        data: Mutex<HashMap<String, String>>,
        classes: Mutex<HashMap<String, MutabilityClass>>,
        seqs: Mutex<HashMap<String, u64>>,
        scratch_versions: Mutex<HashMap<String, u64>>,
    }
    impl SimpleStore {
        fn new_arc() -> Arc<Self> {
            Arc::new(SimpleStore {
                data: Mutex::new(HashMap::new()),
                classes: Mutex::new(HashMap::new()),
                seqs: Mutex::new(HashMap::new()),
                scratch_versions: Mutex::new(HashMap::new()),
            })
        }
    }
    impl MemoryStore for SimpleStore {
        fn get(&self, ns: &str, key: &str) -> anyhow::Result<Option<String>> {
            Ok(self.data.lock().unwrap().get(&format!("{}\x00{}", ns, key)).cloned())
        }
        fn put(&self, ns: &str, key: &str, value: &str) -> anyhow::Result<()> {
            self.data.lock().unwrap().insert(format!("{}\x00{}", ns, key), value.to_string());
            Ok(())
        }
        fn append(&self, ns: &str, key: &str, value: &str) -> anyhow::Result<()> {
            let k = format!("{}\x00{}", ns, key);
            let mut m = self.data.lock().unwrap();
            let e = m.entry(k).or_default();
            if !e.is_empty() { e.push('\n'); }
            e.push_str(value);
            Ok(())
        }
        fn delete(&self, ns: &str, key: &str) -> anyhow::Result<bool> {
            Ok(self.data.lock().unwrap().remove(&format!("{}\x00{}", ns, key)).is_some())
        }
        fn iter(&self, ns: &str) -> anyhow::Result<Vec<(String, String)>> {
            let prefix = format!("{}\x00", ns);
            let m = self.data.lock().unwrap();
            Ok(m.iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(k, v)| (k[prefix.len()..].to_string(), v.clone()))
                .collect())
        }
        fn meta_version(&self) -> anyhow::Result<u64> { Ok(1) }
        fn segment_class(&self, namespace: &str) -> anyhow::Result<Option<MutabilityClass>> {
            Ok(self.classes.lock().unwrap().get(namespace).cloned())
        }
        fn set_segment_class(&self, namespace: &str, class: MutabilityClass) -> anyhow::Result<()> {
            self.classes.lock().unwrap().insert(namespace.to_string(), class);
            Ok(())
        }
        fn next_log_seq(&self, namespace: &str) -> anyhow::Result<u64> {
            let mut seqs = self.seqs.lock().unwrap();
            let seq = seqs.entry(namespace.to_string()).or_insert(0);
            *seq += 1;
            Ok(*seq)
        }
        fn next_scratch_version(&self, namespace: &str, key: &str) -> anyhow::Result<u64> {
            let mut versions = self.scratch_versions.lock().unwrap();
            let v = versions.entry(format!("{namespace}\x00{key}")).or_insert(0);
            *v += 1;
            Ok(*v)
        }
    }

    // ── Gap ④: extract_ns_key error paths ───────────────────────────────────

    #[tokio::test]
    async fn kv_get_missing_namespace_errors() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let err = tool.invoke(json!({"key": "my-note"}), &ctx()).await.unwrap_err();
        assert!(err.to_string().contains("namespace"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_get_missing_key_errors() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let err = tool.invoke(json!({"namespace": "agent:scratch"}), &ctx()).await.unwrap_err();
        assert!(err.to_string().contains("key"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_get_invalid_namespace_chars_errors() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let err = tool
            .invoke(json!({"namespace": "bad namespace!", "key": "my-note"}), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("namespace"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_set_missing_value_field_errors() {
        let tool = KvSet { store: SimpleStore::new_arc() };
        let err = tool
            .invoke(json!({"namespace": "agent:scratch", "key": "my-note"}), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("value"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_set_canon_segment_denied() {
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:canon", MutabilityClass::Canon).unwrap();
        let tool = KvSet { store };
        let err = tool
            .invoke(json!({"namespace": "kb:canon", "key": "any", "value": "data"}), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("canon"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_set_log_segment_denied() {
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:events", MutabilityClass::Log).unwrap();
        let tool = KvSet { store };
        let err = tool
            .invoke(json!({"namespace": "kb:events", "key": "any", "value": "data"}), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("log segment"), "got: {err}");
    }

    // ── Gap ⑥: KvGet miss path returns "" ───────────────────────────────────

    #[tokio::test]
    async fn kv_get_miss_returns_empty_string() {
        let tool = KvGet { store: SimpleStore::new_arc() };
        let result = tool
            .invoke(json!({"namespace": "agent:scratch", "key": "nonexistent"}), &ctx())
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
            .invoke(json!({"namespace": "agent:scratch", "key": "big", "value": big}), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[tokio::test]
    async fn kv_set_max_size_value_succeeds() {
        let tool = KvSet { store: SimpleStore::new_arc() };
        let exact = "x".repeat(MAX_KV_VALUE_BYTES);
        let result = tool
            .invoke(json!({"namespace": "agent:scratch", "key": "big", "value": exact}), &ctx())
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

    // ── p5.3: mem_remember + mem_recall tests ───────────────────────────────

    fn mem_ctx(agent_id: &str) -> ToolContext {
        ToolContext {
            agent_id: agent_id.to_string(),
            turn: 1,
            task_fp: "abc123".to_string(),
        }
    }

    #[tokio::test]
    async fn remember_then_recall_finds_content() {
        let store = SimpleStore::new_arc();
        let remember = MemRemember { store: store.clone() };
        let recall = MemRecall { store: store.clone() };

        let result = remember
            .invoke(
                json!({"content": "the sky is blue", "tags": ["weather", "sky"]}),
                &mem_ctx("agent-a"),
            )
            .await
            .unwrap();
        assert!(result.contains("remembered:"), "got: {result}");

        // Recall by content substring.
        let hits = recall
            .invoke(json!({"query": "sky", "limit": 10}), &mem_ctx("agent-a"))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&hits).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1);
        assert_eq!(parsed[0]["content"], "the sky is blue");
        assert!(parsed[0]["key"].as_str().is_some(), "entry must have a key");
    }

    #[tokio::test]
    async fn recall_no_match_returns_empty_array() {
        let store = SimpleStore::new_arc();
        let remember = MemRemember { store: store.clone() };
        let recall = MemRecall { store: store.clone() };

        remember
            .invoke(json!({"content": "hello world"}), &mem_ctx("agent-a"))
            .await
            .unwrap();

        let hits = recall
            .invoke(json!({"query": "zzznomatch"}), &mem_ctx("agent-a"))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&hits).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn recall_tag_match_works() {
        let store = SimpleStore::new_arc();
        let remember = MemRemember { store: store.clone() };
        let recall = MemRecall { store: store.clone() };

        remember
            .invoke(
                json!({"content": "unrelated text", "tags": ["important", "priority"]}),
                &mem_ctx("agent-a"),
            )
            .await
            .unwrap();

        let hits = recall
            .invoke(json!({"query": "priority"}), &mem_ctx("agent-a"))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&hits).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1, "tag match must find entry");
    }

    #[tokio::test]
    async fn self_namespace_access_without_explicit_cap_grant() {
        // mem_remember / mem_recall require no capability — they use an implicit
        // self-namespace (agent/{id}) and return None from required_capability_for.
        let remember = MemRemember { store: SimpleStore::new_arc() };
        let recall = MemRecall { store: SimpleStore::new_arc() };
        assert!(
            remember.required_capability_for(&serde_json::Value::Null).is_none(),
            "mem_remember must require no capability"
        );
        assert!(
            recall.required_capability_for(&serde_json::Value::Null).is_none(),
            "mem_recall must require no capability"
        );
    }

    #[tokio::test]
    async fn cross_agent_namespaces_are_isolated() {
        // Agent-A and agent-B each have separate namespaces; recalls are scoped
        // to the calling agent's context, so B's memories are invisible to A.
        let store = SimpleStore::new_arc();
        let remember = MemRemember { store: store.clone() };
        let recall = MemRecall { store: store.clone() };

        remember
            .invoke(json!({"content": "secret from agent-b"}), &mem_ctx("agent-b"))
            .await
            .unwrap();

        // Agent-A's recall must not see agent-B's memories.
        let hits = recall
            .invoke(json!({"query": "secret"}), &mem_ctx("agent-a"))
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&hits).unwrap();
        assert_eq!(
            parsed.as_array().unwrap().len(),
            0,
            "agent-a must not see agent-b's memories"
        );
    }

    #[tokio::test]
    async fn remember_oversized_content_errors() {
        let store = SimpleStore::new_arc();
        let remember = MemRemember { store };
        let big = "x".repeat(MAX_MEM_CONTENT_BYTES + 1);
        let err = remember
            .invoke(json!({"content": big}), &mem_ctx("agent-a"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[tokio::test]
    async fn register_native_mem_tools_without_store_silently_skips() {
        let mut reg = ToolRegistry::new();
        register_native(
            &mut reg,
            &["mem_remember".to_string(), "mem_recall".to_string()],
            None,
            None,
        )
        .unwrap();
        assert!(
            !reg.tool_names().contains(&"mem_remember".to_string()),
            "mem_remember must not be registered without store"
        );
        assert!(
            !reg.tool_names().contains(&"mem_recall".to_string()),
            "mem_recall must not be registered without store"
        );
    }

    #[test]
    fn register_native_all_does_not_include_mem_tools() {
        let mut reg = ToolRegistry::new();
        register_native(&mut reg, &["all".to_string()], None, None).unwrap();
        let names = reg.tool_names();
        assert!(!names.contains(&"mem_remember".to_string()), "mem_remember must not be in 'all'");
        assert!(!names.contains(&"mem_recall".to_string()), "mem_recall must not be in 'all'");
    }

    #[tokio::test]
    async fn register_native_kb_tools_without_store_silently_skips() {
        let mut reg = ToolRegistry::new();
        register_native(
            &mut reg,
            &["kb_put".to_string(), "kb_get".to_string()],
            None,
            None,
        )
        .unwrap();
        assert!(
            !reg.tool_names().contains(&"kb_put".to_string()),
            "kb_put must not be registered without store"
        );
        assert!(
            !reg.tool_names().contains(&"kb_get".to_string()),
            "kb_get must not be registered without store"
        );
    }

    #[tokio::test]
    async fn remember_missing_content_field_errors() {
        let tool = MemRemember { store: SimpleStore::new_arc() };
        let err = tool.invoke(json!({}), &mem_ctx("agent-a")).await.unwrap_err();
        assert!(err.to_string().contains("content"), "got: {err}");
    }

    #[tokio::test]
    async fn recall_missing_query_field_errors() {
        let tool = MemRecall { store: SimpleStore::new_arc() };
        let err = tool.invoke(json!({}), &mem_ctx("agent-a")).await.unwrap_err();
        assert!(err.to_string().contains("query"), "got: {err}");
    }

    #[tokio::test]
    async fn recall_limit_clamped_at_50() {
        // Insert 60 matching entries directly; recall with limit: 100 must return ≤50.
        let store = SimpleStore::new_arc();
        let ns = "agent/agent-a";
        for i in 0u64..60 {
            let key = format!("{i:016x}");
            let entry = serde_json::to_string(&serde_json::json!({
                "content": "hello world",
                "tags": [],
                "provenance": { "agent_id": "agent-a", "turn": 0, "ts": i, "task_fp": "" },
            }))
            .unwrap();
            store.put(ns, &key, &entry).unwrap();
        }
        let recall = MemRecall { store };
        let hits = recall
            .invoke(json!({"query": "hello", "limit": 100}), &mem_ctx("agent-a"))
            .await
            .unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&hits).unwrap();
        assert_eq!(parsed.len(), 50, "limit must be clamped to 50; got {}", parsed.len());
    }

    #[tokio::test]
    async fn recall_default_limit_is_10() {
        // Insert 15 matching entries; recall without explicit limit must return exactly 10.
        let store = SimpleStore::new_arc();
        let ns = "agent/agent-a";
        for i in 0u64..15 {
            let key = format!("{i:016x}");
            let entry = serde_json::to_string(&serde_json::json!({
                "content": "hello world",
                "tags": [],
                "provenance": { "agent_id": "agent-a", "turn": 0, "ts": i, "task_fp": "" },
            }))
            .unwrap();
            store.put(ns, &key, &entry).unwrap();
        }
        let recall = MemRecall { store };
        // No `limit` field → must default to 10.
        let hits = recall
            .invoke(json!({"query": "hello"}), &mem_ctx("agent-a"))
            .await
            .unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&hits).unwrap();
        assert_eq!(parsed.len(), 10, "default limit must be 10; got {}", parsed.len());
    }

    #[tokio::test]
    async fn recall_newest_first_ordering() {
        // Insert 3 entries with known hex keys (simulated timestamps 100, 200, 300).
        // MemRecall sorts keys descending → expect 300, 200, 100 order.
        let store = SimpleStore::new_arc();
        let ns = "agent/agent-a";
        for ts in [100u64, 200u64, 300u64] {
            let key = format!("{ts:016x}");
            let entry = serde_json::to_string(&serde_json::json!({
                "content": format!("entry at ts={ts}"),
                "tags": [],
                "provenance": { "agent_id": "agent-a", "turn": 0, "ts": ts, "task_fp": "" },
            }))
            .unwrap();
            store.put(ns, &key, &entry).unwrap();
        }
        let recall = MemRecall { store };
        let hits = recall
            .invoke(json!({"query": "entry at ts=", "limit": 10}), &mem_ctx("agent-a"))
            .await
            .unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&hits).unwrap();
        assert_eq!(parsed.len(), 3);
        assert!(
            parsed[0]["content"].as_str().unwrap().contains("300"),
            "expected ts=300 first, got: {}",
            parsed[0]["content"]
        );
        assert!(
            parsed[1]["content"].as_str().unwrap().contains("200"),
            "expected ts=200 second"
        );
        assert!(
            parsed[2]["content"].as_str().unwrap().contains("100"),
            "expected ts=100 third"
        );
    }

    // ── p5.4: shared KB (kb_put / kb_get) tests ──────────────────────────────

    fn kb_ctx(agent_id: &str) -> ToolContext {
        ToolContext {
            agent_id: agent_id.to_string(),
            turn: 2,
            task_fp: "fp1234567890abcd".to_string(),
        }
    }

    #[tokio::test]
    async fn log_segment_is_append_only_immutable() {
        // Log segments produce a new, unique key on every kb_put.
        // Each key is monotonically increasing (can be ordered by key).
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:events", MutabilityClass::Log).unwrap();
        let put = KbPut { store: store.clone() };

        let r1 = put.invoke(json!({"segment": "kb:events", "content": "first"}),
            &kb_ctx("agent-a")).await.unwrap();
        let r2 = put.invoke(json!({"segment": "kb:events", "content": "second"}),
            &kb_ctx("agent-a")).await.unwrap();

        let v1: serde_json::Value = serde_json::from_str(&r1).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&r2).unwrap();

        let k1 = v1["key"].as_str().unwrap().to_string();
        let k2 = v2["key"].as_str().unwrap().to_string();
        assert_ne!(k1, k2, "log segment must produce unique keys per write");
        assert!(k1 < k2, "log keys must be monotonically increasing");
        assert_eq!(v1["class"], "log");
        assert_eq!(v2["class"], "log");

        // Verify both entries exist in the store with distinct content.
        let e1 = store.get("kb:events", &k1).unwrap().unwrap();
        let e2 = store.get("kb:events", &k2).unwrap().unwrap();
        let e1v: serde_json::Value = serde_json::from_str(&e1).unwrap();
        let e2v: serde_json::Value = serde_json::from_str(&e2).unwrap();
        assert_eq!(e1v["content"], "first");
        assert_eq!(e2v["content"], "second");
    }

    #[tokio::test]
    async fn scratch_last_writer_wins_increments_version() {
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:notes", MutabilityClass::Scratch).unwrap();
        let put = KbPut { store: store.clone() };

        let r1 = put.invoke(
            json!({"segment": "kb:notes", "content": "v1", "key": "status"}),
            &kb_ctx("agent-a"),
        ).await.unwrap();
        let r2 = put.invoke(
            json!({"segment": "kb:notes", "content": "v2", "key": "status"}),
            &kb_ctx("agent-a"),
        ).await.unwrap();

        let v1: serde_json::Value = serde_json::from_str(&r1).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&r2).unwrap();
        assert_eq!(v1["version"], 1, "first write must be version 1");
        assert_eq!(v2["version"], 2, "second write must be version 2");
        assert_eq!(v1["key"], "status");
        assert_eq!(v2["key"], "status");

        // Verify only the latest content survives.
        let stored = store.get("kb:notes", "status").unwrap().unwrap();
        let sv: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(sv["content"], "v2", "scratch must store latest content");
        assert_eq!(sv["version"], 2u64);
    }

    #[tokio::test]
    async fn canon_write_by_agent_denied() {
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:canon", MutabilityClass::Canon).unwrap();
        let put = KbPut { store };
        let err = put.invoke(
            json!({"segment": "kb:canon", "content": "attempt"}),
            &kb_ctx("agent-a"),
        ).await.unwrap_err();
        assert!(
            err.to_string().contains("canon"),
            "must mention canon in error; got: {err}"
        );
        assert!(
            err.to_string().contains("denied"),
            "must mention denied; got: {err}"
        );
    }

    #[tokio::test]
    async fn provenance_stamped_and_unforgeable() {
        // The provenance block must be stamped from ToolContext — not from tool input.
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:data", MutabilityClass::Log).unwrap();
        let put = KbPut { store: store.clone() };

        let ctx = ToolContext {
            agent_id: "agent-prov".to_string(),
            turn: 7,
            task_fp: "deadbeef00000000".to_string(),
        };
        let result = put.invoke(
            json!({"segment": "kb:data", "content": "important", "citation": "src:42"}),
            &ctx,
        ).await.unwrap();
        let rv: serde_json::Value = serde_json::from_str(&result).unwrap();
        let key = rv["key"].as_str().unwrap();

        let stored = store.get("kb:data", key).unwrap().unwrap();
        let sv: serde_json::Value = serde_json::from_str(&stored).unwrap();
        let prov = &sv["provenance"];

        assert_eq!(prov["agent_id"], "agent-prov", "agent_id must come from ToolContext");
        assert_eq!(prov["turn"], 7u64, "turn must come from ToolContext");
        assert_eq!(prov["task_fp"], "deadbeef00000000", "task_fp must come from ToolContext");
        assert_eq!(prov["citation"], "src:42");
        assert!(prov["ts"].as_str().is_some(), "ts must be an RFC3339 string");
    }

    #[tokio::test]
    async fn worked_example_a_logs_b_retrieves() {
        // Agent A writes to a shared log segment; agent B reads the entry back.
        let store = SimpleStore::new_arc();
        store.set_segment_class("shared:log", MutabilityClass::Log).unwrap();
        let put = KbPut { store: store.clone() };
        let get = KbGet { store: store.clone() };

        let put_result = put.invoke(
            json!({"segment": "shared:log", "content": "hello from agent-a"}),
            &kb_ctx("agent-a"),
        ).await.unwrap();
        let pv: serde_json::Value = serde_json::from_str(&put_result).unwrap();
        let key = pv["key"].as_str().unwrap().to_string();

        let get_result = get.invoke(
            json!({"segment": "shared:log", "key": key}),
            &kb_ctx("agent-b"),
        ).await.unwrap();

        assert!(!get_result.is_empty(), "agent-b must see agent-a's entry");
        let gv: serde_json::Value = serde_json::from_str(&get_result).unwrap();
        assert_eq!(gv["content"], "hello from agent-a");
        assert_eq!(gv["provenance"]["agent_id"], "agent-a");
        assert_eq!(gv["class"], "log");
    }

    // ── p5.4 gap tests: KbGet paths ──────────────────────────────────────────

    #[tokio::test]
    async fn kb_get_miss_returns_empty_string() {
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:log", MutabilityClass::Log).unwrap();
        let get = KbGet { store };
        let result = get
            .invoke(json!({"segment": "kb:log", "key": "nonexistent"}), &kb_ctx("agent-a"))
            .await
            .unwrap();
        assert_eq!(result, "", "absent key must return empty string");
    }

    #[tokio::test]
    async fn kb_get_missing_segment_field_errors() {
        let get = KbGet { store: SimpleStore::new_arc() };
        let err = get
            .invoke(json!({"key": "k"}), &kb_ctx("agent-a"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("segment"), "got: {err}");
    }

    #[tokio::test]
    async fn kb_get_missing_key_field_errors() {
        let get = KbGet { store: SimpleStore::new_arc() };
        let err = get
            .invoke(json!({"segment": "kb:log"}), &kb_ctx("agent-a"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("key"), "got: {err}");
    }

    #[test]
    fn kb_get_required_cap_null_returns_empty_segment() {
        let get = KbGet { store: SimpleStore::new_arc() };
        let cap = get.required_capability_for(&Value::Null);
        assert!(matches!(&cap, Some(Capability::KbRead { segment }) if segment.is_empty()));
    }

    #[test]
    fn kb_get_required_cap_with_segment_returns_it() {
        let get = KbGet { store: SimpleStore::new_arc() };
        let cap = get.required_capability_for(&json!({"segment": "kb:notes", "key": "k"}));
        assert!(matches!(&cap, Some(Capability::KbRead { segment }) if segment == "kb:notes"));
    }

    #[test]
    fn kb_put_required_cap_null_returns_empty_segment() {
        let put = KbPut { store: SimpleStore::new_arc() };
        let cap = put.required_capability_for(&Value::Null);
        assert!(matches!(&cap, Some(Capability::KbWrite { segment }) if segment.is_empty()));
    }

    #[test]
    fn kb_put_required_cap_with_segment_returns_it() {
        let put = KbPut { store: SimpleStore::new_arc() };
        let cap = put.required_capability_for(&json!({"segment": "kb:data", "content": "x"}));
        assert!(matches!(&cap, Some(Capability::KbWrite { segment }) if segment == "kb:data"));
    }

    // ── p5.4 gap tests: KbPut paths ──────────────────────────────────────────

    #[tokio::test]
    async fn kb_put_unregistered_segment_defaults_to_scratch() {
        // A segment with no class set should default to Scratch semantics.
        let store = SimpleStore::new_arc();
        // Do NOT set any class — segment is unregistered.
        let put = KbPut { store: store.clone() };
        let result = put
            .invoke(
                json!({"segment": "kb:unknown", "content": "hello", "key": "mykey"}),
                &kb_ctx("agent-a"),
            )
            .await
            .unwrap();
        let rv: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(rv["class"], "scratch", "unregistered segment must default to scratch");
        assert_eq!(rv["version"], 1u64);
    }

    #[tokio::test]
    async fn kb_put_oversized_content_log_errors() {
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:log", MutabilityClass::Log).unwrap();
        let put = KbPut { store };
        let big = "x".repeat(MAX_MEM_CONTENT_BYTES + 1);
        let err = put
            .invoke(json!({"segment": "kb:log", "content": big}), &kb_ctx("agent-a"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[tokio::test]
    async fn kb_put_oversized_content_scratch_errors() {
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:scratch", MutabilityClass::Scratch).unwrap();
        let put = KbPut { store };
        let big = "x".repeat(MAX_MEM_CONTENT_BYTES + 1);
        let err = put
            .invoke(
                json!({"segment": "kb:scratch", "content": big, "key": "k"}),
                &kb_ctx("agent-a"),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[tokio::test]
    async fn kb_put_missing_segment_field_errors() {
        let put = KbPut { store: SimpleStore::new_arc() };
        let err = put
            .invoke(json!({"content": "hello"}), &kb_ctx("agent-a"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("segment"), "got: {err}");
    }

    #[tokio::test]
    async fn kb_put_missing_content_field_errors() {
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:log", MutabilityClass::Log).unwrap();
        let put = KbPut { store };
        let err = put
            .invoke(json!({"segment": "kb:log"}), &kb_ctx("agent-a"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("content"), "got: {err}");
    }

    #[tokio::test]
    async fn kb_put_scratch_missing_key_errors() {
        let store = SimpleStore::new_arc();
        store.set_segment_class("kb:notes", MutabilityClass::Scratch).unwrap();
        let put = KbPut { store };
        let err = put
            .invoke(
                json!({"segment": "kb:notes", "content": "hello"}),
                &kb_ctx("agent-a"),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("key"),
            "scratch kb_put without key must error; got: {err}"
        );
    }

    #[tokio::test]
    async fn kbread_outside_grant_denied() {
        use crate::flight_recorder::FlightRecorder;
        use tempfile::NamedTempFile;

        let mut reg = ToolRegistry::new();
        let store = SimpleStore::new_arc();
        store.set_segment_class("shared:log", MutabilityClass::Log).unwrap();
        register_native(
            &mut reg,
            &["kb_get".to_string()],
            None,
            Some(store as Arc<dyn MemoryStore>),
        ).unwrap();

        let tmp = NamedTempFile::new().unwrap();
        let rec = FlightRecorder::new(tmp.path()).unwrap();
        let ctx = kb_ctx("agent-no-cap");

        let caps: &[crate::capability::Capability] = &[];
        let err = reg.invoke(
            "kb_get",
            json!({"segment": "shared:log", "key": "somekey"}),
            &ctx,
            Some(caps),
            &rec,
        ).await.unwrap_err();
        assert!(
            err.to_string().contains("capability denied"),
            "must report capability denied; got: {err}"
        );
    }

    #[tokio::test]
    async fn kbwrite_outside_grant_denied() {
        use crate::flight_recorder::FlightRecorder;
        use tempfile::NamedTempFile;

        let mut reg = ToolRegistry::new();
        let store = SimpleStore::new_arc();
        store.set_segment_class("shared:log", MutabilityClass::Log).unwrap();
        register_native(
            &mut reg,
            &["kb_put".to_string()],
            None,
            Some(store as Arc<dyn MemoryStore>),
        ).unwrap();

        let tmp = NamedTempFile::new().unwrap();
        let rec = FlightRecorder::new(tmp.path()).unwrap();
        let ctx = kb_ctx("agent-no-cap");

        // No KbWrite cap granted — must be denied.
        let caps: &[crate::capability::Capability] = &[];
        let err = reg.invoke(
            "kb_put",
            json!({"segment": "shared:log", "content": "unauthorized"}),
            &ctx,
            Some(caps),
            &rec,
        ).await.unwrap_err();
        assert!(
            err.to_string().contains("capability denied"),
            "must report capability denied; got: {err}"
        );
    }
}
