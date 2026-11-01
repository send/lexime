#!/usr/bin/env python3
# Fail on a GitHub Security Advisory (GHSA) that names a dependency version in
# engine/Cargo.lock and that RustSec does not carry. Run by [tasks.audit]
# (mise.toml) right after cargo-deny; tests in scripts/check-ghsa-test.sh.
#
# Why: cargo-deny reads the RustSec database only, so an advisory published to
# GitHub alone is invisible to it. GHSA-m665-v7pm-7grj (zip <= 9.0.4) sat in
# main unnoticed for that reason (#402); #401 could only pin a floor by hand.
# This asks the GitHub Advisory API about the lock's (name, version) pairs,
# in chunks that never hold one crate twice, so that an advisory matched in a
# chunk is about the one locked version of each crate it names there (no
# version-range evaluation here). An advisory that names two or more crates of
# one chunk is attributed by asking about each of those pairs alone.
#
# Advisory types: `reviewed` and `malware`, two queries per chunk (the API's
# default is `reviewed` alone, which leaves malware out: GHSA-jwh4-228v-r358,
# arrayref 0.3.10, is only returned for type=malware). `unreviewed` is not
# asked: its package/version mapping is not checked by GitHub, so `affects`
# matching on it cannot be relied on, and it would be noise in a gate.
#
# What it reports and what it leaves out:
#   - A GHSA that references a RustSec advisory (a rustsec.org/advisories/
#     RUSTSEC-* page, or a RUSTSEC-yyyy-nnnn file in the advisory-db repo) is
#     left to cargo-deny, which already fails on it; reporting both would make
#     one finding two failures to ignore, in two places. A link to an
#     advisory-db issue or pull request is not that: the advisory-db issue for
#     a malware report carries no RUSTSEC id, and cargo-deny does not see it.
#   - A withdrawn advisory is not a finding.
#   - An id under `[advisories] ignore` in engine/deny.toml is reported as
#     ignored and does not fail. That list is the one place to accept an
#     advisory: cargo-deny only warns about a GHSA id there ("unknown
#     advisory", exit 0), so it can hold ours too. The list is read by hand
#     (see ignored_ids): Python 3.9 (macOS's /usr/bin/python3) has no tomllib.
#
# What it asks about: every registry dependency in the lock, each run, as
# cargo reads it (`cargo metadata`, which resolves without building; a hand
# parser would skip spellings cargo accepts, #345). Not a diff against main:
# a new advisory lands against a dependency that has been in the lock for
# months.
#
# Failing closed: a request that fails, or an answer that is not the shape the
# API documents, is a FAIL, never a pass. What cannot be checked is not clean
# (the same stance as scripts/check-quarantine.sh).
#
# Network: unauthenticated the API allows 60 requests/hour, which a run (two
# requests per 100 dependencies, plus one per crate of an advisory that names
# several) stays far under; GITHUB_TOKEN / GH_TOKEN,
# when set, only raises that limit and is sent to api.github.com alone.
#
# Test injection (the one, for scripts/check-ghsa-test.sh only): with
# CHECK_GHSA_FIXTURE=<dir> set, nothing touches the network or cargo.
#   <dir>/metadata.json     stands in for `cargo metadata` output
#   <dir>/deny.toml         stands in for engine/deny.toml (required)
#   <dir>/response-<k>.json the k-th request's answer (0-based, in request
#                           order): {"headers": {"Link": ...}, "body": <API body>}
#                           A missing file is a request failure.
#   <dir>/requests.log      every requested URL is appended here
# CI never sets it; a run that does says so on its first line.
import json
import os
import re
import http.client
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

API = "https://api.github.com"
# Pairs per request. Not an API limit (151 entries still answer); the limit is the
# URL's length (HTTP 414), and 100 keeps it comfortably short. per_page caps at 100.
CHUNK = 100
TYPES = ("reviewed", "malware")
MAX_PAGES = 50  # a runaway Link chain is a bug, not data
ATTEMPTS = 3
TIMEOUT = 30
USER_AGENT = "lexime-ci (github.com/send/lexime)"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURE = os.environ.get("CHECK_GHSA_FIXTURE")


class Fail(Exception):
    """A reason the check could not be completed or trusted."""


def registry_deps():
    """Sorted (name, version) of every registry package in the lock."""
    if FIXTURE:
        with open(os.path.join(FIXTURE, "metadata.json"), "rb") as f:
            raw = f.read()
    else:
        proc = subprocess.run(
            ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1"],
            cwd=os.path.join(ROOT, "engine"),
            capture_output=True,
        )
        if proc.returncode != 0:
            raise Fail("cargo metadata failed: " + proc.stderr.decode(errors="replace").strip())
        raw = proc.stdout
    try:
        packages = json.loads(raw)["packages"]
        deps = sorted(
            {
                (p["name"], p["version"])
                for p in packages
                if (p.get("source") or "").startswith("registry+")
            }
        )
    except (ValueError, KeyError, TypeError, AttributeError) as e:
        raise Fail("unreadable cargo metadata output: %r" % e)
    if not deps:
        raise Fail("no registry dependencies found in cargo metadata (nothing was checked)")
    return deps


_HEADING = re.compile(r"^\s*\[")
_ID = r"GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4}"
# `id = "GHSA-..."` (an inline table's key), or a bare string element
# (`["GHSA-..", ..]` or one per line) that is not the value of some `key =`.
_IGNORED = re.compile(
    r'(?<![\w-])id\s*=\s*"(%s)"|(?:^|[\[,])\s*"(%s)"\s*(?=[,\]]|$)' % (_ID, _ID), re.M
)


def ignored_ids():
    """GHSA ids under [advisories] in deny.toml, picked out by hand.

    Not a TOML parse (no tomllib before Python 3.10, and the supported macOS
    ships 3.9). A miss only ever errs toward "not ignored", i.e. stricter, so
    this need not read every TOML spelling: an entry it cannot see is a FAIL
    the author then writes in the plain form. The other way (picking up an id
    that is not an ignore) is kept narrow instead: only the [advisories]
    section, comments dropped, and only `id = "GHSA-.."` or a bare string.
    Entries like { crate = "foo@1.2.3", reason = ".." } are simply not picked.
    """
    path = os.path.join(FIXTURE, "deny.toml") if FIXTURE else os.path.join(ROOT, "engine", "deny.toml")
    try:
        with open(path, encoding="utf-8") as f:
            lines = f.read().splitlines()
    except (OSError, ValueError) as e:
        raise Fail("cannot read the ignore list in %s: %r" % (path, e))
    section, kept = None, []
    for line in lines:
        if _HEADING.match(line) and re.match(r"^\s*\[+[^\]]*\]+\s*(#.*)?$", line):
            section = line.split("#")[0].strip()
            continue
        if section == "[advisories]":
            kept.append(line.split("#")[0])
    return {a or b for a, b in _IGNORED.findall("\n".join(kept))}


_requests = 0


def http_get(url):
    """(Link header or "", parsed JSON body). Retries transient failures."""
    global _requests
    if FIXTURE:
        k = _requests
        _requests += 1
        with open(os.path.join(FIXTURE, "requests.log"), "a") as f:
            f.write(url + "\n")
        try:
            with open(os.path.join(FIXTURE, "response-%d.json" % k)) as f:
                doc = json.load(f)
            return (doc.get("headers") or {}).get("Link", ""), doc["body"]
        except (OSError, ValueError, KeyError, AttributeError) as e:
            raise Fail("request failed (fixture %d): %r" % (k, e))
    if not url.startswith(API + "/"):
        # Also keeps the token from following a Link to another host.
        raise Fail("refusing to request %s" % url)
    headers = {
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": USER_AGENT,
    }
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        headers["Authorization"] = "Bearer " + token
    last = None
    for attempt in range(ATTEMPTS):
        if attempt:
            time.sleep(2)
        try:
            req = urllib.request.Request(url, headers=headers)
            with urllib.request.urlopen(req, timeout=TIMEOUT) as resp:
                return resp.headers.get("Link", ""), json.loads(resp.read())
        except urllib.error.HTTPError as e:
            last = "HTTP %d" % e.code
            if e.code < 500 and e.code != 429:
                break  # a 4xx will not change by asking again
        except (urllib.error.URLError, TimeoutError, OSError, http.client.HTTPException) as e:
            last = repr(e)
        except ValueError as e:
            raise Fail("answer is not JSON (%s): %r" % (url, e))
    raise Fail("request failed (%s): %s" % (url, last))


def next_link(link):
    for part in link.split(","):
        m = re.match(r'\s*<([^>]*)>\s*;.*\brel="?next"?', part)
        if m:
            return m.group(1)
    return None


def advisories_affecting(pairs, kind):
    """Every advisory of this type the API lists for these (name, version) pairs."""
    query = urllib.parse.urlencode(
        {
            "ecosystem": "rust",
            "type": kind,
            "affects": ",".join("%s@%s" % p for p in pairs),
            "per_page": 100,
        },
        safe=",@",
    )
    url, found = "%s/advisories?%s" % (API, query), []
    for _ in range(MAX_PAGES):
        link, body = http_get(url)
        if not isinstance(body, list):
            raise Fail("unexpected answer from %s: not a list" % url)
        for adv in body:
            validate(adv)
        found += body
        url = next_link(link)
        if url is None:
            return found
    raise Fail("more than %d pages for one query" % MAX_PAGES)


def validate(adv):
    """The fields this reads, in the documented shape; anything else is a FAIL."""
    ok = (
        isinstance(adv, dict)
        and isinstance(adv.get("ghsa_id"), str)
        and isinstance(adv.get("references"), list)
        and all(isinstance(r, str) for r in adv["references"])
        and isinstance(adv.get("vulnerabilities"), list)
        and all(
            isinstance(v, dict) and isinstance(v.get("package"), dict) and isinstance(v["package"].get("name"), str)
            for v in adv["vulnerabilities"]
        )
    )
    if not ok:
        raise Fail("unexpected advisory shape: %.200s" % json.dumps(adv))


_RUSTSEC_ID = re.compile(r"RUSTSEC-\d{4}-\d{4}")


def covered_by_rustsec(adv):
    """References a RustSec advisory itself, not just the advisory-db repo."""
    return any(
        "rustsec.org/advisories/RUSTSEC-" in r
        or ("github.com/rustsec/advisory-db/" in r and _RUSTSEC_ID.search(r))
        for r in adv["references"]
    )


def chunks(deps):
    """Lists of at most CHUNK pairs, none holding a crate name twice.

    deps is sorted, so the n-th version of each name goes to layer n; a crate
    locked at two versions is asked about in two requests, never one.
    """
    layers, seen = [], {}
    for name, version in deps:
        n = seen[name] = seen.get(name, -1) + 1
        if n == len(layers):
            layers.append([])
        layers[n].append((name, version))
    return [layer[i : i + CHUNK] for layer in layers for i in range(0, len(layer), CHUNK)]


def patched(adv, name):
    for v in adv["vulnerabilities"]:
        if v["package"]["name"] == name and v.get("first_patched_version"):
            return v["first_patched_version"]
    return "none"


def main():
    if FIXTURE:
        print("ghsa: using test fixture %s (no network)" % FIXTURE)
    deps = registry_deps()
    ignore = ignored_ids()

    # ghsa_id -> (advisory, the locked (name, version) pairs it names). Within a
    # chunk a name is one version, so a single named crate gives the pair
    # directly; several are told apart by asking about each pair alone.
    seen = {}
    for chunk in chunks(deps):
        version_of = dict(chunk)
        for kind in TYPES:
            for adv in advisories_affecting(chunk, kind):
                ghsa_id = adv["ghsa_id"]
                names = sorted({v["package"]["name"] for v in adv["vulnerabilities"]} & version_of.keys())
                if not names:
                    raise Fail("advisory %s names none of the crates asked about" % ghsa_id)
                if len(names) > 1:
                    names = [
                        n
                        for n in names
                        if any(a["ghsa_id"] == ghsa_id for a in advisories_affecting([(n, version_of[n])], kind))
                    ]
                    if not names:
                        raise Fail("advisory %s names several crates but none alone (cannot attribute)" % ghsa_id)
                seen.setdefault(ghsa_id, (adv, set()))[1].update((n, version_of[n]) for n in names)

    withdrawn = rustsec = 0
    ignored, findings = [], []
    for ghsa_id, (adv, pairs) in sorted(seen.items()):
        if adv.get("withdrawn_at"):
            withdrawn += 1
        elif covered_by_rustsec(adv):
            rustsec += 1
        elif ghsa_id in ignore:
            ignored.append(adv)
        else:
            findings.append((adv, pairs))

    for adv in ignored:
        print("ghsa: ignored %s (deny.toml) — %s" % (adv["ghsa_id"], adv.get("summary", "")))
    count = 0
    for adv, pairs in findings:
        for name, version in sorted(pairs):
            count += 1
            label = adv.get("severity") or "unknown"
            if adv.get("type") == "malware":
                label += ", malware"
            print(
                "ghsa: FAIL %s@%s — %s (%s) %s — patched: %s"
                % (name, version, adv["ghsa_id"], label, adv.get("summary", ""), patched(adv, name))
            )
    summary = "%d deps, %d withdrawn, %d also in RustSec (cargo-deny's), %d ignored, %d found" % (
        len(deps), withdrawn, rustsec, len(ignored), count,
    )
    if count:
        print("ghsa: FAIL — " + summary)
        return 1
    print("ghsa: ok — " + summary)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Fail as e:
        print("ghsa: FAIL — %s (unable to verify)" % e)
        sys.exit(1)
