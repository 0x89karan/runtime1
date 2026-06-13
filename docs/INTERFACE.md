# INTERFACE — Operator Interface & Agent Catalogue (Phase 6)

> **Status:** design source of truth for Phase 6. No code; the per-increment
> roadmap lives in `docs/ROADMAP.md` (Phase 6 section).
> **Reads:** `CLAUDE.md`, `NOTES.md`, `DESIGN.md` (Parts 1/4/7), `ROADMAP.md`,
> `DESIGN-memory.md` + `PHASE-5-PLAN.md` (memory views + §E contracts),
> `AUDIT-phase-4-6.md`, the `surfaces/` crate, `agentd/src/{main,scheduler}.rs`,
> `distro/`.

Today a human uses AgentOS by editing `agent.toml`/`agents.toml`, running
`cargo run`, and tailing `flight.jsonl`. That was fine through Phase 4.6. With
concurrent agents, Agent Cards, `/agents/<id>/` FUSE, checkpoint/restore, kernel
sandbox enforcement, and an incoming memory subsystem, the raw CLI is the wrong
permanent face. Phase 6 gives the operator a real interface — **as a view over
what `agentd` already exposes, never a second way to do things.**

---

## 1. Goals

- **A live, legible view of a non-deterministic, multi-agent system** — what's
  running, what each agent is doing, who spawned/messaged whom, what it spent, what
  the kernel is actually enforcing, and what it remembers.
- **A catalogue that answers "what can run, and how do I run it"** — reusable
  *templates* that make agents discoverable *before* they run, complementing the
  Agent Cards that make them discoverable *after*.
- **The CLI stays the contract.** Every interface action maps to a shell-doable
  operation. The interface is richer rendering + ergonomics, not new authority.
- **Read path is free.** Reads come from the existing `/agents/<id>/*` FUSE files
  and `flight.jsonl` — no new agentd-side daemon for observation.
- **Hold the "super light" line.** The interface must not breach the 4 MB `agentd`
  CI guard or bloat the Phase 2 QEMU image.

### Non-goals
No multi-user/RBAC (single-tenant lock). No cloud-hosted console. No browser/WebView
dependency in the QEMU image. No write authority the CLI doesn't already have. Not a
replacement for `flight.jsonl` — a renderer over it.

---

## 2. TUI vs GUI: the decision

**Decision: a terminal UI built with `ratatui`, shipped as a separate `agentctl`
binary — not baked into `agentd`.** One sentence: a TUI is the only choice that runs
unchanged on the QEMU serial console *and* over SSH *and* on a dev laptop, reads
`/agents/<id>/*` with zero new daemon, and stays inside the super-light budget — a
GUI's browser/WebView toolchain (tens of MB) cannot live in the Phase 2 image.

Why each constraint forces it:
- **The Phase 2 image budget.** AgentOS boots to a 3.1 MB static-musl `agentd` under
  busybox with no display server. A GUI (Tauri/WebView, or a localhost web UI needing
  a browser) adds tens of MB and a runtime that doesn't exist in that image. A TUI
  renders to the serial console the image already boots to.
- **The 4 MB `agentd` guard.** `ratatui` + `crossterm` is pure-Rust and small, but it
  is still weight `agentd` doesn't need. **So the TUI is a separate binary
  (`agentctl`), not a subcommand of `agentd`** — the 4 MB guard on `agentd` is
  untouched, and the minimal QEMU boot stays `agentd`-only. `agentctl` is an
  *optional* addition to the image (a second binary in the rootfs) for operators who
  want the live view at the console; headless deployments omit it.
- **Two contexts, one tool.** Dev-on-a-laptop and QEMU-at-the-console both get the
  same `agentctl` over the same data sources. SSH in, run `agentctl`, done.

**Stack:** `ratatui` (TUI widgets) + `crossterm` (backend, cross-platform incl. raw
serial). Both pure-Rust, no C deps, musl-clean. `agentctl` is its own workspace member
(`agentctl/`), so its dependency tree never touches `agentd`'s size guard. Estimated
`agentctl` musl binary: ~2–3 MB (ratatui/crossterm are light; no async runtime needed
for the read path — it polls files on a tick).

**Rejected:** localhost web UI served by `agentd` (forces an HTTP server + a browser
to view it — browser absent in QEMU; HTTP server grows `agentd`); Tauri/iced/egui
(GPU/WebView/windowing deps, multi-MB, no display in QEMU). One line each in §9.

**What runs where:**
| Context | `agentd` | `agentctl` (TUI) |
|---|---|---|
| QEMU minimal boot | yes (PID-1 target) | optional second rootfs binary |
| Dev laptop | yes | yes (reads the mounted `/agents` + `flight.jsonl`) |
| Headless/cron | yes | omitted |

---

## 3. Views

`agentctl` is a single full-screen TUI with a tab bar; number keys / `Tab` switch
views. Footer shows global state always. ASCII sketches are illustrative, not pixel
specs.

### 3.1 Dashboard — all agents at a glance
Source: `ls /agents/` → per-agent `status`, `context_size`, `budget`; global footer
from the proposed `/agents/system` (§4). Sortable (id/status/spend), filterable
(status, name substring).

```
 agentctl ── [1]Dashboard  2 Agent  3 Topology  4 Memory  5 Spawn  6 System  7 Logs     q quit
 ┌ Agents (4) ───────────────────────────────────── sort:spend↓  filter:─ ──────────────┐
 │ ID                STATUS          TURN   TOKENS / BUDGET           ROLE                │
 │ orchestrator      awaiting-child   3     12.4k / 100k  ▰▰▱▱▱▱▱▱   coordinator         │
 │  └ orch-child-0    running          1      3.1k / 100k  ▰▱▱▱▱▱▱▱   scout               │
 │ librarian         awaiting-tool    7     41.0k / 100k  ▰▰▰▰▱▱▱▱   librarian           │
 │ journaler         deferred         0      0.0k / 50k   ▱▱▱▱▱▱▱▱   journaler           │
 ├───────────────────────────────────────────────────────────────────────────────────────┤
 │ global 56.5k / 250k ▰▰▰▰▰▱▱▱▱▱   in-flight 1/2   deferred 1   denied 0   provider ● ok │
 └───────────────────────────────────────────────────────────────────────────────────────┘
```
Status vocabulary mirrors `AgentStatus` (`running`/`deferred`/`awaiting_child`/`done`/
`failed`) plus the awaiting-inference / awaiting-tool distinction (§4 needs a small
status enrichment — today the snapshot collapses both to `Running`).

### 3.2 Agent detail — one agent, deep
Source: `/agents/<id>/{status,context_size,budget,flight}` + proposed
`/agents/<id>/tools` and `/agents/<id>/sandbox` (§4). `Enter` on a dashboard row.

```
 ┌ journaler ── status: running   turn 7   55.5k/100k tokens ───────────────────────────┐
 │ Current effect: CallTools(mem_recall)                                                  │
 │ Last inference: end_turn · in 1,204 / out 318 · "I found three entries from last…"     │
 │ Working context: 18 msgs · ~9.2k ctx tokens                                            │
 ├ Tools (capability scope) ──────────────────────────────────────────────────────────────┤
 │ read_file        FsRead {/workspace}                                                   │
 │ mem_recall       KbRead {agent/journaler}        (Phase 5)                             │
 │ kb_search        KbRead {project:}               (Phase 5)                             │
 │ fs-mcp           Mcp{server=fs}  ⊕ sandbox: landlock✓ seccomp✓ net-ns✓ spawn:fork_vfork│
 ├ Flight (tail, rendered) ─────────────────────────────────────────────────────────────┤
 │ 14:03:01 inference_response  end_turn  out:318                                         │
 │ 14:03:02 tool_call           mem_recall  {"query":"last week"}                         │
 │ 14:03:02 memory_read         tier:3 items:3                                            │
 └────────────────────────────────────────────────────────────────────────────────────────┘
```
The flight tail is *rendered* (kind-colored, fielded), not raw JSONL — it reads the
same `flight` virtual file, filtered to this agent id.

### 3.3 Topology — the spawn tree + message graph *(the hard one)*
Source: derived from `flight.jsonl` (`agent_spawned.parent_id`,
`agent_child_result_delivered`, `message_sent`/`message_received`) cross-checked with
the live snapshot (`AwaitingChild`). **This is the most complex view and the one with
no Phase-4 precedent** — it's a *time-evolving derived graph*, not a file read.

```
 ┌ Topology ──────────────────────────────────────────────────────────────────────────────┐
 │   orchestrator ●running                                                                  │
 │     ├─spawn→ orch-child-0 ●running   (scout)                                             │
 │     └─spawn→ orch-child-1 ✓done      (scout)                                             │
 │   librarian ●awaiting-tool                                                               │
 │     ╌msg→ orchestrator   (2 msgs, last 14:02:55)                                         │
 │                                                                                          │
 │   legend: ─spawn→ parent/child   ╌msg→ message edge   ●live ✓done ✗failed                │
 └──────────────────────────────────────────────────────────────────────────────────────────┘
```
Honest complexity note: a faithful live graph requires either streaming flight events
or a structured snapshot of edges. Phase 4's snapshot already carries
`AwaitingChild(child_id)` (one edge type); the message graph and completed-child edges
must be reconstructed from the flight log. Phase 6 builds this incrementally (p6.4):
v1 = spawn tree from snapshot + completed edges from the log tail; message edges layered
after. ASCII tree first; box-drawing graph if it earns its complexity.

### 3.4 Memory — per-agent stores + KB segments *(Phase 5 dependency)*
Source: the p5.7 FUSE surfaces — `/agents/<id>/memory/{short_term,long_term/}` and
`/agents/kb/<segment>/`. Read-only (matching Phase 5's read-only operator view).
Consumes `PHASE-5-PLAN.md §E` contracts: stable `Provenance` schema, versioned store.

```
 ┌ Memory ─────────────────────────────────────────────────────────────────────────────────┐
 │ Scope: ( ) agent: journaler   (•) kb segment: project:acme                               │
 │ ┌ entries ───────────────────────────────┐ ┌ entry ──────────────────────────────────┐  │
 │ │ project:acme:findings  (log, 7 entries) │ │ "ACME API rate limit is 100 req/min"    │  │
 │ │   #7  rate limit …          14:01       │ │                                         │  │
 │ │   #6  pricing tiers …       13:58       │ │ ── provenance ──                        │  │
 │ │ project:acme:notes     (scratch, v3)    │ │ by scout · turn 4 · 2026-06-13          │  │
 │ │                                         │ │ cite: https://acme.dev/docs             │  │
 │ └─────────────────────────────────────────┘ └──────────────────────────────────────────┘  │
 │ search: [rate limit___________]  (lexical · Layer 1)   note: semantic ⇒ attach Layer-2 KB │
 └──────────────────────────────────────────────────────────────────────────────────────────┘
```
Respects Phase 5 limits: Layer-1 search is lexical; the view labels it so, and notes
that **semantic search appears only when a Layer-2 external hybrid KB is attached** —
in which case it's just another MCP server visible in the agent's Tools panel, and
`kb_search` routes there transparently. **Mutation is out of scope for Phase 6** (read
the curated store; writes happen through agents/tools, per Phase 5).

### 3.5 Spawn — start an agent from a template *(needs a write path)*
Source: catalogue (§5) for the picker; **a control-write surface for the action**.
This is the second genuinely-new piece: the FUSE plane is read-only and there is no
daemon accepting new top-level agents into a running scheduler today.

```
 ┌ Spawn ──────────────────────────────────────────────────────────────────────────────────┐
 │ Template:  ▸ journaler   (long-lived; reads/writes long-term memory)                     │
 │ Task:      [Summarize this week's standup notes_________________________]                │
 │ Capabilities (from template, deny-by-default):                                            │
 │   [x] FsRead {/workspace}   [x] KbRead {agent/journaler}  [x] KbWrite {agent/journaler}  │
 │   [ ] Net {ports:[443]}     [ ] Spawn                                                     │
 │ MCP servers: fs-mcp (landlock+seccomp+net-ns)                                             │
 │                                       [ Generate agent.toml ]   [ Spawn ▶ ]              │
 └──────────────────────────────────────────────────────────────────────────────────────────┘
```
Two write modes (§4): **(a) generate-and-run** — emit a concrete `agent.toml` and
`exec agentd` (works today, no daemon); **(b) inject-into-running-scheduler** — needs a
writable control endpoint (proposed `surfaces/` amendment). Phase 6 ships (a) first
(p6.2/p6.6); (b) is gated on the control surface.

### 3.6 System — global state & enforcement
Source: proposed `/agents/system` (§4): global budget, scheduler queue
(admitted/deferred/denied), MCP servers + `EnforcementStatus`, detected Landlock ABI
version, provider health, active flags (`--log-path`, FUSE on/off).

```
 ┌ System ─────────────────────────────────────────────────────────────────────────────────┐
 │ Budget   global 56.5k / 250k    in-flight 1 / 2    deferred 1    denied 0                │
 │ Provider anthropic ● ok   model claude-…   flight: /var/run/flight.jsonl   fuse: on      │
 │ Landlock ABI: V4 (net enforcement ACTIVE)                                                 │
 │ MCP servers:                                                                              │
 │   fs-mcp     landlock✓  seccomp✓  net-ns✓  mount-ns✓  landlock_net:n/a  spawn:fork_vfork │
 │   git-mcp    isolation=gvisor (Sentry)                                                    │
 │   web-mcp    landlock✓  seccomp✓  net-ns✗(Net cap)  landlock_net✓ ports[443]             │
 └──────────────────────────────────────────────────────────────────────────────────────────┘
```

### 3.7 Logs / inspector — flight log with structured filters
Source: `flight.jsonl` directly (or the `--log-path` target). Filters by event `kind`,
agent id, `capability_denied`, `sandbox_skipped` reason, `error`, time range. This is
"what did agent X do yesterday" without hand-written `jq`.

```
 ┌ Logs ── filter: kind=capability_denied|error  agent=*  since=-24h ───────────────────────┐
 │ 13:58:02  librarian   capability_denied   tool=write_file required=FsWrite{/etc}         │
 │ 14:01:10  web-mcp(agentd) sandbox_skipped reason=deny-spawn-unsupported-arch             │
 │ 14:03:44  orch-child-0 error  stage=inference  "429 rate limited"                        │
 │ [/] search   [k] cycle kind   [a] pick agent   [enter] expand raw JSON                   │
 └──────────────────────────────────────────────────────────────────────────────────────────┘
```

---

## 4. Read-only over `surfaces/` (+ proposed amendments)

Every view sources from the existing FUSE plane or `flight.jsonl`. Where data isn't
exposed today, the fix is a **`surfaces/` amendment**, never a parallel data plane.

| View | Existing source | Proposed `surfaces/` amendment |
|---|---|---|
| Dashboard | `ls /agents/`, `<id>/{status,budget,context_size}` | enrich status to split awaiting-inference vs awaiting-tool |
| Agent detail | `<id>/{status,context_size,budget,flight}` | **`/agents/<id>/tools`** (tool list + capability scope), **`/agents/<id>/sandbox`** (per-server `EnforcementStatus`) |
| Topology | `flight.jsonl`, snapshot `AwaitingChild` | optional **`/agents/<id>/edges`** (structured spawn/msg edges) to avoid log-scraping |
| Memory | — | **p5.7** `/agents/<id>/memory/`, `/agents/kb/<segment>/` (Phase 5 owns these) |
| Spawn | catalogue files | **`/agents/control`** writable endpoint (or a Unix socket) for inject-into-running-scheduler |
| System | snapshot `global_tokens_spent`, `in_flight` | **`/agents/system/{budget,queue,sandbox,provider}`** — expose deferred-queue count (today omitted from the snapshot) + MCP `EnforcementStatus` + Landlock ABI |
| Logs | `flight.jsonl` / `--log-path` | none (direct read) |

Design rules for the amendments: keep `snapshot.rs` the single in-memory truth the
scheduler writes via `try_write` (stale-not-torn, per AUDIT F-002… er, F-2 snapshot
note) and the FUSE handler reads; keep new files read-only; follow the existing inode
scheme (root=1, dirs from 1010 step 10). The **one writable surface** (`/agents/control`
for spawn) is a deliberate, narrow exception — and per the memory threat model it must
*not* live inside any MCP server's FS sandbox prefix.

Two AUDIT findings shape this: **F-004** (FUSE `read()` overflow — fixed in p4.7) is a
prerequisite for the larger `tools`/`memory` files; **F-013/F-014** (event taxonomy /
docs) being current means the Logs view's `kind` filter list is complete and accurate.

---

## 5. The agent catalogue

### Template schema
A **template** is a user-facing, reusable spec; an **`agent.toml`** is the
machine-readable runtime spec a template *generates*. Templates live in
`*.template.toml`:

```toml
[template]
name        = "journaler"
description = "Long-lived agent that journals to and recalls from long-term memory."
showcases   = "Phase-5 durable per-agent memory surviving across runs + checkpoint/restore."

[model]
provider = "anthropic"
model    = "claude-opus-4-8"

# Suggested capabilities — DENY-BY-DEFAULT. Spawn only grants what's listed.
[capabilities]
fs_read   = ["/workspace"]
kb_read   = ["agent/journaler"]     # Phase 5 (p5.1 vocab)
kb_write  = ["agent/journaler"]     # Phase 5
# net, spawn intentionally absent

[[tools.mcp_servers]]
name         = "fs"
command      = "npx"
args         = ["-y", "@modelcontextprotocol/server-filesystem", "/workspace"]
capabilities = [{ FsRead = { prefix = "/workspace" } }]   # sandbox profile

[memory]                              # Phase 5 (p5.4 segments)
segments = ["agent/journaler"]

[card]                                # advertised AgentCard (p1.6)
name        = "Journaler"
description = "Keeps a durable journal across sessions."
skills      = ["journaling", "recall", "summarization"]

sample_tasks = [
  "Summarize this week's standup notes and journal the key decisions.",
  "What did I decide about the sandbox net policy last week?",
]
```

The template is a *superset* of `agent.toml` with `[template]`, `showcases`,
`[capabilities]` (suggested, deny-by-default), `[memory]` segments, `[card]`, and
`sample_tasks`. `agentctl spawn` strips the template-only keys, applies overrides, and
emits a plain `agent.toml`.

### On-disk layout & precedence
Two locations, **user overrides repo**:
1. `templates/` in the repo — the committed starter catalogue (ships in the image).
2. `~/.agentos/templates/` — the operator's own templates.

Resolution: a bare name resolves user-dir first, then repo (so a user can shadow a
shipped template). `agentctl list-templates` shows both with a `source` column.
Rationale: the repo set is the curated showcase (and must be reproducible in QEMU);
the home dir is where an operator's bespoke agents live without touching the repo.

## 6. The starter templates

Each: purpose · capabilities · tools · memory (when Phase 5 ships) · example · **what
AgentOS-specific thing it showcases** (not generic LLM-agent behavior).

1. **scout** — read-only researcher (the historical demo, formalized). Caps:
   `FsRead {/workspace}`. Tools: native `read_file`/`list_dir`. Memory: none.
   *Example:* "List the project dir, read Cargo.toml, summarize the crate." *Showcases:*
   the bare perceive→infer→act→observe loop + flight recording — the minimal honest
   agent.
2. **librarian** — organizes/summarizes a filesystem slice via
   `@modelcontextprotocol/server-filesystem`, **sandboxed**. Caps: `FsRead`/`FsWrite
   {/workspace/library}`. Tools: fs-MCP (landlock+seccomp+net-ns). *Example:* "Index
   /workspace/library and write an INDEX.md." *Showcases:* per-tool **kernel sandbox**
   (Landlock FS confinement on an MCP subprocess) + capability scoping.
3. **journaler** — long-lived; writes/reads **long-term memory across runs**. Caps:
   `KbRead`/`KbWrite {agent/journaler}`. Memory: `agent/journaler` (Phase 5).
   *Example:* above. *Showcases:* **Phase-5 durable memory** + checkpoint/restore (resume
   the same journaler after a reboot).
4. **code-aware** — operates on a repo via filesystem + git MCP servers,
   **`isolation = "gvisor"`** for the untrusted-input case. Caps: `FsRead`/`FsWrite
   {/workspace/repo}`, `Net {ports:[443]}` for git. Tools: fs-MCP, git-MCP (gVisor).
   *Example:* "Review the diff on branch X and flag bugs." *Showcases:* the **gVisor
   isolation tier** (p4.2) + Landlock V4 **net-port** confinement (p4.6).
5. **watcher** — event-driven; runs when something changes. Caps: `FsRead`. *Showcases:*
   the closest thing AgentOS has to a daemon. **Honest dependency:** there is **no
   event-trigger mechanism today** — Watcher requires a future trigger surface (a
   `surfaces/` watch endpoint or a timer); Phase 6 ships its *template* but marks it
   "requires trigger support (post-Phase-6)". Don't pretend it runs unattended yet.
6. **coordinator** — spawns sub-agents and delegates. Caps: `Spawn` + child caps.
   Tools: native `spawn_agent`, `send_message`, `list_agents`. *Example:* "Research
   three competitors; spawn one scout each; synthesize." *Showcases:* the **p1.5 bus +
   p1.6 Agent Cards + spawn tree** — the multi-agent runtime, and the reason the
   Topology view exists.
7. **memory-custodian** — owns and curates a **shared KB segment** (Phase 5). Caps:
   `KbRead`/`KbWrite {project:}` (a shared, not per-agent, namespace). Memory:
   `project:*` (p5.4). *Example:* "Dedupe and re-summarize the project:findings
   segment." *Showcases:* **Phase-5 shared KB + segmentation + provenance** — multiple
   agents reading one curated segment, capability-gated.

(Watcher and the two memory templates are explicitly **Phase-5/future-gated**; the
catalogue lists them with the dependency stated so the showcase set is honest.)

### Running them
**CLI (the contract):**
```bash
agentctl list-templates                 # name · source · showcases
agentctl spawn journaler \
  --task "Summarize this week's standup" \
  --cap-add 'KbWrite {agent/journaler}'  # overrides allowed; deny-by-default base
# → writes ./run/journaler.toml, then: agentd ./run/journaler.toml
```
`agentctl spawn` = "generate `agent.toml` from template + apply overrides + exec
`agentd`." Pure CLI, no daemon, works in QEMU.

**Via the interface:** Spawn view (§3.5) → pick `journaler` → fill task → toggle
capabilities (pre-checked from template, deny-by-default) → **Generate** (preview the
`agent.toml`) → **Spawn** (mode (a): exec `agentd`; mode (b) when the control surface
exists: inject into the running scheduler). The view never grants a capability the
template didn't suggest without an explicit operator toggle.

---

## 7. User workflows (end to end)

- **Start a journaler.** `agentctl` → Spawn → `journaler` → task → confirm caps
  (`KbRead/KbWrite {agent/journaler}` pre-checked; `Net` left off) → Generate (review
  `agent.toml`) → Spawn. Dashboard shows it `running`; Agent-detail shows `mem_recall`/
  `mem_remember` in the tool list scoped to `agent/journaler`.
- **Inspect what an agent learned last week.** Memory view → scope `agent: journaler`
  → long-term entries, sorted by time, each with provenance (turn, date, citation).
  Lexical search box for keyword recall (Layer 1). No `jq`, no raw store access.
- **Pause and resume a long-running agent.** Send SIGTERM (agentd checkpoints to
  `checkpoint.json`, p3.2); later relaunch — agentd restores; Agent-detail shows the
  restored turn count and an `agent_restored` flight line. (Phase 6 surfaces the
  checkpoint state in System; it does not add a new pause mechanism — SIGTERM is the
  contract.)
- **Review a capability denial.** Logs view → filter `kind=capability_denied` → see
  `tool=write_file required=FsWrite{/etc} agent=librarian` → understand the agent tried
  to escape its `/workspace/library` grant and was blocked at the registry boundary.
- **Check sandbox enforcement on a running MCP server.** System view (or Agent-detail
  Tools panel) → `web-mcp: landlock✓ seccomp✓ net-ns✗(Net cap) landlock_net✓
  ports[443]` → confirm outbound is confined to 443 by Landlock V4. If the row reads
  `landlock_net:✗` on a kernel < 6.7, §8 explains the degradation.

---

## 8. Sandbox enforcement surfacing

`EnforcementStatus { landlock, seccomp, spawn_enforcement, namespace_net,
namespace_mount, landlock_net }` is emitted in `SandboxApplied`/`SandboxSkipped` flight
events (actor `agentd`) at spawn. The interface surfaces it in **two places**: the
System view (all servers) and the Agent-detail Tools panel (servers attached to that
agent). Sourced from the proposed `/agents/system/sandbox` + `/agents/<id>/sandbox`
amendments (which read the same status the events carry), so the UI shows live truth,
not just a historical log line.

Operators must be told, prominently, when enforcement **degraded** (this is the whole
point of surfacing it — silent degradation is the p4.6-class trap):
- **Landlock ABI < V4** → `landlock_net` shows **✗ / "net not enforced"** with a
  one-line warning in System: *"kernel < 6.7 — TCP-port confinement inactive; a `Net
  {ports}` server has unrestricted outbound."* This is exactly AUDIT **F-002**'s
  silent-degradation case made visible.
- **`spawn_enforcement = "none"` on a `DenySpawn` server** (e.g. aarch64) → show
  `sandbox_skipped reason=deny-spawn-unsupported-arch`, not a misleading "applied".
- **`isolation = "gvisor"`** → show `isolation=gvisor (Sentry)` and omit the
  Landlock/seccomp row (gVisor owns enforcement).
- **`namespace_net` absent because a `Net` cap is present** → show `net-ns✗(Net cap)`
  so it's clear network is *intended*, not a failure.

The System view's "Landlock ABI: Vn" line is the at-a-glance answer to "is my kernel
enforcing what I configured."

---

## 9. Out of scope (considered, rejected — one line each)

- **GUI (web/Tauri/iced/egui):** browser/WebView/windowing deps absent in QEMU; blows
  the image budget and the super-light line.
- **TUI baked into `agentd`:** would grow `agentd` past its 4 MB guard; kept as a
  separate `agentctl` binary instead.
- **A persistent control daemon:** rejected for v1 — read path is daemon-free over
  FUSE; the only write need (spawn-into-running) is a single narrow `/agents/control`
  endpoint, not a daemon.
- **Multi-user / RBAC / per-view auth:** violates the single-tenant lock.
- **Cloud-hosted console / remote telemetry:** off-box data flow; not single-box, not
  super-light, new attack surface.
- **Writable memory from the UI:** Phase 6 memory view is read-only; mutation belongs
  to agents/tools per Phase 5.
- **Replacing `flight.jsonl` with a DB:** the log is the contract; the inspector
  renders it, doesn't supplant it.

---

## Dependency on Phase 5 (explicit)

- **Memory view (§3.4)** depends on **p5.7** (`/agents/<id>/memory/`, `/agents/kb/`)
  and consumes `PHASE-5-PLAN.md §E` contracts (provenance schema, versioned store,
  `memory_*`/`kb_*` events). Until p5.7 lands, the Memory tab shows "memory subsystem
  not present."
- **Template `[capabilities]`/`[memory]`** use the **p5.1** `KbRead`/`KbWrite` vocab
  and **p5.4** segment model.
- **journaler / memory-custodian** templates are **Phase-5-gated**; **watcher** is
  trigger-gated (post-Phase-6).
- The interface respects Phase 5 *limits*: Layer-1 search is lexical; semantic search
  surfaces only via an attached Layer-2 MCP KB (shown in the Tools panel like any MCP
  server). Phase 6 does **not** add memory features — it views them.
