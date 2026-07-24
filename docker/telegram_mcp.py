#!/usr/bin/env python3
"""
telegram_mcp — two-way Telegram bridge for the AgentOS operator (increment ux.12).

This is a **no-tools stdio MCP server**. It exposes NO agent-facing tools: an
agent that has an `Mcp` grant to this server gets an empty `tools/list`. It is
spawned by agentd like any other `[[tools.mcp_servers]]` entry purely so it
inherits the sandbox (caps → Landlock port allowlist), PASSENV handling, and the
CI sidecar-tests contract that an entrypoint-launched process would not.

The real work happens on ONE background daemon thread (started only AFTER the
MCP `initialize`/`tools/list` handshake, so it never blocks the handshake on a
Telegram long-poll). That thread does two jobs:

  1. OUTBOUND — polls the management API (`GET /api/v1/approvals` + `/api/v1/brief`)
     and pushes new pending approvals and freshly-published briefs to Telegram.
  2. INBOUND  — long-polls Telegram `getUpdates`, accepts `approve`/`deny` replies
     from the single allowlisted operator (in a private chat only), re-verifies
     the approval is still pending with unchanged args, then POSTs approve/deny to
     the management API with the `X-Approval-Token` header.

Security surface (see docs/plans/ux.12-telegram-reach.md):
  * Chat allowlist: honor only `message` updates; accept iff
    `message.from.id == TELEGRAM_CHAT_ID` AND `message.chat.type == "private"`.
    `from.id` is unforgeable via the Bot API (provided the bot token stays secret).
  * Relay-only + re-verify: before POSTing, re-`GET /api/v1/approvals`, confirm the
    id is still pending AND its `args_json` hashes to exactly what we delivered.
    This closes the cross-generation id-collision (deleted checkpoint resets the
    approval_seq) and enforces "the human approved what they actually saw".
  * Route-scoped `X-Approval-Token` is the actual control; the chat-ID allowlist is
    the second factor (who may drive the bot), not the trust boundary.
  * Fail-closed: on any POST/network error the approval stays pending — never
    synthesize an approve, never double-fire.
  * The bot token and the approval secret are NEVER logged.

Configuration (env vars, validated in _init()):
  TELEGRAM_BOT_TOKEN        Bot API token (required at runtime; the crown jewel).
  TELEGRAM_CHAT_ID          Single allowlisted numeric operator user id (required).
  MANAGEMENT_URL            agentd management API base (default http://127.0.0.1:7999).
  AGENTOS_APPROVAL_SECRET   Sent as X-Approval-Token on approve/deny POSTs.
                            If empty we still POST but log a warning.
  TELEGRAM_STATE_DIR        Durable dir for the offset/binding state (default /data).
  TELEGRAM_POLL_INTERVAL_S  Management poll cadence in seconds (default 3).

Example TOML:
  [[tools.mcp_servers]]
  name    = "telegram_bridge"
  command = "python3"
  args    = ["/usr/lib/agentos/docker/telegram_mcp.py"]
  passenv = ["TELEGRAM_BOT_TOKEN", "TELEGRAM_CHAT_ID", "AGENTOS_APPROVAL_SECRET",
             "MANAGEMENT_URL", "TELEGRAM_STATE_DIR", "TELEGRAM_POLL_INTERVAL_S"]
  capabilities = [{ Net = { hosts = ["api.telegram.org"], ports = [443, 7999] } }]
"""
import hashlib, json, os, sys, threading, time
import urllib.error
import urllib.request

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

RESPONSE_CAP        = 4 * 1024 * 1024   # 4 MB cap on any HTTP response body
ARGS_PREVIEW_CAP    = 500               # max chars of args_json surfaced to Telegram
DEFAULT_MGMT_URL    = "http://127.0.0.1:7999"
DEFAULT_STATE_DIR   = "/data"
DEFAULT_POLL_S      = 3
STATE_FILENAME      = "telegram_bridge_state.json"
MGMT_TIMEOUT_S      = 15                 # HTTP timeout for management calls

# Empty — this server exposes no agent-facing tools by design.
TOOLS: list = []

# ---------------------------------------------------------------------------
# Runtime configuration + state (populated by _init(); set directly by _self_test())
# ---------------------------------------------------------------------------

_bot_token:       str  = ""
_chat_id                = None          # int once configured
_management_url:  str  = DEFAULT_MGMT_URL
_approval_secret: str  = ""
_state_dir:       str  = DEFAULT_STATE_DIR
_poll_interval:   int  = DEFAULT_POLL_S
_state_path:      str  = ""

_offset:          int  = 0              # highest telegram update_id seen + 1
_delivered:       dict = {}             # approval_id -> {args_sha256, delivered_at}
_last_brief             = None          # last brief text pushed to Telegram
_state_writable:  bool = True           # False → best-effort in-memory only

_state_lock            = threading.Lock()


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _log(msg: str):
    """Log to stderr. NEVER pass the bot token or approval secret here."""
    print(f"telegram_mcp.py: {msg}", file=sys.stderr)


def _sha256(s: str) -> str:
    return hashlib.sha256((s or "").encode("utf-8")).hexdigest()


def _http_json(url: str, method: str = "GET", data=None,
               headers: dict = None, timeout: int = MGMT_TIMEOUT_S):
    """Perform an HTTP request. Returns (status, raw_bytes).

    status is an int for any HTTP response (including 4xx/5xx), or None on a
    transport-level failure (connection refused, DNS, timeout). raw_bytes may be
    empty. JSON encoding of `data` is applied when `data` is not None.
    """
    body = None
    hdrs = {}
    if data is not None:
        body = json.dumps(data).encode("utf-8")
        hdrs["Content-Type"] = "application/json"
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(url, data=body, headers=hdrs, method=method)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read(RESPONSE_CAP)
            status = getattr(resp, "status", 200)
            return status, raw
    except urllib.error.HTTPError as e:
        raw = b""
        try:
            raw = e.read(RESPONSE_CAP)
        except Exception:
            pass
        return e.code, raw
    except Exception as exc:                     # network / transport failure
        return None, str(exc).encode("utf-8", errors="replace")


# ---------------------------------------------------------------------------
# Telegram Bot API
# ---------------------------------------------------------------------------

def _tg_url(api_method: str) -> str:
    return f"https://api.telegram.org/bot{_bot_token}/{api_method}"


def send_message(text: str) -> bool:
    """Send a Telegram message to the allowlisted chat. Returns True on 200."""
    status, _ = _http_json(_tg_url("sendMessage"), method="POST",
                           data={"chat_id": _chat_id, "text": text})
    if status != 200:
        _log(f"sendMessage failed (status={status})")
    return status == 200


def get_updates(offset: int, timeout: int) -> list:
    """Long-poll Telegram getUpdates. Returns the (possibly empty) update list."""
    # HTTP timeout must exceed the long-poll timeout so the socket does not abort
    # before Telegram returns.
    status, raw = _http_json(
        _tg_url("getUpdates"), method="POST",
        data={"offset": offset, "timeout": timeout, "allowed_updates": ["message"]},
        timeout=timeout + MGMT_TIMEOUT_S,
    )
    if status != 200 or not raw:
        return []
    try:
        payload = json.loads(raw)
    except (ValueError, TypeError):
        return []
    if not payload.get("ok"):
        return []
    result = payload.get("result")
    return result if isinstance(result, list) else []


# ---------------------------------------------------------------------------
# Management API
# ---------------------------------------------------------------------------

def fetch_approvals():
    """GET /api/v1/approvals → list of PendingActionView, or None on error."""
    status, raw = _http_json(f"{_management_url}/api/v1/approvals", method="GET")
    if status != 200 or raw is None:
        return None
    try:
        data = json.loads(raw)
    except (ValueError, TypeError):
        return None
    return data if isinstance(data, list) else None


def fetch_brief():
    """GET /api/v1/brief → dict {brief, approvals_pending}, or None on error."""
    status, raw = _http_json(f"{_management_url}/api/v1/brief", method="GET")
    if status != 200 or raw is None:
        return None
    try:
        data = json.loads(raw)
    except (ValueError, TypeError):
        return None
    return data if isinstance(data, dict) else None


def post_approval(approval_id: str, verb: str, reason):
    """POST approve/deny to the management API. Returns (status, raw_bytes)."""
    url = f"{_management_url}/api/v1/approvals/{approval_id}/{verb}"
    headers = {"X-Approval-Token": _approval_secret}
    data = {"reason": reason} if reason else None
    return _http_json(url, method="POST", data=data, headers=headers)


# ---------------------------------------------------------------------------
# Durable state (offset + delivered bindings + last brief)
# ---------------------------------------------------------------------------

def _load_state():
    global _offset, _delivered, _last_brief
    try:
        with open(_state_path, "r") as f:
            data = json.load(f)
    except FileNotFoundError:
        return
    except Exception as exc:
        _log(f"WARNING: could not read state file: {exc}")
        return
    try:
        _offset = int(data.get("offset", 0))
    except (ValueError, TypeError):
        _offset = 0
    d = data.get("delivered_bindings")
    _delivered = d if isinstance(d, dict) else {}
    _last_brief = data.get("last_brief")


def _persist_state():
    """Atomically persist state. Degrades to in-memory-only if unwritable."""
    global _state_writable
    if not _state_writable or not _state_path:
        return
    data = {
        "offset":             _offset,
        "delivered_bindings": _delivered,
        "last_brief":         _last_brief,
    }
    tmp = _state_path + ".tmp"
    try:
        with open(tmp, "w") as f:
            f.write(json.dumps(data))
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, _state_path)
    except Exception as exc:
        _log(f"WARNING: state persist failed, continuing in-memory: {exc}")
        _state_writable = False
        try:
            os.unlink(tmp)
        except Exception:
            pass


# ---------------------------------------------------------------------------
# Outbound: push pending approvals + brief to Telegram
# ---------------------------------------------------------------------------

def deliver_approval(approval: dict):
    """Send one pending approval to Telegram and record the delivered binding."""
    aid = approval.get("id")
    if not aid:
        return
    args_json = approval.get("args_json", "") or ""
    preview = args_json[:ARGS_PREVIEW_CAP]
    if len(args_json) > ARGS_PREVIEW_CAP:
        preview += " …(truncated — view full in TUI)"
    text = (
        f"⚠ Approval needed [{aid}]\n"
        f"kind: {approval.get('kind', '?')}\n"
        f"risk: {approval.get('risk', '?')}\n"
        f"{approval.get('summary', '')}\n\n"
        f"args: {preview}\n\n"
        f"Reply:  approve {aid}   or   deny {aid} [reason]"
    )
    if send_message(text):
        with _state_lock:
            _delivered[aid] = {
                "args_sha256": _sha256(args_json),
                "delivered_at": int(time.time()),
            }
            _persist_state()


def deliver_brief():
    """Send the current brief to Telegram once, tracking the last-sent text."""
    global _last_brief
    data = fetch_brief()
    if data is None:
        return
    brief = data.get("brief")
    if not brief or brief == _last_brief:
        return
    if send_message(f"📋 Morning brief\n\n{brief}"):
        with _state_lock:
            _last_brief = brief
            _persist_state()


def poll_management():
    """One outbound cycle: push undelivered approvals and any new brief."""
    approvals = fetch_approvals()
    if approvals is not None:
        for a in approvals:
            if not isinstance(a, dict):
                continue
            aid = a.get("id")
            if not aid:
                continue
            with _state_lock:
                already = aid in _delivered
            if not already:
                deliver_approval(a)
    deliver_brief()


# ---------------------------------------------------------------------------
# Inbound: parse Telegram replies and resolve approvals
# ---------------------------------------------------------------------------

def parse_command(text: str):
    """Parse 'approve <id>' / 'deny <id> [reason]'. Returns (verb, id, reason) or None."""
    if not text:
        return None
    parts = text.strip().split(None, 2)
    if len(parts) < 2:
        return None
    verb = parts[0].lower()
    if verb not in ("approve", "deny"):
        return None
    approval_id = parts[1]
    reason = parts[2] if len(parts) >= 3 else None
    return verb, approval_id, reason


def handle_command(verb: str, approval_id: str, reason):
    """Re-verify (fail-closed) then POST the approve/deny. Relay-only."""
    approvals = fetch_approvals()
    if approvals is None:
        # Could not reach agentd — do NOT synthesize; approval stays pending.
        send_message(f"[{approval_id}] could not reach agentd — still pending, try again.")
        return

    match = None
    for a in approvals:
        if isinstance(a, dict) and a.get("id") == approval_id:
            match = a
            break

    if match is None:
        send_message(f"[{approval_id}] already resolved or unknown — nothing to do.")
        return

    with _state_lock:
        binding = _delivered.get(approval_id)
    current_hash = _sha256(match.get("args_json", "") or "")
    if binding is None or binding.get("args_sha256") != current_hash:
        # Cross-generation id reuse (e.g. checkpoint reset) or an id we never
        # delivered — refuse rather than approve something the human didn't see.
        send_message(
            f"[{approval_id}] REFUSED — this approval's details changed or were not "
            f"delivered by me. Check the TUI and re-issue if intended."
        )
        return

    status, _raw = post_approval(approval_id, verb, reason)
    if status == 200:
        tail = f" (reason: {reason})" if (verb == "deny" and reason) else ""
        send_message(f"[{approval_id}] {verb}d ✓{tail}")
        with _state_lock:
            _delivered.pop(approval_id, None)
            _persist_state()
    elif status == 404:
        send_message(f"[{approval_id}] already resolved elsewhere.")
    elif status == 401:
        _log("approve/deny POST rejected (401) — approval secret missing or wrong")
        send_message(f"[{approval_id}] could not {verb}: agentd rejected authorization.")
    elif status is None:
        # Transport failure — fail-closed, leave pending, do not mark resolved.
        send_message(f"[{approval_id}] could not reach agentd — still pending, try again.")
    else:
        send_message(f"[{approval_id}] could not {verb} (agentd returned {status}).")


def process_update(update: dict):
    """Apply the allowlist + chat-type guard to one update, then dispatch."""
    if not isinstance(update, dict):
        return
    msg = update.get("message")
    if not isinstance(msg, dict):
        # Ignore edited_message / channel_post / callback_query / my_chat_member.
        return
    frm = msg.get("from") or {}
    chat = msg.get("chat") or {}
    if frm.get("id") != _chat_id:
        return                                   # not the allowlisted operator
    if chat.get("type") != "private":
        return                                   # group-leak guard
    parsed = parse_command(msg.get("text") or "")
    if parsed is None:
        return
    verb, approval_id, reason = parsed
    handle_command(verb, approval_id, reason)


def handle_updates(updates: list):
    """Dedup by update_id, advance+persist the offset, process each in order."""
    global _offset
    for u in sorted(updates, key=lambda x: x.get("update_id", 0)):
        uid = u.get("update_id")
        if uid is None:
            continue
        if uid < _offset:
            continue                             # already processed (dedup)
        try:
            process_update(u)
        except Exception as exc:
            _log(f"error processing update {uid}: {exc}")
        _offset = uid + 1
    with _state_lock:
        _persist_state()


# ---------------------------------------------------------------------------
# Bridge thread
# ---------------------------------------------------------------------------

def _bridge_loop():
    """Single daemon thread: interleave the management poll and Telegram long-poll.

    getUpdates is long-polled with a timeout equal to the poll interval, so each
    loop iteration also runs one management poll — honoring the configured
    management cadence within a single thread.
    """
    _log(f"bridge thread started (poll={_poll_interval}s, mgmt={_management_url})")
    while True:
        try:
            poll_management()
        except Exception as exc:
            _log(f"management poll error: {exc}")
        try:
            updates = get_updates(_offset, timeout=_poll_interval)
            if updates:
                handle_updates(updates)
        except Exception as exc:
            _log(f"telegram poll error: {exc}")
            time.sleep(_poll_interval)           # backoff on hard failure


# ---------------------------------------------------------------------------
# Startup
# ---------------------------------------------------------------------------

def _init():
    global _bot_token, _chat_id, _management_url, _approval_secret
    global _state_dir, _poll_interval, _state_path, _state_writable

    token = (os.environ.get("TELEGRAM_BOT_TOKEN") or "").strip()
    if not token:
        _log("TELEGRAM_BOT_TOKEN is required")
        sys.exit(1)

    chat_raw = (os.environ.get("TELEGRAM_CHAT_ID") or "").strip()
    if not chat_raw:
        _log("TELEGRAM_CHAT_ID is required")
        sys.exit(1)
    try:
        _chat_id = int(chat_raw)
    except ValueError:
        _log(f"TELEGRAM_CHAT_ID must be a numeric user id, got '{chat_raw}'")
        sys.exit(1)

    _bot_token = token
    _management_url = (os.environ.get("MANAGEMENT_URL") or DEFAULT_MGMT_URL).strip().rstrip("/")
    _approval_secret = os.environ.get("AGENTOS_APPROVAL_SECRET", "")
    if not _approval_secret:
        _log("WARNING: AGENTOS_APPROVAL_SECRET is empty — approve/deny POSTs will be "
             "unauthenticated (agentd may reject them with 401)")

    _state_dir = (os.environ.get("TELEGRAM_STATE_DIR") or DEFAULT_STATE_DIR).strip()
    poll_raw = (os.environ.get("TELEGRAM_POLL_INTERVAL_S") or str(DEFAULT_POLL_S)).strip()
    try:
        _poll_interval = int(poll_raw)
        if _poll_interval < 1:
            raise ValueError
    except ValueError:
        _log(f"TELEGRAM_POLL_INTERVAL_S must be a positive integer, got '{poll_raw}'")
        sys.exit(1)

    _state_path = os.path.join(_state_dir, STATE_FILENAME)
    try:
        os.makedirs(_state_dir, exist_ok=True)
    except Exception as exc:
        _log(f"WARNING: state dir '{_state_dir}' not writable ({exc}) — "
             f"continuing in-memory (offset will not survive restart)")
        _state_writable = False
    if _state_writable:
        _load_state()

    t = threading.Thread(target=_bridge_loop, daemon=True)
    t.start()


# ---------------------------------------------------------------------------
# MCP JSON-RPC stdio loop
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
            "serverInfo":      {"name": "telegram_bridge", "version": "0.1.0"},
        }})
    elif method.startswith("notifications/"):
        pass
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": req_id, "result": {"tools": TOOLS, "nextCursor": None}})
    elif method == "tools/call":
        # No agent-facing tools exist on this bridge.
        send({"jsonrpc": "2.0", "id": req_id, "error": {
            "code": -32601,
            "message": "telegram_bridge exposes no tools (operator bridge only)",
        }})
    else:
        if req_id is not None:
            send({"jsonrpc": "2.0", "id": req_id, "error": {
                "code": -32601, "message": f"Method not found: {method}",
            }})


# ---------------------------------------------------------------------------
# Self-test (offline; mocks ALL HTTP; never starts the poll thread)
# ---------------------------------------------------------------------------

def _self_test():
    """Fully-offline self-test. No env required, no thread started.

    Monkeypatches urllib.request.urlopen with a fake that dispatches on URL:
      api.telegram.org/.../getUpdates   → a canned updates batch
      api.telegram.org/.../sendMessage  → records the send
      127.0.0.1:7999/api/v1/approvals   → a canned pending list
      .../approve or .../deny           → records the POST + X-Approval-Token header
    """
    global _bot_token, _chat_id, _management_url, _approval_secret
    global _state_dir, _poll_interval, _state_path, _state_writable
    global _offset, _delivered, _last_brief

    _bot_token       = "TEST_TOKEN"
    _chat_id         = 42
    _management_url  = "http://127.0.0.1:7999"
    _approval_secret = "test-secret"
    _state_writable  = False           # never touch disk during the test
    _state_path      = ""
    _poll_interval   = 3
    _last_brief      = None

    # Test-controllable canned responses (single-element lists = mutable cells).
    approvals_cell = [[]]              # what GET /api/v1/approvals returns
    updates_cell   = [[]]             # (unused directly; handle_updates called with batch)
    brief_cell     = [None]

    sent_messages: list = []           # texts pushed to Telegram
    posts:         list = []           # {id, action, token, reason} per approve/deny POST

    class _FakeResp:
        def __init__(self, status, body):
            self.status = status
            self._body = body if isinstance(body, bytes) else json.dumps(body).encode()
        def read(self, *a):
            return self._body
        def __enter__(self):
            return self
        def __exit__(self, *a):
            return False

    def fake_urlopen(req, timeout=None, **kw):
        url = req.get_full_url() if hasattr(req, "get_full_url") else str(req)
        data = getattr(req, "data", None)

        # Telegram getUpdates
        if "api.telegram.org" in url and "getUpdates" in url:
            return _FakeResp(200, {"ok": True, "result": updates_cell[0]})
        # Telegram sendMessage
        if "api.telegram.org" in url and "sendMessage" in url:
            body = {}
            if data:
                try:
                    body = json.loads(data)
                except Exception:
                    body = {}
            sent_messages.append(body.get("text", ""))
            return _FakeResp(200, {"ok": True, "result": {"message_id": len(sent_messages)}})
        # Management approve/deny POST (check BEFORE the plain approvals route)
        if "/api/v1/approvals/" in url and (url.endswith("/approve") or url.endswith("/deny")):
            token = req.get_header("X-approval-token")
            tail = url.split("/api/v1/approvals/")[1].split("/")
            aid, action = tail[0], tail[1]
            reason = None
            if data:
                try:
                    reason = json.loads(data).get("reason")
                except Exception:
                    reason = None
            posts.append({"id": aid, "action": action, "token": token, "reason": reason})
            return _FakeResp(200, {"ok": True})
        # Management approvals list
        if url.endswith("/api/v1/approvals"):
            return _FakeResp(200, approvals_cell[0])
        # Management brief
        if url.endswith("/api/v1/brief"):
            return _FakeResp(200, {"brief": brief_cell[0],
                                   "approvals_pending": len(approvals_cell[0])})
        return _FakeResp(200, {})

    orig_urlopen = urllib.request.urlopen
    urllib.request.urlopen = fake_urlopen

    failures: list = []

    def check(cond, name, detail=""):
        if cond:
            print(f"  PASS  {name}", file=sys.stderr)
        else:
            print(f"  FAIL  {name}: {detail}", file=sys.stderr)
            failures.append(name)

    def reset(delivered=None, approvals=None, brief=None):
        global _offset, _delivered, _last_brief
        _offset = 0
        _delivered = delivered or {}
        _last_brief = None
        approvals_cell[0] = approvals or []
        brief_cell[0] = brief
        sent_messages.clear()
        posts.clear()

    ARGS_A = '{"to":"alice@example.com","body":"hi"}'
    ARGS_B = '{"to":"mallory@evil.example","body":"exfil"}'

    def _pending(aid, args_json, kind="send_email", risk="high", summary="Send an email"):
        return {"id": aid, "agent_id": "cos", "kind": kind, "risk": risk,
                "summary": summary, "args_json": args_json, "age_secs": 5}

    def _msg(uid, text, from_id=42, chat_type="private"):
        return {"update_id": uid,
                "message": {"from": {"id": from_id}, "chat": {"type": chat_type},
                            "text": text}}

    try:
        # [1] allowlisted approve in a private chat, pending + args match → ONE POST w/ token
        reset(delivered={"act_1": {"args_sha256": _sha256(ARGS_A), "delivered_at": 0}},
              approvals=[_pending("act_1", ARGS_A)])
        handle_updates([_msg(100, "approve act_1")])
        check(len(posts) == 1
              and posts[0]["id"] == "act_1"
              and posts[0]["action"] == "approve"
              and posts[0]["token"] == "test-secret",
              "1: allowlisted approve (pending, args match) → one POST w/ X-Approval-Token",
              f"posts={posts}")

        # [2] non-allowlisted from.id → NO POST
        reset(delivered={"act_1": {"args_sha256": _sha256(ARGS_A), "delivered_at": 0}},
              approvals=[_pending("act_1", ARGS_A)])
        handle_updates([_msg(101, "approve act_1", from_id=999)])
        check(len(posts) == 0, "2: non-allowlisted from.id → no POST", f"posts={posts}")

        # [3] non-private chat → NO POST
        reset(delivered={"act_1": {"args_sha256": _sha256(ARGS_A), "delivered_at": 0}},
              approvals=[_pending("act_1", ARGS_A)])
        handle_updates([_msg(102, "approve act_1", chat_type="group")])
        check(len(posts) == 0, "3: non-private chat → no POST", f"posts={posts}")

        # [4] same update_id twice → exactly ONE POST (dedup via offset)
        reset(delivered={"act_1": {"args_sha256": _sha256(ARGS_A), "delivered_at": 0}},
              approvals=[_pending("act_1", ARGS_A)])
        batch = [_msg(103, "approve act_1")]
        handle_updates(batch)
        handle_updates(batch)                     # replay
        check(len(posts) == 1, "4: replayed update_id → exactly one POST", f"posts={posts}")

        # [5] approve act_9 but re-GET shows it not pending → NO POST (graceful)
        reset(delivered={"act_9": {"args_sha256": _sha256(ARGS_A), "delivered_at": 0}},
              approvals=[])                        # act_9 no longer pending
        handle_updates([_msg(104, "approve act_9")])
        check(len(posts) == 0
              and any("resolved" in m or "unknown" in m for m in sent_messages),
              "5: id not pending on re-GET → no POST, graceful reply",
              f"posts={posts} sent={sent_messages}")

        # [6] args-hash mismatch on re-verify → NO POST (refuse)
        reset(delivered={"act_1": {"args_sha256": _sha256(ARGS_A), "delivered_at": 0}},
              approvals=[_pending("act_1", ARGS_B)])   # args changed since delivery
        handle_updates([_msg(105, "approve act_1")])
        check(len(posts) == 0
              and any("REFUSED" in m for m in sent_messages),
              "6: args-hash mismatch on re-verify → no POST, refused",
              f"posts={posts} sent={sent_messages}")

        # [7] (bonus) outbound: a new pending approval is delivered + binding recorded
        reset(delivered={}, approvals=[_pending("act_5", ARGS_A)], brief=None)
        poll_management()
        check(len(sent_messages) >= 1
              and "act_5" in sent_messages[0]
              and _sha256(ARGS_A) not in sent_messages[0]   # hash never leaked
              and _delivered.get("act_5", {}).get("args_sha256") == _sha256(ARGS_A),
              "7: outbound delivers pending approval + records binding",
              f"sent={sent_messages} delivered={_delivered}")

        # [8] (bonus) deny with reason → POST carries the reason
        reset(delivered={"act_2": {"args_sha256": _sha256(ARGS_A), "delivered_at": 0}},
              approvals=[_pending("act_2", ARGS_A)])
        handle_updates([_msg(106, "deny act_2 looks phishy")])
        check(len(posts) == 1
              and posts[0]["action"] == "deny"
              and posts[0]["reason"] == "looks phishy",
              "8: deny with reason → POST carries reason",
              f"posts={posts}")
    finally:
        urllib.request.urlopen = orig_urlopen

    if failures:
        print(f"telegram_mcp.py: self-test FAILED ({len(failures)} failing)", file=sys.stderr)
        sys.exit(1)

    print("self-test PASSED", file=sys.stderr)
    sys.exit(0)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--test":
        _self_test()
    _init()
    for line in sys.stdin:
        process_line(line.strip())
