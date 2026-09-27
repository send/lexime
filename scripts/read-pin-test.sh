#!/usr/bin/env bash
# Tests for scripts/read-pin.sh and its two wrappers (mozc-pin.sh,
# lint-toolchain.sh). Run by CI's read-pin job and `mise run test-read-pin`.
#
# The Mozc cases also compare against the inline one-liner read-pin.sh replaced
# (#332). Its output is the accuracy job's snapshot cache key and the SHA
# `mise run fetch-dict-mozc` downloads, so it has to stay byte-identical.
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

# The pre-#332 reader, verbatim. Prints nothing when no line matches.
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

# mozc_value <name> <value> <fixture>: mozc-pin.sh prints <value>, and so did
# the legacy one-liner.
mozc_value() {
  expect_value "mozc: $1" "$2" bash scripts/mozc-pin.sh "$3"
  local legacy
  legacy=$(legacy_mozc "$3")
  [[ $legacy == "$2" ]] || fail "mozc: $1 (legacy one-liner)" "want: $2" "got:  $legacy"
}

# mozc_refusal <name> <fixture>: mozc-pin.sh refuses, and the legacy one-liner
# printed nothing.
mozc_refusal() {
  expect_refusal "mozc: $1" "read-pin: no 40-hex SHA line in $2" bash scripts/mozc-pin.sh "$2"
  local legacy
  legacy=$(legacy_mozc "$2")
  [[ -z $legacy ]] || fail "mozc: $1 (legacy one-liner)" "want: no output" "got:  $legacy"
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
mozc_value "uppercase, spaces/tabs, CRLF" "$sha" "$tmp/upper-crlf"

printf '%s\n' "sha: $sha" "$sha trailing" "x$sha" "not a sha" "  $sha" "$other" >"$tmp/first-wins"
mozc_value "junk lines first, first SHA wins" "$sha" "$tmp/first-wins"

printf '# %s\n' "$sha" >"$tmp/commented"
printf '%s\n' "${sha:1}" >"$tmp/hex39"
printf '%s0\n' "$sha" >"$tmp/hex41"
printf '%s foo\n' "$sha" >"$tmp/trailing-text"
: >"$tmp/empty"
for f in commented hex39 hex41 trailing-text empty; do
  mozc_refusal "$f" "$tmp/$f"
done
mozc_refusal "missing file" "$tmp/missing"

expect_refusal "mozc: ::error annotation under GITHUB_ACTIONS" \
  "::error file=$tmp/empty::read-pin: no 40-hex SHA line in $tmp/empty" \
  env GITHUB_ACTIONS=true bash scripts/mozc-pin.sh "$tmp/empty"

# --- lint-toolchain.sh ------------------------------------------------------

expect_value "toolchain: current pin" \
  "$(grep -m1 -xE '[0-9]+\.[0-9]+\.[0-9]+' engine/lint-toolchain.txt)" \
  bash scripts/lint-toolchain.sh

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

if ((fails)); then
  echo "read-pin-test: $fails failed" >&2
  exit 1
fi
echo "read-pin-test: all passed"
