# cred.3.1 — Credential Broker Hardening Gate

**Version target:** v0.61.0  
**Gating:** All 10 items must be green before cred.4 or orch.1 begin.  
**Requirement (NON-NEGOTIABLE):** For every item — fix + a test that fails without it +
adversarial verification of the real failure path. Update THREAT_MODEL / CLAUDE.md /
ROADMAP to reflect what is actually true.

---

## Context

cred.3 (v0.60.0) shipped the credential broker architecture. A 4-voice audit found the
direction sound but identified 10 gaps that make it unsafe to build cred.4 or orch.1 on
top of. This increment closes all 10.

Key files:
- `agentd/src/credential/mod.rs` — CredentialGateway, CredentialRegistry, OAuthTokenCache
- `agentd/src/egress.rs` — EgressProxy, ProxyRegistry, `content_audited` lie (line 150)
- `agentd/src/evidence.rs` — EvidenceWriter, signing key on disk
- `agentd/src/main.rs` — OV-1 startup invariants, egress_key_path wiring

---

## Group A — Egress guards

### ar-10: Extract shared LoopbackForwardingProxy

**Problem:** `EgressProxy` (egress.rs, ~1173 lines) and `CredentialGateway`
(credential/mod.rs, ~1025 lines) each have their own loopback HTTP proxy implementation.
Security guards (body-size cap, connect timeout, redirect policy, hop-by-hop header
stripping, loopback-only assert) live in two separate copies with no shared enforcement.
A fix in one does not flow to the other.

**Fix location:** Extract a `LoopbackForwardingProxy` struct in a new
`agentd/src/loopback_proxy.rs` module. Both EgressProxy and CredentialGateway delegate
to it. The shared struct enforces all guards exactly once:
- `redirect(Policy::none())` — already in both; moved to one location
- `connect_timeout(10s)` — already in both; moved to one location  
- `MAX_BODY_BYTES` — already in both with different values; unified per-use-site
- `loopback_assert` — verify `127.0.0.1` bind in constructor

The extraction does not change any external API; both structs keep their public interface.

**Test that fails without this fix:**
```rust
// tests/loopback_proxy.rs
// T-ar10-drift: two instances with different redirect policies diverge
// Without the shared struct, this test can only catch drift by running both independently.
// With the struct: assert proxy.inner.redirect_policy == Policy::none() — single source.
fn test_egress_and_cred_gateway_share_redirect_policy() {
    // This test is structural: if the shared struct exists and both use it,
    // any divergence is a compile error, not a runtime test. The test asserts
    // the extracted proxy's redirect policy is none() and both callers use it.
}
```
More precisely: write a clippy lint or unit test that asserts both `EgressProxy::new_client`
and `GatewayState::new_client` call `LoopbackForwardingProxy::build_client()` and that
no other `reqwest::Client::builder()` call exists in egress.rs or credential/mod.rs.

**Adversarial verification:** Introduce a `redirect(Policy::limited(3))` call in
credential/mod.rs only. With the shared struct this is a compile error. Without it,
`test_cred_gateway_no_redirect` passes while `test_egress_no_redirect` also passes —
silent divergence.

---

### ar-04: SSRF host-allowlist on upstream_base

**Problem:** `CredentialGateway::start()` validates `upstream_base.starts_with("https://")`
(line 668) but never resolves the hostname. `https://169.254.169.254/latest/meta-data/`
passes validation and the broker would attach live bearer tokens to IMDS requests.

**Fix location:** `agentd/src/credential/mod.rs` — `CredentialGateway::start()`.  
After the `https://` check, extract the hostname from the URL and resolve it via
`tokio::net::lookup_host()`. Reject if any resolved `IpAddr` is:
- Loopback: `is_loopback()`
- Private (RFC 1918): 10/8, 172.16/12, 192.168/16
- Link-local: 169.254/16 (IMDS)
- IPv6 loopback (`::1`) and link-local (`fe80::/10`)

Add helper `fn is_ssrf_blocked(addr: IpAddr) -> bool` in credential/mod.rs (mirrors
`docker/oauth_mcp.py:_is_ssrf_blocked()`).

**Test that fails without this fix:**
```rust
// In tests mod in credential/mod.rs
#[tokio::test]
async fn test_ssrf_imds_blocked_at_startup() {
    // upstream_base = "https://169.254.169.254/" must be rejected
    let cfg = make_cfg_with_upstream("https://169.254.169.254/");
    let result = CredentialGateway::start(&cfg, recorder()).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("SSRF"));
}

#[tokio::test]
async fn test_ssrf_private_rfc1918_blocked() {
    // upstream_base resolving to 192.168.1.1 must be rejected
    // Use a mock DNS approach or test is_ssrf_blocked() directly
    assert!(is_ssrf_blocked("192.168.1.1".parse().unwrap()));
    assert!(is_ssrf_blocked("169.254.169.254".parse().unwrap()));
    assert!(is_ssrf_blocked("127.0.0.1".parse().unwrap()));
    assert!(!is_ssrf_blocked("142.250.80.46".parse().unwrap())); // google.com
}
```

**Adversarial verification:** Configure `upstream_base = "https://169.254.169.254/"`.
Without the fix, `start()` returns `Ok`. With the fix, it returns
`Err("SSRF: resolved address is private/link-local")`. The test must use the real
`is_ssrf_blocked()` function with a hardcoded IP, not a mock, to prove the guard works.

---

### ar-08: Header whitelist (allow-list, not deny-list)

**Problem:** `SCRUB_HEADERS` (credential/mod.rs:356) is a deny-list of 7 headers. Any
header not in this list passes through from the MCP server to the upstream. A compromised
MCP server can inject `X-Forwarded-For`, `X-Real-IP`, `X-Cloud-Trace-Context`, or any
other header that the upstream provider trusts. The broker becomes a header injection
vector.

**Fix location:** `agentd/src/credential/mod.rs` — `handle_credential_request()`, step 10.  
Replace the deny-list scrub with an explicit allow-list:

```rust
// Headers explicitly forwarded from caller to upstream (all others dropped).
const PASSTHROUGH_HEADERS: &[&str] = &[
    "content-type",
    "accept",
    "accept-encoding",
    "accept-language",
    "cache-control",
    "x-goog-api-version",     // Google APIs version negotiation
    "x-goog-user-project",    // Google Cloud billing project
];
```

Step 10 becomes: `if !PASSTHROUGH_HEADERS.contains(&name_lower) { continue; }`.

The broker always adds:
- `Authorization: Bearer <token>` or `X-Api-Key: <key>` (auth attach, step 11)
- `Content-Length` (set by reqwest from body bytes)
- `Host` (set by reqwest from the URL)

**Test that fails without this fix:**
```rust
#[test]
fn test_header_injection_blocked() {
    // An injected X-Forwarded-For header must NOT appear in the upstream request.
    // Test by building a scrubbed header map and checking the result.
    let injected_headers = vec![
        ("x-forwarded-for", "1.2.3.4"),
        ("x-real-ip", "5.6.7.8"),
        ("x-cloud-trace-context", "abc123"),
        ("content-type", "application/json"),  // this one SHOULD pass
    ];
    let forwarded = apply_header_allowlist(&injected_headers);
    assert!(!forwarded.contains_key("x-forwarded-for"));
    assert!(!forwarded.contains_key("x-real-ip"));
    assert!(!forwarded.contains_key("x-cloud-trace-context"));
    assert!(forwarded.contains_key("content-type"));
}
```

**Adversarial verification:** Send a request with `X-Forwarded-For: 1.2.3.4` from a
mock MCP server to the credential gateway. Without the fix: the header reaches the
upstream in the httpmock assertion. With the fix: the header is absent from the upstream
request.

---

## Group B — OAuth lifecycle

### ar-06: Read state_path back on startup (rotation survives restart)

**Problem:** `OAuthTokenCache::new()` always initializes with
`token: None, expires_at: 0, refresh_token: None` (credential/mod.rs:143). Even when a
valid `state_path` file exists on disk from a previous run, it is never read. The first
request after restart always triggers a full OAuth refresh. If the previous run rotated
the refresh token (Google rotates on use), the old refresh token written to `state_path`
is the only source of truth — but it is ignored, and the broker re-uses the stale token
from the secrets file instead. This breaks rotation survival across restarts.

**Fix location:** `agentd/src/credential/mod.rs` — `OAuthTokenCache` / `get_or_refresh()`.  
Add `OAuthTokenCache::load_from_disk(state_path)` called at first use (inside the mutex,
before the fast-path check). The method reads and parses `state_path`, and if the file
exists and `expires_at > now`, pre-populates `inner.token`, `inner.expires_at`, and
`inner.refresh_token`. If the file is absent or malformed, treat as cold start (no error).

Alternative: call `load_from_disk` once at `GatewayState::new()` time for each provider
that has a `state_path`.

**Test that fails without this fix:**
```rust
#[tokio::test]
async fn test_state_path_loaded_on_cold_start() {
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    // Write a valid state file with a far-future expiry.
    let state = OAuthState {
        access_token: "cached_token".to_string(),
        expires_at_unix: u64::MAX / 2,  // far future
        refresh_token: Some("rotated_refresh".to_string()),
    };
    std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();

    // Create a fresh OAuthTokenCache — simulates restart.
    let cache = OAuthTokenCache::new();
    // Load from state_path (the fix).
    cache.load_from_disk(state_path.to_str().unwrap()).await;

    // Without the fix: inner.token is None, test fails.
    // With the fix: inner.token = Some("cached_token"), no refresh needed.
    let guard = cache.state.lock().await;
    assert_eq!(guard.token.as_deref(), Some("cached_token"));
    assert_eq!(guard.refresh_token.as_deref(), Some("rotated_refresh"));
}
```

**Adversarial verification:** Write a `state_path` with a rotated refresh token. Kill
agentd. Restart. Without the fix: first request calls the token endpoint with the
original (now invalid) refresh token → 401. With the fix: first request uses the cached
access token or the rotated refresh token → succeeds without network call.

---

### ar-07: Deny-by-default provider scoping + scope tokens to owning agent

**Problem — part A (scope tokens to owning agent):** `CredentialRegistry` maps token →
`(agent_id, allowed_providers)`. `agent_id` is recorded and logged but never used as a
second factor in the lookup. Two different agents with different `agent_id`s but both
holding valid tokens can call the same provider. Since tokens are per-spawn (UUID4) this
is currently harmless, but the `agent_id` field implies scoping that isn't enforced.

**Problem — part B (deny-by-default):** `Capability::Credential { provider }` is
checked during `caps_to_rules()` in main.rs to build `allowed_providers`. If an agent
config does not declare the capability, the MCP server is registered without any
`allowed_providers`. Step 4 of `handle_credential_request()` checks
`!allowed_providers.contains(&provider)` — correctly denies. But if a caller calls with
an empty `allowed_providers` and a provider name that isn't in any whitelist, the code
falls through to step 5 (provider config lookup) correctly. The gap: there's no explicit
`deny: all providers if allowed_providers.is_empty()` fast path with a clear event.

**Fix location:** `agentd/src/credential/mod.rs` — `handle_credential_request()`.

Part A: Add `agent_id` parameter to the forward path event. Enforce that the token's
registered `agent_id` matches the requesting `agent_id` if the caller includes an
`x-agent-id` header (future mesh use). For now: log the mismatch as a
`CredentialDenied` event with `reason: "agent_id_mismatch"`.

Part B: Add explicit fast-path before step 4:
```rust
if allowed_providers.is_empty() {
    // Record and 403: no providers configured for this token.
    state.recorder.record(agent_id, None, EventKind::CredentialDenied,
        json!({"reason": "no_providers_configured", "agent_id": agent_id}));
    return Ok(json_response(403, json!({"error": "no_credential_providers_configured"})));
}
```

**Test that fails without this fix:**
```rust
#[tokio::test]
async fn test_empty_allowed_providers_denied_explicitly() {
    // Register a token with empty allowed_providers.
    // Make a request to any provider.
    // Without the fix: falls through to "provider not in config" 503 (wrong error code/reason).
    // With the fix: returns 403 with "no_providers_configured" before hitting step 5.
    // ... test that EventKind::CredentialDenied is emitted with reason="no_providers_configured"
}
```

**Adversarial verification:** Register a token with `allowed_providers = []`. Send a
request for any provider. Without the fix: returns 503 with "credential_not_provisioned"
(misleading — implies a config problem, not a capability gap). With the fix: returns 403
with an explicit denial event. The event reason must appear in the flight log.

---

## Group C — Audit truthfulness

### S1: Signing key not readable by any MCP FsRead capability

**Problem:** The Ed25519 signing key is stored at `cfg.egress.key_path` (evidence.rs,
loaded in `EvidenceWriter::open()`). In `main.rs`, OV-1 checks that `evidence_path`
does not fall inside any MCP server's `FsWrite` sandbox prefix. There is no analogous
check for `egress_key_path` vs `FsRead` prefixes. An MCP server with
`AllowFsRead("/run/evidence")` can read the private signing key if `key_path` is inside
that prefix.

**Fix location:** `agentd/src/main.rs` — OV-1 startup invariant block.  
Add a check parallel to the existing evidence/memory OV-1 guards:
```rust
// Enforce: egress key_path must not fall inside any MCP FsRead prefix.
for server in &cfg.tools.mcp_servers {
    for rule in caps_to_rules(&server.capabilities) {
        if let SandboxRule::AllowFsRead(prefix) = rule {
            anyhow::ensure!(
                !normalize_path(&egress_key_path).starts_with(&normalize_path(&prefix)),
                "OV-1: egress signing key '{}' is inside MCP server '{}' FsRead prefix '{}' — \
                 move key_path outside the MCP server's readable directory",
                egress_key_path, server.name, prefix
            );
        }
    }
}
```

**Test that fails without this fix:**
```rust
#[test]
fn test_ov1_egress_key_inside_fsread_prefix_rejected() {
    // Config: key_path = "/run/evidence/signing.key"
    // MCP server: AllowFsRead("/run/evidence")
    // Without the fix: startup succeeds — key is readable by the MCP server.
    // With the fix: startup returns Err("OV-1: egress signing key ...").
    let result = check_egress_key_guard("/run/evidence/signing.key", "/run/evidence");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("OV-1"));
}

#[test]
fn test_ov1_egress_key_outside_fsread_prefix_allowed() {
    let result = check_egress_key_guard("/run/egress/signing.key", "/run/evidence");
    assert!(result.is_ok());
}
```

**Adversarial verification:** Configure `key_path = "/run/evidence/signing.key"` and an
MCP server with `AllowFsRead("/run/evidence")`. Without the fix: `cargo run` succeeds
and a subsequent `read_file("/run/evidence/signing.key")` in the MCP server returns the
PKCS8 private key bytes. With the fix: startup fails with the OV-1 message before any
MCP server is spawned.

---

### S2: Make content_audited true or drop it from the event

**Problem:** `EventKind::EgressBrokered` emits `"content_audited": true` hardcoded at
egress.rs:150. No content auditing is implemented — the broker forwards inference
responses without scanning them. The flight log records a lie that auditors and the
OTLP sidecar may rely on to assert compliance.

**Fix:** Two options:
- A) Drop the field (honest: no audit claimed). Change line 150 to remove `"content_audited"`.
- B) Implement a real `SecretRewriter` that scans tool output for `ANTHROPIC_API_KEY`-shaped
  tokens and redacts them — then set `"content_audited": true` truthfully.

**Recommended:** Option A for cred.3.1. Option B is cred.3-ar-S3 (below). Doing both
in one increment is scope creep. Drop the field now; the field can be added back when
S3 is built.

Fix location: `agentd/src/egress.rs:150` — remove `"content_audited": true`.
Update `agentd/src/events.rs:99` — remove `content_audited` from the doc comment.

**Test that fails without this fix:**
```rust
#[test]
fn test_egress_brokered_event_has_no_content_audited_lie() {
    // Build an EgressBrokered event via record_inference().
    // Parse the JSON payload.
    // Fail if "content_audited" key is present (it's a lie — nothing is audited).
    let event_json = capture_egress_brokered_event();
    assert!(!event_json.contains_key("content_audited"),
        "content_audited field must not be present until real auditing is implemented");
}
```

**Adversarial verification:** Run the existing flight log and grep for
`"content_audited":true`. Without the fix: all `egress_brokered` events contain the
lie. With the fix: the key is absent. The OTLP sidecar's `SpanBuilder` must also be
checked to ensure it does not forward this field (currently it reads all data fields).

---

### S3: Build the SecretRewriter or de-claim it in THREAT_MODEL

**Problem:** `events.rs:99` documents a `content_audited` field on `EgressBrokered`
(now removed by S2). No `SecretRewriter` struct exists in the codebase. p7.5 was
described as including "boundary secret rewriting" — but the implementation only
generates signed receipts; the SecretRewriter component that was supposed to scan
tool outputs for leaked API keys was never built.

**Fix for cred.3.1 (de-claim):**
- Update `THREAT_MODEL.md` §5 (or wherever the boundary-rewriting claim appears):
  state explicitly "SecretRewriter is not implemented; tool output is not scanned for
  credential-shaped tokens."
- Update the `p7.5` summary in CLAUDE.md to remove or qualify the "boundary secret
  rewriting" claim.
- File `cred.3-ar-S3` in TODOS.md as a future item (P2): build a real `SecretRewriter`
  that scans `ToolResult` content for `sk-ant-*`, `Bearer `, and `BRAVE-SEARCH-*` shaped
  tokens before they reach the flight log.

**Test that fails without this fix (truthfulness, not implementation):**
No Rust test can verify a missing de-claim. The gate for S3 is:
1. THREAT_MODEL.md contains an explicit "NOT IMPLEMENTED" note where boundary-secret
   rewriting was implied.
2. `grep -r "SecretRewriter\|content_audited" agentd/src/` returns zero results (after
   S2 removes the `content_audited` field).
3. TODOS.md has `cred.3-ar-S3` as an open item.

**Adversarial verification:** Read THREAT_MODEL.md. If any sentence implies secret
rewriting is active, the gate fails. The explicit statement must be present.

---

## Documentation — Record before mesh/orch.1

### ar-09: Shared credential service is a mesh prerequisite

**Location:** `docs/ROADMAP.md` — orch.1 entry.  
Add to the orch.1 prerequisites section:

> **Prerequisite (cred.3.1):** orch.1 assumes a stable credential broker API.
> The broker must be hardened (all cred.3.1 gate items green) before orch.1 begins.
> The current broker model is single-host; the mesh credential story (orch agents
> on different hosts sharing a broker) is deferred to cred.5+.

This is documentary only — no code change.

### Universal-tier has no credential path

**Location:** `docs/THREAT_MODEL.md` §8 (or new §8.6).  
Add explicit documentation:

> **§8.6 Universal-tier agents and credentials**
> Universal-tier agents (gVisor/runsc subprocesses) currently receive NO credential
> gateway access. They are spawned with an ephemeral `ANTHROPIC_API_KEY` (for
> inference only, via ProxyRegistry) but do not receive `AGENTD_CREDENTIAL_TOKEN`
> or `AGENTD_CREDENTIAL_GATEWAY_URL`. Universal-tier code that calls MCP servers
> requiring OAuth or API-key credentials will receive 503 from those servers.
> This is intentional for cred.3: universal-tier credential plumbing is deferred
> to cred.4 or cred.5.

---

## Implementation order

1. S2 (drop content_audited) — 1 line + 1 doc + 1 test — do first, zero risk
2. S3 (de-claim SecretRewriter) — doc + TODOS entry — do immediately after S2
3. ar-06 (load state_path on startup) — medium complexity, important for correctness
4. ar-08 (header allowlist) — medium complexity, important for security
5. ar-04 (SSRF DNS check) — medium complexity, async DNS resolution
6. ar-07 (deny-by-default fast path) — low complexity
7. S1 (OV-1 FsRead guard) — low complexity, parallel to ar-07
8. ar-10 (extract LoopbackForwardingProxy) — highest refactor scope, do last of the 10
9. ar-09 + universal-tier doc — pure docs, add at end with the ROADMAP/THREAT_MODEL pass

---

## Acceptance criteria

All of the following must pass before cred.4 or orch.1 begin:

- [ ] `cargo test` passes with ≥ 2 new tests per item (fix + adversarial verification)
- [ ] `cargo clippy -- -D warnings` clean
- [ ] `make clippy-linux` clean (Linux-gated code)
- [ ] `grep -r "content_audited" agentd/src/` returns zero results
- [ ] `grep -r "SecretRewriter\|secret_rewriter" agentd/src/` returns zero results
- [ ] THREAT_MODEL §8.3 updated to note SSRF DNS check present
- [ ] THREAT_MODEL §8.5 updated to describe allow-list model
- [ ] THREAT_MODEL §8.6 added for universal-tier no-credential-path
- [ ] THREAT_MODEL contains explicit "NOT IMPLEMENTED" for SecretRewriter
- [ ] TODOS.md has `cred.3-ar-06` through `cred.3-ar-10` and `cred.3-ar-S1` through `cred.3-ar-S3`
- [ ] ROADMAP.md orch.1 notes cred.3.1 as prerequisite
- [ ] CLAUDE.md "Current status" updated for v0.61.0

---

## Files changed (expected)

| File | Change |
|---|---|
| `agentd/src/credential/mod.rs` | ar-04 (SSRF check), ar-06 (load state_path), ar-07 (deny fast path), ar-08 (allow-list) |
| `agentd/src/egress.rs` | S2 (drop content_audited field) |
| `agentd/src/events.rs` | S2 (remove content_audited from doc comment) |
| `agentd/src/main.rs` | S1 (OV-1 egress key FsRead guard) |
| `agentd/src/loopback_proxy.rs` | ar-10 (new extracted shared proxy struct) |
| `agentd/src/lib.rs` | ar-10 (add mod loopback_proxy) |
| `agentd/Cargo.toml` | Version bump to 0.61.0 |
| `agentctl/Cargo.toml` | Version bump to 0.61.0 |
| `docs/THREAT_MODEL.md` | S2, S3, ar-09, ar-10 doc updates |
| `docs/ROADMAP.md` | ar-09 orch.1 prerequisite note |
| `TODOS.md` | ar-06..ar-10, S1..S3 entries |
| `CLAUDE.md` | Current status block for v0.61.0 |
| `CHANGELOG.md` | v0.61.0 entry |
