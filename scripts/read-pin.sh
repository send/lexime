#!/usr/bin/env bash
# The one reader behind every in-repo pin file. Each pin has a thin wrapper
# (lint-toolchain.sh, mozc-pin.sh) that owns its file and value pattern — the
# pattern the "Format:" line of that pin's header comment describes.
#
#   read-pin.sh [--lower] <file> <what> <value-ERE>
#
# Prints the first line of <file> that is a bare <value-ERE> match, minus the
# surrounding whitespace, so comments and junk lines are excluded by
# construction. --lower lowercases the value. <what> names the value in the
# refusal message.
set -euo pipefail

lower=false
if [ "${1:-}" = --lower ]; then
  lower=true
  shift
fi
if [ $# -ne 3 ]; then
  echo "usage: read-pin.sh [--lower] <file> <what> <value-ERE>" >&2
  exit 2
fi
file=$1 what=$2 ere=$3

# With pipefail a missing match fails the assignment, so an empty value never
# reaches the consumers. In CI the refusal is also a file annotation.
value=$(grep -oE -m1 "^[[:space:]]*${ere}[[:space:]]*\$" "$file" | tr -d '[:space:]') || {
  msg="no $what line in $file"
  if [ "${GITHUB_ACTIONS:-}" = true ]; then
    echo "::error file=$file::$msg" >&2
  else
    echo "read-pin: $msg" >&2
  fi
  exit 1
}

if $lower; then
  value=$(printf '%s' "$value" | tr '[:upper:]' '[:lower:]')
fi
printf '%s\n' "$value"
