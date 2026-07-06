# 06 — cred.3.1 credential-manager hardening (build-session kickoff)

Paste the block below to start the build session. Full plan: `docs/plans/cred.3.1-hardening.md`.

---

```
TASK: cred.3.1 — credential-manager hardening gate (BLOCKS cred.4 / orch.1)

CONTEXT
cred.3 (the credential manager / egress gateway) shipped in v0.60.0. A read-only
audit (whole-system + a 4-voice Phase-10 deep-dive) concluded: the DIRECTION is
sound, but the IMPLEMENTATION is NOT yet robust — "not safe to build cred.4 or
orch.1 on as-is." Authoritative sources in the repo:
  - docs/plans/cred.3.1-hardening.md   (the plan — start here)
  - TODOS.md → "v0.60 whole-system audit" + "Phase 10 — Credential manager"
    (cred.3-ar-01…10)
  - docs/AUDIT-v0.60.html               (rendered report)

YOUR JOB
Close the gate below, then re-verify, BEFORE any new credential/orchestration
feature. Project discipline (CLAUDE.md / ROADMAP): one increment per branch,
/autoplan → build → /review → /qa → /ship, main shippable between. Suggested
split: cred.3.1 (gateway guards) · cred.3.2 (OAuth lifecycle) · cred.3.3
(evidence/audit truthfulness). Confirm via /autoplan.

META-INSTRUCTION (non-negotiable)
The audit's #1 finding was that security/audit features were CLAIMED BUT NOT
BUILT. Do not repeat it. For EVERY item: implement it, add a test that FAILS
without the fix, and ADVERSARIALLY VERIFY the actual failure path — not
"applied." Update THREAT_MODEL.md / CLAUDE.md / ROADMAP to what is ACTUALLY
true. If something isn't done, say so.

THE GATE (all green before cred.3 can be called "robust")

Group A — egress-gateway guards (do ar-10 FIRST; it's the anti-drift vehicle)
  ar-10  Extract a shared LoopbackForwardingProxy core (loopback bind +
         ephemeral identity + full guard set) shared by egress.rs and
         credential/mod.rs, with a pluggable auth-injector. The guard DRIFT
         between the two is the root cause of the SSRF gap — fix guards once.
  ar-04  SSRF: resolve upstream_base host, reject private/loopback/link-local/
         metadata IPs (mirror oauth_mcp.py _is_ssrf_blocked()); per-provider
         host allowlist. TEST: provider → 169.254.169.254 rejected before any
         token is attached.
  ar-08  Replace inbound header BLOCKLIST with a WHITELIST. TEST: a caller-
         injected extra header does not reach the upstream.

Group B — OAuth lifecycle correctness
  ar-06  Read state_path on cache init, prefer the rotated refresh_token; fsync
         file + parent dir. (Or DELETE state_path + document re-auth — no write-
         only cache.) TEST: rotate token → restart → rotated token used, re-auth
         does not break.
  ar-07  Deny-by-default provider scoping (no capabilities block → NO providers);
         scope the gateway token to the owning agent, not the server name. TEST:
         server with no cap block gets zero providers; agent A cannot use B's grant.

Group C — evidence/audit truthfulness
  S1     Fail startup if egress.key_path resolves under any MCP FsRead/FsWrite
         prefix; default the signing key outside any MCP-accessible tree. TEST:
         config with key under an MCP FsRead prefix fails to start.
  S2     Hash the forwarded body into the receipt (make content_audited true) OR
         remove the claim from the event + docs.
  S3     Implement boundary secret-rewriting at ToolRegistry::invoke (tools/mod.rs)
         OR correct CLAUDE.md + memory to state it was never shipped.

RECORD-THE-DECISION (resolve before orch.1/mesh, not cred.3.1 code)
  ar-09  Mesh refresh collision: a shared credential service is a prerequisite
         for mesh.* — do NOT build mesh on the in-process broker.
  Universal-tier has NO provider-credential path — declare it a limitation or
  design a separate mechanism; document it.

DOC RECONCILIATION (cheap; alongside — audit Tier 4)
  RUNBOOK stale (v0.20/v0.59); ROADMAP build-order header vs detail; THREAT_MODEL
  v0.25.0 header + false "no other credentials"/"flight not tamper-evident"; add
  a canonical "v0.60.0 — shipped/unshipped" status line.

DONE =
  every gate item has (fix + failing-without-it test + adversarial verification),
  docs reflect reality, /review + /qa clean. Only then are cred.4/orch.1 unblocked.
```
