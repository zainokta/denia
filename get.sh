#!/bin/sh
# get.sh — one-line installer for Denia (prebuilt, signed binary). Linux + macOS.
#   Windows: use get.ps1  (irm https://raw.githubusercontent.com/zainokta/denia/main/get.ps1 | iex)
#
#   curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/zainokta/denia/main/get.sh | sh
#
# Read-first form (recommended):
#   curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/zainokta/denia/main/get.sh -o get.sh
#   less get.sh && sh get.sh
#
# Linux assets are the full Denia server; macOS assets are the client only
# (server modules are Linux-gated). When run from a terminal you pick the build
# from a menu; when piped without a TTY it installs the auto-detected build.
#
# Trust chain (fail-closed, same as `denia update` / ADR-029):
#   1. verify SHA256SUMS.minisig over SHA256SUMS with the pinned minisign key;
#   2. verify the downloaded binary's SHA256 against the now-trusted SHA256SUMS.
# An attacker who controls the release, a mirror, or transport still cannot
# forge a SHA256SUMS the pinned key accepts.
#
# Env overrides:
#   DENIA_VERSION=vX.Y.Z         install a specific release (default: latest)
#   DENIA_TARGET=<target>        skip the menu, install this build. One of:
#                                x86_64-linux-gnu aarch64-linux-gnu
#                                x86_64-apple-darwin aarch64-apple-darwin
#   DENIA_BIN_DIR=/path          install dir (default: /usr/local/bin)
#   DENIA_SKIP_MINISIGN=1        skip signature check when minisign is absent (NOT recommended)
set -eu

REPO="zainokta/denia"
# Pinned minisign public key (matches key.pub / ADR-029). Verified, not TOFU.
PUBKEY="RWTjef0vJl3g2lcJz4JSOlDB64pmYBRYNHxmShlHtCbbjcm4aMIj+vkP"
BIN_DIR="${DENIA_BIN_DIR:-/usr/local/bin}"

# Auto-detect host -> default target.
case "$(uname -s)" in
  Linux)  os="linux" ;;
  Darwin) os="darwin" ;;
  *) echo "get.sh supports Linux and macOS. For Windows use get.ps1." >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64)  cpu="x86_64" ;;
  aarch64|arm64) cpu="aarch64" ;;
  *) cpu="" ;;
esac
case "${os}-${cpu}" in
  linux-x86_64)   DEFAULT_TARGET="x86_64-linux-gnu" ;;
  linux-aarch64)  DEFAULT_TARGET="aarch64-linux-gnu" ;;
  darwin-x86_64)  DEFAULT_TARGET="x86_64-apple-darwin" ;;
  darwin-aarch64) DEFAULT_TARGET="aarch64-apple-darwin" ;;
  *) DEFAULT_TARGET="" ;;
esac

mark() { [ "$1" = "$DEFAULT_TARGET" ] && printf '  (detected)' || true; }

# Resolve target: explicit env wins; else interactive menu via /dev/tty; else
# the auto-detected default (keeps `curl | sh` working unattended/in CI).
TARGET="${DENIA_TARGET:-}"
if [ -z "$TARGET" ]; then
  if [ -r /dev/tty ]; then
    printf 'Select Denia build:\n' >&2
    printf '  linux = full server  |  macOS = client only (no server)\n' >&2
    printf '  1) linux  x86_64   (server)%s\n'  "$(mark x86_64-linux-gnu)"   >&2
    printf '  2) linux  aarch64  (server)%s\n'  "$(mark aarch64-linux-gnu)"  >&2
    printf '  3) macOS  x86_64   (client only)%s\n'  "$(mark x86_64-apple-darwin)"   >&2
    printf '  4) macOS  arm64    (client only)%s\n'  "$(mark aarch64-apple-darwin)"  >&2
    printf 'Choice [%s]: ' "${DEFAULT_TARGET:-1}" >&2
    read sel < /dev/tty || sel=""
    case "$sel" in
      1) TARGET="x86_64-linux-gnu" ;;
      2) TARGET="aarch64-linux-gnu" ;;
      3) TARGET="x86_64-apple-darwin" ;;
      4) TARGET="aarch64-apple-darwin" ;;
      "") TARGET="$DEFAULT_TARGET" ;;
      *) echo "Invalid choice: $sel" >&2; exit 1 ;;
    esac
  else
    TARGET="$DEFAULT_TARGET"
  fi
fi
[ -n "$TARGET" ] || { echo "Could not determine target; set DENIA_TARGET=." >&2; exit 1; }

ASSET="denia-${TARGET}"

# Resolve version (override with DENIA_VERSION=vX.Y.Z).
TAG="${DENIA_VERSION:-$(curl --proto '=https' --tlsv1.2 -fsSL \
  "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep -m1 '"tag_name"' | cut -d'"' -f4)}"
[ -n "$TAG" ] || { echo "Could not resolve latest release tag." >&2; exit 1; }

BASE="https://github.com/${REPO}/releases/download/${TAG}"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
echo "Downloading ${ASSET} ${TAG}..."
curl --proto '=https' --tlsv1.2 -fsSL "${BASE}/${ASSET}"           -o "$TMP/denia"
curl --proto '=https' --tlsv1.2 -fsSL "${BASE}/SHA256SUMS"         -o "$TMP/SHA256SUMS"
curl --proto '=https' --tlsv1.2 -fsSL "${BASE}/SHA256SUMS.minisig" -o "$TMP/SHA256SUMS.minisig"

# 1) Verify the signature over SHA256SUMS with the pinned key (fail-closed).
if command -v minisign >/dev/null 2>&1; then
  minisign -V -P "$PUBKEY" -m "$TMP/SHA256SUMS" -x "$TMP/SHA256SUMS.minisig"
elif [ "${DENIA_SKIP_MINISIGN:-0}" != "1" ]; then
  echo "minisign not found. Install it (apt install minisign / brew install minisign)" >&2
  echo "or re-run with DENIA_SKIP_MINISIGN=1 (NOT recommended)." >&2
  exit 1
fi

# 2) Verify the binary's checksum against the (now-trusted) SHA256SUMS.
#    macOS ships `shasum`, not `sha256sum`.
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL="$(sha256sum "$TMP/denia" | cut -d' ' -f1)"
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL="$(shasum -a 256 "$TMP/denia" | cut -d' ' -f1)"
else
  echo "No sha256sum/shasum available to verify the download." >&2; exit 1
fi
EXPECT="$(grep " ${ASSET}\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)"
[ -n "$EXPECT" ] || { echo "No checksum for ${ASSET} in SHA256SUMS." >&2; exit 1; }
[ "$EXPECT" = "$ACTUAL" ] || { echo "Checksum mismatch. Aborting." >&2; exit 1; }

# 3) Install (needs root for /usr/local/bin).
SUDO=""; [ "$(id -u)" -ne 0 ] && SUDO="sudo"
$SUDO install -m 0755 "$TMP/denia" "${BIN_DIR}/denia"
echo "Installed denia ${TAG} (${TARGET}) to ${BIN_DIR}/denia"
case "$TARGET" in
  *-linux-gnu) echo "Next: sudo denia setup" ;;
  *) echo "Client installed. Run: denia --help" ;;
esac
