//! The clickable path bar (docs/ideas.md #3), like Windows Explorer's
//! breadcrumb: each folder of the shown path is a button; the `›` after it
//! lists that folder's subfolders to jump to; each button takes dropped
//! files. A click on the empty part of the bar, `Ctrl+L` or typing turns
//! it into the text entry; `Enter` goes there, `Esc` or leaving it turns
//! it back.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::browser::Browser;
use crate::i18n::tr;

/// Most subfolders listed in a `›` menu.
const MENU_LIMIT: usize = 300;

pub struct Crumbs {
    /// Crumbs or entry.
    pub stack: gtk::Stack,
    bar: gtk::Box,
    scroll: gtk::ScrolledWindow,
}

impl Crumbs {
    pub fn new(entry: &gtk::Entry) -> Self {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("tudi-crumbs");
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&bar)
            .hexpand(true)
            .build();
        // The bar looks like the entry (a field), so it reads as one.
        let frame = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        frame.add_css_class("tudi-crumb-field");
        frame.append(&scroll);
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.add_named(&frame, Some("crumbs"));
        stack.add_named(entry, Some("entry"));
        stack.set_visible_child_name("crumbs");
        Self { stack, bar, scroll }
    }
}

/// The folders of `path` from the top: (label, path). Below the home
/// folder it starts at "Home".
fn levels(path: &Path, home: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let (start, rest) = match path.strip_prefix(home) {
        Ok(rest) => {
            out.push((tr("Home"), home.to_path_buf()));
            (home.to_path_buf(), rest.to_path_buf())
        }
        Err(_) => {
            out.push(("/".to_string(), PathBuf::from("/")));
            (
                PathBuf::from("/"),
                path.strip_prefix("/").unwrap_or(path).to_path_buf(),
            )
        }
    };
    let mut cur = start;
    for c in rest.components() {
        cur.push(c);
        out.push((c.as_os_str().to_string_lossy().into_owned(), cur.clone()));
    }
    out
}

/// Subfolders of `dir`, sorted (on a helper thread).
fn subfolders(dir: &Path, hidden: bool) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .filter(|e| hidden || !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    v.sort_by_key(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    });
    v.truncate(MENU_LIMIT);
    v
}

impl Browser {
    /// Show `path` in the path bar (crumbs, and the entry's text).
    pub(crate) fn set_location(self: &Rc<Self>, path: &Path) {
        self.path_entry.set_text(&path.to_string_lossy());
        let c = &self.crumbs;
        while let Some(w) = c.bar.first_child() {
            c.bar.remove(&w);
        }
        let all = levels(path, &glib::home_dir());
        let last = all.len().saturating_sub(1);
        for (i, (label, dir)) in all.into_iter().enumerate() {
            let b = gtk::Button::with_label(&label);
            b.add_css_class("flat");
            b.add_css_class("tudi-crumb");
            if i == last {
                b.add_css_class("current");
            }
            b.set_tooltip_text(Some(&dir.to_string_lossy()));
            let (w, d) = (Rc::downgrade(self), dir.clone());
            b.connect_clicked(move |_| {
                if let Some(br) = w.upgrade() {
                    br.navigate(&d, true);
                    br.record_visit(&d);
                }
            });
            // Files dropped on a level go into that folder.
            let (w, d) = (Rc::downgrade(self), dir.clone());
            b.add_controller(crate::drop::file_drop_target(Rc::new(
                move |files, mods| {
                    w.upgrade()
                        .is_some_and(|br| br.drop_into(files, d.clone(), mods))
                },
            )));
            c.bar.append(&b);
            // The `›` lists the subfolders of this level.
            let more = gtk::Button::with_label("›");
            more.add_css_class("flat");
            more.add_css_class("tudi-crumb-sep");
            more.set_tooltip_text(Some(&tr("Subfolders")));
            let (w, d) = (Rc::downgrade(self), dir);
            more.connect_clicked(move |m| {
                if let Some(br) = w.upgrade() {
                    br.crumb_menu(m, d.clone());
                }
            });
            c.bar.append(&more);
        }
        // Keep the end (the current folder) in view.
        let sw = c.scroll.downgrade();
        c.scroll.add_tick_callback(move |_, _| {
            if let Some(sw) = sw.upgrade() {
                let a = sw.hadjustment();
                a.set_value(a.upper() - a.page_size());
            }
            glib::ControlFlow::Break
        });
    }

    /// The `›` after a level: its subfolders, listed off the main thread.
    fn crumb_menu(self: &Rc<Self>, button: &gtk::Button, dir: PathBuf) {
        let (tx, rx) = async_channel::bounded(1);
        let hidden = self.show_hidden.get();
        std::thread::spawn(move || {
            let _ = tx.send_blocking(subfolders(&dir, hidden));
        });
        let (w, bw) = (Rc::downgrade(self), button.downgrade());
        glib::spawn_future_local(async move {
            let Ok(dirs) = rx.recv().await else { return };
            let (Some(_), Some(button)) = (w.upgrade(), bw.upgrade()) else {
                return;
            };
            let menu = gio::Menu::new();
            if dirs.is_empty() {
                let it = gio::MenuItem::new(Some(&tr("No subfolders")), None);
                menu.append_item(&it);
            }
            for d in &dirs {
                let label = d
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let it = gio::MenuItem::new(Some(&label), None);
                it.set_action_and_target_value(
                    Some("crumb.go"),
                    Some(&d.to_string_lossy().to_variant()),
                );
                menu.append_item(&it);
            }
            let h = f64::from(button.height());
            crate::menu::popup_at(&button, menu.upcast_ref(), 0.0, h);
        });
    }

    /// `crumb.go(path)`, and switching between crumbs and the entry.
    pub(crate) fn install_crumbs(self: &Rc<Self>) {
        let group = gio::SimpleActionGroup::new();
        let go = gio::SimpleAction::new("go", Some(glib::VariantTy::STRING));
        let w = Rc::downgrade(self);
        go.connect_activate(move |_, v| {
            if let (Some(b), Some(p)) = (w.upgrade(), v.and_then(|v| v.get::<String>())) {
                let p = PathBuf::from(p);
                b.navigate(&p, true);
                b.record_visit(&p);
            }
        });
        group.add_action(&go);
        self.window.insert_action_group("crumb", Some(&group));
        // A click on the bar's empty part: type a path.
        let click = gtk::GestureClick::new();
        let w = Rc::downgrade(self);
        click.connect_released(move |g, _, x, y| {
            let Some(b) = w.upgrade() else { return };
            let Some(bar) = g.widget() else { return };
            let on_button =
                std::iter::successors(bar.pick(x, y, gtk::PickFlags::DEFAULT), |c| c.parent())
                    .take_while(|c| *c != bar)
                    .any(|c| c.is::<gtk::Button>());
            if !on_button {
                b.edit_location();
            }
        });
        if let Some(field) = self.crumbs.stack.first_child() {
            field.add_controller(click);
        }
        // Leaving the entry: back to crumbs (the shown folder).
        let focus = gtk::EventControllerFocus::new();
        let w = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(b) = w.upgrade() {
                b.show_crumbs();
            }
        });
        self.path_entry.add_controller(focus);
        let keys = gtk::EventControllerKey::new();
        let w = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape
                && let Some(b) = w.upgrade()
            {
                b.show_crumbs();
                b.tab().view().grab_focus();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.path_entry.add_controller(keys);
    }

    /// `Ctrl+L` or a click on the bar: the path as text, selected.
    pub(crate) fn edit_location(&self) {
        self.crumbs.stack.set_visible_child_name("entry");
        self.path_entry.grab_focus();
        self.path_entry.select_region(0, -1);
    }

    /// Back to the crumbs, with the shown folder's path in the entry.
    pub(crate) fn show_crumbs(&self) {
        let cur = self.active.borrow().current.borrow().clone();
        self.path_entry.set_text(&cur.to_string_lossy());
        self.crumbs.stack.set_visible_child_name("crumbs");
    }
}

#[cfg(test)]
mod tests {
    use super::levels;
    use std::path::Path;

    #[test]
    fn levels_start_at_home_or_root() {
        let home = Path::new("/home/u");
        let l = levels(Path::new("/home/u/a/b"), home);
        let paths: Vec<_> = l.iter().map(|(_, p)| p.to_str().unwrap()).collect();
        assert_eq!(paths, ["/home/u", "/home/u/a", "/home/u/a/b"]);
        assert_eq!(l[2].0, "b");
        let l = levels(Path::new("/etc/x"), home);
        let paths: Vec<_> = l.iter().map(|(_, p)| p.to_str().unwrap()).collect();
        assert_eq!(paths, ["/", "/etc", "/etc/x"]);
        assert_eq!(levels(Path::new("/"), home).len(), 1);
    }
}
