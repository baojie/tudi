//! Memory search overlay (PLAN §5.7, M6): `Ctrl+P` or `/` opens a search
//! box above the panes with live results from the index daemon.
//!
//! - A client thread talks to `tudi-indexd` (starting it if needed); only
//!   the latest pending query is sent, so fast typing never queues up.
//! - Results: icon, name with matched characters in bold, folder dimmed.
//! - `↑`/`↓` select, `Enter` reveals (folder in the tree, file selected in
//!   the list), `Ctrl+Enter` opens, `Alt+Enter` opens a terminal in the
//!   containing folder, `Esc` closes.

use std::cell::{Cell, RefCell};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, gio, glib, pango};
use tudi_proto::{Client, Request, Response, ResultItem};

use crate::browser::Browser;
use crate::i18n::{fill, tr};
use crate::worker::Reply;

/// Results requested per query.
const LIMIT: u32 = 100;

// --------------------------------------------------------------- client

enum Msg {
    /// id, text, show hidden, scope folder.
    Query(u64, String, bool, Option<PathBuf>),
    /// A folder listing with complete metadata, for the index.
    Hint(
        PathBuf,
        std::sync::Arc<tudi_core::listing::Listing>,
        tudi_core::listing::meta::MetaTable,
    ),
}

/// Handle to the search client thread (one per window). Also forwards
/// folder listings to the daemon as hints (only if it is running).
pub struct SearchClient {
    tx: std::sync::mpsc::Sender<Msg>,
}

/// Turn a listing into a hint for the daemon (on the client thread).
fn hint_request(
    dir: &std::path::Path,
    l: &tudi_core::listing::Listing,
    m: &tudi_core::listing::meta::MetaTable,
) -> Request {
    use std::os::unix::ffi::OsStrExt;
    use tudi_core::listing::Kind;
    let entries = (0..l.len())
        .map(|i| {
            let meta = m.get(i);
            tudi_proto::HintEntry {
                name: l.name(i).to_vec(),
                kind: match m.kind(l, i) {
                    Kind::Dir => 1,
                    Kind::Symlink => 2,
                    Kind::File | Kind::Unknown => 0,
                    Kind::Other => 3,
                },
                size: meta.size,
                mtime: meta.mtime,
            }
        })
        .collect();
    Request::ListingHint {
        dir: dir.as_os_str().as_bytes().to_vec(),
        entries,
    }
}

fn start_daemon() {
    let exe = std::env::current_exe()
        .map(|p| p.with_file_name("tudi-indexd"))
        .unwrap_or_else(|_| "tudi-indexd".into());
    let _ = std::process::Command::new(exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn();
}

fn connect() -> Option<Client> {
    if let Ok(c) = Client::connect() {
        return Some(c);
    }
    start_daemon();
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Ok(c) = Client::connect() {
            return Some(c);
        }
    }
    None
}

impl SearchClient {
    pub fn start(replies: async_channel::Sender<Reply>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        std::thread::Builder::new()
            .name("tudi-search".into())
            .spawn(move || {
                let mut client: Option<Client> = None;
                while let Ok(first) = rx.recv() {
                    // Hints go out in order; of queries only the latest
                    // pending one matters.
                    let mut query = None;
                    let mut pending = vec![first];
                    while let Ok(m) = rx.try_recv() {
                        pending.push(m);
                    }
                    for m in pending {
                        match m {
                            Msg::Query(id, text, hidden, scope) => {
                                query = Some((id, text, hidden, scope));
                            }
                            Msg::Hint(dir, l, meta) => {
                                // Never start the daemon just for a hint.
                                if client.is_none() {
                                    client = Client::connect().ok();
                                }
                                if let Some(c) = client.as_mut()
                                    && c.call(&hint_request(&dir, &l, &meta)).is_err()
                                {
                                    client = None;
                                }
                            }
                        }
                    }
                    let Some((id, text, hidden, scope)) = query else {
                        continue;
                    };
                    if client.is_none() {
                        client = connect();
                    }
                    let req = Request::Query {
                        id,
                        text,
                        limit: LIMIT,
                        show_hidden: hidden,
                        dirs_only: false,
                        scope: scope
                            .as_ref()
                            .map(|p| p.as_os_str().as_encoded_bytes().to_vec()),
                    };
                    let resp = client.as_mut().map(|c| c.call(&req));
                    let reply = match resp {
                        Some(Ok(Response::Results {
                            id, items, partial, ..
                        })) => Reply::Search {
                            id,
                            items,
                            partial,
                            error: None,
                        },
                        // Only a scope outside the index is refused.
                        Some(Ok(Response::Error(_))) if scope.is_some() => Reply::Search {
                            id,
                            items: Vec::new(),
                            partial: false,
                            error: Some(tr("This folder is not in the search index")),
                        },
                        Some(Ok(_)) | Some(Err(_)) => {
                            client = None; // reconnect next time
                            Reply::Search {
                                id,
                                items: Vec::new(),
                                partial: false,
                                error: Some(tr("The search index is not available")),
                            }
                        }
                        None => Reply::Search {
                            id,
                            items: Vec::new(),
                            partial: false,
                            error: Some(tr("Cannot start the search index (tudi-indexd)")),
                        },
                    };
                    if replies.send_blocking(reply).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn search thread");
        Self { tx }
    }

    pub fn query(&self, id: u64, text: String, hidden: bool, scope: Option<PathBuf>) {
        let _ = self.tx.send(Msg::Query(id, text, hidden, scope));
    }

    /// Send a folder listing (with complete metadata) to the daemon.
    pub fn hint(
        &self,
        dir: PathBuf,
        listing: std::sync::Arc<tudi_core::listing::Listing>,
        metas: tudi_core::listing::meta::MetaTable,
    ) {
        let _ = self.tx.send(Msg::Hint(dir, listing, metas));
    }
}

// ---------------------------------------------------------------- model

mod row_imp {
    use super::*;

    #[derive(Default)]
    pub struct SearchRow {
        pub index: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchRow {
        const NAME: &'static str = "TudiSearchRow";
        type Type = super::SearchRow;
    }

    impl ObjectImpl for SearchRow {}
}

glib::wrapper! {
    pub struct SearchRow(ObjectSubclass<row_imp::SearchRow>);
}

mod model_imp {
    use super::*;

    #[derive(Default)]
    pub struct SearchModel {
        pub items: RefCell<Vec<ResultItem>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchModel {
        const NAME: &'static str = "TudiSearchModel";
        type Type = super::SearchModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for SearchModel {}

    impl ListModelImpl for SearchModel {
        fn item_type(&self) -> glib::Type {
            SearchRow::static_type()
        }
        fn n_items(&self) -> u32 {
            self.items.borrow().len() as u32
        }
        fn item(&self, position: u32) -> Option<glib::Object> {
            if position as usize >= self.items.borrow().len() {
                return None;
            }
            let o: SearchRow = glib::Object::new();
            o.imp().index.set(position);
            Some(o.upcast())
        }
    }
}

glib::wrapper! {
    /// At most `LIMIT` results: a plain replace is fine.
    pub struct SearchModel(ObjectSubclass<model_imp::SearchModel>)
        @implements gio::ListModel;
}

impl Default for SearchModel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl SearchModel {
    fn set(&self, items: Vec<ResultItem>) {
        let removed = self.n_items();
        let added = items.len() as u32;
        *self.imp().items.borrow_mut() = items;
        self.items_changed(0, removed, added);
    }

    pub fn get(&self, i: u32) -> Option<ResultItem> {
        self.imp().items.borrow().get(i as usize).cloned()
    }
}

/// Name with the matched characters in bold (Pango markup).
fn highlighted(name: &str, indices: &[u32]) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        let esc = glib::markup_escape_text(&c.to_string());
        if indices.binary_search(&(i as u32)).is_ok() {
            out.push_str("<b>");
            out.push_str(&esc);
            out.push_str("</b>");
        } else {
            out.push_str(&esc);
        }
    }
    out
}

// ----------------------------------------------------------------- view

/// The overlay's widgets.
pub struct SearchUi {
    pub container: gtk::Box,
    pub entry: gtk::SearchEntry,
    pub list: gtk::ListView,
    pub sel: gtk::SingleSelection,
    pub model: SearchModel,
    pub status: gtk::Label,
    /// Problems with the query, e.g. a bad date filter.
    pub hint: gtk::Label,
    /// Scoped search (`Ctrl+Shift+F`): the folder and its chip.
    pub scope: RefCell<Option<PathBuf>>,
    pub scope_bar: gtk::Box,
    pub scope_label: gtk::Label,
    pub scope_clear: gtk::Button,
    pub serial: Cell<u64>,
}

/// A query problem in the UI language.
fn hint_text(h: &tudi_index::query::Hint) -> String {
    use tudi_index::query::Hint;
    match h {
        Hint::InNeedsName => tr("“in:” needs a folder name, e.g. in:acme"),
        Hint::ExtNeedsValue => tr("“ext:” needs an extension, e.g. ext:pdf"),
        Hint::BadDate(t) => fill(
            &tr("“{}”: dates look like 2025, 2025-06 or 2025-06-15"),
            &[t],
        ),
        Hint::MonthIsMo(t) => fill(
            &tr("“{}”: months are “mo” (<3mo); for sizes use size>10M"),
            &[t],
        ),
        Hint::AgeNeedsUnit(t) => fill(&tr("“{}”: add a unit: h, d, w, mo or y"), &[t]),
        Hint::BadAge(t) => fill(&tr("“{}”: write <7d, >2w or >2025-06"), &[t]),
        Hint::BadSize(t) => fill(&tr("“{}”: write size>10M or size<500k"), &[t]),
    }
}

impl SearchUi {
    pub fn new() -> Self {
        let model = SearchModel::default();
        let sel = gtk::SingleSelection::new(Some(model.clone()));
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let icon = crate::icons::FileIcon::default();
            let texts = gtk::Box::new(gtk::Orientation::Vertical, 0);
            texts.set_hexpand(true);
            let name = gtk::Label::new(None);
            name.set_xalign(0.0);
            name.set_ellipsize(pango::EllipsizeMode::Middle);
            let dir = gtk::Label::new(None);
            dir.set_xalign(0.0);
            dir.set_ellipsize(pango::EllipsizeMode::Start);
            dir.add_css_class("dim-label");
            dir.add_css_class("caption");
            texts.append(&name);
            texts.append(&dir);
            let meta = gtk::Label::new(None);
            meta.add_css_class("dim-label");
            meta.add_css_class("numeric");
            row.append(&icon);
            row.append(&texts);
            row.append(&meta);
            item.set_child(Some(&row));
        });
        let m = model.clone();
        factory.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = item.child().and_downcast::<gtk::Box>().unwrap();
            let i = item
                .item()
                .and_downcast::<SearchRow>()
                .unwrap()
                .imp()
                .index
                .get();
            let Some(r) = m.get(i) else { return };
            let icon = row
                .first_child()
                .and_downcast::<crate::icons::FileIcon>()
                .unwrap();
            let texts = icon.next_sibling().and_downcast::<gtk::Box>().unwrap();
            let name = texts.first_child().and_downcast::<gtk::Label>().unwrap();
            let dir = name.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let meta = texts.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let path = r.path();
            let fname = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let kind = if r.is_dir() {
                crate::icons::Kind::Folder
            } else {
                crate::icons::Kind::File(crate::icons::category(fname.as_bytes()))
            };
            icon.set(kind, r.kind == 2);
            name.set_markup(&highlighted(&fname, &r.indices));
            dir.set_text(
                &path
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
            let now = glib::DateTime::now_local().expect("local time");
            let date = crate::format::date(r.mtime, &now);
            meta.set_text(&if r.is_dir() {
                date
            } else {
                format!("{}   {}", crate::format::size(r.size), date)
            });
        });
        let list = gtk::ListView::new(Some(sel.clone()), Some(factory));
        list.add_css_class("tudi-search-results");
        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some(&tr(
            "Search files and folders…  (pinyin initials work too)",
        )));
        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.add_css_class("dim-label");
        status.add_css_class("caption");
        let hint = gtk::Label::new(None);
        hint.set_xalign(0.0);
        hint.set_wrap(true);
        hint.add_css_class("warning");
        hint.add_css_class("caption");
        hint.set_visible(false);
        let scope_label = gtk::Label::new(None);
        scope_label.set_ellipsize(pango::EllipsizeMode::Middle);
        scope_label.set_max_width_chars(60);
        let scope_clear = gtk::Button::from_icon_name("window-close-symbolic");
        scope_clear.add_css_class("flat");
        scope_clear.add_css_class("circular");
        scope_clear.set_tooltip_text(Some(&tr("Search everywhere")));
        let scope_bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        scope_bar.add_css_class("tudi-search-scope");
        scope_bar.set_halign(gtk::Align::Start);
        scope_bar.append(&gtk::Image::from_icon_name("folder-symbolic"));
        scope_bar.append(&scope_label);
        scope_bar.append(&scope_clear);
        scope_bar.set_visible(false);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(80)
            .max_content_height(460)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
        container.add_css_class("tudi-search");
        container.add_css_class("card");
        container.set_halign(gtk::Align::Center);
        container.set_valign(gtk::Align::Start);
        container.set_margin_top(12);
        container.set_size_request(680, -1);
        container.append(&scope_bar);
        container.append(&entry);
        container.append(&hint);
        container.append(&scroll);
        container.append(&status);
        container.set_visible(false);
        Self {
            container,
            entry,
            list,
            sel,
            model,
            status,
            hint,
            scope: RefCell::new(None),
            scope_bar,
            scope_label,
            scope_clear,
            serial: Cell::new(0),
        }
    }
}

impl Browser {
    /// `Ctrl+P` / `/`: search everywhere.
    pub(crate) fn open_search(self: &Rc<Self>) {
        self.set_search_scope(None);
        self.show_search();
    }

    /// `Ctrl+Shift+F`: search below the selected folder, or the current
    /// one.
    pub(crate) fn open_search_here(self: &Rc<Self>) {
        let tab = self.tab();
        let d = tab.pane.data();
        let dir = self
            .selected_entries()
            .first()
            .filter(|&&e| d.is_dir(e))
            .map(|&e| {
                use std::os::unix::ffi::OsStrExt;
                self.current_dir()
                    .join(std::ffi::OsStr::from_bytes(d.name(e)))
            })
            .unwrap_or_else(|| self.current_dir());
        drop(d);
        self.set_search_scope(Some(dir));
        self.show_search();
    }

    /// Set or clear the search scope and re-run the query.
    pub(crate) fn set_search_scope(self: &Rc<Self>, dir: Option<PathBuf>) {
        let ui = &self.search;
        match &dir {
            Some(d) => {
                let name = d.file_name().map_or_else(
                    || d.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                ui.scope_label.set_text(&fill(&tr("In {}"), &[&name]));
                ui.scope_bar
                    .set_tooltip_text(Some(&d.display().to_string()));
                ui.scope_bar.set_visible(true);
            }
            None => ui.scope_bar.set_visible(false),
        }
        let changed = *ui.scope.borrow() != dir;
        *ui.scope.borrow_mut() = dir;
        if changed && !ui.entry.text().is_empty() {
            self.run_search();
        }
    }

    fn show_search(self: &Rc<Self>) {
        let ui = &self.search;
        ui.container.set_visible(true);
        ui.entry.grab_focus();
        ui.entry.select_region(0, -1);
    }

    pub(crate) fn close_search(&self) {
        self.search.container.set_visible(false);
        self.tab().pane_view.grab_focus();
    }

    pub(crate) fn connect_search(self: &Rc<Self>) {
        let ui = &self.search;
        let w = Rc::downgrade(self);
        ui.entry.connect_search_changed(move |_| {
            if let Some(b) = w.upgrade() {
                b.run_search();
            }
        });
        let w = Rc::downgrade(self);
        ui.scope_clear.connect_clicked(move |_| {
            if let Some(b) = w.upgrade() {
                b.set_search_scope(None);
                b.search.entry.grab_focus();
            }
        });
        let w = Rc::downgrade(self);
        ui.entry.connect_stop_search(move |_| {
            if let Some(b) = w.upgrade() {
                b.close_search();
            }
        });
        // Arrows and Enter from the entry drive the list.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(b) = w.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let ui = &b.search;
            let n = ui.model.n_items();
            let cur = ui.sel.selected();
            match key {
                gdk::Key::Down | gdk::Key::KP_Down if n > 0 => {
                    let next = if cur == gtk::INVALID_LIST_POSITION {
                        0
                    } else {
                        (cur + 1).min(n - 1)
                    };
                    ui.sel.set_selected(next);
                    ui.list.scroll_to(next, gtk::ListScrollFlags::NONE, None);
                    glib::Propagation::Stop
                }
                gdk::Key::Up | gdk::Key::KP_Up if n > 0 => {
                    let next = if cur == gtk::INVALID_LIST_POSITION || cur == 0 {
                        0
                    } else {
                        cur - 1
                    };
                    ui.sel.set_selected(next);
                    ui.list.scroll_to(next, gtk::ListScrollFlags::NONE, None);
                    glib::Propagation::Stop
                }
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    let pos = if cur == gtk::INVALID_LIST_POSITION {
                        0
                    } else {
                        cur
                    };
                    if let Some(item) = ui.model.get(pos) {
                        if mods.contains(gdk::ModifierType::CONTROL_MASK) {
                            b.open_result(&item);
                        } else if mods.contains(gdk::ModifierType::ALT_MASK) {
                            b.terminal_at_result(&item);
                        } else {
                            b.reveal_result(&item);
                        }
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::Escape => {
                    b.close_search();
                    glib::Propagation::Stop
                }
                // Backspace in an empty entry leaves the scope.
                gdk::Key::BackSpace
                    if ui.entry.text().is_empty() && ui.scope.borrow().is_some() =>
                {
                    b.set_search_scope(None);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        ui.entry.add_controller(keys);
        let w = Rc::downgrade(self);
        ui.list.connect_activate(move |_, pos| {
            if let Some(b) = w.upgrade()
                && let Some(item) = b.search.model.get(pos)
            {
                b.reveal_result(&item);
            }
        });
    }

    /// Send the entry's query (with the scope) to the search thread.
    pub(crate) fn run_search(self: &Rc<Self>) {
        let ui = &self.search;
        let text = ui.entry.text().trim().to_string();
        let id = ui.serial.get() + 1;
        ui.serial.set(id);
        // Problems with filters show at once, before results arrive.
        let hints: Vec<String> = tudi_index::query::parse_local(&text)
            .hints
            .iter()
            .map(hint_text)
            .collect();
        ui.hint.set_text(&hints.join("\n"));
        ui.hint.set_visible(!hints.is_empty());
        if text.is_empty() {
            ui.model.set(Vec::new());
            ui.status.set_text("");
            return;
        }
        let scope = ui.scope.borrow().clone();
        self.search_client
            .query(id, text, self.show_hidden.get(), scope);
    }

    /// A reply from the search thread.
    pub(crate) fn search_results(
        &self,
        id: u64,
        items: Vec<ResultItem>,
        partial: bool,
        error: Option<String>,
    ) {
        if id != self.search.serial.get() {
            return; // an older query
        }
        let n = items.len();
        self.search.model.set(items);
        if n > 0 {
            self.search.sel.set_selected(0);
            self.search
                .list
                .scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
        let mut status = match error {
            Some(e) => e,
            None if n == 0 => tr("No matches"),
            None => crate::i18n::fill(
                &crate::i18n::ntr("{} result", "{} results", n as u64),
                &[&n],
            ),
        };
        if partial {
            status.push_str(" · ");
            status.push_str(&tr("still indexing, results may be incomplete"));
        }
        status.push_str("   ↵ ");
        status.push_str(&tr("reveal"));
        status.push_str("   Ctrl+↵ ");
        status.push_str(&tr("open"));
        status.push_str("   Alt+↵ ");
        status.push_str(&tr("terminal"));
        self.search.status.set_text(&status);
    }

    /// Show the result's folder and select it in the list.
    fn reveal_result(self: &Rc<Self>, item: &ResultItem) {
        let path = item.path();
        self.close_search();
        let Some(dir) = path.parent().map(PathBuf::from) else {
            return;
        };
        if let Some(name) = path.file_name() {
            use std::os::unix::ffi::OsStrExt;
            *self.tab().select_after_load.borrow_mut() = Some(name.as_bytes().to_vec());
        }
        self.navigate(&dir, true);
        self.record_visit(&dir);
    }

    fn open_result(self: &Rc<Self>, item: &ResultItem) {
        let path = item.path();
        self.close_search();
        if item.is_dir() {
            self.navigate(&path, true);
            self.record_visit(&path);
        } else {
            crate::browser::open_default(&path, &self.window);
        }
    }

    fn terminal_at_result(self: &Rc<Self>, item: &ResultItem) {
        let path = item.path();
        let dir = if item.is_dir() {
            path
        } else {
            path.parent().map(PathBuf::from).unwrap_or(path)
        };
        self.close_search();
        let tmpl = self.shared.config.borrow().commands.terminal.clone();
        self.run_command_in(&tmpl, dir, &[]);
    }
}
