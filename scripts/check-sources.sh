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
# A cargo config inside the repository can also move where dependencies come
# from while Cargo.lock still names crates.io: `source` replacement, `patch`,
# `paths` overrides, and `include`, which pulls any of those in from another
# file. So its top-level keys are held to an allowlist of ones that do not
# touch dependency resolution, and anything else — including keys a later
# cargo adds — is refused. Only the configs inside the repository: cargo runs
# from engine/, and the ones it reads above the checkout are the runner's,
# not the tree's.
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
# Cargo config tables that leave dependency sources alone. Extend it only
# with keys that cannot change which code a dependency resolves to.
CONFIG_KEYS = {"alias", "build", "cargo-new", "doc", "env", "future-incompat-report",
               "http", "net", "profile", "resolver", "target", "term"}


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
    refused = sorted(set(config) - CONFIG_KEYS)
    if refused:
        sys.exit(f"sources: {path} sets {', '.join(refused)}, which can change "
                 "where dependencies come from (see scripts/check-sources.sh)")

print("sources: crates.io only")
PY
