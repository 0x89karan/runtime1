# dx.4 — Pre-built images + device auth flow

**Version:** v0.71.0  
**Depends on:** dx.3 (v0.69.0 ✓), ma.2 (v0.57.0 ✓)  
**Goal:** Zero-build-step QEMU deployment; headless Linux server Google auth.

---

## Problem

**QEMU path today:** operator must `git clone`, run `make build` (30 min cold, downloads
Buildroot + compiles kernel + rootfs from source), then copy outputs to `/opt/agentos/`.
This is a meaningful barrier for anyone without a dev environment.

**Auth problem on headless servers:** `agentctl auth google` uses PKCE + local callback
server (port 8585). A headless Linux VPS has no browser — the user cannot complete the
redirect. Workarounds exist (scp secrets from Mac; SSH tunnel) but they're friction.

---

## Scope

### E1 — Prebuilt x86_64 distro images published on GitHub Releases

**What:** Extend `release.yml` with a `build-distro-x86_64` job that:
- Checks out the repo and restores the Buildroot cache (same key as `qemu-boot.yml`)
- Builds the current agentd release binary (musl static)
- Copies it into `distro/overlay/usr/bin/agentd`
- Runs `make build` in `distro/` (x86_64 only)
- Attaches `bzImage` + `rootfs.cpio.gz` to the GitHub Release as:
  - `agentos-<VERSION>-x86_64-bzImage`
  - `agentos-<VERSION>-x86_64-rootfs.cpio.gz`
- Adds their SHA256 digests to `SHA256SUMS`

**Why x86_64 only:** aarch64 Buildroot build requires `qemu-system-aarch64` on CI
and takes 30+ min with no cache. Deferred (see Deferred section). Operators on Apple
Silicon use the Docker path today.

**Cache strategy:** Reuse the `buildroot-2024.02.9-v2-*` cache key already used by
`qemu-boot.yml`. The cache covers the Buildroot toolchain + kernel; only rootfs
repacking (~2 min) runs on a cache hit. The cache is stored under a `v2` suffix so
future Buildroot upgrades can bump to `v3` without the stale key problem.

**Acceptance:** `release.yml` on the `v0.71.0` tag attaches 4 artifacts (agentd,
agentctl, bzImage, rootfs.cpio.gz) + SHA256SUMS to the GitHub Release.

### E2 — Google OAuth Device Authorization Flow (RFC 8628)

**What:** New `agentctl auth google --device` flag (or `agentctl auth google-device`
subcommand). Calls the Google device authorization endpoint, prints a URL + code, polls
until the user completes auth on another device, then writes the token to
`~/.agentos-secrets/google.json` (same format as existing PKCE flow).

**Endpoints:**
- Device authorization: `POST https://oauth2.googleapis.com/device/code`
- Token poll: `POST https://oauth2.googleapis.com/token` (grant_type=urn:ietf:params:oauth:grant-type:device_code)
- Same scope as existing flow: `gmail.readonly drive.readonly`

**Client credentials:** Same `OAUTH_CLIENT_ID` + `OAUTH_CLIENT_SECRET` env vars as
the PKCE flow. Google requires the client to be configured for "Desktop app" type —
device flow is supported on the same client type. The client secret is bundled with
the binary (it's not a credential in the traditional sense — Google's public client
model; see note below).

**Note on client secret:** For "Desktop app" clients, Google treats the client secret
as semi-public — it's not a per-deployment secret. This is the same model as `gcloud`,
`git-credential-oauth`, etc. We embed the client id in the binary if the operator
hasn't set OAUTH_CLIENT_ID. The secret is read from env only (not embedded).

**UX flow:**
```
$ agentctl auth google --device
Opening device auth flow (no browser required).

Visit: https://accounts.google.com/device
Code:  ABCD-EFGH

Waiting for authorization (expires in 30 min)...
✓ Authorized. Credentials written to ~/.agentos-secrets/google.json
```

**Poll interval:** Respect `interval` from device auth response (Google returns 5 s).
**Expiry:** Device code expires per `expires_in` (typically 1800 s). Show countdown.
**Error handling:** `authorization_pending` → continue polling. `slow_down` → add 5s to
current interval (RFC 8628 §3.5 — additive, not doubling). `expired_token` / `access_denied`
/ `invalid_grant` → bail with actionable message.
**Token storage:** Identical to PKCE flow — mode 0600 atomic write, same JSON schema.

**Acceptance:** `agentctl auth google --device` on a machine without a browser (no
DISPLAY, stdin = /dev/null) completes and writes a valid token file.

### E3 — DEPLOYMENT.md: zero-build-step QEMU instructions

**What:** Add a "fast path" to Path 2 (Linux QEMU) using pre-built release artifacts:

```bash
# Fast path (no build step)
TAG=$(curl -s https://api.github.com/repos/0x89karan/runtime1/releases/latest \
  | jq -r .tag_name)
curl -Lo bzImage \
  "https://github.com/0x89karan/runtime1/releases/download/${TAG}/agentos-${TAG#v}-x86_64-bzImage"
curl -Lo rootfs.cpio.gz \
  "https://github.com/0x89karan/runtime1/releases/download/${TAG}/agentos-${TAG#v}-x86_64-rootfs.cpio.gz"
curl -Lo SHA256SUMS \
  "https://github.com/0x89karan/runtime1/releases/download/${TAG}/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS
sudo cp bzImage rootfs.cpio.gz /opt/agentos/
```

Also add a note about `agentctl auth google --device` for headless servers, replacing
the SSH-tunnel workaround.

**Acceptance:** A new Linux operator can follow the guide without any Buildroot
dependency.

---

### E4 — install.sh convenience installer (added by CEO review)

**What:** A shell script `install.sh` at repo root that wraps the E3 curl commands into a single
verified download. Operators run:
```bash
curl -fsSL https://github.com/0x89karan/runtime1/releases/latest/download/install.sh -o install.sh
# Inspect the script, then:
bash install.sh
```
The script:
- Detects arch (uname -m) — errors clearly on unsupported arch
- Fetches latest release tag from GitHub API (or uses `AGENTOS_VERSION` env override)
- Downloads bzImage + rootfs.cpio.gz + SHA256SUMS with `curl -fsSL`
- Verifies SHA256 checksums before writing anything to disk
- Copies to `/opt/agentos/` (requires sudo, prompts if needed)
- Prints success message with next-step DEPLOYMENT.md link

**Security:** Download-then-verify pattern (never `curl | sh`). `set -euo pipefail`. SHA256 mismatch → abort with clear error.

**Acceptance:** `bash install.sh` on a fresh Ubuntu 22.04 x86_64 box downloads and verifies the latest pre-built images without requiring git or Buildroot.

---

## Not in scope

- **aarch64 prebuilt distro images** — Buildroot aarch64 on CI needs `qemu-system-aarch64`
  + cross-toolchain, heavy. Apple Silicon operators use Docker. Deferred.
- **Browser approval UI on :8080** — `distro/agentos-cos.service` has the hostfwd; the
  actual web UI is a separate increment.
- **Credential broker in QEMU mode** — QEMU secrets come via `agentos.env`; the
  credential gateway (cred.3) is not configured in `cos.agents.toml`. Deferred.
- **Automated download + verify script** — `install.sh` convenience wrapper deferred;
  DEPLOYMENT.md curl commands are sufficient for v1.

---

## Files changed

```
.github/workflows/release.yml         # E1: distro build job + SHA256 attachment
agentctl/Cargo.toml                    # E2: no new deps (reqwest blocking already present)
agentctl/src/auth/util.rs              # E2: write_mode_600 moved here (DRY — shared by PKCE + device)
agentctl/src/auth/google.rs            # E2: import write_mode_600 from auth::util
agentctl/src/auth/google_device.rs     # E2: new device-flow module (RFC 8628 poll loop)
agentctl/src/auth/mod.rs               # E2: expose google_device + util
agentctl/src/main.rs                   # E2: --device flag on auth google subcommand
install.sh                             # E4: convenience installer (download + verify)
docs/DEPLOYMENT.md                     # E3: fast path via install.sh + device flow note
docs/ROADMAP.md                        # mark dx.4 done
CHANGELOG.md                           # v0.71.0 entry
```

---

## Key implementation notes (from review)

**E1 — SHA256SUMS:** The existing `build-release` job already produces `dist/SHA256SUMS` (covering
agentd + agentctl binaries). The new distro job must produce a SEPARATE file named
`agentos-${VERSION}-x86_64-SHA256SUMS` to avoid overwriting the binary checksums on the same
GitHub Release. The DEPLOYMENT.md verify step uses the distro-specific file.

**E1 — Build deps:** The distro CI job must install the same kernel build dependencies as
`qemu-boot.yml`: `sudo apt-get install -y libelf-dev libssl-dev bc bison flex`. Set
`timeout-minutes: 90` (cold Buildroot build takes ~30 min; add margin).

**E2 — Device flow `access_type=offline`:** The device authorization POST must include
`access_type=offline`. Without it, Google does not return a `refresh_token` and the token file
is useless after expiry.

**E2 — RFC 8628 `slow_down` handling:** On `slow_down` error, add 5 seconds to the current
poll interval (per RFC 8628 §3.5). Do NOT double the interval — doubling is unnecessarily harsh.

**E2 — `option_env!` for client_id:** Use `option_env!("OAUTH_CLIENT_ID")` as compile-time
default, with a runtime `std::env::var("OAUTH_CLIENT_ID")` override. Surface a clear error if
both are absent (do not silently use an empty string).

**E4 — install.sh SHA256:** Do not use `sha256sum --check --ignore-missing SHA256SUMS` — if
filenames don't match, entries are silently skipped. Instead, use the distro-specific
`agentos-VERSION-x86_64-SHA256SUMS` file so the check is exact.

**E4 — install.sh credential ownership:** After copying to `/opt/agentos/`, print a reminder:
`"Run: sudo chown -R agentos:agentos /home/agentos/.agentos-secrets"`. The device flow writes
to the current user's home; the systemd service runs as `agentos`.

**E4 — Arch check:** `uname -m` check at top of script: emit clear error for non-x86_64
(`"aarch64 detected — Docker path is recommended for Apple Silicon: docker compose up cos"`).

**E3 — agentctl install step:** The fast path must start with installing `agentctl` itself.
Operators on a fresh Linux VPS do not have it. Step 0: curl the release binary, chmod +x,
mv to /usr/local/bin. Add this before any device auth step.

**E3 — No `jq` dependency:** Replace `TAG=$(curl -s ... | jq -r .tag_name)` with portable
grep/sed: `TAG=$(curl -s ... | grep '"tag_name"' | sed 's/.*"tag_name": "\([^"]*\)".*/\1/')`.
Minimal VPS images (Debian net-install, Alpine) often lack `jq`.

**E3 — Portable tag stripping:** Replace `${TAG#v}` (bash-specific parameter expansion) with
`TAG_BARE=$(echo "$TAG" | sed 's/^v//')`. Minimal images may default to dash, not bash.

**E3 — SHA256 failure recovery:** Add one line after the sha256sum check: "If verification
fails, re-run the curl commands above and retry. Do not proceed if verification fails."

**E1 — Boot smoke test:** Before uploading release artifacts, run a 30-second QEMU
boot smoke test (same pattern as qemu-boot.yml) to confirm the image actually boots.
This catches rootfs packing bugs that wouldn't be caught by SHA256 existence alone.

**E2 — Poll security:** Bound total poll time with monotonic `std::time::Instant` (not
`expires_in` trusting server clock). Strip terminal escape sequences from `verification_url`
and user code display — print only printable ASCII. Handle `invalid_grant` (expired or
revoked device code) with a clear "Code expired — run again." message.

---

## Test plan

- `release.yml` job: verified in CI on tag push (acceptance: artifacts present in release)
- `agentctl auth google --device`: unit tests for:
  - device auth request format (client_id, scope, grant_type)
  - poll loop: `authorization_pending` → retry, `slow_down` → doubled interval, `expired_token` → bail
  - token response deserialization → file write
  - no-DISPLAY smoke test (stdin /dev/null)
- `DEPLOYMENT.md`: manual review (no automated test; it's documentation)

---

## Acceptance criteria

1. `cargo build && cargo clippy -- -D warnings && cargo test` all green
2. `release.yml` attaches `agentos-VERSION-x86_64-bzImage` + `agentos-VERSION-x86_64-rootfs.cpio.gz` to GitHub Release (verified via dry-run job log check)
3. `agentctl auth google --device` compiles and unit tests pass
4. DEPLOYMENT.md Path 2 has a "fast path" section showing curl-based download

---

## Decision audit trail

| ID | Dimension | Decision | Source |
|----|-----------|----------|--------|
| D-scope-E2 | Device flow scope | **Include E2** — headless VPS path is the roadmap's intent | User gate |
| D-secret | CLIENT_SECRET mechanism | **`option_env!("OAUTH_CLIENT_SECRET")`** — same model as gcloud; zero setup for operators | User gate |
| D-sha256 | SHA256 naming | **Separate file** `agentos-VERSION-x86_64-SHA256SUMS` to avoid collision with binary checksums | Auto (eng-critical) |
| D-offline | access_type=offline | **Required** in device auth POST — without it no refresh_token is returned | Auto (eng-high) |
| D-deps | Distro CI deps | **Copy from qemu-boot.yml**: `libelf-dev libssl-dev bc bison flex`, `timeout-minutes: 90` | Auto (eng-high) |
| D-boot | Release smoke test | **Boot test before upload** — 30 s QEMU boot smoke, same pattern as qemu-boot.yml | Auto (eng-high) |
| D-slowdown | RFC 8628 slow_down | **+5s additive** per §3.5, not doubling | Auto (eng-medium) |
| D-clientid | client_id mechanism | **`option_env!("OAUTH_CLIENT_ID")`** — compile-time default, runtime override | Auto (eng-medium) |
| D-agentctl | Fast path step 0 | **Explicit agentctl install** — curl binary, chmod+x, mv to /usr/local/bin | Auto (dx-critical) |
| D-recovery | SHA256 failure | **Recovery hint** after sha256sum command | Auto (dx-high) |
| D-arch | Arch guard | **`uname -m` check** with clear error for non-x86_64 | Auto (dx-high) |
| D-jq | jq dependency | **grep/sed fallback** — no jq dep, works on minimal VPS images | Auto (dx-medium) |
| D-bash | Bash-ism | **Portable `sed 's/^v//'`** instead of `${TAG#v}` | Auto (dx-low) |
| D-scope-E4 | install.sh | **Include E4** — CEO dual-voice consensus; CI artifacts are prerequisite | Auto (CEO) |
| D-poll | Poll security | **Monotonic time bound**; strip terminal escapes from URL/code display | Auto (eng2-security) |

---

## GSTACK REVIEW REPORT
