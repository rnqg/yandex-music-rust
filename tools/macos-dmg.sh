#!/bin/bash
set -euo pipefail

if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 || $# != 1 ]]; then
    echo 'Usage on Apple Silicon: bash tools/macos-dmg.sh /path/to/music-rust-macos-arm64.dmg' >&2
    exit 1
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
output="$1"
if [[ "$output" != /* ]]; then
    output="$PWD/$output"
fi
if [[ "$output" != *.dmg || -e "$output" ]]; then
    echo "Choose a new .dmg path: $output" >&2
    exit 1
fi

command -v dmgbuild >/dev/null || { echo 'Install dmgbuild==1.6.7 before building the DMG.' >&2; exit 1; }
work="$(mktemp -d -t music-rust-dmg)"
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

export MACOSX_DEPLOYMENT_TARGET=13.0
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$work/build}"
if [[ "$CARGO_TARGET_DIR" != /* ]]; then
    export CARGO_TARGET_DIR="$PWD/$CARGO_TARGET_DIR"
fi
cd "$root"
cargo build --locked --release
assets="$root/packaging/macos"
name="$(plutil -extract CFBundleName raw -o - "$assets/Info.plist")"
executable="$(plutil -extract CFBundleExecutable raw -o - "$assets/Info.plist")"
binary="$CARGO_TARGET_DIR/release/$executable"
lipo "$binary" -verify_arch arm64
app="$work/image/$name.app"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$work/AppIcon.iconset"
install -m 755 "$binary" "$app/Contents/MacOS/$executable"
cp "$assets/Info.plist" "$app/Contents/Info.plist"
cp LICENSE "$app/Contents/Resources/LICENSE"
plutil -replace CFBundleShortVersionString -string "$version" "$app/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$version" "$app/Contents/Info.plist"

for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$assets/app-icon.png" --out "$work/AppIcon.iconset/icon_${size}x${size}.png" >/dev/null
    retina=$((size * 2))
    sips -z "$retina" "$retina" "$assets/app-icon.png" --out "$work/AppIcon.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$work/AppIcon.iconset" -o "$app/Contents/Resources/AppIcon.icns"
codesign --force --sign - "$app"
codesign --verify --strict "$app"
dmgbuild -s "$assets/dmg.py" -D "application=$app" "$name" "$work/music-rust.dmg"
mv -n "$work/music-rust.dmg" "$output"
test ! -e "$work/music-rust.dmg"
