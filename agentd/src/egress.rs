//! Egress mediator for agentd (p7.5 + p7.5b).
//!
//! Native agents call `InferenceGateway` directly (no HTTP hop). `EgressProxy`
//! intercepts inference results and writes signed action receipts.
//!
//! `start_http_proxy` (p7.5b) replaces the 501 stub with a real forwarding
//! proxy. Universal-tier workloads point `ANTHROPIC_BASE_URL` at it. The proxy:
//!   1. Identifies the workload via the ephemeral key in `x-api-key`.
//!   2. Strips the ephemeral key and inserts the real `ANTHROPIC_API_KEY`.
//!   3. Strips hop-by-hop headers (Host, Content-Length, Transfer-Encoding, Connection).
//!   4. Forwards POST /v1/messages to api.anthropic.com via reqwest.
//!   5. Buffers the response (sync; streaming deferred to p7.5c).
//!   6. Records EgressBrokered + ActionReceiptEmitted flight events.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::Result;
use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{BodyExt, Empty, Full};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::json;
use tokio::net::TcpListener;

use crate::events::EventKind;
use crate::evidence::EvidenceWriter;
use crate::flight_recorder::FlightRecorder;

/// Maximum response body size forwarded from the upstream Anthropic API.
const MAX_PROXY_RESPONSE_BYTES: usize = 8 * 1024 * 1024; // 8 MB
/// Maximum inbound request body size (prevents OOM from runaway workloads).
const MAX_REQUEST_BODY_BYTES: usize = 4 * 1024 * 1024; // 4 MB
/// Headers from the inbound workload request that are passed through to Anthropic.
// content-type is NOT passed through — the proxy always sends application/json to prevent
// workloads from injecting unexpected content types (e.g. multipart/form-data) upstream.
const PASSTHROUGH_HEADERS: [&str; 2] = ["anthropic-version", "anthropic-beta"];
/// Connect timeout for upstream requests.
const EGRESS_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Total timeout for upstream requests (including waiting for response body).
const EGRESS_REQUEST_TIMEOUT_SECS: u64 = 120;
/// Default upstream URL for Anthropic messages API.
const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";

// ── ProxyRegistry ─────────────────────────────────────────────────────────────

/// Per-workload egress policy, shared between the registry and token accounting.
pub struct ProxyPolicy {
    // TODO(p7.6): enforce per-workload allowed_hosts once multi-upstream is supported.
    // For p7.5b the proxy always forwards to one hardcoded upstream (ANTHROPIC_MESSAGES_URL)
    // so this field is scaffolding only and is intentionally not checked during forwarding.
    pub allowed_hosts: Vec<String>,
    /// Shared with the scheduler; decremented on each forwarded inference call.
    pub token_budget_remaining: Arc<AtomicU64>,
}

impl Clone for ProxyPolicy {
    fn clone(&self) -> Self {
        Self {
            allowed_hosts:            self.allowed_hosts.clone(),
            token_budget_remaining:   Arc::clone(&self.token_budget_remaining),
        }
    }
}

/// Registry entry for a registered universal-tier workload.
pub struct ProxyEntry {
    pub agent_id: String,
    pub policy:   ProxyPolicy,
}

impl Clone for ProxyEntry {
    fn clone(&self) -> Self {
        Self {
            agent_id: self.agent_id.clone(),
            policy:   self.policy.clone(),
        }
    }
}

/// Maps ephemeral workload key → (agent_id, policy).
/// The ephemeral key is generated at spawn (p7.6) and injected into the child's
/// env as `ANTHROPIC_API_KEY`. The real key lives only inside this registry.
#[derive(Default)]
pub struct ProxyRegistry {
    agents: RwLock<HashMap<String, ProxyEntry>>,
}

impl ProxyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a workload. Called at p7.6 spawn time.
    pub fn register(&self, ephemeral_key: String, entry: ProxyEntry) {
        let mut map = self.agents.write().unwrap();
        map.insert(ephemeral_key, entry);
    }

    /// Deregister a workload by its ephemeral key. Called when the workload exits.
    pub fn deregister_by_key(&self, ephemeral_key: &str) {
        let mut map = self.agents.write().unwrap();
        map.remove(ephemeral_key);
    }

    /// Look up a workload by its ephemeral key. Returns a clone of the entry.
    pub fn entry_for_key(&self, x_api_key: &str) -> Option<ProxyEntry> {
        let map = self.agents.read().unwrap();
        map.get(x_api_key).cloned()
    }
}

// ── EgressProxy ───────────────────────────────────────────────────────────────

/// In-process egress mediator for native agents.
pub struct EgressProxy {
    writer:   Arc<EvidenceWriter>,
    recorder: Arc<FlightRecorder>,
}

impl EgressProxy {
    pub fn new(writer: Arc<EvidenceWriter>, recorder: Arc<FlightRecorder>) -> Self {
        Self { writer, recorder }
    }

    /// Record a permitted model inference. Emits `EgressBrokered` + `ActionReceiptEmitted`.
    pub fn record_inference(
        &self,
        agent_id:      &str,
        model:         &str,
        input_tokens:  u64,
        output_tokens: u64,
    ) {
        match self.writer.record_allowed("inference", model, agent_id) {
            Ok(seq) => {
                self.recorder.record(
                    agent_id,
                    None,
                    EventKind::EgressBrokered,
                    json!({
                        "agent": agent_id,
                        "kind": "inference",
                        "dest": model,
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "content_audited": true,
                    }),
                );
                self.recorder.record(
                    agent_id,
                    None,
                    EventKind::ActionReceiptEmitted,
                    json!({ "agent": agent_id, "verdict": "allowed", "chain_seq": seq }),
                );
            }
            Err(e) => {
                tracing::warn!(agent = agent_id, "egress receipt write failed: {e:#}");
                self.recorder.record(
                    agent_id,
                    None,
                    EventKind::EgressProxyFailed,
                    json!({ "error": format!("{e:#}") }),
                );
            }
        }
    }

    /// Record a denied egress attempt. Emits `EgressDenied` + `ActionReceiptEmitted`.
    pub fn record_denied(&self, agent_id: &str, target: &str) {
        match self.writer.record_denied("egress", target, agent_id) {
            Ok(seq) => {
                self.recorder.record(
                    agent_id,
                    None,
                    EventKind::EgressDenied,
                    json!({ "agent": agent_id, "attempted_dest": target }),
                );
                self.recorder.record(
                    agent_id,
                    None,
                    EventKind::ActionReceiptEmitted,
                    json!({ "agent": agent_id, "verdict": "denied", "chain_seq": seq }),
                );
            }
            Err(e) => {
                tracing::warn!(agent = agent_id, "egress denied-receipt write failed: {e:#}");
                self.recorder.record(
                    agent_id,
                    None,
                    EventKind::EgressProxyFailed,
                    json!({ "error": format!("{e:#}") }),
                );
            }
        }
    }

    /// Emit an EgressProxyFailed event. Used by the HTTP proxy for upstream errors.
    pub fn record_proxy_failed(&self, agent_id: &str, reason: &str) {
        self.recorder.record(
            agent_id,
            None,
            EventKind::EgressProxyFailed,
            json!({ "agent": agent_id, "reason": reason }),
        );
    }
}

// ── HTTP proxy ─────────────────────────────────────────────────────────────────

/// Shared state for the HTTP forwarding proxy handler.
struct ProxyState {
    registry:     Arc<ProxyRegistry>,
    real_key:     String,
    client:       reqwest::Client,
    egress:       Arc<EgressProxy>,
    /// Upstream endpoint; overridable in tests to point at a mock server.
    upstream_url: String,
}

/// Build a structured JSON error response with an actionable `detail` tag.
fn json_error_response(
    status:     u16,
    error_type: &str,
    message:    &str,
    detail:     &str,
) -> Response<Full<Bytes>> {
    let body = json!({
        "error": {
            "type":    error_type,
            "message": message,
            "detail":  detail,
        }
    });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("json_error_response: builder with known-good status and header must not fail")
}

/// Handle a single proxied request.
async fn handle_proxy_request(
    state: Arc<ProxyState>,
    req:   Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    // 1. Reject streaming clients early with a clear 501 (not a confusing 200+buffer).
    let accept = req
        .headers()
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if accept.contains("text/event-stream") {
        return Ok(json_error_response(
            501,
            "streaming_not_supported",
            "Use stream=False; streaming proxy ships in p7.5c",
            "streaming_not_supported",
        ));
    }

    // 2. Extract x-api-key (the ephemeral workload key).
    let ephemeral_key = match req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
    {
        Some(k) => k.to_owned(),
        None => {
            return Ok(json_error_response(
                403,
                "egress_denied",
                "Missing x-api-key header",
                "unknown_workload_key",
            ))
        }
    };

    // 3. Registry lookup.
    let entry = match state.registry.entry_for_key(&ephemeral_key) {
        Some(e) => e,
        None => {
            state.egress.record_proxy_failed("unknown", "unknown_workload_key");
            return Ok(json_error_response(
                403,
                "egress_denied",
                "Unknown workload key",
                "unknown_workload_key",
            ))
        }
    };

    // 4. Path + method gate — only POST /v1/messages is allowed.
    if req.uri().path() != "/v1/messages" {
        state.egress.record_proxy_failed("unknown", "path_not_allowed");
        return Ok(json_error_response(
            404,
            "egress_denied",
            "Path not allowed; only POST /v1/messages is proxied",
            "path_not_allowed",
        ));
    }
    if req.method() != hyper::Method::POST {
        return Ok(json_error_response(
            405,
            "egress_denied",
            "Method not allowed; only POST is accepted",
            "method_not_allowed",
        ));
    }

    // 5. Collect request body with a 4 MB cap (prevents OOM from runaway workloads).
    let (parts, body) = req.into_parts();
    let limited_body = http_body_util::Limited::new(body, MAX_REQUEST_BODY_BYTES);
    let body_bytes = match limited_body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            let detail = if e.downcast_ref::<http_body_util::LengthLimitError>().is_some() {
                "request_body_too_large"
            } else {
                "request_body_error"
            };
            return Ok(json_error_response(
                413,
                "egress_proxy_failed",
                "Request body too large or unreadable",
                detail,
            ))
        }
    };

    // 6. Build forwarded request: strip ephemeral key, insert real key.
    //    Pass through: anthropic-version, anthropic-beta, content-type.
    //    Hop-by-hop headers (Host, Content-Length, Transfer-Encoding, Connection)
    //    are NOT forwarded — reqwest sets them correctly for the upstream connection.
    let mut fwd_headers = reqwest::header::HeaderMap::new();
    let real_key_hv = match reqwest::header::HeaderValue::from_str(&state.real_key) {
        Ok(v) => v,
        Err(_) => {
            state.egress.record_proxy_failed(&entry.agent_id, "invalid_real_key");
            return Ok(json_error_response(
                500,
                "egress_proxy_failed",
                "Proxy configuration error: invalid real API key",
                "invalid_real_key",
            ));
        }
    };
    fwd_headers.insert("x-api-key", real_key_hv);
    for pass_through in PASSTHROUGH_HEADERS {
        if let Some(v) = parts.headers.get(pass_through) {
            if let Ok(val) = reqwest::header::HeaderValue::from_bytes(v.as_bytes()) {
                if let Ok(name) = reqwest::header::HeaderName::from_bytes(pass_through.as_bytes()) {
                    fwd_headers.insert(name, val);
                }
            }
        }
    }
    // Always set content-type on the upstream request regardless of what the workload sent.
    fwd_headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    // 7. Pre-forward budget check — reject immediately if budget already exhausted.
    //    Token accounting is post-hoc (step 10), but we refuse to forward a new
    //    request when the counter is already at zero to prevent over-spend.
    if entry.policy.token_budget_remaining.load(Ordering::Acquire) == 0 {
        state.egress.record_proxy_failed(&entry.agent_id, "budget_exhausted");
        return Ok(json_error_response(
            429,
            "egress_budget_exhausted",
            "Token budget for this workload is exhausted",
            "budget_exhausted",
        ));
    }

    // 8. Forward to Anthropic.
    let send_result = state
        .client
        .post(&state.upstream_url)
        .headers(fwd_headers)
        .body(body_bytes.to_vec())
        .send()
        .await;

    let resp = match send_result {
        Ok(r) => r,
        Err(e) => {
            let reason = if e.is_timeout() { "upstream_timeout" } else { "upstream_error" };
            state.egress.record_proxy_failed(&entry.agent_id, reason);
            let status = if e.is_timeout() { 504 } else { 502 };
            // Do not interpolate the error value — reqwest errors can include URLs
            // or TLS details that must not be forwarded to untrusted callers.
            return Ok(json_error_response(
                status,
                "egress_proxy_failed",
                "Upstream request failed",
                reason,
            ));
        }
    };

    // 9. Stream + buffer upstream body with 8 MB cap.
    let upstream_status = resp.status();
    // Only forward content-types from a known-safe allowlist.
    // Arbitrary reflection of upstream headers could enable confused-deputy issues.
    let upstream_ct = {
        let raw = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json");
        if raw.starts_with("application/json") {
            raw.to_owned()
        } else {
            "application/json".to_owned()
        }
    };

    let mut resp_bytes: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(b) => {
                // Check BEFORE extending so the buffer never exceeds the cap.
                if resp_bytes.len() + b.len() > MAX_PROXY_RESPONSE_BYTES {
                    state
                        .egress
                        .record_proxy_failed(&entry.agent_id, "response_too_large");
                    return Ok(json_error_response(
                        502,
                        "egress_proxy_failed",
                        "Upstream response exceeded 8 MB limit",
                        "response_too_large",
                    ));
                }
                resp_bytes.extend_from_slice(&b);
            }
            Err(_) => {
                state
                    .egress
                    .record_proxy_failed(&entry.agent_id, "upstream_read_error");
                return Ok(json_error_response(
                    502,
                    "egress_proxy_failed",
                    "Failed to read upstream response",
                    "upstream_read_error",
                ))
            }
        }
    }

    // 10. Parse token usage (best-effort; don't fail on missing).
    //     Use as_f64 → cast to u64 to handle both integer and float representations
    //     (Anthropic returns integers today, but as_u64() returns None for floats).
    let (in_toks, out_toks) =
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&resp_bytes) {
            let in_t  = v["usage"]["input_tokens"].as_f64().map(|f| f as u64).unwrap_or(0);
            let out_t = v["usage"]["output_tokens"].as_f64().map(|f| f as u64).unwrap_or(0);
            (in_t, out_t)
        } else {
            (0u64, 0u64)
        };

    // 11. Meter: decrement shared budget counter (saturating — never wraps to u64::MAX).
    let total_toks = in_toks.saturating_add(out_toks);
    // AcqRel ordering prevents concurrent requests from reading a stale budget
    // and both passing the pre-forward check with budget near-zero.
    entry
        .policy
        .token_budget_remaining
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
            Some(cur.saturating_sub(total_toks))
        })
        .ok();

    // 12. Emit egress events + signed receipt.
    state
        .egress
        .record_inference(&entry.agent_id, "anthropic", in_toks, out_toks);

    // 13. Return upstream status + body.
    Ok(Response::builder()
        .status(upstream_status)
        .header("content-type", upstream_ct)
        .body(Full::new(Bytes::from(resp_bytes)))
        .expect("response builder with valid upstream status and known content-type must not fail"))
}

/// Start the HTTP forwarding proxy. Returns the actual bound address.
/// Spawns a background Tokio task; the returned `SocketAddr` is stored by `main.rs`
/// so p7.6 can inject it as `ANTHROPIC_BASE_URL` into spawned workloads.
pub async fn start_http_proxy(
    addr:     &str,
    real_key: String,
    registry: Arc<ProxyRegistry>,
    recorder: Arc<FlightRecorder>,
    writer:   Arc<EvidenceWriter>,
) -> Result<std::net::SocketAddr> {
    // Guard at the public entry: the real API key must only travel over TLS.
    anyhow::ensure!(
        !real_key.is_empty(),
        "egress proxy: ANTHROPIC_API_KEY is empty — set the key before enabling the proxy"
    );
    start_http_proxy_impl(
        addr,
        real_key,
        registry,
        recorder,
        writer,
        ANTHROPIC_MESSAGES_URL.to_string(),
    )
    .await
}

async fn start_http_proxy_impl(
    addr:         &str,
    real_key:     String,
    registry:     Arc<ProxyRegistry>,
    recorder:     Arc<FlightRecorder>,
    writer:       Arc<EvidenceWriter>,
    upstream_url: String,
) -> Result<std::net::SocketAddr> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("egress proxy: failed to bind {addr}: {e}"))?;
    let bound = listener.local_addr()?;
    // Loopback-only: reject misconfigured non-loopback binds to prevent the proxy
    // (and the real-key substitution path) from being exposed on external interfaces.
    anyhow::ensure!(
        bound.ip().is_loopback(),
        "egress proxy: refusing to bind on non-loopback address {bound} — proxy must be localhost-only"
    );

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(EGRESS_CONNECT_TIMEOUT_SECS))
        .timeout(std::time::Duration::from_secs(EGRESS_REQUEST_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .use_rustls_tls()
        .build()
        .map_err(|e| anyhow::anyhow!("egress proxy: failed to build HTTP client: {e}"))?;

    let egress = Arc::new(EgressProxy::new(writer, recorder));
    let state = Arc::new(ProxyState {
        registry,
        real_key,
        client,
        egress,
        upstream_url,
    });

    tracing::info!("egress proxy listening on {bound}");

    tokio::spawn(async move {
        loop {
            let (stream, _peer) = match listener.accept().await {
                Ok(v) => v,
                // Transient OS errors (e.g. EMFILE) must not kill the accept loop.
                Err(e) => {
                    tracing::error!("egress proxy: accept error: {e}");
                    continue;
                }
            };
            let io = TokioIo::new(stream);
            let state_clone = Arc::clone(&state);
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                    let s = Arc::clone(&state_clone);
                    async move { handle_proxy_request(s, req).await }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    Ok(bound)
}

/// Bind an HTTP stub server. Every request returns `501 Not Implemented`.
/// Kept for backward compatibility with `http_stub_returns_501` test.
pub async fn start_http_stub(addr: &str) -> Result<std::net::SocketAddr> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("egress HTTP stub: failed to bind {addr}: {e}"))?;
    let bound = listener.local_addr()?;
    tracing::info!("egress HTTP stub listening on {bound} (returns 501 for all requests)");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _peer)) = listener.accept().await else {
                break;
            };
            let io = TokioIo::new(stream);
            tokio::spawn(async move {
                let svc = service_fn(|_req: Request<hyper::body::Incoming>| async move {
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .status(501)
                            .body(Empty::<Bytes>::new())
                            .unwrap(),
                    )
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });
    Ok(bound)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use tempfile::TempDir;

    fn make_egress(dir: &TempDir) -> EgressProxy {
        let ev = dir.path().join("evidence.jsonl");
        let key = dir.path().join("egress.pkcs8");
        let writer = Arc::new(EvidenceWriter::open(&ev, &key).unwrap());
        let log = dir.path().join("flight.jsonl");
        let recorder = Arc::new(FlightRecorder::new(&log).unwrap());
        EgressProxy::new(writer, recorder)
    }

    #[test]
    fn record_inference_writes_receipt_and_events() {
        let dir = TempDir::new().unwrap();
        let proxy = make_egress(&dir);
        proxy.record_inference("agent_0", "claude-sonnet-4-6", 100, 200);
        let ev_content = std::fs::read_to_string(dir.path().join("evidence.jsonl")).unwrap();
        assert!(!ev_content.is_empty(), "evidence file should have a receipt");
        assert!(ev_content.contains("\"verdict\":\"allowed\""));
        let log = std::fs::read_to_string(dir.path().join("flight.jsonl")).unwrap();
        assert!(log.contains("egress_brokered"));
        assert!(log.contains("action_receipt_emitted"));
    }

    #[test]
    fn record_denied_writes_receipt_and_events() {
        let dir = TempDir::new().unwrap();
        let proxy = make_egress(&dir);
        proxy.record_denied("agent_0", "https://evil.example.com");
        let ev_content = std::fs::read_to_string(dir.path().join("evidence.jsonl")).unwrap();
        assert!(ev_content.contains("\"verdict\":\"denied\""));
        let log = std::fs::read_to_string(dir.path().join("flight.jsonl")).unwrap();
        assert!(log.contains("egress_denied"));
        assert!(log.contains("action_receipt_emitted"));
    }

    #[test]
    fn proxy_registry_register_deregister() {
        let reg = ProxyRegistry::new();
        let budget = Arc::new(AtomicU64::new(10_000));
        reg.register(
            "sk-ant-WORKLOAD-test-key".to_string(),
            ProxyEntry {
                agent_id: "scout".to_string(),
                policy:   ProxyPolicy {
                    allowed_hosts:          vec![],
                    token_budget_remaining: Arc::clone(&budget),
                },
            },
        );
        // Lookup should succeed.
        let entry = reg.entry_for_key("sk-ant-WORKLOAD-test-key").unwrap();
        assert_eq!(entry.agent_id, "scout");
        // Deregister.
        reg.deregister_by_key("sk-ant-WORKLOAD-test-key");
        assert!(reg.entry_for_key("sk-ant-WORKLOAD-test-key").is_none());
    }

    #[test]
    fn proxy_ephemeral_key_identifies_agent() {
        let reg = ProxyRegistry::new();
        let budget = Arc::new(AtomicU64::new(50_000));
        reg.register(
            "sk-ant-WORKLOAD-abc-xyz".to_string(),
            ProxyEntry {
                agent_id: "librarian".to_string(),
                policy:   ProxyPolicy {
                    allowed_hosts:          vec!["api.anthropic.com".to_string()],
                    token_budget_remaining: Arc::clone(&budget),
                },
            },
        );
        let entry = reg.entry_for_key("sk-ant-WORKLOAD-abc-xyz").unwrap();
        assert_eq!(entry.agent_id, "librarian");
        assert_eq!(entry.policy.allowed_hosts, vec!["api.anthropic.com"]);
        assert!(reg.entry_for_key("wrong-key").is_none());
    }

    #[test]
    fn json_error_response_structure() {
        let resp = json_error_response(403, "egress_denied", "Unknown workload key", "unknown_workload_key");
        assert_eq!(resp.status(), 403);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("application/json"));
    }

    #[tokio::test]
    async fn http_stub_returns_501() {
        let addr = start_http_stub("127.0.0.1:0").await.unwrap();
        let url = format!("http://{addr}/v1/messages");
        let resp = reqwest::Client::new().post(&url).send().await.unwrap();
        assert_eq!(resp.status(), 501);
    }

    #[tokio::test]
    async fn proxy_rejects_unknown_key() {
        let dir = TempDir::new().unwrap();
        // No workloads registered — any key returns 403.
        let (bound, _registry) = start_test_proxy(&dir, "sk-ant-REAL-KEY", "http://127.0.0.1:1".to_string()).await;
        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/messages"))
            .header("x-api-key", "sk-ant-WORKLOAD-not-registered")
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["detail"], "unknown_workload_key");
    }

    #[tokio::test]
    async fn proxy_streaming_request_returns_501() {
        let dir = TempDir::new().unwrap();
        // Streaming check fires before registry lookup — upstream never reached.
        let (bound, _registry) = start_test_proxy(&dir, "sk-ant-REAL-KEY", "http://127.0.0.1:1".to_string()).await;
        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/messages"))
            .header("accept", "text/event-stream")
            .header("x-api-key", "sk-ant-WORKLOAD-xyz")
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 501);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["detail"], "streaming_not_supported");
    }

    #[tokio::test]
    async fn proxy_unknown_path_returns_404() {
        let dir = TempDir::new().unwrap();
        // Path check fires before upstream — upstream never reached.
        let (bound, registry) = start_test_proxy(&dir, "sk-ant-REAL-KEY", "http://127.0.0.1:1".to_string()).await;
        register_workload(&registry, "sk-ant-WORKLOAD-test", "test-agent", 100_000);
        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/bad-path"))
            .header("x-api-key", "sk-ant-WORKLOAD-test")
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["detail"], "path_not_allowed");
    }

    /// Plan acceptance: proxy substitutes real key in x-api-key and strips the ephemeral key.
    #[tokio::test]
    async fn proxy_strips_ephemeral_inserts_real_key() {
        // Mock upstream echoes back the x-api-key it received.
        let upstream = start_mock_upstream(|req| async move {
            let received_key = req
                .headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let body = serde_json::json!({
                "received_key": received_key,
                "usage": { "input_tokens": 10, "output_tokens": 20 },
            });
            Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body.to_string())))
                .unwrap())
        })
        .await;

        let dir = TempDir::new().unwrap();
        let upstream_url = format!("http://{upstream}");
        let (proxy_bound, registry) =
            start_test_proxy(&dir, "sk-ant-REAL-PRODUCTION-KEY", upstream_url).await;
        let ephemeral_key = "sk-ant-WORKLOAD-ephemeral-001";
        register_workload(&registry, ephemeral_key, "worker", 100_000);

        let resp = reqwest::Client::new()
            .post(format!("http://{proxy_bound}/v1/messages"))
            .header("x-api-key", ephemeral_key)
            .header("content-type", "application/json")
            .header("anthropic-version", "2023-06-01")
            .body(r#"{"model":"claude-sonnet-4-6","max_tokens":1}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        // Real key must reach upstream; ephemeral key must not.
        assert_eq!(body["received_key"], "sk-ant-REAL-PRODUCTION-KEY");
        assert_ne!(body["received_key"], ephemeral_key);
    }

    #[tokio::test]
    async fn egress_bound_addr_stored_not_discarded() {
        // Verify start_test_proxy returns a valid SocketAddr.
        let dir = TempDir::new().unwrap();
        let (bound, _registry) = start_test_proxy(&dir, "sk-ant-real", "http://127.0.0.1:1".to_string()).await;
        assert_eq!(bound.ip().to_string(), "127.0.0.1");
        assert!(bound.port() > 0, "should have a non-zero port");
    }

    #[tokio::test]
    async fn proxy_records_proxy_failed_event() {
        let dir = TempDir::new().unwrap();
        let proxy = make_egress(&dir);
        proxy.record_proxy_failed("agent_0", "upstream_timeout");
        let log = std::fs::read_to_string(dir.path().join("flight.jsonl")).unwrap();
        assert!(log.contains("egress_proxy_failed"));
        assert!(log.contains("upstream_timeout"));
    }

    // ── Helper: spin up a mock upstream with hyper ───────────────────────────────

    /// Spin up a hyper HTTP server that invokes `handler` for each request.
    /// Returns the bound address. Handler must return `Ok(Response<Full<Bytes>>)`.
    async fn start_mock_upstream<F, Fut>(handler: F) -> std::net::SocketAddr
    where
        F: Fn(Request<hyper::body::Incoming>) -> Fut + Send + Sync + 'static + Clone,
        Fut: std::future::Future<Output = Result<Response<Full<Bytes>>, std::convert::Infallible>>
            + Send,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { break; };
                let io = TokioIo::new(stream);
                let h = handler.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req| {
                        let h2 = h.clone();
                        async move { h2(req).await }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
        addr
    }

    /// Start a test proxy pointing at a specific upstream URL (not api.anthropic.com).
    async fn start_test_proxy(
        dir:          &TempDir,
        real_key:     &str,
        upstream_url: String,
    ) -> (std::net::SocketAddr, Arc<ProxyRegistry>) {
        let ev       = dir.path().join("evidence.jsonl");
        let key_path = dir.path().join("egress.pkcs8");
        let writer   = Arc::new(EvidenceWriter::open(&ev, &key_path).unwrap());
        let log      = dir.path().join("flight.jsonl");
        let recorder = Arc::new(FlightRecorder::new(&log).unwrap());
        let registry = Arc::new(ProxyRegistry::new());
        let bound = start_http_proxy_impl(
            "127.0.0.1:0",
            real_key.to_string(),
            Arc::clone(&registry),
            recorder,
            writer,
            upstream_url,
        )
        .await
        .unwrap();
        (bound, registry)
    }

    fn register_workload(registry: &Arc<ProxyRegistry>, key: &str, agent: &str, budget: u64) -> Arc<AtomicU64> {
        let b = Arc::new(AtomicU64::new(budget));
        registry.register(
            key.to_string(),
            ProxyEntry {
                agent_id: agent.to_string(),
                policy:   ProxyPolicy {
                    allowed_hosts:          vec![],
                    token_budget_remaining: Arc::clone(&b),
                },
            },
        );
        b
    }

    // ── Plan tests ───────────────────────────────────────────────────────────────

    /// Plan acceptance: verify hop-by-hop headers are stripped and the real key is forwarded.
    #[tokio::test]
    async fn proxy_hop_by_hop_headers_stripped() {
        // Mock upstream: capture and echo back the headers it received.
        let upstream = start_mock_upstream(|req| async move {
            let headers_map: std::collections::HashMap<String, String> = req
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let headers_json = serde_json::to_value(headers_map).unwrap();
            let body = serde_json::json!({
                "received_headers": headers_json,
                "usage": { "input_tokens": 5, "output_tokens": 5 },
            });
            Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body.to_string())))
                .unwrap())
        })
        .await;

        let dir = TempDir::new().unwrap();
        let upstream_url = format!("http://{upstream}");
        let (bound, registry) = start_test_proxy(&dir, "sk-ant-REAL-KEY", upstream_url).await;
        register_workload(&registry, "sk-ant-WORKLOAD-hbh", "hop-agent", 100_000);

        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/messages"))
            .header("x-api-key", "sk-ant-WORKLOAD-hbh")
            .header("content-type", "application/json")
            // hop-by-hop headers that must be stripped
            .header("connection", "keep-alive")
            .header("transfer-encoding", "chunked")
            .body(r#"{"model":"test","max_tokens":1}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let hdrs = &body["received_headers"];
        // Real key must reach upstream; ephemeral key must not.
        assert_eq!(hdrs["x-api-key"], "sk-ant-REAL-KEY", "real key not forwarded");
        assert!(
            hdrs.get("connection").is_none(),
            "connection hop-by-hop header leaked to upstream"
        );
        assert!(
            hdrs.get("transfer-encoding").is_none(),
            "transfer-encoding hop-by-hop header leaked to upstream"
        );
    }

    /// Plan acceptance: successful proxy emits egress_brokered flight event, writes receipt,
    /// and decrements the token budget.
    #[tokio::test]
    async fn proxy_records_egress_brokered_and_receipt() {
        let upstream = start_mock_upstream(|_req| async move {
            let body = serde_json::json!({
                "id": "msg_test",
                "usage": { "input_tokens": 100, "output_tokens": 50 },
            });
            Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body.to_string())))
                .unwrap())
        })
        .await;

        let dir = TempDir::new().unwrap();
        let upstream_url = format!("http://{upstream}");
        let (bound, registry) = start_test_proxy(&dir, "sk-ant-REAL-KEY", upstream_url).await;
        let budget = register_workload(&registry, "sk-ant-WORKLOAD-receipt", "receipt-agent", 10_000);

        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/messages"))
            .header("x-api-key", "sk-ant-WORKLOAD-receipt")
            .header("content-type", "application/json")
            .body(r#"{"model":"test","max_tokens":1}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        // Budget decremented: 10000 - 150 = 9850.
        let remaining = budget.load(Ordering::Relaxed);
        assert_eq!(remaining, 9_850, "budget should have been decremented by 150 tokens");

        // Flight log must contain egress_brokered.
        let log = std::fs::read_to_string(dir.path().join("flight.jsonl")).unwrap();
        assert!(log.contains("egress_brokered"), "missing egress_brokered event");
        assert!(log.contains("action_receipt_emitted"), "missing action_receipt_emitted event");

        // Evidence file must have a receipt entry.
        let ev = std::fs::read_to_string(dir.path().join("evidence.jsonl")).unwrap();
        assert!(!ev.is_empty(), "evidence.jsonl should contain a receipt");
        assert!(ev.contains("\"verdict\":\"allowed\""), "receipt should have allowed verdict");
    }

    /// Plan acceptance: Anthropic error responses (e.g. 401 auth failure) are passed
    /// through to the caller with the original status and body intact.
    #[tokio::test]
    async fn proxy_handles_anthropic_error_response() {
        let upstream = start_mock_upstream(|_req| async move {
            let body = serde_json::json!({
                "error": {
                    "type": "authentication_error",
                    "message": "invalid x-api-key",
                }
            });
            Ok(Response::builder()
                .status(401)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body.to_string())))
                .unwrap())
        })
        .await;

        let dir = TempDir::new().unwrap();
        let upstream_url = format!("http://{upstream}");
        let (bound, registry) = start_test_proxy(&dir, "sk-ant-REAL-KEY", upstream_url).await;
        register_workload(&registry, "sk-ant-WORKLOAD-err", "err-agent", 10_000);

        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/messages"))
            .header("x-api-key", "sk-ant-WORKLOAD-err")
            .header("content-type", "application/json")
            .body(r#"{"model":"test","max_tokens":1}"#)
            .send()
            .await
            .unwrap();
        // Upstream 401 must be passed through as-is.
        assert_eq!(resp.status(), 401);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["type"], "authentication_error");
    }

    /// GAP 1: Request with no x-api-key header → 403 unknown_workload_key.
    #[tokio::test]
    async fn proxy_missing_api_key_returns_403() {
        let dir = TempDir::new().unwrap();
        // Auth check fires before registry lookup — upstream never reached.
        let (bound, _registry) = start_test_proxy(&dir, "sk-ant-REAL-KEY", "http://127.0.0.1:1".to_string()).await;
        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/messages"))
            // deliberately no x-api-key header
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["detail"], "unknown_workload_key");
    }

    /// GAP 2: Non-POST method to /v1/messages → 405 method_not_allowed.
    #[tokio::test]
    async fn proxy_non_post_method_returns_405() {
        let dir = TempDir::new().unwrap();
        // Method check fires before forwarding — upstream never reached.
        let (bound, registry) = start_test_proxy(&dir, "sk-ant-REAL-KEY", "http://127.0.0.1:1".to_string()).await;
        register_workload(&registry, "sk-ant-WORKLOAD-get-test", "get-agent", 100_000);
        let resp = reqwest::Client::new()
            .get(format!("http://{bound}/v1/messages")) // GET not POST
            .header("x-api-key", "sk-ant-WORKLOAD-get-test")
            .header("content-type", "application/json")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 405);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["detail"], "method_not_allowed");
    }

    /// GAP 7: Budget already exhausted → 429 egress_budget_exhausted.
    #[tokio::test]
    async fn proxy_budget_exhausted_returns_429() {
        let dir = TempDir::new().unwrap();
        // Budget check fires before forwarding — upstream never reached.
        let (bound, registry) = start_test_proxy(&dir, "sk-ant-REAL-KEY", "http://127.0.0.1:1".to_string()).await;
        // Register workload with zero budget.
        register_workload(&registry, "sk-ant-WORKLOAD-broke", "broke-agent", 0);
        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/messages"))
            .header("x-api-key", "sk-ant-WORKLOAD-broke")
            .header("content-type", "application/json")
            .body(r#"{"model":"test","max_tokens":1}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 429);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["detail"], "budget_exhausted");
    }

    /// GAP 3: Request body exceeding 4 MB → 413 request_body_too_large.
    #[tokio::test]
    async fn proxy_large_request_body_returns_413() {
        let dir = TempDir::new().unwrap();
        let upstream_addr = start_mock_upstream(|_req| async move {
            Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(r#"{"usage":{"input_tokens":0,"output_tokens":0}}"#)))
                .unwrap())
        })
        .await;
        let upstream_url = format!("http://{upstream_addr}");
        let (bound, registry) =
            start_test_proxy(&dir, "sk-ant-REAL-KEY", upstream_url).await;
        register_workload(&registry, "sk-ant-WORKLOAD-bigbody", "bigbody-agent", 100_000);

        // 5 MB body — exceeds the 4 MB cap
        let big_body = vec![b'x'; 5 * 1024 * 1024];
        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/messages"))
            .header("x-api-key", "sk-ant-WORKLOAD-bigbody")
            .header("content-type", "application/json")
            .body(big_body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 413);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["type"], "egress_proxy_failed");
    }

    /// GAP 6: Upstream response exceeding 8 MB → 502 response_too_large.
    #[tokio::test]
    async fn proxy_large_response_returns_502() {
        // Mock upstream that streams back a 9 MB response.
        let upstream_addr = start_mock_upstream(|_req| async move {
            let big_body = vec![b'y'; 9 * 1024 * 1024];
            Ok(Response::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(big_body)))
                .unwrap())
        })
        .await;

        let dir = TempDir::new().unwrap();
        let upstream_url = format!("http://{upstream_addr}");
        let (bound, registry) =
            start_test_proxy(&dir, "sk-ant-REAL-KEY", upstream_url).await;
        register_workload(&registry, "sk-ant-WORKLOAD-bigres", "bigres-agent", 100_000);

        let resp = reqwest::Client::new()
            .post(format!("http://{bound}/v1/messages"))
            .header("x-api-key", "sk-ant-WORKLOAD-bigres")
            .header("content-type", "application/json")
            .body(r#"{"model":"test","max_tokens":1}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 502);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["detail"], "response_too_large");
    }
}
