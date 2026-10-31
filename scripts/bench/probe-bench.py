#!/usr/bin/env python3
# Round-robin A/B runs of the speed probe (build-probe.sh) over arms.
# python 3.9, as the Xcode python is.
#
#   probe-bench.py wall|ins --dict D --conn C --readings R --user-dict U
#       --history H --now EPOCH [--modes hist1,nohist1,cands] [--laps 10]
#       [--iters 200] [--count 200] [--warmup 20] [--out DIR] name=probe ...
#
# The first arm is the base; an A/A arm "base2" (the base binary again, its
# own process) is added after it, so every delta has its noise floor beside
# it. Each (lap, arm) is one process on a fresh copy of H (and H.wal) at the
# same --now; each lap starts one arm later than the last, so position in
# the lap (page cache, thermal state) and drift land on every arm alike.
#
# wall: the probe's own timing (min and median over laps of total_mean_ns,
#   delta vs base). Waits for the 1-minute load to fall to LOADMAX (env,
#   default 3) before each run: a loaded machine skews the run, not the arm.
# ins: instructions retired and cycles elapsed of the whole process, from
#   /usr/bin/time -l (macOS). Instructions are what decides "same
#   computation": wall moves ±5% with code layout alone, instructions ±0.3%.
#
# With --out, the per-run numbers go to DIR/<cmd>.json.
import argparse
import json
import os
import re
import shutil
import statistics as st
import subprocess
import sys
import tempfile
import time


def run(a, binary, work, wrap):
    d = tempfile.mkdtemp(dir=work)
    hist = os.path.join(d, os.path.basename(a.history))
    for src in (a.history, a.history + ".wal"):
        if os.path.exists(src):
            shutil.copyfile(src, os.path.join(d, os.path.basename(src)))
    cmd = wrap + [binary, "--dict-file", a.dict, "--conn-file", a.conn,
                  "--readings-file", a.readings, "--user-dict", a.user_dict,
                  "--history", hist, "--now", str(a.now), "--modes", a.modes,
                  "--warmup", str(a.warmup), "--iters", str(a.iters), "--count", str(a.count)]
    r = subprocess.run(cmd, capture_output=True, text=True)
    shutil.rmtree(d)
    if r.returncode != 0:
        sys.exit(f"probe-bench: {binary}: exit {r.returncode}\n{r.stderr[-2000:]}")
    return r


def wall(a, binary, work):
    while os.getloadavg()[0] > float(os.environ.get("LOADMAX", "3")):
        time.sleep(10)
    r = run(a, binary, work, [])
    return {l.split()[0]: float(l.split()[2]) for l in r.stdout.splitlines() if "total_mean_ns" in l}


def ins(a, binary, work):
    r = run(a, binary, work, ["/usr/bin/time", "-l"])
    out = {}
    for k in ("instructions_retired", "cycles_elapsed"):
        m = re.search(r"(\d+)\s+" + k.replace("_", " "), r.stderr)
        if m is None:
            sys.exit(f"probe-bench: no '{k}' in /usr/bin/time -l output (macOS only)")
        out[k] = int(m.group(1))
    return out


def pct(x, base):
    return 100 * (x / base - 1)


def main():
    p = argparse.ArgumentParser(description="Round-robin A/B runs of the speed probe (see the header)")
    p.add_argument("cmd", choices=("wall", "ins"))
    for k in ("dict", "conn", "readings", "user-dict", "history"):
        p.add_argument("--" + k, required=True)
    p.add_argument("--now", type=int, required=True)
    p.add_argument("--modes", default="hist1,nohist1,cands")
    p.add_argument("--laps", type=int, default=10)
    p.add_argument("--iters", type=int, default=200)
    p.add_argument("--count", type=int, default=200)
    p.add_argument("--warmup", type=int, default=20)
    p.add_argument("--out")
    p.add_argument("arms", nargs="+", metavar="name=probe")
    a = p.parse_args()
    arms = [tuple(x.split("=", 1)) for x in a.arms]
    if any(len(x) != 2 for x in arms):
        p.error("arms are name=probe")
    arms.insert(1, ("base2", arms[0][1]))
    if len({n for n, _ in arms}) != len(arms):
        p.error("arm names must be distinct (base2 is the added A/A arm)")
    base = arms[0][0]
    measure = wall if a.cmd == "wall" else ins

    res = {n: [] for n, _ in arms}
    work = tempfile.mkdtemp(prefix="probe-bench-")
    try:
        for lap in range(a.laps):
            k = lap % len(arms)
            for n, b in arms[k:] + arms[:k]:
                m = measure(a, b, work)
                res[n].append(m)
                print(lap, n, m, flush=True)
    finally:
        shutil.rmtree(work)
    if a.out:
        os.makedirs(a.out, exist_ok=True)
        with open(os.path.join(a.out, a.cmd + ".json"), "w", encoding="utf-8") as f:
            json.dump(res, f, indent=1)

    for k in res[base][0]:
        lo = {n: min(x[k] for x in res[n]) for n, _ in arms}
        med = {n: st.median(x[k] for x in res[n]) for n, _ in arms}
        for n, _ in arms:
            line = f"{k:22s} {n:12s} min {lo[n]:16.0f}  med {med[n]:16.0f}"
            if n != base:
                line += f"  dmin {pct(lo[n], lo[base]):+6.2f}%  dmed {pct(med[n], med[base]):+6.2f}%"
            print(line)


if __name__ == "__main__":
    main()
