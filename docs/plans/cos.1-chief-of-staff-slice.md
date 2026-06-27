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
