use clap::Args;

#[derive(Args, Debug)]
pub struct OrchestrateArgs {
    /// Initial message to send (if omitted, prompt is shown on stdin)
    #[arg(trailing_var_arg = true)]
    pub message: Vec<String>,
    /// Agent ID to use (spawns a fresh agent if not found)
    #[arg(long, default_value = "orch-default")]
    pub agent_id: String,
    /// Maximum turns for the orchestrated agent
    #[arg(long, default_value_t = 200)]
    pub max_turns: u32,
    /// Management API URL (required for orchestration)
    #[arg(long, env = "AGENTCTL_URL")]
    pub url: Option<String>,
    /// FUSE agents directory (used for fallback source detection only)
    #[arg(long, default_value = "/agents")]
    pub agents_dir: std::path::PathBuf,
}

pub fn run(args: OrchestrateArgs) -> anyhow::Result<()> {
    use crate::watch::source::{detect_source, SpawnRequest};
    use std::io::BufRead as _;

    let source = detect_source(args.url.as_deref(), &args.agents_dir)?;

    // event_stream_url must be present for orchestration.
    let stream_url = source.event_stream_url().ok_or_else(|| {
        anyhow::anyhow!(
            "orchestrate requires the management API (--url http://HOST:7999 or AGENTCTL_URL).\n\
             FUSE-only mode does not support SSE event streaming."
        )
    })?;

    // Build initial message.
    let initial = if !args.message.is_empty() {
        args.message.join(" ")
    } else {
        eprint!("> ");
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        line.trim().to_string()
    };
    anyhow::ensure!(!initial.is_empty(), "No message provided");

    // Open the SSE connection ONCE before the first inject.  Keeping it alive across
    // turns closes the race where a fast agent fires OrchestratorTurnComplete before the
    // next wait_for_turn_complete call can reconnect and subscribe.
    // The management server sends a `: ping` keepalive every 30 s (ar-06) so TCP is
    // never idle long enough to be dropped by a load balancer or OS keepalive.
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(&stream_url)
        .header("accept", "text/event-stream")
        .send()
        .map_err(|e| anyhow::anyhow!("SSE connect failed: {e}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("SSE endpoint returned {}", resp.status());
    }
    let mut reader = std::io::BufReader::new(resp);

    // Check if the agent already exists and is alive.
    let snap = source.load_snapshot();
    // Only resume if the agent is parked waiting for input — any other live
    // status (running, deferred, awaiting_child, awaiting_approval) means we
    // cannot safely inject without corrupting message order.
    let agent_alive = snap.agents.iter().any(|a| {
        a.id == args.agent_id && a.status == "waiting"
    });

    let agent_id = if agent_alive {
        eprintln!("[orchestrate] resuming agent: {}", args.agent_id);
        source.inject(&args.agent_id, &initial)
            .map_err(|e| anyhow::anyhow!("inject failed: {e}"))?;
        args.agent_id.clone()
    } else {
        let req = SpawnRequest {
            task:         initial.clone(),
            id:           Some(args.agent_id.clone()),
            max_turns:    Some(args.max_turns),
            token_budget: None,
            orchestrated: true,
        };
        let resolved_id = source.spawn(&req)
            .map_err(|e| anyhow::anyhow!("spawn failed: {e}"))?;
        eprintln!("[orchestrate] agent: {}", resolved_id);
        resolved_id
    };

    // REPL loop: drain SSE until OrchestratorTurnComplete, print answer, inject next input.
    loop {
        let answer = drain_until_turn_complete(&mut reader, &agent_id)?;
        println!("{}", answer);

        // Inner loop: re-prompt on empty input without re-entering the SSE drain,
        // which would block waiting for a TurnComplete that never arrives.
        let next = 'input: loop {
            eprint!("> ");
            let mut line = String::new();
            match std::io::stdin().lock().read_line(&mut line) {
                Ok(0) => break 'input None,
                Ok(_) => {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        break 'input Some(trimmed);
                    }
                }
                Err(e) => {
                    eprintln!("[orchestrate] stdin error: {e}");
                    break 'input None;
                }
            }
        };

        let Some(next) = next else { break };
        // ar-07: "quit" / "exit" pause the session without injecting into the agent.
        if next == "quit" || next == "exit" {
            eprintln!(
                "[orchestrate] session paused. Resume with:\n  agentctl orchestrate --agent-id {} --url <URL>",
                agent_id
            );
            break;
        }
        source.inject(&agent_id, &next)
            .map_err(|e| anyhow::anyhow!("inject failed: {e}"))?;
    }

    Ok(())
}

/// Read from a persistent SSE reader until an `orchestrator_turn_complete` event for the
/// given agent arrives, then return the answer text.  The reader is kept alive across calls
/// so that events emitted between turns are not lost.
fn drain_until_turn_complete<R: std::io::Read>(
    reader: &mut std::io::BufReader<R>,
    agent_id: &str,
) -> anyhow::Result<String> {
    use std::io::BufRead as _;

    for line in reader.lines() {
        let line = line.map_err(|e| anyhow::anyhow!("SSE read error: {e}"))?;
        if !line.starts_with("data: ") {
            continue;
        }
        let json_str = &line["data: ".len()..];
        let v: serde_json::Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let kind = v["kind"].as_str().unwrap_or("");

        if kind == "orchestrator_turn_complete" {
            let event_agent = v["data"]["agent_id"].as_str().unwrap_or("");
            if event_agent == agent_id {
                let answer = v["data"]["answer"].as_str().unwrap_or("").to_string();
                return Ok(answer);
            }
        }

        if kind == "agent_failed" && v["agent"].as_str().unwrap_or("") == agent_id {
            let reason = v["data"]["reason"].as_str().unwrap_or("unknown");
            anyhow::bail!("agent failed: {reason}");
        }

        // agent_completed fires if the agent terminates without parking (e.g., budget
        // exceeded, or the waiting-set race from orch.1-ar-05). Bail with a clear error
        // rather than hanging forever on a signal that will never arrive.
        if kind == "agent_completed" && v["agent"].as_str().unwrap_or("") == agent_id {
            anyhow::bail!("agent exited without completing orchestrated turn (check flight log)");
        }

        // orchestrator_exited fires when an inject is rejected (e.g. agent already
        // in-flight from a concurrent inject). Without this guard the REPL hangs
        // forever waiting for an orchestrator_turn_complete that will never arrive.
        if kind == "orchestrator_exited" {
            let event_agent = v["data"]["agent_id"].as_str().unwrap_or("");
            if event_agent == agent_id {
                let reason = v["data"]["reason"].as_str().unwrap_or("unknown");
                anyhow::bail!(
                    "inject rejected (reason: {reason}) — agent may still be running.\n  \
                     Resume with: agentctl orchestrate --agent-id {agent_id} --url <URL>"
                );
            }
        }
    }

    anyhow::bail!(
        "SSE stream ended without turn complete signal for agent '{agent_id}'. \
         If the stream timed out, the agent may still be running — resume with:\n  \
         agentctl orchestrate --agent-id {agent_id} --url <URL>"
    )
}
