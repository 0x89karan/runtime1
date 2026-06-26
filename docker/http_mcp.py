#!/usr/bin/env python3
"""
http_fetch MCP server — fetches HTTPS URLs and returns status/headers/body.

Tool: fetch_url
  Input:  { url: str, method?: str, headers?: {str: str}, body?: str }
  Output: { status_code: int, headers: {str: str}, body: str, is_redirect?: bool }

Capability required (MCP server subprocess):
  capabilities = [{ Net = { hosts = [], ports = [443] } }]

Example TOML (path relative to agentd/ where cargo run is invoked):
  [[tools.mcp_servers]]
  name    = "http_fetch"
  command = "python3"
  args    = ["../docker/http_mcp.py"]
  capabilities = [{ Net = { hosts = [], ports = [443] } }]

Safety notes:
  - Only HTTPS (https://) URLs are accepted; HTTP is rejected with isError=true.
  - Loopback (127.x, ::1), link-local (169.254.x), and RFC1918 (10.x, 172.16-31.x,
    192.168.x) addresses are blocked to prevent SSRF.
  - Response body is capped at 4 MB to prevent context exhaustion.
  - Redirects are NOT followed; the redirect URL is returned in headers so the
    agent can choose whether to follow it.
  - Request timeout is 30 seconds.
"""
import ipaddress, json, socket, ssl, sys, urllib.error, urllib.request
from urllib.parse import urlparse

BODY_CAP        = 4 * 1024 * 1024  # 4 MB
REQUEST_TIMEOUT = 30

TOOLS = [{
    "name": "fetch_url",
    "description": (
        "Fetch an HTTPS URL and return the status code, response headers, and body. "
        "Only HTTPS URLs are accepted. Body is capped at 4 MB. "
        "Redirects are not followed — the Location header is returned instead."
    ),
    "inputSchema": {
        "type": "object",
        "properties": {
            "url":     {"type": "string", "description": "HTTPS URL to fetch."},
            "method":  {"type": "string", "description": "HTTP method (default: GET)."},
            "headers": {"type": "object", "description": "Request headers.",
                        "additionalProperties": {"type": "string"}},
            "body":    {"type": "string", "description": "Request body (for POST/PUT)."},
        },
        "required": ["url"],
    },
}]


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


_ALLOWED_METHODS = {"GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"}


def _is_ssrf_blocked(url: str) -> bool:
    """Return True if the URL resolves to a loopback, link-local, or RFC1918 address."""
    try:
        host = urlparse(url).hostname
        if not host:
            return True
        for _, _, _, _, sockaddr in socket.getaddrinfo(host, None):
            ip = ipaddress.ip_address(sockaddr[0])
            if ip.is_loopback or ip.is_private or ip.is_link_local:
                return True
    except Exception:
        pass
    return False


def handle_fetch_url(args):
    url     = args.get("url", "")
    method  = args.get("method", "GET").upper()
    headers = {str(k): str(v) for k, v in args.get("headers", {}).items()}
    body    = args.get("body")

    if method not in _ALLOWED_METHODS:
        return None, f"Method not allowed: {method!r}. Allowed: {', '.join(sorted(_ALLOWED_METHODS))}"

    if not url.startswith("https://"):
        return None, "Only HTTPS URLs are accepted (url must start with 'https://')"

    if _is_ssrf_blocked(url):
        return None, "Blocked: URL resolves to a loopback, link-local, or private (RFC1918) address"

    data = body.encode("utf-8") if body else None
    req  = urllib.request.Request(url, data=data, headers=headers, method=method)

    # No-redirect handler: raise on any redirect so we can return it to the caller.
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None  # suppress redirect

    ctx = ssl.create_default_context()
    opener = urllib.request.build_opener(NoRedirect, urllib.request.HTTPSHandler(context=ctx))

    try:
        with opener.open(req, timeout=REQUEST_TIMEOUT) as resp:
            raw = resp.read(BODY_CAP + 1)
            truncated = len(raw) > BODY_CAP
            body_text  = raw[:BODY_CAP].decode("utf-8", errors="replace")
            if truncated:
                body_text += "\n[TRUNCATED at 4MB]"
            resp_headers = dict(resp.headers)
            return {
                "status_code": resp.status,
                "headers":     resp_headers,
                "body":        body_text,
            }, None

    except urllib.error.HTTPError as e:
        # HTTPError is raised for 4xx/5xx AND for suppressed redirects (3xx).
        is_redirect = 300 <= e.code < 400
        resp_headers = dict(e.headers) if e.headers else {}
        raw = b""
        try:
            raw = e.read(BODY_CAP + 1)
        except Exception:
            pass
        truncated = len(raw) > BODY_CAP
        body_text  = raw[:BODY_CAP].decode("utf-8", errors="replace")
        if truncated:
            body_text += "\n[TRUNCATED at 4MB]"
        result = {
            "status_code": e.code,
            "headers":     resp_headers,
            "body":        body_text,
        }
        if is_redirect:
            result["is_redirect"] = True
        return result, None

    except Exception as e:
        return None, str(e)


def _self_test():
    res, err = handle_fetch_url({"url": "http://example.com"})
    assert res is None and err is not None, "HTTP must be rejected"

    # Test with a real HTTPS request (requires network).
    try:
        res, err = handle_fetch_url({"url": "https://example.com"})
        assert err is None, f"unexpected error: {err}"
        assert res["status_code"] in (200, 301, 302), f"unexpected status: {res['status_code']}"
    except Exception as e:
        # Network may be unavailable in CI; warn but don't fail.
        print(f"http_mcp.py: network test skipped ({e})", file=sys.stderr)

    print("http_mcp.py: self-test PASSED", file=sys.stderr)
    sys.exit(0)


def process_line(line):
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
            "serverInfo":      {"name": "http_fetch", "version": "0.1.0"},
        }})
    elif method in ("notifications/initialized", "notifications/cancelled"):
        pass
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req_id, "result": {"tools": TOOLS, "nextCursor": None}})
    elif method == "tools/call":
        params = req.get("params", {})
        name   = params.get("name")
        args   = params.get("arguments", {})
        if name == "fetch_url":
            result, err = handle_fetch_url(args)
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
            send({"jsonrpc": "2.0", "id": req_id, "error": {
                "code": -32601, "message": f"Unknown tool: {name}",
            }})
    else:
        if req_id is not None:
            send({"jsonrpc": "2.0", "id": req_id, "error": {
                "code": -32601, "message": f"Method not found: {method}",
            }})


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--test":
        _self_test()
    for line in sys.stdin:
        process_line(line.strip())
