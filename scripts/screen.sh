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
# the working tree. For a job whose agent can edit the tree and then build
# (the @claude bot sets it to origin/main): without it, following the
# screen's own "update the baseline" advice would let the next build run
# the crate it had just stopped. CI leaves it unset, because a PR's policy
# change is exactly what its reviewers look at.
set -euo pipefail

# crates.io only. The checks below look at registry packages alone, so a git
# or other-registry dependency would pass them unexamined. The same policy as
# engine/deny.toml's [sources], which cargo-deny enforces only in the audit
# job, alongside the builds rather than before them.
# A missing or unreadable Cargo.lock (grep exit 2) stops here; exit 1, no
# source lines at all, is fine.
sources=$(grep '^source = ' engine/Cargo.lock || [ $? -eq 1 ])
other=$(printf '%s\n' "$sources" \
    | grep -vxF -e 'source = "registry+https://github.com/rust-lang/crates.io-index"' -e '' \
    | sort -u || true)
if [ -n "$other" ]; then
    echo "sources: Cargo.lock has dependencies from outside crates.io:"
    echo "$other" | sed 's/^/  - /'
    exit 1
fi
echo "sources: crates.io only"

bash scripts/check-quarantine.sh
bash scripts/check-build-scripts.sh
