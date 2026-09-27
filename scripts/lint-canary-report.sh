#!/usr/bin/env bash
# Keep the single "lint pin is behind stable" issue in step with the lint
# canary's verdict (scripts/lint-canary.sh). Run by the `report` job of
# .github/workflows/lint-canary.yml.
#
# Input (env): STATE, STABLE, PIN, LINTS, RUN_URL; gh auth via GH_TOKEN/GH_REPO.
#   behind / free-bump: open the issue, or update the open one. Its body
#     always shows the latest run. A comment, which notifies, is added only
#     when the verdict changes (state, stable version or finding set), so an
#     unchanged week stays quiet.
#   current: close the open issue, if any.
#
# The issue is found by the marker comment in its body, not by title or
# search, so a retitled issue still dedupes and there is no search-index lag.
set -euo pipefail

marker='<!-- lint-canary'
title='lint pin is behind stable'

# Everything below lands in an issue body. The values come from rustc output
# via lint-canary.sh, so check their shape rather than trust it.
[[ $STATE =~ ^(current|free-bump|behind)$ ]] || { echo "report: bad STATE '$STATE'" >&2; exit 1; }
[[ $STABLE =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "report: bad STABLE '$STABLE'" >&2; exit 1; }
[[ $PIN =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "report: bad PIN '$PIN'" >&2; exit 1; }
[[ $LINTS =~ ^[A-Za-z0-9_:,]*$ ]] || { echo "report: bad LINTS '$LINTS'" >&2; exit 1; }

number=$(gh issue list --state open --limit 1000 --json number,body \
  --jq "map(select(.body | contains(\"$marker\"))) | .[0].number // empty")

if [ "$STATE" = current ]; then
  if [ -n "$number" ]; then
    gh issue close "$number" --comment "The pin ($PIN) is no longer behind stable ($STABLE). Closing. ([run]($RUN_URL))"
    echo "report: closed #$number"
  else
    echo "report: pin is current, no open issue"
  fi
  exit 0
fi

fingerprint="$marker state=$STATE stable=$STABLE lints=$LINTS -->"

case $STATE in
  behind)
    # shellcheck disable=SC2016 # literal backticks: markdown code spans
    findings=$(printf '%s' "$LINTS" | sed 's/,/`, `/g')
    verdict="Stable **$STABLE** fails the lint gate. The pin is **$PIN**. Findings: \`$findings\`."
    action="Bump the pin and fix the findings in the same PR." ;;
  free-bump)
    verdict="Stable **$STABLE** passes the lint gate as-is. The pin is **$PIN**."
    action="This is a free bump: change the version line and nothing needs fixing." ;;
esac

body=$(cat <<EOF
$fingerprint
$verdict

$action The procedure is in the header of \`engine/lint-toolchain.txt\`.

Last checked: $RUN_URL

_Maintained by \`.github/workflows/lint-canary.yml\`. It keeps this one issue up to date while stable is ahead of the pin, and closes it once the pin catches up._
EOF
)

if [ -z "$number" ]; then
  url=$(gh issue create --title "$title" --body "$body")
  echo "report: opened $url"
  exit 0
fi

old=$(gh issue view "$number" --json body --jq .body | grep -F -m1 "$marker" || true)
gh issue edit "$number" --body "$body" >/dev/null
if [ "$old" = "$fingerprint" ]; then
  echo "report: #$number unchanged, refreshed its last-checked link"
else
  gh issue comment "$number" --body "$verdict ([run]($RUN_URL))" >/dev/null
  echo "report: #$number updated, verdict changed"
fi
