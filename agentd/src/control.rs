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
}

/// Commands dispatched through the /agents/control write surface.
#[derive(Debug)]
pub enum ControlCommand {
    Spawn(OperatorSpawnRequest),
}

/// Parse raw bytes from a FUSE write into a ControlCommand.
/// Returns an error for malformed JSON or a missing `task` field.
pub fn parse_control_command(bytes: &[u8]) -> anyhow::Result<ControlCommand> {
    let req: OperatorSpawnRequest = serde_json::from_slice(bytes)
        .map_err(|e| anyhow::anyhow!("invalid control JSON: {e}"))?;
    anyhow::ensure!(!req.task.is_empty(), "control command: task must not be empty");
    Ok(ControlCommand::Spawn(req))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_spawn() {
        let bytes = br#"{"task":"scan repo for TODOs"}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Spawn(req) = cmd;
        assert_eq!(req.task, "scan repo for TODOs");
        assert!(req.id.is_none());
        assert!(req.capabilities.is_none());
    }

    #[test]
    fn parse_full_spawn() {
        let bytes = br#"{"task":"search","id":"op-scout","max_turns":5,"token_budget":50000,"priority":1}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Spawn(req) = cmd;
        assert_eq!(req.task, "search");
        assert_eq!(req.id.as_deref(), Some("op-scout"));
        assert_eq!(req.max_turns, Some(5));
        assert_eq!(req.token_budget, Some(50_000));
        assert_eq!(req.priority, Some(1));
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
}
