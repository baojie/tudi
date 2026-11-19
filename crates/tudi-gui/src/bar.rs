//! The bookmarks bar under the header (PLAN §5.7, ADR-016): the user's
//! bookmarks first, then — after a separator — the most used folders by
//! frecency, like a browser's bookmarks bar. As many buttons as fit the
//! window's width (launchers, then bookmarks, then frequent folders); the
//! rest are in a drop-down list at the right end ("N more"). Long names
//! are shortened on the buttons (first two words, or a length limit); the
//! tooltip and the drop-down show them in full.
//!
//! Launchers (ADR-014) come first, before the bookmarks: clicking runs the
//! script or app; dropping files on one runs it with them; right-click
//! switches terminal / background, edits arguments, shows it in its folder.
//!
//! Bookmarks: `Ctrl+D` adds the current folder, `Alt+1..9` opens them (in
//! the shown order), right-click renames / removes. Bookmarks and frequent
//! folders are each ordered by how often they were opened, most first
//! (ties keep the saved order) — until bookmarks are dragged into another
//! order, which is then kept ("Sort by Open Count" goes back).
//! Frequent folders: right-click pins as a bookmark, excludes from the
//! history (with or without subfolders) or forgets.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};

use crate::browser::Browser;
use crate::i18n::{fill, ntr, tr};

/// Most buttons built; later items are only in the drop-down list (no
/// window is wide enough to show more).
const MAX_BUILT: usize = 40;

/// Widest button label, in terminal-like columns (CJK counts two).
const LABEL_COLUMNS: usize = 16;

/// Which drop-down section an item belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Launchers,
    Bookmarks,
    Frequent,
}

/// One bar entry: its button (if built) and its drop-down item.
struct BarItem {
    button: Option<gtk::Button>,
    /// Natural width of the button, measured when built.
    width: i32,
    section: Section,
    item: gio::MenuItem,
}

/// Bar widgets and entries of one window.
pub struct BarState {
    clip: gtk::ScrolledWindow,
    items: RefCell<Vec<BarItem>>,
    /// Between the bookmarks and the frequent folders.
    separator: RefCell<Option<(gtk::Separator, i32)>>,
    fit_scheduled: Cell<bool>,
}

impl Default for BarState {
    fn default() -> Self {
        Self {
            clip: gtk::ScrolledWindow::new(),
            items: RefCell::default(),
            separator: RefCell::default(),
            fit_scheduled: Cell::new(false),
        }
    }
}

/// Display width of a character (East Asian wide characters count two).
fn char_columns(c: char) -> usize {
    let u = c as u32;
    let wide = matches!(u,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD);
    if wide { 2 } else { 1 }
}

fn columns(s: &str) -> usize {
    s.chars().map(char_columns).sum()
}

/// A button label for `name`: unchanged if short; else its first two words
/// (split at spaces, `-`, `_`, `.`) if those fit; else cut to the limit.
/// An ellipsis marks a shortened label.
pub(crate) fn short_label(name: &str) -> String {
    if columns(name) <= LABEL_COLUMNS {
        return name.to_string();
    }
    let is_sep = |c: char| c.is_whitespace() || matches!(c, '-' | '_' | '.');
    // End of the second word: the second separator run after a word.
    let mut words = 0;
    let mut in_word = false;
    let mut end = None;
    for (i, c) in name.char_indices() {
        if is_sep(c) {
            if in_word {
                words += 1;
                if words == 2 {
                    end = Some(i);
                    break;
                }
            }
            in_word = false;
        } else {
            in_word = true;
        }
    }
    if let Some(end) = end {
        let two = &name[..end];
        if columns(two) < LABEL_COLUMNS {
            return format!("{two}…");
        }
    }
    let mut out = String::new();
    let mut w = 0;
    for c in name.chars() {
        let cw = char_columns(c);
        if w + cw > LABEL_COLUMNS - 1 {
            break;
        }
        w += cw;
        out.push(c);
    }
    format!("{}…", out.trim_end_matches(is_sep))
}

/// A bar action taking a bookmark or frequent-folder index.
type IndexAction = Box<dyn Fn(&Rc<Browser>, usize)>;

/// What a bar button opens.
#[derive(Clone, Debug)]
enum Target {
    Bookmark(usize),
    Frequent(usize),
}

fn folder_button(label: &str, tooltip: &str) -> gtk::Button {
    let icon = crate::icons::FileIcon::default();
    let l = gtk::Label::new(Some(&short_label(label)));
    l.set_ellipsize(pango::EllipsizeMode::End);
    l.set_max_width_chars(20);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    content.append(&icon);
    content.append(&l);
    let b = gtk::Button::builder().child(&content).build();
    b.add_css_class("flat");
    b.add_css_class("tudi-bar-button");
    b.set_tooltip_text(Some(tooltip));
    b
}

fn launcher_button(label: &str, tooltip: &str) -> gtk::Button {
    let icon = crate::icons::FileIcon::new(crate::icons::Kind::Executable);
    let l = gtk::Label::new(Some(&short_label(label)));
    l.set_ellipsize(pango::EllipsizeMode::End);
    l.set_max_width_chars(20);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    content.append(&icon);
    content.append(&l);
    let b = gtk::Button::builder().child(&content).build();
    b.add_css_class("flat");
    b.add_css_class("tudi-bar-button");
    b.add_css_class("tudi-launcher");
    b.set_tooltip_text(Some(tooltip));
    b
}

fn path_label(p: &Path) -> String {
    p.file_name()
        .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned()
}

impl Browser {
    /// The bar row: scrollable (clipped) buttons and the "»" menu.
    pub(crate) fn bar_widget(&self) -> gtk::Widget {
        self.bar_box.add_css_class("tudi-bar-items");
        // An external scrollbar clips the buttons instead of making the
        // window as wide as all of them.
        let clip = &self.bar.clip;
        clip.set_policy(gtk::PolicyType::External, gtk::PolicyType::Never);
        clip.set_propagate_natural_height(true);
        clip.set_hexpand(true);
        clip.set_child(Some(&self.bar_box));
        self.bar_more.add_css_class("flat");
        self.bar_more.set_always_show_arrow(true);
        self.bar_more
            .set_tooltip_text(Some(&tr("More bookmarks and frequent folders")));
        self.bar_more.set_visible(false);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("tudi-bar");
        row.append(clip);
        row.append(&self.bar_more);
        row.upcast()
    }

    /// Refit the bar after a layout pass (its width changed, or it was
    /// rebuilt). Deferred: visibility must not change during allocation.
    pub(crate) fn schedule_bar_fit(self: &Rc<Self>) {
        if self.bar.fit_scheduled.replace(true) {
            return;
        }
        let w = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(b) = w.upgrade() {
                b.bar.fit_scheduled.set(false);
                b.fit_bar();
            }
        });
    }

    /// Show the leading buttons that fit the bar's width; list the rest in
    /// the "N more" drop-down.
    fn fit_bar(&self) {
        let more_shown = self.bar_more.is_visible();
        let avail = self.bar.clip.width() + if more_shown { self.bar_more.width() } else { 0 };
        if avail <= 0 {
            return;
        }
        let items = self.bar.items.borrow();
        let sep = self.bar.separator.borrow();
        let spacing = self.bar_box.spacing();
        // Buttons that fit in `limit`, in order.
        let fitting = |limit: i32| {
            let mut used = 0;
            let mut n = 0;
            for (k, it) in items.iter().enumerate() {
                if it.button.is_none() {
                    break;
                }
                let mut w = it.width + spacing;
                let first_freq = it.section == Section::Frequent
                    && (k == 0 || items[k - 1].section != Section::Frequent);
                if first_freq && let Some((_, sw)) = sep.as_ref() {
                    w += sw + spacing;
                }
                if used + w > limit {
                    break;
                }
                used += w;
                n += 1;
            }
            n
        };
        let mut shown = fitting(avail);
        if shown < items.len() {
            // Room for the drop-down button too.
            let (_, more_w, _, _) = self.bar_more.measure(gtk::Orientation::Horizontal, -1);
            shown = fitting(avail - more_w.max(self.bar_more.width()));
        }
        let more = gio::Menu::new();
        let sections = [
            (Section::Launchers, tr("Launchers")),
            (Section::Bookmarks, tr("Bookmarks")),
            (Section::Frequent, tr("Frequent")),
        ];
        for (section, title) in sections {
            let menu = gio::Menu::new();
            for it in items[shown..].iter().filter(|it| it.section == section) {
                menu.append_item(&it.item);
            }
            if menu.n_items() > 0 {
                more.append_section(Some(&title), &menu);
            }
        }
        for (k, it) in items.iter().enumerate() {
            if let Some(btn) = &it.button {
                btn.set_visible(k < shown);
            }
        }
        if let Some((s, _)) = sep.as_ref() {
            let freq_shown = items[..shown]
                .iter()
                .any(|it| it.section == Section::Frequent);
            s.set_visible(freq_shown);
        }
        let hidden = (items.len() - shown) as u64;
        self.bar_more.set_menu_model(Some(&more));
        self.bar_more
            .set_label(&fill(&ntr("{} more", "{} more", hidden), &[&hidden]));
        self.bar_more.set_visible(hidden > 0);
    }

    /// Actions used by the bar's buttons and menus.
    pub(crate) fn build_bar(self: &Rc<Self>) {
        // The clip's page size follows the bar's width.
        let w = Rc::downgrade(self);
        self.bar.clip.hadjustment().connect_changed(move |_| {
            if let Some(b) = w.upgrade() {
                b.schedule_bar_fit();
            }
        });
        let group = gio::SimpleActionGroup::new();
        let int = Some(glib::VariantTy::INT32);
        let add = |name: &str, f: IndexAction| {
            let a = gio::SimpleAction::new(name, int);
            let w = Rc::downgrade(self);
            a.connect_activate(move |_, v| {
                if let (Some(b), Some(i)) = (w.upgrade(), v.and_then(|v| v.get::<i32>())) {
                    f(&b, i as usize);
                }
            });
            group.add_action(&a);
        };
        add(
            "open-bookmark",
            Box::new(|b, i| b.open_target(Target::Bookmark(i))),
        );
        add(
            "open-frequent",
            Box::new(|b, i| b.open_target(Target::Frequent(i))),
        );
        add("rename-bookmark", Box::new(|b, i| b.rename_bookmark(i)));
        add(
            "sort-bookmarks",
            Box::new(|b, _| {
                b.shared.bookmarks.borrow_mut().manual_order = false;
                b.shared.bookmarks_changed();
            }),
        );
        add(
            "remove-bookmark",
            Box::new(|b, i| {
                b.shared.bookmarks.borrow_mut().remove(i);
                b.shared.bookmarks_changed();
            }),
        );
        add("pin", Box::new(|b, i| b.pin_frequent(i)));
        add(
            "run-launcher",
            Box::new(|b, i| b.run_launcher(i, Vec::new())),
        );
        add(
            "launcher-terminal",
            Box::new(|b, i| b.set_launcher_mode(i, tudi_core::launchers::RunMode::Terminal)),
        );
        add(
            "launcher-background",
            Box::new(|b, i| b.set_launcher_mode(i, tudi_core::launchers::RunMode::Background)),
        );
        add("launcher-args", Box::new(|b, i| b.edit_launcher_args(i)));
        add("launcher-rename", Box::new(|b, i| b.rename_launcher(i)));
        add("launcher-show", Box::new(|b, i| b.show_launcher(i)));
        add(
            "launcher-left",
            Box::new(|b, i| {
                if i > 0 {
                    b.shared.launchers.borrow_mut().move_to(i, i - 1);
                    b.shared.launchers_changed();
                }
            }),
        );
        add(
            "launcher-right",
            Box::new(|b, i| {
                b.shared.launchers.borrow_mut().move_to(i, i + 1);
                b.shared.launchers_changed();
            }),
        );
        add(
            "launcher-remove",
            Box::new(|b, i| {
                b.shared.launchers.borrow_mut().remove(i);
                b.shared.launchers_changed();
            }),
        );
        add("exclude", Box::new(|b, i| b.exclude_frequent(i, false)));
        add("exclude-tree", Box::new(|b, i| b.exclude_frequent(i, true)));
        add(
            "forget",
            Box::new(|b, i| {
                if let Some(p) = b.frequent_path(i) {
                    b.shared.history.forget(p);
                    b.shared.history_changed();
                }
            }),
        );
        self.window.insert_action_group("bar", Some(&group));
        self.rebuild_bar();
        self.shared.history.query(self.worker.reply_sender());
    }

    fn frequent_path(&self, i: usize) -> Option<PathBuf> {
        self.hist.borrow().entries.get(i).map(|e| e.path.clone())
    }

    fn open_target(self: &Rc<Self>, t: Target) {
        let path = match t {
            Target::Bookmark(i) => self
                .shared
                .bookmarks
                .borrow()
                .items
                .get(i)
                .map(|b| PathBuf::from(&b.path)),
            Target::Frequent(i) => self.frequent_path(i),
        };
        if let Some(p) = path {
            self.navigate(&p, true);
            self.record_visit(&p);
        }
    }

    /// `Alt+1..9`: the `n`th bookmark as shown.
    pub(crate) fn open_bookmark(self: &Rc<Self>, n: usize) {
        if let Some(&i) = self.bookmark_order().get(n) {
            self.open_target(Target::Bookmark(i));
        }
    }

    /// Bookmark indices in display order: most opened first, ties in the
    /// saved order — or the saved order once the user dragged them.
    fn bookmark_order(&self) -> Vec<usize> {
        let bookmarks = self.shared.bookmarks.borrow();
        let hist = self.hist.borrow();
        let count = |i: usize| {
            hist.counts
                .get(Path::new(&bookmarks.items[i].path))
                .copied()
                .unwrap_or(0)
        };
        let mut order: Vec<usize> = (0..bookmarks.items.len()).collect();
        if !bookmarks.manual_order {
            order.sort_by_key(|&i| std::cmp::Reverse(count(i)));
        }
        order
    }

    /// `Ctrl+D`: bookmark the current folder.
    pub(crate) fn bookmark_current(&self) {
        let cur = self.tab().current.borrow().clone();
        let added = self.shared.bookmarks.borrow_mut().add(&cur);
        if added {
            self.toast(&fill(&tr("Bookmarked {}"), &[&path_label(&cur)]));
            self.shared.bookmarks_changed();
        } else {
            self.toast(&tr("Already bookmarked"));
        }
    }

    fn pin_frequent(&self, i: usize) {
        if let Some(p) = self.frequent_path(i)
            && self.shared.bookmarks.borrow_mut().add(&p)
        {
            self.shared.bookmarks_changed();
        }
    }

    fn exclude_frequent(&self, i: usize, recursive: bool) {
        if let Some(p) = self.frequent_path(i) {
            self.shared.history.exclude(p, recursive);
            self.shared.history_changed();
        }
    }

    fn rename_bookmark(self: &Rc<Self>, i: usize) {
        let Some(current) = self
            .shared
            .bookmarks
            .borrow()
            .items
            .get(i)
            .map(|b| b.label())
        else {
            return;
        };
        let w = Rc::downgrade(self);
        self.ask_name(
            &tr("Rename Bookmark"),
            &tr("Rename"),
            &current,
            move |name| {
                if let Some(b) = w.upgrade() {
                    b.shared.bookmarks.borrow_mut().rename(i, &name);
                    b.shared.bookmarks_changed();
                }
            },
        );
    }

    /// Recreate the bar's buttons and the "»" menu from the bookmarks and
    /// the latest history snapshot. At most a few dozen widgets.
    pub(crate) fn rebuild_bar(self: &Rc<Self>) {
        while let Some(c) = self.bar_box.first_child() {
            self.bar_box.remove(&c);
        }
        self.bar.separator.replace(None);
        let order = self.bookmark_order();
        let bookmarks = self.shared.bookmarks.borrow();
        let hist = self.hist.borrow();
        let mut items: Vec<BarItem> = Vec::new();
        // Add a button (while fewer than MAX_BUILT) and remember its width.
        let place = |items: &mut Vec<BarItem>,
                     section: Section,
                     item: gio::MenuItem,
                     make: &dyn Fn() -> gtk::Button| {
            let button = (items.len() < MAX_BUILT).then(|| {
                let btn = make();
                self.bar_box.append(&btn);
                btn
            });
            let width = button
                .as_ref()
                .map_or(0, |b| b.measure(gtk::Orientation::Horizontal, -1).1);
            items.push(BarItem {
                button,
                width,
                section,
                item,
            });
        };
        // Launchers first, then the folder bookmarks.
        let launchers = self.shared.launchers.borrow();
        for (i, l) in launchers.items.iter().enumerate() {
            let label = l.label();
            let mode = if l.is_desktop_file() {
                tr("application")
            } else if l.mode == tudi_core::launchers::RunMode::Terminal {
                tr("runs in a terminal")
            } else {
                tr("runs in the background")
            };
            let tip = format!(
                "{}\n{mode}\n{}",
                l.path,
                tr("Click to run · drop files here to run with them")
            );
            let item = gio::MenuItem::new(Some(&format!("▶ {label}")), None);
            item.set_action_and_target_value(
                Some("bar.run-launcher"),
                Some(&(i as i32).to_variant()),
            );
            place(&mut items, Section::Launchers, item, &|| {
                let btn = launcher_button(&label, &tip);
                btn.set_action_name(Some("bar.run-launcher"));
                btn.set_action_target_value(Some(&(i as i32).to_variant()));
                self.launcher_menu(&btn, i, launchers.items.len(), l);
                self.launcher_drop(&btn, i);
                btn
            });
        }
        for (n, &i) in order.iter().enumerate() {
            let bm = &bookmarks.items[i];
            let label = bm.label();
            let mut tip = bm.path.clone();
            if let Some(&c) = hist.counts.get(Path::new(&bm.path)) {
                tip.push('\n');
                tip.push_str(&fill(&ntr("opened {} time", "opened {} times", c), &[&c]));
            }
            if n < 9 {
                tip.push_str(&format!("  (Alt+{})", n + 1));
            }
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(
                Some("bar.open-bookmark"),
                Some(&(i as i32).to_variant()),
            );
            place(&mut items, Section::Bookmarks, item, &|| {
                let btn = folder_button(&label, &tip);
                btn.set_action_name(Some("bar.open-bookmark"));
                btn.set_action_target_value(Some(&(i as i32).to_variant()));
                self.bookmark_menu(&btn, i);
                self.bookmark_drag(&btn, n);
                btn
            });
        }
        if bookmarks.items.is_empty() && launchers.items.is_empty() {
            let hint = gtk::Label::new(Some(&tr("Ctrl+D bookmarks the current folder")));
            hint.add_css_class("dim-label");
            hint.add_css_class("tudi-bar-hint");
            self.bar_box.append(&hint);
        }
        let mut freq: Vec<(usize, &tudi_core::frecency::Entry)> = hist
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !bookmarks.contains(&e.path))
            .collect();
        // The most used folders (by frecency), most opened first.
        freq.sort_by_key(|(_, e)| std::cmp::Reverse(e.count));
        if !freq.is_empty() && items.len() < MAX_BUILT {
            let sep = gtk::Separator::new(gtk::Orientation::Vertical);
            self.bar_box.append(&sep);
            let w = sep.measure(gtk::Orientation::Horizontal, -1).1;
            self.bar.separator.replace(Some((sep, w)));
        }
        for &(i, e) in &freq {
            let label = path_label(&e.path);
            let tip = format!(
                "{}\n{} · {}",
                e.path.display(),
                fill(
                    &ntr("opened {} time", "opened {} times", e.count),
                    &[&e.count]
                ),
                crate::history::when(e.last)
            );
            let item = gio::MenuItem::new(
                Some(&format!(
                    "{label}  ({})",
                    fill(&ntr("{} open", "{} opens", e.count), &[&e.count])
                )),
                None,
            );
            item.set_action_and_target_value(
                Some("bar.open-frequent"),
                Some(&(i as i32).to_variant()),
            );
            place(&mut items, Section::Frequent, item, &|| {
                let btn = folder_button(&label, &tip);
                btn.add_css_class("tudi-frequent");
                btn.set_action_name(Some("bar.open-frequent"));
                btn.set_action_target_value(Some(&(i as i32).to_variant()));
                self.frequent_menu(&btn, i);
                btn
            });
        }
        drop((launchers, bookmarks, hist));
        self.bar.items.replace(items);
        self.fit_bar();
        self.schedule_bar_fit();
    }

    fn popup_menu(widget: &gtk::Button, menu: &gio::Menu, x: f64, y: f64) {
        crate::menu::popup_at(widget, menu.upcast_ref(), x, y);
    }

    fn on_right_click(btn: &gtk::Button, menu: gio::Menu) {
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        let b = btn.downgrade();
        click.connect_pressed(move |g, _, x, y| {
            if let Some(btn) = b.upgrade() {
                g.set_state(gtk::EventSequenceState::Claimed);
                Self::popup_menu(&btn, &menu, x, y);
            }
        });
        btn.add_controller(click);
    }

    fn bookmark_menu(&self, btn: &gtk::Button, i: usize) {
        let menu = gio::Menu::new();
        let item = |label: String, action: &str| {
            let it = gio::MenuItem::new(Some(&label), None);
            it.set_action_and_target_value(Some(action), Some(&(i as i32).to_variant()));
            it
        };
        menu.append_item(&item(tr("Rename…"), "bar.rename-bookmark"));
        menu.append_item(&item(tr("Remove Bookmark"), "bar.remove-bookmark"));
        if self.shared.bookmarks.borrow().manual_order {
            menu.append_item(&item(tr("Sort by Open Count"), "bar.sort-bookmarks"));
        }
        Self::on_right_click(btn, menu);
    }

    fn frequent_menu(&self, btn: &gtk::Button, i: usize) {
        let menu = gio::Menu::new();
        let item = |label: String, action: &str| {
            let it = gio::MenuItem::new(Some(&label), None);
            it.set_action_and_target_value(Some(action), Some(&(i as i32).to_variant()));
            it
        };
        menu.append_item(&item(tr("Pin as Bookmark"), "bar.pin"));
        let hide = gio::Menu::new();
        hide.append_item(&item(tr("Never show in history"), "bar.exclude"));
        hide.append_item(&item(
            tr("Never show, including subfolders"),
            "bar.exclude-tree",
        ));
        hide.append_item(&item(tr("Remove from history"), "bar.forget"));
        menu.append_section(None, &hide);
        Self::on_right_click(btn, menu);
    }

    fn launcher_menu(
        &self,
        btn: &gtk::Button,
        i: usize,
        n: usize,
        l: &tudi_core::launchers::Launcher,
    ) {
        let item = |label: String, action: &str| {
            let it = gio::MenuItem::new(Some(&label), None);
            it.set_action_and_target_value(Some(action), Some(&(i as i32).to_variant()));
            it
        };
        let menu = gio::Menu::new();
        let run = gio::Menu::new();
        run.append_item(&item(tr("Run"), "bar.run-launcher"));
        if !l.is_desktop_file() {
            let term = l.mode == tudi_core::launchers::RunMode::Terminal;
            run.append_item(&item(
                format!(
                    "{}{}",
                    if term { "✓ " } else { "    " },
                    tr("Run in a Terminal")
                ),
                "bar.launcher-terminal",
            ));
            run.append_item(&item(
                format!(
                    "{}{}",
                    if term { "    " } else { "✓ " },
                    tr("Run in the Background")
                ),
                "bar.launcher-background",
            ));
            run.append_item(&item(tr("Arguments…"), "bar.launcher-args"));
        }
        menu.append_section(None, &run);
        let manage = gio::Menu::new();
        manage.append_item(&item(tr("Show in Folder"), "bar.launcher-show"));
        manage.append_item(&item(tr("Rename…"), "bar.launcher-rename"));
        if i > 0 {
            manage.append_item(&item(tr("Move Left"), "bar.launcher-left"));
        }
        if i + 1 < n {
            manage.append_item(&item(tr("Move Right"), "bar.launcher-right"));
        }
        manage.append_item(&item(tr("Remove Launcher"), "bar.launcher-remove"));
        menu.append_section(None, &manage);
        Self::on_right_click(btn, menu);
    }

    /// Dropping files on a launcher runs it with them as arguments.
    fn launcher_drop(&self, btn: &gtk::Button, i: usize) {
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let w = self.window.downgrade();
        let shared = Rc::downgrade(&self.shared);
        drop.connect_drop(move |_, v, _, _| {
            let Ok(list) = v.get::<gdk::FileList>() else {
                return false;
            };
            let files: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            let (Some(win), Some(sh)) = (w.upgrade(), shared.upgrade()) else {
                return false;
            };
            // The browser owning this window (for its toasts).
            if let Some(b) = sh.each().into_iter().find(|b| b.window == win) {
                b.run_launcher(i, files);
            }
            true
        });
        btn.add_controller(drop);
    }

    fn set_launcher_mode(&self, i: usize, mode: tudi_core::launchers::RunMode) {
        if let Some(l) = self.shared.launchers.borrow_mut().items.get_mut(i) {
            l.mode = mode;
        }
        self.shared.launchers_changed();
    }

    fn edit_launcher_args(self: &Rc<Self>, i: usize) {
        let Some(current) = self
            .shared
            .launchers
            .borrow()
            .items
            .get(i)
            .map(|l| l.args.join(" "))
        else {
            return;
        };
        let w = Rc::downgrade(self);
        self.ask_text(&tr("Arguments"), &tr("Save"), &current, move |text| {
            if let Some(b) = w.upgrade() {
                if let Some(l) = b.shared.launchers.borrow_mut().items.get_mut(i) {
                    l.args = text.split_whitespace().map(String::from).collect();
                }
                b.shared.launchers_changed();
            }
        });
    }

    fn rename_launcher(self: &Rc<Self>, i: usize) {
        let Some(current) = self
            .shared
            .launchers
            .borrow()
            .items
            .get(i)
            .map(|l| l.label())
        else {
            return;
        };
        let w = Rc::downgrade(self);
        self.ask_text(
            &tr("Rename Launcher"),
            &tr("Rename"),
            &current,
            move |name| {
                if let Some(b) = w.upgrade() {
                    if let Some(l) = b.shared.launchers.borrow_mut().items.get_mut(i) {
                        let n = name.trim();
                        l.name = (!n.is_empty()).then(|| n.to_string());
                    }
                    b.shared.launchers_changed();
                }
            },
        );
    }

    /// Open the launcher's folder with the file selected.
    fn show_launcher(self: &Rc<Self>, i: usize) {
        let Some(p) = self
            .shared
            .launchers
            .borrow()
            .items
            .get(i)
            .map(|l| PathBuf::from(&l.path))
        else {
            return;
        };
        if let (Some(dir), Some(name)) = (p.parent(), p.file_name()) {
            use std::os::unix::ffi::OsStrExt;
            *self.tab().select_after_load.borrow_mut() = Some(name.as_bytes().to_vec());
            self.navigate(dir, true);
        }
    }

    /// Drag a bookmark onto another to put it there (shown positions). The
    /// shown order becomes the saved one (manual order).
    fn bookmark_drag(&self, btn: &gtk::Button, n: usize) {
        let src = gtk::DragSource::new();
        src.set_actions(gdk::DragAction::MOVE);
        // Before the button's own click gesture, which claims the press;
        // the drag claims it only once the pointer moved, so clicks work.
        src.set_propagation_phase(gtk::PropagationPhase::Capture);
        src.connect_prepare(move |_, _, _| {
            Some(gdk::ContentProvider::for_value(
                &format!("tudi-bookmark:{n}").to_value(),
            ))
        });
        btn.add_controller(src);
        let drop = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        let shared = Rc::downgrade(&self.shared);
        let order = self.bookmark_order();
        drop.connect_drop(move |_, v, _, _| {
            let Some(from) = v
                .get::<String>()
                .ok()
                .and_then(|s| s.strip_prefix("tudi-bookmark:")?.parse::<usize>().ok())
            else {
                return false;
            };
            if let Some(sh) = shared.upgrade() {
                sh.bookmarks.borrow_mut().reorder(&order, from, n);
                // Rebuild after the drag finishes (this button is replaced).
                glib::idle_add_local_once(move || sh.bookmarks_changed());
            }
            true
        });
        btn.add_controller(drop);
    }
}

#[cfg(test)]
mod tests {
    use super::short_label;

    #[test]
    fn short_names_are_kept() {
        assert_eq!(short_label("Downloads"), "Downloads");
        assert_eq!(short_label("7-讲座"), "7-讲座");
        assert_eq!(short_label("memex-weekly"), "memex-weekly");
    }

    #[test]
    fn long_names_keep_two_words() {
        assert_eq!(short_label("memex-weekly-test-archive"), "memex-weekly…");
        assert_eq!(short_label("My Project Files 2025"), "My Project…");
    }

    #[test]
    fn long_single_words_are_cut() {
        assert_eq!(
            short_label("averyveryverylongfoldername"),
            "averyveryverylo…"
        );
        // CJK characters count two columns: 6 of them and "SAT" fill 15.
        assert_eq!(
            short_label("机考SAT实考真题合集全套资料"),
            "机考SAT实考真题…"
        );
    }
}
