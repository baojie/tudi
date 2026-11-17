//! Metadata phase (PLAN §5.1): `statx` per entry on a worker pool, visible
//! rows first, results delivered in batches.
//!
//! `statx` uses `AT_SYMLINK_NOFOLLOW | AT_STATX_DONT_SYNC` relative to the
//! listing's open directory, so symlinks (including loops) are never followed
//! and network filesystems are not forced to sync.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rustix::fs::{AtFlags, StatxFlags};

use super::{Kind, Listing};

/// Whether an entry's metadata is known yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MetaStatus {
    #[default]
    Pending,
    Ok,
    /// `statx` failed (e.g. the entry vanished or is unreadable).
    Failed,
}

/// Metadata of one entry. `Default` is "pending".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub status: MetaStatus,
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime: i64,
    pub mtime_nsec: u32,
    /// `st_mode` (type and permission bits).
    pub mode: u32,
}

impl Meta {
    pub fn failed() -> Self {
        Self {
            status: MetaStatus::Failed,
            ..Self::default()
        }
    }

    /// Entry type from `mode`, or `None` unless the status is `Ok`.
    pub fn kind(&self) -> Option<Kind> {
        if self.status != MetaStatus::Ok {
            return None;
        }
        Some(match self.mode & 0o170000 {
            0o100000 => Kind::File,
            0o040000 => Kind::Dir,
            0o120000 => Kind::Symlink,
            _ => Kind::Other,
        })
    }
}

/// `statx` entry `i` of `listing`, without following symlinks.
pub fn stat_entry(listing: &Listing, i: usize) -> Meta {
    let mask = StatxFlags::TYPE | StatxFlags::MODE | StatxFlags::SIZE | StatxFlags::MTIME;
    match rustix::fs::statx(
        listing.dir(),
        listing.name_cstr(i),
        AtFlags::SYMLINK_NOFOLLOW | AtFlags::STATX_DONT_SYNC,
        mask,
    ) {
        Ok(st) => Meta {
            status: MetaStatus::Ok,
            size: st.stx_size,
            mtime: st.stx_mtime.tv_sec,
            mtime_nsec: st.stx_mtime.tv_nsec,
            mode: u32::from(st.stx_mode),
        },
        Err(_) => Meta::failed(),
    }
}

/// Metadata for every entry of one listing, filled in from batches.
#[derive(Clone, Debug, Default)]
pub struct MetaTable {
    metas: Vec<Meta>,
    /// Entries whose `getdents` type was unknown and has been resolved.
    resolved_unknown: usize,
}

impl MetaTable {
    pub fn new(len: usize) -> Self {
        Self {
            metas: vec![Meta::default(); len],
            resolved_unknown: 0,
        }
    }

    pub fn get(&self, i: usize) -> &Meta {
        &self.metas[i]
    }

    /// Apply a batch. Returns true if it resolved a `DT_UNKNOWN` type, in
    /// which case a "directories first" order may need re-sorting.
    pub fn apply(&mut self, listing: &Listing, batch: &MetaBatch) -> bool {
        let before = self.resolved_unknown;
        for &(i, meta) in &batch.items {
            let i = i as usize;
            if listing.kind(i) == Kind::Unknown
                && self.metas[i].status == MetaStatus::Pending
                && meta.kind().is_some()
            {
                self.resolved_unknown += 1;
            }
            self.metas[i] = meta;
        }
        self.resolved_unknown != before
    }

    /// Entry type: from `getdents`, or from `statx` if that was unknown.
    pub fn kind(&self, listing: &Listing, i: usize) -> Kind {
        match listing.kind(i) {
            Kind::Unknown => self.metas[i].kind().unwrap_or(Kind::Unknown),
            k => k,
        }
    }
}

/// A batch of metadata results for one listing generation.
#[derive(Debug, Default)]
pub struct MetaBatch {
    pub generation: u64,
    /// `(entry index, metadata)`.
    pub items: Vec<(u32, Meta)>,
    /// Set on the last batch when every entry has been fetched (not sent
    /// after a cancel).
    pub complete: bool,
}

/// Receives batches on a worker thread; typically forwards to a channel.
pub type Sink = Arc<dyn Fn(MetaBatch) + Send + Sync>;

/// Results are flushed every this many entries.
const BATCH: usize = 256;

struct Shared {
    listing: Arc<Listing>,
    /// One bit per entry: claimed by a worker.
    claimed: Vec<AtomicU64>,
    /// Priority entries (visible rows), popped from the end.
    priority: Mutex<Vec<u32>>,
    /// Next entry for the background sweep.
    cursor: AtomicUsize,
    cancelled: AtomicBool,
    active: AtomicUsize,
    sink: Sink,
}

impl Shared {
    fn claim(&self, i: usize) -> bool {
        let bit = 1u64 << (i % 64);
        self.claimed[i / 64].fetch_or(bit, Ordering::AcqRel) & bit == 0
    }

    /// Next entry to fetch, and whether it came from the priority queue.
    fn next(&self) -> Option<(usize, bool)> {
        {
            let mut q = self.priority.lock().unwrap();
            while let Some(i) = q.pop() {
                if self.claim(i as usize) {
                    return Some((i as usize, true));
                }
            }
        }
        loop {
            let i = self.cursor.fetch_add(1, Ordering::Relaxed);
            if i >= self.listing.len() {
                return None;
            }
            if self.claim(i) {
                return Some((i, false));
            }
        }
    }

    fn priority_empty(&self) -> bool {
        self.priority.lock().unwrap().is_empty()
    }

    fn flush(&self, items: &mut Vec<(u32, Meta)>) {
        if !items.is_empty() {
            (self.sink)(MetaBatch {
                generation: self.listing.generation,
                items: std::mem::take(items),
                complete: false,
            });
        }
    }

    fn work(&self) {
        let mut items = Vec::with_capacity(BATCH);
        while !self.cancelled.load(Ordering::Relaxed) {
            let Some((i, prio)) = self.next() else { break };
            items.push((i as u32, stat_entry(&self.listing, i)));
            // Visible rows go out as soon as the priority queue drains.
            if items.len() >= BATCH || (prio && self.priority_empty()) {
                self.flush(&mut items);
            }
        }
        if self.cancelled.load(Ordering::Relaxed) {
            return;
        }
        self.flush(&mut items);
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            (self.sink)(MetaBatch {
                generation: self.listing.generation,
                items: Vec::new(),
                complete: true,
            });
        }
    }
}

fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("tudi-meta-{i}"))
            .build()
            .expect("metadata thread pool")
    })
}

/// Fetches metadata for one listing in the background. Dropping it cancels
/// the remaining work.
pub struct MetaFetcher {
    shared: Arc<Shared>,
}

impl MetaFetcher {
    /// Start fetching with `workers` threads (capped by the pool size).
    /// `priority` entries (e.g. the initially visible rows) go first, in
    /// order.
    pub fn start(listing: Arc<Listing>, priority: &[u32], workers: usize, sink: Sink) -> Self {
        let n = listing.len();
        let workers = workers.max(1).min(n.max(1));
        let shared = Arc::new(Shared {
            claimed: (0..n.div_ceil(64)).map(|_| AtomicU64::new(0)).collect(),
            priority: Mutex::new(priority.iter().rev().copied().collect()),
            cursor: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            active: AtomicUsize::new(workers),
            listing,
            sink,
        });
        for _ in 0..workers {
            let s = shared.clone();
            pool().spawn(move || s.work());
        }
        Self { shared }
    }

    /// Replace the priority queue, e.g. when the visible rows change.
    pub fn prioritize(&self, entries: &[u32]) {
        let mut q = self.shared.priority.lock().unwrap();
        q.clear();
        q.extend(entries.iter().rev().copied());
    }

    /// Stop; batches already sent may still arrive and are not `complete`.
    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Drop for MetaFetcher {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::super::{Kind, Lister, Listing};
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::mpsc;
    use std::time::Duration;

    fn collect(rx: &mpsc::Receiver<MetaBatch>) -> Vec<MetaBatch> {
        let mut out = Vec::new();
        loop {
            let b = rx.recv_timeout(Duration::from_secs(10)).expect("batch");
            let done = b.complete;
            out.push(b);
            if done {
                return out;
            }
        }
    }

    fn channel_sink() -> (Sink, mpsc::Receiver<MetaBatch>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        (Arc::new(move |b| tx.lock().unwrap().send(b).unwrap()), rx)
    }

    #[test]
    fn stat_reports_size_mode_and_does_not_follow_symlinks() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), b"f", 1234);
        symlink("loop", t.path().join("loop")).unwrap();
        std::fs::create_dir(t.path().join("d")).unwrap();
        let l = Lister::new().list(t.path(), 0).unwrap();
        for i in 0..l.len() {
            let m = stat_entry(&l, i);
            assert_eq!(m.status, MetaStatus::Ok);
            match l.name(i) {
                b"f" => {
                    assert_eq!(m.size, 1234);
                    assert_eq!(m.kind(), Some(Kind::File));
                    assert!(m.mtime > 1_600_000_000);
                }
                b"loop" => assert_eq!(m.kind(), Some(Kind::Symlink)),
                b"d" => assert_eq!(m.kind(), Some(Kind::Dir)),
                n => panic!("unexpected {n:?}"),
            }
        }
    }

    #[test]
    fn vanished_entry_is_failed() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), b"gone", 0);
        let l = Lister::new().list(t.path(), 0).unwrap();
        std::fs::remove_file(t.path().join("gone")).unwrap();
        assert_eq!(stat_entry(&l, 0).status, MetaStatus::Failed);
    }

    #[test]
    fn dt_unknown_is_resolved_by_metadata() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), b"file", 0);
        std::fs::create_dir(t.path().join("dir")).unwrap();
        let fd = Lister::new().list(t.path(), 0).unwrap().dir;
        let l = Listing::from_parts(fd, &[(b"file", Kind::Unknown), (b"dir", Kind::Unknown)]);
        let mut table = MetaTable::new(l.len());
        assert_eq!(table.kind(&l, 1), Kind::Unknown);
        let batch = MetaBatch {
            items: (0..2).map(|i| (i as u32, stat_entry(&l, i))).collect(),
            ..Default::default()
        };
        assert!(table.apply(&l, &batch));
        assert_eq!(table.kind(&l, 0), Kind::File);
        assert_eq!(table.kind(&l, 1), Kind::Dir);
        // Re-applying resolves nothing new.
        assert!(!table.apply(&l, &batch));
    }

    #[test]
    fn fetcher_covers_every_entry_once_and_completes() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..1000 {
            touch(t.path(), format!("f{i}").as_bytes(), i);
        }
        let l = Arc::new(Lister::new().list(t.path(), 42).unwrap());
        let (sink, rx) = channel_sink();
        let _f = MetaFetcher::start(l.clone(), &[], 4, sink);
        let batches = collect(&rx);
        let mut seen = vec![0u32; l.len()];
        for b in &batches {
            assert_eq!(b.generation, 42);
            for &(i, m) in &b.items {
                seen[i as usize] += 1;
                let want: usize = std::str::from_utf8(&l.name(i as usize)[1..])
                    .unwrap()
                    .parse()
                    .unwrap();
                assert_eq!(m.size, want as u64);
            }
        }
        assert!(seen.iter().all(|&n| n == 1));
        assert_eq!(batches.iter().filter(|b| b.complete).count(), 1);
    }

    #[test]
    fn priority_entries_come_first() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..2000 {
            touch(t.path(), format!("f{i}").as_bytes(), 0);
        }
        let l = Arc::new(Lister::new().list(t.path(), 0).unwrap());
        let prio: Vec<u32> = (1990..2000).collect();
        let (sink, rx) = channel_sink();
        let _f = MetaFetcher::start(l, &prio, 1, sink);
        let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let got: Vec<u32> = first.items.iter().map(|&(i, _)| i).collect();
        assert_eq!(got, prio, "visible rows are flushed alone and first");
        collect(&rx);
    }

    #[test]
    fn cancel_stops_without_complete() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..3000 {
            touch(t.path(), format!("f{i}").as_bytes(), 0);
        }
        let l = Arc::new(Lister::new().list(t.path(), 0).unwrap());
        let (sink, rx) = channel_sink();
        let f = MetaFetcher::start(l, &[], 1, sink);
        f.cancel();
        drop(f);
        let mut n = 0;
        while let Ok(b) = rx.recv_timeout(Duration::from_millis(500)) {
            assert!(!b.complete);
            n += b.items.len();
        }
        assert!(n < 3000);
    }
}
