#!/usr/bin/env bash
# Print the pinned mise version (see `min_version` in mise.toml).
# The argument exists so the refusal path can be exercised on another file.
set -euo pipefail

exec bash "$(dirname "$0")/read-pin.sh" \
  "${1:-mise.toml}" 'min_version = "YYYY.M.P"' '[0-9]{4}\.[0-9]+\.[0-9]+' min_version
