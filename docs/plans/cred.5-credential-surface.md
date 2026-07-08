# cred.5 — Credential Surface (Full Visibility Plane)

**Version:** v0.68.0  
**Depends on:** cred.3, cred.3.2, cred.4b (all landed)  
**Roadmap goal:** Full credential observability — who has what, last access, token expiry, denied counts, gateway health, provider readiness.  
**Engineering review:** ✓ (11 findings — 2 critical, 3 high, 6 medium/low — all addressed below)

## Problem

An operator running `agentd` with the credential broker today cannot answer:
- Which agents currently hold credential grants, and to which providers?
- When did each agent last successfully call a provider?
- Is a provider's OAuth token about to expire (or already expired)?
- How many requests were denied vs allowed per agent?
- Is the credential gateway healthy? Are all configured providers provisioned?

Without this, credential failures surface only as tool errors in the agent's output or buried in `flight.jsonl` grep sessions. The most common failure mode — silent OAuth token expiry — is invisible until agents start failing.

## What We're Building

### 1. New snapshot types in `surfaces/src/snapshot.rs`

**NOT in `agentd`** — all snapshot types live in the `surfaces` crate (precedent: `SandboxSummary`, `PendingActionView`, `IsolationCapsSummary`). Placing them in `agentd` would be a circular crate dependency.

Per-agent credential detail is embedded directly in `AgentSnapshot` (consistent with `accessible_server_names` and `accessible_server_names` pattern) — no separate `AgentCredentialGrant` struct.

```rust
/// System-wide credential snapshot: gateway health and per-provider status.
/// Per-agent credential usage is embedded in AgentSnapshot fields.
#[derive(Clone, Default, Serialize)]
pub struct CredentialSnapshot {
    /// True when credential_gateway.enabled = true in config.
    pub gateway_enabled:      bool,
    /// Provider names configured at startup (from cfg.providers.keys()).
    pub configured_providers: Vec<String>,
    /// Per-provider health (one entry per configured provider).
    pub provider_health:      Vec<ProviderHealth>,
}

#[derive(Clone, Serialize)]
pub struct ProviderHealth {
    /// Provider name (matches a configured_providers entry).
    pub name:            String,
    /// True when a non-expired token is cached, or (for api-key providers)
    /// when the key env var was non-empty at startup. Always false until
    /// GatewayState::new() runs load_from_disk() and warm_provider_expiry().
    pub token_fresh:     bool,
    /// Unix secs of last successful token refresh; None for api-key providers.
    pub last_refresh_at: Option<u64>,
    /// Unix secs of token expiry from the in-memory cache; None for api-key providers.
    pub expires_at:      Option<u64>,
    /// Last refresh error string; cleared on next successful refresh.
    pub last_error:      Option<String>,
}
```

### 2. `AgentSnapshot` additions (`surfaces/src/snapshot.rs`)

Four new fields on `AgentSnapshot` (empty when no credential grant exists):

```rust
pub credential_providers:      Vec<String>,
pub credential_request_counts: HashMap<String, u64>,  // provider → success count
pub credential_denied_counts:  HashMap<String, u64>,  // provider → denied count
pub credential_last_access_at: HashMap<String, u64>,  // provider → unix secs
```

`AgentSnapshot::serialize` updated to emit these four fields (consistent with
the existing field_count + manual Serialize pattern).

`HashMap` requires adding `use std::collections::HashMap` to `surfaces/src/snapshot.rs`.

### 3. `CredentialGateway` struct changes (`agentd/src/credential/mod.rs`)

Five new `Arc<std::sync::RwLock<...>>` fields. **`std::sync` (not tokio)** — this is deliberate: sync RwLock enables reading from `update_snapshot` (a non-async function) without `.await`. Writes in the async handler hold the lock for microseconds (single HashMap insert), which is acceptable.

```rust
pub struct CredentialGateway {
    registry:                 Arc<CredentialRegistry>,
    counters:                 CapCounters,       // existing: Arc<tokio::sync::RwLock<...>>
    caps_db:                  Option<Arc<Database>>,
    // NEW — 5 sync-readable maps:
    configured_providers:     Vec<String>,       // set once in start(), never mutated
    denied_counters:          Arc<std::sync::RwLock<HashMap<(String,String), u64>>>,
    last_access:              Arc<std::sync::RwLock<HashMap<(String,String), u64>>>,
    provider_expiry:          Arc<std::sync::RwLock<HashMap<String, u64>>>,
    provider_last_refresh_at: Arc<std::sync::RwLock<HashMap<String, u64>>>,
    provider_last_error:      Arc<std::sync::RwLock<HashMap<String, String>>>,
}
```

`configured_providers` exposed via accessor, not a pub field:
```rust
pub fn configured_providers(&self) -> &[String] { &self.configured_providers }
```

All 5 Arc fields populated in `start()` from `cfg.providers.keys().cloned().collect()` (for `configured_providers`) and `Arc::new(std::sync::RwLock::new(HashMap::new()))` (for the maps).

### 4. `GatewayState` additions (`agentd/src/credential/mod.rs`)

`GatewayState` gets the same 5 Arcs (cloned from `CredentialGateway` in `start()`).

In `handle_credential_request`, add:

```rust
// On successful credential delivery:
last_access.write().unwrap()
    .insert((agent_id.clone(), provider.clone()), now_unix);

// On denial (provider_not_allowed / no_providers_configured / etc.):
*denied_counters.write().unwrap()
    .entry((agent_id.clone(), provider.clone())).or_insert(0) += 1;

// After successful get_or_refresh():
// Update expiry and last-refresh maps; clear prior error.
if let Ok(inner) = cache.state.try_lock() {
    provider_expiry.write().unwrap()
        .insert(provider.clone(), inner.expires_at);
    provider_last_refresh_at.write().unwrap()
        .insert(provider.clone(), now_unix);
}
provider_last_error.write().unwrap().remove(&provider);  // clear on success

// After failed get_or_refresh():
provider_last_error.write().unwrap()
    .insert(provider.clone(), err_string);
```

Note: `denied_counters` uses a plain `u64` behind `std::sync::RwLock` (write lock on every denial). Denials are not on the hot path, so this is acceptable. The existing `counters` field uses `Arc<AtomicU64>` behind a tokio RwLock to amortize write-lock cost on the high-frequency success path.

### 5. Cold-start warm-up in `GatewayState::new()`

After `cache.load_from_disk(sp).await` for each OAuth provider:

```rust
// Warm provider_expiry so snapshot() shows FRESH at startup without
// waiting for the first agent request.
if let Ok(inner) = cache.state.try_lock() {
    if inner.token.is_some() && inner.expires_at > now_unix {
        provider_expiry.write().unwrap()
            .insert(name.clone(), inner.expires_at);
    }
}
```

For API-key providers (`auth_style = "api-key-header"` / `"api-key-query"`):
- `token_fresh = true` (the key was successfully loaded into the provider at startup)
- `expires_at = None`, `last_refresh_at = None`
- No warm-up needed (no cache)

### 6. `CredentialGateway::snapshot()` — sync method

Reads all 5 maps + `registry` + `counters` → `CredentialSnapshot`. **Sync** (uses `std::sync::RwLock::read().unwrap()`), callable directly from `update_snapshot` without `.await`.

```rust
pub fn snapshot(&self) -> CredentialSnapshot {
    let provider_expiry = self.provider_expiry.read().unwrap();
    let last_refresh    = self.provider_last_refresh_at.read().unwrap();
    let last_error      = self.provider_last_error.read().unwrap();
    let now             = now_unix_secs();
    let provider_health = self.configured_providers.iter().map(|name| {
        let expires_at = provider_expiry.get(name).copied();
        ProviderHealth {
            name:            name.clone(),
            token_fresh:     expires_at.map(|e| e > now).unwrap_or(false),
            last_refresh_at: last_refresh.get(name).copied(),
            expires_at,
            last_error:      last_error.get(name).cloned(),
        }
    }).collect();
    CredentialSnapshot {
        gateway_enabled:      true,
        configured_providers: self.configured_providers.clone(),
        provider_health,
    }
}
```

### 7. `CredentialGateway::agent_grant_for()` — sync method

Returns per-agent credential usage for embedding in `AgentSnapshot`:

```rust
pub fn agent_grant_for(
    &self, agent_id: &str,
) -> (Vec<String>, HashMap<String,u64>, HashMap<String,u64>, HashMap<String,u64>) {
    let reg  = self.registry.tokens.read();   // existing CredentialRegistry
    let denied = self.denied_counters.read().unwrap();
    let access = self.last_access.read().unwrap();
    let counters = // read from self.counters (existing CapCounters)
    ...
}
```

### 8. `SchedulerSnapshot` addition (`surfaces/src/snapshot.rs`)

```rust
/// System-wide credential snapshot. None when gateway is disabled.
#[serde(skip_serializing_if = "Option::is_none")]
pub credential_snapshot: Option<CredentialSnapshot>,
```

### 9. Scheduler wiring (`agentd/src/scheduler.rs` + `main.rs`)

- Add `cred_gw: Option<Arc<CredentialGateway>>` to `SchedulerState` struct
- In `update_snapshot(snapshot, state)` — **no signature change** needed since `snapshot()` is sync:
  ```rust
  // Build system credential snapshot:
  s.credential_snapshot = state.cred_gw.as_ref().map(|gw| gw.snapshot());
  
  // Per-agent: for each agent in state.agents:
  let (cred_providers, cred_req, cred_denied, cred_access) =
      state.cred_gw.as_ref()
          .map(|gw| gw.agent_grant_for(&agent.id))
          .unwrap_or_default();
  // populate AgentSnapshot with these 4 fields
  ```
- In `main.rs`: set `state.cred_gw = maybe_cred_gw.clone()` after building `SchedulerState`
- `deregister_token` update: also clear `denied_counters` and `last_access` entries for the agent

### 10. FUSE surface (`surfaces/src/agents_fs.rs`)

- **`INO_SYS_CREDENTIALS = 19`** — `/agents/system/credentials` — JSON `CredentialSnapshot` (system-level only).
- **`OFF_CREDENTIALS = 13`** — `/agents/<id>/credentials` — per-agent JSON from `AgentSnapshot` credential fields:
  ```json
  {"providers":["google","brave_search"],"request_counts":{"google":42},"denied_counts":{},"last_access_at":{"google":1720000000}}
  ```

Compile-time assert: `OFF_CREDENTIALS < DIR_STEP - 1` (13 < 19 ✓).

### 11. Management API (`agentd/src/management.rs`)

`GET /api/v1/credentials` → 200 `CredentialSnapshot` JSON.

When gateway disabled: `{"gateway_enabled":false,"configured_providers":[],"provider_health":[]}` — never `{}`.

Note: `CredentialSnapshot` is also embedded in `/api/v1/snapshot` (via `SchedulerSnapshot.credential_snapshot`). The dedicated endpoint is kept for `HttpSource.load_credentials()` efficiency — avoids parsing the full snapshot.

### 12. `agentctl watch` — Credentials pane

New `View::Credentials` (`[c]` key — confirmed free; existing: `[d]` Dashboard, `[a]` Approvals, `[i]` Inspector, `[m]` Memory, `[s]` Spawn, `[t]` Topology).

New file `agentctl/src/watch/credentials.rs`.

Layout:
```
┌─ Credentials ──────────────────────────────────────────────────────────┐
│ Gateway: enabled   Providers: google  brave_search  github             │
│                                                                        │
│ Provider Health:                                                       │
│   google        FRESH  exp: 45m   last_refresh: 2m ago                │
│   brave_search  FRESH  exp: 58m   last_refresh: 5m ago                │
│   github        STALE  exp: ─     err: token_path not found           │
│                                                                        │
│ Agent Grants:                                                          │
│ Agent         Provider       Req  Denied  Last Access                  │
│ ─────────────────────────────────────────────────────────────────────  │
│ scout-1       google          42       0  2m ago                       │
│               brave_search     7       0  8m ago                       │
│ coord-1       google           3       1  15m ago                      │
└────────────────────────────────────────────────────────────────────────┘
```

Keys: `Esc`/`q` → Dashboard. `↑/↓` scroll agent grants table.

Per-agent rows are sourced from `SchedulerSnapshot.agents[*].credential_*` fields (not from `CredentialSnapshot`) — no cross-snapshot lookup in the FUSE handler.

**Plain mode**: prints `credential_gateway: enabled`, provider health table, agent grants table from each agent.

### 13. `DataSource` trait extension + implementations

```rust
async fn load_credentials(&self) -> Option<CredentialSnapshot>;
```

`FuseSource` reads `/agents/system/credentials` (parse JSON).  
`HttpSource` calls `GET /api/v1/credentials`; returns `None` on server error (not panic).

`DataSource` is a breaking change for mock implementors in tests — existing test mocks must add `async fn load_credentials(&self) -> Option<CredentialSnapshot> { None }`.

`agentctl/src/watch/reader.rs` gains deserialization structs: `CredSnapshot`, `ProvHealthInfo` (Deserialize mirrors of `CredentialSnapshot`, `ProviderHealth`).

## Flight Events

None new — existing `EgressBrokered`/`EgressRejected` already record per-request events. This surface is read-only observability over in-memory state.

## Acceptance Criteria (ROADMAP §cred.5)

- [ ] Operator sees per-agent credential grants + last access via FUSE (`/agents/<id>/credentials`)
- [ ] Operator sees system-wide credential view via FUSE (`/agents/system/credentials`)
- [ ] Operator sees per-provider token expiry and health in `agentctl watch` `[c]` pane
- [ ] `GET /api/v1/credentials` returns full `CredentialSnapshot` JSON
- [ ] Gateway disabled → all surfaces return well-formed empty response (not `{}` or 500)

## Out of Scope (deferred)

- **Rotation policy** (cred.5b) — requires scheduler-level cron + config schema
- **Revocation / force refresh / hot reload** (cred.5b) — write paths
- **Alternate `CredentialStore` backends** (cred.5c) — OS keychain / vault
- **Token expiry countdown UI alert** (cred.5b) — proactive scheduler check

## Test Plan (20 tests)

1. `credential_snapshot_empty_when_gateway_disabled` — snapshot with gateway disabled returns `gateway_enabled: false`, empty lists.
2. `credential_snapshot_configured_providers` — configured_providers comes from startup config, not runtime registry.
3. `credential_snapshot_agent_grants_with_counts` — register token, simulate allowed request counter increment, verify AgentSnapshot.credential_request_counts.
4. `credential_snapshot_denied_counts` — simulate denied request, verify `credential_denied_counts` incremented.
5. `credential_snapshot_last_access_updated` — simulate allowed request, verify `credential_last_access_at` updated.
6. `credential_snapshot_provider_expiry` — simulate successful token refresh, verify `expires_at` in provider_health.
7. `credential_snapshot_provider_last_error` — simulate refresh failure, verify `last_error` in provider_health.
8. `credential_provider_last_error_cleared_on_success` — simulate failed refresh, then successful; verify `last_error` becomes None.
9. `credential_token_fresh_true_after_load_from_disk` — provider_health.token_fresh is true immediately after startup with pre-populated cache (no request needed).
10. `credential_api_key_provider_health_fields` — api-key provider shows token_fresh=true, expires_at=None, last_refresh_at=None.
11. `credential_deregister_clears_denied_and_access` — deregister last token, verify denied_counters and last_access cleared.
12. `credential_snapshot_before_first_request` — configured_providers non-empty even when agent_grants empty.
13. `fuse_system_credentials_file_json` — FUSE `/agents/system/credentials` returns valid JSON CredentialSnapshot.
14. `fuse_per_agent_credentials_file` — per-agent file returns correct provider/count JSON from AgentSnapshot fields.
15. `management_api_credentials_200` — GET /api/v1/credentials returns 200 with snapshot JSON.
16. `management_api_credentials_disabled_gateway` — gateway disabled → 200 with `gateway_enabled: false`.
17. `data_source_fuse_load_credentials` — FuseSource reads FUSE file correctly.
18. `data_source_http_load_credentials` — HttpSource calls management API.
19. `data_source_http_load_credentials_error_path` — server unreachable → None, not panic.
20. `compile_assert_off_credentials` — compile-time assert `OFF_CREDENTIALS < DIR_STEP - 1`.

## Implementation Order

1. Add `CredentialSnapshot`, `ProviderHealth` to `surfaces/src/snapshot.rs`; add 4 credential fields to `AgentSnapshot`; update `AgentSnapshot::serialize` field_count and field emissions
2. Add `HashMap` import to `surfaces/src/snapshot.rs`
3. Add 5 new `Arc<std::sync::RwLock<...>>` fields to `CredentialGateway` + `configured_providers: Vec<String>`; add `pub fn configured_providers(&self) -> &[String]` accessor; populate in `start()`
4. Thread the 5 new Arcs into `GatewayState`; update `handle_credential_request` to write `last_access`, `denied_counters`, `provider_expiry`, `provider_last_refresh_at`, `provider_last_error` (including error-clear on success)
5. Add cold-start warm-up in `GatewayState::new()` after each `load_from_disk()`
6. Add `CredentialGateway::snapshot()` sync public method → `CredentialSnapshot`
7. Add `CredentialGateway::agent_grant_for(agent_id: &str)` sync public method
8. Update `deregister_token` to clear `denied_counters` + `last_access` for the agent
9. Add `credential_snapshot: Option<CredentialSnapshot>` to `SchedulerSnapshot` with `#[serde(skip_serializing_if = "Option::is_none")]`; add `cred_gw: Option<Arc<CredentialGateway>>` to `SchedulerState`
10. Update `update_snapshot` to call `state.cred_gw.as_ref().map(|gw| gw.snapshot())` and populate `s.credential_snapshot`; populate per-agent credential fields from `gw.agent_grant_for()`
11. Wire `state.cred_gw = maybe_cred_gw.clone()` in `main.rs`
12. Add `INO_SYS_CREDENTIALS = 19` + `OFF_CREDENTIALS = 13` to `agents_fs.rs`; implement `getattr` + `read` for both; compile-time assert
13. Add `GET /api/v1/credentials` route to `management.rs`
14. Create `agentctl/src/watch/credentials.rs`; add `CredSnapshot`/`ProvHealthInfo` deserialize structs to `reader.rs`
15. Extend `DataSource` trait with `load_credentials()`; implement for `FuseSource` + `HttpSource` (error path → None); update existing test mocks
16. Add `View::Credentials` to `watch/app.rs` + render in `watch/views.rs`
17. Wire `[c]` key in `watch/mod.rs`; add plain-mode output
18. Write 20 tests
19. Update ROADMAP (check cred.5 ✓, note rotation policy → cred.5b), CHANGELOG, bump to v0.68.0

## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| 1 | CEO | Include token expiry in snapshot | Mechanical | P1 | Roadmap AC names it; primary diagnostic for OAuth failures | defer |
| 2 | CEO | Include denied_counts | Mechanical | P1 | Security-relevant signal, same counter pattern as request_counts | defer |
| 3 | CEO | Include last_access_at | Mechanical | P1 | Explicit in ROADMAP acceptance criterion | defer |
| 4 | CEO | Include provider health struct | Taste | P1 | Makes gateway health observable without a new management query | omit |
| 5 | CEO | Defer rotation policy to cred.5b | Mechanical | P3 | Requires scheduler-level cron + config schema; separate concern | include |
| 6 | CEO | Return structured JSON when disabled, not {} | Mechanical | P5 | {} treats disabled=unconfigured=empty as same state | {} |
| 7 | Eng | Define snapshot types in surfaces/ crate | Mechanical | P1 | Crate dep runs agentd→surfaces; placing in agentd = circular dep | agentd |
| 8 | Eng | Per-agent data inline on AgentSnapshot | Mechanical | P1 | Consistent with accessible_server_names; avoids cross-snapshot FUSE lookup | AgentCredentialGrant |
| 9 | Eng | std::sync::RwLock for 5 new maps | Mechanical | P5 | Enables sync read in update_snapshot; brief writes acceptable | tokio RwLock |
| 10 | Eng | snapshot() is sync (not async) | Mechanical | P5 | Reads std::sync maps; eliminates need to change update_snapshot signature | async |
| 11 | Eng | 5th Arc: provider_last_refresh_at | Mechanical | P1 | Distinct from expires_at; refresh time ≠ expiry time | derive from expiry |
| 12 | Eng | Warm provider_expiry at startup | Mechanical | P1 | Prevents false STALE on cold start with valid pre-loaded token | first-request only |
| 13 | Eng | Clear provider_last_error on success | Mechanical | P1 | Stale errors mislead 3am on-call; clear on recovery | persist |
| 14 | Eng | token_fresh=true for api-key providers | Taste | P5 | Key loaded at startup; definitionally fresh; no expiry applies | false/unknown |
| 15 | User | Sequencing cred.5 before dx.3 | User override | — | User explicitly directed this order | swap |

## GSTACK REVIEW REPORT

### CEO Review — Full Credential Control Plane
- Premise: Accepted. User confirmed Full Credential Control Plane scope.
- Decisions D1–D8: all accepted above.

### Design Review — N/A (backend-only surface, no design decisions)

### Eng Review — ✓ Complete
- 2 Critical (both fixed: crate placement, sync/async architecture)
- 3 High (all fixed: cold-start warm, error-clear, 5th Arc)
- 3 Medium (addressed: inline AgentSnapshot, api-key semantics defined, race documented)
- 3 Low (addressed: comment on denied_counters, accessor method, skip_serializing_if)
- 5 additional tests added (total 20)
- Codex voice: unavailable (timeout) — Claude eng only

### DX Review — ✓ Complete
- `[c]` key confirmed free
- `DataSource` breaking change documented (mock impls need `load_credentials()`)
- `HttpSource` error path → None (not panic) added to test plan and implementation
- No other DX blockers
