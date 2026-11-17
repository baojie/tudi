//! Live updates for open directories (PLAN §5.1): inotify watches on the
//! folders the GUI shows, coalesced per folder.
//!
//! One thread per [`Watcher`] polls the inotify fd and an eventfd that wakes
//! it for commands. Callers register directories under their own keys; when
//! something changes in a directory, every key watching it is reported
//! once the burst settles ([`DELAY`]) and at most every [`MIN_INTERVAL`].
//! The number of keys is bounded; the least recently (re)watched key is
//! evicted beyond the limit.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use rustix::event::{EventfdFlags, PollFd, PollFlags};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

/// Wait this long after the first event of a burst before reporting.
pub const DELAY: Duration = Duration::from_millis(50);
/// Report the same key at most this often.
pub const MIN_INTERVAL: Duration = Duration::from_millis(150);

enum Cmd<K> {
    Watch {
        key: K,
        path: PathBuf,
        contents: bool,
    },
    Unwatch(K),
    Stop,
}

pub struct Watcher<K: Send + 'static> {
    tx: mpsc::Sender<Cmd<K>>,
    wake: Arc<OwnedFd>,
}

impl<K: Copy + Eq + Hash + Send + 'static> Watcher<K> {
    /// Start the thread. `on_change` runs on it with the keys whose
    /// directories changed (or all keys after an event-queue overflow).
    pub fn start(limit: usize, on_change: impl Fn(Vec<K>) + Send + 'static) -> io::Result<Self> {
        let ino = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
        let wake = Arc::new(rustix::event::eventfd(
            0,
            EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK,
        )?);
        let (tx, rx) = mpsc::channel();
        let w = wake.clone();
        std::thread::Builder::new()
            .name("tudi-watch".into())
            .spawn(move || {
                State::new(ino, limit).run(&w, &rx, &on_change);
            })?;
        Ok(Self { tx, wake })
    }

    /// Watch `path` under `key` (replacing what `key` watched before).
    /// With `contents`, file modifications count as changes; otherwise only
    /// entries being created, deleted or renamed do.
    pub fn watch(&self, key: K, path: PathBuf, contents: bool) {
        self.send(Cmd::Watch {
            key,
            path,
            contents,
        });
    }

    pub fn unwatch(&self, key: K) {
        self.send(Cmd::Unwatch(key));
    }

    fn send(&self, cmd: Cmd<K>) {
        if self.tx.send(cmd).is_ok() {
            let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
        }
    }
}

impl<K: Send + 'static> Drop for Watcher<K> {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
    }
}

struct Key {
    wd: i32,
    contents: bool,
}

struct State<K> {
    ino: OwnedFd,
    limit: usize,
    keys: HashMap<K, Key>,
    by_wd: HashMap<i32, Vec<K>>,
    /// Keys in watch order, oldest first (may hold stale entries).
    lru: VecDeque<K>,
    /// Keys with unreported changes, and when the first change arrived.
    pending: HashMap<K, Instant>,
    last_report: HashMap<K, Instant>,
}

const STRUCTURE: ReadFlags = ReadFlags::CREATE
    .union(ReadFlags::DELETE)
    .union(ReadFlags::MOVED_FROM)
    .union(ReadFlags::MOVED_TO)
    .union(ReadFlags::DELETE_SELF)
    .union(ReadFlags::MOVE_SELF);

impl<K: Copy + Eq + Hash> State<K> {
    fn new(ino: OwnedFd, limit: usize) -> Self {
        Self {
            ino,
            limit: limit.max(1),
            keys: HashMap::new(),
            by_wd: HashMap::new(),
            lru: VecDeque::new(),
            pending: HashMap::new(),
            last_report: HashMap::new(),
        }
    }

    fn run(mut self, wake: &OwnedFd, rx: &mpsc::Receiver<Cmd<K>>, on_change: &dyn Fn(Vec<K>)) {
        let mut buf = vec![MaybeUninit::<u8>::uninit(); 64 * 1024];
        loop {
            let timeout = self.next_deadline().map(|d| {
                let d = d.saturating_duration_since(Instant::now());
                rustix::time::Timespec {
                    tv_sec: d.as_secs() as i64,
                    tv_nsec: i64::from(d.subsec_nanos()),
                }
            });
            let mut fds = [
                PollFd::new(&self.ino, PollFlags::IN),
                PollFd::new(wake, PollFlags::IN),
            ];
            match rustix::event::poll(&mut fds, timeout.as_ref()) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(_) => return,
            }
            let (ino_ready, wake_ready) =
                (!fds[0].revents().is_empty(), !fds[1].revents().is_empty());
            if wake_ready {
                let mut b = [0u8; 8];
                let _ = rustix::io::read(wake, &mut b);
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        Cmd::Watch {
                            key,
                            path,
                            contents,
                        } => self.watch(key, path, contents),
                        Cmd::Unwatch(key) => self.unwatch(key),
                        Cmd::Stop => return,
                    }
                }
            }
            if ino_ready {
                self.read_events(&mut buf);
            }
            let due = self.take_due();
            if !due.is_empty() {
                on_change(due);
            }
        }
    }

    fn watch(&mut self, key: K, path: PathBuf, contents: bool) {
        self.unwatch(key);
        let mask = WatchFlags::CREATE
            | WatchFlags::DELETE
            | WatchFlags::MOVED_FROM
            | WatchFlags::MOVED_TO
            | WatchFlags::DELETE_SELF
            | WatchFlags::MOVE_SELF
            | WatchFlags::MODIFY
            | WatchFlags::ATTRIB
            | WatchFlags::CLOSE_WRITE
            | WatchFlags::ONLYDIR
            | WatchFlags::EXCL_UNLINK;
        // The same directory watched twice yields the same wd.
        let Ok(wd) = inotify::add_watch(&self.ino, &path, mask) else {
            return;
        };
        self.keys.insert(key, Key { wd, contents });
        self.by_wd.entry(wd).or_default().push(key);
        self.lru.push_back(key);
        while self.keys.len() > self.limit {
            let Some(old) = self.lru.pop_front() else {
                break;
            };
            self.unwatch(old);
        }
        // Drop stale LRU entries occasionally.
        if self.lru.len() > self.limit * 4 {
            let keys = &self.keys;
            let mut seen = std::collections::HashSet::new();
            let mut fresh: VecDeque<K> = self
                .lru
                .iter()
                .rev()
                .filter(|k| keys.contains_key(k) && seen.insert(**k))
                .copied()
                .collect();
            fresh.make_contiguous().reverse();
            self.lru = fresh;
        }
    }

    fn unwatch(&mut self, key: K) {
        let Some(k) = self.keys.remove(&key) else {
            return;
        };
        self.pending.remove(&key);
        if let Some(v) = self.by_wd.get_mut(&k.wd) {
            v.retain(|x| *x != key);
            if v.is_empty() {
                self.by_wd.remove(&k.wd);
                let _ = inotify::remove_watch(&self.ino, k.wd);
            }
        }
    }

    fn read_events(&mut self, buf: &mut [MaybeUninit<u8>]) {
        let now = Instant::now();
        let mut reader = inotify::Reader::new(&self.ino, buf);
        // Ends with EAGAIN once the queue is drained.
        while let Ok(ev) = reader.next() {
            let flags = ev.events();
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                for &k in self.keys.keys() {
                    self.pending.entry(k).or_insert(now);
                }
                continue;
            }
            let Some(keys) = self.by_wd.get(&ev.wd()) else {
                continue;
            };
            let structural = flags.intersects(STRUCTURE);
            for &k in keys {
                let wants = structural || self.keys.get(&k).is_some_and(|k| k.contents);
                if wants && !flags.contains(ReadFlags::IGNORED) {
                    self.pending.entry(k).or_insert(now);
                }
            }
            if flags.contains(ReadFlags::IGNORED) {
                // The kernel dropped the watch (directory deleted or
                // unmounted); report the keys so the GUI re-lists them.
                if let Some(keys) = self.by_wd.remove(&ev.wd()) {
                    for k in keys {
                        self.keys.remove(&k);
                        self.pending.entry(k).or_insert(now);
                    }
                }
            }
        }
    }

    fn due_at(&self, key: &K, first: Instant) -> Instant {
        let after_burst = first + DELAY;
        match self.last_report.get(key) {
            Some(&last) => after_burst.max(last + MIN_INTERVAL),
            None => after_burst,
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .iter()
            .map(|(k, &first)| self.due_at(k, first))
            .min()
    }

    fn take_due(&mut self) -> Vec<K> {
        let now = Instant::now();
        let due: Vec<K> = self
            .pending
            .iter()
            .filter(|(k, first)| self.due_at(k, **first) <= now)
            .map(|(k, _)| *k)
            .collect();
        for k in &due {
            self.pending.remove(k);
            self.last_report.insert(*k, now);
        }
        if self.last_report.len() > self.limit * 4 {
            let keys = &self.keys;
            self.last_report.retain(|k, _| keys.contains_key(k));
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn watcher(limit: usize) -> (Watcher<u32>, mpsc::Receiver<Vec<u32>>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let w = Watcher::start(limit, move |keys| {
            let mut keys = keys;
            keys.sort();
            let _ = tx.lock().unwrap().send(keys);
        })
        .unwrap();
        (w, rx)
    }

    fn settle() {
        // Commands are applied asynchronously.
        std::thread::sleep(Duration::from_millis(50));
    }

    fn recv(rx: &mpsc::Receiver<Vec<u32>>) -> Option<Vec<u32>> {
        rx.recv_timeout(Duration::from_millis(600)).ok()
    }

    #[test]
    fn reports_created_and_deleted_entries() {
        let t = tempfile::tempdir().unwrap();
        let (w, rx) = watcher(8);
        w.watch(1, t.path().to_owned(), false);
        settle();
        let t0 = Instant::now();
        fs::write(t.path().join("a"), b"x").unwrap();
        assert_eq!(recv(&rx), Some(vec![1]));
        assert!(t0.elapsed() < Duration::from_millis(200), "within 200 ms");
        fs::remove_file(t.path().join("a")).unwrap();
        assert_eq!(recv(&rx), Some(vec![1]));
    }

    #[test]
    fn bursts_are_coalesced() {
        let t = tempfile::tempdir().unwrap();
        let (w, rx) = watcher(8);
        w.watch(1, t.path().to_owned(), false);
        settle();
        for i in 0..200 {
            fs::write(t.path().join(format!("f{i}")), b"").unwrap();
        }
        assert_eq!(recv(&rx), Some(vec![1]));
        // At most one more report for the tail of the burst.
        let extra = std::iter::from_fn(|| recv(&rx)).count();
        assert!(extra <= 1, "{extra} extra reports");
    }

    #[test]
    fn contents_flag_controls_modify_events() {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("f"), b"").unwrap();
        let (w, rx) = watcher(8);
        w.watch(1, t.path().to_owned(), false);
        w.watch(2, t.path().to_owned(), true);
        settle();
        fs::write(t.path().join("f"), b"more").unwrap();
        assert_eq!(recv(&rx), Some(vec![2]));
    }

    #[test]
    fn unwatch_and_eviction() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (w, rx) = watcher(1);
        w.watch(1, a.path().to_owned(), false);
        w.watch(2, b.path().to_owned(), false); // evicts key 1
        settle();
        fs::write(a.path().join("x"), b"").unwrap();
        assert_eq!(recv(&rx), None);
        fs::write(b.path().join("x"), b"").unwrap();
        assert_eq!(recv(&rx), Some(vec![2]));
        w.unwatch(2);
        settle();
        fs::write(b.path().join("y"), b"").unwrap();
        assert_eq!(recv(&rx), None);
    }

    #[test]
    fn deleted_directory_is_reported() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("d");
        fs::create_dir(&d).unwrap();
        let (w, rx) = watcher(8);
        w.watch(7, d.clone(), false);
        settle();
        fs::remove_dir(&d).unwrap();
        assert_eq!(recv(&rx), Some(vec![7]));
    }
}
