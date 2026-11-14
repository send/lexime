#!/usr/bin/env bash
# Build one arm of an A/B measurement: lextool, the speed probe
# (engine/crates/lex-cli/examples/speed_probe.rs), the dictionary and the
# connection matrix from a worktree, in release, copied out under a tag.
# An arm is (binary, dict, conn): a change that only moves the dictionary is
# an arm too, so the arm's own dict / conn are built from its own commit.
#
#   build-probe.sh <worktree> <out-dir> <tag>
#       -> <out-dir>/lextool-<tag>, probe-<tag>, lexime-<tag>.dict, lexime-<tag>.conn
#
# The sha256 of all four (and the probe source) is printed: record them, with
# the commit, beside the frozen history. Pass them on as
# `replay-window.py --base/--base-dict/--base-conn/--new/--new-dict/
# --new-conn` and `probe-bench.py name=probe-<tag>:lexime-<tag>.dict:lexime-<tag>.conn`.
# The worktree must be clean (the sha256 of the artifacts must correspond to
# the printed commit): a dirty one is an error. The probe source put in
# place below is the one exception, and is restored.
# The dict / conn come from the worktree's `mise run dict` / `mise run conn`
# (the pinned Mozc data must be there: copy mozc-raw into a fresh worktree).
#
# The worktree's own screen runs first (scripts/screen.sh, through its mise
# task): cargo runs dependency build scripts. The build uses the worktree's
# own target directory and --locked, so it changes no dependency.
#
# Every arm is built from THIS checkout's probe source, put in place for the
# build and the worktree's own file restored after: the probe's inputs and
# timed region are part of the measurement, so arms built from different
# probe sources would compare the probes, not the engines. The probe needs
# `--now` in the API (E2n, #390, and later). The source's sha256 is printed
# with the binaries'.
set -euo pipefail

wt=$1 out=$2 tag=$3
here=$(cd "$(dirname "$0")/../.." && pwd)
probe=engine/crates/lex-cli/examples/speed_probe.rs

git -C "$wt" log -1 --format='%H %s'
if [[ -n $(git -C "$wt" status --porcelain) ]]; then
  git -C "$wt" status --short >&2
  echo "build-probe: $wt is dirty: the artifacts would not correspond to its commit" >&2
  exit 1
fi
mkdir -p "$out"
(cd "$wt" && mise run screen)
(cd "$wt" && mise run dict && mise run conn)

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
/bin/cp "$wt/engine/data/lexime.dict" "$out/lexime-$tag.dict"
/bin/cp "$wt/engine/data/lexime.conn" "$out/lexime-$tag.conn"
shasum -a 256 "$here/$probe" "$out/lextool-$tag" "$out/probe-$tag" "$out/lexime-$tag.dict" "$out/lexime-$tag.conn"
