use std::sync::Arc;
use tempfile::TempDir;

use agentd::capability::Capability;
use agentd::flight_recorder::FlightRecorder;
use agentd::memory::store::RedbStore;
use agentd::tools::{native::register_native, ToolContext, ToolRegistry};

fn ctx(agent_id: &str) -> ToolContext {
    ToolContext { agent_id: agent_id.to_string(), turn: 0, task_fp: String::new() }
}

fn registry_with_store(store: Arc<dyn agentd::memory::MemoryStore>) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    register_native(
        &mut reg,
        &["kv_get".to_string(), "kv_set".to_string()],
        None,
        Some(store),
        None,
        None,
    )
    .unwrap();
    reg
}

fn kb_registry(store: Arc<dyn agentd::memory::MemoryStore>) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    register_native(
        &mut reg,
        &["kb_put".to_string(), "kb_get".to_string(), "kb_search".to_string()],
        None,
        Some(store),
        None,
        None,
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
            &ctx("test-agent"),
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
            &ctx("test-agent"),
            Some(&caps),
            &rec,
        )
        .await
        .unwrap();
    assert_eq!(get_result, "hello", "kv_get must return the stored value");
}

/// kb_search: write 3 documents via kb_put, then search returns ordered hits with provenance.
///
/// This is the acceptance-criteria demo from the p5.5 plan (AC10 / T6):
/// write three entries with distinct relevance signals, search for a specific term,
/// verify hits are returned and contain the expected provenance fields.
#[tokio::test]
async fn kb_search_after_multi_write_returns_ordered_hits_with_provenance() {
    let dir = TempDir::new().unwrap();
    let (store, _) = RedbStore::open(&dir.path().join("mem.redb")).unwrap();
    let store: Arc<dyn agentd::memory::MemoryStore> = Arc::new(store);
    let reg = kb_registry(Arc::clone(&store));
    let rec = recorder(&dir);

    let kb_write_cap = [
        Capability::KbWrite { segment: "kb:notes".to_string() },
        Capability::KbRead { segment: "kb:notes".to_string() },
    ];
    let kb_read_cap = [Capability::KbRead { segment: "kb:notes".to_string() }];

    // Write three documents (scratch class requires explicit key).
    for (content, key) in [
        ("photosynthesis chlorophyll sunlight carbon dioxide glucose", "bio-1"),
        ("photosynthesis plant biology", "bio-2"),
        ("quantum physics entanglement superposition", "physics-1"),
    ] {
        reg.invoke(
            "kb_put",
            serde_json::json!({ "segment": "kb:notes", "key": key, "content": content }),
            &ctx("test-writer"),
            Some(&kb_write_cap),
            &rec,
        )
        .await
        .unwrap();
    }

    // Search for "photosynthesis" — must return 2 hits (not the physics entry).
    let result = reg
        .invoke(
            "kb_search",
            serde_json::json!({ "segment": "kb:notes", "query": "photosynthesis chlorophyll" }),
            &ctx("searcher"),
            Some(&kb_read_cap),
            &rec,
        )
        .await
        .unwrap();

    let v: serde_json::Value = serde_json::from_str(&result).unwrap();
    let hits = v["hits"].as_array().expect("hits must be an array");
    assert!(!hits.is_empty(), "must return at least one hit");
    // The physics entry must not appear.
    for hit in hits {
        let content = hit["content"].as_str().unwrap_or("");
        assert!(
            !content.contains("quantum") && !content.contains("entanglement"),
            "physics entry must not appear in photosynthesis search"
        );
    }
    // The most-relevant hit (bio-1, which has both "photosynthesis" and "chlorophyll") must rank first.
    let first_content = hits[0]["content"].as_str().unwrap_or("");
    assert!(
        first_content.contains("chlorophyll"),
        "most relevant document (with 'chlorophyll') must rank first"
    );
    // Each hit must carry a provenance field.
    assert!(
        hits[0].get("provenance").is_some(),
        "hits must carry provenance field"
    );
    // terms_matched must be > 0.
    let terms_matched = v["terms_matched"].as_u64().unwrap_or(0);
    assert!(terms_matched > 0, "terms_matched must be positive");

    // Verify the kb_search flight event was recorded.
    let flight_content = std::fs::read_to_string(dir.path().join("flight.jsonl")).unwrap();
    let kb_search_event = flight_content
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["kind"] == "kb_search");
    assert!(kb_search_event.is_some(), "kb_search flight event must be emitted");
    let event = kb_search_event.unwrap();
    assert_eq!(event["data"]["segment"], "kb:notes");
    assert!(event["data"]["hits"].as_u64().unwrap_or(0) > 0);
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
            &ctx("test-agent"),
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
