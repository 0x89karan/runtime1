pub mod agents_fs;
pub mod snapshot;

pub use snapshot::{AgentSnapshot, AgentStatus, SchedulerSnapshot};

/// Minimal read-only view of the memory store, used by the FUSE filesystem.
///
/// Defined here (in `surfaces`) so the FUSE handler can hold an
/// `Option<Arc<dyn MemoryAccess>>` without creating a circular dependency:
/// `surfaces` is a leaf crate; `agentd` wraps `RedbStore` in a bridge that
/// implements this trait.
pub trait MemoryAccess: Send + Sync {
    /// Return all distinct namespace names that have at least one entry.
    fn list_namespaces(&self) -> Vec<String>;
    /// Return all keys in `namespace`. Returns empty when namespace is absent.
    fn list_keys(&self, namespace: &str) -> Vec<String>;
    /// Return the raw JSON value for `(namespace, key)`, or `None` if absent.
    fn get_entry(&self, namespace: &str, key: &str) -> Option<String>;
}
