//! Client of the optional fanotify helper (`tudi-fanotifyd`, M7).
//!
//! A background thread connects to the helper's socket, subscribes with
//! the roots that live on filesystems with file handles, and turns the
//! helper's messages into [`Event`]s for the updater. When the helper is
//! not installed or goes away, the thread retries with a backoff (up to a
//! minute) and the daemon keeps using its fallback change sources
//! (inotify on frequent folders, periodic reconcile, listing hints).

use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tudi_fanotifyd::proto::{self, HelperMsg};

pub enum Event {
    /// Connected; these roots now get live events (since `at`).
    Up {
        covered: Vec<PathBuf>,
        at: Instant,
    },
    Changed(Vec<PathBuf>),
    /// Events were lost: reconcile the covered roots.
    Overflow,
    Down,
}

/// What the rest of the daemon may ask about the helper.
#[derive(Default)]
pub struct State {
    covered: Mutex<Vec<PathBuf>>,
}

impl State {
    /// Roots currently receiving live events.
    pub fn covered(&self) -> Vec<PathBuf> {
        self.covered.lock().unwrap().clone()
    }

    /// Is `dir` inside a root with live events?
    pub fn is_live(&self, dir: &Path) -> bool {
        self.covered
            .lock()
            .unwrap()
            .iter()
            .any(|r| dir.starts_with(r))
    }
}

pub fn socket_path() -> PathBuf {
    std::env::var_os("TUDI_FANOTIFY_SOCKET")
        .map_or_else(|| PathBuf::from(proto::SOCKET_PATH), PathBuf::from)
}

/// Run one connection until it ends.
fn session(
    sock: &Path,
    roots: &[PathBuf],
    state: &State,
    on: &impl Fn(Event),
) -> std::io::Result<()> {
    let mut s = UnixStream::connect(sock)?;
    let bytes: Vec<Vec<u8>> = roots
        .iter()
        .map(|r| r.as_os_str().as_bytes().to_vec())
        .collect();
    proto::write_subscribe(&mut s, &bytes)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let ids = match proto::read_helper_msg(&mut s)? {
        HelperMsg::Covered(ids) => ids,
        HelperMsg::Error(e) => return Err(std::io::Error::other(e)),
        _ => return Err(std::io::ErrorKind::InvalidData.into()),
    };
    s.set_read_timeout(None)?;
    let covered: Vec<PathBuf> = roots
        .iter()
        .filter(|r| tudi_fanotifyd::sys::fsid_path(r).is_ok_and(|id| ids.contains(&id)))
        .cloned()
        .collect();
    tracing::info!(
        "fanotify helper connected; live events for {} of {} root(s)",
        covered.len(),
        roots.len()
    );
    *state.covered.lock().unwrap() = covered.clone();
    on(Event::Up {
        covered,
        at: Instant::now(),
    });
    loop {
        match proto::read_helper_msg(&mut s)? {
            HelperMsg::Changed(dirs) => on(Event::Changed(
                dirs.into_iter()
                    .map(|d| PathBuf::from(std::ffi::OsString::from_vec(d)))
                    .collect(),
            )),
            HelperMsg::Overflow => {
                tracing::info!("fanotify helper lost events; reconciling");
                on(Event::Overflow);
            }
            HelperMsg::Error(e) => return Err(std::io::Error::other(e)),
            HelperMsg::Covered(_) => {}
        }
    }
}

/// Start the client thread. `roots`: roots on filesystems that support
/// fanotify.
pub fn spawn(
    roots: Vec<PathBuf>,
    state: std::sync::Arc<State>,
    stop: std::sync::Arc<AtomicBool>,
    on: impl Fn(Event) + Send + 'static,
) {
    if roots.is_empty() {
        return;
    }
    let sock = socket_path();
    let _ = std::thread::Builder::new()
        .name("fanotify".into())
        .spawn(move || {
            let mut backoff = Duration::from_secs(1);
            let mut said_missing = false;
            while !stop.load(Ordering::Relaxed) {
                let t = Instant::now();
                match session(&sock, &roots, &state, &on) {
                    Ok(()) => {}
                    Err(e) => {
                        let up = !state.covered.lock().unwrap().is_empty();
                        if up {
                            tracing::warn!("fanotify helper disconnected: {e}");
                        } else if !said_missing {
                            tracing::info!(
                                "no fanotify helper at {} ({e}); using inotify and reconcile",
                                sock.display()
                            );
                            said_missing = true;
                        }
                    }
                }
                if !std::mem::take(&mut *state.covered.lock().unwrap()).is_empty() {
                    on(Event::Down);
                }
                // A connection that lasted resets the backoff.
                if t.elapsed() > Duration::from_secs(60) {
                    backoff = Duration::from_secs(1);
                }
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        });
}
