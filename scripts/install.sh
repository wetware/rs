#!/bin/sh
# Wetware installer (IPFS-first)
# Usage: curl -sSf https://raw.githubusercontent.com/wetware/ww/master/scripts/install.sh | sh
#   or:  curl -sSf ... | sh -s -- --version <CID>
set -eu

IPNS_NAME="/ipns/releases.wetware.run"
GATEWAY_BASE="https://dweb.link/ipns/releases.wetware.run"
IPNS_TIMEOUT=30
VERSION_CID=""
WW_HOME="${HOME}/.ww"

# Parse arguments
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION_CID="$2"; shift 2 ;;
    --help)
      echo "Usage: install.sh [--version CID]"
      echo "  --version  Install a specific release by immutable CID"
      echo "             (default: resolve latest via IPNS)"
      exit 0
      ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# --- Step 1: Detect platform ---
OS="$(uname -s)"
case "$OS" in
  Linux)  OS_NAME="linux" ;;
  Darwin) OS_NAME="macos" ;;
  *)
    echo "Error: unsupported operating system: $OS"
    echo "Supported: Linux, macOS"
    exit 1
    ;;
esac

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)  ARCH_NAME="x86_64" ;;
  aarch64|arm64) ARCH_NAME="aarch64" ;;
  *)
    echo "Error: unsupported architecture: $ARCH"
    echo "Supported: x86_64, aarch64"
    exit 1
    ;;
esac

echo "Detected platform: ${OS_NAME}/${ARCH_NAME}"

# --- Step 2: Check IPFS availability ---
if ! command -v ipfs >/dev/null 2>&1; then
  echo "Error: Wetware requires IPFS. Install Kubo: https://docs.ipfs.tech/install/"
  exit 1
fi

if ! ipfs id >/dev/null 2>&1; then
  echo "Error: IPFS daemon is not running."
  echo "Start it with: ipfs daemon &"
  echo "Install Kubo if needed: https://docs.ipfs.tech/install/"
  exit 1
fi

# --- Step 3: Resolve IPNS base path ---
USE_GATEWAY=0

if [ -n "$VERSION_CID" ]; then
  # Version pinning: use CID directly, skip IPNS resolution
  IPFS_BASE="/ipfs/${VERSION_CID}"
  echo "Installing pinned release: ${VERSION_CID}"
else
  echo "Resolving latest release via IPNS (may take up to ${IPNS_TIMEOUT}s on first run)..."
  RESOLVED_CID=$(timeout "$IPNS_TIMEOUT" ipfs name resolve "$IPNS_NAME" 2>/dev/null) || true

  if [ -n "$RESOLVED_CID" ]; then
    IPFS_BASE="$RESOLVED_CID"
    echo "Resolved: ${IPFS_BASE}"
  else
    echo "IPNS resolution timed out. Falling back to HTTP gateway..."
    USE_GATEWAY=1
  fi
fi

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# Helper: fetch a file from IPFS (local or gateway fallback)
ipfs_cat() {
  _path="$1"
  if [ "$USE_GATEWAY" -eq 1 ]; then
    curl -sSf "${GATEWAY_BASE}${_path}"
  else
    ipfs cat "${IPFS_BASE}${_path}"
  fi
}

# Helper: fetch a directory from IPFS (local or gateway fallback)
ipfs_get_dir() {
  _path="$1"
  _output="$2"
  if [ "$USE_GATEWAY" -eq 1 ]; then
    # Gateway fallback: download individual files is impractical for dirs.
    # Use ipfs get with the gateway-resolved path instead.
    echo "Warning: directory fetch via gateway is not supported."
    echo "Please ensure your IPFS daemon can resolve IPNS, or use --version <CID>."
    exit 1
  else
    ipfs get "${IPFS_BASE}${_path}" -o "$_output"
  fi
}

# --- Step 4: Fetch binary ---
BIN_PATH="/bin/${OS_NAME}/${ARCH_NAME}/ww"
echo "Fetching binary (${OS_NAME}/${ARCH_NAME})..."
ipfs_cat "$BIN_PATH" > "${TMPDIR}/ww" || {
  echo ""
  echo "Error: could not fetch binary from IPFS."
  echo "The release may not exist or IPFS content is unavailable."
  echo ""
  echo "Fallback: download manually from GitHub Releases:"
  echo "  https://github.com/wetware/ww/releases"
  exit 1
}

# --- Step 5: Fetch and verify checksums ---
echo "Fetching checksums..."
ipfs_cat "/CHECKSUMS.txt" > "${TMPDIR}/CHECKSUMS.txt" || {
  echo "Warning: could not fetch checksums. Skipping verification."
}

if [ -f "${TMPDIR}/CHECKSUMS.txt" ]; then
  echo "Verifying checksum..."
  if command -v b3sum >/dev/null 2>&1; then
    EXPECTED=$(grep "$BIN_PATH" "${TMPDIR}/CHECKSUMS.txt" | grep -v "^#" | head -1 | awk '{print $1}')
    ACTUAL=$(b3sum --no-names "${TMPDIR}/ww")
    ALGO="blake3"
  else
    # Fall back to sha256 (listed after "# sha256" marker in CHECKSUMS.txt)
    EXPECTED=$(sed -n '/^# sha256/,$ p' "${TMPDIR}/CHECKSUMS.txt" | grep "$BIN_PATH" | head -1 | awk '{print $1}')
    ACTUAL=$(sha256sum "${TMPDIR}/ww" 2>/dev/null || shasum -a 256 "${TMPDIR}/ww")
    ACTUAL=$(echo "$ACTUAL" | awk '{print $1}')
    ALGO="sha256"
  fi

  if [ -n "$EXPECTED" ] && [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "Error: checksum mismatch (${ALGO})"
    echo "  expected: ${EXPECTED}"
    echo "  got:      ${ACTUAL}"
    echo ""
    echo "The download may be corrupted. Try again, or download manually:"
    echo "  https://github.com/wetware/ww/releases"
    rm -f "${TMPDIR}/ww"
    exit 1
  elif [ -n "$EXPECTED" ]; then
    echo "Checksum OK (${ALGO})"
  else
    echo "Warning: could not find checksum for ${BIN_PATH} in CHECKSUMS.txt"
  fi
fi

# --- Step 6: Install binary ---
mkdir -p "${WW_HOME}/bin"
mv "${TMPDIR}/ww" "${WW_HOME}/bin/ww"
chmod +x "${WW_HOME}/bin/ww"
echo "Installed binary to ${WW_HOME}/bin/ww"

# --- Step 7: Fetch standard library ---
echo "Fetching standard library..."
mkdir -p "${WW_HOME}/lib"
ipfs_get_dir "/std/" "${WW_HOME}/lib/std/" || {
  echo "Warning: could not fetch standard library. You can fetch it later with:"
  echo "  ipfs get ${IPFS_BASE}/std/ -o ~/.ww/lib/std/"
}

# --- Step 8: Fetch config template ---
echo "Fetching config template..."
mkdir -p "${WW_HOME}/etc"
ipfs_cat "/etc/config.toml.default" > "${WW_HOME}/etc/config.toml.default" 2>/dev/null || {
  echo "Warning: could not fetch config template."
}

# --- Step 9: PATH warning ---
echo ""
case ":$PATH:" in
  *":${WW_HOME}/bin:"*) ;;
  *)
    echo "Warning: ${WW_HOME}/bin is not in your PATH."
    echo "Add it to your shell config:"
    echo ""
    echo "  # bash (~/.bashrc)"
    echo "  export PATH=\"${WW_HOME}/bin:\$PATH\""
    echo ""
    echo "  # zsh (~/.zshrc)"
    echo "  export PATH=\"${WW_HOME}/bin:\$PATH\""
    echo ""
    echo "  # fish (~/.config/fish/config.fish)"
    echo "  fish_add_path ${WW_HOME}/bin"
    echo ""
    ;;
esac

# --- Step 10: Engagement prompt ---
echo "Installed ww from IPFS. Run 'ww join' to connect to a network."
