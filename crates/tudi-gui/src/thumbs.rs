//! Image thumbnails for the thumbnail view (ADR-026).
//!
//! Two `tudi-thumb-*` threads load thumbnails: the freedesktop cache
//! (`~/.cache/thumbnails/large/<md5 of the URI>.png`, written by GNOME
//! Files and others) when present, else the image itself decoded at a
//! reduced size by gdk-pixbuf. Requests are served newest first (what was
//! just scrolled into view) and deduplicated. The main thread only turns
//! the pixels into a texture and keeps a bounded cache; it never touches
//! the file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use gtk::prelude::*;
use gtk::{gdk, gdk_pixbuf, glib};

/// Longest side of a loaded thumbnail, in pixels (the cells show 128
/// logical pixels; 256 stays sharp at scale 2).
pub const THUMB_PX: i32 = 256;
/// Textures kept in memory per window.
const CACHE_LIMIT: usize = 3000;
/// Pending requests kept (older ones are dropped: scrolled past).
const QUEUE_LIMIT: usize = 400;

/// Decoded pixels sent to the main thread.
pub struct Pixels {
    bytes: glib::Bytes,
    width: i32,
    height: i32,
    stride: usize,
    alpha: bool,
}

/// A loaded thumbnail, or `None` when the file could not be decoded.
pub struct Loaded {
    pub path: PathBuf,
    pub pixels: Option<Pixels>,
}

#[derive(Default)]
struct Queue {
    /// Newest last.
    stack: Vec<PathBuf>,
    queued: HashSet<PathBuf>,
}

/// Handle to the thumbnail threads of one window.
pub struct Thumbnailer {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    /// Loaded textures (`None`: not an image we can show).
    cache: std::cell::RefCell<HashMap<PathBuf, Option<gdk::Texture>>>,
    /// Requested and not answered yet.
    asked: std::cell::RefCell<HashSet<PathBuf>>,
}

impl Thumbnailer {
    /// Start the threads; loaded thumbnails arrive on the returned channel.
    pub fn start() -> (Self, async_channel::Receiver<Loaded>) {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let (tx, rx) = async_channel::unbounded();
        for i in 0..2 {
            let (q, tx) = (queue.clone(), tx.clone());
            std::thread::Builder::new()
                .name(format!("tudi-thumb-{i}"))
                .spawn(move || run(&q, &tx))
                .expect("spawn thumbnail thread");
        }
        (
            Self {
                queue,
                cache: Default::default(),
                asked: Default::default(),
            },
            rx,
        )
    }

    /// The texture for `path` if loaded; otherwise ask for it (once).
    /// `Some(None)`: known not to be loadable.
    pub fn get(&self, path: &Path) -> Option<Option<gdk::Texture>> {
        if let Some(t) = self.cache.borrow().get(path) {
            return Some(t.clone());
        }
        if self.asked.borrow_mut().insert(path.to_path_buf()) {
            let (lock, cv) = &*self.queue;
            let mut q = lock.lock().unwrap();
            if q.queued.insert(path.to_path_buf()) {
                q.stack.push(path.to_path_buf());
                if q.stack.len() > QUEUE_LIMIT {
                    let old = q.stack.remove(0);
                    q.queued.remove(&old);
                    self.asked.borrow_mut().remove(&old);
                }
            }
            cv.notify_one();
        }
        None
    }

    /// A thumbnail arrived: keep its texture.
    pub fn store(&self, loaded: Loaded) -> Option<gdk::Texture> {
        self.asked.borrow_mut().remove(&loaded.path);
        let texture = loaded.pixels.map(|p| {
            let format = if p.alpha {
                gdk::MemoryFormat::R8g8b8a8
            } else {
                gdk::MemoryFormat::R8g8b8
            };
            gdk::MemoryTexture::new(p.width, p.height, format, &p.bytes, p.stride).upcast()
        });
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(loaded.path, texture.clone());
        texture
    }

    /// Forget a file's texture (it changed on disk).
    pub fn forget_dir(&self, dir: &Path) {
        self.cache
            .borrow_mut()
            .retain(|p, _| p.parent() != Some(dir));
    }
}

impl Drop for Thumbnailer {
    fn drop(&mut self) {
        // Wake the threads with an empty queue and no receiver: they exit
        // when sending fails.
        let (lock, cv) = &*self.queue;
        lock.lock().unwrap().stack.clear();
        cv.notify_all();
    }
}

fn run(queue: &(Mutex<Queue>, Condvar), tx: &async_channel::Sender<Loaded>) {
    let (lock, cv) = queue;
    loop {
        let path = {
            let mut q = lock.lock().unwrap();
            loop {
                if tx.is_closed() {
                    return;
                }
                if let Some(p) = q.stack.pop() {
                    q.queued.remove(&p);
                    break p;
                }
                q = cv
                    .wait_timeout(q, std::time::Duration::from_secs(5))
                    .unwrap()
                    .0;
            }
        };
        let pixels = load(&path);
        if tx.send_blocking(Loaded { path, pixels }).is_err() {
            return;
        }
    }
}

/// The freedesktop cache entry for `path` (large size).
fn cached_thumbnail(path: &Path) -> Option<PathBuf> {
    let uri = gtk::gio::File::for_path(path).uri();
    let md5 = glib::compute_checksum_for_string(glib::ChecksumType::Md5, &uri)?;
    let p = glib::user_cache_dir()
        .join("thumbnails/large")
        .join(format!("{md5}.png"));
    p.is_file().then_some(p)
}

fn load(path: &Path) -> Option<Pixels> {
    let from_cache = cached_thumbnail(path).and_then(|c| {
        // A cache entry older than the file is stale.
        let newer = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        match (newer(&c), newer(path)) {
            (Some(t), Some(f)) if t >= f => Some(c),
            _ => None,
        }
    });
    let pixbuf = match from_cache {
        Some(c) => gdk_pixbuf::Pixbuf::from_file(&c).ok(),
        None => gdk_pixbuf::Pixbuf::from_file_at_scale(path, THUMB_PX, THUMB_PX, true)
            .ok()
            .and_then(|p| p.apply_embedded_orientation()),
    }?;
    // Keep 8-bit RGB(A), the formats a memory texture takes directly.
    if pixbuf.bits_per_sample() != 8 || !(3..=4).contains(&pixbuf.n_channels()) {
        return None;
    }
    Some(Pixels {
        bytes: pixbuf.read_pixel_bytes(),
        width: pixbuf.width(),
        height: pixbuf.height(),
        stride: pixbuf.rowstride() as usize,
        alpha: pixbuf.has_alpha(),
    })
}

/// Image extensions (as in `icons::category`).
const IMAGE_EXTS: &[&[u8]] = &[
    b"png", b"jpg", b"jpeg", b"gif", b"webp", b"svg", b"bmp", b"tif", b"tiff", b"heic", b"avif",
    b"ico", b"raw", b"cr2", b"nef", b"psd", b"xcf",
];

/// Whether a name is an image (by extension). Called for every entry on
/// the listing worker, so it does not allocate.
pub fn is_image(name: &[u8]) -> bool {
    match name.iter().rposition(|&c| c == b'.') {
        Some(p) if p > 0 => {
            let ext = &name[p + 1..];
            IMAGE_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext))
        }
        _ => false,
    }
}

/// Grid cells currently bound, by position (refilled when a thumbnail
/// arrives).
pub type GridBound = std::rc::Rc<std::cell::RefCell<HashMap<u32, glib::WeakRef<gtk::Box>>>>;

/// Marks a grid cell (to tell cells from the grid's empty space).
const CELL_CLASS: &str = "tudi-cell";

fn cell_widget() -> gtk::Box {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
    cell.add_css_class(CELL_CLASS);
    let icon = crate::icons::FileIcon::default();
    icon.set_size_request(96, 96);
    icon.set_halign(gtk::Align::Center);
    let picture = gtk::Picture::new();
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_can_shrink(true);
    picture.set_size_request(128, 128);
    picture.set_visible(false);
    let label = gtk::Label::new(None);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    label.set_max_width_chars(16);
    label.set_width_chars(16);
    label.set_justify(gtk::Justification::Center);
    cell.append(&icon);
    cell.append(&picture);
    cell.append(&label);
    cell
}

/// Show entry `position` in a grid cell: a thumbnail for images (asked for
/// if not loaded yet), else the file's icon.
pub fn fill_cell(
    cell: &gtk::Box,
    model: &crate::pane::PaneModel,
    position: u32,
    dir: &Path,
    thumbs: &Thumbnailer,
) {
    use std::os::unix::ffi::OsStrExt;
    let Some(icon) = cell.first_child().and_downcast::<crate::icons::FileIcon>() else {
        return;
    };
    let Some(picture) = icon.next_sibling().and_downcast::<gtk::Picture>() else {
        return;
    };
    let Some(label) = picture.next_sibling().and_downcast::<gtk::Label>() else {
        return;
    };
    let d = model.data();
    let Some(&entry) = d.order.get(position as usize) else {
        return;
    };
    let name = d.name(entry);
    let is_dir = d.is_dir(entry);
    label.set_text(&crate::format::name(name));
    let texture = (!is_dir && is_image(name))
        .then(|| thumbs.get(&dir.join(std::ffi::OsStr::from_bytes(name))))
        .flatten()
        .flatten();
    use crate::icons::Kind as Icon;
    icon.set(
        if is_dir {
            Icon::Folder
        } else {
            Icon::File(crate::icons::category(name))
        },
        false,
    );
    match texture {
        Some(t) => {
            picture.set_paintable(Some(&t));
            picture.set_visible(true);
            icon.set_visible(false);
        }
        None => {
            picture.set_paintable(gdk::Paintable::NONE);
            picture.set_visible(false);
            icon.set_visible(true);
        }
    }
}

/// Build a tab's thumbnail grid for the same model and selection as its
/// list (attached when shown). `dir` gives the shown folder.
pub fn build_grid(
    model: &crate::pane::PaneModel,
    selection: &gtk::MultiSelection,
    thumbs: &std::rc::Rc<Thumbnailer>,
    dir: std::rc::Rc<dyn Fn() -> PathBuf>,
    bound: &GridBound,
    on_menu: crate::pane::MenuFn,
) -> gtk::GridView {
    let factory = gtk::SignalListItemFactory::new();
    let menu = on_menu.clone();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let cell = cell_widget();
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        let (f, wi, wc) = (menu.clone(), item.downgrade(), cell.downgrade());
        click.connect_pressed(move |g, _, x, y| {
            if let (Some(item), Some(cell)) = (wi.upgrade(), wc.upgrade()) {
                g.set_state(gtk::EventSequenceState::Claimed);
                f(Some(item.position()), cell.upcast_ref(), x, y);
            }
        });
        cell.add_controller(click);
        item.set_child(Some(&cell));
    });
    let (m, t, d, b) = (model.clone(), thumbs.clone(), dir.clone(), bound.clone());
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let cell = item.child().and_downcast::<gtk::Box>().unwrap();
        let pos = item.position();
        fill_cell(&cell, &m, pos, &d(), &t);
        b.borrow_mut().insert(pos, cell.downgrade());
    });
    let b = bound.clone();
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let Some(cell) = item.child().and_downcast::<gtk::Box>() {
            b.borrow_mut()
                .retain(|_, w| w.upgrade().is_some_and(|c| c != cell));
        }
    });
    // The model is attached only while the grid is shown (see
    // `Browser::set_thumbnails`): a hidden view would still process every
    // change of a 100k-row model.
    let _ = selection;
    let grid = gtk::GridView::new(None::<gtk::MultiSelection>, Some(factory));
    grid.set_max_columns(64);
    grid.add_css_class("tudi-grid");
    // Right-click outside the cells (capture: GridView's own gestures take
    // the press otherwise; presses in a cell go to the cell's gesture).
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(move |g, _, x, y| {
        let Some(w) = g.widget() else { return };
        let on_cell = std::iter::successors(w.pick(x, y, gtk::PickFlags::DEFAULT), |c| c.parent())
            .take_while(|c| c != &w)
            .any(|c| c.has_css_class(CELL_CLASS));
        if !on_cell {
            g.set_state(gtk::EventSequenceState::Claimed);
            on_menu(None, &w, x, y);
        }
    });
    grid.add_controller(click);
    grid
}

#[cfg(test)]
mod tests {
    #[test]
    fn images_by_extension() {
        use super::is_image;
        for n in ["a.jpg", "B.JPEG", "x.y.png", "p.Webp"] {
            assert!(is_image(n.as_bytes()), "{n}");
            assert_eq!(
                crate::icons::category(n.as_bytes()),
                crate::icons::Category::Image
            );
        }
        for n in ["a.txt", ".png", "jpg", "a.jpgx"] {
            assert!(!is_image(n.as_bytes()), "{n}");
        }
    }
}
