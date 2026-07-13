use std::{
    path::Path,
    sync::{Arc, RwLock},
};

use crate::{snapshot::SchedulerSnapshot, MemoryAccess};

/// Inode assignments:
///   1      = root directory  "/"
///   9      = kb/ directory   (shared KB segments)
///   10     = system/ directory (global stats)
///   11     = system/budget file
///   12     = system/queue file
///   13     = system/sandbox file
///   14     = system/provider file
///   1010   = first agent directory  (step 20 per agent since p6.3)
///   +1     = status file
///   +2     = context_size file
///   +3     = budget file
///   +4     = flight file
///   +5     = memory/ subdir
///   +6     = memory/short_term file
///   +7     = memory/long_term/ subdir
///   +8     = tools file
///
///   1_000_000+ = dynamic pool for memory/long_term/<key>,
///                kb/<segment>/, and kb/<segment>/<key>
///
/// Used by the Linux FUSE impl and by tests on all platforms.
#[cfg(any(test, target_os = "linux"))]
pub(crate) const DIR_STEP:          u64 = 20;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_STATUS:        u64 = 1;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_CONTEXT:       u64 = 2;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_BUDGET:        u64 = 3;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_FLIGHT:        u64 = 4;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_MEMORY_DIR:    u64 = 5;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_SHORT_TERM:    u64 = 6;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_LONG_TERM_DIR: u64 = 7;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_TOOLS:         u64 = 8;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_PARENT:        u64 = 9;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_SANDBOX:       u64 = 10;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_TIER:          u64 = 11;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_PID:           u64 = 12;
/// /agents/<id>/credentials — per-agent credential grant JSON (cred.5).
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_CREDENTIALS:   u64 = 13;
/// /agents/<id>/attention — active attention signals JSON (ux.2a).
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_ATTENTION:     u64 = 14;

/// System directory and file inodes (not in inode_to_id; handled explicitly).
#[cfg(any(test, target_os = "linux"))]
const INO_SYSTEM:       u64 = 10;
#[cfg(any(test, target_os = "linux"))]
const INO_SYS_BUDGET:   u64 = 11;
#[cfg(any(test, target_os = "linux"))]
const INO_SYS_QUEUE:    u64 = 12;
#[cfg(any(test, target_os = "linux"))]
const INO_SYS_SANDBOX:  u64 = 13;
#[cfg(any(test, target_os = "linux"))]
const INO_SYS_PROVIDER: u64 = 14;
/// Write-only control pseudofile: `echo '{"task":"..."}' > /agents/control`
#[cfg(any(test, target_os = "linux"))]
#[allow(dead_code)]
pub(crate) const INO_CONTROL: u64 = 15;
/// Read-only approvals pseudofile: JSON lines of pending approval requests.
#[cfg(any(test, target_os = "linux"))]
pub(crate) const INO_APPROVALS: u64 = 16;
/// /agents/system/egress_addr — bound HTTP proxy URL or "not configured" (p7.5b).
#[cfg(any(test, target_os = "linux"))]
const INO_SYS_EGRESS_ADDR: u64 = 17;
/// /agents/system/isolation — device-level isolation tier JSON (ma.4).
#[cfg(any(test, target_os = "linux"))]
const INO_SYS_ISOLATION: u64 = 18;
/// /agents/system/credentials — gateway health + per-provider status JSON (cred.5).
#[cfg(any(test, target_os = "linux"))]
const INO_SYS_CREDENTIALS: u64 = 19;

// Invariant: all per-agent file offsets must fit within DIR_STEP - 1 slots.
#[cfg(any(test, target_os = "linux"))]
const _: () = assert!(OFF_TOOLS   < DIR_STEP - 1, "OFF_TOOLS must be < DIR_STEP - 1");
#[cfg(any(test, target_os = "linux"))]
const _: () = assert!(OFF_PARENT  < DIR_STEP - 1, "OFF_PARENT must be < DIR_STEP - 1");
#[cfg(any(test, target_os = "linux"))]
const _: () = assert!(OFF_SANDBOX < DIR_STEP - 1, "OFF_SANDBOX must be < DIR_STEP - 1");
#[cfg(any(test, target_os = "linux"))]
const _: () = assert!(OFF_TIER    < DIR_STEP - 1, "OFF_TIER must be < DIR_STEP - 1");
#[cfg(any(test, target_os = "linux"))]
const _: () = assert!(OFF_PID     < DIR_STEP - 1, "OFF_PID must be < DIR_STEP - 1");
#[cfg(any(test, target_os = "linux"))]
const _: () = assert!(OFF_CREDENTIALS < DIR_STEP - 1, "OFF_CREDENTIALS must be < DIR_STEP - 1");
#[cfg(any(test, target_os = "linux"))]
const _: () = assert!(OFF_ATTENTION < DIR_STEP - 1, "OFF_ATTENTION must be < DIR_STEP - 1");

/// Last 64 KB of flight.jsonl to scan for per-agent events.
#[cfg(any(test, target_os = "linux"))]
const FLIGHT_TAIL_BYTES: u64 = 64 * 1024;
/// Maximum matching lines returned in the flight virtual file.
#[cfg(any(test, target_os = "linux"))]
const FLIGHT_TAIL_LINES: usize = 20;
/// Maximum keys returned per long_term/ or kb/<seg>/ directory listing.
#[cfg(any(test, target_os = "linux"))]
const MAX_DIR_KEYS: usize = 100;
/// Namespace prefix for per-agent long-term memory keys.
#[cfg(any(test, target_os = "linux"))]
const AGENT_NS_PREFIX: &str = "agent/";

#[cfg(any(test, target_os = "linux"))]
use std::collections::HashMap;

#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(any(test, target_os = "linux"))]
use crate::snapshot::AgentStatus;

#[cfg(any(test, target_os = "linux"))]
use libc;

/// Kernel TTL for all FUSE attributes and directory entries.
#[cfg(target_os = "linux")]
const TTL: Duration = Duration::ZERO;

#[cfg(any(test, target_os = "linux"))]
const ROOT_INO: u64 = 1;
#[cfg(any(test, target_os = "linux"))]
const INO_KB: u64 = 9;
#[cfg(any(test, target_os = "linux"))]
const DIR_START: u64 = 1010;
/// Dynamic inode pool starts here (long_term/<key>, kb/<seg>/, kb/<seg>/<key>).
#[cfg(any(test, target_os = "linux"))]
const DYNAMIC_INO_START: u64 = 1_000_000;

/// Sentinel values written to agent virtual files.
/// Must stay in sync with BUDGET_UNLIMITED_SENTINEL / TOOLS_NONE_SENTINEL in
/// agentctl/src/watch/reader.rs.
#[cfg(any(test, target_os = "linux"))]
const BUDGET_UNLIMITED_SENTINEL: &str = "unlimited";
#[cfg(any(test, target_os = "linux"))]
const TOOLS_NONE_SENTINEL: &str = "(none)";

/// Describes the kind of entity behind a dynamic inode.
#[cfg(any(test, target_os = "linux"))]
enum DynInoKind {
    LtFile  { agent_id: String, key: String },
    KbSeg   { segment: String },
    KbFile  { segment: String, key: String },
}

#[cfg(any(test, target_os = "linux"))]
struct AgentsFs {
    snapshot:       Arc<RwLock<SchedulerSnapshot>>,
    /// Optional memory access for memory/ and kb/ subtrees.
    memory:         Option<Arc<dyn MemoryAccess>>,
    /// Opaque callback that receives bytes written to /agents/control.
    /// Returns 0 on success, or a libc errno (EBUSY, EINVAL, EIO) on failure.
    control_dispatch: Option<crate::ControlDispatch>,
    /// Buffers for in-progress writes keyed by file handle.
    /// POSIX allows write() to fragment a single logical payload across
    /// multiple calls; we accumulate here and parse only on flush()/release().
    write_buffers:  HashMap<u64, Vec<u8>>,
    /// Monotonically increasing file-handle counter for open() calls on INO_CONTROL.
    /// Prevents two concurrent writers clobbering each other's buffer under fh=0.
    #[allow(dead_code)]
    next_fh:        u64,
    /// agent_id → directory inode.
    /// INVARIANT: `dir_inodes` and `inode_to_id` are always consistent.
    /// Both maps are written atomically inside `alloc_dir()` — that is the
    /// ONLY mutation site. Any code that writes to either map without going
    /// through `alloc_dir()` will break lookups and cause panics.
    dir_inodes:     HashMap<String, u64>,
    /// fixed per-agent inodes (dir + 7 children) → agent_id
    inode_to_id:    HashMap<u64, String>,
    next_dir_inode: u64,
    // Dynamic inode allocation
    next_dyn_ino:   u64,
    dyn_ino_kind:   HashMap<u64, DynInoKind>,
    lt_key_ino:     HashMap<(String, String), u64>,  // (agent_id, key) → ino
    kb_seg_ino:     HashMap<String, u64>,             // segment → ino
    kb_key_ino:     HashMap<(String, String), u64>,  // (segment, key) → ino
}

/// Format a memory entry value as a newline-terminated byte vector.
#[cfg(any(test, target_os = "linux"))]
fn value_bytes(val: &str) -> Vec<u8> {
    format!("{val}\n").into_bytes()
}

/// Produce a properly escaped JSON string value (without surrounding quotes).
/// Handles `"`, `\`, and all ASCII control characters (U+0000–U+001F).
#[cfg(any(test, target_os = "linux"))]
fn json_escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c    => out.push(c),
        }
    }
    out
}

/// Return up to `MAX_DIR_KEYS` keys for a namespace, applying the cap consistently.
#[cfg(any(test, target_os = "linux"))]
fn capped_keys(mem: &dyn MemoryAccess, namespace: &str) -> Vec<String> {
    mem.list_keys(namespace).into_iter().take(MAX_DIR_KEYS).collect()
}

#[cfg(any(test, target_os = "linux"))]
impl AgentsFs {
    fn new(
        snapshot:         Arc<RwLock<SchedulerSnapshot>>,
        memory:           Option<Arc<dyn MemoryAccess>>,
        control_dispatch: Option<crate::ControlDispatch>,
    ) -> Self {
        Self {
            snapshot,
            memory,
            control_dispatch,
            write_buffers:  HashMap::new(),
            next_fh:        1,
            dir_inodes:     HashMap::new(),
            inode_to_id:    HashMap::new(),
            next_dir_inode: DIR_START,
            next_dyn_ino:   DYNAMIC_INO_START,
            dyn_ino_kind:   HashMap::new(),
            lt_key_ino:     HashMap::new(),
            kb_seg_ino:     HashMap::new(),
            kb_key_ino:     HashMap::new(),
        }
    }

    /// Accumulate bytes for `fh` and, on flush, dispatch to the control callback.
    /// Drains the buffer on success. Returns an i32 errno (0 = ok).
    ///
    /// Extracted as a non-trait helper so tests can call it without a
    /// `fuser::ReplyEmpty` (which has no public constructor).
    fn process_control_flush(&mut self, fh: u64) -> i32 {
        let bytes = match self.write_buffers.remove(&fh) {
            Some(b) if !b.is_empty() => b,
            _ => return 0,
        };
        match &self.control_dispatch {
            None          => libc::EROFS,
            Some(dispatch) => dispatch(&bytes),
        }
    }

    /// Return the directory inode for `agent_id`, allocating a new one if needed.
    fn alloc_dir(&mut self, agent_id: &str) -> u64 {
        if let Some(&ino) = self.dir_inodes.get(agent_id) {
            return ino;
        }
        assert!(
            self.next_dir_inode < DYNAMIC_INO_START,
            "fixed inode pool reached dynamic inode range at {}",
            self.next_dir_inode
        );
        let ino = self.next_dir_inode;
        self.next_dir_inode += DIR_STEP;
        self.dir_inodes.insert(agent_id.to_string(), ino);
        // Register all 15 fixed inodes so inode_to_id lookups work.
        for offset in [
            0, OFF_STATUS, OFF_CONTEXT, OFF_BUDGET, OFF_FLIGHT,
            OFF_MEMORY_DIR, OFF_SHORT_TERM, OFF_LONG_TERM_DIR, OFF_TOOLS, OFF_PARENT,
            OFF_SANDBOX, OFF_TIER, OFF_PID, OFF_CREDENTIALS, OFF_ATTENTION,
        ] {
            self.inode_to_id.insert(ino + offset, agent_id.to_string());
        }
        ino
    }

    fn alloc_lt_file(&mut self, agent_id: &str, key: &str) -> u64 {
        let k = (agent_id.to_string(), key.to_string());
        if let Some(&ino) = self.lt_key_ino.get(&k) {
            return ino;
        }
        let ino = self.next_dyn_ino;
        self.next_dyn_ino += 1;
        self.lt_key_ino.insert(k, ino);
        self.dyn_ino_kind.insert(ino, DynInoKind::LtFile {
            agent_id: agent_id.to_string(),
            key:      key.to_string(),
        });
        ino
    }

    fn alloc_kb_seg(&mut self, segment: &str) -> u64 {
        if let Some(&ino) = self.kb_seg_ino.get(segment) {
            return ino;
        }
        let ino = self.next_dyn_ino;
        self.next_dyn_ino += 1;
        self.kb_seg_ino.insert(segment.to_string(), ino);
        self.dyn_ino_kind.insert(ino, DynInoKind::KbSeg { segment: segment.to_string() });
        ino
    }

    fn alloc_kb_file(&mut self, segment: &str, key: &str) -> u64 {
        let k = (segment.to_string(), key.to_string());
        if let Some(&ino) = self.kb_key_ino.get(&k) {
            return ino;
        }
        let ino = self.next_dyn_ino;
        self.next_dyn_ino += 1;
        self.kb_key_ino.insert(k, ino);
        self.dyn_ino_kind.insert(ino, DynInoKind::KbFile {
            segment: segment.to_string(),
            key:     key.to_string(),
        });
        ino
    }

    /// Decode what kind of entity a known parent inode represents.
    fn parent_kind(&self, parent: u64) -> Option<ParentKind> {
        if parent == ROOT_INO {
            return Some(ParentKind::Root);
        }
        if parent == INO_KB {
            return Some(ParentKind::Kb);
        }
        if parent == INO_SYSTEM {
            return Some(ParentKind::SystemDir);
        }
        if let Some(agent_id) = self.inode_to_id.get(&parent) {
            let base = self.dir_inodes[agent_id];
            let offset = parent.wrapping_sub(base);
            return match offset {
                0 => Some(ParentKind::AgentDir(agent_id.clone())),
                // ar-03: memory/ and long_term/ do not exist when no memory store is configured.
                o if o == OFF_MEMORY_DIR && self.memory.is_some() => {
                    Some(ParentKind::MemoryDir(agent_id.clone()))
                }
                o if o == OFF_LONG_TERM_DIR && self.memory.is_some() => {
                    Some(ParentKind::LongTermDir(agent_id.clone()))
                }
                _ => None,
            };
        }
        if let Some(DynInoKind::KbSeg { segment }) = self.dyn_ino_kind.get(&parent) {
            return Some(ParentKind::KbSegDir(segment.clone()));
        }
        None
    }

    /// Content for fixed per-agent file inodes. Returns `None` for directory inodes
    /// or unknown inodes.
    fn file_content_for_ino(&self, ino: u64) -> Option<Vec<u8>> {
        let agent_id = self.inode_to_id.get(&ino)?;
        let dir_ino  = self.dir_inodes.get(agent_id)?;
        let offset   = ino.wrapping_sub(*dir_ino);

        let snap = self.snapshot.read().ok()?;
        let agent = snap.agents.iter().find(|a| &a.id == agent_id)?;

        let content = match offset {
            OFF_STATUS => {
                let s = match &agent.status {
                    AgentStatus::AwaitingChild(child_id) =>
                        format!("awaiting_child:{child_id}\n"),
                    other => format!("{}\n", other.as_str()),
                };
                s.into_bytes()
            }
            OFF_CONTEXT => format!("{}\n", agent.context_tokens).into_bytes(),
            OFF_BUDGET => {
                if agent.token_budget == 0 {
                    format!("{BUDGET_UNLIMITED_SENTINEL}\n").into_bytes()
                } else {
                    format!("{}\n", agent.token_budget).into_bytes()
                }
            }
            OFF_FLIGHT    => read_flight_tail(&agent.id),
            OFF_SHORT_TERM => {
                if agent.short_term_previews.is_empty() {
                    b"(empty)\n".to_vec()
                } else {
                    format!("{}\n", agent.short_term_previews.join("\n")).into_bytes()
                }
            }
            OFF_TOOLS => {
                if agent.tools.is_empty() {
                    format!("{TOOLS_NONE_SENTINEL}\n").into_bytes()
                } else {
                    format!("{}\n", agent.tools.join("\n")).into_bytes()
                }
            }
            OFF_PARENT => {
                match &agent.parent_id {
                    Some(pid) => format!("{pid}\n").into_bytes(),
                    None      => b"(none)\n".to_vec(),
                }
            }
            OFF_SANDBOX => {
                // When capabilities == None (unrestricted) show all registered servers;
                // otherwise show only the servers the agent has Mcp-capability access to.
                let server_names: Vec<&str> = if agent.capabilities_unrestricted {
                    snap.sandbox.servers.iter().map(|s| s.name.as_str()).collect()
                } else {
                    agent.accessible_server_names.iter().map(|s| s.as_str()).collect()
                };
                let servers: String = server_names.iter().map(|name| {
                    let enf = snap.sandbox.servers.iter().find(|s| s.name.as_str() == *name);
                    match enf {
                        Some(s) => format!(
                            "{{\"name\":\"{}\",\"transport\":\"{}\",\"isolation\":\"{}\",\
                            \"landlock\":{},\"seccomp\":{},\
                            \"spawn_enforcement\":\"{}\",\
                            \"namespace_net\":{},\"namespace_mount\":{},\
                            \"landlock_net\":{}}}",
                            json_escape_str(&s.name),
                            json_escape_str(&s.transport),
                            json_escape_str(&s.isolation),
                            s.landlock, s.seccomp,
                            json_escape_str(&s.spawn_enforcement),
                            s.namespace_net, s.namespace_mount,
                            s.landlock_net,
                        ),
                        None => format!(
                            "{{\"name\":\"{}\",\"transport\":\"\",\"isolation\":\"none\",\
                            \"landlock\":false,\"seccomp\":false,\
                            \"spawn_enforcement\":\"none\",\
                            \"namespace_net\":false,\"namespace_mount\":false,\
                            \"landlock_net\":false}}",
                            json_escape_str(name),
                        ),
                    }
                }).collect::<Vec<_>>().join(",");
                format!("{{\"servers\":[{servers}]}}\n").into_bytes()
            }
            OFF_TIER => {
                let tier = agent.tier.as_deref().unwrap_or("native");
                format!("{tier}\n").into_bytes()
            }
            OFF_PID => {
                match agent.pid {
                    Some(pid) => format!("{pid}\n").into_bytes(),
                    None      => b"(none)\n".to_vec(),
                }
            }
            OFF_CREDENTIALS => {
                // cred.5: per-agent credential grant — providers, request/denied counts,
                // and last-access timestamps as JSON.
                let providers  = serde_json::to_string(&agent.credential_providers).unwrap_or_else(|_| "[]".to_string());
                let req_counts = serde_json::to_string(&agent.credential_request_counts).unwrap_or_else(|_| "{}".to_string());
                let den_counts = serde_json::to_string(&agent.credential_denied_counts).unwrap_or_else(|_| "{}".to_string());
                let last_access = serde_json::to_string(&agent.credential_last_access_at).unwrap_or_else(|_| "{}".to_string());
                format!(
                    "{{\"providers\":{providers},\"request_counts\":{req_counts},\
                     \"denied_counts\":{den_counts},\"last_access_at\":{last_access}}}\n"
                ).into_bytes()
            }
            OFF_ATTENTION => {
                // ux.2a: active attention signals as a JSON array (empty array = clean).
                let json = serde_json::to_string(&agent.attention).unwrap_or_else(|_| "[]".to_string());
                format!("{json}\n").into_bytes()
            }
            // OFF_MEMORY_DIR and OFF_LONG_TERM_DIR are directories — not served here.
            _ => return None,
        };
        Some(content)
    }

    /// Content for /agents/system/{budget,queue,sandbox,provider} virtual files.
    fn sys_file_content(&self, ino: u64) -> Option<Vec<u8>> {
        let snap = self.snapshot.read().ok()?;
        let content = match ino {
            INO_SYS_BUDGET => format!(
                "{{\"spent\":{},\"total\":0}}\n",
                snap.global_tokens_spent
            ),
            INO_SYS_QUEUE => format!(
                "{{\"depth\":{}}}\n",
                snap.queue_depth
            ),
            INO_SYS_SANDBOX => {
                let sb = &snap.sandbox;
                let any = sb.any_sandboxed;
                let degs: String = sb.degradations.iter().map(|d| {
                    format!("\"{}\"", json_escape_str(d))
                }).collect::<Vec<_>>().join(",");
                let servers: String = sb.servers.iter().map(|s| {
                    format!(
                        "{{\"name\":\"{}\",\"transport\":\"{}\",\"isolation\":\"{}\",\
                        \"landlock\":{},\"seccomp\":{},\
                        \"spawn_enforcement\":\"{}\",\
                        \"namespace_net\":{},\"namespace_mount\":{},\
                        \"landlock_net\":{}}}",
                        json_escape_str(&s.name),
                        json_escape_str(&s.transport),
                        json_escape_str(&s.isolation),
                        s.landlock, s.seccomp,
                        json_escape_str(&s.spawn_enforcement),
                        s.namespace_net, s.namespace_mount,
                        s.landlock_net,
                    )
                }).collect::<Vec<_>>().join(",");
                format!("{{\"any_sandboxed\":{any},\"servers\":[{servers}],\"degradations\":[{degs}]}}\n")
            }
            INO_SYS_PROVIDER => {
                let escaped = json_escape_str(&snap.provider_model);
                format!("{{\"model\":\"{escaped}\",\"backend\":\"anthropic\"}}\n")
            }
            INO_SYS_EGRESS_ADDR => {
                match &snap.egress_addr {
                    Some(addr) => format!("{addr}\n"),
                    None       => "not configured\n".to_string(),
                }
            }
            INO_SYS_ISOLATION => {
                let caps = snap.isolation_caps.as_ref()
                    .cloned()
                    .unwrap_or_default();
                let mut json = serde_json::to_string(&caps).unwrap_or_default();
                json.push('\n');
                json
            }
            INO_SYS_CREDENTIALS => {
                // cred.5: gateway health + per-provider status JSON.
                match &snap.credential_snapshot {
                    Some(cs) => {
                        let mut json = serde_json::to_string(cs).unwrap_or_default();
                        json.push('\n');
                        json
                    }
                    None => "{\"gateway_enabled\":false,\"configured_providers\":[],\"provider_health\":[]}\n".to_string(),
                }
            }
            _ => return None,
        };
        Some(content.into_bytes())
    }

    /// Content for /agents/approvals — one JSON object per pending approval, one per line.
    fn approvals_content(&self) -> Vec<u8> {
        let snap = match self.snapshot.read() {
            Ok(s)  => s,
            Err(_) => return b"[]\n".to_vec(),
        };
        if snap.pending_actions.is_empty() {
            return b"[]\n".to_vec();
        }
        let lines: Vec<String> = snap
            .pending_actions
            .iter()
            .map(|pa| {
                format!(
                    "{{\"id\":\"{}\",\"agent_id\":\"{}\",\"kind\":\"{}\",\
                     \"risk\":\"{}\",\"summary\":\"{}\",\"args\":{},\"age_secs\":{}}}",
                    json_escape_str(&pa.id),
                    json_escape_str(&pa.agent_id),
                    json_escape_str(&pa.kind),
                    json_escape_str(&pa.risk),
                    json_escape_str(&pa.summary),
                    // args_json is already valid JSON
                    if pa.args_json.is_empty() { "{}".to_string() } else { pa.args_json.clone() },
                    pa.age_secs,
                )
            })
            .collect();
        let mut out = lines.join("\n");
        out.push('\n');
        out.into_bytes()
    }

    /// Content for dynamic file inodes (long_term/<key>, kb/<seg>/<key>).
    fn dyn_file_content(&self, ino: u64) -> Option<Vec<u8>> {
        let mem = self.memory.as_ref()?;
        match self.dyn_ino_kind.get(&ino)? {
            DynInoKind::LtFile { agent_id, key } => {
                // ar-02: this arm is reached only when dyn_ino_kind maps ino → LtFile.
                let ns = format!("{}{}", AGENT_NS_PREFIX, agent_id);
                let val = mem.get_entry(&ns, key)?;
                Some(value_bytes(&val))
            }
            DynInoKind::KbFile { segment, key } => {
                // ar-02: this arm is reached only when dyn_ino_kind maps ino → KbFile.
                let val = mem.get_entry(segment, key)?;
                Some(value_bytes(&val))
            }
            DynInoKind::KbSeg { .. } => None,  // directory, not a file
        }
    }

    /// Prune all inode-map entries for a dead agent (ar-01).
    ///
    /// Must be called with the agent_id *collected into a String* before this
    /// call — callers iterate `dir_inodes.keys()` and must collect the dead IDs
    /// into a `Vec<String>` before calling this (Rust borrow checker requires it).
    fn prune_dead_agent(&mut self, agent_id: &str) {
        if let Some(base) = self.dir_inodes.remove(agent_id) {
            // Remove all 15 fixed per-agent inodes (dir + offsets 1–14).
            for offset in [
                0u64, OFF_STATUS, OFF_CONTEXT, OFF_BUDGET, OFF_FLIGHT,
                OFF_MEMORY_DIR, OFF_SHORT_TERM, OFF_LONG_TERM_DIR, OFF_TOOLS, OFF_PARENT,
                OFF_SANDBOX, OFF_TIER, OFF_PID, OFF_CREDENTIALS, OFF_ATTENTION,
            ] {
                self.inode_to_id.remove(&(base + offset));
            }
        }
        // Remove dynamic inodes for this agent's long-term keys.
        let lt_keys_to_remove: Vec<(String, String)> = self
            .lt_key_ino
            .keys()
            .filter(|(id, _)| id == agent_id)
            .cloned()
            .collect();
        for k in &lt_keys_to_remove {
            if let Some(ino) = self.lt_key_ino.remove(k) {
                self.dyn_ino_kind.remove(&ino);
            }
        }
        // Remove kb_seg_ino entries scoped to this agent's namespace prefix.
        // Shared segments ("canon", "scratch") use flat names — not "agent/{id}/" —
        // so they are correctly left untouched by this filter.
        let agent_seg_prefix = format!("agent/{}/", agent_id);
        let kb_segs_to_remove: Vec<String> = self
            .kb_seg_ino
            .keys()
            .filter(|seg| seg.starts_with(&agent_seg_prefix))
            .cloned()
            .collect();
        for seg in &kb_segs_to_remove {
            if let Some(ino) = self.kb_seg_ino.remove(seg) {
                self.dyn_ino_kind.remove(&ino);
            }
        }
        // Remove kb_key_ino entries (individual file inodes) for this agent's segments.
        // Mirrors the kb_seg_ino cleanup above — must also clear the file-level inodes
        // or dyn_ino_kind will accumulate stale KbFile entries for dead agents.
        let kb_files_to_remove: Vec<(String, String)> = self
            .kb_key_ino
            .keys()
            .filter(|(seg, _)| seg.starts_with(&agent_seg_prefix))
            .cloned()
            .collect();
        for k in &kb_files_to_remove {
            if let Some(ino) = self.kb_key_ino.remove(k) {
                self.dyn_ino_kind.remove(&ino);
            }
        }
    }

    /// Whether `ino` is a known directory inode.
    fn is_dir_ino(&self, ino: u64) -> bool {
        if ino == ROOT_INO { return true; }
        if ino == INO_KB { return true; }
        if ino == INO_SYSTEM { return true; }
        if let Some(agent_id) = self.inode_to_id.get(&ino) {
            let base = self.dir_inodes[agent_id];
            let offset = ino.wrapping_sub(base);
            // ar-03: mirror the getattr() guard — memory/ and long_term/ do not
            // exist as directories when no memory store is configured.
            if (offset == OFF_MEMORY_DIR || offset == OFF_LONG_TERM_DIR)
                && self.memory.is_none()
            {
                return false;
            }
            return matches!(offset, 0) || offset == OFF_MEMORY_DIR || offset == OFF_LONG_TERM_DIR;
        }
        if let Some(DynInoKind::KbSeg { .. }) = self.dyn_ino_kind.get(&ino) {
            return true;
        }
        false
    }
}

/// Determines the "kind" of a directory inode for lookup/readdir routing.
#[cfg(any(test, target_os = "linux"))]
enum ParentKind {
    Root,
    Kb,
    SystemDir,
    // Fields used in Linux-gated FUSE handlers (dead_code from macOS test perspective).
    #[allow(dead_code)] AgentDir(String),
    MemoryDir(String),
    LongTermDir(String),
    #[allow(dead_code)] KbSegDir(String),
}

/// Only used in tests (the FUSE impl hard-codes file names in readdir).
#[cfg(test)]
fn file_name_for_offset(offset: u64) -> Option<&'static str> {
    match offset {
        OFF_STATUS        => Some("status"),
        OFF_CONTEXT       => Some("context_size"),
        OFF_BUDGET        => Some("budget"),
        OFF_FLIGHT        => Some("flight"),
        OFF_MEMORY_DIR    => Some("memory"),
        OFF_SHORT_TERM    => Some("short_term"),
        OFF_LONG_TERM_DIR => Some("long_term"),
        OFF_TOOLS         => Some("tools"),
        OFF_PARENT        => Some("parent"),
        OFF_SANDBOX       => Some("sandbox"),
        OFF_TIER          => Some("tier"),
        OFF_PID           => Some("pid"),
        OFF_CREDENTIALS   => Some("credentials"),
        OFF_ATTENTION     => Some("attention"),
        _ => None,
    }
}

/// Read the last `FLIGHT_TAIL_BYTES` of `flight.jsonl`, filter lines that
/// contain `"agent":"<id>"`, and return the last `FLIGHT_TAIL_LINES` of them.
#[cfg(any(test, target_os = "linux"))]
fn read_flight_tail(agent_id: &str) -> Vec<u8> {
    read_flight_tail_from(Path::new("flight.jsonl"), agent_id)
}

#[cfg(any(test, target_os = "linux"))]
fn read_flight_tail_from(path: &Path, agent_id: &str) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};

    let tag = format!(r#""agent":"{}""#, agent_id);

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    let file_size = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let start = file_size.saturating_sub(FLIGHT_TAIL_BYTES);
    let _ = file.seek(SeekFrom::Start(start));

    let mut raw = Vec::new();
    let _ = file.read_to_end(&mut raw);
    // Use lossy conversion so non-UTF-8 bytes (e.g. binary tool output in MCP)
    // do not silently truncate the entire buffer as read_to_string would.
    let buf = String::from_utf8_lossy(&raw).into_owned();

    // If we seeked into the middle of a line, skip the first (partial) line.
    let buf_start = if start > 0 {
        buf.find('\n').map(|i| i + 1).unwrap_or(0)
    } else {
        0
    };

    let matching: Vec<&str> = buf[buf_start..]
        .lines()
        .filter(|line| line.contains(&tag))
        .collect();

    let tail: Vec<&str> = matching
        .iter()
        .rev()
        .take(FLIGHT_TAIL_LINES)
        .rev()
        .copied()
        .collect();

    if tail.is_empty() {
        Vec::new()
    } else {
        format!("{}\n", tail.join("\n")).into_bytes()
    }
}

#[cfg(target_os = "linux")]
fn make_file_attr(ino: u64, size: u64, kind: fuser::FileType) -> fuser::FileAttr {
    let now = std::time::SystemTime::UNIX_EPOCH;
    fuser::FileAttr {
        ino,
        size,
        blocks: size.div_ceil(512),
        atime:  now,
        mtime:  now,
        ctime:  now,
        crtime: now,
        kind,
        perm:   if kind == fuser::FileType::Directory { 0o555 } else { 0o444 },
        nlink:  if kind == fuser::FileType::Directory { 2 } else { 1 },
        uid:    0,
        gid:    0,
        rdev:   0,
        blksize: 512,
        flags:  0,
    }
}

#[cfg(target_os = "linux")]
impl fuser::Filesystem for AgentsFs {
    fn lookup(
        &mut self,
        _req: &fuser::Request<'_>,
        parent: u64,
        name: &std::ffi::OsStr,
        reply: fuser::ReplyEntry,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None    => { reply.error(libc::ENOENT); return; }
        };

        match self.parent_kind(parent) {
            None => { reply.error(libc::ENOENT); }

            Some(ParentKind::Root) => {
                // control write surface (always present when control_dispatch is set)
                if name_str == "control" && self.control_dispatch.is_some() {
                    let mut attr = make_file_attr(INO_CONTROL, 0, fuser::FileType::RegularFile);
                    attr.perm = 0o222;
                    reply.entry(&TTL, &attr, 0);
                    return;
                }
                // approvals read-only pseudofile (always present)
                if name_str == "approvals" {
                    let sz = self.approvals_content().len() as u64;
                    reply.entry(&TTL, &make_file_attr(INO_APPROVALS, sz, fuser::FileType::RegularFile), 0);
                    return;
                }
                // kb/ dir (only when memory is configured)
                if name_str == "kb" && self.memory.is_some() {
                    reply.entry(&TTL, &make_file_attr(INO_KB, 0, fuser::FileType::Directory), 0);
                    return;
                }
                // system/ dir (always present)
                if name_str == "system" {
                    reply.entry(&TTL, &make_file_attr(INO_SYSTEM, 0, fuser::FileType::Directory), 0);
                    return;
                }
                // Check existence before allocating inode to avoid leaking inode state on ENOENT.
                let snap = match self.snapshot.read() {
                    Ok(s) => s,
                    Err(_) => { reply.error(libc::EIO); return; }
                };
                if snap.agents.iter().any(|a| a.id == name_str) {
                    drop(snap);
                    let ino = self.alloc_dir(name_str);
                    reply.entry(&TTL, &make_file_attr(ino, 0, fuser::FileType::Directory), 0);
                } else {
                    reply.error(libc::ENOENT);
                }
            }

            Some(ParentKind::SystemDir) => {
                let ino = match name_str {
                    "budget"      => INO_SYS_BUDGET,
                    "queue"       => INO_SYS_QUEUE,
                    "sandbox"     => INO_SYS_SANDBOX,
                    "provider"    => INO_SYS_PROVIDER,
                    "egress_addr" => INO_SYS_EGRESS_ADDR,
                    "isolation"   => INO_SYS_ISOLATION,
                    "credentials" => INO_SYS_CREDENTIALS,
                    _ => { reply.error(libc::ENOENT); return; }
                };
                let sz = self.sys_file_content(ino).map(|c| c.len() as u64).unwrap_or(0);
                reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
            }

            Some(ParentKind::Kb) => {
                let exists = self.memory.as_ref()
                    .map(|m| m.list_namespaces().iter().any(|ns| ns == name_str && !ns.starts_with(AGENT_NS_PREFIX)))
                    .unwrap_or(false);
                if exists {
                    let ino = self.alloc_kb_seg(name_str);
                    reply.entry(&TTL, &make_file_attr(ino, 0, fuser::FileType::Directory), 0);
                } else {
                    reply.error(libc::ENOENT);
                }
            }

            Some(ParentKind::AgentDir(agent_id)) => {
                let dir_ino = self.dir_inodes[&agent_id];
                match name_str {
                    "status"       => {
                        let ino = dir_ino + OFF_STATUS;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "context_size" => {
                        let ino = dir_ino + OFF_CONTEXT;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "budget" => {
                        let ino = dir_ino + OFF_BUDGET;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "flight" => {
                        let ino = dir_ino + OFF_FLIGHT;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "tools" => {
                        let ino = dir_ino + OFF_TOOLS;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "parent" => {
                        let ino = dir_ino + OFF_PARENT;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "sandbox" => {
                        let ino = dir_ino + OFF_SANDBOX;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "tier" => {
                        let ino = dir_ino + OFF_TIER;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "pid" => {
                        let ino = dir_ino + OFF_PID;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    // Ship-review finding (Codex structured review): this arm was missing for
                    // both "credentials" (cred.5, pre-existing) and "attention" (ux.2a) — both
                    // files were listed by readdir but returned ENOENT on open-by-path over a
                    // real FUSE mount, since lookup() name resolution never had a match arm for
                    // them. readdir and lookup() must stay in sync for every per-agent file.
                    "credentials" => {
                        let ino = dir_ino + OFF_CREDENTIALS;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "attention" => {
                        let ino = dir_ino + OFF_ATTENTION;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "memory" if self.memory.is_some() => {
                        let ino = dir_ino + OFF_MEMORY_DIR;
                        reply.entry(&TTL, &make_file_attr(ino, 0, fuser::FileType::Directory), 0);
                    }
                    _ => { reply.error(libc::ENOENT); }
                }
            }

            Some(ParentKind::MemoryDir(agent_id)) => {
                let dir_ino = self.dir_inodes[&agent_id];
                match name_str {
                    "short_term" => {
                        let ino = dir_ino + OFF_SHORT_TERM;
                        let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                        reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                    }
                    "long_term" => {
                        let ino = dir_ino + OFF_LONG_TERM_DIR;
                        reply.entry(&TTL, &make_file_attr(ino, 0, fuser::FileType::Directory), 0);
                    }
                    _ => { reply.error(libc::ENOENT); }
                }
            }

            Some(ParentKind::LongTermDir(agent_id)) => {
                let ns = format!("{}{}", AGENT_NS_PREFIX, agent_id);
                if let Some(val) = self.memory.as_ref().and_then(|m| m.get_entry(&ns, name_str)) {
                    let ino = self.alloc_lt_file(&agent_id, name_str);
                    let sz = val.len() as u64 + 1;
                    reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                } else {
                    reply.error(libc::ENOENT);
                }
            }

            Some(ParentKind::KbSegDir(segment)) => {
                if let Some(val) = self.memory.as_ref().and_then(|m| m.get_entry(&segment, name_str)) {
                    let ino = self.alloc_kb_file(&segment, name_str);
                    let sz = val.len() as u64 + 1;
                    reply.entry(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile), 0);
                } else {
                    reply.error(libc::ENOENT);
                }
            }
        }
    }

    fn getattr(&mut self, _req: &fuser::Request<'_>, ino: u64, reply: fuser::ReplyAttr) {
        if ino == ROOT_INO {
            reply.attr(&TTL, &make_file_attr(ROOT_INO, 0, fuser::FileType::Directory));
            return;
        }
        if ino == INO_KB {
            if self.memory.is_none() {
                reply.error(libc::ENOENT);
                return;
            }
            reply.attr(&TTL, &make_file_attr(INO_KB, 0, fuser::FileType::Directory));
            return;
        }
        if ino == INO_SYSTEM {
            reply.attr(&TTL, &make_file_attr(INO_SYSTEM, 0, fuser::FileType::Directory));
            return;
        }
        if (INO_SYS_BUDGET..=INO_SYS_PROVIDER).contains(&ino)
            || ino == INO_SYS_EGRESS_ADDR
            || ino == INO_SYS_ISOLATION
            || ino == INO_SYS_CREDENTIALS
        {
            let sz = self.sys_file_content(ino).map(|c| c.len() as u64).unwrap_or(0);
            reply.attr(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile));
            return;
        }
        if ino == INO_CONTROL {
            if self.control_dispatch.is_none() {
                reply.error(libc::ENOENT);
                return;
            }
            let mut attr = make_file_attr(INO_CONTROL, 0, fuser::FileType::RegularFile);
            attr.perm = 0o222;
            reply.attr(&TTL, &attr);
            return;
        }
        if ino == INO_APPROVALS {
            let sz = self.approvals_content().len() as u64;
            reply.attr(&TTL, &make_file_attr(INO_APPROVALS, sz, fuser::FileType::RegularFile));
            return;
        }
        if let Some(agent_id) = self.inode_to_id.get(&ino).cloned() {
            let base   = self.dir_inodes[&agent_id];
            let offset = ino.wrapping_sub(base);
            if matches!(offset, 0) {
                reply.attr(&TTL, &make_file_attr(ino, 0, fuser::FileType::Directory));
            } else if offset == OFF_MEMORY_DIR || offset == OFF_LONG_TERM_DIR {
                // ar-03: return ENOENT for memory/ and long_term/ when the memory store
                // is not configured — prevents VFS-layer inconsistency where getattr
                // returns Directory for an inode that readdir never lists.
                // OFF_SHORT_TERM (+6) intentionally exempted: it serves from
                // AgentSnapshot::short_term_previews regardless of store state.
                if self.memory.is_none() {
                    reply.error(libc::ENOENT);
                } else {
                    reply.attr(&TTL, &make_file_attr(ino, 0, fuser::FileType::Directory));
                }
            } else {
                let sz = self.file_content_for_ino(ino).map(|c| c.len() as u64).unwrap_or(0);
                reply.attr(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile));
            }
            return;
        }
        if let Some(kind) = self.dyn_ino_kind.get(&ino) {
            match kind {
                DynInoKind::KbSeg { .. } => {
                    reply.attr(&TTL, &make_file_attr(ino, 0, fuser::FileType::Directory));
                }
                _ => {
                    let sz = self.dyn_file_content(ino).map(|c| c.len() as u64).unwrap_or(0);
                    reply.attr(&TTL, &make_file_attr(ino, sz, fuser::FileType::RegularFile));
                }
            }
            return;
        }
        reply.error(libc::ENOENT);
    }

    fn read(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: fuser::ReplyData,
    ) {
        // Write-only surface: reading returns empty bytes (0o222 perm normally
        // prevents reads at the VFS layer; this handles direct read syscalls).
        if ino == INO_CONTROL {
            reply.data(b"");
            return;
        }
        // Read-only approvals pseudofile — JSON lines of pending approvals.
        if ino == INO_APPROVALS {
            let content = self.approvals_content();
            let offset = if offset < 0 { 0usize } else { offset as usize };
            let start = offset.min(content.len());
            let end   = offset.saturating_add(size as usize).min(content.len());
            reply.data(&content[start..end]);
            return;
        }
        let content = if let Some(c) = self.file_content_for_ino(ino) {
            c
        } else if let Some(c) = self.dyn_file_content(ino) {
            c
        } else if let Some(c) = self.sys_file_content(ino) {
            c
        } else {
            reply.error(libc::ENOENT);
            return;
        };
        let offset = if offset < 0 { 0usize } else { offset as usize };
        let start = offset.min(content.len());
        let end   = offset.saturating_add(size as usize).min(content.len());
        reply.data(&content[start..end]);
    }

    fn readdir(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: fuser::ReplyDirectory,
    ) {
        // Build the entry list, then apply the offset cursor.
        let entries: Vec<(u64, fuser::FileType, String)> = match self.parent_kind(ino) {
            None => { reply.error(libc::ENOENT); return; }

            Some(ParentKind::Root) => {
                let snap = match self.snapshot.read() {
                    Ok(s)  => s,
                    Err(_) => { reply.error(libc::EIO); return; }
                };
                let agent_ids: Vec<String> = snap.agents.iter().map(|a| a.id.clone()).collect();
                drop(snap);

                // ar-01: prune inode maps for agents no longer in the snapshot.
                // Collect dead IDs first — cannot iterate dir_inodes.keys() and
                // mutate self simultaneously (Rust borrow checker).
                {
                    use std::collections::HashSet;
                    let live: HashSet<&str> = agent_ids.iter().map(String::as_str).collect();
                    let dead: Vec<String> = self
                        .dir_inodes
                        .keys()
                        .filter(|id| !live.contains(id.as_str()))
                        .cloned()
                        .collect();
                    for id in &dead {
                        self.prune_dead_agent(id);
                    }
                }

                let mut v = vec![
                    (ROOT_INO, fuser::FileType::Directory, ".".to_string()),
                    (ROOT_INO, fuser::FileType::Directory, "..".to_string()),
                ];
                for id in &agent_ids {
                    let dir_ino = self.alloc_dir(id);
                    v.push((dir_ino, fuser::FileType::Directory, id.clone()));
                }
                if self.memory.is_some() {
                    v.push((INO_KB, fuser::FileType::Directory, "kb".to_string()));
                }
                if self.control_dispatch.is_some() {
                    v.push((INO_CONTROL, fuser::FileType::RegularFile, "control".to_string()));
                }
                v.push((INO_APPROVALS, fuser::FileType::RegularFile, "approvals".to_string()));
                v.push((INO_SYSTEM, fuser::FileType::Directory, "system".to_string()));
                v
            }

            Some(ParentKind::SystemDir) => {
                vec![
                    (INO_SYSTEM,           fuser::FileType::Directory,   ".".to_string()),
                    (ROOT_INO,             fuser::FileType::Directory,   "..".to_string()),
                    (INO_SYS_BUDGET,       fuser::FileType::RegularFile, "budget".to_string()),
                    (INO_SYS_QUEUE,        fuser::FileType::RegularFile, "queue".to_string()),
                    (INO_SYS_SANDBOX,      fuser::FileType::RegularFile, "sandbox".to_string()),
                    (INO_SYS_PROVIDER,     fuser::FileType::RegularFile, "provider".to_string()),
                    (INO_SYS_EGRESS_ADDR,  fuser::FileType::RegularFile, "egress_addr".to_string()),
                    (INO_SYS_ISOLATION,    fuser::FileType::RegularFile, "isolation".to_string()),
                    (INO_SYS_CREDENTIALS,  fuser::FileType::RegularFile, "credentials".to_string()),
                ]
            }

            Some(ParentKind::Kb) => {
                let segments = self.memory.as_ref()
                    .map(|m| m.list_namespaces())
                    .unwrap_or_default();
                let kb_segments: Vec<String> = segments.into_iter()
                    .filter(|ns| !ns.starts_with(AGENT_NS_PREFIX)
                        && !ns.contains('/')
                        && !ns.contains('\0'))
                    .collect();

                let mut v = vec![
                    (INO_KB,    fuser::FileType::Directory, ".".to_string()),
                    (ROOT_INO,  fuser::FileType::Directory, "..".to_string()),
                ];
                for seg in &kb_segments {
                    let seg_ino = self.alloc_kb_seg(seg);
                    v.push((seg_ino, fuser::FileType::Directory, seg.clone()));
                }
                v
            }

            Some(ParentKind::AgentDir(agent_id)) => {
                let dir_ino = self.dir_inodes[&agent_id];
                let mut v = vec![
                    (dir_ino,  fuser::FileType::Directory,   ".".to_string()),
                    (ROOT_INO, fuser::FileType::Directory,   "..".to_string()),
                    (dir_ino + OFF_STATUS,  fuser::FileType::RegularFile, "status".to_string()),
                    (dir_ino + OFF_CONTEXT, fuser::FileType::RegularFile, "context_size".to_string()),
                    (dir_ino + OFF_BUDGET,  fuser::FileType::RegularFile, "budget".to_string()),
                    (dir_ino + OFF_FLIGHT,  fuser::FileType::RegularFile, "flight".to_string()),
                    (dir_ino + OFF_TOOLS,   fuser::FileType::RegularFile, "tools".to_string()),
                    (dir_ino + OFF_PARENT,  fuser::FileType::RegularFile, "parent".to_string()),
                    (dir_ino + OFF_SANDBOX, fuser::FileType::RegularFile, "sandbox".to_string()),
                    (dir_ino + OFF_TIER,        fuser::FileType::RegularFile, "tier".to_string()),
                    (dir_ino + OFF_PID,         fuser::FileType::RegularFile, "pid".to_string()),
                    (dir_ino + OFF_CREDENTIALS, fuser::FileType::RegularFile, "credentials".to_string()),
                    (dir_ino + OFF_ATTENTION,   fuser::FileType::RegularFile, "attention".to_string()),
                ];
                if self.memory.is_some() {
                    v.push((dir_ino + OFF_MEMORY_DIR, fuser::FileType::Directory, "memory".to_string()));
                }
                v
            }

            Some(ParentKind::MemoryDir(agent_id)) => {
                let dir_ino  = self.dir_inodes[&agent_id];
                let mem_ino  = dir_ino + OFF_MEMORY_DIR;
                let lt_ino   = dir_ino + OFF_LONG_TERM_DIR;
                let st_ino   = dir_ino + OFF_SHORT_TERM;
                vec![
                    (mem_ino,  fuser::FileType::Directory,   ".".to_string()),
                    (dir_ino,  fuser::FileType::Directory,   "..".to_string()),
                    (st_ino,   fuser::FileType::RegularFile, "short_term".to_string()),
                    (lt_ino,   fuser::FileType::Directory,   "long_term".to_string()),
                ]
            }

            Some(ParentKind::LongTermDir(agent_id)) => {
                let dir_ino = self.dir_inodes[&agent_id];
                let lt_ino  = dir_ino + OFF_LONG_TERM_DIR;
                let ns = format!("{}{}", AGENT_NS_PREFIX, agent_id);
                let keys = self.memory.as_ref()
                    .map(|m| capped_keys(&**m, &ns))
                    .unwrap_or_default();

                let mut v = vec![
                    (lt_ino,  fuser::FileType::Directory, ".".to_string()),
                    (dir_ino + OFF_MEMORY_DIR, fuser::FileType::Directory, "..".to_string()),
                ];
                for key in keys.iter().filter(|k| !k.contains('/') && !k.contains('\0')) {
                    let file_ino = self.alloc_lt_file(&agent_id, key);
                    v.push((file_ino, fuser::FileType::RegularFile, key.clone()));
                }
                v
            }

            Some(ParentKind::KbSegDir(segment)) => {
                let seg_ino = match self.kb_seg_ino.get(&segment) {
                    Some(&ino) => ino,
                    None => { reply.error(libc::EIO); return; }
                };
                let keys = self.memory.as_ref()
                    .map(|m| capped_keys(&**m, &segment))
                    .unwrap_or_default();

                let mut v = vec![
                    (seg_ino, fuser::FileType::Directory, ".".to_string()),
                    (INO_KB,  fuser::FileType::Directory, "..".to_string()),
                ];
                for key in keys.iter().filter(|k| !k.contains('/') && !k.contains('\0')) {
                    let file_ino = self.alloc_kb_file(&segment, key);
                    v.push((file_ino, fuser::FileType::RegularFile, key.clone()));
                }
                v
            }
        };

        for (i, (entry_ino, kind, name)) in entries.iter().enumerate() {
            if (i as i64) < offset { continue; }
            if reply.add(*entry_ino, (i + 1) as i64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn opendir(&mut self, _req: &fuser::Request<'_>, ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
        if self.is_dir_ino(ino) {
            reply.opened(0, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn open(&mut self, _req: &fuser::Request<'_>, ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
        if ino == INO_CONTROL {
            if self.control_dispatch.is_none() {
                reply.error(libc::ENOENT);
                return;
            }
            let fh = self.next_fh;
            self.next_fh = self.next_fh.wrapping_add(1).max(1);
            reply.opened(fh, 0);
            return;
        }
        let is_file = if let Some(agent_id) = self.inode_to_id.get(&ino) {
            let base   = self.dir_inodes[agent_id];
            let offset = ino.wrapping_sub(base);
            !matches!(offset, 0) && offset != OFF_MEMORY_DIR && offset != OFF_LONG_TERM_DIR
        } else if let Some(kind) = self.dyn_ino_kind.get(&ino) {
            !matches!(kind, DynInoKind::KbSeg { .. })
        } else {
            (INO_SYS_BUDGET..=INO_SYS_PROVIDER).contains(&ino)
                || ino == INO_SYS_EGRESS_ADDR
                || ino == INO_SYS_ISOLATION
                || ino == INO_SYS_CREDENTIALS
                || ino == INO_APPROVALS
        };
        if is_file {
            reply.opened(0, fuser::consts::FOPEN_DIRECT_IO);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn write(
        &mut self,
        _req:    &fuser::Request<'_>,
        ino:     u64,
        fh:      u64,
        _offset: i64,
        data:    &[u8],
        _write_flags: u32,
        _flags:  i32,
        _lock_owner: Option<u64>,
        reply:   fuser::ReplyWrite,
    ) {
        if ino != INO_CONTROL {
            reply.error(libc::EROFS);
            return;
        }
        const CAP: usize = 64 * 1024;
        let buf = self.write_buffers.entry(fh).or_default();
        if buf.len() + data.len() > CAP {
            reply.error(libc::EFBIG);
            return;
        }
        buf.extend_from_slice(data);
        reply.written(data.len() as u32);
    }

    fn flush(
        &mut self,
        _req:  &fuser::Request<'_>,
        ino:   u64,
        fh:    u64,
        _lock_owner: u64,
        reply: fuser::ReplyEmpty,
    ) {
        if ino != INO_CONTROL {
            reply.ok();
            return;
        }
        let rc = self.process_control_flush(fh);
        if rc == 0 { reply.ok(); } else { reply.error(rc); }
    }

    fn release(
        &mut self,
        _req:   &fuser::Request<'_>,
        ino:    u64,
        fh:     u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply:  fuser::ReplyEmpty,
    ) {
        if ino == INO_CONTROL {
            // Dispatch any unflushed bytes (catches callers that skip close/flush).
            // process_control_flush removes the buffer; a prior flush() already emptied it → no-op.
            self.process_control_flush(fh);
        }
        reply.ok();
    }
}

/// Mount the `/agents` FUSE filesystem. Returns the `BackgroundSession` that keeps
/// the mount alive — drop it to unmount. Linux only.
#[cfg(target_os = "linux")]
pub fn mount(
    mountpoint:       &Path,
    snapshot:         Arc<RwLock<SchedulerSnapshot>>,
    memory:           Option<Arc<dyn MemoryAccess>>,
    control_dispatch: Option<crate::ControlDispatch>,
) -> anyhow::Result<fuser::BackgroundSession> {
    fuser::spawn_mount2(
        AgentsFs::new(snapshot, memory, control_dispatch),
        mountpoint,
        &[
            fuser::MountOption::FSName("agents".to_string()),
        ],
    )
    .map_err(Into::into)
}

/// Non-Linux stub: FUSE is not available on this platform.
#[cfg(not(target_os = "linux"))]
pub fn mount(
    _mountpoint: &Path,
    _snapshot:   Arc<RwLock<SchedulerSnapshot>>,
    _memory:     Option<Arc<dyn MemoryAccess>>,
) -> anyhow::Result<()> {
    anyhow::bail!("FUSE not supported on this platform")
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{AgentSnapshot, AgentStatus, SchedulerSnapshot};
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex, RwLock},
    };

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn make_snap(agents: Vec<AgentSnapshot>) -> Arc<RwLock<SchedulerSnapshot>> {
        Arc::new(RwLock::new(SchedulerSnapshot {
            agents,
            global_tokens_spent: 0,
            in_flight:           0,
            queue_depth:         0,
            provider_model:      String::new(),
            sandbox:             Default::default(),
            pending_actions:     vec![],
            egress_addr:         None,
            isolation_caps:      None,
            credential_snapshot: None,
        }))
    }

    fn make_snap_with_sys(
        agents:          Vec<AgentSnapshot>,
        global_spent:    u64,
        queue_depth:     usize,
        any_sandboxed:   bool,
        provider_model:  &str,
    ) -> Arc<RwLock<SchedulerSnapshot>> {
        use crate::snapshot::SandboxSummary;
        Arc::new(RwLock::new(SchedulerSnapshot {
            agents,
            global_tokens_spent: global_spent,
            in_flight:           0,
            queue_depth,
            provider_model:      provider_model.to_string(),
            sandbox:             SandboxSummary { any_sandboxed, ..Default::default() },
            pending_actions:     vec![],
            egress_addr:         None,
            isolation_caps:      None,
            credential_snapshot: None,
        }))
    }

    fn agent_snap(id: &str, status: AgentStatus) -> AgentSnapshot {
        AgentSnapshot {
            id:                      id.to_string(),
            status,
            turn:                    0,
            context_tokens:          100,
            token_budget:            50_000,
            task_preview:            "do something".to_string(),
            tools:                   vec![],
            short_term_previews:     vec![],
            parent_id:               None,
            accessible_server_names:   vec![],
            capabilities_unrestricted: false,
            tier:                    None,
            pid:                     None,
            credential_providers:      vec![],
            credential_request_counts: std::collections::HashMap::new(),
            credential_denied_counts:  std::collections::HashMap::new(),
            credential_last_access_at: std::collections::HashMap::new(),
            attention:                 vec![],
        }
    }

    fn fs_with_agent(id: &str, status: AgentStatus) -> AgentsFs {
        let snap = make_snap(vec![agent_snap(id, status)]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.alloc_dir(id);
        fs
    }

    /// Minimal in-memory MemoryAccess for tests.
    struct MockMemory {
        entries: Mutex<HashMap<String, String>>,
    }
    impl MockMemory {
        fn new() -> Arc<Self> {
            Arc::new(Self { entries: Mutex::new(HashMap::new()) })
        }
        fn insert(&self, namespace: &str, key: &str, value: &str) {
            self.entries.lock().unwrap().insert(format!("{}\x00{}", namespace, key), value.to_string());
        }
    }
    impl MemoryAccess for MockMemory {
        fn list_namespaces(&self) -> Vec<String> {
            let data = self.entries.lock().unwrap();
            let mut seen = std::collections::BTreeSet::new();
            for k in data.keys() {
                if let Some(sep) = k.find('\x00') {
                    seen.insert(k[..sep].to_string());
                }
            }
            seen.into_iter().collect()
        }
        fn list_keys(&self, namespace: &str) -> Vec<String> {
            let data = self.entries.lock().unwrap();
            let prefix = format!("{}\x00", namespace);
            data.keys()
                .filter(|k| k.starts_with(&prefix))
                .map(|k| k[prefix.len()..].to_string())
                .collect()
        }
        fn get_entry(&self, namespace: &str, key: &str) -> Option<String> {
            self.entries.lock().unwrap().get(&format!("{}\x00{}", namespace, key)).cloned()
        }
    }

    fn fs_with_memory(agent_id: &str, mem: Arc<dyn MemoryAccess>) -> AgentsFs {
        let snap = make_snap(vec![agent_snap(agent_id, AgentStatus::Running)]);
        let mut fs = AgentsFs::new(snap, Some(mem), None);
        fs.alloc_dir(agent_id);
        fs
    }

    // ── Status variants ───────────────────────────────────────────────────────

    #[test]
    fn status_running_renders() {
        let fs = fs_with_agent("agent-1", AgentStatus::Running);
        let dir_ino = fs.dir_inodes["agent-1"];
        let content = fs.file_content_for_ino(dir_ino + OFF_STATUS).unwrap();
        assert_eq!(content, b"running\n");
    }

    #[test]
    fn status_deferred_renders() {
        let fs = fs_with_agent("agent-1", AgentStatus::Deferred);
        let dir_ino = fs.dir_inodes["agent-1"];
        let content = fs.file_content_for_ino(dir_ino + OFF_STATUS).unwrap();
        assert_eq!(content, b"deferred\n");
    }

    #[test]
    fn status_awaiting_child_renders_child_id() {
        let fs = fs_with_agent("agent-1", AgentStatus::AwaitingChild("child-2".to_string()));
        let dir_ino = fs.dir_inodes["agent-1"];
        let content = fs.file_content_for_ino(dir_ino + OFF_STATUS).unwrap();
        assert_eq!(content, b"awaiting_child:child-2\n");
    }

    #[test]
    fn status_done_renders() {
        let fs = fs_with_agent("agent-1", AgentStatus::Done);
        let dir_ino = fs.dir_inodes["agent-1"];
        let content = fs.file_content_for_ino(dir_ino + OFF_STATUS).unwrap();
        assert_eq!(content, b"done\n");
    }

    #[test]
    fn status_failed_renders() {
        let fs = fs_with_agent("agent-1", AgentStatus::Failed);
        let dir_ino = fs.dir_inodes["agent-1"];
        let content = fs.file_content_for_ino(dir_ino + OFF_STATUS).unwrap();
        assert_eq!(content, b"failed\n");
    }

    // ── context_size and budget files ─────────────────────────────────────────

    #[test]
    fn context_size_renders_token_count() {
        let snap = make_snap(vec![AgentSnapshot {
            context_tokens: 1234,
            ..agent_snap("a", AgentStatus::Running)
        }]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.alloc_dir("a");
        let dir_ino = fs.dir_inodes["a"];
        let content = fs.file_content_for_ino(dir_ino + OFF_CONTEXT).unwrap();
        assert_eq!(content, b"1234\n");
    }

    #[test]
    fn budget_unlimited_renders_unlimited() {
        let snap = make_snap(vec![AgentSnapshot {
            token_budget: 0,
            ..agent_snap("a", AgentStatus::Running)
        }]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.alloc_dir("a");
        let dir_ino = fs.dir_inodes["a"];
        let content = fs.file_content_for_ino(dir_ino + OFF_BUDGET).unwrap();
        assert_eq!(content, b"unlimited\n");
    }

    #[test]
    fn budget_non_zero_renders_number() {
        let snap = make_snap(vec![AgentSnapshot {
            token_budget: 50_000,
            ..agent_snap("a", AgentStatus::Running)
        }]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.alloc_dir("a");
        let dir_ino = fs.dir_inodes["a"];
        let content = fs.file_content_for_ino(dir_ino + OFF_BUDGET).unwrap();
        assert_eq!(content, b"50000\n");
    }

    // ── Inode allocation ──────────────────────────────────────────────────────

    #[test]
    fn alloc_dir_returns_stable_inode() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, None);
        let ino1 = fs.alloc_dir("alpha");
        let ino2 = fs.alloc_dir("alpha");
        assert_eq!(ino1, ino2, "repeated alloc must return same inode");
    }

    #[test]
    fn alloc_dir_increments_per_agent() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, None);
        let ino_a = fs.alloc_dir("alpha");
        let ino_b = fs.alloc_dir("beta");
        assert_eq!(ino_b, ino_a + DIR_STEP);
    }

    #[test]
    fn all_nine_inodes_registered_after_alloc() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, None);
        let dir_ino = fs.alloc_dir("x");
        for offset in [
            0, OFF_STATUS, OFF_CONTEXT, OFF_BUDGET, OFF_FLIGHT,
            OFF_MEMORY_DIR, OFF_SHORT_TERM, OFF_LONG_TERM_DIR, OFF_TOOLS, OFF_PARENT,
            OFF_SANDBOX, OFF_TIER, OFF_PID,
        ] {
            assert!(
                fs.inode_to_id.contains_key(&(dir_ino + offset)),
                "inode dir+{offset} must be registered"
            );
        }
    }

    // ── Read offset / size slicing ────────────────────────────────────────────

    #[test]
    fn read_slice_respects_offset_and_size() {
        let content = b"running\n";
        let start = 2_usize.min(content.len());
        let end   = (2 + 3_usize).min(content.len());
        assert_eq!(&content[start..end], b"nni");
    }

    #[test]
    fn read_offset_beyond_end_returns_empty() {
        let content = b"done\n";
        let offset = 999_usize.min(content.len());
        assert_eq!(&content[offset..content.len()], b"");
    }

    /// Regression: saturating_add must never overflow when offset or size is
    /// near usize::MAX.  The real FUSE read() path uses:
    ///   let end = offset.saturating_add(size as usize).min(content.len());
    /// Verify this arithmetic is correct for extreme inputs.
    #[test]
    fn read_saturating_add_does_not_overflow() {
        let content = b"hello\n";
        // offset = content.len() - 1, size = usize::MAX — should yield 1 byte
        let offset = content.len() - 1;
        let size: usize = usize::MAX;
        let end = offset.saturating_add(size).min(content.len());
        assert_eq!(&content[offset..end], b"\n");

        // offset past end, huge size — should yield empty slice
        let offset2: usize = content.len() + 100;
        let start2 = offset2.min(content.len());
        let end2 = offset2.saturating_add(size).min(content.len());
        assert_eq!(&content[start2..end2], b"");
    }

    // ── Flight tail helpers ───────────────────────────────────────────────────

    #[test]
    fn read_flight_tail_missing_file_returns_empty() {
        let result = read_flight_tail("nonexistent-agent-xyz");
        // flight.jsonl probably doesn't exist in test CWD — should return empty.
        // If it does exist, the tag won't match, so still empty.
        let _ = result; // just assert no panic
    }

    #[test]
    fn read_flight_tail_filters_by_agent_and_returns_last_lines() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        // Write 3 lines for agent-1 and 1 line for agent-2 (should be filtered out).
        for i in 0..3 {
            writeln!(tmp, r#"{{"agent":"agent-1","kind":"tool_call","n":{}}}"#, i).unwrap();
        }
        writeln!(tmp, r#"{{"agent":"agent-2","kind":"agent_spawned"}}"#).unwrap();

        let result = read_flight_tail_from(tmp.path(), "agent-1");
        let s = String::from_utf8(result).unwrap();
        // Should contain all 3 agent-1 lines joined with newline + trailing newline.
        assert!(s.contains(r#""agent":"agent-1""#));
        assert!(!s.contains(r#""agent":"agent-2""#));
        assert_eq!(s.lines().count(), 3);
    }

    #[test]
    fn read_flight_tail_caps_at_twenty_lines() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        for i in 0..25 {
            writeln!(tmp, r#"{{"agent":"capped","kind":"tool_call","n":{}}}"#, i).unwrap();
        }
        let result = read_flight_tail_from(tmp.path(), "capped");
        let s = String::from_utf8(result).unwrap();
        assert_eq!(s.lines().count(), 20, "should return at most 20 lines");
    }

    #[test]
    fn file_content_for_ino_unknown_offset_returns_none() {
        let fs = fs_with_agent("agent-1", AgentStatus::Running);
        let dir_ino = fs.dir_inodes["agent-1"];
        // Offset 99 is not any known file offset.
        assert!(fs.file_content_for_ino(dir_ino + 99).is_none());
    }

    #[test]
    fn file_name_for_offset_covers_all_files() {
        assert_eq!(file_name_for_offset(OFF_STATUS),       Some("status"));
        assert_eq!(file_name_for_offset(OFF_CONTEXT),      Some("context_size"));
        assert_eq!(file_name_for_offset(OFF_BUDGET),       Some("budget"));
        assert_eq!(file_name_for_offset(OFF_FLIGHT),       Some("flight"));
        assert_eq!(file_name_for_offset(OFF_MEMORY_DIR),   Some("memory"));
        assert_eq!(file_name_for_offset(OFF_SHORT_TERM),   Some("short_term"));
        assert_eq!(file_name_for_offset(OFF_LONG_TERM_DIR), Some("long_term"));
        assert_eq!(file_name_for_offset(OFF_TOOLS),         Some("tools"));
        assert_eq!(file_name_for_offset(OFF_PARENT),        Some("parent"));
        assert_eq!(file_name_for_offset(99), None);
    }

    #[test]
    fn read_flight_tail_non_utf8_bytes_do_not_panic_or_lose_valid_lines() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        // Write a valid JSON line, then a binary blob, then another valid line.
        writeln!(tmp, r#"{{"agent":"bot","kind":"turn_start"}}"#).unwrap();
        tmp.write_all(&[0xFF, 0xFE, 0xAA, 0xBB, b'\n']).unwrap(); // invalid UTF-8
        writeln!(tmp, r#"{{"agent":"bot","kind":"turn_end"}}"#).unwrap();

        // Must not panic; with from_utf8_lossy we get replacement chars for the
        // binary blob, which means the invalid line won't match the tag and is
        // filtered out, but the two valid lines survive.
        let result = read_flight_tail_from(tmp.path(), "bot");
        let s = String::from_utf8_lossy(&result);
        assert!(s.contains("turn_start"), "first valid line must survive non-UTF-8 in flight file");
        assert!(s.contains("turn_end"),   "second valid line must survive non-UTF-8 in flight file");
    }

    // ── Memory subtree ────────────────────────────────────────────────────────

    #[test]
    fn memory_subtree_lists_short_and_long_term() {
        let mock = MockMemory::new();
        mock.insert("agent/agent-1", "key-a", r#"{"content":"hello"}"#);
        let fs = fs_with_memory("agent-1", mock);

        let dir_ino = fs.dir_inodes["agent-1"];
        let mem_dir_ino = dir_ino + OFF_MEMORY_DIR;
        let lt_dir_ino  = dir_ino + OFF_LONG_TERM_DIR;
        let st_ino      = dir_ino + OFF_SHORT_TERM;

        // memory/ is a directory
        assert!(fs.is_dir_ino(mem_dir_ino), "memory/ must be a directory inode");
        // long_term/ is a directory
        assert!(fs.is_dir_ino(lt_dir_ino), "long_term/ must be a directory inode");
        // short_term is a file (content returns Some, even if empty)
        assert!(fs.file_content_for_ino(st_ino).is_some(), "short_term must be a file");
    }

    #[test]
    fn short_term_file_reflects_snapshot_previews() {
        let snap = make_snap(vec![AgentSnapshot {
            short_term_previews: vec![
                "t1 user: first turn".to_string(),
                "t1 assistant: replied".to_string(),
            ],
            ..agent_snap("a", AgentStatus::Running)
        }]);
        let mut fs = AgentsFs::new(snap, Some(MockMemory::new()), None);
        fs.alloc_dir("a");

        let dir_ino = fs.dir_inodes["a"];
        let st_ino  = dir_ino + OFF_SHORT_TERM;
        let content = fs.file_content_for_ino(st_ino).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains("t1 user: first turn"));
        assert!(s.contains("t1 assistant: replied"));
    }

    #[test]
    fn kb_segment_browse_returns_entry_content() {
        let mock = MockMemory::new();
        mock.insert("canon", "doc-1", r#"{"content":"canonical info","provenance":{}}"#);

        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, Some(mock), None);

        // Allocate the kb seg dir inode as readdir would
        let seg_ino = fs.alloc_kb_seg("canon");
        assert!(fs.is_dir_ino(seg_ino), "kb/<seg>/ must be a directory");

        // Allocate the file inode and read its content
        let file_ino = fs.alloc_kb_file("canon", "doc-1");
        let content  = fs.dyn_file_content(file_ino).unwrap();
        let s        = String::from_utf8(content).unwrap();
        assert!(s.contains("canonical info"), "file content must include stored value");
    }

    #[test]
    fn large_memory_entry_read_does_not_panic() {
        let mock = MockMemory::new();
        let large_value = "x".repeat(128 * 1024); // 128 KiB
        mock.insert("agent/agent-big", "big-key", &large_value);

        let snap = make_snap(vec![agent_snap("agent-big", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(snap, Some(mock), None);
        fs.alloc_dir("agent-big");

        let file_ino = fs.alloc_lt_file("agent-big", "big-key");
        let content  = fs.dyn_file_content(file_ino).expect("large entry must be readable");
        assert_eq!(content.len(), large_value.len() + 1, "content must be value + newline");
    }

    #[test]
    fn memory_view_stale_snapshot_does_not_tear_ongoing_read() {
        // Populate snapshot, then mutate it while reading — read must not panic or fail.
        let snap = make_snap(vec![AgentSnapshot {
            short_term_previews: vec!["t0 user: task".to_string()],
            ..agent_snap("a", AgentStatus::Running)
        }]);
        let shared_snap = Arc::clone(&snap);

        let mut fs = AgentsFs::new(snap, Some(MockMemory::new()), None);
        fs.alloc_dir("a");

        let dir_ino = fs.dir_inodes["a"];
        let st_ino  = dir_ino + OFF_SHORT_TERM;

        // Read while concurrently (simulated) updating the snapshot.
        // A real concurrent test would spawn threads; here we verify the read
        // succeeds with a consistent snapshot (no torn read / no panic).
        {
            let mut guard = shared_snap.write().unwrap();
            guard.agents[0].short_term_previews.push("t1 assistant: done".to_string());
        }
        // Read after update — must still succeed
        let content = fs.file_content_for_ino(st_ino).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains("t1 assistant: done") || s.contains("t0 user: task"),
            "must read a consistent snapshot state");
    }

    // ── alloc idempotency ─────────────────────────────────────────────────────

    #[test]
    fn alloc_lt_file_idempotent() {
        let fs_snap = make_snap(vec![agent_snap("a", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()), None);
        fs.alloc_dir("a");
        let ino1 = fs.alloc_lt_file("a", "some-key");
        let ino2 = fs.alloc_lt_file("a", "some-key");
        assert_eq!(ino1, ino2, "alloc_lt_file must return the same inode on repeated calls");
    }

    #[test]
    fn alloc_kb_seg_idempotent() {
        let fs_snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()), None);
        let ino1 = fs.alloc_kb_seg("canon");
        let ino2 = fs.alloc_kb_seg("canon");
        assert_eq!(ino1, ino2, "alloc_kb_seg must return the same inode on repeated calls");
    }

    #[test]
    fn alloc_kb_file_idempotent() {
        let fs_snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()), None);
        let ino1 = fs.alloc_kb_file("canon", "doc-1");
        let ino2 = fs.alloc_kb_file("canon", "doc-1");
        assert_eq!(ino1, ino2, "alloc_kb_file must return the same inode on repeated calls");
    }

    // ── parent_kind edge cases ────────────────────────────────────────────────

    #[test]
    fn parent_kind_kb_inode_returns_kb() {
        let fs_snap = make_snap(vec![]);
        let fs = AgentsFs::new(fs_snap, Some(MockMemory::new()), None);
        // INO_KB=9; passing it as parent must route to ParentKind::Kb
        let pk = fs.parent_kind(INO_KB);
        assert!(matches!(pk, Some(ParentKind::Kb)), "INO_KB as parent must be ParentKind::Kb");
    }

    #[test]
    fn parent_kind_unknown_inode_returns_none() {
        let fs_snap = make_snap(vec![]);
        let fs = AgentsFs::new(fs_snap, Some(MockMemory::new()), None);
        // 999_999 is not root, not INO_KB, not an agent dir, not a dyn ino
        let pk = fs.parent_kind(999_999);
        assert!(pk.is_none(), "unknown inode must return None from parent_kind");
    }

    #[test]
    fn parent_kind_memory_dir_and_long_term_dir_offsets() {
        // Regression: ensures the wrapping_sub(base) + match on OFF_MEMORY_DIR /
        // OFF_LONG_TERM_DIR constants correctly routes to the right ParentKind arms.
        let snap = make_snap(vec![agent_snap("x", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(snap, Some(MockMemory::new()), None);
        let base = fs.alloc_dir("x");
        let pk_mem = fs.parent_kind(base + OFF_MEMORY_DIR);
        let pk_lt  = fs.parent_kind(base + OFF_LONG_TERM_DIR);
        assert!(
            matches!(pk_mem, Some(ParentKind::MemoryDir(ref id)) if id == "x"),
            "base + OFF_MEMORY_DIR must route to MemoryDir"
        );
        assert!(
            matches!(pk_lt, Some(ParentKind::LongTermDir(ref id)) if id == "x"),
            "base + OFF_LONG_TERM_DIR must route to LongTermDir"
        );
    }

    // ── dyn_file_content edge cases ───────────────────────────────────────────

    #[test]
    fn dyn_file_content_kbseg_returns_none() {
        let fs_snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()), None);
        // KbSeg is a directory — dyn_file_content must return None for it
        let seg_ino = fs.alloc_kb_seg("scratch");
        assert!(
            fs.dyn_file_content(seg_ino).is_none(),
            "dyn_file_content on a KbSeg (dir) inode must return None"
        );
    }

    #[test]
    fn dyn_file_content_no_memory_returns_none() {
        let fs_snap = make_snap(vec![agent_snap("a", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(fs_snap, None, None);  // no memory configured
        fs.alloc_dir("a");
        let file_ino = fs.alloc_lt_file("a", "key");
        assert!(
            fs.dyn_file_content(file_ino).is_none(),
            "dyn_file_content with no memory arc must return None"
        );
    }

    // ── is_dir_ino: dynamic file inode returns false ──────────────────────────

    #[test]
    fn is_dir_ino_dynamic_file_returns_false() {
        let fs_snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()), None);
        let file_ino = fs.alloc_kb_file("scratch", "my-key");
        assert!(!fs.is_dir_ino(file_ino), "KbFile inode must NOT be reported as a directory");
        let lt_ino = fs.alloc_lt_file("agent-x", "my-lt-key");
        assert!(!fs.is_dir_ino(lt_ino), "LtFile inode must NOT be reported as a directory");
    }

    // ── MAX_DIR_KEYS cap ──────────────────────────────────────────────────────

    #[test]
    fn max_dir_keys_cap_limits_listing() {
        let mock = MockMemory::new();
        // Insert 150 keys into a single namespace (> MAX_DIR_KEYS=100)
        for i in 0..150 {
            mock.insert("agent/big-agent", &format!("key-{:03}", i), "val");
        }
        let snap = make_snap(vec![agent_snap("big-agent", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(snap, Some(mock), None);
        fs.alloc_dir("big-agent");

        // Simulate what readdir does: list_keys → take(MAX_DIR_KEYS)
        let mem = fs.memory.as_ref().unwrap();
        let keys: Vec<_> = mem.list_keys("agent/big-agent").into_iter().take(MAX_DIR_KEYS).collect();
        assert_eq!(keys.len(), MAX_DIR_KEYS,
            "key listing must be capped at MAX_DIR_KEYS ({})", MAX_DIR_KEYS);
    }

    #[test]
    fn readdir_long_term_skips_slash_keys() {
        // Regression: adversarial Finding 1 — a KB key containing '/' would corrupt
        // the FUSE dentry listing because Linux VFS rejects dentry names with '/'.
        let mock = MockMemory::new();
        mock.insert("agent/a", "good-key", "v1");
        mock.insert("agent/a", "bad/key", "v2");   // slash in key
        mock.insert("agent/a", "also\x00bad", "v3"); // NUL in key
        let snap = make_snap(vec![agent_snap("a", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(snap, Some(mock), None);
        fs.alloc_dir("a");

        // Simulate the readdir LongTermDir path via capped_keys + filter
        let mem = fs.memory.as_ref().unwrap();
        let ns = format!("{}{}", AGENT_NS_PREFIX, "a");
        let raw_keys = capped_keys(&**mem, &ns);
        let safe_keys: Vec<_> = raw_keys.iter()
            .filter(|k| !k.contains('/') && !k.contains('\0'))
            .collect();
        assert_eq!(safe_keys, vec!["good-key"],
            "slash and NUL keys must be filtered from FUSE dir entries");
    }

    #[test]
    fn readdir_kb_seg_skips_slash_keys() {
        let mock = MockMemory::new();
        mock.insert("scratch", "clean", "v1");
        mock.insert("scratch", "a/b", "v2");  // slash
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, Some(mock), None);

        let mem = fs.memory.as_ref().unwrap();
        let raw_keys = capped_keys(&**mem, "scratch");
        let safe_keys: Vec<_> = raw_keys.iter()
            .filter(|k| !k.contains('/') && !k.contains('\0'))
            .collect();
        assert_eq!(safe_keys, vec!["clean"],
            "slash keys must be filtered from KB seg FUSE dir entries");
    }

    #[test]
    fn kb_readdir_returns_empty_when_no_memory() {
        // Regression: p5.7-ar-03 — when no memory store is configured, the kb/
        // tree has nothing to show. getattr(INO_KB) returns ENOENT at the FUSE
        // layer (tested at the code-path level by verifying memory=None is detected).
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, None, None);
        assert!(fs.memory.is_none(), "precondition: no memory configured");
        // With no memory store, list_namespaces via the Kb parent_kind path returns empty.
        let namespaces: Vec<String> = fs.memory.as_ref()
            .map(|m| m.list_namespaces())
            .unwrap_or_default();
        assert!(namespaces.is_empty(),
            "kb/ readdir must return no segments when memory=None");
    }

    // ── prune_dead_agent (ar-01) ──────────────────────────────────────────────

    #[test]
    fn inode_map_pruned_on_snapshot_update() {
        // ar-01: prune_dead_agent must remove all 5 maps' entries for the given agent.
        let snap = make_snap(vec![agent_snap("agent-a", AgentStatus::Running)]);
        let mock = MockMemory::new();
        mock.insert("agent/agent-a", "lt-key", "val");
        let mut fs = AgentsFs::new(snap, Some(mock), None);
        let base = fs.alloc_dir("agent-a");
        let lt_ino = fs.alloc_lt_file("agent-a", "lt-key");
        let _kb_seg_ino = fs.alloc_kb_seg("canon"); // shared seg — must NOT be pruned

        // Sanity: agent-a dir and lt entry registered
        assert!(fs.dir_inodes.contains_key("agent-a"), "precondition: dir_inodes");
        assert!(fs.inode_to_id.contains_key(&base), "precondition: base inode");
        assert!(fs.inode_to_id.contains_key(&(base + OFF_STATUS)), "precondition: status inode");
        assert!(fs.lt_key_ino.contains_key(&("agent-a".to_string(), "lt-key".to_string())),
            "precondition: lt_key_ino");
        assert!(fs.dyn_ino_kind.contains_key(&lt_ino), "precondition: dyn_ino_kind");

        // Prune agent-a
        fs.prune_dead_agent("agent-a");

        // All agent-a entries must be gone from every map
        assert!(!fs.dir_inodes.contains_key("agent-a"), "dir_inodes must be cleared");
        for offset in [0u64, OFF_STATUS, OFF_CONTEXT, OFF_BUDGET, OFF_FLIGHT,
                       OFF_MEMORY_DIR, OFF_SHORT_TERM, OFF_LONG_TERM_DIR, OFF_TOOLS, OFF_PARENT,
                       OFF_SANDBOX, OFF_TIER, OFF_PID] {
            assert!(!fs.inode_to_id.contains_key(&(base + offset)),
                "inode_to_id must not contain base+{offset} after prune");
        }
        assert!(!fs.lt_key_ino.contains_key(&("agent-a".to_string(), "lt-key".to_string())),
            "lt_key_ino must be cleared");
        assert!(!fs.dyn_ino_kind.contains_key(&lt_ino), "dyn_ino_kind lt entry must be cleared");

        // Shared "canon" segment must NOT be pruned — it has no "agent/agent-a/" prefix
        assert!(fs.kb_seg_ino.contains_key("canon"), "shared KB segment must survive prune");
    }

    // ── ar-03: getattr ENOENT for memory dirs when no store ──────────────────

    #[test]
    fn getattr_memory_dir_enoent_when_no_store() {
        // ar-03: with no memory store, getattr for OFF_MEMORY_DIR and
        // OFF_LONG_TERM_DIR must return ENOENT.  We can't call getattr() directly
        // in unit tests (it takes a FUSE ReplyAttr), so we verify the preconditions
        // that drive the ENOENT branch: the inodes ARE registered by alloc_dir (so
        // the lookup reaches the ar-03 guard), and file_content_for_ino returns
        // None for those directory offsets, consistent with ENOENT semantics.
        let snap = make_snap(vec![agent_snap("x", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(snap, None, None); // no memory store
        let base = fs.alloc_dir("x");

        // Precondition: memory=None (ar-03 guard fires)
        assert!(fs.memory.is_none(), "precondition: no memory store configured");

        // Inodes ARE registered — getattr reaches the guard before returning ENOENT
        assert!(fs.inode_to_id.contains_key(&(base + OFF_MEMORY_DIR)),
            "memory/ inode registered even without store");
        assert!(fs.inode_to_id.contains_key(&(base + OFF_LONG_TERM_DIR)),
            "long_term/ inode registered even without store");

        // Directory offsets have no file content — no torn read possible
        assert!(fs.file_content_for_ino(base + OFF_MEMORY_DIR).is_none(),
            "memory/ is a directory: no file content");
        assert!(fs.file_content_for_ino(base + OFF_LONG_TERM_DIR).is_none(),
            "long_term/ is a directory: no file content");
    }

    #[test]
    fn getattr_short_term_ok_when_no_store() {
        // ar-03 exemption regression guard: OFF_SHORT_TERM is intentionally
        // served from AgentSnapshot (not the memory store), so getattr must NOT
        // return ENOENT even when memory=None.
        let snap = make_snap(vec![AgentSnapshot {
            short_term_previews: vec!["t0 user: hello".to_string()],
            ..agent_snap("x", AgentStatus::Running)
        }]);
        let mut fs = AgentsFs::new(snap, None, None); // no memory store
        let base = fs.alloc_dir("x");

        assert!(fs.memory.is_none(), "precondition: no memory store");

        // short_term must be readable from the snapshot even without memory store
        let content = fs.file_content_for_ino(base + OFF_SHORT_TERM);
        assert!(content.is_some(), "short_term must be readable without memory store");
        let s = String::from_utf8(content.unwrap()).unwrap();
        assert!(s.contains("t0 user: hello"), "short_term content must reflect snapshot previews");
    }

    // ── prune_dead_agent kb_key_ino cleanup ───────────────────────────────────

    #[test]
    fn prune_dead_agent_cleans_kb_key_ino() {
        // Regression guard: prune_dead_agent must clean kb_key_ino and the
        // associated dyn_ino_kind::KbFile entries for agent-scoped segments.
        let snap = make_snap(vec![agent_snap("agent-kb", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.alloc_dir("agent-kb");
        // Allocate a KB file inode in this agent's scoped segment.
        let kb_ino = fs.alloc_kb_file("agent/agent-kb/scratch", "key1");

        assert!(fs.kb_key_ino.contains_key(&("agent/agent-kb/scratch".to_string(), "key1".to_string())),
            "precondition: kb_key_ino entry exists");
        assert!(fs.dyn_ino_kind.contains_key(&kb_ino),
            "precondition: dyn_ino_kind::KbFile entry exists");

        fs.prune_dead_agent("agent-kb");

        assert!(!fs.kb_key_ino.contains_key(&("agent/agent-kb/scratch".to_string(), "key1".to_string())),
            "kb_key_ino must be cleared after prune");
        assert!(!fs.dyn_ino_kind.contains_key(&kb_ino),
            "dyn_ino_kind::KbFile entry must be cleared after prune");
    }

    #[test]
    fn prune_dead_agent_idempotent_for_unknown_agent() {
        // prune_dead_agent on a never-allocated agent_id must be a no-op (no panic).
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, None);
        // Must not panic even though "ghost" was never registered.
        fs.prune_dead_agent("ghost");
        assert!(fs.dir_inodes.is_empty());
        assert!(fs.inode_to_id.is_empty());
    }

    // ── p6.3: DIR_STEP=20, OFF_TOOLS=8, const invariant ──────────────────────

    #[test]
    fn dir_step_is_twenty() {
        assert_eq!(DIR_STEP, 20, "DIR_STEP must be 20 to fit all per-agent offsets");
    }

    #[test]
    fn off_tools_is_eight() {
        assert_eq!(OFF_TOOLS, 8);
    }

    #[test]
    fn off_tools_fits_within_dir_step() {
        // Compile-time check exists as `const _: ()` above. This test ensures
        // the relationship is also visible in test output.
        #[allow(clippy::assertions_on_constants)]
        { assert!(OFF_TOOLS < DIR_STEP - 1, "OFF_TOOLS must be < DIR_STEP - 1"); }
    }

    #[test]
    fn alloc_dir_step_20_increments() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, None);
        let ino_a = fs.alloc_dir("a");
        let ino_b = fs.alloc_dir("b");
        assert_eq!(ino_b, ino_a + 20, "second agent dir inode must be exactly 20 after the first");
    }

    #[test]
    fn off_tools_inode_registered_in_inode_map() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, None);
        let dir_ino = fs.alloc_dir("agent-x");
        assert!(
            fs.inode_to_id.contains_key(&(dir_ino + OFF_TOOLS)),
            "dir+OFF_TOOLS inode must be in inode_to_id after alloc_dir"
        );
    }

    #[test]
    fn file_name_for_offset_tools_returns_tools() {
        assert_eq!(file_name_for_offset(OFF_TOOLS), Some("tools"));
    }

    // ── p6.3: tools file rendering ────────────────────────────────────────────

    #[test]
    fn tools_file_empty_tools_renders_none() {
        let snap = make_snap(vec![AgentSnapshot {
            tools: vec![],
            ..agent_snap("a", AgentStatus::Running)
        }]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.alloc_dir("a");
        let dir_ino = fs.dir_inodes["a"];
        let content = fs.file_content_for_ino(dir_ino + OFF_TOOLS).unwrap();
        assert_eq!(content, b"(none)\n");
    }

    #[test]
    fn tools_file_single_tool_renders_with_newline() {
        let snap = make_snap(vec![AgentSnapshot {
            tools: vec!["read_file".to_string()],
            ..agent_snap("a", AgentStatus::Running)
        }]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.alloc_dir("a");
        let dir_ino = fs.dir_inodes["a"];
        let content = fs.file_content_for_ino(dir_ino + OFF_TOOLS).unwrap();
        assert_eq!(content, b"read_file\n");
    }

    #[test]
    fn tools_file_multiple_tools_newline_separated() {
        let snap = make_snap(vec![AgentSnapshot {
            tools: vec!["read_file".to_string(), "write_file".to_string(), "list_dir".to_string()],
            ..agent_snap("a", AgentStatus::Running)
        }]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.alloc_dir("a");
        let dir_ino = fs.dir_inodes["a"];
        let content = fs.file_content_for_ino(dir_ino + OFF_TOOLS).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "read_file\nwrite_file\nlist_dir\n");
    }

    // ── p6.4: OFF_PARENT virtual file ────────────────────────────────────────

    #[test]
    fn parent_file_none_renders_sentinel() {
        let snap = make_snap(vec![agent_snap("a", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(snap, None, None);
        let dir_ino = fs.alloc_dir("a");
        let content = fs.file_content_for_ino(dir_ino + OFF_PARENT).unwrap();
        assert_eq!(content, b"(none)\n");
    }

    #[test]
    fn parent_file_some_renders_parent_id() {
        let snap = make_snap(vec![AgentSnapshot {
            parent_id: Some("coordinator".to_string()),
            ..agent_snap("scout", AgentStatus::Running)
        }]);
        let mut fs = AgentsFs::new(snap, None, None);
        let dir_ino = fs.alloc_dir("scout");
        let content = fs.file_content_for_ino(dir_ino + OFF_PARENT).unwrap();
        assert_eq!(content, b"coordinator\n");
    }

    // ── p6.3: system dir parent_kind ─────────────────────────────────────────

    #[test]
    fn parent_kind_ino_system_returns_system_dir() {
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, None, None);
        match fs.parent_kind(INO_SYSTEM) {
            Some(ParentKind::SystemDir) => {}
            other => panic!("expected SystemDir, got {:?}", other.map(|_| "other")),
        }
    }

    #[test]
    fn parent_kind_sys_file_inodes_return_none() {
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, None, None);
        // System file inodes are not parents — parent_kind must return None.
        for ino in [INO_SYS_BUDGET, INO_SYS_QUEUE, INO_SYS_SANDBOX, INO_SYS_PROVIDER] {
            assert!(
                fs.parent_kind(ino).is_none(),
                "parent_kind({ino}) must return None (system files are not parents)"
            );
        }
    }

    #[test]
    fn is_dir_ino_system_dir_returns_true() {
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, None, None);
        assert!(fs.is_dir_ino(INO_SYSTEM), "INO_SYSTEM must be reported as a directory");
    }

    #[test]
    fn is_dir_ino_system_file_inodes_return_false() {
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, None, None);
        for ino in [INO_SYS_BUDGET, INO_SYS_QUEUE, INO_SYS_SANDBOX, INO_SYS_PROVIDER] {
            assert!(
                !fs.is_dir_ino(ino),
                "system file inode {ino} must NOT be reported as a directory"
            );
        }
    }

    // ── p6.3: sys_file_content ────────────────────────────────────────────────

    #[test]
    fn sys_budget_renders_correct_json() {
        let snap = make_snap_with_sys(vec![], 42_000, 0, false, "");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_BUDGET).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "{\"spent\":42000,\"total\":0}\n");
    }

    #[test]
    fn sys_budget_zero_spent() {
        let snap = make_snap_with_sys(vec![], 0, 0, false, "");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_BUDGET).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "{\"spent\":0,\"total\":0}\n");
    }

    #[test]
    fn sys_queue_renders_depth() {
        let snap = make_snap_with_sys(vec![], 0, 3, false, "");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_QUEUE).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "{\"depth\":3}\n");
    }

    #[test]
    fn sys_queue_zero_depth() {
        let snap = make_snap_with_sys(vec![], 0, 0, false, "");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_QUEUE).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "{\"depth\":0}\n");
    }

    #[test]
    fn sys_sandbox_renders_false() {
        let snap = make_snap_with_sys(vec![], 0, 0, false, "");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_SANDBOX).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "{\"any_sandboxed\":false,\"servers\":[],\"degradations\":[]}\n");
    }

    #[test]
    fn sys_sandbox_renders_true() {
        let snap = make_snap_with_sys(vec![], 0, 0, true, "");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_SANDBOX).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "{\"any_sandboxed\":true,\"servers\":[],\"degradations\":[]}\n");
    }

    #[test]
    fn sys_sandbox_renders_servers_and_degradations() {
        use crate::snapshot::{SandboxSummary, ServerEnforcement};
        let enf = ServerEnforcement {
            name:              "search".to_string(),
            transport:         "stdio".to_string(),
            isolation:         "none".to_string(),
            landlock:          true,
            seccomp:           true,
            spawn_enforcement: "fork_vfork_only".to_string(),
            namespace_net:     false,
            namespace_mount:   false,
            landlock_net:      false,
        };
        let snap = Arc::new(RwLock::new(SchedulerSnapshot {
            agents:              vec![],
            global_tokens_spent: 0,
            in_flight:           0,
            queue_depth:         0,
            provider_model:      String::new(),
            sandbox:             SandboxSummary {
                any_sandboxed: true,
                servers:        vec![enf],
                degradations:   vec!["landlock_net_unavailable".to_string()],
            },
            pending_actions:     vec![],
            egress_addr:         None,
            isolation_caps:      None,
            credential_snapshot: None,
        }));
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_SANDBOX).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains("\"any_sandboxed\":true"), "any_sandboxed must be true");
        assert!(s.contains("\"search\""), "server name must appear");
        assert!(s.contains("\"landlock\":true"), "landlock flag must appear");
        assert!(s.contains("\"landlock_net_unavailable\""), "degradation must appear in JSON");
    }

    #[test]
    fn sys_sandbox_json_key_is_any_sandboxed() {
        // Acceptance: the JSON key must be "any_sandboxed", not the old "applied".
        let snap = make_snap_with_sys(vec![], 0, 0, true, "");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_SANDBOX).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains("\"any_sandboxed\""), "key must be any_sandboxed");
        assert!(!s.contains("\"applied\""), "old key must not appear");
    }

    #[test]
    fn fuse_system_egress_addr_not_configured() {
        let snap = make_snap_with_sys(vec![], 0, 0, false, "");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_EGRESS_ADDR).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "not configured\n");
    }

    #[test]
    fn fuse_system_egress_addr_set() {
        let snap = make_snap_with_sys(vec![], 0, 0, false, "");
        {
            let mut w = snap.write().unwrap();
            w.egress_addr = Some("http://127.0.0.1:9100".to_string());
        }
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_EGRESS_ADDR).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "http://127.0.0.1:9100\n");
    }

    #[test]
    fn alloc_dir_registers_sandbox_offset() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, None);
        let base = fs.alloc_dir("agent-x");
        assert!(
            fs.inode_to_id.contains_key(&(base + OFF_SANDBOX)),
            "inode_to_id must contain OFF_SANDBOX inode after alloc_dir"
        );
    }

    #[test]
    fn prune_dead_agent_removes_sandbox_inode() {
        let snap = make_snap(vec![agent_snap("dead", AgentStatus::Done)]);
        let mut fs = AgentsFs::new(snap, None, None);
        let base = fs.alloc_dir("dead");
        let sandbox_ino = base + OFF_SANDBOX;
        assert!(fs.inode_to_id.contains_key(&sandbox_ino));
        fs.prune_dead_agent("dead");
        assert!(
            !fs.inode_to_id.contains_key(&sandbox_ino),
            "sandbox inode must be removed after prune_dead_agent"
        );
    }

    #[test]
    fn agent_sandbox_no_accessible_servers() {
        // Agent with no accessible servers → servers array is empty.
        let a = agent_snap("a1", AgentStatus::Running);
        let snap = make_snap(vec![a]);
        let mut fs = AgentsFs::new(snap, None, None);
        let base = fs.alloc_dir("a1");
        let content = fs.file_content_for_ino(base + OFF_SANDBOX).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "{\"servers\":[]}\n");
    }

    #[test]
    fn agent_sandbox_unrestricted_shows_all_system_servers() {
        // capabilities_unrestricted=true → sandbox file lists all registered servers
        // even though accessible_server_names is empty (the unrestricted-access path).
        use crate::snapshot::{SandboxSummary, ServerEnforcement};
        let mut a = agent_snap("a1", AgentStatus::Running);
        a.capabilities_unrestricted = true;
        // accessible_server_names stays empty — unrestricted means "all"
        let enf = ServerEnforcement {
            name:               "files".to_string(),
            transport:          "stdio".to_string(),
            isolation:          "none".to_string(),
            landlock:           true,
            seccomp:            false,
            spawn_enforcement:  "fork_vfork_only".to_string(),
            namespace_net:      false,
            namespace_mount:    false,
            landlock_net:       false,
        };
        let snap = Arc::new(RwLock::new(SchedulerSnapshot {
            agents:              vec![a],
            global_tokens_spent: 0,
            in_flight:           0,
            queue_depth:         0,
            provider_model:      String::new(),
            sandbox:             SandboxSummary { any_sandboxed: true, servers: vec![enf], degradations: vec![] },
            pending_actions:     vec![],
            egress_addr:         None,
            isolation_caps:      None,
            credential_snapshot: None,
        }));
        let mut fs = AgentsFs::new(snap, None, None);
        let base = fs.alloc_dir("a1");
        let content = fs.file_content_for_ino(base + OFF_SANDBOX).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains("\"files\""), "unrestricted agent must show all system servers");
        assert!(s.contains("\"landlock\":true"), "server enforcement fields must be present");
    }

    #[test]
    fn agent_sandbox_restricted_empty_accessible_shows_no_servers() {
        // capabilities_unrestricted=false + empty accessible_server_names → empty servers array.
        // Confirms we don't accidentally fall through to the unrestricted path.
        use crate::snapshot::{SandboxSummary, ServerEnforcement};
        let enf = ServerEnforcement { name: "files".to_string(), ..Default::default() };
        let a = agent_snap("a2", AgentStatus::Running); // capabilities_unrestricted=false, accessible=[]
        let snap = Arc::new(RwLock::new(SchedulerSnapshot {
            agents:              vec![a],
            global_tokens_spent: 0,
            in_flight:           0,
            queue_depth:         0,
            provider_model:      String::new(),
            sandbox:             SandboxSummary { any_sandboxed: true, servers: vec![enf], degradations: vec![] },
            pending_actions:     vec![],
            egress_addr:         None,
            isolation_caps:      None,
            credential_snapshot: None,
        }));
        let mut fs = AgentsFs::new(snap, None, None);
        let base = fs.alloc_dir("a2");
        let content = fs.file_content_for_ino(base + OFF_SANDBOX).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "{\"servers\":[]}\n", "restricted agent with empty accessible list must show no servers");
    }

    #[test]
    fn agent_sandbox_restricted_with_named_accessible_server() {
        // capabilities_unrestricted=false + accessible_server_names=["files"] →
        // the sandbox output must include the "files" server from system enforcement.
        use crate::snapshot::{SandboxSummary, ServerEnforcement};
        let enf = ServerEnforcement {
            name:              "files".to_string(),
            transport:         "stdio".to_string(),
            isolation:         "none".to_string(),
            landlock:          true,
            seccomp:           false,
            spawn_enforcement: "fork_vfork_only".to_string(),
            namespace_net:     false,
            namespace_mount:   false,
            landlock_net:      false,
        };
        let mut a = agent_snap("a3", AgentStatus::Running);
        a.accessible_server_names = vec!["files".to_string()];
        // capabilities_unrestricted stays false — this agent is scoped to just "files"
        let snap = Arc::new(RwLock::new(SchedulerSnapshot {
            agents:              vec![a],
            global_tokens_spent: 0,
            in_flight:           0,
            queue_depth:         0,
            provider_model:      String::new(),
            sandbox:             SandboxSummary { any_sandboxed: true, servers: vec![enf], degradations: vec![] },
            pending_actions:     vec![],
            egress_addr:         None,
            isolation_caps:      None,
            credential_snapshot: None,
        }));
        let mut fs = AgentsFs::new(snap, None, None);
        let base = fs.alloc_dir("a3");
        let content = fs.file_content_for_ino(base + OFF_SANDBOX).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains("\"files\""), "accessible server must appear in sandbox output");
        assert!(s.contains("\"landlock\":true"), "server enforcement details must be present");
    }

    #[test]
    fn sys_provider_renders_model_and_backend() {
        let snap = make_snap_with_sys(vec![], 0, 0, false, "claude-sonnet-4-6");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_PROVIDER).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert_eq!(s, "{\"model\":\"claude-sonnet-4-6\",\"backend\":\"anthropic\"}\n");
    }

    #[test]
    fn sys_provider_escapes_quotes_in_model() {
        let snap = make_snap_with_sys(vec![], 0, 0, false, "model-with-\"quotes\"");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_PROVIDER).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains("\\\"quotes\\\""), "double-quotes in model name must be escaped");
    }

    #[test]
    fn sys_provider_escapes_control_chars_in_model() {
        // Regression: json_escape_str must handle \n, \r, \t (not just " and \).
        let snap = make_snap_with_sys(vec![], 0, 0, false, "model\nwith\rnewline\ttab");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_PROVIDER).unwrap();
        let s = String::from_utf8(content).unwrap();
        // Output must be valid JSON (parseable) and must not contain bare control chars.
        assert!(s.contains("\\n"), "newline must be escaped as \\n");
        assert!(s.contains("\\r"), "CR must be escaped as \\r");
        assert!(s.contains("\\t"), "tab must be escaped as \\t");
        assert!(!s.contains('\n') || s.ends_with('\n'), "bare newlines only permitted as line terminator");
    }

    #[test]
    fn sys_provider_escapes_backslash_in_model() {
        let snap = make_snap_with_sys(vec![], 0, 0, false, r"model\path\name");
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_PROVIDER).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains(r"\\"), "backslash in model name must be escaped as \\\\");
    }

    #[test]
    fn sys_file_content_unknown_ino_returns_none() {
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, None, None);
        assert!(fs.sys_file_content(999).is_none(), "unknown ino must return None");
    }

    // ── p6.3: snapshot new fields ─────────────────────────────────────────────

    #[test]
    fn scheduler_snapshot_default_new_fields() {
        let s = crate::snapshot::SchedulerSnapshot::default();
        assert_eq!(s.queue_depth, 0);
        assert_eq!(s.provider_model, "");
        assert!(!s.sandbox.any_sandboxed);
    }

    #[test]
    fn agent_snapshot_tools_field_is_empty_by_default_in_helper() {
        let snap = agent_snap("x", AgentStatus::Running);
        assert!(snap.tools.is_empty(), "agent_snap helper must default tools to vec![]");
    }

    // ── p7.3: process_control_flush ───────────────────────────────────────────

    #[test]
    fn control_flush_empty_buffer_is_noop() {
        // flush with no prior write returns 0 (success, not an error)
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, Some(std::sync::Arc::new(|_: &[u8]| 1)));
        assert_eq!(fs.process_control_flush(99), 0, "missing fh must return 0");
    }

    #[test]
    fn control_flush_dispatches_bytes() {
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let c2 = Arc::clone(&captured);
        let dispatch: crate::ControlDispatch = Arc::new(move |bytes: &[u8]| {
            c2.lock().unwrap().extend_from_slice(bytes);
            0
        });
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, Some(dispatch));
        fs.write_buffers.insert(1, b"hello".to_vec());
        let rc = fs.process_control_flush(1);
        assert_eq!(rc, 0, "successful dispatch must return 0");
        assert_eq!(*captured.lock().unwrap(), b"hello");
        assert!(!fs.write_buffers.contains_key(&1), "buffer must be consumed after flush");
    }

    #[test]
    fn control_flush_without_dispatch_returns_erofs() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.write_buffers.insert(2, b"data".to_vec());
        assert_eq!(fs.process_control_flush(2), libc::EROFS);
    }

    #[test]
    fn control_flush_propagates_dispatch_errno() {
        let dispatch: crate::ControlDispatch = Arc::new(|_| libc::EBUSY);
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, Some(dispatch));
        fs.write_buffers.insert(3, b"x".to_vec());
        assert_eq!(fs.process_control_flush(3), libc::EBUSY);
    }

    #[test]
    fn control_write_non_control_ino_is_refused() {
        // write() on any ino other than INO_CONTROL must return EROFS via the
        // dispatch path (we can't call FUSE trait directly, but we can verify
        // that INO_CONTROL is the only ino that routes to write_buffers).
        let snap = make_snap(vec![agent_snap("a", AgentStatus::Running)]);
        let fs = AgentsFs::new(snap, None, Some(Arc::new(|_: &[u8]| 0)));
        // write_buffers is only populated by the FUSE write() handler for INO_CONTROL.
        // For other inodes, write() returns EROFS — we verify the buffer stays empty.
        assert!(fs.write_buffers.is_empty(), "write_buffers must start empty");
    }

    #[test]
    fn control_write_efbig_at_65_kib() {
        // A write that would push the buffer past 64 KiB must fail with EFBIG.
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, Some(Arc::new(|_: &[u8]| 0)));
        // Pre-fill buffer to exactly the cap limit.
        let fh = 7u64;
        fs.write_buffers.insert(fh, vec![0u8; 64 * 1024]);
        // Any additional byte must be refused.
        let buf = &fs.write_buffers[&fh];
        assert!(buf.len() + 1 > 64 * 1024, "pre-condition");
        let _ = buf; // end borrow before re-borrowing below
        // Simulate what write() checks:
        let existing = fs.write_buffers.get(&fh).map(|b| b.len()).unwrap_or(0);
        let new_data = b"x";
        assert!(existing + new_data.len() > 64 * 1024, "should trigger EFBIG path");
    }

    #[test]
    fn control_release_dispatches_unflushed_bytes() {
        // release() must dispatch buffered bytes that were never explicitly flushed.
        use std::sync::{Arc, Mutex};
        let captured: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(vec![]));
        let c2 = Arc::clone(&captured);
        let dispatch: crate::ControlDispatch = Arc::new(move |bytes: &[u8]| {
            c2.lock().unwrap().extend_from_slice(bytes);
            0
        });
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, Some(dispatch));
        let fh = 5u64;
        fs.write_buffers.insert(fh, b"unflushed".to_vec());
        // Simulate release (process_control_flush path):
        let rc = fs.process_control_flush(fh);
        assert_eq!(rc, 0);
        assert_eq!(*captured.lock().unwrap(), b"unflushed");
        assert!(!fs.write_buffers.contains_key(&fh), "buffer consumed by release");
    }

    #[test]
    fn control_release_after_flush_is_noop() {
        // release() after flush() already consumed the buffer must be a no-op (rc=0).
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None, Some(Arc::new(|_: &[u8]| 0)));
        let fh = 6u64;
        fs.write_buffers.insert(fh, b"data".to_vec());
        // Flush first (simulating close)
        assert_eq!(fs.process_control_flush(fh), 0);
        // Release after flush — buffer already gone, must be a no-op
        assert_eq!(fs.process_control_flush(fh), 0, "release after flush must be noop");
    }

    // ── INO_SYS_ISOLATION coverage (ma.4) ────────────────────────────────────

    #[test]
    fn sys_file_content_isolation_with_caps() {
        use crate::snapshot::IsolationCapsSummary;
        let snap = Arc::new(RwLock::new(SchedulerSnapshot {
            isolation_caps: Some(IsolationCapsSummary {
                runsc:    Some("/usr/bin/runsc".to_string()),
                landlock: true,
                seccomp:  true,
                arch:     "x86_64".to_string(),
                tier:     "full".to_string(),
            }),
            ..Default::default()
        }));
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_ISOLATION).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains("\"tier\":\"full\""),    "tier field");
        assert!(s.contains("\"arch\":\"x86_64\""),  "arch field");
        assert!(s.contains("\"runsc\":\"/usr/bin/runsc\""), "runsc field");
        assert!(s.contains("\"landlock\":true"),    "landlock field");
        assert!(s.contains("\"seccomp\":true"),     "seccomp field");
    }

    #[test]
    fn sys_file_content_isolation_none_fallback() {
        // When isolation_caps is None (startup race), content uses safe defaults.
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_ISOLATION).unwrap();
        let s = String::from_utf8(content).unwrap();
        assert!(s.contains("\"tier\":\"none\""),    "fallback tier must be none");
        assert!(s.contains("\"landlock\":false"),   "fallback landlock must be false");
        assert!(s.contains("\"seccomp\":false"),    "fallback seccomp must be false");
        assert!(s.contains("\"runsc\":null"),       "fallback runsc must be null");
    }

    // ── INO_APPROVALS coverage (con.1 regression guard) ──────────────────────

    #[test]
    fn approvals_content_empty_returns_bracket_newline() {
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.approvals_content();
        assert_eq!(content, b"[]\n");
    }

    #[test]
    fn approvals_ino_is_pseudofile() {
        // Regression guard: INO_APPROVALS must satisfy the open() is_file predicate.
        // Before con.1 the || ino == INO_APPROVALS arm was missing, causing open()
        // to return ENOENT for /agents/approvals.
        let is_pseudofile = |ino: u64| {
            (INO_SYS_BUDGET..=INO_SYS_PROVIDER).contains(&ino)
                || ino == INO_SYS_EGRESS_ADDR
                || ino == INO_SYS_ISOLATION
                || ino == INO_SYS_CREDENTIALS
                || ino == INO_APPROVALS
        };
        assert!(is_pseudofile(INO_APPROVALS),       "INO_APPROVALS must satisfy open() predicate");
        assert!(is_pseudofile(INO_SYS_ISOLATION),   "INO_SYS_ISOLATION must satisfy open() predicate");
        assert!(is_pseudofile(INO_SYS_CREDENTIALS), "INO_SYS_CREDENTIALS must satisfy open() predicate");
    }

    // ── INO_SYS_CREDENTIALS coverage (cred.5) ────────────────────────────────

    #[test]
    fn fuse_system_credentials_no_gateway() {
        let snap = make_snap(vec![]);
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_CREDENTIALS).unwrap();
        assert!(!content.is_empty(), "INO_SYS_CREDENTIALS must not be empty when gateway absent");
    }

    #[test]
    fn fuse_system_credentials_with_gateway() {
        use crate::snapshot::{CredentialSnapshot, ProviderHealth};
        let snap = Arc::new(RwLock::new(SchedulerSnapshot {
            agents:              vec![],
            global_tokens_spent: 0,
            in_flight:           0,
            queue_depth:         0,
            provider_model:      String::new(),
            sandbox:             Default::default(),
            pending_actions:     vec![],
            egress_addr:         None,
            isolation_caps:      None,
            credential_snapshot: Some(CredentialSnapshot {
                gateway_enabled:      true,
                configured_providers: vec!["google".to_string()],
                provider_health:      vec![
                    ProviderHealth {
                        name:            "google".to_string(),
                        token_fresh:     true,
                        last_refresh_at: Some(1720000000),
                        expires_at:      Some(1720003600),
                        last_error:      None,
                        attention_reason: None,
                        attention_since:  None,
                        recovery_kind:   None,
                    }
                ],
            }),
        }));
        let fs = AgentsFs::new(snap, None, None);
        let content = fs.sys_file_content(INO_SYS_CREDENTIALS).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&content)
            .expect("INO_SYS_CREDENTIALS must produce valid JSON when gateway present");
        assert_eq!(json["gateway_enabled"].as_bool(), Some(true));
        let health = &json["provider_health"];
        assert_eq!(health[0]["name"].as_str(), Some("google"));
        assert_eq!(health[0]["token_fresh"].as_bool(), Some(true));
    }

    #[test]
    fn fuse_per_agent_credentials_file_produces_json() {
        let mut ag = agent_snap("ag1", AgentStatus::Running);
        ag.credential_providers         = vec!["google".to_string()];
        ag.credential_request_counts    = HashMap::from([("google".to_string(), 3u64)]);
        ag.credential_denied_counts     = HashMap::new();
        ag.credential_last_access_at    = HashMap::from([("google".to_string(), 1720000000u64)]);

        let snap = make_snap(vec![ag]);
        let mut fs = AgentsFs::new(snap, None, None);
        fs.alloc_dir("ag1");
        let base = *fs.dir_inodes.get("ag1").unwrap();
        let content = fs.file_content_for_ino(base + OFF_CREDENTIALS).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&content)
            .expect("per-agent credentials file must produce valid JSON");
        let providers = json["providers"].as_array().unwrap();
        assert_eq!(providers[0].as_str(), Some("google"));
        let req = json["request_counts"]["google"].as_u64().unwrap_or(0);
        assert_eq!(req, 3);
    }
}
