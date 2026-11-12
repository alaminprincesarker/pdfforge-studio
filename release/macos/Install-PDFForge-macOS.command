#!/bin/bash
set -e
echo "======================================================"
echo "  PDFForge 1.0.5 - Native Rust macOS Installer"
echo "======================================================"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
APP_BUNDLE="$SCRIPT_DIR/PDFForge.app"
ENTITLEMENTS="$SCRIPT_DIR/entitlements.plist"

if [ -f "$PROJECT_ROOT/Cargo.toml" ] && command -v cargo >/dev/null 2>&1; then
    echo "[1/4] Compiling native macOS Rust binary (cargo build --release)..."
    cd "$PROJECT_ROOT"
    cargo build --release
    cp "target/release/PDFForge-Rust" "$APP_BUNDLE/Contents/MacOS/PDFForge-bin"
    chmod +x "$APP_BUNDLE/Contents/MacOS/PDFForge-bin"
fi

chmod +x "$APP_BUNDLE/Contents/MacOS/PDFForge"
if [ -f "$APP_BUNDLE/Contents/MacOS/PDFForge-bin" ]; then
    chmod +x "$APP_BUNDLE/Contents/MacOS/PDFForge-bin"
fi

echo "[2/4] Clearing macOS Gatekeeper quarantine attributes..."
xattr -cr "$APP_BUNDLE" 2>/dev/null || true

SIGN_ID="${APPLE_SIGNING_IDENTITY:-}"
if [ -z "$SIGN_ID" ]; then
    SIGN_ID=$(security find-identity -v -p codesigning 2>/dev/null | awk -F'"' '/Developer ID Application/ {print $2; exit}')
fi

echo "[3/4] Signing PDFForge.app bundle (Hardened Runtime + Entitlements)..."
if [ -n "$SIGN_ID" ]; then
    echo "      Using identity: $SIGN_ID"
    if [ -f "$ENTITLEMENTS" ]; then
        codesign --force --deep --options runtime --timestamp --entitlements "$ENTITLEMENTS" --sign "$SIGN_ID" "$APP_BUNDLE"
    else
        codesign --force --deep --options runtime --timestamp --sign "$SIGN_ID" "$APP_BUNDLE"
    fi
else
    echo "      No Developer ID certificate found; applying Apple Silicon compatible ad-hoc signature..."
    if [ -f "$ENTITLEMENTS" ]; then
        codesign --force --deep --options runtime --entitlements "$ENTITLEMENTS" --sign - "$APP_BUNDLE"
    else
        codesign --force --deep --options runtime --sign - "$APP_BUNDLE"
    fi
fi

echo "[4/4] Installing PDFForge.app to /Applications & registering LaunchServices..."
rm -rf "/Applications/PDFForge.app"
cp -R "$APP_BUNDLE" "/Applications/PDFForge.app"
xattr -cr "/Applications/PDFForge.app" 2>/dev/null || true
codesign --verify --deep --strict "/Applications/PDFForge.app" || true
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "/Applications/PDFForge.app" || true

echo "Done! PDFForge Studio is installed in /Applications/PDFForge.app"
open -a "/Applications/PDFForge.app"
