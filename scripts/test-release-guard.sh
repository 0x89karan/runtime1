#!/usr/bin/env bash
# Scenario suite for scripts/release-guard.sh (ci.1 review-fix round).
# Builds a temp git repo with a fake origin, shims gh/docker/cargo/python3 on
# PATH, and runs the guard through every branch of the new per-caller
# fail-closed reuse logic plus regression checks on guards 1-3.
set -u

GUARD="$(cd "$(dirname "$0")" && pwd)/release-guard.sh"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Identity via env so EVERY repo this harness creates (origin + clone) can
# commit. Per-repo `git config` on the origin alone is not enough: clones don't
# inherit it, and GitHub's runners have no global identity + a dotless hostname,
# so git's ident auto-detect is FATAL there (works on macOS only because
# hostnames end in .local — a false local green).
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t
export GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

pass=0; fail=0
report() {
  local name="$1" want_rc="$2" got_rc="$3" want_grep="$4" log="$5"
  if [ "$got_rc" = "$want_rc" ] && grep -q "$want_grep" "$log"; then
    echo "PASS: $name"; pass=$((pass+1))
  else
    echo "FAIL: $name (want rc=$want_rc grep='$want_grep'; got rc=$got_rc)"; sed 's/^/    /' "$log"; fail=$((fail+1))
  fi
}

# --- repo fixture: origin with main @ v0.88.0, plus an older v0.87.0 tag ---
mkdir -p "$WORK/origin" && cd "$WORK/origin"
git init -q -b main
git config user.email t@t && git config user.name t
mkdir agentd
cat > agentd/Cargo.toml <<'EOF'
[package]
name = "agentd"
version = "0.88.0"
EOF
git add -A && git commit -qm one
git tag v0.87.0   # pre-existing older tag (monotonicity candidate set)
git commit -qm two --allow-empty
git tag v0.88.0   # distinct commit — git describe --exact-match must be unambiguous
git commit -qm three --allow-empty   # main moves past the tag (still ancestor)

cd "$WORK"
git clone -q origin clone
cd clone
git fetch -q --tags origin

# --- PATH shims -------------------------------------------------------------
mkdir -p "$WORK/bin"
# cargo metadata replacement: emit enough JSON for the python parser.
cat > "$WORK/bin/cargo" <<EOF
#!/bin/sh
echo '{"packages":[{"name":"agentd","version":"0.88.0"}]}'
EOF
# gh shim: behavior selected by MOCK_GH.
cat > "$WORK/bin/gh" <<'EOF'
#!/bin/sh
case "${MOCK_GH:-404}" in
  404)    echo "gh: Not Found (HTTP 404)" >&2; exit 1 ;;
  exists) echo '{"tag_name":"v0.88.0"}'; exit 0 ;;
  auth)   echo "gh: Bad credentials (HTTP 401)" >&2; exit 1 ;;
  ratelimit) echo "gh: API rate limit exceeded (HTTP 403)" >&2; exit 1 ;;
esac
EOF
# docker shim: behavior selected by MOCK_DOCKER.
cat > "$WORK/bin/docker" <<'EOF'
#!/bin/sh
case "${MOCK_DOCKER:-unknown}" in
  unknown) echo "manifest unknown" >&2; exit 1 ;;
  exists)  echo '{"schemaVersion":2}'; exit 0 ;;
  denied)  echo "unauthorized: authentication required" >&2; exit 1 ;;
  noauth)  echo "no basic auth credentials" >&2; exit 1 ;;
  proxy404) echo "error: received unexpected response: 404 Not Found" >&2; exit 1 ;;
  helper)  echo "error getting credentials - err: exec: \"docker-credential-desktop\": executable file not found in \$PATH" >&2; exit 1 ;;
  coreonly)
    # Partial-publish state: only the -core version tag exists.
    case "$3" in
      *-core) echo '{"schemaVersion":2}'; exit 0 ;;
      *)      echo "manifest unknown" >&2; exit 1 ;;
    esac ;;
  allexist) echo '{"schemaVersion":2}'; exit 0 ;;
esac
EOF
chmod +x "$WORK/bin/cargo" "$WORK/bin/gh" "$WORK/bin/docker"
export PATH="$WORK/bin:$PATH"

run() { # run <env...> -- captures rc + log
  local log="$WORK/out.log"
  "$@" > "$log" 2>&1
  echo $?
}

L="$WORK/out.log"
export GITHUB_REPOSITORY="0x89karan/runtime1"

# --- S1: tag path, --check release, gh 404 → all green ----------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 GH_TOKEN=x MOCK_GH=404 run "$GUARD" --check release)
report "S1 release-check 404 passes" 0 "$rc" "all guards green" "$L"

# --- S2: --check release, release exists → refuse ---------------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 GH_TOKEN=x MOCK_GH=exists run "$GUARD" --check release)
report "S2 release exists refuses" 1 "$rc" "version already published" "$L"

# --- S3: --check release, auth error → FAIL CLOSED --------------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 GH_TOKEN=x MOCK_GH=auth run "$GUARD" --check release)
report "S3 gh auth error fails closed" 1 "$rc" "fail-closed" "$L"

# --- S4: --check release, rate limit → FAIL CLOSED --------------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 GH_TOKEN=x MOCK_GH=ratelimit run "$GUARD" --check release)
report "S4 gh rate limit fails closed" 1 "$rc" "fail-closed" "$L"

# --- S5: --check release, no GH_TOKEN → FAIL CLOSED -------------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 run "$GUARD" --check release)
report "S5 missing GH_TOKEN fails closed" 1 "$rc" "requires GH_TOKEN" "$L"

# --- S6: --check image, manifest unknown → all green ------------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=unknown run "$GUARD" --check image)
report "S6 image-check manifest-unknown passes" 0 "$rc" "all guards green" "$L"

# --- S7: --check image, manifest exists → refuse ----------------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=exists run "$GUARD" --check image)
report "S7 image exists refuses" 1 "$rc" "version already published" "$L"

# --- S8: --check image, auth denied → FAIL CLOSED ---------------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=denied run "$GUARD" --check image)
report "S8 docker auth error fails closed" 1 "$rc" "fail-closed" "$L"

# --- S8b: --check image, missing login → FAIL CLOSED ------------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=noauth run "$GUARD" --check image)
report "S8b docker no-login fails closed" 1 "$rc" "fail-closed" "$L"

# --- S7b: partial publish (-core only) must refuse --------------------------
# (probing only :vX.Y.Z would let a partial or foreign -core publish pass)
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=coreonly run "$GUARD" --check image)
report "S7b partial -core publish refuses" 1 "$rc" "version already published" "$L"

# --- S7c/d/e: image-prepush semantics (Codex P1 TOCTOU re-check) -------------
# Complete foreign publish (all 3 manifests) → refuse
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=allexist run "$GUARD" --check image-prepush)
report "S7c prepush all-manifests refuses" 1 "$rc" "complete publish" "$L"
# Partial (own interrupted push) → proceed, named in the log
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=coreonly run "$GUARD" --check image-prepush)
report "S7d prepush partial proceeds" 0 "$rc" "intended recovery" "$L"
# Probe error → still fail closed
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=denied run "$GUARD" --check image-prepush)
report "S7e prepush probe error fails closed" 1 "$rc" "fail-closed" "$L"

# --- S8c: bare "not found" strings must NOT read as unpublished --------------
# (security review: proxy 404 pages and credential-helper "executable file not
# found in $PATH" both contain "not found" — the classifier must be
# manifest-specific, or the probe fails open exactly when blind)
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=proxy404 run "$GUARD" --check image)
report "S8c proxy 404 page fails closed" 1 "$rc" "fail-closed" "$L"
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 MOCK_DOCKER=helper run "$GUARD" --check image)
report "S8d credential-helper missing fails closed" 1 "$rc" "fail-closed" "$L"

# --- S9: cross-workflow simulation: release exists but --check image passes -
# (the F1 deadlock: publish-docker must NOT refuse on its sibling's Release)
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 GH_TOKEN=x MOCK_GH=exists MOCK_DOCKER=unknown run "$GUARD" --check image)
report "S9 sibling Release does not block image publish" 0 "$rc" "all guards green" "$L"

# --- S10: missing/bad --check → usage error ---------------------------------
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 run "$GUARD")
report "S10 missing --check errors" 1 "$rc" "usage" "$L"
rc=$(GITHUB_REF=refs/tags/v0.88.0 GITHUB_REF_NAME=v0.88.0 run "$GUARD" --check both)
report "S10b bad --check value errors" 1 "$rc" "usage" "$L"

# --- S11 regression: version mismatch still refuses -------------------------
git tag v0.89.0 >/dev/null 2>&1   # tag exists locally; Cargo says 0.88.0
rc=$(GITHUB_REF=refs/tags/v0.89.0 GITHUB_REF_NAME=v0.89.0 MOCK_DOCKER=unknown run "$GUARD" --check image)
report "S11 tag!=Cargo refuses" 1 "$rc" "version mismatch" "$L"
git tag -d v0.89.0 >/dev/null

# --- S12 regression: non-monotonic refuses ----------------------------------
# Retarget: pretend we're publishing v0.87.0 while v0.88.0 exists.
cat > "$WORK/bin/cargo" <<EOF
#!/bin/sh
echo '{"packages":[{"name":"agentd","version":"0.87.0"}]}'
EOF
rc=$(GITHUB_REF=refs/tags/v0.87.0 GITHUB_REF_NAME=v0.87.0 MOCK_DOCKER=unknown run "$GUARD" --check image)
report "S12 non-monotonic refuses" 1 "$rc" "non-monotonic" "$L"
cat > "$WORK/bin/cargo" <<EOF
#!/bin/sh
echo '{"packages":[{"name":"agentd","version":"0.88.0"}]}'
EOF

# --- S13 regression: tag not on main refuses --------------------------------
git checkout -qb stray
git commit -qm stray --allow-empty
git tag v0.90.0
cat > "$WORK/bin/cargo" <<EOF
#!/bin/sh
echo '{"packages":[{"name":"agentd","version":"0.90.0"}]}'
EOF
rc=$(GITHUB_REF=refs/tags/v0.90.0 GITHUB_REF_NAME=v0.90.0 MOCK_DOCKER=unknown run "$GUARD" --check image)
report "S13 tag off main refuses" 1 "$rc" "tag not on main" "$L"

# --- S14: non-tag ref (dispatch): only reuse guard runs ---------------------
git checkout -q main
git tag -d v0.90.0 >/dev/null   # S13's off-main tag must not poison later monotonicity checks
cat > "$WORK/bin/cargo" <<EOF
#!/bin/sh
echo '{"packages":[{"name":"agentd","version":"0.88.0"}]}'
EOF
rc=$(GITHUB_REF=refs/heads/main GITHUB_REF_NAME=main MOCK_DOCKER=unknown run "$GUARD" --check image)
report "S14 dispatch path reuse-only passes" 0 "$rc" "tag guards skipped" "$L"
rc=$(GITHUB_REF=refs/heads/main GITHUB_REF_NAME=main MOCK_DOCKER=exists run "$GUARD" --check image)
report "S14b dispatch path reuse refuses" 1 "$rc" "version already published" "$L"

# --- S15: local detached-HEAD tag checkout (F5) -----------------------------
git checkout -q v0.88.0
unset GITHUB_REF GITHUB_REF_NAME 2>/dev/null || true
rc=$(MOCK_DOCKER=unknown run "$GUARD" --check image)
report "S15 detached-HEAD local run finds tag guards" 0 "$rc" "tag guards passed" "$L"
git checkout -q main

echo
echo "=== $pass passed, $fail failed ==="
# Total assertion (ship adversarial F8): a refactor that silently skips
# scenarios must not false-green. Bump EXPECTED when adding scenarios.
EXPECTED=24
if [ $((pass + fail)) -ne "$EXPECTED" ]; then
  echo "FAIL: ran $((pass + fail)) scenarios, expected $EXPECTED — harness itself is broken"
  exit 1
fi
exit "$fail"
