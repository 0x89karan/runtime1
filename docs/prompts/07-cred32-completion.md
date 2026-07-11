# 07 — cred.3.2 credential-manager hardening completion (build-session kickoff)

> **✅ Shipped — cred.3.2 (v0.62.0, PR #87). Historical; kept for reference.**

Paste the block below to start the build session. Full plan: `docs/plans/cred.3.2-hardening-completion.md`.

---

```
TASK: cred.3.2 — finish credential-manager hardening (completes cred.3.1's divergences)
      BLOCKS cred.4 / orch.1 until green.

CONTEXT
cred.3.1 (PR #85, v0.61.0) closed most of the hardening gate but REDUCED three items,
DE-CLAIMED two defenses instead of building them, and left doc reconciliation half-done
(verified against the branch). cred.3.1 was honest about it — this is completion, not a redo.
Read: docs/plans/cred.3.2-hardening-completion.md, docs/plans/cred.3.1-hardening.md,
TODOS.md "Phase 10 — Credential manager", docs/AUDIT-v0.60.html.

ALREADY DONE in cred.3.1 (do NOT redo): ar-08 header allow-list, ar-06 state_path read-back,
ar-07 deny-by-default (fail-open closed), S1 signing-key FsRead invariant, ar-04 SSRF at
startup + IP-class coverage, S2/S3 de-claimed honestly, tests T18–T24.

CLOSE THESE (the increment):

Group A — anti-drift + per-request SSRF (anchor)
  ar-10 (real) Extract a shared forwarding HANDLER, not just the client builder. loopback_proxy.rs
        currently shares only the reqwest client; egress.rs::handle_proxy_request and
        credential::handle_credential_request still DUPLICATE the request-level guards (SSRF,
        header allow-list, body cap, path handling). Unify them into one core with a pluggable
        auth-injector so guards cannot drift. TEST the shared core directly.
  ar-04 (per-request) SSRF is startup-only → DNS rebinding open. Re-validate per request OR pin the
        resolved IP and connect to it (preserve SNI/Host); do it in the ar-10 shared handler.
        TEST: host passes at startup but rebinds to a private IP → blocked at request time.

Group B — per-agent scoping
  ar-07 (per-agent) Scope the gateway token to the OWNING AGENT, not server.name (fix register_token +
        audit attribution). TEST: agent A's token cannot use a provider granted only to agent B;
        flight events attribute to the agent, not the server.

Group C — DECISIONS (take to /plan-eng-review; don't silently keep de-claiming)
  S2  Content audit: implement it (hash the forwarded body into the signed receipt) OR ratify the
      de-claim. Weigh against "observability is half the product."
  S3  SecretRewriter: tool outputs currently reach the model UNSCRUBBED. Implement rewriting at
      ToolRegistry::invoke (tools/mod.rs) OR ratify the de-claim as an accepted limitation.

Group D — finish doc reconciliation (sec.1 / audit D1+D5+V1)
  RUNBOOK.md full pass (version header, drop future-tense on shipped phases, add credential-broker
  section); THREAT_MODEL.md header bump (doc already carries §8); add a canonical
  "vX.Y.Z — shipped/unshipped" status line (CLAUDE.md top-of-status) so this drift can't recur.

Group E — test coverage
  Wire the dropped api-key-header fixture (provider_cfg_api_key_header, credential/mod.rs:887) into a
  REAL adapter test — do not delete it or #[allow(dead_code)] it. (The immediate CI-green fix on #85 to
  merge cred.3.1 is the build session's separate call; cred.3.2 is where the adapter gets tested.)
  Add: ar-04 rebinding test, ar-07 per-agent scoping test, ar-10 shared-guard-core test.

NON-NEGOTIABLE: for every code item — fix + a test that FAILS without it + adversarial verification
of the real failure path, not "applied." No partial doc updates that leave a stale version header. The
audit's #1 finding was security claimed-but-not-built; the ar-10 reduction + S2/S3 de-claims are that
pattern — don't extend it.

OUT OF SCOPE (tracked separately in TODOS, not this increment): whole-system Waves 2–4 (checkpoint
durability, budget reservation, memory F-10, sandbox fail-open, universal netns, broken templates,
boot CI).

DONE = ar-10 unifies the guards (one set, both proxies); ar-04 blocks rebinding at request time; ar-07
scopes tokens to the owning agent; S2 + S3 each explicitly built or ratified; docs headers reflect
reality + a canonical status line; api-key-header adapter tested; every code item has a failing-without-
it test + adversarial verification; /review + /qa clean. Only then are cred.4/orch.1 unblocked.
```
