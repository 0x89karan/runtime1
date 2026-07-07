use crate::capability::Capability;

/// An agent spawn request sent via the /agents/control FUSE surface.
/// All fields except `task` are optional; missing values fall back to AgentConfig defaults.
#[derive(Debug, serde::Deserialize)]
pub struct OperatorSpawnRequest {
    pub task:         String,
    pub id:           Option<String>,
    pub max_turns:    Option<u32>,
    pub token_budget: Option<u64>,
    pub priority:     Option<u32>,
    pub capabilities: Option<Vec<Capability>>,
    /// When true, agent parks after each response awaiting the next inject (orchestration mode).
    #[serde(default)]
    pub orchestrated: bool,
}

/// Commands dispatched through the /agents/control write surface.
///
/// Wire format (tagged JSON):
///   `{"spawn":{...}}` — spawn a new agent (also accepts bare `{"task":...}` for back-compat)
///   `{"approve":{"id":"act_1"}}` — approve a pending action; optional `edits` replaces args
///   `{"reject":{"id":"act_1","reason":"..."}}` — reject a pending action with optional reason
///   `{"approve":{"id":"act_1","auto_approve_kind":"write_file"}}` — approve + policy hint
///   `{"inject":{"agent_id":"...","text":"..."}}` — inject a new user turn into a waiting agent
#[derive(Debug)]
pub enum ControlCommand {
    Spawn(OperatorSpawnRequest),
    Approve {
        id:                String,
        edits:             Option<serde_json::Value>,
        /// When Some, the scheduler emits an `auto_approve_kind` field in the
        /// ApprovalGranted event so the harness policy agent can learn from it.
        auto_approve_kind: Option<String>,
    },
    Reject {
        id:     String,
        reason: Option<String>,
    },
    /// Inject a new user turn into a waiting orchestrated agent.
    Inject {
        agent_id: String,
        text:     String,
    },
}

/// Internal serde target for the tagged format. The public `ControlCommand` does
/// not derive Deserialize directly so the back-compat bare-spawn path can be kept
/// in `parse_control_command` without exposing it through serde.
#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum TaggedCommand {
    Spawn(OperatorSpawnRequest),
    Approve {
        id:                String,
        #[serde(default)]
        edits:             Option<serde_json::Value>,
        #[serde(default)]
        auto_approve_kind: Option<String>,
    },
    Reject {
        id:     String,
        #[serde(default)]
        reason: Option<String>,
    },
    Inject {
        agent_id: String,
        text:     String,
    },
}

/// Parse raw bytes from a FUSE write into a ControlCommand.
///
/// Tries the tagged format first (`{"spawn":{...}}`, `{"approve":{...}}`, `{"reject":{...}}`).
/// Falls back to bare `{"task":...}` (back-compat for existing operator scripts and agentctl).
pub fn parse_control_command(bytes: &[u8]) -> anyhow::Result<ControlCommand> {
    let val: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| anyhow::anyhow!("invalid control JSON: {e}"))?;

    // Try tagged format first.
    if let Ok(tagged) = serde_json::from_value::<TaggedCommand>(val.clone()) {
        return Ok(match tagged {
            TaggedCommand::Spawn(req) => {
                anyhow::ensure!(!req.task.is_empty(), "control command: task must not be empty");
                ControlCommand::Spawn(req)
            }
            TaggedCommand::Approve { id, edits, auto_approve_kind } => {
                anyhow::ensure!(!id.is_empty(), "approve: id must not be empty");
                ControlCommand::Approve { id, edits, auto_approve_kind }
            }
            TaggedCommand::Reject { id, reason } => {
                anyhow::ensure!(!id.is_empty(), "reject: id must not be empty");
                ControlCommand::Reject { id, reason }
            }
            TaggedCommand::Inject { agent_id, text } => {
                anyhow::ensure!(!agent_id.is_empty(), "inject: agent_id must not be empty");
                anyhow::ensure!(!text.is_empty(), "inject: text must not be empty");
                anyhow::ensure!(text.len() <= 65_536, "inject text too large (max 64 KiB)");
                ControlCommand::Inject { agent_id, text }
            }
        });
    }

    // Fall back to bare spawn (back-compat).
    let req: OperatorSpawnRequest = serde_json::from_value(val)
        .map_err(|e| anyhow::anyhow!("invalid control JSON: {e}"))?;
    anyhow::ensure!(!req.task.is_empty(), "control command: task must not be empty");
    Ok(ControlCommand::Spawn(req))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_spawn_bare() {
        let bytes = br#"{"task":"scan repo for TODOs"}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Spawn(req) = cmd else { panic!("expected Spawn") };
        assert_eq!(req.task, "scan repo for TODOs");
        assert!(req.id.is_none());
        assert!(req.capabilities.is_none());
    }

    #[test]
    fn parse_full_spawn_bare() {
        let bytes = br#"{"task":"search","id":"op-scout","max_turns":5,"token_budget":50000,"priority":1}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Spawn(req) = cmd else { panic!("expected Spawn") };
        assert_eq!(req.task, "search");
        assert_eq!(req.id.as_deref(), Some("op-scout"));
        assert_eq!(req.max_turns, Some(5));
        assert_eq!(req.token_budget, Some(50_000));
        assert_eq!(req.priority, Some(1));
    }

    #[test]
    fn parse_tagged_spawn() {
        let bytes = br#"{"spawn":{"task":"tagged task"}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Spawn(req) = cmd else { panic!("expected Spawn") };
        assert_eq!(req.task, "tagged task");
    }

    #[test]
    fn parse_approve_minimal() {
        let bytes = br#"{"approve":{"id":"act_1"}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Approve { id, edits, auto_approve_kind } = cmd else {
            panic!("expected Approve")
        };
        assert_eq!(id, "act_1");
        assert!(edits.is_none());
        assert!(auto_approve_kind.is_none());
    }

    #[test]
    fn parse_approve_with_edits() {
        let bytes = br#"{"approve":{"id":"act_2","edits":{"path":"/safe/path.txt"}}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Approve { id, edits, .. } = cmd else { panic!("expected Approve") };
        assert_eq!(id, "act_2");
        assert_eq!(edits.unwrap()["path"].as_str(), Some("/safe/path.txt"));
    }

    #[test]
    fn parse_approve_with_auto_approve_kind() {
        let bytes = br#"{"approve":{"id":"act_3","auto_approve_kind":"write_file"}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Approve { id, auto_approve_kind, .. } = cmd else {
            panic!("expected Approve")
        };
        assert_eq!(id, "act_3");
        assert_eq!(auto_approve_kind.as_deref(), Some("write_file"));
    }

    #[test]
    fn parse_reject_minimal() {
        let bytes = br#"{"reject":{"id":"act_1"}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Reject { id, reason } = cmd else { panic!("expected Reject") };
        assert_eq!(id, "act_1");
        assert!(reason.is_none());
    }

    #[test]
    fn parse_reject_with_reason() {
        let bytes = br#"{"reject":{"id":"act_1","reason":"path looks unsafe"}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Reject { id, reason } = cmd else { panic!("expected Reject") };
        assert_eq!(id, "act_1");
        assert_eq!(reason.as_deref(), Some("path looks unsafe"));
    }

    #[test]
    fn parse_empty_task_is_error() {
        let bytes = br#"{"task":""}"#;
        assert!(parse_control_command(bytes).is_err());
    }

    #[test]
    fn parse_missing_task_is_error() {
        let bytes = br#"{"id":"x"}"#;
        assert!(parse_control_command(bytes).is_err());
    }

    #[test]
    fn parse_bad_json_is_error() {
        assert!(parse_control_command(b"not json").is_err());
    }

    #[test]
    fn parse_approve_empty_id_is_error() {
        let bytes = br#"{"approve":{"id":""}}"#;
        assert!(parse_control_command(bytes).is_err());
    }

    #[test]
    fn parse_reject_empty_id_is_error() {
        let bytes = br#"{"reject":{"id":""}}"#;
        assert!(parse_control_command(bytes).is_err());
    }

    #[test]
    fn parse_inject_tagged() {
        let bytes = br#"{"inject":{"agent_id":"scout-1","text":"continue the analysis"}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Inject { agent_id, text } = cmd else { panic!("expected Inject") };
        assert_eq!(agent_id, "scout-1");
        assert_eq!(text, "continue the analysis");
    }

    #[test]
    fn inject_empty_agent_id_is_error() {
        let bytes = br#"{"inject":{"agent_id":"","text":"hello"}}"#;
        assert!(parse_control_command(bytes).is_err());
    }

    #[test]
    fn inject_empty_text_is_error() {
        let bytes = br#"{"inject":{"agent_id":"scout-1","text":""}}"#;
        assert!(parse_control_command(bytes).is_err());
    }

    #[test]
    fn inject_too_large_is_error() {
        let big_text = "x".repeat(65_537);
        let bytes = serde_json::json!({"inject": {"agent_id": "scout-1", "text": big_text}})
            .to_string();
        assert!(parse_control_command(bytes.as_bytes()).is_err());
    }
}
