#!/usr/bin/env python3
"""
semantic_kb_mcp — Layer-2 semantic KB MCP sidecar (h8.1).

Exposes kb_put / kb_get / kb_search over HTTP/JSON-RPC (MCP spec 2024-11-05).
Backed by Qdrant (vector store) + Voyage AI (embedding API).

Environment variables:
  QDRANT_URL            Qdrant base URL (default: http://qdrant:6333)
  VOYAGE_API_KEY        Voyage AI key (required unless VOYAGE_MOCK_EMBEDDINGS=1)
  VOYAGE_MODEL          Embedding model (default: voyage-3-lite)
  VOYAGE_MOCK_EMBEDDINGS  Set to "1" to use zero vectors (testing, no key needed)
  PORT                  HTTP port to listen on (default: 8020)

Security:
  - VOYAGE_API_KEY is never logged or returned to callers.
  - SSRF guard on QDRANT_URL: loopback, RFC-1918, and link-local are allowed
    (Docker-internal use case); external IPs are blocked at startup.
  - Key/segment input validation (no path separators, ≤128 chars).
  - kb_search results capped at 100 hits × 8 KB each.

Self-test (no external services needed with VOYAGE_MOCK_EMBEDDINGS=1):
  VOYAGE_MOCK_EMBEDDINGS=1 python3 semantic_kb_mcp.py --test

TOML example (docker-compose peer service):
  [[tools.mcp_servers]]
  name               = "semantic-kb"
  url                = "http://semantic-kb-mcp:8020"
  allow_insecure_local = true
  tool_override      = true
"""

import hmac
import ipaddress
import json
import os
import re
import secrets
import socket
import sys
import threading
import unittest
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse
from uuid import uuid4, uuid5, NAMESPACE_OID

# ── Constants ─────────────────────────────────────────────────────────────────

VOYAGE_API_URL   = "https://api.voyageai.com/v1/embeddings"
VOYAGE_MODELS    = {
    "voyage-3-lite": 512,
    "voyage-3":      1024,
    "voyage-code-3": 1024,
}
DEFAULT_MODEL    = "voyage-3-lite"
MAX_SEARCH_HITS  = 100
HIT_CONTENT_CAP  = 8 * 1024          # 8 KB per hit
KEY_MAX_CHARS    = 128
QDRANT_TIMEOUT   = 10
VOYAGE_TIMEOUT   = 30
SERVER_VERSION   = "0.1.0"
MAX_REQUEST_BODY    = 4 * 1024 * 1024   # 4 MB — matches MCP HTTP client cap in Rust
MAX_QDRANT_RESPONSE = 4 * 1024 * 1024   # 4 MB — guard against oversized Qdrant responses
MAX_VOYAGE_RESPONSE = 4 * 1024 * 1024   # 4 MB — guard against oversized Voyage AI responses

# ── Config ────────────────────────────────────────────────────────────────────

QDRANT_URL      = os.environ.get("QDRANT_URL", "http://qdrant:6333").rstrip("/")
VOYAGE_KEY      = os.environ.get("VOYAGE_API_KEY", "")
VOYAGE_MODEL    = os.environ.get("VOYAGE_MODEL", DEFAULT_MODEL)
MOCK_EMBED      = os.environ.get("VOYAGE_MOCK_EMBEDDINGS", "0") == "1"
PORT            = int(os.environ.get("PORT", "8020"))
SIDECAR_SECRET  = os.environ.get("SIDECAR_SECRET", "")  # optional inbound auth token

VOYAGE_DIM = VOYAGE_MODELS.get(VOYAGE_MODEL, 512)

# ── SSRF guard ────────────────────────────────────────────────────────────────

_LOOPBACK_V4  = ipaddress.ip_network("127.0.0.0/8")
_LINK_LOCAL   = ipaddress.ip_network("169.254.0.0/16")
_RFC1918       = [
    ipaddress.ip_network("10.0.0.0/8"),
    ipaddress.ip_network("172.16.0.0/12"),
    ipaddress.ip_network("192.168.0.0/16"),
]
_LOOPBACK_V6   = ipaddress.ip_network("::1/128")
_PRIVATE_V6    = [
    ipaddress.ip_network("fc00::/7"),
    ipaddress.ip_network("fe80::/10"),
]


def _is_private(addr_str: str) -> bool:
    """Return True when the IP is loopback, link-local, or RFC-1918 (allowed for Docker)."""
    try:
        ip = ipaddress.ip_address(addr_str)
    except ValueError:
        return False
    if isinstance(ip, ipaddress.IPv4Address):
        return (
            ip in _LOOPBACK_V4
            or ip in _LINK_LOCAL
            or any(ip in net for net in _RFC1918)
        )
    if isinstance(ip, ipaddress.IPv6Address) and ip.ipv4_mapped is not None:
        return _is_private(str(ip.ipv4_mapped))
    return ip in _LOOPBACK_V6 or any(ip in net for net in _PRIVATE_V6)


def _validate_upstream_url(url: str) -> None:
    """Raise ValueError when the upstream URL resolves to a public IP."""
    parsed = urlparse(url)
    host = parsed.hostname or ""
    try:
        addrs = {r[4][0] for r in socket.getaddrinfo(host, None)}
    except socket.gaierror as e:
        raise ValueError(f"SSRF guard: cannot resolve {host!r}: {e}") from e
    for addr in addrs:
        if not _is_private(addr):
            raise ValueError(
                f"SSRF guard: {host!r} resolves to {addr!r} which is not a private/loopback "
                f"address — QDRANT_URL must point to a Docker-internal service"
            )


# ── Input validation ──────────────────────────────────────────────────────────

_KEY_RE = re.compile(r"^[^\x00/\\?#@%+=]+$")


def _validate_key(key: str) -> None:
    if not key or len(key) > KEY_MAX_CHARS:
        raise ValueError(f"key must be 1–{KEY_MAX_CHARS} characters, got {len(key)!r}")
    if not _KEY_RE.match(key):
        raise ValueError(f"key must not contain path separators or null bytes: {key!r}")


def _validate_segment(segment: str) -> None:
    if not segment or len(segment) > KEY_MAX_CHARS:
        raise ValueError(f"segment must be 1–{KEY_MAX_CHARS} characters")
    if not _KEY_RE.match(segment):
        raise ValueError(f"segment must not contain path separators or null bytes: {segment!r}")


# ── Embedding ─────────────────────────────────────────────────────────────────

def _embed(texts: list[str]) -> list[list[float]]:
    """Return embeddings for a list of texts. Uses mock zeros when MOCK_EMBED is set."""
    if MOCK_EMBED:
        return [[0.0] * VOYAGE_DIM for _ in texts]
    if not VOYAGE_KEY:
        raise RuntimeError(
            "VOYAGE_API_KEY is not set and VOYAGE_MOCK_EMBEDDINGS is not '1'. "
            "Either export VOYAGE_API_KEY=<key> or set VOYAGE_MOCK_EMBEDDINGS=1 for testing."
        )
    payload = json.dumps({
        "input": texts,
        "model": VOYAGE_MODEL,
    }).encode()
    req = urllib.request.Request(
        VOYAGE_API_URL,
        data=payload,
        headers={
            "Authorization": f"Bearer {VOYAGE_KEY}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=VOYAGE_TIMEOUT) as resp:
            raw = resp.read(MAX_VOYAGE_RESPONSE + 1)
            if len(raw) > MAX_VOYAGE_RESPONSE:
                raise RuntimeError(
                    f"Voyage AI response too large (> {MAX_VOYAGE_RESPONSE} bytes)"
                )
            body = json.loads(raw)
    except urllib.error.HTTPError as e:
        body_bytes = e.read(MAX_VOYAGE_RESPONSE + 1)
        if len(body_bytes) > MAX_VOYAGE_RESPONSE:
            raise RuntimeError(f"Voyage AI API error {e.code}: error body too large") from e
        try:
            detail = json.loads(body_bytes).get("error", {}).get("message", body_bytes.decode("utf-8", errors="replace"))
        except Exception:
            detail = body_bytes.decode("utf-8", errors="replace")
        raise RuntimeError(f"Voyage AI API error {e.code}: {detail}") from e
    return [item["embedding"] for item in body["data"]]


# ── Qdrant helpers ────────────────────────────────────────────────────────────

def _qdrant(path: str, method: str = "GET", body: dict | None = None) -> dict:
    url = f"{QDRANT_URL}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        url,
        data=data,
        headers={"Content-Type": "application/json"} if data else {},
        method=method,
    )
    try:
        with urllib.request.urlopen(req, timeout=QDRANT_TIMEOUT) as resp:
            body = resp.read(MAX_QDRANT_RESPONSE + 1)
            if len(body) > MAX_QDRANT_RESPONSE:
                raise RuntimeError(
                    f"Qdrant {method} {path} response too large (> {MAX_QDRANT_RESPONSE} bytes)"
                )
            return json.loads(body)
    except urllib.error.HTTPError as e:
        body_bytes = e.read(MAX_QDRANT_RESPONSE + 1)
        if len(body_bytes) > MAX_QDRANT_RESPONSE:
            raise RuntimeError(f"Qdrant {method} {path} → HTTP {e.code}: error body too large") from e
        raise RuntimeError(f"Qdrant {method} {path} → HTTP {e.code}: {body_bytes.decode('utf-8', errors='replace')}") from e


def _collection_name(segment: str) -> str:
    """Map a KB segment name to a Qdrant collection name."""
    return f"kb_{segment}"


def _ensure_collection(segment: str) -> None:
    """Create the Qdrant collection for this segment if it does not exist."""
    cname = _collection_name(segment)
    try:
        _qdrant(f"/collections/{cname}")
        return  # already exists
    except RuntimeError as e:
        if "404" not in str(e):
            raise  # non-404 errors (503, connection refused) are real errors
    try:
        _qdrant(
            f"/collections/{cname}",
            method="PUT",
            body={
                "vectors": {
                    "size": VOYAGE_DIM,
                    "distance": "Cosine",
                }
            },
        )
    except RuntimeError as e:
        # 409 Conflict: another concurrent caller created it first — treat as success.
        if "409" not in str(e):
            raise


def _point_id(key: str) -> str:
    """Derive a stable Qdrant point UUID from a segment-scoped key using uuid5."""
    return str(uuid5(NAMESPACE_OID, key))


# ── Tool handlers ─────────────────────────────────────────────────────────────

def _handle_kb_put(args: dict) -> dict:
    segment  = args.get("segment", "default")
    key      = args.get("key", "")
    content  = args.get("content", "")
    metadata = args.get("metadata") or {}

    _validate_segment(segment)
    _validate_key(key)
    if not isinstance(content, str):
        raise ValueError("content must be a string")
    content_bytes = content.encode("utf-8")
    if len(content_bytes) > HIT_CONTENT_CAP:
        raise ValueError(f"content too large ({len(content_bytes)} bytes, max {HIT_CONTENT_CAP})")

    _ensure_collection(segment)
    [vec] = _embed([content])

    point_id = _point_id(key)
    _qdrant(
        f"/collections/{_collection_name(segment)}/points",
        method="PUT",
        body={
            "points": [{
                "id":      point_id,
                "vector":  vec,
                "payload": {
                    "key":      key,
                    "content":  content,
                    "metadata": metadata,
                },
            }]
        },
    )
    return {"stored": True, "segment": segment, "key": key, "point_id": point_id}


def _handle_kb_get(args: dict) -> dict:
    segment = args.get("segment", "default")
    key     = args.get("key", "")

    _validate_segment(segment)
    _validate_key(key)

    point_id = _point_id(key)
    try:
        result = _qdrant(
            f"/collections/{_collection_name(segment)}/points/{point_id}"
        )
    except RuntimeError as e:
        if "404" in str(e):
            return {"found": False, "segment": segment, "key": key}
        raise

    payload = result.get("result", {}).get("payload", {})
    if not payload:
        return {"found": False, "segment": segment, "key": key}
    return {
        "found":    True,
        "segment":  segment,
        "key":      key,
        "content":  payload.get("content", ""),
        "metadata": payload.get("metadata", {}),
    }


def _handle_kb_search(args: dict) -> dict:
    segment = args.get("segment", "default")
    query   = args.get("query", "")
    limit   = int(args.get("limit", 10))

    _validate_segment(segment)
    if not query:
        return {"hits": [], "segment": segment, "query": query}
    limit = max(1, min(limit, MAX_SEARCH_HITS))

    # Ensure collection exists (search on missing collection → error).
    try:
        _qdrant(f"/collections/{_collection_name(segment)}")
    except RuntimeError as e:
        if "404" not in str(e):
            raise  # non-404 errors (503, connection refused) are real errors — propagate
        return {"hits": [], "segment": segment, "query": query, "note": "segment is empty"}

    [qvec] = _embed([query])
    result = _qdrant(
        f"/collections/{_collection_name(segment)}/points/search",
        method="POST",
        body={"vector": qvec, "limit": limit, "with_payload": True},
    )

    hits = []
    for item in result.get("result", []):
        payload = item.get("payload", {})
        content = payload.get("content", "")
        if len(content) > HIT_CONTENT_CAP:
            content = content[:HIT_CONTENT_CAP]
        hits.append({
            "key":      payload.get("key", ""),
            "score":    item.get("score", 0.0),
            "content":  content,
            "metadata": payload.get("metadata", {}),
        })
    return {"hits": hits, "segment": segment, "query": query}


# ── MCP tool descriptors ──────────────────────────────────────────────────────

TOOLS = [
    {
        "name": "kb_put",
        "description": (
            "Store content in the semantic knowledge base under a segment + key. "
            "Embeds the content as a vector; overwrites any existing entry with the same key."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "segment":  {"type": "string", "description": "KB segment name (e.g. 'research')."},
                "key":      {"type": "string", "description": "Unique entry key within the segment."},
                "content":  {"type": "string", "description": "Text content to store and embed."},
                "metadata": {"type": "object", "description": "Optional metadata dict."},
            },
            "required": ["segment", "key", "content"],
        },
    },
    {
        "name": "kb_get",
        "description": "Retrieve a specific entry by segment + key from the semantic knowledge base.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "segment": {"type": "string", "description": "KB segment name."},
                "key":     {"type": "string", "description": "Entry key to retrieve."},
            },
            "required": ["segment", "key"],
        },
    },
    {
        "name": "kb_search",
        "description": (
            "Search the semantic knowledge base using vector similarity. "
            "Embeds the query and returns the closest matching entries. "
            "Returns up to `limit` hits sorted by relevance (highest score first)."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "segment": {"type": "string", "description": "KB segment to search."},
                "query":   {"type": "string", "description": "Natural-language search query."},
                "limit":   {"type": "integer", "description": "Max hits to return (default 10, max 100)."},
            },
            "required": ["segment", "query"],
        },
    },
]


# ── JSON-RPC dispatch ─────────────────────────────────────────────────────────

def _dispatch(method: str, params: dict, req_id) -> dict:
    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities":    {"tools": {}},
                "serverInfo":      {"name": "semantic-kb", "version": SERVER_VERSION},
            },
        }
    if method in ("notifications/initialized", "notifications/cancelled"):
        return {}  # no response needed
    if method == "tools/list":
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {"tools": TOOLS, "nextCursor": None},
        }
    if method == "tools/call":
        name = params.get("name")
        args = params.get("arguments", {})
        handlers = {
            "kb_put":    _handle_kb_put,
            "kb_get":    _handle_kb_get,
            "kb_search": _handle_kb_search,
        }
        if name not in handlers:
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {"code": -32601, "message": f"Unknown tool: {name}"},
            }
        try:
            result_data = handlers[name](args)
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [{"type": "text", "text": json.dumps(result_data, indent=2)}],
                },
            }
        except Exception as e:
            return {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "content": [{"type": "text", "text": str(e)}],
                    "isError": True,
                },
            }
    return {
        "jsonrpc": "2.0",
        "id": req_id,
        "error": {"code": -32601, "message": f"Method not found: {method}"},
    }


# ── HTTP handler ──────────────────────────────────────────────────────────────

SESSION_ID = str(uuid4())


class McpHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):  # suppress default access logs
        pass

    def do_GET(self):
        if self.path in ("/health", "/healthz"):
            body = b'{"status":"ok"}'
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(404)
        self.end_headers()

    def _check_sidecar_secret(self) -> bool:
        """Return True if the request passes the optional inbound auth check."""
        if not SIDECAR_SECRET:
            return True
        token = self.headers.get("X-Sidecar-Token", "")
        return secrets.compare_digest(token.encode(), SIDECAR_SECRET.encode())

    def do_POST(self):
        if not self._check_sidecar_secret():
            self.send_response(401)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        try:
            length = int(self.headers.get("Content-Length", 0))
            if length < 0:
                self._json({"jsonrpc": "2.0", "id": None, "error": {
                    "code": -32700, "message": "Content-Length must not be negative",
                }})
                return
            if length > MAX_REQUEST_BODY:
                self._json({"jsonrpc": "2.0", "id": None, "error": {
                    "code": -32700, "message": f"Request body too large ({length} bytes, max {MAX_REQUEST_BODY})",
                }})
                return
            raw = self.rfile.read(length)
            msg = json.loads(raw)
        except Exception as e:
            self._json({"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": str(e)}})
            return

        if not isinstance(msg, dict):
            self._json({"jsonrpc": "2.0", "id": None, "error": {
                "code": -32700, "message": "Request body must be a JSON object",
            }})
            return
        method  = msg.get("method", "")
        params  = msg.get("params") or {}  # null params is treated as {}
        req_id  = msg.get("id")
        resp    = _dispatch(method, params, req_id)
        if not resp:  # notification — send 204
            self.send_response(204)
            self.end_headers()
            return
        self._json(resp)

    def _json(self, obj: dict):
        body = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Mcp-Session-Id", SESSION_ID)
        self.end_headers()
        self.wfile.write(body)


# ── Self-tests ────────────────────────────────────────────────────────────────

class SelfTests(unittest.TestCase):
    def setUp(self):
        # All tests run with mock embeddings; Qdrant is also mocked via monkey-patching.
        os.environ["VOYAGE_MOCK_EMBEDDINGS"] = "1"
        global MOCK_EMBED
        MOCK_EMBED = True  # re-read module-level for this process
        # Patch _qdrant so tests don't need a running Qdrant.
        self._store: dict[str, dict] = {}  # collection → {point_id → point}
        self._orig_qdrant = globals()["_qdrant"]
        globals()["_qdrant"] = self._mock_qdrant

    def tearDown(self):
        globals()["_qdrant"] = self._orig_qdrant

    def _mock_qdrant(self, path: str, method: str = "GET", body: dict | None = None) -> dict:
        """Minimal in-memory Qdrant mock for self-tests."""
        import re as _re
        # collection info
        if _re.match(r"^/collections/[^/]+$", path) and method == "GET":
            cname = path.split("/")[-1]
            if cname in self._store:
                return {"result": {"name": cname}}
            raise RuntimeError(f"HTTP 404: collection {cname} not found")
        # create collection
        if _re.match(r"^/collections/[^/]+$", path) and method == "PUT":
            cname = path.split("/")[-1]
            self._store.setdefault(cname, {})
            return {"result": True}
        # upsert points
        if _re.match(r"^/collections/[^/]+/points$", path) and method == "PUT":
            cname = path.split("/")[2]
            self._store.setdefault(cname, {})
            for pt in (body or {}).get("points", []):
                self._store[cname][pt["id"]] = pt
            return {"result": {"status": "ok"}}
        # get single point
        m = _re.match(r"^/collections/([^/]+)/points/([^/]+)$", path)
        if m and method == "GET":
            cname, pid = m.group(1), m.group(2)
            coll = self._store.get(cname, {})
            pt = coll.get(pid)
            if pt is None:
                raise RuntimeError(f"HTTP 404: point {pid} not found")
            return {"result": {"id": pid, "payload": pt.get("payload", {})}}
        # search
        if _re.match(r"^/collections/[^/]+/points/search$", path) and method == "POST":
            cname = path.split("/")[2]
            coll = self._store.get(cname, {})
            limit = (body or {}).get("limit", 10)
            results = [
                {"id": pid, "score": 0.9, "payload": pt.get("payload", {})}
                for pid, pt in list(coll.items())[:limit]
            ]
            return {"result": results}
        raise RuntimeError(f"mock: unhandled {method} {path}")

    # T1: kb_put + kb_get round-trip
    def test_t1_put_get_roundtrip(self):
        _handle_kb_put({"segment": "test", "key": "doc1", "content": "hello world"})
        result = _handle_kb_get({"segment": "test", "key": "doc1"})
        self.assertTrue(result["found"], "kb_get must find the stored entry")
        self.assertEqual(result["content"], "hello world")

    # T2: kb_search returns hits in order
    def test_t2_search_returns_hits(self):
        _handle_kb_put({"segment": "test", "key": "alpha", "content": "alpha content"})
        result = _handle_kb_search({"segment": "test", "query": "alpha", "limit": 5})
        self.assertIsInstance(result["hits"], list)
        self.assertGreater(len(result["hits"]), 0, "search must return at least one hit")

    # T3: VOYAGE_API_KEY absent + MOCK_EMBED off → graceful error
    def test_t3_missing_api_key_graceful_error(self):
        orig_mock = globals()["MOCK_EMBED"]
        orig_key = globals()["VOYAGE_KEY"]
        globals()["MOCK_EMBED"] = False
        globals()["VOYAGE_KEY"] = ""
        try:
            result = _dispatch("tools/call", {"name": "kb_put", "arguments": {
                "segment": "x", "key": "k", "content": "c"
            }}, 1)
            self.assertTrue(result["result"]["isError"], "missing API key must produce isError")
        finally:
            globals()["MOCK_EMBED"] = orig_mock
            globals()["VOYAGE_KEY"] = orig_key

    # T4: Qdrant unreachable → kb_put returns error, doesn't crash
    def test_t4_qdrant_unreachable(self):
        def _bad_qdrant(*_a, **_kw):
            raise RuntimeError("HTTP 503: connection refused")
        globals()["_qdrant"] = _bad_qdrant
        try:
            result = _dispatch("tools/call", {"name": "kb_put", "arguments": {
                "segment": "x", "key": "k", "content": "c"
            }}, 1)
            self.assertTrue(result["result"]["isError"], "Qdrant error must produce isError")
        finally:
            globals()["_qdrant"] = self._mock_qdrant  # restore

    # T5: key with path-traversal chars rejected
    def test_t5_path_traversal_rejected(self):
        result = _dispatch("tools/call", {"name": "kb_put", "arguments": {
            "segment": "x", "key": "../etc/passwd", "content": "c"
        }}, 1)
        self.assertTrue(result["result"]["isError"], "path-traversal key must produce isError")

    # T6: kb_search empty query returns empty list, no crash
    def test_t6_empty_query_no_crash(self):
        result = _handle_kb_search({"segment": "test", "query": "", "limit": 10})
        self.assertEqual(result["hits"], [])

    # T7: Voyage AI 429 → sidecar returns isError, stays up
    def test_t7_embedding_error_is_error(self):
        orig_embed = globals()["_embed"]

        def _raise_embed(_texts):
            raise RuntimeError("Voyage 429: rate limited")
        globals()["_embed"] = _raise_embed

        try:
            result = _dispatch("tools/call", {"name": "kb_put", "arguments": {
                "segment": "x", "key": "k", "content": "c"
            }}, 1)
            self.assertTrue(result["result"]["isError"])
        finally:
            globals()["_embed"] = orig_embed

    # T8: SSRF guard rejects public IPs; IPv4-mapped IPv6 Docker IPs are accepted
    def test_t8_ssrf_guard_private_ip_classification(self):
        self.assertFalse(_is_private("8.8.8.8"), "8.8.8.8 must not be private")
        self.assertFalse(_is_private("1.1.1.1"), "1.1.1.1 must not be private")
        self.assertTrue(_is_private("127.0.0.1"), "loopback must be private")
        self.assertTrue(_is_private("172.17.0.2"), "Docker bridge 172.17.0.2 must be private")
        self.assertTrue(_is_private("10.0.0.1"), "RFC-1918 10.x must be private")
        self.assertTrue(_is_private("192.168.1.1"), "RFC-1918 192.168.x must be private")
        # IPv4-mapped IPv6 — Docker bridge may resolve as ::ffff:172.17.0.2
        self.assertTrue(_is_private("::ffff:172.17.0.2"), "IPv4-mapped Docker IP must be private")
        self.assertFalse(_is_private("::ffff:8.8.8.8"), "IPv4-mapped public IP must not be private")

    # T9: kb_put with content > 8 KB returns isError
    def test_t9_kb_put_oversize_content_rejected(self):
        big_content = "x" * (HIT_CONTENT_CAP + 1)
        result = _dispatch("tools/call", {"name": "kb_put", "arguments": {
            "segment": "x", "key": "k", "content": big_content
        }}, 1)
        self.assertTrue(result["result"]["isError"], "oversized content must produce isError")

    # T10: kb_get with nonexistent key returns found=False
    def test_t10_kb_get_not_found(self):
        result = _handle_kb_get({"segment": "empty-seg-t10", "key": "definitely-absent"})
        self.assertFalse(result["found"], "kb_get on missing key must return found=False")
        self.assertEqual(result["key"], "definitely-absent")

    # T11: kb_put/kb_get with empty key returns isError
    def test_t11_empty_key_rejected(self):
        result = _dispatch("tools/call", {"name": "kb_put", "arguments": {
            "segment": "x", "key": "", "content": "c"
        }}, 1)
        self.assertTrue(result["result"]["isError"], "empty key in kb_put must produce isError")
        result_get = _dispatch("tools/call", {"name": "kb_get", "arguments": {
            "segment": "x", "key": ""
        }}, 1)
        self.assertTrue(result_get["result"]["isError"], "empty key in kb_get must produce isError")

    # T12: segment names containing URL special chars are rejected (segment injection guard)
    def test_t12_segment_url_injection_rejected(self):
        for bad_seg in ["prod?limit=1000", "col/../other", "col#fragment", "col@host"]:
            result = _dispatch("tools/call", {"name": "kb_put", "arguments": {
                "segment": bad_seg, "key": "k", "content": "c"
            }}, 1)
            self.assertTrue(
                result["result"]["isError"],
                f"segment {bad_seg!r} must produce isError (URL injection guard)"
            )

    # T13: SIDECAR_SECRET optional inbound auth
    def test_t13_sidecar_secret_auth(self):
        import socketserver
        import http.client
        import threading

        class _FakeHandler(McpHandler):
            def __init__(self):  # skip BaseHTTPRequestHandler.__init__
                pass

        orig_secret = globals()["SIDECAR_SECRET"]
        try:
            globals()["SIDECAR_SECRET"] = "t13-token"
            h = _FakeHandler()

            h.headers = {"X-Sidecar-Token": "t13-token"}
            self.assertTrue(h._check_sidecar_secret(), "correct token must pass auth")

            h.headers = {"X-Sidecar-Token": "wrong-token"}
            self.assertFalse(h._check_sidecar_secret(), "wrong token must fail auth")

            h.headers = {}
            self.assertFalse(h._check_sidecar_secret(), "missing token must fail auth")

            # Empty SIDECAR_SECRET disables the check entirely
            globals()["SIDECAR_SECRET"] = ""
            h.headers = {}
            self.assertTrue(h._check_sidecar_secret(), "empty secret disables check")
        finally:
            globals()["SIDECAR_SECRET"] = orig_secret

    # T14: GET /healthz returns 200 + {"status":"ok"}
    def test_t14_healthz_endpoint(self):
        import socketserver
        import http.client
        import threading

        with socketserver.TCPServer(("127.0.0.1", 0), McpHandler) as srv:
            port = srv.server_address[1]
            t = threading.Thread(target=srv.handle_request)
            t.start()
            conn = http.client.HTTPConnection("127.0.0.1", port)
            conn.request("GET", "/healthz")
            resp = conn.getresponse()
            self.assertEqual(resp.status, 200, "/healthz must return 200")
            body = json.loads(resp.read())
            self.assertEqual(body.get("status"), "ok", "/healthz body must be {status:ok}")
            t.join(timeout=2)

    # T15: POST with negative Content-Length returns JSON-RPC error (not a crash)
    def test_t15_negative_content_length_rejected(self):
        import socketserver
        import http.client
        import threading

        with socketserver.TCPServer(("127.0.0.1", 0), McpHandler) as srv:
            port = srv.server_address[1]
            t = threading.Thread(target=srv.handle_request)
            t.start()
            conn = http.client.HTTPConnection("127.0.0.1", port)
            conn.request("POST", "/", body=b"", headers={"Content-Length": "-1"})
            resp = conn.getresponse()
            body = json.loads(resp.read())
            self.assertIn("error", body, "negative Content-Length must return JSON-RPC error")
            self.assertEqual(body["error"]["code"], -32700)
            t.join(timeout=2)

    # T16: kb_search with Qdrant returning non-404 error propagates as isError (not empty hits)
    def test_t16_kb_search_qdrant_error_propagates(self):
        import unittest.mock

        def _fail_non_404(*args, **kwargs):
            raise RuntimeError("503 Service Unavailable")

        with unittest.mock.patch("__main__._qdrant", side_effect=_fail_non_404):
            result = _dispatch("tools/call", {
                "name": "kb_search",
                "arguments": {"segment": "default", "query": "hello"},
            }, req_id=1)
        self.assertTrue(
            result.get("result", {}).get("isError"),
            "non-404 Qdrant error during kb_search must propagate as isError",
        )

    # T17: params=null in JSON-RPC body is treated as {} (not AttributeError crash)
    def test_t17_null_params_returns_json_error(self):
        import socketserver, http.client, threading

        with socketserver.TCPServer(("127.0.0.1", 0), McpHandler) as srv:
            port = srv.server_address[1]
            t = threading.Thread(target=srv.handle_request)
            t.start()
            conn = http.client.HTTPConnection("127.0.0.1", port)
            body = b'{"jsonrpc":"2.0","id":1,"method":"tools/call","params":null}'
            conn.request("POST", "/", body=body, headers={"Content-Length": str(len(body))})
            resp = conn.getresponse()
            result = json.loads(resp.read())
            self.assertEqual(resp.status, 200, "null params must not crash the server")
            self.assertNotIn("AttributeError", str(result), "null params must not leak AttributeError")
            t.join(timeout=2)

    # T18: non-object JSON body returns JSON-RPC parse error (not AttributeError crash)
    def test_t18_non_object_body_returns_parse_error(self):
        import socketserver, http.client, threading

        with socketserver.TCPServer(("127.0.0.1", 0), McpHandler) as srv:
            port = srv.server_address[1]
            t = threading.Thread(target=srv.handle_request)
            t.start()
            conn = http.client.HTTPConnection("127.0.0.1", port)
            body = b'[1,2,3]'
            conn.request("POST", "/", body=body, headers={"Content-Length": str(len(body))})
            resp = conn.getresponse()
            result = json.loads(resp.read())
            self.assertEqual(resp.status, 200, "array body must not crash the server")
            self.assertIn("error", result, "non-object body must return a JSON-RPC error")
            self.assertEqual(result["error"]["code"], -32700)
            t.join(timeout=2)


def _run_self_tests():
    print("semantic_kb_mcp.py: running self-tests...", file=sys.stderr)
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromTestCase(SelfTests)
    runner = unittest.TextTestRunner(verbosity=2, stream=sys.stderr)
    result = runner.run(suite)
    if result.wasSuccessful():
        print("semantic_kb_mcp.py: self-test PASSED", file=sys.stderr)
        sys.exit(0)
    else:
        print("semantic_kb_mcp.py: self-test FAILED", file=sys.stderr)
        sys.exit(1)


# ── Entry point ───────────────────────────────────────────────────────────────

if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--test":
        _run_self_tests()

    # Startup SSRF guard — fail fast if QDRANT_URL points outside Docker.
    try:
        _validate_upstream_url(QDRANT_URL)
    except ValueError as e:
        print(f"semantic_kb_mcp.py: FATAL — {e}", file=sys.stderr)
        sys.exit(1)

    if not VOYAGE_KEY and not MOCK_EMBED:
        print(
            "semantic_kb_mcp.py: WARNING — VOYAGE_API_KEY is not set. "
            "kb_put and kb_search will fail until VOYAGE_API_KEY is provided. "
            "Set VOYAGE_MOCK_EMBEDDINGS=1 to use zero vectors for testing.",
            file=sys.stderr,
        )

    print(
        f"semantic_kb_mcp.py: starting on port {PORT} "
        f"(model={VOYAGE_MODEL}, dim={VOYAGE_DIM}, mock={MOCK_EMBED}, qdrant={QDRANT_URL})",
        file=sys.stderr,
    )

    server = ThreadingHTTPServer(("0.0.0.0", PORT), McpHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
