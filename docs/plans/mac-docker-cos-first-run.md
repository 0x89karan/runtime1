# Mac + Docker CoS first-run fixes (dogfood findings, 2026-07-09)

**Source:** first real end-to-end run of the Chief of Staff on Docker Desktop (Mac, arm64).
The container starts and the cron loop fires, but the flagship Gmail path **does not work**,
and three DX papercuts block a clean first run. Findings F1–F4 below.

**The headline:** F4 means "run the CoS on your Mac via Docker" — the primary dev path — is
**broken for anything networked** (Gmail, web fetch, search). F1–F3 are DX/doc fixes.

Two groups; the build session can ship as one or two increments (F4 is core + needs eng-review):
- **Group A — Docker first-run DX (F1, F2, F3):** docs + compose. Small, no eng-review.
- **Group B — sandbox Net-fallback (F4):** core sandbox behavior + a security decision. The actual blocker.

Meta-rule (as always): for every code item — fix + a test that fails without it + adversarial
verification of the real failure path, not "applied." No partial doc updates that leave a stale claim.

---

## Group B — F4 (headline, BLOCKING): `Net{ports}` falls back to deny-all on kernels without Landlock ABI V4

**Symptom:** the CoS inbox agent reports "Not authenticated" and tries the (dead) in-container OAuth
flow, even with a valid `~/.agentos-secrets/google.json`. It hangs.

**Root cause (verified from logs + code):** `google_oauth` declares `Net{ hosts=[…], ports=[443] }`
(`agentd/cos.agents.toml:17-23`). On Docker Desktop's kernel there is **no Landlock ABI V4** (needs
Linux ≥ 6.7), so `caps_to_rules_inner` (`agentd/src/main.rs:1344-1349`) falls back to
`SandboxRule::IsolateNetwork` — **deny-all network** — for that sidecar. It gets an empty netns and
cannot reach `oauth2.googleapis.com`, so the token refresh fails → "not authenticated" → dead flow.
Log: `WARN agentd: Net{ports} declared but Landlock ABI V4 is unavailable … falling back to
IsolateNetwork (deny-all)`.

**Impact:** breaks **every sandboxed MCP server that needs network** — `google_oauth`, `http_fetch`,
`web_search` — on any pre-6.7-Landlock kernel, which includes **Docker Desktop on Mac** (the primary
dev setup). The flagship's Gmail integration is unusable there. This is the flip side of the whole-
system audit's S6 (sandbox degradation): there it was fail-open; here it fails **closed in a way that
breaks the declared-needed access**. Same root: inconsistent degradation policy.

**Fix (DECISION for `/plan-eng-review`):** do **not** deny-all a server that declared it needs network.
When `Net{ports}` is declared but Landlock V4 is unavailable, degrade **best-effort-allow + a loud
warn** ("per-port network enforcement unavailable on this kernel; allowing network unenforced") —
consistent with how Landlock *filesystem* rules already degrade best-effort. Alternatively, gate the
deny-all behind an explicit `[tools] strict_network_isolation = true` (default off). Pick one via
eng-review; the default must keep networked tools working on common kernels.

**Acceptance:** on a kernel without Landlock V4 (Docker Desktop), a `Net{ports}` MCP server has working
outbound network to its declared hosts; the CoS `google_oauth` sidecar refreshes and reads Gmail; a
warn is logged. Test: `caps_to_rules_inner(&[Net{…}], v4_available=false)` yields an allow/no-net-isolation
rule set (or the strict-flag path), **not** `IsolateNetwork`.

**Where:** `agentd/src/main.rs:1314-1349` (`caps_to_rules` / `caps_to_rules_inner`), `sandbox/src/lib.rs`.

---

## Group A — Docker first-run DX

### F1 — DEPLOYMENT.md doesn't say where to put `ANTHROPIC_API_KEY`; shell export is fragile
**Symptom:** cos exits immediately with `ERROR: ANTHROPIC_API_KEY is not set` when the operator
`export`ed the key in a different shell than `docker compose up` (compose only passes through the
env of the invoking shell).
**Fix:** DEPLOYMENT.md Path 1 must instruct putting the key in `~/.agentos-secrets/agentos.env`
(shell-independent; cos mounts it and the entrypoint sources it) — not rely on a shell export. The
entrypoint's error already recommends this; the *docs* don't.
**Acceptance:** following Path 1 verbatim, the key reaches cos regardless of which terminal runs compose.
**Where:** `docs/DEPLOYMENT.md` Path 1.

### F2 — `agentctl watch` from the host can't reach the Docker CoS
**Symptom:** `agentctl watch` on the Mac → "Management API at 127.0.0.1:7999 is unreachable."
**Root cause:** the `cos` service publishes no port, and the management API is loopback-bound *inside*
the container (a deliberate single-tenant lock). So host-side watch cannot connect.
**Fix:** DEPLOYMENT.md Path 1 must use `docker compose exec cos agentctl watch` (watch from inside the
container). Publishing `:7999` won't help without binding `0.0.0.0` (which fights the loopback guard),
so `exec` is the correct answer for the Docker path.
**Acceptance:** Path 1's monitoring step works as written on Docker.
**Where:** `docs/DEPLOYMENT.md` Path 1 step 3.

### F3 — brief output path mismatch
**Symptom:** DEPLOYMENT.md says briefs land in `~/.agentos-output/brief-*.md` on the host, but the
Docker cos writes to `/data/output/` **inside** the container (no host bind mount) — so nothing appears
on the host.
**Fix (prefer a):** (a) bind-mount `${HOME}/.agentos-output:/data/output` on the `cos` service so
briefs land on the host as the docs claim; or (b) correct DEPLOYMENT.md to read them via
`docker compose exec cos cat /data/output/…`.
**Acceptance:** briefs appear where DEPLOYMENT.md says they do.
**Where:** `docker-compose.yml` (cos volumes) + `docs/DEPLOYMENT.md`.

---

## Cross-cutting note

All four surfaced on the *first* real end-to-end Mac Docker run — the path had never been exercised
end-to-end. Consider a **Docker CoS smoke** (or a documented manual first-run gate) so this path
doesn't silently rot: `docker compose up cos` → assert healthz + one networked tool call succeeds with
a stub/dry-run credential.

## Done =

F4: a `Net{ports}` MCP server works on a no-Landlock-V4 kernel (best-effort-allow + warn, or the
strict-flag path), with a test; the Mac Docker CoS reaches Gmail. F1–F3: DEPLOYMENT.md Path 1 is
accurate (key → `agentos.env`, watch via `exec`, briefs where documented) and briefs land on the host.
Every code item has a failing-without-it test + adversarial verification. `/review` + `/qa` clean.
