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
//!   `selectall`, `menu ROW|none`. Prefix a command with `!` to run the next one after 50 ms.
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
                b.pane_sel.select_item(row, true);
            }
            "type" => {
                for c in arg.chars() {
                    b.type_ahead(c);
                }
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
                let w: gtk::Widget = b.pane_view.clone().upcast();
                b.file_menu(pos, &w, 300.0, 120.0);
            }
            "settings" => b.open_settings(),
            "settingspage" => {
                let s = b.settings.borrow().as_ref().and_then(|s| s.upgrade());
                if let Some(s) = s {
                    s.show_page(arg);
                }
            }
            "ctrld" => b.bookmark_or_launcher(),
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
                let adj = b.pane_scroll.vadjustment();
                adj.set_value(f * (adj.upper() - adj.page_size()));
            }
            "dump" => dump(&b),
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

/// Print the current folder, the rows of both panes, and the selection.
fn dump(b: &Browser) {
    println!("dir: {}", b.current_dir().display());
    let d = b.pane.data();
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
