//! ux.13 control verbs: `agentctl cancel|set-budget|set-caps`. Each resolves a DataSource
//! (HTTP `--url` or FUSE `--agents-dir`, like `inject`) and drives the matching method.

use clap::Args;

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Agent ID to cancel (cascade-cancels its spawned subtree at the next step boundary)
    pub agent_id: String,
    /// Management API URL (overrides auto-detection)
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,
    /// FUSE agents directory
    #[arg(long, default_value = "/agents")]
    pub agents_dir: std::path::PathBuf,
}

/// Map a verb failure through the SAME copy the cockpit shows. The DX phase's finding was that the
/// CLI and the TUI must not describe one write two ways; /qa found the error paths still did —
/// `agentctl cancel nope` printed `HTTP 404: {"error":"agent 'nope' not found"}` while the overlay
/// explained what it meant and what to do.
fn explain(e: String) -> anyhow::Error {
    anyhow::anyhow!("{}", crate::watch::explain_verb_error(&e))
}

pub fn run_cancel(args: CancelArgs) -> anyhow::Result<()> {
    let source = crate::watch::source::detect_source(args.url.as_deref(), &args.agents_dir)?;
    // ux.13-TUI: the route returns the cascade size (native subtree + universal agents parented into
    // it). The TUI now reports it, so the CLI does too — one vocabulary for one write.
    let count = source.cancel(&args.agent_id).map_err(explain)?;
    if count > 0 {
        println!(
            "cancel requested for '{}' — {count} agent{} flagged (takes effect at the next step boundary)",
            args.agent_id,
            if count == 1 { "" } else { "s" },
        );
    } else {
        println!("cancel requested for '{}' (takes effect at the next step boundary)", args.agent_id);
    }
    Ok(())
}

#[derive(Args, Debug)]
pub struct SetBudgetArgs {
    /// Agent ID
    pub agent_id: String,
    /// New per-agent token budget (0 = unlimited)
    pub limit: u64,
    /// Management API URL (overrides auto-detection)
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,
    /// FUSE agents directory
    #[arg(long, default_value = "/agents")]
    pub agents_dir: std::path::PathBuf,
}

pub fn run_set_budget(args: SetBudgetArgs) -> anyhow::Result<()> {
    let source = crate::watch::source::detect_source(args.url.as_deref(), &args.agents_dir)?;
    source.set_budget(&args.agent_id, args.limit).map_err(explain)?;
    println!("budget for '{}' set to {}", args.agent_id, args.limit);
    Ok(())
}

#[derive(Args, Debug)]
pub struct SetCapsArgs {
    /// Agent ID
    pub agent_id: String,
    /// Capabilities as a JSON array, e.g. '[{"KbRead":{"segment":"ops:briefs"}}]'.
    /// Revoke/narrow-only: the new set must be covered by the agent's current caps.
    pub capabilities_json: String,
    /// Management API URL (overrides auto-detection)
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,
    /// FUSE agents directory
    #[arg(long, default_value = "/agents")]
    pub agents_dir: std::path::PathBuf,
}

pub fn run_set_caps(args: SetCapsArgs) -> anyhow::Result<()> {
    let source = crate::watch::source::detect_source(args.url.as_deref(), &args.agents_dir)?;
    source.set_caps(&args.agent_id, &args.capabilities_json).map_err(explain)?;
    println!("capabilities for '{}' narrowed", args.agent_id);
    Ok(())
}

/// attn.2-R5: `agentctl jobs run <job_id>` — the CLI equivalent of the Jobs view's manual-fire
/// verb, and what its confirm overlay's "equivalent CLI" line actually prints
/// (`PendingVerb::RunJob::equivalent_cli`) — this must exist and stay in sync, or the TUI would
/// teach a command that does not run (the exact class of bug `explain_verb_error`'s doc comment
/// warns about for error text).
#[derive(Args, Debug)]
pub struct RunJobArgs {
    /// Job id to fire (from `[[jobs]]` in the connected agentd's config)
    pub job_id: String,
    /// Management API URL (overrides auto-detection)
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,
    /// FUSE agents directory
    #[arg(long, default_value = "/agents")]
    pub agents_dir: std::path::PathBuf,
}

pub fn run_run_job(args: RunJobArgs) -> anyhow::Result<()> {
    let source = crate::watch::source::detect_source(args.url.as_deref(), &args.agents_dir)?;
    let child_id = source.run_job(&args.job_id).map_err(explain)?;
    println!("fired '{}' — child '{child_id}'", args.job_id);
    Ok(())
}
