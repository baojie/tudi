//! Sorting a listing (PLAN §5.1), always on a worker thread.
//!
//! The expensive natural name comparison runs once per listing and produces
//! a name rank per entry ([`NameRank`]). Every order the details list offers
//! (name / size / modified, ascending or descending, directories first or
//! not) is then a single `sort_unstable_by_key` on entry indices with a
//! compact integer key, with the name rank as the tiebreak.

use rayon::slice::ParallelSliceMut;

use super::meta::{MetaStatus, MetaTable};
use super::{Kind, Listing, natural};

/// Below this many entries a parallel sort is not worth its overhead.
const PAR_THRESHOLD: usize = 8 * 1024;

/// The position of each entry in natural name order, and its inverse.
#[derive(Clone, Debug)]
pub struct NameRank {
    rank: Vec<u32>,
    /// Entry indices in name order.
    sorted: Vec<u32>,
}

fn prefix(key: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    let n = key.len().min(8);
    b[..n].copy_from_slice(&key[..n]);
    u64::from_be_bytes(b)
}

impl NameRank {
    pub fn new(listing: &Listing) -> Self {
        Self::with_pinyin(listing, false)
    }

    /// With `pinyin`, Chinese names sort by their pinyin among Latin ones.
    pub fn with_pinyin(listing: &Listing, pinyin: bool) -> Self {
        let n = listing.len();
        let mut scratch = Vec::new();
        // Natural-order byte keys in one arena; sort by an 8-byte prefix
        // first, falling back to the full key and then the raw name.
        let mut arena = Vec::with_capacity(n * 16);
        let mut spans = Vec::with_capacity(n);
        for i in 0..n {
            let start = arena.len();
            if pinyin {
                natural::pinyin_key(listing.name(i), &mut arena, &mut scratch);
            } else {
                natural::key(listing.name(i), &mut arena);
            }
            spans.push((start, arena.len()));
        }
        let key = |i: u32| &arena[spans[i as usize].0..spans[i as usize].1];
        let mut items: Vec<(u64, u32)> = (0..n as u32).map(|i| (prefix(key(i)), i)).collect();
        let cmp = |a: &(u64, u32), b: &(u64, u32)| {
            a.0.cmp(&b.0)
                .then_with(|| key(a.1).cmp(key(b.1)))
                .then_with(|| listing.name(a.1 as usize).cmp(listing.name(b.1 as usize)))
        };
        if n >= PAR_THRESHOLD {
            items.par_sort_unstable_by(cmp);
        } else {
            items.sort_unstable_by(cmp);
        }
        let sorted: Vec<u32> = items.into_iter().map(|(_, i)| i).collect();
        let mut rank = vec![0u32; n];
        for (r, &i) in sorted.iter().enumerate() {
            rank[i as usize] = r as u32;
        }
        Self { rank, sorted }
    }

    pub fn get(&self, i: usize) -> u32 {
        self.rank[i]
    }

    /// Entry indices in name order.
    pub fn sorted(&self) -> &[u32] {
        &self.sorted
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortSpec {
    pub key: SortKey,
    pub descending: bool,
    pub dirs_first: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            descending: false,
            dirs_first: true,
        }
    }
}

/// Map an `i64` to a `u64` preserving order.
fn ordered(v: i64) -> u64 {
    (v as u64) ^ (1 << 63)
}

/// Entry indices in display order.
///
/// Entries whose metadata is still pending (or failed) sort after the others
/// in their group when sorting by size or time; they move into place when
/// the order is recomputed after metadata arrives. Symlinks sort as files.
/// Directories have no size, so by size they keep name order.
pub fn order(listing: &Listing, rank: &NameRank, metas: &MetaTable, spec: SortSpec) -> Vec<u32> {
    let is_dir = |i: u32| metas.kind(listing, i as usize) == Kind::Dir;
    if spec.key == SortKey::Name {
        // Already sorted by name: only split off the directories. O(n).
        let names: Box<dyn Iterator<Item = &u32>> = if spec.descending {
            Box::new(rank.sorted().iter().rev())
        } else {
            Box::new(rank.sorted().iter())
        };
        if !spec.dirs_first {
            return names.copied().collect();
        }
        let (mut dirs, rest): (Vec<u32>, Vec<u32>) = names.partition(|&&i| is_dir(i));
        dirs.extend(rest);
        return dirs;
    }
    // One packed integer key per entry; the name rank is unique, so it both
    // breaks ties and identifies the entry after sorting.
    // Layout: group (1 bit) | missing (1 bit) | primary (64) | name rank (32).
    let mut keys: Vec<u128> = (0..listing.len() as u32)
        .map(|i| {
            let dir = is_dir(i);
            let group = u128::from(spec.dirs_first && !dir);
            let m = metas.get(i as usize);
            let (missing, primary) = if dir && spec.key == SortKey::Size {
                (0, 0)
            } else if m.status != MetaStatus::Ok {
                (1, 0)
            } else if spec.key == SortKey::Size {
                (0, m.size)
            } else {
                (0, ordered(m.mtime))
            };
            let primary = if spec.descending { !primary } else { primary };
            group << 97
                | missing << 96
                | u128::from(primary) << 32
                | u128::from(rank.get(i as usize))
        })
        .collect();
    if keys.len() >= PAR_THRESHOLD {
        keys.par_sort_unstable();
    } else {
        keys.sort_unstable();
    }
    keys.into_iter()
        .map(|k| rank.sorted()[(k as u32) as usize])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::meta::{MetaBatch, stat_entry};
    use super::super::testutil::*;
    use super::super::{Kind, Lister, Listing};
    use super::*;
    use std::fs;

    fn names(l: &Listing, order: &[u32]) -> Vec<String> {
        order
            .iter()
            .map(|&i| String::from_utf8_lossy(l.name(i as usize)).into_owned())
            .collect()
    }

    fn full_meta(l: &Listing) -> MetaTable {
        let mut t = MetaTable::new(l.len());
        let batch = MetaBatch {
            items: (0..l.len()).map(|i| (i as u32, stat_entry(l, i))).collect(),
            ..Default::default()
        };
        t.apply(l, &batch);
        t
    }

    fn fixture() -> (tempfile::TempDir, Listing) {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), b"file10", 10);
        touch(t.path(), b"file2", 300);
        touch(t.path(), b"File1", 20);
        fs::create_dir(t.path().join("zdir")).unwrap();
        fs::create_dir(t.path().join("adir")).unwrap();
        for (name, secs) in [("file10", 3), ("file2", 1), ("File1", 2)] {
            let f = fs::File::options()
                .write(true)
                .open(t.path().join(name))
                .unwrap();
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs * 1000))
                .unwrap();
        }
        let l = Lister::new().list(t.path(), 0).unwrap();
        (t, l)
    }

    #[test]
    fn name_dirs_first() {
        let (_t, l) = fixture();
        let rank = NameRank::new(&l);
        let o = order(&l, &rank, &MetaTable::new(l.len()), SortSpec::default());
        assert_eq!(names(&l, &o), ["adir", "zdir", "File1", "file2", "file10"]);
    }

    #[test]
    fn name_descending_mixed() {
        let (_t, l) = fixture();
        let rank = NameRank::new(&l);
        let spec = SortSpec {
            descending: true,
            dirs_first: false,
            ..Default::default()
        };
        let o = order(&l, &rank, &MetaTable::new(l.len()), spec);
        assert_eq!(names(&l, &o), ["zdir", "file10", "file2", "File1", "adir"]);
    }

    #[test]
    fn by_size_and_time() {
        let (_t, l) = fixture();
        let rank = NameRank::new(&l);
        let metas = full_meta(&l);
        let spec = SortSpec {
            key: SortKey::Size,
            ..Default::default()
        };
        let o = order(&l, &rank, &metas, spec);
        assert_eq!(names(&l, &o), ["adir", "zdir", "file10", "File1", "file2"]);
        let spec = SortSpec {
            key: SortKey::Modified,
            descending: true,
            ..Default::default()
        };
        let o = order(&l, &rank, &metas, spec);
        assert_eq!(&names(&l, &o)[2..], ["file10", "File1", "file2"]);
    }

    #[test]
    fn pending_metadata_sorts_last_in_group() {
        let (_t, l) = fixture();
        let rank = NameRank::new(&l);
        let mut metas = MetaTable::new(l.len());
        let i2 = (0..l.len()).find(|&i| l.name(i) == b"file2").unwrap();
        metas.apply(
            &l,
            &MetaBatch {
                items: vec![(i2 as u32, stat_entry(&l, i2))],
                ..Default::default()
            },
        );
        let spec = SortSpec {
            key: SortKey::Size,
            ..Default::default()
        };
        let o = order(&l, &rank, &metas, spec);
        assert_eq!(names(&l, &o)[2], "file2");
    }

    #[test]
    fn unknown_kind_joins_dirs_once_resolved() {
        let (_t, l) = fixture();
        let l = Listing::from_parts(l.dir, &[(b"aaa", Kind::Unknown), (b"zdir", Kind::Unknown)]);
        let rank = NameRank::new(&l);
        let mut metas = MetaTable::new(l.len());
        // Unresolved: both sort as non-directories, by name.
        let o = order(&l, &rank, &metas, SortSpec::default());
        assert_eq!(names(&l, &o), ["aaa", "zdir"]);
        let batch = MetaBatch {
            items: (0..2).map(|i| (i as u32, stat_entry(&l, i))).collect(),
            ..Default::default()
        };
        assert!(metas.apply(&l, &batch), "zdir resolved; caller re-sorts");
        let o = order(&l, &rank, &metas, SortSpec::default());
        assert_eq!(names(&l, &o), ["zdir", "aaa"]);
    }

    #[test]
    fn ordered_preserves_i64_order() {
        let v = [i64::MIN, -5, -1, 0, 1, 7, i64::MAX];
        assert!(v.windows(2).all(|w| ordered(w[0]) < ordered(w[1])));
    }

    /// Naive reference: sort with a comparator over tuples.
    fn naive(l: &Listing, metas: &MetaTable, spec: SortSpec) -> Vec<u32> {
        use crate::listing::meta::MetaStatus;
        use std::cmp::Ordering;
        let mut idx: Vec<u32> = (0..l.len() as u32).collect();
        idx.sort_by(|&a, &b| {
            let (a, b) = (a as usize, b as usize);
            let dir = |i| metas.kind(l, i) == Kind::Dir;
            let group = |i| spec.dirs_first && !dir(i);
            let name = super::natural::compare(l.name(a), l.name(b));
            let flip = |o: Ordering| if spec.descending { o.reverse() } else { o };
            group(a).cmp(&group(b)).then_with(|| match spec.key {
                SortKey::Name => flip(name),
                _ => {
                    let missing = |i| {
                        !(spec.key == SortKey::Size && dir(i))
                            && metas.get(i).status != MetaStatus::Ok
                    };
                    let val = |i: usize| {
                        let m = metas.get(i);
                        match spec.key {
                            SortKey::Size if dir(i) => 0i128,
                            SortKey::Size => i128::from(m.size),
                            _ => i128::from(m.mtime),
                        }
                    };
                    missing(a)
                        .cmp(&missing(b))
                        .then_with(|| {
                            if missing(a) {
                                Ordering::Equal
                            } else {
                                flip(val(a).cmp(&val(b)))
                            }
                        })
                        .then(name)
                }
            })
        });
        idx
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(128))]
        #[test]
        fn order_matches_naive_sort(
            names in proptest::collection::vec("[aB0-9.]{1,5}", 1..40),
            dirs in proptest::collection::vec(proptest::bool::ANY, 40),
            metas in proptest::collection::vec(proptest::option::of((0u64..5, -3i64..3)), 40),
            key in 0u8..3, descending in proptest::bool::ANY, dirs_first in proptest::bool::ANY,
        ) {
            use crate::listing::meta::{Meta, MetaStatus};
            let mut names = names;
            names.sort();
            names.dedup();
            names.retain(|n| n != "." && n != "..");
            let t = tempfile::tempdir().unwrap();
            let entries: Vec<(&[u8], Kind)> = names
                .iter()
                .enumerate()
                .map(|(i, n)| (n.as_bytes(), if dirs[i] { Kind::Dir } else { Kind::File }))
                .collect();
            let fd = Lister::new().list(t.path(), 0).unwrap().dir;
            let l = Listing::from_parts(fd, &entries);
            let mut table = MetaTable::new(l.len());
            let items = (0..l.len())
                .filter_map(|i| metas[i].map(|(size, mtime)| (i as u32, Meta {
                    status: MetaStatus::Ok, size, mtime, mtime_nsec: 0,
                    mode: if dirs[i] { 0o040755 } else { 0o100644 },
                })))
                .collect();
            table.apply(&l, &MetaBatch { items, ..Default::default() });
            let spec = SortSpec {
                key: [SortKey::Name, SortKey::Size, SortKey::Modified][key as usize],
                descending,
                dirs_first,
            };
            let rank = NameRank::new(&l);
            proptest::prop_assert_eq!(order(&l, &rank, &table, spec), naive(&l, &table, spec));
        }
    }
}
