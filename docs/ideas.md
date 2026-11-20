# Ideas from Finder and Windows Explorer

Design ideas borrowed from macOS Finder and Windows Explorer, ranked by
value for tudi (keyboard-first, fast). Status: **done** (with the commit
area), **planned**, or open. Recorded 2026-11-20.

## Tier 1 — daily use, high value

1. **Undo (`Ctrl+Z`)** — both have it. Move, rename, trash, new folder or
   file, copy: the last operations can be undone one by one. Most
   valuable since drag and drop moves by default. *Done* (`undo.rs`).
2. **Ask on name conflicts** — Explorer's "Replace / Skip / Keep both"
   when pasting or dropping onto existing names; tudi silently renamed to
   `name (2)`. *Done* (`drop.rs`).
3. **Clickable path bar** — Explorer's breadcrumb: every level is a button,
   the `›` after it lists that folder's subfolders to jump to; each level
   is a drop target. Click the empty part or `Ctrl+L` to type a path.
   *Done* (`crumbs.rs`).
4. **Quick Look (`Space`)** — Finder: a large preview over the window,
   arrow keys step through files, `Space`/`Esc` closes. Reuses the preview
   pane's loaders. Already in PLAN (M9).
5. **"Open With" submenu** — both: other applications for the selected
   file, not only the default one.

## Tier 2 — efficiency

6. **Group by date or kind** — Explorer: sections "Today / Yesterday /
   This week / Earlier" in the list (great for Downloads); the date words
   already exist in the query syntax.
7. **Optional columns** — right-click the column header: type, created,
   permissions, item count of folders. Columns already hide when the name
   needs the room.
8. **Spring-loaded folders** — Finder: while dragging, hovering over a
   folder (or a tab, or a tree row) opens it, to drop deep in one drag.
9. **`Ctrl+wheel` sizes** — thumbnail size in the grid, row height in the
   list (Explorer and Finder have a size slider).
10. **View switch in the status bar** — Explorer's two buttons at the
    bottom right (list / thumbnails), always visible.

## Tier 3 — larger features

11. **Column view** — Finder's Miller columns, one column per level; a
    third view for deep project trees.
12. **Colour tags** — Finder tags, stored as extended attributes
    (compatible with GNOME Files' stars where possible), searchable.
13. **Recent files** — Explorer's Quick access: files opened recently,
    shown in the empty search card.
14. **Batch rename** — `F2` on several files: replace text, number, date.
15. **Progress window** — speed, time left, pause / cancel for large
    copies (now a status-bar spinner).

## Not copied

- Finder's gallery view: the thumbnail view plus the preview pane cover it.
- Explorer's ribbon and big toolbars: tudi stays keyboard-first and lean.
- Selection check boxes: made for touch; keyboard and mouse multi-select
  suffice.
