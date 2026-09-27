#!/usr/bin/env bash
# The supply-chain screen: the one definition behind ci.yml's `screen` job,
# lint-canary.yml's `canary` job, and mise's `screen` task, which `lint`,
# `test` and `audit` run first (the @claude bot's only screen). Add a check
# here, not to a caller, or the other callers will not get it.
#
# Run it before anything that builds: cargo compiles and executes dependency
# build scripts, and these checks are what vets them. None of the checks
# builds (check-build-scripts.sh runs only `cargo metadata --locked`).
#
# Run from the repository root, with origin/main fetched (in Actions:
# actions/checkout with fetch-depth: 0). Without it check-quarantine.sh has
# no base to diff against and checks every dependency, at 1 request/s.
#
# SCREEN_POLICY_REF: when set to a git ref, the policy files — the build.rs
# baseline and the quarantine allowlist — are read from that ref instead of
# the working tree, and quarantine diffs Cargo.lock against it instead of
# origin/main. For a job whose agent can edit the tree and then build
# (the @claude bot sets it to origin/main): without it, following the
# screen's own "update the baseline" advice would let the next build run
# the crate it had just stopped. CI leaves it unset, because a PR's policy
# change is exactly what its reviewers look at.
set -euo pipefail

# crates.io only, because that is all the checks below can vet: quarantine
# asks the crates.io API for publish dates, and both checks skip every
# package that is not from a registry. A git or other-registry dependency
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

bash scripts/check-quarantine.sh
bash scripts/check-build-scripts.sh
