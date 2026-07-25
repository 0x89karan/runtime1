# hardening.1 — test + safety batch (AUDIT tail, bucket 1)

Branch: `hardening.1-test-safety` · Base: `main` (post par.2, v0.104.0)
Kind: low-risk janitorial batch. Groups small, non-interacting, mostly-mechanical items so we
don't cut 3-4 trivial PRs. Behavioral surface is tiny; no scheduler/loop changes.

## Why batch (the premise to confirm)
Per the sequencing call: cheap, homogeneous, non-interacting items ride together; real behavioral
fixes and design-bearing work get their own increments. These three are pure test additions +
two defensive one-liners — none touches the scheduler, the loop, or a trust boundary in a way that
needs isolated review. Doc-drift (audit86-P3-6) is DEFERRED to `doc.1` (it's a doc audit, a different
kind of work — batching it here would make the diff heterogeneous and the review unfocused).

## Items

### 1. run.1-ar-01 — durability fixes are helper-tested, not call-site-tested (test-only)
The run.1 durability fixes assert the helper in isolation but not the wiring that invokes it, so
deleting the call site leaves every test green while the bug returns. Add call-site regression tests:
- `runs/store.rs`: a test that opens/closes > MAX_RUNS segments via `close_segment` (or a tiny
  injected cap) and asserts the table is bounded — exercising `close_segment → prune`, not `prune()`
  in isolation.
- `agent/mod.rs`: drive the `MemoryPaged` path so `short_term.extend` + `cap_short_term()` run
  together (assert `short_term_evicted` in the event / bounded length), covering the `:611` call site.
- `flight_recorder.rs`: pre-write a file larger than `MAX_FLIGHT_BYTES`, open a `FlightRecorder` on
  it, record one line, assert rotation occurred (the metadata-seed-at-open path, not the manual
  counter store the existing test uses).
- `agentctl` kind-string guard: complement the hand-maintained `AGENTCTL_KIND_MATCHES` mirror with a
  test that SCANS agentctl src for `"kind":"…"` / `kind == "…"` literals and asserts each is a real
  `EventKind::as_str()` — so a NEW match site added to agentctl and never added to the list is caught
  (completeness, not just the mirrored subset).

### 2. audit86-P3-1 — UTF-8 panic in the credential gateway (safety, "loop never panics")
`credential/mod.rs:383,395,415` byte-slice `&secrets.token_url[..token_url.len().min(64)]`. A
multi-byte char straddling byte 64 panics (`byte index N is not a char boundary`), reachable via a
malformed operator secrets file (an over-long non-ASCII `token_url`). Fix: a small char-boundary-safe
truncation helper (e.g. `truncate_chars(&str, max) -> &str` walking `char_indices`, or
`s.char_indices().take_while(|(i,_)| *i < max)`), used at all three sites. Upholds "the loop never
panics on bad input" — a config/secrets value must degrade to a `Result`/truncated string, never abort.

### 3. audit86-P3-2 — raw exception in an oauth_mcp error response (security hygiene)
`oauth_mcp.py` `request_failed` site returns `json.dumps({"error": f"request_failed: {exc}"})` —
the raw exception string, a latent leak channel (URLs, internal paths). The sibling token-exchange
site already scrubs to `type(exc).__name__`. Fix: scrub this site the same way, and audit the file
for other `f"...{exc}"` / `{e}` interpolations that should be `type(...).__name__`.

## Acceptance criteria
- Each run.1-ar-01 test FAILS if its fix's call site is deleted (negative-control-verified), passes now.
- `credential/mod.rs` truncation is char-boundary-safe; a unit test with a multi-byte `token_url`
  longer than the cap does not panic and truncates on a char boundary.
- No `oauth_mcp.py` error path returns a raw exception string; a scrub test or grep guard confirms.
- `cargo build/clippy --workspace --all-targets -D warnings` clean; `cargo test --workspace` green.
  No `cargo fmt`.

## NOT in scope
- audit86-P3-6 doc drift → `doc.1`.
- audit86-P1-8 `capability.rs normalize_path` string-identity (the AbsPathPrefix newtype) → `cap.3`
  (design-bearing; P3-1 here is only the truncation panic, NOT the path-matching semantics).
- The behavioral fixes (par.1-ar-01 error-view, budget.1-ar-01 resident signal) → own increments.

## Risk
LOW. Test additions + two defensive changes with no control-flow impact. The one thing to watch: the
credential truncation is only used in log/error strings (verify it's not load-bearing for matching),
and the oauth scrub must not change a success path.

---

## /autoplan adjustments (2026-07-25) — dual-voice Eng review (Codex + Claude subagent)

Premise CONFIRMED (both voices: the 3 items are non-interacting + one-PR-safe; deferring doc-drift
P3-6 to doc.1 is correct). No user challenge. Both recommended **adjust-scope** — ship the batch, but
correct the test recipes (several would pass vacuously) and widen the security item. Applied:

- **P3-2 widened to ALL response sites (was: `request_failed` only).** `oauth_mcp.py` has THREE raw-
  `{exc}` response leaks, not one: `request_failed:733`, **`broker_request_failed:685` (the PRIMARY
  broker-mode path the CoS uses today)**, and `Internal error:790` (tools/call catch-all, returned to
  the model). Plus a stderr `:148` WARNING. Scrub all response-returning sites to `type(exc).__name__`
  (template = the already-scrubbed `:380`/`:422`). Scrub self-tests must cover request_failed AND
  broker_request_failed. Acceptance adds `python3 docker/oauth_mcp.py --test`.
- **MemoryPaged test must pre-seed + assert AND.** Pre-seed `short_term` to `MAX_SHORT_TERM` (1000)
  before driving one hard-pressure paging, then assert **both** `short_term_evicted > 0` AND
  `short_term.len() == MAX_SHORT_TERM`. Without pre-seeding the test passes vacuously (evicted==0).
- **close_segment→prune test drives by AGE, not count.** `prune` at `store.rs:309` is unconditional
  but uses hardcoded consts (no cap injection seam), so count-driving means 5001 segments. Instead:
  close a record at old `ts`, then `close_segment` another at `ts + 91d` → the second close's prune
  ages out the first. Two segments, cheap, hits the real call site.
- **flight_recorder seed test uses a SPARSE file.** `File::set_len(MAX_FLIGHT_BYTES + 1)` (instant,
  ~0 bytes on disk; `metadata().len()` reports full length) — NOT a real 100 MB write. Assert on
  `recorder.size` post-record (rotated ⇒ small), don't read 100 MB back.
- **Negative control reframed: "neutralize the fix," not "delete the call site."** Deleting the call
  sites is a compile error (unbound var), so the control is: no-op the drain / zero the seed / no-op
  prune ⇒ the test goes red.
- **agentctl source-scanner → DROPPED from this increment** (taste, see gate). It's the one non-
  mechanical item: a naive `"kind":"…"` scan false-positives on the documented `KNOWN_NONCANONICAL`
  dead strings + `#[cfg(test)]` fixtures, and false-negatives on non-literal matches — it duplicates
  the shipped self-cleaning mirror with a brittler mechanism. Keep par.1's hand-list + its two guard
  tests (already shipped). If revisited, hard-narrow (production-only, honor KNOWN_NONCANONICAL).
- **P3-1 truncation idiom: `s.chars().take(64).collect::<String>()`** — matches the existing
  `MAX_NARRATIVE_CHARS` idiom at `store.rs:434`, stable std (no `floor_char_boundary` nightly dep),
  no new helper/clippy surface. (64 chars ≠ 64 bytes now; irrelevant for a log preview.) All three
  sites are error-string-only, verified non-load-bearing (matching uses `starts_with`/`extract_host`).
