//! `tudi`: the GTK4/libadwaita file browser.

use adw::prelude::*;

const APP_ID: &str = "io.github.baojie.tudi";

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_window);
    let status = app.run();
    if status != gtk::glib::ExitCode::SUCCESS {
        anyhow::bail!("application exited with status {status:?}");
    }
    Ok(())
}

fn build_window(app: &adw::Application) {
    let header = adw::HeaderBar::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&header);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("tudi")
        .default_width(960)
        .default_height(640)
        .content(&content)
        .build();
    window.present();
}
