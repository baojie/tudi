//! The optional preview pane (ADR-027): to the right of the list, it shows
//! the selected file of the active tab — an image, the start of a text
//! file, or the item's details (several selected: their count and size;
//! none: the shown folder).
//!
//! Files are read on a short-lived thread, never on the main thread; a
//! newer selection makes older results stale (a serial number). The pane
//! is refreshed from `update_status`, which runs on every selection change,
//! and does nothing while it already shows the same thing.

use std::cell::{Cell, RefCell};
use std::io::Read;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk_pixbuf, glib, pango};

use crate::browser::Browser;
use crate::i18n::{fill, ntr, tr};
use crate::icons::{Category, Kind};

/// Longest side of a previewed image, in pixels.
const IMAGE_PX: i32 = 1600;
/// Bytes of a text file shown.
const TEXT_BYTES: usize = 64 * 1024;

pub struct Preview {
    pub widget: gtk::Box,
    stack: gtk::Stack,
    picture: gtk::Picture,
    text: gtk::TextView,
    icon: crate::icons::FileIcon,
    title: gtk::Label,
    details: gtk::Label,
    /// What is shown (path and modification time, or a summary), so a
    /// status update that changes nothing does not reload.
    key: RefCell<String>,
    serial: Cell<u64>,
}

/// What the loader thread found.
enum Content {
    Image(crate::thumbs::Pixels),
    Text(String),
    Nothing,
}

impl Preview {
    pub fn new() -> Self {
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.set_vexpand(true);
        let text = gtk::TextView::new();
        text.set_editable(false);
        text.set_cursor_visible(false);
        text.set_monospace(true);
        text.set_wrap_mode(gtk::WrapMode::WordChar);
        text.set_left_margin(8);
        text.set_right_margin(8);
        text.set_top_margin(6);
        let text_scroll = gtk::ScrolledWindow::builder()
            .child(&text)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let icon = crate::icons::FileIcon::default();
        icon.set_size_request(128, 128);
        icon.set_halign(gtk::Align::Center);
        icon.set_valign(gtk::Align::End);
        icon.set_vexpand(true);
        let stack = gtk::Stack::new();
        stack.add_named(&picture, Some("image"));
        stack.add_named(&text_scroll, Some("text"));
        stack.add_named(&icon, Some("icon"));
        stack.set_vexpand(true);
        let title = gtk::Label::new(None);
        title.set_wrap(true);
        title.set_wrap_mode(pango::WrapMode::WordChar);
        title.set_justify(gtk::Justification::Center);
        title.add_css_class("heading");
        let details = gtk::Label::new(None);
        details.set_wrap(true);
        details.set_justify(gtk::Justification::Center);
        details.add_css_class("dim-label");
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 6);
        widget.add_css_class("tudi-preview");
        widget.set_size_request(200, -1);
        widget.append(&stack);
        widget.append(&title);
        widget.append(&details);
        Self {
            widget,
            stack,
            picture,
            text,
            icon,
            title,
            details,
            key: RefCell::default(),
            serial: Cell::new(0),
        }
    }

    fn show_icon(&self, kind: Kind) {
        self.icon.set(kind, false);
        self.stack.set_visible_child_name("icon");
    }
}

/// How to preview a file of this name, by extension.
fn is_text(name: &[u8]) -> bool {
    matches!(
        crate::icons::category(name),
        Category::Text | Category::Code | Category::Plain
    )
}

/// Read the start of a text file; `None` for binary content.
fn read_text(path: &Path) -> Option<String> {
    let mut buf = Vec::with_capacity(TEXT_BYTES);
    std::fs::File::open(path)
        .ok()?
        .take(TEXT_BYTES as u64)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn load(path: &Path, image: bool) -> Content {
    if image {
        let p = gdk_pixbuf::Pixbuf::from_file_at_scale(path, IMAGE_PX, IMAGE_PX, true)
            .ok()
            .and_then(|p| p.apply_embedded_orientation());
        return p
            .and_then(|p| crate::thumbs::Pixels::from_pixbuf(&p))
            .map_or(Content::Nothing, Content::Image);
    }
    read_text(path).map_or(Content::Nothing, Content::Text)
}

impl Browser {
    /// `Alt+P` / the header button: show or hide the preview pane.
    pub(crate) fn toggle_preview(self: &Rc<Self>) {
        let on = !self
            .preview_paned
            .end_child()
            .is_some_and(|c| c.is_visible());
        self.set_preview(on);
    }

    pub(crate) fn set_preview(self: &Rc<Self>, on: bool) {
        self.preview.widget.set_visible(on);
        if self.preview_toggle.is_active() != on {
            self.preview_toggle.set_active(on);
        }
        self.preview.key.replace(String::new());
        if on {
            self.update_preview();
        }
    }

    pub(crate) fn preview_shown(&self) -> bool {
        self.preview.widget.is_visible()
    }

    /// Show the active tab's selection in the preview pane (if shown).
    pub(crate) fn update_preview(&self) {
        if !self.preview_shown() {
            return;
        }
        let Some(me) = self.me.upgrade() else { return };
        let tab = self.active.borrow().clone();
        let selected = me.with_tab(&tab, |b| b.selected_entries());
        let cur = tab.current.borrow().clone();
        let pv = &self.preview;
        let d = tab.pane.data();
        if selected.len() != 1 {
            // A summary: the shown folder, or the selection's size.
            let key = format!("{}|{}|{}", cur.display(), selected.len(), d.generation);
            if *pv.key.borrow() == key {
                return;
            }
            pv.key.replace(key);
            pv.serial.set(pv.serial.get() + 1);
            if selected.is_empty() {
                pv.show_icon(Kind::Folder);
                pv.title.set_text(&crate::tabs::title_of(&cur));
                pv.details.set_text(&fill(
                    &ntr("{} item", "{} items", d.order.len() as u64),
                    &[&d.order.len()],
                ));
            } else {
                let n = selected.len() as u64;
                let total: u64 = selected
                    .iter()
                    .filter(|&&e| !d.is_dir(e))
                    .map(|&e| d.metas.get(e as usize).size)
                    .sum();
                pv.show_icon(Kind::File(Category::Plain));
                pv.title.set_text(&fill(
                    &ntr("{} item selected", "{} items selected", n),
                    &[&n],
                ));
                pv.details.set_text(&crate::format::size(total));
            }
            return;
        }
        let e = selected[0];
        let name = d.name(e).to_vec();
        let dir = d.is_dir(e);
        let m = *d.metas.get(e as usize);
        drop(d);
        use std::os::unix::ffi::OsStrExt;
        let path = cur.join(std::ffi::OsStr::from_bytes(&name));
        let key = format!("{}|{}|{}", path.display(), m.mtime, m.size);
        if *pv.key.borrow() == key {
            return;
        }
        pv.key.replace(key);
        let serial = pv.serial.get() + 1;
        pv.serial.set(serial);
        pv.title.set_text(&crate::format::name(&name));
        let mut details = Vec::new();
        if m.status == tudi_core::listing::meta::MetaStatus::Ok {
            if !dir {
                details.push(crate::format::size(m.size));
            }
            if let Ok(now) = glib::DateTime::now_local() {
                details.push(crate::format::date(m.mtime, &now));
            }
        }
        pv.details.set_text(&details.join(" · "));
        let kind = if dir {
            Kind::Folder
        } else {
            Kind::File(crate::icons::category(&name))
        };
        let image = !dir && crate::thumbs::is_image(&name);
        let text = !dir && !image && is_text(&name) && m.size > 0;
        pv.show_icon(kind);
        if !image && !text {
            return;
        }
        let (tx, rx) = async_channel::bounded(1);
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send_blocking(load(&p, image));
        });
        let w = self.me.clone();
        glib::spawn_future_local(async move {
            let Ok(content) = rx.recv().await else { return };
            let Some(b) = w.upgrade() else { return };
            let pv = &b.preview;
            if pv.serial.get() != serial {
                return; // another item was selected meanwhile
            }
            match content {
                Content::Image(px) => {
                    pv.picture.set_paintable(Some(&px.texture()));
                    pv.stack.set_visible_child_name("image");
                }
                Content::Text(t) => {
                    pv.text.buffer().set_text(&t);
                    pv.stack.set_visible_child_name("text");
                }
                Content::Nothing => {}
            }
        });
    }

    /// The pane and its toggle, around the list area.
    pub(crate) fn build_preview(
        stage: &gtk::Widget,
        width: i32,
    ) -> (gtk::Paned, gtk::ToggleButton) {
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(stage)
            .wide_handle(true)
            .shrink_start_child(true)
            .resize_end_child(false)
            .shrink_end_child(false)
            .vexpand(true)
            .build();
        let toggle = gtk::ToggleButton::builder()
            .icon_name("view-dual-symbolic")
            .tooltip_text(tr("Preview (Alt+P)"))
            .build();
        // The position is set from the right once the paned has a width.
        let w = if width > 0 { width.max(200) } else { 320 };
        paned.add_tick_callback(move |p, _| {
            if p.width() > 0 {
                p.set_position((p.width() - w).max(200));
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        (paned, toggle)
    }

    /// Width of the preview pane, for the window state (0: hidden).
    pub(crate) fn preview_width(&self) -> i32 {
        if self.preview_shown() {
            self.preview.widget.width()
        } else {
            0
        }
    }
}
