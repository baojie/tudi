//! Running launchers (ADR-014) and adding them.
//!
//! Everything that touches the filesystem — the `stat` that decides how to
//! run a script, spawning, the log file — happens on a helper thread; the
//! result comes back to the main loop for a toast.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;

use gtk::glib;
use tudi_core::launchers::{RunMode, in_terminal, is_launchable, plan};
use tudi_core::listing::meta::MetaStatus;

use crate::browser::Browser;
use crate::i18n::{fill, tr};

/// How a background run ended.
enum Outcome {
    Started,
    Finished { code: Option<i32>, log: PathBuf },
    Failed(String),
}

fn log_path(label: &str) -> PathBuf {
    let dir = tudi_core::config::state_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("launch-logs");
    let stamp = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("%Y%m%d-%H%M%S").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let safe: String = label
        .chars()
        .map(|c| if c == '/' { '_' } else { c })
        .collect();
    dir.join(format!("{safe}-{stamp}.log"))
}

impl Browser {
    /// Run launcher `i`, with `files` as extra arguments.
    pub(crate) fn run_launcher(self: &Rc<Self>, i: usize, files: Vec<PathBuf>) {
        let Some(l) = self.shared.launchers.borrow().items.get(i).cloned() else {
            return;
        };
        let term = self.shared.config.borrow().commands.terminal_exec.clone();
        let label = l.label();
        let (tx, rx) = async_channel::unbounded::<Outcome>();
        std::thread::spawn(move || {
            let executable = std::fs::metadata(&l.path)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false);
            let p = plan(&l, &files, executable);
            let (program, args) = if p.terminal {
                match in_terminal(&term, &p) {
                    Some(c) => c,
                    None => {
                        let _ = tx.send_blocking(Outcome::Failed("no terminal command".into()));
                        return;
                    }
                }
            } else {
                (p.program.clone(), p.args.clone())
            };
            let mut cmd = Command::new(&program);
            cmd.args(&args).current_dir(&p.cwd).stdin(Stdio::null());
            let background = l.mode == RunMode::Background && !l.is_desktop_file();
            let log = log_path(&l.label());
            if background {
                let file = std::fs::create_dir_all(log.parent().unwrap())
                    .and_then(|_| std::fs::File::create(&log));
                match file.and_then(|f| Ok((f.try_clone()?, f))) {
                    Ok((out, err)) => {
                        cmd.stdout(out).stderr(err);
                    }
                    Err(e) => {
                        let _ = tx.send_blocking(Outcome::Failed(e.to_string()));
                        return;
                    }
                }
            }
            match cmd.spawn() {
                Ok(mut child) => {
                    let _ = tx.send_blocking(Outcome::Started);
                    let status = child.wait().ok();
                    if background {
                        let _ = tx.send_blocking(Outcome::Finished {
                            code: status.and_then(|s| s.code()),
                            log,
                        });
                    }
                }
                Err(e) => {
                    let _ = tx.send_blocking(Outcome::Failed(format!("{program}: {e}")));
                }
            }
        });
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(o) = rx.recv().await {
                let Some(b) = w.upgrade() else { return };
                match o {
                    Outcome::Started => b.toast(&fill(&tr("Started {}"), &[&label])),
                    Outcome::Failed(e) => b.toast(&fill(&tr("Cannot run {}: {}"), &[&label, &e])),
                    Outcome::Finished { code, log } => {
                        let text = match code {
                            Some(0) => fill(&tr("{} finished"), &[&label]),
                            Some(c) => fill(&tr("{} failed (exit code {})"), &[&label, &c]),
                            None => fill(&tr("{} was stopped"), &[&label]),
                        };
                        let t = adw::Toast::new(&text);
                        t.set_button_label(Some(&tr("Show Log")));
                        t.set_timeout(8);
                        let win = b.window.clone();
                        t.connect_button_clicked(move |_| {
                            crate::browser::open_default(&log, &win);
                        });
                        b.toasts.add_toast(t);
                    }
                }
            }
        });
    }

    /// Add a file to the launchers.
    pub(crate) fn add_launcher(&self, file: &std::path::Path) {
        let label = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.shared.launchers.borrow_mut().add(file) {
            self.toast(&fill(&tr("Added {} to the launchers"), &[&label]));
            self.shared.launchers_changed();
        } else {
            self.toast(&tr("Already a launcher"));
        }
    }

    /// The single selected file, if it can be a launcher.
    pub(crate) fn selected_launchable(&self) -> Option<PathBuf> {
        let sel = self.selected_entries();
        if sel.len() != 1 {
            return None;
        }
        let d = self.pane.data();
        let e = sel[0];
        if d.is_dir(e) {
            return None;
        }
        let name = d.name(e);
        let m = d.metas.get(e as usize);
        // Before metadata arrives, decide by name only.
        let mode = if m.status == MetaStatus::Ok {
            m.mode
        } else {
            0o100644
        };
        is_launchable(&String::from_utf8_lossy(name), mode)
            .then(|| self.current_dir().join(OsStr::from_bytes(name)))
    }

    /// `Ctrl+D`: a selected script or app becomes a launcher; a selected
    /// folder (or, with nothing selected, the current folder) a bookmark.
    pub(crate) fn bookmark_or_launcher(&self) {
        if let Some(f) = self.selected_launchable() {
            self.add_launcher(&f);
        } else {
            self.bookmark_selected_folder();
        }
    }
}
