#!/usr/bin/env python3
"""
semantic_kb_mcp — Layer-2 semantic KB MCP sidecar (h8.1 / memory-routing).

Exposes kb_put / kb_get / kb_search over HTTP/JSON-RPC (MCP spec 2024-11-05).
Backed by Qdrant (vector store) + OpenAI Embeddings API (text-embedding-3-small).

Environment variables:
  QDRANT_URL            Qdrant base URL (default: http://qdrant:6333)
  OPENAI_API_KEY        OpenAI key (required unless MOCK_EMBEDDINGS=1)
  EMBED_MODEL           Embedding model (default: text-embedding-3-small, 1536 dims)
  MOCK_EMBEDDINGS       Set to "1" to use zero vectors (testing, no key needed)
  PORT                  HTTP port to listen on (default: 8020)
  SEMANTIC_MAX_AGE_DAYS Evict entries older than N days at startup (default: 30, 0=disabled)
  SEMANTIC_MAX_ENTRIES  Evict oldest entries beyond this count per namespace (default: 10000, 0=disabled)

Security:
  - OPENAI_API_KEY is never logged or returned to callers.
  - SSRF guard on QDRANT_URL: loopback, RFC-1918, and link-local are allowed
    (Docker-internal use case); external IPs are blocked at startup.
  - Key/segment input validation (no path separators, ≤128 chars).
  - kb_search results capped at 100 hits × 8 KB each.

Self-test (no external services needed with MOCK_EMBEDDINGS=1):
  MOCK_EMBEDDINGS=1 python3 semantic_kb_mcp.py --test

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

OPENAI_API_URL   = "https://api.openai.com/v1/embeddings"
# Known model → output dimension mapping. EMBED_DIM is derived at startup from EMBED_MODEL.
# If EMBED_MODEL is set to an unlisted model, EMBED_DIM defaults to 0 and a warning is
# emitted — the collection will be created with dim=0, causing a Qdrant 400
# dimension-mismatch error on every kb_put. Set EMBED_MODEL to a known model or update this dict.
_EMBED_MODEL_DIMS: dict = {
    "text-embedding-3-small": 1536,
    "text-embedding-3-large": 3072,
    "text-embedding-ada-002": 1536,
}
MAX_SEARCH_HITS  = 100
HIT_CONTENT_CAP  = 8 * 1024          # 8 KB per hit
KEY_MAX_CHARS    = 128
QDRANT_TIMEOUT   = 10
EMBED_TIMEOUT    = 30
SERVER_VERSION   = "0.2.0"
MAX_REQUEST_BODY    = 4 * 1024 * 1024   # 4 MB — matches MCP HTTP client cap in Rust
MAX_QDRANT_RESPONSE = 4 * 1024 * 1024   # 4 MB — guard against oversized Qdrant responses
MAX_EMBED_RESPONSE  = 4 * 1024 * 1024   # 4 MB — guard against oversized embedding responses

# ── Config ────────────────────────────────────────────────────────────────────

QDRANT_URL      = os.environ.get("QDRANT_URL", "http://qdrant:6333").rstrip("/")
OPENAI_KEY      = os.environ.get("OPENAI_API_KEY", "")
EMBED_MODEL     = os.environ.get("EMBED_MODEL", "text-embedding-3-small")
EMBED_DIM       = _EMBED_MODEL_DIMS.get(EMBED_MODEL, 0)  # 0 = unknown model
MOCK_EMBED      = os.environ.get("MOCK_EMBEDDINGS", "0") == "1"
# attn.2 R1: production degrade mode, distinct from MOCK_EMBEDDINGS on purpose.
#   MOCK_EMBEDDINGS  = a TESTING flag. Deterministic zero vectors, full functionality,
#                      real collection names. The self-tests below run under it.
#   SEMANTIC_DEGRADED = a PRODUCTION flag, set when OPENAI_API_KEY is absent. Also uses zero
#                      vectors so kb_put/kb_get (point lookups by UUID5 of the key) keep
#                      working, but additionally (a) makes kb_search return an explicit empty
#                      rather than arbitrary nearest-neighbours, and (b) namespaces collections
#                      apart so zero vectors never land in a collection holding real ones.
# Conflating them would break the self-tests AND overload a test flag with production meaning.
#
# Tri-state, and AUTO is the default on purpose. This sidecar is a separate container from
# `cos`, so the cos entrypoint cannot export a variable into it, and Compose cannot express
# "set this only when OPENAI_API_KEY is empty". Requiring the operator to keep two settings in
# sync is a footgun: forget the second and you get the old failure (every kb_put erroring)
# instead of clean degradation. This process already knows whether it has a key, so it decides.
# An explicit value always wins, and the startup banner states which mode is active.
def _resolve_degraded(raw: str, has_key: bool, mock: bool) -> bool:
    """Resolve the tri-state. Pure so it can be table-tested: the AUTO branch is the ONLY
    path a key-less deployment takes (compose sends an empty string), and it previously had
    no coverage at all — replacing this whole block with `False` left every test green while
    killing the feature outright."""
    v = raw.strip().lower()
    if v in ("1", "true", "yes"):
        return True
    if v in ("0", "false", "no"):
        return False
    # AUTO: no key and not a mock/test run → degrade rather than fail every call.
    return (not has_key) and (not mock)


SEMANTIC_DEGRADED = _resolve_degraded(
    os.environ.get("SEMANTIC_DEGRADED", ""), bool(OPENAI_KEY), MOCK_EMBED
)
# Single source of truth for vector width. `_embed` used `EMBED_DIM or 1536` while
# `_ensure_collection` used a bare `EMBED_DIM`, so an unlisted EMBED_MODEL created a dim-0
# collection and then wrote 1536-dim vectors into it. Latent while the compose default is a
# known model; R1 makes the zero-vector path production-reachable, so pin both to one value.
EFFECTIVE_EMBED_DIM = EMBED_DIM or 1536
PORT            = int(os.environ.get("PORT", "8020"))
SIDECAR_SECRET  = os.environ.get("SIDECAR_SECRET", "")  # optional inbound auth token
SEMANTIC_MAX_AGE_DAYS = int(os.environ.get("SEMANTIC_MAX_AGE_DAYS", "30"))
SEMANTIC_MAX_ENTRIES  = int(os.environ.get("SEMANTIC_MAX_ENTRIES", "10000"))

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
    """Return embeddings for a list of texts. Uses zeros under MOCK_EMBED or SEMANTIC_DEGRADED."""
    if MOCK_EMBED or SEMANTIC_DEGRADED:
        return [[0.0] * EFFECTIVE_EMBED_DIM for _ in texts]
    if not OPENAI_KEY:
        raise RuntimeError(
            "OPENAI_API_KEY is not set and MOCK_EMBEDDINGS is not '1'. "
            "Either export OPENAI_API_KEY=<key> or set MOCK_EMBEDDINGS=1 for testing."
        )
    payload = json.dumps({
        "input": texts,
        "model": EMBED_MODEL,
    }).encode()
    req = urllib.request.Request(
        OPENAI_API_URL,
        data=payload,
        headers={
            "Authorization": f"Bearer {OPENAI_KEY}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=EMBED_TIMEOUT) as resp:
            raw = resp.read(MAX_EMBED_RESPONSE + 1)
            if len(raw) > MAX_EMBED_RESPONSE:
                raise RuntimeError(
                    f"OpenAI Embeddings response too large (> {MAX_EMBED_RESPONSE} bytes)"
                )
            body = json.loads(raw)
    except urllib.error.HTTPError as e:
        body_bytes = e.read(MAX_EMBED_RESPONSE + 1)
        if len(body_bytes) > MAX_EMBED_RESPONSE:
            raise RuntimeError(f"OpenAI Embeddings API error {e.code}: error body too large") from e
        try:
            detail = json.loads(body_bytes).get("error", {}).get("message", body_bytes.decode("utf-8", errors="replace"))
        except Exception:
            detail = body_bytes.decode("utf-8", errors="replace")
        raise RuntimeError(f"OpenAI Embeddings API error {e.code}: {detail}") from e
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
    except (urllib.error.URLError, OSError) as e:
        # attn.2 R1 (/review, maintainability specialist). Only HTTPError was normalised, so a
        # connection refusal, DNS failure or socket timeout raised URLError — which is NOT a
        # RuntimeError, so every `except RuntimeError` in this file silently missed it and the
        # exception escaped. At startup that killed the process from `_evict_all_collections`,
        # whose handler says "Qdrant not reachable yet — skip eviction" and could never
        # actually catch that case. With `restart: unless-stopped` now on the sidecar, a
        # restart while Qdrant is still coming up became an unbounded crash loop — exactly the
        # failure class R1 exists to remove, reintroduced one layer down.
        # HTTPError is a URLError subclass, so it must stay matched by the arm above.
        raise RuntimeError(f"Qdrant {method} {path} unreachable: {e}") from e


def _collection_name(segment: str) -> str:
    """Map a KB segment name to a Qdrant collection name.

    Qdrant rejects several characters in collection names — notably ':' (HTTP 422:
    "collection name cannot contain ':' char"). KB segments routinely use colons
    (ops:entities, ops:briefs, mail:raw), so map any char outside [A-Za-z0-9_] to '_'
    before prefixing. Applied here (the ONE mapping used by create/put/get/search), so
    writes and reads always resolve to the same collection.

    NOTE: this map is non-injective — segments that differ only by a sanitized char
    collapse to one collection (e.g. 'ops:x', 'ops.x', 'ops-x', 'ops x' all → 'kb_ops_x';
    NOT 'ops/x', which `_validate_segment` rejects outright). Because `_point_id` keys on
    `key` alone (no segment component), a collision would let same-key writes across the
    two logical segments overwrite each other and reads cross the boundary silently. The
    current segment set (mail:raw, ops:entities, ops:briefs, project:meta, project:research)
    sanitizes to five distinct names — see test_t20 for the injectivity fence. Revisit
    (fold the segment into `_point_id`, or hash the raw segment) before adding a segment
    that differs from an existing one only by a sanitized character.

    NOTE (attn.2 R1.4): under SEMANTIC_DEGRADED the prefix becomes `kbdegraded_`, so zero
    vectors are written to a SEPARATE collection. Without this, a degraded run mixes zero
    vectors into a collection holding real embeddings and permanently destroys its search
    quality — restoring OPENAI_API_KEY does not undo it, and nothing detects it. Isolating by
    name means a degraded run is fully reversible: set the key, and the real collection is
    exactly as it was. MOCK_EMBEDDINGS deliberately does NOT change the name, so the
    self-tests keep exercising the real mapping (see test_t20)."""
    safe = re.sub(r"[^A-Za-z0-9_]", "_", segment)
    return f"{_collection_prefix()}{safe}"


def _collection_prefix() -> str:
    """The collection-name prefix for the CURRENT mode. Single source of truth so that
    anything deriving a name stays in step with what `_collection_name` writes."""
    return "kbdegraded_" if SEMANTIC_DEGRADED else "kb_"


# Every prefix this server has ever written, regardless of current mode. The retention
# sweep must cover all of them: a collection written in one mode is invisible to the other
# mode's reads, and if it were also invisible to eviction it would live forever. Note
# "kbdegraded_" does NOT start with "kb_" ('d' != '_'), so listing both is required.
_ALL_COLLECTION_PREFIXES = ("kb_", "kbdegraded_")


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
                    # EFFECTIVE_EMBED_DIM, not EMBED_DIM: `_embed` already falls back to 1536
                    # for an unlisted model, so a bare EMBED_DIM here created a dim-0
                    # collection that then rejected every 1536-dim write.
                    "size": EFFECTIVE_EMBED_DIM,
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
    import time as _time
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
                    "ts":       _time.time(),
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

    # attn.2 R1.3 — degraded mode must return an EXPLICIT empty, never arbitrary hits.
    # Every stored vector and the query vector are zeros, so Qdrant's nearest-neighbour
    # search returns `limit` arbitrary points at meaningless scores. The CoS curator renders
    # whatever comes back as "## Open Items (carried forward)", so without this guard a
    # degraded run replaces an honest "nothing carried forward" with confident noise —
    # resolved items from up to SEMANTIC_MAX_AGE_DAYS ago, presented to the operator as open.
    # A silent wrong answer is strictly worse than the empty result it would replace.
    if SEMANTIC_DEGRADED:
        return {
            "hits": [], "segment": segment, "query": query,
            "note": "semantic search unavailable (degraded mode: no embeddings key) — "
                    "this is an explicit empty, NOT a statement that the segment has no matches",
        }

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


# ── Eviction ──────────────────────────────────────────────────────────────────

def _evict_segment(segment: str) -> int:
    """Evict this segment's collection IN THE CURRENT MODE. Convenience wrapper over
    `_evict_collection` for callers that hold a segment rather than a collection name."""
    return _evict_collection(_collection_name(segment))


def _evict_collection(cname: str) -> int:
    """Remove entries older than SEMANTIC_MAX_AGE_DAYS from a collection, BY NAME.

    Takes the name rather than a segment on purpose (attn.2 R1, /review). The old code
    resolved segment -> name via `_collection_name`, which R1 made mode-dependent, so the
    startup sweep enumerated the real `kb_*` collections and then issued every delete
    against `kbdegraded_*`. Real collections went un-evicted for the whole degraded window
    and degraded ones were only swept when a real collection of the same name happened to
    exist. Passing the enumerated name straight through removes the round-trip entirely.

    Returns the number of entries deleted.
    Note: SEMANTIC_MAX_ENTRIES count-based eviction is not yet implemented (env var is a
    no-op). TTL-only for now; add count-based pass here when needed."""
    import time as _time
    if SEMANTIC_MAX_AGE_DAYS <= 0:
        return 0
    try:
        _qdrant(f"/collections/{cname}")
    except RuntimeError:
        return 0  # collection doesn't exist yet

    cutoff = _time.time() - SEMANTIC_MAX_AGE_DAYS * 86400
    old_ids: list = []
    offset = None
    while True:
        body: dict = {
            "limit": 500,
            "with_vector": False,
            "with_payload": ["ts"],
        }
        if offset is not None:
            body["offset"] = offset
        result = _qdrant(f"/collections/{cname}/points/scroll", method="POST", body=body)
        r = result.get("result", {})
        for pt in r.get("points", []):
            ts = pt.get("payload", {}).get("ts", None)
            # ts is None for pre-memory-routing points (no ts field); treat as very old.
            if ts is None or ts < cutoff:
                old_ids.append(pt["id"])
        next_offset = r.get("next_page_offset")
        if not next_offset:
            break
        offset = next_offset

    if old_ids:
        _qdrant(
            f"/collections/{cname}/points/delete",
            method="POST",
            body={"points": old_ids},
        )
    return len(old_ids)


def _evict_all_collections() -> None:
    """Run TTL eviction across all existing collections at startup."""
    try:
        result = _qdrant("/collections")
        collections = [c["name"] for c in result.get("result", {}).get("collections", [])]
    except RuntimeError:
        return  # Qdrant not reachable yet — skip eviction
    # Sweep BOTH namespaces, always, and evict each collection by its own name.
    # attn.2 R1 (/review): this was a hardcoded "kb_" prefix plus cname[3:], then
    # _evict_segment re-derived the name through the mode-dependent _collection_name. Two
    # bugs fell out: in degraded mode the sweep enumerated the REAL kb_* collections but
    # deleted from kbdegraded_*, so real data went un-evicted for the whole degraded window;
    # and kbdegraded_* was never enumerated in either mode ("kbdegraded_x".startswith("kb_")
    # is False), so once a key was restored those collections were orphaned — unreachable by
    # kb_get/kb_search AND exempt from retention. Sweeping both prefixes keeps the TTL
    # promise across a mode flip in either direction.
    for cname in collections:
        if not any(cname.startswith(p) for p in _ALL_COLLECTION_PREFIXES):
            continue
        try:
            deleted = _evict_collection(cname)
            if deleted:
                print(
                    f"semantic_kb_mcp.py: evicted {deleted} old entries from {cname}",
                    file=sys.stderr,
                )
        except RuntimeError as e:
            print(f"semantic_kb_mcp.py: eviction warning for {cname}: {e}", file=sys.stderr)


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
        os.environ["MOCK_EMBEDDINGS"] = "1"
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

    # T3: OPENAI_API_KEY absent + MOCK_EMBED off → graceful error
    def test_t3_missing_api_key_graceful_error(self):
        orig_mock = globals()["MOCK_EMBED"]
        orig_key = globals()["OPENAI_KEY"]
        globals()["MOCK_EMBED"] = False
        globals()["OPENAI_KEY"] = ""
        try:
            result = _dispatch("tools/call", {"name": "kb_put", "arguments": {
                "segment": "x", "key": "k", "content": "c"
            }}, 1)
            self.assertTrue(result["result"]["isError"], "missing API key must produce isError")
        finally:
            globals()["MOCK_EMBED"] = orig_mock
            globals()["OPENAI_KEY"] = orig_key

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

    # T7: embedding API error → sidecar returns isError, stays up
    def test_t7_embedding_error_is_error(self):
        orig_embed = globals()["_embed"]

        def _raise_embed(_texts):
            raise RuntimeError("OpenAI 429: rate limited")
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

    # T19: eviction removes entries older than the cutoff
    def test_t19_eviction_removes_old_entries(self):
        import time as _time
        # Store two points: one old (ts in the past), one recent.
        old_id = _point_id("old-key")
        new_id = _point_id("new-key")
        cname = _collection_name("evict-test-seg")
        # Prime the mock store with the two points.
        self._store[cname] = {
            old_id: {
                "id": old_id,
                "vector": [],
                "payload": {"key": "old-key", "content": "old", "ts": 1.0},  # epoch = very old
            },
            new_id: {
                "id": new_id,
                "vector": [],
                "payload": {"key": "new-key", "content": "new", "ts": _time.time()},
            },
        }
        # Patch _qdrant to also handle delete + scroll + list.
        deleted_ids: list = []
        orig_qdrant = globals()["_qdrant"]

        def _mock_with_evict(path, method="GET", body=None):
            import re as _re
            # /collections (list all)
            if path == "/collections" and method == "GET":
                return {"result": {"collections": [{"name": cname}]}}
            # /collections/kb_evict_test_seg/points/scroll (hyphens sanitize to '_')
            if _re.match(r".*/points/scroll$", path) and method == "POST":
                return {
                    "result": {
                        "points": list(self._store.get(cname, {}).values()),
                        "next_page_offset": None,
                    }
                }
            # delete
            if _re.match(r".*/points/delete$", path) and method == "POST":
                for pid in (body or {}).get("points", []):
                    deleted_ids.append(pid)
                    self._store.get(cname, {}).pop(pid, None)
                return {"result": {"status": "ok"}}
            return self._mock_qdrant(path, method, body)

        globals()["_qdrant"] = _mock_with_evict
        orig_age = globals()["SEMANTIC_MAX_AGE_DAYS"]
        globals()["SEMANTIC_MAX_AGE_DAYS"] = 30
        try:
            deleted = _evict_segment("evict-test-seg")
            self.assertEqual(deleted, 1, "eviction must remove exactly the old entry")
            self.assertIn(old_id, deleted_ids, "old entry point_id must be in deleted list")
            self.assertNotIn(new_id, deleted_ids, "new entry must not be evicted")
        finally:
            globals()["_qdrant"] = orig_qdrant
            globals()["SEMANTIC_MAX_AGE_DAYS"] = orig_age

    # T20: colon (and other invalid) chars are sanitized out of collection names,
    # and the live CoS segment set maps to distinct collections. This is the
    # regression fence for the Qdrant "cannot contain ':'" bug — the MOCK_EMBED
    # tests above use colon-free segments and would still pass if _collection_name
    # regressed to f"kb_{segment}", so assert the mapping directly here.
    def test_t20_collection_name_sanitizes_and_is_injective(self):
        # Colons (and dots/hyphens/spaces) never survive into a collection name.
        self.assertEqual(_collection_name("ops:entities"), "kb_ops_entities")
        self.assertEqual(_collection_name("mail:raw"), "kb_mail_raw")
        for seg in ("ops:entities", "ops.x", "ops-x", "weird seg:v2"):
            cname = _collection_name(seg)
            self.assertRegex(cname, r"^kb_[A-Za-z0-9_]+$",
                             f"{seg!r} produced an invalid Qdrant collection name {cname!r}")
        # The real segment set the CoS uses must map to DISTINCT collections;
        # a collision would silently mix data (see _collection_name docstring).
        live_segments = [
            "mail:raw", "ops:entities", "ops:briefs",
            "project:meta", "project:research",
        ]
        names = [_collection_name(s) for s in live_segments]
        self.assertEqual(len(set(names)), len(names),
                         f"live segments collide under sanitization: {names}")

    # ── attn.2 R1: degraded mode (no OPENAI_API_KEY) ─────────────────────────
    # These pin the three properties that make running without an embeddings key SAFE
    # rather than silently wrong. Each is mutation-verified in the commit message.

    # T21 (R1.3): degraded kb_search returns an EXPLICIT empty, never arbitrary neighbours.
    # This is the load-bearing one. With zero vectors everywhere, Qdrant's nearest-neighbour
    # search happily returns `limit` arbitrary points, and the curator renders them as
    # "Open Items (carried forward)" — resolved items presented to the operator as open.
    # The guard must fire BEFORE any Qdrant call, so a stubbed _qdrant that would return
    # hits proves the short-circuit rather than an incidentally-empty collection.
    def test_t21_degraded_kb_search_returns_explicit_empty_not_arbitrary_hits(self):
        called = []

        def fake_qdrant(path, method="GET", body=None):
            called.append(path)
            return {"result": [{"id": "x", "score": 0.99,
                                "payload": {"key": "stale-item", "content": "RESOLVED weeks ago"}}]}

        orig_q, orig_deg = globals()["_qdrant"], globals()["SEMANTIC_DEGRADED"]
        globals()["_qdrant"] = fake_qdrant
        globals()["SEMANTIC_DEGRADED"] = True
        try:
            result = _handle_kb_search({"segment": "ops:briefs", "query": "open items", "limit": 10})
            self.assertEqual(result["hits"], [],
                             "degraded search must return NO hits; returning arbitrary "
                             "nearest-neighbours would surface resolved items as open")
            self.assertIn("note", result, "degraded empty must be labelled, not bare")
            self.assertIn("unavailable", result["note"].lower())
            self.assertEqual(called, [],
                             "degraded search must short-circuit BEFORE touching Qdrant; "
                             "a Qdrant round-trip means the guard is placed too late")
        finally:
            globals()["_qdrant"] = orig_q
            globals()["SEMANTIC_DEGRADED"] = orig_deg

    # T22 (R1.4): degraded writes are namespaced away from real embeddings.
    def test_t22_degraded_mode_isolates_collection_namespace(self):
        orig = globals()["SEMANTIC_DEGRADED"]
        try:
            globals()["SEMANTIC_DEGRADED"] = True
            degraded = _collection_name("ops:briefs")
            globals()["SEMANTIC_DEGRADED"] = False
            real = _collection_name("ops:briefs")
            self.assertNotEqual(degraded, real,
                                "degraded zero-vectors must not share a collection with real "
                                "embeddings — mixing them permanently destroys search quality "
                                "and restoring the key does not undo it")
            self.assertEqual(real, "kb_ops_briefs")
            self.assertEqual(degraded, "kbdegraded_ops_briefs")
            self.assertRegex(degraded, r"^[A-Za-z0-9_]+$",
                             "degraded collection name must still be Qdrant-legal")
        finally:
            globals()["SEMANTIC_DEGRADED"] = orig

    # T23: negative control for T22 — MOCK_EMBEDDINGS must NOT change the namespace.
    # Without this, someone "simplifying" the two flags into one would silently reroute
    # every self-test above onto a different collection and T20 would start lying.
    def test_t23_mock_embeddings_alone_does_not_change_namespace(self):
        self.assertTrue(MOCK_EMBED, "self-tests are expected to run under MOCK_EMBEDDINGS=1")
        self.assertFalse(SEMANTIC_DEGRADED, "self-tests must not run in degraded mode")
        self.assertEqual(_collection_name("ops:briefs"), "kb_ops_briefs",
                         "MOCK_EMBEDDINGS is a TESTING flag and must leave the real "
                         "collection mapping intact; only SEMANTIC_DEGRADED namespaces apart")

    # T24 (R1.5): the collection is declared with the SAME width the writer produces.
    # Must be exercised with an UNLISTED model, where EMBED_DIM is 0 and
    # EFFECTIVE_EMBED_DIM is 1536 — under the default model both are 1536, so a test that
    # only compares them there would still pass if `_ensure_collection` regressed to a bare
    # `EMBED_DIM`. Force them apart so the assertion can actually fail.
    def test_t24_collection_declared_with_effective_dim_not_raw_embed_dim(self):
        captured = {}

        def fake_qdrant(path, method="GET", body=None):
            if method == "GET":
                raise RuntimeError("404 not found")  # force the create path
            captured["body"] = body
            return {}

        orig_q   = globals()["_qdrant"]
        orig_dim = globals()["EMBED_DIM"]
        orig_eff = globals()["EFFECTIVE_EMBED_DIM"]
        globals()["_qdrant"] = fake_qdrant
        globals()["EMBED_DIM"] = 0          # simulate an unlisted EMBED_MODEL
        globals()["EFFECTIVE_EMBED_DIM"] = 1536
        try:
            _ensure_collection("dim-test-seg")
            size = captured["body"]["vectors"]["size"]
            self.assertEqual(size, 1536,
                             "collection must be created with EFFECTIVE_EMBED_DIM; a bare "
                             "EMBED_DIM yields a dim-0 collection that rejects every 1536-dim "
                             "write with a Qdrant 400")
            self.assertNotEqual(size, 0, "dim-0 collection is the bug this test exists for")
        finally:
            globals()["_qdrant"] = orig_q
            globals()["EMBED_DIM"] = orig_dim
            globals()["EFFECTIVE_EMBED_DIM"] = orig_eff

    # T26 (R1, /review): TTL eviction sweeps BOTH namespaces and evicts BY NAME.
    # The old scanner hardcoded "kb_" / cname[3:] and then let _evict_segment re-derive the
    # name through the mode-dependent _collection_name. That produced two bugs: in degraded
    # mode it enumerated the REAL collections but deleted from the degraded ones, and
    # kbdegraded_* was never enumerated in either mode. Assert the collection NAMES passed to
    # the evictor, in both modes — that pins "no re-derivation" as well as "both prefixes".
    def test_t26_eviction_sweeps_both_namespaces_by_name(self):
        seen = []

        def fake_qdrant(path, method="GET", body=None):
            if path == "/collections":
                return {"result": {"collections": [
                    {"name": "kb_ops_briefs"},
                    {"name": "kbdegraded_ops_briefs"},
                    {"name": "unrelated_thing"},
                ]}}
            return {}

        orig_q, orig_ev = globals()["_qdrant"], globals()["_evict_collection"]
        orig_deg = globals()["SEMANTIC_DEGRADED"]
        globals()["_qdrant"] = fake_qdrant
        globals()["_evict_collection"] = lambda cn: (seen.append(cn), 0)[1]
        try:
            for degraded in (True, False):
                globals()["SEMANTIC_DEGRADED"] = degraded
                seen.clear()
                _evict_all_collections()
                self.assertEqual(
                    sorted(seen), ["kb_ops_briefs", "kbdegraded_ops_briefs"],
                    f"with SEMANTIC_DEGRADED={degraded} the sweep must cover BOTH namespaces "
                    f"and pass each collection's OWN name through; got {seen}")
                self.assertNotIn("unrelated_thing", seen,
                                 "non-KB collections must not be touched")
        finally:
            globals()["_qdrant"] = orig_q
            globals()["_evict_collection"] = orig_ev
            globals()["SEMANTIC_DEGRADED"] = orig_deg

    # T27 (/review): an unreachable Qdrant must surface as RuntimeError, not URLError.
    # Every caller in this file guards with `except RuntimeError` — including
    # `_evict_all_collections`, whose handler literally says "Qdrant not reachable yet — skip
    # eviction" and could never catch that case, so startup died instead. Verified live: the
    # pre-fix module exits with `URLError: Connection refused`; the fixed one boots and serves.
    def test_t27_unreachable_qdrant_raises_runtimeerror_not_urlerror(self):
        import urllib.error
        # setUp swaps in a mock; `self._orig_qdrant` is the REAL function, which is what
        # this test is about. Point it at a closed port on loopback (fast refusal, no DNS).
        orig_url = globals()["QDRANT_URL"]
        globals()["QDRANT_URL"] = "http://127.0.0.1:59999"
        globals()["_qdrant"] = self._orig_qdrant
        try:
            with self.assertRaises(RuntimeError) as ctx:
                self._orig_qdrant("/collections")
            self.assertNotIsInstance(ctx.exception, urllib.error.URLError,
                                     "must be NORMALISED to RuntimeError, not re-raised as "
                                     "URLError — every caller here guards on RuntimeError")
            self.assertIn("unreachable", str(ctx.exception).lower())
            # And the startup sweep must survive it rather than killing the process: its
            # handler claims "Qdrant not reachable yet — skip eviction" and must be true.
            _evict_all_collections()
        finally:
            globals()["QDRANT_URL"] = orig_url
            globals()["_qdrant"] = self._mock_qdrant

    # T28 (/review): the tri-state resolution itself. Previously untested — replacing the
    # whole derivation with `SEMANTIC_DEGRADED = False` left 26/26 green while killing the
    # feature, because every other degraded test pokes the global directly. The empty-string
    # AUTO row is the one docker-compose.yml actually sends.
    def test_t28_degraded_tri_state_resolution(self):
        cases = [
            ("",      False, False, True,  "AUTO with no key must degrade — this is what compose sends"),
            ("",      True,  False, False, "AUTO with a key must stay in full mode"),
            ("",      False, True,  False, "AUTO under MOCK_EMBEDDINGS must not degrade (self-tests)"),
            ("1",     True,  False, True,  "explicit 1 forces degraded even with a key"),
            ("true",  True,  False, True,  "truthy words accepted"),
            (" YES ", True,  False, True,  "value is stripped and lowercased"),
            ("0",     False, False, False, "explicit 0 forces the old fail-every-call behaviour"),
            ("no",    False, False, False, "falsey words accepted"),
            ("garbage", False, False, True, "an unrecognised value falls through to AUTO"),
        ]
        for raw, has_key, mock, want, why in cases:
            self.assertEqual(
                _resolve_degraded(raw, has_key, mock), want,
                f"SEMANTIC_DEGRADED={raw!r} has_key={has_key} mock={mock}: {why}")

    # T25 (R1.5): the writer's width equals the declared width under the real config.
    def test_t25_embed_width_matches_declared_width(self):
        self.assertGreater(EFFECTIVE_EMBED_DIM, 0,
                           "a dim-0 collection rejects every write with a Qdrant 400")
        self.assertEqual(len(_embed(["probe"])[0]), EFFECTIVE_EMBED_DIM,
                         "embedding width must match the declared collection size")


def _run_self_tests():
    print("semantic_kb_mcp.py: running self-tests...", file=sys.stderr)
    # Hermetic w.r.t. the mode globals (/review). docker-compose.yml tells operators to set
    # SEMANTIC_DEGRADED=1 "to reproduce the no-key path"; doing that and then running the
    # suite used to produce five unrelated failures, because most tests assume full mode and
    # set/restore the global themselves. Force the baseline here; T23 asserts the forcing
    # actually happened, and the degraded tests still flip the global per-test.
    globals()["SEMANTIC_DEGRADED"] = False
    globals()["MOCK_EMBED"] = True
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

    if SEMANTIC_DEGRADED:
        print(
            "semantic_kb_mcp.py: DEGRADED — running without embeddings (OPENAI_API_KEY absent).\n"
            "    WORKING:  kb_put and kb_get (point lookups by key). The morning brief needs\n"
            "              only these, so the CoS pipeline is fully functional.\n"
            "    OFF:      kb_search returns an explicit empty. It does NOT mean 'no matches'.\n"
            f"    ISOLATED: writes go to `kbdegraded_*` collections, so real embeddings in\n"
            "              `kb_*` are untouched and set OPENAI_API_KEY to restore them intact.",
            file=sys.stderr,
        )
    elif not OPENAI_KEY and not MOCK_EMBED:
        print(
            "semantic_kb_mcp.py: WARNING — OPENAI_API_KEY is not set. "
            "kb_put and kb_search will fail until OPENAI_API_KEY is provided. "
            "Set MOCK_EMBEDDINGS=1 to use zero vectors for testing, or SEMANTIC_DEGRADED=1 "
            "to run the brief pipeline without semantic search.",
            file=sys.stderr,
        )

    if EMBED_DIM == 0 and not MOCK_EMBED:
        print(
            f"semantic_kb_mcp.py: WARNING — EMBED_MODEL='{EMBED_MODEL}' is not in the known "
            f"model→dimension table {list(_EMBED_MODEL_DIMS)}. "
            "Qdrant collection will be created with dim=0, causing dimension-mismatch errors on "
            "every kb_put. Set EMBED_MODEL to a known model or update _EMBED_MODEL_DIMS.",
            file=sys.stderr,
        )

    print(
        f"semantic_kb_mcp.py: starting on port {PORT} "
        f"(model={EMBED_MODEL}, dim={EMBED_DIM}, mock={MOCK_EMBED}, qdrant={QDRANT_URL}, "
        f"max_age_days={SEMANTIC_MAX_AGE_DAYS}, max_entries={SEMANTIC_MAX_ENTRIES})",
        file=sys.stderr,
    )

    _evict_all_collections()

    server = ThreadingHTTPServer(("0.0.0.0", PORT), McpHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
