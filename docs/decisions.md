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
- **Follow-up (0.1.1).** Releases before M9 are patch versions (0.1.x),
  even when they add features; the next minor stays reserved for the M9
  release.

### ADR-014 follow-up: launchers live on the bookmarks bar

After the "Frequent" tree root was replaced by the bookmarks bar (ADR-016),
the maintainer chose to show launchers on the bar too, right after the
folder bookmarks, instead of a tree root with a list. `Ctrl+D` on a selected
script, executable or `.desktop` file adds a launcher; on a folder (or with
nothing selected) it adds a bookmark. Non-executable `.sh` / `.py` files run
through `sh` / `python3`; `.desktop` files through `gio launch`.

## ADR-019: Query syntax: forgiving parser, jiff, scope over the protocol

- **Context.** M8 adds filters (extension, kind, dates, size), scoped
  search and shell integration. `chrono` is flagged unmaintained by
  RustSec (RUSTSEC-2026-0543), so `cargo deny` rejects it.
- **Decision.**
  - The parser (`tudi-index::query`) never fails: unreadable filters are
    dropped and reported as structured `Hint`s. The daemon returns them in
    English (`Results.hints`, for `tudi q`); the GUI parses the text itself
    to show translated hints immediately. Calendar math uses `jiff`
    (local time zone, DST-aware).
  - `.xxx` is both an extension filter and a dotfile-name prefix, so
    `.pdf` and `.bashrc` both do what one expects. `size…` is a filter only
    when followed by `:`, `<`, `>` or `=`.
  - Scoped search sends the folder path in `Request::Query.scope`
    (resolved by the daemon) instead of encoding it in the text, so paths
    need no quoting; a folder outside the index is an error.
  - `tudi init` prints shell code; the `cd` hook is opt-in (`--hook`) and
    does not count the folder a shell starts in, matching ADR-016's
    "deliberate opens only". `tudi visit` records with the same exclusions.
  - Pinyin collation is opt-in (`[browse] pinyin_sort`) and implemented as
    a transliteration before the natural sort key.
- **Consequences.** The GUI binary links `tudi-index` (parser only; the
  launcher does not). Adding `scope` changed the wire format: a running
  daemon from an older build must be restarted (the install script does).

## ADR-018: Matcher in `tudi-index`; daemon memory and priority

- **Context.** PLAN placed the matcher in `tudi-core::matcher`, but the
  token-mask propagation works directly on the index's node arrays.
  Measurements showed the daemon at 200–370 MB RSS although the live data
  is about 130 MB: temporary memory of builds on rayon threads stays in
  glibc's per-thread arenas.
- **Decision.** The matcher lives in `tudi-index` (`tudi-core` stays free of
  index types). Pinyin alternates and the typo vocabulary use arenas
  instead of per-entry `String`s. The daemon calls `malloc_trim(0)` (a thin
  `unsafe` wrapper) after builds and reconciles, streams snapshots instead
  of encoding them in memory, and lowers its own CPU and I/O priority.
  `tudi q` / `tudi index` live in the GIO-only launcher (ADR-015) and
  start the daemon on demand, like the GUI's search; the systemd user unit
  is optional.
- **Consequences.** P8 is met (≈ 140 MB with 1.83M entries) with a
  transient peak during the vocabulary build. The daemon depends on glibc
  for `malloc_trim` (Linux-only project).

## ADR-020: `Ctrl+F` finds in the shown folder; `Enter` filters

- **Context.** Type-ahead only matches name prefixes and resets after a
  second; the memory search (`Ctrl+P`) leaves the folder. Users wanted to
  find a row in the current list by any part of its name.
- **Decision.** `Ctrl+F` shows a find field above the list. Typing jumps to
  the first row containing the text (case-insensitive substring); `Enter`
  filters the list to the matches rather than going to the next match
  (`Down`/`F3` do that). The jump scans on the main loop in chunks, sharing
  type-ahead's scan; the filter is applied on the listing workers as part
  of the order (`Job::Sort`), never by iterating the `GListModel`.
- **Consequences.** A filtered listing is an ordinary order, so live
  reloads, sorting and selection keep working; one extra lowercase pass per
  sort while a filter is set. The filter is per folder and is dropped on
  navigation.

## ADR-021: Forward the activation token through the environment

- **Context.** Launching from the dock while the resident instance runs
  (ADR-012) went through the GIO-only launcher, whose forwarded command
  line carried only `cwd` and options. The primary never got the shell's
  activation token, so the shell kept its "starting" spinner until a
  timeout. gio-rs cannot override `add_platform_data`.
- **Decision.** The launcher sets `SEND_ENVIRONMENT`; the primary takes
  `XDG_ACTIVATION_TOKEN` / `DESKTOP_STARTUP_ID` from a remote command
  line's environment and passes it to `GtkWindow::set_startup_id` before
  `present()`. No `unsafe` class-struct patching.
- **Consequences.** Each forwarded launch sends the launcher's environment
  (a few KB over D-Bus, local only).

## ADR-022: Drop in before M9: move on the same filesystem, else copy

- **Context.** PLAN put drop-in (copy/move) in M9; the maintainer asked for
  it now, with "copy across disks, move on the same disk".
- **Decision.** Implement drop-in now on list rows, list empty space and
  tree rows. The default per item is move within the destination's
  filesystem and copy across filesystems (like Nautilus and Finder);
  `Ctrl` forces copy, `Shift` forces move. The filesystem test (`st_dev`)
  runs on the transfer thread with the copy itself. Paste and drop share
  one transfer path. No progress dialog yet (the status bar names the
  task); conflicts still get `name (2)` names.
- **Consequences.** Dragging within tudi moves files by default, which is
  what most users expect on one disk. Large cross-disk copies show only the
  status-bar spinner until the M9 progress UI.

## ADR-023: The bookmarks bar is ordered by open count

- **Context.** The maintainer wants the bar's buttons in the order of how
  often each folder was opened, not a hand-arranged order.
- **Decision.** Bookmarks are shown by their open count in the folder
  history (most first; ties, including never-opened bookmarks, keep the
  saved order), and the frequent folders after the separator are the same
  frecency top list as before, shown by open count. `Alt+1..9` follow the
  shown order. Moving bookmarks left/right and drag-to-reorder are removed.
  The history thread sends every folder's count with each snapshot.
- **Consequences.** The order changes as folders are used, so `Alt+N` is
  not a fixed binding. `bookmarks.toml` keeps its order, used only for
  ties.
- **Follow-up.** The maintainer also wants to drag bookmarks into an
  order. Dragging one fixes the shown order as the saved order and sets
  `manual_order = true` in `bookmarks.toml`: from then on bookmarks are
  shown in that order (new ones at the end) and open counts no longer move
  them; "Sort by Open Count" in a bookmark's menu goes back. The bar's drag
  source runs in the capture phase, before the button's click gesture.

## ADR-024: UI language: English, the system's, or a chosen translation

- **Context.** Besides Simplified Chinese, the maintainer wants common
  languages selectable in the settings: Traditional Chinese, Japanese,
  Korean, German, French, Spanish, Portuguese, Italian, Russian.
- **Decision.** `[ui] language` takes `"en"`, `"system"` or a catalog
  name (`config::LANGUAGES`, stored as a plain string, so existing configs
  stay valid). For a chosen language, `i18n::init` sets `LANGUAGE` at the
  start of `main` (before any thread; the one `set_var`) and binds the
  catalogs as for `"system"`; gettext consults `LANGUAGE` before the
  locale, so GTK's own strings follow too. The settings list shows each
  language by its own name, untranslated. The new catalogs were
  machine-translated from `tudi.pot` with `zh_CN.po` as context and are
  checked by `msgfmt -c` and a placeholder-count check.
- **Consequences.** A chosen language needs a non-`C` locale (gettext
  ignores `LANGUAGE` under `LANG=C`), which every desktop session has. The
  translations need review by native speakers; new strings must be added
  to every catalog (`msgmerge`), untranslated ones show in English.

## ADR-025: Tabs in the file list, two can be shown side by side

- **Context.** The maintainer wants browser-like tabs in the list (named
  after the folder, a "+" after the last tab) and tabs that can be tiled.
- **Decision.** A `Tab` struct owns everything that belonged to the one
  list (model, view, history, find bar, folder size, watch key); the
  window keeps the tree, path bar, status bar, worker and watcher and
  follows the active tab. Existing list code reaches its tab through
  `Browser::tab()`, which is the active tab unless `with_tab` set the tab
  of the reply or event being handled; generations are unique per window
  so replies are routed by them. The strip is our own widgets (not
  `AdwTabBar`, whose "+" sits at the far end). Side by side is two sides
  in a `GtkPaned`, each with its own strip and tabs (like an editor's
  split groups), not a pair of tabs under one strip: the maintainer
  wanted the tabs to follow the split, and a pair under one strip widened
  the window with wide columns. Tabs
  are restored in the first window; inactive ones are listed when first
  shown, so startup lists one folder as before.
- **Consequences.** Every tab keeps its listing in memory and a live watch
  on its folder (within the window's 512 watches). Window-level settings
  (hidden files, column widths) apply to all tabs. Status-bar tasks of a
  background tab are not shown.

## ADR-026: Thumbnail view, thumbnails decoded off the main thread

- **Context.** The maintainer wants to browse folders with images as
  thumbnails.
- **Decision.** A per-tab `GtkGridView` over the same `PaneModel` and
  selection as the list (GridView is a recycling view like ListView; the
  model stays ours, AGENTS.md rule 3). It is offered when the folder has
  image files by extension (`icons::category`, counted on the listing
  worker). Thumbnails are loaded only for bound cells, on two loader
  threads (freedesktop cache first, else gdk-pixbuf at 256 px); the main
  thread only builds textures. Reading file contents here is the purpose
  of the view, not part of listing (rule 5 is about listing).
- **Consequences.** tudi does not write the freedesktop cache, so folders
  never opened by another file manager are decoded each session (once per
  window, then cached in memory). Formats gdk-pixbuf cannot load (e.g.
  HEIC without its loader, RAW) show their icon.

## ADR-027: An optional preview pane

- **Context.** The maintainer wants an optional pane that previews files
  and images.
- **Decision.** One pane per window, right of the list area (both sides
  of a split), following the active tab's selection. Images go through
  gdk-pixbuf at up to 1600 px, text through a 64 KB read, both on a
  short-lived thread per selection; other types show details only (no
  external thumbnailers or document renderers). Hidden by default.
- **Consequences.** Moving through a list with the pane open starts a read
  per selected file; only the newest result is shown. PDF, video and
  office documents show no content preview.

## ADR-028: fanotify helper reports changed folders, checks access itself

- **Context.** M7 needs a root helper that forwards fanotify events to
  user daemons without leaking names (PLAN §5.6). The plan suggested
  checking access with the client's credentials and marking the
  filesystems of the configured roots.
- **Decision.**
  - Messages name changed *folders*, not entries; the client re-lists them.
    Events are deduplicated per folder over a 200 ms window.
  - A folder is sent only if the client can list every folder from one of
    its roots down to it (and search above the root). Permissions are
    evaluated in the helper from `lstat` and POSIX ACLs, not by switching
    to the client's credentials, so the helper keeps only
    `CAP_DAC_READ_SEARCH` (no `CAP_SETUID` / `CAP_SETGID`) and runs as
    `nobody` after setup.
  - Every mounted filesystem of a supported type is marked once at
    startup; `CAP_SYS_ADMIN` is dropped right after. Clients never cause
    marks; they learn which filesystems are covered from the fsids in
    `Covered`.
  - Hand-written frame protocol, `libc` only; logs go to stderr (journald)
    instead of `tracing`, to keep the root binary's dependencies minimal.
- **Consequences.** LSM policies (SELinux, AppArmor) are not evaluated: a
  client may hear about a folder an LSM hides, never one that file
  permissions hide. Filesystems mounted after the helper started are not
  covered until it restarts (their roots keep the fallback). Marking all
  filesystems means the helper also wakes for changes outside any user's
  roots (e.g. `/var`); those are resolved and dropped.
