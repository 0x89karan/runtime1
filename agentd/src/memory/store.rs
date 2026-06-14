use std::path::{Path, PathBuf};

use anyhow::Context;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::memory::{index, MemoryStore, MutabilityClass, SearchHit, SCHEMA_VERSION};

/// Composite entry key: `"{namespace}\x00{key}"`.
/// The `\x00` separator is safe because our namespace/key grammar disallows
/// null bytes — so splitting on the first `\x00` is unambiguous.
const ENTRIES: TableDefinition<&str, &str> = TableDefinition::new("entries");
/// Inverted index: key = `"{namespace}\x00{word}"`, value = JSON array of entry keys.
const INDEX: TableDefinition<&str, &str> = TableDefinition::new("index");
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

const SEG_CLASS_PREFIX: &str = "seg_class:";
const LOG_SEQ_PREFIX: &str = "log_seq:";
const SCRATCH_VER_PREFIX: &str = "scratch_ver:";
const DOC_COUNT_PREFIX: &str = "doc_count:";

fn entry_key(namespace: &str, key: &str) -> String {
    format!("{}\x00{}", namespace, key)
}

fn index_key(namespace: &str, word: &str) -> String {
    format!("{}\x00{}", namespace, word)
}

pub struct RedbStore {
    db: Database,
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
            Err(e) => {
                let msg = format!("{e:?}");
                // DatabaseAlreadyOpen is a lock error — do NOT quarantine.
                if msg.contains("AlreadyOpen")
                    || msg.to_lowercase().contains("already open")
                    || msg.to_lowercase().contains("already locked")
                {
                    return Err(e.context(
                        "memory.redb is held by another process; stop the other \
                         agentd instance or set a unique memory.store_path",
                    ));
                }
                // All other errors are treated as potential corruption.
                // Quarantine the file and open a fresh store.
                let corrupt_path = Self::quarantine_path(path);
                std::fs::rename(path, &corrupt_path).with_context(|| {
                    format!(
                        "quarantining corrupt store: {path:?} → {corrupt_path:?}"
                    )
                })?;
                let store = Self::try_open(path)
                    .context("opening fresh store after quarantine")?;
                Ok((store, Some(corrupt_path)))
            }
        }
    }

    fn quarantine_path(path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .map(|n| format!("{}.corrupt", n.to_string_lossy()))
            .unwrap_or_else(|| "memory.redb.corrupt".to_string());
        path.parent()
            .unwrap_or(Path::new("."))
            .join(name)
    }

    fn try_open(path: &Path) -> anyhow::Result<Self> {
        // redb 4.x splits open (existing) and create (new) — implement open-or-create.
        let db = if path.exists() {
            Database::open(path)
        } else {
            Database::create(path)
        }
        .with_context(|| format!("opening memory store at {path:?}"))?;

        // Set mode 0600 immediately after open so the file is not world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .context("reading store file metadata")?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms)
                .context("setting store permissions to 0600")?;
        }

        let store = Self { db };
        store.init_schema().context("initialising store schema")?;
        Ok(store)
    }

    fn init_schema(&self) -> anyhow::Result<()> {
        let txn = self.db.begin_write().context("beginning schema init transaction")?;
        {
            // Open all tables to ensure they exist before any read.
            let _entries = txn.open_table(ENTRIES).context("opening entries table")?;
            let _index = txn.open_table(INDEX).context("opening index table")?;
            let mut meta = txn.open_table(META).context("opening meta table")?;
            if meta
                .get("format_version")
                .context("reading format_version")?
                .is_none()
            {
                meta.insert("format_version", SCHEMA_VERSION)
                    .context("writing format_version")?;
            }
        }
        txn.commit().context("committing schema init")?;
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
        let ek = entry_key(namespace, key);
        let new_tokens = index::tokenize(value);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        {
            let mut entries_tbl = txn.open_table(ENTRIES).context("opening entries table")?;
            let mut index_tbl = txn.open_table(INDEX).context("opening index table")?;
            let mut meta_tbl = txn.open_table(META).context("opening meta table")?;

            let old_value = entries_tbl
                .get(ek.as_str())
                .context("reading old entry for put")?
                .map(|g| g.value().to_string());

            let is_new = old_value.is_none();
            if let Some(ref old_v) = old_value {
                let old_tokens = index::tokenize(old_v);
                Self::deindex_tokens(&mut index_tbl, namespace, ek.as_str(), &old_tokens)
                    .context("deindexing old tokens on put")?;
            }

            entries_tbl.insert(ek.as_str(), value).context("inserting entry")?;
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
            }
        }
        txn.commit().context("committing put")?;
        Ok(())
    }

    fn append(&self, namespace: &str, key: &str, value: &str) -> anyhow::Result<()> {
        let ek = entry_key(namespace, key);
        let new_tokens = index::tokenize(value);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        {
            let mut entries_tbl = txn.open_table(ENTRIES).context("opening entries table")?;
            let mut index_tbl = txn.open_table(INDEX).context("opening index table")?;
            let mut meta_tbl = txn.open_table(META).context("opening meta table")?;

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
            }
        }
        txn.commit().context("committing append")?;
        Ok(())
    }

    fn delete(&self, namespace: &str, key: &str) -> anyhow::Result<bool> {
        let ek = entry_key(namespace, key);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        let existed = {
            let mut entries_tbl = txn.open_table(ENTRIES).context("opening entries table")?;
            let mut index_tbl = txn.open_table(INDEX).context("opening index table")?;
            let mut meta_tbl = txn.open_table(META).context("opening meta table")?;

            let old_value = entries_tbl
                .get(ek.as_str())
                .context("reading for delete")?
                .map(|g| g.value().to_string());

            if let Some(ref old_v) = old_value {
                let old_tokens = index::tokenize(old_v);
                Self::deindex_tokens(&mut index_tbl, namespace, ek.as_str(), &old_tokens)
                    .context("deindexing deleted entry")?;
                entries_tbl.remove(ek.as_str()).context("deleting entry")?;
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
                true
            } else {
                false
            }
        };
        txn.commit().context("committing delete")?;
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
        }
        txn.commit().context("committing segment class")?;
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
        assert!(corrupt_path.ends_with("corrupt.redb.corrupt"));
        // Fresh store should be empty.
        assert_eq!(store.get("agent:scratch", "any").unwrap(), None);
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
}
