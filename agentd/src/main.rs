use std::path::PathBuf;

use anyhow::Context;

mod config;
mod flight_recorder;

use flight_recorder::{EventKind, FlightRecorder};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let path: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "agent.toml".to_string())
        .into();

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

    Ok(())
}
