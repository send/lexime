#!/usr/bin/env bash
# Generate the test runner's dispatch from the test sources themselves.
#
# `TestRunner.main()` used to call each test by hand. That list was the same
# hazard as the hand-maintained source list #316 was about: a test could be
# written, compiled, and simply never called — green CI, author believes it is
# covered. A silently skipped test is the worst kind of gate failure, because
# the gate keeps reporting the coverage it no longer has.
#
# The first attempt at closing it was a checker that diffed the definitions
# against the hand-written call list. That is a reconciler between two
# human-maintained artifacts — the shape CLAUDE.md rules out — and it inherited
# the usual weaknesses: it passed vacuously when its pattern matched nothing,
# and it counted a name mentioned in a comment as a call. Generating the list
# instead deletes both, because there is no second artifact to disagree with.
#
# Two residuals remain, and they are different in kind.
#
#   1. Declaration *form* — an indented, attributed or parameterised
#      declaration cannot have a call emitted for it. This one IS closed: the
#      broad-vs-strict check below is a totality check on a partial
#      recogniser (the same species as a parser asserting it consumed all its
#      input), not a reconcile between two artifacts, and it fails closed.
#
#   2. Declaration *name* — both patterns key on the `test` prefix, so a test
#      called `verifyThing()` is invisible to the generator AND to the check,
#      and is silently never run. This one is NOT closed, and cannot be from
#      here: a tripwire on every non-`test` function would flag every helper
#      in Tests/, and exempting them means a hand-maintained list, which is
#      the shape this script exists to delete. **Tests must be named
#      `testX()`** — that convention is load-bearing and nothing enforces it.
#      Note the hand-written list did not enforce it either; what changed is
#      that adding a test no longer takes you through TestRunner.main(),
#      which is where a misnamed one used to become obvious.
#
# The real canonical form is XCTest / swift-testing, which discover tests
# themselves — XCTest through the objc runtime, swift-testing through
# macro-emitted metadata — and need no generator. Deferred: it would rewrite
# every test file and link a framework into a runner that deliberately stays
# headless. Revisit if a test ever legitimately needs a declaration form this
# generator cannot emit a call for, if a misnamed test is ever found unrun, or
# if a second dispatch list appears.
set -euo pipefail

out=${1:?usage: test-dispatch.sh <output.swift>}
here=$(dirname "$0")
sources=$(bash "$here/swift-sources.sh" Tests)

# Comments are stripped first. Prose about tests is common in this repo — the
# runner's own doc comment says `func testX()` — and without this the broad
# pattern below trips on it. Block comments matter for the opposite reason:
# wrapping a test in `/* */` is how people disable one, and an unstripped
# block comment gets *dispatched*, emitting a call to a function that no
# longer exists.
#
# Read per file, never `cat` them together. Concatenation with no separator
# lets a file whose last line is a comment and which lacks a trailing newline
# swallow the first line of the next file — and if that line was a
# declaration, both patterns lose it identically, so the tripwire below cannot
# fire and the test is dropped in silence. That is the exact failure this
# script exists to prevent, so it must not be reachable from inside it.
code=$(
  while IFS= read -r f; do
    awk '
      { line = $0 }
      # Block comments: track depth across lines, blank out what is inside.
      {
        out = ""
        while (length(line) > 0) {
          if (depth > 0) {
            p = index(line, "*/")
            if (p == 0) { line = ""; break }
            depth--; line = substr(line, p + 2); continue
          }
          p = index(line, "/*")
          if (p == 0) { out = out line; line = ""; break }
          out = out substr(line, 1, p - 1); depth++; line = substr(line, p + 2)
        }
        sub(/\/\/.*$/, "", out)
        print out
      }
    ' "$f"
    echo   # guarantee a separator even if the file has no trailing newline
  done <<< "$sources"
)

# Strict: declarations we can safely emit a call for — top level, no attributes
# or modifiers, no parameters. Whitespace is `[[:space:]]+` to match the broad
# pattern below: any difference between the two stops the whole suite, so the
# two must not disagree over something as incidental as a second space.
# `|| true` because "no match" is answered by the explicit emptiness check
# below, with a better message than a bare exit 1.
# `\(\)[[:space:]]*\{` rather than `\(\)`: without the trailing brace the
# pattern also matches the prefix of `func testX() throws {}` and `async`,
# and the emitted bare `testX()` then does not compile.
strict=$(printf '%s\n' "$code" \
  | grep -oE '^func[[:space:]]+test[A-Za-z0-9_]*\(\)[[:space:]]*\{' \
  | sed -E 's/^func[[:space:]]+//; s/\(\)[[:space:]]*\{$//' | sort -u || true)

# Broad: anything that reads as a test declaration in any form — indented, or
# behind `private` / `@MainActor` / any other attribute, or taking parameters.
broad=$(printf '%s\n' "$code" \
  | grep -oE 'func[[:space:]]+test[A-Za-z0-9_]*' \
  | sed -E 's/.*func[[:space:]]+//' | sort -u || true)

if [ -z "$strict" ]; then
  echo "test-dispatch: no dispatchable 'func testX()' found under Tests/ —" >&2
  echo "  refusing to generate an empty run" >&2
  exit 1
fi

# Anything the broad pattern sees and the strict one does not would compile but
# never run. Fail rather than emit a dispatch that quietly omits it.
missed=$(comm -13 <(printf '%s\n' "$strict") <(printf '%s\n' "$broad"))
if [ -n "$missed" ]; then
  echo "test-dispatch: these read as tests but cannot be dispatched" >&2
  echo "$missed" | sed 's/^/  /' >&2
  echo "  If it is a helper rather than a test, rename it off the 'test'" >&2
  echo "  prefix — that is the usual cause. If it is a test, give it the" >&2
  echo "  plain top-level 'func testX()' form. Widening this generator is a" >&2
  echo "  last resort: it may only emit bare 'name()' calls, so a declaration" >&2
  echo "  taking parameters — or one that is 'throws' or 'async' — can never" >&2
  echo "  be admitted here. Reaching this on a legitimate test is the recorded" >&2
  echo "  trigger to revisit XCTest/swift-testing (see the header)." >&2
  exit 1
fi

mkdir -p "$(dirname "$out")"
{
  echo "// Generated by scripts/test-dispatch.sh. Do not edit, do not commit."
  echo "func runAllDiscoveredTests() {"
  printf '%s\n' "$strict" | sed 's/^/    /; s/$/()/'
  echo "}"
} > "$out"

echo "test-dispatch: $(printf '%s\n' "$strict" | wc -l | tr -d ' ') tests dispatched"
