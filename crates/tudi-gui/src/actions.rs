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
        self.scan_typeahead(prefix, serial, start % n, 0);
        true
    }

    fn focused_row(&self) -> Option<usize> {
        let bits = self.pane_sel.selection();
        (!bits.is_empty()).then(|| bits.minimum() as usize)
    }

    /// Scan up to `SCAN_CHUNK` rows from `pos` for a name starting with
    /// `prefix` (case-insensitive), continuing in an idle callback.
    fn scan_typeahead(self: &Rc<Self>, prefix: String, serial: u64, pos: usize, scanned: usize) {
        if self.typeahead.borrow().serial != serial {
            return;
        }
        let n = self.pane.n_items() as usize;
        let found = {
            let d = self.pane.data();
            (0..SCAN_CHUNK.min(n - scanned))
                .map(|k| (pos + k) % n)
                .find(|&row| {
                    String::from_utf8_lossy(d.name(d.order[row]))
                        .to_lowercase()
                        .starts_with(&prefix)
                })
        };
        if let Some(row) = found {
            self.pane_sel.select_item(row as u32, true);
            self.pane_view
                .scroll_to(row as u32, gtk::ListScrollFlags::FOCUS, None);
            return;
        }
        let scanned = scanned + SCAN_CHUNK.min(n - scanned);
        if scanned < n {
            let w = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(b) = w.upgrade() {
                    b.scan_typeahead(prefix, serial, (pos + SCAN_CHUNK) % n, scanned);
                }
            });
        }
    }

    // ----------------------------------------------------------- clipboard

    /// Ctrl+C: the selected files, as a file list, GNOME's copied-files
    /// format, and plain text paths.
    pub(crate) fn copy_files(&self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let provider = files_provider(&paths);
        self.window.clipboard().set_content(Some(&provider)).ok();
        self.toast(&format!("Copied {} item(s)", paths.len()));
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
        self.toast(&format!("Copied {} path(s)", paths.len()));
    }

    // ------------------------------------------------------------ commands

    pub(crate) fn open_terminal(self: &Rc<Self>) {
        let t = self.config.commands.terminal.clone();
        self.run_command(&t, &[]);
    }

    pub(crate) fn open_editor(self: &Rc<Self>) {
        let t = self.config.commands.editor.clone();
        let paths = self.selected_paths();
        self.run_command(&t, &paths);
    }

    fn run_command(self: &Rc<Self>, template: &[String], paths: &[PathBuf]) {
        let dir = self.current_dir();
        let Some((program, args)) = expand_command(template, &dir, paths) else {
            self.toast("No command configured");
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
                b.toast(&format!("Cannot run {name}: {e}"));
            }
        });
    }

    // --------------------------------------------------- file operations

    /// Delete: move the selection to the trash.
    pub(crate) fn trash_selected(self: &Rc<Self>) {
        let paths = self.selected_paths();
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
                b.toast(&format!("Moved {ok} item(s) to the trash"));
            } else {
                b.toast(&format!("Could not trash {}", failed.join("; ")));
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
        self.ask_name("Rename", "Rename", &old, move |new| {
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
                Err(e) => b.toast(&format!("Cannot rename: {e}")),
            }
        });
    }

    /// Ctrl+Shift+N: create a folder in the current folder.
    pub(crate) fn new_folder(self: &Rc<Self>) {
        let w = Rc::downgrade(self);
        self.ask_name("New Folder", "Create", "New Folder", move |name| {
            if let Some(b) = w.upgrade() {
                b.make_folder(name);
            }
        });
    }

    pub(crate) fn make_folder(self: &Rc<Self>, name: String) {
        let f = gio::File::for_path(self.current_dir().join(&name));
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = f.make_directory_future(glib::Priority::DEFAULT).await;
            let Some(b) = w.upgrade() else { return };
            match r {
                Ok(()) => b.reload(Some(name.into_bytes())),
                Err(e) => b.toast(&format!("Cannot create folder: {e}")),
            }
        });
    }

    /// A dialog asking for a file name; `done` gets a non-empty name without
    /// `/`. The stem (before the last `.`) is preselected.
    fn ask_name(&self, heading: &str, verb: &str, initial: &str, done: impl Fn(String) + 'static) {
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
        dialog.add_responses(&[("cancel", "Cancel"), ("ok", verb)]);
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let stem = match initial.rfind('.') {
            Some(p) if p > 0 => initial[..p].chars().count() as i32,
            _ => -1,
        };
        let e = entry.clone();
        dialog.connect_response(None, move |_, resp| {
            let name = e.text().to_string();
            if resp == "ok"
                && !name.is_empty()
                && !name.contains('/')
                && name != "."
                && name != ".."
            {
                done(name);
            }
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
        entry.select_region(0, stem);
    }

    // ------------------------------------------------------------ drag out

    pub(crate) fn install_drag_source(self: &Rc<Self>) {
        let src = gtk::DragSource::new();
        src.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK);
        let w = Rc::downgrade(self);
        src.connect_prepare(move |_, _, _| {
            let b = w.upgrade()?;
            let paths = b.selected_paths();
            (!paths.is_empty()).then(|| files_provider(&paths))
        });
        self.pane_view.add_controller(src);
    }
}

/// Content for copying or dragging files.
fn files_provider(paths: &[PathBuf]) -> gdk::ContentProvider {
    let files: Vec<gio::File> = paths.iter().map(gio::File::for_path).collect();
    let list = gdk::FileList::from_array(&files);
    let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
    let gnome = format!("copy\n{}", uris.join("\n"));
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
