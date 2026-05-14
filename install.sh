#!/bin/sh
# XClaudeUsage installer (POSIX shell — Linux + macOS).
#
# Downloads the latest pre-built xclaudeusage binary for this host's OS/arch
# from GitHub Releases, drops it in ~/.claude/bin, and execs `xclaudeusage
# install` so the binary itself handles settings.json + interactive Turso
# config. Re-runs are safe: existing entries are updated in place.
#
#   curl -fsSL https://raw.githubusercontent.com/SrDarf/XClaudeUsage/HighPerformanceXClaudeUsage/install.sh | sh
#
# Set XCLAUDEUSAGE_VERSION to pin a specific tag (default: latest).

set -eu

REPO="SrDarf/XClaudeUsage"
BIN_DIR="$HOME/.claude/bin"
BIN_PATH="$BIN_DIR/xclaudeusage"
VERSION="${XCLAUDEUSAGE_VERSION:-latest}"

info() { printf '[install] %s\n' "$1"; }
fail() { printf '[install] ERROR: %s\n' "$1" >&2; exit 1; }

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}
require_cmd uname
require_cmd mkdir
require_cmd chmod

# Prefer curl, fall back to wget.
if command -v curl >/dev/null 2>&1; then
  DOWNLOAD='curl -fL --proto =https --tlsv1.2 -sS -o'
elif command -v wget >/dev/null 2>&1; then
  DOWNLOAD='wget -qO'
else
  fail "neither curl nor wget is available"
fi

OS=$(uname -s)
ARCH=$(uname -m)
case "$OS" in
  Linux)  os_part="unknown-linux-gnu"; archive_ext="tar.gz"; extract='tar -xzf' ;;
  Darwin) os_part="apple-darwin";      archive_ext="tar.gz"; extract='tar -xzf' ;;
  *)      fail "unsupported OS: $OS (run install.ps1 on Windows)" ;;
esac
case "$ARCH" in
  x86_64|amd64) arch_part="x86_64" ;;
  arm64|aarch64) arch_part="aarch64" ;;
  *) fail "unsupported architecture: $ARCH" ;;
esac

# Intel Macs are not published as pre-built artifacts (GitHub's macos-13
# runners are unreliable). Point those users at `cargo install` instead.
if [ "$os_part" = "apple-darwin" ] && [ "$arch_part" = "x86_64" ]; then
  fail "Intel Macs are not pre-built. Install with: cargo install --git https://github.com/SrDarf/XClaudeUsage --branch HighPerformanceXClaudeUsage --locked"
fi

TARGET="${arch_part}-${os_part}"
ARCHIVE="xclaudeusage-${TARGET}.${archive_ext}"

if [ "$VERSION" = "latest" ]; then
  BASE="https://github.com/${REPO}/releases/latest/download"
else
  BASE="https://github.com/${REPO}/releases/download/${VERSION}"
fi

TMP=$(mktemp -d 2>/dev/null || mktemp -d -t xclaudeusage)
trap 'rm -rf "$TMP"' EXIT INT TERM

info "downloading ${ARCHIVE} from ${BASE}"
$DOWNLOAD "$TMP/$ARCHIVE" "$BASE/$ARCHIVE" || fail "download failed"

# Verify SHA-256 if a manifest is published alongside the binary.
if command -v sha256sum >/dev/null 2>&1; then
  SHASUM=sha256sum
elif command -v shasum >/dev/null 2>&1; then
  SHASUM="shasum -a 256"
else
  SHASUM=""
fi
if [ -n "$SHASUM" ]; then
  if $DOWNLOAD "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" 2>/dev/null; then
    expected=$(grep "  ${ARCHIVE}\$" "$TMP/SHA256SUMS" | awk '{print $1}')
    actual=$($SHASUM "$TMP/$ARCHIVE" | awk '{print $1}')
    if [ -n "$expected" ] && [ "$expected" != "$actual" ]; then
      fail "checksum mismatch for $ARCHIVE (expected $expected, got $actual)"
    fi
    [ -n "$expected" ] && info "checksum verified"
  fi
fi

info "extracting"
(cd "$TMP" && $extract "$ARCHIVE")
[ -f "$TMP/xclaudeusage" ] || fail "archive did not contain a 'xclaudeusage' binary"

mkdir -p "$BIN_DIR"
mv "$TMP/xclaudeusage" "$BIN_PATH"
chmod +x "$BIN_PATH"
info "installed $BIN_PATH"

# Hand off to the binary itself for interactive configuration.
exec "$BIN_PATH" install
