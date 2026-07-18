#!/usr/bin/env bash
# release-guard.sh (ci.1 / audit86-P1-6) — the single source of truth for
# publish-safety invariants, called by BOTH .github/workflows/ci.yml
# (publish-docker) and .github/workflows/release.yml. One script, two callers:
# hand-mirroring the guard is the drift disease this repo keeps treating.
#
# Usage: release-guard.sh --check image|release|image-prepush
#   --check image    (ci.yml release-guard job)  reuse-probe = ANY ghcr version
#                    manifest exists → refuse (first-line, fail-fast)
#   --check release  (release.yml)               reuse-probe = the GitHub Release
#   --check image-prepush (ci.yml publish-docker, inside the concurrency group,
#                    immediately before the pushes) — closes the TOCTOU where a
#                    second same-version publish queued behind the first carries
#                    a stale green guard (Codex P1). Refuses only if ALL version
#                    manifests exist (a COMPLETE publish won the race); a
#                    partial set means THIS version's interrupted publish and a
#                    re-run overwriting same-commit partials is the intended
#                    recovery, so it proceeds.
#
# Each caller probes ONLY the artifact it is about to write. Both workflows run
# concurrently on every tag push, and publish-docker starts ~35-50 min late
# (behind build-aarch64); if it probed the GitHub Release it would always see
# the one its sibling release.yml legitimately just created and refuse every
# first-ever publish (adversarial-review F1 — deterministic, not a race).
#
# Guards (tag pushes):
#   1. Ancestry    — the tagged commit must be on origin/main (the v0.86.0
#                    staleness incident: a tag on a stale branch republished
#                    :latest from old code).
#   2. Tag==Cargo  — vX.Y.Z must equal agentd's Cargo.toml version (images are
#                    tagged from cargo metadata; artifacts from the git tag —
#                    divergence ships mislabeled binaries).
#   3. Monotonic   — the version must exceed every OTHER v* tag (the pushed tag
#                    is already in refs/tags under fetch-depth:0, so it is
#                    excluded from the comparison). Strict vMAJOR.MINOR.PATCH
#                    only; this repo uses no prereleases.
# Guard (ALL paths, including workflow_dispatch from main — which also pushes
# :vCARGO_VERSION image tags, so an unbumped dispatch would silently overwrite
# a published version):
#   4. Reuse       — refuse if this caller's artifact for this Cargo version
#                    already exists. Probes FAIL CLOSED: only an explicit
#                    not-found verdict (HTTP 404 / "manifest unknown") passes;
#                    auth failures, rate limits, network errors, and missing
#                    CLIs abort the publish (adversarial-review F2 — a
#                    fail-open probe green-lights exactly when blind).
#                    --check image needs ghcr login first; --check release
#                    needs GH_TOKEN exported.
#
# Requires: full history + tags (actions/checkout with fetch-depth: 0).
# Operator notes: after a successful publish, "Re-run all jobs" re-runs this
# guard and correctly refuses (the artifact now exists) — use "Re-run failed
# jobs" to retry a flaked downstream job. To intentionally redo a version:
# delete BOTH the GitHub release/tag AND the ghcr manifest, then re-tag.
# A workflow_dispatch publish CONSUMES the current Cargo version for images:
# tagging that same version later half-publishes (Release yes, images refused
# by this guard). Bump the version before tagging after any dispatch publish.
set -euo pipefail

CHECK="${2:-}"
[ "${1:-}" = "--check" ] && { [ "$CHECK" = "image" ] || [ "$CHECK" = "release" ] || [ "$CHECK" = "image-prepush" ]; } \
  || { echo "::error title=release-guard::usage: release-guard.sh --check image|release|image-prepush"; exit 1; }

REPO_SLUG="${GITHUB_REPOSITORY:-0x89karan/runtime1}"
# Local runs: symbolic-ref fails on a detached HEAD (checked-out tag), which
# previously skipped all tag guards with a false all-green — fall back to the
# exact-match tag before giving up (adversarial-review F5).
REF="${GITHUB_REF:-$(git symbolic-ref -q HEAD \
  || { t=$(git describe --exact-match --tags 2>/dev/null) && echo "refs/tags/$t"; } \
  || echo unknown)}"
REF_NAME="${GITHUB_REF_NAME:-${REF##*/}}"

# `|| true`: on a cargo/python failure the friendly error below must fire
# instead of a raw traceback killing the script mid-assignment (F4). Still
# fails closed either way. ci.yml's "Get version" step extracts the same value
# via jq — that copy tags the images, this one gates them; drift between the
# two is caught by guard 2 (tag==Cargo) on the tag path.
cargo_version=$(cargo metadata --no-deps --format-version 1 --manifest-path agentd/Cargo.toml \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="agentd"))' \
  || true)
[ -n "$cargo_version" ] || { echo "::error title=release-guard::could not read agentd Cargo version"; exit 1; }
echo "release-guard: cargo version = $cargo_version, ref = $REF, check = $CHECK"

case "$REF" in
  refs/tags/v*)
    tag="$REF_NAME"

    # Strict format first — the sort -V comparison below is only defined for it.
    echo "$tag" | grep -qE '^v[0-9]+\.[0-9]+\.[0-9]+$' \
      || { echo "::error title=release-guard: bad tag format::'$tag' is not vMAJOR.MINOR.PATCH (no prereleases)"; exit 1; }

    # 1. Ancestry. Resolve the tag to a COMMIT first: on annotated-tag pushes
    #    GITHUB_SHA can be the tag object, which merge-base can't use.
    commit=$(git rev-parse "${tag}^{commit}")
    git fetch origin main --quiet
    if ! git merge-base --is-ancestor "$commit" origin/main; then
      echo "::error title=release-guard: tag not on main::$tag → $commit is not an ancestor of origin/main."
      echo "If the commit IS on main (tag pushed before the branch), re-run this job."
      echo "Otherwise: delete the tag, re-tag the intended main commit, push again."
      exit 1
    fi

    # 2. Tag == Cargo.
    tag_version="${tag#v}"
    if [ "$tag_version" != "$cargo_version" ]; then
      echo "::error title=release-guard: version mismatch::tag says $tag_version, agentd/Cargo.toml says $cargo_version."
      echo "Fix: bump Cargo.toml (+ CLAUDE.md line, test-enforced) and re-tag, or re-tag with v$cargo_version."
      exit 1
    fi

    # 3. Monotonicity — exclude the tag being published from the candidate set.
    #    -F: the tag is a literal, not a regex (dots must not match any char).
    max_other=$(git tag -l 'v*' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | grep -Fvx "$tag" | sort -V | tail -1 || true)
    if [ -n "$max_other" ] && [ "$(printf '%s\n%s\n' "$max_other" "$tag" | sort -V | tail -1)" != "$tag" ]; then
      echo "::error title=release-guard: non-monotonic tag::$tag is not newer than existing $max_other — tagging an old commit would re-point :latest backwards."
      echo "Fix: choose a version above $max_other."
      exit 1
    fi
    echo "release-guard: tag guards passed ($tag on main, == Cargo, > ${max_other:-none})"
    ;;
  *)
    echo "release-guard: non-tag ref ($REF) — tag guards skipped (workflow_dispatch from main); reuse guard still applies"
    ;;
esac

# 4. Reuse — this caller's artifact only, fail-closed on probe errors.
refuse_published() {
  echo "::error title=release-guard: version already published::$1."
  echo "Never republish a used version. To redo v$cargo_version intentionally:"
  echo "  1. gh release delete v$cargo_version --repo $REPO_SLUG (and delete the git tag)"
  echo "  2. delete the ghcr :v$cargo_version manifest (Packages UI or API)"
  echo "  3. re-tag and push. Otherwise: bump to a new version."
  exit 1
}

case "$CHECK" in
  release)
    [ -n "${GH_TOKEN:-}" ] \
      || { echo "::error title=release-guard::--check release requires GH_TOKEN (probe would be blind)"; exit 1; }
    command -v gh >/dev/null 2>&1 \
      || { echo "::error title=release-guard::gh CLI not found — cannot probe for an existing release"; exit 1; }
    set +e
    # 2>&1 >/dev/null: capture stderr (where gh writes "HTTP 404"), discard stdout.
    gh_err=$(gh api "repos/$REPO_SLUG/releases/tags/v$cargo_version" 2>&1 >/dev/null)
    gh_rc=$?
    set -e
    if [ "$gh_rc" -eq 0 ]; then
      refuse_published "GitHub release v$cargo_version exists"
    elif printf '%s' "$gh_err" | grep -q "HTTP 404"; then
      : # explicit not-found — the only verdict that passes
    else
      echo "::error title=release-guard: release probe failed (fail-closed)::gh api rc=$gh_rc — cannot prove v$cargo_version is unpublished. Output:"
      printf '%s\n' "$gh_err"
      exit 1
    fi
    ;;
  image|image-prepush)
    command -v docker >/dev/null 2>&1 \
      || { echo "::error title=release-guard::docker CLI not found — cannot probe the ghcr manifest"; exit 1; }
    # ALL immutable version tags publish-docker writes, not just :vX.Y.Z
    # (ship adversarial: probing one tag lets a partial or foreign publish of
    # :vX.Y.Z-core pass the guard and be overwritten). Mutable tags (:latest,
    # :core, :full) are re-pointed by design and never probed.
    existing=0
    for image_tag in "v$cargo_version" "v$cargo_version-core" "v$cargo_version-full"; do
      set +e
      docker_err=$(docker manifest inspect "ghcr.io/$REPO_SLUG:$image_tag" 2>&1 >/dev/null)
      docker_rc=$?
      set -e
      if [ "$docker_rc" -eq 0 ]; then
        # First-line check: ANY existing version manifest refuses. Pre-push
        # re-check: keep counting — only a COMPLETE set refuses (see usage).
        [ "$CHECK" = "image" ] && refuse_published "ghcr.io manifest :$image_tag exists"
        existing=$((existing + 1))
      # Manifest-specific verdicts ONLY — never bare "not found", which also
      # matches "docker: command not found", credential-helper "executable file
      # not found in $PATH", and proxy 404 pages (all probe-blind states that
      # must fail closed, not pass).
      elif printf '%s' "$docker_err" | grep -qiE "manifest unknown|no such manifest|manifest .* not found|name unknown"; then
        : # explicit not-found — the only verdict that passes
      else
        echo "::error title=release-guard: manifest probe failed (fail-closed)::docker manifest inspect rc=$docker_rc for :$image_tag — cannot prove it is unpublished (missing ghcr login?). Output:"
        printf '%s\n' "$docker_err"
        exit 1
      fi
    done
    if [ "$CHECK" = "image-prepush" ]; then
      if [ "$existing" -eq 3 ]; then
        refuse_published "all three ghcr version manifests for v$cargo_version exist — a complete publish of this version already happened (likely a same-version run that won the race)"
      elif [ "$existing" -gt 0 ]; then
        echo "release-guard: pre-push re-check found $existing/3 version manifests — interrupted publish of THIS version; re-run overwrite is the intended recovery"
      fi
    fi
    ;;
esac
echo "release-guard: reuse guard passed ($CHECK artifact for v$cargo_version unpublished) — all guards green"
