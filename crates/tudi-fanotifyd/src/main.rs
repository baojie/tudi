//! `tudi-fanotifyd`: the root helper (PLAN §5.6, `docs/design/fanotify.md`).
//!
//! One thread, one `poll` loop:
//!
//! - Setup as root: a fanotify group in `FAN_REPORT_DFID_NAME` mode, a
//!   `FAN_MARK_FILESYSTEM` mark on every mounted filesystem of a supported
//!   type, the listening socket (from systemd, or bound here). Then
//!   privileges are dropped to `CAP_DAC_READ_SEARCH` as `nobody`.
//! - Events are collected as folder handles, deduplicated, and every batch
//!   window (200 ms after the first event) resolved to paths and sent to
//!   each subscribed client — only the folders that client may see.
//! - Queue overflow (or too many pending folders) tells every client to
//!   reconcile.
//! - Clients send one `Subscribe` and nothing else. A client that sends
//!   anything more, is not a regular user, or falls too far behind is
//!   disconnected.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use tudi_fanotifyd::access::{Cred, MetaCache, normalise_root};
use tudi_fanotifyd::event::{self, DirFid, Event};
use tudi_fanotifyd::mounts::{self, Mount};
use tudi_fanotifyd::proto::{self, ClientMsg, HelperMsg};
use tudi_fanotifyd::{privs, sys};

/// Regular users (Debian / Fedora `UID_MIN`..`UID_MAX`).
const UID_MIN: u32 = 1000;
const UID_MAX: u32 = 60000;
const MAX_CLIENTS: usize = 64;
/// Unique folders per batch before the batch is turned into an overflow.
const MAX_PENDING: usize = 50_000;
/// Bytes queued for one client before it is disconnected.
const MAX_OUT: usize = 16 * 1024 * 1024;
/// Time a client has to send its subscription.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(5);

macro_rules! log {
    ($($t:tt)*) => { eprintln!("tudi-fanotifyd: {}", format!($($t)*)) };
}

struct Args {
    socket: PathBuf,
    batch: Duration,
    user: String,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        socket: PathBuf::from(proto::SOCKET_PATH),
        batch: Duration::from_millis(200),
        user: "nobody".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--socket" => a.socket = it.next().ok_or("--socket needs a path")?.into(),
            "--batch-ms" => {
                let ms: u64 = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--batch-ms needs a number")?;
                a.batch = Duration::from_millis(ms.clamp(10, 5000));
            }
            "--user" => a.user = it.next().ok_or("--user needs a name")?,
            "--version" => return Err(format!("tudi-fanotifyd {}", env!("CARGO_PKG_VERSION"))),
            _ => {
                return Err(
                    "usage: tudi-fanotifyd [--socket PATH] [--batch-ms N] [--user NAME]\n\
                     Runs as root (normally from tudi-fanotifyd.socket)."
                        .into(),
                );
            }
        }
    }
    Ok(a)
}

/// A marked filesystem: the mount used to resolve handles, and every
/// mount of it (for aliases).
struct Fs {
    fd: OwnedFd,
    primary: Mount,
    all: Vec<Mount>,
}

/// Mark every supported filesystem; returns them by fsid.
fn mark_filesystems(fan: &OwnedFd) -> HashMap<u64, Fs> {
    let info = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut by_fsid: HashMap<u64, Vec<Mount>> = HashMap::new();
    for m in mounts::parse_mountinfo(&info) {
        if !mounts::SUPPORTED.contains(&m.fstype.as_str()) {
            continue;
        }
        let p = Path::new(std::ffi::OsStr::from_bytes(&m.point));
        match sys::fsid_path(p) {
            Ok(id) => by_fsid.entry(id).or_default().push(m),
            Err(e) => log!("skip {}: {e}", p.display()),
        }
    }
    let mut out = HashMap::new();
    for (id, all) in by_fsid {
        // Prefer a mount of the filesystem's own root.
        let primary = all
            .iter()
            .find(|m| m.root == b"/")
            .unwrap_or(&all[0])
            .clone();
        let p = Path::new(std::ffi::OsStr::from_bytes(&primary.point)).to_path_buf();
        let marked = sys::fanotify_mark(
            fan,
            event::FAN_MARK_ADD | event::FAN_MARK_FILESYSTEM,
            event::MASK,
            &p,
        )
        .and_then(|()| sys::open_dir(&p));
        match marked {
            Ok(fd) => {
                log!(
                    "watching {} ({}, fsid {id:#x})",
                    p.display(),
                    primary.fstype
                );
                out.insert(id, Fs { fd, primary, all });
            }
            Err(e) => log!("cannot watch {} ({}): {e}", p.display(), primary.fstype),
        }
    }
    out
}

/// The listening socket: from systemd socket activation, or bound here.
fn listener(path: &Path) -> io::Result<UnixListener> {
    let ours = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|p| p.parse::<u32>().ok())
        == Some(std::process::id());
    let n: i32 = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if ours && n >= 1 {
        return Ok(sys::systemd_listener());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path)?;
    // Anyone may connect; who gets what is decided per client.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(l)
}

struct Client {
    stream: UnixStream,
    cred: Cred,
    input: Vec<u8>,
    roots: Option<Vec<Vec<u8>>>,
    out: Vec<u8>,
    since: Instant,
    dead: bool,
}

impl Client {
    fn queue(&mut self, frame: &[u8]) {
        if self.out.len() + frame.len() > MAX_OUT {
            log!("uid {} is not reading; disconnecting", self.cred.uid);
            self.dead = true;
            return;
        }
        self.out.extend_from_slice(frame);
    }

    fn flush(&mut self) {
        while !self.out.is_empty() {
            match self.stream.write(&self.out) {
                Ok(0) => {
                    self.dead = true;
                    return;
                }
                Ok(n) => {
                    self.out.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.dead = true;
                    return;
                }
            }
        }
    }

    /// Read what the client sent; handle its one subscription.
    fn read(&mut self, covered: &[u64]) {
        let mut buf = [0u8; 4096];
        loop {
            match sys::read(self.stream.as_raw_fd(), &mut buf) {
                Ok(None) => return,
                Ok(Some(0)) => {
                    self.dead = true;
                    return;
                }
                Ok(Some(n)) => {
                    if self.roots.is_some() {
                        // Subscribe-only: nothing else is accepted.
                        self.dead = true;
                        return;
                    }
                    self.input.extend_from_slice(&buf[..n]);
                    if self.input.len() > proto::MAX_CLIENT_FRAME + 4 {
                        self.dead = true;
                        return;
                    }
                    match proto::complete_frame(&self.input, proto::MAX_CLIENT_FRAME) {
                        Ok(None) => {}
                        Ok(Some(r)) => {
                            if r.end != self.input.len() {
                                self.dead = true;
                                return;
                            }
                            let input = std::mem::take(&mut self.input);
                            self.subscribe(&input[r], covered);
                        }
                        Err(_) => {
                            self.dead = true;
                            return;
                        }
                    }
                }
                Err(_) => {
                    self.dead = true;
                    return;
                }
            }
        }
    }

    fn subscribe(&mut self, body: &[u8], covered: &[u64]) {
        match ClientMsg::decode(body) {
            Ok(ClientMsg::Subscribe { version, roots }) if version == proto::VERSION => {
                let roots: Vec<Vec<u8>> = roots.iter().map(|r| normalise_root(r)).collect();
                log!(
                    "uid {} subscribed to {} root(s)",
                    self.cred.uid,
                    roots.len()
                );
                self.roots = Some(roots);
                self.queue(&HelperMsg::Covered(covered.to_vec()).encode());
            }
            Ok(ClientMsg::Subscribe { version, .. }) => {
                self.queue(
                    &HelperMsg::Error(format!("unsupported protocol version {version}")).encode(),
                );
                self.flush();
                self.dead = true;
            }
            Err(_) => self.dead = true,
        }
    }
}

/// Refuse a connection with a message (best effort, never blocks).
fn refuse(mut s: UnixStream, why: &str) {
    let _ = s.set_nonblocking(true);
    let _ = s.write(&HelperMsg::Error(why.into()).encode());
}

fn accept(l: &UnixListener, clients: &mut Vec<Client>) {
    loop {
        let s = match l.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                log!("accept: {e}");
                return;
            }
        };
        let cred = match sys::peer_cred(s.as_raw_fd()) {
            Ok(c) => c,
            Err(e) => {
                log!("peer credentials: {e}");
                continue;
            }
        };
        if !(UID_MIN..=UID_MAX).contains(&cred.uid) {
            log!("refusing uid {}: not a regular user", cred.uid);
            refuse(s, "not a regular user");
            continue;
        }
        if clients.len() >= MAX_CLIENTS {
            refuse(s, "too many clients");
            continue;
        }
        if s.set_nonblocking(true).is_err() {
            continue;
        }
        clients.push(Client {
            stream: s,
            cred,
            input: Vec::new(),
            roots: None,
            out: Vec::new(),
            since: Instant::now(),
            dead: false,
        });
    }
}

/// Resolve the batch's folders to paths (with bind-mount aliases).
fn resolve(pending: &HashSet<DirFid>, fs: &HashMap<u64, Fs>) -> BTreeSet<Vec<u8>> {
    let mut out = BTreeSet::new();
    for f in pending {
        let Some(m) = fs.get(&f.fsid) else { continue };
        let Ok(path) = sys::handle_path(&m.fd, &f.handle) else {
            continue; // gone since; its parent has its own event
        };
        if m.all.len() == 1 && m.primary.root == b"/" {
            out.insert(path);
            continue;
        }
        if let Some(fsp) = mounts::fs_path(&path, &m.primary) {
            let all: Vec<&Mount> = m.all.iter().collect();
            out.extend(mounts::aliases(&fsp, &all));
        }
    }
    out
}

fn deliver(clients: &mut [Client], dirs: &BTreeSet<Vec<u8>>, overflow: bool) {
    let mut cache = MetaCache::new(sys::meta);
    for c in clients.iter_mut().filter(|c| !c.dead) {
        let Some(roots) = c.roots.clone() else {
            continue;
        };
        if overflow {
            c.queue(&HelperMsg::Overflow.encode());
        }
        let visible: Vec<Vec<u8>> = dirs
            .iter()
            .filter(|d| cache.visible(d, &roots, &c.cred))
            .cloned()
            .collect();
        for f in proto::changed_frames(&visible) {
            c.queue(&f);
        }
        c.flush();
    }
}

fn run(args: Args) -> io::Result<()> {
    let fan = sys::fanotify_init(
        event::FAN_CLASS_NOTIF
            | event::FAN_CLOEXEC
            | event::FAN_NONBLOCK
            | event::FAN_REPORT_DFID_NAME,
        (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE) as u32,
    )?;
    let fs = mark_filesystems(&fan);
    let mut covered: Vec<u64> = fs.keys().copied().collect();
    covered.sort_unstable();
    let listener = listener(&args.socket)?;
    listener.set_nonblocking(true)?;
    privs::drop_privileges(&args.user)?;
    log!(
        "ready: {} filesystem(s), batch {} ms, privileges dropped",
        fs.len(),
        args.batch.as_millis()
    );
    let debug = std::env::var_os("TUDI_FANOTIFYD_DEBUG").is_some();

    let mut clients: Vec<Client> = Vec::new();
    let mut pending: HashSet<DirFid> = HashSet::new();
    let mut overflow = false;
    let mut deadline: Option<Instant> = None;
    // fanotify needs a buffer of at least one event; 64 KiB, 8-aligned.
    let mut buf = vec![0u64; 8192];
    let mut events = Vec::new();

    loop {
        let now = Instant::now();
        let mut wake = deadline;
        for c in &clients {
            if c.roots.is_none() {
                let t = c.since + SUBSCRIBE_TIMEOUT;
                wake = Some(wake.map_or(t, |w| w.min(t)));
            }
        }
        let timeout = wake.map_or(-1, |w| {
            w.saturating_duration_since(now).as_millis().min(60_000) as i32 + 1
        });
        let mut fds = vec![
            libc::pollfd {
                fd: fan.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        for c in &clients {
            let mut ev = libc::POLLIN;
            if !c.out.is_empty() {
                ev |= libc::POLLOUT;
            }
            fds.push(libc::pollfd {
                fd: c.stream.as_raw_fd(),
                events: ev,
                revents: 0,
            });
        }
        sys::poll(&mut fds, timeout)?;

        if fds[0].revents & libc::POLLIN != 0 {
            // Bounded number of reads per wakeup, so clients get served.
            for _ in 0..64 {
                let bytes = sys::as_bytes_mut(&mut buf);
                let n = match sys::read(fan.as_raw_fd(), bytes)? {
                    Some(n) if n > 0 => n,
                    _ => break,
                };
                events.clear();
                event::parse(&bytes[..n], &mut events);
                for e in events.drain(..) {
                    match e {
                        Event::Overflow => overflow = true,
                        Event::Fd(fd) => sys::close(fd),
                        Event::Changed(f) => {
                            if pending.len() < MAX_PENDING {
                                pending.insert(f);
                            } else {
                                overflow = true;
                            }
                        }
                    }
                }
                if deadline.is_none() && (overflow || !pending.is_empty()) {
                    deadline = Some(Instant::now() + args.batch);
                }
            }
        }
        if fds[1].revents & libc::POLLIN != 0 {
            accept(&listener, &mut clients);
        }
        for (i, c) in clients.iter_mut().enumerate() {
            let Some(p) = fds.get(2 + i) else { break };
            if p.fd != c.stream.as_raw_fd() {
                continue;
            }
            if p.revents & libc::POLLIN != 0 {
                c.read(&covered);
            }
            if p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                c.dead = true;
            }
            if p.revents & libc::POLLOUT != 0 {
                c.flush();
            }
            if c.roots.is_none() && c.since.elapsed() > SUBSCRIBE_TIMEOUT {
                c.dead = true;
            }
        }

        if deadline.is_some_and(|d| Instant::now() >= d) {
            let t = Instant::now();
            let dirs = resolve(&pending, &fs);
            deliver(&mut clients, &dirs, overflow);
            if overflow {
                log!("event queue overflow: clients told to reconcile");
            }
            if debug {
                log!(
                    "batch: {} handles, {} folders, {} clients, {:?}",
                    pending.len(),
                    dirs.len(),
                    clients.len(),
                    t.elapsed()
                );
            }
            pending.clear();
            pending.shrink_to(1024);
            overflow = false;
            deadline = None;
        }

        clients.retain(|c| {
            if c.dead && c.roots.is_some() {
                log!("uid {} disconnected", c.cred.uid);
            }
            !c.dead
        });
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log!("fatal: {e}");
            ExitCode::FAILURE
        }
    }
}
