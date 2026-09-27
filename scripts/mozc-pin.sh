#!/usr/bin/env bash
# Print the pinned Mozc commit SHA (see engine/data/mozc-pin.txt).
# The argument exists so the refusal path can be exercised on another file.
set -euo pipefail

exec bash "$(dirname "$0")/read-pin.sh" \
  "${1:-engine/data/mozc-pin.txt}" '40-hex SHA' '[0-9a-fA-F]{40}'
