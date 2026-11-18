//! tudi's own file icons, drawn with GSK paths (no image files, so no
//! decoding at startup and crisp at every zoom level).
//!
//! The style is flat and colourful — light-blue folders, white pages with a
//! folded corner and a coloured band for the file type — an original design
//! in the spirit of classic desktop file managers. Sizes come from CSS
//! (`fileicon { min-width; min-height }`, scaled by the zoom level).

use std::cell::Cell;

use gtk::glib;
use gtk::gsk;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene};

/// What a file is, for its icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Folder,
    /// A document of this category.
    File(Category),
    Executable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Category {
    #[default]
    Plain,
    Text,
    Pdf,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Document,
    Spreadsheet,
    Presentation,
}

/// File category from the name's extension (lowercase compare).
pub fn category(name: &[u8]) -> Category {
    let ext = match name.iter().rposition(|&c| c == b'.') {
        Some(p) if p > 0 => name[p + 1..].to_ascii_lowercase(),
        _ => return Category::Plain,
    };
    match ext.as_slice() {
        b"txt" | b"md" | b"markdown" | b"rst" | b"log" | b"csv" | b"tsv" | b"org" | b"tex" => {
            Category::Text
        }
        b"pdf" | b"ps" | b"eps" | b"djvu" | b"epub" => Category::Pdf,
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"webp" | b"svg" | b"bmp" | b"tif" | b"tiff"
        | b"heic" | b"avif" | b"ico" | b"raw" | b"cr2" | b"nef" | b"psd" | b"xcf" => {
            Category::Image
        }
        b"mp4" | b"mkv" | b"mov" | b"avi" | b"webm" | b"m4v" | b"wmv" | b"flv" | b"mpg"
        | b"mpeg" | b"m2ts" => Category::Video,
        b"mp3" | b"flac" | b"wav" | b"ogg" | b"opus" | b"m4a" | b"aac" | b"wma" | b"aiff" => {
            Category::Audio
        }
        b"zip" | b"tar" | b"gz" | b"tgz" | b"bz2" | b"xz" | b"zst" | b"7z" | b"rar" | b"deb"
        | b"rpm" | b"iso" | b"dmg" | b"jar" | b"apk" => Category::Archive,
        b"rs" | b"py" | b"js" | b"ts" | b"jsx" | b"tsx" | b"c" | b"h" | b"cc" | b"cpp" | b"hpp"
        | b"go" | b"java" | b"kt" | b"rb" | b"php" | b"sh" | b"bash" | b"zsh" | b"fish"
        | b"lua" | b"pl" | b"swift" | b"cs" | b"html" | b"htm" | b"css" | b"scss" | b"json"
        | b"toml" | b"yaml" | b"yml" | b"xml" | b"sql" | b"vue" | b"ipynb" | b"r" | b"scala"
        | b"hs" | b"ml" | b"zig" | b"nix" | b"cmake" | b"mk" => Category::Code,
        b"doc" | b"docx" | b"odt" | b"rtf" | b"pages" | b"wps" => Category::Document,
        b"xls" | b"xlsx" | b"ods" | b"numbers" | b"et" => Category::Spreadsheet,
        b"ppt" | b"pptx" | b"odp" | b"key" | b"dps" => Category::Presentation,
        _ => Category::Plain,
    }
}

fn rgba(hex: u32, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
        alpha,
    )
}

/// The band colour of a document category (`None`: text lines instead).
fn band(c: Category) -> Option<u32> {
    Some(match c {
        Category::Plain | Category::Text => return None,
        Category::Pdf => 0xe5483e,
        Category::Image => 0x9b59d0,
        Category::Video => 0xe0457b,
        Category::Audio => 0xf08a24,
        Category::Archive => 0xa47449,
        Category::Code => 0x2aa198,
        Category::Document => 0x2f7de1,
        Category::Spreadsheet => 0x2e9e4f,
        Category::Presentation => 0xe8762c,
    })
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FileIcon {
        pub kind: Cell<Kind>,
        pub link: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FileIcon {
        const NAME: &'static str = "TudiFileIcon";
        type Type = super::FileIcon;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("fileicon");
        }
    }

    impl ObjectImpl for FileIcon {}

    impl WidgetImpl for FileIcon {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let w = self.obj();
            let (width, height) = (w.width() as f32, w.height() as f32);
            let size = width.min(height);
            if size <= 0.0 {
                return;
            }
            snapshot.save();
            // Centre a square of `size`; draw on a 16-unit grid.
            snapshot.translate(&graphene::Point::new(
                (width - size) / 2.0,
                (height - size) / 2.0,
            ));
            snapshot.scale(size / 16.0, size / 16.0);
            let dark = adw::StyleManager::default().is_dark();
            match self.kind.get() {
                Kind::Folder => draw_folder(snapshot),
                Kind::File(c) => draw_page(snapshot, dark, c, false),
                Kind::Executable => draw_page(snapshot, dark, Category::Plain, true),
            }
            if self.link.get() {
                draw_link_badge(snapshot);
            }
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct FileIcon(ObjectSubclass<imp::FileIcon>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for FileIcon {
    fn default() -> Self {
        Self::new(Kind::Folder)
    }
}

impl FileIcon {
    pub fn new(kind: Kind) -> Self {
        let w: Self = glib::Object::new();
        w.set_valign(gtk::Align::Center);
        w.imp().kind.set(kind);
        w
    }

    /// Change what the icon shows; redraws only if it changed.
    pub fn set(&self, kind: Kind, link: bool) {
        let imp = self.imp();
        if imp.kind.get() != kind || imp.link.get() != link {
            imp.kind.set(kind);
            imp.link.set(link);
            self.queue_draw();
        }
    }
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> gsk::Path {
    let b = gsk::PathBuilder::new();
    b.add_rounded_rect(&gsk::RoundedRect::from_rect(
        graphene::Rect::new(x, y, w, h),
        r,
    ));
    b.to_path()
}

fn draw_folder(s: &gtk::Snapshot) {
    // Back panel with the tab.
    let back = gsk::PathBuilder::new();
    back.move_to(1.5, 3.6);
    back.quad_to(1.5, 2.6, 2.5, 2.6);
    back.line_to(5.8, 2.6);
    back.quad_to(6.4, 2.6, 6.8, 3.1);
    back.line_to(7.5, 3.9);
    back.line_to(13.5, 3.9);
    back.quad_to(14.5, 3.9, 14.5, 4.9);
    back.line_to(14.5, 12.5);
    back.line_to(1.5, 12.5);
    back.close();
    s.append_fill(
        &back.to_path(),
        gsk::FillRule::Winding,
        &rgba(0x3d8fd6, 1.0),
    );
    // Front panel with a soft vertical gradient.
    let front = rounded(1.0, 5.4, 14.0, 8.6, 1.1);
    s.push_fill(&front, gsk::FillRule::Winding);
    s.append_linear_gradient(
        &graphene::Rect::new(1.0, 5.4, 14.0, 8.6),
        &graphene::Point::new(0.0, 5.4),
        &graphene::Point::new(0.0, 14.0),
        &[
            gsk::ColorStop::new(0.0, rgba(0x8ccaf7, 1.0)),
            gsk::ColorStop::new(1.0, rgba(0x5fb0ee, 1.0)),
        ],
    );
    s.pop();
    // Highlight along the top edge of the front panel.
    let hl = gsk::PathBuilder::new();
    hl.move_to(2.1, 5.95);
    hl.line_to(13.9, 5.95);
    s.append_stroke(&hl.to_path(), &gsk::Stroke::new(0.5), &rgba(0xffffff, 0.55));
}

fn draw_page(s: &gtk::Snapshot, dark: bool, cat: Category, exec: bool) {
    // A page with its top-right corner folded.
    let (l, t, r, b, fold) = (3.0, 1.0, 13.0, 15.0, 3.2);
    let page = gsk::PathBuilder::new();
    page.move_to(l + 0.8, t);
    page.line_to(r - fold, t);
    page.line_to(r, t + fold);
    page.line_to(r, b - 0.8);
    page.quad_to(r, b, r - 0.8, b);
    page.line_to(l + 0.8, b);
    page.quad_to(l, b, l, b - 0.8);
    page.line_to(l, t + 0.8);
    page.quad_to(l, t, l + 0.8, t);
    page.close();
    let page = page.to_path();
    let paper = if exec {
        rgba(0x3b4250, 1.0)
    } else if dark {
        rgba(0xe6e6e9, 1.0)
    } else {
        rgba(0xffffff, 1.0)
    };
    s.append_fill(&page, gsk::FillRule::Winding, &paper);
    s.append_stroke(
        &page,
        &gsk::Stroke::new(0.6),
        &rgba(0x000000, if exec { 0.55 } else { 0.32 }),
    );
    // The fold.
    let f = gsk::PathBuilder::new();
    f.move_to(r - fold, t);
    f.line_to(r - fold, t + fold - 0.6);
    f.quad_to(r - fold, t + fold, r - fold + 0.6, t + fold);
    f.line_to(r, t + fold);
    f.close();
    s.append_fill(
        &f.to_path(),
        gsk::FillRule::Winding,
        &rgba(if exec { 0x5a6272 } else { 0xd5d8de }, 1.0),
    );
    if exec {
        // A prompt: ">_".
        let p = gsk::PathBuilder::new();
        p.move_to(5.2, 7.6);
        p.line_to(7.0, 9.1);
        p.line_to(5.2, 10.6);
        p.move_to(7.8, 11.0);
        p.line_to(10.4, 11.0);
        let st = gsk::Stroke::new(0.9);
        st.set_line_cap(gsk::LineCap::Round);
        st.set_line_join(gsk::LineJoin::Round);
        s.append_stroke(&p.to_path(), &st, &rgba(0x7ee08a, 1.0));
        return;
    }
    match band(cat) {
        Some(color) => {
            // Coloured band in the lower part of the page.
            s.push_fill(&page, gsk::FillRule::Winding);
            s.append_color(&rgba(color, 1.0), &graphene::Rect::new(l, 9.6, r - l, 3.6));
            s.pop();
        }
        None => {
            let lines = gsk::PathBuilder::new();
            let n = if cat == Category::Text { 5 } else { 0 };
            for i in 0..n {
                let y = 5.4 + i as f32 * 1.8;
                lines.move_to(5.0, y);
                lines.line_to(if i == n - 1 { 9.0 } else { 11.0 }, y);
            }
            if n > 0 {
                s.append_stroke(
                    &lines.to_path(),
                    &gsk::Stroke::new(0.6),
                    &rgba(0x8a9099, 0.9),
                );
            }
        }
    }
}

fn draw_link_badge(s: &gtk::Snapshot) {
    // A small arrow in a white circle, bottom left.
    let c = gsk::PathBuilder::new();
    c.add_circle(&graphene::Point::new(4.2, 11.8), 3.0);
    s.append_fill(&c.to_path(), gsk::FillRule::Winding, &rgba(0xffffff, 1.0));
    s.append_stroke(&c.to_path(), &gsk::Stroke::new(0.5), &rgba(0x000000, 0.35));
    let a = gsk::PathBuilder::new();
    a.move_to(2.8, 13.2);
    a.line_to(5.4, 10.6);
    a.move_to(3.4, 10.6);
    a.line_to(5.4, 10.6);
    a.line_to(5.4, 12.6);
    let st = gsk::Stroke::new(0.8);
    st.set_line_cap(gsk::LineCap::Round);
    st.set_line_join(gsk::LineJoin::Round);
    s.append_stroke(&a.to_path(), &st, &rgba(0x1f1f1f, 1.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(category(b"report.PDF"), Category::Pdf);
        assert_eq!(category("合同.xlsx".as_bytes()), Category::Spreadsheet);
        assert_eq!(category(b"main.rs"), Category::Code);
        assert_eq!(category(b"notes.md"), Category::Text);
        assert_eq!(category(b".bashrc"), Category::Plain);
        assert_eq!(category(b"Makefile"), Category::Plain);
        assert_eq!(category(b"a.tar.gz"), Category::Archive);
    }
}
