#!/usr/bin/env python3
"""M7 acceptance measurements: live index updates through tudi-fanotifyd.

  m7_live.py p10 [-n 10]     touch ~/tudi-m7/xN/y/<unique>.txt, time until
                             `tudi q` finds it (P10 budget: < 1 s)
  m7_live.py clone           git clone a ~100k-file repository into
                             ~/tudi-m7/, sample the daemon's RSS, and time
                             until the clone's files are findable after the
                             clone ends (budget: < 5 s lag, < 2x RSS,
                             sampled every 50 ms until 70 s after the end)

The clone source is generated once under ~/.cache/tudi-fixtures (excluded
from the index): 2,000 folders x 50 files, committed to a git repository.
Everything under ~/tudi-m7 is removed afterwards unless --keep is given.
Run with the daemon already up (`tudi index status`), and check its roots
show "live" when measuring with the helper.
"""

from __future__ import annotations

import argparse
import os
import random
import shutil
import statistics
import subprocess
import sys
import time
import uuid
from pathlib import Path

HOME = Path.home()
WORK = HOME / "tudi-m7"
SRC = HOME / ".cache" / "tudi-fixtures" / "clone-src-100k"
DIRS, FILES = 2000, 50


def query(text: str) -> list[str]:
    r = subprocess.run(["tudi", "q", text], capture_output=True, text=True)
    return r.stdout.splitlines()


def found(name: str) -> bool:
    return any(line.rstrip().endswith(name) for line in query(name))


def wait_found(names: list[str], timeout: float) -> float | None:
    t0 = time.monotonic()
    left = list(names)
    while left:
        left = [n for n in left if not found(n)]
        if not left:
            break
        if time.monotonic() - t0 > timeout:
            return None
        time.sleep(0.01)
    return time.monotonic() - t0


def daemon_pid() -> int:
    out = subprocess.run(["pgrep", "-x", "tudi-indexd"], capture_output=True, text=True).stdout.split()
    if not out:
        sys.exit("tudi-indexd is not running (start it with `tudi index status`)")
    return int(out[0])


def rss_mb(pid: int) -> float:
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) / 1024
    return 0.0


def status() -> None:
    print(subprocess.run(["tudi", "index", "status"], capture_output=True, text=True).stdout.strip())


def p10(n: int, keep: bool) -> None:
    status()
    lat = []
    for i in range(n):
        d = WORK / f"x{i}" / "y"
        d.mkdir(parents=True, exist_ok=True)
        name = f"m7probe{uuid.uuid4().hex[:12]}.txt"
        time.sleep(0.3)  # let the folder creation settle first
        t0 = time.monotonic()
        (d / name).write_bytes(b"")
        dt = wait_found([name], 60.0)
        lat.append(dt if dt is not None else float("inf"))
        print(f"  {i}: {'not found in 60 s' if dt is None else f'{dt * 1000:.0f} ms'}")
    fin = [x for x in lat if x != float("inf")]
    if fin:
        print(
            f"P10 n={n}: min {min(fin) * 1000:.0f} ms, median {statistics.median(fin) * 1000:.0f} ms, "
            f"max {max(fin) * 1000:.0f} ms, missed {len(lat) - len(fin)}"
        )
    if not keep:
        shutil.rmtree(WORK, ignore_errors=True)


def make_source() -> None:
    if (SRC / ".git").is_dir():
        return
    print(f"generating {DIRS * FILES} files in {SRC} (once) ...")
    rng = random.Random(7)
    for d in range(DIRS):
        sub = SRC / f"pkg{d // 100:02d}" / f"mod{d:04d}"
        sub.mkdir(parents=True, exist_ok=True)
        for f in range(FILES):
            (sub / f"c{d:04d}f{f:02d}m7.txt").write_text(f"{rng.random()}\n")
    env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
               GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com")
    subprocess.run(["git", "init", "-q"], cwd=SRC, check=True)
    subprocess.run(["git", "add", "-A"], cwd=SRC, check=True)
    subprocess.run(["git", "commit", "-qm", "fixture"], cwd=SRC, check=True, env=env)


def clone(keep: bool) -> None:
    make_source()
    status()
    pid = daemon_pid()
    base = rss_mb(pid)
    dst = WORK / f"clone-{int(time.time())}"
    WORK.mkdir(exist_ok=True)
    time.sleep(1)
    peak = base
    t0 = time.monotonic()
    p = subprocess.Popen(["git", "clone", "-q", "--no-local", str(SRC), str(dst)])
    while p.poll() is None:
        peak = max(peak, rss_mb(pid))
        time.sleep(0.05)
    t_clone = time.monotonic() - t0
    # The last-written files and a random sample.
    names = [f"c{d:04d}f{f:02d}m7.txt" for d in range(DIRS - 5, DIRS) for f in range(0, FILES, 10)]
    rng = random.Random(1)
    names += [f"c{rng.randrange(DIRS):04d}f{rng.randrange(FILES):02d}m7.txt" for _ in range(25)]
    t1 = time.monotonic()
    lag = None
    while lag is None:
        peak = max(peak, rss_mb(pid))
        names = [n for n in names if not found(n)]
        if not names:
            lag = time.monotonic() - t1
        elif time.monotonic() - t1 > 900:
            break
        else:
            time.sleep(0.05)
    # Keep sampling for 70 s: the daemon refreshes its search data and
    # snapshot within a minute after changes; that peak counts too.
    end = time.monotonic() + 70
    while time.monotonic() < end:
        peak = max(peak, rss_mb(pid))
        time.sleep(0.05)
    print(f"clone of {DIRS * FILES} files: {t_clone:.1f} s")
    print(f"lag after clone end: {'> 900 s' if lag is None else f'{lag:.2f} s'}")
    print(f"RSS: before {base:.0f} MB, peak {peak:.0f} MB ({peak / base:.2f}x), now {rss_mb(pid):.0f} MB")
    if not keep:
        shutil.rmtree(WORK, ignore_errors=True)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("what", choices=["p10", "clone"])
    ap.add_argument("-n", type=int, default=10)
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    if a.what == "p10":
        p10(a.n, a.keep)
    else:
        clone(a.keep)


if __name__ == "__main__":
    main()
