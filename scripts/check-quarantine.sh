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

# (name, version) of every registry dependency in the working tree's lock, as
# cargo reads it (`cargo metadata`, which resolves without building). A hand
# parser would skip packages whose keys are spelled differently but mean the
# same to cargo (#345), and so leave them unchecked.
current_deps() {
    (cd engine && cargo metadata --locked --all-features --format-version 1) | python3 -c '
import json, sys
for pkg in json.load(sys.stdin)["packages"]:
    if (pkg.get("source") or "").startswith("registry+"):
        print(pkg["name"], pkg["version"])
'
}

# The same pairs from a lock with no workspace around it (the base's, from
# git show), so cargo cannot read it and this parses it by line. It can only
# miss entries, never invent them, and a missed base entry makes the diff
# below check that dependency again: stricter, never looser.
parse_lockfile() {
    awk '
        /^\[\[package\]\]/ { name=""; version=""; source="" }
        /^name = / { gsub(/"/, "", $3); name=$3 }
        /^version = / { gsub(/"/, "", $3); version=$3 }
        /^source = "registry\+/ { source="registry" }
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
    deps=$(current_deps)
else
    current=$(current_deps | sort)
    # The vetted set to diff against: main, or SCREEN_POLICY_REF when set, so
    # a job that sets it trusts exactly one ref (see scripts/screen.sh).
    base_ref=${SCREEN_POLICY_REF:-origin/main}
    if git show "$base_ref:$LOCKFILE" >/dev/null 2>&1; then
        base=$(git show "$base_ref:$LOCKFILE" | parse_lockfile /dev/stdin | sort)
        deps=$(comm -23 <(printf '%s\n' "$current") <(printf '%s\n' "$base"))
    else
        deps="$current"
    fi
fi

if [ -z "$deps" ]; then
    echo "quarantine: no new/changed deps to check"
    exit 0
fi

# Parse allowlist: the working tree's, or with SCREEN_POLICY_REF set (see
# scripts/screen.sh) that git ref's. Unreadable there counts as empty.
allowlist=""
if [ -n "${SCREEN_POLICY_REF:-}" ]; then
    allowlist=$(git show "$SCREEN_POLICY_REF:$ALLOWLIST" 2>/dev/null) || true
elif [ -f "$ALLOWLIST" ]; then
    allowlist=$(cat "$ALLOWLIST")
fi
allowed=""
if [ -n "$allowlist" ]; then
    allowed=$(printf '%s\n' "$allowlist" | awk -F' *= *' '
        /^\[allow\]/ { in_allow=1; next }
        /^\[/ { in_allow=0 }
        in_allow && /=/ {
            sub(/ *#.*$/, "");
            gsub(/"/, "", $1); gsub(/"/, "", $2);
            gsub(/^[ \t]+/, "", $1); gsub(/[ \t]+$/, "", $1);
            gsub(/^[ \t]+/, "", $2); gsub(/[ \t]+$/, "", $2);
            if ($1 != "" && $2 != "") print $1 " " $2
        }
    ')
fi

now=$(date +%s)
if ! [[ "$QUARANTINE_DAYS" =~ ^[0-9]+$ ]] || [ "$QUARANTINE_DAYS" -le 0 ]; then
    echo "quarantine: invalid QUARANTINE_DAYS='$QUARANTINE_DAYS' (expected a positive integer)" >&2
    exit 1
fi
threshold=$((now - QUARANTINE_DAYS * 86400))
tmpfile=$(mktemp)
trap 'rm -f "$tmpfile"' EXIT

# Publish dates already looked up, one "name version checksum unix-time" per
# line, so a changed entry costs a crates.io request once per machine rather
# than on every task that builds (and works offline after that). crates.io
# never lets a version be published twice, so its date is fixed and its age
# only grows. The checksum is in the key because a deleted crate's name can
# be taken again: a new upload under the same version has a new checksum,
# and a lock that names it misses here and is asked about. The date is kept
# rather than the verdict so a longer QUARANTINE_DAYS still applies, and only
# for versions that passed, with crates.io's checksum matching the lock's:
# one still in quarantine, or one the API could not answer for, is asked
# about again on every run.
# Outside the repository, and off under SCREEN_POLICY_REF: that marks the
# one kind of job whose agent builds code it edited between screens (see
# scripts/screen.sh), and that code could write this file. Elsewhere whoever
# can write it can edit the allowlist in the tree too, so it grants nothing
# more.
cache=""
if [ -z "${SCREEN_POLICY_REF:-}" ]; then
    cache_dir="${XDG_CACHE_HOME:-${HOME:+$HOME/.cache}}"
    if [ -n "$cache_dir" ] && mkdir -p "$cache_dir/lexime" 2>/dev/null; then
        cache="$cache_dir/lexime/crates-io-published"
    fi
fi
# The lock's checksum for each registry entry, for the cache key. Read by
# line: an entry it misses has no checksum, so it neither reads nor writes
# the cache, and is asked about like any other.
lock_sums=""
if [ -n "$cache" ]; then
    lock_sums=$(awk '
        /^\[\[package\]\]/ { name=""; version=""; sum="" }
        /^name = / { gsub(/"/, "", $3); name=$3 }
        /^version = / { gsub(/"/, "", $3); version=$3 }
        /^checksum = / { gsub(/"/, "", $3); sum=$3 }
        name != "" && version != "" && sum != "" { print name, version, sum; name="" }
    ' "$LOCKFILE") || lock_sums=""
fi

echo "$deps" | while read -r name version; do
    [ -z "$name" ] && continue

    # Check allowlist
    if echo "$allowed" | grep -qxF "$name $version"; then
        echo "quarantine: $name@$version — allowed (in allowlist)"
        continue
    fi

    # The cached date, if any. Malformed lines are ignored; of several, the
    # latest date is the strictest. Held to 1-11 digits without a leading
    # zero: bash's `[ -gt ]` errors on a number past its range, and the error
    # would take the `ok` branch below; a leading zero reads as octal.
    created_at=""
    sum=$(printf '%s\n' "$lock_sums" | awk -v n="$name" -v v="$version" '$1 == n && $2 == v { print $3; exit }')
    [[ $sum =~ ^[0-9a-f]{64}$ ]] || sum=""
    if [ -n "$sum" ] && [ -f "$cache" ]; then
        created_at=$(awk -v n="$name" -v v="$version" -v c="$sum" '
            NF == 4 && $1 == n && $2 == v && $3 == c && $4 ~ /^[1-9][0-9]*$/ && length($4) <= 11 && $4 + 0 > max + 0 { max = $4 }
            END { if (max != "") print max }
        ' "$cache") || created_at=""
    fi
    via=" (date from $cache)"

    if [ -z "$created_at" ]; then
        via=""
        # Query crates.io API
        response=$(curl -sf --connect-timeout 10 --max-time 30 \
            --retry 2 --retry-delay 2 --retry-all-errors \
            -H "User-Agent: $USER_AGENT" \
            "https://crates.io/api/v1/crates/$name/$version" 2>/dev/null) || {
            echo "quarantine: FAIL $name@$version — API request failed (unable to verify age)"
            echo "FAIL" >> "$tmpfile"
            continue
        }

        read -r created_at api_sum < <(echo "$response" | python3 -c "
import sys, json
from datetime import datetime
data = json.load(sys.stdin)
dt = datetime.fromisoformat(data['version']['created_at'].replace('Z', '+00:00'))
print(int(dt.timestamp()), data['version'].get('checksum') or '-')
" 2>/dev/null) && [ -n "$created_at" ] || {
            echo "quarantine: FAIL $name@$version — failed to parse publication date (unable to verify age)"
            echo "FAIL" >> "$tmpfile"
            continue
        }

        # Rate limit: 1 req/sec
        sleep 1
    fi

    age_days=$(( (now - created_at) / 86400 ))

    if [ "$created_at" -gt "$threshold" ]; then
        echo "quarantine: FAIL $name@$version — published $age_days days ago (minimum: $QUARANTINE_DAYS)$via"
        echo "FAIL" >> "$tmpfile"
    else
        echo "quarantine: ok $name@$version — published $age_days days ago$via"
        if [ -z "$via" ] && [ -n "$sum" ] && [ "$api_sum" = "$sum" ]; then
            printf '%s %s %s %s\n' "$name" "$version" "$sum" "$created_at" >> "$cache" 2>/dev/null || true
        fi
    fi
done

failures=$(wc -l < "$tmpfile" | tr -d ' ')

if [ "$failures" -gt 0 ]; then
    echo ""
    echo "quarantine: $failures dep(s) published less than $QUARANTINE_DAYS days ago"
    echo "If this is intentional (e.g. security patch), add to $ALLOWLIST"
    if [ -n "${SCREEN_POLICY_REF:-}" ]; then
        echo "(read from $SCREEN_POLICY_REF here: an entry counts once it is merged there)"
    fi
    exit 1
fi

echo "quarantine: all checked deps passed ($QUARANTINE_DAYS-day policy)"
