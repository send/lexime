#!/usr/bin/env bash
# Tests for scripts/check-ghsa.py, the audit task's GHSA-only advisory check.
# Run unconditionally by CI's screen job (`bash scripts/check-ghsa-test.sh`)
# and locally by `mise run test-check-ghsa`. CHECK_GHSA_PY picks the
# interpreter (default python3); the script must run on macOS's 3.9.
#
# No network, no cargo, no gh: each case is a directory handed to the script's
# one test injection point (CHECK_GHSA_FIXTURE, documented at the top of
# check-ghsa.py). It replaces only urllib's opener, so header assembly, the host
# check, retries, the 4xx cut-off, Link following and JSON parsing are the
# production code. The metadata and advisory JSON have the shape of the real
# `cargo metadata` and GitHub /advisories answers (ghsa_id, severity, summary,
# references as a list of URLs, withdrawn_at, vulnerabilities[].package.name,
# vulnerabilities[].first_patched_version), copied from a live
# `gh api -X GET /advisories -f ecosystem=rust -f affects=zip@9.0.0` (and, for
# the malware shape, `-f type=malware -f affects=arrayref`). The real
# path (cargo + the live API) is exercised by `mise run audit`.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
check="$repo/scripts/check-ghsa.py"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

ZIP_GHSA="GHSA-m665-v7pm-7grj"
GH_REFS='["https://github.com/zip-rs/zip2/security/advisories/GHSA-m665-v7pm-7grj","https://nvd.nist.gov/vuln/detail/CVE-2026-115905","https://github.com/advisories/GHSA-m665-v7pm-7grj"]'
RUSTSEC_REFS='["https://rustsec.org/advisories/RUSTSEC-2026-0001.html","https://github.com/advisories/GHSA-m665-v7pm-7grj"]'
RUSTSEC_REPO_REFS='["https://github.com/rustsec/advisory-db/blob/main/crates/zip/RUSTSEC-2026-0001.md"]'
# The advisory-db issue a malware report links: no RUSTSEC id, cargo-deny never sees it.
MALWARE_GHSA="GHSA-jwh4-228v-r358"
MALWARE_REFS='["https://github.com/rustsec/advisory-db/issues/3161","https://github.com/ossf/malicious-packages/blob/3b4ff66ce44ef274813534c270e0984580ad4f11/osv/malicious/crates.io/arrayref/MAL-2026-14336.json","https://safedep.io/arrayref-proc-macro1-rust-build-time-malware","https://github.com/advisories/GHSA-jwh4-228v-r358"]'
PY=${CHECK_GHSA_PY:-python3}

# advisory <ghsa id> <references json> [withdrawn_at json] [package]
advisory() {
  printf '{"ghsa_id":"%s","cve_id":null,"summary":"Zip Cloaking","type":"reviewed","severity":"critical","references":%s,"withdrawn_at":%s,"vulnerabilities":[{"package":{"ecosystem":"rust","name":"%s"},"vulnerable_version_range":"<= 9.0.4","first_patched_version":"9.0.5","vulnerable_functions":[]}]}' \
    "$1" "$2" "${3:-null}" "${4:-zip}"
}

# advisory2 <ghsa id> <references json>: one advisory naming both zip and serde
advisory2() {
  printf '{"ghsa_id":"%s","cve_id":null,"summary":"Two crates","type":"reviewed","severity":"high","references":%s,"withdrawn_at":null,"vulnerabilities":[{"package":{"ecosystem":"rust","name":"zip"},"vulnerable_version_range":"<= 9.0.4","first_patched_version":"9.0.5","vulnerable_functions":[]},{"package":{"ecosystem":"rust","name":"serde"},"vulnerable_version_range":"< 1.0.5","first_patched_version":"1.0.5","vulnerable_functions":[]}]}' \
    "$1" "$2"
}

# malware_advisory: the shape of type=malware's answer for arrayref (null patch)
malware_advisory() {
  printf '{"ghsa_id":"%s","cve_id":null,"summary":"Malicious code in arrayref (crates.io)","type":"malware","severity":"critical","references":%s,"published_at":"2026-08-21T00:31:25Z","withdrawn_at":null,"vulnerabilities":[{"package":{"ecosystem":"rust","name":"arrayref"},"vulnerable_version_range":"= 0.3.10","first_patched_version":null,"vulnerable_functions":[]}]}' \
    "$MALWARE_GHSA" "$MALWARE_REFS"
}

# response <fixture dir> <k> <body json> [Link header]: the k-th attempt gets 200
response() {
  local link=${4:-}
  link=${link//\"/\\\"} # the header's rel="next" quotes, escaped for JSON
  printf '{"status":200,"headers":{"Link":"%s"},"body":%s}\n' "$link" "$3" >"$1/response-$2.json"
}

# fail_response <fixture dir> <k> <status> [<remaining>]: an error answer in the
# shape of the API's (message body; X-RateLimit-Remaining when given)
fail_response() {
  local rl=""
  [[ -n ${4:-} ]] && rl=",\"X-RateLimit-Remaining\":\"$4\""
  printf '{"status":%s,"headers":{"Content-Type":"application/json; charset=utf-8"%s},"body":{"message":"API rate limit exceeded for 203.0.113.7.","documentation_url":"https://docs.github.com/rest/overview/resources-in-the-rest-api#rate-limiting","status":"%s"}}\n' \
    "$3" "$rl" "$3" >"$1/response-$2.json"
}

# raise_response <fixture dir> <k> <IncompleteRead|URLError|TimeoutError>
raise_response() {
  printf '{"raise":"%s"}\n' "$3" >"$1/response-$2.json"
}

# fixture [<name version> ...]: metadata.json for these registry packages,
# plus a path package and a git package that must not be asked about, and an
# empty-ignore deny.toml (a case that needs ignores overwrites it).
fixture() {
  local dir pkgs=""
  dir=$(mktemp -d "$tmp/case.XXXXXX")
  while [[ $# -ge 2 ]]; do
    pkgs+=",{\"name\":\"$1\",\"version\":\"$2\",\"source\":\"registry+https://github.com/rust-lang/crates.io-index\"}"
    shift 2
  done
  printf '{"packages":[{"name":"lex-core","version":"0.1.0","source":null},{"name":"gitdep","version":"0.1.0","source":"git+file:///x"}%s],"workspace_members":[]}\n' \
    "$pkgs" >"$dir/metadata.json"
  printf '[advisories]\n' >"$dir/deny.toml"
  echo "$dir"
}

# expect <name> <want exit: 0 or fail> <want text in the output> <dir>
expect() {
  local name=$1 want=$2 line=$3 dir=$4 rc=0
  if [[ ! -d $dir ]]; then
    echo "FAIL $name"
    echo "     no fixture directory"
    fails=$((fails + 1))
    return
  fi
  # No token from the caller's environment, unless the case sets CASE_TOKEN.
  env -u GITHUB_TOKEN -u GH_TOKEN ${CASE_TOKEN:+GITHUB_TOKEN=$CASE_TOKEN} \
    CHECK_GHSA_FIXTURE=$dir "$PY" -I "$check" >"$tmp/out" 2>&1 || rc=$?
  if { [[ $want == 0 && $rc -eq 0 ]] || [[ $want == fail && $rc -ne 0 ]]; } &&
    grep -qF -- "$line" "$tmp/out"; then
    echo "ok   $name"
  else
    echo "FAIL $name"
    echo "     want: exit $want, output containing: $line"
    echo "     got:  exit $rc, output: $(cat "$tmp/out")"
    fails=$((fails + 1))
  fi
}

# Each chunk is asked twice, `reviewed` then `malware`: for one chunk,
# response 0 is the reviewed answer and response 1 the malware answer.

# --- what is reported ---
dir=$(fixture zip 9.0.0 serde 1.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
response "$dir" 1 "[]"
expect "GHSA-only advisory fails" fail \
  "ghsa: FAIL zip@9.0.0 — $ZIP_GHSA (critical) Zip Cloaking — patched: 9.0.5" "$dir"
expect "  and the summary counts it" fail "ghsa: FAIL — 2 deps, 0 withdrawn, 0 also in RustSec (cargo-deny's), 0 ignored, 1 found" "$dir"
grep -qF "affects=serde@1.0.0,zip@9.0.0" "$dir/requests.log" ||
  { echo "FAIL query lists name@version pairs (and no path/git package)"; cat "$dir/requests.log"; fails=$((fails + 1)); }

# malware is only returned for type=malware (the default is reviewed alone)
dir=$(fixture arrayref 0.3.10)
response "$dir" 0 "[]"
response "$dir" 1 "[$(malware_advisory)]"
expect "malware advisory fails, and is marked as malware" fail \
  "ghsa: FAIL arrayref@0.3.10 — $MALWARE_GHSA (critical, malware) Malicious code in arrayref (crates.io) — patched: none" "$dir"
[[ $(sed -n 1p "$dir/requests.log") == *"type=reviewed"* && $(sed -n 2p "$dir/requests.log") == *"type=malware"* && \
   $(wc -l <"$dir/requests.log") -eq 2 ]] ||
  { echo "FAIL a chunk is asked as type=reviewed then type=malware"; cat "$dir/requests.log"; fails=$((fails + 1)); }

# --- what is left to cargo-deny, or not a finding ---
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$RUSTSEC_REFS")]"
response "$dir" 1 "[]"
expect "advisory referencing rustsec.org is left to cargo-deny" 0 "1 also in RustSec" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$RUSTSEC_REPO_REFS")]"
response "$dir" 1 "[]"
expect "advisory-db blob with a RUSTSEC id is left to cargo-deny" 0 "1 also in RustSec" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA '["https://github.com/rustsec/advisory-db/issues/3161","https://github.com/rustsec/advisory-db/pull/3200"]')]"
response "$dir" 1 "[]"
expect "advisory-db issue/pull links alone are not RustSec's" fail "ghsa: FAIL zip@9.0.0" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS" '"2026-10-30T00:00:00Z"')]"
response "$dir" 1 "[]"
expect "withdrawn advisory is not a finding" 0 "1 withdrawn" "$dir"

# --- ignore list (engine/deny.toml's [advisories] ignore) ---
# zip_case <deny.toml text>: zip@9.0.0 with the GHSA advisory, that deny.toml
zip_case() {
  dir=$(fixture zip 9.0.0)
  response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
  response "$dir" 1 "[]"
  printf '%s\n' "$1" >"$dir/deny.toml"
}
zip_case "$(printf '[advisories]\nignore = [\n  { id = "RUSTSEC-2025-0141", reason = "x" },\n  { id = "%s", reason = "accepted" },\n]' "$ZIP_GHSA")"
expect "GHSA id in the ignore list does not fail" 0 "ghsa: ignored $ZIP_GHSA" "$dir"
zip_case "$(printf '[advisories]\nignore = ["%s", "RUSTSEC-2025-0141"]' "$ZIP_GHSA")"
expect "a bare GHSA string in the ignore list does not fail" 0 "ghsa: ignored $ZIP_GHSA" "$dir"
zip_case "$(printf '[advisories]\nignore = [\n  "%s", # accepted\n]' "$ZIP_GHSA")"
expect "a bare GHSA string with a trailing comment does not fail" 0 "ghsa: ignored $ZIP_GHSA" "$dir"
zip_case "$(printf '[advisories]\nignore = [\n  { crate = "foo@1.2.3", reason = "x" },\n  { id = "%s", reason = "accepted" },\n]' "$ZIP_GHSA")"
expect "a { crate = .. } entry does not break reading the list" 0 "ghsa: ignored $ZIP_GHSA" "$dir"
zip_case '[advisories]
ignore = ["GHSA-aaaa-bbbb-cccc", "RUSTSEC-2025-0141"]'
expect "an ignore list naming other ids does not hide it" fail "ghsa: FAIL zip@9.0.0" "$dir"
zip_case "$(printf '[advisories]\nignore = [\n  # { id = "%s", reason = "was accepted" },\n]' "$ZIP_GHSA")"
expect "a commented-out id is not an ignore" fail "ghsa: FAIL zip@9.0.0" "$dir"
zip_case "$(printf '[advisories]\nignore = []\n\n[bans]\nskip = [\n  { id = "%s", reason = "x" },\n]' "$ZIP_GHSA")"
expect "an id outside [advisories] is not an ignore" fail "ghsa: FAIL zip@9.0.0" "$dir"
zip_case "$(printf '[advisories]\nignore = [\n  { id = "RUSTSEC-2025-0141", reason = "unrelated to %s" },\n]' "$ZIP_GHSA")"
expect "an id inside a reason string is not an ignore" fail "ghsa: FAIL zip@9.0.0" "$dir"
# The quoting spellings of a reason that merely mentions an id. A regex that
# read `["GHSA-.."]` anywhere in the section took each of them for an ignore.
zip_case "$(printf '[advisories]\nignore = [\n  { id = "RUSTSEC-2025-0141", reason = '"'"'superseded by ["%s"]'"'"' },\n]' "$ZIP_GHSA")"
expect "an id in a literal ('..') string is not an ignore" fail "ghsa: FAIL zip@9.0.0" "$dir"
zip_case "$(printf '[advisories]\nignore = [\n  { id = "RUSTSEC-2025-0141", reason = "see [\\"%s\\"]" },\n]' "$ZIP_GHSA")"
expect "an id in an escaped basic string is not an ignore" fail "ghsa: FAIL zip@9.0.0" "$dir"
zip_case "$(printf '[advisories]\nignore = [\n  { id = "RUSTSEC-2025-0141", reason = """\nwas ["%s"]\n""" },\n]' "$ZIP_GHSA")"
expect "an id in a multi-line basic string is not an ignore" fail "ghsa: FAIL zip@9.0.0" "$dir"
zip_case "$(printf "[advisories]\nignore = [\n  { id = 'RUSTSEC-2025-0141', reason = '''\nwas [\"%s\"]\n''' },\n]" "$ZIP_GHSA")"
expect "an id in a multi-line literal string is not an ignore" fail "ghsa: FAIL zip@9.0.0" "$dir"
zip_case "$(printf '[advisories]\nignore = [\n  { id = "RUSTSEC-2025-0141", reason = "a # b" }, # then\n  { id = "%s", reason = "x # y" },\n]' "$ZIP_GHSA")"
expect "a # inside a string is not a comment" 0 "ghsa: ignored $ZIP_GHSA" "$dir"
zip_case "$(printf "[advisories]\nignore = [\n  { id = '%s', reason = 'x' },\n]" "$ZIP_GHSA")"
expect "a literal-quoted id is an ignore" 0 "ghsa: ignored $ZIP_GHSA" "$dir"
zip_case "[advisories]"
rm "$dir/deny.toml"
expect "unreadable deny.toml fails" fail "cannot read the ignore list" "$dir"

# --- no findings ---
dir=$(fixture zip 9.0.5)
response "$dir" 0 "[]"
response "$dir" 1 "[]"
expect "no advisories passes" 0 "ghsa: ok — 1 deps, 0 withdrawn, 0 also in RustSec (cargo-deny's), 0 ignored, 0 found" "$dir"

# --- failing closed ---
dir=$(fixture zip 9.0.0)
expect "request failure fails" fail "request failed" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[]"
expect "a failing malware query fails" fail "request failed" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 '{"message":"Not Found"}'
expect "answer that is not a list fails" fail "not a list" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 '[{"ghsa_id":"GHSA-x","references":"nope"}]'
expect "advisory of the wrong shape fails" fail "unexpected advisory shape" "$dir"
dir=$(fixture zip 9.0.0)
printf 'not json' >"$dir/response-0.json"
expect "unparseable answer fails" fail "request failed" "$dir"
dir=$(fixture zip 9.0.0)
printf 'garbage' >"$dir/metadata.json"
expect "unreadable cargo metadata fails" fail "unreadable cargo metadata" "$dir"
dir=$(fixture)
expect "no registry dependencies fails" fail "no registry dependencies found" "$dir"

# --- paging and chunking ---
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[]" '<https://api.github.com/advisories?page=2>; rel="next", <https://api.github.com/advisories?page=9>; rel="last"'
response "$dir" 1 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
response "$dir" 2 "[]"
expect "an advisory on the second page is found" fail "ghsa: FAIL zip@9.0.0" "$dir"
grep -qxF "https://api.github.com/advisories?page=2" "$dir/requests.log" ||
  { echo "FAIL the next link is the URL requested"; cat "$dir/requests.log"; fails=$((fails + 1)); }

# 150 dependencies: chunk 0 is requests 0 (reviewed) and 1 (malware), chunk 1
# (which holds c99) is 2 and 3.
dir=$(fixture $(for i in $(seq 1 150); do echo "c$i 1.0.0"; done))
response "$dir" 0 "[]"
response "$dir" 1 "[]"
response "$dir" 2 "[$(advisory $ZIP_GHSA "$GH_REFS" null c99)]"
response "$dir" 3 "[]"
expect "150 dependencies are asked in two chunks" fail "ghsa: FAIL c99@1.0.0" "$dir"
[[ $(wc -l <"$dir/requests.log") -eq 4 ]] ||
  { echo "FAIL 150 dependencies make two chunks of two requests"; cat "$dir/requests.log"; fails=$((fails + 1)); }

# A crate locked at two versions is never asked about twice in one request, so
# a match is the version of that request: zip@8.0.0 shares requests 0 and 1
# with serde, zip@9.0.0 goes to 2 and 3, and only the latter is named.
dir=$(fixture zip 8.0.0 zip 9.0.0 serde 1.0.0)
response "$dir" 0 "[]"
response "$dir" 1 "[]"
response "$dir" 2 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
response "$dir" 3 "[]"
expect "two locked versions: only the affected one is named" fail "ghsa: FAIL zip@9.0.0" "$dir"
if grep -qF "zip@8.0.0 —" "$tmp/out"; then
  echo "FAIL two locked versions: the unaffected one is named"
  fails=$((fails + 1))
fi
[[ $(sed -n 1p "$dir/requests.log") == *"type=reviewed"*"affects=serde@1.0.0,zip@8.0.0&per_page=100" && \
   $(sed -n 2p "$dir/requests.log") == *"type=malware"*"affects=serde@1.0.0,zip@8.0.0&per_page=100" && \
   $(sed -n 3p "$dir/requests.log") == *"type=reviewed"*"affects=zip@9.0.0&per_page=100" && \
   $(sed -n 4p "$dir/requests.log") == *"type=malware"*"affects=zip@9.0.0&per_page=100" && \
   $(wc -l <"$dir/requests.log") -eq 4 ]] ||
  { echo "FAIL two locked versions: requests are not [serde, zip@8] then [zip@9], each reviewed then malware"; cat "$dir/requests.log"; fails=$((fails + 1)); }
# The older version affected instead: the same layers, the other attribution.
dir=$(fixture zip 8.0.0 zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
response "$dir" 1 "[]"
response "$dir" 2 "[]"
response "$dir" 3 "[]"
expect "two locked versions: the first version's match names only it" fail "ghsa: FAIL zip@8.0.0" "$dir"
if grep -qF "zip@9.0.0 —" "$tmp/out"; then
  echo "FAIL two locked versions: the other version is named"
  fails=$((fails + 1))
fi
# Both versions affected: one advisory, two names in the report.
dir=$(fixture zip 8.0.0 zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
response "$dir" 1 "[]"
response "$dir" 2 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
response "$dir" 3 "[]"
expect "two locked versions, both affected: both are named" fail "ghsa: FAIL zip@9.0.0" "$dir"
grep -qF "ghsa: FAIL zip@8.0.0" "$tmp/out" ||
  { echo "FAIL both affected: zip@8.0.0 not named"; fails=$((fails + 1)); }
# An advisory naming no crate of the request it came back for is not attributable.
dir=$(fixture serde 1.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
response "$dir" 1 "[]"
expect "advisory naming none of the asked crates fails" fail "names none of the crates asked about" "$dir"

# --- an advisory naming several crates of one chunk ---
# Request order: the chunk as reviewed, the chunk as malware, then (only for an
# advisory that is reported) each named crate alone, serde then zip. Only zip's
# own query returns it.
dir=$(fixture zip 9.0.0 serde 1.0.0)
response "$dir" 0 "[$(advisory2 $ZIP_GHSA "$GH_REFS")]"
response "$dir" 1 "[]"
response "$dir" 2 "[]"
response "$dir" 3 "[$(advisory2 $ZIP_GHSA "$GH_REFS")]"
expect "advisory naming two crates: only the one it is returned for alone is named" fail "ghsa: FAIL zip@9.0.0 — $ZIP_GHSA (high) Two crates" "$dir"
if grep -qF "serde@1.0.0 —" "$tmp/out"; then
  echo "FAIL two-crate advisory: the unaffected crate is named"
  fails=$((fails + 1))
fi
[[ $(sed -n 1p "$dir/requests.log") == *"type=reviewed"*"affects=serde@1.0.0,zip@9.0.0&"* && \
   $(sed -n 2p "$dir/requests.log") == *"type=malware"*"affects=serde@1.0.0,zip@9.0.0&"* && \
   $(sed -n 3p "$dir/requests.log") == *"type=reviewed"*"affects=serde@1.0.0&per_page=100" && \
   $(sed -n 4p "$dir/requests.log") == *"type=reviewed"*"affects=zip@9.0.0&per_page=100" && \
   $(wc -l <"$dir/requests.log") -eq 4 ]] ||
  { echo "FAIL two-crate advisory: request order"; cat "$dir/requests.log"; fails=$((fails + 1)); }
# Neither crate alone returns it: not attributable, so a FAIL.
dir=$(fixture zip 9.0.0 serde 1.0.0)
response "$dir" 0 "[$(advisory2 $ZIP_GHSA "$GH_REFS")]"
response "$dir" 1 "[]"
response "$dir" 2 "[]"
response "$dir" 3 "[]"
expect "advisory naming two crates that neither alone returns fails" fail "cannot attribute" "$dir"
# Two advisories naming the same two crates: each pair is asked about once.
OTHER_GHSA="GHSA-aaaa-bbbb-cccc"
dir=$(fixture zip 9.0.0 serde 1.0.0)
response "$dir" 0 "[$(advisory2 $ZIP_GHSA "$GH_REFS"),$(advisory2 $OTHER_GHSA "$GH_REFS")]"
response "$dir" 1 "[]"
response "$dir" 2 "[$(advisory2 $ZIP_GHSA "$GH_REFS"),$(advisory2 $OTHER_GHSA "$GH_REFS")]"
response "$dir" 3 "[$(advisory2 $ZIP_GHSA "$GH_REFS"),$(advisory2 $OTHER_GHSA "$GH_REFS")]"
expect "two advisories on the same two crates are both reported" fail "$OTHER_GHSA" "$dir"
[[ $(wc -l <"$dir/requests.log") -eq 4 ]] ||
  { echo "FAIL attribution is asked once per pair, not once per advisory"; cat "$dir/requests.log"; fails=$((fails + 1)); }
# An advisory that is not a finding is not attributed: no requests beyond the chunk's.
for kind in withdrawn rustsec ignored; do
  dir=$(fixture zip 9.0.0 serde 1.0.0)
  case $kind in
    withdrawn) adv=$(advisory2 $ZIP_GHSA "$GH_REFS" | sed 's/"withdrawn_at":null/"withdrawn_at":"2026-10-30T00:00:00Z"/') ;;
    rustsec) adv=$(advisory2 $ZIP_GHSA "$RUSTSEC_REFS") ;;
    ignored)
      adv=$(advisory2 $ZIP_GHSA "$GH_REFS")
      printf '[advisories]\nignore = ["%s"]\n' "$ZIP_GHSA" >"$dir/deny.toml"
      ;;
  esac
  response "$dir" 0 "[$adv]"
  response "$dir" 1 "[]"
  expect "a $kind two-crate advisory passes without attribution" 0 "ghsa: ok" "$dir"
  [[ $(wc -l <"$dir/requests.log") -eq 2 ]] ||
    { echo "FAIL a $kind advisory was attributed (extra requests)"; cat "$dir/requests.log"; fails=$((fails + 1)); }
done

# --- the HTTP layer (production code over a fake opener) ---
# A 500 then a 200: retried.
dir=$(fixture zip 9.0.5)
fail_response "$dir" 0 500
response "$dir" 1 "[]"
response "$dir" 2 "[]"
expect "a 500 is retried" 0 "ghsa: ok" "$dir"
# IncompleteRead (an HTTPException) then a 200: retried.
dir=$(fixture zip 9.0.5)
raise_response "$dir" 0 IncompleteRead
response "$dir" 1 "[]"
response "$dir" 2 "[]"
expect "an IncompleteRead is retried" 0 "ghsa: ok" "$dir"
dir=$(fixture zip 9.0.5)
raise_response "$dir" 0 URLError
raise_response "$dir" 1 TimeoutError
response "$dir" 2 "[]"
response "$dir" 3 "[]"
expect "a URLError and a timeout are retried" 0 "ghsa: ok" "$dir"
# 429 on every attempt: ATTEMPTS (3) requests, then FAIL.
dir=$(fixture zip 9.0.5)
for k in 0 1 2 3; do fail_response "$dir" $k 429; done
expect "a 429 is retried ATTEMPTS times, then fails" fail "request failed" "$dir"
[[ $(wc -l <"$dir/requests.log") -eq 3 ]] ||
  { echo "FAIL 429: want 3 attempts"; cat "$dir/requests.log"; fails=$((fails + 1)); }
# A 404 is not asked again.
dir=$(fixture zip 9.0.5)
for k in 0 1 2; do fail_response "$dir" $k 404; done
expect "a 404 stops at the first attempt and fails" fail "HTTP 404" "$dir"
[[ $(wc -l <"$dir/requests.log") -eq 1 ]] ||
  { echo "FAIL 404: want 1 attempt"; cat "$dir/requests.log"; fails=$((fails + 1)); }
# A Link to another host is refused (the token must not follow it).
dir=$(fixture zip 9.0.5)
response "$dir" 0 "[]" '<https://evil.example/advisories?page=2>; rel="next"'
response "$dir" 1 "[]"
expect "a next link to another host fails" fail "refusing to request https://evil.example/advisories?page=2" "$dir"
[[ $(wc -l <"$dir/requests.log") -eq 1 ]] ||
  { echo "FAIL a foreign Link was requested"; cat "$dir/requests.log"; fails=$((fails + 1)); }
# A body that is not JSON is its own message.
dir=$(fixture zip 9.0.5)
printf '{"status":200,"headers":{},"raw":"<html>"}\n' >"$dir/response-0.json"
expect "a non-JSON body fails" fail "answer is not JSON" "$dir"

# --- the token ---
# Authorization goes in unredirected (so a redirect does not carry it), and
# not among the ordinary headers.
dir=$(fixture zip 9.0.5)
response "$dir" 0 "[]"
response "$dir" 1 "[]"
CASE_TOKEN=test-token-not-real expect "a token is sent" 0 "ghsa: ok" "$dir"
grep -qxF 'U {"Authorization": "Bearer test-token-not-real"}' "$dir/request-headers.log" ||
  { echo "FAIL the token is not an unredirected Authorization header"; cat "$dir/request-headers.log"; fails=$((fails + 1)); }
if grep -q '^H .*Authorization' "$dir/request-headers.log"; then
  echo "FAIL the token is also among the redirect-following headers"
  fails=$((fails + 1))
fi
# No token (and the fixture mode never runs gh): anonymous, nothing sent.
dir=$(fixture zip 9.0.5)
response "$dir" 0 "[]"
response "$dir" 1 "[]"
expect "no token is an anonymous run" 0 "ghsa: ok" "$dir"
grep -qxF 'U {}' "$dir/request-headers.log" ||
  { echo "FAIL an anonymous run sent an Authorization header"; cat "$dir/request-headers.log"; fails=$((fails + 1)); }
# The quota is gone: 403 with X-RateLimit-Remaining 0, and no retry.
dir=$(fixture zip 9.0.5)
fail_response "$dir" 0 403 0
expect "403 with no quota left says so (anonymous)" fail \
  'HTTP 403, API rate limit exhausted; set GITHUB_TOKEN/GH_TOKEN or `gh auth login` (this run: anonymous)' "$dir"
[[ $(wc -l <"$dir/requests.log") -eq 1 ]] ||
  { echo "FAIL rate limit: want 1 attempt"; cat "$dir/requests.log"; fails=$((fails + 1)); }
dir=$(fixture zip 9.0.5)
fail_response "$dir" 0 403 0
CASE_TOKEN=test-token-not-real expect "403 with no quota left says so (with a token)" fail "(this run: token from GITHUB_TOKEN)" "$dir"
# A 403 with quota left is not a rate limit.
dir=$(fixture zip 9.0.5)
fail_response "$dir" 0 403 59
expect "403 with quota left is a plain failure" fail "HTTP 403" "$dir"
if grep -qF "rate limit exhausted" "$tmp/out"; then
  echo "FAIL a 403 with quota left was called a rate limit"
  fails=$((fails + 1))
fi

# --- unused ignore ---
dir=$(fixture zip 9.0.5)
response "$dir" 0 "[]"
response "$dir" 1 "[]"
printf '[advisories]\nignore = [{ id = "%s", reason = "x" }]\n' "$OTHER_GHSA" >"$dir/deny.toml"
expect "an ignore that matches nothing warns, and does not fail" 0 \
  "ghsa: warning — ignore $OTHER_GHSA (deny.toml) matched no advisory for the locked versions; remove it?" "$dir"
zip_case "$(printf '[advisories]\nignore = ["%s"]' "$ZIP_GHSA")"
expect "an ignore that matched is not warned about" 0 "ghsa: ignored $ZIP_GHSA" "$dir"
if grep -qF "ghsa: warning" "$tmp/out"; then
  echo "FAIL a used ignore was warned about"
  fails=$((fails + 1))
fi

if [[ $fails -gt 0 ]]; then
  echo "check-ghsa-test: $fails failed"
  exit 1
fi
echo "check-ghsa-test: all passed"
