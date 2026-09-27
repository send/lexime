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

# fixture: a fresh git checkout of the files the check reads (it lists
# .claude/ through git, as only committed files reach CI and the bot).
fixture() {
  local dir
  dir=$(mktemp -d "$tmp/case.XXXXXX")
  mkdir -p "$dir/.github/workflows"
  cp "$repo/mise.toml" "$dir/mise.toml"
  cp "$repo/.github/workflows/claude.yml" "$dir/.github/workflows/claude.yml"
  mkdir -p "$dir/.claude"
  cp -R "$repo/.claude/skills" "$dir/.claude/skills"
  git -C "$dir" init -q
  git -C "$dir" add -A
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
# case fails only for its own change.
: >"$tmp/global.toml"
run_env=(env -u MISE_TASK_SKIP -u MISE_TASK_SKIP_DEPENDS
  MISE_GLOBAL_CONFIG_FILE="$tmp/global.toml"
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

d=$(fixture)
edit "$d/mise.toml" 'depends = ["lint-toolchain"]' 'depends = ["lint-toolchain", "audit-deps"]'
expect "unscreened task depends on audit-deps" "task fmt runs audit-deps" "$d"

d=$(fixture)
edit "$d/mise.toml" 'run = "bash scripts/icon.sh"' 'run = [{ task = "audit-deps" }]'
expect "unscreened task runs audit-deps" "task icon runs audit-deps" "$d"

# --- mise settings ---
d=$(fixture)
printf '\n[settings]\ntask.skip_depends = true\n' >>"$d/mise.toml"
expect "[settings] task.skip_depends" "task.skip_depends is True" "$d"

d=$(fixture)
printf '\n[settings]\ntask.skip = ["screen"]\n' >>"$d/mise.toml"
expect "[settings] task.skip" "task.skip is ['screen']" "$d"

d=$(fixture)
expect "MISE_TASK_SKIP_DEPENDS in the environment" "task.skip_depends is True" "$d" MISE_TASK_SKIP_DEPENDS=1

# --- the bot's allowlist ---
yml=.github/workflows/claude.yml

d=$(fixture)
edit "$d/$yml" 'Bash(mise run fmt)' 'Bash(mise run bench)'
expect "bot allowed a screened task" pass "$d"

d=$(fixture)
edit "$d/$yml" 'Bash(mise run fmt)' 'Bash(mise run audit-deps)'
expect "bot allowed audit-deps" "bot rule \`mise run audit-deps\`: the task builds without the screen" "$d"

d=$(fixture)
edit "$d/$yml" 'Bash(mise run lint)' 'Bash(mise run lint:*)'
expect "bot prefix rule" "bot rule Bash(mise run lint:*)" "$d"

d=$(fixture)
edit "$d/$yml" 'Bash(mise run lint),' 'Bash(mise run lint),Bash(cargo test -p lex-core),'
expect "bot raw cargo" "bot rule Bash(cargo test -p lex-core)" "$d"

d=$(fixture)
edit "$d/$yml" 'Bash(gh pr checks:*)"' 'Bash(gh pr checks:*)" --allowed-tools "Bash(cargo build)"'
expect "second list, other spelling" "bot rule Bash(cargo build)" "$d"

d=$(fixture)
edit "$d/$yml" 'Bash(gh pr checks:*)"' 'Bash(gh pr checks:*)" --allowedTools Bash(cargo)'
expect "unquoted list" "only 1 parse" "$d"

d=$(fixture)
edit "$d/$yml" 'Bash(gh pr checks:*)"' 'Bash(gh pr checks:*)" --dangerously-skip-permissions'
expect "permissions bypassed" "bypasses the permission rules" "$d"

d=$(fixture)
edit "$d/$yml" 'Bash(gh pr checks:*)' 'Bash(gh pr merge:*)'
expect "bot gh write rule" "bot rule Bash(gh pr merge:*)" "$d"

d=$(fixture)
edit "$d/$yml" '  MISE_OVERRIDE_CONFIG_FILENAMES: mise.toml' '  MISE_OVERRIDE_CONFIG_FILENAMES: mise.toml,.mise.toml'
expect "bot mise reads another config" "does not set MISE_OVERRIDE_CONFIG_FILENAMES: mise.toml" "$d"

d=$(fixture)
edit "$d/$yml" '          SCREEN_POLICY_REF: origin/main' $'          SCREEN_POLICY_REF: origin/main\n          MISE_TASK_SKIP_DEPENDS: 1'
expect "bot job skips depends" "sets a MISE_TASK* variable" "$d"

# --- Claude Code permissions committed to the repository ---
d=$(fixture)
printf '{"permissions": {"allow": ["Bash(cargo build:*)"]}}\n' >"$d/.claude/settings.json"
git -C "$d" add .claude/settings.json
expect "committed .claude/settings.json" ".claude/settings.json: committed Claude Code settings" "$d"

d=$(fixture)
printf '{}\n' >"$d/.claude/settings.local.json"
expect "uncommitted settings.local.json" pass "$d"

d=$(fixture)
edit "$d/.claude/skills/pre-push/SKILL.md" 'user-invocable: true' $'user-invocable: true\nallowed-tools: Bash(cargo:*)'
expect "skill granting tools" ".claude/skills/pre-push/SKILL.md names allowed-tools" "$d"

if [[ $fails -gt 0 ]]; then
  echo "check-task-screen-test: $fails failed"
  exit 1
fi
echo "check-task-screen-test: all passed"
