//! ux.13 runtime QA: the cancel/set-caps HTTP routes over the real wire.
//!
//! The route()-level unit tests cover the 503/400 branches; these boot the actual
//! `management::start` server, stand up a mock scheduler-side responder that drains
//! the control channel and answers the `confirm_tx`, and drive the routes over a
//! real socket — verifying the HTTP → ControlCommand → confirm → HTTP-status
//! round-trip (the 200 success and 404 unknown paths the unit tests can't reach
//! without a live consumer).

use std::sync::{Arc, RwLock};
use std::time::Duration;

use agentd::control::ControlCommand;
use surfaces::{SchedulerSnapshot, SharedSnapshot};
use tokio::sync::{broadcast, mpsc};

async fn boot() -> (String, mpsc::Receiver<ControlCommand>) {
    let (tx, rx) = mpsc::channel::<ControlCommand>(16);
    let (btx, _brx) = broadcast::channel::<String>(16);
    std::mem::forget(_brx);
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let recorder = Arc::new(agentd::flight_recorder::FlightRecorder::new(tmp.path()).unwrap());
    std::mem::forget(tmp);
    let snap: SharedSnapshot = Arc::new(RwLock::new(SchedulerSnapshot::default()));
    let addr = agentd::management::start(
        "127.0.0.1", 0, false, snap, None, None, btx, recorder, Some(tx), None, None,
    )
    .await
    .expect("bind loopback");
    (format!("http://{addr}"), rx)
}

/// Drain the control channel; answer Cancel/SetCaps `confirm_tx` as a stand-in for the
/// scheduler: "known" ids succeed, "ghost" ids 404, a widening SetCaps 400s.
fn spawn_mock_scheduler(mut rx: mpsc::Receiver<ControlCommand>) {
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                ControlCommand::Cancel { agent_id, confirm_tx } => {
                    let r = if agent_id == "ghost" {
                        Err(format!("agent '{agent_id}' not found"))
                    } else {
                        Ok(2u64) // pretend a 2-node subtree was cancelled
                    };
                    if let Some(tx) = confirm_tx { let _ = tx.send(r); }
                }
                ControlCommand::SetCaps { agent_id, capabilities, confirm_tx } => {
                    let r = if agent_id == "ghost" {
                        Err(format!("agent '{agent_id}' not found"))
                    } else if capabilities.is_empty() {
                        // Treat an empty request as a widening rejection for this stub.
                        Err("SetCaps is narrow-only; to widen, respawn".to_string())
                    } else {
                        Ok((3usize, capabilities.len()))
                    };
                    if let Some(tx) = confirm_tx { let _ = tx.send(r); }
                }
                _ => {}
            }
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_route_roundtrip() {
    let (base, rx) = boot().await;
    spawn_mock_scheduler(rx);
    let client = reqwest::Client::new();

    // Known agent → 200 with the cascade count.
    let r = client.post(format!("{base}/api/v1/agents/live/cancel")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200, "cancel of a known agent → 200");
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["count"], 2, "route surfaces the cascade count from the confirm");

    // Unknown agent → 404 (Err text contains 'not found').
    let r = client.post(format!("{base}/api/v1/agents/ghost/cancel")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 404, "cancel of an unknown agent → 404");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn setcaps_route_roundtrip() {
    let (base, rx) = boot().await;
    spawn_mock_scheduler(rx);
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/agents/live/caps");

    // Valid narrow → 200.
    let r = client.post(&url).json(&serde_json::json!({"capabilities": [{"Mcp": {"server": "x", "tools": []}}]}))
        .send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200, "a narrow SetCaps → 200");

    // Widening (stubbed as empty) → 400, not 404.
    let r = client.post(&url).json(&serde_json::json!({"capabilities": []}))
        .send().await.unwrap();
    assert_eq!(r.status().as_u16(), 400, "narrow-only violation → 400");

    // Unknown agent → 404.
    let r = client.post(format!("{base}/api/v1/agents/ghost/caps"))
        .json(&serde_json::json!({"capabilities": [{"Mcp": {"server": "x", "tools": []}}]}))
        .send().await.unwrap();
    assert_eq!(r.status().as_u16(), 404, "unknown agent → 404");
}

/// Sanity: the confirm wait doesn't hang the route if the scheduler never answers
/// (2s server-side timeout). No mock responder here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_route_times_out_without_scheduler() {
    let (base, _rx) = boot().await; // rx dropped-ish: kept but never drained
    let client = reqwest::Client::new();
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        client.post(format!("{base}/api/v1/agents/x/cancel")).send(),
    )
    .await
    .expect("route must return within the client window (server has a 2s confirm timeout)")
    .unwrap();
    // The route should surface a timeout/unavailable status rather than hang forever.
    assert!(r.status().as_u16() >= 400, "no scheduler answer → an error status, not a hang");
}
