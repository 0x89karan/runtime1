use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    bus::MailMessage,
    config::{AgentConfig, ModelConfig, PendingActionRequest},
    inference::{InferenceResponse, Msg, ToolSpec},
    memory::MemItem,
};

pub const FORMAT_VERSION: u32 = 3;

/// Create `path` with mode 0600 on Unix, then write `data`.
///
/// On non-Unix (Windows) falls back to `tokio::fs::write` (different ACL model).
/// Used for checkpoint tmp files so the final `checkpoint.json` is never world-readable.
#[cfg(unix)]
async fn write_mode_600(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt as _;
    // Use O_CREAT|O_EXCL (create_new) so the mode argument is always honoured by
    // the kernel — on Linux, O_CREAT alone silently ignores mode when the file
    // already exists at a different permission.  If a stale tmp file exists (e.g.
    // from a crash), remove it first then retry once.
    let mut f = match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .await
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            tokio::fs::remove_file(path).await?;
            tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .await?
        }
        Err(e) => return Err(e),
    };
    f.write_all(data).await?;
    f.flush().await
}

#[cfg(not(unix))]
async fn write_mode_600(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    tokio::fs::write(path, data).await
}

/// Serializable snapshot of a single `AgentTask`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentCheckpoint {
    pub agent_id:        String,
    pub cfg:             AgentConfig,
    pub model_cfg:       ModelConfig,
    pub messages:        Vec<Msg>,
    pub specs:           Vec<ToolSpec>,
    pub total_input:     u64,
    pub total_output:    u64,
    pub turn:            u32,
    /// None = NeedInfer; Some = ResponseStored.
    pub stored_response: Option<InferenceResponse>,
    /// Always false when saved — guards against the terminal-race described in OV-2.
    pub terminal:        bool,
    /// Tier-2 eviction buffer: turns paged out of active context.
    /// `#[serde(default)]` makes v1 checkpoints (no field) load as empty vec.
    #[serde(default)]
    pub short_term:      Vec<MemItem>,
}

/// A serializable entry in the awaiting map (child → parent relationship).
#[derive(Debug, Serialize, Deserialize)]
pub struct AwaitingEntry {
    pub child_id:  String,
    pub parent_id: String,
    pub call_id:   String,
}

/// A serializable snapshot of a parked approval (written to checkpoint so
/// in-flight approvals survive restart). `created_at` is wall-clock time
/// and cannot be stored; the age resets to zero on restore (acceptable).
#[derive(Debug, Serialize, Deserialize)]
pub struct ParkedApprovalEntry {
    pub approval_id: String,
    pub agent_id:    String,
    pub call_id:     String,
    pub action:      PendingActionRequest,
    /// Scheduler-level sequence counter value at creation.
    /// Stored so we can reconstruct `approval_seq` after restore.
    pub seq:         u64,
}

/// Full scheduler checkpoint, written atomically on shutdown or periodic tick.
#[derive(Debug, Serialize, Deserialize)]
pub struct SchedulerCheckpoint {
    pub format_version: u32,
    pub agents:         Vec<AgentCheckpoint>,
    pub awaiting:       Vec<AwaitingEntry>,
    pub mailboxes:      HashMap<String, Vec<MailMessage>>,
    pub tokens_spent:   u64,
    pub child_seq:      u64,
    pub spawn_depths:   HashMap<String, u32>,
    #[serde(default)]
    pub parent_map:     HashMap<String, String>,
    /// Pending operator-approval requests (parked agents). Absent in v1/v2 → empty vec.
    #[serde(default)]
    pub pending_approvals: Vec<ParkedApprovalEntry>,
    /// Monotonic counter used to generate "act_{seq}" approval IDs.
    #[serde(default)]
    pub approval_seq:   u64,
}

/// Handles checkpoint I/O. Writes are atomic: tmp → rename.
pub struct CheckpointStore {
    path: PathBuf,
}

/// Minimal probe struct — deserialized first to check compatibility before
/// attempting full deserialization of the checkpoint.
#[derive(Deserialize)]
struct VersionProbe {
    format_version: u32,
}

impl CheckpointStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join("checkpoint.json"),
        }
    }

    /// Generate a unique tmp path per save call to prevent races when two agentd
    /// processes share a working directory (e.g. during OS boot overlap).
    fn tmp_path(&self) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let pid = std::process::id();
        self.path.with_file_name(format!("checkpoint.json.{pid}.{nanos}.tmp"))
    }

    /// Write `cp` atomically. Calls `tokio::fs::write` so the scheduler's async
    /// executor is not blocked; a crash after rename leaves the previous good
    /// checkpoint intact because the tmp file was written first.
    ///
    /// On Unix the tmp file is created with mode 0600 (owner read/write only).
    /// `rename(2)` preserves those permissions on the final `checkpoint.json`.
    pub async fn save(&self, cp: &SchedulerCheckpoint) -> Result<()> {
        let json = serde_json::to_string(cp).context("serialize checkpoint")?;
        let tmp = self.tmp_path();
        write_mode_600(&tmp, json.as_bytes())
            .await
            .context("write checkpoint tmp")?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .context("rename checkpoint tmp -> checkpoint.json")?;
        Ok(())
    }

    /// Read and parse the checkpoint file.
    /// Returns `Ok(None)` when the file is absent.
    /// Returns `Err` when the file is present but cannot be parsed (caller should
    /// rename to .corrupt and start fresh — never fail-stop a restart).
    pub fn load(&self) -> Result<Option<SchedulerCheckpoint>> {
        if !self.path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&self.path).context("read checkpoint.json")?;

        // Probe the version field before attempting full deserialization so we can
        // distinguish "too new" (intentional refusal) from "corrupt" (parse error).
        let probe: VersionProbe = serde_json::from_slice(&bytes)
            .context("checkpoint.json: cannot read format_version")?;
        if probe.format_version > FORMAT_VERSION {
            anyhow::bail!(
                "checkpoint format_version {} > supported {}; \
                 this checkpoint was written by a newer agentd — refusing to load",
                probe.format_version,
                FORMAT_VERSION
            );
        }

        let cp: SchedulerCheckpoint =
            serde_json::from_slice(&bytes).context("parse checkpoint.json")?;
        Ok(Some(cp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ModelConfig,
        inference::{Block, Role, StopReason},
    };
    use tempfile::TempDir;

    fn minimal_agent_checkpoint(id: &str) -> AgentCheckpoint {
        AgentCheckpoint {
            agent_id:    id.to_string(),
            cfg:         crate::config::AgentConfig {
                id:           id.to_string(),
                task:         "test task".to_string(),
                max_turns:    5,
                token_budget: 100_000,
                priority:     0,
                capabilities: None,
                name:         None,
                description:  String::new(),
                skills:       vec![],
            },
            model_cfg:       ModelConfig {
                provider:   "mock".to_string(),
                model:      "mock-model".to_string(),
                max_tokens: 1024,
                streaming:  false,
            },
            messages:        vec![Msg {
                role:   Role::User,
                blocks: vec![Block::Text { text: "hello".to_string() }],
            }],
            specs:           vec![],
            total_input:     10,
            total_output:    5,
            turn:            1,
            stored_response: None,
            terminal:        false,
            short_term:      vec![],
        }
    }

    fn minimal_scheduler_checkpoint() -> SchedulerCheckpoint {
        SchedulerCheckpoint {
            format_version:    FORMAT_VERSION,
            agents:            vec![minimal_agent_checkpoint("agent-a")],
            awaiting:          vec![],
            mailboxes:         HashMap::new(),
            tokens_spent:      15,
            child_seq:         0,
            spawn_depths:      [("agent-a".to_string(), 0)].into_iter().collect(),
            parent_map:        HashMap::new(),
            pending_approvals: vec![],
            approval_seq:      0,
        }
    }

    // ── serde roundtrips ──────────────────────────────────────────────────────

    #[test]
    fn agent_checkpoint_serde_roundtrip() {
        let cp = minimal_agent_checkpoint("test-agent");
        let json = serde_json::to_string(&cp).unwrap();
        let back: AgentCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent_id, "test-agent");
        assert_eq!(back.turn, 1);
        assert_eq!(back.total_input, 10);
        assert_eq!(back.total_output, 5);
        assert!(back.stored_response.is_none());
        assert!(!back.terminal);
    }

    #[test]
    fn stored_response_roundtrip() {
        let mut cp = minimal_agent_checkpoint("r");
        cp.stored_response = Some(InferenceResponse {
            blocks:        vec![Block::Text { text: "answer".to_string() }],
            stop_reason:   StopReason::EndTurn,
            input_tokens:  20,
            output_tokens: 10,
        });
        let json = serde_json::to_string(&cp).unwrap();
        let back: AgentCheckpoint = serde_json::from_str(&json).unwrap();
        let resp = back.stored_response.unwrap();
        assert_eq!(resp.input_tokens, 20);
        assert_eq!(resp.output_tokens, 10);
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn scheduler_checkpoint_full_roundtrip() {
        let mut cp = minimal_scheduler_checkpoint();
        cp.awaiting.push(AwaitingEntry {
            child_id:  "child-1".to_string(),
            parent_id: "agent-a".to_string(),
            call_id:   "call-xyz".to_string(),
        });
        cp.mailboxes.insert(
            "agent-a".to_string(),
            vec![MailMessage { from: "agent-b".to_string(), content: "hi".to_string() }],
        );
        let json = serde_json::to_string(&cp).unwrap();
        let back: SchedulerCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.format_version, FORMAT_VERSION);
        assert_eq!(back.agents.len(), 1);
        assert_eq!(back.awaiting.len(), 1);
        assert_eq!(back.awaiting[0].child_id, "child-1");
        assert_eq!(back.mailboxes["agent-a"][0].from, "agent-b");
        assert_eq!(back.tokens_spent, 15);
    }

    #[test]
    fn scheduler_checkpoint_parent_map_serde_default() {
        // Pre-p6.4 checkpoints have no "parent_map" field. Deserializing them must
        // succeed with an empty map, not fail. This guards the `#[serde(default)]`.
        let json = serde_json::json!({
            "format_version": 2,
            "agents": [],
            "awaiting": [],
            "mailboxes": {},
            "tokens_spent": 0,
            "child_seq": 0,
            "spawn_depths": {}
            // "parent_map", "pending_approvals", "approval_seq" are intentionally absent
        });
        let cp: SchedulerCheckpoint = serde_json::from_str(&serde_json::to_string(&json).unwrap()).unwrap();
        assert!(cp.parent_map.is_empty(), "missing parent_map field must deserialize to empty map");
        assert!(cp.pending_approvals.is_empty(), "missing pending_approvals must deserialize to empty vec");
        assert_eq!(cp.approval_seq, 0, "missing approval_seq must deserialize to 0");
    }

    #[test]
    fn scheduler_checkpoint_pending_approvals_serde_default() {
        // Pre-p7.4 checkpoints (format_version=2) have no pending_approvals field.
        // Deserializing them must succeed with an empty vec. Guards `#[serde(default)]`.
        let json = serde_json::json!({
            "format_version": 2,
            "agents": [],
            "awaiting": [],
            "mailboxes": {},
            "tokens_spent": 0,
            "child_seq": 0,
            "spawn_depths": {},
            "parent_map": {}
            // "pending_approvals" and "approval_seq" are intentionally absent
        });
        let cp: SchedulerCheckpoint = serde_json::from_str(&serde_json::to_string(&json).unwrap()).unwrap();
        assert!(cp.pending_approvals.is_empty());
        assert_eq!(cp.approval_seq, 0);
    }

    #[test]
    fn load_corrupt_json_returns_err() {
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        std::fs::write(&store.path, b"not valid json {{{{").unwrap();
        assert!(store.load().is_err(), "corrupt JSON must return Err");
    }

    #[test]
    fn load_future_format_version_returns_err() {
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        let json = serde_json::json!({
            "format_version": FORMAT_VERSION + 99,
            "agents": [],
            "awaiting": [],
            "mailboxes": {},
            "tokens_spent": 0,
            "child_seq": 0,
            "spawn_depths": {}
        });
        std::fs::write(&store.path, serde_json::to_string(&json).unwrap()).unwrap();
        let err = store.load().unwrap_err();
        let msg = format!("{err}");
        assert!(store.load().is_err(), "newer format_version must return Err");
        // Must be identified as "too new" (explicit refusal), not "corrupt".
        assert!(msg.contains("refusing to load"), "error must say 'refusing to load': {msg}");
    }

    #[test]
    fn load_missing_file_returns_none() {
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        assert!(store.load().unwrap().is_none());
    }

    #[tokio::test]
    async fn save_writes_to_tmp_then_renames() {
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        let cp = minimal_scheduler_checkpoint();
        store.save(&cp).await.unwrap();
        assert!(store.path.exists(), "checkpoint.json must exist after save");
        // No .tmp files should remain after a successful save.
        let tmp_files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(tmp_files.is_empty(), "no .tmp files must remain after rename");
    }

    #[tokio::test]
    async fn save_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        let cp = minimal_scheduler_checkpoint();
        store.save(&cp).await.unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.agents.len(), 1);
        assert_eq!(loaded.agents[0].agent_id, "agent-a");
        assert_eq!(loaded.tokens_spent, 15);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_sets_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        store.save(&minimal_scheduler_checkpoint()).await.unwrap();
        let meta = std::fs::metadata(&store.path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "checkpoint.json must be mode 0600, got 0o{mode:03o}");
    }

    /// write_mode_600: write_all failure propagates as Err.
    /// We trigger it by writing to a path inside a read-only directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn save_write_failure_returns_err() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        // Make the directory read-only so open(O_CREAT) fails.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let store = CheckpointStore::new(dir.path());
        let result = store.save(&minimal_scheduler_checkpoint()).await;
        // Restore permissions so TempDir can clean up.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err(), "save must return Err when write fails");
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("write checkpoint tmp"),
            "error context must mention the tmp write: {msg}"
        );
    }

    // AC12: v1 checkpoint (no `short_term` field) loads via CheckpointStore::load()
    // with short_term defaulting to empty vec. format_version=1 passes the guard.
    #[tokio::test]
    async fn v1_checkpoint_loads_with_empty_short_term() {
        use crate::memory::MemItem;

        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());

        // Build a v1-style checkpoint JSON with no `short_term` field.
        let v1_json = serde_json::json!({
            "format_version": 1,
            "agents": [{
                "agent_id": "v1-agent",
                "cfg": {
                    "id": "v1-agent",
                    "task": "test",
                    "max_turns": 5,
                    "token_budget": 100_000,
                    "priority": 0,
                    "capabilities": null,
                    "name": null,
                    "description": "",
                    "skills": []
                },
                "model_cfg": { "provider": "mock", "model": "mock", "max_tokens": 1024 },
                "messages": [],
                "specs": [],
                "total_input": 0,
                "total_output": 0,
                "turn": 0,
                "stored_response": null,
                "terminal": false
                // no "short_term" field — v1 format
            }],
            "awaiting": [],
            "mailboxes": {},
            "tokens_spent": 0,
            "child_seq": 0,
            "spawn_depths": {}
        });
        std::fs::write(&store.path, serde_json::to_string(&v1_json).unwrap()).unwrap();

        let cp = store.load().unwrap().expect("v1 checkpoint must load");
        assert_eq!(cp.format_version, 1);
        assert_eq!(cp.agents.len(), 1);
        assert!(
            cp.agents[0].short_term.is_empty(),
            "v1 checkpoint must deserialize to empty short_term"
        );
        let _: Vec<MemItem> = cp.agents[0].short_term.clone(); // type check
    }

    // AC13: v2 checkpoint with non-empty short_term round-trips through save()/load()
    #[tokio::test]
    async fn v2_checkpoint_with_short_term_roundtrips() {
        use crate::inference::Role;
        use crate::memory::MemItem;

        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());

        let item = MemItem {
            turn:            3,
            role:            Role::Assistant,
            content_preview: "partial answer".to_string(),
            blocks_json:     r#"[{"type":"text","text":"partial answer"}]"#.to_string(),
        };

        let mut cp = minimal_scheduler_checkpoint();
        cp.agents[0].short_term = vec![item.clone()];

        store.save(&cp).await.unwrap();
        let loaded = store.load().unwrap().unwrap();

        assert_eq!(loaded.agents[0].short_term.len(), 1);
        let loaded_item = &loaded.agents[0].short_term[0];
        assert_eq!(loaded_item.turn, 3);
        assert_eq!(loaded_item.role, Role::Assistant);
        assert_eq!(loaded_item.content_preview, "partial answer");
        assert_eq!(loaded_item.blocks_json, item.blocks_json);
    }

    #[test]
    fn tmp_path_has_tmp_extension_and_contains_pid() {
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        let p = store.tmp_path();
        let name = p.file_name().unwrap().to_string_lossy();
        assert!(name.ends_with(".tmp"), "tmp_path must end in .tmp: {name}");
        let pid = std::process::id().to_string();
        assert!(name.contains(&pid), "tmp_path must embed the process id: {name}");
    }

    /// write failure (via read-only directory) propagates as Err.
    /// Note: save() calls write_mode_600 first, which fails before rename is reached;
    /// the rename error path is not separately exercised.
    #[cfg(unix)]
    #[tokio::test]
    async fn save_write_failure_ro_dir_returns_err() {
        use std::os::unix::fs::PermissionsExt;
        // Write the checkpoint into a writable subdir, then make the PARENT read-only
        // after the tmp write but before the rename.  We simulate this by writing
        // the tmp file ourselves and then attempting a store that tries to rename.
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        // Write a valid tmp file directly so write_mode_600 succeeds.
        let cp = minimal_scheduler_checkpoint();
        let json = serde_json::to_string(&cp).unwrap();
        std::fs::write(store.tmp_path(), &json).unwrap();
        // Now lock the directory — rename requires write permission on the parent dir.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        // save() will try write_mode_600 (fails on read-only dir), so the error
        // will be the write error, not the rename error.  Either way save() must Err.
        let result = store.save(&cp).await;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err(), "save must return Err when dir is read-only");
    }
}
