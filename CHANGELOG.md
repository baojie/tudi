# Changelog

## Unreleased

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
