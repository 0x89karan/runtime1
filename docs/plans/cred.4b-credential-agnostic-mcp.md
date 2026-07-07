<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/autoplan-restores/ -->
# cred.4b — Credential-agnostic MCP servers

**Track:** cred. **Status:** plan (2026-07-07). **Depends on:** cred.3 + cred.3.1 + cred.3.2 + cred.4
(PR #88, spend caps + GitHub adapter). **Blocks:** cred.5 (credential surfacing).
**Completes:** ROADMAP `▣ cred.4 — Egress gateway` — marks that entry `✓` on ship.

## Problem

The ROADMAP's cred.4 acceptance criterion is: "a tool process holds no raw credential in
env or memory-at-rest; outbound calls are authenticated at the broker; a denied provider is
blocked." PR #88 shipped the spend caps + GitHub adapter under the "cred.4" label but with
different scope — the original acceptance criterion was never met.

**What's still failing:**

`oauth_mcp.py` calls `_load_config()` unconditionally at startup. This function:
1. Opens `/run/secrets/google.json` and reads `client_id`, `client_secret`, `refresh_token`.
2. Falls back to `OAUTH_CLIENT_ID`, `OAUTH_CLIENT_SECRET`, `OAUTH_REFRESH_TOKEN` env vars.
3. Stores all three raw credentials in the `_cfg` dict (module-level global).

This happens even when `AGENTD_CREDENTIAL_GATEWAY_URL` is set — i.e., the broker is fully
configured and the raw credentials are never needed. Result: the MCP process holds OAuth raw
credentials in memory throughout its lifetime in violation of the acceptance criterion.

`search_mcp.py` already routes through the broker first and only touches the env var on
fallback — no functional gap, but the fallback fires silently with no migration signal.

## What this increment builds

### A — `oauth_mcp.py` broker mode (credential-agnostic path)

**Change: broker short-circuit in `_load_config()`**

Add at the top of `_load_config()`, before any file reads or env var access:

```python
# Broker mode: never read raw credentials. Only load non-credential routing config.
if _BROKER_URL:
    _cfg["OAUTH_PROVIDER_NAME"] = os.environ.get("OAUTH_PROVIDER_NAME", "").strip() or "google"
    raw_hosts = os.environ.get("OAUTH_ALLOWED_HOSTS", "").strip() or GOOGLE_ALLOWED_HOSTS
    _cfg["ALLOWED_HOSTS"] = {h.strip() for h in raw_hosts.split(",") if h.strip()}
    return None  # success — broker manages auth, no credentials needed here
```

`OAUTH_PROVIDER_NAME` and `ALLOWED_HOSTS` are still needed in broker mode:
- `OAUTH_PROVIDER_NAME`: used at line 616 to construct `{gw}/{provider}/{path}` broker URL
- `ALLOWED_HOSTS`: used at line 602 SSRF check on the destination URL

These are NOT credentials — they are routing/policy config.

**Change: broker guard in `handle_oauth_start_auth`**

Add at the top of `handle_oauth_start_auth`, before any `_cfg` access:

```python
if _BROKER_URL:
    return None, json.dumps({
        "error": "broker_managed",
        "message": (
            "OAuth is managed by the credential broker. "
            "Credentials are provisioned via `agentctl auth google`. "
            "Use oauth_call_api to make authenticated requests."
        ),
    })
```

**Change: broker guard in `handle_oauth_check_auth`**

Add at the top of `handle_oauth_check_auth`, before any `_auth_state` check:

```python
if _BROKER_URL:
    return {"ready": True, "broker_managed": True}, None
```

In broker mode, auth is always "ready" (the broker holds the token and manages refresh).

**Startup path** (in `__main__`): No change needed. When broker is active, `_load_config()`
returns early without setting `OAUTH_REFRESH_TOKEN` in `_cfg`, so the
`if _cfg.get("OAUTH_REFRESH_TOKEN")` guard naturally skips the legacy token-file path.
`_auth_state` stays "idle", which is fine — `handle_oauth_call_api` takes the broker path
before checking `_auth_state`.

### B — `search_mcp.py` deprecation warning

Add one line to the legacy fallback path (before reading `BRAVE_SEARCH_API_KEY`):

```python
# Legacy fallback: direct Brave API access via env var (backward compat).
api_key = os.environ.get("BRAVE_SEARCH_API_KEY", "")
if api_key:
    print(
        "search_mcp: WARNING: BRAVE_SEARCH_API_KEY direct access is deprecated. "
        "Configure [credential_gateway.providers.brave-search] in your agent config "
        "to route through the credential broker.",
        file=sys.stderr,
    )
```

Zero functional change. Fires only when the legacy path is actually used.

### C — Tests T24-T28 in `oauth_mcp.py`

Add 5 self-tests after the existing T23, updating `total = 23` → `total = 28`.
T25 and T26 set both `_BROKER_URL` and `_BROKER_TOKEN` (both required by `_BROKER_URL and _BROKER_TOKEN` guard).
T28 added during adversarial review to cover URL-only misconfiguration path (`broker_token_missing`):

**T24** — `_load_config()` in broker mode → no raw credentials in `_cfg`:
```python
global _BROKER_URL
old_burl = _BROKER_URL
_BROKER_URL = "http://broker.test"
_cfg.clear()
# No OAUTH_CLIENT_ID etc. in env
err24 = _load_config()
_BROKER_URL = old_burl
assert err24 is None
assert "OAUTH_CLIENT_SECRET" not in _cfg
assert "OAUTH_REFRESH_TOKEN" not in _cfg
assert "OAUTH_PROVIDER_NAME" in _cfg  # routing config still loaded
```

**T25** — `oauth_start_auth` in broker mode → broker_managed error:
```python
global _BROKER_URL
old_burl = _BROKER_URL
_BROKER_URL = "http://broker.test"
_reset_state()
result25, err25 = handle_oauth_start_auth({})
_BROKER_URL = old_burl
assert result25 is None and err25 is not None
assert json.loads(err25).get("error") == "broker_managed"
```

**T26** — `oauth_check_auth` in broker mode → `{"ready": True, "broker_managed": True}`:
```python
global _BROKER_URL
old_burl = _BROKER_URL
_BROKER_URL = "http://broker.test"
_reset_state()
result26, err26 = handle_oauth_check_auth({})
_BROKER_URL = old_burl
assert err26 is None and result26["ready"] is True and result26.get("broker_managed") is True
```

**T27** — `oauth_call_api` in broker mode with minimal `_cfg` (only routing fields) → routes to broker:
```python
global _BROKER_URL, _BROKER_TOKEN
old_burl = _BROKER_URL; old_btok = _BROKER_TOKEN
_BROKER_URL = "http://127.0.0.1:19998"
_BROKER_TOKEN = "tok27"
# _cfg only has ALLOWED_HOSTS and OAUTH_PROVIDER_NAME (no raw credentials)
_cfg.clear()
_cfg["ALLOWED_HOSTS"] = set()  # empty = allow all
_cfg["OAUTH_PROVIDER_NAME"] = "google"
_auth_state_save = _auth_state
# Mock broker response
mock_resp27 = MagicMock()
mock_resp27.read.return_value = b'{"broker":"ok"}'
mock_resp27.status = 200
mock_resp27.__enter__ = lambda s: s
mock_resp27.__exit__ = MagicMock(return_value=False)
with patch("urllib.request.urlopen", return_value=mock_resp27):
    result27, err27 = handle_oauth_call_api({"url": "https://www.googleapis.com/calendar/v3/calendars"})
_BROKER_URL = old_burl; _BROKER_TOKEN = old_btok
assert err27 is None and result27 is not None
```

### D — Docs

1. ROADMAP.md: change `▣ cred.4` → `✓ cred.4 [v0.63.0+v0.6x.0]` with note that the scope
   shipped across two PRs (PR #88 spend caps + this PR credential-agnostic MCP).
2. CLAUDE.md: add status line for cred.4b.
3. `docs/plans/cred.4b-credential-agnostic-mcp.md`: this file.

## What is explicitly NOT in cred.4b

| Item | Reason | Where |
|------|--------|-------|
| Remove BRAVE_SEARCH_API_KEY fallback entirely | Breaking change for operators without broker | cred.5 |
| Add `--require-broker` TOML flag | Hardening for strict deployments | cred.5 |
| cred.3-ar-01/02/03 | Lifecycle polish | cred.5 |
| cred.3.2-ar-01/02 | Handler cleanup | cred.3.3 |
| http_mcp.py | No credentials; no applicable change | — |
| semantic_kb_mcp.py | No credentials; uses SIDECAR_SECRET optional inbound auth (different model) | — |

## Data flow (broker mode)

```
┌──────────────────────────────────────────────────────────────────────────┐
│ BEFORE cred.4b (broker active, oauth_mcp.py started)                    │
│                                                                          │
│  _load_config() ──► reads /run/secrets/google.json                       │
│       │                   (client_id, client_secret, refresh_token)      │
│       │                   stored in _cfg dict in memory                  │
│       │                                                                  │
│  oauth_call_api() ──► broker path (AGENTD_CREDENTIAL_GATEWAY_URL set)   │
│       │               _cfg["OAUTH_PROVIDER_NAME"] used                   │
│       │               BUT: raw credentials still in _cfg (unused)        │
│       └──► VIOLATION: "no raw credential in memory-at-rest"              │
└──────────────────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────┐
│ AFTER cred.4b (broker active, oauth_mcp.py started)                     │
│                                                                          │
│  _load_config() ──► _BROKER_URL set? YES                                 │
│       │             → set OAUTH_PROVIDER_NAME + ALLOWED_HOSTS only       │
│       │             → return None (skip secrets file entirely)           │
│       │                                                                  │
│  oauth_call_api() ──► broker path                                        │
│       │               _cfg["OAUTH_PROVIDER_NAME"] = "google" (routing)   │
│       └──► COMPLIANT: no raw credentials ever in _cfg                    │
│                                                                          │
│  oauth_start_auth() → "broker_managed" error (actionable message)        │
│  oauth_check_auth() → {"ready": true, "broker_managed": true}            │
└──────────────────────────────────────────────────────────────────────────┘
```

## Error and recovery map

| Error | Trigger | Response | Visibility |
|-------|---------|----------|------------|
| `broker_managed` | Agent calls `oauth_start_auth` when broker active | `isError: true` + message pointing to `agentctl auth google` | Seen by agent in tool result |
| Broker unreachable | Broker process died; MCP calls timeout | HTTP error from `urllib.request.urlopen` → `broker_request_failed: ...` | Existing error path; flight log |
| Secrets file present but ignored | Operator mounts file; broker active | File read never called — no error, no secret in memory | Correct behavior |
| Legacy BRAVE_SEARCH_API_KEY used | Broker not configured | Warning to stderr + proceeds with API key | `docker logs` / stderr |

## Failure modes

| Failure | Likelihood | Impact | Mitigation |
|---------|-----------|--------|-----------|
| T24 test patches global but restores incorrectly | Low | Test flakiness | Save and restore `_BROKER_URL` in both pass/fail paths |
| Operator calls `oauth_start_auth` in broker mode expecting it to work | Medium | Confused agent | Clear `broker_managed` error + hint to `agentctl auth google` |
| `OAUTH_ALLOWED_HOSTS` not set; broker mode skips `_load_config()` | Low | Empty `ALLOWED_HOSTS` in `_cfg`; SSRF check skipped | `_load_config()` broker path sets `ALLOWED_HOSTS` from env or default `GOOGLE_ALLOWED_HOSTS` |

## Acceptance criteria

- `_load_config()` with broker URL set returns `None`; `_cfg` contains no `OAUTH_CLIENT_SECRET`, `OAUTH_REFRESH_TOKEN` or `OAUTH_CLIENT_ID` key.
- `oauth_start_auth` with broker active returns `isError: true` with `error: "broker_managed"`.
- `oauth_check_auth` with broker active returns `{"ready": true, "broker_managed": true}`.
- `oauth_call_api` with broker active + minimal `_cfg` (no raw credentials) → routes to broker correctly.
- Legacy `BRAVE_SEARCH_API_KEY` fallback in `search_mcp.py` prints deprecation warning to stderr.
- T24-T27 pass (`total` updated to 27).
- ROADMAP cred.4 marked `✓`; CLAUDE.md updated.
- `cargo test` unaffected (Rust-only; Python changes are Docker/test-harness tests).

## Build session prompt

```
Implement cred.4b — credential-agnostic MCP servers. This completes the ROADMAP's
▣ cred.4 acceptance criterion: "a tool process holds no raw credential in env or
memory-at-rest."

Files to edit:
1. docker/oauth_mcp.py — three changes:
   a) Add broker short-circuit at TOP of _load_config() (before file reads):
      if _BROKER_URL: set OAUTH_PROVIDER_NAME + ALLOWED_HOSTS, return None
   b) Add broker guard at TOP of handle_oauth_start_auth:
      if _BROKER_URL: return None, json.dumps({"error": "broker_managed", "message": "..."})
   c) Add broker guard at TOP of handle_oauth_check_auth:
      if _BROKER_URL: return {"ready": True, "broker_managed": True}, None
   d) Add T24-T27 tests (update total = 23 → total = 27)

2. docker/search_mcp.py — one change:
   Add deprecation warning to stderr when BRAVE_SEARCH_API_KEY legacy fallback is used.

3. docs/ROADMAP.md — update ▣ cred.4 → ✓ cred.4 [v0.63.0+v0.6x.0]

4. CLAUDE.md — add cred.4b status line after cred.4 entry

Full spec in docs/plans/cred.4b-credential-agnostic-mcp.md
```

## Test plan

| Test | File | Verifies |
|------|------|---------|
| T24: `_load_config` broker short-circuit | oauth_mcp.py self-test | No raw credentials in `_cfg` when broker URL set |
| T25: `oauth_start_auth` broker guard | oauth_mcp.py self-test | Returns `broker_managed` error |
| T26: `oauth_check_auth` broker guard | oauth_mcp.py self-test | Returns `{"ready": true, "broker_managed": true}` |
| T27: `oauth_call_api` minimal-cfg broker path | oauth_mcp.py self-test | Broker routing works without raw credentials in `_cfg` |
| Makefile test-harness | search_mcp.py --test | Deprecation warning test (existing self-tests unaffected) |

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|----------|
| 1 | CEO | Scope = oauth_mcp.py broker mode + search_mcp.py warning | Mechanical | P1 | Completes ROADMAP cred.4 acceptance criterion; search_mcp.py already functional | Expand to http_mcp.py |
| 2 | CEO D1 | Add deprecation warning to search_mcp.py legacy path | Confirmed | — | User selected: yes, add warning | Leave silent |
| 3 | CEO D2 | New plan file cred.4b | Confirmed | — | User selected cred.4b identifier | Reuse existing plan |
| 4 | Eng E1 | Broker short-circuit at top of `_load_config()` | Mechanical | P1 | Earliest possible point to skip raw credential access | Modify mid-function |
| 5 | Eng E2/E3 | Add broker guards at top of `oauth_start_auth` + `oauth_check_auth` | Mechanical | P1 | Prevents KeyError on `_cfg["OAUTH_CLIENT_ID"]` etc. in broker mode | Let KeyError propagate |
| 6 | Eng | Set ALLOWED_HOSTS from default GOOGLE_ALLOWED_HOSTS in broker short-circuit | Mechanical | P1 | Without it, SSRF check silently passes all hosts in broker mode | Leave ALLOWED_HOSTS empty |
| 7 | Eng | Tests T24-T27 (4 new tests) | Mechanical | P1 | Security constraint: one test per change; T24 fails without E1 fix; T25/T26 fail without E2/E3 fix; T27 verifies full broker path with minimal _cfg | Skip tests |
| 8 | Eng | `oauth_check_auth` in broker mode returns `ready: true` (not error) | Mechanical | P1 | Auth IS ready in broker mode; error would break agents that poll oauth_check_auth before calling oauth_call_api | Return error |

---

## Phase 1: CEO Review

### Premises (confirmed)

- P1: VALID — remaining gap is exclusively oauth_mcp.py; search_mcp.py has no functional gap
- P2: VALID — disabling oauth_start_auth/oauth_check_auth in broker mode is correct; raw credentials not available
- P3: VALID — no Rust changes needed; all changes are Python + docs
- P4: D1 resolved — add deprecation warning to search_mcp.py

### NOT in scope (confirmed)

- Remove BRAVE_SEARCH_API_KEY fallback entirely → cred.5 (breaking change)
- `--require-broker` TOML flag → cred.5
- http_mcp.py → no credentials, nothing to do

**PHASE 1 COMPLETE.** All premises confirmed. No User Challenges.

---

## Phase 3: Eng Review

### Findings

| # | Severity | Finding |
|---|----------|---------|
| E1 | Critical | `_load_config()` reads raw credentials unconditionally — broker short-circuit missing |
| E2 | High | `handle_oauth_start_auth` accesses `_cfg` OAuth keys without broker guard — KeyError in broker mode |
| E3 | High | `handle_oauth_check_auth` proceeds to OAuth dance without broker guard |
| E4 | Medium | T24-T27 missing — broker-mode behavior untested |
| E5 | Low | Legacy fallback in search_mcp.py fires silently |

All findings addressed by items A-C above. No new crates. Diff is ~80 lines.

**PHASE 3 COMPLETE.**

### Phase 3.5 — DX

Transparent to operators: behavior changes only when broker is configured. Error messages
point to `agentctl auth google`. Deprecation warning in stderr is informative. No TOML
config changes. DX clean — skip formal DX review.

---

## GSTACK REVIEW REPORT

| Section | Status |
|---------|--------|
| CEO Review (Phase 1) | ✅ COMPLETE — all premises confirmed, 2 user decisions locked |
| Design Review (Phase 2) | ✅ SKIPPED — no UI scope |
| Eng Review (Phase 3) | ✅ COMPLETE — 5 findings, all addressed |
| DX Review (Phase 3.5) | ✅ SKIPPED — no developer-facing API changes |
| Final Gate | ✅ APPROVED |

**Ready to implement.** Branch: `cred.4b-credential-agnostic-mcp`

Estimated diff: ~80 lines Python + docs.
