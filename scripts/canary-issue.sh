#!/usr/bin/env bash
# Shared by the canary report scripts (scripts/lint-canary-report.sh,
# scripts/audit-canary-report.sh), which each keep one tracking issue in step
# with a scheduled workflow's verdict. Sourced, not run.
#
# canary_open_issue <marker>: "<number>\t<marker line>" of the open issue
# whose body holds <marker>, or nothing. The marker line carries the last
# verdict's fingerprint, which the caller compares to decide whether to
# notify.
#
# The issue is found by the marker in its body, not by title or search, so a
# retitled issue still dedupes and there is no search-index lag. Only
# bot-authored issues count: the repo is public, and anyone can paste the
# marker into an issue of their own.
canary_open_issue() {
  local marker=$1
  gh issue list --state open --limit 1000 --json number,author,body --jq '
    map(select(.author.is_bot and (.body | contains("'"$marker"'"))))
    | sort_by(.number) | .[0] | select(.)
    | [.number, (.body | split("\n") | map(rtrimstr("\r"))
                 | map(select(startswith("'"$marker"'"))) | .[0] // "")]
    | @tsv'
}
