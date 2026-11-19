//! `tudi`: the command-line entry point. Links GIO only, not GTK.
//!
//! If a tudi instance is running, the command line is forwarded to it over
//! D-Bus (GApplication's remote protocol), which keeps a second launch fast
//! (P2): loading GTK pulls in the GL driver stack even when nothing is
//! drawn. Otherwise this process execs `tudi-gui` (next to this binary)
//! with the same arguments, which becomes the primary instance.
//!
//! `tudi q` and `tudi index` (see `cli.rs`) talk to the index daemon.
//!
//! The options must match those of `tudi-gui` (see its `main.rs`), since
//! GApplication parses them locally before forwarding.

mod cli;

use std::os::unix::process::CommandExt;

use gio::prelude::*;

const APP_ID: &str = "io.github.baojie.tudi";

fn main() -> glib::ExitCode {
    // `tudi q …` / `tudi index …`: talk to the index daemon, no GUI.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = cli::run(&args) {
        return glib::ExitCode::from(code);
    }
    // Help is printed by tudi-gui, which has the translations.
    let wants_help = std::env::args_os().skip(1).any(|a| {
        a.to_str()
            .is_some_and(|a| a == "-h" || a.starts_with("--help"))
    });
    if wants_help {
        return exec_gui();
    }
    // SEND_ENVIRONMENT: the primary needs our XDG_ACTIVATION_TOKEN to end
    // the shell's startup notification (plain GApplication does not put it
    // in the platform data the way GtkApplication does).
    let app = gio::Application::new(
        Some(APP_ID),
        gio::ApplicationFlags::HANDLES_COMMAND_LINE | gio::ApplicationFlags::SEND_ENVIRONMENT,
    );
    for (name, short, help) in [
        ("new-window", b'n', "Open a new window"),
        (
            "background",
            0,
            "Start the resident instance without a window",
        ),
        ("quit", 0, "Quit the resident instance"),
        ("reimport", 0, "Import bookmarks and folder history again"),
    ] {
        app.add_main_option(
            name,
            glib::Char::from(short),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            help,
            None,
        );
    }
    let registered = app.register(gio::Cancellable::NONE).is_ok();
    if registered && app.is_remote() {
        // Forwards argv, options and cwd; returns the primary's status.
        return app.run();
    }
    // No instance: become one. Dropping our bus connection on exec releases
    // the name for tudi-gui.
    exec_gui()
}

fn exec_gui() -> glib::ExitCode {
    let gui = std::env::current_exe()
        .map(|p| p.with_file_name("tudi-gui"))
        .unwrap_or_else(|_| "tudi-gui".into());
    let err = std::process::Command::new(&gui)
        .args(std::env::args_os().skip(1))
        .exec();
    eprintln!("tudi: cannot start {}: {err}", gui.display());
    glib::ExitCode::FAILURE
}
