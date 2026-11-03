#!/bin/sh
# Ayna Ajan'ı derler: Apple Silicon + Intel tek (universal) uygulama, .zip (güncelleme için) ve .dmg.
# macOS'ta (Xcode komut satırı araçlarıyla) çalışır: ./build.sh
#
# AYNA_BUILD: sürüm numarası (GitHub Actions çalıştırma numarası; yoksa 0 = geliştirme).
# MAC_SIGN_P12 / MAC_SIGN_PASSWORD: sabit imza anahtarı (base64 p12). Varsa her sürüm aynı kimlikle
# imzalanır ve macOS izinleri (Ekran Kaydı, Erişilebilirlik) güncellemelerde korunur; yoksa ad-hoc.
set -eu
cd "$(dirname "$0")"
OUT=build
APP="$OUT/Ayna Ajan.app"
VERSION="${AYNA_BUILD:-0}"
rm -rf "$OUT"
mkdir -p "$APP/Contents/MacOS"

for arch in arm64 x86_64; do
    swiftc -O -target "$arch-apple-macos13.0" \
        -framework AppKit -framework Network -framework ServiceManagement \
        -framework ScreenCaptureKit -framework VideoToolbox -framework CoreMedia -framework CoreVideo -framework CryptoKit -framework Security \
        -framework CoreGraphics -import-objc-header Sources/VirtualDisplay.h \
        -o "$OUT/AynaAjan-$arch" Sources/main.swift
done
lipo -create -output "$APP/Contents/MacOS/AynaAjan" "$OUT/AynaAjan-arm64" "$OUT/AynaAjan-x86_64"
cp Info.plist "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $VERSION" -c "Set :CFBundleShortVersionString 1.$VERSION" "$APP/Contents/Info.plist"

if [ -n "${MAC_SIGN_P12:-}" ]; then
    KC="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/ayna-sign.keychain-db"
    KCPW=$(openssl rand -hex 16)
    P12="$OUT/sign.p12"
    printf '%s' "$MAC_SIGN_P12" | base64 --decode > "$P12"
    security create-keychain -p "$KCPW" "$KC"
    security set-keychain-settings -lut 3600 "$KC"
    security unlock-keychain -p "$KCPW" "$KC"
    security import "$P12" -k "$KC" -P "$MAC_SIGN_PASSWORD" -T /usr/bin/codesign
    rm -f "$P12"
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KCPW" "$KC" > /dev/null
    security list-keychains -d user -s "$KC" $(security list-keychains -d user | tr -d '"')
    security find-identity -p codesigning "$KC" || true
    codesign --force --keychain "$KC" --sign "Ayna Ajan" "$APP"
else
    # Apple Silicon imzasız kodu çalıştırmaz: ad-hoc imza (izinler her sürümde yeniden istenir).
    codesign --force --sign - "$APP"
fi
codesign -d -r- "$APP"

ditto -c -k --keepParent "$APP" "$OUT/AynaAjan.zip"
hdiutil create -volname "Ayna Ajan" -srcfolder "$APP" -ov -format UDZO "$OUT/AynaAjan.dmg"
echo "Hazır: $OUT/AynaAjan.dmg, $OUT/AynaAjan.zip (sürüm $VERSION)"
