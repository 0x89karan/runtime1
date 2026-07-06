# cred.3.2 — Credential-manager hardening completion

**Track:** cred. **Status:** planned. **Depends on:** cred.3.1 (PR #85, v0.61.0). **Blocks:** cred.4 /
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

## Done =

ar-10 unifies the request-level guards (one guard set, both proxies) · ar-04 blocks rebinding at
request time · ar-07 scopes tokens to the owning agent · S2 and S3 are each explicitly built or
ratified via eng-review · RUNBOOK + THREAT_MODEL headers reflect reality + a canonical status line
exists · api-key-header adapter is tested · every code item has a failing-without-it test + adversarial
verification · `/review` + `/qa` clean. Only then is cred.3 "robust" and cred.4 / orch.1 unblocked.
