use std::sync::Arc;
use tempfile::TempDir;

use agentd::capability::Capability;
use agentd::flight_recorder::FlightRecorder;
use agentd::memory::store::RedbStore;
use agentd::tools::{native::register_native, ToolRegistry};

fn registry_with_store(store: Arc<dyn agentd::memory::MemoryStore>) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    register_native(
        &mut reg,
        &["kv_get".to_string(), "kv_set".to_string()],
        None,
        Some(store),
    )
    .unwrap();
    reg
}

fn recorder(dir: &TempDir) -> FlightRecorder {
    FlightRecorder::new(&dir.path().join("flight.jsonl")).unwrap()
}

/// kv_set then kv_get happy path: agent with explicit KbWrite+KbRead can round-trip.
#[tokio::test]
async fn kv_roundtrip_with_capabilities() {
    let dir = TempDir::new().unwrap();
    let (store, _) = RedbStore::open(&dir.path().join("mem.redb")).unwrap();
    let store: Arc<dyn agentd::memory::MemoryStore> = Arc::new(store);
    let reg = registry_with_store(Arc::clone(&store));
    let rec = recorder(&dir);

    let caps = [
        Capability::KbWrite { segment: "agent:scratch".to_string() },
        Capability::KbRead { segment: "agent:scratch".to_string() },
    ];

    let set_result = reg
        .invoke(
            "kv_set",
            serde_json::json!({ "namespace": "agent:scratch", "key": "greeting", "value": "hello" }),
            "test-agent",
            Some(&caps),
            &rec,
        )
        .await
        .unwrap();
    assert!(set_result.contains("stored"), "kv_set should confirm storage: {set_result}");

    let get_result = reg
        .invoke(
            "kv_get",
            serde_json::json!({ "namespace": "agent:scratch", "key": "greeting" }),
            "test-agent",
            Some(&caps),
            &rec,
        )
        .await
        .unwrap();
    assert_eq!(get_result, "hello", "kv_get must return the stored value");
}

/// kv_set without a KbWrite capability must be denied.
#[tokio::test]
async fn kv_set_denied_without_capability() {
    let dir = TempDir::new().unwrap();
    let (store, _) = RedbStore::open(&dir.path().join("mem.redb")).unwrap();
    let store: Arc<dyn agentd::memory::MemoryStore> = Arc::new(store);
    let reg = registry_with_store(Arc::clone(&store));
    let rec = recorder(&dir);

    // Only FsRead — no KbWrite.
    let caps = [Capability::FsRead { prefix: "/".to_string() }];
    let err = reg
        .invoke(
            "kv_set",
            serde_json::json!({ "namespace": "agent:scratch", "key": "k", "value": "v" }),
            "test-agent",
            Some(&caps),
            &rec,
        )
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("capability denied"),
        "expected capability denied error, got: {err}"
    );
}
