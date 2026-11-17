//! `tudi`: the GTK4/libadwaita file browser.

mod actions;
mod browser;
mod dev;
mod format;
mod pane;
mod sidebar;
mod worker;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gdk, glib};

const APP_ID: &str = "io.github.baojie.tudi";

const CSS: &str = "
.tudi-column-header { padding: 0 12px 0 6px; }
.tudi-column-header button { padding: 2px 6px; min-height: 0; font-weight: normal; }
.tudi-details row { padding: 3px 12px; }
.tudi-status { padding: 3px 12px; font-size: smaller; }
.tudi-expander { opacity: 0.7; }
.tudi-tree row { padding: 3px 6px; }
";

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let start = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(glib::home_dir);
    let start = std::path::absolute(&start).unwrap_or(start);

    // Read the config file on a thread while GTK initializes.
    let config = std::thread::spawn(tudi_core::config::Config::load_user);
    let config = std::cell::RefCell::new(Some(config));

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
    app.connect_activate(move |app| {
        let (cfg, err) = match config.borrow_mut().take() {
            Some(h) => h.join().unwrap_or_default(),
            None => Default::default(),
        };
        let b = browser::Browser::new(app, &start, cfg);
        b.window.present();
        if let Some(e) = err {
            b.toast(&e.to_string());
        }
        dev::install(&b);
    });
    let status = app.run_with_args::<&str>(&[]);
    if status != glib::ExitCode::SUCCESS {
        anyhow::bail!("application exited with status {status:?}");
    }
    Ok(())
}
