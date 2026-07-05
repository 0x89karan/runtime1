<!-- /autoplan restore point: cred.2-unified-secrets-substrate -->
# cred.2 — Unified secrets substrate

**Status:** Implemented — v0.59.0
**Depends on:** cred.1 (v0.58.0 — secrets volume already mounted on agent service)
**Design doc:** `docs/plans/credential-manager.md` § cred.2
**autoplan voices:** Codex CEO (adversarial), Codex Eng, Codex DX (4-voice pipeline, 2026-07-05)

---

## Goal

One credentials story across all three surfaces (Docker `agent`, Docker `cos`, QEMU) with
no manual env-var exports. `ANTHROPIC_API_KEY` flows from `~/.agentos-secrets/agentos.env`
on every surface. Google OAuth credentials flow from `~/.agentos-secrets/google.json`.

---

## Changes

### 1. `docker/entrypoint.sh` — source agentos.env before any key check

Added at the top (after helper function definitions, before the mode `case`):

```sh
if [ -f /run/secrets/agentos.env ]; then
  set -a
  . /run/secrets/agentos.env
  set +a
fi
```

**Sourcing order matters:** the file is sourced before `check_api_key` so the key is
available regardless of whether compose also passes `ANTHROPIC_API_KEY`. **Precedence:
file wins** — if the same key is set in both the compose environment block and `agentos.env`,
the file value overrides. This is intentional: the secrets file is the authoritative source.

**Safety:** `set -a; . file; set +a` is shell code execution. The file must use `export VAR=value`
syntax (not `dotenv` format). Under `set -e`, a failing command inside the sourced file will
terminate the entrypoint. File must be owned by the user and mode 0600 (enforced by
`agentctl auth google` and documented as convention).

**Updated `check_api_key` error** now shows both paths (secrets file + `-e` env var).

**Removed `OAUTH_CALLBACK_PORT` warning block** (was lines 169-174) — dead code after compose
strip; compose no longer passes `OAUTH_CALLBACK_PORT` into the container.

### 2. `docker-compose.yml` — strip OAUTH_* from agent block

Removed from `agent` service `environment`:
- `OAUTH_CLIENT_ID`
- `OAUTH_CLIENT_SECRET`
- `OAUTH_REFRESH_TOKEN`
- `OAUTH_CALLBACK_PORT`

These vars are no longer injected by `docker compose run`. Updated comments to explain
the `agentctl auth google` + secrets file path.

**Backwards compat:** the entrypoint's `google-agent` preflight still accepts a complete
set of `OAUTH_CLIENT_ID + OAUTH_CLIENT_SECRET + OAUTH_REFRESH_TOKEN` passed via
`docker run -e` (not compose). The compose strip is the breaking change; direct `docker run`
with all three vars still works.

### 3. `distro/Makefile` — QEMU secrets mount read-only

```makefile
# Before:
-virtfs local,path=$(HOME)/.agentos-secrets,mount_tag=secrets0,security_model=none,id=secrets0 \

# After:
-virtfs local,path=$(HOME)/.agentos-secrets,mount_tag=secrets0,security_model=none,readonly=on,id=secrets0 \
```

**Syntax note:** QEMU requires `readonly=on` (not bare `,readonly`). The 9p guest mount
in `distro/overlay/init` is unaffected — it mounts with `trans=virtio` options and inherits
the host-export read-only flag automatically.

### 4. `docs/RUNBOOK.md` — PARTIALLY STALE banner + §11 credentials rewrite

Added banner at top noting the document is partially stale (last fully updated v0.20.0).

§11.3–11.5 rewritten:
- **Removed:** `OAUTH_CLIENT_ID/SECRET/REFRESH_TOKEN/AUTH_URL/TOKEN_URL/SCOPES/ALLOWED_HOSTS/PROVIDER_NAME` env export instructions
- **Removed:** `~/.agentos-oauth/google.json` credential store path (does not exist)
- **Removed:** `cargo run -- cos.agents.toml` launch instructions
- **Added:** `agentctl auth google` + `~/.agentos-secrets/agentos.env` + `docker compose` workflow

§11.8 monitoring: replaced `cargo run --bin agentctl` with `docker compose exec cos agentctl watch`.

§11.10 troubleshooting: updated to reflect new error messages and credential paths.

---

## Decisions locked by autoplan

| Decision | Choice | Rationale |
|----------|--------|-----------|
| OAUTH_* in compose | Hard break — remove; CHANGELOG migration note only | Soft deprecation notice never fires (compose strips vars before entrypoint) |
| THREAT_MODEL.md | Leave untouched — file sec.1 TODO | Full update too large; 4 missing surfaces documented in sec.1 TODO |
| RUNBOOK.md scope | §11 rewrite + PARTIALLY STALE banner only | Full rewrite increases review surface; other sections not touched in this increment |
| Template passenv cleanup | Deferred to cred.3 | `google-agent.template.toml` OAUTH_* passenv is inert when compose doesn't pass them |
| Entrypoint env-var fallback | Keep for `docker run -e` compat | Breaking compose is enough for cred.2; `docker run` users still work |
| QEMU readonly syntax | `readonly=on` (not bare `readonly`) | Confirmed from QEMU virtfs help output |
| Sourcing precedence | File wins | Agentos.env is the authoritative secrets source; compose env is secondary |
| google.json schema-drift guard | Python fixture test only | No Rust changes in cred.2; full cross-language guard deferred to cred.3 |

---

## Deferred to cred.3

- `templates/google-agent.template.toml` OAUTH_* passenv cleanup
- `agentd/cos.agents.toml` passenv stale comments
- THREAT_MODEL.md update → sec.1 TODO in TODOS.md
- google.json Rust schema-drift test

---

## Acceptance criteria

- Docker `shell`, `run`, `demo`, `cos`, `agent` modes: `ANTHROPIC_API_KEY` from
  `~/.agentos-secrets/agentos.env` works without `-e ANTHROPIC_API_KEY=...`.
- `TEMPLATE_NAME=google-agent` works with `~/.agentos-secrets/google.json` present.
- `TEMPLATE_NAME=scout` (no OAuth) is unaffected.
- `docker compose run --rm agent` with `TEMPLATE_NAME=google-agent` and no OAUTH_* in shell:
  google-agent preflight catches missing credentials and prints actionable error.
- QEMU `make run`: secrets mount is exported read-only from host.
- RUNBOOK §11 instructions are accurate for the current Docker-based workflow.
- `cargo test` still passes (no Rust changes, count unchanged from v0.58.0).
