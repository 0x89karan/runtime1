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

---

## DECISION (autoplan, 2026-07-12): Option A — gated override

Locked at the autoplan gate. Mechanism + implementation:

- **`agentd/src/config.rs`** — add `allow_non_loopback: bool` to `ManagementConfig` (`#[serde(default)]`, default `false`).
- **`agentd/src/management.rs:439`** — relax the guard: `ensure!(bound.ip().is_loopback() || cfg.allow_non_loopback, "...")`. Update the `loopback_guard_rejects_non_loopback` test (still refuses `0.0.0.0` when the flag is false) + ADD a test that `allow_non_loopback=true` permits `0.0.0.0`.
- **`agentd/cos.agents.toml`** + **`distro/overlay/etc/agentd/cos.agents.toml`** — `[management] allow_non_loopback = true` (+ `bind_addr = "0.0.0.0"`). This FIXES the pre-existing QEMU conflict (the overlay already sets `0.0.0.0`; without the flag the guard refuses it → the QEMU management API currently fails to start).
- **`docker-compose.yml`** — cos service `ports: ["127.0.0.1:7999:7999"]` (host loopback ONLY, never bare `7999:7999`).
- **`docs/DEPLOYMENT.md`** — Path 1 step 3 → `agentctl watch --url http://localhost:7999`; drop the `docker compose exec` workaround note.
- **`THREAT_MODEL.md`** — new note: the management API is UNAUTHENTICATED; `allow_non_loopback` is an explicit deployment opt-in; binding `0.0.0.0` in-container exposes spawn/inject/approve/deny to other containers on the CoS compose bridge (operator-controlled sidecars, same trust domain under the single-tenant lock); publish is pinned to host `127.0.0.1` (never LAN). **Revisit per-session auth (Option C) when ux.5 (web cockpit) lands** — a browser reaching `:7999` is a different threat (DNS-rebinding/cross-origin) that needs auth + an Origin/Host allowlist.

**Default stays safe:** agentd default `bind_addr = 127.0.0.1` AND `allow_non_loopback = false` — the guard still refuses non-loopback for any config that doesn't explicitly opt in.

### Acceptance (Option A)
- [ ] From the Mac host (Docker `cos` up): `agentctl watch --url http://localhost:7999` connects — no `docker compose exec`.
- [ ] agentd default bind is `127.0.0.1`; guard STILL refuses `0.0.0.0` when `allow_non_loopback` is unset/false (test).
- [ ] `allow_non_loopback = true` permits the `0.0.0.0` bind (test); the QEMU management API now starts (pre-existing conflict fixed).
- [ ] compose publishes `127.0.0.1:7999:7999` (config assertion — never bare `7999`).
- [ ] THREAT_MODEL documents the opt-in + Docker-bridge exposure + the ux.5-auth follow-up.
- [ ] Every path has a test that fails without the fix. `make clippy-linux` (agentd touches no FUSE here, but run it).

### NOT in scope
Per-session auth / Origin-Host allowlist (Option C → ux.5); any agentctl/watch change (ux.0, shipped); LAN exposure (never).

---

## GSTACK REVIEW REPORT

**Pipeline:** /autoplan (focused — infra/security increment; no UI → Design N/A; DX minimal). **Branch:** ux.0b-host-loopback-reachability.
**Decision (security-sensitive, user-gated):** **Option A — gated override** (`allow_non_loopback` opt-in + `127.0.0.1`-pinned publish + THREAT_MODEL note), auth deferred to ux.5. Premise confirmed. Crux verified against code (management.rs:439 guard, pre-existing QEMU 0.0.0.0 conflict, unauthenticated API).
**Adversarial security pass:** runs at /review on the actual diff (the guard change + the exposure note are the things to pressure-test).

VERDICT: APPROVED — build Option A. The unauthenticated-API-on-Docker-bridge exposure is accepted under the single-tenant lock with host-loopback-pinned publish; revisit auth at ux.5.

NO UNRESOLVED DECISIONS
