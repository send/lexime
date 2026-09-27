#!/usr/bin/env bash
# Print the pinned lint toolchain (see engine/lint-toolchain.txt).
# The argument exists so the refusal path can be exercised on another file.
set -euo pipefail

file=${1:-engine/lint-toolchain.txt}

# First bare X.Y.Z line. With pipefail a missing match fails the assignment, so
# an empty toolchain name never reaches the consumers.
version=$(grep -oE -m1 '^[[:space:]]*[0-9]+\.[0-9]+\.[0-9]+[[:space:]]*$' "$file" | tr -d '[:space:]') || {
  echo "lint-toolchain: no X.Y.Z line in $file" >&2
  exit 1
}

printf '%s\n' "$version"
