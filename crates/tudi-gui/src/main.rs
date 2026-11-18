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
mod bench;
mod browser;
mod dev;
mod format;
mod history;
mod i18n;
mod pane;
mod sidebar;
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
.tudi-column-header { padding: 0 12px; }
.tudi-column-header button { padding: 2px 0; min-height: 0; font-weight: normal; }
.tudi-details row { padding: 3px 12px; }
.tudi-status { padding: 3px 12px; font-size: smaller; }
.tudi-expander { opacity: 0.7; }
.tudi-tree row { padding: 3px 6px; }
";

type Loaded = (Config, Option<ConfigError>, WindowState);

/// Process-wide state of the GUI (main thread only).
#[derive(Default)]
struct App {
    /// Config and state, read on a thread at startup.
    loading: RefCell<Option<std::thread::JoinHandle<Loaded>>>,
    loaded: RefCell<Option<Loaded>>,
    /// Open windows, most recently opened last. Owning the browsers here
    /// keeps them alive while their windows are open.
    browsers: RefCell<Vec<Rc<Browser>>>,
    /// The folder history thread, started with the first window.
    history: RefCell<Option<history::History>>,
}

impl App {
    fn loaded(&self) -> std::cell::Ref<'_, Loaded> {
        if self.loaded.borrow().is_none() {
            let l = self
                .loading
                .borrow_mut()
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| (Config::default(), None, WindowState::default()));
            *self.loaded.borrow_mut() = Some(l);
        }
        std::cell::Ref::map(self.loaded.borrow(), |l| l.as_ref().unwrap())
    }

    fn open_window(self: &Rc<Self>, app: &adw::Application, path: Option<PathBuf>) {
        let (config, err, state) = {
            let l = self.loaded();
            (
                l.0.clone(),
                l.1.as_ref().map(|e| e.to_string()),
                l.2.clone(),
            )
        };
        let start = path
            .or_else(|| state.last_dir.as_ref().map(PathBuf::from))
            .unwrap_or_else(glib::home_dir);
        let hist = self
            .history
            .borrow_mut()
            .get_or_insert_with(|| history::History::start(config.history.exclude.clone()))
            .clone();
        if self.browsers.borrow().is_empty() {
            zoom::set(state.zoom);
        }
        let b = Browser::new(app, &start, config, &state, hist);
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
        b.window.present();
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
        if opts.contains("quit") {
            app.quit();
            return;
        }
        if opts.contains("background") {
            return;
        }
        let path = cl.arguments().into_iter().nth(1).map(|a| {
            let p = PathBuf::from(a);
            match cl.cwd() {
                Some(cwd) if p.is_relative() => cwd.join(p),
                _ => p,
            }
        });
        let last = self.browsers.borrow().last().cloned();
        match last {
            Some(b) if !opts.contains("new-window") => {
                if let Some(p) = path {
                    b.navigate(&p, true);
                    b.record_visit(&p);
                }
                b.window.present();
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

    i18n::init();
    let me = Rc::new(App::default());
    // Read config and window state on a thread while GTK initializes.
    *me.loading.borrow_mut() = Some(std::thread::spawn(|| {
        let (config, err) = Config::load_user();
        let state = WindowState::path()
            .map(|p| WindowState::load(&p))
            .unwrap_or_default();
        (config, err, state)
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
