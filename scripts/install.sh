#!/bin/sh
# Wetware installer (IPFS-first)
# Usage: curl -sSf https://raw.githubusercontent.com/wetware/ww/master/scripts/install.sh | sh
#   or:  curl -sSf ... | sh -s -- --version <CID>
set -eu

IPNS_NAME="/ipns/releases.wetware.run"
IPNS_TIMEOUT=60
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
if [ -n "$VERSION_CID" ]; then
  # Version pinning: use CID directly, skip IPNS resolution
  IPFS_BASE="/ipfs/${VERSION_CID}"
  echo "Installing pinned release: ${VERSION_CID}"
else
  echo "Resolving latest release via IPNS (may take up to ${IPNS_TIMEOUT}s on first run)..."

  # macOS doesn't have timeout(1), use a background process
  RESOLVED_CID=""
  ipfs name resolve "$IPNS_NAME" > /tmp/ww-ipns-resolve.$$ 2>/dev/null &
  RESOLVE_PID=$!

  i=0
  while [ $i -lt $IPNS_TIMEOUT ]; do
    if ! kill -0 "$RESOLVE_PID" 2>/dev/null; then
      # Process finished
      RESOLVED_CID=$(cat /tmp/ww-ipns-resolve.$$ 2>/dev/null || true)
      break
    fi
    i=$((i + 1))
    sleep 1
  done

  # Kill if still running
  kill "$RESOLVE_PID" 2>/dev/null || true
  rm -f /tmp/ww-ipns-resolve.$$

  if [ -n "$RESOLVED_CID" ]; then
    IPFS_BASE="$RESOLVED_CID"
    echo "Resolved: ${IPFS_BASE}"
  else
    echo ""
    echo "Error: IPNS resolution failed."
    echo ""
    echo "Your IPFS node could not resolve releases.wetware.run."
    echo "This usually means the IPNS record has not propagated to your"
    echo "region of the DHT yet. Try again in a few minutes."
    echo ""
    echo "Alternatively, install a specific version by CID:"
    echo "  curl -sSf .../install.sh | sh -s -- --version <CID>"
    echo ""
    echo "Find release CIDs at: https://github.com/wetware/ww/releases"
    exit 1
  fi
fi

TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# --- Step 4: Fetch binary ---
BIN_PATH="/bin/${OS_NAME}/${ARCH_NAME}/ww"
echo "Fetching binary (${OS_NAME}/${ARCH_NAME})..."
ipfs cat "${IPFS_BASE}${BIN_PATH}" > "${TMPDIR}/ww" 2>/dev/null || {
  echo ""
  echo "Error: could not fetch binary from IPFS."
  echo "The release may not include a binary for ${OS_NAME}/${ARCH_NAME}."
  echo ""
  echo "Fallback: download manually from GitHub Releases:"
  echo "  https://github.com/wetware/ww/releases"
  exit 1
}

# --- Step 5: Fetch and verify checksums ---
echo "Fetching checksums..."
ipfs cat "${IPFS_BASE}/CHECKSUMS.txt" > "${TMPDIR}/CHECKSUMS.txt" 2>/dev/null || {
  echo "Warning: could not fetch checksums. Skipping verification."
}

if [ -f "${TMPDIR}/CHECKSUMS.txt" ] && [ -s "${TMPDIR}/CHECKSUMS.txt" ]; then
  echo "Verifying checksum..."

  EXPECTED=""
  ACTUAL=""
  ALGO=""

  # Prefer BLAKE3 if b3sum is available and CHECKSUMS.txt has a blake3 section
  if command -v b3sum >/dev/null 2>&1 && grep -q "^# blake3" "${TMPDIR}/CHECKSUMS.txt"; then
    EXPECTED=$(sed -n '/^# blake3/,/^$/p' "${TMPDIR}/CHECKSUMS.txt" | grep "${BIN_PATH}" | head -1 | awk '{print $1}')
    if [ -n "$EXPECTED" ]; then
      ACTUAL=$(b3sum --no-names "${TMPDIR}/ww")
      ALGO="blake3"
    fi
  fi

  # Fall back to SHA-256 (always available on macOS and Linux)
  if [ -z "$ALGO" ] && grep -q "^# sha256" "${TMPDIR}/CHECKSUMS.txt"; then
    EXPECTED=$(sed -n '/^# sha256/,/^$/p' "${TMPDIR}/CHECKSUMS.txt" | grep "${BIN_PATH}" | head -1 | awk '{print $1}')
    if [ -n "$EXPECTED" ]; then
      ACTUAL=$(sha256sum "${TMPDIR}/ww" 2>/dev/null || shasum -a 256 "${TMPDIR}/ww")
      ACTUAL=$(echo "$ACTUAL" | awk '{print $1}')
      ALGO="sha256"
    fi
  fi

  if [ -n "$ALGO" ] && [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "Error: checksum mismatch (${ALGO})"
    echo "  expected: ${EXPECTED}"
    echo "  got:      ${ACTUAL}"
    echo ""
    echo "The download may be corrupted. Try again, or download manually:"
    echo "  https://github.com/wetware/ww/releases"
    rm -f "${TMPDIR}/ww"
    exit 1
  elif [ -n "$ALGO" ]; then
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
ipfs get "${IPFS_BASE}/std/" -o "${WW_HOME}/lib/std/" 2>/dev/null || {
  echo "Warning: could not fetch standard library. You can fetch it later with:"
  echo "  ipfs get ${IPFS_BASE}/std/ -o ~/.ww/lib/std/"
}

# --- Step 8: Fetch config template ---
echo "Fetching config template..."
mkdir -p "${WW_HOME}/etc"
ipfs cat "${IPFS_BASE}/etc/config.toml.default" > "${WW_HOME}/etc/config.toml.default" 2>/dev/null || {
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
