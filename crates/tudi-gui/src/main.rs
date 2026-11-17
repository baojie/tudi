//! `tudi`: the GTK4/libadwaita file browser.

mod browser;
mod format;
mod pane;
mod sidebar;
mod worker;

use std::path::PathBuf;
use std::rc::Rc;

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
        let b = browser::Browser::new(app, &start);
        b.window.present();
        if let Some(out) = std::env::var_os("TUDI_DEV_SCREENSHOT") {
            screenshot_later(b, PathBuf::from(out));
        }
    });
    let status = app.run_with_args::<&str>(&[]);
    if status != glib::ExitCode::SUCCESS {
        anyhow::bail!("application exited with status {status:?}");
    }
    Ok(())
}

/// Development aid: render the window to a PNG after it settles, then quit.
fn screenshot_later(b: Rc<browser::Browser>, out: PathBuf) {
    glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
        let w = &b.window;
        let paintable = gtk::WidgetPaintable::new(Some(w));
        let (width, height) = (w.width() as f64, w.height() as f64);
        let snap = gtk::Snapshot::new();
        paintable.snapshot(&snap, width, height);
        if let (Some(node), Some(renderer)) = (snap.to_node(), w.renderer()) {
            let tex = renderer.render_texture(&node, None);
            if let Err(e) = tex.save_to_png(&out) {
                tracing::error!("screenshot: {e}");
            }
        }
        w.application().unwrap().quit();
    });
}
