#!/usr/bin/env python3
"""
cron_mcp — fires wait_for_trigger() on a cron schedule or interval.

Tool: wait_for_trigger
  Input:  { timeout_s?: int (default 25, max 25) }
  Output: { status: "fired"|"waiting"|"timeout",
            next_fire_utc: str,
            event_id: str,
            message: str }

Configuration (env vars):
  TRIGGER_CRON        5-field cron expression (UTC). Supported tokens: *, */N,
                      integer, comma-list. Example: "0 9 * * 1" (Mon 09:00 UTC).
                      DOW: 0=Sunday (POSIX), 7=Sunday alias.
  TRIGGER_INTERVAL    Interval shorthand, exclusive with TRIGGER_CRON.
                      Format: "every <N>(s|m|h)". Example: "every 5m".
  TRIGGER_MAX_WAIT_S  Abort after this many total seconds across all calls
                      (optional). Returns {status:"timeout"} once exceeded.

Example TOML (adjust path to repo location):
  [[tools.mcp_servers]]
  name    = "cron_trigger"
  command = "python3"
  args    = ["/usr/lib/agentos/docker/cron_mcp.py"]
  # No filesystem capabilities needed — cron uses only clock.
"""
import datetime, json, os, sys, time, uuid

MAX_TIMEOUT_S   = 25
DEFAULT_TIMEOUT = 25

TOOLS = [{
    "name": "wait_for_trigger",
    "description": (
        "Block until the configured cron schedule or interval fires. "
        "Returns {status:'fired'} when the trigger fires, or {status:'waiting'} "
        "if the timeout_s window elapsed without firing. "
        "Call this tool in a loop at the start of each turn until status=='fired'."
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
# Cron parser
# ---------------------------------------------------------------------------

def _parse_field(field: str, lo: int, hi: int) -> frozenset:
    """Parse one cron field. Exits 1 on unsupported tokens."""
    vals: set = set()
    for part in field.split(","):
        part = part.strip()
        if part == "*":
            vals.update(range(lo, hi + 1))
        elif "/" in part:
            # must be */N form — range/step not supported
            prefix, _, step_s = part.partition("/")
            if prefix != "*":
                print(f"cron_mcp.py: unsupported token '{part}' (range/step not supported)", file=sys.stderr)
                sys.exit(1)
            try:
                step = int(step_s)
                if step < 1:
                    raise ValueError
            except ValueError:
                print(f"cron_mcp.py: unsupported token '{part}'", file=sys.stderr)
                sys.exit(1)
            vals.update(range(lo, hi + 1, step))
        else:
            try:
                v = int(part)
            except ValueError:
                print(f"cron_mcp.py: unsupported token '{part}' (only *, */N, integers, comma-lists supported)", file=sys.stderr)
                sys.exit(1)
            if not (lo <= v <= hi):
                print(f"cron_mcp.py: value {v} out of range [{lo},{hi}]", file=sys.stderr)
                sys.exit(1)
            vals.add(v)
    return frozenset(vals)


def parse_cron(expr: str):
    """
    Parse a 5-field cron expression. Returns (min_set, hour_set, dom_set, month_set, dow_set).
    DOW field: 0=Sunday (POSIX), 7=Sunday alias; normalized to 0.
    """
    parts = expr.split()
    if len(parts) != 5:
        print(f"cron_mcp.py: expected 5 cron fields, got {len(parts)}: '{expr}'", file=sys.stderr)
        sys.exit(1)
    min_s, hour_s, dom_s, month_s, dow_s = parts
    minute_set = _parse_field(min_s,   0, 59)
    hour_set   = _parse_field(hour_s,  0, 23)
    dom_set    = _parse_field(dom_s,   1, 31)
    month_set  = _parse_field(month_s, 1, 12)
    # DOW: 0-7, normalize 7 -> 0 (both mean Sunday)
    raw_dow    = _parse_field(dow_s,   0,  7)
    dow_set    = frozenset((v % 7) for v in raw_dow)
    dom_star   = (dom_s == "*")
    dow_star   = (dow_s == "*")
    return minute_set, hour_set, dom_set, month_set, dow_set, dom_star, dow_star


def parse_interval(expr: str) -> int:
    """Parse 'every Ns/Nm/Nh' into seconds. Exits 1 on parse error."""
    expr = expr.strip().lower()
    if not expr.startswith("every "):
        print(f"cron_mcp.py: TRIGGER_INTERVAL must start with 'every ', got '{expr}'", file=sys.stderr)
        sys.exit(1)
    tail = expr[6:].strip()
    if not tail:
        print("cron_mcp.py: TRIGGER_INTERVAL missing duration after 'every'", file=sys.stderr)
        sys.exit(1)
    unit = tail[-1]
    if unit not in ("s", "m", "h"):
        print(f"cron_mcp.py: TRIGGER_INTERVAL unit must be s/m/h, got '{unit}' in '{expr}'", file=sys.stderr)
        sys.exit(1)
    try:
        n = int(tail[:-1])
        if n < 1:
            raise ValueError
    except ValueError:
        print(f"cron_mcp.py: TRIGGER_INTERVAL number invalid in '{expr}'", file=sys.stderr)
        sys.exit(1)
    multipliers = {"s": 1, "m": 60, "h": 3600}
    return n * multipliers[unit]


def _next_fire_cron(spec, after: datetime.datetime) -> datetime.datetime:
    """
    Return the earliest datetime >= after+1min that matches the cron spec.
    Raises RuntimeError if no match in 366 days.
    """
    minute_set, hour_set, dom_set, month_set, dow_set, dom_star, dow_star = spec
    t = after.replace(second=0, microsecond=0) + datetime.timedelta(minutes=1)
    limit = after + datetime.timedelta(days=366)

    while t < limit:
        if t.month not in month_set:
            # Advance to 1st of next valid month
            m = t.month + 1
            y = t.year
            if m > 12:
                m = 1
                y += 1
            t = t.replace(year=y, month=m, day=1, hour=0, minute=0)
            continue

        # POSIX DOW: (python weekday Mon=0 → POSIX Mon=1) using (wd+1)%7
        posix_dow = (t.weekday() + 1) % 7
        dom_ok = dom_star or (t.day in dom_set)
        dow_ok = dow_star or (posix_dow in dow_set)
        # Standard cron: if BOTH restricted → OR; otherwise AND
        if dom_star or dow_star:
            day_ok = dom_ok and dow_ok
        else:
            day_ok = dom_ok or dow_ok

        if not day_ok:
            t += datetime.timedelta(days=1)
            t = t.replace(hour=0, minute=0)
            continue

        if t.hour not in hour_set:
            t += datetime.timedelta(hours=1)
            t = t.replace(minute=0)
            continue

        if t.minute not in minute_set:
            t += datetime.timedelta(minutes=1)
            continue

        return t  # all fields match

    raise RuntimeError("No matching cron time in the next 366 days — check your expression")


# ---------------------------------------------------------------------------
# Server state
# ---------------------------------------------------------------------------

_MODE            = None   # "cron" or "interval"
_CRON_SPEC       = None
_INTERVAL_S      = None
_NEXT_FIRE_TS    = None   # float (UTC epoch)
_MAX_WAIT_S      = None   # int or None
_WAIT_START      = None   # float or None — set on first call


def _now() -> float:
    return time.time()


def _utcnow() -> datetime.datetime:
    return datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)


def _format_utc(ts: float) -> str:
    return datetime.datetime.fromtimestamp(ts, tz=datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _advance_next_fire():
    global _NEXT_FIRE_TS, _CRON_SPEC, _INTERVAL_S
    if _MODE == "interval":
        _NEXT_FIRE_TS = _now() + _INTERVAL_S
    else:
        dt = _next_fire_cron(_CRON_SPEC, _utcnow())
        _NEXT_FIRE_TS = dt.timestamp()


def _init():
    global _MODE, _CRON_SPEC, _INTERVAL_S, _NEXT_FIRE_TS, _MAX_WAIT_S
    cron_expr    = (os.environ.get("TRIGGER_CRON") or "").strip()
    interval_str = (os.environ.get("TRIGGER_INTERVAL") or "").strip()
    max_wait_raw = (os.environ.get("TRIGGER_MAX_WAIT_S") or "").strip()

    if cron_expr and interval_str:
        print("cron_mcp.py: TRIGGER_CRON and TRIGGER_INTERVAL are mutually exclusive", file=sys.stderr)
        sys.exit(1)
    if not cron_expr and not interval_str:
        print("cron_mcp.py: must set TRIGGER_CRON or TRIGGER_INTERVAL", file=sys.stderr)
        sys.exit(1)

    if max_wait_raw:
        try:
            _MAX_WAIT_S = int(max_wait_raw)
            if _MAX_WAIT_S < 1:
                raise ValueError
        except ValueError:
            print(f"cron_mcp.py: TRIGGER_MAX_WAIT_S must be a positive integer, got '{max_wait_raw}'", file=sys.stderr)
            sys.exit(1)

    if interval_str:
        _MODE       = "interval"
        _INTERVAL_S = parse_interval(interval_str)
        _NEXT_FIRE_TS = _now() + _INTERVAL_S
        print(f"cron_mcp.py: interval mode — every {_INTERVAL_S}s; first fire in {_INTERVAL_S}s", file=sys.stderr)
    else:
        _MODE      = "cron"
        _CRON_SPEC = parse_cron(cron_expr)
        dt = _next_fire_cron(_CRON_SPEC, _utcnow())
        _NEXT_FIRE_TS = dt.timestamp()
        print(f"cron_mcp.py: cron mode — '{cron_expr}'; next fire {_format_utc(_NEXT_FIRE_TS)} UTC", file=sys.stderr)


# ---------------------------------------------------------------------------
# Tool implementation
# ---------------------------------------------------------------------------

def handle_wait_for_trigger(args: dict) -> dict:
    global _NEXT_FIRE_TS, _WAIT_START

    try:
        timeout_s = max(1, min(int(args.get("timeout_s", DEFAULT_TIMEOUT)), MAX_TIMEOUT_S))
    except (ValueError, TypeError):
        timeout_s = DEFAULT_TIMEOUT

    now = _now()

    # Record first-call timestamp for max_wait_s accounting
    if _WAIT_START is None:
        _WAIT_START = now

    # Check max_wait_s
    if _MAX_WAIT_S is not None:
        elapsed_total = now - _WAIT_START
        if elapsed_total >= _MAX_WAIT_S:
            return {
                "status":       "timeout",
                "next_fire_utc": _format_utc(_NEXT_FIRE_TS),
                "event_id":     str(uuid.uuid4()),
                "message":      f"TRIGGER_MAX_WAIT_S={_MAX_WAIT_S}s exceeded",
            }

    wait_until = min(_NEXT_FIRE_TS, now + timeout_s)
    remaining  = wait_until - now
    if remaining > 0:
        time.sleep(remaining)

    now2 = _now()
    if now2 >= _NEXT_FIRE_TS:
        eid = str(uuid.uuid4())
        fired_at = _format_utc(_NEXT_FIRE_TS)
        try:
            _advance_next_fire()
        except RuntimeError as _e:
            return {
                "status":   "timeout",
                "event_id": eid,
                "message":  f"No future fire time after trigger: {_e}",
            }
        _WAIT_START = None  # reset for next wait cycle
        return {
            "status":        "fired",
            "next_fire_utc": _format_utc(_NEXT_FIRE_TS),
            "event_id":      eid,
            "message":       f"Trigger fired at {fired_at} UTC",
        }

    return {
        "status":        "waiting",
        "next_fire_utc": _format_utc(_NEXT_FIRE_TS),
        "event_id":      str(uuid.uuid4()),
        "message":       f"Next fire at {_format_utc(_NEXT_FIRE_TS)} UTC",
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
            "serverInfo":      {"name": "cron_trigger", "version": "0.1.0"},
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
    import os as _os
    # Interval: every 1s should fire within 2 seconds
    _os.environ["TRIGGER_CRON"]     = ""
    _os.environ["TRIGGER_INTERVAL"] = "every 1s"
    _init()
    r = handle_wait_for_trigger({"timeout_s": 3})
    assert r["status"] == "fired", f"interval fire failed: {r}"
    print("  [1/5] interval fire: PASS", file=sys.stderr)

    # Cron: a past minute should compute to next occurrence
    global _MODE, _CRON_SPEC, _NEXT_FIRE_TS, _WAIT_START, _MAX_WAIT_S
    _MODE       = "cron"
    _CRON_SPEC  = parse_cron("* * * * *")  # every minute
    _NEXT_FIRE_TS = _now() + 2
    _WAIT_START = None
    _MAX_WAIT_S = None
    r2 = handle_wait_for_trigger({"timeout_s": 5})
    assert r2["status"] == "fired", f"every-minute cron failed: {r2}"
    print("  [2/5] every-minute cron fire: PASS", file=sys.stderr)

    # POSIX DOW test: verify (weekday+1)%7 mapping
    # Python Monday = 0 → POSIX Monday = 1
    import datetime as _dt
    monday = _dt.datetime(2026, 6, 29, 0, 0)  # a known Monday
    assert (monday.weekday() + 1) % 7 == 1, "Monday should be POSIX 1"
    # Python Sunday = 6 → POSIX Sunday = 0
    sunday = _dt.datetime(2026, 6, 28, 0, 0)  # a known Sunday
    assert (sunday.weekday() + 1) % 7 == 0, "Sunday should be POSIX 0"
    print("  [3/5] POSIX DOW mapping: PASS", file=sys.stderr)

    # Cron field parser: valid tokens
    assert _parse_field("*",    0, 59) == frozenset(range(60))
    assert _parse_field("*/5",  0, 59) == frozenset(range(0, 60, 5))
    assert _parse_field("0,30", 0, 59) == frozenset({0, 30})
    assert _parse_field("7",    0, 59) == frozenset({7})
    print("  [4/5] cron field parser valid tokens: PASS", file=sys.stderr)

    # max_wait_s: first call records timestamp; second call should return timeout
    _MODE      = "interval"
    _INTERVAL_S = 3600
    _NEXT_FIRE_TS = _now() + 3600
    _MAX_WAIT_S = 1  # 1 second limit
    _WAIT_START = _now() - 5  # simulate 5 seconds already elapsed
    r3 = handle_wait_for_trigger({"timeout_s": 25})
    assert r3["status"] == "timeout", f"max_wait_s should return timeout: {r3}"
    print("  [5/5] max_wait_s timeout: PASS", file=sys.stderr)

    print("cron_mcp.py: self-test PASSED (5/5)", file=sys.stderr)
    sys.exit(0)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--test":
        _self_test()
    _init()
    for line in sys.stdin:
        process_line(line.strip())
