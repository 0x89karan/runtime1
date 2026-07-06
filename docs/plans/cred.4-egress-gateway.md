<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260706-213110.md -->
# cred.4 — Egress gateway: spend caps + GitHub adapter

**Track:** cred. **Status:** draft (2026-07-06). **Depends on:** cred.3 + cred.3.1 (hardening
gate) + cred.3.2 (hardening completion). **Blocks:** cred.5 (credential surfacing + scope
granularity). **Source:** reassessed scope after cred.3.1/3.2 pulled forward most gateway
hardening items.

## Scope reassessment vs. original plan

The original cred.4 entry in `credential-manager.md` listed three categories:

| Category | Status |
|----------|--------|
| Additional provider adapters | Not yet shipped — this increment |
| Per-agent upstream spend cap | Not yet shipped — this increment |
| Gateway hardening (redirect, body cap, header allow-list, connect timeout) | **Already shipped in cred.3.1/3.2** |
| THREAT_MODEL.md §8 | **Already shipped in cred.3.2** |

cred.4 is therefore an **additive** increment, not a continuation of the hardening pass.

## What this increment builds

### A — Per-agent upstream spend cap

The `CredentialRegistry` tracks which agent owns each ephemeral token. Extend it to enforce a
per-agent per-provider **request-count cap** at the broker layer.

**Design (TBD at eng-review — options below):**

- `max_requests_per_agent: Option<u32>` on `ProviderConfig` (TOML: `max_requests_per_agent = 500`).
  `None` = unlimited (default, backwards-compatible).
- `request_count: Arc<AtomicU64>` per `(agent_id, provider)` pair in `CredentialRegistry`.
- Enforcement in `handle_credential_request()` before forwarding: if count ≥ cap, return HTTP 429
  with JSON `{"error": "spend_cap_exceeded", "provider": "...", "agent": "...", "limit": N}`.
- Emit `EventKind::CredentialCapExceeded { agent_id, provider, limit }` flight event.
- `agentctl watch` system view shows cap status via the management API.
- Reset policy: session (count zeroed when agent terminates) vs. persistent (count survives restart
  to `/run/memory`). **TBD at eng-review.**

**Alternative: token-budget cap** (count inference tokens routed through the broker) — more
meaningful but requires token-count side-channel from upstream responses; more complex. Defer to
cred.5 if chosen.

**Acceptance:** an agent that exceeds `max_requests_per_agent` for provider X receives a 429 from
the broker; the cap-exceeded event appears in the flight log; requests below the cap are
unaffected; a test verifies cap=1 blocks the second request.

### B — GitHub provider adapter

GitHub Personal Access Tokens (PAT) authenticate as `Authorization: Bearer <token>` or
`Authorization: token <token>`. The existing `api-key-header` adapter style covers this exactly —
no new code path is needed. This item ships:

1. A `templates/github-agent.template.toml` using `auth_style = "api-key-header"` with
   `header_name = "Authorization"` and `secret_key = "GITHUB_TOKEN"`.
2. Update `templates/code-aware.template.toml` to add an optional `Credential { provider: Custom("github") }`
   capability hint and document it in `gated_requires`.
3. Update `docker-compose.yml` `cos` service passthrough list: add `GITHUB_TOKEN` to
   `PASSENV_BLOCKLIST` (if not already there) and document the pattern in `docs/MCP_SERVERS.md`.
4. Add a `GITHUB_TOKEN`-specific test asserting the token appears as the correct header on a
   forwarded request (extends cred.3.2-ar-03 behavioral test pattern).

**Acceptance:** a config with `secret_key = "GITHUB_TOKEN"` and `header_name = "Authorization"`
forwards requests with `Authorization: Bearer <token>`; a test validates the header injection.
Note: `api-key-header` adapter already exists; this is a template + test + doc increment only —
zero new Rust code for the adapter itself.

### C — Maintenance: cred.3-ar-05 (mutex timeout)

`OAuthTokenCache::get_or_refresh()` holds `self.state.lock()` across the full token-refresh
network call (up to `CREDENTIAL_REQUEST_TIMEOUT_SECS = 60 s`). Add a shorter
`CREDENTIAL_REFRESH_TIMEOUT_SECS = 15` constant and wrap the `client.post()` call in
`tokio::time::timeout()`. The forwarding timeout stays at 60 s; only the token-endpoint call is
shortened.

**Test:** mock a token endpoint that sleeps 20 s; assert the refresh returns `Err(timeout)` within
16 s while a concurrent already-valid-token request is not delayed.

**Acceptance:** `CREDENTIAL_REFRESH_TIMEOUT_SECS` constant exists; token-endpoint POST wrapped in
`timeout()`; test for slow endpoint passes.

### D — Maintenance: cred.3.2-ar-03 (api-key-header ATTACH-BEHAVIOR test)

The existing `api-key-header` adapter test is a config roundtrip only. Add a behavioral test that
starts an `httpmock` server, sends a request through the broker with `auth_style = "api-key-header"`,
and asserts the forwarded request carries the correct `Authorization: Bearer <key>` (or
`X-API-Key: <key>`) header. Mirror the `oauth-bearer` T22 pattern.

### E — TODOS housekeeping

Strike through `cred.3-ar-04` in `TODOS.md` as fixed in cred.3.2 via IP pinning. The fix is
already documented in the `cred.3.1-adv-02` section (line ~707) but the original entry header at
line ~534 remains unstruck.

## What is explicitly NOT in cred.4

| Item | Reason | Where |
|------|--------|-------|
| cred.3-ar-01 (OAuth scope granularity) | Deferred to cred.5 per plan note | cred.5 |
| cred.3-ar-02 (refresh_failed persistence) | Depends on cred.5 credential surfacing | cred.5 |
| cred.3-ar-03 (hot credential reload) | Operator lifecycle polish = cred.5 scope | cred.5 |
| cred.3-ar-S3 (SecretRewriter) | Cross-cutting tool-output concern, different crate | Own increment |
| cred.3.2-ar-01 (handler duplication) | Non-blocking, filed as cred.3.3 cleanup | cred.3.3 |
| cred.3.2-ar-02 (canonical status line) | Non-blocking, filed as cred.3.3 cleanup | cred.3.3 |
| Stripe / Notion / other adapters | No h8.* harness server drives them yet | h8.* wave |

## Open decisions for eng-review

1. **Spend cap reset policy** — session (count zeroed on agent termination) vs. persistent (stored
   in `/run/memory`, survives restart). Session is simpler; persistent prevents cap bypass via
   quick restart. Which is the threat model here?

2. **Spend cap metric** — request count (simple, zero latency) vs. estimated token cost (more
   meaningful, requires parsing upstream response bodies). Request count is correct for cred.4;
   token cost could be a cred.5 refinement.

3. **GitHub PAT header style** — `Authorization: Bearer <token>` vs. `Authorization: token <token>`.
   GitHub's docs say both work for PATs; newer tooling prefers `Bearer`. Lock in `Bearer` and
   document it, or make `header_value_prefix` configurable?

4. **PASSENV_BLOCKLIST for GITHUB_TOKEN** — should `GITHUB_TOKEN` be added to the blocklist
   that prevents MCP servers from receiving it via inheritance? It currently isn't, meaning an MCP
   server could receive it via env inheritance before the broker migration.

## Acceptance criteria

- `max_requests_per_agent` cap enforced; 429 + `CredentialCapExceeded` event on breach.
- `CREDENTIAL_REFRESH_TIMEOUT_SECS = 15` constant; token-endpoint POST wrapped in timeout.
- `templates/github-agent.template.toml` exists and validates via `cargo test`.
- Behavioral test for `api-key-header` passes (`Authorization` header on forwarded request).
- `cred.3-ar-04` struck through in `TODOS.md`.
- `cargo test` passes; `cargo clippy -- -D warnings` clean.

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|----------|
| 1 | CEO | Run dual CEO voices | Mechanical | P6 | Always run both; Codex available | — |
| 2 | CEO | Accept premise that items C+D belong in this increment | Taste | P2/P3 | ar-05 mutex timeout is a latent DoS risk; ar-03 behavioral test closes a verification gap; both are small; boil the lake | Defer to cred.3.3 |
| 3 | CEO | Flag PASSENV_BLOCKLIST decision as USER CHALLENGE | User Challenge | — | Both models say resolve before shipping B; surfacing at gate | Leave open |
| 4 | CEO | Flag api-key-header prefix gap as USER CHALLENGE | User Challenge | — | Plan claims "zero Rust" but acceptance criterion requires `Authorization: Bearer <token>` which code cannot produce; surfacing at gate | Ship as-is |
| 5 | CEO | Accept request-count-cap naming as taste issue | Taste | P6 | Both models flag "spend cap" as misleading but request-count cap IS useful for rate limiting; surfacing at gate for rename decision | — |
| 6 | CEO Gate D1 | Proceed — fix both gaps in plan | Confirmed | — | User confirmed: (UC-1) add GITHUB_TOKEN to PASSENV_BLOCKLIST; (UC-2) add header_value_prefix to ProviderConfig for Bearer prefix. Both locked into Section B scope. | Narrow, Reprioritize, Reject |
| 7 | Eng | Counter in GatewayState (not CredEntry) | Mechanical | P1 | (agent_id, provider_name) key needs cross-token aggregation; CredEntry is keyed by token | CredEntry |
| 8 | Eng | fetch_add + rollback on cap boundary | Mechanical | P1 | Without fetch_sub the counter increments past cap semantically | No rollback |
| 9 | Eng | Timeout wraps full slow path (DNS + HTTP) | Mechanical | P1 | DNS lookup can also block; wrapping only client.post() misses it | Narrow wrap |
| 10 | Eng | Add deregister_and_get_agent() + counter cleanup | Mechanical | P1 | Without this, counters leak and "resets on agent exit" is claimed-not-built | Skip cleanup |
| 11 | Eng | CRLF validation on header_value_prefix at startup | Mechanical | P1 | reqwest panics on CRLF in header values; startup ensure! is cleaner | Skip validation |
| 12 | Eng | Add GH_TOKEN to PASSENV_BLOCKLIST (E6) | Mechanical | P1 | GitHub CLI sets GH_TOKEN; both names must be blocked | GITHUB_TOKEN only |
| 13 | Eng | Add T39/T40/T41 (missing tests) | Mechanical | P1 | Security constraint: test-per-item | Omit |
| 14 | Eng | Use Option<u64> not Option<u32> for max_requests_per_agent | Mechanical | P6 | Matches AtomicU64; no cast needed | u32 |
| 15 | Eng | Rename increment: "Egress gateway" → "Rate cap + GitHub adapter" | Taste | P6 | Low stakes; "Egress gateway" was cred.3's name — surfacing at final gate | Keep wrong name |
| 16 | Final Gate D2 | Add file-persisted counter to cred.4 scope | Confirmed | — | User chose expand: caps survive agentd restart; reset on agent deregister only; new CREDENTIAL_CAPS redb table | In-memory only |

---

## Phase 1: CEO Review

### 0A — Premise Challenge

**P1: "Request-count cap is a valid first implementation of a spend cap"**
→ CONTESTED. Both models flag this as misleading — a request count doesn't map to dollars or to the actual cost (Anthropic token spend). However, request-count caps ARE useful for preventing runaway API hammering. The naming is the issue, not the feature. Decision: surface at premise gate for rename.

**P2: "The api-key-header adapter handles GitHub PAT without new Rust code"**
→ FALSE. Code analysis: `AgentOS/agentd/src/credential/mod.rs:745` does `req_builder.header(hname, &credential)` — the raw token is attached verbatim. The plan's acceptance criterion says `Authorization: Bearer <token>`, but the adapter produces `Authorization: <token>` (no prefix). GitHub rejects this. Either a `header_value_prefix` field is needed (4 lines Rust) or the acceptance criterion is wrong. Surface at premise gate.

**P3: "Items A-E form a coherent, shippable increment"**
→ PARTIAL. A (spend cap) + E (housekeeping) are clearly correct here. B (GitHub) needs the prefix gap resolved. C+D (maintenance) are correctly included (boil the lake on ar-05/ar-03). The PASSENV_BLOCKLIST open decision on B must be resolved.

**P4: "cred.4 is the right next increment (vs. orch.1)"**
→ CONTESTED. Claude CEO subagent rates this as High severity: orch.1 closes the CoS conversational-follow-up gap (the biggest friction in the flagship workload). Codex doesn't challenge priority but both flag that B's security posture is incomplete. Surfacing at gate as taste decision.

### 0B — Existing Code Leverage

| Sub-problem | Existing code |
|-------------|---------------|
| Request counting | `CredentialRegistry` (mod.rs:95) — add second HashMap |
| Cap enforcement | `handle_credential_request()` (mod.rs:535) — add check after step 4 |
| GitHub auth | `AuthStyle::ApiKeyHeader` (mod.rs:743) — already exists; needs prefix field |
| Mutex timeout | `OAuthTokenCache::get_or_refresh()` (mod.rs:191) — wrap POST in `tokio::time::timeout()` |
| PASSENV_BLOCKLIST | `mcp.rs::PASSENV_BLOCKLIST` — add "GITHUB_TOKEN" |
| CredentialCapExceeded | `src/events.rs` — add new variant |

### 0C — Dream State

```
CURRENT STATE                   THIS PLAN                          12-MONTH IDEAL
─────────────────────────────   ──────────────────────────────     ───────────────────────────────
Broker routes creds; no caps.   Request-count cap per agent.       Policy ledger: per-agent caps
api-key-header exists but       GitHub template (with prefix        with persistent budgets,
no value prefix support.        fix). ar-05 timeout fix.           scope granularity, operator
                                TODOS housekeeping.                 remediation workflow.
→ cap bypass via restart         → first policy control at          → full credential governance
→ GitHub template broken         egress; prevents hammering         per the cred track vision
  (raw token, no Bearer)        if persistent reset chosen
```

### 0C-bis — Implementation Alternatives

```
APPROACH A: cred.4 as scoped (spend-cap + GitHub + maintenance) [PLAN]
  Summary: Ship items A-E as drafted with prefix-gap fixed and PASSENV_BLOCKLIST resolved.
  Effort:  M (CC: ~2h)
  Risk:    Low
  Pros:    - Makes progress on the credential track
           - C+D close pending ar items, preventing tech debt accumulation
  Cons:    - "Spend cap" naming misleads if persistent reset not chosen
           - Delays orch.1 (CoS follow-up gap stays open)
  Reuses:  CredentialRegistry, AuthStyle::ApiKeyHeader, handle_credential_request

APPROACH B: cred.3.3 cleanup only (fold C+D+E, skip A+B)
  Summary: Move items A and B to cred.5/h8.x; make cred.4 a pure cleanup sprint.
  Effort:  S (CC: ~30min)
  Risk:    Low
  Pros:    - Smallest possible diff; unblocks orch.1 soonest
  Cons:    - Defers the only new capability (rate cap) with no concrete date
           - ar-05 mutex DoS risk stays unaddressed
  Reuses:  All existing

APPROACH C: orch.1 first, cred.4 after [CEO recommendation]
  Summary: Slot orch.1 now, then return to cred.4 (items A-E with fixes above).
  Effort:  XL (orch.1 is large) + M (cred.4)
  Risk:    Medium (orch.1 scope uncertainty)
  Pros:    - Closes the biggest CoS friction (conversational follow-up)
           - cred.4 doesn't block orch.1; no dependency
  Cons:    - ar-05 mutex DoS risk stays unaddressed during orch.1 work
           - Longer wait before credential rate controls ship
  Reuses:  Existing MESH track infrastructure
```

**RECOMMENDATION:** Approach A with fixes (prefix-gap + PASSENV_BLOCKLIST resolved). Approach C is strategically correct if orch.1 is actively queued; but if the user is in a credential iteration cycle, A makes more sense. Surface at gate.

### CEO Dual Voices — Consensus Table

```
CEO DUAL VOICES — CONSENSUS TABLE:
═══════════════════════════════════════════════════════════════
  Dimension                           Claude  Codex  Consensus
  ──────────────────────────────────── ─────── ─────── ─────────
  1. Premises valid?                   Partial  Partial DISAGREE
  2. Right problem to solve?           Partial  Yes    DISAGREE (priority)
  3. Scope calibration correct?        Partial  Partial DISAGREE (prefix gap)
  4. Alternatives sufficiently explored? Yes   Yes    CONFIRMED
  5. Security/hardening risks covered? No      No     CONFIRMED (PASSENV gap)
  6. 6-month trajectory sound?         Partial  Partial DISAGREE (naming/reset)
═══════════════════════════════════════════════════════════════
CONFIRMED = both agree. DISAGREE = models differ (→ taste decision).
USER CHALLENGES:
  UC-1: PASSENV_BLOCKLIST for GITHUB_TOKEN — both models say resolve before shipping B.
  UC-2: api-key-header prefix gap — plan acceptance criterion contradicts implementation.
        Both models agree this must be resolved.
```

### Error & Rescue Registry

| Error | Trigger | Recovery | Event |
|-------|---------|----------|-------|
| cap exceeded | N requests ≥ max_requests_per_agent | Return 429, emit event | CredentialCapExceeded |
| mutex timeout | OAuth endpoint slow (>15s) | Return 503, release mutex | CredentialRefreshFailed |
| missing GITHUB_TOKEN | env var unset | Return 503 with hint | CredentialNotProvisioned |
| GITHUB_TOKEN in env (blocklist gap) | MCP spawn before broker | Block via PASSENV_BLOCKLIST | (prevented) |

### Failure Modes Registry

| Failure | Likelihood | Impact | Mitigation |
|---------|-----------|--------|-----------|
| Request count wraps on u64 | Negligible | None | AtomicU64 never overflows in practice |
| Session reset bypasses cap | High (if not persistent) | Rate limit ineffective | Lock to persistent reset |
| ApiKeyHeader sends raw token, GitHub 401s | CERTAIN (current code) | Template ships broken | Add header_value_prefix |
| ar-05 mutex deadlock under slow endpoint | Medium | All requests to provider blocked 60s | Fix C adds 15s timeout |

### NOT in scope (confirmed)
- cred.3-ar-01, cred.3-ar-02, cred.3-ar-03 → cred.5
- cred.3-ar-S3 (SecretRewriter) → own increment
- cred.3.2-ar-01, cred.3.2-ar-02 → cred.3.3
- Stripe/Notion → h8.* wave
- orch.1 (conversational follow-up) → MESH track
- agentctl watch cap display → stretch; not in test plan

### What already exists
- `CredentialRegistry` → extend with request counter map
- `AuthStyle::ApiKeyHeader` → extend with optional prefix
- `handle_credential_request()` → add step after step 4 (provider check)
- `PASSENV_BLOCKLIST` → add GITHUB_TOKEN
- `src/events.rs` → add CredentialCapExceeded variant

**PHASE 1 COMPLETE.** Codex: 12 concerns. Claude subagent: 8 issues.
Consensus: 2/6 confirmed, 4 disagreements. 2 USER CHALLENGES, 1 taste decision.
Passing to Phase 2 (SKIPPED — no UI scope) → Phase 3 (Eng).

---

## Phase 3: Eng Review

### Findings Table

| # | Severity | Section | Finding |
|---|----------|---------|---------|
| E1 | Critical | A | Counter must live in `GatewayState` (not `CredEntry`); keyed `(agent_id, provider_name)` with `Arc<AtomicU64>` |
| E2 | Critical | A | `fetch_add` without rollback over-counts at cap boundary; needs `fetch_sub` on reject path |
| E3 | High | C | Mutex timeout must wrap entire slow path (DNS + HTTP), not just `client.post()` |
| E4 | High | A | `deregister_token()` has no counter-cleanup call site — "claimed-not-built" risk if omitted |
| E5 | High | B | `header_value_prefix` allows CRLF injection; must validate at startup with `anyhow::ensure!` |
| E6 | High | B | UC-1 scope: must add both `"GITHUB_TOKEN"` AND `"GH_TOKEN"` to PASSENV_BLOCKLIST |
| E7 | Medium | A | `credential_cap_exceeded` event missing from CONVENTIONS.md — invariant violation |
| E8 | Medium | D | Plan missing 3 tests: unlimited-cap guard (T39), agent-isolation (T40), prefix-None regression (T41) |
| E9 | Low | A | Use `Option<u64>` (not `u32`) for `max_requests_per_agent` to match `AtomicU64` without cast |

### Implementation Decisions Locked

**A — Counter storage (E1 confirmed):**
Add `counters: tokio::sync::RwLock<HashMap<(String, String), Arc<AtomicU64>>>` to `GatewayState`.
After step 5 (provider config lookup), before step 6: `fetch_add(1)` → check → `fetch_sub(1)` on reject (E2).

**A — Counter cleanup (E4 fix):**
Add `CredentialRegistry::deregister_and_get_agent()` that removes token and returns `Some(agent_id)` iff it was the last token for that agent. `CredentialGateway::deregister_token()` calls this and then does `counters.write().retain(|(aid,_),_| aid != &agent_id)`.

**B — header_value_prefix (E5 fix):**
Validate `!pfx.contains(['\r', '\n'])` at `GatewayState::new()` startup via `anyhow::ensure!`.
In dispatch: `format!("{pfx} {credential}")` when `Some`, else raw credential.

**B — PASSENV_BLOCKLIST (E6 fix):**
Add both `"GITHUB_TOKEN"` and `"GH_TOKEN"` (GitHub CLI uses `GH_TOKEN`).

**C — Timeout scope (E3 fix):**
Wrap entire slow path (DNS + HTTP, lines ~213–292) in single `tokio::time::timeout(Duration::from_secs(15), async { ... })`.
On timeout: return `Err(...)` → 503 at gateway. No retry. `CredentialRefreshFailed` already emitted at callsite.

**D — T38 behavioral test:**
httpmock-backed: verifies `Authorization: Bearer ghp_testtoken` (not `Authorization: ghp_testtoken`).
Test FAILS without the `header_value_prefix` branch.

**Naming: rename increment** (auto-decided): "Egress gateway" is wrong (that was cred.3). Plan file title updated to "Rate cap + GitHub adapter".

**PHASE 3 COMPLETE.** 6 Critical/High + 3 Medium/Low findings. All addressable without new crates.
PHASE 3.5 (DX): in-memory cap resets on restart (docs it clearly); 429 hint is actionable; CRLF error is startup-fail with message. DX is acceptable.

---

## Test plan

| Test | Type | Scope |
|------|------|-------|
| `spend_cap_enforced_at_limit` | integration | mock upstream; cap=1; 2nd request → 429 + event |
| `spend_cap_not_enforced_below_limit` | unit | cap=5; 4 requests → all pass |
| `spend_cap_per_agent_isolated` | unit | agent A at cap does not block agent B |
| `spend_cap_session_reset` | unit | agent terminate → counter reset |
| `refresh_timeout_returns_err` | integration | httpmock slow endpoint; assert < 16 s |
| `api_key_header_attach_behavior` | integration | httpmock; assert header on forwarded req |
| `github_template_validates` | unit | `TemplateResolver::resolve("github-agent")` → Ok |
