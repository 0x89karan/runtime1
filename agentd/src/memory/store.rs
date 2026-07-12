use std::path::{Path, PathBuf};

use anyhow::Context;
use redb::{
    Database, DatabaseError, ReadableDatabase, ReadableTable, ReadableTableMetadata, StorageError,
    TableDefinition,
};

use crate::memory::{index, EvictedEntry, MemoryStore, MutabilityClass, SearchHit, SCHEMA_VERSION};

/// Composite entry key: `"{namespace}\x00{key}"`.
/// The `\x00` separator is safe because our namespace/key grammar disallows
/// null bytes — so splitting on the first `\x00` is unambiguous.
const ENTRIES: TableDefinition<&str, &str> = TableDefinition::new("entries");
/// Inverted index: key = `"{namespace}\x00{word}"`, value = JSON array of entry keys.
const INDEX: TableDefinition<&str, &str> = TableDefinition::new("index");
/// Write timestamp per entry: key = composite entry key, value = Unix seconds.
const AGE: TableDefinition<&str, u64> = TableDefinition::new("age");
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
/// Namespace → entry count. Maintained atomically with every put/append/delete.
/// Enables O(k) list_namespaces() (k = segment count) instead of O(n) ENTRIES scan.
/// Backfilled from ENTRIES on first open of existing stores (single write transaction).
const NAMESPACES: TableDefinition<&str, u64> = TableDefinition::new("namespaces");

const SEG_CLASS_PREFIX: &str = "seg_class:";
const LOG_SEQ_PREFIX: &str = "log_seq:";
const SCRATCH_VER_PREFIX: &str = "scratch_ver:";
const DOC_COUNT_PREFIX: &str = "doc_count:";
/// Per-segment eviction floor (F-03). `0` is the sentinel for "unset".
const SEG_MAX_ENTRIES_PREFIX: &str = "seg_max_entries:";
const SEG_MAX_AGE_PREFIX: &str = "seg_max_age_secs:";

fn entry_key(namespace: &str, key: &str) -> String {
    format!("{}\x00{}", namespace, key)
}

fn index_key(namespace: &str, word: &str) -> String {
    format!("{}\x00{}", namespace, word)
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct RedbStore {
    db: Database,
}

/// Why an open attempt failed — drives the F-02 quarantine decision.
///
/// Only `Corrupt` (a confirmed `StorageError::Corrupted` from redb) is safe to
/// quarantine; `Locked` and `Other` (permissions, transient I/O, upgrade
/// required, schema init) must leave a potentially-valid file in place.
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

/// Classify a redb `DatabaseError` from `Database::open` to decide whether
/// quarantine is safe.
///
/// Quarantine ONLY when the file's bytes are genuinely not a usable redb
/// database:
///   - `StorageError::Corrupted` — redb's explicit corruption signal, and
///   - `StorageError::Io(InvalidData)` — what redb returns for a file whose
///     header/magic isn't a valid redb database (e.g. truncated or non-redb).
///
/// Everything else must leave a potentially-valid file in place:
///   - `DatabaseAlreadyOpen` → another process holds the lock (Locked),
///   - other `Io` kinds (PermissionDenied, NotFound, transient disk errors),
///     `UpgradeRequired` (a valid OLD store), `RepairAborted`,
///     `TransactionInProgress`, etc. → Other.
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

impl RedbStore {
    /// Open or create the store at `path`.
    ///
    /// Returns `(store, None)` on clean open, or `(store, Some(corrupt_path))`
    /// when the original file was quarantined and a fresh store was created.
    ///
    /// Returns `Err` when the file is held by another process or is
    /// unrecoverable without quarantine (e.g. permission denied).
    pub fn open(path: &Path) -> anyhow::Result<(Self, Option<PathBuf>)> {
        match Self::try_open(path) {
            Ok(store) => Ok((store, None)),
            // F-02: NEVER quarantine on a lock or transient/permission error — a
            // valid store hit by such an error must be left untouched. Only a
            // CONFIRMED redb corruption variant (StorageError::Corrupted) is
            // safe to quarantine.
            Err(OpenFailure::Locked(e)) => Err(e.context(
                "memory.redb is held by another process; stop the other \
                 agentd instance or set a unique memory.store_path",
            )),
            Err(OpenFailure::Other(e)) => Err(e.context(
                "memory store could not be opened; this is NOT corruption, so the \
                 file was left in place (fix the underlying cause — e.g. \
                 permissions, disk, or an old format requiring upgrade — and retry)",
            )),
            Err(OpenFailure::Corrupt(e)) => {
                // Confirmed corruption — quarantine the file and start fresh.
                let corrupt_path = Self::quarantine_path(path);
                std::fs::rename(path, &corrupt_path)
                    .with_context(|| {
                        format!("quarantining corrupt store: {path:?} → {corrupt_path:?}")
                    })
                    .with_context(|| format!("original corruption: {e:#}"))?;
                let store = Self::try_open(path)
                    .map_err(OpenFailure::into_inner)
                    .context("opening fresh store after quarantine")?;
                Ok((store, Some(corrupt_path)))
            }
        }
    }

    /// Unique, timestamped quarantine path so a second quarantine can't clobber
    /// the evidence from the first (F-02).
    fn quarantine_path(path: &Path) -> PathBuf {
        let ts = unix_now_secs();
        let name = path
            .file_name()
            .map(|n| format!("{}.{ts}.corrupt", n.to_string_lossy()))
            .unwrap_or_else(|| format!("memory.redb.{ts}.corrupt"));
        path.parent().unwrap_or(Path::new(".")).join(name)
    }

    fn try_open(path: &Path) -> Result<Self, OpenFailure> {
        // redb 4.x splits open (existing) and create (new) — implement open-or-create.
        // Classify the open/create error so the caller knows whether quarantine is
        // safe: only StorageError::Corrupted is confirmed corruption.
        let db = if path.exists() {
            Database::open(path).map_err(classify_db_error)?
        } else {
            // A create failure means there's no existing file to quarantine.
            Database::create(path).map_err(|e| {
                OpenFailure::Other(
                    anyhow::Error::new(e)
                        .context(format!("creating memory store at {path:?}")),
                )
            })?
        };

        // Set mode 0600 immediately after open so the file is not world-readable.
        // A chmod failure is NOT corruption — surface it as Other (no quarantine).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .map_err(|e| OpenFailure::Other(anyhow::Error::new(e).context("reading store file metadata")))?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms).map_err(|e| {
                OpenFailure::Other(anyhow::Error::new(e).context("setting store permissions to 0600"))
            })?;
        }

        let store = Self { db };
        // A schema-init failure is also not corruption of the file format.
        store
            .init_schema()
            .map_err(|e| OpenFailure::Other(e.context("initialising store schema")))?;
        Ok(store)
    }

    fn init_schema(&self) -> anyhow::Result<()> {
        let txn = self.db.begin_write().context("beginning schema init transaction")?;
        {
            // Open all tables to ensure they exist before any read.
            let _entries = txn.open_table(ENTRIES).context("opening entries table")?;
            let _index = txn.open_table(INDEX).context("opening index table")?;
            let _age = txn.open_table(AGE).context("opening age table")?;
            let mut meta = txn.open_table(META).context("opening meta table")?;
            if meta
                .get("format_version")
                .context("reading format_version")?
                .is_none()
            {
                meta.insert("format_version", SCHEMA_VERSION)
                    .context("writing format_version")?;
            }
            let _ns = txn.open_table(NAMESPACES).context("opening namespaces table")?;
        }
        txn.commit().context("committing schema init")?;

        // One-time backfill: if NAMESPACES is empty but ENTRIES is not, this is an
        // existing store from before p5.8.  Rebuild NAMESPACES in a single atomic
        // transaction (all-or-nothing — a partial backfill followed by a crash would
        // leave NAMESPACES non-empty on next open, skipping this guard permanently).
        let needs_backfill = {
            let rtxn = self.db.begin_read().context("beginning read for backfill check")?;
            let ns_tbl = rtxn.open_table(NAMESPACES).context("checking namespaces table")?;
            let entries_tbl = rtxn.open_table(ENTRIES).context("checking entries table")?;
            let ns_empty = ns_tbl.is_empty().context("checking namespaces empty")?;
            let entries_nonempty = !entries_tbl.is_empty().context("checking entries empty")?;
            ns_empty && entries_nonempty
        };

        if needs_backfill {
            let rtxn = self.db.begin_read().context("beginning read for backfill scan")?;
            let entries_tbl = rtxn.open_table(ENTRIES).context("opening entries for backfill")?;
            let mut counts: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();
            let mut entry_count: u64 = 0;
            for item in entries_tbl.iter().context("iterating entries for backfill")? {
                let (k_guard, _) = item.context("reading entry for backfill")?;
                let k = k_guard.value();
                if let Some(sep) = k.find('\x00') {
                    *counts.entry(k[..sep].to_string()).or_insert(0) += 1;
                }
                entry_count += 1;
            }
            drop(rtxn);
            tracing::info!(
                entries = entry_count,
                namespaces = counts.len(),
                "namespaces: backfilling from existing store entries"
            );
            // Backfill write is non-fatal: a transient I/O failure (ENOSPC, etc.)
            // must not quarantine a valid store.  On failure, list_namespaces()
            // falls back to O(n) scan until the next restart succeeds.
            let backfill_write: anyhow::Result<()> = (|| {
                let wtxn = self.db.begin_write().context("beginning write for backfill")?;
                {
                    let mut ns_tbl = wtxn
                        .open_table(NAMESPACES)
                        .context("opening namespaces for backfill")?;
                    for (ns, count) in &counts {
                        ns_tbl
                            .insert(ns.as_str(), *count)
                            .context("writing backfill namespace")?;
                    }
                }
                wtxn.commit().context("committing backfill")?;
                Ok(())
            })();
            if let Err(e) = backfill_write {
                tracing::warn!(
                    error = %e,
                    "namespaces: backfill write failed; list_namespaces falls back to \
                     O(n) scan until next restart — store remains usable"
                );
            }
        }

        Ok(())
    }

    /// Read the posting list for `word` in `namespace` from the INDEX table.
    /// Returns an empty Vec when absent.
    fn read_posting_list(
        index_table: &redb::Table<&str, &str>,
        namespace: &str,
        word: &str,
    ) -> anyhow::Result<Vec<String>> {
        let k = index_key(namespace, word);
        let raw = index_table
            .get(k.as_str())
            .context("reading posting list")?
            .map(|g| g.value().to_string());
        match raw {
            None => Ok(vec![]),
            Some(json) => {
                let list: Vec<String> =
                    serde_json::from_str(&json).context("deserializing posting list")?;
                Ok(list)
            }
        }
    }

    /// Write `posting_list` for `word` in `namespace` back to the INDEX table.
    /// Removes the entry when the list is empty.
    fn write_posting_list(
        index_table: &mut redb::Table<&str, &str>,
        namespace: &str,
        word: &str,
        posting_list: &[String],
    ) -> anyhow::Result<()> {
        let k = index_key(namespace, word);
        if posting_list.is_empty() {
            index_table.remove(k.as_str()).context("removing empty posting list")?;
        } else {
            let json = serde_json::to_string(posting_list).context("serializing posting list")?;
            index_table
                .insert(k.as_str(), json.as_str())
                .context("writing posting list")?;
        }
        Ok(())
    }

    /// Add `entry_key` to the posting lists for all `tokens` in `namespace`.
    fn index_tokens(
        index_table: &mut redb::Table<&str, &str>,
        namespace: &str,
        entry_key_str: &str,
        tokens: &[String],
    ) -> anyhow::Result<()> {
        // Deduplicate tokens so a repeated word doesn't duplicate entries in posting list.
        let mut seen = std::collections::HashSet::new();
        for token in tokens {
            if !seen.insert(token) {
                continue;
            }
            let mut posting = Self::read_posting_list(index_table, namespace, token)?;
            if !posting.contains(&entry_key_str.to_string()) {
                posting.push(entry_key_str.to_string());
                Self::write_posting_list(index_table, namespace, token, &posting)?;
            }
        }
        Ok(())
    }

    /// Remove `entry_key` from the posting lists for all `tokens` in `namespace`.
    fn deindex_tokens(
        index_table: &mut redb::Table<&str, &str>,
        namespace: &str,
        entry_key_str: &str,
        tokens: &[String],
    ) -> anyhow::Result<()> {
        let mut seen = std::collections::HashSet::new();
        for token in tokens {
            if !seen.insert(token) {
                continue;
            }
            let mut posting = Self::read_posting_list(index_table, namespace, token)?;
            posting.retain(|k| k != entry_key_str);
            Self::write_posting_list(index_table, namespace, token, &posting)?;
        }
        Ok(())
    }

    fn put_at(&self, namespace: &str, key: &str, value: &str, now_secs: u64) -> anyhow::Result<()> {
        let ek = entry_key(namespace, key);
        let new_tokens = index::tokenize(value);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        {
            let mut entries_tbl = txn.open_table(ENTRIES).context("opening entries table")?;
            let mut index_tbl = txn.open_table(INDEX).context("opening index table")?;
            let mut age_tbl = txn.open_table(AGE).context("opening age table")?;
            let mut meta_tbl = txn.open_table(META).context("opening meta table")?;
            let mut ns_tbl = txn.open_table(NAMESPACES).context("opening namespaces table")?;

            let old_value = entries_tbl
                .get(ek.as_str())
                .context("reading old entry for put")?
                .map(|g| g.value().to_string());

            // is_new drives both doc_count and NAMESPACES counter — only increment
            // on actual new key insertion, not on overwrites (OV-1 guard).
            let is_new = old_value.is_none();
            if let Some(ref old_v) = old_value {
                let old_tokens = index::tokenize(old_v);
                Self::deindex_tokens(&mut index_tbl, namespace, ek.as_str(), &old_tokens)
                    .context("deindexing old tokens on put")?;
            }

            entries_tbl.insert(ek.as_str(), value).context("inserting entry")?;
            age_tbl.insert(ek.as_str(), now_secs).context("writing age")?;
            Self::index_tokens(&mut index_tbl, namespace, ek.as_str(), &new_tokens)
                .context("indexing new tokens on put")?;

            if is_new {
                let doc_count_key = format!("{DOC_COUNT_PREFIX}{namespace}");
                let cur = meta_tbl
                    .get(doc_count_key.as_str())
                    .context("reading doc count")?
                    .map(|g| g.value())
                    .unwrap_or(0);
                meta_tbl
                    .insert(doc_count_key.as_str(), cur + 1)
                    .context("incrementing doc count")?;

                let ns_cur = ns_tbl
                    .get(namespace)
                    .context("reading namespace count")?
                    .map(|g| g.value())
                    .unwrap_or(0);
                ns_tbl
                    .insert(namespace, ns_cur + 1)
                    .context("incrementing namespace count")?;
            }
        }
        txn.commit().context("committing put")?;
        self.debug_assert_counters(namespace);
        // F-03: best-effort eviction — a trim failure must not fail the write.
        if let Err(e) = self.enforce_segment_limits(namespace, now_secs) {
            tracing::warn!(namespace, error = %e, "segment eviction after put failed");
        }
        Ok(())
    }

    fn append_at(&self, namespace: &str, key: &str, value: &str, now_secs: u64) -> anyhow::Result<()> {
        let ek = entry_key(namespace, key);
        let new_tokens = index::tokenize(value);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        {
            let mut entries_tbl = txn.open_table(ENTRIES).context("opening entries table")?;
            let mut index_tbl = txn.open_table(INDEX).context("opening index table")?;
            let mut age_tbl = txn.open_table(AGE).context("opening age table")?;
            let mut meta_tbl = txn.open_table(META).context("opening meta table")?;
            let mut ns_tbl = txn.open_table(NAMESPACES).context("opening namespaces table")?;

            let old_value_opt = entries_tbl
                .get(ek.as_str())
                .context("reading for append")?
                .map(|g| g.value().to_string());

            let is_new = old_value_opt.is_none();
            let old_value = old_value_opt.unwrap_or_default();
            let combined = if is_new {
                value.to_string()
            } else {
                format!("{}\n{}", old_value, value)
            };

            entries_tbl
                .insert(ek.as_str(), combined.as_str())
                .context("inserting appended entry")?;
            age_tbl.insert(ek.as_str(), now_secs).context("writing age for append")?;

            // Only index the newly-appended portion; old tokens already indexed.
            Self::index_tokens(&mut index_tbl, namespace, ek.as_str(), &new_tokens)
                .context("indexing appended tokens")?;

            if is_new {
                let doc_count_key = format!("{DOC_COUNT_PREFIX}{namespace}");
                let cur = meta_tbl
                    .get(doc_count_key.as_str())
                    .context("reading doc count")?
                    .map(|g| g.value())
                    .unwrap_or(0);
                meta_tbl
                    .insert(doc_count_key.as_str(), cur + 1)
                    .context("incrementing doc count")?;

                let ns_cur = ns_tbl
                    .get(namespace)
                    .context("reading namespace count")?
                    .map(|g| g.value())
                    .unwrap_or(0);
                ns_tbl
                    .insert(namespace, ns_cur + 1)
                    .context("incrementing namespace count")?;
            }
        }
        txn.commit().context("committing append")?;
        self.debug_assert_counters(namespace);
        // F-03: best-effort eviction — a trim failure must not fail the write.
        if let Err(e) = self.enforce_segment_limits(namespace, now_secs) {
            tracing::warn!(namespace, error = %e, "segment eviction after append failed");
        }
        Ok(())
    }

    /// F-03: apply the persisted per-segment eviction floor on the live write
    /// path. No-op when no limits are configured or the segment is `canon`.
    /// Best-effort: an eviction failure must not fail the write that triggered
    /// it, so callers log-and-continue on `Err`.
    fn enforce_segment_limits(&self, namespace: &str, now_secs: u64) -> anyhow::Result<()> {
        let entries_key = format!("{SEG_MAX_ENTRIES_PREFIX}{namespace}");
        let age_key = format!("{SEG_MAX_AGE_PREFIX}{namespace}");
        let (max_entries_raw, max_age_raw) = {
            let txn = self.db.begin_read().context("beginning read for segment limits")?;
            let table = txn.open_table(META).context("opening meta table")?;
            let me = table
                .get(entries_key.as_str())
                .context("reading segment max_entries")?
                .map(|g| g.value())
                .unwrap_or(0);
            let ma = table
                .get(age_key.as_str())
                .context("reading segment max_age")?
                .map(|g| g.value())
                .unwrap_or(0);
            (me, ma)
        };
        // 0 is the unset sentinel for both dimensions.
        let max_entries = (max_entries_raw > 0).then_some(max_entries_raw as usize);
        let max_age_secs = (max_age_raw > 0).then_some(max_age_raw);
        if max_entries.is_none() && max_age_secs.is_none() {
            return Ok(());
        }
        // evict() itself early-returns on canon, so no extra class check needed.
        self.evict(namespace, max_entries, max_age_secs, now_secs)?;
        Ok(())
    }

    /// Read the persisted NAMESPACES counter for `namespace` (0 if absent).
    /// Used by the F-04 counter reconciliation (debug builds) and tests; in a
    /// release build with assertions off it has no caller.
    #[allow(dead_code)]
    pub(crate) fn namespace_count(&self, namespace: &str) -> anyhow::Result<u64> {
        let txn = self.db.begin_read().context("beginning read for namespace count")?;
        let table = txn.open_table(NAMESPACES).context("opening namespaces table")?;
        let n = table
            .get(namespace)
            .context("reading namespace count")?
            .map(|g| g.value())
            .unwrap_or(0);
        Ok(n)
    }

    /// F-04: in debug builds, assert the persisted NAMESPACES counter matches the
    /// actual entry-key count for `namespace`. Compiled out in release (the live
    /// path stays allocation-free; the invariant is enforced in tests + dev).
    #[cfg(debug_assertions)]
    fn debug_assert_counters(&self, namespace: &str) {
        let actual = self.list_keys(namespace).map(|k| k.len()).unwrap_or(0);
        let counter = self.namespace_count(namespace).unwrap_or(0) as usize;
        debug_assert_eq!(
            counter, actual,
            "NAMESPACES counter drift for {namespace}: counter={counter}, actual={actual}"
        );
    }

    #[cfg(not(debug_assertions))]
    #[inline]
    fn debug_assert_counters(&self, _namespace: &str) {}
}

impl MemoryStore for RedbStore {
    fn get(&self, namespace: &str, key: &str) -> anyhow::Result<Option<String>> {
        let k = entry_key(namespace, key);
        let txn = self.db.begin_read().context("beginning read transaction")?;
        let table = txn.open_table(ENTRIES).context("opening entries table")?;
        let result = table
            .get(k.as_str())
            .context("reading entry")?
            .map(|guard| guard.value().to_string());
        Ok(result)
    }

    fn put(&self, namespace: &str, key: &str, value: &str) -> anyhow::Result<()> {
        self.put_at(namespace, key, value, unix_now_secs())
    }

    fn append(&self, namespace: &str, key: &str, value: &str) -> anyhow::Result<()> {
        self.append_at(namespace, key, value, unix_now_secs())
    }

    fn delete(&self, namespace: &str, key: &str) -> anyhow::Result<bool> {
        let ek = entry_key(namespace, key);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        let existed = {
            let mut entries_tbl = txn.open_table(ENTRIES).context("opening entries table")?;
            let mut index_tbl = txn.open_table(INDEX).context("opening index table")?;
            let mut age_tbl = txn.open_table(AGE).context("opening age table")?;
            let mut meta_tbl = txn.open_table(META).context("opening meta table")?;
            let mut ns_tbl = txn.open_table(NAMESPACES).context("opening namespaces table")?;

            let old_value = entries_tbl
                .get(ek.as_str())
                .context("reading for delete")?
                .map(|g| g.value().to_string());

            if let Some(ref old_v) = old_value {
                let old_tokens = index::tokenize(old_v);
                Self::deindex_tokens(&mut index_tbl, namespace, ek.as_str(), &old_tokens)
                    .context("deindexing deleted entry")?;
                entries_tbl.remove(ek.as_str()).context("deleting entry")?;
                age_tbl.remove(ek.as_str()).context("removing age entry")?;
                let doc_count_key = format!("{DOC_COUNT_PREFIX}{namespace}");
                let cur = meta_tbl
                    .get(doc_count_key.as_str())
                    .context("reading doc count")?
                    .map(|g| g.value())
                    .unwrap_or(0);
                if cur > 0 {
                    meta_tbl
                        .insert(doc_count_key.as_str(), cur - 1)
                        .context("decrementing doc count")?;
                }
                // NAMESPACES counter: decrement and remove the key when count reaches 0
                // (no ghost namespace entries in FUSE kb/ after the last entry is deleted).
                let ns_cur = ns_tbl
                    .get(namespace)
                    .context("reading namespace count for delete")?
                    .map(|g| g.value())
                    .unwrap_or(1);
                if ns_cur <= 1 {
                    ns_tbl.remove(namespace).context("removing namespace at zero count")?;
                } else {
                    ns_tbl
                        .insert(namespace, ns_cur - 1)
                        .context("decrementing namespace count")?;
                }
                true
            } else {
                false
            }
        };
        txn.commit().context("committing delete")?;
        self.debug_assert_counters(namespace);
        Ok(existed)
    }

    fn iter(&self, namespace: &str) -> anyhow::Result<Vec<(String, String)>> {
        // Range: "namespace\x00" ≤ k < "namespace\x01"  (all keys in namespace)
        let prefix_start = format!("{}\x00", namespace);
        let prefix_end = format!("{}\x01", namespace);
        let txn = self.db.begin_read().context("beginning read transaction")?;
        let table = txn.open_table(ENTRIES).context("opening entries table")?;
        let range = table
            .range(prefix_start.as_str()..prefix_end.as_str())
            .context("iterating namespace")?;
        let ns_prefix_len = namespace.len() + 1; // +1 for \x00
        let mut results = Vec::new();
        for item in range {
            let (k_guard, v_guard) = item.context("reading range item")?;
            let k_str = k_guard.value();
            let key = k_str[ns_prefix_len..].to_string();
            let value = v_guard.value().to_string();
            results.push((key, value));
        }
        Ok(results)
    }

    fn list_namespaces(&self) -> anyhow::Result<Vec<String>> {
        let txn = self.db.begin_read().context("beginning read")?;
        let table = txn.open_table(NAMESPACES).context("opening namespaces table")?;
        let mut result = Vec::new();
        for item in table.iter().context("iterating namespaces")? {
            let (k_guard, _) = item.context("reading namespace entry")?;
            result.push(k_guard.value().to_string());
        }
        Ok(result)
    }

    fn list_keys(&self, namespace: &str) -> anyhow::Result<Vec<String>> {
        let prefix_start = format!("{}\x00", namespace);
        let prefix_end   = format!("{}\x01", namespace);
        let txn = self.db.begin_read().context("beginning read transaction")?;
        let table = txn.open_table(ENTRIES).context("opening entries table")?;
        let range = table
            .range(prefix_start.as_str()..prefix_end.as_str())
            .context("iterating namespace keys")?;
        let ns_prefix_len = namespace.len() + 1; // +1 for \x00
        let mut keys = Vec::new();
        for item in range {
            let (k_guard, _) = item.context("reading range item")?;
            let k_str = k_guard.value();
            keys.push(k_str[ns_prefix_len..].to_string());
        }
        Ok(keys)
    }

    fn meta_version(&self) -> anyhow::Result<u64> {
        let txn = self.db.begin_read().context("beginning read transaction")?;
        let table = txn.open_table(META).context("opening meta table")?;
        let v = table
            .get("format_version")
            .context("reading format_version")?
            .map(|g| g.value())
            .unwrap_or(0);
        Ok(v)
    }

    fn segment_class(&self, namespace: &str) -> anyhow::Result<Option<MutabilityClass>> {
        let meta_key = format!("{SEG_CLASS_PREFIX}{namespace}");
        let txn = self.db.begin_read().context("beginning read transaction")?;
        let table = txn.open_table(META).context("opening meta table")?;
        let result = table
            .get(meta_key.as_str())
            .context("reading segment class")?
            .and_then(|g| match g.value() {
                0 => Some(MutabilityClass::Canon),
                1 => Some(MutabilityClass::Log),
                2 => Some(MutabilityClass::Scratch),
                _ => None,
            });
        Ok(result)
    }

    fn set_segment_class(&self, namespace: &str, class: MutabilityClass) -> anyhow::Result<()> {
        let meta_key = format!("{SEG_CLASS_PREFIX}{namespace}");
        let encoded: u64 = match class {
            MutabilityClass::Canon => 0,
            MutabilityClass::Log => 1,
            MutabilityClass::Scratch => 2,
        };
        let txn = self.db.begin_write().context("beginning write transaction")?;
        {
            let mut table = txn.open_table(META).context("opening meta table")?;
            table
                .insert(meta_key.as_str(), encoded)
                .context("writing segment class")?;
            // Register namespace in NAMESPACES so list_namespaces() surfaces it even
            // before any entries are written (configured segments visible at startup).
            // Only insert if absent — do not overwrite the count for an existing namespace.
            let mut ns_tbl = txn.open_table(NAMESPACES).context("opening namespaces table")?;
            if ns_tbl.get(namespace).context("checking namespace")?.is_none() {
                ns_tbl.insert(namespace, 0u64).context("registering namespace")?;
            }
        }
        txn.commit().context("committing segment class")?;
        Ok(())
    }

    fn set_segment_limits(
        &self,
        namespace: &str,
        max_entries: Option<usize>,
        max_age_secs: Option<u64>,
    ) -> anyhow::Result<()> {
        let entries_key = format!("{SEG_MAX_ENTRIES_PREFIX}{namespace}");
        let age_key = format!("{SEG_MAX_AGE_PREFIX}{namespace}");
        // 0 is the "unset" sentinel.
        let entries_val = max_entries.map(|n| n as u64).unwrap_or(0);
        let age_val = max_age_secs.unwrap_or(0);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        {
            let mut table = txn.open_table(META).context("opening meta table")?;
            table
                .insert(entries_key.as_str(), entries_val)
                .context("writing segment max_entries")?;
            table
                .insert(age_key.as_str(), age_val)
                .context("writing segment max_age_secs")?;
        }
        txn.commit().context("committing segment limits")?;
        Ok(())
    }

    fn next_log_seq(&self, namespace: &str) -> anyhow::Result<u64> {
        let meta_key = format!("{LOG_SEQ_PREFIX}{namespace}");
        let txn = self.db.begin_write().context("beginning write transaction")?;
        let next = {
            let mut table = txn.open_table(META).context("opening meta table")?;
            let current = table
                .get(meta_key.as_str())
                .context("reading log seq")?
                .map(|g| g.value())
                .unwrap_or(0);
            let next = current + 1;
            table
                .insert(meta_key.as_str(), next)
                .context("writing log seq")?;
            next
        };
        txn.commit().context("committing log seq")?;
        Ok(next)
    }

    fn next_scratch_version(&self, namespace: &str, key: &str) -> anyhow::Result<u64> {
        let meta_key = format!("{SCRATCH_VER_PREFIX}{namespace}\x00{key}");
        let txn = self.db.begin_write().context("beginning write transaction")?;
        let next = {
            let mut table = txn.open_table(META).context("opening meta table")?;
            let current = table
                .get(meta_key.as_str())
                .context("reading scratch version")?
                .map(|g| g.value())
                .unwrap_or(0);
            let next = current + 1;
            table
                .insert(meta_key.as_str(), next)
                .context("writing scratch version")?;
            next
        };
        txn.commit().context("committing scratch version")?;
        Ok(next)
    }

    fn search(
        &self,
        namespace: Option<&str>,
        query: &str,
        author: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<(Vec<SearchHit>, usize)> {
        let ns = match namespace {
            Some(ns) => ns,
            None => anyhow::bail!("cross-segment search is not supported in MVP; provide a namespace"),
        };

        let mut query_terms = index::tokenize(query);
        query_terms.sort_unstable();
        query_terms.dedup();
        query_terms.truncate(64); // bound worst-case scoring work regardless of query length
        if query_terms.is_empty() {
            return Ok((vec![], 0));
        }
        let terms_matched = query_terms.len();

        let txn = self.db.begin_read().context("beginning read transaction for search")?;
        let entries_tbl = txn.open_table(ENTRIES).context("opening entries table")?;
        let index_tbl = txn.open_table(INDEX).context("opening index table")?;
        let meta_tbl = txn.open_table(META).context("opening meta table")?;

        let doc_count_key = format!("{DOC_COUNT_PREFIX}{ns}");
        let n_docs: f64 = meta_tbl
            .get(doc_count_key.as_str())
            .context("reading doc count")?
            .map(|g| g.value() as f64)
            .unwrap_or(0.0)
            .max(1.0);

        // Collect candidate entry composite-keys and df per query term.
        let mut candidate_keys: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut term_df: Vec<usize> = Vec::with_capacity(query_terms.len());

        for term in &query_terms {
            let ik = index_key(ns, term);
            let posting_list: Vec<String> =
                match index_tbl.get(ik.as_str()).context("reading posting list in search")? {
                    None => vec![],
                    Some(g) => serde_json::from_str(g.value())
                        .context("deserializing posting list in search")?,
                };
            term_df.push(posting_list.len());
            for ek in posting_list {
                candidate_keys.insert(ek);
            }
        }

        let ns_prefix = format!("{}\x00", ns);
        let mut hits: Vec<SearchHit> = Vec::new();

        for composite_key in &candidate_keys {
            let raw_value = match entries_tbl
                .get(composite_key.as_str())
                .context("fetching candidate in search")?
            {
                None => continue, // stale posting after concurrent delete
                Some(g) => g.value().to_string(),
            };

            // Author filter via provenance JSON field.
            if let Some(author_filter) = author {
                let passes = serde_json::from_str::<serde_json::Value>(&raw_value)
                    .ok()
                    .and_then(|v| {
                        v.get("provenance")
                            .and_then(|p| p.get("agent_id"))
                            .and_then(|a| a.as_str())
                            .map(|s| s == author_filter)
                    })
                    .unwrap_or(true); // no provenance → include
                if !passes {
                    continue;
                }
            }

            let doc_tokens = index::tokenize(&raw_value);
            let tfs = index::term_frequencies(&doc_tokens, &query_terms);

            // BM25-lite: score = Σ_t [ tf(t,d) × ln(1 + N/(1+df(t))) ]
            let mut score: f64 = 0.0;
            for (i, &tf) in tfs.iter().enumerate() {
                let df = term_df[i] as f64;
                let idf = (1.0_f64 + n_docs / (1.0 + df)).ln();
                score += (tf as f64) * idf;
            }

            if score <= 0.0 {
                continue;
            }

            let user_key = if composite_key.starts_with(&ns_prefix) {
                composite_key[ns_prefix.len()..].to_string()
            } else {
                composite_key.clone()
            };

            hits.push(SearchHit {
                namespace: ns.to_string(),
                key: user_key,
                score,
                value: raw_value,
            });
        }

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.key.cmp(&b.key))
        });
        hits.truncate(limit);

        Ok((hits, terms_matched))
    }

    fn evict(
        &self,
        namespace: &str,
        max_entries: Option<usize>,
        max_age_secs: Option<u64>,
        now_secs: u64,
    ) -> anyhow::Result<Vec<EvictedEntry>> {
        // F-03: canon segments are immutable history — never evict, regardless of
        // who calls. (The live write path also skips them, but guard here too so
        // a direct evict() call can't violate the invariant.)
        if self.segment_class(namespace)? == Some(MutabilityClass::Canon) {
            return Ok(vec![]);
        }

        let prefix_start = format!("{}\x00", namespace);
        let prefix_end = format!("{}\x01", namespace);
        let ns_prefix_len = namespace.len() + 1;

        // Phase 1: read AGE table to get all entries for this namespace.
        let all_entries: Vec<(String, u64)> = {
            let txn = self.db.begin_read().context("beginning read for eviction scan")?;
            let age_tbl = txn.open_table(AGE).context("opening age table for eviction")?;
            let range = age_tbl
                .range(prefix_start.as_str()..prefix_end.as_str())
                .context("scanning age table")?;
            let mut entries = Vec::new();
            for item in range {
                let (k_guard, v_guard) = item.context("reading age item")?;
                entries.push((k_guard.value().to_string(), v_guard.value()));
            }
            entries
        };

        if all_entries.is_empty() {
            return Ok(vec![]);
        }

        // Identify entries to evict; track (composite_key, reason).
        let mut to_evict: Vec<(String, String)> = Vec::new();

        // Age-based eviction.
        if let Some(max_age) = max_age_secs {
            let oldest_allowed = now_secs.saturating_sub(max_age);
            for (composite_key, ts) in &all_entries {
                if *ts < oldest_allowed {
                    to_evict.push((composite_key.clone(), "age".to_string()));
                }
            }
        }

        // Capacity-based eviction (oldest-first after age evictions are removed).
        if let Some(max_cap) = max_entries {
            let age_evicted: std::collections::HashSet<&str> =
                to_evict.iter().map(|(k, _)| k.as_str()).collect();
            let mut remaining: Vec<(&String, u64)> = all_entries
                .iter()
                .filter(|(k, _)| !age_evicted.contains(k.as_str()))
                .map(|(k, ts)| (k, *ts))
                .collect();
            if remaining.len() > max_cap {
                remaining.sort_unstable_by_key(|(_, ts)| *ts);
                let evict_count = remaining.len() - max_cap;
                for (k, _) in remaining.iter().take(evict_count) {
                    to_evict.push(((*k).clone(), "capacity".to_string()));
                }
            }
        }

        if to_evict.is_empty() {
            return Ok(vec![]);
        }

        // Phase 2: atomically delete the evicted entries from all tables.
        let txn = self.db.begin_write().context("beginning write for eviction")?;
        let mut evicted = Vec::new();
        {
            let mut entries_tbl = txn.open_table(ENTRIES).context("opening entries table")?;
            let mut index_tbl = txn.open_table(INDEX).context("opening index table")?;
            let mut age_tbl = txn.open_table(AGE).context("opening age table")?;
            let mut meta_tbl = txn.open_table(META).context("opening meta table")?;
            let mut ns_tbl = txn.open_table(NAMESPACES).context("opening namespaces table")?;
            let doc_count_key = format!("{DOC_COUNT_PREFIX}{namespace}");
            let mut actually_evicted: u64 = 0;

            for (composite_key, reason) in &to_evict {
                let old_value = entries_tbl
                    .get(composite_key.as_str())
                    .context("reading entry for eviction")?
                    .map(|g| g.value().to_string());

                let Some(ref old_v) = old_value else { continue };
                let old_tokens = index::tokenize(old_v);
                Self::deindex_tokens(
                    &mut index_tbl,
                    namespace,
                    composite_key.as_str(),
                    &old_tokens,
                )
                .context("deindexing evicted entry")?;
                entries_tbl
                    .remove(composite_key.as_str())
                    .context("removing evicted entry")?;
                age_tbl
                    .remove(composite_key.as_str())
                    .context("removing age for evicted entry")?;

                let cur = meta_tbl
                    .get(doc_count_key.as_str())
                    .context("reading doc count")?
                    .map(|g| g.value())
                    .unwrap_or(0);
                if cur > 0 {
                    meta_tbl
                        .insert(doc_count_key.as_str(), cur - 1)
                        .context("decrementing doc count after eviction")?;
                }
                actually_evicted += 1;

                let user_key = composite_key
                    .strip_prefix(&prefix_start)
                    .map(str::to_string)
                    .unwrap_or_else(|| composite_key[ns_prefix_len..].to_string());
                evicted.push(EvictedEntry { key: user_key, reason: reason.clone() });
            }

            // Update NAMESPACES counter for the evicted entries.
            if actually_evicted > 0 {
                let ns_cur = ns_tbl
                    .get(namespace)
                    .context("reading namespace count for eviction")?
                    .map(|g| g.value())
                    .unwrap_or(0);
                if ns_cur <= actually_evicted {
                    ns_tbl.remove(namespace).context("removing namespace after eviction")?;
                } else {
                    ns_tbl
                        .insert(namespace, ns_cur - actually_evicted)
                        .context("decrementing namespace count after eviction")?;
                }
            }
        }
        txn.commit().context("committing eviction")?;
        self.debug_assert_counters(namespace);

        Ok(evicted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_store(dir: &TempDir) -> RedbStore {
        let path = dir.path().join("test.redb");
        let (store, quarantined) = RedbStore::open(&path).unwrap();
        assert!(quarantined.is_none(), "fresh open must not quarantine");
        store
    }

    #[test]
    fn round_trip_basic() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("agent:scratch", "hello", "world").unwrap();
        let val = store.get("agent:scratch", "hello").unwrap();
        assert_eq!(val.as_deref(), Some("world"));
    }

    #[test]
    fn write_survives_reopen() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("durable.redb");
        {
            let (store, _) = RedbStore::open(&path).unwrap();
            store.put("agent:scratch", "persist", "yes").unwrap();
        }
        // Drop and reopen.
        let (store2, quarantined) = RedbStore::open(&path).unwrap();
        assert!(quarantined.is_none());
        assert_eq!(
            store2.get("agent:scratch", "persist").unwrap().as_deref(),
            Some("yes")
        );
    }

    #[test]
    fn corrupt_file_quarantines_and_starts_empty() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("corrupt.redb");
        // Write garbage that redb cannot parse as a valid database.
        std::fs::write(&path, b"not a valid redb file").unwrap();
        let (store, quarantined) = RedbStore::open(&path).unwrap();
        assert!(
            quarantined.is_some(),
            "corrupt file must trigger quarantine"
        );
        let corrupt_path = quarantined.unwrap();
        assert!(corrupt_path.exists(), "quarantine file must exist");
        // F-02: quarantine name is timestamped (corrupt.redb.<unix_secs>.corrupt)
        // so a second quarantine can't clobber the first.
        let fname = corrupt_path.file_name().unwrap().to_string_lossy();
        assert!(
            fname.starts_with("corrupt.redb.") && fname.ends_with(".corrupt"),
            "unexpected quarantine name: {fname}"
        );
        // Fresh store should be empty.
        assert_eq!(store.get("agent:scratch", "any").unwrap(), None);
    }

    // F-02: a transient open failure (here: permission denied) on a VALID store
    // must surface as an error WITHOUT quarantining/renaming the file. The old
    // code treated every non-lock error as corruption and renamed the good file.
    #[cfg(unix)]
    #[test]
    fn transient_open_error_is_not_quarantined() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("valid.redb");
        // Create a real, populated, valid store.
        {
            let (store, _) = RedbStore::open(&path).unwrap();
            store.put("agent:scratch", "k", "v").unwrap();
        }
        // Make it unreadable → Database::open fails with a transient I/O error,
        // NOT StorageError::Corrupted.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Skip on root, where 0o000 is bypassed (open would still succeed).
        if std::fs::File::open(&path).is_ok() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            return;
        }

        let result = RedbStore::open(&path);
        assert!(
            result.is_err(),
            "a transient I/O error must surface as Err, not a silent quarantine"
        );
        assert!(path.exists(), "the valid store must be left in place");
        // No .corrupt sibling may have been created.
        let any_corrupt = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains(".corrupt"));
        assert!(!any_corrupt, "must NOT quarantine on a non-corruption error");

        // Restore perms so TempDir cleanup succeeds.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    // F-04: the NAMESPACES counter must equal the actual key count after a mix
    // of puts and deletes — the invariant the audit flagged as unasserted.
    #[test]
    fn namespace_counter_matches_key_count() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        for i in 0..5 {
            store.put("kb:count", &format!("k{i}"), "v").unwrap();
        }
        store.delete("kb:count", "k0").unwrap();
        store.delete("kb:count", "k1").unwrap();
        // Re-put an existing key (must NOT change the count).
        store.put("kb:count", "k2", "v2").unwrap();

        let actual_keys = store.list_keys("kb:count").unwrap().len();
        let counter = store.namespace_count("kb:count").unwrap();
        assert_eq!(
            counter as usize, actual_keys,
            "NAMESPACES counter ({counter}) must equal actual key count ({actual_keys})"
        );
        assert_eq!(actual_keys, 3, "expected 3 surviving keys");
    }

    #[cfg(unix)]
    #[test]
    fn mode_0600_on_create() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("perms.redb");
        let (_, _) = RedbStore::open(&path).unwrap();
        let perms = std::fs::metadata(&path).unwrap().permissions();
        let mode = perms.mode() & 0o777;
        assert_eq!(mode, 0o600, "store file must be mode 0600, got {mode:o}");
    }

    #[test]
    fn delete_returns_true_on_existing_key() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("agent:scratch", "k", "v").unwrap();
        assert!(store.delete("agent:scratch", "k").unwrap());
        assert!(store.get("agent:scratch", "k").unwrap().is_none());
    }

    #[test]
    fn delete_returns_false_on_missing_key() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        assert!(!store.delete("agent:scratch", "nope").unwrap());
    }

    #[test]
    fn iter_returns_only_namespace_keys() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("agent:scratch", "a", "1").unwrap();
        store.put("agent:scratch", "b", "2").unwrap();
        store.put("other:ns", "c", "3").unwrap();
        let mut items = store.iter("agent:scratch").unwrap();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], ("a".to_string(), "1".to_string()));
        assert_eq!(items[1], ("b".to_string(), "2".to_string()));
    }

    #[test]
    fn append_creates_and_concatenates() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.append("agent:scratch", "log", "line1").unwrap();
        store.append("agent:scratch", "log", "line2").unwrap();
        let val = store.get("agent:scratch", "log").unwrap().unwrap();
        assert_eq!(val, "line1\nline2");
    }

    #[test]
    fn meta_version_returns_schema_version() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        assert_eq!(store.meta_version().unwrap(), SCHEMA_VERSION);
    }

    // ── segment_class / set_segment_class / next_log_seq ────────────────────

    #[test]
    fn segment_class_returns_none_for_unset_namespace() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        assert!(store.segment_class("kb:unknown").unwrap().is_none());
    }

    #[test]
    fn segment_class_round_trips_all_three_variants() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.set_segment_class("kb:canon", MutabilityClass::Canon).unwrap();
        store.set_segment_class("kb:log", MutabilityClass::Log).unwrap();
        store.set_segment_class("kb:scratch", MutabilityClass::Scratch).unwrap();
        assert_eq!(store.segment_class("kb:canon").unwrap(), Some(MutabilityClass::Canon));
        assert_eq!(store.segment_class("kb:log").unwrap(), Some(MutabilityClass::Log));
        assert_eq!(store.segment_class("kb:scratch").unwrap(), Some(MutabilityClass::Scratch));
    }

    #[test]
    fn segment_class_survives_reopen() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("seg_class.redb");
        {
            let (store, _) = RedbStore::open(&path).unwrap();
            store.set_segment_class("kb:events", MutabilityClass::Log).unwrap();
        }
        let (store2, _) = RedbStore::open(&path).unwrap();
        assert_eq!(store2.segment_class("kb:events").unwrap(), Some(MutabilityClass::Log));
    }

    #[test]
    fn next_log_seq_starts_at_1_and_increments() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        assert_eq!(store.next_log_seq("kb:events").unwrap(), 1);
        assert_eq!(store.next_log_seq("kb:events").unwrap(), 2);
        assert_eq!(store.next_log_seq("kb:events").unwrap(), 3);
    }

    #[test]
    fn next_log_seq_is_independent_per_namespace() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        assert_eq!(store.next_log_seq("kb:a").unwrap(), 1);
        assert_eq!(store.next_log_seq("kb:b").unwrap(), 1);
        assert_eq!(store.next_log_seq("kb:a").unwrap(), 2);
        assert_eq!(store.next_log_seq("kb:b").unwrap(), 2);
    }

    #[test]
    fn next_scratch_version_starts_at_1_and_increments() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        assert_eq!(store.next_scratch_version("kb:notes", "status").unwrap(), 1);
        assert_eq!(store.next_scratch_version("kb:notes", "status").unwrap(), 2);
        assert_eq!(store.next_scratch_version("kb:notes", "status").unwrap(), 3);
    }

    #[test]
    fn next_scratch_version_is_independent_per_key() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        assert_eq!(store.next_scratch_version("kb:notes", "a").unwrap(), 1);
        assert_eq!(store.next_scratch_version("kb:notes", "b").unwrap(), 1);
        assert_eq!(store.next_scratch_version("kb:notes", "a").unwrap(), 2);
        assert_eq!(store.next_scratch_version("kb:notes", "b").unwrap(), 2);
    }

    #[test]
    fn next_scratch_version_is_independent_per_namespace() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        assert_eq!(store.next_scratch_version("kb:x", "key").unwrap(), 1);
        assert_eq!(store.next_scratch_version("kb:y", "key").unwrap(), 1);
    }

    // ── p5.5: BM25-lite inverted index ──────────────────────────────────────

    #[test]
    fn ranks_relevant_entry_first() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        // key-a has "rust" twice; key-b has it once — key-a must rank higher.
        store.put("kb:docs", "key-a", "rust makes fast software and rust ensures safety").unwrap();
        store.put("kb:docs", "key-b", "rust programming language introduction").unwrap();
        let (hits, terms) = store.search(Some("kb:docs"), "rust", None, 10).unwrap();
        assert_eq!(terms, 1, "one indexable query term");
        assert!(hits.len() >= 2, "both entries must match");
        assert_eq!(hits[0].key, "key-a", "entry with higher TF must rank first");
    }

    #[test]
    fn segment_scoped_search_excludes_other_segments() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("kb:docs", "k1", "quetzalcoatl feathered serpent mythology").unwrap();
        store.put("kb:other", "k2", "quetzalcoatl appears in other segment").unwrap();
        let (hits, _) = store.search(Some("kb:docs"), "quetzalcoatl", None, 10).unwrap();
        assert_eq!(hits.len(), 1, "search must be scoped to the requested segment");
        assert_eq!(hits[0].namespace, "kb:docs");
        assert_eq!(hits[0].key, "k1");
    }

    #[test]
    fn author_filter_returns_only_matching_provenance() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        let entry_a = serde_json::to_string(&serde_json::json!({
            "content": "important strategic discovery",
            "provenance": {"agent_id": "agent1", "turn": 0, "task_fp": ""}
        })).unwrap();
        let entry_b = serde_json::to_string(&serde_json::json!({
            "content": "important operational note",
            "provenance": {"agent_id": "agent2", "turn": 0, "task_fp": ""}
        })).unwrap();
        store.put("kb:shared", "key-a", &entry_a).unwrap();
        store.put("kb:shared", "key-b", &entry_b).unwrap();
        let (hits, _) = store.search(Some("kb:shared"), "important", Some("agent1"), 10).unwrap();
        assert_eq!(hits.len(), 1, "only agent1 entries should be returned");
        assert_eq!(hits[0].key, "key-a");
    }

    #[test]
    fn index_updated_on_write_and_delete() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("kb:docs", "k1", "unique term zygomorphic flowers").unwrap();
        let (hits_before, _) = store.search(Some("kb:docs"), "zygomorphic", None, 10).unwrap();
        assert_eq!(hits_before.len(), 1, "entry must be searchable after put");
        store.delete("kb:docs", "k1").unwrap();
        let (hits_after, _) = store.search(Some("kb:docs"), "zygomorphic", None, 10).unwrap();
        assert_eq!(hits_after.len(), 0, "entry must not appear after delete");
    }

    #[test]
    fn append_reindexes_new_content() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.append("kb:docs", "k1", "initial content here").unwrap();
        // bananaphone is not in initial content
        let (before, _) = store.search(Some("kb:docs"), "bananaphone", None, 10).unwrap();
        assert_eq!(before.len(), 0);
        store.append("kb:docs", "k1", "now mentioning bananaphone device").unwrap();
        let (after, _) = store.search(Some("kb:docs"), "bananaphone", None, 10).unwrap();
        assert_eq!(after.len(), 1, "appended token must be searchable");
        assert_eq!(after[0].key, "k1");
    }

    #[test]
    fn delete_prunes_posting_list_and_decrements_doc_count() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("kb:docs", "k1", "quetzalcoatl mythology mexico").unwrap();
        store.put("kb:docs", "k2", "another quetzalcoatl reference aztec").unwrap();
        let (before, _) = store.search(Some("kb:docs"), "quetzalcoatl", None, 10).unwrap();
        assert_eq!(before.len(), 2, "both entries should be present before delete");
        store.delete("kb:docs", "k1").unwrap();
        let (after, _) = store.search(Some("kb:docs"), "quetzalcoatl", None, 10).unwrap();
        assert_eq!(after.len(), 1, "only k2 should remain after deleting k1");
        assert_eq!(after[0].key, "k2");
    }

    #[test]
    fn search_all_stopwords_returns_zero_terms_matched() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("kb:docs", "k1", "some content in here").unwrap();
        let (hits, terms_matched) =
            store.search(Some("kb:docs"), "the a an is in", None, 10).unwrap();
        assert_eq!(terms_matched, 0, "all-stopword query must produce 0 terms_matched");
        assert!(hits.is_empty(), "no hits when query terms are all stopwords");
    }

    #[test]
    fn put_overwrite_deindexes_old_tokens() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        // First put — "xylophone" is indexed.
        store.put("kb:docs", "k1", "xylophone orchestral instrument").unwrap();
        let (before, _) = store.search(Some("kb:docs"), "xylophone", None, 10).unwrap();
        assert_eq!(before.len(), 1, "xylophone must be searchable after first put");
        // Second put with different content — old tokens must be deindexed.
        store.put("kb:docs", "k1", "completely unrelated content bassoon").unwrap();
        let (after_old, _) = store.search(Some("kb:docs"), "xylophone", None, 10).unwrap();
        assert_eq!(after_old.len(), 0, "xylophone must not appear after overwrite");
        let (after_new, _) = store.search(Some("kb:docs"), "bassoon", None, 10).unwrap();
        assert_eq!(after_new.len(), 1, "new token bassoon must be searchable");
        assert_eq!(after_new[0].key, "k1");
    }

    #[test]
    fn search_cross_segment_none_returns_error() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        let result = store.search(None, "anything", None, 10);
        assert!(result.is_err(), "search(None, ...) must return an error");
        assert!(
            result.unwrap_err().to_string().contains("cross-segment"),
            "error must mention cross-segment restriction"
        );
    }

    #[test]
    fn search_author_filter_no_provenance_field_includes_entry() {
        // When the stored value is NOT a JSON object with a provenance field,
        // author filtering must include the entry (unwrap_or(true) path).
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        // Plain string value — no provenance JSON.
        store.put("kb:docs", "plain", "quokka marsupial australia").unwrap();
        let (hits, _) = store.search(Some("kb:docs"), "quokka", Some("any-agent"), 10).unwrap();
        assert_eq!(hits.len(), 1, "entry without provenance must be included under author filter");
        assert_eq!(hits[0].key, "plain");
    }

    // F-03/p5.9: 2-boot continuity at the store level (the QEMU version that
    // additionally exercises the 9p mount needs a live model + qemu host). Boot 1
    // seeds canon + writes an agent finding; after "reboot" (drop + reopen the
    // same path) the finding survives and canon re-seeds idempotently.
    #[test]
    fn two_boot_continuity_at_store_level() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("memory.redb");

        // ── Boot 1 ──
        {
            let (store, q) = RedbStore::open(&path).unwrap();
            assert!(q.is_none());
            store.set_segment_class("kb:meta", MutabilityClass::Canon).unwrap();
            store.put("kb:meta", "guidelines", "cite evidence").unwrap(); // operator seed
            store.put("kb:research", "finding", "rust is memory-safe").unwrap(); // agent write
        }

        // ── Boot 2 (reopen same path) ──
        let (store2, q2) = RedbStore::open(&path).unwrap();
        assert!(q2.is_none(), "a clean store must not quarantine on reboot");
        assert_eq!(
            store2.get("kb:research", "finding").unwrap().as_deref(),
            Some("rust is memory-safe"),
            "agent finding must survive a reboot"
        );
        assert_eq!(
            store2.get("kb:meta", "guidelines").unwrap().as_deref(),
            Some("cite evidence"),
            "canon seed must persist across reboot"
        );
        assert_eq!(
            store2.segment_class("kb:meta").unwrap(),
            Some(MutabilityClass::Canon),
            "segment class must persist across reboot"
        );
    }

    // ── p5.6: eviction ──────────────────────────────────────────────────────

    // F-03: eviction actually runs on the live write path once limits are set —
    // not just when a test calls evict() directly (the bug: evict() was dead code).
    #[test]
    fn eviction_runs_through_live_path() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.set_segment_class("kb:rolling", MutabilityClass::Log).unwrap();
        store.set_segment_limits("kb:rolling", Some(3), None).unwrap();

        // Write 6 entries; each put self-trims to the 3-entry floor.
        for i in 0..6 {
            store.put("kb:rolling", &format!("k{i}"), "v").unwrap();
        }
        let keys = store.list_keys("kb:rolling").unwrap();
        assert_eq!(keys.len(), 3, "live write path must trim to max_entries=3");
        // The 3 OLDEST must be gone; the 3 newest survive.
        assert_eq!(store.namespace_count("kb:rolling").unwrap(), 3);
        assert!(store.get("kb:rolling", "k5").unwrap().is_some());
        assert!(store.get("kb:rolling", "k0").unwrap().is_none());
    }

    // F-03: canon segments are immutable — eviction must never remove from them,
    // even when limits are configured and exceeded.
    #[test]
    fn canon_is_not_evicted() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.set_segment_class("kb:law", MutabilityClass::Canon).unwrap();
        store.set_segment_limits("kb:law", Some(1), Some(1)).unwrap();

        for i in 0..5 {
            store.put("kb:law", &format!("k{i}"), "v").unwrap();
        }
        // Despite max_entries=1, all 5 survive (canon protected on live path).
        assert_eq!(store.list_keys("kb:law").unwrap().len(), 5);
        // A direct evict() call must also refuse to touch canon.
        let evicted = store.evict("kb:law", Some(1), Some(1), 9_999_999_999).unwrap();
        assert!(evicted.is_empty(), "direct evict() must skip canon segments");
        assert_eq!(store.list_keys("kb:law").unwrap().len(), 5);
    }

    #[test]
    fn evict_empty_namespace_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        let evicted = store.evict("kb:empty", Some(10), Some(3600), 1_000_000).unwrap();
        assert!(evicted.is_empty(), "nothing to evict in empty namespace");
    }

    #[test]
    fn evicts_oldest_beyond_capacity() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        // Write 3 entries with distinct timestamps using put_at.
        store.put_at("kb:test", "old", "oldest entry content", 1000).unwrap();
        store.put_at("kb:test", "mid", "middle entry content", 2000).unwrap();
        store.put_at("kb:test", "new", "newest entry content", 3000).unwrap();
        // Capacity = 2 → oldest must be evicted.
        let evicted = store.evict("kb:test", Some(2), None, 4000).unwrap();
        assert_eq!(evicted.len(), 1, "one entry must be evicted");
        assert_eq!(evicted[0].key, "old");
        assert_eq!(evicted[0].reason, "capacity");
        // Remaining: mid, new.
        assert!(store.get("kb:test", "old").unwrap().is_none(), "old must be gone");
        assert!(store.get("kb:test", "mid").unwrap().is_some(), "mid must remain");
        assert!(store.get("kb:test", "new").unwrap().is_some(), "new must remain");
    }

    #[test]
    fn evicts_entries_past_max_age() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put_at("kb:test", "stale", "stale content here", 100).unwrap();
        store.put_at("kb:test", "fresh", "fresh content here", 5000).unwrap();
        // max_age = 3600s; now = 6000 → cutoff = 2400 → stale (ts=100) must be evicted.
        let evicted = store.evict("kb:test", None, Some(3600), 6000).unwrap();
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].key, "stale");
        assert_eq!(evicted[0].reason, "age");
        assert!(store.get("kb:test", "stale").unwrap().is_none());
        assert!(store.get("kb:test", "fresh").unwrap().is_some());
    }

    #[test]
    fn eviction_removes_index_postings() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put_at("kb:test", "evict-me", "catamaran sailing unique", 100).unwrap();
        store.put_at("kb:test", "keep-me", "kayak paddling unique", 5000).unwrap();
        // Verify both are searchable before eviction.
        let (before, _) = store.search(Some("kb:test"), "unique", None, 10).unwrap();
        assert_eq!(before.len(), 2);
        // Evict by age: cutoff = 4000 → evict-me (ts=100) gone, keep-me (ts=5000) stays.
        store.evict("kb:test", None, Some(3600), 6000).unwrap();
        // "catamaran" must no longer appear.
        let (cat, _) = store.search(Some("kb:test"), "catamaran", None, 10).unwrap();
        assert_eq!(cat.len(), 0, "catamaran posting must be removed after eviction");
        // "unique" from keep-me must still appear.
        let (uniq, _) = store.search(Some("kb:test"), "unique", None, 10).unwrap();
        assert_eq!(uniq.len(), 1);
        assert_eq!(uniq[0].key, "keep-me");
    }

    #[test]
    fn evict_below_capacity_does_nothing() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put_at("kb:test", "a", "alpha content", 1000).unwrap();
        store.put_at("kb:test", "b", "beta content", 2000).unwrap();
        // max_entries = 5; only 2 present → nothing evicted.
        let evicted = store.evict("kb:test", Some(5), None, 3000).unwrap();
        assert!(evicted.is_empty(), "nothing should be evicted when under capacity");
    }

    // ── list_namespaces ───────────────────────────────────────────────────────

    // ── p5.8: NAMESPACES table ───────────────────────────────────────────────

    #[test]
    fn namespaces_backfill_from_entries() {
        // Simulate a pre-p5.8 store: ENTRIES + META populated, no NAMESPACES table.
        // On open, init_schema creates NAMESPACES (empty) and backfill should run.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("backfill.redb");
        {
            let db = redb::Database::create(&path).unwrap();
            let txn = db.begin_write().unwrap();
            {
                let mut e = txn.open_table(ENTRIES).unwrap();
                e.insert("kb:alpha\x00key1", "val1").unwrap();
                e.insert("kb:alpha\x00key2", "val2").unwrap();
                e.insert("kb:beta\x00key1", "val3").unwrap();
                let mut m = txn.open_table(META).unwrap();
                m.insert("format_version", SCHEMA_VERSION).unwrap();
                // Deliberately omit NAMESPACES table — pre-p5.8 schema.
            }
            txn.commit().unwrap();
        }
        let (store, q) = RedbStore::open(&path).unwrap();
        assert!(q.is_none());
        let mut ns = store.list_namespaces().unwrap();
        ns.sort();
        assert_eq!(ns, vec!["kb:alpha", "kb:beta"],
            "backfill must populate NAMESPACES from ENTRIES on first open of pre-p5.8 store");
    }

    #[test]
    fn namespaces_counter_put_and_delete() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("kb:counter", "a", "alpha").unwrap();
        store.put("kb:counter", "b", "beta").unwrap();
        store.put("kb:counter", "c", "gamma").unwrap();
        // Update existing key — should NOT increment counter.
        store.put("kb:counter", "a", "updated-alpha").unwrap();
        let ns = store.list_namespaces().unwrap();
        assert_eq!(ns, vec!["kb:counter"], "one namespace should be present");
        // Delete 2 of 3 entries.
        store.delete("kb:counter", "a").unwrap();
        store.delete("kb:counter", "b").unwrap();
        // Namespace still present with 1 remaining entry.
        let ns2 = store.list_namespaces().unwrap();
        assert_eq!(ns2, vec!["kb:counter"], "namespace must still appear after partial delete");
        // Delete the last entry.
        store.delete("kb:counter", "c").unwrap();
        let ns3 = store.list_namespaces().unwrap();
        assert!(ns3.is_empty(), "namespace must disappear after last entry is deleted");
    }

    #[test]
    fn list_namespaces_uses_namespaces_table() {
        // Verify: list_namespaces reflects the NAMESPACES table — deletions
        // should remove a namespace once all its keys are gone.
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("seg:a", "k1", "v1").unwrap();
        store.put("seg:b", "k1", "v1").unwrap();
        let mut ns = store.list_namespaces().unwrap();
        ns.sort();
        assert_eq!(ns, vec!["seg:a", "seg:b"]);
        store.delete("seg:a", "k1").unwrap();
        let ns2 = store.list_namespaces().unwrap();
        assert_eq!(ns2, vec!["seg:b"], "seg:a must vanish after its only key is deleted");
    }

    #[test]
    fn delete_removes_namespace_key_at_zero() {
        // NAMESPACES must use remove() not insert(0) when count reaches zero —
        // no ghost entries should remain after the last key is deleted.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ghost.redb");
        let (store, _) = RedbStore::open(&path).unwrap();
        store.put("ephemeral", "only-key", "only-value").unwrap();
        store.delete("ephemeral", "only-key").unwrap();
        drop(store);
        // Reopen and verify NAMESPACES has no entry for "ephemeral".
        let (store2, _) = RedbStore::open(&path).unwrap();
        let ns = store2.list_namespaces().unwrap();
        assert!(!ns.contains(&"ephemeral".to_string()),
            "NAMESPACES must not contain ghost entry after last key deleted");
    }

    #[test]
    fn list_namespaces_empty_store_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        let ns = store.list_namespaces().unwrap();
        assert!(ns.is_empty(), "empty store must return no namespaces");
    }

    #[test]
    fn list_namespaces_deduplicates_and_sorts() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("agent/alice", "k1", "v1").unwrap();
        store.put("agent/alice", "k2", "v2").unwrap();  // same namespace, second key
        store.put("agent/bob", "k1", "v1").unwrap();
        store.put("canon", "doc-1", "v1").unwrap();
        let mut ns = store.list_namespaces().unwrap();
        ns.sort(); // redb iterates in btree order (lexicographic), but sort explicitly for clarity
        assert_eq!(ns, vec!["agent/alice", "agent/bob", "canon"],
            "namespaces must be deduplicated and alphabetically sorted");
    }

    #[test]
    fn list_namespaces_single_namespace_multiple_keys() {
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        for i in 0..10 {
            store.put("scratch", &format!("key-{}", i), "val").unwrap();
        }
        let ns = store.list_namespaces().unwrap();
        assert_eq!(ns, vec!["scratch"],
            "multiple keys in one namespace must yield exactly one namespace entry");
    }

    // ── p5.8: NAMESPACES counter via append ───────────────────────────────────

    #[test]
    fn namespaces_counter_append() {
        // Verify that append() increments the NAMESPACES counter for new keys.
        // Log segments always create new keys (unique seq-based keys), so each
        // append should increment the counter.
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.append("kb:log", "ts:1", "first entry").unwrap();
        store.append("kb:log", "ts:2", "second entry").unwrap();
        // NAMESPACES must contain "kb:log" with count 2
        let ns = store.list_namespaces().unwrap();
        assert_eq!(ns, vec!["kb:log"], "append must create namespace entry");
        let keys = store.list_keys("kb:log").unwrap();
        assert_eq!(keys.len(), 2, "two appended entries must exist");
        // Overwrite an existing key via append — same key means is_new=false,
        // NAMESPACES counter must NOT double-count.
        store.append("kb:log", "ts:1", "updated first entry").unwrap();
        let ns2 = store.list_namespaces().unwrap();
        assert_eq!(ns2, vec!["kb:log"], "namespace must still appear after overwrite append");
        let keys2 = store.list_keys("kb:log").unwrap();
        assert_eq!(keys2.len(), 2, "overwrite append must not add a new key");
    }

    // ── p5.8: NAMESPACES counter after eviction ───────────────────────────────

    #[test]
    fn namespaces_counter_evict_to_zero() {
        // Evicting all entries from a namespace must remove it from NAMESPACES.
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("kb:evict-ns", "a", "val1").unwrap();
        store.put("kb:evict-ns", "b", "val2").unwrap();
        assert!(store.list_namespaces().unwrap().contains(&"kb:evict-ns".to_string()),
            "precondition: namespace must appear before eviction");
        // Evict all entries: max_entries = 0 removes everything.
        let evicted = store.evict("kb:evict-ns", Some(0), None, 0).unwrap();
        assert_eq!(evicted.len(), 2, "both entries must be evicted");
        assert!(!store.list_namespaces().unwrap().contains(&"kb:evict-ns".to_string()),
            "namespace must disappear after all entries are evicted");
    }

    #[test]
    fn namespaces_counter_partial_evict_retains_namespace() {
        // Evicting a subset of entries must keep the namespace in NAMESPACES.
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("kb:partial", "a", "val1").unwrap();
        store.put("kb:partial", "b", "val2").unwrap();
        store.put("kb:partial", "c", "val3").unwrap();
        // Evict 1 of 3 (max_entries = 2 keeps 2, removes 1).
        let evicted = store.evict("kb:partial", Some(2), None, 0).unwrap();
        assert_eq!(evicted.len(), 1, "one entry must be evicted");
        assert!(store.list_namespaces().unwrap().contains(&"kb:partial".to_string()),
            "namespace must remain after partial eviction");
    }

    // ── set_segment_class registers namespace in NAMESPACES ───────────────────

    #[test]
    fn set_segment_class_registers_namespace_before_write() {
        // cos-polish #3: configured segments (ops:briefs, ops:entities) must appear
        // in list_namespaces() — and therefore in FUSE /agents/kb/ — immediately
        // at startup, before any entries are written.
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        // No data written yet — namespace must not appear.
        assert!(store.list_namespaces().unwrap().is_empty(),
            "precondition: no namespaces before set_segment_class");
        // set_segment_class with a colon-containing name (matches real CoS config).
        store.set_segment_class("ops:briefs", MutabilityClass::Log).unwrap();
        store.set_segment_class("ops:entities", MutabilityClass::Scratch).unwrap();
        let mut ns = store.list_namespaces().unwrap();
        ns.sort();
        assert_eq!(ns, vec!["ops:briefs", "ops:entities"],
            "configured segments must appear in list_namespaces before any data is written");
    }

    #[test]
    fn set_segment_class_does_not_reset_existing_namespace_count() {
        // set_segment_class called on a namespace that already has entries must not
        // overwrite the NAMESPACES counter with 0 (idempotent registration guard).
        let dir = TempDir::new().unwrap();
        let store = open_store(&dir);
        store.put("ops:briefs", "k1", "v1").unwrap();
        store.put("ops:briefs", "k2", "v2").unwrap();
        // Two entries — count = 2.
        assert_eq!(store.namespace_count("ops:briefs").unwrap(), 2,
            "precondition: two entries must be counted");
        // Calling set_segment_class again must not reset count to 0.
        store.set_segment_class("ops:briefs", MutabilityClass::Log).unwrap();
        assert_eq!(store.namespace_count("ops:briefs").unwrap(), 2,
            "set_segment_class must not overwrite existing namespace count");
        // list_namespaces must still return the namespace.
        let ns = store.list_namespaces().unwrap();
        assert!(ns.contains(&"ops:briefs".to_string()),
            "namespace must still appear after set_segment_class on existing namespace");
    }
}
