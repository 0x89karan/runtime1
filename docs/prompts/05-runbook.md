# Prompt 5 — Operations runbook (Phase 4.6 / v0.16.0 + Phase 5/6 plans)

> **✅ Shipped — produced `RUNBOOK.md` (kept current since; last refreshed v0.62.0). Historical; kept for reference.**

**Run as:** a fresh Claude Code session inside the `agentos/` repo. Sonnet is
fine — synthesis-heavy, judgment-light. Run **last**: this runbook documents
the system as it stands today plus what's designed for Phases 5 and 6, so it
needs the prior four prompts complete.

**Suggested branch:** `docs/runbook`. One new file. No code.

---

You're writing the operations runbook for AgentOS. The audience is the
operator — for now that's the project's author, but write as if it's someone
who didn't build the system. The runbook must be specific enough that
someone with the repo, a Linux box, and an `ANTHROPIC_API_KEY` can follow it
linearly and end up with a working deployment.

## Crucial context: this is not a greenfield runbook

AgentOS already has real operational surface area at **v0.16.0 / Phase 4.6**.
The runbook documents the **shipping system**, not an aspirational one.
What exists today:

- **Phase 2 distro.** `distro/` Buildroot tree producing a bootable musl
  rootfs. `make build/run/test` work end-to-end. Two virtio-9p mounts:
  `secrets/` (API key) and `output/` (flight logs, etc.). DNS via QEMU SLIRP.
- **Sandboxing.** `sandbox/` crate — Landlock (FS + V4 TCP port enforcement,
  p4.6) + seccomp + namespaces. `mcp_require_capabilities = true` enforces
  it. `isolation = "gvisor"` is an opt-in upgrade per MCP server.
- **Checkpoint/restore.** `checkpoint.json` (mode 0600, p4.4) survives
  crashes; `.corrupt` quarantine; deleted on success; periodic auto-
  checkpoint cadence configurable.
- **FUSE.** `/agents/<id>/{status,context_size,budget,flight}` mounted in
  dev mode. `--no-fuse` flag (p4.4) and `AGENTOS_NO_FUSE` env var.
- **Multi-agent + scheduler.** `[[agents]]` config; global + per-agent
  token budgets; max-turns + max-spawn-depth caps; SIGTERM/SIGINT trigger
  `SystemShutdownRequested` with deferred-queue drain.
- **CLI surface.** `--log-path` (p4.5), `--no-fuse` (p4.4); `log_path`
  TOML field (p4.5); `AGENTOS_NO_FUSE`, `ANTHROPIC_API_KEY`,
  `ANTHROPIC_BASE_URL`, `RUST_LOG`.
- **Sandbox observability.** `SandboxApplied` events carry full
  `EnforcementStatus` (Landlock V4 net status, seccomp arch, namespace
  enforcement, gvisor isolation). Operators verify enforcement, not assume.
- **Threat model.** `THREAT_MODEL.md` is comprehensive and authoritative.
  The runbook translates its findings into operator actions.

Phase 5 (memory) and Phase 6 (interface) are designed but not built. The
runbook covers them with explicit *(lands in pX.Y)* markers so the
distinction is unambiguous.

## Read first

1. `notes.md` — orientation, including the development commands section
   (lift relevant ones verbatim where useful).
2. `CLAUDE.md`, `docs/DESIGN.md`, `docs/ROADMAP.md` end-to-end (especially
   **Delivered** sections — that's what's actually operational).
3. `docs/CONVENTIONS.md` — event taxonomy is the operator's parsing
   reference.
4. `docs/THREAT_MODEL.md` — primary reference for §8 (security ops).
5. `docs/AUDIT-phase-4-6.md` (Prompt 1) — operationally relevant findings,
   especially in correctness §1 and security §5.
6. `docs/DESIGN-memory.md`, `docs/PHASE-5-PLAN.md`, `docs/INTERFACE.md`
   (Prompts 2–4) — for the "*lands in pX.Y*" annotations.
7. `agentd/Cargo.toml`, `agentd/agent.toml`, `agents.toml` if present,
   `distro/Makefile` and configs.
8. `TODOS.md` — known issues an operator might hit.
9. `CHANGELOG.md` if present.

## Deliverable: `docs/RUNBOOK.md`

The runbook is the single source of operational truth. Every command runnable
as written. Sections in order:

### 1. Scope
What this document covers (operations) and what it doesn't (design — see
DESIGN.md; threats — see THREAT_MODEL.md; how to extend — see CONVENTIONS.md).
One paragraph, no fluff.

### 2. Deployment modes
Three are real today:

- **Dev mode.** `cargo run` on a normal Linux dev box. Document host
  requirements: Linux 5.13+ for Landlock V1 (FS); Linux 6.7+ for Landlock V4
  (TCP port enforcement, p4.6 — degrades silently below); `runsc` on PATH if
  using gVisor; `fusermount3` for FUSE.
- **QEMU image mode.** `cd distro && make build && make run`. Buildroot
  rootfs boots straight to `agentd`. Document the virtio-9p mounts (how to
  put the key under `secrets/`; where flight logs land in `output/`).
- **Hybrid.** Dev `agentd` talking to remote MCP servers — uncommon, but
  threat-model considerations differ; document briefly.

For each: prerequisites, install/build steps, first-run sequence, how to
know it's working (specific flight events to look for —
`agent_spawned` → `inference_request` → `inference_response` →
`agent_completed`), clean shutdown (SIGTERM behaviour, the
`SystemShutdownRequested` event, drain semantics).

### 3. Configuration reference
Every knob, in one place. Authoritative; cross-reference design docs but
don't defer to them.

- **`agent.toml` reference.** Every section, every field, types, defaults,
  examples:
  - `[agent]` — `name`, `description`, `skills`, `priority`,
    `token_budget`, `max_turns`, `capabilities` (the full
    Capability vocabulary including `Net { hosts, ports }` from p4.6;
    `KbRead` / `KbWrite` when Phase 5 lands).
  - `[model]` — provider, model id, base URL override.
  - `[tools]` — `native`, `mcp_require_capabilities` (default false; set
    true for production), `[[mcp_servers]]` with `name`, `command`, `args`,
    `capabilities`, `isolation` (`"none"` | `"gvisor"`).
  - `[[agents]]` — multi-agent specs.
  - `[scheduler]` — `global_token_budget`, `max_concurrent_inferences`,
    `max_spawn_depth`.
  - Top-level `log_path` (p4.5).
- **Environment variables.** Authoritative list with where each is read and
  whether it's required: `ANTHROPIC_API_KEY` (required), `ANTHROPIC_BASE_URL`
  (optional), `RUST_LOG` (optional), `AGENTOS_NO_FUSE` (optional). Anything
  Phase 5 adds when it ships.
- **CLI flags.** `--log-path PATH` (p4.5), `--no-fuse` (p4.4). Precedence
  between flag, env var, and TOML field.
- **Filesystem layout.** Where `flight.jsonl`, `checkpoint.json`, FUSE mount
  point, and other state live in dev mode and QEMU mode. Default paths and
  how to override.
- **Secrets handling.** How to store keys in dev (env file, `direnv`, OS
  keychain) and in QEMU (`secrets/` 9p mount, `0600` on the host file).
  Never in `agent.toml`. Never in flight logs (cross-reference
  THREAT_MODEL §1, §2).

### 4. Hooking up dependencies
The user's phrasing: *"main things we need to hook up and how to do it like
model keys, mcp servers, etc."* Be exhaustive.

- **Model providers.** Today: Anthropic only. How to set the key in dev and
  in QEMU. `ANTHROPIC_BASE_URL` for proxies/gateways. How to add a new
  provider (point at the `InferenceGateway` extension recipe in CONVENTIONS,
  but show operator-side steps: env vars, config field, restart).
- **MCP servers.** End-to-end for each commonly useful one. Each entry:
  install command, example `[[tools.mcp_servers]]` block including
  `capabilities` (so the sandbox is actually applied, not bypassed via
  `None`), `isolation` recommendation, what tools it exposes, common
  failure modes. Cover at minimum:
  - `@modelcontextprotocol/server-filesystem` (path-scoped read/write)
  - `@modelcontextprotocol/server-git`
  - `@modelcontextprotocol/server-sqlite`
  - `@modelcontextprotocol/server-brave-search` (requires `Net { hosts,
    ports }` — be explicit about what that grants under Landlock V4 vs
    older kernels; cross-reference §6 BP-4 of THREAT_MODEL.md)
  - At least one custom-server example (Python or Rust stub) showing
    binary path and capability declaration.
- **gVisor.** When to use `isolation = "gvisor"` (untrusted servers, BP-1
  to BP-3 from THREAT_MODEL). How to install `runsc`. The known
  experimental status of `runsc do`.
- **The FUSE surface.** Mount/unmount, `--no-fuse` flag, what appears
  under `/agents/<id>/`. Read-only nature, what each virtual file means.
- **The memory store** *(lands in p5.1)*. How to point AgentOS at the
  storage substrate (per Phase 5 design); back it up; inspect it.
- **The interface** *(lands in p6.x)*. How to launch the TUI/GUI; point
  it at a running `agentd`; use it from QEMU console.

### 5. Running agents
End-to-end walkthroughs. Each: exact commands, expected output (key flight
events to grep for), how to interpret success vs failure.

Walkthroughs that work today:

- **The single-agent scout.** `agent.toml` → `cargo run -- agent.toml` →
  tail `flight.jsonl` → final answer on stdout.
- **The multi-agent + bus walkthrough.** Two `[[agents]]` in `agents.toml`,
  one spawning another via `spawn_agent`, watching the cross-agent
  `message_sent` / `message_received` / `agent_card_registered` events.
- **The capability-denial walkthrough.** Configure a read-only agent, give
  it a write task, observe `capability_denied` in the flight log; explain
  how to read the event and adjust the capability.
- **The sandbox-enforcement walkthrough.** Attach an MCP server with
  `capabilities = [...]`, observe `sandbox_applied` with full
  `EnforcementStatus`. Try with `mcp_require_capabilities = true` and a
  bad server (no caps) — observe startup failure.
- **The QEMU-image walkthrough.** `make build`, mount key under
  `secrets/`, `make run`, read flight log from `output/`, shut down
  cleanly with the SIGTERM drain.
- **Checkpoint/restore walkthrough.** Run a long agent, SIGTERM it,
  restart, verify resume from the persisted state.

Walkthroughs marked *(lands in p6.7)*: each starter template from
`INTERFACE.md`.

### 6. Day-2 operations
- **Observability.** Useful `jq` queries against `flight.jsonl` —
  "what did agent X do today?", "where are tokens going?", "which
  capability denials fired?", "did any agents get deferred or
  admission-denied?", "did sandbox enforcement degrade on this kernel?"
  (filter `sandbox_applied` where `landlock_net: false` and reason).
  Provide every query.
- **Budget management.** Setting per-agent vs global; per-agent `priority`
  semantics under contention; interpreting `agent_deferred` (waiting for a
  slot) vs `agent_admission_denied` (terminal — global budget exhausted).
- **Updates.** How to rebuild and roll out `agentd`. In-flight agents:
  checkpoint discipline. Orderly drain via SIGTERM.
- **Backups.** What to back up (`checkpoint.json` is sensitive — same
  posture as your home dir; flight logs grow unbounded — rotation plan;
  memory store backups *lands in p5.x*).
- **Image upgrades (QEMU mode).** Replace `bzImage` + `rootfs` together,
  not separately. Why.

### 7. Troubleshooting
A table: *symptom → likely cause → confirm by → fix*. Cover at minimum:

- `agentd` won't start (bad config, missing key, FUSE mount fail, Landlock
  unavailable on old kernel, `mcp_require_capabilities=true` with a server
  missing `capabilities`).
- Agent stuck `awaiting_inference` (provider down, rate limited, key
  invalid, BASE_URL wrong, TLS issue).
- Agent stuck `awaiting_tool` (MCP server crashed — check inherited stderr;
  capability denied; sandbox blocked a syscall — check `sandbox_applied`
  vs `sandbox_skipped`).
- `sandbox_applied` has `enforced: false` for landlock/seccomp/namespace
  (kernel feature missing — what to install or upgrade to).
- `landlock_net: false` after p4.6 net enforcement was configured (kernel
  < 6.7; document the degradation and what stops being enforced).
- MCP server spawned but exposes no tools (initialize handshake failed —
  inspect stderr; protocol version mismatch; missing `tools/list` support).
- `capability_denied` storm (capability config wrong — how to debug).
- Flight log fills the disk (rotation strategy; `*.jsonl` rotation tools;
  `--log-path` placement on a separate volume).
- Checkpoint `.corrupt` on startup (what happened; what to inspect; how to
  recover; when the agent is rerun fresh).
- QEMU boot hangs (key not in `secrets/`; kernel config gap; virtio-9p
  unmounted; `init` script trace).
- Net-only sandbox config that worked pre-v0.16.0 but locks down FS access
  (the p4.6 critical-bug fix — confirm operators on older binaries upgrade).

For each: the failure event signature in the flight log (kind + key data
fields), operator action, underlying fix.

### 8. Security operations
Threat-model findings translated into operator behaviour. Don't restate
THREAT_MODEL.md; turn it into "what the operator does":

- **Key rotation.** Revoke at console; replace env; restart. No in-process
  rotation (gap TM §1.1).
- **MCP server trust.** Every server gets `capabilities = [...]`
  explicitly. `mcp_require_capabilities = true` enforces it. Untrusted-
  source MCP servers → `isolation = "gvisor"`. BP-1 to BP-6 gaps in the
  namespace-only path are real; recommend gVisor for adversarial workloads.
- **Checkpoint posture.** Mode 0600 is automatic; encryption is the
  documented gap (TM §3.3). For long-lived checkpoints on shared hosts,
  use a `noexec` mount or LUKS until in-process encryption ships.
- **Egress posture.** What goes out: API calls to Anthropic; whatever
  net-capable MCP servers reach (under Landlock V4 port enforcement when
  available, else namespace-only). How to confirm: `tcpdump`-style or
  network-namespace audit.
- **Audit.** "What did agent X do yesterday?" — the flight log answers.
  Provide the canonical `jq` recipe.

### 9. The phase ahead (Phase 5 / Phase 6 preview)
A forward-looking section. Memory subsystem (per `docs/DESIGN-memory.md`
and `docs/PHASE-5-PLAN.md`) will add storage to manage; interface (per
`docs/INTERFACE.md`) will add a TUI/GUI surface. Document the operational
implications:

- Backup story for the memory store (sketch — full ops land with p5.x).
- New flight event kinds operators should care about (from Phase 5 plan).
- New capability checks (`KbRead` / `KbWrite`).
- Interface deployment: where it runs, whether it ships in the QEMU image
  (per Prompt 4's TUI-vs-GUI decision).
- Mark each subsection clearly as *lands in pX.Y*.

## Working rules

- **Every command runnable as written.** No `<placeholder>` without an
  inline example.
- **No "consult the docs."** This *is* the docs. Inline what's needed.
- **Show, then explain.** Lead with the command/config; explain underneath.
- **Honest about state.** Where Phase 5 or 6 hasn't shipped, mark it
  *lands in pX.Y*. Don't pretend it's done.
- **No filler.** If a subsection has nothing operator-relevant, cut it.
- **Cross-reference, don't duplicate.** THREAT_MODEL §N.M is the canonical
  threat reference; refer to it by section rather than restating.

When done, post a one-paragraph summary in chat: which sections are fully
operational vs aspirational, the single biggest operational gap that needs
an increment to close, and the one piece you weren't sure how to document
because the code didn't tell you clearly enough.

Now begin. Read `notes.md`, then `docs/ROADMAP.md` (delivered sections),
then `THREAT_MODEL.md`.
