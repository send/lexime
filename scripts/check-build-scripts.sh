#!/usr/bin/env bash
# Detect when new crates with build.rs are added to the dependency tree.
# This helps catch supply chain attacks that use build scripts to execute
# arbitrary code at compile time.
set -euo pipefail

BASELINE="engine/build-script-baseline.txt"

# --locked: metadata must not rewrite an out-of-sync Cargo.lock — this script
# now runs before the audit chain's `cargo check --locked` and would otherwise
# silently repair the lockfile that check is supposed to verify.
# --all-features: CI builds with --all-features, so a build.rs behind an
# optional feature must be visible to this screen too.
current=$(cargo metadata --locked --all-features --manifest-path engine/Cargo.toml --format-version 1 | python3 -c "
import sys, json
data = json.load(sys.stdin)
for pkg in data['packages']:
    if pkg.get('source') and pkg['source'].startswith('registry'):
        for t in pkg.get('targets', []):
            if 'custom-build' in t.get('kind', []):
                print(pkg['name'])
                break
" | sort -u)

if [[ "${1:-}" == "--update" ]]; then
    echo "$current" > "$BASELINE"
    echo "build-scripts: baseline updated ($(wc -l < "$BASELINE" | tr -d ' ') crates)"
    exit 0
fi

# The baseline checked against: the working tree's, or with SCREEN_POLICY_REF
# set (see scripts/screen.sh) that git ref's. --update above always writes
# the working tree's.
if [ -n "${SCREEN_POLICY_REF:-}" ]; then
    baseline=$(git show "$SCREEN_POLICY_REF:$BASELINE" | sort -u) || {
        echo "ERROR: cannot read $BASELINE at $SCREEN_POLICY_REF"
        exit 1
    }
elif [ -f "$BASELINE" ]; then
    baseline=$(sort -u "$BASELINE")
else
    echo "ERROR: baseline file $BASELINE not found"
    echo "Generate it with: scripts/check-build-scripts.sh --update"
    exit 1
fi

added=$(comm -23 <(printf '%s\n' "$current") <(printf '%s\n' "$baseline"))
removed=$(comm -13 <(printf '%s\n' "$current") <(printf '%s\n' "$baseline"))

if [ -n "$removed" ]; then
    echo "build-scripts: removed (info only):"
    echo "$removed" | sed 's/^/  - /'
fi

if [ -n "$added" ]; then
    echo "build-scripts: NEW crates with build.rs detected:"
    echo "$added" | sed 's/^/  - /'
    echo ""
    echo "Review their build.rs before accepting. If safe, update baseline:"
    echo "  scripts/check-build-scripts.sh --update"
    if [ -n "${SCREEN_POLICY_REF:-}" ]; then
        echo "The baseline here is read from $SCREEN_POLICY_REF, so an update takes"
        echo "effect only once it has been reviewed and merged there. If the crate"
        echo "is one $SCREEN_POLICY_REF has since dropped, merge it into this branch."
    fi
    exit 1
fi

echo "build-scripts: no new build.rs crates (baseline${SCREEN_POLICY_REF:+ at $SCREEN_POLICY_REF}: $(printf '%s\n' "$baseline" | wc -l | tr -d ' ') crates)"
