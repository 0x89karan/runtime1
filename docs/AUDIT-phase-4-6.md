# State-of-the-Union Audit — Phase 4.6 / v0.16.0

**Auditor:** fresh Claude (Opus) session, branch `audit/state-of-union`.
**Scope:** senior review at the Phase 4 → Phase 5 boundary. Deciding what must be
cleaned up before Phase 5 (memory) starts. No code changes — this report is the
deliverable.
**Corpus read:** `NOTES.md`, `CLAUDE.md`, `ROADMAP.md`, `CONVENTIONS.md`,
`THREAT_MODEL.md`, `DESIGN.md`, and all crate source (`agentd/`, `sandbox/`,
`surfaces/`). ~10.3k LOC source. Documented state: v0.16.0, 253 passing tests,
Phases 0–4 complete.

Every concrete claim cites `file:line`. Findings are labelled **bug** vs **taste**,
severity high/med/low, and whether they are **p4.6-shaped** — i.e. an implicit
invariant ("if you declare X you must provide Y") that holds on the happy path but
fails silently/catastrophically on a configuration the test matrix never exercised,
and that a type or a startup assertion could have enforced.

---

## 1. Correctness & robustness

### F-001 — MCP subprocesses inherit the full parent environment, including `ANTHROPIC_API_KEY` *(bug, high, p4.6-shaped)*
`agentd/src/tools/mcp.rs:83-88` builds the child `Command` with no `env_clear()` /
`env_remove()`; spawn sites `mcp.rs:134/171/177`. Confirmed by repo-wide grep: the
only env touch in the whole crate is the read in `inference/anthropic.rs:20`.
Every MCP server subprocess — *including the untrusted third-party servers the
entire `sandbox/` crate exists to contain* — inherits `ANTHROPIC_API_KEY` and every
other secret in agentd's environment. The Landlock/seccomp/namespace sandbox does
**not** mitigate this: the env is copied into the child's address space at fork,
before any FS/net rule is relevant, and a `Net`-capable (or pre-V4, see F-002)
server can read `getenv("ANTHROPIC_API_KEY")` and exfiltrate it.

This violates the spirit of the CLAUDE.md invariant *"secrets come from the
environment, never config or code… never log a secret, never write one to disk"* —
the secret is handed to an arbitrary subprocess. It is **not** covered by
THREAT_MODEL.md §1.1 (which only asserts the key is never *logged* or in *config*)
and is a gap in §6 (sandbox coverage).
- **Consequence:** secret exfiltration by a malicious/compromised MCP server.
- **Fix:** `cmd.env_clear()` then re-add a vetted allowlist (`PATH`, `HOME`, `LANG`,
  plus an optional per-server `env` map from config). At minimum
  `cmd.env_remove("ANTHROPIC_API_KEY")`. Add a test asserting the child env excludes
  the key.
- p4.6-shaped: textbook — invisible on the happy path, catastrophic with a hostile
  server, preventable by an explicit env-construction policy.

### F-002 — `Net { ports }` on a pre-V4 kernel yields *fully unrestricted* network, silently *(bug, high, p4.6-shaped)*
`main.rs:545-547` + `main.rs:556-566` + `sandbox/src/lib.rs:372-395`.
`caps_to_rules` adds `IsolateNetwork` **only when `Net` is absent**. When an operator
declares `Net { ports: [443] }` intending "outbound HTTPS only", two things happen:
(1) `IsolateNetwork` is *not* added (Net present), and (2) `AllowNetConnect{443}` is
emitted. In `sandbox::compile`, `AllowNetConnect` is enforced only when
`query_landlock_abi_version() >= 4` (`lib.rs:372-377`); on kernels < 6.7 it degrades
to nothing (`use_v4_net = false`, and for net-only configs `has_landlock_rules =
false`, so no ruleset is created at all — `lib.rs:382-395`).

Net effect on a pre-V4 kernel: the process has **no network isolation and no port
restriction** — strictly *more* network access than a server that declared no `Net`
capability at all (which gets `IsolateNetwork`). The careful operator is punished.
The degradation is observable only as `landlock_net: false` in the `SandboxApplied`
payload (`main.rs:297`); there is no loud warning. THREAT_MODEL.md BP-4 mentions
Landlock degradation generically but does not cover this **port-restriction →
full-network inversion** introduced by p4.6.
- **Consequence:** silent loss of network confinement on any kernel < 6.7; the
  flight log says `sandbox_applied`.
- **Fix:** when `Net.ports` is non-empty but the kernel ABI is < 4, either (a) fall
  back to `IsolateNetwork` (deny-all is safer than allow-all), or (b) fail fast at
  startup, or (c) emit a prominent `SandboxSkipped`/warn. Decide deny-by-default.
- p4.6-shaped: exactly the original template — declared access class (net ports)
  silently un-enforced; an ABI gate could pick the safe fallback.

### F-003 — Default profile's `unshare(CLONE_NEWUSER)` downgrades the MCP process to the overflow uid, breaking `AllowFsWrite`/`AllowFsRead` DAC *(bug, med, p4.6-shaped — needs runtime confirmation)*
`sandbox/src/lib.rs:644-659`. `IsolateNetwork`/`IsolateMount` call
`unshare(CLONE_NEWUSER | …)` and never write a `uid_map`/`gid_map`. A user namespace
with no mapping leaves the process running as the overflow uid (`nobody`, 65534) for
DAC purposes. Because `caps_to_rules` adds `IsolateNetwork` to **every server that
lacks the `Net` capability** (`main.rs:545-547`) — i.e. the common/default profile —
the typical sandboxed server runs as `nobody`. Landlock may *permit* a path, but
classic DAC then denies it: a `0644` file owned by the real user is still readable
(other-readable) but a `0600` file is not, and `AllowFsWrite` to any
non-world-writable path fails with `EACCES`.

So the headline Phase-3/4 capability — "grant an MCP server write access to
`/workspace`" — is silently defeated for user-owned files whenever the same server
also (by default) gets network isolation. The `pre_exec` error pipe (p4.4) would not
flag this; the failure surfaces only as runtime `EACCES` from the server's own file
ops.
- **Consequence:** `AllowFsWrite`/`AllowFsRead` grants are partially inert under the
  default net-isolation profile; confusing, untested.
- **Fix:** after `unshare(CLONE_NEWUSER)`, write `/proc/self/uid_map` and
  `gid_map` (with `setgroups=deny`) mapping the real uid 1:1, so DAC identity is
  preserved inside the namespace. Add a Linux integration test that writes a `0600`
  file through a net-isolated sandbox.
- p4.6-shaped: yes — the implicit invariant "Landlock-allowed ⇒ accessible" is broken
  by an unrelated namespace rule. **Confidence caveat:** reasoned from the syscall
  sequence; not reproduced on the QEMU target (see §9).

### F-004 — FUSE `read()` can panic on `offset + size` overflow *(bug, low-med, p4.6-shaped)*
`surfaces/src/agents_fs.rs:305`: `let end = (offset + size as usize).min(content.len());`.
`offset` derives from a kernel-supplied `i64`; `offset + size` can overflow `usize`
and panic in debug. FUSE input is not a trusted ABI, and the handler runs on its own
thread — a panic there poisons the mount. Violates "the loop never panics on bad
input" at the surface layer.
- **Fix:** `offset.saturating_add(size as usize).min(content.len())`; clamp `start`
  the same way. One line.
- p4.6-shaped: latent panic on an input the kernel "never" sends.

### F-005 — Mailbox injection runs while the just-arrived response is un-pushed, mis-ordering inter-agent messages *(bug, med, p4.6-shaped)*
`scheduler.rs:313-314` calls `provide_inference(resp)` (stores the response in
`stored_response`, does **not** push it to `messages`) and then `drain_mailbox` →
`agent/mod.rs:240` appends the mailbox text to the **last `Role::User` message**.
But `step_with_response` pushes the assistant turn only afterwards (`agent/mod.rs:327`).
So on the inference-completion path the injected message lands on the user turn that
*precedes* an assistant reply that was generated without it — the message is stitched
into history as if already answered. (On the tool-result path the ordering is fine,
because `provide_tool_results` has already pushed the user turn.)
- **Consequence:** inter-agent messages that arrive on the same tick an inference
  completes are positioned as stale/already-addressed; effectively unreliable
  delivery. Not an API error, not a crash — a silent semantic mis-order untested by
  the suite (unit tests inject only into a fresh task, `agent/mod.rs` tests).
- **Fix:** drain the mailbox only at a clean turn boundary (after the assistant
  message is pushed), or `debug_assert!(self.stored_response.is_none())` in
  `inject_messages` and gate the call. Add a test that injects between
  `provide_inference` and `step`.
- p4.6-shaped: yes. *(I rate this MED, not HIGH — the message is not lost and there is
  no API rejection; the corruption is positional.)*

### F-006 — `inject_messages` appends `Text` onto a ToolResult-only user turn *(taste/bug, med, p4.6-shaped)*
`agent/mod.rs:240-241`. After a tool cycle the last user message is all
`Block::ToolResult`; injection pushes a `Block::Text` into it, yielding
`[ToolResult, …, Text]`. Anthropic tolerates mixed content, so not a hard error, but
the "append to last User message" comment assumes that message is a plain text task,
and a stricter provider (or future validation) would reject a tool-result turn that
also carries free text.
- **Fix:** when the target user message contains any `ToolResult`, push a *new* user
  message instead (no alternating-role violation — the prior turn is assistant), or
  document the mixed-content assumption.

### F-007 — `StopReason::MaxTokens` is reported as `BudgetExceeded` *(bug, med)*
`agent/mod.rs:333-344`. A per-response generation cap (`model.max_tokens`) is
conflated with the cumulative per-agent `token_budget`. The agent emits a
`budget_exceeded` event carrying `"budget": self.cfg.token_budget` and dies, even
when nowhere near its budget. Discarding the partial text is deliberate (D1) and
fine; the *labelling* is wrong and the kill may be undesirable.
- **Fix:** distinct event kind (e.g. `max_tokens_truncated`); don't attach the
  unrelated `token_budget`.

### F-008 — `StopReason::Other(_)` and empty `EndTurn` complete successfully with `""` *(bug, low, p4.6-shaped)*
`agent/mod.rs:346-371`. `EndTurn | Other(_)` both extract the first `Text` block and
`Completed(unwrap_or_default())`. An unknown/future stop reason (`refusal`,
`pause_turn`, `stop_sequence`) — or an `end_turn` whose content was only a filtered
`thinking` block — yields `Completed("")`: a silent empty answer reported as success.
- **Fix:** treat empty extracted text as `Failed`/retry; handle `Other(_)` distinctly
  with at least a warning event; consider modelling `pause_turn`/`refusal`.

### F-009 — Global token budget is a soft, post-hoc ceiling *(taste, low)*
`scheduler.rs:608-609` admits an inference whenever `tokens_spent <
global_token_budget`, then adds the full response cost afterwards
(`scheduler.rs:323`). A single inference can overshoot the ceiling by an unbounded
amount; the guard only blocks *new* admissions once exceeded. The ROADMAP p1.3
acceptance ("total spend never exceeds the ceiling") is true only for the specific
test shape. Worth documenting as "soft ceiling, overshoot ≤ one in-flight inference
per agent."

### F-010 — `read_line_bounded` validates UTF-8 per `fill_buf` chunk → spurious failure on multibyte boundaries *(bug, med, p4.6-shaped)*
`agentd/src/tools/mcp.rs:423-424`. When no newline is in the current `fill_buf`
chunk, `end = available.len()` and `std::str::from_utf8(&available[..end])` validates
a slice that ends at the BufReader's 8 KB buffer boundary, which can split a
multi-byte codepoint. A perfectly valid MCP response containing any non-ASCII text
longer than ~8 KB intermittently fails as "not valid UTF-8" depending on where the
fill boundary lands. (The 4 MiB cap and pagination are otherwise correctly enforced
— see §3/§4.)
- **Fix:** accumulate raw bytes in a `Vec<u8>` and run `String::from_utf8` once at the
  newline, or carry the incomplete tail across iterations. Tests use ASCII only, so
  this is invisible to the suite.

### F-011 — Checkpoint `format_version` validated *after* full deserialization; fixed tmp filename *(bug, med, p4.6-shaped)*
`checkpoint.rs:131-141` deserializes the whole `SchedulerCheckpoint` *then* checks
`format_version > FORMAT_VERSION`. A future schema that keeps field names but changes
their meaning, written without bumping the version, would restore silently with v1
semantics; a genuinely incompatible v2 fails the serde step and is misclassified as
*corrupt* (renamed `.corrupt`, state discarded) rather than "too new, refuse." Also
`checkpoint.rs:102` uses a constant `checkpoint.json.tmp` and `write_mode_600`
(`checkpoint.rs:36-44`) deletes a pre-existing tmp before retrying — two concurrent
writers sharing a CWD (explicitly plausible for a "PID 1 of an OS") race and can
produce a torn final file.
- **Fix:** read a `{ format_version }` probe struct *before* the full deserialize and
  reject mismatches (distinguishing "too new" from "corrupt"); make the tmp name
  unique per write (`checkpoint.json.<pid>.<nanos>.tmp`) and never delete a tmp this
  call didn't create.

### F-012 — No `fsync` before/after the checkpoint rename *(bug, low-med)*
`checkpoint.rs:47,117`. `write_all` is not followed by `sync_all()`, and the parent
dir is not fsynced after `rename`. The "previous good checkpoint stays intact"
guarantee holds for *ordering* but not for a real power loss, where the rename or the
tmp data blocks may not be durable — yielding a zero-length/stale `checkpoint.json`
for an OS that takes SIGTERM-checkpointing seriously.

### Verified correct (not findings)
- **The p4.6 FS-lockout fix is properly in place.** `sandbox/src/lib.rs:470`
  (`fs_access = 0` when `path_entries.is_empty()`) and the `has_landlock_rules` gate
  (`lib.rs:382`) prevent the net-only/empty-rules lockout. Regression tests at
  `lib.rs:1046-1073`.
- **No production `unwrap`/`expect`/`panic!`/indexing on runtime data** in `agent/`,
  `tools/`, `checkpoint.rs`, `flight_recorder.rs`, `events.rs`, `config.rs`,
  `anthropic.rs`. The two `unreachable!()` in `agent/mod.rs:416,454` are genuinely
  unreachable (post-filter). The one runtime panic surface is F-004 (FUSE).
- **MCP pagination** is bounded by `MCP_MAX_TOOL_PAGES = 100` (cannot hang); **graceful
  shutdown ordering** (notifications/shutdown → SIGTERM → SIGKILL) is correct;
  **pre_exec error pipe** correctly distinguishes sandbox failure from missing binary;
  **transport Mutex** holds across the full request/response so concurrent `tools/call`
  cannot mis-correlate (`mcp.rs:267,301-305`).
- **Capability path checks** (`capability.rs`) are correct: component-wise
  `starts_with` (no `/workspace` vs `/workspace-evil` sibling escape), `..`
  normalization denies traversal, empty-prefix grants fail-safe to deny
  (`capability.rs:108,120`), relative paths fail-safe.
- **Flight recorder** is genuinely best-effort/panic-free with no lock held across an
  await (`flight_recorder.rs:59-74`; `record()` is sync, std `Mutex`, `lock().ok()`).
- **Tokio cancellation:** the SIGTERM/SIGINT arms (`scheduler.rs:386-405`) just set a
  flag and `break`; in-flight inference futures in `pending` are dropped, which is
  acceptable (their results are simply discarded; tokens already spent are not lost
  because they're only added on result delivery). The post-loop checkpoint
  (`scheduler.rs:410-411`) captures state. No state-losing `await` mid-`select!` arm.

---

## 2. Conventions drift

### F-013 — Six emitted `EventKind` variants are missing from the CONVENTIONS.md taxonomy *(bug, med — doc invariant)*
`events.rs` defines, and code emits, six kinds absent from the CONVENTIONS.md table
(`CONVENTIONS.md:57-79`):

| variant | string | emitted at |
|---|---|---|
| `ToolsRegistered` | `tools_registered` | `main.rs:327` |
| `AgentChildResultDelivered` | `agent_child_result_delivered` | `scheduler.rs:464` |
| `AgentCheckpointed` | `agent_checkpointed` | `scheduler.rs:1058` |
| `AgentRestored` | `agent_restored` | `main.rs:413` |
| `SystemShutdownRequested` | `system_shutdown_requested` | `scheduler.rs:390/400` |
| `FuseSkipped` | `fuse_skipped` | `main.rs:365` |

CLAUDE.md ("new behavior gets new event kinds — see the taxonomy in CONVENTIONS.md")
and the `events.rs:6` "keep in sync" comment are both violated. Conversely, every
string already in the table has a live variant — no orphans.
- **Fix:** add the six rows in the same PR; add a test asserting each variant's
  serialized string so the table can't silently drift again.

### Module boundaries
`events.rs` (extracted in p4.5) is not listed in the CONVENTIONS.md module-boundary
table (`CONVENTIONS.md:21-31`). Add it. Otherwise boundaries hold: no policy leaked
into `agent/`, no business logic in `flight_recorder`, no tool logic in `inference`.
`bus.rs` is a thin type module (it does *not* own a router — delivery logic lives in
`scheduler.rs::dispatch_send_message`); the table says `bus` owns "addressing,
messaging, spawn", which overstates the current split. Minor.

### Truncation discipline
`scheduler.rs:934` truncates the `send_message` preview with an inline
`content.chars().take(200)` instead of the shared `truncate(_, PREVIEW_CHARS)` helper.
Same length, but it bypasses the constant — a drift risk if `PREVIEW_CHARS` changes.
Low/taste.

### Error handling
No `unwrap`/`expect`/`panic!` on runtime data outside tests (verified §1). Compliant.

---

## 3. Performance & footprint

- **Binary size:** no local musl artifact to measure (`cross` not run here); NOTES.md
  records 3.1 MB against the 4 MB CI guard, which is intact
  (`.github/workflows/ci.yml:48-53`, `MAX_BYTES=4194304`, `stat -c %s`). ~0.9 MB
  headroom. Phase 4 added only `nix` (signals) and raw-syscall `sandbox` (libc only)
  — trajectory is flat.
- **Dependency tree** (`cargo tree --depth 1`): anyhow, async-trait, chrono, futures,
  libc, nix, reqwest (rustls, default-features off → no OpenSSL), serde, serde_json,
  tokio, toml, tracing, tracing-subscriber + local sandbox/surfaces. Light and
  justified. **`fuser` is correctly Linux-gated** (`surfaces/Cargo.toml`, under
  `[target.'cfg(target_os="linux")'.dependencies]`) — compiles out cleanly elsewhere.
- **Flight recorder `Mutex<File>`:** with the multi-agent scheduler real, the
  synchronous `writeln!` under a std `Mutex` runs on the executor; a stalled disk
  (9p-backed QEMU rootfs, full disk) blocks *all* agents, not one. Not a correctness
  bug; for a "super light" PID-1 runtime it's a latent stall point. Acceptable now;
  note for later (async/batched writer).
- **`BinaryHeap<DeferredInfer>`:** O(log N) per admit/defer (`scheduler.rs:577,652`);
  `update_snapshot` does a linear scan of `deferred` + `awaiting` per tick
  (`scheduler.rs:978-989`) — O(agents) per event, fine at expected scale.
- **Per-turn `Vec<Msg>` clone:** `agent/mod.rs:302` clones the full message history
  into every `InferenceRequest`. Necessary today because the request is moved into a
  spawned future and the agent retains its history; with long conversations this is
  the obvious cost as Phase 5 grows context. A borrow/`Arc<[Msg]>` refactor is the fix
  when it matters — flagged for Phase 5 (see §7).

---

## 4. Test coverage (shape, not the 253 count)

Strong unit coverage (mock-gateway scheduler paths, capability matrix, sandbox
compile matrix, config back-compat, checkpoint serde round-trips). Gaps that matter:

- **No end-to-end checkpoint→restore through the real binary.** Unit/scheduler tests
  cover restore (`scheduler.rs:2113-2269`) but nothing exercises `main.rs:404-429`:
  write checkpoint → restart process → assert awaiting/mailboxes/spawn_depths survive.
  The corrupt→`.corrupt` branch (`main.rs:425-426`) and remove-after-load (`main.rs:418`)
  are untested e2e. *(med, p4.6-shaped)*
- **No Landlock-V4 port-deny enforcement test.** p4.6's headline feature. The
  `sandbox-probe` integration tests (p4.4) cover FS grant/deny and `DenySpawn` fork
  blocking, but no test asserts a connect to a *denied* TCP port is actually refused
  under V4. Enforcement rests on manual QA. *(med, p4.6-shaped)*
- **No FUSE-under-concurrent-writers test.** All `agents_fs` tests call helpers
  directly; the `try_write`/`read` interleaving and F-004 path are unverified.
- **No bus-delivery-under-shutdown-drain test.** `sigterm_drains_scheduler` exists but
  doesn't assert in-flight mailbox messages are delivered/checkpointed at SIGTERM.
- **The demo undersells Phase 4 — "demo as smoke test" rule violated.**
  `agent.toml` is single-agent, `native=["all"]`, MCP commented out
  (`agent.toml:20-24`). `agents.toml` has two agents but capabilities are commented
  out (`agents.toml:36-39`), no MCP servers, `global_token_budget=0` and
  `max_concurrent_inferences=0` (both unlimited → admission control unexercised), no
  spawning, no bus traffic, no sandbox rules. **Capabilities, MCP-with-sandbox, the
  bus, spawning, and the kernel sandbox are demonstrated in no runnable config** —
  only in tests and commented examples. For a phase-4 boundary this is the most
  defensible "demo doesn't match the claims" finding. *(med, p4.6-shaped)*
- **Property tests** would help: scheduler admission ordering under random
  priority/seq; checkpoint round-trip preserving event/turn order. None present.

---

## 5. Security — beyond THREAT_MODEL.md (cross-referenced, not duplicated)

THREAT_MODEL.md §1–§6 is comprehensive and current to p4.3. New/operationally
relevant gaps:

- **Env-var leakage to MCP subprocesses — F-001.** Not covered by §1.1 (which scopes
  to logs/config only). This is the single most important security gap in the audit.
- **Net-port → full-network inversion on pre-V4 kernels — F-002.** Extends BP-4; the
  specific inversion (declaring ports gives *less* isolation than declaring nothing)
  is new with p4.6 and undocumented.
- **User-namespace DAC downgrade — F-003.** Interaction between the default
  `IsolateNetwork` profile and Landlock FS grants; not in the model.
- **Landlock ABI degradation transparency.** §6.2 BP-4 says degradation is reported in
  `SandboxApplied`, and it is (`landlock`, `landlock_net` fields) — but only as a
  field, never as a `tracing::warn!`. An operator tailing stderr gets no signal that
  enforcement silently dropped. Recommend a warn when a requested mechanism degrades.
- **Log injection:** tool/agent text in `flight.jsonl` is JSON-encoded via
  `serde_json` (`flight_recorder.rs`), so embedded newlines are escaped — JSONL
  parsing is **not** desyncable. Verified safe; worth one line in the threat model to
  close the question.
- **`tracing` at debug level:** no secret is passed to `tracing` (key only in the
  header at `anthropic.rs:146`); previews use `PREVIEW_CHARS`. No leak found.
- **BP-1…BP-6 pace:** all still open, all documented; no code change since p4.6. BP-5
  (unsandboxed-without-capabilities) is mitigated by `mcp_require_capabilities`
  (`main.rs:145-162`). Recommend stating an explicit plan/owner for BP-1 (clone3) and
  BP-3 (PID-ns re-fork) before Phase 5, or formally accepting them.
- **Supply chain (§5.2):** `cargo audit`/`cargo deny` still absent from CI
  (`.github/workflows/ci.yml`). Cheap to add; recommend doing so in p4.7.

---

## 6. Documentation hygiene

- **F-014 — `README.md:17` is stale** *(bug, low, p4.6-shaped doc-sync miss)*: claims
  *"Phases 0–3 in progress (p3.3 done)"* and tags `sandbox/` as "Phase 3" (`README.md:42-43`)
  while the repo is v0.16.0, Phases 0–4 complete. CLAUDE.md, NOTES.md, CHANGELOG,
  ROADMAP are all current; only the top-level README missed the phase-4 updates.
- **ROADMAP markers** consistent with merged state (▣/✓ all present through p4.6).
- **NOTES.md** matches the code (v0.16.0, Phase 4 complete, layout accurate).
- **No orphaned module references** — layout sections in CLAUDE.md/NOTES.md match the
  tree (`agent/` split, `events.rs` extraction reflected).
- **CONVENTIONS.md taxonomy** out of date — F-013 (six missing event kinds) and the
  `events.rs` module row.
- **Demo model id** `claude-sonnet-4-6` (`agent.toml`, `agents.toml`) is a current,
  active model id — not stale.

---

## 7. Phase 5 readiness

Phase 5 is memory (Prompts 2/3). What in the current code helps or blocks it:

- **In-context working memory** is `Vec<Msg>` private to `AgentTask` (`agent/mod.rs`).
  It does **not** leak across `scheduler`/`bus` — the scheduler only calls
  `provide_*`/`step`/`to_checkpoint`. Good: a memory tier can wrap or replace the
  `Vec<Msg>` behind `AgentTask` without touching the scheduler. The one coupling to
  loosen is the **per-turn full clone** (`agent/mod.rs:302`, F-009/§3) — a memory tier
  that grows context makes this expensive; move to `Arc<[Msg]>` or a borrowed request.
- **`checkpoint.json` is proto-long-term-memory** — it already persists full
  conversation history with a `format_version`. Structurally reusable, **but** the
  version handling (F-011) must be fixed *before* it becomes a durable store, or the
  first schema evolution silently mis-restores. This is the strongest argument for a
  pre-Phase-5 cleanup.
- **`surfaces/` `/agents/<id>/`** is a natural home for memory reads and is extensible
  as-is: `snapshot.rs` (data) / `agents_fs.rs` (handler) are cleanly separated, inode
  allocation is centralized (`alloc_dir`). Adding a `memory` virtual file per agent is
  mechanical — fix F-004 (read overflow) first since memory files will be larger and
  more often partially read.

### Recommendation: yes — schedule **p4.7 — pre-Phase-5 cleanup** before Prompt 2
Concrete items, in priority order:
1. **F-001** env allowlist for MCP subprocesses (P0 security).
2. **F-002 / F-003** net-port deny-default + uid_map in the user namespace (P1
   security/correctness).
3. **F-011** checkpoint version-probe-before-deserialize + unique tmp name (P1 — blocks
   evolving checkpoint into the memory store).
4. **F-013 / F-014 / events.rs row** doc-sync (P1, cheap).
5. **F-004** FUSE read overflow; **F-010** MCP UTF-8 chunking (P1/P2, small).
6. `Arc<[Msg]>` request refactor (enables Phase 5 context growth).
7. Demo config that actually exercises caps + MCP + bus + spawn + sandbox (closes the
   smoke-test gap); `cargo audit` in CI.

---

## 8. Prioritized findings

| ID | Severity | Section | Summary | Suggested handling |
|---|---|---|---|---|
| F-001 | **P0 / bug** | §1,§5 | MCP subprocess inherits full env incl. `ANTHROPIC_API_KEY` (`mcp.rs:83-88`) | Fix in p4.7 — env_clear + allowlist |
| F-002 | **P1 / bug** | §1,§5 | `Net{ports}` → fully unrestricted net on kernels < 6.7 (`main.rs:545`, `lib.rs:372-395`) | p4.7 — deny-default fallback + warn |
| F-003 | P1 / bug | §1,§5 | `unshare(CLONE_NEWUSER)` w/o uid_map breaks FS grants via DAC (`lib.rs:644-659`) | p4.7 — write uid/gid_map; verify on QEMU |
| F-011 | P1 / bug | §1,§7 | Checkpoint version checked after deserialize; fixed tmp name race (`checkpoint.rs:102,131-141`) | p4.7 — version probe + unique tmp |
| F-013 | P1 / bug | §2 | 6 event kinds missing from CONVENTIONS.md taxonomy | p4.7 — add rows + assertion test |
| F-004 | P1 / bug | §1 | FUSE `read()` offset+size overflow panic (`agents_fs.rs:305`) | p4.7 — saturating_add |
| F-005 | P1 / bug | §1 | Mailbox injected before assistant turn pushed → mis-ordered (`scheduler.rs:313`) | p4.7 — drain at clean boundary |
| F-014 | P1 / bug | §6 | README claims "Phases 0–3 in progress" (`README.md:17`) | p4.7 — one-paragraph fix |
| F-010 | P2 / bug | §1 | MCP UTF-8 validated per fill-chunk → fails on multibyte boundary (`mcp.rs:423`) | p4.7/TODOS — buffer bytes, validate once |
| F-007 | P2 / bug | §1 | `MaxTokens` mislabelled `BudgetExceeded` (`agent/mod.rs:333`) | TODOS — distinct event |
| F-008 | P2 / bug | §1 | `Other(_)`/empty `EndTurn` → silent `Completed("")` (`agent/mod.rs:346`) | TODOS — fail on empty |
| F-006 | P2 / taste | §1 | Text appended to ToolResult-only user turn (`agent/mod.rs:240`) | TODOS — new user msg |
| F-012 | P2 / bug | §1 | No fsync before/after checkpoint rename (`checkpoint.rs:47,117`) | TODOS — sync_all + dir fsync |
| F-009 | P2 / taste | §1,§3 | Global budget is a soft ceiling (overshoot ≤ 1 inf/agent) (`scheduler.rs:608`) | TODOS — document |
| — | P2 | §4 | Demo configs don't exercise caps/MCP/bus/spawn/sandbox | p4.7 — richer demo |
| — | P2 | §4 | No e2e checkpoint round-trip / V4 port-deny / FUSE concurrency tests | p4.7 — add |
| — | P2 | §5 | `cargo audit`/`cargo deny` absent from CI | p4.7 — add job |
| — | P2 | §3,§7 | Per-turn `Vec<Msg>` full clone (`agent/mod.rs:302`) | Phase 5 — `Arc<[Msg]>` |

---

## 9. Confidence

I read the orientation docs and the high-judgment source (scheduler, sandbox,
capability, main, and the relevant `agent/mod.rs` and `mcp.rs` spans) directly, and
delegated breadth reads of `checkpoint.rs`/`events.rs`/`config.rs`/`flight_recorder.rs`
and `surfaces`/`anthropic.rs`/tests/CI to focused sub-reviewers, then personally
verified every finding's cited lines that drives a P0/P1 (env leak by grep; F-002/F-003
from the `caps_to_rules` + `sandbox::compile`/`apply_compiled_inner` source; F-005 and
F-010 by reading the exact spans; the FS-lockout fix in `lib.rs`).

What I could **not** verify without running things: F-003 (user-namespace DAC
downgrade) is reasoned from the syscall sequence — I did not reproduce a `0600` write
failure on the QEMU/V4 target, and it is possible the deployment writes a uid_map
somewhere I did not see (I grepped `agentd/src`; nothing writes `uid_map`). F-002's
real-world impact depends on the operator running a kernel < 6.7; the QEMU target is
6.x and may or may not be ≥ 6.7. Binary size (§3) is quoted from NOTES.md, not
re-measured (no local `cross` build). The 253-test figure is the documented passing
count; a static `grep` counts 315 test *functions* including `cfg`-gated ones that
don't all compile on every target.

What would change the assessment: if a uid/gid_map is in fact written for the MCP
namespace (F-003 dissolves), or if the production kernel is guaranteed ≥ 6.7 and
`mcp_require_capabilities` is always on (F-002 narrows to a dev-only concern). Neither
changes F-001, which stands regardless of kernel or config.
