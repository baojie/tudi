//! One browser window: header bar (history, path), folder tree on the left,
//! details list on the right, status bar. Owns navigation and wires worker
//! replies into the two models.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tudi_core::config::SortBy;
use tudi_core::listing::meta::{MetaFetcher, MetaStatus};
use tudi_core::listing::sort::{SortKey, SortSpec};
use tudi_core::state::WindowState;
use tudi_core::tree::{Expand, NodeId};

use crate::history::Snapshot;
use crate::i18n::{fill, ntr, tr};
use crate::pane::{self, PaneModel, Wanted};
use crate::shared::Shared;
use crate::sidebar::{self, TreeModel};
use crate::worker::{Job, Reply, WatchKey, Worker};

/// Most directories watched for live updates per window (PLAN §5.1).
const WATCH_LIMIT: usize = 512;
/// Most selected entries carried over a live reload.
const KEEP_SELECTION_LIMIT: usize = 5000;

/// Metadata worker threads per listing.
const META_WORKERS: usize = 4;
/// Rows fetched first after a listing arrives (about one screen).
const FIRST_SCREEN: usize = 80;
/// Above this many selected entries the status bar omits the size, so the
/// main thread never loops over a huge selection.
const STATUS_SIZE_LIMIT: u64 = 5000;

/// A window-level shortcut handler.
type Handler = Box<dyn Fn(&Rc<Browser>)>;

enum Walk {
    Reached(NodeId),
    /// Waiting for a listing.
    Pending,
    Missing,
}

/// A pending "reveal this path in the tree" that waits for listings.
struct Reveal {
    target: PathBuf,
}

pub struct Browser {
    pub window: adw::ApplicationWindow,
    pub(crate) shared: Rc<Shared>,
    pub(crate) search: crate::search::SearchUi,
    pub(crate) search_client: crate::search::SearchClient,
    /// Bookmarks bar: buttons, and the "»" menu with everything.
    pub(crate) bar_box: gtk::Box,
    pub(crate) bar_more: gtk::MenuButton,
    /// Latest folder history (most used folders, exclusions).
    pub(crate) hist: RefCell<Snapshot>,
    /// The settings dialog, while open.
    pub(crate) settings: RefCell<Option<std::rc::Weak<crate::settings::Settings>>>,
    settings_strong: RefCell<Option<Rc<crate::settings::Settings>>>,
    /// The folder the tree's context menu was opened on.
    pub(crate) tree_target: RefCell<Option<crate::tree_menu::Target>>,
    volume_monitor: RefCell<Option<gio::VolumeMonitor>>,
    /// Total size below the shown folder, once computed (≤ 1 s).
    folder_size: Cell<Option<u64>>,
    /// Free space on the shown folder's filesystem.
    free_space: Cell<Option<u64>>,
    /// Cancels the running size computation.
    size_cancel: RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>,
    /// Folder whose size is shown or being computed.
    size_for: RefCell<PathBuf>,
    /// The generation the size computation was started with (a live reload
    /// of the same folder must not make its result look stale).
    size_gen: Cell<u64>,
    zoom_toast: RefCell<Option<adw::Toast>>,
    paned: RefCell<Option<gtk::Paned>>,
    pub(crate) toasts: adw::ToastOverlay,
    pub(crate) typeahead: crate::actions::TypeAheadState,
    pub(crate) worker: Worker,
    watcher: Option<tudi_core::watch::Watcher<WatchKey>>,
    /// Tree nodes with a live watch (expanded and listed).
    watched: RefCell<std::collections::HashSet<NodeId>>,
    /// Scroll position to restore after a live reload of the pane.
    keep_scroll: Cell<Option<f64>>,

    pub(crate) tree: TreeModel,
    pub(crate) tree_sel: gtk::SingleSelection,
    pub(crate) tree_view: gtk::ListView,
    pub(crate) roots: RefCell<Vec<(PathBuf, NodeId)>>,
    reveal: RefCell<Option<Reveal>>,
    /// Folders to expand once their ancestors are listed (restored state).
    expand_paths: RefCell<Vec<PathBuf>>,
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
    pub fn new(
        app: &adw::Application,
        start: &Path,
        state: &WindowState,
        shared: Rc<Shared>,
    ) -> Rc<Self> {
        let config = shared.config.borrow().clone();
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
        crate::worker::PINYIN_SORT.store(
            config.browse.pinyin_sort,
            std::sync::atomic::Ordering::Relaxed,
        );
        let (worker, replies) = Worker::start();
        let tx = worker.reply_sender();
        let watcher = tudi_core::watch::Watcher::start(WATCH_LIMIT, move |keys| {
            let _ = tx.send_blocking(Reply::Changed(keys));
        })
        .map_err(|e| tracing::warn!("live updates disabled: {e}"))
        .ok();

        // --- tree
        let tree = TreeModel::default();
        let tree_sel = gtk::SingleSelection::new(Some(tree.clone()));
        tree_sel.set_autoselect(false);
        tree_sel.set_can_unselect(true);

        // --- pane
        let pane = PaneModel::default();
        let pane_sel = gtk::MultiSelection::new(Some(pane.clone()));
        let wanted: Wanted = Rc::default();
        // The list is built before the browser; its menu handler finds the
        // browser through this slot, filled in below.
        let menu_slot: Rc<RefCell<std::rc::Weak<Browser>>> = Rc::default();
        let slot = menu_slot.clone();
        let pane_view = pane::build_view(
            &pane,
            &pane_sel,
            &wanted,
            Rc::new(move |pos, w, x, y| {
                if let Some(b) = slot.borrow().upgrade() {
                    b.file_menu(pos, w, x, y);
                }
            }),
        );
        let (col_header, sort_buttons) = pane::build_header();
        let pane_scroll = gtk::ScrolledWindow::builder()
            .overlay_scrolling(false)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&pane_view)
            .build();

        // --- header bar
        let back = gtk::Button::from_icon_name("go-previous-symbolic");
        back.set_tooltip_text(Some(&tr("Back (Alt+Left)")));
        let forward = gtk::Button::from_icon_name("go-next-symbolic");
        forward.set_tooltip_text(Some(&tr("Forward (Alt+Right)")));
        let up = gtk::Button::from_icon_name("go-up-symbolic");
        up.set_tooltip_text(Some(&tr("Parent folder (Alt+Up)")));
        let path_entry = gtk::Entry::new();
        path_entry.set_hexpand(true);
        path_entry.set_width_chars(40);
        let header = adw::HeaderBar::new();
        header.pack_start(&back);
        header.pack_start(&forward);
        header.pack_start(&up);
        let gear = gtk::Button::from_icon_name("emblem-system-symbolic");
        gear.set_tooltip_text(Some(&tr("Settings (Ctrl+,)")));
        header.pack_end(&gear);
        header.set_title_widget(Some(&path_entry));

        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.add_css_class("dim-label");
        status.add_css_class("tudi-status");

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("tudi")
            .default_width(state.width)
            .default_height(state.height)
            .maximized(state.maximized)
            .build();
        window.add_css_class("tudi-window");

        let b = Rc::new_cyclic(|weak: &std::rc::Weak<Browser>| {
            let w = weak.clone();
            let w2 = weak.clone();
            let tree_view = sidebar::build_view(
                &tree,
                &tree_sel,
                move |pos| {
                    if let Some(b) = w.upgrade() {
                        b.toggle_tree_row(pos as usize);
                    }
                },
                move |pos, row, x, y| {
                    if let Some(b) = w2.upgrade() {
                        b.tree_menu(pos as usize, row, x, y);
                    }
                },
            );
            let search_client = crate::search::SearchClient::start(worker.reply_sender());
            Browser {
                window: window.clone(),
                shared,
                search: crate::search::SearchUi::new(),
                search_client,
                bar_box: gtk::Box::new(gtk::Orientation::Horizontal, 2),
                bar_more: gtk::MenuButton::new(),
                hist: RefCell::default(),
                settings: RefCell::default(),
                settings_strong: RefCell::default(),
                tree_target: RefCell::default(),
                volume_monitor: RefCell::default(),
                folder_size: Cell::new(None),
                free_space: Cell::new(None),
                size_cancel: RefCell::default(),
                size_for: RefCell::default(),
                size_gen: Cell::new(0),
                zoom_toast: RefCell::default(),
                paned: RefCell::default(),
                toasts: adw::ToastOverlay::new(),
                typeahead: Rc::default(),
                worker,
                watcher,
                watched: RefCell::default(),
                keep_scroll: Cell::new(None),
                tree,
                tree_sel,
                tree_view,
                roots: RefCell::default(),
                reveal: RefCell::default(),
                expand_paths: RefCell::new(state.expanded.iter().map(PathBuf::from).collect()),
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
            .overlay_scrolling(false)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&b.tree_view)
            .build();
        tree_scroll.add_css_class("tudi-tree-scroll");
        let files = gtk::Box::new(gtk::Orientation::Vertical, 0);
        files.append(&col_header);
        files.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        files.append(&b.pane_scroll);
        let right = &files;
        // The divider between the panes is draggable; a wide handle makes it
        // easy to grab (the theme's default is a 1 px line).
        tree_scroll.set_size_request(120, -1);
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&tree_scroll)
            .end_child(right)
            .position(state.tree_width.max(120))
            .shrink_start_child(false)
            .resize_start_child(false)
            .shrink_end_child(false)
            .wide_handle(true)
            .vexpand(true)
            .build();
        b.paned.replace(Some(paned.clone()));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&header);
        content.append(&b.bar_widget());
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        // The search overlay floats above the panes.
        let overlay = gtk::Overlay::builder().child(&paned).vexpand(true).build();
        overlay.add_overlay(&b.search.container);
        content.append(&overlay);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        content.append(&b.status);
        b.toasts.set_child(Some(&content));
        b.window.set_content(Some(&b.toasts));

        b.add_roots();
        // Volumes after the first frame: GIO's volume monitor talks to
        // udisks over D-Bus.
        let w = Rc::downgrade(&b);
        glib::idle_add_local_once(move || {
            if let Some(b) = w.upgrade() {
                b.watch_volumes();
            }
        });
        b.build_bar();
        b.connect_search();
        b.connect_signals(&up);
        b.install_shortcuts();
        b.install_drag_source();
        *menu_slot.borrow_mut() = Rc::downgrade(&b);
        b.install_file_actions();
        b.install_tree_actions();
        let w = Rc::downgrade(&b);
        gear.connect_clicked(move |_| {
            if let Some(b) = w.upgrade() {
                b.open_settings();
            }
        });
        b.spawn_reply_loop(replies);
        b.update_sort_buttons();
        b.navigate(start, true);
        b
    }

    fn add_roots(&self) {
        let home = home();
        let mut roots = self.roots.borrow_mut();
        let id = self.tree.add_root(&home.to_string_lossy(), &tr("Home"));
        roots.push((home, id));
        let id = self.tree.add_root("/", &tr("File System"));
        roots.push((PathBuf::from("/"), id));
    }

    /// Mounted volumes (USB disks, other partitions) as tree roots, kept up
    /// to date as they are mounted and unmounted.
    fn watch_volumes(self: &Rc<Self>) {
        let monitor = gio::VolumeMonitor::get();
        for m in monitor.mounts() {
            self.add_volume(&m);
        }
        let w = Rc::downgrade(self);
        monitor.connect_mount_added(move |_, m| {
            if let Some(b) = w.upgrade() {
                b.add_volume(m);
            }
        });
        let w = Rc::downgrade(self);
        monitor.connect_mount_removed(move |_, m| {
            if let Some(b) = w.upgrade() {
                b.remove_volume(m);
            }
        });
        // Keep the monitor (and its signals) alive with the window.
        self.volume_monitor.replace(Some(monitor));
    }

    fn add_volume(&self, m: &gio::Mount) {
        if m.is_shadowed() {
            return;
        }
        let Some(path) = m.root().path() else { return };
        if path == Path::new("/") || self.roots.borrow().iter().any(|(p, _)| *p == path) {
            return;
        }
        let id = self.tree.add_root(&path.to_string_lossy(), &m.name());
        self.roots.borrow_mut().push((path, id));
    }

    fn remove_volume(&self, m: &gio::Mount) {
        let Some(path) = m.root().path() else { return };
        let id = {
            let mut roots = self.roots.borrow_mut();
            let Some(i) = roots.iter().position(|(p, _)| *p == path) else {
                return;
            };
            roots.remove(i).1
        };
        self.unwatch_tree_node(id);
        self.tree.remove_root(id);
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
            // Count it only if the user stays (not passing over with keys).
            b.record_visit_after_dwell(path);
        });

        // Double-click (or Enter) on a tree folder expands or collapses it;
        // a single click already shows it in the list.
        let w = Rc::downgrade(self);
        self.tree_view.connect_activate(move |_, pos| {
            if let Some(b) = w.upgrade() {
                b.toggle_tree_row(pos as usize);
            }
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
                b.record_visit(&p);
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
        add("<Control>d", Box::new(|b| b.bookmark_or_launcher()));
        add("<Control>p", Box::new(|b| b.open_search()));
        add("<Control><Shift>f", Box::new(|b| b.open_search_here()));
        add("<Control>comma", Box::new(|b| b.open_settings()));
        for n in 1..=9usize {
            add(
                &format!("<Alt>{n}"),
                Box::new(move |b| b.open_bookmark(n - 1)),
            );
        }
        for accel in ["<Control>equal", "<Control>plus", "<Control>KP_Add"] {
            add(accel, Box::new(|b| b.show_zoom(crate::zoom::zoom_in())));
        }
        for accel in ["<Control>minus", "<Control>KP_Subtract"] {
            add(accel, Box::new(|b| b.show_zoom(crate::zoom::zoom_out())));
        }
        for accel in ["<Control>0", "<Control>KP_0"] {
            add(accel, Box::new(|b| b.show_zoom(crate::zoom::reset())));
        }
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
        add("<Control>x", Box::new(|b| b.cut_files()));
        add("<Control>v", Box::new(|b| b.paste()));
        add("<Control>a", Box::new(|b| b.select_all()));
        add("<Alt>Return", Box::new(|b| b.show_properties()));
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
            if key == gdk::Key::slash && mods.is_empty() {
                b.open_search();
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
            select: self
                .select_after_load
                .borrow_mut()
                .take()
                .into_iter()
                .collect(),
        });
        if let Some(w) = &self.watcher {
            w.watch(WatchKey::Pane, path.clone(), true);
        }
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
        let nodes: Vec<NodeId> = self.watched.borrow().iter().copied().collect();
        self.refresh_tree_nodes(&nodes);
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
            self.record_visit(&path);
        } else {
            open_default(&path, &self.window);
        }
    }

    // --------------------------------------------------------------- tree

    pub(crate) fn toggle_tree_row(self: &Rc<Self>, pos: usize) {
        let expanded = {
            let tree = self.tree.tree();
            match tree.rows().get(pos) {
                // An error row stands for its parent; ignore it.
                Some(r) if r.kind == tudi_core::tree::RowKind::Error => return,
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
        match outcome {
            Expand::NeedsListing(node) => self.list_tree_node(node),
            Expand::Spliced(_) => self.watch_tree_node(node),
            Expand::Nothing => {}
        }
    }

    fn watch_tree_node(&self, node: NodeId) {
        if let Some(w) = &self.watcher
            && self.watched.borrow_mut().insert(node)
        {
            w.watch(WatchKey::Tree(node), self.tree.tree().path(node), false);
        }
    }

    fn unwatch_tree_node(&self, node: NodeId) {
        if let Some(w) = &self.watcher
            && self.watched.borrow_mut().remove(&node)
        {
            w.unwatch(WatchKey::Tree(node));
        }
    }

    /// Re-list tree nodes (live update or hidden-files toggle).
    fn refresh_tree_nodes(&self, nodes: &[NodeId]) {
        for &node in nodes {
            self.worker.send(Job::ListTree {
                node,
                path: self.tree.tree().path(node),
                show_hidden: self.show_hidden.get(),
                refresh: true,
            });
        }
    }

    pub(crate) fn collapse_tree_row(&self, pos: usize) {
        let node = self.tree.tree().rows()[pos].node;
        self.tree.update(Some(node), |t| ((), t.collapse(pos)));
        self.unwatch_tree_node(node);
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
            refresh: false,
        });
    }

    /// Expand the tree down to `target` and select it, listing directories
    /// as needed (continues in [`Self::continue_reveal`]).
    fn reveal_in_tree(self: &Rc<Self>, target: PathBuf) {
        *self.reveal.borrow_mut() = Some(Reveal { target });
        self.continue_reveal();
    }

    /// Walk the tree towards `target`, expanding (and listing) folders on
    /// the way, and the target itself if `expand_target`.
    fn walk_to(self: &Rc<Self>, target: &Path, expand_target: bool) -> Walk {
        use tudi_core::tree::Children;
        // The root with the longest matching prefix.
        let Some((root_path, mut node)) = self
            .roots
            .borrow()
            .iter()
            .filter(|(p, _)| target.starts_with(p))
            .max_by_key(|(p, _)| p.as_os_str().len())
            .cloned()
        else {
            return Walk::Missing;
        };
        let rest: Vec<_> = target
            .strip_prefix(&root_path)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect();
        for comp in rest {
            if !self.ensure_expanded(node) {
                return Walk::Missing;
            }
            let child = {
                let tree = self.tree.tree();
                match tree.node(node).children() {
                    Children::Loading => return Walk::Pending,
                    Children::Loaded(kids) => kids
                        .iter()
                        .copied()
                        .find(|&k| tree.node(k).name() == comp.as_os_str()),
                    _ => None,
                }
            };
            match child {
                Some(c) => node = c,
                // Hidden (and hidden files are off), deleted, or not listed.
                None => return Walk::Missing,
            }
        }
        if expand_target && !self.ensure_expanded(node) {
            return Walk::Missing;
        }
        Walk::Reached(node)
    }

    /// Expand `node` if it is collapsed. False if it is not visible.
    fn ensure_expanded(self: &Rc<Self>, node: NodeId) -> bool {
        if self.tree.tree().node(node).is_expanded() {
            return true;
        }
        let Some(row) = self.tree.tree().row_of(node) else {
            return false;
        };
        self.expand_tree_row(row);
        true
    }

    /// Advance the pending reveal and restored expansions after a listing.
    fn continue_reveal(self: &Rc<Self>) {
        let pending: Vec<PathBuf> = std::mem::take(&mut *self.expand_paths.borrow_mut());
        let still: Vec<PathBuf> = pending
            .into_iter()
            .filter(|p| matches!(self.walk_to(p, true), Walk::Pending))
            .collect();
        self.expand_paths.borrow_mut().extend(still);

        let Some(target) = self.reveal.borrow().as_ref().map(|r| r.target.clone()) else {
            return;
        };
        let found = match self.walk_to(&target, false) {
            Walk::Pending => return, // continues on the next tree reply
            Walk::Reached(node) => self.tree.tree().row_of(node),
            Walk::Missing => None,
        };
        self.reveal.replace(None);
        self.syncing.set(true);
        match found {
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

    /// The list shows the listing of the current navigation.
    pub(crate) fn pane_settled(&self) -> bool {
        let d = self.pane.data();
        d.generation == self.generation.get() && d.listing.is_some()
    }

    pub(crate) fn show_zoom(&self, level: u32) {
        // Replace the previous zoom toast instead of stacking them.
        if let Some(t) = self.zoom_toast.take() {
            t.dismiss();
        }
        let t = adw::Toast::new(&fill(&tr("Zoom {}%"), &[&level]));
        t.set_timeout(1);
        self.toasts.add_toast(t.clone());
        self.zoom_toast.replace(Some(t));
    }

    /// Window state to remember: size, current folder, expanded folders.
    pub fn snapshot_state(&self) -> WindowState {
        let mut st = WindowState {
            zoom: crate::zoom::level(),
            tree_width: self.paned.borrow().as_ref().map_or(280, |p| p.position()),
            width: self.window.default_width(),
            height: self.window.default_height(),
            maximized: self.window.is_maximized(),
            ..Default::default()
        };
        let tree = self.tree.tree();
        let mut expanded: Vec<PathBuf> = self
            .watched
            .borrow()
            .iter()
            .filter(|&&n| tree.node(n).is_expanded())
            .map(|&n| tree.path(n))
            .collect();
        // Parents first, so restoring expands top-down; stable file order.
        expanded.sort();
        st.set_paths(&self.current.borrow(), &expanded);
        st
    }

    // ----------------------------------------------------------- settings

    pub(crate) fn open_settings(self: &Rc<Self>) {
        let open = self.settings.borrow().as_ref().and_then(|s| s.upgrade());
        if open.is_some() {
            return;
        }
        let s = crate::settings::Settings::open(self);
        // The dialog's own handlers keep it alive; hold it strongly while
        // shown (dropped on close).
        self.settings_strong.replace(Some(s.clone()));
        self.settings.replace(Some(Rc::downgrade(&s)));
    }

    pub(crate) fn drop_settings(&self) {
        // Not inside the dialog's own signal emission.
        let s = self.settings_strong.take();
        glib::idle_add_local_once(move || drop(s));
    }

    /// Apply changed browsing settings (from the settings dialog).
    pub(crate) fn apply_browse(self: &Rc<Self>, browse: &tudi_core::config::Browse) {
        let mut spec = self.spec.get();
        spec.dirs_first = browse.dirs_first;
        spec.key = match browse.sort {
            SortBy::Name => SortKey::Name,
            SortBy::Size => SortKey::Size,
            SortBy::Mtime => SortKey::Modified,
        };
        spec.descending = browse.sort != SortBy::Name;
        self.spec.set(spec);
        self.update_sort_buttons();
        let pinyin = &crate::worker::PINYIN_SORT;
        if pinyin.swap(browse.pinyin_sort, std::sync::atomic::Ordering::Relaxed)
            != browse.pinyin_sort
        {
            // Name ranks are made when listing: list again.
            self.reload(None);
        } else if self.show_hidden.get() != browse.show_hidden {
            self.toggle_hidden();
        } else {
            self.request_sort();
        }
    }

    // ------------------------------------------------------------ history

    /// Record a deliberate visit of a folder (ADR-016) and refresh the bar.
    pub(crate) fn record_visit(&self, path: &Path) {
        self.shared.history.visit(path.to_path_buf());
        self.shared.history.query(self.worker.reply_sender());
    }

    /// Record a visit if the folder is still shown after one second, so
    /// moving through the tree with the keyboard does not count.
    fn record_visit_after_dwell(self: &Rc<Self>, path: PathBuf) {
        let w = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_secs(1), move || {
            if let Some(b) = w.upgrade()
                && *b.current.borrow() == path
            {
                b.record_visit(&path);
            }
        });
    }

    // Test hooks for TUDI_DEV_SCRIPT.
    pub(crate) fn dev_bar_labels(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut c = self.bar_box.first_child();
        while let Some(w) = c {
            if w.is::<gtk::Separator>() {
                out.push("‖".into());
            } else if let Some(b) = w.downcast_ref::<gtk::Button>() {
                let label = b
                    .child()
                    .and_then(|bx| bx.last_child())
                    .and_downcast::<gtk::Label>()
                    .map(|l| l.text().to_string())
                    .unwrap_or_default();
                out.push(label);
            }
            c = w.next_sibling();
        }
        out
    }

    pub(crate) fn dev_set_tree_width(&self, w: i32) {
        if let Some(p) = &*self.paned.borrow() {
            p.set_position(w);
        }
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
        let labels = [tr("Name"), tr("Size"), tr("Modified")];
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
        let (files, dirs) = (u64::from(d.files), u64::from(d.dirs));
        let mut text = fill(&ntr("{} file", "{} files", files), &[&fmt_count(files)]);
        text.push_str(", ");
        text.push_str(&fill(
            &ntr("{} folder", "{} folders", dirs),
            &[&fmt_count(dirs)],
        ));
        if let Some(size) = self.folder_size.get() {
            text.push_str(" · ");
            text.push_str(&crate::format::size(size));
        }
        if let Some(free) = self.free_space.get() {
            text.push_str(" · ");
            text.push_str(&fill(&tr("{} free"), &[&crate::format::size(free)]));
        }
        let selected = self.pane_sel.selection().size();
        if selected > 0 {
            text.push_str(" · ");
            text.push_str(&fill(&tr("{} selected"), &[&fmt_count(selected)]));
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
                self.start_folder_size(p.generation);
            }
            Reply::FolderSize {
                generation,
                size,
                available,
            } => {
                if generation == self.size_gen.get() {
                    self.folder_size.set(size);
                    self.free_space.set(available);
                    self.update_status();
                }
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
                    self.after_order_change(keep, Vec::new());
                }
            }
            Reply::Meta(batch) => {
                if self.pane.apply_meta(&batch) {
                    self.request_sort();
                }
                if batch.complete {
                    // Free accuracy for the search index: what the user
                    // looks at is always current there.
                    let d = self.pane.data();
                    if d.generation == batch.generation
                        && let Some(l) = d.listing.clone()
                    {
                        // MetaTable is a flat Vec: this clone is a memcpy.
                        self.search_client
                            .hint(self.current.borrow().clone(), l, d.metas.clone());
                    }
                }
                if batch.complete || self.pane_sel.selection().size() > 0 {
                    self.update_status();
                }
            }
            Reply::Tree {
                node,
                refresh,
                result,
            } => {
                let ok = result.is_ok();
                self.tree.update(Some(node), |t| {
                    let s = if refresh {
                        t.refresh_children(node, result)
                    } else {
                        t.set_children(node, result)
                    };
                    ((), s)
                });
                if ok && self.tree.tree().node(node).is_expanded() {
                    self.watch_tree_node(node);
                }
                self.continue_reveal();
            }
            Reply::Search {
                id,
                items,
                partial,
                error,
            } => self.search_results(id, items, partial, error),
            Reply::History(snap) => {
                *self.hist.borrow_mut() = snap;
                self.rebuild_bar();
                let dialog = self.settings.borrow().as_ref().and_then(|w| w.upgrade());
                if let Some(d) = dialog {
                    d.refresh_exclusions(&self.hist.borrow());
                }
            }
            Reply::Changed(keys) => {
                let mut tree_nodes = Vec::new();
                for k in keys {
                    match k {
                        WatchKey::Pane => self.live_reload_pane(),
                        WatchKey::Tree(n) => tree_nodes.push(n),
                    }
                }
                self.refresh_tree_nodes(&tree_nodes);
            }
        }
    }

    /// Lazily compute the total size of a newly shown folder (live reloads
    /// of the same folder keep the last value).
    fn start_folder_size(&self, generation: u64) {
        let cur = self.current.borrow().clone();
        if *self.size_for.borrow() == cur {
            return;
        }
        if let Some(c) = self.size_cancel.take() {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.folder_size.set(None);
        *self.size_for.borrow_mut() = cur.clone();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.size_cancel.replace(Some(cancel.clone()));
        self.size_gen.set(generation);
        self.worker.folder_size(cur, generation, cancel);
    }

    /// Re-list the current folder after it changed on disk, keeping the
    /// selection (by name) and the scroll position.
    fn live_reload_pane(&self) {
        let selected = self.selected_entries();
        let select = if selected.len() <= KEEP_SELECTION_LIMIT {
            let d = self.pane.data();
            selected.iter().map(|&e| d.name(e).to_vec()).collect()
        } else {
            Vec::new()
        };
        self.keep_scroll
            .set(Some(self.pane_scroll.vadjustment().value()));
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.worker.send(Job::ListPane {
            path: self.current.borrow().clone(),
            generation,
            spec: self.spec.get(),
            show_hidden: self.show_hidden.get(),
            select,
        });
    }

    /// Restore selection after the rows were replaced: the given entries,
    /// or the rows at `select` (focusing the first unless the scroll
    /// position is being kept).
    fn after_order_change(&self, keep: Vec<u32>, select: Vec<u32>) {
        let scroll = self.keep_scroll.take();
        if !select.is_empty() {
            let bits = gtk::Bitset::new_empty();
            for &p in &select {
                bits.add(p);
            }
            self.pane_sel
                .set_selection(&bits, &gtk::Bitset::new_range(0, self.pane.n_items()));
            if scroll.is_none() {
                self.pane_view
                    .scroll_to(select[0], gtk::ListScrollFlags::FOCUS, None);
            }
        } else if !keep.is_empty() {
            self.select_entries(&keep);
        } else {
            // Items are positions: a selection left from the previous order
            // would point at other entries.
            self.pane_sel.unselect_all();
            if scroll.is_none() {
                self.pane_scroll.vadjustment().set_value(0.0);
            }
        }
        if let Some(v) = scroll {
            // Wait for the new rows' layout before restoring the position.
            let sw = self.pane_scroll.downgrade();
            let frames = Cell::new(0);
            self.pane_view.add_tick_callback(move |_, _| {
                if let Some(sw) = sw.upgrade() {
                    sw.vadjustment().set_value(v);
                }
                frames.set(frames.get() + 1);
                if frames.get() < 2 {
                    glib::ControlFlow::Continue
                } else {
                    glib::ControlFlow::Break
                }
            });
        }
        self.update_status();
    }
}

pub(crate) fn fmt_count(n: u64) -> String {
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
pub(crate) fn open_default(path: &Path, window: &adw::ApplicationWindow) {
    let uri = gio::File::for_path(path).uri();
    let ctx = gtk::prelude::WidgetExt::display(window).app_launch_context();
    glib::spawn_future_local(async move {
        if let Err(e) = gio::AppInfo::launch_default_for_uri_future(&uri, Some(&ctx)).await {
            tracing::warn!("cannot open {uri}: {e}");
        }
    });
}
