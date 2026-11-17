//! One browser window: header bar (history, path), folder tree on the left,
//! details list on the right, status bar. Owns navigation and wires worker
//! replies into the two models.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tudi_core::config::{Config, SortBy};
use tudi_core::listing::meta::{MetaFetcher, MetaStatus};
use tudi_core::listing::sort::{SortKey, SortSpec};
use tudi_core::tree::{Expand, NodeId};

use crate::pane::{self, PaneModel, Wanted};
use crate::sidebar::{self, TreeModel};
use crate::worker::{Job, Reply, Worker};

/// Metadata worker threads per listing.
const META_WORKERS: usize = 4;
/// Rows fetched first after a listing arrives (about one screen).
const FIRST_SCREEN: usize = 80;
/// Above this many selected entries the status bar omits the size, so the
/// main thread never loops over a huge selection.
const STATUS_SIZE_LIMIT: u64 = 5000;

/// A window-level shortcut handler.
type Handler = Box<dyn Fn(&Rc<Browser>)>;

/// A pending "reveal this path in the tree" that waits for listings.
struct Reveal {
    target: PathBuf,
}

pub struct Browser {
    pub window: adw::ApplicationWindow,
    pub(crate) config: Config,
    pub(crate) toasts: adw::ToastOverlay,
    pub(crate) typeahead: crate::actions::TypeAheadState,
    pub(crate) worker: Worker,

    pub(crate) tree: TreeModel,
    pub(crate) tree_sel: gtk::SingleSelection,
    pub(crate) tree_view: gtk::ListView,
    pub(crate) roots: RefCell<Vec<(PathBuf, NodeId)>>,
    reveal: RefCell<Option<Reveal>>,
    /// Set while the tree selection is changed programmatically.
    pub(crate) syncing: Cell<bool>,

    pub(crate) pane: PaneModel,
    pub(crate) pane_sel: gtk::MultiSelection,
    pub(crate) pane_view: gtk::ListView,
    pub(crate) pane_scroll: gtk::ScrolledWindow,
    pub(crate) sort_buttons: [gtk::Button; 3],
    pub(crate) fetcher: RefCell<Option<MetaFetcher>>,
    pub(crate) wanted: Wanted,
    pub(crate) wanted_scheduled: Cell<bool>,

    pub(crate) path_entry: gtk::Entry,
    pub(crate) back: gtk::Button,
    pub(crate) forward: gtk::Button,
    pub(crate) status: gtk::Label,

    pub(crate) current: RefCell<PathBuf>,
    pub(crate) history: RefCell<Vec<PathBuf>>,
    pub(crate) future: RefCell<Vec<PathBuf>>,
    pub(crate) generation: Cell<u64>,
    pub(crate) spec: Cell<SortSpec>,
    pub(crate) show_hidden: Cell<bool>,
    /// Entry name to select once the next listing arrives (e.g. the folder
    /// we came from when going up).
    pub(crate) select_after_load: RefCell<Option<Vec<u8>>>,
}

fn home() -> PathBuf {
    glib::home_dir()
}

fn expand_tilde(text: &str) -> PathBuf {
    if text == "~" {
        home()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(text)
    }
}

impl Browser {
    pub fn new(app: &adw::Application, start: &Path, config: Config) -> Rc<Self> {
        let spec = SortSpec {
            key: match config.browse.sort {
                SortBy::Name => SortKey::Name,
                SortBy::Size => SortKey::Size,
                SortBy::Mtime => SortKey::Modified,
            },
            descending: config.browse.sort != SortBy::Name,
            dirs_first: config.browse.dirs_first,
        };
        let show_hidden = config.browse.show_hidden;
        let (worker, replies) = Worker::start();
        worker.send(Job::WarmMime);

        // --- tree
        let tree = TreeModel::default();
        let tree_sel = gtk::SingleSelection::new(Some(tree.clone()));
        tree_sel.set_autoselect(false);
        tree_sel.set_can_unselect(true);

        // --- pane
        let pane = PaneModel::default();
        let pane_sel = gtk::MultiSelection::new(Some(pane.clone()));
        let wanted: Wanted = Rc::default();
        let pane_view = pane::build_view(&pane, &pane_sel, &wanted);
        let (col_header, sort_buttons) = pane::build_header();
        let pane_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&pane_view)
            .build();

        // --- header bar
        let back = gtk::Button::from_icon_name("go-previous-symbolic");
        back.set_tooltip_text(Some("Back (Alt+Left)"));
        let forward = gtk::Button::from_icon_name("go-next-symbolic");
        forward.set_tooltip_text(Some("Forward (Alt+Right)"));
        let up = gtk::Button::from_icon_name("go-up-symbolic");
        up.set_tooltip_text(Some("Parent folder (Alt+Up)"));
        let path_entry = gtk::Entry::new();
        path_entry.set_hexpand(true);
        path_entry.set_width_chars(40);
        let header = adw::HeaderBar::new();
        header.pack_start(&back);
        header.pack_start(&forward);
        header.pack_start(&up);
        header.set_title_widget(Some(&path_entry));

        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.add_css_class("dim-label");
        status.add_css_class("tudi-status");

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("tudi")
            .default_width(1100)
            .default_height(700)
            .build();

        let b = Rc::new_cyclic(|weak: &std::rc::Weak<Browser>| {
            let w = weak.clone();
            let tree_view = sidebar::build_view(&tree, &tree_sel, move |pos| {
                if let Some(b) = w.upgrade() {
                    b.toggle_tree_row(pos as usize);
                }
            });
            Browser {
                window: window.clone(),
                config,
                toasts: adw::ToastOverlay::new(),
                typeahead: Rc::default(),
                worker,
                tree,
                tree_sel,
                tree_view,
                roots: RefCell::default(),
                reveal: RefCell::default(),
                syncing: Cell::new(false),
                pane,
                pane_sel,
                pane_view,
                pane_scroll,
                sort_buttons,
                fetcher: RefCell::default(),
                wanted,
                wanted_scheduled: Cell::new(false),
                path_entry,
                back,
                forward,
                status,
                current: RefCell::default(),
                history: RefCell::default(),
                future: RefCell::default(),
                generation: Cell::new(0),
                spec: Cell::new(spec),
                show_hidden: Cell::new(show_hidden),
                select_after_load: RefCell::default(),
            }
        });

        // --- layout
        let tree_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&b.tree_view)
            .build();
        let right = gtk::Box::new(gtk::Orientation::Vertical, 0);
        right.append(&col_header);
        right.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        right.append(&b.pane_scroll);
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&tree_scroll)
            .end_child(&right)
            .position(280)
            .shrink_start_child(false)
            .resize_start_child(false)
            .vexpand(true)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&paned);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(&b.status);
        b.toasts.set_child(Some(&content));
        b.window.set_content(Some(&b.toasts));

        b.add_roots();
        b.connect_signals(&up);
        b.install_shortcuts();
        b.install_drag_source();
        b.spawn_reply_loop(replies);
        b.update_sort_buttons();
        b.navigate(start, true);
        b
    }

    fn add_roots(&self) {
        let home = home();
        let mut roots = self.roots.borrow_mut();
        let id = self.tree.add_root(&home.to_string_lossy(), "Home");
        roots.push((home, id));
        let id = self.tree.add_root("/", "File System");
        roots.push((PathBuf::from("/"), id));
    }

    // ------------------------------------------------------------ signals

    fn connect_signals(self: &Rc<Self>, up: &gtk::Button) {
        let w = Rc::downgrade(self);
        self.tree_sel.connect_selection_changed(move |sel, _, _| {
            let Some(b) = w.upgrade() else { return };
            if b.syncing.get() {
                return;
            }
            let Some(row) = sel.selected_item().and_downcast::<sidebar::TreeRow>() else {
                return;
            };
            if row.is_error() {
                return;
            }
            let path = b.tree.tree().path(row.node());
            b.navigate(&path, true);
        });

        let w = Rc::downgrade(self);
        self.pane_view.connect_activate(move |_, pos| {
            if let Some(b) = w.upgrade() {
                b.activate_entry(pos);
            }
        });

        let w = Rc::downgrade(self);
        self.pane_sel.connect_selection_changed(move |_, _, _| {
            if let Some(b) = w.upgrade() {
                b.update_status();
            }
        });

        let w = Rc::downgrade(self);
        self.path_entry.connect_activate(move |e| {
            if let Some(b) = w.upgrade() {
                let p = expand_tilde(e.text().trim());
                b.navigate(&p, true);
                b.pane_view.grab_focus();
            }
        });

        let w = Rc::downgrade(self);
        self.back.connect_clicked(move |_| {
            if let Some(b) = w.upgrade() {
                b.go_back();
            }
        });
        let w = Rc::downgrade(self);
        self.forward.connect_clicked(move |_| {
            if let Some(b) = w.upgrade() {
                b.go_forward();
            }
        });
        let w = Rc::downgrade(self);
        up.connect_clicked(move |_| {
            if let Some(b) = w.upgrade() {
                b.go_up();
            }
        });

        for i in 0..3 {
            let w = Rc::downgrade(self);
            self.sort_buttons[i].connect_clicked(move |_| {
                if let Some(b) = w.upgrade() {
                    b.click_sort(i);
                }
            });
        }

        // Tree keys: Right expands, Left collapses or selects the parent.
        let keys = gtk::EventControllerKey::new();
        let w = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(b) = w.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if !mods.is_empty() {
                return glib::Propagation::Proceed;
            }
            let pos = b.tree_sel.selected();
            if pos == gtk::INVALID_LIST_POSITION {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::Right | gdk::Key::KP_Right => {
                    b.expand_tree_row(pos as usize);
                    glib::Propagation::Stop
                }
                gdk::Key::Left | gdk::Key::KP_Left => {
                    b.collapse_or_parent(pos as usize);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.tree_view.add_controller(keys);
    }

    fn install_shortcuts(self: &Rc<Self>) {
        let ctl = gtk::ShortcutController::new();
        ctl.set_scope(gtk::ShortcutScope::Managed);
        let add = |accel: &str, f: Handler| {
            let w = Rc::downgrade(self);
            let action = gtk::CallbackAction::new(move |_, _| {
                if let Some(b) = w.upgrade() {
                    f(&b);
                }
                glib::Propagation::Stop
            });
            ctl.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(accel),
                Some(action),
            ));
        };
        add("<Alt>Left", Box::new(|b| b.go_back()));
        add("<Alt>Right", Box::new(|b| b.go_forward()));
        add("<Alt>Up", Box::new(|b| b.go_up()));
        add("<Control>l", Box::new(|b| b.focus_path()));
        add("<Control>h", Box::new(|b| b.toggle_hidden()));
        add("F6", Box::new(|b| b.switch_pane_focus()));
        add("<Control><Alt>t", Box::new(|b| b.open_terminal()));
        add("<Control>e", Box::new(|b| b.open_editor()));
        add("<Control><Shift>n", Box::new(|b| b.new_folder()));
        self.window.add_controller(ctl);

        // List-only shortcuts, so they do not steal keys from the path bar.
        let ctl = gtk::ShortcutController::new();
        let add = |accel: &str, f: Handler| {
            let w = Rc::downgrade(self);
            let action = gtk::CallbackAction::new(move |_, _| {
                if let Some(b) = w.upgrade() {
                    f(&b);
                }
                glib::Propagation::Stop
            });
            ctl.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(accel),
                Some(action),
            ));
        };
        add("<Control>c", Box::new(|b| b.copy_files()));
        add("<Control><Shift>c", Box::new(|b| b.copy_paths()));
        add("Delete", Box::new(|b| b.trash_selected()));
        add("F2", Box::new(|b| b.rename_selected()));
        self.pane_view.add_controller(ctl);

        // Backspace goes up only from the panes, not while editing the path.
        let keys = gtk::EventControllerKey::new();
        let w = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(b) = w.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if key == gdk::Key::BackSpace && mods.is_empty() {
                b.go_up();
                return glib::Propagation::Stop;
            }
            let plain = (mods - gdk::ModifierType::SHIFT_MASK).is_empty();
            if plain
                && let Some(c) = key.to_unicode()
                && b.type_ahead(c)
            {
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.pane_view.add_controller(keys);
    }

    // --------------------------------------------------------- navigation

    /// Show `path` in the details list and reveal it in the tree.
    pub fn navigate(self: &Rc<Self>, path: &Path, push_history: bool) {
        let path = path.to_path_buf();
        {
            let mut cur = self.current.borrow_mut();
            if push_history && !cur.as_os_str().is_empty() && *cur != path {
                self.history.borrow_mut().push(cur.clone());
                self.future.borrow_mut().clear();
            }
            *cur = path.clone();
        }
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.fetcher.replace(None);
        self.path_entry.set_text(&path.to_string_lossy());
        self.window.set_title(Some(&format!(
            "{} — tudi",
            path.file_name()
                .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
        )));
        self.back.set_sensitive(!self.history.borrow().is_empty());
        self.forward.set_sensitive(!self.future.borrow().is_empty());
        self.worker.send(Job::ListPane {
            path: path.clone(),
            generation,
            spec: self.spec.get(),
            show_hidden: self.show_hidden.get(),
            select: self.select_after_load.borrow_mut().take(),
        });
        self.reveal_in_tree(path);
    }

    pub(crate) fn go_back(self: &Rc<Self>) {
        let Some(prev) = self.history.borrow_mut().pop() else {
            return;
        };
        self.future.borrow_mut().push(self.current.borrow().clone());
        self.navigate(&prev, false);
    }

    pub(crate) fn go_forward(self: &Rc<Self>) {
        let Some(next) = self.future.borrow_mut().pop() else {
            return;
        };
        self.history
            .borrow_mut()
            .push(self.current.borrow().clone());
        self.navigate(&next, false);
    }

    pub(crate) fn go_up(self: &Rc<Self>) {
        let cur = self.current.borrow().clone();
        if let Some(parent) = cur.parent() {
            use std::os::unix::ffi::OsStrExt;
            *self.select_after_load.borrow_mut() = cur.file_name().map(|n| n.as_bytes().to_vec());
            self.navigate(parent, true);
        }
    }

    fn focus_path(&self) {
        self.path_entry.grab_focus();
        self.path_entry.select_region(0, -1);
    }

    fn switch_pane_focus(&self) {
        if self.pane_view.has_focus() || self.pane_view.focus_child().is_some() {
            self.tree_view.grab_focus();
        } else {
            self.pane_view.grab_focus();
        }
    }

    pub(crate) fn toggle_hidden(self: &Rc<Self>) {
        self.show_hidden.set(!self.show_hidden.get());
        self.request_sort();
    }

    pub(crate) fn activate_entry(self: &Rc<Self>, pos: u32) {
        let (name, is_dir) = {
            let d = self.pane.data();
            let Some(&entry) = d.order.get(pos as usize) else {
                return;
            };
            (d.name(entry).to_vec(), d.is_dir(entry))
        };
        use std::os::unix::ffi::OsStrExt;
        let path = self
            .current
            .borrow()
            .join(std::ffi::OsStr::from_bytes(&name));
        if is_dir {
            self.navigate(&path, true);
        } else {
            open_default(&path, &self.window);
        }
    }

    // --------------------------------------------------------------- tree

    fn toggle_tree_row(self: &Rc<Self>, pos: usize) {
        let expanded = {
            let tree = self.tree.tree();
            match tree.rows().get(pos) {
                Some(r) => tree.node(r.node).is_expanded(),
                None => return,
            }
        };
        if expanded {
            self.collapse_tree_row(pos);
        } else {
            self.expand_tree_row(pos);
        }
    }

    pub(crate) fn expand_tree_row(self: &Rc<Self>, pos: usize) {
        let node = self.tree.tree().rows()[pos].node;
        let outcome = self.tree.update(Some(node), |t| {
            let e = t.expand(pos);
            let s = match &e {
                Expand::Spliced(s) => Some(*s),
                _ => None,
            };
            (e, s)
        });
        if let Expand::NeedsListing(node) = outcome {
            self.list_tree_node(node);
        }
    }

    pub(crate) fn collapse_tree_row(&self, pos: usize) {
        let node = self.tree.tree().rows()[pos].node;
        self.tree.update(Some(node), |t| ((), t.collapse(pos)));
    }

    fn collapse_or_parent(self: &Rc<Self>, pos: usize) {
        let (expanded, parent) = {
            let tree = self.tree.tree();
            let n = tree.node(tree.rows()[pos].node);
            (n.is_expanded(), n.parent())
        };
        if expanded {
            self.collapse_tree_row(pos);
        } else if let Some(p) = parent {
            let row = self.tree.tree().row_of(p);
            if let Some(row) = row {
                self.tree_sel.set_selected(row as u32);
                self.tree_view
                    .scroll_to(row as u32, gtk::ListScrollFlags::FOCUS, None);
            }
        }
    }

    fn list_tree_node(&self, node: NodeId) {
        let path = self.tree.tree().path(node);
        self.worker.send(Job::ListTree {
            node,
            path,
            show_hidden: self.show_hidden.get(),
        });
    }

    /// Expand the tree down to `target` and select it, listing directories
    /// as needed (continues in [`Self::continue_reveal`]).
    fn reveal_in_tree(self: &Rc<Self>, target: PathBuf) {
        *self.reveal.borrow_mut() = Some(Reveal { target });
        self.continue_reveal();
    }

    fn continue_reveal(self: &Rc<Self>) {
        let Some(target) = self.reveal.borrow().as_ref().map(|r| r.target.clone()) else {
            return;
        };
        // The root with the longest matching prefix.
        let Some((root_path, mut node)) = self
            .roots
            .borrow()
            .iter()
            .filter(|(p, _)| target.starts_with(p))
            .max_by_key(|(p, _)| p.as_os_str().len())
            .cloned()
        else {
            self.reveal.replace(None);
            return;
        };
        let rest: Vec<_> = target
            .strip_prefix(&root_path)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect();
        let mut complete = true;
        for comp in rest {
            // Make sure `node` is expanded and loaded.
            let state = {
                let tree = self.tree.tree();
                let n = tree.node(node);
                (n.is_expanded(), n.children().clone())
            };
            use tudi_core::tree::Children;
            match state {
                (_, Children::Loading) => return, // continue on reply
                (false, _) | (true, Children::Unknown) => {
                    let Some(row) = self.tree.tree().row_of(node) else {
                        complete = false;
                        break;
                    };
                    self.expand_tree_row(row);
                    if matches!(self.tree.tree().node(node).children(), Children::Loading) {
                        return;
                    }
                }
                _ => {}
            }
            let child = {
                let tree = self.tree.tree();
                match tree.node(node).children() {
                    Children::Loaded(kids) => kids
                        .iter()
                        .copied()
                        .find(|&k| tree.node(k).name() == comp.as_os_str()),
                    _ => None,
                }
            };
            match child {
                Some(c) => node = c,
                None => {
                    // Hidden (and hidden files are off) or not listed.
                    complete = false;
                    break;
                }
            }
        }
        self.reveal.replace(None);
        self.syncing.set(true);
        match self.tree.tree().row_of(node).filter(|_| complete) {
            Some(row) => {
                self.tree_sel.set_selected(row as u32);
                // Rows inserted in this turn are not measured yet; scroll
                // after the next frame's layout (and once more after that,
                // since ListView refines its row-height estimate).
                let frames = Cell::new(0);
                self.tree_view.add_tick_callback(move |v, _| {
                    v.scroll_to(row as u32, gtk::ListScrollFlags::NONE, None);
                    frames.set(frames.get() + 1);
                    if frames.get() < 2 {
                        glib::ControlFlow::Continue
                    } else {
                        glib::ControlFlow::Break
                    }
                });
            }
            // Do not highlight an ancestor as if it were the current folder.
            None => self.tree_sel.set_selected(gtk::INVALID_LIST_POSITION),
        }
        self.syncing.set(false);
    }

    // ------------------------------------------------------------- pane

    /// A column header was clicked: sort by it, or flip the direction.
    pub(crate) fn click_sort(&self, column: usize) {
        let key = [SortKey::Name, SortKey::Size, SortKey::Modified][column];
        let mut spec = self.spec.get();
        if spec.key == key {
            spec.descending = !spec.descending;
        } else {
            spec.key = key;
            spec.descending = key != SortKey::Name;
        }
        self.spec.set(spec);
        self.update_sort_buttons();
        self.request_sort();
    }

    pub(crate) fn request_sort(&self) {
        let d = self.pane.data();
        let (Some(listing), Some(rank)) = (d.listing.clone(), d.rank.clone()) else {
            return;
        };
        // MetaTable is a flat Vec of small structs: this clone is a memcpy.
        self.worker.send(Job::Sort {
            generation: d.generation,
            listing,
            rank,
            metas: d.metas.clone(),
            spec: self.spec.get(),
            show_hidden: self.show_hidden.get(),
        });
    }

    fn update_sort_buttons(&self) {
        let spec = self.spec.get();
        let labels = ["Name", "Size", "Modified"];
        let keys = [SortKey::Name, SortKey::Size, SortKey::Modified];
        for i in 0..3 {
            let arrow = if keys[i] != spec.key {
                ""
            } else if spec.descending {
                " ▾"
            } else {
                " ▴"
            };
            if let Some(l) = self.sort_buttons[i].child().and_downcast::<gtk::Label>() {
                l.set_text(&format!("{}{arrow}", labels[i]));
            }
        }
    }

    fn schedule_prioritize(self: &Rc<Self>) {
        if self.wanted_scheduled.replace(true) {
            return;
        }
        let w = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(b) = w.upgrade() else { return };
            b.wanted_scheduled.set(false);
            let wanted = std::mem::take(&mut *b.wanted.borrow_mut());
            if let Some(f) = &*b.fetcher.borrow() {
                f.prioritize(&wanted);
            }
        });
    }

    pub(crate) fn selected_entries(&self) -> Vec<u32> {
        let d = self.pane.data();
        let bits = self.pane_sel.selection();
        let mut out = Vec::with_capacity(bits.size() as usize);
        if let Some((mut it, first)) = gtk::BitsetIter::init_first(&bits) {
            let mut p = first;
            loop {
                if let Some(&e) = d.order.get(p as usize) {
                    out.push(e);
                }
                match it.next() {
                    Some(n) => p = n,
                    None => break,
                }
            }
        }
        out
    }

    fn select_entries(&self, entries: &[u32]) {
        let d = self.pane.data();
        let bits = gtk::Bitset::new_empty();
        for &e in entries {
            if let Some(&p) = d.pos_of.get(e as usize)
                && p != u32::MAX
            {
                bits.add(p);
            }
        }
        drop(d);
        let n = self.pane.n_items();
        self.pane_sel
            .set_selection(&bits, &gtk::Bitset::new_range(0, n));
    }

    pub(crate) fn update_status(&self) {
        let d = self.pane.data();
        let n = d.order.len();
        let mut text = if n == 1 {
            "1 item".to_string()
        } else {
            format!("{} items", fmt_count(n as u64))
        };
        let selected = self.pane_sel.selection().size();
        if selected > 0 {
            text.push_str(&format!(" · {} selected", fmt_count(selected)));
            if selected <= STATUS_SIZE_LIMIT {
                drop(d);
                let entries = self.selected_entries();
                let d = self.pane.data();
                let mut total = 0u64;
                let mut known = true;
                for e in entries {
                    let m = d.metas.get(e as usize);
                    if d.is_dir(e) {
                        continue;
                    }
                    if m.status == MetaStatus::Ok {
                        total += m.size;
                    } else {
                        known = false;
                    }
                }
                if known && total > 0 {
                    text.push_str(&format!(" ({})", crate::format::size(total)));
                }
            }
        }
        self.status.set_text(&text);
    }

    // ------------------------------------------------------------ replies

    fn spawn_reply_loop(self: &Rc<Self>, replies: async_channel::Receiver<Reply>) {
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(reply) = replies.recv().await {
                let Some(b) = w.upgrade() else { break };
                b.handle(reply);
                // Coalesce whatever else is already queued (≤ 1 batch of
                // work per main-loop turn).
                while let Ok(reply) = replies.try_recv() {
                    b.handle(reply);
                }
            }
        });
        let w = Rc::downgrade(self);
        // Bound rows push pending entries into `wanted`; forward them to
        // the fetcher after each frame's binds.
        self.pane_view
            .add_tick_callback(move |_, _| match w.upgrade() {
                Some(b) => {
                    if !b.wanted.borrow().is_empty() {
                        b.schedule_prioritize();
                    }
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            });
    }

    fn handle(self: &Rc<Self>, reply: Reply) {
        match reply {
            Reply::Pane(p) => {
                if p.generation != self.generation.get() {
                    return;
                }
                let first: Vec<u32> = p.order.order.iter().take(FIRST_SCREEN).copied().collect();
                let listing = p.listing.clone();
                self.pane
                    .set_listing(p.generation, p.listing, p.rank, p.order, self.spec.get());
                self.fetcher.replace(Some(MetaFetcher::start(
                    listing,
                    &first,
                    META_WORKERS,
                    self.worker.meta_sink(),
                )));
                self.after_order_change(Vec::new(), p.select);
            }
            Reply::PaneError { generation, error } => {
                if generation != self.generation.get() {
                    return;
                }
                self.pane.clear(generation);
                self.status.set_text(&error.to_string());
            }
            Reply::Sorted { generation, order } => {
                let keep = self.selected_entries();
                if self.pane.set_order(generation, order, self.spec.get()) {
                    self.after_order_change(keep, None);
                }
            }
            Reply::Meta(batch) => {
                if self.pane.apply_meta(&batch) {
                    self.request_sort();
                }
                if batch.complete || self.pane_sel.selection().size() > 0 {
                    self.update_status();
                }
            }
            Reply::Tree { node, result } => {
                self.tree
                    .update(Some(node), |t| ((), t.set_children(node, result)));
                self.continue_reveal();
            }
        }
    }

    /// Restore selection after the rows were replaced: the given entries,
    /// or the row at `select` (e.g. the folder we came up from).
    fn after_order_change(&self, keep: Vec<u32>, select: Option<u32>) {
        if let Some(p) = select {
            self.pane_sel.select_item(p, true);
            self.pane_view
                .scroll_to(p, gtk::ListScrollFlags::FOCUS, None);
        } else if !keep.is_empty() {
            self.select_entries(&keep);
        } else {
            self.pane_scroll.vadjustment().set_value(0.0);
        }
        self.update_status();
    }
}

fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Open a file with its default application, asynchronously (GIO looks up
/// the content type off the main thread).
fn open_default(path: &Path, window: &adw::ApplicationWindow) {
    let uri = gio::File::for_path(path).uri();
    let ctx = gtk::prelude::WidgetExt::display(window).app_launch_context();
    glib::spawn_future_local(async move {
        if let Err(e) = gio::AppInfo::launch_default_for_uri_future(&uri, Some(&ctx)).await {
            tracing::warn!("cannot open {uri}: {e}");
        }
    });
}
