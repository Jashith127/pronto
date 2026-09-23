#!/bin/sh
set -eu

repository_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=$(mktemp "${TMPDIR:-/tmp}/pronto-hotkey-test.XXXXXX")
trap 'rm -f "$binary"' EXIT HUP INT TERM

clang -fobjc-arc -framework Carbon -framework AppKit \
  -o "$binary" \
  "$repository_dir/scripts/test-macos-hotkeys.m" \
  "$repository_dir/src-tauri/src/platform/macos/hotkey_carbon.m"
"$binary"
