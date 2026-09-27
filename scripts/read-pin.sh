#!/usr/bin/env bash
# The one reader behind the pins in engine/lint-toolchain.txt,
# engine/data/mozc-pin.txt and mise.toml (`min_version`). Each has a thin
# wrapper (lint-toolchain.sh, mozc-pin.sh, mise-version.sh) that owns its file
# and value pattern.
#
#   read-pin.sh <file> <what> <value-ERE> [<key>]
#
# Prints the first line of <file> that is a bare <value-ERE> match — or, with
# <key>, a TOML `<key> = "<value>"` line, printing only the value — without the
# surrounding whitespace and lowercased (so a case-variant SHA pin maps to the
# same CI cache key, as in dictool; X.Y.Z is unaffected). <what> names the
# value in the refusal message.
set -euo pipefail

file=$1 what=$2 ere=$3 key=${4:-}

pat=$ere
if [[ -n $key ]]; then
  # A bare TOML key has no regex metacharacters, so it can go into the ERE as
  # is. Anything else (a dotted or quoted key) is refused, not half-matched.
  [[ $key =~ ^[A-Za-z0-9_-]+$ ]] || {
    echo "read-pin: <key> must be a bare TOML key, got '$key'" >&2
    exit 2
  }
  pat="${key}[[:space:]]*=[[:space:]]*\"(${ere})\""
fi

# With pipefail a missing match fails the assignment, so an empty value never
# reaches the consumers. In CI the refusal is also a file annotation.
value=$(grep -oE -m1 "^[[:space:]]*(${pat})[[:space:]]*\$" "$file" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]') || {
  echo "${GITHUB_ACTIONS:+::error file=$file::}read-pin: no $what line in $file" >&2
  exit 1
}

if [[ -n $key ]]; then
  value=${value#*=\"}
  value=${value%\"}
fi

printf '%s\n' "$value"
