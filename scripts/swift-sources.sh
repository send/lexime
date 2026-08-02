#!/usr/bin/env bash
# Single definition of "which Swift files live under <dir>", printed one per line.
#
#   scripts/swift-sources.sh Sources   → the app's compile set
#   scripts/swift-sources.sh Tests     → the test binary's compile set
#
# Four consumers: `build` compiles the Sources set into Lexime.app, `compile-swift`
# gates it in CI, `test-swift` compiles the Tests set, and `test-dispatch.sh`
# reads the Tests set to generate the runner's dispatch. They have to agree. A
# file one of them sees and another does not is #316 reopening, and it reopens
# *silently* — which is why this is one script rather than the same `find`
# written four times.
#
# Sorted so a compile failure reproduces in the same order every run;
# find(1) order is filesystem-dependent.
set -euo pipefail

dir=${1:?usage: swift-sources.sh <dir>}

sources=$(find "$dir" -name '*.swift' -type f | sort)

# An empty result would make the consumers pass by compiling nothing — the same
# shape of failure these gates exist to close. This needs its own test: `find`
# on an existing-but-empty tree *succeeds*, so `set -e` and `pipefail` have
# nothing to fire on. (They do cover the other direction — a missing directory
# makes find fail, and a plain assignment propagates that status.)
if [ -z "$sources" ]; then
  echo "swift-sources: no Swift files under $dir/ — refusing to report an empty set" >&2
  exit 1
fi

printf '%s\n' "$sources"
