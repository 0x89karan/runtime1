# ma.3 — multi-arch container images

**Status:** complete (v0.56.0 shipped 2026-07-04)
**Version target:** v0.56.0
**Depends on:** ma.1 ✅ (v0.55.0)
**Branch:** `ma.3-multi-arch-images`

---

## Goal

`docker pull ghcr.io/0x89karan/runtime1:latest` runs natively on both x86_64 and ARM64 hosts
with no emulation warning. The existing x86_64 Docker build is unchanged for local users.

---

## Current state

- `Dockerfile`: single-arch, builds for native host arch via `FROM rust:1-alpine AS builder` + `cargo build --release`
- No multi-arch manifest published anywhere
- `docker-compose.yml`: uses `build: .` — native-arch only, no `platform:` key
- No CI job publishes a Docker image

The Dockerfile is already arch-agnostic: `rust:1-alpine`, `alpine:3.20`, `apk add fuse-dev musl-dev`
all have arm64 packages. Under `docker buildx --platform linux/arm64`, `cargo build --release`
compiles natively inside a QEMU-emulated aarch64 container.

---

## Scope

### Files to create

_(none — existing Dockerfile is correct as-is)_

### Files to modify

1. **`.github/workflows/ci.yml`** — add `publish-docker` job:
   - Trigger: `push` to `main` only (not PRs — avoids registry rate limits + branch noise)
   - Uses `docker/setup-qemu-action@v3` for QEMU emulation
   - Uses `docker/setup-buildx-action@v3`
   - Logs into `ghcr.io` via `docker/login-action@v3` with `GITHUB_TOKEN`
   - `docker buildx build --platform linux/amd64,linux/arm64 --push`
   - Tags: `ghcr.io/0x89karan/runtime1:latest` + `ghcr.io/0x89karan/runtime1:v{version}`
   - No image publish on PRs — only on main merge

2. **`CHANGELOG.md`** — v0.56.0 entry

3. **`docs/ROADMAP.md`** — mark ma.3 complete

---

## Key decisions

### D1: Build method — QEMU emulation vs cross-compilation
**Chosen: QEMU emulation (native build inside container)**

Under `docker buildx --platform linux/arm64`, `cargo build --release` runs inside a QEMU-emulated
aarch64 container. Output is a native arm64 binary. No Dockerfile changes required.

Alternative (cross-compilation): Set `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER` inside the
builder and compile from x86_64 to arm64. Faster, but requires toolchain setup in Dockerfile
(musl cross-linker), which adds complexity. QEMU is the standard `docker buildx` approach.

**Risk**: QEMU emulation slows Rust compilation ~10x. For this codebase (~45s on native x86_64),
arm64 under QEMU would take ~7-10 min. GitHub Actions job limit is 60 min, so it fits — but only
barely for very large future codebases.

### D2: Registry
**Chosen: ghcr.io (GitHub Container Registry)**

Specified by DEPLOYMENT-TOPOLOGY.md. Free for public repos. Authenticated via built-in `GITHUB_TOKEN`
(no external secret setup). Compatible with `docker-compose.yml` image: overrides.

### D3: Image tags
**Chosen: `:latest` + `:v{semver}` on every main push**

`:latest` is the ergonomic target for `docker pull`. Semver tag (e.g., `:v0.56.0`) is pinnable.
No `aarch64`/`x86_64` suffix tags — the multi-arch manifest handles arch selection transparently.

### D4: Trigger policy
**Chosen: publish only on push to `main`**

No publish on PRs (avoids registry noise + rate limits). CI on PRs still builds and tests.
The `publish-docker` job adds a `needs: build-and-test` gate so broken code can never be published.

### D5: Image variant — `core` only vs `core` + `full`
**Chosen: publish `:latest` (= `:core`) only for now**

The `:full` variant (includes all MCP Python servers) is defined in h8.2, not ma.3. Shipping
just `:core` keeps ma.3 self-contained. Tag alias: `:latest` = `:core` = agentd + agentctl + agentd-otel.

### D6: Dockerfile arch-specific changes
**None needed.** `rust:1-alpine`, `alpine:3.20`, and `apk add fuse-dev musl-dev` all support arm64.
`cargo build --release` produces the correct arch binary natively under QEMU emulation.

---

## Files overview

```
.github/workflows/ci.yml    MODIFY (add publish-docker job)
CHANGELOG.md                MODIFY (v0.56.0)
docs/ROADMAP.md             MODIFY (mark ma.3 done)
```

No Dockerfile changes. No docker-compose.yml changes. No Rust code changes.

---

## Acceptance criteria

1. `docker run --rm ghcr.io/0x89karan/runtime1:latest --version` prints the version on an x86_64 host.
2. Same command on an ARM64 host (Apple Silicon Mac or ARM cloud instance) prints the version without `WARNING: The requested image's platform (linux/amd64) does not match the detected host platform (linux/arm64)`.
3. `docker buildx imagetools inspect ghcr.io/0x89karan/runtime1:latest` shows manifests for both `linux/amd64` and `linux/arm64`.
4. The `publish-docker` CI job runs only on push to `main` (not on PRs).
5. The `publish-docker` job depends on `build-and-test` passing (never publishes a broken image).
6. 1,096+ workspace tests still pass (no Rust code changes).

---

## NOT in scope

- `:full` image variant (h8.2)
- ma.2 QEMU/HVF distro boot (deferred, separate increment)
- Windows container images
- `docker-compose.yml` image: override (pulls instead of builds) — operator DX, not ma.3
- Cross-compilation in Dockerfile (QEMU emulation is sufficient for now)

---

## What already exists

| Sub-problem | Existing code |
|---|---|
| aarch64 Rust binary | ma.1: cross-compiled, CI-tested |
| Dockerfile | Already arch-agnostic (Alpine + native cargo build) |
| docker-compose.yml | Uses `build: .`; no registry ref yet |
| ghcr.io registry | Available via GITHUB_TOKEN in Actions |
| CI jobs | build-and-test, build-aarch64, audit |

---

## Implementation steps

1. Add `publish-docker` job to `.github/workflows/ci.yml`:

```yaml
  publish-docker:
    needs: [build-and-test, build-aarch64]
    runs-on: ubuntu-latest
    timeout-minutes: 60
    if: github.ref == 'refs/heads/main' && github.event_name == 'push'
    permissions:
      packages: write
    steps:
      - uses: actions/checkout@v4

      - name: Get version
        id: version
        run: |
          echo "version=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "agentd") | .version')" >> "$GITHUB_OUTPUT"
        working-directory: agentd

      - name: Set up QEMU
        uses: docker/setup-qemu-action@v3

      - name: Set up Docker Buildx
        uses: docker/setup-buildx-action@v3

      - name: Log in to ghcr.io
        uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.repository_owner }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - name: Build and push multi-arch image
        uses: docker/build-push-action@v6
        with:
          context: .
          platforms: linux/amd64,linux/arm64
          push: true
          provenance: false
          tags: |
            ghcr.io/0x89karan/runtime1:latest
            ghcr.io/0x89karan/runtime1:v${{ steps.version.outputs.version }}
          cache-from: type=gha
          cache-to: type=gha,mode=max
```

2. **One-time post-first-push step** (manual, after merging): Navigate to
   GitHub repo → Packages → agentos → Package Settings → "Change visibility" → Public.
   Without this, `docker pull ghcr.io/0x89karan/runtime1:latest` fails with 401 for unauthenticated users.
   This can also be done via GitHub API: `gh api -X PATCH /user/packages/container/agentos/versions/{id}/restore`
   — but the simplest path is the web UI.

3. Bump version → v0.56.0, update CHANGELOG + ROADMAP.

Notes:
- `--provenance=false` closes the door on future OCI SBOM attestations — note this before adding SBOM tooling later
- QEMU arm64 build: ~8-12 min with GHA cache hit, ~20-30 min cold; both within 60-min timeout
- `needs: [build-and-test, build-aarch64]` ensures both arch Rust CIs must pass before any image is published

Total estimated CC time: ~15 minutes. No Rust code changes.

---

## Test plan

- Merge to main → `publish-docker` CI job runs → check ghcr.io package page
- `docker buildx imagetools inspect ghcr.io/0x89karan/runtime1:latest` on any machine
- `docker run --rm --platform linux/arm64 ghcr.io/0x89karan/runtime1:latest --version` on x86_64 host (forces arm64 pull, verifies no panic)
- PR CI does NOT trigger publish-docker (verify job doesn't appear)

---

---

## CEO Review — Phase 1 Outputs

### Dream state delta

```
TODAY:         Docker image is x86_64-only; ARM users see "WARNING: The requested image's
               platform does not match the detected host platform" or run under Rosetta
THIS PLAN:     Multi-arch manifest on ghcr.io; native arm64 image for Apple Silicon + ARM cloud
12-MONTH IDEAL: `:full` image variant (h8.2), `docker-compose.yml` references ghcr image by default
GAP after ma.3: docker-compose.yml still `build: .` (local build); `:full` image absent;
               QEMU build time may grow as codebase scales
```

### Error & Rescue Registry

| Error | Trigger | Severity | Rescue |
|---|---|---|---|
| `Error: denied: permission_denied` on ghcr push | `permissions: packages: write` missing | High | Add `permissions: {packages: write}` to job |
| `no matching manifest for linux/arm64` on old Docker | Missing `--provenance=false` | High | Add `--provenance=false` to buildx command |
| arm64 image build takes 30+ min | No GHA layer cache | Medium | Add `cache-from/cache-to: type=gha,mode=max` |
| Image published despite aarch64 Rust CI failure | `needs: build-and-test` only | Medium | Add `needs: build-aarch64` to publish job |
| Semver tag is `:v` (empty) | Version extraction fails | Medium | Use `cargo metadata --no-deps --format-version 1 \| jq -r '.packages[] \| select(.name == "agentd") \| .version'` |
| `aarch64/musl-dev` not found in Alpine | Package doesn't exist for arm64 | Low | Verify with `docker run --platform linux/arm64 alpine:3.20 apk info musl-dev` |

### Failure Modes Registry

| Mode | Impact | Detection | Mitigation |
|---|---|---|---|
| QEMU build grows to 45+ min | Every main push blocked | CI timeout | Switch to native ARM runner or cross-compilation in Dockerfile |
| ghcr.io image diverges from `docker compose up` local build | Users get different behavior depending on pull vs build | No automated check | Document explicitly; ma.x can add `image: ghcr.io/…` to docker-compose.yml |
| `:latest` broken by bad merge | No pinnable known-good tag | No rollback | Semver tags `:v{version}` serve as stable pins |

### NOT in scope (deferred)

- `:full` image variant (h8.2)
- `docker-compose.yml` image: ghcr reference (operator DX, separate increment)
- Native ARM64 GitHub Actions runner (when build time becomes a problem)
- Cross-compilation in Dockerfile (QEMU is sufficient for now)

---

<!-- AUTONOMOUS DECISION LOG -->
## Decision Audit Trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| 1 | CEO | Add `permissions: packages: write` | Bug fix | — | Both voices: ghcr.io push fails without it | Rely on repo defaults |
| 2 | CEO | Add `needs: build-aarch64` to publish gate | Bug fix | — | Voice 1: aarch64 Rust CI must pass before publishing arm64 image | Gate on build-and-test only |
| 3 | CEO | Add GHA layer cache to buildx | Bug fix | P3 | Voice 2: without cache, QEMU Rust build = 20-30 min every push; with cache = 2-3 min on dep-unchanged | No cache (simpler but slow) |
| 4 | CEO | Add `--provenance=false` | Bug fix | — | Voice 2: Docker < 24.x fails on provenance attestations | Include provenance (breaks old clients) |
| 5 | CEO | Specify version extraction via `cargo metadata` | Doc fix | P5 | Both voices: vague "extract from Cargo.toml" is a silent failure mode | grep/awk (fragile) |
| 6 | CEO | Document docker-compose.yml inconsistency | Doc fix | P5 | Voice 2: users will notice divergence; silence is worse than honest docs | Silently defer |
| 7 | Eng | ghcr.io package visibility defaults private | Bug fix | — | First push creates private package; `docker pull` fails for all unauthenticated users. Add post-first-push step: set to Public via GitHub web UI or API | Assume public (breaks users) |
| 8 | Eng | Add `timeout-minutes: 60` to publish-docker | Bug fix | — | Docker QEMU arm64 build: 8-12 min cached, 20-30 min cold. Default 360 min is fine, but explicit timeout is best practice | No explicit timeout |
| 9 | Eng | Use `cargo metadata` for version extraction | Bug fix | — | Voice 1: precise and machine-readable; `grep` is fragile | grep/awk |
| 10 | Eng | Note --provenance=false SBOM trade-off | Doc fix | — | Closing the SBOM door; note it explicitly in plan | Treat as no-downside |
| 11 | Eng | Action SHA pinning at packages:write | Deferred | P3 | Personal project; major version tag consistent with existing CI pattern | SHA-pin now (low priority) |
| 12 | DX | No user-facing DX changes | Pass | — | No CLI/config changes; Docker publish is CI-only; docker-compose.yml unchanged | — |
