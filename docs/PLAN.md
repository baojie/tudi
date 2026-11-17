# tudi — Implementation Plan

> **Tudi (土地)** is the Chinese earth god who knows every inch of his local land.
> **tudi** knows every file on yours.

tudi is a fast, tree-first local file browser for Linux, with a memory-driven
search ("I remember it had *contract* in the name, was a PDF, and lived somewhere
under *acme*") and keyboard-reachable bookmarks.

This document is the master plan. It is written to be executed milestone by
milestone by a coding agent (Claude Code) with a human reviewing at each
milestone gate. Each milestone has **scope**, **tasks**, and **acceptance
criteria**. Do not start a milestone before the previous one's acceptance
criteria pass, except where noted.

---

## 0. How to use this plan

1. Place this file at `docs/PLAN.md` and `AGENTS.md` at the repo root
   (symlink `CLAUDE.md -> AGENTS.md`).
2. Work one milestone per session: *"Implement milestone M3 per docs/PLAN.md.
   Stop when its acceptance criteria pass and report the results."*
3. When a milestone finishes, tick it in §12 (Progress) and record any decision
   in `docs/decisions.md` (short ADR-style entries: context / decision /
   consequences).
4. If an acceptance criterion cannot be met, stop and report rather than
   weakening the criterion.

---

## 1. Goals and non-goals

### Goals

1. **Speed above everything.** A directory with 10,000+ entries opens and scrolls
   without perceptible lag. The UI thread never blocks on I/O.
2. **Tree-first browsing.** The main view is an expandable tree (not a
   column/icon view). Expanding a large directory is instant.
3. **Bookmarks with instant jumps.** `Alt+1..9`, a sidebar, and an auto-generated
   "frequent places" section ranked by frecency.
4. **Memory-stub search.** Type a few fragments in any order — name pieces, path
   pieces, pinyin initials, file type, rough time — and get the file in < 30 ms
   per keystroke over millions of paths. Enter reveals it in the tree.
5. **Native Linux citizen.** GTK4/libadwaita on GNOME/Wayland, works with
   fcitx5/IBus input methods, uses GIO for trash/mounts/open-with.
6. **Free copy & paste.** Paths are always one keystroke away as plain text.

### Non-goals (for v0.x)

- Cross-platform support (macOS/Windows).
- Icon/grid views, thumbnails-first browsing.
- Being a full Nautilus replacement for every file operation (bulk copy/move
  progress UIs come late; see M9).
- Network filesystem browsing (SMB/SFTP) beyond what GIO gives for free.
- Full-text content search (post-v0.1; see §11).

---

## 2. Performance budgets (hard requirements)

Measured on the reference machine (Ubuntu 26.04, Wayland, NVMe SSD, warm page
cache unless stated). Every budget has an automated benchmark (see §9).

| ID | Metric | Budget |
|----|--------|--------|
| P1 | Cold start → first window frame | < 150 ms |
| P2 | Second launch (single instance, window raise) | < 50 ms |
| P3 | Open a 10k-entry directory → first painted rows | < 50 ms |
| P4 | Open a 100k-entry directory → first painted rows | < 250 ms |
| P5 | Scroll through a 100k-row tree | no frame > 16.7 ms (60 fps) at p99 |
| P6 | Expand node inserting 10k rows → next frame painted | < 50 ms |
| P7 | Memory search keystroke over 2M indexed paths → results | < 30 ms p95 |
| P8 | Index daemon RSS with 2M entries | < 150 MB |
| P9 | Initial full scan of 2M entries (warm cache) | < 20 s, nice'd, never stalls UI |
| P10 | fanotify event → visible in search results | < 1 s |

Rules that make these achievable (also in AGENTS.md):

- No `stat`/`statx` on the GTK main thread. Ever.
- No O(n) work on the main thread for n > ~5,000 per frame.
- No `GtkTreeListModel`, `GtkSortListModel`, `GtkFilterListModel`,
  `GtkDirectoryList`, or `GtkStringList` for file rows: all are O(n) GObject
  heavy. The model is ours (§5.3).

---

## 3. Architecture overview

```
                    ┌──────────────────────────────┐
                    │ tudi-fanotifyd (root, opt.)  │  system service
                    │ FAN_MARK_FILESYSTEM on /home │
                    └──────────────┬───────────────┘
                                   │ change events (dir + name), Unix socket,
                                   │ filtered by peer uid
┌──────────────┐   query/results   ▼
│  tudi (GUI)  │◄──────────►┌──────────────────────┐
│  GTK4 + adw  │  Unix sock │  tudi-indexd (user)  │  systemd --user service
└──────┬───────┘            │  in-memory index,    │
       │                    │  mmap snapshot,      │
       │ links              │  matcher, frecency   │
       ▼                    └──────────────────────┘
┌──────────────┐                     ▲
│  tudi-core   │◄────────────────────┘ links
│  (library)   │
└──────────────┘
       ▲
       │ `tudi q <query>` (CLI path in the same binary, no GTK init)
```

- **tudi-core**: pure Rust, no GTK. Directory listing, row/tree data
  structure, query parser, matcher, pinyin, frecency, config.
- **tudi-index**: the index data structure, persistence (snapshot), and
  applying change events. Used by the daemon; testable without it.
- **tudi-proto**: IPC message types + framing (shared by GUI, CLI, daemon,
  fanotify helper).
- **tudi-indexd**: user daemon owning the index; serves queries.
- **tudi-fanotifyd**: tiny privileged helper; only forwards change events.
- **tudi** (crate `tudi-gui`, binary `tudi`): GTK4 app + `tudi q` CLI
  subcommand.

The GUI works **without** the daemon (browsing, bookmarks); search shows
"index not running" and offers to start it.

---

## 4. Repository layout

```
tudi/
├── AGENTS.md                 # agent/dev rules (CLAUDE.md symlinks here)
├── CLAUDE.md -> AGENTS.md
├── README.md
├── LICENSE-MIT
├── LICENSE-APACHE
├── Cargo.toml                # workspace
├── rust-toolchain.toml       # pin stable
├── deny.toml                 # cargo-deny: licenses, advisories
├── .github/workflows/ci.yml
├── crates/
│   ├── tudi-core/
│   ├── tudi-index/
│   ├── tudi-proto/
│   ├── tudi-indexd/
│   ├── tudi-fanotifyd/
│   └── tudi-gui/             # binary name: tudi
├── spikes/
│   └── gtk4-list/            # M1 throwaway benchmark app
├── benches/fixtures/         # scripts that generate synthetic trees
├── data/
│   ├── io.github.baojie.tudi.desktop
│   ├── io.github.baojie.tudi.metainfo.xml
│   ├── icons/
│   └── systemd/
│       ├── tudi-indexd.service          # user unit
│       ├── tudi-fanotifyd.service       # system unit
│       └── tudi-fanotifyd.socket
├── po/                       # gettext translations (zh_CN first)
├── docs/
│   ├── PLAN.md               # this file
│   ├── architecture.md
│   ├── decisions.md
│   ├── benchmarks.md         # budgets, how to run, latest numbers
│   ├── query-syntax.md
│   └── design/
│       ├── listing.md
│       ├── tree-model.md
│       ├── index.md
│       ├── matcher-and-ranking.md
│       ├── fanotify.md
│       └── ui.md
└── private/                  # local only, git-ignored (Chinese notes OK)
```

**Language rule:** everything outside `private/` is English: code, comments,
identifiers, docs, commit messages, issue templates. `private/` is a local,
git-ignored directory for the maintainer's notes; it is never committed.

**License:** dual MIT / Apache-2.0. GTK4 and libadwaita are LGPL, dynamically
linked — compatible.

**App ID:** `io.github.baojie.tudi`.

---

## 5. Component design

### 5.1 Directory listing (`tudi-core::listing`)

- Read directories with `getdents64` via `rustix::fs::RawDir` using a large
  reusable buffer (e.g. 256 KiB). This yields name + `d_type` with no `stat`.
- If `d_type == DT_UNKNOWN` (some filesystems), mark the entry "type pending"
  and resolve it in the metadata phase.
- Store names in an **arena**: one `Vec<u8>` per listing plus `(offset, len)`
  per entry. Names are `OsStr` bytes, never forced to UTF-8; display uses
  lossy conversion only at render time.
- **Sorting** happens in the listing worker, never in GTK:
  - Directories first (configurable), then natural sort
    (`file2` < `file10`), case-insensitive.
  - Precompute a compact sort key per entry so re-sorting (by name / mtime /
    size) is a single `sort_unstable_by_key` on indices.
  - CJK collation: v0 uses code-point order inside the natural sort; pinyin
    collation is an option behind a setting (M8), reusing the pinyin table.
- **Metadata phase** (size, mtime, mode, symlink target): `statx` with
  `AT_SYMLINK_NOFOLLOW | AT_STATX_DONT_SYNC` and a minimal mask, run on a
  worker pool (rayon). Priority order: the rows currently visible first, then
  the rest. Results are sent to the UI in batches (≤ 1 per frame).
  - Later optimization (only if P3/P4 miss): batch `statx` via io_uring
    (`io-uring` crate, `IORING_OP_STATX`).
- **Content type/icon:** guessed from the file name only
  (`gio::content_type_guess(name, None)`), cached per extension. Never read
  file contents for icons.
- **Generations:** every listing request carries a generation number; stale
  results (user navigated away) are dropped.
- **Live updates for open directories:** inotify watches only on directories
  that are currently expanded/visible in the GUI (bounded count; LRU-evict
  watches beyond a limit, e.g. 512). Coalesce events per directory over
  ~50 ms, then apply a diff (re-list + merge, keep selection/scroll stable).

### 5.2 Tree data structure (`tudi-core::tree`)

- The visible tree is a **flattened array of rows** (`Vec<Row>`), in display
  order. `Row { node_id, depth, flags (expanded, has_children, loading) }`.
- Node data (name, metadata) lives in a separate slab keyed by `node_id`.
- **Expand** = list the directory (async), then splice its sorted children into
  the row array after the parent. **Collapse** = remove the contiguous range
  of descendants (find end by scanning for the next row with
  `depth <= parent.depth`).
- Each splice produces a `(position, removed, added)` triple that maps 1:1 onto
  `GListModel::items-changed`.
- Expanding a directory with 100k children = one `Vec::splice` (memmove of the
  tail) — fine for P6. If profiling shows otherwise, switch to a chunked rope
  (e.g. B-tree of 4k-row chunks); keep the API identical.
- Expanded state is remembered per path (restored when re-expanding a parent
  and across sessions, capped).

### 5.3 GTK model and view (`tudi-gui`)

- A custom `gio::ListModel` implemented in Rust (`glib::subclass`):
  - `n_items()` = `rows.len()`.
  - `item(i)` returns a lightweight `RowObject` GObject holding only the row
    index/node id; create on demand and keep a small cache keyed by node id
    (GTK requests only visible items plus a margin).
  - Emit `items_changed` exactly from the tree splice triples.
- View: `gtk::ListView` (or `gtk::ColumnView` — decided by the M1 spike) with
  `SignalListItemFactory`. Row widget: indentation (margin = depth × 16 px),
  expander, icon, name label (ellipsized), right-aligned size and date labels
  in fixed-width columns.
- Selection: `gtk::MultiSelection` over the custom model. Avoid any API that
  iterates all items on the main thread (e.g. select-all must be implemented
  against our model, not by iterating `GListModel`).
- Threading: workers → `async_channel` → `glib::MainContext::spawn_local`
  consumer on the main thread. The main-thread consumer applies at most one
  batch per frame (use the frame clock or an idle source with a time budget of
  ~4 ms).

### 5.4 Index (`tudi-index`, `tudi-indexd`)

**Scope:** `$HOME` plus extra roots from config. Default excludes (configurable,
gitignore-like globs): `.cache`, `.local/share/Trash`, `node_modules`,
`target`, `__pycache__`, `.venv`, `.git/objects`, `snap`, `.npm`, `.cargo/registry`.
Excluded directories themselves are indexed (so you can find `node_modules`),
their contents are not.

**Extra roots on slow or limited filesystems** are in scope for v0.1 (e.g. a
large data disk that is a rotational HDD formatted as exFAT). Each root carries
its own settings (§8): reconcile interval (default for a rotational device:
daily instead of every 10 min, so the disk is not kept spinning) and excludes.
The daemon detects the filesystem type and device rotation per root and:

- skips fanotify for filesystems without file-handle support (exFAT, vfat,
  ntfs3, most FUSE) and uses the fallback sources below for that root;
- does not rely on inode numbers or symlinks there, and handles
  case-insensitive names;
- relies on directory mtime for reconcile only after M5 verifies that the
  filesystem updates it on entry create/delete/rename; otherwise the
  reconcile pass re-lists every directory of that root;
- marks a root "offline" (keeps its entries, flagged, excluded from results
  by default) when it is unmounted, and reconciles when it comes back.

**Data layout (struct-of-arrays, cache friendly):**

```
nodes: Vec<Node>        // stored in parent-before-child order
Node {
  parent: u32,          // index into nodes; u32::MAX for roots
  name_off: u32,        // into names arena
  name_len: u16,
  kind: u8,             // file / dir / symlink / other
  flags: u8,            // excluded, hidden, has_cjk, ...
  mtime: u32,           // seconds since 2000-01-01 UTC
  size: u64,            // files only; 0 for dirs
}                        // 24 bytes
names: Vec<u8>          // arena
cjk_alt: HashMap<u32, AltNames>  // pinyin full + initials, only for CJK names
```

- Full paths are reconstructed by walking parents (only for displayed results).
- Deletions leave tombstones; compact when tombstones > 20% (rebuild in a
  background thread, swap atomically).
- Inserts append; parent-before-child order is preserved because a child is
  always inserted after its parent exists.

**Initial scan:** parallel walk (`jwalk` or a custom rayon walker over
`RawDir`), `statx` only for size/mtime, `ionice -c3`/`nice` equivalent
(`ioprio_set` + `setpriority`), throttled if the system is busy.

**Persistence:** snapshot to `$XDG_CACHE_HOME/tudi/index-v{N}.bin`
(versioned header + magic + checksum), written atomically (temp + rename)
every N minutes if dirty and on shutdown. On start: mmap/load the snapshot,
serve queries immediately, then run a **reconcile pass** in the background:
`statx` every indexed directory and re-list those whose mtime changed
(directory mtime changes on create/delete/rename of entries).

**Change sources (in priority order):**

1. `tudi-fanotifyd` events, if the helper is installed (M7).
2. Fallback without the helper (or for roots it cannot watch): inotify on the
   top-K frecent directories + periodic reconcile pass (per-root interval;
   default every 10 min, daily on rotational devices).
3. GUI hints: when the GUI lists a directory, it sends the fresh listing to the
   daemon (cheap, free accuracy for places the user actually visits).

**IPC (`tudi-proto`):** Unix socket at `$XDG_RUNTIME_DIR/tudi/indexd.sock`,
length-prefixed frames, `serde` + `postcard`. Messages:
`Query { id, text, limit, scope? }`, `Results { id, items, total, elapsed }`,
`Cancel { id }`, `FrecencyChanged` (hint to reload, see §5.5), `Status`,
`ListingHint`.
A new `Query` supersedes the previous in-flight one from the same client.
`tudi q --json` prints JSON lines for scripting.

### 5.5 Matcher and ranking (`tudi-core::matcher`)

**Query model:** whitespace-separated tokens, order-independent, all must
match (AND). Token kinds (full grammar in `docs/query-syntax.md`, M8):

- plain fragment: fuzzy-matched against path components
- `.pdf` / `ext:pdf`: extension filter
- `/acme/` or `in:acme`: must match a directory component
- time/size filters (M8): `<7d`, `>2025-06`, `today`, `week`, `size>10M`,
  plus Chinese aliases `今天 昨天 本周 上周 本月 上月`

**Algorithm (token-mask propagation) — avoids building 2M path strings:**

1. For each plain token *t_i*, score every node's *own name* (and its pinyin
   alternates, if any) with `nucleo-matcher` (the low-level crate from the
   Helix editor). Directories and files are scored in parallel chunks (rayon).
2. Each node gets `own_mask` (bit *i* set if token *i* matches its name) and
   per-token best scores.
3. Propagate down the tree in parent-before-child order:
   `cum_mask(n) = cum_mask(parent) | own_mask(n)` (directories only need a
   pass; files read their parent's `cum_mask`).
4. A node is a hit if `cum_mask == all_tokens` and it passes filters.
5. Score = Σ token scores, with a bonus when a token matched the file's **own
   name** vs an ancestor, a bonus for prefix/word-boundary matches, and a
   penalty for depth.
6. Final rank = `match_score × frecency_boost × recency_boost`, where
   `frecency_boost` comes from the node or its nearest frecent ancestor and
   `recency_boost` from mtime. Weights live in one config struct and are tuned
   against the golden set (§9.3).
7. Keep top-K with a bounded heap per worker; merge.

**Incremental:** if the new query extends the last token of the previous query
(user typing), restrict to the previous hit set (+ ancestors) instead of all
nodes.

**Pinyin (`tudi-core::translit`):** a pluggable transliteration trait;
the first implementation uses the `pinyin` crate to produce full pinyin
(no tones, e.g. `hetonghedui`) and initials (`hthd`) for names containing CJK
characters. Computed at index time, stored only for CJK names. Polyphonic
characters: include all common readings for initials (small cartesian cap,
e.g. max 8 variants).

**Frecency (`tudi-core::frecency`):** zoxide-style. Store in
`$XDG_DATA_HOME/tudi/frecency.sqlite` (rusqlite, bundled). Each visit adds
weight; score multiplied by 4 (last hour), 2 (last day), 0.5 (last week),
0.25 (older); aging when total exceeds a cap. Visits come from: opening a
directory or file in the GUI, revealing a search result, and optionally a
shell hook (M8).

Ownership: the store is a library in `tudi-core`, opened in SQLite WAL mode.
Writers (the GUI, `tudi` shell hook) write visits directly, so frecency works
without the daemon (M4 precedes M5). The daemon only reads: it reloads the
weights it needs for ranking periodically and when a client sends
`FrecencyChanged`.

**Imports (first run, M4):** `~/.local/share/autojump/autojump.txt`
(`weight\tpath`), `zoxide query -ls` output if `zoxide` exists, GTK bookmarks
(`~/.config/gtk-3.0/bookmarks`).

### 5.6 fanotify helper (`tudi-fanotifyd`, M7)

- Runs as root via a system unit; socket-activated by
  `tudi-fanotifyd.socket` (`/run/tudi/fanotify.sock`).
- `fanotify_init(FAN_CLASS_NOTIF | FAN_REPORT_DFID_NAME | FAN_CLOEXEC |
  FAN_NONBLOCK, O_RDONLY)` and `fanotify_mark(FAN_MARK_ADD |
  FAN_MARK_FILESYSTEM, FAN_CREATE | FAN_DELETE | FAN_MOVED_FROM |
  FAN_MOVED_TO | FAN_ONDIR | FAN_DELETE_SELF | FAN_MOVE_SELF, …)` on each
  filesystem containing a configured root (kernel ≥ 5.9 for DFID_NAME).
  Optional `FAN_MODIFY | FAN_ATTRIB` (coalesced) for mtime/size freshness,
  off by default.
- Resolve the directory file handle with `open_by_handle_at` (needs
  `CAP_DAC_READ_SEARCH`) against a mount fd, then `readlink
  /proc/self/fd/N` → path; append the name.
- **Security model (document in `docs/design/fanotify.md`):**
  - Accept only clients whose `SO_PEERCRED` uid is a regular user.
  - Forward an event to a client only if the path is inside that uid's
    configured roots and the uid can access it (path under the user's home,
    or `faccessat` check with the client's credentials for extra roots).
  - The protocol is subscribe-only: no client can make the helper open,
    read, or mark arbitrary paths.
  - Drop privileges after setup to the minimum capabilities
    (`CAP_SYS_ADMIN` for marks is only needed at mark time; keep
    `CAP_DAC_READ_SEARCH` for handle resolution).
  - Small, auditable crate: no async runtime, minimal dependencies.
- On `FAN_Q_OVERFLOW`, tell clients to run a reconcile pass.
- Coalesce bursts (e.g. `git checkout`, `tar x`) per directory over ~200 ms.
- Packaging installs it **optionally**; the daemon detects its absence and
  uses the fallback (§5.4).

### 5.7 UI design (`tudi-gui`, details in `docs/design/ui.md`)

Layout (libadwaita `AdwApplicationWindow`):

```
┌───────────────────────────────────────────────────────────────┐
│ [◀][▶][▲]  ~/projects/acme                  [ search… Ctrl+P ]│  header bar
├──────────────┬────────────────────────────────────────────────┤
│ BOOKMARKS    │ ▾ projects                       —     2 days  │
│ 1 Home       │   ▾ acme                         —     1 hour  │
│ 2 Projects   │     ▸ docs                       —     3 days  │
│ 3 Downloads  │       report-2026Q3.pdf      2.1 MB    5 days  │
│ …            │       合同核对.xlsx            88 KB    3 weeks │
│ FREQUENT     │   ▸ tudi                         —     now     │
│ acme         │                                                │
│ notes        │                                                │
│ DEVICES      │                                                │
├──────────────┴────────────────────────────────────────────────┤
│ 12,034 items · 3 selected (4.2 MB) · index: 1.9M entries ✓     │  status bar
└───────────────────────────────────────────────────────────────┘
```

- **Path bar** is a normal `GtkEntry` (selectable, copyable, editable;
  Tab-completion; `Ctrl+L` focuses it).
- **Memory search**: `Ctrl+P` or `/` opens a search overlay above the tree
  with live results (name with matched characters highlighted, parent path
  dimmed, size/date). `Enter` reveals the result in the tree (expand
  ancestors, scroll, select). `Ctrl+Enter` opens it. `Alt+Enter` opens its
  containing folder in a terminal.
- **Plain typing in the tree** = type-ahead jump among siblings of the
  focused row (prefix match), not global search.
- **Bookmarks**: `Alt+1..9` jump, `Ctrl+D` bookmark current dir,
  drag to reorder, stored in `$XDG_CONFIG_HOME/tudi/bookmarks.toml`.
  "Frequent" section is the top frecency directories not already
  bookmarked.

Default keybindings (all remappable in `config.toml`):

| Key | Action |
|-----|--------|
| `↑ ↓` / `PgUp PgDn` / `Home End` | move |
| `→` / `←` | expand / collapse (or go to parent) |
| `Shift+→` | expand recursively (async, capped at 50k rows) |
| `Enter` | open file / toggle directory |
| `Backspace` / `Alt+↑` | go up (re-root at parent) |
| `Ctrl+P`, `/` | memory search |
| `Ctrl+L` | path bar |
| `Ctrl+H` | toggle hidden files |
| `Ctrl+C` | copy selected files (uri-list + text paths) |
| `Ctrl+Shift+C` | copy absolute path(s) as text |
| `Ctrl+Alt+T` | open terminal here (configurable command) |
| `Ctrl+E` | open in editor (configurable, default `zed`) |
| `F2` | rename |
| `Delete` | move to trash (GIO) |
| `Ctrl+Shift+N` | new folder |
| `Alt+1..9`, `Ctrl+D` | bookmarks |

External commands (config):

```toml
[commands]
terminal = ["kitty", "--directory", "{dir}"]
editor   = ["zed", "{path}"]
```

- Single instance via `GApplication` (`HANDLES_COMMAND_LINE`): `tudi PATH`
  opens/raises a window at PATH in the running instance.
- Drag and drop **out** of tudi (file list as `text/uri-list`) from M3;
  drop **in** (copy/move) in M9.
- i18n: gettext from M3; ship `zh_CN`.

### 5.8 CLI (`tudi q`)

```
tudi q contract .pdf            # top results, one path per line
tudi q --json -n 50 acme 季报    # JSON lines
tudi q --dir acme               # directories only (for cd)
tudi index status|rescan|stop
```

The `q` path must not initialize GTK. If dynamic linking of GTK makes
`tudi q` slower than 30 ms total, split into a separate `tudi-q` binary
(record in decisions.md).

Shell integration (M8): `tudi init zsh|bash|fish` prints a function `tj`
(tudi jump) = `cd "$(tudi q --dir "$@" | head -1)"` and an optional hook
that reports `cd` visits to frecency.

---

## 6. Technology choices

| Concern | Choice | Notes |
|---------|--------|-------|
| Language | Rust (stable, pinned) | single language across all crates |
| GUI | GTK4 ≥ 4.14 + libadwaita ≥ 1.5 via `gtk4`, `libadwaita` crates | 4.14 so CI on Ubuntu 24.04 builds; reference machine has newer |
| Syscalls | `rustix` (getdents, statx, inotify); `nix` or `libc` for fanotify | verify current fanotify support in `nix`; else thin `libc` wrapper |
| Parallelism | `rayon` | listing metadata, scan, matcher |
| Channels | `async-channel` | worker → GLib main loop |
| Walk | `jwalk` or custom rayon walker | decide in M5 by benchmark |
| Fuzzy | `nucleo-matcher` | per-token scoring; MPL-2.0 (allowed in `deny.toml`, used unmodified; ADR) |
| Pinyin | `pinyin` crate | behind `Transliterator` trait |
| Storage | snapshot: custom binary; frecency: `rusqlite` (bundled) | |
| IPC | `serde` + `postcard`, Unix sockets | `serde_json` for `--json` |
| Config | `toml` + `serde`, `directories` crate for XDG | |
| Logging | `tracing` + `tracing-subscriber` (journald in daemons) | |
| Benchmarks | `criterion` (core), custom harness (GUI) | |
| Lint | `rustfmt`, `clippy -D warnings`, `cargo-deny` | |

Verify the latest versions of each crate when adding them; do not copy version
numbers from this document.

---

## 7. Milestones

### M0 — Repository bootstrap

**Tasks**
- Cargo workspace with empty crates listed in §4; `tudi-gui` builds a binary
  named `tudi`.
- `README.md` (pitch, status: pre-alpha, build instructions), licenses,
  `AGENTS.md` + `CLAUDE.md` symlink, `docs/` skeleton with stub design docs,
  `docs/decisions.md` with ADR-001 (GTK4), ADR-002 (fanotify helper,
  optional), ADR-003 (name "tudi").
- `.gitignore` (target/, *.bin snapshots, `private/`, etc.).
- CI (`ubuntu-24.04`): install `libgtk-4-dev libadwaita-1-dev`, run fmt,
  clippy `-D warnings`, tests, `cargo deny check`.
- `benches/fixtures/make_tree.py` (or a Rust bin): generate synthetic trees
  — a flat dir with N files (10k, 100k), a deep realistic tree with ~2M
  entries (mixed English/CJK names, extensions, depths), deterministic seed.
  Default output directory `~/.cache/tudi-fixtures` (a real disk; `/tmp` is
  often tmpfs, which would cost GBs of RAM and give unrepresentative numbers).

**Acceptance**
- `cargo build --workspace` and CI green.
- `cargo run -p tudi-gui` opens an empty libadwaita window titled "tudi".
- Fixture generator produces all three fixture sets reproducibly.

### M1 — GTK4 list spike (decision gate)

Throwaway app in `spikes/gtk4-list/`. Purpose: prove GTK4 meets P1, P5, P6
before investing.

**Tasks**
- Custom `gio::ListModel` over a `Vec<Row>` of 100k synthetic rows (§5.3).
- Two variants behind a CLI flag: `ListView` with fixed-width labels vs
  `ColumnView` with 3 columns.
- Automated measurement mode (`--bench`):
  - Programmatic scroll through the whole list via the vertical
    `Adjustment` at a constant speed; record frame intervals from
    `gdk::FrameClock` (`after-paint`) → p50/p95/p99/max.
  - Insert 10k rows in the middle (simulated expand) → time to next
    `after-paint`.
  - Startup: time from `main()` to first `after-paint` (and external
    measurement via a wrapper script with `date +%s%N`).
- Write results to stdout as JSON and to `docs/benchmarks.md`.

**Acceptance**
- On the reference machine: P1, P5, P6 met by at least one variant.
- `docs/decisions.md` records which variant (ListView vs ColumnView) is
  chosen and the numbers.
- **If neither variant meets the budgets:** stop, report numbers, and
  propose either tuning (e.g. disabling animations, GSK renderer choice
  `GSK_RENDERER=ngl|vulkan`) or a Qt6 fallback. The maintainer decides.

### M2 — Core listing engine

**Tasks**
- `tudi-core::listing` per §5.1: RawDir reader, name arena, natural sort,
  metadata worker pool with visible-first priority, generations.
- `tudi-core::tree` per §5.2 with splice triples.
- Criterion benches: list+sort 10k / 100k entries; metadata for 10k.
- Unit tests: natural sort cases (incl. CJK, mixed case, numbers,
  dotfiles), DT_UNKNOWN fallback (simulate), non-UTF-8 names, symlink
  loops not followed, permission-denied directories surfaced as an error
  row, tree expand/collapse splice correctness (property tests with
  `proptest`: random expand/collapse sequences keep the invariant that the
  row array equals a reference DFS).

**Acceptance**
- 10k list + sort (no stat) < 10 ms; 100k < 80 ms (warm cache).
- All tests green; benches recorded in `docs/benchmarks.md`.

### M3 — Tree browser MVP (daily-driver candidate)

**Tasks**
- GUI per §5.3 / §5.7 using the M1-chosen view and M2 engine.
- Path bar, back/forward/up history, hidden-files toggle, type-ahead among
  siblings, status bar counts.
- Open with default app (`gio::AppInfo::launch_default_for_uri`),
  open terminal / editor via configurable commands, copy files / copy
  paths, rename, trash, new folder, DnD out.
- inotify live updates for expanded directories (§5.1).
- Single instance + `tudi PATH`; restore last window size, root, and
  expanded paths.
- `config.toml` with defaults written on first run; gettext wired, zh_CN
  catalog started.

**Acceptance**
- P1, P2, P3, P4, P5, P6 measured on real directories and fixtures, recorded.
- Creating/deleting/renaming files from a terminal is reflected in an
  expanded directory within 200 ms without losing selection or scroll
  position.
- Input methods: renaming a file to a Chinese name via fcitx5 works under
  Wayland.
- No main-thread stall > 16 ms in a 5-minute scripted session (log long
  frames with `tracing`).

### M4 — Bookmarks and frecency

**Tasks**
- Bookmarks sidebar, `Alt+1..9`, `Ctrl+D`, reorder, rename, remove;
  `bookmarks.toml`.
- Frecency store (§5.5) with visits from the GUI; "Frequent" section.
- First-run import from autojump, zoxide, GTK bookmarks (each optional,
  silent if absent); a `--reimport` flag.
- Devices section from `gio::VolumeMonitor` (mounted volumes only).

**Acceptance**
- Jump to any bookmark in one keystroke; jump latency < 50 ms when the
  directory is cached, < P3 otherwise.
- Import tested with fixture files for each format.

### M5 — Index daemon

**Tasks**
- `tudi-index` data structure (§5.4), snapshot save/load with versioning
  and checksum, tombstones + compaction.
- `tudi-indexd`: initial scan (low priority), reconcile pass, periodic
  reconcile, inotify on top-K frecent directories, GUI listing hints.
- `tudi-proto` framing and messages; `tudi index status|rescan|stop`.
- systemd user unit; the GUI offers to enable it if not running.
- Config: roots, excludes, reconcile interval; per-root overrides (§8).
- Per-root filesystem detection (type, rotational) and the behaviour in
  §5.4 for roots on exFAT/vfat/HDD; offline/online handling on unmount.

**Acceptance**
- 2M-entry fixture: P8 (RSS) and P9 (scan time) met.
- Restart with snapshot: queries served < 1 s after daemon start.
- Kill the daemon mid-snapshot-write → next start loads the previous good
  snapshot (atomic rename) and reconciles.
- Fallback mode: a file created in a non-watched directory appears after
  the next reconcile; in a watched one within 1 s.
- An extra root on an exFAT image (loop-mounted in a test, or a real disk on
  the reference machine) is scanned, reconciled on its own interval, and
  never triggers a fanotify mark attempt that fails noisily.
- Unmounting an extra root keeps the daemon healthy; its entries are hidden
  from results until it is mounted again.

### M6 — Memory search

**Tasks**
- Matcher per §5.5 (token-mask propagation, nucleo-matcher scoring,
  incremental refinement, top-K heap), pinyin transliteration.
- Ranking with frecency + recency; weights in one struct.
- Golden search-quality test set (§9.3) with ≥ 40 cases.
- GUI search overlay (§5.7) with highlighted matches, reveal-in-tree,
  open, open-folder-in-terminal.
- `tudi q` CLI with `--json`, `-n`, `--dir`.

**Acceptance**
- P7 met on the 2M fixture (bench in CI on fixture subset; full on the
  reference machine).
- Golden set: ≥ 90% of cases have the expected file in the top 3, 100% in
  the top 10.
- Queries: `hthd` finds `合同核对.xlsx`; `acme 季报` finds files named
  `*季报*` under any `acme` directory; token order does not matter.

### M7 — fanotify helper

**Tasks**
- `tudi-fanotifyd` per §5.6, system unit + socket, install script /
  packaging hook.
- Daemon prefers helper events when available; falls back otherwise.
- Event coalescing; overflow → reconcile.
- Integration tests requiring root are `#[ignore]`; build them as the user
  (`cargo test --no-run`) and run only the test binary under `sudo`, as
  documented in `docs/design/fanotify.md` and AGENTS.md.
- Security review checklist in that doc, completed.

**Acceptance**
- P10 met: `touch ~/x/y/new.txt` → findable in < 1 s.
- `git clone` of a large repo (~100k files) does not make the daemon
  lag > 5 s behind or exceed 2× RSS temporarily.
- A client running as user A never receives paths outside A's roots
  (test with two users).

### M8 — Query power features

**Tasks**
- Full query grammar (`docs/query-syntax.md`): `.ext`, `ext:`, `in:`,
  `/dir/`, time (`<7d`, `>2025-06`, `today`, `yesterday`, `week`,
  `month`, Chinese aliases), size (`size>10M`), kind (`dir:`, `file:`).
- Scoped search: `Ctrl+Shift+F` searches under the selected directory.
- Optional pinyin collation for sorting.
- `tudi init zsh|bash|fish` (`tj` jump function + optional visit hook).

**Acceptance**
- Parser has unit tests for every documented form, including invalid
  input producing a helpful inline hint rather than an error dialog.
- `合同 .pdf 上月` returns only PDFs modified last calendar month whose
  path matches "合同".

### M9 — Polish and v0.1 release

**Tasks**
- Copy/move via DnD-in and paste with GIO async operations and a
  progress indicator; undo for rename/trash (single level).
- Optional quick preview pane (text, markdown, images) toggled with
  `Space`; loads asynchronously, size-capped.
- Accessibility pass (keyboard-only use, screen-reader labels).
- Packaging: `cargo-deb` `.deb` (GUI + daemon + user unit; helper as a
  separate optional package `tudi-fanotify`). Flatpak is deferred: the
  sandbox conflicts with whole-home indexing and the root helper (note in
  README).
- Docs: README with screenshots/GIF, `docs/architecture.md` final,
  CONTRIBUTING.md, CHANGELOG.md.
- Tag `v0.1.0`.

**Acceptance**
- Fresh Ubuntu 24.04 and 26.04 VMs: install `.deb`, run, index, search,
  uninstall cleanly.
- All performance budgets re-measured and published in
  `docs/benchmarks.md`.

---

## 8. Configuration

`$XDG_CONFIG_HOME/tudi/config.toml` (all keys optional; defaults documented in
`docs/config.md`):

```toml
[browse]
show_hidden = false
dirs_first = true
sort = "natural"          # natural | name | mtime | size
collation = "codepoint"   # codepoint | pinyin

[index]
roots = ["~"]
exclude = [".cache", "node_modules", "target", "__pycache__", ".venv", ".git/objects"]
reconcile_minutes = 10
use_fanotify = "auto"     # auto | never

# Extra roots with their own settings (all keys except `path` optional).
[[index.extra_roots]]
path = "/mnt/data"
reconcile_minutes = 1440  # default for rotational devices
exclude = ["$RECYCLE.BIN", "System Volume Information"]

[search]
max_results = 200
translit = ["pinyin"]

[commands]
terminal = ["kitty", "--directory", "{dir}"]
editor = ["zed", "{path}"]

[keys]
# action = "accelerator" overrides
```

---

## 9. Testing and benchmarking

### 9.1 Unit and property tests
Every core module has unit tests; tree splicing and index mutation have
`proptest` property tests against a naive reference implementation.

### 9.2 Performance benchmarks
- `cargo bench -p tudi-core` / `-p tudi-index` (criterion) on fixtures.
- GUI benchmarks: the `--bench` mode from M1 is kept and extended in
  `tudi-gui` behind a hidden flag `tudi --bench <scenario>`, emitting JSON.
- `scripts/bench_all.sh` runs everything and updates
  `docs/benchmarks.md` with machine info (CPU, disk, kernel, GTK version).
- A regression of > 20% on any budgeted metric blocks a milestone.

### 9.3 Search quality golden set
`crates/tudi-core/tests/golden/search.toml`: a fixture tree description plus
cases `{ query, expected_top3 | expected_top10, note }`. Includes English,
CJK, pinyin initials, misspellings, path-fragment queries, and
time/extension filters (from M8). Run as a normal test; prints a quality
report.

### 9.4 Manual test checklist
`docs/manual-test.md`: IME input, HiDPI, dark mode, very long names,
non-UTF-8 names, unreadable directories, symlink loops, removable media
unplugged while browsing, 100k-file directory on a slow USB stick.

---

## 10. Risks and mitigations

| Risk | Mitigation |
|------|------------|
| GTK4 list performance insufficient | M1 gate before any real work; fallback options listed there |
| `Vec::splice` too slow for huge expands | switch tree storage to chunked rope behind same API |
| Index memory too high | struct-of-arrays, 24-byte nodes, pinyin only for CJK names, excludes |
| fanotify helper security concerns | optional, subscribe-only, peer-uid filtering, minimal caps, documented review |
| Event storms (git checkout, builds) | per-directory coalescing, overflow → reconcile |
| Stale index without helper | GUI listing hints + frecent-dir inotify + periodic reconcile |
| Non-UTF-8 / odd filenames | bytes end-to-end, lossy only at display |
| Agent drift from budgets | budgets are automated benchmarks; AGENTS.md hard rules |

---

## 11. Post-v0.1 ideas (not scheduled)

- Full-text content search (ripgrep on demand; later a `tantivy` index for
  selected roots).
- Tags/notes on files stored in xattrs (`user.tudi.*`), searchable.
- Split panes / second tree for copy-move workflows.
- Git status decorations in the tree.
- "Memory" by context: files opened together, files near a time you were
  working on something.

---

## 12. Progress

- [x] M0 Repository bootstrap
- [ ] M1 GTK4 list spike (decision gate)
- [ ] M2 Core listing engine
- [ ] M3 Tree browser MVP
- [ ] M4 Bookmarks and frecency
- [ ] M5 Index daemon
- [ ] M6 Memory search
- [ ] M7 fanotify helper
- [ ] M8 Query power features
- [ ] M9 Polish and v0.1 release
