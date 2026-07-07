use clap::Args;

#[derive(Args, Debug)]
pub struct InjectArgs {
    /// Agent ID to inject into
    pub agent_id: String,
    /// Text to inject (all remaining arguments joined with spaces)
    #[arg(trailing_var_arg = true, required = true)]
    pub text: Vec<String>,
    /// Management API URL (overrides auto-detection)
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,
    /// FUSE agents directory
    #[arg(long, default_value = "/agents")]
    pub agents_dir: std::path::PathBuf,
}

pub fn run(args: InjectArgs) -> anyhow::Result<()> {
    let text = args.text.join(" ");
    anyhow::ensure!(!text.is_empty(), "inject: text must not be empty");

    let source = crate::watch::source::detect_source(args.url.as_deref(), &args.agents_dir)?;
    source.inject(&args.agent_id, &text)
        .map_err(|e| anyhow::anyhow!("{e}"))
}
