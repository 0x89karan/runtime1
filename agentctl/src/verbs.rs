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

pub fn run_cancel(args: CancelArgs) -> anyhow::Result<()> {
    let source = crate::watch::source::detect_source(args.url.as_deref(), &args.agents_dir)?;
    source.cancel(&args.agent_id).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("cancel requested for '{}' (takes effect at the next step boundary)", args.agent_id);
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
    source.set_budget(&args.agent_id, args.limit).map_err(|e| anyhow::anyhow!("{e}"))?;
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
    source.set_caps(&args.agent_id, &args.capabilities_json).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("capabilities for '{}' narrowed", args.agent_id);
    Ok(())
}
