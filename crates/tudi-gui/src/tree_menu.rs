//! Context menu on the folder tree: acts on the right-clicked folder (not
//! on the folder shown in the list).

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use tudi_core::tree::RowKind;

use crate::browser::Browser;
use crate::i18n::{fill, tr};

/// The folder a tree menu was opened on.
#[derive(Clone, Debug)]
pub struct Target {
    pub path: PathBuf,
    pub row: usize,
    /// Home / File System: not renamed, moved or trashed.
    pub root: bool,
}

type TreeAction = fn(&Rc<Browser>, Target);

impl Browser {
    /// Actions behind the tree menu ("tree.*").
    pub(crate) fn install_tree_actions(self: &Rc<Self>) {
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, f: TreeAction| {
            let a = gio::SimpleAction::new(name, None);
            let w = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                let Some(b) = w.upgrade() else { return };
                let t = b.tree_target.borrow().clone();
                if let Some(t) = t {
                    f(&b, t);
                }
            });
            group.add_action(&a);
        };
        add("open", |b, t| {
            b.navigate(&t.path, true);
            b.record_visit(&t.path);
        });
        add("open-tab", |b, t| b.open_in_new_tab(&t.path));
        add("toggle", |b, t| b.toggle_tree_row(t.row));
        add("terminal", |b, t| {
            let tmpl = b.shared.config.borrow().commands.terminal.clone();
            b.run_command_in(&tmpl, t.path.clone(), &[]);
        });
        add("copy-name", |b, t| {
            let name = t
                .path
                .file_name()
                .map_or_else(|| t.path.to_string_lossy(), |n| n.to_string_lossy());
            b.window.clipboard().set_text(&name);
            b.toast(&tr("Copied the name"));
        });
        add("copy-path", |b, t| {
            b.window.clipboard().set_text(&t.path.to_string_lossy());
            b.toast(&tr("Copied the path"));
        });
        add("copy", |b, t| {
            if !t.root {
                b.put_on_clipboard(vec![t.path], false);
            }
        });
        add("cut", |b, t| {
            if !t.root {
                b.put_on_clipboard(vec![t.path], true);
            }
        });
        add("paste", |b, t| b.paste_into(t.path));
        add("new-folder", |b, t| b.new_folder_in(t.path));
        add("rename", |b, t| {
            if t.root {
                return;
            }
            let old = t
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let w = Rc::downgrade(b);
            b.ask_name(&tr("Rename"), &tr("Rename"), &old, move |new| {
                if let Some(b) = w.upgrade() {
                    b.rename(t.path.clone(), new);
                }
            });
        });
        add("bookmark", |b, t| {
            if b.shared.bookmarks.borrow_mut().add(&t.path) {
                b.toast(&fill(
                    &tr("Bookmarked {}"),
                    &[&t.path.file_name().unwrap_or_default().to_string_lossy()],
                ));
                b.shared.bookmarks_changed();
            } else {
                b.toast(&tr("Already bookmarked"));
            }
        });
        add("properties", |b, t| {
            crate::properties::show(b, vec![t.path])
        });
        add("trash", |b, t| {
            if !t.root {
                b.trash_paths(vec![t.path]);
            }
        });
        self.window.insert_action_group("tree", Some(&group));
    }

    /// Right-click on tree row `pos`.
    pub(crate) fn tree_menu(self: &Rc<Self>, pos: usize, widget: &gtk::Widget, x: f64, y: f64) {
        let (path, expanded, root) = {
            let tree = self.tree.tree();
            let Some(r) = tree.rows().get(pos).copied() else {
                return;
            };
            if r.kind == RowKind::Error {
                return;
            }
            let n = tree.node(r.node);
            (tree.path(r.node), n.is_expanded(), n.parent().is_none())
        };
        *self.tree_target.borrow_mut() = Some(Target {
            path,
            row: pos,
            root,
        });
        let item = |label: String, action: &str| gio::MenuItem::new(Some(&label), Some(action));
        let menu = gio::Menu::new();
        let open = gio::Menu::new();
        open.append_item(&item(tr("Open"), "tree.open"));
        open.append_item(&item(tr("Open in New Tab"), "tree.open-tab"));
        open.append_item(&item(
            if expanded {
                tr("Collapse")
            } else {
                tr("Expand")
            },
            "tree.toggle",
        ));
        open.append_item(&item(tr("Open Terminal Here"), "tree.terminal"));
        menu.append_section(None, &open);
        let edit = gio::Menu::new();
        if !root {
            edit.append_item(&item(tr("Cut"), "tree.cut"));
            edit.append_item(&item(tr("Copy"), "tree.copy"));
        }
        edit.append_item(&item(tr("Paste Into Folder"), "tree.paste"));
        edit.append_item(&item(tr("Copy Name"), "tree.copy-name"));
        edit.append_item(&item(tr("Copy Full Path"), "tree.copy-path"));
        menu.append_section(None, &edit);
        let manage = gio::Menu::new();
        manage.append_item(&item(tr("New Folder…"), "tree.new-folder"));
        manage.append_item(&item(tr("Add to Bookmarks"), "tree.bookmark"));
        if !root {
            manage.append_item(&item(tr("Rename…"), "tree.rename"));
            manage.append_item(&item(tr("Move to Trash"), "tree.trash"));
        }
        menu.append_section(None, &manage);
        let info = gio::Menu::new();
        info.append_item(&item(tr("Properties"), "tree.properties"));
        menu.append_section(None, &info);
        crate::menu::popup_at(widget, menu.upcast_ref(), x, y);
    }
}
