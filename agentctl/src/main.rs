use std::path::{Path, PathBuf};

use agentd::template::TemplateResolver;
use clap::Parser;

mod approve;
mod auth;
mod brief;
mod docker;
mod inject;
mod jobs;
mod list;
mod orchestrate;
mod spawn;
mod verbs;
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
    /// Validate [[jobs]] schedules and print upcoming fire times — no running daemon
    /// needed (attn.4 DX: answers "will my schedule work?" without waiting up to 24h)
    Jobs(jobs::Args),
    /// Cancel an agent (cascade-cancels its spawned subtree) — ux.13
    Cancel(verbs::CancelArgs),
    /// Set a per-agent token budget at runtime (0 = unlimited) — ux.13
    SetBudget(verbs::SetBudgetArgs),
    /// Narrow an agent's capabilities at runtime (revoke/narrow-only) — ux.13
    SetCaps(verbs::SetCapsArgs),
    /// Fire a config-declared sealed job on demand, bypassing its schedule and ignoring
    /// shadow mode — attn.2-R5
    RunJob(verbs::RunJobArgs),
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
        Commands::Jobs(args) => jobs::run(args),
        Commands::Cancel(args) => verbs::run_cancel(args),
        Commands::SetBudget(args) => verbs::run_set_budget(args),
        Commands::SetCaps(args) => verbs::run_set_caps(args),
        Commands::RunJob(args) => verbs::run_run_job(args),
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

#[cfg(test)]
mod cli_tests {
    use super::*;

    /// ux.13-TUI: the overlay PRINTS an `agentctl …` command for every verb ("Equivalent: agentctl
    /// cancel scout-2") so the operator learns the fallback path and can paste it into an incident
    /// note. A printed command that does not parse is worse than none — it teaches a CLI that does not
    /// exist. This caught exactly that: the first draft printed `set-budget --agent X --limit N`, but
    /// `SetBudgetArgs` takes POSITIONAL args, so the copy would have failed for anyone who tried it.
    ///
    /// The guard runs the real clap parser over the real generated strings, so the two cannot drift.
    #[test]
    fn every_printed_equivalent_cli_actually_parses() {
        use crate::watch::overlay::PendingVerb;
        use clap::Parser as _;

        let verbs = [
            PendingVerb::Cancel { agent_id: "scout-2".to_string() },
            PendingVerb::SetBudget { agent_id: "scout-2".to_string(), limit: 47_000, park: true },
            PendingVerb::SetBudget { agent_id: "scout-2".to_string(), limit: 0, park: false },
        ];
        // Both forms: bare, and carrying the connection flags the TUI actually prints when attached
        // over HTTP. The flag-bearing form is the one an operator pastes, and /review found it was
        // ALSO the one that had never been parsed.
        for (verb, conn) in verbs.iter().flat_map(|v| {
            ["", " --url http://127.0.0.1:7999", " --agents-dir /tmp/agents"]
                .into_iter().map(move |c| (v, c))
        }) {
            let cmd = verb.equivalent_cli(conn);
            let argv: Vec<&str> = cmd.split_whitespace().collect();
            Cli::try_parse_from(&argv)
                .map(|_| ())
                .unwrap_or_else(|e| panic!("the overlay prints an unparseable command '{cmd}': {e}"));
        }
    }
}
