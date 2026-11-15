#!/usr/bin/env python3
# The replay half of a PR-G-type withdrawal condition: after a ranking change
# ships, the next N rank>0 selections are replayed under the binaries from
# before and after the change, each on its own copy of the history frozen at
# install, at the same `--now`, and the per-selection movement is compared.
# python 3.9, as the Xcode python is.
#
#   replay-window.py count  --log commit-log.jsonl --start-line L [--rows 200]
#   replay-window.py replay --log commit-log.jsonl --start-line L [--rows 200]
#       --frozen DIR --app-dir DIR --out DIR
#       --base LEXTOOL --base-dict D --base-conn C
#       --new   LEXTOOL --new-dict D  --new-conn C
#       [--all-ranks] [--verbose]
#
# An arm is (binary, dict, conn), each given for each arm: a change that only
# moves the dictionary needs each arm to read its own (with the same dict on
# both, the movement is zero by construction). The same triple on both arms
# is allowed (an A/A run). Each arm's three paths and their sha256 go to
# window.txt and stdout, so a result names the artifacts it was measured on.
# `--out` must be absent or empty (a finished window is never overwritten).
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
# --all-ranks passes lextool `--all-ranks` (rank 0 selections are replayed
# too; both arms need a lextool from PR2a-t on, which has the flag). The window itself is unchanged (still from L to the N-th rank>0 line);
# the check becomes: both arms replayed every selection in that line range,
# rank 0 included, the same number.
#
# It prints counts only, and fails unless both arms replayed exactly the
# selections of the window (N rank>0 ones; with --all-ranks, all in the range). The output directory holds the cut log, the
# window's bounds and `--now` (window.txt: the time of a correction) and,
# with --verbose, each selection's reading and surface: personal input — keep it
# local, put only the counts in a PR or issue. What counts as a withdrawal
# (which movement, of which selections) is the plan's, read from the
# verbose files; this script stops at the counts every plan shares.
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile


def is_int(v):
    return isinstance(v, int) and not isinstance(v, bool) and v >= 0


def selection(line, min_rank=1):
    """The line's `t` if lextool replays it as a selection: UTF-8 JSON of
    the shape {t: u64, reading, surface, rank: usize} with rank > 0
    (rank_ops::read_selections; rank >= 0 under --all-ranks, min_rank=0).
    Anything else — a torn line included — lextool skips, and so does the
    window."""
    try:
        r = json.loads(line.decode("utf-8"))
    except ValueError:  # UnicodeDecodeError is a ValueError
        return None
    if (isinstance(r, dict) and is_int(r.get("t")) and isinstance(r.get("reading"), str)
            and isinstance(r.get("surface"), str) and is_int(r.get("rank")) and r["rank"] >= min_rank):
        return r["t"]
    return None


def window(log, start, rows):
    """Lines of `log` (bytes, split on b"\\n" as lextool does), the index and
    `t` of the window's last line (None, None if not full), and the
    rank>0 selections seen from `start` on."""
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


def count_selections(lines, start, end):
    """Every selection (rank 0 included) in lines start..end: what --all-ranks replays."""
    return sum(selection(l, 0) is not None for l in lines[start : end + 1])


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def describe(name, arm):
    """One line per arm: its binary, dict and conn with their sha256."""
    return f"arm {name}: " + " ".join(f"{k}={v} sha256={sha256(v)}" for k, v in zip(("bin", "dict", "conn"), arm))


def fresh(src_dir, names, work):
    d = tempfile.mkdtemp(dir=work)
    for n in names:
        if os.path.exists(os.path.join(src_dir, n)):
            shutil.copyfile(os.path.join(src_dir, n), os.path.join(d, n))
    return d


def replay(a, arm, log, now, work, app, extra, name, verbose=False):
    binary, dict_, conn = arm
    hist = fresh(a.frozen, ("user_history.lxud", "user_history.lxud.wal"), work)
    cmd = [binary, "--now", str(now), "replay-commit-log", dict_, conn, log,
           "--history", os.path.join(hist, "user_history.lxud"), "--app-dir", app, "--json"] + (["--all-ranks"] if a.all_ranks else []) + extra + (["--verbose"] if verbose else [])
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f"replay-window: {name}: exit {r.returncode}\n{r.stderr[-2000:]}")
    with open(os.path.join(a.out, name + ".json"), "w", encoding="utf-8") as f:
        f.write(r.stdout)
    if verbose:
        with open(os.path.join(a.out, name + ".verbose"), "w", encoding="utf-8") as f:
            f.write(r.stderr)
    return json.loads(r.stdout)


def gap_symbol(r):
    # Absent from the report of a lextool older than the symbol move (the main
    # arms): n/a, not 0.
    return f"{r['gap_symbol']:5d}" if "gap_symbol" in r else "  n/a"


def main():
    p = argparse.ArgumentParser(description="Replay a withdrawal window under two binaries (see the header)")
    sub = p.add_subparsers(dest="cmd", required=True)
    cmds = {"count": sub.add_parser("count"), "replay": sub.add_parser("replay")}
    for c in cmds.values():
        c.add_argument("--log", required=True)
        c.add_argument("--start-line", type=int, required=True)
        c.add_argument("--rows", type=int, default=200)
    for k in ("frozen", "app-dir", "out", "base", "base-dict", "base-conn",
              "new", "new-dict", "new-conn"):
        cmds["replay"].add_argument("--" + k, required=True)
    cmds["replay"].add_argument("--all-ranks", action="store_true")
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

    if os.path.exists(a.out) and (not os.path.isdir(a.out) or os.listdir(a.out)):
        sys.exit(f"replay-window: --out {a.out} exists and is not an empty directory (not overwritten)")
    arms = {"base": (a.base, a.base_dict, a.base_conn), "new": (a.new, a.new_dict, a.new_conn)}
    for name, arm in arms.items():
        for path in arm:
            if not os.path.isfile(path):
                sys.exit(f"replay-window: arm {name}: {path} is not a file")
    arm_lines = [describe(name, arm) for name, arm in arms.items()]
    os.makedirs(a.out, exist_ok=True)
    log = os.path.join(a.out, "window.jsonl")
    with open(log, "wb") as f:
        f.writelines(lines[: end + 1])
    work = tempfile.mkdtemp(prefix="work-", dir=a.out)
    try:
        app = fresh(a.app_dir, ("user_dict.lxuw", "settings.toml"), work)
        baseline = os.path.join(a.out, "base.baseline")
        win = ["--from-line", str(a.start_line)]
        base_arm, new_arm = arms["base"], arms["new"]
        replay(a, base_arm, log, now, work, app, ["--emit-baseline", baseline], "base-full")
        base = replay(a, base_arm, log, now, work, app, win, "base", a.verbose)
        new = replay(a, new_arm, log, now, work, app, win + ["--baseline", baseline], "new", a.verbose)
    finally:
        shutil.rmtree(work)

    with open(os.path.join(a.out, "window.txt"), "w", encoding="utf-8") as f:
        f.write(f"lines {a.start_line}..{end} --now {now}\n")
        f.write(f"mode {'--all-ranks' if a.all_ranks else 'rank>0'}\n")
        f.writelines(l + "\n" for l in arm_lines)
    d = new["baseline"]
    expect = count_selections(lines, a.start_line, end) if a.all_ranks else a.rows
    if not (base["selections"] == new["selections"] == expect and d["only_current"] == d["only_baseline"] == 0):
        sys.exit(f"replay-window: lextool replayed a different window: selections base {base['selections']} "
                 f"new {new['selections']} (expected {expect}), only_current {d['only_current']} "
                 f"only_baseline {d['only_baseline']} — the line filter here has drifted from rank_ops")
    print(f"window: {end + 1 - a.start_line} lines, {a.rows} rank>0"
          + (f", {expect} selections with --all-ranks" if a.all_ranks else "")
          + " (bounds, --now and mode in window.txt)")
    print("\n".join(arm_lines))
    for name, r in (("base", base), ("new", new)):
        print(f"  {name:5s} selections {r['selections']:5d}  page 1 {r['in_page']:5d}"
              f"  in list {r['in_list']:5d}  absent {r['absent']:5d}"
              f"  learned #1 {r['gap_below_top']:5d}"
              f"  learned symbol #1 {gap_symbol(r)}")
    print("  new against base: " + json.dumps(d))


if __name__ == "__main__":
    main()
