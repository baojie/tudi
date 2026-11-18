//! State shared by all windows of the process: config, bookmarks, the
//! history thread, and the open browsers (to broadcast changes).

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use tudi_core::bookmarks::Bookmarks;
use tudi_core::config::Config;
use tudi_core::launchers::Launchers;

use crate::browser::Browser;
use crate::history::History;

pub struct Shared {
    pub config: RefCell<Config>,
    pub bookmarks: RefCell<Bookmarks>,
    pub launchers: RefCell<Launchers>,
    pub history: History,
    pub browsers: RefCell<Vec<Weak<Browser>>>,
}

impl Shared {
    pub fn new(config: Config, bookmarks: Bookmarks, launchers: Launchers) -> Rc<Self> {
        let history = History::start(config.history.exclude.clone());
        Rc::new(Self {
            config: RefCell::new(config),
            bookmarks: RefCell::new(bookmarks),
            launchers: RefCell::new(launchers),
            history,
            browsers: RefCell::default(),
        })
    }

    /// Live browsers.
    pub fn each(&self) -> Vec<Rc<Browser>> {
        let mut v = self.browsers.borrow_mut();
        v.retain(|w| w.strong_count() > 0);
        v.iter().filter_map(Weak::upgrade).collect()
    }

    /// Write `config.toml` (keeping comments) on a thread.
    pub fn save_config(&self) {
        let c = self.config.borrow().clone();
        std::thread::spawn(move || {
            if let Some(p) = tudi_core::config::config_dir().map(|d| d.join("config.toml"))
                && let Err(e) = c.save_preserving(&p)
            {
                tracing::warn!("cannot save config: {e}");
            }
        });
    }

    /// Write `bookmarks.toml` on a thread and refresh every window's bar.
    pub fn bookmarks_changed(&self) {
        let b = self.bookmarks.borrow().clone();
        std::thread::spawn(move || {
            if let Some(p) = Bookmarks::path()
                && let Err(e) = b.save(&p)
            {
                tracing::warn!("cannot save bookmarks: {e}");
            }
        });
        for w in self.each() {
            w.rebuild_bar();
        }
    }

    /// Write `launchers.toml` on a thread and refresh every window's bar.
    pub fn launchers_changed(&self) {
        let l = self.launchers.borrow().clone();
        std::thread::spawn(move || {
            if let Some(p) = Launchers::path()
                && let Err(e) = l.save(&p)
            {
                tracing::warn!("cannot save launchers: {e}");
            }
        });
        for w in self.each() {
            w.rebuild_bar();
        }
    }

    /// First-run imports (or forced with `--reimport`): GTK bookmarks into
    /// the bar, autojump / zoxide weights into the history. Files are read
    /// on threads; the bookmarks are merged here on the main thread.
    pub fn run_imports(self: &Rc<Self>, force: bool) {
        self.history.import(force);
        let (tx, rx) = async_channel::bounded::<Vec<(std::path::PathBuf, Option<String>)>>(1);
        std::thread::spawn(move || {
            let Some(marker) = crate::history::import_marker("imported-bookmarks") else {
                return;
            };
            if marker.exists() && !force {
                return;
            }
            let file = tudi_core::import::gtk_bookmarks_file(&gtk::glib::home_dir());
            let found = std::fs::read_to_string(file)
                .map(|t| tudi_core::import::parse_gtk_bookmarks(&t))
                .unwrap_or_default();
            if let Some(dir) = marker.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&marker, "");
            let _ = tx.send_blocking(found);
        });
        let me = Rc::downgrade(self);
        gtk::glib::spawn_future_local(async move {
            let Ok(found) = rx.recv().await else { return };
            let Some(me) = me.upgrade() else { return };
            let mut added = false;
            {
                let mut bm = me.bookmarks.borrow_mut();
                for (path, label) in found {
                    if bm.add(&path) {
                        added = true;
                        if let Some(l) = label {
                            let i = bm.items.len() - 1;
                            bm.rename(i, &l);
                        }
                    }
                }
            }
            if added {
                me.bookmarks_changed();
            }
            me.history_changed();
        });
    }

    /// Ask every window to reload the history (after a change).
    pub fn history_changed(&self) {
        for w in self.each() {
            self.history.query(w.worker.reply_sender());
        }
    }
}
