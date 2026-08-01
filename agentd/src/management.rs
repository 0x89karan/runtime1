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
//!   GET  /api/v1/runs?from=&to=&agent_id=&parent_id=&status=&limit=  → 200 [RunRecord, ...] (ux.11b)
//!   GET  /api/v1/brief[?n=K]                      → 200 {brief|briefs, approvals_pending, server_now} | 503 (ux.11c)
//!     (`server_now` is the server's unix clock, added by attn.1a so a consumer can compute
//!      brief age — `server_now - created_at` — and say "the pipeline has stopped running")
//!   GET  /api/v1/events                          → 200 text/event-stream (SSE)
//!   POST /api/v1/spawn                           → 200 | 400 | 503 (orch.1)
//!   POST /api/v1/agents/:id/inject               → 200 | 400 | 503 (orch.1)
//!   POST /api/v1/agents/:id/cancel               → 200 {cancelled,count} | 400 | 404 | 503 (ux.13)
//!     (`cancelled` is the agent id; `count` is the whole cascaded subtree — what `agentctl cancel`
//!      and the TUI's `[x]` confirm both report. The FUSE path has no confirmation channel, so it
//!      cannot report either.)
//!   POST /api/v1/agents/:id/caps                 → 200 {agent,old,new} | 400 (widening / inert) | 404 | 503 (ux.13)
//!     (body: {"capabilities":[…]}; revoke/narrow-only — the new set must be covered by the old)
//!   POST /api/v1/budget/reset                    → 200 {target,spent_before,reset_to,window_start} | 400 | 404 | 503 (ux.8′)
//!   POST /api/v1/budget/set                       → 200 {target,old_limit,limit} | 400 (incl. global) | 404 | 503 (ux.11a)
//!     (per-agent native-tier only; universal-tier agents are proxy-metered and return 404)
//!   GET  /api/v1/credentials                     → 200 CredentialSnapshot JSON (cred.5)
//!   POST /api/v1/credentials/:provider/reset-attention → 200 | 404 (cred.7)

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

use crate::capability::Capability;
use crate::control::ControlCommand;
use crate::events::EventKind;

/// A spawned agent's capability is "privileged" unless it is in a small read-only-local safe
/// set. DENY-BY-DEFAULT (cap.4 / AUDIT-v0.97 P2-3; Codex review): a hand-enumerated *denylist*
/// is fragile — agent-level `Credential` is INERT, while `Mcp{google_oauth}` (NOT `Credential`)
/// is what actually grants live Gmail (the broker derives the provider allowlist from the MCP
/// *server's* own cap, not the agent's). So `/spawn` from an untrusted `:7999` caller may mint
/// ONLY caps from the safe set without the operator opt-in; anything that grants tools (`Mcp`),
/// network, writes, spawning, sealed-jobs, brief-publish, or credentials requires
/// `AGENTOS_ALLOW_PRIVILEGED_SPAWN=1`. (Unrestricted `capabilities: None` = every cap → also
/// privileged; handled at the call site.)
///
/// `FsRead` is DELIBERATELY NOT in the safe set (AUDIT-v0.97 holistic review, Codex High):
/// its prefix is caller-controlled and unbounded, so `FsRead { prefix: "/" }` would satisfy
/// every absolute path — `read_file`/`list_dir` on the egress signing key, OAuth token cache,
/// checkpoints, and mounted secrets. "read-only-local" is only safe when the *scope* is bounded;
/// a caller-supplied prefix is not. The bounded read paths a benign untrusted caller needs are
/// covered by `KbRead { segment }` (a named KB segment) and `RunsRead` (run history). An operator
/// who genuinely wants an arbitrary-filesystem read spawn sets `AGENTOS_ALLOW_PRIVILEGED_SPAWN=1`.
fn is_privileged_spawn_cap(c: &Capability) -> bool {
    !matches!(
        c,
        Capability::KbRead { .. } | Capability::RunsRead
    )
}

/// Operator opt-in for privileged /spawn (env, per the secrets-from-env invariant).
fn privileged_spawn_allowed() -> bool {
    std::env::var("AGENTOS_ALLOW_PRIVILEGED_SPAWN")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}
use crate::flight_recorder::FlightRecorder;
use crate::memory::MemoryStore;
use surfaces::SharedSnapshot;

const MAX_MEMORY_LIMIT: usize = 100;

/// Shared state threaded into every request handler.
struct ApiState {
    snapshot:           SharedSnapshot,
    memory_store:       Option<Arc<dyn MemoryStore>>,
    /// Durable run-history store (ux.11b). None when the store failed to open.
    runs_store:         Option<Arc<crate::runs::RunsStore>>,
    broadcast_tx:       broadcast::Sender<String>,
    recorder:           Arc<FlightRecorder>,
    /// Sender half of the scheduler control channel. None when not wired (non-Linux or test).
    control_tx:         Option<mpsc::Sender<ControlCommand>>,
    /// Credential gateway for reset-attention endpoint (cred.7). None when disabled.
    credential_gateway: Option<Arc<crate::credential::CredentialGateway>>,
    /// Route-scoped shared secret for the approve/deny endpoints (ux.12). When `Some`,
    /// `POST /api/v1/approvals/*/{approve,deny}` requires a matching `X-Approval-Token`
    /// header (constant-time compared). When `None`, the routes are open (pre-ux.12
    /// behavior) — the CoS deployment that runs the Telegram sidecar sets the secret;
    /// non-secret deployments are no worse off than before. Full API auth stays ux.5.
    approval_secret:    Option<String>,
}

/// Constant-time check of the `X-Approval-Token` header against the configured secret.
/// `None` secret ⇒ open (returns true). `Some` secret ⇒ requires an exactly-matching
/// header (constant-time to avoid a timing oracle on the token).
fn approval_token_ok(secret: Option<&str>, provided: Option<&str>) -> bool {
    match secret {
        None => true,
        Some(s) => match provided {
            Some(p) => constant_time_eq(s.as_bytes(), p.as_bytes()),
            None => false,
        },
    }
}

/// Constant-time byte-slice equality (no timing oracle on the matching prefix). The
/// length comparison short-circuits — token length is not secret; the content is.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// True for every MUTATING route the approval secret gates (cap.4 / AUDIT-v0.97 P2-3).
/// The ux.12 gate covered only approve/deny, leaving the strictly-more-powerful routes
/// (spawn — which mints capabilities — inject, budget, cancel, caps) unauthenticated on the
/// identical `:7999` surface. The token now gates the whole mutating surface; GET/read routes
/// stay ungated. When `AGENTOS_APPROVAL_SECRET` is unset, all routes are open (pre-ux.12).
fn is_mutating_route(method: &Method, path: &str) -> bool {
    if *method != Method::POST {
        return false;
    }
    match path {
        "/api/v1/spawn" | "/api/v1/budget/reset" | "/api/v1/budget/set" => true,
        p if p.starts_with("/api/v1/approvals/")
            && (p.ends_with("/approve") || p.ends_with("/deny")) => true,
        p if p.starts_with("/api/v1/agents/")
            && (p.ends_with("/inject") || p.ends_with("/cancel") || p.ends_with("/caps")) => true,
        p if p.starts_with("/api/v1/credentials/") && p.ends_with("/reset-attention") => true,
        _ => false,
    }
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

    // ux.12 + cap.4: approval-token gate over the whole MUTATING surface (not just
    // approve/deny). Enforced here (handle) rather than in route() so the header is in
    // scope and route()'s signature stays test-stable.
    if is_mutating_route(&method, &path) {
        let provided = req
            .headers()
            .get("x-approval-token")
            .and_then(|v| v.to_str().ok());
        if !approval_token_ok(state.approval_secret.as_deref(), provided) {
            let resp = error_response(StatusCode::UNAUTHORIZED, "missing or invalid X-Approval-Token");
            state.recorder.record(
                "management",
                None,
                EventKind::ManagementRequest,
                json!({"method": method.as_str(), "path": path, "status": 401}),
            );
            return Ok(resp);
        }
    }

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
            let mut snap = {
                let guard = match state.snapshot.read() {
                    Ok(g) => g,
                    Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "snapshot lock poisoned"),
                };
                guard.clone()
            };
            // ux.2b: merge the READ-TIME Idle signal per agent against the reader's clock, so
            // idle advances between polls without a new scheduler snapshot (mirrors FUSE).
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            for agent in &mut snap.agents {
                if let Some(idle) = agent.idle_signal(now, surfaces::IDLE_THRESHOLD_SECS) {
                    agent.attention.push(idle);
                }
            }
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

        (Method::GET, "/api/v1/runs") => {
            // ux.11b: read-only durable run history. Filters via query params:
            // ?from=&to=&agent_id=&parent_id=&status=&limit= (limit clamped to [1,100]).
            let Some(store) = &state.runs_store else {
                return error_response(StatusCode::SERVICE_UNAVAILABLE, "run history not configured");
            };
            let mut filter = crate::runs::RunFilter::default();
            for pair in query.split('&').filter(|s| !s.is_empty()) {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                match k {
                    "from"      => filter.from = v.parse().ok(),
                    "to"        => filter.to = v.parse().ok(),
                    "agent_id"  => filter.agent_id = Some(v.to_string()),
                    "parent_id" => filter.parent_id = Some(v.to_string()),
                    "status"    => filter.status = Some(v.to_string()),
                    "limit"     => filter.limit = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
            let store = Arc::clone(store);
            match tokio::task::spawn_blocking(move || store.list(&filter)).await {
                Ok(Ok(recs)) => json_response(StatusCode::OK, json!(recs)),
                Ok(Err(e))   => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
                Err(e)       => error_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("runs query join: {e}")),
            }
        }

        (Method::GET, "/api/v1/brief") => {
            // ux.11c: the durable morning brief (pull surface). Returns the latest brief
            // (or ?n=K for the last K, newest-first) with a LIVE "approvals_pending"
            // overlay — pending approvals are current scheduler state (Eng G1), not part
            // of the persisted, at-compose-time record. `brief` is null when none exists yet.
            let Some(store) = &state.runs_store else {
                return error_response(StatusCode::SERVICE_UNAVAILABLE, "run history not configured");
            };
            // Live approvals count from the snapshot. Caveat: pending_actions is capped at
            // ≤100 entries, so this undercounts past 100 (pathological for single-tenant).
            let approvals_pending = match state.snapshot.read() {
                Ok(g) => g.pending_actions.len(),
                Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "snapshot lock poisoned"),
            };
            let n: Option<usize> = query
                .split('&')
                .filter_map(|p| p.split_once('='))
                .find(|(k, _)| *k == "n")
                .and_then(|(_, v)| v.parse().ok());
            // attn.1a liveness: stamp the SERVER's clock into every brief response.
            // A brief that is eight days old renders identically to one written five
            // minutes ago, which is exactly how "three briefs in fifteen days" stayed
            // invisible. The consumer computes age as `server_now - created_at`.
            //
            // Why the server's clock and not the client's: `agentctl brief --url` can
            // point at another host, so differencing two different clocks would report
            // skew as staleness. Both numbers here come from the same clock.
            //
            // Why a raw timestamp rather than a `stale` boolean: the freshness threshold
            // depends on the operator's configured cron cadence, and this route does not
            // know it. Ship the fact, let the renderer apply the policy.
            let server_now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let store = Arc::clone(store);
            match n {
                Some(k) => match tokio::task::spawn_blocking(move || store.list_briefs(k)).await {
                    Ok(Ok(briefs)) => json_response(StatusCode::OK, json!({ "briefs": briefs, "approvals_pending": approvals_pending, "server_now": server_now })),
                    Ok(Err(e))     => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
                    Err(e)         => error_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("brief query join: {e}")),
                },
                None => match tokio::task::spawn_blocking(move || store.latest_brief()).await {
                    Ok(Ok(brief)) => json_response(StatusCode::OK, json!({ "brief": brief, "approvals_pending": approvals_pending, "server_now": server_now })),
                    Ok(Err(e))    => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
                    Err(e)        => error_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("brief query join: {e}")),
                },
            }
        }

        (Method::GET, "/api/v1/events") => {
            let mut rx = state.broadcast_tx.subscribe();

            // Build an SSE stream: each broadcast line becomes `data: <line>\n\n`.
            // A 30 s keepalive comment (`: ping`) is injected to prevent load-balancer timeouts.
            let stream = async_stream::stream! {
                let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(30));
                keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                keepalive.tick().await; // consume the first tick immediately
                loop {
                    tokio::select! {
                        msg = rx.recv() => {
                            match msg {
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
                        _ = keepalive.tick() => {
                            // SSE comment — clients ignore it; keeps TCP alive through proxies.
                            yield Ok::<Frame<Bytes>, Infallible>(Frame::data(Bytes::from_static(b": ping\n\n")));
                        }
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
            let mut req: crate::control::OperatorSpawnRequest = match serde_json::from_slice(body) {
                Ok(r) => r,
                Err(e) => return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")),
            };
            if req.task.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "task must not be empty");
            }
            // cap.4 / AUDIT-v0.97 P2-3: /spawn mints caller-supplied capabilities verbatim.
            // Refuse privileged caps from a `:7999` caller unless the operator opted in — closes
            // "any bridge peer spawns a full-Gmail+Spawn agent". Operator-driven privileged
            // spawns set AGENTOS_ALLOW_PRIVILEGED_SPAWN=1.
            if !privileged_spawn_allowed() {
                // Unrestricted (None) grants every capability → privileged. A concrete list is
                // privileged if ANY cap is outside the read-only-local safe set (deny-by-default).
                let refused: Option<String> = match req.capabilities.as_deref() {
                    None => Some("unrestricted (all capabilities)".to_string()),
                    Some(caps) => caps.iter().find(|c| is_privileged_spawn_cap(c)).map(|c| format!("{c:?}")),
                };
                if let Some(bad) = refused {
                    return error_response(
                        StatusCode::BAD_REQUEST,
                        &format!("spawn refused: {bad} is privileged; set AGENTOS_ALLOW_PRIVILEGED_SPAWN=1 to allow operator-driven privileged spawns"),
                    );
                }
            }
            let Some(tx) = &state.control_tx else {
                return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel not available");
            };
            // Wire a confirmation channel so we can return the resolved agent ID (ar-02).
            let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel::<String>();
            req.confirm_tx = Some(confirm_tx);
            let cmd = ControlCommand::Spawn(req);
            match tx.try_send(cmd) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel full, retry");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler not running");
                }
            }
            // Await confirmation (2 s) then return 201 with the assigned agent_id.
            match tokio::time::timeout(std::time::Duration::from_secs(2), confirm_rx).await {
                Ok(Ok(agent_id)) => json_response(StatusCode::CREATED, json!({"agent_id": agent_id})),
                Ok(Err(_)) => error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler closed confirmation channel"),
                Err(_) => error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "timed out waiting for agent creation"),
            }
        }

        (Method::POST, "/api/v1/budget/reset") => {
            // ux.8′ D2 manual escape hatch. Body: {"target":"global"} or
            // {"target":{"agent":"<id>"}}. Confirm-channel (like spawn) so we can
            // report old→new and 404 an unknown agent — never fire-and-forget.
            if body.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "request body required");
            }
            #[derive(serde::Deserialize)]
            struct ResetReq {
                target: crate::control::BudgetTarget,
            }
            let req: ResetReq = match serde_json::from_slice(body) {
                Ok(r) => r,
                Err(e) => return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")),
            };
            let target_label = match &req.target {
                crate::control::BudgetTarget::Global => "global".to_string(),
                crate::control::BudgetTarget::Agent(id) => id.clone(),
            };
            let Some(tx) = &state.control_tx else {
                return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel not available");
            };
            let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel::<Result<(u64, u64), String>>();
            let cmd = ControlCommand::ResetBudget { target: req.target, confirm_tx: Some(confirm_tx) };
            match tx.try_send(cmd) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel full, retry");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler not running");
                }
            }
            match tokio::time::timeout(std::time::Duration::from_secs(2), confirm_rx).await {
                Ok(Ok(Ok((spent_before, window_start)))) => json_response(
                    StatusCode::OK,
                    json!({ "target": target_label, "spent_before": spent_before, "reset_to": 0, "window_start": window_start }),
                ),
                Ok(Ok(Err(e))) => error_response(StatusCode::NOT_FOUND, &e),
                Ok(Err(_)) => error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler closed confirmation channel"),
                Err(_) => error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "timed out waiting for budget reset"),
            }
        }

        (Method::POST, "/api/v1/budget/set") => {
            // ux.11a SetBudget. Body: {"target":{"agent":"<id>"},"limit":50000}
            // (limit:0 = unlimited). Per-agent only — the global ceiling is immutable
            // config (400). Confirm-channel reports old→new; 404 for an unknown agent.
            if body.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "request body required");
            }
            #[derive(serde::Deserialize)]
            struct SetReq {
                target: crate::control::BudgetTarget,
                limit:  u64,
            }
            let req: SetReq = match serde_json::from_slice(body) {
                Ok(r) => r,
                Err(e) => return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")),
            };
            let id = match &req.target {
                crate::control::BudgetTarget::Global => {
                    return error_response(StatusCode::BAD_REQUEST, "global budget is not runtime-settable; target a specific agent");
                }
                crate::control::BudgetTarget::Agent(id) => id.clone(),
            };
            // Validate the agent id here (400) rather than turning malformed ids into
            // scheduler traffic that comes back as a misleading 404 (Codex ship review).
            if id.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "agent id must not be empty");
            }
            if !id.bytes().all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')) {
                return error_response(StatusCode::BAD_REQUEST, "agent id must match [a-zA-Z0-9_-]");
            }
            let Some(tx) = &state.control_tx else {
                return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel not available");
            };
            let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel::<Result<(u64, u64), String>>();
            let cmd = ControlCommand::SetBudget { target: req.target, limit: req.limit, confirm_tx: Some(confirm_tx) };
            match tx.try_send(cmd) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel full, retry");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler not running");
                }
            }
            match tokio::time::timeout(std::time::Duration::from_secs(2), confirm_rx).await {
                Ok(Ok(Ok((old_limit, new_limit)))) => json_response(
                    StatusCode::OK,
                    json!({ "target": id, "old_limit": old_limit, "limit": new_limit }),
                ),
                Ok(Ok(Err(e))) => error_response(StatusCode::NOT_FOUND, &e),
                Ok(Err(_)) => error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler closed confirmation channel"),
                Err(_) => error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "timed out waiting for budget set"),
            }
        }

        (Method::POST, path) if path.starts_with("/api/v1/agents/") && path.ends_with("/cancel") => {
            // ux.13 Cancel. Cascade-cancels the agent's spawned subtree at the next step
            // boundary. Confirm-channel reports the node count; 404 for an unknown agent.
            let agent_id = path
                .strip_prefix("/api/v1/agents/")
                .and_then(|s| s.strip_suffix("/cancel"))
                .unwrap_or("")
                .trim();
            if agent_id.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "agent id must not be empty");
            }
            let Some(tx) = &state.control_tx else {
                return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel not available");
            };
            let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel::<Result<u64, String>>();
            let cmd = ControlCommand::Cancel { agent_id: agent_id.to_string(), confirm_tx: Some(confirm_tx) };
            match tx.try_send(cmd) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel full, retry");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler not running");
                }
            }
            match tokio::time::timeout(std::time::Duration::from_secs(2), confirm_rx).await {
                Ok(Ok(Ok(count))) => json_response(StatusCode::OK, json!({ "cancelled": agent_id, "count": count })),
                Ok(Ok(Err(e))) => error_response(StatusCode::NOT_FOUND, &e),
                Ok(Err(_)) => error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler closed confirmation channel"),
                Err(_) => error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "timed out waiting for cancel"),
            }
        }

        (Method::POST, path) if path.starts_with("/api/v1/agents/") && path.ends_with("/caps") => {
            // ux.13 SetCaps (revoke/narrow-only). Body: {"capabilities":[...]}.
            // 404 for an unknown agent; 400 for a widening or inert-cap request.
            let agent_id = path
                .strip_prefix("/api/v1/agents/")
                .and_then(|s| s.strip_suffix("/caps"))
                .unwrap_or("")
                .trim();
            if agent_id.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "agent id must not be empty");
            }
            if body.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "request body required");
            }
            #[derive(serde::Deserialize)]
            struct CapsReq {
                capabilities: Vec<crate::capability::Capability>,
            }
            let req: CapsReq = match serde_json::from_slice(body) {
                Ok(r) => r,
                Err(e) => return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON: {e}")),
            };
            let Some(tx) = &state.control_tx else {
                return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel not available");
            };
            let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel::<Result<(usize, usize), String>>();
            let cmd = ControlCommand::SetCaps {
                agent_id: agent_id.to_string(),
                capabilities: req.capabilities,
                confirm_tx: Some(confirm_tx),
            };
            match tx.try_send(cmd) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    return error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "control channel full, retry");
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler not running");
                }
            }
            match tokio::time::timeout(std::time::Duration::from_secs(2), confirm_rx).await {
                Ok(Ok(Ok((old, new)))) => json_response(StatusCode::OK, json!({ "agent": agent_id, "old": old, "new": new })),
                // "not found" → 404; narrow-only / inert-cap rejections → 400.
                Ok(Ok(Err(e))) => {
                    let code = if e.contains("not found") { StatusCode::NOT_FOUND } else { StatusCode::BAD_REQUEST };
                    error_response(code, &e)
                }
                Ok(Err(_)) => error_response(StatusCode::SERVICE_UNAVAILABLE, "scheduler closed confirmation channel"),
                Err(_) => error_response_with_retry(StatusCode::SERVICE_UNAVAILABLE, "timed out waiting for set-caps"),
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

        (Method::GET, "/api/v1/credentials") => {
            let cred_snap = {
                let guard = match state.snapshot.read() {
                    Ok(g) => g,
                    Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, "snapshot lock poisoned"),
                };
                guard.credential_snapshot.clone()
            };
            match cred_snap {
                Some(cs) => match serde_json::to_value(&cs) {
                    Ok(v) => json_response(StatusCode::OK, v),
                    Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
                },
                None => json_response(StatusCode::OK, json!({"enabled": false})),
            }
        }

        (Method::POST, path)
            if path.starts_with("/api/v1/credentials/")
                && path.ends_with("/reset-attention") =>
        {
            let provider = path
                .strip_prefix("/api/v1/credentials/")
                .and_then(|s| s.strip_suffix("/reset-attention"))
                .unwrap_or("")
                .trim();
            if provider.is_empty() {
                return error_response(StatusCode::BAD_REQUEST, "provider must not be empty");
            }
            let Some(gw) = &state.credential_gateway else {
                return error_response(StatusCode::SERVICE_UNAVAILABLE, "credential gateway not configured");
            };
            if gw.reset_attention(provider).await {
                json_response(StatusCode::OK, json!({"reset": provider}))
            } else {
                error_response(StatusCode::NOT_FOUND, "provider not found or not configured")
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
/// - The resolved address is not loopback and `allow_non_loopback` is false
///   (misconfiguration guard; the API is unauthenticated).
#[allow(clippy::too_many_arguments)]
pub async fn start(
    bind_addr: &str,
    port: u16,
    allow_non_loopback: bool,
    snapshot: SharedSnapshot,
    memory_store: Option<Arc<dyn MemoryStore>>,
    runs_store: Option<Arc<crate::runs::RunsStore>>,
    broadcast_tx: broadcast::Sender<String>,
    recorder: Arc<FlightRecorder>,
    control_tx: Option<mpsc::Sender<ControlCommand>>,
    credential_gateway: Option<Arc<crate::credential::CredentialGateway>>,
    approval_secret: Option<String>,
) -> anyhow::Result<SocketAddr> {
    let addr = format!("{bind_addr}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("management: failed to bind {addr}: {e}"))?;
    let bound = listener.local_addr()?;

    anyhow::ensure!(
        bound.ip().is_loopback() || allow_non_loopback,
        "management: refusing to bind on non-loopback address {bound} — API must be localhost-only \
         (set [management] allow_non_loopback = true to opt in explicitly)"
    );

    let non_loopback_opt_in = !bound.ip().is_loopback();
    if non_loopback_opt_in {
        tracing::warn!(
            addr = %bound,
            "management API bound to a non-loopback address via allow_non_loopback — \
             the API is unauthenticated; see THREAT_MODEL.md §9"
        );
    }

    recorder.record(
        "management",
        None,
        EventKind::ManagementStarted,
        json!({"addr": bound.to_string(), "non_loopback_opt_in": non_loopback_opt_in}),
    );

    let state = Arc::new(ApiState {
        snapshot,
        memory_store,
        runs_store,
        broadcast_tx,
        recorder,
        control_tx,
        credential_gateway,
        approval_secret,
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
            runs_store: None,
            broadcast_tx: tx,
            recorder,
            control_tx,
            credential_gateway: None,
            approval_secret: None,
        })
    }

    #[test]
    fn approval_token_gate() {
        // No secret configured → open (pre-ux.12 behavior).
        assert!(approval_token_ok(None, None));
        assert!(approval_token_ok(None, Some("anything")));
        // Secret configured → exact match required (constant-time).
        assert!(approval_token_ok(Some("s3cr3t"), Some("s3cr3t")));
        assert!(!approval_token_ok(Some("s3cr3t"), Some("wrong")));
        assert!(!approval_token_ok(Some("s3cr3t"), None));
        assert!(!approval_token_ok(Some("s3cr3t"), Some("")));
        // Length-mismatch must not match (and must not panic — constant_time handles it).
        assert!(!approval_token_ok(Some("s3cr3t"), Some("s3cr3t-longer")));
    }

    #[test]
    fn mutating_route_matcher() {
        // cap.4: the token now gates the WHOLE mutating surface, not just approve/deny.
        for p in [
            "/api/v1/approvals/act_1/approve",
            "/api/v1/approvals/act_1/deny",
            "/api/v1/spawn",
            "/api/v1/budget/set",
            "/api/v1/budget/reset",
            "/api/v1/agents/a/inject",
            "/api/v1/agents/a/cancel",
            "/api/v1/agents/a/caps",
            "/api/v1/credentials/google/reset-attention",
        ] {
            assert!(is_mutating_route(&Method::POST, p), "POST {p} must be gated");
        }
        // Reads and non-POST are NOT gated.
        assert!(!is_mutating_route(&Method::GET, "/api/v1/approvals"));
        assert!(!is_mutating_route(&Method::GET, "/api/v1/spawn"));
        assert!(!is_mutating_route(&Method::GET, "/api/v1/brief"));
        assert!(!is_mutating_route(&Method::POST, "/api/v1/snapshot"));
    }

    #[test]
    fn privileged_spawn_cap_classification() {
        use crate::capability::{Capability, CredentialProvider};
        // Deny-by-default: everything outside the read-only-local safe set is privileged.
        assert!(is_privileged_spawn_cap(&Capability::Spawn));
        assert!(is_privileged_spawn_cap(&Capability::RunJob));
        assert!(is_privileged_spawn_cap(&Capability::Credential { provider: CredentialProvider::Google }));
        // The REAL live-Gmail vector Codex caught: Mcp{google_oauth} grants tools without a
        // Credential cap — it MUST be privileged (the old denylist missed it).
        assert!(is_privileged_spawn_cap(&Capability::Mcp { server: "google_oauth".into(), tools: vec![] }));
        assert!(is_privileged_spawn_cap(&Capability::FsWrite { prefix: "/".into() }));
        assert!(is_privileged_spawn_cap(&Capability::KbWrite { segment: "x".into() }));
        assert!(is_privileged_spawn_cap(&Capability::BriefPublish));
        assert!(is_privileged_spawn_cap(&Capability::Net { hosts: vec![], ports: vec![] }));
        // FsRead is privileged too (AUDIT-v0.97 holistic review, Codex High): its prefix is
        // caller-controlled, so FsRead{"/"} would read the whole filesystem — egress signing
        // key, OAuth cache, secrets. "read-only-local" is only safe when the scope is bounded.
        assert!(is_privileged_spawn_cap(&Capability::FsRead { prefix: "/".into() }));
        assert!(is_privileged_spawn_cap(&Capability::FsRead { prefix: "/data".into() }));
        // Safe set: only the bounded read paths a benign untrusted caller needs.
        assert!(!is_privileged_spawn_cap(&Capability::RunsRead));
        assert!(!is_privileged_spawn_cap(&Capability::KbRead { segment: "x".into() }));
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
            attention: vec![],
            last_event_at_unix: u64::MAX,
            status: AgentStatus::Running,
            turn: 2,
            context_tokens: 100,
            token_budget: 50_000,
            windowed_spent: 100,
            task_preview: "test task".to_string(),
            tools: vec![],
            short_term_previews: vec![],
            parent_id: None,
            accessible_server_names: vec![],
            capabilities_unrestricted: true,
            tier: None,
            pid: None,
            credential_providers: vec![],
            credential_request_counts: std::collections::HashMap::new(),
            credential_denied_counts:  std::collections::HashMap::new(),
            credential_last_access_at: std::collections::HashMap::new(),
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
            attention: vec![],
            last_event_at_unix: u64::MAX,
            status: AgentStatus::AwaitingChild("child-1".to_string()),
            turn: 1,
            context_tokens: 0,
            token_budget: 0,
            windowed_spent: 0,
            task_preview: String::new(),
            tools: vec![],
            short_term_previews: vec![],
            parent_id: None,
            accessible_server_names: vec![],
            capabilities_unrestricted: true,
            tier: None,
            pid: None,
            credential_providers: vec![],
            credential_request_counts: std::collections::HashMap::new(),
            credential_denied_counts:  std::collections::HashMap::new(),
            credential_last_access_at: std::collections::HashMap::new(),
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
        // 0.0.0.0:0 binds successfully but is not loopback — guard should fire
        // when allow_non_loopback is unset (the default).
        let result = start(
            "0.0.0.0",
            0,
            false,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
            None,
            tx,
            recorder,
            None,
            None,
            None,
        )
        .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("non-loopback"), "expected non-loopback error, got: {msg}");
    }

    #[tokio::test]
    async fn loopback_guard_allows_non_loopback_with_opt_in() {
        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap(),
        );
        // Same non-loopback bind, but allow_non_loopback=true is the explicit
        // deployment opt-in — the guard must permit it.
        let result = start(
            "0.0.0.0",
            0,
            true,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
            None,
            tx,
            recorder,
            None,
            None,
            None,
        )
        .await;
        assert!(result.is_ok(), "expected bind to succeed with allow_non_loopback=true, got: {result:?}");
    }

    #[tokio::test]
    async fn management_started_event_flags_non_loopback_opt_in() {
        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap(),
        );
        start(
            "0.0.0.0",
            0,
            true,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
            None,
            tx,
            recorder,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let logged = std::fs::read_to_string(tmp.path()).unwrap();
        let event: serde_json::Value = logged
            .lines()
            .find_map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).ok()?;
                (v["kind"] == "management_started").then_some(v)
            })
            .expect("management_started event must be recorded");
        assert_eq!(
            event["data"]["non_loopback_opt_in"], true,
            "management_started event must flag non_loopback_opt_in so operators \
             have an audit signal when the unauthenticated API bypass is active"
        );
    }

    #[tokio::test]
    async fn loopback_guard_allows_loopback_default() {
        // Completes the guard's truth table (ux.0b coverage audit): the other two
        // combinations bind on the default/most common path — loopback with the
        // opt-in unset, and loopback with the opt-in set as a harmless no-op.
        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap(),
        );
        let result = start(
            "127.0.0.1",
            0,
            false,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
            None,
            tx,
            recorder,
            None,
            None,
            None,
        )
        .await;
        assert!(result.is_ok(), "expected loopback bind to succeed by default, got: {result:?}");
    }

    #[tokio::test]
    async fn management_started_event_false_on_loopback_default() {
        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap(),
        );
        start(
            "127.0.0.1",
            0,
            false,
            Arc::new(RwLock::new(SchedulerSnapshot::default())),
            None,
            None,
            tx,
            recorder,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let logged = std::fs::read_to_string(tmp.path()).unwrap();
        let event: serde_json::Value = logged
            .lines()
            .find_map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).ok()?;
                (v["kind"] == "management_started").then_some(v)
            })
            .expect("management_started event must be recorded");
        assert_eq!(
            event["data"]["non_loopback_opt_in"], false,
            "management_started event must NOT flag non_loopback_opt_in on the default loopback path"
        );
    }

    #[tokio::test]
    async fn snapshot_done_agent_no_status_detail() {
        let mut snap = SchedulerSnapshot::default();
        snap.agents.push(AgentSnapshot {
            id: "done-agent".to_string(),
            attention: vec![],
            last_event_at_unix: u64::MAX,
            status: AgentStatus::Done,
            turn: 5,
            context_tokens: 0,
            token_budget: 0,
            windowed_spent: 0,
            task_preview: String::new(),
            tools: vec![],
            short_term_previews: vec![],
            parent_id: None,
            accessible_server_names: vec![],
            capabilities_unrestricted: true,
            tier: None,
            pid: None,
            credential_providers: vec![],
            credential_request_counts: std::collections::HashMap::new(),
            credential_denied_counts:  std::collections::HashMap::new(),
            credential_last_access_at: std::collections::HashMap::new(),
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

    // ux.13 route wiring: cancel/caps mirror the budget/set confirm-channel handler.
    #[tokio::test]
    async fn cancel_route_503_without_control_tx() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/agents/solo/cancel", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn cancel_route_empty_id_400() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/agents//cancel", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn caps_route_requires_body_400() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/agents/solo/caps", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn caps_route_bad_json_400() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/agents/solo/caps", "", b"{not json}").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
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
    async fn budget_reset_empty_body_returns_400() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/budget/reset", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn budget_reset_malformed_body_returns_400() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/budget/reset", "", b"{not json}").await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn budget_reset_without_control_tx_returns_503() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/budget/reset", "", br#"{"target":"global"}"#).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn budget_reset_happy_path_reports_old_and_window() {
        use crate::control::ControlCommand;
        let (tx, mut rx) = mpsc::channel::<ControlCommand>(4);
        // Responder: reply Ok((spent_before, window_start)) as the scheduler would.
        tokio::spawn(async move {
            if let Some(ControlCommand::ResetBudget { confirm_tx: Some(c), .. }) = rx.recv().await {
                let _ = c.send(Ok((500, 12_345)));
            }
        });
        let state = make_state_with_control(SchedulerSnapshot::default(), Some(tx));
        let resp = route(state, Method::POST, "/api/v1/budget/reset", "", br#"{"target":"global"}"#).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["spent_before"], 500);
        assert_eq!(v["reset_to"], 0);
        assert_eq!(v["window_start"], 12_345);
    }

    #[tokio::test]
    async fn budget_reset_unknown_agent_returns_404() {
        use crate::control::ControlCommand;
        let (tx, mut rx) = mpsc::channel::<ControlCommand>(4);
        tokio::spawn(async move {
            if let Some(ControlCommand::ResetBudget { confirm_tx: Some(c), .. }) = rx.recv().await {
                let _ = c.send(Err("agent 'ghost' not found".into()));
            }
        });
        let state = make_state_with_control(SchedulerSnapshot::default(), Some(tx));
        let resp = route(state, Method::POST, "/api/v1/budget/reset", "", br#"{"target":{"agent":"ghost"}}"#).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn runs_unconfigured_returns_503() {
        let state = make_state(SchedulerSnapshot::default()); // runs_store: None
        let resp = route(state, Method::GET, "/api/v1/runs", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn runs_returns_records_with_store() {
        // Populate a temp RunsStore, inject it into ApiState, assert GET /api/v1/runs 200 + shape.
        let dir = tempfile::tempdir().unwrap();
        let (store, _q) = crate::runs::RunsStore::open(&dir.path().join("runs.redb")).unwrap();
        store.apply(crate::runs::RunEvent::Open {
            agent_id: "inbox".into(), parent_id: Some("cos".into()), start_reason: "child_spawn".into(),
            start_context_tokens: Some(10), tier: "native".into(), ts: 100,
        }).unwrap();
        store.apply(crate::runs::RunEvent::Close {
            agent_id: "inbox".into(), status: "done".into(), stop_reason: Some("completed".into()),
            last_error: None, end_context_tokens: Some(50), ts: 200,
        }).unwrap();

        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap());
        let state = Arc::new(ApiState {
            snapshot: Arc::new(RwLock::new(SchedulerSnapshot::default())),
            memory_store: None,
            runs_store: Some(Arc::new(store)),
            broadcast_tx: tx,
            recorder,
            control_tx: None,
            credential_gateway: None,
            approval_secret: None,
        });
        let resp = route(state, Method::GET, "/api/v1/runs", "agent_id=inbox&limit=10", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["status"], "done");
        assert_eq!(arr[0]["spend"], 40);
        assert_eq!(arr[0]["parent_id"], "cos");
    }

    #[tokio::test]
    async fn brief_unconfigured_returns_503() {
        let state = make_state(SchedulerSnapshot::default()); // runs_store: None
        let resp = route(state, Method::GET, "/api/v1/brief", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn brief_returns_latest_with_live_approvals_overlay() {
        // ux.11c: GET /api/v1/brief returns the persisted brief + a LIVE approvals count
        // from the snapshot (Eng G1) — the count is NOT part of the stored record.
        let dir = tempfile::tempdir().unwrap();
        let (store, _q) = crate::runs::RunsStore::open(&dir.path().join("runs.redb")).unwrap();
        // One failed run in the window, then publish a brief over it.
        store.apply(crate::runs::RunEvent::Open {
            agent_id: "scout".into(), parent_id: Some("cos".into()), start_reason: "child_spawn".into(),
            start_context_tokens: Some(0), tier: "native".into(), ts: 20_000,
        }).unwrap();
        store.apply(crate::runs::RunEvent::Close {
            agent_id: "scout".into(), status: "failed".into(), stop_reason: Some("error".into()),
            last_error: Some("boom".into()), end_context_tokens: Some(14), ts: 20_050,
        }).unwrap();
        store.publish_brief(Some("one failure overnight".into()), 100_000).unwrap();

        // Snapshot carries two pending approvals → live overlay must report 2.
        let mut snap = SchedulerSnapshot::default();
        for i in 0..2 {
            snap.pending_actions.push(surfaces::PendingActionView {
                id: format!("act_{i}"), agent_id: "curator".into(), kind: "write_file".into(),
                risk: "low".into(), summary: "s".into(), args_json: "{}".into(), age_secs: 1,
            });
        }

        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap());
        let state = Arc::new(ApiState {
            snapshot: Arc::new(RwLock::new(snap)),
            memory_store: None,
            runs_store: Some(Arc::new(store)),
            broadcast_tx: tx,
            recorder,
            control_tx: None,
            credential_gateway: None,
            approval_secret: None,
        });
        let resp = route(state, Method::GET, "/api/v1/brief", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["approvals_pending"], 2, "live overlay from snapshot, not persisted");
        assert_eq!(v["brief"]["run_count"], 1);
        assert_eq!(v["brief"]["failed_count"], 1);
        assert_eq!(v["brief"]["narrative"], "one failure overnight");
        assert_eq!(v["brief"]["items"][0]["agent_id"], "scout");
    }

    #[tokio::test]
    async fn brief_stamps_server_now_on_both_the_single_and_list_arms() {
        // attn.1a (/review, testing specialist — CRITICAL). `server_now` is the entire
        // SERVER half of the staleness feature and had ZERO coverage on either arm.
        // Mutation-proven at review time: deleting `"server_now": server_now` from the
        // `?n=K` arm left all 53 management tests green AND clippy clean (the binding stayed
        // used by the other arm). No test in the workspace had ever requested `?n=` at all.
        //
        // Why that matters more than a normal coverage gap: if the field vanishes,
        // `agentctl` falls back to its deliberately-silent `None` path, the STALE banner
        // never fires again, and a dead pipeline renders as a healthy one — the exact
        // failure mode this increment exists to eliminate.
        let dir = tempfile::tempdir().unwrap();
        let (store, _q) = crate::runs::RunsStore::open(&dir.path().join("runs.redb")).unwrap();
        store.publish_brief(None, 100_000).unwrap();
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let store = Arc::new(store);
        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap());

        for query in ["", "n=3"] {
            let state = Arc::new(ApiState {
                snapshot: Arc::new(RwLock::new(SchedulerSnapshot::default())),
                memory_store: None,
                runs_store: Some(Arc::clone(&store)),
                broadcast_tx: tx.clone(),
                recorder: Arc::clone(&recorder),
                control_tx: None,
                credential_gateway: None,
                approval_secret: None,
            });
            let resp = route(state, Method::GET, "/api/v1/brief", query, &[]).await;
            assert_eq!(resp.status(), StatusCode::OK, "query={query:?}");
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            // Bound it, don't merely check presence: a `json!(0)` or a stringified value
            // would satisfy `.is_some()` and still break the age computation.
            let now = v["server_now"].as_u64().unwrap_or_else(|| {
                panic!(
                    "query={query:?}: no integer `server_now`. agentctl computes brief age \
                     as server_now - created_at; without it the STALE banner silently never \
                     fires and a stopped pipeline reads as a healthy one."
                )
            });
            assert!(
                now >= before,
                "query={query:?}: server_now {now} predates the request ({before})"
            );
        }
    }

    #[tokio::test]
    async fn brief_null_when_none_published_yet() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _q) = crate::runs::RunsStore::open(&dir.path().join("runs.redb")).unwrap();
        let (tx, _) = broadcast::channel(16);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let recorder = Arc::new(crate::flight_recorder::FlightRecorder::new(tmp.path()).unwrap());
        let state = Arc::new(ApiState {
            snapshot: Arc::new(RwLock::new(SchedulerSnapshot::default())),
            memory_store: None,
            runs_store: Some(Arc::new(store)),
            broadcast_tx: tx,
            recorder,
            control_tx: None,
            credential_gateway: None,
            approval_secret: None,
        });
        let resp = route(state, Method::GET, "/api/v1/brief", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["brief"].is_null(), "no brief yet → brief:null (200, not 404)");
        assert_eq!(v["approvals_pending"], 0);
    }

    #[tokio::test]
    async fn budget_set_empty_body_returns_400() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/budget/set", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn budget_set_global_returns_400() {
        // F1: the global ceiling is not runtime-settable — reject before dispatch.
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/budget/set", "", br#"{"target":"global","limit":5000}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn budget_set_missing_limit_returns_400() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/budget/set", "", br#"{"target":{"agent":"cos"}}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn budget_set_without_control_tx_returns_503() {
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/budget/set", "", br#"{"target":{"agent":"cos"},"limit":5000}"#).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn budget_set_happy_path_reports_old_and_new() {
        use crate::control::ControlCommand;
        let (tx, mut rx) = mpsc::channel::<ControlCommand>(4);
        tokio::spawn(async move {
            if let Some(ControlCommand::SetBudget { confirm_tx: Some(c), .. }) = rx.recv().await {
                let _ = c.send(Ok((100_000, 500_000)));
            }
        });
        let state = make_state_with_control(SchedulerSnapshot::default(), Some(tx));
        let resp = route(state, Method::POST, "/api/v1/budget/set", "", br#"{"target":{"agent":"cos"},"limit":500000}"#).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["old_limit"], 100_000);
        assert_eq!(v["limit"], 500_000);
    }

    #[tokio::test]
    async fn budget_set_unknown_agent_returns_404() {
        use crate::control::ControlCommand;
        let (tx, mut rx) = mpsc::channel::<ControlCommand>(4);
        tokio::spawn(async move {
            if let Some(ControlCommand::SetBudget { confirm_tx: Some(c), .. }) = rx.recv().await {
                let _ = c.send(Err("agent 'ghost' not found".into()));
            }
        });
        let state = make_state_with_control(SchedulerSnapshot::default(), Some(tx));
        let resp = route(state, Method::POST, "/api/v1/budget/set", "", br#"{"target":{"agent":"ghost"},"limit":5000}"#).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
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

    // ── GET /api/v1/credentials (cred.5) ──────────────────────────────────────

    #[tokio::test]
    async fn credentials_gateway_disabled_returns_enabled_false() {
        let snap = SchedulerSnapshot::default(); // credential_snapshot = None
        let state = make_state(snap);
        let resp = route(state, Method::GET, "/api/v1/credentials", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["enabled"], false, "no credential_snapshot => enabled:false");
    }

    #[tokio::test]
    async fn credentials_gateway_enabled_returns_snapshot() {
        use surfaces::{CredentialSnapshot, ProviderHealth};
        let snap = SchedulerSnapshot {
            credential_snapshot: Some(CredentialSnapshot {
                gateway_enabled: true,
                configured_providers: vec!["google".to_string()],
                provider_health: vec![ProviderHealth {
                    name: "google".to_string(),
                    token_fresh: true,
                    last_refresh_at: Some(1_700_000_000),
                    expires_at: Some(1_700_003_600),
                    last_error: None,
                    attention_reason: None,
                    attention_since: None,
                    recovery_kind: None,
                }],
            }),
            ..Default::default()
        };
        let state = make_state(snap);
        let resp = route(state, Method::GET, "/api/v1/credentials", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["gateway_enabled"], true);
        assert_eq!(v["configured_providers"][0], "google");
        assert_eq!(v["provider_health"][0]["token_fresh"], true);
        assert_eq!(v["provider_health"][0]["last_error"], serde_json::Value::Null);
    }

    // ── cred.7: reset-attention route ─────────────────────────────────────────

    #[tokio::test]
    async fn reset_attention_returns_503_when_no_gateway() {
        // No credential_gateway in state → must return 503.
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/credentials/google/reset-attention", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE,
            "POST /api/v1/credentials/<p>/reset-attention must return 503 when gateway is not configured");
    }

    #[tokio::test]
    async fn reset_attention_empty_provider_returns_400() {
        // Path /api/v1/credentials//reset-attention has empty provider segment → must return 400.
        let state = make_state(SchedulerSnapshot::default());
        let resp = route(state, Method::POST, "/api/v1/credentials//reset-attention", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST,
            "POST /api/v1/credentials//reset-attention must return 400 for empty provider");
    }

    #[tokio::test]
    async fn credentials_provider_health_attention_fields_serialized() {
        // A ProviderHealth with attention fields set must include them in JSON.
        use surfaces::{CredentialSnapshot, ProviderHealth};
        let snap = SchedulerSnapshot {
            credential_snapshot: Some(CredentialSnapshot {
                gateway_enabled: true,
                configured_providers: vec!["google".to_string()],
                provider_health: vec![ProviderHealth {
                    name: "google".to_string(),
                    token_fresh: false,
                    last_refresh_at: None,
                    expires_at: None,
                    last_error: Some("invalid_grant".to_string()),
                    attention_reason: Some("Token was revoked".to_string()),
                    attention_since: Some(1_720_000_000),
                    recovery_kind: Some("reauth".to_string()),
                }],
            }),
            ..Default::default()
        };
        let state = make_state(snap);
        let resp = route(state, Method::GET, "/api/v1/credentials", "", &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["provider_health"][0]["attention_reason"], "Token was revoked",
            "attention_reason must be serialized in JSON");
        assert_eq!(v["provider_health"][0]["recovery_kind"], "reauth",
            "recovery_kind must be serialized in JSON");
    }
}
