//! Tabs in the file list, like a web browser's (ADR-025).
//!
//! Each tab has its own folder, history, list model and view, selection,
//! find bar, sort order and folder size; the window's folder tree, path
//! bar and status bar follow the active tab. Tabs sit in a strip above the
//! list, labelled with their folder's name, with a "+" right after the
//! last one. Two tabs can be tiled side by side: while the active tab is
//! one of the pair, both are shown; clicking or focusing one makes it the
//! active tab.
//!
//! Code written for one list keeps using `Browser::tab()`, which is the
//! active tab — or, while a worker reply or a tab's own widget event is
//! being handled, the tab it belongs to (`Browser::with_tab`).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use tudi_core::listing::meta::MetaFetcher;
use tudi_core::listing::sort::SortSpec;

use crate::browser::Browser;
use crate::i18n::tr;
use crate::pane::{self, PaneModel, Wanted};

/// A handler of a "tab." action, given the tab it was invoked on.
type TabAction = Box<dyn Fn(&Rc<Browser>, Rc<Tab>)>;

/// One tab: a folder shown in the details list.
pub struct Tab {
    pub id: u64,
    pub(crate) pane: PaneModel,
    pub(crate) pane_sel: gtk::MultiSelection,
    pub(crate) pane_view: gtk::ListView,
    pub(crate) pane_scroll: gtk::ScrolledWindow,
    pub(crate) sort_buttons: [gtk::Button; 3],
    /// Find bar, column header and list: what the tab shows.
    pub(crate) files: gtk::Box,
    pub(crate) fetcher: RefCell<Option<MetaFetcher>>,
    pub(crate) wanted: Wanted,
    pub(crate) wanted_scheduled: Cell<bool>,
    pub(crate) typeahead: crate::actions::TypeAheadState,
    pub(crate) find: crate::find::FindBar,

    pub(crate) current: RefCell<PathBuf>,
    pub(crate) history: RefCell<Vec<PathBuf>>,
    pub(crate) future: RefCell<Vec<PathBuf>>,
    pub(crate) generation: Cell<u64>,
    /// The order parameters (sort, hidden, find filter) changed while a
    /// newer listing was in flight or before the first one arrived: re-sort
    /// when it arrives (it was computed with the old parameters).
    pub(crate) resort_pending: Cell<bool>,
    /// Why the shown folder could not be listed; the status bar shows it
    /// instead of the counts until the next listing.
    pub(crate) pane_error: RefCell<Option<String>>,
    pub(crate) spec: Cell<SortSpec>,
    /// Entry name to select once the next listing arrives (e.g. the folder
    /// we came from when going up).
    pub(crate) select_after_load: RefCell<Option<Vec<u8>>>,
    /// Scroll position to restore after a live reload of the pane.
    pub(crate) keep_scroll: Cell<Option<f64>>,
    /// Folder to list when the tab is first shown (restored tabs are listed
    /// lazily).
    pub(crate) pending: RefCell<Option<PathBuf>>,

    /// Total size below the shown folder, once computed (≤ 1 s).
    pub(crate) folder_size: Cell<Option<u64>>,
    /// Free space on the shown folder's filesystem.
    pub(crate) free_space: Cell<Option<u64>>,
    /// Cancels the running size computation.
    pub(crate) size_cancel: RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>,
    /// Folder whose size is shown or being computed.
    pub(crate) size_for: RefCell<PathBuf>,
    /// The generation the size computation was started with.
    pub(crate) size_gen: Cell<u64>,

    /// The tab's button in the strip, and its label.
    pub(crate) button: gtk::Box,
    pub(crate) label: gtk::Label,
}

impl Tab {
    /// Build a tab's widgets. Events from its list are handled with the tab
    /// as `Browser::tab()` (see [`Browser::with_tab`]).
    pub(crate) fn new(
        b: &Weak<Browser>,
        id: u64,
        columns: &Rc<pane::Columns>,
        spec: SortSpec,
    ) -> Rc<Tab> {
        Rc::new_cyclic(|me: &Weak<Tab>| {
            let pane = PaneModel::default();
            let pane_sel = gtk::MultiSelection::new(Some(pane.clone()));
            let wanted: Wanted = Rc::default();
            let (bw, tw) = (b.clone(), me.clone());
            let on_menu: pane::MenuFn = Rc::new(move |pos, w, x, y| {
                if let (Some(b), Some(t)) = (bw.upgrade(), tw.upgrade()) {
                    b.with_tab(&t, |b| b.file_menu(pos, w, x, y));
                }
            });
            let (bw, tw) = (b.clone(), me.clone());
            let on_drop: pane::RowDropFn = Rc::new(move |pos, files, mods| {
                let (Some(b), Some(t)) = (bw.upgrade(), tw.upgrade()) else {
                    return false;
                };
                b.activate_tab(&t);
                b.drop_on_pane(Some(pos), files, mods)
            });
            let pane_view = pane::build_view(&pane, &pane_sel, &wanted, columns, on_menu, on_drop);
            let (col_header, sort_buttons) = pane::build_header(&pane, columns);
            let pane_scroll = gtk::ScrolledWindow::builder()
                .overlay_scrolling(false)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&pane_view)
                .build();
            keep_header_aligned(&col_header, &pane_scroll);
            let find = crate::find::FindBar::new();
            let files = gtk::Box::new(gtk::Orientation::Vertical, 0);
            files.add_css_class("tudi-tab-page");
            files.set_hexpand(true);
            files.append(&find.revealer);
            files.append(&col_header);
            files.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            files.append(&pane_scroll);
            let (button, label) = tab_button();
            Tab {
                id,
                pane,
                pane_sel,
                pane_view,
                pane_scroll,
                sort_buttons,
                files,
                fetcher: RefCell::default(),
                wanted,
                wanted_scheduled: Cell::new(false),
                typeahead: Rc::default(),
                find,
                current: RefCell::default(),
                history: RefCell::default(),
                future: RefCell::default(),
                generation: Cell::new(0),
                resort_pending: Cell::new(false),
                pane_error: RefCell::default(),
                spec: Cell::new(spec),
                select_after_load: RefCell::default(),
                keep_scroll: Cell::new(None),
                pending: RefCell::default(),
                folder_size: Cell::new(None),
                free_space: Cell::new(None),
                size_cancel: RefCell::default(),
                size_for: RefCell::default(),
                size_gen: Cell::new(0),
                button,
                label,
            }
        })
    }

    /// The tab's title: the shown folder's name.
    pub(crate) fn title(&self) -> String {
        let cur = self.current.borrow();
        let cur = if cur.as_os_str().is_empty() {
            self.pending.borrow().clone().unwrap_or_default()
        } else {
            cur.clone()
        };
        title_of(&cur)
    }

    pub(crate) fn update_title(&self) {
        let t = self.title();
        self.label.set_text(&t);
        self.button
            .set_tooltip_text(Some(&self.shown_path().to_string_lossy()));
    }

    fn shown_path(&self) -> PathBuf {
        let cur = self.current.borrow();
        if cur.as_os_str().is_empty() {
            self.pending.borrow().clone().unwrap_or_default()
        } else {
            cur.clone()
        }
    }

    /// Stop the folder-size computation (the tab is closed).
    fn cancel_work(&self) {
        if let Some(c) = self.size_cancel.take() {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.fetcher.replace(None);
    }
}

/// The rows are narrower by the vertical scrollbar only while it is shown
/// (a short folder has none): give the column header the same right
/// margin, so its columns and lines stay above the rows'.
fn keep_header_aligned(header: &gtk::Box, scroll: &gtk::ScrolledWindow) {
    let sync = {
        let (h, s) = (header.downgrade(), scroll.downgrade());
        move || {
            let (Some(h), Some(s)) = (h.upgrade(), s.upgrade()) else {
                return;
            };
            let adj = s.vadjustment();
            let sb = s.vscrollbar();
            let shown = adj.upper() > adj.page_size() + 0.5;
            let w = if shown {
                sb.measure(gtk::Orientation::Horizontal, -1).0
            } else {
                0
            };
            if h.margin_end() != w {
                h.set_margin_end(w);
            }
        }
    };
    let s = sync.clone();
    scroll.vadjustment().connect_changed(move |_| s());
    sync();
}

/// A folder's name for a tab ("/" for the root).
pub(crate) fn title_of(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned()
}

/// A tab in the strip: the folder name and a close button.
fn tab_button() -> (gtk::Box, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    b.add_css_class("tudi-tab");
    let label = gtk::Label::new(None);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(22);
    label.set_width_chars(6);
    label.set_xalign(0.0);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("flat");
    close.add_css_class("circular");
    close.add_css_class("tudi-tab-close");
    close.set_tooltip_text(Some(&tr("Close Tab (Ctrl+W)")));
    b.append(&label);
    b.append(&close);
    (b, label)
}

/// The window's tab strip and the area that shows the active tab (or the
/// tiled pair).
pub struct TabStrip {
    pub(crate) bar: gtk::Box,
    pub(crate) tabs_box: gtk::Box,
    pub(crate) stage: gtk::Box,
    pub(crate) add: gtk::Button,
    /// The split shown while the active tab is tiled.
    pub(crate) split: RefCell<Option<gtk::Paned>>,
}

impl TabStrip {
    pub(crate) fn new() -> Self {
        let tabs_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let add = gtk::Button::from_icon_name("list-add-symbolic");
        add.add_css_class("flat");
        add.add_css_class("tudi-tab-add");
        add.set_tooltip_text(Some(&tr("New Tab (Ctrl+T)")));
        // The "+" follows the last tab; the strip scrolls when the tabs do
        // not fit.
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        row.set_halign(gtk::Align::Start);
        row.append(&tabs_box);
        row.append(&add);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&row)
            .hexpand(true)
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("tudi-tabs");
        bar.append(&scroll);
        let stage = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        stage.set_vexpand(true);
        Self {
            bar,
            tabs_box,
            stage,
            add,
            split: RefCell::default(),
        }
    }
}

impl Browser {
    /// The tab being worked on: the one whose reply or event is being
    /// handled, else the active tab.
    pub(crate) fn tab(&self) -> Rc<Tab> {
        if let Some(t) = self.tab_ctx.borrow().last() {
            return t.clone();
        }
        self.active.borrow().clone()
    }

    pub(crate) fn is_active(&self, tab: &Tab) -> bool {
        self.active.borrow().id == tab.id
    }

    /// Run `f` with `tab` as [`Self::tab`].
    pub(crate) fn with_tab<R>(self: &Rc<Self>, tab: &Rc<Tab>, f: impl FnOnce(&Rc<Self>) -> R) -> R {
        self.tab_ctx.borrow_mut().push(tab.clone());
        let r = f(self);
        self.tab_ctx.borrow_mut().pop();
        r
    }

    /// A new listing generation, unique across the window's tabs (replies
    /// find their tab by it).
    pub(crate) fn next_generation(&self) -> u64 {
        let g = self.generations.get() + 1;
        self.generations.set(g);
        g
    }

    /// The tab whose list has (or is about to get) `generation`.
    pub(crate) fn tab_of_generation(&self, generation: u64) -> Option<Rc<Tab>> {
        self.tabs
            .borrow()
            .iter()
            .find(|t| t.generation.get() == generation || t.pane.data().generation == generation)
            .cloned()
    }

    /// The tab with a running folder-size computation of `generation`.
    pub(crate) fn tab_of_size(&self, generation: u64) -> Option<Rc<Tab>> {
        self.tabs
            .borrow()
            .iter()
            .find(|t| t.size_gen.get() == generation)
            .cloned()
    }

    pub(crate) fn tab_by_id(&self, id: u64) -> Option<Rc<Tab>> {
        self.tabs.borrow().iter().find(|t| t.id == id).cloned()
    }

    /// Create a tab showing `path` (listed now if `activate`, else when
    /// first shown), after the active one.
    pub(crate) fn new_tab(self: &Rc<Self>, path: &Path, activate: bool) -> Rc<Tab> {
        let id = self.next_tab_id.get() + 1;
        self.next_tab_id.set(id);
        let spec = self.active.borrow().spec.get();
        let tab = Tab::new(&Rc::downgrade(self), id, &self.columns, spec);
        tab.pending.replace(Some(path.to_path_buf()));
        self.with_tab(&tab, |b| b.wire_tab());
        self.wire_tab_button(&tab);
        {
            let mut tabs = self.tabs.borrow_mut();
            let at = tabs
                .iter()
                .position(|t| self.is_active(t))
                .map_or(tabs.len(), |i| i + 1);
            tabs.insert(at, tab.clone());
        }
        self.rebuild_strip();
        tab.update_title();
        if activate {
            self.activate_tab(&tab);
        }
        tab
    }

    /// Ctrl+T: a new tab on the active tab's folder.
    pub(crate) fn new_tab_here(self: &Rc<Self>) {
        let cur = self.tab().current.borrow().clone();
        let tab = self.new_tab(&cur, true);
        tab.pane_view.grab_focus();
    }

    /// Open `path` in a new tab without switching to it (middle-click,
    /// "Open in New Tab").
    pub(crate) fn open_in_new_tab(self: &Rc<Self>, path: &Path) {
        self.new_tab(path, false);
        self.record_visit(path);
    }

    /// Make `tab` the active tab: the tree, path bar, title and status
    /// bar follow it.
    pub(crate) fn activate_tab(self: &Rc<Self>, tab: &Rc<Tab>) {
        let changed = !self.is_active(tab);
        if changed {
            self.active.replace(tab.clone());
        }
        if let Some(p) = tab.pending.take() {
            self.with_tab(tab, |b| b.navigate(&p, false));
        }
        if !changed {
            return;
        }
        self.show_stage();
        for t in self.tabs.borrow().iter() {
            if t.id == tab.id {
                t.button.add_css_class("active");
                t.files.add_css_class("active");
            } else {
                t.button.remove_css_class("active");
                t.files.remove_css_class("active");
            }
        }
        self.sync_chrome();
    }

    /// Path bar, title, history buttons, tree and status bar for the
    /// active tab.
    pub(crate) fn sync_chrome(self: &Rc<Self>) {
        let tab = self.active.borrow().clone();
        let cur = tab.current.borrow().clone();
        self.path_entry.set_text(&cur.to_string_lossy());
        self.window
            .set_title(Some(&format!("{} — tudi", title_of(&cur))));
        self.back.set_sensitive(!tab.history.borrow().is_empty());
        self.forward.set_sensitive(!tab.future.borrow().is_empty());
        if !cur.as_os_str().is_empty() {
            self.reveal_in_tree(cur);
        }
        self.update_status();
    }

    /// Put the active tab (or its tiled pair) in the stage.
    pub(crate) fn show_stage(&self) {
        let stage = &self.strip.stage;
        while let Some(c) = stage.first_child() {
            stage.remove(&c);
        }
        if let Some(p) = self.strip.split.take() {
            p.set_start_child(gtk::Widget::NONE);
            p.set_end_child(gtk::Widget::NONE);
        }
        let active = self.active.borrow().clone();
        let pair = self.tiled.borrow().clone();
        match pair.clone() {
            Some((l, r)) if l.id == active.id || r.id == active.id => {
                let p = gtk::Paned::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .start_child(&l.files)
                    .end_child(&r.files)
                    .wide_handle(true)
                    .hexpand(true)
                    .shrink_start_child(false)
                    .shrink_end_child(false)
                    .build();
                l.files.add_css_class("tudi-tile");
                r.files.add_css_class("tudi-tile");
                stage.append(&p);
                // Half and half once the split has its width.
                let pw = p.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(p) = pw.upgrade() {
                        p.set_position(p.width() / 2);
                    }
                });
                self.strip.split.replace(Some(p));
            }
            _ => {
                stage.append(&active.files);
            }
        }
        for t in self.tabs.borrow().iter() {
            let tiled = pair
                .as_ref()
                .is_some_and(|(l, r)| l.id == t.id || r.id == t.id);
            if tiled {
                t.button.add_css_class("tiled");
            } else {
                t.button.remove_css_class("tiled");
                t.files.remove_css_class("tudi-tile");
            }
            if !tiled
                || pair
                    .as_ref()
                    .is_some_and(|(l, r)| l.id != active.id && r.id != active.id)
            {
                t.files.remove_css_class("tudi-tile");
            }
        }
    }

    /// Re-fill the strip in tab order.
    pub(crate) fn rebuild_strip(&self) {
        let bx = &self.strip.tabs_box;
        while let Some(c) = bx.first_child() {
            bx.remove(&c);
        }
        for t in self.tabs.borrow().iter() {
            bx.append(&t.button);
        }
    }

    /// Close `tab`; closing the last tab closes the window.
    pub(crate) fn close_tab(self: &Rc<Self>, tab: &Rc<Tab>) {
        let (index, next) = {
            let tabs = self.tabs.borrow();
            if tabs.len() <= 1 {
                drop(tabs);
                self.window.close();
                return;
            }
            let i = tabs.iter().position(|t| t.id == tab.id).unwrap_or(0);
            let next = if i + 1 < tabs.len() {
                tabs[i + 1].clone()
            } else {
                tabs[i - 1].clone()
            };
            (i, next)
        };
        let untile = self
            .tiled
            .borrow()
            .as_ref()
            .is_some_and(|(l, r)| l.id == tab.id || r.id == tab.id);
        if untile {
            self.tiled.replace(None);
        }
        self.tabs.borrow_mut().remove(index);
        tab.cancel_work();
        if let Some(w) = &self.watcher {
            w.unwatch(crate::worker::WatchKey::Pane(tab.id));
        }
        self.rebuild_strip();
        if self.is_active(tab) {
            // Not the closed tab's pair partner by surprise: the neighbour.
            self.activate_tab(&next);
        } else {
            self.show_stage();
        }
        self.active.borrow().pane_view.grab_focus();
    }

    pub(crate) fn close_active_tab(self: &Rc<Self>) {
        let t = self.active.borrow().clone();
        self.close_tab(&t);
    }

    /// Ctrl+Tab / Ctrl+PageDown (`step` 1) and back (-1), wrapping.
    pub(crate) fn cycle_tab(self: &Rc<Self>, step: isize) {
        let next = {
            let tabs = self.tabs.borrow();
            let n = tabs.len() as isize;
            let i = tabs.iter().position(|t| self.is_active(t)).unwrap_or(0) as isize;
            tabs[((i + step).rem_euclid(n)) as usize].clone()
        };
        self.activate_tab(&next);
        next.pane_view.grab_focus();
    }

    /// Show `tab` side by side with the active tab (or `tab` and its
    /// right neighbour when it is the active one).
    pub(crate) fn tile_with_active(self: &Rc<Self>, tab: &Rc<Tab>) {
        let active = self.active.borrow().clone();
        let other = if active.id != tab.id {
            active.clone()
        } else {
            let tabs = self.tabs.borrow();
            let i = tabs.iter().position(|t| t.id == tab.id).unwrap_or(0);
            match tabs
                .get(i + 1)
                .or_else(|| i.checked_sub(1).and_then(|j| tabs.get(j)))
            {
                Some(t) => t.clone(),
                None => return,
            }
        };
        // Left and right in strip order.
        let (l, r) = {
            let tabs = self.tabs.borrow();
            let pos = |t: &Rc<Tab>| tabs.iter().position(|x| x.id == t.id).unwrap_or(0);
            if pos(&other) < pos(tab) {
                (other, tab.clone())
            } else {
                (tab.clone(), other)
            }
        };
        if let Some(p) = l.pending.take() {
            self.with_tab(&l, |b| b.navigate(&p, false));
        }
        if let Some(p) = r.pending.take() {
            self.with_tab(&r, |b| b.navigate(&p, false));
        }
        self.tiled.replace(Some((l, r)));
        self.show_stage();
    }

    pub(crate) fn untile(self: &Rc<Self>) {
        self.tiled.replace(None);
        self.show_stage();
    }

    /// Ctrl+\: tile the active tab with its neighbour, or untile.
    pub(crate) fn toggle_tile(self: &Rc<Self>) {
        let active = self.active.borrow().clone();
        let tiled = self
            .tiled
            .borrow()
            .as_ref()
            .is_some_and(|(l, r)| l.id == active.id || r.id == active.id);
        if tiled {
            self.untile();
        } else if self.tabs.borrow().len() > 1 {
            self.tile_with_active(&active);
        } else {
            // One tab: split it into two on the same folder.
            let cur = active.current.borrow().clone();
            let t = self.new_tab(&cur, false);
            self.tile_with_active(&t);
        }
    }

    /// Clicks on a tab: select (primary), close (middle), menu (secondary);
    /// a click or focus in a tab's list makes it active.
    pub(crate) fn wire_tab_button(self: &Rc<Self>, tab: &Rc<Tab>) {
        let click = gtk::GestureClick::new();
        click.set_button(0);
        let (w, t) = (Rc::downgrade(self), Rc::downgrade(tab));
        click.connect_pressed(move |g, _, x, y| {
            let (Some(b), Some(tab)) = (w.upgrade(), t.upgrade()) else {
                return;
            };
            match g.current_button() {
                gdk::BUTTON_MIDDLE => {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    b.close_tab(&tab);
                }
                gdk::BUTTON_SECONDARY => {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    b.tab_menu(&tab, x, y);
                }
                _ => {
                    b.activate_tab(&tab);
                    tab.pane_view.grab_focus();
                }
            }
        });
        tab.button.add_controller(click);
        if let Some(close) = tab.button.last_child().and_downcast::<gtk::Button>() {
            let (w, t) = (Rc::downgrade(self), Rc::downgrade(tab));
            close.connect_clicked(move |_| {
                if let (Some(b), Some(tab)) = (w.upgrade(), t.upgrade()) {
                    b.close_tab(&tab);
                }
            });
        }
        // Any press or focus inside the tab's page activates it (a tile).
        let press = gtk::GestureClick::new();
        press.set_button(0);
        press.set_propagation_phase(gtk::PropagationPhase::Capture);
        let (w, t) = (Rc::downgrade(self), Rc::downgrade(tab));
        press.connect_pressed(move |_, _, _, _| {
            if let (Some(b), Some(tab)) = (w.upgrade(), t.upgrade()) {
                b.activate_tab(&tab);
            }
        });
        tab.files.add_controller(press);
        let focus = gtk::EventControllerFocus::new();
        let (w, t) = (Rc::downgrade(self), Rc::downgrade(tab));
        focus.connect_enter(move |_| {
            if let (Some(b), Some(tab)) = (w.upgrade(), t.upgrade()) {
                b.activate_tab(&tab);
            }
        });
        tab.files.add_controller(focus);
    }

    fn tab_menu(self: &Rc<Self>, tab: &Rc<Tab>, x: f64, y: f64) {
        let menu = gio::Menu::new();
        let id = tab.id as i64;
        let item = |label: String, action: &str| {
            let it = gio::MenuItem::new(Some(&label), None);
            it.set_action_and_target_value(Some(action), Some(&id.to_variant()));
            it
        };
        let n = self.tabs.borrow().len();
        let tiled = self
            .tiled
            .borrow()
            .as_ref()
            .is_some_and(|(l, r)| l.id == tab.id || r.id == tab.id);
        let first = gio::Menu::new();
        first.append_item(&item(tr("New Tab"), "tab.new"));
        if tiled {
            first.append_item(&item(tr("Stop Side by Side"), "tab.untile"));
        } else if n > 1 {
            let label = if self.is_active(tab) {
                tr("Side by Side with Next Tab")
            } else {
                tr("Side by Side with Current Tab")
            };
            first.append_item(&item(label, "tab.tile"));
        }
        menu.append_section(None, &first);
        let close = gio::Menu::new();
        close.append_item(&item(tr("Close Tab"), "tab.close"));
        if n > 1 {
            close.append_item(&item(tr("Close Other Tabs"), "tab.close-others"));
        }
        menu.append_section(None, &close);
        crate::menu::popup_at(&tab.button, menu.upcast_ref(), x, y);
    }

    /// The "tab." actions used by the tab menu.
    pub(crate) fn install_tab_actions(self: &Rc<Self>) {
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, f: TabAction| {
            let a = gio::SimpleAction::new(name, Some(glib::VariantTy::INT64));
            let w = Rc::downgrade(self);
            a.connect_activate(move |_, v| {
                let id = v.and_then(|v| v.get::<i64>()).unwrap_or(-1);
                if let Some(b) = w.upgrade()
                    && let Some(t) = b.tab_by_id(id as u64)
                {
                    f(&b, t);
                }
            });
            group.add_action(&a);
        };
        add(
            "new",
            Box::new(|b, t| {
                b.activate_tab(&t);
                b.new_tab_here();
            }),
        );
        add("tile", Box::new(|b, t| b.tile_with_active(&t)));
        add("untile", Box::new(|b, _| b.untile()));
        add("close", Box::new(|b, t| b.close_tab(&t)));
        add(
            "close-others",
            Box::new(|b, t| {
                let others: Vec<Rc<Tab>> = b
                    .tabs
                    .borrow()
                    .iter()
                    .filter(|o| o.id != t.id)
                    .cloned()
                    .collect();
                b.activate_tab(&t);
                for o in others {
                    b.close_tab(&o);
                }
            }),
        );
        self.window.insert_action_group("tab", Some(&group));
        let w = Rc::downgrade(self);
        self.strip.add.connect_clicked(move |_| {
            if let Some(b) = w.upgrade() {
                b.new_tab_here();
            }
        });
    }

    /// Tabs to remember: folders in strip order and the active one.
    pub(crate) fn tab_paths(&self) -> (Vec<PathBuf>, usize) {
        let tabs = self.tabs.borrow();
        let paths = tabs.iter().map(|t| t.shown_path()).collect();
        let active = tabs.iter().position(|t| self.is_active(t)).unwrap_or(0);
        (paths, active)
    }
}
