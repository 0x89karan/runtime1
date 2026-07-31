# AgentOS — Completed Increments

Detailed per-increment completion notes.
The roadmap (with acceptance criteria and what's next) lives in `docs/ROADMAP.md`.

> **Coverage gap, be honest about it:** this file is complete through **v0.88.0** and then
> jumps to **v0.115.0**. The 26 releases in between (v0.89.0–v0.114.0 — the AUDIT-v0.97
> remediation stack, the cap cluster, and the UX tail) were written up in `CHANGELOG.md`
> and summarised in `CLAUDE.md`'s "Current status", but never backfilled here. For anything
> in that range, `CHANGELOG.md` is the record. Backfilling is tracked as documentation debt.

---

**brief.1 complete (v0.117.0).** The morning brief's action items are addressable — each carries its
Gmail `threadId` and is one click from the thread — and the brief now survives its own size limit.
**Its headline claim is withdrawn:** handled items still reappear (`brief.2`). Plan:
`docs/plans/connectors-action-queue.md`; QA report: `.gstack/qa-reports/qa-report-agentos-cos-2026-07-29.md`.
- **The premise was wrong, and five of six review passes said so independently** (Codex: `Reject`).
  brief.1 assumed `open:{date}:{N}` keying caused the re-listing. Nothing reads those keys —
  `kb_search` is single-segment and no list/scan/prefix tool exists in either backend, so `open:*` is
  write-only by construction. The re-listing comes from `kb_search(segment='ops:briefs')` returning
  whole historical briefs, nothing deletes a resolved item, and neither job can observe resolution.
  Shipped as an instrument plus defect fixes, with criterion 1 explicitly OPEN.
- **The brief was over its own cap before this shipped.** 8 660 B at the prompt's documented maxima
  against a hard 8 192 B limit: write fails → curator finds no input → no brief that morning, no
  visible cause. The first fix mis-sized it too (against raw JSON; `kb_put` measures the JSON-escaped
  payload inside a provenance wrapper, ~600 B more) leaving 39 B of real margin. Now byte-stated caps,
  a shed-and-retry ladder with a guaranteed-fit floor, and a `⚠ Shortened to fit` line so a shortened
  brief is never mistaken for a complete one.
- **Three rounds, and each round's fixes were the next round's defect source.** /review found 9
  criticals in the original 30 lines; /qa found 2 in /review's fixes (driving a real `agentd` against a
  fake provider); /ship's fix-review round found 9 in those, five of them **mutation-proven false
  greens in guards written one round earlier** — a 4-line scanner window, parens counted inside string
  literals, an assertion satisfied by a comment, a regex grep that matched two unrelated sites, and a
  cap check covering 2 of 9 caps. All seven controls are now negative-controlled by mutation.
- **The security fix initially guarded the wrong field.** `thread_id` was locked to
  `^[0-9a-f]{1,20}$` while `subject`/`from`/`ask`/`summary` still reached markdown raw — a subject
  reading `Payment overdue [Pay now](https://evil.example)` needed no escape trick. Now entity-escaped
  by rule; `brief-03` re-rated P3 → P1, because a prompt rule is not enforcement.
- **All four prompt copies now move together** (Docker, QEMU production, both spawn templates). Only
  one had been updated. Widening the guards also surfaced that the shipped `cos-curator` template
  passed `value=` to `kb_put`, so its KB writes had persisted nothing since v0.77.0.
- **Prompt adherence is unverified and cannot be verified here** — no API key, no Docker, no OAuth
  token, and faking Gmail would mean disabling the broker's SSRF controls. The first real brief is the
  test. The one-week operator tally is the decision input for whether this track continues at all.

---

**ux.6a complete (v0.116.0).** De-claimed the receipt chain and closed the `evidence.jsonl` boot trap;
no UI. Split from ux.6 at the /autoplan premise gate (both CEO voices RESHAPE); `ux.6b` (signed action
ledger) deferred, named and specified. Closes `audit86-P2-4` and `audit-S5`, both filed against `run.1`,
which shipped without them.
- The chain could not say "no": `record_denied` had ZERO production callers, so a 100%-`allowed` log was a
  property of the code. Wiring only the HTTP proxy would not have fixed it (that proxy never starts in
  production, per its own in-code comment); the reachable denial is native scheduler admission.
- Denial receipts are edge-triggered — one per `(agent, reason)` episode, not per attempt — because
  `write_receipt` fsyncs under a mutex, so per-attempt receipting is write amplification against the
  boot-read file and, with rotation, an audit-eviction primitive. Deferral is not denial (ux.8′).
- `resume_chain` no longer reads the whole file at boot (measured 1 KiB → 0.14 s vs 30 MiB → 0.16 s), and
  rotation needed no format or verifier change: genesis anchoring only ever blocked in-place truncation.
- `THREAT_MODEL.md` §8.7.1 states the real limits — model calls only, the signer IS the audited party
  (self-attestation, not third-party evidence), and deletion/rotation seams are undetectable. The mv
  external gate date is now named (earlier of mv.3 or 2026-10-01).
- /review: 6 CRITICALs across three rounds — a boot panic (`hex_decode` byte-slicing a `&str` with
  `panic = "abort"` on PID 1), an unbounded `seq` from an unverified receipt, a rotation cascade that
  unlinked the live inode while returning Ok, a false-green test that never entered the code it claimed
  to guard, and one introduced BY the fix (a fallible unlink after the live rename). /qa: 9/9 against a
  real agentd. 1851 workspace tests.


**Phases 0–3 complete (p3.1 + p3.2 + p3.3 all landed).** `agentd/` is a
working Rust binary. Phases 0–2 built the full single/multi-agent loop, config,
flight recorder, Anthropic gateway, tool ABI, native tools, MCP stdio client,
cooperative scheduler, capability system, agent spawning, agent cards, rustls
static binary, Buildroot rootfs + QEMU boot, signal handling, MCP pagination,
and graceful shutdown.

Phase 3 (Surfaces + Sandbox):

- **p3.1** (done): `/agents` FUSE virtual filesystem — `surfaces/` crate;
  `AgentsFs` + `SchedulerSnapshot`; each running agent appears as a directory
  with `status`, `context_size`, `budget`, `flight` virtual files; inode scheme
  (root=1, dirs from 1010 step 10); `Arc<RwLock<SchedulerSnapshot>>` shared
  between scheduler and FUSE handler; `FuseMounted`/`FuseUnmounted` flight
  events; `fuser` dep Linux-only; `CONFIG_FUSE_FS=y` in kernel-extras.config;
  15 unit tests in `surfaces`.
- **p3.2** (done): Agent checkpoint / restore — `checkpoint.rs` with
  `CheckpointStore` (atomic tmp→rename), `AgentCheckpoint` + `SchedulerCheckpoint`
  serde types; `AgentTask::to_checkpoint`/`from_checkpoint`/`is_terminal`; periodic
  auto-checkpoint every N turns (`checkpoint_interval_turns`, default=1); SIGTERM
  checkpoint; corrupt checkpoint → rename to `.corrupt` + start fresh; full restore
  of awaiting map, mailboxes, `tokens_spent`, `child_seq`, `spawn_depths`;
  `AgentCheckpointed`/`AgentRestored` flight events; 175 unit tests pass.
- **p3.3** (done): Landlock LSM + seccomp-bpf sandbox — `sandbox/` crate;
  `SandboxRule` enum (`AllowFsRead`, `AllowFsWrite`, `DenySpawn`);
  `compile()` + `apply_compiled()` API; `capabilities` field on
  `[[tools.mcp_servers]]`; `caps_to_rules()` adapter in `main.rs`;
  `SandboxApplied`/`SandboxSkipped` flight events; `CONFIG_SECCOMP=y` in
  kernel-extras.config; 180 tests pass.

**Phases 0–4 complete (p4.7 landed). Phase 4 (Isolation & hardening) + pre-Phase-5 cleanup complete.**

**p5.1 complete (v0.18.0).** Storage primitive: redb 4.1.0-backed `MemoryStore`, `KbRead`/`KbWrite`
capabilities, `kv_get`/`kv_set` native tools, 4 new flight events, 304 tests.
**p5.2 complete (v0.19.0).** Per-agent short-term memory + paging: `memory/context.rs` with
`MemoryPressure`, `assess()`, `page_count()`, `page_turns()`; `MemItem` struct; soft/hard
thresholds (75%/90%); `short_term: Vec<MemItem>` on `AgentTask` + checkpoint;
`FORMAT_VERSION` 1→2; `MemoryPressureAdvisory` + `MemoryPaged` events; 322 tests.
**p5.3 complete (v0.20.0).** Per-agent long-term memory + checkpoint coexistence:
`ToolContext { agent_id, turn, task_fp }` injected into every `Tool::invoke`; `MemRemember`
(`mem_remember`) + `MemRecall` (`mem_recall`) tools under implicit self-grant; nanosecond
key, 8 KiB limit, provenance JSON; `task_fp` = FNV-1a 64-bit hash of initial task text;
`EventKind::MemoryDistilled`; cross-agent namespace isolation via `agent/{id}`; 336 tests.
**p5.3.5 complete.** Detachable memory volume (infra-only, no crate changes): `memory.redb`
moved to a persistent 9p virtfs mount (`memory0` → `~/.agentos-memory/` → `/run/memory`);
three-mount model (`secrets0`/`output0`/`memory0`); `store_path = "/run/memory/memory.redb"`
in the demo agent.toml; CI guard confirmed at 6 MB; stale "≤ 4 MB" doc refs fixed.
**p5.4 complete (v0.21.0).** Shared KB MVP — multi-agent segmented KB with three mutability
classes: `canon` (operator-seeded, deny agent writes), `log` (append-only, monotonic key),
`scratch` (last-writer-wins, version counter); `kb_put`/`kb_get` tools with `KbWrite`/`KbRead`
capability enforcement; `[[memory.segments]]` TOML config seeds classes at startup;
`MemoryStore` trait extended with `segment_class`/`set_segment_class`/`next_log_seq`/
`next_scratch_version` (atomic version counter, fixes TOCTOU); `config::SegmentClass` unified
with `memory::MutabilityClass`; `kv_set` canon/log bypass fixed; provenance stamped by runtime;
`memory_write`/`memory_read` events with `tier:4` + `class`; THREAT_MODEL §7.1/7.2; 376 tests.
**p5.5 complete (v0.22.0).** Retrieval as tool: `kb_search` with BM25-lite inverted index;
`INDEX` redb table (key=`"{ns}\x00{word}"`, value=JSON posting list); atomic ENTRIES+INDEX+META
writes; `doc_count:{namespace}` META key for IDF; 21-word stoplist tokenizer in `memory/index.rs`;
`MemoryStore::search()` + `SearchHit`; flat output with content/provenance expanded; `KbRead`-gated;
`kb_search` flight event; 390 tests.
**p5.6 complete (v0.23.0).** Eviction & summarization: `AGE` redb table (composite key → Unix
timestamp) written atomically with every put/append, removed on delete; `MemoryStore::evict()`
drops oldest entries beyond `max_entries`/`max_age` removing ENTRIES+INDEX+AGE+META in one txn;
`EvictedEntry` struct; `EventKind::MemoryEvicted`; `[memory]` config fields
`max_entries_per_segment`, `max_entry_age_days`, `distill_on_complete`; `Scheduler::with_distillation()`
builder enables post-run short-term→Tier-3 inference summarization (budget-bounded, off by default);
`docs/CONVENTIONS.md` row for `memory_evicted`; 406 tests.
**p5.7 complete (v0.24.0).** FUSE memory surface: `surfaces::MemoryAccess` trait (leaf-crate,
no circular dep); `MemoryStore::list_namespaces()` default-impl with `RedbStore` override;
`MemoryAccessBridge` in `main.rs` wraps `Arc<dyn MemoryStore>` → `Arc<dyn MemoryAccess>`;
`AgentSnapshot::short_term_previews` (≤20 items); new inode offsets +5/+6/+7 for
`memory/`/`memory/short_term`/`memory/long_term/`; fixed inode 9 for `kb/`; dynamic pool
≥1_000_000 for `memory/long_term/<key>`, `kb/<seg>/`, `kb/<seg>/<key>`; bounded key lists
(MAX_DIR_KEYS=100); `mount()` accepts `Option<Arc<dyn MemoryAccess>>`; 446 tests.
**p5.8 complete (v0.25.0).** Phase 5 hardening: OV-1 startup invariant (`memory.store_path`
must not fall inside any MCP server's FS sandbox prefix, checked via `normalize_path` +
`anyhow::ensure!`); `NAMESPACES` redb table for O(k) `list_namespaces()` with one-time backfill
on pre-p5.8 stores; `prune_dead_agent()` in `AgentsFs` (lazy inode-map cleanup in `readdir(Root)`
for terminated agents, all 6 maps); `getattr` ENOENT guard for `memory/` and `long_term/` dirs
when store is absent; `memory_distilled` row added to CONVENTIONS.md event table;
THREAT_MODEL.md §7 expanded to §7.1–7.6 (memory substrate threats); memory-demo `agents.toml`;
`event_taxonomy_completeness` test; 476 tests.
**p5.9 complete (v0.26.0).** Phase 5 audit remediation: paging keyed on retained-context estimate
(fixes re-paging every turn); quarantine only on confirmed corruption (timestamped `.corrupt`);
eviction floor wired to live write path; NAMESPACES counter reconciliation; `page_turns`
alternating-role invariant promoted to runtime `Err`; `spawn_agent`/`send_message` batch
rejection returns `is_error` and re-infers; `validate_child_id` rejects traversal separators;
operator segment seeding (`seed = [{ key, value }]`); root `.gitignore`; 476+ tests.
**p6.1 complete (v0.27.0).** Template schema + on-disk catalogue: `agentd::template` module
with `TemplateConfig`, `TemplateMeta`, `TemplateCapabilities`, `TemplateCard`, `TemplateResolver`,
`TemplateSource`, `TemplateEntry`; `to_agent_config()` lowers template to plain `Config`;
`templates/scout.template.toml` first catalogue entry; path-traversal rejection; name identity
check; `[capabilities]` deny-by-default; `TemplateResolver::from_env()` for `~/.agentos/templates/`;
`Config` + sub-structs gain `Serialize`+`Clone` unblocking p6.2; 22 tests.
**p6.2 complete (v0.28.0).** Operator CLI: `agentctl/` workspace crate; `[lib]` on `agentd`;
`TemplateCard.suggested_caps: Vec<Capability>`; `agentctl list-templates` (tab-aligned, user/repo);
`agentctl spawn <name> --task "..." [--cap-add ...] [--dry-run]`; `parse_cap_alias()` flat CLI
syntax → `Capability`; `cap_add_allowed_by_suggestion()` guard; `--force` bypass;
`ANTHROPIC_API_KEY` pre-exec check; `arg_required_else_help`; `tempfile` atomic write; sibling+PATH
agentd resolution; `--dry-run` provenance header; distro Makefile adds `/etc/agentd/templates/`; 18 new tests.
**p6.3 complete (v0.29.0).** Read-only TUI dashboard: `agentctl watch` with ratatui/crossterm;
Dashboard / AgentDetail / System views; `--plain` mode + auto-TTY detection; startup FUSE mount
validation; CleanupGuard for panic-safe terminal restore; per-agent `tools` virtual file;
`/agents/system/{budget,queue,sandbox,provider}` FUSE surface; `DIR_STEP` 10→20; `OFF_TOOLS=8`;
`SchedulerSnapshot.{queue_depth,provider_model,sandbox_applied}`; `AgentTask::spec_names()`;
agentctl CI guard bumped 4 MB → 6 MB; +32 new tests (24 surfaces + 8 agentctl reader). 503 tests total.
**p6.4 complete (v0.30.0).** Topology view: `parent_id: Option<String>` on `AgentSnapshot`;
insert-only `parent_map: HashMap<String,String>` in `SchedulerState` + checkpoint (`#[serde(default)]`
for compat); `OFF_PARENT = 9` — new FUSE virtual file `/agents/<id>/parent`; `reader.rs` reads it
into `AgentInfo.parent_id`; `agentctl/src/watch/topology.rs` with `TopologyGraph`, `build_graph()`
(512 KB tail cap, directed `message_sent` edges, cycle guard), `render_tree()`, `status_badge()`,
`parse_message_edges()`; `View::Topology` in `agentctl watch` (`[t]` key; `Esc`/`q` back to
Dashboard; ↑/↓/j/k scroll; fixed legend footer; min 60 cols guard); `--log-path` CLI flag for message
edge data; plain-mode topology section; `coordinator-demo.agents.toml` acceptance fixture; 455 tests.
**p6.5 complete (v0.31.0).** Memory view for `agentctl watch`: `agentctl/src/watch/memory.rs` new module
with `MemoryEntry`/`AgentMemory`/`KbSegment` data types; `read_agent_memory()`/`read_kb_segments()` FUSE
readers; `filter_entries()`/`filter_short_term()` client-side substring filters;
`MAX_DISPLAY_ENTRIES=20`/`MAX_SEARCH_ENTRIES=100`; `View::Memory` TUI with `[m]` key; true-tab pane model
(Short-term/Long-term/KB) with per-pane scroll; `[/]` search; KB always accessible without live agent;
`MemoryAbsence::Subsystem`/`Empty` graceful degradation; ns u64 → RFC3339 provenance via chrono;
`[log]`/`[scratch]`/`[canon]` class badges; plain-mode memory dump; 727 tests.
**p6.6 complete (v0.32.0).** Spawn view in `agentctl watch`: `View::Spawn` with template picker,
task field, capability toggles (deny-by-default, pre-checked from `suggested_caps`), generate
preview, and exec agentd; `PendingSpawn`/`SpawnViewState`/`SpawnFocus` structs; `execute_pending_spawn`
in `watch/mod.rs`; `agentctl/src/watch/spawn.rs`; cap revoke fix (disabled caps stripped from
lowered config); flush guard before rename (Linux SIGTERM checkpoint stability); 793 tests.
**p6.7 complete (v0.33.0).** Starter catalogue — 6 new templates (librarian, journaler, coordinator,
code-aware, watcher, memory-custodian) plus the existing scout (7 total); `gated_requires: Option<String>`
on `TemplateMeta` with pre-spawn warning in both CLI and TUI paths; `sample_tasks: Vec<String>` on
`TemplateEntry` with TUI pre-fill of `task_input`; 14 new catalogue tests; UTF-8-safe `showcases`
truncation in `agentctl list-templates`; 808 workspace tests.
**p6.8 complete (v0.34.0).** Sandbox-enforcement surface + flight-log inspector: `SandboxSummary` +
`ServerEnforcement` replacing `sandbox_applied: bool`; `/agents/<id>/sandbox` FUSE virtual file
(`OFF_SANDBOX=10`, 11th per-agent inode); `/agents/system/sandbox` expanded with full `servers[]` +
`degradations[]`; `accessible_server_names` on `AgentSnapshot`; `main.rs` builds `ServerEnforcement`
per MCP server + detects degradations; `agentctl` reader/views show per-server sandbox flags +
degradation warnings; `View::Inspector` (`[i]` key) with load-once flight-log tail, filter
(All/Errors/Sandbox/CapDenied), substring search, color-coded body; 840+ workspace tests.
**Phase 6 complete.**

**p7.1 complete (v0.35.0).** Streamable HTTP MCP transport (MCP spec 2025-03-26):
`McpBackend` trait unifies stdio (`McpClient`) and HTTP (`McpHttpClient`); `McpTool.client`
changed from `Arc<McpClient>` to `Arc<dyn McpBackend>`; `McpHttpClient` — single-POST JSON-RPC
with SSE state machine, `Mcp-Session-Id` header capture, `read_bounded_http_body()` (4 MB guard),
`parse_sse_stream()`, 30 s `MCP_TIMEOUT`, 100-page `tools/list` guard; `McpServerConfig` gains
`url: Option<String>` + `headers_env: HashMap<String,String>` with `is_http()` + `validate()`
(mutual-exclusion guard, `https://` required, embedded credentials rejected); header secrets
read from env at startup, never logged; transport dispatch in `main.rs`; HTTP servers skip sandbox
(externally isolated); `ServerEnforcement.transport` field exposed in FUSE + `agentctl watch`;
`mcp_http_connected` + `mcp_http_error` flight events; `docs/MCP_SERVERS.md` new file (Linear,
GitHub); `reqwest` `stream` feature + `tokio` `net` feature; security: notify() body drain bounded,
10 s connect_timeout, redirect following disabled; 862 workspace tests (up from 841).
**p7.2 complete (v0.36.0).** Streaming inference: `streaming: bool` on `ModelConfig` + `InferenceRequest`
(opt-in, default `false`); `InferenceGateway::infer_with_stream()` async trait method with default
fallback to `infer()`; `AnthropicGateway` override with `parse_sse_event()` + `parse_sse_stream()`
(CRLF-safe, 1 MB line cap, `text_delta` → channel, `input_json_delta` → tool accumulator, 4 MB
tool-input cap, empty `input_json` → `{}`); `make_infer_future()` scheduler helper shared by
`enqueue_or_defer` + `drain_deferred`; `tokio::join!(infer_fut, print_fut)` with async stdout,
`[agent-id]` prefix in multi-agent runs, BrokenPipe silenced; `Arc<Mutex<HashSet<String>>>
streamed_agents` on `Scheduler` for double-print suppression; `InferenceStreamStarted` +
`InferenceStreamCompleted` flight events; 889 workspace tests (up from 862).
**p7.3 complete (v0.37.0).** FUSE write control surface + live agent injection: `/agents/control`
write-only FUSE file; `ControlMsg` enum (`Inject { agent_id, text }` / `Kill { agent_id }`);
`control_rx: Option<Receiver<ControlMsg>>` on `Scheduler`; inject writes a User turn into the
live agent's `context` deque without consuming a tool call; `tokio::select!` arm drains
`control_rx` each tick; `agentctl inject <id> <text>` CLI subcommand + FUSE write path;
`OFF_CONTROL = 9` system file; `ControlInjected` + `ControlKilled` + `ControlWriteError` flight
events; 902 workspace tests (up from 889). Revisit: `agentctl spawn` CLI should detect running
agentd via `/agents/control` — filed as `p7.3-ar-01`.
**p7.4 complete (v0.38.0).** Approval gate: `request_approval` native tool; `/agents/approvals`
FUSE directory + per-approval JSON files; `approve` / `deny` write paths; `ApprovalStore` with
pending/decided maps; `PendingActionView` in `SchedulerSnapshot`; `agentctl watch` Approvals pane
(`[a]` key) with approve/deny key-bindings; `REQUEST_APPROVAL` / `APPROVAL_GRANTED` /
`APPROVAL_DENIED` flight events; 932 workspace tests (up from 902).
**p7.5 complete (v0.39.0).** Ed25519 signed action receipts: `EvidenceWriter` signs egress
allow/deny records in `evidence.jsonl`; `agentctl verify <evidence.jsonl> <egress-key.pub>`
validates the receipt chain (this said `<flight.jsonl>` — the wrong file, and missing the key
argument; corrected in ux.6a); `EgressBrokered` + `ActionReceiptEmitted` flight events; 945 workspace tests. NOTE:
boundary secret rewriting (`SecretRewriter`) was planned but NOT built — tool output is NOT
scanned for credential-shaped tokens, and no `BoundarySecretRedacted` event exists.
De-claimed in cred.3.1 (v0.61.0); tracked as cred.3-ar-S3 (P2) in TODOS.md.
**p7.5b complete (v0.40.0).** HTTP forwarding proxy (real key routing): `ProxyRegistry`
(`RwLock<HashMap<String, ProxyEntry>>`); `ProxyPolicy { allowed_hosts, token_budget_remaining }`;
`start_http_proxy()` binds hyper v1 listener + routes requests via `handle_proxy_request()`;
ephemeral key identity via `x-api-key` header; real `ANTHROPIC_API_KEY` stored in `EgressProxy`,
never in `ProxyEntry`; loopback-only bind; adversarial hardening (forced content-type, loopback
assert, budget pre-check); `egress_brokered` + `egress_rejected` flight events; 968 workspace tests.
**p7.6 complete (v0.41.0).** Isolation floor — gVisor/runsc universal-tier agent spawning:
`agentd/src/universal.rs` (new) with `UniversalAgent::spawn()` (`env_clear()` + allowlist + ephemeral
key injection), `kill()` (SIGTERM → 5 s → SIGKILL), `which_runsc()`; `AgentTier { Native, Universal }`
+ 5 new `AgentConfig` fields (all `#[serde(default)]`); `universal_agents: HashMap<String, UniversalAgent>`
in `SchedulerState`; per-agent ephemeral key registered/deregistered in `ProxyRegistry`; `poll_universal_agents()`
enforces `max_wall_seconds`; `dispatch_send_message` guard returns `is_error` for universal recipients;
`build_scheduler_checkpoint` skips universal agents; FUSE `OFF_TIER=11` + `OFF_PID=12`; `agentctl watch`
shows `TIER: universal | ISO: gvisor | PID: <n>` badge; `langchain-worker.template.toml`; 3 new flight
event kinds; 978 workspace tests (up from 968).
**obs.1 complete (v0.42.0).** OTLP observability sidecar (`agentos-otel` crate): `SpanBuilder`
maps flight events → OpenTelemetry spans (run/agent/tool/inference hierarchy); `TokenCounter`
emits per-model token metrics via OTLP metrics API; `FileTailer` tails `flight.jsonl` with
copy-truncate rotation detection; `grpc` + `http/protobuf` OTLP export; credential guard
(`ANTHROPIC_API_KEY`-shaped tokens scrubbed from span attrs); saturating `parse_ts` preventing
u64 overflow; spans-dropped OTLP counter; 998 workspace tests.
**obs.2 complete (v0.43.0).** OTLP sidecar hardening — batch exporter, SIGTERM/SIGINT flush,
log rotation reset, validation unit tests: `BatchSpanProcessor::builder` + `BatchConfigBuilder`
replaces `with_simple_exporter` (`max_export_batch_size=512`, `max_export_timeout=30s`);
`OTEL_EXPORT_BATCH_DELAY_MS` env var (default 5000ms, min 100ms); SIGTERM + SIGINT handlers
drain open spans + call `provider.force_flush()` with error logging; `SpanBuilder::reset_for_rotation()`
drains + resets trace context (trace_id/run_id/agent_span_ids/span_counter) preventing phantom
cross-file span relationships; `flushed_on_rotation` counter in stats; 8 new validation unit
tests for `validate_log_path` / `validate_endpoint` (incl. world-writable + `%40` credential
bypass guards); 1006 workspace tests.
**obs.3 complete (v0.44.0).** OTLP sidecar gap remediation — content sentinel + export-drop
counting: `FileTailer` gains `last_sentinel: Vec<u8>` (64 bytes at last-consumed offset);
sentinel window re-read on poll when same inode + `cur_len >= offset`; mismatch → rotation;
three guards prevent false positives (small file, unpopulated sentinel, u64 underflow); 3 new
tail tests (fast-grow, no-FP, startup-FP); `export_drops: u64` tracked in main loop; all three
`force_flush()` call sites (SIGTERM, SIGINT, periodic stats) use
`tokio::task::spawn_blocking(move || p.force_flush()).await`; final stats line emitted at
shutdown; new `export_drops_counter` in `TokenCounter` (`agentos.otel.export_drops`, unit
"failures") separate from channel-drops counter; stats line: `exported=N open=M dropped=D
export_drops=E flushed_on_rotation=R`; obs.3-ar-01 (BSP internal queue uncounted) filed in
TODOS.md; 1009 workspace tests.
**h7.1 complete (v0.45.0).** Standard MCP servers: `docker/shell_mcp.py` (`run_command` tool,
`shell=True`, 30 s / 120 s timeout, 64 KB cap), `docker/http_mcp.py` (`fetch_url`, HTTPS-only,
4 MB cap, no redirects), `docker/search_mcp.py` (`web_search`, Brave API, graceful missing-key
message); `Capability::ShellExec` — subprocess sandbox capability that suppresses `DenySpawn`;
`agentctl spawn` `shell-exec` alias + `display_cap()` arm; templates updated: scout gains
`http_fetch`+`web_search` + 2 sample tasks, code-aware gains `shell_exec`+`http_fetch` (both
`isolation = "gvisor"`), librarian gains `http_fetch`+`web_search`+`net_ports`; `docs/MCP_SERVERS.md`
"Standard servers" section; `Makefile` `test-harness` target; 1025 workspace tests.
**h7.2 complete (v0.46.0).** Generic OAuth2 + PKCE MCP sidecar (`docker/oauth_mcp.py`): three
tools (`oauth_start_auth`, `oauth_check_auth`, `oauth_call_api`); PKCE RFC 7636 S256; CSRF state
nonce; SSRF dual-layer (allowlist + IP block); token file atomic-write (mode 0600); state machine
`idle→pending→authorized`; OAUTH_REFRESH_TOKEN bypass; `templates/google-agent.template.toml`;
`docs/MCP_SERVERS.md` oauth_mcp section; 1025 workspace tests.
**h7.3 complete (v0.47.0).** Event-trigger MCP servers — poll-and-retry design fits MCP_TIMEOUT=30s:
`docker/cron_mcp.py` (5-field cron UTC + interval; POSIX DOW; 5 self-tests),
`docker/fs_watch_mcp.py` (mtime+size+inode; fnmatch ignore; quiet-period debounce; 6 self-tests),
`docker/webhook_mcp.py` (ThreadingHTTPServer; Content-Length bomb guard; HMAC-SHA256; ±5 min
timestamp; 6 self-tests); `templates/cron-agent.template.toml` + `templates/webhook-agent.template.toml`
(new); `templates/watcher.template.toml` — `gated_requires` removed (now fully operational);
`Makefile test-harness` extended to all 6 servers; 2 new Rust template tests; 1027 workspace tests.
**h7.4 complete (v0.51.0).** Streaming-by-default + connect timeout: `ModelConfig.streaming`
default flipped to `true` via `fn default_streaming() -> bool { true }` +
`#[serde(default = "default_streaming")]`; `Default` impl updated; `connect_timeout(10s)` added
to `AnthropicGateway` reqwest client; 4 streaming-default tests (defaults_to_true, can_be_disabled,
can_be_enabled, default_impl_streaming_is_true); fixes Docker agent silent hang and google-agent
OAuth URL invisibility; 1030 workspace tests.
**dx.1 complete (v0.52.0).** Mac Docker DX: secrets model + `agentctl auth google`: PKCE
OAuth2 flow on Mac host writes `~/.agentos-secrets/google.json` (SHA256/base64url, atomic
write, mode 0600); `agentctl/src/auth/google.rs` with RFC test vector; `docker-compose.yml`
`cos` service gains `~/.agentos-secrets:/run/secrets:ro` volume bind + removes 7 static OAuth
env vars (now `ANTHROPIC_API_KEY`-only); `entrypoint.sh cos` preflight exits on missing
secrets with actionable message; `oauth_mcp.py` reads `/run/secrets/google.json` first with
env-var fallback + hardcoded Google URL defaults; `reqwest = {blocking}` + `sha2 = "0.10"` in
agentctl; 14 unit tests (PKCE primitives + RFC test vector + callback + secrets file); 7 pre-landing
fixes (redirect_uri timing, CSRF bail→Ok(None), HTML XSS escape, double lock-gap race, empty
refresh_token validation, dead code removal); 5 `exchange_code()` httpmock tests; 6 oauth_mcp
self-tests (21/21 — 5× `_is_ssrf_blocked()` + 401 auto-retry); 1062 workspace tests.
**p7.7 complete (v0.53.0).** Management HTTP API: `agentd/src/management.rs` loopback-only
server (`:7999`) with `GET /healthz`, `/api/v1/snapshot`, `/api/v1/approvals`,
`/api/v1/memory/:ns?limit=&offset=` (paginated, max 100), `/api/v1/events` (SSE fan-out);
`broadcast::Sender<String>` added to `FlightRecorder` via `with_broadcast()` consuming builder;
`ManagementConfig { enabled, port, bind_addr }` in `agentd/src/config.rs`; `Serialize` added to
`surfaces` snapshot types (`ServerEnforcement`, `SandboxSummary`, `PendingActionView`,
`SchedulerSnapshot`, manual impl on `AgentSnapshot` emitting `status` as flat string +
optional `status_detail` for tuple variants); `ManagementStarted`/`ManagementRequest` flight
events; `agentctl/src/watch/source.rs` — `DataSource` trait + `FuseSource` + `HttpSource`
(with JSON→`AgentInfo` mapping) + `detect_source()` auto-detection; `--url`/`AGENTCTL_URL`
flag on `agentctl watch`; `tokio-stream` (sync feature) + `async-stream` deps; 1081 workspace tests.
**dx.2 complete (v0.54.0).** HTTP approval surface (fail-closed): `POST /api/v1/approvals/:id/approve`
+ `POST /api/v1/approvals/:id/deny` on the management API; 503+`Retry-After: 1` on full channel;
404 on unknown ID; 400 on empty ID; `ApprovalHttpApproved`/`ApprovalHttpDenied` flight events;
`DataSource` trait extended with `load_approvals()`/`approve()`/`deny()` — `FuseSource` +
`HttpSource` both implement; `HttpSource.mutation_client` (500 ms timeout); FUSE control channel
always wired on Linux (removed `maybe_session` gate); optimistic TUI removal on `Ok(())`; plain-mode
`source.load_approvals()` fix; `AgentInfo.status_detail` parsed + shown in AgentDetail view;
`agentctl approve <id>` + `agentctl deny <id> [--reason ...]` CLI subcommands; resolved
p7.7-ar-01/02/04; 1096 workspace tests.
**ma.1 complete (v0.55.0).** aarch64 CI — `build-aarch64` CI job using `cross` + QEMU emulation
(`ubuntu-latest` + `taiki-e/install-action`); both `agentd` and `agentctl` build, clippy, and test
under QEMU for `aarch64-unknown-linux-musl`; per-binary size guard (≤ 6 MB, `if: always()`);
`Cross.toml` at repo root pinning `ghcr.io/cross-rs/aarch64-unknown-linux-musl:0.2.5` for `ring`
compat; `make clippy-aarch64` Makefile target + CLAUDE.md gate for `#[cfg(target_arch)]`-conditional
code; TODOS P4 closed.
**ma.2 complete (v0.57.0).** arm64 distro + HVF boot: `distro/buildroot.aarch64.config` (aarch64
target, `BR2_LINUX_KERNEL_IMAGE=y`); `distro/kernel-extras.aarch64.config` (adds
`CONFIG_VIRTIO_MMIO=y` + `CONFIG_SERIAL_AMBA_PL011_CONSOLE=y` for `ttyAMA0` console);
`distro/Makefile` parameterized by `ARCH` — `ARCH=aarch64` selects `qemu-system-aarch64 -M virt`,
`Image` kernel, `output/aarch64/` output dir, `aarch64-unknown-linux-musl` binary;
HVF/KVM/TCG accel auto-detected (macOS → HVF, Linux+KVM → KVM, fallback → TCG `-cpu cortex-a72`);
separate `build/output-$(ARCH)/` trees prevent x86_64/aarch64 clobber; `distro-aarch64` CI
dry-run job (`make -n build/run ARCH=aarch64`); `distro/README.md` Apple Silicon quickstart.
**ma.3 complete (v0.56.0).** Multi-arch Docker image: `publish-docker` CI job builds and pushes
`linux/amd64` + `linux/arm64` manifest to `ghcr.io/0x89karan/runtime1:latest` and
`ghcr.io/0x89karan/runtime1:v{semver}` on every push to `main`; gated on `build-and-test` +
`build-aarch64` + `audit`; `docker/setup-qemu-action@v3` + `docker/setup-buildx-action@v3` + GHA
layer cache; `provenance: false` for Docker client < 24.x compat; concurrency group prevents
parallel publishes.
**cred.3 complete (v0.60.0).** Credential broker as egress gateway: `agentd/src/credential/mod.rs`
(new, ~955 lines) with `CredentialGateway` (second OS-assigned loopback HTTP listener), `CredentialRegistry`
(ephemeral UUID4 token per MCP spawn, `tokio::sync::RwLock`), `OAuthTokenCache` (atomic state write,
token expiry buffer, `credential_refresh_failed` even on write failure); TOML-driven provider adapters
(`oauth-bearer`, `api-key-header`, `api-key-query`) via `[credential_gateway.providers.<name>]`; header
scrubbing (`Authorization`, `Host`, `X-Subscription-Token`, `header_name`) before upstream forward;
`Capability::Credential { provider: CredentialProvider }` + `CredentialProvider` enum (`Google`,
`BraveSearch`, `Custom`); `PASSENV_BLOCKLIST` extended (6 broker-managed vars); `McpClient::spawn()`
gains `credential_env` param (highest priority, collision warning); `ProxyRegistry` converted from
`std::sync::RwLock` to `tokio::sync::RwLock`; `docker/search_mcp.py` + `docker/oauth_mcp.py` migrated
to broker with legacy env-var fallback; `THREAT_MODEL.md` §8 (5 subsections); 5 new `EventKind` variants;
1112 workspace tests (up from 1096).
**cred.3.1 complete (v0.61.0).** Credential broker hardening gate — 10 security items closed:
`loopback_proxy.rs` shared `build_loopback_client()` (ar-10, drift guard compile error);
`is_ssrf_blocked()` + DNS resolution check on `upstream_base` at startup (ar-04); PASSTHROUGH_HEADERS
allow-list replacing SCRUB_HEADERS deny-list (ar-08); OAuthTokenCache `load_from_disk()` with expired-
token guard (ar-06); deny-by-default fast path for empty `allowed_providers` (ar-07); OV-1 FsRead
startup invariant for the Ed25519 signing key path (S1); removed `content_audited: true` lie from
`EgressBrokered` event (S2); de-claimed `SecretRewriter`/`BoundarySecretRedacted` from docs and
THREAT_MODEL (S3); THREAT_MODEL §8.6 (universal-tier has no credential path) + §8.7 (egress content
audit NOT implemented); ar-09 doc: cred.3.1 is prerequisite for cred.4/orch.1. Every gate item has
a test that fails without the fix. 7 new adversarial tests (T18–T24) + 5 follow-up SSRF fixes
(IPv4-mapped IPv6, fc00::/7, extract_host IPv6 literal, extract_host userinfo, empty-token
guard); T22 rewritten as live-gateway integration test; 1136 workspace tests total.
**cred.3.2 complete (v0.62.0).** Credential gateway hardening completion — security fixes + review-pass hardening:
`is_ssrf_blocked()` + `extract_host()` moved to `loopback_proxy.rs` as single source of truth (ar-10);
`GatewayState::new()` DNS-resolves each `upstream_base` and pins the IP via `reqwest::ClientBuilder::resolve()` —
DNS rebinding defended for process lifetime (ar-04/D2); empty DNS iterator (`NOERROR NODATA`) now warns instead
of silently bypassing SSRF check and IP pin (ADV-1/ADV-2); `get_or_refresh()` SSRF-checks `token_url` before
token endpoint POST (ar-04c); `upstream_resp.bytes().await` → `bytes_stream()` per-chunk cap (OOM fix / D14);
inbound query string always discarded (D3); `owning_agent_id()` helper with multi-agent "shared" sentinel
(ar-07 wiring fix — prevents all tokens being attributed to agent[0] in multi-agent mode);
`loopback_proxy::base_builder()` extracted so `GatewayState::new()` shares the canonical builder settings
(drift guard); self-referential source-scan assertions fixed in T28/T34/T35b; THREAT_MODEL §8.3 updated,
§8.7 ratified de-claims; RUNBOOK.md v0.62.0 + §11.11; 1159 workspace tests (21 new).
**cred.4b complete (v0.65.0).** Credential-agnostic MCP servers — completes ROADMAP cred.4 acceptance criterion
("tool process holds no raw credential in memory-at-rest"): `_load_config()` broker short-circuit skips secrets
file and credential env reads when `AGENTD_CREDENTIAL_GATEWAY_URL` is set, populating only routing config
(`OAUTH_PROVIDER_NAME`, `ALLOWED_HOSTS`); all three broker-path handlers (`handle_oauth_start_auth`,
`handle_oauth_check_auth`, `handle_oauth_call_api`) gate on `_BROKER_URL and _BROKER_TOKEN` (both required);
URL-only misconfiguration returns `broker_token_missing` error; `OAUTH_PROVIDER_NAME` validated against
`[a-zA-Z0-9_-]+` (path-traversal guard); `search_mcp.py` legacy `BRAVE_SEARCH_API_KEY` fallback emits
deprecation warning once per process; 6 new self-tests (T24–T29) in `oauth_mcp.py` (total 29); ROADMAP
cred.4 marked ✓; no Rust changes.
**orch.1 complete (v0.66.0).** Interactive agent orchestrator: `AgentStatus::Waiting` — new scheduler state
for orchestrated agents parked between turns, reflected in FUSE `/agents/<id>/status`, snapshot API, and
`agentctl watch` (shown as `⏸waiting`); `POST /api/v1/spawn` on the management API — spawn an agent from
JSON (`task`, optional `id`, `max_turns`, `orchestrated` flag); `POST /api/v1/agents/:id/inject` — inject
a user turn into a waiting agent over HTTP (400 on invalid input, 503+`Retry-After` on full channel);
`OrchestratorTurnComplete` SSE event fired when an orchestrated agent parks after completing a turn;
`OrchestratorDispatched` / `OrchestratorInjected` / `OrchestratorExited` flight events; `agentctl orchestrate`
REPL subcommand — spawn an orchestrated agent with an initial task, receive its answer, continue the
conversation across turns with a persistent SSE connection; `agentctl inject <id> <text>` subcommand (also
wired to HTTP inject path); `templates/orchestrator.template.toml` — new catalogue template (`max_turns=200`,
`token_budget=200000`, streaming enabled); `docker/entrypoint.sh orchestrate` mode — auto-detects a running
agentd via healthz, cold-starts one if absent, waits 15 s, then execs `agentctl orchestrate`; forwards
SIGTERM/SIGINT to agentd for graceful checkpoint on `docker stop`; `AGENTD_MANAGEMENT_ENABLED=true` env var
enables management HTTP API without editing TOML.
**ma.4 complete (v0.67.0).** Isolation-tier detection + honest per-device reporting:
`agentd/src/isolation_caps.rs` new module with `probe()` → `IsolationCapsSummary`; `sandbox::landlock_available()`
new public function (ABI ≥ 1 check); `detect_seccomp()` reads `/proc/sys/kernel/seccomp/actions_avail` on
x86_64 Linux only; tier taxonomy: `full` = runsc AND landlock AND seccomp, `capability` = any one or more
present (including runsc-only), `none` = none detected; `IsolationCapsSummary` struct on `SchedulerSnapshot` (Serialize-only, skip_serializing_if None);
`INO_SYS_ISOLATION = 18` FUSE virtual file `/agents/system/isolation`; `IsolationProbed` flight event at startup;
`SysIsolation` struct in `agentctl/src/watch/reader.rs`; `isolation_from_json` in `source.rs`; color-coded
isolation row in `agentctl watch` System view (green=full, yellow=capability, red=none) + legend; plain-mode
`isolation_tier:`/`isolation_arch:` lines; `ma.4-ar-01 (P3)` `require_isolation_tier` config key deferred to
TODOS.md; 1218 workspace tests.
**cred.5 complete (v0.68.0).** Credential control plane visibility: `CredentialSnapshot` + `ProviderHealth` types in `surfaces`; 5 new observability maps on `CredentialGateway`; `CredentialRegistry` converted to `std::sync::RwLock`; `OFF_CREDENTIALS=13` + `INO_SYS_CREDENTIALS=19` FUSE files; `GET /api/v1/credentials` management API; `agentctl watch [c]` credentials pane; `HttpSource` fetches `/api/v1/credentials`; 1244 workspace tests.
**orch.2 complete (v0.70.0).** Orchestrator hardening — closes 6 orch.1 ARs + 2 pre-conditions: FORMAT_VERSION 3→4 with `waiting_agents`/`orchestrated_agents` checkpoint fields; `from_checkpoint` restores actual `terminal` flag; seed loop guard prevents immediate deletion of restored waiting agents; `state.waiting` split into `orchestrated` (persistent membership) + `waiting` (currently parked); `handle_agent_terminal` consolidates removal from both sets (C2 phantom-entry fix); `OrchestratorTurnComplete.answer` capped at 512 chars; `POST /api/v1/spawn` returns 201 + `{"agent_id":"..."}` via oneshot confirmation (2 s timeout); 30 s SSE `": ping"` keepalive in management server; `agentctl orchestrate` quit/exit pause with resume hint + improved SSE timeout error message; 3 event-trigger templates gain `mcp` capability grant (audit-O1); `write_mode_600` + parent-dir `sync_all()` durability fix (audit-C3); 1253 workspace tests.

**dx.3 complete (v0.69.0).** Linux QEMU production path: `distro/buildroot.config` adds `BR2_PACKAGE_PYTHON3=y` + `BR2_PACKAGE_OPENSSL=y`; `distro/Makefile` refactors netdev out of shared `QEMU_FLAGS` into per-target lines, adds `RUN_NETDEV` with loopback `hostfwd:7999/8080` for `make run`, Python MCP overlay build step, `clean` fix; `distro/overlay/init` parses kernel cmdline `agentd.config=<path>` for config selection; `distro/overlay/etc/agentd/cos.agents.toml` (new) QEMU-mode CoS config with `bind_addr="0.0.0.0"` management, absolute MCP paths, `/run/memory` + `/run/output`; `agentd/cos.agents.toml` gains `[management] enabled=true`; `distro/agentos-cos.service` (new) systemd unit (`User=agentos`, loopback hostfwd, `ExecStartPre` mkdir, `-accel kvm`, 512 MB); `docs/DEPLOYMENT.md` (new) two-page operator guide with complete `agentos.env` template, SSH tunnel instructions, troubleshooting.

**ux.9 complete (v0.82.0).** Cockpit mode — `docker/entrypoint.sh`'s new `cockpit)` case is now the
Dockerfile's zero-arg `CMD` (was `shell`): cold-starts `agentd` from `/data` with a zero-agent config
(`docker/cockpit.toml`, needs the new `[scheduler] allow_empty_agents` opt-in + a relaxed
`Config::agent_configs()`) and attaches `agentctl watch` non-exec'd in the foreground; FUSE preferred
under `--privileged`, else `agentctl watch`'s existing `detect_source` falls back to the management
API over HTTP (`docker/cockpit.toml` sets `[management] enabled = true`). Two bugs found and fixed
during `/review`: checkpoint bleed-through (was running from the bind-mounted `/workspace`, silently
restoring a prior session's agents — now matches `cos)`/`agent)` and runs from `/data`, deleting any
stale `checkpoint.json` first) and terminal corruption on `docker stop`/`kill` (`agentctl watch` gained
its own SIGTERM/SIGINT handler, `agentctl/src/watch/mod.rs`, since a panic hook + `Drop` don't run on
an uncaught signal's default disposition). `make compose-config-check` guards that `docker-compose.yml`'s
`cos`/`agent` explicit `command:` lines keep overriding the image `CMD` regardless of what it is.
`THREAT_MODEL.md` §9.4 documents that the management API is now on-by-default for the primary
entrypoint (loopback-only per §9.1 — not a new vulnerability class, but higher likelihood of an
operator not realizing it's live). Full plan: `docs/plans/ux.9-cockpit-mode.md`; 9 adversarial-review
findings deferred to TODOS.md (`ux.9-ar-01`..`09`); next is ux.2 or the remaining cockpit track (ux.1/ux.3/ux.5/ux.8).
**cred.6 complete (v0.83.0).** CoS broker migration — `google_oauth` MCP sidecar holds no raw refresh
token in memory-at-rest; `OAUTH_CLIENT_SECRET` + `OAUTH_REFRESH_TOKEN` removed from passenv; `FsRead
/run/secrets` removed from `google_oauth` capabilities; `Credential{Google}` grant added to orchestrator;
`passthrough_query_params` allowlist field on `ProviderConfig` (default empty = D3 full-discard, CoS
configured with Gmail params); `state_path = "/run/memory/oauth/google.json"` in both Docker and QEMU
configs; `allow_non_loopback = true` explicit opt-in on `ManagementConfig` for 0.0.0.0 bind in Docker
compose mode; 1307 workspace tests. Manual gate (Step 4 of plan): live Gmail auth dance required after
deploy. `cred.6-ar-01` (P3 — URL-encoded `%26` in allowlist values, inert for Gmail) filed in TODOS.md.
**cred.7 complete (v0.84.0).** Credential resilience — 3-way failure classifier (`FailureClass`/`RecoveryKind`);
per-provider `ProviderHealthState` machine (`Healthy`/`AttentionRequired { recovery_kind, reason, since }`);
`CredentialError` struct replacing plain `String` in `get_or_refresh()` return type; `CredentialAttentionRequired`
+ `CredentialRecovered` flight events; proactive OAuth refresh background task per provider (`PROACTIVE_REFRESH_LEAD_SECS=300`);
`POST /api/v1/credentials/<provider>/reset-attention` management API (503 when disabled, 404 for unknown provider);
`ProviderHealth` gains `attention_reason`/`recovery_kind`/`attention_since` fields (skip_serializing_if None);
`ProviderHealthCheckpoint` serde type + `credential_health` field on `SchedulerCheckpoint` (`#[serde(default)]`
for backwards compat with v1–v4 checkpoints); early checkpoint peek in `main.rs` to restore health before gateway
start; `write_secrets_file_ext()` preserves custom `token_url` across re-auth; `sync_all()` durability on OAuth
state writes; `agentctl auth google --device` re-auth without `--force` when credentials read from existing file;
1327 workspace tests (+20).
**ux.2a complete (v0.85.0).** Attention — outcome/risk signals on the cockpit Dashboard: `AttentionReason`
enum (`ApprovalPending | Degraded | BudgetRisk | EvaluationUnavailable`, declaration order doubles as
tie-break/routing priority) + `AttentionSignal { reason, since, evidence }` struct (`surfaces/src/snapshot.rs`),
added to `AgentSnapshot` and served over both FUSE (`/agents/<id>/attention`, new `OFF_ATTENTION` offset) and
the management HTTP API (reused `Serialize` impl); `derive_attention()` (`agentd/src/scheduler.rs`) computes
all three signals from already-existing scheduler/credential state — no new instrumentation, no new flight
events. Dashboard gains an `ATTN` column, an always-visible "N need attention · M unavailable" summary line,
a stacked reason line per flagged agent, a persistent `AgentDetail` attention strip, and `--plain` markers;
actionability-driven Enter-key routing (`ApprovalPending` → Approvals, `Degraded` → Credentials, else →
AgentDetail) is a deliberately separate axis from severity-driven row color. Reframe of the original
"Observe" plan (`docs/plans/ux.2-observe.md`, preserved for reference) toward outcome/risk signals per CEO
dual-voice review — full review in `docs/plans/ux.2-attention-evidence.md`. Does **not** close `cos-ux-01`:
Idle/Error signals need new `AgentTask` fields that don't exist yet, deferred to **ux.2b**. Three rounds of
ship-review found and fixed 2 CRITICAL bugs (`since` rendered as raw epoch instead of elapsed time;
`EvaluationUnavailable` was dead code, never constructed, so a failed FUSE/HTTP read silently degraded to
"clean") plus a FUSE `lookup()` gap (no match arm for `"attention"`, mirroring a pre-existing `"credentials"`
gap fixed in the same pass) and a `--plain` output concatenation bug; 1377 workspace tests (+50).
**ux.1 complete (v0.86.0).** Converse — permanent chat rail on `agentctl watch`'s Dashboard view (agent
table `Min(72)` | rail `Length(32)`), honoring the project's locked D1 "one unified screen" decision instead
of a 10th full-screen tab as the rough scope originally proposed. `/autoplan` found and fixed two critical
issues before implementation: (1) this branch was cut ahead of `ux.2a-attention` in violation of the
roadmap's own sequencing — paused, merged ux.2a first, re-cut clean; (2) the plan's live-streaming premise
was false — three independent traces (manual + Claude subagent + Codex) confirmed `agentd` never actually
emitted per-token events onto the wire (`text_delta` existed only in the local-stdout print path inside
`scheduler.rs`'s `make_infer_future`) — closed by adding `EventKind::InferenceStreamDelta`, recorded
per-chunk via a new `FlightRecorder::record_streamed()` that broadcasts the full chunk live over SSE while
capping the `flight.jsonl` disk copy at 256 bytes (preserves the log's preview/audit-metadata contract).
`agentctl/src/watch/converse.rs` (new): `ConverseState` per-target state machine (Idle → Dispatching →
Streaming → flush) in a `HashMap<AgentId, ConverseState>` so a backgrounded conversation keeps streaming
while another is focused; `dispatch()` (spawn-or-resume) and the four terminal-event field-path lookups
ported byte-for-byte from `orchestrate.rs` rather than re-derived as "shared general knowledge" — Eng
review found the four kinds are NOT uniform (`orchestrator_exited`'s top-level `agent` field is a hardcoded
literal `"agentd"`, only `data.agent_id` is valid; `agent_failed` has no `data.agent_id` at all). `Tab`
toggles rail focus (reusing `Memory`/`Spawn`'s existing sub-pane-cycling idiom, including `Spawn`'s
`TaskField` Esc-capture idiom for the input box) via a `handle_dashboard_key` retrofit that reads focus
internally (mirrors `handle_spawn_key`'s exact shape — zero call-site changes for the ~15 pre-existing
tests); `r` retargets to the selected row. `[c]` stays bound to Credentials — the rough scope's original
`[c]`-for-chat proposal collided with the already-shipped Credentials hotkey (cred.5, v0.68.0), caught
during Design review alongside a corrected minimum-rail-width floor (95→115 cols, arithmetic from the
table's real column constraints). `agentctl orchestrate`'s CLI does NOT share converse.rs's helpers (kept
its own duplicated spawn/inject + field-path logic) and does NOT consume the delta stream — it still
block-then-prints the server-capped 512-char `answer`, unchanged from before this branch. The plan called
for both; `/ship`'s Step 8 plan-completion audit caught that neither landed and that this note (and
CHANGELOG.md) had claimed otherwise — corrected here, follow-up filed as TODOs.
docs/INTERFACE.md §3 annotated as superseded-by-shipped-implementation. `/review`'s adversarial pass
(Codex + Claude subagent) then found and fixed 10 more real bugs post-implementation, including one
critical panic (byte-index truncation at the 64KB cap could slice mid-UTF8-character and crash the
whole TUI) and a logic bug where the 30s dispatch timeout was measured from dispatch start rather than
last activity, killing any turn that streamed longer than 30s total — see CHANGELOG.md for the full
list. Two remaining architectural findings (blocking dispatch can freeze the TUI ~8s worst case; the
shared SSE broadcast channel now carries much higher-frequency traffic) are bounded, not correctness
bugs, and filed as TODOs rather than fixed in this pass. Interactive QA against the real binaries then
caught a critical bug the whole pipeline above missed: typing `q` into the focused chat rail quit the
entire TUI (`step_key`'s outer quit-check didn't know about rail focus). `/ship`'s own Step 9-11 review
pipeline (coverage audit, specialist review army, red team, cross-model adversarial pass) then found and
fixed 6 more real bugs, including one as severe as anything above: the chat rail was silently, completely
non-functional whenever `agentctl watch` runs over FUSE instead of `--url` HTTP (the default local mode on
AgentOS's own target Linux platform) — `FuseSource` supports neither `spawn()` nor `event_stream_url()`,
so every message hung at "Dispatching..." for 30s, forever, since this session's QA only exercised `--url`
mode. Also fixed: the 64KB reply cap didn't actually cap anything past the first overflow (unbounded
marker-repeat growth); turn completion was discarding the full streamed text and using the server's
512-char preview instead; resize below the rail's fit floor left it invisibly focused; the scroll offset
counted logical turns instead of wrapped visual rows. See CHANGELOG.md and TODOS.md's ux.1 section for
the complete list, including further findings filed as TODOs rather than fixed under ship-time pressure.
Full plan + dual-voice review trail: `docs/plans/ux.1-converse.md`; 1420 workspace tests (+52).

**audit.1 complete (v0.87.0).** P0 hotfix + guard batch from the v0.86.2 full-system audit
(`docs/plans/audit.1-p0-hotfix-guards.md`): the default QEMU boot config could never boot —
`distro/overlay/etc/agentd/agent.toml` used `model_id`, a key that never existed on
`ModelConfig` (`deny_unknown_fields`), so every non-CoS QEMU boot panicked PID-1 at config
parse; renamed to `model`, and `agentd/tests/config_parse_all.rs` now parse+validate+
lowering-proves every checked-in agent-spec TOML (docker/, agentd/, distro overlay) with
negative-control fixtures. `agentd/tests/repo_consistency.rs` test-enforces CLAUDE.md's
`**Current version:**` line against `agentd/Cargo.toml` (closes cred.3.2-ar-02 after three
drift recurrences) and requires template `gated_requires` env vars to exist in product
sources — closing the librarian-semantic class where the template's badge/spawn warning
named `VOYAGE_API_KEY` while its sidecar reads `OPENAI_API_KEY`. Boot guards: the
`cos)`/`agent)` sed pipeline gained line-anchored negative assertions (both quote styles,
positive-form path-key check, args anchoring) that refuse to boot naming the surviving
line, verified credential-free via `DRY_RUN_ONLY=1`, with an `AGENTOS_SKIP_PATH_GUARDS=1`
escape hatch; adversarial hardening ensured guards never match over user task text, guard
grep errors fail the boot instead of passing, and behavior flags can't be injected via the
secrets file. CHANGELOG reordered newest-first; six long-fixed TODOS struck; ux.8 moved
ahead of ux.10 at the review gate. 1428+ workspace tests.

**ci.1 complete (v0.88.0).** CI tests the artifact, not just the source
(`docs/plans/ci.1-ci-tests-the-artifact.md`). `build-and-test` goes workspace-wide
(`cargo build/clippy/test --workspace --all-targets` from the repo root): `surfaces`
(96 tests incl. Linux-gated FUSE glue), `sandbox` (35), and `otel` (34) build, lint, and
test in CI for the first time, with FUSE headers preinstalled, root-workspace caches that
actually hit, and the sandbox crate added to both aarch64 clippy lanes (`make
clippy-aarch64` + CI) — closes audit86-P1-4. New `docker-smoke` job builds the real Docker
image on every PR and boots it four ways: a credential-free CoS dry-run, an agent dry-run
that must render the requested template, a binary error probe, and a negative-control
fixture (`.github/fixtures/cos-broken-relative.toml`) that must *refuse* to boot with the
offending line named (`17:store_path`) — the PR-#124 "relative path survives the boot
rewrite" class can no longer land silently (closes the audit86-P1-5 remainder). Sidecar
self-test contract: all nine `docker/*_mcp.py` servers must exit 0 AND print their
`self-test PASSED` marker (either alone can lie); enforced by the `sidecar-tests` CI job
and mirrored by `make test-harness`, both globbing the directory; `weather_mcp.py` gained
its missing `--test` mode. Fail-closed release guards (`scripts/release-guard.sh`, closes
audit86-P1-6): a tag must be on `main` (ancestry), match `agentd/Cargo.toml`, exceed every
prior `v*` tag, and target an unpublished version — probed per-caller (`--check release`
in release.yml, `--check image` in ci.yml) across all three version manifests, fail-closed
(auth/network errors refuse rather than pass), with a serialized pre-push re-check closing
the concurrent-publish race; a 24-scenario harness (`scripts/test-release-guard.sh`) runs
in the `harness-tests` job on every push, and tag publishes now wait for the sidecar and
harness jobs. Nightly artifact E2E (`.github/workflows/nightly-e2e.yml`, 03:17 UTC): a
real agent cycle with a tool call against a stateful mock provider
(`.github/fixtures/mock_provider.py` — dispatches on request content, self-tests in CI,
refuses wrong endpoints) at zero API cost. The QEMU 2-boot continuity test moves from
manual-only (red for months without anyone noticing) to a monthly cron with a preflight
that names the missing secret and QEMU stderr capture. `distro/overlay/init` mirrors the
entrypoint's env denylist (`GREP_OPTIONS`/`POSIXLY_CORRECT`/guard-bypass filtering).
Broker→oauth_mcp→provider fake-provider E2E deferred as named `ci.2` (TODOS.md), replacing
the plan's "after smoke proves stable" never-trigger. `docs/DEPLOYMENT.md` documents every
guard refusal with remediation, the required-status-check setup (`build-and-test`,
`docker-smoke`, `sidecar-tests`, `harness-tests`), and the release operating rules (linear
versioning, tag spacing, safe re-run paths). 1430 workspace tests.


**ux.13-TUI complete (v0.115.0).** Row-scoped control verbs in `agentctl watch` — the ux.3b reshape
(`docs/plans/ux.13-tui-verbs.md`; the `:` command palette was STRUCK at the /autoplan CEO gate on 6/6
adverse consensus and the overlay half redirected here). ux.13 (v0.97.0) shipped `Cancel`/`SetBudget`/
`SetCaps` across the management API, FUSE control, and the CLI but left `docs/ROADMAP.md` recording
"**TUI keys deferred**", so the operator could not stop a runaway agent from the screen showing it to
them. `[x]` on a Dashboard row now opens a graded row-action overlay (`agentctl/src/watch/overlay.rs`,
new module; `App.pending_verb` + `drain_pending_verb` in `watch/app.rs`).
- **Park** = `set_budget` at the spend already recorded, double-gated: `park_limit()` returns `None`
  below a 1 000-token floor (`0` ≡ UNLIMITED and `set_token_budget` writes the CHECKPOINTED config, so
  parking a zero-spend agent would have un-capped it permanently across restart — in Park's primary use
  case), and `park_would_widen()` refuses when the recorded spend EXCEEDS the current cap (the normal
  post-exhaustion state, where capping at spend RAISES the ceiling). Its label states which of two
  things it is, because neither is a reversible pause: with `budget_reset_interval > 0` the park expires
  by itself at the next rollover (`maybe_roll_budget_window` rebases windowed spend, `drain_deferred`
  re-admits); with no window, exhaustion ends the agent.
- **Set budget** = numeric field prefilled with the current limit, `0 = unlimited` stated on the field,
  a second gate for any removal or raise, and a typo rejected in place rather than parsed as `0`.
- **Cancel** = its own confirm showing "at least N" from a cycle-safe `descendants()` walk
  (`watch/topology.rs`) and reporting the SERVER's count (native subtree + universal agents parented
  in); `agentctl cancel` prints the same count in the same words. `DataSource::cancel` became
  `Result<u64, String>`.
- **`SetCaps` stayed CLI-only** — no snapshot data stands behind it (absent from `AgentSnapshot`,
  `AgentInfo`, and the FUSE surface) and `SetCaps` REPLACES the whole set, so revoking one cap means
  transmitting all the others. Its own increment when someone asks.
- **Verbs are performed by the event loop, never the key handler.** `HttpSource`'s confirm client blocks
  up to 3 s, so the confirm keypress writes `App.pending_verb` + an `InFlight` frame and returns; the
  loop draws, then `drain_pending_verb` makes the call. `handle_overlay_key` takes no `source` argument,
  making that a compile-time property. Measured on a real pty: frame on the wire 0.02 s after Enter
  against a 2.5 s server. Keystrokes buffered during the call are discarded (Resize applied, Ctrl-C
  honoured) so two impatient presses cannot dismiss the result and then quit the cockpit. The chat
  rail's `converse::dispatch` moved onto the same slot.
- **New honesty methods on `DataSource`** (`watch/source.rs`): `confirms_mutations()` (false on
  `FuseSource` — a FUSE write is fire-and-forget, so past-tense copy becomes "cannot confirm the
  scheduler accepted it"), `supports_auto_approve_kind()` (true on FUSE only, since `auto_approve_kind`
  rides the `approve` control command and no HTTP route carries it — `[d]` stopped claiming a standing
  rule over `--url`), and `cli_connection_flags()` (every printed `Equivalent: agentctl …` carries the
  flags that reach THIS daemon, because `run_cancel`/`run_set_budget` re-resolve the source from scratch).
- **`budget_resettable: bool`** on `SchedulerSnapshot` (`surfaces/src/snapshot.rs`), `GET
  /api/v1/snapshot`, and FUSE `system/budget` (now `{"spent":N,"total":0,"resettable":BOOL}`), set from
  `init_budget_window` in `scheduler.rs`. `agentctl` reads either wire name and defaults to `false` —
  the cautious reading and the config default.
- **`?` — the first help key this cockpit ever had**, rendering from the same `DASHBOARD_KEYS` table as
  the footer so the two cannot drift, and documenting the keys the footer has no room for
  (`Ctrl-c`, `Esc`).
- **A client-side `cancelling…` row marker**, since no `AgentStatus::Cancelling` exists and a cancelled
  row otherwise reads `running` for a turn and then presents as a bare red `failed`. Escalates to
  `NOT CANCELLED` for anything the source could not confirm, settles to `cancelled by you` on
  confirmation.
- **/review fixes:** the Approvals confirm dialog rendered by INDEX while acting on a pinned id
  (`update_approvals` replaces the list in Confirm mode and clamps only the index, so an approval
  resolving out of band could show one item's id/kind/**risk**/summary while `[a]` approved another) —
  one resolver, `App::confirm_item`, now serves the renderer and all three write paths; the Dashboard
  footer clipped its own resize hint (measured 162 columns with `[l]ogs` present) — now bounded per
  state and asserted against a rendered 80x24 frame; agent ids that would re-point a mutation URL
  (`/ \ ? # %`, control characters) refused at the `DataSource` boundary and every id in a dialog or
  printed command sanitized and shell-quoted.
- **/qa fixes:** a cancelled row read `failed` with nothing saying the operator did it, and
  `agentctl cancel|set-budget|set-caps` printed raw HTTP errors while the cockpit explained them — both
  now share `explain_verb_error` in surface-neutral wording.
- 1816 workspace tests (+103 over v0.114.0's 1 713).
