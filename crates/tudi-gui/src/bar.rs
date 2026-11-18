//! The bookmarks bar under the header (PLAN §5.7, ADR-016): the user's
//! bookmarks first, then — after a separator — the most used folders by
//! frecency, like a browser's bookmarks bar. At most [`BAR_BUTTONS`]
//! buttons (launchers, then bookmarks, then frequent folders); the rest
//! are in a drop-down list at the right end ("N more").
//!
//! Launchers (ADR-014) come first, before the bookmarks: clicking runs the
//! script or app; dropping files on one runs it with them; right-click
//! switches terminal / background, edits arguments, shows it in its folder.
//!
//! Bookmarks: `Ctrl+D` adds the current folder, `Alt+1..9` opens them,
//! right-click renames / moves / removes, drag and drop reorders.
//! Frequent folders: right-click pins as a bookmark, excludes from the
//! history (with or without subfolders) or forgets.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};

use crate::browser::Browser;
use crate::i18n::{fill, ntr, tr};

/// Most buttons on the bar; the rest go to the drop-down list.
const BAR_BUTTONS: usize = 10;

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
    let l = gtk::Label::new(Some(label));
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
    let l = gtk::Label::new(Some(label));
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
        let clip = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .hexpand(true)
            .child(&self.bar_box)
            .build();
        self.bar_more.add_css_class("flat");
        self.bar_more.set_always_show_arrow(true);
        self.bar_more
            .set_tooltip_text(Some(&tr("More bookmarks and frequent folders")));
        self.bar_more.set_visible(false);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("tudi-bar");
        row.append(&clip);
        row.append(&self.bar_more);
        row.upcast()
    }

    /// Actions used by the bar's buttons and menus.
    pub(crate) fn build_bar(self: &Rc<Self>) {
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
            "remove-bookmark",
            Box::new(|b, i| {
                b.shared.bookmarks.borrow_mut().remove(i);
                b.shared.bookmarks_changed();
            }),
        );
        add(
            "bookmark-left",
            Box::new(|b, i| {
                if i > 0 {
                    b.shared.bookmarks.borrow_mut().move_to(i, i - 1);
                    b.shared.bookmarks_changed();
                }
            }),
        );
        add(
            "bookmark-right",
            Box::new(|b, i| {
                b.shared.bookmarks.borrow_mut().move_to(i, i + 1);
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

    /// `Alt+1..9`.
    pub(crate) fn open_bookmark(self: &Rc<Self>, i: usize) {
        self.open_target(Target::Bookmark(i));
    }

    /// `Ctrl+D`: bookmark the current folder.
    pub(crate) fn bookmark_current(&self) {
        let cur = self.current.borrow().clone();
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
    pub(crate) fn rebuild_bar(&self) {
        while let Some(c) = self.bar_box.first_child() {
            self.bar_box.remove(&c);
        }
        let bookmarks = self.shared.bookmarks.borrow();
        let hist = self.hist.borrow();
        let more = gio::Menu::new();
        let bm_section = gio::Menu::new();
        // Buttons so far; later items only go to the drop-down list.
        let mut shown = 0usize;
        let mut hidden = 0u64;
        // Launchers first, then the folder bookmarks.
        let launchers = self.shared.launchers.borrow();
        let ln_section = gio::Menu::new();
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
            if shown < BAR_BUTTONS {
                shown += 1;
                let btn = launcher_button(&label, &tip);
                btn.set_action_name(Some("bar.run-launcher"));
                btn.set_action_target_value(Some(&(i as i32).to_variant()));
                self.launcher_menu(&btn, i, launchers.items.len(), l);
                self.launcher_drop(&btn, i);
                self.bar_box.append(&btn);
                continue;
            }
            hidden += 1;
            let item = gio::MenuItem::new(Some(&format!("▶ {label}")), None);
            item.set_action_and_target_value(
                Some("bar.run-launcher"),
                Some(&(i as i32).to_variant()),
            );
            ln_section.append_item(&item);
        }
        for (i, bm) in bookmarks.items.iter().enumerate() {
            let label = bm.label();
            let mut tip = bm.path.clone();
            if i < 9 {
                tip.push_str(&format!("  (Alt+{})", i + 1));
            }
            if shown < BAR_BUTTONS {
                shown += 1;
                let btn = folder_button(&label, &tip);
                btn.set_action_name(Some("bar.open-bookmark"));
                btn.set_action_target_value(Some(&(i as i32).to_variant()));
                self.bookmark_menu(&btn, i, bookmarks.items.len());
                self.bookmark_drag(&btn, i);
                self.bar_box.append(&btn);
                continue;
            }
            hidden += 1;
            let item = gio::MenuItem::new(Some(&label), None);
            item.set_action_and_target_value(
                Some("bar.open-bookmark"),
                Some(&(i as i32).to_variant()),
            );
            bm_section.append_item(&item);
        }
        if bookmarks.items.is_empty() && launchers.items.is_empty() {
            let hint = gtk::Label::new(Some(&tr("Ctrl+D bookmarks the current folder")));
            hint.add_css_class("dim-label");
            hint.add_css_class("tudi-bar-hint");
            self.bar_box.append(&hint);
        }
        let freq: Vec<(usize, &tudi_core::frecency::Entry)> = hist
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !bookmarks.contains(&e.path))
            .collect();
        if !freq.is_empty() && shown < BAR_BUTTONS {
            self.bar_box
                .append(&gtk::Separator::new(gtk::Orientation::Vertical));
        }
        let freq_section = gio::Menu::new();
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
            if shown < BAR_BUTTONS {
                shown += 1;
                let btn = folder_button(&label, &tip);
                btn.add_css_class("tudi-frequent");
                btn.set_action_name(Some("bar.open-frequent"));
                btn.set_action_target_value(Some(&(i as i32).to_variant()));
                self.frequent_menu(&btn, i);
                self.bar_box.append(&btn);
                continue;
            }
            hidden += 1;
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
            freq_section.append_item(&item);
        }
        for (title, section) in [
            (tr("Launchers"), &ln_section),
            (tr("Bookmarks"), &bm_section),
            (tr("Frequent"), &freq_section),
        ] {
            if section.n_items() > 0 {
                more.append_section(Some(&title), section);
            }
        }
        self.bar_more.set_menu_model(Some(&more));
        self.bar_more
            .set_label(&fill(&ntr("{} more", "{} more", hidden), &[&hidden]));
        self.bar_more.set_visible(hidden > 0);
    }

    fn popup_menu(widget: &gtk::Button, menu: &gio::Menu, x: f64, y: f64) {
        let pop = gtk::PopoverMenu::from_model(Some(menu));
        pop.set_parent(widget);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.set_has_arrow(false);
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
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

    fn bookmark_menu(&self, btn: &gtk::Button, i: usize, n: usize) {
        let menu = gio::Menu::new();
        let item = |label: String, action: &str| {
            let it = gio::MenuItem::new(Some(&label), None);
            it.set_action_and_target_value(Some(action), Some(&(i as i32).to_variant()));
            it
        };
        menu.append_item(&item(tr("Rename…"), "bar.rename-bookmark"));
        if i > 0 {
            menu.append_item(&item(tr("Move Left"), "bar.bookmark-left"));
        }
        if i + 1 < n {
            menu.append_item(&item(tr("Move Right"), "bar.bookmark-right"));
        }
        menu.append_item(&item(tr("Remove Bookmark"), "bar.remove-bookmark"));
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
            *self.select_after_load.borrow_mut() = Some(name.as_bytes().to_vec());
            self.navigate(dir, true);
        }
    }

    /// Drag a bookmark onto another to move it there.
    fn bookmark_drag(&self, btn: &gtk::Button, i: usize) {
        let src = gtk::DragSource::new();
        src.set_actions(gdk::DragAction::MOVE);
        src.connect_prepare(move |_, _, _| {
            Some(gdk::ContentProvider::for_value(
                &format!("tudi-bookmark:{i}").to_value(),
            ))
        });
        btn.add_controller(src);
        let drop = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        let shared = Rc::downgrade(&self.shared);
        drop.connect_drop(move |_, v, _, _| {
            let Some(from) = v
                .get::<String>()
                .ok()
                .and_then(|s| s.strip_prefix("tudi-bookmark:")?.parse::<usize>().ok())
            else {
                return false;
            };
            if let Some(sh) = shared.upgrade() {
                sh.bookmarks.borrow_mut().move_to(from, i);
                // Rebuild after the drag finishes (this button is replaced).
                glib::idle_add_local_once(move || sh.bookmarks_changed());
            }
            true
        });
        btn.add_controller(drop);
    }
}
