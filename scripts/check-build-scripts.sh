#!/usr/bin/env bash
# Detect when new crates with build.rs are added to the dependency tree.
# This helps catch supply chain attacks that use build scripts to execute
# arbitrary code at compile time.
set -euo pipefail

BASELINE="engine/build-script-baseline.txt"

if [ ! -f "$BASELINE" ]; then
    echo "ERROR: baseline file $BASELINE not found"
    echo "Generate it with: scripts/check-build-scripts.sh --update"
    exit 1
fi

current=$(cargo metadata --manifest-path engine/Cargo.toml --format-version 1 | python3 -c "
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

added=$(comm -23 <(echo "$current") <(sort -u "$BASELINE"))
removed=$(comm -13 <(echo "$current") <(sort -u "$BASELINE"))

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
    exit 1
fi

echo "build-scripts: no new build.rs crates (baseline: $(wc -l < "$BASELINE" | tr -d ' ') crates)"
