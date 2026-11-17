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

/// The position of each entry in natural name order.
#[derive(Clone, Debug)]
pub struct NameRank(Vec<u32>);

impl NameRank {
    pub fn new(listing: &Listing) -> Self {
        let mut idx: Vec<u32> = (0..listing.len() as u32).collect();
        let cmp = |a: &u32, b: &u32| {
            natural::compare(listing.name(*a as usize), listing.name(*b as usize))
        };
        if idx.len() >= PAR_THRESHOLD {
            idx.par_sort_unstable_by(cmp);
        } else {
            idx.sort_unstable_by(cmp);
        }
        let mut rank = vec![0u32; idx.len()];
        for (r, &i) in idx.iter().enumerate() {
            rank[i as usize] = r as u32;
        }
        Self(rank)
    }

    pub fn get(&self, i: usize) -> u32 {
        self.0[i]
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
    let key = |i: u32| -> (u8, u8, u64, u32) {
        let i = i as usize;
        let is_dir = metas.kind(listing, i) == Kind::Dir;
        let group = u8::from(spec.dirs_first && !is_dir);
        let name = rank.get(i);
        let m = metas.get(i);
        let (missing, primary) = match spec.key {
            SortKey::Name => (0, u64::from(name)),
            SortKey::Size if is_dir => (0, 0),
            SortKey::Size | SortKey::Modified if m.status != MetaStatus::Ok => (1, 0),
            SortKey::Size => (0, m.size),
            SortKey::Modified => (0, ordered(m.mtime)),
        };
        let primary = if spec.descending { !primary } else { primary };
        (group, missing, primary, name)
    };
    let mut idx: Vec<u32> = (0..listing.len() as u32).collect();
    if idx.len() >= PAR_THRESHOLD {
        idx.par_sort_unstable_by_key(|&i| key(i));
    } else {
        idx.sort_unstable_by_key(|&i| key(i));
    }
    idx
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
}
