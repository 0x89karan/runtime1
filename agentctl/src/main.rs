use std::path::{Path, PathBuf};

use agentd::template::TemplateResolver;
use clap::Parser;

mod approve;
mod auth;
mod brief;
mod inject;
mod list;
mod orchestrate;
mod spawn;
mod verify;
mod watch;

#[derive(Parser)]
#[command(
    name = "agentctl",
    version,
    about = "AgentOS operator CLI",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Approve a pending agent action (via FUSE or HTTP management API)
    Approve(approve::ApproveArgs),
    /// Deny a pending agent action (via FUSE or HTTP management API)
    Deny(approve::DenyArgs),
    /// Provision OAuth credentials for use by Docker agents
    Auth(auth::AuthCmd),
    /// Show the Chief-of-Staff morning brief (durable pull; ux.11c)
    Brief(brief::BriefArgs),
    /// Inject a new user turn into a waiting orchestrated agent
    Inject(inject::InjectArgs),
    ListTemplates(list::Args),
    /// Start an interactive orchestration REPL (orch.1+)
    Orchestrate(orchestrate::OrchestrateArgs),
    Spawn(spawn::Args),
    Verify(verify::Args),
    Watch(watch::Args),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Approve(args) => approve::run_approve(args),
        Commands::Deny(args) => approve::run_deny(args),
        Commands::Auth(cmd) => auth::run(cmd),
        Commands::Brief(args) => brief::run(args),
        Commands::Inject(args) => inject::run(args),
        Commands::ListTemplates(args) => list::run(args),
        Commands::Orchestrate(args) => orchestrate::run(args),
        Commands::Spawn(args) => spawn::run(args),
        Commands::Verify(args) => verify::run(args),
        Commands::Watch(args) => watch::run(args),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Build a `TemplateResolver` from optional CLI overrides.
///
/// Priority (highest to lowest):
/// 1. `--user-templates-dir` / `--repo-dir` flags (or env vars picked up by clap)
/// 2. Heuristic defaults (`~/.agentos/templates/` + auto-detected repo dir)
pub(crate) fn build_resolver(
    user_templates_dir: Option<&Path>,
    repo_dir: Option<&Path>,
) -> TemplateResolver {
    let user_dir = user_templates_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_user_dir);
    let repo_dir = repo_dir
        .map(|p| p.to_path_buf())
        .unwrap_or_else(default_repo_dir);
    TemplateResolver::new(repo_dir, user_dir)
}

fn default_user_dir() -> PathBuf {
    std::env::var("HOME")
        .map(|h| PathBuf::from(h).join(".agentos").join("templates"))
        .unwrap_or_else(|_| PathBuf::from("/run/user/templates"))
}

/// Crate-level mutex that serializes tests mutating process env vars.
/// Using a process-wide lock prevents parallel test races on ANTHROPIC_API_KEY.
#[cfg(test)]
pub(crate) static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn default_repo_dir() -> PathBuf {
    // Walk up from the binary location to find a `templates/` directory.
    // This lets `./target/debug/agentctl` find `templates/` at the repo root in dev,
    // and falls back to the QEMU in-image path when no `templates/` is found walking up.
    if let Ok(exe) = std::env::current_exe() {
        let mut check = exe.as_path();
        for _ in 0..8 {
            if let Some(parent) = check.parent() {
                let candidate = parent.join("templates");
                if candidate.is_dir() {
                    return candidate;
                }
                check = parent;
            }
        }
    }
    PathBuf::from("/etc/agentd/templates")
}
