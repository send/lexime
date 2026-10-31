#!/usr/bin/env python3
# The replay half of a PR-G-type withdrawal condition: after a ranking change
# ships, the next N rank>0 selections are replayed under the binaries from
# before and after the change, each on its own copy of the history frozen at
# install, at the same `--now`, and the per-selection movement is compared.
# python 3.9, as the Xcode python is.
#
#   replay-window.py count  --log commit-log.jsonl --start-line L [--rows 200]
#   replay-window.py replay --log commit-log.jsonl --start-line L [--rows 200]
#       --frozen DIR --app-dir DIR --dict lexime.dict --conn lexime.conn
#       --base LEXTOOL --new LEXTOOL --out DIR [--verbose]
#
# L is the commit log's line count recorded at the freeze: the window is the
# lines from L on, up to and including the N-th rank>0 line (a `rank` that is
# an integer > 0). `count` prints how far the window has filled and exits 0
# whether or not it is full.
#
# The replay cuts the log at the window's last line, takes that line's `t` as
# `--now` for both arms, and gives each run a fresh copy of the frozen
# history (DIR/user_history.lxud and its .wal: opening a history may evict
# and rewrite it). The app directory's user_dict.lxuw / settings.toml
# (`replay-commit-log --app-dir`, the configuration the app runs with) is
# copied once, so a running app that edits them cannot split the arms; pass
# a copy taken at the freeze to replay under the configuration at install
# rather than today's. The
# base arm writes the baseline over the whole cut log; both arms report on
# the window (`--from-line L`), the new arm against that baseline. Both
# binaries must take `--now` (E2n, #390, and later).
#
# It prints counts only, and fails unless both arms replayed exactly the N
# selections of the window. The output directory holds the cut log, the
# window's bounds and `--now` (window.txt: the time of a correction) and,
# with --verbose, each selection's reading and surface: personal input — keep it
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


def is_int(v):
    return isinstance(v, int) and not isinstance(v, bool) and v >= 0


def selection(line):
    """The line's `t` if lextool replays it as a selection: UTF-8 JSON of
    the shape {t: u64, reading, surface, rank: usize} with rank > 0
    (rank_ops::read_selections). Anything else — a torn line included —
    lextool skips, and so does the window."""
    try:
        r = json.loads(line.decode("utf-8"))
    except ValueError:  # UnicodeDecodeError is a ValueError
        return None
    if (isinstance(r, dict) and is_int(r.get("t")) and isinstance(r.get("reading"), str)
            and isinstance(r.get("surface"), str) and is_int(r.get("rank")) and r["rank"] > 0):
        return r["t"]
    return None


def window(log, start, rows):
    """Lines of `log` (bytes, split on b"\\n" as lextool does), the index and
    `t` of the window's last line (None, None if not full), and the
    selections seen from `start` on."""
    with open(log, "rb") as f:
        parts = f.read().split(b"\n")
    lines = [p + b"\n" for p in parts[:-1]] + ([parts[-1]] if parts[-1] else [])
    if start > len(lines):
        sys.exit(f"replay-window: --start-line {start} is past the log ({len(lines)} lines)")
    seen = 0
    for i in range(start, len(lines)):
        t = selection(lines[i])
        if t is not None:
            seen += 1
            if seen == rows:
                return lines, i, t, seen
    return lines, None, None, seen


def fresh(src_dir, names, work):
    d = tempfile.mkdtemp(dir=work)
    for n in names:
        if os.path.exists(os.path.join(src_dir, n)):
            shutil.copyfile(os.path.join(src_dir, n), os.path.join(d, n))
    return d


def replay(a, binary, log, now, work, app, extra, name, verbose=False):
    hist = fresh(a.frozen, ("user_history.lxud", "user_history.lxud.wal"), work)
    cmd = [binary, "--now", str(now), "replay-commit-log", a.dict, a.conn, log,
           "--history", os.path.join(hist, "user_history.lxud"), "--app-dir", app, "--json"] + extra + (["--verbose"] if verbose else [])
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"replay-window: {name}: exit {r.returncode}\n{r.stderr[-2000:]}")
    with open(os.path.join(a.out, name + ".json"), "w", encoding="utf-8") as f:
        f.write(r.stdout)
    if verbose:
        with open(os.path.join(a.out, name + ".verbose"), "w", encoding="utf-8") as f:
            f.write(r.stderr)
    return json.loads(r.stdout)


def main():
    p = argparse.ArgumentParser(description="Replay a withdrawal window under two binaries (see the header)")
    sub = p.add_subparsers(dest="cmd", required=True)
    cmds = {"count": sub.add_parser("count"), "replay": sub.add_parser("replay")}
    for c in cmds.values():
        c.add_argument("--log", required=True)
        c.add_argument("--start-line", type=int, required=True)
        c.add_argument("--rows", type=int, default=200)
    for k in ("frozen", "app-dir", "dict", "conn", "base", "new", "out"):
        cmds["replay"].add_argument("--" + k, required=True)
    cmds["replay"].add_argument("--verbose", action="store_true")
    a = p.parse_args()

    lines, end, now, seen = window(a.log, a.start_line, a.rows)
    if a.cmd == "count":
        print(f"rank>0 from line {a.start_line}: {seen}/{a.rows}, log {len(lines)} lines"
              + (f", window ends at line {end}" if end is not None else ""))
        return
    if not os.path.isdir(a.app_dir):
        # lextool refuses a missing --app-dir; an empty copy would hide the typo.
        sys.exit(f"replay-window: --app-dir {a.app_dir} is not a directory")
    if end is None:
        sys.exit(f"replay-window: the window is not full: {seen}/{a.rows} rank>0 lines from line {a.start_line}")

    os.makedirs(a.out, exist_ok=True)
    log = os.path.join(a.out, "window.jsonl")
    with open(log, "wb") as f:
        f.writelines(lines[: end + 1])
    work = tempfile.mkdtemp(prefix="work-", dir=a.out)
    try:
        app = fresh(a.app_dir, ("user_dict.lxuw", "settings.toml"), work)
        baseline = os.path.join(a.out, "base.baseline")
        win = ["--from-line", str(a.start_line)]
        replay(a, a.base, log, now, work, app, ["--emit-baseline", baseline], "base-full")
        base = replay(a, a.base, log, now, work, app, win, "base", a.verbose)
        new = replay(a, a.new, log, now, work, app, win + ["--baseline", baseline], "new", a.verbose)
    finally:
        shutil.rmtree(work)

    with open(os.path.join(a.out, "window.txt"), "w", encoding="utf-8") as f:
        f.write(f"lines {a.start_line}..{end} --now {now}\n")
    d = new["baseline"]
    if not (base["selections"] == new["selections"] == a.rows and d["only_current"] == d["only_baseline"] == 0):
        sys.exit(f"replay-window: lextool replayed a different window: selections base {base['selections']} "
                 f"new {new['selections']} (expected {a.rows}), only_current {d['only_current']} "
                 f"only_baseline {d['only_baseline']} — the line filter here has drifted from rank_ops")
    print(f"window: {end + 1 - a.start_line} lines, {a.rows} rank>0 (bounds and --now in window.txt)")
    for name, r in (("base", base), ("new", new)):
        print(f"  {name:5s} selections {r['selections']:5d}  page 1 {r['in_page']:5d}"
              f"  in list {r['in_list']:5d}  absent {r['absent']:5d}"
              f"  learned #1 {r['gap_below_top']:5d}")
    print("  new against base: " + json.dumps(d))


if __name__ == "__main__":
    main()
