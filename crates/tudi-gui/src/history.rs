//! Folder history (ADR-016): a thread owning the frecency database.
//!
//! The database is only touched on the `tudi-history` thread. Windows send
//! requests and receive [`Reply::History`] on their own reply channel; the
//! bookmarks bar shows the most used folders, the settings dialog the
//! exclusions.

use std::path::PathBuf;

use tudi_core::frecency::{Entry, ExcludeRules, Exclusion, Frecency};

use crate::i18n::{fill, ntr, tr};
use crate::worker::Reply;

/// Most folders sent to windows (bar buttons plus the "»" menu).
pub const SHOWN: usize = 30;

enum Req {
    Visit(PathBuf),
    Exclude(PathBuf, bool),
    Unexclude(PathBuf),
    Forget(PathBuf),
    Clear,
    SetRules(Vec<String>),
    /// Import autojump / zoxide weights (first run, or forced).
    Import(bool),
    /// Send the current list (and exclusions) to this window.
    Query(async_channel::Sender<Reply>),
}

/// What the history thread sends back.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    pub exclusions: Vec<Exclusion>,
}

/// Handle to the history thread (one per process).
#[derive(Clone)]
pub struct History {
    tx: std::sync::mpsc::Sender<Req>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn rules(patterns: &[String]) -> ExcludeRules {
    ExcludeRules::new(patterns).unwrap_or_else(|e| {
        tracing::warn!("{e}");
        ExcludeRules::default()
    })
}

impl History {
    /// Start the thread. Errors (unwritable database, bad patterns) are
    /// logged; history then stays empty.
    pub fn start(patterns: Vec<String>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Req>();
        std::thread::Builder::new()
            .name("tudi-history".into())
            .spawn(move || {
                let r = rules(&patterns);
                let store = tudi_core::frecency::default_path()
                    .map(|p| Frecency::open(&p, r.clone()))
                    .unwrap_or_else(|| Frecency::open_in_memory(r));
                let mut store = match store {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("history disabled: {e}");
                        return;
                    }
                };
                while let Ok(req) = rx.recv() {
                    let r = match req {
                        Req::Visit(p) => store.visit(&p, now()).map(|_| ()),
                        Req::Exclude(p, rec) => store.exclude(&p, rec),
                        Req::Unexclude(p) => store.unexclude(&p).map(|_| ()),
                        Req::Forget(p) => store.forget(&p),
                        Req::Clear => store.clear(),
                        Req::SetRules(p) => {
                            store.set_rules(rules(&p));
                            Ok(())
                        }
                        Req::Import(force) => import_history(&mut store, force),
                        Req::Query(reply) => store.top(SHOWN, now()).and_then(|entries| {
                            let exclusions = store.exclusions()?;
                            let _ = reply.send_blocking(Reply::History(Snapshot {
                                entries,
                                exclusions,
                            }));
                            Ok(())
                        }),
                    };
                    if let Err(e) = r {
                        tracing::warn!("{e}");
                    }
                }
            })
            .expect("spawn history thread");
        Self { tx }
    }

    fn send(&self, r: Req) {
        let _ = self.tx.send(r);
    }

    pub fn visit(&self, p: PathBuf) {
        self.send(Req::Visit(p));
    }

    pub fn exclude(&self, p: PathBuf, recursive: bool) {
        self.send(Req::Exclude(p, recursive));
    }

    pub fn unexclude(&self, p: PathBuf) {
        self.send(Req::Unexclude(p));
    }

    pub fn forget(&self, p: PathBuf) {
        self.send(Req::Forget(p));
    }

    pub fn clear(&self) {
        self.send(Req::Clear);
    }

    pub fn set_rules(&self, patterns: Vec<String>) {
        self.send(Req::SetRules(patterns));
    }

    /// Import other tools' folder weights, once (or again with `force`).
    pub fn import(&self, force: bool) {
        self.send(Req::Import(force));
    }

    pub fn query(&self, reply: async_channel::Sender<Reply>) {
        self.send(Req::Query(reply));
    }
}

/// Marker files in the state directory: an import runs once per source.
pub fn import_marker(name: &str) -> Option<PathBuf> {
    tudi_core::config::state_dir().map(|d| d.join(name))
}

/// Import from autojump and zoxide (each optional) into the history.
fn import_history(
    store: &mut Frecency,
    force: bool,
) -> Result<(), tudi_core::frecency::FrecencyError> {
    use tudi_core::import::{autojump_file, parse_autojump, parse_zoxide, to_visits};
    let Some(marker) = import_marker("imported-history") else {
        return Ok(());
    };
    if marker.exists() && !force {
        return Ok(());
    }
    let home = gtk::glib::home_dir();
    let mut items = std::fs::read_to_string(autojump_file(&home))
        .map(|t| parse_autojump(&t))
        .unwrap_or_default();
    if let Ok(out) = std::process::Command::new("zoxide")
        .args(["query", "-ls"])
        .stderr(std::process::Stdio::null())
        .output()
        && out.status.success()
    {
        items.extend(parse_zoxide(&String::from_utf8_lossy(&out.stdout)));
    }
    let n = store.import(&to_visits(&items), now())?;
    if n > 0 {
        tracing::info!("imported {n} folders from autojump/zoxide");
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, "");
    Ok(())
}

/// "3 min ago", "2 days ago", or a date.
pub fn when(secs: i64) -> String {
    let age = (now() - secs).max(0);
    if age < 60 {
        tr("just now")
    } else if age < 3_600 {
        let n = (age / 60) as u64;
        fill(&ntr("{} minute ago", "{} minutes ago", n), &[&n])
    } else if age < 86_400 {
        let n = (age / 3_600) as u64;
        fill(&ntr("{} hour ago", "{} hours ago", n), &[&n])
    } else if age < 30 * 86_400 {
        let n = (age / 86_400) as u64;
        fill(&ntr("{} day ago", "{} days ago", n), &[&n])
    } else {
        gtk::glib::DateTime::from_unix_local(secs)
            .ok()
            .and_then(|d| d.format("%Y-%m-%d").ok())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}
