//! `tudi`: the GTK4/libadwaita file browser.
//!
//! ```text
//! tudi [PATH]          open PATH (in the running instance, if any)
//! tudi --new-window    open another window
//! tudi --background    start the resident instance without a window
//! tudi --quit          stop the resident instance
//! ```
//!
//! tudi keeps a resident instance (ADR-012): closing the last window keeps
//! the process, so the next window opens fast (P1a). Single instance via
//! `GApplication`: a second `tudi PATH` is forwarded to the running one.

mod actions;
mod activity;
mod bar;
mod bench;
mod browser;
mod crumbs;
mod dev;
mod drop;
mod find;
mod format;
mod history;
mod i18n;
mod icons;
mod launch;
mod menu;
mod pane;
mod preview;
mod properties;
mod search;
mod settings;
mod shared;
mod sidebar;
mod tabs;
mod thumbs;
mod tree_menu;
mod undo;
mod worker;
mod zoom;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tudi_core::config::{Config, ConfigError};
use tudi_core::state::WindowState;

use browser::Browser;

const APP_ID: &str = "io.github.baojie.tudi";

const CSS: &str = "
/* Right padding = row padding (12px) + the always-visible scrollbar (22px),
   so the header columns line up with the list columns. */
.tudi-column-header { padding: 0 12px; }
.tudi-column-header button { padding: 2px 0; min-height: 0; font-weight: normal; }
.tudi-col-handle { margin: 3px 0; }
.tudi-col-handle:hover { background: linear-gradient(to right, transparent 3px, alpha(currentColor, 0.4) 3px, alpha(currentColor, 0.4) 5px, transparent 5px); }
/* The header's line between columns: the size and date buttons' left
   border, exactly like the rows' below. */
.tudi-column-header .tudi-col { margin-left: -1px; border-left: 1px solid alpha(currentColor, 0.12); border-radius: 0; }
/* A faint line between columns in every row, under the header's line
   (the label's left border, pulled into the gap: content unmoved; the
   dim-label opacity halves it, hence the stronger alpha). */
.tudi-details > row .tudi-col { margin: -2px 0 -2px -1px; padding: 2px 0; border-left: 1px solid alpha(currentColor, 0.22); }
.tudi-details > row:selected .tudi-col { border-left-color: alpha(white, 0.25); }
.tudi-details > row { padding: 2px 12px; min-height: 0; }
.tudi-status { padding: 3px 12px; font-size: smaller; }
.tudi-expander { opacity: 0.7; }
.tudi-tree > row { padding: 2px 6px; min-height: 0; }
/* Finder-like look: a light sidebar, blue selection in the file list
   (grey when the list does not have the focus). */
.tudi-tree, .tudi-tree-scroll { background-color: @sidebar_bg_color; }
.tudi-tree > row:selected { background-color: alpha(currentColor, 0.12); color: inherit; }
.tudi-details > row:selected { background-color: #2f7de1; color: white; }
.tudi-details > row:selected .dim-label { color: alpha(white, 0.85); opacity: 1; }
.tudi-details:not(:focus-within) > row:selected { background-color: alpha(currentColor, 0.14); color: inherit; }
.tudi-details:not(:focus-within) > row:selected .dim-label { color: inherit; opacity: 0.55; }
.tudi-bar { background-color: @headerbar_bg_color; }
.tudi-bar { padding: 2px 6px; }
.tudi-find { padding: 4px 6px; }
.tudi-drop-row:drop(active) { background: alpha(@accent_bg_color, 0.25); border-radius: 4px; }
.tudi-tabs { padding: 3px 6px 0 6px; background-color: @headerbar_bg_color; border-bottom: 1px solid alpha(currentColor, 0.12); }
.tudi-tab { padding: 1px 2px 1px 10px; border-radius: 6px 6px 0 0; min-width: 80px; opacity: 0.75; }
.tudi-tab:hover { background-color: alpha(currentColor, 0.06); opacity: 1; }
.tudi-tab.active { background-color: @view_bg_color; opacity: 1; box-shadow: inset 0 2px @accent_bg_color; }
.tudi-tab.shown { background-color: @view_bg_color; opacity: 1; }
.tudi-tab-page.tudi-tile { box-shadow: inset 0 2px alpha(currentColor, 0.08); }
.tudi-tab-page.tudi-tile.active { box-shadow: inset 0 2px @accent_bg_color; }
.tudi-grid > child { padding: 8px; border-radius: 8px; }
.tudi-grid > child:selected { background-color: alpha(@accent_bg_color, 0.25); color: inherit; }
.tudi-preview { padding: 12px; background-color: @view_bg_color; }
.tudi-crumb-field { background-color: alpha(currentColor, 0.08); border-radius: 6px; min-height: 30px; padding: 0 4px; }
.tudi-crumb { padding: 2px 6px; min-height: 0; font-weight: normal; }
.tudi-crumb.current { font-weight: bold; }
.tudi-crumb-sep { padding: 2px 3px; min-width: 0; min-height: 0; opacity: 0.6; }
.tudi-crumb:drop(active) { background-color: alpha(@accent_bg_color, 0.25); }
.tudi-tab-close { min-width: 20px; min-height: 20px; padding: 0; }
.tudi-tab-add { min-width: 24px; min-height: 24px; padding: 0 4px; margin-bottom: 2px; }
.tudi-search { padding: 10px; box-shadow: 0 8px 28px alpha(black, 0.25); }
.tudi-search-results > row { padding: 3px 6px; }
.tudi-search-scope { padding: 0 2px 0 8px; border-radius: 999px; background: alpha(@accent_bg_color, 0.15); }
.tudi-bar-button { font-weight: normal; padding: 2px 8px; min-height: 0; }
.tudi-bar-items separator { margin: 5px 8px; min-width: 1px; background: alpha(currentColor, 0.25); }
.tudi-bar-hint { padding: 0 8px; font-size: smaller; }
/* Always-visible, wider scrollbars (overlay scrolling is off). */
window.tudi-window scrollbar.vertical slider { min-width: 10px; }
window.tudi-window scrollbar.horizontal slider { min-height: 10px; }
";

type Loaded = (
    Config,
    Option<ConfigError>,
    WindowState,
    tudi_core::bookmarks::Bookmarks,
    tudi_core::launchers::Launchers,
);

/// Process-wide state of the GUI (main thread only).
#[derive(Default)]
struct App {
    /// Config and state, read on a thread at startup.
    loading: RefCell<Option<std::thread::JoinHandle<Loaded>>>,
    loaded: RefCell<Option<Loaded>>,
    /// Open windows, most recently opened last. Owning the browsers here
    /// keeps them alive while their windows are open.
    browsers: RefCell<Vec<Rc<Browser>>>,
    /// Config, bookmarks and the history thread, shared by all windows;
    /// created with the first window.
    shared: RefCell<Option<Rc<shared::Shared>>>,
    /// Activation token of a forwarded launch, for the next window shown.
    startup_id: RefCell<Option<String>>,
}

impl App {
    fn loaded(&self) -> std::cell::Ref<'_, Loaded> {
        if self.loaded.borrow().is_none() {
            let l = self
                .loading
                .borrow_mut()
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| {
                    (
                        Config::default(),
                        None,
                        WindowState::default(),
                        Default::default(),
                        Default::default(),
                    )
                });
            *self.loaded.borrow_mut() = Some(l);
        }
        std::cell::Ref::map(self.loaded.borrow(), |l| l.as_ref().unwrap())
    }

    fn open_window(self: &Rc<Self>, app: &adw::Application, path: Option<PathBuf>) {
        let (config, err, state, bookmarks, launchers) = {
            let l = self.loaded();
            (
                l.0.clone(),
                l.1.as_ref().map(|e| e.to_string()),
                l.2.clone(),
                l.3.clone(),
                l.4.clone(),
            )
        };
        let path_given = path.is_some();
        let start = path
            .or_else(|| state.last_dir.as_ref().map(PathBuf::from))
            .unwrap_or_else(glib::home_dir);
        let shared = self
            .shared
            .borrow_mut()
            .get_or_insert_with(|| {
                let s = shared::Shared::new(config, bookmarks, launchers);
                s.run_imports(false);
                s
            })
            .clone();
        if self.browsers.borrow().is_empty() {
            zoom::set(state.zoom);
        }
        // The remembered tabs come back in the first window when no folder
        // was asked for.
        let restore_tabs = !path_given && self.browsers.borrow().is_empty();
        let b = Browser::new(app, &start, &state, shared.clone(), restore_tabs);
        shared.browsers.borrow_mut().push(Rc::downgrade(&b));
        let first = self.browsers.borrow().is_empty();
        self.browsers.borrow_mut().push(b.clone());

        let me = Rc::downgrade(self);
        let weak = Rc::downgrade(&b);
        b.window.connect_close_request(move |_| {
            if let (Some(me), Some(b)) = (me.upgrade(), weak.upgrade()) {
                me.closed(&b);
            }
            glib::Propagation::Proceed
        });
        self.present(&b.window);
        if let Some(e) = err.filter(|_| first) {
            b.toast(&e);
        }
        let (me, app2) = (Rc::downgrade(self), app.clone());
        bench::attach(
            &b,
            Rc::new(move || {
                if let Some(me) = me.upgrade() {
                    me.open_window(&app2, None);
                }
            }),
        );
        if first {
            dev::install(&b);
        }
    }

    /// Show a window, completing a forwarded launch's startup
    /// notification (otherwise the shell shows a busy launcher until it
    /// times out, and Wayland may refuse to focus the window).
    fn present(&self, window: &adw::ApplicationWindow) {
        if let Some(id) = self.startup_id.take() {
            window.set_startup_id(&id);
        }
        window.present();
    }

    /// A window closes: remember its state and drop the browser (its
    /// listing, models and watches), keeping the process resident.
    fn closed(&self, b: &Rc<Browser>) {
        let state = b.snapshot_state();
        if let Some(l) = self.loaded.borrow_mut().as_mut() {
            l.2 = state.clone();
        }
        std::thread::spawn(move || {
            if let Some(p) = WindowState::path()
                && let Err(e) = state.save(&p)
            {
                tracing::warn!("cannot save window state: {e}");
            }
        });
        self.browsers.borrow_mut().retain(|x| !Rc::ptr_eq(x, b));
    }

    /// On exit (`--quit`), save the state of the last open window. Written
    /// synchronously: no more frames are drawn at this point.
    fn save_on_exit(&self) {
        let Some(b) = self.browsers.borrow().last().cloned() else {
            return;
        };
        if let Some(p) = WindowState::path()
            && let Err(e) = b.snapshot_state().save(&p)
        {
            tracing::warn!("cannot save window state: {e}");
        }
    }

    fn command_line(self: &Rc<Self>, app: &adw::Application, cl: &gio::ApplicationCommandLine) {
        let opts = cl.options_dict();
        // A launch forwarded by `tudi` carries the launcher's environment
        // (SEND_ENVIRONMENT), including the shell's activation token. A
        // local launch's token was already taken by GTK itself. The token
        // is kept only on the paths that present a window, so a stale one
        // never reaches a window opened later by other means.
        let token = cl
            .is_remote()
            .then(|| {
                cl.getenv("XDG_ACTIVATION_TOKEN")
                    .or_else(|| cl.getenv("DESKTOP_STARTUP_ID"))
                    .map(|s| s.to_string())
            })
            .flatten();
        if opts.contains("quit") {
            app.quit();
            return;
        }
        if opts.contains("reimport") {
            // Make sure the shared state exists, then import again.
            if self.shared.borrow().is_none() {
                *self.startup_id.borrow_mut() = token;
                self.open_window(app, None);
            }
            if let Some(s) = self.shared.borrow().clone() {
                s.run_imports(true);
            }
            return;
        }
        if opts.contains("background") {
            return;
        }
        let path = cl.arguments().into_iter().nth(1).map(|a| {
            // Other apps open folders with file:// URIs (desktop Exec %u).
            use std::os::unix::ffi::OsStrExt;
            if a.as_bytes().starts_with(b"file://")
                && let Some(p) = gio::File::for_uri(&a.to_string_lossy()).path()
            {
                return p;
            }
            let p = PathBuf::from(a);
            match cl.cwd() {
                Some(cwd) if p.is_relative() => cwd.join(p),
                _ => p,
            }
        });
        *self.startup_id.borrow_mut() = token;
        let last = self.browsers.borrow().last().cloned();
        match last {
            Some(b) if !opts.contains("new-window") => {
                if let Some(p) = path {
                    b.navigate(&p, true);
                    b.record_visit(&p);
                }
                self.present(&b.window);
            }
            _ => {
                let visit = path.clone();
                self.open_window(app, path);
                if let (Some(p), Some(b)) = (visit, self.browsers.borrow().last()) {
                    b.record_visit(&p);
                }
            }
        }
    }
}

fn main() -> glib::ExitCode {
    bench::START.get_or_init(std::time::Instant::now);
    bench::init();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // The config is read first (a small file, before GTK starts): the UI
    // language must be known before any string is translated.
    let (config, err) = Config::load_user();
    i18n::init(config.ui.language.clone());
    let me = Rc::new(App::default());
    // Read the window state on a thread while GTK initializes.
    *me.loading.borrow_mut() = Some(std::thread::spawn(move || {
        let state = WindowState::path()
            .map(|p| WindowState::load(&p))
            .unwrap_or_default();
        let bookmarks = tudi_core::bookmarks::Bookmarks::path()
            .map(|p| tudi_core::bookmarks::Bookmarks::load(&p))
            .unwrap_or_default();
        let launchers = tudi_core::launchers::Launchers::path()
            .map(|p| tudi_core::launchers::Launchers::load(&p))
            .unwrap_or_default();
        (config, err, state, bookmarks, launchers)
    }));

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    // Users start `tudi` (the launcher); show that name in --help.
    glib::set_prgname(Some("tudi"));
    for (name, short, help) in [
        ("new-window", b'n', i18n::tr("Open a new window")),
        (
            "background",
            0,
            i18n::tr("Start the resident instance without a window"),
        ),
        ("quit", 0, i18n::tr("Quit the resident instance")),
        (
            "reimport",
            0,
            i18n::tr("Import bookmarks and folder history from GTK, autojump and zoxide again"),
        ),
    ] {
        app.add_main_option(
            name,
            glib::Char::from(short),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            &help,
            None,
        );
    }
    app.connect_startup(|app| {
        // Resident instance: closing the last window keeps the process.
        std::mem::forget(app.hold());
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
    let m = me.clone();
    app.connect_command_line(move |app, cl| {
        m.command_line(app, cl);
        glib::ExitCode::SUCCESS
    });
    let m = me.clone();
    app.connect_activate(move |app| m.open_window(app, None));
    let m = me.clone();
    app.connect_shutdown(move |_| m.save_on_exit());
    app.run()
}
