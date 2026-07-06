# cred.3.1 — Credential-manager hardening gate

**Track:** cred. **Status:** planned (blocks cred.4 / orch.1). **Source:** the v0.60 whole-system
audit + the 4-voice Phase-10 deep-dive (see `TODOS.md` → "v0.60 whole-system audit" and "Phase 10 —
Credential manager"; rendered in `docs/AUDIT-v0.60.html`).

## Why this is a gate, not an increment

cred.3 (the credential manager / egress gateway) shipped in v0.60.0. The audit concluded: the
**direction is sound** (single-gateway model is correct; it's the observability chokepoint the thesis
wants) but the **implementation is not yet robust** — verdict: *not safe to build cred.4 or orch.1 on
as-is*. This doc is the checklist that, when fully green, lets us honestly call cred.3 robust and
unblocks downstream credential/orchestration work.

## Meta-instruction (non-negotiable)

The audit's #1 finding was that security/audit features were **claimed but not built**
(`content_audited: true` audited nothing; the `SecretRewriter` didn't exist). Do not repeat it.
For **every** gate item: implement it, add a test that **fails without** the fix, and
**adversarially verify** the actual failure path — not "applied." Update `THREAT_MODEL.md` /
`CLAUDE.md` / `ROADMAP.md` to what is *actually* true. If something isn't done, say so.

## Suggested increment split

Confirm via `/autoplan`, one increment per branch, `main` shippable between:
- **cred.3.1** — Group A (egress-gateway guards, via the shared-proxy extraction).
- **cred.3.2** — Group B (OAuth lifecycle correctness).
- **cred.3.3** — Group C (evidence/audit truthfulness).

## The gate

### Group A — egress-gateway guards (do `ar-10` first; it's the anti-drift vehicle)

**ar-10 [structural, first] — extract a shared `LoopbackForwardingProxy` core.**
`credential/mod.rs` shares no code with `egress.rs`; the **guard drift** between the two parallel
egress proxies is the root cause of the SSRF gap. Extract a shared core (loopback bind + ephemeral
identity + the full guard set) with a pluggable auth-injector, used by both. Also reconcile the docs
that call cred.3 an "EgressProxy extension" — it is a sibling subsystem.
*Where:* new module factored from `agentd/src/egress.rs` + `agentd/src/credential/mod.rs`.

**ar-04 — SSRF / host allowlist on `upstream_base`.**
Resolve the host and reject private/loopback/link-local/metadata IPs (mirror
`docker/oauth_mcp.py:_is_ssrf_blocked()`); enforce a per-provider host allowlist. The broker is
currently *weaker* than the Python tools it replaced.
*Accept:* a provider whose `upstream_base` resolves to `169.254.169.254` / a private IP is rejected
**before** any credential is attached; test covers DNS-resolve + rebinding.
*Where:* `agentd/src/credential/mod.rs` (`CredentialGateway::start` + per-request path).

**ar-08 — inbound header whitelist (not blocklist).**
Replace the fail-open scrub list with a whitelist (content-type / accept / provider-necessary only),
matching `egress.rs`.
*Accept:* a caller-injected extra header (e.g. `X-Forwarded-For`, a second auth header) does **not**
reach the upstream; test asserts it.
*Where:* `agentd/src/credential/mod.rs:355`.

### Group B — OAuth lifecycle correctness

**ar-06 — `state_path` must be read back.**
The writable token cache is currently write-only: startup always re-reads the original `/run/secrets`
refresh token, so a rotated single-use token is lost on any restart (re-auth breaks; worst on QEMU/9p).
Read `state_path` on cache init and prefer its `refresh_token`; fsync file + parent dir. If you decide
not to persist rotations, **delete** `state_path` and document the re-auth requirement — no write-only
cache.
*Accept:* rotate a refresh token, restart agentd, confirm the rotated token is used (re-auth does not
break); test covers the rotation→restart path.
*Where:* `agentd/src/credential/mod.rs` (`OAuthTokenCache` init + write path ~:184,235).

**ar-07 — deny-by-default provider scoping; scope tokens to the owning agent.**
An MCP server with no `capabilities` block is currently granted **every** configured provider
(fail-open), and the registry stores the *server name* as the `agent_id`.
*Accept:* a server with no cap block gets **zero** providers; agent A's token cannot use agent B's
grant; audit attribution records the owning agent.
*Where:* `agentd/src/main.rs:530,544`.

### Group C — evidence / audit truthfulness

**S1 — signing key must not be readable by a tool.**
Fail startup if `egress.key_path` resolves under any MCP `FsRead`/`FsWrite` prefix; default the key
outside any MCP-accessible tree.
*Accept:* a config with the key under an MCP `FsRead` prefix fails to start; test asserts it.
*Where:* `agentd/src/config.rs:181`, `agentd/src/main.rs:859`, `agentd/src/evidence.rs:88`.

**S2 — `content_audited` must be true or gone.**
Either hash the forwarded body into the receipt (make the claim true) or remove `content_audited`
from the event + docs. No asserting an audit that doesn't happen.
*Where:* `agentd/src/egress.rs:150`.

**S3 — reconcile `SecretRewriter`.**
Implement boundary secret-rewriting at the `ToolRegistry::invoke` choke point (`tools/mod.rs`) — so
tool outputs are scrubbed before reaching the model — **or** correct `CLAUDE.md` + memory to state it
was never shipped. Reconcile claim with reality either way.
*Where:* `agentd/src/tools/mod.rs:182`.

## Record-the-decision (not cred.3.1 code, but resolve before orch.1 / mesh)

- **ar-09 — mesh refresh collision.** In-process broker + Google single-use rotation → multiple
  instances mutually invalidate the same token. A shared credential service is a **prerequisite for
  `mesh.*`** (couples to mesh.3). Add the forward-reference; do **not** build mesh on the in-process
  broker.
- **Universal-tier has no provider-credential path** (`universal.rs` uses no MCP tools). Declare it a
  limitation or design a separate mechanism; document it — don't imply "any framework governed."

## Doc reconciliation (cheap; do alongside — audit Tier 4)

RUNBOOK stale (v0.20/v0.59); ROADMAP build-order header vs detail (D2); THREAT_MODEL v0.25.0 header +
false "no other credentials" / "flight not tamper-evident"; add a canonical
"v0.60.0 — shipped/unshipped" status line so this drift can't recur.

## Done =

Every gate item has (fix + a failing-without-it test + adversarial verification), the docs reflect
reality, and `/review` + `/qa` are clean. Only then is cred.3 "robust" and cred.4 / orch.1 unblocked.
