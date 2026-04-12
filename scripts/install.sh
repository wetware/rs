#!/bin/sh
# Wetware installer (IPFS-first)
# Usage: curl -sSf https://raw.githubusercontent.com/wetware/ww/master/scripts/install.sh | sh
#   or:  curl -sSf ... | sh -s -- --version <CID>
set -eu

IPNS_NAME="/ipns/releases.wetware.run"
IPNS_TIMEOUT=60
VERSION_CID=""
WW_HOME="${HOME}/.ww"

# --- TTY detection & output helpers ---

IS_TTY=false
if [ -t 1 ]; then
  IS_TTY=true
fi

SPINNER_PID=""

_spinner_frame() {
  case $(($1 % 10)) in
    0) printf '\342\240\213' ;;  # ⠋
    1) printf '\342\240\231' ;;  # ⠙
    2) printf '\342\240\271' ;;  # ⠹
    3) printf '\342\240\270' ;;  # ⠸
    4) printf '\342\240\274' ;;  # ⠼
    5) printf '\342\240\264' ;;  # ⠴
    6) printf '\342\240\246' ;;  # ⠦
    7) printf '\342\240\247' ;;  # ⠧
    8) printf '\342\240\207' ;;  # ⠇
    9) printf '\342\240\217' ;;  # ⠏
  esac
}

# Start a spinner with a message.  Usage: spin "Doing thing..."
spin() {
  if $IS_TTY; then
    _spin_msg="$1"
    (
      i=0
      while true; do
        frame=$(_spinner_frame $i)
        printf '\r  %s %s' "$frame" "$_spin_msg" >&2
        i=$((i + 1))
        sleep 0.1
      done
    ) &
    SPINNER_PID=$!
  else
    printf '%s' "$1" >&2
  fi
}

# Stop spinner and show success.  Usage: spin_ok "Done thing"
spin_ok() {
  if $IS_TTY; then
    if [ -n "$SPINNER_PID" ]; then
      kill "$SPINNER_PID" 2>/dev/null || true
      wait "$SPINNER_PID" 2>/dev/null || true
      SPINNER_PID=""
    fi
    printf '\r\033[K  \342\234\223 %s\n' "$1" >&2
  else
    printf ' ok\n' >&2
  fi
}

# Stop spinner and show failure.  Usage: spin_fail "Failed thing"
spin_fail() {
  if $IS_TTY; then
    if [ -n "$SPINNER_PID" ]; then
      kill "$SPINNER_PID" 2>/dev/null || true
      wait "$SPINNER_PID" 2>/dev/null || true
      SPINNER_PID=""
    fi
    printf '\r\033[K  \342\234\227 %s\n' "$1" >&2
  else
    printf ' FAILED\n' >&2
  fi
}

# Dim warning (single line, non-fatal)
warn() {
  if $IS_TTY; then
    printf '  \033[2m%s\033[0m\n' "$1" >&2
  else
    printf '  %s\n' "$1" >&2
  fi
}

# Fatal error
die() {
  spin_fail "$1"
  shift
  for line in "$@"; do
    printf '  %s\n' "$line" >&2
  done
  exit 1
}

# Clean up spinner on exit
cleanup() {
  if [ -n "$SPINNER_PID" ]; then
    kill "$SPINNER_PID" 2>/dev/null || true
    wait "$SPINNER_PID" 2>/dev/null || true
  fi
  rm -rf "${TMPDIR:-}"
}
trap cleanup EXIT

# --- Parse arguments ---
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

# --- Detect platform ---
OS="$(uname -s)"
case "$OS" in
  Linux)  OS_NAME="linux" ;;
  Darwin) OS_NAME="macos" ;;
  *) die "Unsupported OS: $OS" "Supported: Linux, macOS" ;;
esac

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)  ARCH_NAME="x86_64" ;;
  aarch64|arm64) ARCH_NAME="aarch64" ;;
  *) die "Unsupported architecture: $ARCH" "Supported: x86_64, aarch64" ;;
esac

# --- Check IPFS ---
if ! command -v ipfs >/dev/null 2>&1; then
  die "IPFS not found" \
    "Wetware requires IPFS.  Install Kubo:" \
    "  https://docs.ipfs.tech/install/"
fi

if ! ipfs id >/dev/null 2>&1; then
  die "IPFS daemon not running" \
    "Start it with: ipfs daemon &" \
    "Install Kubo:  https://docs.ipfs.tech/install/"
fi

# --- Header ---
if $IS_TTY; then
  printf '\n\342\232\227\357\270\217  Installing wetware...\n'
else
  printf 'Installing wetware...\n'
fi

# --- Resolve release ---
if [ -n "$VERSION_CID" ]; then
  IPFS_BASE="/ipfs/${VERSION_CID}"
else
  spin "Resolving latest release..."

  RESOLVED_CID=""
  ipfs name resolve "$IPNS_NAME" > /tmp/ww-ipns-resolve.$$ 2>/dev/null &
  RESOLVE_PID=$!

  i=0
  while [ $i -lt $IPNS_TIMEOUT ]; do
    if ! kill -0 "$RESOLVE_PID" 2>/dev/null; then
      RESOLVED_CID=$(cat /tmp/ww-ipns-resolve.$$ 2>/dev/null || true)
      break
    fi
    i=$((i + 1))
    sleep 1
  done

  kill "$RESOLVE_PID" 2>/dev/null || true
  rm -f /tmp/ww-ipns-resolve.$$

  if [ -z "$RESOLVED_CID" ]; then
    die "IPNS resolution failed" \
      "Your IPFS node could not resolve releases.wetware.run." \
      "Try again in a few minutes, or install by CID:" \
      "  curl -sSf .../install.sh | sh -s -- --version <CID>" \
      "Release CIDs: https://github.com/wetware/ww/releases"
  fi

  IPFS_BASE="$RESOLVED_CID"
  spin_ok "Resolved latest release"
fi

TMPDIR=$(mktemp -d)

# --- Fetch binary ---
BIN_PATH="/bin/${OS_NAME}/${ARCH_NAME}/ww"
spin "Fetching binary (${OS_NAME}/${ARCH_NAME})..."

if ! ipfs cat "${IPFS_BASE}${BIN_PATH}" > "${TMPDIR}/ww" 2>/dev/null; then
  die "Could not fetch binary" \
    "No binary for ${OS_NAME}/${ARCH_NAME} in this release." \
    "Download manually: https://github.com/wetware/ww/releases"
fi

spin_ok "Fetched binary (${OS_NAME}/${ARCH_NAME})"

# --- Verify checksum ---
CHECKSUM_ALGO=""
ipfs cat "${IPFS_BASE}/CHECKSUMS.txt" > "${TMPDIR}/CHECKSUMS.txt" 2>/dev/null || true

if [ -f "${TMPDIR}/CHECKSUMS.txt" ] && [ -s "${TMPDIR}/CHECKSUMS.txt" ]; then
  if command -v b3sum >/dev/null 2>&1; then
    EXPECTED=$(grep "${BIN_PATH}" "${TMPDIR}/CHECKSUMS.txt" | grep -v "^#" | head -1 | awk '{print $1}')
    ACTUAL=$(b3sum --no-names "${TMPDIR}/ww")
    CHECKSUM_ALGO="blake3"
  else
    EXPECTED=$(sed -n '/^# sha256/,$ p' "${TMPDIR}/CHECKSUMS.txt" | grep "${BIN_PATH}" | head -1 | awk '{print $1}')
    ACTUAL=$(sha256sum "${TMPDIR}/ww" 2>/dev/null || shasum -a 256 "${TMPDIR}/ww")
    ACTUAL=$(echo "$ACTUAL" | awk '{print $1}')
    CHECKSUM_ALGO="sha256"
  fi

  if [ -n "$EXPECTED" ] && [ "$EXPECTED" != "$ACTUAL" ]; then
    die "Checksum mismatch (${CHECKSUM_ALGO})" \
      "expected: ${EXPECTED}" \
      "got:      ${ACTUAL}" \
      "Download may be corrupted.  Try again or download manually:" \
      "  https://github.com/wetware/ww/releases"
  elif [ -n "$EXPECTED" ]; then
    if $IS_TTY; then
      printf '  \342\234\223 Checksum OK (%s)\n' "$CHECKSUM_ALGO" >&2
    else
      printf 'Checksum OK (%s)\n' "$CHECKSUM_ALGO" >&2
    fi
  else
    warn "Checksum entry not found for ${BIN_PATH}, skipping verification"
  fi
else
  warn "Checksums not available, skipping verification"
fi

# --- Install binary ---
mkdir -p "${WW_HOME}/bin"
mv "${TMPDIR}/ww" "${WW_HOME}/bin/ww"
chmod +x "${WW_HOME}/bin/ww"

# --- Fetch standard library ---
spin "Fetching standard library..."

if ipfs get "${IPFS_BASE}/std/" -o "${WW_HOME}/lib/std/" >/dev/null 2>&1; then
  spin_ok "Fetched standard library"
else
  spin_fail "Standard library not available"
  warn "Fetch it later: ipfs get <release-cid>/std/ -o ~/.ww/lib/std/"
fi

# --- Fetch config template (silent) ---
mkdir -p "${WW_HOME}/etc"
ipfs cat "${IPFS_BASE}/etc/config.toml.default" > "${WW_HOME}/etc/config.toml.default" 2>/dev/null || true

# --- Detect installed version ---
INSTALLED_VERSION=""
if "${WW_HOME}/bin/ww" --version >/dev/null 2>&1; then
  INSTALLED_VERSION=$("${WW_HOME}/bin/ww" --version 2>/dev/null | awk '{print $NF}')
fi

VERSION_DISPLAY="${INSTALLED_VERSION:-unknown}"

# --- Summary ---
printf '\n'
if $IS_TTY; then
  printf '\342\232\227\357\270\217  wetware installed successfully\n'
else
  printf 'wetware installed successfully\n'
fi
printf '\n'
printf '  ww v%s (%s/%s)\n' "$VERSION_DISPLAY" "$OS_NAME" "$ARCH_NAME"
printf '  %s/bin/ww\n' "$WW_HOME"
printf '\n'
if $IS_TTY; then
  printf '  AI agents:    ipfs cat /ipns/releases.wetware.run/.agents/prompt.md\n'
else
  printf '  AI agents: ipfs cat /ipns/releases.wetware.run/.agents/prompt.md\n'
fi
printf '\n'
printf '  Get started:  ww --help\n'
printf '  Join network: ww perform install\n'

# --- PATH warning (shell-specific) ---
case ":${PATH}:" in
  *":${WW_HOME}/bin:"*) ;;
  *)
    printf '\n'
    SHELL_NAME="${SHELL:-}"
    case "$SHELL_NAME" in
      */fish)
        warn "${WW_HOME}/bin is not in your PATH.  Add it:"
        printf '  fish_add_path %s/bin\n' "$WW_HOME"
        ;;
      */zsh)
        warn "${WW_HOME}/bin is not in your PATH.  Add to ~/.zshrc:"
        printf '  export PATH="%s/bin:$PATH"\n' "$WW_HOME"
        ;;
      *)
        warn "${WW_HOME}/bin is not in your PATH.  Add to ~/.bashrc:"
        printf '  export PATH="%s/bin:$PATH"\n' "$WW_HOME"
        ;;
    esac
    ;;
esac

printf '\n'
