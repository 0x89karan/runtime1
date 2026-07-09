# Mac + Docker CoS First-Run Fixes (F1–F4)

**Branch:** cos-first-run-fixes  
**Status:** Planning  
**Source:** Live dogfood run on Mac + Docker Desktop — four failures on the primary dev path.

---

## Problem

The first real end-to-end run of the Chief of Staff on Docker Desktop (Mac) surfaces four
breakages. F4 is a **blocker**: the flagship Gmail path cannot work on any Docker Desktop /
pre-Linux-6.7 kernel because `Net{ports}` degrades to deny-all network. F1–F3 are DX
papercuts that make the path fail silently or give incorrect instructions.

---

## F4 — BLOCKING: Net{ports} → deny-all on no-V4 kernels

### Root cause

`caps_to_rules_inner` (`agentd/src/main.rs:1322–1375`):

```rust
// Line 1330: correctly skips IsolateNetwork when Net is declared
if !has_net { rules.push(SandboxRule::IsolateNetwork); }

// Line 1340–1348: BUT adds IsolateNetwork again in the ports arm
Capability::Net { ports, .. } => {
    if !v4_available {
        // deny-all as "safe fallback"
        rules.push(SandboxRule::IsolateNetwork);  // ← BUG
        continue;
    }
    ...
}
```

When `Net{ports=[443]}` is declared and the kernel has no Landlock V4 (Docker Desktop,
any Linux < 6.7), the server ends up with `IsolateNetwork` despite having declared it
needs outbound access. Token refresh fails → "Not authenticated" → agent tries in-container
browser OAuth → hangs indefinitely.

### Decision (F4 fallback policy)

**Best-effort ALLOW + loud warn** (not deny-all, not a strict-flag).

Rationale:
- The operator declaring `Net{ports=[443]}` explicitly stated "this tool needs network".
  Denying ALL network silently inverts their intent. The current behavior is the exact
  opposite of what the capability declaration means.
- Landlock FS degrades best-effort (allows unrestricted FS reads rather than denying).
  Network should follow the same pattern.
- A `[tools] strict_network_isolation=true` flag adds complexity with unclear user value —
  operators on pre-6.7 kernels are typically Docker Desktop users, not hardened production
  environments. Default must keep networked tools working.
- The warn message must be loud (stderr + flight event) so operators know port-level
  enforcement didn't apply.

### Fix

`agentd/src/main.rs` — change the V4-unavailable arm in `caps_to_rules_inner`:

```rust
if !v4_available {
    // Landlock V4 unavailable (kernel < 6.7): per-port enforcement impossible.
    // Best-effort ALLOW: do not push IsolateNetwork. The server gets unrestricted
    // network rather than deny-all, preserving the operator's declared intent.
    // (FS rules degrade the same way: best-effort allow, not deny.)
    tracing::warn!(
        "Net{{ports={:?}}} declared but Landlock ABI V4 unavailable (kernel < 6.7); \
         per-port enforcement skipped — server has unrestricted network access. \
         Upgrade to Linux ≥ 6.7 for port-level isolation.",
        ports
    );
    // No rule pushed: network is unrestricted (best-effort allow).
    continue;
}
```

### Test

`agentd/src/main.rs` — in `#[cfg(test)]` for `caps_to_rules_inner`:

```rust
#[test]
fn net_ports_v4_unavailable_degrades_to_allow_not_deny() {
    let caps = vec![Capability::Net { ports: vec![443] }];
    let rules = caps_to_rules_inner(&caps, false);  // v4_available = false
    // Must NOT contain IsolateNetwork — deny-all would break networked tools on Docker Desktop.
    assert!(!rules.iter().any(|r| matches!(r, SandboxRule::IsolateNetwork)),
        "Net{{ports}} on no-V4 kernel must degrade to allow, not deny-all: {:?}", rules);
}

#[test]
fn net_ports_v4_available_emits_allow_connect() {
    let caps = vec![Capability::Net { ports: vec![443, 80] }];
    let rules = caps_to_rules_inner(&caps, true);
    let has_connect_443 = rules.iter().any(|r| matches!(r, SandboxRule::AllowNetConnect { port: 443 }));
    let has_connect_80  = rules.iter().any(|r| matches!(r, SandboxRule::AllowNetConnect { port: 80 }));
    assert!(has_connect_443 && has_connect_80);
    assert!(!rules.iter().any(|r| matches!(r, SandboxRule::IsolateNetwork)));
}

#[test]
fn no_net_cap_still_isolates_v4_unavailable() {
    // A server with no Net capability should still get IsolateNetwork regardless of V4.
    let caps = vec![Capability::FsRead { prefix: "/tmp".into() }];
    let rules = caps_to_rules_inner(&caps, false);
    assert!(rules.iter().any(|r| matches!(r, SandboxRule::IsolateNetwork)));
}
```

---

## F1 — ANTHROPIC_API_KEY location not documented in Path 1

### Root cause

DEPLOYMENT.md Path 1, step 2 says `docker compose up -d cos` but never explains that
`ANTHROPIC_API_KEY` must be in the shell env where `docker compose` is invoked. Users
who have it in `~/.zshrc` but open a new terminal, or who have it exported in a different
shell, see the cos service exit immediately with "ANTHROPIC_API_KEY is not set".

The correct and durable path: put it in `~/.agentos-secrets/agentos.env`. The entrypoint
(`docker/entrypoint.sh:11–21`) already reads this file and exports its contents before
checking `ANTHROPIC_API_KEY`. The cos service already mounts `~/.agentos-secrets:/run/secrets:ro`.

### Fix

DEPLOYMENT.md Path 1 — add a "step 0" before `docker compose up`:

```
# 0. Provision secrets (one-time — survives terminal restarts)
mkdir -p ~/.agentos-secrets
printf 'ANTHROPIC_API_KEY=sk-ant-...\n' >> ~/.agentos-secrets/agentos.env
chmod 600 ~/.agentos-secrets/agentos.env
```

Remove the implication that a shell export suffices. Mention that the file wins over
any shell env var (entrypoint precedence).

### Test

Shell test: verify entrypoint reads agentos.env before `check_api_key`:

In `docker/entrypoint.sh`, the ANTHROPIC_API_KEY check already depends on the file-read
block (lines 11–21). No code change needed for F1 — doc-only fix. Verify by reading
the entrypoint and confirming the parse block runs before `check_api_key`.

---

## F2 — `agentctl watch` from Mac host → "127.0.0.1:7999 unreachable"

### Root cause

The `cos` service in docker-compose.yml does not publish port 7999 to the host. The
management API binds to `127.0.0.1:7999` inside the container. `agentctl watch` (run
on the Mac host) tries `http://localhost:7999` → connection refused.

The correct command is `docker compose exec cos agentctl watch` (runs `agentctl watch`
inside the container where the FUSE mount and management API are live).

### Fix

DEPLOYMENT.md Path 1, step 3:

```bash
# 3. Monitor (runs inside the container — FUSE and API are container-local)
docker compose exec cos agentctl watch
```

Add a note: "Running `agentctl watch` directly on the Mac host won't work — port 7999
is loopback-only inside the container and not published. Use `exec` to run inside."

### Test

No code change needed — doc-only fix. Verify that docker-compose.yml has no `ports:` on
the `cos` service (confirmed: only volumes).

---

## F3 — Briefs written to /data inside container, not to ~/.agentos-output on host

### Root cause

DEPLOYMENT.md Path 1 says "Briefs appear in `~/.agentos-output/brief-YYYY-MM-DD.md`".
But the `cos` service volume is `cos-data:/data` with no host bind mount. The CoS agent
writes briefs inside the container at `/data/output/` (or wherever `cd /data/output` lands).
They are never visible on the Mac host.

### Fix (prefer A — host bind mount)

A. **docker-compose.yml**: add a bind mount to the `cos` service:

```yaml
cos:
  volumes:
    - cos-data:/data
    - ${HOME}/.agentos-secrets:/run/secrets:ro
    - ${HOME}/.agentos-output:/data/output  # briefs land on the host
```

And update DEPLOYMENT.md Path 1 to match: "Briefs appear in `~/.agentos-output/brief-YYYY-MM-DD.md`
(the CoS writes to `/data/output` inside the container, which is bind-mounted to this host path)."

This makes `mkdir -p ~/.agentos-output` part of the setup.

Note: the cos entrypoint `cd`s to `/data/output` before running agentd (confirmed in the
init script; same pattern as the distro path). The bind mount wires the container path
directly to the host.

B. (Fallback, doc-only): Correct the docs to say `docker compose exec cos ls /data/output/`.
Prefer A — bind mount is better DX.

### Test

No unit test (infra-only). Verify via smoke test: after bind mount, brief files appear on
the host after a CoS run.

---

## Docker CoS smoke test (CI guard)

Add a minimal Docker smoke test so this path stops rotting silently. It had never been run
end-to-end before the dogfood run.

**What to test:**
- `docker compose build cos` succeeds
- `docker compose up -d cos` starts and passes `GET /healthz` (via `docker compose exec cos
  curl -sf localhost:7999/healthz`)
- At least one networked MCP tool call succeeds with a stub credential
  (or: management API responds to `/api/v1/snapshot`, confirming agentd is running)

**Where:** `.github/workflows/ci.yml` — new `docker-cos-smoke` job, gated on `build-and-test`.

---

## Acceptance criteria

- **F4:** `caps_to_rules_inner(&[Net{ports:[443]}], v4_available=false)` returns NO `IsolateNetwork`
  rule. Docker Desktop CoS reaches Gmail (token refresh succeeds).
- **F1:** DEPLOYMENT.md Path 1 has explicit `agentos.env` setup step before `docker compose up`.
  No mention of shell export for API key.
- **F2:** DEPLOYMENT.md Path 1 step 3 uses `docker compose exec cos agentctl watch`.
  Old plain `agentctl watch` instruction removed.
- **F3:** docker-compose.yml `cos` service has `~/.agentos-output:/data/output` bind mount.
  DEPLOYMENT.md matches. Briefs land on the host after a real CoS run.
- `/review` + `/qa` clean.

---

## Files to change

| File | Change |
|------|--------|
| `agentd/src/main.rs` | `caps_to_rules_inner`: V4-unavailable → best-effort allow + warn (+ 3 tests) |
| `docs/DEPLOYMENT.md` | F1: add agentos.env step; F2: exec-based watch; F3: output path |
| `docker-compose.yml` | F3: `~/.agentos-output:/data/output` bind mount on `cos` |
| `.github/workflows/ci.yml` | Docker CoS smoke test job |

## NOT in scope

- Per-port enforcement on older kernels via an alternative mechanism (iptables/nftables) — future increment
- Publishing port 7999 to the host (loopback-only is a security property, not a bug)
- `strict_network_isolation` config flag — rejected in F4 decision (complexity with no present user value)
- Changing DEPLOYMENT.md Path 2 (Linux QEMU) — separate path, not affected by these bugs

<!-- GSTACK REVIEW REPORT -->
