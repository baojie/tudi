//! M1 spike: does GTK4 meet P1 (startup), P5 (scroll) and P6 (expand) with a
//! custom `ListModel` over 100k rows? Throwaway code; see docs/PLAN.md §7 M1.
//!
//! ```text
//! gtk4-list-spike [--variant listview|columnview] [--rows N]
//!                 [--bench startup|reopen|all] [--speed PX_PER_S]
//!                 [--inserts N] [--insert-rows N]
//! ```
//!
//! With `--bench`, results are printed to stdout as one JSON object and the
//! app quits.

mod bench;
mod model;
mod view;

use std::sync::OnceLock;
use std::time::Instant;

use adw::prelude::*;
use gtk::gio;

pub static START: OnceLock<Instant> = OnceLock::new();

thread_local! {
    /// Named startup checkpoints (milliseconds since `START`).
    pub static MARKS: std::cell::RefCell<Vec<(&'static str, f64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub fn mark(name: &'static str) {
    let t = START
        .get()
        .map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0);
    MARKS.with(|m| m.borrow_mut().push((name, t)));
}

#[derive(Clone, Debug)]
pub struct Options {
    pub variant: view::Variant,
    pub rows: usize,
    pub bench: Option<bench::Mode>,
    pub speed: f64,
    pub inserts: usize,
    pub insert_rows: usize,
}

fn parse_args() -> Result<Options, String> {
    let mut o = Options {
        variant: view::Variant::ListView,
        rows: 100_000,
        bench: None,
        speed: 50_000.0,
        inserts: 5,
        insert_rows: 10_000,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--variant" => {
                o.variant = match val()?.as_str() {
                    "listview" => view::Variant::ListView,
                    "columnview" => view::Variant::ColumnView,
                    v => return Err(format!("unknown variant {v}")),
                }
            }
            "--icons" => view::ICONS.with(|i| {
                i.set(match args.next().as_deref() {
                    Some("themed") => view::Icons::Themed,
                    Some("symbolic") => view::Icons::Symbolic,
                    Some("none") => view::Icons::None,
                    v => return Err(format!("--icons: bad value {v:?}")),
                });
                Ok(())
            })?,
            "--rows" => o.rows = val()?.parse().map_err(|e| format!("--rows: {e}"))?,
            "--speed" => o.speed = val()?.parse().map_err(|e| format!("--speed: {e}"))?,
            "--inserts" => o.inserts = val()?.parse().map_err(|e| format!("--inserts: {e}"))?,
            "--insert-rows" => {
                o.insert_rows = val()?.parse().map_err(|e| format!("--insert-rows: {e}"))?
            }
            "--bench" => {
                o.bench = Some(match val()?.as_str() {
                    "startup" => bench::Mode::Startup,
                    "reopen" => bench::Mode::Reopen,
                    "all" => bench::Mode::All,
                    v => return Err(format!("unknown bench mode {v}")),
                })
            }
            _ => return Err(format!("unknown argument {a}")),
        }
    }
    Ok(o)
}

fn main() -> gtk::glib::ExitCode {
    START.get_or_init(Instant::now);
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return gtk::glib::ExitCode::FAILURE;
        }
    };

    mark("main");
    let app = adw::Application::builder()
        .application_id("io.github.baojie.tudi.Spike")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|_| mark("startup_signal"));
    app.connect_activate(move |app| {
        mark("activate");
        let model = model::RowModel::synthetic(opts.rows);
        mark("model_built");
        build_window(app, &opts, &model, opts.bench);
    });
    // Our own arguments are parsed above; do not let GApplication see them.
    app.run_with_args::<&str>(&[])
}

/// Build and present a window over `model`. Also used by `--bench reopen`
/// to open a second window from the still-running process.
pub fn build_window(
    app: &adw::Application,
    opts: &Options,
    model: &model::RowModel,
    bench: Option<bench::Mode>,
) {
    let list = view::build(model, opts.variant);
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&adw::HeaderBar::new());
    content.append(&scrolled);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(format!("tudi spike — {}", opts.variant.name()))
        .default_width(1200)
        .default_height(800)
        .content(&content)
        .build();

    if let Some(mode) = bench {
        bench::install(app, &window, &scrolled, model, opts, mode);
    }
    mark("window_built");
    window.present();
    mark("present_returned");
}
