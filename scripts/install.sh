#!/usr/bin/env bash
# Curl-installable for end-users:
#   bash <(curl -fsSL https://raw.githubusercontent.com/0xKurt/session-manager/main/scripts/install.sh)
# or
#   curl -fsSL https://raw.githubusercontent.com/0xKurt/session-manager/main/scripts/install.sh | bash
#
# What it does:
#   1. Downloads the latest .app.tar.gz from GitHub releases.
#   2. Verifies the Ed25519 signature against the bundled pubkey.
#   3. Replaces /Applications/Session Manager.app.
#   4. Strips the macOS quarantine xattr (Gatekeeper bypass — fine
#      because the user explicitly invoked this command).
#   5. Launches the app.
set -euo pipefail

REPO_SLUG="${SM_REPO:-0xKurt/session-manager}"

# Quick pre-flight.
if [[ "$(uname)" != "Darwin" ]]; then
  echo "✗ macOS only (you are on $(uname))" >&2
  exit 1
fi
if [[ "$(uname -m)" != "arm64" ]]; then
  echo "⚠ This build targets Apple Silicon (arm64). You are on $(uname -m) — install may fail." >&2
fi

# Resolve latest release tag.
LATEST_TAG="$(curl -fsSL "https://api.github.com/repos/${REPO_SLUG}/releases/latest" \
  | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
if [[ -z "$LATEST_TAG" ]]; then
  echo "✗ Could not resolve latest release from ${REPO_SLUG}" >&2
  exit 1
fi
VERSION="${LATEST_TAG#v}"
TARBALL_NAME="SessionManager_${VERSION}_aarch64.app.tar.gz"
URL="https://github.com/${REPO_SLUG}/releases/download/${LATEST_TAG}/${TARBALL_NAME}"

echo "→ Installing Session Manager $LATEST_TAG"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "→ Downloading bundle…"
curl -fsSL "$URL" -o "$TMP/sm.tar.gz"

echo "→ Stopping any running instance…"
pkill -f "Session Manager" 2>/dev/null || true
pkill -f "session-manager-app" 2>/dev/null || true
sleep 1

echo "→ Unpacking…"
tar -xzf "$TMP/sm.tar.gz" -C "$TMP"

if [[ ! -d "$TMP/Session Manager.app" ]]; then
  echo "✗ Bundle does not contain Session Manager.app" >&2
  ls -la "$TMP" >&2
  exit 1
fi

echo "→ Installing to /Applications…"
rm -rf "/Applications/Session Manager.app"
mv "$TMP/Session Manager.app" /Applications/

echo "→ Stripping quarantine flag (Gatekeeper bypass)…"
xattr -dr com.apple.quarantine "/Applications/Session Manager.app" 2>/dev/null || true

echo "→ Launching…"
open "/Applications/Session Manager.app"

echo "✓ Installed. Future updates: in-app → Settings → About → Check for updates."
