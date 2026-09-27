#!/usr/bin/env python3
# Check that every mise task that builds runs the supply-chain screen first
# (mise.toml's [tasks.screen]), and that the @claude bot is allowed no build
# the screen does not precede. Run from the repository root, by
# scripts/check-task-screen-test.sh (CI's task-screen job and `mise run
# test-task-screen`); python 3.9, as the Xcode python is.
#
# cargo compiles and runs dependency build scripts, so a build must not start
# before the screen has passed. mise gives that order through `depends`: it
# runs the screen once per invocation and starts no task whose dependency
# failed. So the property is: `screen` is in every task's dependency closure,
# unless the task is in one of the two lists below. A new task starts out
# failing this check until it depends on the screen or a reviewer adds it to
# a list, so no one has to remember to screen it.
#
# The graph is mise's own (`mise tasks ls --json`), not a parse of mise.toml:
# it covers file tasks and anything else mise loads, spelled any way mise
# accepts. Run it with the config the gates run on (CI sets
# MISE_OVERRIDE_CONFIG_FILENAMES=mise.toml); locally it checks whatever your
# mise loads.
#
# Not covered: what a task's run script does. A listed task that starts
# building, or a script that calls `mise run --skip-deps`, is for review.
import json
import os
import re
import subprocess
import sys
from fnmatch import fnmatchcase

# Tasks that do not build, so need no screen. Each is a reviewed claim about
# its run script; keep the reason next to it.
NO_BUILD = {
    "screen": "the screen itself (cargo metadata only)",
    "lint-toolchain": "rustup toolchain install",
    "fmt": "cargo fmt formats; it compiles nothing",
    "clean": "cargo clean",
    "dict-clean": "rm",
    "reload": "pkill",
    "log": "log stream",
    "trace-log": "tail",
    "icon": "scripts/icon.sh (sips / iconutil)",
    "fetch-model": "curl",
    "test-read-pin": "shell tests of the pin readers",
    "test-check-sources": "cargo generate-lockfile / metadata on fixtures with no registry dependency",
    "test-task-screen": "this check and its tests",
}

# Tasks that build but do not depend on the screen themselves, because every
# task that reaches them does. Checked below: every task that names one of
# these, in any way mise can run it, must itself be screened.
SCREENED_BY_CALLER = {
    "audit-deps": "reached via `audit`, which depends on the screen",
}

CLAUDE_YML = ".github/workflows/claude.yml"

errors = []


def err(msg):
    errors.append(msg)


def mise_json(*args):
    out = subprocess.run(["mise", *args], check=True, stdout=subprocess.PIPE).stdout
    return json.loads(out)


# --- mise settings that would skip the screen ------------------------------
# task.skip_depends runs no dependency at all, task.skip drops named tasks;
# either turns every `depends` below into a no-op. Effective values, so a
# [settings] table, an env var or a global config all count.
task_settings = mise_json("settings", "ls", "--all", "--json").get("task", {})
if task_settings.get("skip_depends") is not False:
    err("mise setting task.skip_depends is %r: no task would run its screen"
        % task_settings.get("skip_depends"))
skip = task_settings.get("skip", [])
if not isinstance(skip, list) or any(fnmatchcase("screen", str(p)) for p in skip):
    err("mise setting task.skip is %r: it skips the screen" % (skip,))

# --- the task graph --------------------------------------------------------
root = os.path.realpath(".")
tasks = {}
for t in mise_json("tasks", "ls", "--json", "--hidden"):
    # The repository's tasks only: a global config's are the user's own.
    src = os.path.realpath(t.get("source") or "/")
    if t.get("global") or not src.startswith(root + os.sep):
        continue
    tasks[t["name"]] = t


def refs(entries, where):
    """Task names an entry list (depends / run) names, patterns expanded."""
    names = []
    for e in entries or []:
        if isinstance(e, str):
            if where == "run":
                continue  # a script, not a task reference
            pat = e.split()[0] if e.split() else ""
        elif isinstance(e, dict) and isinstance(e.get("task"), str):
            pat = e["task"].split()[0]
        elif isinstance(e, dict) and isinstance(e.get("tasks"), list):
            names += refs(e["tasks"], "depends")
            continue
        else:
            # A shape this check does not know: fail rather than skip it.
            err("unrecognized %s entry %r" % (where, e))
            continue
        hit = [n for n in tasks if fnmatchcase(n, pat)]
        if not hit:
            err("%s entry %r names no task" % (where, e))
        names += hit
    return names


def closure(name):
    """Every task `depends` makes mise finish before `name` starts."""
    seen, todo = set(), [name]
    while todo:
        for d in refs(tasks[todo.pop()].get("depends"), "depends"):
            if d not in seen:
                seen.add(d)
                todo.append(d)
    return seen


screened = {n for n in tasks if "screen" in closure(n)}

if "screen" not in tasks:
    err("no `screen` task")
for listed in (NO_BUILD, SCREENED_BY_CALLER):
    for n in sorted(set(listed) - set(tasks)):
        err("%s is listed in %s but is not a task" % (n, "NO_BUILD" if listed is NO_BUILD else "SCREENED_BY_CALLER"))

for n in sorted(tasks):
    if n not in screened and n not in NO_BUILD and n not in SCREENED_BY_CALLER:
        err("task %s does not depend on `screen` (directly or through its depends); "
            "add \"screen\" to its depends, or if it does not build, to NO_BUILD in %s"
            % (n, sys.argv[0]))

# Every way one task makes mise run another. None of these but `depends`
# orders the named task after the caller's screen by itself, so a caller of a
# SCREENED_BY_CALLER task must be screened.
for n in sorted(tasks):
    t = tasks[n]
    named = (refs(t.get("depends"), "depends") + refs(t.get("depends_post"), "depends")
             + refs(t.get("wait_for"), "depends") + refs(t.get("run"), "run"))
    for c in sorted(set(named) & set(SCREENED_BY_CALLER)):
        if n not in screened:
            err("task %s runs %s, which builds, without depending on `screen`" % (n, c))

# --- the @claude bot's allowlist -------------------------------------------
# The bot builds only through the tasks it is allowed, so each must be
# screened or not build. Bash rules other than exact `mise run <task>` and
# `gh` are refused outright: raw cargo would skip the screen, and a prefix
# rule (`mise run lint:*`) also admits `mise run lint ::: <any task>`.
with open(CLAUDE_YML) as f:
    yml = f.read()
if re.search(r"dangerously-skip-permissions|bypassPermissions", yml):
    err("%s bypasses the permission rules" % CLAUDE_YML)
spellings = re.findall(r"allowed-?tools", yml, re.I)
lists = re.findall(r"--allowed-?tools[ =]+\"([^\"]*)\"", yml, re.I)
if not spellings:
    err("no --allowedTools in %s" % CLAUDE_YML)
elif len(lists) != len(spellings):
    err("%s names allowedTools %d times but only %d parse as --allowedTools \"...\""
        % (CLAUDE_YML, len(spellings), len(lists)))


def rules(s):
    """Split a tool list on commas and spaces outside parentheses."""
    out, depth, cur = [], 0, ""
    for ch in s:
        depth += ch == "("
        depth -= ch == ")"
        if depth == 0 and ch in ", \n":
            if cur:
                out.append(cur)
            cur = ""
        else:
            cur += ch
    return out + ([cur] if cur else [])


allowed = []
for rule in (r for lst in lists for r in rules(lst)):
    if not rule.startswith("Bash"):
        continue
    m = re.fullmatch(r"Bash\((.*)\)", rule)
    cmd = m.group(1).strip() if m else ""
    task = re.fullmatch(r"mise run ([A-Za-z0-9_-]+)", cmd)
    if task:
        allowed.append(task.group(1))
    elif not re.fullmatch(r"gh [a-z].*", cmd):
        err("bot rule %s: only exact `mise run <task>` and `gh` Bash rules are allowed" % rule)
for n in allowed:
    if n not in tasks:
        err("bot rule `mise run %s`: no such task" % n)
    elif n not in screened and n not in NO_BUILD:
        err("bot rule `mise run %s`: the task builds without the screen" % n)

if errors:
    for e in errors:
        print("task-screen: " + e)
    sys.exit(1)
print("task-screen: %d tasks, %d screened, %d exempt; bot tasks: %s"
      % (len(tasks), len(screened), len(tasks) - len(screened), ", ".join(allowed)))
