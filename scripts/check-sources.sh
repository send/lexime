#!/usr/bin/env bash
# Refuse dependency sources the rest of the supply-chain screen cannot vet.
# One of scripts/screen.sh's checks (see there); run from the repository
# root, or from a fixture tree laid out like it (scripts/check-sources-test.sh).
#
# Nothing here parses TOML itself: a hand parser that misses a spelling cargo
# accepts fails open (#345: an indented lock `source`, a quoted config
# table), and a real TOML parser (tomllib) needs a Python the supported macOS
# setup does not have.
set -euo pipefail

# No cargo config inside the repository. Several of its keys move where
# dependencies come from while Cargo.lock still names crates.io — `source`
# replacement, `patch`, `paths`, and `include`, which pulls any of those in
# from another file — and none of it is visible to the checks below. The
# repository has never needed one; adding one means changing this check,
# in review. Only the repository's own: cargo runs from engine/, and the
# configs it reads above the checkout are the runner's, not the tree's.
for f in .cargo/config .cargo/config.toml engine/.cargo/config engine/.cargo/config.toml; do
    if [ -e "$f" ]; then
        echo "sources: $f is a cargo config inside the repository; the screen"
        echo "cannot vet what it does to dependency sources (scripts/check-sources.sh)"
        exit 1
    fi
done

# crates.io only, because that is all the screen's other checks can vet:
# check-quarantine.sh asks the crates.io API for publish dates, and it and
# check-build-scripts.sh skip every package that is not from a registry. A
# git or other-registry dependency would pass them unexamined. (engine/
# deny.toml's [sources] says the same, but cargo-deny runs only in the audit
# job, alongside the builds.) The sources are cargo's own reading of
# Cargo.lock, from `cargo metadata`, which resolves without building.
# --locked: a lock out of step with the manifests is refused, not rewritten.
other=$(cd engine && cargo metadata --locked --all-features --format-version 1 | python3 -c '
import json, sys
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
for pkg in json.load(sys.stdin)["packages"]:
    source = pkg.get("source")
    if source is not None and source != CRATES_IO:
        print(pkg["name"], pkg["version"], source)
' | sort -u)
if [ -n "$other" ]; then
    echo "sources: Cargo.lock has dependencies from outside crates.io:"
    echo "$other" | sed 's/^/  - /'
    exit 1
fi
echo "sources: crates.io only"
