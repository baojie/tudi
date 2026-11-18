//! The folder tree (left pane): a `gio::ListModel` over the rows of
//! [`tudi_core::tree::Tree`], forwarding its splices as `items-changed`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib, pango};
use tudi_core::tree::{Children, NodeId, RowKind, Splice, Tree};

use crate::format;

mod row_imp {
    use super::*;

    #[derive(Default)]
    pub struct TreeRow {
        pub node: Cell<u32>,
        pub error: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TreeRow {
        const NAME: &'static str = "TudiTreeRow";
        type Type = super::TreeRow;
    }

    impl ObjectImpl for TreeRow {}
}

glib::wrapper! {
    pub struct TreeRow(ObjectSubclass<row_imp::TreeRow>);
}

impl TreeRow {
    fn new(node: NodeId, error: bool) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().node.set(node);
        obj.imp().error.set(error);
        obj
    }

    pub fn node(&self) -> NodeId {
        self.imp().node.get()
    }

    pub fn is_error(&self) -> bool {
        self.imp().error.get()
    }
}

type Key = (NodeId, bool);

mod model_imp {
    use super::*;

    #[derive(Default)]
    pub struct TreeModel {
        pub tree: RefCell<Tree>,
        /// Display names for roots, by node.
        pub root_labels: RefCell<HashMap<NodeId, String>>,
        pub bound: RefCell<HashMap<Key, glib::WeakRef<gtk::Box>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TreeModel {
        const NAME: &'static str = "TudiTreeModel";
        type Type = super::TreeModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for TreeModel {}

    impl ListModelImpl for TreeModel {
        fn item_type(&self) -> glib::Type {
            TreeRow::static_type()
        }

        fn n_items(&self) -> u32 {
            self.tree.borrow().rows().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let r = *self.tree.borrow().rows().get(position as usize)?;
            Some(TreeRow::new(r.node, r.kind == RowKind::Error).upcast())
        }
    }
}

glib::wrapper! {
    pub struct TreeModel(ObjectSubclass<model_imp::TreeModel>)
        @implements gio::ListModel;
}

impl Default for TreeModel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl TreeModel {
    pub fn tree(&self) -> std::cell::Ref<'_, Tree> {
        self.imp().tree.borrow()
    }

    /// Mutate the tree; the returned splice (if any) is emitted, and the
    /// rows of `touched` nodes are refreshed (expander state).
    pub fn update<R>(
        &self,
        touched: Option<NodeId>,
        f: impl FnOnce(&mut Tree) -> (R, Option<Splice>),
    ) -> R {
        let (r, splice) = f(&mut self.imp().tree.borrow_mut());
        if let Some(s) = splice {
            self.items_changed(s.position, s.removed, s.added);
        }
        if let Some(node) = touched {
            self.refresh(node);
        }
        r
    }

    pub fn add_root(&self, path: &str, label: &str) -> NodeId {
        let (id, s) = self.imp().tree.borrow_mut().add_root(path);
        // Record the label before telling GTK: it may bind the row at once.
        self.imp().root_labels.borrow_mut().insert(id, label.into());
        self.items_changed(s.position, s.removed, s.added);
        id
    }

    fn refresh(&self, node: NodeId) {
        let row = self
            .imp()
            .bound
            .borrow()
            .get(&(node, false))
            .and_then(|w| w.upgrade());
        if let Some(row) = row {
            fill(&row, self, node, false);
        }
    }

    fn label(&self, node: NodeId) -> String {
        if let Some(l) = self.imp().root_labels.borrow().get(&node) {
            return l.clone();
        }
        let tree = self.tree();
        format::name(tree.node(node).name().as_encoded_bytes())
    }
}

struct Parts {
    expander: gtk::Image,
    folder: crate::icons::FileIcon,
    /// Shown instead of the folder on error rows.
    icon: gtk::Image,
    label: gtk::Label,
}

fn row_widget() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let expander = gtk::Image::new();
    expander.add_css_class("tudi-icon");
    expander.add_css_class("tudi-expander");
    let folder = crate::icons::FileIcon::default();
    let icon = gtk::Image::new();
    icon.add_css_class("tudi-icon");
    icon.set_visible(false);
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(pango::EllipsizeMode::End);
    b.append(&expander);
    b.append(&folder);
    b.append(&icon);
    b.append(&label);
    b
}

fn parts(b: &gtk::Box) -> Parts {
    let expander = b.first_child().and_downcast::<gtk::Image>().unwrap();
    let folder = expander
        .next_sibling()
        .and_downcast::<crate::icons::FileIcon>()
        .unwrap();
    let icon = folder.next_sibling().and_downcast::<gtk::Image>().unwrap();
    let label = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
    Parts {
        expander,
        folder,
        icon,
        label,
    }
}

fn fill(b: &gtk::Box, model: &TreeModel, node: NodeId, error: bool) {
    let p = parts(b);
    let (depth, expanded, children) = {
        let tree = model.tree();
        let n = tree.node(node);
        (n.depth(), n.is_expanded(), n.children().clone())
    };
    if error {
        b.set_margin_start(i32::from(depth + 1) * 16);
        p.expander.clear();
        p.folder.set_visible(false);
        p.icon.set_visible(true);
        p.icon.set_icon_name(Some("dialog-warning-symbolic"));
        let msg = match &children {
            Children::Failed(f) => f.message.clone(),
            _ => String::new(),
        };
        p.label.set_text(&msg);
        p.label.add_css_class("dim-label");
        return;
    }
    b.set_margin_start(i32::from(depth) * 16);
    p.label.remove_css_class("dim-label");
    let leaf = matches!(&children, Children::Loaded(k) if k.is_empty());
    if leaf {
        p.expander.clear();
    } else {
        p.expander.set_icon_name(Some(if expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
    }
    p.icon.set_visible(false);
    p.folder.set_visible(true);
    p.label.set_text(&model.label(node));
}

/// Build the tree `ListView`. `on_toggle(position)` is called when an
/// expander is clicked.
/// `on_toggle(position)`: an expander was clicked. `on_menu(position, row,
/// x, y)`: a row was right-clicked.
pub fn build_view(
    model: &TreeModel,
    selection: &gtk::SingleSelection,
    on_toggle: impl Fn(u32) + 'static,
    on_menu: impl Fn(u32, &gtk::Widget, f64, f64) + 'static,
) -> gtk::ListView {
    let factory = gtk::SignalListItemFactory::new();
    let on_toggle = std::rc::Rc::new(on_toggle);
    let on_menu = std::rc::Rc::new(on_menu);
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = row_widget();
        let rclick = gtk::GestureClick::new();
        rclick.set_button(gtk::gdk::BUTTON_SECONDARY);
        let (f, wi, wr) = (on_menu.clone(), item.downgrade(), row.downgrade());
        rclick.connect_pressed(move |g, _, x, y| {
            if let (Some(item), Some(row)) = (wi.upgrade(), wr.upgrade()) {
                g.set_state(gtk::EventSequenceState::Claimed);
                f(item.position(), row.upcast_ref(), x, y);
            }
        });
        row.add_controller(rclick);
        let click = gtk::GestureClick::new();
        let (t, weak_item) = (on_toggle.clone(), item.downgrade());
        click.connect_pressed(move |g, _, _, _| {
            if let Some(item) = weak_item.upgrade() {
                g.set_state(gtk::EventSequenceState::Claimed);
                t(item.position());
            }
        });
        parts(&row).expander.add_controller(click);
        item.set_child(Some(&row));
    });
    let m = model.clone();
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let obj = item.item().and_downcast::<TreeRow>().unwrap();
        fill(&row, &m, obj.node(), obj.is_error());
        m.imp()
            .bound
            .borrow_mut()
            .insert((obj.node(), obj.is_error()), row.downgrade());
    });
    let m = model.clone();
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(obj) = item.item().and_downcast::<TreeRow>() else {
            return;
        };
        let row = item.child().and_downcast::<gtk::Box>().unwrap();
        let key = (obj.node(), obj.is_error());
        let mut bound = m.imp().bound.borrow_mut();
        if bound.get(&key).and_then(|w| w.upgrade()).as_ref() == Some(&row) {
            bound.remove(&key);
        }
    });
    let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
    view.add_css_class("tudi-tree");
    view
}
