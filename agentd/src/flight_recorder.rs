use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::Mutex,
};

use anyhow::Context;
use serde::Serialize;

const FLIGHT_LOG: &str = "flight.jsonl";

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
// Variants beyond AgentSpawned are used from p0.4 onward; defined here to
// establish the canonical taxonomy from CONVENTIONS.md from day one.
#[allow(dead_code)]
pub enum EventKind {
    AgentSpawned,
    Perceive,
    InferenceRequest,
    InferenceResponse,
    ToolCall,
    ToolResult,
    Observe,
    AgentCompleted,
    BudgetExceeded,
    MaxTurnsReached,
    Error,
}

pub struct FlightRecorder {
    file: Mutex<File>,
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
        })
    }

    pub fn open() -> anyhow::Result<Self> {
        Self::new(Path::new(FLIGHT_LOG))
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
    #[ignore = "requires chmod 000 on path — not automatable in CI without root"]
    fn unwritable_path_returns_error() {
        // Manual test: create a file, chmod 000, verify FlightRecorder::new returns Err.
        // Do not run in CI; test manually with: chmod 000 /tmp/unwritable && cargo test -- --ignored
    }
}
