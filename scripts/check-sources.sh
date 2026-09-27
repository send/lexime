#!/usr/bin/env bash
# Refuse dependency sources the rest of the supply-chain screen cannot vet.
# One of scripts/screen.sh's checks (see there); run from the repository
# root, or from a fixture tree laid out like it (scripts/check-sources-test.sh).
set -euo pipefail

# crates.io only, because that is all the screen's other checks can vet:
# check-quarantine.sh asks the crates.io API for publish dates, and it and
# check-build-scripts.sh skip every package that is not from a registry. A git or other-registry dependency
# would pass them unexamined. (engine/deny.toml's [sources] says the same,
# but cargo-deny runs only in the audit job, alongside the builds.)
#
# Source replacement in a cargo config would fetch "crates.io" packages from
# somewhere else while Cargo.lock still names crates.io, so it is refused
# too. Only the configs inside the repository: cargo runs from engine/, and
# the ones it reads above the checkout are the runner's, not the tree's.
#
# Both files are parsed as TOML, as cargo parses them: a line pattern misses
# spellings TOML treats as the same key (indentation, quoted keys, dotted
# keys, inline tables). A config that does not parse is refused.
python3 - engine/Cargo.lock .cargo/config .cargo/config.toml \
    engine/.cargo/config engine/.cargo/config.toml <<'PY'
import os
import sys

try:
    import tomllib
except ImportError:
    sys.exit("sources: needs Python 3.11+ (tomllib) to parse Cargo.lock")

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
lock, configs = sys.argv[1], sys.argv[2:]


def sources(node):
    # Every `source` string anywhere in the lock: [[package]] entries and
    # [[patch.unused]] alike.
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "source" and isinstance(value, str):
                yield value
            else:
                yield from sources(value)
    elif isinstance(node, list):
        for value in node:
            yield from sources(value)


with open(lock, "rb") as f:
    other = sorted(set(s for s in sources(tomllib.load(f)) if s != CRATES_IO))
if other:
    print("sources: Cargo.lock has dependencies from outside crates.io:")
    for s in other:
        print(f"  - {s}")
    sys.exit(1)

for path in configs:
    if not os.path.isfile(path):
        continue
    try:
        with open(path, "rb") as f:
            config = tomllib.load(f)
    except tomllib.TOMLDecodeError as e:
        sys.exit(f"sources: {path} does not parse as TOML ({e}); refusing it")
    if "source" in config:
        sys.exit(f"sources: {path} replaces a cargo source")

print("sources: crates.io only")
PY
