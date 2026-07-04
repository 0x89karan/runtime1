# Plan: Align CoS Harness UX with Pure OS Vision

## Problem

AgentOS has two runtime modes:

1. **Pure OS mode** — `agentd` is PID 1 inside a QEMU VM. Secrets come from a
   host-side virtfs mount (`~/.agentos-secrets/agentos.env`). No display, no
   browser, no host userspace at runtime. This is the constitutional vision.

2. **Docker/dev mode** — `agentd` runs inside an Alpine container on a host OS.
   The CoS harness (cos.agents.toml + oauth_mcp.py + docker-compose.yml) was
   built for this mode.

The CoS harness as built conflicts with the pure OS vision in three ways:

### Conflict 1: OAuth browser dance happens inside the agent loop
`oauth_mcp.py` runs an interactive PKCE flow: it starts a local HTTP server on
port 8585, returns an authorization URL, and waits for the browser callback.
Inside a headless QEMU VM there is no browser, no display, no way to complete
this flow. The dance should happen on the host, once, before the VM boots.

### Conflict 2: agentctl watch TUI is unreachable from outside the VM
The TUI reads `/agents/*` FUSE files that exist inside the VM. There is no
bridge from host → VM FUSE namespace. The approval surface (`/agents/approvals/`)
is similarly inaccessible. Operators have no way to approve `request_approval`
calls from a running QEMU instance.

### Conflict 3: Env var sprawl leaks into the wrong abstraction layer
The current CoS Docker setup requires 9+ env vars, many of which are static
Google OAuth endpoints that should be hardcoded defaults in `oauth_mcp.py`.
The pure OS model has one clean injection point: the virtfs secrets file. The
current pattern trains users to think in env vars rather than secrets mounts.

## Proposed Direction

### Separate "first-time provisioning" from "runtime token use"

The OAuth dance moves to the host, outside the VM, as a one-time setup step:

```bash
# On host (once)
agentos-auth google          # opens browser, completes OAuth, writes token
# → writes ~/.agentos-secrets/google.json (mode 0600)
# → VM reads it at boot via virtfs; oauth_mcp.py uses refresh token directly
```

Inside the VM, `oauth_mcp.py` never initiates an OAuth flow. It reads the
pre-provisioned refresh token from `/run/secrets/google.json` and only calls
`oauth_call_api`. If the token is missing, it fails fast with a clear error
rather than starting a browser dance that can never complete.

### Replace TUI with a network-accessible approval surface

The `/agents/approvals/` FUSE directory is useful when agentd is local.
For the VM case, approvals need a host-reachable surface:

Option A: Serve a minimal HTTP approval UI from inside the VM on a known port
(e.g., 8080). The host opens `http://localhost:8080` in a browser. Virtio-net
with QEMU user-mode networking forwards the port to the host.

Option B: Write pending approvals to a virtfs-mounted directory on the host.
The host operator writes an `approved` or `denied` file; agentd polls it.

Option A is cleaner (single direction, no polling race).

### Harden the secrets model

- `~/.agentos-secrets/agentos.env` — API keys, one per line `KEY=value`
- `~/.agentos-secrets/google.json` — OAuth refresh token (written by `agentos-auth`)
- No OAuth env vars at runtime; `oauth_mcp.py` reads from `/run/secrets/` directly
- Drop `OAUTH_AUTH_URL`, `OAUTH_TOKEN_URL`, `OAUTH_SCOPES`, `OAUTH_ALLOWED_HOSTS`,
  `OAUTH_PROVIDER_NAME` from user-facing config — hardcode Google defaults

## Open Questions

1. Does `agentos-auth` live in `agentctl` (host-side tool) or as a separate binary?
2. HTTP approval UI: served by agentd directly, or by a sidecar (agentos-otel already
   runs as a sidecar — could co-locate)?
3. Does the Docker path (cos service) adopt the same secrets model, or stay
   env-var-based for Docker-native users?
4. Timeline relative to current roadmap (p7.7 and h7.x increments)?

## Acceptance Criteria

- [ ] `agentos-auth google` provisions refresh token to `~/.agentos-secrets/google.json`
- [ ] `oauth_mcp.py` reads token from secrets path; never initiates browser dance
- [ ] VM boots and runs CoS without any interactive OAuth step
- [ ] Pending approvals reachable from host (HTTP or virtfs)
- [ ] Required user-facing env vars reduced to: `ANTHROPIC_API_KEY`, `OAUTH_CLIENT_ID`,
      `OAUTH_CLIENT_SECRET` (needed once for provisioning, not at runtime)
- [ ] Docker cos service works with same secrets model (backward compat via env var
      fallback in oauth_mcp.py)
