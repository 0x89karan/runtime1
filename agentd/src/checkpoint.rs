use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    bus::MailMessage,
    config::{AgentConfig, ModelConfig},
    inference::{InferenceResponse, Msg, ToolSpec},
};

pub const FORMAT_VERSION: u32 = 1;

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
    f.write_all(data).await
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
}

/// A serializable entry in the awaiting map (child → parent relationship).
#[derive(Debug, Serialize, Deserialize)]
pub struct AwaitingEntry {
    pub child_id:  String,
    pub parent_id: String,
    pub call_id:   String,
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
}

/// Handles checkpoint I/O. Writes are atomic: tmp → rename.
pub struct CheckpointStore {
    path:     PathBuf,
    tmp_path: PathBuf,
}

impl CheckpointStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            path:     dir.join("checkpoint.json"),
            tmp_path: dir.join("checkpoint.json.tmp"),
        }
    }

    /// Write `cp` atomically. Calls `tokio::fs::write` so the scheduler's async
    /// executor is not blocked; a crash after rename leaves the previous good
    /// checkpoint intact because the tmp file was written first.
    ///
    /// On Unix the tmp file is created with mode 0600 (owner read/write only).
    /// `rename(2)` preserves those permissions on the final `checkpoint.json`.
    pub async fn save(&self, cp: &SchedulerCheckpoint) -> Result<()> {
        let json = serde_json::to_string(cp).context("serialize checkpoint")?;
        write_mode_600(&self.tmp_path, json.as_bytes())
            .await
            .context("write checkpoint.json.tmp")?;
        tokio::fs::rename(&self.tmp_path, &self.path)
            .await
            .context("rename checkpoint.json.tmp -> checkpoint.json")?;
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
        let cp: SchedulerCheckpoint =
            serde_json::from_slice(&bytes).context("parse checkpoint.json")?;
        if cp.format_version > FORMAT_VERSION {
            anyhow::bail!(
                "checkpoint format_version {} > supported {}; refusing to load stale format",
                cp.format_version,
                FORMAT_VERSION
            );
        }
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
        }
    }

    fn minimal_scheduler_checkpoint() -> SchedulerCheckpoint {
        SchedulerCheckpoint {
            format_version: FORMAT_VERSION,
            agents:         vec![minimal_agent_checkpoint("agent-a")],
            awaiting:       vec![],
            mailboxes:      HashMap::new(),
            tokens_spent:   15,
            child_seq:      0,
            spawn_depths:   [("agent-a".to_string(), 0)].into_iter().collect(),
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
        assert!(store.load().is_err(), "newer format_version must return Err");
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
        assert!(!store.tmp_path.exists(), "checkpoint.json.tmp must not persist after rename");
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
            msg.contains("write checkpoint.json.tmp"),
            "error context must mention the tmp path: {msg}"
        );
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
        std::fs::write(&store.tmp_path, &json).unwrap();
        // Now lock the directory — rename requires write permission on the parent dir.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        // save() will try write_mode_600 (fails on read-only dir), so the error
        // will be the write error, not the rename error.  Either way save() must Err.
        let result = store.save(&cp).await;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err(), "save must return Err when dir is read-only");
    }
}
