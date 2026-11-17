//! The two view variants under test: `ListView` with fixed-width labels, and
//! `ColumnView` with three columns.

use gtk::glib;
use gtk::pango;
use gtk::prelude::*;

use crate::model::{Node, RowModel, RowObject};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icons {
    Themed,
    Symbolic,
    None,
}

thread_local! {
    pub static ICONS: std::cell::Cell<Icons> = const { std::cell::Cell::new(Icons::Themed) };
}

fn icons() -> Icons {
    ICONS.with(|i| i.get())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    ListView,
    ColumnView,
}

impl Variant {
    pub fn name(self) -> &'static str {
        match self {
            Variant::ListView => "listview",
            Variant::ColumnView => "columnview",
        }
    }
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn human_age(secs: u32) -> String {
    match secs {
        0..60 => "now".into(),
        60..3_600 => format!("{} min", secs / 60),
        3_600..86_400 => format!("{} hours", secs / 3_600),
        86_400..2_592_000 => format!("{} days", secs / 86_400),
        2_592_000..31_536_000 => format!("{} months", secs / 2_592_000),
        _ => format!("{} years", secs / 31_536_000),
    }
}

fn set_name_cell(cell: &gtk::Box, node: &Node, indent: bool) {
    if indent {
        cell.set_margin_start(i32::from(node.depth) * 16);
    }
    let expander = cell.first_child().and_downcast::<gtk::Image>().unwrap();
    let icon = expander
        .next_sibling()
        .and_downcast::<gtk::Image>()
        .unwrap();
    let label = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
    if node.is_dir {
        expander.set_icon_name(Some("pan-down-symbolic"));
    } else {
        expander.clear();
    }
    match icons() {
        Icons::Themed => icon.set_icon_name(Some(if node.is_dir {
            "folder"
        } else {
            "text-x-generic"
        })),
        Icons::Symbolic => icon.set_icon_name(Some(if node.is_dir {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        })),
        Icons::None => {}
    }
    label.set_text(&node.name);
}

fn name_cell() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let expander = gtk::Image::new();
    expander.set_pixel_size(16);
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

fn fixed_label(chars: i32) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_xalign(1.0);
    l.set_width_chars(chars);
    l.set_max_width_chars(chars);
    l.add_css_class("dim-label");
    l.add_css_class("numeric");
    l
}

fn node_of(item: &glib::Object) -> u32 {
    let li = item.downcast_ref::<gtk::ListItem>().unwrap();
    li.item().and_downcast::<RowObject>().unwrap().node()
}

fn child_of<T: IsA<gtk::Widget>>(item: &glib::Object) -> T {
    let li = item.downcast_ref::<gtk::ListItem>().unwrap();
    li.child().and_downcast::<T>().unwrap()
}

fn set_child(item: &glib::Object, w: &impl IsA<gtk::Widget>) {
    item.downcast_ref::<gtk::ListItem>()
        .unwrap()
        .set_child(Some(w));
}

/// Build the scrollable list for `variant`.
pub fn build(model: &RowModel, variant: Variant) -> gtk::Widget {
    let selection = gtk::MultiSelection::new(Some(model.clone()));
    match variant {
        Variant::ListView => {
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(|_, item| {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                row.append(&name_cell());
                row.append(&fixed_label(9));
                row.append(&fixed_label(10));
                set_child(item, &row);
            });
            let m = model.clone();
            factory.connect_bind(move |_, item| {
                let row: gtk::Box = child_of(item);
                let d = m.data();
                let node = &d.nodes[node_of(item) as usize];
                let name = row.first_child().and_downcast::<gtk::Box>().unwrap();
                set_name_cell(&name, node, true);
                let size = name.next_sibling().and_downcast::<gtk::Label>().unwrap();
                let date = size.next_sibling().and_downcast::<gtk::Label>().unwrap();
                size.set_text(&if node.is_dir {
                    "—".into()
                } else {
                    human_size(node.size)
                });
                date.set_text(&human_age(node.age));
            });
            let view = gtk::ListView::new(Some(selection), Some(factory));
            view.add_css_class("rich-list");
            view.upcast()
        }
        Variant::ColumnView => {
            let view = gtk::ColumnView::new(Some(selection));

            let f_name = gtk::SignalListItemFactory::new();
            f_name.connect_setup(|_, item| set_child(item, &name_cell()));
            let m = model.clone();
            f_name.connect_bind(move |_, item| {
                let cell: gtk::Box = child_of(item);
                let d = m.data();
                set_name_cell(&cell, &d.nodes[node_of(item) as usize], true);
            });
            let c_name = gtk::ColumnViewColumn::new(Some("Name"), Some(f_name));
            c_name.set_expand(true);
            view.append_column(&c_name);

            let f_size = gtk::SignalListItemFactory::new();
            f_size.connect_setup(|_, item| set_child(item, &fixed_label(9)));
            let m = model.clone();
            f_size.connect_bind(move |_, item| {
                let l: gtk::Label = child_of(item);
                let d = m.data();
                let node = &d.nodes[node_of(item) as usize];
                l.set_text(&if node.is_dir {
                    "—".into()
                } else {
                    human_size(node.size)
                });
            });
            let c_size = gtk::ColumnViewColumn::new(Some("Size"), Some(f_size));
            c_size.set_fixed_width(110);
            view.append_column(&c_size);

            let f_date = gtk::SignalListItemFactory::new();
            f_date.connect_setup(|_, item| set_child(item, &fixed_label(10)));
            let m = model.clone();
            f_date.connect_bind(move |_, item| {
                let l: gtk::Label = child_of(item);
                let d = m.data();
                l.set_text(&human_age(d.nodes[node_of(item) as usize].age));
            });
            let c_date = gtk::ColumnViewColumn::new(Some("Modified"), Some(f_date));
            c_date.set_fixed_width(120);
            view.append_column(&c_date);

            view.upcast()
        }
    }
}
