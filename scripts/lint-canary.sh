#!/usr/bin/env bash
# Run the lint gate (`mise run lint`) on a floating toolchain instead of the pin
# and classify the result. Driven weekly by .github/workflows/lint-canary.yml.
#
# Usage: scripts/lint-canary.sh [toolchain]   (default: stable)
#
# The lint command is not spelled out here: it is `mise run lint` with
# LINT_TOOLCHAIN overriding the pin: the same task CI's lint job runs, not a
# copy of its flags.
#
# The verdict depends only on whether the toolchain is newer than the pin and
# whether lint passed:
#   current    not newer, passes    nothing to do
#   free-bump  newer, passes        bumping the pin costs nothing
#   behind     newer, fails         bump the pin and fix what it reports
#   error      not newer, fails     not a pin-age signal: the gate is red on
#                                   the pin itself, or the run broke
# `lints` (the lint names / error codes found in the log) is informational.
# If rustc rewords its notes, `lints` comes back empty but the verdict does not
# change.
#
# After the lint log, prints state / stable / pin / lints as key=value lines
# (appended to $GITHUB_OUTPUT when set) and a table to $GITHUB_STEP_SUMMARY.
# Exits 1 only for `error`. `behind` exits 0 so the workflow's canary job
# succeeds and hands its outputs on; the report job turns the run red instead.
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

# Newer means the toolchain, not the pin, sorts last (equal versions: the pin).
newest=$(printf '%s\n%s\n' "$pin" "$stable" | sort -V | tail -n1)
case $status:$([ "$newest" != "$pin" ] && echo newer) in
  0:newer) state=free-bump ;;
  0:) state=current ;;
  *:newer) state=behind ;;
  *) state=error ;;
esac

printf 'state=%s\nstable=%s\npin=%s\nlints=%s\n' "$state" "$stable" "$pin" "$lints" |
  tee -a "${GITHUB_OUTPUT:-/dev/null}"

{
  echo "## Lint canary: $state"
  echo
  echo "| $toolchain | pin | findings |"
  echo "|---|---|---|"
  echo "| $stable | $pin | ${lints:-none recognised} |"
} >> "${GITHUB_STEP_SUMMARY:-/dev/null}"

[ "$state" != error ]
