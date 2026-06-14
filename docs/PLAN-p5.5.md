<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/p5.4-shared-kb-mvp-autoplan-restore-20260614-183100.md -->
# p5.5 — Retrieval as Tool (Lexical Search)

**Branch:** p5.5-kb-search-lexical (created from main after p5.4 lands)
**Depends on:** p5.4 (shared KB MVP, landed PR #32, v0.21.0)
**Target version:** v0.22.0

## Goal

`kb_search { segment?, query, author?, limit? }` returns ranked entries for a query,
scoped to a segment and optionally filtered by author, over a tokenized inverted index.
No embeddings, no network. The agent-facing retrieval API is complete after this.

---

## Architecture (decided)

### Index storage

A new redb table `INDEX` (separate from `ENTRIES`) with:
- Key: `"{namespace}\x00{word}"` (same separator pattern as ENTRIES)
- Value: JSON array of entry keys (strings) that contain this word

Rationale for separate table: prefix keys in ENTRIES would pollute `iter()` results.
Rationale for JSON array: atomic read-modify-write in one redb write transaction;
`serde_json` already a dependency.

### Write-path atomicity (CRITICAL finding resolved)

`RedbStore::put()`, `::append()`, and `::delete()` each open ONE `begin_write()`
transaction and update BOTH `ENTRIES` and `INDEX` before committing. This is internal
to `RedbStore` — the `MemoryStore` trait callers see no difference. No separate
`put_with_index()` method needed.

```
RedbStore::put(namespace, key, value):
  txn = db.begin_write()
  -- ENTRIES: write new value
  entries_table.insert(ns\0key, value)
  -- INDEX: for each token in tokenize(value):
  --   read current posting list
  --   if key not already in list: append key
  --   write updated list back
  -- META: if key was not previously in ENTRIES:
  --   increment doc_count:namespace
  txn.commit()
```

For `delete()`: read the old value first, remove key from each token's posting list
in the same transaction; decrement `doc_count:namespace`.

For `append()`: index only the newly appended portion (not the full accumulated value).

### Document count for IDF

Stored in META table as `doc_count:{namespace}` → `u64`. Updated atomically:
- `put()` where key did not previously exist → increment
- `delete()` where key existed → decrement

Needed to compute IDF in BM25-lite:
```
idf(t, s) = log(1.0 + N(s) / (1.0 + df(t, s)))
            where N(s) = doc_count:namespace from META
                  df(t, s) = posting_list(t, s).len()
score(d, q, s) = Σ_t [ tf(t,d) × idf(t,s) ]
```

### Tokenizer (memory/index.rs)

```
tokenize(text: &str) -> Vec<String>:
  1. text.to_lowercase()               // std only, no unicode crate
  2. split on non-alphanumeric chars
  3. filter empty tokens
  4. filter tokens > 64 bytes          // DoS cap
  5. filter stopwords: ["the","a","an","is","in","of","to","and","or","for","be","with","as","at","by","from","it","its","that","this","was"]
```

### `search()` on the `MemoryStore` trait

Consistent with all other trait methods. `RedbStore` uses the INDEX table.
`SimpleStore` (test mock) implements brute-force linear scan via `iter()`.

Signature:
```rust
fn search(
    &self,
    namespace: Option<&str>,   // None = search all segments agent has KbRead on
    query: &str,
    author: Option<&str>,      // filter by provenance.agent_id
    limit: usize,
) -> anyhow::Result<Vec<SearchHit>>;

pub struct SearchHit {
    pub namespace: String,
    pub key: String,
    pub score: f64,
    pub value: String,   // raw JSON (same as kb_get returns)
}
```

### `init_schema()` extension

Opens `INDEX` table alongside `ENTRIES` so the table exists before any read.

---

## Files changed

| File | Change |
|---|---|
| `agentd/src/memory/index.rs` | NEW — `tokenize()` fn + stopword set |
| `agentd/src/memory/mod.rs` | Add `SearchHit` struct + `search()` to `MemoryStore` trait |
| `agentd/src/memory/store.rs` | `INDEX` table definition; `init_schema` opens it; `put/append/delete` maintain index + doc_count; `RedbStore::search()` BM25-lite; `SimpleStore` brute-force impl |
| `agentd/src/tools/native.rs` | `KbSearch` struct + `impl Tool`; register `kb_search` (opt-in only) |
| `agentd/src/tools/mod.rs` | `SimpleStore::search()` brute-force impl |
| `agentd/src/events.rs` | `EventKind::KbSearch` |
| `docs/CONVENTIONS.md` | Add `kb_search` row to event taxonomy table |
| `agentd/Cargo.toml` | Version bump `0.21.0` → `0.22.0` |
| `CHANGELOG.md` | p5.5 entry |
| `CLAUDE.md` | Status update |
| `docs/ROADMAP.md` | Check off ▢ p5.5 |
| `TODOS.md` | Add p5.5-ar-01 (posting list unbounded in memory) |
| `agentd/agent.toml` | Add `[memory]` + `kb_search` to native tools comment |
| `agentd/agents.toml` | Add `kb_search` to multi-agent KB example comment |

---

## Event

```
kind: kb_search
data: {
  agent_id: String,
  segment: Option<String>,
  query_preview: String,    // first 100 chars of query
  hits: usize,              // count of returned results
  terms_matched: usize,     // count of query terms that survived tokenization (0 = stopwords only)
}
```

## KbSearch tool

```
name: kb_search
NOT included in "all" — must be opt-in (like kb_put / kb_get)

description (for model):
  "Search the shared knowledge base by content. Returns entries ranked by relevance
   to the query. Use kb_get when you have an exact key. Use kb_search when you want
   to find entries by content — it returns up to `limit` results ranked by relevance.
   Requires KbRead capability on the queried segment."

input schema:
  segment:  String (optional) — restrict to this namespace; if absent, must have
            KbRead on all searched segments (scope to one segment for MVP)
  query:    String (required) — free-text search query (tokenized + stopword-filtered)
  author:   String (optional) — filter by provenance.agent_id
  limit:    u64 (optional, default 10, max 100)

required capability: KbRead { segment } (the queried segment)

output: structured JSON object (NOT bare array):
  {
    "hits": [
      {
        "key": "...",
        "namespace": "...",
        "score": 0.95,
        "content": "...",         // expanded from inner JSON
        "tags": [...],            // expanded from inner JSON
        "provenance": { ... }     // expanded from inner JSON
      }
    ],
    "terms_matched": N,           // 0 if query tokenizes to no terms
    "note": "..."                 // present only when terms_matched == 0
  }
  Empty hits array (with terms_matched: 0) when no terms or no results.
```

---

## Architecture ASCII Diagram

```
    ┌─────────────────────────────────────────────────────────┐
    │                    KbSearch tool                        │
    │  kb_search { segment?, query, author?, limit? }         │
    │  Requires KbRead capability on queried segment          │
    └────────────────────────┬────────────────────────────────┘
                             │ store.search(segment?, query, author?, limit)
                             ▼
    ┌─────────────────────────────────────────────────────────┐
    │              MemoryStore::search() [trait]              │
    ├─────────────────────────┬───────────────────────────────┤
    │     RedbStore (prod)    │    SimpleStore (test mock)    │
    │  1. tokenize query      │  brute-force iter(segment)    │
    │  2. guard: terms empty? │  → parse value JSON           │
    │     → return []         │  → count term matches         │
    │  3. for each term t:    │  → sort by score              │
    │     read INDEX[ns\0t]   │                               │
    │     → posting list      │                               │
    │  4. union posting lists │                               │
    │  5. score each doc:     │                               │
    │     tf × idf (BM25lite) │                               │
    │  6. author filter       │                               │
    │  7. sort desc, limit    │                               │
    │  8. fetch full entries  │                               │
    └─────────────────────────┴───────────────────────────────┘

    Write path (maintained atomically by RedbStore):
    ┌─────────────────────────────────────────────────────────┐
    │  RedbStore::put / append / delete                       │
    │                                                         │
    │  txn = db.begin_write()                                 │
    │  ├── ENTRIES table: write/delete value                  │
    │  ├── INDEX table: update posting lists for each token   │
    │  │   (add key on put/append, remove key on delete)      │
    │  └── META table: update doc_count:{namespace}           │
    │  txn.commit()   ← atomic: all or nothing               │
    └─────────────────────────────────────────────────────────┘

    redb tables:
    ENTRIES : "{namespace}\x00{key}"   → JSON entry string
    INDEX   : "{namespace}\x00{word}"  → JSON array of entry keys
    META    : "doc_count:{namespace}"  → u64 (new key)
              "seg_class:{namespace}"  → u64 (existing)
              "log_seq:{namespace}"    → u64 (existing)
              "scratch_ver:{ns}\x00{k}"→ u64 (existing)
```

---

## Test Plan (+10 tests, minimum 6 required)

| # | Test | Location | Codepath |
|---|---|---|---|
| 1 | `ranks_relevant_entry_first` | `memory::store::tests` | BM25 TF×IDF ordering; entry with more query terms scores higher |
| 2 | `segment_scoped_search_excludes_other_segments` | `memory::store::tests` | INDEX key prefix prevents cross-namespace leakage |
| 3 | `author_filter_returns_only_matching_provenance` | `memory::store::tests` | post-retrieval provenance.agent_id filter |
| 4 | `index_updated_on_write_and_delete` | `memory::store::tests` | write → found; delete → not found; posting list pruned |
| 5 | `kb_search_requires_kbread_on_segment` | `tools::native::tests` | capability check; no KbRead → capability_denied event |
| 6 | `integration_search_after_multi_write_returns_ordered_hits` | `tests/memory_integration.rs` | full path: `kb_put` × N, then `kb_search`, verify order + provenance |
| 7 | `all_stopword_query_returns_empty_no_panic` | `memory::index::tests` | tokenizer drops all terms; `terms.is_empty()` → `[]`; no divide-by-zero |
| 8 | `append_reindexes_new_content` | `memory::store::tests` | `append()` path: new tokens become searchable, old tokens unchanged |
| 9 | `delete_prunes_posting_list` | `memory::store::tests` | delete sole holder of a term → posting list removed from INDEX |
| 10 | `token_length_capped_silently` | `memory::index::tests` | 128-byte token → tokenizer skips it; result still correct |

---

## Acceptance Criteria (updated from ROADMAP + CEO + Eng findings)

1. `cargo build` + `cargo clippy -- -D warnings` + `cargo test` clean
2. `make clippy-linux` clean (INDEX table access is Linux-only redb code path)
3. Test count +10 (minimum +6 from roadmap spec)
4. `kb_search` flight event present in integration test; `hits` count matches returned array length
5. Search latency note in PR (brute-force is fine at MVP scale — document O(posting_list_size))
6. CONVENTIONS.md updated (1 new row: `kb_search`)
7. Binary size delta ≤ +40 KB on x86_64-linux-musl
8. **Atomic write invariant (eng finding #1):** Integration test confirms a `kb_put` immediately followed by `kb_search` returns the entry (no eventual-consistency window)
9. **Delete posting cleanup (CEO finding #3 / eng finding #8):** After `kb_delete`, the entry must not appear in search results
10. **Demo usage (CEO finding #1):** `tests/memory_integration.rs` includes a scenario where an agent calls `kb_put` × 3, then `kb_search` returns ordered results with provenance
11. **DX: Tool output is flat** — `KbSearch::invoke` expands `content`/`provenance` from inner JSON into top-level hit fields; no double-parse needed by the agent
12. **DX: Structured empty result** — stopword-only query returns `{hits:[], terms_matched:0, note:...}`, not bare `[]`
13. **DX: Capability error text** — `CapabilityDenied` error follows existing pattern: `"capability KbRead{segment=...} required; agent has: [...]"`

---

## Open Questions (resolved)

| Question | Decision | Rationale |
|---|---|---|
| Index storage: separate TABLE vs prefix keys in ENTRIES | Separate `INDEX` table | Prevents iter() pollution; cleaner schema |
| Posting list format | JSON array as `&str` value | Atomic r-m-w; serde_json already a dep |
| `search()` on trait vs standalone function | Trait method | Consistent with codebase pattern; `SimpleStore` does brute-force |
| `SimpleStore` search implementation | Brute-force linear scan over HashMap | Test accuracy; no index maintenance complexity in mock |
| Tokenizer stopword set | 21 words (minimal English) | Covers common noise without unicode crate |
| BM25 IDF approximation | `log(1 + N/(1+df))` with N from `doc_count` META | Correct IDF; counter updated atomically |
| Linear scan alternative | Rejected: index adds correctness (ranked results); linear scan degrades beyond ~100 entries | BM25-lite is standard for this use case |

---

## NOT in scope

- Vector/semantic search (DESIGN-memory §9 Q1 — deferred to Layer-2 MCP in Beyond phase)
- Index compaction / eviction (p5.6 — posting list pruning deferred; delete path keeps it consistent)
- Index size budget enforcement (p5.6)
- `segment = None` cross-segment search (requires KbRead on all segments — complex; scope to single-segment for MVP)

**Sequencing note:** p5.5 before p5.6 follows the roadmap order. p5.6 eviction will also evict INDEX postings atomically (the delete path in p5.5 establishes that pattern).

---

## TODOS.md additions

- `p5.5-ar-01` (P3): Posting list deserialization is O(posting_list_size) at query time. For high-cardinality segments (>1k entries), a common word's posting list deserialization dominates query latency. Mitigation: cap `limit` at 100 max; document the known O() cost in the PR. Fix: BM25 lazy-evaluation or top-K index structure (deferred to post-p5.6).

---

## GSTACK REVIEW REPORT

### CEO Phase
- **Premises:** Accepted by user.
- **Scope:** SELECTIVE EXPANSION — two in-blast-radius additions approved (demo integration test, explicit atomic-txn acceptance criterion).
- **Deferred:** Linear-scan alternative consideration (noted, not blocking). Sequencing risk (noted, roadmap order preserved).
- **Findings auto-resolved:** 5/5.
- **Taste decisions:** 0.
- **User challenges:** 0.

### Eng Phase [codex-unavailable — single-model]
- **Architecture:** Sound. Separate INDEX table, JSON posting list, `search()` on trait, `init_schema()` opens INDEX.
- **Critical resolved:** Atomic write-path (ENTRIES + INDEX + META in one txn).
- **High resolved:** Token length cap (64B), BM25 doc-count in META, JSON posting format.
- **Medium resolved:** INDEX in init_schema, append re-index, search on trait, delete posting cleanup.
- **Deferred to TODOS:** Posting list memory O(n) (p5.5-ar-01, P3).
- **Test plan:** 10 tests (minimum 6 required). Integration test covers demo scenario.

### DX Phase [codex-unavailable — single-model]
- **Tool description:** Added "use kb_search when you don't know the key" disambiguation.
- **Output format:** Flat hit objects (content/provenance expanded); structured empty result for stopword queries.
- **Event:** Added `terms_matched` field for operator observability.
- **Config examples:** agent.toml + agents.toml updated with `kb_search` in native list.
- **Findings auto-resolved:** 6/6. Taste decisions: 0.

### Taste decisions for final gate
- None yet.
