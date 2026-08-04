# AUDIT — agentOS v0.118.0

Run 2026-08-01/02 against `attn.2-r1-r2-boot-and-restore` @ `0a2a54fa` (v0.118.0 + attn.2 R1/R2).
Five lanes: docs-vs-code, security/threat-model, test integrity, runtime correctness, product state.
~74k Rust LOC, ~6k Python, 5 crates, 23 docs.

**Every finding below carries a file:line quote from both sides of the contradiction.** Findings
that could not be quoted were dropped. Where a claim was measured rather than read, the
measurement is given.

---

## Verdict

The engineering discipline here is unusually high — the drift guards, the atomic checkpoint
write, the fail-closed universal-tier metering gate and the honest de-claims in `THREAT_MODEL.md`
are all better than typical. The problems are concentrated in three places:

1. **A measured, live defect that explains the product's central failure** and that the last
   increment did not fix.
2. **Claims in the outward-facing thesis doc that the code does not implement** — three of them
   security guarantees.
3. **A recurring false-green pattern in self-scanning tests**, now found in a fifth location.

The single most important finding is **R1** below. It is the difference between "the CoS is
unreliable" and "the CoS cannot work as configured."

> **⚠ SUPERSEDED 2026-08-04 (attn.3). The sentence above is WRONG, and R1's own severity claim with
> it.** Left in place because an audit is a record, not a living doc — but do not act on it. Measured
> during attn.3's /autoplan, on the same volume this audit read: retained context at the observed
> failure was **11,569 tokens against R1's own proposed trigger of 172,627** (15× away), and at the
> measured ~159 tokens/poll-pair the global budget dies at turn ~355 while paging first fires at
> ~1,086 — so **R1 is arithmetically inert on `cos-orchestrator`, the agent it headlines.** R1's
> claim to be "an independent, **sufficient** explanation for three briefs in fifteen days" is
> **withdrawn**; `audit118-R1` is now **P2 and BLOCKED** (paging is lossy, so fixing it alone is an
> active regression). **R2's premise was also wrong** — it asserts the 400 loop was in-memory ("no
> `checkpoint.json` in the volume"), but the volume holds 65 `agent_checkpointed` and 2
> `agent_restored` with the 400 landing one second after each restore; it is the restore path, already
> fixed by attn.2. **The actual dominant defect this audit missed:** the trigger spends ~3,456
> inference calls/day polling `wait_for_trigger` to be told "next fire 14 h from now" — 414,016 input
> tokens to *wait* — which is what empties the 10M/24h window. That is `attn.4`. See
> `docs/plans/attn.3-real-context-window.md` §0 and `TODOS.md`.

---

## R1 — CRITICAL. Context paging is keyed to the token BUDGET, not the context WINDOW

`agentd/src/memory/context.rs:43-47`:
```rust
pub fn assess(tokens: u64, token_budget: u64) -> MemoryPressure {
    if token_budget == 0 { return MemoryPressure::None; }
    let pct = tokens as f64 / token_budget as f64;
```
`agentd/src/agent/mod.rs:813`: `if assess(retained, self.cfg.token_budget) == MemoryPressure::Hard {`

`token_budget` is a **spend ceiling**. The constraint paging exists to protect is the **model's
context window**. There is no `context_window` concept anywhere in the codebase.

With `HARD_THRESHOLD = 0.90` and the shipped CoS budgets, the paging trigger sits far above the
200k window (`anthropic.rs:13` sends no `context-1m` beta header):

| agent | `token_budget` | Hard fires at | vs 200k window |
|---|---:|---:|---:|
| `cos-orchestrator` (`cos.agents.toml:277`) | 5,000,000,000 | 4.5e9 | **22,500× too high** |
| `cos-inbox` (`:346`) | 1,500,000 | 1.35M | 6.75× too high |
| `cos-curator` (`:443`) | 500,000 | 450k | 2.25× too high |

**Measured on the live `agentos_cos-data` volume** — 29 minutes, 65 inference requests:
`msg_count` climbs 121 → 126 monotonically, **zero** `memory_paged`, **zero**
`memory_pressure_advisory`, `total_tokens` **417,638**, `output/` empty.

Spend is quadratic in turns because the full transcript is resent each turn. At the measured
slope the 10M/24h global window is exhausted in 2–3 hours, after which every agent defers until
rollover — and the cron fires once daily.

**This is an independent, sufficient explanation for "three briefs in fifteen days," and
`attn.1a`'s restart-policy fix does not address it.** The config comment at `cos.agents.toml:276`
("The trigger's per-turn context is now tiny (it reads nothing), so this headroom is generous")
names the false assumption exactly: tiny per turn, but the transcript accumulates across
`max_turns = 200_000`, and the 5e9 budget disables the only drain.

**Fix:** give `assess()` a real context-window input, separate from the spend budget.

---

## R2 — CRITICAL. The dangling-`tool_use` repair runs only on the restore path

attn.2 R2 (this branch) fixes restore. But the live 400 loop was **in-memory**: there is no
`checkpoint.json` in the volume, and the same failure recurred 3.5 minutes apart with
`msg_count` frozen at 126 and `messages.125` the final element:

```
"error":"Anthropic API 400 Bad Request: messages.125: `tool_use` ids were found without
 `tool_result` blocks immediately after: toolu_01FonBTdyrQmHudK1ChQAHcZ"
```

`repair_dangling_tool_uses` is reachable only via `repair_and_record` (`scheduler.rs:110`), called
only at `scheduler.rs:419` and `:434` — both restore paths. **No equivalent invariant check runs
before an `InferenceRequest` is built.** Once the in-memory history is malformed it stays
malformed: every turn resends it and gets the same 400.

The repair's own doc comment (`agent/mod.rs:368-370`) names the runtime producer as reachable.

**Fix:** run the pairing check before every `InferenceRequest`, not only on restore. This is a
gap in the increment on this branch, found by auditing it.

---

## R3 — HIGH. Three security guarantees in `PRODUCT-THESIS.md` are not enforced in code

This is the outward-facing document.

| Claim | Reality |
|---|---|
| `:63` "a network namespace confines egress to the gateways… **without its cooperation**" | `universal.rs:68` passes `--network=host`; zero `IsolateNetwork` references in that file. Routing is an env hint (`ANTHROPIC_BASE_URL`) the workload can ignore. Tracked as `audit-S7` (P2) — but the thesis asserts the opposite. |
| `:65` "eBPF audits actual syscalls" | No eBPF anywhere. Only seccomp-**bpf** (`sandbox/src/lib.rs`). CLAUDE.md lists Phase 9 eBPF as unbuilt. |
| `:69`/`:108` "per-tool approval gates" / "approval gates on risky actions" | `config.rs:495-496`: the action is "**Passed by the agent** as the input to the `request_approval` tool". No config policy, no interception. An agent-initiated primitive, not a gate. |

Two `THREAT_MODEL.md` mitigations also cannot fire: `IsolateMount` (`:258`, under "What the
sandbox **enforces**") has zero production callers, and `CheckpointStore::remove()` (`:139`) —
the basis of the checkpoint risk-window argument — does not exist.

---

## R4 — HIGH. Self-scanning test guards are vacuous in a fifth location

`credential/mod.rs` does `let src = include_str!("mod.rs");` and asserts on bare literals. Because
`include_str!` includes the test module, the assertion's own text satisfies it.

Mutation-proven:

| Guard | Mutation applied | Result |
|---|---|---|
| `test_bytes_stream_err_arm_returns_502_source_guard` | replaced the 502 Err arm with `break` (returns a **truncated body as HTTP 200**) | **passed** — and no other test caught it either |
| `test_oauth_refresh_timeout_constant_and_wrapper_present` | removed the OAuth refresh timeout (the ar-05 regression) | **passed** — `ar-05` appears 6 times, 4 inside the test |
| `cos_spawn_caps_subset.rs:49-59` | added `Mcp { server = "shell_exec" }` to `cos-orchestrator` | **passed** — the arm is `Capability::Mcp { .. }`, a wildcard |

The third is the sharp one: it guards the **cap.2b P1-10 acceptance criterion**. The header claims
it proves the trigger holds "no Gmail, Credential, KB, FsWrite, BriefPublish, or Spawn", but only
Gmail is actually checked. `Mcp{semantic-kb}` and `Mcp{telegram_bridge}` would also pass.
Fix: `Capability::Mcp { server, .. } if server == "cron_trigger" => {}`.

**The codebase already knows this hazard and fixed it seven times** — `credential/mod.rs:2732`:
"Build the banned pattern from parts so the literal doesn't appear in this file". Seven guards use
`.concat()`; four do not. Better uniform fix: scan only the production region —
`include_str!("mod.rs").split("#[cfg(test)]").next().unwrap()`.

Python harnesses have the same shape: `http_mcp.py:153` and `search_mcp.py:178` wrap asserts in
`except Exception` and **exit 0** on failure (proven: a deliberately-false assert printed
"skipped" and passed). `oauth_mcp.py:1536` hardcodes `total = 37` while test 22 skips when a
fixture is absent — so the production image has always reported a test that did not run.

---

## R5 — CRITICAL/HIGH. Metering and panic-safety gaps

- **`mcp.rs:382`** — `&raw.trim()[..raw.trim().len().min(256)]` byte-slices a `&str`. The correct
  version is at **`mcp.rs:821` in the same file**, with a comment explaining why. `panic = "abort"`
  + PID 1 = full-system failure. One-line fix. Same class as the ux.6a `hex_decode` boot panic.
- **Streaming errors discard all token accounting.** `input_tokens` is known at `message_start`
  (`anthropic.rs:85`) and thrown away on any mid-stream failure; `scheduler.rs:1558`/`:1599` gate
  accounting behind `if let Ok(ref resp)`. Live: `inference_stream_started` = 12,
  `inference_stream_completed` = **0**. A persistently-failing stream bills at full rate while
  `tokens_spent` stays flat, so the budget never trips.
- **No panic hook anywhere** in any crate. The one event that ends the system produces no record.
  With `restart: unless-stopped`, a crash loop is invisible in the event stream. ~10 lines.
- **`catch_unwind` at `main.rs:1361` is a no-op** under `panic = "abort"`.
- **`flight.jsonl` rotation is `set_len(0)`** (`flight_recorder.rs:57`) — a total wipe of audit
  history at the 100 MB cap, contrasted with `evidence.rs`, which keeps segments.
- **`flight.jsonl` is world-readable** (`-rw-r--r--`) and carries model-output previews, while
  `memory.redb` and `egress-key.pkcs8` are 0600.
- **`google_device.rs:30-36`** uses `option_env!("OAUTH_CLIENT_SECRET")` — compiles a secret into
  the binary, against CLAUDE.md's stated invariant.

---

## R6 — MEDIUM. Docs that fail if followed literally

- `MCP_SERVERS.md:173-175`'s `[capabilities]` snippet is a **top-level table serde silently
  drops** (`config.rs:29-40` has no such field, and `deny_unknown_fields` is intentionally
  omitted). Following it yields an agent with no Google access and no error.
- `MCP_SERVERS.md:97` documents `passenv = ["BRAVE_SEARCH_API_KEY"]`, which `mcp.rs:46`
  **blocklists**. The same dead line ships in `templates/scout.template.toml:47` and
  `templates/librarian.template.toml:59`.
- `RUNBOOK.md:916-919` tells the operator to `rm agentd/checkpoint.json` "to clear spend". The
  window self-resets (`budget_reset_interval = 86400`) and the file holds live run state
  (context, awaiting set, pending approvals). `POST /api/v1/budget/reset` exists.
- `MCP_SERVERS.md:120` / `DEPLOYMENT.md:385` say the requested scopes need no Google verification;
  `agentctl/src/auth/google.rs:17` requests `drive.readonly`, a **restricted** scope.
- `cos.agents.toml:52` tells operators to grep for `kind=="egress_rejected"`; the real kind is
  `egress_denied` (`events.rs:286`). The query returns empty forever, reading as "no attempts".
- `CONVENTIONS.md:280` says only the final answer reaches stdout; streaming defaults on and prints
  chunks there (`config.rs:590-597`).
- `model.provider` dispatches nothing — `AnthropicGateway` is constructed unconditionally.
  `provider = "openai"` silently calls Anthropic.
- `docs/AUDIT-v0.97.md` is cited by `CLAUDE.md:182` and `CHANGELOG.md:711` but **is not in the
  tree**. Recoverable: `git checkout 102f351c -- docs/AUDIT-v0.97.md`.
- `README.md:17` Status reads "Phases 0–7 … complete (v0.66.0)" — 52 releases stale.
- `AGENTD_MANAGEMENT_ENABLED` / `AGENTD_MANAGEMENT_PORT` enable the unauthenticated :7999 control
  API and appear in no shipped doc. `SIDECAR_SECRET` is the semantic-KB sidecar's only inbound
  auth and is missing from that server's own env table.
- `otel/README.md:54` inverts the `OTEL_REDACT_PREVIEWS` default (code defaults **true**).

---

## R7 — Product state

- **The CoS produced output on 3 of 16 days** and is down now (`Exited(1)`, `RestartCount=10`).
  R1 above is a second, independent cause beyond the boot failure this branch fixes.
- **The mv design-partner gate is 2026-10-01 — 61 days out, 0 of 10 named, 0 of 3 demos, zero
  engineering required.** It is the only irreversible item on the board.
- **TODOS.md overstates open debt.** It lists 17 open P1s; at least two are done — `ux.2b (P1)`
  shipped as v0.111.0 (`CLAUDE.md:176`) and `brief-05 (P1)` is "CLOSED with evidence"
  (`CLAUDE.md:239`). The P1 list is the work queue, so an inflated queue misprioritises.
- CI is green on `main` (6/6). The 5 "unmerged" branches are squash-merge residue. The stash is
  2 lines of `Cargo.lock`. PR #141 has been open 9 days. Tag gaps at v0.97–v0.108 and v0.110–v0.112;
  the head IS tagged (v0.118.0).

---

## What is healthy (recorded so it is not re-litigated)

`compose_policy.rs`, `entrypoint_gates.rs`, `env_denylist_parity.rs`, `repo_consistency.rs`,
`distro_packaging.rs` and `conventions_completeness.rs` are properly anchored, filter comments,
parse values rather than substring-matching, and carry negative controls. The checkpoint write is
genuinely atomic and durable (`create_new` + 0600 + `sync_all` before `rename`, then a parent-dir
fsync). Universal-tier metering is fail-closed at boot (`main.rs:209`). All native tools are async
or `spawn_blocking`. `PASSENV_BLOCKLIST` covers the real keys. `THREAT_MODEL.md` §8.7's de-claims
are honest and accurate — including the `SecretRewriter` gap, which is correctly documented as
never built.

---

## Suggested order

1. **R1** — re-key `assess()` to a real context window. Nothing in the brief/attention track can be
   measured until this is fixed; it is the live outage.
2. **R2** — run the pairing check before every inference, not only on restore.
3. **R5 `mcp.rs:382`** — one line, mirror `:821`.
4. **R4 `cos_spawn_caps_subset.rs`** — the wildcard arm is guarding a P1 security criterion.
5. **R5 panic hook** — ~10 lines, closes the largest "Record everything" hole.
6. **R3** — correct the three PRODUCT-THESIS claims. It is the outward-facing document.

---

## Process note

The audit fan-out **contaminated the working tree**. Subagents performed mutation testing by
editing live source in place rather than in a worktree, including adding
`Mcp { server = "shell_exec" }` to the schedule-exposed `cos-orchestrator` — the exact inverse of
cap.2b. Their cleanup then over-reverted by filename and destroyed an unrelated legitimate edit.
The tree was verified clean afterwards and the `shell_exec` grant confirmed absent.

Two rules follow, now logged as learnings: run mutating agents in an isolated worktree, and commit
your own work before fanning out.

---

## Addendum — late findings (event taxonomy sweep)

### R8 — CRITICAL. `CompletedTruncated` makes the durable record contradict the outcome

`agent/mod.rs:976-989` emits `EventKind::AgentCompleted` on **both** branches (payload does carry
`"truncated": truncated`), then returns `AgentEffect::CompletedTruncated`. The scheduler converts
that to a failure at `scheduler.rs:2170` — `handle_agent_terminal(…, Err(anyhow!("model output
truncated at max_tokens…")))` — and emits nothing. Verified: `EventKind::AgentFailed` appears in
`scheduler.rs` only at `:741`, `:753`, `:962`, none on this path.

So `flight.jsonl` says `agent_completed` while `runs.redb` and FUSE say `failed`. `kind` is the
field every consumer switches on. `CompletedTruncated` is the one `AgentEffect` with no event kind
of its own — budget.1-ar-01 introduced it so the scheduler would treat it distinctly, and the
taxonomy never followed.

**Fix:** give it a kind, or emit `AgentFailed` at `scheduler.rs:2170`.

### R9 — HIGH. `EventKind::ALL` is not compile-time exhaustive, mutation-proven

Current state has zero drift: 84 variants, 84 `as_str()` arms, 84 `ALL` entries. The *guard* is the
problem. Adding a variant produces exactly two compile errors — `as_str()` and `touch()` — and
neither forces it into `ALL`:

| Added to | `conventions_completeness` | `all_is_exhaustive` | Compiles |
|---|---|---|---|
| enum + `as_str()` + `touch()` | **PASS (false green)** | **PASS (vacuous)** | **yes** |

That is the realistic developer path, and it ships an undocumented kind with every guard green.
`all_is_exhaustive` iterates `ALL` to check `ALL`, so it can only catch variants already in it. All
three `ALL` consumers are downstream of the same unenforced array. The file header claims a
verified negative control, but it exercises only the doc side — the one direction that works.

**Fix:** generate `ALL` from the `as_str()` match so it is compile-time exhaustive; that repairs
all downstream guards at once. Record the enum-side scenario as the negative control.

### R10 — HIGH/MEDIUM. Three unrecorded lifecycle transitions

- `scheduler.rs:1982-1996` — the reset-window branch parks an agent Running → Deferred but emits
  `AgentAdmissionDenied`, which `CONVENTIONS.md:75` documents as *terminal*. `AgentDeferred` is
  never emitted, and the code comments "deferral is NOT denial (ux.8′)" 18 lines later. **This is
  the default CoS path** (`budget_reset_interval = 86400`).
- `scheduler.rs:1876-1878` — budget holdback re-queue is silent every drain cycle.
- `scheduler.rs:1331` — cancel drops pending approvals with no per-approval event, on the
  authority boundary.

**Root cause worth recording:** `AgentStatus` is not stored. It is derived at snapshot time
(`scheduler.rs:3451-3474`) as a projection over five collections, so there is no chokepoint where a
status change can be forced to emit. "Record everything" is unenforceable by construction for
lifecycle state — it is convention at ~20 scattered sites. That is why these gaps coexist with
otherwise strong discipline.

### Correction to H4

The flight-recorder path is MEDIUM, not HIGH: `record()` does blocking file I/O on reactor workers,
but with no `fsync` and no mutex held across an `.await`. The HIGH case is `evidence.rs:347-354`,
which fsyncs under a `std::sync::Mutex` inside the inference future via `record_inference`.
