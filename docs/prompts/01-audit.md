# Prompt 1 — State-of-the-union audit (Phase 4.6 / v0.16.0)

**Run as:** a fresh Claude Code session inside the `agentos/` repo, on the
strongest reasoning model available (Opus tier — this is judgment-heavy).

**Suggested branch:** `audit/state-of-union`. The deliverable is one new
markdown report. No code changes.

---

You are auditing the AgentOS codebase as it stands at the end of **Phase 4.6**
(version **v0.16.0**, **253 tests passing**). Phases 0 through 4 have shipped:
single-agent spike, multi-agent scheduler with budgets and capabilities,
inter-agent bus with Agent Cards, static musl Buildroot QEMU image, FUSE
`/agents`, checkpoint/restore, Landlock+seccomp+namespaces+gVisor sandbox, a
comprehensive threat-model pass, and Landlock V4 TCP port enforcement.

This is not a fresh review of a young codebase. It is a **senior review at a
phase boundary**, deciding what must be cleaned up before Phase 5 (memory,
Prompts 2 and 3) starts.

## Read first, in this order

1. `notes.md` — high-density orientation. The single best fast read.
2. `CLAUDE.md` — invariants and locked decisions.
3. `docs/ROADMAP.md` — full history; pay attention to ✓ markers and the
   **Deviation** notes (those are honest records of where reality diverged from
   spec; audit whether each deviation is sound).
4. `docs/CONVENTIONS.md` — current event taxonomy and module rules.
5. `docs/THREAT_MODEL.md` — **already comprehensive. Do not redo it.** Read it
   so you know what's covered. Your security work focuses on what it does *not*
   cover and on gaps it flagged but didn't close (BP-1 through BP-6).
6. `docs/DESIGN.md` — anchors the why.
7. `TODOS.md` — what's tracked; some closed in p2.5, p4.4, p4.5; some open.
8. `CHANGELOG.md` if present — version-by-version delta.
9. The crates: `agentd/`, `sandbox/`, `surfaces/`, and the `distro/` Buildroot
   tree. Read top-down: `Cargo.toml`s, then `src/main.rs`, then by module.

## The lens: p4.6's critical bug

In p4.6 you fixed a critical bug: net-only configs caused **complete FS
lockout** because `handled_access_fs` was set with **zero path rules**. That's
not a typo bug; it's a **latent invariant violation** that survived multiple
review passes because the invariant ("if you declare an access class you must
provide at least one rule for it") was implicit, not enforced or asserted.

This audit's most valuable output is finding the **other places like that**.
Code that does the right thing on the happy path, fails silently or
catastrophically on a configuration the test matrix didn't cover, and would
benefit from a structural assertion the type system or a startup check can
enforce. Treat the p4.6 bug as a template: look for analogous shapes across
the scheduler, sandbox, capabilities, bus, and FUSE.

## Produce: `docs/AUDIT-phase-4-6.md`

Structure below is mandatory. Every concrete claim cites `file:line`.

### 1. Correctness & robustness
Bugs, latent or active. Search hard in these specific places:

- **Scheduler** (`agentd/src/scheduler.rs`): admission control, the
  `BinaryHeap<DeferredInfer>` ordering, the SIGTERM drain path, race conditions
  between deferred-queue admission and `SystemShutdownRequested`, the
  `Vec<Msg>` clone-per-turn cost when multi-agent runs scale up.
- **Bus and mailboxes** (`agentd/src/bus.rs` / `tools/native.rs` `send_message`
  / `list_agents`): the "append to last User message block" trick for
  alternating-role compliance — does it hold on every Anthropic stop reason,
  including tool-use turns? Message ordering under shutdown drain. Unknown-
  recipient handling.
- **MCP client** (`agentd/src/tools/mcp.rs`): the pagination loop (`nextCursor`,
  p2.5), graceful shutdown ordering (`notifications/shutdown` → SIGTERM →
  SIGKILL, p2.5), the pre-exec error pipe (p4.4), `MAX_RESPONSE_BYTES = 4 MiB`,
  the `tokio::sync::Mutex` serialization on stdin/stdout.
- **Checkpoint/restore** (`agentd/src/checkpoint.rs`): the atomic write path
  (`write_mode_600()`, p4.4), the `.corrupt` rename, periodic auto-checkpoint
  cadence, the SIGTERM checkpoint, the deserialize-then-validate ordering, the
  interaction with multi-agent scheduler state.
- **FUSE** (`surfaces/`): partial reads, concurrent reads while the agent is
  writing context, stale snapshots vs the `Arc<RwLock<_>>`, unmount-on-exit
  under both clean and crash exits, the `--no-fuse` / `AGENTOS_NO_FUSE`
  bypass logic (does anything else assume `/agents/` exists?).
- **Sandbox** (`sandbox/`): `compile()`, `apply_compiled()` via `pre_exec`,
  Landlock V4 ABI version detection (p4.6), the **net-only / FS-only invariant
  pairs** (the p4.6 bug template — look for more!), the BPF arch gate for
  `DenySpawn` on aarch64 (p4.5), the `EnforcementStatus.landlock_net` field.
- **Capabilities** (`agentd/src/capability.rs`): `caps_to_rules()` and the
  asymmetric default (`Net` absent → `IsolateNetwork` added; FS capabilities
  absent → ?). Backward-compat with `#[serde(default)]` for `Net.ports` (p4.6).
- **Events** (`agentd/src/events.rs`, extracted in p4.5): does any emit site
  bypass the enum and emit a string literal? Is every variant covered in
  `CONVENTIONS.md`? Conversely, is anything in CONVENTIONS.md unimplemented?
- **Tokio cancellation safety** across the codebase: any `await` inside a
  `tokio::select!` arm that drops mid-future and loses state? Particularly
  around SIGTERM and inference timeouts.

For each finding: `file:line`, the problem, observed/feared consequence,
proposed fix, severity (bug vs taste; high/med/low), and **whether it's a
"p4.6-shaped" latent invariant violation**.

### 2. Conventions drift
22+ increments have landed since CONVENTIONS.md was first written. Audit:

- **Event taxonomy completeness.** Every `EventKind` variant in `events.rs`
  documented in the CONVENTIONS table? Anything in the table that no longer
  emits? Any event whose `data` payload has drifted from what the table
  documents?
- **Module boundary table integrity.** `events.rs` (new in p4.5) is not in the
  table. What else changed? Has policy leaked into `agent/`? Business logic
  into `flight_recorder`? Tool logic into `inference`?
- **Error-handling rules.** Any `unwrap()` / `expect()` / `panic!` on runtime
  data outside tests and truly-invariant internal state?
- **Truncation discipline** (p4.3 redaction). Any new emit site that bypasses
  `PREVIEW_CHARS`?

### 3. Performance & footprint
- Binary size today (release musl `cross` build) vs the CI 4 MB guard (p2.4 /
  p4.1). Current headroom? Trajectory across recent phases?
- Dependency tree (`cargo tree --depth 1`). Anything added across phases that
  violates the "light" ethos? `fuser` is Linux-only (p3.1) — does it compile
  out cleanly on other targets?
- The single `Mutex<File>` flight recorder under many concurrent agents. With
  the multi-agent scheduler real now, is this the bottleneck CONVENTIONS says
  it isn't, or is it actually fine? Estimate or measure.
- The `BinaryHeap<DeferredInfer>` wake/sleep cycle: O(log N) per admit or
  accidentally O(N²)?
- Per-turn `Vec<Msg>` allocation: necessary copy or fixable to a borrow?

### 4. Test coverage (253 tests ≠ good coverage)
Coverage is shape, not count. Audit:

- The `MockGateway` discipline: which non-trivial scheduler or agent paths are
  *not* covered by mock-gateway tests? Name them.
- Integration tests: `sandbox_probe` exists (p4.4). What else has integration
  tests (MCP fixture server ✓)? What doesn't (checkpoint/restore round-trip
  end-to-end? FUSE under concurrent writers? Bus delivery under shutdown drain?
  The new Landlock V4 net path?).
- The "demo as smoke test" rule from CONVENTIONS.md — does `agent.toml` /
  `agents.toml` actually exercise multi-agent + capability + bus + sandbox
  paths now, or has the demo stayed single-agent?
- Property tests where they'd help (scheduler ordering invariants under random
  admission; checkpoint round-trip preserving event order)?

### 5. Security — what THREAT_MODEL.md doesn't cover
Read the threat model first; do **not** duplicate. Then look for:

- Operationally relevant gaps not yet in the threat model:
  - Log injection (a tool returns text containing newlines that desync JSONL
    parsing for `jq`-based audit queries?).
  - Environment-variable leakage to MCP server subprocesses. The parent's
    env includes `ANTHROPIC_API_KEY` — what's filtered?
  - `tracing` output at debug level: does anything secret leak there?
  - FUSE filesystem authorization model: anyone with read access to
    `/agents/<id>/` reads everything the agent did.
  - Landlock ABI degradation transparency: a kernel without V4 silently
    degrades net enforcement. Is this loudly logged for the operator?
- BP-1 through BP-6 mitigation pace. Severity is documented; what's the
  *plan*?
- Supply-chain follow-through. `cargo audit` was flagged missing in
  THREAT_MODEL §5.2. Still missing? `cargo deny`? SBOM?
- `runsc do` reality check: documented as experimental. What breaks when a
  real MCP server actually needs FS writes through it?

### 6. Documentation hygiene
- Every increment in ROADMAP.md has either ▣ or ✓ — consistent with what's
  actually merged?
- `notes.md` is the high-density orientation; does it match the code today
  (it claims v0.16.0, 253 tests, Phase 4 complete)?
- Anything still claiming Phase 0 / Phase 1 state in README, CLAUDE.md,
  DESIGN.md?
- Orphaned references — modules renamed, files moved (agent/ split, events.rs
  extraction)?
- gstack workflow references still accurate?

### 7. Phase 5 readiness
Phase 5 is memory (Prompts 2 and 3). The audit question: **what in the
current code blocks Phase 5, or makes its first increment unreasonably hard?**
Concretely:

- The in-context working memory is per-agent `Vec<Msg>` in `AgentTask`. Is it
  ergonomically extractable into a memory tier, or does it leak across
  `agent/`, `scheduler/`, `bus/`?
- `checkpoint.json` already persists conversation history. **Structurally it
  is proto-long-term-memory.** Is the format stable enough to evolve into the
  memory store, or will it need rewriting?
- The `surfaces/` `/agents/<id>/` FUSE is a natural home for memory reads. Is
  it extensible the way it stands (snapshot.rs / agents_fs.rs separation)?
- Tight couplings to loosen before Phase 5 starts? If so, propose a small
  **p4.7 — pre-Phase-5 cleanup** increment with concrete items.

### 8. Prioritized findings
A single table sorted by priority:

| ID | Severity | Section | Summary | Suggested handling |
|---|---|---|---|---|
| F-001 | P0 / bug | §1 | Net-only-FS-lockout shaped pattern at X:Y | Fix in p4.7 |

Priorities: **P0** must fix before any new feature work; **P1** should fix in
the next pre-feature cleanup; **P2** track in TODOS.md.

### 9. Confidence
One paragraph. What you couldn't verify without running things; what
assumptions you made about subsystems you didn't fully read; what would
change your assessment.

## Working rules

- Be specific. `file:line` for every concrete claim.
- Distinguish *bug* from *taste*. Both can appear; label them.
- Don't relitigate locked decisions (remote cognition, single-tenant).
- Don't duplicate THREAT_MODEL.md. Cross-reference its sections by number.
- The Deviation notes in ROADMAP.md exist for a reason — challenge them only
  with a real argument, not a stylistic preference.
- No code changes. The deliverable is the report.

When done, post a one-paragraph summary in chat: the three highest-priority
findings, whether a `p4.7` cleanup increment is needed before Phase 5, and
the one finding you're least sure about.

Now begin. Read `notes.md` first.
