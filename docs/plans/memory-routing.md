<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/lane-cos-backend-autoplan-restore-20260712-125629.md -->
# memory-routing — raw emails to the semantic harness (Layer 2)

**Increment:** memory-routing (harness; builds on h8.1 semantic KB)
**Status:** Planned (2026-07-12). /autoplan complete. Ready to implement.
**Depends on:** h8.1 (semantic KB sidecar — Qdrant + `tool_override`), cos-polish (landed v0.79.0).
**Motivates:** fixes the CoS token blowup (~820k tokens/run from email body re-stuffing) and enables
semantic search over email history ("find emails about the ADP issue").

## The memory model (clarified)

There are **layers of one KB interface**, not an inside/outside split. Today everything is in the core.
This increment adds the L2 layer as the **default** for the CoS.

| Layer | What | Where | Active in the CoS after this increment |
|---|---|---|---|
| per-agent | short-term context + `mem_remember`/`recall` | core (`memory.redb`) | yes |
| **L1 — shared KB** (BM25 keyword) | `kb_put`/`get`/`search`, `ops:*` segments | **core** | **yes** — used when sidecar absent |
| **L2 — semantic KB** (h8.1) | same `kb_*` tools, vector search | **harness sidecar** (Qdrant + OpenAI) | **yes (default-on)** |
| L3 — operator brain (Track PERSONAL) | cross-deployment gbrain | external | planned |

Because h8.1 uses `tool_override: true`, all `kb_put`/`kb_search` calls route to Qdrant when the
sidecar is running — no agent-prompt change required for this increment's routing mechanism.

**Routing decision (D2, locked):** When the sidecar is on, ALL kb operations go to L2 (Qdrant +
OpenAI embeddings) — including `ops:briefs` and `ops:entities`. This is "binary mode":
sidecar-on = full L2, sidecar-off = full L1 core. No per-segment split in this increment.

## Goal

Route **all KB operations through the semantic sidecar by default**. The inbox agent stores each
raw email once to `mail:raw` (via `kb_put`), then works from `kb_search` results instead of
re-stuffing full bodies into context every turn. This:
1. breaks the ~820k token/run blowup,
2. enables "find emails about X" via semantic recall, and
3. keeps the core detachable — swap sidecar off and L1 takes over.

## Locked decisions (/autoplan 2026-07-12)

**D1 — Default-on with OpenAI embeddings:**
- Remove `profiles: [semantic]` from `qdrant` and `semantic-kb-mcp` in `docker-compose.yml`
- `cos` service gains `depends_on: {semantic-kb-mcp: {condition: service_healthy}}` (required)
- Embedding backend: OpenAI `text-embedding-3-small` (1536 dims), `OPENAI_API_KEY`
- `semantic_kb_mcp.py` swaps `voyageai` → `openai` package; Qdrant collection dimension: 1536
- `cos` service env gains `OPENAI_API_KEY`; `VOYAGE_API_KEY` removed from CoS path

**D2 — Everything → L2 when sidecar on (Option A):**
- `tool_override: true` on `semantic-kb-mcp` in `cos.agents.toml` → all kb calls go to Qdrant
- No per-segment routing, no distinct tool names, no sidecar proxy
- `ops:briefs`/`ops:entities` also land in Qdrant (gains semantic search as a bonus)
- L1 core remains the fallback for bare `agentd` without compose

**D3 — Eviction on L2:**
- Add TTL loop in `semantic_kb_mcp.py`: entries older than `SEMANTIC_MAX_AGE_DAYS` (default 30)
  and/or beyond `SEMANTIC_MAX_ENTRIES` (default 10000) per namespace are purged on startup
- Eviction runs once at boot, not on every write (avoid latency)

**D4 — PII / flight-event safety:**
- `mail:raw` content is email body — never log content in flight events
- `agentd` flight events for `kb_put` to `mail:raw` cap preview at 0 chars (existing
  `MAX_MEM_CONTENT_BYTES` guard applies at write time; event body stays empty)
- Add note to `DEPLOYMENT.md`: email bodies are sent to OpenAI Embeddings API for vector generation;
  operators should review OpenAI's data usage policies

## Design

1. **`docker/semantic_kb_mcp.py` — swap to OpenAI embeddings:**
   - Replace `voyageai` client with `openai` client
   - `embed(texts)` calls `client.embeddings.create(model="text-embedding-3-small", input=texts)`
   - Qdrant collection vector size: 1536 (OpenAI small) instead of current Voyage default
   - Add startup eviction loop per D3
   - Add `OPENAI_API_KEY` env read; remove `VOYAGE_API_KEY` read

2. **`docker-compose.yml`:**
   - Remove `profiles: [semantic]` from `qdrant` and `semantic-kb-mcp` services
   - `cos` service: add `OPENAI_API_KEY` env; add `depends_on: semantic-kb-mcp`; remove
     any `VOYAGE_API_KEY` env
   - `semantic-kb-mcp` service: replace `VOYAGE_API_KEY` with `OPENAI_API_KEY`
   - Optionally add `SEMANTIC_MAX_AGE_DAYS` and `SEMANTIC_MAX_ENTRIES` env vars for tuning

3. **`agentd/cos.agents.toml`:**
   - Add `semantic-kb-mcp` MCP server block to inbox-curator (and optionally orchestrator):
     ```toml
     [[agents.mcp_servers]]
     name = "semantic-kb"
     url = "http://semantic-kb-mcp:8000"   # or stdio path
     tool_override = true
     ```
   - Grant `KbRead`/`KbWrite` caps to inbox-curator (already has them for ops:*)
   - Inbox-curator prompt update: add step to `kb_put` each fetched email into `mail:raw` with
     provenance (message-id, thread-id, subject, date); then search via `kb_search` before fetching
     new emails to detect already-processed messages

4. **`docker/entrypoint.sh`:**
   - Add `OPENAI_API_KEY` to preflight check (exit 1 if missing for `cos` mode)
   - Remove Voyage-specific checks

## Non-goals (this increment)

- Per-segment L1/L2 routing (deferred; Option A covers the use case)
- Rebuilding the KB interface (h8.1 provides the L2 tools)
- The operator brain / gbrain (Track PERSONAL, L3)
- Multi-device migration (h8.3, horizon)
- Local embedding model (no-key path) — revisit if OPENAI_API_KEY becomes a blocker

## Acceptance criteria

1. `docker compose up` (no extra flags) starts Qdrant + semantic-kb-mcp alongside the CoS agents
2. `OPENAI_API_KEY` present → inbox agent `kb_put` each email to `mail:raw`; `kb_search` returns
   semantically relevant results; token usage drops materially vs baseline (target: ≤ 300k/run)
3. `OPENAI_API_KEY` absent → `semantic-kb-mcp` fails its health check; `cos` service exits with
   an actionable error message from `entrypoint.sh`
4. `agentctl watch [m]` shows `mail:raw` segment in the KB pane
5. Flight events for `kb_put` to `mail:raw` contain no email body content
6. Eviction: if `SEMANTIC_MAX_ENTRIES` is set to 5, adding 6 entries evicts the oldest 1 at startup
7. `cargo test` still passes (no Rust changes expected)
