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
# one chunk is attributed by asking about each of those pairs alone (only for
# an advisory that would be reported, and once per pair and type).
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
#     An ignore that matches no advisory of the locked versions is a warning
#     (never a failure): it has likely outlived its dependency.
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
# requests per 100 dependencies, plus one per crate of a finding that names
# several) stays far under. A token only raises that limit. Looked for in
# GITHUB_TOKEN, GH_TOKEN, then (neither set and GITHUB_ACTIONS unset, i.e. a
# local run) `gh auth token`; none found is an anonymous run, not an error. This
# script sends it to api.github.com alone, and never on to a redirect target
# (Authorization goes in as an unredirected header). That is about where *this
# script* sends it: in CI the variable is in the environment of the whole
# `mise run audit` step, which includes the build scripts of dependencies that
# cargo runs. It is the same exposure as the accuracy job's step and as the
# token actions/checkout leaves in .git/config (permission: contents: read).
# When the quota is gone (403/429 with X-RateLimit-Remaining: 0) the FAIL says
# so, and whether the run was anonymous.
#
# Test injection (the one, for scripts/check-ghsa-test.sh only): with
# CHECK_GHSA_FIXTURE=<dir> set, nothing touches the network, cargo or gh. It
# replaces only the layer under urllib (_open below); the headers, host check,
# retries, 4xx cut-off, Link following and JSON parsing run as in production.
#   <dir>/metadata.json     stands in for `cargo metadata` output
#   <dir>/deny.toml         stands in for engine/deny.toml (required)
#   <dir>/response-<k>.json the k-th attempt's answer (0-based, in request
#                           order, retries included; a missing file is a
#                           request failure): {"status": 200, "headers":
#                           {"Link": ..}, "body": <API body>}; "raw": <text>
#                           replaces body as the literal bytes; "raise":
#                           "IncompleteRead" | "URLError" | "TimeoutError" makes
#                           the attempt fail with that exception instead.
#   <dir>/requests.log      every requested URL, one per line
#   <dir>/request-headers.log  per request, `H <json>` (Request.headers) and
#                           `U <json>` (Request.unredirected_hdrs)
# Retry sleeps are 0 in this mode. CI never sets it; a run that does says so
# on its first line.
import email.message
import io
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
RETRY_SLEEP = 2
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
        try:
            proc = subprocess.run(
                ["cargo", "metadata", "--locked", "--all-features", "--format-version", "1"],
                cwd=os.path.join(ROOT, "engine"),
                capture_output=True,
            )
        except OSError as e:  # cargo not on PATH: a FAIL line, not a traceback
            raise Fail("cannot run cargo metadata: %r" % e)
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


_ID = r"GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4}"
_ID_RE = re.compile(_ID)
# A table header line, as it can stand for the one section read: nothing else.
_ADVISORIES_HEADER = re.compile(r"\[\s*advisories\s*\]\s*(?:#.*)?")
_WORD = re.compile(r"[A-Za-z0-9_-]+")


def _advisories_tokens(text):
    """Tokens of the [advisories] section of a TOML text, strings told from the rest.

    A small lexer, not a parser: ('s', value) for a one-line string (basic or
    literal; value is the raw text between the quotes), ('m', None) for a
    multi-line one (never matches anything), ('w', word) for a bare word and
    ('p', char) for any other character that is not blank. Comments (`#` to
    the end of the line, outside a string) are dropped. A line that starts
    with `[` outside any bracket is a table header: it ends the section, or
    starts it when it is `[advisories]`.
    """
    toks, i, n = [], 0, len(text)
    in_section, depth, bol = False, 0, True
    while i < n:
        c = text[i]
        if c == "\n":
            bol = True
            i += 1
        elif c in " \t\r":
            i += 1
        elif c == "#":
            while i < n and text[i] != "\n":
                i += 1
        elif bol and depth == 0 and c == "[":
            j = text.find("\n", i)
            j = n if j < 0 else j
            in_section = _ADVISORIES_HEADER.fullmatch(text[i:j].rstrip("\r")) is not None
            i = j
        else:
            bol = False
            if c in "\"'":
                kind, value, i = _lex_string(text, i)
                tok = (kind, value)
            elif (m := _WORD.match(text, i)) is not None:
                tok, i = ("w", m.group()), m.end()
            else:
                tok, i = ("p", c), i + 1
                if c in "[{":
                    depth += 1
                elif c in "]}":
                    depth = max(0, depth - 1)
            if in_section:
                toks.append(tok)
    return toks


def _lex_string(text, i):
    """(kind, value, next index) of the string starting at text[i], a quote."""
    q, n = text[i], len(text)
    escapes = q == '"'  # only a basic string has backslash escapes
    if text.startswith(q * 3, i):
        j = i + 3
        while j < n and not text.startswith(q * 3, j):
            j += 2 if escapes and text[j] == "\\" else 1
        return "m", None, min(j + 3, n)
    j = i + 1
    while j < n and text[j] not in (q, "\n"):
        j += 2 if escapes and text[j] == "\\" and j + 1 < n and text[j + 1] != "\n" else 1
    if j < n and text[j] == q:
        return "s", text[i + 1 : j], j + 1
    return "s", None, j  # unterminated: a string that is never a GHSA id


def ignored_ids(text):
    """GHSA ids under [advisories] in a deny.toml text, picked out by hand.

    Not a TOML parse (no tomllib before Python 3.10, and the supported macOS
    ships 3.9). A miss only ever errs toward "not ignored", i.e. stricter, so
    this need not read every TOML spelling: an entry it cannot see is a FAIL
    the author then writes in the plain form. The other way (picking up an id
    that is not an ignore: fail-open, the advisory would pass) is what the
    lexer is for. An id counts only as a string token that is exactly a GHSA
    id and is either an array element (after `[` or `,`, before `,` or `]`) or
    the value of `id =`. So an id inside a longer string (a reason, whatever
    the quoting: `'..["GHSA-.."]..'`, `"..\\"GHSA-..\\".."`, a multi-line
    string), inside a comment, or in another section is not an ignore.
    Entries like { crate = "foo@1.2.3", reason = ".." } are simply not picked.
    """
    toks = _advisories_tokens(text)
    out = set()
    for j, (kind, value) in enumerate(toks):
        if kind != "s" or value is None or not _ID_RE.fullmatch(value):
            continue
        prev = toks[j - 1] if j >= 1 else None
        nxt = toks[j + 1] if j + 1 < len(toks) else None
        element = prev in (("p", "["), ("p", ",")) and nxt in (("p", ","), ("p", "]"))
        keyed = prev == ("p", "=") and j >= 2 and toks[j - 2] == ("w", "id")
        if element or keyed:
            out.add(value)
    return out


def read_ignored():
    path = os.path.join(FIXTURE, "deny.toml") if FIXTURE else os.path.join(ROOT, "engine", "deny.toml")
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except (OSError, ValueError) as e:
        raise Fail("cannot read the ignore list in %s: %r" % (path, e))
    return ignored_ids(text)


def _gh_cli_token():
    # Its own function so that the fixture mode (tests) can never reach it: a
    # test must not run gh, or depend on the developer's login.
    try:
        proc = subprocess.run(
            ["gh", "auth", "token"], capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=15
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return proc.stdout.strip() or None if proc.returncode == 0 else None


_token = None  # (token or None, where it came from), looked up once


def find_token():
    global _token
    if _token is None:
        for var in ("GITHUB_TOKEN", "GH_TOKEN"):
            if os.environ.get(var):
                _token = (os.environ[var], var)
                break
        else:
            local = not FIXTURE and not os.environ.get("GITHUB_ACTIONS")
            tok = _gh_cli_token() if local else None
            _token = (tok, "gh auth token") if tok else (None, "anonymous")
    return _token


class _FakeResponse:
    def __init__(self, headers, data):
        self.headers, self._data = headers, data

    def read(self):
        return self._data

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


_RAISES = {
    "IncompleteRead": lambda: http.client.IncompleteRead(b"partial"),
    "URLError": lambda: urllib.error.URLError("fixture: connection refused"),
    "TimeoutError": lambda: TimeoutError("fixture: timed out"),
}
_attempts = 0


def _fixture_open(req, timeout):
    """Stands in for urlopen under CHECK_GHSA_FIXTURE: the next response file."""
    global _attempts
    k = _attempts
    _attempts += 1
    with open(os.path.join(FIXTURE, "requests.log"), "a") as f:
        f.write(req.full_url + "\n")
    with open(os.path.join(FIXTURE, "request-headers.log"), "a") as f:
        f.write("H %s\nU %s\n" % (json.dumps(req.headers, sort_keys=True), json.dumps(req.unredirected_hdrs, sort_keys=True)))
    try:
        with open(os.path.join(FIXTURE, "response-%d.json" % k)) as f:
            doc = json.load(f)
        status = doc.get("status", 200)
        raised = doc.get("raise")
        data = b""
        if not raised:
            data = doc["raw"].encode() if "raw" in doc else json.dumps(doc["body"]).encode()
        hdrs = email.message.Message()
        for name, value in (doc.get("headers") or {}).items():
            hdrs[name] = value
    except (OSError, ValueError, KeyError, AttributeError) as e:
        raise Fail("request failed (fixture %d): %r" % (k, e))
    if raised:
        if raised not in _RAISES:
            raise Fail("request failed (fixture %d): unknown raise %r" % (k, raised))
        raise _RAISES[raised]()
    if status >= 400:
        raise urllib.error.HTTPError(req.full_url, status, "fixture", hdrs, io.BytesIO(data))
    return _FakeResponse(hdrs, data)


def _real_open(req, timeout):
    return urllib.request.urlopen(req, timeout=timeout)


_open = _fixture_open if FIXTURE else _real_open


def http_get(url):
    """(Link header or "", parsed JSON body). Retries transient failures."""
    if not url.startswith(API + "/"):
        # Also keeps the token from following a Link to another host.
        raise Fail("refusing to request %s" % url)
    headers = {
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": USER_AGENT,
    }
    token, source = find_token()
    last = None
    for attempt in range(ATTEMPTS):
        if attempt:
            time.sleep(0 if FIXTURE else RETRY_SLEEP)  # 0 under test: no waiting
        try:
            req = urllib.request.Request(url, headers=headers)
            if token:
                # Unredirected: a redirect (to another host) must not carry it.
                req.add_unredirected_header("Authorization", "Bearer " + token)
            with _open(req, TIMEOUT) as resp:
                return resp.headers.get("Link", ""), json.loads(resp.read())
        except urllib.error.HTTPError as e:
            last = "HTTP %d" % e.code
            if e.code in (403, 429) and e.headers is not None and e.headers.get("X-RateLimit-Remaining") == "0":
                last += (
                    ", API rate limit exhausted; set GITHUB_TOKEN/GH_TOKEN or `gh auth login` (this run: %s)"
                    % ("anonymous" if not token else "token from " + source)
                )
                break  # asking again within seconds cannot help
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


_alone = {}


def returned_alone(name, version, kind):
    """ghsa ids the API lists for this one pair; asked once per (pair, type)."""
    key = (name, version, kind)
    if key not in _alone:
        _alone[key] = {a["ghsa_id"] for a in advisories_affecting([(name, version)], kind)}
    return _alone[key]


def attribute(ghsa_id, sightings):
    """The locked (name, version) pairs an advisory is about.

    Within a chunk a name is one version, so a single named crate gives the
    pair directly; several are told apart by asking about each pair alone.
    Only asked for an advisory that is going to be reported.
    """
    pairs = set()
    for kind, version_of, names in sightings:
        if len(names) > 1:
            names = [n for n in names if ghsa_id in returned_alone(n, version_of[n], kind)]
            if not names:
                raise Fail("advisory %s names several crates but none alone (cannot attribute)" % ghsa_id)
        pairs.update((n, version_of[n]) for n in names)
    return pairs


def main():
    if FIXTURE:
        print("ghsa: using test fixture %s (no network)" % FIXTURE)
    deps = registry_deps()
    ignore = read_ignored()

    # ghsa_id -> (advisory, [(type, chunk's version_of, crates it names there)])
    seen = {}
    for chunk in chunks(deps):
        version_of = dict(chunk)
        for kind in TYPES:
            for adv in advisories_affecting(chunk, kind):
                ghsa_id = adv["ghsa_id"]
                names = sorted({v["package"]["name"] for v in adv["vulnerabilities"]} & version_of.keys())
                if not names:
                    raise Fail("advisory %s names none of the crates asked about" % ghsa_id)
                seen.setdefault(ghsa_id, (adv, []))[1].append((kind, version_of, names))

    withdrawn = rustsec = 0
    ignored, findings = [], []
    for ghsa_id, (adv, sightings) in sorted(seen.items()):
        if adv.get("withdrawn_at"):
            withdrawn += 1
        elif covered_by_rustsec(adv):
            rustsec += 1
        elif ghsa_id in ignore:
            ignored.append(adv)
        else:
            findings.append((adv, attribute(ghsa_id, sightings)))

    for adv in ignored:
        print("ghsa: ignored %s (deny.toml) — %s" % (adv["ghsa_id"], adv.get("summary", "")))
    for ghsa_id in sorted(ignore - seen.keys()):
        print(
            "ghsa: warning — ignore %s (deny.toml) matched no advisory for the locked versions; remove it?" % ghsa_id
        )
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
