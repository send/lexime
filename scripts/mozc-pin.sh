#!/usr/bin/env bash
# Print the pinned Mozc commit SHA (see engine/data/mozc-pin.txt).
# The argument exists so the refusal path can be exercised on another file.
#
# Lowercased to match dictool's pin normalization, so a case-variant pin line
# maps to the same CI cache key (mozc-raw-<sha>).
set -euo pipefail

exec bash "$(dirname "$0")/read-pin.sh" --lower \
  "${1:-engine/data/mozc-pin.txt}" '40-hex SHA' '[0-9a-fA-F]{40}'
