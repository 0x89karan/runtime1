# TODOS

## Phase 5 — Open (deferred from p5.9 hardening; all P2, none data-loss-class)

See `docs/AUDIT-phase-5.md §8` for full context. p5.9 closed every P1; these P2s remain:

- **F-05 (P2) — `checkpoint.save()` has no `fsync` before/after rename.** 4.6 F-012
  unfixed: a crash mid-rename can leave a zero-length/partial checkpoint. Add
  data fsync + parent-dir fsync around the atomic rename.
- **F-06 (P2) — FUSE dynamic-inode counter never reclaimed / unguarded.** Long-lived
  mounts can exhaust the counter; no overflow guard on the mount thread.
- **F-07b (P2) — `inject_messages` can append `Text` onto a ToolResult-only User turn.**
  Reintroduces 4.6 F-006 shape; `page_turns` previews `blocks.first()` (the ToolResult),
  so a paged inter-agent message is misrepresented in `short_term` / FUSE. (F-07a — the
  alternation debug-assert — was promoted to a runtime `Err` in p5.9.) Fix needs care:
  pushing a new User turn risks consecutive-User violations; prefer making the preview
  pick the first `Text` block, and/or fold injected text into the assistant-adjacent turn.
- **F-08 (P2) — `short_term` is unbounded and full-cloned every checkpoint + snapshot tick.**
  Bound it (ring buffer) and avoid the per-tick clone.
- **F-10 (P2) — private `agent/<id>` tier shares keyspace/delimiter with grantable `kb:*`.**
  Reserve the `agent/` prefix so it can't be granted via `segment:"agent"`.
- **F-11 (P2) — unconfigured segments default to writable Scratch (fail-open).**
  Consider deny-by-default for undeclared segments.
- **F-12 (P2) — distillation ignores per-agent budget; emits event even if the store
  write failed.** Charge distillation against budget; only emit on a confirmed write.

## Phase 5 — Open (deferred from p5.8)

**~~p5.7-ar-01 (P2) — Inode map entries are never pruned for terminated agents~~** ✓ Fixed in p5.8.
- `prune_dead_agent()` method added to `AgentsFs`; called in `readdir(Root)` for every agent ID
  in the current snapshot that is absent from the live agent set. Cleans all 6 maps: `dir_inodes`,
  `inode_to_id`, `dyn_ino_kind`, `lt_key_ino`, `kb_seg_ino`, `kb_key_ino`.

**~~p5.7-ar-02 (P2) — HashMap lookup in `getattr`/`read` does not assert inode kind~~** ✓ Fixed in p5.8.
- Tautological `debug_assert!(matches!(...))` removed from `dyn_file_content()` LtFile and KbFile arms;
  the enclosing `match` already guarantees the variant — explanatory `// ar-02:` comments added instead.

**~~p5.7-ar-03 (P2) — `getattr` returns `Directory` for `memory/` and `memory/long_term/` even when memory store is not configured~~** ✓ Fixed in p5.8.
- `getattr()` now returns `ENOENT` for `OFF_MEMORY_DIR` (+5) and `OFF_LONG_TERM_DIR` (+7) inodes when
  `self.memory.is_none()`. `OFF_SHORT_TERM` (+6) intentionally exempted (served from `AgentSnapshot`).

**~~p5.7-ar-04 (P3) — `list_namespaces` is O(n) full ENTRIES scan (no NAMESPACES index)~~** ✓ Fixed in p5.8.
- `NAMESPACES: TableDefinition<&str, u64>` redb table maintained atomically on every put/append/delete/evict.
  One-time backfill on first open of pre-p5.8 stores. `list_namespaces()` is now O(k) (k = distinct namespaces).

**p5.7-ar-05 (P3) — `MAX_DIR_KEYS=100` truncation is silent (no overflow marker in readdir)**
- `agents_fs.rs:capped_keys`: the cap is applied with `.take(MAX_DIR_KEYS)`. When a namespace has more
  than 100 entries, the directory listing is silently truncated. An `ls` that returns 100 entries is
  indistinguishable from one that exhausted the full set.
- Fix (or document): emit a sentinel file entry (e.g. `…truncated`) in readdir when the cap fires,
  or increase `MAX_DIR_KEYS` and add a per-call budget. Document the limit prominently in RUNBOOK.md.
  (RUNBOOK.md already documents this; a sentinel file would be the runtime signal.) Deferred to p6+.

## Phase 7 — Open (deferred from p7.5 autoplan, 2026-06-23)

**p7.5-scope-01 — Universal-tier egress deferred to p7.5b** ✅ *resolved in p7.5b (v0.40.0)*
- All three architecture problems addressed: hyper v1 (E2 ✅), ephemeral per-workload API keys
  for caller identity (E3 ✅, key-based vs. FD-pass — simpler and avoids exec boundary complications),
  loopback-only bind + FUSE `egress_addr` surface (E1 ✅). FD-pass deferred to p7.6 when microVM
  isolation requires it.
- `[[workloads]]` TOML schema not needed: workloads register at runtime via `ProxyRegistry::register()`.
- Fail-closed proxy invariant: `start_http_proxy()` → `anyhow::ensure!` on empty real key; returns
  `Err` on bind failure; scheduler fails-closed if proxy start fails.

**p7.5-scope-03 — resume_chain does not re-verify existing receipts on restart**
- On restart `EvidenceWriter::open()` re-opens an existing `evidence.jsonl` and anchors to the
  last line without verifying its signature. An attacker with file write access before restart
  can inject a poisoned anchor line; the verifier (`agentctl verify`) still catches this from
  genesis, but future receipts chain from the poisoned anchor.
- Fix in p7.5b: on open, call a lightweight `scan_last_verified_line()` that verifies the last
  N lines (or reads the last signed seq from a sidecar `.chain-head` file). Low priority while
  evidence.jsonl lives in the operator-controlled runtime dir (same threat model as the keyfile).

**p7.5-scope-02 — Allowlisted host forwarder deferred (host policy hard problem)**
- `Capability::Net.hosts` remains advisory in p7.5 and p7.5b (proxy always forwards to one
  hardcoded upstream `ANTHROPIC_MESSAGES_URL`; `allowed_hosts` field is scaffolding only).
  Full per-workload host enforcement requires: DNS rebinding mitigations, CNAME/IP canonicalization,
  IPv4/IPv6 literals, redirect following policy, link-local/RFC-1918 denials, host suffix
  confusion guards.
- Deferred to p7.6 alongside the isolation floor. Document semantic change in CONVENTIONS.md
  and CHANGELOG when proxy enforcement lands.

**p7.5b-ar-01 (HIGH) — Budget TOCTOU: concurrent requests can over-spend**
- Pre-check `load() == 0` and post-response `fetch_update(AcqRel, ...)` are not atomic.
  N concurrent requests can all pass the zero-check and collectively over-spend the budget
  by up to N × request_cost before any decrement lands. Saturating arithmetic prevents wrap,
  but the counter reaches zero only after over-spend.
- Acceptable for p7.5b single-agent soft-budget. True fix: pre-reserve an estimated cost
  via CAS before forwarding and refund the remainder post-response. Revisit in p7.6 when
  microVM workloads can fire concurrent requests.

**p7.5b-ar-02 (MEDIUM) — anthropic-beta passthrough allows workload-controlled feature flags**
- `anthropic-beta` header is forwarded verbatim. A workload can opt into experimental
  Anthropic API features (e.g. `files-api-2025-04-14`) that may have different cost profiles
  or capability gates the operator did not intend to allow.
- Low risk in current single-agent context (operator controls the workload config anyway).
  Revisit in p7.6 when multi-workload operator may want to restrict beta feature usage.
  Fix: validate against an operator-configured allowlist in `EgressConfig`.

## Phase 7 — Open (deferred from p7.6 review)

**p7.6-ar-02 (P2) — Ephemeral key uses nanosecond timestamp, not CSPRNG**
- `scheduler.rs:439`: `format!("ua-{}-{:016x}", cfg.id, ts_nanos)` — predictable from agent ID (visible at `/agents/<id>/`) + timestamp. A compromised child agent could reconstruct a sibling's key and make proxy requests billed to the sibling's budget.
- Mitigated by: proxy listens on loopback-only (single-tenant, no remote attacker), and agent IDs differ between siblings.
- Fix: replace timestamp with `rand::thread_rng().gen::<u64>()` (16 hex chars of CSPRNG). Requires adding `rand` to `agentd/Cargo.toml`.

**p7.6-ar-03 (P2) — 11 of 17 plan-required tests absent, including env isolation tests**
- Most security-relevant gaps: `universal_agent_env_injection` (child env contains ephemeral key, not real key) and `universal_agent_env_clear` (parent secrets e.g. GITHUB_TOKEN absent from child). These are integration-level tests that require spawning real processes and passing a secret through the allowlist. Also missing: `universal_agent_started_event`, `universal_agent_exit_event`, `universal_agent_sigterm_on_shutdown`, `universal_agent_gvisor_argv`, `universal_agent_send_message_is_error`, `scheduler_universal_only_does_not_exit_prematurely`, `checkpoint_compat_native_only_restores`, `egress_addr_threaded_to_universal_spawn`.
- The env_clear() + allowlist behavior is verified by code inspection (Finding 1 PASS) but not by an automated test.
- Fix: add `universal_agent_env_injection` and `universal_agent_env_clear` as integration tests that spawn a real `printenv`-style child and check the output. Then complete the remaining list.

**p7.6-ar-04 (LOW) — `universal+isolation=none` silently allowed (plan says reject)**
- `universal.rs:62–64`: the plan's validation matrix says `tier=universal + isolation=none → error`. The implementation accepts it and spawns without sandboxing. The `agent_config_universal_defaults` test explicitly asserts `isolation=none` is valid, confirming this is an intentional relaxation for development ergonomics.
- Fix: decide whether `isolation=none` is the allowed dev mode (update plan + doc) or should be rejected (add validation + update test). Currently blocked on that design decision.

**p7.6-ar-05 (LOW) — `UniversalOutputTruncated` event missing; stdout uses `Stdio::inherit()`**
- Plan specifies `universal_output_truncated` event when stdout/stderr exceeds 4 MB, with stdout/stderr captured and forwarded to the flight log. `universal.rs:95–96` uses `Stdio::inherit()` instead, meaning output goes directly to agentd's fd without capture or truncation detection.
- Acceptable scope reduction for v1 (operator sees output in terminal), but `universal_output_truncated` event kind should be added to `events.rs` as a stub so CONVENTIONS.md is internally consistent.

**p7.6-ar-01 (P2) — `dispatch_spawn` collision guard misses `universal_agents` map**
- `dispatch_spawn` checks `state.agents.contains_key(&child_id) || state.outcomes.contains_key(&child_id)` before inserting a dynamic child. Because universal agents live in the separate `state.universal_agents` map (not `state.agents`), a native agent that explicitly requests `child_id = "my-universal-agent"` would collide silently — two agents with the same ID in different maps, producing duplicate FUSE snapshots and confusing `send_message` routing.
- Only triggered by an agent-supplied `child_id` that matches a static universal agent config ID. Auto-generated IDs (`{parent_id}-child-{seq}`) never collide. Single-tenant, operator-controlled config makes this low-probability in practice.
- Fix: extend the collision guard: `|| state.universal_agents.contains_key(&child_id)`.

## Phase 7 — Open (deferred from p7.4 QA)

**p7.4-qa-01 (LOW) — Silent no-op when approval item disappears between List→Confirm mode transitions**
- Repro: operator enters Confirm mode on `act_0`; external process resolves `act_0` via FUSE;
  next tick refreshes `approvals_items` (item gone); operator presses 'a' (approve).
- `approvals_items.get(selected_idx)` returns `None` → no `write_control_command` sent,
  mode silently returns to List with no result message. User may be confused whether approve fired.
- In practice the List view correctly shows 0 items (item was already resolved), so no data loss.
- Fix path: add `result_msg = Some("Item already resolved")` in the `if let Some` else branch
  in `handle_approvals_key` Confirm arms (`agentctl/src/watch/mod.rs`).

**p7.4-qa-02 (LOW) — `read_approvals` has no unit test**
- `agentctl/src/watch/reader.rs::read_approvals()` parses the `/agents/approvals` JSONL file
  (11 lines) but has no coverage for: empty/`[]\n` sentinel, multi-item path, malformed-line skip.
- Fix path: add 3 unit tests to `reader.rs` covering these paths.

## Phase 7 / Harness — Open (from h7.2)

**~~h7.2-ar-01~~ (resolved v0.50.0) — generic `agent` entrypoint mode**
- Resolved by adding `agent)` mode to `docker/entrypoint.sh`: `agentctl spawn --dry-run` + sed
  path rewrite (`../docker/` → `/etc/agentd/`) covers all standalone templates.
- `docker-compose.yml` now has `agent` service with `HOME=/data`, `AGENTOS_REPO_TEMPLATES_DIR`,
  OAuth env, and `agent-data` named volume. `DRY_RUN_ONLY=1` env enables smoke testing without
  a live API key.

## Phase 7 / Harness — Open (from h7.1)

**h7.1-ar-01 (P3) — MCP server script paths are hardcoded relative to agentd/ CWD**
- `args = ["../docker/shell_mcp.py"]` in template TOML files works only when agentd is
  invoked from `agentd/` (the CWD assumed by `cargo run`). If agentd is run from the repo
  root or an installed path, the relative path breaks.
- Future fix: support a `${AGENTOS_SCRIPTS_DIR}` interpolation token in `args` that resolves
  to the directory of the running agentd binary, or an env var the operator sets at install
  time. Deferred — relative paths work for the common `cargo run` development case.

## Phase 7 — Open (deferred from p7.3 review)

**p7.3-ar-01 (LOW) — `child_seq` consumed on auto-ID collision with existing named agents**
- `dispatch_operator_spawn` increments `state.child_seq` before the collision guard runs.
  If the auto-generated `"operator-N"` collides with a user-named agent and the command is
  rejected, `child_seq` is permanently incremented — causing gaps in numbering on the next spawn.
- Not data-loss class; only manifests if the operator boots an agent named `"operator-0"` etc.
  Silent rejection is observable via `FuseControlError` in `tail flight.jsonl`.
- Fix path: move the `child_seq` increment to after the collision guard, or generate the ID
  without incrementing (probe loop). Deferred — not worth restructuring the lock for this.

**p7.3-ar-02 (P3) — `agentctl spawn` CLI execs a second agentd instead of using /agents/control**
- The `agentctl spawn <template> --task "…"` CLI path always execs a new agentd binary,
  even when an agentd scheduler is already running with the FUSE surface mounted.
- Correct fix: detect `/agents/control` exists → write JSON there → print confirmation.
  `execute_pending_spawn()` already implements this logic in the TUI watch path; extract
  it as a shared helper so both the TUI and CLI spawn paths route correctly.
- Deferred; tracked in memory as p7.3-cli-revisit. Implement before p8.

## Phase 7 — Open (deferred from p7.2 review)

**p7.2-ar-01 (P2) — `stdout_lock` held across `write_all().await` + `flush()` per chunk**
- `scheduler.rs:make_infer_future`: `tokio::sync::Mutex<()>` is correctly held across the
  async write to prevent byte-level interleaving in multi-agent streaming runs.
- Architectural concern: when stdout is a slow pipe, all concurrent streaming agents
  serialise output at OS write speed. Correct tradeoff for the non-interleaving guarantee,
  but future improvement: dedicated writer task that owns stdout, receives chunks from all
  agents via a bounded channel (one queue-send per chunk instead of one lock+write+flush).
- Only manifests with slow consumers (`agentd … | tee slow_log`); no correctness issue.

**p7.2-ar-02 (P2) — `unbounded_channel` for SSE chunks has no backpressure**
- `scheduler.rs:make_infer_future` uses `tokio::sync::mpsc::unbounded_channel` for SSE chunks.
- If `print_fut` stalls (e.g. holding `stdout_lock` while another agent writes), the SSE
  producer can buffer the full model response in memory before the consumer makes progress.
- In practice, buffered data is bounded by `max_tokens × bytes/token`; not a practical risk
  for typical usage. Improvement: switch to `channel(64)` for natural backpressure.
- Pair with p7.2-ar-01: both resolved together by the dedicated-writer-task architecture.

## Phase 7 — Open (deferred from p7.1 review)

**p7.1-ar-01 (P2) — `McpHttpError` event defined but never emitted**
- `events.rs` defines `EventKind::McpHttpError` and CONVENTIONS.md lists it in the taxonomy,
  but `McpHttpClient::request()` and `McpTool::invoke()` never emit it.
- Threading a `FlightRecorder` into `McpHttpClient` at connect time (similar to how native
  tools access the scheduler) is the right fix path.
- Impact: HTTP tool failures are not flight-logged with the `http_status`/`method` fields
  defined in the taxonomy; they surface only as generic tool errors from the agent loop.
- Fix: pass `Arc<FlightRecorder>` + `agent_id` into `McpHttpClient::connect()`, store on
  the struct, emit `McpHttpError` in `request()` before returning `Err`. Defer to p7.2.

**p7.1-ar-03 (P3) — `agentd/tests/mcp_http.rs` integration test file not written**
- The p7.1 plan specified a live-HTTP-listener integration test suite (tokio listener,
  session-ID continuity, 4 MB guard, pagination, HTTP error status codes).
- Unit tests cover the SSE parser and config validation well; the network-path tests
  require `httpmock` or `wiremock` infrastructure that wasn't added in p7.1.
- Deferred from plan: `docs/plans/p7.1-http-sse-mcp-transport.md`. Fix in p7.2.

**p7.1-ar-02 (P3) — SSRF to RFC-1918 / link-local addresses not blocked** ✅ FIXED in h7.1
- `docker/http_mcp.py` now resolves the target hostname via `socket.getaddrinfo` and checks
  `ipaddress.ip_address.is_loopback / .is_private / .is_link_local` before opening any connection.
  Blocks loopback, `169.254.x`, `10.x`, `172.16-31.x`, `192.168.x`.

## cos.1 / Live Testing — Open

**cos-ux-01 — TUI lacks per-agent progress and error visibility**
- During long-running agent turns (e.g. inbox agent fetching 20 Gmail messages), `agentctl watch`
  shows only `running` status and a growing context-size counter. There is no indication of
  what tool the agent last called, what it returned, or whether errors occurred.
- Needed: a live activity pane (or per-agent detail view) showing:
  - Last tool call + result summary (tool name, truncated args/result, timestamp)
  - Last error, if any (`is_error` tool result or `capability_denied` event)
  - Turn count and last-inference timestamp so the operator can distinguish "busy" from "hung"
- Implementation path: tail `/data/flight.jsonl` per-agent (the Inspector view at `[i]` already
  does this globally); expose a filtered single-agent stream in the AgentDetail view; add a
  `last_tool` field to `AgentSnapshot` so the Dashboard table can show it without opening Detail.
- Precedent: `View::Inspector` (`[i]`) already tails the full log with filter/search — extend
  it with an agent-scoped filter that auto-selects when entering from the Dashboard row.

## Phase 5 — Open (deferred from p5.1–p5.5 adversarial reviews)

**p5.5-ar-01 (P3) — Posting list loading is O(n) RAM at query time**
- `RedbStore::search()` fetches the full posting list for each query term into a Vec,
  then unions candidates in a HashSet. For large segments (>100k entries), a common
  term's posting list could consume significant memory.
- Fix path: lazy iterator over posting list entries; avoid materializing the full Vec.
  Or add a cap on posting-list size returned (top-k by recency). Defer to p5.6
  or a dedicated search-performance pass.

**p5.5-ar-02 (P3) — Provenance metadata tokenized alongside content for BM25 scoring**
- `store.rs`: the full raw stored JSON (including `agent_id`, `ts`, `class`, `task_fp`
  fields from provenance) is passed to `index::tokenize()` at both write and score time.
  Queries for structural terms like `"scratch"` or `"agent"` match documents whose
  provenance contains those strings, not based on content relevance.
- Fix: extract `value["content"]` before tokenizing; fall back to raw string only when
  the value is not parseable JSON. Defer to p5.6.

**p5.5-ar-03 (P3) — Author filter silently passes entries that lack provenance**
- `store.rs:526`: `unwrap_or(true)` means entries without a parseable `provenance.agent_id`
  field always pass an `author` filter, regardless of who the caller is filtering for.
  The behavior is intentional and tested, but the tool description doesn't document it.
- Fix: add a `provenance_unknown: true` flag on affected hits, or document the inclusive
  default in the `kb_search` tool's description string. Defer to p5.6.

**p5.4-ar-01 (P3) — Version/seq counter can be bumped without a corresponding entry**
- `tools/native.rs:KbPut::invoke`: for both Log and Scratch, the counter increment
  (`next_log_seq` / `next_scratch_version`) commits in its own write transaction before
  `store.put()`. If the `anyhow::ensure!` size check fires between the two calls, the counter
  is permanently advanced (version gap in Scratch; sequence gap in Log). A deliberately oversized
  `content` field reliably triggers this.
- Impact: low — single-tenant cooperative agents are not adversarial. Consumers of log streams
  may see non-consecutive sequence numbers.
- Fix: move the size check on raw content before the counter call, or combine counter increment
  and entry write in a single redb write transaction. Defer to p5.5 or the next tool-ABI revision.

**kv-ar-01 (P2) — `kv_get` returns `""` for both missing key and empty stored value**
- `tools/native.rs:KvGet::invoke`: `None` and `Some("")` both return `Ok(String::new())`.
  The `MemoryRead` flight event's `found` field uses a non-empty heuristic (`!result.is_empty()`),
  misclassifying an empty stored value as a cache miss.
- Fix: return a sentinel (or a separate `exists()` call) to distinguish miss from empty-value hit.
  Requires ABI change; defer to p5.4 or next tool-ABI revision.

**kv-ar-02 (P2) — `try_open` TOCTOU: `path.exists()` check before `open`/`create`**
- `memory/store.rs:try_open`: the exists check and the subsequent `open`/`create` are not atomic.
  On a concurrent `rm`, the path could disappear between the check and the open.
  Low risk for single-tenant use; note for p5.x when multi-process agents share a store path.

**kv-ar-03 (P2) — AlreadyOpen detection uses `format!("{e:?}")` string matching**
- `memory/store.rs:open`: the code matches `"AlreadyOpen"` / `"already open"` / `"already locked"` in
  the debug-formatted error string. Fragile if redb renames the variant.
- Fix: downcast `anyhow::Error` to `redb::DatabaseError` and match the typed variant.
  Safe to defer while redb is pinned to `= "4.1.0"` in Cargo.toml.

**kv-ar-04 (P2) — Silent kv tool skip when `store=None` emits no flight event**
- `tools/native.rs:register_native`: when `kv_get`/`kv_set` are listed but `store` is `None`,
  the tools are silently skipped (no registration, no warning). An agent that expects the tools
  will see `unknown tool` errors at runtime with no log to explain why.
- Fix: emit a `MemoryStoreOpened`-equivalent warning event (or `MemoryError`) in the skip branch.

**p5.2-ar-01 (P2) — `short_term` Vec grows unbounded; no size/depth cap**
- `agent/mod.rs`: `short_term.extend(items)` is never trimmed. A long-running agent that reads large
  files under sustained hard pressure accumulates unbounded `MemItem` records (each `blocks_json`
  can be MB-sized). The vec is checkpointed every N turns.
- Fix: add a `max_short_term_depth: usize` config (or size-in-bytes watermark); evict oldest items
  from `short_term` itself when the cap is hit. Defer to p5.6 (principled eviction policy).

**p5.2-ar-02 (P2) — `MemItem.turn` records eviction time, not original message turn**
- `agent/mod.rs:page_turns(…, at_turn=self.turn)`: all items in a single batch get the same
  `turn` value (the turn when eviction happened), not the turn when the message was generated.
  Items from turns 1, 2, 3 all appear as e.g. `turn=7` in `short_term`.
- Impact on p5.3: temporal ordering within a batch is lost; items appear artificially newer.
- Fix: pass `original_index: usize` derived from message position when building `MemItem`, or
  add an `evicted_at_turn: u32` alongside a separate `original_turn` field. Coordinate with p5.3.

**p5.2-ar-03 (P2) — Conservative `page_count` formula may provide no practical budget relief**
- `context.rs:page_count`: `(len-1)/4` evicts only 1 pair for 5–8 messages. Under sustained hard
  pressure an agent with 7 messages at 91% removes 2 messages; cumulative spend is unchanged; next
  turn is still Hard. Agent may burn many turns evicting tiny amounts of context.
- Fix in p5.6: replace formula with an aggressive mode (evict all available pairs when entering
  Hard for the first time) and/or a token-weighted eviction (evict until Tier-1 size drops below target).

**kv-ar-05 (P2) — Corrupt-quarantine overwrites previous `.corrupt` file**
- `memory/store.rs:try_open`: rename to `<path>.corrupt` silently discards a previous quarantine
  if a second corruption event occurs before the user investigates.
- Fix: include a timestamp or counter in the quarantine name (e.g. `memory.redb.corrupt.1234567890`).

**p5.3-ar-04 (P2) — `mem_recall` loads full namespace into memory before filtering**
- `tools/native.rs:MemRecall::invoke`: `store.iter(&ns)` returns the entire namespace as a
  `Vec<(String, String)>` before sorting and filtering. An agent that calls `mem_remember` thousands
  of times causes `mem_recall` to allocate all stored data on every search call.
- Fix: enforce a max entry count per namespace at write time in `MemRemember`, or implement a
  cursor/range API in `MemoryStore` that applies the limit at the scan level.

**p5.3-ar-05 (P2) — `task_fp` on checkpoint restore uses `messages[0].blocks[0]`; wrong when first block is not Text**
- `agent/mod.rs:from_checkpoint`: `task_fp` is recomputed from the first block of the first message.
  If the first block is a `ToolUse` or `ToolResult` (programmatically injected task), `task_text`
  becomes `""` and all such agents share the same FNV-1a fingerprint (`cbf29ce484222325`), breaking
  provenance isolation in Tier-3 memory.
- Fix: persist `task_fp` directly in `AgentCheckpoint` (serialize/restore the already-computed value
  rather than recomputing from messages). Alternatively, compute from `cp.cfg.task` if present.

**p5.3-ar-01 (P3) — `PREVIEW_CHARS` constant duplicated across two modules**
- `memory/context.rs` and `agent/mod.rs` each define their own `PREVIEW_CHARS = 200`. If they
  diverge (e.g. someone changes one and not the other), content previews in Tier-2 short_term items
  will be truncated inconsistently compared to the inline `content_preview` field built in the agent loop.
- Fix: define once in `memory/context.rs` as `pub(crate)` and import in `agent/mod.rs`.

**p5.3-ar-02 (P3) — No test for `MemoryDistilled` suppression on `mem_remember` failure**
- `tools/mod.rs:ToolRegistry::invoke`: the `MemoryDistilled` post-call hook fires only after
  `tool.invoke(...).await?` succeeds. There is no test that verifies the event is NOT emitted
  when `mem_remember` fails (e.g. oversized content routed through the registry).
- Fix: add a unit test invoking `mem_remember` via `ToolRegistry` with >8 KiB content; assert
  `Err` returned and no `memory_distilled` event recorded.

**p5.3-ar-03 (P3) — No test for `task_fingerprint()` determinism on known inputs**
- `agent/mod.rs:task_fingerprint`: private FNV-1a hash; no test pins the output for a known
  input string, so a refactor could silently shift all stored `task_fp` values.
- Fix: add a unit test with a known ASCII string (and empty string) asserting the exact 16-char
  hex output; verifies stability across refactors.

## Phase 4 — Open (deferred from p4.7 audit)

Findings from `docs/AUDIT-phase-4-6.md` that are real bugs but do not block Phase 5:

**F-006 (P2) — `inject_messages` appends `Block::Text` onto a ToolResult-only user turn**
- `agent/mod.rs:240-241`: after a tool cycle, the last user message is all `Block::ToolResult`;
  injection pushes a `Block::Text` into it yielding mixed content. Anthropic tolerates this today
  but a stricter provider (or future validation) would reject it.
- Fix: when the target user message contains any `ToolResult`, push a *new* user message instead.

**F-007 (P2) — `StopReason::MaxTokens` mislabelled as `BudgetExceeded`**
- `agent/mod.rs:333-344`: a per-response generation cap (`model.max_tokens`) is conflated with the
  cumulative per-agent `token_budget`. The agent emits `budget_exceeded` even when nowhere near budget.
- Fix: distinct event kind (e.g. `max_tokens_truncated`); don't attach the unrelated `token_budget`.

**F-008 (P2) — `StopReason::Other(_)` and empty `EndTurn` complete silently with `""`**
- `agent/mod.rs:346-371`: unknown/future stop reason or end_turn with only a filtered thinking block
  yields `Completed("")` — a silent empty answer reported as success.
- Fix: treat empty extracted text as `Failed`; handle `Other(_)` distinctly with a warning event.

**F-009 (P2) — Global token budget is a soft, post-hoc ceiling**
- `scheduler.rs:608-609`: a single inference can overshoot the ceiling by up to one inference.
- Fix: document "soft ceiling, overshoot ≤ one in-flight inference per agent" in ROADMAP/NOTES.

**F-012 (P2) — No `fsync` before/after checkpoint rename**
- `checkpoint.rs:47,117`: `write_all` not followed by `sync_all()`; parent dir not fsynced after rename.
  On real power loss the rename or tmp data blocks may not be durable.
- Fix: `f.sync_all().await?` after `write_all`; `File::open(parent).sync_all()` after rename.

**F-015 (P2) — `extra_env` can re-inject secrets via operator config**
- `tools/mcp.rs:100-102`: the `extra_env` loop from `McpServerConfig.env` runs after `env_clear()` with
  no denylist. An operator (or compromised `agents.toml`) can write `ANTHROPIC_API_KEY = "sk-…"` to pass
  the key explicitly to an MCP subprocess — defeating the F-001 env isolation.
- Fix: apply a short hardcoded denylist (e.g. `ANTHROPIC_API_KEY`, any `*_API_KEY`/`*_SECRET` pattern)
  before inserting `extra_env` keys. Log a warning and drop the offending key.

**F-016 (P2) — `drain_mailbox` can inject messages into a just-terminated agent**
- `scheduler.rs:325`: `drain_mailbox` runs after `sm.step()` regardless of terminal state. When `step()`
  returns `AgentEffect::Completed`/`Failed`, the agent is terminal but the mailbox is still drained and
  messages injected, only to be immediately discarded (or serialized into the terminal checkpoint).
- Fix: check `state.agents[&agent_id].is_terminal()` after `step()` and skip drain when terminal.

**F-017 (P3) — `Net{}` with empty ports grants unrestricted network (worse than no `Net`)**
- `main.rs:568-570`: `Net { ports: [] }` sets `has_net = true` (suppressing `IsolateNetwork`) then skips
  the rule loop (`ports.is_empty()` → `continue`). The result: full unrestricted network with no isolation.
  Declaring no `Net` capability gives `IsolateNetwork`; declaring `Net` with empty ports gives nothing.
- Fix (or document): either treat empty-ports `Net` as "unrestricted but acknowledged" (add a warn event),
  or change the semantics to treat empty ports as equivalent to `IsolateNetwork`. Decide in Phase 5.

## Phase 6 — Open (deferred from p6.6 adversarial review)

**p6.6-ar-01 (P3) — `execute_pending_spawn` leaks a NamedTempFile in `/tmp`**
- `agentctl/src/watch/mod.rs:execute_pending_spawn`: `tmpfile.keep()` makes the tempfile
  permanent. On every spawn, a `/tmp/<random>` agent config is left behind and never cleaned up.
  Single-tenant tool; no secrets in the file. Accepted in QA but noted for housekeeping.
- Fix: write the config to a deterministic path (e.g. `~/.agentos/last-spawn.toml`) and
  overwrite on each spawn; or pass the config via stdin/env rather than a file.

**p6.6-ar-02 (P3) — Template picker does not scroll to keep `template_idx` in view**
- `views.rs:render_spawn`: the picker renders a fixed window; if `template_idx` scrolls out
  of the visible region, the selected template is invisible.
- Fix: track a `template_scroll_offset` that follows `template_idx` (clamp to keep the selected
  row in the visible window). Deferred to next surface polish pass.

## Phase 0 — Technical Debt

**~~P2 — Sync I/O in native tool impls (p0.5)~~** ✓ Done in p2.5.
- `ReadFile`, `WriteFile`, `ListDir` migrated to `tokio::fs` (non-blocking).

**~~P2 — ToolRegistry::register should error on collision (p0.5)~~** ✓ Done in p0.5.

**~~P3 — Per-agent capability scoping for native file tools (p1.4)~~** ✓ Done in p1.4.

**~~P3 — FsRead/FsWrite enforcement assumes absolute paths (p1.4)~~** ✓ Documented in p4.5.
- Assumption documented in `Capability` enum doc comment: relative paths fail-safe
  to deny; `~` not expanded. Production target is Linux with absolute paths.

**~~P3 — Symlink traversal not blocked by capability prefix check (p1.4)~~** ✓ Documented in p4.5.
- Documented in `Capability` enum doc comment. Phase 4 namespace sandbox (p4.2)
  is the correct enforcement layer; IsolateMount/IsolateNetwork mitigate escalation paths.

**~~P3 — 2 MB binary target needs re-evaluation at p0.2~~** ✓ Done in p2.1.
- Switched to `rustls-tls`; static musl binary is 3.1 MB (vs ~1.4 MB macOS debug).
  Acceptable for Phase 2; a dedicated size-audit increment is p2.4.

**~~P3 — flight.jsonl CWD footgun for multi-agent (p1.2)~~** ✓ Resolved in p1.2.
- Resolution: single shared `flight.jsonl` + per-event `agent` field (CONVENTIONS.md invariant).
  All events emitted by `Scheduler::run()` carry the agent_id. Consumers filter by `agent` key.
- **P3 — stdout ordering for multi-agent answers**: answers are printed in completion order (fastest
  agent first), not in config declaration order. Fine for p1.2; a flag or ordered output mode
  may be desirable in a future increment.

**~~P3 — Net capability is advisory (p1.4 intentional)~~** ✓ Enforced in p4.2.
- `caps_to_rules()` now adds `IsolateNetwork` when `Net` is absent. Network isolation is
  enforced at the kernel level via `unshare(CLONE_NEWNET)` for all sandboxed MCP servers
  that don't declare a `Net` capability. `satisfies()` for `Net` remains advisory (no net
  tools exist yet), but the sandbox enforces it independently.

**~~P3 — Net enforcement via Landlock ABI v4 not yet wired (p3.3 deferred)~~** ✓ Done in p4.6.
- `AllowNetConnect { port: u16 }` added to `SandboxRule`; `Net { hosts, ports: Vec<u16> }`
  in `Capability` (`#[serde(default)]` for backward compat). `caps_to_rules()` generates
  `AllowNetConnect` rules from `Net.ports`. Runtime ABI detection: V4 (kernel ≥ 6.7) activates
  TCP port enforcement; older kernels degrade silently (BestEffort). Port-only (not host) is
  enforced at the kernel level — hostname restriction is advisory and remains in `hosts`.
  `EnforcementStatus.landlock_net` and `SandboxApplied enforced.landlock_net` field added.

**~~P3 — MCP server without `capabilities` runs unsandboxed with warn-only (p3.3)~~** ✓ Done in p4.1.
- `[tools] mcp_require_capabilities = true` flag added in p4.1. When set, startup fails
  if any MCP server has no effective sandbox rules. Default remains `false` for backward compat.

**P3 — DenySpawn does not block `clone()`/`clone3()` on x86_64 (p3.3 adversarial review)**
- seccomp filter blocks `fork(57)` and `vfork(58)` but not `clone(56)` or `clone3(435)`.
  A sandboxed MCP server can spawn child processes via `clone(SIGCHLD)`. Classic BPF cannot
  inspect `clone` flags to distinguish thread-create vs. process-create without SECCOMP_DATA_ARGS.
- **Accepted limitation.** `gVisor` (`isolation = "gvisor"`) fully mitigates this — the Sentry
  intercepts `clone3`. For namespace-only mode, this gap is documented in THREAT_MODEL.md.
  Switching to `SECCOMP_FILTER_FLAG_NEW_LISTENER` is deferred to Phase 5 if needed.

**~~P3 — DenySpawn is a no-op on aarch64 (p3.3 adversarial review)~~** ✓ Fixed in p4.5.
- `main.rs` now detects when all enforcement fields are false/none with non-empty rules
  and emits `SandboxSkipped { reason: "deny-spawn-unsupported-arch" }` instead of a
  misleading `SandboxApplied` with all-false fields.

**~~P3 — SandboxApplied fires even when Landlock degrades to no-op (p3.3 adversarial review)~~** ✓ Done in p4.1.
- `SandboxApplied` payload includes `enforced.landlock` and `enforced.seccomp` booleans
  since p4.1. Operators can inspect the event to see exactly what was active.

**~~P3 — `required_capability_for → None` tools are always visible (p1.4 design)~~** ✓ Documented in p4.5.
- Policy documented in `Tool::required_capability_for` doc comment: `None` tools are
  control-plane primitives (list_agents, send_message) intentionally visible under any
  cap-set, including deny-all. Future tools that should be suppressed must return `Some`.

**~~P3 — Case-sensitive path prefix matching on case-insensitive filesystems (p1.4)~~** ✓ Documented in p4.5.
- Assumption documented in `Capability` enum doc comment. Linux production target is
  case-sensitive; macOS is a dev-environment edge case, not a security gap.

**~~P3 — SchedulerState refactor (p1.5)~~** ✓ Done in p1.5.

**~~P3 — Shutdown drain re-enqueues to exited poll loop (p1.5 red-team)~~** ✓ Done in p1.6.
- Added `shutdown_requested: bool` to `SchedulerState`; `drain_deferred` now
  checks the flag and emits `agent_admission_denied { reason: "shutdown" }`
  instead of re-enqueueing onto the already-exited poll loop.

**~~P3 — EventKind enum in flight_recorder.rs → events.rs at p0.4~~** ✓ Done in p4.5.
- `EventKind` extracted to `agentd/src/events.rs`; re-exported from `flight_recorder`
  so all existing import paths (`agentd::flight_recorder::EventKind`) remain valid.

**~~P2 — MCP tools/list pagination not followed (p0.5 adversarial review)~~** ✓ Done in p2.5.
- `McpClient::spawn` now follows `nextCursor` in a loop until all pages are loaded.

**~~P2 — MCP graceful shutdown (p0.5 adversarial review)~~** ✓ Done in p2.5.
- `McpClient::shutdown()` sends `notifications/shutdown`, waits 5s for clean exit, escalates to SIGTERM, then SIGKILL.

**~~P2 — StopReason::MaxTokens produces empty Ok("") (pre-existing, p0.4)~~** ✓ Done in p2.5.
- `StopReason::MaxTokens` now emits `BudgetExceeded` flight event and returns `AgentEffect::Failed`.

**~~P3 — Buildroot ccache volume not wired (p2.2)~~** ✓ Done in p4.5.
- `BR2_CCACHE=y` and `BR2_CCACHE_DIR=$(HOME)/.buildroot-ccache` added to
  `distro/buildroot.config`. Subsequent clean builds use the host ccache (~2 min vs ~30 min).

**~~P3 — agentd flight log path is hard-coded to CWD (p2.2)~~** ✓ Done in p4.5.
- `--log-path <file>` CLI flag and `log_path` top-level TOML field added. Precedence:
  CLI > TOML > default `"flight.jsonl"`. In the VM `log_path` can be set to
  `/run/output/flight.jsonl` to make the destination explicit.

**~~P4 — `run_probe` ignores `--log-path` (p4.5 review)~~** ✓ Done in p4.6.
- `run_probe` now accepts `log_path: PathBuf`; call site passes
  `resolve_log_path(log_path_override, None)`; uses `FlightRecorder::new(&log_path)`.
  `--probe --log-path /path/to/flight.jsonl` now works correctly.

**~~P2 — Linux-gated code not verifiable on macOS dev machines (p3.1 lesson)~~** ✓ Mitigated in p3.1.
- `make clippy-linux` target added to workspace Makefile; `CLAUDE.md` quality gate updated.
  Required before pushing any branch touching `#[cfg(target_os = "linux")]` code.

**~~P3 — checkpoint.json has no access-control or encryption (p4.3)~~** ✓ Mode restriction done in p4.4.
- `checkpoint.json` is now created with mode 0600 via `write_mode_600()` in `checkpoint.rs`.
  `rename(2)` preserves those permissions on the final file. Test `save_sets_mode_0600` added.
- Encryption at rest remains a future item (THREAT_MODEL.md §3.3).

**P4 — `runsc do` is experimental; full OCI bundle integration deferred (p4.2)**
- `isolation = "gvisor"` wraps the MCP server command with `runsc do -- <cmd>`. The `do`
  subcommand is undocumented/experimental in gVisor and may not be stable across versions.
- Action: build a minimal OCI bundle (config.json + rootfs) on the fly via `runsc run` for
  production-grade gVisor integration. Deferred because `runsc do` suffices for p4.2 exploration.

**P4 — PID namespace via `unshare()` only affects future children (p4.2)**
- `unshare(CLONE_NEWPID)` in `pre_exec` makes the *calling* process's future children be in
  a new PID namespace, but the MCP server itself (after exec) remains in the parent PID namespace.
  To put the MCP server in a new PID namespace, a second fork is needed before exec.
- Action: implement double-fork in `McpClient::spawn` using a pipe to propagate the inner PID
  back to the parent. Deferred to a future Phase 4 increment.

**P4 — `clone3()` bypass remains in namespace-only sandbox path (p4.2)**
- `DenySpawn` seccomp blocks `fork(57)` + `vfork(58)` but not `clone(56)` or `clone3(435)`.
  Combined with `IsolateNetwork`, a sandboxed MCP server can still spawn children in the
  parent PID namespace via `clone3()`. `isolation = "gvisor"` fully fixes this (Sentry intercepts
  `clone3`); the namespace-only path does not.
- Action: accept the limitation for namespace-only mode; document in operator guidance.
  gVisor is the recommended mode for truly adversarial workloads.

**~~P3 — pre_exec sandbox errors are masked as EPERM (p4.1 red-team)~~** ✓ Done in p4.4.
- `mcp.rs::McpClient::spawn` now uses a pre-exec error pipe (`pipe2 + O_CLOEXEC`) on Linux.
  On pre_exec failure, the child writes "sandbox" to the pipe; the parent reads it and includes
  "(sandbox stage: 'sandbox')" in the error message. Previously all failures surfaced as EPERM.

**P4 — aarch64 CI runner needed to validate DenySpawn no-op behavior (p4.1 eng review)**
- The fix in T1 (gate BPF with `#[cfg(target_arch = "x86_64")]`) emits `SandboxSkipped` on
  non-x86_64 when DenySpawn is requested. There is no aarch64 GitHub Actions runner in CI
  to verify the behavior. Current `ubuntu-latest` runners are x86_64.
- Action: add a self-hosted aarch64 runner or QEMU-emulated job to CI when one becomes
  available; until then the logic is unit-tested but not E2E verified on real hardware.

**~~P4 — `sandbox_probe` fixture not wired to any integration test (p4.1 eng review)~~** ✓ Done in p4.4.
- 3 integration tests added in `tests/integration.rs` (Linux-gated): `allowed_path_read_succeeds`,
  `denied_path_read_fails`, `deny_spawn_blocks_exec` (x86_64-only). Tests spawn sandbox_probe
  directly with `pre_exec` + compiled sandbox rules; verify exit codes 0/1/non-0.

**~~P3 — No `--no-fuse` flag for CI and host dev environments (p3.1)~~** ✓ Done in p4.4.
- `--no-fuse` CLI flag and `AGENTOS_NO_FUSE` env var added to `main.rs`. When either is set,
  the FUSE mount is skipped with `tracing::info!` instead of attempted (no warning on CI).

## obs.3 — Open (deferred from obs.3)

**obs.3-ar-01 (P3) — `BatchSpanProcessor` internal 2048-slot queue drops uncounted**
- The `BatchSpanProcessor` maintains its own internal 2048-slot queue (separate from the
  10,000-slot `mpsc` channel). When spans fill this queue, the SDK silently drops them.
  This drop count is not exposed through any public API and is not reflected in
  `agentos.otel.spans_dropped` (channel-level) or `agentos.otel.export_drops` (flush-level).
- Mitigation: set `OTEL_BSP_MAX_QUEUE_SIZE` env var to a higher value (e.g. 8192) in
  high-throughput deployments. Full fix requires an SDK fork or wrapper — deferred.

## Resolved — obs.2-ar-01 and obs.2-ar-02 (closed in obs.3 / v0.44.0)

**~~obs.2-ar-01 (P3) — Copy-truncate rotation undetected when new file length ≥ old offset~~** ✓ Done in obs.3.
- Fixed via content sentinel: `FileTailer` stores `last_sentinel: Vec<u8>` (last 64 bytes at
  last-consumed offset). On poll, sentinel window is re-read and compared; mismatch → rotation.
  Three guards prevent false positives. Three new unit tests cover the fix.

**~~obs.2-ar-02 (P3) — OTLP backend-down export failures not surfaced in stats~~** ✓ Done in obs.3.
- Fixed via `export_drops: u64` counter + `spawn_blocking(move || p.force_flush())` in all three
  call sites (SIGTERM, SIGINT, periodic stats). New `agentos.otel.export_drops` OTLP metric
  (unit "failures") separate from channel-drop counter. Final stats line now emitted at shutdown.

## Completed

**cos.1 — Daily Operating Brief (Chief of Staff flagship)**
- `agentd/cos.agents.toml`: three-agent system (orchestrator + inbox + curator). Orchestrator:
  `max_turns=200_000`, `token_budget=5_000_000_000`, cron-triggered via `cron_mcp.py`. Inbox agent:
  read-only Gmail via `oauth_mcp.py` (no Spawn/FsWrite). Curator: Haiku model, KB-only.
- All 4 critical eng constraints resolved: max_turns set, budget set to 5B, child IDs date-stamped
  in orchestrator task prompt, `cos.1-eng-04` (mock testing) left as a deferred integration note.
- 3 new templates: `cos-orchestrator`, `cos-inbox`, `cos-curator`.
- `docs/RUNBOOK.md §11` with full first-run OAuth dance and 7 verification commands.
- Template test updated: 10 → 13 expected templates. All 503+ tests pass.

**p6.4 — Topology view (multi-agent graph)**
- `parent_id: Option<String>` on `AgentSnapshot`; insert-only `parent_map` in `SchedulerState` + checkpoint (`#[serde(default)]` for compat).
- `OFF_PARENT = 9`: new FUSE virtual file `/agents/<id>/parent`; `reader.rs` reads it into `AgentInfo.parent_id`.
- `agentctl/src/watch/topology.rs`: `TopologyGraph`, `build_graph()` (512 KB tail cap, directed `message_sent` edges, cycle guard), `render_tree()`, `status_badge()`, `parse_message_edges()`.
- `View::Topology` in `agentctl watch`; `[t]` key; `Esc`/`q` back to Dashboard; ↑/↓ scroll; fixed legend; min 60 cols guard.
- `--log-path` CLI arg; plain-mode topology section; `coordinator-demo.agents.toml` acceptance fixture.
- 455 tests pass; `make clippy-linux` required for surfaces changes.
- **Completed:** v0.30.0 (2026-06-18)

**p6.3 — Read-only TUI dashboard (`agentctl watch`)**
- `agentctl watch` command with three views: Dashboard (agent table), AgentDetail, System.
- `ratatui` 0.29 + `crossterm` 0.28; `--plain` / auto-TTY-detection for non-interactive use.
- `CleanupGuard` (`Drop` + `std::panic::set_hook`) for terminal restore on exit and panic.
- `surfaces/` FUSE amendments: `DIR_STEP` 10→20, `OFF_TOOLS = 8`, `/agents/system/` dir
  with four virtual files (`budget`, `queue`, `sandbox`, `provider`), `SchedulerSnapshot`
  + `AgentSnapshot` field additions; 24 new surfaces tests.
- Pre-landing review hardening (6 items): `is_tty` cached, cross-crate sentinel constants,
  stdout flush in plain mode, `spec_names()` cached as `&[String]`, ANSI sanitizer,
  `debug_assert` → `assert` in `alloc_dir`.
- 565 tests pass; `make clippy-linux` clean.
- **Completed:** v0.29.0 (2026-06-17)

**p3.3 — Landlock LSM + seccomp-bpf sandbox for MCP server subprocesses**
- `sandbox/` crate: `SandboxRule` enum (`AllowFsRead`, `AllowFsWrite`, `DenySpawn`);
  `CompiledSandbox` / `compile()` / `apply_compiled()` / `apply_sandbox()` API.
- Landlock V1 FS rules via raw syscalls (444/445/446); `ACCESS_FS_HANDLED = 0x1FFE`
  (excludes Execute bit to allow initial exec of MCP binary).
- seccomp-bpf filter blocks `fork(57)` + `vfork(58)` on x86_64 only; classic BPF.
- `caps_to_rules()` in `main.rs`: converts agent `Capability` set to `SandboxRule` list.
- `SandboxApplied` / `SandboxSkipped` flight events; `O_NOFOLLOW` on Landlock path fds.
- `CONFIG_SECCOMP=y / CONFIG_SECCOMP_FILTER=y` in `distro/kernel-extras.config`.
- 180 tests pass; Linux-gated tests deferred to CI via `#[cfg(target_os = "linux")]`.
- **Completed:** v0.10.0 (2026-06-11)

**p3.1 — /agents FUSE virtual filesystem**
- `surfaces/` crate: `SchedulerSnapshot` / `AgentSnapshot` / `AgentStatus` snapshot types.
- `AgentsFs` (`fuser` 0.14, Linux-only): root dir + per-agent dirs with `status`, `context_size`,
  `budget`, `flight` virtual files. Inode scheme: root=1, dirs from 1010 step 10, files at dir+1..4.
- `mount()` spawns FUSE thread; returns `FuseMounted` guard (RAII unmount); stubs on non-Linux.
- `Scheduler::new` gains 7th `Arc<RwLock<SchedulerSnapshot>>` arg; `update_snapshot` called after
  every scheduler effect; `AgentTask::context_tokens()` + `task_preview()` supply snapshot fields.
- New flight events: `FuseMounted`, `FuseUnmounted`.
- Workspace promoted: root `Cargo.toml` with `members = ["agentd", "surfaces"]`.
- `distro/kernel-extras.config` adds `CONFIG_FUSE_FS=y`; `distro/overlay/agents/` mount point.
- 188 tests pass (all platforms); negative FUSE read offset guard added post review-army.
- **Completed:** v0.9.0 (2026-06-10)

**p2.5 — Deferred cleanup (sync I/O, MCP pagination, MaxTokens, graceful shutdown)**
- Native tools (`ReadFile`, `WriteFile`, `ListDir`) migrated to `tokio::fs`.
- `McpClient::spawn` follows `nextCursor` in a loop until all pages loaded; capped at 100 pages.
- `StopReason::MaxTokens` now emits `BudgetExceeded` flight event and returns `AgentEffect::Failed`
  instead of silent `Ok("")`.
- `McpClient::shutdown()` sends `notifications/shutdown`, waits 5s, escalates to SIGTERM then SIGKILL.
- **Completed:** v0.8.0 (2026-06-09)

**p2.3 — Boot/supervision basics (SIGTERM/SIGINT handling)**
- `loop { tokio::select! { ... } }` in `Scheduler::run()` replaces `while let`.
- SIGTERM/SIGINT arms set `shutdown_requested = true` and break; deferred drain runs as before.
- `EventKind::SystemShutdownRequested` flight event emitted on signal.
- 1 new test: `sigterm_drains_scheduler` — sends SIGTERM, asserts < 5s exit + flight event.
- Essential mounts and zombie reaping required no code (handled by `/init` and tokio respectively).
- **Completed:** p2.3 (2026-06-09)

**p0.1 — Crate scaffold + config + flight recorder**
- Created `agentd/` binary crate with Config (TOML), FlightRecorder (append-only JSONL),
  EventKind enum, CI workflow, README, LICENSE.
- All acceptance criteria met: `cargo build` + `cargo clippy -D warnings` + `cargo test` pass.
- **Completed:** v0.1.0 (2026-06-07)

**p0.2 — Inference gateway + Anthropic backend**
- Added `InferenceGateway` trait, neutral message/tool types, `AnthropicGateway`
  (Anthropic Messages API), `--probe` smoke-test mode, 120s HTTP timeout.
- All acceptance criteria met.
- **Completed:** 2026-06-07

**p0.3 — Tool ABI + native tools**
- Added `Tool` trait, `ToolRegistry` (warn on collision, sorted specs), and three
  native tools: `read_file` (100k-char cap), `write_file` (mkdir-p), `list_dir`
  (sorted, `/`-suffixed dirs). `register_native(reg, &["all"])` wires them up.
  `tools_registered` flight event emitted at startup.
- All acceptance criteria met.
- **Completed:** 2026-06-07

**p0.4 — The agent loop (perceive → infer → act → observe)**
- `agent::run()`: full perceive → infer → act → observe loop with flight events.
  Token budget guard, max-turns guard, tool errors as `is_error` blocks.
- `main.rs`: stdin fallback for task, final answer on stdout.
- All Phase 0 flight events emitted.
- **Completed:** 2026-06-07

**p0.5 — Real MCP stdio client**
- `McpClient`: newline-delimited JSON-RPC 2.0 over tokio::process::Child (kill_on_drop).
  Handshake: initialize → notifications/initialized → tools/list. `tools/call` for invocation.
- `McpTool` implements `Tool`; `isError: true` → `anyhow` error.
- `ToolRegistry::register` now errors on collision (upgraded from warn).
- `echo-mcp` fixture binary + integration tests for MCP startup, coexistence, missing-server.
- Release binary: 1.4 MB on macOS.
- **Completed:** 2026-06-07

**p1.1 — Agent as a sans-IO state machine**
- `AgentTask` + `AgentEffect` (`#[must_use]`) + `step()` + `provide_inference()` + `provide_tool_results()`.
- Terminal guard on all `provide_*` and `step()` calls; MaxTurns fires before InferenceRequest.
- `agent/mod.rs` + `agent/driver.rs` split; driver is backward-compat shim.
- Unit tests: `step_machine_text_tool_text_cycle`, `max_turns_fires_before_infer_request`, `provide_inference_on_terminal_task_is_noop`.
- **Completed:** 2026-06-08

**p1.2 — The scheduler (multi-agent, cooperative)**
- `Scheduler` in `agentd/src/scheduler.rs`: `HashMap<String, AgentTask>` + `FuturesUnordered` drive loop.
  `Scheduler::new()` validates duplicate IDs. `Scheduler::run()` owns all IO concurrently.
- `config.rs`: `[[agents]]` multi-agent form + `agent_configs()` + backward-compat `[agent]` single form.
- `run_tools_sequential` extracted as `pub(crate)` in `agent/mod.rs`, shared by driver and scheduler.
- `agents.toml`: example two-agent config.
- `main.rs`: uses Scheduler for all runs; exit non-zero if any agent fails; stdin fallback preserved for single form.
- 4 scheduler tests + 8 config tests. All 74 unit + 16 integration tests pass.
- **Completed:** 2026-06-08

**p1.3 — Metered scheduling & admission control**
- `SchedulerConfig` in `config.rs`: `global_token_budget` (u64) + `max_concurrent_inferences` (usize); wired into `Scheduler::new`.
- Per-agent `priority: u32` field (default 0); `BinaryHeap<DeferredInfer>` keyed by `(priority desc, seq asc)`.
- `enqueue_or_defer` / `drain_deferred` manage the admission lifecycle in `scheduler.rs`.
- Flight events: `agent_scheduled`, `agent_deferred`, `agent_admission_denied`.
- `in_flight` underflow guards promoted from `debug_assert!` to `assert!`.
- New config tests: `scheduler_config_explicit_values_parse`, `scheduler_config_defaults_to_unlimited`, `agent_priority_parses_from_toml`.
- **Completed:** v0.3.0 (2026-06-08)

**p1.4 — Capability system**
- `Capability` enum (`FsRead{prefix}`, `FsWrite{prefix}`, `Net{hosts}`, `Mcp{server,tools}`, `Spawn`).
- `normalize_path` + `satisfies` + `satisfies_type` in `capability.rs`.
- `Tool::required_capability_for` + enforcement at `ToolRegistry::invoke`.
- `filtered_specs(cap_set)` — per-agent model context filtering.
- `CapabilityDenied` flight event; `capability_denied` in `flight.jsonl`.
- `McpTool::server_name` for Mcp{} cap gating.
- 130 tests pass (unit + integration + MCP + MCP client).
- **Completed:** v0.4.0 (2026-06-08)

**p1.5 — Inter-agent spawn-await**
- `spawn_agent` tool: parent with `Spawn` cap creates a child agent; child runs to completion;
  result injected back into parent as a `ToolResult`. Sole-call guard enforced.
- `AgentEffect::SpawnAgent { call_id, config }` — intercepted by scheduler before `invoke()`.
- `SpawnConfig` in `config.rs`: `task` (required), `child_id`/`priority`/`token_budget` (optional).
- `SchedulerState` struct consolidates all mutable scheduler state (`agents`, `outcomes`, `pending`,
  `deferred`, `in_flight`, `tokens_spent`, `awaiting`, `child_seq`, `spawn_depths`, `max_spawn_depth`).
- `dispatch_spawn` / `handle_agent_terminal` in `scheduler.rs` manage the full spawn lifecycle.
- Spawn depth limit: `max_spawn_depth: u32` in `[scheduler]` TOML (default 4; 0 = disabled).
- `agent_child_result_delivered` flight event.
- `Capability::Spawn` `satisfies()` fix; `SchedulerConfig::Default` fix (max_spawn_depth was 0).
- `send_message` deferred to p1.6 (Agent Cards increment).
- 133 tests pass (unit + integration).
- **Completed:** v0.5.0 (2026-06-09)

**p2.1 — rustls + static musl binary**
- Switched `reqwest` from `native-tls` to `rustls-tls`; all 142 tests pass.
- Cross-compiled `x86_64-unknown-linux-musl` via `cross` (Docker); binary is `static-pie linked, stripped`, 3.1 MB.
- **Completed:** v0.7.0 (2026-06-09)

**p2.2 — Buildroot minimal rootfs**
- `distro/` external Buildroot tree: x86_64 musl + BusyBox, cpio.gz initramfs, `make build/run/test`.
- `/init` PID-1 sh: mounts proc/sys/9p shares, sources `agentos.env`, `exec`s agentd.
- Two virtio-9p mounts: `secrets0` (API key) + `output0` (flight.jsonl visible on host).
- `make test` boots with `-no-reboot`, checks flight.jsonl for `agent_completed` event.
- **Completed:** p2.2 (2026-06-09)

**p1.6 — Agent identity & Agent Cards (discovery)**
- `AgentCard { id, name, description, skills }` derived from `AgentConfig` at scheduler seed; `agent_card_registered` flight event.
- `AgentConfig` gains `name`, `description`, `skills` optional TOML fields.
- `bus.rs`: `MailMessage` + `Mailboxes`.
- `list_agents` tool: sorted JSON array of all AgentCards; no capability required.
- `send_message` tool + `AgentEffect::SendMessage`: sole-call; scheduler delivers to mailbox; synthesizes ToolResult; unknown recipient → `is_error` (no panic).
- Mailbox drain before each inference; `inject_messages` appends to last User message (no consecutive-User-message violation).
- Shutdown drain fix: `shutdown_requested` flag in `SchedulerState`.
- New flight events: `agent_card_registered`, `message_sent`, `message_received`.
- 142 tests pass (unit + integration).
- **Completed:** v0.6.0 (2026-06-09)

**p5.1 — Storage primitive (redb-backed MemoryStore)**
- `memory/` module: `MemoryStore` trait + `RedbStore` backend (`redb` 4.1.0).
- `kv_get` / `kv_set` native tools; `KbRead` / `KbWrite` capability gating.
- `MemoryStoreOpened`, `MemoryRead`, `MemoryWrite`, `MemoryError` flight events.
- `[memory]` TOML section with `enabled`, `path`, `table` fields; `memory.enabled` default false.
- `FORMAT_VERSION` stored as metadata; `try_open` with TOCTOU-noted lock; 0600 mode on db file.
- 304 tests pass.
- **Completed:** v0.18.0 (PR #26)

**p5.2 — Per-agent short-term memory + paging**
- `memory/context.rs`: `MemoryPressure` enum (`None`/`Soft`/`Hard`), `assess()`, `page_count()`, `page_turns()`.
- `MemItem { turn, role: Role, content_preview, blocks_json }` stored in `short_term: Vec<MemItem>` on `AgentTask`.
- Soft threshold 75% → edge-triggered `MemoryPressureAdvisory` (fires once on `None→Soft` transition, not every turn).
- Hard threshold 90% → `page_turns()` evicts oldest turn PAIRS (preserves alternating-role invariant); Hard+n=0 path emits advisory once on first entry.
- `last_pressure: MemoryPressure` runtime field on `AgentTask` for edge-triggering; not checkpointed (resets to None on restore, correct behavior).
- `FORMAT_VERSION` 1→2; `#[serde(default)] short_term` for backward compat; `to_checkpoint`/`from_checkpoint` updated.
- `MemoryPressureAdvisory` + `MemoryPaged` flight events; both documented in `docs/CONVENTIONS.md`.
- `content_preview` covers all three `Block` variants (`Text`, `ToolResult`, `ToolUse`); `debug_assert!` for alternating-role invariant.
- 322 tests pass (14 new unit tests covering all acceptance criteria).
- Deferred: p5.2-ar-01 (unbounded Vec → p5.6), p5.2-ar-02 (at_turn stamps → p5.3), p5.2-ar-03 (conservative eviction → p5.6).
- **Completed:** v0.19.0

**p5.3 — Per-agent long-term memory + checkpoint coexistence**
- `ToolContext` struct (`tools/mod.rs`): `{ agent_id, turn, task_fp }` injected into every `Tool::invoke`. `task_fp` = FNV-1a 64-bit hash of initial task text, 16 hex chars; recomputed on restore.
- `MemRemember` tool: `mem_remember { content, tags }` — stores JSON entry with provenance under `agent/{id}` namespace; nanosecond-timestamp key; 8 KiB limit. Returns `None` from `required_capability_for` (implicit self-grant).
- `MemRecall` tool: `mem_recall { query, limit }` — iterates `agent/{id}` namespace, substring match, newest-first. Default 10, max 50 results.
- `EventKind::MemoryDistilled` — emitted post-call by `ToolRegistry::invoke` for `mem_remember`.
- All existing `Tool::invoke` signatures updated to accept `ctx: &ToolContext`; test helpers updated in `native.rs`, `mod.rs`, `mcp_client.rs`, and `memory_integration.rs`.
- 331 tests pass (9 new; up from 322 in p5.2).
- **Completed:** v0.20.0

**p5.7 — FUSE `/agents/<id>/memory/` + `/agents/kb/`**
- `MemoryAccess` trait in `surfaces/src/lib.rs`: `list_namespaces`, `list_keys`, `get_entry`.
- `MemoryAccessBridge` newtype in `main.rs` (Linux-only) bridges `Arc<dyn MemoryStore>` → `Arc<dyn MemoryAccess>`.
- `AgentsFs` extended: `INO_KB=9`, `AGENT_NS_PREFIX`, `MAX_DIR_KEYS=100`, `MAX_SHORT_TERM_PREVIEWS=20`; dynamic inode pool (`DYNAMIC_INO_START=1_000_000`) for `LtFile`/`KbSeg`/`KbFile` inodes.
- FUSE lookup/readdir for `memory/`, `memory/short_term`, `memory/long_term/`, `memory/long_term/<key>`, `kb/`, `kb/<seg>/`, `kb/<seg>/<key>`.
- `AgentSnapshot::short_term_previews: Vec<String>` populated in `update_snapshot`.
- `MemoryStore::list_keys` added (key-only range scan, skips value deserialization).
- Correctness fixes: existence check before `alloc_dir`/`alloc_kb_seg` in lookup; single `get_entry` per LongTermDir/KbSegDir lookup; removed double RwLock in `OFF_SHORT_TERM`.
- Deferred: `list_namespaces` full-scan → p5.8 (NAMESPACES table).
- 445 tests pass (33 surfaces + 412 agentd).
- **Completed:** v0.24.0
