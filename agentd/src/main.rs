use std::{io::IsTerminal, path::PathBuf, sync::Arc};

use anyhow::Context;
use agentd::{agent, config};
use agentd::flight_recorder::{EventKind, FlightRecorder};
use agentd::inference::anthropic::AnthropicGateway;
use agentd::tools::{
    mcp::{McpClient, McpTool},
    native::register_native,
    ToolRegistry,
};

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
    match args.next().as_deref() {
        Some("--probe") => {
            let prompt = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--probe requires a prompt argument"))?;
            run_probe(&prompt).await
        }
        Some(path) => run_agent(PathBuf::from(path)).await,
        None => run_agent(PathBuf::from("agent.toml")).await,
    }
}

async fn run_agent(path: PathBuf) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("loading config from {path:?}"))?;
    let cfg: config::Config =
        toml::from_str(&raw).with_context(|| format!("parsing config from {path:?}"))?;

    let recorder = FlightRecorder::open()?;

    recorder.record(
        &cfg.agent.id,
        None,
        EventKind::AgentSpawned,
        serde_json::json!({
            "model": cfg.model.model,
            "provider": cfg.model.provider,
            "max_tokens": cfg.model.max_tokens,
            "max_turns": cfg.agent.max_turns,
            "token_budget": cfg.agent.token_budget,
            "task": cfg.agent.task,
            "native_tools": cfg.tools.native,
            "mcp_servers": cfg.tools.mcp_servers.len(),
        }),
    );

    tracing::info!(
        agent = %cfg.agent.id,
        model = %cfg.model.model,
        "agent spawned"
    );

    let mut registry = ToolRegistry::new();
    register_native(&mut registry, &cfg.tools.native)?;

    // Spawn MCP servers and register their tools. McpTool holds an Arc<McpClient>,
    // so the child processes remain alive as long as the registry does. We also
    // collect the Arcs here so the clients are explicitly dropped after the agent
    // run rather than at an arbitrary point during registry cleanup.
    let mut mcp_clients: Vec<Arc<McpClient>> = Vec::new();
    for server in &cfg.tools.mcp_servers {
        tracing::info!(
            agent = %cfg.agent.id,
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
                .register(Box::new(McpTool::new(Arc::clone(&client), spec)))
                .with_context(|| format!("registering tools from MCP server '{}'", server.name))?;
        }
        tracing::info!(
            agent = %cfg.agent.id,
            name = %server.name,
            tools = n,
            "MCP server connected"
        );
        mcp_clients.push(client);
    }

    let tool_names = registry.tool_names();
    recorder.record(
        &cfg.agent.id,
        None,
        EventKind::ToolsRegistered,
        serde_json::json!({ "tools": tool_names }),
    );

    tracing::info!(
        agent = %cfg.agent.id,
        tools = ?tool_names,
        "tools registered"
    );

    // Task: config field takes precedence; fall back to stdin when not a tty.
    let task = if !cfg.agent.task.is_empty() {
        cfg.agent.task.clone()
    } else if !std::io::stdin().is_terminal() {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading task from stdin")?;
        let trimmed = buf.trim().to_string();
        if trimmed.is_empty() {
            return Err(anyhow::anyhow!("no task: stdin was empty"));
        }
        trimmed
    } else {
        return Err(anyhow::anyhow!(
            "no task: set [agent].task in config or pipe text to stdin"
        ));
    };

    let gateway = AnthropicGateway::from_env(&cfg.model.model)
        .context("initializing Anthropic gateway")?;

    let answer = agent::run(
        &cfg.agent.id,
        &task,
        &cfg.agent,
        &cfg.model,
        &gateway,
        &registry,
        &recorder,
    )
    .await?;

    println!("{answer}");
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
