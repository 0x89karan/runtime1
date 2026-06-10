use std::{
    path::Path,
    sync::{Arc, RwLock},
};

use crate::snapshot::SchedulerSnapshot;

/// Inode assignments:
///   1      = root directory  "/"
///   1010   = first agent directory  (step 10 per agent)
///   1011   = status file  (+1)
///   1012   = context_size file  (+2)
///   1013   = budget file  (+3)
///   1014   = flight file  (+4)
///
/// Used by the Linux FUSE impl and by tests on all platforms.
#[cfg(any(test, target_os = "linux"))]
pub(crate) const DIR_STEP:    u64 = 10;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_STATUS:  u64 = 1;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_CONTEXT: u64 = 2;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_BUDGET:  u64 = 3;
#[cfg(any(test, target_os = "linux"))]
pub(crate) const OFF_FLIGHT:  u64 = 4;

/// Last 64 KB of flight.jsonl to scan for per-agent events.
#[cfg(any(test, target_os = "linux"))]
const FLIGHT_TAIL_BYTES: u64 = 64 * 1024;
/// Maximum matching lines returned in the flight virtual file.
#[cfg(any(test, target_os = "linux"))]
const FLIGHT_TAIL_LINES: usize = 20;

#[cfg(any(test, target_os = "linux"))]
use std::collections::HashMap;

#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(any(test, target_os = "linux"))]
use crate::snapshot::AgentStatus;

/// Kernel TTL for all FUSE attributes and directory entries.
#[cfg(target_os = "linux")]
const TTL: Duration = Duration::ZERO;

#[cfg(target_os = "linux")]
const ROOT_INO: u64 = 1;
#[cfg(any(test, target_os = "linux"))]
const DIR_START: u64 = 1010;

#[cfg(any(test, target_os = "linux"))]
struct AgentsFs {
    snapshot:       Arc<RwLock<SchedulerSnapshot>>,
    /// agent_id → directory inode
    dir_inodes:     HashMap<String, u64>,
    /// any inode (dir or file) → agent_id
    inode_to_id:    HashMap<u64, String>,
    next_dir_inode: u64,
}

#[cfg(any(test, target_os = "linux"))]
impl AgentsFs {
    fn new(snapshot: Arc<RwLock<SchedulerSnapshot>>) -> Self {
        Self {
            snapshot,
            dir_inodes: HashMap::new(),
            inode_to_id: HashMap::new(),
            next_dir_inode: DIR_START,
        }
    }

    /// Return the directory inode for `agent_id`, allocating a new one if needed.
    fn alloc_dir(&mut self, agent_id: &str) -> u64 {
        if let Some(&ino) = self.dir_inodes.get(agent_id) {
            return ino;
        }
        let ino = self.next_dir_inode;
        self.next_dir_inode += DIR_STEP;
        self.dir_inodes.insert(agent_id.to_string(), ino);
        // Register all 5 inodes (dir + 4 files) so inode_to_id lookups work.
        self.inode_to_id.insert(ino,              agent_id.to_string());
        self.inode_to_id.insert(ino + OFF_STATUS,  agent_id.to_string());
        self.inode_to_id.insert(ino + OFF_CONTEXT, agent_id.to_string());
        self.inode_to_id.insert(ino + OFF_BUDGET,  agent_id.to_string());
        self.inode_to_id.insert(ino + OFF_FLIGHT,  agent_id.to_string());
        ino
    }

    fn file_content_for_ino(&self, ino: u64) -> Option<Vec<u8>> {
        let agent_id = self.inode_to_id.get(&ino)?;
        let dir_ino  = self.dir_inodes.get(agent_id)?;
        let offset   = ino - dir_ino;

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
            OFF_FLIGHT => read_flight_tail(&agent.id),
            _ => return None,
        };
        Some(content)
    }
}

/// Only used in tests (the FUSE impl hard-codes file names in readdir).
#[cfg(test)]
fn file_name_for_offset(offset: u64) -> Option<&'static str> {
    match offset {
        OFF_STATUS  => Some("status"),
        OFF_CONTEXT => Some("context_size"),
        OFF_BUDGET  => Some("budget"),
        OFF_FLIGHT  => Some("flight"),
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

    let mut buf = String::new();
    let _ = file.read_to_string(&mut buf);

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

        if parent == ROOT_INO {
            // Looking up an agent directory by name.
            let ino = self.alloc_dir(name_str);
            let snap = match self.snapshot.read() {
                Ok(s) => s,
                Err(_) => { reply.error(libc::EIO); return; }
            };
            if snap.agents.iter().any(|a| a.id == name_str) {
                drop(snap);
                reply.entry(&TTL, &make_file_attr(ino, 0, fuser::FileType::Directory), 0);
            } else {
                reply.error(libc::ENOENT);
            }
            return;
        }

        // Looking up a file inside an agent directory.
        let agent_id = match self.inode_to_id.get(&parent).cloned() {
            Some(id) => id,
            None     => { reply.error(libc::ENOENT); return; }
        };
        let dir_ino = match self.dir_inodes.get(&agent_id) {
            Some(&d) => d,
            None     => { reply.error(libc::ENOENT); return; }
        };
        if parent != dir_ino {
            reply.error(libc::ENOENT);
            return;
        }

        let file_ino = match name_str {
            "status"       => dir_ino + OFF_STATUS,
            "context_size" => dir_ino + OFF_CONTEXT,
            "budget"       => dir_ino + OFF_BUDGET,
            "flight"       => dir_ino + OFF_FLIGHT,
            _              => { reply.error(libc::ENOENT); return; }
        };

        let size = self.file_content_for_ino(file_ino)
            .map(|c| c.len() as u64)
            .unwrap_or(0);
        reply.entry(&TTL, &make_file_attr(file_ino, size, fuser::FileType::RegularFile), 0);
    }

    fn getattr(&mut self, _req: &fuser::Request<'_>, ino: u64, reply: fuser::ReplyAttr) {
        if ino == ROOT_INO {
            reply.attr(&TTL, &make_file_attr(ROOT_INO, 0, fuser::FileType::Directory));
            return;
        }
        // Check if it's a known directory inode.
        if self.inode_to_id.contains_key(&ino) {
            if self.dir_inodes.values().any(|&d| d == ino) {
                reply.attr(&TTL, &make_file_attr(ino, 0, fuser::FileType::Directory));
                return;
            }
            // It's a file inode.
            let size = self.file_content_for_ino(ino)
                .map(|c| c.len() as u64)
                .unwrap_or(0);
            reply.attr(&TTL, &make_file_attr(ino, size, fuser::FileType::RegularFile));
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
        let content = match self.file_content_for_ino(ino) {
            Some(c) => c,
            None    => { reply.error(libc::ENOENT); return; }
        };
        let offset = if offset < 0 { 0usize } else { offset as usize };
        let start = offset.min(content.len());
        let end   = (offset + size as usize).min(content.len());
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
        if ino == ROOT_INO {
            let snap = match self.snapshot.read() {
                Ok(s)  => s,
                Err(_) => { reply.error(libc::EIO); return; }
            };
            let agent_ids: Vec<String> = snap.agents.iter().map(|a| a.id.clone()).collect();
            drop(snap);

            let entries: Vec<(u64, fuser::FileType, String)> = {
                let mut v = vec![
                    (ROOT_INO,     fuser::FileType::Directory, ".".to_string()),
                    (ROOT_INO,     fuser::FileType::Directory, "..".to_string()),
                ];
                for id in &agent_ids {
                    let dir_ino = self.alloc_dir(id);
                    v.push((dir_ino, fuser::FileType::Directory, id.clone()));
                }
                v
            };

            for (i, (entry_ino, kind, name)) in entries.iter().enumerate() {
                if (i as i64) < offset { continue; }
                if reply.add(*entry_ino, (i + 1) as i64, *kind, name) {
                    break;
                }
            }
            reply.ok();
            return;
        }

        // readdir inside an agent directory.
        let agent_id = match self.inode_to_id.get(&ino).cloned() {
            Some(id) => id,
            None     => { reply.error(libc::ENOENT); return; }
        };
        let dir_ino = match self.dir_inodes.get(&agent_id) {
            Some(&d) => d,
            None     => { reply.error(libc::ENOENT); return; }
        };
        if ino != dir_ino {
            reply.error(libc::ENOENT);
            return;
        }

        let entries: Vec<(u64, fuser::FileType, &'static str)> = vec![
            (dir_ino,                fuser::FileType::Directory,   "."),
            (ROOT_INO,               fuser::FileType::Directory,   ".."),
            (dir_ino + OFF_STATUS,   fuser::FileType::RegularFile, "status"),
            (dir_ino + OFF_CONTEXT,  fuser::FileType::RegularFile, "context_size"),
            (dir_ino + OFF_BUDGET,   fuser::FileType::RegularFile, "budget"),
            (dir_ino + OFF_FLIGHT,   fuser::FileType::RegularFile, "flight"),
        ];

        for (i, (entry_ino, kind, name)) in entries.iter().enumerate() {
            if (i as i64) < offset { continue; }
            if reply.add(*entry_ino, (i + 1) as i64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn opendir(&mut self, _req: &fuser::Request<'_>, ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
        if ino == ROOT_INO || self.inode_to_id.contains_key(&ino) {
            reply.opened(0, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn open(&mut self, _req: &fuser::Request<'_>, ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
        if self.inode_to_id.contains_key(&ino) && !self.dir_inodes.values().any(|&d| d == ino) {
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
    snapshot: Arc<RwLock<SchedulerSnapshot>>,
) -> anyhow::Result<fuser::BackgroundSession> {
    fuser::spawn_mount2(
        AgentsFs::new(snapshot),
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
    _snapshot: Arc<RwLock<SchedulerSnapshot>>,
) -> anyhow::Result<()> {
    anyhow::bail!("FUSE not supported on this platform")
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{AgentSnapshot, AgentStatus, SchedulerSnapshot};
    use std::sync::{Arc, RwLock};

    fn make_snap(agents: Vec<AgentSnapshot>) -> Arc<RwLock<SchedulerSnapshot>> {
        Arc::new(RwLock::new(SchedulerSnapshot {
            agents,
            global_tokens_spent: 0,
            in_flight: 0,
        }))
    }

    fn agent_snap(id: &str, status: AgentStatus) -> AgentSnapshot {
        AgentSnapshot {
            id:             id.to_string(),
            status,
            turn:           0,
            context_tokens: 100,
            token_budget:   50_000,
            task_preview:   "do something".to_string(),
        }
    }

    fn fs_with_agent(id: &str, status: AgentStatus) -> AgentsFs {
        let snap = make_snap(vec![agent_snap(id, status)]);
        let mut fs = AgentsFs::new(snap);
        fs.alloc_dir(id);
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
        let mut fs = AgentsFs::new(snap);
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
        let mut fs = AgentsFs::new(snap);
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
        let mut fs = AgentsFs::new(snap);
        fs.alloc_dir("a");
        let dir_ino = fs.dir_inodes["a"];
        let content = fs.file_content_for_ino(dir_ino + OFF_BUDGET).unwrap();
        assert_eq!(content, b"50000\n");
    }

    // ── Inode allocation ──────────────────────────────────────────────────────

    #[test]
    fn alloc_dir_returns_stable_inode() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap);
        let ino1 = fs.alloc_dir("alpha");
        let ino2 = fs.alloc_dir("alpha");
        assert_eq!(ino1, ino2, "repeated alloc must return same inode");
    }

    #[test]
    fn alloc_dir_increments_per_agent() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap);
        let ino_a = fs.alloc_dir("alpha");
        let ino_b = fs.alloc_dir("beta");
        assert_eq!(ino_b, ino_a + DIR_STEP);
    }

    #[test]
    fn all_five_inodes_registered_after_alloc() {
        let snap = make_snap(vec![]);
        let mut fs = AgentsFs::new(snap);
        let dir_ino = fs.alloc_dir("x");
        for offset in [0, OFF_STATUS, OFF_CONTEXT, OFF_BUDGET, OFF_FLIGHT] {
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
        // Offset 99 is not OFF_STATUS/CONTEXT/BUDGET/FLIGHT.
        assert!(fs.file_content_for_ino(dir_ino + 99).is_none());
    }

    #[test]
    fn file_name_for_offset_covers_all_files() {
        assert_eq!(file_name_for_offset(OFF_STATUS),  Some("status"));
        assert_eq!(file_name_for_offset(OFF_CONTEXT), Some("context_size"));
        assert_eq!(file_name_for_offset(OFF_BUDGET),  Some("budget"));
        assert_eq!(file_name_for_offset(OFF_FLIGHT),  Some("flight"));
        assert_eq!(file_name_for_offset(99),          None);
    }
}
