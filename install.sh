#!/usr/bin/env bash
# AgentOS prebuilt image installer — downloads, verifies, and installs to /opt/agentos.
# Usage: bash install.sh [--version v0.71.0]
# Requires: curl, sha256sum, sudo
set -euo pipefail

REPO="0x89karan/runtime1"
INSTALL_DIR="/opt/agentos"
AGENTOS_VERSION="${AGENTOS_VERSION:-}"

# ---------------------------------------------------------------------------
# Parse args
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            AGENTOS_VERSION="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: bash install.sh [--version v0.71.0]"
            echo "  --version TAG   Install a specific release (default: latest)"
            echo ""
            echo "Environment variables:"
            echo "  AGENTOS_VERSION   Same as --version"
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Arch check — x86_64 only (aarch64 users: docker compose up cos)
# ---------------------------------------------------------------------------
ARCH="$(uname -m)"
if [[ "$ARCH" != "x86_64" ]]; then
    echo "ERROR: Unsupported architecture: $ARCH"
    echo ""
    echo "Prebuilt QEMU images are x86_64 only."
    if [[ "$ARCH" == "aarch64" || "$ARCH" == "arm64" ]]; then
        echo "Apple Silicon / ARM Linux users: use the Docker path instead:"
        echo ""
        echo "  docker compose up -d cos"
    fi
    exit 1
fi

# ---------------------------------------------------------------------------
# Resolve release tag (no jq dependency — use grep/sed)
# ---------------------------------------------------------------------------
if [[ -z "$AGENTOS_VERSION" ]]; then
    echo "Fetching latest release tag..."
    RELEASE_JSON="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest")"
    AGENTOS_VERSION="$(echo "$RELEASE_JSON" | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/' | head -1)"
    if [[ -z "$AGENTOS_VERSION" ]]; then
        echo "ERROR: Could not determine latest release tag." >&2
        echo "Set AGENTOS_VERSION manually: AGENTOS_VERSION=v0.71.0 bash install.sh" >&2
        exit 1
    fi
fi
# Normalize: ensure version tag starts with 'v' (users often omit it)
[[ "$AGENTOS_VERSION" == v* ]] || AGENTOS_VERSION="v${AGENTOS_VERSION}"
# Strip leading 'v' for filenames (tag: v0.71.0, filename: 0.71.0)
VERSION="$(echo "$AGENTOS_VERSION" | sed 's/^v//')"
echo "Installing AgentOS ${AGENTOS_VERSION} (x86_64)..."

# ---------------------------------------------------------------------------
# Download artifacts to a temp directory
# ---------------------------------------------------------------------------
WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

BASE_URL="https://github.com/${REPO}/releases/download/${AGENTOS_VERSION}"
BZIMAGE="agentos-${VERSION}-x86_64-bzImage"
ROOTFS="agentos-${VERSION}-x86_64-rootfs.cpio.gz"
SHA256FILE="agentos-${VERSION}-x86_64-SHA256SUMS"

echo "Downloading kernel image..."
curl -fsSL --output "${WORKDIR}/${BZIMAGE}"   "${BASE_URL}/${BZIMAGE}"
echo "Downloading rootfs..."
curl -fsSL --output "${WORKDIR}/${ROOTFS}"    "${BASE_URL}/${ROOTFS}"
echo "Downloading checksums..."
curl -fsSL --output "${WORKDIR}/${SHA256FILE}" "${BASE_URL}/${SHA256FILE}"

# ---------------------------------------------------------------------------
# Verify SHA256 checksums — exact match, no --ignore-missing
# ---------------------------------------------------------------------------
echo "Verifying checksums..."
(cd "$WORKDIR" && sha256sum --check "${SHA256FILE}")
echo "Checksums verified."

# ---------------------------------------------------------------------------
# Install
# ---------------------------------------------------------------------------
echo "Installing to ${INSTALL_DIR} (requires sudo)..."
sudo mkdir -p "${INSTALL_DIR}"
sudo cp "${WORKDIR}/${BZIMAGE}"  "${INSTALL_DIR}/bzImage"
sudo cp "${WORKDIR}/${ROOTFS}"   "${INSTALL_DIR}/rootfs.cpio.gz"

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
echo ""
echo "AgentOS ${AGENTOS_VERSION} installed to ${INSTALL_DIR}/:"
ls -lh "${INSTALL_DIR}/bzImage" "${INSTALL_DIR}/rootfs.cpio.gz" | awk '{print "  " $5, $NF}'
echo ""
echo "Next steps — see docs/DEPLOYMENT.md Path 2 for full instructions:"
echo ""
echo "  1. Create system user:        sudo useradd -m -r -s /usr/sbin/nologin agentos"
echo "  2. Provision secrets:         /home/agentos/.agentos-secrets/agentos.env"
echo "  3. Google credentials (headless):  agentctl auth google --device"
echo "     Google credentials (Mac):       agentctl auth google"
echo "  4. Install service:           sudo cp distro/agentos-cos.service /etc/systemd/system/"
echo "                                sudo systemctl enable --now agentos-cos"
echo ""
echo "Service note: if Google credentials were provisioned as a different user, copy them:"
echo "  sudo cp ~/.agentos-secrets/google.json /home/agentos/.agentos-secrets/google.json"
echo "  sudo chown agentos:agentos /home/agentos/.agentos-secrets/google.json"
