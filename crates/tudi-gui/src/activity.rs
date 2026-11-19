//! Background work shown in the status bar: a spinner and what is running
//! ("Reading folder…", "Pasting…"), so a busy window says why.
//!
//! A task appears only after it has run for [`SHOW_AFTER`], so quick
//! listings and reloads do not make the status bar flicker.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::browser::Browser;
use crate::i18n::tr;

/// Tasks shorter than this are not shown.
pub const SHOW_AFTER: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    /// Listing the shown folder.
    Listing,
    /// Reading sizes, dates and types of the listed entries.
    Details,
    /// Adding up the size below the shown folder.
    FolderSize,
    Copying,
    Moving,
    /// Dropped files: moved or copied depending on the filesystem.
    Transferring,
    Trashing,
}

impl Task {
    fn label(self) -> String {
        match self {
            Task::Listing => tr("Reading folder…"),
            Task::Details => tr("Reading file details…"),
            Task::FolderSize => tr("Measuring folder size…"),
            Task::Copying => tr("Copying…"),
            Task::Moving => tr("Moving…"),
            Task::Transferring => tr("Copying and moving…"),
            Task::Trashing => tr("Moving to the trash…"),
        }
    }
}

/// Running tasks (one entry per running instance) and their start times.
#[derive(Default)]
pub struct Activity {
    running: RefCell<Vec<(Task, Instant)>>,
}

impl Activity {
    /// Labels of the tasks running for at least [`SHOW_AFTER`], each once,
    /// in start order.
    pub fn visible_labels(&self, now: Instant) -> Vec<String> {
        let mut shown: Vec<Task> = Vec::new();
        for &(t, since) in self.running.borrow().iter() {
            if now.duration_since(since) >= SHOW_AFTER && !shown.contains(&t) {
                shown.push(t);
            }
        }
        shown.into_iter().map(Task::label).collect()
    }

    fn begin(&self, task: Task, now: Instant) {
        self.running.borrow_mut().push((task, now));
    }

    /// Returns whether anything was running.
    fn end(&self, task: Task) -> bool {
        let mut r = self.running.borrow_mut();
        match r.iter().position(|&(t, _)| t == task) {
            Some(i) => {
                r.remove(i);
                true
            }
            None => false,
        }
    }

    fn end_all(&self, task: Task) -> bool {
        let mut r = self.running.borrow_mut();
        let n = r.len();
        r.retain(|&(t, _)| t != task);
        r.len() != n
    }
}

impl Browser {
    /// A task started; the status bar mentions it if it is still running
    /// after [`SHOW_AFTER`].
    pub(crate) fn begin_task(self: &Rc<Self>, task: Task) {
        self.activity.begin(task, Instant::now());
        let w = Rc::downgrade(self);
        glib::timeout_add_local_once(SHOW_AFTER, move || {
            if let Some(b) = w.upgrade() {
                b.update_status();
            }
        });
    }

    /// One instance of `task` finished.
    pub(crate) fn end_task(&self, task: Task) {
        if self.activity.end(task) {
            self.update_status();
        }
    }

    /// Every instance of `task` finished or was superseded.
    pub(crate) fn end_tasks(&self, task: Task) {
        if self.activity.end_all(task) {
            self.update_status();
        }
    }

    /// Prefix for the status text ("Reading folder… · "), and show the
    /// spinner while anything visible runs.
    pub(crate) fn activity_text(&self) -> String {
        let labels = self.activity.visible_labels(Instant::now());
        let busy = !labels.is_empty();
        self.status_spinner.set_visible(busy);
        self.status_spinner.set_spinning(busy);
        if busy {
            format!("{} · ", labels.join(" · "))
        } else {
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_tasks_stay_hidden_and_labels_are_deduplicated() {
        let a = Activity::default();
        let t0 = Instant::now();
        a.begin(Task::Copying, t0);
        a.begin(Task::Copying, t0);
        assert!(a.visible_labels(t0).is_empty());
        let later = t0 + SHOW_AFTER;
        assert_eq!(a.visible_labels(later).len(), 1);
        assert!(a.end(Task::Copying));
        assert_eq!(a.visible_labels(later).len(), 1);
        assert!(a.end(Task::Copying));
        assert!(!a.end(Task::Copying));
        assert!(a.visible_labels(later).is_empty());
    }

    #[test]
    fn end_all_removes_every_instance() {
        let a = Activity::default();
        let t0 = Instant::now();
        a.begin(Task::Listing, t0);
        a.begin(Task::Listing, t0);
        a.begin(Task::Details, t0);
        assert!(a.end_all(Task::Listing));
        assert!(!a.end_all(Task::Listing));
        assert_eq!(a.visible_labels(t0 + SHOW_AFTER).len(), 1);
    }
}
