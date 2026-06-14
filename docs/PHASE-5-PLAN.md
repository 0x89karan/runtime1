# Phase 5 — Memory Substrate: Build Plan

> The standalone, buildable increment plan for Phase 5. Each increment is one
> gstack cycle: `/autoplan` → build → `/review` → `/qa` → `/ship`. The design
> source of truth is `docs/DESIGN-memory.md`; this doc is buildable without
> flipping back to it for basic facts. Audit blockers are in
> `docs/AUDIT-phase-4-6.md`. ROADMAP.md carries the paragraph-level summary.

---

## A. Pre-Phase-5 readiness checklist

Confirm all of these before starting **p5.1**. Each: how to verify / what to do if
it fails.

1. **All P0 audit findings closed.** *Verify:* AUDIT-phase-4-6.md F-001 fixed (MCP
   subprocess env is filtered — `grep -n "env_clear\|env_remove" agentd/src/tools/mcp.rs`
   returns a hit; a test asserts the child env excludes `ANTHROPIC_API_KEY`).
   *If it fails:* it is the first item in p4.7; do not start p5.1.
2. **Working-memory abstraction is extractable.** *Verify:* the per-turn
   `messages.clone()` at `agent/mod.rs:302` is gone — the request carries
   `Arc<[Msg]>` (AUDIT F-009). *If it fails:* sequence in p4.7; paging on top of a
   full-clone-per-turn multiplies cost.
3. **`inject_messages` ordering fixed.** *Verify:* AUDIT F-005/F-006 closed —
   mailbox injection happens at a clean turn boundary (a test injects between
   `provide_inference` and `step` and asserts ordering). *If it fails:* p4.7; a
   second writer to `messages` (paging) compounds the bug.
4. **Checkpoint format version pinned + probe-before-trust.** *Verify:* AUDIT F-011
   closed — `checkpoint.rs::load` reads a `{ format_version }` probe struct before
   the full deserialize and distinguishes "too new" (refuse) from "corrupt" (rename);
   tmp filename is unique per write. *If it fails:* p4.7; p5.3's `FORMAT_VERSION`
   bump to 2 has no safe source otherwise.
5. **Capability vocabulary frozen for memory.** *Verify:* `KbRead { segment: String }`
   and `KbWrite { segment: String }` are the agreed additions (DESIGN-memory §4); no
   competing proposal open. *If it fails:* resolve in design review before p5.1 — the
   vocabulary is load-bearing across p5.1–p5.8.
6. **Storage substrate vetted against the 4 MB budget.** *Verify:* a throwaway
   `cargo add redb` + empty use, `cross build --release --target
   x86_64-unknown-linux-musl`, `stat -c %s` shows the binary still ≤ 4,194,304 bytes.
   *If it fails:* re-open the substrate decision (DESIGN-memory §4) before p5.1.
7. **FUSE read bounds fixed.** *Verify:* AUDIT F-004 closed — `agents_fs.rs:305` uses
   `saturating_add`. *If it fails:* p4.7; required before p5.7 exposes larger memory
   files.
8. **Event taxonomy is current.** *Verify:* the six undocumented kinds (AUDIT F-013)
   are in the CONVENTIONS.md table. *If it fails:* p4.7 (cheap, do it with the rest).

---

## B. Pre-Phase-5 cleanup increment — p4.7 (REQUIRED)

The audit recommends it and Phase 5 depends on it. Written in full increment format.

> #### ✓ p4.7 — Pre-Phase-5 cleanup (audit blockers)
>
> **Depends on:** Phase 4.6 (current `main`).
>
> **Goal:** Close the audit findings that block Phase 5 or compound once a second
> writer touches working memory. After p4.7, the env-leak is fixed, working memory
> is cheaply pageable, the checkpoint format can evolve safely, the FUSE read path
> is panic-free, and the event taxonomy/README are accurate. No new features; `main`
> stays shippable and demo flight logs are byte-for-byte unchanged.
>
> **Design reference:** `docs/AUDIT-phase-4-6.md` F-001, F-004, F-005, F-006, F-009,
> F-011, F-013, F-014.
>
> **Scope — files modified:**
> - `agentd/src/tools/mcp.rs` — F-001: `cmd.env_clear()` + allowlist (`PATH`, `HOME`,
>   `LANG`, `TERM`) + optional per-server `env` map; spawn sites at the three
>   `McpClient::spawn` paths.
> - `agentd/src/config.rs` — optional `env: HashMap<String,String>` on
>   `McpServerConfig` (`#[serde(default)]`, p4.6 pattern).
> - `agentd/src/inference/mod.rs` — F-009: `InferenceRequest.messages: Arc<[Msg]>`.
> - `agentd/src/agent/mod.rs` — F-009 build `Arc<[Msg]>` (no per-turn deep clone);
>   F-005/F-006 drain the mailbox at a clean turn boundary (after the assistant turn
>   is pushed) and push a new user message when the last turn carries `ToolResult`.
> - `agentd/src/scheduler.rs` — F-005 move the `drain_mailbox` call to the
>   post-`step_with_response` boundary.
> - `agentd/src/checkpoint.rs` — F-011: `VersionProbe { format_version }` read before
>   full deserialize; unique tmp name (`checkpoint.json.<pid>.<nanos>.tmp`); never
>   delete a tmp this call didn't create.
> - `surfaces/src/agents_fs.rs` — F-004: `offset.saturating_add(size as usize)`.
> - `docs/CONVENTIONS.md` — F-013: add `tools_registered`, `agent_child_result_delivered`,
>   `agent_checkpointed`, `agent_restored`, `system_shutdown_requested`, `fuse_skipped`.
> - `README.md` — F-014: phase status → "Phases 0–4 complete (v0.16.0)".
>
> **Capability additions:** none.
>
> **Event additions:** none (documents six existing kinds).
>
> **Tests added:**
> - `tools::mcp::tests::child_env_excludes_api_key` — spawn a fixture, assert env has
>   no `ANTHROPIC_API_KEY`.
> - `agent::tests::mailbox_injected_after_assistant_turn` — inject between
>   `provide_inference` and `step`; assert the message lands after the assistant turn.
> - `agent::tests::inject_into_tool_result_turn_pushes_new_message`.
> - `checkpoint::tests::load_future_version_refuses_not_corrupts` — v2 file → "too new"
>   error, file NOT renamed `.corrupt`.
> - `checkpoint::tests::concurrent_save_unique_tmp` — two saves don't clobber.
> - A serialization test asserting each `EventKind` → snake_case string (taxonomy lock).
>
> **Test invariants that must hold:**
> - `jq 'del(.ts)' flight.jsonl` for the `agent.toml` and `agents.toml` demos is
>   byte-for-byte identical before and after p4.7 (the `Arc<[Msg]>` and mailbox-timing
>   changes must not alter the unused-memory event sequence).
>
> **Acceptance criteria:**
> - `cargo build` debug + release; `cargo clippy -- -D warnings`; `cargo test` pass.
> - `make clippy-linux` clean (touches `agents_fs.rs`, `mcp.rs`).
> - `cargo test` count increases by ≥ 6.
> - Binary size unchanged within ±20 KB (no deps added except `Arc`, which is std).
> - `docs/CONVENTIONS.md` taxonomy table has all event kinds; the lock test passes.
>
> **Out of scope (explicit):**
> - F-002 (Net-port pre-V4 inversion) and F-003 (userns DAC downgrade) — sandbox-net
>   hardening, **not memory-blocking**; sequence as **p4.8** (below) or accept. They do
>   not gate p5.1.
> - F-007/F-008/F-010/F-012 — tracked in TODOS.md; low priority, non-blocking.
>
> **Known risks / open questions:**
> - The `Arc<[Msg]>` change ripples through `InferenceRequest` construction, the
>   gateway, and checkpoint serde (`Arc<[Msg]>` serializes as a seq — verify
>   round-trip). If the ripple is large, split p4.7 into **p4.7a** (env + FUSE +
>   docs + checkpoint) and **p4.7b** (`Arc<[Msg]>` + mailbox ordering).

> #### ▢ p4.8 — Sandbox net/userns hardening (recommended, non-blocking)
> **Depends on:** p4.6. **May run in parallel with p5.x — does not gate Phase 5.**
> Closes AUDIT F-002 (deny-default when `Net.ports` present but kernel ABI < 4) and
> F-003 (write `uid_map`/`gid_map` after `unshare(CLONE_NEWUSER)` so Landlock-allowed
> FS access isn't defeated by DAC). Full format deferred to its own plan; listed here
> so it isn't lost. Acceptance: a Linux integration test writes a 0600 file through a
> net-isolated sandbox; a pre-V4 `Net{ports}` config logs a degradation warning and
> falls back to `IsolateNetwork`.

---

## C. Phase 5 increments

### Phase 5 events preview (consolidated — merged into CONVENTIONS.md as each ships)

| kind | added in | when | data shape |
|---|---|---|---|
| `memory_read` | p5.1 | a Tier-2/3/4 read returns | `{tier, segment?, agent, items}` |
| `memory_write` | p5.1 | a Tier-3/4 write commits | `{tier, segment?, class?, agent, bytes}` |
| `memory_unavailable` | p5.1 | store open/read failed | `{stage, error}` |
| `memory_quarantined` | p5.1 | corrupt store → `.corrupt` | `{path}` |
| `memory_paged` | p5.2 | Tier-1 → Tier-2 paging | `{agent, blocks, forced, freed_tokens_est}` |
| `memory_distilled` | p5.3 | Tier-2 → Tier-3 promotion | `{agent, items, segment}` |
| `kb_search` | p5.5 | a `kb_search` tool call returns | `{segment?, query_preview, hits}` |
| `memory_evicted` | p5.6 | capacity/age eviction | `{segment, key, reason}` |

8 new event kinds. KB capability denials reuse the existing `capability_denied`.
(p4.7 separately backfills 6 pre-existing kinds → CONVENTIONS grows by 14 rows total.)

---

> #### ▢ p5.1 — Storage primitive (redb behind `MemoryStore`)
>
> **Depends on:** p4.7
>
> **Goal:** A durable, crash-safe, capability-gated key/value store exists behind a
> thin trait, usable by a single agent via `kv_get`/`kv_set` native tools over a
> reserved `scratch:` namespace. No tier integration yet — this is the substrate,
> demoable in isolation. The 4 MB binary budget is preserved.
>
> **Design reference:** `docs/DESIGN-memory.md` §4 (storage substrate), §5 (events).
>
> **Scope — files added:**
> - `agentd/src/memory/mod.rs` — `MemoryStore` trait (`open`, `get`, `put`, `append`,
>   `iter`, `delete`, `meta_version`); types `MemItem`, `KbEntry`, `Provenance`,
>   `MutabilityClass { Canon, Log, Scratch }`.
> - `agentd/src/memory/store.rs` — `RedbStore`: opens `memory.redb` (mode 0600),
>   tables `entries` (key `(namespace, key)` → `KbEntry`) and `meta`
>   (`format_version`); quarantine-on-corrupt (`memory.redb.corrupt`); never deleted
>   on success.
> - `agentd/tests/memory_integration.rs` — MockGateway agent uses `kv_set` then
>   `kv_get`; flight log shows `memory_write`/`memory_read`.
>
> **Scope — files modified:**
> - `agentd/Cargo.toml` — add `redb` (pin a version; pure-Rust, no default C deps).
> - `agentd/src/capability.rs` — add `KbRead { segment }` / `KbWrite { segment }`;
>   `satisfies`/`satisfies_type` prefix logic (mirror `FsRead`), empty-segment grant
>   = deny.
> - `agentd/src/tools/native.rs` — `KvGet`/`KvSet` tools; `required_capability_for`
>   computes `KbRead/KbWrite { segment }` from the call's namespace arg.
> - `agentd/src/events.rs` + `docs/CONVENTIONS.md` — the four p5.1 events.
> - `agentd/src/config.rs` — `[memory]` section: `path` (default `memory.redb`),
>   `enabled` (default true).
> - `agentd/src/main.rs` — open the store, pass into the registry/scheduler;
>   `memory_unavailable` on open failure (proceed without memory — best-effort).
>
> **Capability additions:**
> ```rust
> Capability::KbRead  { segment: String }
> Capability::KbWrite { segment: String }
> ```
> Backward compat: new variants — old configs simply lack them; deny-by-default.
> `#[serde(default)]` reserved for future fields on these variants.
>
> **Event additions:** `memory_read`, `memory_write`, `memory_unavailable`,
> `memory_quarantined` (shapes in the preview table).
>
> **Tests added:**
> - `memory::store::tests::round_trip_basic` — put then get returns equal.
> - `memory::store::tests::write_durable_across_reopen` — survives drop + reopen.
> - `memory::store::tests::corrupt_file_quarantines_and_starts_empty`.
> - `memory::store::tests::mode_0600_on_create` (unix).
> - `capability::tests::kb_read_prefix_match` / `kb_write_empty_segment_denies`.
> - Integration: `kv_set_then_kv_get_via_mock_agent`.
>
> **Test invariants that must hold across increments:**
> - The `agent.toml` / `agents.toml` demos produce an identical flight-event sequence
>   when memory is unused (no agent has a KB capability and calls no memory tool).
>
> **Acceptance criteria:**
> - `cargo build` debug + release; `clippy -D warnings`; `cargo test` pass.
> - `make clippy-linux` clean.
> - `cargo test` count increases by 6.
> - New `memory_write`/`memory_read` events present in the integration test's log
>   (`jq 'select(.kind=="memory_write")' flight.jsonl` matches).
> - **Binary size delta documented in the PR; expected ≈ +0.6 MB, must stay ≤ 4 MB**
>   (the CI guard). If the musl build exceeds 4 MB, the increment fails — re-open §4.
> - `docs/CONVENTIONS.md` taxonomy updated (4 rows).
>
> **Out of scope:** tiers (p5.2+), provenance enforcement (p5.4), search (p5.5),
> FUSE (p5.7), eviction (p5.6).
>
> **Known risks:** redb's on-disk format version vs. our `meta.format_version` — keep
> them distinct (redb owns file format; we own schema version). Confirm redb's musl
> build under `cross`.

---

> #### ▢ p5.2 — Per-agent short-term memory + paging
>
> **Depends on:** p5.1, p4.7 (Arc<[Msg]> + checkpoint version)
>
> **Goal:** Working memory (Tier 1) and short-term (Tier 2) are cleanly separated. The
> context manager pages evictable blocks from Tier 1 to Tier 2 under token-budget
> pressure — deferring while budget allows, force-paging at a hard ceiling so an agent
> never exceeds budget and never silently loses state. Tier 2 survives SIGTERM/restart
> via the checkpoint.
>
> **Design reference:** `docs/DESIGN-memory.md` §3 (Tier 1/2, eviction policy), §6.
>
> **Scope — files added:**
> - `agentd/src/memory/context.rs` — `ContextManager`: `estimate_request_tokens`,
>   `page_out(&mut messages, n) -> Vec<MemItem>`, soft/hard threshold logic, page
>   markers.
>
> **Scope — files modified:**
> - `agentd/src/agent/mod.rs` — `short_term: Vec<MemItem>` on `AgentTask`; pre-infer
>   paging check in `step_need_infer`; `mem_page { op: get|put }` and
>   `mem_scratch { op, key, value }` tools; soft-threshold system note injection.
> - `agentd/src/checkpoint.rs` — `short_term` field on `AgentCheckpoint`;
>   `FORMAT_VERSION` 1 → 2; `#[serde(default)]` so v1 loads (empty short-term).
> - `agentd/src/config.rs` — `[memory] page_soft_pct` (default 80), `page_hard_pct`
>   (default 95).
> - `agentd/src/events.rs` + CONVENTIONS — `memory_paged`.
>
> **Capability additions:** none (Tier 2 is the agent's own; no grant needed).
>
> **Event additions:** `memory_paged { agent, blocks, forced, freed_tokens_est }`.
>
> **Tests added:**
> - `memory::context::tests::soft_threshold_advertises_not_forces`.
> - `memory::context::tests::hard_ceiling_force_pages_until_fits`.
> - `memory::context::tests::never_pages_system_task_or_open_tool_use`.
> - `agent::tests::self_page_via_mem_page_tool`.
> - `checkpoint::tests::v1_checkpoint_loads_with_empty_short_term` (back-compat).
> - `checkpoint::tests::short_term_survives_roundtrip`.
> - Integration: agent under a tight budget pages, then recalls via `mem_page(get)`.
>
> **Test invariants:** demos with slack budget never emit `memory_paged` — identical
> unused-memory event sequence.
>
> **Acceptance criteria:**
> - Build/clippy/test/clippy-linux clean. Test count +7.
> - `FORMAT_VERSION == 2`; a v1 checkpoint fixture loads cleanly.
> - Under a budget that forces paging, `memory_paged { forced: true }` appears and the
>   agent still completes (no `budget_exceeded` where paging could have saved it).
> - CONVENTIONS updated (1 row). Binary size delta ≤ +50 KB.
>
> **Out of scope:** persistence of Tier 2 beyond the run (it's discarded on
> completion — that's Tier 3, p5.3); shared access (p5.4).
>
> **Known risks:** token estimation accuracy — a too-low estimate lets a request
> exceed budget anyway. Mitigate with a conservative estimator + the existing p1.3
> budget guard as backstop. **Most likely increment to need scope adjustment** —
> paging policy tuning may reveal the estimator needs the real tokenizer.

---

> #### ▢ p5.3 — Per-agent long-term + checkpoint coexistence
>
> **Depends on:** p5.1, p5.2
>
> **Goal:** Agents durably remember across runs. `mem_remember` writes to the agent's
> own `agent/<id>` namespace; `mem_recall` reads it back after a restart. checkpoint
> and the memory store coexist with distinct roles (checkpoint = crash recovery,
> deleted on success; memory store = durable, kept).
>
> **Design reference:** `docs/DESIGN-memory.md` §3 (Tier 3), §2.2 + §6 (coexistence).
>
> **Scope — files added:** none (extends `memory/`, `agent/`).
>
> **Scope — files modified:**
> - `agentd/src/agent/mod.rs` — `mem_remember { content, tags }`,
>   `mem_recall { query, limit }` tools; implicit self-grant for `agent/<self-id>`.
> - `agentd/src/memory/store.rs` — namespace conventions (`agent/<id>`); provenance
>   stamping on write (runtime-supplied `agent_id`/`turn`/`ts`/`task_fp`).
> - `agentd/src/capability.rs` — implicit-self-grant injection helper.
> - `agentd/src/events.rs` + CONVENTIONS — `memory_distilled`.
>
> **Capability additions:** none new; formalizes the implicit `agent/<self-id>` grant.
>
> **Event additions:** `memory_distilled { agent, items, segment }` (manual remember
> counts as a one-item distillation; the auto path lands in p5.6).
>
> **Tests added:**
> - `memory::store::tests::remember_then_recall_across_reopen`.
> - `agent::tests::self_namespace_access_without_explicit_grant`.
> - `agent::tests::cross_agent_tier3_read_requires_kbread` (denied without grant).
> - Integration: agent A remembers; restart; A recalls; checkpoint absent, memory
>   present.
>
> **Test invariants:** demos still identical when no agent calls `mem_remember`.
>
> **Acceptance criteria:**
> - Build/clippy/test/clippy-linux clean. Test count +4.
> - After a clean completion, `memory.redb` exists and `checkpoint.json` does not
>   (`ls` assertion in the integration test).
> - Provenance present on every Tier-3 entry; a test asserts the agent cannot set
>   `agent_id`/`turn` (runtime-stamped).
> - CONVENTIONS updated (1 row).
>
> **Out of scope:** shared (multi-agent) segments (p5.4); auto-distillation (p5.6).
>
> **Known risks:** `task_fp` stability — define as first 16 hex of sha256(task); pass
> the timestamp in (no `Date::now()` in pure logic; use the recorder's clock seam).

---

> #### ▢ p5.3.5 — Detachable memory volume (distro/infra)
>
> **Depends on:** p5.1 (`store_path`). Independent of p5.4+ — infra-only, parallelizable.
> **Sequence:** run next / in parallel with p5.4; land before relying on container-respawn
> memory continuity. Not a blocker for the crate work.
>
> **Goal:** Make the durable store (`memory.redb`, Tiers 3/4) a separate, persistent,
> re-attachable volume — distinct from the ephemeral container, the secrets-in mount, and
> the disposable output mount. Kill + respawn the AgentOS container → re-attach the same
> volume → knowledge continuity. **No crate logic, no schema, no migration; default
> `store_path` unchanged.**
>
> **Design reference:** `docs/DESIGN-memory.md` §2.2 (checkpoint vs memory) + §6
> (Persistence — the detachable memory volume).
>
> **Scope — files modified (exact diff):**
> - `distro/overlay/init` — add a third 9p mount, mirroring `secrets0`/`output0`:
>   ```sh
>   mkdir -p /run/secrets /run/output /run/memory
>   # …after the secrets0 + output0 mounts:
>   mount -t 9p -o trans=virtio,version=9p2000.L memory0 /run/memory || {
>       echo "ERROR: failed to mount memory0 via 9p." >&2
>       echo "       Is -virtfs ...,mount_tag=memory0 in the QEMU command?" >&2
>       exec sh
>   }
>   ```
> - `distro/Makefile` — add to the `run` AND `test` targets (test → throwaway dir so real
>   runs aren't polluted; `run` → create `~/.agentos-memory` like `~/.agentos-secrets`):
>   ```make
>   # run target:
>   -virtfs local,path=$(HOME)/.agentos-memory,mount_tag=memory0,security_model=none,id=memory0
>   # test target:
>   -virtfs local,path=$(CURDIR)/$(OUTPUT_DIR)/test-memory,mount_tag=memory0,security_model=none,id=memory0
>   ```
> - `distro/overlay/etc/agentd/agent.toml` — `[memory]` with
>   `store_path = "/run/memory/memory.redb"` (and add `kv_get`/`kv_set` to `native` if the
>   demo exercises memory).
> - `agentd/src/config.rs` — **no default change** (keep `"memory.redb"`); add a doc-comment:
>   "container/production deployments set `store_path` to an absolute path on a persistent
>   mount, e.g. `/run/memory/memory.redb`."
> - `docs/RUNBOOK.md` — §2b (three-mount model + `~/.agentos-memory`), §3 (filesystem
>   layout), §6 (backups: the volume is the durable artifact). While here, fix the stale
>   `4 MB` guard references → `6 MB` (p5.2 bumped the CI guard).
> - `docs/THREAT_MODEL.md` — note: the memory volume is durable + outside the container, so
>   the at-rest-encryption gap (§3.3) applies with a larger window; mode 0600 + host perms.
>
> **Capability additions:** none. **Event additions:** none.
>
> **Tests added:**
> - `config::tests::absolute_store_path_honored` (if not already covered by p5.1).
> - **2-boot QA** (document in the PR; full automation needs a two-boot harness): `make run`
>   → agent `kv_set`s a key → halt → `make run` → `kv_get` returns it. A scripted version
>   drives two QEMU boots against the same `~/.agentos-memory` and asserts via the flight log.
>
> **Test invariants that must hold:** `cargo test` is unchanged (default `store_path`
> untouched); the single-agent demo flight sequence is unchanged when memory is unused.
>
> **Acceptance criteria:**
> - `make build/run/test` pass; `ls /run/memory` works on the console; the demo's
>   `memory.redb` lands in `~/.agentos-memory/` on the host.
> - A `kv_set` value survives a fresh boot (2-boot QA documented in the PR).
> - Wiping `/run/output` or `make clean` does **not** lose memory.
> - `make clippy-linux` clean (no crate logic changed). Default `store_path` unchanged.
> - `docs/RUNBOOK.md` + `docs/THREAT_MODEL.md` updated.
>
> **Out of scope:** checkpoint-on-volume (cross-respawn *run*-continuity) — a separate
> optional toggle; concurrent multi-container access (needs the Layer-2 KB service, §4);
> at-rest encryption (THREAT_MODEL gap, separate increment).
>
> **Known risks:** redb is **single-writer** — the volume supports *sequential* container
> generations, not two at once (that's the external KB service, §4). The p5.8
> store-path-vs-sandbox invariant is satisfied for free: `/run/memory` is outside any MCP
> server's FS sandbox prefix.

> #### ▢ p5.4 — Shared KB MVP (namespace + mutability classes + provenance)
>
> **Depends on:** p5.3
>
> **Goal:** Multiple agents read/write a shared, segmented KB. One namespace axis with
> three mutability classes (`canon` read-only, `log` append-only, `scratch` mutable).
> `KbRead`/`KbWrite` are enforced across agents. Every write is provenance-stamped and
> unforgeable. The §4 worked example (A logs, B retrieves) passes as a test.
>
> **Design reference:** `docs/DESIGN-memory.md` §4 (segmentation, write semantics,
> worked example).
>
> **Scope — files modified:**
> - `agentd/src/agent/mod.rs` — `kb_put { segment, class, content, citation? }`,
>   `kb_get { segment, key }` tools.
> - `agentd/src/memory/store.rs` — class-aware writes: canon→deny-agent, log→append
>   `(segment, seq)`, scratch→RMW with `version`.
> - `agentd/src/config.rs` — `[[memory.segments]]` (name, class) to declare/seed
>   segments and canon content.
> - `agentd/src/capability.rs` — KB enforcement across non-self namespaces.
> - `agentd/src/events.rs` + CONVENTIONS — extend `memory_write`/`memory_read` with
>   `tier:4` + `class`.
>
> **Capability additions:** none new (uses p5.1's `KbRead`/`KbWrite`).
>
> **Event additions:** none new (extends existing shapes with `class`/`tier:4`).
>
> **Tests added:**
> - `memory::store::tests::log_segment_is_append_only_immutable`.
> - `memory::store::tests::scratch_last_writer_wins_increments_version`.
> - `memory::store::tests::canon_write_by_agent_denied`.
> - `agent::tests::provenance_stamped_and_unforgeable`.
> - Integration `worked_example_a_logs_b_retrieves` — scout (KbWrite project:acme)
>   writes; analyst (KbRead project:, no Net) retrieves with provenance; flight shows
>   `memory_write{tier:4}` then `memory_read{tier:4}`.
> - `agent::tests::kbwrite_outside_grant_denied` (`capability_denied` event).
>
> **Test invariants:** demos unchanged when no agent holds a KB capability.
>
> **Acceptance criteria:**
> - Build/clippy/test/clippy-linux clean. Test count +6.
> - The worked-example integration test passes end-to-end.
> - `docs/THREAT_MODEL.md` gains a stub §7.1/§7.2 (cross-agent flow, provenance
>   integrity) — full text in p5.8, but the surface changed here so it's noted.
> - CONVENTIONS reflects `class`/`tier` fields. Binary size delta ≤ +30 KB.
>
> **Out of scope:** ranked search (p5.5 — p5.4 reads are key/segment scans);
> eviction (p5.6); FUSE (p5.7).
>
> **Known risks:** segment-creation policy (who may create a new namespace) — Phase 5
> says segments are declared in config or implicitly created on first `KbWrite` within
> a granted prefix; confirm that implicit creation can't be used to escape a grant.

---

> #### ▢ p5.5 — Retrieval as tool (lexical search)
>
> **Depends on:** p5.4
>
> **Goal:** `kb_search` returns ranked entries for a query, scoped to a segment and
> optionally filtered by author, over a tokenized inverted index — no embeddings, no
> network. The agent-facing retrieval API is complete.
>
> **Design reference:** `docs/DESIGN-memory.md` §4 (read semantics, storage substrate
> — inverted index).
>
> **Scope — files added:**
> - `agentd/src/memory/index.rs` — tokenizer (lowercase, split, stopword drop),
>   `index` table (word → posting list), BM25-lite scorer in Rust.
>
> **Scope — files modified:**
> - `agentd/src/memory/store.rs` — maintain the index on `append`/`put`/`delete`.
> - `agentd/src/agent/mod.rs` — `kb_search { segment?, query, author?, limit? }` tool.
> - `agentd/src/events.rs` + CONVENTIONS — `kb_search`.
>
> **Capability additions:** none (search requires `KbRead` on the queried segment).
>
> **Event additions:** `kb_search { segment?, query_preview, hits }`.
>
> **Tests added:**
> - `memory::index::tests::ranks_relevant_entry_first`.
> - `memory::index::tests::segment_scoped_search_excludes_other_segments`.
> - `memory::index::tests::author_filter`.
> - `memory::index::tests::index_updated_on_write_and_delete`.
> - `agent::tests::kb_search_requires_kbread_on_segment`.
> - Integration: search after multi-entry writes returns ordered hits with provenance.
>
> **Test invariants:** demos unchanged when no `kb_search` is called.
>
> **Acceptance criteria:**
> - Build/clippy/test/clippy-linux clean. Test count +6.
> - `kb_search` flight event present; `hits` matches the returned count.
> - Search latency note in the PR (brute-force scorer is fine for the MVP scale).
> - CONVENTIONS updated (1 row). Binary size delta ≤ +40 KB.
>
> **Out of scope:** vector/semantic search (deferred — DESIGN-memory §9 Q1); index
> compaction (folds into p5.6 eviction).
>
> **Known risks:** index/entry consistency on crash mid-write — wrap the entry write +
> index update in one redb transaction so they commit atomically.

---

> #### ▢ p5.6 — Eviction & summarization policy
>
> **Depends on:** p5.5
>
> **Goal:** The store cannot grow unbounded: per-segment capacity and age floors evict
> oldest entries (and their index postings). Optionally, an end-of-run distillation
> pass **compiles** the run's salient short-term items into markdown-wiki Tier-3 entries
> (the "llm-wiki" content format, DESIGN-memory §4) — distilled, human-readable, and
> lexically searchable — rather than copying raw turns.
>
> **Design reference:** `docs/DESIGN-memory.md` §3 (eviction), §4 (failure modes), §9 Q3.
>
> **Scope — files added:** none (extends `memory/store.rs`, `memory/context.rs`).
>
> **Scope — files modified:**
> - `agentd/src/memory/store.rs` — `evict(segment)`: drop oldest beyond
>   `max_entries`/`max_age`; remove index postings in the same txn.
> - `agentd/src/config.rs` — `[memory] max_entries_per_segment`, `max_entry_age_days`,
>   `distill_on_complete` (default false).
> - `agentd/src/scheduler.rs` — on agent completion, if `distill_on_complete`, run the
>   distillation (one bounded inference; counts against budget) → `memory_distilled`.
> - `agentd/src/events.rs` + CONVENTIONS — `memory_evicted`.
>
> **Capability additions:** none.
>
> **Event additions:** `memory_evicted { segment, key, reason }` (`reason` ∈
> `capacity`|`age`).
>
> **Tests added:**
> - `memory::store::tests::evicts_oldest_beyond_capacity`.
> - `memory::store::tests::evicts_entries_past_max_age`.
> - `memory::store::tests::eviction_removes_index_postings`.
> - `scheduler::tests::distill_on_complete_promotes_to_tier3` (MockGateway).
> - `scheduler::tests::distill_disabled_no_extra_inference` (default path unchanged).
>
> **Test invariants:** with eviction config at defaults (generous) and distillation
> off, demos are unchanged — no `memory_evicted`/extra inference.
>
> **Acceptance criteria:**
> - Build/clippy/test/clippy-linux clean. Test count +5.
> - `memory_evicted` appears when a segment exceeds its cap in a test.
> - Distillation, when enabled, shows one extra `inference_request` + a
>   `memory_distilled` event and respects the budget guard.
> - CONVENTIONS updated (1 row).
>
> **Out of scope:** FUSE (p5.7); the automatic-retrieval question (DESIGN-memory §9 Q2,
> deferred entirely).
>
> **Known risks:** distillation spending cognition the user meters — gate behind the
> default-off flag and a per-distillation token cap; document in THREAT_MODEL DoS.

---

> #### ▢ p5.7 — `/agents/<id>/memory/...` + `/agents/kb/` FUSE (read-only)
>
> **Depends on:** p5.4 (KB exists), p4.7 (F-004 FUSE read bounds)
>
> **Goal:** Memory is observable from the control plane. `/agents/<id>/memory/` shows an
> agent's short-term and long-term; `/agents/kb/<segment>/` is an operator browse of
> shared segments. Read-only, following the existing inode scheme.
>
> **Design reference:** `docs/DESIGN-memory.md` §5 (surfaces layout).
>
> **Scope — files modified:**
> - `surfaces/src/snapshot.rs` — per-agent memory view + KB segment list in the
>   snapshot (best-effort, `try_write` as today).
> - `surfaces/src/agents_fs.rs` — `memory/` subtree per agent dir; `kb/` top-level dir;
>   inode allocation via the existing `alloc_dir` (root=1, dirs 1010 step 10).
> - `agentd/src/scheduler.rs` — populate the memory view in `update_snapshot`.
> - `agentd/src/events.rs` — none (reuses fuse_* + memory_read on backing reads).
>
> **Capability additions:** none. The `/agents/kb/` view is an **operator** surface,
> not an agent capability — it does not bypass `KbRead` for agents.
>
> **Event additions:** none.
>
> **Tests added:**
> - `surfaces::tests::memory_subtree_lists_short_and_long_term`.
> - `surfaces::tests::kb_segment_browse_returns_entry_with_provenance`.
> - `surfaces::tests::large_memory_entry_read_does_not_panic` (F-004 regression).
> - `surfaces::tests::memory_view_stale_not_torn_under_concurrent_write`.
>
> **Test invariants:** `/agents/<id>/{status,context_size,budget,flight}` unchanged;
> only `memory/` is added.
>
> **Acceptance criteria:**
> - Build/clippy/test clean; **`make clippy-linux` clean (Linux-gated FUSE code)**.
> - Test count +4.
> - `cat /agents/<id>/memory/long_term/<key>` returns the entry + provenance footer in
>   a QEMU/Linux manual check (documented in the PR).
> - No new panics; F-004 regression test passes.
>
> **Out of scope:** writable FUSE (memory is written via tools, never via the VFS);
> live tail of memory writes.
>
> **Known risks:** snapshot bloat — the memory view must be a *bounded* projection
> (counts + recent keys), not the full store, or `update_snapshot` grows O(store) per
> tick. Cap the projection size.

---

> #### ▢ p5.8 — Phase 5 hardening pass
>
> **Depends on:** p5.1–p5.7
>
> **Goal:** Close the increment's accumulated debt: complete the THREAT_MODEL §7,
> assert the sandbox/store-path invariant, verify the CONVENTIONS table, ship a demo
> that actually exercises memory, and sweep TODOS. The Phase 5 equivalent of p4.5/p4.6.
>
> **Design reference:** `docs/DESIGN-memory.md` §7 (sandbox invariant), §8 (threat
> model).
>
> **Scope — files modified:**
> - `docs/THREAT_MODEL.md` — write §7 in full (7.1–7.6 from DESIGN-memory §8).
> - `agentd/src/main.rs` — **startup assertion (p4.6-shaped invariant):** the
>   `memory.redb` path must not fall inside any MCP server's `AllowFsRead`/`AllowFsWrite`
>   prefix; `bail!` if it does.
> - `agentd/agents.toml` — a demo config exercising memory: two agents, one with
>   `KbWrite { "project:" }` + `Net`, one with `KbRead { "project:" }`, a seeded
>   `canon:` segment, spawning, and a non-zero `global_token_budget` so paging +
>   admission are both demonstrated.
> - `docs/CONVENTIONS.md` — final completeness check (all 14 added rows present);
>   `events.rs` row added to the module-boundary table.
> - `TODOS.md` — close memory items; record any deferred (vectors, auto-retrieval).
>
> **Capability additions:** none.
>
> **Event additions:** none (verification only).
>
> **Tests added:**
> - `main::tests::store_path_inside_sandbox_prefix_fails_startup`.
> - A taxonomy-completeness test asserting every emitted `memory_*`/`kb_*` kind is in
>   the CONVENTIONS table (extends the p4.7 lock test).
> - Integration: the new demo runs end-to-end on MockGateway, exercising
>   write→search→retrieve→page with the expected event sequence.
>
> **Test invariants:** the *old* single-agent `agent.toml` demo is still byte-for-byte
> identical (memory unused); the *new* memory demo is the one that exercises the
> features.
>
> **Acceptance criteria:**
> - Build/clippy/test/clippy-linux clean. Test count +3.
> - `THREAT_MODEL.md §7` complete; the sandbox-path assertion test passes.
> - The memory demo's flight log contains `memory_write`, `kb_search`, `memory_read`,
>   `memory_paged` (jq-verifiable).
> - CONVENTIONS table complete; `TODOS.md` swept. Binary size still ≤ 4 MB.
>
> **Out of scope:** vectors/semantic search, automatic retrieval, at-rest encryption
> (all explicitly deferred to a later phase per DESIGN-memory §9).
>
> **Known risks:** the demo must not require a live network — use MockGateway in the
> integration test; the shipped `agents.toml` is for manual/live runs.

---

## D. Phase 5 exit criteria

After p5.8 ships, all of these are observable:

- An agent can `kb_put` a finding and a *different* agent, spawned later with only a
  `KbRead` grant and no network, can `kb_search`/`kb_get` it — with provenance — and
  the exchange is fully recorded (`memory_write` → `kb_search` → `memory_read`).
- Working memory pages to short-term under budget pressure (`memory_paged`) and the
  agent recalls paged content; an agent that would have hit `budget_exceeded` instead
  completes because paging deferred the cost.
- Long-term memory survives process restart: `memory.redb` persists across runs while
  `checkpoint.json` is deleted on success.
- KB segments enforce `canon`/`log`/`scratch` semantics and `KbRead`/`KbWrite`
  capabilities deny-by-default; a denial emits `capability_denied`.
- `/agents/<id>/memory/` and `/agents/kb/<segment>/` are browsable read-only on the
  QEMU image.
- The binary is still ≤ 4 MB (`cross` musl build under the CI guard).
- `THREAT_MODEL.md §7` documents the memory security surface; the
  store-path-vs-sandbox-prefix invariant is asserted at startup.
- The single-agent demo's flight-event sequence is unchanged from Phase 4 (memory is
  invisible when unused).

## E. Dependencies on later phases

Phase 6 (interface) will surface memory views to the human. Contracts Phase 5 must
expose cleanly for Phase 6:

- **A read-only query API independent of the agent loop.** Phase 6's console/root
  agent needs to `kb_search`/`kb_get` and read an agent's Tier-3 without *being* that
  agent. p5.7's operator `/agents/kb/` view is the seed; Phase 6 will want a
  programmatic equivalent (a `MemoryStore::query` that takes an explicit capability
  set rather than an agent identity).
- **Stable provenance schema.** Phase 6 will *display* provenance; freeze the
  `Provenance` fields in p5.4 and treat changes as a versioned migration.
- **A stable `memory.redb` schema version** in the `meta` table, evolved by the same
  probe-before-trust discipline as checkpoint, so Phase 6 tooling can read the store
  without guessing the layout.
- **Event taxonomy stability.** Phase 6 dashboards will key off `memory_*`/`kb_*`
  events; their data shapes are frozen at p5.8 and changed only via CONVENTIONS-tracked
  migrations.
