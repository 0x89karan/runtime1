# Phase 10 — Credential manager (`cred.*`)

**Status:** planned (this doc; ready for `/plan-eng-review`).
**Source:** first end-to-end Mac dogfooding (2026-07-05) + a 4-voice CEO/Eng review of the
secrets model. This phase **subsumes dx.5** (the unified secrets/first-run work becomes
`cred.1` + `cred.2`) and extends it into a proper credential subsystem.

Goal: give AgentOS **one credential model across all surfaces**. Today every MCP server reads
its own secrets file or env, and the three surfaces (Docker `agent`, Docker `cos`, QEMU boot)
handle secrets differently. This phase makes one in-process broker own provisioning, the OAuth
lifecycle, per-agent capability scoping, and audit, so tools become credential-agnostic —
generalizing the `EgressProxy` model-key broker (p7.5b) to *all* credentials.

## Why (the root problem)

The dx.5 review found AgentOS has **no coherent credential model**:

- The same host dir `~/.agentos-secrets/` means "shell env file to source" in QEMU
  (`distro/overlay/init` sources `agentos.env`) but "JSON credential store" in Docker
  (`oauth_mcp.py` reads `google.json`); `ANTHROPIC_API_KEY` lives *inside* the dir in QEMU
  but comes from a *different* channel (compose `environment:`) in Docker.
- Docker `agent` mounts no secrets at all; `cos` does. Per-server direct file reads mean each
  new credentialed tool re-invents provisioning and re-introduces the same bugs.
- In-container OAuth binds `0.0.0.0` under `privileged` — a real exposure.
- `README.md` falsely claims the container "never sees your OAuth client credentials."
- There is **no per-agent scoping** of which credentials an agent may use, and **no audit**.

Patching per-surface (the narrow dx.5) fixes symptoms. The architectural fix is one component
that owns credentials for the whole system.

## Thesis

`EgressProxy` / `ProxyRegistry` (`agentd/src/egress.rs`, p7.5b) already brokers the model key —
handing each workload an *ephemeral* key and swapping it for the real `ANTHROPIC_API_KEY` at the
boundary. This phase generalizes that broker from the model key to *all* credentials
(third-party API keys + OAuth client creds/refresh/access tokens).

## Key design decisions

### D1 — In-process native subsystem in agentd, NOT a separate sidecar
The broker is a component inside `agentd` (loopback-exposed), extending `EgressProxy`.
- **Rationale:** locked decisions (super-light; agents in-process; single trust domain). It
  needs the scheduler's per-agent `Capability` state (`agentd/src/capability.rs`) to enforce
  scoping — a sidecar would need IPC. It must run identically whether agentd is a Docker
  process or PID 1 in a QEMU VM; an in-process component is automatically present on every
  surface. The `agentos-otel` sidecar is separate only because it exports *outward*; the broker
  is on the hot path and stays in-process.
- **When a sidecar would be reconsidered:** multiple agentd *instances* sharing one broker —
  `orch.*` territory, deferred.

### D2 — The "MCP gateway" is at the EGRESS boundary (authenticating proxy)
The broker's gateway role sits between a tool and the *upstream API it calls*, not between the
agent and its tools.
- Tools make **unauthenticated** outbound calls through the broker's loopback proxy; the broker
  attaches the credential (OAuth bearer / API-key header) selected by the calling agent's
  capabilities, then forwards. Generalizes `EgressProxy` (p7.5b) + p7.5 boundary secret
  rewriting. After this a tool **never holds a raw credential**.
- Distinct from an **MCP aggregator** (agent → one MCP → many downstream MCP servers), which is
  a separate "simplify the tool surface" feature — **out of scope** and not a credential fix.

### D3 — Two-tier storage: read-only provisioning input + writable runtime state
- **Provisioning input (read-only):** `~/.agentos-secrets/` stays the one host-provisioned store
  — `agentos.env` (process env incl. `ANTHROPIC_API_KEY`) + `<provider>.json` (OAuth client
  creds + refresh token). Mounted `:ro` on every surface.
- **Runtime state (writable):** rotated refresh tokens + access-token cache go to a defined
  writable path (`/run/state/oauth`, backed by `/data` in Docker and the memory volume in QEMU)
  — **never** back into `:ro` `/run/secrets`. Resolves the refresh-under-`:ro` edge.
- **Pluggable backend behind a `CredentialStore` trait** (mirroring `InferenceGateway`):
  file-backed now; OS keychain / external vault later without touching the core.

### D4 — Per-agent capability scoping + audit
- Extend `agentd/src/capability.rs` with a credential scope (e.g. `Capability::Credential {
  provider }`). An agent may use only the providers its capabilities grant; the broker enforces
  on every issue/inject/egress call.
- Every access emits a flight event (`credential_issued` / `credential_denied` /
  `credential_refreshed` / `credential_egress_brokered`), consistent with record-everything and
  the existing `egress_brokered` event.

## Architecture

```
  host: agentctl auth google  ─┐        (provisioning, one-time, host-OS-neutral)
                               ▼
      ~/.agentos-secrets/ {agentos.env, google.json}   ── :ro mount ──▶ /run/secrets
                                                                            │
   agentd (single process; PID 1 in QEMU) ───────────────────────────────┐ │
     ┌───────────────────────────────────────────────────────────────┐   │ │
     │  CredentialBroker  (extends EgressProxy, loopback)             │◀──┘ │
     │   • ingest provisioning (/run/secrets)                          │     │
     │   • OAuth lifecycle: refresh/rotate → /run/state/oauth (rw)     │     │
     │   • per-agent capability check + audit (flight events)          │     │
     │   • CredentialStore trait (file | keychain | vault)             │     │
     └───────────────┬───────────────────────────┬───────────────────┘     │
        inject-at-spawn (cred.3)         egress gateway (cred.4)             │
                     │                             │                          │
             MCP server env                unauth outbound call              │
             (scoped subset)                      │                          │
                     ▼                            ▼                          │
             oauth_mcp / http_mcp ──────▶ broker attaches cred ──▶ upstream API
```

## Increments (each shippable; `cred.1` first)

### ▣ cred.1 — Immediate unblock (secrets mount + README fix)
**Depends on:** nothing (near-term; ship first despite the phase number — this is the
"test it today" increment).
**Goal:** the google-agent runs on a clean Apple-Silicon Mac via host-auth.
**Scope:**
- `docker-compose.yml`: add `${HOME}/.agentos-secrets:/run/secrets:ro` to the `agent` service
  (mirror `cos`).
- `README.md`: fix the false "container never sees your OAuth client credentials" claim.
- `mkdir -p ~/.agentos-secrets` guidance + fail-fast preflight when `/run/secrets/google.json`
  is absent for OAuth templates.
**Acceptance:** clean Apple-Silicon Mac runs `docker compose` scout **and** google-agent via
host-auth (`agentctl auth google`), no manual patching.

### ▣ cred.2 — Unified secrets substrate
**Depends on:** cred.1.
**Goal:** one host-OS-neutral credentials story across Docker `agent`, Docker `cos`, and QEMU.
**Scope:**
- Docker entrypoint sources `/run/secrets/agentos.env` if present (guarded `set -a; . file;
  set +a`, before `check_api_key`, no clobber of compose env) so the `ANTHROPIC_API_KEY` channel
  matches QEMU.
- Make the QEMU 9p secrets mount read-only (`distro/Makefile`).
- Deprecate + gate in-container OAuth (strip `OAUTH_*` from the `agent` compose block; stderr
  deprecation notice; record the `0.0.0.0` bind in `THREAT_MODEL.md`).
- Rewrite the stale `RUNBOOK.md` (v0.20.0 header): one "Credentials & first run" section naming
  both dirs (`~/.agentos-secrets` = provisioned input; `/run/state/oauth` = runtime cache) and
  per-surface formats; de-Mac the strings.
**Acceptance:** the one credentials story works on all three surfaces; `ANTHROPIC_API_KEY` comes
from `agentos.env` everywhere; no misleading docs remain. Tests: writer/reader schema-drift
guard, entrypoint DRY_RUN smoke, `agentos.env` source-safety, `macos-latest` CI build for
`agentctl`.

### ▣ cred.3 — Broker core (the credential manager)
**Depends on:** cred.2, `/plan-eng-review` on the open questions below.
**Goal:** one in-process broker owns provisioning, OAuth lifecycle, scoping, and audit.
**Scope:**
- `CredentialBroker` in `agentd/src/egress.rs` (extend `EgressProxy`): ingest provisioning;
  OAuth refresh/rotation → `/run/state/oauth`; `CredentialStore` trait (file backend).
- `Capability::Credential { provider }` + enforcement + audit flight events.
- **Inject-at-spawn:** when agentd spawns an MCP server, the broker supplies only the
  credentials that agent's capabilities allow — replacing per-server direct file reads and the
  ad-hoc `passenv`/`extra_env` path in `agentd/src/tools/mcp.rs`. Existing tools keep working.
**Acceptance:** MCP servers get credentials via the broker with per-agent scoping + audit; a
capability-denied agent cannot obtain a provider credential. Broker unit tests + inject-at-spawn
integration test.

### ▣ cred.4 — Egress gateway (the "MCP of MCP servers")
**Depends on:** cred.3.
**Goal:** tools never hold raw credentials.
**Scope:**
- Authenticating egress proxy: tools call upstream APIs through the broker unauthenticated; the
  broker attaches the credential and forwards.
- Rewrite `oauth_mcp.py` / `http_mcp.py` / `search_mcp.py` to be credential-agnostic.
**Acceptance:** a tool process contains no raw credential in its env or memory-at-rest; outbound
calls are authenticated at the broker; a denied provider is blocked. Egress-proxy tests +
raw-cred-never-in-tool-env assertion.

### ▣ cred.5 — Surfacing + hardening (optional)
**Depends on:** cred.3 (cred.4 optional).
**Goal:** operator visibility + lifecycle polish.
**Scope:** `/agents/credentials` FUSE view + `agentctl` credential pane (which agent may use
which provider; last access; token expiry); rotation policy; alternate `CredentialStore`
backends (OS keychain / vault).
**Acceptance:** operator can see per-agent credential grants + last access via FUSE and
`agentctl`; rotation policy configurable.

## How it subsumes dx.5
dx.5's three findings (`mac-df-01/02/03`) map onto **cred.1 + cred.2** — the substrate the broker
sits on. dx.5 is not shipped separately; its near-term unblock is preserved as `cred.1`. The
original `mac-df-02` fix (add `OAUTH_CALLBACK_PORT` to the shared template) is **dropped** — under
this design it polishes the in-container OAuth path that cred.2 deprecates.

## Cross-surface behavior
Because the broker is in-process, it is identical on Docker `agent`, Docker `cos`, and QEMU boot.
Provisioning (`~/.agentos-secrets`) and runtime state (`/run/state/oauth`) use the same paths on
all three (backed by the appropriate volume/mount per surface). No arch-conditional code
(respects "architecture never leaks into the core"); works on x86_64 and aarch64.

## Security / threat model
- **Single component holds all credentials.** Single-tenant + mutually-trusting agents bound the
  blast radius, but it raises the stakes for that component: it stays in-process (no new network
  surface beyond the existing loopback proxy), never logs secrets, never writes secrets to a
  container image layer.
- **Least privilege between agents:** capability scoping means a compromised/over-eager agent
  can only reach the providers it was granted; every attempt is audited.
- **Egress gateway (cred.4)** removes raw credentials from tool processes entirely.
- **`:ro` provisioning + separate writable runtime state** prevents tools from mutating the
  source of truth.
- New/updated `THREAT_MODEL.md` section for the broker + the deprecated in-container OAuth bind.

## Open questions (for `/plan-eng-review`, before cred.3)
1. `CredentialStore` trait shape + file-backend on-disk format (unify `agentos.env` +
   `<provider>.json`, or abstract over both?).
2. Capability granularity: per-provider (`Credential{provider}`) vs per-scope (Gmail read vs
   Drive read) — start coarse and refine?
3. Egress gateway auth per upstream (OAuth bearer vs API-key header vs query param): a small
   per-provider adapter, or a generic "attach header X" config?
4. `/run/state/oauth` backing on QEMU — reuse the memory volume, or a new state mount?
5. Keep the in-container OAuth flow after cred.4, or delete it once the broker owns the lifecycle?

## Out of scope
- MCP **aggregator** (agent → single MCP → downstream MCPs) — separate feature.
- Multi-instance shared broker / remote credential service — `orch.*`.
- Exotic auth (mTLS, SAML) beyond OAuth2 + API-key; secret *generation* / cloud STS — future.

## Relationship to existing code
- Extends `agentd/src/egress.rs` (`EgressProxy`/`ProxyRegistry`) and reuses its loopback +
  ephemeral-identity machinery.
- Extends `agentd/src/capability.rs` (new credential scope) + `caps_to_rules()` wiring.
- Replaces direct secret reads in `docker/oauth_mcp.py`, `docker/http_mcp.py`,
  `docker/search_mcp.py` (cred.4) and the ad-hoc credential path in `agentd/src/tools/mcp.rs`
  (cred.3).
- New flight events per `docs/CONVENTIONS.md`; `THREAT_MODEL.md` section.
