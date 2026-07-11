# dx.6 — Fast local dev-image loop + on-demand multi-arch publish

**Version:** v0.74.0  
**Branch:** feat/dx.6-dev-image-loop  
**Depends on:** dx.3, dx.4, ma.3 (all complete)

## Problem

`publish-docker` runs on every push to `main` and QEMU-builds arm64 (~60–90 min on
first push, ~15 min cached). Every increment in the current build queue must wait for
a completed CI run before a pullable `ghcr.io/0x89karan/runtime1:full` image exists.
This serializes the build loop to CI speed, not developer speed.

## Scope

### (a) `make dev-image` — native single-arch local build

Add two targets to the root `Makefile`:

```make
# Build the full image (Python MCP harness + Rust binaries) — for CoS dogfood
.PHONY: dev-image
dev-image:
	DOCKER_BUILDKIT=1 docker build --target runtime-full -t agentos:dev .

# Build the core image (Rust binaries only) — faster, for agentd/agentctl-only changes
.PHONY: dev-image-core
dev-image-core:
	DOCKER_BUILDKIT=1 docker build --target runtime-core -t agentos:dev-core .
```

Add a BuildKit cache mount to the `cargo build` steps in the `Dockerfile` builder stage
so incremental rebuilds are fast when only Rust source changes:

```dockerfile
# Dep pre-build: cache the crate downloads (sharing=locked: safe for concurrent BuildKit builds)
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo build --release 2>/dev/null || true

# Real build: cache the crate downloads only; /src/target stays in the image layer
# NOTE: do NOT cache /src/target — cache mounts are not committed to image layers,
# which would break the COPY --from=builder step in stage 2.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    touch agentd/src/main.rs agentd/src/lib.rs agentctl/src/main.rs \
          surfaces/src/lib.rs sandbox/src/lib.rs otel/src/main.rs \
 && cargo build --release --bin agentd --bin agentctl --bin agentos-otel
```

The `/usr/local/cargo/registry` cache persists crate downloads across builds (the slow
part of a first Rust build). Source-change incremental builds are already handled by the
existing stub-build pattern: deps are compiled in the first `RUN`, real source is compiled
in the second `RUN`, and only changed crates recompile.

**AGENTOS_IMAGE env var + compose override:**  
Add `AGENTOS_IMAGE` support to `docker-compose.yml` so `cos` can run against the local
image instead of rebuilding from source every time. Replace the bare `build: .` with:

```yaml
services:
  cos:
    image: ${AGENTOS_IMAGE:-agentos:dev}
    build:
      context: .
      target: runtime-full
  agent:
    image: ${AGENTOS_IMAGE:-agentos:dev}
    build:
      context: .
      target: runtime-full
```

When `AGENTOS_IMAGE` is unset: `image: agentos:dev` — matches the name `make dev-image`
produces, so `make dev-image && docker compose up cos` works with no env var. If the image
doesn't exist locally, Compose builds from source and tags it `agentos:dev`.

When `AGENTOS_IMAGE=ghcr.io/0x89karan/runtime1:full docker compose up cos`: Compose
pulls and uses the published image.

NOTE: do NOT use `image: ${AGENTOS_IMAGE:-}` (empty-string default) — Docker Compose
behavior with an empty `image:` is undefined and likely errors. The `agentos:dev` fallback
is a valid image name that Compose will build-and-tag on first run.

Also add `.env.example` at the repo root:
```
# Copy to .env and Docker Compose will pick it up automatically.
# Uncomment to use a pre-built local image instead of building from source.
# AGENTOS_IMAGE=agentos:dev
```

### (b) Gate `publish-docker` CI trigger to `workflow_dispatch` + `v*` tag

Two changes to `.github/workflows/ci.yml`:

**1. Add `workflow_dispatch` and `push.tags` to the top-level `on:` trigger:**

```yaml
on:
  push:
    branches: ["**"]
    tags: ["v*"]          # ← NEW: tag pushes reach all jobs
  pull_request:
    branches: ["main"]
  workflow_dispatch:       # ← NEW: manual dispatch from Actions UI
```

Without `push.tags` in the `on:` block, a `v*` tag push never even reaches the workflow.
The job-level `if` alone is insufficient.

**2. Update `build-aarch64` job condition to also run on tags and workflow_dispatch:**

```yaml
build-aarch64:
  if: |
    github.ref == 'refs/heads/main' ||
    github.event_name == 'pull_request' ||
    startsWith(github.ref, 'refs/tags/v') ||
    github.event_name == 'workflow_dispatch'
```

Why: `publish-docker.needs` includes `build-aarch64`. A needed job that is *skipped*
causes the dependent to also skip. If `build-aarch64` skips on a tag push, `publish-docker`
never runs. Running `build-aarch64` on release triggers also ensures aarch64 validation
before publishing, which is desirable.

**3. Change the `publish-docker` job condition to:**

```yaml
if: |
  (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main') ||
  (github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v'))
```

`workflow_dispatch` is constrained to `main` to prevent publishing from unreviewed branches.

All other jobs (`build-and-test`, `build-macos`, `audit`, `distro-aarch64`) keep their
existing conditions.

### (c) DEPLOYMENT.md — local dev quickstart + release instructions

Add a "## Dev image (local fast loop)" section before "Path 1 — Mac + Docker":

- `make dev-image` builds native arm64 (on Apple Silicon) in ~2 min cached
- `AGENTOS_IMAGE=agentos:dev docker compose up cos` runs with the local image
- State the tradeoff: `:latest` no longer auto-updates per merge; publish once per
  batch of increments

Add a "## Cutting a release image" section:

- Dispatch `publish-docker` from Actions UI, **or**
- Push a `v*` tag: `git tag v0.74.0 && git push origin v0.74.0`

### (d) TODOS.md — defer native ARM64 CI runners

Add a P4 item: "Native ARM64 CI runners — private repo → paid runners needed; revisit
if we go public or want fast releases."

Close the implicit requirement that every increment produces a pullable image on merge.

## Acceptance criteria

1. `make dev-image` completes in ≤ 2 min on second run (cargo registry cache hit) with no CI.
   First build: ≤ 15 min on Apple Silicon M-series (full Rust compile from scratch).
2. `AGENTOS_IMAGE=agentos:dev docker compose up cos` starts the CoS using the local image.
3. `docker compose up cos` (AGENTOS_IMAGE unset) still works: builds and tags as `agentos:cos-local`.
4. A push to `main` does NOT trigger `publish-docker` in CI.
5. `publish-docker` still runs and produces the full `linux/amd64`+`linux/arm64` manifest
   when dispatched via `workflow_dispatch` or a `v*` tag push.
6. DEPLOYMENT.md documents both paths clearly, with a note that `:latest` is release-tagged only.
7. All existing CI jobs (build-and-test, build-macos, build-aarch64, audit) continue to run
   on every push.
8. `make dev-image-core` builds the `agentos:dev-core` (Rust-only) image for faster inner-loop testing.

## Files touched

- `Dockerfile` — BuildKit cargo registry cache mounts on both `cargo build` steps
- `Makefile` — `dev-image` + `dev-image-core` targets
- `docker-compose.yml` — `AGENTOS_IMAGE` override with `agentos:cos-local` fallback + explicit `target: runtime-full`
- `.github/workflows/ci.yml` — add `push.tags: ["v*"]` + `workflow_dispatch` to top-level `on:`; update `publish-docker` job condition
- `docs/DEPLOYMENT.md` — dev loop quickstart + "cutting a release image" section + `:latest` tradeoff note
- `TODOS.md` — native ARM64 runner deferral (P4) + cross-compile investigation (P3)
- `docs/ROADMAP.md` — mark dx.6 complete
- `CHANGELOG.md` — release entry

## Non-goals / deferred

- Incremental musl static cross builds (the root CI binary is already cached by Swatinem)
- Per-commit automatic `:latest` publish (the whole point of this increment is to turn this off)
- Native ARM64 CI runners (deferred to TODOS.md — paid runners on private repo)
- Cross-compiled arm64 Docker builds (deferred to TODOS.md — investigate as potential root-cause fix)
- Cosign image signing (deferred)
- P2 security debt items (audit-C2, audit-S6, audit-S7) — addressed by the main build queue, not dx.6

---

## GSTACK REVIEW REPORT

### Phase 1 — CEO Review

**Dream state delta:**
- CURRENT: Every `main` push triggers 60-90 min QEMU arm64 CI; `ghcr.io/:latest` always fresh but blocks developer velocity
- AFTER DX.6: `make dev-image` yields runnable arm64 image in ≤2 min (2nd run); `:latest` is release-tagged only; CoS inner loop unblocked
- 12-MONTH IDEAL: Native ARM64 CI runners eliminate QEMU entirely; cross-compiled arm64 Docker via `cross` for fast releases

**What already exists:**
- `cross` toolchain + `Cross.toml` (already used in `build-aarch64` CI job) — foundation for future cross-compiled Docker
- Dockerfile stub-build pattern (already handles source-change incremental; cache mount adds registry-fetch speedup)
- `Swatinem/rust-cache@v2` (musl binary CI caching already handled)

**Error & Rescue Registry:**

| Error | Trigger | Impact | Rescue |
|-------|---------|--------|--------|
| `/src/target` cache mount breaks COPY --from=builder | Implemented naively | Build produces empty binaries; silent failure | Cache only `/usr/local/cargo/registry`; fixed in plan |
| `image: ""` in Compose | AGENTOS_IMAGE unset + empty default | `docker compose up cos` errors | Use `agentos:cos-local` as fallback; fixed in plan |
| Tag trigger missing from `on:` | v* tag push | Workflow never fires on tag | Add `push.tags: ["v*"]` to on: block; fixed in plan |
| `:latest` stale | No manual dispatch between releases | Users pull old image | Document tradeoff; add version/digest to DEPLOYMENT.md note |
| dev-image vs release diverge | amd64-only local testing | Green local, red amd64 CI | Note in DEPLOYMENT.md; run `make dev-image` on apple silicon only |

**Failure Modes Registry:**

| Mode | Probability | Severity | Mitigation |
|------|-------------|----------|------------|
| Developer forgets to dispatch publish after batch | Medium | High (stale :latest) | Document release cadence; add TODOS reminder |
| cargo registry cache corruption | Low | Medium (slow build) | `docker builder prune` clears cache |
| AGENTOS_IMAGE set to wrong image | Low | Low (obvious error on startup) | Compose errors early with image-not-found |
| CI `build-aarch64` fails after trigger change | Low | Medium (blocks PR) | `build-aarch64` condition unchanged; not affected |

**NOT in scope (deferred to TODOS.md):**
- Cross-compiled arm64 Docker build (P3 investigation: `cross` already in toolchain)
- Native ARM64 CI runners (P4: paid/private repo)
- Automatic publish with Docker-path filtering (P3: alternative if stale :latest hurts)
- P2 security audit items

### Phase 3 — Eng Review

**Architecture ASCII diagram:** (see plan scope section above)

**Scope challenge:** 8 files changed. All in blast radius of the CI/Docker build system.
No new Rust code. No changes to `agentd/` or `agentctl/` source. Risk is low for runtime
regressions; risk is higher for CI correctness (workflow conditions, needs dependencies).

**Test diagram:**

| UX/Data Flow | Test Type | Exists? | Gap? |
|---|---|---|---|
| `make dev-image` produces runnable arm64 image | Manual smoke: `docker run --rm agentos:dev agentd --help` | No | Add to AC |
| `make dev-image` 2nd run reuses cache | Manual: check Docker build output for cache hit | No | Manual |
| `make dev-image-core` produces runtime-core | Manual smoke: same as above | No | Add |
| `docker compose up cos` (AGENTOS_IMAGE unset, image absent) | Manual: check Compose builds + starts | No | Manual |
| `AGENTOS_IMAGE=agentos:dev docker compose up cos` | Manual: check Compose uses pre-built | No | Manual |
| `AGENTOS_IMAGE=missing` → should error or warn | Manual: document behavior | No | Document |
| Tag push triggers publish-docker, not on main push | CI: `git push origin vX.Y.Z` | No | Test in PR |
| workflow_dispatch triggers publish from main only | CI: manual dispatch | No | Test post-merge |
| `build-aarch64` runs on tag/dispatch | CI: verify | No | Verify in PR |
| Version alignment: git tag matches Cargo version | Pre-push check (add note to DEPLOYMENT.md) | No | Document |

**Phase 3 Decision Audit Trail additions:**

| # | Phase | Decision | Class | Principle | Rationale | Rejected |
|---|-------|----------|-------|-----------|-----------|---------|
| D9 | Eng | build-aarch64 must also run on tags/dispatch (skipped needed = publish skips) | Mechanical | P5 explicit | GitHub Actions: skipped needed job → dependent skips | Leave condition unchanged |
| D10 | Eng | Add sharing=locked to cargo registry cache mount | Mechanical | P5 | Safe for concurrent multi-platform BuildKit builds | default sharing |
| D11 | Eng | Constrain workflow_dispatch to github.ref == refs/heads/main | Mechanical | P5 security | Prevents arbitrary-branch publishing from write-access collaborators | Any-ref dispatch |
| D12 | Eng | Document AGENTOS_IMAGE=missing behavior (silent build) rather than adding preflight guard | Taste | P3 pragmatic | Preflight would require removing build: entirely; complexity not justified for solo dev | Add preflight |
| D13 | Eng | Add version alignment note to DEPLOYMENT.md (tag must match Cargo version) | Mechanical | P1 | Publish job derives tags from Cargo metadata; git tag mismatch produces wrong Docker tags | Workflow validation |

### Phase 1 Decision Audit Trail

| # | Phase | Decision | Class | Principle | Rationale | Rejected |
|---|-------|----------|-------|-----------|-----------|---------|
| D1 | CEO | Cache only /usr/local/cargo/registry, NOT /src/target | Mechanical | P5 explicit | Cache mounts not committed to image layers; /src/target cache breaks multi-stage COPY | /src/target cache |
| D2 | CEO | Add push.tags to on: trigger | Mechanical | P5 | Job-level if insufficient without top-level trigger | Job-if-only |
| D3 | CEO | Use agentos:cos-local fallback in Compose | Mechanical | P5 | Empty image: causes undefined behavior | image: ${:-} |
| D4 | CEO | Proceed with dx.6 direction (cross-compile deferred) | Mechanical | P6 bias-to-action | User confirmed premises; cross-compile is P3 investigation | Scope pivot |
| D5 | CEO | Add dev-image-core target (h8.2 core tier) | Mechanical | P1 completeness | h8.2 shipped core/full split; inner-loop Rust dev only needs core | Full-only |
| D6 | CEO | Accept :latest staleness tradeoff; document it | Taste | P3 pragmatic | Solo dev project, not public product; manual release discipline acceptable | Auto-publish filter |
| D7 | CEO | P2 security debt: not in dx.6 scope | Mechanical | P4 DRY | Build queue in prompt 12 defines order; dx.6 is first | Security scope |
| D8 | CEO | Add first-build time estimate to AC | Mechanical | P1 completeness | Onboarding experience needs a known upper bound | Omit |
