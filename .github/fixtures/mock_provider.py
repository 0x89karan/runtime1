#!/usr/bin/env python3
"""Mock Anthropic /v1/messages endpoint for the nightly artifact E2E (ci.1).

Serves NON-streaming responses (the nightly fixture sets streaming = false)
matching the fields AnthropicResp parses in agentd/src/inference/anthropic.rs:
content[], stop_reason, usage.

Dispatch is on request CONTENT, not a counter, so readiness probes and retries
cannot skew it (adversarial review: a stateful counter miscounts the workflow's
readiness POSTs):
  - body without "messages"                → readiness probe: plain end_turn
  - messages containing a tool_result     → turn 2: end_turn "done"
  - otherwise                             → turn 1: tool_use list_dir, forcing
    the agent through the REAL tool loop + capability check (a text-only reply
    would leave the tool path untested)

Only POST /v1/messages is served; any other path 404s so an agentd regression
that hits the wrong endpoint fails loudly instead of green (F10).

Stdlib only; binds 0.0.0.0:8082 — the ONE port constant, mirrored by
nightly-e2e.yml's readiness curl and ANTHROPIC_BASE_URL (keep in sync). The
open bind is safe on an ephemeral CI runner: canned data, no secrets.
"""
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = 8082  # mirrored by nightly-e2e.yml's readiness curl + ANTHROPIC_BASE_URL
USAGE = {"input_tokens": 10, "output_tokens": 2}

TOOL_USE_RESP = {
    "id": "msg_mock_tool",
    "type": "message",
    "role": "assistant",
    "content": [
        {"type": "text", "text": "Listing the directory."},
        {"type": "tool_use", "id": "toolu_mock_1", "name": "list_dir", "input": {"path": "."}},
    ],
    "model": "mock-model",
    "stop_reason": "tool_use",
    "usage": USAGE,
}

END_TURN_RESP = {
    "id": "msg_mock_done",
    "type": "message",
    "role": "assistant",
    "content": [{"type": "text", "text": "done"}],
    "model": "mock-model",
    "stop_reason": "end_turn",
    "usage": USAGE,
}


def has_tool_result(payload):
    for msg in payload.get("messages", []):
        content = msg.get("content")
        if isinstance(content, list):
            for block in content:
                if isinstance(block, dict) and block.get("type") == "tool_result":
                    return True
    return False


def select_response(payload):
    """The dispatch contract the nightly E2E rides on — kept as a pure
    function so --test can assert every branch without a socket."""
    if "messages" not in payload:
        return END_TURN_RESP  # readiness probe ({} body)
    if has_tool_result(payload):
        return END_TURN_RESP
    return TOOL_USE_RESP


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        # Defensive parse: the bind is 0.0.0.0 by design (container must reach
        # it), so a garbage/negative Content-Length from a stray LAN peer must
        # not hang the handler (read(-1) blocks) or traceback the thread.
        try:
            length = max(0, int(self.headers.get("content-length", 0)))
        except ValueError:
            length = 0
        raw = self.rfile.read(min(length, 1 << 20))
        if self.path != "/v1/messages":
            self.send_error(404, "mock only serves POST /v1/messages")
            return
        try:
            payload = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            payload = {}
        body = json.dumps(select_response(payload)).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):  # quiet: one line per request is enough
        print(f"mock_provider: {self.command} {self.path}", flush=True)


if __name__ == "__main__":
    import sys

    if len(sys.argv) > 1 and sys.argv[1] == "--test":
        # Self-test (ci.1): asserts the dispatch contract in CI (harness-tests
        # job) — same rc==0 + stderr-marker contract as the docker/ sidecars.
        checks = 0
        # T1: readiness probe (no messages) → end_turn
        assert select_response({})["stop_reason"] == "end_turn", "T1 readiness"
        checks += 1
        # T2: first real turn (no tool_result) → tool_use(list_dir)
        r = select_response({"messages": [{"role": "user", "content": "hi"}]})
        assert r["stop_reason"] == "tool_use", "T2 stop_reason"
        assert any(
            b.get("type") == "tool_use" and b.get("name") == "list_dir" for b in r["content"]
        ), "T2 tool block"
        checks += 1
        # T3: post-tool turn (tool_result present) → end_turn "done"
        r = select_response(
            {"messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "x"}]}]}
        )
        assert r["stop_reason"] == "end_turn", "T3 end_turn"
        checks += 1
        # T4: both canned responses serialize and carry the fields
        # AnthropicResp parses (content[], stop_reason, usage)
        for resp in (TOOL_USE_RESP, END_TURN_RESP):
            round_tripped = json.loads(json.dumps(resp))
            assert round_tripped["content"] and round_tripped["usage"], "T4 fields"
        checks += 1
        print(f"mock_provider.py: self-test PASSED ({checks}/4)", file=sys.stderr)
        sys.exit(0)

    print(f"mock_provider: listening on :{PORT}", flush=True)
    HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
