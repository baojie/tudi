//! Dropping files on the list or the folder tree, and the transfer behind
//! both drop and paste.
//!
//! A drop on a folder (a list row or a tree row) puts the files into that
//! folder; anywhere else in the list, into the shown folder. Without
//! modifiers, files move within a filesystem and are copied to another one
//! (decided on the transfer thread, not with a `stat` on the main thread);
//! `Ctrl` forces a copy, `Shift` a move.

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tudi_core::fileops::{Choice, Report};

use crate::activity::Task;
use crate::browser::Browser;
use crate::i18n::{fill, ntr, tr};

/// A drop handler: the dropped paths and the modifier keys held.
pub type DropFn = Rc<dyn Fn(Vec<PathBuf>, gdk::ModifierType) -> bool>;

/// A drop target for files (from tudi or other apps).
pub fn file_drop_target(on_drop: DropFn) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(
        gdk::FileList::static_type(),
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
    target.connect_drop(move |t, v, _, _| {
        let Ok(list) = v.get::<gdk::FileList>() else {
            return false;
        };
        let files: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        !files.is_empty() && on_drop(files, t.current_event_state())
    });
    target
}

/// `Ctrl` copies, `Shift` moves, otherwise depending on the filesystem.
pub fn choice_for(mods: gdk::ModifierType) -> Choice {
    if mods.contains(gdk::ModifierType::CONTROL_MASK) {
        Choice::Copy
    } else if mods.contains(gdk::ModifierType::SHIFT_MASK) {
        Choice::Move
    } else {
        Choice::Auto
    }
}

impl Browser {
    /// Files dropped on list row `pos` (a folder takes them) or on empty
    /// space (`None`: the shown folder).
    pub(crate) fn drop_on_pane(
        self: &Rc<Self>,
        pos: Option<u32>,
        files: Vec<PathBuf>,
        mods: gdk::ModifierType,
    ) -> bool {
        let dir = self.current_dir();
        let dest = pos
            .and_then(|pos| {
                let tab = self.tab();
                let d = tab.pane.data();
                let &e = d.order.get(pos as usize)?;
                d.is_dir(e).then(|| d.path(e))
            })
            .unwrap_or(dir);
        self.drop_into(files, dest, mods)
    }

    /// Files dropped on tree row `pos`.
    pub(crate) fn drop_on_tree(
        self: &Rc<Self>,
        pos: u32,
        files: Vec<PathBuf>,
        mods: gdk::ModifierType,
    ) -> bool {
        let dest = {
            let t = self.tree.tree();
            match t.rows().get(pos as usize) {
                Some(r) if r.kind == tudi_core::tree::RowKind::Node => t.path(r.node),
                _ => return false,
            }
        };
        self.drop_into(files, dest, mods)
    }

    pub(crate) fn drop_into(
        self: &Rc<Self>,
        files: Vec<PathBuf>,
        dest: PathBuf,
        mods: gdk::ModifierType,
    ) -> bool {
        // A folder dropped on itself (a short drag that missed): ignore.
        if files.contains(&dest) {
            return false;
        }
        self.transfer_into(files, dest, choice_for(mods));
        true
    }

    /// Copy or move `paths` into `dest`: first look for name clashes (on a
    /// helper thread) and ask what to do about them, then transfer.
    pub(crate) fn transfer_into(
        self: &Rc<Self>,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        choice: Choice,
    ) {
        let (tx, rx) = async_channel::bounded(1);
        let (p, d) = (paths.clone(), dest.clone());
        std::thread::spawn(move || {
            let _ = tx.send_blocking(tudi_core::fileops::conflicts(&p, &d));
        });
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(clash) = rx.recv().await else { return };
            let Some(b) = w.upgrade() else { return };
            if clash.is_empty() {
                b.run_transfer(paths, dest, choice, Vec::new());
            } else {
                b.ask_conflicts(paths, dest, choice, clash);
            }
        });
    }

    /// Some names exist in `dest`: replace them (the old ones go to the
    /// trash, so undo can bring them back), skip those items, or keep both
    /// (the new ones get "name (2)").
    fn ask_conflicts(
        self: &Rc<Self>,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        choice: Choice,
        clash: Vec<PathBuf>,
    ) {
        let n = clash.len() as u64;
        let folder = crate::tabs::title_of(&dest);
        let heading = fill(
            &ntr(
                "{} item already exists in “{}”",
                "{} items already exist in “{}”",
                n,
            ),
            &[&n, &folder],
        );
        let mut names: Vec<String> = clash
            .iter()
            .take(6)
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        if clash.len() > 6 {
            names.push("…".into());
        }
        let dialog = adw::AlertDialog::new(Some(&heading), Some(&names.join("\n")));
        dialog.add_responses(&[
            ("cancel", &tr("Cancel")),
            ("skip", &tr("Skip")),
            ("keep", &tr("Keep Both")),
            ("replace", &tr("Replace")),
        ]);
        dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("keep", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("cancel");
        let w = Rc::downgrade(self);
        dialog.connect_response(None, move |_, r| {
            let Some(b) = w.upgrade() else { return };
            let names: Vec<_> = clash.iter().filter_map(|p| p.file_name()).collect();
            match r {
                "skip" => {
                    let rest: Vec<PathBuf> = paths
                        .iter()
                        .filter(|p| p.file_name().is_none_or(|n| !names.contains(&n)))
                        .cloned()
                        .collect();
                    b.run_transfer(rest, dest.clone(), choice, Vec::new());
                }
                "keep" => b.run_transfer(paths.clone(), dest.clone(), choice, Vec::new()),
                "replace" => b.run_transfer(paths.clone(), dest.clone(), choice, clash.clone()),
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    /// Transfer on a helper thread (after sending `replace` to the trash),
    /// then report, remember it for undo and show the result.
    fn run_transfer(
        self: &Rc<Self>,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        choice: Choice,
        replace: Vec<PathBuf>,
    ) {
        if paths.is_empty() {
            return;
        }
        let task = match choice {
            Choice::Copy => Task::Copying,
            Choice::Move => Task::Moving,
            Choice::Auto => Task::Transferring,
        };
        self.begin_task(task);
        let n_replace = replace.len();
        let (tx, rx) = async_channel::bounded(1);
        let dir = dest.clone();
        // Copying can take long: off the main thread.
        std::thread::spawn(move || {
            let replaced: Vec<PathBuf> = replace
                .into_iter()
                .filter(|p| gio::File::for_path(p).trash(gio::Cancellable::NONE).is_ok())
                .collect();
            let report = tudi_core::fileops::transfer_choice(&paths, &dir, choice);
            let _ = tx.send_blocking((report, replaced));
        });
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = rx.recv().await;
            let Some(b) = w.upgrade() else { return };
            b.end_task(task);
            let Ok((report, replaced)) = result else {
                return;
            };
            if replaced.len() < n_replace {
                // Not trashable (e.g. a system mount): both were kept.
                b.toast(&tr(
                    "Some existing items could not be moved to the trash; both were kept",
                ));
            }
            if !report.done.is_empty() || !replaced.is_empty() {
                let items = report
                    .sources
                    .iter()
                    .zip(&report.done)
                    .map(|((src, mode), dest)| (src.clone(), dest.clone(), *mode))
                    .collect();
                b.push_undo(crate::undo::Op::Transfer { items, replaced });
            }
            b.report_transfer(&report);
            if *b.tab().current.borrow() == dest && !report.done.is_empty() {
                let first = report
                    .done
                    .first()
                    .and_then(|p| p.file_name())
                    .map(|n| n.as_bytes().to_vec());
                b.reload(first);
            }
        });
    }

    fn report_transfer(&self, report: &Report) {
        if let Some((p, e)) = report.failed.first() {
            self.toast(&fill(&tr("Could not paste {}: {}"), &[&p.display(), &e]));
            return;
        }
        let moved = report.moved as u64;
        let copied = (report.done.len() - report.moved) as u64;
        let msg = match (moved, copied) {
            (0, 0) => return,
            (m, 0) => fill(&ntr("Moved {} item", "Moved {} items", m), &[&m]),
            (0, c) => fill(&ntr("Pasted {} item", "Pasted {} items", c), &[&c]),
            (m, c) => fill(&tr("Moved {}, copied {}"), &[&m, &c]),
        };
        self.toast(&msg);
    }

    /// Accept files dropped on the list's empty space (rows have their own
    /// targets, see `pane::build_view`).
    pub(crate) fn install_drop_targets(self: &Rc<Self>, view: &gtk::Widget) {
        let tab = self.tab();
        let (w, t) = (Rc::downgrade(self), Rc::downgrade(&tab));
        let target = file_drop_target(Rc::new(move |files, mods| {
            let (Some(b), Some(t)) = (w.upgrade(), t.upgrade()) else {
                return false;
            };
            // A drop on a tile that is not the active tab goes to its folder.
            b.activate_tab(&t);
            b.drop_on_pane(None, files, mods)
        }));
        view.add_controller(target);
    }
}
