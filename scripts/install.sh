#!/bin/sh
# Wetware installer
# Usage: curl -sSf https://raw.githubusercontent.com/wetware/ww/master/scripts/install.sh | sh
#   or:  curl -sSf ... | sh -s -- --version v0.2.0 --prefix /opt/bin
set -eu

REPO="wetware/ww"
VERSION=""
PREFIX="${HOME}/.local/bin"

# Parse arguments
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --prefix)  PREFIX="$2"; shift 2 ;;
    --help)
      echo "Usage: install.sh [--version VERSION] [--prefix PATH]"
      echo "  --version  Specific release tag (default: latest)"
      echo "  --prefix   Install directory (default: ~/.local/bin)"
      exit 0
      ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# Detect OS
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

# Detect architecture (normalize macOS arm64 -> aarch64)
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH_NAME="x86_64" ;;
  aarch64|arm64) ARCH_NAME="aarch64" ;;
  *)
    echo "Error: unsupported architecture: $ARCH"
    echo "Supported: x86_64, aarch64"
    exit 1
    ;;
esac

ARTIFACT="ww-${OS_NAME}-${ARCH_NAME}.tar.gz"

# Resolve version
if [ -z "$VERSION" ]; then
  echo "Fetching latest release..."
  VERSION=$(curl -sSf "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | cut -d'"' -f4)
  if [ -z "$VERSION" ]; then
    echo "Error: could not determine latest release version"
    exit 1
  fi
fi

echo "Installing wetware ${VERSION} (${OS_NAME}/${ARCH_NAME})..."

BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

# Download binary and checksums
echo "Downloading ${ARTIFACT}..."
curl -fSL "${BASE_URL}/${ARTIFACT}" -o "${TMPDIR}/${ARTIFACT}" || {
  echo "Error: download failed: ${BASE_URL}/${ARTIFACT}"
  exit 1
}

echo "Downloading checksums..."
curl -fSL "${BASE_URL}/CHECKSUMS.txt" -o "${TMPDIR}/CHECKSUMS.txt" || {
  echo "Warning: could not download checksums. Skipping verification."
}

# Verify checksum
if [ -f "${TMPDIR}/CHECKSUMS.txt" ]; then
  echo "Verifying checksum..."
  if command -v b3sum >/dev/null 2>&1; then
    # Verify via blake3 multihash
    EXPECTED=$(grep "^1e20" "${TMPDIR}/CHECKSUMS.txt" | grep "$ARTIFACT" | head -1 | awk '{print $1}' | cut -c5-)
    ACTUAL=$(b3sum --no-names "${TMPDIR}/${ARTIFACT}")
    ALGO="blake3"
  else
    # Fall back to sha256 (raw section at bottom of CHECKSUMS.txt)
    EXPECTED=$(grep -v "^#" "${TMPDIR}/CHECKSUMS.txt" | grep -v "^1" | grep "$ARTIFACT" | head -1 | awk '{print $1}')
    ACTUAL=$(sha256sum "${TMPDIR}/${ARTIFACT}" 2>/dev/null || shasum -a 256 "${TMPDIR}/${ARTIFACT}" | awk '{print $1}')
    # sha256sum output includes filename, extract just hash
    ACTUAL=$(echo "$ACTUAL" | awk '{print $1}')
    ALGO="sha256"
  fi

  if [ -n "$EXPECTED" ] && [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "Error: checksum mismatch (${ALGO})"
    echo "  expected: ${EXPECTED}"
    echo "  got:      ${ACTUAL}"
    rm -f "${TMPDIR}/${ARTIFACT}"
    exit 1
  elif [ -n "$EXPECTED" ]; then
    echo "Checksum OK (${ALGO})"
  else
    echo "Warning: could not find checksum for ${ARTIFACT} in CHECKSUMS.txt"
  fi
fi

# Install
mkdir -p "$PREFIX"
tar xzf "${TMPDIR}/${ARTIFACT}" -C "$PREFIX"
chmod +x "${PREFIX}/ww"

echo ""
echo "Installed ww ${VERSION} to ${PREFIX}/ww"

# Check PATH
case ":$PATH:" in
  *":${PREFIX}:"*) ;;
  *)
    echo ""
    echo "Warning: ${PREFIX} is not in your PATH."
    echo "Add it with:"
    echo "  export PATH=\"${PREFIX}:\$PATH\""
    ;;
esac

echo ""
echo "Run 'ww --help' to get started."
