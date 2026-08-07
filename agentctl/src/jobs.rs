//! `agentctl jobs` — attn.4 DX finding: TTHW for a scheduled job was "wait up to 24h for
//! the next real fire, or read Rust source." This subcommand validates every job's
//! `schedule` and prints its next few computed fire times WITHOUT touching a running
//! daemon or restarting anything — reusing `agentd::scheduler_cron`'s exact parsing/next-
//! fire logic (not a reimplementation) so this command's answer can never drift from what
//! the real scheduler would actually do.

use std::path::PathBuf;

use agentd::config::Config;

#[derive(clap::Args)]
pub struct Args {
    /// Path to the agentd TOML config to check (e.g. cos.agents.toml).
    pub config: PathBuf,
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(&args.config)
        .map_err(|e| anyhow::anyhow!("reading {:?}: {e}", args.config))?;
    let cfg: Config = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("parsing {:?}: {e}", args.config))?;

    if cfg.jobs.is_empty() {
        println!("no [[jobs]] declared in {:?}", args.config);
        return Ok(());
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut any_invalid = false;
    for job in &cfg.jobs {
        print!("{}: ", job.id);
        match &job.schedule {
            None => println!("manual-fire-only (no schedule declared)"),
            Some(expr) => match job.validate_schedule() {
                Err(err) => {
                    any_invalid = true;
                    println!("INVALID — {err}");
                }
                Ok(()) => {
                    println!("{}", agentd::scheduler_cron::describe(expr));
                    let mut after = now;
                    for i in 1..=3 {
                        match agentd::scheduler_cron::next_fire_after(expr, after) {
                            Ok(ts) => {
                                let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
                                    .map(|d| d.to_rfc3339())
                                    .unwrap_or_else(|| ts.to_string());
                                println!("  next fire #{i}: {dt}");
                                after = ts;
                            }
                            Err(e) => {
                                println!("  next fire #{i}: could not compute ({e})");
                                break;
                            }
                        }
                    }
                }
            },
        }
    }

    if any_invalid {
        anyhow::bail!(
            "one or more schedules are invalid — a running agentd degrades these jobs to \
             manual-fire-only (never fails boot); this command exits non-zero so it's usable \
             in CI/pre-deploy checks"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(raw: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.toml");
        std::fs::write(&path, raw).unwrap();
        (dir, path)
    }

    #[test]
    fn no_jobs_declared_is_ok_and_does_not_bail() {
        let (_dir, path) = write_config("");
        let result = run(Args { config: path });
        assert!(result.is_ok(), "an empty config (no [[jobs]]) must not error: {result:?}");
    }

    #[test]
    fn all_valid_schedules_is_ok() {
        let (_dir, path) = write_config(
            r#"
[[jobs]]
id = "cos-inbox"
schedule = "0 8 * * *"
capabilities = []
task = "t"

[[jobs]]
id = "cos-curator"
schedule = "5 8 * * *"
capabilities = []
task = "t"
"#,
        );
        let result = run(Args { config: path });
        assert!(result.is_ok(), "all-valid schedules must exit 0: {result:?}");
    }

    #[test]
    fn manual_fire_only_job_with_no_schedule_is_ok() {
        let (_dir, path) = write_config(
            r#"
[[jobs]]
id = "manual-job"
capabilities = []
task = "t"
"#,
        );
        let result = run(Args { config: path });
        assert!(result.is_ok(), "a job with no schedule field must not error: {result:?}");
    }

    /// The exact behavior the testing specialist flagged as untested: a malformed schedule
    /// must make the command exit non-zero (`Err`), since the doc comment explicitly
    /// promises this is "usable in CI/pre-deploy checks".
    #[test]
    fn malformed_schedule_bails_with_non_zero_exit() {
        let (_dir, path) = write_config(
            r#"
[[jobs]]
id = "broken-job"
schedule = "not a cron string"
capabilities = []
task = "t"
"#,
        );
        let result = run(Args { config: path });
        assert!(result.is_err(), "a malformed schedule must exit non-zero for CI use");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("invalid") || msg.contains("degrades"), "error must explain why: {msg}");
    }

    #[test]
    fn one_invalid_among_several_still_bails() {
        // The "any_invalid" accumulator must catch a failure even when it's not the first job.
        let (_dir, path) = write_config(
            r#"
[[jobs]]
id = "good-job"
schedule = "0 8 * * *"
capabilities = []
task = "t"

[[jobs]]
id = "bad-job"
schedule = "99 8 * * *"
capabilities = []
task = "t"
"#,
        );
        let result = run(Args { config: path });
        assert!(result.is_err(), "one invalid schedule among several must still bail");
    }

    #[test]
    fn missing_config_file_errors_with_the_path() {
        let missing = PathBuf::from("/nonexistent/path/agents.toml");
        let result = run(Args { config: missing.clone() });
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("nonexistent"),
            "error should name the path that couldn't be read"
        );
    }

    #[test]
    fn unparseable_toml_errors_distinctly_from_a_missing_file() {
        let (_dir, path) = write_config("this is not valid toml [[[");
        let result = run(Args { config: path });
        assert!(result.is_err(), "unparseable TOML must error, not panic");
    }
}
