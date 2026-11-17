# Benchmarks

Budgets are defined in `docs/PLAN.md` §2. This file records how to run the
benchmarks and the latest measured numbers on the reference machine.

## Reference machine

To be filled by `scripts/bench_all.sh` (CPU, disk, kernel, GTK version).

## Fixtures

```sh
python3 benches/fixtures/make_tree.py --all ~/.cache/tudi-fixtures
```

Sizes are sparse (apparent ~1.1 TB, ~1.5 GB on disk, mostly inodes);
exclude the fixture directory from backups.

| Set | Contents | SHA-256 (seed 20261118, generator v1) |
|-----|----------|------|
| `flat-10k` | one directory, 10,000 files | `3ec0188a…259767` |
| `flat-100k` | one directory, 100,000 files | `01d385c8…e746c5` |
| `deep-2m` | home-like tree, exactly 2,000,000 entries | `c6d8bb97…39c8c4` |

`deep-2m` shape: ~389k directories, max depth 17, largest directory ~2.7k
entries, ~272k CJK names, ~780 non-UTF-8 names, ~790 symlinks (loops and
dangling), ~100 empty unreadable directories. Generation takes 12–20 s on the
reference machine (16 processes).

## Results

No measurements yet (first numbers come from M1).
