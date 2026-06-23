//! In-process egress mediator for native agents (p7.5 vertical slice).
//!
//! Native agents call `InferenceGateway` directly (no HTTP hop). This struct
//! intercepts inference results and:
//!   1. Records a signed action receipt via `EvidenceWriter`.
//!   2. Emits `EgressBrokered` + `ActionReceiptEmitted` flight events.
//!
//! An HTTP stub server is included for p7.5b readiness: when `proxy_addr` is
//! configured it binds and returns `501 Not Implemented` for all requests.
//! Actual universal-tier forwarding is deferred to p7.5b.

use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use http_body_util::Empty;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::json;
use tokio::net::TcpListener;

use crate::events::EventKind;
use crate::evidence::EvidenceWriter;
use crate::flight_recorder::FlightRecorder;

/// In-process egress mediator for native agents.
pub struct EgressProxy {
    writer: Arc<EvidenceWriter>,
    recorder: Arc<FlightRecorder>,
}

impl EgressProxy {
    pub fn new(writer: Arc<EvidenceWriter>, recorder: Arc<FlightRecorder>) -> Self {
        Self { writer, recorder }
    }

    /// Record a permitted model inference. Emits `EgressBrokered` + `ActionReceiptEmitted`.
    pub fn record_inference(
        &self,
        agent_id: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
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
}

/// Bind an HTTP stub server. Every request returns `501 Not Implemented`.
/// Returns the actual bound address. Spawns a background Tokio task.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{evidence::EvidenceWriter, flight_recorder::FlightRecorder};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_proxy(dir: &TempDir) -> EgressProxy {
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
        let proxy = make_proxy(&dir);
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
        let proxy = make_proxy(&dir);
        proxy.record_denied("agent_0", "https://evil.example.com");
        let ev_content = std::fs::read_to_string(dir.path().join("evidence.jsonl")).unwrap();
        assert!(ev_content.contains("\"verdict\":\"denied\""));
        let log = std::fs::read_to_string(dir.path().join("flight.jsonl")).unwrap();
        assert!(log.contains("egress_denied"));
        assert!(log.contains("action_receipt_emitted"));
    }

    #[tokio::test]
    async fn http_stub_returns_501() {
        let addr = start_http_stub("127.0.0.1:0").await.unwrap();
        let url = format!("http://{addr}/v1/messages");
        let resp = reqwest::Client::new().post(&url).send().await.unwrap();
        assert_eq!(resp.status(), 501);
    }
}
