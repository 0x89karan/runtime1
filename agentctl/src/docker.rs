//! ux.10 sub-part A — Docker Compose context detection + `docker compose logs` tailing.
//!
//! This module is deliberately process-level ONLY: it runs the two short `docker compose
//! ps` probes that decide whether a Compose project exists, and it starts the long-lived
//! `--follow` tail. It does not own the child (`watch::pump::Producers` does, so `Drop`
//! can kill it) and it does not parse the emitted lines (`watch::logs` does, since the
//! parsed `LogLine` is the view's data model).

use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Total backfill lines the tail should replay at startup. `docker compose logs --follow`
/// replays the ENTIRE history by default — potentially hundreds of thousands of lines on a
/// long-running project, all read, parsed, batched, and then immediately tail-dropped by the
/// 2 000-line ring.
///
/// This is a PROJECT budget, but `--tail` is per CONTAINER, so it is divided by the container
/// count (`backfill_per_container`). Passing a flat 500 meant this repo's own 4-service stack
/// replayed 4 × 500 = 2 000 lines — exactly the whole ring, so nothing but backfill was
/// visible and every live line immediately evicted history (/review's red-team pass).
const LOG_TAIL_BUDGET: usize = 600;
/// Floor, so a project with many services still keeps a little context per container.
const LOG_TAIL_MIN: usize = 40;

/// Per-container `--tail` value for `services` known services (see `LOG_TAIL_BUDGET`).
fn backfill_per_container(services: usize) -> usize {
    (LOG_TAIL_BUDGET / services.max(1)).max(LOG_TAIL_MIN)
}

/// A detected Docker Compose project: at least one container exists for the compose file
/// in agentctl's current working directory. `services` is the project's declared service
/// list — used for the Logs view's `Tab` filter and to resolve compose's per-line prefix
/// (`cos-1  | …`) back to a service name.
#[derive(Debug, Clone, Default)]
pub struct DockerContext {
    pub services: Vec<String>,
    /// Which project these logs come from, for display. The compose project is resolved from
    /// agentctl's CWD while the data source is resolved separately (`--url` / FUSE), so the two
    /// can legitimately describe different machines: run `agentctl watch --url http://host:7999`
    /// from an unrelated directory and the Logs view tails THAT directory's containers. Naming
    /// it in the view is what keeps that honest instead of passing local output off as the
    /// watched host's (/review's red-team pass).
    pub project: String,
}

/// Detect a Compose context ONCE at startup (D1). `docker compose ps --all --quiet` must exit
/// 0 AND list at least one container id; anything else — no `docker` binary, no daemon, no
/// compose file in the CWD, an empty project — means "not in Docker", and the Logs view
/// stays absent (on bare agentd / QEMU there is nothing to tail).
///
/// `--all` is deliberate: a STOPPED project still has readable history, and the postmortem
/// ("why did cos die?") is exactly when an operator wants the Logs view. Without it, compose
/// lists only running containers and the view would vanish precisely when it's most useful.
///
/// Called from `run_tui` BEFORE the terminal enters raw mode + the alternate screen (and
/// before the SIGTERM/SIGINT handler is installed, so Ctrl-C during a wedged probe still uses
/// the OS default disposition and actually kills the process). Gating is on detection alone,
/// NOT on the data source: an operator watching a container over `--url` from the repo root is
/// exactly the case where the Logs view is most useful, while `agentctl` *inside* the image
/// has no docker CLI and so detects nothing.
pub fn detect_docker_context() -> Option<DockerContext> {
    let probe = compose(&["ps", "--all", "--quiet"]).ok()?;
    if !probe.status.success() {
        return None;
    }
    if String::from_utf8_lossy(&probe.stdout).trim().is_empty() {
        return None;
    }
    // The daemon just answered, so this second probe is cheap. It is also best-effort:
    // `ps --quiet` returned container *ids*, and an empty service list only costs the Tab
    // filter its named entries — prefixes seen on real log lines are registered as they
    // arrive (see `LogsState::push_lines`).
    let services = compose(&["ps", "--services"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_services(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    Some(DockerContext { services, project: project_label() })
}

/// Human label for the compose project being tailed: the CWD's directory name, which is also
/// how compose derives the default project name. Falls back to the full path, then to `?`.
fn project_label() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .or_else(|| Some(p.to_string_lossy().to_string()))
        })
        .unwrap_or_else(|| "?".to_string())
}

/// Split `docker compose ps --services` output into a de-duplicated service list.
/// Extracted as a pure function so the parse is unit-testable without a docker daemon
/// (the shell-out itself is exercised at /qa, not in `cargo test`).
fn parse_services(stdout: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in stdout.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !out.iter().any(|s| s == name) {
            out.push(name.to_string());
        }
    }
    out
}

/// Start the log tail with stdout piped for the reader thread.
///
/// Flags, each load-bearing:
/// - `--follow`: stream forever. This child never EOFs on its own — `Producers::drop`
///   kills it (A2), which is what unblocks the reader thread parked in `fill_buf`.
/// - `--timestamps`: per-line RFC3339 timestamp, rendered relative/absolute (`[t]`).
/// - `--no-color`: no ANSI in the payload. The renderer sanitizes control characters
///   anyway, but stripping at the source keeps the parsed text honest.
/// - `--tail`: bound the initial backfill. PER CONTAINER, so the value is the project budget
///   divided by the container count (see `LOG_TAIL_BUDGET`).
/// - the service prefix is deliberately KEPT (no `--no-log-prefix`): it is the only thing
///   carrying which service a line came from, and the `Tab` service filter parses it.
///
/// stderr goes to `/dev/null`, never inherited: an inherited stderr would print docker's
/// diagnostics straight into the alternate screen and corrupt the frame. A failure
/// therefore shows up as a prompt EOF, which the reader surfaces as a visible notice line.
///
/// `process_group(0)` is load-bearing for the orphan-kill (A2), not tidiness. `docker
/// compose` is a **CLI plugin**: the `docker` process we spawn forks `docker-compose` as its
/// own child and passes our stdout pipe down to it. SIGKILL cannot be forwarded, so killing
/// only our direct child would leave the plugin alive — still holding the write end of the
/// pipe, still streaming, and blocked forever once the pipe filled. Making the child a
/// process-group leader lets `kill_group` take out the whole tree in one call.
pub fn spawn_compose_logs(services: usize) -> io::Result<Child> {
    let tail = backfill_per_container(services).to_string();
    Command::new("docker")
        .args([
            "compose",
            "logs",
            "--follow",
            "--timestamps",
            "--no-color",
            "--tail",
            &tail,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
}

/// Kill the log tail and everything it spawned, then reap it.
///
/// `kill(-pid)` targets the process GROUP `spawn_compose_logs` created (the child is its
/// leader, so pgid == pid), which is the only way to reach the `docker-compose` plugin
/// process. `child.kill()` follows as a fallback for the case where `process_group` didn't
/// take effect, and `wait()` reaps: `kill` only signals, and an unreaped child would sit as
/// a zombie for the rest of the agentctl process.
///
/// Signalling before `wait()` is deliberate — the pid cannot be recycled while the child is
/// still unreaped, so `-pid` can never name a group that has since been reused.
///
/// The reap is time-bounded for the same reason the startup probe is: this runs from
/// `Producers::drop` on the MAIN thread at quit, while raw mode and the alternate screen are
/// still active, so an unbounded `wait()` against a wedged docker CLI would freeze the TUI with
/// no working Ctrl-C and leave the terminal unusable (/review's red-team pass). A child that
/// somehow outlives the deadline is left unreaped — strictly better than hanging the quit path,
/// and it is killed, so it is not streaming.
pub fn kill_tail(child: &mut Child) {
    let pid = child.id() as libc::pid_t;
    if pid > 0 {
        // SAFETY: a plain kill(2) with a pid we own; failure (ESRCH — already gone) is fine.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let deadline = Instant::now() + REAP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() >= deadline => return,
            Ok(None) => std::thread::sleep(PROBE_POLL),
        }
    }
}

/// Wall-clock ceiling for a startup probe. A wedged Docker daemon (Desktop mid-start, a
/// half-dead socket) can leave `docker compose ps` hanging indefinitely, and this probe runs
/// before the first frame — so without a deadline the whole cockpit would refuse to start on
/// account of an OPTIONAL view. On timeout the probe is killed and detection reports "no
/// Docker", which is the correct degradation: `[l]` is absent, everything else works.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Poll granularity while waiting for a probe (cheap: 3 s / 50 ms = at most 60 `try_wait`s).
const PROBE_POLL: Duration = Duration::from_millis(50);
/// Ceiling on reaping a SIGKILLed tail at quit (see `kill_tail`). Short: this is on the path
/// between the operator pressing `q` and the terminal being restored.
const REAP_TIMEOUT: Duration = Duration::from_millis(500);

/// Run a short `docker compose …` probe, capturing both streams (so nothing leaks to the
/// terminal), never inheriting stdin, and giving up after `PROBE_TIMEOUT`.
///
/// Not `Command::output()`: that blocks forever on a wedged daemon. stdout is drained on a
/// side thread rather than with a post-exit `read_to_end`, because `docker` can exit while a
/// descendant still holds the pipe's write end — a `read_to_end` on this thread would then
/// block past the deadline it exists to enforce (found by /review round 2). Every exit path
/// here is bounded by `PROBE_TIMEOUT`.
/// Unlike the log tail, the probe deliberately stays in **agentctl's own process group**. Its
/// own group looks tidier but is worse: the terminal delivers Ctrl-C to the foreground group
/// only, so a probe in a separate group SURVIVES the Ctrl-C that kills agentctl — and this
/// probe runs before the TUI, exactly where an operator reaches for Ctrl-C (found by
/// `/review`'s structured Codex pass). Sharing the group means Ctrl-C reaches both; the
/// timeout path below kills the child directly, which is enough for a short-lived probe.
fn compose(args: &[&str]) -> io::Result<Output> {
    let mut child = Command::new("docker")
        .arg("compose")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let (out_tx, out_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    if let Some(mut pipe) = child.stdout.take() {
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut v = Vec::new();
            let _ = pipe.read_to_end(&mut v);
            let _ = out_tx.send(v);
        });
    }
    loop {
        match child.try_wait()? {
            Some(status) => {
                // Losing the output to a stuck descendant degrades to "no compose project"
                // (the Logs view stays absent), never to a hang. The child is already reaped
                // at this point, so its process group must NOT be signalled — the pid can
                // have been recycled, and killing a recycled group would hit an innocent
                // process.
                let remaining = deadline.saturating_duration_since(Instant::now());
                let stdout = out_rx.recv_timeout(remaining.max(PROBE_POLL)).unwrap_or_default();
                return Ok(Output { status, stdout, stderr: Vec::new() });
            }
            None if Instant::now() >= deadline => {
                // Direct kill, not a group kill: the probe shares agentctl's group (see
                // above), so `kill(-pgid)` here would signal agentctl itself.
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "docker compose probe timed out",
                ));
            }
            None => std::thread::sleep(PROBE_POLL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_services_trims_blanks_and_dedupes() {
        let out = "cos\n\n agent \nqdrant\ncos\n";
        assert_eq!(parse_services(out), vec!["cos", "agent", "qdrant"]);
    }

    #[test]
    fn parse_services_of_empty_output_is_empty() {
        assert!(parse_services("").is_empty());
        assert!(parse_services("   \n\n").is_empty());
    }

    /// Install `script` as a fake `docker` on PATH and run `f`. Serialized on `ENV_MUTEX`
    /// because PATH is process-global.
    fn with_fake_docker<T>(script: &str, f: impl FnOnce() -> T) -> T {
        let _env = crate::ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir  = tempfile::tempdir().unwrap();
        let fake = dir.path().join("docker");
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let prev = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", dir.path().display().to_string());
        let out = f();
        std::env::set_var("PATH", prev);
        out
    }

    /// The gate that decides whether the whole `[l]` view exists. Each "not in Docker" branch
    /// matters: a regression that treated an EMPTY project as present would make bare
    /// agentd/QEMU advertise `[l]ogs` and open a permanently empty view.
    #[test]
    fn detect_docker_context_requires_exit_zero_and_at_least_one_container() {
        assert!(
            with_fake_docker("#!/bin/sh\nexit 1\n", detect_docker_context).is_none(),
            "a non-zero exit is not a compose project"
        );
        assert!(
            with_fake_docker("#!/bin/sh\nexit 0\n", detect_docker_context).is_none(),
            "exit 0 with no container ids is an empty project"
        );
        let ctx = with_fake_docker(
            "#!/bin/sh\ncase \"$*\" in *--quiet*) echo abc123 ;; \
             *--services*) echo cos; echo agent ;; esac\n",
            detect_docker_context,
        )
        .expect("one container must be detected as a project");
        assert_eq!(ctx.services, vec!["cos", "agent"]);
        assert!(!ctx.project.is_empty(), "the project label must be populated for the title");
    }

    #[test]
    fn detect_docker_context_is_none_when_there_is_no_docker_binary() {
        // Empty PATH: the exec fails, which must degrade to "no Docker", not panic.
        assert!(with_fake_docker("#!/bin/sh\nexit 0\n", || {
            std::env::set_var("PATH", "");
            detect_docker_context()
        })
        .is_none());
    }

    /// The probe is deadline-bounded, so a wedged daemon cannot stop the cockpit from starting.
    #[test]
    fn a_hanging_probe_gives_up_within_the_deadline() {
        let started = Instant::now();
        let out = with_fake_docker("#!/bin/sh\nsleep 30\n", detect_docker_context);
        let elapsed = started.elapsed();
        assert!(out.is_none(), "a timed-out probe reports no Docker");
        assert!(
            elapsed < PROBE_TIMEOUT * 2,
            "probe must abandon a wedged daemon (took {elapsed:?})"
        );
    }

    /// /review round-2 regression: `docker` can exit while a descendant still holds the stdout
    /// pipe. A post-exit `read_to_end` on this thread would block past the deadline.
    #[test]
    fn a_probe_whose_grandchild_holds_stdout_still_returns_within_the_deadline() {
        let started = Instant::now();
        let out = with_fake_docker(
            "#!/bin/sh\ncase \"$*\" in *--quiet*) echo abc123 ;; *--services*) echo cos ;; esac\n\
             sh -c 'sleep 20' &\nexit 0\n",
            detect_docker_context,
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < PROBE_TIMEOUT * 3,
            "a descendant holding the pipe must not extend the probe (took {elapsed:?})"
        );
        // Whether the ids were captured before the stall is timing-dependent; the guarantee
        // under test is the bound, and that detection degrades rather than hanging.
        let _ = out;
    }

    /// Every flag is load-bearing and none of them is visible in a type: dropping
    /// `--timestamps` silently kills `[t]`, dropping `--tail` restores a full-history replay,
    /// and ADDING `--no-log-prefix` would leave the service filter nothing to parse.
    #[test]
    fn spawn_compose_logs_passes_the_load_bearing_flags() {
        let argv = with_fake_docker("#!/bin/sh\necho \"$@\"\n", || {
            let out = spawn_compose_logs(3).unwrap().wait_with_output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        });
        assert_eq!(
            argv,
            format!(
                "compose logs --follow --timestamps --no-color --tail {}",
                backfill_per_container(3)
            )
        );
        assert!(
            !argv.contains("--no-log-prefix"),
            "the prefix is the only thing carrying which service a line came from"
        );
    }

    #[test]
    fn backfill_is_a_project_budget_divided_across_containers() {
        // 4 services (this repo's own stack) must not replay the entire 2 000-line ring.
        assert!(backfill_per_container(4) * 4 <= crate::watch::logs::LOG_RING_CAP / 2);
        assert_eq!(backfill_per_container(1), LOG_TAIL_BUDGET);
        assert_eq!(backfill_per_container(0), LOG_TAIL_BUDGET, "no divide-by-zero");
        assert_eq!(backfill_per_container(1000), LOG_TAIL_MIN, "floored, never zero");
    }

    /// Is `pid` still alive? `kill(pid, 0)` only checks permission/existence.
    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 delivers nothing; it only probes for the process.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// A2 regression test, WITHOUT needing docker: a fake `docker` on PATH that forks a
    /// grandchild — exactly the shape of the real `docker compose` CLI plugin — and then
    /// waits. `kill_tail` must take out BOTH: killing only the direct child would leave the
    /// grandchild streaming into our pipe for the rest of the session, which is the orphan
    /// leak this increment had to fix.
    #[test]
    fn kill_tail_kills_the_forked_grandchild_not_just_the_direct_child() {
        use std::io::{BufRead, BufReader};

        let _env = crate::ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("docker");
        // The grandchild announces its own pid on stdout, then streams. `wait` keeps the
        // parent alive so a naive "kill the child only" would strand the grandchild.
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             sh -c 'echo grandchild=$$; while true; do echo tick; sleep 1; done' &\n\
             wait\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let prev_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{prev_path}", dir.path().display()));
        let spawned = spawn_compose_logs(1);
        std::env::set_var("PATH", &prev_path);
        let mut child = spawned.expect("fake docker should spawn");

        // Read the handshake on a side thread with a deadline: a bare `read_line` here blocks
        // forever if the fake script never runs (noexec tmpdir, missing /bin/sh), and libtest
        // applies no per-test timeout — a hung CI job instead of a failed assertion
        // (/review's testing pass).
        let stdout = child.stdout.take().unwrap();
        let (line_tx, line_rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut first = String::new();
            if reader.read_line(&mut first).is_ok() {
                let _ = line_tx.send(first);
            }
            // `reader` stays alive until this thread ends, holding the pipe's READ end open —
            // which is what makes this a clean isolation of the group kill: the grandchild
            // cannot die of SIGPIPE, so only `kill_tail` can have killed it.
            std::thread::sleep(std::time::Duration::from_secs(5));
        });
        let first = match line_rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(l) => l,
            Err(_) => {
                kill_tail(&mut child);
                panic!("fake docker never produced its handshake line");
            }
        };
        let grandchild: i32 = first
            .trim()
            .strip_prefix("grandchild=")
            .expect("fake docker should announce its grandchild pid")
            .parse()
            .unwrap();
        assert!(alive(grandchild));

        kill_tail(&mut child);

        // SIGKILL is asynchronous; give the kernel a moment to tear the group down.
        let mut gone = false;
        for _ in 0..50 {
            if !alive(grandchild) {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !gone {
            // Never leak a spinning process out of a failing test.
            unsafe { libc::kill(grandchild, libc::SIGKILL) };
        }
        assert!(gone, "kill_tail must reap the whole process group, not only the child");
    }
}
