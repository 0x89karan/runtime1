use std::{io::IsTerminal, path::PathBuf, sync::{Arc, RwLock}};

use anyhow::Context;
use agentd::{agent::{truncate, PREVIEW_CHARS}, checkpoint::CheckpointStore, config, scheduler::Scheduler};
use agentd::capability::{normalize_path, Capability};
use agentd::credential::CredentialGateway;
use agentd::flight_recorder::{EventKind, FlightRecorder};
use agentd::inference::anthropic::AnthropicGateway;
use agentd::memory::store::RedbStore;
use agentd::tools::{
    mcp::{McpBackend, McpClient, McpHttpClient, McpTool, PASSENV_BLOCKLIST},
    native::register_native,
    ToolRegistry,
};
use sandbox::{CompiledSandbox, SandboxRule};
#[cfg(target_os = "linux")]
use surfaces::MemoryAccess;
use surfaces::SchedulerSnapshot;

/// Bridge from `Arc<dyn MemoryStore>` to `Arc<dyn MemoryAccess>` for the FUSE handler.
#[cfg(target_os = "linux")]
struct MemoryAccessBridge(Arc<dyn agentd::memory::MemoryStore>);
#[cfg(target_os = "linux")]
impl MemoryAccess for MemoryAccessBridge {
    fn list_namespaces(&self) -> Vec<String> {
        self.0.list_namespaces().unwrap_or_else(|e| {
            tracing::warn!("memory FUSE: list_namespaces error: {e:#}");
            vec![]
        })
    }
    fn list_keys(&self, namespace: &str) -> Vec<String> {
        self.0.list_keys(namespace).unwrap_or_else(|e| {
            tracing::warn!("memory FUSE: list_keys({namespace}) error: {e:#}");
            vec![]
        })
    }
    fn get_entry(&self, namespace: &str, key: &str) -> Option<String> {
        match self.0.get(namespace, key) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("memory FUSE: get({namespace}, {key}) error: {e:#}");
                None
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    // --no-fuse: skip FUSE mount unconditionally (useful in CI and dev environments
    // without the FUSE kernel module). AGENTOS_NO_FUSE env var has the same effect.
    let no_fuse = raw_args.iter().any(|a| a == "--no-fuse")
        || std::env::var("AGENTOS_NO_FUSE")
            .is_ok_and(|v| !matches!(v.to_lowercase().as_str(), "" | "0" | "false" | "no"));

    // --log-path <path>: override the flight log destination (default: flight.jsonl in CWD).
    let log_path_override = parse_log_path(&raw_args);
    if raw_args.iter().any(|a| a == "--log-path") && log_path_override.is_none() {
        anyhow::bail!("--log-path requires a value (e.g. --log-path /path/to/flight.jsonl)");
    }

    // Strip recognised flags (and their value arguments) from the positional args.
    let filtered_strings = filter_positional_args(&raw_args);
    let filtered: Vec<&str> = filtered_strings.iter().map(|s| s.as_str()).collect();

    let result = match filtered.first().copied() {
        Some("--probe") => {
            let prompt = filtered
                .get(1)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("--probe requires a prompt argument"))?;
            run_probe(prompt, resolve_log_path(log_path_override, None)).await
        }
        // `agentd check [--strict] <config>` — capability declaration-surface linter (cap.1).
        // Static, synchronous, no boot side-effects; fail-closed on any error finding.
        Some("check") => {
            let strict = raw_args.iter().any(|a| a == "--strict");
            match filtered.get(1).copied() {
                Some(path) => agentd::check::run_check(std::path::Path::new(path), strict),
                None => Err(anyhow::anyhow!("agentd check requires a config path: agentd check [--strict] <config.toml>")),
            }
        }
        Some(path) => run_agent(PathBuf::from(path), no_fuse, log_path_override).await,
        None => run_agent(PathBuf::from("agent.toml"), no_fuse, log_path_override).await,
    };

    if let Err(e) = result {
        // Use {e:#} to emit the full anyhow error chain (not just the outermost context).
        tracing::error!("agentd exited with error: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run_agent(path: PathBuf, no_fuse: bool, log_path_override: Option<PathBuf>) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("loading config from {path:?}"))?;
    let mut cfg: config::Config =
        toml::from_str(&raw).with_context(|| format!("parsing config from {path:?}"))?;
    cfg.management.apply_env_overrides();

    // ux.8′: a global ceiling with no reset window permanently denies once
    // exhausted (audit86-P0-2). Warn so the operator sets a window before the
    // always-on agent self-bricks.
    if cfg.scheduler.global_token_budget > 0 && cfg.scheduler.budget_reset_interval == 0 {
        tracing::warn!(
            global_token_budget = cfg.scheduler.global_token_budget,
            "global_token_budget is set but budget_reset_interval is 0 — this is a \
             LIFETIME ceiling and will permanently deny once exhausted. Set \
             [scheduler] budget_reset_interval (e.g. 86400 for daily) to auto-reset."
        );
    }
    // ux.8′ S3: an absurdly small window rebases so often it neuters the ceiling
    // (each window is a fresh full budget), effectively unbounding lifetime spend.
    if cfg.scheduler.budget_reset_interval > 0 && cfg.scheduler.budget_reset_interval < 3600 {
        tracing::warn!(
            budget_reset_interval = cfg.scheduler.budget_reset_interval,
            "budget_reset_interval is under 1h — the window rebases so frequently the \
             ceiling barely constrains cumulative spend. Use a longer window (e.g. 86400)."
        );
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let config_hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        raw.hash(&mut h);
        format!("{:016x}", h.finish())
    };

    // Resolve flight log path: CLI flag > TOML field > default "flight.jsonl".
    let log_path = resolve_log_path(log_path_override, cfg.log_path.as_deref());
    // Broadcast channel for SSE fan-out (p7.7). Always created; only subscribed when
    // management is enabled. Overhead is negligible when no subscribers are present.
    let (broadcast_tx, _broadcast_rx_guard) = tokio::sync::broadcast::channel::<String>(1024);
    let recorder = Arc::new(
        FlightRecorder::new(&log_path)?.with_broadcast(broadcast_tx.clone()),
    );

    // cap.1: log each agent's + MCP server's EFFECTIVE capability set once at boot, computed
    // by the same shared resolver `agentd check` uses (enforced vs inert). Descriptive only.
    for payload in agentd::check::capabilities_resolved_events(&cfg) {
        recorder.record("agentd", None, EventKind::CapabilitiesResolved, payload);
    }

    let mut agent_cfgs = cfg.agent_configs()?;

    // stdin fallback: only for the single [agent] form with an empty task
    if cfg.agent.is_some() {
        let ac = &mut agent_cfgs[0];
        if ac.task.is_empty() {
            if !std::io::stdin().is_terminal() {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .context("reading task from stdin")?;
                let trimmed = buf.trim().to_string();
                anyhow::ensure!(!trimmed.is_empty(), "no task: stdin was empty");
                ac.task = trimmed;
            } else {
                anyhow::bail!("no task: set [agent].task in config or pipe text to stdin");
            }
        }
    } else {
        // [[agents]] form: every agent must have a task in config
        for ac in &agent_cfgs {
            anyhow::ensure!(
                !ac.task.is_empty(),
                "agent '{}' has no task; set task in [[agents]]",
                ac.id
            );
        }
    }

    // p7.6: startup validation for universal-tier agents.
    {
        use agentd::config::AgentTier;

        // native-tier agents must not set `command`; that field belongs to universal-tier.
        for c in &agent_cfgs {
            if c.tier == AgentTier::Native && c.command.is_some() {
                anyhow::bail!(
                    "agent '{}': native tier does not use `command`; \
                     remove it or set tier = \"universal\"",
                    c.id
                );
            }
        }

        let universal_count = agent_cfgs.iter().filter(|c| c.tier == AgentTier::Universal).count();
        if universal_count > 0 {
            if cfg.egress.proxy_addr.is_none() {
                anyhow::bail!(
                    "{universal_count} universal-tier agent(s) configured but \
                     [egress].proxy_addr is not set. \
                     Add `proxy_addr = \"127.0.0.1:<port>\"` under `[egress]` in your config."
                );
            }
            // Fail-fast on Linux if any universal+gvisor agent needs runsc but it is absent.
            // Silently running without isolation defeats the purpose of requesting gVisor.
            #[cfg(target_os = "linux")]
            {
                use agentd::config::IsolationMode;
                let needs_gvisor = agent_cfgs.iter().any(|c| {
                    c.tier == AgentTier::Universal && c.isolation == IsolationMode::Gvisor
                });
                if needs_gvisor && agentd::universal::which_runsc().is_none() {
                    tracing::error!(
                        "one or more agents require isolation = \"gvisor\" but 'runsc' is not found on PATH.\n\
                         Install gVisor: https://gvisor.dev/docs/user_guide/install/\n\
                         Then verify with: runsc --version"
                    );
                    std::process::exit(1);
                }
            }
        }
    }

    // Build AgentCards from configs — static, used by list_agents tool.
    let cards: Arc<Vec<agentd::config::AgentCard>> = Arc::new(
        agent_cfgs.iter().map(agentd::config::AgentCard::from).collect()
    );
    for card in cards.iter() {
        recorder.record(
            &card.id,
            None,
            EventKind::AgentCardRegistered,
            serde_json::json!({
                "id":          card.id,
                "name":        card.name,
                "description": card.description,
                "skills":      card.skills,
            }),
        );
    }

    let mut registry = ToolRegistry::new();

    // Open memory store (if enabled). Quarantine corrupt files and emit flight events.
    let memory_store: Option<Arc<dyn agentd::memory::MemoryStore>> = if cfg.memory.enabled {
        let store_path = PathBuf::from(&cfg.memory.store_path);

        // Startup invariant (p5.8): the memory store must not fall inside any MCP
        // server's AllowFsRead or AllowFsWrite sandbox prefix.  A sandboxed server
        // that can read/write the store path could corrupt or exfiltrate all memory.
        // We only enforce the absolute-path requirement when MCP FS prefixes are
        // present — without them, starts_with is not applicable and relative paths
        // are harmless (they resolve relative to CWD as before p5.8).
        let norm_store = normalize_path(&store_path);
        let has_mcp_fs_prefix = cfg.tools.mcp_servers.iter().any(|srv| {
            srv.capabilities.iter().flatten().any(|cap| {
                matches!(cap, Capability::FsRead { .. } | Capability::FsWrite { .. })
            })
        });
        if has_mcp_fs_prefix {
            anyhow::ensure!(
                norm_store.is_absolute(),
                "memory.store_path must be an absolute path when MCP FS prefixes are \
                 configured, got: {}  \
                 (set store_path = \"/run/memory/memory.redb\" in [memory])",
                store_path.display()
            );
        }
        for srv in &cfg.tools.mcp_servers {
            for cap in srv.capabilities.iter().flatten() {
                let prefix = match cap {
                    Capability::FsRead { prefix } | Capability::FsWrite { prefix } => prefix,
                    _ => continue,
                };
                let norm_prefix = normalize_path(std::path::Path::new(prefix));
                // Empty prefix after normalization is not a valid grant — skip it.
                // (mirrors the guard in capability.rs satisfies())
                if norm_prefix.as_os_str().is_empty() {
                    continue;
                }
                anyhow::ensure!(
                    !norm_store.starts_with(&norm_prefix),
                    "memory store {} falls inside MCP server {:?}'s {} sandbox prefix {}; \
                     move the store outside all server FS prefixes \
                     (e.g. set store_path = \"/run/memory/memory.redb\" in [memory])",
                    norm_store.display(),
                    srv.name,
                    if matches!(cap, Capability::FsRead { .. }) { "FsRead" } else { "FsWrite" },
                    norm_prefix.display()
                );
            }
        }

        match RedbStore::open(&store_path) {
            Ok((store, quarantined)) => {
                if let Some(ref corrupt_path) = quarantined {
                    recorder.record(
                        "agentd",
                        None,
                        EventKind::MemoryQuarantined,
                        serde_json::json!({ "path": corrupt_path.display().to_string() }),
                    );
                    tracing::warn!(path = %corrupt_path.display(), "corrupt memory store quarantined; starting fresh");
                }
                Some(Arc::new(store))
            }
            Err(e) => {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::MemoryUnavailable,
                    serde_json::json!({
                        "stage": "open",
                        "hint": "kv_get/kv_set will not be registered",
                        "error": format!("{e:#}"),
                    }),
                );
                tracing::warn!(error = %e, "memory store unavailable; kv tools will not be registered");
                None
            }
        }
    } else {
        tracing::info!("memory store disabled (memory.enabled = false)");
        None
    };

    // ── ux.11b-substrate: durable run history ──────────────────────────────
    // Separate `runs.redb` beside the memory store (E6 — no shared write-lock with
    // KB traffic). Best-effort: an open failure emits RunsUnavailable and boot
    // continues with a disabled tracker (run history is best-effort, like the
    // flight recorder — it must never block boot).
    let runs_path = {
        let base = PathBuf::from(&cfg.memory.store_path);
        base.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.join("runs.redb"))
            .unwrap_or_else(|| PathBuf::from("runs.redb"))
    };
    #[allow(clippy::type_complexity)]
    let (run_tracker, runs_store, run_writer_handle): (
        agentd::runs::RunTracker,
        Option<Arc<agentd::runs::RunsStore>>,
        Option<tokio::task::JoinHandle<()>>,
    ) =
        match agentd::runs::RunsStore::open(&runs_path) {
            Ok((store, quarantined)) => {
                if let Some(ref corrupt_path) = quarantined {
                    tracing::warn!(path = %corrupt_path.display(), "corrupt runs store quarantined; starting fresh");
                }
                let store = Arc::new(store);
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                // Keep the JoinHandle so we can drain queued run events at shutdown —
                // otherwise the runtime can cancel the writer before the final Close
                // events land, leaving terminal runs stuck "running" (ship review).
                let handle = tokio::spawn(agentd::runs::run_writer(rx, Arc::clone(&store)));
                (agentd::runs::RunTracker::new(tx), Some(store), Some(handle))
            }
            Err(e) => {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::RunsUnavailable,
                    serde_json::json!({
                        "hint": "runs_query / GET /api/v1/runs / /agents/runs will be empty",
                        "error": format!("{e:#}"),
                    }),
                );
                tracing::warn!(error = %e, "runs store unavailable; run history disabled this boot");
                (agentd::runs::RunTracker::disabled(), None, None)
            }
        };
    let runs_store_for_management = runs_store.clone();

    // Initialise declared KB segment classes (p5.4). Done before registering tools so
    // that `kb_put` enforces the correct class from the first invocation.
    if let Some(ref store) = memory_store {
        for seg in &cfg.memory.segments {
            if let Err(e) = store.set_segment_class(&seg.name, seg.class.clone()) {
                tracing::warn!(segment = %seg.name, error = %e, "failed to initialise segment class");
            } else {
                tracing::debug!(segment = %seg.name, class = ?seg.class, "KB segment initialised");
            }
            // F-03: persist the eviction floor so live writes self-trim. The
            // limits are global defaults (cfg.memory), applied to every non-canon
            // segment; evict() itself skips canon.
            let max_age_secs = cfg.memory.max_entry_age_days.map(|d| d * 86_400);
            if let Err(e) =
                store.set_segment_limits(&seg.name, cfg.memory.max_entries_per_segment, max_age_secs)
            {
                tracing::warn!(segment = %seg.name, error = %e, "failed to initialise segment limits");
            }
            // F-14: operator-seed declared entries (e.g. canon trust anchors).
            // This is an operator write at startup; it intentionally bypasses the
            // agent-facing canon write-protection enforced by the kb_put tool.
            for entry in &seg.seed {
                if let Err(e) = store.put(&seg.name, &entry.key, &entry.value) {
                    tracing::warn!(segment = %seg.name, key = %entry.key, error = %e, "failed to seed segment entry");
                }
            }
        }
    }

    // Keep clones for distillation wiring (p5.6) and management API (p7.7) before moving into registry.
    let memory_store_for_distillation = memory_store.clone();
    let memory_store_for_management = memory_store.clone();
    // ux.11c: publish_brief routes through the run-writer lane via a BriefPublisher on the
    // same channel as lifecycle events (ordering fix); disabled when no run store opened.
    let brief_publisher = if runs_store.is_some() {
        Some(run_tracker.brief_publisher())
    } else {
        None
    };
    register_native(&mut registry, &cfg.tools.native, Some(Arc::clone(&cards)), memory_store, runs_store.clone(), brief_publisher)?;

    // Pass 1: validate capabilities and isolation settings before spawning any process.

    // Validate each server's transport/command mutual exclusion upfront.
    for server in &cfg.tools.mcp_servers {
        server.validate()
            .with_context(|| format!("validating MCP server '{}'", server.name))?;
    }

    // Check gVisor availability upfront so the error is clear, not buried in spawn output.
    #[cfg(target_os = "linux")]
    for server in cfg.tools.mcp_servers.iter().filter(|s| !s.is_http()) {
        if server.isolation == config::IsolationMode::Gvisor {
            let runsc_found = std::env::var("PATH").ok()
                .map(|p| std::env::split_paths(&p).any(|dir| dir.join("runsc").exists()))
                .unwrap_or(false);
            if !runsc_found {
                anyhow::bail!(
                    "MCP server '{}' requires isolation = \"gvisor\" but 'runsc' is not found on PATH",
                    server.name
                );
            }
        }
    }

    // When mcp_require_capabilities is true, refuse to start if any stdio server would
    // run unsandboxed — either because the field is missing OR because the caps
    // produce no effective rules (e.g. capabilities=[{Spawn}] yields empty rules).
    // HTTP servers are excluded: they are externally isolated (no subprocess to sandbox).
    if cfg.tools.mcp_require_capabilities {
        let missing: Vec<&str> = cfg.tools.mcp_servers
            .iter()
            .filter(|s| !s.is_http())
            .filter(|s| {
                s.capabilities.is_none()
                    || s.capabilities.as_deref().map(|c| caps_to_rules(c).is_empty()).unwrap_or(false)
            })
            .map(|s| s.name.as_str())
            .collect();
        if !missing.is_empty() {
            anyhow::bail!(
                "mcp_require_capabilities is set but the following MCP servers have no \
                 effective sandbox rules: {}. Add capabilities (e.g. `capabilities = []` for \
                 spawn-deny only) or set mcp_require_capabilities = false.",
                missing.join(", ")
            );
        }
    }

    // ── cred.3: credential broker ─────────────────────────────────────────────
    // Start the credential gateway BEFORE spawning any MCP server so we can
    // inject AGENTD_CREDENTIAL_GATEWAY_URL + AGENTD_CREDENTIAL_TOKEN into each
    // subprocess environment at spawn time.

    // cred.7: peek at the checkpoint file early (before the full restore at line ~1115)
    // to extract per-provider health states so the gateway can restore AttentionRequired.
    // The file is not removed here; the full restore below handles that.
    let early_credential_health: std::collections::HashMap<String, agentd::checkpoint::ProviderHealthCheckpoint> = {
        let store = CheckpointStore::new(std::path::Path::new("."));
        store.load().ok().flatten()
            .map(|cp| cp.credential_health)
            .unwrap_or_default()
    };

    let (maybe_cred_gw, cred_gw_url): (Option<Arc<CredentialGateway>>, Option<String>) =
        if cfg.credential_gateway.enabled {
            // cred.4: derive caps_db_path from memory store dir so cap counters survive
            // agentd restarts without requiring operator configuration.
            let mut cred_gw_cfg = cfg.credential_gateway.clone();
            if cred_gw_cfg.caps_db_path.is_none() {
                if let Some(parent) = std::path::Path::new(&cfg.memory.store_path).parent() {
                    cred_gw_cfg.caps_db_path =
                        Some(parent.join("caps.redb").to_string_lossy().into_owned());
                }
            }
            // OV-1: caps_db_path must not fall inside any MCP server's FS sandbox prefix.
            // A sandboxed server with FsWrite access to caps.redb could reset all caps to 0,
            // granting unlimited requests on next restart.
            if let Some(ref caps_path) = cred_gw_cfg.caps_db_path {
                let norm_caps = normalize_path(std::path::Path::new(caps_path));
                let caps_has_mcp_fs = cfg.tools.mcp_servers.iter().any(|srv| {
                    srv.capabilities.iter().flatten().any(|cap| {
                        matches!(cap, Capability::FsRead { .. } | Capability::FsWrite { .. })
                    })
                });
                if caps_has_mcp_fs && norm_caps.is_absolute() {
                    for srv in &cfg.tools.mcp_servers {
                        for cap in srv.capabilities.iter().flatten() {
                            let prefix = match cap {
                                Capability::FsRead { prefix } | Capability::FsWrite { prefix } => prefix,
                                _ => continue,
                            };
                            let norm_prefix = normalize_path(std::path::Path::new(prefix));
                            if norm_prefix.as_os_str().is_empty() {
                                continue;
                            }
                            anyhow::ensure!(
                                !norm_caps.starts_with(&norm_prefix),
                                "caps_db_path {} falls inside MCP server {:?}'s {} sandbox prefix {}; \
                                 move caps_db_path outside all server FS prefixes",
                                norm_caps.display(),
                                srv.name,
                                if matches!(cap, Capability::FsRead { .. }) { "FsRead" } else { "FsWrite" },
                                norm_prefix.display()
                            );
                        }
                    }
                }
            }
            match CredentialGateway::start(&cred_gw_cfg, Arc::clone(&recorder), early_credential_health).await {
                Ok((gw, addr)) => {
                    tracing::info!(addr = %addr, "credential gateway started");
                    (Some(gw), Some(format!("http://{addr}")))
                }
                Err(e) => {
                    return Err(e.context("credential gateway failed to bind (fail-closed)"));
                }
            }
        } else {
            (None, None)
        };
    // Tokens issued to MCP servers; deregistered after the scheduler exits.
    let mut cred_tokens: Vec<String> = Vec::new();
    // ─────────────────────────────────────────────────────────────────────────

    // Held for Drop: keeps MCP child processes (stdio) or connection objects (HTTP) alive.
    // std::process::exit() bypasses Drop, so we must return Err instead of
    // calling exit() while mcp_backends is still in scope.
    let mut mcp_backends: Vec<Arc<dyn McpBackend>> = Vec::new();
    let mut any_sandbox_applied = false;
    // Per-server enforcement records collected during spawn; used to populate
    // SandboxSummary on the snapshot after all servers are started.
    // degradations computed once at server spawn; reflect startup-time policy, not runtime state.
    let mut server_enforcements: Vec<surfaces::ServerEnforcement> = Vec::new();
    // On non-Linux, degradation_set is never mutated (linux-gated code only).
    // BTreeSet gives deterministic ordering so the JSON array is stable across runs.
    #[allow(unused_mut)]
    let mut degradation_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for server in &cfg.tools.mcp_servers {
        if server.is_http() {
            // ── HTTP / Streamable-HTTP transport path ─────────────────────────
            let url = server.url.as_deref().expect("validated above");
            // Strip query string before logging — query params may contain secrets.
            let url_for_log = url.split('?').next().unwrap_or(url);
            tracing::info!(name = %server.name, url = url_for_log, "connecting to HTTP MCP server");

            let (backend, specs, session_id_present) =
                McpHttpClient::connect(&server.name, url, &server.headers_env)
                    .await
                    .with_context(|| format!("connecting to HTTP MCP server '{}'", server.name))?;

            recorder.record(
                "agentd",
                None,
                EventKind::McpHttpConnected,
                serde_json::json!({
                    "server_name":        server.name,
                    "url":                url_for_log,
                    "session_id_present": session_id_present,
                }),
            );

            let n = specs.len();
            for spec in specs {
                let tool = Box::new(McpTool::new(Arc::clone(&backend), spec, server.name.clone()));
                if server.tool_override {
                    registry
                        .register_override(tool)
                        .with_context(|| format!("registering (override) tools from HTTP MCP server '{}'", server.name))?;
                } else {
                    registry
                        .register(tool)
                        .with_context(|| format!("registering tools from HTTP MCP server '{}'", server.name))?;
                }
            }
            tracing::info!(name = %server.name, tools = n, "HTTP MCP server connected");
            mcp_backends.push(backend);

            // HTTP servers are externally isolated — no sandbox fields to populate.
            server_enforcements.push(surfaces::ServerEnforcement {
                name:      server.name.clone(),
                transport: "http".to_string(),
                ..Default::default()
            });
            continue;
        }

        // ── stdio transport path ───────────────────────────────────────────────

        // caps_to_rules() may return an empty vec (e.g. capabilities=[{Spawn},{Net}])
        // when only spawn/net caps are present with no FS rules.
        // Treat empty rules the same as None: no kernel mechanism is installed, so
        // emitting SandboxApplied would be misleading. filter(non-empty) collapses it.
        let sandbox_rules: Option<Vec<SandboxRule>> = server.capabilities
            .as_deref()
            .map(caps_to_rules)
            .filter(|r| !r.is_empty());

        if sandbox_rules.is_none() {
            let reason = if server.capabilities.is_none() {
                "no capabilities field"
            } else {
                "capabilities produce no effective rules"
            };
            tracing::warn!(
                name = %server.name,
                reason,
                "MCP server running unsandboxed"
            );
            recorder.record(
                "agentd",
                None,
                EventKind::SandboxSkipped,
                serde_json::json!({ "server": server.name, "reason": reason }),
            );
        }

        // Compile sandbox rules in the parent before fork so apply_compiled() in
        // the child's pre_exec closure can be allocation-free (raw syscalls only).
        let compiled: Option<CompiledSandbox> = match sandbox_rules {
            Some(ref rules) => Some(
                sandbox::compile(rules)
                    .with_context(|| format!("compiling sandbox for '{}'", server.name))?,
            ),
            None => None,
        };

        // gVisor mode: transform command to `runsc do [--network=none] -- <cmd> [args]`.
        // Landlock/seccomp/namespace pre_exec is skipped — gVisor's Sentry handles isolation.
        let is_gvisor = server.isolation == config::IsolationMode::Gvisor;
        let has_net_cap = server.capabilities.as_deref()
            .map(|c| c.iter().any(|cap| matches!(cap, Capability::Net { .. })))
            .unwrap_or(true); // absent capabilities field = unrestricted (net allowed)
        let (effective_cmd, effective_args, effective_compiled): (&str, Vec<String>, Option<sandbox::CompiledSandbox>) =
            if is_gvisor {
                let mut gv_args = vec!["do".to_string()];
                if !has_net_cap { gv_args.push("--network=none".to_string()); }
                gv_args.push("--".to_string());
                gv_args.push(server.command.clone());
                gv_args.extend_from_slice(&server.args);
                ("runsc", gv_args, None)
            } else {
                (server.command.as_str(), server.args.clone(), compiled)
            };

        // Read enforcement status before consuming the compiled sandbox (Linux only).
        #[cfg(target_os = "linux")]
        let enforcement = effective_compiled.as_ref().map(|c| c.enforcement_status());

        let had_sandbox = effective_compiled.is_some() || is_gvisor;
        any_sandbox_applied |= had_sandbox;
        tracing::info!(
            name = %server.name,
            command = %server.command,
            sandboxed = had_sandbox,
            isolation = ?server.isolation,
            "spawning MCP server"
        );
        // Build credential env: AGENTD_CREDENTIAL_GATEWAY_URL + AGENTD_CREDENTIAL_TOKEN.
        // Issued only when the credential gateway is enabled. The token's allowed_providers
        // list mirrors the Credential capabilities on this server. None (no capabilities
        // field) means no credential access — providers must be granted explicitly.
        let credential_env: std::collections::HashMap<String, String> =
            if let (Some(ref gw), Some(ref gw_url)) = (&maybe_cred_gw, &cred_gw_url) {
                let allowed = credential_allowed_providers(&server.capabilities);
                let token = uuid::Uuid::new_v4().to_string();
                // ar-07: attribute the token to the owning agent principal.
                // In multi-agent mode mcp_servers is a flat shared pool — use "shared"
                // sentinel to avoid false attribution to the first agent only.
                let owning_agent = owning_agent_id(&agent_cfgs, &server.name);
                gw.register_token(token.clone(), owning_agent, allowed).await;
                cred_tokens.push(token.clone());
                let mut env = std::collections::HashMap::new();
                env.insert("AGENTD_CREDENTIAL_GATEWAY_URL".to_string(), gw_url.clone());
                env.insert("AGENTD_CREDENTIAL_TOKEN".to_string(), token);
                env
            } else {
                std::collections::HashMap::new()
            };

        let (client, specs) = McpClient::spawn(
            effective_cmd,
            &effective_args,
            effective_compiled,
            &server.env,
            &server.passenv,
            &credential_env,
        )
        .await
        .with_context(|| format!("spawning MCP server '{}'", server.name))?;

        // Audit which passenv names were forwarded, blocked, or absent.
        if !server.passenv.is_empty() {
            let mut forwarded = Vec::new();
            let mut blocked = Vec::new();
            let mut absent = Vec::new();
            for name in &server.passenv {
                if PASSENV_BLOCKLIST.contains(&name.as_str()) {
                    blocked.push(name.as_str());
                } else if std::env::var(name).is_ok() {
                    forwarded.push(name.as_str());
                } else {
                    absent.push(name.as_str());
                }
            }
            recorder.record(
                "agentd",
                None,
                EventKind::McpPassenvForwarded,
                serde_json::json!({
                    "server":    server.name,
                    "forwarded": forwarded,
                    "blocked":   blocked,
                    "absent":    absent,
                }),
            );
        }

        // Record SandboxApplied after spawn succeeds.
        // On Linux: emit full enforcement detail.
        // On non-Linux: emit SandboxSkipped (no kernel mechanisms active).
        #[cfg(target_os = "linux")]
        {
            let isolation_str = if is_gvisor { "gvisor" } else { "none" };
            if is_gvisor {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::SandboxApplied,
                    serde_json::json!({
                        "server":    server.name,
                        "isolation": isolation_str,
                        "enforced":  { "mode": "gvisor" },
                    }),
                );
            } else if let Some(ref enf) = enforcement {
                // On non-x86_64, DenySpawn compiles to nothing (seccomp is x86_64-only).
                // If DenySpawn was the only effective rule, the compiled sandbox is a
                // complete no-op — emitting SandboxApplied with all-false fields would
                // mislead operators. Emit SandboxSkipped instead.
                let noop_deny_spawn = is_noop_deny_spawn(
                    enf,
                    sandbox_rules.as_deref().is_some_and(|r| {
                        r.iter().any(|rule| matches!(rule, SandboxRule::DenySpawn))
                    }),
                );
                if noop_deny_spawn {
                    recorder.record(
                        "agentd",
                        None,
                        EventKind::SandboxSkipped,
                        serde_json::json!({
                            "server": server.name,
                            "reason": "deny-spawn-unsupported-arch",
                        }),
                    );
                } else {
                    recorder.record(
                        "agentd",
                        None,
                        EventKind::SandboxApplied,
                        serde_json::json!({
                            "server":    server.name,
                            "isolation": isolation_str,
                            "enforced": {
                                "landlock":          enf.landlock,
                                "seccomp":           enf.seccomp,
                                "spawn_enforcement": enf.spawn_enforcement,
                                "namespace_net":     enf.namespace_net,
                                "namespace_mount":   enf.namespace_mount,
                                "landlock_net":      enf.landlock_net,
                            },
                        }),
                    );
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        if had_sandbox {
            recorder.record(
                "agentd",
                None,
                EventKind::SandboxSkipped,
                serde_json::json!({ "server": server.name, "reason": "non-Linux platform" }),
            );
        }
        let client: Arc<dyn McpBackend> = client;
        let n = specs.len();
        for spec in specs {
            let tool = Box::new(McpTool::new(Arc::clone(&client), spec, server.name.clone()));
            if server.tool_override {
                registry
                    .register_override(tool)
                    .with_context(|| format!("registering (override) tools from MCP server '{}'", server.name))?;
            } else {
                registry
                    .register(tool)
                    .with_context(|| format!("registering tools from MCP server '{}'", server.name))?;
            }
        }
        tracing::info!(name = %server.name, tools = n, "MCP server connected");
        mcp_backends.push(client);

        // Build per-server enforcement record for the snapshot surface.
        let isolation_str = if is_gvisor { "gvisor" } else { "none" }.to_string();
        #[cfg(target_os = "linux")]
        let se = {
            if is_gvisor {
                // gVisor: native kernel mechanisms not applied; isolation handled by Sentry.
                surfaces::ServerEnforcement {
                    name:              server.name.clone(),
                    transport:         "stdio".to_string(),
                    isolation:         isolation_str,
                    landlock:          false,
                    seccomp:           false,
                    spawn_enforcement: "none".to_string(),
                    namespace_net:     false,
                    namespace_mount:   false,
                    landlock_net:      false,
                }
            } else if let Some(ref enf) = enforcement {
                // Check degradations from enforcement status.
                // Pre-V4 net-cap: deny-all fallback is active (safer than unrestricted).
                let has_net_ports = server.capabilities.as_deref()
                    .map(|c| c.iter().any(|cap| {
                        if let agentd::capability::Capability::Net { ports, .. } = cap {
                            !ports.is_empty()
                        } else { false }
                    }))
                    .unwrap_or(false);
                if has_net_ports && !enf.landlock_net {
                    degradation_set.insert("landlock_net_unavailable".to_string());
                }
                let has_deny_spawn = sandbox_rules.as_deref()
                    .is_some_and(|r| r.iter().any(|rule| matches!(rule, SandboxRule::DenySpawn)));
                if has_deny_spawn && enf.spawn_enforcement == "none" {
                    degradation_set.insert("spawn_enforcement_unavailable_arch".to_string());
                }
                surfaces::ServerEnforcement {
                    name:              server.name.clone(),
                    transport:         "stdio".to_string(),
                    isolation:         isolation_str,
                    landlock:          enf.landlock,
                    seccomp:           enf.seccomp,
                    spawn_enforcement: enf.spawn_enforcement.to_string(),
                    namespace_net:     enf.namespace_net,
                    namespace_mount:   enf.namespace_mount,
                    landlock_net:      enf.landlock_net,
                }
            } else {
                surfaces::ServerEnforcement {
                    name:      server.name.clone(),
                    transport: "stdio".to_string(),
                    isolation: isolation_str,
                    ..Default::default()
                }
            }
        };
        #[cfg(not(target_os = "linux"))]
        let se = surfaces::ServerEnforcement {
            name:      server.name.clone(),
            transport: "stdio".to_string(),
            isolation: isolation_str,
            ..Default::default()
        };
        server_enforcements.push(se);
    }

    let tool_names = registry.tool_names();
    recorder.record(
        "agentd",
        None,
        EventKind::ToolsRegistered,
        serde_json::json!({ "tools": tool_names }),
    );
    tracing::info!(tools = ?tool_names, "tools registered");

    // Warn when tool_override is active on a server whose shadowed tools (kb_put/kb_get/kb_search)
    // require KbRead/KbWrite, but the agent's cap-set has KbRead/KbWrite without a matching Mcp grant.
    // Agents in that state would hit a CapabilityDenied because the MCP tool's required_capability_for
    // returns Mcp{server=...}, not KbRead/KbWrite.
    for server in cfg.tools.mcp_servers.iter().filter(|s| s.tool_override) {
        for ac in &agent_cfgs {
            if let Some(ref caps) = ac.capabilities {
                let has_kb_cap = caps.iter().any(|c| {
                    matches!(c, agentd::capability::Capability::KbRead { .. }
                                | agentd::capability::Capability::KbWrite { .. })
                });
                let has_mcp_grant = caps.iter().any(|c| {
                    matches!(c, agentd::capability::Capability::Mcp { server: srv, .. } if srv == &server.name)
                });
                if has_kb_cap && !has_mcp_grant {
                    tracing::warn!(
                        agent = %ac.id,
                        server = %server.name,
                        "tool_override: agent has KbRead/KbWrite but no Mcp grant for server '{}'; \
                         native kb tools are shadowed — add '{{ Mcp = {{ server = \"{}\" }} }}' \
                         to this agent's capabilities",
                        server.name, server.name
                    );
                }
            }
        }
    }

    // Emit AgentSpawned per agent before any API calls so startup events are
    // always present in the flight log even if gateway init fails.
    for ac in &agent_cfgs {
        recorder.record(
            &ac.id,
            None,
            EventKind::AgentSpawned,
            serde_json::json!({
                "model":        cfg.model.model,
                "provider":     cfg.model.provider,
                "max_tokens":   cfg.model.max_tokens,
                "max_turns":    ac.max_turns,
                "token_budget": ac.token_budget,
                "task_preview": truncate(&ac.task, PREVIEW_CHARS),
                "native_tools": cfg.tools.native,
                "mcp_servers":  cfg.tools.mcp_servers.len(),
            }),
        );
        tracing::info!(agent = %ac.id, model = %cfg.model.model, "agent spawned");
    }

    let snapshot: Arc<RwLock<SchedulerSnapshot>> =
        Arc::new(RwLock::new(SchedulerSnapshot::default()));

    // Probe device-level isolation capabilities before the FUSE mount so the
    // /agents/system/isolation file is accurate from first access.
    let isolation_caps = agentd::isolation_caps::probe();
    recorder.record(
        "agentd",
        None,
        EventKind::IsolationProbed,
        serde_json::json!({
            "tier":     isolation_caps.tier,
            "arch":     isolation_caps.arch,
            "runsc":    isolation_caps.runsc,
            "landlock": isolation_caps.landlock,
            "seccomp":  isolation_caps.seccomp,
        }),
    );

    // Set static snapshot fields (provider model + sandbox status + isolation caps) once
    // at startup, before the FUSE mount, so the /agents/system/ files are accurate from
    // first access.
    if let Ok(mut snap) = snapshot.write() {
        snap.provider_model = cfg.model.model.clone();
        snap.sandbox = surfaces::SandboxSummary {
            any_sandboxed:  any_sandbox_applied,
            servers:        server_enforcements,
            degradations:   degradation_set.into_iter().collect(),
        };
        snap.isolation_caps = Some(isolation_caps);
    }

    #[cfg(target_os = "linux")]
    let fuse_mountpoint = PathBuf::from("/agents");

    // Control channel: FUSE writes on /agents/control → scheduler's run loop.
    // Created unconditionally so the management API can route inject/spawn on non-Linux.
    let (control_tx, control_rx) = tokio::sync::mpsc::channel::<agentd::control::ControlCommand>(16);

    #[cfg(target_os = "linux")]
    let maybe_session = if no_fuse {
        tracing::info!("FUSE mount skipped (--no-fuse / AGENTOS_NO_FUSE)");
        recorder.record(
            "agentd",
            None,
            EventKind::FuseSkipped,
            serde_json::json!({ "mountpoint": fuse_mountpoint.display().to_string() }),
        );
        None
    } else {
        let fuse_mem_access: Option<Arc<dyn MemoryAccess>> = memory_store_for_distillation
            .as_ref()
            .map(|s| Arc::new(MemoryAccessBridge(Arc::clone(s))) as Arc<dyn MemoryAccess>);
        let fuse_ctrl: Option<surfaces::ControlDispatch> = {
            let tx = control_tx.clone();
            Some(Arc::new(move |bytes: &[u8]| {
                match agentd::control::parse_control_command(bytes) {
                    Err(_) => libc::EINVAL,
                    Ok(cmd) => match tx.try_send(cmd) {
                        Ok(_)                                        => 0,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_))   => libc::EBUSY,
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => libc::EIO,
                    },
                }
            }) as Arc<dyn Fn(&[u8]) -> i32 + Send + Sync>)
        };
        match surfaces::agents_fs::mount(&fuse_mountpoint, Arc::clone(&snapshot), fuse_mem_access, fuse_ctrl) {
            Ok(session) => {
                recorder.record(
                    "agentd",
                    None,
                    EventKind::FuseMounted,
                    serde_json::json!({ "mountpoint": fuse_mountpoint.display().to_string() }),
                );
                Some(session)
            }
            Err(e) => {
                tracing::warn!("FUSE mount failed (continuing without /agents): {e}");
                None
            }
        }
    };
    #[cfg(not(target_os = "linux"))]
    let _ = no_fuse;
    #[cfg(not(target_os = "linux"))]
    let maybe_session: Option<()> = None;

    let gateway = match AnthropicGateway::from_env(&cfg.model.model)
        .context("initializing Anthropic gateway")
    {
        Ok(gw) => Arc::new(gw),
        Err(e) => {
            for client in &mcp_backends {
                client.shutdown().await;
            }
            return Err(e);
        }
    };
    let registry = Arc::new(registry);

    // ── p7.5 egress mediator ─────────────────────────────────────────────────
    let evidence_path = {
        let p = std::path::Path::new(&cfg.egress.evidence_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(p)
        }
    };
    let egress_key_path = {
        let p = std::path::Path::new(&cfg.egress.key_path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .join(p)
        }
    };
    // OV-1 (p5.8 pattern): evidence file must not fall inside any MCP server's FsWrite prefix.
    // A sandboxed server with write access to evidence_path could tamper with the receipt chain.
    {
        let norm_ev = normalize_path(&evidence_path);
        for srv in &cfg.tools.mcp_servers {
            for cap in srv.capabilities.iter().flatten() {
                let prefix = match cap {
                    Capability::FsWrite { prefix } => prefix,
                    _ => continue,
                };
                let norm_prefix = normalize_path(std::path::Path::new(prefix));
                if norm_prefix.as_os_str().is_empty() {
                    continue;
                }
                anyhow::ensure!(
                    !norm_ev.starts_with(&norm_prefix),
                    "evidence file {} falls inside MCP server {:?}'s FsWrite sandbox prefix {}; \
                     move the evidence file outside all server FS write prefixes \
                     (e.g. set evidence_path = \"/run/evidence.jsonl\" in [egress])",
                    norm_ev.display(),
                    srv.name,
                    norm_prefix.display()
                );
            }
        }
    }
    // OV-1 (S1 / cred.3.1): signing key must not fall inside any MCP server's FsRead prefix.
    // An MCP server that can read egress_key_path can exfiltrate the Ed25519 private key and
    // forge the receipt chain.
    {
        let norm_key = normalize_path(&egress_key_path);
        for srv in &cfg.tools.mcp_servers {
            for cap in srv.capabilities.iter().flatten() {
                let prefix = match cap {
                    Capability::FsRead { prefix } => prefix,
                    _ => continue,
                };
                let norm_prefix = normalize_path(std::path::Path::new(prefix));
                if norm_prefix.as_os_str().is_empty() {
                    continue;
                }
                anyhow::ensure!(
                    !norm_key.starts_with(&norm_prefix),
                    "egress signing key {} falls inside MCP server {:?}'s FsRead sandbox prefix {}; \
                     move key_path outside all server FS read prefixes \
                     (e.g. set key_path = \"/run/egress/signing.key\" in [egress])",
                    norm_key.display(),
                    srv.name,
                    norm_prefix.display()
                );
            }
        }
    }
    let evidence_writer = match agentd::evidence::EvidenceWriter::open(&evidence_path, &egress_key_path) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            for client in &mcp_backends {
                client.shutdown().await;
            }
            return Err(e.context("initializing egress evidence writer (fail-closed)"));
        }
    };
    // Capture the real API key before overwriting env with the placeholder.
    // The proxy uses this key for upstream forwarding; the key never leaves agentd's
    // memory and is never written to disk or logged.
    let real_api_key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
    // Fail fast: if the egress proxy is configured but ANTHROPIC_API_KEY is unset,
    // there is no key to forward — reject clearly rather than silently proxying with
    // an empty credential.
    if cfg.egress.proxy_addr.is_some() && real_api_key.is_empty() {
        anyhow::bail!(
            "egress proxy configured but ANTHROPIC_API_KEY is unset — \
             set the key or remove [egress] proxy_addr from config"
        );
    }
    // Overwrite ANTHROPIC_API_KEY with a placeholder after the gateway has
    // captured the real key into its field. Native agents never see real credentials.
    // Safety: called before scheduler.run() (i.e. before any agent tasks are spawned),
    // so no concurrent threads are reading env vars at this point.
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-PLACEHOLDER-agentd");
    let egress_proxy = Arc::new(agentd::egress::EgressProxy::new(
        Arc::clone(&evidence_writer),
        Arc::clone(&recorder),
    ));
    let proxy_registry = Arc::new(agentd::egress::ProxyRegistry::new());
    let egress_bound_addr: Option<std::net::SocketAddr> = if let Some(ref addr) = cfg.egress.proxy_addr {
        match agentd::egress::start_http_proxy(
            addr,
            real_api_key,
            Arc::clone(&proxy_registry),
            Arc::clone(&recorder),
            Arc::clone(&evidence_writer),
        ).await {
            Ok(bound) => {
                tracing::info!(addr = %bound, "egress proxy started");
                Some(bound)
            }
            Err(e) => {
                for client in &mcp_backends {
                    client.shutdown().await;
                }
                return Err(e.context("egress proxy failed to bind (fail-closed)"));
            }
        }
    } else {
        None
    };
    // ─────────────────────────────────────────────────────────────────────────

    // ── p7.7 management HTTP API ─────────────────────────────────────────────
    if cfg.management.enabled {
        // Wire control_tx unconditionally — management API needs it on all platforms (orch.1).
        let mgmt_control_tx = Some(control_tx.clone());

        match agentd::management::start(
            &cfg.management.bind_addr,
            cfg.management.port,
            cfg.management.allow_non_loopback,
            Arc::clone(&snapshot),
            memory_store_for_management,
            runs_store_for_management,
            broadcast_tx.clone(),
            Arc::clone(&recorder),
            mgmt_control_tx,
            maybe_cred_gw.clone(),
            // ux.12: route-scoped approval-token secret from env (secrets-from-env invariant).
            // When set, gates POST /api/v1/approvals/*/{approve,deny}; the Telegram sidecar
            // and agentctl must send the matching X-Approval-Token.
            std::env::var("AGENTOS_APPROVAL_SECRET").ok().filter(|s| !s.is_empty()),
        ).await {
            Ok(bound) => {
                tracing::info!(addr = %bound, "management API started");
            }
            Err(e) => {
                for client in &mcp_backends {
                    client.shutdown().await;
                }
                return Err(e.context("management API failed to bind (fail-closed)"));
            }
        }
    }
    // ─────────────────────────────────────────────────────────────────────────

    // Attempt to restore from a prior checkpoint. On corrupt file: rename and start fresh.
    let store = CheckpointStore::new(std::path::Path::new("."));
    let maybe_checkpoint = match store.load() {
        Ok(Some(cp)) => {
            tracing::info!(agents = cp.agents.len(), "restoring from checkpoint");
            for agent_cp in &cp.agents {
                recorder.record(
                    &agent_cp.agent_id,
                    Some(agent_cp.turn),
                    EventKind::AgentRestored,
                    serde_json::json!({ "turn": agent_cp.turn }),
                );
            }
            // AUDIT-v0.97 P2-1: rename (not delete) the checkpoint after load, so a crash
            // AFTER restore but BEFORE the first new save is recoverable from the .restored
            // copy (load() falls back to it). Deleting here meant a deterministic startup
            // crash after restore erased all CoS state on the next boot.
            if let Err(e) = store.mark_restored() {
                tracing::warn!("could not rename checkpoint.json to .restored after restore: {e}");
            }
            Some(cp)
        }
        Ok(None) => None,
        Err(e) => {
            // load() already quarantined the actual bad source (primary or .restored) to
            // <name>.corrupt (AUDIT-v0.97 P2-1 review) — just log and start fresh.
            tracing::error!("checkpoint unreadable, starting fresh (bad file quarantined to *.corrupt): {e:#}");
            None
        }
    };

    let scheduler = match Scheduler::new(
        agent_cfgs,
        &cfg.model,
        cfg.scheduler,
        gateway,
        registry,
        Arc::clone(&recorder),
        Arc::clone(&snapshot),
        maybe_checkpoint,
    ) {
        Ok(s) => s,
        Err(e) => {
            for client in &mcp_backends {
                client.shutdown().await;
            }
            return Err(e);
        }
    };

    // Wire p5.6 distillation if enabled.
    let scheduler = if cfg.memory.distill_on_complete {
        if let Some(store) = memory_store_for_distillation {
            scheduler.with_distillation(store)
        } else {
            scheduler
        }
    } else {
        scheduler
    };

    // Wire the control receiver when at least one consumer is active: FUSE writes
    // /agents/control, and the management API routes HTTP approve/deny/inject commands.
    // Without a consumer the scheduler would hang waiting for commands after all agents finish.
    let scheduler = if maybe_session.is_some() || cfg.management.enabled {
        scheduler.with_control(control_rx)
    } else {
        scheduler
    };

    let scheduler = scheduler
        .with_egress(egress_proxy)
        .with_egress_addr(egress_bound_addr)
        .with_proxy_registry(proxy_registry);

    // cred.5: wire credential gateway so per-agent grant data flows into the snapshot.
    let scheduler = if let Some(ref gw) = maybe_cred_gw {
        scheduler.with_credential_gateway(Arc::clone(gw))
    } else {
        scheduler
    };

    // ux.11b: wire the run-history tracker so lifecycle transitions author runs.redb.
    let scheduler = scheduler.with_run_tracker(run_tracker);

    // cap.2b: register config-declared sealed jobs so run_job(job_id) can materialize them.
    let scheduler = scheduler.with_jobs(cfg.jobs.clone());

    let streamed_agents = scheduler.streamed_agents();
    recorder.record(
        "agentd",
        None,
        EventKind::SchedulerStarted,
        serde_json::json!({ "run_id": run_id, "config_hash": config_hash }),
    );
    let outcomes = scheduler.run().await;
    recorder.record(
        "agentd",
        None,
        EventKind::SchedulerStopped,
        serde_json::json!({ "run_id": run_id, "agent_count": outcomes.len() }),
    );

    // Drain queued run-history writes before exit (ship review): scheduler.run()
    // dropped the RunTracker (sender), so the writer finishes once it applies the
    // remaining events. Bounded wait so a wedged writer can't hang shutdown.
    if let Some(handle) = run_writer_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }

    #[cfg(target_os = "linux")]
    if let Some(session) = maybe_session {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(session)));
        recorder.record(
            "agentd",
            None,
            EventKind::FuseUnmounted,
            serde_json::json!({ "mountpoint": fuse_mountpoint.display().to_string() }),
        );
    }

    // Deregister credential tokens before MCP server shutdown so any last-gasp
    // requests from shutting-down servers receive 401 rather than making
    // broker calls with a token whose agent no longer exists.
    if let Some(ref gw) = maybe_cred_gw {
        for token in &cred_tokens {
            gw.deregister_token(token).await;
        }
    }

    for client in &mcp_backends {
        client.shutdown().await;
    }

    let mut any_failed = false;
    for (id, result) in &outcomes {
        match result {
            Ok(answer) => {
                // Suppress if this agent already wrote its answer via streaming stdout.
                let was_streamed = streamed_agents
                    .lock()
                    .map(|s| s.contains(id.as_str()))
                    .unwrap_or(false);
                if !was_streamed {
                    println!("{answer}");
                }
            }
            Err(e) => {
                tracing::error!(agent = %id, error = %e, "agent failed");
                any_failed = true;
            }
        }
    }

    if any_failed {
        // Return Err so main() calls exit() after run_agent has returned and
        // mcp_backends has been dropped — process::exit skips destructors.
        anyhow::bail!("one or more agents failed");
    }
    Ok(())
}

/// Extract the value of `--log-path <path>` from raw CLI args, if present.
fn parse_log_path(args: &[String]) -> Option<PathBuf> {
    args.windows(2)
        .find(|w| w[0] == "--log-path")
        .map(|w| PathBuf::from(&w[1]))
}

/// Strip recognised flag/value pairs from args and return the positional remainder.
/// Flags consumed: `--no-fuse` (bare), `--log-path <value>` (consumes two tokens).
fn filter_positional_args(args: &[String]) -> Vec<String> {
    let mut skip_next = false;
    args.iter()
        .filter_map(|a| {
            if skip_next { skip_next = false; return None; }
            if a == "--no-fuse" { return None; }
            if a == "--strict" { return None; } // cap.1: `agentd check --strict` flag, not a positional
            if a == "--log-path" { skip_next = true; return None; }
            Some(a.clone())
        })
        .collect()
}

/// Resolve the flight log path: CLI override > TOML `log_path` field > default.
fn resolve_log_path(cli_override: Option<PathBuf>, toml_path: Option<&str>) -> PathBuf {
    cli_override
        .or_else(|| toml_path.map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("flight.jsonl"))
}

/// Return true when a compiled sandbox is a complete no-op because the only
/// requested rule was `DenySpawn` on a non-x86_64 arch (seccomp is x86_64-only,
/// so nothing was actually installed). Emitting `SandboxApplied` with all-false
/// fields in that case would mislead operators; callers should emit `SandboxSkipped`.
///
/// `has_rules` should be `true` when the original `sandbox_rules` slice was
/// non-empty (i.e. DenySpawn was requested but produced no kernel mechanism).
#[cfg(any(test, target_os = "linux"))]
fn is_noop_deny_spawn(enf: &sandbox::EnforcementStatus, has_rules: bool) -> bool {
    !enf.landlock
        && !enf.landlock_net
        && !enf.seccomp
        && !enf.namespace_net
        && !enf.namespace_mount
        && enf.spawn_enforcement == "none"
        && has_rules
}

/// Format the diagnostic message emitted when Landlock ABI V4 is unavailable.
/// Reports the actual detected ABI version rather than inferring the kernel version,
/// because a kernel ≥ 6.7 may still lack V4 if compiled without full Landlock support.
fn net_landlock_v4_unavailable_message(ports: &[u16]) -> String {
    let abi = sandbox::landlock_abi_version();
    format!(
        "Net{{ports={ports:?}}} declared but Landlock ABI V4 unavailable (detected ABI: v{abi}); \
         per-port enforcement skipped — server has unrestricted network access. \
         ABI V4 (Linux ≥ 6.7, CONFIG_SECURITY_LANDLOCK=y) required for port-level isolation."
    )
}

/// Convert an agent capability set into sandbox rules for an MCP server subprocess.
///
/// Landlock FS rules map 1:1 from FsRead/FsWrite capabilities. DenySpawn is
/// added whenever the Spawn capability is absent. IsolateNetwork is added
/// whenever the Net capability is absent — enforcing network isolation at the
/// Linux namespace level for servers that don't need outbound access.
///
/// When `Net{ports}` is declared but the kernel does not support Landlock V4,
/// port-level enforcement is impossible. Best-effort ALLOW: skip IsolateNetwork
/// so the server retains network access, preserving the operator's declared intent.
/// Landlock FS degrades the same way.
fn caps_to_rules(caps: &[Capability]) -> Vec<SandboxRule> {
    let v4_available = sandbox::landlock_v4_available();
    caps_to_rules_inner(caps, v4_available)
}

fn caps_to_rules_inner(caps: &[Capability], v4_available: bool) -> Vec<SandboxRule> {
    let mut rules = Vec::new();
    let has_spawn = caps.iter().any(|c| matches!(c, Capability::Spawn | Capability::ShellExec));
    let has_net   = caps.iter().any(|c| matches!(c, Capability::Net { .. }));
    if !has_spawn {
        rules.push(SandboxRule::DenySpawn);
    }
    if !has_net {
        rules.push(SandboxRule::IsolateNetwork);
    }
    for cap in caps {
        match cap {
            Capability::FsRead { prefix } => {
                rules.push(SandboxRule::AllowFsRead { prefix: prefix.clone() });
            }
            Capability::FsWrite { prefix } => {
                rules.push(SandboxRule::AllowFsWrite { prefix: prefix.clone() });
            }
            Capability::Net { ports, .. } => {
                if ports.is_empty() {
                    // Net{} with no ports: network unrestricted (explicit no-isolation).
                    continue;
                }
                if !v4_available {
                    // Landlock V4 unavailable: per-port enforcement impossible.
                    // Best-effort ALLOW: do not push IsolateNetwork. The server retains
                    // unrestricted network access rather than getting deny-all, preserving
                    // the operator's declared intent. Landlock FS degrades the same way.
                    tracing::warn!("{}", net_landlock_v4_unavailable_message(ports));
                    continue;
                }
                // V4 available: emit per-port AllowNetConnect rules.
                // Port 0 is not a valid TCP port; Landlock returns EINVAL for it.
                for &port in ports {
                    if port == 0 {
                        tracing::warn!("Net capability: port 0 is not a valid TCP port and will be ignored");
                        continue;
                    }
                    rules.push(SandboxRule::AllowNetConnect { port });
                }
            }
            // Mcp/KbRead/KbWrite/ShellExec/Credential/RunsRead/BriefPublish are
            // agent-level or broker-handled; no sandbox rule.
            Capability::Mcp { .. }
            | Capability::Spawn
            | Capability::ShellExec
            | Capability::KbRead { .. }
            | Capability::KbWrite { .. }
            | Capability::Credential { .. }
            | Capability::RunsRead
            | Capability::BriefPublish
            | Capability::RunJob => {}
        }
    }
    rules
}

async fn run_probe(prompt: &str, log_path: PathBuf) -> anyhow::Result<()> {
    use agentd::inference::{Block, InferenceGateway, InferenceRequest, Msg, Role};

    let model = "claude-sonnet-4-6";
    let recorder = FlightRecorder::new(&log_path)?;

    let gateway = AnthropicGateway::from_env(model).context("initializing Anthropic gateway")?;

    recorder.record(
        "probe",
        None,
        EventKind::InferenceRequest,
        serde_json::json!({
            "model": gateway.model_id(),
            "msg_count": 1,
            "tool_count": 0,
        }),
    );

    tracing::info!(
        model,
        prompt = %prompt.chars().take(PREVIEW_CHARS).collect::<String>(),
        "probe: sending request"
    );

    let request = InferenceRequest {
        system: None,
        messages: vec![Msg {
            role: Role::User,
            blocks: vec![Block::Text {
                text: prompt.to_string(),
            }],
        }],
        tools:      vec![],
        max_tokens: 4096,
        streaming:  false,
    };

    let response = match gateway.infer(request).await {
        Ok(r) => r,
        Err(e) => {
            recorder.record(
                "probe",
                None,
                EventKind::Error,
                serde_json::json!({"stage": "inference", "error": e.to_string()}),
            );
            return Err(e);
        }
    };

    let preview: String = response
        .blocks
        .iter()
        .find_map(|b| {
            if let Block::Text { text } = b {
                Some(text.chars().take(PREVIEW_CHARS).collect())
            } else {
                None
            }
        })
        .unwrap_or_default();

    recorder.record(
        "probe",
        None,
        EventKind::InferenceResponse,
        serde_json::json!({
            "stop_reason": response.stop_reason.as_str(),
            "input_tokens": response.input_tokens,
            "output_tokens": response.output_tokens,
            "preview": preview,
        }),
    );

    tracing::info!(
        input_tokens = response.input_tokens,
        output_tokens = response.output_tokens,
        stop_reason = response.stop_reason.as_str(),
        "probe: response received"
    );

    for block in &response.blocks {
        if let Block::Text { text } = block {
            println!("{text}");
        }
    }

    Ok(())
}

/// Derive a credential broker token's `allowed_providers` list from an MCP server's
/// OWN `capabilities` field (not the owning agent's capabilities list, which grants
/// `Credential` separately for a different check — `Mcp{server=...}` tool visibility).
/// `None` (no capabilities field) means no credential access at all — deny-by-default.
///
/// Found by /review (testing specialist, 2026-07-15): this logic previously lived only
/// inline in the credential_env construction below, duplicated by hand in
/// `none_capabilities_yields_empty_credential_providers` — neither the production
/// closure nor the pre-existing capability-declaration tests actually exercised each
/// other, so a regression here could ship undetected. Extracted so both do.
fn credential_allowed_providers(caps: &Option<Vec<Capability>>) -> Vec<String> {
    match caps {
        None => vec![],
        Some(cap_list) => cap_list
            .iter()
            .filter_map(|cap| {
                if let Capability::Credential { provider } = cap {
                    // Shared key derivation (cap.1) — `agentd check`'s wiring cross-check calls
                    // the same function, so linter and broker can never disagree.
                    Some(agentd::capability::credential_provider_key(provider))
                } else {
                    None
                }
            })
            .collect(),
    }
}

/// Return the agent principal to attribute a shared MCP server credential token to.
///
/// ar-07: tokens must be attributed to the agent principal, not the MCP server name.
/// In single-agent mode there is exactly one agent. In multi-agent mode `mcp_servers`
/// is a flat shared pool so no single agent owns a server — use "shared" sentinel.
fn owning_agent_id(agent_cfgs: &[config::AgentConfig], server_name: &str) -> String {
    match agent_cfgs {
        [] => server_name.to_owned(),
        [single] => single.id.clone(),
        _ => {
            tracing::warn!(
                server = %server_name,
                agents = %agent_cfgs.len(),
                "credential token attributed to 'shared' — mcp_servers pool is global in \
                 multi-agent mode; per-agent attribution requires per-agent tools sections"
            );
            "shared".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentd::capability::Capability;
    use sandbox::SandboxRule;

    #[test]
    fn caps_to_rules_empty_caps_yields_deny_spawn_and_isolate_network() {
        let rules = caps_to_rules(&[]);
        assert!(rules.contains(&SandboxRule::DenySpawn),       "empty caps → DenySpawn");
        assert!(rules.contains(&SandboxRule::IsolateNetwork),  "empty caps → IsolateNetwork");
    }

    #[test]
    fn caps_to_rules_spawn_cap_removes_deny_spawn() {
        let rules = caps_to_rules(&[Capability::Spawn]);
        assert!(!rules.contains(&SandboxRule::DenySpawn),     "Spawn cap → no DenySpawn");
        assert!(rules.contains(&SandboxRule::IsolateNetwork), "Spawn cap alone → still IsolateNetwork");
    }

    #[test]
    fn caps_to_rules_fs_read_maps_correctly() {
        let rules = caps_to_rules(&[Capability::FsRead { prefix: "/workspace".into() }]);
        assert!(rules.contains(&SandboxRule::AllowFsRead { prefix: "/workspace".into() }));
        assert!(rules.contains(&SandboxRule::DenySpawn));
        assert!(rules.contains(&SandboxRule::IsolateNetwork));
    }

    #[test]
    fn caps_to_rules_fs_write_maps_correctly() {
        let rules = caps_to_rules(&[Capability::FsWrite { prefix: "/tmp".into() }]);
        assert!(rules.contains(&SandboxRule::AllowFsWrite { prefix: "/tmp".into() }));
        assert!(rules.contains(&SandboxRule::DenySpawn));
        assert!(rules.contains(&SandboxRule::IsolateNetwork));
    }

    #[test]
    fn caps_to_rules_net_cap_permits_network() {
        // Net present (empty ports) → no IsolateNetwork; Mcp is still advisory.
        let rules = caps_to_rules(&[
            Capability::Net { hosts: vec!["example.com".into()], ports: vec![] },
            Capability::Mcp { server: "echo".into(), tools: vec![] },
        ]);
        assert!(rules.contains(&SandboxRule::DenySpawn),        "no Spawn cap → DenySpawn");
        assert!(!rules.contains(&SandboxRule::IsolateNetwork),  "Net cap → no IsolateNetwork");
    }

    #[test]
    fn caps_to_rules_spawn_with_fs_omits_deny_spawn() {
        let rules = caps_to_rules(&[
            Capability::Spawn,
            Capability::FsRead { prefix: "/workspace".into() },
        ]);
        assert!(!rules.contains(&SandboxRule::DenySpawn));
        assert!(rules.contains(&SandboxRule::AllowFsRead { prefix: "/workspace".into() }));
        assert!(rules.contains(&SandboxRule::IsolateNetwork), "no Net cap → still IsolateNetwork");
    }

    #[test]
    fn caps_to_rules_net_and_spawn_both_present_no_isolation_no_deny() {
        let rules = caps_to_rules(&[Capability::Net { hosts: vec![], ports: vec![] }, Capability::Spawn]);
        assert!(!rules.contains(&SandboxRule::DenySpawn),      "Spawn → no DenySpawn");
        assert!(!rules.contains(&SandboxRule::IsolateNetwork), "Net → no IsolateNetwork");
    }

    #[test]
    fn caps_to_rules_net_with_ports_v4_generates_allow_net_connect() {
        // Simulate a V4-capable kernel.
        let rules = caps_to_rules_inner(&[Capability::Net {
            hosts: vec!["api.anthropic.com".into()],
            ports: vec![443],
        }], true);
        assert!(
            rules.contains(&SandboxRule::AllowNetConnect { port: 443 }),
            "Net {{ ports: [443] }} on V4 kernel → AllowNetConnect {{ port: 443 }}"
        );
        assert!(!rules.contains(&SandboxRule::IsolateNetwork), "Net → no IsolateNetwork");
    }

    #[test]
    fn net_ports_v4_unavailable_degrades_to_allow_not_deny() {
        // FAILS without the F4 fix: old code pushes IsolateNetwork, denying all network
        // to an MCP server that explicitly declared it needs outbound access.
        let rules = caps_to_rules_inner(&[Capability::Net {
            hosts: vec!["api.anthropic.com".into()],
            ports: vec![443],
        }], false); // v4_available = false (simulated; e.g. kernel with Landlock ABI < 4)
        assert!(
            !rules.iter().any(|r| matches!(r, SandboxRule::IsolateNetwork)),
            "Net{{ports}} on no-V4 kernel must degrade to allow, not deny-all: {:?}", rules
        );
    }

    #[test]
    fn net_ports_v4_available_emits_allow_connect_both_ports() {
        let rules = caps_to_rules_inner(&[Capability::Net {
            hosts: vec!["api.anthropic.com".into()],
            ports: vec![443, 80],
        }], true);
        assert!(rules.contains(&SandboxRule::AllowNetConnect { port: 443 }));
        assert!(rules.contains(&SandboxRule::AllowNetConnect { port: 80 }));
        assert!(!rules.iter().any(|r| matches!(r, SandboxRule::IsolateNetwork)));
    }

    #[test]
    fn no_net_cap_still_isolates_on_no_v4_kernel() {
        // A server with no Net capability must get IsolateNetwork regardless of V4 availability.
        let rules = caps_to_rules_inner(&[Capability::FsRead { prefix: "/tmp".into() }], false);
        assert!(
            rules.iter().any(|r| matches!(r, SandboxRule::IsolateNetwork)),
            "no Net cap → IsolateNetwork even on no-V4 kernel"
        );
    }

    #[test]
    fn caps_to_rules_net_multiple_ports_generates_multiple_rules() {
        // Simulate V4-capable kernel.
        let rules = caps_to_rules_inner(&[Capability::Net {
            hosts: vec![],
            ports: vec![80, 443],
        }], true);
        assert!(rules.contains(&SandboxRule::AllowNetConnect { port: 80 }));
        assert!(rules.contains(&SandboxRule::AllowNetConnect { port: 443 }));
        assert!(!rules.contains(&SandboxRule::IsolateNetwork));
    }

    #[test]
    fn caps_to_rules_net_empty_ports_no_allow_net_connect() {
        // Backward compat: Net with empty ports must not generate AllowNetConnect.
        let rules = caps_to_rules(&[Capability::Net { hosts: vec![], ports: vec![] }]);
        assert!(
            !rules.iter().any(|r| matches!(r, SandboxRule::AllowNetConnect { .. })),
            "empty ports → no AllowNetConnect rules"
        );
    }

    // ── ShellExec sandbox tests ───────────────────────────────────────────────

    #[test]
    fn caps_to_rules_shell_exec_removes_deny_spawn() {
        let rules = caps_to_rules(&[Capability::ShellExec]);
        assert!(!rules.contains(&SandboxRule::DenySpawn), "ShellExec → no DenySpawn");
        assert!(rules.contains(&SandboxRule::IsolateNetwork), "ShellExec alone → still IsolateNetwork");
    }

    #[test]
    fn caps_to_rules_shell_exec_with_fs_grants() {
        let rules = caps_to_rules(&[
            Capability::ShellExec,
            Capability::FsRead { prefix: "/workspace".into() },
            Capability::FsWrite { prefix: "/tmp".into() },
        ]);
        assert!(!rules.contains(&SandboxRule::DenySpawn), "ShellExec → no DenySpawn");
        assert!(rules.contains(&SandboxRule::AllowFsRead { prefix: "/workspace".into() }));
        assert!(rules.contains(&SandboxRule::AllowFsWrite { prefix: "/tmp".into() }));
        assert!(rules.contains(&SandboxRule::IsolateNetwork), "no Net cap → still IsolateNetwork");
    }

    #[test]
    fn caps_to_rules_shell_exec_with_net_lifts_isolation() {
        let rules = caps_to_rules(&[
            Capability::ShellExec,
            Capability::Net { hosts: vec![], ports: vec![] },
        ]);
        assert!(!rules.contains(&SandboxRule::DenySpawn), "ShellExec → no DenySpawn");
        assert!(!rules.contains(&SandboxRule::IsolateNetwork), "Net → no IsolateNetwork");
    }

    // ── G1: parse_log_path ────────────────────────────────────────────────────

    #[test]
    fn parse_log_path_returns_value_when_flag_present() {
        let args: Vec<String> = ["--log-path", "/tmp/run.jsonl"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_log_path(&args), Some(PathBuf::from("/tmp/run.jsonl")));
    }

    #[test]
    fn parse_log_path_returns_none_when_flag_absent() {
        let args: Vec<String> = ["agent.toml"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_log_path(&args), None);
    }

    #[test]
    fn parse_log_path_returns_none_for_empty_args() {
        assert_eq!(parse_log_path(&[]), None);
    }

    // ── G2: filter_positional_args ────────────────────────────────────────────

    #[test]
    fn filter_positional_args_strips_log_path_and_its_value() {
        let args: Vec<String> = ["--log-path", "/tmp/x.jsonl", "agent.toml"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(filter_positional_args(&args), vec!["agent.toml".to_string()]);
    }

    #[test]
    fn filter_positional_args_strips_no_fuse() {
        let args: Vec<String> = ["--no-fuse", "agent.toml"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(filter_positional_args(&args), vec!["agent.toml".to_string()]);
    }

    #[test]
    fn filter_positional_args_preserves_positional_when_no_flags() {
        let args: Vec<String> = ["agent.toml"].iter().map(|s| s.to_string()).collect();
        assert_eq!(filter_positional_args(&args), vec!["agent.toml".to_string()]);
    }

    // ── G3: resolve_log_path precedence chain ─────────────────────────────────

    #[test]
    fn resolve_log_path_cli_overrides_toml() {
        let result = resolve_log_path(
            Some(PathBuf::from("/cli/path.jsonl")),
            Some("/toml/path.jsonl"),
        );
        assert_eq!(result, PathBuf::from("/cli/path.jsonl"));
    }

    #[test]
    fn resolve_log_path_toml_used_when_no_cli_override() {
        let result = resolve_log_path(None, Some("/toml/path.jsonl"));
        assert_eq!(result, PathBuf::from("/toml/path.jsonl"));
    }

    #[test]
    fn resolve_log_path_default_when_neither_set() {
        let result = resolve_log_path(None, None);
        assert_eq!(result, PathBuf::from("flight.jsonl"));
    }

    // ── G4: is_noop_deny_spawn ────────────────────────────────────────────────
    // EnforcementStatus has all pub fields and is available on all platforms,
    // so these tests run on macOS and Linux alike.

    #[test]
    fn noop_deny_spawn_true_when_all_false_and_has_rules() {
        let enf = sandbox::EnforcementStatus {
            landlock: false,
            seccomp: false,
            spawn_enforcement: "none",
            namespace_net: false,
            namespace_mount: false,
            landlock_net: false,
        };
        assert!(is_noop_deny_spawn(&enf, true),
            "all mechanisms false + rules present → noop DenySpawn");
    }

    #[test]
    fn noop_deny_spawn_false_when_has_rules_is_false() {
        let enf = sandbox::EnforcementStatus {
            landlock: false,
            seccomp: false,
            spawn_enforcement: "none",
            namespace_net: false,
            namespace_mount: false,
            landlock_net: false,
        };
        assert!(!is_noop_deny_spawn(&enf, false),
            "no rules present → not a noop DenySpawn case");
    }

    #[test]
    fn noop_deny_spawn_false_when_seccomp_active() {
        let enf = sandbox::EnforcementStatus {
            landlock: false,
            seccomp: true,
            spawn_enforcement: "fork_vfork_only",
            namespace_net: false,
            namespace_mount: false,
            landlock_net: false,
        };
        assert!(!is_noop_deny_spawn(&enf, true),
            "seccomp active → not a noop; real enforcement applied");
    }

    #[test]
    fn noop_deny_spawn_false_when_landlock_active() {
        let enf = sandbox::EnforcementStatus {
            landlock: true,
            seccomp: false,
            spawn_enforcement: "none",
            namespace_net: false,
            namespace_mount: false,
            landlock_net: false,
        };
        assert!(!is_noop_deny_spawn(&enf, true),
            "landlock active → not a noop; real enforcement applied");
    }

    #[test]
    fn noop_deny_spawn_false_when_namespace_net_active() {
        let enf = sandbox::EnforcementStatus {
            landlock: false,
            seccomp: false,
            spawn_enforcement: "none",
            namespace_net: true,
            namespace_mount: false,
            landlock_net: false,
        };
        assert!(!is_noop_deny_spawn(&enf, true),
            "namespace_net active → not a noop; real enforcement applied");
    }

    #[test]
    fn noop_deny_spawn_false_when_namespace_mount_active() {
        let enf = sandbox::EnforcementStatus {
            landlock: false,
            seccomp: false,
            spawn_enforcement: "none",
            namespace_net: false,
            namespace_mount: true,
            landlock_net: false,
        };
        assert!(!is_noop_deny_spawn(&enf, true),
            "namespace_mount active → not a noop; real enforcement applied");
    }

    #[test]
    fn noop_deny_spawn_false_when_landlock_net_active() {
        let enf = sandbox::EnforcementStatus {
            landlock: false,
            seccomp: false,
            spawn_enforcement: "none",
            namespace_net: false,
            namespace_mount: false,
            landlock_net: true,
        };
        assert!(!is_noop_deny_spawn(&enf, true),
            "landlock_net active (V4 port enforcement) → not a noop; real enforcement applied");
    }

    #[test]
    fn caps_to_rules_net_port_zero_is_skipped() {
        // Simulate V4-capable kernel so port filtering logic is exercised.
        let caps = vec![Capability::Spawn, Capability::Net {
            hosts: vec![],
            ports: vec![0, 443],
        }];
        let rules = caps_to_rules_inner(&caps, true);
        assert!(!rules.iter().any(|r| matches!(r, SandboxRule::AllowNetConnect { port: 0 })),
            "port 0 must be skipped (invalid TCP port)");
        assert!(rules.iter().any(|r| matches!(r, SandboxRule::AllowNetConnect { port: 443 })),
            "valid port 443 must still be included");
    }

    // ── G2 extension: both flags together ────────────────────────────────────

    #[test]
    fn filter_positional_args_handles_both_flags_together() {
        let args: Vec<String> = ["--no-fuse", "--log-path", "/tmp/x.jsonl", "agent.toml"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(filter_positional_args(&args), vec!["agent.toml".to_string()]);
    }

    // ── G1 extension: trailing flag with no value ─────────────────────────────

    #[test]
    fn parse_log_path_returns_none_when_flag_is_last_arg() {
        // --log-path as the final token with no following value: windows(2) won't
        // match, so None is returned silently. This documents the contract.
        let args: Vec<String> = ["agent.toml", "--log-path"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_log_path(&args), None);
    }

    // ── p5.8 startup invariant: store_path must not fall inside MCP FS prefix ─

    #[test]
    fn store_path_inside_sandbox_prefix_fails_startup() {
        use agentd::capability::normalize_path;
        use std::path::{Path, PathBuf};

        // Replicates the startup assertion logic from run() in isolation.
        fn check(store_path_str: &str, prefix_str: &str) -> anyhow::Result<()> {
            let store_path = PathBuf::from(store_path_str);
            let norm_store = normalize_path(&store_path);
            anyhow::ensure!(
                norm_store.is_absolute(),
                "memory.store_path must be an absolute path: {}",
                store_path.display()
            );
            let norm_prefix = normalize_path(Path::new(prefix_str));
            if norm_prefix.as_os_str().is_empty() {
                return Ok(());
            }
            anyhow::ensure!(
                !norm_store.starts_with(&norm_prefix),
                "store {} inside prefix {}",
                norm_store.display(),
                norm_prefix.display()
            );
            Ok(())
        }

        // Case 1: absolute store inside FS prefix → error
        assert!(
            check("/var/run/memory.redb", "/var/run").is_err(),
            "absolute store inside FS prefix must be rejected"
        );

        // Case 2: absolute store outside all FS prefixes → ok
        assert!(
            check("/run/memory/memory.redb", "/tmp/workspace").is_ok(),
            "absolute store outside FS prefixes must be accepted"
        );

        // Case 3: store path with '..' that normalizes into prefix → error
        assert!(
            check("/var/run/../run/memory.redb", "/var/run").is_err(),
            "store path with '..' resolving inside prefix must be rejected"
        );

        // Case 4: empty prefix is skipped; absolute store → ok
        assert!(
            check("/run/memory/memory.redb", "").is_ok(),
            "empty MCP FS prefix must be skipped (not a wildcard match)"
        );
    }

    // ── cred.3.1 (S1) startup invariant: signing key must not fall inside MCP FsRead prefix ──

    #[test]
    fn signing_key_inside_fs_read_prefix_fails_startup() {
        use agentd::capability::normalize_path;
        use std::path::{Path, PathBuf};

        // Replicates the S1 OV-1 guard from run() in isolation.
        fn check(key_path_str: &str, prefix_str: &str) -> anyhow::Result<()> {
            let norm_key = normalize_path(&PathBuf::from(key_path_str));
            let norm_prefix = normalize_path(Path::new(prefix_str));
            if norm_prefix.as_os_str().is_empty() {
                return Ok(());
            }
            anyhow::ensure!(
                !norm_key.starts_with(&norm_prefix),
                "signing key {} inside FsRead prefix {}",
                norm_key.display(),
                norm_prefix.display()
            );
            Ok(())
        }

        // Case 1: key inside MCP FsRead prefix → rejected
        assert!(
            check("/run/egress/signing.key", "/run/egress").is_err(),
            "signing key inside FsRead prefix must be rejected"
        );

        // Case 2: key outside all MCP FsRead prefixes → accepted
        assert!(
            check("/run/egress/signing.key", "/tmp/workspace").is_ok(),
            "signing key outside FsRead prefix must be accepted"
        );

        // Case 3: key path with '..' that normalizes into prefix → rejected
        assert!(
            check("/run/egress/../egress/signing.key", "/run/egress").is_err(),
            "signing key path with '..' resolving inside prefix must be rejected"
        );

        // Case 4: empty prefix is skipped → accepted
        assert!(
            check("/run/egress/signing.key", "").is_ok(),
            "empty MCP FsRead prefix must be skipped"
        );
    }

    // ── cred.3.2: owning_agent_id attribution (ar-07 wiring + multi-agent sentinel) ──
    //
    // ar-07 fix: token must be attributed to agent ID, not server name.
    // Multi-agent case: when mcp_servers is a flat shared pool, all tokens must use
    // the "shared" sentinel rather than silently attributing all accesses to agent[0].
    // This test FAILS if owning_agent_id() reverts to using agent_cfgs.first() always.

    fn make_agent_cfg(id: &str) -> config::AgentConfig {
        toml::from_str(&format!("id = \"{id}\"")).unwrap()
    }

    #[test]
    fn owning_agent_id_single_agent_uses_agent_id() {
        let id = owning_agent_id(&[make_agent_cfg("scout-1")], "google-mcp-server");
        assert_eq!(id, "scout-1",
            "single-agent mode must return the agent ID (ar-07 reverted to server.name?)");
    }

    #[test]
    fn owning_agent_id_multi_agent_uses_shared_sentinel() {
        let id = owning_agent_id(
            &[make_agent_cfg("coordinator"), make_agent_cfg("scout")],
            "google-mcp-server",
        );
        assert_eq!(id, "shared",
            "multi-agent mode must use 'shared' sentinel — not agent[0].id (misleading audit trail)");
    }

    #[test]
    fn owning_agent_id_empty_agents_falls_back_to_server_name() {
        let id = owning_agent_id(&[], "fallback-server");
        assert_eq!(id, "fallback-server",
            "zero-agent case must fall back to server name");
    }

    // ── cred.3.1 Codex Critical: None capabilities must not grant all credential providers ──
    // This test FAILS without the `None => vec![]` fix in the credential_env build block.
    // Before the fix: `None` (no capabilities field) granted the server tokens for ALL
    // configured providers — a forgotten capabilities field silently bypassed deny-by-default.

    #[test]
    fn none_capabilities_yields_empty_credential_providers() {
        use agentd::capability::Capability;

        // Calls the REAL production function (credential_allowed_providers, defined
        // above main.rs's MCP-spawn loop) instead of a hand-copied duplicate — found by
        // /review (testing specialist, 2026-07-15): the duplicate meant this test could
        // never catch a regression in the actual credential_env construction closure.

        // None capabilities → empty list (deny-by-default)
        assert!(
            credential_allowed_providers(&None).is_empty(),
            "None capabilities must not grant any credential providers"
        );

        // Some([]) → empty list (explicit empty grant)
        assert!(
            credential_allowed_providers(&Some(vec![])).is_empty(),
            "Empty capabilities list must not grant any credential providers"
        );

        // Some([Credential{Google}]) → ["google"]
        let allowed = credential_allowed_providers(&Some(vec![Capability::Credential {
            provider: agentd::capability::CredentialProvider::Google,
        }]));
        assert_eq!(allowed, ["google"]);
    }

    // Closes the loop end-to-end: loads the REAL cos.agents.toml files' google_oauth
    // server through the REAL production function, rather than a hand-built Capability
    // list (the test above) or a TOML-structure-only check (config.rs's
    // cos_agents_toml_google_oauth_server_has_credential_capability). Found by /review
    // (adversarial pass, 2026-07-15) as the suggested closing check.
    #[test]
    fn cos_agents_toml_google_oauth_yields_google_allowed_provider() {
        for (raw, label) in [
            (include_str!("../cos.agents.toml"), "agentd/cos.agents.toml"),
            (
                include_str!("../../distro/overlay/etc/agentd/cos.agents.toml"),
                "distro overlay cos.agents.toml",
            ),
        ] {
            let cfg: agentd::config::Config = toml::from_str(raw).expect("must parse");
            let server = cfg
                .tools
                .mcp_servers
                .iter()
                .find(|s| s.name == "google_oauth")
                .unwrap_or_else(|| panic!("{label}: no google_oauth MCP server defined"));
            let allowed = credential_allowed_providers(&server.capabilities);
            assert_eq!(
                allowed, ["google"],
                "{label}: google_oauth's own capabilities must derive exactly \
                 allowed_providers=[\"google\"] through the real production function"
            );
        }
    }

    // ── p5.8 CONVENTIONS.md event taxonomy completeness check ─────────────────

    #[test]
    fn event_taxonomy_completeness() {
        // All tracked event kind strings must appear in CONVENTIONS.md.
        // This fails if a new event is added to events.rs but the docs table is not updated.
        let conventions = include_str!("../../docs/CONVENTIONS.md");
        let required_kinds = [
            "memory_read",
            "memory_write",
            "memory_unavailable",
            "memory_quarantined",
            "memory_pressure_advisory",
            "memory_paged",
            "memory_distilled",
            "kb_search",
            "memory_evicted",
            "mcp_http_connected",
            "mcp_http_error",
            "inference_stream_started",
            "inference_stream_completed",
            "approval_requested",
            "approval_granted",
            "approval_rejected",
            "egress_brokered",
            "egress_denied",
            "action_receipt_emitted",
            "egress_proxy_failed",
            "inference_transport_retried",
            "management_started",
            "management_request",
            "credential_cap_exceeded",
        ];
        for kind in &required_kinds {
            assert!(
                conventions.contains(kind),
                "CONVENTIONS.md missing event kind: `{kind}` — add a row to the taxonomy table"
            );
        }
    }

    // ── cred.6: broker mode — google_oauth must NOT have FsRead /run/secrets ──
    //
    // In broker mode the Rust credential gateway reads the token file; the sidecar
    // holds no raw credential. FsRead /run/secrets must be absent from google_oauth's
    // capabilities, and the orchestrator must carry Credential{Google} so spawned
    // children can route through the broker.
    // Checks BOTH config files so Docker and QEMU paths stay in sync.

    #[test]
    fn cos_config_broker_mode_and_no_fs_read() {
        fn assert_broker_mode(raw: &str, label: &str) {
            let cfg: agentd::config::Config =
                toml::from_str(raw).unwrap_or_else(|e| panic!("{label} must parse: {e}"));
            // 1. google_oauth must NOT have FsRead /run/secrets (broker reads it instead).
            let server = cfg
                .tools
                .mcp_servers
                .iter()
                .find(|s| s.name == "google_oauth")
                .unwrap_or_else(|| panic!("google_oauth server must exist in {label}"));
            let caps = server.capabilities.as_deref().unwrap_or(&[]);
            let has_fs_read = caps.iter().any(|c| {
                matches!(c, Capability::FsRead { prefix } if prefix == "/run/secrets")
            });
            assert!(
                !has_fs_read,
                "{label}: google_oauth must NOT have FsRead{{prefix=\"/run/secrets\"}} \
                 in broker mode — the sidecar holds no raw credential (cred.6)"
            );
            // 2. Credential{Google} must be declared somewhere agent-facing so the Gmail-fetching
            //    node can call through the broker gateway (cred.6). cap.2b moved it OFF the
            //    de-privileged orchestrator trigger and ONTO the cos-inbox [[jobs]] declaration —
            //    check agents AND jobs (union) so the wiring is verified wherever it lives.
            let agent_caps = cfg
                .agent_configs()
                .unwrap_or_default()
                .into_iter()
                .flat_map(|a| a.capabilities.unwrap_or_default())
                .chain(cfg.jobs.iter().flat_map(|j| j.capabilities.clone()))
                .collect::<Vec<_>>();
            let has_credential_google = agent_caps.iter().any(|c| {
                matches!(c, Capability::Credential { provider }
                    if matches!(provider, agentd::capability::CredentialProvider::Google))
            });
            assert!(
                has_credential_google,
                "{label}: an agent or job must declare Credential{{provider=Google}} \
                 so the Gmail node can call through the broker gateway (cred.6)"
            );
            // 3. google provider must have non-empty passthrough_query_params so Gmail
            //    query params (maxResults, q, format, pageToken) reach the upstream API.
            //    An empty list silently drops all query params, causing Gmail to return
            //    unfiltered results without a diagnostically clear error.
            let gw_providers = &cfg.credential_gateway.providers;
            let google_prov = gw_providers
                .get("google")
                .unwrap_or_else(|| panic!("{label}: credential_gateway must have a 'google' provider"));
            assert!(
                !google_prov.passthrough_query_params.is_empty(),
                "{label}: credential_gateway.providers.google must have non-empty \
                 passthrough_query_params so Gmail query params reach the upstream API (cred.6)"
            );
        }
        assert_broker_mode(include_str!("../cos.agents.toml"),       "agentd/cos.agents.toml");
        assert_broker_mode(
            include_str!("../../distro/overlay/etc/agentd/cos.agents.toml"),
            "distro/overlay/etc/agentd/cos.agents.toml",
        );
    }

    // ── ux.0b: shipped cos configs must pair a non-loopback bind_addr with the
    // explicit allow_non_loopback opt-in, or agentd's management::start guard
    // refuses to bind — this is the pre-existing QEMU conflict ux.0b fixes. ──

    #[test]
    fn cos_configs_pair_non_loopback_bind_with_opt_in() {
        fn assert_non_loopback_opted_in(raw: &str, label: &str) {
            let cfg: agentd::config::Config =
                toml::from_str(raw).unwrap_or_else(|e| panic!("{label} must parse: {e}"));
            if cfg.management.bind_addr != "127.0.0.1" {
                assert!(
                    cfg.management.allow_non_loopback,
                    "{label}: bind_addr={:?} is non-loopback but allow_non_loopback is false — \
                     agentd's management::start guard will refuse to bind",
                    cfg.management.bind_addr
                );
            }
        }
        assert_non_loopback_opted_in(include_str!("../cos.agents.toml"), "agentd/cos.agents.toml");
        assert_non_loopback_opted_in(
            include_str!("../../distro/overlay/etc/agentd/cos.agents.toml"),
            "distro/overlay/etc/agentd/cos.agents.toml",
        );
    }

    // ── ux.0b (adversarial-review follow-up): docker-compose.yml's management-
    // port publish must stay pinned to host loopback. This has no YAML-parsing
    // dependency (agentd stays a light runtime — see CLAUDE.md); a substring
    // check is enough to catch the one-line regression that matters: reverting
    // to a bare `7999:7999` publish, which LAN-exposes the unauthenticated
    // management API. ──

    #[test]
    fn compose_management_port_is_loopback_pinned() {
        // Scoped to the `cos:` service block specifically — a whole-file substring
        // check (the original version of this test) passes as long as the safe
        // string appears ANYWHERE in the file, even in a comment, even alongside
        // an unsafe second port mapping added elsewhere in the same service.
        // Adversarial review (ux.0b ship pass) confirmed both bypasses reproduce
        // against the naive check; this version asserts there is exactly one
        // `ports:` list item under `cos:`, and it is the loopback-pinned mapping.
        let raw = include_str!("../../docker-compose.yml");
        let lines: Vec<&str> = raw.lines().collect();
        let cos_start = lines
            .iter()
            .position(|l| l.trim_end() == "  cos:")
            .expect("docker-compose.yml must define a `cos:` service");
        let cos_end = lines[cos_start + 1..]
            .iter()
            .position(|l| !l.trim().is_empty() && !l.starts_with("    "))
            .map(|i| cos_start + 1 + i)
            .unwrap_or(lines.len());
        let cos_block = &lines[cos_start..cos_end];

        let ports_line = cos_block
            .iter()
            .position(|l| l.trim() == "ports:")
            .expect("cos service must declare a `ports:` mapping");
        let port_items: Vec<&str> = cos_block[ports_line + 1..]
            .iter()
            .take_while(|l| l.trim_start().starts_with('-'))
            .copied()
            .collect();

        assert_eq!(
            port_items.len(),
            1,
            "cos service must publish exactly one port mapping (the loopback-pinned \
             management API); found: {port_items:?}"
        );
        assert_eq!(
            port_items[0].trim(),
            "- \"127.0.0.1:7999:7999\"",
            "cos service's only port mapping must be pinned to host loopback \
             (127.0.0.1:7999:7999) — never bare, never 0.0.0.0, never a different host IP; \
             got: {:?}",
            port_items[0]
        );
    }

    // ── FIX 2: Landlock V4 unavailable message reports actual ABI ─────────────

    #[test]
    fn net_landlock_v4_unavailable_message_no_hardcoded_kernel_version() {
        // FAILS without FIX 2: old message hardcoded "kernel < 6.7" which is
        // misleading on kernels ≥ 6.7 compiled without full Landlock V4 support.
        let msg = net_landlock_v4_unavailable_message(&[443]);
        assert!(
            !msg.contains("kernel < 6.7"),
            "message must not claim 'kernel < 6.7' — report actual ABI version instead: {msg}"
        );
        assert!(
            msg.contains("ABI"),
            "message must mention 'ABI' to explain the real constraint: {msg}"
        );
        assert!(
            msg.contains("detected ABI: v"),
            "message must include the detected ABI version number: {msg}"
        );
    }
}

