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

## Scope — config + one Rust addition + retest gate

### Step 0: Add `passthrough_query_params` to `ProviderConfig` (REQUIRED before config flip)

The credential gateway (`agentd/src/credential/mod.rs:927`) unconditionally discards inbound query
strings: `let query = String::new()`. This was intentional (cred.3.2 decision D3 — prevents MCP
servers from injecting billing-affecting params). However, Gmail API calls require query params
(`?maxResults=50&q=newer_than:1d`, `?format=full`) to work correctly. Without them, the broker
returns all-time messages with no body — producing a brief that appears to succeed but is silently
wrong.

**Fix:** Add `passthrough_query_params: Vec<String>` to `ProviderConfig` in `config.rs`
(default = empty, meaning no params forwarded — preserving D3 security for all other providers).
In `handle_credential_request`, extract only the listed param names from the inbound query string
and forward them to upstream. Replace test T35b (`test_query_string_discarded_from_upstream`) with
a test that asserts the allowlist behavior: listed params are forwarded, unlisted params are blocked.

In the google provider config: `passthrough_query_params = ["maxResults", "q", "format", "pageToken", "includeSpamTrash"]`

### Step 1: Flip both CoS configs to broker mode

In `agentd/cos.agents.toml` and `distro/overlay/etc/agentd/cos.agents.toml`:
- Add a `[credential_gateway]` block with a `google` provider (`oauth-bearer` adapter,
  `upstream_base = "https://gmail.googleapis.com"`, `token_url`, `state_path`, `passthrough_query_params`).
- For `state_path`: use `/run/memory/oauth/google.json` for BOTH Docker and QEMU (the writable 9p
  memory mount; `/run/state` does NOT exist in the QEMU initramfs).
- Grant the CoS agent(s) `Capability::Credential { provider = Google }`.
- Remove the now-unnecessary `OAUTH_REFRESH_TOKEN`/`OAUTH_CLIENT_SECRET`/`OAUTH_ACCESS_TOKEN`
  from the sidecar's `passenv` list (they're still in `PASSENV_BLOCKLIST`, so the runtime would
  already block them — the config change makes the intent explicit).
- Remove the direct `FsRead { prefix = "/run/secrets" }` grant from the `google_oauth` MCP server
  (the gateway reads the token source, the sidecar does not need filesystem access to credentials).

### Step 2: Confirm the sidecar takes the broker path

`oauth_mcp.py` already short-circuits to the broker when `AGENTD_CREDENTIAL_GATEWAY_URL` + the
per-spawn broker token are present (cred.4b `_load_config` broker short-circuit). Verify the CoS
spawn injects both, and that the legacy env/secrets-file reads are skipped.

### Step 3: Update the polarity-inverted test

`cos_config_google_oauth_grants_fs_read_secrets` (currently at `agentd/src/main.rs:2095`) asserts
that `FsRead { prefix = "/run/secrets" }` IS present. After cred.6 this will correctly fail.
Replace it with a test that asserts: (a) `FsRead /run/secrets` is ABSENT from `google_oauth`
capabilities, and (b) `Capability::Credential { provider = Google }` IS present in the agent caps.

### Step 4: Auth retest gate

Broker mode changes the credential path we *just* stabilized. This increment does **not** land until
a full end-to-end retest passes on Mac+Docker: cold start → `agentctl auth google` (or device flow)
→ CoS reads Gmail through the gateway → a brief is produced with the correct date range.
Capture the retest result in `DEPLOYMENT.md`.

## P0 do-first (defense-in-depth, not a live-leak blocker)

- **Secret-redaction of token-endpoint bodies.** `credential/mod.rs` already guards refresh errors
  via `test_token_refresh_error_does_not_include_body` (T36). `oauth_mcp.py` Tests 32–35 guard
  raw-token body exposure in error strings. The originally-cited paths (`oauth_mcp.py:430`,
  `mod.rs:341,371`) are already covered. The remaining exposure is `provider_health.last_error` in
  `cred.5`-ar-01 (the CredentialSnapshot FUSE/API surface). This is defense-in-depth: strip
  `access_token`/`refresh_token`/`id_token`/`client_secret` + bearer-shaped strings from
  `last_error` before surface exposure. Ship with the migration rather than before it.
  (Folds cred.5-ar-01; the same redaction is reused by cred.7 §8.)

## Test plan (each fails without its fix)

- **Query passthrough (T35b replacement):** `passthrough_query_params = ["maxResults"]` → the gateway
  forwards `?maxResults=50` to upstream and blocks `?x-goog-user-project=evil`. Test both paths.
- **Broker path taken:** a CoS Gmail call routes through the gateway (gateway sees the request; the
  sidecar env carries no raw token). Assert `AGENTD_CREDENTIAL_GATEWAY_URL` + broker token present and
  the legacy secrets-file read is skipped.
- **No raw credential in the tool process:** inspect the sidecar's effective env/config — no
  `refresh_token` / `client_secret` present; only the ephemeral broker token.
- **Config polarity flip:** `cos_config_broker_mode_and_no_fs_read` — both `cos.agents.toml` files
  carry `[credential_gateway]` + a `Credential { Google }` grant; `FsRead /run/secrets` is absent
  from the `google_oauth` server's capabilities.
- **Secret redaction (defense-in-depth):** `provider_health.last_error` on the FUSE/API surface
  contains no `ya29.`, no `1//…`, no `client_secret`.
- **Auth retest (integration, manual gate):** Mac+Docker cold start → auth → Gmail read with
  `?q=newer_than:1d` filter → brief produced through the gateway with correct date-filtered results.
  Recorded in the run guide.

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

---

## AUTOPLAN REVIEW REPORT

**Skill:** /autoplan · **Branch:** lane/cos-backend · **Date:** 2026-07-13
**Mode:** CEO dual-voice + Eng dual-voice (Codex). Phase 2 (Design) skipped — no UI scope.

### CEO Consensus

| Dimension | Claude | Codex | Consensus |
|---|---|---|---|
| Premises valid? | PARTIALLY (D3 ignored) | PARTIALLY (D3 confirmed) | SCOPE WIDER THAN STATED |
| Right problem? | YES | YES | CONFIRMED |
| Scope calibration correct? | NO (D3 fix needed) | NO (D3 fix + test flip) | SCOPE EXPANDED |
| Alternatives explored? | YES | YES | CONFIRMED |
| 6-month trajectory sound? | CONDITIONAL (fix D3) | CONDITIONAL | BLOCKED until D3 fixed |
| CoS backend fit (user Q)? | YES | YES | CONFIRMED — orthogonal |

### Eng Consensus

| Dimension | Claude | Codex | Consensus |
|---|---|---|---|
| Architecture sound? | ISSUE (D3 blocks) | ISSUE (D3 blocks) | D3 must be fixed first |
| Test coverage sufficient? | GAP (polarity flip) | GAP (T35b replace) | Both required |
| Performance risks? | NONE | NONE | CONFIRMED |
| Security threats? | P0 stale claim | P0 stale claim | Tests 32-35 + T36 already cover cited paths |
| Error paths handled? | PARTIAL | OK | defense-in-depth for last_error |
| Deployment risk (QEMU)? | state_path wrong | state_path wrong | use /run/memory not /run/state |

### Decisions

| # | Kind | Decision | Rationale |
|---|---|---|---|
| D1 | USER CHOICE | `passthrough_query_params: Vec<String>` per-param allowlist on `ProviderConfig` | Preserves D3 security intent while unblocking Gmail `?maxResults`/`?q` params. User chose recommended option. |
| D2 | USER CHOICE | `state_path = "/run/memory/oauth/google.json"` for BOTH Docker and QEMU | `/run/state` does not exist in the QEMU initramfs; use the writable 9p memory mount. User chose recommended option. |
| D3 | USER CHOICE | CoS backend fit: CONFIRMED clean integration | memory-routing/cos-polish/ux.0b are orthogonal to credential path. User confirmed both-models finding. |
| A1 | AUTO | Phase 2 (Design) skipped | No UI scope in this increment |
| A2 | AUTO | P0 "live leak" claim is stale | Tests 32–35 (`oauth_mcp.py`) + T36 (`mod.rs:3037`) already cover cited leak paths. Ship redaction as defense-in-depth with migration, not as a separate do-first blocker. |
| A3 | AUTO | Both cos.agents.toml files updated in same PR | They must stay in sync; separate PRs would introduce a window where CoS behavior differs across deploy targets. |
| A4 | AUTO | `cos_config_google_oauth_grants_fs_read_secrets` test must be replaced | After the migration this test correctly fails (FsRead removed). Replace with inverted assertion: FsRead absent + Credential{Google} present. |
| A5 | AUTO | T35b (`test_query_string_discarded_from_upstream`) must be replaced | Source-scan guard enforces total discard; the new allowlist behavior makes it fail by design. Replace with a test that asserts allowlist semantics (listed params forwarded, unlisted blocked). |

### CoS backend fit (user's explicit question)

The user asked: "does it work well with the cos backend development we've done so far?"

**YES** — confirmed by both models independently. `memory-routing` (semantic-kb, mail:raw, tool_override),
`cos-polish` (KB fixes, budget, max_turns), and `ux.0b` (cos-net/agent-net segmentation,
allow_non_loopback) are all orthogonal to the credential path. The `oauth_call_api → broker → Gmail →
kb_put → Qdrant` chain is unchanged by cred.6. The credential gateway binds to loopback inside the
cos container, so Docker network segmentation is irrelevant. The only edits to `cos.agents.toml` are
in the `google_oauth` server block and a new `[credential_gateway]` block — no touch to `semantic-kb`,
memory segments, or orchestrator capabilities.

VERDICT: APPROVED WITH EXPANDED SCOPE — the "mostly config + retest" premise was incomplete. Actual
scope: config changes + `passthrough_query_params` Rust addition + test updates (polarity flip +
T35b replacement) + retest gate. All decisions locked. Ready to implement.
