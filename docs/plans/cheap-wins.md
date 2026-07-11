# cheap-wins — Google OAuth Production-publish + secret-redaction

**Branch:** `feat/cheap-wins`
**Version:** v0.75.0
**Depends on:** dx.6 (v0.74.0) merge (in-flight PR #105)

<!-- /autoplan restore point: /Users/0x89karan/.gstack/projects/0x89karan-runtime1/feat-cheap-wins-autoplan-restore-20260711-181812.md -->

## Problem

Two orthogonal bugs that block the CoS from working reliably in production:

**(a) Google OAuth Testing-mode trap (cos-polish #9):**
`MCP_SERVERS.md:119` instructs operators to add themselves as a "Test user" on the Google
Cloud Console OAuth consent screen. This puts the app in Testing mode, where refresh tokens
for unverified apps expire after **7 days**. The CoS silently loses Gmail auth every week.
`gmail.readonly` is a non-sensitive scope — unverified apps can publish to Production
and get a "This app isn't verified" browser warning, but tokens don't expire.

**(b) Token-endpoint body leak (cred.6 P0 / cred.5-ar-01):**
Two code paths include the raw HTTP response body from a failed token endpoint call in
their error strings, which propagate into the flight recorder and `provider_last_error`:
1. `docker/oauth_mcp.py:430` — `return f"refresh failed {e.code}: {body}"`
2. `agentd/src/credential/mod.rs:341-342` — `let body = resp.text()...;
   return Err(format!("token refresh HTTP {status}: {body:.512}"))`

RFC 6749 §5.2 error responses technically shouldn't include credential material, but a
misconfigured provider can echo the request. The body serves zero debugging value once
you have the HTTP status code.

## Scope

### In scope
1. `docs/MCP_SERVERS.md:119` — Remove "Add your email as a Test user" line; replace step 3 with
   Production publishing guidance.
2. `docs/MCP_SERVERS.md` — Add `invalid_grant` to the error reference table.
3. `docs/DEPLOYMENT.md` — Add a note about Production publishing in the Google OAuth step.
4. `docker/oauth_mcp.py:430` — Drop body from `_do_refresh()` error return.
   Add test that asserts body content does NOT appear in the return value.
5. `agentd/src/credential/mod.rs:341-342` — Drop body from `get_or_refresh()` error string.
   Add test that asserts body content does NOT appear in the `Err(...)`.
6. `TODOS.md` — Mark cred.5-ar-01 closed.
7. `agentd/Cargo.toml` + `agentctl/Cargo.toml` — bump 0.73.2 → 0.75.0
   (0.74.0 is claimed by dx.6 on PR #105; if this merges first, adjust to 0.74.0).
8. `CHANGELOG.md` — v0.75.0 entry.
9. `ROADMAP.md` — mark cheap-wins shipped (after ROADMAP from dx.6 lands).

### Out of scope
- `oauth_call_api` error body lines (690, 738): those are API response bodies returned to the
  agent — different from token credentials. No PII.
- `agentctl auth google` warning about Testing mode: nice-to-have but zero correctness impact.
- Other cos-polish items (FsWrite cap, KB findability, etc.) — separate increment.
- `cred.3-ar-S3` (SecretRewriter) — a far bigger project; not addressed here.

## Implementation plan

### (a) MCP_SERVERS.md

**Before (line 119):**
```
   Add your email as a **Test user** if the app is in testing mode.
```

**After:**
```
   ⚠️  Do **not** stay in Testing mode — publish the app to **Production** so refresh tokens
   don't expire after 7 days. For `gmail.readonly` (a non-sensitive scope), Google allows
   unverified Production apps: users see a "This app isn't verified" warning but tokens
   stay valid indefinitely.
   To publish: **OAuth consent screen → Publishing status → Publish App**.
```

Also add `invalid_grant` to the error reference table:
```
| `refresh failed 400` / `invalid_grant` | OAuth refresh token expired (Testing-mode 7-day limit) | Publish app to Production on Google Cloud Console → OAuth consent screen |
```

### (b) DEPLOYMENT.md

Add a "> ⚠️ Production mode note" callout box after the Step 3 env block and after Step 4,
noting that the OAuth app must be in Production mode (not Testing mode).

### (c) oauth_mcp.py line 430

```python
# Before:
return f"refresh failed {e.code}: {body}"

# After:
return f"refresh failed {e.code}"
```

Remove the `body` read block too, since body is no longer used in `_do_refresh()`.

Add test T30: `_do_refresh()` with a mocked HTTPError whose body contains
`"access_token=abc123"` — assert result does NOT contain `"abc123"`.

### (d) credential/mod.rs lines 341-342

```rust
// Before:
let body = resp.text().await.unwrap_or_default();
return Err(format!("token refresh HTTP {status}: {body:.512}"));

// After:
return Err(format!("token refresh HTTP {status}"));
```

The `body` variable and `resp.text()` call are removed entirely.

Add test: `get_or_refresh()` against a mock 400 endpoint returning body
`"error=invalid_grant&hint=access_token=REDACTED"` — assert `Err(...)` does not
contain `"REDACTED"`.

## Tests

| Test | Location | What it checks |
|------|----------|---------------|
| T30 | `docker/oauth_mcp.py` self-test | `_do_refresh` error does not include body |
| `test_token_refresh_error_body_redacted` | `agentd/src/credential/mod.rs` | `get_or_refresh` error does not include body |

Total tests: 1271 + 1 (Python) + 1 (Rust) = 1273 expected.

## TODOS.md changes

Close:
- `cred.5-ar-01` — token-refresh error body in provider_health.last_error (addressed by mod.rs fix)

## CHANGELOG

```
## v0.75.0 — 2026-07-11

### Fixed
- **secret-redaction** — OAuth token-refresh error bodies (HTTP non-2xx responses from
  the token endpoint) are no longer included in error strings, flight-recorder events,
  or `provider_last_error`. Only the HTTP status code is retained. Folds `cred.5-ar-01`.
  (`docker/oauth_mcp.py`, `agentd/src/credential/mod.rs`)
- **Google OAuth Testing-mode trap** — `MCP_SERVERS.md` no longer steers operators into
  Testing mode (7-day token expiry). Docs now guide operators to publish the OAuth app to
  Production. Adds `invalid_grant` to the error reference table. (`docs/MCP_SERVERS.md`,
  `docs/DEPLOYMENT.md`)
```

---

## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| 1 | CEO | Strip body entirely (not redact with regex) | Mechanical | P5 (explicit) | Regex over HTTP bodies is fragile and adds no security advantage; status code is sufficient for debugging | Redact with regex |
| 2 | CEO | Single increment for both fixes (docs + code) | Mechanical | P3 (pragmatic) | Both fixes are trivially small and orthogonal; combining saves a PR | Two separate PRs |
| 3 | Eng | Bump to 0.75.0, not 0.74.0 | Mechanical | P6 (action) | 0.74.0 is claimed by dx.6 (PR #105 in-flight); avoid version collision | 0.74.0 |
| 4 | Eng | Remove entire body read block in oauth_mcp.py (not just the return) | Mechanical | P5 (explicit) | Dead code (body variable unused after fix) | Keep body read, just don't use in return |
| 5 | Eng | Do NOT add `agentctl auth google` Testing-mode warning | Mechanical | P3 (pragmatic) | Marginal UX value; the doc fix is the authoritative source; adds Rust compile cost | Add warning |

## GSTACK REVIEW REPORT
