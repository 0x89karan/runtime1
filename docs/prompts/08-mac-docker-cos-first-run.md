# 08 — Mac + Docker CoS first-run fixes (build-session kickoff)

> **✅ Shipped — dx.4b (v0.71.0, PR #96). Historical; kept for reference.**

Paste the block below. Full plan: `docs/plans/mac-docker-cos-first-run.md`.

---

```
TASK: Mac + Docker CoS first-run fixes (F1–F4 from a live dogfood run, 2026-07-09)

CONTEXT
First real end-to-end run of the Chief of Staff on Docker Desktop (Mac). The container
starts and the cron loop fires, but the flagship Gmail path DOES NOT WORK, plus three DX
papercuts. Full plan: docs/plans/mac-docker-cos-first-run.md.

Two groups — F4 is the blocker (core + a security decision); F1–F3 are DX/docs. Ship as
one or two increments; confirm via /autoplan. F4 needs /plan-eng-review on the fallback policy.

━━ GROUP B — F4 (BLOCKING, headline) ━━
Net{ports} falls back to DENY-ALL network on kernels without Landlock ABI V4.
- Symptom: CoS inbox agent reports "Not authenticated" (valid google.json present) and tries
  the dead in-container OAuth flow → hang.
- Root cause: google_oauth declares Net{ports=[443]} (cos.agents.toml:17-23); Docker Desktop
  has no Landlock ABI V4 (needs Linux ≥6.7), so caps_to_rules_inner (main.rs:1344-1349)
  pushes IsolateNetwork (deny-all) → the sidecar has NO network → token refresh fails.
- Impact: breaks EVERY sandboxed network MCP tool (google_oauth, http_fetch, web_search) on
  Docker Desktop / any pre-6.7 kernel — the primary Mac dev path.
- FIX (decide via /plan-eng-review): a server that DECLARED Net{ports} must not be denied all
  network when V4 is unavailable. Degrade best-effort-ALLOW + loud warn (like Landlock FS
  degrades best-effort), OR gate deny-all behind [tools] strict_network_isolation=true
  (default off). Default must keep networked tools working on common kernels.
- TEST: caps_to_rules_inner(&[Net{...}], v4_available=false) yields allow/no-net-isolation
  (or the strict-flag path), NOT IsolateNetwork; and the Docker CoS reaches Gmail.
- Where: agentd/src/main.rs:1314-1349, sandbox/src/lib.rs.

━━ GROUP A — Docker first-run DX ━━
F1  DEPLOYMENT.md Path 1 doesn't say where ANTHROPIC_API_KEY goes → cos exits
    "ANTHROPIC_API_KEY not set" when exported in a different shell than `docker compose up`.
    FIX: Path 1 instructs putting the key in ~/.agentos-secrets/agentos.env (cos mounts it +
    entrypoint sources it), not a shell export. Where: docs/DEPLOYMENT.md.
F2  Host `agentctl watch` → "127.0.0.1:7999 unreachable": cos publishes no port + management
    is loopback-bound inside the container. FIX: Path 1 uses `docker compose exec cos agentctl
    watch`. Where: docs/DEPLOYMENT.md step 3.
F3  Briefs: DEPLOYMENT says ~/.agentos-output/brief-*.md but Docker cos writes /data/output/
    inside the container (no host mount). FIX (prefer a): bind-mount ${HOME}/.agentos-output:
    /data/output on cos so briefs land on the host; or correct the doc to read via exec.
    Where: docker-compose.yml (cos volumes) + docs/DEPLOYMENT.md.

Also: add a Docker CoS smoke (docker compose up cos → healthz + one networked tool call with a
stub cred) so this path stops rotting silently — it had never been run end-to-end.

NON-NEGOTIABLE: for every code item — fix + a test that FAILS without it + adversarial
verification, not "applied." No partial doc updates that leave a stale claim.

DONE = F4: a Net{ports} MCP server works on a no-V4 kernel (best-effort-allow + warn, or the
strict-flag path) with a test, and the Mac Docker CoS reaches Gmail. F1–F3: DEPLOYMENT Path 1
is accurate and briefs land where documented. /review + /qa clean.
```
