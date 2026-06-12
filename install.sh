#!/usr/bin/env sh
# Innate installer — downloads the pre-built binary from GitHub releases.
#
# Usage:
#   curl -fsSL https://innate.mengkai.ren/ | sh
#   curl -fsSL https://innate.mengkai.ren/ | INNATE_VERSION=0.1.7 sh
#
# Environment variables:
#   INNATE_VERSION   — version to install (default: latest release)
#   INNATE_DIR       — install directory  (default: ~/.local/bin)
#   NO_INNATE_SETUP  — set to 1 to skip running `innate install` after download
#   NO_COLOR         — set to disable color output

set -eu

INNATE_REPO="vima-tech/Innate"
INNATE_DIR="${INNATE_DIR:-$HOME/.local/bin}"

# ── Color helpers ─────────────────────────────────────────────────────────────

_tty() { [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; }
_green() { _tty && printf '\033[32m%s\033[0m' "$1" || printf '%s' "$1"; }
_gray()  { _tty && printf '\033[90m%s\033[0m' "$1" || printf '%s' "$1"; }
_bold()  { _tty && printf '\033[1m%s\033[0m'  "$1" || printf '%s' "$1"; }
_red()   { _tty && printf '\033[31m%s\033[0m' "$1" || printf '%s' "$1"; }
_cyan()  { _tty && printf '\033[36m%s\033[0m' "$1" || printf '%s' "$1"; }

_step()  { printf '%s  %s\n' "$(_green '◆')" "$1"; }
_info()  { printf '%s  %s\n' "$(_gray  '│')" "$1"; }
_sep()   { printf '%s\n'     "$(_gray  '│')"; }
_error() { printf '%s  %s\n' "$(_red   '✗')" "$1" >&2; }
_warn()  { printf '%s  %s\n' "$(_cyan  '△')" "$1"; }

# ── Platform detection ────────────────────────────────────────────────────────

detect_target() {
  _OS="$(uname -s)"
  _ARCH="$(uname -m)"

  case "$_OS" in
    Linux)
      case "$_ARCH" in
        x86_64)          _TARGET="x86_64-unknown-linux-musl" ;;
        aarch64|arm64)   _TARGET="aarch64-unknown-linux-musl" ;;
        armv7l)          _TARGET="armv7-unknown-linux-musleabihf" ;;
        *) _error "Unsupported Linux arch: $_ARCH"; exit 1 ;;
      esac
      _EXT=""
      ;;
    Darwin)
      case "$_ARCH" in
        x86_64)          _TARGET="x86_64-apple-darwin" ;;
        arm64)           _TARGET="aarch64-apple-darwin" ;;
        *) _error "Unsupported macOS arch: $_ARCH"; exit 1 ;;
      esac
      _EXT=""
      ;;
    MINGW*|MSYS*|CYGWIN*)
      case "$_ARCH" in
        x86_64)          _TARGET="x86_64-pc-windows-msvc" ;;
        *) _error "Unsupported Windows arch: $_ARCH"; exit 1 ;;
      esac
      _EXT=".exe"
      ;;
    *)
      _error "Unsupported OS: $_OS"
      _info  "Install manually: https://github.com/${INNATE_REPO}/releases"
      exit 1
      ;;
  esac
}

# ── Latest version resolver ───────────────────────────────────────────────────

resolve_version() {
  if [ -n "${INNATE_VERSION:-}" ]; then
    return
  fi
  _info "Resolving latest release..."
  _API="https://api.github.com/repos/${INNATE_REPO}/releases/latest"
  if command -v curl > /dev/null 2>&1; then
    INNATE_VERSION="$(curl -fsSL "$_API" | grep '"tag_name"' | sed 's/.*"v\([^"]*\)".*/\1/')"
  elif command -v wget > /dev/null 2>&1; then
    INNATE_VERSION="$(wget -qO- "$_API" | grep '"tag_name"' | sed 's/.*"v\([^"]*\)".*/\1/')"
  else
    _error "curl or wget required"
    exit 1
  fi
  if [ -z "$INNATE_VERSION" ]; then
    _error "Could not resolve latest version. Set INNATE_VERSION manually."
    exit 1
  fi
}

# ── Download helper ───────────────────────────────────────────────────────────

_download() {
  _URL="$1"
  _DEST="$2"
  if command -v curl > /dev/null 2>&1; then
    curl -fsSL --progress-bar "$_URL" -o "$_DEST"
  else
    wget -q --show-progress "$_URL" -O "$_DEST" 2>&1 | tail -1 || \
    wget -q "$_URL" -O "$_DEST"
  fi
}

# ── Checksum verification ─────────────────────────────────────────────────────

verify_checksum() {
  _FILE="$1"
  _SUM_URL="$2"
  _TMP_SUM="$(mktemp)"
  if _download "$_SUM_URL" "$_TMP_SUM" 2>/dev/null; then
    _EXPECTED="$(awk '{print $1}' "$_TMP_SUM")"
    if command -v sha256sum > /dev/null 2>&1; then
      _ACTUAL="$(sha256sum "$_FILE" | awk '{print $1}')"
    elif command -v shasum > /dev/null 2>&1; then
      _ACTUAL="$(shasum -a 256 "$_FILE" | awk '{print $1}')"
    else
      rm -f "$_TMP_SUM"
      _warn "sha256sum/shasum not found — skipping checksum verification"
      return
    fi
    if [ "$_EXPECTED" != "$_ACTUAL" ]; then
      rm -f "$_TMP_SUM" "$_FILE"
      _error "Checksum mismatch!"
      _error "  Expected: $_EXPECTED"
      _error "  Got:      $_ACTUAL"
      exit 1
    fi
    _step "Checksum verified $(_gray "(sha256)")"
  else
    _warn "Checksum file not found — skipping verification"
  fi
  rm -f "$_TMP_SUM"
}

# ── Main ──────────────────────────────────────────────────────────────────────

main() {
  printf '%s  %s\n' "$(_green '┌')" "$(_bold 'Innate installer')"
  _sep

  detect_target
  resolve_version

  _info "Version : $(_bold "v${INNATE_VERSION}")"
  _info "Platform: $_TARGET"
  _info "Dest    : $(_bold "${INNATE_DIR}/innate${_EXT}")"
  _sep

  _BASE="https://github.com/${INNATE_REPO}/releases/download/v${INNATE_VERSION}"
  _BINARY_URL="${_BASE}/innate-${_TARGET}${_EXT}"
  _SUM_URL="${_BASE}/innate-${_TARGET}${_EXT}.sha256"

  mkdir -p "$INNATE_DIR"
  _TMP="$(mktemp)"

  _step "Downloading innate v${INNATE_VERSION}..."
  _download "$_BINARY_URL" "$_TMP"

  verify_checksum "$_TMP" "$_SUM_URL"

  chmod +x "$_TMP"
  mv "$_TMP" "${INNATE_DIR}/innate${_EXT}"

  _step "Installed $(_bold "${INNATE_DIR}/innate${_EXT}")"

  # PATH advisory
  case ":${PATH}:" in
    *":${INNATE_DIR}:"*) ;;
    *)
      _sep
      _warn "$(_bold "${INNATE_DIR}") is not in your PATH"
      _info "Add to your shell profile ($(_bold '~/.bashrc') / $(_bold '~/.zshrc')):"
      _info ""
      _info "  $(_bold 'export PATH=\"\$HOME/.local/bin:\$PATH\"')"
      ;;
  esac

  # Run innate install to configure agents
  if [ -z "${NO_INNATE_SETUP:-}" ]; then
    _sep
    _step "Running $(_bold 'innate install') to configure agents..."
    _sep
    "${INNATE_DIR}/innate${_EXT}" install || true
  fi

  _sep
  printf '%s  %s\n' "$(_green '└')" "$(_bold 'Done!')"
}

main "$@"
