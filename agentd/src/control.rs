use crate::capability::Capability;

/// An agent spawn request sent via the /agents/control FUSE surface or management HTTP API.
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
    /// Optional confirmation channel (not serialized). The scheduler sends the assigned
    /// agent ID after insertion. Used by the HTTP API to return 201 + agent_id synchronously.
    #[serde(skip)]
    pub confirm_tx: Option<tokio::sync::oneshot::Sender<String>>,
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
    /// Reset a budget window: rebase the target's window anchor to current spend
    /// (the D2 manual escape hatch — clears the windowed ceiling without
    /// destroying the lifetime meter). ux.8′.
    ResetBudget {
        target:     BudgetTarget,
        /// Optional confirmation channel. The scheduler sends
        /// `(spent_before, window_start)` so the HTTP API can report old→new and
        /// the new window anchor; `Err` for an unknown agent (→ 404). `None` on
        /// the fire-and-forget FUSE path. (ControlCommand is not a serde type.)
        confirm_tx: Option<tokio::sync::oneshot::Sender<Result<(u64, u64), String>>>,
    },
    /// Set a per-agent token budget ceiling at runtime (ux.11a). `limit = 0` = unlimited.
    /// `BudgetTarget::Global` is rejected (the global ceiling is immutable config); only
    /// `Agent(id)` is honored. Raising a ceiling revives a deferred agent (drain).
    SetBudget {
        target:     BudgetTarget,
        limit:      u64,
        /// Confirmation channel. The scheduler sends `(old_budget, new_budget)` on success,
        /// `Err` for an unknown agent (→ 404) or a Global target (→ 400 at the HTTP layer).
        /// `None` on the fire-and-forget FUSE path.
        confirm_tx: Option<tokio::sync::oneshot::Sender<Result<(u64, u64), String>>>,
    },
    /// Cancel a running/parked agent at the next step boundary (ux.13). Cascade-cancels
    /// the whole spawned subtree; each node stops before any NEW world-affecting dispatch.
    Cancel {
        agent_id:   String,
        /// Confirmation channel. Sends `Ok(count)` = number of cancelled nodes (incl.
        /// cascaded children), or `Err` for an unknown agent (→ 404). `None` on the
        /// fire-and-forget FUSE path.
        confirm_tx: Option<tokio::sync::oneshot::Sender<Result<u64, String>>>,
    },
    /// Narrow a running agent's capabilities at runtime (ux.13, revoke/narrow-only — a
    /// misconfig-repair ergonomic, NOT a security response). The new set must be covered
    /// by the current set (widening is rejected). Takes effect at the next step boundary.
    SetCaps {
        agent_id:     String,
        capabilities: Vec<Capability>,
        /// Confirmation channel. Sends `Ok((old_len, new_len))` on success, `Err` for an
        /// unknown agent (→ 404) or a widening / inert-cap request (→ 400 at the HTTP
        /// layer). `None` on the fire-and-forget FUSE path.
        confirm_tx:   Option<tokio::sync::oneshot::Sender<Result<(usize, usize), String>>>,
    },
    /// Fire a config-declared sealed job (cap.2b) on demand, bypassing its `schedule`
    /// entirely (attn.2-R5 redesign). Always dispatches for real — `native_cron_shadow`
    /// only governs the automatic schedule-driven path; a manual trigger is an explicit
    /// operator override, and is the only way to exercise the pipeline while shadow mode
    /// is on. Routes through the SAME `dispatch_scheduled_job` primitive the native tick
    /// uses (parent_id: None, no RunJob capability check needed — the job's capabilities
    /// are config-declared, not caller-supplied), which sidesteps the two bugs the
    /// original R5 design (built on `dispatch_run_job` + a synthetic parent id) was found
    /// to have: a panic in `reject()` on every rejection path, and a silently-halved
    /// token budget from a nonexistent-parent model-config fallback.
    RunJobNow {
        job_id:     String,
        /// Confirmation channel. Sends `Ok(child_id)` on success, `Err` for an unknown job id
        /// (→ 404) or a job that already has a live run (→ 409 at the HTTP layer — the
        /// `reject_if_job_already_running` guard's normal, expected rejection on any repeat
        /// fire while one is in progress; a bare nanosecond-timestamp `child_id` collision is
        /// the other, vanishingly rare, 409 cause). `None` on the fire-and-forget FUSE path.
        confirm_tx: Option<tokio::sync::oneshot::Sender<Result<String, String>>>,
    },
}

/// Which budget a `ResetBudget` command targets. Typed (not a bare string) so
/// "global" can never collide with an agent literally named `global`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BudgetTarget {
    Global,
    Agent(String),
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
    #[serde(rename = "reset_budget")]
    ResetBudget {
        target: BudgetTarget,
    },
    #[serde(rename = "set_budget")]
    SetBudget {
        target: BudgetTarget,
        limit:  u64,
    },
    Cancel {
        agent_id: String,
    },
    #[serde(rename = "set_caps")]
    SetCaps {
        agent_id:     String,
        capabilities: Vec<Capability>,
    },
    #[serde(rename = "run_job")]
    RunJobNow {
        job_id: String,
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
            TaggedCommand::ResetBudget { target } => {
                if let BudgetTarget::Agent(id) = &target {
                    anyhow::ensure!(!id.is_empty(), "reset_budget: agent id must not be empty");
                }
                // FUSE path is fire-and-forget; the HTTP path supplies confirm_tx.
                ControlCommand::ResetBudget { target, confirm_tx: None }
            }
            TaggedCommand::SetBudget { target, limit } => {
                if let BudgetTarget::Agent(id) = &target {
                    anyhow::ensure!(!id.is_empty(), "set_budget: agent id must not be empty");
                }
                // FUSE path is fire-and-forget; the HTTP path supplies confirm_tx.
                ControlCommand::SetBudget { target, limit, confirm_tx: None }
            }
            TaggedCommand::Cancel { agent_id } => {
                anyhow::ensure!(!agent_id.is_empty(), "cancel: agent_id must not be empty");
                ControlCommand::Cancel { agent_id, confirm_tx: None }
            }
            TaggedCommand::SetCaps { agent_id, capabilities } => {
                anyhow::ensure!(!agent_id.is_empty(), "set_caps: agent_id must not be empty");
                ControlCommand::SetCaps { agent_id, capabilities, confirm_tx: None }
            }
            TaggedCommand::RunJobNow { job_id } => {
                anyhow::ensure!(!job_id.is_empty(), "run_job: job_id must not be empty");
                // FUSE path is fire-and-forget; the HTTP path supplies confirm_tx.
                ControlCommand::RunJobNow { job_id, confirm_tx: None }
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
    fn parse_set_budget_tagged() {
        let bytes = br#"{"set_budget":{"target":{"agent":"cos"},"limit":50000}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::SetBudget { target, limit, confirm_tx } = cmd else {
            panic!("expected SetBudget")
        };
        assert!(matches!(target, BudgetTarget::Agent(ref id) if id == "cos"));
        assert_eq!(limit, 50_000);
        assert!(confirm_tx.is_none(), "FUSE path is fire-and-forget");
    }

    #[test]
    fn parse_set_budget_empty_agent_id_is_error() {
        let bytes = br#"{"set_budget":{"target":{"agent":""},"limit":1}}"#;
        assert!(parse_control_command(bytes).is_err());
    }

    #[test]
    fn parse_set_budget_global_target_parses() {
        // Global parses at the command layer; rejection happens in dispatch/HTTP (400).
        let bytes = br#"{"set_budget":{"target":"global","limit":1000}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        assert!(matches!(cmd, ControlCommand::SetBudget { target: BudgetTarget::Global, .. }));
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

    #[test]
    fn parse_cancel_tagged() {
        let bytes = br#"{"cancel":{"agent_id":"scout-1"}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::Cancel { agent_id, confirm_tx } = cmd else { panic!("expected Cancel") };
        assert_eq!(agent_id, "scout-1");
        assert!(confirm_tx.is_none(), "FUSE path is fire-and-forget");
    }

    #[test]
    fn parse_cancel_empty_id_is_error() {
        assert!(parse_control_command(br#"{"cancel":{"agent_id":""}}"#).is_err());
    }

    #[test]
    fn parse_set_caps_tagged() {
        let bytes = br#"{"set_caps":{"agent_id":"scout-1","capabilities":[{"KbRead":{"segment":"ops:briefs"}}]}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::SetCaps { agent_id, capabilities, confirm_tx } = cmd else {
            panic!("expected SetCaps")
        };
        assert_eq!(agent_id, "scout-1");
        assert_eq!(capabilities.len(), 1);
        assert!(confirm_tx.is_none(), "FUSE path is fire-and-forget");
    }

    #[test]
    fn parse_set_caps_empty_list_ok() {
        // An empty cap set is the maximal narrow (deny-all) — valid.
        let bytes = br#"{"set_caps":{"agent_id":"scout-1","capabilities":[]}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::SetCaps { capabilities, .. } = cmd else { panic!("expected SetCaps") };
        assert!(capabilities.is_empty());
    }

    #[test]
    fn parse_set_caps_empty_id_is_error() {
        assert!(parse_control_command(br#"{"set_caps":{"agent_id":"","capabilities":[]}}"#).is_err());
    }

    #[test]
    fn parse_run_job_tagged() {
        let bytes = br#"{"run_job":{"job_id":"cos-inbox"}}"#;
        let cmd = parse_control_command(bytes).unwrap();
        let ControlCommand::RunJobNow { job_id, confirm_tx } = cmd else { panic!("expected RunJobNow") };
        assert_eq!(job_id, "cos-inbox");
        assert!(confirm_tx.is_none(), "FUSE path is fire-and-forget");
    }

    #[test]
    fn parse_run_job_empty_id_is_error() {
        assert!(parse_control_command(br#"{"run_job":{"job_id":""}}"#).is_err());
    }
}
