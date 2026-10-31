#!/usr/bin/env bash
# Fetch a PR's Codex review state from all three channels in one call — the
# Step-1 fetch of /external-converge and /external-review
# (.claude/skills/external-converge/project.md `reviewer`).
#
#   codex-review-fetch.sh <PR> [<since>]
#
# <since> is the trigger time of the round (ISO 8601, Z); default: the epoch.
# A round's findings arrive split across three channels, and each alone
# misses some (#339): inline review threads (GraphQL reviewThreads, author is
# the BARE login), the review BODY (P-badge findings, invisible to
# reviewThreads) and issue comments (the dry verdict is a plain comment, not a
# review). Every channel is read in full: threads page by endCursor, REST by
# --paginate (REST stops at 30 otherwise, #320). The counts come first, so a
# "0 findings" reads as "0 of N".
#
# The last line is a JSON verdict: assessed_commit (from the `Reviewed
# commit:` marker, the latest across reviews and comments), on_head, findings
# (P-badge bodies and non-verdict comments, on head and after <since>),
# unresolved_threads (the bot's, any commit: a thread carries none),
# dry_verdict (a dry verdict on head after <since>, both counts 0),
# errors_since_trigger.
#
# Env: REVIEWER_BOT (default chatgpt-codex-connector[bot]), REPO (default: the
# current checkout's GitHub repository).
set -euo pipefail

pr=$1 since=${2:-1970-01-01T00:00:00Z}
repo=${REPO:-$(gh repo view --json nameWithOwner -q .nameWithOwner)}
owner=${repo%/*} name=${repo#*/}
bot=${REVIEWER_BOT:-chatgpt-codex-connector[bot]}
bot_bare=${bot%\[bot\]}
run=$(mktemp -d "${TMPDIR:-/tmp}/codex-review-fetch.$pr.XXXXXX")
trap 'rm -rf "$run"' EXIT

head=$(gh pr view "$pr" -R "$repo" --json headRefOid -q .headRefOid)
echo "head=$head since=$since"
marker="Reviewed commit:" dry="Didn't find any major issues"
err="Something went wrong" status="codex-pull-request-review-summary"

cursor="" page=1
while :; do
  after=""
  [[ -n $cursor ]] && after=", after: \"$cursor\""
  gh api graphql -f query="{ repository(owner: \"$owner\", name: \"$name\") { pullRequest(number: $pr) { reviewThreads(first: 100$after) { totalCount pageInfo { hasNextPage endCursor } nodes { id isResolved comments(first: 1) { nodes { databaseId createdAt path line body author { login } } } } } } } }" \
    >"$run/threads_$page.json"
  has=$(jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.hasNextPage' "$run/threads_$page.json")
  cursor=$(jq -r '.data.repository.pullRequest.reviewThreads.pageInfo.endCursor' "$run/threads_$page.json")
  [[ $has == true ]] || break
  page=$((page + 1))
done
gh api --paginate "repos/$repo/pulls/$pr/reviews" \
  --jq '.[]|select(.user.login=="'"$bot"'")|{kind:"review",at:.submitted_at,commit:.commit_id,body:.body}' |
  jq -s . >"$run/reviews.json"
gh api --paginate "repos/$repo/issues/$pr/comments" \
  --jq '.[]|select(.user.login=="'"$bot"'")|{kind:"comment",at:.created_at,updated:.updated_at,body:.body}' |
  jq -s . >"$run/comments.json"

jq -rs '[.[].data.repository.pullRequest.reviewThreads] | "threads: totalCount=\(.[0].totalCount) fetched=\([.[].nodes[]]|length) hasNextPage=\(.[-1].pageInfo.hasNextPage)"' "$run"/threads_*.json
echo "scanned: $(jq length "$run/reviews.json") bot reviews ($(jq '[.[]|select(.body|test("shields.io/badge/P"))]|length' "$run/reviews.json") with P-badge bodies), $(jq length "$run/comments.json") bot comments"
jq -s --arg bot "$bot_bare" '[.[].data.repository.pullRequest.reviewThreads.nodes] | flatten
  | map(select(.isResolved == false and (.comments.nodes[0].author.login | sub("\\[bot\\]$";"")) == $bot))
  | map({thread: .id, path: .comments.nodes[0].path, line: .comments.nodes[0].line, body: .comments.nodes[0].body})' \
  "$run"/threads_*.json >"$run/unresolved.json"
echo "-- unresolved bot threads:"
jq '.[]' "$run/unresolved.json"
echo "-- P-badge review bodies:"
jq '.[] | select(.body|test("shields.io/badge/P")) | {commit: (.commit//"")[0:10], at, body}' "$run/reviews.json"
echo "-- comments:"
jq -r '.[] | "\(.at) upd=\(.updated) \(.body[0:200]|gsub("\n";" "))"' "$run/comments.json"
unresolved=$(jq length "$run/unresolved.json")
jq -rn --arg head "$head" --arg marker "$marker" --arg dry "$dry" --arg err "$err" --arg status "$status" --arg since "$since" --argjson unresolved "$unresolved" '
  ([inputs]|add|map(. + {
      assessed: (((((.body//"")|capture($marker+"[^`]*`(?<c>[0-9a-f]{7,40})").c) // ((.body//"")|capture("/blob/(?<c>[0-9a-f]{7,40})/").c) // .commit) // "")[0:10]),
      is_dry: ((.body//"")|test($dry)), is_err: ((.body//"")|test($err)),
      is_status: ((.body//"")|test($status)), has_badge: ((.body//"")|test("shields.io/badge/P"))})) as $all
  | ($all|map(select(.assessed[0:7]==$head[0:7] and .at > $since))) as $round
  | ($round|map(select(.has_badge or (.kind=="comment" and (.is_dry|not) and (.is_err|not) and (.is_status|not))))) as $fi
  | {assessed_commit: ($all|map(select(.is_status|not))|sort_by(.at)|last|.assessed),
     on_head: (($all|map(select(.assessed[0:7]==$head[0:7]))|length) > 0),
     findings: ($fi|length),
     unresolved_threads: $unresolved,
     dry_verdict: (($round|map(.is_dry)|any) and ($fi|length) == 0 and $unresolved == 0),
     errors_since_trigger: ($all|map(select(.at > $since and .is_err))|length)}' "$run/reviews.json" "$run/comments.json"
