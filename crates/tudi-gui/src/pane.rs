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
    /// Shown folders and other entries.
    pub dirs: u32,
    pub files: u32,
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
            d.dirs = order.dirs;
            d.files = order.files;
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
            d.dirs = order.dirs;
            d.files = order.files;
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
/// Narrowest and widest a dragged column gets, in pixels.
const COL_MIN: i32 = 24;
const COL_MAX: i32 = 800;

/// Widths of the size and date columns in pixels, set by dragging the
/// handles in the column header; `None` is the default width in characters
/// (which follows the zoom). The name column takes the rest. Shared by the
/// header and the rows (applied to a row when it is bound).
#[derive(Default)]
pub struct Columns {
    pub size: Cell<Option<i32>>,
    pub date: Cell<Option<i32>>,
}

/// Give a size or date label (row or header) its column width.
fn apply_width(label: &gtk::Label, px: Option<i32>, chars: i32) {
    match px {
        Some(px) => {
            // Narrow natural width, so the size request is the width.
            label.set_width_chars(-1);
            label.set_max_width_chars(1);
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.set_size_request(px, -1);
        }
        None => {
            label.set_width_chars(chars);
            label.set_max_width_chars(-1);
            label.set_ellipsize(pango::EllipsizeMode::None);
            label.set_size_request(-1, -1);
        }
    }
}

fn apply_row_widths(row: &gtk::Box, cols: &Columns) {
    let p = parts(row);
    apply_width(&p.size, cols.size.get(), COL_SIZE_CHARS);
    apply_width(&p.date, cols.date.get(), COL_DATE_CHARS);
}

fn fixed_label(chars: i32) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(1.0);
    l.set_width_chars(chars);
    l.set_max_width_chars(chars);
    l.add_css_class("dim-label");
    l.add_css_class("numeric");
    l.add_css_class("tudi-col");
    l
}

/// Row widget: icon, name, size, modified.
/// Marks a row widget (to tell rows from the list's empty space).
const ROW_CLASS: &str = "tudi-row";

fn row_widget() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class(ROW_CLASS);
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

impl PaneModel {
    /// The position a bound row widget shows (a few dozen are bound).
    pub fn bound_position(&self, row: &gtk::Widget) -> Option<u32> {
        self.imp()
            .bound
            .borrow()
            .iter()
            .find(|(_, w)| {
                w.upgrade()
                    .is_some_and(|r| r.upcast_ref::<gtk::Widget>() == row)
            })
            .map(|(&p, _)| p)
    }
}

/// Entries whose rows were bound while their metadata was pending: the
/// visible rows, fetched first.
pub type Wanted = std::rc::Rc<RefCell<Vec<u32>>>;

/// Build the details `ListView`.
/// Right-click handler: the row position (or `None` for empty space), the
/// widget clicked, and the click coordinates in it.
pub type MenuFn = Rc<dyn Fn(Option<u32>, &gtk::Widget, f64, f64)>;

/// Files dropped on a row: its position, the paths, the modifiers held.
pub type RowDropFn = Rc<dyn Fn(u32, Vec<std::path::PathBuf>, gtk::gdk::ModifierType) -> bool>;

pub fn build_view(
    model: &PaneModel,
    selection: &gtk::MultiSelection,
    wanted: &Wanted,
    columns: &Rc<Columns>,
    on_menu: MenuFn,
    on_drop: RowDropFn,
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
        let (f, wi) = (on_drop.clone(), item.downgrade());
        row.add_css_class("tudi-drop-row");
        row.add_controller(crate::drop::file_drop_target(Rc::new(
            move |files, mods| {
                wi.upgrade()
                    .is_some_and(|item| f(item.position(), files, mods))
            },
        )));
        item.set_child(Some(&row));
        crate::bench::ROW_WORK.with(|r| {
            let mut w = r.get();
            w.setups += 1;
            r.set(w);
        });
    });
    let (m, w, cols) = (model.clone(), wanted.clone(), columns.clone());
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let pos = item.item().and_downcast::<PaneRow>().unwrap().position();
        let t0 = std::time::Instant::now();
        // Unchanged widths are no-ops (GTK setters compare first).
        apply_row_widths(&row, &cols);
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
    // Right-click outside the rows. In the capture phase: GtkListView's own
    // gestures take a press on its empty area before a bubble-phase gesture
    // sees it. A press inside a row is left to the row's gesture.
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(move |g, _, x, y| {
        let Some(w) = g.widget() else { return };
        let on_row = std::iter::successors(w.pick(x, y, gtk::PickFlags::DEFAULT), |c| c.parent())
            .take_while(|c| c != &w)
            .any(|c| c.has_css_class(ROW_CLASS));
        if !on_row {
            g.set_state(gtk::EventSequenceState::Claimed);
            on_menu(None, &w, x, y);
        }
    });
    view.add_controller(click);
    view
}

/// Column header row matching the row layout. Returns the box and the three
/// buttons (name, size, modified). Between the buttons are drag handles:
/// the one left of a column sets that column's width (dragging left widens
/// it; the name column takes the rest); a double-click restores the
/// default. The widths are saved with the window state.
pub fn build_header(model: &PaneModel, columns: &Rc<Columns>) -> (gtk::Box, [gtk::Button; 3]) {
    // No spacing: the handles fill the gaps the rows have between columns.
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
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
    size.add_css_class("tudi-col");
    date.add_css_class("tudi-col");
    let label_of = |b: &gtk::Button| b.child().and_downcast::<gtk::Label>().unwrap();
    let (size_label, date_label) = (label_of(&size), label_of(&date));
    apply_width(&size_label, columns.size.get(), COL_SIZE_CHARS);
    apply_width(&date_label, columns.date.get(), COL_DATE_CHARS);
    // Re-apply the widths to the header label and the bound rows.
    let refresh: Rc<dyn Fn()> = {
        let (m, cols) = (model.clone(), columns.clone());
        let (sl, dl) = (size_label.clone(), date_label.clone());
        Rc::new(move || {
            apply_width(&sl, cols.size.get(), COL_SIZE_CHARS);
            apply_width(&dl, cols.date.get(), COL_DATE_CHARS);
            // Only bound rows (on screen, a few dozen); the others get the
            // widths when bound.
            for row in m.imp().bound.borrow().values().filter_map(|w| w.upgrade()) {
                apply_row_widths(&row, &cols);
            }
        })
    };
    let handle = |column: gtk::Button, is_size: bool| {
        let h = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        h.set_size_request(8, -1);
        h.add_css_class("tudi-col-handle");
        h.set_cursor_from_name(Some("col-resize"));
        let drag = gtk::GestureDrag::new();
        // The handle moves with the column edge, so its own coordinates are
        // not a fixed frame: measure the pointer in the window's.
        let in_window = |w: &gtk::Widget, x: f64| {
            let root = w.root()?;
            w.compute_point(&root, &gtk::graphene::Point::new(x as f32, 0.0))
                .map(|p| f64::from(p.x()))
        };
        // (column width, pointer x in the window) at the press.
        let start = Rc::new(Cell::new((0, 0.0)));
        let (st, col) = (start.clone(), column.clone());
        drag.connect_drag_begin(move |g, x, _| {
            g.set_state(gtk::EventSequenceState::Claimed);
            let Some(w) = g.widget() else { return };
            // The label's width: the button has no horizontal padding.
            st.set((col.width(), in_window(&w, x).unwrap_or(0.0)));
        });
        let (st, cols, r) = (start.clone(), columns.clone(), refresh.clone());
        drag.connect_drag_update(move |g, dx, _| {
            let (Some(w), Some((sx, _))) = (g.widget(), g.start_point()) else {
                return;
            };
            let Some(now) = in_window(&w, sx + dx) else {
                return;
            };
            let (width, x0) = st.get();
            let px = (width - (now - x0).round() as i32).clamp(COL_MIN, COL_MAX);
            let c = if is_size { &cols.size } else { &cols.date };
            if c.get() != Some(px) {
                c.set(Some(px));
                r();
            }
        });
        h.add_controller(drag);
        let reset = gtk::GestureClick::new();
        let (cols, r) = (columns.clone(), refresh.clone());
        reset.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                let c = if is_size { &cols.size } else { &cols.date };
                c.set(None);
                r();
            }
        });
        h.add_controller(reset);
        h
    };
    header.append(&name);
    header.append(&handle(size.clone(), true));
    header.append(&size);
    header.append(&handle(date.clone(), false));
    header.append(&date);
    (header, [name, size, date])
}
