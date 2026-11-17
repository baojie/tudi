#!/usr/bin/env python3
"""Generate deterministic synthetic directory trees for tudi benchmarks.

Fixture sets:

  flat-10k    one directory with 10,000 files
  flat-100k   one directory with 100,000 files
  deep-2m     a home-like tree with exactly 2,000,000 entries: mixed English
              and CJK names, many extensions, depths up to ~14, hidden
              directories, heavy directories that the index excludes
              (node_modules, .git/objects, target, __pycache__), symlinks
              (including a loop and a dangling one), a few non-UTF-8 names,
              and an empty unreadable directory.

Every set is fully determined by its name and the seed: names, sizes (sparse
files, no data written), and mtimes. After generating, the script prints a
SHA-256 digest of the tree (paths, kinds, sizes, mtimes, link targets) and
writes it to <out>/manifest-<set>.json. Running it again with --force must
print the same digest.

Usage:
  make_tree.py --all OUT                 generate every set under OUT
  make_tree.py --set deep-2m OUT         generate one set
  make_tree.py --digest PATH             print the digest of an existing tree
  make_tree.py --self-test               small end-to-end determinism check

OUT defaults to ~/.cache/tudi-fixtures. Do not use a tmpfs such as /tmp for
the large sets: it costs gigabytes of RAM and gives unrepresentative numbers.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import multiprocessing
import os
import random
import shutil
import stat
import sys
import tempfile
import time
import zlib

GENERATOR_VERSION = 1
DEFAULT_SEED = 20261118
DEFAULT_OUT = os.path.join(os.path.expanduser("~"), ".cache", "tudi-fixtures")

# Fixed reference instant so mtimes do not depend on when the script runs.
REF_EPOCH = 1_780_000_000  # 2026-05-28T20:26:40Z
MAX_DEPTH = 14

SETS = {
    "flat-10k": ("flat", 10_000),
    "flat-100k": ("flat", 100_000),
    "deep-2m": ("deep", 2_000_000),
}

EN_WORDS = (
    "report contract invoice budget meeting notes draft final summary plan "
    "design spec review slides data export backup photo scan receipt letter "
    "resume paper thesis chapter figure table results analysis proposal "
    "minutes agenda schedule travel tax bank statement insurance manual guide "
    "readme config setup install build release roadmap todo ideas journal "
    "diary lecture homework exam course syllabus recipe garden car house "
    "lease payroll quote order shipping customer vendor audit compliance "
    "benchmark profile trace dump sample fixture prototype mockup wireframe "
    "logo banner poster invitation certificate license warranty"
).split()

CJK_WORDS = (
    "合同 核对 季报 年报 发票 会议 纪要 报告 总结 计划 方案 预算 简历 论文 "
    "照片 扫描 收据 说明书 项目 设计 需求 测试 数据 备份 草稿 定稿 周报 月报 "
    "通知 申请 证明 合作协议 采购 报价单 客户 财务 人事 培训 课件 笔记 日记 "
    "旅行 账单 保险 体检 学习 资料 文档 演示 讲稿 规划 评审 清单 统计 汇总"
).split()

DIR_WORDS = (
    "projects work archive old new misc shared assets media images docs "
    "reports clients vendors finance personal family school research papers "
    "drafts exports imports scans photos videos music books courses notes "
    "templates scripts tools data backups releases 2019 2020 2021 2022 2023 "
    "2024 2025 2026 q1 q2 q3 q4"
).split()

CJK_DIR_WORDS = (
    "项目 工作 归档 资料 文档 照片 财务 客户 合同 会议 学习 课程 个人 家庭 "
    "备份 下载 素材 论文 报告 年度 季度 部门 研发 市场"
).split()

CODE_DIRS = "src lib tests docs examples benches scripts assets config build dist".split()
CODE_FILES = (
    "main mod lib utils helpers config index app server client model view "
    "controller handler router parser lexer token types errors test spec"
).split()

# (extension, weight) for general files.
EXTENSIONS = [
    ("pdf", 12), ("docx", 8), ("xlsx", 6), ("pptx", 3), ("txt", 8), ("md", 8),
    ("jpg", 10), ("png", 6), ("heic", 1), ("mp4", 2), ("mov", 1), ("mp3", 2),
    ("zip", 3), ("tar.gz", 2), ("csv", 3), ("json", 3), ("html", 2), ("log", 3),
    ("py", 2), ("rs", 1), ("js", 1), ("svg", 1), ("odt", 1), ("epub", 1), ("", 2),
]
CODE_EXTENSIONS = [("rs", 4), ("py", 4), ("js", 4), ("ts", 3), ("json", 2), ("toml", 1),
                   ("md", 1), ("c", 1), ("h", 1), ("go", 1)]

TOP_LEVEL = [
    # (name, flavor, share of the total budget)
    ("Documents", "docs", 0.10),
    ("Downloads", "generic", 0.06),
    ("Pictures", "photos", 0.08),
    ("Music", "generic", 0.02),
    ("Videos", "generic", 0.01),
    ("Desktop", "generic", 0.005),
    ("projects", "code", 0.30),
    ("work", "docs", 0.12),
    ("文档", "docs", 0.05),
    ("下载", "generic", 0.02),
    ("notes", "docs", 0.02),
    ("archive", "generic", 0.08),
    (".config", "code", 0.03),
    (".local", "code", 0.08),
    ("src", "code", 0.015),
]


def _weighted(rng: random.Random, pairs):
    total = sum(w for _, w in pairs)
    r = rng.random() * total
    for value, w in pairs:
        r -= w
        if r < 0:
            return value
    return pairs[-1][0]


def _mtime(rng: random.Random) -> int:
    # Mostly recent, long tail into the past (up to ~10 years).
    age = min(rng.expovariate(1 / (200 * 86400)), 10 * 365 * 86400)
    return int(REF_EPOCH - age)


def _size(rng: random.Random, ext: str) -> int:
    big = {"mp4": 3.0, "mov": 3.0, "zip": 2.0, "tar.gz": 2.0, "jpg": 1.0,
           "png": 0.8, "heic": 1.0, "mp3": 1.5, "pdf": 0.8, "epub": 0.8}
    return int(rng.lognormvariate(9.5 + 2.0 * big.get(ext, 0.0), 1.6))


def _with_ext(stem: str, ext: str) -> str:
    return f"{stem}.{ext}" if ext else stem


def _file_name(rng: random.Random, flavor: str) -> tuple[str, str]:
    """Return (name, extension)."""
    if flavor == "photos":
        y = rng.randint(2015, 2026)
        stem = (f"IMG_{y}{rng.randint(1, 12):02d}{rng.randint(1, 28):02d}"
                f"_{rng.randint(0, 235959):06d}")
        ext = _weighted(rng, [("jpg", 8), ("heic", 2), ("png", 1), ("mp4", 1)])
        return _with_ext(stem, ext), ext
    if flavor == "code":
        ext = _weighted(rng, CODE_EXTENSIONS)
        stem = rng.choice(CODE_FILES)
        if rng.random() < 0.5:
            stem = f"{stem}_{rng.choice(CODE_FILES)}"
        return _with_ext(stem, ext), ext

    ext = _weighted(rng, EXTENSIONS)
    cjk = flavor == "docs" and rng.random() < 0.35 or rng.random() < 0.12
    k = rng.random()
    if cjk:
        a, b = rng.choice(CJK_WORDS), rng.choice(CJK_WORDS)
        if k < 0.3:
            stem = f"{a}{b}"
        elif k < 0.5:
            stem = f"{a}-{rng.randint(2018, 2026)}Q{rng.randint(1, 4)}"
        elif k < 0.7:
            stem = f"{rng.choice(EN_WORDS)}_{a}{b}"
        elif k < 0.85:
            stem = f"{a}（{b}）"
        else:
            stem = f"{rng.randint(2018, 2026)}年{rng.randint(1, 12)}月{a}"
    else:
        a, b = rng.choice(EN_WORDS), rng.choice(EN_WORDS)
        if k < 0.2:
            stem = a
        elif k < 0.4:
            stem = f"{a}-{b}"
        elif k < 0.55:
            stem = f"{a}_{rng.randint(2018, 2026)}-{rng.randint(1, 12):02d}"
        elif k < 0.65:
            stem = f"{a.capitalize()} {b.capitalize()} v{rng.randint(1, 12)}"
        elif k < 0.8:
            stem = f"{a}{rng.randint(1, 120)}"  # natural-sort cases: file2 < file10
        elif k < 0.9:
            stem = f"{a.upper()}_{b}"
        else:
            stem = f"{a} ({rng.randint(1, 9)})"
    return _with_ext(stem, ext), ext


def _dir_name(rng: random.Random, flavor: str) -> str:
    if flavor == "code":
        return rng.choice(CODE_DIRS + DIR_WORDS)
    if flavor == "photos":
        return f"{rng.randint(2015, 2026)}-{rng.randint(1, 12):02d}"
    if rng.random() < 0.2:
        return rng.choice(CJK_DIR_WORDS) + rng.choice(["", "", str(rng.randint(2018, 2026))])
    word = rng.choice(DIR_WORDS)
    return word if rng.random() < 0.6 else f"{word}-{rng.choice(EN_WORDS)}"


class Names:
    """Names already used in one directory, with O(1) amortized dedup."""

    def __init__(self) -> None:
        self.used: set[bytes] = set()
        self.next_k: dict[bytes, int] = {}

    def add(self, name: bytes) -> None:
        self.used.add(name)


def _unique(name: bytes, used: Names) -> bytes:
    if name not in used.used:
        used.used.add(name)
        return name
    stem, dot, ext = name.partition(b".")
    k = used.next_k.get(name, 2)
    while True:
        cand = stem + b"-" + str(k).encode() + (dot + ext if dot else b"")
        k += 1
        if cand not in used.used:
            used.used.add(cand)
            used.next_k[name] = k
            return cand


def _make_file(path: bytes, size: int, mtime: int) -> None:
    fd = os.open(path, os.O_CREAT | os.O_WRONLY | os.O_EXCL | os.O_CLOEXEC, 0o644)
    try:
        if size:
            os.ftruncate(fd, size)  # sparse: no data blocks written
    finally:
        os.close(fd)
    os.utime(path, (mtime, mtime))


class Builder:
    """Builds one subtree depth-first; consumes exactly `budget` entries."""

    def __init__(self, rng: random.Random):
        self.rng = rng
        self.count = 0
        self.dir_mtimes: list[tuple[bytes, int]] = []

    def files(self, path: bytes, n: int, flavor: str, used: Names) -> None:
        rng = self.rng
        for _ in range(n):
            name, ext = _file_name(rng, flavor)
            raw = name.encode()
            if rng.random() < 0.0005:
                # Non-UTF-8 name (Latin-1 byte), a feature under test.
                raw = raw.replace(b"a", b"caf\xe9", 1) if b"a" in raw else b"\xff" + raw
            if flavor != "code" and rng.random() < 0.01:
                raw = b"." + raw  # hidden file
            raw = _unique(raw, used)
            _make_file(os.path.join(path, raw), _size(rng, ext), _mtime(rng))
            self.count += 1

    def special(self, path: bytes, name: bytes, n: int, depth: int) -> None:
        """Excluded-by-default heavy directories with mechanical names."""
        rng = self.rng
        d = os.path.join(path, name)
        os.mkdir(d)
        self.count += 1
        n -= 1
        if name == b".git":
            objects = os.path.join(d, b"objects")
            os.mkdir(objects)
            self.count += 1
            n -= 1
            buckets: dict[int, list[bytes]] = {}
            # Choose distinct bucket dirs first so the count is exact.
            nb = min(256, max(0, n // 2))
            for b in rng.sample(range(256), nb):
                buckets[b] = []
            n -= nb
            keys = sorted(buckets)
            for _ in range(max(0, n)):
                buckets[rng.choice(keys)].append(b"%038x" % rng.getrandbits(152))
            mtime = _mtime(rng)
            for b in keys:
                bd = os.path.join(objects, b"%02x" % b)
                os.mkdir(bd)
                self.count += 1
                for h in buckets[b]:
                    _make_file(os.path.join(bd, h), rng.randint(100, 50_000), mtime)
                    self.count += 1
                os.utime(bd, (mtime, mtime))
            os.utime(objects, (mtime, mtime))
        else:
            # Nested like real node_modules/target trees, without further specials.
            self.tree(d, depth + 1, n, "code", allow_special=False)
        mtime = _mtime(rng)
        os.utime(d, (mtime, mtime))

    def tree(self, path: bytes, depth: int, budget: int, flavor: str,
             allow_special: bool = True) -> None:
        """Fill existing directory `path` with exactly `budget` entries."""
        rng = self.rng
        used = Names()
        if budget <= 0:
            return

        if allow_special and flavor == "code" and depth >= 1 and budget > 60 and rng.random() < 0.25:
            # A code project: some of the budget goes to excluded directories.
            for special, share in ((b".git", 0.25), (b"node_modules", 0.35),
                                   (b"target", 0.15), (b"__pycache__", 0.03)):
                if rng.random() < 0.5 and budget > 40:
                    n = max(4, int(budget * share * rng.random()))
                    n = min(n, budget - 20)
                    used.add(special)
                    self.special(path, special, n, depth)
                    budget -= n

        if rng.random() < 0.003 and budget > 3:
            # Symlinks: a loop back to the parent, and a dangling one.
            for raw, target in ((b"link-to-parent", b".."),
                                (b"dangling-link", b"does-not-exist")):
                os.symlink(target, os.path.join(path, _unique(raw, used)))
                self.count += 1
                budget -= 1

        if rng.random() < 0.0005 and budget > 1:
            locked = os.path.join(path, _unique(b"locked", used))
            os.mkdir(locked, 0o000)  # empty and unreadable
            self.count += 1
            budget -= 1

        if depth >= MAX_DEPTH:
            n_files, n_sub = budget, 0
        else:
            n_files = min(budget, int(rng.lognormvariate(2.3, 1.3)))
            rest = budget - n_files
            n_sub = 0 if rest <= 0 else min(rest, rng.randint(1, 9))
            if rest > 0 and n_sub == 0:
                n_files = budget

        self.files(path, n_files, flavor, used)
        rest = budget - n_files
        if n_sub:
            weights = [rng.paretovariate(1.1) for _ in range(n_sub)]
            pool = rest - n_sub  # each subdirectory costs one entry itself
            total_w = sum(weights)
            shares = [int(pool * w / total_w) for w in weights]
            shares[0] += pool - sum(shares)
            for share in shares:
                sub = os.path.join(path, _unique(_dir_name(rng, flavor).encode(), used))
                os.mkdir(sub)
                self.count += 1
                self.tree(sub, depth + 1, share, flavor, allow_special)
                mtime = _mtime(rng)
                os.utime(sub, (mtime, mtime))


def _build_top(args) -> int:
    root, name, flavor, budget, seed = args
    rng = random.Random(seed)
    b = Builder(rng)
    d = os.path.join(root, name.encode())
    os.mkdir(d)
    b.count += 1
    b.tree(d, 1, budget - 1, flavor)
    mtime = _mtime(rng)
    os.utime(d, (mtime, mtime))
    return b.count


def build_deep(root: bytes, total: int, seed: int, jobs: int) -> int:
    rng = random.Random(seed)
    shares = [int(total * s) for _, _, s in TOP_LEVEL]
    shares[TOP_LEVEL.index(next(t for t in TOP_LEVEL if t[0] == "projects"))] += total - sum(shares)
    tasks = [(root, name, flavor, share, rng.getrandbits(64))
             for (name, flavor, _), share in zip(TOP_LEVEL, shares)]
    # Largest first for better parallel packing; results do not depend on order.
    tasks.sort(key=lambda t: -t[3])
    if jobs > 1:
        with multiprocessing.Pool(jobs) as pool:
            counts = pool.map(_build_top, tasks, chunksize=1)
    else:
        counts = [_build_top(t) for t in tasks]
    return sum(counts)


def build_flat(root: bytes, total: int, seed: int) -> int:
    b = Builder(random.Random(seed))
    used = Names()
    b.files(root, total, "generic", used)
    return b.count


def digest(root: str | bytes) -> tuple[str, int]:
    """SHA-256 over (path, kind, size, mtime, link target) of every entry."""
    root = os.fsencode(root)
    h = hashlib.sha256()
    count = 0
    stack = [b""]
    while stack:
        rel = stack.pop()
        try:
            names = sorted(os.listdir(os.path.join(root, rel) if rel else root))
        except PermissionError:
            continue
        for name in names:
            rp = os.path.join(rel, name) if rel else name
            st = os.lstat(os.path.join(root, rp))
            count += 1
            if stat.S_ISLNK(st.st_mode):
                rec = b"l\0" + rp + b"\0" + os.readlink(os.path.join(root, rp))
            elif stat.S_ISDIR(st.st_mode):
                rec = b"d\0" + rp + b"\0%d" % int(st.st_mtime)
                stack.append(rp)
            else:
                rec = b"f\0" + rp + b"\0%d\0%d" % (st.st_size, int(st.st_mtime))
            h.update(rec + b"\n")
    return h.hexdigest(), count


def _remove(path: str) -> None:
    def onexc(func, p, exc):
        # Restore permissions on the unreadable test directory, then retry.
        os.chmod(p, 0o755)
        func(p)

    if sys.version_info >= (3, 12):
        shutil.rmtree(path, onexc=onexc)
    else:  # pragma: no cover
        shutil.rmtree(path, onerror=lambda f, p, e: onexc(f, p, e))


def generate(out: str, name: str, seed: int, force: bool, jobs: int,
             total: int | None = None) -> dict:
    kind, default_total = SETS[name]
    total = default_total if total is None else total
    target = os.path.join(out, name)
    if os.path.lexists(target):
        if not force:
            raise SystemExit(f"{target} exists; pass --force to regenerate")
        _remove(target)
    os.makedirs(target)
    set_seed = seed ^ zlib.crc32(name.encode())
    t0 = time.monotonic()
    if kind == "flat":
        made = build_flat(os.fsencode(target), total, set_seed)
    else:
        made = build_deep(os.fsencode(target), total, set_seed, jobs)
    os.utime(target, (REF_EPOCH, REF_EPOCH))
    elapsed = time.monotonic() - t0
    dig, count = digest(target)
    if made != total or count != total:
        raise SystemExit(f"{name}: expected {total} entries, made {made}, found {count}")
    manifest = {
        "set": name,
        "entries": count,
        "seed": seed,
        "generator_version": GENERATOR_VERSION,
        "sha256": dig,
    }
    with open(os.path.join(out, f"manifest-{name}.json"), "w") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")
    print(f"{name}: {count} entries in {elapsed:.1f}s, sha256 {dig}")
    return manifest


def self_test() -> None:
    sizes = {"flat-10k": 2_000, "deep-2m": 30_000}
    results = []
    for _ in range(2):
        with tempfile.TemporaryDirectory(prefix="tudi-fixture-selftest-") as tmp:
            results.append([
                generate(tmp, name, DEFAULT_SEED, False, 4, total=n)["sha256"]
                for name, n in sizes.items()
            ])
            # Another seed must give a different tree.
            other = generate(tmp, "flat-10k", DEFAULT_SEED + 1, True, 1, total=2_000)
            assert other["sha256"] != results[-1][0], "seed has no effect"
    assert results[0] == results[1], f"not deterministic: {results}"
    print("self-test ok")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--all", action="store_true", help="generate every fixture set")
    g.add_argument("--set", choices=sorted(SETS), action="append", help="generate one set")
    g.add_argument("--digest", metavar="PATH", help="print the digest of an existing tree")
    g.add_argument("--self-test", action="store_true", help="small determinism check")
    p.add_argument("out", nargs="?", default=DEFAULT_OUT, help=f"output directory (default {DEFAULT_OUT})")
    p.add_argument("--seed", type=int, default=DEFAULT_SEED)
    p.add_argument("--force", action="store_true", help="replace existing sets")
    p.add_argument("--jobs", type=int, default=min(16, os.cpu_count() or 1))
    a = p.parse_args()

    if a.self_test:
        self_test()
        return
    if a.digest:
        dig, count = digest(a.digest)
        print(f"{count} entries, sha256 {dig}")
        return
    os.makedirs(a.out, exist_ok=True)
    for name in sorted(SETS) if a.all else a.set:
        generate(a.out, name, a.seed, a.force, a.jobs)


if __name__ == "__main__":
    main()
