#!/usr/bin/env bash
# Print the pinned lint toolchain (see engine/lint-toolchain.txt).
# The argument exists so the refusal path can be exercised on another file.
set -euo pipefail

exec bash "$(dirname "$0")/read-pin.sh" \
  "${1:-engine/lint-toolchain.txt}" X.Y.Z '[0-9]+\.[0-9]+\.[0-9]+'
