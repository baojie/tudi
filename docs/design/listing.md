# Directory listing (M2)

Code: `crates/tudi-core/src/listing/`. Plan: `docs/PLAN.md` §5.1.

## Pipeline

1. **Read** (`Lister::list`). `open(O_DIRECTORY)` then `getdents64` through
   `rustix::fs::RawDir` with a reusable 256 KiB buffer (one `Lister` per
   worker thread). No `stat`. `.` and `..` are skipped. Result: a `Listing`
   holding the open directory fd, a name arena (`Vec<u8>`, every name
   NUL-terminated so it can be passed to `*at` syscalls without copying) and
   per entry `(offset, len, kind)`. `kind` comes from `d_type` and may be
   `Unknown`.
2. **Name rank** (`sort::NameRank::new`). Each name gets a byte key
   (`natural::key`) whose bytewise order is the natural order; entries are
   sorted by an 8-byte big-endian key prefix, then the full key, then the
   raw name. The result is a rank per entry and the inverse (indices in name
   order). Parallel (rayon) from 8k entries.
3. **Display order** (`sort::order`). By name: walk the name order (or its
   reverse) and split off directories: O(n). By size or modified time: one
   packed `u128` key per entry, `group | missing | primary | name rank`,
   sorted as integers; the unique name rank identifies the entry afterwards.
4. **Metadata** (`meta::MetaFetcher`). `statx(dirfd, name,
   AT_SYMLINK_NOFOLLOW | AT_STATX_DONT_SYNC, TYPE|MODE|SIZE|MTIME)` on a
   dedicated rayon pool (`tudi-meta-N`, at most 8 threads). Each entry is
   claimed once through an atomic bitset. Workers take entries from the
   priority queue (the visible rows; replaced by `prioritize`) before the
   background sweep, and flush a batch as soon as the priority queue drains,
   otherwise every 256 entries. The last worker to finish sends an empty
   batch with `complete = true`; after `cancel` (or drop) no `complete`
   batch is sent.

## Natural order

`natural::compare` is the definition; `natural::key` is proven equivalent by
a property test. Rules: one leading `.` ignored; digit runs by numeric value
(any length); ASCII case-insensitive; non-ASCII bytes raw (code-point order
for UTF-8, so CJK sorts after Latin in code-point order; pinyin collation is
M8); ties broken by raw bytes, so the order is total.

Known v0 limit: case folding is ASCII only (`É` and `é` are not folded).

## Generations and staleness

`Listing::generation` is chosen by the caller and copied into every
`MetaBatch`. The GUI drops batches whose generation is not the current one
and cancels the old fetcher (dropping a `MetaFetcher` cancels it).

## DT_UNKNOWN and symlinks

Entries of unknown type sort as non-directories until metadata resolves
them. `MetaTable::apply` returns `true` when a batch resolved an unknown
type; the caller then recomputes the order (directories first may change).
Symlinks are never followed when listing or `stat`ing; they sort as files.
Whether a symlink points to a directory is not resolved in M2.

## Errors

`Lister::list` returns `ListError { path, source: io::Error }`; callers use
`kind()` (e.g. `PermissionDenied`). An entry whose `statx` fails (vanished,
unreadable) gets `MetaStatus::Failed` and sorts with the pending entries.
