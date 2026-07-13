<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260713-160705.md -->
# cred.7 — Credential resilience (general-purpose refresh/credential-failure recovery)

**Increment:** cred.7 (Phase 10 — Credential manager). Supersedes the `auth-resilience` seed.
**Renamed 2026-07-11:** was `cred.6`; the CEO review split the broker-mode migration out as its own
prerequisite increment (**cred.6**, `docs/plans/cred.6-broker-migration.md`). This plan is the
resilience framework that rides *on top of* broker mode.
**Status:** Planned — hardened by `/autoplan` (2026-07-13: CEO + Eng dual voices applied). Not started.
**Depends on:** **cred.6 (broker migration)** — the CoS must be broker-mode first; plus cred.3+ broker
(`agentd/src/credential/mod.rs`), dx.4 device flow (`agentctl auth google --device`), p7.4 approvals.
**Premise correction (2026-07-11):** an earlier draft attributed the live "not authenticated" failure
to the FsRead sandbox cap (`4a0951a4`) / a stale image. That diagnosis was **wrong** — the FsRead cap
was a no-op on Docker Desktop (Landlock absent), and the real cause was the `oauth_mcp.py` `check_auth`
state-machine bug, fixed in **v0.73.2 (#102)**. Auth works today; this increment is not about that bug.

## Problem (corrected — the honest gap)

When a broker credential's refresh/validation **terminally** fails (revoked, expired, key rotated, deleted client), the agent gets an **opaque error**, keeps burning cron cycles, the operator gets **no signal**, and **nothing resumes** after the credential is fixed. This is provider-agnostic across the broker's OAuth-refresh and api-key adapters.

Note the seed overstated the failure as an automatic "loopback dead-end." There is no automatic fallback — on refresh failure the gateway/sidecar just returns an error and stops. The real gap is **no terminal detection, no operator surfacing, no resume signal**.

## Decisions (locked — /autoplan CEO 2026-07-13)

- **Layer = the Rust broker gateway** (`credential/mod.rs::get_or_refresh`), provider-agnostic. NOT the Python `oauth_mcp.py` (that would be Gmail-specific, and becomes dead code once the CoS moves to broker mode — which cred.4b built the sidecars for).
- **General-purpose framework + BOTH credential classes wired in v1** (OAuth-refresh AND api-key). Google is instance #1.
- **D1 = host-driven recovery** (reuse dx.4's device flow). **D1b + D3 cut** — no in-container device flow, no container-writable token store.
- **Push (ux.4) cut** — it does not exist. MVP notification = durable flight event + credential health surface.
- **Google Production-publishing is docs/CLI guidance only** — cannot be a required code gate; 7-day behavior for `gmail.readonly` unverified app must be confirmed by operator, not assumed.
- **Proactive OAuth token refresh IN SCOPE** (CEO D1 user decision) — background tokio task per OAuth provider in `GatewayState`; refreshes access tokens 5 min before `expires_at`; eliminates Transient failures for normal operation entirely.
- **Dedup/idempotency CUT from v1** — the scenario (N concurrent agents hitting invalid_grant simultaneously) is not yet observed in production; if two attention events fire, it is a benign duplicate; consolidate in cred.8 after observing in production.
- **p7.4 approval NOT the durability anchor** (CEO flagged wrong data model; Eng confirmed architectural impossibility — gateway has no path to create scheduler approvals). Durability = `ProviderHealth` in `SchedulerCheckpoint` + surfaced via existing `CredentialSnapshot` in cred.5's `GET /api/v1/credentials` and `agentctl watch [c]`.
- **Precondition = cred.6 (broker migration), now its own increment.**

## The general framework (in the gateway)

### §1 — Classification

Parse the token-endpoint / API JSON `error` field (never HTTP status class alone) into a **3-way enum** (simplified from 5-way per CEO review — `ApiKeyInvalid` doesn't fit the refresh path; `Unknown → Transient` is dangerous for terminal errors):

```rust
enum FailureClass {
    Retryable,                         // 5xx, 429/rate_limit_exceeded, timeout, DNS
    AttentionRequired { recovery_kind: RecoveryKind },  // terminal
}
enum RecoveryKind {
    Reauth,      // invalid_grant → re-run device flow
    ConfigFix,   // invalid_client / unauthorized_client / invalid_scope → fix OAuth app config
    SecretReplace, // api-key 401/403 → supply new key
}
```

Critical distinction: **401 on the API call** (stale access token → silent refresh + retry once, not attention) vs **failure on the refresh call** (bad refresh token → AttentionRequired). Conflating them creates false reauth prompts.

`get_or_refresh()` signature changes from `Result<String, String>` to `Result<String, CredentialError>` where:
```rust
struct CredentialError { class: FailureClass, message: String }
```
All call sites updated: `handle_credential_request()` (line ~964), timeout wrapper maps `Elapsed` → `CredentialError { class: Retryable, ... }`, all test call sites (T6, T7, T27, T32, G1, G2).

### §1.5 — Proactive token refresh

A background `tokio::task` per OAuth provider, started in `GatewayState::new()`. Per provider:
- Sleeps until `expires_at - 5min` (computed from the `OAuthCacheInner` after each successful refresh)
- Uses `try_lock()` on the cache mutex — if foreground holds it, skip (foreground will refresh anyway)
- If `try_lock()` succeeds: check `expires_at` and call `refresh_token_request()` directly (skips slow path)
- Emits `CredentialRefreshFailed` if the proactive refresh fails (applies redaction — see §8)
- On panic: the outer `tokio::spawn` restart loop catches the `JoinError` and re-arms the task with a 60s backoff
- On SIGTERM: a `CancellationToken` passed at construction cancels all proactive tasks before the gateway shuts down
- Only OAuth providers have proactive refresh (api-key providers have no access token expiry)

### §2 — Per-provider state machine

**Above** (not inside) `OAuthTokenCache`:

```rust
enum ProviderHealth {
    Healthy,
    TransientRetry { until: Instant },
    AttentionRequired { reason: String, recovery_kind: RecoveryKind, detected_at: SystemTime },
}
// Checkpointed as (stored without Instant — TransientRetry is not checkpointed):
enum ProviderHealthCheckpoint {
    Healthy,
    AttentionRequired { reason: String, recovery_kind_str: String, detected_at_secs: u64 },
}
```

Stored on `GatewayState`:
```rust
provider_states: Arc<std::sync::RwLock<HashMap<String, ProviderHealth>>>
```

In `handle_credential_request()`:
1. **Read** `provider_states[provider]`:
   - `AttentionRequired` → fast-fail immediately with attention sentinel (no token cache touch)
   - `TransientRetry { until }` and `Instant::now() < until` → fast-fail with retry-after
   - `Healthy` or expired `TransientRetry` → proceed to step 2
2. Call `get_or_refresh()` → on `CredentialError`:
   - `Retryable` → **write-lock**: transition to `TransientRetry { until: now + backoff }`, no event if already `TransientRetry` with unexpired `until`
   - `AttentionRequired { recovery_kind }` → **write-lock**: transition; only the first caller to observe the transition (reads `Healthy` → writes `AttentionRequired`) emits `CredentialAttentionRequired` + updates checkpoint

### §3 — Durability

`ProviderHealth` is checkpointed in `SchedulerCheckpoint` (added field: `credential_health: HashMap<String, ProviderHealthCheckpoint>`, `#[serde(default)]`, no FORMAT_VERSION bump needed — field is additive). `TransientRetry` is NOT checkpointed (it is transient by definition; restores as `Healthy`). `AttentionRequired` IS checkpointed.

On startup: `main.rs` reads checkpoint → extracts `credential_health` → passes to `CredentialGateway::start(credential_health: HashMap<...>)` → pre-populates `provider_states`. **This prevents duplicate attention events on restart** (Finding 6 / Eng review).

### §4 — Surface

One `CredentialAttentionRequired` flight event per provider per transition (not per call). Extended `CredentialSnapshot` (already in `surfaces::CredentialSnapshot` from cred.5):
```rust
// Add to CredentialSnapshot:
pub attention_reason: Option<String>,
pub recovery_kind: Option<String>,
pub attention_since: Option<u64>,
```
Exposed via `GET /api/v1/credentials` and `agentctl watch [c]` (cred.5 surface — no new pane needed). `CredentialRecovered` event when attention clears.

No p7.4 approval, no GatewayCommand channel, no scheduler wiring for credential attention.

### §5 — Resume (fix for cred.3.1-adv-01, consolidated here)

On `AttentionRequired` transition: set `inner.token = None` and `inner.expires_at = 0` in the cache. **No mtime tracking** — mtime has sub-second races (same-second overwrites, 9p virtfs propagation lag, NTP skew). Instead:
- Provider state machine fast-fails (§2) before touching cache — no retry storm
- When the operator updates `google.json` (or the api-key secret), they must re-trigger via management API: `POST /api/v1/credentials/<provider>/reset-attention` (new endpoint, fails-closed if unknown provider ID) OR the gateway picks it up on next use after clearing `ProviderHealth` from `AttentionRequired → Healthy` manually

**Actually simpler**: the gateway detects recovery by attempting `get_or_refresh()` when the operator calls the management API to clear attention. The management API endpoint `POST /api/v1/credentials/<provider>/reset-attention`:
- Write-locks `provider_states[provider]` → sets to `Healthy`
- Clears checkpoint field
- Emits `CredentialRecovered`
- Returns 200

Next MCP call will attempt `get_or_refresh()` fresh (cache is empty from the token=None write). If it succeeds → resumed. If it fails again → re-transitions to AttentionRequired.

Closes cred.3.1-adv-01: the `load_from_disk()` slow path re-reads `state_path` from disk (already done) after the manual reset. No need to track mtime — the provider-state reset IS the trigger.

### §6 — Retry budget vs MCP_TIMEOUT=30s

In-call retry ≤1 for **401 on the API call** (stale access token). Per-leg timeout budget:
- Token fetch (each leg): ≤6s (was 8–10s in plan; tightened to leave headroom)
- Upstream forward (each leg): ≤4s (pass-through, not gateway-owned — but set `timeout` on the upstream client)
- Total fast-fail guard: if `(token1_elapsed + upstream1_elapsed) > 15s`, do NOT retry the second leg; return classified `Retryable` to the agent immediately

Two-leg sequence: 6 + 4 = 10s per leg × 2 legs = 20s max, leaving 10s headroom before MCP_TIMEOUT. Adds test: stub slow upstream (first upstream call hangs 16s) → tool call returns `Retryable` in <20s (not 30s guillotine).

### §7 — No-loopback enforcement

In headless/broker mode `oauth_start_auth` returns a device-flow-redirect message (as `broker_managed` already does), so the dead-end loopback dance is unreachable.

### §8 — Secrets redaction (harden new channels, not fix old ones)

cred.5-ar-01 is already closed (v0.75.0). This section hardens the NEW channels introduced by this increment:
- `CredentialAttentionRequired` event body: include only classified enum + hint, never raw endpoint body
- `CredentialSnapshot.attention_reason`: must not contain `ya29.`, `1//...`, `refresh_token`, `client_secret`, `access_token`
- Proactive refresh task's `CredentialRefreshFailed` emission: same redaction as production path
- Checkpoint `credential_health`: reason string passed through redaction before write

Test: token endpoint returns JSON with `{"error":"invalid_grant","refresh_token":"ya29.secret"}` → assert none of `ya29.`, `1//`, `client_secret` appear in flight events, checkpoint, or credential snapshot.

## Google — instance #1 (OAuth-refresh, fully wired)

- **Recovery vehicle:** host device flow. Surfaced instruction in `attention_reason`: *"On your Mac host (not the container), run `agentctl auth google --device`."*
- **Fix `agentctl auth google --device` (Eng E5 + E7-new):** when CLI arg `--client-id`/`--client-secret` and env vars are absent, read `client_id`/`client_secret` from the **existing `google.json`** (confirmed to include all four fields). Also read and preserve `token_url` (if present; `write_secrets_file` currently drops it, silently breaking custom/WIF token endpoints on recovery). Guard inversion: if credentials are read FROM the existing file, skip the `--force` guard (the file WILL be overwritten in-place with atomic rename; force is not needed when the source and dest are the same file). The recovery command is a clean `agentctl auth google --device` with **no `--force` and no re-supplied secrets**.
- **Docs/CLI guidance (not REQUIRED code gate):** update `agentctl auth google` output + `DEPLOYMENT.md`; fix `MCP_SERVERS.md:119` ("add your email as a Test user") which steers operators INTO the weekly-expiry trap; add an `invalid_grant` row to the error table. Verify 7-day behavior for `gmail.readonly` unverified app before documenting.

## Api-key — instance (Brave / custom, wired)

- **Recovery vehicle:** operator updates the secret (`agentos.env` / the key file). The framework re-reads on next use when the management API `POST /api/v1/credentials/<provider>/reset-attention` is called. Surfaced instruction names the exact secret/env var to update.

## Durability gap to close

- **`write_state_atomic()` missing `sync_all()`** (Finding 8 / Eng review): add `f.sync_all().await` before the `rename()` call, and fsync the parent directory after rename (matching `CheckpointStore::save()` pattern). The proactive refresh task writes this file every ~55 min; durability gap becomes more significant at higher write frequency.

## Adjacent cred debts to close in the same PR

- **cred.3-ar-02** — surface `credential_refresh_failed` when no operator is attached (prerequisite for "operator sees the 2am failure").
- **cred.3.1-adv-01** — consolidated into §5 (manual reset via management API; slow path re-reads `state_path` from disk; no mtime tracking needed).

(cred.5-ar-01 is CLOSED in v0.75.0 — removed from scope.)

## Test plan (each fails without its fix)

- **Classifier (table-driven):** `(status, error_code)` → class. Assert `(429,rate_limit_exceeded)→Retryable`, `(401,invalid_client)→AttentionRequired(ConfigFix)` (NOT Reauth — would cause infinite operator loop), `(400,invalid_grant)→AttentionRequired(Reauth)`, 400-non-JSON→Retryable (fail-safe), 500/timeout/DNS→Retryable. Separate: `401 on API call` → one silent cache invalidate + retry, NOT AttentionRequired.
- **Provider state machine:** stub token endpoint returns `invalid_grant` → assert first caller emits `CredentialAttentionRequired`, second caller gets fast-fail (no second endpoint call), no second event.
- **Proactive refresh:** stub token endpoint to succeed with 55-min expiry → assert background task refreshes before expiry; try-lock test: foreground holds mutex while background fires → assert background skips (no double refresh); panic test: task panics → assert it restarts within 60s + 10s.
- **Proactive task SIGTERM:** cancel token fires → task exits without holding mutex.
- **Durability (checkpoint):** AttentionRequired state → agentd restart → provider still AttentionRequired after restore; no second CredentialAttentionRequired event; `provider_states` pre-populated from checkpoint.
- **Management API reset-attention:** POST /api/v1/credentials/google/reset-attention → ProviderHealth transitions to Healthy; next MCP call attempts get_or_refresh(); success → CredentialRecovered emitted.
- **Recovery command (agentctl):** `agentctl auth google --device` with no env vars and existing `google.json` → reads client_id/secret from file, NO `--force` needed, atomic overwrite-in-place, success.
- **Secret redaction:** token endpoint returns JSON with `refresh_token`, `client_secret`, `access_token` values → assert none appear in flight events, checkpoint, or credential snapshot.
- **MCP_TIMEOUT two-leg retry:** stub token endpoint to slow (each call: 5s); stub upstream to slow (each call: 4s) → first 401-retry sequence returns Retryable before 20s; second test: stub first upstream to hang 16s → fast-fail before 15s total elapsed.
- **write_state_atomic durability:** write_state_atomic called → assert sync_all() called before rename (verify with file-level tracing or a unit test on the function's internal sequence).
- **Api-key:** invalid key → AttentionRequired(SecretReplace) surfaced in credential snapshot; operator calls reset-attention + updates key → CredentialRecovered.
- **google.json schema:** confirm all four fields (client_id, client_secret, refresh_token, access_token placeholder) present; assert write_secrets_file writes all four and recovery read path finds them.
- **EventKind completeness:** `CredentialAttentionRequired`, `CredentialRecovered` added to events.rs enum, CONVENTIONS.md taxonomy table, and `event_taxonomy_completeness` test.

## Acceptance

- A revoked/expired Google refresh token → gateway classifies AttentionRequired(Reauth), emits `CredentialAttentionRequired` flight event, surfaces `attention_reason` + `recovery_kind` in `agentctl watch [c]`; the agent loop stays alive (non-Gmail work continues); no browser-callback dead-end; proactive refresh task sees the same error and does NOT storm the token endpoint (provider-state fast-fail active).
- Operator runs `agentctl auth google --device` on the host (clean, no `--force`) → `google.json` rewritten → operator calls `POST /api/v1/credentials/google/reset-attention` → gateway picks up fresh credential on next MCP call → `CredentialRecovered` emitted → Gmail resumes.
- A transient blip → Retryable, bounded retry with per-leg budget, no false AttentionRequired prompt.
- An api-key provider with a bad key → AttentionRequired(SecretReplace) surfaced with re-supply instruction.
- No token/secret in any log/event/approval/checkpoint.
- Proactive refresh eliminates Transient failures during normal operation (60-min token renewed at 55 min).
- Every path has a test that fails without the fix.

## Non-goals

- In-container device flow (D1b) · container-writable token store (D3) · push notifications (ux.4 — separate) · a second OAuth provider's device flow (GitHub — follow-up, same seam) · Google app verification (operator/GCP task) · multi-instance credential health (cred.8) · dedup/idempotency for N-agent storms (cred.8)

## Autoplan decision audit (2026-07-13 — final, all voices applied)

| # | Decision | Classification | Basis |
|---|----------|----------------|-------|
| 1 | Layer = broker gateway (not Python sidecar) | User Challenge → user chose general-purpose | 4-voice consensus + provider-agnostic requirement |
| 2 | v1 wires both credential classes (OAuth + api-key) | User decision | user pick "framework + both classes" |
| 3 | D1 = host-driven; cut D1b + D3 | Auto (consensus) | reuse dx.4; in-container infeasible under MCP_TIMEOUT |
| 4 | Cut push (ux.4) | Auto (mechanical) | ux.4 does not exist |
| 5 | 3-way FailureClass (not 5-way) | Auto (CEO/Eng consensus) | ApiKeyInvalid doesn't fit refresh path; Unknown→Transient loops |
| 6 | Dedup cut from v1 | Auto (CEO) | scenario not observed in production; benign if it fires |
| 7 | Redact endpoint body from new channels | Auto (security) | harden new channels, not old ones (old already closed v0.75.0) |
| 8 | ProviderHealth in checkpoint (not p7.4 approval) | Auto (CEO+Eng consensus) | p7.4 wrong data model; gateway can't create approvals |
| 9 | Google Production-publishing = docs/CLI only | Auto (CEO) | cannot be code gate; 7-day behavior unconfirmed |
| 10 | Recovery command reads secrets from google.json | Auto (DX) | schema confirmed; --force guard must be inverted |
| 11 | Proactive refresh IN SCOPE (tokio task per OAuth provider) | User decision (CEO D1) | user chose "Reactive + proactive" |
| 12 | Provider state machine above OAuthTokenCache | Auto (Eng) | mutex only serializes refreshes; N sequential callers each classify |
| 13 | try_lock() for proactive task | Auto (Eng) | prevents foreground request starvation under cache mutex |
| 14 | per-leg timeout budget (6s + 4s) | Auto (Eng) | 2-leg retry must complete < 20s to leave 10s headroom before MCP_TIMEOUT |
| 15 | management API reset-attention endpoint | Auto (Eng) | replaces mtime-based resume; removes sub-second race conditions |
| 16 | sync_all() in write_state_atomic() | Auto (Eng) | closes durability gap opened in cred.3 |
| 17 | cred.5-ar-01 dropped | Auto (CEO) | already closed in v0.75.0 |
| 18 | cred.3.1-adv-01 consolidated under §5 | Auto (CEO+Eng) | management API reset-attention covers the same ground |
