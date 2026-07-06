<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260706-173131.md -->
# cred.3.2 — Credential-manager hardening completion

**Track:** cred. **Status:** approved (2026-07-06, autoplan Phase 4 gate passed). **Depends on:** cred.3.1 (PR #85, v0.61.0). **Blocks:** cred.4 /
orch.1 (the hardening gate stays closed until this lands). **Source:** a read-only verification of the
cred.3.1 branch against `docs/plans/cred.3.1-hardening.md` (2026-07-06).

## Why this exists

cred.3.1 closed most of the hardening gate, but a branch-level verification found **three gate items
were reduced** (partial), **two defenses were de-claimed rather than built**, and the **doc
reconciliation was left half-done**. cred.3.1 was honest about this (it did not over-claim), so this
is completion work, not a redo. cred.3 is not "robust" — and cred.4/orch.1 stay blocked — until every
item below is closed.

## Verified state after cred.3.1 (do NOT redo these)

- **ar-08** — header allow-list (`PASSTHROUGH_HEADERS` replacing the deny-list). ✅
- **ar-06** — `state_path` read-back (`OAuthTokenCache::load_from_disk` + expired-token guard). ✅
- **ar-07 (fail-open half)** — deny-by-default for empty `allowed_providers`. ✅
- **S1** — Ed25519 signing-key FsRead startup invariant. ✅
- **ar-04 (startup half)** — `is_ssrf_blocked()` + DNS resolution of `upstream_base` at startup, with
  IPv4-mapped-IPv6 / `fc00::/7` / IPv6-literal / userinfo coverage. ✅ (partial — see below)
- **S2 / S3** — the false claims were removed and documented (§8.7 content-audit-not-implemented;
  SecretRewriter de-claimed). ✅ (honest, but the defenses do not exist — see below)
- Tests T18–T24. ✅

## The completion scope

### Group A — the real anti-drift fix + per-request SSRF (anchor)

> **Updated by cred.3.2 autoplan (CEO phase, D1-A):** ar-10 implementation is shared
> guard *functions* (not a monolithic handler) in `loopback_proxy.rs`. Group A also
> now includes ar-04c (OAuth token_url SSRF) and the `bytes()` OOM fix in the credential
> handler. Group C (S2/S3) must be decided before any Group A code is written.

**ar-10 (real) — extract a shared forwarding *handler*, not just the client builder.**
cred.3.1 added `loopback_proxy.rs` but it only shares the reqwest **client** (`build_loopback_client`,
redirect policy + timeouts). `egress.rs::handle_proxy_request` and
`credential::handle_credential_request` **still both exist** and duplicate the *request-level* guards
(SSRF check, header allow-list, body cap, path handling). The drift surface ar-10 targeted is still
open. Extract those request-level guards into the shared core with a pluggable auth-injector so both
proxies enforce one guard set.
- *Accept:* there is exactly one place that applies redirect/SSRF/header/body-cap guards; both proxies
  route through it; a guard added there covers both. Test the shared core directly.
- *Where:* `agentd/src/loopback_proxy.rs`, `agentd/src/egress.rs`, `agentd/src/credential/mod.rs`.

**ar-04 (per-request) — close DNS rebinding.**
The SSRF check runs once in `CredentialGateway::start()`; a host that resolves clean at startup but is
later rebound to a private IP bypasses it. Re-validate per request, **or** pin the resolved IP at
startup and connect to that IP (preserving SNI/Host). Do it inside the ar-10 shared handler.
- *Accept:* a host that passes at startup but rebinds to a private/loopback/link-local IP is blocked
  at request time; test covers the rebinding path.
- *Where:* `agentd/src/credential/mod.rs` (`is_ssrf_blocked` call site) + the shared handler.

### Group B — per-agent token scoping

**ar-07 (per-agent) — scope the gateway token to the owning agent, not the MCP server name.**
`register_token(token, server.name, allowed)` keys the token to `server.name`, so audit attribution
records the server rather than the owning agent, and an MCP server shared across agents blurs scope.
Scope the token (and its `CredentialAccessed`/audit attribution) to the owning agent/session.
- *Accept:* agent A's token cannot use a provider granted only to agent B; flight events attribute the
  call to the agent, not the server. Test both.
- *Where:* `agentd/src/main.rs` (token registration) + `agentd/src/credential/mod.rs` (registry).

### Group C — the two de-claimed defenses (DECISIONS for /plan-eng-review)

These are not code chores — they are build-or-ratify calls. Take them to `/plan-eng-review`; do not
silently keep de-claiming.

**S2 — egress content audit.** Currently de-claimed (§8.7). Either implement it (hash the forwarded
request/response body into the signed receipt so `content_audited` can be true) or ratify the
de-claim as permanent. Weigh against the "observability is half the product" thesis.

**S3 — boundary secret-rewriting.** Currently de-claimed; tool outputs reach the model **unscrubbed**.
Either implement rewriting at the `ToolRegistry::invoke` choke point (`tools/mod.rs`) or ratify the
de-claim as an accepted single-tenant limitation. This is a real missing defense, not just a doc line.

### Group D — finish the doc reconciliation

cred.3.1 added THREAT_MODEL §8.6/§8.7 but left the **header at "v0.25.0"** and RUNBOOK at **"v0.59"** —
the exact partial-update trap the audit flagged (D5 / sec.1). Finish it:
- RUNBOOK.md — full pass (this is the `sec.1` debt): correct the version header, delete future-tense on
  shipped phases, add the credential-broker section.
- THREAT_MODEL.md — bump the header to reflect v0.61+ reality (the doc already carries §8).
- Add a **canonical "vX.Y.Z — shipped / unshipped" status line** (CLAUDE.md top-of-status) so this
  drift cannot recur (audit V1).

### Group E — test coverage the gate implied

**api-key-header adapter test.** cred.3.1 left an unused test fixture `provider_cfg_api_key_header()`
(`credential/mod.rs:887`) — which broke CI as dead code *and* signals the `api-key-header` auth path
was never tested. Wire the fixture into a real test of the api-key-header adapter (do not just delete
it or `#[allow(dead_code)]` it — that discards a dropped test).
- Note: the **immediate CI-green fix on PR #85 is the build session's** (to merge cred.3.1); cred.3.2
  is where the adapter gets a proper test.
- Also add: the ar-04 rebinding test, the ar-07 per-agent scoping test, and a direct test of the ar-10
  shared guard core.

## Meta-instruction (non-negotiable)

Same rule cred.3.1 was given — and the ar-10 reduction + S2/S3 de-claims are exactly why it matters:
for every code item, **fix + a test that fails without it + adversarial verification of the real
failure path** — not "applied." No partial doc updates that leave a stale version header. If a defense
isn't built, the docs must say so plainly.

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| 1 | CEO | ar-10: shared guard functions, not monolithic handler | Auto-decided | P1 Completeness + P5 Pragmatic | Both voices confirm blast radius high for monolithic; guard FNs close drift without coupling egress/credential event paths | Monolithic shared handler with auth-injector |
| 2 | CEO | Group C (S2/S3) must be decided before Group A coding | Auto-decided | P1 Completeness | S2 build would change ar-10 shared handler interface; sequencing error would cause re-do | Plan's listed order (A first, C third) |
| 3 | CEO | T25 already wired in PR #85 — plan must note, not re-implement | Auto-decided | P3 DRY | `provider_cfg_api_key_header()` is used at credential/mod.rs:1504 | Delete the fixture or allow(dead_code) |
| 4 | CEO | `bytes()` OOM in credential handler belongs in Group A scope | Auto-decided | P1 Completeness | credential/mod.rs:703 buffers full upstream response before cap check; egress.rs:419 does per-chunk; real memory safety gap | Defer as separate bug fix |
| 5 | CEO | `provider_cfg_oauth()` dead_code — integration test needed in Group E | Auto-decided | P1 Completeness | Same dead_code pattern as api-key-header fixture; OAuth bearer auth path untested through handle_credential_request | Leave as dead_code |
| 6 | CEO | Token deregistration lifecycle test — add in Group E | Auto-decided | P1 Completeness | Capability escalation vector on MCP server restart; test is low-cost and clearly scoped | Defer |
| 7 | CEO | OAuth token_url SSRF added to Group A scope | USER CONFIRMED | User selection D1-A | User confirmed: Codex finding is real; token refresh path bypasses upstream_base SSRF gate; fix alongside ar-04 in shared guard | Track as P1 TODOS only |
| 8 | CEO | Group C (S2/S3) sequencing before Group A coding | Auto-decided | P1 Completeness | Both voices: S2 build would change ar-10 interface; must decide before implementing | Implicit order in plan doc |
| 9 | ENG | S2: RATIFY de-claim for credential gateway | Auto-decided | P4 Pragmatic | EvidenceWriter wiring adds complexity for weak gain; FlightRecorder events sufficient | Build body hash in credential handler |
| 10 | ENG | S3: RATIFY de-claim in cred.3.2 | Auto-decided | P4 Pragmatic | Per-type regex design required; false-positive risk high; track as P2 TODOS | Build SecretRewriter in cred.3.2 |
| 11 | ENG | bytes() OOM fix: use bytes_stream() per-chunk loop | Auto-decided | P1 Correctness | http_body_util::Limited is wrong type for reqwest::Response; streaming matches egress.rs:419 | Use http_body_util::Limited for response |
| 12 | ENG | ar-07 fix: pass agent ID from agent config scope | Auto-decided | P1 Correctness | agent config IS in scope at main.rs:412 loop; one-line fix not architectural change | Per-(agent×server) re-architecture |
| 13 | ENG | ar-10: move is_ssrf_blocked to loopback_proxy.rs | Auto-decided | P3 Organisation | Canonical location for future forwarders; zero new runtime sharing with egress by design | Leave in credential/mod.rs |
| 14 | ENG | ar-04: IP pinning at startup, not per-request re-resolve | USER CONFIRMED | User selection D2 | Per-request re-resolve is defense theater; IP pinning is the only real control | Per-request DNS re-resolve |
| 15 | ENG | Query string: sanitize in cred.3.2 | USER CONFIRMED | User selection D3 | Real injection risk for ApiKeyQuery providers; fix is low-cost | Defer to TODOS P2 |
| 16 | GATE | Full scope approved — start build session | USER CONFIRMED | User selection D4 | All 15 decisions locked; no scope changes; build proceeds | Narrow scope / add scope / hold |

## Done =

ar-10 unifies the request-level guards (one guard set, both proxies) · ar-04 blocks rebinding at
request time · ar-07 scopes tokens to the owning agent · S2 and S3 are each explicitly built or
ratified via eng-review · RUNBOOK + THREAT_MODEL headers reflect reality + a canonical status line
exists · api-key-header adapter is tested · every code item has a failing-without-it test + adversarial
verification · `/review` + `/qa` clean. Only then is cred.3 "robust" and cred.4 / orch.1 unblocked.

---

<!-- GSTACK REVIEW REPORT — Phase 3 (Eng Review) 2026-07-06 -->
## GSTACK ENG REVIEW REPORT

**Phase:** 3 — Engineering Review  
**Voices:** Claude subagent (independent, fresh context) + Codex (code search + analysis)  
**Test plan artifact:** `~/.gstack/projects/0x89karan-runtime1/eng-plans/cred32-test-plan-20260706.md`

---

### ENG DUAL VOICES — CONSENSUS TABLE

```
═══════════════════════════════════════════════════════════════════════
  Dimension                                    Claude   Codex  Consensus
  ─────────────────────────────────────────── ──────── ─────── ─────────
  1. ar-07: needs per-agent-id at registration HIGH    no flag FLAGGED
  2. ar-04: per-request re-resolve = theater   HIGH    no flag FLAGGED
  3. ar-10: is_ssrf_blocked move phantom share MEDIUM  implicit DISAGREE
  4. bytes() OOM: need bytes_stream() not Lim  LOW     no flag FLAGGED
  5. S2: EvidenceWriter wiring needed in cred  HIGH    no flag FLAGGED
  6. Query string passthrough to upstream      MEDIUM  no flag FLAGGED
═══════════════════════════════════════════════════════════════════════
5 single-voice (Claude), 1 disagreement (ar-10 scope) → surfaced at gate.
```

---

### Section 1 — Architecture

**Dependency graph of cred.3.2 scope:**

```
agentd/src/main.rs:546
  gw.register_token(token, server.name, allowed)  ← ar-07: WRONG PARAM
  │
  ↓ (token issued with agent's ID after fix)
  CredentialRegistry (credential/mod.rs:100)
  │  stores: token → (agent_id, allowed_providers)
  │
  └─→ handle_credential_request (credential/mod.rs:454)
        step 2: registry lookup → returns agent_id ← at-07: attribution fix
        step 8: OauthBearer → get_or_refresh()
                  token_url SSRF check ← ar-04c: MISSING
                  POST to token_url   ← ar-04c: gap
              ApiKeyHeader/Query ← T25 covers header
        step 9: normalize_path_segment ← T15b/c ✅
        step 10: PASSTHROUGH_HEADERS ← T9/10 ✅
        step 13: upstream send via state.client
        step 14: upstream_resp.bytes().await ← OOM gap: buffers full body
                 ↕ should be: bytes_stream() per-chunk cap like egress.rs:419

agentd/src/loopback_proxy.rs
  build_loopback_client() ← SHARED ✅
  is_ssrf_blocked() ← NOT here yet (in credential/mod.rs)
                       egress.rs does NOT use SSRF (fixed upstream)
                       moving to loopback_proxy.rs = zero new sharing with egress

agentd/src/egress.rs
  handle_proxy_request()
    upstream = api.anthropic.com (hardcoded, no SSRF needed)
    response cap: per-chunk streaming ✅ (correct pattern)

agentd/src/evidence.rs
  EvidenceWriter::record_allowed() ← called from egress.rs
  NOT called from credential/mod.rs ← S2 wiring gap
```

**Architecture verdict:** The two proxies have fundamentally different security models — egress has a fixed upstream (no SSRF needed), credential has operator-configured upstreams (SSRF needed). ar-10 "shared guard functions" means: move `is_ssrf_blocked` to `loopback_proxy.rs` as the canonical location so future loopback forwarders inherit it. This is an organisational improvement, not a runtime sharing win (egress already doesn't need it). The plan's framing of "one guard set, both proxies" is only partially correct — it's really "canonical location + forward-compat for future forwarders."

---

### Section 2 — Code Quality

| Item | File:Line | Issue | Decision |
|------|-----------|-------|---------|
| bytes() OOM | credential/mod.rs:703 | Buffers full upstream response before cap; egress does per-chunk | AUTO-FIX: use bytes_stream() loop matching egress.rs:419-436 |
| server.name as agent_id | main.rs:546 | Wrong identifier; agent config IS in scope | AUTO-FIX: use agent's ID from agent config |
| provider_cfg_oauth() dead code | credential/mod.rs:875 | #[allow(dead_code)] without live test | AUTO-FIX: add OAuth bearer integration test T32 |
| Response content-type hardcoded | credential/mod.rs:737 | Ignores upstream content-type | DEFER: doesn't affect security, MCP servers expect JSON |
| Query string verbatim | credential/mod.rs:1090 | Appended without sanitization | GATE Q: raise at Phase 4 |

---

### Section 3 — Test Review (NEVER SKIP)

Test plan artifact written to: `~/.gstack/projects/0x89karan-runtime1/eng-plans/cred32-test-plan-20260706.md`

**Existing coverage:** T1–T25 (35 assertions). Strong coverage of:
- PASSTHROUGH_HEADERS correctness (T9, T10, T10b)
- is_ssrf_blocked comprehensive IP classes (T18–T20)
- OAuth state disk round-trip (T21)
- ar-07 deny-fast-path via live gateway (T22)
- S2 de-claim integrity (T23)
- ar-10 client-builder sharing (T24)
- api-key-header via live gateway (T25)

**Critical gaps (P0/P1 — block ship):**

| Test ID | Gap | Fails without fix |
|---------|-----|-------------------|
| T26 | ar-04: per-request rebind blocks private IP | YES |
| T27 | ar-04c: OAuth token_url SSRF blocked | YES |
| T28 | bytes() streaming cap enforced | YES |
| T29 | is_ssrf_blocked callable from loopback_proxy | YES |
| T30 | ar-07: CredentialAccessed event has correct agent_id | YES |
| T31 | ar-07: cross-agent token use denied | YES |
| T32 | OAuth bearer path via live gateway | YES |
| T33 | Deregistered token returns 401 live | NO (regression-guard) |

**Target:** 1139 + ~10 = ~1149 workspace tests.

---

### Section 4 — Performance

| Issue | Location | Impact | Recommendation |
|-------|----------|--------|----------------|
| bytes() OOM | credential/mod.rs:703 | Up to MAX_UPSTREAM_RESPONSE_BYTES (currently checking bytes.len() AFTER full buffer) | Fix: streaming per-chunk cap |
| DNS lookup at startup | CredentialGateway::start | Blocks startup per provider; warning not error on failure | Acceptable |
| N+1 refresh | OAuthTokenCache per provider | Mutex held during entire token refresh POST; acceptable for single-tenant | DEFER |
| No N+1 in registry | CredentialRegistry | O(1) HashMap lookup | OK |

---

### Section 5 — NOT in Scope (cred.3.2)

- Universal-tier credential plumbing (cred.4/5)
- Per-agent MCP server spawning architecture (separate increment)
- SecretRewriter per-type regex (cred.3.2 ratifies de-claim; P2 TODOS)
- Body hash in signed receipt for credential gateway (S2 ratified; needs EvidenceWriter wiring which is a larger change)
- Response content-type reflection from upstream
- Per-call budget enforcement (cred.4)
- Full RUNBOOK rewrite (Group D: pass + header update, not full rewrite)

---

### Section 6 — What Already Exists (use, don't recreate)

| Item | File | Available for |
|------|------|--------------|
| `is_ssrf_blocked()` | credential/mod.rs:754 | Move to loopback_proxy.rs; call from both |
| `build_loopback_client()` | loopback_proxy.rs:43 | Already shared |
| `bytes_stream()` per-chunk pattern | egress.rs:419-436 | Copy pattern for credential handler step 14 |
| `provider_cfg_api_key_header()` | credential/mod.rs:887 | Used in T25; still useful for T31+ |
| `CredentialRegistry` agent_id field | credential/mod.rs:100 | Correct type; fix caller only |
| `T22` live gateway harness | credential/mod.rs:1425 | Reuse harness pattern for T26–T35 |

---

### Section 7 — Failure Modes Registry

| Scenario | Current behavior | Correct behavior | Blocked by |
|----------|-----------------|-----------------|------------|
| DNS rebinding: upstream_base valid at start, rebinds to 169.254.x.x | Request forwarded → IMDS data returned | 502 blocked at request time | ar-04 per-request fix |
| Malicious secrets file: token_url = https://169.254.169.254/ | Token refresh POST sent to IMDS | Blocked before POST | ar-04c fix |
| Large upstream response (> MAX_UPSTREAM_RESPONSE_BYTES) | Full body buffered in RAM | 502 returned after first chunk exceeds cap | bytes() OOM fix |
| MCP server crash: token stays live | Token usable until agentd restart | Deregistered on server exit | Architecture gap (P2) |
| Wrong agent_id in CredentialAccessed | server.name appears in flight log | Owning agent's ID appears | ar-07 fix |
| Cross-agent token theft | Token from agent-A works for agent-B providers | Denied (scoped per-agent) | ar-07 fix |

**Critical gaps flagged:** ar-04 rebinding (IMDS theft), ar-04c token_url (SSRF via secrets), bytes() OOM (memory exhaustion).

---

### Section 8 — S2/S3 Eng Review Recommendation

**S2 — Egress content audit (body hash in signed receipt)**

RECOMMENDATION: **RATIFY DE-CLAIM** (do not build in cred.3.2)

Rationale:
1. The credential gateway uses `FlightRecorder`, not `EvidenceWriter`. Wiring `EvidenceWriter` into `GatewayState` requires threading the signer and evidence file path through the gateway constructor — a non-trivial change that risks introducing bugs into the startup path.
2. The defense is weak: the hash is computed by the broker process itself. A memory-corruption exploit that can forge request content can also forge the hash before signing. This is not stronger than the existing `CredentialEgressBrokered` flight event.
3. The `EgressProxy` (egress.rs, Anthropic inference path) has `EvidenceWriter` wiring already. The credential gateway's upstreams are operator-configured OAuth/API endpoints — the trust model is fundamentally different.
4. `T23` already verifies that `content_audited: true` is not falsely claimed.

**Action:** Update THREAT_MODEL §8.7 to explicitly state "credential gateway does not use EvidenceWriter; only FlightRecorder events are emitted" and mark S2 as permanently de-claimed for the credential gateway path. The egress proxy path is out of scope (already has evidence).

**S3 — Boundary secret-rewriting (SecretRewriter at tools/mod.rs:182)**

RECOMMENDATION: **RATIFY DE-CLAIM** (do not build in cred.3.2, P2 track)

Rationale:
1. A regex broad enough to catch Google OAuth tokens (`ya29.[A-Za-z0-9_-]+`), Anthropic keys (`sk-ant-[A-Za-z0-9_-]{40,}`), and Brave keys (`BSA[A-Za-z0-9]{32,}`) will produce false positives on base64-encoded file content, SHA hashes in git output, and JWT payloads from legitimate tool results.
2. The right design is per-type patterns with `(?:^|\s|")` prefix anchors and coverage tests for false positives — this is a separate 2-day increment.
3. Single-tenant single-trust model means tool output reaching the model from a trusted tool is not a cross-trust-boundary leak. The risk is model prompt injection, not exfiltration.

**Action:** Update TODOS.md with a P2 item for SecretRewriter with explicit design requirements (per-type patterns, false-positive test suite). Keep §8.7 note that tool outputs are unscrubbed.

---

### Section 9 — Decision Updates (Eng Phase additions to Decision Audit Trail)

| # | Decision | Classification | Rationale |
|---|----------|----------------|-----------|
| 9 | S2: RATIFY de-claim for credential gateway | Auto-decided | EvidenceWriter wiring adds complexity with weak security gain; FlightRecorder events sufficient |
| 10 | S3: RATIFY de-claim in cred.3.2 | Auto-decided | Per-type regex design required; false-positive risk high; P2 TODOS track |
| 11 | bytes() OOM fix: use bytes_stream() per-chunk loop | Auto-decided | http_body_util::Limited is wrong type for reqwest::Response; streaming matches egress.rs:419 |
| 12 | ar-07 fix: pass agent ID from agent config scope | Auto-decided | agent config IS in scope at main.rs:412 loop; one-line fix not architectural change |
| 13 | ar-10: move is_ssrf_blocked to loopback_proxy.rs | Auto-decided | Canonical location for future forwarders; zero new runtime sharing with egress (by design) |
| 14 | ar-04: IP pinning (store resolved IP at startup) | GATE QUESTION D2 | Per-request re-resolve is defense theater (OS resolver cache + reqwest own resolution); IP pinning required |
| 15 | Query string verbatim passthrough | GATE QUESTION D3 | Real injection risk for ApiKeyQuery providers; needs decision on sanitization scope |

---

### Phase 3 Completion Summary

**Codex:** 1 critical finding (ar-04c OAuth token_url SSRF — new scope item, USER CONFIRMED D1-A); primary activity was code reading.  
**Claude subagent:** 10 findings (2 HIGH architectural corrections, 3 MEDIUM new gaps, 2 HIGH S2/S3 wiring analysis, 3 LOW clarifications).  
**Consensus:** 5 flagged (single-voice Claude), 1 disagreement (ar-10 scope), 2 auto-decided (bytes() OOM fix approach, S2/S3 ratified), 2 gate questions (ar-04 implementation approach, query string sanitization).

**DX Review (Phase 3.5):** SKIP — cred.3.2 has no new CLI commands, no new template entries, no user-facing features. Pure security hardening + doc update. DX score unchanged.

**Gate questions for Phase 4:**
- D2: ar-04 implementation — IP pinning at startup vs. per-request re-resolve  
- D3: query string passthrough — sanitize in cred.3.2 vs. defer to TODOS

**Phase 3 score: 4.5/6** (2 auto-decided in eng, 2 at gate, 2 confirmed from CEO phase)

<!-- END GSTACK REVIEW REPORT -->
