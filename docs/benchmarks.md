# Benchmarks

Budgets are defined in `docs/PLAN.md` §2. This file records how to run the
benchmarks and the latest measured numbers on the reference machine.

## Reference machine

To be filled by `scripts/bench_all.sh` (CPU, disk, kernel, GTK version).

## Fixtures

```sh
python3 benches/fixtures/make_tree.py --all ~/.cache/tudi-fixtures
```

| Set | Contents |
|-----|----------|
| `flat-10k` | one directory, 10,000 files |
| `flat-100k` | one directory, 100,000 files |
| `deep-2m` | realistic tree, ~2,000,000 entries |

## Results

No measurements yet (first numbers come from M1).
