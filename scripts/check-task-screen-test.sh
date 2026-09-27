#!/usr/bin/env bash
# Tests for scripts/check-task-screen.py, which checks that every mise task
# that builds depends on the supply-chain screen and that the @claude bot is
# allowed no build without it. Run by CI's task-screen job and by
# `mise run test-task-screen`.
#
# The first case is the check itself, on this repository. The others copy
# this repository's mise.toml and claude.yml and change one thing each, so
# they test the shapes those files really have.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
check="$repo/scripts/check-task-screen.py"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0

# fixture: a fresh copy of the two files the check reads.
fixture() {
  local dir
  dir=$(mktemp -d "$tmp/case.XXXXXX")
  mkdir -p "$dir/.github/workflows"
  cp "$repo/mise.toml" "$dir/mise.toml"
  cp "$repo/.github/workflows/claude.yml" "$dir/.github/workflows/claude.yml"
  echo "$dir"
}

# edit <file> <old> <new>: replace the first <old>, which must be there — a
# case whose edit no longer applies must fail, not test the unchanged file.
edit() {
  OLD=$2 NEW=$3 python3 -c '
import os, sys
p, old, new = sys.argv[1], os.environ["OLD"], os.environ["NEW"]
s = open(p).read()
if old not in s:
    sys.exit("edit: %r not in %s" % (old, p))
open(p, "w").write(s.replace(old, new, 1))
' "$1"
}

# expect <name> pass|<message> <dir> [env...]: run the check in <dir>, under
# run_env and then [env...].
expect() {
  local name=$1 want=$2 dir=$3 out rc=0
  shift 3
  out=$(cd "$dir" && "${run_env[@]}" "$@" python3 "$check" 2>&1) || rc=$?
  if [[ $want == pass && $rc -eq 0 ]] || [[ $want != pass && $rc -ne 0 && $out == *"$want"* ]]; then
    echo "ok   $name"
  else
    echo "FAIL $name (exit $rc, want: $want)"
    printf '%s\n' "$out" | sed 's/^/     /'
    fails=$((fails + 1))
  fi
}

# The repository, as the gates run it (ci.yml and claude.yml set this).
run_env=(env MISE_OVERRIDE_CONFIG_FILENAMES=mise.toml)
expect "this repository" pass "$repo"

# Fixtures see no global mise config or skip settings from the caller, so a
# case fails only for its own change. Their mise state goes under $tmp too:
# mise records every config it loads in its state dir (tracked-configs), and
# in the caller's that left ~30 links per run to fixtures deleted on exit.
: >"$tmp/global.toml"
run_env=(env -u MISE_TASK_SKIP -u MISE_TASK_SKIP_DEPENDS
  MISE_GLOBAL_CONFIG_FILE="$tmp/global.toml"
  MISE_STATE_DIR="$tmp/state"
  MISE_TRUSTED_CONFIG_PATHS="$tmp"
  MISE_OVERRIDE_CONFIG_FILENAMES=mise.toml)

d=$(fixture)
expect "copy of this repository" pass "$d"

# --- the task graph ---
d=$(fixture)
printf '\n[tasks.newbuild]\nrun = "cd engine && cargo build"\n' >>"$d/mise.toml"
expect "new task without the screen" "task newbuild does not depend on \`screen\`" "$d"

d=$(fixture)
printf '\n[tasks.newbuild]\ndepends = ["screen"]\nrun = "cd engine && cargo build"\n' >>"$d/mise.toml"
expect "new task depending on the screen" pass "$d"

d=$(fixture)
printf '\n[tasks.newbuild]\ndepends = ["dict"]\nrun = "cd engine && cargo build"\n' >>"$d/mise.toml"
expect "new task screened through its depends" pass "$d"

# mise loads file tasks too, which a parse of mise.toml would not see.
d=$(fixture)
mkdir -p "$d/mise-tasks"
printf '#!/usr/bin/env bash\ncd engine && cargo build\n' >"$d/mise-tasks/filebuild"
chmod +x "$d/mise-tasks/filebuild"
expect "file task without the screen" "task filebuild does not depend on \`screen\`" "$d"

d=$(fixture)
edit "$d/mise.toml" $'[tasks.engine-lib]\ndescription = "Build universal static library (x86_64 + aarch64)"\ndepends = ["screen"]\n' \
  $'[tasks.engine-lib]\ndescription = "Build universal static library (x86_64 + aarch64)"\n'
expect "leaf task's screen removed" "task engine-lib does not depend on \`screen\`" "$d"

d=$(fixture)
edit "$d/mise.toml" '[tasks.icon]' '[tasks.icons]'
expect "listed task renamed" "icon is listed in NO_BUILD but is not a task" "$d"

# mise would skip a screen with sources/outputs as up to date.
d=$(fixture)
edit "$d/mise.toml" 'run = "bash scripts/screen.sh"' $'sources = ["engine/Cargo.lock"]\noutputs = ["build/.screened"]\nrun = "bash scripts/screen.sh"'
expect "screen made skippable" "the \`screen\` task must be exactly" "$d"

d=$(fixture)
edit "$d/mise.toml" 'run = "bash scripts/screen.sh"' $'env = { SCREEN_POLICY_REF = false }\nrun = "bash scripts/screen.sh"'
expect "screen given its own env" "the \`screen\` task must be exactly" "$d"

d=$(fixture)
edit "$d/mise.toml" 'depends = ["screen", "lint-toolchain"]' 'depends = [{ task = "screen", env = { QUARANTINE_DAYS = "1" } }, "lint-toolchain"]'
expect "a depends entry with its own env" "unrecognized depends entry" "$d"

# mise's cargo backend compiles with cargo install, before any task runs.
d=$(fixture)
printf '\n[tasks.newtool]\ndepends = ["screen"]\ntools = { "cargo:ripgrep" = "14.1.1" }\nrun = "rg --version"\n' >>"$d/mise.toml"
expect "a task tool from cargo" "task newtool uses the tool cargo:ripgrep" "$d"

d=$(fixture)
printf '\n[tools]\n"cargo:ripgrep" = "14.1.1"\n' >>"$d/mise.toml"
expect "a repository tool from cargo" "sets the tool cargo:ripgrep" "$d"

# --- mise settings ---
d=$(fixture)
printf '\n[settings]\ntask.skip_depends = true\n' >>"$d/mise.toml"
expect "[settings] task.skip_depends" "task.skip_depends is True" "$d"

d=$(fixture)
printf '\n[settings]\ntask.skip = ["screen"]\n' >>"$d/mise.toml"
expect "[settings] task.skip" "task.skip is ['screen']" "$d"

d=$(fixture)
expect "MISE_TASK_SKIP_DEPENDS in the environment" "task.skip_depends is True" "$d" MISE_TASK_SKIP_DEPENDS=1

# --- the bot's tool list ---
yml=.github/workflows/claude.yml
shape="every allowedTools mention must be"

d=$(fixture)
edit "$d/$yml" 'Bash(mise run fmt)' 'Bash(mise run bench)'
expect "bot allowed a screened task" pass "$d"

d=$(fixture)
printf '\n[tasks.newbuild]\nrun = "cd engine && cargo build"\n' >>"$d/mise.toml"
edit "$d/$yml" 'Bash(mise run fmt)' 'Bash(mise run newbuild)'
expect "bot allowed an unscreened task" "bot rule \`mise run newbuild\`: the task builds without the screen" "$d"

d=$(fixture)
edit "$d/$yml" 'Bash(mise run fmt)' 'Bash(mise run nosuchtask)'
expect "bot allowed a missing task" "bot rule \`mise run nosuchtask\`: no such task" "$d"

# Each of these widens the list in a way a looser reader passed.
while IFS='|' read -r name old new; do
  d=$(fixture)
  edit "$d/$yml" "$old" "${new//\\t/$'\t'}"  # \t in the table is a tab
  expect "bot list: $name" "$shape" "$d"
done <<'CASES'
prefix rule|Bash(mise run lint)|Bash(mise run lint:*)
raw cargo|Bash(mise run lint),|Bash(mise run lint),Bash(cargo test -p lex-core),
gh write command|Bash(gh pr checks:*)|Bash(gh pr merge:*)
another tool|Bash(gh pr checks:*)"|Bash(gh pr checks:*),WebFetch"
an expression|Bash(gh pr checks:*)"|Bash(gh pr checks:*),${{ vars.EXTRA }}"
a tab before a rule|Bash(gh pr checks:*)"|Bash(gh pr checks:*),	Bash(cargo build:*)"
a stray parenthesis|Bash(gh pr checks:*)"|Bash(gh pr checks:*)),Bash(cargo build:*)"
a second list, other spelling|Bash(gh pr checks:*)"|Bash(gh pr checks:*)" --allowed-tools "Bash(cargo build)"
an unquoted list|Bash(gh pr checks:*)"|Bash(gh pr checks:*)" --allowedTools Bash(cargo)
a value after the list|Bash(gh pr checks:*)"|Bash(gh pr checks:*)" "Bash(cargo build:*)"
CASES

if [[ $fails -gt 0 ]]; then
  echo "check-task-screen-test: $fails failed"
  exit 1
fi
echo "check-task-screen-test: all passed"
