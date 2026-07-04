pub mod google;

#[derive(clap::Args)]
#[command(subcommand_required = true, arg_required_else_help = true)]
pub struct AuthCmd {
    #[command(subcommand)]
    pub command: AuthCommands,
}

#[derive(clap::Subcommand)]
pub enum AuthCommands {
    /// Provision Google OAuth credentials to ~/.agentos-secrets/google.json
    Google(google::Args),
}

pub fn run(cmd: AuthCmd) -> anyhow::Result<()> {
    match cmd.command {
        AuthCommands::Google(args) => google::run(args),
    }
}
