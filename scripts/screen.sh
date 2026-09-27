#!/usr/bin/env bash
# The supply-chain screen: the one definition behind ci.yml's `screen` job,
# lint-canary.yml's `canary` job, `mise run audit`, and mise's `screen` task,
# which `lint` and `test` depend on (the @claude bot's only screen). Add a
# check here, not to a caller, or the other callers will not get it.
#
# Run it before anything that builds: cargo compiles and executes dependency
# build scripts, and these checks are what vets them. Neither check builds
# (check-build-scripts.sh runs only `cargo metadata --locked`).
#
# Run from the repository root, with origin/main fetched (in Actions:
# actions/checkout with fetch-depth: 0). Without it check-quarantine.sh has
# no base to diff against and checks every dependency, at 1 request/s.
set -euo pipefail

bash scripts/check-quarantine.sh
bash scripts/check-build-scripts.sh
