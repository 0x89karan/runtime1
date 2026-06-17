# Changelog

All notable changes to agentd are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [p6.3] - 2026-06-17 (v0.29.0)

Read-only TUI dashboard. `agentctl watch` opens a live ratatui view of all running agents,
their status, token budgets, and tools. Three views: Dashboard (agent table), Agent Detail
(expanded per-agent), and System (provider, tokens, queue, sandbox status).

### Added
- `agentctl watch [--agents-dir /agents] [--interval N] [--plain] [--no-plain]` — live TUI dashboard.
  - **Dashboard view**: agent table with ID / Status (colour-coded) / Context tokens / Budget / Tool count.
  - **Agent detail view**: expanded view showing status, context, budget, and tool list for the selected agent.
  - **System view**: provider model+backend, global tokens spent, deferred queue depth, sandbox status.
  - **Plain mode**: `--plain` or auto-detected non-TTY stdout emits plain-text snapshots; `--no-plain` forces TUI.
  - **Startup validation**: fails fast with a clear error if `{agents-dir}/system/` is not mounted.
  - **CleanupGuard**: restores terminal on both normal exit and panic via `Drop` + `std::panic::set_hook`.
  - Key bindings: ↑/↓/j/k select, Enter → detail, `s` → system, `q`/Ctrl-C → quit, Esc → back.
- `surfaces/`: FUSE virtual filesystem amendments (Linux-gated):
  - `DIR_STEP` bumped 10→20 to accommodate 9 per-agent virtual files without inode collision.
  - `OFF_TOOLS = 8` — new `tools` virtual file per agent directory listing capability-filtered tool names.
  - `/agents/system/` directory with four virtual files: `budget` (`{spent, total}`), `queue` (`{depth}`),
    `sandbox` (`{applied}`), `provider` (`{model, backend}`).
  - `SchedulerSnapshot` gains `queue_depth`, `provider_model`, `sandbox_applied` fields.
  - `AgentSnapshot` gains `tools: Vec<String>` populated from `AgentTask::spec_names()`.
- `agentd`: `AgentTask::spec_names()` returns capability-filtered tool names from pre-built `specs`.
- `agentd/src/scheduler.rs`: `update_snapshot()` sets `tools` + `queue_depth`; `main.rs` sets
  `provider_model` and tracks `any_sandbox_applied` across MCP server loop.
- CI: agentctl binary size guard bumped 4 MB → 6 MB (ratatui+crossterm add ~1–1.5 MB).

### Changed
- `agentctl` version bumped to `0.29.0`.
- `agentd` version bumped to `0.29.0`.

### Fixed (pre-landing review)
- `io::stdout().is_terminal()` called twice in `run()` → cached as `is_tty` local.
- Cross-crate sentinel literals (`"unlimited"`, `"(none)"`) replaced with named constants in
  both `surfaces/src/agents_fs.rs` and `agentctl/src/watch/reader.rs` with sync comments.
- `run_plain`: flush stdout after each snapshot block so piped readers see complete output;
  SIGINT terminates cleanly via OS default handler (no raw mode is active).
- `AgentTask::spec_names()` now returns `&[String]` from a cached `tool_names` field
  built at construction, eliminating per-tick per-element String allocation in snapshot path.
- `sanitize()` helper added to `views.rs`: strips control chars (< 0x20 except tab) from
  error strings before rendering, preventing ANSI injection via OS error messages.
- `debug_assert!` in `AgentsFs::alloc_dir()` promoted to `assert!` so inode pool exhaustion
  is caught in release builds before silent corruption occurs.

## [p6.2] - 2026-06-17 (v0.28.0)

Operator CLI. Agents can now be spawned from templates without editing TOML files.

### Added
- `agentctl/` workspace crate — new operator CLI binary.
- `agentctl list-templates` — tab-aligned table of templates from repo catalogue and
  `~/.agentos/templates/`, showing name, source (Repo/User), and showcases.
- `agentctl spawn <name> --task "..." [--cap-add ...] [--dry-run]` — resolves a template,
  lowers it to an `agent.toml`, writes it atomically via tempfile rename, then `exec`s agentd.
- `TemplateCard.suggested_caps: Vec<Capability>` — guards `--cap-add` without `--force`;
  uses real `Capability` type (single vocabulary, not alias strings).
- `parse_cap_alias()` — maps flat CLI syntax (`fs-read:<path>`, `net:<ports>`, etc.) to
  `Capability` values; rejects relative paths, bare `net`, and `mcp:...`.
- `cap_add_allowed_by_suggestion()` — FsRead/FsWrite ancestor-of semantics; KbRead/KbWrite
  prefix match; Net port-subset check; Spawn exact match.
- `--dry-run` — prints parseable TOML to stdout with provenance header; does not require
  agentd to be on PATH.
- `--force` — bypasses suggested_caps guard.
- `ANTHROPIC_API_KEY` preflight check before exec.
- Sibling + PATH agentd resolution; `--agentd-path` override.
- Distro: `/usr/bin/agentctl` + `/etc/agentd/templates/` in QEMU image overlay.

### Changed
- `agentd` gains a `[lib]` target so `agentctl` can import `agentd::template`,
  `agentd::capability`, and `agentd::config` types.
- `agentd` version bumped to `0.28.0`.

## [p6.1] - 2026-06-17 (v0.27.0)

Phase 6 begins. Agents are now discoverable before they run: `*.template.toml` files in `templates/` describe an agent's capabilities, tools, and sample tasks. `TemplateResolver` loads from the repo catalogue and `~/.agentos/templates/` (user overrides), then lowers to a plain `Config` for `agentd`.

### Added
- `agentd::template` — new public module with `TemplateConfig`, `TemplateMeta`,
  `TemplateCapabilities`, `TemplateCard`, `TemplateResolver`, `TemplateSource`,
  `TemplateEntry`. `TemplateConfig::to_agent_config()` lowers a template to a plain
  `Config` with template-only keys stripped.
- `templates/scout.template.toml` — scout agent as first catalogue entry.
- `TemplateResolver::from_env()` convenience constructor (`~/.agentos/templates/`).
- Path-traversal rejection in `TemplateResolver::resolve()`.
- Name identity check in `resolve()` and `list()` — mismatched `[template].name` vs
  filename stem is rejected (`resolve`) or skipped with `tracing::warn!` (`list`).
- Absolute path validation on `[capabilities].fs_read`/`fs_write` in `to_agent_config()`.
- `list()` deduplicates by name (user dir wins); emits `tracing::warn!` on parse errors
  instead of silently discarding the file.
- 22 unit tests.

### Changed
- `to_agent_config()` now preserves `[agent].capabilities` (e.g. `Mcp` grants with no
  sugar form): sugar caps are built first, then existing agent caps are appended, so
  previously-discarded `Mcp` grants are no longer lost.
- `Config`, `ToolsConfig`, `SchedulerConfig`, `MemoryConfig`, `McpServerConfig`,
  `IsolationMode`, `SeedEntry`, `SegmentConfig`, `MutabilityClass` now derive
  `Serialize` (and `Clone` where missing), unblocking `agentctl` TOML write in p6.2.
- `CONVENTIONS.md` — new "Templates" section.

### Security
- Missing `[capabilities]` in a template lowers to `agent.capabilities = Some([])`
  (deny-all), never `None` (unrestricted).
- Relative paths in `[capabilities].fs_read`/`fs_write` are rejected at lowering time.

---

## [p5.9] - 2026-06-16 (v0.26.0)

Phase 5 hardening (audit remediation) — closes the P1 findings from `docs/AUDIT-phase-5.md`
(resolution table in §8). Each fix ships with a regression test that fails pre-fix. Gate before Phase 6.

### Fixed
- **F-01:** working-memory paging is keyed on a retained-context estimate
  (`memory::context::estimate_context_tokens`) instead of cumulative lifetime spend, which only grew
  and re-paged every turn once budget crossed 90%. Lifetime spend still drives the budget guard +
  advisory. Test: `paging_stops_when_context_below_target`.
- **F-02:** `RedbStore::open` quarantines only on confirmed corruption
  (`StorageError::Corrupted` / `Io(InvalidData)`); lock, permission, transient I/O, and
  upgrade-required errors surface without renaming a valid store. Timestamped `.corrupt` path.
  Test: `transient_open_error_is_not_quarantined`.
- **F-03:** eviction floor is wired to the live write path (`set_segment_limits` → `put`/`append`
  self-trim); canon segments are never evicted (guarded in `evict()`). Tests:
  `eviction_runs_through_live_path`, `canon_is_not_evicted`.
- **F-04:** `debug_assert_counters` reconciles the NAMESPACES counter vs actual key count after every
  mutation. Test: `namespace_counter_matches_key_count`.
- **F-07a:** `page_turns` alternating-role invariant is a runtime `Err` (was a `debug_assert!`).
- **F-09:** `spawn_agent.child_id` is validated (`validate_child_id`) — rejects traversal / namespace
  separators. Tests: `validate_child_id_*`, `spawn_rejects_invalid_child_id`.
- **F-16:** `spawn_agent`/`send_message` batched with other tools no longer terminates the agent; it
  returns `is_error` results for every call and re-infers so the model retries the sole tool alone.

### Added
- Operator segment seeding: `[[memory.segments]]` `seed = [{ key, value }]` (F-14); demo `agents.toml`
  now parses + runs (also adds `spawn_agent`/`send_message`/`list_agents` to `native`, F-15).
- Root `.gitignore` (workspace `target/`, `*.redb`, `.gstack/`, `.DS_Store`).
- 2-boot continuity test: `two_boot_continuity_at_store_level`.

### Changed
- CI + `make clippy-linux` run `cargo clippy --all-targets` (F-13); fixed all surfaced test-only lints.

## [p5.8] - 2026-06-15 (v0.25.0)

Phase 5 hardening: security invariants, FUSE inode pruning, memory store index, docs completeness.

### Added
- **Startup invariant (OV-1):** `memory.store_path` must be absolute and must not fall inside any
  MCP server's `FsRead`/`FsWrite` sandbox prefix. Checked on every startup via `anyhow::ensure!`;
  `..` traversal is resolved by `normalize_path` before comparison. Test:
  `store_path_inside_sandbox_prefix_fails_startup`.
- **`NAMESPACES` redb table:** `TableDefinition<&str, u64>` maintained atomically on every
  `put`, `append`, `delete`, and `evict`. `list_namespaces()` is now O(k) (k = number of distinct
  namespaces) instead of O(n) full ENTRIES scan. One-time backfill on first open of pre-p5.8 stores.
- **`prune_dead_agent()` in `AgentsFs`** (ar-01): lazy pruning in `readdir(Root)` for terminated
  agents. Cleans all 6 inode maps: `dir_inodes`, `inode_to_id`, `dyn_ino_kind`, `lt_key_ino`,
  `kb_seg_ino`, `kb_key_ino`. Shared segments (no `agent/{id}/` prefix) are not pruned.
- **`dyn_file_content()` match dispatch clarified** (ar-02): removed tautological
  `debug_assert!(matches!(...))` inside `LtFile` and `KbFile` arms; the enclosing `match`
  already guarantees the variant — added explanatory comments instead.
- **`getattr` ENOENT guard for memory dirs** (ar-03): `OFF_MEMORY_DIR` (+5) and `OFF_LONG_TERM_DIR`
  (+7) return `ENOENT` when `self.memory.is_none()`. `OFF_SHORT_TERM` (+6) exempted — still served
  from `AgentSnapshot.short_term_previews`.
- **Memory demo `agents.toml`**: two-agent KB write→search→read demo exercising `canon`/`scratch`
  segments, `KbRead`/`KbWrite` capabilities, `spawn_agent`, and `global_token_budget`.
- **CONVENTIONS.md completeness**: `memory_distilled` row added to the Phase-5 event table
  (was missing from p5.3). `event_taxonomy_completeness` test asserts all 9 `memory_*`/`kb_*`
  EventKind strings appear in CONVENTIONS.md.
- **THREAT_MODEL.md §7 expanded** to §7.1–7.6 (memory substrate threats): §7.3 KB exfiltration
  channel, §7.4 prompt-injection persistence, §7.5 `memory.redb` at rest, §7.6 availability.
  Old §7 Summary renumbered to §8.
- **9 new tests** (476 total up from 467): 4 NAMESPACES tests in `store.rs`, 3 surfaces tests
  (`inode_map_pruned_on_snapshot_update`, `getattr_memory_dir_enoent_when_no_store`,
  `getattr_short_term_ok_when_no_store`), 2 main.rs tests
  (`store_path_inside_sandbox_prefix_fails_startup`, `event_taxonomy_completeness`).

### Fixed
- **NAMESPACES backfill non-fatal**: a transient I/O failure (ENOSPC, NFS timeout) during the
  one-time post-upgrade backfill no longer quarantines a valid pre-p5.8 store. On write failure
  the store opens successfully; `list_namespaces()` falls back to O(n) scan until next restart.
- **`ar-03` guard extended to `is_dir_ino()` and `parent_kind()`**: the `getattr()` ENOENT guard
  for `memory/` and `long_term/` when no memory store is configured is now also applied in
  `is_dir_ino()` (prevents stale-inode `opendir` success) and `parent_kind()` (prevents `readdir`
  returning a partial listing instead of propagating ENOENT to the caller).

### Changed
- `agents.toml` rewritten as a memory demo (writer + spawned reader, `project:meta` canon seed,
  `project:research` scratch segment, `claude-haiku-4-5-20251001`, 100k global budget).
- TODOS.md: p5.7-ar-01/02/03/04 closed; p5.7-ar-05 (`MAX_DIR_KEYS` silent truncation) deferred to p6+.

---

## [p5.7] - 2026-06-14 (v0.24.0)

FUSE memory surface: `/agents/<id>/memory/` and `/agents/kb/` read-only directories
expose agent short-term/long-term memory and shared KB segments to control-plane tools.

### Added
- **`surfaces::MemoryAccess` trait**: minimal read-only interface (`list_namespaces`,
  `list_keys`, `get_entry`) defined in the `surfaces` leaf crate so `AgentsFs` can
  browse memory without a circular dependency.
- **`MemoryStore::list_namespaces()`**: default-impl trait method on `MemoryStore`
  (returns empty); overridden by `RedbStore` to scan ENTRIES for distinct namespace
  prefixes.
- **`MemoryAccessBridge`** in `main.rs` (Linux-only): wraps `Arc<dyn MemoryStore>` and
  implements `MemoryAccess` via `iter()` / `get()` / `list_namespaces()`.
- **`AgentSnapshot::short_term_previews: Vec<String>`**: bounded projection (≤20 items)
  of the agent's Tier-2 short-term buffer, formatted `"t{turn} {role}: {preview}"`.
  Populated by `update_snapshot` in the scheduler.
- **FUSE inode scheme extended** (new offsets within per-agent 10-slot window):
  `+5 memory/` (dir), `+6 memory/short_term` (file), `+7 memory/long_term/` (dir).
  Fixed inode `9` for top-level `kb/` dir. Dynamic pool at `≥1_000_000` for
  `memory/long_term/<key>`, `kb/<seg>/`, and `kb/<seg>/<key>`.
- **`/agents/<id>/memory/short_term`**: renders `short_term_previews` from snapshot.
- **`/agents/<id>/memory/long_term/<key>`**: reads live from `MemoryAccess`; up to
  100 keys listed per directory to bound snapshot size.
- **`/agents/kb/<seg>/<key>`**: operator-visible KB browse; only namespaces without
  `agent/` prefix appear; up to 100 keys per segment.
- **`mount()` signature updated** to accept `Option<Arc<dyn MemoryAccess>>`; memory
  subtrees only appear when the store is configured.
- **467 tests** (up from 406): 9 new surfaces tests in initial commit (`memory_subtree_lists_short_and_long_term`,
  `short_term_file_reflects_snapshot_previews`, `kb_segment_browse_returns_entry_content`,
  `large_memory_entry_read_does_not_panic`, `memory_view_stale_snapshot_does_not_tear_ongoing_read`,
  plus updated `all_eight_inodes_registered_after_alloc` and `file_name_for_offset_covers_all_files`);
  13 regression tests added during review/QA hardening passes.

### Changed
- **`MemoryAccessBridge`** errors now emit `tracing::warn!` instead of silently returning
  empty/`None` — `list_namespaces`, `list_keys`, and `get_entry` all log the error and
  the namespace/key on failure, making FUSE surface issues visible in the diagnostic log.
- **`MemoryStore::list_keys(namespace)`** added as a new trait method (default-impl on
  `MemoryStore`, overridden by `RedbStore`): scans ENTRIES keys for a given namespace
  prefix without deserializing values, cutting per-readdir allocation in half for
  `long_term/<key>` and `kb/<seg>/<key>` listings.

### Fixed
- **`getattr(INO_KB)` returns `ENOENT` when no memory store is configured**: previously
  the `kb/` directory appeared in `getattr` responses even when `self.memory.is_none()`,
  making it visible but empty and inconsistent with `readdir`. Now `ENOENT` is returned
  at the `getattr` level to match the `readdir` behavior.
- **`alloc_dir()` inode pool exhaustion guard**: added `debug_assert!` to detect if the
  fixed-inode counter reaches `DYNAMIC_INO_START` (1 000 000), which would corrupt inode
  lookups silently. Fires in debug/test builds; the fixed pool is large enough for
  any realistic agent count.
- **Slash/NUL key filter in `LongTermDir` and `KbSegDir` readdir**: keys containing
  `/` or `\0` are now silently skipped before being emitted as FUSE directory entries.
  Such keys would have caused FUSE to corrupt the virtual path tree or cause kernel
  EINVAL errors on directory listing.
- **Slash/NUL segment filter in `Kb` readdir**: `list_namespaces()` results are now
  additionally filtered for `/` and `\0` characters beyond the existing `agent/` prefix
  filter, preventing malformed segment names from corrupting the `kb/` directory tree.
- **`KbSegDir` readdir no longer panics on map divergence**: the `self.kb_seg_ino[&segment]`
  index access (which could panic if `kb_seg_ino` and `dyn_ino_map` diverge) is replaced
  with `.get()` + `EIO` reply, consistent with the "loop never panics on bad input" invariant.
- **`wrapping_sub` consistency in `file_content_for_ino`**: plain `ino - dir_ino`
  subtraction replaced with `ino.wrapping_sub(*dir_ino)` to match the wrapping arithmetic
  used in every other offset calculation in `agents_fs.rs`.

## [p5.6] - 2026-06-14 (v0.23.0)

Eviction & summarization: per-segment capacity/age eviction floor and optional
end-of-run short-term distillation.

### Added
- **`MemoryStore::evict()`** trait method and `RedbStore` implementation: drops oldest
  entries beyond `max_entries` and/or older than `max_age_secs`, removing ENTRIES +
  INDEX postings + AGE + META doc_count in a single atomic transaction. Returns
  `Vec<EvictedEntry>` with key + reason (`"capacity"` or `"age"`).
- **`AGE` redb table**: composite key → Unix timestamp (seconds). Written atomically
  with every `put()` and `append()` write; removed on `delete()` and eviction.
- **`EvictedEntry`** struct in `memory/mod.rs`: `key: String`, `reason: String`.
- **`EventKind::MemoryEvicted`** in `events.rs`: serializes as `"memory_evicted"`;
  data shape: `{ segment, key, reason }`.
- **Config fields** on `[memory]`: `max_entries_per_segment: Option<usize>`,
  `max_entry_age_days: Option<u64>`, `distill_on_complete: bool` (default false).
- **`Scheduler::with_distillation(store)`** builder: attaches a memory store and
  enables end-of-run short-term distillation. For each completed agent whose
  `short_term` buffer is non-empty, makes one budget-bounded inference call to
  summarize the paged turns and writes the result to `agent/{id}/distilled/…` under
  Tier 3. Respects the global token budget guard; off by default (demos unchanged).
- **`docs/CONVENTIONS.md`**: `memory_evicted` row added to event taxonomy table.
- 406 tests (up from 397): 5 new eviction store tests (`evicts_oldest_beyond_capacity`,
  `evicts_entries_past_max_age`, `eviction_removes_index_postings`,
  `evict_empty_namespace_returns_empty`, `evict_below_capacity_does_nothing`),
  2 scheduler tests (`distill_on_complete_promotes_to_tier3`,
  `distill_disabled_no_extra_inference`), 2 config tests.

## [p5.5] - 2026-06-14 (v0.22.0)

Retrieval as tool: `kb_search` with BM25-lite inverted index over the shared KB.

### Added
- **`kb_search` tool** (`tools/native.rs`): BM25-lite ranked retrieval over a KB
  segment. Requires `KbRead` capability. Inputs: `segment`, `query`, optional `author`
  filter, optional `limit` (default 10, max 50). Output: flat JSON with `hits` (content +
  provenance expanded), `terms_matched`. All-stopword queries return a structured empty
  with `note` field.
- **Inverted index** (`memory/index.rs`): `tokenize()` (lowercase, split non-alphanumeric,
  skip stopwords + >64-byte tokens), `term_frequencies()`. 21-word stoplist.
- **`INDEX` redb table**: key = `"{namespace}\x00{word}"`, value = JSON posting list.
  ENTRIES + INDEX + META updated atomically in a single write transaction per put/append/delete.
- **`doc_count:{namespace}` META key**: tracks corpus size for BM25 IDF; incremented on
  new-key writes, decremented on delete.
- **`MemoryStore::search()`** trait method: `(hits, terms_matched)` return; `SearchHit`
  struct; `RedbStore` implements full BM25-lite; `SimpleStore` test mock uses brute-force
  linear scan.
- **`KbSearch` flight event**: `agent_id`, `segment`, `query_preview` (64-char truncated),
  `hits`, `terms_matched`.
- 397 tests (up from 376): 7 new `store::tests` (ranking, namespace isolation, author
  filter, write/delete round-trip, append indexing, posting-list pruning, stopword guard),
  5 `store::tests` coverage additions (put-overwrite deindex, search-None error, author
  no-provenance include), 2 `tools::tests` flight-event + query-preview tests,
  1 integration test (multi-write ordered hits with provenance), 2 `native::tests`
  (KbSearch missing-segment and empty-query guards).

### Fixed
- `append()` used `is_empty()` on a `String` to detect new keys; replaced with
  `is_none()` on the `Option` so an existing entry whose value is `""` does not
  re-increment `doc_count`, preventing permanent BM25 IDF drift.
- `search()` now skips zero-score candidates (consistent with `SimpleStore` mock) so
  documents whose only posting-list entry is a race artifact do not appear in results.
- Query terms are deduplicated and capped at 64 unique terms before scoring to bound
  worst-case BM25 work regardless of repeated terms in the LLM-supplied query.

### Security
- `kb_search` gated behind `KbRead` capability on the queried segment — same enforcement
  as `kb_get`.
- Cross-segment search returns an error (not silently returning cross-namespace data).
- Stale posting entries (post-delete race) silently skipped during scoring.
- Query term deduplication + 64-term cap prevents adversarial O(n²) scoring via repeated terms.

## [p5.4] - 2026-06-14 (v0.21.0)

Shared KB MVP: multi-agent segmented knowledge base with three mutability classes
(`canon` / `log` / `scratch`), runtime-stamped provenance, and capability-gated
`kb_put` / `kb_get` tools.

### Added
- **`KbPut` / `KbGet` tools** (`tools/native.rs`): Tier-4 KB tools gated behind
  `KbWrite`/`KbRead` capabilities. `kb_put` enforces mutability class: canon → deny,
  log → auto-generated monotonic hex key, scratch → caller key + incrementing version.
  Provenance (`agent_id`, `turn`, `task_fp`, `ts`, `citation`) stamped from `ToolContext`.
- **`MemoryStore` trait extensions**: `segment_class`, `set_segment_class`,
  `next_log_seq` — implemented in `RedbStore` via the META table.
- **`[[memory.segments]]` TOML config**: `SegmentConfig { name, class }` in
  `MemoryConfig`; `main.rs` seeds classes into the store at startup.
- **`memory_write` / `memory_read` events extended** with `tier: 4` and `class`
  fields for KB operations.
- **THREAT_MODEL.md §7.1/§7.2**: KB poisoning and cross-agent exfiltration analysis.
- 376 tests (up from 336). 6 new feature tests + 30 new tests from pre-landing review:
  RedbStore segment_class/next_log_seq/next_scratch_version persistence, kb tool
  exclusion from "all", store=None silent-skip, kv_set canon/log denial, event field
  assertions.

### Fixed (pre-landing review)
- **`kv_set` bypassed canon/log enforcement**: `KvSet::invoke` now checks `segment_class`
  before writing, matching the invariant enforced by `KbPut`.
- **Scratch version TOCTOU**: `next_scratch_version()` added to `MemoryStore` trait and
  `RedbStore`; `KbPut` scratch branch atomically bumps the counter before constructing
  the entry, preventing two concurrent writers from producing identical version numbers.
- **Enum duplication**: `config::SegmentClass` replaced by re-export of
  `memory::MutabilityClass` (with `serde::Deserialize`); manual 3-arm translation in
  `main.rs` eliminated.
- **`seg_class:` / `log_seq:` prefixes**: extracted to named constants in `store.rs`.

## [p5.3.5] - 2026-06-14 (infra-only, no version bump)

Detachable memory volume: `memory.redb` (Tiers 3/4) now lives on a persistent,
re-attachable host volume rather than the ephemeral output mount. Kill + respawn the
AgentOS container and re-attach the same volume for knowledge continuity.

### Changed
- **`distro/overlay/init`**: added `memory0` 9p virtfs mount to `/run/memory`.
- **`distro/Makefile`**: added `-virtfs local,path=$(HOME)/.agentos-memory,...` to
  `QEMU_FLAGS`; `prereqs` creates `~/.agentos-memory/` on first run; `test` target
  uses a per-run temp directory for the memory volume.
- **`distro/overlay/etc/agentd/agent.toml`**: `store_path = "/run/memory/memory.redb"`.
- **`agentd/src/config.rs`**: doc comment updated with container deployment guidance.
- **CI guard confirmed at 6 MB** (`MAX_BYTES=6291456`); stale "≤ 4 MB" references in
  ROADMAP.md and RUNBOOK.md corrected.
- No crate logic changed; no schema migration; default `store_path` unchanged.

## [p5.3] - 2026-06-14 (v0.20.0)

Per-Agent Long-Term Memory + Checkpoint Coexistence: agents can now explicitly
distil knowledge to a durable Tier-3 store (`mem_remember`) and retrieve it
across restarts (`mem_recall`). Memory survives clean exit; checkpoints do not.

### Added
- **`ToolContext`** struct (`tools/mod.rs`): `{ agent_id, turn, task_fp }` —
  runtime-stamped, unforgeable provenance injected into every `Tool::invoke`.
  `task_fp` is an FNV-1a 64-bit hash of the agent's initial task text (16 hex
  chars), recomputed from the checkpoint on restore.
- **`MemRemember`** tool (`tools/native.rs`): `mem_remember { content, tags }` —
  stores a JSON entry `{ content, tags, provenance: { agent_id, turn, ts, task_fp } }`
  under `agent/{id}` namespace with a nanosecond-timestamp key. Max 8 KiB per entry.
  No capability required (implicit self-grant). Emits `memory_distilled` flight event.
- **`MemRecall`** tool (`tools/native.rs`): `mem_recall { query, limit }` — iterates
  `agent/{id}` namespace, filters by substring match (content + tags, case-insensitive),
  returns JSON array newest-first. Default limit 10, max 50.
- **`EventKind::MemoryDistilled`** (`events.rs`) — emitted by `ToolRegistry::invoke`
  post-call for `mem_remember`.
- **`register_native`** updated: `"mem_remember"` and `"mem_recall"` are explicit
  opt-in names (like `kv_get`/`kv_set`); silently skipped if `store = None`.

### Changed
- `Tool::invoke` signature: `async fn invoke(&self, input, ctx: &ToolContext)` —
  all existing `impl Tool` updated to accept `_ctx: &ToolContext`.
- `ToolRegistry::invoke` takes `ctx: &ToolContext` instead of `agent_id: &str`.
- `AgentTask` gains `task_fp: String` field (runtime-only; recomputed on restore).
- `agentd` version: 0.19.0 → 0.20.0.

### Fixed (ship review)
- `MemRemember`/`MemRecall`: namespace now validated via `validate_segment` (consistent
  with `kv_get`/`kv_set`; rejects agent IDs with spaces or null bytes).
- `MemRemember` size guard now checks the serialized entry (`content + tags + provenance`)
  instead of `content` alone; tags could previously cause the stored value to exceed 8 KiB.
- `MemRecall` now rejects empty `query` with an explicit error instead of silently returning
  all entries (empty string matched every record).
- `MemRemember` key generation propagates system clock errors instead of falling back to
  key `0x0000000000000000` (which could silently overwrite other entries).
- `MemoryDistilled` event docstring corrected (removed `key`/`segment` fields that were
  never emitted; actual payload is `{ agent, turn, items: 1 }`).

### Tests
- 336 tests (up from 322 in p5.2). New: 15 unit tests for `MemRemember`/`MemRecall`
  (remember→recall, tag match, cross-agent isolation, oversized content, no-cap,
  store-absent skip, not-in-all, registry post-call hook; coverage gap tests: missing
  field errors, limit clamping, default limit, newest-first ordering, MemoryDistilled
  event emission).

## [p5.2] - 2026-06-14 (v0.19.0)

Per-Agent Short-Term Memory + Paging: Tier-2 eviction buffer; agents under
budget pressure page old turns out of active context instead of hitting
`budget_exceeded`.

### Added
- **`memory/context.rs`**: `MemoryPressure` enum, `assess()` (budget % → pressure
  level), `page_count()` (pairs eligible for eviction), `page_turns()` (two-pass
  serialize-then-drain, preserving alternating-role invariant). Constants:
  `SOFT_THRESHOLD = 0.75`, `HARD_THRESHOLD = 0.90`.
- **`MemItem`** struct (`memory/context.rs`): `{ turn: u32, role: Role,
  content_preview: String, blocks_json: String }` — serializable paged turn pair.
  `role: Role` (typed, not `String`).
- **`short_term: Vec<MemItem>`** field on `AgentTask` and `AgentCheckpoint`
  (`#[serde(default)]` for v1 back-compat).
- **Paging in `step_need_infer`**: Soft pressure → `MemoryPressureAdvisory` event
  (advisory only, no text injection, edge-triggered on None→Soft transition only).
  Hard pressure → `page_turns()` evicts oldest pairs; on success emits `MemoryPaged`;
  on serde error emits `Error` and skips. Hard pressure with context too short to page
  emits one advisory on first entry instead of silently doing nothing.
- **`to_checkpoint` / `from_checkpoint`** explicitly updated to include `short_term`.
- **Flight events**: `EventKind::MemoryPressureAdvisory`, `EventKind::MemoryPaged`.
- **FORMAT_VERSION 1 → 2**: additive bump; v1 checkpoints load with `short_term = []`.
- **`short_term_depth()`** public accessor on `AgentTask`.
- **FORMAT_VERSION migration policy** documented in `docs/CONVENTIONS.md`.
- Both new events added to CONVENTIONS.md event table; `memory` module boundary updated.

### Changed
- `FORMAT_VERSION` in `checkpoint.rs`: 1 → 2.
- `agentd` version: 0.18.0 → 0.19.0.

### Fixed
- `MemoryPressureAdvisory` no longer spams the flight log — edge-triggered (fires
  once on transition, not every turn at soft/hard pressure).
- `content_preview` in `MemItem` was always empty for `ToolUse` blocks; now uses
  the tool name as preview (e.g. `"read_file"`).
- `debug_assert!` in `page_turns` validates alternating-role invariant before drain.

### Tests
- 322 tests (up from 304 in p5.1). New: 14 unit tests covering all acceptance
  criteria (AC1–AC14 from plan).

## [p5.1] - 2026-06-14 (v0.18.0)

Storage Primitive: durable key/value store backed by redb 4.1.0.

### Added
- **`MemoryStore` trait** (`memory/store.rs`): `get`, `put`, `append`, `delete`,
  `iter`, `meta_version`. Sync methods; `Send + Sync`.
- **`RedbStore`** (`memory/redb_store.rs`): redb 4.1.0 implementation.
  Namespace+key encoding: `"{ns}\x00{key}"`. Handles `DatabaseAlreadyOpen`
  (retry with fresh open after brief delay), corrupt db (renamed to `.corrupt`
  and recreated). `RedbStore::open()` on macOS / `RedbStore::try_open()` internal.
- **`[memory]` config block** (`config.rs`): `store_path` (default `"memory.redb"`)
  and `enabled` (default `true`).
- **`kv_get` / `kv_set` tools** (`tools/native.rs`): structured namespace + key
  fields; `spawn_blocking` for sync redb calls; `MAX_KV_VALUE_BYTES = 256 KiB`
  per-value limit enforced in `kv_set`. **Not** included in `native = ["all"]`;
  require explicit listing.
- **`KbRead` / `KbWrite` capabilities** (`capability.rs`): `segment` field with
  `:` / `/` delimiter-boundary validation; `satisfies()` extended.
- **Flight events**: `MemoryRead`, `MemoryWrite`, `MemoryError`, `MemoryStoreOpened`.
  `ToolRegistry::invoke` emits memory events after successful kv tool calls.
- **`docs/INTERFACE.md`** — agent-facing interface doc (tools, capabilities, events).
- **`docs/SPIKES/p5.1-storage-primitive.md`** — implementation notes.

### Fixed
- Adversarial review (p5.1): `kv_set` now enforces `MAX_KV_VALUE_BYTES = 256 KiB`
  to prevent unbounded redb entry growth.

## [p4.7] - 2026-06-13 (v0.17.0)

Pre-Phase-5 cleanup sprint. Addresses all P0/P1 findings from
`docs/AUDIT-phase-4-6.md`. No new features.

### Security
- **F-001 (P0): MCP subprocesses no longer inherit parent environment.**
  `McpClient::spawn` now calls `env_clear()` then re-adds a vetted allowlist
  (`PATH`, `HOME`, `USER`, `LANG`, `LC_ALL`, `TMPDIR`) plus any explicit
  `env` map declared in `[[tools.mcp_servers]]`. `ANTHROPIC_API_KEY` and all
  other secrets are no longer passed to MCP server subprocesses.
  `McpServerConfig` gains an `env: HashMap<String, String>` field (default empty).
  `docs/THREAT_MODEL.md §1.3` documents the env isolation contract.
- **F-002 (P1): `Net{ports}` on pre-V4 kernel falls back to `IsolateNetwork`.**
  Previously, declaring `Net{ports:[443]}` on a kernel < 6.7 silently resulted in
  no network isolation at all (worse than declaring no `Net` capability).
  Now `caps_to_rules` detects V4 availability via a new public `sandbox::landlock_v4_available()`
  function and emits `IsolateNetwork` (deny-all) with a `tracing::warn!` on pre-V4 kernels.
  Documented in `THREAT_MODEL.md BP-4a`.
- **F-003 (P1): uid/gid_map written after `unshare(CLONE_NEWUSER)`.**
  `apply_compiled_inner` now writes `/proc/self/setgroups=deny`, `uid_map`, and
  `gid_map` (1:1 mapping of the real uid) after a successful `unshare`. Without
  this, the subprocess ran as the overflow uid (`nobody`/65534) for DAC purposes,
  silently defeating `AllowFsRead`/`AllowFsWrite` Landlock grants for user-owned
  files with modes < 0644.

### Fixed
- **F-004 (P1): FUSE `read()` `offset + size` overflow fixed.**
  `agents_fs.rs:305` now uses `offset.saturating_add(size as usize)` to prevent
  a panic in debug mode on kernel-supplied offsets near `usize::MAX`.
- **F-005 (P1): Mailbox drain moved to after `step_with_response`.**
  Previously `drain_mailbox` ran between `provide_inference` (stores response but
  doesn't push it to messages) and `step_with_response` (pushes the assistant turn).
  Injected messages were silently stitched before the assistant reply they were
  conceptually delivered after. Now the drain runs after `step_with_response` so
  injected messages land on the *next* turn's user message.
- **F-010 (P2): MCP UTF-8 validated once at newline, not per fill-chunk.**
  `read_line_bounded` now accumulates raw bytes in a `Vec<u8>` and calls
  `String::from_utf8` once at the newline. Per-chunk `str::from_utf8` failed on
  multi-byte codepoints spanning the 8 KB BufReader boundary.
- **F-011 (P1): Checkpoint version probed before full deserialization.**
  `CheckpointStore::load` now deserializes a `VersionProbe { format_version }`
  stub first, distinguishing "too new" (explicit refusal with a clear message) from
  "corrupt" (serde error). Tmp files now use a unique name
  (`checkpoint.json.<pid>.<nanos>.tmp`) to prevent races between concurrent agentd
  processes sharing a working directory.
- **F-014 (P1): README `##Status` updated to reflect Phases 0–4 complete (v0.16.0).**
- **Ship-review: `apply_compiled_inner` no longer heap-allocates in fork child.**
  `format!("0 {uid} 1\n")` inside `apply_compiled_inner` violated the function's
  async-signal-safe contract — `format!` calls `malloc`, which can deadlock in a
  multi-threaded process (Tokio runtime) if the allocator mutex is held by another
  thread at the moment of `fork`. Replaced with a stack-allocated `id_map_entry`
  helper that writes "0 {id} 1\n" into a `[u8; 16]` without any allocation.
- **Ship-review: checkpoint tmp-name uniqueness window fixed.**
  `tmp_path()` used `subsec_nanos()` (wraps 0–999,999,999 every second) instead of
  `as_nanos()` (monotonically increasing since UNIX epoch). Two saves within the same
  process in the same wall-clock second could produce the same tmp filename. Changed to
  `d.as_nanos() as u64` for true monotonic uniqueness.

### Documentation
- **F-013 (P1): CONVENTIONS.md event taxonomy completed.**
  Added 6 missing `EventKind` rows (`tools_registered`, `agent_child_result_delivered`,
  `agent_checkpointed`, `agent_restored`, `system_shutdown_requested`, `fuse_skipped`)
  and added `events.rs` to the module boundary table. Added an assertion test in
  `events.rs` that pins every variant's serialized string so the table can't drift.
- Updated `THREAT_MODEL.md`: new §1.3 (env isolation), BP-4a (port→deny-all fallback),
  log-injection note.
- **Demo config (`agents.toml`) now exercises admission control and per-agent capability grants.**
  `global_token_budget = 200_000`, `max_concurrent_inferences = 4`, both agents
  have `capabilities = [{ FsRead = { prefix = "." } }]`. Commented MCP example included.
- **CI**: Added `audit` job running `cargo audit` on every push.
- **TODOS.md**: Added TODOS entries for F-006, F-007, F-008, F-009, F-012 (deferred P2 findings).

## [p4.6] - 2026-06-13 (v0.16.0)

### Added
- **Landlock V4 TCP port enforcement**: New `SandboxRule::AllowNetConnect { port: u16 }` enforces
  per-port TCP connects via Landlock ABI V4 (Linux kernel ≥ 6.7). `Net { hosts, ports: Vec<u16> }`
  capability gains a `ports` field (`#[serde(default)]` for backward compat). `caps_to_rules()`
  maps `Net.ports` to `AllowNetConnect` rules. ABI version is detected at runtime via
  `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)`: V4 (≥ 6.7) enables
  enforcement; older kernels degrade silently (BestEffort). Hostname enforcement remains advisory
  (Landlock restricts ports, not hostnames).
- **`EnforcementStatus.landlock_net`**: New boolean field on `EnforcementStatus` and
  `SandboxApplied { enforced }` payload. Allows operators to distinguish V4 net enforcement
  (TCP ports restricted) from V3/degraded (no net enforcement).
- **`run_probe --log-path` fix**: `run_probe` now threads `log_path: PathBuf` through to
  `FlightRecorder::new` via `resolve_log_path()`, honouring the CLI flag. Previously it always
  wrote to `"flight.jsonl"` regardless of `--log-path`.

### Fixed
- **Landlock FS lockout on net-only configs (CRITICAL)**: When only `AllowNetConnect` rules were
  present and no FS rules, `build_landlock_ruleset` set `handled_access_fs=ACCESS_FS_HANDLED`
  with zero path-beneath rules. After `landlock_restrict_self`, ALL filesystem access was denied
  (EACCES on every open/read/write). Fixed in two places:
  - `compile()`: ABI version is now queried before the `has_landlock_rules` gate. On V3 kernels
    with only net rules, `has_landlock_rules=false`, so no ruleset is created at all (correct
    BestEffort degradation).
  - `build_landlock_ruleset()`: `handled_access_fs = if path_entries.is_empty() { 0 } else
    { ACCESS_FS_HANDLED }`. A net-only V4 ruleset correctly declares no FS handling.
- **`is_noop_deny_spawn` false positive for V4 net enforcement**: Added `&& !enf.landlock_net`
  check so active V4 port enforcement is not treated as a no-op sandbox.
- **Port 0 validation in `caps_to_rules()`**: Port 0 is not a valid TCP port (kernel returns
  EINVAL). `caps_to_rules()` now skips port 0 with `tracing::warn` rather than forwarding it to
  `AllowNetConnect`.
- **`PREVIEW_CHARS` constant**: Named constant replacing magic numbers `80` and `200` in
  `run_probe`; ensures truncation lengths are consistent.
- **Stale `agentd/Cargo.lock`**: Removed nested `agentd/Cargo.lock` (recorded v0.8.0); the
  workspace-root `Cargo.lock` is authoritative (v0.16.0).

### Tests
- 253 tests (up from 244 at p4.5). Coverage additions:
  - `noop_deny_spawn_false_when_landlock_net_active`: V4 net enforcement → not a noop
  - `caps_to_rules_net_port_zero_is_skipped`: port 0 filtered before AllowNetConnect
  - `allow_net_connect_only_no_fs_rules_does_not_lock_out_fs`: net-only compile must succeed
  - `compile_net_only_has_landlock_rules_iff_v4_available`: BestEffort consistency check
  - `no_fuse_env_var_falsy_values_do_not_activate`: AGENTOS_NO_FUSE=0/false/no/"" are falsy
  - `log_path_flag_without_value_exits_nonzero`: --probe --log-path (missing arg) exits non-zero
  - `allow_net_connect_enforcement_status_reflects_abi` (Linux): fd/flag consistency on V3/V4
  - `allow_net_connect_with_fs_rule_compiles_together` (Linux): combined FS+net compiles

## [p4.5] - 2026-06-13 (v0.15.0)

### Added
- **`--log-path <file>` CLI flag and `log_path` TOML field**: Override the flight
  recorder destination. Precedence: CLI `--log-path` > TOML `log_path` > default
  `"flight.jsonl"`. `--log-path` missing its value argument now fails with a clear
  error instead of silently falling back. `run_agent` helper `resolve_log_path`
  encapsulates the precedence chain.
- **aarch64 DenySpawn noop detection**: On non-x86_64 targets where seccomp is not
  compiled, a sandbox with only `DenySpawn` produces no kernel mechanism. The runtime
  now detects this and emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }`
  instead of a misleading `SandboxApplied` with all-false fields. Detection is gated
  on `DenySpawn` specifically in the rule set (not `!is_empty()`) to avoid false
  positives when FS rules were also present but Landlock is unavailable.
- **`EventKind` extracted to `events.rs`**: The `EventKind` enum moved from
  `flight_recorder.rs` into its own `events.rs` module and is re-exported from
  `flight_recorder` for backward compat. Makes the event taxonomy a first-class module.
- **`BR2_CCACHE`**: Buildroot ccache enabled in `distro/buildroot.config`. Subsequent
  clean builds use the host cache (~2 min vs ~30 min).

### Fixed
- `is_noop_deny_spawn` call site now checks specifically for `SandboxRule::DenySpawn`
  in the rule set rather than `!is_empty()`, preventing a misleading
  `"deny-spawn-unsupported-arch"` diagnostic when FS or network rules were present.

### Tests
- 244 tests (up from 225 at p4.4). Coverage additions:
  - `parse_log_path`: 4 cases including trailing-flag-no-value (documents silent-None contract)
  - `filter_positional_args`: 4 cases including both flags together
  - `resolve_log_path`: 3 precedence-chain cases
  - `is_noop_deny_spawn`: 6 cases covering all 4 enforcement fields + has_rules variants

## [p4.4] - 2026-06-13 (v0.14.0)

### Added
- **`checkpoint.json` mode 0600**: `write_mode_600()` creates the tmp file with
  `O_CREAT|O_EXCL|mode(0o600)` plus unlink-retry, guaranteeing 0600 even if a
  stale tmp file exists at a different mode. `rename(2)` atomically replaces the
  final `checkpoint.json`. Checkpoint is now owner-readable only regardless of umask.
- **pre_exec sandbox error pipe**: `McpClient::spawn` on Linux creates a
  `pipe2(O_CLOEXEC)` error pipe *only* when a sandbox is configured. On spawn
  failure the error message includes `"(sandbox stage: 'sandbox'|'unknown')"` so
  operators can distinguish a sandbox-apply failure from a missing-binary error.
  Unsandboxed servers produce a clean error without the stage suffix.
- **`--no-fuse` CLI flag + `AGENTOS_NO_FUSE` env var**: `agentd --no-fuse agent.toml`
  or `AGENTOS_NO_FUSE=1 agentd agent.toml` skips the FUSE mount and emits a
  `FuseSkipped` flight event. `AGENTOS_NO_FUSE=0/false/no` correctly disables the
  flag (any other non-empty value enables it). Makes CI output clean.
- **`EventKind::FuseSkipped`**: new flight event kind emitted when `--no-fuse` is
  active; preserves the CONVENTIONS.md invariant that every meaningful step is
  recorded (analogous to `SandboxSkipped`).
- **`sandbox_probe` integration tests (Linux)**: 3 tests in `tests/integration.rs`
  — `allowed_path_read_succeeds`, `denied_path_read_fails`, `deny_spawn_blocks_fork`
  (x86_64 only) — verify Landlock + seccomp enforcement end-to-end using the
  `sandbox_probe` fixture binary.

### Fixed
- THREAT_MODEL.md §3.2–3.3: updated to reflect checkpoint mode restriction.

## [p4.3] - 2026-06-12 (v0.13.0)

### Added
- **`docs/THREAT_MODEL.md`**: full threat model covering secret handling,
  flight-recorder data sensitivity, checkpoint.json exposure, budget-exhaustion DoS
  guards, supply chain posture, and sandbox bypass vectors (BP-1 through BP-6) with
  explicit "not yet fixed" labels for each known gap.

### Fixed
- **`ToolCall` event now logs `input_preview` (≤200 chars) instead of the full,
  untruncated tool input**: prevents large file contents and any short secrets
  passed as tool arguments from landing verbatim in `flight.jsonl`.
- **`ToolResult` error path now logs `error` as ≤200-char preview**: previously the
  error message (which may echo back tool arguments) was logged verbatim.
- **`AgentSpawned` event now logs `task_preview` (≤200 chars) instead of the full
  task string** on both the TOML-config path (`main.rs`) and the dynamic spawn path
  (`scheduler.rs`); both now use `truncate()` with the `…` truncation marker.
- **`truncate()` and `PREVIEW_CHARS` made `pub` in `agentd::agent`**: previously
  private, preventing reuse from `main.rs` and `scheduler.rs`.

### Known Limitations (TODOS.md)
- `checkpoint.json` has no encryption or restricted file permissions; tracked as
  P3 TODOS entry for a future increment.
- 200-char truncation does not prevent short secrets (≤200 chars) in tool
  arguments; operational guidance: pass secrets via environment, not tool inputs.
- `cargo audit` CVE scanning not yet in CI.

### Tests
- All 216 tests pass (macOS; +1 new unit test for ToolResult error truncation).

## [p4.2] - 2026-06-11

### Added
- **`IsolateNetwork` and `IsolateMount` `SandboxRule` variants**: applied via `unshare(CLONE_NEWUSER | CLONE_NEWNET/CLONE_NEWNS)` in `pre_exec`. BestEffort degradation if kernel policy blocks user namespaces (`EPERM`/`ENOSYS`).
- **`Net` capability now enforced at kernel level**: `caps_to_rules()` adds `IsolateNetwork` whenever the `Net` capability is absent. MCP servers without an explicit `Net` grant are network-isolated by default. Previously `Net` was advisory-only.
- **`isolation = "gvisor"` field on `[[tools.mcp_servers]]`**: wraps the server command with `runsc do [--network=none] --`. agentd fails fast at startup if `runsc` is not found on PATH. gVisor's Sentry handles all syscall interception — Landlock/seccomp/namespace pre_exec is skipped for gVisor-mode servers.
- **`EnforcementStatus` extended**: `namespace_net: bool` and `namespace_mount: bool` fields added. `SandboxApplied` event payload extended with `isolation`, `namespace_net`, `namespace_mount` fields.
- **`CONFIG_USER_NS=y`, `CONFIG_NET_NS=y`, `CONFIG_UTS_NS=y`** in `distro/kernel-extras.config` for QEMU image.

### Changed
- **Breaking:** `capabilities = []` now also produces `IsolateNetwork` (network-isolated). Previously it produced only `DenySpawn`. Servers that need outbound access must add `Net` to their capabilities list.
- **`capabilities = ["Spawn"]` behavior**: previously produced empty rules (caught by `mcp_require_capabilities` as a bypass). Now produces `[IsolateNetwork]` — a real enforcement rule. The config is valid; the server can spawn children but cannot reach the network.

### Known Limitations (TODOS.md)
- `runsc do` is experimental; full OCI bundle integration deferred.
- `clone3()` bypass remains in the namespace-only path (gVisor fixes it).
- `CLONE_NEWPID` for PID namespace requires a re-fork; deferred.

### Tests
- **209 tests pass** (macOS + CI).
- 8 new sandbox unit tests (`isolate_network/mount` variants, `enforcement_status` namespace fields).
- 3 new config unit tests (`isolation` field parsing).
- 7 updated `caps_to_rules` unit tests reflecting `IsolateNetwork` default.
- 1 new integration test: `isolation_gvisor_fails_fast_when_runsc_not_on_path` (Linux only).

## [p4.1] - 2026-06-11

### Added
- **`EnforcementStatus` struct** in `sandbox/src/lib.rs`: `{ landlock: bool, seccomp: bool, spawn_enforcement: &'static str }` — returned by `CompiledSandbox::enforcement_status()` and included in `SandboxApplied` flight events, so operators can distinguish kernels where Landlock or seccomp degraded to a no-op.
- **`mcp_require_capabilities = true`** flag in `[tools]` config: when set, startup fails if any MCP server would run unsandboxed (missing `capabilities` field OR field present but `caps_to_rules()` produces empty rules). Lists all offending server names in the error message.
- **CI binary size guard**: new workflow step checks that the x86_64-unknown-linux-musl release binary is ≤ 4 MB (4 194 304 bytes); fails with a clear message if exceeded.

### Fixed
- **aarch64 BPF gate**: seccomp-bpf fork/vfork block is now gated under `#[cfg(target_arch = "x86_64")]`. On aarch64 (and other non-x86_64 arches), `DenySpawn` emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }` instead of installing a no-op filter that silently claims enforcement.
- **`compile()` moved to `main.rs`**: `McpClient::spawn` no longer calls `compile()` internally. The parent compiles rules before fork and passes `Option<CompiledSandbox>` directly, keeping the child's `pre_exec` closure allocation-free.
- **`mcp_require_capabilities` bypass**: validation now calls `caps_to_rules()` to check for empty effective rules, not just `capabilities.is_none()`. `capabilities = ["Spawn"]` (which maps to zero kernel rules) is correctly rejected.
- **`SandboxSkipped` on non-Linux with capabilities**: the `had_sandbox` variable is captured before the compiled sandbox is consumed by `McpClient::spawn`, fixing a case where the non-Linux `SandboxSkipped` event was never emitted for servers with capabilities configured.
- **Misleading sandbox log**: the "MCP server running unsandboxed" warning now distinguishes between "no capabilities field" and "capabilities produce no effective rules".

### Tests
- **208 tests pass** (macOS + CI).
- 6 `EnforcementStatus` unit tests in `sandbox/src/lib.rs`.
- 4 `mcp_require_capabilities` integration tests in `agentd/tests/mcp.rs`, including a regression test for the `capabilities = ["Spawn"]` bypass.
- `MAX_BYTES` named constant replaces bare `4194304` in the CI size guard script.

## [p3.3] - 2026-06-11

### Added
- **`sandbox/` crate**: new Rust library crate (`sandbox`) in the workspace. Provides
  kernel-level enforcement for MCP server subprocesses via two mechanisms:
  - **Landlock LSM** (Linux 5.13+): filesystem path-beneath rules. `AllowFsRead { prefix }`
    grants `ReadFile | ReadDir`; `AllowFsWrite { prefix }` grants all ABI V1 flags except
    Execute. BestEffort — degrades silently on older kernels without breaking startup.
  - **seccomp-bpf** (`DenySpawn` rule): classic BPF filter installed in `pre_exec` that
    blocks `fork(2)` and `vfork(2)` on x86_64, preventing the MCP server from spawning
    new child processes. Exec is intentionally left unblocked (the initial `execve` that
    loads the MCP binary must succeed); Landlock FS rules persist across exec.
- **`capabilities` field on `[[tools.mcp_servers]]`**: optional array of capability objects
  (`FsRead { prefix }`, `FsWrite { prefix }`, `Net { hosts }`, `Mcp { server, tools }`,
  `Spawn`). When present, a sandbox is compiled and applied to the server subprocess before
  exec. When absent, the server runs unsandboxed with a `tracing::warn!` and a
  `SandboxSkipped` flight event. `capabilities = []` with no `Spawn` produces a
  `DenySpawn`-only sandbox (fork/vfork blocked; no FS restriction).
- **`caps_to_rules()` adapter** in `main.rs`: converts agent `Capability` values to
  `SandboxRule` values — `FsRead`/`FsWrite` map 1:1; `Spawn` suppresses `DenySpawn`;
  `Net`/`Mcp` are advisory (kernel-level net enforcement deferred to Landlock ABI V4).
- **`EventKind::SandboxApplied` / `SandboxSkipped`**: emitted in `flight.jsonl` after
  each MCP server spawn, recording which rules were applied or why the sandbox was skipped.
- **`CONFIG_SECCOMP=y` / `CONFIG_SECCOMP_FILTER=y`** added to `distro/kernel-extras.config`.
- **`docs/SPIKES/p3.3-ebpf-lsm.md`**: implementation spike doc covering raw syscall ABI,
  BPF filter construction, execute-bit exclusion, known limitations, and CI gate.

### Fixed
- **`O_NOFOLLOW` on Landlock path fds**: `open_path_fd` now passes `O_NOFOLLOW` so a
  symlink at the configured prefix cannot redirect the Landlock allowance to another dir.
- **`SandboxApplied` accuracy**: only emitted on Linux (non-Linux is a no-op platform);
  not emitted when compiled rules are empty (e.g. `capabilities = [{ Spawn }]` only).
- **Empty `caps_to_rules` result treated as no sandbox**: `capabilities=[{Spawn}]` maps to
  zero kernel rules and now correctly emits `SandboxSkipped` rather than a misleading
  `SandboxApplied { rules: [] }`.

### Tests
- **180 tests pass** (macOS + CI); Linux-gated tests (`allow_fs_write_*`, `combined_fs_*`,
  `deny_spawn_bpf_includes_vfork_on_x86_64`) verified by CI.
- 6 `caps_to_rules` unit tests in `main.rs`.
- 3 `McpServerConfig` capability TOML parse tests in `config.rs`.
- 1 `sandbox_event_kinds_serialize_to_snake_case` test in `flight_recorder.rs`.
- 5 sandbox-crate tests: `PartialEq`, Landlock rule construction, combined Landlock+BPF,
  vfork BPF instruction count (expects 6: `load + fork + vfork + allow`).

## [p3.2] - 2026-06-10

### Added
- **`agentd/src/checkpoint.rs`**: new module — `CheckpointStore` (atomic
  `tmp → rename` writes), `AgentCheckpoint`, `SchedulerCheckpoint`,
  `AwaitingEntry` serde types; `FORMAT_VERSION = 1`.
- **`AgentTask::to_checkpoint()`** / **`from_checkpoint()`** / **`is_terminal()`**:
  serialise/deserialise agent working state; `from_checkpoint` always clears
  `terminal` to guard against the terminal-race (OV-2); `is_terminal` lets the
  scheduler filter finished agents from checkpoint writes.
- **Periodic auto-checkpoint**: `SchedulerConfig::checkpoint_interval_turns`
  (default `1`); fires at every `provide_tool_results` boundary when the agent
  turn count is a non-zero multiple of the interval.
- **SIGTERM checkpoint**: when the scheduler's SIGTERM handler fires it calls
  `checkpoint_all()` before exiting; if the save fails the error is recorded and
  shutdown continues without crashing.
- **Corrupt-checkpoint recovery**: if `checkpoint.json` exists but fails to
  parse, `main.rs` renames it to `checkpoint.json.corrupt` and boots fresh.
- **Full restore**: `Scheduler::new()` accepts an optional `SchedulerCheckpoint`;
  restores `awaiting` map, per-agent mailboxes, `tokens_spent`, `child_seq`, and
  `spawn_depths`; orphan children in the checkpoint (not in the TOML spec) are
  also restored.
- **New flight events**: `AgentCheckpointed { agent_id }`,
  `AgentRestored { agent_id }`, `CheckpointFailed { reason }`.
- **`agentd/.gitignore`**: `checkpoint.json` and `checkpoint.json.corrupt`
  excluded from version control.

### Changed
- `SchedulerConfig` gains `checkpoint_interval_turns: u32`; default `1`; `0`
  disables periodic checkpointing.
- `Scheduler::new()` signature gains a 7th argument
  `Option<SchedulerCheckpoint>`; existing call-sites in `main.rs` updated.
- `InferenceResponse` and `MailMessage` derive `Serialize` (required by checkpoint
  serialisation).
- `Makefile` `clippy-linux` target: add `rustup component add clippy` before the
  cargo invocation so the Docker image works on aarch64 hosts.
- Test helper `sched_cfg()` sets `checkpoint_interval_turns: 0` to prevent
  concurrent scheduler tests from racing on `./checkpoint.json.tmp`; dedicated
  checkpoint tests explicitly opt in with `checkpoint_interval_turns: 1`.

### Tests
- 9 new unit tests in `agentd/src/scheduler.rs` (checkpoint restore, periodic
  checkpoint, `AgentCheckpointed` flight event, test-isolation mutex for
  `sigterm_drains_scheduler`).
- 5 new unit tests in `agentd/src/agent/mod.rs` (`is_terminal`, `to_checkpoint`,
  `from_checkpoint`, roundtrip).
- 1 new unit test in `agentd/src/flight_recorder.rs` (checkpoint event
  serialisation).
- 10 unit tests in `agentd/src/checkpoint.rs` (serde roundtrips, save/load,
  corrupt handling).
- Total: **175 tests** (174 pass; 1 live-API integration skipped).

## [p3.1] - 2026-06-10

### Added
- **`surfaces/` crate**: new Rust library crate (`surfaces`) sibling to `agentd/`;
  root `Cargo.toml` promoted to a workspace with `members = ["agentd", "surfaces"]`
  and the release profile moved there.
- **`surfaces::snapshot`**: `SchedulerSnapshot`, `AgentSnapshot`, `AgentStatus`
  (`Running`, `Deferred`, `AwaitingChild(String)`, `Done`, `Failed`); shared via
  `Arc<RwLock<SchedulerSnapshot>>` between scheduler and FUSE handler.
- **`surfaces::agents_fs`** (Linux-only FUSE handler): `AgentsFs` implements
  `fuser::Filesystem`; inode scheme (root=1, agent dirs from 1010 step 10, file
  offsets +1..+4); four virtual files per agent (`status`, `context_size`, `budget`,
  `flight`); TTL=0 (no kernel caching); `read_flight_tail()` scans last 64 KB of
  `flight.jsonl`, returns up to 20 matching lines per agent.
- **`surfaces::agents_fs::mount()`**: spawns FUSE `BackgroundSession` on Linux;
  no-op stub on other platforms — clean build everywhere.
- **`Scheduler` snapshot plumbing**: `Scheduler::new()` accepts a 7th argument
  `Arc<RwLock<SchedulerSnapshot>>`; `update_snapshot()` is called after the seed loop
  and after every effect result, keeping the snapshot current.
- **`AgentTask` getters**: `context_tokens()` and `task_preview(max_chars)` added
  to `agent/mod.rs` for snapshot population.
- **`EventKind::FuseMounted` / `FuseUnmounted`**: emitted in `main.rs` when
  `agentd` mounts/unmounts `/agents`.
- **`distro/overlay/agents/.gitkeep`**: creates the `/agents` mount point in the
  Buildroot rootfs overlay.
- **`CONFIG_FUSE_FS=y`** in `distro/kernel-extras.config` so the QEMU VM can
  serve FUSE mounts.
- **15 unit tests** in `surfaces/src/agents_fs.rs` covering inode allocation, file
  content rendering, read slicing, and flight tail parsing.

### Changed
- **`fuser` dependency** is in `[target.'cfg(target_os = "linux")'.dependencies]`
  to avoid `pkg-config --libs fuse` failing on macOS during `cargo check/test`.
- All `#[cfg(target_os = "linux")]`-gated items that are also needed by tests use
  `#[cfg(any(test, target_os = "linux"))]` so the test suite runs on all platforms.

## [p2.5] - 2026-06-09

### Added
- **MCP tools/list pagination**: `McpClient::spawn` now follows `nextCursor` in a
  cursor-based loop until all pages are exhausted. Previously only the first page was
  fetched; tools on page 2+ were silently dropped.
- **`McpClient::shutdown()` method**: sends `notifications/shutdown` (JSON-RPC notification,
  no id), waits up to 5 s for the server to exit cleanly, then escalates to SIGTERM, waits
  another 5 s, and lets `kill_on_drop` deliver the final SIGKILL. Servers that flush WAL or
  release locks on clean exit now get the chance to do so.
- **Graceful shutdown on all exit paths**: `run_agent` in `main.rs` calls
  `client.shutdown().await` for each MCP client on three exit paths: successful completion,
  `AnthropicGateway::from_env` failure, and `Scheduler::new` failure. The previous
  code used `?` early-return on the latter two, causing SIGKILL-only teardown.
- **`StopReason::MaxTokens` → `AgentEffect::Failed`**: when the model is cut off
  mid-generation the agent now emits a `BudgetExceeded` flight event and returns
  `AgentEffect::Failed("model generation hit max_tokens limit …")` instead of silently
  returning `Ok("")`. Callers can now distinguish a truncated response from a real empty answer.
- **`nix` dependency** (`v0.29`, `signal` feature) promoted from dev-dependency to
  dependency so `kill(SIGTERM, …)` is available in production `shutdown()`.
- **`tokio` `fs` feature** added to `Cargo.toml` for `tokio::fs` in native tools.

### Changed
- **Native tools use `tokio::fs`**: `ReadFile`, `WriteFile`, and `ListDir` now use
  `tokio::fs::read_to_string`, `tokio::fs::write`, `tokio::fs::create_dir_all`, and
  `tokio::fs::read_dir` with the async entry iterator. Previously they used blocking
  `std::fs` calls on the tokio thread pool, which would have stalled concurrent agents.

### Tests
- 2 new unit tests in `agent/mod.rs`: `max_tokens_with_no_text_returns_failed`,
  `max_tokens_with_partial_text_returns_failed`.
- 2 new integration tests in `tests/mcp.rs`: `mcp_pagination_loads_all_pages` (asserts
  all three tools from a two-page echo-mcp paginated server appear in `tools_registered`);
  `mcp_graceful_shutdown_sends_notification` (asserts echo-mcp writes a file on
  `notifications/shutdown` before exiting).
- `echo-mcp` fixture updated: `--paginate` flag returns two-page tool list with
  `nextCursor`; `--shutdown-file <path>` flag writes `"shutdown"` to path on notification.

## [p2.3] - 2026-06-09

### Added
- **SIGTERM/SIGINT handling in `Scheduler::run()`**: replaced the `while let
  Some(er) = pending.next().await` loop with `loop { tokio::select! { ... } }`.
  Signal arms set `shutdown_requested = true` and break, causing in-flight futures
  to be dropped and the existing deferred-queue drain to run.
- **`EventKind::SystemShutdownRequested`** flight event: emitted with
  `{ "signal": "SIGTERM" }` or `{ "signal": "SIGINT" }` when a signal fires.
- **`tokio` `signal` feature** added to `Cargo.toml`; **`nix` dev-dependency**
  (v0.29, `signal` feature) added for test-side signal delivery.
- **`sigterm_drains_scheduler` test**: sends SIGTERM 50 ms into a 30-second gateway
  delay; asserts `run()` returns in < 5 s and the flight log contains the shutdown
  event.

### Not in scope
- Graceful MCP shutdown (SIGTERM + drain before SIGKILL) → p2.5
- Essential mounts: already done in `distro/overlay/init` (p2.2)
- Zombie reaping: already handled by tokio (owns SIGCHLD; competing handler disallowed)

## [p2.2] - 2026-06-09

### Added
- **`distro/` Buildroot external tree**: x86_64 musl + BusyBox; `make build` produces
  `output/bzImage` + `output/rootfs.cpio.gz` (cpio initramfs).
- **`/init` PID-1 script** (`distro/overlay/init`): mounts proc/sys/devtmpfs, mounts two
  virtio-9p host directories (`secrets0` → `/run/secrets/`, `output0` → `/run/output/`),
  sources `agentos.env`, and `exec`s agentd. Drops to busybox sh on mount/secret failure.
- **virtio-9p kernel config** (`distro/kernel-extras.config`): `CONFIG_9P_FS`, `CONFIG_NET_9P`,
  `CONFIG_NET_9P_VIRTIO`, `CONFIG_VIRTIO_NET`, `CONFIG_IP_PNP_DHCP` applied on top of
  `x86_64_defconfig`.
- **`make prereqs / build / run / test / clean / distclean`**: `test` boots with `-no-reboot`
  and confirms an `agent_completed` or `budget_exceeded` event in `output/test-run/flight.jsonl`.
- **Demo agent config** (`distro/overlay/etc/agentd/agent.toml`): Haiku model, native tools
  only, writes a greeting to `/run/output/greeting.txt`. Validates the full boot-to-inference path.
- **No system CA certs needed**: agentd's bundled `webpki-roots` (via `reqwest rustls-tls`)
  provides Mozilla CAs; the rootfs carries no `ca-certificates` package.

## [0.7.0] - 2026-06-09

### Changed
- **`reqwest` TLS backend**: switched from `native-tls` to `rustls-tls` (`default-features = false, features = ["json", "rustls-tls"]`). No longer requires OpenSSL headers at build time or system OpenSSL at runtime.

### Build
- **Static musl binary**: `cross build --target x86_64-unknown-linux-musl --release` produces a `static-pie linked, stripped` ELF binary (~3.1 MB) with no dynamic dependencies. Use `cross` (Docker-based) from macOS; on Linux with musl toolchain available, `cargo build --target x86_64-unknown-linux-musl --release` works directly.

## [0.6.0] - 2026-06-09

### Added
- **`AgentCard { id, name, description, skills }`**: derived from `AgentConfig` at scheduler seed time. Emits `agent_card_registered` flight event per agent.
- **`AgentConfig` identity fields**: optional `name`, `description`, `skills` TOML fields (all with `#[serde(default)]`). `name` defaults to `id` when absent.
- **`bus.rs` module**: `MailMessage { from, content }` and `Mailboxes = HashMap<String, Vec<MailMessage>>`. Canonical home for A2A bus primitives.
- **`list_agents` tool**: returns a sorted JSON array of all registered `AgentCard`s. No capability required — available to every agent.
- **`send_message` tool + `AgentEffect::SendMessage { call_id, to, content }`**: sole-call tool intercepted by the scheduler. Delivers message to recipient's mailbox; synthesizes an immediate `ToolResult` so the sender continues. Unknown recipient returns an `is_error` tool result (no panic, no crash).
- **Mailbox drain before each inference**: `drain_mailbox` is called after `provide_inference`/`provide_tool_results` and before `step()`. `AgentTask::inject_messages` appends mail as a `Block::Text` to the last `User` message, preserving the Anthropic API's strict alternating-role requirement.
- **Shutdown drain fix**: `shutdown_requested: bool` in `SchedulerState`. `drain_deferred` now checks this flag and emits `agent_admission_denied { reason: "shutdown" }` instead of re-queuing agents that can never run.
- **New flight events**: `AgentCardRegistered`, `MessageSent`, `MessageReceived`.
- **9 new unit tests** covering: `inject_messages` appends to last User msg; empty inject is noop; sole-call guard for `send_message`; missing `to` field error; `send_message` delivery + `message_sent` event; unknown-recipient error; `AgentCard` name defaulting; explicit name/skills round-trip; TOML parsing of new identity fields.

### For contributors
- `dispatch_send_message` in `scheduler.rs` handles the full message lifecycle: recipient validation → mailbox push → `MessageSent` flight event → synthesize ToolResult → re-enqueue sender.
- `register_native` gains a third `cards: Option<Arc<Vec<AgentCard>>>` parameter; pass `None` in tests.
- `agents.toml` example updated with `name`, `description`, `skills` fields on both agents.

## [0.5.0] - 2026-06-09

### Added
- **`spawn_agent` tool**: an agent with the `Spawn` capability calls `spawn_agent{task, child_id?, priority?, token_budget?}` to create a child agent. The child runs to completion; its result is injected back into the parent as a `ToolResult` so the parent can continue. The call must be the sole tool use in its turn.
- **`SchedulerState` refactor**: all mutable scheduler run-loop state consolidated into a single `SchedulerState` struct (`agents`, `outcomes`, `pending`, `deferred`, `in_flight`, `tokens_spent`, `awaiting`, `child_seq`, `spawn_depths`, `max_spawn_depth`). Eliminates the previous 13-loose-locals pattern.
- **`AgentEffect::SpawnAgent { call_id, config }`**: new variant intercepted by the scheduler before any tool `invoke()`. The agent state machine recognizes a `spawn_agent` tool-use response and returns this effect instead of `CallTools`.
- **Spawn depth limit**: `max_spawn_depth: u32` in `[scheduler]` TOML (default 4). If exceeded, the parent receives an `is_error` tool result instead of a child being created.
- **Child admission denial**: if a child's first inference is denied (budget or slot exhausted), the parent receives an `is_error` tool result and continues running.
- **`Capability::Spawn` enforcement**: `dispatch_spawn` checks the parent's cap set; absence of `Spawn` returns an `is_error` tool result to the parent rather than creating a child.
- **`agent_child_result_delivered` flight event**: emitted when a child's result is injected into its parent, carrying `{child_id, parent_id, call_id, success}`.
- **`SpawnAgentTool`** in `native.rs`: registered as a stub tool so it appears in `filtered_specs` for agents with `Spawn` capability. Its `invoke()` is a safety net that always errors (the scheduler intercepts before `invoke` is reached).
- **Child ID naming**: auto-generated as `"{parent_id}-child-{seq}"` with a monotonic counter.
- **Child inherits parent's capabilities and `model_cfg`**: spawned child uses the same model and capability set as its parent (unless overridden).

### Fixed
- `Capability::Spawn` was previously hard-coded to always return `false` in `satisfies()`; it now correctly checks whether the granted set contains `Spawn`.
- `SchedulerConfig::Default` now returns `max_spawn_depth = 4` instead of `0` (the derived `Default` was overriding the serde default, silently disabling all spawning for Rust-constructed configs).

### For contributors
- `SpawnConfig` struct in `config.rs`: `{ child_id: Option<String>, task: String, priority: u32, token_budget: Option<u64> }`.
- `dispatch_spawn` in `scheduler.rs` handles the full spawn lifecycle: cap check → depth check → child ID → child `AgentTask` creation → awaiting registration → seeding.
- `handle_agent_terminal` routes child completions to the parent via `provide_tool_results` + `step` + `enqueue_or_defer`; non-child completions go straight to `outcomes`.
- `send_message` deferred to p1.6 (Agent Cards increment).

## [0.4.0] - 2026-06-08

### Added
- **Capability system** (`capabilities` TOML field on `[[agents]]`/`[agent]`):
  least-privilege tool grants — `FsRead{prefix}`, `FsWrite{prefix}`, `Net{hosts}`,
  `Mcp{server, tools}`, `Spawn`. Absent field = unrestricted (backward compat);
  `capabilities = []` = deny all.
- **Capability enforcement at `ToolRegistry::invoke`**: the single unbypassable
  boundary; denials emit a `capability_denied` flight event with data `{tool, required}`
  (the agent id is in the event's top-level `agent` field) and return an `is_error`
  tool result to the agent.
- **`filtered_specs`**: agents only receive the tool specs they are authorized to
  call in their inference context — no wasted inference turns on inaccessible tools.
- **`normalize_path`**: resolves `..` components without filesystem access before
  prefix matching, blocking directory traversal (e.g. `/workspace/../etc/passwd`
  is correctly denied against a `/workspace` prefix grant).
- **`satisfies_type`**: type-level capability check used by `filtered_specs` —
  "does this agent have any FsRead capability?" vs. "can they access this specific path?"
- **`McpTool` server provenance**: `server_name` field on `McpTool` enables
  `Mcp{server, tools}` capability gating on per-server MCP tool access.

### For contributors
- New `agentd/src/capability.rs`: `Capability` enum, `normalize_path`, `satisfies`,
  `satisfies_type`. All capability logic lives here; no policy is embedded in tools.
- `Tool` trait gains `fn required_capability_for(&self, input: &Value) -> Option<Capability>`
  (default `None`). Path-based tools return the actual access path at invocation time.
- `ToolRegistry::invoke` gains `(agent_id, cap_set, recorder)` params.
- `run_tools_sequential` gains `cap_set: Option<&[Capability]>` param; threaded through
  to `invoke`. Driver passes `None` (backward compat).
- `Scheduler::new` calls `filtered_specs(cap_set)` per agent instead of shared `specs()`.

## [0.3.0] - 2026-06-08

### Added
- **Metered scheduling & admission control** (`[scheduler]` TOML section): cap total
  token spend across all agents with `global_token_budget` and limit how many model
  calls can run concurrently with `max_concurrent_inferences`. Both default to `0`
  (unlimited), preserving all prior behavior.
- **Priority-based deferred queue**: each agent carries a `priority: u32` field
  (default `0`). When the concurrency cap is full, the agent's inference is queued and
  admitted in descending-priority order (FIFO within a band) when a slot opens.
- **Admission-control flight events**: `agent_scheduled`, `agent_deferred`, and
  `agent_admission_denied` appear in `flight.jsonl`, giving full observability into
  scheduler decisions.

### Fixed
- `in_flight` underflow guards promoted from `debug_assert!` (compiled out in release)
  to `assert!`, ensuring the invariant is enforced in production builds.

### For contributors
- `SchedulerConfig` struct in `config.rs` carries `global_token_budget` and
  `max_concurrent_inferences`; wired into `Scheduler::new` via `main.rs`.
- `DeferredInfer` type with a custom `Ord` drives the `BinaryHeap` deferred queue.
- `drain_deferred` / `enqueue_or_defer` manage the admission lifecycle; both are
  tagged with `TODO(p1.x)` noting a planned `SchedulerState` refactor.

## [0.2.0] - 2026-06-08

### Added
- **Multi-agent scheduler**: Run multiple agents concurrently on independent tasks with a
  single `agentd agents.toml` invocation. Agents share a gateway and tool registry; each
  runs its own perceive → infer → act → observe loop without blocking the others.
- **`[[agents]]` config form**: Declare multiple agents in one TOML file using the
  `[[agents]]` array. The original `[agent]` single-agent form is fully backward-compatible.
- **`agents.toml` example**: Ships a two-agent example config alongside the existing
  `agent.toml`.
- **`AgentFailed` flight event**: Emitted when an agent terminates due to an inference
  error, completing the `AgentSpawned` ↔ terminal-event symmetry in the flight log.
- Non-zero exit code when any agent fails; individual per-agent errors logged with agent ID.

### For contributors
- Agent loop refactored into a sans-IO state machine (`AgentTask` + `AgentEffect`).
  `step()` → `AgentEffect` drives the loop; the scheduler performs all async IO and
  feeds results back via `provide_inference()` / `provide_tool_results()`. Enables
  concurrent IO across agents without threads.
- `driver::run` is now a single-agent backward-compat shim; the scheduler is the
  primary execution engine for all runs.
- `AgentSpawned` events are emitted before gateway initialization so startup events
  always appear in the flight log even when API key setup fails.
- `run_tools_sequential` extracted as `pub(crate)` in `agent/mod.rs`, shared by the
  driver and the scheduler.

### Fixed
- MCP child processes are now properly cleaned up on agent failure: `run_agent` returns
  `Err` instead of calling `std::process::exit(1)` while `mcp_clients` is still in scope,
  ensuring `kill_on_drop` fires before the process exits.
- Guard added for `stop_reason=tool_use` responses that contain no `ToolUse` blocks —
  previously would have sent an empty User message to the API.

## [0.1.0] - 2026-06-07

Initial release: config loader, flight recorder, `InferenceGateway` trait, Anthropic
backend, tool ABI, native file tools, MCP stdio client, and a single-agent
perceive → infer → act → observe loop.
