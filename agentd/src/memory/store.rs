use std::path::{Path, PathBuf};

use anyhow::Context;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::memory::{MemoryStore, SCHEMA_VERSION};

/// Composite entry key: `"{namespace}\x00{key}"`.
/// The `\x00` separator is safe because our namespace/key grammar disallows
/// null bytes — so splitting on the first `\x00` is unambiguous.
const ENTRIES: TableDefinition<&str, &str> = TableDefinition::new("entries");
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

fn entry_key(namespace: &str, key: &str) -> String {
    format!("{}\x00{}", namespace, key)
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
            // Open both tables to ensure they exist.
            let _entries = txn.open_table(ENTRIES).context("opening entries table")?;
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
        let k = entry_key(namespace, key);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        {
            let mut table = txn.open_table(ENTRIES).context("opening entries table")?;
            table
                .insert(k.as_str(), value)
                .context("inserting entry")?;
        }
        txn.commit().context("committing put")?;
        Ok(())
    }

    fn append(&self, namespace: &str, key: &str, value: &str) -> anyhow::Result<()> {
        let k = entry_key(namespace, key);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        {
            let mut table = txn.open_table(ENTRIES).context("opening entries table")?;
            let current = table
                .get(k.as_str())
                .context("reading for append")?
                .map(|g| g.value().to_string())
                .unwrap_or_default();
            let new_value = if current.is_empty() {
                value.to_string()
            } else {
                format!("{}\n{}", current, value)
            };
            table
                .insert(k.as_str(), new_value.as_str())
                .context("inserting appended entry")?;
        }
        txn.commit().context("committing append")?;
        Ok(())
    }

    fn delete(&self, namespace: &str, key: &str) -> anyhow::Result<bool> {
        let k = entry_key(namespace, key);
        let txn = self.db.begin_write().context("beginning write transaction")?;
        let existed = {
            let mut table = txn.open_table(ENTRIES).context("opening entries table")?;
            let removed = table.remove(k.as_str()).context("deleting entry")?;
            removed.is_some()
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
}
