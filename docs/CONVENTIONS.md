# Conventions

How to extend `agentd` without the codebase drifting. Read this before adding a
subsystem, tool, or provider. (For *what* to build, see `ROADMAP.md`; for *why*,
`DESIGN.md`.)

## Ethos

- **Light.** This is meant to be a small, fast runtime. Justify every dependency;
  prefer the standard library and small focused crates. The release profile is
  size-optimized (`opt-level = "z"`, LTO, strip, `panic = "abort"`) — keep it that way.
- **Narrow seams.** Subsystems talk through small traits (`InferenceGateway`,
  `Tool`). New subsystems get their own module and a narrow interface, not a web of
  cross-calls.
- **Mechanism vs policy.** The agent is mechanism (it runs a loop); the scheduler is
  policy (budget, concurrency, priority). Don't push policy into the agent or
  mechanism into config.

## Module boundaries

| Module | Owns | Don't put here |
|---|---|---|
| `agent` | the sans-IO loop state machine | IO, scheduling policy |
| `scheduler` | driving many agents, budget/concurrency policy, performing IO | per-agent loop logic |
| `inference` | provider abstraction + neutral message types | tool logic |
| `tools` | the `Tool` ABI, native tools, MCP client | agent/loop logic |
| `capability` | what an agent is allowed to do | enforcement of unrelated concerns |
| `bus` | agent addressing, messaging, spawn | scheduling internals |
| `events` | `EventKind` enum + stable string serialization; canonical taxonomy source of truth for all `EventKind` variants | business logic |
| `flight_recorder` | the event log (append-only JSONL writer) | business logic |
| `config` | the TOML spec | runtime state |
| `memory` | `MemoryStore` trait + `RedbStore` backend; `context` pressure manager + Tier-2 `MemItem`; `validate_segment` | scheduling, agent loop logic |
| `surfaces` | FUSE virtual filesystem (`AgentsFs`), `SchedulerSnapshot`, `MemoryAccess` bridge trait | business logic, scheduling internals |
| `sandbox` | `SandboxRule` enum, `compile()`/`apply_compiled()` for Landlock + seccomp-bpf | agent loop logic, scheduling |
| `agentctl` | operator CLI binary; `list-templates` + `spawn`; `parse_cap_alias()`; `cap_add_allowed_by_suggestion()` | runtime logic, scheduler, memory |

When a new subsystem appears in the roadmap, add a module; don't bolt it onto an
existing one.

## Error handling

- Use `anyhow::Result` at boundaries; add context with `.map_err(|e| anyhow!(...))`
  or `.context(...)`. Errors should say *which* thing failed (path, server name, etc.).
- **The agent loop never panics on bad input.** Provider, tool, and parse failures
  become recorded errors and a `Result` / an `is_error` tool result — never `panic!`,
  `unwrap()`, or `expect()` on runtime data. (`unwrap` is fine on truly-invariant
  internal state and in tests.)
- Tool failures are normal control flow: capture them as `Block::ToolResult { is_error:
  true, .. }` and let the agent react, rather than aborting the run.

## Flight-recorder event taxonomy

Every meaningful step emits exactly one event via `rec.record(turn, kind, data)`.
`kind` is a stable snake_case string; `data` is a JSON object. **Record everything;
logging is best-effort and must never crash an agent.** Previews of long text/tool
output are truncated (~200 chars) — never log secrets or full file contents.

Phase 0 kinds (canonical — do not rename):

| kind | when |
|---|---|
| `agent_spawned` | agent created (id, model, tools, limits, task_preview) |
| `perceive` | a task/event enters the agent's context |
| `inference_request` | before calling the gateway (msg count, tool count) |
| `inference_response` | after (stop_reason, token usage, running total, preview) |
| `tool_call` | before invoking a tool (id, name, input_preview) |
| `tool_result` | after (ok + preview, or error) |
| `observe` | tool results folded back into context |
| `agent_completed` | terminal: produced a final answer |
| `budget_exceeded` | terminal: per-agent token budget blown |
| `max_turns_reached` | terminal: hit the turn cap |
| `agent_failed` | terminal: inference error terminated the agent (p1.2+) |
| `agent_scheduled` | scheduler admitted the agent's inference request (p1.3+) |
| `agent_deferred` | inference deferred: concurrency cap full; includes priority + seq (p1.3+) |
| `agent_admission_denied` | terminal: global token budget exhausted; agent cannot run (p1.3+) |
| `error` | a stage failed (stage, error) |
| `capability_denied` | tool invocation blocked by capability check (tool, required, agent id) (p1.4+) |
| `message_sent` | agent sent a message to another agent (from, to) (p1.6+) |
| `message_received` | agent received a message (from, to) (p1.6+) |
| `agent_card_registered` | agent card recorded at scheduler seed (id, name, skills) (p1.6+) |
| `fuse_mounted` | `/agents` FUSE filesystem mounted (mount_point) (p3.1+) |
| `fuse_unmounted` | `/agents` FUSE filesystem unmounted (p3.1+) |
| `fuse_skipped` | FUSE mount skipped (non-Linux or `NO_FUSE` set) (p3.1+) |
| `sandbox_applied` | kernel sandbox applied to MCP server subprocess (server, rules) (p3.3+) |
| `sandbox_skipped` | MCP server spawned without sandbox (server, reason) (p3.3+) |
| `tools_registered` | tool registry populated at boot (tool_count) (p0+) |
| `agent_child_result_delivered` | child agent result delivered to spawning parent (parent_id, child_id) (p1.5+) |
| `agent_checkpointed` | agent state checkpointed to disk (agent_id, turn) (p3.2+) |
| `agent_restored` | agent state restored from checkpoint (agent_id, turn) (p3.2+) |
| `system_shutdown_requested` | SIGTERM or SIGINT received; graceful shutdown initiated (p2.3+) |
| `memory_read` | `kv_get` / `kb_get` completed (agent, found: bool; p5.4+: tier:4, class) (p5.1+) |
| `memory_write` | `kv_set` / `kb_put` committed (agent, bytes: usize; p5.4+: tier:4, class) (p5.1+) |
| `memory_unavailable` | store open or transaction failed; kv tools not registered (stage, hint, error) (p5.1+) |
| `memory_quarantined` | corrupt store renamed to `.corrupt`; fresh store opened (path) (p5.1+) |
| `memory_pressure_advisory` | token spend reached SOFT_THRESHOLD (75%); advisory only, no eviction (agent, turn, tokens_spent_pct, soft_threshold) (p5.2+) |
| `memory_paged` | oldest turn pairs evicted from active context to short_term Tier 2 (agent, turn, pages_moved, short_term_depth, tokens_spent_pct) (p5.2+) |
| `memory_distilled` | `mem_remember` committed a long-term memory entry to Tier 3 (agent, key, bytes) (p5.3+) |
| `kb_search` | `kb_search` tool invoked; inverted index queried (agent_id, segment, query_preview, hits: usize, terms_matched: usize) (p5.5+) |
| `memory_evicted` | entry evicted from a KB segment by capacity or age floor (segment, key, reason: "capacity"\|"age") (p5.6+) |
| `mcp_http_connected` | HTTP MCP server connected after initialize + tools/list (server_name, url, session_id_present: bool) (p7.1+) |
| `mcp_http_error` | HTTP MCP server returned non-2xx or JSON-RPC error (server_name, http_status: u16, method) (p7.1+) |
| `mcp_passenv_forwarded` | MCP server spawned with passenv; names which env vars were forwarded, blocked, or absent (server, forwarded: [str], blocked: [str], absent: [str]) (h7.1+) |
| `inference_stream_started` | SSE streaming inference started for an agent turn (agent_id, model) (p7.2+) |
| `inference_stream_completed` | SSE streaming inference completed successfully (agent_id, text_chunks_emitted: u64, input_tokens: u32, output_tokens: u32) (p7.2+) |
| `inference_stream_delta` | one text chunk of a streaming inference response, recorded per-chunk so remote SSE subscribers see live output (agent_id, turn_seq: u64, chunk_seq: u64, text) (ux.1+) |
| `inference_transport_retried` | stale pooled connection caused connect error; request retried once and succeeded (agent_id, model, retries: u32) (con.1+) |
| `fuse_control_received` | operator wrote a valid spawn command to `/agents/control`; agent queued (task_preview, id) (p7.3+) |
| `fuse_control_error` | operator command via `/agents/control` could not be dispatched (error, is_error: true) (p7.3+) |
| `approval_requested` | agent invoked `request_approval`; scheduler parks agent pending operator decision (agent_id, approval_id, kind, risk, summary) (p7.4+) |
| `approval_granted` | operator approved a pending action; agent resumed (agent_id, approval_id, auto_approve_kind: Option<String>) (p7.4+) |
| `approval_rejected` | operator rejected a pending action; agent receives rejection reason (agent_id, approval_id, reason: Option<String>) (p7.4+) |
| `egress_brokered` | egress call permitted; signed receipt written to evidence.jsonl (agent, kind, dest, input_tokens, output_tokens) (p7.5+) |
| `egress_denied` | egress call denied by policy; receipt written (agent, attempted_dest) (p7.5+) |
| `action_receipt_emitted` | action receipt appended to evidence.jsonl (agent, verdict, chain_seq) (p7.5+) |
| `egress_proxy_failed` | egress proxy failed to initialise or write a receipt (error) (p7.5+) |
| `universal_agent_started` | universal-tier child process spawned (id, command, pid, isolation) (p7.6+) |
| `universal_agent_exited` | universal-tier child process exited (id, exit_code: Option<i32>, wall_seconds) (p7.6+) |
| `universal_agent_isolation_degraded` | `runsc` missing; fell back to unsandboxed exec (id, reason) (p7.6+) |
| `scheduler_started` | agentd boot complete; emits `run_id` (UUID v4) used as the OTLP trace root + `config_hash` (obs.1+) |
| `scheduler_stopped` | agentd graceful shutdown; emits `run_id` + `agent_count` (obs.1+) |
| `management_started` | management HTTP API bound and ready (addr, non_loopback_opt_in: bool) (p7.7+; non_loopback_opt_in added ux.0b) |
| `management_request` | management HTTP API received a request (method, path, status: u16) (p7.7+) |
| `approval_http_approved` | operator approved a pending action via the HTTP management API (id, agent_id) (dx.2+) |
| `approval_http_denied` | operator denied a pending action via the HTTP management API (id, agent_id, reason: Option<String>) (dx.2+) |
| `credential_egress_brokered` | credential broker forwarded an upstream API call on behalf of an agent (agent_id, provider, path, response_status: u16, response_bytes: usize) (cred.3+) |
| `credential_accessed` | credential broker received a valid request from an MCP server (agent_id, provider, path, method) (cred.3+) |
| `credential_refresh_failed` | OAuth token refresh write to state_path failed; access token still returned for this request (provider, error, token_written: bool) (cred.3+) |
| `credential_not_provisioned` | requested provider is not configured in [credential_gateway.providers] (provider, hint) (cred.3+) |
| `credential_denied` | MCP server's allowed_providers list does not include the requested provider (agent_id, provider) (cred.3+) |
| `credential_cap_exceeded` | per-agent per-provider request-count cap reached; request rejected with 429 (agent_id, provider, count, limit) (cred.4+) |
| `credential_attention_required` | OAuth provider health transitioned to AttentionRequired after a non-retryable failure; operator must re-authenticate (provider, recovery_kind, reason) (cred.7+) |
| `credential_recovered` | provider health transitioned back to Healthy after a successful credential operation (provider, source: "foreground_request" | "proactive_refresh" | "reset_attention") (cred.7+) |
| `orchestrator_dispatched` | orchestrator spawned an agent in waiting mode (agent_id, task_preview) (orch.1+) |
| `orchestrator_injected` | orchestrator injected a new user turn into a waiting agent (agent_id, text_len: usize) (orch.1+) |
| `orchestrator_turn_complete` | orchestrated agent completed a turn and parked, awaiting next inject (agent_id, answer) (orch.1+) |
| `orchestrator_exited` | orchestrated agent exited; typically because the target agent was not found (agent_id, reason) (orch.1+) |
| `isolation_probed` | device-level isolation capabilities probed at startup (tier, arch, runsc: path\|null, landlock: bool, seccomp: bool) (ma.4+) |
| `capabilities_resolved` | effective capability set computed once at boot from the shared `tier_legality` resolver; descriptive, enforcement unchanged (kind: "agent"\|"mcp_server", name, enforced: [str], inert: [{cap, reason}]) (cap.1+) |
| `agent_spawn_denied` | a spawn was rejected because the child requested a capability not covered by the parent's set — fail-closed, reject not clamp (cap.2+) |
| `capabilities_set` | operator narrowed an agent's capabilities at runtime (ux.13 SetCaps) |
| `budget_set` | operator set a per-agent token budget at runtime (ux.11a SetBudget) |
| `budget_reset` | budget window rolled over / operator reset the spend window (ux.8′+) |
| `agent_cancelled` | operator cancelled an agent at runtime; emitted once per cancelled node, including cascaded children (cause: "cascade from <parent>") (ux.13+) |
| `runs_unavailable` | runs.redb could not be opened; run history unavailable this boot (hint, error) (ux.11b+) |
| `brief_written` | CoS published a morning brief, authored deterministically from runs.redb (agent_id, brief_id, window_from, window_to, run_count, failed_count, spend_total) (ux.11c+) |

Adding events: new behavior gets new kinds, in the same snake_case style, with a
small flat `data` object. The table above is the canonical reference — update it
when a new event kind lands.

Keep the recorder agent-tagged: in multi-agent phases, every event must carry the
acting agent's id so a single `flight.jsonl` is demultiplexable.

## Extension recipes

### Add an inference backend
1. New file `agentd/src/inference/<provider>.rs`; `pub mod <provider>;` in `inference/mod.rs`.
2. `impl InferenceGateway` — map the neutral `Block`/`Msg`/`ToolSpec` types to/from the
   provider's wire format; return `InferenceResponse` with `stop_reason` and token usage.
3. Read credentials from **env only**. Add a base-URL env override.
4. Wire it into the `match config.model.provider` in `main.rs` (and later the scheduler).
> Reminder of the locked decision: remote-only is the default. A *local* backend is
> permitted solely as another `impl InferenceGateway`, never as a core assumption.

### Add a native tool
1. In `agentd/src/tools/native.rs`, define a struct and `impl Tool` (name, description,
   `input_schema` as JSON Schema, async `invoke`).
2. Validate inputs in `invoke`; return a helpful `anyhow` error on bad input.
3. Register it in `register_native`'s table.
4. From p1.4 on, declare the capability the tool requires so the registry can gate it.
> Native tools are for zero-dependency convenience. Prefer exposing real capabilities
> as **MCP servers** — MCP is the tool ABI.

### Connect an MCP server
Configure it under `[[tools.mcp_servers]]` (name, command, args, optional capabilities).
The stdio client (`agentd/src/tools/mcp.rs`) spawns it, does the `initialize` handshake,
lists tools, and registers each as a `Tool`. No code change needed to add a server — only config.

To sandbox the server subprocess (p3.3+), add a `capabilities` array to the server entry.
The `sandbox/` crate compiles Landlock FS rules + seccomp-bpf into a `CompiledSandbox`
applied via `pre_exec`. Omitting `capabilities` runs the server unsandboxed (warn emitted).

**Bundled sidecars carry a self-test contract (ci.1).** Every file matching
`docker/*_mcp.py` MUST implement a `--test` mode that exits 0 **and** prints a
`self-test PASSED` marker — either signal alone can lie (a sidecar can print
PASSED then crash; a flagless server EOFing on `/dev/null` stdin can exit 0).
CI's `sidecar-tests` job and `make test-harness` glob the directory, so a new
sidecar without `--test` fails CI on the next push. No API key may be required;
use a mock/offline path (see `MOCK_EMBEDDINGS=1` in `semantic_kb_mcp.py`).

### Add a memory namespace

Memory namespaces use the `agent:<scope>` convention (e.g. `agent:scratch`, `agent:notes`).
To give an agent read/write access to a namespace:

1. Add `KbRead`/`KbWrite` capabilities in the agent's `capabilities` array in `agent.toml` /
   `agents.toml`:
   ```toml
   capabilities = [
     { KbWrite = { segment = "agent:scratch" } },
     { KbRead  = { segment = "agent:scratch" } },
   ]
   ```
2. Add `kv_get` and/or `kv_set` to `native` in `[tools]`:
   ```toml
   [tools]
   native = ["read_file", "kv_get", "kv_set"]
   ```
   **Note:** `kv_get`/`kv_set` are NOT included in `"all"` — they must be listed explicitly.
3. `KbRead { segment: "agent" }` is a prefix grant: it permits reading any namespace
   starting with `agent:` or `agent/`. Use the tightest scope your agent actually needs.
4. `KbWrite { segment: "" }` (empty) always denies — the empty segment is a sentinel for
   "capability type check only", never a wildcard write grant.

### FUSE surface paths (p3.1+)

`/agents` is a read-only virtual filesystem mounted via FUSE at boot (Linux only).
Each agent appears as a directory; memory and KB surfaces appeared in p5.7.

| Path | Content | Format | Notes |
|---|---|---|---|
| `/agents/<id>/status` | agent lifecycle state | `running` \| `waiting` \| `deferred` \| `awaiting_child:<id>` \| `awaiting_approval` \| `done` \| `failed` | `waiting` = orchestrated agent parked between turns (orch.1+) |
| `/agents/<id>/context_size` | token count | integer | |
| `/agents/<id>/budget` | token budget | integer or `unlimited` | |
| `/agents/<id>/windowed_spend` | spend within the current budget window | integer | ux.11a+ |
| `/agents/<id>/flight` | recent flight events for this agent | JSONL tail (last 20 lines) | |
| `/agents/<id>/memory/short_term` | in-context conversation previews | one `t{n} {role}: {preview}` per line, ≤20 entries | `(empty)` if none; absent if no memory store configured |
| `/agents/<id>/memory/long_term/<key>` | per-agent Tier-3 KB entry | raw JSON value + provenance | key is nanosecond timestamp; ≤100 keys shown |
| `/agents/<id>/tools` | tools available to this agent | one tool name per line, or a none-sentinel | |
| `/agents/<id>/parent` | spawning parent agent id | id, or `(none)` for a root agent | |
| `/agents/<id>/sandbox` | per-agent sandbox rules | JSON | |
| `/agents/<id>/tier` | agent tier | `native` or `universal` (p7.6+) | |
| `/agents/<id>/pid` | OS process ID of universal-tier child | integer, or `(none)` for native agents (p7.6+) | |
| `/agents/<id>/credentials` | per-agent credential grant | JSON — providers, request/denied counts (cred.5+) | |
| `/agents/<id>/attention` | active attention signals | JSON (ux.2a+) | |
| `/agents/kb/<segment>/<key>` | shared KB segment entry | raw JSON value + provenance | agent-namespaced entries (`agent/…`) excluded; ≤100 keys per segment |
| `/agents/approvals` | all pending approval requests (all agents) | JSONL one `PendingActionView` JSON per line; `[]\n` when empty (p7.4+) | read-only; write approvals/rejections via `/agents/control` |
| `/agents/control` | **write-only** control channel | `echo '{"task":"…"}' > /agents/control` — `spawn` / `inject` / `approve` / `reject` / `reset_budget` / `set_budget` / `cancel` / `set_caps` (p7.3+, verbs through ux.13). Full wire format: `docs/CONTROL_SURFACE.md` | the only writable node in the surface. Fire-and-forget: the writer gets no scheduler verdict (see `confirms_mutations()` in `agentctl`) |
| `/agents/system/budget` | global budget window state | JSON — `{"spent":N,"total":0,"resettable":BOOL}`; `resettable` is `[scheduler] budget_reset_interval > 0` (ux.13-TUI) | `total` is always `0` — the global ceiling is not published here |
| `/agents/system/queue` | scheduler queue depth | JSON — `{"depth":N}` | |
| `/agents/system/sandbox` | active sandbox enforcement summary | JSON — `{"any_sandboxed":BOOL,"servers":[{name,transport,isolation,landlock,seccomp,spawn_enforcement,namespace_net,namespace_mount,landlock_net}],"degradations":[…]}` | |
| `/agents/system/provider` | inference provider health | JSON — `{"model":"…","backend":"…"}` | |
| `/agents/system/egress_addr` | bound HTTP egress-proxy URL | URL or `not configured` (p7.5b+) | |
| `/agents/system/isolation` | device-level isolation tier | JSON (ma.4+) | |
| `/agents/system/credentials` | credential-gateway health + per-provider status | JSON (cred.5+) | |

Silent truncation: directories with more than 100 entries show the first 100 (no overflow marker). An ENTRIES index per segment (NAMESPACES table) is deferred to p5.8.

Inode allocation (`agents_fs.rs`): per-agent files are addressed by a fixed `OFF_*` offset added to that agent directory's base inode, on a `DIR_STEP = 20` stride per agent. Offsets `1..=15` are consumed today (`OFF_STATUS` … `OFF_WINDOWED_SPEND`), so a new per-agent file takes the next free offset and **must satisfy the code invariant `OFF_* < DIR_STEP - 1`** (i.e. offset ≤ 18 — compile-time `const _` asserts in `agents_fs.rs` enforce this; `DIR_STEP - 1` leaves the top slot unused as a guard against colliding with the next agent's directory). Global/system pseudofiles instead use the low static inodes, assigned explicitly, not via `OFF_*`: `INO_KB = 9`, `INO_SYSTEM = 10` (the `/agents/system/` dir), `INO_SYS_BUDGET/QUEUE/SANDBOX/PROVIDER = 11–14`, `INO_CONTROL = 15`, `INO_APPROVALS = 16`, then `INO_SYS_EGRESS_ADDR/ISOLATION/CREDENTIALS = 17–19`. A new global file takes the next free static inode. When you add either kind, add a row to this table in the same PR.

## Checkpoint FORMAT_VERSION migration policy

`checkpoint.rs` exports `FORMAT_VERSION: u32`. On load, the probe guard rejects checkpoints
where `format_version > FORMAT_VERSION`. Rules:

- **Additive fields** (new optional data): use `#[serde(default)]` on the new field and bump
  `FORMAT_VERSION`. Old checkpoints load with the default value.
- **Breaking changes** (field removal or rename): bump `FORMAT_VERSION` AND refuse to load
  the old version (or add a migration path, which is preferred).
- `FORMAT_VERSION` is a **compatibility floor**, not a field inventory. The number
  represents the minimum version a reader can safely parse with this code.
- Test each migration: the v(N-1) compat test must use `CheckpointStore::load()` on a raw
  JSON fixture (not just bare serde), to exercise the version-probe path.

## Config

- An agent is a TOML spec. Secrets are **never** in config — env only.
- Provide serde defaults for optional fields (see `config.rs`) so specs stay terse.
- When extending config (multi-agent, capabilities), keep older single-agent specs
  working where reasonable, or migrate the sample and note it in the PR.

## Diagnostics vs. the flight recorder

Two separate channels — don't conflate them:
- **`tracing`** → human diagnostics to **stderr** (`RUST_LOG` controls level). For
  operators watching a run.
- **Flight recorder** → structured **agent activity** to `flight.jsonl`. The durable,
  machine-readable record of what agents did.

The agent's **final answer** goes to **stdout** and nothing else does, so `agentd` is
pipeline-friendly.

## Testing

- Unit-test each module. **Loop and scheduler tests must not hit the network** — use
  the `#[cfg(test)] MockGateway` (added in p1.1) that returns canned
  `InferenceResponse`s, including tool-use turns.
- For the MCP transport, test against a tiny mock stdio server (a fixture that speaks
  the JSON-RPC handshake) rather than a real external server.
- Keep `agent.toml`'s demo as a living smoke test: its flight-event sequence is the
  regression baseline for the single-agent path — don't let refactors change it.

## Templates (p6.1+)

Agent templates live in `templates/` (repo) and `~/.agentos/templates/` (user). User dir
takes precedence on name collision.

**Naming:** filename must be `{name}.template.toml` where `name` matches the
`[template].name` field. No slashes or `..` in the name (enforced by `resolve()`;
`list()` checks name identity but does not reject traversal characters in filenames).

**File layout:** `sample_tasks` is a **top-level key** — it must appear *before* the first
table header (`[template]`, `[model]`, etc.). Keys placed after a table header belong to
that table. Putting `sample_tasks` after `[card]` makes it `card.sample_tasks`, which
causes an explicit parse error (unknown field `sample_tasks` in `TemplateCard`).

**Required sections:** `[template]` (name, description, showcases) and either `[agent]`
(single-agent) or `[[agents]]` (multi-agent). Everything else is optional.
`[template]` is required for parsing; `[agent]`/`[[agents]]` are required for lowering
(enforced by `to_agent_config()`, not by the parser — a `list()` call accepts template-only
files but `to_agent_config()` on them will error).

**Capabilities (`[capabilities]`):** flat sugar; absent = deny-all in the lowered
`AgentConfig`. Available fields: `fs_read`, `fs_write`, `kb_read`, `kb_write`,
`net_ports`, `net_hosts`, `spawn`. Paths must be absolute.
`Capability::Mcp` cannot be expressed here — put Mcp grants directly in
`[agent].capabilities` using the struct form: `capabilities = [{ Mcp = { server = "fs", tools = [] } }]`.

**`[card]`:** catalogue metadata only. Not visible at runtime; stripped by
`TemplateConfig::to_agent_config()`. Runtime `AgentCard` is always derived from `[agent]`
fields (id, name, description, skills).

**`[card]` and `--cap-add` enforcement:** `agentctl spawn --cap-add` checks each
requested capability against `[card].suggested_caps`. Absent `[card]` is treated the
same as `[card]` with empty `suggested_caps` — all `--cap-add` requires `--force`.
Templates that intentionally accept any capability must include `[card]` with the
appropriate `suggested_caps` list. This prevents silent unguarded surfaces when
authors forget the `[card]` section.

**`~/.agentos/templates/`:** not created automatically. Operators create it themselves and
drop `*.template.toml` files there to override or extend the repo catalogue.
