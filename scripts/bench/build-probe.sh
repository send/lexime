#!/usr/bin/env bash
# Build one arm of an A/B measurement: lextool and the speed probe
# (engine/crates/lex-cli/examples/speed_probe.rs) from a worktree, in release,
# copied out under a tag.
#
#   build-probe.sh <worktree> <out-dir> <tag>   -> <out-dir>/lextool-<tag>, probe-<tag>
#
# The worktree's own screen runs first (scripts/screen.sh, through its mise
# task): cargo runs dependency build scripts. The build uses the worktree's
# own target directory and --locked, so it changes no dependency. A worktree
# whose commit predates the probe gets this checkout's copy as an untracked
# file for the build, removed after; the probe needs `--now` in the API (E2n,
# #390, and later).
set -euo pipefail

wt=$1 out=$2 tag=$3
here=$(cd "$(dirname "$0")/../.." && pwd)
probe=engine/crates/lex-cli/examples/speed_probe.rs

git -C "$wt" log -1 --format='%H %s'
git -C "$wt" status --short
mkdir -p "$out"
(cd "$wt" && mise run screen)

borrowed=""
if [[ ! -e $wt/$probe ]]; then
  mkdir -p "$(dirname "$wt/$probe")"
  /bin/cp "$here/$probe" "$wt/$probe"
  borrowed=$wt/$probe
  trap 'rm -f "$borrowed"' EXIT
fi
(cd "$wt/engine" && cargo build --release --locked -p lex-cli --bin lextool --example speed_probe)
target=$(cd "$wt/engine" && cargo metadata --format-version 1 --no-deps | jq -r .target_directory)
/bin/cp "$target/release/lextool" "$out/lextool-$tag"
/bin/cp "$target/release/examples/speed_probe" "$out/probe-$tag"
shasum "$out/lextool-$tag" "$out/probe-$tag"
