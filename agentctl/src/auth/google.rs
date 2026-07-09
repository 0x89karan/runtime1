// PKCE OAuth2 authorization-code flow for Google.
// Writes ~/.agentos-secrets/google.json (mode 0600, atomic).
// Requires OAUTH_CLIENT_ID + OAUTH_CLIENT_SECRET from env or CLI flags.
// Use --device for headless/no-browser environments (RFC 8628 device flow).

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_SCOPES: &str =
    "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/drive.readonly";

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_PORT: u16 = 8585;

#[derive(clap::Args)]
pub struct Args {
    /// Google OAuth client ID (or set OAUTH_CLIENT_ID)
    #[arg(long, env = "OAUTH_CLIENT_ID")]
    pub(super) client_id: Option<String>,

    /// Google OAuth client secret (or set OAUTH_CLIENT_SECRET)
    #[arg(long, env = "OAUTH_CLIENT_SECRET")]
    pub(super) client_secret: Option<String>,

    /// Local port for the OAuth callback server (PKCE flow only)
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,

    /// Overwrite existing token file without prompting
    #[arg(long)]
    pub(super) force: bool,

    /// Use device authorization flow (RFC 8628) — for headless servers without a browser
    #[arg(long)]
    device: bool,
}

pub fn run(args: Args) -> Result<()> {
    if args.device {
        return super::google_device::run(args.client_id, args.client_secret, args.force);
    }

    let port = args.port;

    let client_id = args.client_id.unwrap_or_default();
    let client_secret = args.client_secret.unwrap_or_default();

    if client_id.is_empty() {
        bail!(
            "OAUTH_CLIENT_ID is not set.\n\
             \n\
             Set it with:\n\
             \n\
             \x20 export OAUTH_CLIENT_ID=<your-client-id>\n\
             \n\
             Get credentials at: https://console.cloud.google.com/apis/credentials\n\
             Create a \"Desktop app\" OAuth 2.0 Client ID.\n\
             Add http://127.0.0.1:{port} as an Authorized redirect URI."
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

    let secrets_file = super::util::secrets_file_path()?;

    if secrets_file.exists() && !args.force {
        bail!(
            "{} already exists.\n\
             \n\
             Use --force to overwrite:\n\
             \n\
             \x20 agentctl auth google --force",
            secrets_file.display()
        );
    }

    // Bind the callback port early so we fail fast on EADDRINUSE before
    // creating any side effects (directory, browser, etc.).
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::anyhow!(
                "Port {port} is already in use.\n\
                 \n\
                 Kill the conflicting process (lsof -i :{port}) or pick another port:\n\
                 \n\
                 \x20 agentctl auth google --port {alt}\n\
                 \n\
                 NOTE: if you change the port, update the redirect URI in Google Cloud Console.",
                alt = port + 1
            )
        } else {
            anyhow::anyhow!("Failed to bind port {port}: {e}")
        }
    })?;
    listener
        .set_nonblocking(true)
        .context("Failed to set non-blocking on callback listener")?;
    // Use the actual bound port in case the OS assigned a different one.
    let actual_port = listener
        .local_addr()
        .context("Failed to get listener address")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{actual_port}");

    let code_verifier = gen_code_verifier()?;
    let code_challenge = base64url(Sha256::digest(code_verifier.as_bytes()).as_slice());
    let state = gen_state()?;

    let auth_url = format!(
        "{GOOGLE_AUTH_URL}\
         ?client_id={}\
         &redirect_uri={}\
         &response_type=code\
         &scope={}\
         &state={}\
         &code_challenge={}\
         &code_challenge_method=S256\
         &access_type=offline\
         &prompt=consent",
        urlencode(&client_id),
        urlencode(&redirect_uri),
        urlencode(GOOGLE_SCOPES),
        urlencode(&state),
        urlencode(&code_challenge),
    );

    println!("Opening browser for Google authorization...");
    println!();
    println!("  If the browser does not open, visit this URL manually:");
    println!("  {auth_url}");
    println!();
    println!("Waiting for callback on {redirect_uri} (timeout: 10 min)...");

    open_browser(&auth_url);

    let code = wait_for_callback(listener, &state, CALLBACK_TIMEOUT)?;

    println!("Exchanging authorization code for refresh token...");
    let refresh_token = exchange_code(
        &client_id,
        &client_secret,
        &code,
        &redirect_uri,
        &code_verifier,
        GOOGLE_TOKEN_URL,
    )?;

    super::util::write_secrets_file(&secrets_file, &client_id, &client_secret, &refresh_token)?;

    println!();
    println!("  Authorization complete.");
    println!("  Credentials written to: {}", secrets_file.display());
    println!();
    println!("  Next step:");
    println!("    docker compose up -d cos");
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// PKCE helpers
// ---------------------------------------------------------------------------

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .context("Failed to read random bytes from /dev/urandom")?;
    Ok(buf)
}

fn gen_code_verifier() -> Result<String> {
    Ok(base64url(&random_bytes(32)?))
}

fn gen_state() -> Result<String> {
    Ok(random_bytes(16)?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn base64url(data: &[u8]) -> String {
    const CHARS: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((data.len() * 4).div_ceil(3));
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 63) as usize] as char);
        out.push(CHARS[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(CHARS[(n & 63) as usize] as char);
        }
    }
    out
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3 / 2);
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(char::from_digit((byte >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            out.push(char::from_digit((byte & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let mut out = String::new();
    let mut iter = s.bytes().peekable();
    while let Some(b) = iter.next() {
        if b == b'+' {
            out.push(' ');
        } else if b == b'%' {
            let h1 = iter.next().and_then(|c| char::from(c).to_digit(16));
            let h2 = iter.next().and_then(|c| char::from(c).to_digit(16));
            if let (Some(h1), Some(h2)) = (h1, h2) {
                out.push((h1 * 16 + h2) as u8 as char);
            }
        } else {
            out.push(b as char);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Browser open
// ---------------------------------------------------------------------------

fn open_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}

// ---------------------------------------------------------------------------
// Callback server
// ---------------------------------------------------------------------------

fn wait_for_callback(
    listener: TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String> {
    let deadline = Instant::now() + timeout;

    loop {
        if Instant::now() > deadline {
            bail!(
                "OAuth callback timed out after {} minutes.\n\
                 Run agentctl auth google again.",
                timeout.as_secs() / 60
            );
        }

        match listener.accept() {
            Ok((stream, _)) => {
                match handle_callback_connection(stream, expected_state) {
                    Ok(Some(code)) => return Ok(code),
                    Ok(None) => {} // favicon or non-callback request; keep waiting
                    Err(e) => bail!("Callback error: {e}"),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => bail!("Callback server accept error: {e}"),
        }
    }
}

fn handle_callback_connection(
    mut stream: TcpStream,
    expected_state: &str,
) -> Result<Option<String>> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok();

    // Read until \r\n\r\n (end of HTTP headers).
    let mut request = String::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                request.push_str(&String::from_utf8_lossy(&buf[..n]));
                if request.contains("\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    // First line: "GET /path?query HTTP/1.1"
    let first_line = request.lines().next().unwrap_or("");

    // Skip favicon and other non-callback requests silently.
    if first_line.contains("/favicon") || !first_line.starts_with("GET") {
        let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
        return Ok(None);
    }

    // Extract the query string from the request path.
    // Split the request line into METHOD PATH HTTP/VERSION and take only PATH.
    let path = first_line.split_whitespace().nth(1).unwrap_or("");
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut error: Option<String> = None;

    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        let k = kv.next().unwrap_or("");
        let v = urldecode(kv.next().unwrap_or(""));
        match k {
            "code" => code = Some(v),
            "state" => state = Some(v),
            "error" => error = Some(v),
            _ => {}
        }
    }

    if let Some(err) = error {
        send_html(
            &mut stream,
            400,
            "<h2>Authorization failed.</h2><p>Check the terminal for details.</p>",
        );
        bail!("Google returned an error: {err}");
    }

    // If there is neither a code nor a state, this is not an auth callback
    // (browser preconnect, favicon at an unexpected path, etc.) — ignore it.
    if code.is_none() && state.is_none() {
        return Ok(None);
    }

    if state.as_deref() != Some(expected_state) {
        send_html(
            &mut stream,
            400,
            "<h2>State mismatch — possible CSRF attack.</h2>",
        );
        // Don't bail — stray browser requests (prefetch, dev-tools) with the
        // wrong or absent state should be ignored, not kill the whole flow.
        eprintln!("Warning: CSRF state mismatch on callback — ignoring (stray browser request?)");
        return Ok(None);
    }

    match code {
        None => {
            send_html(
                &mut stream,
                400,
                "<h2>No authorization code in callback.</h2>",
            );
            // Not a fatal error — might be a stray request; keep listening.
            Ok(None)
        }
        Some(code) => {
            send_html(
                &mut stream,
                200,
                "<h2>Authorization complete.</h2>\
                 <p>You can close this tab and return to the terminal.</p>",
            );
            Ok(Some(code))
        }
    }
}

fn send_html(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Unknown",
    };
    let html = format!("<!DOCTYPE html><html><body>{body}</body></html>");
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {html}",
        len = html.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

// ---------------------------------------------------------------------------
// Token exchange
// ---------------------------------------------------------------------------

fn exchange_code(
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    token_url: &str,
) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")?;

    let resp = client
        .post(token_url)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
            ("code_verifier", code_verifier),
        ])
        .send()
        .context("Failed to reach Google token endpoint")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        bail!(
            "Token exchange failed (HTTP {status}).\n\
             Response: {body}\n\
             \n\
             Check that your redirect URI matches exactly in Google Cloud Console:\n\
             \x20 {redirect_uri}"
        );
    }

    let json: serde_json::Value = resp
        .json()
        .context("Failed to parse token endpoint response")?;

    let token = json["refresh_token"]
        .as_str()
        .map(|s| s.to_string())
        .context(
            "Google response is missing refresh_token.\n\
             Ensure the OAuth app requested offline access and prompt=consent.\n\
             If you have already granted access, revoke it at:\n\
             \x20 https://myaccount.google.com/permissions\n\
             Then run agentctl auth google again.",
        )?;
    if token.is_empty() {
        bail!(
            "Google returned an empty refresh_token.\n\
             Ensure your OAuth app has offline access and prompt=consent.\n\
             If you previously authorized, revoke it at:\n\
             \x20 https://myaccount.google.com/permissions\n\
             Then run agentctl auth google again."
        );
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_empty() {
        assert_eq!(base64url(b""), "");
    }

    #[test]
    fn base64url_hello() {
        // "Man" → "TWFu" in standard base64; same in base64url
        assert_eq!(base64url(b"Man"), "TWFu");
    }

    #[test]
    fn base64url_no_padding() {
        // base64url of single byte should have no '=' padding
        let s = base64url(b"\xff");
        assert!(!s.contains('='));
        assert_eq!(s, "_w");
    }

    #[test]
    fn urlencode_passthrough_safe_chars() {
        let s = "abc-123_xyz.~";
        assert_eq!(urlencode(s), s);
    }

    #[test]
    fn urlencode_percent_encodes_special() {
        let encoded = urlencode("hello world&foo=bar");
        assert_eq!(encoded, "hello%20world%26foo%3Dbar");
    }

    #[test]
    fn urldecode_roundtrip() {
        let original = "hello world&foo=bar";
        let encoded = urlencode(original);
        let decoded = urldecode(&encoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn gen_code_verifier_length() {
        // 32 bytes → 43 base64url chars (no padding)
        let v = gen_code_verifier().unwrap();
        assert_eq!(v.len(), 43);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn gen_state_is_hex() {
        let s = gen_state().unwrap();
        assert_eq!(s.len(), 32); // 16 bytes × 2 hex chars
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sha256_challenge_matches_spec() {
        // Known PKCE test vector:
        // verifier  = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        // challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = base64url(Sha256::digest(verifier.as_bytes()).as_slice());
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    // ── write_secrets_file ────────────────────────────────────────────────────

    #[test]
    fn write_secrets_file_produces_valid_json() {
        let dir = std::env::temp_dir().join(format!("agentos-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("google.json");
        crate::auth::util::write_secrets_file(&path, "cid", "csecret", "rtoken").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["client_id"], "cid");
        assert_eq!(v["client_secret"], "csecret");
        assert_eq!(v["refresh_token"], "rtoken");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_secrets_file_sets_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("agentos-test-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("google.json");
        crate::auth::util::write_secrets_file(&path, "cid", "csecret", "rtoken").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secrets file must be 0600");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── handle_callback_connection ────────────────────────────────────────────

    fn connect_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    fn send_request(client: &mut TcpStream, request: &str) {
        client.write_all(request.as_bytes()).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
    }

    #[test]
    fn callback_favicon_returns_none() {
        let (mut client, server) = connect_pair();
        send_request(
            &mut client,
            "GET /favicon.ico HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        let result = handle_callback_connection(server, "state123").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn callback_no_code_no_state_returns_none() {
        let (mut client, server) = connect_pair();
        send_request(
            &mut client,
            "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        let result = handle_callback_connection(server, "state123").unwrap();
        assert!(result.is_none(), "stray GET / must be ignored, not CSRF-bailed");
    }

    #[test]
    fn callback_csrf_mismatch_returns_none() {
        // CSRF mismatch should return Ok(None) (keep listening) not Err
        // (which would abort the entire auth flow on a stray browser request).
        let (mut client, server) = connect_pair();
        send_request(
            &mut client,
            "GET /?code=AUTHCODE&state=WRONGSTATE HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        let result = handle_callback_connection(server, "RIGHTSTATE");
        assert!(result.is_ok(), "state mismatch must return Ok");
        assert!(result.unwrap().is_none(), "state mismatch must return Ok(None)");
    }

    #[test]
    fn callback_error_param_errors() {
        let (mut client, server) = connect_pair();
        send_request(
            &mut client,
            "GET /?error=access_denied&state=S HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        let result = handle_callback_connection(server, "S");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("access_denied"));
    }

    #[test]
    fn callback_valid_code_and_state_returns_code() {
        let (mut client, server) = connect_pair();
        send_request(
            &mut client,
            "GET /?code=MYAUTHCODE&state=MYSTATE HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        let result = handle_callback_connection(server, "MYSTATE").unwrap();
        assert_eq!(result, Some("MYAUTHCODE".to_string()));
    }

    #[test]
    fn callback_url_encoded_code_is_decoded() {
        let (mut client, server) = connect_pair();
        // Code contains URL-encoded characters
        send_request(
            &mut client,
            "GET /?code=hello%20world&state=MYSTATE HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        let result = handle_callback_connection(server, "MYSTATE").unwrap();
        assert_eq!(result, Some("hello world".to_string()));
    }

    // ── callback: state-only (no code), valid state ───────────────────────────
    // Google shouldn't send this, but ensures we don't panic or falsely
    // return a code when only state is present.
    #[test]
    fn callback_state_only_no_code_returns_none() {
        let (mut client, server) = connect_pair();
        send_request(
            &mut client,
            "GET /?state=MYSTATE HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        );
        // state matches but no code → the code branch returns Ok(None)
        let result = handle_callback_connection(server, "MYSTATE").unwrap();
        assert!(result.is_none(), "state-only request with no code must return None");
    }

    // ── urldecode: plus-sign is decoded as space ──────────────────────────────
    #[test]
    fn urldecode_plus_becomes_space() {
        assert_eq!(urldecode("hello+world"), "hello world");
    }

    // ── urldecode: truncated percent sequence is dropped silently ─────────────
    #[test]
    fn urldecode_truncated_percent_is_silently_dropped() {
        // "%2" is incomplete; the implementation silently drops it
        let out = urldecode("%2");
        // Must not panic; content doesn't matter as long as it's non-crashing
        let _ = out;
    }

    // ── base64url: two-byte chunk (no padding on third char) ──────────────────
    #[test]
    fn base64url_two_byte_chunk() {
        // b"\x00\xff" → two bytes; only three output chars (no fourth)
        let s = base64url(b"\x00\xff");
        assert_eq!(s.len(), 3);
        assert!(!s.contains('='));
    }

    // ── send_html: status 400 produces a 400 response line ───────────────────
    #[test]
    fn send_html_400_response_contains_bad_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut reader = TcpStream::connect(addr).unwrap();
        let (mut writer, _) = listener.accept().unwrap();
        send_html(&mut writer, 400, "<h2>CSRF error</h2>");
        drop(writer);
        let mut response = String::new();
        reader.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
        assert!(response.contains("CSRF error"));
    }

    // ── send_html: status 200 produces a 200 response line ───────────────────
    #[test]
    fn send_html_200_response_contains_ok() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut reader = TcpStream::connect(addr).unwrap();
        let (mut writer, _) = listener.accept().unwrap();
        send_html(&mut writer, 200, "<h2>Done</h2>");
        drop(writer);
        let mut response = String::new();
        reader.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Done"));
    }

    // ── secrets_file_path: returns path under $HOME ───────────────────────────
    #[test]
    fn secrets_file_path_under_home() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let path = crate::auth::util::secrets_file_path().unwrap();
        assert!(
            path.starts_with(&home),
            "secrets path must be under $HOME, got {:?}",
            path
        );
        assert!(path.ends_with("google.json"));
    }

    // ── write_secrets_file: creates parent dir when absent ────────────────────
    #[test]
    fn write_secrets_file_creates_parent_dir() {
        let base = std::env::temp_dir()
            .join(format!("agentos-test-newdir-{}", std::process::id()));
        // base must NOT exist before the call
        let _ = std::fs::remove_dir_all(&base);
        let path = base.join("google.json");
        crate::auth::util::write_secrets_file(&path, "a", "b", "c").unwrap();
        assert!(path.exists(), "file must be created");
        std::fs::remove_dir_all(&base).ok();
    }

    // ── exchange_code() — token exchange against a mock HTTP server ───────────
    fn call_exchange(server: &httpmock::MockServer) -> anyhow::Result<String> {
        exchange_code(
            "cid", "cs", "authcode",
            "http://127.0.0.1/cb", "verifier",
            &server.url("/token"),
        )
    }

    #[test]
    fn exchange_code_success_returns_refresh_token() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"refresh_token":"rt-abc","access_token":"at-xyz","expires_in":3600}"#);
        });
        let token = call_exchange(&server).unwrap();
        assert_eq!(token, "rt-abc");
    }

    #[test]
    fn exchange_code_non_200_errors() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(400).body(r#"{"error":"invalid_grant"}"#);
        });
        let err = call_exchange(&server).unwrap_err();
        assert!(
            err.to_string().contains("400"),
            "expected HTTP 400 in error, got: {err}"
        );
    }

    #[test]
    fn exchange_code_missing_refresh_token_errors() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"access_token":"at-only"}"#);
        });
        let err = call_exchange(&server).unwrap_err();
        assert!(
            err.to_string().contains("refresh_token"),
            "expected 'refresh_token' in error, got: {err}"
        );
    }

    #[test]
    fn exchange_code_malformed_json_errors() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(200).body("not-valid-json!!!!");
        });
        let err = call_exchange(&server).unwrap_err();
        assert!(!err.to_string().is_empty(), "expected error for malformed JSON");
    }

    #[test]
    fn exchange_code_empty_refresh_token_errors() {
        let server = httpmock::MockServer::start();
        let _m = server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"refresh_token":""}"#);
        });
        let err = call_exchange(&server).unwrap_err();
        assert!(
            err.to_string().contains("empty"),
            "expected 'empty' in error, got: {err}"
        );
    }
}
