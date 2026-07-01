# Changelog

All notable changes to agentd are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [h7.2-ar-01] - 2026-07-01 (v0.50.0)

### Added
- Generic `agent` Docker entrypoint mode: `TEMPLATE_NAME=<name> AGENT_TASK="..." docker compose run --rm agent`
  lowers any single-agent template to a valid TOML config via `agentctl spawn --dry-run`, rewrites
  `../docker/` paths to `/etc/agentd/` (Docker layout), and execs `agentd`. Covers `scout`,
  `librarian`, `google-agent`, and all future templates without per-template entrypoint cases.
- `DRY_RUN_ONLY=1` env var exits before `exec agentd` and prints the rendered `/data/agent.toml`
  — enables smoke testing path rewriting without a live API key.
- `docker-compose.yml` `agent` service: named `agent-data` volume, `HOME=/data` for OAuth token
  persistence, `AGENTOS_REPO_TEMPLATES_DIR` for explicit template resolution, full OAuth + web-search
  env wiring, per-template task examples as inline comments, Google Cloud Console setup instructions.
- `set -o pipefail` added to `docker/entrypoint.sh` — catches `agentctl` failures that `set -e`
  alone misses in pipeline context.

### Fixed
- Removed static `8585:8585` port binding from both `cos` and `agent` compose services — eliminates
  the port conflict when both services are configured. OAuth callback now requires `--service-ports`.
- `agentctl list-templates` printed on template-not-found error so the valid template names are
  immediately visible without reading docs.
- `code-aware` template exits with a clear error (requires `runsc`/gVisor not in standard image)
  instead of failing silently mid-run when `runsc` is not found.

## [con.1] - 2026-06-30 (v0.49.0)

### Fixed
- TCP keepalive (`SO_KEEPALIVE`, 15 s probe interval) on the Anthropic reqwest client keeps
  Docker NAT conntrack entries alive through long MCP wait periods — fixes silent connection
  drops on the third inference call in multi-turn cos.1 runs.
- Retry once on `is_connect()` errors (stale pooled connection reuse); non-streaming path only.
  Streaming retry is intentionally omitted (would cause duplicate stdout + double billing).
- Removed `streaming = false` stopgap that was previously sed-patched into the Docker cos
  entrypoint; native TCP keepalive makes the workaround unnecessary.

### Added
- `InferenceTransportRetried` flight event emitted when a stale-connection retry succeeds
  (`agent_id`, `model`, `retries: u32` in payload).
- `InferenceResponse.transport_retries: u32` field (`#[serde(default)]` for checkpoint compat).
- OTEL exhaustiveness guard (`otel/tests/event_kind_coverage.rs`) updated for the new event.
- Docker `cos` mode in `entrypoint.sh` — fully self-contained CoS launch with `/data` volume
  and absolute-path rewriting (replaces ad-hoc dev workflow).
- OAUTH_CALLBACK_PORT support in `oauth_mcp.py` — fixes callback port binding inside Docker.

## [cos.1] - 2026-06-27 (v0.48.0)

### Added
- `agentd/cos.agents.toml` — three-agent Chief of Staff system: Executive Orchestrator
  (cron-triggered, always-on, `max_turns=200_000`, `token_budget=5_000_000_000`), Inbox Agent
  (read-only Gmail via OAuth2 sidecar), Curator (KB persistence to `ops:briefs`/`ops:entities`).
- `templates/cos-orchestrator.template.toml` — orchestrator template with lifetime-correct defaults;
  `gated_requires` warning for OAuth + cron env vars.
- `templates/cos-inbox.template.toml` — read-only Gmail analyst; explicit capability scoping
  (no Spawn, no FsWrite, MCP-only); `memory.enabled=false`.
- `templates/cos-curator.template.toml` — KB curator (Haiku model, mechanical writes); scoped to
  `ops:briefs` (log) + `ops:entities` (scratch) only.
- `docs/RUNBOOK.md §11` — Chief of Staff runbook: prerequisites, env-var block, first-run OAuth
  dance, brief location, 7 verification commands, agentctl watch keys, known limits, troubleshooting.
- Template catalogue test updated to expect 13 templates (was 10).

### Architecture
- Composes 8 shipped primitives without new core code: cron (h7.3) + OAuth (h7.2) +
  scheduler/spawn (p1) + KB (p5) + approval gate (p7.4) + egress/receipts (p7.5) +
  gVisor floor (p7.6) + OTLP (obs.1–3).
- Child agents use date-stamped IDs (`inbox-YYYY-MM-DD`, `curator-YYYY-MM-DD`) to avoid
  outcome-map collision across daily cycles.
- Orchestrator prompt has explicit LOOP_BACK step instructing the model to call
  `wait_for_trigger` again after writing the brief (prevents premature FinalAnswer).
- Trust story demonstrable: token-absence, egress-denial, OTLP+signed-receipts,
  approval-gated send, bounded cost.

## [h7.3] - 2026-06-27 (v0.47.0)

### Added
- `docker/cron_mcp.py` — cron/interval trigger MCP server; `wait_for_trigger()` poll-and-retry
  design (MCP_TIMEOUT=30s constraint); supports 5-field cron (UTC) and `every N(s|m|h)` intervals;
  bounded grammar (exit 1 on unsupported tokens); POSIX DOW mapping `(weekday+1)%7`; debounce via
  `_wait_start`; `TRIGGER_MAX_WAIT_S` global abort; 5 self-tests.
- `docker/fs_watch_mcp.py` — filesystem watch trigger MCP server; polls via `os.scandir` every
  `TRIGGER_POLL_INTERVAL_S` (default 2s); tracks mtime_ns + size + inode (detects delete+recreate);
  `TRIGGER_IGNORE_PATTERNS` (fnmatch globs); `TRIGGER_QUIET_PERIOD_S` debounce; 6 self-tests.
- `docker/webhook_mcp.py` — HTTP webhook trigger MCP server; `ThreadingHTTPServer` (no HOL
  blocking); Content-Length cap before read (64 KB); `hmac.compare_digest` HMAC-SHA256; timestamp
  tolerance ±5 min always applied; queue full → 429; `rejected_count` in waiting response; 6
  self-tests.
- `templates/cron-agent.template.toml` — scheduled agent template (cron or interval trigger).
- `templates/webhook-agent.template.toml` — webhook-driven agent template.
- `templates/watcher.template.toml` — updated: `gated_requires` removed (now fully operational),
  wired to `fs_watch_mcp.py`, sample tasks added.
- `docs/MCP_SERVERS.md` — Trigger Servers section with "How trigger agents work" explanation,
  per-server TOML snippets, webhook security notes, and curl example.
- 2 new Rust template tests: `catalogue_watcher_no_longer_gated`,
  `catalogue_trigger_templates_lower_to_valid_config`.
- `Makefile test-harness` — extended to self-test all 6 MCP servers (3 existing + 3 trigger).
- Plan: `docs/plans/h7.3-event-trigger-mcp-servers.md` (26 autoplan decisions, all mechanical).

### Changed
- `docs/ROADMAP.md` — h7.3 marked complete (v0.47.0).
- `docs/MCP_SERVERS.md` — webhook "replay protection" note clarified: timestamp window is ±5 min
  (not nonce-based; sub-window replays require HMAC + application-level dedup).
- `templates/webhook-agent.template.toml` — added comment that changing `TRIGGER_WEBHOOK_PORT`
  requires updating `net_ports` in both capability sections; noted `TRIGGER_WEBHOOK_SECRET`
  strongly recommended when `write_file` is enabled.
- Workspace tests: 1025 → 1027.

### Fixed
- `docker/cron_mcp.py` — `_advance_next_fire()` wrapped in try/except `RuntimeError`; a
  non-repeating cron schedule no longer crashes the server after the first trigger fires.
  Returns `{status: "timeout", message: "No future fire time..."}` instead.
- `docker/cron_mcp.py` — moved `import os` to top-level imports (was inadvertently at module bottom).
- `docker/webhook_mcp.py` — `OSError` on port bind is now caught in `_init()` with a clean
  error message + `sys.exit(1)` instead of a raw Python traceback.

## [h7.2] - 2026-06-27 (v0.46.0)

OAuth MCP Sidecar (harness increment) — generic OAuth2 authorization-code + PKCE Python MCP server,
Google agent template, and full operator quickstart docs.

- **`docker/oauth_mcp.py`** — new Python MCP server with three tools:
  - `oauth_start_auth()` → starts local `127.0.0.1:<random>/callback` server, returns PKCE auth URL
  - `oauth_check_auth()` → exchanges code for tokens after browser flow; returns `{ready: bool, scopes: [...]}`
  - `oauth_call_api(url, method?, headers?, body?)` → authenticated HTTPS call, auto-refresh on 401, host allowlist enforced
- **PKCE (RFC 7636 S256)** — `secrets.token_urlsafe(64)` → 86-char verifier (safe below 128-char hard ceiling)
- **Tool state machine** — `idle → pending → authorized`; `oauth_call_api` before auth returns `{error: "auth_not_ready"}`
- **Threading** — callback server runs on daemon thread; all token state protected by `threading.Lock`
- **AuthSession dataclass** — explicit session object (state, code_verifier, redirect_uri, expires_at, server, thread, result, lock)
- **CSRF protection** — `state` nonce validated exactly in callback (RFC 6749 §10.12); path must be `GET /callback`
- **SSRF dual-layer** — hostname allowlist (`OAUTH_ALLOWED_HOSTS`) + IP block (`_is_ssrf_blocked`) rejects loopback/RFC1918/link-local
- **Token file** — refresh token atomic-written to `~/.agentos-oauth/<OAUTH_PROVIDER_NAME>.json` (mode 0600, dir 0700)
- **`OAUTH_REFRESH_TOKEN` bypass** — env var skips the dance entirely; `oauth_check_auth` returns ready immediately
- **Startup validation** — server prints `oauth_mcp: missing required env OAUTH_CLIENT_ID` and exits 1 if required vars absent
- **`host_not_allowed` error body** — `{"error": "host_not_allowed", "host": "<rejected>"}` for actionable diagnosis
- **`--test` self-test** — 10-case matrix (no real credentials required); `python3 docker/oauth_mcp.py --test`
- **`templates/google-agent.template.toml`** — new template; pre-sets 5 of 7 env vars for Google (AUTH_URL, TOKEN_URL, SCOPES, ALLOWED_HOSTS, PROVIDER_NAME); operator sets only `OAUTH_CLIENT_ID` + `OAUTH_CLIENT_SECRET`; `gated_requires` warning with GCP Console note; includes `gmail.googleapis.com` in allowed hosts
- **`docs/MCP_SERVERS.md`** — new `oauth_mcp` section: full TOML snippet, GCP console setup checklist (incl. "Desktop app" callout), 2-var export list, approval dance sequence (3 steps), error reference table; updated Known servers note to remove "deferred to future increment" caveat

## [h7.1] - 2026-06-26 (v0.45.0)

Standard MCP Servers (harness increment) — three first-party Python MCP servers in `docker/`,
a new `ShellExec` subprocess-sandbox capability, and template updates for scout, code-aware, and librarian.

- **`docker/shell_mcp.py`** — `run_command` tool: `shell=True` subprocess, 30 s default/120 s max timeout,
  stdout/stderr capped at 64 KB, `--test` self-test mode. Exit code, stdout, and stderr returned as JSON.
- **`docker/http_mcp.py`** — `fetch_url` tool: HTTPS-only, no redirect following (returns `is_redirect: true`
  + Location header for 3xx), response body capped at 4 MB. `--test` self-test mode.
- **`docker/search_mcp.py`** — `web_search` tool: Brave Search API, `count` param (1–10), graceful
  `isError: true` with setup instructions when `BRAVE_SEARCH_API_KEY` is absent. `--test` mode.
- **`ShellExec` capability** (`agentd/src/capability.rs`) — subprocess sandbox capability; when present in
  an MCP server's `capabilities`, suppresses `DenySpawn` so the server subprocess can fork/exec shell
  commands. `agentctl spawn` parses alias `shell-exec`; `display_cap()` renders `"ShellExec"` in the TUI.
- **Template updates** — scout gains `http_fetch` + `web_search` MCP blocks + 2 new sample tasks;
  code-aware gains `shell_exec` + `http_fetch` (both with `isolation = "gvisor"`); librarian gains
  `http_fetch` + `web_search` + `net_ports = [443]` capability.
- **`docs/MCP_SERVERS.md`** — new "Standard servers (bundled)" section with per-server table, self-test
  commands, and copy-paste TOML snippets.
- **`agentd/agent.toml`** — commented examples for all 3 standard servers.
- **`Makefile`** `test-harness` target runs `--test` self-tests for all 3 servers.
- **`passenv` field on `McpServerConfig`** — forward named env vars from the parent process into stdio
  MCP server subprocesses. Required for `BRAVE_SEARCH_API_KEY` since `env_clear()` strips the env.
  Templates updated: `passenv = ["BRAVE_SEARCH_API_KEY"]` on `web_search` servers.
- **search_mcp.py fixes** — removed `Accept-Encoding: gzip` (urllib cannot decode gzip; caused silent
  failure on every live query); non-integer `count` now caught with try/except instead of crashing the server.
- **shell_mcp.py fixes** — non-integer `timeout_s` now caught; `start_new_session=True` + `os.killpg`
  for proper process group cleanup on timeout (eliminates orphan grandchild processes).
- **JSON parse error responses** — all three Python servers now return a JSON-RPC `-32700` Parse error
  response on `json.JSONDecodeError` instead of silently returning (which caused Rust MCP client to wait
  30 s then mark the connection broken for the rest of the session).
- **Post-SIGKILL communicate timeout** (`shell_mcp.py`) — after `os.killpg`, `communicate()` is called
  with `timeout=5` to prevent indefinite blocking when a grandchild escapes the process group.
- **HTTP method allowlist** (`http_mcp.py`) — only `GET POST PUT DELETE PATCH HEAD OPTIONS` accepted;
  CONNECT and TRACE rejected to prevent port-scanning and request-smuggling vectors.
- **SSRF loopback/RFC1918 block** (`http_mcp.py`) — DNS-resolved IP of the target host is checked
  against `ipaddress.ip_address.is_loopback / .is_private / .is_link_local` before any connection.
  Blocks `127.x`, `::1`, `169.254.x`, `10.x`, `172.16-31.x`, `192.168.x`.
- **LD_PRELOAD/linker var stripping** (`shell_mcp.py`) — `LD_PRELOAD`, `LD_LIBRARY_PATH`, `LD_AUDIT`,
  `LD_DEBUG`, `DYLD_INSERT_LIBRARIES`, `DYLD_LIBRARY_PATH` are stripped from any agent-supplied `env`
  dict before merging with the process environment.
- **`PASSENV_BLOCKLIST`** (`agentd/src/tools/mcp.rs`) — `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`
  are blocked from passenv forwarding; scheduler overwrites the key with an ephemeral scoped key after
  spawn, so forwarding it here would expose the production key to an untrusted subprocess.
- **`McpPassenvForwarded` flight event** — emitted after each MCP server spawn when `passenv` is
  non-empty; records `forwarded`, `blocked`, and `absent` name lists (never values).
- 1025 workspace tests pass (up from 1009).

## [obs.3] - 2026-06-26 (v0.44.0)

OTLP sidecar gap remediation — copy-truncate fast-grow detection (content sentinel) + export-drop counting.

- **Content sentinel** (`otel/src/tail.rs`) — `FileTailer` stores `last_sentinel: Vec<u8>` (last 64 bytes
  at last-consumed offset). On each poll, when same inode and `cur_len >= offset`, the sentinel window is
  re-read and compared; a mismatch means copy-truncate rotation occurred between polls. Three guards prevent
  false positives: (1) skip check when `offset < SENTINEL_SIZE`; (2) skip check when `last_sentinel.len() !=
  SENTINEL_SIZE` (not yet populated — prevents spurious rotation on first append after `from_beginning=false`
  startup); (3) skip sentinel capture when `new_offset < SENTINEL_SIZE` (prevents u64 underflow). Fixes
  obs.2-ar-01.
- **Export-drop counting** (`otel/src/main.rs` + `otel/src/exporter.rs`) — `export_drops: u64` tracks
  `force_flush()` error count. SIGTERM, SIGINT, and periodic stats paths all use
  `tokio::task::spawn_blocking(move || p.force_flush())` (non-blocking; `SdkTracerProvider` wraps `Arc`,
  `clone()` is O(1)). SIGTERM/SIGINT print a final stats line before break. Periodic stats path records
  `export_drops` delta via new `export_drops_counter` OTLP metric (`agentos.otel.export_drops`, unit
  "failures"), separate from channel-drop counter. Code comment: "export_drops counts flush-attempt
  failures, not spans; one error may represent many lost spans." Fixes obs.2-ar-02.
- **Stats line** — updated format: `exported=N open=M dropped=D export_drops=E flushed_on_rotation=R`.
- **New tests** — 3 new `FileTailer` tests: `test_tail_copy_truncate_fast_grow` (sentinel detects
  copy-truncate when new file grows past old offset); `test_tail_sentinel_no_false_positive` (normal append
  does not trigger rotation); `test_tail_startup_no_false_positive` (first append after `from_beginning=false`
  start does not trigger rotation).
- 1009 workspace tests pass (up from 1006).

## [obs.2] - 2026-06-26 (v0.43.0)

OTLP sidecar hardening — batch exporter, validation unit tests, log rotation flush.

- **`BatchSpanProcessor`** (`otel/src/exporter.rs`) — replaced `with_simple_exporter` with
  `BatchSpanProcessor::builder` + `BatchConfigBuilder` (`max_export_batch_size=512`,
  `max_export_timeout=30s`); `OTEL_EXPORT_BATCH_DELAY_MS` env var (default: 5000ms) wires into
  `with_scheduled_delay`; startup banner now includes `batch_delay_ms`.
- **SIGTERM flush** (`otel/src/main.rs`) — `tokio::signal::unix` SIGTERM handler calls
  `sb.drain_all(now_ns, "shutdown")` + `provider.force_flush()` before exit; prints
  `"agentos-otel: shutdown — flushed N open spans"`; handles short-run sessions (<30s) before
  the idle watchdog fires.
- **Log rotation flush** (`otel/src/main.rs` + `otel/src/span_builder.rs`) — `rotated` flag
  from `tailer.poll()` now handled: calls `sb.reset_for_rotation(now_ns)` which drains all open
  spans AND resets `trace_id`/`run_id`/`run_span_id`/`agent_span_ids`/`span_counter` to prevent
  phantom span relationships across file rotations; rotation-flushed spans tagged with
  `forced_close=log_rotated`; tracked separately as `flushed_on_rotation` (not counted in
  `exported_count`); printed in periodic stats line.
- **Validation error improvements** (`otel/src/main.rs`) — world-writable error now includes
  `(fix: chmod o-w <path>)`; embedded-credentials error includes OTLP_HEADERS alternative;
  absolute-path error includes example path; help text updated to `'true' or '1'` for all
  boolean env vars.
- **Validation unit tests** (8 new in `otel/src/main.rs`) — `validate_log_path_rejects_relative`,
  `validate_log_path_rejects_non_jsonl`, `validate_log_path_accepts_valid_missing_file`,
  `validate_log_path_rejects_world_writable` (unix-gated), `validate_endpoint_rejects_non_http`,
  `validate_endpoint_rejects_embedded_credentials`, `validate_endpoint_accepts_http`,
  `validate_endpoint_accepts_https`.
- **TODOS.md** — obs.1-ar-01/02/03 resolved; copy-truncate detection gap and backend-down
  invisibility added as obs.2-ar-01/02.
- 1006 workspace tests pass (up from 998).

## [obs.1] - 2026-06-26 (v0.42.0)

OTLP observability sidecar — `agentos-otel` tails `flight.jsonl` and exports OpenTelemetry traces to any OTLP backend (Jaeger, Grafana Tempo, Honeycomb, etc.).

- **`otel/`** (new workspace crate `agentos-otel`) — standalone binary; `tail.rs` with `(dev, ino, offset)` triple tracking for log rotation (rename + copy-truncate); `span_builder.rs` state machine that reconstructs spans from flight events (agent/turn/inference/tool hierarchy); `exporter.rs` using OTLP HTTP/protobuf via `opentelemetry-otlp 0.17.0` + `opentelemetry_sdk 0.24.0`; `semconv.rs` with GenAI semconv v1.29.0 attribute constants; `otel/tests/event_kind_coverage.rs` compile-time exhaustiveness guard over all 58 `EventKind` variants.
- **Trace model** — `scheduler_started.run_id` (UUID v4, hyphens stripped → 32-hex) is the OTLP trace ID; agents are child spans; inference/tool calls are grandchild spans; orphan events synthesize missing parent spans.
- **Policies** — duplicate open event force-closes existing span as `UNFINISHED (reason=duplicate_open)`; inactivity watchdog (default 30 s, `OTEL_IDLE_TIMEOUT_SECS`) drains open spans; backpressure channel capped at 10,000 spans (`agentos.otel.spans_dropped` counter).
- **agentd changes** — 2 new `EventKind` variants: `SchedulerStarted` (emits `run_id` UUID v4 + `config_hash`) and `SchedulerStopped` (emits `run_id` + `agent_count`); `uuid 1.x` dep added to agentd; events emitted around `scheduler.run()` in `main.rs`.
- **`docker/otel-compose.yml`** (new) — Jaeger all-in-one with OTLP ports 4317/4318 and UI at 16686; `OTEL_REDACT_PREVIEWS=true` guidance.
- **`docs/CONVENTIONS.md`** — `scheduler_started` and `scheduler_stopped` rows added to event taxonomy table.
- **Env vars** — `FLIGHT_LOG_PATH`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME` (default: `agentos`), `OTEL_TAIL_FROM_BEGINNING`, `OTEL_POLL_INTERVAL_MS`, `OTEL_IDLE_TIMEOUT_SECS`, `OTEL_REDACT_PREVIEWS`, `OTEL_SESSION_ID`, `OTEL_EXPORT_PROTOCOL`.
- **Token metrics** — `gen_ai.client.token.usage` OTLP counter with labels `gen_ai.system`, `gen_ai.request.model`, `session_id`, `token.type` (input/output); extracted from closed `gen_ai.chat` spans and forwarded to the same OTLP endpoint via a separate `SdkMeterProvider`.
- **Security** — `FLIGHT_LOG_PATH` validated (absolute, `.jsonl` extension, not world-writable); `OTEL_EXPORTER_OTLP_ENDPOINT` validated (`http://`/`https://` only, embedded credentials rejected); `otel/` is a separate workspace crate, keeping OTLP deps out of the 6 MB `agentd` binary; `parse_ts` uses saturating arithmetic to prevent u64 overflow on malformed far-future timestamps.
- **Dockerfile** — `agentos-otel` added to the builder and runtime stages alongside `agentd` and `agentctl`.
- 998 workspace tests pass.

## [p7.6] - 2026-06-25 (v0.41.0)

Universal-tier isolation floor — gVisor/runsc child process wrapping for agent workloads that host untrusted or foreign-framework code.

- **`agentd/src/universal.rs`** (new) — `UniversalAgent` struct with `ephemeral_key` field; `spawn()` takes `ephemeral_key: &str`, clears child env, injects PATH/HOME/USER/LANG/TMPDIR + per-agent ephemeral key + `ANTHROPIC_BASE_URL`; `stdin(Stdio::null())` to avoid shared stdin fd; `kill()` (SIGTERM → 5 s → SIGKILL); `try_wait()`, `pid()`, `wall_seconds()`; `which_runsc()` probes PATH.
- **`agentd/src/config.rs`** — `AgentTier` enum (`Native` | `Universal`); 5 new `AgentConfig` fields: `tier`, `command`, `args`, `isolation`, `max_wall_seconds` (all `#[serde(default)]`).
- **`agentd/src/scheduler.rs`** — `universal_agents: HashMap<String, UniversalAgent>` on `SchedulerState`; spawn block generates per-agent ephemeral key and registers it in `proxy_registry`; deregisters on exit/timeout/shutdown; spawn/wall-timeout/nonzero-exit failures propagated to `state.outcomes`; duplicate ID check covers both native and universal agent maps; `poll_universal_agents()` enforces `max_wall_seconds` and inserts into `outcomes`; `update_snapshot()` encodes actual isolation as `"universal:gvisor"` or `"universal:none"` in the tier field.
- **`agentd/src/events.rs`** — 3 new event kinds: `UniversalAgentStarted`, `UniversalAgentExited`, `UniversalAgentIsolationDegraded` (note: `UniversalOutputTruncated` removed — stdout is inherited, not buffered).
- **`surfaces/src/snapshot.rs`** — `tier: Option<String>` + `pid: Option<u32>` on `AgentSnapshot`.
- **`surfaces/src/agents_fs.rs`** — `OFF_TIER = 11`, `OFF_PID = 12`; 13 fixed inodes per agent dir; `tier`/`pid` virtual files.
- **`agentctl/src/watch/reader.rs`** — reads `tier` file (parses `"universal:gvisor"` → `tier="universal"`, `isolation="gvisor"`); adds `isolation: String` to `AgentInfo`.
- **`agentctl/src/watch/views.rs`** — universal agents show `N/A` for context tokens; AgentDetail badge shows actual isolation (`ISO: gvisor` or `ISO: none`) from snapshot; plain-mode output includes tier info.
- **`distro/kernel-extras.config`** — gVisor/KVM comment block added.
- **`docs/CONVENTIONS.md`** — 3 new event rows + 2 new FUSE path rows (`tier`, `pid`).
- **`templates/langchain-worker.template.toml`** (new) — universal-tier template with `gated_requires = "gvisor"`.
- **Security hardening (post-review)** — deregister ephemeral key before `kill()` in both shutdown and wall-timeout paths to close SIGTERM auth window; `egress_addr=None` with universal agents upgraded from `tracing::warn!` to `anyhow::bail!` (fail-fast at startup); native-tier `command` field now rejected at startup.
- 979 workspace tests pass.

## [p7.5b] - 2026-06-24 (v0.40.0)

Universal-tier HTTP forwarding proxy — real key-routing gateway replacing the 501 stub.

- **`agentd/src/egress.rs`** — `ProxyRegistry` (`RwLock<HashMap<String, ProxyEntry>>`);
  `ProxyPolicy { allowed_hosts, token_budget_remaining }`; `register()` / `deregister_by_key()`
  / `entry_for_key()`; `start_http_proxy()` binds hyper v1 listener + routes requests via
  `handle_proxy_request()`; ephemeral key identity via `x-api-key` header; real
  `ANTHROPIC_API_KEY` lives only in proxy memory; hop-by-hop header stripping (Host,
  Content-Length, Transfer-Encoding, Connection); 8 MB response cap → 502; 120 s upstream
  timeout → 504; `Accept: text/event-stream` → 501 with structured `detail` field;
  `json_error_response()` helper; `record_proxy_failed()` flight event; `start_http_stub()`
  kept for backward compat; 12 new unit tests.
- **`agentd/src/scheduler.rs`** — `egress_addr: Option<SocketAddr>` + `proxy_registry:
  Option<Arc<ProxyRegistry>>` on `SchedulerState`; builder methods `with_egress_addr()` +
  `with_proxy_registry()`; `egress_addr()` getter; `update_snapshot()` writes
  `http://{addr}` string; 4 new tests.
- **`surfaces/src/snapshot.rs`** — `egress_addr: Option<String>` on `SchedulerSnapshot`.
- **`surfaces/src/agents_fs.rs`** — `INO_SYS_EGRESS_ADDR = 17`; `sys_file_content()` arm
  returns address or `"not configured\n"`; `SystemDir` lookup + `getattr`/`open` range
  checks + `readdir` entry; 2 new tests.
- **`agentd/src/main.rs`** — captures `real_api_key` before env overwrite; constructs
  `ProxyRegistry`; `start_http_proxy()` → `egress_bound_addr`; fail-closed on bind error;
  passes `with_egress_addr()` + `with_proxy_registry()` to scheduler.
- **RUNBOOK §9** — "HTTP egress proxy" section: enabling, discovering bound port, wiring a
  workload, streaming limitation, verifying proxy started; `[egress]` example in config ref.
- **`agentd/agent.toml`** — commented `[egress]` example block.
- **QA hardening** — 5 CRITICAL/HIGH fixes from ship review: pre-forward budget check (429),
  upstream error message sanitized (no `format!("{e}")`), buffer overflow check before extend,
  `Ordering::AcqRel` on budget `fetch_update` to close TOCTOU race, accept loop `continue` on
  transient errors; content-type allowlist (only `application/json` passes); float token parsing
  (`as_f64()`) for robustness; test helpers DRY-ified via `start_test_proxy` + `register_workload`;
  `proxy_strips_ephemeral_inserts_real_key` rewired to mock upstream (no real API calls in tests).
- **Adversarial-review hardening** — H-2: `start_http_proxy_impl` visibility narrowed (was
  `pub(crate)`, tests use `super::*`); loopback-only assertion added; M-2: `content-type` removed
  from `PASSTHROUGH_HEADERS` — proxy always sets `application/json` upstream regardless of workload
  value; two `unwrap()` in response builder paths changed to `expect()`; H-1 TOCTOU over-spend
  and M-3 `anthropic-beta` passthrough deferred to p7.6 (see TODOS.md `p7.5b-ar-*`).
- **968 workspace tests** (up from 945 in p7.5, +23 in p7.5b).

## [p7.5] - 2026-06-23 (v0.39.0)

Native-tier egress governance — tamper-evident signed audit receipts, boundary
secret rewriting, in-process inference mediator, and offline chain verifier.

- **`agentd/src/evidence.rs`** — `EvidenceWriter` with Ed25519 signing (via `ring`)
  and SHA-256 hash-chaining; `ActionReceipt` / `ReceiptBody` serde types; genesis
  hash = 64 zero hex chars; `record_allowed()` / `record_denied()` return u64 seq;
  `verify_chain()` for offline verification; private key written at 0600 permissions
  on Unix; `resume_chain()` on restart; 5 unit tests.
- **`agentd/src/egress.rs`** — `EgressProxy { writer, recorder }`; `record_inference()`
  emits `EgressBrokered` + `ActionReceiptEmitted`; `record_denied()` emits `EgressDenied`
  + `ActionReceiptEmitted`; `start_http_stub()` binds hyper v1 HTTP server returning 501
  on all paths (p7.5b readiness).
- **Boundary secret rewriting** — after `AnthropicGateway::from_env()` captures the real
  `ANTHROPIC_API_KEY`, `main.rs` overwrites the env var with `sk-ant-PLACEHOLDER-agentd`
  so a memory dump of the agent process yields an inert string.
- **`[egress]` TOML config** — `EgressConfig { evidence_path, key_path, proxy_addr }`;
  `#[serde(default)]`; fail-closed startup if `EvidenceWriter::open()` fails; evidence
  path resolved to absolute at startup (OV-1 pattern).
- **Scheduler threading** — `SchedulerState.egress: Option<Arc<EgressProxy>>`; builder
  `Scheduler::with_egress()`; `make_infer_future()` calls `record_inference()` on both
  streaming and non-streaming paths after a successful response.
- **4 new `EventKind` variants** — `EgressBrokered`, `EgressDenied`,
  `ActionReceiptEmitted`, `EgressProxyFailed`.
- **`agentctl verify`** — offline chain verifier subcommand; reads `evidence.jsonl` +
  Ed25519 public key file; prints `chain ok: N receipts verified` on success.
- **Inspector `Egress` filter** — cycles `All → Errors → Sandbox → CapDenied → Egress → All`
  in `agentctl watch`; matches `egress_brokered`, `egress_denied`, `action_receipt_emitted`.
- **`docs/CONVENTIONS.md`** — 4 new event rows for the egress tier.
- **`ring` / `hyper` / `hyper-util` / `http-body-util`** made explicit in `Cargo.toml`
  (all were already transitive; now declarative).
- 945 workspace tests (up from 932); 13 new tests.

## [p7.4] - 2026-06-22 (v0.38.0)

Human-in-the-loop approval gate: agents can pause and ask the operator for
explicit approval before executing high-risk actions. The operator resolves
pending approvals from the `agentctl watch` TUI or any shell script.

- **`request_approval` native tool** — `kind`, `risk`, `summary`, `args_json`
  inputs; returns `{"approved":true}` or `{"approved":false,"reason":"..."}`;
  available without capability grant (implicit; requires `HumanApprovals` cap).
- **`AgentEffect::RequestApproval`** — new scheduler effect; agent yields until
  operator resolves via `/agents/control`.
- **`ParkedApproval`** / `pending_approvals: HashMap<String, ParkedApproval>`
  on `SchedulerState` — approval ID counter, parked sender, snapshot fields.
- **`ControlCommand` tagged enum** — `{"approve":{"id":"…"}}`,
  `{"approve":{"id":"…","auto_approve_kind":"write_file"}}`,
  `{"reject":{"id":"…","reason":"…"}}`; existing spawn path unchanged.
- **`PendingActionView`** in `surfaces/` — `id`, `agent_id`, `kind`, `risk`,
  `summary`, `args_json`, `age_secs`; exposed via `SchedulerSnapshot`.
- **`INO_APPROVALS = 16`** — read-only root-level FUSE pseudofile
  `/agents/approvals`; JSONL one `PendingActionView` per line; `[]\n` sentinel
  when empty.
- **`AgentStatus::AwaitingApproval(String)`** — new variant; renders as
  `awaiting_approval:<id>` in FUSE status file and `agentctl` table.
- **Checkpoint FORMAT_VERSION 2→3** — `pending_approvals` field added with
  `#[serde(default)]` for backward compat.
- **Flight events** — `ApprovalRequested`, `ApprovalGranted`, `ApprovalRejected`.
- **`agentctl watch` Approvals view** (`[a]` from dashboard) — list of pending
  actions with ID/agent/kind/risk/summary/age columns; `Enter` opens 3-option
  confirm dialog (`[a]pprove`, `[d]` approve+don't-ask-again, `[r]eject`);
  reject reason text input; `write_control_command()` helper (libc::close flush
  guard); `approvals_items` refreshed every tick (time-critical).
- **`agentctl/src/watch/approvals.rs`** — `ApprovalsMode` enum,
  `ApprovalsViewState` struct; 4 unit tests.
- **`agentctl/src/watch/reader.rs`** — `PendingAction` struct +
  `read_approvals()` (handles `[]\n` sentinel + JSONL).
- **`views.rs`** — `render_approvals()` TUI function; `awaiting_approval`
  case in `status_style()` (magenta); `[a]pprove` hint in dashboard footer;
  approvals section in `render_plain()`.
- **`docs/CONVENTIONS.md`** — 3 new event rows, FUSE path row for
  `/agents/approvals`, `awaiting_approval:<id>` added to status table.
- 932 workspace tests (up from 902); 30 new tests.

## [p7.3] - 2026-06-21 (v0.37.0)

Write JSON to `/agents/control` to inject a new agent into a running scheduler
without restarting it. `agentctl watch` spawn view routes through the control
surface when available, staying in the TUI with a green banner after injection.

- **`agentd/src/control.rs`** — `OperatorSpawnRequest` (task, id, max_turns,
  token_budget, priority, capabilities) + `parse_control_command()` with
  empty-task rejection; 5 unit tests.
- **FUSE surface** (`surfaces/`) — `INO_CONTROL = 15` write-only pseudo-file at
  `/agents/control`; per-fh `write_buffers` (64 KiB cap → EFBIG); dispatches
  on `flush()` and `release()`; `perm 0o222`; `read()` returns empty bytes;
  `MountOption::RO` removed; `mount()` accepts `Option<ControlDispatch>`.
- **`ControlDispatch`** — `Arc<dyn Fn(&[u8]) -> i32 + Send + Sync>` callback
  (opaque, avoids circular dep); `try_send` in main.rs returns EBUSY if channel
  full; explicit `libc::close()` in agentctl propagates the error.
- **Scheduler** (`agentd/src/scheduler.rs`) — `with_control(rx)` builder;
  two-case `'main` loop (select on `control_rx` or break when empty, interleave
  when pending); `dispatch_operator_spawn()` (ID validation, collision guard,
  `validate_child_id`, inserts into `parent_map`); gated on `maybe_session`
  (fixes deadlock when FUSE not mounted).
- **`agentctl watch`** — `SpawnOutcome` enum (`InjectedViaControl` keeps TUI,
  `FellBackToExec` replaces process); JSON preview when control surface present;
  green banner on successful injection.
- **Flight events** — `FuseControlReceived`, `FuseControlError`.
- **`docs/CONTROL_SURFACE.md`** — operator reference (wire format, errno table,
  shell examples, TUI integration, EBUSY footgun warning).
- 902 workspace tests (up from 889); 13 new tests.

## [p7.2] - 2026-06-20 (v0.36.0)

Set `streaming = true` in `[model]` and text chunks print to stdout as the
model generates them, instead of waiting for the full response. The existing
run-to-completion path is unchanged — `streaming` defaults to `false`.

- **`InferenceRequest.streaming: bool`** — flag propagated from `ModelConfig`
  via `agent/mod.rs`; `DeferredInfer` carries it transparently.
- **`InferenceGateway::infer_with_stream()`** — new async trait method with
  default fallback (drops channel, calls `infer()`); `AnthropicGateway`
  overrides with a real SSE parser.
- **SSE parser** (`inference/anthropic.rs`) — `parse_sse_event()` helper +
  `parse_sse_stream()`; CRLF-safe; 1 MB line cap; `text_delta` → channel;
  `input_json_delta` → tool accumulator; index-ordered block assembly; sender
  `Err` check to abort on dropped receiver.
- **SSE correctness hardening** — `TextDelta` for an unregistered block index
  now returns `Err` instead of silently drifting state; `input_json` accumulator
  capped at 4 MB (`MAX_TOOL_INPUT_BYTES`) matching the non-streaming body limit;
  empty `input_json` (tool called with no arguments) folds to `{}` instead of
  failing JSON parse.
- **`make_infer_future()`** helper (`scheduler.rs`) — extracts the 30-line
  streaming dispatch block; used by both `enqueue_or_defer` and `drain_deferred`.
- **Scheduler streaming** — `tokio::join!(infer_fut, print_fut)`; async stdout
  via `tokio::io::AsyncWriteExt`; final `\n` after stream; BrokenPipe early
  return (silenced, not fatal); `[agent-id]` prefix for multi-agent runs;
  chunk count only incremented on successful `write_all` + `flush`.
- **Double-print suppression** — `Arc<Mutex<HashSet<String>>> streamed_agents`
  on `Scheduler`; main.rs reads it after `run()` and skips `println!` for
  agents that already streamed.
- **Flight events** — `InferenceStreamStarted` + `InferenceStreamCompleted`
  (with `text_chunks_emitted`; `event_taxonomy_completeness` test updated).
- **`docs/CONVENTIONS.md`** — two new event table rows.
- 889 workspace tests (up from 862); 27 new tests.

## [p7.1] - 2026-06-20 (v0.35.0)

You can now connect agentd to hosted MCP services (Linear, GitHub, and any
Streamable-HTTP-capable server) without running a local subprocess — add a
`url` + `headers_env` block to `[[tools.mcp_servers]]` and the agent gains
those tools automatically. Implements the MCP spec 2025-03-26 HTTP transport.

- **`McpBackend` trait** (`agentd/src/tools/mcp.rs`) — unified interface over
  stdio (`McpClient`) and HTTP (`McpHttpClient`); `McpTool.client` changed from
  `Arc<McpClient>` to `Arc<dyn McpBackend>`; `transport_kind()` returns `"stdio"`
  or `"http"`.
- **`McpHttpClient`** — single-POST JSON-RPC client with SSE state machine;
  `Mcp-Session-Id` header capture; `read_bounded_http_body()` with streaming
  byte-count guard (4 MB limit); `parse_sse_stream()` per SSE spec; 30 s
  `MCP_TIMEOUT`; multi-page `tools/list` with 100-page guard (warns on truncation).
- **Config** (`config.rs`) — `McpServerConfig` extended with `url: Option<String>`
  and `headers_env: HashMap<String,String>`; `command` now `#[serde(default)]`;
  `is_http()` + `validate()` (mutual-exclusion guard; `https://` required;
  embedded credentials in URL rejected).
- **Header-secret safety** — values read from env at startup; never logged;
  only header names appear in error messages.
- **`main.rs`** — transport dispatch (`is_http()` branch) before
  `mcp_require_capabilities` / gVisor / sandbox compile; `McpHttpConnected`
  event on connect; HTTP servers skip sandbox (externally isolated);
  `ServerEnforcement.transport` field (`"stdio"` | `"http"`).
- **FUSE + agentctl surfaces** — `ServerEnforcement.transport` exposed in
  `/agents/system/sandbox`, `/agents/<id>/sandbox` JSON; `agentctl watch`
  shows transport in sandbox rows; plain-mode output includes `transport=`.
- **Flight events** — `mcp_http_connected` (server_name, url, session_id_present)
  + `mcp_http_error` (server_name, http_status, method); CONVENTIONS.md updated.
- **docs/MCP_SERVERS.md** — new file with known HTTP MCP server URLs (Linear, GitHub).
- **`agent.toml`** — commented HTTP server example block added.
- **reqwest** `stream` feature added; **tokio** `net` feature added.
- **Security hardening**: `notify()` body drain bounded (OOM fix); 10 s
  `connect_timeout` (fast-fail on unreachable hosts); redirect following disabled
  (auth header leak prevention).
- **Tests**: 21 new tests (SSE parser, McpServerConfig validation, transport
  rendering, HTTP sandbox rows, taxonomy completeness); 862 total workspace tests
  (up from 841).

## [p6.8] - 2026-06-19 (v0.34.0)

Sandbox-enforcement surface + flight-log inspector for `agentctl watch`.

- **`SandboxSummary` + `ServerEnforcement`** (`surfaces/src/snapshot.rs`) — replaces
  `sandbox_applied: bool` on `SchedulerSnapshot` with a full `SandboxSummary { any_sandboxed,
  servers: Vec<ServerEnforcement>, degradations: Vec<String> }`. `ServerEnforcement` carries
  per-MCP-server fields: `name`, `isolation`, `landlock`, `seccomp`, `spawn_enforcement`,
  `namespace_net`, `namespace_mount`, `landlock_net`.
- **`/agents/<id>/sandbox` FUSE virtual file** (`surfaces/src/agents_fs.rs`) — `OFF_SANDBOX=10`,
  11th per-agent inode slot, emits JSON with the per-agent server enforcement list. Updated
  `alloc_dir`, `prune_dead_agent`, readdir, lookup, getattr, `file_content_for_ino`.
- **`/agents/system/sandbox` expanded** — now emits full `SandboxSummary` JSON including
  `servers[]` and `degradations[]` arrays (was boolean `applied` only).
- **`accessible_server_names: Vec<String>`** on `AgentSnapshot` — names of MCP servers the
  agent has `Mcp`-capability access to; populated from `AgentTask` in `scheduler.rs`.
- **`main.rs` sandbox builder** — builds `ServerEnforcement` per MCP server
  (`#[cfg(target_os = "linux")]`), detects `landlock_net_unavailable` and
  `spawn_enforcement_unavailable_arch` degradations.
- **`agentctl` reader expansion** (`agentctl/src/watch/reader.rs`) — `SysSandbox` gains
  `servers` + `degradations` fields (`#[serde(alias = "applied")]` for backward compat);
  `AgentSandbox` struct; `read_agent_sandbox()` helper; `AgentInfo.sandbox` field.
- **Agent-detail sandbox row** (`views.rs`) — shows per-server flags
  (`landlock`, `seccomp`, `net_ns`, `mount_ns`) inline in the detail pane.
- **System-view degradation warnings** — yellow warning rows appear for each entry in
  `SysSandbox.degradations` (e.g. "landlock_net_unavailable").
- **`View::Inspector`** (`[i]` key in `agentctl watch`) — new `agentctl/src/watch/inspector.rs`
  module with `InspectorState`: loads last 512 KB of `flight.jsonl` (load-once model,
  `[r]` to reload); `[Tab]` cycles filter (All → Errors → Sandbox → CapDenied); `[/]`
  substring search; color-coded body (red=errors, cyan=sandbox, yellow=cap_denied); scroll
  with ↑/↓/j/k/PgUp/PgDn/Home/End.
- **`MAX_INSPECTOR_LINES=500`** cap on loaded flight-log lines.
- **Tests**: 14 new inspector tests, 11 new FUSE tests (incl. unrestricted-caps path, restricted-empty
  path, named-accessible-server intersection, sys sandbox with servers+degradations), 10 new reader
  tests (incl. render_plain sandbox blocks), 1 new checkpoint test (parent_map serde default) —
  total workspace test count: 841.

## [p6.7] - 2026-06-18 (v0.33.0)

Starter catalogue — 7 committed templates covering every AgentOS primitive layer.

- **6 new templates** (`templates/`) — `librarian` (Landlock MCP sandbox), `journaler`
  (Phase-5 durable memory), `coordinator` (spawn + bus), `code-aware` (gVisor isolation),
  `watcher` (trigger-gated, ships as honest one-shot scanner), `memory-custodian`
  (shared KB curation). Each has `sample_tasks`, `showcases`, and `gated_requires` where
  applicable.
- **`TemplateMeta.gated_requires: Option<String>`** — new field in `agentd/src/template.rs`.
  When set, `agentctl spawn` prints a pre-flight warning before exec so operators know
  about Phase-5 memory, gVisor, or event-trigger dependencies.
- **`TemplateEntry.sample_tasks: Vec<String>`** — catalogue listing now carries sample
  tasks; `agentctl list-templates` shows DESCRIPTION (not SHOWCASES) as the primary column
  with showcases on a sub-line for scannability.
- **TUI Spawn view pre-fill** — `SpawnViewState` pre-fills `task_input` with
  `sample_tasks[0]` when navigating to a template with an empty task field.
- **22 new tests** (14 catalogue in `agentd/src/template.rs`, 1 gated_requires parse in
  `agentctl/src/spawn.rs`, 3 prefill/reset in `agentctl/src/watch/app.rs`, 2 truncation
  safety in `agentctl/src/list.rs`, 2 coverage for `load_spawn_templates` + boundary paths).
- Total test count: 808 workspace (agentd lib 396 + agentctl 259 + surfaces 32 + sandbox + integration).

## [p6.6] - 2026-06-18 (v0.32.0)

Spawn view for `agentctl watch`. The new `[n]ew` view is the first interactive/write
form in the TUI — it lets the operator pick a template, fill in a task, toggle
capability grants (pre-checked from the template's `suggested_caps`, deny-by-default),
preview the generated `agent.toml`, then spawn agentd (mode a: generate-and-exec).

- **`agentctl/src/watch/spawn.rs`** — new module: `SpawnTemplate` (name, source,
  description, showcases, suggested_caps); `load_spawn_templates()` (lazy via
  `TemplateResolver`); `display_cap()` struct-form formatter ("FsRead {/workspace}").
- **`View::Spawn`** in `agentctl watch`; `[n]` from Dashboard; `[Tab]` cycles through
  `TemplatePicker → TaskField → CapToggles → ActionGenerate → ActionSpawn → wrap`.
- **`SpawnViewState`** — lazy-loaded templates (once on first entry), cap toggles
  `Vec<(Capability, String, bool)>` (all pre-checked), `task_input`, `preview`,
  `result_msg`, `pending_exec: Option<PendingSpawn>`.
- **Key bindings** — `[g]` generate preview; `[r]` spawn (sets `pending_exec`);
  `Esc` in TaskField defocuses (does not exit view); `Esc`/`q` elsewhere back to Dashboard.
- **`pending_exec` pattern** — `handle_spawn_key` sets `pending_exec`; `run_tui`
  detects it after the event match, breaks the loop, drops `CleanupGuard` (restoring
  the terminal), then calls `execute_pending_spawn` which resolves the template,
  writes a `NamedTempFile`, and `exec`s agentd (Unix `execvp`, replacing the TUI process).
- **`render_spawn()`** in `views.rs` — 5-row layout (header / picker / task / mid-split /
  footer); focused section border highlighted in Yellow; action buttons in mid-split right
  pane; preview shows first 20 lines of generated TOML.
- **Plain mode** — `render_plain` appends a `spawn:` section listing the template catalogue
  (loaded only if the Spawn view was entered; otherwise shows "none loaded").
- **`agentctl/src/spawn.rs`** — `exec_agentd`, `resolve_agentd`, `format_cap` promoted
  to `pub(crate)` for reuse from the watch module.
- **Adversarial review fixes** (landed in the same increment):
  - Cap toggles now revoke baseline caps — `PendingSpawn` gains `disabled_caps: Vec<Capability>`;
    `do_generate` and `execute_pending_spawn` strip them via `caps.retain(|c| !disabled.contains(c))`
    so unchecking a suggested cap that is also in the template `[capabilities]` section actually
    removes it from the generated TOML.
  - `flush()` added before `keep()` in `execute_pending_spawn` — matches CLI `spawn.rs:173`;
    without it the OS could discard buffered TOML bytes when `execvp` replaces the process.
- **793 tests** (+45 new: 8 watch/spawn.rs, 9 app.rs, 8 mod.rs, 0 views.rs — render
  functions tested via render_plain and existing TUI infrastructure; +21 from adversarial
  review coverage: disabled_caps, flush guard, r/g keystroke passthrough, API key guard).

## [p6.5] - 2026-06-18 (v0.31.0)

Memory view for `agentctl watch`. The new `[m]emory` view lets operators browse
per-agent short-term and long-term memory stores, plus shared KB segments, all
with provenance metadata. Data flows through the existing FUSE virtual filesystem
— no direct redb dependency in agentctl. Degrades gracefully when Phase 5 is absent.

- **`agentctl/src/watch/memory.rs`** — new module: `MemoryEntry`, `AgentMemory`,
  `KbSegment` data types; `read_agent_memory()`/`read_kb_segments()` FUSE readers;
  `filter_entries()`/`filter_short_term()` client-side substring filters;
  `MAX_DISPLAY_ENTRIES = 20` / `MAX_SEARCH_ENTRIES = 100` constants.
- **`View::Memory`** in `agentctl watch`; `[m]` from Dashboard; true-tab pane model
  (`[Tab]` cycles Short-term → Long-term → KB); per-pane scroll offsets preserved
  across tab switches; `[/]` search mode filters all three panes; `Esc`/`q` back.
- **`MemoryPaneState`** — `search_query`, `search_active`, `short_term_scroll`,
  `long_term_scroll`, `kb_scroll`, `pane`, `absence`; `active_scroll_mut()` helper.
- **KB pane** always accessible regardless of selected agent — KB data is persistent
  and independent of live agent state.
- **Absence handling** — `MemoryAbsence::Subsystem` when `/agents/kb/` missing;
  `MemoryAbsence::Empty` when present but no segments written; documented messages
  with doc pointer in both cases.
- **Provenance formatting** — nanosecond u64 `ts` (long-term) → RFC3339 UTC via
  chrono; RFC3339 string `ts` (KB) displayed as-is with sub-second stripped;
  `[log]`/`[scratch]`/`[canon]` class badges on KB segments.
- **Plain mode** — `render_plain` dumps all agents' short-term + long-term (first 5
  entries each) plus all KB segments; skips cleanly when Phase 5 absent.
- **727 tests** (+55 new: 22 memory.rs, 14 app.rs, 13 views.rs, 3 mod.rs, 3 other).

## [p6.4] - 2026-06-18 (v0.30.0)

Topology view for `agentctl watch`. The new `[t]opology` view renders the live spawn tree
and directed message graph derived from the scheduler snapshot and an optional
`flight.jsonl` tail (up to 512 KB). Key additions:

- **`parent_id: Option<String>`** on `AgentSnapshot`; populated from an insert-only
  `parent_map: HashMap<String,String>` in `SchedulerState`, persisted in checkpoints
  with `#[serde(default)]` for backwards compatibility.
- **`OFF_PARENT = 9`** — new FUSE virtual file `/agents/<id>/parent`; `reader.rs` reads
  it into `AgentInfo.parent_id`.
- **`agentctl/src/watch/topology.rs`** — `TopologyGraph`, `build_graph()` (512 KB tail
  cap, directed edges from `message_sent` events, cycle guard), `render_tree()`,
  `status_badge()`, `parse_message_edges()`.
- **`View::Topology`** in `agentctl watch`; key `t` from Dashboard; `Esc`/`q` returns
  to Dashboard; ↑/↓/j/k scrolls; fixed legend footer outside scrollable region; minimum
  terminal width 60 cols.
- **`--log-path`** CLI flag on `agentctl watch` for message edge data.
- **Plain mode topology section**: `topology: <id> parent=<id>|none status=<status>`.
- **`coordinator-demo.agents.toml`** — acceptance fixture: coordinator + 2 scouts.
- 455 tests pass (macOS; Linux adds FUSE surface + sandbox tests); `make clippy-linux`
  required for FUSE surface changes.

### Fixed (adversarial review)
- `parse_message_edges` now reads `data.to` (not top-level `to`) to match the
  `FlightRecorder` event schema — message edges were always empty against real
  `flight.jsonl` because `"to"` is nested under `"data"`.
- Test fixtures updated to use the correct flight-log event structure.
- `topology_scroll` reset to 0 when switching into the Topology view so
  scroll state does not carry over stale offsets from a prior visit.
- Clippy: `map_or(true, …)` → `is_none_or`; `map_or(false, …)` → `is_some_and`;
  `#[allow(clippy::too_many_arguments)]` on the private recursive tree renderer.

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
