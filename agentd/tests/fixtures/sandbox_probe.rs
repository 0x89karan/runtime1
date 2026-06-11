/// Sandbox probe binary used by integration tests to verify Landlock + seccomp.
///
/// Usage:
///   sandbox-probe --path <path>   Try to read the file; exit 0=success, 1=denied, 2=error
///   sandbox-probe --exec          Try to exec /bin/true; exit 0=allowed (bad!), non-zero=blocked
///
/// Exit codes:
///   0  — operation succeeded (may indicate sandbox is NOT blocking)
///   1  — EACCES / EPERM (expected when Landlock/seccomp is working)
///   2  — other error or bad usage
use std::process;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if let Some(idx) = args.iter().position(|a| a == "--path") {
        let path = args.get(idx + 1).unwrap_or_else(|| {
            eprintln!("sandbox-probe: --path requires an argument");
            process::exit(2);
        });

        match std::fs::read(path) {
            Ok(_) => {
                println!("read ok");
                process::exit(0);
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("sandbox-probe: access denied (EACCES/EPERM): {e}");
                process::exit(1);
            }
            Err(e) => {
                eprintln!("sandbox-probe: error: {e}");
                process::exit(2);
            }
        }
    }

    if args.contains(&"--exec".to_string()) {
        let result = process::Command::new("/bin/true").status();
        match result {
            Ok(status) if status.success() => {
                println!("exec succeeded — seccomp is NOT blocking (expected for unsandboxed)");
                process::exit(0);
            }
            Ok(_) => {
                eprintln!("exec completed with non-zero status");
                process::exit(1);
            }
            Err(e) => {
                eprintln!("sandbox-probe: exec error: {e}");
                process::exit(1);
            }
        }
    }

    eprintln!("usage: sandbox-probe --path <file> | --exec");
    process::exit(2);
}
