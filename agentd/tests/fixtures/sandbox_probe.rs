/// Sandbox probe fixture for integration tests (Landlock + seccomp).
///
/// Modes:
///   --path <path>
///       Read the file; exit 0=success, 1=access-denied, 2=other-error.
///
///   --sandbox-read <prefix> --path <path>
///       Apply AllowFsRead{prefix} to self (AFTER exec, so the binary itself
///       loads cleanly), then read the file.
///       Exit 0=success, 1=access-denied (Landlock enforcing), 2=other-error.
///
///   --sandbox-deny-spawn   (x86_64 Linux only)
///       Apply DenySpawn to self, then call libc::fork() directly.
///       The seccomp BPF filter kills the process via SIGSYS if DenySpawn works.
///       Exit 0 = fork was NOT blocked (test-failure path); killed = expected.
///
///   --exec
///       Spawn /bin/true via Command::new; exit 0=success, 1=failed.
use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // --sandbox-read <prefix> --path <path>
    // Apply AllowFsRead to self (after exec), then test file access.
    if let Some(sr_idx) = args.iter().position(|a| a == "--sandbox-read") {
        let prefix = match args.get(sr_idx + 1) {
            Some(p) => p.clone(),
            None => {
                eprintln!("sandbox-probe: --sandbox-read requires an argument");
                process::exit(2);
            }
        };
        let path_idx = match args.iter().position(|a| a == "--path") {
            Some(i) => i,
            None => {
                eprintln!("sandbox-probe: --sandbox-read requires --path <path>");
                process::exit(2);
            }
        };
        let path = match args.get(path_idx + 1) {
            Some(p) => p.clone(),
            None => {
                eprintln!("sandbox-probe: --path requires an argument");
                process::exit(2);
            }
        };

        #[cfg(target_os = "linux")]
        {
            let rules = vec![sandbox::SandboxRule::AllowFsRead { prefix }];
            if let Err(e) = sandbox::apply_sandbox(&rules) {
                eprintln!("sandbox-probe: apply_sandbox: {e}");
                process::exit(2);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = prefix;

        match std::fs::read(&path) {
            Ok(_) => process::exit(0),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => process::exit(1),
            Err(e) => {
                eprintln!("sandbox-probe: read error: {e}");
                process::exit(2);
            }
        }
    }

    // --sandbox-deny-spawn
    // Apply DenySpawn to self, then invoke the raw fork() syscall (57) via
    // libc::syscall(SYS_fork) to trigger the seccomp filter.  Must use the raw
    // syscall — both libc::fork() and Command::new() use clone() in modern glibc,
    // bypassing the fork/vfork filter (same class as BP-1 documented in THREAT_MODEL).
    if args.contains(&"--sandbox-deny-spawn".to_string()) {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let rules = vec![sandbox::SandboxRule::DenySpawn];
            if let Err(e) = sandbox::apply_sandbox(&rules) {
                eprintln!("sandbox-probe: apply_sandbox: {e}");
                process::exit(2);
            }
            // Use the raw fork() SYSCALL directly (not libc::fork() which uses
            // clone() in modern glibc, bypassing the BPF filter).
            // libc::syscall(SYS_fork=57) exercises exactly the syscall our filter
            // blocks.  SECCOMP_RET_KILL_PROCESS kills via SIGSYS before returning.
            let _ret = unsafe { libc::syscall(libc::SYS_fork) };
            // Reaching here means syscall 57 was NOT killed.
            process::exit(0);
        }
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        {
            eprintln!("sandbox-probe: --sandbox-deny-spawn requires linux x86_64");
            process::exit(2);
        }
    }

    // --path <path>  (no sandbox — raw read)
    if let Some(idx) = args.iter().position(|a| a == "--path") {
        let path = match args.get(idx + 1) {
            Some(p) => p,
            None => {
                eprintln!("sandbox-probe: --path requires an argument");
                process::exit(2);
            }
        };
        match std::fs::read(path) {
            Ok(_) => {
                println!("read ok");
                process::exit(0);
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox-probe: access denied: {e}");
                process::exit(1);
            }
            Err(e) => {
                eprintln!("sandbox-probe: error: {e}");
                process::exit(2);
            }
        }
    }

    // --exec  (no sandbox — spawn /bin/true)
    if args.contains(&"--exec".to_string()) {
        match process::Command::new("/bin/true").status() {
            Ok(s) if s.success() => process::exit(0),
            Ok(_) => process::exit(1),
            Err(e) => {
                eprintln!("sandbox-probe: exec error: {e}");
                process::exit(1);
            }
        }
    }

    eprintln!(
        "usage: sandbox-probe --path <file> \
         | --sandbox-read <prefix> --path <file> \
         | --sandbox-deny-spawn \
         | --exec"
    );
    process::exit(2);
}
