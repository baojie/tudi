//! `tudi-indexd`: the user index daemon (PLAN §5.4, M5).
//!
//! - Loads the snapshot and serves queries right away; then reconciles in
//!   the background, or scans roots that are not in the snapshot.
//! - Roots: `[index] roots` plus `[[index.extra_roots]]`, each with its own
//!   excludes and reconcile interval. Slow devices (rotational or USB, e.g.
//!   an SD card in a reader) default to a daily reconcile. Roots whose
//!   filesystem does not keep folder mtimes (FUSE) are re-listed fully.
//! - A root that looks unmounted (missing, or an empty mount point) goes
//!   offline: its entries are kept and hidden from results; when it is back
//!   it is reconciled at once.
//! - Scans of a root run into a separate index and are grafted in, so
//!   searching keeps working while a slow volume is scanned. Reconciles
//!   `statx` under a read lock and take the write lock only to apply.
//! - Change sources: the fanotify helper (`tudi-fanotifyd`, M7) when it is
//!   installed — live events for every root on a filesystem it marks, and
//!   such roots are reconciled at most hourly; otherwise inotify on the
//!   most used folders. Always: listing hints from the GUI, periodic
//!   reconcile. Snapshots after changes and on `Stop`.
//!
//! Threads: one updater owns every change to the index; queries take a
//! read lock. The daemon lowers its own CPU and I/O priority.

mod helper;

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tudi_index::fsinfo::{RootInfo, detect, looks_offline};
use tudi_index::matcher::{Context, SearchData, Weights, search_query};
use tudi_index::scan::{
    Excludes, Listed, apply_changed, apply_dir, changed_dirs, reconcile_root, scan_below,
};
use tudi_index::{Entry, Index, Kind, snapshot};
use tudi_proto::{Request, Response, ResultItem, Status, read_frame, write_frame};

/// How often removable roots are checked for being (un)mounted.
const ONLINE_CHECK: Duration = Duration::from_secs(30);
/// Default reconcile interval for slow devices.
const SLOW_MINUTES: u32 = 24 * 60;
/// Shortest reconcile interval for roots with live fanotify events (the
/// reconcile is then only a safety net).
const LIVE_RECONCILE: Duration = Duration::from_secs(60 * 60);

struct Engine {
    ix: Index,
    sd: Arc<SearchData>,
    /// `ix.generation()` that `sd` was built for.
    sd_gen: u64,
    frecency: HashMap<u32, f32>,
}

/// A configured root.
#[derive(Clone)]
struct RootCfg {
    path: PathBuf,
    excludes: Excludes,
    every: Duration,
    info: RootInfo,
}

struct Shared {
    engine: RwLock<Engine>,
    state: Mutex<String>,
    last_scan: Mutex<i64>,
    roots: Vec<RootCfg>,
    /// Paths of roots currently offline.
    offline: Mutex<Vec<PathBuf>>,
    updates: Mutex<mpsc::Sender<Update>>,
    stop: AtomicBool,
    helper: Arc<helper::State>,
    /// Serialises snapshot writes (updater and refresher).
    save_lock: Mutex<()>,
    /// Wakes the refresher thread.
    refresh: Mutex<mpsc::Sender<()>>,
}

enum Update {
    Hint(PathBuf, Vec<Entry>),
    Changed(Vec<PathBuf>),
    Helper(helper::Event),
    Rescan,
    Frecency,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn rss_mb() -> f32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f32>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

/// Frecency weights (0..=1) by folder node, from the history database.
fn load_frecency(ix: &Index) -> HashMap<u32, f32> {
    let Some(path) = tudi_core::frecency::default_path().filter(|p| p.exists()) else {
        return HashMap::new();
    };
    let Ok(f) = tudi_core::frecency::Frecency::open(&path, Default::default()) else {
        return HashMap::new();
    };
    let top = f.top(1000, now()).unwrap_or_default();
    let max = top.iter().map(|e| e.score).fold(0.0f64, f64::max);
    if max <= 0.0 {
        return HashMap::new();
    }
    top.iter()
        .filter_map(|e| Some((ix.find(&e.path)?, (e.score / max) as f32)))
        .collect()
}

/// Most used folders (paths), for inotify.
fn frecent_dirs(limit: usize) -> Vec<PathBuf> {
    let Some(path) = tudi_core::frecency::default_path().filter(|p| p.exists()) else {
        return Vec::new();
    };
    tudi_core::frecency::Frecency::open(&path, Default::default())
        .and_then(|f| f.top(limit, now()))
        .map(|v| v.into_iter().map(|e| e.path).collect())
        .unwrap_or_default()
}

fn set_state(sh: &Shared, s: &str) {
    let mut cur = sh.state.lock().unwrap();
    if *cur != s {
        tracing::info!("state: {s}");
        *cur = s.to_string();
    }
}

fn root_node(ix: &Index, path: &Path) -> Option<u32> {
    ix.roots.iter().find(|(p, _)| p == path).map(|(_, n)| *n)
}

/// New search data for `ix`. With the same node ids as `prev` (built for
/// generation `prev_gen`), keep its typo vocabulary — the biggest temporary
/// allocation, several times the index — and add a small one for the newer
/// nodes; after a compaction everything is rebuilt (by `warm`).
fn build_search_data(ix: &Index, prev: &SearchData, prev_gen: u64) -> SearchData {
    if prev_gen == ix.generation() {
        SearchData::build_reusing(ix, prev)
    } else {
        SearchData::build(ix)
    }
}

/// Rebuild the search data off the write lock, then swap it in. Only the
/// updater thread changes the index, so nothing races.
fn refresh_search_data(sh: &Shared) {
    let (sd, frec, generation) = {
        let e = sh.engine.read().unwrap();
        (
            Arc::new(build_search_data(&e.ix, &e.sd, e.sd_gen)),
            load_frecency(&e.ix),
            e.ix.generation(),
        )
    };
    {
        let mut e = sh.engine.write().unwrap();
        e.sd = sd.clone();
        e.sd_gen = generation;
        e.frecency = frec;
    }
    let e = sh.engine.read().unwrap();
    sd.warm(&e.ix);
    drop(e);
    tudi_index::release_free_memory();
}

fn save(sh: &Shared, path: &Path) {
    let _g = sh.save_lock.lock().unwrap();
    let e = sh.engine.read().unwrap();
    let t = Instant::now();
    match snapshot::save(&e.ix, path) {
        Ok(()) => tracing::info!("snapshot saved ({} entries, {:?})", e.ix.len(), t.elapsed()),
        Err(err) => tracing::warn!("cannot save snapshot: {err}"),
    }
}

/// Refresher thread: after small changes, rebuild the search data and save
/// the snapshot off the updater thread, which keeps applying change events
/// meanwhile (P10). The search data is built under a read lock (≈ 0.1 s:
/// the typo vocabulary is reused, see `build_search_data`), so the updater
/// waits at most that long for its write lock; no copy of the index is
/// made, which would add its size to the peak memory.
fn refresher(sh: Arc<Shared>, rx: mpsc::Receiver<()>, snap: PathBuf) {
    while rx.recv().is_ok() {
        while rx.try_recv().is_ok() {}
        let t = Instant::now();
        let (sd, frec, generation) = {
            let e = sh.engine.read().unwrap();
            let sd = build_search_data(&e.ix, &e.sd, e.sd_gen);
            sd.warm(&e.ix);
            (Arc::new(sd), load_frecency(&e.ix), e.ix.generation())
        };
        {
            let mut e = sh.engine.write().unwrap();
            if e.ix.generation() == generation {
                e.sd = sd;
                e.sd_gen = generation;
                e.frecency = frec;
            }
        }
        save(&sh, &snap);
        tracing::info!("refreshed search data and snapshot in {:?}", t.elapsed());
        tudi_index::release_free_memory();
    }
}

/// Scan one root into a separate index (no lock held), then graft it in,
/// replacing what was there.
fn scan_root(sh: &Shared, r: &RootCfg, cancel: &AtomicBool) {
    set_state(sh, "scanning");
    let t = Instant::now();
    let mut tmp = Index::new();
    let mtime = std::fs::metadata(&r.path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    let node = tmp.add_root(&r.path, mtime);
    let mnt = tudi_index::scan::mount_id(&r.path);
    if scan_below(&mut tmp, node, mnt, &r.excludes, cancel, |_| {}).is_none() {
        return;
    }
    tracing::info!(
        "scanned {} ({} entries) in {:?}",
        r.path.display(),
        tmp.len(),
        t.elapsed()
    );
    {
        let mut e = sh.engine.write().unwrap();
        if let Some(old) = root_node(&e.ix, &r.path) {
            e.ix.remove_root(old);
        }
        e.ix.graft(&tmp);
        if e.ix.needs_compaction() {
            e.ix.compact();
        }
    }
    drop(tmp);
    *sh.last_scan.lock().unwrap() = now();
    refresh_search_data(sh);
}

/// Reconcile one root; returns whether the index changed.
fn reconcile_one(sh: &Shared, r: &RootCfg, cancel: &AtomicBool) -> bool {
    set_state(sh, "reconciling");
    let t = Instant::now();
    let node = {
        let e = sh.engine.read().unwrap();
        root_node(&e.ix, &r.path)
    };
    let Some(node) = node else { return false };
    let changed = if r.info.mtime_reliable {
        // Phase 1 under a read lock: queries keep running.
        let found = {
            let e = sh.engine.read().unwrap();
            changed_dirs(&e.ix, cancel, Some(node))
        };
        let Some((checked, dirs)) = found else {
            return false;
        };
        let n = dirs.len();
        let stats = if dirs.is_empty() {
            None
        } else {
            let mut e = sh.engine.write().unwrap();
            apply_changed(&mut e.ix, dirs, &r.excludes, cancel)
        };
        tracing::info!(
            "reconciled {}: {checked} folders checked, {n} changed, {:?}",
            r.path.display(),
            t.elapsed()
        );
        stats.is_some_and(|s| s.changed)
    } else {
        let mut e = sh.engine.write().unwrap();
        let s = reconcile_root(&mut e.ix, node, &r.excludes, cancel, true);
        tracing::info!("re-listed {} fully in {:?}", r.path.display(), t.elapsed());
        s.is_some_and(|s| s.changed)
    };
    *sh.last_scan.lock().unwrap() = now();
    tudi_index::release_free_memory();
    changed
}

/// Is this root unmounted (and so to be hidden)?
fn check_offline(sh: &Shared, r: &RootCfg) -> bool {
    let children = {
        let e = sh.engine.read().unwrap();
        root_node(&e.ix, &r.path).map_or(0, |n| e.ix.child_count(n))
    };
    looks_offline(&r.path, children)
}

/// The configured root holding `dir` (the innermost one).
fn root_of<'a>(roots: &'a [RootCfg], dir: &Path) -> Option<&'a RootCfg> {
    roots
        .iter()
        .filter(|r| dir.starts_with(&r.path))
        .max_by_key(|r| r.path.as_os_str().len())
}

/// Re-list changed folders: find them under a read lock, list them with no
/// lock held, apply under the write lock. Folders not in the index (inside
/// excluded folders, or gone) are skipped. Returns whether anything
/// changed.
fn relist(sh: &Shared, roots: &[RootCfg], mut dirs: Vec<PathBuf>, cancel: &AtomicBool) -> bool {
    dirs.sort_unstable();
    dirs.dedup();
    let found: Vec<(u32, PathBuf)> = {
        let e = sh.engine.read().unwrap();
        dirs.into_iter()
            .filter_map(|d| {
                let n = e.ix.find(&d)?;
                let node = &e.ix.nodes[n as usize];
                (node.is_dir() && node.flags & tudi_index::flags::EXCLUDED == 0).then_some((n, d))
            })
            .collect()
    };
    if found.is_empty() {
        return false;
    }
    let mut buf = vec![std::mem::MaybeUninit::<u8>::uninit(); 64 * 1024];
    let listed: Vec<(u32, PathBuf, Option<u64>, Vec<Listed>)> = found
        .into_iter()
        .filter_map(|(n, d)| {
            let mnt = tudi_index::scan::mount_id(&d);
            let items = tudi_index::scan::list_dir(&d, mnt, &mut buf).ok()?;
            Some((n, d, mnt, items))
        })
        .collect();
    let mut e = sh.engine.write().unwrap();
    let mut changed = false;
    for (n, d, mnt, items) in listed {
        if let Some(r) = root_of(roots, &d) {
            changed |= apply_dir(&mut e.ix, n, &d, items, mnt, &r.excludes, cancel);
        }
    }
    changed
}

fn updater(sh: Arc<Shared>, rx: mpsc::Receiver<Update>, snap: PathBuf) {
    let cancel = AtomicBool::new(false);
    let mut dirty = false;
    let mut last_save = Instant::now();
    let roots = sh.roots.clone();
    let startup = Instant::now();
    // Roots with live helper events.
    let mut live: Vec<bool> = vec![false; roots.len()];
    let every = |i: usize, live: &[bool]| {
        if live[i] {
            roots[i].every.max(LIVE_RECONCILE)
        } else {
            roots[i].every
        }
    };

    // Roots in the snapshot that are no longer configured.
    {
        let mut e = sh.engine.write().unwrap();
        let stale: Vec<u32> =
            e.ix.roots
                .iter()
                .filter(|(p, _)| !roots.iter().any(|r| r.path == *p))
                .map(|(_, n)| *n)
                .collect();
        for n in stale {
            e.ix.remove_root(n);
            dirty = true;
        }
    }

    // Start: fast roots first, so they are searchable before a slow volume
    // is done. Roots not in the snapshot are scanned, others reconciled.
    let mut order: Vec<usize> = (0..roots.len()).collect();
    order.sort_by_key(|&i| roots[i].info.slow());
    let mut next: Vec<Instant> = vec![Instant::now(); roots.len()];
    let mut online: Vec<bool> = vec![true; roots.len()];
    // When each root's startup pass began: helper events from before that
    // need no extra reconcile.
    let mut began: Vec<Instant> = vec![startup; roots.len()];
    for &i in &order {
        let r = &roots[i];
        began[i] = Instant::now();
        if check_offline(&sh, r) {
            tracing::info!("{} is offline", r.path.display());
            online[i] = false;
            continue;
        }
        let known = root_node(&sh.engine.read().unwrap().ix, &r.path).is_some();
        if known {
            dirty |= reconcile_one(&sh, r, &cancel);
            refresh_search_data(&sh);
        } else {
            scan_root(&sh, r, &cancel);
            save(&sh, &snap);
            last_save = Instant::now();
        }
        next[i] = Instant::now() + r.every;
    }
    *sh.offline.lock().unwrap() = roots
        .iter()
        .zip(&online)
        .filter(|(_, on)| !**on)
        .map(|(r, _)| r.path.clone())
        .collect();
    set_state(&sh, "ready");
    if dirty {
        save(&sh, &snap);
        dirty = false;
        last_save = Instant::now();
    }

    // Tests shorten the online check with TUDI_INDEXD_ONLINE_CHECK_SECS.
    let online_check = std::env::var("TUDI_INDEXD_ONLINE_CHECK_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(ONLINE_CHECK, Duration::from_secs);
    let mut next_online_check = Instant::now() + online_check;
    let mut stash: Option<Update> = None;
    loop {
        if sh.stop.load(Ordering::Relaxed) {
            return;
        }
        let due = next
            .iter()
            .copied()
            .min()
            .unwrap_or(next_online_check)
            .min(next_online_check);
        let job = if let Some(j) = stash.take() {
            Some(j)
        } else {
            match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                Ok(j) => Some(j),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        };
        match job {
            Some(Update::Rescan) => {
                for &i in &order {
                    if online[i] {
                        scan_root(&sh, &roots[i], &cancel);
                        next[i] = Instant::now() + every(i, &live);
                    }
                }
                save(&sh, &snap);
                dirty = false;
                last_save = Instant::now();
            }
            Some(Update::Hint(dir, entries)) => {
                let mut e = sh.engine.write().unwrap();
                if let (Some(node), Some(r)) = (e.ix.find(&dir), root_of(&roots, &dir)) {
                    let mnt = tudi_index::scan::mount_id(&dir);
                    let items = entries
                        .into_iter()
                        .map(|entry| Listed {
                            entry,
                            other_fs: false,
                        })
                        .collect();
                    dirty |= apply_dir(&mut e.ix, node, &dir, items, mnt, &r.excludes, &cancel);
                }
            }
            Some(Update::Changed(dirs)) => {
                dirty |= relist(&sh, &roots, dirs, &cancel);
            }
            Some(Update::Helper(ev)) => match ev {
                helper::Event::Changed(dirs) => {
                    // Batch whatever else the helper already sent.
                    let mut dirs = dirs;
                    while dirs.len() < 100_000 {
                        match rx.try_recv() {
                            Ok(Update::Helper(helper::Event::Changed(more))) => dirs.extend(more),
                            Ok(other) => {
                                // Not a change batch: handle it next.
                                stash = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    dirty |= relist(&sh, &roots, dirs, &cancel);
                }
                helper::Event::Up { covered, at } => {
                    for (i, r) in roots.iter().enumerate() {
                        live[i] = covered.contains(&r.path);
                        // Changes between the root's startup pass and this
                        // connection were seen by no one: reconcile now.
                        // Connected before that pass began: the next
                        // reconcile is a safety net, an hour away.
                        next[i] = if live[i] && at > began[i] {
                            Instant::now()
                        } else {
                            Instant::now() + every(i, &live)
                        };
                    }
                }
                helper::Event::Overflow => {
                    for i in 0..roots.len() {
                        if live[i] {
                            next[i] = Instant::now();
                        }
                    }
                }
                helper::Event::Down => {
                    for (i, l) in live.iter_mut().enumerate() {
                        if *l {
                            *l = false;
                            next[i] = Instant::now() + roots[i].every;
                        }
                    }
                }
            },
            Some(Update::Frecency) => {
                let frec = {
                    let e = sh.engine.read().unwrap();
                    load_frecency(&e.ix)
                };
                sh.engine.write().unwrap().frecency = frec;
            }
            None => {}
        }
        let now = Instant::now();
        if now >= next_online_check || next.iter().any(|t| now >= *t) {
            // Timers: online checks, then due reconciles.
            if Instant::now() >= next_online_check {
                next_online_check = Instant::now() + online_check;
                for (i, r) in roots.iter().enumerate() {
                    let off = check_offline(&sh, r);
                    let was_offline = !online[i];
                    if off != was_offline {
                        online[i] = !off;
                        tracing::info!(
                            "{} is {}",
                            r.path.display(),
                            if off { "offline" } else { "back online" }
                        );
                        if !off {
                            next[i] = Instant::now(); // reconcile now
                        }
                    }
                }
                *sh.offline.lock().unwrap() = roots
                    .iter()
                    .zip(&online)
                    .filter(|(_, on)| !**on)
                    .map(|(r, _)| r.path.clone())
                    .collect();
            }
            for &i in &order {
                if Instant::now() < next[i] {
                    continue;
                }
                next[i] = Instant::now() + every(i, &live);
                if !online[i] {
                    continue;
                }
                let r = &roots[i];
                let known = root_node(&sh.engine.read().unwrap().ix, &r.path).is_some();
                if known {
                    if reconcile_one(&sh, r, &cancel) {
                        refresh_search_data(&sh);
                        save(&sh, &snap);
                        dirty = false;
                        last_save = Instant::now();
                    }
                } else {
                    scan_root(&sh, r, &cancel);
                    save(&sh, &snap);
                    dirty = false;
                    last_save = Instant::now();
                }
            }
            let mut e = sh.engine.write().unwrap();
            if e.ix.needs_compaction() {
                e.ix.compact();
                dirty = true;
            }
        }
        set_state(&sh, "ready");
        // Small changes (helper, hints, inotify): searchable at once; search
        // data and snapshot refreshed at most once a minute, off this thread.
        if dirty && last_save.elapsed() > Duration::from_secs(60) {
            let _ = sh.refresh.lock().unwrap().send(());
            dirty = false;
            last_save = Instant::now();
        }
    }
}

fn handle(sh: &Shared, req: Request, snap: &Path) -> Response {
    match req {
        Request::Query {
            id,
            text,
            limit,
            show_hidden,
            dirs_only,
            scope,
        } => {
            let t = Instant::now();
            let q = tudi_index::query::parse_local(&text);
            let e = sh.engine.read().unwrap();
            let scope = match scope {
                None => None,
                Some(p) => match e.ix.find(Path::new(std::ffi::OsStr::from_bytes(&p))) {
                    Some(n) => Some(n),
                    None => {
                        return Response::Error(format!(
                            "{} is not in the index",
                            String::from_utf8_lossy(&p)
                        ));
                    }
                },
            };
            let offline: Vec<u32> = sh
                .offline
                .lock()
                .unwrap()
                .iter()
                .filter_map(|p| root_node(&e.ix, p))
                .collect();
            let ctx = Context {
                frecency: Some(&e.frecency),
                now: now(),
                show_hidden,
                dirs_only,
                offline_roots: &offline,
                scope,
            };
            let hits = search_query(
                &e.ix,
                &e.sd,
                &q,
                limit.min(1000) as usize,
                &Weights::default(),
                &ctx,
            );
            let items = hits
                .into_iter()
                .map(|h| {
                    let n = e.ix.nodes[h.node as usize];
                    ResultItem {
                        path: e.ix.path(h.node).as_os_str().as_bytes().to_vec(),
                        kind: n.kind,
                        size: n.size,
                        mtime: tudi_index::unpack_mtime(n.mtime),
                        score: h.score,
                        indices: h.indices,
                    }
                })
                .collect();
            let state = sh.state.lock().unwrap().clone();
            Response::Results {
                id,
                items,
                elapsed_us: t.elapsed().as_micros().min(u128::from(u32::MAX)) as u32,
                partial: state == "scanning",
                hints: q.hints.iter().map(ToString::to_string).collect(),
            }
        }
        Request::Status => {
            let e = sh.engine.read().unwrap();
            let offline = sh.offline.lock().unwrap();
            let live = sh.helper.covered();
            Response::Status(Status {
                entries: e.ix.len() as u64,
                roots: sh
                    .roots
                    .iter()
                    .map(|r| {
                        let mut tags = vec![r.info.fs.clone()];
                        if r.info.slow() {
                            tags.push("slow".into());
                        }
                        if offline.contains(&r.path) {
                            tags.push("offline".into());
                        }
                        let every = if live.contains(&r.path) {
                            tags.push("live".into());
                            r.every.max(LIVE_RECONCILE)
                        } else {
                            r.every
                        };
                        tags.push(format!("every {} min", every.as_secs() / 60));
                        format!("{} ({})", r.path.display(), tags.join(", "))
                    })
                    .collect(),
                state: sh.state.lock().unwrap().clone(),
                last_scan: *sh.last_scan.lock().unwrap(),
                rss_mb: rss_mb(),
            })
        }
        Request::Rescan => {
            let _ = sh.updates.lock().unwrap().send(Update::Rescan);
            Response::Ok
        }
        Request::FrecencyChanged => {
            let _ = sh.updates.lock().unwrap().send(Update::Frecency);
            Response::Ok
        }
        Request::ListingHint { dir, entries } => {
            use std::os::unix::ffi::OsStringExt;
            let dir = PathBuf::from(std::ffi::OsString::from_vec(dir));
            let entries = entries
                .into_iter()
                .map(|h| Entry {
                    name: h.name,
                    kind: Kind::from_u8(h.kind),
                    size: h.size,
                    mtime: h.mtime,
                })
                .collect();
            let _ = sh.updates.lock().unwrap().send(Update::Hint(dir, entries));
            Response::Ok
        }
        Request::Stop => {
            sh.stop.store(true, Ordering::Relaxed);
            save(sh, snap);
            Response::Ok
        }
    }
}

fn serve(sh: Arc<Shared>, stream: UnixStream, snap: PathBuf) {
    let mut s = stream;
    while let Ok(req) = read_frame::<Request>(&mut s) {
        let stop = matches!(req, Request::Stop);
        let resp = handle(&sh, req, &snap);
        if write_frame(&mut s, &resp).is_err() {
            return;
        }
        if stop {
            std::process::exit(0);
        }
    }
}

/// Run in the background: lower CPU priority (nice 10) and idle I/O
/// scheduling. Linux applies both per thread and new threads inherit them,
/// so this runs before any thread is started.
fn lower_priority() {
    // SAFETY: setpriority only changes this process's scheduling priority;
    // the arguments are plain integers.
    let r = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
    if r != 0 {
        tracing::debug!("setpriority failed");
    }
    const IOPRIO_WHO_PROCESS: libc::c_long = 1;
    const IOPRIO_CLASS_IDLE: libc::c_long = 3;
    const IOPRIO_CLASS_SHIFT: libc::c_long = 13;
    // SAFETY: ioprio_set(2) takes integers only and affects this process.
    let r = unsafe {
        libc::syscall(
            libc::SYS_ioprio_set,
            IOPRIO_WHO_PROCESS,
            0 as libc::c_long,
            IOPRIO_CLASS_IDLE << IOPRIO_CLASS_SHIFT,
        )
    };
    if r != 0 {
        tracing::debug!("ioprio_set failed");
    }
}

/// Configured roots with their settings and what they live on.
fn configured_roots(cfg: &tudi_core::config::IndexConfig) -> Vec<RootCfg> {
    let base = Excludes::new(&cfg.exclude).unwrap_or_else(|e| {
        tracing::warn!("bad [index] exclude: {e}; using defaults");
        Excludes::defaults()
    });
    let mut out: Vec<RootCfg> = cfg
        .root_paths()
        .into_iter()
        .map(|path| {
            let info = detect(&path);
            RootCfg {
                path,
                excludes: base.clone(),
                every: Duration::from_secs(u64::from(cfg.reconcile_minutes.max(1)) * 60),
                info,
            }
        })
        .collect();
    for x in &cfg.extra_roots {
        let path = PathBuf::from(&x.path);
        let info = detect(&path);
        let minutes = x.reconcile_minutes.unwrap_or(if info.slow() {
            SLOW_MINUTES
        } else {
            cfg.reconcile_minutes
        });
        let mut patterns = cfg.exclude.clone();
        patterns.extend(x.exclude.iter().cloned());
        let excludes = Excludes::new(&patterns).unwrap_or_else(|e| {
            tracing::warn!("bad exclude for {}: {e}", x.path);
            base.clone()
        });
        out.push(RootCfg {
            path,
            excludes,
            every: Duration::from_secs(u64::from(minutes.max(1)) * 60),
            info,
        });
    }
    // One entry per path.
    out.dedup_by(|a, b| a.path == b.path);
    for r in &out {
        tracing::info!(
            "root {}: {:?}, reconcile every {} min",
            r.path.display(),
            r.info,
            r.every.as_secs() / 60
        );
    }
    out
}

fn main() -> anyhow::Result<()> {
    tudi_index::tune_allocator();
    lower_priority();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let sock = tudi_proto::socket_path();
    // One daemon per user.
    if UnixStream::connect(&sock).is_ok() {
        tracing::info!("already running");
        return Ok(());
    }
    if let Some(dir) = sock.parent() {
        std::fs::create_dir_all(dir)?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;

    let (config, _) = tudi_core::config::Config::load_user();
    let roots = configured_roots(&config.index);
    let snap = snapshot::default_path().unwrap_or_else(|| PathBuf::from("/tmp/tudi-index.bin"));

    // Serve the snapshot at once; the updater reconciles it.
    let t = Instant::now();
    let ix = snapshot::load(&snap).unwrap_or_default();
    tudi_index::release_free_memory();
    if !ix.is_empty() {
        tracing::info!("loaded snapshot: {} entries in {:?}", ix.len(), t.elapsed());
    }
    let state = if ix.is_empty() { "scanning" } else { "ready" };
    let sd = Arc::new(SearchData::build(&ix));
    let frecency = load_frecency(&ix);
    let (tx, rx) = mpsc::channel();
    let (refresh_tx, refresh_rx) = mpsc::channel();
    let sh = Arc::new(Shared {
        engine: RwLock::new(Engine {
            sd_gen: ix.generation(),
            ix,
            sd,
            frecency,
        }),
        state: Mutex::new(state.into()),
        last_scan: Mutex::new(0),
        roots,
        offline: Mutex::default(),
        updates: Mutex::new(tx.clone()),
        stop: AtomicBool::new(false),
        helper: Arc::default(),
        save_lock: Mutex::new(()),
        refresh: Mutex::new(refresh_tx),
    });

    // Live events from the fanotify helper, if installed.
    {
        let roots: Vec<PathBuf> = sh
            .roots
            .iter()
            .filter(|r| r.info.fanotify)
            .map(|r| r.path.clone())
            .collect();
        let htx = Mutex::new(tx.clone());
        helper::spawn(
            roots,
            sh.helper.clone(),
            Arc::new(AtomicBool::new(false)),
            move |ev| {
                let _ = htx.lock().unwrap().send(Update::Helper(ev));
            },
        );
    }

    // inotify on the most used folders (fallback change source; ignored
    // for roots with live helper events).
    let watched = frecent_dirs(200);
    let table = Arc::new(watched.clone());
    let wtx = Mutex::new(tx.clone());
    let t2 = table.clone();
    let hs = sh.helper.clone();
    let watcher = tudi_core::watch::Watcher::start(256, move |keys: Vec<usize>| {
        let dirs: Vec<PathBuf> = keys
            .into_iter()
            .filter_map(|k| t2.get(k).cloned())
            .filter(|d| !hs.is_live(d))
            .collect();
        if !dirs.is_empty() {
            let _ = wtx.lock().unwrap().send(Update::Changed(dirs));
        }
    })
    .ok();
    if let Some(w) = &watcher {
        for (k, d) in watched.into_iter().enumerate() {
            w.watch(k, d, false);
        }
    }

    {
        let (sh, snap) = (sh.clone(), snap.clone());
        std::thread::Builder::new()
            .name("updater".into())
            .spawn(move || updater(sh, rx, snap))?;
    }
    {
        let (sh, snap) = (sh.clone(), snap.clone());
        std::thread::Builder::new()
            .name("refresher".into())
            .spawn(move || refresher(sh, refresh_rx, snap))?;
    }
    tracing::info!("listening on {}", sock.display());
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (sh, snap) = (sh.clone(), snap.clone());
        std::thread::spawn(move || serve(sh, stream, snap));
    }
    drop(watcher);
    Ok(())
}
