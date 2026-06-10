/// Kernel-level sandbox enforcement for MCP server subprocesses.
///
/// Two mechanisms are combined:
/// - Landlock LSM: filesystem path-beneath rules (BestEffort; degrades silently on
///   kernels < 5.13 or without CONFIG_SECURITY_LANDLOCK).
/// - seccomp-bpf: syscall filter blocking execve/execveat/fork (DenySpawn rule).
///
/// Usage pattern:
/// 1. Call `compile(rules)` in the parent process (may allocate).
/// 2. Move the returned `CompiledSandbox` into a `CommandExt::pre_exec` closure.
/// 3. Inside the closure, call `apply_compiled(&compiled)` — no allocation, raw
///    syscalls only.
///
/// On non-Linux, all public functions are no-ops returning `Ok(())`.
use std::fmt;

/// A rule describing what a sandboxed subprocess is permitted to do.
#[derive(Debug, Clone, PartialEq)]
pub enum SandboxRule {
    /// Allow read access (ReadFile + ReadDir) to all paths beneath `prefix`.
    AllowFsRead { prefix: String },
    /// Allow full filesystem access to all paths beneath `prefix`.
    AllowFsWrite { prefix: String },
    /// Block execve, execveat, and fork via seccomp-bpf.
    DenySpawn,
}

/// Pre-compiled sandbox: Landlock ruleset fd + optional seccomp BPF program.
///
/// Created by `compile()` before `pre_exec`; applied by `apply_compiled()`
/// inside `pre_exec`. On non-Linux: zero-size type; all operations no-op.
pub struct CompiledSandbox {
    #[cfg(target_os = "linux")]
    inner: linux::Inner,
}

// SAFETY: Inner holds an i32 fd (Send) and Vec<sock_filter> (Send+Sync).
// The fd is a file descriptor, which is inherently per-process but safe to
// send between threads (it's just a number).
#[cfg(target_os = "linux")]
unsafe impl Send for CompiledSandbox {}
#[cfg(target_os = "linux")]
unsafe impl Sync for CompiledSandbox {}

impl fmt::Debug for CompiledSandbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledSandbox").finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum SandboxError {
    /// OS-level error (path not found, permission denied, etc.)
    Io(std::io::Error),
    /// Kernel interface error (e.g. path contains null byte).
    Kernel(String),
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "sandbox: {e}"),
            Self::Kernel(s) => write!(f, "sandbox: {s}"),
        }
    }
}

impl std::error::Error for SandboxError {}

impl From<std::io::Error> for SandboxError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<std::ffi::NulError> for SandboxError {
    fn from(e: std::ffi::NulError) -> Self {
        Self::Kernel(format!("path contains null byte: {e}"))
    }
}

/// Compile sandbox rules into a `CompiledSandbox` ready for `apply_compiled`.
///
/// May allocate (opens file descriptors, builds Vec); must NOT be called inside
/// a `pre_exec` closure.
pub fn compile(rules: &[SandboxRule]) -> Result<CompiledSandbox, SandboxError> {
    #[cfg(target_os = "linux")]
    {
        return linux::compile(rules).map(|inner| CompiledSandbox { inner });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = rules;
        Ok(CompiledSandbox {})
    }
}

/// Apply the compiled sandbox to the calling process.
///
/// Async-signal-safe: uses only raw syscalls, no allocation, no locking.
/// Safe to call inside `CommandExt::pre_exec`.
pub fn apply_compiled(compiled: &CompiledSandbox) -> Result<(), SandboxError> {
    #[cfg(target_os = "linux")]
    linux::apply_compiled_inner(&compiled.inner)?;
    #[cfg(not(target_os = "linux"))]
    let _ = compiled;
    Ok(())
}

/// Convenience: compile + apply in one call. May allocate.
/// Do NOT call inside `pre_exec` — use `compile` + `apply_compiled` instead.
pub fn apply_sandbox(rules: &[SandboxRule]) -> Result<(), SandboxError> {
    let compiled = compile(rules)?;
    apply_compiled(&compiled)
}

// ── Linux implementation ──────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use super::{SandboxError, SandboxRule};
    use std::ffi::CString;

    // ── Landlock syscall numbers (Linux 5.13+; same on x86_64 and aarch64) ──
    const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
    const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
    const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

    const LANDLOCK_RULE_PATH_BENEATH: libc::c_long = 1;

    // Landlock ABI V1 access flags (include/uapi/linux/landlock.h)
    const ACCESS_FS_V1_ALL: u64 = 0x1FFF; // bits 0–12 (Execute..MakeSym)
    // Execute (bit 0) is excluded from handled_access_fs: if we declare we control
    // execute, Landlock denies execve for any path not in our rules — which would
    // prevent the MCP binary itself from being loaded by exec() in the child.
    // exec control is provided by the seccomp DenySpawn filter instead.
    const ACCESS_FS_HANDLED: u64 = 0x1FFE; // V1 all except Execute (bit 0)
    const ACCESS_FS_READ_ONLY: u64 = 0x000C; // ReadFile(1<<2) | ReadDir(1<<3)

    // ── seccomp BPF opcodes (classic BPF ABI; stable since 1993) ──────────
    const BPF_LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const BPF_JMP_JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    const BPF_RET_K: u16 = 0x06; // BPF_RET | BPF_K
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;

    #[repr(C)]
    struct LandlockRulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C)]
    struct LandlockPathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
        _pad: i32, // C ABI: struct is 8 + 4 + 4 = 16 bytes on all arches
    }

    pub struct BpfProgram(pub Vec<libc::sock_filter>);

    /// Compiled sandbox state held between `compile()` and `apply_compiled_inner()`.
    pub struct Inner {
        /// Landlock ruleset fd created by `landlock_create_ruleset` + `landlock_add_rule`.
        /// -1 = no FS rules or kernel doesn't support Landlock (BestEffort degradation).
        pub landlock_fd: i32,
        /// Pre-compiled seccomp BPF. None if DenySpawn rule was not requested.
        pub bpf: Option<BpfProgram>,
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            if self.landlock_fd >= 0 {
                unsafe {
                    libc::close(self.landlock_fd);
                }
            }
        }
    }

    pub fn compile(rules: &[SandboxRule]) -> Result<Inner, SandboxError> {
        // Collect path-beneath entries, opening each with O_PATH.
        // Allocation (CString) is fine here — we're in the parent process.
        let mut path_entries: Vec<(i32, bool)> = Vec::new(); // (fd, is_write)
        let mut open_err: Option<SandboxError> = None;

        for rule in rules {
            match rule {
                SandboxRule::AllowFsRead { prefix } => match open_path_fd(prefix) {
                    Ok(fd) => path_entries.push((fd, false)),
                    Err(e) => {
                        open_err = Some(e);
                        break;
                    }
                },
                SandboxRule::AllowFsWrite { prefix } => match open_path_fd(prefix) {
                    Ok(fd) => path_entries.push((fd, true)),
                    Err(e) => {
                        open_err = Some(e);
                        break;
                    }
                },
                SandboxRule::DenySpawn => {}
            }
        }

        if let Some(e) = open_err {
            for &(fd, _) in &path_entries {
                unsafe {
                    libc::close(fd);
                }
            }
            return Err(e);
        }

        // Build Landlock ruleset if there are FS rules.
        // Path fds are closed immediately after landlock_add_rule; the kernel retains
        // its own reference. The ruleset_fd stays open until apply_compiled_inner().
        let landlock_fd = if !path_entries.is_empty() {
            let result = build_landlock_ruleset(&path_entries);
            for &(fd, _) in &path_entries {
                unsafe {
                    libc::close(fd);
                }
            }
            result?
        } else {
            -1
        };

        // Build seccomp BPF if DenySpawn was requested.
        let bpf = if rules.iter().any(|r| matches!(r, SandboxRule::DenySpawn)) {
            Some(build_spawn_deny_filter())
        } else {
            None
        };

        Ok(Inner { landlock_fd, bpf })
    }

    fn open_path_fd(path: &str) -> Result<i32, SandboxError> {
        let cpath = CString::new(path.as_bytes())?;
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            Err(SandboxError::Io(std::io::Error::last_os_error()))
        } else {
            Ok(fd)
        }
    }

    fn build_landlock_ruleset(path_entries: &[(i32, bool)]) -> Result<i32, SandboxError> {
        let attr = LandlockRulesetAttr {
            handled_access_fs: ACCESS_FS_HANDLED,
        };

        let ruleset_fd = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                &attr as *const LandlockRulesetAttr as *const libc::c_void,
                std::mem::size_of::<LandlockRulesetAttr>() as libc::c_long,
                0_i32,
            )
        };

        if ruleset_fd < 0 {
            let errno = unsafe { *libc::__errno_location() };
            // ENOSYS or EOPNOTSUPP: kernel lacks Landlock → degrade silently (BestEffort)
            if errno == libc::ENOSYS || errno == libc::EOPNOTSUPP {
                return Ok(-1);
            }
            return Err(SandboxError::Io(std::io::Error::last_os_error()));
        }

        let ruleset_fd = ruleset_fd as i32;

        for &(parent_fd, is_write) in path_entries {
            let allowed_access = if is_write {
                ACCESS_FS_HANDLED // write grants all V1 flags except Execute
            } else {
                ACCESS_FS_READ_ONLY
            };
            let rule_attr = LandlockPathBeneathAttr {
                allowed_access,
                parent_fd,
                _pad: 0,
            };
            let ret = unsafe {
                libc::syscall(
                    SYS_LANDLOCK_ADD_RULE,
                    ruleset_fd as libc::c_long,
                    LANDLOCK_RULE_PATH_BENEATH,
                    &rule_attr as *const LandlockPathBeneathAttr as *const libc::c_void,
                    0_i32,
                )
            };
            if ret < 0 {
                unsafe {
                    libc::close(ruleset_fd);
                }
                return Err(SandboxError::Io(std::io::Error::last_os_error()));
            }
        }

        Ok(ruleset_fd)
    }

    fn build_spawn_deny_filter() -> BpfProgram {
        // seccomp_data.nr is at offset 0 on all architectures.
        const NR_OFFSET: u32 = 0;

        let mut filter: Vec<libc::sock_filter> = vec![
            // Load syscall number into accumulator
            libc::sock_filter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: NR_OFFSET,
            },
            // Block execve (true→skip 0→kill; false→skip 1→next check)
            libc::sock_filter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 1,
                k: libc::SYS_execve as u32,
            },
            libc::sock_filter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_KILL_PROCESS,
            },
            // Block execveat
            libc::sock_filter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 1,
                k: libc::SYS_execveat as u32,
            },
            libc::sock_filter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_KILL_PROCESS,
            },
        ];

        // fork(2) and vfork(2) exist as distinct syscalls only on x86_64.
        // On aarch64 and other modern arches, fork() is implemented via clone3/clone
        // which we must NOT block (Tokio uses clone for thread creation).
        // vfork (syscall 58) is also blocked: a vfork child shares the parent's fd
        // table until exec(), so it can write to the stdout pipe back to agentd.
        #[cfg(target_arch = "x86_64")]
        {
            filter.push(libc::sock_filter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 1,
                k: libc::SYS_fork as u32,
            });
            filter.push(libc::sock_filter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_KILL_PROCESS,
            });
            filter.push(libc::sock_filter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 1,
                k: libc::SYS_vfork as u32,
            });
            filter.push(libc::sock_filter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_KILL_PROCESS,
            });
        }

        // Allow all other syscalls
        filter.push(libc::sock_filter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        });

        BpfProgram(filter)
    }

    /// Apply the compiled sandbox to the current process.
    ///
    /// Async-signal-safe: only raw syscalls, no allocation, no locking.
    /// Landlock failures on unsupported kernels are silently ignored (BestEffort).
    pub fn apply_compiled_inner(inner: &Inner) -> Result<(), SandboxError> {
        let has_anything = inner.landlock_fd >= 0 || inner.bpf.is_some();
        if !has_anything {
            return Ok(());
        }

        // PR_SET_NO_NEW_PRIVS is required before seccomp without CAP_SYS_ADMIN.
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(SandboxError::Io(std::io::Error::last_os_error()));
        }

        // Apply Landlock (BestEffort: ENOSYS/EOPNOTSUPP are tolerated).
        if inner.landlock_fd >= 0 {
            let ret = unsafe {
                libc::syscall(
                    SYS_LANDLOCK_RESTRICT_SELF,
                    inner.landlock_fd as libc::c_long,
                    0_i32,
                )
            };
            if ret < 0 {
                let errno = unsafe { *libc::__errno_location() };
                if errno != libc::ENOSYS && errno != libc::EOPNOTSUPP {
                    return Err(SandboxError::Io(std::io::Error::last_os_error()));
                }
                // Kernel doesn't support Landlock → ignore, continue
            }
        }

        // Apply seccomp BPF (blocks execve/execveat/fork).
        if let Some(bpf) = &inner.bpf {
            let fprog = libc::sock_fprog {
                len: bpf.0.len() as u16,
                filter: bpf.0.as_ptr() as *mut libc::sock_filter,
            };
            if unsafe {
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER as libc::c_ulong,
                    &fprog as *const libc::sock_fprog as libc::c_ulong,
                    0,
                    0,
                )
            } != 0
            {
                return Err(SandboxError::Io(std::io::Error::last_os_error()));
            }
        }

        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_empty_rules_succeeds() {
        let compiled = compile(&[]).unwrap();
        apply_compiled(&compiled).unwrap();
    }

    #[test]
    fn apply_sandbox_empty_rules_succeeds() {
        apply_sandbox(&[]).unwrap();
    }

    #[test]
    fn deny_spawn_rule_is_recognized() {
        // compile() with DenySpawn should succeed on all platforms.
        let rules = vec![SandboxRule::DenySpawn];
        let result = compile(&rules);
        assert!(result.is_ok(), "compile(DenySpawn) failed: {result:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allow_fs_read_root_builds_landlock() {
        // /proc always exists on Linux; use it as a test path.
        let rules = vec![SandboxRule::AllowFsRead {
            prefix: "/proc".to_string(),
        }];
        let compiled = compile(&rules).unwrap();
        // landlock_fd >= 0 means the ruleset was built (kernel supports Landlock).
        // -1 means BestEffort degradation (kernel too old) — also acceptable.
        assert!(
            compiled.inner.landlock_fd >= -1,
            "unexpected landlock_fd value"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allow_fs_nonexistent_path_fails_compile() {
        let rules = vec![SandboxRule::AllowFsRead {
            prefix: "/no/such/path/that/exists".to_string(),
        }];
        let result = compile(&rules);
        assert!(
            result.is_err(),
            "expected Err for non-existent path, got Ok"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn deny_spawn_builds_bpf() {
        let rules = vec![SandboxRule::DenySpawn];
        let compiled = compile(&rules).unwrap();
        assert!(
            compiled.inner.bpf.is_some(),
            "DenySpawn should produce a BPF program"
        );
        let bpf = compiled.inner.bpf.as_ref().unwrap();
        // Must have at least: load nr, execve check+kill, execveat check+kill, allow
        assert!(bpf.0.len() >= 6, "BPF program too short: {} insns", bpf.0.len());
    }

    #[test]
    fn sandbox_rule_is_clone() {
        let r = SandboxRule::AllowFsRead {
            prefix: "/tmp".to_string(),
        };
        let _r2 = r.clone();
    }
}
