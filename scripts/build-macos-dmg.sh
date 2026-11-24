#!/bin/bash
set -euo pipefail

# Build a Universal (Apple Silicon + Intel) macOS drag-to-Applications .dmg installer locally on macOS.
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/version[[:space:]]*=[[:space:]]*"([^"]+)"/\1/')"
echo "Building PDFForge Studio v${VERSION} macOS Universal DMG..."

rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

mkdir -p target/universal-release
lipo -create -output target/universal-release/PDFForge-Rust \
  target/aarch64-apple-darwin/release/PDFForge-Rust \
  target/x86_64-apple-darwin/release/PDFForge-Rust

STAGE="release/dmg-stage"
APP="$STAGE/PDFForge.app"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp -R release/macos/PDFForge.app "$APP"
cp target/universal-release/PDFForge-Rust "$APP/Contents/MacOS/PDFForge-bin"
chmod +x "$APP/Contents/MacOS/PDFForge-bin" "$APP/Contents/MacOS/PDFForge"

if [ -d "ocr-runtime/tessdata" ]; then
  mkdir -p "$APP/Contents/Resources/tessdata"
  cp ocr-runtime/tessdata/* "$APP/Contents/Resources/tessdata/"
fi

/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString ${VERSION}" "$APP/Contents/Info.plist" || true
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${VERSION}" "$APP/Contents/Info.plist" || true
xattr -cr "$APP" || true

SIGN_ID="${APPLE_SIGNING_IDENTITY:-}"
if [ -n "$SIGN_ID" ]; then
  codesign --force --deep --options runtime --timestamp \
    --entitlements release/macos/entitlements.plist \
    --sign "$SIGN_ID" "$APP"
else
  codesign --force --deep --options runtime \
    --entitlements release/macos/entitlements.plist \
    --sign - "$APP"
fi

ln -s /Applications "$STAGE/Applications"
DMG_OUT="release/PDFForge-${VERSION}-macOS-Universal.dmg"
rm -f "$DMG_OUT"
hdiutil create -volname "PDFForge Studio ${VERSION}" -srcfolder "$STAGE" -ov -format UDZO "$DMG_OUT"
rm -rf "$STAGE"

echo "Created macOS installer DMG: ${DMG_OUT}"
