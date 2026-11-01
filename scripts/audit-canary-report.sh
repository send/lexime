#!/usr/bin/env bash
# Keep the single "dependency audit fails on main" issue in step with the
# audit canary's verdict. Run by the `report` job of
# .github/workflows/audit-canary.yml.
#
# Input (env): STATE, IDS, RUN_URL; gh auth via GH_TOKEN/GH_REPO.
#   STATE  success | failure — the outcome of `mise run audit` (after one
#          retry in the workflow, so a single transient outage is not one).
#   IDS    the advisory ids (RUSTSEC-… / GHSA-…) the failing run's log named,
#          comma-separated and sorted; empty when the failure names none (a
#          yanked crate, cargo-vet, machete, a tool or network failure).
#   failure: open the issue, or update the open one; its body always links the
#     latest run. A comment, which notifies, is added when the issue opens and
#     whenever the id set changes, so a failure that persists unchanged stays
#     quiet in the issue (the red scheduled run still notifies; that is meant:
#     an advisory one has decided to live with belongs in deny.toml's ignore,
#     which makes the audit pass).
#   success: close the open issue, if any.
#
# Only the ids reach the issue, never log text: they are checked against the
# id shapes below, so nothing from tool output has to be escaped for markdown.
set -euo pipefail
# shellcheck source=scripts/canary-issue.sh
source "$(dirname "$0")/canary-issue.sh"

marker='<!-- audit-canary'
title='dependency audit fails on main'

need() { [[ ${!1} =~ $2 ]] || { echo "report: bad $1 '${!1}'" >&2; exit 1; }; }
id='(RUSTSEC-[0-9]{4}-[0-9]{4}|GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4})'
need STATE '^(success|failure)$'
need IDS "^($id(,$id)*)?$"
need RUN_URL '^https://github\.com/[A-Za-z0-9._/-]+$'

found=$(canary_open_issue "$marker")
IFS=$'\t' read -r number old <<<"$found" || true

if [ "$STATE" = success ]; then
  if [ -n "$number" ]; then
    gh issue close "$number" --comment "\`mise run audit\` passes on main again. Closing. ([run]($RUN_URL))"
    echo "report: closed #$number"
  else
    echo "report: audit passes, no open issue"
  fi
  exit 0
fi

fingerprint="$marker ids=$IDS -->"
# shellcheck disable=SC2016 # literal backticks: markdown code spans
if [ -n "$IDS" ]; then
  verdict="\`mise run audit\` fails on main. Advisories named in the log: \`$(printf '%s' "$IDS" | sed 's/,/`, `/g')\`."
else
  verdict="\`mise run audit\` fails on main, and the log names no advisory id: read the run for which check failed."
fi

body=$(cat <<EOF
$fingerprint
$verdict

The run log names the failing check. What fixes it depends on which:

- **an advisory** (cargo-deny, or the GHSA check \`scripts/check-ghsa.py\`): bump to the patched version, or accept it with a reason in \`engine/deny.toml\` \`[advisories] ignore\`;
- **a yanked crate** (\`yanked = "deny"\`): move to a version that is not yanked;
- **cargo-vet / cargo-machete**: an audit or exemption in \`engine/supply-chain/\`, or an unused dependency to drop;
- **a tool or service**: a new cargo-deny / cargo-vet / machete release, or an outage of crates.io, the advisory DB or the GitHub Advisory API (the workflow already retried once). ci.yml's audit job uses the same unpinned tools, so a tool change fails PRs the same way.

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
if [ "$old" = "$fingerprint" ]; then
  echo "report: #$number unchanged, refreshed its last-checked link"
else
  gh issue comment "$number" --body "$verdict ([run]($RUN_URL))" >/dev/null
  echo "report: #$number updated, advisory set changed"
fi
