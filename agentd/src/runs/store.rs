//! `runs.redb` — durable run-segment store (ux.11b-substrate).
//!
//! A separate redb file from `memory.redb` (E6): the scheduler's run writes must
//! not share a write-lock with KB read/search traffic. Open/create, 0600, and
//! corruption-quarantine mirror `memory::store::RedbStore`; the schema is a
//! versioned `META.format_version` (E5).

use std::path::{Path, PathBuf};

use anyhow::Context;
use redb::{
    Database, DatabaseError, ReadableDatabase, ReadableTable, StorageError, TableDefinition,
};

use super::{RunEvent, RunRecord};

/// segment_id → RunRecord JSON.
const RUNS: TableDefinition<&str, &str> = TableDefinition::new("runs");
/// agent_id → currently-open segment_id (idempotent-open index, G3).
const OPEN_BY_AGENT: TableDefinition<&str, &str> = TableDefinition::new("open_by_agent");
/// meta: "format_version" + "seq:{agent_id}" per-agent segment counter.
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

pub const RUNS_SCHEMA_VERSION: u64 = 1;

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Read-only filter for `runs_query` / the management API. All fields optional;
/// `limit` is clamped to `[1, 100]` by `RunsStore::list`.
#[derive(Debug, Clone, Default)]
pub struct RunFilter {
    pub from:      Option<u64>,
    pub to:        Option<u64>,
    pub agent_id:  Option<String>,
    pub parent_id: Option<String>,
    pub status:    Option<String>,
    pub limit:     usize,
}

enum OpenFailure {
    Locked(anyhow::Error),
    Corrupt(anyhow::Error),
    Other(anyhow::Error),
}

impl OpenFailure {
    fn into_inner(self) -> anyhow::Error {
        match self {
            OpenFailure::Locked(e) | OpenFailure::Corrupt(e) | OpenFailure::Other(e) => e,
        }
    }
}

fn classify_db_error(e: DatabaseError) -> OpenFailure {
    if matches!(e, DatabaseError::DatabaseAlreadyOpen) {
        return OpenFailure::Locked(anyhow::Error::new(e));
    }
    let is_corruption = matches!(&e, DatabaseError::Storage(StorageError::Corrupted(_)))
        || matches!(
            &e,
            DatabaseError::Storage(StorageError::Io(io))
                if io.kind() == std::io::ErrorKind::InvalidData
        );
    if is_corruption {
        OpenFailure::Corrupt(anyhow::Error::new(e))
    } else {
        OpenFailure::Other(anyhow::Error::new(e))
    }
}

pub struct RunsStore {
    db: Database,
}

impl RunsStore {
    /// Open or create `runs.redb`. Returns `(store, Some(quarantine_path))` when a
    /// corrupt file was quarantined. `Err` on lock / permission / non-corruption.
    pub fn open(path: &Path) -> anyhow::Result<(Self, Option<PathBuf>)> {
        match Self::try_open(path) {
            Ok(store) => Ok((store, None)),
            Err(OpenFailure::Locked(e)) => Err(e.context(
                "runs.redb is held by another process; stop the other agentd instance",
            )),
            Err(OpenFailure::Other(e)) => Err(e.context(
                "runs store could not be opened (NOT corruption; file left in place)",
            )),
            Err(OpenFailure::Corrupt(e)) => {
                let corrupt_path = Self::quarantine_path(path);
                std::fs::rename(path, &corrupt_path)
                    .with_context(|| format!("quarantining corrupt runs store: {path:?} → {corrupt_path:?}"))
                    .with_context(|| format!("original corruption: {e:#}"))?;
                let store = Self::try_open(path)
                    .map_err(OpenFailure::into_inner)
                    .context("opening fresh runs store after quarantine")?;
                Ok((store, Some(corrupt_path)))
            }
        }
    }

    fn quarantine_path(path: &Path) -> PathBuf {
        let ts = unix_now_secs();
        let name = path
            .file_name()
            .map(|n| format!("{}.{ts}.corrupt", n.to_string_lossy()))
            .unwrap_or_else(|| format!("runs.redb.{ts}.corrupt"));
        path.parent().unwrap_or(Path::new(".")).join(name)
    }

    fn try_open(path: &Path) -> Result<Self, OpenFailure> {
        let db = if path.exists() {
            Database::open(path).map_err(classify_db_error)?
        } else {
            Database::create(path).map_err(|e| {
                OpenFailure::Other(anyhow::Error::new(e).context(format!("creating runs store at {path:?}")))
            })?
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .map_err(|e| OpenFailure::Other(anyhow::Error::new(e).context("reading runs store metadata")))?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms).map_err(|e| {
                OpenFailure::Other(anyhow::Error::new(e).context("setting runs store permissions to 0600"))
            })?;
        }
        let store = Self { db };
        store
            .init_schema()
            .map_err(|e| OpenFailure::Other(e.context("initialising runs store schema")))?;
        Ok(store)
    }

    fn init_schema(&self) -> anyhow::Result<()> {
        let txn = self.db.begin_write().context("beginning runs schema init")?;
        {
            let _runs = txn.open_table(RUNS).context("opening runs table")?;
            let _open = txn.open_table(OPEN_BY_AGENT).context("opening open_by_agent table")?;
            let mut meta = txn.open_table(META).context("opening runs meta table")?;
            if meta.get("format_version").context("reading runs format_version")?.is_none() {
                meta.insert("format_version", RUNS_SCHEMA_VERSION)
                    .context("writing runs format_version")?;
            }
        }
        txn.commit().context("committing runs schema init")?;
        Ok(())
    }

    /// Test/introspection: current schema version from META.
    pub fn format_version(&self) -> anyhow::Result<u64> {
        let txn = self.db.begin_read().context("read txn")?;
        let meta = txn.open_table(META).context("opening meta")?;
        Ok(meta.get("format_version").context("reading version")?.map(|g| g.value()).unwrap_or(0))
    }

    /// Apply one lifecycle event (writer task only — single writer → seq ordering).
    pub fn apply(&self, ev: RunEvent) -> anyhow::Result<()> {
        match ev {
            RunEvent::Open { agent_id, parent_id, start_reason, start_context_tokens, tier, ts } => {
                self.open_segment(&agent_id, parent_id, &start_reason, start_context_tokens, &tier, ts)
            }
            RunEvent::Close { agent_id, status, stop_reason, last_error, end_context_tokens, ts } => {
                self.close_segment(&agent_id, &status, stop_reason, last_error, end_context_tokens, ts)
            }
            RunEvent::IncrApproval { agent_id } => self.incr_approval(&agent_id),
        }
    }

    /// Open a new segment for `agent_id`. Idempotent (G3): if the agent already
    /// has an open segment, this is a no-op — a restart re-opening continues it.
    fn open_segment(
        &self,
        agent_id: &str,
        parent_id: Option<String>,
        start_reason: &str,
        start_context_tokens: Option<u64>,
        tier: &str,
        ts: u64,
    ) -> anyhow::Result<()> {
        let txn = self.db.begin_write().context("begin open_segment")?;
        {
            let mut open_tbl = txn.open_table(OPEN_BY_AGENT).context("open open_by_agent")?;
            // Idempotent (G3): already-open agent → no write (restart continues the segment).
            let already_open = open_tbl.get(agent_id).context("checking existing open segment")?.is_some();
            if !already_open {
                let seq = {
                    let mut meta_tbl = txn.open_table(META).context("open meta")?;
                    let seq_key = format!("seq:{agent_id}");
                    let s = meta_tbl.get(seq_key.as_str()).context("read seq")?.map(|g| g.value()).unwrap_or(0);
                    meta_tbl.insert(seq_key.as_str(), s + 1).context("bump seq")?;
                    s
                };
                let segment_id = format!("{agent_id}:{seq}");
                let rec = RunRecord {
                    segment_id:   segment_id.clone(),
                    agent_id:     agent_id.to_string(),
                    segment_seq:  seq,
                    parent_id,
                    start_reason: start_reason.to_string(),
                    start_ts:     ts,
                    end_ts:       None,
                    status:       "running".to_string(),
                    stop_reason:  None,
                    last_error:   None,
                    approvals_count: 0,
                    start_context_tokens,
                    spend:        None,
                    tier:         tier.to_string(),
                };
                let json = serde_json::to_string(&rec).context("serialize open record")?;
                {
                    let mut runs_tbl = txn.open_table(RUNS).context("open runs")?;
                    runs_tbl.insert(segment_id.as_str(), json.as_str()).context("insert open record")?;
                }
                open_tbl.insert(agent_id, segment_id.as_str()).context("index open segment")?;
            }
        }
        // Committing an empty (idempotent-skip) txn is harmless.
        txn.commit().context("commit open_segment")?;
        Ok(())
    }

    fn close_segment(
        &self,
        agent_id: &str,
        status: &str,
        stop_reason: Option<String>,
        last_error: Option<String>,
        end_context_tokens: Option<u64>,
        ts: u64,
    ) -> anyhow::Result<()> {
        let txn = self.db.begin_write().context("begin close_segment")?;
        {
            let mut open_tbl = txn.open_table(OPEN_BY_AGENT).context("open open_by_agent")?;
            // Materialize the open segment id (drop the guard) before any write.
            let segment_id = open_tbl.get(agent_id).context("lookup open segment")?.map(|g| g.value().to_string());
            if let Some(segment_id) = segment_id {
                let mut runs_tbl = txn.open_table(RUNS).context("open runs")?;
                let existing = runs_tbl.get(segment_id.as_str()).context("read record for close")?.map(|g| g.value().to_string());
                if let Some(existing) = existing {
                    // A corrupt/unparseable record must NOT propagate here: doing so would
                    // return before clearing OPEN_BY_AGENT and wedge the agent forever
                    // (every future open no-ops, every close re-hits the poison). Skip the
                    // rewrite on parse failure but still clear the open index below.
                    match serde_json::from_str::<RunRecord>(&existing) {
                        Ok(mut rec) => {
                            rec.end_ts = Some(ts);
                            rec.status = status.to_string();
                            rec.stop_reason = stop_reason;
                            rec.last_error = last_error;
                            debug_assert!(
                                match (rec.start_context_tokens, end_context_tokens) {
                                    (Some(s), Some(e)) => e >= s,
                                    _ => true,
                                },
                                "run spend underflow: end_context_tokens < start (non-monotonic context_tokens?)"
                            );
                            rec.spend = match (rec.start_context_tokens, end_context_tokens) {
                                (Some(s), Some(e)) => Some(e.saturating_sub(s)),
                                _ => None,
                            };
                            let json = serde_json::to_string(&rec).context("serialize closed record")?;
                            runs_tbl.insert(segment_id.as_str(), json.as_str()).context("write closed record")?;
                        }
                        Err(_) => { /* corrupt record left as-is; open index cleared below */ }
                    }
                }
                open_tbl.remove(agent_id).context("clear open index")?;
            }
            // else: no open segment (double-close / close-before-open) → no-op.
        }
        txn.commit().context("commit close_segment")?;
        Ok(())
    }

    fn incr_approval(&self, agent_id: &str) -> anyhow::Result<()> {
        let txn = self.db.begin_write().context("begin incr_approval")?;
        {
            let open_tbl = txn.open_table(OPEN_BY_AGENT).context("open open_by_agent")?;
            let segment_id = open_tbl.get(agent_id).context("lookup open segment")?.map(|g| g.value().to_string());
            if let Some(segment_id) = segment_id {
                let mut runs_tbl = txn.open_table(RUNS).context("open runs")?;
                let existing = runs_tbl.get(segment_id.as_str()).context("read record")?.map(|g| g.value().to_string());
                if let Some(existing) = existing {
                    let mut rec: RunRecord = serde_json::from_str(&existing).context("parse record")?;
                    rec.approvals_count += 1;
                    let json = serde_json::to_string(&rec).context("serialize record")?;
                    runs_tbl.insert(segment_id.as_str(), json.as_str()).context("write record")?;
                }
            }
        }
        txn.commit().context("commit incr_approval")?;
        Ok(())
    }

    /// Read one record by segment id.
    pub fn get(&self, segment_id: &str) -> anyhow::Result<Option<RunRecord>> {
        let txn = self.db.begin_read().context("begin read")?;
        let runs_tbl = txn.open_table(RUNS).context("open runs")?;
        match runs_tbl.get(segment_id).context("get record")? {
            Some(v) => Ok(Some(serde_json::from_str(v.value()).context("parse record")?)),
            None => Ok(None),
        }
    }

    /// The agent's currently-open segment id, if any (idempotent-open probe / tests).
    pub fn latest_open(&self, agent_id: &str) -> anyhow::Result<Option<String>> {
        let txn = self.db.begin_read().context("begin read")?;
        let open_tbl = txn.open_table(OPEN_BY_AGENT).context("open open_by_agent")?;
        Ok(open_tbl.get(agent_id).context("get open")?.map(|g| g.value().to_string()))
    }

    /// List records matching `filter`, newest-first (by start_ts, then segment_seq).
    /// `limit` clamps to `[1, 100]` (default 20 when 0).
    pub fn list(&self, filter: &RunFilter) -> anyhow::Result<Vec<RunRecord>> {
        let limit = if filter.limit == 0 { 20 } else { filter.limit.min(100) };
        let txn = self.db.begin_read().context("begin read")?;
        let runs_tbl = txn.open_table(RUNS).context("open runs")?;
        let mut out: Vec<RunRecord> = Vec::new();
        for item in runs_tbl.iter().context("iter runs")? {
            let (_k, v) = item.context("read run item")?;
            let rec: RunRecord = match serde_json::from_str(v.value()) {
                Ok(r) => r,
                Err(_) => continue, // skip an unparseable record rather than fail the whole query
            };
            if let Some(f) = filter.from { if rec.start_ts < f { continue; } }
            if let Some(t) = filter.to { if rec.start_ts > t { continue; } }
            if let Some(ref a) = filter.agent_id { if &rec.agent_id != a { continue; } }
            if let Some(ref p) = filter.parent_id { if rec.parent_id.as_deref() != Some(p.as_str()) { continue; } }
            if let Some(ref s) = filter.status { if &rec.status != s { continue; } }
            out.push(rec);
        }
        // Newest-first: highest start_ts first, tie-break on segment_seq.
        out.sort_by(|a, b| b.start_ts.cmp(&a.start_ts).then(b.segment_seq.cmp(&a.segment_seq)));
        out.truncate(limit);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (RunsStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (s, q) = RunsStore::open(&dir.path().join("runs.redb")).unwrap();
        assert!(q.is_none(), "fresh store must not quarantine");
        (s, dir)
    }

    #[test]
    fn open_sets_schema_version() {
        let (s, _d) = store();
        assert_eq!(s.format_version().unwrap(), RUNS_SCHEMA_VERSION);
    }

    #[test]
    fn open_then_close_computes_spend_delta() {
        let (s, _d) = store();
        s.open_segment("scout:1", Some("cos".into()), "child_spawn", Some(100), "native", 1_000).unwrap();
        let open = s.latest_open("scout:1").unwrap();
        assert_eq!(open.as_deref(), Some("scout:1:0"));
        s.close_segment("scout:1", "done", Some("completed".into()), None, Some(160), 1_050).unwrap();
        let rec = s.get("scout:1:0").unwrap().unwrap();
        assert_eq!(rec.status, "done");
        assert_eq!(rec.spend, Some(60), "spend = 160 - 100");
        assert_eq!(rec.stop_reason.as_deref(), Some("completed"));
        assert!(s.latest_open("scout:1").unwrap().is_none(), "open index cleared on close");
    }

    #[test]
    fn open_is_idempotent_continues_not_duplicates() {
        // G3: a second open for the same agent (e.g. restart re-seed) is a no-op.
        let (s, _d) = store();
        s.open_segment("cos", None, "config_seed", Some(0), "native", 1).unwrap();
        s.open_segment("cos", None, "restore", Some(0), "native", 2).unwrap();
        // Still exactly one segment, seq 0, original start_reason.
        let all = s.list(&RunFilter { agent_id: Some("cos".into()), ..Default::default() }).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].segment_seq, 0);
        assert_eq!(all[0].start_reason, "config_seed");
    }

    #[test]
    fn child_spawn_then_terminal_one_closed_record() {
        let (s, _d) = store();
        s.open_segment("inbox", Some("cos".into()), "child_spawn", Some(0), "native", 10).unwrap();
        s.close_segment("inbox", "failed", Some("error".into()), Some("boom".into()), Some(42), 20).unwrap();
        let recs = s.list(&RunFilter::default()).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].status, "failed");
        assert_eq!(recs[0].parent_id.as_deref(), Some("cos"));
        assert_eq!(recs[0].last_error.as_deref(), Some("boom"));
        assert_eq!(recs[0].spend, Some(42));
    }

    #[test]
    fn park_closes_with_parked_status() {
        let (s, _d) = store();
        s.open_segment("orch", None, "operator_spawn", Some(5), "native", 1).unwrap();
        // park = close with status "parked"
        s.close_segment("orch", "parked", Some("approval_requested".into()), None, Some(9), 2).unwrap();
        let rec = s.list(&RunFilter::default()).unwrap().pop().unwrap();
        assert_eq!(rec.status, "parked");
        assert_eq!(rec.stop_reason.as_deref(), Some("approval_requested"));
    }

    #[test]
    fn universal_spend_is_none() {
        let (s, _d) = store();
        s.open_segment("uni", None, "universal_spawn", None, "universal", 1).unwrap();
        s.close_segment("uni", "done", None, None, None, 2).unwrap();
        let rec = s.get("uni:0").unwrap().unwrap();
        assert_eq!(rec.spend, None, "universal-tier has no context_tokens → spend None");
        assert_eq!(rec.tier, "universal");
    }

    #[test]
    fn incr_approval_counts_on_open_segment() {
        let (s, _d) = store();
        s.open_segment("a", None, "child_spawn", Some(0), "native", 1).unwrap();
        s.incr_approval("a").unwrap();
        s.incr_approval("a").unwrap();
        let open = s.latest_open("a").unwrap().unwrap();
        assert_eq!(s.get(&open).unwrap().unwrap().approvals_count, 2);
    }

    #[test]
    fn list_newest_first_and_limit() {
        let (s, _d) = store();
        for (i, ts) in [(0u64, 10u64), (1, 30), (2, 20)] {
            s.open_segment(&format!("a{i}"), None, "child_spawn", Some(0), "native", ts).unwrap();
        }
        let recs = s.list(&RunFilter { limit: 2, ..Default::default() }).unwrap();
        assert_eq!(recs.len(), 2, "limit respected");
        assert_eq!(recs[0].start_ts, 30, "newest first");
        assert_eq!(recs[1].start_ts, 20);
    }

    #[test]
    fn list_limit_clamps_to_100() {
        let (s, _d) = store();
        let f = RunFilter { limit: 9999, ..Default::default() };
        // No panic, clamp applied (empty store → empty result).
        assert!(s.list(&f).unwrap().is_empty());
    }
}
