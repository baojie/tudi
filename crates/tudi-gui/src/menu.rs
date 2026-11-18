//! Context menus: a `GtkPopoverMenu` at the pointer that always shows all
//! its items.
//!
//! GtkPopoverMenu wraps each page in a scrolled window. When the popup gets
//! less height than its content (seen on multi-monitor GNOME setups even
//! with room below), the menu scrolls and hides its last items. Turning
//! the vertical scrollbar off makes the full content the menu's minimum
//! height, so it is never cut; and a menu that does not fit below the
//! pointer inside the window opens upwards.

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

/// Make every scrolled window inside `w` grow to its content height.
/// Returns the tallest content height found.
fn no_scrolling(w: &gtk::Widget) -> i32 {
    let mut tallest = 0;
    if let Some(sw) = w.downcast_ref::<gtk::ScrolledWindow>() {
        sw.set_vscrollbar_policy(gtk::PolicyType::Never);
        sw.set_propagate_natural_height(true);
        tallest = sw.measure(gtk::Orientation::Vertical, -1).1;
    }
    let mut c = w.first_child();
    while let Some(child) = c {
        tallest = tallest.max(no_scrolling(&child));
        c = child.next_sibling();
    }
    tallest
}

/// Show `menu` at (`x`, `y`) in `widget`'s coordinates. The popover is
/// unparented when closed.
pub fn popup_at(widget: &impl IsA<gtk::Widget>, menu: &gio::MenuModel, x: f64, y: f64) {
    let pop = gtk::PopoverMenu::from_model(Some(menu));
    pop.set_parent(widget);
    pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    pop.set_has_arrow(false);
    pop.set_halign(gtk::Align::Start);
    // Menu margins and padding on top of the items.
    let natural = no_scrolling(pop.upcast_ref()) + 24;
    if let Some(root) = widget.as_ref().root()
        && let Some(p) = widget
            .as_ref()
            .compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
    {
        let (py, h) = (f64::from(p.y()), f64::from(root.height()));
        let n = f64::from(natural);
        if py + n > h && py - n >= 0.0 {
            pop.set_position(gtk::PositionType::Top);
        }
        tracing::debug!("menu: natural height {natural}, pointer y {py:.0} of {h:.0}");
    }
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}
