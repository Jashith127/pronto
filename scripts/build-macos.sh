#!/bin/sh
set -eu

repository_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
mode=${1:-local}

"$repository_dir/scripts/prepare-macos-assets.sh"
cd "$repository_dir/src-tauri"

case "$mode" in
  local)
    CI=true APPLE_SIGNING_IDENTITY=- cargo tauri build --target aarch64-apple-darwin --bundles app,dmg --ci
    ;;
  signed)
    : "${APPLE_SIGNING_IDENTITY:?Set APPLE_SIGNING_IDENTITY to a Developer ID Application identity}"
    if [ -z "${APPLE_API_KEY:-}" ] && [ -z "${APPLE_ID:-}" ]; then
      echo 'Set App Store Connect API key variables or APPLE_ID, APPLE_PASSWORD, and APPLE_TEAM_ID for notarization.' >&2
      exit 1
    fi
    CI=true cargo tauri build --target aarch64-apple-darwin --bundles app,dmg --ci
    app="$repository_dir/src-tauri/target/aarch64-apple-darwin/release/bundle/macos/Pronto.app"
    codesign --verify --deep --strict --verbose=2 "$app"
    xcrun stapler validate "$app"
    ;;
  *)
    echo 'Usage: scripts/build-macos.sh [local|signed]' >&2
    exit 2
    ;;
esac
