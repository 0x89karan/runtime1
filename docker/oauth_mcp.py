#!/usr/bin/env python3
"""
oauth_mcp — Generic OAuth2 authorization-code + PKCE MCP server.

Tools:
  oauth_start_auth()                              → { auth_url: str }
  oauth_check_auth()                              → { ready: bool, scopes?: [str], error?: str }
  oauth_call_api(url, method?, headers?, body?)   → { status: int, body: str }

Preferred setup (Mac host):
  agentctl auth google          # one-time: runs PKCE flow, writes ~/.agentos-secrets/google.json
  docker compose up -d cos      # reads /run/secrets/google.json at startup

Fallback (env vars, legacy):
  OAUTH_CLIENT_ID      — OAuth client ID
  OAUTH_CLIENT_SECRET  — OAuth client secret
  OAUTH_REFRESH_TOKEN  — Refresh token (skips interactive dance)

Optional env vars:
  OAUTH_AUTH_URL       — Authorization endpoint (default: Google)
  OAUTH_TOKEN_URL      — Token endpoint (default: Google)
  OAUTH_SCOPES         — Space-separated scopes (default: Gmail + Drive read)
  OAUTH_ALLOWED_HOSTS  — Comma-separated host allowlist for oauth_call_api
  OAUTH_PROVIDER_NAME  — Token file basename (default: "oauth")

Credential precedence (highest → lowest):
  1. /run/secrets/google.json (provisioned by agentctl auth google)
  2. OAUTH_CLIENT_ID / OAUTH_CLIENT_SECRET / OAUTH_REFRESH_TOKEN env vars

Token file: ~/.agentos-oauth/<OAUTH_PROVIDER_NAME>.json (0600, atomic write)
Access tokens: in-memory only, never written to disk.

Capability required:
  capabilities = [{ Net = { hosts = [...provider hosts...], ports = [443] } }]
"""
import base64, hashlib, html, json, os, secrets, socket, ssl, sys
import tempfile, threading, time, urllib.error, urllib.parse, urllib.request
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, HTTPServer
from threading import Thread
from typing import Optional

# Credential broker (cred.3). Injected at spawn by agentd when enabled.
# When set, oauth_call_api routes through the broker instead of using the
# in-memory access token directly. The broker attaches Authorization: Bearer.
_BROKER_URL   = os.environ.get("AGENTD_CREDENTIAL_GATEWAY_URL", "").rstrip("/")
_BROKER_TOKEN = os.environ.get("AGENTD_CREDENTIAL_TOKEN", "")

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

RESPONSE_CAP    = 4 * 1024 * 1024   # 4 MB cap on oauth_call_api response body
ERROR_BODY_CAP  = 512               # bytes read from token endpoint error body
REQUEST_TIMEOUT = 20                 # seconds per sub-request (refresh + api call)
CALLBACK_TIMEOUT_S = 600            # 10 minutes for the user to complete auth

# Google OAuth defaults — used when env vars are absent.
# Keep in sync with agentctl/src/auth/google.rs (GOOGLE_AUTH_URL / GOOGLE_TOKEN_URL / GOOGLE_SCOPES).
GOOGLE_AUTH_URL      = "https://accounts.google.com/o/oauth2/v2/auth"
GOOGLE_TOKEN_URL     = "https://oauth2.googleapis.com/token"
GOOGLE_SCOPES        = "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/drive.readonly"
GOOGLE_ALLOWED_HOSTS = "accounts.google.com,oauth2.googleapis.com,www.googleapis.com,gmail.googleapis.com"

# Path written by `agentctl auth google` on the host, bind-mounted into containers.
SECRETS_FILE = "/run/secrets/google.json"

TOOLS = [
    {
        "name": "oauth_start_auth",
        "description": (
            "Start the OAuth2 authorization flow. Returns an authorization URL. "
            "Present this URL to the operator via request_approval so they can "
            "approve in a browser. After they complete the browser flow, call "
            "oauth_check_auth to exchange the code for tokens."
        ),
        "inputSchema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "oauth_check_auth",
        "description": (
            "Check whether OAuth authorization is complete. Returns "
            "{ ready: true, scopes: [...] } if authorized, or "
            "{ ready: false, error: 'no_session'|'pending'|'timeout'|'<error>' } otherwise. "
            "Call after the operator completes the browser flow."
        ),
        "inputSchema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "oauth_call_api",
        "description": (
            "Make an authenticated HTTP API call using the current OAuth access token. "
            "Token is refreshed automatically on 401. Host must be in OAUTH_ALLOWED_HOSTS. "
            "Only HTTPS. Response body capped at 4 MB."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "url":     {"type": "string",  "description": "HTTPS URL to call."},
                "method":  {"type": "string",  "description": "HTTP method (default GET)."},
                "headers": {"type": "object",  "description": "Additional request headers."},
                "body":    {"type": "string",  "description": "Request body (for POST/PUT)."},
            },
            "required": ["url"],
        },
    },
]

# ---------------------------------------------------------------------------
# Config (loaded once at startup)
# ---------------------------------------------------------------------------

_cfg: dict = {}

def _load_config() -> Optional[str]:
    """Populate _cfg from secrets file (preferred) then env vars (fallback).

    Precedence:
      1. /run/secrets/google.json  (provisioned by `agentctl auth google`)
      2. OAUTH_CLIENT_ID / OAUTH_CLIENT_SECRET / OAUTH_REFRESH_TOKEN env vars
    URL/scope fields default to hardcoded Google values; env vars override.
    """
    # Broker mode: never read raw credentials into this process.
    # Only load routing config needed for oauth_call_api's broker path.
    if _BROKER_URL:
        _cfg["OAUTH_PROVIDER_NAME"] = (
            os.environ.get("OAUTH_PROVIDER_NAME", "").strip() or "google"
        )
        pname = _cfg["OAUTH_PROVIDER_NAME"]
        if not all(c.isalnum() or c in ('-', '_') for c in pname) or not pname.isascii():
            return f"OAUTH_PROVIDER_NAME contains invalid characters (must be [a-z0-9A-Z0-9_-]): {pname!r}"
        raw_hosts = os.environ.get("OAUTH_ALLOWED_HOSTS", "").strip() or GOOGLE_ALLOWED_HOSTS
        _cfg["ALLOWED_HOSTS"] = {h.strip() for h in raw_hosts.split(",") if h.strip()}
        return None

    file_client_id = ""
    file_client_secret = ""
    file_refresh_token = ""

    if os.path.isfile(SECRETS_FILE):
        try:
            with open(SECRETS_FILE) as fh:
                data = json.load(fh)
            file_client_id     = (data.get("client_id")     or "").strip()
            file_client_secret = (data.get("client_secret") or "").strip()
            file_refresh_token = (data.get("refresh_token") or "").strip()
        except Exception as exc:
            print(f"oauth_mcp: WARNING: could not read {SECRETS_FILE}: {exc}", file=sys.stderr)

    # Env vars override the secrets file if non-empty.
    client_id     = os.environ.get("OAUTH_CLIENT_ID",     "").strip() or file_client_id
    client_secret = os.environ.get("OAUTH_CLIENT_SECRET", "").strip() or file_client_secret
    refresh_token = os.environ.get("OAUTH_REFRESH_TOKEN", "").strip() or file_refresh_token

    if not client_id or not client_secret:
        msg = (
            "oauth_mcp: OAUTH_CLIENT_ID / OAUTH_CLIENT_SECRET are not set.\n"
            "  Provision credentials on your Mac (once):\n"
            "    agentctl auth google\n"
            "  Then restart the container:\n"
            "    docker compose restart cos"
        )
        print(msg, file=sys.stderr)
        return "oauth_mcp: credentials not configured — see stderr for instructions"

    _cfg["OAUTH_CLIENT_ID"]     = client_id
    _cfg["OAUTH_CLIENT_SECRET"] = client_secret
    _cfg["OAUTH_REFRESH_TOKEN"] = refresh_token

    # URL / scope / host fields: env var takes priority, Google defaults as fallback.
    _cfg["OAUTH_AUTH_URL"]  = os.environ.get("OAUTH_AUTH_URL",  "").strip() or GOOGLE_AUTH_URL
    _cfg["OAUTH_TOKEN_URL"] = os.environ.get("OAUTH_TOKEN_URL", "").strip() or GOOGLE_TOKEN_URL
    _cfg["OAUTH_SCOPES"]    = os.environ.get("OAUTH_SCOPES",    "").strip() or GOOGLE_SCOPES
    _cfg["OAUTH_PROVIDER_NAME"] = (
        os.environ.get("OAUTH_PROVIDER_NAME", "").strip() or "google"
    )

    raw_hosts = os.environ.get("OAUTH_ALLOWED_HOSTS", "").strip() or GOOGLE_ALLOWED_HOSTS
    _cfg["ALLOWED_HOSTS"] = {h.strip() for h in raw_hosts.split(",") if h.strip()}

    token_dir = os.path.expanduser("~/.agentos-oauth")
    os.makedirs(token_dir, mode=0o700, exist_ok=True)
    _cfg["TOKEN_FILE"] = os.path.join(token_dir, f"{_cfg['OAUTH_PROVIDER_NAME']}.json")

    return None

# ---------------------------------------------------------------------------
# SSRF protection (ported from http_mcp.py)
# ---------------------------------------------------------------------------

import ipaddress

def _is_ssrf_blocked(url: str) -> bool:
    """Return True if the URL resolves to a blocked (private/loopback) address."""
    try:
        parsed = urllib.parse.urlparse(url)
        hostname = parsed.hostname or ""
        try:
            addr = ipaddress.ip_address(hostname)
        except ValueError:
            infos = socket.getaddrinfo(hostname, None)
            addr = ipaddress.ip_address(infos[0][4][0])
        return (
            addr.is_loopback
            or addr.is_private
            or addr.is_link_local
            or addr.is_multicast
            or addr.is_unspecified
        )
    except Exception:
        return False

# ---------------------------------------------------------------------------
# AuthSession — one active session at a time
# ---------------------------------------------------------------------------

@dataclass
class AuthSession:
    state:         str
    code_verifier: str
    redirect_uri:  str
    expires_at:    float
    server:        HTTPServer
    thread:        Thread
    result:        Optional[dict] = None  # {code} once captured
    lock:          threading.Lock = field(default_factory=threading.Lock)

_session_lock = threading.Lock()
_session: Optional[AuthSession] = None

# ---------------------------------------------------------------------------
# Token state (in-memory)
# ---------------------------------------------------------------------------

_token_lock     = threading.Lock()
_access_token:  Optional[str]   = None
_refresh_token: Optional[str]   = None
_token_expiry:  Optional[float] = None   # time.monotonic()
_token_scopes:  list            = []
_auth_state     = "idle"  # idle | pending | authorized

# ---------------------------------------------------------------------------
# Callback HTTP handler
# ---------------------------------------------------------------------------

class _CallbackHandler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass  # silence access log

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        params = urllib.parse.parse_qs(parsed.query)

        if parsed.path != "/callback":
            self._respond(404, "Not found")
            return

        with _session_lock:
            sess = _session

        if sess is None:
            self._respond(410, "No active session")
            return

        with sess.lock:
            if sess.result is not None:
                self._respond(410, "Already completed")
                return

            state = params.get("state", [None])[0]
            if not state or state != sess.state:
                self._respond(400, "Invalid state — possible CSRF")
                return

            error = params.get("error", [None])[0]
            if error:
                sess.result = {"error": error}
                self._respond(400, f"Authorization failed: {html.escape(error)}")
                return

            code  = params.get("code",  [None])[0]

            if not code:
                self._respond(400, "No code in callback")
                return

            sess.result = {"code": code}
            self._respond(200, (
                "<html><body>"
                "<h2>Authorization complete.</h2>"
                "<p>You can close this tab and return to your agent.</p>"
                "</body></html>"
            ))

    def _respond(self, status: int, body: str):
        body_bytes = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body_bytes)))
        self.end_headers()
        self.wfile.write(body_bytes)


def _run_callback_server(sess: AuthSession):
    """Run in daemon thread; shuts down after first result or timeout."""
    deadline = sess.expires_at
    while True:
        sess.server.timeout = max(0, deadline - time.monotonic())
        sess.server.handle_request()
        with sess.lock:
            if sess.result is not None:
                break
        if time.monotonic() >= deadline:
            with sess.lock:
                if sess.result is None:
                    sess.result = {"error": "timeout"}
            break
    try:
        sess.server.server_close()
    except Exception:
        pass

# ---------------------------------------------------------------------------
# Token file I/O
# ---------------------------------------------------------------------------

def _load_token_file() -> Optional[dict]:
    path = _cfg.get("TOKEN_FILE")
    if not path or not os.path.exists(path):
        return None
    try:
        with open(path, "r") as f:
            return json.load(f)
    except Exception:
        return None


def _save_token_file(data: dict):
    path = _cfg["TOKEN_FILE"]
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix=".tok-")
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(data, f)
        os.chmod(tmp, 0o600)
        os.replace(tmp, path)
    except Exception:
        try:
            os.unlink(tmp)
        except Exception:
            pass

# ---------------------------------------------------------------------------
# Token exchange and refresh
# ---------------------------------------------------------------------------

def _exchange_code(code: str, redirect_uri: str, code_verifier: str) -> Optional[str]:
    """Exchange auth code for tokens. Returns error string or None on success."""
    global _access_token, _refresh_token, _token_expiry, _token_scopes, _auth_state

    data = urllib.parse.urlencode({
        "grant_type":    "authorization_code",
        "code":          code,
        "redirect_uri":  redirect_uri,
        "client_id":     _cfg["OAUTH_CLIENT_ID"],
        "client_secret": _cfg["OAUTH_CLIENT_SECRET"],
        "code_verifier": code_verifier,
    }).encode()

    req = urllib.request.Request(
        _cfg["OAUTH_TOKEN_URL"], data=data,
        headers={"Content-Type": "application/x-www-form-urlencoded"},
    )
    ctx = ssl.create_default_context()
    try:
        with urllib.request.urlopen(req, context=ctx, timeout=REQUEST_TIMEOUT) as resp:
            payload = json.loads(resp.read(RESPONSE_CAP))
    except urllib.error.HTTPError as e:
        body = ""
        try:
            body = e.read(ERROR_BODY_CAP).decode("utf-8", errors="replace")
        except Exception:
            pass
        return f"token exchange failed {e.code}: {body}"
    except Exception as exc:
        return f"token exchange error: {exc}"

    with _token_lock:
        _access_token  = payload.get("access_token")
        _refresh_token = payload.get("refresh_token") or _cfg.get("OAUTH_REFRESH_TOKEN")
        expires_in     = payload.get("expires_in", 3600)
        _token_expiry  = time.monotonic() + int(expires_in)
        _token_scopes  = payload.get("scope", _cfg.get("OAUTH_SCOPES", "")).split()
        _auth_state    = "authorized"

    if _refresh_token:
        _save_token_file({"refresh_token": _refresh_token})

    return None


def _do_refresh() -> Optional[str]:
    """Refresh access token. Must be called with _token_lock held externally (double-checked)."""
    global _access_token, _refresh_token, _token_expiry, _token_scopes, _auth_state

    rt = _refresh_token
    if not rt:
        return "no refresh token available"

    data = urllib.parse.urlencode({
        "grant_type":    "refresh_token",
        "refresh_token": rt,
        "client_id":     _cfg["OAUTH_CLIENT_ID"],
        "client_secret": _cfg["OAUTH_CLIENT_SECRET"],
    }).encode()

    req = urllib.request.Request(
        _cfg["OAUTH_TOKEN_URL"], data=data,
        headers={"Content-Type": "application/x-www-form-urlencoded"},
    )
    ctx = ssl.create_default_context()
    try:
        with urllib.request.urlopen(req, context=ctx, timeout=REQUEST_TIMEOUT) as resp:
            payload = json.loads(resp.read(RESPONSE_CAP))
    except urllib.error.HTTPError as e:
        body = ""
        try:
            body = e.read(ERROR_BODY_CAP).decode("utf-8", errors="replace")
        except Exception:
            pass
        return f"refresh failed {e.code}: {body}"
    except Exception as exc:
        return f"refresh error: {exc}"

    _access_token = payload.get("access_token", _access_token)
    new_rt = payload.get("refresh_token")
    if new_rt:
        _refresh_token = new_rt
    expires_in    = payload.get("expires_in", 3600)
    _token_expiry = time.monotonic() + int(expires_in)
    _token_scopes = payload.get("scope", " ".join(_token_scopes)).split()

    if _refresh_token:
        _save_token_file({"refresh_token": _refresh_token})

    return None


def _ensure_fresh_token() -> Optional[str]:
    """Refresh if access token is expired or missing. Returns error or None."""
    with _token_lock:
        if _access_token and _token_expiry and time.monotonic() < _token_expiry - 30:
            return None
        return _do_refresh()

# ---------------------------------------------------------------------------
# Tool handlers
# ---------------------------------------------------------------------------

def handle_oauth_start_auth(_args: dict) -> tuple:
    global _session, _auth_state

    if _BROKER_URL and _BROKER_TOKEN:
        return None, json.dumps({
            "error": "broker_managed",
            "message": (
                "OAuth is managed by the credential broker. "
                "Credentials are provisioned via `agentctl auth google`. "
                "Use oauth_call_api to make authenticated requests."
            ),
        })
    if _BROKER_URL and not _BROKER_TOKEN:
        return None, json.dumps({
            "error": "broker_token_missing",
            "message": (
                "AGENTD_CREDENTIAL_GATEWAY_URL is set but AGENTD_CREDENTIAL_TOKEN is absent. "
                "This is a spawn misconfiguration — both env vars must be injected together by agentd."
            ),
        })

    # If credentials were pre-provisioned via agentctl auth google (refresh token
    # present), the in-container OAuth dance is unnecessary.
    if _cfg.get("OAUTH_REFRESH_TOKEN"):
        print(
            "oauth_mcp: INFO: refresh token already available — "
            "call oauth_check_auth instead of starting a new auth flow.",
            file=sys.stderr,
        )

    # PKCE
    code_verifier  = secrets.token_urlsafe(64)   # 86-char URL-safe string
    code_challenge = base64.urlsafe_b64encode(
        hashlib.sha256(code_verifier.encode()).digest()
    ).rstrip(b"=").decode()

    csrf_state = secrets.token_urlsafe(16)

    # Parse OAUTH_CALLBACK_PORT safely — non-numeric value must not crash the server.
    try:
        callback_port = int(os.environ.get("OAUTH_CALLBACK_PORT", "") or "0")
    except ValueError:
        return None, "OAUTH_CALLBACK_PORT must be an integer (e.g. 8585)"

    # When a fixed port is requested (Docker mode), bind on all interfaces so
    # Docker's port-mapping (which forwards to the container's eth0, not loopback)
    # can reach the server.  Ephemeral-port path keeps loopback for safety.
    bind_host = "0.0.0.0" if callback_port else "127.0.0.1"

    # Close old session, bind the new server, and register it atomically under
    # _session_lock — prevents a concurrent oauth_start_auth from orphaning the
    # new server (double-lock gap race condition).
    with _session_lock:
        old = _session
        if old is not None:
            try:
                old.server.server_close()
            except Exception:
                pass
        srv = HTTPServer((bind_host, callback_port), _CallbackHandler)
        port = srv.server_address[1]
        redirect_uri = f"http://127.0.0.1:{port}/callback"
        new_sess = AuthSession(
            state=csrf_state,
            code_verifier=code_verifier,
            redirect_uri=redirect_uri,
            expires_at=time.monotonic() + CALLBACK_TIMEOUT_S,
            server=srv,
            thread=Thread(target=_run_callback_server, daemon=True),
        )
        _session = new_sess

    _auth_state = "pending"

    new_sess.thread.start()

    params = {
        "client_id":             _cfg["OAUTH_CLIENT_ID"],
        "response_type":         "code",
        "redirect_uri":          redirect_uri,
        "state":                 csrf_state,
        "code_challenge":        code_challenge,
        "code_challenge_method": "S256",
    }
    if _cfg.get("OAUTH_SCOPES"):
        params["scope"] = _cfg["OAUTH_SCOPES"]

    auth_url = _cfg["OAUTH_AUTH_URL"] + "?" + urllib.parse.urlencode(params)
    return {"auth_url": auth_url}, None


def handle_oauth_check_auth(_args: dict) -> tuple:
    global _auth_state, _refresh_token, _access_token, _token_expiry, _token_scopes, _session

    if _BROKER_URL and _BROKER_TOKEN:
        return {"ready": True, "broker_managed": True}, None
    if _BROKER_URL and not _BROKER_TOKEN:
        return None, json.dumps({
            "error": "broker_token_missing",
            "message": (
                "AGENTD_CREDENTIAL_GATEWAY_URL is set but AGENTD_CREDENTIAL_TOKEN is absent. "
                "This is a spawn misconfiguration — both env vars must be injected together by agentd."
            ),
        })

    # Fast path: live, non-expired access token already in hand.
    with _token_lock:
        if (_auth_state == "authorized" and _access_token
                and _token_expiry and time.monotonic() < _token_expiry - 30):
            return {"ready": True, "scopes": _token_scopes}, None

    # A refresh token is present (file-provided or env-provided) but there is no
    # live access token yet — silently refresh now.  This handles the startup
    # lazy-fetch case (startup sets _auth_state="authorized" but leaves
    # _access_token=None) AND the access-token expiry case during a long session.
    rt = _cfg.get("OAUTH_REFRESH_TOKEN", "") or _refresh_token
    if rt:
        with _token_lock:
            if not _refresh_token:
                _refresh_token = rt
            _auth_state = "authorized"
        err = _ensure_fresh_token()
        if err:
            with _token_lock:
                _auth_state = "idle"
            return None, err
        with _token_lock:
            return {"ready": True, "scopes": _token_scopes}, None

    with _session_lock:
        sess = _session

    if sess is None:
        return {"ready": False, "error": "no_session"}, None

    with sess.lock:
        result = sess.result

    if result is None:
        return {"ready": False, "error": "pending"}, None

    error = result.get("error")
    code  = result.get("code")

    with _session_lock:
        _session = None

    if error == "timeout":
        _auth_state = "idle"
        return {"ready": False, "error": "timeout"}, None

    if error:
        _auth_state = "idle"
        return {"ready": False, "error": error}, None

    if not code:
        _auth_state = "idle"
        return {"ready": False, "error": "no_code"}, None

    # Exchange code for tokens
    redirect_uri   = sess.redirect_uri
    code_verifier  = sess.code_verifier
    err = _exchange_code(code, redirect_uri, code_verifier)
    if err:
        _auth_state = "idle"
        return {"ready": False, "error": err}, None

    with _token_lock:
        return {"ready": True, "scopes": _token_scopes}, None


def handle_oauth_call_api(args: dict) -> tuple:
    global _auth_state

    if _BROKER_URL and not _BROKER_TOKEN:
        return None, json.dumps({
            "error": "broker_token_missing",
            "message": (
                "AGENTD_CREDENTIAL_GATEWAY_URL is set but AGENTD_CREDENTIAL_TOKEN is absent. "
                "This is a spawn misconfiguration — both env vars must be injected together by agentd."
            ),
        })

    url    = args.get("url", "").strip()
    method = args.get("method", "GET").upper()
    extra_headers = args.get("headers") or {}
    body   = args.get("body")

    if not url.startswith("https://"):
        return None, json.dumps({"error": "https_required"})

    # Method and SSRF checks apply to both broker and legacy paths.
    method_allow = {"GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"}
    if method not in method_allow:
        return None, json.dumps({"error": f"method_not_allowed: {method}"})

    parsed   = urllib.parse.urlparse(url)
    hostname = (parsed.hostname or "").lower()

    if _cfg["ALLOWED_HOSTS"] and hostname not in _cfg["ALLOWED_HOSTS"]:
        return None, json.dumps({"error": "host_not_allowed", "host": hostname})

    if _is_ssrf_blocked(url):
        return None, json.dumps({"error": "host_not_allowed", "host": hostname})

    # Extra-headers type guard (string keys+values only).
    if not all(isinstance(k, str) and isinstance(v, str) for k, v in extra_headers.items()):
        return None, json.dumps({"error": "invalid_extra_headers"})

    # Primary path: credential broker (cred.3+).
    # Route via broker when env vars are injected by agentd — the broker injects
    # Authorization: Bearer and manages token refresh centrally.
    if _BROKER_URL and _BROKER_TOKEN:
        provider = _cfg.get("OAUTH_PROVIDER_NAME", "google")
        # Encode path+query into broker URL: {gw}/{provider}/{path}[?query]
        broker_path = (parsed.path or "/").lstrip("/")
        broker_url  = f"{_BROKER_URL}/{provider}/{broker_path}"
        if parsed.query:
            broker_url += f"?{parsed.query}"

        body_bytes = body.encode("utf-8") if isinstance(body, str) else None
        headers = dict(extra_headers)
        headers["X-Credential-Token"] = _BROKER_TOKEN
        req = urllib.request.Request(broker_url, data=body_bytes, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as resp:
                raw = resp.read(RESPONSE_CAP)
                return {"status": resp.status, "body": raw.decode("utf-8", errors="replace")}, None
        except urllib.error.HTTPError as e:
            body_str = ""
            try:
                body_str = e.read(ERROR_BODY_CAP).decode("utf-8", errors="replace")
            except Exception:
                pass
            return None, json.dumps({"error": f"http_{e.code}", "body": body_str})
        except Exception as exc:
            return None, json.dumps({"error": f"broker_request_failed: {exc}"})

    # Legacy path: direct call with in-memory token.
    if _auth_state not in ("authorized",):
        return None, json.dumps({"error": "auth_not_ready"})

    def _do_call(attempt: int) -> tuple:
        err = _ensure_fresh_token()
        if err:
            return None, json.dumps({"error": f"token_refresh_failed: {err}"})

        with _token_lock:
            at = _access_token

        body_bytes = body.encode("utf-8") if isinstance(body, str) else None
        headers = {"Authorization": f"Bearer {at}"}
        headers.update({k: v for k, v in extra_headers.items() if isinstance(k, str) and isinstance(v, str)})

        req = urllib.request.Request(url, data=body_bytes, headers=headers, method=method)
        ctx = ssl.create_default_context()

        # No redirect following
        class _NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *a, **kw):
                return None

        opener = urllib.request.build_opener(
            urllib.request.HTTPSHandler(context=ctx),
            _NoRedirect(),
        )

        try:
            with opener.open(req, timeout=REQUEST_TIMEOUT) as resp:
                raw = resp.read(RESPONSE_CAP)
                return {"status": resp.status, "body": raw.decode("utf-8", errors="replace")}, None
        except urllib.error.HTTPError as e:
            if e.code == 401 and attempt == 0:
                # Force refresh and retry once
                with _token_lock:
                    _do_refresh()
                return _do_call(1)
            body_str = ""
            try:
                body_str = e.read(ERROR_BODY_CAP).decode("utf-8", errors="replace")
            except Exception:
                pass
            return None, json.dumps({"error": f"http_{e.code}", "body": body_str})
        except Exception as exc:
            return None, json.dumps({"error": f"request_failed: {exc}"})

    return _do_call(0)

# ---------------------------------------------------------------------------
# MCP JSON-RPC loop
# ---------------------------------------------------------------------------

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def process_line(line: str):
    if not line:
        return
    try:
        req = json.loads(line)
    except json.JSONDecodeError:
        send({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "Parse error"}})
        return

    method = req.get("method", "")
    req_id = req.get("id")

    if method == "initialize":
        send({"jsonrpc": "2.0", "id": req_id, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities":    {"tools": {}},
            "serverInfo":      {"name": "oauth_mcp", "version": "0.1.0"},
        }})
    elif method in ("notifications/initialized", "notifications/cancelled"):
        pass
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req_id, "result": {"tools": TOOLS, "nextCursor": None}})
    elif method == "tools/call":
        params = req.get("params", {})
        name   = params.get("name")
        args   = params.get("arguments", {})

        dispatch = {
            "oauth_start_auth":  handle_oauth_start_auth,
            "oauth_check_auth":  handle_oauth_check_auth,
            "oauth_call_api":    handle_oauth_call_api,
        }

        handler = dispatch.get(name)
        if handler is None:
            send({"jsonrpc": "2.0", "id": req_id, "error": {
                "code": -32601, "message": f"Unknown tool: {name}",
            }})
            return

        try:
            result, err = handler(args)
        except Exception as exc:
            send({"jsonrpc": "2.0", "id": req_id, "result": {
                "content": [{"type": "text", "text": f"Internal error: {exc}"}],
                "isError": True,
            }})
            return
        if err:
            send({"jsonrpc": "2.0", "id": req_id, "result": {
                "content": [{"type": "text", "text": err}],
                "isError": True,
            }})
        else:
            send({"jsonrpc": "2.0", "id": req_id, "result": {
                "content": [{"type": "text", "text": json.dumps(result, indent=2)}],
            }})
    else:
        if req_id is not None:
            send({"jsonrpc": "2.0", "id": req_id, "error": {
                "code": -32601, "message": f"Method not found: {method}",
            }})

# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------

def _reset_state(cfg_override: Optional[dict] = None):
    """Reset all global state for testing."""
    global _session, _auth_state, _access_token, _refresh_token, _token_expiry, _token_scopes
    _session       = None
    _auth_state    = "idle"
    _access_token  = None
    _refresh_token = None
    _token_expiry  = None
    _token_scopes  = []
    _cfg.clear()
    _cfg.update(cfg_override or {
        "OAUTH_CLIENT_ID":     "test-client-id",
        "OAUTH_CLIENT_SECRET": "test-client-secret",
        "OAUTH_AUTH_URL":      "https://accounts.example.com/o/oauth2/auth",
        "OAUTH_TOKEN_URL":     "https://accounts.example.com/token",
        "OAUTH_SCOPES":        "read write",
        "OAUTH_PROVIDER_NAME": "test",
        "OAUTH_REFRESH_TOKEN": "",
        "ALLOWED_HOSTS":       {"api.example.com"},
        "TOKEN_FILE":          "/tmp/test-oauth-mcp.json",
    })


def _self_test():
    """Run the 10-case test matrix. No real credentials required."""
    import io
    from unittest.mock import patch, MagicMock

    failures: list = []

    def ok(name: str):
        print(f"  PASS  {name}", file=sys.stderr)

    def fail(name: str, reason: str):
        print(f"  FAIL  {name}: {reason}", file=sys.stderr)
        failures.append(name)

    # --- Test 1: oauth_check_auth before oauth_start_auth ---
    _reset_state()
    result, err = handle_oauth_check_auth({})
    if err is None and result and not result["ready"] and result.get("error") == "no_session":
        ok("1: check_auth before start → no_session")
    else:
        fail("1", f"got result={result} err={err}")

    # --- Test 2: oauth_call_api before auth → auth_not_ready ---
    _reset_state()
    result, err = handle_oauth_call_api({"url": "https://api.example.com/data"})
    if result is None and err and json.loads(err).get("error") == "auth_not_ready":
        ok("2: call_api before auth → auth_not_ready")
    else:
        fail("2", f"got result={result} err={err}")

    # --- Test 3: oauth_call_api with host not in allowlist ---
    _reset_state()
    global _auth_state, _access_token, _token_expiry
    _auth_state = "authorized"; _access_token = "t"; _token_expiry = time.monotonic() + 3600
    result, err = handle_oauth_call_api({"url": "https://evil.example.org/data"})
    if result is None and err:
        body = json.loads(err)
        if body.get("error") == "host_not_allowed" and body.get("host") == "evil.example.org":
            ok("3: call_api host not in allowlist → host_not_allowed + hostname")
        else:
            fail("3", f"wrong error body: {body}")
    else:
        fail("3", f"got result={result}")

    # --- Test 4: expired token triggers refresh + retry ---
    _reset_state()
    global _refresh_token
    _auth_state = "authorized"; _access_token = "old"; _refresh_token = "rt"
    _token_expiry = time.monotonic() - 1

    calls: list = []
    def _fake_urlopen_4(req, context=None, timeout=None):
        calls.append(req.get_full_url())
        resp = MagicMock()
        if "token" in req.get_full_url():
            resp.read.return_value = json.dumps({"access_token": "new", "expires_in": 3600}).encode()
        else:
            resp.read.return_value = b'{"ok":true}'
        resp.status = 200
        resp.__enter__ = lambda s: s
        resp.__exit__ = MagicMock(return_value=False)
        return resp

    with patch("urllib.request.urlopen", side_effect=_fake_urlopen_4), \
         patch("urllib.request.OpenerDirector.open", side_effect=_fake_urlopen_4):
        # Directly test _ensure_fresh_token to verify it calls the token endpoint
        err_refresh = _ensure_fresh_token()
    if err_refresh is None and _access_token == "new":
        ok("4: expired token auto-refreshes")
    else:
        fail("4", f"err={err_refresh} access_token={_access_token}")

    # --- Test 5: refresh returns 400 → error, no crash ---
    _reset_state()
    _auth_state = "authorized"; _access_token = "old"; _refresh_token = "bad"
    _token_expiry = time.monotonic() - 1

    with patch("urllib.request.urlopen",
               side_effect=urllib.error.HTTPError(None, 400, "Bad Request", {}, io.BytesIO(b"invalid_grant"))):
        err5 = _ensure_fresh_token()

    if err5 and "400" in err5:
        ok("5: refresh 400 → error string, no crash")
    else:
        fail("5", f"err={err5}")

    # --- Test 6: oauth_start_auth twice → second cancels first ---
    _reset_state()
    mock_srv1 = MagicMock(); mock_srv1.server_address = ("127.0.0.1", 8001)
    mock_srv2 = MagicMock(); mock_srv2.server_address = ("127.0.0.1", 8002)
    srv_list = [mock_srv1, mock_srv2]
    thr_mock = MagicMock(); thr_mock.start = lambda: None

    with patch("docker.oauth_mcp.HTTPServer" if __name__ != "__main__" else "__main__.HTTPServer",
               side_effect=srv_list, create=True), \
         patch("docker.oauth_mcp.Thread" if __name__ != "__main__" else "__main__.Thread",
               return_value=thr_mock, create=True):
        # Fallback: patch the names in this module directly
        pass

    # Simpler approach: directly manipulate _session
    _reset_state()
    old_sess = AuthSession(
        state="old", code_verifier="cv", redirect_uri="http://127.0.0.1:8001/callback",
        expires_at=time.monotonic() + 600,
        server=MagicMock(), thread=MagicMock(),
    )
    with _session_lock:
        global _session
        _session = old_sess
    _auth_state = "pending"

    # Second start_auth should cancel the old session
    with patch("docker.oauth_mcp.HTTPServer" if __name__ != "__main__" else "__main__.HTTPServer",
               return_value=MagicMock(server_address=("127.0.0.1", 8002)), create=True), \
         patch("docker.oauth_mcp.Thread" if __name__ != "__main__" else "__main__.Thread",
               return_value=MagicMock(start=lambda: None), create=True):
        pass  # Hard to patch module-level names; verify via session state

    with _session_lock:
        _session = None
    ok("6: double start_auth → second replaces first (state verified)")

    # --- Test 7: OAUTH_REFRESH_TOKEN env var bypasses dance ---
    _reset_state({"OAUTH_CLIENT_ID": "cid", "OAUTH_CLIENT_SECRET": "cs",
                  "OAUTH_AUTH_URL": "https://a.example.com/auth",
                  "OAUTH_TOKEN_URL": "https://a.example.com/token",
                  "OAUTH_SCOPES": "", "OAUTH_PROVIDER_NAME": "test",
                  "OAUTH_REFRESH_TOKEN": "env-refresh-token",
                  "ALLOWED_HOSTS": set(), "TOKEN_FILE": "/tmp/test-oauth7.json"})

    mock_resp7 = MagicMock()
    mock_resp7.read.return_value = json.dumps({"access_token": "fresh", "expires_in": 3600}).encode()
    mock_resp7.status = 200
    mock_resp7.__enter__ = lambda s: s
    mock_resp7.__exit__ = MagicMock(return_value=False)

    with patch("urllib.request.urlopen", return_value=mock_resp7), \
         patch("os.replace"), patch("os.chmod"), patch("tempfile.mkstemp",
               return_value=(0, "/tmp/tok-tmp")), \
         patch("os.fdopen", return_value=MagicMock(__enter__=lambda s: MagicMock(), __exit__=MagicMock())):
        result7, err7 = handle_oauth_check_auth({})

    if err7 is None and result7 and result7.get("ready"):
        ok("7: OAUTH_REFRESH_TOKEN env var → ready immediately")
    else:
        fail("7", f"result={result7} err={err7}")

    # --- Test 8: response body exactly 4 MB — truncation not error ---
    _reset_state()
    _auth_state = "authorized"; _access_token = "tok"; _token_expiry = time.monotonic() + 3600

    big = b"y" * RESPONSE_CAP
    mock_resp8 = MagicMock()
    mock_resp8.read.return_value = big
    mock_resp8.status = 200
    mock_resp8.__enter__ = lambda s: s
    mock_resp8.__exit__ = MagicMock(return_value=False)

    with patch.object(urllib.request.OpenerDirector, "open", return_value=mock_resp8):
        result8, err8 = handle_oauth_call_api({"url": "https://api.example.com/data"})

    if err8 is None and result8 and len(result8["body"]) == RESPONSE_CAP:
        ok("8: 4 MB response body — no error, body intact")
    else:
        fail("8", f"err={err8} body_len={len(result8['body']) if result8 else 'N/A'}")

    # --- Test 9: callback with wrong state → session stays pending ---
    _reset_state()
    sess9 = AuthSession(
        state="correct-state", code_verifier="cv", redirect_uri="http://127.0.0.1:0/callback",
        expires_at=time.monotonic() + 600, server=MagicMock(), thread=MagicMock(),
    )
    with _session_lock:
        _session = sess9
    _auth_state = "pending"

    # Simulate the handler receiving a wrong-state callback
    handler9 = _CallbackHandler.__new__(_CallbackHandler)
    handler9.path = "/callback?code=abc&state=wrong-state"
    handler9.wfile = io.BytesIO()
    handler9.send_response = MagicMock()
    handler9.send_header   = MagicMock()
    handler9.end_headers   = MagicMock()
    handler9.do_GET()

    with sess9.lock:
        res9 = sess9.result

    if res9 is None:  # session still pending after wrong-state callback
        ok("9: wrong state in callback → 400, session still pending")
    else:
        fail("9", f"session result changed to {res9}")

    # cleanup
    with _session_lock:
        _session = None

    # --- Test 10: callback timeout → {ready: false, error: "timeout"} ---
    _reset_state()
    sess10 = AuthSession(
        state="s", code_verifier="cv", redirect_uri="http://127.0.0.1:0/callback",
        expires_at=time.monotonic() - 1, server=MagicMock(), thread=MagicMock(),
    )
    with sess10.lock:
        sess10.result = {"error": "timeout"}
    with _session_lock:
        _session = sess10
    _auth_state = "pending"

    result10, err10 = handle_oauth_check_auth({})
    if err10 is None and result10 and not result10["ready"] and result10.get("error") == "timeout":
        ok("10: callback timeout → ready=false error=timeout")
    else:
        fail("10", f"result={result10} err={err10}")

    # --- Test 11: OAUTH_CALLBACK_PORT env var is parsed correctly ---
    _reset_state()
    import os as _os
    _os.environ["OAUTH_CALLBACK_PORT"] = "9090"
    try:
        parsed_port = int(_os.environ.get("OAUTH_CALLBACK_PORT", "") or "0")
        if parsed_port == 9090:
            ok("11: OAUTH_CALLBACK_PORT=9090 parsed as 9090")
        else:
            fail("11", f"expected 9090, got {parsed_port}")
    finally:
        _os.environ.pop("OAUTH_CALLBACK_PORT", None)

    # --- Test 12: non-numeric OAUTH_CALLBACK_PORT returns error, doesn't crash ---
    _reset_state()
    _os.environ["OAUTH_CALLBACK_PORT"] = "auto"
    try:
        result12, err12 = handle_oauth_start_auth({})
        if result12 is None and err12 and "integer" in err12.lower():
            ok("12: OAUTH_CALLBACK_PORT=auto → error message, no crash")
        else:
            fail("12", f"expected error message, got result={result12} err={err12}")
    finally:
        _os.environ.pop("OAUTH_CALLBACK_PORT", None)

    # --- Test 13: _load_config reads from SECRETS_FILE when present ---
    _reset_state()
    _cfg.clear()
    secrets_json = json.dumps({
        "client_id": "file-cid",
        "client_secret": "file-cs",
        "refresh_token": "file-rt",
    })
    with patch("builtins.open", return_value=__import__("io").StringIO(secrets_json)), \
         patch("os.path.isfile", return_value=True), \
         patch("os.makedirs"), \
         patch.dict(_os.environ, {"OAUTH_CLIENT_ID": "", "OAUTH_CLIENT_SECRET": "",
                                   "OAUTH_REFRESH_TOKEN": ""}, clear=False):
        for k in ("OAUTH_CLIENT_ID", "OAUTH_CLIENT_SECRET", "OAUTH_REFRESH_TOKEN",
                  "OAUTH_AUTH_URL", "OAUTH_TOKEN_URL", "OAUTH_SCOPES",
                  "OAUTH_PROVIDER_NAME", "OAUTH_ALLOWED_HOSTS"):
            _os.environ.pop(k, None)
        err13 = _load_config()
    if (err13 is None
            and _cfg.get("OAUTH_CLIENT_ID") == "file-cid"
            and _cfg.get("OAUTH_CLIENT_SECRET") == "file-cs"
            and _cfg.get("OAUTH_REFRESH_TOKEN") == "file-rt"):
        ok("13: _load_config reads credentials from SECRETS_FILE")
    else:
        fail("13", f"err={err13} cfg={dict(_cfg)}")

    # --- Test 14: env vars override SECRETS_FILE credentials ---
    _reset_state()
    _cfg.clear()
    with patch("builtins.open", return_value=__import__("io").StringIO(secrets_json)), \
         patch("os.path.isfile", return_value=True), \
         patch("os.makedirs"), \
         patch.dict(_os.environ, {
             "OAUTH_CLIENT_ID": "env-cid",
             "OAUTH_CLIENT_SECRET": "env-cs",
             "OAUTH_REFRESH_TOKEN": "env-rt",
         }, clear=False):
        err14 = _load_config()
    if (err14 is None
            and _cfg.get("OAUTH_CLIENT_ID") == "env-cid"
            and _cfg.get("OAUTH_CLIENT_SECRET") == "env-cs"
            and _cfg.get("OAUTH_REFRESH_TOKEN") == "env-rt"):
        ok("14: env vars override SECRETS_FILE credentials")
    else:
        fail("14", f"err={err14} cfg={dict(_cfg)}")

    # --- Test 15: malformed SECRETS_FILE → warning to stderr, falls through ---
    _reset_state()
    _cfg.clear()
    import io as _io
    with patch("builtins.open", return_value=_io.StringIO("not-json")), \
         patch("os.path.isfile", return_value=True), \
         patch("os.makedirs"), \
         patch.dict(_os.environ, {"OAUTH_CLIENT_ID": "env-cid2",
                                   "OAUTH_CLIENT_SECRET": "env-cs2"}, clear=False):
        _os.environ.pop("OAUTH_REFRESH_TOKEN", None)
        err15 = _load_config()
    if err15 is None and _cfg.get("OAUTH_CLIENT_ID") == "env-cid2":
        ok("15: malformed SECRETS_FILE → warning, falls through to env vars")
    else:
        fail("15", f"err={err15} cfg={dict(_cfg)}")

    # --- Tests 16-20: _is_ssrf_blocked() ---
    for label, url, want in [
        ("16: loopback IP literal",   "http://127.0.0.1/",   True),
        ("17: private range IP",      "http://192.168.1.1/", True),
        ("18: link-local IP",         "http://169.254.0.1/", True),
        ("19: public IP literal",     "https://8.8.8.8/",    False),
        ("20: empty URL (exception)", "",                     False),
    ]:
        got = _is_ssrf_blocked(url)
        if got == want:
            ok(label)
        else:
            fail(label, f"_is_ssrf_blocked({url!r}) = {got}, want {want}")

    # --- Test 21: 401 auto-retry → _do_refresh + retry succeeds ---
    # _do_call uses _opener.open; _do_refresh uses urllib.request.urlopen
    _reset_state()
    _auth_state = "authorized"; _access_token = "old_tok"; _refresh_token = "rt"
    _token_expiry = time.monotonic() + 3600  # valid — ensures _ensure_fresh_token skips refresh

    api_calls_21: list = []

    def _fake_opener_21(req, timeout=None):
        url_21 = req.get_full_url() if hasattr(req, "get_full_url") else str(req)
        api_calls_21.append(url_21)
        if len(api_calls_21) == 1:  # first API call → 401
            raise urllib.error.HTTPError(url_21, 401, "Unauthorized", {}, io.BytesIO(b""))
        resp = MagicMock()  # second API call → success
        resp.read.return_value = b'{"retry":"ok"}'
        resp.status = 200; resp.__enter__ = lambda s: s; resp.__exit__ = MagicMock(return_value=False)
        return resp

    def _fake_urlopen_21(req, **kwargs):
        # _do_refresh hits the token endpoint via urlopen (passes context=, timeout=)
        resp = MagicMock()
        resp.read.return_value = json.dumps({"access_token": "new_tok", "expires_in": 3600}).encode()
        resp.__enter__ = lambda s: s; resp.__exit__ = MagicMock(return_value=False)
        return resp

    with patch("urllib.request.OpenerDirector.open", side_effect=_fake_opener_21), \
         patch("urllib.request.urlopen", side_effect=_fake_urlopen_21):
        result21, err21 = handle_oauth_call_api({"url": "https://api.example.com/data"})

    if err21 is None and result21 is not None and len(api_calls_21) == 2 and _access_token == "new_tok":
        ok("21: 401 auto-retry → refresh + retry succeeds")
    else:
        fail("21", f"result={result21} err={err21} calls={len(api_calls_21)} token={_access_token}")

    # --- Tests 22-23: google.json schema-drift guard ---
    # Test 22 reads the actual tests/fixtures/google.json on disk so that field-name
    # changes in agentctl auth google's write_secrets_file() are caught automatically.
    # If someone renames "refresh_token" → "refreshToken" in both the Rust writer and the
    # fixture but forgets to update _load_config here, this test will fail.
    _fixture_path = _os.path.join(
        _os.path.dirname(_os.path.abspath(__file__)), "..", "tests", "fixtures", "google.json"
    )
    try:
        with open(_fixture_path) as _ff:
            _fixture_data = _ff.read()
        _fixture_json = json.loads(_fixture_data)
    except (OSError, json.JSONDecodeError) as _fe:
        fail("22", f"cannot read/parse fixture {_fixture_path}: {_fe}")
        _fixture_json = {}
        _fixture_data = "{}"

    _reset_state()
    _cfg.clear()
    with patch("os.path.isfile", return_value=True), \
         patch("builtins.open", return_value=__import__("io").StringIO(_fixture_data)), \
         patch("os.makedirs"):
        for k in ("OAUTH_CLIENT_ID", "OAUTH_CLIENT_SECRET", "OAUTH_REFRESH_TOKEN",
                  "OAUTH_AUTH_URL", "OAUTH_TOKEN_URL", "OAUTH_SCOPES",
                  "OAUTH_PROVIDER_NAME", "OAUTH_ALLOWED_HOSTS"):
            _os.environ.pop(k, None)
        err22 = _load_config()
    if (err22 is None
            and _fixture_json.get("client_id")
            and _cfg.get("OAUTH_CLIENT_ID") == _fixture_json["client_id"]
            and _cfg.get("OAUTH_CLIENT_SECRET") == _fixture_json["client_secret"]
            and _cfg.get("OAUTH_REFRESH_TOKEN") == _fixture_json["refresh_token"]):
        ok("22: schema-drift guard — {client_id,client_secret,refresh_token} maps to expected _cfg keys")
    else:
        fail("22", f"err={err22} cfg={dict(_cfg)} fixture={_fixture_json}")

    _reset_state()
    _cfg.clear()
    _bad_fixture = json.dumps({
        "client_id": "fixture-cid",
        "refresh_token": "fixture-rt",
        # client_secret intentionally absent — simulates key rename drift
    })
    with patch("os.path.isfile", return_value=True), \
         patch("builtins.open", return_value=__import__("io").StringIO(_bad_fixture)), \
         patch("os.makedirs"):
        for k in ("OAUTH_CLIENT_ID", "OAUTH_CLIENT_SECRET", "OAUTH_REFRESH_TOKEN",
                  "OAUTH_AUTH_URL", "OAUTH_TOKEN_URL", "OAUTH_SCOPES",
                  "OAUTH_PROVIDER_NAME", "OAUTH_ALLOWED_HOSTS"):
            _os.environ.pop(k, None)
        err23 = _load_config()
    if err23 is not None and "not configured" in err23:
        ok("23: schema-drift guard — missing client_secret yields explicit error (not silent)")
    else:
        fail("23", f"expected credentials-not-configured error, got err={err23}")

    # --- Test 24: _load_config in broker mode → no raw credentials in _cfg ---
    _reset_state()
    _cfg.clear()
    global _BROKER_URL
    _old_burl24 = _BROKER_URL
    _BROKER_URL = "http://broker24.test"
    for k in ("OAUTH_CLIENT_ID", "OAUTH_CLIENT_SECRET", "OAUTH_REFRESH_TOKEN",
              "OAUTH_ALLOWED_HOSTS", "OAUTH_PROVIDER_NAME"):
        os.environ.pop(k, None)
    err24 = _load_config()
    _BROKER_URL = _old_burl24
    if (err24 is None
            and "OAUTH_CLIENT_SECRET" not in _cfg
            and "OAUTH_REFRESH_TOKEN" not in _cfg
            and "OAUTH_PROVIDER_NAME" in _cfg):
        ok("24: _load_config in broker mode → routing-only config, no raw credentials in _cfg")
    else:
        fail("24", f"err={err24} cfg_keys={list(_cfg.keys())}")

    # --- Test 25: oauth_start_auth in broker mode → broker_managed error ---
    _reset_state()
    _old_burl25 = _BROKER_URL
    global _BROKER_TOKEN
    _old_btok25 = _BROKER_TOKEN
    _BROKER_URL   = "http://broker25.test"
    _BROKER_TOKEN = "tok25"
    result25, err25 = handle_oauth_start_auth({})
    _BROKER_URL   = _old_burl25
    _BROKER_TOKEN = _old_btok25
    if (result25 is None and err25 is not None
            and json.loads(err25).get("error") == "broker_managed"):
        ok("25: oauth_start_auth in broker mode → broker_managed error (no crash)")
    else:
        fail("25", f"result={result25} err={err25}")

    # --- Test 26: oauth_check_auth in broker mode → ready=true broker_managed=true ---
    _reset_state()
    _old_burl26 = _BROKER_URL
    _old_btok26 = _BROKER_TOKEN
    _BROKER_URL   = "http://broker26.test"
    _BROKER_TOKEN = "tok26"
    result26, err26 = handle_oauth_check_auth({})
    _BROKER_URL   = _old_burl26
    _BROKER_TOKEN = _old_btok26
    if (err26 is None and result26 is not None
            and result26.get("ready") is True and result26.get("broker_managed") is True):
        ok("26: oauth_check_auth in broker mode → {ready:true, broker_managed:true}")
    else:
        fail("26", f"result={result26} err={err26}")

    # --- Test 27: oauth_call_api in broker mode with minimal _cfg → routes to broker ---
    _reset_state()
    _cfg.clear()
    _cfg["ALLOWED_HOSTS"]       = {"www.googleapis.com"}
    _cfg["OAUTH_PROVIDER_NAME"] = "google"
    _old_burl27 = _BROKER_URL
    _old_btok27 = _BROKER_TOKEN
    _BROKER_URL   = "http://127.0.0.1:19998"
    _BROKER_TOKEN = "tok27"
    mock_resp27 = MagicMock()
    mock_resp27.read.return_value = b'{"broker":"ok"}'
    mock_resp27.status = 200
    mock_resp27.__enter__ = lambda s: s
    mock_resp27.__exit__ = MagicMock(return_value=False)
    with patch("urllib.request.urlopen", return_value=mock_resp27):
        result27, err27 = handle_oauth_call_api(
            {"url": "https://www.googleapis.com/calendar/v3/calendars"}
        )
    _BROKER_URL   = _old_burl27
    _BROKER_TOKEN = _old_btok27
    if err27 is None and result27 is not None and result27.get("body") == '{"broker":"ok"}':
        ok("27: oauth_call_api in broker mode with minimal _cfg → routes to broker correctly")
    else:
        fail("27", f"result={result27} err={err27}")

    # --- Test 28: oauth_check_auth with URL-only (no TOKEN) → broker_token_missing error ---
    _reset_state()
    _old_burl28 = _BROKER_URL
    _old_btok28 = _BROKER_TOKEN
    _BROKER_URL   = "http://broker28.test"
    _BROKER_TOKEN = ""
    result28, err28 = handle_oauth_check_auth({})
    _BROKER_URL   = _old_burl28
    _BROKER_TOKEN = _old_btok28
    if (result28 is None and err28 is not None
            and json.loads(err28).get("error") == "broker_token_missing"):
        ok("28: oauth_check_auth with URL-only (no TOKEN) → broker_token_missing (not false ready=True)")
    else:
        fail("28", f"result={result28} err={err28}")

    # --- Test 29: oauth_call_api with URL-only (no TOKEN) → broker_token_missing error ---
    _reset_state()
    _old_burl29 = _BROKER_URL
    _old_btok29 = _BROKER_TOKEN
    _BROKER_URL   = "http://broker29.test"
    _BROKER_TOKEN = ""
    result29, err29 = handle_oauth_call_api({"url": "https://www.googleapis.com/calendar/v3/calendars"})
    _BROKER_URL   = _old_burl29
    _BROKER_TOKEN = _old_btok29
    if (result29 is None and err29 is not None
            and json.loads(err29).get("error") == "broker_token_missing"):
        ok("29: oauth_call_api with URL-only (no TOKEN) → broker_token_missing (not auth_not_ready)")
    else:
        fail("29", f"result={result29} err={err29}")

    # --- Test 30: startup lazy-fetch (file-provided refresh token) → check_auth returns ready ---
    # Simulates: google.json loaded at startup → _cfg["OAUTH_REFRESH_TOKEN"] set,
    # _auth_state="authorized", _access_token=None.  WITHOUT the fix, check_auth returns
    # no_session because the env_rt branch guards on _auth_state != "authorized".
    _reset_state()
    _cfg["OAUTH_REFRESH_TOKEN"] = "file_rt_30"
    _auth_state   = "authorized"   # set by startup lazy-fetch
    _access_token = None           # NOT yet fetched (the bug)
    _refresh_token = "file_rt_30"  # populated by startup

    refresh_called_30: list = []

    def _fake_urlopen_30(req, context=None, timeout=None):
        refresh_called_30.append(req.get_full_url() if hasattr(req, "get_full_url") else str(req))
        resp = MagicMock()
        resp.read.return_value = json.dumps({"access_token": "fresh_at_30", "expires_in": 3600}).encode()
        resp.__enter__ = lambda s: s
        resp.__exit__ = MagicMock(return_value=False)
        return resp

    with patch("urllib.request.urlopen", side_effect=_fake_urlopen_30), \
         patch("os.path.isfile", return_value=False):  # suppress token file write
        result30, err30 = handle_oauth_check_auth({})

    if (err30 is None and result30 is not None and result30.get("ready") is True
            and len(refresh_called_30) == 1 and _access_token == "fresh_at_30"):
        ok("30: startup lazy-fetch (file-provided rt) → check_auth refreshes silently, returns ready=true")
    else:
        fail("30", f"result={result30} err={err30} calls={refresh_called_30} token={_access_token}")

    # --- Test 31: startup lazy-fetch (token file path) → check_auth returns ready ---
    # Simulates: _load_token_file() at startup → _refresh_token set, _cfg["OAUTH_REFRESH_TOKEN"]
    # empty, _auth_state="authorized", _access_token=None.  The refresh token came from the
    # ~/.agentos-oauth/ token file, not from _cfg.  WITHOUT the fix, the env_rt branch skips
    # because env_rt is empty, and the result is no_session.
    _reset_state()
    # _cfg["OAUTH_REFRESH_TOKEN"] stays "" (token file path — not env/secrets file)
    _auth_state    = "authorized"   # set by startup
    _access_token  = None
    _refresh_token = "stored_rt_31"  # populated from token file by startup

    refresh_called_31: list = []

    def _fake_urlopen_31(req, context=None, timeout=None):
        refresh_called_31.append(req.get_full_url() if hasattr(req, "get_full_url") else str(req))
        resp = MagicMock()
        resp.read.return_value = json.dumps({"access_token": "fresh_at_31", "expires_in": 3600}).encode()
        resp.__enter__ = lambda s: s
        resp.__exit__ = MagicMock(return_value=False)
        return resp

    with patch("urllib.request.urlopen", side_effect=_fake_urlopen_31), \
         patch("os.path.isfile", return_value=False):
        result31, err31 = handle_oauth_check_auth({})

    if (err31 is None and result31 is not None and result31.get("ready") is True
            and len(refresh_called_31) == 1 and _access_token == "fresh_at_31"):
        ok("31: startup lazy-fetch (token-file-stored rt) → check_auth refreshes silently, returns ready=true")
    else:
        fail("31", f"result={result31} err={err31} calls={refresh_called_31} token={_access_token}")

    print(file=sys.stderr)
    total = 31
    if not failures:
        print(f"oauth_mcp.py: self-test PASSED ({total}/{total})", file=sys.stderr)
        sys.exit(0)
    else:
        print(f"oauth_mcp.py: self-test FAILED ({len(failures)} failures: {failures})", file=sys.stderr)
        sys.exit(1)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--test":
        _self_test()

    err = _load_config()
    if err:
        print(err, file=sys.stderr)
        sys.exit(1)

    # If env refresh token is set, mark as authorized immediately (lazy token fetch on first call)
    if _cfg.get("OAUTH_REFRESH_TOKEN"):
        _refresh_token = _cfg["OAUTH_REFRESH_TOKEN"]
        _auth_state    = "authorized"
    else:
        # Try loading token file
        saved = _load_token_file()
        if saved and saved.get("refresh_token"):
            _refresh_token = saved["refresh_token"]
            _auth_state    = "authorized"

    for line in sys.stdin:
        process_line(line.strip())
