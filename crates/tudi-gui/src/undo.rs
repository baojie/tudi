//! Undo (`Ctrl+Z`) of the window's file operations (docs/ideas.md #1).
//!
//! Each finished operation pushes an [`Op`]; `Ctrl+Z` (or the background
//! menu) pops the last one and reverses it on a helper thread: moved items
//! go back, copies and new items go to the trash, trashed items are
//! restored from it (found in `trash:///` by their original path), renames
//! are reverted. Undo never overwrites: an item whose old place is taken
//! again stays where it is and the toast says so.

use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use tudi_core::fileops::{self, Mode};

use crate::browser::Browser;
use crate::i18n::{fill, ntr, tr};

/// Most operations remembered per window.
const DEPTH: usize = 50;

/// An operation that can be undone.
#[derive(Clone, Debug)]
pub enum Op {
    /// Copied or moved: (source, destination, how), and the items a
    /// "Replace" sent to the trash.
    Transfer {
        items: Vec<(PathBuf, PathBuf, Mode)>,
        replaced: Vec<PathBuf>,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    /// Sent to the trash (original paths).
    Trash(Vec<PathBuf>),
    /// A new folder or file.
    Create(PathBuf),
}

impl Op {
    /// "move of 3 items", for the toast.
    fn describe(&self) -> String {
        match self {
            Op::Transfer { items, .. } => {
                let n = items.len() as u64;
                if items.iter().all(|(_, _, m)| *m == Mode::Move) {
                    fill(&ntr("move of {} item", "move of {} items", n), &[&n])
                } else {
                    fill(&ntr("copy of {} item", "copy of {} items", n), &[&n])
                }
            }
            Op::Rename { to, .. } => fill(&tr("rename of {}"), &[&name(to)]),
            Op::Trash(p) => {
                let n = p.len() as u64;
                fill(
                    &ntr("trashing of {} item", "trashing of {} items", n),
                    &[&n],
                )
            }
            Op::Create(p) => fill(&tr("creation of {}"), &[&name(p)]),
        }
    }
}

fn name(p: &std::path::Path) -> String {
    p.file_name()
        .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned()
}

/// Send to the trash (blocking; helper thread).
fn trash(p: &std::path::Path) -> Result<(), String> {
    gio::File::for_path(p)
        .trash(gio::Cancellable::NONE)
        .map_err(|e| format!("{}: {e}", p.display()))
}

/// Restore items from the trash to their original paths (blocking): the
/// latest trashed before `before` (a `trash::deletion-date`, local time),
/// so items this undo itself just trashed (a copy over a replaced file)
/// are not taken for the original.
fn restore(paths: &[PathBuf], before: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if paths.is_empty() {
        return errors;
    }
    let trash_dir = gio::File::for_uri("trash:///");
    let list = match trash_dir.enumerate_children(
        "standard::name,trash::orig-path,trash::deletion-date",
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    ) {
        Ok(l) => l,
        Err(e) => return vec![e.to_string()],
    };
    // The latest trashed item for each wanted path.
    let mut best: std::collections::HashMap<PathBuf, (String, gio::File)> = Default::default();
    while let Ok(Some(info)) = list.next_file(gio::Cancellable::NONE) {
        let Some(orig) = info
            .attribute_byte_string("trash::orig-path")
            .map(|s| PathBuf::from(s.as_str()))
        else {
            continue;
        };
        if !paths.contains(&orig) {
            continue;
        }
        let date = info
            .attribute_string("trash::deletion-date")
            .map(|s| s.to_string())
            .unwrap_or_default();
        if date.as_str() >= before {
            continue;
        }
        let child = trash_dir.child(info.name());
        let newer = best.get(&orig).is_none_or(|(d, _)| date > *d);
        if newer {
            best.insert(orig, (date, child));
        }
    }
    for p in paths {
        match best.get(p) {
            Some((_, f)) => {
                if let Err(e) = f.move_(
                    &gio::File::for_path(p),
                    gio::FileCopyFlags::NOFOLLOW_SYMLINKS,
                    gio::Cancellable::NONE,
                    None,
                ) {
                    errors.push(format!("{}: {e}", p.display()));
                }
            }
            None => errors.push(format!("{}: {}", p.display(), tr("not in the trash"))),
        }
    }
    errors
}

/// Reverse `op` (blocking; helper thread). Returns the errors.
fn reverse(op: &Op) -> Vec<String> {
    let mut errors = Vec::new();
    // Trash dates have one-second resolution, in local time.
    let start = glib::DateTime::now_local()
        .and_then(|d| d.format("%Y-%m-%dT%H:%M:%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "9999".into());
    match op {
        Op::Transfer { items, replaced } => {
            for (src, dest, mode) in items.iter().rev() {
                let r = match mode {
                    Mode::Move => fileops::move_back(dest, src).map_err(|e| format!("{e}")),
                    Mode::Copy => trash(dest),
                };
                if let Err(e) = r {
                    errors.push(e);
                }
            }
            errors.extend(restore(replaced, &start));
        }
        Op::Rename { from, to } => {
            if let Err(e) = fileops::move_back(to, from) {
                errors.push(e.to_string());
            }
        }
        Op::Trash(paths) => errors.extend(restore(paths, &start)),
        Op::Create(p) => {
            if let Err(e) = trash(p) {
                errors.push(e);
            }
        }
    }
    errors
}

impl Browser {
    /// Remember a finished operation for `Ctrl+Z`.
    pub(crate) fn push_undo(&self, op: Op) {
        let mut stack = self.undo.borrow_mut();
        stack.push(op);
        if stack.len() > DEPTH {
            stack.remove(0);
        }
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.borrow().is_empty()
    }

    /// `Ctrl+Z`: undo the last file operation.
    pub(crate) fn undo_last(self: &Rc<Self>) {
        let Some(op) = self.undo.borrow_mut().pop() else {
            self.toast(&tr("Nothing to undo"));
            return;
        };
        let what = op.describe();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(reverse(&op));
        });
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(errors) = rx.recv().await else { return };
            let Some(b) = w.upgrade() else { return };
            if errors.is_empty() {
                b.toast(&fill(&tr("Undid the {}"), &[&what]));
            } else {
                b.toast(&fill(
                    &tr("Could not undo all of the {}: {}"),
                    &[&what, &errors.join("; ")],
                ));
            }
            b.reload(None);
        });
    }
}
