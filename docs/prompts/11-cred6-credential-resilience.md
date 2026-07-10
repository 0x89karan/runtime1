# 11 — cred.6 Credential resilience — build-session kickoff

Paste the block below. Full plan (read it first): `docs/plans/cred.6-credential-resilience.md`.
Autoplan-hardened (4 voices) — build to the plan; the hardening items are the point, not extras.

---

```
TASK: cred.6 — Credential resilience (general-purpose refresh/credential-failure recovery)
Full plan (READ FIRST): docs/plans/cred.6-credential-resilience.md

SEPARATE — DO THIS FIRST / NOT PART OF cred.6:
The live CoS "not authenticated" failure is the FsRead sandbox blocker, ALREADY FIXED on main as
commit 4a0951a4. It's a STALE IMAGE, not new work — republish/re-pull agentos:full built from
4a0951a4 and the CoS authenticates silently. Also: there is a LIVE SECRET LEAK worth a standalone
fix now — the refresh error path forwards the raw token-endpoint body to the agent + logs + flight
events (oauth_mcp.py:430, credential/mod.rs:341,371). cred.6 §8 redacts it; land that redaction
even ahead of the rest if you split the work.

WHY (the honest gap — the seed overstated it)
When a broker credential's refresh/validation TERMINALLY fails (revoked/expired/key-rotated/deleted
client), the agent gets an opaque error, keeps burning cron cycles, the operator gets NO signal, and
NOTHING resumes after the credential is fixed. There is no automatic loopback "dead-end" — the gap is
no terminal detection, no operator surfacing, no resume, and (multi-agent) no de-dup.

LOCKED DECISIONS (autoplan 2026-07-10 — do not relitigate)
- LAYER = the Rust broker gateway (credential/mod.rs get_or_refresh), PROVIDER-AGNOSTIC. NOT the
  Python oauth_mcp.py (Gmail-specific; dead code in broker mode). General-purpose was the explicit call.
- v1 wires BOTH credential classes: OAuth-refresh (Google = instance #1, device-flow recovery) AND
  api-key (Brave/custom = re-supply the secret + resume).
- D1 = HOST-DRIVEN recovery (reuse dx.4 `agentctl auth google --device`). CUT D1b (in-container device
  flow) + D3 (container-writable token store) + push (ux.4 doesn't exist; approval + flight event = MVP).
- PRECONDITION (part of cred.6): move the CoS to broker mode — add [credential_gateway] + a
  Credential{Google} grant to agentd/cos.agents.toml + distro/overlay/etc/agentd/cos.agents.toml.

THE GENERAL FRAMEWORK (in the gateway) — each item ships with a test that FAILS without it:
1. Classification (RFC 6749, on the JSON `error` field, NEVER HTTP status class alone) → 5-way:
   Transient (5xx/429/rate_limit/slow_down/network/DNS/timeout → retry+backoff) ·
   UserReauthRequired (invalid_grant → re-auth) ·
   OperatorConfigRequired (invalid_client/unauthorized_client/invalid_scope → FIX CONFIG, distinct
     message — re-auth will NOT fix it; routing to reauth = infinite operator loop) ·
   ApiKeyInvalid (401/403, no refresh → supply new key) ·
   Unknown → Transient (fail toward retry, never toward spamming the operator).
   Also distinguish 401-on-API-call (access token → refresh+retry once) vs failure-on-refresh
   (refresh token → attention).
2. Per-provider state: Healthy | TransientRetry{until} | AttentionRequired{reason, recovery_kind,
   approval_id, detected_at}.
3. Durability: anchor AttentionRequired to a CHECKPOINTED p7.4 approval (sidecar/gateway loses
   in-memory state on restart). Restore before spawning MCPs. FORMAT_VERSION bump + serde(default) if a
   gateway flag is added.
4. De-dup: provider-scoped idempotency key `reauth:<provider>` in ApprovalStore (collapse duplicates);
   gateway returns an attention sentinel so agents 2..N don't each open an approval. (Today 3 inbox
   agents → 3 prompts.)
5. Surface: ONE coalesced p7.4 approval with a per-provider recovery instruction; flight events
   CredentialAttentionRequired / CredentialRecovered; bounded escalating re-notify (0, then daily),
   NEVER per-turn; park only the affected capability (rest of the agent keeps working); do NOT
   re-refresh a terminal failure every turn (no retry storm / budget guard).
6. Resume: fresh-read the credential source (token_path/state_path) by mtime/version on next use,
   invalidating the stale in-memory token, so a host-updated google.json / updated api-key secret is
   picked up with NO restart → clear AttentionRequired, emit CredentialRecovered.
7. Retry vs MCP_TIMEOUT=30s: in-call retry ≤1 with per-request timeout ~8-10s (whole call <25s);
   longer backed-off retries ACROSS tool calls or in the gateway's own refresh path. Never let a retry
   get guillotined and misreported as a generic timeout.
8. Secrets: redact before ANY log/event/approval/tool-response — strip access_token/refresh_token/
   id_token/client_secret/authorization + long bearer-looking strings; emit only the classified enum +
   safe hint. (Fixes the live leak above.)
9. No-loopback gate: headless/broker mode → oauth_start_auth returns a device-flow redirect (like
   broker_managed), never a bound loopback server. Concrete gate, not a behavioral hope.

GOOGLE INSTANCE #1 (OAuth-refresh, fully wired)
- Recovery: host device flow. Surfaced instruction = "on your Mac HOST (not the container), run
  `agentctl auth google --device`".
- FIX `agentctl auth google --device`: read client_id/secret from the EXISTING google.json when
  env/flags absent; overwrite-in-place atomically (.bak); no --force needed (today it bails without
  --force AND OAUTH_* env even though google.json already has them).
- PREVENTION (REQUIRED, biggest lever): publish the OAuth app to Production; VERIFY the 7-day clock is
  gone for a gmail.readonly UNVERIFIED app (confirm, don't assume); update `agentctl auth google`
  output + DEPLOYMENT.md; FIX MCP_SERVERS.md:119 ("add your email as a Test user" steers INTO weekly
  expiry); add an invalid_grant row to the error table.

API-KEY INSTANCE (Brave/custom): recovery = operator updates the secret; framework re-reads + resumes;
surfaced instruction names the exact secret/env var. No device flow.

ADJACENT CRED DEBTS to close in the same PR: cred.3-ar-02 (surface credential_refresh_failed with no
operator attached), cred.3.1-adv-01 (rotated refresh token lost on restart), cred.5-ar-01 (error-body
leak into provider_health.last_error).

NON-NEGOTIABLE: every code item = fix + a test that FAILS without it + adversarial verification. Loop
never panics on a credential failure. Secrets never logged/written by the runtime. Linux-gated code
(sandbox/caps/broker) → `make clippy-linux` before push. Update ROADMAP.md (cred.6), CONVENTIONS.md
(new event kinds), THREAT_MODEL.md (§8 redaction + reauth surfacing) in the same PR. Test plan is in
the plan doc — implement all of it. /plan-eng-review is already done (this autoplan); go
build → /review → /qa → /ship.

DONE = a revoked/expired Google refresh token → gateway classifies terminal, emits
CredentialAttentionRequired + ONE p7.4 approval with the exact host device-flow command; the loop
stays alive (non-Gmail work continues); operator runs `agentctl auth google --device` on the host
(clean, no --force) → google.json rewritten → gateway picks it up on next refresh with NO restart →
CredentialRecovered → Gmail resumes. Transient blip → bounded retry, no false prompt. Bad api-key →
attention + re-supply instruction → update secret → resume. Multi-agent → one approval, never N. No
token/secret in any log/event/approval/checkpoint. All test-plan cases green.
```
