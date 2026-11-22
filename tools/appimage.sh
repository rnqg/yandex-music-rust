#!/bin/bash
# Собирает AppImage из уже собранного target/release/volna-rs: бинарник, ярлык и значки.
# Библиотеки не вшиваются, их даёт система: сборка на Ubuntu 22.04 работает с glibc 2.35
# и новее. appimagetool 1.9.1 скачивается сам и сверяется по sha256; его рантайм собран
# статически и не требует libfuse2.
# Результат: target/appimage/volna-rs-x86_64.AppImage.
set -euo pipefail
cd "$(dirname "$0")/.."

TOOL_URL="https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage"
TOOL_SHA256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
OUT=target/appimage
APPDIR="$OUT/volna-rs.AppDir"
TOOL="$OUT/appimagetool"

mkdir -p "$OUT"
if ! echo "$TOOL_SHA256  $TOOL" | sha256sum -c --status 2>/dev/null; then
  curl -fsSL -o "$TOOL" "$TOOL_URL"
  echo "$TOOL_SHA256  $TOOL" | sha256sum -c --quiet
  chmod +x "$TOOL"
fi

rm -rf "$APPDIR"
install -Dm755 target/release/volna-rs "$APPDIR/usr/bin/volna-rs"
install -Dm644 packaging/volna-rs.desktop "$APPDIR/usr/share/applications/volna-rs.desktop"
for size in 16 32 48 64 128 256; do
  install -Dm644 "packaging/icons/$size.png" "$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps/volna-rs.png"
done
ln -s usr/bin/volna-rs "$APPDIR/AppRun"
ln -s usr/share/applications/volna-rs.desktop "$APPDIR/volna-rs.desktop"
ln -s usr/share/icons/hicolor/256x256/apps/volna-rs.png "$APPDIR/volna-rs.png"
ln -s volna-rs.png "$APPDIR/.DirIcon"

# Сам appimagetool — тоже AppImage: распаковка вместо FUSE работает и в контейнерах CI.
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$OUT/volna-rs-x86_64.AppImage"
