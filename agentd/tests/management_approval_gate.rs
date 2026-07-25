//! ux.12 runtime QA: the X-Approval-Token gate over the real HTTP wire.
//!
//! The unit tests cover the `approval_token_ok` predicate; these boot the actual
//! `management::start` server and drive it with a real HTTP client, verifying the
//! gate fires in `handle()` BEFORE `route()` (so a rejected approve never reaches
//! the scheduler control channel) — the negative control that proves the gate is
//! load-bearing, not decorative.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use agentd::control::ControlCommand;
use surfaces::{PendingActionView, SchedulerSnapshot, SharedSnapshot};
use tokio::sync::{broadcast, mpsc};
use tokio::sync::mpsc::error::TryRecvError;

fn snap_with_action() -> SharedSnapshot {
    let mut snap = SchedulerSnapshot::default();
    snap.pending_actions.push(PendingActionView {
        id: "act_0".to_string(),
        agent_id: "cos".to_string(),
        kind: "egress_send".to_string(),
        risk: "high".to_string(),
        summary: "send an email".to_string(),
        args_json: "{}".to_string(),
        age_secs: 0,
    });
    Arc::new(RwLock::new(snap))
}

/// Boot a management server on an ephemeral loopback port with the given approval
/// secret. Returns the base URL and the receiver end of the control channel.
async fn boot(secret: Option<String>) -> (String, mpsc::Receiver<ControlCommand>) {
    let (tx, rx) = mpsc::channel::<ControlCommand>(16);
    let (btx, _brx) = broadcast::channel::<String>(16);
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let recorder = Arc::new(agentd::flight_recorder::FlightRecorder::new(tmp.path()).unwrap());
    // Keep the temp file alive for the server's lifetime (recorder writes are best-effort,
    // but don't yank the path out from under it): leak the handle for the test process.
    std::mem::forget(tmp);
    // Keep a broadcast receiver alive so the recorder's best-effort sends don't matter either way.
    std::mem::forget(_brx);
    let addr = agentd::management::start(
        "127.0.0.1",
        0,
        false,
        snap_with_action(),
        None,
        None,
        btx,
        recorder,
        Some(tx),
        None,
        secret,
    )
    .await
    .expect("management server should bind on loopback");
    (format!("http://{addr}"), rx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_gate_rejects_unauthorized_and_never_reaches_scheduler() {
    let (base, mut rx) = boot(Some("test-secret".to_string())).await;
    let client = reqwest::Client::new();
    let approve = format!("{base}/api/v1/approvals/act_0/approve");

    // Negative control 1: no token → 401, and NOTHING enqueued to the scheduler.
    let r = client.post(&approve).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401, "missing token must be rejected");
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)), "rejected approve must not reach the control channel");

    // Negative control 2: wrong token → 401, still nothing enqueued.
    let r = client.post(&approve).header("X-Approval-Token", "wrong").send().await.unwrap();
    assert_eq!(r.status().as_u16(), 401, "wrong token must be rejected");
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)), "wrong-token approve must not reach the control channel");

    // The read route stays ungated (the sidecar must poll it): 200 with no token.
    let r = client.get(format!("{base}/api/v1/approvals")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200, "GET /approvals must remain ungated");

    // Correct token → 200 AND a real ControlCommand::Approve reaches the scheduler.
    let r = client.post(&approve).header("X-Approval-Token", "test-secret").send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200, "correct token must be accepted");
    let cmd = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("a command should arrive within 2s")
        .expect("channel should not be closed");
    match cmd {
        ControlCommand::Approve { id, .. } => assert_eq!(id, "act_0"),
        other => panic!("expected Approve, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_gate_open_when_no_secret_configured() {
    // Backward compatibility: no secret ⇒ routes are open (pre-ux.12 behavior).
    let (base, mut rx) = boot(None).await;
    let client = reqwest::Client::new();
    let r = client
        .post(format!("{base}/api/v1/approvals/act_0/approve"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "no-secret deployment must accept unauthenticated approve");
    let cmd = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("a command should arrive within 2s")
        .expect("channel should not be closed");
    assert!(matches!(cmd, ControlCommand::Approve { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cap4_gate_covers_the_whole_mutating_surface() {
    // cap.4 / AUDIT-v0.97 P2-3: with a secret set, EVERY mutating route (not just approve/deny)
    // requires the token — and none of them reach the scheduler control channel without it.
    let (base, mut rx) = boot(Some("test-secret".to_string())).await;
    let client = reqwest::Client::new();
    let posts = [
        ("/api/v1/spawn", serde_json::json!({"task": "x"})),
        ("/api/v1/agents/a/inject", serde_json::json!({"text": "x"})),
        ("/api/v1/agents/a/cancel", serde_json::json!({})),
        ("/api/v1/agents/a/caps", serde_json::json!({"capabilities": []})),
        ("/api/v1/budget/set", serde_json::json!({"target": {"agent": "a"}, "limit": 1})),
        ("/api/v1/budget/reset", serde_json::json!({"target": "global"})),
    ];
    for (path, body) in posts {
        let r = client.post(format!("{base}{path}")).json(&body).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 401, "POST {path} must require X-Approval-Token");
    }
    // A read stays open without the token.
    let r = client.get(format!("{base}/api/v1/approvals")).send().await.unwrap();
    assert_eq!(r.status().as_u16(), 200, "GET must stay ungated");
    // Nothing reached the scheduler.
    assert!(matches!(rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)),
        "no rejected mutation may reach the control channel");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_refuses_privileged_caps_without_optin() {
    // cap.4 / AUDIT-v0.97 P2-3: /spawn must refuse caller-supplied privileged caps
    // (Spawn/Credential/RunJob) unless AGENTOS_ALLOW_PRIVILEGED_SPAWN is set. Gate is open
    // here (no secret) so the token is not the blocker — the refusal is. Runs before the
    // control-channel send, so no scheduler responder is needed.
    let (base, _rx) = boot(None).await;
    let client = reqwest::Client::new();
    // Deny-by-default across the vectors: Spawn, the Mcp{google_oauth} live-Gmail vector Codex
    // caught (no Credential cap needed), and unrestricted (capabilities omitted = all caps).
    for body in [
        serde_json::json!({"task": "x", "capabilities": ["Spawn"]}),
        serde_json::json!({"task": "x", "capabilities": [{"Mcp": {"server": "google_oauth", "tools": []}}]}),
        // AUDIT-v0.97 holistic review (Codex High): FsRead{"/"} is whole-filesystem read (egress
        // signing key, OAuth cache, secrets) — deny-by-default must refuse it without the opt-in.
        serde_json::json!({"task": "x", "capabilities": [{"FsRead": {"prefix": "/"}}]}),
        serde_json::json!({"task": "x"}),  // no capabilities field → unrestricted
    ] {
        let r = client.post(format!("{base}/api/v1/spawn")).json(&body).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 400, "privileged/unrestricted /spawn refused without opt-in: {body}");
        let resp: serde_json::Value = r.json().await.unwrap();
        assert!(resp["error"].as_str().unwrap_or("").contains("privileged"),
            "error names the privileged refusal: {resp}");
    }
    // A read-only-local spawn (KbRead) is allowed without the opt-in (deny-by-default lets safe
    // caps through) — it reaches the control channel (503 here: no scheduler responder wired).
    let r = client.post(format!("{base}/api/v1/spawn"))
        .json(&serde_json::json!({"task": "x", "capabilities": [{"KbRead": {"segment": "ops:briefs"}}]}))
        .send().await.unwrap();
    assert_ne!(r.status().as_u16(), 400, "read-only-local spawn is not refused as privileged");
}
