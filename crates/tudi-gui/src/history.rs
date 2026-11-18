//! Folder history (ADR-016): a thread owning the frecency database, and
//! the "Frequent" view shown in the right pane.
//!
//! The database is only touched on the `tudi-history` thread. Windows send
//! requests and receive [`Reply::History`] on their own reply channel.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib, pango};
use tudi_core::frecency::{Entry, ExcludeRules, Exclusion, Frecency};

use crate::i18n::{fill, ntr, tr};
use crate::worker::Reply;

/// Most folders shown in the history view.
pub const SHOWN: usize = 300;

enum Req {
    Visit(PathBuf),
    Exclude(PathBuf, bool),
    Unexclude(PathBuf),
    Forget(PathBuf),
    /// Send the current list (and exclusions) to this window.
    Query(async_channel::Sender<Reply>),
}

/// What the history thread sends back.
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

impl History {
    /// Start the thread. Errors (unwritable database, bad patterns) are
    /// logged; history then stays empty.
    pub fn start(patterns: Vec<String>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Req>();
        std::thread::Builder::new()
            .name("tudi-history".into())
            .spawn(move || {
                let rules = ExcludeRules::new(&patterns).unwrap_or_else(|e| {
                    tracing::warn!("{e}");
                    ExcludeRules::default()
                });
                let store = tudi_core::frecency::default_path()
                    .map(|p| Frecency::open(&p, rules.clone()))
                    .unwrap_or_else(|| Frecency::open_in_memory(rules));
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

    pub fn visit(&self, p: PathBuf) {
        let _ = self.tx.send(Req::Visit(p));
    }

    pub fn exclude(&self, p: PathBuf, recursive: bool) {
        let _ = self.tx.send(Req::Exclude(p, recursive));
    }

    pub fn unexclude(&self, p: PathBuf) {
        let _ = self.tx.send(Req::Unexclude(p));
    }

    pub fn forget(&self, p: PathBuf) {
        let _ = self.tx.send(Req::Forget(p));
    }

    pub fn query(&self, reply: async_channel::Sender<Reply>) {
        let _ = self.tx.send(Req::Query(reply));
    }
}

// --------------------------------------------------------------- the view

/// One row of the history view: an index into the shown rows.
mod row_imp {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    pub struct HistoryRow {
        pub index: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HistoryRow {
        const NAME: &'static str = "TudiHistoryRow";
        type Type = super::HistoryRow;
    }

    impl ObjectImpl for HistoryRow {}
}

glib::wrapper! {
    pub struct HistoryRow(ObjectSubclass<row_imp::HistoryRow>);
}

/// A shown row: a visited folder, or an exclusion.
#[derive(Clone, Debug)]
pub enum Item {
    Visited(Entry),
    Excluded(Exclusion),
}

impl Item {
    pub fn path(&self) -> &std::path::Path {
        match self {
            Item::Visited(e) => &e.path,
            Item::Excluded(x) => &x.path,
        }
    }
}

mod model_imp {
    use super::*;

    #[derive(Default)]
    pub struct HistoryModel {
        pub items: RefCell<Vec<Item>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HistoryModel {
        const NAME: &'static str = "TudiHistoryModel";
        type Type = super::HistoryModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for HistoryModel {}

    impl ListModelImpl for HistoryModel {
        fn item_type(&self) -> glib::Type {
            HistoryRow::static_type()
        }

        fn n_items(&self) -> u32 {
            self.items.borrow().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            if position as usize >= self.items.borrow().len() {
                return None;
            }
            let obj: HistoryRow = glib::Object::new();
            obj.imp().index.set(position);
            Some(obj.upcast())
        }
    }
}

glib::wrapper! {
    /// At most [`SHOWN`] rows, so a plain replace is fine here.
    pub struct HistoryModel(ObjectSubclass<model_imp::HistoryModel>)
        @implements gio::ListModel;
}

impl Default for HistoryModel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl HistoryModel {
    pub fn set(&self, items: Vec<Item>) {
        let removed = self.n_items();
        let added = items.len() as u32;
        *self.imp().items.borrow_mut() = items;
        self.items_changed(0, removed, added);
    }

    pub fn get(&self, i: u32) -> Option<Item> {
        self.imp().items.borrow().get(i as usize).cloned()
    }
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
        glib::DateTime::from_unix_local(secs)
            .ok()
            .and_then(|d| d.format("%Y-%m-%d").ok())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

fn label(xalign: f32, dim: bool) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(xalign);
    if dim {
        l.add_css_class("dim-label");
    }
    l
}

/// Build the history list view.
/// `on_menu(position, row widget, x, y)` is called on a right-click.
pub fn build_view(
    model: &HistoryModel,
    on_menu: impl Fn(u32, &gtk::Widget, f64, f64) + 'static,
) -> (gtk::ListView, gtk::SingleSelection) {
    let on_menu = Rc::new(on_menu);
    let sel = gtk::SingleSelection::new(Some(model.clone()));
    sel.set_autoselect(false);
    sel.set_can_unselect(true);
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let (f, weak_item, weak_row) = (on_menu.clone(), item.downgrade(), row.downgrade());
        click.connect_pressed(move |g, _, x, y| {
            if let (Some(item), Some(row)) = (weak_item.upgrade(), weak_row.upgrade()) {
                g.set_state(gtk::EventSequenceState::Claimed);
                f(item.position(), row.upcast_ref(), x, y);
            }
        });
        row.add_controller(click);
        let icon = gtk::Image::from_icon_name("folder-symbolic");
        icon.add_css_class("tudi-icon");
        let name = label(0.0, false);
        let parent = label(0.0, true);
        parent.set_hexpand(true);
        parent.set_ellipsize(pango::EllipsizeMode::Start);
        let count = label(1.0, true);
        count.set_width_chars(8);
        count.add_css_class("numeric");
        let last = label(1.0, true);
        last.set_width_chars(14);
        for w in [
            icon.upcast_ref::<gtk::Widget>(),
            name.upcast_ref(),
            parent.upcast_ref(),
            count.upcast_ref(),
            last.upcast_ref(),
        ] {
            row.append(w);
        }
        item.set_child(Some(&row));
    });
    let m = model.clone();
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let idx = item
            .item()
            .and_downcast::<HistoryRow>()
            .unwrap()
            .imp()
            .index
            .get();
        let Some(it) = m.get(idx) else { return };
        let mut w = row.first_child().unwrap().next_sibling();
        let mut next = || {
            let cur = w.clone().and_downcast::<gtk::Label>().unwrap();
            w = cur.next_sibling();
            cur
        };
        let (name, parent, count, last) = (next(), next(), next(), next());
        let p = it.path();
        name.set_text(
            &p.file_name()
                .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy()),
        );
        parent.set_text(
            &p.parent()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        match &it {
            Item::Visited(e) => {
                count.set_text(&fill(&ntr("{} open", "{} opens", e.count), &[&e.count]));
                last.set_text(&when(e.last));
            }
            Item::Excluded(x) => {
                count.set_text("");
                last.set_text(&if x.recursive {
                    tr("with subfolders")
                } else {
                    tr("this folder only")
                });
            }
        }
    });
    let view = gtk::ListView::new(Some(sel.clone()), Some(factory));
    view.add_css_class("tudi-details");
    (view, sel)
}

/// Shared state of a window's history view.
#[derive(Default)]
pub struct ViewState {
    pub snapshot: Option<Snapshot>,
    /// Showing exclusions instead of visited folders.
    pub show_excluded: bool,
}

pub type ViewStateRc = Rc<RefCell<ViewState>>;
