#!/usr/bin/env python3
"""
fs_watch_mcp — fires wait_for_trigger() when files in a directory change.

Tool: wait_for_trigger
  Input:  { timeout_s?: int (default 25, max 25) }
  Output: { status: "fired"|"waiting"|"timeout",
            changed: [{path: str, event: str}],
            event_id: str,
            message: str }

Configuration (env vars):
  TRIGGER_WATCH_PATH      Directory to watch (required).
  TRIGGER_POLL_INTERVAL_S Polling interval in seconds (default 2).
  TRIGGER_IGNORE_PATTERNS Comma-separated glob patterns to ignore.
                          Applied to the filename component.
                          Example: "*.pyc,__pycache__,*.swp"
  TRIGGER_QUIET_PERIOD_S  Debounce window in seconds (default 1).
                          Reports only after this many seconds of quiet.
  TRIGGER_MAX_WAIT_S      Abort after this many total seconds across all
                          calls (optional).

Example TOML (adjust path to repo location):
  # Requires: export TRIGGER_WATCH_PATH=/path/to/watch
  [[tools.mcp_servers]]
  name    = "fs_watch"
  command = "python3"
  args    = ["/usr/lib/agentos/docker/fs_watch_mcp.py"]
  capabilities = [{ FsRead = { prefix = "/" } }]

Note: On checkpoint, agentd restarts this process. The snapshot is rebuilt
from the current directory state. Changes that occurred during the downtime
window are NOT reported (they become the new baseline).
"""
import fnmatch, json, os, sys, time, uuid

MAX_TIMEOUT_S   = 25
DEFAULT_TIMEOUT = 25

TOOLS = [{
    "name": "wait_for_trigger",
    "description": (
        "Block until a file in the watched directory changes. "
        "Returns {status:'fired', changed:[...]} when a change is detected, "
        "or {status:'waiting'} if the timeout_s window elapsed without changes. "
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
# Server state
# ---------------------------------------------------------------------------

_WATCH_PATH      = ""
_POLL_INTERVAL   = 2.0
_IGNORE_PATTERNS: list = []
_QUIET_PERIOD    = 1.0
_MAX_WAIT_S      = None

_snapshot: dict  = {}   # path -> (mtime_ns, size, inode)
_pending: list   = []   # [{path, event}] awaiting quiet period
_last_change_t   = None # monotonic time of most recent detected change
_wait_start      = None # monotonic time of first call in this wait cycle


def _matches_ignore(name: str) -> bool:
    return any(fnmatch.fnmatch(name, pat) for pat in _IGNORE_PATTERNS)


def _scan(path: str) -> dict:
    """Recursively scan directory; return {path: (mtime_ns, size, inode)}."""
    result: dict = {}
    try:
        with os.scandir(path) as it:
            for entry in it:
                if _matches_ignore(entry.name):
                    continue
                if entry.is_dir(follow_symlinks=False):
                    result.update(_scan(entry.path))
                elif entry.is_file(follow_symlinks=False):
                    try:
                        st = entry.stat(follow_symlinks=False)
                        result[entry.path] = (st.st_mtime_ns, st.st_size, st.st_ino)
                    except OSError:
                        pass
    except PermissionError:
        pass
    return result


def _diff(old: dict, new: dict) -> list:
    """Return list of {path, event} for changes between snapshots."""
    changes = []
    for path, stat in new.items():
        if path not in old:
            changes.append({"path": path, "event": "created"})
        elif stat != old[path]:
            changes.append({"path": path, "event": "modified"})
    for path in old:
        if path not in new:
            changes.append({"path": path, "event": "deleted"})
    return changes


def _init():
    global _WATCH_PATH, _POLL_INTERVAL, _IGNORE_PATTERNS, _QUIET_PERIOD, _MAX_WAIT_S, _snapshot

    _WATCH_PATH = (os.environ.get("TRIGGER_WATCH_PATH") or "").strip()
    if not _WATCH_PATH:
        print("fs_watch_mcp.py: TRIGGER_WATCH_PATH must be set", file=sys.stderr)
        sys.exit(1)
    if not os.path.isdir(_WATCH_PATH):
        print(f"fs_watch_mcp.py: TRIGGER_WATCH_PATH '{_WATCH_PATH}' is not a directory", file=sys.stderr)
        sys.exit(1)

    poll_raw = (os.environ.get("TRIGGER_POLL_INTERVAL_S") or "").strip()
    if poll_raw:
        try:
            _POLL_INTERVAL = float(poll_raw)
            if _POLL_INTERVAL <= 0:
                raise ValueError
        except ValueError:
            print(f"fs_watch_mcp.py: TRIGGER_POLL_INTERVAL_S must be a positive number, got '{poll_raw}'", file=sys.stderr)
            sys.exit(1)

    ignore_raw = (os.environ.get("TRIGGER_IGNORE_PATTERNS") or "").strip()
    if ignore_raw:
        _IGNORE_PATTERNS = [p.strip() for p in ignore_raw.split(",") if p.strip()]

    quiet_raw = (os.environ.get("TRIGGER_QUIET_PERIOD_S") or "").strip()
    if quiet_raw:
        try:
            _QUIET_PERIOD = float(quiet_raw)
            if _QUIET_PERIOD < 0:
                raise ValueError
        except ValueError:
            print(f"fs_watch_mcp.py: TRIGGER_QUIET_PERIOD_S must be a non-negative number, got '{quiet_raw}'", file=sys.stderr)
            sys.exit(1)

    max_wait_raw = (os.environ.get("TRIGGER_MAX_WAIT_S") or "").strip()
    if max_wait_raw:
        try:
            _MAX_WAIT_S = int(max_wait_raw)
            if _MAX_WAIT_S < 1:
                raise ValueError
        except ValueError:
            print(f"fs_watch_mcp.py: TRIGGER_MAX_WAIT_S must be a positive integer, got '{max_wait_raw}'", file=sys.stderr)
            sys.exit(1)

    _snapshot = _scan(_WATCH_PATH)
    print(
        f"fs_watch_mcp.py: watching '{_WATCH_PATH}'; poll={_POLL_INTERVAL}s "
        f"quiet={_QUIET_PERIOD}s ignore={_IGNORE_PATTERNS or 'none'} "
        f"baseline={len(_snapshot)} files",
        file=sys.stderr,
    )


# ---------------------------------------------------------------------------
# Tool implementation
# ---------------------------------------------------------------------------

def handle_wait_for_trigger(args: dict) -> dict:
    global _snapshot, _pending, _last_change_t, _wait_start

    try:
        timeout_s = max(1, min(int(args.get("timeout_s", DEFAULT_TIMEOUT)), MAX_TIMEOUT_S))
    except (ValueError, TypeError):
        timeout_s = DEFAULT_TIMEOUT

    mono = time.monotonic()

    # Record first-call timestamp for max_wait_s accounting
    if _wait_start is None:
        _wait_start = mono

    # Check max_wait_s
    if _MAX_WAIT_S is not None:
        elapsed_total = mono - _wait_start
        if elapsed_total >= _MAX_WAIT_S:
            return {
                "status":   "timeout",
                "changed":  [],
                "event_id": str(uuid.uuid4()),
                "message":  f"TRIGGER_MAX_WAIT_S={_MAX_WAIT_S}s exceeded",
            }

    deadline = mono + timeout_s

    while time.monotonic() < deadline:
        # Check debounced pending changes first
        if _pending and _last_change_t is not None:
            if time.monotonic() - _last_change_t >= _QUIET_PERIOD:
                changes = list(_pending)
                _pending.clear()
                _last_change_t = None
                _wait_start = None
                return {
                    "status":   "fired",
                    "changed":  changes,
                    "event_id": str(uuid.uuid4()),
                    "message":  f"{len(changes)} change(s) detected in '{_WATCH_PATH}'",
                }

        # Poll the directory
        new_snap = _scan(_WATCH_PATH)
        diff     = _diff(_snapshot, new_snap)
        _snapshot = new_snap

        if diff:
            _pending.extend(diff)
            _last_change_t = time.monotonic()

        sleep_remaining = deadline - time.monotonic()
        if sleep_remaining <= 0:
            break
        time.sleep(min(_POLL_INTERVAL, sleep_remaining))

    # One last debounce check before returning "waiting"
    if _pending and _last_change_t is not None:
        if time.monotonic() - _last_change_t >= _QUIET_PERIOD:
            changes = list(_pending)
            _pending.clear()
            _last_change_t = None
            _wait_start = None
            return {
                "status":   "fired",
                "changed":  changes,
                "event_id": str(uuid.uuid4()),
                "message":  f"{len(changes)} change(s) detected in '{_WATCH_PATH}'",
            }

    return {
        "status":   "waiting",
        "changed":  [],
        "event_id": str(uuid.uuid4()),
        "message":  f"No changes in '{_WATCH_PATH}' within {timeout_s}s",
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
            "serverInfo":      {"name": "fs_watch", "version": "0.1.0"},
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
    import tempfile, os as _os

    print("fs_watch_mcp.py: running self-test ...", file=sys.stderr)

    with tempfile.TemporaryDirectory() as tmpdir:
        # Wire up globals directly (bypass _init env var parsing)
        global _WATCH_PATH, _POLL_INTERVAL, _IGNORE_PATTERNS, _QUIET_PERIOD
        global _MAX_WAIT_S, _snapshot, _pending, _last_change_t, _wait_start
        _WATCH_PATH      = tmpdir
        _POLL_INTERVAL   = 0.1
        _IGNORE_PATTERNS = ["*.pyc", "__pycache__"]
        _QUIET_PERIOD    = 0.2
        _MAX_WAIT_S      = None
        _snapshot        = _scan(tmpdir)
        _pending         = []
        _last_change_t   = None
        _wait_start      = None

        # [1] Ignored file does not trigger
        ignored = _os.path.join(tmpdir, "module.pyc")
        with open(ignored, "w") as f:
            f.write("x")
        r = handle_wait_for_trigger({"timeout_s": 1})
        assert r["status"] == "waiting", f"ignored file should not fire: {r}"
        print("  [1/6] ignore pattern: PASS", file=sys.stderr)

        # [2] New file triggers
        new_file = _os.path.join(tmpdir, "hello.txt")
        with open(new_file, "w") as f:
            f.write("hello")
        r2 = handle_wait_for_trigger({"timeout_s": 5})
        assert r2["status"] == "fired", f"new file should fire: {r2}"
        assert any(e["event"] == "created" for e in r2["changed"]), f"expected 'created': {r2}"
        print("  [2/6] file created fires: PASS", file=sys.stderr)

        # [3] Modified file triggers (mtime/size change)
        time.sleep(0.05)
        with open(new_file, "w") as f:
            f.write("hello world — longer content")
        r3 = handle_wait_for_trigger({"timeout_s": 5})
        assert r3["status"] == "fired", f"modified file should fire: {r3}"
        assert any(e["event"] == "modified" for e in r3["changed"]), f"expected 'modified': {r3}"
        print("  [3/6] file modified fires: PASS", file=sys.stderr)

        # [4] Delete + recreate at same size detected via inode change
        # Write exactly same content so mtime may differ but size is same
        original_inode = _os.stat(new_file).st_ino
        # Rebuild baseline
        _snapshot = _scan(tmpdir)
        _pending  = []
        _last_change_t = None
        _wait_start = None
        _os.unlink(new_file)
        # Recreate with SAME content (same size, different inode)
        with open(new_file, "w") as f:
            f.write("hello world — longer content")
        new_inode = _os.stat(new_file).st_ino
        # Inodes differ only when on same filesystem with real unlink+create
        # (On some tmpfs implementations they may differ or not — test logic regardless)
        r4 = handle_wait_for_trigger({"timeout_s": 5})
        # Should fire (deleted + created events, or modified via inode)
        assert r4["status"] == "fired", f"delete+recreate should fire: {r4}"
        print("  [4/6] delete+recreate fires: PASS", file=sys.stderr)

        # [5] max_wait_s returns timeout
        _MAX_WAIT_S  = 1
        _wait_start  = time.monotonic() - 5  # simulate 5s already elapsed
        _snapshot    = _scan(tmpdir)
        _pending     = []
        _last_change_t = None
        r5 = handle_wait_for_trigger({"timeout_s": 25})
        assert r5["status"] == "timeout", f"max_wait_s should return timeout: {r5}"
        print("  [5/6] max_wait_s timeout: PASS", file=sys.stderr)

        # [6] Debounce: rapid writes coalesce to single fire
        _MAX_WAIT_S  = None
        _wait_start  = None
        _snapshot    = _scan(tmpdir)
        _pending     = []
        _last_change_t = None
        # Write 3 files rapidly
        for i in range(3):
            with open(_os.path.join(tmpdir, f"rapid_{i}.txt"), "w") as f:
                f.write(f"data{i}")
        r6 = handle_wait_for_trigger({"timeout_s": 5})
        assert r6["status"] == "fired", f"rapid writes should fire once: {r6}"
        assert len(r6["changed"]) == 3, f"expected 3 changes coalesced: {r6}"
        print("  [6/6] debounce coalesces rapid writes: PASS", file=sys.stderr)

    print("fs_watch_mcp.py: self-test PASSED (6/6)", file=sys.stderr)
    sys.exit(0)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--test":
        _self_test()
    _init()
    for line in sys.stdin:
        process_line(line.strip())
