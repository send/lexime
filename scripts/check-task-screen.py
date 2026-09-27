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
CLAUDE_ACTION = "anthropics/claude-code-action@"
# What claude.yml may hold. An allowlist, as each thing outside it is another
# way to give the bot tools, settings or hooks (the action's `settings`,
# `plugins`, `additional_permissions`; `--settings` or `--mcp-config` in
# claude_args; an environment variable mise or Claude Code reads).
CLAUDE_STEP_KEYS = {"name", "id", "uses", "env", "with"}
CLAUDE_WITH_KEYS = {"claude_code_oauth_token", "github_token", "claude_args"}
CLAUDE_ENV = {
    # mise reads mise.toml alone: no committed .mise.toml or mise.local.toml
    # can redefine a task, and no other config can set task.skip*.
    "MISE_OVERRIDE_CONFIG_FILENAMES": "mise.toml",
    # scripts/screen.sh: policy files read from main. Also what turns off
    # check-quarantine.sh's publish-date cache, which the bot could write.
    "SCREEN_POLICY_REF": "origin/main",
}

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

# --- the @claude bot's workflow --------------------------------------------
# The bot builds only through the tasks it is allowed, so each must be
# screened or not build, and nothing else in the workflow may widen that.
# This is a check of claude.yml's shape against the one it has, read line by
# line: anything it does not recognize is refused, not skipped.
with open(CLAUDE_YML) as f:
    yml = f.read()
lines = [(len(l) - len(l.lstrip(" ")), l.strip()) for l in yml.splitlines()]
lines = [(i, s) for i, s in lines if s and not s.startswith("#")]


def mapping(at):
    """Key/value pairs of the block mapping under line `at` (`key:` alone)."""
    indent, pairs, child = lines[at][0], [], None
    for i, s in lines[at + 1:]:
        if i <= indent:
            break
        child = i if child is None else child
        if i > child:
            pairs[-1][1].append(s)  # a continuation line of the value above
            continue
        m = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_-]*):(.*)", s)
        if i < child or not m:
            err("%s: cannot read %r under %r" % (CLAUDE_YML, s, lines[at][1]))
            break
        pairs.append((m.group(1), [m.group(2).strip()] if m.group(2).strip() else []))
    return pairs


for word in ("dangerously-skip-permissions", "bypassPermissions", "GITHUB_ENV", "GITHUB_PATH"):
    if word in yml:
        err("%s names %s, which can widen what the bot runs" % (CLAUDE_YML, word))

# Every env: block holds only CLAUDE_ENV's entries, all of them together.
env = {}
for n, (_, s) in enumerate(lines):
    if re.match(r"(- )?env:", s):
        if not re.fullmatch(r"(- )?env:", s):
            err("%s: an inline env: cannot be checked" % CLAUDE_YML)
        for key, value in mapping(n):
            env[key] = " ".join(value)
            if CLAUDE_ENV.get(key) != env[key]:
                err("%s sets %s=%s; its env may hold only %r" % (CLAUDE_YML, key, env[key], CLAUDE_ENV))
for key in sorted(set(CLAUDE_ENV) - set(env)):
    err("%s does not set %s: %s" % (CLAUDE_YML, key, CLAUDE_ENV[key]))
if set(re.findall(r"MISE_[A-Z0-9_]*", yml)) - set(CLAUDE_ENV):
    err("%s names a MISE_* variable other than MISE_OVERRIDE_CONFIG_FILENAMES" % CLAUDE_YML)

# The action's step: its keys, its inputs, and claude_args, which must be one
# --allowedTools list and nothing else.
steps = [n for n, (_, s) in enumerate(lines) if re.fullmatch(r"(- )?uses:\s*" + re.escape(CLAUDE_ACTION) + r"\S+(\s.*)?", s)]
allowed = []
if len(steps) != 1:
    err("%s uses %s %d times, not once" % (CLAUDE_YML, CLAUDE_ACTION, len(steps)))
else:
    n = steps[0]
    key_indent = lines[n][0] + (2 if lines[n][1].startswith("- ") else 0)
    start = n
    while not lines[start][1].startswith("- "):
        start -= 1
    step = []
    for m, (i, s) in enumerate(lines[start:], start):
        if m > start and (i < key_indent or (i == key_indent - 2 and s.startswith("- "))):
            break
        if m == start or i == key_indent:
            step.append((m, re.sub(r"^- ", "", s).split(":")[0]))
    with_at = None
    for m, key in step:
        if key not in CLAUDE_STEP_KEYS:
            err("%s: the Claude step's key %r is not one of %s" % (CLAUDE_YML, key, sorted(CLAUDE_STEP_KEYS)))
        if key == "with":
            with_at = m
    inputs = dict((k, " ".join(v)) for k, v in mapping(with_at)) if with_at is not None else {}
    for key in sorted(set(inputs) - CLAUDE_WITH_KEYS):
        err("%s: the Claude step's input %r is not one of %s" % (CLAUDE_YML, key, sorted(CLAUDE_WITH_KEYS)))
    args = re.fullmatch(r'(?:>-\s*)?--allowedTools "([^"]*)"', inputs.get("claude_args", ""))
    if not args:
        err("%s: claude_args must be exactly --allowedTools \"...\"" % CLAUDE_YML)
    else:
        # Split on commas and spaces outside parentheses.
        rules, depth, cur = [], 0, ""
        for ch in args.group(1) + ",":
            depth += (ch == "(") - (ch == ")")
            if depth == 0 and ch in ", ":
                rules += [cur] if cur else []
                cur = ""
            else:
                cur += ch
        # Bash rules: exact `mise run <task>` (a prefix rule also admits
        # `mise run lint ::: <any task>`) or the read-only gh pr commands.
        # No raw cargo, which would skip the screen.
        for rule in rules:
            if not rule.startswith("Bash"):
                continue
            m = re.fullmatch(r"Bash\((.*)\)", rule)
            cmd = m.group(1).strip() if m else ""
            task = re.fullmatch(r"mise run ([A-Za-z0-9_-]+)", cmd)
            if task:
                allowed.append(task.group(1))
            elif not re.fullmatch(r"gh pr (view|diff|checks)(:\*)?", cmd):
                err("bot rule %s: only exact `mise run <task>` and `gh pr view/diff/checks` Bash rules are allowed" % rule)
for n in allowed:
    if n not in tasks:
        err("bot rule `mise run %s`: no such task" % n)
    elif n not in screened and n not in NO_BUILD:
        err("bot rule `mise run %s`: the task builds without the screen" % n)

# Claude Code also takes permissions and hooks (shell commands) from the
# repository: .claude/settings*.json, and `allowed-tools` in skills and
# commands. None of it is needed, and none of it would be read by the rules
# above, so a committed one is refused; adding one means changing this
# check, in review. Committed only: CI and the bot see nothing else, and a
# local settings.local.json is the developer's own.
tracked = subprocess.run(["git", "ls-files", "-z", "--", ".claude"], check=True,
                         stdout=subprocess.PIPE, universal_newlines=True).stdout.split("\0")
for path in filter(None, tracked):
    if re.fullmatch(r"\.claude/settings[^/]*\.json", path):
        err("%s: committed Claude Code settings can grant the bot tools or run hooks" % path)
    elif path.endswith(".md"):
        with open(path) as f:
            if re.search(r"allowed[-_]tools", f.read(), re.I):
                err("%s names allowed-tools, which grants tools while it is active" % path)

if errors:
    for e in dict.fromkeys(errors):
        print("task-screen: " + e)
    sys.exit(1)
print("task-screen: %d tasks, %d screened, %d not building; bot tasks: %s"
      % (len(tasks), len(screened), len(tasks) - len(screened), ", ".join(allowed)))
