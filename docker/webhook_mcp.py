#!/usr/bin/env python3
"""
webhook_mcp — fires wait_for_trigger() when an HTTP POST arrives.

Tool: wait_for_trigger
  Input:  { timeout_s?: int (default 25, max 25) }
  Output: { status: "fired"|"waiting"|"timeout",
            payload: str,
            event_id: str,
            message: str,
            rejected_count: int }

Configuration (env vars):
  TRIGGER_WEBHOOK_PORT    Port to listen on (default 9000).
  TRIGGER_WEBHOOK_HOST    Bind address (default 127.0.0.1).
  TRIGGER_WEBHOOK_SECRET  If set, validates HMAC-SHA256 signature in the
                          X-Hub-Signature-256 header (format: "sha256=<hex>").
  TRIGGER_MAX_WAIT_S      Abort after this many total seconds across all
                          calls (optional).

HTTP protocol:
  POST / (or any path)
    Headers:
      X-Timestamp: <Unix epoch seconds, integer> (always validated, ±5 min)
      X-Hub-Signature-256: sha256=<hex>  (required when TRIGGER_WEBHOOK_SECRET is set)
      Content-Type: application/json  (recommended)
    Body: arbitrary text/JSON (capped at 64 KB)
  Response:
    200: accepted
    400: missing or malformed required headers
    403: invalid HMAC or timestamp out of window
    413: body exceeds 64 KB
    429: internal queue full (max 10 events)
    500: server error

Security notes:
  - Timestamp tolerance: ±5 minutes (prevents replay attacks).
  - HMAC uses hmac.compare_digest to prevent timing oracle attacks.
  - Body capped at 64 KB via Content-Length pre-check before reading.
  - Default bind: 127.0.0.1 — loopback only.

Example TOML (adjust path to repo location):
  [[tools.mcp_servers]]
  name    = "webhook_trigger"
  command = "python3"
  args    = ["/usr/lib/agentos/docker/webhook_mcp.py"]
  passenv = ["TRIGGER_WEBHOOK_PORT", "TRIGGER_WEBHOOK_SECRET"]
  capabilities = [{ Net = { ports = [9000] } }]

Note: On checkpoint, agentd restarts this process. The in-memory event
queue is cleared. Events that arrived during the downtime window are lost.
For production use-cases requiring durability, write events to disk
in your webhook sender before POSTing.
"""
import hashlib, hmac, json, os, queue, sys, time, threading, uuid
from http.server import BaseHTTPRequestHandler, HTTPServer
from socketserver import ThreadingMixIn

MAX_TIMEOUT_S   = 25
DEFAULT_TIMEOUT = 25
BODY_CAP        = 64 * 1024   # 64 KB
QUEUE_MAXSIZE   = 10
TIMESTAMP_TOLERANCE_S = 300   # ±5 minutes

TOOLS = [{
    "name": "wait_for_trigger",
    "description": (
        "Block until an HTTP POST arrives at the configured webhook endpoint. "
        "Returns {status:'fired', payload:...} when a request is received, "
        "or {status:'waiting'} if the timeout_s window elapsed without one. "
        "Call this tool in a loop at the start of each turn until status=='fired'. "
        "rejected_count shows how many requests were rejected (bad HMAC/timestamp/etc)."
    ),
    "inputSchema": {
        "type": "object",
        "properties": {
            "timeout_s": {
                "type": "integer",
                "description": "Max seconds to wait per call (default 25, max 25).",
            },
        },
        "required": [],
    },
}]


# ---------------------------------------------------------------------------
# Server state
# ---------------------------------------------------------------------------

_event_queue:    queue.Queue = queue.Queue(maxsize=QUEUE_MAXSIZE)
_rejected_count: int         = 0
_rejected_lock               = threading.Lock()
_secret:         bytes       = b""
_max_wait_s                  = None
_wait_start                  = None


def _inc_rejected():
    global _rejected_count
    with _rejected_lock:
        _rejected_count += 1


def _get_rejected() -> int:
    with _rejected_lock:
        return _rejected_count


# ---------------------------------------------------------------------------
# HTTP handler
# ---------------------------------------------------------------------------

class _WebhookHandler(BaseHTTPRequestHandler):

    def log_message(self, fmt, *args):
        # Redirect HTTP server logs to stderr
        print(f"webhook_mcp.py: {fmt % args}", file=sys.stderr)

    def _send(self, code: int, body: str = ""):
        self.send_response(code)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body.encode())))
        self.end_headers()
        if body:
            self.wfile.write(body.encode())

    def do_POST(self):
        headers = self.headers

        # --- Validate timestamp (always, even without HMAC secret) ---
        ts_header = headers.get("X-Timestamp", "").strip()
        if not ts_header:
            _inc_rejected()
            self._send(400, "Missing X-Timestamp header")
            return
        try:
            req_ts = int(ts_header)
        except ValueError:
            _inc_rejected()
            self._send(400, "X-Timestamp must be an integer Unix epoch")
            return
        now_ts = int(time.time())
        if abs(now_ts - req_ts) > TIMESTAMP_TOLERANCE_S:
            _inc_rejected()
            self._send(403, f"Timestamp out of ±{TIMESTAMP_TOLERANCE_S}s window")
            return

        # --- Cap body size BEFORE reading (Content-Length bomb prevention) ---
        cl_header = headers.get("Content-Length", "0").strip()
        try:
            cl = int(cl_header)
        except ValueError:
            cl = 0
        if cl > BODY_CAP:
            _inc_rejected()
            self._send(413, f"Body exceeds {BODY_CAP} byte cap")
            return
        read_len = max(0, min(cl, BODY_CAP))
        try:
            body_bytes = self.rfile.read(read_len)
        except Exception as e:
            _inc_rejected()
            self._send(500, f"Read error: {e}")
            return
        body_str = body_bytes.decode("utf-8", errors="replace")

        # --- HMAC validation (only when secret is configured) ---
        if _secret:
            sig_header = headers.get("X-Hub-Signature-256", "").strip()
            if not sig_header.startswith("sha256="):
                _inc_rejected()
                self._send(403, "Missing or malformed X-Hub-Signature-256")
                return
            provided_sig = sig_header[7:]  # strip "sha256="
            expected_sig = hmac.new(_secret, body_bytes, hashlib.sha256).hexdigest()
            if not hmac.compare_digest(provided_sig, expected_sig):
                _inc_rejected()
                self._send(403, "HMAC signature mismatch")
                return

        # --- Enqueue ---
        event = {
            "payload":    body_str,
            "event_id":   str(uuid.uuid4()),
            "received_at": now_ts,
        }
        try:
            _event_queue.put_nowait(event)
            self._send(200, "Accepted")
        except queue.Full:
            _inc_rejected()
            self._send(429, "Queue full — try again later")

    def do_GET(self):
        # Health check endpoint for self-test
        if self.path in ("/", "/health"):
            body = "ok"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body.encode())
        else:
            self._send(404, "Not found")


class _ThreadingHTTPServer(ThreadingMixIn, HTTPServer):
    daemon_threads = True


# ---------------------------------------------------------------------------
# Server startup
# ---------------------------------------------------------------------------

def _init():
    global _secret, _max_wait_s

    port_raw   = (os.environ.get("TRIGGER_WEBHOOK_PORT") or "9000").strip()
    host       = (os.environ.get("TRIGGER_WEBHOOK_HOST") or "127.0.0.1").strip()
    secret_raw = (os.environ.get("TRIGGER_WEBHOOK_SECRET") or "").strip()
    max_raw    = (os.environ.get("TRIGGER_MAX_WAIT_S") or "").strip()

    try:
        port = int(port_raw)
        if not (1 <= port <= 65535):
            raise ValueError
    except ValueError:
        print(f"webhook_mcp.py: TRIGGER_WEBHOOK_PORT must be 1-65535, got '{port_raw}'", file=sys.stderr)
        sys.exit(1)

    if secret_raw:
        _secret = secret_raw.encode()

    if max_raw:
        try:
            _max_wait_s = int(max_raw)
            if _max_wait_s < 1:
                raise ValueError
        except ValueError:
            print(f"webhook_mcp.py: TRIGGER_MAX_WAIT_S must be a positive integer, got '{max_raw}'", file=sys.stderr)
            sys.exit(1)

    try:
        server = _ThreadingHTTPServer((host, port), _WebhookHandler)
    except OSError as e:
        print(f"webhook_mcp.py: cannot bind {host}:{port}: {e}", file=sys.stderr)
        sys.exit(1)
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    print(
        f"webhook_mcp.py: listening on {host}:{port} "
        f"(HMAC={'enabled' if _secret else 'disabled'} "
        f"max_wait={_max_wait_s or 'none'}s)",
        file=sys.stderr,
    )


# ---------------------------------------------------------------------------
# Tool implementation
# ---------------------------------------------------------------------------

def handle_wait_for_trigger(args: dict) -> dict:
    global _wait_start

    try:
        timeout_s = max(1, min(int(args.get("timeout_s", DEFAULT_TIMEOUT)), MAX_TIMEOUT_S))
    except (ValueError, TypeError):
        timeout_s = DEFAULT_TIMEOUT

    mono = time.monotonic()

    # Record first-call timestamp for max_wait_s accounting
    if _wait_start is None:
        _wait_start = mono

    # Check max_wait_s
    if _max_wait_s is not None:
        elapsed_total = mono - _wait_start
        if elapsed_total >= _max_wait_s:
            return {
                "status":         "timeout",
                "payload":        "",
                "event_id":       str(uuid.uuid4()),
                "message":        f"TRIGGER_MAX_WAIT_S={_max_wait_s}s exceeded",
                "rejected_count": _get_rejected(),
            }

    try:
        event = _event_queue.get(timeout=timeout_s)
        _wait_start = None  # reset for next wait cycle
        return {
            "status":         "fired",
            "payload":        event["payload"],
            "event_id":       event["event_id"],
            "message":        "Webhook received",
            "rejected_count": _get_rejected(),
        }
    except queue.Empty:
        return {
            "status":         "waiting",
            "payload":        "",
            "event_id":       str(uuid.uuid4()),
            "message":        f"No webhook received within {timeout_s}s",
            "rejected_count": _get_rejected(),
        }


# ---------------------------------------------------------------------------
# JSON-RPC stdio loop
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
            "serverInfo":      {"name": "webhook_trigger", "version": "0.1.0"},
        }})
    elif method in ("notifications/initialized", "notifications/cancelled"):
        pass
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req_id, "result": {"tools": TOOLS, "nextCursor": None}})
    elif method == "tools/call":
        params = req.get("params", {})
        name   = params.get("name")
        args   = params.get("arguments", {})
        if name == "wait_for_trigger":
            result = handle_wait_for_trigger(args)
            send({"jsonrpc": "2.0", "id": req_id, "result": {
                "content": [{"type": "text", "text": json.dumps(result, indent=2)}],
            }})
        else:
            send({"jsonrpc": "2.0", "id": req_id, "error": {
                "code": -32601, "message": f"Unknown tool: {name}",
            }})
    else:
        if req_id is not None:
            send({"jsonrpc": "2.0", "id": req_id, "error": {
                "code": -32601, "message": f"Method not found: {method}",
            }})


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------

def _self_test():
    import http.client, threading, time as _time

    print("webhook_mcp.py: running self-test ...", file=sys.stderr)

    # Use a high ephemeral port for testing
    TEST_PORT   = 19876
    TEST_SECRET = "test-secret-key"

    global _secret, _max_wait_s, _wait_start, _rejected_count
    _secret          = TEST_SECRET.encode()
    _max_wait_s      = None
    _wait_start      = None
    _rejected_count  = 0

    # Clear queue
    while not _event_queue.empty():
        try:
            _event_queue.get_nowait()
        except queue.Empty:
            break

    server = _ThreadingHTTPServer(("127.0.0.1", TEST_PORT), _WebhookHandler)
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    _time.sleep(0.1)

    def post(body: str, add_headers: dict = None, use_hmac: bool = True):
        conn = http.client.HTTPConnection("127.0.0.1", TEST_PORT, timeout=5)
        body_bytes = body.encode()
        now_ts = str(int(_time.time()))
        headers = {
            "Content-Type":   "application/json",
            "Content-Length": str(len(body_bytes)),
            "X-Timestamp":    now_ts,
        }
        if use_hmac:
            sig = hmac.new(TEST_SECRET.encode(), body_bytes, hashlib.sha256).hexdigest()
            headers["X-Hub-Signature-256"] = f"sha256={sig}"
        if add_headers:
            headers.update(add_headers)
        conn.request("POST", "/", body=body_bytes, headers=headers)
        resp = conn.getresponse()
        resp.read()
        conn.close()
        return resp.status

    # [1] Valid request fires
    def _fire_later():
        _time.sleep(0.3)
        post('{"hello": "world"}')
    threading.Thread(target=_fire_later, daemon=True).start()
    r = handle_wait_for_trigger({"timeout_s": 5})
    assert r["status"] == "fired", f"valid POST should fire: {r}"
    assert "hello" in r["payload"], f"payload mismatch: {r}"
    print("  [1/6] valid POST fires: PASS", file=sys.stderr)

    # [2] Invalid HMAC returns 403, does not queue
    before = _get_rejected()
    sc = post('{"bad": 1}', use_hmac=False)
    assert sc == 403, f"bad HMAC should return 403, got {sc}"
    assert _get_rejected() == before + 1, "rejected_count should increment"
    print("  [2/6] invalid HMAC rejected: PASS", file=sys.stderr)

    # [3] Old timestamp returns 403
    sc2 = post('{"ts": "old"}', add_headers={"X-Timestamp": "1000000"})
    assert sc2 == 403, f"old timestamp should return 403, got {sc2}"
    print("  [3/6] old timestamp rejected: PASS", file=sys.stderr)

    # [4] Body > 64 KB rejected with 413
    big_body = "x" * (BODY_CAP + 1)
    # We send Content-Length header > BODY_CAP; handler should reject before reading
    conn2 = http.client.HTTPConnection("127.0.0.1", TEST_PORT, timeout=5)
    now_ts = str(int(_time.time()))
    conn2.request("POST", "/", body=big_body.encode(), headers={
        "Content-Type":   "text/plain",
        "Content-Length": str(len(big_body) + 1),
        "X-Timestamp":    now_ts,
    })
    resp2 = conn2.getresponse()
    resp2.read()
    conn2.close()
    assert resp2.status == 413, f"oversized body should return 413, got {resp2.status}"
    print("  [4/6] oversized body rejected: PASS", file=sys.stderr)

    # [5] Queue full returns 429
    # Fill queue to capacity
    for i in range(QUEUE_MAXSIZE):
        sc3 = post(f'{{"n": {i}}}')
        assert sc3 == 200, f"fill queue item {i} failed: {sc3}"
    sc4 = post('{"overflow": true}')
    assert sc4 == 429, f"overflow should return 429, got {sc4}"
    print("  [5/6] queue full returns 429: PASS", file=sys.stderr)

    # Clear queue
    while not _event_queue.empty():
        try:
            _event_queue.get_nowait()
        except queue.Empty:
            break

    # [6] max_wait_s timeout
    _max_wait_s = 1
    _wait_start = _time.monotonic() - 5  # simulate 5s elapsed
    r6 = handle_wait_for_trigger({"timeout_s": 25})
    assert r6["status"] == "timeout", f"max_wait_s should return timeout: {r6}"
    print("  [6/6] max_wait_s timeout: PASS", file=sys.stderr)

    server.shutdown()
    print("webhook_mcp.py: self-test PASSED (6/6)", file=sys.stderr)
    sys.exit(0)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--test":
        _self_test()
    _init()
    for line in sys.stdin:
        process_line(line.strip())
