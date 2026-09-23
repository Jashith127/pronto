#!/bin/sh
set -eu

repository_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
archive_name=nemo-speech-0.1.0-macos-aarch64-metal.tar.gz
archive_sha256=f1dff4f9dd9c96214f8cb78b982812459132df8a4ad1a42409fd94de4a366244
runtime_dir="$repository_dir/runtime/nemo-speech-macos"
iconset_dir="$repository_dir/src-tauri/icons/icon.iconset"
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT HUP INT TERM

if [ ! -x "$runtime_dir/bin/nemo-speech" ]; then
  curl --fail --location --retry 3 --output "$work_dir/$archive_name" \
    "https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/$archive_name"
  actual_sha256=$(shasum -a 256 "$work_dir/$archive_name" | awk '{print $1}')
  if [ "$actual_sha256" != "$archive_sha256" ]; then
    echo "NeMo-Speech archive checksum mismatch" >&2
    exit 1
  fi
  tar -xf "$work_dir/$archive_name" -C "$work_dir"
  mv "$work_dir/nemo-speech" "$runtime_dir"
fi

mkdir -p "$iconset_dir"
sips -s format png "$repository_dir/src-tauri/icons/icon.svg" \
  --out "$work_dir/source.png" >/dev/null
cp "$work_dir/source.png" "$repository_dir/src-tauri/icons/icon.png"
for size in 16 32 64 128 256 512; do
  sips -z "$size" "$size" "$work_dir/source.png" \
    --out "$iconset_dir/icon_${size}x${size}.png" >/dev/null
done
cp "$iconset_dir/icon_32x32.png" "$iconset_dir/icon_16x16@2x.png"
cp "$iconset_dir/icon_64x64.png" "$iconset_dir/icon_32x32@2x.png"
cp "$iconset_dir/icon_256x256.png" "$iconset_dir/icon_128x128@2x.png"
cp "$iconset_dir/icon_512x512.png" "$iconset_dir/icon_256x256@2x.png"
sips -z 1024 1024 "$work_dir/source.png" \
  --out "$iconset_dir/icon_512x512@2x.png" >/dev/null
iconutil -c icns "$iconset_dir" -o "$repository_dir/src-tauri/icons/icon.icns"
rm -rf "$iconset_dir"
