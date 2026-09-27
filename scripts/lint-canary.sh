#!/usr/bin/env bash
# Run the lint gate (`mise run lint`) on a floating toolchain instead of the pin
# and classify the result. Driven weekly by .github/workflows/lint-canary.yml;
# `behind` or `free-bump` is the cue to bump engine/lint-toolchain.txt.
#
# Usage: scripts/lint-canary.sh [toolchain]   (default: stable)
#
# The lint command is not spelled out here: it is `mise run lint` with
# LINT_TOOLCHAIN overriding the pin, so the canary runs exactly the gate that
# CI and the local hook run.
#
# After the lint log, prints key=value lines (appended to $GITHUB_OUTPUT when
# set):
#   state   current    toolchain not newer than the pin, lint passes
#           free-bump  newer than the pin, lint passes: bumping costs nothing
#           behind     newer than the pin, lint reports findings: bump and fix
#           error      lint failed, but not as a sign of pin age: no finding
#                      was recognised (the run itself broke), or the toolchain
#                      is not newer than the pin
#   stable  version of the toolchain that ran
#   pin     pinned version
#   lints   comma-separated lint names / error codes (behind only)
# Exits 0 for current / free-bump and 1 for behind / error.
set -euo pipefail

toolchain=${1:-stable}
pin=$(bash scripts/lint-toolchain.sh)
stable=$(rustc +"$toolchain" --version | awk '{print $2}')

log=$(mktemp)
trap 'rm -f "$log"' EXIT

status=0
LINT_TOOLCHAIN=$toolchain mise run lint 2>&1 | tee "$log" || status=$?

# rustc prints one of these notes the first time each lint fires in a crate:
#   `-D clippy::needless-return` implied by `-D warnings`
#   `#[deny(clippy::absurd_extreme_comparisons)]` on by default
# Hard errors carry a code instead: error[E0308]. Hyphens are normalised to the
# underscore spelling #[allow] takes. `lint` runs fmt first and stops at a
# diff, so a rustfmt change is reported as `rustfmt` and clippy is not reached.
# Unanchored, in case mise prefixes task output with the task name.
# shellcheck disable=SC2016 # the backticks are literal, in rustc's notes
lints=$(
  {
    sed -nE 's/.*`-D ([A-Za-z0-9_:-]+)` implied by `-D warnings`.*/\1/p' "$log"
    sed -nE 's/.*`#\[deny\(([A-Za-z0-9_:]+)\)\]` on by default.*/\1/p' "$log"
    sed -nE 's/.*error\[(E[0-9]{4})\].*/\1/p' "$log"
    if grep -q 'Diff in /' "$log"; then echo rustfmt; fi
  } | sed 's/-/_/g' | sort -u | paste -sd, -
)

# The pin is behind when it sorts strictly before the toolchain.
if [ "$pin" != "$stable" ] && [ "$(printf '%s\n%s\n' "$pin" "$stable" | sort -V | head -n1)" = "$pin" ]; then
  newer=true
else
  newer=false
fi

if [ "$status" -eq 0 ]; then
  if $newer; then state=free-bump; else state=current; fi
  lints=
elif $newer && [ -n "$lints" ]; then
  state=behind
else
  state=error
fi

printf 'state=%s\nstable=%s\npin=%s\nlints=%s\n' "$state" "$stable" "$pin" "$lints" |
  tee -a "${GITHUB_OUTPUT:-/dev/null}"

case $state in
  current | free-bump) exit 0 ;;
  *) exit 1 ;;
esac
