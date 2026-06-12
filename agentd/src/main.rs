use std::{io::IsTerminal, path::PathBuf, sync::{Arc, RwLock}};

use anyhow::Context;
use agentd::{agent::{truncate, PREVIEW_CHARS}, checkpoint::CheckpointStore, config, scheduler::Scheduler};
use agentd::capability::Capability;
use agentd::flight_recorder::{EventKind, FlightRecorder};
use agentd::inference::anthropic::AnthropicGateway;
use agentd::tools::{
    mcp::{McpClient, McpTool},
    native::register_native,
    ToolRegistry,
};
use sandbox::{CompiledSandbox, SandboxRule};
use surfaces::SchedulerSnapshot;

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
        || std::env::var("AGENTOS_NO_FUSE").is_ok_and(|v| !v.is_empty());
    let filtered: Vec<&str> = raw_args.iter()
        .filter(|a| a.as_str() != "--no-fuse")
        .map(String::as_str)
        .collect();

    let result = match filtered.first().copied() {
        Some("--probe") => {
            let prompt = filtered
                .get(1)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("--probe requires a prompt argument"))?;
            run_probe(prompt).await
        }
        Some(path) => run_agent(PathBuf::from(path), no_fuse).await,
        None => run_agent(PathBuf::from("agent.toml"), no_fuse).await,
    };

    if let Err(e) = result {
        // Use {e:#} to emit the full anyhow error chain (not just the outermost context).
        tracing::error!("agentd exited with error: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run_agent(path: PathBuf, no_fuse: bool) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("loading config from {path:?}"))?;
    let cfg: config::Config =
        toml::from_str(&raw).with_context(|| format!("parsing config from {path:?}"))?;

    let recorder = Arc::new(FlightRecorder::open()?);

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
    register_native(&mut registry, &cfg.tools.native, Some(Arc::clone(&cards)))?;

    // Pass 1: validate capabilities and isolation settings before spawning any process.

    // Check gVisor availability upfront so the error is clear, not buried in spawn output.
    #[cfg(target_os = "linux")]
    for server in &cfg.tools.mcp_servers {
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

    // When mcp_require_capabilities is true, refuse to start if any server would
    // run unsandboxed — either because the field is missing OR because the caps
    // produce no effective rules (e.g. capabilities=[{Spawn}] yields empty rules).
    if cfg.tools.mcp_require_capabilities {
        let missing: Vec<&str> = cfg.tools.mcp_servers
            .iter()
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

    // Held for Drop: keeps MCP child processes alive until run_agent returns.
    // std::process::exit() bypasses Drop, so we must return Err instead of
    // calling exit() while mcp_clients is still in scope.
    let mut mcp_clients: Vec<Arc<McpClient>> = Vec::new();
    for server in &cfg.tools.mcp_servers {
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
                        },
                    }),
                );
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
        let n = specs.len();
        for spec in specs {
            registry
                .register(Box::new(McpTool::new(Arc::clone(&client), spec, server.name.clone())))
                .with_context(|| format!("registering tools from MCP server '{}'", server.name))?;
        }
        tracing::info!(name = %server.name, tools = n, "MCP server connected");
        mcp_clients.push(client);
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
        match surfaces::agents_fs::mount(&fuse_mountpoint, Arc::clone(&snapshot)) {
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
            for client in &mcp_clients {
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
            for client in &mcp_clients {
                client.shutdown().await;
            }
            return Err(e);
        }
    };

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

    for client in &mcp_clients {
        client.shutdown().await;
    }

    let mut any_failed = false;
    for (id, result) in &outcomes {
        match result {
            Ok(answer) => println!("{answer}"),
            Err(e) => {
                tracing::error!(agent = %id, error = %e, "agent failed");
                any_failed = true;
            }
        }
    }

    if any_failed {
        // Return Err so main() calls exit() after run_agent has returned and
        // mcp_clients has been dropped — process::exit skips destructors.
        anyhow::bail!("one or more agents failed");
    }
    Ok(())
}

/// Convert an agent capability set into sandbox rules for an MCP server subprocess.
///
/// Landlock FS rules map 1:1 from FsRead/FsWrite capabilities. DenySpawn is
/// added whenever the Spawn capability is absent. IsolateNetwork is added
/// whenever the Net capability is absent — enforcing network isolation at the
/// Linux namespace level for servers that don't need outbound access.
fn caps_to_rules(caps: &[Capability]) -> Vec<SandboxRule> {
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
            // Mcp is advisory; Spawn and Net are handled above.
            Capability::Net { .. } | Capability::Mcp { .. } | Capability::Spawn => {}
        }
    }
    rules
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
        // Net present → no IsolateNetwork; Mcp is still advisory.
        let rules = caps_to_rules(&[
            Capability::Net { hosts: vec!["example.com".into()] },
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
        let rules = caps_to_rules(&[Capability::Net { hosts: vec![] }, Capability::Spawn]);
        assert!(!rules.contains(&SandboxRule::DenySpawn),      "Spawn → no DenySpawn");
        assert!(!rules.contains(&SandboxRule::IsolateNetwork), "Net → no IsolateNetwork");
    }
}

async fn run_probe(prompt: &str) -> anyhow::Result<()> {
    use agentd::inference::{Block, InferenceGateway, InferenceRequest, Msg, Role};

    let model = "claude-sonnet-4-6";
    let recorder = FlightRecorder::open()?;

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
        prompt = %prompt.chars().take(80).collect::<String>(),
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
        tools: vec![],
        max_tokens: 4096,
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
                Some(text.chars().take(200).collect())
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
