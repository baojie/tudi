//! Properties dialog for files and folders (context menu, `Alt+Enter`).
//!
//! Basic facts (type, dates, permissions, owner) are read on a thread; the
//! size of folders is counted recursively on that thread too, updating the
//! dialog about every 100 ms with a spinner while it runs (like a desktop
//! file manager's properties window). Closing the dialog stops the count.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gtk::glib;
use tudi_core::listing::meta::{SizeStats, walk_size};

use crate::browser::Browser;
use crate::i18n::{fill, ntr, tr};

/// Facts about one item, read on the worker thread.
struct Facts {
    name: String,
    location: String,
    is_dir: bool,
    link_target: Option<String>,
    size: u64,
    mtime: i64,
    atime: i64,
    mode: u32,
    owner: String,
    group: String,
    category: crate::icons::Kind,
}

enum Msg {
    Facts(Vec<Facts>),
    Progress(SizeStats),
    Done(SizeStats),
}

/// Name for a uid / gid from /etc/passwd or /etc/group (no NSS lookups).
fn id_name(file: &str, id: u32) -> String {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let mut f = l.split(':');
                let name = f.next()?;
                let _pw = f.next()?;
                (f.next()?.parse::<u32>().ok()? == id).then(|| name.to_string())
            })
        })
        .unwrap_or_else(|| id.to_string())
}

fn facts(p: &Path) -> Option<Facts> {
    let lmeta = std::fs::symlink_metadata(p).ok()?;
    let link_target = lmeta
        .file_type()
        .is_symlink()
        .then(|| std::fs::read_link(p).ok())
        .flatten()
        .map(|t| t.to_string_lossy().into_owned());
    // Describe what a link points to, if it resolves.
    let meta = std::fs::metadata(p).unwrap_or(lmeta);
    let name = p
        .file_name()
        .map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned();
    let is_dir = meta.is_dir();
    let category = if is_dir {
        crate::icons::Kind::Folder
    } else if meta.permissions().mode() & 0o111 != 0 && !name.contains('.') {
        crate::icons::Kind::Executable
    } else {
        crate::icons::Kind::File(crate::icons::category(name.as_bytes()))
    };
    Some(Facts {
        location: p
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default(),
        name,
        is_dir,
        link_target,
        size: meta.len(),
        mtime: meta.mtime(),
        atime: meta.atime(),
        mode: meta.mode(),
        owner: id_name("/etc/passwd", meta.uid()),
        group: id_name("/etc/group", meta.gid()),
        category,
    })
}

/// `rwxr-x---` style permissions, with the octal value.
fn perms(mode: u32) -> String {
    let mut s = String::new();
    for shift in [6, 3, 0] {
        let b = (mode >> shift) & 7;
        s.push(if b & 4 != 0 { 'r' } else { '-' });
        s.push(if b & 2 != 0 { 'w' } else { '-' });
        s.push(if b & 1 != 0 { 'x' } else { '-' });
    }
    format!("{s}  ({:o})", mode & 0o7777)
}

fn date(secs: i64) -> String {
    glib::DateTime::from_unix_local(secs)
        .ok()
        .and_then(|d| d.format("%Y-%m-%d %H:%M:%S").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn kind_name(f: &Facts) -> String {
    use crate::icons::{Category as C, Kind as K};
    let base = match f.category {
        K::Folder => tr("Folder"),
        K::Executable => tr("Program"),
        K::File(c) => match c {
            C::Plain => tr("File"),
            C::Text => tr("Text"),
            C::Pdf => tr("PDF document"),
            C::Image => tr("Image"),
            C::Video => tr("Video"),
            C::Audio => tr("Audio"),
            C::Archive => tr("Archive"),
            C::Code => tr("Source code"),
            C::Document => tr("Document"),
            C::Spreadsheet => tr("Spreadsheet"),
            C::Presentation => tr("Presentation"),
        },
    };
    if f.link_target.is_some() {
        fill(&tr("Link to {}"), &[&base.to_lowercase()])
    } else {
        base
    }
}

fn value_row(title: &str) -> (adw::ActionRow, gtk::Label) {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_css_class("property");
    let v = gtk::Label::new(None);
    v.set_selectable(true);
    v.set_wrap(true);
    v.set_xalign(1.0);
    v.set_hexpand(true);
    v.set_justify(gtk::Justification::Right);
    row.add_suffix(&v);
    (row, v)
}

fn size_text(s: &SizeStats) -> String {
    format!(
        "{}  ({} {})",
        crate::format::size(s.bytes),
        crate::browser::fmt_count(s.bytes),
        tr("bytes")
    )
}

fn contains_text(s: &SizeStats) -> String {
    let mut t = fill(
        &ntr("{} file", "{} files", s.files),
        &[&crate::browser::fmt_count(s.files)],
    );
    t.push_str(", ");
    t.push_str(&fill(
        &ntr("{} folder", "{} folders", s.dirs),
        &[&crate::browser::fmt_count(s.dirs)],
    ));
    if s.errors > 0 {
        t.push('\n');
        t.push_str(&fill(
            &ntr(
                "{} item could not be read",
                "{} items could not be read",
                s.errors,
            ),
            &[&s.errors],
        ));
    }
    t
}

/// Show the properties of `paths` (one item, or a summary of several).
pub fn show(b: &Rc<Browser>, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let dialog = adw::Dialog::builder()
        .title(tr("Properties"))
        .content_width(460)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let page = adw::PreferencesPage::new();

    // Header: big icon and name.
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    head.set_margin_top(6);
    let icon = crate::icons::FileIcon::new(crate::icons::Kind::Folder);
    icon.set_size_request(48, 48);
    icon.add_css_class("tudi-big-icon");
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    titles.set_valign(gtk::Align::Center);
    let name = gtk::Label::new(None);
    name.set_xalign(0.0);
    name.set_selectable(true);
    name.set_wrap(true);
    name.add_css_class("title-3");
    let kind = gtk::Label::new(None);
    kind.set_xalign(0.0);
    kind.add_css_class("dim-label");
    titles.append(&name);
    titles.append(&kind);
    head.append(&icon);
    head.append(&titles);
    let head_group = adw::PreferencesGroup::new();
    head_group.add(&head);
    page.add(&head_group);

    let main = adw::PreferencesGroup::new();
    let (loc_row, loc) = value_row(&tr("Location"));
    let (link_row, link) = value_row(&tr("Link target"));
    let (size_row, size) = value_row(&tr("Size"));
    let spinner = gtk::Spinner::new();
    spinner.set_valign(gtk::Align::Center);
    size_row.add_suffix(&spinner);
    let (contains_row, contains) = value_row(&tr("Contains"));
    for r in [&loc_row, &link_row, &size_row, &contains_row] {
        main.add(r);
    }
    link_row.set_visible(false);
    contains_row.set_visible(false);
    page.add(&main);

    let more = adw::PreferencesGroup::new();
    let (mtime_row, mtime) = value_row(&tr("Modified"));
    let (atime_row, atime) = value_row(&tr("Accessed"));
    let (perm_row, perm) = value_row(&tr("Permissions"));
    let (owner_row, owner) = value_row(&tr("Owner"));
    for r in [&mtime_row, &atime_row, &perm_row, &owner_row] {
        more.add(r);
    }
    page.add(&more);
    toolbar.set_content(Some(&page));
    dialog.set_child(Some(&toolbar));

    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    dialog.connect_closed(move |_| c.store(true, Ordering::Relaxed));
    dialog.present(Some(&b.window));
    // Do not start with a label focused (it would select its text).
    dialog.set_focus(None::<&gtk::Widget>);

    let (tx, rx) = async_channel::unbounded::<Msg>();
    let several = paths.len() > 1;
    std::thread::spawn(move || {
        let all: Vec<Facts> = paths.iter().filter_map(|p| facts(p)).collect();
        let dirs: Vec<PathBuf> = paths
            .iter()
            .zip(&all)
            .filter(|(_, f)| f.is_dir && f.link_target.is_none())
            .map(|(p, _)| p.clone())
            .collect();
        // Files count directly; folders are walked.
        let mut base = SizeStats::default();
        for f in all.iter().filter(|f| !f.is_dir || f.link_target.is_some()) {
            base.files += 1;
            base.bytes += f.size;
        }
        let _ = tx.send_blocking(Msg::Facts(all));
        // A single folder's "contains" excludes itself; a selection counts
        // the selected folders.
        let own = u64::from(several);
        let mut total = base;
        for d in dirs {
            let before = total;
            let r = walk_size(&d, None, &cancel, |s| {
                let mut cur = before;
                cur.bytes += s.bytes;
                cur.files += s.files;
                cur.dirs += s.dirs + own;
                cur.errors += s.errors;
                let _ = tx.send_blocking(Msg::Progress(cur));
            });
            match r {
                Some(s) => {
                    total.bytes += s.bytes;
                    total.files += s.files;
                    total.dirs += s.dirs + own;
                    total.errors += s.errors;
                }
                None => return, // cancelled
            }
        }
        let _ = tx.send_blocking(Msg::Done(total));
    });

    glib::spawn_future_local(async move {
        while let Ok(m) = rx.recv().await {
            match m {
                Msg::Facts(all) => {
                    let Some(first) = all.first() else {
                        name.set_text(&tr("Not found"));
                        return;
                    };
                    if several {
                        let n = all.len() as u64;
                        name.set_text(&fill(&ntr("{} item", "{} items", n), &[&n]));
                        kind.set_text(&tr("Selection"));
                        icon.set(
                            crate::icons::Kind::File(crate::icons::Category::Plain),
                            false,
                        );
                        loc.set_text(&first.location);
                        for r in [&mtime_row, &atime_row, &perm_row, &owner_row] {
                            r.set_visible(false);
                        }
                    } else {
                        name.set_text(&first.name);
                        kind.set_text(&kind_name(first));
                        icon.set(first.category, first.link_target.is_some());
                        loc.set_text(&first.location);
                        if let Some(t) = &first.link_target {
                            link.set_text(t);
                            link_row.set_visible(true);
                        }
                        mtime.set_text(&date(first.mtime));
                        atime.set_text(&date(first.atime));
                        perm.set_text(&perms(first.mode));
                        owner.set_text(&format!("{} : {}", first.owner, first.group));
                    }
                    let any_dir = all.iter().any(|f| f.is_dir && f.link_target.is_none());
                    if any_dir {
                        spinner.start();
                        contains_row.set_visible(true);
                        size.set_text(&tr("Counting…"));
                    } else {
                        let total: u64 = all.iter().map(|f| f.size).sum();
                        size.set_text(&size_text(&SizeStats {
                            bytes: total,
                            ..Default::default()
                        }));
                    }
                }
                Msg::Progress(s) => {
                    size.set_text(&size_text(&s));
                    contains.set_text(&contains_text(&s));
                }
                Msg::Done(s) => {
                    spinner.stop();
                    spinner.set_visible(false);
                    if contains_row.is_visible() {
                        size.set_text(&size_text(&s));
                        contains.set_text(&contains_text(&s));
                    }
                }
            }
        }
    });
}
