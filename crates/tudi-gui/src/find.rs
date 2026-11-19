//! Find in the shown folder (`Ctrl+F`): a bar above the details list.
//!
//! Typing jumps to the first row whose name contains the text (ignoring
//! case); `Down`/`F3` and `Up`/`Shift+F3` go to the next or previous one.
//! `Enter` filters the list to the matching rows (an empty field clears the
//! filter) and moves the focus to the list. `Esc` closes the bar and clears
//! the filter. The jump scans rows in chunks on the main loop (AGENTS.md
//! rule 2); the filter runs on the listing workers with the sort.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::actions::Match;
use crate::browser::Browser;
use crate::i18n::tr;

pub struct FindBar {
    pub revealer: gtk::Revealer,
    pub entry: gtk::SearchEntry,
    /// The applied filter (lowercase), if any.
    pub filter: RefCell<Option<Arc<str>>>,
    /// Focus the list once the filtered order arrives.
    focus_after_sort: Cell<bool>,
}

impl FindBar {
    pub fn new() -> Self {
        let entry = gtk::SearchEntry::new();
        entry.set_hexpand(true);
        entry.set_placeholder_text(Some(&tr("Find in this folder (Enter filters)")));
        // Jump as the user types; the scan is cheap and chunked.
        entry.set_search_delay(0);
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.add_css_class("tudi-find");
        bar.append(&entry);
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::None)
            .child(&bar)
            .build();
        Self {
            revealer,
            entry,
            filter: RefCell::default(),
            focus_after_sort: Cell::new(false),
        }
    }
}

impl Browser {
    /// `Ctrl+F`: show the find bar and focus it.
    pub(crate) fn open_find(&self) {
        self.find.revealer.set_reveal_child(true);
        self.find.entry.grab_focus();
        self.find.entry.select_region(0, -1);
    }

    /// Hide the find bar and clear any filter. With `focus`, give the
    /// keyboard to the list at the selected row.
    pub(crate) fn close_find(&self, focus: bool) {
        if !self.find.revealer.reveals_child() && self.find.filter.borrow().is_none() {
            return;
        }
        self.find.revealer.set_reveal_child(false);
        self.find.entry.remove_css_class("error");
        self.find.focus_after_sort.set(false);
        self.set_find_filter(None);
        if focus {
            self.focus_list_at_selection();
        }
    }

    fn focus_list_at_selection(&self) {
        match self.focused_row() {
            Some(row) => {
                self.pane_view
                    .scroll_to(row as u32, gtk::ListScrollFlags::FOCUS, None);
            }
            None => {
                self.pane_view.grab_focus();
            }
        }
    }

    /// The lowercase find text, or `None` when the field is empty.
    fn find_needle(&self) -> Option<String> {
        let text = self.find.entry.text();
        (!text.is_empty()).then(|| text.to_lowercase())
    }

    /// Jump to a matching row: the first one (`from_top`), or the next or
    /// previous one after the selected row.
    pub(crate) fn find_jump(self: &Rc<Self>, from_top: bool, forward: bool) {
        let serial = self.next_scan_serial();
        let Some(needle) = self.find_needle() else {
            self.find.entry.remove_css_class("error");
            return;
        };
        let n = self.pane.n_items() as usize;
        let start = match (from_top, self.focused_row()) {
            (true, _) | (_, None) => 0,
            (false, Some(cur)) if forward => (cur + 1) % n.max(1),
            (false, Some(cur)) => (cur + n.max(1) - 1) % n.max(1),
        };
        self.scan_rows(Match::Contains(needle), serial, start, forward);
    }

    /// A find scan ended: mark the field when nothing matched.
    pub(crate) fn find_result(&self, found: bool) {
        if found {
            self.find.entry.remove_css_class("error");
        } else {
            self.find.entry.add_css_class("error");
        }
    }

    /// `Enter`: filter the list to the matching rows (or clear the filter).
    pub(crate) fn apply_find_filter(&self) {
        let needle = self.find_needle();
        self.find.entry.remove_css_class("error");
        // The row the typing jumped to matches, so it stays selected.
        if self.set_find_filter(needle.map(Arc::from)) {
            self.find.focus_after_sort.set(true);
        } else {
            self.focus_list_at_selection();
        }
    }

    /// The filtered order replaced the rows: focus the selected row, or the
    /// first one.
    pub(crate) fn after_find_sort(&self) {
        if !self.find.focus_after_sort.take() {
            return;
        }
        if self.focused_row().is_none() && self.pane.n_items() > 0 {
            self.pane_sel.select_item(0, true);
        }
        self.focus_list_at_selection();
    }

    /// Returns whether the filter changed (a re-sort was requested).
    fn set_find_filter(&self, filter: Option<Arc<str>>) -> bool {
        if *self.find.filter.borrow() == filter {
            return false;
        }
        self.find.filter.replace(filter);
        self.request_sort();
        true
    }

    pub(crate) fn connect_find(self: &Rc<Self>) {
        let entry = &self.find.entry;
        let w = Rc::downgrade(self);
        entry.connect_search_changed(move |_| {
            if let Some(b) = w.upgrade() {
                b.find_jump(true, true);
            }
        });
        let w = Rc::downgrade(self);
        entry.connect_activate(move |_| {
            if let Some(b) = w.upgrade() {
                b.apply_find_filter();
            }
        });
        let w = Rc::downgrade(self);
        entry.connect_stop_search(move |_| {
            if let Some(b) = w.upgrade() {
                b.close_find(true);
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(b) = w.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            match key {
                gdk::Key::Down | gdk::Key::KP_Down => b.find_jump(false, true),
                gdk::Key::Up | gdk::Key::KP_Up => b.find_jump(false, false),
                gdk::Key::F3 => b.find_jump(false, !shift),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        entry.add_controller(keys);
    }
}
