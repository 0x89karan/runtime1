use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::Mutex,
};

use anyhow::Context;
use serde::Serialize;
use tokio::sync::broadcast;

pub use crate::events::EventKind;

const FLIGHT_LOG: &str = "flight.jsonl";

pub struct FlightRecorder {
    file: Mutex<File>,
    /// Optional SSE fan-out channel; each subscriber gets a clone of every JSON line.
    broadcast_tx: Option<broadcast::Sender<String>>,
}

impl FlightRecorder {
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening flight log at {path:?}"))?;
        Ok(Self {
            file: Mutex::new(file),
            broadcast_tx: None,
        })
    }

    pub fn open() -> anyhow::Result<Self> {
        Self::new(Path::new(FLIGHT_LOG))
    }

    /// Attach an SSE broadcast sender. Every recorded event line is also sent
    /// to this channel. Call before wiring up the management server.
    pub fn with_broadcast(mut self, tx: broadcast::Sender<String>) -> Self {
        self.broadcast_tx = Some(tx);
        self
    }

    pub fn record(
        &self,
        agent: &str,
        turn: Option<u32>,
        kind: EventKind,
        data: serde_json::Value,
    ) {
        #[derive(Serialize)]
        struct Event<'a> {
            ts: String,
            agent: &'a str,
            turn: Option<u32>,
            kind: EventKind,
            data: serde_json::Value,
        }

        let event = Event {
            ts: chrono::Utc::now().to_rfc3339(),
            agent,
            turn,
            kind,
            data,
        };

        let line = match serde_json::to_string(&event) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("flight record serialization failed: {e}");
                return;
            }
        };

        let Some(mut file) = self.file.lock().ok() else {
            tracing::warn!("flight recorder mutex poisoned; dropping event");
            return;
        };

        if let Err(e) = writeln!(file, "{line}") {
            tracing::warn!("flight record write failed: {e}");
        }

        // Best-effort broadcast; lagged subscribers are handled at the receiver.
        if let Some(tx) = &self.broadcast_tx {
            let _ = tx.send(line);
        }
    }

    /// Like `record`, but writes a different `data` payload to disk than what's
    /// broadcast over SSE. Used only by `InferenceStreamDelta` (ux.1): the full chunk
    /// text is broadcast live so `agentctl watch`'s chat rail can render it token-by-
    /// token, but the durable `flight.jsonl` copy keeps the existing preview/audit-
    /// metadata contract (bounded field sizes) rather than becoming a full model-output
    /// transcript store — every other event kind still goes through plain `record()`.
    pub fn record_streamed(
        &self,
        agent: &str,
        turn: Option<u32>,
        kind: EventKind,
        disk_data: serde_json::Value,
        broadcast_data: serde_json::Value,
    ) {
        #[derive(Serialize)]
        struct Event<'a> {
            ts: String,
            agent: &'a str,
            turn: Option<u32>,
            kind: EventKind,
            data: serde_json::Value,
        }

        let ts = chrono::Utc::now().to_rfc3339();

        let disk_line = match serde_json::to_string(&Event {
            ts: ts.clone(),
            agent,
            turn,
            kind,
            data: disk_data,
        }) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("flight record serialization failed: {e}");
                return;
            }
        };

        let Some(mut file) = self.file.lock().ok() else {
            tracing::warn!("flight recorder mutex poisoned; dropping event");
            return;
        };
        if let Err(e) = writeln!(file, "{disk_line}") {
            tracing::warn!("flight record write failed: {e}");
        }
        drop(file);

        if let Some(tx) = &self.broadcast_tx {
            let broadcast_line = serde_json::to_string(&Event {
                ts,
                agent,
                turn,
                kind,
                data: broadcast_data,
            });
            if let Ok(line) = broadcast_line {
                let _ = tx.send(line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::NamedTempFile;

    #[test]
    fn record_appends_valid_jsonl() {
        let tmp = NamedTempFile::new().unwrap();
        let recorder = FlightRecorder::new(tmp.path()).unwrap();

        recorder.record(
            "test-agent",
            None,
            EventKind::AgentSpawned,
            serde_json::json!({"model": "claude-sonnet-4-6"}),
        );

        let mut content = String::new();
        File::open(tmp.path())
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(event["kind"], "agent_spawned");
        assert!(event["turn"].is_null());
        assert_eq!(event["agent"], "test-agent");
        assert!(event["ts"].is_string());
        assert_eq!(event["data"]["model"], "claude-sonnet-4-6");
    }

    #[test]
    fn record_with_turn_number() {
        let tmp = NamedTempFile::new().unwrap();
        let recorder = FlightRecorder::new(tmp.path()).unwrap();

        recorder.record(
            "agent-1",
            Some(1),
            EventKind::Perceive,
            serde_json::json!({}),
        );

        let mut content = String::new();
        File::open(tmp.path())
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(event["kind"], "perceive");
        assert_eq!(event["turn"], 1);
    }

    #[test]
    fn fuse_event_kinds_serialize_to_snake_case() {
        let tmp = NamedTempFile::new().unwrap();
        let recorder = FlightRecorder::new(tmp.path()).unwrap();

        recorder.record("agentd", None, EventKind::FuseMounted,   serde_json::json!({"mountpoint": "/agents"}));
        recorder.record("agentd", None, EventKind::FuseUnmounted, serde_json::json!({"mountpoint": "/agents"}));

        let mut content = String::new();
        File::open(tmp.path()).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<serde_json::Value> = content.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["kind"], "fuse_mounted");
        assert_eq!(lines[1]["kind"], "fuse_unmounted");
    }

    #[test]
    fn checkpoint_event_kinds_serialize_to_snake_case() {
        let tmp = NamedTempFile::new().unwrap();
        let recorder = FlightRecorder::new(tmp.path()).unwrap();

        recorder.record("agent-1", Some(2), EventKind::AgentCheckpointed, serde_json::json!({}));
        recorder.record("agent-1", Some(2), EventKind::AgentRestored,     serde_json::json!({ "turn": 2 }));

        let mut content = String::new();
        File::open(tmp.path()).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<serde_json::Value> = content.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["kind"], "agent_checkpointed");
        assert_eq!(lines[1]["kind"], "agent_restored");
        assert_eq!(lines[1]["data"]["turn"], 2);
    }

    #[test]
    fn sandbox_event_kinds_serialize_to_snake_case() {
        let tmp = NamedTempFile::new().unwrap();
        let recorder = FlightRecorder::new(tmp.path()).unwrap();

        recorder.record("agentd", None, EventKind::SandboxApplied,
            serde_json::json!({"server": "echo", "rules": ["DenySpawn"]}));
        recorder.record("agentd", None, EventKind::SandboxSkipped,
            serde_json::json!({"server": "echo"}));

        let mut content = String::new();
        File::open(tmp.path()).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<serde_json::Value> = content.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["kind"], "sandbox_applied");
        assert_eq!(lines[1]["kind"], "sandbox_skipped");
    }

    #[test]
    fn broadcast_receives_recorded_events() {
        let tmp = NamedTempFile::new().unwrap();
        let (tx, mut rx) = broadcast::channel(16);
        let recorder = FlightRecorder::new(tmp.path()).unwrap().with_broadcast(tx);

        recorder.record("a", None, EventKind::AgentSpawned, serde_json::json!({}));

        let line = rx.try_recv().expect("broadcast should have one message");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["kind"], "agent_spawned");
    }

    #[test]
    #[ignore = "requires chmod 000 on path — not automatable in CI without root"]
    fn unwritable_path_returns_error() {
        // Manual test: create a file, chmod 000, verify FlightRecorder::new returns Err.
        // Do not run in CI; test manually with: chmod 000 /tmp/unwritable && cargo test -- --ignored
    }

    #[test]
    fn record_streamed_broadcasts_full_text_but_truncates_disk_copy() {
        let tmp = NamedTempFile::new().unwrap();
        let (tx, mut rx) = broadcast::channel(16);
        let recorder = FlightRecorder::new(tmp.path()).unwrap().with_broadcast(tx);

        recorder.record_streamed(
            "agent-1",
            Some(3),
            EventKind::InferenceStreamDelta,
            serde_json::json!({"agent_id": "agent-1", "turn_seq": 3, "chunk_seq": 0, "text": "sho"}),
            serde_json::json!({"agent_id": "agent-1", "turn_seq": 3, "chunk_seq": 0, "text": "short-but-full-text"}),
        );

        // Broadcast subscriber sees the FULL text.
        let line = rx.try_recv().expect("broadcast should have one message");
        let broadcast_event: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(broadcast_event["kind"], "inference_stream_delta");
        assert_eq!(broadcast_event["data"]["text"], "short-but-full-text");

        // Disk copy has the TRUNCATED text.
        let mut content = String::new();
        File::open(tmp.path()).unwrap().read_to_string(&mut content).unwrap();
        let disk_event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(disk_event["data"]["text"], "sho");
        assert_eq!(disk_event["turn"], 3);
    }

    #[test]
    fn record_streamed_without_broadcast_only_writes_disk() {
        let tmp = NamedTempFile::new().unwrap();
        let recorder = FlightRecorder::new(tmp.path()).unwrap(); // no with_broadcast

        recorder.record_streamed(
            "agent-1",
            None,
            EventKind::InferenceStreamDelta,
            serde_json::json!({"text": "disk"}),
            serde_json::json!({"text": "full"}),
        );

        let mut content = String::new();
        File::open(tmp.path()).unwrap().read_to_string(&mut content).unwrap();
        let disk_event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(disk_event["data"]["text"], "disk");
    }
}
