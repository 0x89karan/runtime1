# HARNESS-OPS-PLAN — the personal operations harness (`agentos-ops`)

**Status:** design + phased plan (not yet built). Authored against `origin/main`
@ `d7cad0df` (post p7.2). Source of requirements: `docs/prompts/70-custom-harness-prompt.md`.

> This is a **harness** in the Core-vs-Harness sense (`ROADMAP.md` §"Core vs.
> Harness"): an *application* built on top of the `agentd`/`agentctl` core, not a
> change to the core. It should live as its own track (plausibly its own repo
> `agentos-ops`, shipped in `agentos:full`). The roadmap captures the **foundation**
> this needs; it does **not** capture this application.

---

## 0. TL;DR — read this first

The prompt describes an always-on **Chief-of-Staff / Product-Ops** system: a central
orchestrator + 13 subagents that watch Gmail/Linear/Attio/GitHub/Calendar/GBrain,
keep a cross-tool context graph + durable memory, surface "what matters now," and
execute **human-approved** workflows.

Three load-bearing conclusions:

1. **The keystone is missing and is not on the roadmap: an approval / autonomy
   primitive.** `agentd` runs agents *autonomously to completion*; there is no
   "pause this action pending operator approval" mechanism. The entire prompt is
   "human-in-the-loop by default, escalate autonomy" — impossible without this. Its
   natural substrate is **p7.3's `/agents/control` surface** (write half exists in
   plan; we add the pending-queue read half + `Approve`/`Reject` commands). **Build
   this first.**
2. **Reuse tool adapters; do not build them.** Mature MCP servers already exist for
   GitHub, Gmail, Google Calendar/Drive, Linear, Atlassian — several are wired into
   the operator's Claude environment today. Attach them via `[[tools.mcp_servers]]`.
   Build only what's missing (Attio likely; thin glue/idempotency around the rest).
3. **Do not build all 13 subagents up front. Ship a thin vertical slice** (one
   workflow, end-to-end, at autonomy L0/L1) and widen. A 13-agent big-bang stalls.

**The context graph** wants **h8.1 (HelixDB, graph+vector)** — so h8.1 is not "later
polish"; it is this harness's memory model.

---

## 1. Proposed harness architecture

```
┌─────────────────────────────  agentos-ops (HARNESS)  ─────────────────────────────┐
│                                                                                    │
│   Executive Orchestrator (root agent, "the shell")                                 │
│     ├─ delegates → specialized subagents (spawn_agent / bus messages)              │
│     ├─ maintains operating state in GBrain + context graph                         │
│     └─ emits pending actions → APPROVAL QUEUE (never executes high-risk directly)  │
│                                                                                    │
│   Subagents (templates): Inbox · People · OpenItems · Customer · Team · Product    │
│                          · Linear · Attio · GitHub · GBrain-Curator · Search       │
│                          · Approval/Safety                                         │
│                                                                                    │
│   Context graph (HelixDB, h8.1)        Durable memory (KB / GBrain, Phase 5)       │
│   Event sources (trigger MCP servers)  Tool adapters (reused MCP servers)          │
└────────────────────────────────────────────────────────────────────────────────┘
                                    │ uses (unchanged)
┌──────────────────────────────  agentd / agentctl (CORE)  ─────────────────────────┐
│  scheduler · capabilities · sandbox · MCP gateway (stdio+HTTP/SSE) · LLM gateway   │
│  · KB store · FUSE surfaces (+ p7.3 /agents/control) · flight recorder · agentctl  │
└────────────────────────────────────────────────────────────────────────────────┘
```

**Principles (from the prompt, mapped to core):** agent-native (templates+scheduler),
persistent (Phase 5 + detachable volume), event-driven (trigger MCP servers, h7.3),
tool-agnostic (MCP-as-ABI, reused adapters), memory-first (KB + context graph),
human-in-the-loop (approval primitive), idempotent (per-adapter check-before-write),
auditable (flight recorder + provenance), secure (capabilities + sandbox), modular
(every subagent a template; every tool an MCP server).

---

## 2. Core orchestrator design

The **Executive Orchestrator** is the `coordinator` template (p6.7) specialized. It is
the DESIGN.md "root/orchestrator agent = login shell." It does **not** itself touch
external systems; it routes.

- **Inputs:** events (from trigger servers), user commands (TUI → `/agents/control`),
  and its own daily/weekly cadence (a `cron` trigger).
- **Loop:** classify event → resolve context (Search/Retrieval + context graph) →
  pick subagent → delegate (spawn or message) → collect structured result → update
  state (GBrain + graph) → decide *act vs. ask* (autonomy policy) → if act-and-risky,
  enqueue a **pending action**; if safe, dispatch.
- **State it owns:** active projects, people, customers, commitments, open loops, and
  the "What matters now?" view — all persisted to GBrain (durable) + the graph
  (relationships), never only in working context.
- **Capabilities:** `Spawn`, `KbRead{*}`/`KbWrite{ops:*}`, `Mcp{...}` for the graph +
  trigger servers. No direct write caps to external tools (those live in the per-tool
  subagents, least-privilege).

---

## 3. Subagent design (the 13)

Each subagent = one `*.template.toml` with a tight capability grant (only the MCP
servers it needs) + a system prompt (§11). Risk-bearing subagents **never execute**;
they propose, and the orchestrator routes proposals through the approval queue.

| # | Subagent | MCP servers it needs | Default autonomy | Writes? |
|---|---|---|---|---|
| 1 | Executive Orchestrator | graph, trigger, GBrain | L3 (routing only) | internal only |
| 2 | Inbox & Communications | Gmail | L1 (drafts) | approval-gated send |
| 3 | People & Relationship | graph, GBrain, Gmail(r), Calendar(r) | L1 | internal |
| 4 | Open Items & Commitments | graph, GBrain, Linear(r), GitHub(r) | L1 | proposals |
| 5 | Customer Context | Gmail(r), Attio, GBrain, graph | L1 | approval-gated Attio |
| 6 | Team & Internal Discussion | GBrain, Linear(r), GitHub(r), graph | L1 | proposals |
| 7 | Product Planning | Linear, GitHub(r), GBrain, graph | L1 | approval-gated Linear |
| 8 | Linear Operations | Linear | L2 | approval-gated |
| 9 | Attio / CRM | Attio | L2 | approval-gated |
| 10 | GitHub Context | GitHub | L0/L1 (read+summaries) | rarely |
| 11 | GBrain / KB Curator | GBrain/KB, graph | L3 (internal memory) | internal durable |
| 12 | Search & Retrieval | all (read-only) | L0 | none |
| 13 | Approval & Safety | control surface, flight | n/a (policy) | resolves queue |

`(r)` = read-only grant. **Build order** (§13) starts with **1, 11, 12, 2, 13** — the
minimum to deliver Workflow 1.

---

## 4. Memory model

Three layers, all already in AgentOS except the graph:

- **Tier 3/4 durable (GBrain) — Phase 5 KB.** Entity profiles, decision records,
  project/customer memory, thread summaries. Namespace per domain
  (`people:`, `companies:`, `customers:`, `projects:`, `decisions:`, `openitems:`),
  `canon`/`log`/`scratch` classes, **provenance stamped by the runtime** (already
  unforgeable, p5.3). Lexical search (p5.5) + semantic (h8.1).
- **Context graph (new) — §5.** Relationships between those entities.
- **Working/short-term — per-agent, Phase 5.** Each subagent's scratch.

**Every memory carries** source URL/id, tool, timestamp, agent, confidence, linked
entities, and (for updates) prev/new state — required by the prompt's
provenance/auditability section and already supported by the KB provenance schema.

---

## 5. Context graph model

**Engine: HelixDB (h8.1)**, attached as an MCP server over the HTTP/SSE transport
(p7.1). Graph + vector in one — exactly the prompt's "implicit or explicit context
graph." Embeddings remote (Voyage AI), preserving cognition-is-remote.

**Node types:** Person, Company, Customer, Email/Thread, Meeting, LinearIssue,
GitHubRepo/PR/Issue, AttioRecord, ProductArea, RoadmapItem, Decision, OpenItem,
KBRecord.

**Edge types:** `mentions`, `owns`, `assigned_to`, `relates_to`, `blocks`,
`derived_from`, `follow_up_for`, `decided_in`, `implements`, `about_customer`.

**Queries it must answer** (prompt §2): *what do I owe X · what happened with customer
Y · what asks recur · what did we decide · which Linear issues relate to this thread ·
what GitHub activity changed product state · what should I focus on today.* Each maps
to a graph traversal + a GBrain fetch for the record body.

Graph **schema + ingestion logic is application work** (HelixDB is just the engine).
Ingestion is idempotent (upsert by stable external id).

---

## 6. Event model

**Mechanism:** h7.3 trigger MCP servers — each exposes a single blocking
`wait_for_trigger()`; an agent calls it as its first action and parks (checkpointed)
until the condition fires. **No scheduler changes** (reuses await-tool + checkpoint).

**Event sources (trigger servers to add):** Gmail push/poll, Calendar, Linear
webhook, GitHub webhook, Attio change, KB change, cron (daily/weekly cadence), and TUI
command (via `/agents/control`).

**Pipeline:** trigger fires → orchestrator wakes → classify (importance/urgency/
person) → resolve context (graph+GBrain) → route to subagent → produce output → either
act (if policy allows) or enqueue pending action. Every step is a flight event.

---

## 7. Tool adapter model — **reuse first**

MCP is the ABI; adapters are replaceable (prompt §9). **Decision per tool:**

| Tool | Approach | Notes |
|---|---|---|
| GitHub | **Reuse** existing GitHub MCP server | + `gh` CLI fallback |
| Gmail / Calendar / Drive | **Reuse** (Google MCP connectors) via **h7.2 OAuth sidecar** | OAuth flow is the only build |
| Linear | **Reuse** existing Linear MCP server | |
| Atlassian (if used) | **Reuse** | |
| Attio | **Build** a small stdio MCP server | likely no mature server; thin REST wrapper |
| Web search / fetch | **Reuse** h7.1 (`http_fetch`, `web_search`) | |
| HelixDB (graph) | **Build** thin MCP wrapper / use HTTP transport | h8.1 |

**Idempotency layer (new, per write-capable adapter):** before any external write,
check "does this already exist?" (issue/note/open-item/link/memory/summary). Implement
as a shared `ops-idempotency` helper keyed by external id + content hash, recorded in
GBrain. Required by prompt §6.

---

## 8. TUI surfaces

`agentctl watch` is read-only (dashboard/topology/memory/spawn). The ops harness needs
a **richer, partly-interactive** mode — recommend `agentctl ops` (or new tabs), reading
existing FUSE surfaces + a new approvals surface:

- **Today's operating brief** (Workflow 1 output) · **Important people focus list** ·
  **Open items** · **Pending approvals (interactive: approve/reject/edit/defer)** ·
  **Customer summaries** · **Product planning queue** · **Search** · **Agent activity
  feed** (flight) · **Tool-call/LLM/cost logs** (inspector, p6.8) · **memory updates** ·
  **errors/conflicts**.

The **only write path** is the approval queue → `/agents/control` (`Approve`/`Reject`).
Everything else is a view over FUSE + flight + KB — no parallel data plane (INTERFACE.md
rule). Degrades gracefully when the harness isn't present.

---

## 9. Approval & autonomy model — **the keystone (new core+harness work)**

The prompt's autonomy ladder: **L0** read-only · **L1** draft-only · **L2** approved
execution · **L3** trusted low-risk auto · **L4** fully autonomous scoped. Default
conservative; escalate to the user when uncertain.

**Mechanism (anchored to p7.3):**

1. **Agent side — a `request_approval` native tool (CORE).** A risk-bearing tool call
   is not executed directly; the subagent (or a wrapping policy) calls
   `request_approval{ action, risk, summary, prev_state?, new_state? }`, which **parks
   the agent** (same await mechanism as a blocking tool) and writes a **PendingAction**
   to a queue.
2. **Read surface — `/agents/approvals` (CORE, surfaces amendment).** A read-only FUSE
   file listing PendingActions (the ops TUI renders it). Mirrors the existing snapshot
   pattern; bounded.
3. **Resolve — extend p7.3 `ControlCommand` (CORE).** Add
   `Approve{action_id, edits?}` / `Reject{action_id, reason}`. Writing to
   `/agents/control` resolves the PendingAction → the parked agent receives an
   `is_error:false` (approved, possibly edited) or `is_error:true` (rejected) tool
   result and resumes. Reuses p7.3's mpsc→scheduler path exactly.
4. **Policy — the Approval/Safety agent (subagent #13) + a policy file.** Classifies
   each proposed action's risk and maps the agent's autonomy level → auto-execute vs.
   enqueue. L3/L4 actions matching pre-approved patterns skip the queue (still
   recorded). The hard gates (send email, mutate Linear/Attio/GitHub, delete/archive,
   change priority/ownership, ambiguous context) always enqueue.

**Why core, not pure harness:** parking-pending-approval and the control resolution are
runtime scheduler concerns (like the read-only FUSE surface already in core). The
*policy* is harness. This is a small, additive core increment — call it **p7.4 —
approval gate** — and it should land **right after p7.3** (its substrate).

---

## 10. Data schemas (key objects)

```jsonc
// PendingAction (core — the approval queue)
{ "id":"act_…", "agent":"linear-ops", "kind":"linear.update_issue",
  "risk":"high", "summary":"…", "args":{…}, "prev_state":{…}, "new_state":{…},
  "linked":["person:…","customer:…"], "confidence":0.0, "created_ts":… }

// Entity (GBrain record body; graph holds the edges)
{ "id":"person:jane@acme", "type":"person", "name":"Jane",
  "attrs":{…}, "provenance":[{src,tool,ts,agent,confidence}], "updated_ts":… }

// OpenItem
{ "id":"oi_…", "text":"send pricing deck", "owner":"me", "due":…,
  "status":"open|stale|done", "source":{tool,id,url}, "home":"linear|attio|kb|queue",
  "linked":["person:…","customer:…"], "evidence_of_completion":null }

// OperatingBrief (Workflow 1 output)
{ "ts":…, "important_emails":[…], "people_needing_attention":[…],
  "customer_updates":[…], "team_updates":[…], "open_items":[…],
  "product_changes":[…], "linear_github":[…], "pending_approvals":[…],
  "recommended_focus":[…] }
```

(Provenance schema is frozen per PHASE-5-PLAN §E — reuse it.)

---

## 11. Subagent system prompts (condensed)

Each ships in its template's `task`/system block. Common preamble for all:
*"You are a subagent in a personal operations OS. Touch the world only through your
granted MCP tools. Never execute a write directly — for any external mutation, call
`request_approval` with a clear summary, risk, and prev/new state. Always cite source
+ provenance. Be idempotent: check before you create or update. Return structured
output matching your schema; the orchestrator consumes it, not a human."*

Then per-agent specializations (one line each — full prompts in Phase 1 build):
- **Orchestrator:** "Decide what matters now and who handles it. Maintain operating
  state in GBrain + the graph. Decide act vs. ask per the autonomy policy."
- **Inbox:** "Triage for priority/urgency/sender-importance; extract asks, deadlines,
  commitments; link to people/customers/issues; draft (never send) responses."
- **Customer:** "Summarize the discussion; extract pain/asks/objections/next-steps;
  propose Attio updates + Linear issues; store durable customer memory; flag recurring
  asks."
- *(… remaining nine follow the same shape, scoped to their tool + outputs in §3.)*

---

## 12. Workflow examples (mapped to subagents + tools)

**W1 Daily Operating Brief** *(the vertical-slice target)*: cron trigger → Orchestrator
→ Search/Retrieval gathers (Inbox summary, people-needing-attention, open items,
Linear/GitHub deltas, pending approvals) → GBrain-Curator persists → brief rendered in
`agentctl ops`. Autonomy L0 (read-only). **No external writes — safe first win.**

**W2 Important-Person Interaction:** Gmail/Calendar trigger → People agent pulls graph
history + open commitments → pre-meeting brief + suggested reply (draft, L1) → enqueue
if send requested.

**W3 Customer Discussion → Product Planning:** new customer thread → Customer agent
summarizes + extracts → proposes Attio update (approval) + Linear issues (approval) →
links roadmap + stores GBrain memory → recurring-ask detection.

**W4 Team Discussion → Execution:** internal thread → Team agent extracts
decisions/actions/blockers → proposes Linear updates + links GitHub → decision records
to GBrain.

**W5 GitHub → Product Context:** GitHub webhook → GitHub agent summarizes in product
language → maps PR→Linear/roadmap → drafts release-note language → GBrain update.

**W6 Open-Loop Closure:** evidence appears → OpenItems agent verifies against source →
proposes closure (approval) → updates system + archives in queue.

Each workflow is: **trigger → orchestrator → subagent(s) → structured output →
(approval if risky) → memory+graph update → TUI**. Idempotent and fully recorded.

---

## 13. Implementation phases

**Foundation (mostly roadmap; finish these first):**
- p7.3 control surface (in progress) · **p7.4 approval gate (NEW — §9, the keystone)**
  · h7.1 generic MCP servers · h7.3 trigger mechanism · h8.1 HelixDB (context-graph
  substrate) · h7.2 OAuth (for Gmail/Calendar).

**Phase O1 — Vertical slice (Daily Operating Brief, read-only): → DONE as `cos.1` (v0.48.0).** Full, substrate-grounded spec: `docs/plans/cos.1-chief-of-staff-slice.md`.
Orchestrator + GBrain-Curator + Inbox (read) over Gmail (`google_oauth` h7.2) + cron trigger
(h7.3), with the approval gate (p7.4), egress + signed receipts (p7.5), gVisor floor (p7.6),
and OTLP (obs.1–3) all live. Output to `/run/output`; `agentctl ops` view deferred. Autonomy
L0 (L1 send opt-in). **DoD:** a real morning brief from live Gmail+KB, fully recorded +
receipt-verifiable, zero external writes — and customer-zero actually uses it. **Gate before
h8.1.**

**Phase O2 — First approved write:** add Linear (reused) + Linear-Ops + Open-Items
agents + the approval queue end-to-end (W6 / a single Linear update). Proves the gate.

**Phase O3 — Context graph:** stand up HelixDB; define schema; backfill from GBrain;
People + relationship queries (W2).

**Phase O4 — Customer/Product loop:** Attio (build) + Customer + Product + Team agents
(W3, W4).

**Phase O5 — GitHub + autonomy escalation:** GitHub agent (W5); enable L3 pre-approved
patterns; broaden trigger sources.

**Phase O6 — Full TUI + packaging:** complete `agentctl ops` surfaces; ship in
`agentos:full` (h8.2).

Each O-phase = one increment per subagent/adapter, one branch each, reusing the gstack
loop. **Never** big-bang the 13.

---

## 14. Risks & mitigations

| Risk | Mitigation |
|---|---|
| **No approval primitive** → can't be safe | Build p7.4 *first*; nothing risk-bearing ships before it |
| **13-agent big-bang stalls** | Vertical slice (O1) first; one subagent per increment |
| **External writes duplicate/clobber** | Idempotency layer (§7); approval gate; prev/new-state on every PendingAction |
| **Tool-adapter churn / building what exists** | Reuse mature MCP servers; build only Attio + glue |
| **Wrong/over-confident actions** | Confidence on every output; conservative default autonomy; human-in-loop gates |
| **Token/cost blowup (always-on)** | Core budget guard already meters; per-subagent budgets; triggers park (no busy-loop) |
| **Secret sprawl (many OAuth tokens)** | secrets-from-env invariant + OAuth sidecar keychain (h7.2); never logged/written |
| **Context-graph drift** | Idempotent upsert by external id; GBrain provenance as source of truth |
| **Shared-tree dev collisions** | Build in worktrees / its own `agentos-ops` repo |

---

## 15. Definition of done

- An operator, from `agentctl ops` over the QEMU console or SSH, gets a **daily brief**
  drawn from live Gmail + KB, sees an **open-items list** and a **pending-approvals
  queue**, and can **approve/reject** an external write — with the action executed
  idempotently, recorded with provenance, and reflected in the context graph.
- At least **W1, W2, W6** run end-to-end at autonomy ≤ L2.
- Every external mutation passes the approval gate (or a logged L3 pre-approved
  pattern); every action is in the flight log with source/tool/agent/confidence.
- `agentd` core unchanged except p7.3 + p7.4 (approval gate); everything else is
  harness (MCP servers + templates), shipped in `agentos:full`; core still ≤ 6 MB.

---

## Appendix — what's foundation (roadmap) vs. net-new (this plan)

**Foundation, exists/planned:** core runtime, MCP gateway (stdio+HTTP/SSE), LLM
gateway, KB+provenance+lexical/semantic (h8.1), templates, FUSE surfaces, p7.3 control,
h7.1 generic MCP, h7.2 OAuth, h7.3 triggers, h8.2 packaging.

**Net-new (this plan, not in the roadmap):**
1. **p7.4 — approval/autonomy gate** (core: `request_approval` tool + `/agents/approvals`
   read surface + `Approve`/`Reject` control commands).
2. Context-graph **schema + ingestion** on HelixDB.
3. **Attio** MCP adapter + the **idempotency** helper.
4. Specific **event-source** trigger servers (Gmail/Linear/GitHub/Attio/cron).
5. The **13 subagent templates** + Executive Orchestrator specialization.
6. **`agentctl ops`** interactive surfaces (esp. the approval queue).
7. The **autonomy policy** engine + file.

**Recommended single highest-leverage next step:** spec **p7.4 (approval gate)** as a
core increment to land right after p7.3 — it is the keystone the entire harness needs
and the one thing that cannot be reused or deferred.
