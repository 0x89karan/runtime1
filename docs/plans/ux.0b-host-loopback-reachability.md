# ux.0b — Host-loopback reachability for the management API (security increment)

**Increment:** ux.0b (Track UX cockpit). Split out of ux.0 by the autoplan final gate (2026-07-11) because it is a security decision, not a config edit.
**Depends on:** ux.0 (loop refactor). **Blocks:** nothing in the cockpit lane strictly, but the cockpit is only *live from the Mac host* once this lands (until then, run `agentctl watch` inside the container or against FUSE).
**Needs:** `/plan-eng-review` (security-sensitive: unauthenticated API + bind-guard change).

## Problem

`agentctl watch --url http://localhost:7999` from the Mac host must reach the Docker `cos` container's management API. Docker `-p` **cannot** reach a service bound to `127.0.0.1` *inside* the container, so the API must be reachable on `0.0.0.0:7999` in-container. But:

- **Fail-closed guard:** `agentd/src/management.rs:438` does `ensure!(bound.ip().is_loopback(), …)` with an enforcing test `loopback_guard_rejects_non_loopback` (`:640`). Setting `bind_addr = "0.0.0.0"` **refuses to start**.
- **Pre-existing conflict (fix here):** `distro/overlay/etc/agentd/cos.agents.toml:28` already sets `bind_addr = "0.0.0.0"` — so the QEMU management API is likely hitting this guard today. Flagged to the build session; ux.0b resolves it.
- **Exposure:** the management API is **unauthenticated** (spawn/inject/approve/deny). Binding `0.0.0.0` in-container exposes it to every peer on the Docker bridge network, not just host loopback.

## Options (decide via /plan-eng-review)

- **A — gated override.** Add `[management] allow_non_loopback = true` (default false). When set, the guard permits a non-loopback bind; the deployment opts in explicitly. Publish pinned to `127.0.0.1:7999:7999` (never bare `7999:7999`). THREAT_MODEL note: Docker-bridge-peer exposure of the unauthenticated API, accepted under the single-tenant lock. Update the guard test. Smallest change; keeps the API unauthenticated.
- **B — loopback-forwarding proxy.** agentd stays `127.0.0.1` (guard untouched); a small forwarder listens `0.0.0.0:7999` in-container → `127.0.0.1:7999`. Reuse the cred.3.1 `LoopbackForwardingProxy` seam. Guard preserved, but bridge-peer exposure still exists via the forwarder + an extra process.
- **C — add auth.** A per-session token / bearer on the management API for non-loopback binds. Biggest change; the only option that actually closes the exposure. Consider if the cockpit ever goes browser/web (ux.5 already needs an Origin/Host allowlist).

**Lean:** A for the CoS single-container case (publish is host-loopback-pinned; bridge peers are operator-controlled sidecars under the single-tenant lock), with the THREAT_MODEL note explicit; revisit C when ux.5 (web cockpit) lands. Confirm at eng review.

## Scope (once the option is chosen)
`agentd/src/config.rs` (+ the flag or proxy), `agentd/src/management.rs` (guard) OR a forwarder module; `agentd/cos.agents.toml` + `distro/overlay/etc/agentd/cos.agents.toml` (resolve the conflict); `docker-compose.yml` (`127.0.0.1:7999:7999`); `docs/DEPLOYMENT.md` (Path 1 → `--url http://localhost:7999`, drop the exec-only note); `THREAT_MODEL.md`.

## Acceptance
- From the Mac host (Docker `cos` up): `agentctl watch --url http://localhost:7999` connects.
- agentd default `bind_addr` still `127.0.0.1` (guard intact for the default); non-loopback only via the explicit opt-in / proxy.
- The pre-existing QEMU `0.0.0.0`/guard conflict is resolved (management API starts in the QEMU deployment).
- THREAT_MODEL documents the Docker-bridge exposure + the mitigation; compose publish is `127.0.0.1`-pinned.
- Every path has a test that fails without the fix.
