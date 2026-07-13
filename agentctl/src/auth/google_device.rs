// RFC 8628 OAuth 2.0 Device Authorization Grant for Google.
// Writes ~/.agentos-secrets/google.json (mode 0600, atomic).
// Invoked by `agentctl auth google --device`.

use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};

const GOOGLE_DEVICE_AUTH_URL: &str = "https://oauth2.googleapis.com/device/code";
const GOOGLE_TOKEN_POLL_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_SCOPES: &str =
    "https://www.googleapis.com/auth/gmail.readonly \
     https://www.googleapis.com/auth/drive.readonly";

// Monotonic safety cap: never poll past 30 min regardless of server's expires_in.
const MAX_POLL_DURATION: Duration = Duration::from_secs(1800);

pub fn run(
    client_id_arg: Option<String>,
    client_secret_arg: Option<String>,
    force: bool,
) -> Result<()> {
    let secrets_file = super::util::secrets_file_path()?;

    // Resolve client_id / client_secret:
    // 1. CLI/env arg  2. compile-time embed  3. existing secrets file (cred.7 re-auth path).
    // When credentials come from the existing file we skip the --force guard (the operator
    // is re-authenticating with the same app credentials, not replacing them).
    let creds_from_args = client_id_arg.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
        || client_secret_arg.as_deref().map(|s| !s.is_empty()).unwrap_or(false)
        || option_env!("OAUTH_CLIENT_ID").is_some()
        || option_env!("OAUTH_CLIENT_SECRET").is_some();

    let id_from_env  = client_id_arg.filter(|s| !s.is_empty())
        .or_else(|| option_env!("OAUTH_CLIENT_ID").map(str::to_owned));
    let sec_from_env = client_secret_arg.filter(|s| !s.is_empty())
        .or_else(|| option_env!("OAUTH_CLIENT_SECRET").map(str::to_owned));

    // Try reading existing secrets file to fill missing credentials and preserve token_url.
    let existing_json: Option<serde_json::Value> = if secrets_file.exists() {
        std::fs::read(&secrets_file).ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    } else {
        None
    };

    let client_id = id_from_env
        .or_else(|| existing_json.as_ref().and_then(|v| v["client_id"].as_str().map(str::to_owned)))
        .unwrap_or_default();
    let client_secret = sec_from_env
        .or_else(|| existing_json.as_ref().and_then(|v| v["client_secret"].as_str().map(str::to_owned)))
        .unwrap_or_default();

    // Preserve token_url from existing file so operator customisations survive re-auth.
    let existing_token_url: Option<String> = existing_json.as_ref()
        .and_then(|v| v["token_url"].as_str().map(str::to_owned));

    if client_id.is_empty() {
        bail!(
            "OAUTH_CLIENT_ID is not set.\n\
             \n\
             Set it with:\n\
             \n\
             \x20 export OAUTH_CLIENT_ID=<your-client-id>\n\
             \n\
             Get credentials at: https://console.cloud.google.com/apis/credentials\n\
             Create a \"Desktop app\" OAuth 2.0 Client ID."
        );
    }
    if client_secret.is_empty() {
        bail!(
            "OAUTH_CLIENT_SECRET is not set.\n\
             \n\
             Set it with:\n\
             \n\
             \x20 export OAUTH_CLIENT_SECRET=<your-client-secret>\n\
             \n\
             Get credentials at: https://console.cloud.google.com/apis/credentials"
        );
    }

    // --force is required only when the operator is supplying new/different credentials.
    // Re-authenticating with credentials read from the existing file (cred.7 reset path)
    // does not need --force — we're updating the refresh token, not the app credentials.
    if secrets_file.exists() && creds_from_args && !force {
        bail!(
            "{} already exists.\n\
             \n\
             Use --force to overwrite:\n\
             \n\
             \x20 agentctl auth google --device --force",
            secrets_file.display()
        );
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")?;

    // Step 1: Request device code.
    let device_resp = client
        .post(GOOGLE_DEVICE_AUTH_URL)
        .form(&[
            ("client_id", client_id.as_str()),
            ("scope", GOOGLE_SCOPES),
            ("access_type", "offline"),
        ])
        .send()
        .context("Failed to reach Google device auth endpoint")?;

    if !device_resp.status().is_success() {
        let status = device_resp.status();
        let body = device_resp.text().unwrap_or_default();
        bail!("Device auth request failed (HTTP {status}): {body}");
    }

    let device_json: serde_json::Value = device_resp
        .json()
        .context("Failed to parse device auth response")?;

    let device_code = device_json["device_code"]
        .as_str()
        .context("Missing device_code in response")?
        .to_string();
    let user_code = device_json["user_code"]
        .as_str()
        .context("Missing user_code in response")?
        .to_string();
    let verification_url = device_json["verification_url"]
        .as_str()
        .context("Missing verification_url in response")?
        .to_string();
    let expires_in = device_json["expires_in"].as_u64().unwrap_or(1800);
    let base_interval_secs = device_json["interval"].as_u64().unwrap_or(5);

    // Strip control characters (incl. ESC sequences) before printing.
    let safe_url = strip_control(&verification_url);
    let safe_code = strip_control(&user_code);

    println!();
    println!("Opening device auth flow (no browser required).");
    println!();
    println!("  Visit: {safe_url}");
    println!("  Code:  {safe_code}");
    println!();
    println!(
        "Waiting for authorization (expires in {} min)...",
        expires_in / 60
    );

    // Step 2: Poll until authorized, expired, or our monotonic deadline hit.
    // Use expires_in + 30 s as the deadline; the server enforces actual expiry
    // via the expired_token error, so this is only a safety net.
    // (Do NOT use min(MAX_POLL_DURATION, expires_in+30) — when expires_in == 1800
    //  the +30 grace period is silently cancelled, causing false "expired" errors.)
    let deadline = Instant::now()
        + MAX_POLL_DURATION.max(Duration::from_secs(expires_in.saturating_add(30)));
    let mut interval = Duration::from_secs(base_interval_secs);

    loop {
        std::thread::sleep(interval);

        if Instant::now() >= deadline {
            bail!(
                "Device code expired after {} min.\n\
                 Run `agentctl auth google --device` again.",
                expires_in / 60
            );
        }

        let poll_resp = client
            .post(GOOGLE_TOKEN_POLL_URL)
            .form(&[
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("device_code", device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .context("Failed to reach token endpoint")?;

        let poll_json: serde_json::Value =
            poll_resp.json().context("Failed to parse token poll response")?;

        if let Some(error) = poll_json["error"].as_str() {
            match error {
                "authorization_pending" => {
                    // Normal — user hasn't completed auth yet.
                    continue;
                }
                "slow_down" => {
                    // Server asks us to back off — RFC 8628 §3.5: add 5 s (not double).
                    interval += Duration::from_secs(5);
                    continue;
                }
                "expired_token" | "invalid_grant" => {
                    bail!(
                        "Code expired — run `agentctl auth google --device` again."
                    );
                }
                "access_denied" => {
                    bail!("Authorization denied by the user.");
                }
                other => {
                    let desc = poll_json["error_description"].as_str().unwrap_or("");
                    bail!("Authorization failed ({other}): {desc}");
                }
            }
        }

        // Success path — token response should contain refresh_token.
        let refresh_token = poll_json["refresh_token"]
            .as_str()
            .context(
                "Google response is missing refresh_token.\n\
                 This flow requires access_type=offline in the device auth request.",
            )?
            .to_string();
        if refresh_token.is_empty() {
            bail!("Google returned an empty refresh_token.");
        }

        super::util::write_secrets_file_ext(
            &secrets_file,
            &client_id,
            &client_secret,
            &refresh_token,
            existing_token_url.as_deref(),
        )?;

        println!();
        println!("  Authorization complete.");
        println!("  Credentials written to: {}", secrets_file.display());
        println!();
        println!("  Next step:");
        println!("    docker compose up -d cos");
        println!();
        println!("  If running as the agentos service user, also run:");
        println!(
            "    sudo cp {} /home/agentos/.agentos-secrets/google.json",
            secrets_file.display()
        );
        println!("    sudo chown agentos:agentos /home/agentos/.agentos-secrets/google.json");
        println!();

        return Ok(());
    }
}

/// Remove all ASCII control characters (< 0x20) and DEL (0x7f) from `s`.
/// This prevents terminal escape-sequence injection from a malicious server.
fn strip_control(s: &str) -> String {
    s.chars()
        .filter(|&c| c >= ' ' && c != '\x7f')
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_control_passthrough_clean() {
        let url = "https://accounts.google.com/device";
        assert_eq!(strip_control(url), url);
    }

    #[test]
    fn strip_control_removes_esc() {
        // ESC byte is 0x1b < 0x20, stripped; remaining chars kept.
        let s = "\x1b[1mBOLD\x1b[0m";
        assert_eq!(strip_control(s), "[1mBOLD[0m");
    }

    #[test]
    fn strip_control_removes_del() {
        assert_eq!(strip_control("AB\x7fCD"), "ABCD");
    }

    #[test]
    fn strip_control_removes_nul_and_tab() {
        assert_eq!(strip_control("A\x00B\tC"), "ABC");
    }

    // Poll-state unit tests use httpmock to simulate Google token endpoint responses.
    // These tests exercise the state machine logic without making real network calls.

    fn make_client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    /// Drive one poll iteration against `server` and return the JSON response.
    fn poll_once(server: &httpmock::MockServer, device_code: &str) -> serde_json::Value {
        make_client()
            .post(server.url("/token"))
            .form(&[
                ("client_id", "cid"),
                ("client_secret", "csec"),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .unwrap()
            .json()
            .unwrap()
    }

    #[test]
    fn poll_authorization_pending_has_error_field() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(428)
                .header("content-type", "application/json")
                .body(r#"{"error":"authorization_pending"}"#);
        });
        let json = poll_once(&server, "dc-abc");
        assert_eq!(json["error"].as_str(), Some("authorization_pending"));
    }

    #[test]
    fn poll_slow_down_has_error_field() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(428)
                .header("content-type", "application/json")
                .body(r#"{"error":"slow_down"}"#);
        });
        let json = poll_once(&server, "dc-abc");
        assert_eq!(json["error"].as_str(), Some("slow_down"));
    }

    #[test]
    fn poll_expired_token_has_error_field() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(400)
                .header("content-type", "application/json")
                .body(r#"{"error":"expired_token"}"#);
        });
        let json = poll_once(&server, "dc-abc");
        assert_eq!(json["error"].as_str(), Some("expired_token"));
    }

    #[test]
    fn poll_success_returns_refresh_token() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"at-xyz","refresh_token":"rt-secret","token_type":"Bearer"}"#);
        });
        let json = poll_once(&server, "dc-abc");
        assert_eq!(json["refresh_token"].as_str(), Some("rt-secret"));
        assert!(json["error"].is_null());
    }

    #[test]
    fn poll_missing_refresh_token_detected() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"at-only"}"#);
        });
        let json = poll_once(&server, "dc-abc");
        // refresh_token field absent — callers must detect this.
        assert!(json["refresh_token"].is_null());
    }

    #[test]
    fn poll_access_denied_has_error_field() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(400)
                .header("content-type", "application/json")
                .body(r#"{"error":"access_denied"}"#);
        });
        let json = poll_once(&server, "dc-abc");
        assert_eq!(json["error"].as_str(), Some("access_denied"));
    }
}
