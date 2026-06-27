# cos.1 — Chief-of-Staff vertical slice: the Daily Operating Brief [HARNESS / flagship]

**Branch:** `cos.1-chief-of-staff-slice`
**Is:** `HARNESS-OPS-PLAN.md` Phase O1, made concrete against the shipped substrate (v0.47.0).
**Why now:** the substrate is complete (core + observability + full h7.x harness) but unproven
against a real workload, and the wedge is still n=1 conviction. This is the flagship workload
that proves both. **Gate before h8.1** — semantic memory deepens an already-strong substrate;
this proves it. Build cos.1, test it working on the founder's real Gmail, *then* h8.1.

## Goal

An always-on Chief of Staff that produces a **Daily Operating Brief** from Gmail — read-only
(autonomy L0), cron-triggered, with the full trust story *live and demonstrable*. It is a
**composition** of shipped pieces, not new infrastructure: the point is to prove they combine
into something a founder would actually run unsupervised on their own inbox.

## What it composes (all shipped — no new core)

| Need | Shipped piece |
|---|---|
| Wake on schedule | `cron_mcp` `wait_for_trigger` (h7.3) |
| Read Gmail without holding the token | `google_oauth` MCP `oauth_call_api` (h7.2) — token lives in the sidecar, agent never sees it (boundary-secret by construction) |
| Multi-agent coordination | scheduler + `spawn_agent`/bus (Phase 1) |
| Durable, provenanced memory | KB / `kb_put`/`kb_search` (Phase 5) |
| Any write gated by a human | `request_approval` → `/agents/approvals` → `Approve` (p7.4) |
| Confined, metered, audited egress | egress mediator + signed receipts (p7.5/p7.5b) |
| Isolation for any universal-tier agent | gVisor/runsc floor (p7.6) |
| Every step observable | OTLP sidecar (obs.1–3) |
| Brief output | `write_file` to the `output0` mount (`/run/output`) |

## Subagents — minimal three (not the full 13)

1. **Executive Orchestrator** (specialize `coordinator`): calls `cron_trigger.wait_for_trigger`
   first (parks until the daily fire), then drives the slice, assembles the `OperatingBrief`,
   asks the curator to persist it, writes it to `/run/output/brief-<date>.md`. Caps: `Spawn`,
   `KbRead`/`KbWrite` on `ops:*`, `Mcp{cron_trigger}`, `FsWrite{/run/output}`.
2. **Inbox agent** (specialize `google-agent`, **read-only**): `oauth_call_api` to list + read
   the last 24h of mail; triage by priority/sender/urgency; extract asks, deadlines,
   commitments; summarize important threads. Returns a structured summary. Caps:
   `Mcp{google_oauth}` only. No send, no FS.
3. **Curator** (specialize `memory-custodian`): persist the brief + extracted entities
   (people, open items) to the KB with runtime provenance, so tomorrow's brief has context.
   Caps: `KbWrite{ops:*}`.

(Search/Retrieval folds into the orchestrator via `kb_search`; Approval/Safety is the core
`request_approval` gate — no separate agent needed for the slice.)

## The workflow (W1, read-only)

```
cron fires → Orchestrator wakes
  → spawn Inbox(read last 24h) → triage + extract → structured summary
  → Orchestrator assembles OperatingBrief{important, response_needed, open_items, focus}
  → Curator persists brief + entities to KB (provenance)
  → Orchestrator writes /run/output/brief-<date>.md
  → re-checkpoint; Orchestrator parks on the next cron fire
```

Autonomy **L0** (no external writes). **L1 option (one extra step, proves the gate):** the
Inbox agent *drafts* a reply to the single most-urgent thread and calls `request_approval`;
the operator approves/edits/rejects via `agentctl`/`/agents/control`; only on approve does a
send occur. Default the slice to L0; make the L1 send an opt-in flag.

## The trust story made demonstrable (this is the actual deliverable)

Each must be *shown*, not asserted:
1. **Agent never holds the Gmail token** — it lives in the `google_oauth` sidecar; the Inbox
   agent only calls `oauth_call_api`. A memory/context dump of the agent contains no token.
2. **Egress confined** — the Inbox agent reaches only Gmail's API host + the model gateway;
   an attempted connection elsewhere yields `egress_denied` in the log.
3. **Fully observed** — every model call, tool call, and Gmail fetch appears in OTLP *and* the
   signed `evidence.jsonl`; the receipt chain verifies offline (`agentctl verify` → exit 0).
4. **Writes gated** — with the L1 flag on, no email sends without `request_approval` firing and
   an operator `Approve`.
5. **Cost bounded** — the run completes under a per-agent token budget; metered in OTLP.

## Scope — files (mostly config + thin glue)

- **`agentd/cos.agents.toml`** (new) — the multi-agent config: the 3 subagents with task
  prompts + caps + budgets, the 3 MCP servers (`cron_trigger`, `google_oauth`, memory), the
  `ops:*` KB segments, a non-zero global budget. This is the bulk of the work.
- **`templates/cos-orchestrator|cos-inbox|cos-curator.template.toml`** (new, optional) —
  package the three as templates so `agentctl spawn` / the catalogue surfaces them.
- **Brief renderer** — minimal: the orchestrator's task instructs it to `write_file` the
  assembled brief as markdown to `/run/output`. No new core code expected; if structured
  rendering needs a helper, keep it a tiny native formatter, not a subsystem.
- **`docs/RUNBOOK.md`** — a "run the Chief of Staff" section: set `OAUTH_CLIENT_ID/SECRET`
  (Google), `ANTHROPIC_API_KEY`, mounts, the cron schedule, where the brief lands.
- **`agentctl ops` brief/approvals view** — **defer to a later increment**; file output +
  the existing inspector are enough to prove cos.1.

## Tests

- `cos::brief_loop_on_mock` — orchestrator → inbox → curator on `MockGateway` produces a
  well-formed `OperatingBrief` and persists it (no network).
- `cos::inbox_is_read_only` — the inbox agent has no send capability; a send attempt is denied.
- `cos::send_requires_approval` (L1) — drafting + send routes through `request_approval`;
  no `Approve` → no send.
- `cos::token_not_in_agent` — the agent's context/env never contains the OAuth token.
- `cos::egress_denied_offdomain` — a non-Gmail/non-model egress attempt is recorded denied.
- **Live smoke** (gated on `OAUTH_*` + `ANTHROPIC_API_KEY`): a real cron-triggered run over a
  test Gmail produces a brief; `agentctl verify` on the evidence chain exits 0.

## Acceptance

- A cron-triggered run produces a real Daily Operating Brief from the founder's actual Gmail,
  end-to-end on AgentOS, written to `/run/output`.
- All five trust-story properties above are demonstrable (token-absence, egress-denial,
  OTLP+verified-receipt-chain, approval-gated send, bounded cost).
- `cargo build` + `clippy --all-targets` + `test` + `make clippy-linux` + `make test-harness`
  clean; `agentd` ≤ 6 MB (composition adds no binary weight).
- The single-agent + memory demos are unchanged.
- **Founder uses it:** customer-zero runs it on their own inbox for a few days and it's useful.
  (This is the real acceptance — the slice exists to be *used*, not demoed.)

## Out of scope (later O-phases)

The other 12 subagents, Linear/Attio/GitHub adapters, the context graph (h8.1), the full
`agentctl ops` TUI, write-heavy workflows. cos.1 is one workflow, one system, read-first.

## Then → h8.1

Once cos.1 is tested and the founder is using it, h8.1 (HelixDB semantic memory / context
graph) becomes the natural next step — it makes the brief *smarter* (cross-tool recall,
"what do I owe this person") on a workload that already works. Do not start h8.1 before cos.1
is working: a smarter memory under a product that doesn't exist proves nothing.

---

## Phase 3 — Engineering Review (autoplan, 2026-06-27)

### Eng Dual Voices — Consensus Table

| Dimension | Claude | Codex | Consensus |
|---|---|---|---|
| Architecture sound? | No | No | **CONFIRMED** — max_turns lifecycle + child ID collision |
| Test coverage sufficient? | No | No | **CONFIRMED** — MockGateway private, test pattern gaps |
| Performance risks addressed? | No | No | **CONFIRMED** — context growth + polling overhead |
| Security threats covered? | No | No | **CONFIRMED** — Inbox caps missing, OAuth token shape wrong |
| Error paths handled? | No | No | **CONFIRMED** — FinalAnswer loop-back missing, SIGTERM idempotency |
| Deployment risk manageable? | Yes | Yes | **CONFIRMED** — pure composition of shipped primitives |

### Critical Gaps (must fix before implementation)

**ENG-1 (CRITICAL — both voices):** `max_turns` lifecycle. cron polling at 25 s = 3 456 turns/day.
Default `max_turns = 20` → orchestrator dies in under 8 minutes before the first cron fire.
Fix: `max_turns = 200_000` in `cos.agents.toml`. Children can keep defaults (short-lived).

**ENG-2 (CRITICAL — both voices):** Token budget is lifetime-monotonic. `tokens_spent` persists in
checkpoint and never resets. After ~100–150 daily runs the orchestrator hits `BudgetExceeded`.
Fix: `token_budget = 5_000_000_000` (≈13 years at 100 k tokens/day) OR document explicit restart cadence.

**ENG-3 (CRITICAL — both voices):** Child ID collision on 2nd cron cycle. `dispatch_spawn` checks
`state.agents` AND `state.outcomes`; terminated children stay in `state.outcomes`. On the second
cycle, orchestrator re-spawns with the same static ID and the collision guard fires → spawn denied.
Fix: use date-stamped child IDs every cycle (e.g. `inbox-2026-06-27`, `curator-2026-06-27`).

**ENG-4 (CRITICAL — both voices):** `MockGateway` is private (`#[cfg(test)]` inside `scheduler.rs:2212`).
`cos::brief_loop_on_mock` cannot compile as described.
Fix: define a local `struct FakeGateway` in the `cos` test module (or the `agentd/src/cos.rs` file).

**ENG-5 (HIGH — Claude voice):** Orchestrator loop-back missing. After writing the brief the model
naturally emits `FinalAnswer` and terminates — always-on property silently breaks after first run.
Fix: task prompt must explicitly say "after writing, call `wait_for_trigger` again to park until next fire."

**ENG-6 (SECURITY — Claude voice):** Inbox agent has no `capabilities` field. Absent = unrestricted
(includes `Spawn`). Inbox agent must set `capabilities = [{ Mcp = { server = "google_oauth", tools = [] } }]`.

**ENG-7 (TEST GAP — Claude voice):** `cos::token_not_in_agent` checks for `ANTHROPIC_KEY`-shaped
tokens but OAuth refresh tokens are `ya29.*` format. `SecretRewriter` (p7.5) won't catch them.
Fix: grep flight log events for `ya29\.[A-Za-z0-9_-]+` pattern (not ANTHROPIC_KEY shape).

**ENG-8 (OPERATIONAL):** `OAUTH_ALLOWED_HOSTS` must be in `passenv` for the google_oauth MCP server
or all `oauth_call_api` calls fail the SSRF dual-layer guard in `oauth_mcp.py`.
Fix: add `"OAUTH_ALLOWED_HOSTS"` to the `passenv` list of the `google_oauth` `[[tools.mcp_servers]]` entry.

### Architecture Diagram

```
cos.agents.toml
│
├── [scheduler]  global_token_budget = 5_000_000_000
│
└── [[agents]] Executive Orchestrator (coordinator template)
    │   max_turns = 200_000  ← NOT default 20
    │   caps: Spawn, KbRead/KbWrite{ops:*}, Mcp{cron_trigger}, FsWrite{/run/output}
    │
    ├── MCP: cron_trigger (cron_mcp.py)
    │       TRIGGER_CRON="0 9 * * 1-5"
    │       Loop: wait_for_trigger(25s) → "waiting"|"fired"|"timeout"
    │
    ├── MCP: google_oauth (oauth_mcp.py)    ← TOKEN NEVER IN AGENT CONTEXT
    │       All OAuth vars in passenv incl. OAUTH_ALLOWED_HOSTS
    │
    ├── MCP: memory (implicit via KB tools)
    │       [[memory.segments]] ops:* class=scratch
    │
    ├── [tools.native]: spawn_agent, kb_put, kb_search, write_file, request_approval
    │
    └── cron fires → Orchestrator wakes
          │
          ├── spawn Inbox "inbox-{date}"  ← date-stamped! not static
          │       caps: Mcp{google_oauth} ONLY  ← no Spawn, no FsWrite
          │       task: list+read 24h mail → triage → structured summary
          │       returns: OperatingBrief fields
          │
          ├── spawn Curator "curator-{date}"
          │       caps: KbWrite{ops:*}
          │       task: persist brief + entities to KB
          │
          ├── Orchestrator assembles OperatingBrief
          ├── write_file /run/output/brief-{date}.md
          ├── (L1 opt-in: request_approval for draft reply)
          └── ← LOOP: wait_for_trigger again ← MUST BE IN TASK PROMPT
```

### Test Coverage Map

```
Test                          Codepath covered                        MockGateway?
──────────────────────────────────────────────────────────────────────────────────
brief_loop_on_mock            Orchestrator → Inbox → Curator cycle    FakeGateway (local)
inbox_is_read_only            Inbox spawn_agent denied (no Spawn cap) MockGateway-free
send_requires_approval (L1)   request_approval → no send without OK   FakeGateway
token_not_in_agent            ya29.* pattern absent in flight events  live flight log scan
egress_denied_offdomain       non-Gmail/model host → egress_denied    live proxy
live_smoke (gated)            real cron → Gmail → brief → verify      real Anthropic
```

### Failure Modes Registry

| Failure | Detection | Recovery |
|---|---|---|
| `max_turns` exhausted (wrong default) | `max_turns_reached` flight event | Restart agentd (checkpoint restores) |
| Child ID collision (2nd cycle) | `agent_admission_denied` in flight | Fix: date-stamp child IDs |
| OAuth token expired | `oauth_call_api` error | Orchestrator surfaces `oauth_start_auth` URL via `request_approval` |
| Gmail API rate limit | HTTP 429 in oauth_mcp.py | Inbox agent backs off; brief is partial |
| KB write failure | `memory_error` flight event | Curator reports to orchestrator; brief still written |
| Context growth (long-running) | `memory_paged` events | Normal — paging is designed |
| Budget exhausted (lifetime) | `budget_exceeded` flight event | Restart agentd; checkpoint restores state |
| Brief write fails (disk full) | `write_file` `is_error` | Orchestrator emits error; loops back to next cron |

## Phase 3.5 — DX Review (autoplan, 2026-06-27)

**DX Score: 4/10** (before applying engineering fixes; rises to ~8/10 once all critical gaps are addressed)

### Top Friction Points

| Priority | Issue | Fix |
|---|---|---|
| F1 CRITICAL | `max_turns=20` → agent dies before first cron fire | `max_turns = 200_000` in orchestrator [[agents]] |
| F2 CRITICAL | `OAUTH_ALLOWED_HOSTS` missing from passenv → silent API failure | Add to passenv for google_oauth server |
| F3 HIGH | OAuth URL only visible in `agentctl watch` Approvals pane | RUNBOOK must require second terminal with agentctl watch |
| F4 HIGH | Child ID collision on second cron cycle → spawn denied | Orchestrator task prompt must use date-stamped IDs |
| F5 HIGH | Model emits FinalAnswer after first brief | Task prompt must end with explicit re-trigger instruction |
| F6 MEDIUM | `agentctl spawn` cannot assemble cos.1 (single-agent limitation) | RUNBOOK must say: `cargo run -- agentd/cos.agents.toml` |
| F7 MEDIUM | Dev vs distro MCP paths (`../docker/` vs `/usr/lib/agentos/docker/`) | Comment both variants in cos.agents.toml |

### Auto-Decided (DX)

- **gated_requires** on cos-orchestrator + cos-inbox templates (both need OAuth credentials)
- **RUNBOOK §11** "Running the Chief of Staff" is required; outline is in test plan artifact
- **cos.agents.toml** must include a first-run comment block with all required env vars
- **Templates cannot substitute** for `cos.agents.toml` — `agentctl spawn` is single-agent only; multi-agent config must be launched with `cargo run -- agentd/cos.agents.toml`
- **Dev path** `../docker/*.py` (cargo run) and **distro path** `/usr/lib/agentos/docker/*.py` both commented in TOML

### Decision Audit Trail

| ID | Dimension | Decision | Rationale | Auto-decided? |
|---|---|---|---|---|
| D-ENG-1 | max_turns value | 200_000 | 3456 turns/day polling, need years of headroom | Yes — math |
| D-ENG-2 | token_budget value | 5_000_000_000 | Lifetime-monotonic; 13 years at 100k/day | Yes — math |
| D-ENG-3 | Child ID scheme | Date-stamped (inbox-YYYY-MM-DD) | state.outcomes collision guard | Yes — architectural constraint |
| D-ENG-4 | MockGateway approach | Local FakeGateway in test module | Private struct, cannot cross module boundary | Yes — Rust visibility |
| D-ENG-5 | Loop-back instruction | Explicit in task prompt | Model will FinalAnswer without it | Yes — model behavior |
| D-ENG-6 | Inbox capabilities | Explicit Mcp{google_oauth} only | Absent = unrestricted including Spawn | Yes — security |
| D-ENG-7 | Token test pattern | ya29.* regex (not ANTHROPIC_KEY shape) | OAuth tokens are ya29.* format; SecretRewriter won't catch them | Yes — format mismatch |
| D-ENG-8 | OAUTH_ALLOWED_HOSTS | In passenv for google_oauth server | SSRF dual-layer in oauth_mcp.py blocks missing env | Yes — operational constraint |
| D-DX-1 | gated_requires | Set on cos-orchestrator + cos-inbox templates | Both require OAUTH_CLIENT_ID/SECRET | Yes — required creds |
| D-DX-2 | RUNBOOK section | §11 "Running the Chief of Staff" required | Spec mandates it; OAuth dance needs step-by-step | Yes — plan requirement |
| D-DX-3 | Launch command | `cargo run -- agentd/cos.agents.toml` (not agentctl spawn) | agentctl spawn is single-agent; cos.1 is 3-agent | Yes — architectural |
| D2 | Orchestrator model | claude-sonnet-4-6 | Good quality/cost balance for daily brief synthesis | **User decision** |
| D3 | Inbox model | claude-sonnet-4-6 | Email triage needs nuance; this is the value-generating step | **User decision** |
| D4 | Curator design | Separate third agent (as spec'd) | Isolates KB write concerns, preserves per-agent provenance | **User decision** |
| D5 | L1 mode config | Commented out but visible in TOML | Best for discovery and future opt-in | **User decision** |
| D6 | TRIGGER_CRON default | `0 8 * * *` (daily 08:00 UTC) | Daily including weekends; both variants commented | **User decision** |

## Status: APPROVED — Ready for implementation

All 6 critical engineering gaps addressed in spec, all 5 taste decisions locked.
Implementation target: `agentd/cos.agents.toml` + 3 templates + RUNBOOK §11.
Next step: `/ship` on the branch after implementation, or `/review` before merging.

