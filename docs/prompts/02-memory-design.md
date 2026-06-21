# Prompt 2 — Memory subsystem design (architectural)

**Run as:** a fresh Claude Code session inside the `agentos/` repo, on the
strongest reasoning model available (Opus tier — this is the hardest design
problem in the queue). Run **after** Prompt 1 (audit findings inform memory's
integration points).

**Suggested branch:** `design/memory`. One new design doc. No code, no
roadmap yet (the roadmap is Prompt 3's job — keep them separate so each
session can do its work deeply).

---

You are designing the memory subsystem for AgentOS. The codebase is at
**Phase 4.6 / v0.16.0**: agents run under a scheduler, message each other
over the bus, are scoped by capabilities, persist via checkpoint/restore, and
are kernel-sandboxed when they fork MCP servers. **Memory is the missing
piece.** It is the next major subsystem.

**This prompt produces the design doc only.** Prompt 3 takes the design and
decomposes it into a fully-fleshed Phase 5 roadmap with every increment
buildable. Don't try to do both here.

## Crucial context: what already exists that's memory-shaped

Three memory-adjacent substrates exist today. Your design must explain how
each evolves, integrates, or is retired:

1. **In-context working memory.** Per-agent `Vec<Msg>` inside `AgentTask`
   (`agentd/src/agent/mod.rs`). Token-budgeted. No eviction beyond the run
   ending. Read it before designing.
2. **`checkpoint.json`** (`agentd/src/checkpoint.rs`, p3.2 + p4.4). Already
   persists *the full conversation history* of every agent across crashes and
   restarts, atomically, mode 0600. This is **structurally already long-term
   memory** — except today it's used only for crash recovery and deleted on
   success. Re-purposing its storage substrate is on the table.
3. **`flight.jsonl`.** Append-only structured log of every event. Not memory
   per se, but the truth of what an agent *did*. Long-term memory will want
   to read from it.

The single-tenant + remote-cognition locks are constitutional. Single-tenant
means **agent-to-segment** authorization, not user-to-segment. Remote
cognition means every byte of in-context state costs tokens; eviction policy
matters.

## Read first, in this order

1. `notes.md` — high-density orientation.
2. `CLAUDE.md` — locked decisions and invariants.
3. `docs/DESIGN.md` — Part 4 (architecture) and Part 5 ("hard problems," where
   the memory substrate is explicitly called out).
4. `docs/ROADMAP.md` — the "Beyond" section already names memory; read the
   existing increment format so you can write to it (Prompt 3 will).
5. `docs/CONVENTIONS.md` — event taxonomy. Memory will add new kinds.
6. `docs/THREAT_MODEL.md` — §1 (secrets), §2 (flight recorder), §3
   (checkpoint). Memory inherits all of these constraints.
7. `docs/AUDIT-phase-4-6.md` (Prompt 1's output) — any findings touching
   working memory, checkpoint, or `Vec<Msg>` shape this design.
8. The code: `agentd/src/agent/mod.rs`, `checkpoint.rs`, `scheduler.rs`,
   `bus.rs`, `capability.rs`, `events.rs`, `surfaces/`.

## The ask, unpacked

The user's framing, verbatim: *"not just cater to context, short term, and
long term memory but a shared knowledge base with the right kind of
segmentation when required."* Four tiers, designed as one substrate.

### Tier 1 — In-context working memory
The prompt window itself. The hard new question is the **eviction/paging
policy.** MemGPT/Letta's self-paging via function calls is the baseline; what
you'd do differently and why. Connect to the per-agent and global token
budgets (p1.3) — eviction should defer rather than evict when budget allows.

### Tier 2 — Per-agent short-term
Within-session scratchpad. Persists across turns but lifecycle-tied to the
run. Some of what `AgentTask` already carries is this tier; clarify what's
working memory vs short-term so the boundary doesn't leak.

### Tier 3 — Per-agent long-term
Durable, agent-owned. Where checkpoint.json's role evolves from
"crash recovery" to "structured agent memory store." Selective retention —
what's worth keeping vs just being in the log. Provenance (which task, when,
why kept).

### Tier 4 — Shared knowledge base
The interesting tier. Multiple agents read/write. **Segmentation** is the
central design question. Answer with conviction (no "we could do either"):

- **Segmentation axes.** Pick 2–3 that compose. Candidates: topic /
  namespace; capability tier (Phase 1.4 vocabulary); mutability (read-only
  canon / append-only log / mutable scratch); provenance (which agent
  authored). Justify each choice; reject the rest with one-line reasons.
- **Access model.** Reads and writes per segment. Tie directly to the
  existing capability vocabulary (`FsRead`, `FsWrite`, `Mcp`, `Spawn`,
  `Net { hosts, ports }`). New capabilities: `KbRead { segment }`,
  `KbWrite { segment }`? Spell them out. Follow the p4.6 `Net` evolution
  pattern: backward-compat via `#[serde(default)]`.
- **Write semantics.** Append vs mutable. Conflict handling. Provenance
  metadata schema (agent id, turn, task fingerprint, source citation).
- **Read semantics.** Explicit tool call vs automatic retrieval injection?
  Be opinionated. If automatic, when does it run and what's the relevance
  signal? If a tool, what's the API and what events does it emit?
- **Storage substrate.** Be concrete and hold the **"super light" line**.
  Current binary is **3.1 MB static musl**. Options to evaluate:
  - SQLite via rusqlite — zero-deps, well-trodden, ~700 KB
  - SQLite + FTS5 for full-text search — same dep, more capability
  - sqlite-vec for vector search — adds ~few MB
  - lance — much heavier; probably excludes itself
  - JSONL with structured indexing — laughable but provable
  - sled / redb — pure-Rust embedded KV
  Pick one. Justify against the binary-size budget. Name the worst-case
  size addition.
- **Eviction and summarization.** Working memory hits budget → page what
  to short-term? Short-term overflows → distill what to long-term? Who
  decides — the agent (MemGPT-style self-paging via tools), the runtime
  (heuristic), or both? **Be opinionated.**
- **Failure modes.** Memory unreachable → agent stalls or proceeds without?
  Memory corrupted → `.corrupt` quarantine like checkpoints? Memory grows
  unbounded → eviction floor by size or age?

## Deliverable: `docs/DESIGN-memory.md`

Sections in this exact order:

1. **Goals and non-goals.** What this is and what it explicitly isn't.
2. **The status quo, audited.** Clear-eyed read of the three existing
   memory-shaped substrates above. What stays, what gets refactored, what
   gets retired. Cross-reference the audit findings.
3. **The four tiers.** One subsection per tier. Lifetime, ownership, read /
   write paths, integration with `AgentTask` and the scheduler. Diagrams
   (ASCII fine).
4. **The shared KB.** Segmentation model. Access model. Storage substrate
   decision and its dependency cost. Provenance schema. **Worked example:**
   agent A logs a finding; agent B (spawned later, different capability
   set) retrieves it. Trace the calls.
5. **Integration points.**
   - With p1.1's state machine: new `AgentEffect` variants for memory? New
     `Block` types? Or memory-as-tool with no loop changes?
   - With p1.4 capabilities: full new vocabulary, defaults, deny-by-default
     discipline.
   - With `surfaces/`: memory readable via `/agents/<id>/memory/...`?
     Propose the FUSE layout.
   - With `events.rs`: propose every new `EventKind` variant memory needs,
     with full data-payload shape (matching the CONVENTIONS.md table style).
6. **Migration: checkpoint.json → memory store.** Does checkpoint become
   the long-term store, or coexist? If migration, spell out the format
   conversion and the backward-compat window. Address the threat-model
   implications (§3.2, §3.3, the deferred encryption gap).
7. **Sandbox interactions.** When an MCP server is given access to a memory
   segment, what `SandboxRule` set applies? Does the storage file live
   inside or outside the sandbox's `AllowFsRead` / `AllowFsWrite` prefixes?
   The Net-shaped invariant trap from p4.6 is worth a deliberate check here.
8. **Threat-model amendments.** What new section(s) does THREAT_MODEL.md
   need when memory ships? Sketch them — these become real edits in Phase 5.
9. **Open questions.** Real ones with trade-offs articulated. *Not*
   decorative ("how big should the KB be?" is not an open question — pick a
   default; "should retrieval be tool-call or automatic?" is, only if you
   genuinely can't decide).

## Working rules

- **Opinionated and concrete.** A design that says "we could use X or Y"
  isn't a design. The reader should be able to start building from it.
- **Cite prior art briefly.** MemGPT/Letta (self-paging), Mem0 (extraction-
  based), A-MEM (Zettelkasten-style), AIOS's memory module, standard RAG
  patterns. ≤15 words per citation.
- **Hold the "super light" line.** Quantify the binary-size cost of the
  storage substrate choice. If your choice adds ≥1 MB, explicitly defend it.
- **Stay inside the locked decisions.** Cognition-local memory caches
  (on-device embedding models) violate "cognition is remote" unless you can
  argue retrieval-without-inference doesn't count — and that argument is
  yours to make explicit.
- **Inherit threat-model constraints.** Memory persists conversation-like
  data; everything THREAT_MODEL.md says about checkpoint.json and
  flight.jsonl applies in spirit.
- **No roadmap here.** Prompt 3 does the Phase 5 increments. Resist the
  urge to start decomposing.

When done, post a one-paragraph summary in chat: the central design call,
the chosen storage substrate (one sentence on why), the three biggest open
questions, and what shape Phase 5 will have at increment-count granularity
(e.g., "roughly 6 increments, p5.1 storage primitive through p5.6 KB MVP").

Now begin. Read `notes.md` first.
