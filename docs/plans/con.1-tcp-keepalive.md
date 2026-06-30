# con.1 — TCP Keepalive for Inference Gateway

**Version:** v0.49.0  
**Status:** Planning  
**Branch:** main  

## Problem

The `cos.1` always-on system uses MCP `wait_for_trigger` calls that block the
scheduler for 20 seconds per poll cycle. Docker's netfilter/conntrack silently
drops idle TCP connections after ~30 seconds of inactivity. After two consecutive
`wait_for_trigger` calls (~40 seconds of accumulated idle time), the Anthropic
API connection in reqwest's connection pool is dead. When the scheduler resumes
and makes a third inference request, reqwest tries to reuse the dead connection,
producing an `inference_error: "sending streaming request to Anthropic API"`.

The `cos-orchestrator` agent consistently fails at turn 3 with this error:
```
[flight event] inference_error: sending streaming request to Anthropic API
```

**Root cause:** `SO_KEEPALIVE` is not set on the reqwest client sockets.
Without it, Docker's conntrack drops idle connections silently. The requisite fix
is to enable TCP keepalives at the application level.

**Stopgap currently in place (to be removed):**  
`docker/entrypoint.sh` patches `streaming  = true` → `streaming  = false` in the
cos config. This is a hack and must be removed as part of this increment.

## Fix

Add `tcp_keepalive(Duration::from_secs(15))` to the `reqwest::Client::builder()`
chain in `AnthropicGateway::from_env()`. This sets `SO_KEEPALIVE` on all
connections, causing the kernel to send TCP keepalive probe packets every 15
seconds during idle periods — keeping Docker's conntrack entry alive even during
the 20-second MCP wait windows.

15 seconds was chosen because it is less than the 20-second `wait_for_trigger`
blocking interval, guaranteeing at least one probe per MCP wait cycle.

## Files Changed

### `agentd/src/inference/anthropic.rs`
Add `tcp_keepalive` to the `Client::builder()` in `AnthropicGateway::from_env()`:

```rust
let client = Client::builder()
    .timeout(std::time::Duration::from_secs(120))
    .redirect(reqwest::redirect::Policy::none())
    .tcp_keepalive(std::time::Duration::from_secs(15))  // ← add this
    .build()
    .context("building HTTP client")?;
```

### `docker/entrypoint.sh`
Remove the stopgap sed line that patches `streaming = false`:
```diff
-  -e 's|streaming  = true|streaming  = false|' \
```

## Not In Scope

- `docker-compose.yml` sysctls (`net.ipv4.tcp_keepalive_*`) — already present,
  keep as belt-and-suspenders; they control probe interval and count once
  `SO_KEEPALIVE` is set by the app.
- No new `Capability` variants needed.
- No new `EventKind` variants needed (connection recovery is transparent to agents).
- No new TOML config fields — keepalive is appropriate for all deployment
  contexts (baremetal, QEMU, Docker); not operator-configurable.
- The `McpHttpClient` (p7.1) uses a separate reqwest client; it connects to
  external MCP servers over HTTPS. Same keepalive fix applies if needed, but
  MCP HTTP connections are short-lived (per-request), so conntrack drop is not
  a practical concern there. Defer.

## Acceptance Criteria

1. `cargo build` succeeds.
2. `cargo clippy -- -D warnings` passes.
3. `cargo test` passes (all 1027+ tests).
4. `docker/entrypoint.sh` no longer contains the `streaming  = false` sed patch.
5. `agentd/src/inference/anthropic.rs:from_env` includes `.tcp_keepalive(Duration::from_secs(15))`.
6. `agentd/cos.agents.toml` retains `streaming  = true`.
7. `make clippy-linux` passes (touches inference code, Linux clippy required).

## What Already Exists

- `reqwest 0.12` with `rustls-tls` feature — `ClientBuilder::tcp_keepalive()`
  is available and stable.
- `docker-compose.yml` already has `sysctls` for `tcp_keepalive_time/intvl/probes`
  — these complement the app-level fix.
- The `AnthropicGateway::from_env` already uses `Client::builder()` at line 29
  of `agentd/src/inference/anthropic.rs`.

## Threat Model / Security

No new attack surface. TCP keepalive is a standard OS socket option that only
affects how the kernel manages idle connections. It does not expose new ports
or change auth behavior.

## GSTACK REVIEW REPORT

(populated by /autoplan)
