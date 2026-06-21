# Prompt 4 — Interface design & agent catalogue (Phase 6)

**Run as:** a fresh Claude Code session inside the `agentos/` repo. Opus or
Sonnet both work — bounded surface area, but the TUI-vs-GUI call is a real
judgment. Run **after** Prompts 1, 2, and 3.

**Suggested branch:** `design/interface`. Deliverable is one new doc plus an
in-place ROADMAP amendment. No code.

---

You're designing how a human actually *uses* AgentOS day to day. Today the
runtime is a CLI: edit `agent.toml` or `agents.toml`, run `cargo run`, tail
`flight.jsonl`. That worked through Phase 4.6. With multiple concurrent
agents, Agent Cards (p1.6), `/agents/<id>/` FUSE (p3.1), checkpoint/restore
(p3.2), kernel sandbox (p3.3 + p4.1–p4.6 hardening), and an incoming memory
subsystem (Phase 5, Prompts 2 + 3), the CLI is the wrong permanent face.

## Crucial context: what surfaces already exist

Don't design in a vacuum. Build on what's there:

- **`/agents/<id>/` FUSE filesystem** (`surfaces/`, p3.1). Each running agent
  appears as a directory with `status`, `context_size`, `budget`, `flight`
  virtual files. A TUI reads these directly — no new agentd-side daemon
  needed for the read path.
- **Agent Cards** (p1.6). `AgentCard { id, name, description, skills }` is
  emitted at startup. The catalogue concept connects directly.
- **Multi-agent + bus + spawn** (p1.2–p1.6). Agents run concurrently,
  message each other, spawn children. The interface must show this graph.
- **Phase 2 QEMU image.** AgentOS already boots in QEMU as a minimal
  Buildroot rootfs (3.1 MB static binary). The TUI/GUI choice affects what
  runs *inside* that boot.
- **Sandbox enforcement reporting** (p4.1–p4.6). `SandboxApplied` events
  carry `EnforcementStatus { landlock, seccomp, namespace_net, namespace_mount,
  landlock_net, spawn_enforcement }`. The interface should surface this so
  operators can verify what's actually enforced.
- **CLI flags grown across phases:** `--no-fuse` (p4.4), `--log-path` (p4.5),
  plus `AGENTOS_NO_FUSE`, `RUST_LOG`, etc. The interface inherits/wraps
  these.

## Read first

1. `notes.md` — orientation.
2. `CLAUDE.md`, `docs/DESIGN.md` (Parts 1, 4, 7), `docs/ROADMAP.md` end-to-
   end (especially **Delivered** entries — that's what's actually shippable).
3. `docs/DESIGN-memory.md` and `docs/PHASE-5-PLAN.md` (Prompts 2 + 3) —
   memory views are part of the interface spec.
4. `docs/AUDIT-phase-4-6.md` (Prompt 1) — findings touching the FUSE
   surface, Agent Cards, or shutdown flow shape the interface.
5. `surfaces/` (the FUSE crate) — what's exposed today; what's read-only;
   the snapshot.rs / agents_fs.rs separation.
6. `agentd/src/main.rs` and `agentd/src/scheduler.rs` — current CLI surface
   and SchedulerSnapshot shape.
7. `distro/` — the QEMU boot environment so you know what fits there.

## The ask, in two halves

### Half A — the interface itself

Pick **TUI or GUI** and defend the choice. Constraints worth weighing:

- AgentOS is single-tenant and light. A TUI fits philosophically, works over
  SSH or a serial console (Phase 2 boots to one), and reads `/agents/<id>/*`
  directly with zero new daemon work. A GUI is richer for visualizing the
  multi-agent topology and flight events, but the toolchain weight (browser
  dependency, WebView2/Tauri, etc.) is real and may not survive the QEMU
  image budget.
- The CLI is the contract. Whatever you pick must be a richer view *over*
  what `agentd` already exposes — never the only way to do something.
- Two distinct contexts: developer on a laptop, and the QEMU-booted AgentOS
  image where there's nothing else. Both want the interface (or at least a
  subset of it).

Make the call. Don't write "we could do either." If TUI, name the library
(`ratatui` is the default — defend or override). If GUI, be specific —
localhost web UI served by `agentd`? Native (Tauri / iced / egui)? Each
choice has knock-on effects on the Phase 2 image and the 4 MB CI guard.

For the chosen interface, spec these **views** in detail (ASCII sketches
fine, multiple sketches better):

1. **Dashboard.** All running agents at a glance — id, name, role/skill,
   status (running / awaiting inference / awaiting tool / awaiting agent /
   deferred / completed / failed), turn count, token spend vs per-agent
   budget vs global budget. Sortable, filterable.
2. **Agent detail.** Live flight log tail (rendered, not raw JSONL), working
   context size, tool catalogue with capability scopes, current effect, last
   inference response preview, **sandbox enforcement status** per attached
   MCP server (from `EnforcementStatus`).
3. **Topology.** The spawn tree and message graph. Who spawned whom; who's
   messaging whom right now. This is the view that didn't exist in Phase 4
   and is *uniquely useful* given the multi-agent runtime.
4. **Memory.** From Prompts 2 + 3's design. Browse per-agent stores and KB
   segments. Initially read-only; mutation can come later.
5. **Spawn.** Start a new agent from a template (Half B). Pick template →
   fill task → optionally adjust capabilities → go.
6. **System.** Global budget remaining, scheduler queue (admitted / deferred
   / denied), MCP servers attached with their `EnforcementStatus`, Landlock
   ABI version detected, provider health, `--log-path`/FUSE flags in effect.
7. **Logs / inspector.** Flight log with structured filters: by event kind,
   by agent, by capability denial, by sandbox skip reason, by error. This is
   how an operator answers "what did agent X do yesterday" without writing
   `jq` by hand.

Be specific about layout and information density. Lean on the user's taste —
they've thought about observability for a non-deterministic system.

### Half B — the agent catalogue

The user's words: *"clearly surface what kind of agents can run, and how to
run them."* Today an agent is a `.toml` file. AgentCards make discovery
possible at runtime; **templates** make discovery possible *before* an agent
runs.

Design an **agent catalogue / template system**:

- **Template schema.** A reusable spec extending `agent.toml`: role, default
  model, suggested capabilities (with deny-by-default discipline), suggested
  tools (native + MCP servers with their `capabilities` arrays and any
  `isolation = "gvisor"`), suggested memory segments (Phase 5), sample
  tasks, and the AgentCard `name` / `description` / `skills` to advertise.
- **On-disk layout.** A `templates/` directory in the repo? `~/.agentos/
  templates/`? Both with precedence rules? Be opinionated.
- **A starter catalogue.** Define 5–7 templates that cover the range of what
  AgentOS is *for*. Each entry: name, one-paragraph purpose, capabilities
  required, tools used, memory segments accessed (when Phase 5 ships),
  example task, and **why it's a good showcase of AgentOS specifically**
  (not generic LLM agent capabilities — what does it demonstrate that only
  this runtime gets right?).
  Suggested categories:
  - **Scout** / researcher (read-only, the historical demo formalized).
  - **Librarian** — organizes and summarizes a filesystem slice via
    `@modelcontextprotocol/server-filesystem`, sandboxed.
  - **Journaler** — long-lived, writes to and reads from long-term memory
    across runs (Phase 5 dependency).
  - **Code-aware agent** — operates on a repo via filesystem + git MCP
    servers, sandboxed with `isolation = "gvisor"` for the
    untrusted-input-handling case.
  - **Watcher** — event-driven, runs when something changes (closest thing
    AgentOS has to a daemon).
  - **Coordinator** — spawns sub-agents and delegates (exercises p1.5 bus +
    p1.6 cards).
  - **Memory custodian** — owns a shared KB segment and curates it (Phase 5).
  Each one demonstrates a specific runtime capability — say which.
- **Running them.** Exact command sequences (CLI and via the interface)
  from "I want a journaler" to "the journaler is running with these
  capabilities, against these MCP servers, writing to this segment."

## Deliverable: `docs/INTERFACE.md` + ROADMAP amendment

### `docs/INTERFACE.md`

Sections in order:

1. **Goals.** What the interface is and isn't.
2. **TUI vs GUI: the decision** and the stack. Implementation cost.
   What it means for the Phase 2 QEMU image size budget.
3. **Views.** Each of the seven above in detail with sketches.
4. **Read-only over `surfaces/`.** Where each view sources its data —
   which `/agents/<id>/*` files, which flight events, which memory paths
   (when Phase 5 ships). Propose new FUSE surfaces if needed, don't
   bypass the existing one.
5. **The agent catalogue.** Template schema, on-disk layout, precedence.
6. **The starter templates.** Each in detail per the spec above.
7. **User workflows.** End-to-end walkthroughs: starting a journaler;
   inspecting what an agent learned last week; pausing and resuming a
   long-running agent (uses checkpoint/restore); reviewing a capability
   denial; checking sandbox enforcement on a running MCP server.
8. **Sandbox enforcement surfacing.** How `EnforcementStatus` shows up in
   the UI. When operators need to know that Landlock V4 isn't available on
   their kernel (degraded net enforcement). When `gvisor` is in effect.
9. **Out of scope.** Things considered and rejected, one line each.
   Specifically: anything multi-user, anything cloud-hosted, anything
   compromising "super light" or the Phase 2 image budget.

### `docs/ROADMAP.md` amendment

Add **Phase 6 — Interface and agent catalogue.** Slot after Phase 5.
Decompose into increments in the standard format (matching Phase 1 / 4
specification quality, not the deeper PHASE-5-PLAN style). A reasonable
sequence (justify deviation):

- **p6.1** — Template schema and on-disk catalogue (CLI-consumable,
  no UI yet).
- **p6.2** — `agentos list-templates` / `agentos spawn <template>`
  subcommands.
- **p6.3** — Read-only TUI/GUI: dashboard + agent detail + system views.
- **p6.4** — Topology view (multi-agent graph).
- **p6.5** — Memory view (depends on Phase 5).
- **p6.6** — Spawn view (template picker + form).
- **p6.7** — Starter catalogue (the committed templates themselves).
- **p6.8** — Sandbox enforcement surface in the UI + edge-case polish.

Updated "Depends on:" labels where p5.x or p3.x feed individual increments.

## Working rules

- The CLI stays the contract. The interface is a *view*; everything it does
  must be doable from the shell.
- Build on `/agents/<id>/` rather than inventing a parallel data plane. If a
  view needs data not currently exposed, propose a `surfaces/` amendment,
  don't bypass it.
- Honest about complexity. The topology view is the hard one; say so.
- Distinguish "agent template" (catalogue concept, user-facing) from
  `agent.toml` (runtime spec, machine-readable).
- Respect the Phase 2 image budget. If the interface adds 50 MB of WebView
  dependency, it doesn't live in the QEMU image — be explicit about which
  parts run where.
- Don't redo Phase 5; depend on it cleanly.

When done, post a one-paragraph summary in chat: the TUI-vs-GUI decision in
one sentence with the deciding reason; the catalogue concept in one
sentence; the first increment to build.

Now begin. Read `notes.md`, then the existing `surfaces/` crate, then
`DESIGN-memory.md` and `PHASE-5-PLAN.md` for the memory view's contracts.
