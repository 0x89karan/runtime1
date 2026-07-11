# cred.6 — Migrate the CoS to broker mode (close Phase 10 for the flagship)

**Increment:** cred.6 (Phase 10 — Credential manager).
**Status:** Planned (2026-07-11, split from the old cred.6 by the CEO review). Not started.
**Depends on:** cred.3–cred.5 (broker gateway + registry + visibility, all shipped) and cred.4b
(broker-capable Python sidecars, shipped). Nothing new in those layers is required.
**Unlocks:** cred.7 (credential resilience) — which only makes sense once the CoS is broker-mode.

## Why

Phase 10's goal is **one credential model across all surfaces: tools never hold a raw credential in
memory-at-rest** (ROADMAP §"Phase 10", cred.4 acceptance criterion). The broker that delivers this
is **built and shipped** (cred.3 gateway, cred.4b sidecars, cred.5 visibility) — but the flagship CoS
**never migrated**. Both `cos.agents.toml` files still run the legacy file path:

```
"OAUTH_PROVIDER_NAME", ...              # env-driven routing
{ FsRead = { prefix = "/run/secrets" } }  # sidecar reads /run/secrets/google.json directly
```

So the goal is **half-delivered**: the broker exists, the flagship doesn't use it, and the
`oauth_mcp.py` sidecar process holds the raw refresh token in its own memory. A compromised sidecar
leaks it. This increment finishes the job for the CoS.

This is **not** a user-visible change — the CoS authenticates and works today (v0.73.2). The payoff is
(1) security (the raw credential is confined to the Rust gateway) and (2) it closes Phase 10 for the
flagship and unlocks cred.7.

## Scope — mostly config + a retest gate

1. **Flip both CoS configs to broker mode.** In `agentd/cos.agents.toml` and
   `distro/overlay/etc/agentd/cos.agents.toml`: add a `[credential_gateway]` block with a `google`
   provider (`oauth-bearer` adapter, upstream `https://gmail.googleapis.com`, token_url, the
   PASSTHROUGH allow-list), and grant the CoS agent(s) `Capability::Credential { provider = Google }`.
   Remove the now-unnecessary `OAUTH_PROVIDER_NAME` env routing and the direct `FsRead /run/secrets`
   grant for the sidecar (the gateway reads the token source, not the sidecar). The gateway still
   reads the credential *source* (`/run/state/oauth` / the provisioned `google.json`) — "file mode
   gone" means the *tool process* no longer reads it, not that no file exists.
2. **Confirm the sidecar takes the broker path.** `oauth_mcp.py` already short-circuits to the broker
   when `AGENTD_CREDENTIAL_GATEWAY_URL` + the per-spawn broker token are present (cred.4b). Verify the
   CoS spawn injects both, and that the legacy env/secrets-file reads are skipped (cred.4b
   `_load_config` broker short-circuit).
3. **Auth retest gate (the real risk — do not re-break v0.73.2).** Broker mode changes the credential
   path we *just* stabilized. This increment does **not** land until a full end-to-end retest passes on
   Mac+Docker: cold start → `agentctl auth google` (or device flow) → CoS reads Gmail through the
   gateway → a brief is produced. Capture the retest in `DEPLOYMENT.md` / the run guide.

## P0 do-first (ships independently, before or with the migration)

- **Secret-redaction of token-endpoint bodies.** `oauth_mcp.py:430` returns the raw token-endpoint
  body to the agent; `credential/mod.rs:341,371` store it in error strings + flight events; cred.5-ar-01
  leaks it into `provider_health.last_error`. Strip `access_token`/`refresh_token`/`id_token`/
  `client_secret`/`authorization` + long bearer-looking strings before any log/event/approval/tool
  response; emit only a safe classified hint. **This is a live leak — it is not gated on the migration
  and should ship first.** (Folds cred.5-ar-01; the same redaction is reused by cred.7 §8.)

## Test plan (each fails without its fix)

- **Broker path taken:** a CoS Gmail call routes through the gateway (gateway sees the request; the
  sidecar env carries no raw token). Assert `AGENTD_CREDENTIAL_GATEWAY_URL` + broker token present and
  the legacy secrets-file read is skipped.
- **No raw credential in the tool process:** inspect the sidecar's effective env/config — no
  `refresh_token` / `client_secret` present; only the ephemeral broker token.
- **Secret redaction:** a token endpoint returns JSON with `refresh_token` + `client_secret` → logs,
  flight events, `provider_health.last_error`, and any tool response contain no `ya29.`, no `1//…`,
  no `client_secret`.
- **Auth retest (integration, manual gate):** Mac+Docker cold start → auth → Gmail read → brief
  produced, through the gateway. Recorded in the run guide.
- **Config assertion:** both `cos.agents.toml` files carry `[credential_gateway]` + a `Credential`
  grant and no longer grant the sidecar `FsRead /run/secrets` for credential reads.

## Acceptance

- The CoS authenticates and reads Gmail **through the broker gateway**; the `oauth_mcp.py` process
  holds **no raw refresh token** at rest.
- The v0.73.2 auth flow still works end-to-end on Mac+Docker (retest gate passed).
- No token/secret appears in any log, flight event, approval, or tool response (redaction landed).
- Phase 10's "tools hold no raw credential" criterion is now true for the flagship, not just in the
  abstract. cred.7 (resilience) is unblocked.
- Every path has a test that fails without the fix.

## Non-goals

- Terminal-failure detection / operator surfacing / resume-without-restart / multi-agent dedup —
  that is **cred.7** (`docs/plans/cred.7-credential-resilience.md`), which depends on this.
- The Google Production-publishing fix (kills the weekly 7-day Testing-mode expiry) — that is an
  operator-usability fix tracked in **cos-polish** (helps file mode *or* broker mode; orthogonal).
- Any second provider's broker migration (Brave is already broker-capable; GitHub is a follow-up).

---

## GSTACK REVIEW REPORT

**Skill:** /plan-ceo-review · **Branch:** plan/cockpit-expansion (PR #103) · **Date:** 2026-07-11
**Mode:** SELECTIVE EXPANSION → restructure (prior CEO review already expanded ux.4–7; no new cathedral scope surfaced).
**Scope reviewed:** cred.6/cred.7 (this split), cos-polish, memory-routing, ux-cockpit + the "prioritize broker mode?" question.

| Decision | Verdict | Basis |
|---|---|---|
| Sequencing strategy | **User-value-first + cheap wins pulled forward** (user-selected) | broker mode is a means not a user-felt end; cos-polish is what makes the CoS look un-broken |
| Split old cred.6 | **cred.6 = broker migration** (first) · **cred.7 = resilience** (on top) | number order = build order; migration is a low-risk config flip, resilience rides on it |
| 7-day Google expiry | **Pulled forward into cos-polish #9** | highest-frequency real failure, orthogonal to broker/file, cheap |
| Secret-redaction | **Do-first P0 in cred.6, ships independently** | live leak (oauth_mcp.py:430, mod.rs:341/371); folds cred.5-ar-01 |
| Broker migration cost | **Mostly config + auth-retest gate, not a big build** | cred.4b already built the sidecar broker path; CoS just never opted in |
| Stale premise in old cred.6 | **Corrected** | "stale image / FsRead" was the wrong diagnosis; real cause was the check_auth bug fixed in v0.73.2 (#102) |

**Full sequence:** [now, parallel] Google Prod-publish · secret-redaction → [then] cos-polish → memory-routing → cred.6 (broker migration) → cred.7 (resilience). [Parallel, agentctl-client] ux.0 → ux.9 → ux.2 → ux.1 → ux.8 → ux.3.

**Coordination flag (collaborative repo):** cred.6 and cockpit ux.0 both touch the two `cos.agents.toml` files ([credential_gateway] grant vs [management] bind_addr) — sequence them or rebase carefully against the concurrent build session.

VERDICT: APPROVED — restructure applied to the docs on PR #103. No unresolved decisions.

NO UNRESOLVED DECISIONS
