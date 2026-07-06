//! Credential broker (cred.3).
//!
//! `CredentialGateway` is a second loopback HTTP listener (OS-assigned port) that
//! MCP server subprocesses call to access provider credentials without holding them
//! directly. The gateway:
//!
//! 1. Identifies the caller via an ephemeral credential token (`x-credential-token`
//!    header) issued per MCP spawn.
//! 2. Scrubs credential headers (`Authorization`, `Host`, `X-Subscription-Token`, and
//!    the provider's configured `header_name`) from the caller's request.
//! 3. Retrieves the credential for the named provider (OAuth bearer token or API key).
//! 4. Attaches the credential per the provider's `auth_style`.
//! 5. Forwards the request to `upstream_base + path` via reqwest.
//! 6. Emits `CredentialAccessed` + `CredentialEgressBrokered` flight events.
//!
//! Budget enforcement is deferred to cred.4. This increment instruments the path only.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};

use crate::config::{AuthStyle, CredentialGatewayConfig, ProviderConfig};
use crate::events::EventKind;
use crate::flight_recorder::FlightRecorder;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Seconds before a cached access token expires where we proactively refresh.
const TOKEN_EXPIRY_BUFFER_SECS: u64 = 60;
/// Maximum response body from an upstream provider (4 MB).
const MAX_UPSTREAM_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum inbound request body from an MCP server (4 MB).
const MAX_INBOUND_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// Connect timeout for upstream requests.
const CREDENTIAL_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Total request + response timeout.
const CREDENTIAL_REQUEST_TIMEOUT_SECS: u64 = 60;

// ── Token cache types ──────────────────────────────────────────────────────────

/// Secrets file written by `agentctl auth google` and mounted at `/run/secrets/google.json`.
#[derive(Debug, Deserialize)]
struct OAuthSecretsFile {
    client_id:     String,
    client_secret: String,
    refresh_token: String,
    #[serde(default = "default_google_token_url")]
    token_url:     String,
}

fn default_google_token_url() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

/// Runtime state written to `state_path` (atomic tmp→rename).
#[derive(Debug, Serialize, Deserialize)]
struct OAuthState {
    access_token:    String,
    /// Unix timestamp (seconds) when this token expires.
    expires_at_unix: u64,
    /// Rotated refresh token from the provider response (may be absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token:   Option<String>,
}

/// Response from an OAuth token endpoint (subset).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token:  String,
    expires_in:    Option<u64>,
    refresh_token: Option<String>,
}

// ── CredentialRegistry ─────────────────────────────────────────────────────────

/// One entry in the credential token registry.
struct CredEntry {
    agent_id:          String,
    allowed_providers: Vec<String>,
}

/// Maps ephemeral credential token → `CredEntry`.
/// Tokens are UUID4 strings issued at MCP spawn and deregistered on exit.
#[derive(Default)]
struct CredentialRegistry {
    tokens: RwLock<HashMap<String, CredEntry>>,
}

impl CredentialRegistry {
    fn new() -> Self {
        Self::default()
    }

    async fn register(&self, token: String, agent_id: String, providers: Vec<String>) {
        let mut map = self.tokens.write().await;
        map.insert(token, CredEntry { agent_id, allowed_providers: providers });
    }

    async fn deregister(&self, token: &str) {
        let mut map = self.tokens.write().await;
        map.remove(token);
    }

    /// Returns `(agent_id, allowed_providers)` or `None` if the token is unknown.
    async fn lookup(&self, token: &str) -> Option<(String, Vec<String>)> {
        let map = self.tokens.read().await;
        map.get(token).map(|e| (e.agent_id.clone(), e.allowed_providers.clone()))
    }
}

// ── OAuthTokenCache ────────────────────────────────────────────────────────────

/// Inner state of the token cache, held under a single Mutex.
struct OAuthCacheInner {
    token:         Option<String>,
    expires_at:    u64,
    refresh_token: Option<String>,
}

/// Per-provider OAuth token cache. Reads secrets from `token_path`, caches the
/// access token in memory, and writes refreshed state to `state_path` atomically.
///
/// A single `Mutex<OAuthCacheInner>` serializes concurrent refreshes for the same
/// provider. The mutex is held for the entire refresh (including the network call)
/// so a second concurrent caller waits and then finds the already-refreshed token,
/// preventing duplicate network requests and rotated-token clobbering.
struct OAuthTokenCache {
    state: Mutex<OAuthCacheInner>,
}

impl OAuthTokenCache {
    fn new() -> Self {
        Self {
            state: Mutex::new(OAuthCacheInner {
                token:         None,
                expires_at:    0,
                refresh_token: None,
            }),
        }
    }

    /// Return a valid access token, refreshing if needed.
    ///
    /// If the refresh token rotation write fails (QEMU 9p atomicity issue), emits
    /// `CredentialRefreshFailed` with `token_written: false` but still returns the
    /// access token for this request — preventing silent unrecoverable failure.
    async fn get_or_refresh(
        &self,
        provider:  &str,
        cfg:       &ProviderConfig,
        recorder:  &Arc<FlightRecorder>,
        client:    &reqwest::Client,
    ) -> Result<String, String> {
        let mut inner = self.state.lock().await;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Fast path: check cached token under the mutex (prevents TOCTOU with slow path).
        if let Some(ref tok) = inner.token {
            if inner.expires_at > now + TOKEN_EXPIRY_BUFFER_SECS {
                return Ok(tok.clone());
            }
        }

        // Slow path: need to refresh. Lock held to serialize concurrent refreshes.
        let token_path = cfg.token_path.as_deref().unwrap_or("");
        if token_path.is_empty() {
            return Err(format!("provider '{}' has no token_path configured", provider));
        }
        let secrets_bytes = tokio::fs::read(token_path).await.map_err(|e| {
            format!("cannot read secrets file '{}': {e}", token_path)
        })?;
        let secrets: OAuthSecretsFile = serde_json::from_slice(&secrets_bytes).map_err(|e| {
            format!("cannot parse secrets file '{}': {e}", token_path)
        })?;
        if !secrets.token_url.starts_with("https://") {
            return Err(format!(
                "provider '{}' token_url must use https://, got '{}'",
                provider,
                &secrets.token_url[..secrets.token_url.len().min(64)],
            ));
        }

        // Use the cached refresh_token if available (rotation may have updated it).
        let refresh_token = inner.refresh_token.clone()
            .unwrap_or_else(|| secrets.refresh_token.clone());

        // POST to token endpoint.
        let params = [
            ("client_id",     secrets.client_id.as_str()),
            ("client_secret", secrets.client_secret.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type",    "refresh_token"),
        ];
        let resp = client.post(&secrets.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| format!("token refresh request failed: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("token refresh HTTP {status}: {body:.512}"));
        }
        let tok_resp: TokenResponse = resp.json().await.map_err(|e| {
            format!("token refresh response parse failed: {e}")
        })?;

        let new_access = tok_resp.access_token;
        let expires_in = tok_resp.expires_in.unwrap_or(3600);
        let new_expires = now + expires_in;
        let new_refresh = tok_resp.refresh_token;

        // Update in-memory cache atomically (all fields in one lock acquisition).
        inner.token = Some(new_access.clone());
        inner.expires_at = new_expires;
        if let Some(ref rt) = new_refresh {
            inner.refresh_token = Some(rt.clone());
        }

        // Atomically write state to state_path.
        if let Some(ref state_path) = cfg.state_path {
            let state = OAuthState {
                access_token:    new_access.clone(),
                expires_at_unix: new_expires,
                refresh_token:   new_refresh.clone(),
            };
            let json_bytes = serde_json::to_vec(&state).unwrap_or_default();
            let write_result = write_state_atomic(state_path, &json_bytes).await;
            if let Err(ref e) = write_result {
                // Critical: emit even though we still return the token.
                // Prevents silent loss of a rotated refresh token on QEMU 9p.
                let err_str = e.to_string();
                recorder.record(
                    "credential_gateway",
                    None,
                    EventKind::CredentialRefreshFailed,
                    json!({
                        "provider":      provider,
                        "error":         err_str,
                        "token_written": false,
                    }),
                );
                tracing::warn!(
                    provider = %provider,
                    path     = %state_path,
                    error    = %e,
                    "credential: state write failed — rotated token not persisted (cred.3-ar-02)"
                );
            }
        }

        Ok(new_access)
    }
}

/// Normalise a path segment from the inbound URI, removing `..` components to
/// prevent path traversal outside the upstream base path.
fn normalize_path_segment(seg: &str) -> String {
    seg.split('/')
        .filter(|c| !c.is_empty() && *c != ".." && *c != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Atomic tmp → rename write. Writes to a sibling `.tmp` file then renames.
async fn write_state_atomic(path: &str, data: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let tmp = format!("{}.tmp", path);
    // Ensure parent directory exists.
    if let Some(parent) = std::path::Path::new(path).parent() {
        tokio::fs::create_dir_all(parent).await
            .with_context(|| format!("create state dir '{}'", parent.display()))?;
    }
    let open_opts = {
        let mut o = tokio::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        o.mode(0o600);
        o
    };
    let mut f = open_opts
        .open(&tmp)
        .await
        .with_context(|| format!("open tmp state file '{}'", tmp))?;
    f.write_all(data).await.with_context(|| format!("write tmp state file '{}'", tmp))?;
    f.flush().await.with_context(|| format!("flush tmp state file '{}'", tmp))?;
    drop(f);
    tokio::fs::rename(&tmp, path).await
        .with_context(|| format!("rename '{}' → '{}'", tmp, path))?;
    Ok(())
}

// ── GatewayState ──────────────────────────────────────────────────────────────

struct GatewayState {
    config:    CredentialGatewayConfig,
    registry:  Arc<CredentialRegistry>,
    caches:    RwLock<HashMap<String, Arc<OAuthTokenCache>>>,
    client:    reqwest::Client,
    recorder:  Arc<FlightRecorder>,
}

impl GatewayState {
    fn new(config: CredentialGatewayConfig, recorder: Arc<FlightRecorder>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(CREDENTIAL_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(CREDENTIAL_REQUEST_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("build credential gateway reqwest client")?;
        Ok(Self {
            config,
            registry:  Arc::new(CredentialRegistry::new()),
            caches:    RwLock::new(HashMap::new()),
            client,
            recorder,
        })
    }

    async fn get_cache(&self, provider: &str) -> Arc<OAuthTokenCache> {
        {
            let map = self.caches.read().await;
            if let Some(c) = map.get(provider) {
                return Arc::clone(c);
            }
        }
        let mut map = self.caches.write().await;
        // Re-check: another task may have inserted while we waited for the write lock.
        if let Some(c) = map.get(provider) {
            return Arc::clone(c);
        }
        let c = Arc::new(OAuthTokenCache::new());
        map.insert(provider.to_string(), Arc::clone(&c));
        c
    }
}

// ── HTTP handler ──────────────────────────────────────────────────────────────

/// Headers always stripped from inbound caller requests before broker attaches auth.
const SCRUB_HEADERS: &[&str] = &[
    "authorization",
    "host",
    "x-subscription-token",
    "x-credential-token",
    // Hop-by-hop headers.
    "connection",
    "transfer-encoding",
    "content-length",
];

fn json_response(status: u16, body: serde_json::Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .expect("credential json_response builder must not fail")
}

async fn handle_credential_request(
    state: Arc<GatewayState>,
    req:   Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    // 1. Extract x-credential-token.
    let cred_token = match req.headers().get("x-credential-token").and_then(|v| v.to_str().ok()) {
        Some(t) => t.to_owned(),
        None => {
            return Ok(json_response(
                401,
                json!({"error": "missing_credential_token", "hint": "Set x-credential-token header"}),
            ))
        }
    };

    // 2. Registry lookup.
    let (agent_id, allowed_providers) = match state.registry.lookup(&cred_token).await {
        Some(e) => e,
        None => {
            return Ok(json_response(
                401,
                json!({"error": "invalid_credential_token"}),
            ))
        }
    };

    // 3. Parse /<provider>/<path...> from URI.
    let uri_path = req.uri().path().to_owned();
    let mut segments = uri_path.splitn(3, '/');
    segments.next(); // leading empty before first '/'
    let provider = match segments.next() {
        Some(p) if !p.is_empty() => p.to_owned(),
        _ => {
            return Ok(json_response(
                400,
                json!({"error": "bad_request", "hint": "URI must be /<provider>/<path>"}),
            ))
        }
    };
    let rest = segments.next().unwrap_or("");

    // 4. Provider capability check (cred.4 enforces; here we just audit).
    if !allowed_providers.contains(&provider) {
        state.recorder.record(
            &agent_id,
            None,
            EventKind::CredentialDenied,
            json!({"agent_id": agent_id, "provider": provider}),
        );
        return Ok(json_response(
            403,
            json!({
                "error":    "credential_denied",
                "provider": provider,
                "hint":     "Add Credential capability for this provider to your agent config",
            }),
        ));
    }

    // 5. Provider config lookup.
    let prov_cfg = match state.config.providers.get(&provider) {
        Some(c) => c.clone(),
        None => {
            state.recorder.record(
                &agent_id,
                None,
                EventKind::CredentialNotProvisioned,
                json!({
                    "provider": provider,
                    "hint": format!(
                        "Add [credential_gateway.providers.{provider}] to your agent config"
                    ),
                }),
            );
            return Ok(json_response(
                503,
                json!({
                    "error":    "credential_not_provisioned",
                    "provider": provider,
                    "hint":     format!(
                        "Add [credential_gateway.providers.{provider}] to your agent config \
                         or run `agentctl auth {provider}` on the host"
                    ),
                }),
            ));
        }
    };

    // 6. Collect inbound body (bounded).
    let method = req.method().clone();
    let query   = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let (parts, body) = req.into_parts();
    let limited = http_body_util::Limited::new(body, MAX_INBOUND_REQUEST_BYTES);
    let body_bytes = match limited.collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Ok(json_response(
                413,
                json!({"error": "request_body_too_large"}),
            ))
        }
    };

    // 7. Build scrub set: always-scrubbed + provider-specific header.
    let mut scrub_set: Vec<String> = SCRUB_HEADERS.iter().map(|s| s.to_lowercase()).collect();
    if let Some(ref hname) = prov_cfg.header_name {
        scrub_set.push(hname.to_lowercase());
    }

    // 8. Get credential.
    let credential = match prov_cfg.auth_style {
        AuthStyle::OauthBearer => {
            let cache = state.get_cache(&provider).await;
            match cache.get_or_refresh(&provider, &prov_cfg, &state.recorder, &state.client).await {
                Ok(tok) => tok,
                Err(e) => {
                    tracing::warn!(provider = %provider, error = %e, "credential refresh failed");
                    return Ok(json_response(
                        503,
                        json!({
                            "error":    "credential_refresh_failed",
                            "provider": provider,
                            "hint":     "Check token_path secrets and state_path permissions",
                        }),
                    ));
                }
            }
        }
        AuthStyle::ApiKeyHeader | AuthStyle::ApiKeyQuery => {
            let key_var = prov_cfg.secret_key.as_deref().unwrap_or("");
            if key_var.is_empty() {
                return Ok(json_response(
                    503,
                    json!({
                        "error":    "credential_not_provisioned",
                        "provider": provider,
                        "hint":     "Set secret_key in [credential_gateway.providers] config",
                    }),
                ));
            }
            match std::env::var(key_var) {
                Ok(v) if !v.is_empty() => v,
                _ => {
                    state.recorder.record(
                        &agent_id,
                        None,
                        EventKind::CredentialNotProvisioned,
                        json!({"provider": provider, "hint": format!("{key_var} env var not set")}),
                    );
                    return Ok(json_response(
                        503,
                        json!({
                            "error":    "credential_not_provisioned",
                            "provider": provider,
                            "hint":     format!("Set the {key_var} environment variable"),
                        }),
                    ));
                }
            }
        }
    };

    // 9. Build upstream URL, normalising the rest segment to prevent path traversal.
    let upstream_base = prov_cfg.upstream_base.trim_end_matches('/');
    let safe_rest = normalize_path_segment(rest);
    let upstream_url = if safe_rest.is_empty() {
        format!("{upstream_base}/{query}")
    } else {
        format!("{upstream_base}/{safe_rest}{query}")
    };

    // 10. Build upstream request, scrubbing inbound headers.
    let mut req_builder = state.client.request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
        &upstream_url,
    );
    for (name, value) in &parts.headers {
        let name_lower = name.as_str().to_lowercase();
        if scrub_set.contains(&name_lower) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            req_builder = req_builder.header(name.as_str(), v);
        }
    }

    // 11. Attach credential per auth_style. ApiKeyQuery uses reqwest's .query() so
    //     the key and value are percent-encoded automatically.
    req_builder = match prov_cfg.auth_style {
        AuthStyle::OauthBearer => {
            req_builder.header("Authorization", format!("Bearer {credential}"))
        }
        AuthStyle::ApiKeyHeader => {
            let hname = prov_cfg.header_name.as_deref().unwrap_or("X-Api-Key");
            req_builder.header(hname, &credential)
        }
        AuthStyle::ApiKeyQuery => {
            let key_param = prov_cfg.header_name.as_deref().unwrap_or("key");
            req_builder.query(&[(key_param, &credential)])
        }
    };

    if !body_bytes.is_empty() {
        req_builder = req_builder.body(body_bytes);
    }

    // 12. Emit CredentialAccessed before forwarding.
    let path_str = format!("/{}{}{}", provider, if rest.is_empty() { "" } else { "/" }, rest);
    state.recorder.record(
        &agent_id,
        None,
        EventKind::CredentialAccessed,
        json!({
            "agent_id": agent_id,
            "provider": provider,
            "path":     path_str,
            "method":   method.as_str(),
        }),
    );

    // 13. Send upstream request.
    let upstream_resp = match req_builder.send().await {
        Ok(r)  => r,
        Err(e) => {
            tracing::warn!(provider = %provider, error = %e, "credential upstream request failed");
            return Ok(json_response(
                502,
                json!({"error": "upstream_error", "provider": provider}),
            ));
        }
    };

    let response_status = upstream_resp.status().as_u16();

    // 14. Collect response body (bounded).
    let resp_bytes = match upstream_resp.bytes().await {
        Ok(b) if b.len() <= MAX_UPSTREAM_RESPONSE_BYTES => b,
        Ok(_) => {
            return Ok(json_response(
                502,
                json!({"error": "upstream_response_too_large", "provider": provider}),
            ));
        }
        Err(e) => {
            tracing::warn!(provider = %provider, error = %e, "reading upstream response body");
            return Ok(json_response(
                502,
                json!({"error": "upstream_body_error", "provider": provider}),
            ));
        }
    };

    // 15. Emit CredentialEgressBrokered.
    let response_bytes = resp_bytes.len();
    state.recorder.record(
        &agent_id,
        None,
        EventKind::CredentialEgressBrokered,
        json!({
            "agent_id":        agent_id,
            "provider":        provider,
            "path":            path_str,
            "response_status": response_status,
            "response_bytes":  response_bytes,
        }),
    );

    Ok(Response::builder()
        .status(response_status)
        .header("content-type", "application/json")
        .body(Full::new(resp_bytes))
        .expect("credential response builder must not fail"))
}

// ── CredentialGateway ──────────────────────────────────────────────────────────

/// In-process credential broker.
///
/// Binds a second OS-assigned loopback listener. MCP servers call it via
/// `AGENTD_CREDENTIAL_GATEWAY_URL` with `x-credential-token`.
pub struct CredentialGateway {
    registry: Arc<CredentialRegistry>,
}

impl CredentialGateway {
    /// Start the gateway. Returns `(Arc<CredentialGateway>, bound_addr)`.
    pub async fn start(
        cfg:      &CredentialGatewayConfig,
        recorder: Arc<FlightRecorder>,
    ) -> Result<(Arc<Self>, std::net::SocketAddr)> {
        // Validate all provider configs at startup.
        for (name, prov) in &cfg.providers {
            anyhow::ensure!(
                prov.upstream_base.starts_with("https://"),
                "credential gateway: provider '{}' upstream_base must use https://, got '{}'",
                name,
                prov.upstream_base,
            );
        }
        let state = Arc::new(GatewayState::new(cfg.clone(), recorder)?);
        let registry = Arc::clone(&state.registry);

        let listener = TcpListener::bind("127.0.0.1:0").await
            .context("credential gateway: bind loopback listener")?;
        let bound = listener.local_addr().context("credential gateway: local_addr")?;

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s)  => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "credential gateway: accept error");
                        continue;
                    }
                };
                let io    = TokioIo::new(stream);
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let svc = service_fn(move |req| {
                        handle_credential_request(Arc::clone(&state), req)
                    });
                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await
                    {
                        tracing::debug!(error = %e, "credential gateway: connection closed");
                    }
                });
            }
        });

        Ok((Arc::new(Self { registry }), bound))
    }

    /// Register an ephemeral credential token for a new MCP server spawn.
    pub async fn register_token(&self, token: String, agent_id: String, providers: Vec<String>) {
        self.registry.register(token, agent_id, providers).await;
    }

    /// Deregister an ephemeral token when an MCP server exits.
    pub async fn deregister_token(&self, token: &str) {
        self.registry.deregister(token).await;
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthStyle, CredentialGatewayConfig, ProviderConfig};

    #[allow(dead_code)]
    fn provider_cfg_oauth() -> ProviderConfig {
        ProviderConfig {
            auth_style:    AuthStyle::OauthBearer,
            upstream_base: "https://www.googleapis.com".to_string(),
            header_name:   None,
            secret_key:    None,
            token_path:    Some("/run/secrets/google.json".to_string()),
            state_path:    Some("/data/state/oauth/google.json".to_string()),
        }
    }

    fn provider_cfg_api_key_header() -> ProviderConfig {
        ProviderConfig {
            auth_style:    AuthStyle::ApiKeyHeader,
            upstream_base: "https://api.search.brave.com".to_string(),
            header_name:   Some("X-Subscription-Token".to_string()),
            secret_key:    Some("BRAVE_SEARCH_API_KEY".to_string()),
            token_path:    None,
            state_path:    None,
        }
    }

    // ── T1: TOML parse/serialize for oauth-bearer ────────────────────────────

    #[test]
    fn test_provider_config_oauth_bearer_roundtrip() {
        let toml_str = r#"
auth_style    = "oauth-bearer"
upstream_base = "https://www.googleapis.com"
token_path    = "/run/secrets/google.json"
state_path    = "/data/state/oauth/google.json"
"#;
        let cfg: ProviderConfig = toml::from_str(toml_str).expect("parse oauth-bearer config");
        assert_eq!(cfg.auth_style, AuthStyle::OauthBearer);
        assert_eq!(cfg.upstream_base, "https://www.googleapis.com");
        assert_eq!(cfg.token_path.as_deref(), Some("/run/secrets/google.json"));
        assert_eq!(cfg.state_path.as_deref(), Some("/data/state/oauth/google.json"));
    }

    // ── T2: TOML parse for api-key-header ────────────────────────────────────

    #[test]
    fn test_provider_config_api_key_header_roundtrip() {
        let toml_str = r#"
auth_style    = "api-key-header"
upstream_base = "https://api.search.brave.com"
header_name   = "X-Subscription-Token"
secret_key    = "BRAVE_SEARCH_API_KEY"
"#;
        let cfg: ProviderConfig = toml::from_str(toml_str).expect("parse api-key-header config");
        assert_eq!(cfg.auth_style, AuthStyle::ApiKeyHeader);
        assert_eq!(cfg.header_name.as_deref(), Some("X-Subscription-Token"));
        assert_eq!(cfg.secret_key.as_deref(), Some("BRAVE_SEARCH_API_KEY"));
        assert!(cfg.token_path.is_none());
    }

    // ── T3: disabled config skips gateway start ───────────────────────────────

    #[test]
    fn test_credential_gateway_config_disabled_by_default() {
        let cfg = CredentialGatewayConfig::default();
        assert!(!cfg.enabled, "default config must be disabled");
        assert!(cfg.providers.is_empty());
    }

    // ── T4: register + deregister token ──────────────────────────────────────

    #[tokio::test]
    async fn test_register_deregister_token() {
        let reg = CredentialRegistry::new();
        reg.register("tok1".to_string(), "agent-a".to_string(), vec!["google".to_string()]).await;
        let result = reg.lookup("tok1").await;
        assert!(result.is_some());
        let (agent, providers) = result.unwrap();
        assert_eq!(agent, "agent-a");
        assert_eq!(providers, vec!["google"]);
        reg.deregister("tok1").await;
        assert!(reg.lookup("tok1").await.is_none());
    }

    // ── T5: unknown token returns None ────────────────────────────────────────

    #[tokio::test]
    async fn test_token_not_found_returns_none() {
        let reg = CredentialRegistry::new();
        assert!(reg.lookup("ghost-token").await.is_none());
    }

    // ── T6: oauth state — valid unexpired token returned directly ─────────────

    #[tokio::test]
    async fn test_oauth_token_state_read_valid() {
        let cache = OAuthTokenCache::new();
        let future_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH).unwrap().as_secs()
            + 3600;
        {
            let mut inner = cache.state.lock().await;
            inner.token = Some("ya29.valid".to_string());
            inner.expires_at = future_ts;
        }

        // Should return the cached token without a network call.
        let inner = cache.state.lock().await;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(inner.token.is_some(), "cached token should be present");
        assert!(inner.expires_at > now + TOKEN_EXPIRY_BUFFER_SECS, "token should not need refresh");
    }

    // ── T7: expired token triggers refresh path ───────────────────────────────

    #[tokio::test]
    async fn test_oauth_token_state_read_expired() {
        let cache = OAuthTokenCache::new();
        // Expired 5 seconds ago.
        let past_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH).unwrap().as_secs()
            .saturating_sub(5);
        {
            let mut inner = cache.state.lock().await;
            inner.token = Some("ya29.expired".to_string());
            inner.expires_at = past_ts;
        }

        let inner = cache.state.lock().await;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(inner.expires_at <= now + TOKEN_EXPIRY_BUFFER_SECS, "expired token must trigger refresh path");
    }

    // ── T8: atomic state write ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_oauth_state_write_atomic() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let state_path = dir.path().join("google.json").to_string_lossy().to_string();
        let data = br#"{"access_token":"ya29.x","expires_at_unix":9999999999}"#;
        write_state_atomic(&state_path, data).await.expect("write_state_atomic must succeed");
        let read_back = std::fs::read(&state_path).expect("read state file");
        assert_eq!(read_back, data);
        // No .tmp file should remain.
        let tmp_path = format!("{}.tmp", state_path);
        assert!(!std::path::Path::new(&tmp_path).exists(), ".tmp file must be cleaned up");
    }

    // ── T9: header scrubbing removes Authorization ────────────────────────────

    #[test]
    fn test_header_scrubbing_removes_authorization() {
        let scrub = SCRUB_HEADERS;
        assert!(scrub.contains(&"authorization"), "authorization must be scrubbed");
        assert!(scrub.contains(&"host"), "host must be scrubbed");
        assert!(scrub.contains(&"x-subscription-token"), "x-subscription-token must be scrubbed");
        assert!(scrub.contains(&"x-credential-token"), "x-credential-token must be scrubbed");
    }

    // ── T10: custom header_name also scrubbed ─────────────────────────────────

    #[test]
    fn test_header_scrubbing_removes_custom_header() {
        let cfg = provider_cfg_api_key_header();
        // Verify header_name is set.
        assert_eq!(cfg.header_name.as_deref(), Some("X-Subscription-Token"));
        // The handler adds header_name to scrub set (lowercased).
        let mut scrub_set: Vec<String> = SCRUB_HEADERS.iter().map(|s| s.to_lowercase()).collect();
        if let Some(ref hname) = cfg.header_name {
            scrub_set.push(hname.to_lowercase());
        }
        assert!(scrub_set.contains(&"x-subscription-token".to_string()));
    }

    // ── T11: Capability::Credential satisfies ────────────────────────────────

    #[test]
    fn test_capability_credential_google_satisfies() {
        use crate::capability::{satisfies, Capability, CredentialProvider};
        let granted = vec![Capability::Credential {
            provider: CredentialProvider::Google,
        }];
        assert!(satisfies(
            &granted,
            &Capability::Credential { provider: CredentialProvider::Google }
        ));
        assert!(!satisfies(
            &granted,
            &Capability::Credential { provider: CredentialProvider::BraveSearch }
        ));
    }

    // ── T12: Custom provider denied without matching grant ────────────────────

    #[test]
    fn test_capability_credential_custom_denied_without_matching_grant() {
        use crate::capability::{satisfies, Capability, CredentialProvider};
        let granted = vec![Capability::Credential {
            provider: CredentialProvider::Custom("my-api".to_string()),
        }];
        // Same custom name → granted.
        assert!(satisfies(
            &granted,
            &Capability::Credential { provider: CredentialProvider::Custom("my-api".to_string()) }
        ));
        // Different custom name → denied.
        assert!(!satisfies(
            &granted,
            &Capability::Credential { provider: CredentialProvider::Custom("other-api".to_string()) }
        ));
    }

    // ── T13: PASSENV_BLOCKLIST contains broker vars ───────────────────────────

    #[test]
    fn test_passenv_blocklist_contains_broker_vars() {
        use crate::tools::mcp::PASSENV_BLOCKLIST;
        assert!(PASSENV_BLOCKLIST.contains(&"BRAVE_SEARCH_API_KEY"));
        assert!(PASSENV_BLOCKLIST.contains(&"OAUTH_REFRESH_TOKEN"));
        assert!(PASSENV_BLOCKLIST.contains(&"OAUTH_CLIENT_SECRET"));
        assert!(PASSENV_BLOCKLIST.contains(&"AGENTD_CREDENTIAL_TOKEN"));
    }

    // ── T14: AuthStyle deserializes from kebab-case ───────────────────────────

    #[test]
    fn test_auth_style_deserialize_kebab_case() {
        // Use serde_json for bare-value deserialization (TOML requires key = value).
        let s: AuthStyle = serde_json::from_str(r#""oauth-bearer""#).expect("parse oauth-bearer");
        assert_eq!(s, AuthStyle::OauthBearer);
        let s: AuthStyle = serde_json::from_str(r#""api-key-header""#).expect("parse api-key-header");
        assert_eq!(s, AuthStyle::ApiKeyHeader);
        let s: AuthStyle = serde_json::from_str(r#""api-key-query""#).expect("parse api-key-query");
        assert_eq!(s, AuthStyle::ApiKeyQuery);
    }

    // ── T15: 503 response body contains required fields ───────────────────────

    #[test]
    fn test_credential_not_provisioned_response_format() {
        let body = json!({
            "error":    "credential_not_provisioned",
            "provider": "google",
            "hint":     "Add [credential_gateway.providers.google] to config",
        });
        let resp = json_response(503, body.clone());
        assert_eq!(resp.status().as_u16(), 503);
        assert_eq!(body["error"], "credential_not_provisioned");
        assert_eq!(body["provider"], "google");
    }

    // ── T15b: normalize_path_segment blocks traversal ────────────────────────

    #[test]
    fn test_normalize_path_segment_blocks_traversal() {
        assert_eq!(normalize_path_segment("../../etc/passwd"), "etc/passwd");
        assert_eq!(normalize_path_segment("foo/../bar"), "foo/bar");
        assert_eq!(normalize_path_segment("./foo/./bar"), "foo/bar");
        assert_eq!(normalize_path_segment("v1/messages"), "v1/messages");
        assert_eq!(normalize_path_segment(""), "");
    }

    // ── T16: upstream_base must use https:// ─────────────────────────────────

    #[tokio::test]
    async fn test_gateway_rejects_http_upstream_base() {
        let dir = tempfile::tempdir().expect("tempdir");
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("flight.jsonl")).expect("recorder"),
        );
        let mut cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        cfg.providers.insert("bad-provider".to_string(), ProviderConfig {
            auth_style:    AuthStyle::ApiKeyHeader,
            upstream_base: "http://insecure.example.com".to_string(),
            header_name:   None,
            secret_key:    None,
            token_path:    None,
            state_path:    None,
        });
        let err = CredentialGateway::start(&cfg, recorder).await
            .err()
            .expect("gateway must reject http:// upstream_base");
        assert!(err.to_string().contains("https://"), "error must mention https://");
    }

    // ── T17: full gateway round-trip using a test provider ────────────────────

    #[tokio::test]
    async fn test_gateway_registers_and_deregisters_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("flight.jsonl")).expect("recorder"),
        );
        let cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        let (gw, addr) = CredentialGateway::start(&cfg, recorder).await
            .expect("gateway must start");
        assert!(addr.port() > 0, "gateway must bind on non-zero port");
        gw.register_token("tok".to_string(), "agent-x".to_string(), vec!["google".to_string()]).await;
        gw.deregister_token("tok").await;
    }
}
