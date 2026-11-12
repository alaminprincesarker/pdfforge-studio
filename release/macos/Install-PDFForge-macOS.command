#!/bin/bash
set -e
echo "======================================================"
echo "  PDFForge 1.0.0 - Native Rust macOS Installer"
echo "======================================================"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [ -f "$PROJECT_ROOT/Cargo.toml" ] && command -v cargo >/dev/null 2>&1; then
    echo "[1/3] Compiling native macOS Rust binary (cargo build --release)..."
    cd "$PROJECT_ROOT"
    cargo build --release
    cp "target/release/PDFForge-Rust" "$SCRIPT_DIR/PDFForge.app/Contents/MacOS/PDFForge-bin"
    chmod +x "$SCRIPT_DIR/PDFForge.app/Contents/MacOS/PDFForge-bin"
fi

chmod +x "$SCRIPT_DIR/PDFForge.app/Contents/MacOS/PDFForge"
echo "[2/3] Installing PDFForge.app to /Applications..."
rm -rf "/Applications/PDFForge.app"
cp -R "$SCRIPT_DIR/PDFForge.app" "/Applications/PDFForge.app"

echo "[3/3] Registering PDFForge & PDF Document Icons with macOS LaunchServices..."
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "/Applications/PDFForge.app" || true

echo "Done! PDFForge is now installed in /Applications/PDFForge.app"
open -a "/Applications/PDFForge.app"

