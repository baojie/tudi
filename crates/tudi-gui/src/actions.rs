//! User actions on the details list: type-ahead, clipboard, external
//! commands, rename, trash, new folder, drag out.
//!
//! Filesystem changes go through GIO's async API (worker threads inside
//! GIO); external commands are spawned on a helper thread. The main thread
//! never blocks on the filesystem.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tudi_core::config::expand_command;

use crate::browser::Browser;
use crate::i18n::{fill, ntr, tr};

/// Type-ahead buffer resets after this pause.
const TYPEAHEAD_RESET: Duration = Duration::from_millis(1000);
/// Rows scanned per main-loop turn while searching (AGENTS.md rule 2).
const SCAN_CHUNK: usize = 4096;

#[derive(Default)]
pub struct TypeAhead {
    text: String,
    last: Option<Instant>,
    /// Bumped on every keystroke; a running chunked search stops when stale.
    serial: u64,
}

pub type TypeAheadState = Rc<RefCell<TypeAhead>>;

/// What a row scan looks for (names compared case-insensitively).
pub(crate) enum Match {
    /// Type-ahead: the name starts with this (lowercase) text.
    Prefix(String),
    /// Find bar: the name contains this (lowercase) text.
    Contains(String),
}

impl Match {
    fn matches(&self, name: &[u8]) -> bool {
        match self {
            Match::Prefix(p) => String::from_utf8_lossy(name).to_lowercase().starts_with(p),
            Match::Contains(n) => crate::worker::name_contains(name, n),
        }
    }

    fn focus_list(&self) -> bool {
        matches!(self, Match::Prefix(_))
    }

    fn done(&self, b: &Browser, found: bool) {
        if let Match::Contains(_) = self {
            b.find_result(found);
        }
    }
}

impl Browser {
    pub(crate) fn current_dir(&self) -> PathBuf {
        self.current.borrow().clone()
    }

    /// Absolute paths of the selected entries.
    pub(crate) fn selected_paths(&self) -> Vec<PathBuf> {
        let dir = self.current_dir();
        let entries = self.selected_entries();
        let d = self.pane.data();
        entries
            .into_iter()
            .map(|e| dir.join(OsStr::from_bytes(d.name(e))))
            .collect()
    }

    pub(crate) fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    /// Re-list the current folder (after a change made by tudi itself),
    /// then select `select` if given.
    pub(crate) fn reload(self: &Rc<Self>, select: Option<Vec<u8>>) {
        *self.select_after_load.borrow_mut() = select;
        let cur = self.current_dir();
        self.navigate(&cur, false);
    }

    // ---------------------------------------------------------- type-ahead

    /// Handle a printable key in the details list. Returns true if consumed.
    pub(crate) fn type_ahead(self: &Rc<Self>, c: char) -> bool {
        if c.is_control() {
            return false;
        }
        let (prefix, serial) = {
            let mut t = self.typeahead.borrow_mut();
            let now = Instant::now();
            if t.last.is_none_or(|l| now - l > TYPEAHEAD_RESET) {
                t.text.clear();
            }
            if t.text.is_empty() && c == ' ' {
                return false;
            }
            t.text.extend(c.to_lowercase());
            t.last = Some(now);
            t.serial += 1;
            (t.text.clone(), t.serial)
        };
        // Start at the current row when extending a match, else after it.
        let n = self.pane.n_items() as usize;
        if n == 0 {
            return true;
        }
        let cur = self.focused_row().unwrap_or(0);
        let start = if prefix.chars().count() > 1 {
            cur
        } else {
            cur + 1
        };
        self.scan_rows(Match::Prefix(prefix), serial, start % n, true);
        true
    }

    pub(crate) fn focused_row(&self) -> Option<usize> {
        let bits = self.pane_sel.selection();
        (!bits.is_empty()).then(|| bits.minimum() as usize)
    }

    /// Start a new row scan, cancelling any running one.
    pub(crate) fn next_scan_serial(&self) -> u64 {
        let mut t = self.typeahead.borrow_mut();
        t.serial += 1;
        t.serial
    }

    /// Find the first row from `start` (wrapping, forwards or backwards)
    /// whose name matches, `SCAN_CHUNK` rows per main-loop turn, and select
    /// it. Calls `found` with the outcome when the scan ends (unless a newer
    /// scan replaced it).
    pub(crate) fn scan_rows(self: &Rc<Self>, m: Match, serial: u64, start: usize, forward: bool) {
        self.scan_chunk(Rc::new(m), serial, start, 0, forward);
    }

    fn scan_chunk(
        self: &Rc<Self>,
        m: Rc<Match>,
        serial: u64,
        start: usize,
        scanned: usize,
        forward: bool,
    ) {
        if self.typeahead.borrow().serial != serial {
            return;
        }
        let n = self.pane.n_items() as usize;
        if n == 0 {
            m.done(self, false);
            return;
        }
        let row_at = |k: usize| {
            if forward {
                (start + k) % n
            } else {
                (start + n - k % n) % n
            }
        };
        let end = (scanned + SCAN_CHUNK).min(n);
        let found = {
            let d = self.pane.data();
            (scanned..end)
                .map(row_at)
                .find(|&row| m.matches(d.name(d.order[row])))
        };
        if let Some(row) = found {
            self.pane_sel.select_item(row as u32, true);
            // From the find bar, keep the keyboard focus in its entry.
            let flags = if m.focus_list() {
                gtk::ListScrollFlags::FOCUS
            } else {
                gtk::ListScrollFlags::NONE
            };
            self.pane_view.scroll_to(row as u32, flags, None);
            m.done(self, true);
            return;
        }
        if end < n {
            let w = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(b) = w.upgrade() {
                    b.scan_chunk(m, serial, start, end, forward);
                }
            });
        } else {
            m.done(self, false);
        }
    }

    // ----------------------------------------------------------- clipboard

    /// Ctrl+C: the selected files, as a file list, GNOME's copied-files
    /// format, and plain text paths.
    pub(crate) fn copy_files(&self) {
        self.put_on_clipboard(self.selected_paths(), false);
    }

    /// Put files on the clipboard for a later paste (copy, or move if
    /// `cut`).
    pub(crate) fn put_on_clipboard(&self, paths: Vec<PathBuf>, cut: bool) {
        if paths.is_empty() {
            return;
        }
        let provider = files_provider(&paths, cut);
        self.window.clipboard().set_content(Some(&provider)).ok();
        let n = paths.len() as u64;
        let msg = if cut {
            ntr("Cut {} item", "Cut {} items", n)
        } else {
            ntr("Copied {} item", "Copied {} items", n)
        };
        self.toast(&fill(&msg, &[&n]));
    }

    /// Ctrl+X: like copy, but pasting moves the files.
    pub(crate) fn cut_files(&self) {
        self.put_on_clipboard(self.selected_paths(), true);
    }

    /// Ctrl+A.
    pub(crate) fn select_all(&self) {
        self.pane_sel.select_all();
    }

    /// Ctrl+V: copy (or move, after a cut) the files on the clipboard into
    /// the current folder. Works with files from other file managers too.
    pub(crate) fn paste(self: &Rc<Self>) {
        self.paste_into(self.current_dir());
    }

    /// Paste the clipboard's files into `dest`.
    pub(crate) fn paste_into(self: &Rc<Self>, dest: PathBuf) {
        let clipboard = self.window.clipboard();
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some((paths, mode)) = read_clipboard_files(&clipboard).await else {
                if let Some(b) = w.upgrade() {
                    b.toast(&tr("Nothing to paste"));
                }
                return;
            };
            let (tx, rx) = async_channel::bounded(1);
            let (srcs, dir) = (paths.clone(), dest.clone());
            // Copying can take long: off the main thread.
            std::thread::spawn(move || {
                let _ = tx.send_blocking(tudi_core::fileops::transfer_all(&srcs, &dir, mode));
            });
            let Ok(report) = rx.recv().await else { return };
            let Some(b) = w.upgrade() else { return };
            if mode == tudi_core::fileops::Mode::Move && !report.done.is_empty() {
                // The cut files are gone from where the clipboard points.
                clipboard.set_content(None::<&gdk::ContentProvider>).ok();
            }
            let n = report.done.len() as u64;
            if report.failed.is_empty() {
                let msg = if mode == tudi_core::fileops::Mode::Move {
                    ntr("Moved {} item", "Moved {} items", n)
                } else {
                    ntr("Pasted {} item", "Pasted {} items", n)
                };
                b.toast(&fill(&msg, &[&n]));
            } else {
                let (p, e) = &report.failed[0];
                b.toast(&fill(&tr("Could not paste {}: {}"), &[&p.display(), &e]));
            }
            if *b.current.borrow() == dest {
                let first = report
                    .done
                    .first()
                    .and_then(|p| p.file_name())
                    .map(|n| n.as_bytes().to_vec());
                b.reload(first);
            }
        });
    }

    /// Ctrl+Shift+C: absolute paths as text (current folder if none).
    pub(crate) fn copy_paths(&self) {
        let mut paths = self.selected_paths();
        if paths.is_empty() {
            paths.push(self.current_dir());
        }
        let text = paths
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n");
        self.window.clipboard().set_text(&text);
        let n = paths.len() as u64;
        self.toast(&fill(&ntr("Copied {} path", "Copied {} paths", n), &[&n]));
    }

    // ------------------------------------------------------------ commands

    pub(crate) fn open_terminal(self: &Rc<Self>) {
        let t = self.shared.config.borrow().commands.terminal.clone();
        self.run_command(&t, &[]);
    }

    pub(crate) fn open_editor(self: &Rc<Self>) {
        let t = self.shared.config.borrow().commands.editor.clone();
        let paths = self.selected_paths();
        self.run_command(&t, &paths);
    }

    fn run_command(self: &Rc<Self>, template: &[String], paths: &[PathBuf]) {
        self.run_command_in(template, self.current_dir(), paths);
    }

    pub(crate) fn run_command_in(
        self: &Rc<Self>,
        template: &[String],
        dir: PathBuf,
        paths: &[PathBuf],
    ) {
        let Some((program, args)) = expand_command(template, &dir, paths) else {
            self.toast(&tr("No command configured"));
            return;
        };
        let (tx, rx) = async_channel::bounded::<Result<(), String>>(1);
        let name = program.to_string_lossy().into_owned();
        // PATH lookup and fork/exec happen off the main thread; the thread
        // then waits for the child so it does not linger as a zombie.
        std::thread::spawn(move || {
            let child = std::process::Command::new(&program)
                .args(&args)
                .current_dir(&dir)
                .spawn();
            match child {
                Ok(mut c) => {
                    let _ = tx.send_blocking(Ok(()));
                    let _ = c.wait();
                }
                Err(e) => {
                    let _ = tx.send_blocking(Err(e.to_string()));
                }
            }
        });
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if let Ok(Err(e)) = rx.recv().await
                && let Some(b) = w.upgrade()
            {
                b.toast(&fill(&tr("Cannot run {}: {}"), &[&name, &e]));
            }
        });
    }

    // --------------------------------------------------- file operations

    /// Delete: move the selection to the trash.
    pub(crate) fn trash_selected(self: &Rc<Self>) {
        self.trash_paths(self.selected_paths());
    }

    pub(crate) fn trash_paths(self: &Rc<Self>, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut failed = Vec::new();
            for p in &paths {
                let f = gio::File::for_path(p);
                if let Err(e) = f.trash_future(glib::Priority::DEFAULT).await {
                    failed.push(format!("{}: {e}", p.display()));
                }
            }
            let Some(b) = w.upgrade() else { return };
            let ok = paths.len() - failed.len();
            if failed.is_empty() {
                let ok = ok as u64;
                b.toast(&fill(
                    &ntr(
                        "Moved {} item to the trash",
                        "Moved {} items to the trash",
                        ok,
                    ),
                    &[&ok],
                ));
            } else {
                b.toast(&fill(&tr("Could not trash {}"), &[&failed.join("; ")]));
            }
            b.reload(None);
        });
    }

    /// F2: rename the focused item.
    pub(crate) fn rename_selected(self: &Rc<Self>) {
        let paths = self.selected_paths();
        let Some(path) = paths.first().cloned() else {
            return;
        };
        let old = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let w = Rc::downgrade(self);
        self.ask_name(&tr("Rename"), &tr("Rename"), &old, move |new| {
            if let Some(b) = w.upgrade() {
                b.rename(path.clone(), new);
            }
        });
    }

    pub(crate) fn rename(self: &Rc<Self>, path: PathBuf, new: String) {
        let f = gio::File::for_path(&path);
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = f
                .set_display_name_future(&new, glib::Priority::DEFAULT)
                .await;
            let Some(b) = w.upgrade() else { return };
            match r {
                Ok(_) => b.reload(Some(new.into_bytes())),
                Err(e) => b.toast(&fill(&tr("Cannot rename: {}"), &[&e])),
            }
        });
    }

    /// Ctrl+Shift+N: create a folder in the current folder.
    pub(crate) fn new_folder(self: &Rc<Self>) {
        self.new_folder_in(self.current_dir());
    }

    /// Ask for a name and create a folder in `dir`.
    pub(crate) fn new_folder_in(self: &Rc<Self>, dir: PathBuf) {
        let w = Rc::downgrade(self);
        self.ask_name(
            &tr("New Folder"),
            &tr("Create"),
            &tr("New Folder"),
            move |name| {
                if let Some(b) = w.upgrade() {
                    b.make_folder_in(&dir, name);
                }
            },
        );
    }

    pub(crate) fn make_folder(self: &Rc<Self>, name: String) {
        self.make_folder_in(&self.current_dir(), name);
    }

    pub(crate) fn make_folder_in(self: &Rc<Self>, dir: &std::path::Path, name: String) {
        let f = gio::File::for_path(dir.join(&name));
        let in_current = dir == self.current_dir();
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = f.make_directory_future(glib::Priority::DEFAULT).await;
            let Some(b) = w.upgrade() else { return };
            match r {
                Ok(()) if !in_current => {}
                Ok(()) => b.reload(Some(name.into_bytes())),
                Err(e) => b.toast(&fill(&tr("Cannot create folder: {}"), &[&e])),
            }
        });
    }

    /// A dialog asking for a file name; `done` gets a non-empty name without
    /// `/`. The stem (before the last `.`) is preselected.
    pub(crate) fn ask_name(
        &self,
        heading: &str,
        verb: &str,
        initial: &str,
        done: impl Fn(String) + 'static,
    ) {
        let stem = match initial.rfind('.') {
            Some(p) if p > 0 => initial[..p].chars().count() as i32,
            _ => -1,
        };
        self.ask(heading, verb, initial, stem, move |name| {
            if !name.is_empty() && !name.contains('/') && name != "." && name != ".." {
                done(name);
            }
        });
    }

    /// A dialog asking for any text (e.g. arguments); all is preselected.
    pub(crate) fn ask_text(
        &self,
        heading: &str,
        verb: &str,
        initial: &str,
        done: impl Fn(String) + 'static,
    ) {
        self.ask(heading, verb, initial, -1, done);
    }

    fn ask(
        &self,
        heading: &str,
        verb: &str,
        initial: &str,
        select_to: i32,
        done: impl Fn(String) + 'static,
    ) {
        let entry = gtk::Entry::builder()
            .text(initial)
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .extra_child(&entry)
            .default_response("ok")
            .close_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", &tr("Cancel")), ("ok", verb)]);
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let e = entry.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "ok" {
                done(e.text().to_string());
            }
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
        entry.select_region(0, select_to);
    }

    // --------------------------------------------------------- context menu

    /// Actions behind the context menu ("file.*").
    pub(crate) fn install_file_actions(self: &Rc<Self>) {
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, f: fn(&Rc<Browser>)| {
            let a = gio::SimpleAction::new(name, None);
            let w = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(b) = w.upgrade() {
                    f(&b);
                }
            });
            group.add_action(&a);
        };
        add("open", |b| b.open_selected());
        add("copy", |b| b.copy_files());
        add("cut", |b| b.cut_files());
        add("paste", |b| b.paste());
        add("copy-path", |b| b.copy_paths());
        add("rename", |b| b.rename_selected());
        add("trash", |b| b.trash_selected());
        add("new-folder", |b| b.new_folder());
        add("terminal", |b| b.open_terminal());
        add("editor", |b| b.open_editor());
        add("select-all", |b| b.select_all());
        add("hidden", |b| b.toggle_hidden());
        add("properties", |b| b.show_properties());
        add("folder-properties", |b| {
            crate::properties::show(b, vec![b.current_dir()]);
        });
        add("bookmark", |b| b.bookmark_selected_folder());
        add("launcher", |b| {
            if let Some(f) = b.selected_launchable() {
                b.add_launcher(&f);
            }
        });
        self.window.insert_action_group("file", Some(&group));
    }

    /// Open every selected item: the first folder is entered, files open
    /// in their default applications.
    fn open_selected(self: &Rc<Self>) {
        let entries = self.selected_entries();
        let mut files = Vec::new();
        let mut folder = None;
        {
            let d = self.pane.data();
            let dir = self.current_dir();
            for e in entries {
                let p = dir.join(OsStr::from_bytes(d.name(e)));
                if d.is_dir(e) {
                    folder.get_or_insert(p);
                } else {
                    files.push(p);
                }
            }
        }
        for f in &files {
            crate::browser::open_default(f, &self.window);
        }
        if let Some(p) = folder {
            self.navigate(&p, true);
            self.record_visit(&p);
        }
    }

    /// `Alt+Enter`: properties of the selection, or of the current folder.
    pub(crate) fn show_properties(self: &Rc<Self>) {
        let mut paths = self.selected_paths();
        if paths.is_empty() {
            paths.push(self.current_dir());
        }
        crate::properties::show(self, paths);
    }

    /// Bookmark the selected folder, or the current folder.
    pub(crate) fn bookmark_selected_folder(&self) {
        let target = {
            let d = self.pane.data();
            self.selected_entries()
                .into_iter()
                .find(|&e| d.is_dir(e))
                .map(|e| self.current_dir().join(OsStr::from_bytes(d.name(e))))
        };
        let Some(p) = target else {
            self.bookmark_current();
            return;
        };
        if self.shared.bookmarks.borrow_mut().add(&p) {
            self.toast(&fill(
                &tr("Bookmarked {}"),
                &[&p.file_name().unwrap_or_default().to_string_lossy()],
            ));
            self.shared.bookmarks_changed();
        } else {
            self.toast(&tr("Already bookmarked"));
        }
    }

    /// Right-click in the list: on a row (`Some(position)`) the menu acts
    /// on the selection, selecting the row first if it is not selected; on
    /// empty space it offers folder-level actions.
    pub(crate) fn file_menu(
        self: &Rc<Self>,
        pos: Option<u32>,
        widget: &gtk::Widget,
        x: f64,
        y: f64,
    ) {
        if let Some(p) = pos
            && !self.pane_sel.is_selected(p)
        {
            self.pane_sel.select_item(p, true);
        }
        let item = |label: String, action: &str, accel: &str| {
            let it = gio::MenuItem::new(Some(&label), Some(action));
            if !accel.is_empty() {
                it.set_attribute_value("accel", Some(&accel.to_variant()));
            }
            it
        };
        let menu = gio::Menu::new();
        if pos.is_some() {
            let one_folder = {
                let d = self.pane.data();
                let sel = self.selected_entries();
                sel.len() == 1 && d.is_dir(sel[0])
            };
            let open = gio::Menu::new();
            open.append_item(&item(tr("Open"), "file.open", "Return"));
            open.append_item(&item(tr("Open in Editor"), "file.editor", "<Control>e"));
            menu.append_section(None, &open);
            let edit = gio::Menu::new();
            edit.append_item(&item(tr("Cut"), "file.cut", "<Control>x"));
            edit.append_item(&item(tr("Copy"), "file.copy", "<Control>c"));
            edit.append_item(&item(tr("Paste"), "file.paste", "<Control>v"));
            edit.append_item(&item(
                tr("Copy Path"),
                "file.copy-path",
                "<Control><Shift>c",
            ));
            menu.append_section(None, &edit);
            let manage = gio::Menu::new();
            manage.append_item(&item(tr("Rename…"), "file.rename", "F2"));
            if one_folder {
                manage.append_item(&item(tr("Add to Bookmarks"), "file.bookmark", "<Control>d"));
            }
            if self.selected_launchable().is_some() {
                manage.append_item(&item(tr("Add to Launchers"), "file.launcher", "<Control>d"));
            }
            manage.append_item(&item(tr("Move to Trash"), "file.trash", "Delete"));
            menu.append_section(None, &manage);
            let info = gio::Menu::new();
            info.append_item(&item(tr("Properties"), "file.properties", "<Alt>Return"));
            menu.append_section(None, &info);
        } else {
            let edit = gio::Menu::new();
            edit.append_item(&item(tr("Paste"), "file.paste", "<Control>v"));
            edit.append_item(&item(tr("Select All"), "file.select-all", "<Control>a"));
            menu.append_section(None, &edit);
            let folder = gio::Menu::new();
            folder.append_item(&item(
                tr("New Folder…"),
                "file.new-folder",
                "<Control><Shift>n",
            ));
            folder.append_item(&item(
                tr("Open Terminal Here"),
                "file.terminal",
                "<Control><Alt>t",
            ));
            folder.append_item(&item(
                tr("Bookmark This Folder"),
                "file.bookmark",
                "<Control>d",
            ));
            folder.append_item(&item(tr("Show Hidden Files"), "file.hidden", "<Control>h"));
            menu.append_section(None, &folder);
            let info = gio::Menu::new();
            info.append_item(&item(tr("Folder Properties"), "file.folder-properties", ""));
            menu.append_section(None, &info);
        }
        crate::menu::popup_at(widget, menu.upcast_ref(), x, y);
    }

    // ------------------------------------------------------------ drag out

    pub(crate) fn install_drag_source(self: &Rc<Self>) {
        let src = gtk::DragSource::new();
        src.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK);
        let w = Rc::downgrade(self);
        src.connect_prepare(move |_, _, _| {
            let b = w.upgrade()?;
            let paths = b.selected_paths();
            (!paths.is_empty()).then(|| files_provider(&paths, false))
        });
        self.pane_view.add_controller(src);
    }
}

/// Content for copying or dragging files.
fn files_provider(paths: &[PathBuf], cut: bool) -> gdk::ContentProvider {
    let files: Vec<gio::File> = paths.iter().map(gio::File::for_path).collect();
    let list = gdk::FileList::from_array(&files);
    let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
    // GNOME's format tells file managers whether to copy or move on paste.
    let gnome = format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"));
    let text = paths
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_value(&list.to_value()),
        gdk::ContentProvider::for_bytes(
            "x-special/gnome-copied-files",
            &glib::Bytes::from_owned(gnome.into_bytes()),
        ),
        gdk::ContentProvider::for_bytes(
            "text/plain;charset=utf-8",
            &glib::Bytes::from_owned(text.into_bytes()),
        ),
    ])
}

/// Files on the clipboard, and whether pasting should move them (after a
/// cut in tudi or another GNOME-style file manager).
async fn read_clipboard_files(
    clipboard: &gdk::Clipboard,
) -> Option<(Vec<PathBuf>, tudi_core::fileops::Mode)> {
    use tudi_core::fileops::Mode;
    let formats = clipboard.formats();
    if formats.contain_mime_type("x-special/gnome-copied-files")
        && let Ok((stream, _)) = clipboard
            .read_future(&["x-special/gnome-copied-files"], glib::Priority::DEFAULT)
            .await
    {
        let mut data = Vec::new();
        loop {
            match stream
                .read_bytes_future(64 * 1024, glib::Priority::DEFAULT)
                .await
            {
                Ok(b) if !b.is_empty() => data.extend_from_slice(&b),
                _ => break,
            }
        }
        let text = String::from_utf8_lossy(&data);
        let mut lines = text.lines();
        let mode = match lines.next() {
            Some("cut") => Mode::Move,
            _ => Mode::Copy,
        };
        let paths: Vec<PathBuf> = lines
            .filter(|l| !l.is_empty())
            .filter_map(|uri| gio::File::for_uri(uri).path())
            .collect();
        if !paths.is_empty() {
            return Some((paths, mode));
        }
    }
    if formats.contains_type(gdk::FileList::static_type())
        && let Ok(v) = clipboard
            .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
            .await
        && let Ok(list) = v.get::<gdk::FileList>()
    {
        let paths: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        if !paths.is_empty() {
            return Some((paths, Mode::Copy));
        }
    }
    None
}
