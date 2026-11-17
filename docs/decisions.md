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
- **Consequences.** Whatever is chosen for P1 (tuning, a resident instance,
  a different budget, or a Qt6 spike) is recorded as a follow-up ADR.
