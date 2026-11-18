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
        /// Roots that are views, not folders (e.g. "Frequent"): an icon
        /// name each; they never expand.
        pub special: RefCell<HashMap<NodeId, &'static str>>,
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

    /// Add a root that is a view, not a folder.
    pub fn add_special_root(&self, label: &str, icon: &'static str) -> NodeId {
        let id = self.add_root("", label);
        self.imp().special.borrow_mut().insert(id, icon);
        id
    }

    pub fn is_special(&self, node: NodeId) -> bool {
        self.imp().special.borrow().contains_key(&node)
    }

    pub fn add_root(&self, path: &str, label: &str) -> NodeId {
        let (id, s) = self.imp().tree.borrow_mut().add_root(path);
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
    icon: gtk::Image,
    label: gtk::Label,
}

fn row_widget() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let expander = gtk::Image::new();
    expander.set_pixel_size(16);
    expander.add_css_class("tudi-expander");
    let icon = gtk::Image::new();
    icon.set_pixel_size(16);
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_ellipsize(pango::EllipsizeMode::End);
    b.append(&expander);
    b.append(&icon);
    b.append(&label);
    b
}

fn parts(b: &gtk::Box) -> Parts {
    let expander = b.first_child().and_downcast::<gtk::Image>().unwrap();
    let icon = expander
        .next_sibling()
        .and_downcast::<gtk::Image>()
        .unwrap();
    let label = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
    Parts {
        expander,
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
    if let Some(icon) = model.imp().special.borrow().get(&node) {
        p.expander.clear();
        p.icon.set_icon_name(Some(icon));
        p.label.set_text(&model.label(node));
        return;
    }
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
    p.icon.set_from_gicon(&format::folder_icon());
    p.label.set_text(&model.label(node));
}

/// Build the tree `ListView`. `on_toggle(position)` is called when an
/// expander is clicked.
pub fn build_view(
    model: &TreeModel,
    selection: &gtk::SingleSelection,
    on_toggle: impl Fn(u32) + 'static,
) -> gtk::ListView {
    let factory = gtk::SignalListItemFactory::new();
    let on_toggle = std::rc::Rc::new(on_toggle);
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let row = row_widget();
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
