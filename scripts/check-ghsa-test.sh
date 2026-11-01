#!/usr/bin/env bash
# Tests for scripts/check-ghsa.py, the audit task's GHSA-only advisory check.
# Run by CI's audit job before the audit itself, and by `mise run
# test-check-ghsa`.
#
# No network and no cargo: each case is a directory handed to the script's one
# test injection point (CHECK_GHSA_FIXTURE, documented at the top of
# check-ghsa.py). The metadata and advisory JSON have the shape of the real
# `cargo metadata` and GitHub /advisories answers (ghsa_id, severity, summary,
# references as a list of URLs, withdrawn_at, vulnerabilities[].package.name,
# vulnerabilities[].first_patched_version), copied from a live
# `gh api -X GET /advisories -f ecosystem=rust -f affects=zip@9.0.0`. The real
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

# advisory <ghsa id> <references json> [withdrawn_at json] [package]
advisory() {
  printf '{"ghsa_id":"%s","cve_id":null,"summary":"Zip Cloaking","type":"reviewed","severity":"critical","references":%s,"withdrawn_at":%s,"vulnerabilities":[{"package":{"ecosystem":"rust","name":"%s"},"vulnerable_version_range":"<= 9.0.4","first_patched_version":"9.0.5","vulnerable_functions":[]}]}' \
    "$1" "$2" "${3:-null}" "${4:-zip}"
}

# response <fixture dir> <k> <body json> [Link header]
response() {
  local link=${4:-}
  link=${link//\"/\\\"} # the header's rel="next" quotes, escaped for JSON
  printf '{"headers":{"Link":"%s"},"body":%s}\n' "$link" "$3" >"$1/response-$2.json"
}

# fixture [<name version> ...]: metadata.json for these registry packages,
# plus a path package and a git package that must not be asked about, and an
# empty-ignore deny.toml (a case that needs ignores overwrites it).
fixture() {
  local dir pkgs="" sep=""
  dir=$(mktemp -d "$tmp/case.XXXXXX")
  while [[ $# -ge 2 ]]; do
    pkgs+="$sep{\"name\":\"$1\",\"version\":\"$2\",\"source\":\"registry+https://github.com/rust-lang/crates.io-index\"}"
    sep=","
    shift 2
  done
  printf '{"packages":[{"name":"lex-core","version":"0.1.0","source":null},{"name":"gitdep","version":"0.1.0","source":"git+file:///x"},%s],"workspace_members":[]}\n' \
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
  CHECK_GHSA_FIXTURE=$dir python3 -I "$check" >"$tmp/out" 2>&1 || rc=$?
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

# --- what is reported ---
dir=$(fixture zip 9.0.0 serde 1.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
expect "GHSA-only advisory fails" fail \
  "ghsa: FAIL zip@9.0.0 — $ZIP_GHSA (critical) Zip Cloaking — patched: 9.0.5" "$dir"
expect "  and the summary counts it" fail "ghsa: FAIL — 2 deps, 0 withdrawn, 0 also in RustSec (cargo-deny's), 0 ignored, 1 found" "$dir"
grep -qF "affects=serde@1.0.0,zip@9.0.0" "$dir/requests.log" ||
  { echo "FAIL query lists name@version pairs (and no path/git package)"; cat "$dir/requests.log"; fails=$((fails + 1)); }

# --- what is left to cargo-deny, or not a finding ---
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$RUSTSEC_REFS")]"
expect "advisory referencing rustsec.org is left to cargo-deny" 0 "1 also in RustSec" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$RUSTSEC_REPO_REFS")]"
expect "advisory referencing the advisory-db repo is left to cargo-deny" 0 "1 also in RustSec" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS" '"2026-10-30T00:00:00Z"')]"
expect "withdrawn advisory is not a finding" 0 "1 withdrawn" "$dir"

# --- ignore list (engine/deny.toml's [advisories] ignore) ---
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
printf '[advisories]\nignore = [\n  { id = "RUSTSEC-2025-0141", reason = "x" },\n  { id = "%s", reason = "accepted" },\n]\n' "$ZIP_GHSA" >"$dir/deny.toml"
expect "GHSA id in the ignore list does not fail" 0 "ghsa: ignored $ZIP_GHSA" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
printf '[advisories]\nignore = ["GHSA-aaaa-bbbb-cccc", "RUSTSEC-2025-0141"]\n' >"$dir/deny.toml"
expect "an ignore list naming other ids does not hide it" fail "ghsa: FAIL zip@9.0.0" "$dir"
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[]"
printf '[advisories\n' >"$dir/deny.toml"
expect "unreadable deny.toml fails" fail "cannot read the ignore list" "$dir"

# --- no findings ---
dir=$(fixture zip 9.0.5)
response "$dir" 0 "[]"
expect "no advisories passes" 0 "ghsa: ok — 1 deps, 0 withdrawn, 0 also in RustSec (cargo-deny's), 0 ignored, 0 found" "$dir"

# --- failing closed ---
dir=$(fixture zip 9.0.0)
expect "request failure fails" fail "request failed" "$dir"
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

# --- paging and chunking ---
dir=$(fixture zip 9.0.0)
response "$dir" 0 "[]" '<https://api.github.com/advisories?page=2>; rel="next", <https://api.github.com/advisories?page=9>; rel="last"'
response "$dir" 1 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
expect "an advisory on the second page is found" fail "ghsa: FAIL zip@9.0.0" "$dir"
grep -qxF "https://api.github.com/advisories?page=2" "$dir/requests.log" ||
  { echo "FAIL the next link is the URL requested"; cat "$dir/requests.log"; fails=$((fails + 1)); }

dir=$(fixture $(for i in $(seq 1 150); do echo "c$i 1.0.0"; done))
response "$dir" 0 "[]"
response "$dir" 1 "[$(advisory $ZIP_GHSA "$GH_REFS" null c99)]"
expect "150 dependencies are asked in two chunks" fail "ghsa: FAIL c99@1.0.0" "$dir"
[[ $(wc -l <"$dir/requests.log") -eq 2 ]] ||
  { echo "FAIL 150 dependencies make two requests"; cat "$dir/requests.log"; fails=$((fails + 1)); }

# A crate locked at two versions is never asked about twice in one request, so
# a match is the version of that request: zip@8.0.0 shares request 0 with
# serde, zip@9.0.0 goes to request 1, and only the latter is named.
dir=$(fixture zip 8.0.0 zip 9.0.0 serde 1.0.0)
response "$dir" 0 "[]"
response "$dir" 1 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
expect "two locked versions: only the affected one is named" fail "ghsa: FAIL zip@9.0.0" "$dir"
if grep -qF "zip@8.0.0 —" "$tmp/out"; then
  echo "FAIL two locked versions: the unaffected one is named"
  fails=$((fails + 1))
fi
[[ $(sed -n 1p "$dir/requests.log") == *"affects=serde@1.0.0,zip@8.0.0&per_page=100" && \
   $(sed -n 2p "$dir/requests.log") == *"affects=zip@9.0.0&per_page=100" && \
   $(wc -l <"$dir/requests.log") -eq 2 ]] ||
  { echo "FAIL two locked versions: requests are not [serde, zip@8] then [zip@9]"; cat "$dir/requests.log"; fails=$((fails + 1)); }
# The older version affected instead: the same layers, the other attribution.
dir=$(fixture zip 8.0.0 zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
response "$dir" 1 "[]"
expect "two locked versions: the first version's match names only it" fail "ghsa: FAIL zip@8.0.0" "$dir"
if grep -qF "zip@9.0.0 —" "$tmp/out"; then
  echo "FAIL two locked versions: the other version is named"
  fails=$((fails + 1))
fi
# Both versions affected: one advisory, two names in the report.
dir=$(fixture zip 8.0.0 zip 9.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
response "$dir" 1 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
expect "two locked versions, both affected: both are named" fail "ghsa: FAIL zip@9.0.0" "$dir"
grep -qF "ghsa: FAIL zip@8.0.0" "$tmp/out" ||
  { echo "FAIL both affected: zip@8.0.0 not named"; fails=$((fails + 1)); }
# An advisory naming no crate of the request it came back for is not attributable.
dir=$(fixture serde 1.0.0)
response "$dir" 0 "[$(advisory $ZIP_GHSA "$GH_REFS")]"
expect "advisory naming none of the asked crates fails" fail "names none of the crates asked about" "$dir"

if [[ $fails -gt 0 ]]; then
  echo "check-ghsa-test: $fails failed"
  exit 1
fi
echo "check-ghsa-test: all passed"
