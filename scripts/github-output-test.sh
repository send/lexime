#!/usr/bin/env bash
# Exercise the real canary writer with a released shoutx and fake lint/rustc;
# no build, network, or issue-reporting side effects. CI's read-pin job runs it.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v shoutx >/dev/null
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
unset GITHUB_ACTIONS GITHUB_OUTPUT GITHUB_STEP_SUMMARY
pin=$(bash scripts/lint-toolchain.sh)

rustc() { printf 'rustc %s (fixture)\n' "$TEST_VERSION"; }
mise() { printf '%s\n' "$TEST_LOG"; return "$TEST_STATUS"; }
export -f rustc mise
export TEST_VERSION TEST_STATUS TEST_LOG

check_case() {
  local want=$1 rc=0 want_rc=$2
  : > "$tmp/output"
  GITHUB_OUTPUT="$tmp/output" GITHUB_STEP_SUMMARY="$tmp/summary" \
    bash scripts/lint-canary.sh > "$tmp/log" || rc=$?
  [[ $rc == "$want_rc" ]]
  printf 'state=%s\nstable=%s\npin=%s\nlints=%s\n' \
    "$want" "$TEST_VERSION" "$pin" "$3" > "$tmp/expected"
  cmp "$tmp/expected" "$tmp/output"
  grep -qF "## Lint canary: $want" "$tmp/summary"
  echo "ok   canary: $want"
}

TEST_VERSION=$pin TEST_STATUS=0 TEST_LOG=''
check_case current 0 ''
TEST_VERSION=999.0.0
check_case free-bump 0 ''
TEST_STATUS=1 TEST_LOG='error[E0308]: fixture'
check_case behind 0 E0308
TEST_VERSION=$pin
check_case error 1 E0308

# Local use must not call shoutx when there is no command-file destination.
(
  # shellcheck disable=SC2329 # exported to the child bash
  shoutx() { echo 'unexpected shoutx call' >&2; return 99; }
  export -f shoutx
  TEST_STATUS=0 TEST_LOG='' bash scripts/lint-canary.sh > "$tmp/local"
)
grep -qxF 'state=current' "$tmp/local"
echo 'ok   canary: local use needs no shoutx'

# A writer failure must fail CI rather than silently omit its outputs.
if (
  # shellcheck disable=SC2329 # exported to the child bash
  shoutx() { return 99; }
  export -f shoutx
  TEST_STATUS=0 GITHUB_OUTPUT="$tmp/output" bash scripts/lint-canary.sh > "$tmp/failed"
); then
  echo 'FAIL canary ignored a writer failure' >&2
  exit 1
fi
echo 'ok   canary: writer failure propagates'

# Malformed multi-line tool output must not become an extra output record.
TEST_VERSION=$'999.0.0\nrustc injected=yes' TEST_STATUS=0 TEST_LOG=''
: > "$tmp/output"
if GITHUB_OUTPUT="$tmp/output" bash scripts/lint-canary.sh > "$tmp/rejected" 2> "$tmp/error"; then
  echo 'FAIL canary accepted a multi-line version' >&2
  exit 1
fi
if grep -q '^injected=' "$tmp/output"; then
  echo 'FAIL canary emitted an injected record' >&2
  exit 1
fi
echo 'ok   canary: multi-line output rejected'
