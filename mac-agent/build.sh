#!/bin/sh
# Ayna Ajan'ı derler: Apple Silicon + Intel tek (universal) uygulama, imzasız (ad-hoc) .app ve .dmg.
# macOS'ta (Xcode komut satırı araçlarıyla) çalışır: ./build.sh
set -eu
cd "$(dirname "$0")"
OUT=build
APP="$OUT/Ayna Ajan.app"
rm -rf "$OUT"
mkdir -p "$APP/Contents/MacOS"

for arch in arm64 x86_64; do
    swiftc -O -target "$arch-apple-macos13.0" \
        -framework AppKit -framework Network -framework ServiceManagement \
        -o "$OUT/AynaAjan-$arch" Sources/main.swift
done
lipo -create -output "$APP/Contents/MacOS/AynaAjan" "$OUT/AynaAjan-arm64" "$OUT/AynaAjan-x86_64"
cp Info.plist "$APP/Contents/Info.plist"

# Apple Silicon imzasız kodu çalıştırmaz: ad-hoc imza (geliştirici hesabı gerekmez).
codesign --force --sign - "$APP"

hdiutil create -volname "Ayna Ajan" -srcfolder "$APP" -ov -format UDZO "$OUT/AynaAjan.dmg"
echo "Hazır: $OUT/AynaAjan.dmg"
