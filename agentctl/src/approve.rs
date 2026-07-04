use clap::Parser;

use crate::watch::source::detect_source;

/// Approve a pending agent action (via FUSE or HTTP management API).
#[derive(Parser)]
pub struct ApproveArgs {
    /// Approval request ID (shown in `agentctl watch` Approvals pane).
    pub id: String,
    /// Path to the /agents FUSE mount (default: /agents).
    #[arg(long, default_value = "/agents")]
    pub agents_dir: std::path::PathBuf,
    /// URL of the management HTTP API (overrides FUSE; e.g. http://HOST:7999).
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,
}

/// Deny a pending agent action (via FUSE or HTTP management API).
#[derive(Parser)]
pub struct DenyArgs {
    /// Approval request ID (shown in `agentctl watch` Approvals pane).
    pub id: String,
    /// Optional reason for denial.
    #[arg(long)]
    pub reason: Option<String>,
    /// Path to the /agents FUSE mount (default: /agents).
    #[arg(long, default_value = "/agents")]
    pub agents_dir: std::path::PathBuf,
    /// URL of the management HTTP API (overrides FUSE; e.g. http://HOST:7999).
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,
}

pub fn run_approve(args: ApproveArgs) -> anyhow::Result<()> {
    let source = detect_source(args.url.as_deref(), &args.agents_dir)?;
    source.approve(&args.id).map_err(|e| anyhow::anyhow!(e))?;
    println!("approved: {}", args.id);
    Ok(())
}

pub fn run_deny(args: DenyArgs) -> anyhow::Result<()> {
    let source = detect_source(args.url.as_deref(), &args.agents_dir)?;
    source
        .deny(&args.id, args.reason.as_deref())
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("denied: {}", args.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn approve_args_parse_minimal() {
        let args = ApproveArgs::try_parse_from(["agentctl", "approval-id"]).unwrap();
        assert_eq!(args.id, "approval-id");
        assert_eq!(args.agents_dir, PathBuf::from("/agents"));
        assert!(args.url.is_none());
    }

    #[test]
    fn deny_args_parse_with_reason() {
        let args = DenyArgs::try_parse_from([
            "agentctl", "approval-id", "--reason", "too risky",
        ])
        .unwrap();
        assert_eq!(args.id, "approval-id");
        assert_eq!(args.reason.as_deref(), Some("too risky"));
    }

    #[test]
    fn approve_no_server_returns_error() {
        let args = ApproveArgs {
            id:          "act_0".to_string(),
            agents_dir:  PathBuf::from("/nonexistent_dx2_test"),
            url:         None,
        };
        // detect_source falls back to localhost:7999 — if no server is running,
        // either detect_source errs or approve errs. Either way the overall result is Err.
        let result = run_approve(args);
        assert!(result.is_err(), "approve with no FUSE/server must fail");
    }

    #[test]
    fn deny_no_server_returns_error() {
        let args = DenyArgs {
            id:          "act_0".to_string(),
            reason:      Some("test".to_string()),
            agents_dir:  PathBuf::from("/nonexistent_dx2_test"),
            url:         None,
        };
        let result = run_deny(args);
        assert!(result.is_err(), "deny with no FUSE/server must fail");
    }
}
