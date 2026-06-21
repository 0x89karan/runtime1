# Prompt 3 — Phase 5 roadmap (fully-fleshed implementation queue)

**Run as:** a fresh Claude Code session inside the `agentos/` repo, on Opus
tier (this is detailed engineering planning, not just prose). Run **after**
Prompts 1 and 2 — this prompt consumes their outputs.

**Suggested branch:** `roadmap/phase-5`. Deliverable is the in-place ROADMAP
amendment plus a per-increment plan document. No code.

---

You are decomposing the Phase 5 memory subsystem into a **fully-buildable
roadmap**. The architectural decisions are already made in `docs/DESIGN-
memory.md` (from Prompt 2). Your job is to turn that design into the same
quality of increment specification you see for Phases 0–4 in
`docs/ROADMAP.md` — small enough for one gstack `/autoplan` → build →
`/review` → `/qa` → `/ship` cycle each, explicit enough that Claude Code
can build from them.

The user's emphasis: **"a fully fleshed out Phase 5."** That means every
increment fully specified — files, types, tests, events, capabilities,
acceptance criteria. No hand-waving. No "the implementation will figure it
out."

## Read first, in this order

1. `notes.md` — orientation.
2. `CLAUDE.md` — invariants.
3. `docs/DESIGN-memory.md` (from Prompt 2) — **the design source of truth.**
   Every increment you write must trace back to a decision here.
4. `docs/AUDIT-phase-4-6.md` (from Prompt 1) — any P0/P1 findings that block
   Phase 5 must be sequenced before p5.1 starts.
5. `docs/ROADMAP.md` — especially Phase 1, Phase 3, and Phase 4 increments.
   These set the bar for specification quality. Match them.
6. `docs/CONVENTIONS.md` — event taxonomy table style, error-handling rules,
   module boundary table.
7. `docs/THREAT_MODEL.md` — Phase 5 increments must include threat-model
   updates where they alter the security surface.

## Deliverables (two files + one amendment)

### File 1: `docs/PHASE-5-PLAN.md`

The per-increment build plan. This is the document Claude Code will
`/autoplan` against at the start of each increment, so it must be **standalone
buildable** — references the design doc by section, but doesn't require the
reader to flip back to it for basic facts.

Sections:

#### A. Pre-Phase-5 readiness checklist
A small checklist (5–10 items) the operator confirms before starting p5.1:

- All P0 audit findings closed (or explicitly accepted as not-blocking).
- Working memory abstraction is extractable (the in-context `Vec<Msg>` in
  `AgentTask` is reachable without leaking across modules; if not, sequence
  the refactor as p4.7).
- Checkpoint format version pinned (so the migration in p5.3 has a stable
  source).
- Capability vocabulary frozen for memory additions (`KbRead{segment}`,
  `KbWrite{segment}` or whatever Prompt 2 decided).
- Storage substrate dependency vetted against the 4 MB binary budget.

Each item: how to verify, what to do if it fails.

#### B. Pre-Phase-5 cleanup increment (if needed)
If the audit recommended a p4.7 cleanup, write it here in full increment
format. If not, say so explicitly with one-line justification.

#### C. Phase 5 increments
Every increment in this exact format (matching ROADMAP.md style, with
*more* detail per increment than Phase 1's spec):

> #### ▢ p5.N — Title
>
> **Depends on:** p5.N-1 (or specific other increments)
>
> **Goal:** One paragraph: what this increment makes true in the world.
>
> **Design reference:** `docs/DESIGN-memory.md` §X.Y
>
> **Scope — files added:**
> - `agentd/src/memory/mod.rs` — types and trait
> - `agentd/src/memory/store.rs` — storage impl
> - …
>
> **Scope — files modified:**
> - `agentd/src/agent/mod.rs` — wire memory into `AgentTask`
> - `agentd/src/events.rs` — new variants (see Events below)
> - `agentd/src/capability.rs` — new vocabulary (see Capabilities below)
> - …
>
> **Capability additions:**
> ```rust
> Capability::KbRead { segment: String }
> Capability::KbWrite { segment: String }
> ```
> Backward compat: `#[serde(default)]` on new fields. (Follow the p4.6 `Net`
> pattern.)
>
> **Event additions (to `events.rs` and CONVENTIONS.md table):**
>
> | kind | when | data shape |
> |---|---|---|
> | `memory_read` | tier-N read returns data | `{tier, segment?, agent, items_count}` |
> | `memory_write` | tier-N write committed | `{tier, segment?, agent, bytes}` |
> | … | … | … |
>
> **Tests added:**
> - Unit: `memory::store::tests::round_trip_basic` — write then read returns equal
> - Unit: `memory::store::tests::write_durable_across_reopen` — survives drop
> - Integration (`tests/memory_integration.rs`): MockGateway agent uses memory
>   tool to persist and retrieve; flight log shows the expected event sequence.
> - Property (if applicable): write order preserved across N concurrent agents
>
> **Test invariants that must hold across increments:**
> - The existing demo (`agent.toml`, `agents.toml`) still produces an
>   identical flight-event sequence when memory is unused.
>
> **Acceptance criteria (concrete, testable):**
> - `cargo build` debug + release succeeds.
> - `cargo clippy -- -D warnings` clean.
> - `cargo test` count increases by N (specify exact number expected).
> - `make clippy-linux` clean for Linux-gated paths.
> - New flight events appear in `flight.jsonl` for the integration test.
> - Binary size delta documented in the PR; if > 200 KB, justification
>   inline.
> - `docs/CONVENTIONS.md` event-taxonomy table updated.
> - `docs/THREAT_MODEL.md` updated if the security surface changed.
>
> **Out of scope for this increment** (explicit list of what gets done later):
> - Eviction (lands in p5.X)
> - FUSE exposure (lands in p5.Y)
> - …
>
> **Known risks / open questions left to discover during build:**
> - …

A reasonable shape for the Phase 5 increment list (justify any deviation):

- **p5.1 — Storage primitive.** The chosen substrate, behind a thin trait, no
  agent integration yet, capability-gated. Single agent can use it via a
  `kv_get`/`kv_set` native tool. Demoable in isolation.
- **p5.2 — Per-agent short-term memory.** Working memory and short-term
  separated cleanly. Token-budget-aware paging from working → short-term.
  Audit finding fixes if any apply here.
- **p5.3 — Per-agent long-term + checkpoint migration.** checkpoint.json
  becomes structured. Format version field. Backward-compat read path.
- **p5.4 — Shared KB MVP (one segmentation axis).** Single axis from Prompt 2
  (probably namespace). Capability gates real. Provenance tracking.
- **p5.5 — Retrieval as tool.** The agent-facing API. Self-paging via tool
  calls if Prompt 2 chose that path; automatic injection otherwise.
- **p5.6 — Eviction and summarization.** The policy from Prompt 2's design.
- **p5.7 — `/agents/<id>/memory/...` FUSE.** Read-only exposure via
  `surfaces/`.
- **p5.8 — Phase 5 hardening pass.** A small cleanup increment paralleling
  p4.5 / p4.6 style — closes any TODOS that accumulated, verifies the
  CONVENTIONS table is complete, audits threat-model updates.

If Prompt 2's design pushes you to a different shape (more increments, fewer,
different order), follow the design — but justify in the plan doc.

#### D. Phase 5 exit criteria
What's true about the system after p5.8 ships. Specific, observable claims.

#### E. Dependencies on later phases
Phase 6 (interface) will surface memory views. What contracts does Phase 5
need to expose for Phase 6 to consume cleanly?

### File 2: `docs/ROADMAP.md` — amended in place

Insert **Phase 5 — Memory substrate** between Phase 4's last delivered
increment and the "Beyond" section. Each increment in the standard ▢/▣/✓
format. The full per-increment detail lives in `PHASE-5-PLAN.md`; ROADMAP
entries should mirror the Phase 1/3/4 entries' depth (a paragraph or two
each, not the giant blocks from PHASE-5-PLAN). Cross-reference the plan doc.

Update any "Beyond" entries that get re-homed into Phase 5.

### File 3 (implied) — CONVENTIONS table preview

In PHASE-5-PLAN.md §C, every increment lists the events it adds with their
data shapes. Consolidate them at the top of §C as a "Phase 5 events
preview" table so a reviewer can see the full taxonomy growth in one place.
These get merged into CONVENTIONS.md as each increment ships.

## Working rules

- **Specificity over brevity.** A Phase 5 increment that says "add storage"
  is a failure. A Phase 5 increment that says "create `agentd/src/memory/
  store.rs` with `MemoryStore { open, get, put, delete, iter }` against
  rusqlite, gated by `KbRead`/`KbWrite`, emitting `memory_read` /
  `memory_write` events" is right.
- **One gstack cycle per increment.** If an increment looks bigger than a
  single `/autoplan` → build → `/review` → `/qa` → `/ship` session, split it.
- **Honor the existing format.** Match Phase 1 / Phase 4's writing style and
  level of detail. Use ▢ for not-started.
- **Trace every increment back to Prompt 2's design.** If something needs to
  be built but isn't in the design doc, that's a Prompt 2 hole to flag, not
  a unilateral decision to make here.
- **Acceptance criteria must be observable** — flight events to look for, jq
  queries that should match, test names that should exist, binary-size
  deltas to verify. No "it should work."
- **Test invariant clause** in every increment: the existing
  single-agent/multi-agent demo flight-event sequences are unchanged when
  memory is unused. This is the Phase 5 equivalent of Phase 1's
  "byte-for-byte flight log parity."
- **Threat-model deltas inline.** Each increment that touches a security
  surface lists the THREAT_MODEL.md updates it owns.

When done, post a one-paragraph summary in chat: the increment count, total
estimated CONVENTIONS-table growth (N new event kinds), the increment most
likely to need scope adjustment during build, and the single biggest open
question that Prompt 2's design doesn't fully answer.

Now begin. Read `notes.md`, then `DESIGN-memory.md`, then the existing
ROADMAP Phase 4 entries to calibrate format.
