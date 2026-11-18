//! Scanning folders into the index (PLAN §5.4, initial scan and reconcile).
//!
//! A level-by-level breadth-first walk: each level's folders are listed in
//! parallel (rayon), then applied to the index in order, which keeps nodes
//! in parent-before-child order. Listing uses `getdents64` plus one `statx`
//! per entry (size, mtime, mount id), never following symlinks.
//!
//! Not descended into: folders matching the excludes (they are indexed
//! themselves, so `node_modules` can still be found) and folders on another
//! filesystem than their root (network and FUSE mounts inside `$HOME`).

use std::ffi::OsStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use rustix::fs::{AtFlags, Mode, OFlags, RawDir, StatxFlags};

use crate::{Entry, Index, Kind, flags};

/// Default excluded folders (PLAN §5.4).
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".cache",
    ".local/share/Trash",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    ".git/objects",
    "snap",
    ".npm",
    ".cargo/registry",
];

/// Folder exclusion rules. A pattern starting with `/` is an absolute glob;
/// otherwise it matches that relative path anywhere (`node_modules`,
/// `.git/objects`).
#[derive(Clone, Debug)]
pub struct Excludes(GlobSet);

impl Excludes {
    pub fn new(patterns: &[String]) -> Result<Self, globset::Error> {
        let mut b = GlobSetBuilder::new();
        for p in patterns {
            let g = if p.starts_with('/') {
                p.clone()
            } else {
                format!("**/{}", p.trim_end_matches('/'))
            };
            b.add(Glob::new(&g)?);
        }
        Ok(Self(b.build()?))
    }

    pub fn defaults() -> Self {
        let v: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        Self::new(&v).expect("default excludes are valid")
    }

    pub fn matches(&self, path: &Path) -> bool {
        self.0.is_match(path)
    }
}

/// A listed entry plus whether it is a folder on another filesystem.
pub struct Listed {
    pub entry: Entry,
    pub other_fs: bool,
}

/// List `path` (entries with size, mtime), noting folders whose mount
/// differs from `mnt`.
pub fn list_dir(
    path: &Path,
    mnt: Option<u64>,
    buf: &mut [MaybeUninit<u8>],
) -> io::Result<Vec<Listed>> {
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let mut names: Vec<std::ffi::CString> = Vec::new();
    let mut raw = RawDir::new(&fd, buf);
    while let Some(e) = raw.next() {
        let e = e?;
        let n = e.file_name();
        if n.to_bytes() != b"." && n.to_bytes() != b".." {
            names.push(n.to_owned());
        }
    }
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let Ok(st) = rustix::fs::statx(
            &fd,
            name.as_c_str(),
            AtFlags::SYMLINK_NOFOLLOW | AtFlags::STATX_DONT_SYNC,
            StatxFlags::TYPE | StatxFlags::SIZE | StatxFlags::MTIME | StatxFlags::MNT_ID,
        ) else {
            continue; // vanished meanwhile
        };
        let kind = match u32::from(st.stx_mode) & 0o170000 {
            0o040000 => Kind::Dir,
            0o100000 => Kind::File,
            0o120000 => Kind::Symlink,
            _ => Kind::Other,
        };
        out.push(Listed {
            other_fs: kind == Kind::Dir && mnt.is_some_and(|m| m != st.stx_mnt_id),
            entry: Entry {
                name: name.into_bytes(),
                kind,
                size: st.stx_size,
                mtime: st.stx_mtime.tv_sec,
            },
        });
    }
    Ok(out)
}

/// Mount id of a folder.
pub fn mount_id(path: &Path) -> Option<u64> {
    rustix::fs::statx(
        rustix::fs::CWD,
        path,
        AtFlags::empty(),
        StatxFlags::MNT_ID | StatxFlags::MTIME,
    )
    .ok()
    .map(|s| s.stx_mnt_id)
}

/// Scan statistics.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScanStats {
    pub dirs: u64,
    pub entries: u64,
    pub errors: u64,
}

/// Walk below `start` (a folder node already in the index) and add or
/// update everything under it. `mnt`: the root's mount id. Calls
/// `progress` after each level. Returns `None` if cancelled.
pub fn scan_below(
    ix: &mut Index,
    start: u32,
    mnt: Option<u64>,
    excludes: &Excludes,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&ScanStats),
) -> Option<ScanStats> {
    let mut stats = ScanStats::default();
    let mut frontier: Vec<(u32, PathBuf)> = vec![(start, ix.path(start))];
    while !frontier.is_empty() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        // List this level in parallel, in bounded chunks (memory).
        let mut next = Vec::new();
        for chunk in frontier.chunks(4096) {
            let listed: Vec<(u32, &PathBuf, io::Result<Vec<Listed>>)> = chunk
                .par_iter()
                .map_init(
                    || vec![MaybeUninit::<u8>::uninit(); 64 * 1024],
                    |buf, (node, path)| (*node, path, list_dir(path, mnt, buf)),
                )
                .collect();
            for (node, path, res) in listed {
                stats.dirs += 1;
                let Ok(items) = res else {
                    stats.errors += 1;
                    continue;
                };
                stats.entries += items.len() as u64;
                let other: std::collections::HashSet<Vec<u8>> = items
                    .iter()
                    .filter(|l| l.other_fs)
                    .map(|l| l.entry.name.clone())
                    .collect();
                let entries: Vec<Entry> = items.into_iter().map(|l| l.entry).collect();
                let (new_dirs, _) = ix.apply_listing(node, &entries);
                // Existing folders are re-listed too (reconcile); new ones
                // are always descended unless excluded.
                let mut dirs: Vec<u32> = ix
                    .children(node)
                    .filter(|&c| ix.nodes[c as usize].is_dir())
                    .collect();
                dirs.sort_unstable();
                dirs.dedup();
                let _ = new_dirs;
                for c in dirs {
                    let child_path = path.join(OsStr::from_bytes(ix.name(c)));
                    let name = ix.name(c).to_vec();
                    if other.contains(&name) || excludes.matches(&child_path) {
                        ix.nodes[c as usize].flags |= flags::EXCLUDED;
                        continue;
                    }
                    ix.nodes[c as usize].flags &= !flags::EXCLUDED;
                    next.push((c, child_path));
                }
            }
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
        }
        progress(&stats);
        frontier = next;
    }
    Some(stats)
}

/// Build a fresh index of `roots`.
pub fn build(
    roots: &[PathBuf],
    excludes: &Excludes,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&ScanStats),
) -> Option<Index> {
    let mut ix = Index::new();
    for root in roots {
        let mnt = mount_id(root);
        let mtime = std::fs::metadata(root)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        let r = ix.add_root(root, mtime);
        scan_below(&mut ix, r, mnt, excludes, cancel, &mut progress)?;
    }
    Some(ix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scan_respects_excludes_and_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        fs::create_dir_all(r.join("proj/src")).unwrap();
        fs::create_dir_all(r.join("proj/node_modules/lib")).unwrap();
        fs::create_dir_all(r.join("proj/.git/objects/ab")).unwrap();
        fs::write(r.join("proj/src/main.rs"), "fn main(){}").unwrap();
        fs::write(r.join("proj/node_modules/lib/x.js"), "").unwrap();
        std::os::unix::fs::symlink(r, r.join("proj/loop")).unwrap();
        let none = AtomicBool::new(false);
        let ix = build(&[r.to_path_buf()], &Excludes::defaults(), &none, |_| {}).unwrap();
        assert!(ix.find(&r.join("proj/src/main.rs")).is_some());
        // Excluded folders are indexed, their contents are not.
        let nm = ix.find(&r.join("proj/node_modules")).unwrap();
        assert!(ix.nodes[nm as usize].flags & flags::EXCLUDED != 0);
        assert!(ix.find(&r.join("proj/node_modules/lib")).is_none());
        assert!(ix.find(&r.join("proj/.git/objects")).is_some());
        assert!(ix.find(&r.join("proj/.git/objects/ab")).is_none());
        // The symlink loop is an entry, not followed.
        let l = ix.find(&r.join("proj/loop")).unwrap();
        assert_eq!(ix.nodes[l as usize].kind(), Kind::Symlink);
        let size = ix.nodes[ix.find(&r.join("proj/src/main.rs")).unwrap() as usize].size;
        assert_eq!(size, 11);
    }

    #[test]
    fn rescan_reconciles_changes() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        fs::create_dir_all(r.join("a/b")).unwrap();
        fs::write(r.join("a/b/old.txt"), "").unwrap();
        let none = AtomicBool::new(false);
        let mut ix = build(&[r.to_path_buf()], &Excludes::defaults(), &none, |_| {}).unwrap();
        fs::remove_file(r.join("a/b/old.txt")).unwrap();
        fs::write(r.join("a/b/new.txt"), "x").unwrap();
        fs::create_dir(r.join("a/c")).unwrap();
        let root = ix.roots[0].1;
        scan_below(
            &mut ix,
            root,
            mount_id(r),
            &Excludes::defaults(),
            &none,
            |_| {},
        )
        .unwrap();
        assert!(ix.find(&r.join("a/b/old.txt")).is_none());
        assert!(ix.find(&r.join("a/b/new.txt")).is_some());
        assert!(ix.find(&r.join("a/c")).is_some());
    }

    #[test]
    fn cancel_stops() {
        let t = tempfile::tempdir().unwrap();
        let yes = AtomicBool::new(true);
        assert!(
            build(
                &[t.path().to_path_buf()],
                &Excludes::defaults(),
                &yes,
                |_| {}
            )
            .is_none()
        );
    }
}
