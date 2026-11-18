//! The in-memory file index (PLAN §5.4): every file and folder below the
//! indexed roots, in a compact struct-of-arrays layout, with incremental
//! updates and snapshots.
//!
//! - `nodes`: 24-byte records in parent-before-child order (a child is
//!   always appended after its parent exists), so a single forward pass can
//!   propagate per-folder information to descendants (the matcher does).
//! - `names`: one arena of name bytes (`OsStr`, never forced to UTF-8).
//! - `first_child` / `next_sibling`: child lists, for updates by folder.
//! - Deleted entries become tombstones; [`Index::compact`] rewrites the
//!   arrays when they exceed 20 %.
//!
//! This crate does no GTK; scanning lives in [`scan`], snapshots in
//! [`snapshot`].

pub mod fsinfo;
pub mod matcher;
pub mod scan;
pub mod snapshot;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// No node (no parent / child / sibling).
pub const NONE: u32 = u32::MAX;

/// Seconds between the Unix epoch and 2000-01-01 (the `mtime` epoch).
pub const EPOCH_2000: i64 = 946_684_800;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    #[default]
    File = 0,
    Dir = 1,
    Symlink = 2,
    Other = 3,
}

impl Kind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Kind::Dir,
            2 => Kind::Symlink,
            3 => Kind::Other,
            _ => Kind::File,
        }
    }
}

/// Node flags.
pub mod flags {
    /// Deleted; skipped everywhere until compaction.
    pub const TOMBSTONE: u8 = 1;
    /// An excluded folder: indexed itself, its contents are not.
    pub const EXCLUDED: u8 = 2;
    /// Name starts with a dot.
    pub const HIDDEN: u8 = 4;
    /// Name contains CJK characters (has transliterations).
    pub const CJK: u8 = 8;
}

/// One file or folder. 24 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Node {
    pub parent: u32,
    pub name_off: u32,
    pub name_len: u16,
    pub kind: u8,
    pub flags: u8,
    /// Seconds since 2000-01-01 UTC (saturating).
    pub mtime: u32,
    /// Size in bytes (files only).
    pub size: u64,
}

const _: () = assert!(std::mem::size_of::<Node>() == 24);

impl Node {
    pub fn kind(&self) -> Kind {
        Kind::from_u8(self.kind)
    }

    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir as u8
    }

    pub fn is_dead(&self) -> bool {
        self.flags & flags::TOMBSTONE != 0
    }
}

/// Give freed heap memory back to the system. Building the index and its
/// search data allocates a lot of temporary memory on rayon threads; glibc
/// keeps it in per-thread arenas, so RSS stays high until trimmed.
pub fn release_free_memory() {
    // SAFETY: malloc_trim only walks glibc's own heap bookkeeping and has
    // no preconditions; it is safe to call at any time from any thread.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// `mtime` (Unix seconds) to the stored form.
pub fn pack_mtime(unix: i64) -> u32 {
    (unix - EPOCH_2000).clamp(0, i64::from(u32::MAX)) as u32
}

pub fn unpack_mtime(m: u32) -> i64 {
    i64::from(m) + EPOCH_2000
}

/// An entry of a folder listing, for building and updating.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u8>,
    pub kind: Kind,
    pub size: u64,
    /// Unix seconds.
    pub mtime: i64,
}

fn has_cjk(name: &[u8]) -> bool {
    // Quick reject: CJK needs multi-byte UTF-8.
    name.iter().any(|&b| b >= 0xe2)
        && std::str::from_utf8(name)
            .is_ok_and(|s| s.chars().any(|c| ('\u{3400}'..='\u{9fff}').contains(&c)))
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub nodes: Vec<Node>,
    pub names: Vec<u8>,
    pub first_child: Vec<u32>,
    pub next_sibling: Vec<u32>,
    /// Root folders: (absolute path, node).
    pub roots: Vec<(PathBuf, u32)>,
    tombstones: usize,
}

impl Index {
    pub fn new() -> Self {
        Self::default()
    }

    /// Live entries.
    pub fn len(&self) -> usize {
        self.nodes.len() - self.tombstones
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn tombstones(&self) -> usize {
        self.tombstones
    }

    pub(crate) fn set_tombstones(&mut self, n: usize) {
        self.tombstones = n;
    }

    pub fn name(&self, i: u32) -> &[u8] {
        let n = &self.nodes[i as usize];
        &self.names[n.name_off as usize..n.name_off as usize + usize::from(n.name_len)]
    }

    /// Append a node (its parent must exist). Returns its index.
    pub fn push(&mut self, parent: u32, e: &Entry, extra_flags: u8) -> u32 {
        let idx = self.nodes.len() as u32;
        let name_off = self.names.len() as u32;
        let len = e.name.len().min(u16::MAX as usize);
        self.names.extend_from_slice(&e.name[..len]);
        let mut f = extra_flags;
        if e.name.first() == Some(&b'.') {
            f |= flags::HIDDEN;
        }
        if has_cjk(&e.name) {
            f |= flags::CJK;
        }
        self.nodes.push(Node {
            parent,
            name_off,
            name_len: len as u16,
            kind: e.kind as u8,
            flags: f,
            mtime: pack_mtime(e.mtime),
            size: if e.kind == Kind::Dir { 0 } else { e.size },
        });
        self.first_child.push(NONE);
        self.next_sibling.push(NONE);
        if parent != NONE {
            let p = parent as usize;
            self.next_sibling[idx as usize] = self.first_child[p];
            self.first_child[p] = idx;
        }
        idx
    }

    /// Add a root folder (its name is the absolute path).
    pub fn add_root(&mut self, path: &Path, mtime: i64) -> u32 {
        let e = Entry {
            name: path.as_os_str().as_bytes().to_vec(),
            kind: Kind::Dir,
            size: 0,
            mtime,
        };
        let id = self.push(NONE, &e, 0);
        // A root's own name is a path, not a searchable name.
        self.nodes[id as usize].flags &= !(flags::HIDDEN | flags::CJK);
        self.roots.push((path.to_path_buf(), id));
        id
    }

    /// Live children of `dir`.
    pub fn children(&self, dir: u32) -> impl Iterator<Item = u32> + '_ {
        let mut c = self.first_child[dir as usize];
        std::iter::from_fn(move || {
            while c != NONE {
                let cur = c;
                c = self.next_sibling[cur as usize];
                if !self.nodes[cur as usize].is_dead() {
                    return Some(cur);
                }
            }
            None
        })
    }

    /// Absolute path of a node.
    pub fn path(&self, i: u32) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = i;
        while cur != NONE {
            parts.push(cur);
            cur = self.nodes[cur as usize].parent;
        }
        let mut p = PathBuf::new();
        for &n in parts.iter().rev() {
            p.push(OsStr::from_bytes(self.name(n)));
        }
        p
    }

    /// The folder node at `path`, if indexed.
    pub fn find(&self, path: &Path) -> Option<u32> {
        let &(ref root_path, root) = self
            .roots
            .iter()
            .filter(|(p, n)| path.starts_with(p) && !self.nodes[*n as usize].is_dead())
            .max_by_key(|(p, _)| p.as_os_str().len())?;
        let mut node = root;
        for comp in path.strip_prefix(root_path).ok()?.components() {
            let name = comp.as_os_str().as_bytes();
            node = self.children(node).find(|&c| self.name(c) == name)?;
        }
        Some(node)
    }

    /// Mark `i` and everything below it deleted.
    fn kill(&mut self, i: u32) {
        let mut stack = vec![i];
        while let Some(n) = stack.pop() {
            if self.nodes[n as usize].is_dead() {
                continue;
            }
            self.nodes[n as usize].flags |= flags::TOMBSTONE;
            self.tombstones += 1;
            stack.extend(self.children(n));
        }
    }

    /// Replace what is known about `dir`'s children with a fresh listing:
    /// vanished entries (and their subtrees) are deleted, new ones added,
    /// changed size / mtime / kind updated. Returns the new folders (to be
    /// listed in turn) and whether anything changed.
    pub fn apply_listing(&mut self, dir: u32, entries: &[Entry]) -> (Vec<u32>, bool) {
        use std::collections::HashMap;
        let existing: HashMap<Vec<u8>, u32> = self
            .children(dir)
            .map(|c| (self.name(c).to_vec(), c))
            .collect();
        let mut keep = std::collections::HashSet::new();
        let mut new_dirs = Vec::new();
        let mut changed = false;
        let excluded = self.nodes[dir as usize].flags & flags::EXCLUDED != 0;
        if excluded {
            return (new_dirs, false);
        }
        for e in entries {
            match existing.get(&e.name) {
                Some(&c) if self.nodes[c as usize].kind == e.kind as u8 => {
                    keep.insert(c);
                    let n = &mut self.nodes[c as usize];
                    let (m, s) = (
                        pack_mtime(e.mtime),
                        if e.kind == Kind::Dir { 0 } else { e.size },
                    );
                    if n.mtime != m || n.size != s {
                        n.mtime = m;
                        n.size = s;
                        changed = true;
                    }
                }
                _ => {
                    let c = self.push(dir, e, 0);
                    keep.insert(c);
                    if e.kind == Kind::Dir {
                        new_dirs.push(c);
                    }
                    changed = true;
                }
            }
        }
        for (_, c) in existing {
            if !keep.contains(&c) {
                self.kill(c);
                changed = true;
            }
        }
        (new_dirs, changed)
    }

    /// Remove a root and everything below it.
    pub fn remove_root(&mut self, root: u32) {
        self.kill(root);
        self.roots.retain(|(_, r)| *r != root);
    }

    /// Append the (single-root) index `other` as a new root of this one,
    /// e.g. after scanning a slow volume separately. Returns the new root.
    pub fn graft(&mut self, other: &Index) -> Option<u32> {
        let (path, oroot) = other.roots.first()?.clone();
        let mut remap = vec![NONE; other.nodes.len()];
        let r = self.add_root(&path, unpack_mtime(other.nodes[oroot as usize].mtime));
        remap[oroot as usize] = r;
        for (i, n) in other.nodes.iter().enumerate() {
            if n.is_dead() || i as u32 == oroot || n.parent == NONE {
                continue;
            }
            let parent = remap[n.parent as usize];
            if parent == NONE {
                continue;
            }
            let e = Entry {
                name: other.name(i as u32).to_vec(),
                kind: n.kind(),
                size: n.size,
                mtime: unpack_mtime(n.mtime),
            };
            let id = self.push(parent, &e, 0);
            self.nodes[id as usize].flags = n.flags;
            remap[i] = id;
        }
        Some(r)
    }

    /// Live direct children of a node.
    pub fn child_count(&self, node: u32) -> usize {
        self.children(node).count()
    }

    /// Whether compaction is due (> 20 % tombstones).
    pub fn needs_compaction(&self) -> bool {
        self.tombstones * 5 > self.nodes.len()
    }

    /// Rewrite without tombstones, keeping parent-before-child order.
    pub fn compact(&mut self) {
        let mut remap = vec![NONE; self.nodes.len()];
        let mut out = Index::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.is_dead() {
                continue;
            }
            let parent = if n.parent == NONE {
                NONE
            } else {
                remap[n.parent as usize]
            };
            if n.parent != NONE && parent == NONE {
                continue; // parent was deleted
            }
            let e = Entry {
                name: self.name(i as u32).to_vec(),
                kind: n.kind(),
                size: n.size,
                mtime: unpack_mtime(n.mtime),
            };
            let id = out.push(parent, &e, 0);
            out.nodes[id as usize].flags = n.flags;
            remap[i] = id;
        }
        out.roots = self
            .roots
            .iter()
            .filter(|(_, r)| remap[*r as usize] != NONE)
            .map(|(p, r)| (p.clone(), remap[*r as usize]))
            .collect();
        *self = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, kind: Kind) -> Entry {
        Entry {
            name: name.as_bytes().to_vec(),
            kind,
            size: 10,
            mtime: 1_800_000_000,
        }
    }

    fn sample() -> Index {
        let mut ix = Index::new();
        let r = ix.add_root(Path::new("/w"), 0);
        let (dirs, _) = ix.apply_listing(
            r,
            &[
                e("a", Kind::Dir),
                e("f.txt", Kind::File),
                e("合同.xlsx", Kind::File),
            ],
        );
        ix.apply_listing(dirs[0], &[e("b", Kind::Dir), e(".hidden", Kind::File)]);
        ix
    }

    #[test]
    fn build_paths_find_flags() {
        let ix = sample();
        assert_eq!(ix.len(), 6);
        let b = ix.find(Path::new("/w/a/b")).unwrap();
        assert_eq!(ix.path(b), Path::new("/w/a/b"));
        assert!(ix.find(Path::new("/w/nope")).is_none());
        let h = ix.find(Path::new("/w/a/.hidden")).unwrap();
        assert!(ix.nodes[h as usize].flags & flags::HIDDEN != 0);
        let c = ix.find(Path::new("/w/合同.xlsx")).unwrap();
        assert!(ix.nodes[c as usize].flags & flags::CJK != 0);
        // Parent before child.
        for (i, n) in ix.nodes.iter().enumerate() {
            assert!(n.parent == NONE || (n.parent as usize) < i);
        }
    }

    #[test]
    fn update_delete_and_compact() {
        let mut ix = sample();
        let r = ix.roots[0].1;
        // "a" (with its subtree) vanished, f.txt changed, g added.
        let mut f = e("f.txt", Kind::File);
        f.size = 99;
        let (_, changed) = ix.apply_listing(r, &[f, e("合同.xlsx", Kind::File), e("g", Kind::Dir)]);
        assert!(changed);
        assert!(ix.find(Path::new("/w/a")).is_none());
        assert!(ix.find(Path::new("/w/a/b")).is_none());
        assert_eq!(ix.tombstones(), 3);
        assert_eq!(ix.len(), 4);
        let fi = ix.find(Path::new("/w/f.txt")).unwrap();
        assert_eq!(ix.nodes[fi as usize].size, 99);
        // Same listing again: no change.
        let (_, changed) = ix.apply_listing(
            r,
            &[
                {
                    let mut f = e("f.txt", Kind::File);
                    f.size = 99;
                    f
                },
                e("合同.xlsx", Kind::File),
                e("g", Kind::Dir),
            ],
        );
        assert!(!changed);
        assert!(ix.needs_compaction());
        ix.compact();
        assert_eq!(ix.tombstones(), 0);
        assert_eq!(ix.nodes.len(), 4);
        assert!(ix.find(Path::new("/w/g")).is_some());
        assert_eq!(
            ix.nodes[ix.find(Path::new("/w/f.txt")).unwrap() as usize].size,
            99
        );
    }

    #[test]
    fn graft_and_remove_root() {
        let mut ix = sample();
        let mut card = Index::new();
        let r = card.add_root(Path::new("/media/card"), 0);
        let (d, _) = card.apply_listing(r, &[e("DCIM", Kind::Dir)]);
        card.apply_listing(d[0], &[e("img1.jpg", Kind::File)]);
        let root = ix.graft(&card).unwrap();
        assert!(ix.find(Path::new("/media/card/DCIM/img1.jpg")).is_some());
        assert!(
            ix.find(Path::new("/w/a/b")).is_some(),
            "existing roots kept"
        );
        for (i, n) in ix.nodes.iter().enumerate() {
            assert!(n.parent == NONE || (n.parent as usize) < i);
        }
        ix.remove_root(root);
        assert!(ix.find(Path::new("/media/card/DCIM")).is_none());
        assert_eq!(ix.roots.len(), 1);
    }

    #[test]
    fn excluded_folders_keep_no_children() {
        let mut ix = Index::new();
        let r = ix.add_root(Path::new("/w"), 0);
        let (d, _) = ix.apply_listing(r, &[e("node_modules", Kind::Dir)]);
        ix.nodes[d[0] as usize].flags |= flags::EXCLUDED;
        let (new, changed) = ix.apply_listing(d[0], &[e("x", Kind::File)]);
        assert!(new.is_empty() && !changed);
        assert_eq!(ix.len(), 2);
    }
}
