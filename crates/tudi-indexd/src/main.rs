//! `tudi-indexd`: the user index daemon (PLAN §5.4, M5).
//!
//! - Loads the snapshot and serves queries right away; then reconciles in
//!   the background (or scans from scratch without a snapshot).
//! - Reconciles every `[index] reconcile_minutes`, watches the most used
//!   folders with inotify, and applies folder listings sent by the GUI.
//! - Serves `tudi-proto` requests on `$XDG_RUNTIME_DIR/tudi/indexd.sock`.
//! - Saves the snapshot after changes and on `Stop`.
//!
//! Threads: one updater owns every change to the index (so a search-data
//! rebuild never races another change); queries take a read lock. Run it at
//! low priority (the systemd unit sets `Nice` and idle I/O).

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tudi_index::matcher::{Context, SearchData, Weights, search};
use tudi_index::scan::{Excludes, reconcile, scan_below};
use tudi_index::{Entry, Index, Kind, snapshot};
use tudi_proto::{Request, Response, ResultItem, Status, read_frame, write_frame};

struct Engine {
    ix: Index,
    sd: Arc<SearchData>,
    frecency: HashMap<u32, f32>,
}

struct Shared {
    engine: RwLock<Engine>,
    state: Mutex<String>,
    last_scan: Mutex<i64>,
    roots: Vec<PathBuf>,
    updates: Mutex<mpsc::Sender<Update>>,
    stop: AtomicBool,
}

enum Update {
    Hint(PathBuf, Vec<Entry>),
    Changed(Vec<PathBuf>),
    Reconcile,
    Rescan,
    Frecency,
    Save,
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
    let Some(path) = tudi_core::frecency::default_path() else {
        return HashMap::new();
    };
    if !path.exists() {
        return HashMap::new();
    }
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
    let Some(path) = tudi_core::frecency::default_path() else {
        return Vec::new();
    };
    tudi_core::frecency::Frecency::open(&path, Default::default())
        .and_then(|f| f.top(limit, now()))
        .map(|v| v.into_iter().map(|e| e.path).collect())
        .unwrap_or_default()
}

fn set_state(sh: &Shared, s: &str) {
    *sh.state.lock().unwrap() = s.to_string();
    tracing::info!("state: {s}");
}

/// Rebuild the search data off the write lock, then swap it in. Only the
/// updater thread changes the index, so nothing races.
fn refresh_search_data(sh: &Shared) {
    let (sd, frec) = {
        let e = sh.engine.read().unwrap();
        (Arc::new(SearchData::build(&e.ix)), load_frecency(&e.ix))
    };
    {
        let mut e = sh.engine.write().unwrap();
        e.sd = sd.clone();
        e.frecency = frec;
    }
    // Typo vocabulary in the background (~0.5 s on 2M entries), then give
    // the temporary memory of the build back to the system.
    let e = sh.engine.read().unwrap();
    sd.warm(&e.ix);
    tudi_index::release_free_memory();
}

fn save(sh: &Shared, path: &std::path::Path) {
    let e = sh.engine.read().unwrap();
    let t = Instant::now();
    match snapshot::save(&e.ix, path) {
        Ok(()) => tracing::info!("snapshot saved ({} entries, {:?})", e.ix.len(), t.elapsed()),
        Err(err) => tracing::warn!("cannot save snapshot: {err}"),
    }
}

fn updater(
    sh: Arc<Shared>,
    rx: mpsc::Receiver<Update>,
    excludes: Excludes,
    loaded: bool,
    every: Duration,
    snap: PathBuf,
) {
    let cancel = AtomicBool::new(false);
    let mut dirty = false;
    // First: scan from scratch or reconcile the loaded snapshot.
    let first = if loaded {
        Update::Reconcile
    } else {
        Update::Rescan
    };
    let mut pending = Some(first);
    let mut next_reconcile = Instant::now() + every;
    let mut last_save = Instant::now();
    // The first reconcile after loading a snapshot always refreshes the
    // search data (which also warms the typo vocabulary).
    let mut first = true;
    loop {
        let job = match pending.take() {
            Some(j) => j,
            None => {
                let wait = next_reconcile.saturating_duration_since(Instant::now());
                match rx.recv_timeout(wait) {
                    Ok(j) => j,
                    Err(mpsc::RecvTimeoutError::Timeout) => Update::Reconcile,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        };
        if sh.stop.load(Ordering::Relaxed) {
            return;
        }
        match job {
            Update::Rescan => {
                set_state(&sh, "scanning");
                let t = Instant::now();
                let fresh = tudi_index::scan::build(&sh.roots, &excludes, &cancel, |_| {});
                if let Some(ix) = fresh {
                    tracing::info!("scanned {} entries in {:?}", ix.len(), t.elapsed());
                    sh.engine.write().unwrap().ix = ix;
                }
                *sh.last_scan.lock().unwrap() = now();
                refresh_search_data(&sh);
                // Save right away: a full scan is expensive to repeat.
                save(&sh, &snap);
                dirty = false;
                last_save = Instant::now();
                next_reconcile = Instant::now() + every;
            }
            Update::Reconcile => {
                set_state(&sh, "reconciling");
                let t = Instant::now();
                let r = {
                    let mut e = sh.engine.write().unwrap();
                    if e.ix.needs_compaction() {
                        e.ix.compact();
                    }
                    reconcile(&mut e.ix, &excludes, &cancel)
                };
                if let Some(r) = r {
                    tracing::info!(
                        "reconciled: {} folders checked, {} changed, {:?}",
                        r.dirs_checked,
                        r.dirs_changed,
                        t.elapsed()
                    );
                    if r.changed || first {
                        refresh_search_data(&sh);
                    }
                    if r.changed {
                        save(&sh, &snap);
                        dirty = false;
                        last_save = Instant::now();
                    }
                }
                first = false;
                tudi_index::release_free_memory();
                *sh.last_scan.lock().unwrap() = now();
                next_reconcile = Instant::now() + every;
            }
            Update::Hint(dir, entries) => {
                let mut e = sh.engine.write().unwrap();
                if let Some(node) = e.ix.find(&dir) {
                    let (new_dirs, ch) = e.ix.apply_listing(node, &entries);
                    let mnt = tudi_index::scan::mount_id(&dir);
                    for d in new_dirs {
                        let _ = scan_below(&mut e.ix, d, mnt, &excludes, &cancel, |_| {});
                    }
                    dirty |= ch;
                    // New nodes are searchable at once (prefilter lets them
                    // through); the full refresh follows on the next save.
                }
            }
            Update::Changed(dirs) => {
                let mut buf = vec![std::mem::MaybeUninit::<u8>::uninit(); 64 * 1024];
                let mut e = sh.engine.write().unwrap();
                for dir in dirs {
                    let Some(node) = e.ix.find(&dir) else {
                        continue;
                    };
                    let mnt = tudi_index::scan::mount_id(&dir);
                    if let Ok(items) = tudi_index::scan::list_dir(&dir, mnt, &mut buf) {
                        let entries: Vec<Entry> = items.into_iter().map(|l| l.entry).collect();
                        let (new_dirs, ch) = e.ix.apply_listing(node, &entries);
                        for d in new_dirs {
                            let _ = scan_below(&mut e.ix, d, mnt, &excludes, &cancel, |_| {});
                        }
                        dirty |= ch;
                    }
                }
            }
            Update::Frecency => {
                let frec = {
                    let e = sh.engine.read().unwrap();
                    load_frecency(&e.ix)
                };
                sh.engine.write().unwrap().frecency = frec;
            }
            Update::Save => {}
        }
        set_state(&sh, "ready");
        // Changes from hints and inotify are searchable at once (new nodes
        // pass the prefilter); at most once a minute the search data is
        // rebuilt (pinyin, vocabulary) and the snapshot saved.
        if dirty && last_save.elapsed() > Duration::from_secs(60) {
            refresh_search_data(&sh);
            save(&sh, &snap);
            dirty = false;
            last_save = Instant::now();
        }
    }
}

fn handle(sh: &Shared, req: Request, snap: &std::path::Path) -> Response {
    match req {
        Request::Query {
            id,
            text,
            limit,
            show_hidden,
            dirs_only,
        } => {
            let t = Instant::now();
            let e = sh.engine.read().unwrap();
            let ctx = Context {
                frecency: Some(&e.frecency),
                now: now(),
                show_hidden,
                dirs_only,
            };
            let hits = search(
                &e.ix,
                &e.sd,
                &text,
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
                partial: state == "scanning" || state == "loading",
            }
        }
        Request::Status => {
            let e = sh.engine.read().unwrap();
            Response::Status(Status {
                entries: e.ix.len() as u64,
                roots: sh.roots.iter().map(|r| r.display().to_string()).collect(),
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

fn main() -> anyhow::Result<()> {
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
    let roots = config.index.root_paths();
    let excludes = Excludes::new(&config.index.exclude).unwrap_or_else(|e| {
        tracing::warn!("bad [index] exclude: {e}; using defaults");
        Excludes::defaults()
    });
    let every = Duration::from_secs(u64::from(config.index.reconcile_minutes.max(1)) * 60);
    let snap = snapshot::default_path().unwrap_or_else(|| PathBuf::from("/tmp/tudi-index.bin"));

    // A snapshot of the same roots is served at once.
    let t = Instant::now();
    let loaded = snapshot::load(&snap)
        .ok()
        .filter(|ix| ix.roots.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>() == roots);
    let is_loaded = loaded.is_some();
    let ix = loaded.unwrap_or_default();
    tudi_index::release_free_memory();
    if is_loaded {
        tracing::info!("loaded snapshot: {} entries in {:?}", ix.len(), t.elapsed());
    }
    let sd = Arc::new(SearchData::build(&ix));
    let frecency = load_frecency(&ix);
    let (tx, rx) = mpsc::channel();
    let sh = Arc::new(Shared {
        engine: RwLock::new(Engine { ix, sd, frecency }),
        state: Mutex::new(if is_loaded { "ready" } else { "scanning" }.into()),
        last_scan: Mutex::new(0),
        roots,
        updates: Mutex::new(tx.clone()),
        stop: AtomicBool::new(false),
    });

    // inotify on the most used folders (fallback change source).
    let wtx = Mutex::new(tx.clone());
    let watcher = tudi_core::watch::Watcher::start(256, move |keys: Vec<usize>| {
        let _ = wtx.lock().unwrap().send(Update::Changed(
            keys.into_iter()
                .filter_map(|k| WATCHED.lock().unwrap().get(k).cloned())
                .collect(),
        ));
    })
    .ok();
    if let Some(w) = &watcher {
        let dirs = frecent_dirs(200);
        let mut table = WATCHED.lock().unwrap();
        for (k, d) in dirs.into_iter().enumerate() {
            w.watch(k, d.clone(), false);
            table.push(d);
        }
    }

    {
        let sh = sh.clone();
        let snap = snap.clone();
        std::thread::Builder::new()
            .name("updater".into())
            .spawn(move || updater(sh, rx, excludes, is_loaded, every, snap))?;
    }
    let _ = tx.send(Update::Save);
    tracing::info!("listening on {}", sock.display());
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (sh, snap) = (sh.clone(), snap.clone());
        std::thread::spawn(move || serve(sh, stream, snap));
    }
    drop(watcher);
    Ok(())
}

/// Watched folder paths by watch key.
static WATCHED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
