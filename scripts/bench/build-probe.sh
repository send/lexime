#!/usr/bin/env bash
# Build one arm of an A/B measurement: lextool and the speed probe
# (engine/crates/lex-cli/examples/speed_probe.rs) from a worktree, in release,
# copied out under a tag.
#
#   build-probe.sh <worktree> <out-dir> <tag>   -> <out-dir>/lextool-<tag>, probe-<tag>
#
# The worktree's own screen runs first (scripts/screen.sh, through its mise
# task): cargo runs dependency build scripts. The build uses the worktree's
# own target directory and --locked, so it changes no dependency.
#
# Every arm is built from THIS checkout's probe source, put in place for the
# build and the worktree's own file restored after: the probe's inputs and
# timed region are part of the measurement, so arms built from different
# probe sources would compare the probes, not the engines. The probe needs
# `--now` in the API (E2n, #390, and later). The source's shasum is printed
# with the binaries'.
set -euo pipefail

wt=$1 out=$2 tag=$3
here=$(cd "$(dirname "$0")/../.." && pwd)
probe=engine/crates/lex-cli/examples/speed_probe.rs

git -C "$wt" log -1 --format='%H %s'
git -C "$wt" status --short
mkdir -p "$out"
(cd "$wt" && mise run screen)

saved=$(mktemp -d "${TMPDIR:-/tmp}/build-probe.XXXXXX")
restore() {
  if [[ -e $saved/speed_probe.rs ]]; then
    /bin/cp "$saved/speed_probe.rs" "$wt/$probe"
  else
    rm -f "$wt/$probe"
  fi
  rm -rf "$saved"
}
if [[ $wt/$probe -ef $here/$probe ]]; then
  restore() { rm -rf "$saved"; }
else
  [[ -e $wt/$probe ]] && /bin/cp "$wt/$probe" "$saved/speed_probe.rs"
  mkdir -p "$(dirname "$wt/$probe")"
  /bin/cp "$here/$probe" "$wt/$probe"
fi
trap restore EXIT
(cd "$wt/engine" && cargo build --release --locked -p lex-cli --bin lextool --example speed_probe)
target=$(cd "$wt/engine" && cargo metadata --format-version 1 --no-deps | jq -r .target_directory)
/bin/cp "$target/release/lextool" "$out/lextool-$tag"
/bin/cp "$target/release/examples/speed_probe" "$out/probe-$tag"
shasum "$here/$probe" "$out/lextool-$tag" "$out/probe-$tag"
