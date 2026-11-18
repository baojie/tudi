# Decisions

Short ADR-style records: context / decision / consequences. Newest last.

## ADR-001: GTK4 + libadwaita for the GUI

- **Context.** tudi targets GNOME on Wayland first and must work with fcitx5
  and IBus input methods, GIO trash/mounts/open-with, and the system theme.
  The main risk is list performance with 100k+ rows.
- **Decision.** Use GTK4 (≥ 4.14) and libadwaita (≥ 1.5) through the
  `gtk4`/`libadwaita` crates, with our own `gio::ListModel` over a Rust row
  array. Code is compiled against the 4.14 / 1.5 feature level so CI on
  Ubuntu 24.04 builds.
- **Consequences.** Milestone M1 is a decision gate: if neither `ListView`
  nor `ColumnView` meets P1, P5, P6, we stop and choose between GTK tuning
  and a Qt6 fallback before writing real GUI code.

## ADR-002: Optional privileged fanotify helper

- **Context.** inotify cannot watch a whole home directory cheaply; fanotify
  with `FAN_MARK_FILESYSTEM` can, but needs `CAP_SYS_ADMIN`.
- **Decision.** A tiny root helper, `tudi-fanotifyd`, forwards change events
  to user daemons over a socket. It is subscribe-only, filters by peer uid,
  keeps minimal capabilities, has no async runtime, and is packaged
  separately. The user daemon works without it (inotify on frecent
  directories, periodic reconcile, GUI listing hints).
- **Consequences.** Two change paths must be tested. The helper needs a
  documented security review (M7).

## ADR-003: The name "tudi"

- **Context.** Tudi (土地) is the Chinese earth god who knows every inch of
  his local land.
- **Decision.** The project, binary, and crate prefix are `tudi`. App ID:
  `io.github.baojie.tudi`.
- **Consequences.** Short to type for the CLI (`tudi q …`); no clash with
  common Linux packages.

## ADR-004: Maintainer notes live in a git-ignored `private/` directory

- **Context.** The plan originally made `private/` a git submodule. The
  maintainer does not need a separate repository for these notes.
- **Decision.** `private/` is a plain local directory listed in `.gitignore`.
  Nothing from it is ever committed or copied into public files.
- **Consequences.** The notes are not backed up by the repository; the
  maintainer handles that.

## ADR-005: Extra index roots on slow or limited filesystems are in v0.1

- **Context.** The reference machine has a large data disk that is a
  rotational HDD formatted as exFAT. exFAT has no file-handle export (so no
  fanotify FID events), no symlinks, no stable inode numbers, and
  case-insensitive names. Reconciling a large HDD every 10 minutes would
  keep it spinning.
- **Decision.** `[[index.extra_roots]]` with per-root settings (reconcile
  interval, excludes). The daemon detects filesystem type and rotation per
  root, skips fanotify where unsupported, defaults rotational roots to a
  daily reconcile, and treats unmounted roots as offline.
- **Consequences.** M5 gains tasks and acceptance criteria for this; M5 must
  verify whether exFAT updates directory mtime on entry changes before the
  reconcile pass relies on it.

## ADR-006: Frecency store is a `tudi-core` library; writers write directly

- **Context.** Bookmarks/frecency (M4) come before the daemon (M5), and the
  GUI must work without the daemon. The daemon also needs frecency weights
  for ranking.
- **Decision.** The frecency store lives in `tudi-core` on SQLite in WAL
  mode. The GUI and the shell hook write visits directly. The daemon only
  reads, reloading periodically or on a `FrecencyChanged` hint.
- **Consequences.** No `Visit` IPC message; two processes open the same
  database, which WAL handles.

## ADR-007: Allow MPL-2.0 for `nucleo-matcher`

- **Context.** The project is MIT OR Apache-2.0. `nucleo-matcher` is
  MPL-2.0, a file-level copyleft license.
- **Decision.** Allow `MPL-2.0` in `deny.toml`. Use the crate unmodified as a
  dependency; any change to its files would have to be published under
  MPL-2.0.
- **Consequences.** Binary distributions must make the source of MPL files
  available (crates.io already does) and include its license text.

## ADR-008: Pinned toolchain and edition

- **Context.** The plan requires a pinned stable Rust. The reference
  machine's default rustup toolchain is nightly.
- **Decision.** `rust-toolchain.toml` pins `1.94.0` (latest stable at
  bootstrap) with rustfmt and clippy; crates use edition 2024 and
  `rust-version = "1.94"`.
- **Consequences.** Bumping the toolchain is a deliberate commit.

## ADR-009: Benchmark fixtures live on a real disk

- **Context.** `/tmp` is tmpfs on many systems (including the reference
  machine). A 2M-entry tree there costs GBs of RAM and its
  `getdents`/`statx` costs differ from ext4.
- **Decision.** The fixture generator defaults to `~/.cache/tudi-fixtures`.
- **Consequences.** Benchmarks and docs use that path.

## ADR-010: Two-pane layout with a browser-style bookmarks bar

- **Context.** The original plan had a single expandable tree containing
  both folders and files, with bookmarks in a sidebar. The maintainer prefers
  the classic layout.
- **Decision.** Left pane: folder tree, directories only, with Home, File
  System, and mounted volumes as roots. Right pane: the selected folder's
  contents as a flat details list (name/size/modified, sortable); folders are
  entered by double-click/`Enter`, not expanded in place. Bookmarks live in a
  browser-style bar below the header (`Alt+1..9`), with a "Frequent ▾" menu
  at its right end.
- **Consequences.** The tree model (§5.2) only holds directories, so it is
  much smaller; the 100k-row budgets apply to the details list. Both panes
  use our own `ListModel`. P5/P6 numbers are unchanged; their wording now
  names which pane they apply to.

## ADR-011: M1 result — ListView for both panes; P1 not met (pending)

- **Context.** The M1 spike measured GTK4 with 100k rows (see
  `docs/benchmarks.md`). P6 is met with a wide margin by both variants. P5 is
  met by ListView if read as "dropped frames at p99" (0.05 % dropped, work
  time never above 16.7 ms); the raw p99 frame interval is 17.4 ms because of
  vsync jitter. P1 is not met: 370–450 ms by default, ~305 ms in the best
  GL configuration, and even an empty libadwaita window takes 237 ms (GL) or
  157 ms (cairo).
- **Decision.** Use `ListView` for the folder tree and the details list
  (better frame times than `ColumnView`; the details list gets its own header
  row with fixed-width, sortable columns). The P1 outcome and the reading of
  P5 are for the maintainer to decide, per the M1 gate; M1 stays open until
  then.
- **Consequences.** The maintainer's decision on P1 and P5 is ADR-012.

## ADR-012: P1 split into resident-instance open (P1a) and cold start (P1b); P5 measured as work time plus dropped frames

- **Context.** ADR-011: on the reference machine even an empty libadwaita
  window needs 157–237 ms to its first frame, so the original P1 (cold start
  < 150 ms) cannot be met by GTK4 regardless of tudi's own code. The raw p99
  frame interval in P5 is dominated by ±1 ms vsync jitter at 60 Hz, which is
  not a visible stutter. The maintainer decided both questions.
- **Decision.**
  - tudi keeps a resident instance: closing the last window does not exit the
    process; an optional XDG autostart entry starts it hidden at login.
  - P1 becomes two hard budgets: **P1a** opening a window from the resident
    instance < 150 ms, and **P1b** true cold start < 400 ms.
  - P5 is met when frame work time (before-paint → after-paint) is
    ≤ 16.7 ms at p99 and fewer than 1 % of frame intervals exceed 1.5
    refresh periods (25 ms at 60 Hz).
  - Startup tuning is part of M3: symbolic icons for file rows (themed
    colour icons go through glycin, which spawns a `bwrap` sandbox), and
    investigating integrated-GPU rendering on hybrid-GPU machines.
- **Consequences.** The resident process costs memory while no window is
  open; it must trim caches when the last window closes. P1a is measured by
  the spike's `--bench reopen` mode and later by tudi itself.
- **Follow-up (same day).** A desktop re-run was invalid (machine fully loaded
  by unrelated jobs, on battery). The maintainer chose to close M1 on the
  headless numbers and continue; P1a, P1b, P5 and P6 are confirmed on the
  desktop as part of M3 acceptance, which already measures them.

## ADR-013: Typo-tolerant fallback and full pinyin in memory search

- **Context.** The maintainer wants to find files despite typos and by full
  pinyin, not only by subsequence and pinyin initials. `nucleo-matcher` is a
  subsequence matcher, so transpositions and wrong letters find nothing.
- **Decision.** Full pinyin (already stored at index time) becomes an M6
  acceptance query. A typo fallback (Damerau-Levenshtein ≤ 1 or 2 by token
  length) runs only when a token has fewer than K strict matches, over a
  deduplicated name-word vocabulary with a deletion-neighbourhood index. Typo
  hits rank below strict hits. Content search stays out of v0.1.
- **Consequences.** The vocabulary index adds daemon memory that counts
  against P8, and the fallback counts against P7; both are measured in M6.
  The golden set gets typo and full-pinyin cases.

## ADR-014: Launchers — a special collection that runs scripts and apps

- **Context.** The maintainer wants a special favourites collection where a
  double-click starts any script or application.
- **Decision.** A "Launchers" root in the folder tree (plus a bookmarks-bar
  button) shows the collection in the details list; activating an item runs
  it. `.desktop` files launch via GIO; executables run in their own
  directory. Each item has a run mode: terminal (default for scripts, via a
  `commands.terminal_exec` template that keeps the window open) or
  background (log file plus an exit-code toast). Files dropped onto an item
  become its arguments; fixed arguments and working directory are optional
  per item. Stored in `launchers.toml`. Scheduled in M4 with bookmarks.
- **Consequences.** tudi starts arbitrary programs, but only on an explicit
  double-click, `Enter` or drop on an item the user added themselves; normal
  folders keep "open with default app" for executables. Spawning and
  `.desktop` parsing happen off the main thread.

## ADR-015: `tudi` is a GIO-only launcher; the GUI is `tudi-gui`

- **Context.** A second `tudi PATH` (P2, < 50 ms) took 45–53 ms in release
  builds although it only forwards its command line: linking GTK 4.22 pulls
  in `libgstgl` → `libGL` → the NVIDIA GLX driver, which initializes at load
  time (about 5,800 syscalls) even when nothing is drawn.
- **Decision.** Two binaries in `tudi-gui`: `tudi`, linking only GLib/GIO,
  registers the application id; if an instance is running it forwards the
  command line (GApplication remote protocol), otherwise it execs
  `tudi-gui` next to it with the same arguments. Both declare the same
  options. `cargo run -p tudi-gui` runs the GUI directly.
- **Consequences.** P2 drops to 10–11 ms. A cold start pays one extra
  `exec` of a small binary (~2 ms). The CLI (`tudi q`, M6) goes into the
  launcher, which keeps it free of GTK as §5.8 requires.

## ADR-016: Folder history counts deliberate opens and supports exclusions

- **Context.** The maintainer wants the number of times each folder was
  opened by hand recorded, the most used folders first in the history, and
  some folders never to appear there.
- **Decision.** Keep the zoxide-style frecency ranking (count weighted by
  recency, ADR-006) and also store and show the raw open count and last
  visit. Only deliberate entries count (list activation, tree choice,
  bookmark, path bar, search result, `tudi PATH`); restores, back/forward,
  going up, live reloads and keyboard pass-overs in the tree (< 1 s) do
  not. Exclusions come from the context menu (one folder, or with its
  subfolders) and from glob rules in `[history] exclude`; an excluded folder
  is not recorded, and its existing records are deleted.
- **Consequences.** The store has an exclusion table next to the visits.
  Database access runs on a dedicated thread in the GUI. A "Frequent" root
  in the tree shows the history.

## ADR-017: Publish 0.1.0 as an early preview; M9 becomes the next minor

- **Context.** The maintainer asked to publish the project on GitHub and
  release 0.1.0 now, while PLAN called the M9 release "v0.1".
- **Decision.** 0.1.0 is the first public preview (M0–M2 done, M3 features
  done, its acceptance open). The M9 release keeps its scope and takes the
  next minor version. Releases are built by `.github/workflows/release.yml`
  on Ubuntu 24.04 when a `v*` tag matching the workspace version is pushed:
  a `.deb` (cargo-deb, dependencies from `dpkg-shlibdeps`) and a `tar.gz`
  in prefix layout, with SHA-256 sums. The Debian Maintainer field is built
  from the repository owner's GitHub noreply address in CI, so it is not in
  the repository files.
- **Consequences.** Packages are only built in CI (local builds embed local
  paths). The English UI is the default; translations follow the system
  locale only with `[ui] language = "system"`.
