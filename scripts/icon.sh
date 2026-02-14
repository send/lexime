#!/usr/bin/env bash
set -euo pipefail

SIZES=(16 32 64 128 256 512 1024)

# --- PNG export (light + dark) ---
for variant in light dark; do
  for s in "${SIZES[@]}"; do
    rsvg-convert -w "$s" -h "$s" "assets/icon/master/lexime-${variant}.svg" \
      -o "assets/icon/export/${variant}/png/icon_${s}x${s}.png"
  done
done

# --- macOS iconset ---
ICONSET_DIR="assets/icon/macos/lexime.iconset"
mkdir -p "$ICONSET_DIR"

LIGHT="assets/icon/export/light/png"
cp "$LIGHT/icon_16x16.png"    "$ICONSET_DIR/icon_16x16.png"
cp "$LIGHT/icon_32x32.png"    "$ICONSET_DIR/icon_16x16@2x.png"
cp "$LIGHT/icon_32x32.png"    "$ICONSET_DIR/icon_32x32.png"
cp "$LIGHT/icon_64x64.png"    "$ICONSET_DIR/icon_32x32@2x.png"
cp "$LIGHT/icon_128x128.png"  "$ICONSET_DIR/icon_128x128.png"
cp "$LIGHT/icon_256x256.png"  "$ICONSET_DIR/icon_128x128@2x.png"
cp "$LIGHT/icon_256x256.png"  "$ICONSET_DIR/icon_256x256.png"
cp "$LIGHT/icon_512x512.png"  "$ICONSET_DIR/icon_256x256@2x.png"
cp "$LIGHT/icon_512x512.png"  "$ICONSET_DIR/icon_512x512.png"
cp "$LIGHT/icon_1024x1024.png" "$ICONSET_DIR/icon_512x512@2x.png"

# --- icns ---
iconutil -c icns "$ICONSET_DIR" -o assets/icon/macos/lexime.icns

# --- IME menu bar icon (template, tiff) ---
MENUBAR_PNG=$(mktemp /tmp/lexime-menubar-XXXXXX.png)
rsvg-convert -w 16 -h 16 "assets/icon/master/lexime-menubar.svg" -o "$MENUBAR_PNG"
sips -s format tiff "$MENUBAR_PNG" --out Resources/icon.tiff >/dev/null
rm -f "$MENUBAR_PNG"

echo "Icon generation complete."
