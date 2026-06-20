use std::{io::IsTerminal, path::PathBuf, sync::{Arc, RwLock}};

use anyhow::Context;
use agentd::{agent::{truncate, PREVIEW_CHARS}, checkpoint::CheckpointStore, config, scheduler::Scheduler};
use agentd::capability::{normalize_path, Capability};
use agentd::flight_recorder::{EventKind, FlightRecorder};
use agentd::inference::anthropic::AnthropicGateway;
use agentd::memory::store::RedbStore;
use agentd::tools::{
    mcp::{McpBackend, McpClient, McpHttpClient, McpTool},
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
    let cfg: config::Config =
        toml::from_str(&raw).with_context(|| format!("parsing config from {path:?}"))?;

    // Resolve flight log path: CLI flag > TOML field > default "flight.jsonl".
    let log_path = resolve_log_path(log_path_override, cfg.log_path.as_deref());
    let recorder = Arc::new(FlightRecorder::new(&log_path)?);

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

    // Build AgentCards from configs — static, used by list_agents tool.
    let cards: Arc<Vec<crate::config::AgentCard>> = Arc::new(
        agent_cfgs.iter().map(crate::config::AgentCard::from).collect()
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

    // Keep a clone for distillation wiring (p5.6) before moving into the registry.
    let memory_store_for_distillation = memory_store.clone();
    register_native(&mut registry, &cfg.tools.native, Some(Arc::clone(&cards)), memory_store)?;

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
                registry
                    .register(Box::new(McpTool::new(Arc::clone(&backend), spec, server.name.clone())))
                    .with_context(|| format!("registering tools from HTTP MCP server '{}'", server.name))?;
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
        let (client, specs) = McpClient::spawn(
            effective_cmd,
            &effective_args,
            effective_compiled,
            &server.env,
        )
        .await
        .with_context(|| format!("spawning MCP server '{}'", server.name))?;

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
            registry
                .register(Box::new(McpTool::new(Arc::clone(&client), spec, server.name.clone())))
                .with_context(|| format!("registering tools from MCP server '{}'", server.name))?;
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

    // Set static snapshot fields (provider model + sandbox status) once at startup,
    // before the FUSE mount, so the /agents/system/ files are accurate from first access.
    if let Ok(mut snap) = snapshot.write() {
        snap.provider_model = cfg.model.model.clone();
        snap.sandbox = surfaces::SandboxSummary {
            any_sandboxed:  any_sandbox_applied,
            servers:        server_enforcements,
            degradations:   degradation_set.into_iter().collect(),
        };
    }

    #[cfg(target_os = "linux")]
    let fuse_mountpoint = PathBuf::from("/agents");

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
        match surfaces::agents_fs::mount(&fuse_mountpoint, Arc::clone(&snapshot), fuse_mem_access) {
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
    let _maybe_session: Option<()> = None;

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
            // Remove checkpoint after successful load so a second restart starts fresh.
            if let Err(e) = std::fs::remove_file("checkpoint.json") {
                tracing::warn!("could not remove checkpoint.json after restore: {e}");
            }
            Some(cp)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::error!("checkpoint corrupt, starting fresh: {e:#}");
            let _ = std::fs::rename("checkpoint.json", "checkpoint.json.corrupt");
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

    let streamed_agents = scheduler.streamed_agents();
    let outcomes = scheduler.run().await;

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

/// Convert an agent capability set into sandbox rules for an MCP server subprocess.
///
/// Landlock FS rules map 1:1 from FsRead/FsWrite capabilities. DenySpawn is
/// added whenever the Spawn capability is absent. IsolateNetwork is added
/// whenever the Net capability is absent — enforcing network isolation at the
/// Linux namespace level for servers that don't need outbound access.
///
/// When `Net{ports}` is declared but the kernel does not support Landlock V4
/// (< Linux 6.7), port-level enforcement is impossible. The safe fallback is
/// `IsolateNetwork` (deny-all) rather than no restriction, so the operator's
/// intent (controlled egress) is approximated rather than silently inverted.
fn caps_to_rules(caps: &[Capability]) -> Vec<SandboxRule> {
    let v4_available = sandbox::landlock_v4_available();
    caps_to_rules_inner(caps, v4_available)
}

fn caps_to_rules_inner(caps: &[Capability], v4_available: bool) -> Vec<SandboxRule> {
    let mut rules = Vec::new();
    let has_spawn = caps.iter().any(|c| matches!(c, Capability::Spawn));
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
                    // Kernel < 6.7: Landlock V4 TCP port enforcement unavailable.
                    // Fall back to full network isolation (deny-all) — safer than
                    // silently allowing unrestricted access.
                    tracing::warn!(
                        "Net{{ports}} declared but Landlock ABI V4 is unavailable on this kernel; \
                         falling back to IsolateNetwork (deny-all). \
                         Upgrade to Linux ≥ 6.7 for per-port enforcement."
                    );
                    rules.push(SandboxRule::IsolateNetwork);
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
            // Mcp/KbRead/KbWrite are agent-level only; no sandbox rule maps to them.
            Capability::Mcp { .. } | Capability::Spawn | Capability::KbRead { .. } | Capability::KbWrite { .. } => {}
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
    fn caps_to_rules_net_with_ports_pre_v4_falls_back_to_isolate_network() {
        // Simulate a pre-V4 kernel: Net{ports} must fall back to IsolateNetwork, not allow-all.
        let rules = caps_to_rules_inner(&[Capability::Net {
            hosts: vec!["api.anthropic.com".into()],
            ports: vec![443],
        }], false);
        assert!(
            rules.contains(&SandboxRule::IsolateNetwork),
            "Net {{ ports: [443] }} on pre-V4 kernel → IsolateNetwork (deny-all fallback)"
        );
        assert!(
            !rules.contains(&SandboxRule::AllowNetConnect { port: 443 }),
            "pre-V4 must NOT emit AllowNetConnect (unenforced on this kernel)"
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
        ];
        for kind in &required_kinds {
            assert!(
                conventions.contains(kind),
                "CONVENTIONS.md missing event kind: `{kind}` — add a row to the taxonomy table"
            );
        }
    }
}

