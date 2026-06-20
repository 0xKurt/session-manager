#!/usr/bin/env bash
# Local release pipeline.
#
# 1. Reads the version from tauri.conf.json (caller should bump it first).
# 2. Builds the macOS app bundle (release profile).
# 3. Packs the .app into a .tar.gz the updater can consume.
# 4. Signs the tarball with the local Ed25519 private key.
# 5. Emits latest.json with version + URL + signature.
# 6. Creates a GitHub release and uploads .tar.gz + .sig + latest.json.
#
# Requires:
#   - gh CLI (authenticated)
#   - jq
#   - ~/.config/session-manager/updater.key (the Ed25519 private key,
#     generated once via `cargo tauri signer generate`)
#
# Usage:  ./scripts/release.sh             # release at current tauri.conf.json version
#         ./scripts/release.sh --notes "Fixed X" "Added Y"
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.config/session-manager/updater.key}"
REPO_SLUG="${SM_REPO:-0xKurt/session-manager}"

if [[ ! -f "$KEY_PATH" ]]; then
  echo "✗ Signing key missing at $KEY_PATH" >&2
  echo "  Generate one: cargo tauri signer generate -w \"$KEY_PATH\" --ci -p ''" >&2
  exit 1
fi
command -v gh >/dev/null || { echo "✗ gh CLI required" >&2; exit 1; }
command -v jq >/dev/null || { echo "✗ jq required (brew install jq)" >&2; exit 1; }

VERSION="$(jq -r .version src-tauri/tauri.conf.json)"
TAG="v${VERSION}"
echo "→ Releasing $TAG"

# Don't clobber an existing release silently — caller bumped the version
# or this is a re-run we should refuse.
if gh release view "$TAG" --repo "$REPO_SLUG" >/dev/null 2>&1; then
  echo "✗ Release $TAG already exists on $REPO_SLUG. Bump the version in src-tauri/tauri.conf.json first." >&2
  exit 1
fi

echo "→ Building release bundle…"
export TAURI_SIGNING_PRIVATE_KEY_PATH="$KEY_PATH"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
# --bundles app: skip .dmg (currently broken under macOS 26 + tauri 2.11).
cargo tauri build --bundles app

APP_PATH="$REPO_ROOT/target/release/bundle/macos/Session Manager.app"
if [[ ! -d "$APP_PATH" ]]; then
  echo "✗ Bundle not found at $APP_PATH" >&2
  exit 1
fi

OUT_DIR="$REPO_ROOT/target/release/release-assets"
rm -rf "$OUT_DIR"; mkdir -p "$OUT_DIR"

TARBALL="$OUT_DIR/SessionManager_${VERSION}_aarch64.app.tar.gz"
SIG_FILE="${TARBALL}.sig"
LATEST_JSON="$OUT_DIR/latest.json"

echo "→ Packing .app → .tar.gz"
# Tauri's updater expects the inner entry to be the .app directory; tar
# from the parent dir so the archive root contains "Session Manager.app".
# `COPYFILE_DISABLE=1` strips the macOS AppleDouble metadata
# (`._Session Manager.app/...`) that BSD tar otherwise embeds alongside
# every file with extended attributes. Without this, tauri-plugin-updater
# tries to unpack the dot-underscore sidecar as if it were the app bundle
# and fails with "failed to unpack `._Session Manager.app`".
( cd "$REPO_ROOT/target/release/bundle/macos" && COPYFILE_DISABLE=1 tar -czf "$TARBALL" "Session Manager.app" )

echo "→ Signing tarball"
cargo tauri signer sign \
  --private-key-path "$KEY_PATH" \
  --password "" \
  "$TARBALL"
# `tauri signer sign` writes the .sig as <input>.sig — verify it.
if [[ ! -f "$SIG_FILE" ]]; then
  echo "✗ Expected $SIG_FILE after signing" >&2
  ls "$OUT_DIR" >&2
  exit 1
fi
SIG_CONTENT="$(cat "$SIG_FILE")"

NOTES=""
if [[ "${1:-}" == "--notes" ]]; then
  shift; NOTES="$*"
fi

DOWNLOAD_URL="https://github.com/${REPO_SLUG}/releases/download/${TAG}/$(basename "$TARBALL")"

echo "→ Writing latest.json"
jq -n \
  --arg version "$VERSION" \
  --arg notes "${NOTES:-Release $TAG}" \
  --arg url "$DOWNLOAD_URL" \
  --arg sig "$SIG_CONTENT" \
  '{
    version: $version,
    notes: $notes,
    pub_date: (now | strftime("%Y-%m-%dT%H:%M:%SZ")),
    platforms: {
      "darwin-aarch64": { signature: $sig, url: $url }
    }
  }' > "$LATEST_JSON"

echo "→ Creating GitHub release"
gh release create "$TAG" \
  --repo "$REPO_SLUG" \
  --title "Session Manager $TAG" \
  --notes "${NOTES:-Release $TAG}" \
  "$TARBALL" "$SIG_FILE" "$LATEST_JSON"

echo "✓ Released $TAG → https://github.com/${REPO_SLUG}/releases/tag/${TAG}"
