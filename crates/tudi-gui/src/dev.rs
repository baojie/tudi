//! Development and test aids, enabled by environment variables only:
//!
//! - `TUDI_DEV_SCREENSHOT=FILE`: render the window to a PNG after it
//!   settles, then quit.
//! - `TUDI_DEV_SCRIPT="cmd; cmd; …"`: run commands one after another (each
//!   after the previous one settled), printing results to stdout. Used for
//!   scripted GUI checks. Commands:
//!   `wait MS`, `nav PATH`, `up`, `back`, `forward`, `select ROW`,
//!   `type TEXT`, `hidden`, `sort name|size|mtime`, `rename NAME`,
//!   `mkdir NAME`, `trash`, `expand ROW`, `collapse ROW`, `scroll FRACTION`,
//!   `dump`, `open PATH` (counted visit), `treeselect ROW`, `bookmark`,
//!   `settings`, bar actions `bm N`, `freq N`, `pin N`, `barexclude N`,
//!   `barexclude-tree N`, `forget N`, `rmbm N`, `copy`, `cut`, `paste`,
//!   `selectall`, `menu ROW|none`, find bar `find TEXT`, `findnext`,
//!   `findprev`, `findenter`, `findclose`, `winwidth PX`, `newfile NAME`,
//!   `drop ROW|- PATHS [ctrl|shift]`, `droptree ROW PATHS [ctrl|shift]`. Prefix a command with `!` to run the next one after 50 ms.
//!   `shot FILE`, `quit`.

// Script results go to stdout on purpose: they are read by test tooling.
#![allow(clippy::print_stdout)]

use std::cell::RefCell;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::browser::Browser;

/// Pause between script commands, enough for listings and GIO to finish.
const STEP: Duration = Duration::from_millis(400);

pub fn install(b: &Rc<Browser>) {
    if let Some(out) = std::env::var_os("TUDI_DEV_SCREENSHOT") {
        let b = b.clone();
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            screenshot(&b, Path::new(&out));
            b.window.application().unwrap().quit();
        });
    }
    if let Ok(script) = std::env::var("TUDI_DEV_SCRIPT") {
        let cmds: Vec<String> = script
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let queue = Rc::new(RefCell::new(cmds.into_iter()));
        step(b.clone(), queue, Duration::from_millis(1000));
    }
}

type Queue = Rc<RefCell<std::vec::IntoIter<String>>>;

fn step(b: Rc<Browser>, queue: Queue, delay: Duration) {
    glib::timeout_add_local_once(delay, move || {
        let Some(cmd) = queue.borrow_mut().next() else {
            return;
        };
        let t = crate::bench::START
            .get()
            .map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0);
        println!("> [{t:.0}] {cmd}");
        let (verb, arg) = cmd.split_once(' ').unwrap_or((&cmd, ""));
        let next = STEP;
        let (verb, arg, next) = match verb.strip_prefix('!') {
            // `!cmd`: run the next command after 50 ms (keyboard-speed).
            Some(v) => (v, arg, Duration::from_millis(50)),
            None => (verb, arg, next),
        };
        let mut next = next;
        match verb {
            "wait" => next = Duration::from_millis(arg.parse().unwrap_or(500)),
            "nav" => b.navigate(&PathBuf::from(arg), true),
            "up" => b.go_up(),
            "back" => b.go_back(),
            "forward" => b.go_forward(),
            "select" => {
                let row: u32 = arg.parse().unwrap_or(0);
                b.tab().pane_sel.select_item(row, true);
            }
            "type" => {
                for c in arg.chars() {
                    b.type_ahead(c);
                }
            }
            "find" => {
                b.open_find();
                b.tab().find.entry.set_text(arg);
            }
            "findnext" => b.find_jump(false, true),
            "findprev" => b.find_jump(false, false),
            "findenter" => b.apply_find_filter(),
            "findclose" => b.close_find(true),
            "winwidth" => {
                let w: i32 = arg.parse().unwrap_or(800);
                b.window.unmaximize();
                b.window.set_default_size(w, b.window.default_height());
            }
            "hidden" => b.toggle_hidden(),
            "sort" => b.click_sort(match arg {
                "size" => 1,
                "mtime" => 2,
                _ => 0,
            }),
            "rename" => {
                if let Some(p) = b.selected_paths().first().cloned() {
                    b.rename(p, arg.to_string());
                }
            }
            "mkdir" => b.make_folder(arg.to_string()),
            "newfile" => b.make_file_in(&b.current_dir(), arg.to_string()),
            "drop" | "droptree" => {
                // `drop ROW|- PATH,PATH [ctrl|shift]`: as if files were dropped
                // on a list row (`-`: empty space) or, with `droptree`, a tree row.
                let mut it = arg.split_whitespace();
                let row = it.next().and_then(|r| r.parse::<u32>().ok());
                let files: Vec<PathBuf> = it
                    .next()
                    .unwrap_or("")
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .collect();
                let mods = match it.next() {
                    Some("ctrl") => gtk::gdk::ModifierType::CONTROL_MASK,
                    Some("shift") => gtk::gdk::ModifierType::SHIFT_MASK,
                    _ => gtk::gdk::ModifierType::empty(),
                };
                let ok = if verb == "droptree" {
                    b.drop_on_tree(row.unwrap_or(0), files, mods)
                } else {
                    b.drop_on_pane(row, files, mods)
                };
                println!("drop accepted: {ok}");
            }
            "trash" => b.trash_selected(),
            "expand" => b.expand_tree_row(arg.parse().unwrap_or(0)),
            "collapse" => b.collapse_tree_row(arg.parse().unwrap_or(0)),
            "open" => {
                let p = PathBuf::from(arg);
                b.navigate(&p, true);
                b.record_visit(&p);
            }
            "treeselect" => b.tree_sel.set_selected(arg.parse().unwrap_or(0)),
            "panewidth" => b.dev_set_tree_width(arg.parse().unwrap_or(280)),
            "zoom" => b.show_zoom(crate::zoom::set(arg.parse().unwrap_or(100))),
            "bookmark" => b.bookmark_current(),
            "copy" => b.copy_files(),
            "cut" => b.cut_files(),
            "paste" => b.paste(),
            "selectall" => b.select_all(),
            "menu" => {
                let pos = arg.parse::<u32>().ok();
                let w: gtk::Widget = b.tab().pane_view.clone().upcast();
                b.file_menu(pos, &w, 300.0, 120.0);
            }
            "menuat" => {
                // Dev only: the background menu at a height in the list.
                let y = arg.parse::<f64>().unwrap_or(120.0);
                let w: gtk::Widget = b.tab().pane_view.clone().upcast();
                b.file_menu(None, &w, 300.0, y);
            }
            "settings" => b.open_settings(),
            "settingspage" => {
                let s = b.settings.borrow().as_ref().and_then(|s| s.upgrade());
                if let Some(s) = s {
                    s.show_page(arg);
                }
            }
            "ctrld" => b.bookmark_or_launcher(),
            "search" => {
                b.open_search();
                b.search.entry.set_text(arg);
            }
            "searchhere" => {
                b.open_search_here();
                b.search.entry.set_text(arg);
            }
            "props" => crate::properties::show(&b, vec![PathBuf::from(arg)]),
            "treemenupath" => {
                // Dev only: find the visible row showing this path.
                let target = PathBuf::from(arg);
                let pos = {
                    let t = b.tree.tree();
                    t.rows().iter().position(|r| {
                        r.kind == tudi_core::tree::RowKind::Node && t.path(r.node) == target
                    })
                };
                match pos {
                    Some(pos) => {
                        let w: gtk::Widget = b.tree_view.clone().upcast();
                        b.tree_menu(pos, &w, 100.0, 100.0);
                    }
                    None => println!("treemenupath: {arg} not visible"),
                }
            }
            "treeaction" => {
                let _ = WidgetExt::activate_action(&b.window, &format!("tree.{arg}"), None);
            }
            "treeactivate" => {
                let pos: u32 = arg.parse().unwrap_or(0);
                b.tree_view.emit_by_name::<()>("activate", &[&pos]);
            }
            "bm" | "freq" | "pin" | "barexclude" | "barexclude-tree" | "forget" | "rmbm"
            | "runl" | "lbg" | "lterm" => {
                let action = match verb {
                    "runl" => "run-launcher",
                    "lbg" => "launcher-background",
                    "lterm" => "launcher-terminal",
                    "bm" => "open-bookmark",
                    "freq" => "open-frequent",
                    "pin" => "pin",
                    "barexclude" => "exclude",
                    "barexclude-tree" => "exclude-tree",
                    "forget" => "forget",
                    _ => "remove-bookmark",
                };
                let i: i32 = arg.parse().unwrap_or(0);
                let _ = WidgetExt::activate_action(
                    &b.window,
                    &format!("bar.{action}"),
                    Some(&i.to_variant()),
                );
            }
            "scroll" => {
                let f: f64 = arg.parse().unwrap_or(0.0);
                let adj = b.tab().pane_scroll.vadjustment();
                adj.set_value(f * (adj.upper() - adj.page_size()));
            }
            "dump" => dump(&b),
            "newtab" => {
                b.new_tab(Path::new(arg), true);
            }
            "bgtab" => b.open_in_new_tab(Path::new(arg)),
            "tab" => {
                let t = b.tabs.borrow().get(arg.parse().unwrap_or(0)).cloned();
                if let Some(t) = t {
                    b.activate_tab(&t);
                }
            }
            "closetab" => b.close_active_tab(),
            "thumbs" => b.set_thumbnails(arg != "off"),
            "preview" => b.set_preview(arg != "off"),
            "tile" => b.toggle_tile(),
            "shot" => screenshot(&b, Path::new(arg)),
            "quit" => {
                b.window.application().unwrap().quit();
                return;
            }
            _ => println!("unknown command {verb}"),
        }
        step(b, queue, next);
    });
}

/// Print the current folder, the rows of both panes, the selection, the
/// status bar, the find bar, the focused widget and the list's scroll
/// position.
fn dump(b: &Browser) {
    println!("dir: {}", b.current_dir().display());
    let tab = b.tab();
    let d = tab.pane.data();
    let names: Vec<String> = d
        .order
        .iter()
        .take(40)
        .map(|&e| String::from_utf8_lossy(d.name(e)).into_owned())
        .collect();
    println!("list ({}): {}", d.order.len(), names.join(" | "));
    drop(d);
    let sel: Vec<String> = b
        .selected_paths()
        .iter()
        .map(|p| String::from_utf8_lossy(p.file_name().unwrap().as_bytes()).into_owned())
        .collect();
    println!("selected: {}", sel.join(" | "));
    println!("status: {}", b.status.text());
    println!(
        "find: shown={} text={:?} error={} filter={:?}",
        b.tab().find.revealer.reveals_child(),
        b.tab().find.entry.text(),
        b.tab().find.entry.has_css_class("error"),
        b.tab().find.filter.borrow().as_deref()
    );
    println!("focus: {}", focus_desc(b));
    println!("scroll: {:.0}", b.tab().pane_scroll.vadjustment().value());
    let t = b.tree.tree();
    let rows: Vec<String> = t
        .rows()
        .iter()
        .take(60)
        .map(|r| {
            format!(
                "{}{}",
                "  ".repeat(r.depth as usize),
                t.node(r.node).name().to_string_lossy()
            )
        })
        .collect();
    println!("tree ({}):\n{}", t.rows().len(), rows.join("\n"));
    let sel = b.tree_sel.selected();
    if sel != gtk::INVALID_LIST_POSITION {
        println!(
            "tree selected: {}",
            t.path(t.rows()[sel as usize].node).display()
        );
    }
    println!("bar: {}", b.dev_bar_labels().join(" | "));
    let tabs: Vec<String> = b
        .tabs
        .borrow()
        .iter()
        .map(|t| {
            let mark = if b.is_active(t) { "*" } else { "" };
            let side = if t.side.get() == 1 { ">" } else { "" };
            format!("{side}{mark}{}", t.label.text())
        })
        .collect();
    println!("tabs: {}", tabs.join(" | "));
    println!("window: {}x{}", b.window.width(), b.window.height());
    // Column header: each button's x range in window coordinates.
    let cols: Vec<String> = b
        .tab()
        .sort_buttons
        .iter()
        .filter_map(|btn| btn.compute_bounds(&b.window))
        .map(|r| {
            format!(
                "{:.0}-{:.0}@{:.0}",
                r.x(),
                r.x() + r.width(),
                r.y() + r.height() / 2.0
            )
        })
        .collect();
    println!("columns: {}", cols.join(" "));
    let mut c = b.tab().page.first_child();
    let mut parts = Vec::new();
    while let Some(w) = c {
        if let Some(r) = w.compute_bounds(&b.window) {
            parts.push(format!(
                "{}@{:.0}+{:.0}",
                w.type_().name(),
                r.y(),
                r.height()
            ));
        }
        c = w.next_sibling();
    }
    println!("page: {}", parts.join(" "));
    let n = b.search.model.n_items();
    for i in 0..n.min(5) {
        if let Some(r) = b.search.model.get(i) {
            println!("result: {}", r.path().display());
        }
    }
    if b.search.container.is_visible() {
        println!("search status: {}", b.search.status.text());
    }
    println!("status: {}", b.status.text());
    let f = crate::bench::frame_stats();
    println!(
        "frames: {} long(>16ms): {} max: {:.1} ms",
        f.frames, f.long_frames, f.max_ms
    );
}

fn screenshot(b: &Browser, out: &Path) {
    let w = &b.window;
    let paintable = gtk::WidgetPaintable::new(Some(w));
    let snap = gtk::Snapshot::new();
    paintable.snapshot(&snap, w.width() as f64, w.height() as f64);
    if let (Some(node), Some(renderer)) = (snap.to_node(), w.renderer())
        && let Err(e) = renderer.render_texture(&node, None).save_to_png(out)
    {
        tracing::error!("screenshot: {e}");
    }
}

/// The focused widget's type, and the first label text inside it (for a
/// list row: its name column).
fn focus_desc(b: &Browser) -> String {
    let Some(w) = gtk::prelude::GtkWindowExt::focus(&b.window) else {
        return "none".into();
    };
    fn first_label(w: &gtk::Widget) -> Option<String> {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            return Some(l.text().to_string());
        }
        let mut c = w.first_child();
        while let Some(child) = c {
            if let Some(t) = first_label(&child) {
                return Some(t);
            }
            c = child.next_sibling();
        }
        None
    }
    format!("{} {:?}", w.type_().name(), first_label(&w))
}
