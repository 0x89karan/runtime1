# State-of-the-Union Audit — Phase 5 (Memory) / v0.25.0

**Auditor:** fresh Claude (Opus) review session, branch `docs/audit-phase-5`.
**Trigger:** `/autoplan` and `/qa` were skipped on most Phase 5 increments (p5.1–p5.8
shipped fast in ~2 days), so the operator does not trust that the full build works.
This is an independent review of the **new memory subsystem** at the Phase 5 → Phase 6
boundary. No code changes — this report is the deliverable, analogous to
`docs/AUDIT-phase-4-6.md` (whose findings became p4.7).

**Method:**
- **Stage 0 (build baseline):** full workspace `build`/`clippy`/`test`, `cross` musl
  release + size guard, `make clippy-linux`. *(run; results in §1)*
- **Stage 2 (code audit):** five parallel subsystem reads (store/redb core; paging +
  short-term; checkpoint-v2 + distill/evict; KB capabilities + tools; FUSE + BM25),
  with the highest-severity findings **re-verified by hand** against the code.
- **Stage 1 (behavioral / does-it-run):** **deferred** — `ANTHROPIC_API_KEY` not set
  and no `qemu` in this environment. Hand-off commands in §6.
- **Stage 3 (codex cross-check):** **blocked** — codex auth token revoked; pending
  re-login, to be appended to §7.

Severities: **P0** fix before any new feature work; **P1** fix in the next cleanup;
**P2** track. Each finding is labelled **bug** vs **taste**, and whether it is
**p4.6-shaped** (works on the happy path, fails silently/catastrophically on an
untested input, preventable by a type or assertion).

Scope read: `agentd/src/memory/{store,mod,context,index}.rs`, and the integration
touchpoints `agent/mod.rs`, `scheduler.rs`, `checkpoint.rs`, `capability.rs`,
`config.rs`, `tools/{native,mod}.rs`, `events.rs`, `surfaces/src/agents_fs.rs`,
`main.rs`.

---

## 1. Build & quality-gate baseline (Stage 0)

| Check | Result |
|---|---|
| `cargo build` debug + release (workspace) | ✅ clean |
| `cargo test` (workspace) | ✅ **all pass** — lib 357 (356 + 1 ignored) + integration/surfaces/sandbox (~481 executions) |
| `cross` musl static release | ✅ **3.98 MB** (4,181,984 B) — under the 6 MB guard, 2.1 MB headroom |
| `make clippy-linux` (production, Linux-gated) | ✅ clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | ❌ **9 lints** — see F-13 |

The foundation is real: it compiles, links as a 3.98 MB static binary (the redb
subsystem added ~0.9 MB over Phase 4's 3.1 MB, but "super light" held), and the whole
test suite passes. The distrust is **not** "it's a house of cards" — the defects below
are concentrated and specific.

---

## 2. Correctness & data-safety findings

### F-01 — **P1 / bug** — Working-memory paging is driven by cumulative lifetime spend, not context size *(verified; p4.6-shaped)*
`agent/mod.rs:327` computes `total_spent = self.total_input + self.total_output` — a
**monotonic lifetime accumulator** — and feeds it to `assess(total_spent,
token_budget)` (`:333`). The **Hard branch pages but is not edge-gated** (only the Soft
*advisory* checks `last_pressure == None`, `:335`). Consequence: once an agent crosses
~90% of its `token_budget`, **every** subsequent `step_need_infer` re-computes Hard
pressure and force-pages a turn — because paging mutates `self.messages` but never
decrements `total_input`/`total_output`, so the measured quantity never falls. The
working context is progressively gutted toward `[task, last-assistant]` while paging
delivers **zero** relief to the metric that triggered it. Silent context amnesia on any
long run; invisible except as repeated `memory_paged` events.
- **Fix:** drive pressure off an estimate of the **retained context size** (sum of
  current `messages` block lengths), not lifetime spend; and edge-gate the paging
  branch like the advisory. A type distinction (`ContextTokens` vs `LifetimeSpend`)
  would have prevented this.
- p4.6-shaped: yes — happy path (short demo never reaching 90%) looks perfect.

### F-02 — **P1 / bug** — `MemoryStore::open()` quarantines a *valid* store on any transient open error *(verified; p4.6-shaped)*
`memory/store.rs:54-82`: `open()` calls `try_open()`; on `Err` it special-cases only
the lock error ("AlreadyOpen"/"already locked"), and **treats all other errors as
corruption** — renaming `memory.redb` → `memory.redb.corrupt` and booting a fresh empty
store. But `try_open()` also does `set_permissions(0600)` (`:103`) and `init_schema`
(`:115`). So a transient **ENOSPC / permission / I/O** error from the chmod or
schema-init step silently renames the user's real memory away and boots empty = **total
memory loss mis-reported as corruption**. (Scope correction from the codex cross-check:
the namespace *backfill* write is explicitly non-fatal, `:174`, so it is **not** part of
the trigger — the misclassification comes from chmod + schema-init, not backfill.) The
only test feeds actual garbage, so the happy/garbage paths are covered but the
transient-error path is not.
- **Fix:** match redb's actual corruption variant (`redb::DatabaseError::…`/`Corrupted`)
  rather than "all other errors"; only quarantine on a provable parse-corruption from
  `Database::open`, not on `set_permissions`/`init_schema` failures.
- p4.6-shaped: yes — exactly the 4.6 template (ships green, an uncovered input destroys
  real data).

### F-03 — **P1 / bug (scope gap)** — Phase 5 eviction floor is implemented but **never called** *(verified)*
`memory/store.rs:731` `evict()` is fully implemented and `config.rs` exposes
`max_entries_per_segment` / `max_entry_age_days` (p5.6), but **no production code calls
`evict()`** — the only callers are `store.rs` tests. So the headline p5.6 guarantee,
"the store cannot grow unbounded," **does not hold**: memory grows forever. (Relatedly,
`evict()` takes no `MutabilityClass` and would delete `canon` entries — a dormant hole,
harmless only because nothing triggers it.)
- **Fix:** wire eviction into the store write path or a periodic sweep, honour the
  config knobs, and early-return on `canon` segments. Add a test that an over-cap
  segment actually shrinks *through the live path*.

### F-04 — **P1 / bug (agent-reported)** — Namespace/doc-count counters mask desync instead of preventing it
`memory/store.rs` `delete`/`evict` decrement `doc_count` and the NAMESPACES counter
under `if cur > 0` / clamp guards (≈`:447-465`, `:835-862`). The clamps *hide* any
divergence rather than asserting against it; if a counter ever drifts (e.g. via the F-02
backfill-failure path), `list_namespaces` permanently under-reports — and that feeds the
`/agents/kb/` FUSE surface, so a real segment silently shows as **empty memory**.
`doc_count` skew only perturbs BM25 ranking (harmless). *(Agent-reported; line cites
not independently re-verified — see §7.)*
- **Fix:** `debug_assert!` the counter equals the actual key count after each mutation,
  or reconcile periodically.

### F-05 — **P2 / bug (agent-reported)** — `checkpoint.save()` still has no `fsync` before/after rename
`checkpoint.rs` `save()` writes the tmp file then `rename`s, with **no `sync_all()` on
the data and no parent-dir fsync** — the durability gap flagged as F-012 at Phase 4.6,
**still unfixed**. A power-loss/crash after `rename` returns but before the page cache
flushes can leave a zero-length or torn `checkpoint.json`, defeating the atomicity the
module's own doc-comment claims.
- **Fix:** `f.sync_all().await` before drop/rename; fsync the containing dir after.

### F-06 — **P2 / bug (agent-reported)** — FUSE dynamic-inode counter never reclaimed / unguarded *(p4.6-shaped)*
`surfaces/src/agents_fs.rs` `next_dyn_ino` (starts 1,000,000) only ever increments
(`alloc_lt_file`/`alloc_kb_seg`/`alloc_kb_file`), is never reclaimed (even by
`prune_dead_agent`), and uses bare `+= 1` (no `checked_add`). On a long-lived PID-1
process churning many memory keys it grows unbounded; at overflow the increment panics
in debug / wraps in release — and a panic on the FUSE thread poisons the mount.
(`alloc_dir` has a guard; the dynamic allocators do not.)
- **Fix:** `checked_add` → reply `EIO` on `None`; ideally reclaim via a free-list or
  derive inodes deterministically.

### F-07 — **P2 / bug (agent-reported)** — `inject_messages` can append `Text` onto a ToolResult-only turn, then paging buries it
`agent/mod.rs` (mailbox drain on the *Tools* branch) reintroduces the 4.6 **F-006**
shape: a User turn of `[ToolResult…, Text]`. `page_turns` then previews `blocks.first()`
(the ToolResult), so a paged inter-agent message is misrepresented in `short_term` /
the FUSE `short_term` view. Not a crash; observability/correctness erosion that p5.3
recall inherits. *(Agent-reported.)*
- **Fix:** push injected text as a new turn when the last User turn is ToolResult-bearing;
  have the preview prefer the first `Text` block.

### F-08 — **P2 / taste (agent-reported)** — `short_term` is unbounded and full-cloned on every checkpoint + snapshot tick
`agent/mod.rs` `short_term` is never trimmed, holds full block JSON (not previews), and
is cloned in full on every `to_checkpoint` (written to disk) and every
`update_snapshot` pass. Combined with F-01 (paging every turn) it grows ~1 pair/turn →
unbounded RAM + checkpoint-file growth on long-lived agents, against the "super light"
goal.
- **Fix:** cap `short_term` (ring buffer / spill oldest to the p5.3 tier); store
  previews, not full JSON, in the in-memory buffer.

### Verified **not** a bug (a subagent's HIGH was wrong)
The checkpoint subagent flagged "corrupt-checkpoint quarantine is not wired at the
caller → agentd fails to start." **Refuted by direct read:** `main.rs:558-559` renames a
corrupt `checkpoint.json` → `.corrupt` and starts fresh, and `main.rs:154-213` does the
same for a corrupt memory store (emitting a flight event). Both quarantine paths are
wired. Dropped.

---

## 3. Capability / least-privilege

The core `KbRead`/`KbWrite` segment matching is **correct and p4.6-clean** — empty
grants fail-safe deny, sibling-prefix escape is blocked (`"agent:scratch"` ∤
`agent:scratchpad`), cross-type isolation holds (`capability.rs` tests). The exposures
are at the **edges where the namespace string is chosen**:

### F-09 — **P1 / bug (agent-reported)** — `spawn_agent.child_id` is unvalidated → forgeable memory namespace + provenance
`scheduler.rs` (`dispatch_spawn`) uses an agent-supplied `child_id` verbatim as the
child's id; the child inherits the parent's full cap set, and its `mem_remember` writes
to `agent/<child_id>`. `child_id` is **never** `validate_segment`'d (only an
ID-collision check). An agent with `Spawn` + memory can therefore aim a child's
namespace at another slot, embed `/`/`:`/`..`, and — since `provenance.agent_id` is
stamped from this id — **forge the `author`** that `kb_search` filters on.
- **Fix:** `validate_segment(child_id)`; reject ids colliding with configured agents or
  containing traversal; consider always auto-generating `child_id`.

### F-10 — **P2 / latent (agent-reported, single-tenant-softened)** — private `agent/<id>` tier shares keyspace + delimiter with operator-grantable `kb:*` segments
Granting `KbRead/KbWrite { segment: "agent" }` matches `agent/<any-id>` (slash
delimiter), silently exposing **every** agent's private memory — contradicting the
`mem_remember` privacy contract. Softened by single-tenant trust, but it's a
least-privilege foot-gun.
- **Fix:** reserve the `agent/` prefix (reject it in `validate_segment` for the
  kb_/kv_ tool paths), or namespace the self-grant under a sentinel the segment grammar
  can't express.

### F-11 — **P2 / taste (agent-reported)** — unconfigured segments default to *writable* Scratch
`segment_class` is an exact-string lookup; unconfigured → `None` → `kb_put`/`kv_set`
treat it as writable Scratch. Fail-open in a deny-by-default system (the capability gate
still applies, so it's not an escape).
- **Fix:** default unconfigured segments to read-only, or require explicit classification.

---

## 4. Test-coverage shape (Stage 2 Step 11)

357 lib tests pass, but count ≠ coverage. Concrete gaps:
- **No 2-boot / cross-respawn continuity test** for the p5.3.5 detachable volume (the
  promise that was never actually exercised) — 0 matches.
- **No distillation test** (`distill_on_complete`) — 0 matches.
- **Eviction is tested but unwired** (F-03) — the tests pass against a path nothing calls.
- Thin **concurrency** coverage (no FUSE-under-concurrent-writers; shared-KB concurrent
  writes).
- **`clippy --all-targets`** (test code) ungated by CI (F-13).

---

## 5. Prioritized findings

| ID | Sev | Area | Summary | Verified |
|---|---|---|---|---|
| F-01 | **P1** bug | paging | Pressure on lifetime spend → pages every turn, no relief, gutts context | ✅ by hand + codex |
| F-02 | **P1** bug | store | `open()` quarantines a valid store on any transient error → silent data loss | ✅ by hand + codex |
| F-16 | **P1** bug | agent | spawn_agent/send_message mixed with other tools → **terminates** agent; kills the flagship multi-agent demo live | ✅ live + code |
| F-14 | **P1** bug | demo | shipped `agents.toml` doesn't parse (`seed` field unsupported) | ✅ live + code |
| F-15 | P2 bug | demo | `agents.toml` grants Spawn but omits the `spawn_agent` tool | ✅ live + code |
| F-03 | **P1** bug | eviction | p5.6 eviction floor never called → unbounded growth (+ canon hole, dormant) | ✅ by hand |
| F-09 | **P1** bug | caps | `spawn_agent.child_id` unvalidated → forgeable namespace + provenance | agent-reported |
| F-04 | P1 | store | counter clamps mask desync → `list_namespaces`/FUSE shows empty memory | agent-reported |
| F-05 | P2 | checkpoint | no fsync before/after rename (4.6 F-012 unfixed) | agent-reported |
| F-06 | P2 | FUSE | dynamic inode counter unbounded + unguarded → panic on mount thread | agent-reported |
| F-07 | P2 | agent | inject-onto-ToolResult-turn (4.6 F-006 reintroduced); preview buries msg | agent-reported |
| F-10 | P2 | caps | `agent/` private tier grantable via `segment:"agent"` | agent-reported |
| F-08 | P2 | agent | `short_term` unbounded + full-cloned per checkpoint/snapshot | agent-reported |
| F-11 | P2 | caps | unconfigured segments default writable (fail-open) | agent-reported |
| F-12 | P2 | distill | distillation ignores per-agent budget; emits event even if store write failed | agent-reported |
| F-13 | P2 | CI | `clippy --all-targets` (tests) ungated; 9 lints incl. `await_holding_lock` | ✅ by hand |

---

## 6. Behavioral verification — RUN against the live API (Stage 1)

Run dev-mode against a real `ANTHROPIC_API_KEY` (QEMU steps still pending — no `qemu`
here). **The core runs; the flagship multi-agent demo does not.**

**What works (verified live):**
- **Single-agent scout (`agent.toml`):** full loop end-to-end — `agent_spawned →
  perceive → inference_request → inference_response → tool_call(list_dir) → tool_result
  → tool_call(read_file) → tool_result → observe → agent_checkpointed →
  inference_request → inference_response → agent_completed` (2 turns), correct answer on
  stdout. The build genuinely works.
- **Memory primitives:** `kb_put` to a scratch segment committed (`memory_write
  {class:scratch, tier:4, bytes:54}`, redb store created 180 KB on disk); `kb_get` of an
  absent key returned `found:false` gracefully; no false capability denials.
- **Secret redaction:** the API key appears **0 times** in the flight log / stderr.
- **No panics:** every failure path was a recorded `error`/`agent_failed`, never a crash.

**Stage-1 findings (all verified live + in code):**

### F-14 — **P1 / bug** — the shipped `agents.toml` memory demo does not parse
`[[memory.segments]]` declares `seed = [...]` to pre-load canon content, but
`MemorySegmentConfig` only accepts `name`/`class` (`config.rs:60-61`) →
`agentd agents.toml` fails at startup with `unknown field 'seed'`. The canonical
multi-agent/memory smoke-test demo is non-functional, and canon segments can be
*declared* but not *seeded* (so the writer's `kb_get(project:meta, guidelines)` has
nothing to read). CI never caught it (it runs `cargo test`, not the demo).
- **Fix:** implement segment seeding, or remove `seed` from the demo + document that
  canon is seeded another way.

### F-15 — **P2 / bug** — `agents.toml` grants `Spawn` but never registers the `spawn_agent` tool
`native = ["kb_put","kb_get","kb_search","mem_remember","mem_recall"]` omits
`spawn_agent` (and `send_message`/`list_agents`), yet the writer's task is to spawn a
reader. Live, the agent correctly reported "I don't have the capability to spawn" — the
demo's headline writer→reader flow never executes.
- **Fix:** add `spawn_agent` (and the bus tools) to the demo's `native` list.

### F-16 — **P1 / bug** — `spawn_agent`/`send_message` mixed with other tools **terminates** the agent *(verified in code)*
`agent/mod.rs:540` (spawn) and `:579` (send) return `AgentEffect::Failed` when the
model batches them with any other tool call in a turn. Live, haiku naturally batched the
demo's three steps (`kb_get` + `kb_put` + `spawn_agent`) into one turn → the writer was
**killed** (`agent_failed: "spawn_agent must be the sole tool call per turn"`), so the
cross-agent KB-read + provenance path could not be exercised at all. Models routinely
batch tool calls, so this makes the spawn + bus features fragile in practice. It also
violates the project convention that *tool failures are normal control flow* (an
`is_error` result the agent reacts to), not termination. A test even asserts the current
terminal behavior (`agent/mod.rs:1251`), so it is by-design and wrong.
- **Fix:** return an `is_error` tool_result for the spawn/send call (and run or
  also-`is_error` the siblings) so the agent retries with spawn alone, instead of dying.

**Not verifiable yet:** the cross-agent shared-KB read + provenance flow (blocked by
F-15 → F-16) and the QEMU 2-boot continuity (no `qemu` here). Run on a `qemu` host:
`cd distro && make run`.

---

## 7. Confidence

**Verified by hand against the code:** F-01 (paging trigger, `agent/mod.rs:327/333`),
F-02 (`store.rs:54-82` corruption classification), F-03 (`evict()` has no production
caller), F-13 (clippy gate), and the **refutation** of the "checkpoint quarantine not
wired" claim (`main.rs:558-559`). These I stand behind.

**Agent-reported, not independently re-verified** (high-quality subagent reads with
specific line cites, but confirm before acting): F-04, F-05, F-06, F-07, F-08, F-09,
F-10, F-11, F-12. Several are single-tenant-softened (F-09/F-10).

**Codex cross-check (Stage 3, independent cross-model review):** **F-01 — CONFIRMED**
(codex independently traced lifetime-spend → Hard-at-0.90, only the soft advisory
edge-gated; same fix: split budget accounting from working-context pressure, add
hysteresis / a post-page target). **F-02 — PARTIAL/CONFIRMED** (the transient-error
catch-all is real at `store.rs:57/71` via chmod `:103` + init `:115`; codex corrected
the backfill overstatement — `:174` is non-fatal). Codex also independently surfaced two
items already in this audit: the `page_turns` debug-assert-only alternation check
(`context.rs:85`, corroborates F-07) and the deterministic `.corrupt` quarantine path
clobbering prior evidence (`store.rs:84`). Net: the two verified P1s are corroborated by
an independent model.

**Not done:** Stage 1 behavioral verification (no `ANTHROPIC_API_KEY`/`qemu` in the
review environment) — run the §6 hand-off before declaring Phase 5 trustworthy.

**Recommendation:** a **p5.9 — Phase 5 hardening** increment (the analogue of p4.7) that
closes the P1s (F-01, F-02, F-03, F-09, F-04) before Phase 6 starts, with the P2s
tracked in `TODOS.md`. The behavioral QA (Stage 1) and the codex cross-check should run
as the gate before declaring Phase 5 trustworthy.
