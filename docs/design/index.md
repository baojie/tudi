# Index and daemon (M5)

Code: `crates/tudi-index` (data, scan, snapshot, matcher),
`crates/tudi-indexd` (daemon), `crates/tudi-proto` (IPC).
Plan: `docs/PLAN.md` §5.4.

## Data

`Index` holds 24-byte `Node`s in parent-before-child order, one name
arena, and `first_child` / `next_sibling` arrays for updates by folder.
Roots are nodes whose name is their absolute path. Deletions set the
`TOMBSTONE` flag; `compact()` rewrites the arrays when tombstones exceed
20 %. Flags: `EXCLUDED` (indexed, contents not), `HIDDEN`, `CJK`.

## Scanning

`scan::build` / `scan_below`: level-by-level breadth-first walk; each
level is listed in parallel (rayon, `getdents64` + `statx` per entry, never
following symlinks), then applied in order. Not descended into: folders
matching `[index] exclude` and folders on another mount than their root.
`scan::reconcile`: `statx` every folder in parallel, re-list those whose
mtime changed, scan new folders fully. (Folder mtimes change on create,
delete and rename of entries; file size changes are picked up when the
folder is listed again for another reason.)

## Snapshots

`snapshot::write_to` streams magic, version, roots, nodes, names and a
CRC32; `save` writes a temp file, fsyncs and renames. `load` checks magic,
version, checksum and structure (names in range, parents before children)
and rebuilds the child lists. `$XDG_CACHE_HOME/tudi/index-v1.bin`.

## Daemon

- Single instance per user (a live socket means another daemon).
- Lowers its own priority (nice 10, idle I/O) before starting threads.
- Serves queries from the loaded snapshot at once; the updater thread then
  reconciles (or scans from scratch without a snapshot of the same roots).
- One updater thread owns every change: periodic reconcile, inotify events
  for the 200 most used folders, listing hints from the GUI, rescans. Search
  data is rebuilt off the write lock and swapped in; new nodes are
  searchable before that because the prefilter lets nodes without a
  character bag through.
- Snapshot saved after every scan / changing reconcile, at most once a
  minute for small changes, and on `Stop`.
- After large builds `malloc_trim` returns the temporary memory of rayon
  threads to the system (RSS 140 MB with 1.83M entries).

## Protocol

`tudi-proto`: `$XDG_RUNTIME_DIR/tudi/indexd.sock`, frames of a `u32` length
plus `postcard`. Requests: `Query`, `Status`, `Rescan`, `Stop`,
`ListingHint`, `FrecencyChanged`. Clients: the GUI's search thread (also
sends hints) and `tudi q` / `tudi index` in the GIO-only launcher; both
start the daemon on demand. `systemctl --user enable tudi-indexd` (or the
settings switch) starts it at login instead.

## Roots and devices

`fsinfo::detect` reads the filesystem type (`statfs`) and the block device
(sysfs: rotational, USB, removable). Slow roots (rotational or USB — an SD
card in a USB reader reports "rotational" too) default to a daily
reconcile; `[[index.extra_roots]]` can set `reconcile_minutes` and add
excludes. Folder mtimes are trusted except on FUSE, where a reconcile
re-lists the whole root. exFAT, vfat, NTFS and FUSE are marked as not
supporting fanotify (for M7).

A root looks offline when it is missing or is an empty folder although the
index has entries for it (an unmounted mount point). Offline roots keep
their entries, are hidden from results (`Context::offline_roots`), are
checked every 30 s and reconciled as soon as they are back.

Scans of a root run into a separate `Index` and are grafted into the live
one (`Index::graft`), so a long scan of a slow volume never blocks queries;
home roots are done first. Reconciles `statx` under a read lock
(`scan::changed_dirs`) and take the write lock only to re-list changed
folders (`scan::apply_changed`). Roots added to or removed from the config
are scanned or dropped on the next start without rescanning the others.
