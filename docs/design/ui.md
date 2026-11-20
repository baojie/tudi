# UI (M3)

Code: `crates/tudi-gui/src/`. Plan: `docs/PLAN.md` §5.3, §5.7.

## Modules

| Module | Role |
|---|---|
| `main.rs` | `App`: single instance, resident process, window registry, config/state loading |
| `browser.rs` | `Browser`: one window; navigation, history, tree/list sync, worker replies, live updates |
| `pane.rs` | details list: `PaneModel` (`gio::ListModel` over sorted entry indices), row widgets, column header |
| `sidebar.rs` | folder tree: `TreeModel` (`gio::ListModel` over `tudi_core::tree::Tree` rows) |
| `worker.rs` | listing threads: `Job` in, `Reply` out over `async-channel` |
| `actions.rs` | type-ahead, clipboard, external commands, rename, trash, new folder, drag out |
| `find.rs` | `Ctrl+F` find bar: jump to matches, `Enter` filters the list |
| `tabs.rs` | tabs of the list, the tab strip, side-by-side tiles |
| `undo.rs` | `Ctrl+Z`: the window's last file operations, reversed |
| `crumbs.rs` | clickable path bar: a button per folder, `›` lists subfolders |
| `preview.rs` | optional preview pane: image, start of a text file, or details |
| `markdown.rs` | Markdown to styled text runs for the preview pane |
| `highlight.rs` | syntax highlighting (syntect) into the same runs |
| `video.rs` | first frame of a video (GStreamer appsink) |
| `thumbs.rs` | thumbnail view: grid cells, thumbnail loader threads, texture cache |
| `activity.rs` | background tasks named in the status bar, with a spinner |
| `format.rs` | sizes, dates, icons from file names |
| `dev.rs` | `TUDI_DEV_SCREENSHOT` and `TUDI_DEV_SCRIPT` test hooks |

## Threads and data flow

- The GTK main thread never touches the filesystem. Listing, sorting,
  filtering hidden files and name lookups run on two `tudi-list-*` threads
  (`worker.rs`); `statx` runs on the `tudi-meta-*` pool (`tudi-core`);
  inotify on `tudi-watch`; file operations use GIO's async API; external
  commands are spawned on a short-lived thread that also reaps the child.
- Replies are drained on the main loop; everything already queued is
  applied in the same turn.
- Every pane listing has a generation. Stale listings, orders and metadata
  batches are dropped.

## Details list

- The display order (`order`: entry indices) and its inverse (`pos_of`)
  are computed on a worker.
- Model items are *positions*: item i means "row i", whose entry is
  `order[i]` at bind time. A new order (sort, hidden toggle, other folder)
  reports only a changed row count, as a tail change, and refills the bound
  row widgets in place. Replacing all items would make GTK destroy and
  recreate its ~200 cached row widgets. Selection is always restored (or
  cleared) explicitly after a new order.
- Metadata arrives after the rows are shown. Bound row widgets are tracked
  per entry; a batch refills only those widgets, so no `items-changed` is
  emitted and the selection is untouched.
- Rows bound while their metadata is pending are pushed to the fetcher's
  priority queue once per frame: what is on screen is `stat`ed first.
- Re-sorting after metadata (by size or time, or resolved `DT_UNKNOWN`) is
  requested once, when the metadata is complete.
- Type-ahead scans at most 4,096 rows per main-loop turn and continues in
  idle callbacks; a newer keystroke cancels an older scan.
- The status bar sums sizes only for selections of ≤ 5,000 entries.
- Hidden entries (dot names) are dimmed — name and icon at 55 % opacity —
  in the list, the tree and the thumbnail grid (`pane::hidden_opacity`).
- Column widths: the header has an 8 px drag handle left of the size and
  date columns (the gaps the rows have between columns). Dragging sets that
  column's width in pixels (24–800; the name column takes the rest);
  double-click fits the column to its widest text (header and the rows on
  screen), at most 280 px. Without a dragged width a column has its
  default width in characters, which follows the zoom. Widths live in `pane::Columns`, shared by the header and the
  rows; a drag re-applies them to the bound rows only, and a row gets them
  when bound. The drag is measured in window coordinates, since the handle
  moves with the column edge. Saved per window in `state.toml`
  (`size_column`, `date_column`, 0 = default).
- Columns that do not fit are hidden from the right (the date, then the
  size) so the name keeps at least 180 px; they come back when there is
  room (`pane::columns_to_hide`, re-checked in an idle callback when the
  tab page's width changes and after a drag or fit). Header and rows hide
  together (`pane::Hidden`, per tab).
- A faint 1 px line separates the columns: the left border of the size
  and date widgets, in the header (buttons) and in every row (labels,
  pulled 1 px into the gap and 2 px into the row padding so the line is
  continuous and the text does not move; a higher alpha because
  `dim-label` halves it). Both hang off the column's own edge, so they
  line up at any zoom. The header's right margin is the vertical
  scrollbar's width while it is shown and 0 otherwise (a short folder has
  none), keeping header and row columns at the same x.

## Folder tree

- Roots: Home, File System, then every mounted disk (ADR-033): the
  mounts of `gio::VolumeMonitor` plus disk mounts read from
  `/proc/self/mountinfo` (fstab mounts such as `/data`, which GIO hides),
  added and removed live.
- Selecting a folder shows it in the list. Navigation from anywhere reveals
  the folder in the tree: ancestors are expanded (listing as needed) and the
  folder is selected; if the path goes through a hidden folder while hidden
  files are off, nothing is selected. The selected row is scrolled to the
  middle of the tree once the inserted rows are measured (a few frames;
  `tudi PATH` from outside can expand hundreds of rows).
- Expanded state is remembered while collapsed (`tudi-core` tree), not
  across sessions: a new window's tree opens only down to the shown folder
  (`state.toml` keeps `expanded` empty).
- Tree listings include symlinks to directories (resolved with `statx` on
  the worker).

## Live updates

- `tudi_core::watch::Watcher` per window, keys `Pane` (the shown folder,
  including file modifications) and `Tree(node)` (expanded folders, entry
  changes only), at most 512 keys, LRU-evicted.
- Events are coalesced: reported 50 ms after the first event of a burst and
  at most every 150 ms per key.
- The list re-lists the folder and restores the selection by name (up to
  5,000 entries) and the scroll position. The tree replaces only the
  changed rows (`Tree::refresh_children`), keeping node state.
- Known cost: a live reload re-`stat`s every entry of the folder.

## Resident instance and single instance (ADR-012)

- `GApplication` with `HANDLES_COMMAND_LINE`. `tudi PATH` in a second process
  is forwarded: the last window navigates to PATH and is raised, or a new
  window opens if none is open. `--new-window`, `--background`, `--quit`.
- The process holds itself: closing the last window drops the window's
  `Browser` (models, listings, watches) and saves the window state; the
  process stays for the next window.
- Startup notification: the launcher `tudi` is a plain `GApplication`,
  which (unlike `GtkApplication`) does not put the shell's activation token
  in the forwarded platform data. It sets `SEND_ENVIRONMENT`; the primary
  reads `XDG_ACTIVATION_TOKEN` (or `DESKTOP_STARTUP_ID`) from the forwarded
  environment and calls `set_startup_id` before presenting the window.
  Without it the dock shows a busy launcher until the shell times out.

## Context menus

`menu.rs::popup_at` shows every context menu (list, tree, bookmarks bar).
GtkPopoverMenu puts its items in a scrolled window, and on a multi-monitor
GNOME session menus came up shorter than their items and scrolled. The
helper turns that scrollbar off (the items become the menu's minimum
height) and opens the menu upwards when it does not fit below the pointer
inside the window but does above it. When it fits on neither side, scrolling
stays on and the menu opens on the roomier side: an unscrollable popover
taller than its room is dismissed at once. A popover is never parented to a
`GtkListView` or `GtkScrolledWindow` (they do not present popover children,
so it would stay invisible); the helper moves up to the first other
ancestor. The list's empty-space right-click gesture runs in the capture
phase (the list view's own gestures take the press otherwise) and leaves
presses inside a row (`.tudi-row`) to the row's gesture.

## Keys

See PLAN §5.7. Implemented in M3: history (`Alt+←/→`), up (`Alt+↑`,
`Backspace` in the list), `Ctrl+L`, `Ctrl+H`, `F6`, tree `←/→`, `Enter` and
double-click, type-ahead, `Ctrl+C`, `Ctrl+Shift+C`, `Ctrl+Alt+T`, `Ctrl+E`,
`F2`, `Delete`, `Ctrl+Shift+N`. List-only keys (`Ctrl+C`, `Delete`, `F2`)
are bound on the list so they do not take keys from the path bar.

## Drop and paste (ADR-022)

`drop.rs`: list rows, the list's empty space and tree rows take files
dragged from tudi or other apps (`GdkFileList`). A folder row takes them
into that folder; a file row or empty space, into the shown folder. With no
modifier, each item is moved when it is on the destination's filesystem
and copied otherwise (`fileops::Choice::Auto`, compared by `st_dev` on the
transfer thread, never on the main thread); items already in the
destination are skipped. `Ctrl` copies, `Shift` moves. A folder dropped on
itself is ignored. Paste (`Ctrl+V`, menus) uses the same `transfer_into`:
a helper thread, a status-bar task, one toast ("Moved 2, copied 1"). Rows
under a drag are highlighted (`:drop(active)`). The list's background menu
also has "New File…" (an empty file, never replacing one).

## Undo (`undo.rs`, docs/ideas.md #1)

Each finished copy or move (paste, drop), rename, trash, new folder or
file pushes an `Op` on the window's stack (50 deep). `Ctrl+Z` in the list
or the tree, or "Undo" in the list's background menu, pops the last one
and reverses it on a helper thread: moved items go back
(`fileops::move_back`, never over an existing name), copies and new items
go to the trash, trashed items come back from `trash:///` (matched by
`trash::orig-path`, the latest trashed before the undo started, so a copy
the undo itself just trashed is not taken for a replaced original),
renames are reverted. The toast says what was undone or what could not
be.

## Name conflicts (`drop.rs`, docs/ideas.md #2)

Before a paste or drop, `fileops::conflicts` (helper thread) lists the
names that already exist in the destination. If there are any, a dialog
asks: **Replace** (the existing items go to the trash first, so undo
brings them back; if they cannot be trashed, both are kept and a toast
says so), **Skip** (those items are left out), **Keep Both** (default:
`name (2)`), or Cancel.

## Path bar (`crumbs.rs`, docs/ideas.md #3)

The header's path is a row of buttons, one per folder (from "Home" below
the home folder, else from "/"), the current one in bold; the end stays in
view. A click goes there; files dropped on a button go into that folder.
The `›` after each lists its subfolders (read on a helper thread, hidden
ones only when shown, at most 300) to jump to. A click on the empty part
or `Ctrl+L` shows the text entry with the path selected; `Enter` goes
there, `Esc` or leaving the entry shows the buttons again.

## Status bar activity

`activity.rs`: background work of the window — listing the folder,
reading file details, measuring the folder size, copying, moving, trashing
— registers a `Task` while it runs. Tasks running longer than 300 ms show a
spinner and a label ("Measuring folder size…") at the start of the status
bar, so a busy window says what it is doing; shorter ones never appear.
The status text is always rebuilt whole by `update_status` (a task's
delayed display calls it too), so nothing else writes the label: a listing
error is kept in `pane_error` and shown instead of the counts until the
next listing is requested.

## Tabs (ADR-025)

`tabs.rs`: each tab (`Tab`) owns a details list — model, selection, view,
column header, find bar, folder, back/forward history, sort order,
listing generation, metadata fetcher, folder size, live watch
(`WatchKey::Pane(tab id)`). The window keeps the tree, path bar, bookmarks
bar, status bar, worker and watcher, and follows the active tab: switching
tabs re-syncs the path bar, title, history buttons, tree selection and
status. Hidden files and column widths are per window.

- Code written for one list calls `Browser::tab()`: the active tab, or,
  while `with_tab` runs, the tab a reply or a widget event belongs to.
  Listing generations come from one per-window counter, so worker replies
  (`Pane`, `Sorted`, `Meta`, `PaneError`, `FolderSize`) find their tab by
  generation; replies for closed tabs find none and are dropped. Window
  chrome (status, path bar, tasks in the status bar) is only touched for
  the active tab.
- The strip above the list: one button per tab (folder name, at most 22
  characters, full path in the tooltip, close button), then "+". Click
  selects, middle-click closes, right-click: New Tab, side by side, Close
  Tab, Close Other Tabs. Middle-click on a folder in the list, or "Open in
  New Tab" in the list and tree menus, opens it in a background tab.
- Side by side: the list area has two sides (`TabStrip::sides`), each with
  its own tab strip, "+" and shown tab; a tab's `side` says where it is.
  Without tabs on the right only the left side is shown; with them, both
  sit in a `GtkPaned` split in the middle. "Open Side by Side" (tab menu,
  `Ctrl+\`) moves a tab to the right (a lone tab is copied), "Move to
  Other Side" moves it back and forth, "Stop Side by Side" puts every tab
  on the left; a side left without tabs ends the split. New tabs open on
  the active tab's side, `Ctrl+Tab` cycles within it. A press or focus in
  a side makes its tab active, so menus, keys and drops act on the side
  they happen in; the active side has an accent line on top.
- Each tab page sits in a `GtkScrolledWindow` with an external (hidden)
  horizontal scrollbar, which does not ask for the page's minimum width:
  wide dragged columns are cut off on the right of a narrow side, instead
  of widening the window (or GtkPaned sliding the side out of view).
- Tabs are saved in `state.toml` (`tabs`, `active_tab`, `right_tabs`) and
  restored in the
  first window when no folder was given; tabs other than the active one
  are listed when first shown.

## Thumbnail view (ADR-026)

When the shown folder has images (counted by extension on the listing
worker, with the order: `Order::images`), a header-bar button switches the
tab between the list and a `GtkGridView` of thumbnails (`Ctrl+1` /
`Ctrl+2`). Both views share the tab's model and selection, so the
selection, sort, find filter, keys, menus, drag and drop carry over; the
column header is hidden in the grid. Grid cells show a 128 px thumbnail
for images and the file icon otherwise.

`thumbs.rs`: two `tudi-thumb-*` threads per window serve requests newest
first (cells just scrolled into view), at most 400 pending. A request
loads the freedesktop large thumbnail (`~/.cache/thumbnails/large`, MD5 of
the URI) when it is not older than the file, else decodes the image with
gdk-pixbuf at 256 px (EXIF orientation applied). Pixels go to the main
thread, which makes a `GdkMemoryTexture`, keeps up to 3,000 per window,
and refills the bound cells. Undecodable files are remembered and keep
their icon. A live reload of the folder drops its textures.

## Preview pane (ADR-027)

`preview.rs`: `Alt+P` or a header-bar toggle shows a pane right of the
list area, or below it (the button at the pane's top switches; the
`GtkPaned` turns vertical). Its place and size are saved per window
(`preview_below`, `preview_width`, 0 = hidden). It follows the
active tab: one selected image is shown whole (decoded at up to 1600 px),
a text or code file shows its first 64 KB (monospace; NUL bytes mean
binary, shown as details), Markdown (`.md`, `.markdown`, `.mdown`, `.mkd`,
`.mdx`) rendered (ADR-030), code files and fenced code blocks
syntax-highlighted (ADR-031), a video its first frame (ADR-032), anything
else its icon, name, size and date;
several selected items show their count and size, none the shown folder.
Files are read on a short-lived thread; a serial number drops results of
an older selection. The pane is refreshed from `update_status` (every
selection change) and skips the work when the item (path, size, mtime)
is unchanged.

## Find in folder

`find.rs`: `Ctrl+F` reveals a `GtkSearchEntry` above the column header.
Typing selects and scrolls to the first row whose name contains the text
(case-insensitive, lossy UTF-8), scanning in 4096-row chunks on idle like
type-ahead (both share one scan serial, so a newer scan cancels an older
one); keyboard focus stays in the field. `Down`/`F3` and `Up`/`Shift+F3`
jump to the next / previous match, wrapping. No match turns the field red.
`Enter` filters: the lowercase text goes with `Job::Sort`/`Job::ListPane`
and the worker drops non-matching rows after the hidden-file filter, so
counts, selection and live reloads all see the filtered order; the status
bar says "filtered by …" and focus moves to the list (the jumped-to row
stays selected). `Enter` on an empty field removes the filter. `Esc` in the
field or in the list hides the bar and removes the filter, and focuses the
selected row once the unfiltered order arrives (rows are positions, so the
row cannot be focused before). Showing another folder hides the bar and
clears its text, red state and filter.

A filter (or sort, or hidden toggle) change while a newer listing is in
flight, or before the first one arrived, sets `resort_pending`: that listing
was ordered with the old parameters and its arrival rejects the older
`Sorted` reply, so the order is recomputed when it arrives. Sending a new
`ListPane` clears the flag (it carries the current parameters).

## Bookmarks bar and folder history (ADR-016)

As many buttons as fit the window's width (launchers, then bookmarks,
then frequent folders); the rest are in a drop-down at the right end,
labelled "N more" and hidden when nothing overflows. Up to 40 buttons are
built and measured once (natural width) on each rebuild; the fit is redone
in an idle callback when the bar's width changes (its clip's horizontal
adjustment), never during allocation. Long names are shortened on the
buttons to their first two words (split at spaces, `-`, `_`, `.`) or to 16
columns (CJK counts two), with an ellipsis; tooltips and the drop-down
show the full name.

A visit (PLAN §5.5) of the folder a tab is still listing is recorded only
once the listing arrives (`Tab::visit_after_load`) and dropped if the
folder cannot be listed, so a mistyped path never becomes a frequent
folder. Paths of list rows are built from the folder the rows were listed
from (`PaneData::dir`), not from the tab's current folder: while a slow
folder (an automount) loads, the old rows are still shown.

- `bar.rs`: a bar under the header, like a browser's bookmarks bar: the
  user's bookmarks (`bookmarks.toml`, `Ctrl+D`, `Alt+1..9` in the shown
  order, right-click to rename / remove), a separator, then up to 12 most
  used folders by frecency. Both groups are ordered by open count, most
  first (ADR-023): the history snapshot carries every folder's count;
  bookmarks never opened keep their saved order at the end. Dragging a
  bookmark onto another switches bookmarks to the saved order
  (`manual_order`); "Sort by Open Count" switches back (tooltip: path, open count, last visit;
  right-click: pin as bookmark, never show — folder or with subfolders —,
  remove from history). The buttons are clipped, not wrapped; a "»" menu
  lists all bookmarks and the 30 most used folders.
- `history.rs`: one `tudi-history` thread per process owns the frecency
  database; windows send visits and changes and receive snapshots (top 30
  plus exclusions) as `Reply::History`.
- Counted visits: list activation of a folder, the path bar, a bar button,
  `tudi PATH`, and tree selections still shown after 1 s. Not counted:
  back/forward, up, startup restore, live reloads.
- `shared.rs`: config, bookmarks and the history handle are shared by all
  windows; bookmark and history changes refresh every window's bar.

## Settings

`settings.rs`: the gear button (`Ctrl+,`) opens an `AdwPreferencesDialog`:
General (language, zoom, hidden files, folders first, sort), Commands
(terminal, editor), History (exclude patterns, excluded folders with
"Allow", clear history). Changes apply to all windows immediately and are
written to `config.toml` with `Config::save_preserving`, which keeps the
user's comments (`toml_edit`). The language takes effect after a restart.

## Search overlay (M6)

`search.rs`: `Ctrl+P`, `/` (in the list) or the magnifier button in the
header bar (so the search can be found without knowing the keys) shows a
card above the panes
(`GtkOverlay`) with a `GtkSearchEntry` and up to 100 results: icon, name
with matched characters in bold, folder, size and date. A `tudi-search`
thread per window talks to the daemon (starting it if needed), sends only
the latest pending query, and also forwards folder listings with complete
metadata as `ListingHint`s. `Enter` reveals (navigates to the folder and
selects the item), `Ctrl+Enter` opens, `Alt+Enter` opens a terminal there,
`Esc` closes. While the field is empty, the line under it says how to
search (fragments in any order, pinyin, `dir:`, `in:`, `.pdf`, `<7d`,
`Ctrl+Shift+F`); the field's tooltip says the same.

The query syntax (`docs/query-syntax.md`) is parsed once more in the GUI
(`tudi_index::query::parse_local`, microseconds) only to show translated
hints for unreadable filters at once, under the field. `Ctrl+Shift+F`
scopes the search to the selected folder (or the shown one): a chip above
the field shows its name (full path in the tooltip); `×` or `Backspace`
in the empty field clears it. The folder kind comes from the row data, not
a `stat`.

Optional pinyin collation (`[browse] pinyin_sort`, settings switch):
names are ranked by `natural::pinyin_key` on the worker thread; switching
it lists the folder again.
