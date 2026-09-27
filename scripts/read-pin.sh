#!/usr/bin/env bash
# The one reader behind every in-repo pin file. Each pin has a thin wrapper
# (lint-toolchain.sh, mozc-pin.sh) that owns its file and value pattern.
#
#   read-pin.sh <file> <what> <value-ERE>
#
# Prints the first line of <file> that is a bare <value-ERE> match, without the
# surrounding whitespace and lowercased (so a case-variant SHA pin maps to the
# same CI cache key, as in dictool; X.Y.Z is unaffected). <what> names the
# value in the refusal message.
set -euo pipefail

file=$1 what=$2 ere=$3

# With pipefail a missing match fails the assignment, so an empty value never
# reaches the consumers. In CI the refusal is also a file annotation.
value=$(grep -oE -m1 "^[[:space:]]*${ere}[[:space:]]*\$" "$file" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]') || {
  echo "${GITHUB_ACTIONS:+::error file=$file::}read-pin: no $what line in $file" >&2
  exit 1
}

printf '%s\n' "$value"
