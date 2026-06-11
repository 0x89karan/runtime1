use std::{io::IsTerminal, path::PathBuf, sync::{Arc, RwLock}};

use anyhow::Context;
use agentd::{checkpoint::CheckpointStore, config, scheduler::Scheduler};
use agentd::flight_recorder::{EventKind, FlightRecorder};
use agentd::inference::anthropic::AnthropicGateway;
use agentd::tools::{
    mcp::{McpClient, McpTool},
    native::register_native,
    ToolRegistry,
};
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

    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("--probe") => {
            let prompt = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--probe requires a prompt argument"))?;
            run_probe(&prompt).await
        }
        Some(path) => run_agent(PathBuf::from(path)).await,
        None => run_agent(PathBuf::from("agent.toml")).await,
    };

    if let Err(e) = result {
        // Use {e:#} to emit the full anyhow error chain (not just the outermost context).
        tracing::error!("agentd exited with error: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

async fn run_agent(path: PathBuf) -> anyhow::Result<()> {
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

    // Held for Drop: keeps MCP child processes alive until run_agent returns.
    // std::process::exit() bypasses Drop, so we must return Err instead of
    // calling exit() while mcp_clients is still in scope.
    let mut mcp_clients: Vec<Arc<McpClient>> = Vec::new();
    for server in &cfg.tools.mcp_servers {
        tracing::info!(
            name = %server.name,
            command = %server.command,
            "spawning MCP server"
        );
        let (client, specs) = McpClient::spawn(&server.command, &server.args)
            .await
            .with_context(|| format!("spawning MCP server '{}'", server.name))?;
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
                "task":         ac.task,
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
    let maybe_session = {
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
