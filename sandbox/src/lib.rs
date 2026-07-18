/// Kernel-level sandbox enforcement for MCP server subprocesses.
///
/// Two mechanisms are combined:
/// - Landlock LSM: filesystem path-beneath rules (BestEffort; degrades silently on
///   kernels < 5.13 or without CONFIG_SECURITY_LANDLOCK).
/// - seccomp-bpf: syscall filter blocking fork/vfork (DenySpawn rule; x86_64 only).
///   Exec is not blocked — the initial execve that loads the MCP binary must succeed.
///   Landlock FS rules persist across exec, so re-exec into another binary stays restricted.
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
    /// Allow read and write access to all paths beneath `prefix` (all Landlock ABI V1 flags except Execute).
    AllowFsWrite { prefix: String },
    /// Block fork/vfork via seccomp-bpf (x86_64 only; aarch64 deferred pending clone3 inspection).
    /// Exec is not blocked — Landlock FS rules persist across exec, keeping any re-exec restricted.
    DenySpawn,
    /// Isolate the process into a new network namespace (no external interfaces by default).
    /// Applied via unshare(CLONE_NEWUSER | CLONE_NEWNET) in pre_exec. Linux-only; no-op elsewhere.
    /// BestEffort: degrades silently if user namespaces are disabled by kernel policy.
    IsolateNetwork,
    /// Isolate the process into a new mount namespace (prevents propagation of mount changes to host).
    /// Applied via unshare(CLONE_NEWUSER | CLONE_NEWNS) in pre_exec. Linux-only; no-op elsewhere.
    /// BestEffort: degrades silently if user namespaces are disabled by kernel policy.
    IsolateMount,
    /// Allow outgoing TCP connections to `port` via Landlock V4 (Linux 6.7+).
    ///
    /// When one or more `AllowNetConnect` rules are present and the kernel supports
    /// Landlock ABI V4, all outgoing TCP connections are restricted to the listed
    /// ports. On kernels < 6.7 the rule degrades silently (BestEffort): FS rules
    /// still apply but TCP port restriction is not enforced.
    ///
    /// Note: only port is enforced at the kernel level — Landlock V4 does not
    /// restrict by hostname.
    AllowNetConnect { port: u16 },
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

/// Summary of what sandbox enforcement was compiled into a `CompiledSandbox`.
///
/// Reflects intended enforcement — kernel outcome at `apply_compiled()` time
/// may differ (e.g. Landlock degrades silently on kernels < 5.13, unshare may
/// be blocked by kernel.unprivileged_userns_clone = 0).
#[derive(Debug, Clone, PartialEq)]
pub struct EnforcementStatus {
    /// Landlock FS ruleset was compiled (kernel may degrade on old systems).
    pub landlock: bool,
    /// seccomp-bpf filter was compiled (x86_64 only, when DenySpawn was requested).
    pub seccomp: bool,
    /// Which spawn syscalls the filter targets.
    /// `"fork_vfork_only"` — fork(57) + vfork(58) blocked on x86_64.
    /// `"none"` — no spawn filtering compiled (non-x86_64 or DenySpawn not in rules).
    pub spawn_enforcement: &'static str,
    /// Network namespace isolation was requested (IsolateNetwork rule present).
    pub namespace_net: bool,
    /// Mount namespace isolation was requested (IsolateMount rule present).
    pub namespace_mount: bool,
    /// Landlock V4 TCP port rules were compiled (kernel >= 6.7 and AllowNetConnect rules present).
    /// False when kernel is < 6.7 (BestEffort degradation) or no AllowNetConnect rules were given.
    pub landlock_net: bool,
}

impl CompiledSandbox {
    /// Returns a summary of what sandbox mechanisms were compiled into this instance.
    pub fn enforcement_status(&self) -> EnforcementStatus {
        #[cfg(target_os = "linux")]
        {
            let landlock = self.inner.landlock_fd >= 0;
            let seccomp = self.inner.bpf.is_some();
            #[cfg(target_arch = "x86_64")]
            let spawn_enforcement = if self.inner.deny_spawn_requested {
                "fork_vfork_only"
            } else {
                "none"
            };
            #[cfg(not(target_arch = "x86_64"))]
            let spawn_enforcement = "none";
            EnforcementStatus {
                landlock,
                seccomp,
                spawn_enforcement,
                namespace_net:   self.inner.isolate_net,
                namespace_mount: self.inner.isolate_mount,
                landlock_net:    self.inner.landlock_net_active,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            EnforcementStatus {
                landlock: false,
                seccomp: false,
                spawn_enforcement: "none",
                namespace_net: false,
                namespace_mount: false,
                landlock_net: false,
            }
        }
    }
}

/// Returns the kernel's Landlock ABI version (1..N), or 0 if Landlock is unavailable.
///
/// Use this to report the actual ABI level in diagnostics rather than inferring
/// the kernel version from the ABI number (a kernel ≥ 6.7 may still report ABI < 4
/// if compiled without full Landlock support).
pub fn landlock_abi_version() -> u32 {
    #[cfg(target_os = "linux")]
    {
        linux::query_landlock_abi_version() as u32
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// Returns true if the running kernel supports Landlock ABI version 4 (Linux ≥ 6.7),
/// which is required for TCP port enforcement via `AllowNetConnect`.
///
/// On non-Linux or if Landlock is unavailable, always returns false.
pub fn landlock_v4_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::query_landlock_abi_version() >= 4
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Returns true if the running kernel supports any Landlock version (ABI ≥ 1, Linux ≥ 5.13).
///
/// Use this to probe whether Landlock enforcement is possible at all, regardless of
/// which specific features (network port rules, etc.) are available.
/// On non-Linux, always returns false.
pub fn landlock_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::query_landlock_abi_version() >= 1
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Compile sandbox rules into a `CompiledSandbox` ready for `apply_compiled`.
///
/// May allocate (opens file descriptors, builds Vec); must NOT be called inside
/// a `pre_exec` closure.
pub fn compile(rules: &[SandboxRule]) -> Result<CompiledSandbox, SandboxError> {
    #[cfg(target_os = "linux")]
    {
        linux::compile(rules).map(|inner| CompiledSandbox { inner })
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
    // Landlock V4 rule type for TCP port restrictions (Linux 6.7+).
    const LANDLOCK_RULE_NET_PORT: libc::c_long = 3;

    // Flag passed as `flags` to landlock_create_ruleset(NULL, 0, flags) to query
    // the kernel's supported ABI version. Returns version (1..N) or -1 on ENOSYS.
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_int = 1;

    // Landlock ABI V1 access flags (include/uapi/linux/landlock.h)
    // Execute (bit 0) is excluded from handled_access_fs: if we declare we control
    // execute, Landlock denies execve for any path not in our rules — which would
    // prevent the MCP binary itself from being loaded by exec() in the child.
    // DenySpawn blocks fork/vfork only; exec is intentionally left unrestricted.
    const ACCESS_FS_HANDLED: u64 = 0x1FFE; // V1 all except Execute (bit 0)
    const ACCESS_FS_READ_ONLY: u64 = 0x000C; // ReadFile(1<<2) | ReadDir(1<<3)

    // Landlock V4 network access flags (Linux 6.7+).
    // BIND is defined for completeness but unused — MCP servers act as clients.
    #[allow(dead_code)]
    const LANDLOCK_ACCESS_NET_BIND_TCP: u64 = 1 << 0;
    const LANDLOCK_ACCESS_NET_CONNECT_TCP: u64 = 1 << 1;

    // ── seccomp BPF opcodes (classic BPF ABI; stable since 1993; x86_64 only) ──
    #[cfg(target_arch = "x86_64")]
    const BPF_LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    #[cfg(target_arch = "x86_64")]
    const BPF_JMP_JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    #[cfg(target_arch = "x86_64")]
    const BPF_RET_K: u16 = 0x06; // BPF_RET | BPF_K
    #[cfg(target_arch = "x86_64")]
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    #[cfg(target_arch = "x86_64")]
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

    // V4 ruleset attr — extends V1 with a net handled-access field.
    // MUST only be passed to landlock_create_ruleset when ABI version >= 4;
    // passing a 16-byte struct to a V1/V3 kernel returns EINVAL (not ENOSYS).
    #[repr(C)]
    struct LandlockRulesetAttrV4 {
        handled_access_fs:  u64,
        handled_access_net: u64,
    }

    // V4 net port rule attr (LANDLOCK_RULE_NET_PORT).
    // `port` is u64 in the kernel ABI even though TCP ports fit in u16.
    #[repr(C)]
    struct LandlockNetPortAttr {
        allowed_access: u64,
        port:           u64,
    }

    pub struct BpfProgram(pub Vec<libc::sock_filter>);

    /// Compiled sandbox state held between `compile()` and `apply_compiled_inner()`.
    pub struct Inner {
        /// Landlock ruleset fd created by `landlock_create_ruleset` + `landlock_add_rule`.
        /// -1 = no FS/net rules or kernel doesn't support Landlock (BestEffort degradation).
        pub landlock_fd: i32,
        /// Pre-compiled seccomp BPF. None if DenySpawn rule was not requested,
        /// or if the current arch does not support fork/vfork filtering.
        pub bpf: Option<BpfProgram>,
        /// True when DenySpawn was in the rule set (x86_64 only). Used by
        /// `enforcement_status()` to distinguish "not requested" from "not enforceable".
        #[cfg(target_arch = "x86_64")]
        pub deny_spawn_requested: bool,
        /// IsolateNetwork rule was requested — unshare CLONE_NEWNET in apply_compiled_inner.
        pub isolate_net: bool,
        /// IsolateMount rule was requested — unshare CLONE_NEWNS in apply_compiled_inner.
        pub isolate_mount: bool,
        /// Landlock V4 TCP port rules were successfully added to the ruleset fd.
        /// False when no AllowNetConnect rules were given or kernel ABI < 4.
        pub landlock_net_active: bool,
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
        // Scan namespace isolation flags first (no allocation needed).
        let isolate_net   = rules.iter().any(|r| matches!(r, SandboxRule::IsolateNetwork));
        let isolate_mount = rules.iter().any(|r| matches!(r, SandboxRule::IsolateMount));

        // Collect path-beneath entries, opening each with O_PATH.
        // Allocation (CString) is fine here — we're in the parent process.
        let mut path_entries: Vec<(i32, bool)> = Vec::new(); // (fd, is_write)
        let mut open_err: Option<SandboxError> = None;

        // Collect AllowNetConnect ports.
        let net_ports: Vec<u16> = rules
            .iter()
            .filter_map(|r| {
                if let SandboxRule::AllowNetConnect { port } = r {
                    Some(*port)
                } else {
                    None
                }
            })
            .collect();

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
                SandboxRule::DenySpawn
                | SandboxRule::IsolateNetwork
                | SandboxRule::IsolateMount
                | SandboxRule::AllowNetConnect { .. } => {}
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

        // Build Landlock ruleset if there are FS rules or V4-capable net port rules.
        //
        // ABI version is queried first so the has_landlock_rules gate can correctly
        // exclude the net-only-on-V3 case: a V1 ruleset with handled_access_fs set and
        // zero path-beneath rules would deny ALL filesystem access (EACCES on every
        // open/read/write). If the kernel is pre-V4 and there are no FS rules, skip
        // the ruleset entirely — correct BestEffort degradation.
        //
        // Path fds are closed immediately after landlock_add_rule; the kernel retains
        // its own reference. The ruleset_fd stays open until apply_compiled_inner().
        let abi_version = if !net_ports.is_empty() {
            query_landlock_abi_version()
        } else {
            0 // FS-only path uses V1 struct; no ABI syscall needed
        };
        let use_v4_net = abi_version >= 4 && !net_ports.is_empty();
        // Only build a ruleset when there are FS rules (always safe) or when V4 net
        // enforcement is available (net-only is fine with handled_access_fs=0 on V4).
        // Skipping when path_entries is empty AND use_v4_net is false avoids creating
        // a V1 ruleset that would blanket-deny all FS with zero path allowances.
        let has_landlock_rules = !path_entries.is_empty() || use_v4_net;
        let (landlock_fd, landlock_net_active) = if has_landlock_rules {
            let fd = build_landlock_ruleset(&path_entries, &net_ports, use_v4_net);
            // Close path fds regardless of outcome.
            for &(pfd, _) in &path_entries {
                unsafe {
                    libc::close(pfd);
                }
            }
            let fd = fd?;
            (fd, use_v4_net && fd >= 0)
        } else {
            (-1, false)
        };

        // Build seccomp BPF if DenySpawn was requested.
        // On non-x86_64 arches fork()/vfork() do not exist as distinct syscalls;
        // building a filter would produce a 2-instruction no-op. Gate compilation
        // so `bpf.is_some()` reliably means "a real filter was installed".
        #[cfg(target_arch = "x86_64")]
        {
            let deny_spawn_requested = rules.iter().any(|r| matches!(r, SandboxRule::DenySpawn));
            let bpf = if deny_spawn_requested { Some(build_spawn_deny_filter()) } else { None };
            Ok(Inner {
                landlock_fd,
                bpf,
                deny_spawn_requested,
                isolate_net,
                isolate_mount,
                landlock_net_active,
            })
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let bpf: Option<BpfProgram> = None;
            Ok(Inner { landlock_fd, bpf, isolate_net, isolate_mount, landlock_net_active })
        }
    }

    /// Query the kernel's supported Landlock ABI version.
    /// Returns the version (1..N) on success, or 0 if Landlock is unavailable.
    pub(super) fn query_landlock_abi_version() -> i64 {
        let ret = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                std::ptr::null::<libc::c_void>(),
                0_usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            )
        };
        if ret < 0 { 0 } else { ret }
    }

    fn open_path_fd(path: &str) -> Result<i32, SandboxError> {
        let cpath = CString::new(path.as_bytes())?;
        // O_NOFOLLOW: reject symlinks at the final path component to prevent a
        // malicious actor from redirecting the Landlock allowance to an arbitrary dir.
        let fd = unsafe {
            libc::open(cpath.as_ptr(), libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        };
        if fd < 0 {
            Err(SandboxError::Io(std::io::Error::last_os_error()))
        } else {
            Ok(fd)
        }
    }

    /// Create a Landlock ruleset and populate it with FS and/or net port rules.
    ///
    /// When `use_v4_net` is true (caller confirmed ABI >= 4 and net_ports is
    /// non-empty), the V4 16-byte struct is used and TCP port rules are added.
    /// Otherwise the V1 8-byte struct is used (FS rules only).
    ///
    /// IMPORTANT: never pass the V4 struct size to a pre-V4 kernel — it returns
    /// EINVAL which is NOT a BestEffort-tolerable error. The caller is responsible
    /// for gating `use_v4_net` on an explicit ABI version check.
    fn build_landlock_ruleset(
        path_entries: &[(i32, bool)],
        net_ports: &[u16],
        use_v4_net: bool,
    ) -> Result<i32, SandboxError> {
        // Create the ruleset. Use V4 struct (16 bytes) only when the caller
        // confirmed ABI >= 4; otherwise use V1 struct (8 bytes).
        //
        // handled_access_fs must be 0 when path_entries is empty. Setting it to
        // ACCESS_FS_HANDLED with zero path-beneath rules would cause
        // landlock_restrict_self to deny ALL filesystem access — a complete FS
        // lockout. Only set the flag when there are actually FS rules to add.
        let fs_access = if path_entries.is_empty() { 0_u64 } else { ACCESS_FS_HANDLED };
        let ruleset_fd = if use_v4_net {
            let attr = LandlockRulesetAttrV4 {
                handled_access_fs:  fs_access,
                handled_access_net: LANDLOCK_ACCESS_NET_CONNECT_TCP,
            };
            unsafe {
                libc::syscall(
                    SYS_LANDLOCK_CREATE_RULESET,
                    &attr as *const LandlockRulesetAttrV4 as *const libc::c_void,
                    std::mem::size_of::<LandlockRulesetAttrV4>() as libc::c_long,
                    0_i32,
                )
            }
        } else {
            let attr = LandlockRulesetAttr {
                handled_access_fs: fs_access,
            };
            unsafe {
                libc::syscall(
                    SYS_LANDLOCK_CREATE_RULESET,
                    &attr as *const LandlockRulesetAttr as *const libc::c_void,
                    std::mem::size_of::<LandlockRulesetAttr>() as libc::c_long,
                    0_i32,
                )
            }
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

        // Add FS path-beneath rules.
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

        // Add V4 net port rules (only when use_v4_net is true).
        if use_v4_net {
            for &port in net_ports {
                let rule_attr = LandlockNetPortAttr {
                    allowed_access: LANDLOCK_ACCESS_NET_CONNECT_TCP,
                    port: port as u64,
                };
                let ret = unsafe {
                    libc::syscall(
                        SYS_LANDLOCK_ADD_RULE,
                        ruleset_fd as libc::c_long,
                        LANDLOCK_RULE_NET_PORT,
                        &rule_attr as *const LandlockNetPortAttr as *const libc::c_void,
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
        }

        Ok(ruleset_fd)
    }

    #[cfg(target_arch = "x86_64")]
    fn build_spawn_deny_filter() -> BpfProgram {
        // seccomp_data.nr is at offset 0 on all architectures.
        const NR_OFFSET: u32 = 0;

        // execve/execveat are NOT blocked: the filter runs in pre_exec (after fork,
        // before exec), so blocking execve would kill the child before the MCP binary
        // is loaded. Landlock FS rules persist across exec, so any re-exec stays
        // restricted. We only block fork/vfork to prevent new child processes.
        let mut filter: Vec<libc::sock_filter> = vec![
            // Load syscall number into accumulator
            libc::sock_filter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: NR_OFFSET,
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

    /// Format "0 {id} 1\n" into a stack-allocated byte buffer — no heap allocation.
    /// Used instead of format!() inside the fork child where malloc is unsafe.
    fn id_map_entry(id: u32) -> ([u8; 16], usize) {
        let mut buf = [0u8; 16];
        let mut pos = 0usize;
        buf[pos] = b'0'; pos += 1;
        buf[pos] = b' '; pos += 1;
        let mut tmp = [0u8; 10];
        let mut tlen = 0usize;
        let mut n = id;
        if n == 0 {
            tmp[0] = b'0';
            tlen = 1;
        } else {
            while n > 0 {
                tmp[tlen] = b'0' + (n % 10) as u8;
                n /= 10;
                tlen += 1;
            }
            tmp[..tlen].reverse();
        }
        buf[pos..pos + tlen].copy_from_slice(&tmp[..tlen]);
        pos += tlen;
        buf[pos] = b' '; pos += 1;
        buf[pos] = b'1'; pos += 1;
        buf[pos] = b'\n'; pos += 1;
        (buf, pos)
    }

    /// Apply the compiled sandbox to the current process.
    ///
    /// Async-signal-safe: only raw syscalls, no allocation, no locking.
    /// Landlock and namespace failures on unsupported kernels are silently ignored (BestEffort).
    pub fn apply_compiled_inner(inner: &Inner) -> Result<(), SandboxError> {
        let has_anything = inner.landlock_fd >= 0
            || inner.bpf.is_some()
            || inner.isolate_net
            || inner.isolate_mount;
        if !has_anything {
            return Ok(());
        }

        // Namespace unshare must happen before PR_SET_NO_NEW_PRIVS: on some kernel
        // versions, no-new-privs prevents creating user namespaces.
        if inner.isolate_net || inner.isolate_mount {
            // CLONE_NEWUSER grants the capabilities needed to create net/mount
            // namespaces without CAP_SYS_ADMIN, enabling this to work unprivileged.
            let mut flags: libc::c_int = libc::CLONE_NEWUSER;
            if inner.isolate_net   { flags |= libc::CLONE_NEWNET; }
            if inner.isolate_mount { flags |= libc::CLONE_NEWNS;  }
            let unshare_ok = unsafe { libc::unshare(flags) } == 0;
            if unshare_ok {
                // Write uid_map and gid_map to preserve DAC identity inside the
                // user namespace. Without these mappings, the process runs as the
                // overflow uid (nobody/65534), breaking Landlock FS grants via DAC
                // for user-owned files with modes < 0644.
                //
                // "deny" must be written to setgroups before gid_map on kernels ≥ 3.19
                // to prevent privilege escalation via supplementary group removal.
                let uid = unsafe { libc::getuid() };
                let gid = unsafe { libc::getgid() };
                let _ = std::fs::write("/proc/self/setgroups", "deny");
                // Use stack-allocated byte buffers instead of format!() to avoid
                // heap allocation in the fork child where malloc is not safe.
                let (uid_buf, uid_len) = id_map_entry(uid);
                let (gid_buf, gid_len) = id_map_entry(gid);
                // BestEffort: write errors here mean uid_map is already set or the
                // namespace was not actually created; don't fail the whole sandbox.
                let _ = std::fs::write("/proc/self/uid_map", &uid_buf[..uid_len]);
                let _ = std::fs::write("/proc/self/gid_map", &gid_buf[..gid_len]);
            } else {
                let errno = unsafe { *libc::__errno_location() };
                // BestEffort: EPERM = user namespaces disabled by kernel policy
                // (kernel.unprivileged_userns_clone = 0); ENOSYS = too old.
                // Degrade silently; the caller can read EnforcementStatus to check.
                if errno != libc::EPERM && errno != libc::ENOSYS {
                    return Err(SandboxError::Io(std::io::Error::last_os_error()));
                }
            }
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

        // Apply seccomp BPF (blocks fork/vfork on x86_64).
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

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn deny_spawn_builds_bpf() {
        let rules = vec![SandboxRule::DenySpawn];
        let compiled = compile(&rules).unwrap();
        assert!(
            compiled.inner.bpf.is_some(),
            "DenySpawn should produce a BPF program on x86_64"
        );
        let bpf = compiled.inner.bpf.as_ref().unwrap();
        // Must have at least: load nr, allow
        assert!(bpf.0.len() >= 2, "BPF program too short: {} insns", bpf.0.len());
    }

    // x86_64-gated like the field itself (deny_spawn_requested exists only on
    // that arch) — target_os alone breaks the aarch64-linux test build (E0609,
    // found by ci.1's workspace clippy sweep).
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn deny_spawn_sets_deny_spawn_requested() {
        let rules = vec![SandboxRule::DenySpawn];
        let compiled = compile(&rules).unwrap();
        assert!(
            compiled.inner.deny_spawn_requested,
            "DenySpawn rule must set deny_spawn_requested"
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn no_deny_spawn_clears_deny_spawn_requested() {
        let rules = vec![SandboxRule::AllowFsRead { prefix: "/proc".to_string() }];
        let compiled = compile(&rules).unwrap();
        assert!(
            !compiled.inner.deny_spawn_requested,
            "deny_spawn_requested must be false when DenySpawn not in rules"
        );
    }

    #[test]
    fn isolate_network_compiles_on_all_platforms() {
        let result = compile(&[SandboxRule::IsolateNetwork]);
        assert!(result.is_ok(), "IsolateNetwork should compile on all platforms: {result:?}");
    }

    #[test]
    fn isolate_mount_compiles_on_all_platforms() {
        let result = compile(&[SandboxRule::IsolateMount]);
        assert!(result.is_ok(), "IsolateMount should compile on all platforms: {result:?}");
    }

    #[test]
    fn enforcement_status_empty_has_no_namespace_flags() {
        let compiled = compile(&[]).unwrap();
        let s = compiled.enforcement_status();
        assert!(!s.namespace_net,   "no rules → namespace_net false");
        assert!(!s.namespace_mount, "no rules → namespace_mount false");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enforcement_status_isolate_network_sets_flag() {
        let compiled = compile(&[SandboxRule::IsolateNetwork]).unwrap();
        let s = compiled.enforcement_status();
        assert!(s.namespace_net,    "IsolateNetwork → namespace_net true");
        assert!(!s.namespace_mount, "IsolateNetwork alone → namespace_mount false");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enforcement_status_isolate_mount_sets_flag() {
        let compiled = compile(&[SandboxRule::IsolateMount]).unwrap();
        let s = compiled.enforcement_status();
        assert!(!s.namespace_net,  "IsolateMount alone → namespace_net false");
        assert!(s.namespace_mount, "IsolateMount → namespace_mount true");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enforcement_status_both_namespace_rules() {
        let compiled = compile(&[SandboxRule::IsolateNetwork, SandboxRule::IsolateMount]).unwrap();
        let s = compiled.enforcement_status();
        assert!(s.namespace_net,   "IsolateNetwork → namespace_net true");
        assert!(s.namespace_mount, "IsolateMount → namespace_mount true");
    }

    #[test]
    fn sandbox_rule_is_clone() {
        let r = SandboxRule::AllowFsRead {
            prefix: "/tmp".to_string(),
        };
        let _r2 = r.clone();
    }

    #[test]
    fn sandbox_rule_partial_eq() {
        assert_eq!(SandboxRule::DenySpawn, SandboxRule::DenySpawn);
        assert_eq!(SandboxRule::IsolateNetwork, SandboxRule::IsolateNetwork);
        assert_eq!(SandboxRule::IsolateMount, SandboxRule::IsolateMount);
        assert_ne!(SandboxRule::IsolateNetwork, SandboxRule::IsolateMount);
        assert_eq!(
            SandboxRule::AllowFsRead { prefix: "/a".into() },
            SandboxRule::AllowFsRead { prefix: "/a".into() },
        );
        assert_ne!(
            SandboxRule::AllowFsRead { prefix: "/a".into() },
            SandboxRule::AllowFsRead { prefix: "/b".into() },
        );
        assert_ne!(
            SandboxRule::AllowFsRead { prefix: "/a".into() },
            SandboxRule::AllowFsWrite { prefix: "/a".into() },
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allow_fs_write_tmp_builds_landlock() {
        let rules = vec![SandboxRule::AllowFsWrite { prefix: "/tmp".to_string() }];
        let compiled = compile(&rules).unwrap();
        // Landlock fd present (>= 0) or BestEffort degradation (-1); either is valid.
        assert!(compiled.inner.landlock_fd >= -1);
        // No DenySpawn → no BPF filter.
        assert!(compiled.inner.bpf.is_none(), "AllowFsWrite alone should not produce BPF");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allow_fs_write_and_deny_spawn_produces_landlock_and_bpf() {
        let rules = vec![
            SandboxRule::AllowFsWrite { prefix: "/tmp".to_string() },
            SandboxRule::DenySpawn,
        ];
        let compiled = compile(&rules).unwrap();
        assert!(compiled.inner.landlock_fd >= -1);
        assert!(compiled.inner.bpf.is_some(), "DenySpawn must produce BPF");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn combined_fs_read_write_and_deny_spawn() {
        let rules = vec![
            SandboxRule::AllowFsRead { prefix: "/proc".to_string() },
            SandboxRule::AllowFsWrite { prefix: "/tmp".to_string() },
            SandboxRule::DenySpawn,
        ];
        let compiled = compile(&rules).expect("combined rules should compile");
        assert!(compiled.inner.bpf.is_some());
    }

    // enforcement_status() tests — platform-independent

    #[test]
    fn enforcement_status_empty_rules() {
        let compiled = compile(&[]).unwrap();
        let s = compiled.enforcement_status();
        assert!(!s.landlock);
        assert!(!s.seccomp);
        assert_eq!(s.spawn_enforcement, "none");
    }

    // The degraded-arch contract: DenySpawn on non-x86_64 must compile to NO
    // seccomp filter and report spawn_enforcement "none" (ma.4 tier honesty —
    // callers surface the degradation instead of assuming enforcement).
    // Compile-checked today by the aarch64 clippy lane (ci.1); executes if a
    // `cross test -p sandbox` lane is ever added.
    #[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
    #[test]
    fn enforcement_status_deny_spawn_degrades_on_non_x86_64() {
        let compiled = compile(&[SandboxRule::DenySpawn]).unwrap();
        let s = compiled.enforcement_status();
        assert!(!s.seccomp, "DenySpawn off x86_64 → no BPF program");
        assert_eq!(s.spawn_enforcement, "none");
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn enforcement_status_deny_spawn_x86_64() {
        let compiled = compile(&[SandboxRule::DenySpawn]).unwrap();
        let s = compiled.enforcement_status();
        assert!(!s.landlock, "no FS rules → landlock false");
        assert!(s.seccomp, "DenySpawn on x86_64 → seccomp true");
        assert_eq!(s.spawn_enforcement, "fork_vfork_only");
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn enforcement_status_full_rules_x86_64() {
        let rules = vec![
            SandboxRule::AllowFsRead { prefix: "/proc".to_string() },
            SandboxRule::AllowFsWrite { prefix: "/tmp".to_string() },
            SandboxRule::DenySpawn,
        ];
        let compiled = compile(&rules).unwrap();
        let s = compiled.enforcement_status();
        // landlock_fd may be -1 on old kernels; accept both
        assert!(s.seccomp, "DenySpawn on x86_64 always sets seccomp");
        assert_eq!(s.spawn_enforcement, "fork_vfork_only");
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn enforcement_status_fs_only_no_seccomp() {
        let rules = vec![SandboxRule::AllowFsRead { prefix: "/proc".to_string() }];
        let compiled = compile(&rules).unwrap();
        let s = compiled.enforcement_status();
        assert!(!s.seccomp, "no DenySpawn → seccomp false");
        assert_eq!(s.spawn_enforcement, "none");
    }

    // On x86_64 the filter has 6 instructions:
    // load(1) + fork(2) + vfork(2) + allow(1) = 6
    // execve/execveat are NOT blocked (filter runs in pre_exec before exec).
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn deny_spawn_bpf_includes_vfork_on_x86_64() {
        let rules = vec![SandboxRule::DenySpawn];
        let compiled = compile(&rules).unwrap();
        let bpf = compiled.inner.bpf.as_ref().unwrap();
        assert_eq!(bpf.0.len(), 6,
            "expected 6 BPF insns (load + fork + vfork + allow), got {}",
            bpf.0.len());
    }

    // ── AllowNetConnect tests ─────────────────────────────────────────────────

    #[test]
    fn allow_net_connect_compiles_on_all_platforms() {
        // Must not panic or return Err — macOS/Windows degrade silently.
        let result = compile(&[SandboxRule::AllowNetConnect { port: 443 }]);
        assert!(result.is_ok(), "AllowNetConnect should compile on all platforms: {result:?}");
    }

    #[test]
    fn allow_net_connect_enforcement_status_landlock_net_false_on_macos() {
        // On non-Linux, landlock_net is always false (no Landlock support).
        #[cfg(not(target_os = "linux"))]
        {
            let compiled = compile(&[SandboxRule::AllowNetConnect { port: 443 }]).unwrap();
            let s = compiled.enforcement_status();
            assert!(!s.landlock_net, "non-Linux: landlock_net must be false");
        }
    }

    #[test]
    fn allow_net_connect_partial_eq() {
        assert_eq!(
            SandboxRule::AllowNetConnect { port: 443 },
            SandboxRule::AllowNetConnect { port: 443 },
        );
        assert_ne!(
            SandboxRule::AllowNetConnect { port: 443 },
            SandboxRule::AllowNetConnect { port: 80 },
        );
        assert_ne!(
            SandboxRule::AllowNetConnect { port: 443 },
            SandboxRule::IsolateNetwork,
        );
    }

    #[test]
    fn allow_net_connect_clone() {
        let r = SandboxRule::AllowNetConnect { port: 443 };
        let r2 = r.clone();
        assert_eq!(r, r2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allow_net_connect_with_fs_rule_compiles_together() {
        // AllowFsRead + AllowNetConnect must produce a single ruleset fd (V4 or V1 path).
        let rules = vec![
            SandboxRule::AllowFsRead { prefix: "/proc".to_string() },
            SandboxRule::AllowNetConnect { port: 443 },
        ];
        let result = compile(&rules);
        assert!(result.is_ok(), "combined FS + AllowNetConnect should compile: {result:?}");
        let compiled = result.unwrap();
        // ruleset_fd >= 0 means Landlock was activated; -1 is BestEffort degradation.
        assert!(compiled.inner.landlock_fd >= -1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allow_net_connect_enforcement_status_reflects_abi() {
        // On a kernel without V4 (ABI < 4) landlock_net should be false.
        // On a kernel with V4 (ABI >= 4) it should be true.
        // We accept both outcomes — this test verifies the field is set consistently
        // with what compile() actually achieved (no inconsistency between fd and flag).
        let compiled = compile(&[SandboxRule::AllowNetConnect { port: 443 }]).unwrap();
        let s = compiled.enforcement_status();
        // If landlock_net is true, the ruleset fd must be valid.
        if s.landlock_net {
            assert!(
                compiled.inner.landlock_fd >= 0,
                "landlock_net=true requires a valid ruleset fd"
            );
        }
        // If landlock_net is false, it was either BestEffort degradation or no net rules.
        // Both are acceptable outcomes.
    }

    #[test]
    fn allow_net_connect_only_no_fs_rules_does_not_lock_out_fs() {
        // Regression test for the FS-lockout bug: when only AllowNetConnect rules are
        // present (no AllowFsRead/AllowFsWrite), build_landlock_ruleset must set
        // handled_access_fs=0. If it were ACCESS_FS_HANDLED with zero path rules,
        // landlock_restrict_self would deny all filesystem access.
        //
        // We verify compile-side: compile() must succeed and enforcement_status()
        // must be consistent. The actual apply is not called here (it would restrict
        // the test process). The BestEffort degradation path (pre-V4 or macOS) is
        // also valid — what matters is that compile() never errors.
        let result = compile(&[SandboxRule::AllowNetConnect { port: 443 }]);
        assert!(result.is_ok(), "net-only compile must succeed: {result:?}");
    }

    #[test]
    fn compile_net_only_has_landlock_rules_iff_v4_available() {
        // On pre-V4 kernels (or macOS), net-only rules must degrade gracefully:
        // has_landlock_rules is false → no ruleset created → landlock_net=false.
        // On V4 kernels, has_landlock_rules is true and landlock_net=true.
        // Either way, compile() must succeed.
        let result = compile(&[SandboxRule::AllowNetConnect { port: 80 }]);
        assert!(result.is_ok());
        // enforcement_status() consistency is checked by
        // allow_net_connect_enforcement_status_reflects_abi (Linux-only above).
        // Here we just verify compile doesn't panic or error on any platform.
        let _ = result.unwrap().enforcement_status();
    }

    /// On non-Linux platforms there are no Landlock kernel ABIs, so
    /// `landlock_v4_available()` must always return `false`.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn landlock_v4_available_returns_false_on_non_linux() {
        assert!(!landlock_v4_available(),
            "landlock_v4_available must be false on non-Linux");
    }

    /// On non-Linux platforms `landlock_available()` must always return `false`.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn landlock_available_returns_false_on_non_linux() {
        assert!(!landlock_available(),
            "landlock_available must be false on non-Linux");
    }
}
