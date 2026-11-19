# Changelog

## Unreleased

## 0.1.2 — 2026-11-20

Tabs and a side-by-side view, thumbnails, drag and drop, resizable
columns and nine more interface languages.

- Tabs in the file list, named after their folder, "+" after the last tab
  (`Ctrl+T`, `Ctrl+W`, `Ctrl+Tab`, middle-click a folder or "Open in New
  Tab"); the list can be split in two sides, each with its own tabs
  (tab menu "Open Side by Side" or `Ctrl+\`). Tabs are restored on the
  next start.
- Thumbnail view: in a folder with images, a header-bar button (or
  `Ctrl+2` / `Ctrl+1`) switches between thumbnails and the list.
- Drag files onto a folder in the list or the tree, or onto the list's
  empty space, to move them (same disk) or copy them (another disk);
  `Ctrl` copies, `Shift` moves. Works with files from other apps.
- Columns: drag the handles between the column headers to resize the size
  and modified columns, double-click a handle to fit the column to its
  content (up to a limit); remembered per window. A faint line separates
  the columns. When the name would get too narrow (e.g. side by side), the
  modified and then the size column are hidden.
- Languages: Traditional Chinese, Japanese, Korean, German, French,
  Spanish, Portuguese (Brazil), Italian and Russian, selectable in the
  settings (or `[ui] language = "de"` …) whatever the system language.
- A header-bar button shows or hides hidden files (`Ctrl+H`).
- "New File…" in the list's right-click menu on empty space.
- Bookmarks bar: shows as many buttons as fit the window instead of at
  most 10; long folder names are shortened to their first two words or a
  length limit (full name in the tooltip and the drop-down). Bookmarks
  and frequent folders are ordered by how often they were opened
  (`Alt+1..9` follow the shown order); drag a bookmark to put them in
  your own order instead, "Sort by Open Count" goes back.
- Fix: right-click on the list's empty space showed no menu; a context
  menu taller than the room above and below the pointer closed at once.
- Fix: an empty band above the column headers (the closed find bar still
  took its height).

## 0.1.1 — 2026-11-20

Memory search with its index daemon, the bookmarks bar, launchers and many
browsing improvements.

- `Ctrl+F` finds in the shown folder: typing jumps to the first name
  containing the text, `↓`/`F3` and `↑`/`Shift+F3` step through matches,
  `Enter` filters the list to the matches, `Esc` clears.
- The status bar says what is running in the background (reading the
  folder, file details, folder size, copying, moving, trashing) with a
  spinner, for work that takes longer than a moment.
- Fix: launching tudi from the dock while it was already running left the
  dock icon busy until a timeout.
- Context menus always show all their items (no scrollbar); near the
  bottom of the window they open upwards.
- Bookmarks bar: launchers first, then bookmarks and frequent folders; at
  most 10 buttons, the rest in an "N more" drop-down.
- Search filters (`docs/query-syntax.md`): `.pdf`, `ext:`, `in:acme`,
  `dir:` / `file:`, `<7d`, `>2025-06`, `date:2025-06`, `today` … `lastyear`
  and `今天 昨天 本周 上周 本月 上月 今年 去年`, `size>10M`. Unreadable filters
  show a hint instead of failing.
- `Ctrl+Shift+F` searches below the selected folder; `tudi q --in DIR`.
- Shell integration: `tudi init bash|zsh|fish` adds `tj` to jump to
  folders; `--hook` records each `cd` in the folder history.
- Optional pinyin sorting of Chinese names (settings → Browsing).
- Extra index roots (`[[index.extra_roots]]`), e.g. a data disk or SD card:
  slow devices are checked daily, unmounted ones are hidden from results
  and re-checked when they come back.
- Memory search: `Ctrl+P` (or `/`) searches every file and folder in your
  home by name fragments in any order, pinyin initials or full pinyin, and
  tolerates typos; results in ~10 ms on two million entries.
- Index daemon `tudi-indexd` (started on demand, or at login via the
  settings), and `tudi q TERMS…` / `tudi index status|rescan|stop` on the
  command line.
- First-run import of GTK bookmarks and autojump / zoxide history;
  `tudi --reimport`.
- Mounted volumes appear as roots in the folder tree.
- Free space in the status bar.
- Properties dialog (context menu, `Alt+Enter`): type, location, dates,
  permissions, owner, and the size of folders counted live with a spinner.
- Finder-like look: light sidebar, blue selection in the list (grey when
  unfocused), tighter rows.
- Status bar: files and folders in the shown folder, and its total size
  (computed in the background, skipped if it takes over a second).
- Context menu on folders in the tree (open, expand, terminal, copy / cut /
  paste into, new folder, rename, bookmark, trash).
- Double-click (or Enter) on a folder in the tree expands or collapses it.
- Launchers: `Ctrl+D` on a script, executable or `.desktop` file pins it to
  the bookmarks bar; click to run (in a terminal, or in the background with
  a log), drop files on it to run it with them.
- tudi's own drawn file icons: blue folders, pages with a colour band per
  file type.
- Cut / copy / paste files (`Ctrl+X`, `Ctrl+C`, `Ctrl+V`), also with other file
  managers; `Ctrl+A` selects all.
- Context menu on files and on empty space in the list.
- Bookmarks bar under the header: your bookmarks (`Ctrl+D`, `Alt+1..9`,
  drag to reorder), then your most used folders; a "»" menu lists all.
  Replaces the "Frequent" tree root.
- Settings dialog (gear button, `Ctrl+,`); changes are saved to
  `config.toml` keeping your comments.
- tudi can be the default folder handler (`inode/directory`, file:// URIs).
- Fix: the "Frequent" tree root showed an expander and a folder icon.
- Fix: files of unknown type showed a missing-image icon with some icon
  themes; executables without an extension get an executable icon.
- Always-visible, wider scrollbars.

## 0.1.0 — 2026-11-18

First public preview. Usable as a daily file browser; search (index
daemon, `Ctrl+P`), bookmarks bar and launchers are not there yet (see
`docs/PLAN.md`).

- Two panes: folder tree (directories only) on the left, details list on
  the right with sortable name / size / modified columns, natural sort,
  directories first.
- Fast listing: `getdents64` without `stat`, metadata on a worker pool with
  visible rows first; 100k-entry folders open in ~140 ms.
- Live updates via inotify for the shown folder and expanded tree folders,
  keeping selection and scroll position.
- History, path bar, type-ahead, hidden files (`Ctrl+H`), copy files and
  paths, open terminal / editor (configurable), rename, trash, new folder,
  drag out.
- "Frequent" view: folders by frecency with open counts and last visit;
  folders can be excluded (with or without subfolders, or by glob rules).
- Resident single instance: closing the last window keeps tudi running, so
  windows reopen in ~30 ms; `tudi PATH` forwards to it in ~10 ms.
- Interface zoom (`Ctrl+=`, `Ctrl+-`, `Ctrl+0`).
- English UI by default; Simplified Chinese with `[ui] language = "system"`
  on a Chinese system.
- Packages: `.deb` and a `tar.gz` for Ubuntu 24.04 or newer (GTK ≥ 4.14,
  libadwaita ≥ 1.5).
