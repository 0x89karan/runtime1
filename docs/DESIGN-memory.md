# DESIGN — Memory Substrate (Phase 5)

> **Status:** design source of truth for Phase 5. No code, no roadmap (the
> roadmap is `docs/PHASE-5-PLAN.md`). Written against v0.16.0 / Phase 4.6.
> **Reads:** DESIGN.md Part 4.3 / 5.3 / 7, CONVENTIONS.md, THREAT_MODEL.md,
> AUDIT-phase-4-6.md, and the code (`agent/mod.rs`, `checkpoint.rs`,
> `scheduler.rs`, `bus.rs`, `capability.rs`, `events.rs`, `surfaces/`).

The user's framing, verbatim: *"not just cater to context, short term, and long
term memory but a shared knowledge base with the right kind of segmentation when
required."* Four tiers, designed as one substrate.

This document is opinionated by mandate. Where DESIGN.md Part 7 and the measured
reality of the codebase disagree, the measured reality wins and the disagreement
is called out explicitly (see §4, storage substrate).

---

## 1. Goals and non-goals

### Goals
- **One substrate, four tiers.** Working memory (in-context), per-agent short-term,
  per-agent long-term, and a shared knowledge base — built on a single storage
  primitive and a single capability vocabulary, not four bolted-on systems.
- **Metered eviction.** Working-memory paging is driven by the existing token
  budget (p1.3): defer eviction while budget allows, force it at a hard ceiling so
  an agent never silently exceeds its budget *and* never silently drops state.
- **Durable, provenance-stamped knowledge.** Every long-term/KB write records who
  wrote it, when, in service of what task. The store is auditable from
  `flight.jsonl` and browsable from `/agents`.
- **Segmentation that maps to capabilities.** Shared-KB access reuses the p1.4
  least-privilege model: `KbRead { segment }` / `KbWrite { segment }`, prefix-
  matched exactly like `FsRead`/`FsWrite`, deny-by-default.
- **Hold the "super light" line.** The storage primitive must not breach the 4 MB
  CI binary guard. This is a hard constraint, not an aspiration (§4).
- **Zero agent-loop changes.** Memory is exposed as native tools through the
  existing `CallTools` effect. No new `AgentEffect`, no new `Block` types.

### Non-goals (Phase 5)
- **No semantic/vector retrieval *in the embedded store*.** The built-in
  `redb`-backed store (Layer 1, §4) is lexical only — token/substring search, no
  embeddings, no vector index. Semantic + keyword (hybrid) retrieval is delivered by
  an **optional external KB attached over MCP** (Layer 2, §4), whose embeddings come
  from a **remote embedding API** — preserving the remote-cognition lock (no weights
  on the `agentd` host). Layer 2's full integration (an HTTP/SSE MCP transport) is a
  later increment; Phase 5 ships Layer 1 plus the stdio-sidecar path. Decided in §9 Q1.
- **No automatic retrieval injection.** Retrieval is an explicit tool call, never
  silent context stuffing (§3.4 read semantics, justified).
- **No at-rest encryption.** Inherits the checkpoint §3.3 gap; tracked, not closed.
- **No MCP-server-direct KB access.** Only in-process agents reach the KB, via
  native tools. MCP servers never touch the store file (§7, removes a whole class
  of sandbox-placement bugs).
- **No CRDT/distributed consistency.** Single-tenant, mutually-trusting agents on
  one box. Last-writer-wins + provenance is sufficient (§4 write semantics).
- **Not a replacement for the real filesystem.** Per DESIGN.md 5.3 — a minimal real
  FS stays; the agent-facing world is the memory substrate + `/agents`.

---

## 2. The status quo, audited

Three memory-shaped substrates exist. Their fate:

### 2.1 In-context working memory — `Vec<Msg>` in `AgentTask` *(refactor, lightly)*
`agent/mod.rs:34` — `messages: Vec<Msg>`, token-budgeted by p1.3, no eviction
beyond run-end. This **is** Tier 1. It stays where it is and stays private to
`AgentTask` (the audit confirms it does not leak across `scheduler`/`bus` — the
scheduler only calls `provide_*`/`step`/`to_checkpoint`). Two audit findings touch
it:
- **AUDIT F-009 / §3** — the per-turn full clone (`agent/mod.rs:302`,
  `messages.clone()` into every `InferenceRequest`). A memory tier that grows
  context makes this O(context) per turn. **Prerequisite refactor** to `Arc<[Msg]>`
  (sequenced in the plan's p4.7) so paging doesn't multiply the clone cost.
- **AUDIT F-005 / F-006** — `inject_messages` ordering and ToolResult-turn append.
  Memory paging will add a second writer to `messages`; the injection invariant
  must be fixed (p4.7) before a second writer lands or the bug compounds.

What changes: a thin **context manager** seam is introduced inside `AgentTask`
(not the scheduler — keeps the mechanism/policy split) so paging can evict blocks
to Tier 2 and leave a marker. The `Vec<Msg>` representation is unchanged; only the
access discipline tightens.

### 2.2 `checkpoint.json` — `checkpoint.rs` *(keep; coexists, does NOT become the store)*
Persists the **full conversation history** of every agent atomically, mode 0600,
deleted on success (THREAT_MODEL §3). It is structurally long-term-memory-shaped,
and the temptation is to make it *the* long-term store. **We reject that.**
checkpoint and the memory store have incompatible lifetimes and fidelity
requirements:

| | `checkpoint.json` | memory store (`memory.redb`) |
|---|---|---|
| Purpose | exactly resume an interrupted run | what's worth keeping across runs |
| Fidelity | complete, verbatim | selective, distilled |
| Lifetime | ephemeral — deleted on success | durable — survives success |
| Trigger | every N turns / SIGTERM | explicit agent decision / distillation |

Merging them muddies both. checkpoint stays the crash-recovery snapshot of *live*
working memory + scheduler state; the memory store is a separate durable file.
**But** the audit's **F-011** (format_version validated *after* deserialization;
fixed tmp filename race) must be fixed first — the memory store reuses the exact
same "versioned single-file, atomic tmp→rename, version-probe-before-trust"
discipline, so checkpoint's version handling becomes the template. p4.7 owns it.

### 2.3 `flight.jsonl` — `flight_recorder.rs` *(keep; becomes a read source)*
Append-only truth of what every agent *did*. Not memory, but the provenance
ground-truth. Long-term distillation (Tier 2→3) and provenance stamping read
*from* the same event stream they write *to*. No change to the recorder; memory
adds new `EventKind` variants (§5).

---

## 3. The four tiers

```
            COST              LIFETIME          OWNER         BACKING
 Tier 1  tokens (live)     this turn..run     1 agent       Vec<Msg> (RAM)
 Tier 2  RAM + checkpoint  the run            1 agent       AgentTask scratch
 Tier 3  disk              forever            1 agent       memory.redb (agent ns)
 Tier 4  disk              forever            N agents      memory.redb (shared ns)
                                              ▲ segmented + capability-gated
```

The dividing line that must not leak: **Tier 1 is what the model currently sees
and pays for; Tier 2 is everything the agent has set aside this run that it is *not*
currently paying for.** Paging moves blocks across that line.

### Tier 1 — In-context working memory
- **What:** the `Vec<Msg>` the model sees each turn. The prompt window.
- **Lifetime:** populated from turn 0; lives until the run ends or a block is paged out.
- **Read/write path:** unchanged inference loop. The new operation is **paging out**:
  the context manager moves the oldest evictable blocks (completed tool-result /
  observe pairs, never the system task or an unanswered tool_use) to Tier 2 and
  replaces them with a single `Block::Text` *page marker*:
  `"[paged: 3 earlier tool results → short-term, recall with mem_page(get)]"`.
- **Eviction policy (opinionated, hybrid — this is the hard new question):**
  - The **runtime owns the signal and the hard ceiling.** Before each
    `InferenceRequest`, the context manager estimates request tokens. Two thresholds,
    both relative to the agent's `token_budget` (p1.3):
    - **Soft (default 80%):** inject a one-line system note ("working memory near
      budget; consider mem_page to set aside detail") and advertise the `mem_page`
      tool. The **agent decides** what to page (MemGPT/Letta self-paging — borrow the
      mechanism). *Defer eviction while budget allows* — per the user's framing, a
      well-funded agent is never force-evicted.
    - **Hard (default 95%):** the runtime **force-pages** oldest evictable blocks
      until the request fits, emitting `memory_paged { forced: true }`. This is the
      guarantee that an agent never silently blows its budget and never silently
      loses state — it is set aside, not dropped.
  - Why hybrid and not pure self-paging (MemGPT) or pure heuristic: pure self-paging
    can run away (the agent ignores the hint and the budget guard kills it mid-task,
    losing context); pure heuristic evicts things the agent wanted. Runtime-as-floor +
    agent-as-policy gets both — the agent steers, the runtime guarantees the invariant.

### Tier 2 — Per-agent short-term
- **What:** within-run scratchpad. Paged-out Tier-1 blocks land here; the agent can
  also write to it deliberately (`mem_scratch put/get`).
- **Lifetime:** the run. Captured in `checkpoint.json` (so a SIGTERM/restart restores
  it), discarded when the run completes — it does **not** auto-promote to Tier 3.
- **Ownership:** single agent. No cross-agent access (that's Tier 4's job).
- **Backing:** an in-memory structure on `AgentTask` (`short_term: Vec<MemItem>`),
  added to `AgentCheckpoint` (format_version bump — see §6).
- **Boundary clarification (the "don't leak" rule):** Tier 2 is *not* in the prompt
  window — reading it costs a tool call + re-injection, which costs tokens, which is
  the point (it's "swapped out"). The moment something is in `messages`, it's Tier 1.

### Tier 3 — Per-agent long-term
- **What:** durable, agent-owned memory. Survives run completion and restart.
- **Lifetime:** forever (subject to eviction floor, §4 failure modes).
- **Ownership:** the authoring agent's namespace (`agent/<id>/...`). An agent reads/writes
  its own Tier-3 without a KB capability (it's *its own* memory); cross-agent reads of
  another agent's Tier-3 require a `KbRead` grant on that namespace (Tier-3 is just a
  reserved, per-agent slice of the same segmented store as Tier 4).
- **Read/write path:** `mem_remember { content, tags }` (write), `mem_recall { query }`
  (read). Selective retention: nothing auto-promotes; the agent (or an end-of-run
  distillation step) explicitly remembers. Every entry carries provenance (§4).
- **Backing:** `memory.redb`, namespace `agent/<id>`.

### Tier 4 — Shared knowledge base
The interesting tier. Multiple agents read/write. Segmentation is §4.

```
 AgentTask (Tier 1: Vec<Msg>)
    │  step() → CallTools([mem_* / kb_*])         ← no new AgentEffect
    ▼
 ToolRegistry.invoke (capability check: KbRead/KbWrite)   ← p1.4 boundary
    ▼
 MemoryStore (trait)  ──────────────►  redb (memory.redb)
    ├─ short_term (RAM, per AgentTask)               Tier 2
    ├─ namespace "agent/<id>"                         Tier 3
    └─ namespace "<segment>" (canon/log/scratch)      Tier 4
```

---

## 4. The shared KB

### Segmentation model — three composing axes
1. **Namespace (primary).** A `:`-delimited hierarchical string, e.g.
   `project:acme`, `canon:security`, `agent/orchestrator`. The partition key.
   Capability prefixes match on namespace exactly like FS path prefixes
   (`KbRead { segment: "project:" }` grants all `project:*`). This is the axis that
   carries authorization.
2. **Mutability class (per namespace, set at segment creation).** Three classes,
   which decide write semantics:
   - `canon` — read-only after creation; only an admin/seed config writes it (no
     agent is granted `KbWrite` on a canon segment). The trusted reference layer.
   - `log` — append-only; every write is a new immutable, provenance-stamped entry.
     Never conflicts. The default for agent-authored findings.
   - `scratch` — mutable; last-writer-wins with a monotonic `version` + provenance.
     For coordination state multiple agents revise.
3. **Provenance (metadata on every entry, also a read filter).** Who authored it.
   Not an authorization axis (that's namespace) — a *trust/attribution* axis: a
   reader can filter "only entries authored by agent X" or weight by author.

**Rejected axes** (one line each): *capability-tier-as-segment* — conflates authz
with data layout; authz already lives in `KbRead`/`KbWrite` over namespaces.
*Per-task segment* — too granular; provenance metadata captures task without
fragmenting the keyspace. *Time-window segment* — age is an eviction concern, not a
partition.

### Access model — capability vocabulary
New variants on `Capability` (`capability.rs`), following the `FsRead`/`FsWrite`
prefix pattern and the p4.6 backward-compat discipline:

```rust
Capability::KbRead  { segment: String }   // prefix match, like FsRead
Capability::KbWrite { segment: String }   // prefix match, like FsWrite
```

- **Matching:** reuse the `satisfies` prefix logic — `KbRead { segment: "project:" }`
  satisfies a required `KbRead { segment: "project:acme" }`. Empty segment in a
  *granted* cap is a non-grant (fail-safe deny, mirroring the empty-prefix guard at
  `capability.rs:108`).
- **Deny-by-default:** an agent with `capabilities: None` (unrestricted, back-compat)
  can read/write KB — consistent with today's "None = unrestricted." An agent with
  `Some([...])` needs the explicit `KbRead`/`KbWrite`. `Some([])` denies all KB.
- **Backward compat:** new *variants* don't break old configs (they simply never
  appear); no `#[serde(default)]` needed on the enum. The p4.6 `#[serde(default)]`
  pattern applies if we later add *fields* to these variants (e.g. `classes: Vec<..>`).
- **Tier-3 self-access:** an agent's own `agent/<id>` namespace is readable/writable
  by that agent without an explicit grant (it's its own memory); the runtime injects
  an implicit `KbRead/KbWrite { segment: "agent/<self-id>" }`. Cross-agent Tier-3
  read needs the explicit grant.

### Write semantics
- **canon:** rejected at write time unless the writer is the seed/admin path (no
  agent holds `KbWrite` on canon) → `is_error` tool result.
- **log:** append a new `KbEntry` keyed `(segment, monotonic_seq)`. Immutable. No
  conflict possible.
- **scratch:** read-modify-write under a redb transaction; `version += 1`; previous
  value overwritten; provenance updated. Last-writer-wins is correct for
  mutually-trusting single-tenant agents — the flight log + provenance record *who*
  overwrote, which is the accountability the lock-free choice trades for.

**Provenance schema** (stamped by the runtime, not the agent — agents cannot forge it):
```rust
struct Provenance {
    agent_id:  String,   // who wrote
    turn:      u32,      // at which turn
    task_fp:   String,   // first 16 hex of sha256(task) — links to the originating task
    ts:        String,   // RFC3339, from the same clock as flight.jsonl
    citation:  Option<String>, // optional agent-supplied source ("flight:<event>", URL, file)
}
```

### Read semantics — explicit tool, never automatic injection *(opinionated)*
Retrieval is a **tool call** (`kb_search`, `kb_get`, `mem_recall`), never automatic
context stuffing. Three reasons, in order:
1. **Metered cognition (the lock).** Automatic injection spends tokens the agent
   didn't ask for, on every turn, unpredictably — it fights the budget guard.
2. **Auditability.** A tool call is one `kb_search` flight event with a query and a
   result count. Silent injection is invisible in the log and breaks replay.
3. **Proven pattern + zero loop change.** MemGPT/Letta self-paging is tool-driven;
   it fits `CallTools` with no new effect. Automatic retrieval (Mem0-style) is a
   genuine open question (§9 Q2) deferred until proven necessary.

`kb_search` API: `{ segment?: String, query: String, author?: String, limit?: u32 }`
→ ranked entries (lexical BM25-ish over a tokenized index; §below). Emits
`kb_search { segment, query_preview, hits }`. `kb_get { segment, key }` →
one entry + provenance.

### Storage substrate — **redb** (pure-Rust embedded KV), *not* SQLite
DESIGN.md Part 7 says "SQLite + sqlite-vec + markdown wiki." **We override that for
Phase 5**, and here is the defense, because the doc line predates the measured
footprint:

- **The binary is 3.1 MB against a hard 4 MB CI guard** (`.github/workflows/ci.yml`).
  Headroom ≈ 0.9 MB.
- **rusqlite with `bundled` SQLite** compiles the SQLite C amalgamation into the
  binary — ~1.0–1.5 MB even size-optimized. That **breaches the 4 MB guard.**
- **Non-bundled (dynamic) SQLite** breaks the static-musl property the threat model
  explicitly values (THREAT_MODEL §5.3: "no dynamic library loading ⇒ no LD_PRELOAD").
  Not an option.
- **redb** (pure Rust, single-file, ACID, MVCC, crash-safe) compiles to
  **~0.4–0.6 MB** with no C toolchain and no dynamic link. It fits the budget with
  headroom to spare and preserves the static binary.

**Decision: `redb`.** One file `memory.redb`, multiple tables keyed by
`(namespace, key)`. Worst-case size addition: **~0.6 MB** (well under the ≥1 MB
"defend it" bar, and under the CI ceiling). FTS5-grade ranked search is *not*
free in redb — so Phase 5 ships a **simple tokenized inverted index** maintained in
a second redb table (lowercased word → posting list), with BM25-lite scoring
computed in Rust. Good enough for a KB MVP; ranked FTS and vectors are deferred
(§9 Q1). **Compiled markdown wiki — the content format, not a separate mechanism.**
Long-term / `canon` / `log` entries are markdown documents, and the Tier-2→Tier-3
distillation (p5.6) *compiles* salient run knowledge into them — the Karpathy
"llm-wiki" pattern (DESIGN.md 4.3): a human-readable, agent-maintained wiki written at
distillation time and read back by lexical search, rather than reconstructed by runtime
vector retrieval. (Mem0 / `claude-mem` are the auto-capture→compress→inject
counter-design; see §9 Q2/Q3.)

### Two storage layers — embedded (Layer 1) + external hybrid KB over MCP (Layer 2)

The single `redb` decision above covers **Layer 1**: the always-present, zero-dependency,
in-binary store for per-agent short/long-term memory and a lexical shared KB. It works
offline, ships in the 4 MB binary, and is what Phase 5 builds.

Semantic search at scale is delivered by **Layer 2: an optional external hybrid KB
attached as an MCP server** — *not* by adding a vector engine to `agentd`. This is the
cleaner split and it falls out of the existing architecture: MCP is already the tool
ABI, and the runtime already spawns, sandboxes, and capability-scopes MCP servers. So
"connect an external semantic+keyword KB to the AgentOS container" is just "attach a
tool server." Benefits: the heavy index/embedding machinery lives **outside** the
binary (holds the super-light line) and **outside** the `agentd` host (holds the
remote-cognition lock — see embeddings below).

- **Agent-facing surface is identical across layers.** The same `kb_search` / `kb_get`
  / `kb_put` tools (§4 read/write semantics), capability-gated by `KbRead`/`KbWrite`
  (+ `Mcp { server, tools }` for the external server). An agent cannot tell whether
  retrieval was lexical-local or hybrid-remote — only the backing changes.
- **Hybrid engine choice (Layer 2):** an engine that fuses BM25/keyword with vector
  search (reciprocal-rank fusion). Candidates, in ethos-fit order: **HelixDB** (a Rust
  graph+vector OLTP DB — best fit for the Rust/light line, and its graph layer lets the
  KB model entry-to-entry links and provenance as a *graph*, not just a flat vector
  index: the A-MEM / Zettelkasten angle); **Postgres + `pgvector` + native FTS** (one
  container does both halves — the boring, robust default); **Qdrant** or **Meilisearch**
  (purpose-built, light). **`gbrain`** (garrytan) is a working reference of this exact
  pattern — a repo-semantic brain exposed over MCP — and a drop-in candidate for the
  code-aware case. A KB-builder agent (cf. `Understand-Anything`) can populate a graph
  segment by turning a codebase/corpus into a queryable knowledge graph. The engine, not
  `agentd`, owns the index.
- **Connection mechanism:** today's MCP client is **stdio-only** (p0.5), so the first
  version is a **co-located stdio sidecar** — `agentd` spawns the KB server like the
  filesystem server, zero new runtime code. A **networked KB container** needs an
  **HTTP/SSE MCP transport** added to `agentd` (a later increment); when it lands, the
  p4.6 Landlock V4 TCP-port rules are the enforcement layer — grant the KB server
  `Net { ports: [<kb-port>] }`. (On kernels < 6.7 that port rule degrades to a deny-all
  network namespace — a safe fallback emitted with a startup warning, fixed in p4.7 /
  AUDIT F-002.)

**Embeddings — remote API, lock preserved (decided, §9 Q1).** Semantic retrieval needs
an embedding model, and embeddings are inference-shaped, so the **Layer 2 KB sidecar
computes them by calling a remote embedding API** — never `agentd`, and no local
embedding weights anywhere in AgentOS. **Anthropic offers no first-party embeddings
API**, so the canonical pairing with Claude is **Voyage AI** (the `voyage-3` family;
Voyage is part of MongoDB); Cohere and OpenAI embedding APIs are equally viable. The
exact Voyage model id should be read from Voyage's own docs at integration time (it is
outside the Claude API surface). The KB server is the only component that holds an
embedding-API key — and per AUDIT **F-001**, that key (and `ANTHROPIC_API_KEY`) must be
kept out of the sidecar's environment unless explicitly granted (the p4.7 env-allowlist
fix is the control).

### Worked example — A logs a finding, B retrieves it
Agents: `scout` (caps: `KbWrite { "project:acme" }`, `Net { ports:[443] }`) and a
later-spawned `analyst` (caps: `KbRead { "project:" }`, no Net).

1. `scout` finishes a web search, calls
   `kb_put { segment: "project:acme:findings", class: "log",
             content: "ACME's API rate limit is 100 req/min",
             citation: "https://acme.dev/docs" }`.
2. `ToolRegistry.invoke` checks `KbWrite { "project:acme:findings" }` against
   scout's grant `KbWrite { "project:acme" }` → prefix match → allowed.
3. `MemoryStore.append` writes `(("project:acme:findings", seq=7), KbEntry{ body,
   provenance: { agent_id:"scout", turn:4, task_fp:"a91f…", ts:"2026-…",
   citation:"https://acme.dev/docs" } })` and updates the inverted index.
   Flight: `memory_write { tier:4, segment:"project:acme:findings", class:"log",
   bytes:41 }`.
4. Later, `analyst` is spawned. It calls
   `kb_search { segment:"project:acme", query:"rate limit", limit:5 }`.
5. Capability check: required `KbRead { "project:acme" }` ⊑ granted
   `KbRead { "project:" }` → allowed. Flight: `kb_search { segment:"project:acme",
   query_preview:"rate limit", hits:1 }`.
6. The tool returns the entry **with its provenance** ("scout, turn 4, cited
   acme.dev/docs"). `analyst` re-injects only that one result into Tier 1 (paying
   tokens for exactly what it retrieved), and proceeds — without ever having Net.
   The finding crossed agents, capability-gated, fully recorded, with attribution.

---

## 5. Integration points

### With p1.1's state machine
**No new `AgentEffect` variants, no new `Block` types.** Memory is native tools
(`mem_page`, `mem_scratch`, `mem_remember`, `mem_recall`, `kb_put`, `kb_get`,
`kb_search`) invoked through the existing `CallTools` effect and `ToolRegistry`.
The one runtime-side hook is the context manager's **pre-inference paging check**
inside `AgentTask::step_need_infer` (a method call, not a new effect). The eviction
"signal" to the agent is a `Block::Text` system note appended to working memory —
reusing the existing `Perceive`/injection machinery, not a new mechanism. This is
the lightest possible integration and matches "tools are syscalls."

### With p1.4 capabilities
- New vocabulary: `KbRead { segment }`, `KbWrite { segment }` (§4).
- Defaults: `None` = unrestricted (back-compat); `Some([...])` = explicit grants;
  `Some([])` = deny all. Implicit self-grant for `agent/<self-id>` (Tier 3).
- Deny-by-default discipline: `satisfies` returns false for any KB requirement not
  covered by a grant; empty-segment grant is a non-grant (fail-safe).
- `Tool::required_capability_for` on each memory tool computes the required
  `KbRead`/`KbWrite { segment }` from the call's `segment` argument — exactly as
  `read_file` computes `FsRead { prefix: path }` today (`native.rs:43-46`).

### With `surfaces/` — `/agents/<id>/memory/...` FUSE
Read-only, following the existing inode scheme (root=1, dirs from 1010 step 10,
`agents_fs.rs`). The snapshot (`snapshot.rs`) gains a per-agent memory view
(populated best-effort, `try_write` like today). Proposed layout:
```
/agents/<id>/memory/
    short_term         # current scratchpad, text dump (Tier 2)
    long_term/         # dir: one file per Tier-3 entry key
        <key>          # body + provenance footer
/agents/kb/            # shared KB browse (Tier 4)
    <segment>/         # dir per namespace the *operator* (not an agent) may read
        <key>          # entry body + provenance
```
KB exposure under `/agents/kb/` is an **operator** view (the human at the console),
not an agent capability — it does not bypass `KbRead` for agents. Read-only;
larger entries make AUDIT **F-004** (the FUSE `read()` offset+size overflow) a
must-fix prerequisite (p4.7) before this lands.

### With `events.rs` — new `EventKind` variants
Matching the CONVENTIONS.md table style. (All six pre-existing undocumented kinds
from AUDIT F-013 are also added to the table in p4.7 — separate from these.)

| kind | when | data shape |
|---|---|---|
| `memory_read` | a Tier-2/3/4 read returns | `{tier, segment?, agent, items}` |
| `memory_write` | a Tier-3/4 write commits | `{tier, segment?, class?, agent, bytes}` |
| `memory_paged` | Tier-1 → Tier-2 paging | `{agent, blocks, forced, freed_tokens_est}` |
| `memory_distilled` | Tier-2 → Tier-3 promotion | `{agent, items, segment}` |
| `memory_evicted` | capacity/age eviction | `{segment, key, reason}` |
| `kb_search` | a `kb_search` tool call returns | `{segment?, query_preview, hits}` |
| `memory_unavailable` | store open/read failed | `{stage, error}` |
| `memory_quarantined` | corrupt store → `.corrupt` | `{path}` |

KB capability denials reuse the existing `capability_denied` event (no new kind).

---

## 6. Migration: checkpoint.json → memory store

**Decision: coexist, distinct roles (justified in §2.2). No migration of
checkpoint *into* the store.** What changes:

1. **`AgentCheckpoint` gains `short_term: Vec<MemItem>`** (Tier 2 is run-scoped and
   must survive SIGTERM/restart). This bumps `FORMAT_VERSION` 1 → 2.
2. **Backward-compat read path (required).** AUDIT **F-011** must be fixed first: a
   `{ format_version }` probe struct read *before* the full deserialize. Then a v1
   checkpoint (no `short_term`) loads via `#[serde(default)]` on the new field →
   empty short-term. A v2 checkpoint on a v1 binary is refused (probe says "too
   new"), not misclassified as corrupt. This is the backward-compat window: v2
   readers accept v1 and v2; v1 readers reject v2 cleanly.
3. **The long-term store (`memory.redb`) has its own version key** in a `meta`
   table, evolved by the same probe-before-trust discipline (the checkpoint fix is
   the template). It is **never deleted on success** (unlike checkpoint).
4. **Threat-model implications (THREAT_MODEL §3.2/§3.3):** `memory.redb` inherits
   the mode-0600 requirement (created with restrictive perms) and the **deferred
   at-rest encryption gap** — but its risk window is *larger* than checkpoint's
   because it is durable, not deleted on success. §8 expands this.

---

## 7. Sandbox interactions

The p4.6 Net-shaped invariant trap is worth a deliberate check here, and it yields
a clean decision:

- **MCP servers get no direct KB access in Phase 5.** Only in-process agents reach
  the store, via native tools. Therefore `memory.redb` lives **outside every
  server's `AllowFsRead`/`AllowFsWrite` prefix** — a sandboxed server can neither
  read nor corrupt the shared KB. This removes an entire class of "where does the
  store file live relative to the sandbox" bugs.
- **Explicit invariant to assert (the p4.6-shaped check):** *granting an agent
  `KbWrite { segment }` must never be implemented by also granting the agent's MCP
  servers `AllowFsWrite` over the store path.* The two are unrelated; conflating
  them would let a compromised server bypass segmentation and corrupt canon. A
  startup assertion (`memory.redb` path ∉ any server's FS prefixes) enforces it —
  mirroring how p4.7 should assert "declared access class ⇒ at least one rule."
- **AUDIT F-001 (env leak) interaction:** the KB is a new place secrets could land
  (an agent writes an API response containing a token into a `log` segment another
  agent later reads). The env-leak fix (p4.7) and the §8 KB-exfiltration note
  together bound this; truncation/redaction discipline (PREVIEW_CHARS) applies to
  KB *flight events* but **not** to KB *contents* (the KB stores full bodies by
  design) — so the operational guidance "never put a secret in memory" mirrors the
  existing "never pass a secret as a tool arg" (THREAT_MODEL §2.2).

When (future) an MCP server *does* need KB access, the right design is
agentd-mediated (a built-in MCP endpoint the server calls back into), never raw
file access — explicitly deferred.

---

## 8. Threat-model amendments (sketch — become real edits in Phase 5)

A new **§7 — Memory substrate** for THREAT_MODEL.md, sketched:

- **7.1 Shared-KB cross-agent information flow.** Segmentation (`KbRead`/`KbWrite`
  over namespaces) is the control. A low-capability agent reading what a
  high-capability agent wrote is *intended* when the namespace grant allows it and a
  bug when it doesn't — covered by the deny-by-default capability tests. Canon vs
  log vs scratch bounds write authority.
- **7.2 Provenance integrity.** Provenance is stamped by the runtime, never
  agent-supplied (except the optional `citation` string). An agent cannot forge
  authorship. The `citation` field is agent-controlled and therefore untrusted —
  display-only, never an authz input.
- **7.3 KB as exfiltration channel.** Agent A (no Net) writes a secret to a shared
  segment; agent B (with Net) reads and exfiltrates. Controls: segmentation limits
  who reads; the env-leak fix (F-001) limits what secrets exist to write; operational
  guidance "don't write secrets to memory." Residual risk documented.
- **7.4 Prompt-injection persistence (the scariest).** Poisoned tool output written
  into a `log`/`scratch` segment influences *future* agents that retrieve it —
  injection that outlives the run. Controls: (a) provenance is always shown on
  retrieval, so a downstream agent sees "authored by an agent that had Net," (b)
  `canon` is the only fully-trusted layer and no agent can write it, (c) no automatic
  injection means poisoned content only enters a context when an agent explicitly
  retrieves it. Full mitigation (content signing, trust scoring) is future work.
- **7.5 `memory.redb` at rest.** Mode 0600; **no encryption** (inherits §3.3); risk
  window is *larger* than checkpoint (durable, not deleted). Quarantine-on-corrupt
  mirrors checkpoint. Unbounded-growth DoS bounded by the eviction floor.
- **7.6 Availability.** Store-unreachable degrades to "proceed without memory"
  (best-effort, like the flight recorder) — an availability-over-consistency choice
  for a single-tenant box; documented so it isn't mistaken for a silent failure.

---

## 9. Open questions (real, with trade-offs)

**Q1 — How do semantic (vector) retrieval and embeddings enter? — RESOLVED.**
**Decision: option (a) — external hybrid KB over MCP, with embeddings from a remote
API; the remote-cognition lock stays literal.** Lexical search (Layer 1) covers the
MVP; semantic recall arrives as the optional Layer 2 external KB (§4), whose sidecar
calls a remote embedding API (Voyage AI canonical; Cohere/OpenAI viable) — no local
embedding weights on the `agentd` host. The rejected alternative (b) — running a
*local* embedding model and arguing "retrieval, not cognition" is outside the lock —
was a real reinterpretation of a constitutional decision and is explicitly **not**
taken. Trade-off accepted: every semantic write/query is a network + (metered) cost
event, owned and bounded by the KB sidecar. Residual sub-question (timing): Layer 2's
HTTP/SSE MCP transport is a later increment; the stdio-sidecar path is available within
Phase 5's reach.

**Q2 — Should any retrieval ever be automatic?** Phase 5 says no (explicit tools
only). But a "working-set" of the agent's own most-recent Tier-3 entries, auto-
injected at low token cost, might measurably improve continuity. Trade-off:
auditability and budget predictability (against) vs. ergonomics and fewer wasted
tool-call turns (for). Deferred; revisit with usage data, not speculation. (`claude-mem`
is the reference implementation of this auto-inject path — auto-capture, AI-compress,
inject into future sessions — to mine if we ever flip the default.)

**Q3 — Distillation trigger for Tier 2 → Tier 3.** Who decides what's worth keeping
across runs: the agent (an explicit `mem_remember` before completion), an end-of-run
runtime distillation pass (an extra inference call — costs tokens, needs a budget
line), or nothing auto-promotes (Phase 5 default)? The default is safe but pushes all
retention onto the agent's discipline. The runtime-distillation option is the
ergonomic win but spends cognition the user is metering. When enabled, distillation
**compiles** the run's salient items into markdown-wiki Tier-3 entries (the llm-wiki
content format, §4) — distilled and human-readable — not raw copies. Leaning default-for-now,
runtime-distillation as an opt-in `[memory] distill_on_complete = true` later.

---

### Summary for the roadmap (Prompt 3 input)
Central call: **four tiers on one redb-backed, capability-segmented substrate,
memory-as-tool with zero agent-loop changes, runtime-floor + agent-policy eviction.**
Storage: **redb** (pure-Rust, ~0.6 MB, fits the 4 MB guard; SQLite bundled would
breach it). Phase 5 is **~8 increments** (p4.7 prerequisite cleanup → p5.1 storage
primitive → p5.7 FUSE → p5.8 hardening), detailed in `docs/PHASE-5-PLAN.md`.
