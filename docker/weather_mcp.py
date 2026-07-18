#!/usr/bin/env python3
"""Minimal weather MCP server — wraps wttr.in, no API key required."""
import json, sys, urllib.request

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

TOOLS = [{
    "name": "get_weather",
    "description": "Get current weather for any city using wttr.in. Returns temp, feels-like, description, humidity, and wind.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "city": {"type": "string", "description": "City name, e.g. 'New York City'"}
        },
        "required": ["city"]
    }
}]

if len(sys.argv) > 1 and sys.argv[1] == "--test":
    # Self-test (ci.1): no network, no stdin. CI asserts the PASSED marker on
    # stderr — exit codes alone are insufficient (an argv-less server EOFs on
    # /dev/null stdin and exits 0, which is exactly the false-pass this closes).
    import io
    checks = 0
    # T1: tool schema shape
    assert len(TOOLS) == 1 and TOOLS[0]["name"] == "get_weather", "T1 tool name"
    assert "city" in TOOLS[0]["inputSchema"]["properties"], "T1 schema city"
    assert TOOLS[0]["inputSchema"]["required"] == ["city"], "T1 required"
    checks += 1
    # T2: TOOLS serializes to valid JSON and round-trips
    assert json.loads(json.dumps(TOOLS)) == TOOLS, "T2 round-trip"
    checks += 1
    # T3: send() emits one valid JSONL line (finally: never leave stdout swapped)
    _real_stdout = sys.stdout
    try:
        sys.stdout = io.StringIO()
        send({"jsonrpc": "2.0", "id": 1, "result": {"ok": True}})
        _out = sys.stdout.getvalue()
    finally:
        sys.stdout = _real_stdout
    assert _out.endswith("\n") and json.loads(_out)["result"]["ok"] is True, "T3 send"
    checks += 1
    print(f"weather_mcp.py: self-test PASSED ({checks}/3)", file=sys.stderr)
    sys.exit(0)

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except json.JSONDecodeError:
        continue

    method = req.get("method", "")
    req_id = req.get("id")

    if method == "initialize":
        send({"jsonrpc": "2.0", "id": req_id, "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "weather", "version": "0.1.0"}
        }})
    elif method in ("notifications/initialized", "notifications/cancelled"):
        pass  # no response for notifications
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req_id, "result": {"tools": TOOLS, "nextCursor": None}})
    elif method == "tools/call":
        params  = req.get("params", {})
        name    = params.get("name")
        args    = params.get("arguments", {})
        if name == "get_weather":
            city = args.get("city", "New York").replace(" ", "+")
            try:
                url = f"https://wttr.in/{city}?format=j1"
                r   = urllib.request.urlopen(url, timeout=10)
                raw = json.loads(r.read())
                cur = raw["current_condition"][0]
                area = raw["nearest_area"][0]
                result = {
                    "city":          area["areaName"][0]["value"],
                    "country":       area["country"][0]["value"],
                    "temp_c":        int(cur["temp_C"]),
                    "temp_f":        int(cur["temp_F"]),
                    "feels_like_c":  int(cur["FeelsLikeC"]),
                    "feels_like_f":  int(cur["FeelsLikeF"]),
                    "description":   cur["weatherDesc"][0]["value"],
                    "humidity_pct":  int(cur["humidity"]),
                    "wind_mph":      int(cur["windspeedMiles"]),
                    "visibility_mi": int(cur["visibility"]),
                    "uv_index":      int(cur["uvIndex"]),
                }
                send({"jsonrpc": "2.0", "id": req_id, "result": {
                    "content": [{"type": "text", "text": json.dumps(result, indent=2)}]
                }})
            except Exception as e:
                send({"jsonrpc": "2.0", "id": req_id, "result": {
                    "content": [{"type": "text", "text": f"Error: {e}"}],
                    "isError": True
                }})
        else:
            send({"jsonrpc": "2.0", "id": req_id, "error": {
                "code": -32601, "message": f"Unknown tool: {name}"
            }})
    else:
        if req_id is not None:
            send({"jsonrpc": "2.0", "id": req_id, "error": {
                "code": -32601, "message": f"Method not found: {method}"
            }})
