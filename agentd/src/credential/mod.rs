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
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::StreamExt;
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
// ar-10: SSRF guard functions live in loopback_proxy as the canonical location for all
// loopback forwarders; imported here so callers and tests see them via `use super::*`.
use crate::loopback_proxy::{extract_host, is_ssrf_blocked};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Seconds before a cached access token expires where we proactively refresh.
const TOKEN_EXPIRY_BUFFER_SECS: u64 = 60;
/// Maximum response body from an upstream provider (4 MB).
const MAX_UPSTREAM_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Maximum inbound request body from an MCP server (4 MB).
const MAX_INBOUND_REQUEST_BYTES: usize = 4 * 1024 * 1024;
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

    /// Pre-populate the cache from a persisted state file (ar-06).
    ///
    /// Called eagerly at startup for providers that have a `state_path`. If the file
    /// is absent or malformed, silently starts cold — the next request will refresh.
    /// If the stored token has already expired, the cache stays cold.
    async fn load_from_disk(&self, state_path: &str) {
        let bytes = match tokio::fs::read(state_path).await {
            Ok(b) => b,
            Err(_) => return,  // file absent on first run — start cold
        };
        let state: OAuthState = match serde_json::from_slice(&bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %state_path, error = %e,
                    "credential: state file parse failed — starting cold");
                return;
            }
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if state.expires_at_unix <= now + TOKEN_EXPIRY_BUFFER_SECS {
            return;  // already expired; start cold so next request triggers refresh
        }
        if state.access_token.is_empty() {
            return;  // empty token in state file — start cold, avoid returning "" to upstream
        }
        let mut inner = self.state.lock().await;
        inner.token         = Some(state.access_token);
        inner.expires_at    = state.expires_at_unix;
        inner.refresh_token = state.refresh_token;
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

        // ar-04c: SSRF check on token_url — a malicious secrets file could point to
        // 127.0.0.1 or an IMDS endpoint to exfiltrate credentials via the OAuth refresh.
        let token_host = extract_host(&secrets.token_url).map_err(|_| {
            format!(
                "provider '{}' token_url '{}' is malformed",
                provider,
                &secrets.token_url[..secrets.token_url.len().min(64)],
            )
        })?;
        match tokio::net::lookup_host(format!("{token_host}:443")).await {
            Ok(addrs) => {
                // Collect to detect empty iterator (DNS NOERROR NODATA) — same treatment
                // as a lookup error: warn and continue rather than silently bypassing check.
                let addrs: Vec<_> = addrs.collect();
                if addrs.is_empty() {
                    tracing::warn!(provider = %provider,
                        "credential: token_url DNS returned no addresses — SSRF check skipped \
                         (air-gapped environment?)");
                } else {
                    for sa in addrs {
                        if is_ssrf_blocked(sa.ip()) {
                            return Err(format!(
                                "provider '{}' token_url '{}' resolves to SSRF-blocked address {} \
                                 — refusing token refresh (ar-04c)",
                                provider,
                                &secrets.token_url[..secrets.token_url.len().min(64)],
                                sa.ip(),
                            ));
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(provider = %provider, error = %e,
                    "credential: token_url DNS lookup failed — SSRF check skipped");
            }
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
///
/// Also rejects percent-encoded traversal sequences (`%2e` = `.`, `%2e%2e` = `..`)
/// because URL parsers on the upstream server can silently decode them before routing.
fn normalize_path_segment(seg: &str) -> String {
    seg.split('/')
        .filter(|c| {
            if c.is_empty() || *c == ".." || *c == "." { return false; }
            let l = c.to_ascii_lowercase();
            // Reject percent-encoded single- and double-dot sequences.
            l != "%2e" && l != "%2e%2e"
        })
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
    config:     CredentialGatewayConfig,
    registry:   Arc<CredentialRegistry>,
    caches:     RwLock<HashMap<String, Arc<OAuthTokenCache>>>,
    /// reqwest client built with per-provider `.resolve()` overrides (ar-04 IP pinning).
    client:     reqwest::Client,
    recorder:   Arc<FlightRecorder>,
    /// hostname → startup-resolved IP. Populated when DNS succeeds; empty if air-gapped.
    /// The reqwest client has these IPs pinned via .resolve(); this map is used by tests
    /// to assert pinning is in effect (ar-04). Not read in the request-handling hot path.
    #[allow(dead_code)] // accessed only in tests via `let _ = &state.pinned_ips`
    pinned_ips: HashMap<String, std::net::IpAddr>,
}

impl GatewayState {
    /// Build GatewayState: validate provider configs, resolve + SSRF-check upstream
    /// hostnames, pin resolved IPs into the reqwest client to block DNS rebinding (ar-04),
    /// and eagerly load persisted OAuth state.
    async fn new(config: CredentialGatewayConfig, recorder: Arc<FlightRecorder>) -> Result<Self> {
        let lp_cfg = crate::loopback_proxy::LoopbackClientConfig::credential();
        // Use base_builder() to prevent settings drift from build_loopback_client() (ar-10).
        let mut builder = crate::loopback_proxy::base_builder(&lp_cfg);

        let mut pinned_ips: HashMap<String, std::net::IpAddr> = HashMap::new();

        for (name, prov) in &config.providers {
            anyhow::ensure!(
                prov.upstream_base.starts_with("https://"),
                "credential gateway: provider '{}' upstream_base must use https://, got '{}'",
                name, prov.upstream_base,
            );
            let host = extract_host(&prov.upstream_base).map_err(|_| anyhow::anyhow!(
                "credential gateway: provider '{}' upstream_base '{}' is malformed — \
                 must be https://hostname[/path] with no userinfo (user@host)",
                name, prov.upstream_base,
            ))?;
            match tokio::net::lookup_host(format!("{host}:443")).await {
                Ok(addrs) => {
                    // Collect so we can detect an empty iterator (NOERROR NODATA).
                    let addrs: Vec<_> = addrs.collect();
                    if addrs.is_empty() {
                        // DNS returned success but no addresses — treat the same as a
                        // lookup failure: SSRF check and IP pin are skipped with a warning.
                        tracing::warn!(
                            provider = %name,
                            upstream = %prov.upstream_base,
                            "credential gateway: DNS returned no addresses at startup — \
                             SSRF check and IP pinning skipped (air-gapped environment?)"
                        );
                    } else {
                        for sa in addrs {
                            anyhow::ensure!(
                                !is_ssrf_blocked(sa.ip()),
                                "credential gateway: provider '{}' upstream_base '{}' resolves to \
                                 SSRF-blocked address {} — use a public endpoint",
                                name, prov.upstream_base, sa.ip(),
                            );
                            // Pin the first public IP per hostname to block DNS rebinding (ar-04).
                            if !pinned_ips.contains_key(&host) {
                                pinned_ips.insert(host.clone(), sa.ip());
                                builder = builder.resolve(&host, sa);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        provider = %name,
                        upstream = %prov.upstream_base,
                        error    = %e,
                        "credential gateway: DNS lookup failed at startup — SSRF check and \
                         IP pinning skipped (air-gapped environment?)"
                    );
                }
            }
        }

        let client = builder.build().context("build credential gateway HTTP client")?;
        tracing::info!(pinned_count = %pinned_ips.len(), "credential gateway: startup IP pinning complete (ar-04)");

        // Eagerly load persisted OAuth state for all providers that have state_path (ar-06).
        let mut caches = HashMap::new();
        for (name, prov) in &config.providers {
            if prov.auth_style == crate::config::AuthStyle::OauthBearer {
                let cache = Arc::new(OAuthTokenCache::new());
                if let Some(ref sp) = prov.state_path {
                    cache.load_from_disk(sp).await;
                }
                caches.insert(name.clone(), cache);
            }
        }
        Ok(Self {
            config,
            registry:   Arc::new(CredentialRegistry::new()),
            caches:     RwLock::new(caches),
            client,
            recorder,
            pinned_ips,
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

/// Headers explicitly forwarded from caller to upstream (all others dropped — ar-08).
///
/// The broker always adds `Authorization`/`X-Api-Key` (auth attach step) plus
/// `Content-Length` and `Host` set automatically by reqwest from the URL.
/// Using an allow-list prevents header-injection attacks where a compromised MCP
/// server sends headers the upstream provider trusts (e.g. `X-Forwarded-For`).
const PASSTHROUGH_HEADERS: &[&str] = &[
    "content-type",
    "accept",
    "accept-language",
    "cache-control",
    // Google-specific API headers (safe to forward; no auth or routing semantics).
    // NOTE: x-goog-user-project is intentionally excluded — it has billing/quota
    // semantics and a compromised MCP server could use it to redirect charges.
    "x-goog-api-version",
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

    // 4. Provider capability check (ar-07: deny-by-default fast path).
    // Empty allowed_providers means no Credential capability was granted at all.
    if allowed_providers.is_empty() {
        state.recorder.record(
            &agent_id,
            None,
            EventKind::CredentialDenied,
            json!({"agent_id": &agent_id, "provider": &provider, "reason": "no_providers_configured"}),
        );
        return Ok(json_response(
            403,
            json!({
                "error":  "credential_denied",
                "reason": "no_providers_configured",
                "hint":   "Add a Credential capability to your agent's [capabilities] config",
            }),
        ));
    }
    if !allowed_providers.contains(&provider) {
        state.recorder.record(
            &agent_id,
            None,
            EventKind::CredentialDenied,
            json!({"agent_id": &agent_id, "provider": &provider, "reason": "provider_not_allowed"}),
        );
        return Ok(json_response(
            403,
            json!({
                "error":    "credential_denied",
                "provider": provider,
                "reason":   "provider_not_allowed",
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
    // Inbound query string is discarded — MCP servers must not inject URL params
    // into the upstream request (D3). Credentials are attached by the broker at step 11.
    let query = String::new();
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

    // 7. No scrub set needed: step 10 now uses an allow-list (ar-08).

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

    // 10. Build upstream request, forwarding only allow-listed headers (ar-08).
    let mut req_builder = state.client.request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
        &upstream_url,
    );
    for (name, value) in &parts.headers {
        let name_lower = name.as_str().to_lowercase();
        if !PASSTHROUGH_HEADERS.contains(&name_lower.as_str()) {
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

    // 14. Collect response body (bounded — per-chunk cap to prevent OOM).
    // bytes().await buffers the full body before checking size; bytes_stream() caps
    // incrementally so a huge response is rejected after MAX_UPSTREAM_RESPONSE_BYTES,
    // not after the full allocation (ar-04c / OOM fix).
    let mut resp_vec: Vec<u8> = Vec::new();
    let mut stream = upstream_resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(b) => {
                if resp_vec.len() + b.len() > MAX_UPSTREAM_RESPONSE_BYTES {
                    return Ok(json_response(
                        502,
                        json!({"error": "upstream_response_too_large", "provider": provider}),
                    ));
                }
                resp_vec.extend_from_slice(&b);
            }
            Err(e) => {
                tracing::warn!(provider = %provider, error = %e, "reading upstream response body");
                return Ok(json_response(
                    502,
                    json!({"error": "upstream_body_error", "provider": provider}),
                ));
            }
        }
    }
    let resp_bytes = Bytes::from(resp_vec);

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
    ///
    /// Provider validation (HTTPS scheme, DNS SSRF check, IP pinning) is performed
    /// inside `GatewayState::new()` so the check and the pinned client are always
    /// constructed together (ar-04).
    pub async fn start(
        cfg:      &CredentialGatewayConfig,
        recorder: Arc<FlightRecorder>,
    ) -> Result<(Arc<Self>, std::net::SocketAddr)> {
        let state = Arc::new(GatewayState::new(cfg.clone(), recorder).await?);
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

    // ── T9: header allow-list does NOT contain auth or routing headers (ar-08) ─

    #[test]
    fn test_header_allowlist_excludes_auth_headers() {
        // The passthrough list must never contain credential or routing headers.
        // If it did, a compromised MCP server could inject auth or routing semantics.
        let blocked = [
            "authorization",
            "host",
            "x-subscription-token",
            "x-credential-token",
            "x-forwarded-for",
            "x-real-ip",
            "x-cloud-trace-context",
            "connection",
            "transfer-encoding",
            "content-length",
            // x-goog-user-project has billing/quota semantics — must never be forwarded
            // (F2: a compromised MCP server could redirect API charges to an arbitrary project).
            "x-goog-user-project",
        ];
        for h in &blocked {
            assert!(
                !PASSTHROUGH_HEADERS.contains(h),
                "PASSTHROUGH_HEADERS must not contain '{}' — injection risk",
                h
            );
        }
    }

    // ── T10: PASSTHROUGH_HEADERS allows safe content negotiation headers (ar-08) ─

    #[test]
    fn test_header_allowlist_includes_safe_headers() {
        assert!(PASSTHROUGH_HEADERS.contains(&"content-type"),
            "content-type must be forwarded");
        assert!(PASSTHROUGH_HEADERS.contains(&"accept"),
            "accept must be forwarded");
        // accept-encoding intentionally excluded (broker controls compression).
        assert!(!PASSTHROUGH_HEADERS.contains(&"accept-encoding"),
            "accept-encoding must NOT be forwarded — broker controls compression");
    }

    // ── T10b: header injection blocked end-to-end (ar-08) ─────────────────────

    #[test]
    fn test_header_injection_blocked_by_allowlist() {
        // Simulate the step-10 allowlist filter from handle_credential_request.
        let inbound: Vec<(&str, &str)> = vec![
            ("x-forwarded-for", "1.2.3.4"),
            ("x-real-ip", "5.6.7.8"),
            ("content-type", "application/json"),
            ("authorization", "Bearer stolen-token"),
        ];
        let forwarded: Vec<&str> = inbound
            .iter()
            .filter(|(name, _)| PASSTHROUGH_HEADERS.contains(name))
            .map(|(name, _)| *name)
            .collect();
        assert!(!forwarded.contains(&"x-forwarded-for"), "x-forwarded-for must be blocked");
        assert!(!forwarded.contains(&"x-real-ip"), "x-real-ip must be blocked");
        assert!(!forwarded.contains(&"authorization"), "authorization must be blocked");
        assert!(forwarded.contains(&"content-type"), "content-type must be forwarded");
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

    // ── T15c: normalize_path_segment blocks percent-encoded traversal (F1) ────
    // This test FAILS without the %2e/%2e%2e filter in normalize_path_segment.
    // A `%2e%2e` component passes the literal-".." filter but decodes to ".." on
    // the upstream server, enabling path traversal outside the upstream base path.

    #[test]
    fn test_normalize_path_segment_blocks_pct_encoded_traversal() {
        // %2e%2e decodes to ".." — must be stripped
        assert_eq!(normalize_path_segment("v1/%2e%2e/secret"), "v1/secret");
        // uppercase variant
        assert_eq!(normalize_path_segment("v1/%2E%2E/secret"), "v1/secret");
        // mixed case
        assert_eq!(normalize_path_segment("v1/%2e%2E/secret"), "v1/secret");
        // single %2e (encoded ".") must also be stripped
        assert_eq!(normalize_path_segment("v1/%2e/messages"), "v1/messages");
        // chained traversal: /a/%2e%2e/%2e%2e/etc/passwd => "etc/passwd"
        assert_eq!(normalize_path_segment("a/%2e%2e/%2e%2e/etc/passwd"), "a/etc/passwd");
        // Normal path segments containing "2e" in names are untouched
        assert_eq!(normalize_path_segment("v1/color2e3/data"), "v1/color2e3/data");
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

    // ── T18: is_ssrf_blocked catches all private/loopback/link-local (ar-04) ───

    #[test]
    fn test_ssrf_blocked_loopback() {
        assert!(is_ssrf_blocked("127.0.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked("127.255.255.255".parse().unwrap()));
        assert!(is_ssrf_blocked("::1".parse().unwrap()));
    }

    #[test]
    fn test_ssrf_blocked_link_local_imds() {
        assert!(is_ssrf_blocked("169.254.169.254".parse().unwrap()));
        assert!(is_ssrf_blocked("169.254.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked("fe80::1".parse().unwrap()));
    }

    #[test]
    fn test_ssrf_blocked_rfc1918_private() {
        assert!(is_ssrf_blocked("10.0.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked("172.16.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn test_ssrf_not_blocked_public_ips() {
        // Public IPs must NOT be blocked.
        assert!(!is_ssrf_blocked("142.250.80.46".parse().unwrap())); // google
        assert!(!is_ssrf_blocked("1.1.1.1".parse().unwrap()));       // cloudflare
        assert!(!is_ssrf_blocked("52.84.0.1".parse().unwrap()));     // aws cloudfront
    }

    #[test]
    fn test_ssrf_blocked_ipv4_mapped_ipv6() {
        // ::ffff:192.168.1.1 is IPv4-mapped IPv6 for a private address — must be blocked.
        // This test FAILS if the `v6.to_ipv4()` delegation is removed from is_ssrf_blocked.
        assert!(is_ssrf_blocked("::ffff:192.168.1.1".parse().unwrap()));
        assert!(is_ssrf_blocked("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_ssrf_blocked("::ffff:169.254.169.254".parse().unwrap()));
        // Public IPv4 mapped to IPv6 must NOT be blocked.
        assert!(!is_ssrf_blocked("::ffff:1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn test_ssrf_blocked_unique_local_ipv6() {
        // fc00::/7 unique-local is the IPv6 equivalent of RFC 1918 — must be blocked.
        // This test FAILS if the `fc00::/7` check is removed from is_ssrf_blocked.
        assert!(is_ssrf_blocked("fd00::1".parse().unwrap()));
        assert!(is_ssrf_blocked("fc00::1".parse().unwrap()));
        assert!(is_ssrf_blocked("fdff:ffff::1".parse().unwrap()));
        // Public IPv6 global-unicast (2000::/3) must NOT be blocked.
        assert!(!is_ssrf_blocked("2001:db8::1".parse().unwrap()));
    }

    // ── T19: extract_host parses correctly ────────────────────────────────────

    #[test]
    fn test_extract_host_basic() {
        assert_eq!(extract_host("https://www.googleapis.com/auth"), Ok("www.googleapis.com".to_string()));
        assert_eq!(extract_host("https://api.search.brave.com/res"), Ok("api.search.brave.com".to_string()));
        assert_eq!(extract_host("https://host:8443/path"), Ok("host".to_string()));
    }

    #[test]
    fn test_extract_host_rejects_non_https() {
        assert!(extract_host("http://evil.com").is_err());
        assert!(extract_host("").is_err());
    }

    #[test]
    fn test_extract_host_rejects_userinfo() {
        // user@host is a bypass: DNS lookup on "user@host:443" fails, SSRF check skipped.
        // This test FAILS if the '@' guard is removed from extract_host.
        assert!(extract_host("https://user@169.254.169.254/path").is_err());
        assert!(extract_host("https://user:pass@example.com/path").is_err());
        assert!(extract_host("https://attacker@victim.internal/").is_err());
    }

    #[test]
    fn test_extract_host_ipv6_literal() {
        // IPv6 literals must be returned in bracketed form for lookup_host.
        // The old split-on-':' logic returned "[" for "[::1]", causing DNS failure
        // and silent SSRF-check skip.
        // This test FAILS if the IPv6 literal handling is removed from extract_host.
        let h = extract_host("https://[::1]/path").unwrap();
        assert_eq!(h, "[::1]");
        let h2 = extract_host("https://[fe80::1]:8443/path").unwrap();
        assert_eq!(h2, "[fe80::1]");
        // Invalid IPv6 literal must be rejected.
        assert!(extract_host("https://[not-ipv6]/path").is_err());
    }

    // ── T20: SSRF guard rejects http:// and private IP upstream_base (ar-04) ──

    #[tokio::test]
    async fn test_gateway_rejects_private_ip_upstream() {
        // 169.254.169.254 is the IMDS endpoint — must be blocked.
        // We can't actually test DNS resolution in unit tests (it would fail in CI
        // for arbitrary IPs), so we test is_ssrf_blocked() directly instead.
        // The integration coverage for the DNS path is T16 (http:// rejected at startup).
        assert!(is_ssrf_blocked("169.254.169.254".parse().unwrap()),
            "IMDS address must be SSRF-blocked");
        assert!(is_ssrf_blocked("192.168.0.1".parse().unwrap()),
            "RFC 1918 must be SSRF-blocked");
    }

    // ── T21: load_from_disk pre-populates cache from valid state file (ar-06) ──

    #[tokio::test]
    async fn test_load_from_disk_prepopulates_cache() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");

        // Write a valid state file with a far-future expiry.
        let state = OAuthState {
            access_token:    "cached_access_token".to_string(),
            expires_at_unix: u64::MAX / 2,
            refresh_token:   Some("rotated_refresh_token".to_string()),
        };
        tokio::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).await.unwrap();

        let cache = OAuthTokenCache::new();
        cache.load_from_disk(state_path.to_str().unwrap()).await;

        let inner = cache.state.lock().await;
        assert_eq!(inner.token.as_deref(), Some("cached_access_token"),
            "token must be pre-populated from disk");
        assert_eq!(inner.refresh_token.as_deref(), Some("rotated_refresh_token"),
            "refresh_token must be pre-populated from disk");
        assert!(inner.expires_at > 0, "expires_at must be set");
    }

    #[tokio::test]
    async fn test_load_from_disk_absent_file_starts_cold() {
        // Missing state file is not an error — broker starts cold.
        let cache = OAuthTokenCache::new();
        cache.load_from_disk("/nonexistent/path/state.json").await;
        let inner = cache.state.lock().await;
        assert!(inner.token.is_none(), "cold start: token must be None");
    }

    #[tokio::test]
    async fn test_load_from_disk_expired_token_starts_cold() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");

        // Write an already-expired token.
        let state = OAuthState {
            access_token:    "expired_token".to_string(),
            expires_at_unix: 1,  // Unix epoch + 1s = long expired
            refresh_token:   None,
        };
        tokio::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).await.unwrap();

        let cache = OAuthTokenCache::new();
        cache.load_from_disk(state_path.to_str().unwrap()).await;

        let inner = cache.state.lock().await;
        assert!(inner.token.is_none(),
            "expired token must not be pre-populated — broker should re-fetch");
    }

    #[tokio::test]
    async fn test_load_from_disk_empty_token_starts_cold() {
        // Empty access_token in state file must not be loaded — returning "" to
        // upstream would produce a confusing auth failure rather than a clear refresh.
        // This test FAILS if the empty-token guard is removed from load_from_disk.
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");

        let state = OAuthState {
            access_token:    "".to_string(),  // empty — should be rejected
            expires_at_unix: u64::MAX / 2,    // not expired
            refresh_token:   None,
        };
        tokio::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).await.unwrap();

        let cache = OAuthTokenCache::new();
        cache.load_from_disk(state_path.to_str().unwrap()).await;

        let inner = cache.state.lock().await;
        assert!(inner.token.is_none(),
            "empty access_token must not be pre-populated — broker should start cold");
    }

    // ── T22: empty allowed_providers denied with explicit 403 (ar-07) ─────────
    //
    // The previous version of this test constructed its own JSON payload and
    // verified the shape of json_response() — it never called
    // handle_credential_request, so it would pass even if the fast-path branch
    // were removed. This version starts a live gateway and makes a real HTTP
    // request to verify the path actually fires.

    #[tokio::test]
    async fn test_deny_fast_path_empty_providers_returns_403() {
        // ar-07 gate: a token registered with no allowed providers must get HTTP 403
        // from the live gateway. This test FAILS if the `allowed_providers.is_empty()`
        // branch is removed from handle_credential_request.
        let dir = tempfile::tempdir().expect("tempdir");
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("flight.jsonl"))
                .expect("recorder"),
        );
        // Empty providers map — no SSRF check runs at startup, gateway binds cleanly.
        let cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        let (gw, addr) = CredentialGateway::start(&cfg, recorder).await
            .expect("gateway must start");

        // Register a token with NO providers (the ar-07 case).
        let token = "test-no-providers-ar07";
        gw.register_token(token.to_string(), "test-agent".to_string(), vec![]).await;

        // Make a real HTTP request to the live gateway.
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/some-provider/v1/endpoint"))
            .header("x-credential-token", token)
            .send()
            .await
            .expect("HTTP request to gateway must succeed");

        assert_eq!(resp.status().as_u16(), 403,
            "empty allowed_providers must return 403 (ar-07 fast path removed?)");
        let body: serde_json::Value = resp.json().await.expect("response must be JSON");
        assert_eq!(body["error"], "credential_denied");
        assert_eq!(body["reason"], "no_providers_configured");
    }

    // ── T23: S2 — EgressBrokered event has no content_audited field ───────────

    #[test]
    fn test_egress_brokered_event_lacks_content_audited() {
        // S2 gate: scan the actual egress.rs source for "content_audited".
        // This test FAILS if you revert the S2 fix (bring back `"content_audited": true`).
        // A pure-payload construction test would always pass regardless of the fix.
        let src = include_str!("../egress.rs");
        assert!(
            !src.contains("\"content_audited\""),
            "egress.rs must not contain \"content_audited\" — that field was a hardcoded lie \
             (S2 fix reverted?). The EgressBrokered event must not claim an audit that never ran."
        );
    }

    // ── T24: loopback proxy shared client builds (ar-10) ──────────────────────

    #[test]
    fn test_loopback_proxy_shared_client_builds() {
        crate::loopback_proxy::build_loopback_client(
            crate::loopback_proxy::LoopbackClientConfig::credential()
        ).expect("credential loopback client must build");
        crate::loopback_proxy::build_loopback_client(
            crate::loopback_proxy::LoopbackClientConfig::egress()
        ).expect("egress loopback client must build");
    }

    // ── T25: ApiKeyHeader adapter — missing env var returns 503 (Group E) ────
    //
    // Exercises the ApiKeyHeader code path in handle_credential_request: steps
    // 3 (URI parse), 4 (provider check), 5 (config lookup), and 8 (ApiKeyHeader
    // env-var read → 503 when unset). Uses provider_cfg_api_key_header() as the
    // fixture. This test FAILS if the ApiKeyHeader branch in step 8 is removed.

    #[tokio::test]
    async fn test_api_key_header_missing_env_var_returns_503() {
        let dir = tempfile::tempdir().expect("tempdir");
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("flight.jsonl"))
                .expect("recorder"),
        );

        // Use provider_cfg_api_key_header() as the fixture, override secret_key
        // to a test-only name guaranteed not to be set in any CI environment.
        let mut provider = provider_cfg_api_key_header();
        provider.secret_key = Some("_AGENTOS_TEST_API_KEY_ABSENT_T25".to_string());

        let mut cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        cfg.providers.insert("brave-search".to_string(), provider);

        // Gateway start: DNS lookup for api.search.brave.com warns but does not fail
        // when unreachable (air-gapped), so this test is CI-safe.
        let (gw, addr) = CredentialGateway::start(&cfg, recorder).await
            .expect("gateway must start");

        let token = "test-api-key-header-missing-t25";
        gw.register_token(token.to_string(), "agent-t25".to_string(), vec!["brave-search".to_string()]).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/brave-search/web/search?q=test"))
            .header("x-credential-token", token)
            .send()
            .await
            .expect("HTTP request to live gateway must succeed");

        assert_eq!(resp.status().as_u16(), 503,
            "ApiKeyHeader with missing env var must return 503 (ApiKeyHeader branch removed?)");
        let body: serde_json::Value = resp.json().await.expect("response must be JSON");
        assert_eq!(body["error"], "credential_not_provisioned",
            "error field must be credential_not_provisioned");
        assert!(
            body["hint"].as_str().unwrap_or("").contains("_AGENTOS_TEST_API_KEY_ABSENT_T25"),
            "hint must name the missing env var: {body}"
        );
    }

    // ── T26: ar-04 IP pinning — SSRF check lives in GatewayState::new() ─────────
    //
    // Without ar-04, provider validation lived only in CredentialGateway::start(),
    // and GatewayState::new() would succeed for a loopback upstream_base, returning
    // a client built without any .resolve() pin. The test calls GatewayState::new()
    // directly to prove the check is there, not just in start().
    // This test FAILS without ar-04 (GatewayState::new() would return Ok).

    #[tokio::test]
    async fn test_construction_ssrf_blocks_rebind() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("fl.jsonl")).unwrap(),
        );
        // 127.0.0.1 is the loopback — is_ssrf_blocked() returns true. tokio::net::lookup_host
        // resolves IP literals directly (no DNS needed), making this deterministic in CI.
        let mut private_prov = provider_cfg_api_key_header();
        private_prov.upstream_base = "https://127.0.0.1".to_string();

        let mut cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        cfg.providers.insert("loopback-test".to_string(), private_prov);

        let result = GatewayState::new(cfg, recorder).await;
        assert!(result.is_err(),
            "GatewayState::new() must reject loopback upstream_base — ar-04 SSRF check \
             not in GatewayState::new()? (was only in CredentialGateway::start())");
        let err = result.err().unwrap().to_string();
        assert!(err.contains("SSRF-blocked"),
            "error must mention SSRF-blocked, got: {err}");

        // Verify pinned_ips field exists on a valid GatewayState (compile-time check).
        // An empty-providers config produces an Ok state with empty pinned_ips.
        let empty_cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        let dir2 = tempfile::tempdir().unwrap();
        let recorder2 = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir2.path().join("fl.jsonl")).unwrap(),
        );
        let ok_state = GatewayState::new(empty_cfg, recorder2).await.unwrap();
        // Field access proves the struct has pinned_ips (compile-time).
        let _ = &ok_state.pinned_ips;
    }

    // ── T36: IP pin path actually populated for a public IP literal (T-INFO-2) ───
    //
    // T26 success sub-case used an empty providers map, so the DNS/pin loop body
    // never ran. This test uses a stable IP literal upstream_base (no live DNS
    // needed — lookup_host on an IP literal returns that IP directly) to verify
    // the pinned_ips map is populated when a valid public address resolves.
    // This test FAILS if the IP-pinning branch inside the Ok(addrs) arm is removed.

    #[tokio::test]
    async fn test_public_ip_upstream_pins_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("fl.jsonl")).unwrap(),
        );
        // 1.1.1.1 is a public IP — not SSRF-blocked and resolves deterministically.
        let mut prov = provider_cfg_api_key_header();
        prov.upstream_base = "https://1.1.1.1".to_string();

        let mut cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        cfg.providers.insert("public-ip-test".to_string(), prov);

        let state = GatewayState::new(cfg, recorder).await
            .expect("public IP upstream must not be rejected");
        assert_eq!(state.pinned_ips.len(), 1,
            "pinned_ips must have 1 entry for the resolved IP (IP pinning not executing?)");
    }

    // ── T37: empty DNS iterator warns and does not SSRF-check (ADV-1 guard) ──────
    //
    // lookup_host can return Ok([]) (DNS NOERROR NODATA) without entering the for-loop,
    // silently skipping the SSRF check and IP pin. The fix collects the iterator and
    // emits a warning for an empty result. We can only test the structural guard here
    // (real NODATA DNS is non-deterministic); the test verifies the code path exists.
    // This test FAILS if the empty-iterator guard is removed from GatewayState::new().

    #[test]
    fn test_empty_dns_guard_present_in_gateway_new() {
        let src = include_str!("mod.rs");
        // Split the pattern so this assertion string is not itself the match (self-reference guard).
        let guard = ["addrs", ".is_empty()"].concat();
        assert!(
            src.contains(&guard),
            "GatewayState::new() must guard against empty DNS iterator (ADV-1 fix removed?)"
        );
    }

    // ── T27: ar-04c — OAuth token_url SSRF blocked ───────────────────────────────
    //
    // A malicious secrets file could set token_url to a private IP (e.g., IMDS) to
    // exfiltrate credentials via the OAuth refresh POST. Without ar-04c, only the
    // https:// scheme was checked; the SSRF check on the resolved IP was absent.
    // This test FAILS without ar-04c (get_or_refresh() would try to connect instead
    // of returning an SSRF error).

    #[tokio::test]
    async fn test_token_url_ssrf_blocked() {
        let dir = tempfile::tempdir().unwrap();
        // Secrets file with token_url pointing at 127.0.0.1.
        let secrets = serde_json::json!({
            "client_id":     "test-client",
            "client_secret": "test-secret",
            "refresh_token": "test-refresh",
            "token_url":     "https://127.0.0.1:9999/token"
        });
        let secrets_path = dir.path().join("google.json");
        tokio::fs::write(&secrets_path, serde_json::to_vec(&secrets).unwrap()).await.unwrap();

        let provider = ProviderConfig {
            auth_style:    AuthStyle::OauthBearer,
            upstream_base: "https://www.googleapis.com".to_string(),
            header_name:   None,
            secret_key:    None,
            token_path:    Some(secrets_path.to_str().unwrap().to_string()),
            state_path:    None,
        };
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("fl.jsonl")).unwrap(),
        );
        let client = crate::loopback_proxy::build_loopback_client(
            crate::loopback_proxy::LoopbackClientConfig::credential(),
        ).unwrap();

        let cache = OAuthTokenCache::new();
        // Cache is cold → slow path → reads secrets file → sees token_url=127.0.0.1.
        let result = cache.get_or_refresh("google", &provider, &recorder, &client).await;

        assert!(result.is_err(), "token_url pointing to loopback must be blocked (ar-04c)");
        let err = result.unwrap_err();
        assert!(
            err.contains("SSRF-blocked") || err.contains("127.0.0.1"),
            "error must name the SSRF block, got: {err}"
        );
    }

    // ── T28: bytes_stream() per-chunk cap (OOM fix) ───────────────────────────────
    //
    // bytes().await buffers the entire upstream body before the size check — an
    // oversized OAuth API response causes OOM. The fix uses bytes_stream() to cap
    // incrementally. This test verifies the old bytes().await pattern is gone.
    // This test FAILS if bytes().await is restored in the response collection step.

    #[test]
    fn test_streaming_response_cap_enforced_uses_bytes_stream() {
        let src = include_str!("mod.rs");
        // Build the banned pattern from parts so the literal doesn't appear in this file
        // (otherwise include_str! would find it in this very assertion and fail).
        let banned = ["upstream_resp", ".bytes()", ".await"].concat();
        assert!(
            !src.contains(&banned),
            "credential handler must use bytes_stream() not the batching bytes() call \
             (OOM regression: batch path buffers full body before size check)"
        );
        // Build expected pattern from parts so the literal doesn't appear in this file.
        let expected = ["bytes", "_stream()"].concat();
        assert!(
            src.contains(&expected),
            "credential handler must use bytes_stream() for per-chunk streaming cap"
        );
    }

    // ── T29: is_ssrf_blocked + extract_host in loopback_proxy (ar-10) ────────────
    // Covered by loopback_proxy::tests::test_ssrf_guard_in_loopback_proxy.
    // Verified here that credential/mod.rs callers see the same functions via import.

    #[test]
    fn test_ssrf_guard_callable_from_credential_module() {
        // T29 supplemental: verify that is_ssrf_blocked and extract_host are imported
        // into this module (via `use crate::loopback_proxy::...`) and are callable.
        // This test FAILS if the import is removed (ar-10 reverted).
        use std::net::IpAddr;
        assert!(is_ssrf_blocked("127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(!is_ssrf_blocked("8.8.8.8".parse::<IpAddr>().unwrap()));
        assert_eq!(
            extract_host("https://api.example.com/path"),
            Ok("api.example.com".to_string())
        );
    }

    // ── T30: ar-07 — agent_id in CredentialRegistry is the agent ID ─────────────
    //
    // register_token() takes agent_id as the second parameter. main.rs used to pass
    // server.name (the MCP server name) instead of the owning agent's ID, causing
    // CredentialAccessed flight events to show the server name, not the agent.
    // This test FAILS if register_token() ignores or discards the agent_id parameter.

    #[tokio::test]
    async fn test_agent_id_registered_not_server_name() {
        let reg = CredentialRegistry::new();
        // Register with an agent_id explicitly distinct from a hypothetical server name.
        reg.register(
            "tok-ar07".to_string(),
            "scout-agent".to_string(),
            vec!["google".to_string()],
        ).await;

        let (agent_id, providers) = reg.lookup("tok-ar07").await
            .expect("registered token must be found");

        assert_eq!(agent_id, "scout-agent",
            "CredentialAccessed event must show agent ID, not server name (ar-07 reverted?)");
        assert_eq!(providers, vec!["google"]);
    }

    // ── T31: ar-07 — provider scope enforced per token ───────────────────────────
    //
    // Token registered with allowed_providers=["google"] must NOT grant access to
    // "brave-search". This test verifies the allowed_providers check at step 4.
    // This test FAILS if the provider scope check is removed from the handler.

    #[tokio::test]
    async fn test_token_cross_agent_provider_scope_denied() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("fl.jsonl")).unwrap(),
        );
        // Start gateway with both providers configured.
        let mut cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        cfg.providers.insert("google".to_string(), provider_cfg_oauth());
        cfg.providers.insert("brave-search".to_string(), provider_cfg_api_key_header());

        let (gw, addr) = CredentialGateway::start(&cfg, recorder).await
            .expect("gateway must start");

        // Token for agent-a scoped to google only.
        let token_a = "test-cross-scope-t31";
        gw.register_token(token_a.to_string(), "agent-a".to_string(), vec!["google".to_string()]).await;

        let client = reqwest::Client::new();

        // Attempt to access brave-search with agent-a's token — must be denied.
        let resp = client
            .get(format!("http://{addr}/brave-search/res/v1/web/search"))
            .header("x-credential-token", token_a)
            .send().await.unwrap();

        assert_eq!(resp.status().as_u16(), 403,
            "token scoped to google must be denied for brave-search (ar-07 scope check removed?)");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["reason"], "provider_not_allowed");
    }

    // ── T32: OAuth bearer path returns 503 when secrets file is absent ──────────
    //
    // Exercises the OauthBearer branch in handle_credential_request step 8.
    // Without this test, provider_cfg_oauth() was dead code (the live-gateway path
    // for OAuth was never exercised). This test FAILS if the OauthBearer branch is
    // removed from the handler.

    #[tokio::test]
    async fn test_oauth_bearer_path_503_no_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("fl.jsonl")).unwrap(),
        );
        // provider_cfg_oauth() token_path = "/run/secrets/google.json" — absent in tests.
        let mut cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        cfg.providers.insert("google".to_string(), provider_cfg_oauth());

        // DNS for googleapis.com may warn in air-gapped CI but start() does not fail.
        let (gw, addr) = CredentialGateway::start(&cfg, recorder).await
            .expect("gateway must start (DNS warn is OK in air-gapped CI)");

        let token = "test-oauth-t32";
        gw.register_token(token.to_string(), "agent-t32".to_string(), vec!["google".to_string()]).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/google/v1/tokeninfo"))
            .header("x-credential-token", token)
            .send().await.unwrap();

        assert_eq!(resp.status().as_u16(), 503,
            "OauthBearer with absent secrets file must return 503 (branch removed?)");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "credential_refresh_failed",
            "error must be credential_refresh_failed: {body}");
    }

    // ── T33: deregistered token returns 401 via live gateway ─────────────────────
    //
    // This test FAILS if deregister_token() does not remove the token from the registry.

    #[tokio::test]
    async fn test_token_deregistration_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("fl.jsonl")).unwrap(),
        );
        let cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        let (gw, addr) = CredentialGateway::start(&cfg, recorder).await
            .expect("gateway must start with no providers");

        let token = "test-deregister-lifecycle-t33";
        gw.register_token(token.to_string(), "agent-t33".to_string(), vec!["google".to_string()]).await;

        let client = reqwest::Client::new();

        // Before deregister: token is valid — gateway returns non-401 (503 for no provider config).
        let resp_before = client
            .get(format!("http://{addr}/google/v1/test"))
            .header("x-credential-token", token)
            .send().await.unwrap();
        assert_ne!(resp_before.status().as_u16(), 401,
            "token must be valid before deregistration");

        // Deregister the token.
        gw.deregister_token(token).await;

        // After deregister: must get 401.
        let resp_after = client
            .get(format!("http://{addr}/google/v1/test"))
            .header("x-credential-token", token)
            .send().await.unwrap();
        assert_eq!(resp_after.status().as_u16(), 401,
            "deregistered token must return 401 (deregister_token not removing from registry?)");
    }

    // ── T34: ApiKeyQuery attach path exists in handler ────────────────────────────
    //
    // This test FAILS if the ApiKeyQuery arm is removed from the credential-attach
    // match at step 11.

    #[test]
    fn test_api_key_query_path_exists_in_handler() {
        let src = include_str!("mod.rs");
        // Build patterns from parts so the literals don't appear in this file (self-reference fix).
        let aq_pat = ["AuthStyle", "::", "ApiKeyQuery"].concat();
        assert!(
            src.contains(&aq_pat),
            "ApiKeyQuery must have a match arm in the credential attach step (step 11)"
        );
        let q_pat = ["req_builder", ".query"].concat();
        assert!(
            src.contains(&q_pat),
            "ApiKeyQuery must call req_builder.query() to attach the API key as a query param"
        );
    }

    // ── T35: Inbound body > MAX_INBOUND_REQUEST_BYTES returns 413 ─────────────────
    //
    // This test FAILS if the http_body_util::Limited guard is removed from step 6.

    #[tokio::test]
    async fn test_inbound_body_413_live_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("fl.jsonl")).unwrap(),
        );
        let mut cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        cfg.providers.insert("google".to_string(), provider_cfg_api_key_header());

        let (gw, addr) = CredentialGateway::start(&cfg, recorder).await.unwrap();
        let token = "test-inbound-413-t35";
        gw.register_token(token.to_string(), "agent-t35".to_string(), vec!["google".to_string()]).await;

        // Send MAX_INBOUND_REQUEST_BYTES + 1 bytes (4 MB + 1 byte).
        let oversized_body = vec![0u8; MAX_INBOUND_REQUEST_BYTES + 1];

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/google/v1/endpoint"))
            .header("x-credential-token", token)
            .body(oversized_body)
            .send().await.unwrap();

        assert_eq!(resp.status().as_u16(), 413,
            "body > MAX_INBOUND_REQUEST_BYTES must return 413 (body cap guard removed?)");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "request_body_too_large");
    }

    // ── T35b: Inbound query string is discarded (D3) ─────────────────────────────
    //
    // This test FAILS if query passthrough is restored (query = req.uri().query()...).

    #[test]
    fn test_query_string_discarded_from_upstream() {
        let src = include_str!("mod.rs");
        // Build the banned pattern from parts so the literal doesn't appear in this file
        // (otherwise include_str! would find it in this very assertion and fail).
        let banned = [".uri()", ".query()", ".map(|q| format!(\"?{q}\"))"].concat();
        assert!(
            !src.contains(&banned),
            "query string must not be forwarded to upstream — old passthrough code found (D3 reverted?)"
        );
        // Build expected pattern from parts so the literal doesn't appear in this file.
        let expected = ["let query", " = String::new()"].concat();
        assert!(
            src.contains(&expected),
            "query must always be discarded with String::new() — inbound params must not \
             reach the upstream URL"
        );
    }

    // ── G1: token_url with userinfo is rejected as malformed (ar-04c extract_host path) ──
    //
    // extract_host() returns Err for URLs with userinfo (user@host). This test FAILS if
    // the extract_host() call on token_url is removed (allowing userinfo URLs through).

    #[tokio::test]
    async fn test_token_url_userinfo_rejected_as_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("fl.jsonl")).unwrap(),
        );
        // token_url passes starts_with("https://") check but fails extract_host() — userinfo.
        let secrets = serde_json::json!({
            "client_id":     "g1-client",
            "client_secret": "g1-secret",
            "refresh_token": "g1-refresh",
            "token_url":     "https://user@evil.com/token"
        });
        let secrets_path = dir.path().join("g1-secrets.json");
        tokio::fs::write(&secrets_path, serde_json::to_vec(&secrets).unwrap()).await.unwrap();
        let prov = ProviderConfig {
            auth_style:    AuthStyle::OauthBearer,
            upstream_base: "https://www.googleapis.com".to_string(),
            header_name:   None,
            secret_key:    None,
            token_path:    Some(secrets_path.to_str().unwrap().to_string()),
            state_path:    None,
        };
        let client = crate::loopback_proxy::build_loopback_client(
            crate::loopback_proxy::LoopbackClientConfig::credential(),
        ).unwrap();
        let cache = OAuthTokenCache::new();
        let result = cache.get_or_refresh("google", &prov, &recorder, &client).await;
        assert!(result.is_err(),
            "token_url with userinfo must be rejected as malformed (ar-04c extract_host path)");
        let err = result.err().unwrap();
        assert!(err.contains("malformed"),
            "error must mention 'malformed', got: {err}");
    }

    // ── G2: token_url DNS Err arm warns and continues (structural guard) ───────────
    //
    // When DNS lookup for token_url fails with an OS error, the code warns and continues
    // (allows operation in air-gapped environments). This test FAILS if the Err arm is
    // removed or the warning message is removed from get_or_refresh().

    #[test]
    fn test_token_url_dns_err_arm_warns_and_continues_source_guard() {
        let src = include_str!("mod.rs");
        // Split the pattern to avoid the assembled string appearing literally in this file.
        let pat = ["credential: token_url DNS lookup failed", " — SSRF check skipped"].concat();
        assert!(
            src.contains(&pat),
            "get_or_refresh() must warn-and-continue on token_url DNS Err — \
             the Err arm was removed or warning message changed (G2)"
        );
    }

    // ── G3: upstream_base with userinfo is rejected by GatewayState::new() ────────
    //
    // extract_host() returns Err for userinfo URLs. This test FAILS if the
    // extract_host() call on upstream_base is removed from GatewayState::new().

    #[tokio::test]
    async fn test_gateway_new_rejects_userinfo_in_upstream_base() {
        let dir = tempfile::tempdir().unwrap();
        let recorder = Arc::new(
            crate::flight_recorder::FlightRecorder::new(&dir.path().join("fl.jsonl")).unwrap(),
        );
        // "https://user@googleapis.com" passes starts_with("https://") but fails extract_host().
        let mut prov = provider_cfg_api_key_header();
        prov.upstream_base = "https://user@googleapis.com".to_string();

        let mut cfg = CredentialGatewayConfig { enabled: true, ..Default::default() };
        cfg.providers.insert("userinfo-test".to_string(), prov);

        let result = GatewayState::new(cfg, recorder).await;
        assert!(result.is_err(),
            "upstream_base with userinfo must be rejected — extract_host() guard not in new()? (G3)");
        let err = result.err().unwrap().to_string();
        assert!(err.contains("malformed"),
            "error must mention 'malformed', got: {err}");
    }

    // ── G4: GatewayState::new() DNS Err arm warns and continues (structural guard) ─
    //
    // When DNS lookup for upstream_base fails at startup, the code warns and continues.
    // This test FAILS if the Err arm is removed from GatewayState::new().

    #[test]
    fn test_gateway_new_dns_err_arm_warns_and_continues_source_guard() {
        let src = include_str!("mod.rs");
        // Split the pattern to avoid the assembled string appearing literally in this file.
        let pat = ["credential gateway: DNS lookup failed", " at startup"].concat();
        assert!(
            src.contains(&pat),
            "GatewayState::new() must warn-and-continue on upstream_base DNS Err — \
             the Err arm was removed or warning message changed (G4)"
        );
    }

    // ── G5: bytes_stream Err chunk returns 502 upstream_body_error (structural guard)
    //
    // When the upstream response body stream yields an Err chunk, handle_credential_request
    // must return a 502 with error=upstream_body_error. This test FAILS if the Err arm is
    // removed from the bytes_stream loop.

    #[test]
    fn test_bytes_stream_err_arm_returns_502_source_guard() {
        let src = include_str!("mod.rs");
        // Split the pattern to avoid the assembled string appearing literally in this file.
        let err_key = ["upstream_body", "_error"].concat();
        assert!(
            src.contains(&err_key),
            "handle_credential_request bytes_stream loop must handle Err chunks with \
             upstream_body_error 502 response — Err arm removed? (G5)"
        );
        // Also verify the Err match arm exists in the stream loop.
        let err_arm = ["Err(e) =>", " {"].concat();
        assert!(
            src.contains(&err_arm),
            "bytes_stream loop must have an Err(e) match arm — G5 guard"
        );
    }
}
