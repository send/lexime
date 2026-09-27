#!/usr/bin/env bash
# Tests for scripts/read-pin.sh and its three wrappers (mozc-pin.sh,
# lint-toolchain.sh, mise-version.sh), and checks that every mise install is
# held to the pin (min_version's placement, claude.yml's inline version, the
# jdx/mise-action call sites). Run by CI's read-pin job and
# `mise run test-read-pin`.
#
# The Mozc cases also compare against the inline one-liner read-pin.sh replaced
# (#332), whose output was the accuracy job's snapshot cache key and the SHA
# `mise run fetch-dict-mozc` downloads: it pins that the switch changed neither.
# It is a record of #332, not a contract on the pin grammar. A deliberate
# grammar change updates or drops legacy_mozc rather than bending around it.
set -euo pipefail

cd "$(dirname "$0")/.."
# The expected refusals would otherwise be ::error annotations on the CI run.
# The one case that checks the annotation sets it for itself.
unset GITHUB_ACTIONS

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

fail() {
  echo "FAIL $1"
  shift
  printf '     %s\n' "$@"
  fails=$((fails + 1))
}

# The pre-#332 reader, with the file as an argument and grep's stderr (a missing
# file) silenced. Prints nothing when no line matches.
legacy_mozc() {
  grep -oE -m1 '^[[:space:]]*[0-9a-fA-F]{40}[[:space:]]*$' "$1" 2>/dev/null |
    tr -d '[:space:]' | tr '[:upper:]' '[:lower:]' || true
}

# expect_value <name> <value> <cmd...>: exits 0 and stdout is exactly
# "<value>\n", byte for byte.
expect_value() {
  local name=$1 want=$2 rc=0
  shift 2
  "$@" >"$tmp/out" 2>"$tmp/err" || rc=$?
  printf '%s\n' "$want" >"$tmp/want"
  if [[ $rc -eq 0 ]] && cmp -s "$tmp/out" "$tmp/want"; then
    echo "ok   $name"
  else
    fail "$name" "want: exit 0, stdout $(od -c "$tmp/want" | head -3)" \
      "got:  exit $rc, stdout $(od -c "$tmp/out" | head -3)" "stderr: $(cat "$tmp/err")"
  fi
}

# expect_refusal <name> <stderr line> <cmd...>: exits 1, prints nothing on
# stdout, and <stderr line> is one whole line of stderr (a missing file also
# gets grep's own message).
expect_refusal() {
  local name=$1 line=$2 rc=0
  shift 2
  "$@" >"$tmp/out" 2>"$tmp/err" || rc=$?
  if [[ $rc -eq 1 && ! -s $tmp/out ]] && grep -qxF -- "$line" "$tmp/err"; then
    echo "ok   $name"
  else
    fail "$name" "want: exit 1, no stdout, stderr line: $line" \
      "got:  exit $rc, stdout: $(cat "$tmp/out")" "stderr: $(cat "$tmp/err")"
  fi
}

# mozc_case <name> <value or ""> <fixture>: mozc-pin.sh prints <value>, or
# refuses when it is empty, and the legacy one-liner printed <value> (nothing).
mozc_case() {
  if [[ -n $2 ]]; then
    expect_value "mozc: $1" "$2" bash scripts/mozc-pin.sh "$3"
  else
    expect_refusal "mozc: $1" "read-pin: no 40-hex SHA line in $3" bash scripts/mozc-pin.sh "$3"
  fi
  local legacy
  legacy=$(legacy_mozc "$3")
  [[ $legacy == "$2" ]] || fail "mozc: $1 (legacy one-liner)" "want: '$2'" "got:  '$legacy'"
}

sha=0123456789abcdef0123456789abcdef01234567
upper=0123456789ABCDEF0123456789ABCDEF01234567
other=fedcba9876543210fedcba9876543210fedcba98

# --- mozc-pin.sh ------------------------------------------------------------

# The real pin, read by its default path. The legacy one-liner is the oracle.
current=$(legacy_mozc engine/data/mozc-pin.txt)
[[ $current =~ ^[0-9a-f]{40}$ ]] || fail "mozc: current pin (legacy one-liner)" "got: $current"
expect_value "mozc: current pin" "$current" bash scripts/mozc-pin.sh

printf '# pin\r\n \t%s \t\r\n' "$upper" >"$tmp/upper-crlf"
mozc_case "uppercase, spaces/tabs, CRLF" "$sha" "$tmp/upper-crlf"

printf '%s\n' "sha: $sha" "$sha trailing" "x$sha" "not a sha" "  $sha" "$other" >"$tmp/first-wins"
mozc_case "junk lines first, first SHA wins" "$sha" "$tmp/first-wins"

printf '# %s\n' "$sha" >"$tmp/commented"
printf '%s\n' "${sha:1}" >"$tmp/hex39"
printf '%s0\n' "$sha" >"$tmp/hex41"
printf '%s foo\n' "$sha" >"$tmp/trailing-text"
: >"$tmp/empty"
# `missing` is never created.
for f in commented hex39 hex41 trailing-text empty missing; do
  mozc_case "$f" "" "$tmp/$f"
done

expect_refusal "mozc: ::error annotation under GITHUB_ACTIONS" \
  "::error file=$tmp/empty::read-pin: no 40-hex SHA line in $tmp/empty" \
  env GITHUB_ACTIONS=true bash scripts/mozc-pin.sh "$tmp/empty"

# --- lint-toolchain.sh ------------------------------------------------------

# The real pin, read by its default path, against a plain grep of it.
current=$(grep -oE -m1 '^[[:space:]]*[0-9]+\.[0-9]+\.[0-9]+[[:space:]]*$' engine/lint-toolchain.txt |
  tr -d '[:space:]' || true)
[[ -n $current ]] || fail "toolchain: current pin (grep)" "no X.Y.Z line"
expect_value "toolchain: current pin" "$current" bash scripts/lint-toolchain.sh

printf '# pin\r\n\t 1.98.1 \r\n' >"$tmp/tc-crlf"
expect_value "toolchain: spaces/tabs, CRLF" 1.98.1 bash scripts/lint-toolchain.sh "$tmp/tc-crlf"

printf '# 1.98.1\n' >"$tmp/tc-commented"
printf '1.98\n' >"$tmp/tc-two-part"
printf 'v1.98.1\n' >"$tmp/tc-v-prefix"
for f in tc-commented tc-two-part tc-v-prefix; do
  expect_refusal "toolchain: $f" "read-pin: no X.Y.Z line in $tmp/$f" \
    bash scripts/lint-toolchain.sh "$tmp/$f"
done

# --- read-pin.sh: an alternation is anchored as a whole ----------------------
# Unparenthesised, `^…aaa|bbb…$` anchors aaa only at the start and bbb only at
# the end, so each fixture below would match through the other anchor.

printf 'aaax\n' >"$tmp/alt-tail"
printf 'xbbb\n' >"$tmp/alt-head"
for f in alt-tail alt-head; do
  expect_refusal "alternation: $f" "read-pin: no thing line in $tmp/$f" \
    bash scripts/read-pin.sh "$tmp/$f" thing 'aaa|bbb'
done
printf 'xbbb\n bbb \naaa\n' >"$tmp/alt-match"
expect_value "alternation: either branch matches" bbb \
  bash scripts/read-pin.sh "$tmp/alt-match" thing 'aaa|bbb'

# --- mise-version.sh: read-pin.sh's key mode --------------------------------

# The real pin, read by its default path, against a plain grep of it.
current=$(grep -oE -m1 '^min_version = "[0-9]{4}\.[0-9]+\.[0-9]+"$' mise.toml | cut -d'"' -f2 || true)
[[ -n $current ]] || fail "mise: current pin (grep)" 'no min_version = "YYYY.M.P" line'
expect_value "mise: current pin" "$current" bash scripts/mise-version.sh

printf '# min_version = "2000.1.1"\r\n \tmin_version\t=  "2026.9.11" \r\n' >"$tmp/mv-crlf"
expect_value "mise: commented line skipped, spaces/tabs, CRLF" 2026.9.11 \
  bash scripts/mise-version.sh "$tmp/mv-crlf"

printf 'min_version = "2026.9.11" # note\n' >"$tmp/mv-trailing-comment"
printf 'min_version = { hard = "2026.9.11" }\n' >"$tmp/mv-table"
printf 'min_version = 2026.9.11\n' >"$tmp/mv-unquoted"
printf 'xmin_version = "2026.9.11"\n' >"$tmp/mv-prefixed-key"
printf 'min_version = "26.9.11"\n' >"$tmp/mv-short-year"
for f in mv-trailing-comment mv-table mv-unquoted mv-prefixed-key mv-short-year; do
  expect_refusal "mise: $f" "read-pin: no min_version = \"YYYY.M.P\" line in $tmp/$f" \
    bash scripts/mise-version.sh "$tmp/$f"
done

# A key that is not a bare TOML key would go into the ERE with its
# metacharacters, so it is a caller error (exit 2), refused before matching.
rc=0
bash scripts/read-pin.sh "$tmp/mv-crlf" thing '[0-9.]+' 'min.version' >"$tmp/out" 2>"$tmp/err" || rc=$?
if [[ $rc -eq 2 && ! -s $tmp/out ]]; then
  echo "ok   key: dotted key refused"
else
  fail "key: dotted key refused" "want: exit 2, no stdout" "got:  exit $rc, stdout: $(cat "$tmp/out")"
fi

# --- claude.yml installs the pinned mise ------------------------------------
# The @claude job spells the version inline: it runs nothing from the checkout
# before its secret-holding step, so it cannot call mise-version.sh. Held to the
# pin here, so a bump cannot leave the bot on the old mise.
claude=$(sed -nE 's/^[[:space:]]+version:[[:space:]]*([^[:space:]#]+).*$/\1/p' .github/workflows/claude.yml)
if [[ -n $current && $claude == "$current" ]]; then
  echo "ok   mise: claude.yml version is min_version"
else
  fail "mise: claude.yml version is min_version" "want: $current" "got:  $claude"
fi

# --- mise.toml: min_version is top-level -------------------------------------
# Below a [table] header mise reads it as that table's key and drops the local
# floor, while read-pin.sh (which does not parse TOML) would still find it.
mv_line=$(grep -nE '^[[:space:]]*min_version[[:space:]]*=' mise.toml | head -n1 | cut -d: -f1)
table_line=$(grep -nE '^[[:space:]]*\[' mise.toml | head -n1 | cut -d: -f1)
if [[ -n $mv_line && (-z $table_line || $mv_line -lt $table_line) ]]; then
  echo "ok   mise: min_version is above the first table"
else
  fail "mise: min_version is above the first table" "min_version line: $mv_line" "first table line: $table_line"
fi

# --- jdx/mise-action: only the two pinned installs --------------------------
# Without `version:` the action installs the latest mise, so a job using it
# directly would float. Only setup-mise and claude.yml may, at one commit.
uses=$(grep -rlE "uses:[[:space:]]*[\"']?jdx/mise-action" .github | sort)
want=$(printf '%s\n' .github/actions/setup-mise/action.yml .github/workflows/claude.yml)
if [[ $uses == "$want" ]]; then
  echo "ok   mise-action: used only by setup-mise and claude.yml"
else
  fail "mise-action: used only by setup-mise and claude.yml" "got: $(echo $uses)"
fi
refs=$(grep -rhoE 'jdx/mise-action@[^[:space:]"'"'"']+' .github | sort -u)
if [[ $refs =~ ^jdx/mise-action@[0-9a-f]{40}$ ]]; then
  echo "ok   mise-action: one commit SHA"
else
  fail "mise-action: one commit SHA" "got: $(echo $refs)"
fi

if ((fails)); then
  echo "read-pin-test: $fails failed" >&2
  exit 1
fi
echo "read-pin-test: all passed"
