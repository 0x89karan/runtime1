//! Management HTTP API (p7.7).
//!
//! Exposes scheduler state as JSON + SSE on a loopback-only port (default :7999)
//! so `agentctl watch` can run on the Mac/Linux host without FUSE filesystem access.
//!
//! Routes:
//!   GET  /healthz                                → 200 {"ok": true}
//!   GET  /api/v1/snapshot                        → 200 SchedulerSnapshot JSON
//!   GET  /api/v1/approvals                       → 200 [PendingActionView, ...]
//!   POST /api/v1/approvals/:id/approve           → 200 | 400 | 404 | 503
//!   POST /api/v1/approvals/:id/deny              → 200 | 400 | 404 | 503
//!   GET  /api/v1/memory/:ns?limit=&offset=       → 200 [{key, value}, ...] paginated
//!   GET  /api/v1/events                          → 200 text/event-stream (SSE)
//!   POST /api/v1/spawn                           → 200 | 400 | 503 (orch.1)
//!   POST /api/v1/agents/:id/inject               → 200 | 400 | 503 (orch.1)

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use tokio::sync::mpsc;

use crate::control::ControlCommand;
use crate::events::EventKind;
use crate::flight_recorder::FlightRecorder;
use crate::memory::MemoryStore;
use surfaces::SharedSnapshot;

const MAX_MEMORY_LIMIT: usize = 100;

/// Shared state threaded into every request handler.
struct ApiState {
    snapshot:      SharedSnapshot,
    memory_store:  Option<Arc<dyn MemoryStore>>,
    broadcast_tx:  broadcast::Sender<String>,
    recorder:      Arc<FlightRecorder>,
    /// Sender half of the scheduler control channel. None when not wired (non-Linux or test).
    control_tx:    Option<mpsc::Sender<ControlCommand>>,
}

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

fn json_response(status: StatusCode, body: serde_json::Value) -> Response<BoxBody> {
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(bytes)).map_err(|e| match e {}).boxed())
        .unwrap()
}

fn error_response(status: StatusCode, message: &str) -> Response<BoxBody> {
    json_response(status, json!({"error": message}))
}

fn error_response_with_retry(status: StatusCode, message: &str) -> Response<BoxBody> {
    let bytes = serde_json::to_vec(&json!({"error": message})).unwrap_or_default();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("retry-after", "1")
        .body(Full::new(Bytes::from(bytes)).map_err(|e| match e {}).boxed())
        .unwrap()
}

async fn handle(
    state: Arc<ApiState>,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<BoxBody>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let query = req.uri().query().unwrap_or("").to_owned();

    // Read body bytes for POST requests, bounded at 64 KiB; other methods ignore body.
    const MAX_BODY_BYTES: usize = 64 * 1024;
    let body_bytes = if method == Method::POST {
        use http_body_util::Limited;
        match Limited::new(req.into_body(), MAX_BODY_BYTES).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) => Bytes::new(), // body too large or read error → treat as empty
        }
    } else {
        Bytes::new()
    };

    let resp = route(Arc::clone(&state), method.clone(), &path, &query, &body_bytes).await;
    let status = resp.status().as_u16();

    state.recorder.record(
        "management",
        None,
        EventKind::ManagementRequest,
        json!({"method": method.as_str(), "path": path, "status": status}),
    );

    Ok(resp)
}

async fn route(
    state: Arc<ApiState>,
    method: Method,
    path: &str,
    query: &str,
    body: &[u8],
) -> Response<BoxBody> {
    match (method.clone(), path) {
        (Method::GET, "/healthz") => {
            json_response(StatusCode::OK, json!({"ok": true}))
        }

        (Method::GET, "/api/v1/snapshot") => {
            // Clone snapshot under read lock, serialize after releasing.
            let snap = {
                let guard = match state.snapshot.read() {
                    Ok(g) => g,
                    Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "snapshot lock poisoned"),
                };
                guard.clone()
            };
            match serde_json::to_value(&snap) {
                Ok(v) => json_response(StatusCode::OK, v),
                Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
            }
        }

        (Method::GET, "/api/v1/approvals") => {
            let actions = {
                let guard = match state.snapshot.read() {
                    Ok(g) => g,
                    Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "snapshot lock poisoned"),
                };
                guard.pending_actions.clone()
            };
            match serde_json::to_value(&actions) {
                Ok(v) => json_response(StatusCode::OK, v),
                Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
            }
        }

        (Method::POST, path) if path.ends_with("/approve") && path.starts_with("/api/v1/approvals/") => {
            let id = path
                .strip_prefix("/api/v1/approvals/")
                .and_then(|s| s.strip_suffix("/approve"))
                .unwrap_or("")
                .trim();
            if id.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "approval id must not be empty");
            }
            // 404 if not in current pending_actions
            let agent_id = {
                let guard = match state.snapshot.read() {
                    Ok(g) => g,
                    Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "snapshot lock poisoned"),
                };
                guard.pending_actions.iter().find(|a| a.id == id).map(|a| a.agent_id.clone())
            };
            let Some(agent_id) = agent_id else {
                return error_response(StatusCode::NOT_FOUND, "approval id not found or already resolved");
            };
            let Some(tx) = &state.control_tx else {
                return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel not available");
            };
            let cmd = ControlCommand::Approve { id: id.to_string(), edits: None, auto_approve_kind: None };
            match tx.try_send(cmd) {
                Ok(()) => {
                    state.recorder.record("management", None, EventKind::ApprovalHttpApproved,
                        json!({"id": id, "agent_id": agent_id}));
                    json_response(StatusCode::OK, json!({"approved": id}))
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel full, retry")
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler not running")
                }
            }
        }

        (Method::POST, path) if path.ends_with("/deny") && path.starts_with("/api/v1/approvals/") => {
            let id = path
                .strip_prefix("/api/v1/approvals/")
                .and_then(|s| s.strip_suffix("/deny"))
                .unwrap_or("")
                .trim();
            if id.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "approval id must not be empty");
            }
            // 404 if not in current pending_actions
            let agent_id = {
                let guard = match state.snapshot.read() {
                    Ok(g) => g,
                    Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "snapshot lock poisoned"),
                };
                guard.pending_actions.iter().find(|a| a.id == id).map(|a| a.agent_id.clone())
            };
            let Some(agent_id) = agent_id else {
                return error_response(StatusCode::NOT_FOUND, "approval id not found or already resolved");
            };
            // Parse optional {"reason": "..."} from body.
            let reason = if !body.is_empty() {
                serde_json::from_slice::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
            } else {
                None
            };
            let Some(tx) = &state.control_tx else {
                return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel not available");
            };
            let cmd = ControlCommand::Reject { id: id.to_string(), reason: reason.clone() };
            match tx.try_send(cmd) {
                Ok(()) => {
                    state.recorder.record("management", None, EventKind::ApprovalHttpDenied,
                        json!({"id": id, "agent_id": agent_id, "reason": reason}));
                    json_response(StatusCode::OK, json!({"denied": id}))
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel full, retry")
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler not running")
                }
            }
        }

        (Method::GET, path) if path.starts_with("/api/v1/memory/") => {
            let ns = &path["/api/v1/memory/".len()..];
            if ns.is_empty() || ns.contains('/') {
                return error_response(StatusCode::BAD_REQUEST, "invalid namespace");
            }
            let Some(store) = &state.memory_store else {
                return error_response(StatusCode::SERVICE_UNAVAILABLE, "memory subsystem not configured");
            };
            // Parse ?limit=N&offset=M
            let (limit, offset) = parse_pagination(query);
            match store.iter(ns) {
                Ok(pairs) => {
                    let page: Vec<_> = pairs
                        .into_iter()
                        .skip(offset)
                        .take(limit)
                        .map(|(k, v)| json!({"key": k, "value": v}))
                        .collect();
                    json_response(StatusCode::OK, json!(page))
                }
                Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
            }
        }

        (Method::GET, "/api/v1/events") => {
            let mut rx = state.broadcast_tx.subscribe();

            // Build an SSE stream: each broadcast line becomes `data: <line>\n\n`.
            let stream = async_stream::stream! {
                loop {
                    match rx.recv().await {
                        Ok(line) => {
                            let sse = format!("data: {}\n\n", line.trim_end_matches('\n'));
                            yield Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from(sse)));
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            let sse = format!("data: {{\"lagged\": {n}}}\n\n");
                            yield Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from(sse)));
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            };

            let body = StreamBody::new(stream).boxed();
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("x-accel-buffering", "no")
                .body(body)
                .unwrap()
        }

        (Method::POST, "/api/v1/spawn") => {
            if body.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "request body required");
            }
            let req: crate::control::OperatorSpawnRequest = match serde_json::from_slice(body) {
                Ok(r) => r,
                Err(e) => return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")),
            };
            if req.task.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "task must not be empty");
            }
            let Some(tx) = &state.control_tx else {
                return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel not available");
            };
            let id_hint = req.id.clone().unwrap_or_else(|| "operator-agent".to_string());
            let cmd = ControlCommand::Spawn(req);
            match tx.try_send(cmd) {
                Ok(()) => json_response(StatusCode::OK, json!({"spawned": id_hint})),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel full, retry")
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler not running")
                }
            }
        }

        (Method::POST, path) if path.starts_with("/api/v1/agents/") && path.ends_with("/inject") => {
            let agent_id = path
                .strip_prefix("/api/v1/agents/")
                .and_then(|s| s.strip_suffix("/inject"))
                .unwrap_or("")
                .trim();
            if agent_id.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "agent_id must not be empty");
            }
            if !agent_id.bytes().all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')) {
                return error_response(StatusCode::BAD_REQUEST, "agent_id must match [a-zA-Z0-9_-]");
            }
            if body.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "request body required");
            }
            let val: serde_json::Value = match serde_json::from_slice(body) {
                Ok(v) => v,
                Err(e) => return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")),
            };
            let text = match val.get("text").and_then(|t| t.as_str()) {
                Some(t) if !t.is_empty() => t.to_string(),
                Some(_) => return error_response(StatusCode::BAD_REQUEST, "text must not be empty"),
                None => return error_response(StatusCode::BAD_REQUEST, "field 'text' required"),
            };
            if text.len() > 65_536 {
                return error_response(StatusCode::BAD_REQUEST, "text too large (max 64 KiB)");
            }
            let Some(tx) = &state.control_tx else {
                return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel not available");
            };
            let cmd = ControlCommand::Inject { agent_id: agent_id.to_string(), text };
            match tx.try_send(cmd) {
                Ok(()) => json_response(StatusCode::OK, json!({"injected": agent_id})),
                Err(mpsc::error::TrySendError::Full(_)) => {
                    error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel full, retry")
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler not running")
                }
            }
        }

        _ => error_response(StatusCode::NOT_FOUND, "not found"),
    }
}

fn parse_pagination(query: &str) -> (usize, usize) {
    let mut limit = MAX_MEMORY_LIMIT;
    let mut offset = 0usize;
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        match k {
            "limit"  => limit  = v.parse().unwrap_or(MAX_MEMORY_LIMIT).min(MAX_MEMORY_LIMIT),
            "offset" => offset = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    (limit, offset)
}

/// Start the management HTTP server. Returns the bound `SocketAddr`.
///
/// # Errors
/// Returns an error if:
/// - Binding fails (port in use, permission denied).
/// - The resolved address is not loopback (misconfiguration guard).
pub async fn start(
    bind_addr: &str,
    port: u16,
    snapshot: SharedSnapshot,
    memory_store: Option<Arc<dyn MemoryStore>>,
    broadcast_tx: broadcast::Sender<String>,
    recorder: Arc<FlightRecorder>,
    control_tx: Option<mpsc::Sender<ControlCommand>>,
) -> anyhow::Result<SocketAddr> {
    let addr = format!("{bind_addr}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("management: failed to bind {addr}: {e}"))?;
    let bound = listener.local_addr()?;

    anyhow::ensure!(
        bound.ip().is_loopback(),
        "management: refusing to bind on non-loopback address {bound} — API must be localhost-only"
    );

    recorder.record(
        "management",
        None,
        EventKind::ManagementStarted,
        json!({"addr": bound.to_string()}),
    );

    let state = Arc::new(ApiState {
        snapshot,
        memory_store,
        broadcast_tx,
        recorder,
        control_tx,
    });

    tracing::info!("management API listening on {bound}");

    tokio::spawn(async move {
        loop {
            let (stream, _peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("management: accept error: {e}");
                    continue;
                }
            };
            let io = TokioIo::new(stream);
            let state_clone = Arc::clone(&state);
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let s = Arc::clone(&state_clone);
                    async move { handle(s, req).await }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, RwLock};
    use surfaces::{SchedulerSnapshot, AgentSnapshot, AgentStatus};

    fn make_state(snap: SchedulerSnapshot) -> Arc<ApiState> {
        make_state_with_control(snap, None)
    }

    fn make_state_with_control(
        snap: SchedulerSnapshot,
        control_tx: Option<mpsc::Sender<ControlCommand>>,
    ) -> Arc<ApiState> {
        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap(),
        );
        Arc::new(ApiState {
            snapshot: Arc::new(RwLock::new(snap)),
            memory_store: None,
            broadcast_tx: tx,
            recorder,
            control_tx,
        })
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::GET, "/healthz", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn snapshot_returns_agents() {
        let mut snap = SchedulerSnapshot::default();
        snap.agents.push(AgentSnapshot {
            id: "a1".to_string(),
            status: AgentStatus::Running,
            turn: 2,
            context_tokens: 100,
            token_budget: 50_000,
            task_preview: "test task".to_string(),
            tools: vec![],
            short_term_previews: vec![],
            parent_id: None,
            accessible_server_names: vec![],
            capabilities_unrestricted: true,
            tier: None,
            pid: None,
        });
        let state = make_state(snap);
        let resp = route(state, Method::GET, "/api/v1/snapshot", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["agents"][0]["id"], "a1");
        assert_eq!(v["agents"][0]["status"], "running");
    }

    #[tokio::test]
    async fn snapshot_awaiting_child_has_status_detail() {
        let mut snap = SchedulerSnapshot::default();
        snap.agents.push(AgentSnapshot {
            id: "a2".to_string(),
            status: AgentStatus::AwaitingChild("child-1".to_string()),
            turn: 1,
            context_tokens: 0,
            token_budget: 0,
            task_preview: String::new(),
            tools: vec![],
            short_term_previews: vec![],
            parent_id: None,
            accessible_server_names: vec![],
            capabilities_unrestricted: true,
            tier: None,
            pid: None,
        });
        let state = make_state(snap);
        let resp = route(state, Method::GET, "/api/v1/snapshot", "", &[]).await;
        let body = collect_body(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["agents"][0]["status"], "awaiting_child");
        assert_eq!(v["agents"][0]["status_detail"], "child-1");
    }

    #[tokio::test]
    async fn unknown_route_returns_404() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::GET, "/nonexistent", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = collect_body(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].is_string());
    }

    #[tokio::test]
    async fn memory_route_without_store_returns_503() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::GET, "/api/v1/memory/myns", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn memory_route_nested_namespace_returns_400() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::GET, "/api/v1/memory/agent/key1", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn approvals_returns_empty_list() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::GET, "/api/v1/approvals", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn pagination_parse_defaults() {
        let (limit, offset) = parse_pagination("");
        assert_eq!(limit, 100);
        assert_eq!(offset, 0);
    }

    #[tokio::test]
    async fn pagination_parse_values() {
        let (limit, offset) = parse_pagination("limit=10&offset=5");
        assert_eq!(limit, 10);
        assert_eq!(offset, 5);
    }

    #[tokio::test]
    async fn pagination_parse_clamps_limit() {
        let (limit, _) = parse_pagination("limit=9999");
        assert_eq!(limit, 100);
    }

    #[tokio::test]
    async fn loopback_guard_rejects_non_loopback() {
        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap(),
        );
        // 0.0.0.0:0 binds successfully but is not loopback — guard should fire.
        let result = start(
            "0.0.0.0",
            0,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
            tx,
            recorder,
            None,
        )
        .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("non-loopback"), "expected non-loopback error, got: {msg}");
    }

    #[tokio::test]
    async fn snapshot_done_agent_no_status_detail() {
        let mut snap = SchedulerSnapshot::default();
        snap.agents.push(AgentSnapshot {
            id: "done-agent".to_string(),
            status: AgentStatus::Done,
            turn: 5,
            context_tokens: 0,
            token_budget: 0,
            task_preview: String::new(),
            tools: vec![],
            short_term_previews: vec![],
            parent_id: None,
            accessible_server_names: vec![],
            capabilities_unrestricted: true,
            tier: None,
            pid: None,
        });
        let state = make_state(snap);
        let resp = route(state, Method::GET, "/api/v1/snapshot", "", &[]).await;
        let body = collect_body(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["agents"][0]["status"], "done");
        // status_detail must be absent for non-tuple variants
        assert!(v["agents"][0]["status_detail"].is_null());
    }

    #[tokio::test]
    async fn healthz_content_type_is_json() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::GET, "/healthz", "", &[]).await;
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"), "expected json content-type, got: {ct}");
    }

    #[tokio::test]
    async fn post_to_snapshot_returns_404() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/snapshot", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    fn make_snap_with_action(id: &str, agent_id: &str) -> SchedulerSnapshot {
        use surfaces::PendingActionView;
        let mut snap = SchedulerSnapshot::default();
        snap.pending_actions.push(PendingActionView {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            kind: "write_file".to_string(),
            risk: "medium".to_string(),
            summary: "test action".to_string(),
            args_json: "{}".to_string(),
            age_secs: 0,
        });
        snap
    }

    #[tokio::test]
    async fn approve_returns_503_without_control_tx() {
        let snap = make_snap_with_action("act_0", "agent-1");
        let state = make_state(snap);
        let resp = route(state, Method::POST, "/api/v1/approvals/act_0/approve", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let ct = resp.headers().get("retry-after").map(|v| v.to_str().unwrap_or(""));
        assert_eq!(ct, Some("1"));
    }

    #[tokio::test]
    async fn deny_returns_503_without_control_tx() {
        let snap = make_snap_with_action("act_0", "agent-1");
        let state = make_state(snap);
        let resp = route(state, Method::POST, "/api/v1/approvals/act_0/deny", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let ra = resp.headers().get("retry-after").map(|v| v.to_str().unwrap_or(""));
        assert_eq!(ra, Some("1"), "deny 503 must carry Retry-After: 1");
    }

    #[tokio::test]
    async fn approve_empty_id_returns_400() {
        let state = make_state(SchedulerSnapshot::default());
        // Path that strips to empty: "/api/v1/approvals//approve"
        let resp = route(state, Method::POST, "/api/v1/approvals//approve", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn approve_unknown_id_returns_404() {
        let state = make_state(SchedulerSnapshot::default()); // no pending actions
        let resp = route(state, Method::POST, "/api/v1/approvals/act_999/approve", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn approve_happy_path_sends_command() {
        use crate::control::ControlCommand;
        let (tx, mut rx) = mpsc::channel::<ControlCommand>(4);
        let snap = make_snap_with_action("act_0", "agent-1");
        let state = make_state_with_control(snap, Some(tx));
        let resp = route(state, Method::POST, "/api/v1/approvals/act_0/approve", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let cmd = rx.try_recv().expect("command should have been sent");
        match cmd {
            ControlCommand::Approve { id, .. } => assert_eq!(id, "act_0"),
            _ => panic!("expected Approve command"),
        }
    }

    #[tokio::test]
    async fn deny_with_reason_sends_command() {
        use crate::control::ControlCommand;
        let (tx, mut rx) = mpsc::channel::<ControlCommand>(4);
        let snap = make_snap_with_action("act_0", "agent-1");
        let state = make_state_with_control(snap, Some(tx));
        let body = br#"{"reason":"too risky"}"#;
        let resp = route(state, Method::POST, "/api/v1/approvals/act_0/deny", "", body).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let cmd = rx.try_recv().expect("command should have been sent");
        match cmd {
            ControlCommand::Reject { id, reason } => {
                assert_eq!(id, "act_0");
                assert_eq!(reason, Some("too risky".to_string()));
            }
            _ => panic!("expected Reject command"),
        }
    }

    #[tokio::test]
    async fn sse_content_type_and_framing() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::GET, "/api/v1/events", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("text/event-stream"), "expected event-stream, got: {ct}");
        assert_eq!(
            resp.headers().get("cache-control").unwrap().to_str().unwrap(),
            "no-cache"
        );
    }

    async fn collect_body(resp: Response<BoxBody>) -> Bytes {
        resp.into_body().collect().await.unwrap().to_bytes()
    }
}
