<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/main-autoplan-restore-20260705-230323.md -->
# Phase 10 — Credential manager (`cred.*`)

**Status:** planned — architecture locked by 4-voice autoplan review (2026-07-05); pending
`/plan-eng-review` on the 5 gateway-mechanics questions before cred.3 implementation.

**Source:** first end-to-end Mac dogfooding + autoplan CEO/Eng/DX review. This phase
**subsumes dx.5** (the unified secrets/first-run work becomes `cred.1` + `cred.2`) and
extends it into a proper credential subsystem.

Goal: give AgentOS **one credential model across all surfaces**. Today every MCP server reads
its own secrets file or env, and the three surfaces (Docker `agent`, Docker `cos`, QEMU boot)
handle secrets differently. This phase makes one in-process broker own provisioning, the OAuth
lifecycle, per-agent capability scoping, and audit — generalizing the `EgressProxy` model-key
broker (p7.5b) to *all* credentials.

## Why (the root problem)

The dx.5 review found AgentOS has **no coherent credential model**:

- The same host dir `~/.agentos-secrets/` means "shell env file to source" in QEMU
  but "JSON credential store" in Docker; `ANTHROPIC_API_KEY` lives *inside* the dir in QEMU
  but comes from a different channel (compose `environment:`) in Docker.
- Docker `agent` mounts no secrets at all; `cos` does. Per-server direct file reads mean each
  new credentialed tool re-invents provisioning and re-introduces the same bugs.
- In-container OAuth binds `0.0.0.0` under `privileged` — a real exposure.
- `README.md` falsely claims the container "never sees your OAuth client credentials."
- There is **no per-agent scoping** of which credentials an agent may use, and **no audit**.

Patching per-surface (the narrow dx.5) fixes symptoms. The architectural fix is one component
that owns credentials for the whole system.

## Decision record (architecture locked — do not re-litigate)

The autoplan review surfaced an architectural conflict in the original plan: cred.3 described
**inject-at-spawn** (broker puts credentials into MCP subprocess env) and cred.4 described an
**egress gateway** (tools make unauthenticated calls; broker attaches auth). These are
competing models that would have migrated the Python MCP servers twice.

**Resolution: one model — the egress gateway. Inject-at-spawn is dropped.**

Reasons:

1. **Inject-at-spawn breaks on token expiry.** It puts a ~1-hour access token into the
   subprocess env. The CoS runs longer; you can't re-inject into a live subprocess. The only
   fixes are: inject the durable refresh token (tool holds the durable credential — defeats the
   broker), or have the tool pull from the broker on expiry (a proxy in disguise). Neither is
   clean. The gateway sidesteps this entirely — it holds the token and refreshes transparently
   on every forwarded call.

2. **The gateway extends what already exists.** `EgressProxy` (p7.5b, `agentd/src/egress.rs`)
   is already a loopback forward proxy that swaps an ephemeral key for the real
   `ANTHROPIC_API_KEY`. The credential gateway generalizes it: attach OAuth Bearer / API-key
   headers for other upstreams. One pattern, not a parallel env-injection mechanism.

3. **It is the audit + budget chokepoint.** A proxy that sees every outbound call is where
   you log it, enforce per-agent upstream spend caps, and block denied providers. Inject-at-spawn
   gives none of that visibility once the token is in the subprocess.

## The model

One in-process broker (`CredentialGateway`, extending `EgressProxy`) that is also the egress
gateway:

- **Tools make credential-free calls** to a broker loopback endpoint (e.g.
  `http://127.0.0.1:PORT/google/gmail/v1/users/...`); the broker looks up the right credential
  for the calling agent's capabilities, attaches auth (OAuth Bearer / API-key header /
  query-param — per-provider adapter), and forwards to the real upstream. Tools never hold a
  raw credential — durable or access.
- **Broker owns:** provisioning ingest (`/run/secrets`), OAuth refresh/rotation (transparent to
  tools; solves token expiry for long-running agents), per-agent `Capability::Credential`
  enforcement, and audit + budget on every forwarded call.
- **Two-tier storage:**
  - Provisioning input (`:ro`): `~/.agentos-secrets/` — `agentos.env` + `<provider>.json`.
    Never written back to.
  - Runtime state (writable): `/data/state/oauth/` (Docker named volume) /
    memory-volume-backed path (QEMU) — rotated refresh tokens + access-token cache.
    Exact QEMU backing is an `/plan-eng-review` question (see below).

## Key design decisions

### D1 — In-process native subsystem in agentd, NOT a separate sidecar
The broker is a component inside `agentd` (loopback-exposed), extending `EgressProxy`.
- **Rationale:** locked decisions (super-light; agents in-process; single trust domain). It
  needs the scheduler's per-agent `Capability` state to enforce scoping — a sidecar would need
  IPC. It runs identically whether agentd is a Docker process or PID 1 in a QEMU VM.
- **When a sidecar would be reconsidered:** multiple agentd *instances* sharing one broker —
  `orch.*` territory, deferred.

### D2 — Egress gateway is the single credential delivery model
The broker's gateway role sits between a tool and the *upstream API it calls*.

- Tools make **unauthenticated** outbound calls through the broker's loopback; the broker
  attaches the credential, forwards, and logs. After cred.3 a tool process holds **no raw
  credential** — not in env, not in memory-at-rest.
- This is a generalization of `EgressProxy` (p7.5b), not a new pattern. The same
  loopback-proxy + ephemeral-identity machinery is reused.
- inject-at-spawn is **not used**. The Python MCP servers (`oauth_mcp.py`, `http_mcp.py`,
  `search_mcp.py`) are migrated **once** in cred.3 to make credential-free calls to the broker.
  cred.4 is additive on top — no re-migration.
- Distinct from an **MCP aggregator** — out of scope.

### D3 — Two-tier storage
- **Provisioning input (`:ro`):** `~/.agentos-secrets/` → `/run/secrets` on every surface.
  Never written back to. Contains `agentos.env` + `<provider>.json`.
- **Runtime state (writable):** `/data/state/oauth/<provider>.json` (Docker); QEMU path TBD
  at `/plan-eng-review` (memory volume vs new state mount). Atomic write (tmp→rename).
  Stores rotated refresh tokens + access-token cache with expiry timestamp.
- No `CredentialStore` trait in cred.3 — concrete file-backed structs only. Trait introduced
  in cred.5 if a second backend (keychain/vault) is added.

### D4 — Per-agent capability scoping + audit
- `Capability::Credential { provider: CredentialProvider }` where `CredentialProvider` is an
  enum (`Google`, `BraveSearch`, `Custom(String)`) — not a bare `String`. Unknown providers
  fail at config-load time with a clear message.
- Broker enforces on every forwarded call: if the calling agent's capability set does not
  include `Credential { provider }` for the upstream being proxied, return 403 and emit a
  `credential_denied` flight event.
- Flight events: `credential_issued` / `credential_denied` / `credential_refreshed` /
  `credential_refresh_failed` / `credential_egress_brokered`. Consistent with
  `egress_brokered` (p7.5b). `credential_refresh_failed` must be surfaced visibly — the agent
  will otherwise see "no tools available" or cryptic API errors, not "re-authorize Google."

## Architecture

```
  host: agentctl auth google  ────────────────────── (provisioning, one-time)
                               │
      ~/.agentos-secrets/ {agentos.env, google.json}
                               │  :ro mount
                               ▼
                          /run/secrets
                               │
   agentd ─────────────────────┼──────────────────────────────────────────────
     ┌────────────────────────────────────────────────────────────────────┐   │
     │  CredentialGateway  (agentd/src/credential/; wired from egress.rs) │◀──┘
     │   • ingest /run/secrets at startup; validate schema                │
     │   • OAuth lifecycle: refresh/rotate → /data/state/oauth/ (rw)      │
     │   • per-agent Capability::Credential enforcement                    │
     │   • audit + budget on every forwarded call (flight events)          │
     │   • per-provider adapter (host/path map + auth attachment)          │
     └────────────────────────────┬───────────────────────────────────────┘
                                  │  loopback  (http://127.0.0.1:PORT/…)
                  ┌───────────────┘
                  │  credential-free call
                  ▼
          oauth_mcp / http_mcp / search_mcp
          (no credential in env or memory-at-rest)
                  │  broker attaches auth header, forwards
                  ▼
          upstream API  (Google, Brave, …)
```

## Increments (each shippable; `cred.1` first)

### ▣ cred.1 — Immediate unblock (secrets mount + README fix)
**Depends on:** nothing.
**Goal:** the google-agent runs on a clean Apple-Silicon Mac via host-auth.
**Scope:**
- `docker-compose.yml`: add `${HOME}/.agentos-secrets:/run/secrets:ro` to the `agent` service.
- `README.md`: fix the false "container never sees your OAuth client credentials" claim.
- Fail-fast preflight when `/run/secrets/google.json` is absent for OAuth templates.
**Acceptance:** clean Apple-Silicon Mac runs `docker compose` scout **and** google-agent via
`agentctl auth google`, no manual patching.
**Status:** ✅ shipped (v0.58.0, PR #80).

### ▣ cred.2 — Unified secrets substrate
**Depends on:** cred.1.
**Goal:** one host-OS-neutral credentials story across Docker `agent`, Docker `cos`, and QEMU.
**Scope:**
- Docker entrypoint parses `/run/secrets/agentos.env` with a safe KEY=value parser (no shell
  sourcing; denylist for `BASH_ENV`, `LD_PRELOAD`, etc.) so `ANTHROPIC_API_KEY` channel matches
  QEMU.
- QEMU 9p secrets mount made read-only (`distro/Makefile`).
- Deprecate + gate in-container OAuth (strip `OAUTH_*` from `agent` compose block; record
  `0.0.0.0` bind in `THREAT_MODEL.md`).
- Rewrite stale `RUNBOOK.md`: one "Credentials & first run" section; de-Mac strings.
**Acceptance:** one credentials story on all three surfaces; `ANTHROPIC_API_KEY` from
`agentos.env` everywhere; no misleading docs. Tests: schema-drift guard, entrypoint DRY_RUN
smoke, `agentos.env` source-safety, `macos-latest` CI build for `agentctl`.
**Status:** ✅ shipped (v0.59.0, PR #81).

### ⚠️ /plan-eng-review gate (before cred.3)
Resolve the five gateway-mechanics questions below before writing broker code. These are
implementation decisions with non-trivial trade-offs; the build session should not guess.

### ▣ cred.3 — Credential broker as egress gateway
**Depends on:** cred.2, `/plan-eng-review` on the five questions below.
**Goal:** one in-process broker owns provisioning, OAuth lifecycle, per-agent scoping, audit,
and upstream forwarding. Tools never hold a raw credential.
**Scope:**
- `CredentialGateway` in `agentd/src/credential/` (new module; `egress.rs` wires it):
  ingest `/run/secrets`; OAuth refresh/rotate → `/data/state/oauth/`; per-agent
  `Capability::Credential` enforcement; TOML config-driven per-provider adapters; audit flight
  events (budget enforcement deferred to cred.4). Second OS-assigned loopback listener (same
  pattern as `EgressProxy`). Header scrubbing: strip caller-supplied `Authorization`, `Host`,
  and credential headers before attaching broker auth.
- `Capability::Credential { provider: CredentialProvider }` in `agentd/src/capability.rs`.
  `CredentialProvider` enum: `Google`, `BraveSearch`, `Custom(String)`. Unknown provider →
  config-load error.
- `tokio::sync::RwLock` for credential state (not `std::sync::RwLock` — would block the async
  runtime on write paths).
- Gateway loopback port managed alongside the existing management API port
  (`agentd/src/management.rs`), or a separate listener — TBD at eng-review.
- **Migrate Python MCP servers once:** `oauth_mcp.py` and `search_mcp.py` rewritten to make
  credential-free calls to the broker loopback (`http://127.0.0.1:PORT/<provider>/...` with
  `x-credential-token` header). `http_mcp.py` excluded — it has no credential path to migrate.
  `passenv` entries for credential vars (`OAUTH_REFRESH_TOKEN`, `BRAVE_SEARCH_API_KEY`, etc.)
  removed from all affected templates as part of this PR — `PASSENV_BLOCKLIST` extended.
- `credential_refresh_failed` flight event must produce a visible agentctl alert, not a cryptic
  "no tools available" downstream error.
- `DRY_RUN_ONLY=1` must remain trustworthy: broker not yet active in dry-run; rendered TOML
  shows tool config, not credential injection.
**Acceptance:** a tool process holds no raw credential (durable or access) in env; every
upstream call to a credentialed provider is authenticated and audited at the broker (`credential_egress_brokered`
flight event); a capability-denied provider is blocked with a 403 and `credential_denied` event;
access-token expiry is handled transparently (agent running longer than token TTL keeps working);
caller-supplied `Authorization`/`Host` headers are stripped before broker auth is attached;
`cargo test --workspace` passes (target ≥ 1110+).

### ▣ cred.4 — Additive hardening (no re-migration)
**Depends on:** cred.3.
**Goal:** more providers, budget policies, and gateway hardening on top of the model shipped in
cred.3. No Python MCP server changes needed — they're already credential-free.
**Scope:**
- Additional provider adapters (e.g. GitHub, Stripe, Notion — driven by new harness MCP
  servers in h8.*).
- Per-agent upstream spend cap enforcement (budget model TBD at eng-review).
- Gateway hardening: redirect-following disabled, response body size cap, connection timeout,
  adversarial header scrubbing.
- `THREAT_MODEL.md` section for the gateway (single component holds all credentials; loopback
  surface; refresh-token rotation story).
**Acceptance:** new providers work with no migration burden; spend caps enforced; hardening
tests pass.

### ▣ cred.5 — Surfacing + lifecycle polish (optional)
**Depends on:** cred.3.
**Goal:** operator visibility + advanced lifecycle.
**Scope:** `/agents/credentials` FUSE view + `agentctl` credential pane (which agent may use
which provider; last access; token expiry countdown); rotation policy config; `CredentialStore`
trait introduced if a second backend (OS keychain / vault) is added.
**Acceptance:** operator can see per-agent credential grants + last access; rotation policy
configurable.

## Eng-review focus — LOCKED (resolved by `/plan-eng-review` 2026-07-06)

All five questions answered. No TBD remains before cred.3 implementation.

1. **How tools reach the gateway — locked:** Base-URL rewrite. Inject
   `AGENTD_CREDENTIAL_GATEWAY_URL=http://127.0.0.1:<PORT>` and
   `AGENTD_CREDENTIAL_TOKEN=<ephemeral-uuid>` via `extra_env` in `McpClient::spawn()`.
   Not credentials — service discovery + identity token only. HTTP_PROXY dropped (requires TLS
   interception).

2. **Per-provider adapter shape — locked:** TOML config-driven.
   `[credential_gateway.providers.<name>]` table with:
   - `auth_style`: `"oauth-bearer"` | `"api-key-header"` | `"api-key-query"`
   - `upstream_base`: e.g. `"https://gmail.googleapis.com"`
   - `header_name`: (required for `api-key-header`) e.g. `"X-Subscription-Token"`
   - `secret_key`: which key from `agentos.env` holds the credential (for non-OAuth providers)
   Config-load rejects unknown `auth_style` values and validates required fields.
   `CredentialProvider::Custom("foo")` requires a matching `[credential_gateway.providers.foo]`
   entry — missing entry is a config-load error, not a runtime error.

3. **Token cache + QEMU backing — locked:** Reuse existing memory volume.
   QEMU: `/run/memory/state/oauth/<provider>.json`. Docker: `/data/state/oauth/<provider>.json`.
   Format: `{"access_token": "...", "refresh_token": "...", "expiry_ts": 1234567890, "scopes": [...]}`.
   Refresh if `expiry_ts - now() < 300s`. Concurrent refresh: one `tokio::sync::Mutex<Option<
   JoinHandle>>` per provider. **Critical:** if Google rotates the refresh token, atomic write
   the new token BEFORE returning the access token; emit `credential_refresh_failed` on write
   failure even if current access token is still valid.

4. **Budget model — locked:** Enforcement deferred to cred.4. cred.3 ships
   `credential_egress_brokered` flight event (`agent_id`, `provider`, `path`, `response_status`,
   `response_bytes`). Call count visible in flight log. No enforcement in cred.3.

5. **Dev escape hatch — locked:** No bypass flag. Gateway returns HTTP 503 with JSON:
   `{"error": "credential_not_provisioned", "provider": "google", "hint": "Run: agentctl auth google ..."}`.
   MCP server propagates as `is_error` tool result. `DRY_RUN_ONLY=1`: gateway not started;
   dry-run trustworthy.

**Additional architecture decisions locked by `/plan-eng-review`:**

- **Module home:** `agentd/src/credential/` (new module), not inline in `egress.rs`.
  `egress.rs` wires and re-exports; stays as the model-key proxy only.
- **Gateway listener:** Second OS-assigned loopback `TcpListener`. Same dynamic-port pattern
  as `EgressProxy`. Clean separation from model-key proxy and management API (port 7999).
- **Agent identity:** Ephemeral token per MCP spawn (UUID4 registered in a credential registry,
  deregistered on process exit). Gateway reads `x-credential-token` header, maps to agent ID.
  Same pattern as the model-key proxy.
- **Header scrubbing in cred.3:** Strip caller-supplied `Authorization`, `Host`, credential
  headers (`X-Subscription-Token`, etc.) from incoming requests before attaching broker auth.
  Blocklist of ~5 headers stripped in `handle_request()`. Not deferred to cred.4.
- **http_mcp.py excluded:** `http_mcp.py` has no credential path to migrate. cred.3 touches
  only `oauth_mcp.py` and `search_mcp.py`.
- **`tokio::sync::RwLock`:** Applies to both new credential state AND the existing
  `ProxyRegistry` (`egress.rs:89`) — both converted in the same PR.

## How it subsumes dx.5
dx.5's three findings (`mac-df-01/02/03`) map onto **cred.1 + cred.2**. dx.5 is not shipped
separately. The original `mac-df-02` fix (add `OAUTH_CALLBACK_PORT` to the shared template) is
**dropped** — it polishes the in-container OAuth path that cred.2 deprecates.

## Cross-surface behavior
The broker is in-process and runs identically on Docker `agent`, Docker `cos`, and QEMU boot.
Provisioning path (`/run/secrets`) and token-cache path are the same on all surfaces (backed
by the appropriate volume per surface). No arch-conditional code; works on x86_64 and aarch64.

## Security / threat model
- **Single component holds all credentials.** Single-tenant + mutually-trusting agents bound
  the blast radius. Broker stays in-process (no new network surface beyond existing loopback).
  Never logs secrets. Never writes to a container image layer.
- **Least privilege between agents:** `Capability::Credential` means a compromised or
  over-eager agent can only reach providers its capability set grants. Every attempt — granted
  or denied — is audited via flight events.
- **Tools never hold raw credentials** (durable or access) after cred.3. The egress gateway is
  the only component that ever sees a live credential.
- **`:ro` provisioning + separate writable runtime state** prevents tools from mutating the
  source of truth.
- New `THREAT_MODEL.md` section covers: broker as single credential holder; loopback gateway
  surface; refresh-token rotation story; deprecated in-container OAuth.

## Invariant to preserve

**After cred.3, MCP Python servers hold no direct credential references.** Subsequent
provider additions, credential path changes, or budget enforcement in cred.4+ must be broker-side
changes only — no MCP server touches required. If a later increment proposes modifying an MCP
server's credential handling, the broker abstraction has leaked.

## Out of scope
- MCP **aggregator** (agent → single MCP → downstream MCPs) — separate feature.
- Multi-instance shared broker / remote credential service — `orch.*`.
- Exotic auth (mTLS, SAML) beyond OAuth2 + API-key; secret *generation* / cloud STS — future.
- inject-at-spawn — **explicitly dropped** (see decision record above).

## Relationship to existing code
- New `agentd/src/credential/` module: `CredentialGateway`, `OAuthTokenCache`, TOML provider
  config structs, TOML-driven adapter engine, ephemeral credential token registry. `egress.rs`
  wires it; `ProxyRegistry` and credential state both converted to `tokio::sync::RwLock`.
- Extends `agentd/src/capability.rs`: new `Credential { provider: CredentialProvider }` variant
  + `caps_to_rules()` wiring.
- Migrates `docker/oauth_mcp.py` and `docker/search_mcp.py` to credential-free calls (cred.3).
  `docker/http_mcp.py` excluded (no credential path). No further MCP server changes in cred.4+.
- Removes `passenv` entries for credential vars from all affected templates (same PR as cred.3).
- New flight events per `docs/CONVENTIONS.md`; `THREAT_MODEL.md` section.

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 1 | CLEAR | 4-voice autoplan; resolved inject-at-spawn vs egress-gateway conflict; locked egress-only model |
| Codex Review | `/codex review` | Independent 2nd opinion | 1 | CLEAR | 13 findings; plan doc updated; header scrubbing added to cred.3; 3 TODOs filed |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | CLEAR | 10 decisions locked; 1 critical gap (QEMU atomic write on token rotation) mitigated; 22 test gaps identified |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | — | not applicable (no new UI surfaces in cred.3) |
| DX Review | `/plan-devex-review` | Developer experience gaps | 1 | CLEAR | DX phase completed in autoplan; structured 503 error + no bypass flag locked |

**VERDICT:** CEO + ENG CLEARED — ready to implement cred.3.

NO UNRESOLVED DECISIONS
