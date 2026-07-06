//! Shared loopback-proxy HTTP client builder + SSRF guards (ar-10 / cred.3.1).
//!
//! Both `EgressProxy` and `CredentialGateway` use a loopback-bound HTTP server that
//! forwards requests to upstream APIs. The security guards (redirect policy, connect
//! timeout, TLS backend, SSRF blocking) must be identical and centrally enforced —
//! divergence between two separate `reqwest::Client::builder()` calls has caused real bugs.
//!
//! `is_ssrf_blocked()` and `extract_host()` live here as the canonical SSRF guard for
//! all loopback forwarders. Both proxies import them from this crate; neither holds a
//! private copy (ar-10).

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result};

/// Parameters for building a loopback forwarding HTTP client.
pub struct LoopbackClientConfig {
    pub connect_timeout_secs: u64,
    pub total_timeout_secs:   u64,
}

impl LoopbackClientConfig {
    /// Config for the egress (inference) proxy: long total timeout for streaming.
    pub fn egress() -> Self {
        Self { connect_timeout_secs: 10, total_timeout_secs: 120 }
    }

    /// Config for the credential gateway: shorter total timeout for OAuth calls.
    pub fn credential() -> Self {
        Self { connect_timeout_secs: 10, total_timeout_secs: 60 }
    }
}

/// Return true if the IP is private, loopback, or link-local (SSRF-blocked).
///
/// Mirrors the logic in docker/oauth_mcp.py:_is_ssrf_blocked(). Covers all ranges
/// that a compromised MCP server could exploit: loopback, RFC 1918, link-local (IMDS),
/// IPv4-mapped IPv6, and fc00::/7 unique-local. Centralised here so both the egress
/// proxy and credential gateway use identical logic (ar-10).
pub(crate) fn is_ssrf_blocked(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => {
            v4.is_loopback()           // 127.0.0.0/8
            || v4.is_private()         // 10/8, 172.16/12, 192.168/16
            || v4.is_link_local()      // 169.254.0.0/16 (IMDS)
            || v4.is_broadcast()
            || v4.is_documentation()
            || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()           // ::1
            || v6.is_unspecified()     // ::
            // fe80::/10 link-local
            || (v6.segments()[0] & 0xffc0) == 0xfe80
            // fc00::/7 unique-local (IPv6 equivalent of RFC 1918)
            || (v6.segments()[0] & 0xfe00) == 0xfc00
            // ::ffff:0:0/96 IPv4-mapped — delegate to the IPv4 check
            || v6.to_ipv4().map(|v4| is_ssrf_blocked(IpAddr::V4(v4))).unwrap_or(false)
        }
    }
}

/// Extract the hostname from an `https://host/path` URL for DNS resolution.
///
/// Returns `Err(())` for any URL that is structurally malformed, contains userinfo
/// (`user@host`), or has a non-HTTPS scheme — the caller must treat `Err` as a hard
/// startup failure, not a silent skip, to prevent SSRF bypass via parse failure.
pub(crate) fn extract_host(url: &str) -> Result<String, ()> {
    let without_scheme = url.strip_prefix("https://").ok_or(())?;
    // Reject userinfo (user@host or user:pass@host) — DNS lookup on "user@host"
    // fails, causing the SSRF check to be skipped rather than enforced.
    if without_scheme.contains('@') { return Err(()); }
    // IPv6 literals: https://[::1]/path — must NOT split on ':'.
    if without_scheme.starts_with('[') {
        let close = without_scheme.find(']').ok_or(())?;
        let addr_str = &without_scheme[1..close];
        // Validate it parses as a real IPv6 address (not a bypass like "[junk]").
        addr_str.parse::<std::net::Ipv6Addr>().map_err(|_| ())?;
        // Return bracketed so lookup_host("[::1]:443") resolves correctly.
        return Ok(format!("[{addr_str}]"));
    }
    let host = without_scheme
        .split('/')
        .next()
        .ok_or(())?
        .split(':')  // strip port if present
        .next()
        .ok_or(())?;
    if host.is_empty() { Err(()) } else { Ok(host.to_string()) }
}

/// Return a `reqwest::ClientBuilder` with all loopback-proxy security guards applied.
///
/// Callers that need to chain additional methods before `.build()` — e.g. the credential
/// gateway adding `.resolve()` pins for DNS-rebinding defence — call this instead of
/// constructing a bare `reqwest::Client::builder()`. Keeping the base configuration here
/// prevents the two proxies from silently diverging (ar-10 drift guard).
///
/// Enforced guards (single source of truth):
/// - `redirect(Policy::none())` — no silent redirects; callers must handle 3xx explicitly
/// - `connect_timeout` — fail fast on unreachable upstream
/// - `use_rustls_tls()` — consistent TLS backend (no openssl dependency)
pub(crate) fn base_builder(cfg: &LoopbackClientConfig) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(cfg.connect_timeout_secs))
        .timeout(Duration::from_secs(cfg.total_timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .use_rustls_tls()
}

/// Build a hardened reqwest client for loopback proxy use.
pub fn build_loopback_client(cfg: LoopbackClientConfig) -> Result<reqwest::Client> {
    base_builder(&cfg)
        .build()
        .context("build loopback forwarding HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_egress_config_values() {
        let c = LoopbackClientConfig::egress();
        assert_eq!(c.connect_timeout_secs, 10);
        assert_eq!(c.total_timeout_secs, 120);
    }

    #[test]
    fn test_credential_config_values() {
        let c = LoopbackClientConfig::credential();
        assert_eq!(c.connect_timeout_secs, 10);
        assert_eq!(c.total_timeout_secs, 60);
    }

    #[test]
    fn test_build_loopback_client_succeeds() {
        // Verifies that both configurations produce a usable client.
        build_loopback_client(LoopbackClientConfig::egress()).unwrap();
        build_loopback_client(LoopbackClientConfig::credential()).unwrap();
    }

    // ── base_builder drift guard (M-CRIT) ─────────────────────────────────────────
    //
    // GatewayState::new() must call base_builder() rather than constructing a bare
    // reqwest::Client::builder(). This test FAILS if base_builder() is removed or if
    // credential/mod.rs reverts to a direct Client::builder() call (drift risk).

    #[test]
    fn test_base_builder_produces_client() {
        let cfg = LoopbackClientConfig::credential();
        base_builder(&cfg).build().expect("base_builder must produce a valid client");
    }

    #[test]
    fn test_credential_module_uses_base_builder_not_direct_builder() {
        let src = include_str!("credential/mod.rs");
        // Build the banned pattern from parts so the literal doesn't appear in this file.
        let banned = ["reqwest", "::Client::builder()"].concat();
        assert!(
            !src.contains(&banned),
            "credential/mod.rs must use base_builder() not a direct reqwest::Client::builder() \
             call — reinstating the direct call re-introduces the drift risk (ar-10)"
        );
        assert!(
            src.contains("base_builder("),
            "credential/mod.rs must call base_builder() from loopback_proxy (ar-10 drift guard)"
        );
    }

    // ── T29: is_ssrf_blocked + extract_host callable from loopback_proxy (ar-10) ──

    #[test]
    fn test_ssrf_guard_in_loopback_proxy() {
        // T29: is_ssrf_blocked() and extract_host() must live in loopback_proxy (ar-10).
        // This test FAILS if either function is removed from this module.
        use std::net::IpAddr;

        // is_ssrf_blocked: loopback, private, link-local blocked; public not blocked.
        assert!(is_ssrf_blocked("127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_ssrf_blocked("192.168.1.1".parse::<IpAddr>().unwrap()));
        assert!(is_ssrf_blocked("169.254.169.254".parse::<IpAddr>().unwrap()));
        assert!(is_ssrf_blocked("::1".parse::<IpAddr>().unwrap()));
        assert!(is_ssrf_blocked("::ffff:192.168.1.1".parse::<IpAddr>().unwrap()));
        assert!(!is_ssrf_blocked("8.8.8.8".parse::<IpAddr>().unwrap()));
        assert!(!is_ssrf_blocked("2001:4860:4860::8888".parse::<IpAddr>().unwrap()));

        // extract_host: parses correctly, rejects non-https, rejects userinfo.
        assert_eq!(
            extract_host("https://www.googleapis.com/auth"),
            Ok("www.googleapis.com".to_string())
        );
        assert_eq!(
            extract_host("https://api.search.brave.com/res"),
            Ok("api.search.brave.com".to_string())
        );
        assert!(extract_host("http://evil.com").is_err(),  "non-https must fail");
        assert!(extract_host("https://user@host/").is_err(), "userinfo must fail");
        assert!(extract_host("").is_err(), "empty must fail");
    }
}
