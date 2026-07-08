# AgentOS / agentd — Project Memory

You are working on **AgentOS**: a Linux-based operating system where **agents are
the primitive, not applications**, designed to be **super light**. `agentd` is its
runtime. In the full system this process *is* the userspace (PID 1 / the boot
target); today it runs as an ordinary binary on a normal distro.

Read `docs/DESIGN.md` for the full thesis, architecture, and rationale.
Read `docs/ROADMAP.md` for the build plan — **this is the work queue.**
Read `docs/CONVENTIONS.md` before adding a subsystem, tool, or provider.

## Locked decisions — constitutional, do not drift

These were decided deliberately. Do not relitigate or quietly violate them:

1. **Cognition is remote.** The device is a thin agent host. The model is an API
   call behind `InferenceGateway`. There are **no local model weights** and no
   local inference engine. (Adding a *local backend* later is allowed only as a
   new `impl InferenceGateway`, never as a core assumption.)
2. **Single-tenant.** This is an OS for one individual. Agents are mutually
   trusting and run **in-process**. Do not add multi-user isolation, per-user
   auth, or tenancy boundaries. (Capability *scoping between agents* is in scope
   — see the roadmap — but that is about least-privilege, not distrust.)

## Current status

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
allow/deny records in `evidence.jsonl`; `agentctl verify <flight.jsonl>` validates the receipt
chain; `EgressBrokered` + `ActionReceiptEmitted` flight events; 945 workspace tests. NOTE:
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

## How to work here

- **Work the roadmap in order.** Each increment in `docs/ROADMAP.md` is a small,
  self-contained unit of work with explicit dependencies and acceptance criteria.
  Implement exactly one per branch; do not bundle several together. `main` stays
  shippable at every step. The roadmap's "How to use this with gstack" section
  describes the per-increment loop (`/plan-eng-review` or `/autoplan` → build →
  `/review` → `/qa` → `/ship`).
- **Preserve behavior across refactors.** Phase 1 begins by refactoring the loop
  into a steppable state machine; the single-agent path must keep working
  identically (the flight-recorder output for the demo should not regress).
- **Build, lint, and test before every commit:** `cargo build && cargo clippy --
  -D warnings && cargo test`. Do not commit code that does not compile or that
  has clippy warnings.
- **Linux-gated code requires a Linux clippy pass before pushing.** Any code
  under `#[cfg(target_os = "linux")]` (e.g. `surfaces/src/agents_fs.rs`) is
  never compiled on macOS, so local clippy is a false green. Run
  `make clippy-linux` from the repo root (requires Docker) before pushing a
  branch that touches Linux-gated code. This mirrors the CI step exactly.
- **aarch64-gated code requires an aarch64 clippy pass before pushing.** Any code
  under `#[cfg(target_arch = "x86_64")]` or `#[cfg(not(target_arch = "x86_64"))]`
  (e.g. `sandbox/src/lib.rs` DenySpawn gate) has different behavior on aarch64.
  Run `make clippy-aarch64` from the repo root (requires Docker and `cross` installed
  via `cargo install cross --locked`) before pushing a branch that changes
  arch-conditional behavior. `Cross.toml` at the repo root pins the Docker image
  version so `ring`'s `build.rs` gets the correct `aarch64-linux-musl-gcc`.
- **Match the existing style.** Small modules, narrow traits, minimal
  dependencies. This is meant to be a *light* runtime — justify every new crate.
- Update `docs/ROADMAP.md` (check off the increment) and any affected doc in the
  same PR as the code.

## Invariants you must preserve

- **Record everything.** Every meaningful step an agent takes emits a structured
  flight-recorder event. New behavior gets new event kinds (see the taxonomy in
  `docs/CONVENTIONS.md`). Logging is best-effort and must never crash an agent.
- **Cognition is metered.** Token/$ usage is always accounted and bounded. New
  scheduling never removes the budget guard; it builds on it.
- **Secrets come from the environment, never config or code.** `ANTHROPIC_API_KEY`
  and friends are read from env. Never log a secret. Never write one to disk.
- **Tools go behind the `Tool` trait.** Anything an agent does to the world is a
  `Tool`. **MCP is the tool ABI** — prefer exposing capabilities as MCP servers;
  native tools exist only for zero-dependency convenience.
- **The loop never panics on bad input.** Provider/tool/parse failures become
  recorded errors and `Result`, not panics.

## gstack

Use `/browse` from gstack for all web browsing. **Never use `mcp__claude-in-chrome__*` tools.**

Available skills: `/office-hours`, `/plan-ceo-review`, `/plan-eng-review`, `/plan-design-review`, `/design-consultation`, `/design-shotgun`, `/design-html`, `/review`, `/ship`, `/land-and-deploy`, `/canary`, `/benchmark`, `/browse`, `/connect-chrome`, `/qa`, `/qa-only`, `/design-review`, `/setup-browser-cookies`, `/setup-deploy`, `/setup-gbrain`, `/retro`, `/investigate`, `/document-release`, `/document-generate`, `/codex`, `/cso`, `/autoplan`, `/plan-devex-review`, `/devex-review`, `/careful`, `/freeze`, `/guard`, `/unfreeze`, `/gstack-upgrade`, `/learn`.

## Commands

Runtime code lives in `agentd/`; run cargo from there.

```bash
cd agentd

# Build
cargo build                      # debug
cargo build --release            # ~2 MB size-optimized binary

# Quality gate (run before committing)
cargo clippy -- -D warnings
cargo test

# Run an agent (logs to stderr; final answer to stdout; events to flight.jsonl)
export ANTHROPIC_API_KEY=sk-...
cargo run -- agent.toml          # single agent
cargo run -- agents.toml         # multiple agents concurrently (p1.2+)
tail -f flight.jsonl             # watch it think
```

No OpenSSL dependency since p2.1 (`rustls-tls`). For a static musl build:
```bash
# requires `cross` (cargo install cross) and Docker
cross build --target x86_64-unknown-linux-musl --release
```

## Repo layout

```
agentos/                   the repo root (run `claude` here)
  CLAUDE.md                this file
  README.md                project overview
  CHANGELOG.md             notable changes per release
  TODOS.md                 open technical-debt items and completed increments
  docs/
    DESIGN.md              full design & research (the "why")
    ROADMAP.md             the staged build plan (the work queue)
    CONVENTIONS.md         how to extend the codebase consistently
    SPIKES/                exploratory spike docs (implementation notes per increment)
  agentd/                  the runtime (Rust crate)
    Cargo.toml             manifest
    agent.toml             single-agent example spec
    agents.toml            multi-agent example spec (p1.2+)
    README.md              runtime-specific quickstart
    src/
      main.rs              boot: load config -> wire gateway + tools -> run scheduler
      config.rs            TOML agent spec (single [agent] + multi [[agents]] forms)
      flight_recorder.rs   append-only JSONL event log
      scheduler.rs         cooperative multi-agent scheduler (p1.2+)
      agent/
        mod.rs             AgentTask state machine: step() → AgentEffect (p1.1+)
        driver.rs          single-agent backward-compat shim
      inference/
        mod.rs             InferenceGateway trait + neutral message/tool types
        anthropic.rs       remote backend (Anthropic Messages API)
      tools/
        mod.rs             Tool trait + registry
        native.rs          built-in read_file / write_file / list_dir
        mcp.rs             real MCP stdio client -> tools
  templates/               Phase 6: agent template catalogue (p6.1+)
    scout.template.toml    read-only researcher; first catalogue entry
  surfaces/                Phase 3: system surfaces (p3.1+)
    Cargo.toml             manifest (fuser dep Linux-only)
    src/
      lib.rs               re-exports snapshot types + agents_fs module
      snapshot.rs          SchedulerSnapshot / AgentSnapshot / AgentStatus
      agents_fs.rs         AgentsFs FUSE handler + mount() (Linux); stub (others)
  sandbox/                 Phase 3: kernel sandbox for MCP subprocesses (p3.3+)
    Cargo.toml             manifest (Linux-only raw syscall dependencies)
    src/
      lib.rs               SandboxRule enum + CompiledSandbox + compile()/apply_compiled()
  distro/                  Phase 2: Buildroot external tree + QEMU boot
    Makefile               build / run / test / prereqs / clean
    buildroot.config       Buildroot defconfig (x86_64 musl, busybox, cpio.gz)
    kernel-extras.config   kernel fragment: virtio-net + virtio-9p + FUSE + SECCOMP
    overlay/
      init                 /init PID-1 sh script
      agents/              mount point for /agents FUSE filesystem (p3.1)
      usr/bin/agentd       (gitignored; copied by make build)
      etc/
        resolv.conf        nameserver 10.0.2.3 (QEMU SLIRP DNS)
        agentd/
          agent.toml       demo agent config
```

Phase 6 adds further siblings: `agentctl/` (p6.2 operator CLI), more templates (p6.7 starter catalogue).

`agentctl/` layout (p6.2+):

```
agentctl/                operator CLI binary
  src/
    main.rs              arg dispatch
    list.rs              list-templates subcommand (p6.2)
    spawn.rs             spawn <template> subcommand (p6.2)
    inject.rs            inject <id> <text> subcommand (p7.3+)
    orchestrate.rs       orchestrate REPL — spawn + multi-turn SSE loop (orch.1+)
    watch/
      mod.rs             watch entry point; run_plain / run_tui
      app.rs             App state machine + View enum
      reader.rs          reads /agents/ FUSE files → AgentInfo
      views.rs           ratatui render functions
      topology.rs        TopologyGraph + build_graph() + render_tree() (p6.4)
```

`agentd/coordinator-demo.agents.toml` — multi-agent fixture for topology testing (coordinator + 2 scouts).

When in doubt about *what* to build next, the roadmap decides. When in doubt
about *how*, conventions decide. When in doubt about *why*, the design doc decides.

## Skill routing

When the user's request matches an available skill, invoke it via the Skill tool. When in doubt, invoke the skill.

Key routing rules:
- Product ideas/brainstorming → invoke /office-hours
- Strategy/scope → invoke /plan-ceo-review
- Architecture → invoke /plan-eng-review
- Design system/plan review → invoke /design-consultation or /plan-design-review
- Full review pipeline → invoke /autoplan
- Bugs/errors → invoke /investigate
- QA/testing site behavior → invoke /qa or /qa-only
- Code review/diff check → invoke /review
- Visual polish → invoke /design-review
- Ship/deploy/PR → invoke /ship or /land-and-deploy
- Save progress → invoke /context-save
- Resume context → invoke /context-restore
- Author a backlog-ready spec/issue → invoke /spec
