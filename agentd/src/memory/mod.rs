pub mod store;

/// Schema version written to the `meta` table on first create.
/// Distinct from redb's own file-format version (redb owns that; we own this).
pub const SCHEMA_VERSION: u64 = 1;

/// Mutability class of a memory entry (for future tier enforcement).
/// Unused in p5.1 but stored with entries so p5.2+ can enforce invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutabilityClass {
    Canon,
    Log,
    Scratch,
}

/// Thin abstraction over the durable key/value backend.
///
/// `Arc<dyn MemoryStore>` is the currency passed to `KvGet`/`KvSet` tools.
/// The only implementation in p5.1 is `RedbStore`; p5.2+ may add additional
/// tiers that implement the same interface.
///
/// All methods are synchronous. Callers in async contexts must use
/// `tokio::task::spawn_blocking` for write operations.
pub trait MemoryStore: Send + Sync {
    /// Read a value. Returns `None` when the key is absent.
    fn get(&self, namespace: &str, key: &str) -> anyhow::Result<Option<String>>;

    /// Upsert a value.
    fn put(&self, namespace: &str, key: &str, value: &str) -> anyhow::Result<()>;

    /// Append `value` to an existing entry (newline-separated). Creates if absent.
    fn append(&self, namespace: &str, key: &str, value: &str) -> anyhow::Result<()>;

    /// Delete a key. Returns `true` if it existed.
    fn delete(&self, namespace: &str, key: &str) -> anyhow::Result<bool>;

    /// Return all `(key, value)` pairs in `namespace`.
    fn iter(&self, namespace: &str) -> anyhow::Result<Vec<(String, String)>>;

    /// Return the schema version written on first create.
    fn meta_version(&self) -> anyhow::Result<u64>;
}

/// Validate that a namespace or key string conforms to the allowed grammar.
///
/// Allowed: `[a-zA-Z0-9_\-:./]`, max 1024 bytes, non-empty.
/// This is called before the capability check so format errors get a
/// human-readable message rather than a cryptic capability-denied error.
pub fn validate_segment(s: &str, label: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!s.is_empty(), "{label} must not be empty");
    anyhow::ensure!(
        s.len() <= 1024,
        "{label} exceeds 1024 bytes (got {})",
        s.len()
    );
    anyhow::ensure!(
        s.bytes().all(|b| matches!(b,
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' |
            b'_' | b'-' | b':' | b'.' | b'/'
        )),
        "{label} contains invalid characters; \
         allowed: [a-zA-Z0-9_\\-:./], got: {s:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_segment_allows_valid_strings() {
        assert!(validate_segment("agent:scratch", "ns").is_ok());
        assert!(validate_segment("my-key_123", "key").is_ok());
        assert!(validate_segment("foo/bar.baz", "key").is_ok());
    }

    #[test]
    fn validate_segment_rejects_empty() {
        assert!(validate_segment("", "ns").is_err());
    }

    #[test]
    fn validate_segment_rejects_null_bytes() {
        assert!(validate_segment("foo\x00bar", "key").is_err());
    }

    #[test]
    fn validate_segment_rejects_spaces() {
        assert!(validate_segment("foo bar", "key").is_err());
    }

    #[test]
    fn validate_segment_rejects_too_long() {
        let long = "a".repeat(1025);
        assert!(validate_segment(&long, "key").is_err());
    }
}
