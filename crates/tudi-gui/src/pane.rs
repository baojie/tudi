//! The details list (right pane): a custom `gio::ListModel` over the sorted
//! entry indices of one listing (PLAN §5.3), and its `ListView`.
//!
//! Metadata arrives in batches after the rows are shown. Instead of
//! `items-changed` (which would drop the selection), the model tracks which
//! row widget is bound to which entry and refills those widgets directly.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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
        pub entry: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PaneRow {
        const NAME: &'static str = "TudiPaneRow";
        type Type = super::PaneRow;
    }

    impl ObjectImpl for PaneRow {}
}

glib::wrapper! {
    /// One row of the details list: just an entry index.
    pub struct PaneRow(ObjectSubclass<row_imp::PaneRow>);
}

impl PaneRow {
    fn new(entry: u32) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().entry.set(entry);
        obj
    }

    pub fn entry(&self) -> u32 {
        self.imp().entry.get()
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
        /// Row widgets currently bound, by entry.
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
            let entry = *self.data.borrow().order.get(position as usize)?;
            let mut live = self.live.borrow_mut();
            if let Some(obj) = live.get(&entry).and_then(|w| w.upgrade()) {
                return Some(obj.upcast());
            }
            // GTK holds only visible items plus a margin; prune dead weak
            // refs when the cache grows.
            if live.len() > 2048 {
                live.retain(|_, w| w.upgrade().is_some());
            }
            let obj = PaneRow::new(entry);
            live.insert(entry, obj.downgrade());
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
        self.imp().live.borrow_mut().clear();
        self.imp().bound.borrow_mut().clear();
        self.items_changed(0, removed, self.n_items());
    }

    /// Show nothing (e.g. the directory could not be listed).
    pub fn clear(&self, generation: u64) {
        let removed = self.n_items();
        *self.imp().data.borrow_mut() = PaneData {
            generation,
            ..Default::default()
        };
        self.imp().live.borrow_mut().clear();
        self.imp().bound.borrow_mut().clear();
        self.items_changed(0, removed, 0);
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
        self.imp().live.borrow_mut().clear();
        self.imp().bound.borrow_mut().clear();
        self.items_changed(0, removed, self.n_items());
        true
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
            batch
                .items
                .iter()
                .filter_map(|&(e, _)| Some((e, bound.get(&e)?.upgrade()?)))
                .collect()
        };
        for (entry, row) in rows {
            fill(&row, self, entry);
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
    let icon = gtk::Image::new();
    icon.set_pixel_size(16);
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
    icon: gtk::Image,
    name: gtk::Label,
    size: gtk::Label,
    date: gtk::Label,
}

fn parts(row: &gtk::Box) -> RowParts {
    let icon = row.first_child().and_downcast::<gtk::Image>().unwrap();
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

fn fill(row: &gtk::Box, model: &PaneModel, entry: u32) {
    let d = model.data();
    if d.listing.is_none() {
        return;
    }
    let p = parts(row);
    let name = d.name(entry);
    let dir = d.is_dir(entry);
    p.icon.set_from_gicon(&if dir {
        format::folder_icon()
    } else {
        format::file_icon(name)
    });
    p.name.set_text(&format::name(name));
    let m = d.metas.get(entry as usize);
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
pub fn build_view(
    model: &PaneModel,
    selection: &gtk::MultiSelection,
    wanted: &Wanted,
) -> gtk::ListView {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        item.set_child(Some(&row_widget()));
    });
    let (m, w) = (model.clone(), wanted.clone());
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let entry = item.item().and_downcast::<PaneRow>().unwrap().entry();
        fill(&row, &m, entry);
        if m.data().metas.get(entry as usize).status == MetaStatus::Pending {
            w.borrow_mut().push(entry);
        }
        m.imp().bound.borrow_mut().insert(entry, row.downgrade());
    });
    let m = model.clone();
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(obj) = item.item().and_downcast::<PaneRow>() else {
            return;
        };
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let mut bound = m.imp().bound.borrow_mut();
        if bound.get(&obj.entry()).and_then(|w| w.upgrade()).as_ref() == Some(&row) {
            bound.remove(&obj.entry());
        }
    });
    let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
    view.add_css_class("tudi-details");
    view
}

/// Column header row matching the row layout. Returns the box and the three
/// buttons (name, size, modified).
pub fn build_header() -> (gtk::Box, [gtk::Button; 3]) {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class("tudi-column-header");
    let mk = |label: &str, xalign: f32| {
        let l = gtk::Label::new(Some(label));
        l.set_xalign(xalign);
        let b = gtk::Button::builder().child(&l).build();
        b.add_css_class("flat");
        b
    };
    let name = mk(&tr("Name"), 0.0);
    name.set_hexpand(true);
    let size = mk(&tr("Size"), 1.0);
    size.set_width_request(COL_SIZE_CHARS * 9 + 12);
    let date = mk(&tr("Modified"), 1.0);
    date.set_width_request(COL_DATE_CHARS * 9 + 12);
    header.append(&name);
    header.append(&size);
    header.append(&date);
    (header, [name, size, date])
}
