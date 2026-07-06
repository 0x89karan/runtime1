//! Shared loopback-proxy HTTP client builder (ar-10 / cred.3.1).
//!
//! Both `EgressProxy` and `CredentialGateway` use a loopback-bound HTTP server that
//! forwards requests to upstream APIs. The security guards (redirect policy, connect
//! timeout, TLS backend) must be identical and centrally enforced — divergence between
//! two separate `reqwest::Client::builder()` calls has caused real bugs.
//!
//! Usage:
//! ```ignore
//! let client = LoopbackClient::build(
//!     LoopbackClientConfig::egress()   // 10s connect, 120s total, 8 MB response
//! )?;
//! ```

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

/// Build a hardened reqwest client for loopback proxy use.
///
/// Enforced guards (single source of truth):
/// - `redirect(Policy::none())` — no silent redirects; callers must handle 3xx explicitly
/// - `connect_timeout` — fail fast on unreachable upstream
/// - `use_rustls_tls()` — consistent TLS backend (no openssl dependency)
pub fn build_loopback_client(cfg: LoopbackClientConfig) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(cfg.connect_timeout_secs))
        .timeout(Duration::from_secs(cfg.total_timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .use_rustls_tls()
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
}
