# memory-routing — raw data to the harness (Layer 2), distilled state in the core

**Increment:** memory-routing (harness; builds on h8.1 semantic KB)
**Status:** Planned (2026-07-11). Not started.
**Depends on:** h8.1 (semantic KB sidecar — Qdrant + Voyage, `tool_override`), the CoS config.
**Motivates:** keeps the core runtime *super-light* (constitutional goal) and **also fixes the CoS token
blowup** (the inbox agent re-stuffing full email bodies into context — ~820 k tokens/run).

## The memory model (clarified)
There is no "memory inside / KB outside" split — there are **layers of one KB interface**, and today
everything runs in the **core**:

| Layer | What | Where | Active in the CoS today |
|---|---|---|---|
| per-agent | short-term context + `mem_remember`/`recall` | core (`memory.redb`) | yes |
| **L1 — shared KB** (BM25 keyword) | `kb_put`/`get`/`search`, `ops:*` segments | **core (`memory.redb`)** | **yes** — brief + entities live here |
| **L2 — semantic KB** (h8.1) | same `kb_*` tools, vector search | **harness sidecar** (Qdrant + Voyage) | **no** (needs `--profile semantic`) |
| L3 — operator brain (Track PERSONAL) | cross-deployment gbrain | external | planned |

Because h8.1 uses `tool_override`, the *same* `kb_put`/`kb_search` calls route to Qdrant instead of
`memory.redb` when the sidecar is on — no agent-prompt change.

## Goal
Route **bulk / raw data (emails, attachments, anything searched by meaning) → L2 harness (semantic)**, and
keep **distilled operational state (today's brief, open items, entities) → L1 core (fast keyword)**. This:
1. keeps the core light and detachable (the super-light thesis),
2. gives semantic recall ("find the thread about the ADP issue"), and
3. **breaks the token blowup** — the inbox agent stores each raw email **once** to L2, then works from
   summaries + `kb_search`, instead of re-sending full bodies into context every turn.

## Design (to harden via /autoplan)
1. **Enable the semantic sidecar for the CoS** — add `semantic-kb-mcp` (h8.1) to the CoS compose/config
   (`--profile semantic`, Qdrant + `VOYAGE_API_KEY`), with a segment routed to it (e.g. `mail:raw`). Keep
   the L1 `ops:*` segments in core.
2. **Inbox agent persists raw emails to L2** — on fetch, `kb_put` each message (headers + body) into the
   semantic `mail:raw` segment with provenance (message-id, thread, date). Then it distills to the L1
   `ops:entities`/`ops:briefs` segments as today. It never needs to re-fetch or re-stuff bodies.
3. **Retrieval** — "what did X say about Y" → `kb_search` against `mail:raw` (semantic); "today's brief" →
   L1 `ops:briefs` (keyword). The layer is chosen by segment, transparently.
4. **Graceful degradation** — if the semantic sidecar is absent, `mail:raw` writes fall back to L1 (or are
   skipped with a warning), so the CoS still runs without Qdrant/Voyage.

## Open decisions (for /autoplan)
- **D1 — Default on or opt-in?** Ship the CoS with the semantic sidecar on by default (needs Qdrant +
  Voyage key), or keep it opt-in (`--profile semantic`) with L1-only as the zero-config default.
- **D2 — What goes where exactly?** Raw emails → L2 only; distilled → L1 only; or mirror briefs to both
  (L1 for fast daily read, L2 for semantic history).
- **D3 — Retention/eviction** on `mail:raw` (emails accumulate) — reuse p5.6 eviction (max_entries/age).
- **D4 — Secrets/PII** — email bodies contain PII; confirm redaction rules for any flight-event previews of
  `mail:raw` writes (never log full bodies).

## Non-goals
- Rebuilding the KB interface (h8.1 already provides the L2 tools).
- The operator brain / gbrain (Track PERSONAL, L3).

## Done
The CoS stores raw emails in the semantic harness (L2), answers "find emails about X" via semantic search,
keeps briefs/entities in the light core (L1), and a full inbox run **no longer blows the token budget**
because bodies are stored once instead of re-stuffed into context. Works with the sidecar off (L1 fallback).
