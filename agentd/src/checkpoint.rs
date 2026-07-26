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

// Stays 4 for ux.8′ (Codex P2): the ux.8′ budget fields (window_anchor,
// budget_window_start, global_window_anchor) are purely additive #[serde(default)]
// and no checkpoint struct uses deny_unknown_fields, so both directions are safe —
// a new binary fills missing fields via defaults; an old binary ignores the extra
// ones. Bumping to 5 would make the old (v0.88) loader refuse a new checkpoint
// (`format_version > FORMAT_VERSION`), rename it as corrupt, and DISCARD CoS state
// on rollback — the exact data loss this increment fights. Only bump for a
// genuinely breaking (non-additive) schema change.
pub const FORMAT_VERSION: u32 = 4;

/// Serializable per-provider health state for checkpoint persistence.
///
/// `TransientRetry` is NOT checkpointed — it is transient by definition and
/// resets on restart. `ProviderHealthState::Healthy` maps to `None` in the map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealthCheckpoint {
    /// "reauth" | "config_fix" | "secret_replace"
    pub recovery_kind: String,
    pub reason:        String,
    /// Unix secs when the provider first entered AttentionRequired.
    pub since:         u64,
}

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
    f.flush().await?;
    // Durability guarantee (audit-C3): ensure kernel flushes file data before rename.
    f.sync_all().await
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
    /// False for normal agents (only non-terminal agents are checkpointed).
    /// True for orchestrated waiting agents (parked between REPL turns) — they are
    /// checkpointed as terminal so the scheduler does not step them on restore;
    /// they re-enter via `resume_for_orchestration()` when the next inject arrives.
    pub terminal:        bool,
    /// Tier-2 eviction buffer: turns paged out of active context.
    /// `#[serde(default)]` makes v1 checkpoints (no field) load as empty vec.
    #[serde(default)]
    pub short_term:      Vec<MemItem>,
    /// Budget-window anchor (ux.8′): lifetime spend (`total_input+total_output`)
    /// at the start of the current budget window. Windowed spend = lifetime −
    /// anchor. `#[serde(default)]` = 0 → pre-ux.8′ checkpoints load unanchored
    /// and get a clean first window on restore (see scheduler init).
    #[serde(default)]
    pub window_anchor:   u64,
}

/// A serializable entry in the awaiting map (child → parent relationship).
#[derive(Debug, Serialize, Deserialize)]
pub struct AwaitingEntry {
    pub child_id:  String,
    pub parent_id: String,
    pub call_id:   String,
    /// Whether the child's answer is delivered to the parent on completion (cap.2b).
    /// Default `true` so pre-cap.2b checkpoints (spawn_agent awaits only) restore correctly.
    #[serde(default = "crate::checkpoint::default_true")]
    pub deliver_content: bool,
}

/// Serde default for `AwaitingEntry.deliver_content` — see the field doc.
pub fn default_true() -> bool {
    true
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
    /// Agent IDs that were parked waiting for an orchestrator inject at checkpoint time.
    /// On restore these IDs are re-inserted into `state.waiting`. Absent in v1–v3 → empty.
    #[serde(default)]
    pub waiting_agents: Vec<String>,
    /// Agent IDs that were spawned as orchestrated (persistent across turns).
    /// On restore these IDs are re-inserted into `state.orchestrated`. Absent in v1–v3 → empty.
    #[serde(default)]
    pub orchestrated_agents: Vec<String>,
    /// Per-provider credential health state (cred.7).
    /// Only AttentionRequired entries are stored; absent entries are Healthy.
    /// Absent in v1–v4 → empty.
    #[serde(default)]
    pub credential_health: HashMap<String, ProviderHealthCheckpoint>,
    /// Global budget-window start (ux.8′), wall-clock Unix seconds. 0 = unset →
    /// the scheduler starts a fresh window at `now` on restore (clean migration).
    #[serde(default)]
    pub budget_window_start: u64,
    /// Global lifetime `tokens_spent` at the current window's start (ux.8′).
    /// Windowed global spend = `tokens_spent − global_window_anchor`.
    #[serde(default)]
    pub global_window_anchor: u64,
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
        // Durability guarantee (audit-C3): fsync the parent directory so the directory
        // entry for the renamed file is flushed before we return.
        if let Some(parent) = self.path.parent() {
            if let Ok(dir) = tokio::fs::File::open(parent).await {
                let _ = dir.sync_all().await; // best-effort; never crash a checkpoint
            }
        }
        // AUDIT-v0.97 P2-1 (review follow-up): a successful save supersedes the pre-restore
        // copy — consume it now, regardless of whether this boot loaded from the primary or
        // fell back to `.restored`. This closes the edge where a boot that recovered from
        // `.restored` left it in place to be re-loaded on a later boot that never saved.
        let _ = std::fs::remove_file(self.restored_path());
        Ok(())
    }

    /// Read and parse the checkpoint file.
    /// Returns `Ok(None)` when the file is absent.
    /// Returns `Err` when the file is present but cannot be parsed (caller should
    /// rename to .corrupt and start fresh — never fail-stop a restart).
    /// Pre-restore copy (AUDIT-v0.97 P2-1). On restore we rename `checkpoint.json` here
    /// instead of deleting it, so a crash AFTER restore but BEFORE the first new save is
    /// recoverable. `load()` falls back to it; the next boot's `mark_restored` rename
    /// overwrites it, so it self-cleans (no accumulation, no scheduler hook needed).
    fn restored_path(&self) -> PathBuf {
        self.path.with_file_name("checkpoint.json.restored")
    }

    /// Rename the just-loaded checkpoint to the pre-restore copy. Call after a successful
    /// `load()` instead of deleting — preserves recoverable state across a crash before
    /// the first post-restore save.
    pub fn mark_restored(&self) -> std::io::Result<()> {
        match std::fs::rename(&self.path, self.restored_path()) {
            Ok(()) => Ok(()),
            // Benign on a repeat restart that recovered from `.restored` (AUDIT-v0.97 holistic
            // review NIT): the primary was already renamed on a prior boot and no save has
            // happened since, so the recoverable copy is already in place. Don't surface a
            // scary "could not rename" warning for the more-resilient path — only real errors.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && self.restored_path().exists() => {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Best-effort sweep of crash-orphaned checkpoint tmp files (audit86-P3-5). `save()` writes to
    /// `checkpoint.json.<pid>.<nanos>.tmp` then renames; a crash between write and rename leaks the
    /// tmp forever. Removes stale siblings matching `<basename>.*.tmp` only — STRICT, so it can never
    /// touch `checkpoint.json`, `.restored`, `.corrupt`, or any unrelated file. Runs at startup (from
    /// `load()`). Never propagates an error.
    fn sweep_stale_tmp(&self) {
        // P3-5 /review (FIX B): only sweep tmp files OLDER than the threshold. A crash-orphan is by
        // definition old; a live tmp that a second agentd sharing the dir is mid-write on (the
        // boot-overlap case `tmp_path` guards against) is seconds-fresh — sweeping it would fail
        // that process's rename. An unreadable mtime is treated as NOT sweepable (conservative).
        self.sweep_stale_tmp_with(std::time::Duration::from_secs(60));
    }

    fn sweep_stale_tmp_with(&self, min_age: std::time::Duration) {
        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let Some(base) = self.path.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        let prefix = format!("{base}.");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !(name.starts_with(&prefix) && name.ends_with(".tmp")) {
                continue;
            }
            // Age gate: sweep only if the file is at least `min_age` old. Unreadable mtime → skip.
            let old_enough = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|mtime| mtime.elapsed().ok())
                .is_some_and(|age| age >= min_age);
            if old_enough {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    pub fn load(&self) -> Result<Option<SchedulerCheckpoint>> {
        // audit86-P3-5: clear crash-orphaned `checkpoint.json.*.tmp` debris at startup.
        self.sweep_stale_tmp();
        // Prefer the primary; fall back to the pre-restore copy left by a crash after
        // restore but before the first new save (AUDIT-v0.97 P2-1).
        let src = if self.path.exists() {
            self.path.clone()
        } else if self.restored_path().exists() {
            self.restored_path()
        } else {
            return Ok(None);
        };
        match Self::read_and_parse(&src) {
            Ok(cp) => Ok(Some(cp)),
            Err(e) => {
                // Source-aware quarantine (AUDIT-v0.97 P2-1 review): rename the ACTUAL bad
                // file — primary OR .restored — to `<name>.corrupt` so the same corrupt/
                // too-new checkpoint is not re-loaded on the next boot (the caller starts
                // fresh). Best-effort; never fail-stop a restart.
                if let Some(name) = src.file_name().and_then(|n| n.to_str()) {
                    let _ = std::fs::rename(&src, src.with_file_name(format!("{name}.corrupt")));
                }
                // AUDIT-v0.97 holistic review (Codex Medium): a CORRUPT primary must not
                // suppress a valid pre-restore copy. If we just quarantined the primary and a
                // `.restored` copy exists (crash after restore, then a partial/garbled primary
                // write or on-disk corruption), fall back to it before giving up — the whole
                // point of the pre-restore copy is to survive exactly this. Quarantine it too
                // if it is also unreadable, so the next boot starts genuinely fresh.
                if src == self.path && self.restored_path().exists() {
                    match Self::read_and_parse(&self.restored_path()) {
                        Ok(cp) => return Ok(Some(cp)),
                        Err(_) => {
                            let restored = self.restored_path();
                            if let Some(name) = restored.file_name().and_then(|n| n.to_str()) {
                                let _ = std::fs::rename(
                                    &restored,
                                    restored.with_file_name(format!("{name}.corrupt")),
                                );
                            }
                        }
                    }
                }
                Err(e)
            }
        }
    }

    fn read_and_parse(src: &Path) -> Result<SchedulerCheckpoint> {
        let bytes = std::fs::read(src).context("read checkpoint")?;
        // Probe the version field before full deserialization so we can distinguish
        // "too new" (intentional refusal) from "corrupt" (parse error).
        let probe: VersionProbe = serde_json::from_slice(&bytes)
            .context("checkpoint: cannot read format_version")?;
        if probe.format_version > FORMAT_VERSION {
            anyhow::bail!(
                "checkpoint format_version {} > supported {}; \
                 this checkpoint was written by a newer agentd — refusing to load",
                probe.format_version,
                FORMAT_VERSION
            );
        }
        serde_json::from_slice(&bytes).context("parse checkpoint")
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
                id:              id.to_string(),
                task:            "test task".to_string(),
                max_turns:       5,
                token_budget:    100_000,
                priority:        0,
                capabilities:    None,
                name:            None,
                description:     String::new(),
                skills:          vec![],
                tier:            crate::config::AgentTier::Native,
                command:         None,
                args:            vec![],
                isolation:       crate::config::IsolationMode::None,
                max_wall_seconds: 0,
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
            window_anchor:   0,
        }
    }

    fn minimal_scheduler_checkpoint() -> SchedulerCheckpoint {
        SchedulerCheckpoint {
            format_version:     FORMAT_VERSION,
            agents:             vec![minimal_agent_checkpoint("agent-a")],
            awaiting:           vec![],
            mailboxes:          HashMap::new(),
            tokens_spent:       15,
            child_seq:          0,
            spawn_depths:       [("agent-a".to_string(), 0)].into_iter().collect(),
            parent_map:         HashMap::new(),
            pending_approvals:  vec![],
            approval_seq:       0,
            waiting_agents:     vec![],
            orchestrated_agents: vec![],
            credential_health:  HashMap::new(),
            budget_window_start: 0,
            global_window_anchor: 0,
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
            blocks:            vec![Block::Text { text: "answer".to_string() }],
            stop_reason:       StopReason::EndTurn,
            input_tokens:      20,
            output_tokens:     10,
            transport_retries: 0,
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
            deliver_content: true,
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
        // Must be identified as "too new" (explicit refusal), not "corrupt".
        assert!(msg.contains("refusing to load"), "error must say 'refusing to load': {msg}");
        // AUDIT-v0.97 P2-1 (review): load() quarantines the source so it is NOT re-loaded
        // next boot — the primary is renamed away and a subsequent load starts fresh.
        assert!(!store.path.exists(), "too-new checkpoint quarantined away from the primary path");
        assert!(store.load().unwrap().is_none(), "after quarantine, next load starts fresh");
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

    #[tokio::test]
    async fn mark_restored_then_load_recovers_from_restored_copy() {
        // AUDIT-v0.97 P2-1: after restore we rename (not delete) the checkpoint. A crash
        // before the first new save must be recoverable — load() falls back to .restored.
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        store.save(&minimal_scheduler_checkpoint()).await.unwrap();
        // Simulate a restore: rename checkpoint.json -> checkpoint.json.restored.
        store.mark_restored().unwrap();
        assert!(!dir.path().join("checkpoint.json").exists(), "primary renamed away");
        assert!(dir.path().join("checkpoint.json.restored").exists(), ".restored copy kept");
        // A crash before the first new save: load() must still recover the state.
        let loaded = store.load().unwrap().expect("load recovers from .restored");
        assert_eq!(loaded.agents[0].agent_id, "agent-a");
        assert_eq!(loaded.tokens_spent, 15);
    }

    #[tokio::test]
    async fn load_prefers_primary_over_restored() {
        // The next boot's fresh save recreates checkpoint.json; load() must prefer it over a
        // stale .restored (which the next mark_restored rename then overwrites — self-cleaning).
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        store.save(&minimal_scheduler_checkpoint()).await.unwrap(); // valid primary
        // A stale/garbage .restored must be ignored while the primary is present.
        std::fs::write(dir.path().join("checkpoint.json.restored"), b"{\"format_version\":999}").unwrap();
        let loaded = store.load().unwrap().expect("primary present -> loads, ignoring .restored");
        assert_eq!(loaded.agents[0].agent_id, "agent-a");
    }

    #[tokio::test]
    async fn save_consumes_restored_copy() {
        // AUDIT-v0.97 P2-1 (review): a successful save supersedes the pre-restore copy, so a
        // boot that recovered from .restored cannot re-load it on a later boot.
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        store.save(&minimal_scheduler_checkpoint()).await.unwrap();
        store.mark_restored().unwrap();
        assert!(dir.path().join("checkpoint.json.restored").exists(), ".restored present pre-save");
        store.save(&minimal_scheduler_checkpoint()).await.unwrap();
        assert!(!dir.path().join("checkpoint.json.restored").exists(), "save consumes the .restored copy");
        assert!(dir.path().join("checkpoint.json").exists(), "fresh primary written");
    }

    #[test]
    fn load_quarantines_corrupt_restored_source() {
        // AUDIT-v0.97 P2-1 (review): a corrupt FALLBACK is quarantined (not left to re-load
        // every boot). Primary absent, .restored garbage -> Err + renamed to .restored.corrupt.
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        std::fs::write(dir.path().join("checkpoint.json.restored"), b"not json").unwrap();
        assert!(store.load().is_err(), "corrupt .restored -> Err");
        assert!(!dir.path().join("checkpoint.json.restored").exists(), "corrupt .restored quarantined away");
        assert!(dir.path().join("checkpoint.json.restored.corrupt").exists(), "renamed to .restored.corrupt");
    }

    #[test]
    fn sweep_removes_only_orphaned_tmp_files() {
        // audit86-P3-5 + /review FIX B: an OLD crash-orphaned `checkpoint.json.*.tmp` is swept, a
        // FRESH (concurrent-live) tmp survives the age gate, and nothing else is ever touched.
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        let orphan1 = dir.path().join("checkpoint.json.99999.123.tmp");
        let orphan2 = dir.path().join("checkpoint.json.1.2.tmp");
        std::fs::write(&orphan1, b"orphan").unwrap();
        std::fs::write(&orphan2, b"orphan2").unwrap();
        // Non-matching siblings — must survive at ANY threshold (strict name predicate).
        std::fs::write(dir.path().join("checkpoint.json"), b"primary").unwrap();
        std::fs::write(dir.path().join("checkpoint.json.restored"), b"restored").unwrap();
        std::fs::write(dir.path().join("checkpoint.json.corrupt"), b"corrupt").unwrap();
        std::fs::write(dir.path().join("other.txt"), b"unrelated").unwrap();

        // Age gate: with the real 1h threshold the just-created tmps are fresh (like a concurrent
        // agentd's live in-flight tmp) and must SURVIVE — this is the FIX B protection.
        store.sweep_stale_tmp_with(std::time::Duration::from_secs(3600));
        assert!(orphan1.exists(), "fresh tmp survives the age gate (concurrent-live protection)");
        assert!(orphan2.exists(), "fresh tmp survives the age gate");

        // Name predicate: threshold 0 → the orphan tmps are swept, non-`.tmp` siblings are NOT.
        store.sweep_stale_tmp_with(std::time::Duration::ZERO);
        assert!(!orphan1.exists(), "orphaned tmp swept");
        assert!(!orphan2.exists(), "second orphaned tmp swept");
        assert!(dir.path().join("checkpoint.json").exists(), "primary NOT swept");
        assert!(dir.path().join("checkpoint.json.restored").exists(), ".restored NOT swept");
        assert!(dir.path().join("checkpoint.json.corrupt").exists(), ".corrupt NOT swept");
        assert!(dir.path().join("other.txt").exists(), "unrelated file NOT swept");
    }

    #[test]
    fn load_falls_back_to_restored_when_primary_corrupt() {
        // AUDIT-v0.97 holistic review (Codex Medium): a CORRUPT primary must not suppress a
        // valid pre-restore copy. Crash after restore leaves a good `.restored`; a later partial
        // primary write / on-disk corruption then leaves `checkpoint.json` present-but-garbled.
        // load() must quarantine the bad primary AND recover from `.restored`, not start fresh.
        let dir = TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        let good = serde_json::to_vec(&minimal_scheduler_checkpoint()).unwrap();
        std::fs::write(dir.path().join("checkpoint.json.restored"), &good).unwrap();
        std::fs::write(dir.path().join("checkpoint.json"), b"garbled primary").unwrap();

        let loaded = store.load().expect("primary corrupt but .restored valid -> recover, not Err");
        let cp = loaded.expect("recovered checkpoint is Some");
        assert_eq!(cp.tokens_spent, 15, "recovered the pre-restore copy's state");
        assert!(!dir.path().join("checkpoint.json").exists(), "corrupt primary quarantined away");
        assert!(dir.path().join("checkpoint.json.corrupt").exists(), "bad primary renamed to .corrupt");
        assert!(dir.path().join("checkpoint.json.restored").exists(), ".restored preserved for the next boot");
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

    // ── orchestration checkpoint fields (orch.2) ──────────────────────────────

    #[test]
    fn waiting_orchestrated_fields_roundtrip() {
        let mut cp = minimal_scheduler_checkpoint();
        cp.waiting_agents = vec!["orch-1".to_string()];
        cp.orchestrated_agents = vec!["orch-1".to_string()];
        cp.agents.push({
            let mut a = minimal_agent_checkpoint("orch-1");
            a.terminal = true; // parked waiting agent
            a
        });
        let json = serde_json::to_string(&cp).unwrap();
        let back: SchedulerCheckpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(back.waiting_agents, vec!["orch-1"]);
        assert_eq!(back.orchestrated_agents, vec!["orch-1"]);
        assert!(back.agents[1].terminal, "parked agent must round-trip as terminal=true");
    }

    #[test]
    fn v3_checkpoint_loads_without_orchestration_fields() {
        // Pre-orch.2 checkpoints (format_version=3) have no waiting_agents or
        // orchestrated_agents. They must deserialize to empty vecs via #[serde(default)].
        let json = serde_json::json!({
            "format_version": 3,
            "agents": [],
            "awaiting": [],
            "mailboxes": {},
            "tokens_spent": 0,
            "child_seq": 0,
            "spawn_depths": {},
            "parent_map": {},
            "pending_approvals": [],
            "approval_seq": 0
            // "waiting_agents" and "orchestrated_agents" are intentionally absent
        });
        let cp: SchedulerCheckpoint =
            serde_json::from_str(&serde_json::to_string(&json).unwrap()).unwrap();
        assert!(cp.waiting_agents.is_empty(), "missing waiting_agents must be empty vec");
        assert!(cp.orchestrated_agents.is_empty(), "missing orchestrated_agents must be empty vec");
    }

    #[test]
    fn pre_ux8_checkpoint_defaults_budget_fields_to_zero() {
        // Codex P2 / T6: FORMAT_VERSION stays 4 for ux.8′ (additive serde-default
        // change). A checkpoint written WITHOUT the ux.8′ budget fields
        // (window_anchor / budget_window_start / global_window_anchor) must load
        // with them defaulting to 0 — the migration then opens a clean window.
        // This is the forward-compat proof; keeping v4 also keeps ROLLBACK safe
        // (an old binary won't refuse a new checkpoint as "too new").
        let mut agent = serde_json::to_value(minimal_agent_checkpoint("a")).unwrap();
        agent.as_object_mut().unwrap().remove("window_anchor");
        let mut sched = serde_json::to_value(minimal_scheduler_checkpoint()).unwrap();
        {
            let o = sched.as_object_mut().unwrap();
            o.remove("budget_window_start");
            o.remove("global_window_anchor");
            o["agents"] = serde_json::json!([agent]);
        }
        let cp: SchedulerCheckpoint = serde_json::from_value(sched).unwrap();
        assert_eq!(cp.budget_window_start, 0, "absent budget_window_start defaults to 0");
        assert_eq!(cp.global_window_anchor, 0, "absent global_window_anchor defaults to 0");
        assert_eq!(cp.agents[0].window_anchor, 0, "absent per-agent window_anchor defaults to 0");
    }

    #[test]
    fn from_checkpoint_terminal_true_round_trips() {
        // An AgentCheckpoint with terminal=true must deserialize back with terminal=true.
        let mut cp = minimal_agent_checkpoint("orch-parked");
        cp.terminal = true;
        let json = serde_json::to_string(&cp).unwrap();
        let back: AgentCheckpoint = serde_json::from_str(&json).unwrap();
        assert!(back.terminal, "terminal=true must survive checkpoint round-trip");
    }

    #[tokio::test]
    async fn checkpoint_save_includes_fsync() {
        // Verify that save() completes successfully when sync_all() is called —
        // the filesystem-level durability guarantee (audit-C3). We can't directly
        // observe whether sync_all() flushed to hardware, but we can verify the
        // save/load cycle works and the file is readable after save().
        let dir = tempfile::TempDir::new().unwrap();
        let store = CheckpointStore::new(dir.path());
        let mut cp = minimal_scheduler_checkpoint();
        cp.waiting_agents = vec!["w1".to_string()];
        cp.orchestrated_agents = vec!["w1".to_string()];
        store.save(&cp).await.expect("save with sync_all must succeed");
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.waiting_agents, vec!["w1"]);
        assert_eq!(loaded.orchestrated_agents, vec!["w1"]);
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
