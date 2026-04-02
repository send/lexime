#!/usr/bin/env bash
# Check that all dependency versions in Cargo.lock were published at least
# QUARANTINE_DAYS ago. This protects against supply chain attacks where a
# compromised version is detected and removed within a few days.
#
# Usage: scripts/check-quarantine.sh [--all]
#   --all: check all deps (default: only check deps changed vs main)
set -euo pipefail

QUARANTINE_DAYS="${QUARANTINE_DAYS:-7}"
LOCKFILE="engine/Cargo.lock"
ALLOWLIST="engine/quarantine-allowlist.toml"
USER_AGENT="lexime-ci (github.com/send/lexime)"
CHECK_ALL=false

if [[ "${1:-}" == "--all" ]]; then
    CHECK_ALL=true
fi

# Parse Cargo.lock for (name, version) pairs of registry deps
parse_lockfile() {
    awk '
        /^\[\[package\]\]/ { name=""; version=""; source="" }
        /^name = / { gsub(/"/, "", $3); name=$3 }
        /^version = / { gsub(/"/, "", $3); version=$3 }
        /^source = "registry/ { source="registry" }
        /^$/ {
            if (name != "" && version != "" && source == "registry") {
                print name " " version
            }
            name=""; version=""; source=""
        }
        END {
            if (name != "" && version != "" && source == "registry") {
                print name " " version
            }
        }
    ' "$1"
}

# Get deps to check (changed only, or all)
if $CHECK_ALL; then
    deps=$(parse_lockfile "$LOCKFILE")
else
    current=$(parse_lockfile "$LOCKFILE" | sort)
    if git show origin/main:"$LOCKFILE" >/dev/null 2>&1; then
        base=$(git show origin/main:"$LOCKFILE" | parse_lockfile /dev/stdin | sort)
        deps=$(comm -23 <(echo "$current") <(echo "$base"))
    else
        deps="$current"
    fi
fi

if [ -z "$deps" ]; then
    echo "quarantine: no new/changed deps to check"
    exit 0
fi

# Parse allowlist
allowed=""
if [ -f "$ALLOWLIST" ]; then
    allowed=$(awk -F' *= *' '
        /^\[allow\]/ { in_allow=1; next }
        /^\[/ { in_allow=0 }
        in_allow && /=/ {
            gsub(/"/, "", $1); gsub(/"/, "", $2);
            gsub(/^[ \t]+/, "", $1); gsub(/[ \t]+$/, "", $1);
            gsub(/^[ \t]+/, "", $2); gsub(/[ \t]+$/, "", $2);
            print $1 " " $2
        }
    ' "$ALLOWLIST")
fi

now=$(date +%s)
threshold=$((now - QUARANTINE_DAYS * 86400))
tmpfile=$(mktemp)
trap 'rm -f "$tmpfile"' EXIT

echo "$deps" | while read -r name version; do
    [ -z "$name" ] && continue

    # Check allowlist
    if echo "$allowed" | grep -qxF "$name $version"; then
        echo "quarantine: $name@$version — allowed (in allowlist)"
        continue
    fi

    # Query crates.io API
    response=$(curl -sf -H "User-Agent: $USER_AGENT" \
        "https://crates.io/api/v1/crates/$name/$version" 2>/dev/null) || {
        echo "quarantine: $name@$version — WARNING: API request failed, skipping"
        continue
    }

    created_at=$(echo "$response" | python3 -c "
import sys, json
from datetime import datetime
data = json.load(sys.stdin)
dt = datetime.fromisoformat(data['version']['created_at'].replace('Z', '+00:00'))
print(int(dt.timestamp()))
" 2>/dev/null) || {
        echo "quarantine: $name@$version — WARNING: failed to parse date, skipping"
        continue
    }

    age_days=$(( (now - created_at) / 86400 ))

    if [ "$created_at" -gt "$threshold" ]; then
        echo "quarantine: FAIL $name@$version — published $age_days days ago (minimum: $QUARANTINE_DAYS)"
        echo "FAIL" >> "$tmpfile"
    else
        echo "quarantine: ok $name@$version — published $age_days days ago"
    fi

    # Rate limit: 1 req/sec
    sleep 1
done

failures=$(wc -l < "$tmpfile" | tr -d ' ')

if [ "$failures" -gt 0 ]; then
    echo ""
    echo "quarantine: $failures dep(s) published less than $QUARANTINE_DAYS days ago"
    echo "If this is intentional (e.g. security patch), add to $ALLOWLIST"
    exit 1
fi

echo "quarantine: all checked deps passed ($QUARANTINE_DAYS-day policy)"
