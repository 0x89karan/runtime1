<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/ux.6-evidence-autoplan-restore-20260729-001554.md -->
# ux.6a — De-claim the receipt chain + close the evidence boot trap

**Status:** /autoplan complete through Eng + DX. **Awaiting final approval gate.**
Reshaped from `ux.6-evidence-view.md` at the premise gate (2026-07-29): both CEO voices returned
RESHAPE, and the user split the increment.

**What this is:** the honesty + durability half. No new UI.
**What this is not:** `ux.6b` (signed action ledger) — DEFERRED, see `ux.6b-signed-action-ledger.md`.

**Closes filed debt:** `audit86-P2-4` (P2, evidence.jsonl cannot rotate safely) and `audit-S5` (P2,
signed chain not re-verified on resume). Both were assigned to `run.1`, which shipped without them.

---

## Why this reshape happened

The original plan proposed a TUI view over the Ed25519 receipt chain under the roadmap's label
"Provable accountability". Findings, all code-verified:

1. **The chain cannot record a denial.** `EvidenceWriter::record_denied` (`evidence.rs:130`) is
   reached only through `EgressProxy::record_denied` (`egress.rs:265`), whose only two call sites in
   the workspace are **tests** (`evidence.rs:326`, `egress.rs:770`). The measured 2 022/2 022
   `verdict: "allowed"` sample is a property of the *code*, not of the run.
2. **`evidence.rs` has an unbounded whole-file read on a fail-closed boot path** — `resume_chain`
   (`:184-197`) + `main.rs:1139`. This is `audit86-P2-4`, filed and never done.
3. **The original plan's two non-negotiables were mutually exclusive** — "bounded reads" and "inline
   verify + badge" cannot coexist, because `verify_chain` starts at `expected_seq = 0` / `GENESIS_HASH`
   (`:254-255`). Shape A was not cheap; its cost was mis-estimated at the premise stage.
4. **Docs contradict each other.** `THREAT_MODEL.md:554` correctly de-claims this subsystem
   (*"`EvidenceWriter` (p7.5) is the signing mechanism for the **egress proxy** path only"*), while
   `ROADMAP.md:1260` claims "Provable accountability … EU AI Act Art.12" and `PRODUCT-THESIS.md:100`
   claims the boundary "emits hash-chained, Ed25519-signed **action receipts**".

**Premise correction:** the earlier draft's P2 said the mv design doc could not be found. It exists,
**outside the repo**, at `~/.gstack/projects/0x89karan-runtime1/0x89karan-mv-governed-agent-runtime-design-20260720-114634.md`
(APPROVED 2026-07-20) — and it **already rejected** widening the receipt chain ("Approach C … harden
receipts before hypervisor work" — *"dogfooding requires none of it yet"*). That is why ux.6b is
deferred rather than built.

## Corrections made at the Eng phase (this plan was wrong about four things)

- **C1 — `agentd` does NOT fail closed on a corrupt chain today.** `resume_chain` performs *zero*
  verification: no `serde_json::from_str`, no `seq` check, no `chain_prev_hash` check, no signature
  check. It counts lines and SHA-256s the last one's bytes. It fails only on I/O error or invalid
  UTF-8. **A fully tampered, reordered, or forged chain boots fine.** An earlier draft listed
  "must still boot fail-closed on a corrupt chain" as a non-negotiable — that described fiction, and
  an implementer obeying it literally would build a *new* brick (an operator who archived or
  hand-edited `evidence.jsonl` could never boot again).
- **C2 — rotation is NOT blocked by genesis-anchored verification.** That is true only of *in-place
  truncation*. **Rotation by rename needs no format change and no verifier change:** `evidence.jsonl`
  → `evidence.jsonl.1`, fresh file restarts at `(0, GENESIS_HASH)`, and `agentctl verify <segment>
  <pubkey>` passes on **both** segments, because each is a complete genesis-anchored chain. This kills
  the case for checkpoint anchors entirely, and it means `audit86-P2-4` can be closed more cheaply
  than the audit assumed (it called for "teach `agentctl verify` non-genesis starts" — unnecessary).
- **C3 — `egress.rs:533` is not a rejection.** Lines 535-546 *coerce* a non-JSON upstream
  content-type to `application/json` and forward. No 4xx. So there are **two** attributable deny
  sites, not three.
- **C4 — both remaining attributable sites are unreachable in production.** `egress.rs:491-492`
  states it in the code: *"today cos ships no `universal` agents and no `[egress] proxy_addr`, so this
  proxy never starts in production."* Wiring only those two would replace "structurally cannot say no"
  with "in practice never says no" — **the increment would not deliver its own headline.** The
  production-reachable denial is native scheduler admission (`AgentAdmissionDenied`,
  `scheduler.rs:~1690` `global_budget_exhausted`, `~1767` `agent_budget_exhausted`);
  `SchedulerState.egress: Option<Arc<EgressProxy>>` is already in scope at `:149`.
- **C5 — allow and deny use different `action` strings.** `record_allowed("inference", …)`
  (`egress.rs:231`) vs `record_denied("egress", …)` (`:266`) — they cannot even be grouped.

---

## Scope — three work items, no UI

### 1. De-claim (docs only)

Bring overclaiming docs down to `THREAT_MODEL.md` §8.7's standard: the chain covers **egress
(inference) calls through the proxy** and nothing else.

- `ROADMAP.md:1260-1262` → replace the ux.6 line with ux.6a + ux.6b (deferred). Drop "Provable
  accountability" and the Art. 12 "build it once for both" cross-track claim.
- `ROADMAP.md:934` — same overclaim ("every step is in OTLP + the signed `evidence.jsonl`").
- `ROADMAP.md:1305` — "ux.6 evidence, shared with mv track".
- `ROADMAP.md:65` — points at `docs/plans/0x89karan-mv-governed-agent-runtime-design-*.md`, which **is not
  in the repo**. Fix the pointer to the explicit `~/.gstack/...` path, or vendor the doc.
- `PRODUCT-THESIS.md:100-101` — "action receipts" → egress/inference receipts, scoped honestly.
- `RUNBOOK.md:652` — "Tamper-evident: Ed25519-signed receipt chain" needs scoping.
- **State key custody, not just coverage.** `EvidenceWriter::open` generates the key locally
  (`evidence.rs:88-99`) and **overwrites the `.pub` from the private key on every open** (`:101-103`);
  `verify_chain` reads that same `.pub`. Anyone who can write the directory can mint a key and rewrite
  from genesis. `resume_chain` derives `seq` by counting lines, so deleting the file silently restarts
  at 0 and still verifies. With rotation, segment seams are unprovable. Therefore: **self-attestation
  against a local key, not third-party evidence.** Goes in `THREAT_MODEL.md` §8.7.
  (The OV-1 guards at `main.rs:1084-1131` keep MCP sandboxes off the key and are irrelevant here —
  the excluded adversary is the operator/root/the agent process itself.)

**DX — three documented commands are broken** (verified: `verify.rs:7-12` requires **two**
positionals):
- `DEPLOYMENT.md:248` and `DEPLOYMENT.md:595` pass one argument → the command fails as printed.
  `:595` also labels it "Are credentials valid?", which the verifier has nothing to do with.
- `STATUS.md:189` says `agentctl verify <flight.jsonl>` — wrong file entirely.
- `RUNBOOK.md:747` greps `egress_rejected`, which is not an event kind and never fires.

### 2. Record the mv gate date (docs only — user decision at the gate: mv is LIVE)

Verified: **no gate date and no `mv.*` increment** anywhere in `ROADMAP.md`, `TODOS.md`, `CLAUDE.md`.
The approved mv doc assigned: name the external gate — earlier of mv.3 shipped or **2026-10-01** —
plus 10 named humans and 3 booked demos. Record the date + `mv.*` increments in the roadmap. Its own
warning applies: *"an unnamed gate date is how 'deferred' becomes 'never'."*

Also file a correction against the mv doc: its demo package ("~2 days, everything exists") requires
"normal tool use → signed receipt" and "forbidden capability → denied + receipt". **Neither action
class is receipted and no deny was reachable**, so that estimate is wrong.

### 3. De-trap + deny path (code)

---

## Eng rulings

### Q1 — boot trap → **(a) bounded tail resume + (c) rename-based rotation. Reject (b) anchors.**

**Bounded tail resume.** Replace the whole-file read: `stat` → seek to `max(0, len − 64 KiB)` → read
window → split on `b'\n'` → take the last *complete* segment → `serde_json::from_str::<ActionReceipt>`
→ return `(receipt.seq + 1, sha256(segment))`. If no `\n` in the window and `len > window`, double to
1 MiB, then error.

- `seq` moves from positional to read-from-receipt. **Strictly better** — it reads the same field
  `verify_chain` checks (`:261`), so resume and verify can no longer disagree. Not a format change:
  `seq` has been on every line since p7.5.
- **Highest-risk line in the increment:** the segment splitter must reproduce `str::lines()`
  byte-for-byte (splits on `\n`, strips one trailing `\r`) or **every existing chain breaks at its
  next append**. Guarded by a mandatory differential test, below.
- One accepted divergence: a file ending `"\n\n"` today yields `sha256("")` and counts the empty line.
  Such a file *already* fails `verify_chain` (`:258`). Divergence on an already-unverifiable file is
  fine; scope the differential test to well-formed files and document it.

**Torn tail — self-heal, warn loudly, never brick.** Today a mid-line truncation is hashed as if it
were a line, the next `writeln!` appends directly onto it (O_APPEND inserts no newline), and the chain
becomes **permanently unverifiable while `agentd` boots happily** — silent, not fail-closed.
- Trailing bytes that do **not** deserialize → `set_len(last_newline_offset + 1)`. This can only
  discard bytes that were never `sync_data()`'d, i.e. a receipt that was never durable.
- Trailing bytes that **do** deserialize (final newline stripped) → write one `\n` before appending.
- Additionally verify the tail receipt's signature with the just-loaded key (available at `:99-103`,
  before the `resume_chain` call at `:105`). **On failure: warn and boot anyway.** This closes
  `audit-S5` with real detection and no new brick; full detection stays `agentctl verify`'s job.
- Surface via `pub fn resume_note(&self) -> Option<&str>` set during `open`; `main.rs` records it
  after `:1139` (where `recorder` already exists). **Do not change `open`'s signature** — six call
  sites across `evidence.rs`, `egress.rs`, `agentctl/src/verify.rs` tests.
- Accepted: the self-heal erases evidence of one appended garbage fragment. Anyone who can append can
  also rewrite from genesis and mint a key (`:88-103`). The flight event records that the repair happened.

**Rotation.** `MAX_EVIDENCE_BYTES` = 32 MiB (≈100k receipts at ~330 B ≈ months of CoS operation),
checked in `open` (before `resume_chain`) and in `write_receipt` under the `Inner` mutex. At the cap:
`rename` → **create and swap in a new `File`/`BufWriter`** → reset `inner.seq = 0`,
`inner.chain_prev_hash = GENESIS_HASH`.
- **Trap:** after `rename` the existing fd still writes to the renamed inode. You must *replace*
  `inner.writer`. (`FlightRecorder` uses `set_len(0)` instead to preserve the inode for the otel
  `tail.rs` sentinel — `flight_recorder.rs:53-65`. Nothing tails `evidence.jsonl`; the only readers
  are `agentctl verify` and the operator. So rename is safe here, and truncate-in-place is *not*,
  because it destroys the old segment.)

**Rollback safety (the audit86-P3-4 test), explicit:**
1. No byte of the on-disk line format changes. Every new line verifies under the old `verify_chain`
   and any already-built `agentctl`.
2. Roll back before any rotation: old code counts lines → `seq = N`; new code reads line N−1's `seq`
   → N. Identical.
3. Roll back after a rotation: the old binary sees a smaller complete chain starting at seq 0;
   counting yields the same next `seq`. `evidence.jsonl.1` is ignored, and still verifies standalone.
4. **No `FORMAT_VERSION` is introduced or bumped.**

**(b) anchors — rejected.** In-file anchors make old `verify_chain` hard-error at `:258-259` on first
contact — exactly the audit86-P3-4 shape. The rollback-safe sibling-file variant needs an anchor
record type, its own signing, `verify_from_anchor`, an `agentctl verify` mode, and rotation interplay:
2-3× the rest of this increment, and verbatim the work the mv doc already rejected. Relitigation.

**Hard constraint the earlier draft missed:** **nothing may be added to `ReceiptBody`/`ActionReceipt`.**
`verify_chain` reconstructs `ReceiptBody` field-by-field and re-serializes it to obtain the signed
bytes (`:279-288`). Add a field and an old verifier deserializes into the old struct, re-serializes
*without* it, and **every new signature fails** — same trap, with no version constant to warn you.
There is nowhere to put a denial `reason`; it goes in the flight event.

### Q2 — deny path → **(i), extended to the production-reachable native site, and edge-triggered.**

**Reject (ii) categorically — it is a DoS and an evidence-eviction primitive.** `write_receipt`
(`:134-177`) holds a `std::sync::Mutex` across `writeln!` + `flush()` + `sync_data()`, and is called
from async contexts (`egress.rs:611`; `scheduler.rs:1473`/`:1497`). Wiring `:365`/`:379` = **one fsync
per unauthenticated request**, serialized on one mutex, on a tokio worker, before any auth. A caller
that reaches the loopback proxy (a universal-tier workload in its sandbox is exactly such a caller)
gets unbounded growth of the file `agentd` reads at boot, a mutex it can pin so real receipts stall,
and — **once rotation lands — the ability to roll the audit log and evict older segments at will.**
It is also *less* honest: a receipt with `principal: "unknown"` has no subject, and padding the chain
with subject-less rows buries the rows that matter.

**Bound (i) too.** `:463` fires on *every* request from a budget-exhausted workload, so a retry loop
reproduces the same primitive one auth hop later.

**Design rule: signed receipt count must be bounded by real work, never by request volume.** Allowed
receipts already satisfy this (each costs metered tokens). So:
- `denied_edges: Mutex<HashSet<(String, &'static str)>>` on `EgressProxy`.
- `record_denied_policy(&self, agent_id, target, reason: &'static str)`: **always** emit
  `EventKind::EgressDenied` (per-attempt — keeps `count_egress_by_agent` accurate, bounded by flight
  rotation); write the signed receipt + `ActionReceiptEmitted` **only on the first occurrence of
  `(agent_id, reason)`**.
- Clear that agent's edges in `record_inference` on success ⇒ a second denied receipt requires an
  intervening *allowed* inference, which costs tokens. Receipts are bounded by budget, transitively.
- Semantics to document: the **chain** records that the boundary said no to this principal for this
  reason; the **flight log** records how many times. The TUI's "N denied" counts attempts while the
  chain holds one receipt per episode.

**Sites to wire:**
- `egress.rs:463` → `reason = "budget_exhausted"`.
- `egress.rs:495` → `reason = "global_budget_exhausted"`.
- `scheduler.rs:~1690`, `~1767` (and the legacy branch of `enqueue_or_defer`) — **terminal** branches
  only; `target = gateway.model_id()`. Naturally bounded: the agent terminates.
- **Do not** receipt `scheduler.rs:~1662` (`reason: "shutdown"`) — not a policy denial, and its loop
  drains the whole `deferred` queue (N fsyncs on the shutdown path).
- **Do not** receipt the deferred (`budget_reset_interval > 0`) branches. Deferral is not denial —
  ux.8′ fought for that distinction.
- **Do not** touch `:348, :365, :379, :391, :399, :418, :436, :522, :558, :571`; keep
  `record_proxy_failed`. `:533` is not a deny site (C3).
- Fix C5 while here: `action = "inference"` for inference denials. Changing an existing string field's
  *value* on new lines is rollback-safe (old `verify_chain` never inspects it).

Not (iii) delete: `scheduler.rs:~1690`/`~1767` are production-reachable, so the deny path has a real caller.

### Q3 — events → **no new kind. Two existing kinds are mis-applied.**

- `EgressDenied` is already defined for exactly this (`CONVENTIONS.md:113`), and the consumer chain
  already exists end to end: `inspector.rs:52` Egress filter → `reader.rs:364` counter →
  `views.rs:1249` Detail render. **Zero UI work**, consistent with the no-UI non-negotiable.
- `ActionReceiptEmitted` already carries `{agent, verdict, chain_seq}` and handles `"denied"`.
- **Mis-application to fix:** both attributable deny sites currently call `record_proxy_failed`,
  emitting `egress_proxy_failed` — documented as *"failed to initialise or write a receipt"*
  (`:115`). A budget denial is not a proxy failure. This also moves budget denials out of the Errors
  filter into Egress (`inspector.rs:79`), which is where an operator looks.
- Add `reason` to the documented `egress_denied` shape (`CONVENTIONS.md:113`) — flight.jsonl is
  unsigned and best-effort, so a data field is free. Amend its "receipt written" wording: with
  edge-triggering that is true of the *episode*, not the event.
- Reuse `EgressProxyFailed` for the resume-repair note; amend the row rather than adding a kind for a
  once-per-boot rarity.
- `agentd/tests/conventions_completeness.rs` only asserts every `EventKind::ALL` appears in
  `CONVENTIONS.md`, so no new kind ⇒ no drift-guard churn. `agentctl/tests/event_kind_strings.rs:31,38`
  already pin both strings.

### fsync → **stays, unchanged** — *because* Q2 bounds the callers, not by luck.

Residual to file, not fix: `write_receipt` holds a `std::sync::Mutex` across `flush()` + `sync_data()`
on tokio worker threads. Any future change making receipts per-request — ux.6b's B1 widening
explicitly would — must first move the write onto a dedicated writer lane, as ux.11b did with
`run_writer`. File as P3 alongside ux.13's "blocking verb on the loop thread".

---

## Non-negotiables

- **Do not add a field to `ReceiptBody`/`ActionReceipt`.** Breaks every old verifier on new lines.
- **Do not introduce or bump a persisted format version.** The audit86-P3-4 trap.
- **Do not add chain verification to the boot path.** Detection = warn, never refuse. Full
  verification stays `agentctl verify`'s job.
- **`fsync` per receipt stays.**
- **No new UI.** The Design phase was struck for this reason.
- **Receipt volume must be bounded by metered work, never by request volume.**

---

## Build order + test plan

**Step 0 — golden fixture FIRST, before touching `resume_chain`.** Generate a 3-receipt chain with the
current binary; commit `agentd/tests/fixtures/evidence/evidence.jsonl` + `egress-key.pub` (**public
key only — never the pkcs8**). Test `evidence_golden_fixture_still_verifies`: `verify_chain == 3`.
This is the permanent canonicalization + backward-compat guard (there is no
`#[serde(deny_unknown_fields)]` and the signed bytes are whatever serde emits for `ReceiptBody`'s
current field order — a future reorder would silently invalidate all history). Must be green before
and after every later step.

**Step 1 — `evidence.rs`: bounded tail resume + torn-tail repair + tail signature check + `resume_note()`.**
- `resume_tail_matches_legacy_full_scan` — **mandatory.** Copy the current 14-line counting algorithm
  verbatim into the test module as `legacy_resume`; assert byte-identical `(seq, hash)` for 0, 1, 2,
  17 receipts. This protects every `evidence.jsonl` in the field.
- `resume_reads_seq_from_last_receipt_not_line_count` — write 5, externally delete line 2, assert
  `seq == 5` (old code gives 4).
- `resume_ignores_body_and_reads_only_the_tail` — `set_len(8 MiB)` sparse prefix + one appended
  receipt; assert `seq == receipt.seq + 1`, an answer counting can never produce. Behavioral proof of
  boundedness, no slow test. (Precedent: `flight_recorder.rs:245-263`.)
- `resume_repairs_torn_tail_fragment` — truncate mid-line-3; assert file ends at the line-2 boundary,
  `resume_note()` is `Some`, then append one and `verify_chain == 3`.
- `resume_appends_newline_when_tail_complete_but_unterminated`.
- `resume_warns_but_boots_on_unsigned_tail` — hand-append a well-formed line with a garbage signature;
  assert `open()` **succeeds**, `resume_note()` is `Some`, `verify_chain` still fails. Pins C1.
- `key_persists_across_open`, `empty_chain_verifies_zero` stay green untouched.

**Step 2 — `evidence.rs`: rename rotation.** `MAX_EVIDENCE_BYTES` + private `open_with_cap` for tests
(precedent: `start_http_proxy_impl`, `egress.rs:645`).
- `rotation_renames_at_cap_and_both_segments_verify_independently` — `.1` exists, both verify, new
  file's first line has `seq == 0` and `chain_prev_hash == GENESIS_HASH`.
- `rotation_writes_to_the_new_inode_not_the_renamed_one` — catches the fd-follows-rename bug.
- `rotation_at_open_when_preexisting_file_over_cap` — sparse seed.

**Step 3 — `main.rs`: record `resume_note()` as `EgressProxyFailed` right after `:1139`.** No
signature change to `open`.

**Step 4 — `egress.rs`: `record_denied_policy` + `denied_edges` + clear-on-allow; wire `:463`, `:495`.**
- `denied_receipt_written_once_per_deny_episode` — 100 calls, same `(agent, reason)`: exactly **1**
  `"verdict":"denied"` line, **100** `egress_denied` events.
- `deny_edge_rearms_after_an_allowed_inference` — deny/allow/deny → 2 receipts.
- `proxy_unauthenticated_requests_write_no_receipt` — 50 bogus-key + 50 no-header requests; assert
  `evidence.jsonl` is **zero bytes**. The negative control that pins the Q2 ruling; without it a later
  "helpful" patch re-opens the DoS.
- Extend `proxy_budget_exhausted_returns_429` (`egress.rs:1355`) and
  `universal_spend_counted_and_globally_throttled` (`:1231`) to assert the flight event, a denied
  receipt naming the workload's `agent_id`, and that `verify_chain` still passes.

**Step 5 — `scheduler.rs`: wire the terminal admission denials. Explicitly not the shutdown branch.**
- `native_admission_denial_writes_denied_receipt` — egress proxy attached, tiny `global_token_budget`,
  `budget_reset_interval = 0`; assert `principal == agent_id`, `verdict == "denied"`,
  `action == "inference"`, `verify_chain` passes.
- `deferred_agent_writes_no_denied_receipt` — `budget_reset_interval > 0` ⇒ zero denied receipts.
  Protects ux.8′'s defer-is-not-deny invariant.
- `shutdown_denial_writes_no_receipt`.

**Step 6 — docs.** All of work items 1 + 2, plus: `CONVENTIONS.md:113`/`:115` rows;
`THREAT_MODEL.md` §8.7 key-custody paragraph; `TODOS.md` — close `audit86-P2-4` and `audit-S5`, bump
`p7.7-ar-03` from LOW (once denials are real, `HttpSource` hardcodes `egress_denied: 0` at
`source.rs:607`, so HTTP-mode operators would see a false "0 denied" — **do not fix here**, no-UI
holds), and file the two P3 residuals (fsync-on-reactor; anchoring the chain outside the boundary as
the only real fix for delete-and-restart-at-0).

**Gate:** `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings &&
cargo test --workspace` from the repo root. No Linux- or arch-gated code touched ⇒ no
`make clippy-linux` / `clippy-aarch64`. Expect ≈ +20 tests over 1 816.

**/qa (runtime, not tests):** boot a real `agentd` with an 8 MiB sparse `evidence.jsonl`; confirm flat
boot latency and that `.1` verifies. Drive an agent to legacy budget exhaustion; confirm a denied
receipt appears, `agentctl verify` still returns `chain ok`, and `watch` Detail shows non-zero
"denied" in FUSE mode (observation only — no UI change).

**Out of scope, resist:** streaming `verify_chain`'s own `read_to_string` (`:251`) is tempting and
3 lines, but it touches chain-hash computation. Rotation bounds it anyway.

---

## Struck / deferred

- **Design phase** — struck; ux.6a ships no UI. (Same rationale as ux.10 sub-part C.)
- **`audit86-P1-9`** (wrong-tier capability grants inert) — struck as scope smuggling. Its own
  rationale ("shape B would put capability verdicts in the chain") argues for settling it *separately
  and first*. Stays in `TODOS.md`.
- **Everything chain-widening** → `ux.6b`, deferred.
