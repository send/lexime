#!/usr/bin/env python3
# The replay half of a PR-G-type withdrawal condition: after a ranking change
# ships, the next N rank>0 selections are replayed under the binaries from
# before and after the change, each on its own copy of the history frozen at
# install, at the same `--now`, and the per-selection movement is compared.
# python 3.9, as the Xcode python is.
#
#   replay-window.py --log commit-log.jsonl --start-line L [--rows 200] --count-only
#   replay-window.py --log commit-log.jsonl --start-line L [--rows 200]
#       --frozen DIR --app-dir DIR --dict lexime.dict --conn lexime.conn
#       --base LEXTOOL --new LEXTOOL --out DIR [--verbose]
#
# L is the commit log's line count recorded at the freeze: the window is the
# lines from L on, up to and including the N-th rank>0 line (a `rank` that is
# an integer > 0). --count-only prints how far the window has filled and
# exits 0 whether or not it is full.
#
# The replay cuts the log at the window's last line, takes that line's `t` as
# `--now` for both arms, and gives each run a fresh copy of the frozen
# history (DIR/user_history.lxud and its .wal: opening a history may evict
# and rewrite it) and of the app directory's user_dict.lxuw / settings.toml
# (`replay-commit-log --app-dir`, the configuration the app runs with). The
# base arm writes the baseline over the whole cut log; both arms report on
# the window (`--from-line L`), the new arm against that baseline. Both
# binaries must take `--now` (E2n, #390, and later).
#
# It prints counts only. The output directory holds the cut log and, with
# --verbose, each selection's reading and surface: personal input — keep it
# local, put only the counts in a PR or issue. What counts as a withdrawal
# (which movement, of which selections) is the plan's, read from the
# verbose files; this script stops at the counts every plan shares.
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile


def window(log, start, rows):
    """Lines of `log`, the index of the window's last line (None if not
    full), and the rank>0 lines seen from `start` on."""
    with open(log, encoding="utf-8") as f:
        lines = f.readlines()
    if start > len(lines):
        sys.exit(f"replay-window: --start-line {start} is past the log ({len(lines)} lines)")
    seen = 0
    for i in range(start, len(lines)):
        try:
            rank = json.loads(lines[i]).get("rank")
        except (ValueError, AttributeError):
            continue
        if isinstance(rank, int) and not isinstance(rank, bool) and rank > 0:
            seen += 1
            if seen == rows:
                return lines, i, seen
    return lines, None, seen


def fresh(src_dir, names, work):
    d = tempfile.mkdtemp(dir=work)
    for n in names:
        if os.path.exists(os.path.join(src_dir, n)):
            shutil.copyfile(os.path.join(src_dir, n), os.path.join(d, n))
    return d


def replay(a, binary, log, now, work, extra, name):
    hist = fresh(a.frozen, ("user_history.lxud", "user_history.lxud.wal"), work)
    app = fresh(a.app_dir, ("user_dict.lxuw", "settings.toml"), work)
    cmd = [binary, "--now", str(now), "replay-commit-log", a.dict, a.conn, log,
           "--history", os.path.join(hist, "user_history.lxud"), "--app-dir", app, "--json"] + extra
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"replay-window: {name}: exit {r.returncode}\n{r.stderr[-2000:]}")
    with open(os.path.join(a.out, name + ".json"), "w", encoding="utf-8") as f:
        f.write(r.stdout)
    if "--verbose" in extra:
        with open(os.path.join(a.out, name + ".verbose"), "w", encoding="utf-8") as f:
            f.write(r.stderr)
    return json.loads(r.stdout)


def main():
    p = argparse.ArgumentParser(description="Replay a withdrawal window under two binaries (see the header)")
    p.add_argument("--log", required=True)
    p.add_argument("--start-line", type=int, required=True)
    p.add_argument("--rows", type=int, default=200)
    p.add_argument("--count-only", action="store_true")
    for k in ("frozen", "app-dir", "dict", "conn", "base", "new", "out"):
        p.add_argument("--" + k)
    p.add_argument("--verbose", action="store_true")
    a = p.parse_args()

    lines, end, seen = window(a.log, a.start_line, a.rows)
    if a.count_only:
        print(f"rank>0 from line {a.start_line}: {seen}/{a.rows}, log {len(lines)} lines"
              + (f", window ends at line {end}" if end is not None else ""))
        return
    missing = [k for k in ("frozen", "app_dir", "dict", "conn", "base", "new", "out") if getattr(a, k) is None]
    if missing:
        p.error("replay needs --" + ", --".join(m.replace("_", "-") for m in missing))
    if end is None:
        sys.exit(f"replay-window: the window is not full: {seen}/{a.rows} rank>0 lines from line {a.start_line}")
    now = json.loads(lines[end])["t"]

    os.makedirs(a.out, exist_ok=True)
    log = os.path.join(a.out, "window.jsonl")
    with open(log, "w", encoding="utf-8") as f:
        f.writelines(lines[: end + 1])
    work = tempfile.mkdtemp(prefix="work-", dir=a.out)
    try:
        baseline = os.path.join(a.out, "base.baseline")
        v = ["--verbose"] if a.verbose else []
        win = ["--from-line", str(a.start_line)]
        replay(a, a.base, log, now, work, ["--emit-baseline", baseline], "base-full")
        base = replay(a, a.base, log, now, work, win + v, "base")
        new = replay(a, a.new, log, now, work, win + v + ["--baseline", baseline], "new")
    finally:
        shutil.rmtree(work)

    print(f"window: lines {a.start_line}..{end} ({end + 1 - a.start_line} lines, {a.rows} rank>0), --now {now}")
    for name, r in (("base", base), ("new", new)):
        print(f"  {name:5s} selections {r['selections']:5d}  page 1 {r['in_page']:5d}"
              f"  in list {r['in_list']:5d}  absent {r['absent']:5d}"
              f"  learned #1 {r['gap_below_top']:5d}")
    print("  new against base: " + json.dumps(new["baseline"]))


if __name__ == "__main__":
    main()
