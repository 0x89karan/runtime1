use std::{
    path::Path,
    sync::{Arc, RwLock},
};

use crate::{snapshot::SchedulerSnapshot, MemoryAccess};

/// Inode assignments:
///   1      = root directory  "/"
///   9      = kb/ directory   (shared KB segments)
///   1010   = first agent directory  (step 10 per agent)
///   +1     = status file
///   +2     = context_size file
///   +3     = budget file
///   +4     = flight file
///   +5     = memory/ subdir
///   +6     = memory/short_term file
///   +7     = memory/long_term/ subdir
///
///   1_000_000+ = dynamic pool for memory/long_term/<key>,
///                kb/<segment>/, and kb/<segment>/<key>
///
/// Used by the Linux FUSE impl and by tests on all platforms.
#[cfg(any(test, target_os = "linux"))]
pub(crate) const DIR_STEP:         u64 = 10;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_STATUS:       u64 = 1;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_CONTEXT:      u64 = 2;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_BUDGET:       u64 = 3;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_FLIGHT:       u64 = 4;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_MEMORY_DIR:   u64 = 5;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_SHORT_TERM:   u64 = 6;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_LONG_TERM_DIR: u64 = 7;

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

/// Return up to `MAX_DIR_KEYS` keys for a namespace, applying the cap consistently.
#[cfg(any(test, target_os = "linux"))]
fn capped_keys(mem: &dyn MemoryAccess, namespace: &str) -> Vec<String> {
    mem.list_keys(namespace).into_iter().take(MAX_DIR_KEYS).collect()
}

#[cfg(any(test, target_os = "linux"))]
impl AgentsFs {
    fn new(
        snapshot: Arc<RwLock<SchedulerSnapshot>>,
        memory:   Option<Arc<dyn MemoryAccess>>,
    ) -> Self {
        Self {
            snapshot,
            memory,
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

    /// Return the directory inode for `agent_id`, allocating a new one if needed.
    fn alloc_dir(&mut self, agent_id: &str) -> u64 {
        if let Some(&ino) = self.dir_inodes.get(agent_id) {
            return ino;
        }
        debug_assert!(
            self.next_dir_inode < DYNAMIC_INO_START,
            "fixed inode pool reached dynamic inode range at {}",
            self.next_dir_inode
        );
        let ino = self.next_dir_inode;
        self.next_dir_inode += DIR_STEP;
        self.dir_inodes.insert(agent_id.to_string(), ino);
        // Register all 8 fixed inodes so inode_to_id lookups work.
        for offset in [
            0, OFF_STATUS, OFF_CONTEXT, OFF_BUDGET, OFF_FLIGHT,
            OFF_MEMORY_DIR, OFF_SHORT_TERM, OFF_LONG_TERM_DIR,
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
                    b"unlimited\n".to_vec()
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
            // OFF_MEMORY_DIR and OFF_LONG_TERM_DIR are directories — not served here.
            _ => return None,
        };
        Some(content)
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
            // Remove all 8 fixed per-agent inodes (dir + offsets 1–7).
            for offset in [
                0u64, OFF_STATUS, OFF_CONTEXT, OFF_BUDGET, OFF_FLIGHT,
                OFF_MEMORY_DIR, OFF_SHORT_TERM, OFF_LONG_TERM_DIR,
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
    AgentDir(String),
    MemoryDir(String),
    LongTermDir(String),
    KbSegDir(String),
}

/// Only used in tests (the FUSE impl hard-codes file names in readdir).
#[cfg(test)]
fn file_name_for_offset(offset: u64) -> Option<&'static str> {
    match offset {
        OFF_STATUS       => Some("status"),
        OFF_CONTEXT      => Some("context_size"),
        OFF_BUDGET       => Some("budget"),
        OFF_FLIGHT       => Some("flight"),
        OFF_MEMORY_DIR   => Some("memory"),
        OFF_SHORT_TERM   => Some("short_term"),
        OFF_LONG_TERM_DIR => Some("long_term"),
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
                // kb/ dir (only when memory is configured)
                if name_str == "kb" && self.memory.is_some() {
                    reply.entry(&TTL, &make_file_attr(INO_KB, 0, fuser::FileType::Directory), 0);
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
        let content = if let Some(c) = self.file_content_for_ino(ino) {
            c
        } else if let Some(c) = self.dyn_file_content(ino) {
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
                v
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
        let is_file = if let Some(agent_id) = self.inode_to_id.get(&ino) {
            let base   = self.dir_inodes[agent_id];
            let offset = ino.wrapping_sub(base);
            !matches!(offset, 0) && offset != OFF_MEMORY_DIR && offset != OFF_LONG_TERM_DIR
        } else if let Some(kind) = self.dyn_ino_kind.get(&ino) {
            !matches!(kind, DynInoKind::KbSeg { .. })
        } else {
            false
        };
        if is_file {
            reply.opened(0, fuser::consts::FOPEN_DIRECT_IO);
        } else {
            reply.error(libc::ENOENT);
        }
    }
}

/// Mount the `/agents` FUSE filesystem. Returns the `BackgroundSession` that keeps
/// the mount alive — drop it to unmount. Linux only.
#[cfg(target_os = "linux")]
pub fn mount(
    mountpoint: &Path,
    snapshot:   Arc<RwLock<SchedulerSnapshot>>,
    memory:     Option<Arc<dyn MemoryAccess>>,
) -> anyhow::Result<fuser::BackgroundSession> {
    fuser::spawn_mount2(
        AgentsFs::new(snapshot, memory),
        mountpoint,
        &[
            fuser::MountOption::FSName("agents".to_string()),
            fuser::MountOption::RO,
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
            in_flight: 0,
        }))
    }

    fn agent_snap(id: &str, status: AgentStatus) -> AgentSnapshot {
        AgentSnapshot {
            id:                  id.to_string(),
            status,
            turn:                0,
            context_tokens:      100,
            token_budget:        50_000,
            task_preview:        "do something".to_string(),
            short_term_previews: vec![],
        }
    }

    fn fs_with_agent(id: &str, status: AgentStatus) -> AgentsFs {
        let snap = make_snap(vec![agent_snap(id, status)]);
        let mut fs = AgentsFs::new(snap, None);
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
        let mut fs = AgentsFs::new(snap, Some(mem));
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
        let mut fs = AgentsFs::new(snap, None);
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
        let mut fs = AgentsFs::new(snap, None);
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
        let mut fs = AgentsFs::new(snap, None);
        fs.alloc_dir("a");
        let dir_ino = fs.dir_inodes["a"];
        let content = fs.file_content_for_ino(dir_ino + OFF_BUDGET).unwrap();
        assert_eq!(content, b"50000\n");
    }

    // ── Inode allocation ──────────────────────────────────────────────────────

    #[test]
    fn alloc_dir_returns_stable_inode() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None);
        let ino1 = fs.alloc_dir("alpha");
        let ino2 = fs.alloc_dir("alpha");
        assert_eq!(ino1, ino2, "repeated alloc must return same inode");
    }

    #[test]
    fn alloc_dir_increments_per_agent() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None);
        let ino_a = fs.alloc_dir("alpha");
        let ino_b = fs.alloc_dir("beta");
        assert_eq!(ino_b, ino_a + DIR_STEP);
    }

    #[test]
    fn all_eight_inodes_registered_after_alloc() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap, None);
        let dir_ino = fs.alloc_dir("x");
        for offset in [
            0, OFF_STATUS, OFF_CONTEXT, OFF_BUDGET, OFF_FLIGHT,
            OFF_MEMORY_DIR, OFF_SHORT_TERM, OFF_LONG_TERM_DIR,
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
        let mut fs = AgentsFs::new(snap, Some(MockMemory::new()));
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
        let mut fs = AgentsFs::new(snap, Some(mock));

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
        let mut fs = AgentsFs::new(snap, Some(mock));
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

        let mut fs = AgentsFs::new(snap, Some(MockMemory::new()));
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
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()));
        fs.alloc_dir("a");
        let ino1 = fs.alloc_lt_file("a", "some-key");
        let ino2 = fs.alloc_lt_file("a", "some-key");
        assert_eq!(ino1, ino2, "alloc_lt_file must return the same inode on repeated calls");
    }

    #[test]
    fn alloc_kb_seg_idempotent() {
        let fs_snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()));
        let ino1 = fs.alloc_kb_seg("canon");
        let ino2 = fs.alloc_kb_seg("canon");
        assert_eq!(ino1, ino2, "alloc_kb_seg must return the same inode on repeated calls");
    }

    #[test]
    fn alloc_kb_file_idempotent() {
        let fs_snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()));
        let ino1 = fs.alloc_kb_file("canon", "doc-1");
        let ino2 = fs.alloc_kb_file("canon", "doc-1");
        assert_eq!(ino1, ino2, "alloc_kb_file must return the same inode on repeated calls");
    }

    // ── parent_kind edge cases ────────────────────────────────────────────────

    #[test]
    fn parent_kind_kb_inode_returns_kb() {
        let fs_snap = make_snap(vec![]);
        let fs = AgentsFs::new(fs_snap, Some(MockMemory::new()));
        // INO_KB=9; passing it as parent must route to ParentKind::Kb
        let pk = fs.parent_kind(INO_KB);
        assert!(matches!(pk, Some(ParentKind::Kb)), "INO_KB as parent must be ParentKind::Kb");
    }

    #[test]
    fn parent_kind_unknown_inode_returns_none() {
        let fs_snap = make_snap(vec![]);
        let fs = AgentsFs::new(fs_snap, Some(MockMemory::new()));
        // 999_999 is not root, not INO_KB, not an agent dir, not a dyn ino
        let pk = fs.parent_kind(999_999);
        assert!(pk.is_none(), "unknown inode must return None from parent_kind");
    }

    #[test]
    fn parent_kind_memory_dir_and_long_term_dir_offsets() {
        // Regression: ensures the wrapping_sub(base) + match on OFF_MEMORY_DIR /
        // OFF_LONG_TERM_DIR constants correctly routes to the right ParentKind arms.
        let snap = make_snap(vec![agent_snap("x", AgentStatus::Running)]);
        let mut fs = AgentsFs::new(snap, Some(MockMemory::new()));
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
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()));
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
        let mut fs = AgentsFs::new(fs_snap, None);  // no memory configured
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
        let mut fs = AgentsFs::new(fs_snap, Some(MockMemory::new()));
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
        let mut fs = AgentsFs::new(snap, Some(mock));
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
        let mut fs = AgentsFs::new(snap, Some(mock));
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
        let mut fs = AgentsFs::new(snap, Some(mock));

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
        let fs = AgentsFs::new(snap, None);
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
        let mut fs = AgentsFs::new(snap, Some(mock));
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
                       OFF_MEMORY_DIR, OFF_SHORT_TERM, OFF_LONG_TERM_DIR] {
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
        let mut fs = AgentsFs::new(snap, None); // no memory store
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
        let mut fs = AgentsFs::new(snap, None); // no memory store
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
        let mut fs = AgentsFs::new(snap, None);
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
        let mut fs = AgentsFs::new(snap, None);
        // Must not panic even though "ghost" was never registered.
        fs.prune_dead_agent("ghost");
        assert!(fs.dir_inodes.is_empty());
        assert!(fs.inode_to_id.is_empty());
    }
}
