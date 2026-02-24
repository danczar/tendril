#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ICON_SRC="$ROOT/crates/tendril-ui/ui/tendril-icon-new.png"
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

echo "==> Generating .icns from tendril-icon-new.png..."
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

echo "==> Ad-hoc code signing..."
codesign --force --deep --sign - "$APP_DIR"

echo "==> Done: $APP_DIR"
