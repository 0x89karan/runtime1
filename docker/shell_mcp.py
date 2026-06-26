#!/usr/bin/env python3
"""
shell_exec MCP server — runs shell commands and returns stdout/stderr/exit_code.

Tool: run_command
  Input:  { command: str, timeout_s?: int (default 30, max 120), env?: {str: str} }
  Output: { stdout: str, stderr: str, exit_code: int }

Capability required (MCP server subprocess):
  capabilities = [{ ShellExec = {} }, { FsRead = { prefix = "..." } }, ...]

Example TOML (path relative to agentd/ where cargo run is invoked):
  [[tools.mcp_servers]]
  name    = "shell_exec"
  command = "python3"
  args    = ["../docker/shell_mcp.py"]
  capabilities = [
    { ShellExec = {} },
    { FsRead  = { prefix = "/workspace" } },
    { FsWrite = { prefix = "/tmp" } },
  ]

Safety notes:
  - shell=True is intentional; the operator controls which agents get shell access.
  - The MCP server subprocess starts with agentd's restricted env allowlist
    (PATH, HOME, USER, LANG, LC_ALL, TMPDIR); ANTHROPIC_API_KEY is not present.
  - stdout and stderr are each capped at 64 KB to prevent context exhaustion.
  - timeout_s is clamped to [1, 120]; 5-minute stalls are unacceptable in MCP.
"""
import json, os, signal, subprocess, sys

TOOL_STDOUT_CAP = 64 * 1024  # 64 KB per stream
MAX_TIMEOUT     = 120
DEFAULT_TIMEOUT = 30

# Dynamic linker vars that can hijack shared-library loading; strip them from
# any agent-supplied env dict before merging with the process environment.
_LINKER_ENV_BLOCKLIST = frozenset({
    "LD_PRELOAD", "LD_LIBRARY_PATH", "LD_AUDIT", "LD_DEBUG",
    "DYLD_INSERT_LIBRARIES", "DYLD_LIBRARY_PATH",
})

TOOLS = [{
    "name": "run_command",
    "description": (
        "Run a shell command and return stdout, stderr, and the exit code. "
        "Commands run via /bin/sh -c. Output is capped at 64 KB per stream. "
        "Timeout defaults to 30 s (max 120 s)."
    ),
    "inputSchema": {
        "type": "object",
        "properties": {
            "command":   {"type": "string",  "description": "Shell command to run."},
            "timeout_s": {"type": "integer", "description": "Timeout in seconds (default 30, max 120)."},
            "env":       {"type": "object",  "description": "Extra environment variables to merge in.",
                          "additionalProperties": {"type": "string"}},
        },
        "required": ["command"],
    },
}]


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def truncate(s, cap=TOOL_STDOUT_CAP):
    if isinstance(s, bytes):
        if len(s) > cap:
            return s[:cap].decode("utf-8", errors="replace") + "\n[TRUNCATED]"
        return s.decode("utf-8", errors="replace")
    if len(s) > cap:
        return s[:cap] + "\n[TRUNCATED]"
    return s


def handle_run_command(args):
    command = args.get("command", "")
    try:
        timeout_s = max(1, min(int(args.get("timeout_s", DEFAULT_TIMEOUT)), MAX_TIMEOUT))
    except (ValueError, TypeError):
        timeout_s = DEFAULT_TIMEOUT
    extra_env = args.get("env", {})

    # Strip linker-hijack vars from the agent-supplied dict before merging.
    safe_extra = {str(k): str(v) for k, v in extra_env.items()
                  if str(k).upper() not in _LINKER_ENV_BLOCKLIST}
    env = {**os.environ, **safe_extra}

    try:
        with subprocess.Popen(
            command,
            shell=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            stdin=subprocess.DEVNULL,
            env=env,
            start_new_session=True,  # new process group so we can kill children on timeout
        ) as proc:
            try:
                stdout, stderr = proc.communicate(timeout=timeout_s)
                return {
                    "stdout":    truncate(stdout),
                    "stderr":    truncate(stderr),
                    "exit_code": proc.returncode,
                }
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except Exception:
                    pass
                try:
                    proc.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
                return {"stdout": "", "stderr": f"[TIMEOUT after {timeout_s}s]", "exit_code": -1}
    except Exception as e:
        return {"stdout": "", "stderr": f"[ERROR: {e}]", "exit_code": -1}


def _self_test():
    """Smoke-test: exercise the core logic directly."""
    res = handle_run_command({"command": "echo hello", "timeout_s": 5})
    assert res["exit_code"] == 0, f"exit_code must be 0, got {res['exit_code']}"
    assert "hello" in res["stdout"], f"stdout must contain 'hello', got {res['stdout']!r}"
    assert res["stderr"] == "", f"stderr must be empty, got {res['stderr']!r}"

    res2 = handle_run_command({"command": "false"})
    assert res2["exit_code"] != 0, "false must exit non-zero"

    res3 = handle_run_command({"command": "echo hi", "timeout_s": 200})
    assert res3["exit_code"] == 0, "timeout clamp must not crash"

    print("shell_mcp.py: self-test PASSED", file=sys.stderr)
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
            "serverInfo":      {"name": "shell_exec", "version": "0.1.0"},
        }})
    elif method in ("notifications/initialized", "notifications/cancelled"):
        pass
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req_id, "result": {"tools": TOOLS, "nextCursor": None}})
    elif method == "tools/call":
        params = req.get("params", {})
        name   = params.get("name")
        args   = params.get("arguments", {})
        if name == "run_command":
            result = handle_run_command(args)
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
