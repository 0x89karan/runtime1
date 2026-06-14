pub mod context;
pub mod index;
pub mod store;

pub use context::MemItem;

/// Schema version written to the `meta` table on first create.
/// Distinct from redb's own file-format version (redb owns that; we own this).
pub const SCHEMA_VERSION: u64 = 1;

/// Mutability class for shared KB segments (p5.4).
/// Enforced by the `kb_put` / `kb_get` Tier-4 tools.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MutabilityClass {
    Canon,
    Log,
    Scratch,
}

/// An entry evicted from a KB segment by the capacity/age floor.
#[derive(Debug, Clone)]
pub struct EvictedEntry {
    pub key: String,
    /// `"capacity"` or `"age"`.
    pub reason: String,
}

/// A single result from a `kb_search` query.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// The namespace (segment) this entry lives in.
    pub namespace: String,
    /// The entry key within the namespace.
    pub key: String,
    /// BM25-lite relevance score (higher = more relevant).
    pub score: f64,
    /// The raw entry JSON string (same format as `kb_get` returns).
    pub value: String,
}

/// Thin abstraction over the durable key/value backend.
///
/// `Arc<dyn MemoryStore>` is the currency passed to storage-backed tools.
/// The sole implementation is `RedbStore` (backed by redb). All methods are
/// synchronous; callers in async contexts must use `spawn_blocking` for writes.
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

    // ── Shared KB (p5.4) ────────────────────────────────────────────────────

    /// Return the mutability class of `namespace`, or `None` if not configured.
    fn segment_class(&self, namespace: &str) -> anyhow::Result<Option<MutabilityClass>>;

    /// Persist the mutability class for `namespace`.
    /// Called at startup for each `[[memory.segments]]` entry.
    fn set_segment_class(&self, namespace: &str, class: MutabilityClass) -> anyhow::Result<()>;

    /// Atomically increment and return the next monotonic sequence number for
    /// a log segment. Starts at 1 on first call. Used to generate unique,
    /// ordered log entry keys in the form `"{seq:016x}"`.
    fn next_log_seq(&self, namespace: &str) -> anyhow::Result<u64>;

    /// Atomically increment and return the next version number for a scratch
    /// key. Starts at 1 on first call. Prevents two concurrent writers from
    /// both producing the same version number for the same key.
    fn next_scratch_version(&self, namespace: &str, key: &str) -> anyhow::Result<u64>;

    // ── Retrieval (p5.5) ────────────────────────────────────────────────────

    /// Search entries using BM25-lite ranking over the inverted index.
    ///
    /// `namespace`: if `Some`, restrict search to that segment; if `None`,
    /// search is not yet supported across all segments — callers should always
    /// pass `Some` for the MVP.
    ///
    /// Returns up to `limit` hits sorted by descending score. Returns an empty
    /// `Vec` when no terms survive tokenization (all stopwords) or no entries
    /// match — callers must distinguish these via `terms_matched`.
    fn search(
        &self,
        namespace: Option<&str>,
        query: &str,
        author: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<(Vec<SearchHit>, usize)>;
    // returns (hits, terms_matched)

    // ── Eviction (p5.6) ─────────────────────────────────────────────────────

    /// Evict entries from `namespace` that exceed capacity or age limits.
    ///
    /// - `max_entries`: if `Some(n)`, evict oldest entries until `count ≤ n`.
    /// - `max_age_secs`: if `Some(s)`, evict entries written before `now_secs - s`.
    /// - `now_secs`: caller-supplied Unix timestamp in seconds (avoids `SystemTime` in store).
    ///
    /// Returns the list of evicted entries (key + reason). Eviction happens in
    /// one atomic transaction: ENTRIES + INDEX + AGE + META all consistent.
    fn evict(
        &self,
        namespace: &str,
        max_entries: Option<usize>,
        max_age_secs: Option<u64>,
        now_secs: u64,
    ) -> anyhow::Result<Vec<EvictedEntry>>;
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
