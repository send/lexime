#!/usr/bin/env bash
set -euo pipefail

ICONSET_DIR="assets/icon/macos/lexime.iconset"
mkdir -p "$ICONSET_DIR"

cp assets/icon/export/light/png/icon_16x16.png   "$ICONSET_DIR/icon_16x16.png"
cp assets/icon/export/light/png/icon_32x32.png   "$ICONSET_DIR/icon_16x16@2x.png"

cp assets/icon/export/light/png/icon_32x32.png   "$ICONSET_DIR/icon_32x32.png"
cp assets/icon/export/light/png/icon_64x64.png   "$ICONSET_DIR/icon_32x32@2x.png"

cp assets/icon/export/light/png/icon_128x128.png "$ICONSET_DIR/icon_128x128.png"
cp assets/icon/export/light/png/icon_256x256.png "$ICONSET_DIR/icon_128x128@2x.png"

cp assets/icon/export/light/png/icon_256x256.png "$ICONSET_DIR/icon_256x256.png"
cp assets/icon/export/light/png/icon_512x512.png "$ICONSET_DIR/icon_256x256@2x.png"

cp assets/icon/export/light/png/icon_512x512.png  "$ICONSET_DIR/icon_512x512.png"
cp assets/icon/export/light/png/icon_1024x1024.png "$ICONSET_DIR/icon_512x512@2x.png"

