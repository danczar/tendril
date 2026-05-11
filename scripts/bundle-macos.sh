#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ICON_SRC="$ROOT/crates/tendril-ui/ui/tendril-icon-1024.png"
APP_DIR="$ROOT/target/bundle/Tendril.app"
CONTENTS="$APP_DIR/Contents"

echo "==> Building release binaries (arm64 + x86_64)..."
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

echo "==> Creating universal binary..."
mkdir -p "$CONTENTS/MacOS"
lipo -create \
    "$ROOT/target/aarch64-apple-darwin/release/Tendril" \
    "$ROOT/target/x86_64-apple-darwin/release/Tendril" \
    -output "$CONTENTS/MacOS/Tendril"

echo "==> Generating .icns from tendril-icon-1024.png..."
ICONSET=$(mktemp -d)/tendril.iconset
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z $size $size "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z $double $double "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
mkdir -p "$CONTENTS/Resources"
iconutil -c icns "$ICONSET" -o "$CONTENTS/Resources/tendril.icns"
rm -rf "$(dirname "$ICONSET")"

echo "==> Copying Info.plist..."
cp "$ROOT/macos/Info.plist" "$CONTENTS/Info.plist"

# Override CFBundleVersion and CFBundleShortVersionString from the workspace
# Cargo.toml so the macOS About window can't drift from the shipped binary's
# CARGO_PKG_VERSION. Reads the first `version = "..."` line, which is
# workspace.package.version (both crates use `version.workspace = true`).
VERSION=$(awk -F'"' '/^version *= *"/ { print $2; exit }' "$ROOT/Cargo.toml")
plutil -replace CFBundleVersion -string "$VERSION" "$CONTENTS/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$CONTENTS/Info.plist"
echo "    bundled version: $VERSION"

echo "==> Ad-hoc code signing..."
codesign --force --deep --sign - "$APP_DIR"

echo "==> Done: $APP_DIR"
