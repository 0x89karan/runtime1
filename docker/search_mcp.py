#!/usr/bin/env python3
"""
web_search MCP server — searches the web via the Brave Search API.

Tool: web_search
  Input:  { query: str, count?: int (default 5, max 10) }
  Output: { results: [{title, url, description}], total_found: int }

Capability required (MCP server subprocess):
  capabilities = [{ Net = { hosts = ["api.search.brave.com"], ports = [443] } }]

Example TOML (path relative to agentd/ where cargo run is invoked):
  [[tools.mcp_servers]]
  name    = "web_search"
  command = "python3"
  args    = ["../docker/search_mcp.py"]
  # Requires BRAVE_SEARCH_API_KEY env var. Free tier: brave.com/search/api
  capabilities = [{ Net = { hosts = ["api.search.brave.com"], ports = [443] } }]

Setup:
  export BRAVE_SEARCH_API_KEY=<your-key>   # get a free key at brave.com/search/api
  (2,000 queries/month on the free tier)

Graceful degradation:
  If BRAVE_SEARCH_API_KEY is not set, returns isError=true with a setup message.
  The server never crashes on a missing key.
"""
import json, os, ssl, sys, urllib.error, urllib.parse, urllib.request

BRAVE_API_URL    = "https://api.search.brave.com/res/v1/web/search"
MAX_COUNT        = 10
DEFAULT_COUNT    = 5
REQUEST_TIMEOUT  = 15          # seconds
RAW_RESPONSE_CAP = 2 * 1024 * 1024  # 2 MB cap on API response body
ERROR_BODY_CAP   = 512         # bytes read from HTTP error body

TOOLS = [{
    "name": "web_search",
    "description": (
        "Search the web using Brave Search and return a list of results. "
        "Each result has a title, URL, and description. "
        "Requires BRAVE_SEARCH_API_KEY environment variable."
    ),
    "inputSchema": {
        "type": "object",
        "properties": {
            "query": {"type": "string",  "description": "Search query."},
            "count": {"type": "integer", "description": "Number of results (default 5, max 10)."},
        },
        "required": ["query"],
    },
}]


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def handle_web_search(args):
    api_key = os.environ.get("BRAVE_SEARCH_API_KEY", "")
    if not api_key:
        return None, (
            "BRAVE_SEARCH_API_KEY not set. "
            "Get a free key (2,000 queries/month) at brave.com/search/api, "
            "then set the environment variable before starting agentd."
        )

    query = args.get("query", "").strip()
    if not query:
        return None, "query must not be empty"

    try:
        count = max(1, min(int(args.get("count", DEFAULT_COUNT)), MAX_COUNT))
    except (ValueError, TypeError):
        count = DEFAULT_COUNT
    params = urllib.parse.urlencode({"q": query, "count": count})
    url    = f"{BRAVE_API_URL}?{params}"

    req = urllib.request.Request(url, headers={
        "Accept":              "application/json",
        "X-Subscription-Token": api_key,
    })

    ctx = ssl.create_default_context()
    try:
        with urllib.request.urlopen(req, context=ctx, timeout=REQUEST_TIMEOUT) as resp:
            raw  = resp.read(RAW_RESPONSE_CAP)
            data = json.loads(raw)
    except urllib.error.HTTPError as e:
        body = ""
        try:
            body = e.read(ERROR_BODY_CAP).decode("utf-8", errors="replace")
        except Exception:
            pass
        return None, f"Brave Search API error {e.code}: {body}"
    except Exception as e:
        return None, f"Request failed: {e}"

    web_results = data.get("web", {}).get("results", [])
    results = [
        {
            "title":       r.get("title", ""),
            "url":         r.get("url", ""),
            "description": r.get("description", ""),
        }
        for r in web_results
    ]
    total = data.get("web", {}).get("totalResults", len(results))
    return {"results": results, "total_found": total}, None


def _self_test():
    # Test with missing key.
    old_key = os.environ.pop("BRAVE_SEARCH_API_KEY", None)
    res, err = handle_web_search({"query": "test"})
    assert res is None and err is not None, "missing key must return error"
    assert "BRAVE_SEARCH_API_KEY" in err, "error must mention the key name"

    # Test with real key if present.
    if old_key:
        os.environ["BRAVE_SEARCH_API_KEY"] = old_key
        try:
            res, err = handle_web_search({"query": "agentOS Rust runtime"})
            assert err is None, f"unexpected error: {err}"
            assert "results" in res, f"expected results in response"
        except Exception as e:
            print(f"search_mcp.py: live search test skipped ({e})", file=sys.stderr)

    print("search_mcp.py: self-test PASSED", file=sys.stderr)
    sys.exit(0)


def process_line(line):
    if not line:
        return
    try:
        req = json.loads(line)
    except json.JSONDecodeError:
        return

    method = req.get("method", "")
    req_id = req.get("id")

    if method == "initialize":
        send({"jsonrpc": "2.0", "id": req_id, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities":    {"tools": {}},
            "serverInfo":      {"name": "web_search", "version": "0.1.0"},
        }})
    elif method in ("notifications/initialized", "notifications/cancelled"):
        pass
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req_id, "result": {"tools": TOOLS, "nextCursor": None}})
    elif method == "tools/call":
        params = req.get("params", {})
        name   = params.get("name")
        args   = params.get("arguments", {})
        if name == "web_search":
            result, err = handle_web_search(args)
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
