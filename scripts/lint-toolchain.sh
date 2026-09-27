#!/usr/bin/env bash
# Print the Rust toolchain the `lint` gate runs on: the one version pinned in
# engine/lint-toolchain.txt (see that file for why, and for the bump procedure).
#
# Two consumers, which have to agree: the CI `lint` job and `mise run lint`.
# A local gate on a different clippy than CI is how a PR passes locally and
# goes red on push, so both read the version here instead of spelling it.
set -euo pipefail

file=${1:-engine/lint-toolchain.txt}

# First bare X.Y.Z line (surrounding whitespace allowed). Comments and anything
# else are excluded by the shape of the match.
version=$(grep -oE -m1 '^[[:space:]]*[0-9]+\.[0-9]+\.[0-9]+[[:space:]]*$' "$file" | tr -d '[:space:]' || true)

# An empty result would hand the consumers an empty toolchain name, and the
# toolchain action / `cargo +` would fall back or fail confusingly. Refuse here.
if [ -z "$version" ]; then
  echo "lint-toolchain: no X.Y.Z line in $file" >&2
  exit 1
fi

printf '%s\n' "$version"
