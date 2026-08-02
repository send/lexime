#!/usr/bin/env bash
# Single definition of "which Swift files make up the app", printed one per line.
#
# Two mise tasks consume it: `build` compiles this set into Lexime.app, and
# `compile-swift` gates it in CI. They have to agree. A file the build
# compiles but the gate does not is #316 reopening, and it reopens *silently* —
# which is why this is one script rather than the same `find` written twice.
#
# Sorted so a compile failure reproduces in the same order every run;
# find(1) order is filesystem-dependent.
set -euo pipefail

sources=$(find Sources -name '*.swift' -type f | sort)

# An empty result would make `compile-swift` pass by compiling nothing — the
# same shape of failure the gate exists to close. This needs its own test:
# `find` on an existing-but-empty tree *succeeds*, so `set -e` and `pipefail`
# have nothing to fire on. (They do cover the other direction — a missing
# Sources/ makes find fail, and a plain assignment propagates that status.)
if [ -z "$sources" ]; then
  echo "swift-sources: no Swift files under Sources/ — refusing to report an empty app" >&2
  exit 1
fi

printf '%s\n' "$sources"
