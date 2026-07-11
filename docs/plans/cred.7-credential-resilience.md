# cred.7 — Credential resilience (general-purpose refresh/credential-failure recovery)

**Increment:** cred.7 (Phase 10 — Credential manager). Supersedes the `auth-resilience` seed.
**Renamed 2026-07-11:** was `cred.6`; the CEO review split the broker-mode migration out as its own
prerequisite increment (**cred.6**, `docs/plans/cred.6-broker-migration.md`). This plan is the
resilience framework that rides *on top of* broker mode.
**Status:** Planned — hardened by `/autoplan` (2026-07-10, 4 independent voices: CEO/Eng/DX subagents + Codex). Not started.
**Depends on:** **cred.6 (broker migration)** — the CoS must be broker-mode first; plus cred.3+ broker
(`agentd/src/credential/mod.rs`), dx.4 device flow (`agentctl auth google --device`), p7.4 approvals.
**Premise correction (2026-07-11):** an earlier draft attributed the live "not authenticated" failure
to the FsRead sandbox cap (`4a0951a4`) / a stale image. That diagnosis was **wrong** — the FsRead cap
was a no-op on Docker Desktop (Landlock absent), and the real cause was the `oauth_mcp.py` `check_auth`
state-machine bug, fixed in **v0.73.2 (#102)**. Auth works today; this increment is not about that bug.
**Secret-redaction moved out:** the token-endpoint-body redaction (§8 below) is pulled forward into
**cred.6** as a do-first P0 (it is a live leak, ships independently of the resilience framework).

## Problem (corrected — the honest gap)

When a broker credential's refresh/validation **terminally** fails (revoked, expired, key rotated, deleted client), the agent gets an **opaque error**, keeps burning cron cycles, the operator gets **no signal**, and **nothing resumes** after the credential is fixed. This is provider-agnostic across the broker's OAuth-refresh and api-key adapters.

Note the seed overstated the failure as an automatic "loopback dead-end." There is no automatic fallback — on refresh failure the gateway/sidecar just returns an error and stops. The real gap is **no terminal detection, no operator surfacing, no resume signal** — and, in multi-agent mode, no de-duplication.

## Decisions (locked via /autoplan 2026-07-10)

- **Layer = the Rust broker gateway** (`credential/mod.rs::get_or_refresh`), provider-agnostic. NOT the Python `oauth_mcp.py` (that would be Gmail-specific, and becomes dead code once the CoS moves to broker mode — which cred.4b built the sidecars for). *(User Challenge resolved: general-purpose ⇒ the only provider-agnostic layer is the gateway.)*
- **General-purpose framework + BOTH credential classes wired in v1** (OAuth-refresh AND api-key). Google is instance #1.
- **D1 = host-driven recovery** (reuse dx.4's device flow). **D1b + D3 cut** — no in-container device flow, no container-writable token store. The RFC 8628 poller already exists and is hardened in `agentctl/src/auth/google_device.rs`, and in-container polling cannot fit under `MCP_TIMEOUT=30s`.
- **Push (ux.4) cut** — it does not exist. MVP notification = a p7.4 approval + a durable flight event. (Add ux.4 as its own increment later if wanted.)
- **Google Production-publishing prevention is REQUIRED**, not docs-optional — it removes the weekly 7-day Testing-mode expiry that is most of the failure volume.
- **Precondition = cred.6 (broker migration), now its own increment.** Moving the CoS to broker mode
  (`[credential_gateway]` + a `Credential{Google}` grant in both `cos.agents.toml` files) is the
  prerequisite; it is built and validated in cred.6 before this resilience work starts.

## The general framework (in the gateway)

1. **Classification (RFC 6749, provider-agnostic).** Parse the token-endpoint / API JSON `error` field (never HTTP status class alone) into a 5-way enum:
   - `Transient` — 5xx, `429`/`rate_limit_exceeded`/`slow_down`, network/DNS/timeout → bounded retry + backoff.
   - `UserReauthRequired` — `invalid_grant` on refresh → attention, recovery = re-auth.
   - `OperatorConfigRequired` — `invalid_client` / `unauthorized_client` / `invalid_scope` → attention, recovery = **fix config** (a DISTINCT message; re-auth will NOT fix it — routing this to re-auth = infinite operator loop).
   - `ApiKeyInvalid` — 401/403 on an api-key provider (no refresh possible) → attention, recovery = supply a new key.
   - `Unknown` → treat as `Transient` (fail toward retry, never toward spamming the operator).
   - Distinguish **401 on the API call** (access-token stale → refresh + retry once) from **failure on the refresh call** (refresh-token bad → attention). Conflating them misroutes an access-token blip into a reauth prompt.

2. **Per-provider state:** `Healthy | TransientRetry{until} | AttentionRequired{reason, recovery_kind, approval_id, detected_at}`. Owned by the gateway, keyed by provider.

3. **Durability:** the `AttentionRequired` state is anchored by a **checkpointed p7.4 approval** (the sidecar/gateway loses in-memory state on restart; the parked approval is the durable owner — `ParkedApprovalEntry`, already checkpointed). If a gateway-side flag is added, bump `FORMAT_VERSION` + `#[serde(default)]`; restore before spawning MCP servers.

4. **De-duplication:** a provider-scoped idempotency key **`reauth:<provider>`** in `ApprovalStore` collapses duplicate pending requests into one. The gateway serializes classification (existing `Mutex`), so N concurrent callers → one attention record → one approval. The gateway returns an **attention sentinel** so agents 2..N don't each open a new approval. (Today: 3 inbox agents on a cron trigger → 3 identical prompts.)

5. **Surface:** ONE coalesced p7.4 approval carrying a **per-provider recovery instruction**. Durable flight events `CredentialAttentionRequired{provider, reason, recovery_kind}` and `CredentialRecovered{provider}`. **Bounded escalating re-notify** (at 0, then daily) — never per-turn. **Park only the affected capability** — the rest of the agent keeps working — and **do NOT re-refresh a terminal failure every turn** (budget guard / no retry storm).

6. **Resume:** on next use, the gateway does a **fresh read of the credential source** (`token_path`/`state_path`) by mtime/version, invalidating the stale in-memory token, so a host-updated `google.json` (or an updated api-key secret) is picked up with **NO restart**. Clears `AttentionRequired`, emits `CredentialRecovered`. (Fixes the current bug where the in-memory token wins over the rewritten file.)

7. **Retry budget vs `MCP_TIMEOUT=30s`:** in-call retry ≤1 with the per-request timeout dropped to ~8–10s (whole tool call stays < ~25s). Longer backed-off retries happen **across** tool calls (agent re-invokes) or inside the gateway's own refresh path (no 30s tool ceiling). Never let a retry get guillotined by agentd's 30s cancel and misreported as a generic timeout.

8. **Secrets — redact before any log/event/approval/tool-response.** Strip `access_token`/`refresh_token`/`id_token`/`client_secret`/`authorization` and long bearer-looking strings; emit only the classified enum + a safe hint, never the raw endpoint body. **Fixes a live leak:** `oauth_mcp.py:430` returns the raw token-endpoint body to the agent, and `credential/mod.rs:341,371` stores it in error strings + flight events.

9. **No-loopback enforcement (concrete gate, not a hope):** in headless/broker mode `oauth_start_auth` returns a device-flow-redirect message (as `broker_managed` already does), so the dead-end loopback dance is unreachable.

## Google — instance #1 (OAuth-refresh, fully wired)

- **Recovery vehicle:** host device flow. Surfaced instruction: *"On your Mac host (not the container), run `agentctl auth google --device`."*
- **Fix `agentctl auth google --device`:** when env/flags are absent, read `client_id`/`client_secret` from the **existing `google.json`**, and overwrite-in-place atomically (`.bak`) — so the recovery command is a clean `agentctl auth google --device` with **no `--force` and no re-supplied secrets** (today it bails without both).
- **Prevention (REQUIRED):** publish the OAuth app to **Production** (verify the 7-day clock is actually gone for a `gmail.readonly` *unverified* app — the one caveat to confirm, not assume); update `agentctl auth google` output + `DEPLOYMENT.md`; **fix `MCP_SERVERS.md:119`** ("add your email as a Test user") which steers operators INTO the weekly-expiry trap; add an `invalid_grant` row to the error table.

## Api-key — instance (Brave / custom, wired)

- **Recovery vehicle:** operator updates the secret (`agentos.env` / the key file). The framework re-reads on next use and resumes. Surfaced instruction names the exact secret/env var to update. (No device flow — re-reading the updated secret IS the recovery.)

## Adjacent cred debts to close in the same PR
- **cred.3-ar-02** — surface `credential_refresh_failed` when no operator is attached (prerequisite for "operator sees the 2am failure").
- **cred.3.1-adv-01** — rotated refresh token lost on restart.
- **cred.5-ar-01** — error-body leak into `provider_health.last_error` (part of §8 redaction).

## Test plan (each fails without its fix)
- **Classifier (table-driven):** `(status, error_code)` → class. Assert `(429,rate_limit_exceeded)→transient`, `(401,invalid_client)→operator-config` (NOT reauth), `(400,invalid_grant)→reauth`, 400-non-JSON→unknown-fail-safe (transient), 500/timeout/DNS→transient. Separate: `401 on API call` → one silent refresh, not attention.
- **Dedup:** 3 concurrent Gmail calls hit `invalid_grant` → exactly one attention record + one approval.
- **Durability:** kill+restore agentd with a parked attention approval → exactly one approval after restore; provider still `AttentionRequired`; agent does NOT resume as authorized.
- **Resume-without-restart:** rewrite `google.json` under a running gateway → next refresh uses the new token, no restart, `AttentionRequired` clears, `CredentialRecovered` emitted. Same for an updated api-key secret.
- **Secret redaction:** token endpoint returns JSON with `refresh_token`+`client_secret` → assert logs, flight events, approval body, checkpoint contain no `ya29.`, no `1//…`, no `client_secret`.
- **MCP_TIMEOUT:** stub token endpoint to hang → tool call returns a classified `transient` error in < 25s (not agentd's 30s cancel).
- **Loopback gate:** headless mode → `oauth_start_auth` returns a device-flow redirect, no bound loopback server.
- **Recovery command:** `agentctl auth google --device` with an existing `google.json` and no `OAUTH_*` env → succeeds (reads client_id/secret from the file), no `--force`.
- **No retry storm:** terminal failure → the parked capability is not re-refreshed every turn (bounded).
- **Api-key:** invalid key → attention surfaced with re-supply instruction; update the secret → resume.

## Acceptance
- A revoked/expired Google refresh token → gateway classifies terminal, emits `CredentialAttentionRequired` + one p7.4 approval carrying the exact host device-flow command; the loop stays alive (non-Gmail work continues); no browser-callback dead-end.
- Operator runs `agentctl auth google --device` on the host (clean, no `--force`) → `google.json` rewritten → gateway picks it up on next refresh with NO restart → `CredentialRecovered` → Gmail resumes.
- A transient blip → bounded retry, no false attention prompt.
- An api-key provider with a bad key → attention with a re-supply instruction; updating the secret resumes it.
- Multi-agent → one attention record + one approval, never N.
- No token/secret in any log/event/approval/checkpoint.
- Every path has a test that fails without the fix.

## Non-goals
- In-container device flow (D1b) · container-writable token store (D3) · push notifications (ux.4 — separate) · a second OAuth provider's device flow (GitHub — follow-up, same seam) · Google app verification (operator/GCP task).

## Autoplan decision audit (2026-07-10)
| # | Decision | Classification | Basis |
|---|----------|----------------|-------|
| 1 | Layer = broker gateway (not Python sidecar) | User Challenge → user chose general-purpose | 4-voice consensus + provider-agnostic requirement |
| 2 | v1 wires both credential classes (OAuth + api-key) | User decision | user pick "framework + both classes" |
| 3 | D1 = host-driven; cut D1b + D3 | Auto (consensus) | reuse dx.4; in-container infeasible under MCP_TIMEOUT |
| 4 | Cut push (ux.4) | Auto (mechanical) | ux.4 does not exist; approval+event is MVP |
| 5 | 5-way classification on JSON error field | Auto (consensus) | invalid_client→config not reauth (infinite-loop guard); 429→transient |
| 6 | Provider-scoped `reauth:<provider>` dedup | Auto (consensus) | N-agents→N-prompts today |
| 7 | Redact endpoint body from logs/events/approval | Auto (security) | live leak at oauth_mcp.py:430, mod.rs:341,371 |
| 8 | Durability via checkpointed approval | Auto (consensus) | sidecar loses in-memory state on restart |
| 9 | Google Production-publishing prevention = required | Auto (P1/P2) | removes most failure volume; fix MCP_SERVERS.md trap |
| 10 | Recovery command reads secrets from google.json | Auto (DX) | today it fails without --force + env |
