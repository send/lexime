#!/usr/bin/env bash
# Print the pinned mise version (see `min_version` in mise.toml).
# The argument exists so the refusal path can be exercised on another file.
set -euo pipefail

# read-pin.sh returns the whole line with its whitespace removed,
# min_version="YYYY.M.P"; keep what is between the quotes.
line=$(bash "$(dirname "$0")/read-pin.sh" "${1:-mise.toml}" \
  'min_version = "YYYY.M.P"' \
  'min_version[[:space:]]*=[[:space:]]*"[0-9]{4}\.[0-9]+\.[0-9]+"')
line=${line#*\"}
printf '%s\n' "${line%\"}"
