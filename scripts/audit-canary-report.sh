#!/usr/bin/env bash
# Keep the single "dependency audit fails on main" issue in step with the
# audit canary's verdict. Run by the `report` job of
# .github/workflows/audit-canary.yml.
#
# Input (env): STATE, RUN_URL; gh auth via GH_TOKEN/GH_REPO.
#   failure: open the issue, or update the open one. Its body always links
#     the latest run. A comment, which notifies, is added only when the issue
#     is opened, so a failure that persists stays quiet day to day.
#   success: close the open issue, if any.
#
# The verdict is the outcome of `mise run audit` only, not its findings: the
# run log names them, and the log text never goes into the issue (it comes
# from tool output, so nothing here has to be escaped for markdown). A failure
# that is the network or a registry outage rather than an advisory reads the
# same here; the run log tells them apart.
#
# The issue is found by the marker comment in its body, not by title or
# search, as in scripts/lint-canary-report.sh: a retitled issue still dedupes,
# and only bot-authored issues count (anyone can paste the marker into an
# issue of their own in a public repo).
set -euo pipefail

marker='<!-- audit-canary -->'
title='dependency audit fails on main'

[[ $STATE =~ ^(success|failure)$ ]] || { echo "report: bad STATE '$STATE'" >&2; exit 1; }
[[ $RUN_URL =~ ^https://github\.com/[A-Za-z0-9._/-]+$ ]] || { echo "report: bad RUN_URL '$RUN_URL'" >&2; exit 1; }

number=$(gh issue list --state open --limit 1000 --json number,author,body --jq '
  map(select(.author.is_bot and (.body | contains("'"$marker"'"))))
  | sort_by(.number) | .[0] | select(.) | .number')

if [ "$STATE" = success ]; then
  if [ -n "$number" ]; then
    gh issue close "$number" --comment "\`mise run audit\` passes on main again. Closing. ([run]($RUN_URL))"
    echo "report: closed #$number"
  else
    echo "report: audit passes, no open issue"
  fi
  exit 0
fi

body=$(cat <<EOF
$marker
\`mise run audit\` fails on main: a dependency check (cargo-deny, the GHSA check, cargo-vet or machete) no longer passes for the lock main has. The run log names the check and the findings; it can also be an outage of crates.io or the GitHub Advisory API, which the log shows as a request failure.

Fix the dependency (bump to the patched version), or accept the advisory in \`engine/deny.toml\` \`[advisories] ignore\` with a reason.

Last checked: $RUN_URL

_Maintained by \`.github/workflows/audit-canary.yml\`. It keeps this one issue up to date while the audit fails on main, and closes it once the audit passes._
EOF
)

if [ -z "$number" ]; then
  url=$(gh issue create --title "$title" --body "$body")
  echo "report: opened $url"
  exit 0
fi

gh issue edit "$number" --body "$body" >/dev/null
echo "report: #$number still failing, refreshed its last-checked link"
