//! The details list (right pane): a custom `gio::ListModel` over the sorted
//! entry indices of one listing (PLAN §5.3), and its `ListView`.
//!
//! Metadata arrives in batches after the rows are shown. Instead of
//! `items-changed` (which would drop the selection), the model tracks which
//! row widget is bound to which position and refills those widgets directly.
//!
//! Items are *positions*, not entries: a row object stands for "row i", and
//! the entry shown there is `order[i]`, looked up at bind time. A new order
//! (sorting, hidden files, another folder) therefore changes no items except
//! a tail when the row count changes; the bound widgets are refilled in
//! place. Replacing all items instead would make GTK destroy and recreate
//! its ~200 cached row widgets (measured: ~205 `setup` calls per re-sort).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib, pango};
use tudi_core::listing::meta::{MetaBatch, MetaStatus, MetaTable};
use tudi_core::listing::sort::{NameRank, SortKey, SortSpec};
use tudi_core::listing::{Kind, Listing};

use crate::format;
use crate::i18n::tr;
use crate::worker::Order;

// ---------------------------------------------------------------- row object

mod row_imp {
    use super::*;

    #[derive(Default)]
    pub struct PaneRow {
        pub position: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PaneRow {
        const NAME: &'static str = "TudiPaneRow";
        type Type = super::PaneRow;
    }

    impl ObjectImpl for PaneRow {}
}

glib::wrapper! {
    /// One row of the details list: just its position.
    pub struct PaneRow(ObjectSubclass<row_imp::PaneRow>);
}

impl PaneRow {
    fn new(position: u32) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().position.set(position);
        obj
    }

    pub fn position(&self) -> u32 {
        self.imp().position.get()
    }
}

// --------------------------------------------------------------------- model

/// Everything the pane shows for the current directory.
#[derive(Default)]
pub struct PaneData {
    pub generation: u64,
    pub listing: Option<Arc<Listing>>,
    pub rank: Option<Arc<NameRank>>,
    pub order: Vec<u32>,
    pub pos_of: Vec<u32>,
    pub metas: MetaTable,
    pub spec: SortSpec,
    pub meta_complete: bool,
    /// Unknown entry types were resolved since the order was computed.
    pub resort_pending: bool,
}

impl PaneData {
    pub fn kind(&self, entry: u32) -> Kind {
        let l = self.listing.as_ref().expect("listing");
        self.metas.kind(l, entry as usize)
    }

    pub fn name(&self, entry: u32) -> &[u8] {
        self.listing.as_ref().expect("listing").name(entry as usize)
    }

    pub fn is_dir(&self, entry: u32) -> bool {
        self.kind(entry) == Kind::Dir
    }
}

mod model_imp {
    use super::*;

    #[derive(Default)]
    pub struct PaneModel {
        pub data: RefCell<PaneData>,
        pub live: RefCell<HashMap<u32, glib::WeakRef<PaneRow>>>,
        /// Row widgets currently bound, by position.
        pub bound: RefCell<HashMap<u32, glib::WeakRef<gtk::Box>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PaneModel {
        const NAME: &'static str = "TudiPaneModel";
        type Type = super::PaneModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for PaneModel {}

    impl ListModelImpl for PaneModel {
        fn item_type(&self) -> glib::Type {
            PaneRow::static_type()
        }

        fn n_items(&self) -> u32 {
            self.data.borrow().order.len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            if position as usize >= self.data.borrow().order.len() {
                return None;
            }
            let mut live = self.live.borrow_mut();
            if let Some(obj) = live.get(&position).and_then(|w| w.upgrade()) {
                return Some(obj.upcast());
            }
            // GTK holds only visible items plus a margin; prune dead weak
            // refs when the cache grows.
            if live.len() > 2048 {
                live.retain(|_, w| w.upgrade().is_some());
            }
            let obj = PaneRow::new(position);
            live.insert(position, obj.downgrade());
            Some(obj.upcast())
        }
    }
}

glib::wrapper! {
    pub struct PaneModel(ObjectSubclass<model_imp::PaneModel>)
        @implements gio::ListModel;
}

impl Default for PaneModel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PaneModel {
    pub fn data(&self) -> std::cell::Ref<'_, PaneData> {
        self.imp().data.borrow()
    }

    /// Show a new directory (replaces all rows).
    pub fn set_listing(
        &self,
        generation: u64,
        listing: Arc<Listing>,
        rank: Arc<NameRank>,
        order: Order,
        spec: SortSpec,
    ) {
        let removed = self.n_items();
        {
            let mut d = self.imp().data.borrow_mut();
            d.metas = MetaTable::new(listing.len());
            d.generation = generation;
            d.listing = Some(listing);
            d.rank = Some(rank);
            d.order = order.order;
            d.pos_of = order.pos_of;
            d.spec = spec;
            d.meta_complete = false;
            d.resort_pending = false;
        }
        self.reshape(removed);
    }

    /// Show nothing (e.g. the directory could not be listed).
    pub fn clear(&self, generation: u64) {
        let removed = self.n_items();
        *self.imp().data.borrow_mut() = PaneData {
            generation,
            ..Default::default()
        };
        self.reshape(removed);
    }

    /// Replace the order (same listing). Returns false if stale.
    pub fn set_order(&self, generation: u64, order: Order, spec: SortSpec) -> bool {
        let removed = self.n_items();
        {
            let mut d = self.imp().data.borrow_mut();
            if d.generation != generation {
                return false;
            }
            d.order = order.order;
            d.pos_of = order.pos_of;
            d.spec = spec;
            d.resort_pending = false;
        }
        self.reshape(removed);
        true
    }

    /// The rows now show a new order. Only a changed row count is reported
    /// to GTK (as a change at the tail); bound widgets are refilled.
    fn reshape(&self, old_len: u32) {
        let new_len = self.n_items();
        let common = old_len.min(new_len);
        if old_len != new_len {
            self.items_changed(common, old_len - common, new_len - common);
        }
        let rows: Vec<(u32, gtk::Box)> = self
            .imp()
            .bound
            .borrow()
            .iter()
            .filter(|&(&pos, _)| pos < common)
            .filter_map(|(&pos, w)| Some((pos, w.upgrade()?)))
            .collect();
        for (pos, row) in rows {
            fill(&row, self, pos);
        }
    }

    /// Apply a metadata batch and refresh bound rows it touches. Returns
    /// whether a re-sort is due: once metadata is complete, if unknown types
    /// were resolved or the order depends on size/time.
    pub fn apply_meta(&self, batch: &MetaBatch) -> bool {
        let resort = {
            let mut d = self.imp().data.borrow_mut();
            if d.generation != batch.generation {
                return false;
            }
            let Some(listing) = d.listing.clone() else {
                return false;
            };
            if d.metas.apply(&listing, batch) {
                d.resort_pending = true;
            }
            if batch.complete {
                d.meta_complete = true;
            }
            batch.complete && (d.resort_pending || d.spec.key != SortKey::Name)
        };
        let rows: Vec<(u32, gtk::Box)> = {
            let bound = self.imp().bound.borrow();
            let d = self.data();
            batch
                .items
                .iter()
                .filter_map(|&(e, _)| {
                    let pos = *d.pos_of.get(e as usize)?;
                    Some((pos, bound.get(&pos)?.upgrade()?))
                })
                .collect()
        };
        for (pos, row) in rows {
            fill(&row, self, pos);
        }
        resort
    }
}

// ---------------------------------------------------------------------- view

const COL_SIZE_CHARS: i32 = 9;
const COL_DATE_CHARS: i32 = 11;

fn fixed_label(chars: i32) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(1.0);
    l.set_width_chars(chars);
    l.set_max_width_chars(chars);
    l.add_css_class("dim-label");
    l.add_css_class("numeric");
    l
}

/// Row widget: icon, name, size, modified.
fn row_widget() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let icon = crate::icons::FileIcon::default();
    let name = gtk::Label::new(None);
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.set_ellipsize(pango::EllipsizeMode::Middle);
    row.append(&icon);
    row.append(&name);
    row.append(&fixed_label(COL_SIZE_CHARS));
    row.append(&fixed_label(COL_DATE_CHARS));
    row
}

struct RowParts {
    icon: crate::icons::FileIcon,
    name: gtk::Label,
    size: gtk::Label,
    date: gtk::Label,
}

fn parts(row: &gtk::Box) -> RowParts {
    let icon = row
        .first_child()
        .and_downcast::<crate::icons::FileIcon>()
        .unwrap();
    let name = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
    let size = name.next_sibling().and_downcast::<gtk::Label>().unwrap();
    let date = size.next_sibling().and_downcast::<gtk::Label>().unwrap();
    RowParts {
        icon,
        name,
        size,
        date,
    }
}

thread_local! {
    static NOW: RefCell<Option<(std::time::Instant, glib::DateTime)>> = const { RefCell::new(None) };
}

/// Local "now", refreshed at most once a second (binds are hot).
fn now() -> glib::DateTime {
    NOW.with(|n| {
        let mut n = n.borrow_mut();
        match &*n {
            Some((t, dt)) if t.elapsed().as_secs() < 1 => dt.clone(),
            _ => {
                let dt = glib::DateTime::now_local().expect("local time");
                *n = Some((std::time::Instant::now(), dt.clone()));
                dt
            }
        }
    })
}

fn fill(row: &gtk::Box, model: &PaneModel, position: u32) {
    let d = model.data();
    let Some(&entry) = d.order.get(position as usize) else {
        return;
    };
    let p = parts(row);
    let name = d.name(entry);
    let dir = d.is_dir(entry);
    let m = d.metas.get(entry as usize);
    // Executable regular files without an extension (binaries, scripts in
    // bin/) get an "executable" icon. Files with an extension keep their
    // type icon: some filesystems (exFAT, NTFS) mark every file executable.
    let executable = m.status == MetaStatus::Ok
        && m.mode & 0o170000 == 0o100000
        && m.mode & 0o111 != 0
        && !name[1..].contains(&b'.');
    use crate::icons::Kind as Icon;
    let link = d.listing.as_ref().map(|l| l.kind(entry as usize)) == Some(Kind::Symlink);
    p.icon.set(
        if dir {
            Icon::Folder
        } else if executable {
            Icon::Executable
        } else {
            Icon::File(crate::icons::category(name))
        },
        link,
    );
    p.name.set_text(&format::name(name));
    match m.status {
        MetaStatus::Ok => {
            p.size.set_text(&if dir {
                "—".into()
            } else {
                format::size(m.size)
            });
            p.date.set_text(&format::date(m.mtime, &now()));
        }
        MetaStatus::Pending | MetaStatus::Failed => {
            p.size.set_text("");
            p.date.set_text("");
        }
    }
}

/// Entries whose rows were bound while their metadata was pending: the
/// visible rows, fetched first.
pub type Wanted = std::rc::Rc<RefCell<Vec<u32>>>;

/// Build the details `ListView`.
/// Right-click handler: the row position (or `None` for empty space), the
/// widget clicked, and the click coordinates in it.
pub type MenuFn = Rc<dyn Fn(Option<u32>, &gtk::Widget, f64, f64)>;

pub fn build_view(
    model: &PaneModel,
    selection: &gtk::MultiSelection,
    wanted: &Wanted,
    on_menu: MenuFn,
) -> gtk::ListView {
    let factory = gtk::SignalListItemFactory::new();
    let row_menu = on_menu.clone();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = row_widget();
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let (f, wi, wr) = (row_menu.clone(), item.downgrade(), row.downgrade());
        click.connect_pressed(move |g, _, x, y| {
            if let (Some(item), Some(row)) = (wi.upgrade(), wr.upgrade()) {
                g.set_state(gtk::EventSequenceState::Claimed);
                f(Some(item.position()), row.upcast_ref(), x, y);
            }
        });
        row.add_controller(click);
        item.set_child(Some(&row));
        crate::bench::ROW_WORK.with(|r| {
            let mut w = r.get();
            w.setups += 1;
            r.set(w);
        });
    });
    let (m, w) = (model.clone(), wanted.clone());
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let pos = item.item().and_downcast::<PaneRow>().unwrap().position();
        let t0 = std::time::Instant::now();
        fill(&row, &m, pos);
        crate::bench::ROW_WORK.with(|r| {
            let mut w = r.get();
            w.binds += 1;
            w.bind_us += t0.elapsed().as_micros() as u64;
            r.set(w);
        });
        {
            let d = m.data();
            if let Some(&entry) = d.order.get(pos as usize)
                && d.metas.get(entry as usize).status == MetaStatus::Pending
            {
                w.borrow_mut().push(entry);
            }
        }
        m.imp().bound.borrow_mut().insert(pos, row.downgrade());
    });
    let m = model.clone();
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(obj) = item.item().and_downcast::<PaneRow>() else {
            return;
        };
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let mut bound = m.imp().bound.borrow_mut();
        if bound
            .get(&obj.position())
            .and_then(|w| w.upgrade())
            .as_ref()
            == Some(&row)
        {
            bound.remove(&obj.position());
        }
    });
    let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
    view.add_css_class("tudi-details");
    // Right-click outside the rows (rows claim their own clicks).
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    click.connect_pressed(move |g, _, x, y| {
        if let Some(w) = g.widget() {
            on_menu(None, &w, x, y);
        }
    });
    view.add_controller(click);
    view
}

/// Column header row matching the row layout. Returns the box and the three
/// buttons (name, size, modified).
pub fn build_header() -> (gtk::Box, [gtk::Button; 3]) {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class("tudi-column-header");
    // Widths in characters, like the row labels, so the columns stay
    // aligned at any zoom level.
    let mk = |label: &str, xalign: f32, chars: i32| {
        let l = gtk::Label::new(Some(label));
        l.set_xalign(xalign);
        if chars > 0 {
            l.set_width_chars(chars);
        }
        let b = gtk::Button::builder().child(&l).build();
        b.add_css_class("flat");
        b
    };
    let name = mk(&tr("Name"), 0.0, 0);
    name.set_hexpand(true);
    let size = mk(&tr("Size"), 1.0, COL_SIZE_CHARS);
    let date = mk(&tr("Modified"), 1.0, COL_DATE_CHARS);
    header.append(&name);
    header.append(&size);
    header.append(&date);
    (header, [name, size, date])
}
