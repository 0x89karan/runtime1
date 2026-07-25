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

use super::{BriefItem, BriefRecord, RunEvent, RunRecord};

/// segment_id → RunRecord JSON.
const RUNS: TableDefinition<&str, &str> = TableDefinition::new("runs");
/// agent_id → currently-open segment_id (idempotent-open index, G3).
const OPEN_BY_AGENT: TableDefinition<&str, &str> = TableDefinition::new("open_by_agent");
/// meta: "format_version" + "seq:{agent_id}" per-agent segment counter + "brief_seq" (ux.11c).
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
/// zero-padded brief seq → BriefRecord JSON (ux.11c). Lexical key order == seq order,
/// so `iter().next_back()` is the latest brief. Added additively — opening this table in
/// `init_schema`'s write txn creates it on an existing ux.11b `runs.redb` (no migration).
const BRIEFS: TableDefinition<&str, &str> = TableDefinition::new("briefs");

pub const RUNS_SCHEMA_VERSION: u64 = 1;

/// Retention bounds for the runs table (AUDIT-v0.97 P2-9). An always-on CoS otherwise accretes
/// run records forever, so `list()` / `publish_brief()` full-scans grow without limit. Pruning
/// (in the run_writer lane, after each close) keeps the table — and thus every scan — bounded.
/// Only CLOSED records are eligible; a live/open run is never pruned.
const MAX_RUNS: usize = 5_000;
const MAX_RUN_AGE_SECS: u64 = 90 * 24 * 3600; // 90 days

/// First-ever brief covers at most the last 24h (Eng G5) — a long-lived install must not
/// report "every run ever" on day one. Subsequent briefs derive `window_from` from the
/// previous brief's `window_to`.
const FIRST_BRIEF_LOOKBACK_SECS: u64 = 86_400;

/// Attention items retained per brief (Eng G7). `done` runs collapse into `overflow_count`;
/// attention items beyond this cap surface as `attention_overflow`. Counts are always
/// computed over the FULL window, never the clamped list.
const MAX_BRIEF_ITEMS: usize = 100;

/// Cap on the model-authored narrative (review H3): it is copied into the stored record and
/// re-parsed by the flight-event hook, so an unbounded narrative is a memory-amplification
/// vector. Truncated on a char boundary.
const MAX_NARRATIVE_CHARS: usize = 4096;

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
            // ux.11c: create the briefs table (additive — no-op if it already exists).
            let _briefs = txn.open_table(BRIEFS).context("opening briefs table")?;
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
        // P2-9: prune after a close (same writer lane → no concurrency). Best-effort: a prune
        // failure must not fail the close (the run record is already durably committed).
        if let Err(e) = self.prune(ts, MAX_RUNS, MAX_RUN_AGE_SECS) {
            tracing::warn!("runs prune failed (non-fatal): {e:#}");
        }
        Ok(())
    }

    /// Bound the runs table (AUDIT-v0.97 P2-9): remove CLOSED records older than `max_age_secs`
    /// or beyond the newest `max_runs`. Open records (end_ts=None) are never pruned. Runs in a
    /// dedicated write txn — call only from the run_writer lane (after a close), never
    /// concurrently with another write txn.
    fn prune(&self, now: u64, max_runs: usize, max_age_secs: u64) -> anyhow::Result<()> {
        let txn = self.db.begin_write().context("begin prune")?;
        {
            let mut runs_tbl = txn.open_table(RUNS).context("open runs for prune")?;
            // Collect closed records with a sort timestamp (prefer end_ts, fall back to start).
            let mut closed: Vec<(String, u64)> = Vec::new();
            for entry in runs_tbl.iter().context("iter runs for prune")? {
                let (k, v) = entry.context("read run entry")?;
                if let Ok(rec) = serde_json::from_str::<RunRecord>(v.value()) {
                    if let Some(end) = rec.end_ts {
                        closed.push((k.value().to_string(), end.max(rec.start_ts)));
                    }
                }
            }
            let cutoff = now.saturating_sub(max_age_secs);
            let mut remove: Vec<String> =
                closed.iter().filter(|(_, ts)| *ts < cutoff).map(|(k, _)| k.clone()).collect();
            if closed.len() > max_runs {
                let mut by_ts = closed.clone();
                by_ts.sort_by_key(|(_, ts)| *ts); // oldest first
                for (k, _) in by_ts.into_iter().take(closed.len() - max_runs) {
                    if !remove.contains(&k) {
                        remove.push(k);
                    }
                }
            }
            for k in remove {
                runs_tbl.remove(k.as_str()).context("prune remove")?;
            }
        }
        txn.commit().context("commit prune")?;
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

    // ───────────────────────────── ux.11c: morning brief ─────────────────────────────
    //
    // Concurrency (Eng G4): `publish_brief` takes its OWN `begin_write` on the shared db,
    // disjoint from `run_writer`'s tables — `run_writer` owns RUNS/OPEN_BY_AGENT and the
    // `seq:*`/`format_version` META keys; `publish_brief` reads RUNS and writes BRIEFS +
    // the `brief_seq` META key. redb serializes the two write txns (one at a time, no
    // nesting across threads) → no deadlock, worst case a single fsync wait once per cron.
    // The read+compose+insert is ONE txn, which is what makes "advance only on success"
    // (E7) atomic: a failed commit writes no row, so the next brief re-derives the same
    // `window_from` and re-covers the gap.

    /// Compose and persist a morning brief over `[window_from, window_to=now)`, where
    /// `window_from` is the previous brief's `window_to` (or `now − 24h` for the first
    /// brief). Facts are authored here from `runs.redb`; `narrative` is model color only.
    /// Returns the persisted record (the caller emits `BriefWritten` from it).
    pub fn publish_brief(&self, narrative: Option<String>, now: u64) -> anyhow::Result<BriefRecord> {
        // Bound the model-authored narrative before it is stored + re-emitted (H3).
        let narrative = narrative.map(|n| {
            if n.chars().count() > MAX_NARRATIVE_CHARS {
                n.chars().take(MAX_NARRATIVE_CHARS).collect::<String>()
            } else {
                n
            }
        });
        let txn = self.db.begin_write().context("begin publish_brief")?;
        let record;
        {
            // Prior brief → window_from (Eng G5). Newest key == largest zero-padded seq.
            let window_from = {
                let briefs_tbl = txn.open_table(BRIEFS).context("open briefs")?;
                // Bind the iterator result to a named local so its guard drops before the
                // table binding at block end (redb AccessGuard borrow-lifetime).
                let last = briefs_tbl.iter().context("iter briefs")?.next_back();
                match last {
                    Some(item) => {
                        let (_k, v) = item.context("read latest brief")?;
                        let prev: BriefRecord =
                            serde_json::from_str(v.value()).context("parse latest brief")?;
                        prev.window_to
                    }
                    None => now.saturating_sub(FIRST_BRIEF_LOOKBACK_SECS),
                }
            };
            // Clamp to `now` (review H4): if a prior brief persisted a future window_to
            // during a forward clock jump, a stale future window_from would exclude every
            // real completion below it forever. Clamping recovers within one cycle.
            let window_from = window_from.min(now);
            // Monotonic brief seq from META (same idiom as per-agent `seq:{agent_id}`).
            let next_seq = {
                let meta_tbl = txn.open_table(META).context("open meta")?;
                let seq = meta_tbl.get("brief_seq").context("read brief_seq")?.map(|g| g.value()).unwrap_or(0);
                seq
            };
            let window_to = now.max(window_from); // never an inverted window

            // Scan RUNS once. A run belongs to this window if it reached a terminal state
            // inside [from,to) OR it is still running (Eng G2: window by completion, not
            // start_ts — else an overnight-completed failure that started before the window
            // is silently dropped from every brief). "still running" is UNCONDITIONAL on
            // start time (matches the ratified plan): a long-running or hung child that
            // started before the window is exactly what "trust after absence" must surface,
            // so it must not be filtered out. The one thing to exclude is a perpetual
            // config-seed orchestrator (the always-on CoS itself) — excluded by IDENTITY
            // (start_reason), never by start_ts.
            let mut matched: Vec<RunRecord> = Vec::new();
            {
                let runs_tbl = txn.open_table(RUNS).context("open runs")?;
                for item in runs_tbl.iter().context("iter runs")? {
                    let (_k, v) = item.context("read run item")?;
                    let rec: RunRecord = match serde_json::from_str(v.value()) {
                        Ok(r) => r,
                        Err(_) => continue,
                    };
                    let terminal_in_window = rec
                        .end_ts
                        .is_some_and(|e| e >= window_from && e < window_to);
                    let still_running = rec.end_ts.is_none()
                        && rec.status == "running"
                        && rec.start_reason != "config_seed";
                    if terminal_in_window || still_running {
                        matched.push(rec);
                    }
                }
            }

            // Aggregates over the FULL matched set (Eng G7).
            let run_count = matched.len() as u64;
            let failed_count = matched.iter().filter(|r| r.status == "failed").count() as u64;
            let spend_total: u64 = matched.iter().filter_map(|r| r.spend).sum();

            // Attention items = non-`done`, newest-first; retain all failures/blocked/running
            // up to the cap. `done` runs + any overflow collapse into overflow_count.
            let mut attention: Vec<&RunRecord> =
                matched.iter().filter(|r| r.status != "done").collect();
            attention.sort_by(|a, b| {
                let a_ts = a.end_ts.unwrap_or(a.start_ts);
                let b_ts = b.end_ts.unwrap_or(b.start_ts);
                b_ts.cmp(&a_ts).then(b.segment_seq.cmp(&a.segment_seq))
            });
            let items: Vec<BriefItem> = attention
                .iter()
                .take(MAX_BRIEF_ITEMS)
                .map(|r| BriefItem {
                    run_id:      r.segment_id.clone(),
                    agent_id:    r.agent_id.clone(),
                    status:      r.status.clone(),
                    spend:       r.spend,
                    stop_reason: r.stop_reason.clone(),
                    last_error:  r.last_error.clone(),
                })
                .collect();
            // overflow_count is the genuinely-ok (done) runs only — never truncated
            // failures (review M1: mislabeling failures as "ok" is a trust-surface bug).
            let overflow_count = matched.iter().filter(|r| r.status == "done").count() as u64;
            let attention_overflow = (attention.len() as u64).saturating_sub(items.len() as u64);

            record = BriefRecord {
                brief_id: format!("brief:{next_seq}"),
                created_at: now,
                window_from,
                window_to,
                run_count,
                failed_count,
                spend_total,
                items,
                overflow_count,
                attention_overflow,
                narrative,
            };

            let json = serde_json::to_string(&record).context("serialize brief")?;
            {
                let mut briefs_tbl = txn.open_table(BRIEFS).context("open briefs for write")?;
                briefs_tbl
                    .insert(brief_key(next_seq).as_str(), json.as_str())
                    .context("insert brief")?;
            }
            {
                let mut meta_tbl = txn.open_table(META).context("open meta")?;
                meta_tbl.insert("brief_seq", next_seq + 1).context("bump brief_seq")?;
            }
        }
        txn.commit().context("commit publish_brief")?;
        Ok(record)
    }

    /// The most recent persisted brief, if any (pull surface / `GET /api/v1/brief`).
    pub fn latest_brief(&self) -> anyhow::Result<Option<BriefRecord>> {
        let txn = self.db.begin_read().context("begin read")?;
        let briefs_tbl = txn.open_table(BRIEFS).context("open briefs")?;
        let last = briefs_tbl.iter().context("iter briefs")?.next_back();
        match last {
            Some(item) => {
                let (_k, v) = item.context("read latest brief")?;
                Ok(Some(serde_json::from_str(v.value()).context("parse brief")?))
            }
            None => Ok(None),
        }
    }

    /// The `n` most recent briefs, newest-first (clamped to `[1, 100]`, default 20).
    pub fn list_briefs(&self, n: usize) -> anyhow::Result<Vec<BriefRecord>> {
        let limit = if n == 0 { 20 } else { n.min(100) };
        let txn = self.db.begin_read().context("begin read")?;
        let briefs_tbl = txn.open_table(BRIEFS).context("open briefs")?;
        let mut out: Vec<BriefRecord> = Vec::new();
        for item in briefs_tbl.iter().context("iter briefs")?.rev() {
            let (_k, v) = item.context("read brief item")?;
            match serde_json::from_str::<BriefRecord>(v.value()) {
                Ok(b) => out.push(b),
                Err(_) => continue,
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
}

/// Zero-padded brief key so lexical order matches numeric seq order.
fn brief_key(seq: u64) -> String {
    format!("{seq:020}")
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
    fn prune_bounds_by_count_and_age_and_never_prunes_open() {
        // AUDIT-v0.97 P2-9: retention keeps the runs table bounded. Count + age caps prune
        // only CLOSED records; a live/open run always survives.
        let (s, _d) = store();
        for (a, ts) in [("a1", 100u64), ("a2", 200), ("a3", 300)] {
            s.open_segment(a, None, "child_spawn", Some(0), "native", ts).unwrap();
            s.close_segment(a, "done", Some("completed".into()), None, Some(0), ts + 1).unwrap();
        }
        s.open_segment("live", None, "config_seed", Some(0), "native", 50).unwrap(); // in-progress

        // Count cap = 2 → the oldest closed (a1) is pruned; a2/a3 + the open one remain.
        s.prune(1_000, 2, u64::MAX).unwrap();
        assert!(s.get("a1:0").unwrap().is_none(), "oldest closed pruned by count cap");
        assert!(s.get("a2:0").unwrap().is_some());
        assert!(s.get("a3:0").unwrap().is_some());
        assert!(s.latest_open("live").unwrap().is_some(), "open record never pruned");

        // Age cap: now=1000, max_age=750 → cutoff 250 → a2 (end 201) pruned, a3 (end 301) kept.
        s.prune(1_000, usize::MAX, 750).unwrap();
        assert!(s.get("a2:0").unwrap().is_none(), "aged-out closed pruned");
        assert!(s.get("a3:0").unwrap().is_some());
        assert!(s.latest_open("live").unwrap().is_some(), "open still not pruned after age prune");
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

    // ───────────────────────────── ux.11c: brief tests ─────────────────────────────

    #[test]
    fn quiet_night_persists_a_row_and_advances_window() {
        // G6: 0 runs still writes a brief — presence is the liveness signal.
        let (s, _d) = store();
        let b0 = s.publish_brief(None, 100_000).unwrap();
        assert_eq!(b0.run_count, 0);
        assert!(b0.items.is_empty());
        assert_eq!(b0.overflow_count, 0);
        assert_eq!(b0.brief_id, "brief:0");
        assert_eq!(b0.window_from, 100_000 - 86_400, "first-ever window floors at now−24h");
        assert_eq!(b0.window_to, 100_000);
        // Second brief: window_from = prior window_to; seq advances.
        let b1 = s.publish_brief(None, 200_000).unwrap();
        assert_eq!(b1.brief_id, "brief:1");
        assert_eq!(b1.window_from, 100_000, "advance: from = prior window_to");
        assert_eq!(b1.window_to, 200_000);
        // latest_brief returns the newest.
        assert_eq!(s.latest_brief().unwrap().unwrap().brief_id, "brief:1");
    }

    #[test]
    fn brief_windows_by_completion_not_start() {
        // G2 (the trust-critical regression): a run that STARTED before the window but
        // COMPLETED (failed) inside it must appear; a still-running child (even one started
        // before the window — a hung/long run) must appear; only the perpetual config-seed
        // orchestrator is excluded, by IDENTITY not by start_ts (review Claude #1).
        let (s, _d) = store();
        // A: done inside window (start & end in window), spend 30.
        s.open_segment("A", None, "child_spawn", Some(0), "native", 20_000).unwrap();
        s.close_segment("A", "done", Some("ok".into()), None, Some(30), 20_050).unwrap();
        // B: perpetual seed — started before window, still running → excluded by identity.
        s.open_segment("B", None, "config_seed", Some(0), "native", 1_000).unwrap();
        // C: started inside window, still running → included as running.
        s.open_segment("C", None, "child_spawn", Some(0), "native", 50_000).unwrap();
        // D: started BEFORE window, failed INSIDE it, spend 70.
        s.open_segment("D", None, "child_spawn", Some(0), "native", 5_000).unwrap();
        s.close_segment("D", "failed", Some("boom".into()), Some("kaboom".into()), Some(70), 60_000).unwrap();
        // E: the fix — a child that STARTED before the window and is STILL running (hung)
        // must NOT be filtered out just because start_ts < window_from.
        s.open_segment("E", None, "child_spawn", Some(0), "native", 2_000).unwrap();

        let b = s.publish_brief(None, 100_000).unwrap();
        assert_eq!(b.run_count, 4, "A(done)+C(running)+D(failed)+E(hung running); B(seed) excluded");
        assert_eq!(b.failed_count, 1, "D");
        assert_eq!(b.spend_total, 100, "A 30 + D 70; running runs have no spend");
        // Attention items = non-done (C, D, E).
        assert_eq!(b.items.len(), 3);
        assert!(b.items.iter().any(|i| i.run_id == "D:0" && i.status == "failed"
            && i.last_error.as_deref() == Some("kaboom")));
        assert!(b.items.iter().any(|i| i.agent_id == "C" && i.status == "running"));
        assert!(b.items.iter().any(|i| i.agent_id == "E" && i.status == "running"),
            "hung child started before window must still surface");
        assert!(!b.items.iter().any(|i| i.agent_id == "A"), "done run is not an attention item");
        assert!(!b.items.iter().any(|i| i.agent_id == "B"), "perpetual seed excluded entirely");
        assert_eq!(b.overflow_count, 1, "overflow_count = done/ok runs only = A");
        assert_eq!(b.attention_overflow, 0, "under the 100-item cap");
    }

    #[test]
    fn brief_overflow_never_labels_failures_as_ok() {
        // Review M1: with >MAX_BRIEF_ITEMS failures, truncated failures must land in
        // attention_overflow, NOT overflow_count (which is "✓ N ok" and done-only).
        let (s, _d) = store();
        for i in 0..(MAX_BRIEF_ITEMS + 20) {
            let id = format!("f{i}");
            s.open_segment(&id, None, "child_spawn", Some(0), "native", 20_000 + i as u64).unwrap();
            s.close_segment(&id, "failed", None, Some("x".into()), Some(1), 20_500 + i as u64).unwrap();
        }
        // Plus a couple of genuine ok runs.
        for i in 0..3 {
            let id = format!("ok{i}");
            s.open_segment(&id, None, "child_spawn", Some(0), "native", 30_000 + i as u64).unwrap();
            s.close_segment(&id, "done", None, None, Some(1), 30_500 + i as u64).unwrap();
        }
        let b = s.publish_brief(None, 100_000).unwrap();
        assert_eq!(b.failed_count, (MAX_BRIEF_ITEMS + 20) as u64);
        assert_eq!(b.items.len(), MAX_BRIEF_ITEMS, "attention capped");
        assert_eq!(b.attention_overflow, 20, "truncated failures surfaced, not hidden");
        assert_eq!(b.overflow_count, 3, "✓ N ok counts only the done runs");
    }

    #[test]
    fn brief_narrative_is_preserved_facts_are_authored() {
        let (s, _d) = store();
        s.open_segment("X", None, "child_spawn", Some(0), "native", 20_000).unwrap();
        s.close_segment("X", "done", None, None, Some(5), 20_010).unwrap();
        let b = s.publish_brief(Some("all quiet, one sync".into()), 100_000).unwrap();
        assert_eq!(b.narrative.as_deref(), Some("all quiet, one sync"));
        assert_eq!(b.run_count, 1, "fact authored from the store, not the narrative");
    }

    #[test]
    fn list_briefs_newest_first() {
        let (s, _d) = store();
        s.publish_brief(None, 100_000).unwrap();
        s.publish_brief(None, 200_000).unwrap();
        s.publish_brief(None, 300_000).unwrap();
        let recent = s.list_briefs(2).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].brief_id, "brief:2", "newest first");
        assert_eq!(recent[1].brief_id, "brief:1");
    }
}
