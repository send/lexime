#!/usr/bin/env bash
# Tests for scripts/check-sources.sh, the screen's refusal of dependency
# sources it cannot vet. Run by CI's screen job, before the screen itself, and
# by `mise run test-check-sources`.
#
# Each case is a real cargo tree (engine/Cargo.toml + Cargo.lock), run from
# its root. The git dependency is a local file:// repository, so the
# fixtures need no network. The spelling cases are why the check reads the lock
# through cargo: each passed a line pattern in #345, and cargo reads them all
# the same way.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
check="$repo/scripts/check-sources.sh"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

# A crate in a local git repository, for the git-dependency cases.
mkdir -p "$tmp/gitdep/src"
printf '[package]\nname = "gitdep"\nversion = "0.1.0"\nedition = "2021"\n' >"$tmp/gitdep/Cargo.toml"
: >"$tmp/gitdep/src/lib.rs"
git -C "$tmp/gitdep" init -q
git -C "$tmp/gitdep" add -A
git -C "$tmp/gitdep" -c user.email=test@example.com -c user.name=test commit -qm init

# fixture [<dependencies line>]: a fresh tree with a generated Cargo.lock.
# (mktemp, not a counter: this runs in a $(...) subshell, which would lose it.)
fixture() {
  local dir
  dir=$(mktemp -d "$tmp/case.XXXXXX")
  mkdir -p "$dir/engine/src"
  printf '[package]\nname = "fx"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\n%s\n' \
    "${1:-}" >"$dir/engine/Cargo.toml"
  : >"$dir/engine/src/lib.rs"
  (cd "$dir/engine" && cargo generate-lockfile -q)
  echo "$dir"
}

GITDEP="gitdep = { git = \"file://$tmp/gitdep\" }"

# expect <name> <want exit: 0 or fail> <want text in the output> <dir>
expect() {
  local name=$1 want=$2 line=$3 dir=$4 rc=0
  # $(fixture ...) is an argument, so set -e does not catch its failure; an
  # empty dir would otherwise run the case wherever this script stands.
  if [[ ! -d $dir ]]; then
    echo "FAIL $name"
    echo "     no fixture directory"
    fails=$((fails + 1))
    return
  fi
  (cd "$dir" && bash "$check") >"$tmp/out" 2>&1 || rc=$?
  if { [[ $want == 0 && $rc -eq 0 ]] || [[ $want == fail && $rc -ne 0 ]]; } &&
    grep -qF -- "$line" "$tmp/out"; then
    echo "ok   $name"
  else
    echo "FAIL $name"
    echo "     want: exit $want, output containing: $line"
    echo "     got:  exit $rc, output: $(cat "$tmp/out")"
    fails=$((fails + 1))
  fi
}

# --- Cargo.lock ---
expect "no dependencies passes" 0 "sources: crates.io only" "$(fixture)"
expect "the repository's own Cargo.lock passes" 0 "sources: crates.io only" "$repo"
expect "git dependency" fail "  - gitdep 0.1.0 git+file://" "$(fixture "$GITDEP")"
indented=$(fixture "$GITDEP")
sed -i.bak -e 's/^name = /  name = /' -e 's/^version = /  version = /' \
  -e 's/^source = /  source = /' "$indented/engine/Cargo.lock"
expect "git dependency, lock keys indented (#345)" fail "  - gitdep 0.1.0 git+file://" "$indented"
quoted=$(fixture "$GITDEP")
sed -i.bak -e 's/^source = /"source" = /' "$quoted/engine/Cargo.lock"
expect "git dependency, lock key quoted" fail "  - gitdep 0.1.0 git+file://" "$quoted"
missing=$(fixture)
rm "$missing/engine/Cargo.lock"
expect "missing Cargo.lock" fail "--locked" "$missing"

# --- cargo configs: any inside the repository is refused, whatever it says ---
for cfg in engine/.cargo/config.toml engine/.cargo/config .cargo/config.toml .cargo/config; do
  dir=$(fixture)
  mkdir -p "$dir/$(dirname "$cfg")"
  printf '[build]\nrustflags = []\n' >"$dir/$cfg"
  expect "$cfg" fail "sources: $cfg is a cargo config inside the repository" "$dir"
done
# The #345 spelling, and include (re-gate): refused before anything is parsed.
dir=$(fixture)
mkdir -p "$dir/engine/.cargo"
printf 'include = ["mirror.toml"]\n' >"$dir/engine/.cargo/config.toml"
printf '["source".crates-io]\nreplace-with = "v"\n' >"$dir/engine/.cargo/mirror.toml"
expect "config with include of a source replacement" fail "is a cargo config inside the repository" "$dir"

if [[ $fails -gt 0 ]]; then
  echo "check-sources-test: $fails failed"
  exit 1
fi
echo "check-sources-test: all passed"
