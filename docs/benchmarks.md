# Benchmarks

Budgets are defined in `docs/PLAN.md` §2. This file records how to run the
benchmarks and the latest measured numbers on the reference machine.

## Reference machine

Ubuntu 26.04, kernel 7.0, GNOME on Wayland, 22 cores, 30 GB RAM, NVMe SSD.
Hybrid graphics: Intel Arc (Meteor Lake) + NVIDIA RTX 4060 Mobile; 1920×1080
at 60 Hz. GTK 4.22.4, libadwaita 1.9.1. Desktop theme Greybird with a user
`gtk.css`, icon theme breeze. (`scripts/bench_all.sh` will record this
automatically from M3 on.)

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

### M1 — GTK4 list spike (2026-11-18)

Run with `spikes/gtk4-list/bench.sh`. 100,000 synthetic rows, custom
`gio::ListModel`, `MultiSelection`, release build, default GSK renderer (GL).

**Startup (P1, budget < 150 ms)** — `main()` → first `after-paint`, median of
5–7 runs in the desktop session. External launch adds 30–35 ms.

| Configuration | Startup | GTK/adw init | `present()` | first paint |
|---|---|---|---|---|
| default (GL on NVIDIA, Greybird, breeze, themed icons) | 370–450 ms | ~105 ms | ~160 ms | ~90 ms |
| Mesa EGL (Intel iGPU) | 346 ms | 110 | 88 | 119 |
| Mesa EGL + `GTK_THEME=Adwaita` | 305 ms | 57 | 79 | 140 |
| Mesa EGL + Adwaita, 0 rows (empty window) | 237 ms | 77 | 65 | 94 |
| cairo renderer + Adwaita, 0 rows (empty window) | 157 ms | 63 | 31 | 61 |
| Vulkan renderer (NVIDIA) | 524 ms | 111 | 282 | 101 |

Findings: by default GTK picks the NVIDIA EGL vendor (dGPU wake-up, ~70 ms
extra in `present()`); themed (non-symbolic) icons are decoded through glycin,
which spawns a `bwrap`-sandboxed loader process; fontconfig opens ~400 font
files for the CJK fallback on first paint. Even an empty libadwaita window
does not reach 150 ms with the GL renderer.

**Scroll (P5) and insert (P6)** — run in a private headless gnome-shell
(virtual 1920×1080@60) because an occluded window on the desktop gets no frame
callbacks. Scroll at a constant 50,000 px/s through the whole list (every
frame shows entirely new rows); 5 inserts and removes of 10,000 rows in the
visible area, triggered from a main-loop callback between frames.

| Metric | ListView | ColumnView | Budget |
|---|---|---|---|
| frames measured | 5,749 | 4,186 | |
| frame interval p50 / p99 / max | 16.67 / 17.4 / 164 ms | 16.67 / 18.2 / 179 ms | P5: no frame > 16.7 ms at p99 |
| dropped frames (interval > 25 ms) | 3 (0.05 %) | 5 (0.12 %) | |
| frame work time p99 / max | 8.9 / 15.8 ms | 10.8 / 25.5 ms | |
| frames with work > 16.7 ms | 0 | 1 | |
| insert 10k rows: splice / → next paint (max) | 3.4 / 14.4 ms | 3.8 / 14.7 ms | P6: < 50 ms |
| remove 10k rows → next paint (max) | 14.4 ms | 14.3 ms | |

P6 is met by both variants with a wide margin. For P5, the p99 frame interval
is above 16.7 ms only because of vsync jitter (16.67 ms ± 1 ms); the p99 of
dropped frames is zero and ListView's work time never exceeds a frame. These
are headless numbers; they still need confirming on the real desktop.

### M2 — core listing engine (2026-11-18)

`cargo bench -p tudi-core --bench listing`, criterion medians, warm cache,
fixtures on ext4/NVMe, load average < 2.5. An earlier run under unrelated
load (≥ 18) was discarded.

| Benchmark | flat-10k | flat-100k | Budget |
|---|---|---|---|
| list + sort (no stat) | 2.68 ms | 31.8 ms | < 10 ms / < 80 ms ✅ |
| `getdents64` listing only | 1.55 ms | 18.4 ms | — |
| natural name rank | 1.04 ms | 9.38 ms | — |
| display order (dirs first) | 0.02 ms | 0.21 ms | — |
| metadata (`statx`, 8 threads) | 3.56 ms | — | — |

Listing is dominated by kernel time in `getdents64`. Natural sorting uses
precomputed byte keys compared by an 8-byte prefix (`docs/design/listing.md`);
before that change the 100k name rank took ~62 ms.

### M3 — two-pane browser (2026-11-18, in progress)

Release build, `TUDI_BENCH` scenarios (`crates/tudi-gui/src/bench.rs`).
P1b and P2 on the desktop session (cold start needs no focused window);
P1a, P3, P4 in a headless gnome-shell (`scripts/headless.sh`).

| Budget | Measured | Status |
|---|---|---|
| P1a window from the resident instance < 150 ms | 22–32 ms (3 runs) | ✅ |
| P1b cold start < 400 ms | 340–375 ms in process (5 runs) | ✅ |
| P2 second launch < 50 ms | 10–11 ms with the GIO-only launcher (was 45–53 ms) | ✅ |
| P3 open 10k-entry folder < 50 ms | median 43 ms, max 77 ms (10 runs) | ✅ median |
| P4 open 100k-entry folder < 250 ms | median 142 ms, max 207 ms (10 runs) | ✅ |
| P5, P6 in tudi itself | not yet measured | open |
| No main-thread stall > 16 ms (scripted session) | not met yet: stalls on large re-sorts and jumps; runs so far were under unrelated load (≥ 9) | open |

The scripted session found that replacing all items made GTK recreate its
~200 cached row widgets on every re-sort; items are now positions and the
widgets are reused (`docs/design/ui.md`).

### M5/M6 — index and memory search (2026-11-18)

`deep-2m` fixture, release build, load average < 2
(`cargo run --release -p tudi-index --example scan|search`).

| Metric | Measured | Budget |
|---|---|---|
| P9 initial scan (1,830,862 entries after default excludes) | 3.1 s | < 20 s ✅ |
| P8 RSS after scan | 99 MB | < 150 MB ✅ |
| Snapshot save / load (69 MB file) | 57 ms / 79 ms | — |
| Search data build (bags, pinyin) / typo vocabulary | 86 ms / 471 ms (background) | — |
| P7 query over 1.83M entries, 12 queries × 5 | p50 7.6 ms, p95 23.2 ms, max 34.9 ms | < 30 ms p95 ✅ |

Single-character queries are the slowest (~23 ms): almost every name
passes the character-bag prefilter.

### M8 — query filters (2026-11-18)

Home index (1.85M entries), release build, **load average 7–9** (busy
machine; the default set measured p95 38–43 ms both before and after M8, so
the filters add no cost to plain queries; P7 to be re-measured when idle).

| Query | Time |
|---|---|
| `.pdf 上月` (filters only) | 4.2 ms |
| `size>1G` / `today` / `<7d .md` | 3.2 / 4.6 / 5.5 ms |
| `合同 .pdf` / `in:projects main` / `dir: tudi` | 20.2 / 21.5 / 12.7 ms |

**P8 on the real home index is over budget (open).** `tudi-indexd`
started from the snapshot of a real home folder (1.85M entries, longer and
more often Chinese names than the fixture): 181–197 MB once ready,
195–212 MB after the first queries (typo vocabulary built), the same before
M5, before M8 and after M8 (two runs each). Budget < 150 MB. The fixture
measurement above (99 MB after scan, ≈ 140 MB with search data) does not
represent real names; the index arrays plus search data alone take
≈ 172 MB here (`examples/search`).

Pinyin collation (optional, worker thread), `flat-100k` (12 % CJK names):
name ranking 10.6 ms → 14.3 ms (`cargo bench -p tudi-core -- name-rank`).
