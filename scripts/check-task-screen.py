#!/usr/bin/env python3
# Check that every mise task that builds runs the supply-chain screen first
# (mise.toml's [tasks.screen]), and that the @claude bot is allowed no build
# the screen does not precede. Run from the repository root, by
# scripts/check-task-screen-test.sh (CI's task-screen job and `mise run
# test-task-screen`); python 3.9, as the Xcode python is.
#
# cargo compiles and runs dependency build scripts, so a build must not start
# before the screen has passed. mise gives that order through `depends`: it
# runs a task's dependencies first, starts no task whose dependency failed,
# and runs the screen once per invocation. So the property is: `screen` is
# in the dependency closure of every task not listed in NO_BUILD. A new task
# fails this check until it depends on the screen or a reviewer lists it, so
# no one has to remember to screen it. It is each task's own closure that
# counts: a task reached another way (a run list, depends_post) still runs
# its own dependencies first, while two tasks side by side in one `depends`
# run at the same time, so only a task that has the screen itself is safe.
#
# The graph is mise's own (`mise tasks ls --json`), not a parse of mise.toml:
# it covers file tasks and anything else mise loads, spelled any way mise
# accepts. Run it with the config the gates run on (CI sets
# MISE_OVERRIDE_CONFIG_FILENAMES=mise.toml); locally it checks whatever your
# mise loads.
#
# Not covered: what a task's run script does. A NO_BUILD task that starts
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

# The screen task itself, exactly: with `sources`/`outputs` mise could skip
# it as up to date while every closure still names it.
SCREEN_RUN = ["bash scripts/screen.sh"]

CLAUDE_YML = ".github/workflows/claude.yml"
# The shape of the bot's tool list: comma-separated, no spaces, and only
# these Bash rules (an exact `mise run <task>`: a prefix rule would also admit
# `mise run lint ::: <any task>`; and the read-only `gh pr` commands). No raw
# cargo, which would skip the screen, and no other tool; allowing one is a
# change to this check, in review.
BOT_RULE = r"Bash\((mise run [a-z0-9-]+|gh pr (view|diff|checks):\*)\)"

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


def refs(entries):
    """Task names a `depends` list names, patterns expanded."""
    names = []
    for e in entries or []:
        if isinstance(e, str):
            pat = (e.split() or [""])[0]
        elif isinstance(e, dict) and isinstance(e.get("task"), str):
            pat = e["task"].split()[0]
        else:
            # A shape this check does not know: fail rather than skip it.
            err("unrecognized depends entry %r" % (e,))
            continue
        hit = [n for n in tasks if fnmatchcase(n, pat)]
        if not hit:
            err("depends entry %r names no task" % (e,))
        names += hit
    return names


depends = {n: refs(t.get("depends")) for n, t in tasks.items()}


def closure(name):
    """Every task `depends` makes mise finish before `name` starts."""
    seen, todo = set(), [name]
    while todo:
        for d in depends[todo.pop()]:
            if d not in seen:
                seen.add(d)
                todo.append(d)
    return seen


screened = {n for n in tasks if "screen" in closure(n)}

screen = tasks.get("screen")
if screen is None:
    err("no `screen` task")
elif screen.get("run") != SCREEN_RUN or screen.get("sources") or screen.get("outputs"):
    err("the `screen` task must be exactly run = %r, with no sources or outputs" % SCREEN_RUN[0])
for n in sorted(set(NO_BUILD) - set(tasks)):
    err("%s is listed in NO_BUILD but is not a task" % n)

for n in sorted(set(tasks) - screened - set(NO_BUILD)):
    err("task %s does not depend on `screen` (directly or through its depends); "
        "add \"screen\" to its depends, or if it does not build, to NO_BUILD in %s"
        % (n, sys.argv[0]))

# --- the @claude bot's tool list ------------------------------------------
# The bot builds only through the tasks its --allowedTools list allows, so
# each must be screened or not build. Every mention of the flag in claude.yml,
# in any spelling, must be that list in BOT_RULE's shape; anything else
# fails rather than being read around.
#
# Not covered, and for review: the rest of claude.yml (other steps and jobs,
# the action's other inputs such as `settings`, where SCREEN_POLICY_REF and
# MISE_OVERRIDE_CONFIG_FILENAMES are set) and other Claude Code configuration
# in the repository (.claude/ settings, hooks in skills or agents, .mcp.json),
# nor a flag spelled so no text search finds it (a YAML escape in a quoted
# value).
# Each widens what the bot can run, and a line reader of YAML or of Claude
# Code's formats fails open on spellings it does not know (tried here: an
# adversarial pass found a dozen that parse, and pass, as something else).
with open(CLAUDE_YML) as f:
    yml = f.read()
mentions = re.findall(r"allowed[-_ ]?tools", yml, re.I)
lists = re.findall(r'--allowedTools "(%s(?:,%s)*)"' % (BOT_RULE, BOT_RULE), yml)
if not mentions or len(lists) != len(mentions):
    err("%s: every allowedTools mention must be --allowedTools \"<rules>\" with only "
        "comma-separated %s rules (%d mentions, %d such lists)"
        % (CLAUDE_YML, BOT_RULE, len(mentions), len(lists)))
allowed = [m for lst in lists for m in re.findall(r"Bash\(mise run ([a-z0-9-]+)\)", lst[0])]
for n in allowed:
    if n not in tasks:
        err("bot rule `mise run %s`: no such task" % n)
    elif n not in screened and n not in NO_BUILD:
        err("bot rule `mise run %s`: the task builds without the screen" % n)

if errors:
    for e in dict.fromkeys(errors):
        print("task-screen: " + e)
    sys.exit(1)
print("task-screen: %d tasks, %d screened, %d not building; bot tasks: %s"
      % (len(tasks), len(screened), len(tasks) - len(screened), ", ".join(allowed)))
