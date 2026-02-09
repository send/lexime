#!/usr/bin/env bash
set -euo pipefail

SIZES=(16 32 64 128 256 512 1024)

# light
for s in "${SIZES[@]}"; do
  rsvg-convert -w "$s" -h "$s" "assets/icon/master/lexime-light.svg" \
    -o "assets/icon/export/light/png/icon_${s}x${s}.png"
done

# dark
for s in "${SIZES[@]}"; do
  rsvg-convert -w "$s" -h "$s" "assets/icon/master/lexime-dark.svg" \
    -o "assets/icon/export/dark/png/icon_${s}x${s}.png"
done
