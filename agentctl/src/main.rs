use std::path::{Path, PathBuf};

use agentd::template::TemplateResolver;
use clap::Parser;

mod list;
mod spawn;

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
    ListTemplates(list::Args),
    Spawn(spawn::Args),
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::ListTemplates(args) => list::run(args),
        Commands::Spawn(args) => spawn::run(args),
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
