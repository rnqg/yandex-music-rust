#!/bin/bash
# Собирает AppImage из уже собранного target/release/music-rust: бинарник, ярлык и значки.
# Библиотеки не вшиваются, их даёт система: сборка на Ubuntu 22.04 работает с glibc 2.35
# и новее. appimagetool 1.9.1 скачивается сам и сверяется по sha256; его рантайм собран
# статически и не требует libfuse2.
# Результат: target/appimage/music-rust-x86_64.AppImage.
set -euo pipefail
cd "$(dirname "$0")/.."

TOOL_URL="https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage"
TOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
OUT=target/appimage
APPDIR="$OUT/music-rust.AppDir"
TOOL="$OUT/appimagetool"

mkdir -p "$OUT"
if ! echo "$TOOL_SHA256  $TOOL" | sha256sum -c --status 2>/dev/null; then
  curl -fsSL -o "$TOOL" "$TOOL_URL"
  echo "$TOOL_SHA256  $TOOL" | sha256sum -c --quiet
  chmod +x "$TOOL"
fi

rm -rf "$APPDIR"
install -Dm755 target/release/music-rust "$APPDIR/usr/bin/music-rust"
install -Dm644 packaging/music-rust.desktop "$APPDIR/usr/share/applications/music-rust.desktop"
for size in 16 32 48 64 128 256; do
  install -Dm644 "packaging/icons/$size.png" "$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps/music-rust.png"
done
ln -s usr/bin/music-rust "$APPDIR/AppRun"
ln -s usr/share/applications/music-rust.desktop "$APPDIR/music-rust.desktop"
ln -s usr/share/icons/hicolor/256x256/apps/music-rust.png "$APPDIR/music-rust.png"
ln -s music-rust.png "$APPDIR/.DirIcon"

# Сам appimagetool — тоже AppImage: распаковка вместо FUSE работает и в контейнерах CI.
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$OUT/music-rust-x86_64.AppImage"
