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

    /// Ask every window to reload the history (after a change).
    pub fn history_changed(&self) {
        for w in self.each() {
            self.history.query(w.worker.reply_sender());
        }
    }
}
